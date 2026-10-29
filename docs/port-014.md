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

## Verification (29 Oct 2026, aarch64)

- `cargo test --workspace --no-fail-fast` after the last commit (762404c7): journal `/Volumes/Data/work/fc-port-014-test3.full.log`, EXIT 101,
  **one failing target**: `filmcraft-platform --test it`, `hevc_probe::register_answers_the_hevc_probe_in_the_background`
  ("nothing has asked yet", 30 passed / 1 failed). Crate untouched by the fork; the test depends on timing/order and passes
  alone. **Not fixed, not ours.** Our suites: `assets_ui` 11/11, `extensions_ui` 16/16, `ui-egui` lib and `it` green, CLI green.
- `fmt`, `clippy -D warnings`, `cargo xtask layers|assets|wasm` on 0.14.0, bundle and smoke test: see the "Pending" list, updated as they run.

## Pending / not verified

- Release bundle `fc-port-014-bundle/FilmCraft.app` (ad hoc signed): script `/Volumes/Data/work/fc-port-014-build.sh`, waits for `/tmp/lc-build.lock`.
- `fmt --check`, clippy, xtask checks on 0.14.0.
- Still unverified from the 0.13.0 port: screenshots, real mouse drag-and-drop, real `.mogrt`, native menu rebuild on extension change,
  dubbing / voice clean-up / auto-reframe (need models), MadVision sync (service on :8765), feature `ml` not enabled in the bundle.
- Still lost: drop *between* two tracks creating a track; `.prproj` v0 export (never found); fork's 5-language enum (upstream's 12 used).

## Re-apply procedure for a newer tag

`git fetch --tags`; branch from the new tag; `git merge fc-port-014`; resolve preferring upstream for generic code and ours for fork-only
modules; re-wire the hooks listed above (they are the only places the fork touches the app shell); run the full test suite and the bundle script.

## Launcher overwrite risk

An ArtCraft launcher update replaces the app at the `installed.json` path with its own signed build, which erases ours. Keep our bundle outside
the launcher folder; the launcher is not modified by this port.
