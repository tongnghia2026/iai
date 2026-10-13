# Phase 5 — Light and tone mapping v2

## Rendering modes

- **Perceptual** (default for new edits): strengthens the max-RGB ratio path, restores a small amount of coloured-shadow chroma, and only desaturates highlights in proportion to both tone compression and the real output-gamut excursion.
- **Film-like**: retains the previous softer per-channel shoulder and stronger highlight convergence. Old `.iai` documents deserialize to this mode.
- **Neutral**: uses the ratio path without highlight bleaching or shadow-chroma compensation, minimizing hue/chroma drift.

The UI keeps the existing Light workflow and adds one compact `Tone Mapping` selector.

## Light controls

- Exposure remains a pure scene-linear `2^EV` multiplier before tone mapping.
- Highlights/Shadows/Whites/Blacks remain EV-domain Gaussian zones sampled from the edge-aware regional exposure plane.
- Positive Shadows/Blacks now receive a continuous deep-sensor-floor confidence guard. It has no hard mask edge and does not affect the normal zone centres.
- The four controls are tested for monotonic response over a neutral HDR ramp.
- Negative Highlights is tested to recover chroma in a coloured HDR highlight.

## Curves and histogram

- The master curve explicitly offers **Perceptual** (OKLab L, preserves C/h until gamut mapping) and **Luminance** (legacy encoded-luma) modes.
- R/G/B tabs remain explicitly per-channel.
- Existing documents deserialize to Luminance mode; new edits default to Perceptual.
- The Develop histogram is generated through the same scene/tone/colour/display-curve chain that the settled image uses, and existing histogram-stage regression tests remain active.

## Automated gates

- Legacy serialization migration.
- Exposure and all four Light sliders monotonic.
- Deep-shadow noise guard continuity.
- Negative-highlight chroma recovery.
- No RGB channel-order inversion on coloured highlight ramps in all three modes.
- CPU/WGSL mirror validation.
- Actual headless GPU/CPU commit parity with an active Perceptual master curve: max 1/255, P99 1/255.

Manual review remains required before closing Phase 5.
