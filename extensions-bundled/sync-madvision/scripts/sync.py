#!/usr/bin/env python3
"""Synchro audio MadVision: aligns the selected clips on a master clip with the MadVision sync
service (POST /sync/analyze on 127.0.0.1:8765, audio fingerprints + consensus), then prints one
`timeline.move` that FilmCraft runs after this program exits (one undo step).

Reads the SAVED .fcproj (save first). Standard library only.

  stdin : the FilmCraft exec context (selection, project, vars)
  argv  : "apply" (move the clips) or "analyze" (report only)
  vars  : master=<clip id> (default: the longest selected clip), url=<service base URL>

The service token is read from ~/.madvision/config.json (`serviceToken`) and sent in the
X-MadVision-Token header; it is never printed.
"""
from __future__ import annotations

import json
import os
import sys
import urllib.error
import urllib.request

TICKS = 254016000000  # per second


def log(msg):
    print(json.dumps({"log": msg}, ensure_ascii=False), flush=True)


def die(msg):
    log(msg)
    sys.exit(1)


def load(ctx):
    path = ctx.get("project")
    if not path or not os.path.isfile(path):
        die("Enregistrez le projet d'abord : la synchro lit le .fcproj enregistré.")
    with open(path, encoding="utf-8") as f:
        doc = json.load(f)
    items = doc["project"]["items"]
    view = doc.get("view") or {}
    sid = str(view.get("active_sequence")) if view.get("active_sequence") is not None else None
    if sid not in items or "Sequence" not in items[sid]["kind"]:
        die("aucune séquence active dans le projet enregistré")
    return items, items[sid]["kind"]["Sequence"]


def media_path(items, item_id):
    it = items.get(str(item_id)) or {}
    kind = it.get("kind")
    if not isinstance(kind, dict) or "Media" not in kind:
        return None
    ref = kind["Media"].get("media") or {}
    f = ref.get("File") if isinstance(ref, dict) else None
    return f.get("path") if isinstance(f, dict) else None


def clips_of(seq):
    """clip id -> (track id, clip dict) for every clip of the sequence."""
    out = {}
    for tr in seq["video_tracks"] + seq["audio_tracks"]:
        for c in tr["items"]:
            out[c["id"]] = (tr["id"], c, tr)
    return out


def token():
    cfg = os.path.expanduser("~/.madvision/config.json")
    try:
        with open(cfg, encoding="utf-8") as f:
            t = json.load(f).get("serviceToken")
    except (OSError, ValueError):
        t = None
    if not t:
        die("jeton du service MadVision introuvable (~/.madvision/config.json, serviceToken) : le service a-t-il démarré une fois ?")
    return t


def analyze(url, master, rushes):
    body = json.dumps({"masterPath": master, "rushPaths": rushes}).encode()
    rq = urllib.request.Request(url.rstrip("/") + "/sync/analyze", data=body,
                                headers={"Content-Type": "application/json", "X-MadVision-Token": token()})
    try:
        with urllib.request.urlopen(rq, timeout=3500) as r:
            return json.load(r)
    except urllib.error.HTTPError as e:
        try:
            why = json.load(e).get("error", "")
        except ValueError:
            why = ""
        die("service MadVision : HTTP %d %s" % (e.code, why))
    except (urllib.error.URLError, OSError) as e:
        die("service MadVision injoignable (%s) : lancer `launchctl kickstart -k gui/$(id -u)/com.madson.madvision.sync`" % getattr(e, "reason", e))


def tc(ticks, fps):
    s = ticks / TICKS
    sign = "-" if s < 0 else ""
    s = abs(s)
    return "%s%02d:%02d:%06.3f (image %.2f)" % (sign, int(s // 3600), int(s // 60 % 60), s % 60, s * fps)


def main():
    mode = sys.argv[1] if len(sys.argv) > 1 else "apply"
    ctx = json.load(sys.stdin)
    vars_ = ctx.get("vars") or {}
    items, seq = load(ctx)
    fps = seq["settings"]["frame_rate"]["num"] / seq["settings"]["frame_rate"]["den"]
    clips = clips_of(seq)
    sel = [c for c in (ctx.get("selection") or []) if c in clips]
    if len(sel) < 2:
        die("sélectionnez au moins deux clips : le master (le plus long, ou --var master=<id>) et les clips à caler")
    # one clip per linked group: the others follow through `linked`
    seen_links, picked = set(), []
    for cid in sel:
        link = clips[cid][1].get("link")
        if link is not None and link in seen_links:
            continue
        if link is not None:
            seen_links.add(link)
        picked.append(cid)
    want = vars_.get("master")
    if want not in (None, ""):
        try:
            master = int(want)
        except ValueError:
            die("master=%r : un numéro de clip est attendu" % want)
        if master not in clips:
            die("le clip master %d n'est pas dans la séquence active" % master)
        mlink = clips[master][1].get("link")
        picked = [c for c in picked if c != master and (mlink is None or clips[c][1].get("link") != mlink)]
    else:
        master = max(picked, key=lambda c: clips[c][1]["duration"])
        picked = [c for c in picked if c != master]
    mtrack, mc, _ = clips[master]
    mpath = media_path(items, mc["item"])
    if not mpath:
        die("le master n'est pas un fichier média")
    if abs(mc.get("speed", 1.0) - 1.0) > 1e-9 or mc.get("reverse"):
        die("le master est accéléré/ralenti ou inversé : la synchro suppose une vitesse de 100 %")
    rushes, skipped = [], []
    for cid in picked:
        c = clips[cid][1]
        p = media_path(items, c["item"])
        if not p:
            skipped.append("%s : pas un fichier média" % c["name"])
        elif p == mpath:
            skipped.append("%s : même fichier que le master" % c["name"])
        elif abs(c.get("speed", 1.0) - 1.0) > 1e-9 or c.get("reverse"):
            skipped.append("%s : vitesse différente de 100 %%" % c["name"])
        else:
            rushes.append((cid, p))
    for s in skipped:
        log("ignoré — " + s)
    if not rushes:
        die("aucun clip à caler")
    url = vars_.get("url") or "http://127.0.0.1:8765"
    log("master : %s (clip %d) ; %d clip(s) à analyser par le service MadVision…" % (mc["name"], master, len(rushes)))
    paths = sorted({p for _, p in rushes})
    res = analyze(url, mpath, paths)
    by_path = {r["path"]: r for r in res.get("results", [])}
    moves = [{"clip": master, "track": mtrack, "time": mc["start"]}]
    report = []
    for cid, p in rushes:
        track, c, tr = clips[cid]
        r = by_path.get(p)
        if not r or r.get("status") != "sync" or r.get("offsetSeconds") is None:
            why = (r or {}).get("reason") or (r or {}).get("error") or (r or {}).get("status") or "pas de résultat"
            log("non calé — %s : %s (laissé en place)" % (c["name"], why))
            report.append({"clip": cid, "name": c["name"], "status": (r or {}).get("status", "missing")})
            continue
        off = round(r["offsetSeconds"] * TICKS)
        # media time m of the master and r of the rush meet when m = r + offset:
        # start_rush = start_master + in_rush - in_master + offset
        start = mc["start"] + c["source_in"] - mc["source_in"] + off
        # a clip that is not moving and sits where this one would land would be overwritten
        end = start + c["duration"]
        moving = set(sel)
        hit = [o for o in tr["items"] if o["id"] not in moving and o["start"] < end and start < o["start"] + o["duration"]]
        if hit:
            log("non déplacé — %s : la place (%s) est occupée sur sa piste par %s" % (c["name"], tc(start, fps), ", ".join(o["name"] for o in hit)))
            report.append({"clip": cid, "name": c["name"], "status": "blocked", "offsetSeconds": r["offsetSeconds"]})
            continue
        moves.append({"clip": cid, "track": track, "time": start})
        log("%s : décalage %+.4f s sur le master (%s, consensus %s, confiance %s) → début %s, déplacé de %+.3f s"
            % (c["name"], r["offsetSeconds"], r.get("method", "?"), r.get("consensus", "?"), r.get("confidence", "?"),
               tc(start, fps), (start - c["start"]) / TICKS))
        report.append({"clip": cid, "name": c["name"], "status": "sync", "offsetSeconds": r["offsetSeconds"], "start": start, "was": c["start"]})
    out = os.path.join(ctx.get("dataDir") or os.path.expanduser("~"), "sync-madvision-last.json")
    try:
        with open(out, "w", encoding="utf-8") as f:
            json.dump({"master": {"clip": master, "path": mpath, "start": mc["start"]}, "results": report, "service": res}, f, ensure_ascii=False, indent=1)
    except OSError:
        pass
    if mode == "apply" and len(moves) > 1:
        print(json.dumps({"id": "timeline.move", "params": {"moves": moves, "linked": True}}))
        log("%d clip(s) calé(s) en une seule étape (Annuler pour revenir) ; détail : %s" % (len(moves) - 1, out))
    elif mode == "apply":
        log("rien à déplacer")
    else:
        log("analyse seule : rien n'a bougé ; détail : %s" % out)


if __name__ == "__main__":
    main()
