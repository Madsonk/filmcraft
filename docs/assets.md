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

## Dropping on an empty place: a new track

As in Premiere, a drop in the room **above the top video track** (or on its top edge) makes a new
video track there, a drop **below the last audio track** (or on its bottom edge) a new audio track,
and a drop **on the edge between two tracks** of the same kind inserts a track between them. An
accent line shows where while the pointer is there, and the hint says "On a new video track V3".
The track and the placement are **one undo step**. This works for Window ▸ Resources (LUT library
excepted: a LUT needs a shot), files from the Finder, and items from the Project panel
(`assets.place` / `timeline.place` take `newTrack: {kind: "video"|"audio", index}`, index = how
many tracks of that kind come before the new one).

## Window ▸ Resources

Four tabs: **LUTs · Sound Effects · Music · Titles**, a search field (every word must match the
file name or its folder), the folder button (one folder per tab) and Refresh.

- **Indexes are built in the background** and cached in `<data>/assets/index-<tab>.json`: the
  panel shows the cache at once, a walk re-lists only folders whose modification time changed, and
  publishes its progress every 3 s. Folders starting with `.` or `_` (Bridge caches, preview
  folders) are skipped. Nothing is ever listed on the UI thread: libraries on slow USB disks or a
  NAS stay usable.
- **Sounds**: if the `/sfx` tool's SQLite index exists (`~/audio-tools/audio_index.db`, table
  `sons`: path, duration, family, and when present bpm, key, envelope, loop, peaks), it seeds the
  index through the system `sqlite3` (read only), so durations, families, tempo and key show at
  once. Double-click / Enter = at the playhead on a free audio track. Tags written in file names
  (`name [tag · tag · 1.4s].wav`) are shown in the tooltip.
- **Waveforms** (ressources-v2): each sound row draws its waveform (1000 min/max columns, about
  2 KB per sound). They are computed once, in one background thread, one file at a time (the
  library may be on a NAS), visible rows first, then the rest of the library; the sweep pauses
  while the timeline plays. Kept in `<peaks dir>/peaks-v1.bin` (default
  `/Volumes/Data/FilmCraft-cache/peaks`, setting `peaksDir`); waveforms already in the sound
  index (`peaks` column, written by `index_build.py --peaks`) are taken from there. A file that
  cannot be decoded (for example a macOS `._` AppleDouble file) shows *not a readable sound*.
- **Light previews** (previews-cache): the same single reading that makes a waveform also makes
  the sound's preview, a mono AAC-LC file (32 kHz, 96 kbps, `.m4a`) on the local disk
  (`/Volumes/Data/FilmCraft-cache/previews`, setting `previewsDir`; `previews: false` turns them
  off): a sound effect whole, a music from 30 % of its length for 30 s. Key = path + size + date
  of the original (a changed file is made again); index `previews-v1.jsonl` (failures recorded,
  not retried). Listening on hover and ▶ read the preview when it covers the point asked, else
  the original; **dropping a sound always places the original**. The status line counts
  `previews: N/M`; right-click on it empties the preview cache (the waveforms are kept).
  Night run: `filmcraft-cli assets.buildPreviews --kind sfx|music [--limit N] [--root DIR]
  [--cache DIR] [--list FILE]` (one file at a time, resumable; `<cache>/previews/STOP` stops it
  after the current file).
- **Listening** happens inside the app (no `afplay`): play head on the waveform, position, pause.
  **On hover** (after 150 ms still; button ♪, settings `hoverSfx` / `hoverMusic`, on by default):
  sound effects from the start, music from 30 % of its length. Moving along the waveform scrubs,
  a click on it plays from that point, leaving the row stops. Listening is refused while the
  timeline plays, and stops when the timeline starts.
- **Search, filters, favourites**: search by relevance (words in the name count more than words
  in the folder), filters family / duration / tempo range / key (relative key included) / loops
  only, sort (relevance, name, duration, tempo), ★ favourites and ↺ recently used (the last 50
  dropped or applied), kept in `<data>/assets/library-state.json`.
- **Keyboard** (panel focused): ↑ ↓ select (and listen when listening on hover is on), Space play
  / pause, Enter apply, Esc stop then clear the search, `F` favourite, `/` search.
- **LUTs**: hover a row to preview the LUT on the current shot (selected video clip, else the
  clip under the playhead): before / after, rendered small (320 px) with the look slot switched
  off, the LUT applied on the CPU; nothing is written to the project. The LUT file is read in a
  background thread. Double-click = Creative Look of that shot.
- **Titles**: entries of `<titles folder>/index.json`, written by `mogrt_to_alpha.py index`
  (which only reads the ZIPs). A title made in Premiere is marked *editable*: dropping it converts
  it (once, cached by path + size + date) and places it as an editable title; **Convert all
  shown (N)** converts the listed ones in a background thread. A title made in After Effects is
  marked *preview only*: its preview can be dropped as a **reference**, never passed off as the
  title. Button ▦ switches between a list and a grid of thumbnails (setting `titlesGrid`). See
  [mogrt-import.md](mogrt-import.md).

Settings: `<data>/assets/settings.json` (`luts`, `sfx`, `music`, `titles`, `soundDb`). Defaults
point at Madson's library when it is mounted (`/Volumes/Qnap bureau/6. Garden/Ressources/03 LUTS`,
`02 SFX`, `01 MUSIC`; titles in `/Volumes/Studio Mad/mogrt-alpha`), else empty.

Control channel: `ui.dropFiles {"paths":[…], "id"|"x","y"}` drops files at a window point as the
Finder does; `ui.assets` returns the last drop (parameters and answer), the open tab, the hovered
LUT, the libraries' state, the audition (path, playing, position, progress), the selection, the
waveform store (known, computed, failed, queued, visiblePending, lastMs) and the filters.
Automation ids: `resources.tab.<luts|sfx|music|titles>`, `resources.search`, `resources.folder`,
`resources.refresh`, `resources.favorites`, `resources.recents`, `resources.hoverPlay`,
`resources.view`, `resources.filter.<family|duration|bpm|key|loops|sort>`, `resources.row.<n>`,
`resources.play.<n>`, `resources.star.<n>`, `resources.wave.<n>`, `resources.preview`. Control
methods: `ui.assets.audition {path, frac?}` (listen as hover / ▶ do), `ui.assets.clearPreviews`.

## `.mogrt` titles

See [mogrt-import.md](mogrt-import.md): titles made in Premiere become native, editable FilmCraft
titles; titles made in After Effects stay *preview only*.
