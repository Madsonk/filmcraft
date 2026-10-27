#!/usr/bin/env python3
"""Doublage vocal, a FilmCraft extension program.

Reads the extension context (JSON) on standard input and the saved project next to it, and prints
FilmCraft commands (one JSON object per line) or {"log": ...} lines. Standard library only; it needs
ffmpeg, a running (or startable) Voicebox, and, only when no translation exists yet, a Python with
transformers + sentencepiece + torch and an Opus-MT model (see `mt.py`).

  dub.py dub            segments of the selected clip -> translation -> Voicebox voice -> time fit
                        -> one audio file aligned on the sequence -> sequence.placeDub (one undo step)
  dub.py choose-voice   starts Voicebox if needed, lists its voice profiles and remembers the choice

Variables (`--var name=value`, or the `vars` of `extensions.run`):
  target=en|fr|…        language to speak (default: the entry's)          source=auto|fr|en|…
  voice=<name|id>       Voicebox profile (default: the one remembered by "Choisir la voix…")
  mine=1                use the profile remembered as "my voice" (name containing "madson" otherwise)
  original=duck|cut|keep   original voice under the dub: lowered (default), silenced, untouched
  duck_db=-18           attenuation of the original voice, dB             max_stretch=0.15 (speed bound)
  shorten=auto|off      rephrase shorter when a phrase cannot fit         fill=1  (slow a short phrase)
  segments=<file>       .srt/.json of segments (sequence seconds)         translation=<file> .srt/.json
  engine=<voicebox engine> seed=<n>  gen_timeout=300  start_timeout=240  dry_run=1  keep_server=1
  voicebox=http://127.0.0.1:17493  voicebox_server=<binary>  voicebox_data=<dir>  python=<python for mt>
  mt_model=<Opus-MT folder>  track_name=<name of the new track>
"""
import hashlib
import json
import os
import re
import shutil
import subprocess
import sys
import time
import urllib.error
import urllib.request
import wave

TICKS = 254016000000
EXT_ID = "doublage-voix"
START = time.time()


# --------------------------------------------------------------------------------- output / errors
class Fail(Exception):
    pass


def out(obj):
    print(json.dumps(obj, ensure_ascii=False))
    sys.stdout.flush()


def log(msg):
    out({"log": msg})
    sys.stderr.write(msg + "\n")
    sys.stderr.flush()


def progress(msg):
    sys.stderr.write("[%5.0f s] %s\n" % (time.time() - START, msg))
    sys.stderr.flush()


def fmt_s(x):
    return ("%.1f" % x).replace(".", ",") + " s"


# ---------------------------------------------------------------------------------------- settings
def opt(ctx, name, default=None):
    v = (ctx.get("vars") or {}).get(name)
    if v is None or v == "":
        return default
    return v


def flag(ctx, name, default=False):
    v = opt(ctx, name)
    if v is None:
        return default
    return str(v).strip().lower() in ("1", "true", "yes", "oui", "on")


def num(ctx, name, default):
    try:
        return float(opt(ctx, name, default))
    except (TypeError, ValueError):
        raise Fail("`%s` doit être un nombre (reçu : %r)" % (name, opt(ctx, name)))


def state_dir(ctx):
    base = ctx.get("dataDir") or os.path.join(os.path.expanduser("~"), ".filmcraft")
    d = os.path.join(base, "extension-data", EXT_ID)
    os.makedirs(d, exist_ok=True)
    return d


def load_config(ctx):
    try:
        with open(os.path.join(state_dir(ctx), "config.json"), encoding="utf-8") as f:
            return json.load(f)
    except (OSError, ValueError):
        return {}


def save_config(ctx, cfg):
    with open(os.path.join(state_dir(ctx), "config.json"), "w", encoding="utf-8") as f:
        json.dump(cfg, f, ensure_ascii=False, indent=1)


def find_ffmpeg():
    p = shutil.which("ffmpeg")
    if p:
        return p
    for c in ("/opt/homebrew/bin/ffmpeg", "/usr/local/bin/ffmpeg", "/usr/bin/ffmpeg"):
        if os.path.exists(c):
            return c
    raise Fail("ffmpeg est introuvable (PATH, /opt/homebrew/bin, /usr/local/bin) : le doublage l'utilise pour caler et mixer la voix. "
               "Installez-le (brew install ffmpeg).")


def ffmpeg(args, ff):
    r = subprocess.run([ff, "-hide_banner", "-loglevel", "error", "-y"] + args, stdin=subprocess.DEVNULL,
                       stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
    if r.returncode != 0:
        raise Fail("ffmpeg a échoué : " + (r.stderr.strip().splitlines() or ["?"])[-1])


def wav_seconds(path):
    with wave.open(path, "rb") as w:
        return w.getnframes() / float(w.getframerate())


# ------------------------------------------------------------------------------- language guessing
STOP = {
    "fr": "le la les un une des du de et est que qui dans pour pas sur avec ce cette ces au aux je vous nous il elle on ne se mais ou donc ont va vais comment merci d'abord ensuite aujourd'hui bonjour tous".split(),
    "en": "the a an and is are that this with for you we it of to in on not but or so have has will how thank first then today hello all".split(),
    "es": "el la los las un una y es que en de por con para no se pero o como gracias hola todos".split(),
    "de": "der die das ein eine und ist nicht mit für ich wir sie es zu auf aber oder wie danke hallo".split(),
    "pt": "o a os as um uma e é que em de por com para não se mas ou como obrigado olá todos".split(),
    "it": "il lo la gli le un una e è che in di per con non si ma o come grazie ciao tutti".split(),
}


def guess_lang(text):
    words = re.findall(r"[a-zàâçéèêëîïôûùüÿœ']+", text.lower())
    if not words:
        return None
    best, score = None, 0
    for lang, sw in STOP.items():
        s = sum(1 for w in words if w in sw)
        if s > score:
            best, score = lang, s
    return best if score >= 1 else None


def lang_tag(x):
    return (x or "").replace("_", "-").split("-")[0].lower()


# ------------------------------------------------------------------------------------------ project
def load_project(ctx):
    path = ctx.get("project")
    if not path or not os.path.isfile(path):
        raise Fail("enregistrez le projet d'abord : le doublage lit le .fcproj (transcription, sous-titres) à côté de lui")
    with open(path, encoding="utf-8") as f:
        doc = json.load(f)
    items = doc["project"]["items"]
    view = doc.get("view") or {}
    sid = str(view.get("active_sequence")) if view.get("active_sequence") is not None else None
    if sid not in items or not isinstance(items[sid]["kind"], dict) or "Sequence" not in items[sid]["kind"]:
        sid = next((k for k, v in items.items() if isinstance(v["kind"], dict) and "Sequence" in v["kind"]), None)
    if sid is None:
        raise Fail("le projet n'a pas de séquence")
    return doc["project"], items[sid]["kind"]["Sequence"]


def selected_audio(seq, selection):
    """Audio clips of the selection (a selected picture clip brings its linked sound)."""
    sel = set(selection or [])
    links = set()
    for kind in ("video_tracks", "audio_tracks"):
        for tr in seq[kind]:
            for c in tr["items"]:
                if c["id"] in sel and c.get("link") is not None:
                    links.add(c["link"])
    res = []
    for tr in seq["audio_tracks"]:
        for c in tr["items"]:
            if c["id"] in sel or (c.get("link") is not None and c["link"] in links):
                res.append((tr, c))
    return res


def words_in_sequence(project, tr_clip):
    _, c = tr_clip
    t = (project.get("transcripts") or {}).get(str(c["item"]))
    if not t:
        return [], None
    speed = c.get("speed", 1.0) or 1.0
    if c.get("reverse"):
        raise Fail("un clip joué à l'envers ne peut pas être doublé")
    sin = c.get("source_in", 0)
    sout = sin + c["duration"] * speed
    res = []
    for w in t.get("words", []):
        if w["end"] <= sin or w["start"] >= sout:
            continue
        a = c["start"] + (max(w["start"], sin) - sin) / speed
        b = c["start"] + (min(w["end"], sout) - sin) / speed
        res.append({"text": w["text"], "start": a / TICKS, "end": b / TICKS})
    return res, t.get("language")


def sentences(words, pause=0.8, max_s=14.0):
    segs, cur = [], []
    for i, w in enumerate(words):
        cur.append(w)
        nxt = words[i + 1] if i + 1 < len(words) else None
        end_p = bool(re.search(r"[.!?…]['\")»]*$", w["text"]))
        gap = (nxt["start"] - w["end"]) if nxt else 99
        long_ = (w["end"] - cur[0]["start"]) > max_s and w["text"].endswith((",", ";", ":"))
        if end_p or gap >= pause or long_ or nxt is None:
            segs.append({"start": cur[0]["start"], "end": cur[-1]["end"], "src": " ".join(x["text"] for x in cur)})
            cur = []
    return segs


def merge_chunks(segs, pause=0.8, max_s=14.0):
    """Subtitles are cut by length, not by sense (« …importez vos rushes dans » / « le projet… »): a dub speaks sentences. Join
    consecutive cues until the text ends a sentence, there is a pause, or the phrase gets long."""
    def decisive(x):
        return (x.get("src") or x.get("text") or "").strip()

    def ends(x):
        return bool(re.search(r"[.!?…]['\")»]*$", decisive(x)))

    out = []
    for sg in segs:
        sg = dict(sg)
        if out:
            prev = out[-1]
            if not ends(prev) and sg["start"] - prev["end"] < pause and sg["end"] - prev["start"] <= max_s:
                prev["end"] = sg["end"]
                prev["src"] = (prev.get("src", "") + " " + sg.get("src", "")).strip()
                prev["text"] = None if prev.get("text") is None or sg.get("text") is None else (prev["text"] + " " + sg["text"]).strip()
                continue
        out.append(sg)
    return out


SENT = re.compile(r"(?<=[.!?…])['\")»]*\s+(?=[A-ZÀ-Ý0-9«\"'¿¡])")


def split_sentences(segs):
    """A merged group may hold several sentences: split them again, sharing the group's time by characters."""
    out = []
    for sg in segs:
        key = "src" if sg.get("src") else "text"
        parts = [x.strip() for x in SENT.split(sg.get(key) or "") if x.strip()]
        tparts = [x.strip() for x in SENT.split(sg["text"]) if x.strip()] if sg.get("text") else None
        if len(parts) < 2 or (tparts is not None and sg.get("src") and len(tparts) != len(parts)):
            out.append(sg)
            continue
        if tparts is not None and not sg.get("src"):
            parts = tparts
        total = float(sum(len(x) for x in parts))
        span = sg["end"] - sg["start"]
        t = sg["start"]
        for i, part in enumerate(parts):
            d = span * len(part) / total
            n = {"start": t, "end": t + d, "src": part if key == "src" else sg.get("src", ""), "text": None}
            if sg.get("text") is not None:
                n["text"] = tparts[i] if tparts is not None and len(tparts) == len(parts) else None
            out.append(n)
            t += d
    return out


FILLERS = re.compile(r"(?:(?<=\s)|^)(?:euh+|heu+h?|hum+|hmm+|mmh+|mm+|uh+|um+|erm+)[,.]?(?=\s|$)", re.I)


def clean_fillers(text, lang):
    t = text
    if lang == "fr":
        t = re.sub(r",\s*eux,", ",", t)  # whisper writes a spoken « euh » as « eux, »
    t = FILLERS.sub("", t)
    t = re.sub(r"\s+,", ",", t)
    t = re.sub(r",\s*,", ",", t)
    t = re.sub(r"\s{2,}", " ", t).strip(" ,")
    return t


# ------------------------------------------------------------------------------ SRT / JSON segments
def parse_time(s):
    m = re.match(r"(\d+):(\d+):(\d+)[,.](\d+)", s.strip())
    if not m:
        raise Fail("horodatage illisible : " + s)
    h, mi, se, ms = m.groups()
    return int(h) * 3600 + int(mi) * 60 + int(se) + int(ms.ljust(3, "0")[:3]) / 1000.0


def read_segments_file(path, what="segments"):
    if not os.path.isfile(path):
        raise Fail("fichier de %s introuvable : %s" % (what, path))
    with open(path, encoding="utf-8-sig") as f:
        raw = f.read()
    if path.lower().endswith(".json"):
        d = json.loads(raw)
        d = d.get("segments", d) if isinstance(d, dict) else d
        res = []
        for x in d:
            if isinstance(x, str):
                res.append({"start": None, "end": None, "text": x})
            else:
                res.append({"start": x.get("start"), "end": x.get("end"),
                            "text": x.get("text") or x.get("src") or "", "translation": x.get("translation") or x.get("target")})
        return res
    res = []
    for block in re.split(r"\n\s*\n", raw.replace("\r\n", "\n").strip()):
        lines = block.split("\n")
        k = next((i for i, l in enumerate(lines) if "-->" in l), None)
        if k is None:
            continue
        a, b = lines[k].split("-->")
        res.append({"start": parse_time(a), "end": parse_time(b.split()[0]), "text": " ".join(l.strip() for l in lines[k + 1:]).strip()})
    return res


# ------------------------------------------------------------------------------- collecting segments
def collect(ctx, project, seq, target):
    """-> (segments, info). A segment: {start, end, src, text|None}; times in sequence seconds."""
    sel_audio = selected_audio(seq, ctx.get("selection"))
    if not sel_audio:
        raise Fail("sélectionnez le clip à doubler (sa piste audio) dans la séquence")
    span = (min(c["start"] for _, c in sel_audio) / TICKS, max((c["start"] + c["duration"]) for _, c in sel_audio) / TICKS)
    first_track = sel_audio[0][0]
    info = {"track": first_track, "span": span, "clips": [c["id"] for _, c in sel_audio], "origin": ""}
    src_opt = opt(ctx, "source", "auto")

    # 1. explicit files
    f_seg = opt(ctx, "segments")
    f_tr = opt(ctx, "translation")
    if f_seg or f_tr:
        base = read_segments_file(f_seg or f_tr, "segments")
        segs = [{"start": b["start"], "end": b["end"], "src": b["text"], "text": b.get("translation")} for b in base]
        if f_seg and f_tr:
            tr = read_segments_file(f_tr, "traduction")
            if len(tr) != len(segs):
                raise Fail("la traduction (%d segments) n'a pas le même nombre de segments que l'original (%d)" % (len(tr), len(segs)))
            for s, t in zip(segs, tr):
                s["text"] = t.get("translation") or t["text"]
        elif f_tr and not f_seg:
            # a translated file alone: its text is the dub, timings are its own
            for s in segs:
                s["text"], s["src"] = (s.get("text") or s["src"]), ""
        if any(s["start"] is None or s["end"] is None for s in segs):
            raise Fail("le fichier de segments doit donner un début et une fin pour chaque segment (secondes de la séquence)")
        shift = num(ctx, "offset", 0.0)
        for s in segs:
            s["start"] += shift
            s["end"] += shift
        info["origin"] = "fichier " + os.path.basename(f_seg or f_tr)
        info["source_lang"] = lang_tag(src_opt) if src_opt != "auto" else (guess_lang(" ".join(s["src"] for s in segs)) or "?")
        return segs, info

    # 2. caption tracks of the sequence, inside the clip's span
    tracks = []
    for ct in seq.get("caption_tracks") or []:
        caps = [c for c in ct["captions"] if c["start"] / TICKS < span[1] and (c["start"] + c["duration"]) / TICKS > span[0]]
        if not caps:
            continue
        text = " ".join(c["text"] for c in caps)
        det = guess_lang(text)
        declared = lang_tag(ct.get("language"))
        lang = det or declared  # a track is often labelled with the UI language, not the spoken one
        tracks.append({"track": ct, "caps": sorted(caps, key=lambda c: c["start"]), "lang": lang, "declared": declared})
    done = [t for t in tracks if t["lang"] == target]
    others = [t for t in tracks if t["lang"] != target and (src_opt == "auto" or t["lang"] == lang_tag(src_opt))]
    if done:
        t = done[0]
        segs = []
        for c in t["caps"]:
            segs.append({"start": c["start"] / TICKS, "end": (c["start"] + c["duration"]) / TICKS, "src": "",
                         "text": " ".join(c["text"].split())})
        if others:  # source text of the same moments, for the report
            o = others[0]["caps"]
            for s in segs:
                mid = (s["start"] + s["end"]) / 2
                m = next((" ".join(c["text"].split()) for c in o if c["start"] / TICKS <= mid <= (c["start"] + c["duration"]) / TICKS), "")
                s["src"] = m
        info["origin"] = "piste de sous-titres « %s » (déjà en %s)" % (t["track"]["name"], target)
        info["source_lang"] = others[0]["lang"] if others else "?"
        return segs, info
    # 3. transcript of the selected clips: word times give true sentence boundaries (subtitles are cut by length)
    words, tl = [], None
    for tc in sel_audio:
        w, lang = words_in_sequence(project, tc)
        words += w
        tl = tl or lang
    if not words and others:
        t = others[0]
        segs = [{"start": c["start"] / TICKS, "end": (c["start"] + c["duration"]) / TICKS, "src": " ".join(c["text"].split()), "text": None}
                for c in t["caps"]]
        info["origin"] = "piste de sous-titres « %s » (%s)" % (t["track"]["name"], t["lang"])
        info["source_lang"] = t["lang"]
        return segs, info
    if not words:
        raise Fail("ni sous-titres ni transcription pour ce clip : lancez d'abord Extensions ▸ Sous-titres ▸ Générer les sous-titres "
                   "(puis enregistrez le projet), ou donnez un fichier avec --var segments=… .srt")
    words.sort(key=lambda w: w["start"])
    segs = sentences(words)
    src_lang = lang_tag(src_opt) if src_opt != "auto" else (lang_tag(tl) or guess_lang(" ".join(s["src"] for s in segs)) or "fr")
    for s in segs:
        s["text"] = None
    info["origin"] = "transcription du clip (%d mots)" % len(words)
    info["source_lang"] = src_lang
    return segs, info


# --------------------------------------------------------------------------------------- translation
def find_python(ctx):
    cands = [opt(ctx, "python"), os.environ.get("DUB_PYTHON"), sys.executable,
             os.path.expanduser("~/stack/tts-bench/.venv/bin/python"), "/opt/homebrew/bin/python3", "/usr/local/bin/python3", "/usr/bin/python3"]
    for c in cands:
        if not c or not (os.path.isfile(c) or shutil.which(c)):
            continue
        r = subprocess.run([c, "-c", "import transformers, sentencepiece, torch"], stdin=subprocess.DEVNULL,
                           stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        if r.returncode == 0:
            return c
    return None


def find_mt_model(ctx, src, tgt):
    name = "opus-mt-tc-big-%s-%s" % (src, tgt)
    cands = [opt(ctx, "mt_model"), os.environ.get("FILMCRAFT_MT_MODEL")]
    for base in (os.environ.get("FILMCRAFT_MODELS_DIR"), os.path.join(ctx.get("dataDir") or "", "models")):
        if base:
            cands.append(os.path.join(base, name))
    for c in cands:
        if c and os.path.isfile(os.path.join(c, "config.json")):
            return c
    return None


class Translator:
    def __init__(self, ctx, src, tgt):
        self.src, self.tgt = src, tgt
        self.model = find_mt_model(ctx, src, tgt)
        self.py = find_python(ctx) if self.model else None
        self.helper = os.path.join(os.path.dirname(os.path.abspath(__file__)), "mt.py")
        if not self.model or not self.py:
            why = ("le modèle Opus-MT %s→%s n'est pas installé (dossier opus-mt-tc-big-%s-%s dans <données FilmCraft>/models, ou --var mt_model=…)"
                   % (src, tgt, src, tgt)) if not self.model else "aucun Python avec transformers + sentencepiece + torch (--var python=…)"
            raise Fail("pas de traduction disponible : %s. Traduisez d'abord les sous-titres (piste %s) ou donnez --var translation=fichier.srt" % (why, tgt.upper()))

    def translate(self, texts, nbest=1):
        req = json.dumps({"texts": texts, "nbest": nbest}, ensure_ascii=False)
        r = subprocess.run([self.py, self.helper, self.model], input=req, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
        if r.returncode != 0:
            raise Fail("la traduction a échoué : " + (r.stderr.strip().splitlines() or ["?"])[-1])
        return json.loads(r.stdout)["out"]


# ------------------------------------------------------------------------------- shortening (target en)
CONTRACT = [
    (r"\bI am\b", "I'm"), (r"\bI will\b", "I'll"), (r"\bI have\b", "I've"), (r"\byou are\b", "you're"), (r"\bwe are\b", "we're"),
    (r"\bthey are\b", "they're"), (r"\bit is\b", "it's"), (r"\bthat is\b", "that's"), (r"\bthere is\b", "there's"), (r"\bdo not\b", "don't"),
    (r"\bdoes not\b", "doesn't"), (r"\bdid not\b", "didn't"), (r"\bcannot\b", "can't"), (r"\bcan not\b", "can't"), (r"\bwill not\b", "won't"),
    (r"\bis not\b", "isn't"), (r"\bare not\b", "aren't"), (r"\bwe will\b", "we'll"), (r"\byou will\b", "you'll"), (r"\blet us\b", "let's"),
    (r"\bin order to\b", "to"), (r"\bI am going to\b", "I'll"), (r"\bat the moment\b", "now"),
    (r"\bin addition,? ", ""), (r"\bof course,? ", ""),
    (r"\b(?:really|very|just|actually|basically|simply|quite|rather|truly)\s+", ""),
    (r"\bwhich is why\b", "so"), (r"\bdue to the fact that\b", "because"), (r"\bat this point\b", "now"),
]


def condense(text, lang):
    if lang != "en":
        return text
    t = text
    for pat, rep in CONTRACT:
        t = re.sub(pat, rep, t, flags=re.I)
    return re.sub(r"\s{2,}", " ", t).strip()


# ---------------------------------------------------------------------------------------- Voicebox
class Voicebox:
    def __init__(self, ctx):
        self.base = (opt(ctx, "voicebox") or os.environ.get("VOICEBOX_URL") or "http://127.0.0.1:17493").rstrip("/")
        self.ctx = ctx
        self.proc = None
        self.started = False

    def _get(self, path, timeout=10, raw=False):
        with urllib.request.urlopen(self.base + path, timeout=timeout) as r:
            data = r.read()
        return data if raw else json.loads(data.decode("utf-8"))

    def _post(self, path, body, timeout=30):
        req = urllib.request.Request(self.base + path, data=json.dumps(body).encode("utf-8"), headers={"Content-Type": "application/json"})
        with urllib.request.urlopen(req, timeout=timeout) as r:
            return json.loads(r.read().decode("utf-8") or "null")

    def alive(self, tries=3, timeout=8):
        """A busy or swapping server can take seconds to answer: retry before concluding it is down."""
        for _ in range(tries):
            for p in ("/health", "/profiles"):
                try:
                    self._get(p, timeout=timeout)
                    return True
                except urllib.error.HTTPError as e:
                    if e.code != 404:
                        return True
                except Exception:
                    break
            time.sleep(1)
        return False

    def binary(self):
        cands = [opt(self.ctx, "voicebox_server"), os.environ.get("VOICEBOX_SERVER"),
                 os.path.expanduser("~/Applications/Voicebox.app/Contents/MacOS/voicebox-server"),
                 "/Applications/Voicebox.app/Contents/MacOS/voicebox-server", shutil.which("voicebox-server")]
        for c in cands:
            if c and os.path.isfile(c) and os.access(c, os.X_OK):
                return c
        return None

    def data_dir(self):
        cands = [opt(self.ctx, "voicebox_data"), os.environ.get("VOICEBOX_DATA"),
                 os.path.expanduser("~/Library/Application Support/sh.voicebox.app"), os.path.expanduser("~/tools/voicebox-data")]
        for c in cands:
            if c and os.path.isdir(c):
                return c
        return None

    def ensure(self):
        if self.alive():
            progress("Voicebox déjà lancé (%s)" % self.base)
            return
        b = self.binary()
        if not b:
            raise Fail("Voicebox est introuvable (ni lancé sur %s, ni installé dans ~/Applications ou /Applications). Installez Voicebox "
                       "(github.com/jamiepine/voicebox) ou donnez --var voicebox_server=/chemin/voicebox-server" % self.base)
        m = re.match(r"https?://([^:/]+):?(\d+)?", self.base)
        host, port = (m.group(1), m.group(2) or "17493") if m else ("127.0.0.1", "17493")
        if host not in ("127.0.0.1", "localhost", "::1"):
            raise Fail("Voicebox ne répond pas sur %s (serveur distant : je ne le lance pas)" % self.base)
        d = self.data_dir()
        logp = os.path.join(state_dir(self.ctx), "voicebox-server.log")
        cmd = [b, "--host", "127.0.0.1", "--port", port] + (["--data-dir", d] if d else [])
        progress("démarrage de Voicebox : " + " ".join(cmd))
        lf = open(logp, "ab")
        self.proc = subprocess.Popen(cmd, stdin=subprocess.DEVNULL, stdout=lf, stderr=lf, start_new_session=True)
        self.started = True
        timeout = num(self.ctx, "start_timeout", 240)
        t0 = time.time()
        while time.time() - t0 < timeout:
            if self.proc.poll() is not None:
                tail = ""
                try:
                    tail = " | ".join(open(logp, errors="replace").read().strip().splitlines()[-3:])
                except OSError:
                    pass
                raise Fail("Voicebox s'est arrêté au démarrage (code %s) : %s (journal : %s)" % (self.proc.returncode, tail, logp))
            if self.alive(1, 3):
                progress("Voicebox prêt après %.0f s" % (time.time() - t0))
                return
            time.sleep(2)
        raise Fail("Voicebox ne répond pas après %.0f s (le premier lancement charge torch, ~45 s ; macOS peut ralentir le premier démarrage). "
                   "Journal : %s. Relancez l'entrée, ou augmentez --var start_timeout=…" % (timeout, logp))

    def stop_if_started(self):
        if self.started and self.proc and not flag(self.ctx, "keep_server"):
            try:
                self.proc.terminate()
                self.proc.wait(timeout=15)
            except Exception:
                pass

    def profiles(self):
        ps = self._get("/profiles", timeout=20)
        return ps if isinstance(ps, list) else ps.get("profiles", [])

    def pick(self, profiles, key):
        k = str(key).strip().lower()
        for p in profiles:
            if p["id"].lower() == k or p["id"].lower().startswith(k) and len(k) >= 6:
                return p
        for p in profiles:
            if p.get("name", "").lower() == k:
                return p
        hits = [p for p in profiles if k in p.get("name", "").lower()]
        if len(hits) == 1:
            return hits[0]
        if len(hits) > 1:
            raise Fail("« %s » correspond à %d voix (%s) : précisez" % (key, len(hits), ", ".join(h["name"] for h in hits)))
        raise Fail("aucune voix Voicebox ne correspond à « %s ». Voix : %s" % (key, ", ".join(p["name"] for p in profiles) or "(aucune)"))

    def generate(self, profile, text, lang, wav_path, engine=None, seed=None):
        body = {"profile_id": profile["id"], "text": text, "language": lang}
        engine = engine or profile.get("default_engine")
        if engine:
            body["engine"] = engine
        if seed is not None:
            body["seed"] = int(seed)
        r = self._post("/generate", body, timeout=60)
        gid = r.get("id") or r.get("generation_id")
        if not gid:
            raise Fail("Voicebox n'a pas renvoyé d'identifiant de génération : %s" % r)
        limit = num(self.ctx, "gen_timeout", 300)
        t0 = time.time()
        while True:
            h = self._get("/history/%s" % gid, timeout=20)
            st = (h.get("status") or "").lower()
            if st == "completed":
                break
            if st in ("failed", "error", "cancelled"):
                raise Fail("Voicebox a échoué sur « %s » : %s" % (text[:40], h.get("error") or st))
            if time.time() - t0 > limit:
                try:
                    self._post("/generate/%s/cancel" % gid, {}, timeout=10)
                except Exception:
                    pass
                raise Fail("Voicebox n'a pas fini « %s » en %.0f s (--var gen_timeout=…). Si le CPU du serveur est à 0 %%, le worker est mort : "
                           "arrêtez voicebox-server, relancez l'entrée" % (text[:40], limit))
            time.sleep(1.0)
        with open(wav_path, "wb") as f:
            f.write(self._get("/audio/%s" % gid, timeout=60, raw=True))


def label_of(p):
    return "%s  [%s, %s, %s]" % (p.get("name", "?"), p.get("voice_type", "?"), p.get("language") or "?", p.get("default_engine") or "?")


def osascript_choose(profiles, prompt):
    if sys.platform != "darwin" or not shutil.which("osascript"):
        return None
    labels = [label_of(p).replace('"', "'") for p in profiles]
    items = ", ".join('"%s"' % l for l in labels)
    script = 'choose from list {%s} with title "Doublage vocal" with prompt "%s" OK button name "Choisir" cancel button name "Annuler"' % (items, prompt)
    try:
        r = subprocess.run(["osascript", "-e", script], stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True, timeout=300)
    except Exception:
        return None
    ans = r.stdout.strip()
    if r.returncode != 0 or not ans:
        return None
    if ans == "false":
        raise Fail("choix de la voix annulé")
    return profiles[labels.index(ans)] if ans in labels else None


def select_voice(vb, ctx, cfg, target, mine):
    profiles = vb.profiles()
    if not profiles:
        raise Fail("Voicebox n'a aucun profil de voix. Créez-en un dans l'application Voicebox (preset, ou cloné à partir de 10-15 s de voix propre).")
    key = opt(ctx, "voice")
    if key:
        return vb.pick(profiles, key)
    if mine:
        if cfg.get("my_voice"):
            try:
                return vb.pick(profiles, cfg["my_voice"])
            except Fail:
                pass
        hits = [p for p in profiles if "madson" in p.get("name", "").lower()]
        if len(hits) == 1:
            return hits[0]
        p = osascript_choose(profiles, "Quel profil est VOTRE voix (cloné à partir de votre enregistrement) ?")
        if p:
            cfg["my_voice"] = p["id"]
            return p
        raise Fail("aucun profil de voix « Madson » dans Voicebox. Enregistrez 30 à 60 s de votre voix, créez un profil cloné (voice_type cloned) "
                   "dans Voicebox, puis relancez avec --var voice=<nom> ou choisissez-le via « Choisir la voix de doublage… »")
    saved = (cfg.get("voices") or {}).get(target)
    if saved:
        try:
            return vb.pick(profiles, saved)
        except Fail:
            pass
    p = osascript_choose(profiles, "Voix pour le doublage en %s :" % target.upper())
    if p:
        cfg.setdefault("voices", {})[target] = p["id"]
        return p
    raise Fail("aucune voix choisie : lancez « Choisir la voix de doublage… » ou ajoutez --var voice=<nom>. Voix disponibles : "
               + ", ".join(label_of(p) for p in profiles))


# ------------------------------------------------------------------------------------------ the job
def fit_plan(d, slot, room, m, fill):
    """Speed factor (>1 = faster) for a phrase of natural length d in a slot, and the final length."""
    f = 1.0
    if d > room:
        f = min(d / room, 1 + m)
    elif fill and d < slot:
        f = max(d / slot, 1 - m / 2.0)
    return f, d / f


def run_dub(ctx, target, mine):
    ff = find_ffmpeg()
    project, seq = load_project(ctx)
    segs, info = collect(ctx, project, seq, target)
    src_lang = info["source_lang"]
    if str(opt(ctx, "merge", "auto")).lower() != "off":
        segs = split_sentences(merge_chunks(segs))
    if src_lang == target:
        raise Fail("le clip est déjà en %s (langue détectée) : rien à doubler" % target.upper())
    cfg = load_config(ctx)
    m = num(ctx, "max_stretch", 0.15)
    fill = flag(ctx, "fill", True)
    shorten = str(opt(ctx, "shorten", "auto")).lower() != "off"
    original = str(opt(ctx, "original", "duck")).lower()
    duck_db = num(ctx, "duck_db", -18.0)
    segs = [s for s in segs if (s["text"] or s["src"]).strip()]
    for s in segs:
        if s["text"] is None:
            s["src"] = clean_fillers(s["src"], src_lang)
    segs = [s for s in segs if (s["text"] or s["src"]).strip()]
    if not segs:
        raise Fail("aucune phrase à doubler dans ce clip")
    segs.sort(key=lambda s: s["start"])
    log("Doublage %s→%s : %d phrases (%s)" % (src_lang.upper(), target.upper(), len(segs), info["origin"]))

    tr = None
    todo = [s for s in segs if s["text"] is None]
    if todo:
        tr = Translator(ctx, src_lang, target)
        progress("traduction de %d phrases (%s)" % (len(todo), tr.model))
        cpath = os.path.join(os.path.join(ctx["projectDir"], ctx["projectName"] + "-doublage"), "translations-%s-%s.json" % (src_lang, target))
        try:
            with open(cpath, encoding="utf-8") as f:
                tcache = json.load(f)
        except (OSError, ValueError):
            tcache = {}
        miss = [s for s in todo if s["src"] not in tcache]
        if miss:
            for s, t in zip(miss, tr.translate([s["src"] for s in miss])):
                tcache[s["src"]] = t[0] if isinstance(t, list) else t
            os.makedirs(os.path.dirname(cpath), exist_ok=True)
            with open(cpath, "w", encoding="utf-8") as f:
                json.dump(tcache, f, ensure_ascii=False, indent=1)
        for s in todo:
            s["text"] = tcache[s["src"]]
    for s in segs:
        s["text"] = " ".join(s["text"].split())

    work = os.path.join(ctx["projectDir"], ctx["projectName"] + "-doublage")
    cache = os.path.join(work, "cache")
    if flag(ctx, "dry_run"):
        for i, s in enumerate(segs, 1):
            log("%2d. [%s – %s] %s  =>  %s" % (i, fmt_s(s["start"]), fmt_s(s["end"]), s["src"], s["text"]))
        log("dry_run : rien n'a été synthétisé ni posé")
        return
    os.makedirs(cache, exist_ok=True)

    vb = Voicebox(ctx)
    try:
        vb.ensure()
        voice = select_voice(vb, ctx, cfg, target, mine)
        save_config(ctx, cfg)
        log("Voix : %s" % label_of(voice))
        if voice.get("language") and lang_tag(voice["language"]) != target:
            log("Attention : la voix « %s » est étiquetée %s ; elle parlera %s (accent possible)" % (voice["name"], voice["language"], target.upper()))
        engine = opt(ctx, "engine")
        seed = opt(ctx, "seed")
        deadline = START + max(60.0, num(ctx, "budget_seconds", 3300))

        def synth(text):
            key = hashlib.sha1(("%s|%s|%s|%s|%s" % (voice["id"], target, engine, seed, text)).encode("utf-8")).hexdigest()[:16]
            raw = os.path.join(cache, key + ".raw.wav")
            trimmed = os.path.join(cache, key + ".wav")
            if not os.path.exists(trimmed):
                if not os.path.exists(raw):
                    vb.generate(voice, text, target, raw, engine, seed)
                ffmpeg(["-i", raw, "-af",
                        "silenceremove=start_periods=1:start_threshold=-48dB:start_silence=0.03,areverse,"
                        "silenceremove=start_periods=1:start_threshold=-48dB:start_silence=0.08,areverse",
                        "-ar", "48000", "-ac", "2", "-c:a", "pcm_s16le", trimmed], ff)
            return trimmed, wav_seconds(trimmed)

        n = len(segs)
        for i, s in enumerate(segs):
            nxt = segs[i + 1]["start"] if i + 1 < n else float("inf")
            s["slot"] = max(0.2, s["end"] - s["start"])
            s["room"] = s["slot"] + min(max(0.0, nxt - s["end"] - 0.08), 0.5)
            s["skipped"] = False
        for i, s in enumerate(segs):
            if time.time() > deadline:
                s["skipped"] = True
                continue
            progress("synthèse %d/%d : %s" % (i + 1, n, s["text"][:60]))
            s["wav"], s["d0"] = synth(s["text"])
            s["spoken"] = s["text"]
            s["shortened"] = False
        # rephrase the phrases that cannot fit even at the fastest allowed speed
        if shorten:
            for s in segs:
                if s.get("skipped") or s["d0"] <= s["room"] * (1 + m) or time.time() > deadline:
                    continue
                cands = []
                c1 = condense(s["text"], target)
                if c1 != s["text"]:
                    cands.append(c1)
                if tr is not None:
                    try:
                        for alt in tr.translate([s["src"]], nbest=4)[0]:
                            alt = " ".join(alt.split())
                            if alt != s["text"] and len(alt) < len(s["text"]) * 0.92 and len(alt) > len(s["text"]) * 0.5:
                                cands.append(condense(alt, target))
                    except Fail:
                        pass
                for alt in sorted(set(cands), key=len)[:2]:
                    progress("reformulation plus courte : %s" % alt[:60])
                    w2, d2 = synth(alt)
                    if d2 < s["d0"] * 0.97:
                        s["wav"], s["d1"], s["spoken"], s["shortened"] = w2, d2, alt, True
                        s["d0_orig"] = s["d0"]
                        s["d0"] = d2
                        break

        # time fit
        stats = {"n": 0, "shortened": 0, "stretched": 0, "overrun": 0, "skipped": 0}
        flagged = []
        mix_in = []
        for i, s in enumerate(segs):
            if s.get("skipped"):
                stats["skipped"] += 1
                continue
            f, final = fit_plan(s["d0"], s["slot"], s["room"], m, fill)
            s["factor"] = f
            s["final"] = final
            s["overrun"] = max(0.0, final - s["room"])
            fit = os.path.join(cache, "fit_%03d_%s.wav" % (i, hashlib.sha1(("%s|%.4f" % (s["wav"], f)).encode()).hexdigest()[:8]))
            if abs(f - 1.0) > 0.005:
                ffmpeg(["-i", s["wav"], "-af", "atempo=%.5f" % f, "-c:a", "pcm_s16le", fit], ff)
            else:
                shutil.copyfile(s["wav"], fit)
            s["final_measured"] = wav_seconds(fit)
            s["fit"] = fit
            stats["n"] += 1
            if s["shortened"]:
                stats["shortened"] += 1
            if abs(f - 1.0) > 0.005:
                stats["stretched"] += 1
            if s["overrun"] > 0.05:
                stats["overrun"] += 1
                flagged.append((i + 1, s))
            mix_in.append(s)
        if not mix_in:
            raise Fail("aucune phrase n'a pu être synthétisée dans le temps imparti (--var budget_seconds=…) ; relancez : les phrases déjà faites sont gardées")

        t0 = mix_in[0]["start"]
        total = max(s["start"] + s["final_measured"] for s in mix_in) - t0 + 0.2
        key = hashlib.sha1(("|".join("%s@%.3f" % (s["fit"], s["start"]) for s in mix_in)).encode()).hexdigest()[:8]
        voice_slug = re.sub(r"[^a-z0-9]+", "-", voice["name"].lower()).strip("-")[:24]
        outp = os.path.join(work, "doublage-%s-%s-%s.wav" % (target, voice_slug, key))
        mix(mix_in, t0, total, outp, ff, num(ctx, "gain_db", 0.0))
    finally:
        vb.stop_if_started()

    # ducking of the original voice
    cmd = {"path": outp, "time": int(round(t0 * TICKS)), "name": "Doublage %s – %s" % (target.upper(), voice["name"])}
    track = info["track"]
    duck_txt = ""
    if original in ("duck", "cut"):
        base_db = float(track.get("volume_db", 0.0) or 0.0)
        low = -96.0 if original == "cut" else base_db + duck_db
        attack, release = 0.12, 0.25
        spans = []
        for s in mix_in:
            a, b = s["start"], s["start"] + s["final_measured"]
            if spans and a - spans[-1][1] < (attack + release):
                spans[-1][1] = max(spans[-1][1], b)
            else:
                spans.append([a, b])
        pts = []
        for a, b in spans:
            pts += [(max(0.0, a - attack), base_db), (a, low), (b, low), (b + release, base_db)]
        # strictly increasing times (merged neighbours can touch)
        clean = []
        for t, v in pts:
            tt = int(round(t * TICKS))
            if clean and tt <= clean[-1][0]:
                tt = clean[-1][0] + 1
            clean.append((tt, v))
        cmd["duck"] = {"strip": track["id"], "points": [[t, v] for t, v in clean]}
        duck_txt = " · voix d'origine %s" % ("coupée" if original == "cut" else "atténuée de %d dB" % abs(round(duck_db)))
    n_ok = stats["n"]
    summary = "%d phrase%s doublée%s (%s, voix %s), %d raccourcie%s, %d accélérée%s/ralentie%s, %d dépassement%s%s" % (
        n_ok, "s" if n_ok > 1 else "", "s" if n_ok > 1 else "", target.upper(), voice["name"],
        stats["shortened"], "s" if stats["shortened"] > 1 else "", stats["stretched"], "s" if stats["stretched"] > 1 else "",
        "s" if stats["stretched"] > 1 else "", stats["overrun"], "s" if stats["overrun"] > 1 else "", duck_txt)
    if stats["skipped"]:
        summary += " · %d non doublée%s (temps imparti : relancez, le reste est en cache)" % (stats["skipped"], "s" if stats["skipped"] > 1 else "")
    cmd["note"] = summary
    for i, s in flagged:
        log("Phrase %d dépasse de %s sa place (%s disponibles, %s de voix à la vitesse maximale) : « %s »" % (
            i, fmt_s(s["overrun"]), fmt_s(s["room"]), fmt_s(s["final_measured"]), s["spoken"]))
    write_report(work, outp, segs, stats, info, voice, target, src_lang)
    log(summary)
    out({"id": "sequence.placeDub", "params": cmd})


def mix(items, t0, total, outp, ff, gain_db):
    os.makedirs(os.path.dirname(outp), exist_ok=True)
    groups = [items[i:i + 48] for i in range(0, len(items), 48)]
    parts = []
    for gi, g in enumerate(groups):
        args, filt = [], []
        for k, s in enumerate(g):
            args += ["-i", s["fit"]]
            ms = max(0, int(round((s["start"] - t0) * 1000)))
            filt.append("[%d:a]afade=t=in:d=0.008,adelay=%d|%d:all=1[a%d]" % (k, ms, ms, k))
        labels = "".join("[a%d]" % k for k in range(len(g)))
        target = outp if len(groups) == 1 else outp + ".part%d.wav" % gi
        post = "volume=%.2fdB,alimiter=limit=0.95,apad=whole_dur=%.3f,atrim=0:%.3f" % (gain_db, total, total)
        filt.append("%samix=inputs=%d:normalize=0:duration=longest,%s[o]" % (labels, len(g), post))
        script = target + ".filter.txt"
        with open(script, "w") as f:
            f.write(";\n".join(filt))
        ffmpeg(args + ["-filter_complex_script", script, "-map", "[o]", "-ar", "48000", "-ac", "2", "-c:a", "pcm_s16le", target], ff)
        os.remove(script)
        parts.append(target)
    if len(groups) > 1:
        args = []
        for p in parts:
            args += ["-i", p]
        ffmpeg(args + ["-filter_complex", "".join("[%d:a]" % i for i in range(len(parts))) + "amix=inputs=%d:normalize=0:duration=longest,alimiter=limit=0.95[o]" % len(parts),
                       "-map", "[o]", "-ar", "48000", "-ac", "2", "-c:a", "pcm_s16le", outp], ff)
        for p in parts:
            os.remove(p)


def write_report(work, outp, segs, stats, info, voice, target, src_lang):
    rows = []
    for i, s in enumerate(segs, 1):
        rows.append({"i": i, "start": s["start"], "end": s["end"], "slot": s.get("slot"), "room": s.get("room"), "src": s.get("src"),
                     "text": s.get("text"), "spoken": s.get("spoken"), "shortened": s.get("shortened"), "natural": s.get("d0"),
                     "natural_before_rephrase": s.get("d0_orig"), "factor": s.get("factor"), "final": s.get("final_measured"),
                     "overrun": s.get("overrun"), "skipped": s.get("skipped")})
    with open(outp[:-4] + ".report.json", "w", encoding="utf-8") as f:
        json.dump({"source_lang": src_lang, "target": target, "voice": voice.get("name"), "origin": info["origin"], "stats": stats, "segments": rows},
                  f, ensure_ascii=False, indent=1)


def run_choose(ctx):
    vb = Voicebox(ctx)
    cfg = load_config(ctx)
    try:
        vb.ensure()
        profiles = vb.profiles()
        if not profiles:
            raise Fail("Voicebox n'a aucun profil de voix")
        for p in profiles:
            log("Voix : " + label_of(p))
        target = str(opt(ctx, "target", "en")).lower()
        key = opt(ctx, "voice")
        p = vb.pick(profiles, key) if key else osascript_choose(profiles, "Voix pour le doublage (anglais) :")
        if not p:
            raise Fail("aucune voix choisie (pas de boîte de dialogue ici) : ajoutez --var voice=<nom>")
        cfg.setdefault("voices", {})[target] = p["id"]
        if flag(ctx, "mine"):
            cfg["my_voice"] = p["id"]
        save_config(ctx, cfg)
        log("Voix de doublage (%s) : %s" % (target.upper(), label_of(p)))
    finally:
        vb.stop_if_started()


def main():
    mode = sys.argv[1] if len(sys.argv) > 1 else "dub"
    try:
        ctx = json.load(sys.stdin)
    except ValueError:
        ctx = {}
    ctx.setdefault("vars", {})
    for a in sys.argv[2:]:  # entry arguments: a language code and/or "mine"
        if a == "mine":
            ctx["vars"].setdefault("mine", "1")
        elif a in ("en", "fr", "es", "de", "pt", "it", "ja", "zh", "ko"):
            ctx["vars"].setdefault("target", a)
    try:
        if mode == "choose-voice":
            run_choose(ctx)
        elif mode == "dub":
            run_dub(ctx, str(opt(ctx, "target", "en")).lower(), flag(ctx, "mine"))
        else:
            raise Fail("mode inconnu : " + mode)
    except Fail as e:
        log("Doublage : %s" % e)
        sys.exit(1)


if __name__ == "__main__":
    main()
