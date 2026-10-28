#!/usr/bin/env python3
"""A stand-in for Voicebox's HTTP API, for the Doublage vocal test: /profiles, /generate, /history/<id>, /audio/<id>.
Prints its port on the first line of standard output. Each "voice" is a tone 0.065 s per character long."""
import json, math, struct, sys, threading
from http.server import BaseHTTPRequestHandler, HTTPServer

GEN = {}

def wav(seconds, rate=24000):
    n = int(seconds * rate)
    pcm = b"".join(struct.pack("<h", int(9000 * math.sin(2 * math.pi * 220 * i / rate))) for i in range(n))
    hdr = b"RIFF" + struct.pack("<I", 36 + len(pcm)) + b"WAVEfmt " + struct.pack("<IHHIIHH", 16, 1, 1, rate, rate * 2, 2, 16) + b"data" + struct.pack("<I", len(pcm))
    return hdr + pcm

class H(BaseHTTPRequestHandler):
    def log_message(self, *a):
        pass
    def reply(self, body, ctype="application/json"):
        data = body if isinstance(body, bytes) else json.dumps(body).encode()
        self.send_response(200); self.send_header("Content-Type", ctype); self.send_header("Content-Length", str(len(data))); self.end_headers(); self.wfile.write(data)
    def do_GET(self):
        if self.path == "/health":
            self.reply({"status": "ok"})
        elif self.path == "/profiles":
            self.reply([{"id": "p-test-0001", "name": "Test voix", "voice_type": "preset", "language": "en", "default_engine": "kokoro"}])
        elif self.path.startswith("/history/"):
            g = GEN.get(self.path.split("/")[-1])
            self.reply({"id": self.path.split("/")[-1], "status": "completed" if g else "failed"})
        elif self.path.startswith("/audio/"):
            self.reply(wav(0.065 * len(GEN[self.path.split("/")[-1]])), "audio/wav")
        else:
            self.send_response(404); self.end_headers()
    def do_POST(self):
        body = json.loads(self.rfile.read(int(self.headers.get("Content-Length", 0))) or b"{}")
        if self.path == "/generate":
            gid = "g%d" % (len(GEN) + 1)
            GEN[gid] = body["text"]
            self.reply({"id": gid})
        else:
            self.reply({})

srv = HTTPServer(("127.0.0.1", 0), H)
print(srv.server_address[1], flush=True)
srv.serve_forever()
