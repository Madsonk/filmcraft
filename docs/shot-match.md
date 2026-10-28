# Shot colour matching (Clip ▸ Match Shot Colour…)

Two cameras on the same scene rarely agree: a Canon in a flat picture profile and a Panasonic in
V-Log, for example. **Match Shot Colour** makes one clip (the *target*) look like another (the
*reference*) and keeps the correction as an ordinary 33³ `.cube` in the project's LUT library,
played by the target's Lumetri **Creative ▸ Look** slot.

## Use

* Menu **Clip ▸ Match Shot Colour…** (French: *Clip ▸ Calage couleur entre plans…*). Pick the
  reference clip and the clip to match from the lists, or select each in the timeline and press
  **Use selected clip** (the window is not modal). With two clips selected when the dialog opens,
  the first is the reference and the second the target.
* **Method**: *Mean and spread (Reinhard)*, the default; *Linear transport 3×3
  (Monge–Kantorovich)*, which also aligns how the colour channels vary together; *Histogram per
  channel*, which follows the whole tonal distribution (strongest, can be harsh on small samples).
* **Strength** 0–100 %: the LUT is mixed with the identity (0 % = no change).
* **Apply to the clip**: on, the LUT becomes the target's Creative Look at intensity 100 (Lumetri
  is added if the clip has none). Off, the LUT only joins the library (`lib:lutNN`), to be applied
  by hand or to other clips of the same camera.
* The Program monitor is the preview; **Edit ▸ Undo** removes the look *and* the library entry
  (one step). The media files are never written.
* The status bar reports the distance between the mean colours of the two shots before and after
  (Oklab × 100, ≈ ΔE: below ~2 is hard to see).

## Command

```text
color.matchShots {"referenceClip":id,"targetClip":id,"method":"reinhard"|"mkl"|"histogram"="reinhard",
                  "strength":n=100,"apply":bool=true,"frames":n=5,"name":str?,"path":str?}
```

`frames` (1–24) frames are sampled per clip, at the middles of equal slices of the clip; `path`
also writes the `.cube` to disk. The result: `lut` (`lib:<id>`), `name`, `samples` (usable
pixels per side) and `meanDistance` `{before, predictedAfter, measuredAfter}` (`measuredAfter`
re-renders the target with its new look on the same frames; `null` without `apply`). CLI:
`filmcraft-cli --project p.fcproj --save exec color.matchShots referenceClip=12 targetClip=40`.
MCP: `command_run` with the same id and params.

## Log and flat footage

The statistics are taken where the Creative Look works: **after** each clip's Input LUT and Basic
Correction, on the display-referred Rec.709 signal. The reference is measured as it plays; the
target with its look, wheels, curves and HSL Secondary switched off. So convert log or flat
footage to Rec.709 **first** (Interpret Footage, or a conversion LUT in Lumetri's Input LUT slot),
then match: the match LUT stays valid as long as the conversion does not change. Matching raw log
against Rec.709 works numerically but asks one LUT to do both jobs. Sequences in an HDR working
space are refused (Rec.709 only).

## Method details and sources (clean-room)

Implemented from the papers only, in `crates/color/src/shot_match.rs`:

* E. Reinhard, M. Ashikhmin, B. Gooch, P. Shirley, *Color Transfer between Images*, IEEE Computer
  Graphics and Applications 21(5), 2001 — per-channel mean / standard deviation, here in Oklab
  (B. Ottosson, *A perceptual color space for image processing*, 2020); gain bounded to ¼…4.
* F. Pitié, A. Kokaram, *The linear Monge-Kantorovitch linear colour mapping for example-based
  colour transfer*, IET European Conference on Visual Media Production (CVMP), 2007 — closed-form
  `T = Σt^-½ (Σt^½ Σr Σt^½)^½ Σt^-½` in Oklab, covariances regularised by 1e-5.
* Histogram specification per channel (cumulative distributions, e.g. Gonzalez & Woods, *Digital
  Image Processing*), 257 quantiles per Oklab channel.

Pixels with the brightest channel ≤ 0.02 (crushed blacks) or any channel ≥ 0.985 (clipped
whites) and transparent pixels are ignored. No GPL/LGPL/AGPL code was read (in particular not the
`color-matcher` Python package).

## Versus Lumetri ▸ Apply Match

`lumetri.applyMatch` (Color Wheels & Match) matches **one frame** to **one reference frame** by
solving Lumetri's wheels and saturation (an editable grade, with skin-tone protection).
`color.matchShots` matches **whole shots** (several frames each), produces a **LUT** reusable on
every clip of the same camera, and leaves the wheels free for the creative grade. Use Apply Match
for a quick touch on one cut, Match Shot Colour to align two cameras for a whole interview.
