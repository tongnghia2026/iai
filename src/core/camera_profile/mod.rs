//! Camera-characterization resolution and provenance.
//!
//! This first vertical slice deliberately exposes only the decoder-matrix
//! compatibility path. It creates the stable boundary where DCP and camera ICC
//! candidates will be resolved later without changing the RAW decoder or the
//! scene/render contracts in the meantime.

pub mod dcp;
pub mod dcp_transform;
pub mod discovery;
pub mod embedded_dng;
pub mod icc_camera;
pub mod resolver;

/// RAW front-end that produced the sensor samples and compatibility matrix.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RawDecoderBackend {
    Rawloader,
    Rawler,
}

/// Characterization selected for a RAW scene master.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CameraProfileSource {
    /// Compatibility fallback derived from the decoder's normalized
    /// camera-to-XYZ matrix. This is the exact path used before the camera
    /// profile resolver existed.
    DecoderMatrix,
}

/// Human-readable, transient trace of how a RAW was characterized.
///
/// This belongs to [`crate::core::develop_scene::SceneSource`], which is not
/// serialized into `.iai`; it must not be confused with the ICC profile of the
/// rendered document pixels.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CameraProfileProvenance {
    pub decoder: RawDecoderBackend,
    pub source: CameraProfileSource,
    pub camera_make: String,
    pub camera_model: String,
}

impl CameraProfileProvenance {
    /// Whether characterization fell back to a decoder-provided matrix rather
    /// than a dedicated DCP/camera-ICC profile.
    pub fn is_fallback(&self) -> bool {
        matches!(self.source, CameraProfileSource::DecoderMatrix)
    }
}

/// Resolved technical camera characterization for the current compatibility
/// scene path.
///
/// `camera_to_linear_srgb` intentionally remains the current intermediate
/// boundary. The RAW writer still performs its existing linear-sRGB -> linear
/// ProPhoto conversion separately, preserving operation order and rounding.
#[derive(Clone, Debug, PartialEq)]
pub struct ResolvedCameraProfile {
    pub camera_to_linear_srgb: [[f32; 3]; 3],
    pub provenance: CameraProfileProvenance,
}

/// How the RAW decode aligned the scene master to the embedded camera JPEG.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JpegMatchMode {
    /// No embedded-JPEG brightness, colour, or tone fit was applied.
    None,
    /// Only the baseline brightness match was applied.
    BrightnessOnly,
    /// Baseline brightness plus the spatially fitted 3x3 colour matrix.
    BrightnessAndMatrix,
    /// Baseline brightness plus the fitted per-channel tone curves.
    BrightnessAndCurves,
    /// Bounded preview match: 3x3 matrix, a post-matrix scalar exposure fit,
    /// and luminance-preserving chroma enrichment. No RGB tone curves.
    PreviewSafe,
    /// Brightness plus the per-channel colour matrix and tone curve.
    Full,
}

/// Version of the decode-time RAW master recipe.
///
/// `LegacyBaked1` is the exact look that shipped before the Q1 technical/creative
/// split. `TechnicalNeutral2` is an opt-in audit recipe which removes the
/// global colour-NR, capture-sharpen, brightness, warmth, and chroma taste
/// stages. Keeping this in provenance makes A/B output explainable instead of
/// depending on an unrecorded collection of environment overrides.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RawRenderRecipeVersion {
    LegacyBaked1,
    TechnicalNeutral2,
}

impl RawRenderRecipeVersion {
    pub const fn name(self) -> &'static str {
        match self {
            Self::LegacyBaked1 => "legacy-baked-v1",
            Self::TechnicalNeutral2 => "technical-neutral-v2",
        }
    }
}

impl JpegMatchMode {
    /// Map the decoder's independently gated embedded-JPEG fit stages to stable
    /// provenance. Matrix/curve diagnostic modes always retain baseline gain.
    pub fn from_stages(
        apply_gain: bool,
        apply_matrix: bool,
        apply_curves: bool,
        preview_safe: bool,
    ) -> Self {
        match (apply_gain, apply_matrix, apply_curves, preview_safe) {
            (_, _, _, true) => Self::PreviewSafe,
            (_, true, true, false) => Self::Full,
            (true, true, false, false) => Self::BrightnessAndMatrix,
            (true, false, true, false) => Self::BrightnessAndCurves,
            (true, false, false, false) => Self::BrightnessOnly,
            (false, false, false, false) => Self::None,
            (false, true, false, false) => Self::BrightnessAndMatrix,
            (false, false, true, false) => Self::BrightnessAndCurves,
        }
    }
}

/// Transient trace of how a RAW scene master was characterized and matched.
///
/// This belongs to [`crate::core::develop_scene::SceneSource`], which is not
/// serialized into `.iai`. It carries the deterministic resolver's full
/// provenance plus the embedded-JPEG match mode chosen for this decode.
#[derive(Clone, Debug, PartialEq)]
pub struct RawSceneCharacterization {
    pub resolution: resolver::ResolverProvenance,
    pub jpeg_match: JpegMatchMode,
    pub raw_render_recipe: RawRenderRecipeVersion,
}

impl RawSceneCharacterization {
    /// Whether characterization fell back to the decoder compatibility matrix.
    pub fn is_decoder_fallback(&self) -> bool {
        matches!(
            self.resolution.selected,
            resolver::SelectedProfileProvenance::DecoderMatrix { .. }
        )
    }
}

// XYZ (D65) -> linear sRGB (D65). Keep these coefficients and the scalar
// accumulation below identical to the legacy implementation in formats/raw.rs;
// changing association or using `mul_add` could change f32 output bits.
const XYZ_TO_LINEAR_SRGB: [[f32; 3]; 3] = [
    [3.2404542, -1.5371385, -0.4985314],
    [-0.9692660, 1.8760108, 0.0415560],
    [0.0556434, -0.2040259, 1.0572252],
];

/// Resolve the compatibility camera-matrix fallback.
///
/// `camera_to_xyz` is the decoder's normalized `[XYZ][RGBE]` matrix. Ordinary
/// three-colour sensors have a zero E column, so the first three columns form
/// the same exact 3x3 composition used by the pre-resolver RAW path.
pub fn resolve_decoder_matrix(
    decoder: RawDecoderBackend,
    camera_make: &str,
    camera_model: &str,
    camera_to_xyz: &[[f32; 4]; 3],
) -> ResolvedCameraProfile {
    let mut camera_to_linear_srgb = [[0.0f32; 3]; 3];
    for i in 0..3 {
        for j in 0..3 {
            let mut sum = 0.0;
            for k in 0..3 {
                sum += XYZ_TO_LINEAR_SRGB[i][k] * camera_to_xyz[k][j];
            }
            camera_to_linear_srgb[i][j] = sum;
        }
    }

    ResolvedCameraProfile {
        camera_to_linear_srgb,
        provenance: CameraProfileProvenance {
            decoder,
            source: CameraProfileSource::DecoderMatrix,
            camera_make: camera_make.trim().to_string(),
            camera_model: camera_model.trim().to_string(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jpeg_match_provenance_distinguishes_matrix_and_curve_diagnostics() {
        assert_eq!(
            JpegMatchMode::from_stages(true, true, true, false),
            JpegMatchMode::Full
        );
        assert_eq!(
            JpegMatchMode::from_stages(true, true, false, false),
            JpegMatchMode::BrightnessAndMatrix
        );
        assert_eq!(
            JpegMatchMode::from_stages(true, false, true, false),
            JpegMatchMode::BrightnessAndCurves
        );
        assert_eq!(
            JpegMatchMode::from_stages(true, false, false, false),
            JpegMatchMode::BrightnessOnly
        );
        assert_eq!(
            JpegMatchMode::from_stages(false, false, false, false),
            JpegMatchMode::None
        );
        assert_eq!(
            JpegMatchMode::from_stages(true, true, false, true),
            JpegMatchMode::PreviewSafe
        );
    }

    /// Frozen copy of the matrix composition removed from `formats/raw.rs`.
    /// Keep this independent of the production constant so the test catches a
    /// coefficient, association, loop-bound, or column-order change.
    fn legacy_camera_to_srgb(camera_to_xyz: &[[f32; 4]; 3]) -> [[f32; 3]; 3] {
        let xyz_to_srgb = [
            [3.2404542f32, -1.5371385, -0.4985314],
            [-0.9692660, 1.8760108, 0.0415560],
            [0.0556434, -0.2040259, 1.0572252],
        ];
        let mut result = [[0.0f32; 3]; 3];
        for i in 0..3 {
            for j in 0..3 {
                let mut sum = 0.0;
                for k in 0..3 {
                    sum += xyz_to_srgb[i][k] * camera_to_xyz[k][j];
                }
                result[i][j] = sum;
            }
        }
        result
    }

    #[test]
    fn decoder_fallback_is_bit_exact_to_legacy_matrix_composition() {
        let fixtures = [
            [
                [1.0, 0.0, 0.0, 0.0],
                [0.0, 1.0, 0.0, 0.0],
                [0.0, 0.0, 1.0, 0.0],
            ],
            [
                [0.624_71, 0.205_11, 0.170_18, 0.0],
                [0.124_32, 0.804_16, 0.071_52, 0.0],
                [-0.018_41, 0.126_73, 0.891_68, 0.0],
            ],
            [
                [1.314, -0.229, -0.085, 0.25],
                [-0.112, 1.238, -0.126, -0.5],
                [0.034, -0.417, 1.383, 0.75],
            ],
        ];

        for camera_to_xyz in fixtures {
            let legacy = legacy_camera_to_srgb(&camera_to_xyz);
            let resolved = resolve_decoder_matrix(
                RawDecoderBackend::Rawloader,
                " Test Make ",
                " Test Model ",
                &camera_to_xyz,
            );
            for row in 0..3 {
                for column in 0..3 {
                    assert_eq!(
                        resolved.camera_to_linear_srgb[row][column].to_bits(),
                        legacy[row][column].to_bits(),
                        "coefficient [{row}][{column}] changed bits"
                    );
                }
            }
        }
    }

    #[test]
    fn fallback_provenance_preserves_decoder_and_clean_camera_identity() {
        let resolved = resolve_decoder_matrix(
            RawDecoderBackend::Rawler,
            "  Canon  ",
            " EOS R5 ",
            &[
                [1.0, 0.0, 0.0, 0.0],
                [0.0, 1.0, 0.0, 0.0],
                [0.0, 0.0, 1.0, 0.0],
            ],
        );
        assert_eq!(resolved.provenance.decoder, RawDecoderBackend::Rawler);
        assert_eq!(
            resolved.provenance.source,
            CameraProfileSource::DecoderMatrix
        );
        assert_eq!(resolved.provenance.camera_make, "Canon");
        assert_eq!(resolved.provenance.camera_model, "EOS R5");
        assert!(resolved.provenance.is_fallback());
    }
}
