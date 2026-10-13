# sRGB/Rec.709 assumption inventory — August 2026

## Develop/RAW assumptions requiring migration

- `src/formats/raw.rs`: camera XYZ was converted directly into unclamped linear sRGB. New RAW masters now add the explicit linear-ProPhoto conversion.
- `src/core/develop_scene.rs`: CAT16, tone luminance, grading and the current gamut boundary operate in linear-sRGB coordinates. Phase 1 composes working → linear-sRGB into the shared matrix; Phase 2/4 will move perceptual and gamut operations to their dedicated models.
- `src/core/develop/color.rs` and `src/core/develop/mixer.rs`: display classification and current Oklab conversion assume sRGB. This is intentionally retained until Color Mixer v2.
- `src/gpu/compositor.wgsl`: shader twins use Rec.709 luma and sRGB transfer functions. The CPU-generated scene matrix now includes the selected working-space conversion, so storage primaries do not require duplicate WGSL constants.

## Valid boundary assumptions

- Raster canvas/tile compositing is currently display-referred sRGB and remains the compatibility boundary.
- JPEG and SVG are treated as sRGB when untagged; embedded ICC profiles are converted by lcms.
- PNG/TIFF/JPEG export embeds the document/output ICC profile when requested.
- Monitor and soft-proof transforms are applied through the display 3D LUT in `core::cms::build_display_lut`; the surface’s sRGB format alone is not treated as monitor color management.

## Non-Develop uses

Selection, grayscale conversion, blend helpers, print preview and vector display contain intentional Rec.709/sRGB calculations. They are outside the RAW scene-working buffer and must not be mechanically replaced with ACEScg coefficients.
