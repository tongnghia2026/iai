# Phase 4 — Hue-preserving gamut mapping

## Production path

- The output boundary is separate from scene tone mapping: tone produces display-linear RGB; `gamut_map` resolves only output-gamut excursions.
- In-gamut pixels are returned bit-identically.
- Out-of-gamut pixels are converted to OKLCh and retain perceptual lightness and hue while an 18-step binary search finds the maximum reproducible chroma.
- CPU and WGSL use the same conversion, tolerance and iteration count.
- The boundary API supports sRGB and Display P3. Display P3 results remain represented as linear sRGB until the later output/profile transform; they must not be prematurely clamped to the sRGB cube.
- Existing film-like highlight shoulder remains a distinct tone operation. Its redesign belongs to Phase 5.

## Algorithm comparison

Synthetic debug benchmark, 3,600 out-of-gamut vectors:

| Candidate | Lookup/map time | Quality observation | Decision |
|---|---:|---|---|
| OKLCh binary, 18 steps | 6.58 ms | Reference; continuous hue/L and precise cusp | Production |
| 1° hue × 0.01 L nearest cusp LUT | 0.54 ms | max linear-RGB error 0.06895; contour risk | Reject coarse LUT; revisit with interpolation/profile cache |
| Constant-luma RGB analytic clip | 0.41 ms | max hue disagreement 146.58° on extreme vectors | Reject |

Release compilation exceeded the local 180-second command budget, so these timings are debug-build engineering comparisons, not shipping performance claims. GPU settled-preview parity remains the practical acceptance measurement.

## Automated acceptance

- Exact identity for in-gamut samples.
- Zero remaining out-of-gamut samples on the synthetic hue/chroma grid.
- Saturation ramp monotonic through the cusp.
- Hue continuity at red wrap and blue/cyan cusps.
- P3 retains at least as much chroma as sRGB for a wide-gamut vector.
- Actual headless WGPU vs CPU commit: max 2/255, P99 1/255.

Manual review is still required for skin, red wrap, blue/cyan, neon highlights and smooth gradients before Phase 4 is closed.
