# ADR-001: Develop Engine 2 graph contracts

Status: accepted for the first hybrid-rebuild milestone (2026-08-09).

## Decision

Develop UI state remains `DevelopSettings`. A serialized
`develop_engine_version` selects `Legacy1`, `Scene1`, or `Develop3`; absent
fields select `Scene1`, while the retired serialized value `Develop2` migrates
to `Develop3`.

The canonical Develop graph compiles that snapshot into a versioned sequence whose
edges declare color model, scene/display reference domain, precision, signed
range, and boundedness. Validation rejects representation mismatches, stage
order inversions, unsupported schema versions, and graphs without exactly the
declared final encode boundary. The graph signature includes canonical settings
and node versions for future cache invalidation.

The first vertical slice adopts the existing independently-written iAi scene
kernels behind this boundary. This deliberately preserves output while node
kernels migrate one by one. Preview tone construction, histogram evaluation,
full-resolution apply, and export-baked document pixels all validate the same
recipe; proxy code may reduce sampling only.

## Clean-room provenance

No ART source, constants, LUTs, profiles, resources, comments, or class layout
were used in this implementation. The contracts follow the project master plan
and generic CIE/ICC/DNG concepts: explicit color encodings, a scene-linear
unbounded working representation, a declared scene-to-display boundary, and a
single final encoding/quantization boundary. Existing iAi kernels remain iAi
code and are treated as compatibility implementations.

## Profile-aware input/scene boundary (2026-08-10 slice)

The graph now describes the *real* scene master color model instead of assuming
ProPhoto for every source. `compile_for_scene` reads the scene master's working
space (linear ProPhoto for RAW, linear sRGB for a display-referred layer) and
builds the scene-stage contracts from it; the signature folds in each edge's
color model so the two recipes cannot collide in a future cache. An
`InputBoundary` records the source color model, its provenance
(`RawCameraMatrix` or `DisplayReferredAssumedSrgb`), and the colorimetric
transform from the source working space into the D50 profile-connection space
(CIE XYZ). `execute_scene` validates the boundary (finite coefficients, adopted
white resolves to the D50 connection white) before rendering.

This is a contract/description change only. Pixel production still adopts the
Scene1 kernels, so `develop_color_golden` and the RGBA16 bit-exact parity slices
remain unchanged. The colorimetric core (`develop2/color.rs`) derives
RGB↔XYZ from primaries and Bradford adaptation from first principles; its unit
tests cross-check the derived sRGB→ProPhoto and sRGB→ACEScg matrices against the
matrices already shipping in `working_color.rs`, so the general path provably
reproduces current color without a second set of constants.

### Clean-room provenance (color core)

`develop2/color.rs` was written from published CIE 1931 colorimetry, the
standard RGB-primaries → XYZ matrix construction (SMPTE RP 177 linear algebra),
and the published Bradford cone-response matrix. Primary chromaticities are the
public specifications for sRGB/BT.709, ROMM (ProPhoto), ACEScg (AP1), Display
P3, and BT.2020. No third-party imaging engine source was consulted.

## Rollback

Select `Scene1` in the settings snapshot or revert the milestone commit. The
Scene1 renderer remains present and its code path is unchanged. The
profile-aware boundary is additive: reverting the 2026-08-10 slice restores the
fixed-ProPhoto `compile`/`execute_scene` path with no data conversion.
