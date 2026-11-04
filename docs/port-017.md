# Port of the Madson fork (FilmCraft-Ext) onto upstream v0.17.0

Status (4 Nov 2026): **merged, compiles, clippy/fmt/xtask green, tests green except two upstream tests that are fragile in parallel (below)**.
Branch `fc-port-017` = branch `fc-port-014` (tag `v0.14.0` + the fork) + merge of tag `v0.17.0` (eb59ec09, 904 commits / 560 files since 0.14.0), merge commit `443473be`,
compile fixes `a65ca340`. Rule from Madson (3 Nov): the ArtCraft launcher is dropped, our own versions are updated directly, always to the latest upstream.
Decisions of [`port-013.md`](port-013.md) and [`port-014.md`](port-014.md) still hold (upstream first, keep from ours only what it adds).

## What 0.17.0 changed in the places the fork touches

| Area | 0.14 → 0.17 upstream change | Resolution |
|---|---|---|
| Lumetri panel (`panels/lumetri.rs`) | curve editor rebuilt (full width, double-click, `hue_lut` / `vs_sat_lut`) | **upstream's file taken whole**. Our reset button, `curve_size` and `CURVE_GRAB` are dropped (upstream's editor covers the use); fork tests on them follow upstream's behaviour |
| Timeline layout | `Layout` gains `v_scroll`, `a_scroll`, `video_top`, `audio_bottom` | all four fields kept next to ours (`new_track_zone`); 4 test initialisers completed |
| Stabiliser cache (`render/vfx/temporal.rs`) | `corrected()` cache and `chunked_steps` | cache key extended with our L1 margin: `CorrectedKey = (usize, u64, bool, bool, Option<u64>)`, `corrected(path, smooth, no_motion, preserve_scale, l1_margin)`, test updated |
| Remix / beat grid (`audio-dsp`) | norm caching in `remix.rs` | upstream's cache kept, our `cos_sim` (`pub(crate)`) kept for `beatgrid.rs` |
| File drops | dropped project files open (#3843) | upstream first; our Resources kinds (LUT / sound / `.mogrt`) still go to `assets::drop_files` |
| `menus::invoke` | Clip Mismatch Warning | both guards coexist (our "extension running" lock and upstream's dialog) |
| Markers | `Marker` gains `url`, `author`, `custom_color` | Mark Beats (`engine/beats.rs`) fills them empty |
| Undo history | entries are now `(label, project, Option<String>)` | Place Dub (`engine/dub.rs`) keeps the third element when it folds steps into one |
| `hevc_probe` | upstream made the same fix as ours (own test binary) | upstream's |
| i18n | more languages | upstream's list (16 `.tsv`); our `fr` / `es` keys re-appended |

## Function-by-function comparison, fork on 0.14.0 → fork on 0.17.0 (proof: `git diff fc-port-014 HEAD --numstat -- <path>`)

Fork-only modules are **byte-identical** (+0 / −0): crates `extensions`, `ml`, `nn`, `mlsearch`, `mlrestore`, `mlfetch`, `modelstore`, `voice`, `reframe`,
`narration`; `extensions-bundled/`; `ui-egui/src/assets/`, `panels/resources.rs`, `panels/extensions.rs`; `filmcraft-cli/src/ext.rs`; `apps/filmcraft/src/open_files.rs`;
`render/src/vfx/filmlook.rs` and `l1path.rs`. None of them exists upstream at v0.17.0, so nothing was replaced by an upstream equivalent. Nothing dropped there.
Whole delta between the two ports: 562 files, +60 048 / −4 037 (almost all of it upstream's).
Shared files: resolved as in the table above; the app-shell hooks of [`port-014.md`](port-014.md) (drop partition, Finder opens, live layout, Resources keys) are unchanged and their tests pass.

## The 3 losses noted at 0.13 → 0.14, checked again on 0.17.0

| Loss | Finding on 0.17.0 |
|---|---|
| Drop *between* two tracks creating a track | **Code present.** `Layout::new_track_zone` (`panels/timeline/layout.rs:75`), used by the Resources drop (`assets/mod.rs:260`, `:546`); unit test `new_track_zones_above_below_and_between_tracks` passes (part of the `ui-egui` lib run). Not verified with a real mouse drag |
| `.prproj` v0 export | **Not lost, never existed in any fork branch.** Upstream has no `.prproj` code at v0.17.0 either (only roadmap / parity docs: `ROADMAP.md`, `docs/file-format-parity.md`). Still open work |
| 5-language enum | Replaced by upstream's list (16 translation files: bs cs de es fr id it ja ko pl pt-br ru sv tr uk zh-cn); our French / Spanish keys kept (`fr.tsv` 3907 lines, `es.tsv` 3902). Nothing to restore |

## Verification (3–4 Nov 2026, aarch64)

- `cargo clippy --workspace --all-targets --keep-going -j2 -- -D warnings`: first pass found only compile errors of the merge (3 in `engine`, 4 in `ui-egui` tests, listed above), then **EXIT 0** (`fc-port-017-pkg/clippy3.log`). `cargo fmt --check`: **EXIT 0** (after one `cargo fmt` on `beats.rs`).
- `cargo xtask layers`, `assets`, `wasm`: **EXIT 0 each** (`fc-port-017-pkg/xtask.log`).
- Tests per crate (`cargo test --no-fail-fast -j2 -p <crate>`, never `--workspace`), passed / failed:
  project 83/0, engine 918/0, render 257/0, color 44/0, speech 26/0, audio-dsp 146/0, extensions 38/0, filmcraft 85/0, cli 53/0, time 23/0, edit 87/0, export 147/0, media 118/0,
  text 55/0, captions 103/0, ml 4/0, voice 4/0, reframe 9/0, mlrestore 5/0; **ui-egui 1113/1** and **platform 72/1** (below).
- The first seven crates ran on the old target dir (`filmcraft-port-014-target`), the rest on a cold target on Studio Mad (`_build-tmp/fc017`, temporary, authorised by Madson on 4 Nov). The code did not change between the two runs (commit `a65ca340`).

### The two failing tests are upstream's, not the fork's (their files are identical to v0.17.0: `git diff v0.17.0 HEAD --stat -- <file>` is empty)

- `ui-egui` `perf_ui::program_monitor_plays_on_while_a_value_is_dragged` (needs ≥ 5 distinct frames in 90 passes of 16 ms; saw 4 under heavy machine load): **passes alone 3/3** and passed again in a quiet rerun (EXIT 0). Fragile under load.
- `platform` `hardware_encode::rate_conversion_and_cancel_then_export_again`: asserts the process-global `declined` counter is unchanged while another test of the same binary (`declines_what_the_hardware_path_does_not_take`) increments it. **Fails in parallel 2 runs out of 2** (left 6, right 5), **passes alone 3/3 and with the whole `it` binary at `--test-threads=1` (34/34)**. Same family of defect as `hevc_probe` at 0.14 (shared global state in one test binary). Not modified here; debt: serialise or isolate it, or report upstream.
- Neither test was edited and no threshold was changed.

## Known debt / not verified

- 3 `cognitive_complexity` `#[allow]` from 0.14 (Resources `show` and its row-list closure, the `.mogrt` import test) are still there.
- Not verified: real drag-and-drop from Finder, a real `.mogrt`, native menu rebuild on extension change, dubbing / voice clean-up / auto-reframe (models not in the bundle), MadVision sync.
- The bundle is ad hoc signed; Gatekeeper may block it on another Mac. Nothing is installed in `/Applications`.
- The build environment was unstable (disk, then swap saturated by other jobs); three `cargo test` runs of `ui-egui` were killed after hours at ~0 % CPU before the final cold-target run. No test result above comes from a killed run.

## Re-apply procedure for a newer tag

`git fetch --tags`; branch from the new tag; `git merge fc-port-017`; resolve preferring upstream for generic code and ours for fork-only modules; re-wire the hooks of
`port-014.md`; run clippy `--keep-going` once, fix everything, then tests per crate (`-p`, `-j2`) and the bundle script.
