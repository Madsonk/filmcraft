# Port of the Madson fork (FilmCraft-Ext 0.5.0-ext-all-v6.6) onto upstream v0.13.0

Status: **merged, compiles, bundle built; see the verification section for what was and was not run** (28 Oct 2026). Branch `fc-port-013` = tag `v0.13.0` + a merge of
`ui-fixes` (the last branch of the fork, shipped as `FilmCraft-Ext-050-v6.6.app`). The merge base is
`v0.5.0`; the fork adds 58 commits and ~43 000 lines (225 files) on top of it, upstream added 1 441
commits (910 files) between `v0.5.0` and `v0.13.0`. The sections "Build and verification" below are
filled in as the checks run; anything not listed as verified is **not verified**.

Principle (Madson): independent of Adobe; take the idea and do better with our own code; when
upstream 0.13.0 already has a function, prefer upstream and keep from ours only what it adds.

## What the fork adds on top of v0.5.0 (by function, with the commits)

| Function | Commits (fork) | Where |
|---|---|---|
| Extension host + 7 first extensions, Extensions menu (macOS native too), manifests, bundled extensions seeding, CLI `extensions` | `d21d7b3b` ext, `2eab5297`, `bc626f1e`, `6716d111`, `049b6a2e` | `crates/extensions`, `extensions-bundled/`, `apps/filmcraft-cli/src/ext.rs`, `ui-egui/src/panels/extensions.rs` |
| Chantiers A/B/C (silence remover, beat edit, auto-edit, tics de langage, sous-titres, nettoyage de voix, reframe, upscale, RIFE, rush search…) | `1123ed9a`, `ee1a1e35`, `5ef24b58` | `crates/{ml,nn,mlsearch,mlrestore,mlfetch,modelstore,voice,reframe}`, `engine/src/{ml_*,voice_clean,auto_reframe,beats,…}` |
| Voice dubbing (doublage), Marian offline subtitle translation, animated word captions, Whisper turbo + initial prompt | `04bc4140`, `ext-all-v2` | `engine/src/{dub,captions_translate,word_captions,transcript}.rs`, `speech/src/mt`, `speech/src/whisper` |
| Match Shot Colour (33³ project LUT) | `9f19cc63` | `engine/src/shot_match.rs`, `color.rs` |
| Film Look effect folder (Film Look, Grain, Halation, Bloom, Diffusion, Stock, Gate Weave, Dust, Lens) | `170bc115`, `be69f4f8`, `749f4de5` | `render/src/vfx`, `project/src/effect` |
| Window ▸ Resources (LUTs, sounds, music, titles by drag and drop), audition, waveforms (FCPK), preview cache | `261906d1`, `b7a5ebc6`, `883513c0`, `a95edb74`… | `engine/src/{assets,sound_previews}.rs`, `ui-egui/src/assets`, `panels/resources.rs` |
| Premiere `.mogrt` titles → native `.fcgt` | `34684de6`, `cd51910d` | `engine/src/mogrt_import.rs` |
| LUT parity (64³ resample), MadVision audio sync extension | `5e135a61` | `color/src/lut.rs`, `extensions-bundled/sync-madvision` |
| Finder opens `.fcproj` (Apple Event), window title | `08376738` | `apps/filmcraft/src/open_files.rs` |
| French interface | `38b36140` | `ui-egui/src/i18n/fr.tsv` |
| UI fixes: drops make new tracks, tab docking, Lumetri curves full width, double-click curve points | `f4b568c0`, `559d9d9a` | `ui-egui` |

## Decision table

| Function | Decision | Reason |
|---|---|---|
| French interface | **Upstream** + 179 of our keys appended | Upstream ships `fr.tsv` (3 587 lines) and 11 languages; our 5-language `Language` enum is dropped. Only keys upstream lacks (extensions, Film Look, Resources…) are appended under a comment. |
| Docking by dragging a panel tab | **Upstream** | 0.13.0 has `DockAction::BeginDrag` and floating panels. |
| Drop on empty timeline space = new track | **Upstream** (#483, `"track":"new"`) | Equivalent for above-V-top / below-A-last. **Lost:** our drop *between* two tracks (`timeline.place newTrack:{kind,index}`); `assets.place newTrack` (Resources panel) is kept. |
| Whisper | **Merge** | Upstream hardened allocations (`MAX_ALIGNMENT_HEADS`, `MAX_VOCAB_ID`); ours adds the initial prompt, turbo and BPE `encode`. Both kept in `speech/src/whisper`. |
| TTS / narration | **Upstream** | Kokoro + `tts-text` are upstream; our dubbing builds on the extension host, not on `tts`. |
| Captions import/export, text panel | **Upstream** | Large upstream work (TTML, STL, burn-in, speaker names). Our animated word captions stay as an engine command. |
| Remix / beat analysis (`audio-dsp/remix.rs`) | **Merge** | Upstream added resource bounds (`MAX_ANALYSIS_*`); we keep them and re-apply our `pub(crate)` split (`trimmed_beats`, `track_beats_with`) used by `beats.rs`. |
| Audio sync (multicam) | **Upstream** `audio-dsp/sync.rs` | The MadVision sync is an extension (HTTP service on :8765), independent. |
| LUT import | **Merge** | Upstream rejects huge/non-finite LUTs; we add `premiere_parity()`. |
| `.mogrt` import | **Ours** | Upstream still refuses Adobe `.mogrt`. |
| Film Look, Match Shot Colour, Resources panel, waveforms, preview cache | **Ours** | Nothing equivalent upstream. |
| Extension host, Extensions menu, ML tools (RIFE, Beat This, Demucs, SigLIP, SPAN, SCUNet), voice clean-up, auto-reframe, dubbing, offline translation | **Ours** | Upstream rates plugins ~5 % (ROADMAP, gaps G11). |
| Native macOS menu | **Merge** | Upstream's menu updater (enabled state, language) + our Extensions submenu rebuilt when the extensions change. |
| Finder open (`open_files.rs`) | **Ours** (Info.plist is upstream's, it already claims `.fcproj`) | Upstream has no `application:openFiles:`. |
| `.prproj` export v0 | **Not found** | No `.prproj` code exists in `ui-fixes` or the other fork branches (only mogrt/docs mentions); it was done outside these repositories. Not ported; to be located before claiming anything. Upstream has no `.prproj` import yet. |

## Conflicts resolved by hand (35 files) — what to re-check first when compiling

`apps/filmcraft/src/main.rs` (our hooks re-injected in upstream's `app_creator`), `native_menu.rs`,
`ui-egui/src/{lib,menus,dock}.rs` (file drop router `assets::drop_files` kept, upstream
`pending_timeline_drop` path no longer reached from drops — check), `engine/src/commands.rs`
(`timeline.place` is upstream's), `speech/src/whisper/{mod,tokenizer}.rs`, `audio-dsp/src/remix.rs`,
`color/src/lut.rs`, tests (`tests/it/*.rs` is upstream's single-binary layout; our
`extensions_ui.rs` etc. must be added to `tests/it/main.rs`). `color_ui.rs` restarted from upstream with
our two new tests appended (edits to existing tests were not carried over).

## Not ported / known losses (so far)

- Drop between two tracks creating a track (see above).
- Our edits to pre-existing tests in `color_ui.rs`.
- The 5-language `Language` enum (replaced by upstream's 12).

## Build and verification (28 Oct 2026, aarch64, outputs from the commands)

- `cargo check --workspace --all-targets`: **EXIT 0** (after fixing: lost `#[cfg(test)]` on merged test modules,
  `MenuItem.separator_before`, `SavedWorkspace.floating`, `DockAction::Dock`, `Layout`/`Row` test structs,
  `motion_matrix` arity, `FxCtx.frame_rate` in examples/tests, test fixture path `tests/it/fake_voicebox.py`).
- `cargo fmt --check`: OK. `cargo clippy -D warnings --all-targets` on `filmcraft-{extensions,speech,audio-dsp,color,render,engine,ui-egui}`, `filmcraft`,
  `filmcraft-cli`: first pass failed on 2 dead-code lints in `ui-egui/src/dock.rs` (our superseded tab-drag), fixed; second pass `CLIPPY2-EXIT 0` for ui-egui/app/cli.
  (The first pass for the other crates ran before the fix; they compiled clean in it.)
- Tests: `filmcraft-{extensions,speech,audio-dsp,color}` all green; `filmcraft-engine --lib` filtered on our modules
  (`assets_tests dub_tests auto_reframe_tests word_captions_tests beats_tests voice_clean_tests shot_match_tests mogrt_import_tests transcript_tests sequence_extras`): **53 passed, 0 failed** after one fix:
  `a_sound_dropped_on_a_shot…` expected the clip to start exactly at the dropped time, 0.13.0 snaps clips to the frame grid
  (start was 0.02 s earlier while `assets.place` still reports the unsnapped `time`): test relaxed to one frame. That small
  inconsistency in the reported time is **not fixed**.
- Second pass (28 Oct, after "finis"): `cargo xtask layers`, `assets`, `wasm`: **EXIT 0 / 0 / 0**.
  Full `cargo test --workspace --no-fail-fast`: everything passed except 9 tests, triaged:
  - `filmcraft-cli long_help_matches_help_command`: `--help` did not list the extension commands (fixed, CLI suite now 28/28).
  - `ui-egui i18n french_translates_everything_spanish_does`: a test of the fork, contradicting upstream's policy (a missing translation never fails, #3012); it now only reports (215/215 lib tests).
  - `assets_ui` (6 failures): `assets::after_panels` was no longer called by the 0.13.0 dock (lost in the merge): fixed in `dock_ui.rs`; now **10/11**.
  - **Still failing: `assets_ui::sounds_show_waveforms_and_listening_on_hover_plays_in_the_app`** (assertion at `assets_ui.rs:604`: the audition is expected to stop when `playback.playing` is set). Not resolved: either 0.13.0's transport resets `playing` in the test's headless frames, or the audition really keeps playing while the timeline plays. Unknown.
  - `codecs h264_draft::draft_playback_never_reaches_exports` and `platform hevc_probe::register_answers_the_hevc_probe_in_the_background`: crates we did not touch (no diff against v0.13.0); both **pass when run alone**, they fail only inside the workspace run (shared process state). Treated as upstream flakiness, not proven.
  - Targeted re-run after the fixes: `extensions_ui`, `film_look_ui`, `robustness_ui` green, `assets_tests` (engine) 9/9. The full workspace run was **not** repeated after the fixes.
- `assets.place` now reports the clip's real start (frame grid) in `time`, instead of the asked time.
- **Still not verified:** screenshots (display asleep), real drag and drop with a mouse, a real `.mogrt` file (only the engine's fixture tests ran), the native macOS menu rebuild when extensions change, dubbing, voice clean-up, auto-reframe (need models), MadVision sync (needs :8765).
- Release build: `cargo build --release -p filmcraft -p filmcraft-cli --features filmcraft/models` with `CRAFT_FONTS_DIR=/Volumes/Data/work/craft-fonts`.
  **Not enabled:** feature `ml` (RIFE, Beat This!, Demucs, SigLIP rush search, SPAN upscale) — the commands exist but report the model tools as absent; only the model download (`models`) is in.
- Bundle: `/Volumes/Data/work/filmcraft-port-bundle/FilmCraft.app` (CLI next to it in `cli/filmcraft-cli`): bundle id `ai.storyteller.filmcraft`, `CFBundleVersion` 0.13.0
  (same as the launcher's), Info.plist from upstream's template, icon, font licences copied from the launcher's bundle, `extensions-bundled/` in `Resources`,
  ad-hoc signed (with upstream's entitlements, **without** the hardened runtime and **not** notarized): `codesign --verify --strict` → valid on disk.
  No version suffix was added: the launcher reads `installed.json`, not the bundle, so a suffix proves nothing and a lower semver would only invite an "update".
- Run test on a throw-away project (two ffmpeg clips, `/Volumes/Data/work/filmcraft-port-test/`):
  import → new sequence → two `timeline.place` → `export --preset "Match Source – Adaptive High Bitrate"`: `out.mp4` H.264 1920×1080 + AAC, 5.013 s, in 1.9 s.
  `sequence.removeSilence` (detect and apply), `extensions run silence-remover`, `effects.apply film_look` + `film_grain` (export differs from the plain one: PSNR 27.6 dB) work.
  The desktop app starts, opens a `.fcproj` through the Finder Apple Event (`open_files`), and its control server answers: `ui.menu.list` shows **94** Extensions entries
  (after restoring the call lost in the merge, `menus.rs`, commit "restore the Extensions menu").
- **Not verified:** screenshots (`ui.screenshot` → "window hidden or display asleep"), the Resources panel and drag and drop, dubbing, voice clean-up, auto-reframe (need models), MadVision sync (needs the :8765 service), native macOS menu rebuild when extensions change, `.mogrt` import on a real file.
- Side effect: at start the app seeds the bundled extensions into `~/Library/Application Support/FilmCraft/extensions` (the real user folder, even with `--data-dir`), as our previous builds did.

## Re-applying our changes when the launcher updates FilmCraft

The ArtCraft launcher (v0.8.1) keeps `~/Library/Application Support/ArtCraft Launcher/installed.json`
(`filmcraft.version`, `path`) and offers an update when the latest GitHub release of
`storytold/filmcraft` is semver-newer than `version`. It then replaces the app at `path`
(`/Volumes/Madson/ArtCraft/FilmCraft.app`): **a launcher update overwrites any locally built bundle**.
The bundle identifier is `ai.storyteller.filmcraft` and the shipped one is Developer-ID signed and notarized;
ours is ad-hoc signed.

Procedure for upstream `vX.Y.Z`:

```bash
cd /Volumes/Data/work/filmcraft-port && git fetch origin --tags
git switch -c fc-port-X.Y.Z fc-port-013        # keep the old branch
git merge vX.Y.Z                                # or: git rebase --onto vX.Y.Z v0.13.0 fc-port-013
# resolve conflicts (same hot spots as above), then, with the build lock:
cargo fmt --check && cargo clippy -p <touched crates> -- -D warnings && cargo test -p <touched crates>
cargo xtask layers && cargo xtask assets
# build the bundle, sign ad hoc, drop it in /Volumes/Data/work/filmcraft-port-bundle/
```

To avoid being overwritten: do not click "Update" for FilmCraft in the launcher, or keep our bundle at a
different path (e.g. `/Volumes/Madson/ArtCraft/filmcraft/…app` as for the v6.x builds) and leave the launcher's copy alone.
We do not edit the launcher's configuration.
