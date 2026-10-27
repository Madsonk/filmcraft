# Film Look effects (Effects ▸ Film Look)

A bin of nine video effects that give digital footage the character of photochemical film. They
are our own effects, not part of Premiere's catalogue, so they sit in their own top-level folder
of the Effects panel (**Film Look**, French *Rendu pellicule*), between *Video Transitions* and
*Legacy*. Ids are stable (projects reference them by id).

| Effect | Id | What it does |
|---|---|---|
| Film Look | `film_look` | All of the below in one effect: 7 presets (35mm Clean, 35mm Gritty, 16mm Documentary, Super 8 Memory, Large Format 65, Vintage 70s Print, Night Neon), one **Amount** slider and one slider per section (Grain, Halation, Bloom, Stock Colour, Gate Weave & Flicker, Dust & Scratches). Amount 0 leaves the picture untouched. |
| Film Grain | `film_grain` | Grain that depends on exposure (shadows / midtones / highlights responses), clumped (two octaves of smooth noise plus a per-pixel "roughness" term), partly colour-correlated, re-rolled every film frame (Frame Rate, Random Seed). |
| Halation | `halation` | Red-orange glow around highlights (light scattered back through the film base). |
| Bloom | `bloom` | Energy-spreading glow with a soft threshold, plus an optional anamorphic streak. |
| Diffusion Filter | `diffusion` | Pro-Mist-style veil: softens and lifts around highlights, optional warmth and black lift. |
| Film Stock | `film_stock` | Tone curve, dye cross-talk, split toning and highlight desaturation as 9 named recipes, with trims. |
| Gate Weave & Flicker | `gate_weave` | Per-frame jitter, tiny rotation, breathing and exposure flicker. |
| Dust & Scratches | `film_damage` | Procedural specks, hairs and vertical scratches that come and go. Not Premiere's *Dust & Scratches* (a median denoiser): different id, different bin. |
| Lens Character | `lens_character` | Lateral chromatic aberration and corner softness. |

Spatial parameters (grain size, radii, weave…) are in **full-resolution clip pixels**: a look keeps
its size when the Program monitor plays at 1/2 or 1/4, and when the clip is scaled in the frame.
Every effect is deterministic: the same frame renders the same grain, dust and weave every time
(seeded by the frame index and **Random Seed**), so a preview, a re-render and an export agree.

## Preview and export

The Film Look effects have no GPU (WGSL) implementation. When a clip carries one, the frame plan
does not hand the clip to the GPU effect stage (`FxOp::eval` returns `None`): the clip is rendered
by the CPU reference renderer — the same code the export runs — and the GPU only composites the
finished image. There is no separate preview approximation to drift from the export.

At a reduced playback resolution one working pixel stands for n×n clip pixels. Glows (Halation,
Bloom, Diffusion) are blurs sized in clip pixels and filter down naturally. Grain does not: drawn
at full strength on bigger pixels it would look rougher in the monitor than in the export seen at
the same size. Film Grain therefore keeps, per noise octave, the share of its deviation that
survives an n×n box average (and 1/n of the per-pixel roughness term); at full resolution
(export) nothing changes.

Measured by `filmlook_preview_tests.rs` (`cargo test -p filmcraft-render --release filmlook_preview -- --nocapture`):

* the GPU plan of a clip carrying each of the nine effects contains no GPU effect stage, and its
  result equals the reference render;
* grain standard deviation, preview ÷ export box-filtered to the preview size: 0.90–1.20 at 1/2 and
  1/4 (it was 1.46 at 1/2 before the correction);
* Halation, Bloom, Diffusion and Film Look (grain, weave and damage off): mean |preview − export|
  is 5 %, 16 %, 8 % and 6 % of the effect's own mean contribution at 1/2.

Cost: these effects are CPU work on every frame (the render bar counts them as medium, 20 ms on a
1080p frame); render previews (Sequence ▸ Render In to Out) for real-time playback of long stacks.

## Origin

The look recipes were written for FilmCraft from the published descriptions of film behaviour
(grain response curves, halation, gate weave). No code, preset or LUT from another product was
copied.
