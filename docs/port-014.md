# Port of the Madson fork (FilmCraft-Ext 0.5.0-ext-all-v6.6) onto upstream v0.14.0

Status (29 Oct 2026): **merged, compiles, workspace tests green except one upstream flake; bundle pending** (see
"Verification"). Branch `fc-port-014` = tag `v0.14.0` + merge of `fc-port-013` (itself tag `v0.13.0` + the fork).
Rule from Madson: every improvement lands on the **latest launcher version** (the ArtCraft launcher installs
FilmCraft 0.14.0). Decisions of [`port-013.md`](port-013.md) still hold (its decision table is the reference: upstream
first, keep from ours only what it adds); this page lists what 0.14.0 changed and how it was re-wired.

## What 0.14.0 restructured (and how our features were re-attached)

| 0.14.0 change | Effect on the fork | Resolution |
|---|---|---|
| `ui-egui/src/lib.rs` split into `app.rs` + `app/{commands,control,files,frame,gpu,ime,input,keys,playback,prefs,textures,window}.rs` | our hooks in `lib.rs` had no home | re-injected in the new modules (below) |
| File drops reworked (`window_files`, `pending_timeline_drop`, drop-import pref) | dropped LUT/sound/`.mogrt` no longer reached `assets::drop_files` | `app/input.rs::import_dropped` partitions paths: `AssetKind::{Lut,Sound,Mogrt}` go to `assets::drop_files` (pointer from `assets::os_pointer`), everything else stays on upstream's path (stills/video unchanged) |
| Finder opens (`application:openFiles:`) | upstream pass did not poll our hook | `app/input.rs::handle_opened_files` (last `.fcproj` opened via `file.open`, the rest via `file.import`), called after `handle_drops` |
| New docking (`craft_ui::docking`: `DockArea`, `Node`, `Layout{root,floating}`) | our `WorkspacePrefs.live` (layout kept after a restart) had no code | `app/window.rs::{keep_live_layout, live_layout}`, `set_workspace` clears `live`, restore on launch in `app/prefs.rs`, save when a panel is relocated in `dock_ui.rs::apply_dock_all` |
| New `handle_shortcuts` | Resources keyboard and menu rebuild were lost | `app/keys.rs`: `panels::resources::keyboard` when Resources is focused; bindings rebuilt when `extensions.rev` changes |
| `ItemKind::Media(Box<MediaClip>)` | 4 test files did not compile | `Box::new(...)` in `platform/tests/it/{hardware_encode,nvenc_hevc_export}.rs`, `platform/tests/nvenc_hevc_hdr_export.rs`, `render/src/filmlook_preview_tests.rs` (upstream's own platform tests were already broken by it) |
| egui 0.37 | merge conflicts in `Cargo.toml`/UI | upstream's versions kept |
| Workspace tests moved to the new dock model | `workspaces_ui.rs` used old tree types | `group_of` now walks `craft_ui::docking::Node` |

## Function-by-function comparison, fork on 0.13.0 → fork on 0.14.0 (proof: `git diff fc-port-013 HEAD --numstat -- <path>`)

Fork-only modules are **byte-identical** between the 0.13.0 port and this branch (0 lines added, 0 removed): crates `extensions`, `ml`, `nn`,
`mlsearch`, `mlrestore`, `mlfetch`, `modelstore`, `voice`, `reframe`, `narration`; `extensions-bundled/`; engine `assets`, `sound_previews`,
`mogrt_import`, `shot_match`, `dub`, `word_captions`, `voice_clean`, `auto_reframe`, `beats`, `captions_translate`; `ui-egui/src/assets/`,
`panels/resources.rs`, `panels/extensions.rs`; `apps/filmcraft/src/open_files.rs`; `apps/filmcraft-cli/src/ext.rs`. Nothing dropped there.

Shared files where upstream also moved (the delta is checked against upstream's own 0.13→0.14 delta, so our additions are intact):

| File(s) | Port vs v0.14.0 (ours only) | Upstream 0.13→0.14 |
|---|---|---|
| `render/src/vfx` (Film Look, `filmlook.rs`, `l1path.rs`) | +1572 / −7 (all additions, plus 8 lines of hooks in `lights.rs`/`temporal.rs`) | +63 / −554 |
| `color/src/lut.rs` (`resampled`, `premiere_parity`, `PREMIERE_SIZE`) | +68 / −0 | +17 / −1 |
| `speech/src/whisper` (initial prompt, turbo, BPE `encode`) | +156 / −5 | +15 / −412 |
| `ui-egui/src/i18n/fr.tsv` (our 180 keys appended) | +180 / −0 | +45 / −3 |

App shell (`ui-egui`, replaced by upstream's `app/*` split): the hooks are re-wired by hand, see the first table; each one has a test that passed
(`assets_ui` drops/keyboard, `extensions_ui`, `workspaces_ui` layout kept after a restart).

## `hevc_probe`: not a flake, an upstream test-layout bug (fixed here)

`crates/platform/tests/it/hevc_probe.rs` asserts `!hevc_probed()` at its start and says "one test in this file on purpose". In 0.14.0 upstream
moved it into the single `it` binary, where the other tests (`hardware_encode`…) ask `hevc_available()` first. Proof (compiled `it` binary,
this machine): the test alone passes 5 times out of 5; the full `it` binary fails 3 times out of 3 (`nothing has asked yet`) and also with
`--test-threads=1`. The file is identical to v0.14.0 (`git diff v0.14.0 HEAD -- crates/platform/tests/it/hevc_probe.rs` empty before the fix), so it is
upstream's. Fix: the test is moved to its own binary `crates/platform/tests/hevc_probe.rs` (same convention as `nvenc_hevc_warm.rs`), `mod hevc_probe;` removed from `tests/it/main.rs`.
Result of the rerun: see the verification list.

## Verification (29 Oct 2026, aarch64; outputs from the commands)

- Full `cargo test --workspace --no-fail-fast` before the last fixes (`fc-port-014-test3.full.log`): one failing target, `hevc_probe` (above). Our suites: `assets_ui` 11/11,
  `extensions_ui` 16/16, `ui-egui` lib and `it`, CLI: green.
- `hevc_probe` after the move: `cargo test -p filmcraft-platform --test hevc_probe --test it`: 1 passed, then `it` 30 passed / 0 failed, EXIT 0 (`fc-port-014-hevc.log`).
- Gates (`fc-port-014-gates.log`): `cargo xtask layers` EXIT 0, `assets` EXIT 0 (69 assets attributed), `wasm` EXIT 0. `clippy -D warnings` first pass EXIT 101: one `wildcard_imports`
  (`project/src/effect/filmlook.rs`, `use super::*`), fixed with an explicit list (035d8923). `cargo fmt --check` first pass: one blank line in `timeline/layout.rs`, fixed (8220f38a).
- Clippy needed several rounds (it stops at the first failing crate; the last passes used `--keep-going`): 2 `wildcard_imports` (Film Look), `too_many_arguments` in `sound_previews`, `scunet`, `rvm`, `curve_editor`,
  24 deprecated `egui::Id::new` (→ `Id::unique`, as upstream), 5 `egui::ComboBox` (→ `craft_ui::ComboBox`, per `clippy.toml`), a test module not last in `app/input.rs`,
  and **3 `cognitive_complexity` silenced with a justified `#[allow]`** (Resources `show` and its row-list closure, the `.mogrt` import test): known debt, not split.
- Last gates, read from the logs (`fc-port-014-pkg/`): `cargo clippy --workspace --all-targets --keep-going -j2 -- -D warnings` EXIT 0 (`clippy5.log`), `cargo fmt --check` EXIT 0,
  `cargo xtask wasm` EXIT 0, `layers` EXIT 0, `assets` EXIT 0.
- Tests per crate (`cargo test --no-fail-fast -j2 -p <crate>`, all EXIT 0, 0 failed): project 70, engine 771, render 231, color 41, speech 18, audio-dsp 133, extensions 38, ui-egui 859 (after the test fix below), filmcraft 77,
  cli 45, platform 65, time 23, edit 77, export 137, media 94, text 45, captions 94, ml 4, mlrestore 5, voice 4, reframe 9. `cargo test --workspace` in one go was abandoned: it hung (cargo stuck with no child, killed) and the machine rebooted twice meanwhile.
- `extensions_ui::cancelling_a_background_transcription…` failed once in a loaded run: our own race (the fake recogniser lasted 800 ms, the run finished before the cancel click). It now lasts 16 s and is cancelled at the first progress step; `extensions_ui` 3 runs out of 3: 16 passed, 0 failed.
- Bundle `/Volumes/Data/work/fc-port-014-bundle/FilmCraft.app` (release, built in 31 min 39 s, built before the two small fixes above, which change no behaviour): `Info.plist` OK,
  `CFBundleShortVersionString` 0.14.0, id `ai.storyteller.filmcraft`, ad hoc signature, `codesign --verify --strict`: valid on disk, satisfies its Designated Requirement. CLI `filmcraft-cli --help` lists `extensions <list|install|remove|run|validate|pack>`.
- Smoke test on a **copy** of the bundle (`/tmp/fc014-smoke`, control server on port 9877, 64-hex token): app starts ("FilmCraft 0.14.0 (macos aarch64)", VideoToolbox available);
  `ui.menu.list` returns 500 commands, 103 under Extensions (Montage auto, Beat Edit…); `ui.panel.show Resources` ok; `engine.execute sequence.addEdit` ok; `ui.screenshot` 1600×953 shows the Resources panel
  (tabs LUT / Bruitages / Musique / Titres, 627 LUTs) and the demo project. The app offered to recover an old unsaved session from the user's real data dir: not accepted, app killed, nothing saved.

## Not verified

- Real mouse drag-and-drop of files from Finder, real `.mogrt`, native menu rebuild on extension change, dubbing / voice clean-up / auto-reframe (need models; feature `ml` not enabled in the bundle), MadVision sync (service on :8765).
- The bundle is ad hoc signed (the launcher's own build is Developer ID signed and notarized), so Gatekeeper may block it on another Mac.
- Still lost: drop *between* two tracks creating a track; `.prproj` v0 export (never found); fork's 5-language enum (upstream's 12 used).

## Re-apply procedure for a newer tag

`git fetch --tags`; branch from the new tag; `git merge fc-port-014`; resolve preferring upstream for generic code and ours for fork-only
modules; re-wire the hooks listed above (they are the only places the fork touches the app shell); run the full test suite and the bundle script.

## Launcher overwrite risk

An ArtCraft launcher update replaces the app at the `installed.json` path with its own signed build, which erases ours. Keep our bundle outside
the launcher folder; the launcher is not modified by this port.
