#!/usr/bin/env python3
"""Storyboard Maker, an FilmCraft extension program.

Reads the extension context (JSON) on standard input, the saved project next to it, and prints
FilmCraft commands (one JSON object per line) or {"log": ...} lines.

  storyboard.py frames clips     one still per shot (clip of the first video track that has clips)
  storyboard.py frames markers   one still per sequence marker
  storyboard.py sheet            contact sheet (PNG + PDF) of the stills with their titles
                                 (needs the Pillow package: pip3 install pillow)

Stills go to <project folder>/<project name>-storyboard/NNN.png with an index.json; the sheet is
<project folder>/<project name>-storyboard.png and .pdf.
"""
import json
import os
import sys

TICKS = 254016000000


def log(msg):
    print(json.dumps({"log": msg}, ensure_ascii=False))


def die(msg):
    log(msg)
    sys.exit(1)


def load(ctx):
    path = ctx.get("project")
    if not path or not os.path.isfile(path):
        die("save the project first: the storyboard reads the saved .fcproj")
    with open(path, encoding="utf-8") as f:
        doc = json.load(f)
    items = doc["project"]["items"]
    view = doc.get("view") or {}
    sid = str(view.get("active_sequence")) if view.get("active_sequence") is not None else None
    if sid not in items or "Sequence" not in items[sid]["kind"]:
        sid = next((k for k, v in items.items() if isinstance(v["kind"], dict) and "Sequence" in v["kind"]), None)
    if sid is None:
        die("the project has no sequence")
    return items, items[sid]["name"], items[sid]["kind"]["Sequence"]


def timecode(ticks, fps):
    frames = int(round(ticks / TICKS * fps))
    f = frames % int(round(fps))
    s = frames // int(round(fps))
    return "%02d:%02d:%02d:%02d" % (s // 3600, s // 60 % 60, s % 60, f)


def title_of(item):
    name = item.get("name") or ""
    base, ext = os.path.splitext(name)
    return base if ext and len(ext) <= 5 else name


def shots(items, seq):
    for tr in seq["video_tracks"]:
        clips = [c for c in tr["items"] if c.get("enabled", True)]
        if clips:
            out = []
            for c in sorted(clips, key=lambda c: c["start"]):
                it = items.get(str(c["item"]), {})
                t = c["start"] + min(TICKS // 2, c["duration"] // 2)
                out.append({"title": c.get("name") or title_of(it), "start": c["start"], "at": t, "duration": c["duration"]})
            return out
    return []


def marks(seq):
    out = []
    for m in sorted(seq["markers"], key=lambda m: m["start"]):
        title = m.get("name") or "Marker"
        if m.get("comment"):
            title += " - " + m["comment"]
        out.append({"title": title, "start": m["start"], "at": m["start"], "duration": m.get("duration", 0)})
    return out


def frames(ctx, what):
    items, seq_name, seq = load(ctx)
    plan = shots(items, seq) if what == "clips" else marks(seq)
    if not plan:
        die("no shot on the video tracks" if what == "clips" else "the sequence has no marker")
    folder = os.path.join(ctx["projectDir"], ctx["projectName"] + "-storyboard")
    os.makedirs(folder, exist_ok=True)
    for old in os.listdir(folder):
        if old.endswith(".png"):
            os.remove(os.path.join(folder, old))
    fps = seq["settings"]["frame_rate"]["num"] / seq["settings"]["frame_rate"]["den"]
    index = []
    for i, p in enumerate(plan, 1):
        name = "%03d.png" % i
        index.append({"file": name, "title": p["title"], "timecode": timecode(p["start"], fps), "seconds": p["start"] / TICKS})
        print(json.dumps({"id": "playhead.set", "params": {"time": p["at"]}}))
        print(json.dumps({"id": "file.exportFrame", "params": {"path": os.path.join(folder, name), "format": "png"}}, ensure_ascii=False))
    with open(os.path.join(folder, "index.json"), "w", encoding="utf-8") as f:
        json.dump({"sequence": seq_name, "frames": index}, f, ensure_ascii=False, indent=1)
    log("%d stills planned in %s; build the sheet with the entry 'Assembler la planche'" % (len(plan), folder))


def font(size):
    from PIL import ImageFont
    for p in ("/System/Library/Fonts/Helvetica.ttc", "/System/Library/Fonts/Supplemental/Arial.ttf",
              "C:/Windows/Fonts/arial.ttf", "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf"):
        try:
            return ImageFont.truetype(p, size)
        except OSError:
            pass
    return ImageFont.load_default(size)


def sheet(ctx):
    try:
        from PIL import Image, ImageDraw
    except ImportError:
        die("the contact sheet needs the Pillow package: pip3 install pillow")
    folder = os.path.join(ctx["projectDir"], ctx["projectName"] + "-storyboard")
    try:
        with open(os.path.join(folder, "index.json"), encoding="utf-8") as f:
            index = json.load(f)
    except OSError:
        die("no stills yet: run 'Exporter une image par plan' (or par marqueur) first")
    cols = int(ctx.get("vars", {}).get("columns", 3))
    cw, ch, pad, label_h, head = 480, 270, 24, 64, 90
    rows = (len(index["frames"]) + cols - 1) // cols
    W = pad + cols * (cw + pad)
    H = head + rows * (ch + label_h + pad) + pad
    img = Image.new("RGB", (W, H), "white")
    d = ImageDraw.Draw(img)
    d.text((pad, 28), "%s  (%d)" % (index["sequence"], len(index["frames"])), fill="black", font=font(34))
    missing = 0
    for i, fr in enumerate(index["frames"]):
        x = pad + (i % cols) * (cw + pad)
        y = head + (i // cols) * (ch + label_h + pad)
        try:
            th = Image.open(os.path.join(folder, fr["file"])).convert("RGB")
            th.thumbnail((cw, ch))
            img.paste(th, (x + (cw - th.width) // 2, y + (ch - th.height) // 2))
        except OSError:
            missing += 1
            d.rectangle([x, y, x + cw, y + ch], outline="red")
            d.text((x + 10, y + 10), "missing " + fr["file"], fill="red", font=font(20))
        d.rectangle([x - 1, y - 1, x + cw, y + ch], outline=(160, 160, 160))
        d.text((x, y + ch + 8), "%02d  %s" % (i + 1, fr["title"][:44]), fill="black", font=font(22))
        d.text((x, y + ch + 36), fr["timecode"], fill=(110, 110, 110), font=font(18))
    base = os.path.join(ctx["projectDir"], ctx["projectName"] + "-storyboard")
    img.save(base + ".png")
    img.save(base + ".pdf", "PDF", resolution=100.0)
    log("sheet: %s.png and %s.pdf (%d stills%s)" % (base, base, len(index["frames"]), ", %d missing" % missing if missing else ""))
    if missing:
        sys.exit(1)


def main():
    ctx = json.load(sys.stdin)
    mode = sys.argv[1] if len(sys.argv) > 1 else ""
    if mode == "frames" and len(sys.argv) > 2 and sys.argv[2] in ("clips", "markers"):
        frames(ctx, sys.argv[2])
    elif mode == "sheet":
        sheet(ctx)
    else:
        die("usage: storyboard.py frames clips|markers | sheet")


main()
