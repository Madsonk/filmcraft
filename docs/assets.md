# Your own resources: LUTs, sounds, music, titles (assets-dnd)

FilmCraft takes the resource library you already have (LUT packs, sound-effect and music banks,
titles) by drag and drop, from the Finder or from **Window ▸ Resources**.

## What a drop does

| Dropped file | On a video clip | On an empty track | Elsewhere (Project panel, ruler…) |
|---|---|---|---|
| LUT (`.cube`, `.3dl`) | the clip's Lumetri **Creative Look** (`lumetri.setLook`) | LUT library | LUT library (`lut.import`) |
| Sound (WAV, AIFF, MP3, M4A, FLAC, Ogg, Opus, CAF) | at the drop time, on the first **free** audio track | that track if free there, else the next free one below, else a new track | imported into the bin |
| Picture (video, still, title with alpha) | at the drop time, on the first free video track **above** the clip | that track if free, else the next free one above, else a new track | imported into the bin |
| `.mogrt` | its converted video if `mogrt_to_alpha` made one, else refused with the way to convert it | same | same |
| anything else (project, XML, captions) | imported as before | | |

A drop never overwrites a clip and is **one undo step** (import + new track + placement). The
dropped files are only read. Engine command: `assets.place {"paths":[…],"clip"?:id,"track"?:id|"A1","time"?:ticks,"bin"?:id}`
(CLI, MCP and control channel); it answers `placed` (`look` / `library` / `placed` / `imported`)
and `refused` (`code`: `mogrt`, `lutOnSound`, `failed`).

macOS reports no pointer position during a drag from the Finder (winit 0.30): the app asks AppKit
(`mouseLocationOutsideOfEventStream`, `apps/filmcraft/src/asset_hooks.rs`) at the drop, so the
target is the clip actually under the pointer. While files hover the window a hint says what the
drop will do ("Creative Look on PANA6668", "On a free audio track at this time").

## Window ▸ Resources

Four tabs: **LUTs · Sound Effects · Music · Titles**, a search field (every word must match the
file name or its folder), the folder button (one folder per tab) and Refresh.

- **Indexes are built in the background** and cached in `<data>/assets/index-<tab>.json`: the
  panel shows the cache at once, a walk re-lists only folders whose modification time changed, and
  publishes its progress every 3 s. Folders starting with `.` or `_` (Bridge caches, preview
  folders) are skipped. Nothing is ever listed on the UI thread: libraries on slow USB disks or a
  NAS stay usable.
- **Sounds**: if the `/sfx` tool's SQLite index exists (`~/audio-tools/audio_index.db`, table
  `sons`: path, duration, family), it seeds the index through the system `sqlite3` (read only), so
  durations and families show at once. ▶ auditions the file (macOS `afplay`, one sound at a time).
  Double-click = at the playhead on a free audio track. Tags written in file names
  (`name [tag · tag · 1.4s].wav`) are shown in the tooltip.
- **LUTs**: hover a row to preview the LUT on the current shot (selected video clip, else the
  clip under the playhead): before / after, rendered small (320 px) with the look slot switched
  off, the LUT applied on the CPU; nothing is written to the project. The LUT file is read in a
  background thread. Double-click = Creative Look of that shot.
- **Titles**: entries of `<titles folder>/index.json`, written by `mogrt_to_alpha.py`. A title
  converted to a video with transparency is placed above the shot; a title not converted yet is
  marked *preview only*: its preview video (or thumbnail) can be dropped as a **reference**, never
  passed off as the title.

Settings: `<data>/assets/settings.json` (`luts`, `sfx`, `music`, `titles`, `soundDb`). Defaults
point at Madson's library when it is mounted (`/Volumes/Qnap bureau/6. Garden/Ressources/03 LUTS`,
`02 SFX`, `01 MUSIC`; titles in `/Volumes/Studio Mad/mogrt-alpha`), else empty.

Control channel: `ui.dropFiles {"paths":[…], "id"|"x","y"}` drops files at a window point as the
Finder does; `ui.assets` returns the last drop (parameters and answer), the open tab, the hovered
LUT and the libraries' state. Automation ids: `resources.tab.<luts|sfx|music|titles>`,
`resources.search`, `resources.folder`, `resources.refresh`, `resources.row.<n>`,
`resources.play.<n>`, `resources.preview`.

## `.mogrt`: why FilmCraft does not open them, and what to do instead

A `.mogrt` is a ZIP holding `definition.json` (name, size, duration, editable controls, fonts),
previews (`thumb.png`, often `thumb.mp4`, 640×360 H.264 **without** alpha) and the template
itself: `project.aegraphic` (a ZIP with an **After Effects project**, binary `.aep`, plus footage)
or `project.prgraphic` (a **Premiere** project, gzipped XML). Playing one means running the
After Effects or Premiere project: FilmCraft does not, and never opens the template.

`mogrt_to_alpha.py` (Madson's vault, `5. Systeme/scripts/ae/`) bridges it:

- `list` — packs of the library and their counts;
- `index PACK…` — reads each `.mogrt` as a ZIP (nothing executed): controls with their default
  texts and colours, fonts (with the ones missing on this Mac), size, duration, and extracts the
  previews; writes `<out>/<pack>/index.json` and `<out>/index.json` (the Titles tab);
- `render PACK|MOGRT…` — with After Effects installed and signed in: renders each After Effects
  template with its default texts through `aerender`, output module "High Quality with Alpha"
  (QuickTime ProRes 4444 + alpha), checks the alpha channel with `ffprobe`, records the video in
  the index. Premiere templates (`prgraphic`) cannot be rendered by `aerender`.

The Premiere templates' text layers are readable JSON inside the project XML (text, font, size,
colours), which would allow a native `.fcgt` for simple static titles: not built yet.
