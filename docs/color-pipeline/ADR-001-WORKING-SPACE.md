# ADR-001 — Linear ProPhoto for new RAW scene masters

Date: 2026-08-09  
Status: Accepted

## Decision

New RAW scene masters use linear ProPhoto RGB in an unclamped RGBA16F buffer. Existing raster/legacy Develop sessions remain linear sRGB and carry explicit compatibility metadata. ACEScg remains available as a reference transform and future HDR candidate.

## Reasons

- Both candidates provide ample headroom for camera colors and preserve signed/HDR values.
- The measured corpus produced 51,987 negative working channels in linear ProPhoto versus 98,037 in ACEScg. The plan requires choosing from measurements, so linear ProPhoto wins this phase despite ACEScg's scene/VFX ecosystem advantage.
- Both candidates cost two 3×3 matrix transforms and have effectively identical CPU/GPU instruction cost.
- Existing display, ICC and UI code remains sRGB-referred at the boundary. The working-to-sRGB matrix is composed into the existing CAT16/exposure matrix and therefore shared by CPU and WGSL.
- Legacy raster documents retain their previous look through `WorkingColorSpace::LinearSrgb`.

## Benchmark

Command: `cargo test --release --test working_space_bench -- --ignored --nocapture --test-threads=1`

The test processes 500,000 deterministic signed/HDR vectors. ACEScg: 4.335 ms, max roundtrip error 0.00010496, 98,037 negative channels. Linear ProPhoto: 4.310 ms, max error 0.00004029, 51,987 negative channels.

## Consequences

- RAW camera RGB is converted camera → linear sRGB → linear ProPhoto once during decode; no clamp occurs.
- The scene tone builder composes linear ProPhoto → linear sRGB with CAT16 and exposure once per settings snapshot.
- Perceptual Color Mixer and output gamut mapping remain separate later phases; this ADR does not claim that legacy sRGB-primary mixer math is the final color model.
