# Phase 2 — Perceptual color core

Date: 2026-08-09

## Decision

OKLab/OKLCh is the production perceptual core. It accepts linear RGB in any
`WorkingColorSpace`, converts through linear sRGB/D65 at the module boundary,
and never clamps signed or HDR values. Negative LMS components use a signed
cube root on both CPU and WGSL.

JzAzBz/JzCzHz remains an opt-in forward prototype under Cargo feature
`jzazbz-prototype`. It is not connected to editing or UI until Phase 3/5
measurements show a clear highlight benefit.

## Numeric budgets

- f32 working RGB → OKLab → working RGB: relative/absolute error `<= 1e-4`.
- CPU/WGSL OKLab: error `<= 2e-5` (scaled for HDR inverse results).
- f16 storage budget: `<= 3e-3`; conversion math remains f32.
- Working-matrix neutral noise with chroma `<= 2e-5` is treated as achromatic.

The Rust and WGSL implementations remain explicit twins. Automatic shader
generation was not introduced because the existing compositor is a standalone
WGSL source; a numeric parity test and shader-source invariants guard drift.

## Verification

- Neutral axis, hue/cylindrical roundtrip, signed RGB, and HDR extremes.
- CPU/WGSL numerical parity and Naga shader validation.
- Feature-enabled JzAzBz signed/HDR finiteness.
- Full repository test suite.
