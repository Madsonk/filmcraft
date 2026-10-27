# Port of the Madson fork (FilmCraft-Ext 0.5.0-ext-all-v6.6) onto upstream v0.13.0

Status: **merged, not yet compiled** (27 Oct 2026). Branch `fc-port-013` = tag `v0.13.0` + a merge of
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

## Build and verification

*To be filled in once the machine lets us build (see the shared build lock).*

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
