# Phase 3 — Color Mixer v2

Date: 2026-08-09

## Implementation

- New documents/settings use a unified OKLCh mixer for band classification,
  hue, chroma, and perceptual lightness edits.
- Adjacent hue bands use a normalized periodic raised-cosine crossfade with an
  inner full-strength plateau. At most two adjacent bands overlap; weights are
  non-negative and sum to one.
- Neutral protection is a continuous OKLCh chroma confidence, with a lower
  threshold for removing weak colour casts.
- Old `.iai` documents and presets missing `mixer_algorithm` deserialize to the
  Legacy engine. New `DevelopSettings` default to V2.
- CPU and RAW GPU preview consume the same 360-entry curves. The WGSL path has
  the same OKLab transforms and response constants.
- Targeted eyedropper and grayscale mask-preview engine APIs exist, but are not
  added to the main UI before the optional Phase 7 UX review.

The existing Develop panel, slider ranges, labels, reset behavior, and workflow
are unchanged.

## Automated acceptance

- Raised-cosine normalization, isolation, overlap count, and 0/360 continuity.
- Hue-only preserves OKLCh L/C; saturation-only preserves L/h; luminance-only
  preserves C/h before later output gamut mapping.
- Neutral protection and targeted-mask behavior.
- Missing-version legacy migration and new-default V2 selection.
- Legacy behavior regression tests remain active explicitly on Legacy.
- Headless GPU/commit V2 parity stays within max 2/255 and P99 1/255.

Manual review remains required for skin, blue/cyan, red/orange, magenta wrap,
and neon/highlight material before Phase 3 can be marked complete.
