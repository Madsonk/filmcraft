# Your own resources: LUTs, sounds, music, titles (assets-dnd)

FilmCraft takes the resource library you already have (LUT packs, sound-effect and music banks,
titles) by drag and drop, from the Finder or from **Window ▸ Resources**.

## What a drop does

| Dropped file | On a video clip | On an empty track | Elsewhere (Project panel, ruler…) |
|---|---|---|---|
| LUT (`.cube`, `.3dl`) | the clip's Lumetri **Creative Look** (`lumetri.setLook`) | LUT library | LUT library (`lut.import`) |
| Sound (WAV, AIFF, MP3, M4A, FLAC, Ogg, Opus, CAF) | at the drop time, on the first **free** audio track | that track if free there, else the next free one below, else a new track | imported into the bin |
| Picture (video, still, title with alpha) | at the drop time, on the first free video track **above** the clip | that track if free, else the next free one above, else a new track | imported into the bin |
| `.mogrt` made in Premiere | converted once into an editable title (`.fcgt`), placed on the first free video track **above** | same | converted into Graphics Templates |
| `.mogrt` made in After Effects | refused: *preview only* (FilmCraft cannot run an After Effects project) | same | same |
| anything else (project, XML, captions) | imported as before | | |

A drop never overwrites a clip and is **one undo step** (import + new track + placement). The
dropped files are only read. Engine command: `assets.place {"paths":[…],"clip"?:id,"track"?:id|"A1","time"?:ticks,"bin"?:id}`
(CLI, MCP and control channel); it answers `placed` (`look` / `library` / `placed` / `imported`)
and `refused` (`code`: `afterEffects`, `mogrt` (unreadable), `lutOnSound`, `failed`). A converted title's `conversion` lists `missingFonts`, `warnings` and `lost`.

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
- **Titles**: entries of `<titles folder>/index.json`, written by `mogrt_to_alpha.py index`
  (which only reads the ZIPs). A title made in Premiere is marked *editable*: dropping it converts
  it (once, cached by path + size + date) and places it as an editable title; **Convert all
  shown (N)** converts the listed ones in a background thread. A title made in After Effects is
  marked *preview only*: its preview can be dropped as a **reference**, never passed off as the
  title. See [mogrt-import.md](mogrt-import.md).

Settings: `<data>/assets/settings.json` (`luts`, `sfx`, `music`, `titles`, `soundDb`). Defaults
point at Madson's library when it is mounted (`/Volumes/Qnap bureau/6. Garden/Ressources/03 LUTS`,
`02 SFX`, `01 MUSIC`; titles in `/Volumes/Studio Mad/mogrt-alpha`), else empty.

Control channel: `ui.dropFiles {"paths":[…], "id"|"x","y"}` drops files at a window point as the
Finder does; `ui.assets` returns the last drop (parameters and answer), the open tab, the hovered
LUT and the libraries' state. Automation ids: `resources.tab.<luts|sfx|music|titles>`,
`resources.search`, `resources.folder`, `resources.refresh`, `resources.row.<n>`,
`resources.play.<n>`, `resources.preview`.

## `.mogrt` titles

See [mogrt-import.md](mogrt-import.md): titles made in Premiere become native, editable FilmCraft
titles; titles made in After Effects stay *preview only*.
