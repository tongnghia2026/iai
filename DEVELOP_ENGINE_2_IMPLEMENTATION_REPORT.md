# Develop Engine 2 — Implementation Report

Date: 2026-08-17  
Branch: `codex/develop-engine-2-implementation`  
Validated head before this report: `0c11bd9`

## Outcome

The remaining data-independent work in `CODEX_DEVELOP_HANDOFF.md` is implemented and validated:

- Phase 7.A: live Waveform, RGB Parade, and Vectorscope are connected to both Develop UI hosts, independently toggleable, cached, and stopped when hidden.
- Phase 7.B: Soft Proof, target profile selection, ICC loading, and Gamut Warning use the shared display/proof LUT and remain view-only.
- Bug C / Phase 6 spatial completion: Detail and Local adjustments now run interactively on a reduced-resolution viewport proxy using the same kernels, constants, stage order, and regional Light base as commit.
- Phase 8 validation: all renderer versions have serialize/reopen output guards; pre-versioned settings remain Scene1; a Develop2 commit survives `.iai` save, reopen, and 16-bit PNG export exactly.

No vector, text, scan, or PDF behavior was changed. No remote push was performed.

## Owner GUI acceptance

On 2026-08-19, the owner tested the release artifact and reported the GUI validation as **OK**. This accepts the delivered Phase 7.A scopes, Phase 7.B soft proof/gamut warning workflow, Bug C preview behavior, Temp/Tint consistency, and the Phase 8 validation/release package.

The two data-dependent items below remain deferred until the owner supplies their fixtures. Conditional twin/JPEG-match/legacy cleanup is not authorized by this acceptance and still requires a separate owner decision, adoption evidence, and rollback plan.

## Local commits

- `11e8181 feat(develop): show live color scopes`
- `8eef677 feat(develop): preview soft proof and gamut warnings`
- `01a73ca feat(develop): preview detail and local edits`
- `0c11bd9 test(develop): lock migration and export roundtrips`

## Bug C implementation details

- Detail/Local no longer force the full CPU path while dragging when the normal GPU preview path is available.
- Legacy/display and RAW/scene sessions reuse their existing commit kernels on the viewport proxy.
- Local mask geometry is evaluated in normalized source-image coordinates, so pan, zoom, ROI origin, and proxy downsample do not move masks.
- Shadows/Highlights regional luma/E is sampled by the spatial proxy too; stacking Light + Detail/Local does not switch to a different tone model.
- Legacy stage order remains tone → colour → effects → local → detail. RAW stage order remains scene tone/colour → effects → detail → local → output boundary.
- Detail proxy input uses a five-tap corners-plus-centre prefilter to suppress single-pixel colour aliasing that caused the earlier thin-edge bead artifact.
- Commit remains full-resolution. Interactive Detail uses a tier-aware budget equal to two thirds of the normal fast-preview budget.

### Measured latency

Release probe, Detail + Local tail, High-tier production budget:

- Rejected configuration: 240k proxy pixels, p95 **63.36 ms**.
- Shipped configuration: 160k proxy pixels, p95 **40.35 ms**.
- Gate: p95 < 50 ms.

The probe is retained as ignored test `fast_preview_detail_local_p95_probe` because timing is hardware-dependent.

## Validation evidence

Final gates after Phase 8 changes:

- `cargo fmt --all -- --check`: pass.
- `git diff --check`: pass.
- `cargo test --target-dir target/phase1-foundation --locked -j 1 --lib`: **1363 passed, 0 failed, 6 ignored**.
- `cargo test ... --test develop_color_golden`: **4 passed**.
- `cargo test ... --test develop_cpu_gpu_parity`: **3 passed**, including headless GPU.
- Legacy Detail+Local proxy at sample density 1 matches commit within quantization tolerance.
- RAW Detail+Local+Shadows proxy at sample density 1 matches commit within 16-bit quantization tolerance.
- All `Legacy1`, `Scene1`, and `Develop2` settings round-trip without renderer drift.
- A pre-versioned settings snapshot reopens as Scene1 bit-exact.
- Develop2 settled commit → `.iai` save → reopen → 16-bit PNG export is exact.

Known pre-existing warnings remain unchanged:

- unused `GlyphStyle` import in test compilation;
- two future-incompatible f32 literal fallback warnings in `src/ui/library.rs`.

## Release artifact

- Built with `cargo build --release --target-dir target/phase1-foundation --locked -j 1`.
- Copied successfully to `dist/iai.exe`.
- Size: **67,606,016 bytes**.
- SHA-256: `FEF12B253DFA18D401159973E08605AF8D49A04671F268624A3B999051BBB7B9`.

## Intentionally deferred by required gates

### Phase 7.C high-contrast edge grain

No owner-provided native-resolution crop exists in the repository. Per handoff, demosaic/CA tuning must not be performed by eye. Required input: one native crop centred on a remaining bad edge, preferably with the source RAW and crop centre usable via `IAI_RAW_LOOK_CROP="cx,cy"`.

### HP0917 NEF decode-black

No `HP0917` fixture exists locally, so the failure cannot be reproduced or fixed responsibly. Required input: the affected NEF (or a minimized reproducible fixture and decode log).

### Twin/JPEG-match/legacy removal

Not removed. The master plan permits removal only after complete golden coverage, real adoption evidence, and a rollback path; the handoff also explicitly requires preserving the legacy renderer in this work. The new migration/reopen gates are prerequisites, not proof of adoption. Removing these paths now would violate the compatibility requirement.

## Owner GUI acceptance checklist — completed 2026-08-19

The owner reported this rebuilt `dist/iai.exe` GUI checklist as **OK** using representative RAW files:

1. Toggle Waveform, RGB Parade, and Vectorscope independently; confirm they update with sliders and stop when all are off.
2. Toggle Soft Proof and Gamut Warning; switch sRGB/Adobe RGB and load an ICC; confirm only the view changes.
3. Stack Shadows/Highlights, Detail/denoise, a Local gradient/radial mask, and Colour; drag then release; confirm there is no obvious settle jump.
4. Recheck Temperature/Tint on D810.
5. Open an old project/preset and confirm the Scene1 look is unchanged.

## Addendum — Defringe control (2026-08-19)

Under the owner's chosen "improve RAW image quality" direction, an edge Defringe
control was added (a Phase 7-class high-value feature from the master plan's
"features worth adding" table). It is additive and does not alter any previously
accepted behaviour.

- Commit: `eb564f6 feat(develop): add edge Defringe control`.
- UI: a `Defringe` slider (0–100) in the Detail panel, with a purple→neutral→green
  track. Inert at 0.
- Model: a chroma-only pass in the CPU Detail bake (`src/core/develop/detail.rs`,
  `apply_defringe`). At each pixel the chroma is pulled toward a blurred regional
  reference only where three gates agree — a steep luminance edge, a chroma
  direction near the green↔magenta axis, and a local chroma spike above the
  reference. Luminance is untouched. It runs before Colour NR and Sharpen and is
  decoupled from the Sharpening amount.
- Parity: the Detail stage has no live GPU mirror (it previews through the
  debounced commit-quality CPU bake), so there is no WGSL twin to keep in sync.
- Compatibility: `defringe` is a `serde(default)` field (0.0), so old projects and
  presets load unaffected; it is wired into `is_neutral`, `same_image_effect`, the
  `differs_only_*` fast paths, and `has_detail`.
- Test: `core::develop::detail::defringe_tests::defringe_clears_magenta_rim_but_spares_real_colour`
  asserts a synthetic magenta edge rim loses >50% of its chroma while a uniform
  saturated patch and a saturated red (non-fringe) edge are preserved.
- Gates: `cargo fmt --all -- --check` clean; `git diff --check` clean;
  `--lib` **1364 passed, 0 failed, 6 ignored**; `develop_color_golden` **4 passed**;
  `develop_cpu_gpu_parity` **3 passed** (incl. headless GPU).

### Release artifact (Defringe build)

Rebuilt `dist/iai.exe` from `eb564f6`. This supersedes the 2026-08-19 accepted
build and is **pending owner GUI acceptance**; the previous accepted binary can be
rebuilt from `c9adb0c` if a rollback is needed.

- Size: **67,621,376** bytes.
- SHA-256: `88491B05A8FB9A347A31BA7E9FDB29E5C59CC7F42A2DE94306994175CEFA4C11`.

Owner GUI check for this build: on a photo with visible purple/green fringing at a
high-contrast edge (e.g. backlit branches against bright sky), raise Defringe and
confirm the coloured rim fades while real colours and sharpness stay intact; at 0
the image is unchanged.

## Addendum — Output sizing & sharpening on export; Defringe parked (2026-08-19)

### Defringe: failed GUI acceptance, slider hidden

The owner GUI-tested the Defringe build and reported it **not OK**. Without a
native crop that reproduces the fringe, it cannot be re-tuned responsibly (the
same measure-first blocker as the deferred high-contrast-edge grain item). The
Detail-panel **Defringe slider is now hidden** (`refine(develop): hide unproven
Defringe slider`, `43035f4`); the `defringe` field, the `apply_defringe` pass and
its regression test are retained so it can be re-exposed once validated on a real
sample.

### Output sizing & sharpening on export

Next item in the owner's "improve RAW quality" direction. Adds an optional
**Resize (longest side)** + **Output sharpening** step to the Export dialog for
raster image formats (PNG/JPEG/WebP/TIFF/BMP).

- Commit: `757a040 feat(export): output sizing and sharpening on export`.
- Rationale: downscaling for a delivery size softens an image; output sharpening
  restores acutance by sharpening AFTER the resize, at the final pixel grid.
- Engine: `src/core/output_sharpen.rs::apply_output_sharpen` — a luminance unsharp
  mask (Gaussian blur → tanh-limited high-pass → equal RGB delta), so it re-crisps
  edges without hue shift, halos, or amplifying noise in flat areas.
- Export path: `ExportOptions` gains `resize_long_edge: Option<u32>` and
  `output_sharpen: u8`; `FormatRegistry::export` builds a derived 8-bit canvas
  (flatten → Lanczos downscale → sharpen) only for raster targets when either is
  set. Resize is **downscale-only**.
- Compatibility: defaults are `None`/`0`, so every existing export — and all
  iai/PDF/SVG output — is **byte-identical**. A reprocessed (resized/sharpened)
  export is written as 8-bit (output sharpening is a display-referred final step).
- UI: wired through the existing export-state pattern (shell state → action →
  viewmodel → the export sites); the dialog shows the controls for raster formats
  only.
- Tests: `core::output_sharpen::tests` (re-crisps a soft edge, spares a flat
  field, amount=0 no-op) and `formats::output_prep_tests` (fits the longest side,
  downscales only, skips non-raster targets and default no-op exports).
- Gates: `cargo fmt --all -- --check` clean; `git diff --check` clean;
  `--lib` **1369 passed, 0 failed, 6 ignored**; `develop_color_golden` **4 passed**;
  `develop_cpu_gpu_parity` **3 passed** (incl. headless GPU).

### Release artifact (output-sharpening build)

Rebuilt `dist/iai.exe` from `43035f4` (output sharpening + Defringe slider hidden).
**Pending owner GUI acceptance.** The previous accepted engine build is rebuildable
from `c9adb0c`.

- Size: **67,634,688** bytes.
- SHA-256: `B5193DB1D42D0D5CDAFAF0079E4E615FB519B1B22C0997B614FF6A33D7E502FB`.

Owner GUI check: File ▸ Export, pick JPEG/PNG, tick **Resize (longest side)** and
set e.g. 2048 px, choose **Output sharpening: Standard**, export, and confirm the
downscaled file looks crisp (not soft) without halos; with both controls off the
export is unchanged.

## Addendum — Tone-adaptive (professional) noise reduction (2026-08-19)

Third item in the owner's "improve RAW quality" direction: upgrade the existing
Detail-panel **Noise Reduction** and **Color Noise Reduction** from a fixed-
strength wavelet denoise to a **tone-adaptive** one.

- Commit: `7bdd843 feat(develop): tone-adaptive (professional) noise reduction`.
- What changed: the luminance garrote threshold and the chroma-band attenuation
  are now scaled by a per-pixel **shadow weight** (`nr_shadow_weight`) derived
  from local brightness — rising toward `1 + gain` in the shadows and `1.0` in
  the highlights. Display-domain shadow grain/colour blotches (the most visible
  noise) are cleaned markedly harder; highlight detail and edges are preserved.
- Why brightness-keyed (not a measured global noise sigma): a measured sigma
  depends on resolution, which would make the reduced-resolution interactive
  Detail preview diverge from the full-resolution commit (a settle jump). Keying
  off brightness is resolution-invariant, so **preview still matches commit**.
- Compatibility: highlights are bit-for-bit the pre-upgrade garrote, and both
  sliders default to 0, so the default render and every previously accepted
  result are **unchanged**. CPU-only (the Detail stage has no live GPU mirror),
  so no WGSL parity work.
- Test: `core::develop::detail::nr_tests::noise_reduction_cleans_shadows_harder_and_keeps_edges`
  — a dark and a bright noisy half both get cleaner, the shadow half harder
  (reduction gap > 0.08), and the luminance edge between them is preserved.
- Gates: `cargo fmt --all -- --check` clean; `git diff --check` clean;
  `--lib` **1370 passed, 0 failed, 6 ignored**; `develop_color_golden` **4 passed**;
  `develop_cpu_gpu_parity` **3 passed** (incl. headless GPU).

Scope note: this upgrade stays in the display-domain Detail stage (safe, default-
off, preview==commit). A deeper scene-linear *profiled* denoise on the RAW master
(shot-noise model / Anscombe VST) is the natural next step but was deliberately
not attempted here, as it would touch the owner-accepted RAW default look and the
RAW pipeline — hold for an explicit owner decision.

### Release artifact (noise-reduction build)

Rebuilt `dist/iai.exe` from `7bdd843` (tone-adaptive NR + output sharpening +
Defringe slider hidden). **Pending owner GUI acceptance.** Previous accepted engine
build rebuildable from `c9adb0c`.

- Size: **67,634,688** bytes.
- SHA-256: `D7A5939CE38CBCE9D5DD366E880B37C0A417A60875627A215D0D0068C46A0236`.

Owner GUI check: open a high-ISO / underexposed RAW, in Develop raise **Detail ▸
Noise Reduction** (and Color Noise Reduction), and confirm shadow grain/colour
blotches clean up more than before while highlight detail and edges stay sharp;
at 0 the image is unchanged, and dragging shows no settle jump between preview and
the committed result.
