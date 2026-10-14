# Premiere `.mogrt` titles → editable FilmCraft titles (titles-native)

A `.mogrt` is a ZIP: `definition.json` (name, size, duration, editable controls, fonts), previews
(`thumb.png`, often `thumb.mp4`) and the template itself:

- **`project.prgraphic`** — made in **Premiere**: a ZIP holding a Premiere project (gzipped XML).
  Its graphic clip lists text and shape layers whose properties are plain values or JSON in the
  XML. FilmCraft's graphic layers follow the same model (Essential Graphics), so these titles
  **are converted** into a FilmCraft graphics template (`.fcgt`).
- **`project.aegraphic`** — made in **After Effects**: an After Effects project (binary). FilmCraft
  cannot run it and never opens it beyond the ZIP directory: such a title is **preview only**.

In Madson's library (measured 14 Oct 2026, `mogrt_to_alpha.py index` over the 49 packs): **194
Premiere titles, 1 323 After Effects titles, of 1 517**. Premiere titles are in 5 packs:
`Titles/Premiere Pro Lower Thirds` (60), `Titles/4e platinumTitles` (51), `Titles/Movie Titles`
(50), `Titles/james abadi` (23), `cinecom` (10 of 12).

## How to use it

- Drop a `.mogrt` on the timeline (Finder or Window ▸ Resources ▸ Titles): a Premiere title is
  converted once and placed on the first free video track above the shot under the pointer; its
  text, fonts and colours are edited in Essential Graphics ▸ Edit (or `graphics.template.set`).
- Elsewhere (Project panel): converted into Graphics Templates (Essential Graphics ▸ Browse).
- Titles ▸ **Convert all shown (N)**: converts the listed Premiere titles in the background.
- Commands: `graphics.template.importMogrt {"path"}` (answers `id`, `path`, `layers`, `controls`,
  `missingFonts`, `warnings`, `lost`, `cached`), `graphics.template.install {"path": "x.mogrt"}`
  and `graphics.template.apply {"template": "x.mogrt"}` convert too. CLI: `filmcraft-cli
  --data-dir <dir> exec graphics.template.importMogrt path=…` (an explicit `--data-dir` holds the
  templates).
- Converted templates are `<data>/Graphics Templates/user-mogrt-<name>-<id>.fcgt`, category
  "Converted Titles", tagged with the source path and its size + date: a title is converted again
  only when the `.mogrt` changes. The `.mogrt` is only read.

## What is translated

| Premiere | FilmCraft |
|---|---|
| Text layer: text (several lines), font (PostScript name → installed family + style), size, alignment, tracking, leading, ligatures, faux bold / italic, caps, baseline shift, vertical text, paragraph box | text layer, same parameters |
| Per-character styles (size, font, fill, tracking, faux, baseline, caps) | style runs |
| Fill (solid), first stroke, shadow (colour, opacity, angle, distance, size, blur), background box (colour, opacity, size) | appearance |
| Shape layer: rectangle | rectangle shape |
| Shape layer: other path (Bézier vertices) | path shape (curves flattened to 16 segments) |
| Position, anchor point, scale, horizontal scale + uniform scale, rotation, opacity — static or keyframed | same parameters and keyframes (linear, or Bézier ease where Premiere eases; speeds not kept) |
| Responsive Design – Position (layer pinned to another) | position made absolute, and a pin to the parent (found by its size) |
| Clip opacity (fade in / out of the whole title) | folded into each layer's opacity |
| Clip Motion, static (whole title moved / scaled / rotated) | folded into each layer |
| Layer names | template controls: text, font and colour of each text layer, colour of each shape |

A font the Mac does not have is replaced by FilmCraft's default (Inter), and the conversion says
which (status bar, `missingFonts`, the template's description). Fonts are never copied into the
template (they are not ours to redistribute).

## What is left out (listed in `lost` and in the template's description)

- **Masks** of the graphic (`Mask`, often animated reveals: 81 of the 194 titles) and text used as
  a mask (cut-out, 4 titles: drawn as plain text).
- **Effects on the graphic**: Crop (48), Wave Warp (31), Color Balance, Lighting Effects, Noise,
  Basic 3D, Drop Shadow, Gaussian Blur, Motion Blur, Brightness & Contrast… (the title is drawn
  without them).
- **Animated clip Motion** (24): the layers' own animations are kept, the movement of the whole
  graphic is not.
- Gradient fills (plain colour), strokes after the first, images inside a graphic.

## Fidelity, measured

19 real titles from the 5 packs, converted with the CLI and rendered at 60 % of their duration,
side by side with the same moment of their own preview (`/Volumes/Studio Mad/_tests/
filmcraft-titles/compare/NN-side.png`, original left): see `infra-artcraft.md` § titles-native
for the title-by-title reading.
