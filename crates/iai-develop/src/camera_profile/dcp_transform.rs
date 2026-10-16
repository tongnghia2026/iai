//! Pure colorimetric evaluation for parsed DNG camera profiles.
//!
//! The parser in [`super::dcp`] establishes container safety. This module is
//! the separate, side-effect-free layer that selects a calibration, builds the
//! technical camera transform, and evaluates `ProfileHueSatMapData`. It does
//! not inspect files, mutate a scene, or apply the creative `ProfileLookTable`
//! and `ProfileToneCurve` fields.
//!
//! The current parser supports RGB cameras without `AnalogBalance` or
//! `CameraCalibration`, so both are the identity here. With those terms made
//! explicit, the DNG equations used below are:
//!
//! * `XYZtoCamera = ColorMatrix`;
//! * without a forward matrix, `CameraToXYZD50 = Bradford(white -> D50) *
//!   inverse(XYZtoCamera)`;
//! * with a forward matrix, `CameraToXYZD50 = ForwardMatrix * D`, where
//!   `D = inverse(diag(CameraNeutral))`.
//!
//! RAW decoding already applies mosaic white-balance gains. Consequently the
//! render-facing matrix is `M_effective = M_profile * diag(1 / gain)`. Keeping
//! that compensation at this boundary prevents white balance from being
//! applied twice while retaining the DNG transform in its specified form.

use std::fmt;

use super::dcp::{DcpHsvAdjustment, DcpHsvTable, DcpProfile, DcpTableDimensions, DcpTableEncoding};

/// A double-precision 3x3 color matrix.
pub type Matrix3 = [[f64; 3]; 3];

/// ICC/DNG D50 profile-connection white, normalized to `Y = 1`.
pub const D50_XYZ: [f64; 3] = [0.964_22, 1.0, 0.825_21];

/// CIE XYZ D50 to linear ROMM RGB (ProPhoto RGB).
///
/// This is the inverse of the published ROMM RGB-to-XYZ matrix. No transfer
/// function is applied: both input and output are linear-light values.
pub const XYZ_D50_TO_LINEAR_PROPHOTO: Matrix3 = [
    [1.345_943_3, -0.255_607_5, -0.051_111_8],
    [-0.544_598_9, 1.508_167_3, 0.020_535_1],
    [0.0, 0.0, 1.211_812_8],
];

const BRADFORD: Matrix3 = [
    [0.895_1, 0.266_4, -0.161_4],
    [-0.750_2, 1.713_5, 0.036_7],
    [0.038_9, -0.068_5, 1.029_6],
];

const BRADFORD_INVERSE: Matrix3 = [
    [0.986_992_9, -0.147_054_3, 0.159_962_7],
    [0.432_305_3, 0.518_360_3, 0.049_291_2],
    [-0.008_528_7, 0.040_042_8, 0.968_486_7],
];

const MAX_NEUTRAL_ITERATIONS: u8 = 16;
const NEUTRAL_CONVERGENCE_MIRED: f64 = 0.01;

#[cfg(test)]
thread_local! {
    /// Test-only structural probe: prepared pixel evaluation must not re-enter
    /// whole-table validation. Thread-local storage keeps parallel tests isolated.
    static HUE_SAT_VALIDATION_CALLS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Explicit EXIF light sources accepted for reciprocal-temperature blending.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SupportedIlluminant {
    StandardA,
    D50,
    D55,
    D65,
    D75,
    IsoStudioTungsten,
}

/// A supported calibration light and its nominal correlated color temperature.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct IlluminantSpecification {
    pub illuminant: SupportedIlluminant,
    pub cct_kelvin: f64,
}

/// Why an EXIF `LightSource` value cannot drive DCP interpolation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnsupportedIlluminantReason {
    Unknown,
    /// Broad labels such as daylight, flash, or generic tungsten do not name a
    /// sufficiently precise calibration white.
    Vague,
    /// A named legacy standard that this deliberately small mapping does not
    /// implement (currently Standard B and Standard C).
    UnsupportedStandard,
    Custom,
    Unrecognized,
}

/// Failure to select or evaluate a technical DCP transform.
#[derive(Clone, Debug, PartialEq)]
pub enum DcpTransformError {
    InvalidCalibrationCount {
        actual: usize,
    },
    UnsupportedIlluminant {
        code: u16,
        reason: UnsupportedIlluminantReason,
    },
    InvalidCct {
        kelvin: f64,
    },
    CoincidentCalibrationTemperatures {
        first_kelvin: f64,
        second_kelvin: f64,
    },
    InvalidWhiteBalanceGain {
        channel: usize,
        value: f64,
    },
    InvalidCameraNeutral {
        channel: usize,
        value: f64,
    },
    NonFiniteMatrix {
        role: &'static str,
        row: usize,
        column: usize,
    },
    SingularMatrix {
        role: &'static str,
    },
    InvalidSourceWhite,
    MismatchedForwardMatrices,
    HueSatMapOnlyOnSecondCalibration,
    MismatchedHueSatDimensions,
    MismatchedHueSatEncoding,
    InvalidHueSatDimensions,
    HueSatEntryCount {
        expected: usize,
        actual: usize,
    },
    InvalidHueSatEntry {
        index: usize,
    },
    InvalidZeroSaturationEntry {
        index: usize,
    },
    HsvInputOutOfRange {
        channel: usize,
        value: f64,
    },
    NonFiniteRgb {
        channel: usize,
        value: f64,
    },
    Overflow,
}

impl fmt::Display for DcpTransformError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidCalibrationCount { actual } => {
                write!(
                    f,
                    "DCP has {actual} calibrations; exactly one or two are supported"
                )
            }
            Self::UnsupportedIlluminant { code, reason } => {
                write!(f, "DCP illuminant {code} is unsupported ({reason:?})")
            }
            Self::InvalidCct { kelvin } => {
                write!(f, "invalid correlated color temperature {kelvin} K")
            }
            Self::CoincidentCalibrationTemperatures {
                first_kelvin,
                second_kelvin,
            } => write!(
                f,
                "DCP calibration temperatures coincide ({first_kelvin} K and {second_kelvin} K)"
            ),
            Self::InvalidWhiteBalanceGain { channel, value } => {
                write!(f, "invalid white-balance gain {value} in channel {channel}")
            }
            Self::InvalidCameraNeutral { channel, value } => {
                write!(
                    f,
                    "invalid CameraNeutral value {value} in channel {channel}"
                )
            }
            Self::NonFiniteMatrix { role, row, column } => {
                write!(f, "{role} has a non-finite entry at [{row}][{column}]")
            }
            Self::SingularMatrix { role } => write!(f, "{role} is singular or ill-conditioned"),
            Self::InvalidSourceWhite => {
                f.write_str("CameraNeutral and ColorMatrix do not produce a physical source white")
            }
            Self::MismatchedForwardMatrices => {
                f.write_str("dual DCP calibrations must both provide ForwardMatrix or neither")
            }
            Self::HueSatMapOnlyOnSecondCalibration => {
                f.write_str("HueSatMapData2 cannot exist without HueSatMapData1")
            }
            Self::MismatchedHueSatDimensions => {
                f.write_str("dual DCP hue/saturation maps have different dimensions")
            }
            Self::MismatchedHueSatEncoding => {
                f.write_str("dual DCP hue/saturation maps have different encodings")
            }
            Self::InvalidHueSatDimensions => {
                f.write_str("DCP hue/saturation map has a zero dimension")
            }
            Self::HueSatEntryCount { expected, actual } => write!(
                f,
                "DCP hue/saturation map has {actual} entries; expected {expected}"
            ),
            Self::InvalidHueSatEntry { index } => {
                write!(f, "DCP hue/saturation map entry {index} is invalid")
            }
            Self::InvalidZeroSaturationEntry { index } => write!(
                f,
                "DCP hue/saturation map entry {index} changes value at zero saturation"
            ),
            Self::HsvInputOutOfRange { channel, value } => write!(
                f,
                "SDR hue/saturation-map input channel {channel} is outside [0, 1]: {value}"
            ),
            Self::NonFiniteRgb { channel, value } => {
                write!(f, "RGB channel {channel} is non-finite: {value}")
            }
            Self::Overflow => f.write_str("DCP table dimensions overflow addressable memory"),
        }
    }
}

impl std::error::Error for DcpTransformError {}

/// Where a calibration-selection temperature came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DcpSelectionOrigin {
    ExplicitCct,
    /// Fixed-point selection from `CameraNeutral`. The algorithm repeatedly
    /// interpolates `ColorMatrix`, maps the neutral back to XYZ, estimates CCT,
    /// and damps the update in reciprocal-temperature space.
    CameraNeutralIteration {
        iterations: u8,
        converged: bool,
    },
}

/// One selected/interpolated technical calibration.
#[derive(Clone, Debug, PartialEq)]
pub struct SelectedDcpCalibration {
    pub cct_kelvin: f64,
    /// Linear weight of calibration 2. Zero selects calibration 1; one selects
    /// calibration 2. The coordinate is reciprocal CCT and clamps at the ends.
    pub second_calibration_weight: f64,
    pub color_matrix: Matrix3,
    pub forward_matrix: Option<Matrix3>,
    pub hue_sat_map: Option<DcpHsvTable>,
    pub origin: DcpSelectionOrigin,
}

/// A complete technical camera transform in D50 XYZ and linear ProPhoto.
#[derive(Clone, Debug, PartialEq)]
pub struct DcpCameraTransform {
    pub camera_neutral: [f64; 3],
    pub selection: SelectedDcpCalibration,
    /// Source white inferred as `inverse(ColorMatrix) * CameraNeutral`, with
    /// `Y = 1`. Retained for diagnostics even when `ForwardMatrix` is present.
    pub source_white_xyz: [f64; 3],
    /// DNG camera-native values to D50 XYZ, before mosaic-WB compensation.
    pub camera_to_xyz_d50: Matrix3,
    /// DNG camera-native values to linear ProPhoto, before WB compensation.
    pub camera_to_linear_prophoto: Matrix3,
    /// Matrix for pixels whose mosaic samples already had `wb_gains` applied.
    pub post_wb_camera_to_linear_prophoto: Matrix3,
    /// The selected technical map. Creative look/tone metadata is intentionally
    /// absent from this scene-linear transform.
    pub hue_sat_map: Option<DcpHsvTable>,
}

/// Result status for the explicit scene-safe HueSatMap policy.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SceneSafeHueSatStatus {
    AppliedSdr,
    /// The last value slice was sampled, but the original HDR value magnitude
    /// was retained and scaled rather than clipped to one.
    AppliedAtHdrBoundary,
    /// HSV is not well-defined for signed RGB triplets, so the technical LUT
    /// was bypassed and the original signed value was retained exactly.
    BypassedSigned,
}

/// Output of [`apply_hue_sat_map_scene_safe`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SceneSafeHueSatResult {
    pub rgb: [f64; 3],
    pub status: SceneSafeHueSatStatus,
}

/// A validated HueSatMap with dimensions and strides prepared for repeated
/// pixel evaluation.
///
/// Construction scans and validates the complete table exactly once. The
/// sampling/application methods still validate their small per-pixel input,
/// but never rescan LUT entries or redo checked dimension arithmetic. A RAW
/// importer should construct one of these before entering its parallel pixel
/// loop instead of repeatedly calling the convenience functions below.
#[derive(Clone, Copy, Debug)]
pub struct PreparedHueSatMap<'a> {
    table: &'a DcpHsvTable,
    hue_divisions: usize,
    saturation_divisions: usize,
    value_divisions: usize,
    value_stride: usize,
}

impl<'a> PreparedHueSatMap<'a> {
    /// Validate and prepare one technical DCP HueSatMap.
    pub fn new(table: &'a DcpHsvTable) -> Result<Self, DcpTransformError> {
        validate_hue_sat_table(table)?;
        let hue_divisions = usize::try_from(table.dimensions.hue_divisions)
            .map_err(|_| DcpTransformError::Overflow)?;
        let saturation_divisions = usize::try_from(table.dimensions.saturation_divisions)
            .map_err(|_| DcpTransformError::Overflow)?;
        let value_divisions = usize::try_from(table.dimensions.value_divisions)
            .map_err(|_| DcpTransformError::Overflow)?;
        let value_stride = hue_divisions
            .checked_mul(saturation_divisions)
            .ok_or(DcpTransformError::Overflow)?;
        debug_assert_eq!(
            value_stride.checked_mul(value_divisions),
            Some(table.entries.len())
        );
        Ok(Self {
            table,
            hue_divisions,
            saturation_divisions,
            value_divisions,
            value_stride,
        })
    }

    /// Trilinearly sample normalized SDR HSV coordinates.
    pub fn sample_sdr(&self, hsv: [f64; 3]) -> Result<DcpHsvAdjustment, DcpTransformError> {
        for (channel, value) in hsv.into_iter().enumerate() {
            if !value.is_finite() || !(0.0..=1.0).contains(&value) {
                return Err(DcpTransformError::HsvInputOutOfRange { channel, value });
            }
        }
        Ok(sample_prepared_hue_sat_map(
            self,
            hsv[0],
            hsv[1],
            encoded_value(self.table.encoding, hsv[2]),
        ))
    }

    /// Apply this prepared table in the DNG SDR/spec domain.
    pub fn apply_sdr(&self, linear_prophoto: [f64; 3]) -> Result<[f64; 3], DcpTransformError> {
        validate_rgb_finite(linear_prophoto)?;
        for (channel, value) in linear_prophoto.into_iter().enumerate() {
            if !(0.0..=1.0).contains(&value) {
                return Err(DcpTransformError::HsvInputOutOfRange { channel, value });
            }
        }
        Ok(apply_prepared_hue_sat_map_nonnegative(self, linear_prophoto, false).rgb)
    }

    /// Apply this prepared table with the signed/HDR scene-safe policy.
    pub fn apply_scene_safe(
        &self,
        linear_prophoto: [f64; 3],
    ) -> Result<SceneSafeHueSatResult, DcpTransformError> {
        validate_rgb_finite(linear_prophoto)?;
        if linear_prophoto.into_iter().any(|value| value < 0.0) {
            return Ok(SceneSafeHueSatResult {
                rgb: linear_prophoto,
                status: SceneSafeHueSatStatus::BypassedSigned,
            });
        }
        let hdr = linear_prophoto.into_iter().any(|value| value > 1.0);
        Ok(apply_prepared_hue_sat_map_nonnegative(
            self,
            linear_prophoto,
            hdr,
        ))
    }

    #[inline]
    fn index(&self, value: usize, hue: usize, saturation: usize) -> usize {
        debug_assert!(value < self.value_divisions);
        debug_assert!(hue < self.hue_divisions);
        debug_assert!(saturation < self.saturation_divisions);
        value * self.value_stride + hue * self.saturation_divisions + saturation
    }
}

/// Resolve an EXIF `LightSource` code to the exact small set supported here.
///
/// Numeric mapping: Standard A = 17, D55 = 20, D65 = 21, D75 = 22, D50 = 23,
/// and ISO studio tungsten = 24. Generic/vague and custom values return a
/// typed error so the resolver can deliberately fall back to another profile.
pub fn illuminant_specification(code: u16) -> Result<IlluminantSpecification, DcpTransformError> {
    let (illuminant, cct_kelvin) = match code {
        17 => (SupportedIlluminant::StandardA, 2_856.0),
        20 => (SupportedIlluminant::D55, 5_503.0),
        21 => (SupportedIlluminant::D65, 6_504.0),
        22 => (SupportedIlluminant::D75, 7_504.0),
        23 => (SupportedIlluminant::D50, 5_003.0),
        24 => (SupportedIlluminant::IsoStudioTungsten, 3_200.0),
        0 => {
            return Err(DcpTransformError::UnsupportedIlluminant {
                code,
                reason: UnsupportedIlluminantReason::Unknown,
            });
        }
        1..=16 => {
            return Err(DcpTransformError::UnsupportedIlluminant {
                code,
                reason: UnsupportedIlluminantReason::Vague,
            });
        }
        18 | 19 => {
            return Err(DcpTransformError::UnsupportedIlluminant {
                code,
                reason: UnsupportedIlluminantReason::UnsupportedStandard,
            });
        }
        255 => {
            return Err(DcpTransformError::UnsupportedIlluminant {
                code,
                reason: UnsupportedIlluminantReason::Custom,
            });
        }
        _ => {
            return Err(DcpTransformError::UnsupportedIlluminant {
                code,
                reason: UnsupportedIlluminantReason::Unrecognized,
            });
        }
    };
    Ok(IlluminantSpecification {
        illuminant,
        cct_kelvin,
    })
}

/// Convert the three positive mosaic white-balance gains to DNG
/// `CameraNeutral`: reciprocal gains normalized so their largest coordinate is
/// one. Scaling all gains uniformly therefore does not change the neutral.
pub fn camera_neutral_from_wb_gains(wb_gains: [f64; 3]) -> Result<[f64; 3], DcpTransformError> {
    let mut neutral = [0.0; 3];
    for (channel, (&gain, slot)) in wb_gains.iter().zip(&mut neutral).enumerate() {
        if !gain.is_finite() || gain <= 0.0 {
            return Err(DcpTransformError::InvalidWhiteBalanceGain {
                channel,
                value: gain,
            });
        }
        *slot = 1.0 / gain;
        if !slot.is_finite() {
            return Err(DcpTransformError::InvalidWhiteBalanceGain {
                channel,
                value: gain,
            });
        }
    }
    let scale = neutral.into_iter().fold(0.0f64, f64::max);
    for value in &mut neutral {
        *value /= scale;
    }
    validate_camera_neutral(neutral)?;
    Ok(neutral)
}

/// Select/interpolate a DCP at an explicit CCT.
///
/// Dual-illuminant interpolation is linear in reciprocal CCT and clamps beyond
/// either endpoint. Matrices and HueSatMap entries are interpolated before any
/// matrix inversion. Reversing the order of the two illuminants therefore only
/// swaps the reported endpoint weight, not the selected transform.
pub fn select_calibration_at_cct(
    profile: &DcpProfile,
    cct_kelvin: f64,
) -> Result<SelectedDcpCalibration, DcpTransformError> {
    select_calibration_at_cct_with_origin(profile, cct_kelvin, DcpSelectionOrigin::ExplicitCct)
}

/// Select a DCP by solving the ColorMatrix/CameraNeutral dependency.
///
/// Selection starts at the reciprocal-temperature midpoint. Each iteration
/// interpolates `ColorMatrix`, derives `XYZ = inverse(ColorMatrix) *
/// CameraNeutral`, estimates its CCT with McCamy's xy approximation, clamps it
/// to the profile's calibration interval, then damps the update halfway in
/// reciprocal-temperature space. The bounded iteration is deterministic; its
/// convergence state is exposed in [`DcpSelectionOrigin`] rather than hidden.
pub fn select_calibration_for_camera_neutral(
    profile: &DcpProfile,
    camera_neutral: [f64; 3],
) -> Result<SelectedDcpCalibration, DcpTransformError> {
    validate_camera_neutral(camera_neutral)?;
    validate_calibration_count(profile)?;

    if profile.calibrations.len() == 1 {
        let cct = illuminant_specification(profile.calibrations[0].illuminant)?.cct_kelvin;
        return select_calibration_at_cct_with_origin(
            profile,
            cct,
            DcpSelectionOrigin::CameraNeutralIteration {
                iterations: 0,
                converged: true,
            },
        );
    }

    let first = illuminant_specification(profile.calibrations[0].illuminant)?.cct_kelvin;
    let second = illuminant_specification(profile.calibrations[1].illuminant)?.cct_kelvin;
    ensure_distinct_temperatures(first, second)?;
    let low = first.min(second);
    let high = first.max(second);

    // Midpoint in the same reciprocal-temperature domain used by DNG.
    let mut reciprocal = 0.5 * (first.recip() + second.recip());
    let mut iterations = 0;
    let mut converged = false;

    for iteration in 1..=MAX_NEUTRAL_ITERATIONS {
        iterations = iteration;
        let cct = reciprocal.recip();
        let selected =
            select_calibration_at_cct_with_origin(profile, cct, DcpSelectionOrigin::ExplicitCct)?;
        let inverse = invert_matrix(&selected.color_matrix, "selected ColorMatrix")?;
        let source_white = source_white_from_inverse(&inverse, camera_neutral)?;
        let estimated = estimate_cct_from_xyz(source_white)?.clamp(low, high);
        let estimated_reciprocal = estimated.recip();
        let delta_mired = (estimated_reciprocal - reciprocal).abs() * 1.0e6;
        if delta_mired <= NEUTRAL_CONVERGENCE_MIRED {
            reciprocal = estimated_reciprocal;
            converged = true;
            break;
        }
        reciprocal = 0.5 * (reciprocal + estimated_reciprocal);
    }

    select_calibration_at_cct_with_origin(
        profile,
        reciprocal.recip(),
        DcpSelectionOrigin::CameraNeutralIteration {
            iterations,
            converged,
        },
    )
}

/// Build the technical DCP transform, deriving profile selection from the
/// white-balance gains through the bounded neutral/CCT iteration.
pub fn build_camera_transform(
    profile: &DcpProfile,
    wb_gains: [f64; 3],
) -> Result<DcpCameraTransform, DcpTransformError> {
    let camera_neutral = camera_neutral_from_wb_gains(wb_gains)?;
    let selection = select_calibration_for_camera_neutral(profile, camera_neutral)?;
    build_camera_transform_from_selection(selection, camera_neutral, wb_gains)
}

/// Build the technical DCP transform at an externally determined CCT.
///
/// This is useful when a decoder provides a trusted temperature directly and
/// also makes calibration endpoint behavior independently testable.
pub fn build_camera_transform_at_cct(
    profile: &DcpProfile,
    wb_gains: [f64; 3],
    cct_kelvin: f64,
) -> Result<DcpCameraTransform, DcpTransformError> {
    let camera_neutral = camera_neutral_from_wb_gains(wb_gains)?;
    let selection = select_calibration_at_cct(profile, cct_kelvin)?;
    build_camera_transform_from_selection(selection, camera_neutral, wb_gains)
}

/// Compensate a profile matrix for white balance already applied to mosaic
/// samples: `M_effective = M_profile * diag(1 / gain)`.
pub fn compensate_for_applied_white_balance(
    profile_matrix: &Matrix3,
    wb_gains: [f64; 3],
) -> Result<Matrix3, DcpTransformError> {
    validate_matrix(profile_matrix, "camera profile matrix")?;
    let mut inverse_gain = [[0.0; 3]; 3];
    for channel in 0..3 {
        let gain = wb_gains[channel];
        if !gain.is_finite() || gain <= 0.0 {
            return Err(DcpTransformError::InvalidWhiteBalanceGain {
                channel,
                value: gain,
            });
        }
        inverse_gain[channel][channel] = 1.0 / gain;
    }
    checked_matrix_multiply(profile_matrix, &inverse_gain, "post-WB camera matrix")
}

/// Apply a matrix to a vector without clamping signed or HDR values.
pub fn apply_matrix(matrix: &Matrix3, value: [f64; 3]) -> [f64; 3] {
    [
        matrix[0][0] * value[0] + matrix[0][1] * value[1] + matrix[0][2] * value[2],
        matrix[1][0] * value[0] + matrix[1][1] * value[1] + matrix[1][2] * value[2],
        matrix[2][0] * value[0] + matrix[2][1] * value[1] + matrix[2][2] * value[2],
    ]
}

/// Interpolate two compatible technical HueSatMaps entry-by-entry.
pub fn interpolate_hue_sat_tables(
    first: &DcpHsvTable,
    second: &DcpHsvTable,
    second_weight: f64,
) -> Result<DcpHsvTable, DcpTransformError> {
    validate_hue_sat_table(first)?;
    validate_hue_sat_table(second)?;
    if first.dimensions != second.dimensions {
        return Err(DcpTransformError::MismatchedHueSatDimensions);
    }
    if first.encoding != second.encoding {
        return Err(DcpTransformError::MismatchedHueSatEncoding);
    }
    if !second_weight.is_finite() {
        return Err(DcpTransformError::InvalidCct {
            kelvin: second_weight,
        });
    }
    let weight = second_weight.clamp(0.0, 1.0);
    if weight == 0.0 {
        return Ok(first.clone());
    }
    if weight == 1.0 {
        return Ok(second.clone());
    }

    let entries = first
        .entries
        .iter()
        .zip(&second.entries)
        .map(|(a, b)| DcpHsvAdjustment {
            hue_shift_degrees: lerp(
                f64::from(a.hue_shift_degrees),
                f64::from(b.hue_shift_degrees),
                weight,
            ) as f32,
            saturation_scale: lerp(
                f64::from(a.saturation_scale),
                f64::from(b.saturation_scale),
                weight,
            ) as f32,
            value_scale: lerp(f64::from(a.value_scale), f64::from(b.value_scale), weight) as f32,
        })
        .collect();
    let table = DcpHsvTable {
        dimensions: first.dimensions,
        encoding: first.encoding,
        entries,
    };
    validate_hue_sat_table(&table)?;
    Ok(table)
}

/// Trilinearly sample a DCP table at normalized SDR HSV coordinates.
///
/// Hue is cyclic, so the last hue division interpolates back to division zero.
/// Hue and value dimensions of one are supported; DNG requires at least two
/// saturation divisions. The value coordinate honors `ProfileHueSatMapEncoding`;
/// sRGB encoding changes only the lookup coordinate.
///
/// This convenience API validates the table for each call. Repeated pixel
/// evaluation should construct [`PreparedHueSatMap`] once instead.
pub fn sample_hue_sat_map(
    table: &DcpHsvTable,
    hsv: [f64; 3],
) -> Result<DcpHsvAdjustment, DcpTransformError> {
    PreparedHueSatMap::new(table)?.sample_sdr(hsv)
}

/// Apply a DNG technical HueSatMap according to its SDR/spec domain.
///
/// Input is linear ProPhoto RGB and every channel must be finite and inside
/// `[0, 1]`. Following the DNG algorithm, adjusted saturation and value are
/// constrained to `[0, 1]` before conversion back to RGB.
/// Repeated pixel evaluation should use [`PreparedHueSatMap::apply_sdr`].
pub fn apply_hue_sat_map_sdr(
    table: &DcpHsvTable,
    linear_prophoto: [f64; 3],
) -> Result<[f64; 3], DcpTransformError> {
    PreparedHueSatMap::new(table)?.apply_sdr(linear_prophoto)
}

/// Apply a technical HueSatMap with an explicit scene-linear safety policy.
///
/// * Any signed RGB triplet bypasses the HSV table exactly, because ordinary
///   HSV has no meaningful signed-light definition.
/// * Non-negative HDR retains its original value magnitude. Only the table's
///   lookup coordinate is pinned to the last value slice; the pixel itself is
///   never clipped.
/// * SDR input follows [`apply_hue_sat_map_sdr`].
///
/// This checked convenience API validates the table for each call. RAW hot
/// paths should use [`PreparedHueSatMap::apply_scene_safe`].
pub fn apply_hue_sat_map_scene_safe(
    table: &DcpHsvTable,
    linear_prophoto: [f64; 3],
) -> Result<SceneSafeHueSatResult, DcpTransformError> {
    PreparedHueSatMap::new(table)?.apply_scene_safe(linear_prophoto)
}

fn select_calibration_at_cct_with_origin(
    profile: &DcpProfile,
    cct_kelvin: f64,
    origin: DcpSelectionOrigin,
) -> Result<SelectedDcpCalibration, DcpTransformError> {
    validate_cct(cct_kelvin)?;
    validate_calibration_count(profile)?;
    let first = &profile.calibrations[0];
    let first_spec = illuminant_specification(first.illuminant)?;
    validate_matrix(&first.color_matrix, "ColorMatrix1")?;
    if let Some(matrix) = &first.forward_matrix {
        validate_matrix(matrix, "ForwardMatrix1")?;
    }
    if let Some(table) = &first.hue_sat_map {
        validate_hue_sat_table(table)?;
    }

    if profile.calibrations.len() == 1 {
        return Ok(SelectedDcpCalibration {
            cct_kelvin,
            second_calibration_weight: 0.0,
            color_matrix: first.color_matrix,
            forward_matrix: first.forward_matrix,
            hue_sat_map: first.hue_sat_map.clone(),
            origin,
        });
    }

    let second = &profile.calibrations[1];
    let second_spec = illuminant_specification(second.illuminant)?;
    validate_matrix(&second.color_matrix, "ColorMatrix2")?;
    if let Some(matrix) = &second.forward_matrix {
        validate_matrix(matrix, "ForwardMatrix2")?;
    }
    if let Some(table) = &second.hue_sat_map {
        validate_hue_sat_table(table)?;
    }

    ensure_distinct_temperatures(first_spec.cct_kelvin, second_spec.cct_kelvin)?;
    let weight =
        reciprocal_temperature_weight(first_spec.cct_kelvin, second_spec.cct_kelvin, cct_kelvin)?;
    let color_matrix = interpolate_matrix(&first.color_matrix, &second.color_matrix, weight);
    validate_matrix(&color_matrix, "interpolated ColorMatrix")?;

    let forward_matrix = match (&first.forward_matrix, &second.forward_matrix) {
        (None, None) => None,
        (Some(a), Some(b)) => {
            let matrix = interpolate_matrix(a, b, weight);
            validate_matrix(&matrix, "interpolated ForwardMatrix")?;
            Some(matrix)
        }
        _ => return Err(DcpTransformError::MismatchedForwardMatrices),
    };

    // DNG permits Data1 to stand in for both illuminants when Data2 is absent.
    let hue_sat_map = match (&first.hue_sat_map, &second.hue_sat_map) {
        (None, None) => None,
        (Some(a), None) => Some(a.clone()),
        (None, Some(_)) => return Err(DcpTransformError::HueSatMapOnlyOnSecondCalibration),
        (Some(a), Some(b)) => Some(interpolate_hue_sat_tables(a, b, weight)?),
    };

    Ok(SelectedDcpCalibration {
        cct_kelvin,
        second_calibration_weight: weight,
        color_matrix,
        forward_matrix,
        hue_sat_map,
        origin,
    })
}

fn build_camera_transform_from_selection(
    selection: SelectedDcpCalibration,
    camera_neutral: [f64; 3],
    wb_gains: [f64; 3],
) -> Result<DcpCameraTransform, DcpTransformError> {
    validate_camera_neutral(camera_neutral)?;
    let inverse_color = invert_matrix(&selection.color_matrix, "selected ColorMatrix")?;
    let source_white_xyz = source_white_from_inverse(&inverse_color, camera_neutral)?;

    let camera_to_xyz_d50 = if let Some(forward) = &selection.forward_matrix {
        let neutral_inverse = [
            [1.0 / camera_neutral[0], 0.0, 0.0],
            [0.0, 1.0 / camera_neutral[1], 0.0],
            [0.0, 0.0, 1.0 / camera_neutral[2]],
        ];
        checked_matrix_multiply(forward, &neutral_inverse, "ForwardMatrix * D")?
    } else {
        let adaptation = bradford_adaptation(source_white_xyz, D50_XYZ)?;
        checked_matrix_multiply(
            &adaptation,
            &inverse_color,
            "Bradford-adapted inverse ColorMatrix",
        )?
    };
    validate_matrix(&camera_to_xyz_d50, "camera to XYZ D50")?;

    let camera_to_linear_prophoto = checked_matrix_multiply(
        &XYZ_D50_TO_LINEAR_PROPHOTO,
        &camera_to_xyz_d50,
        "camera to linear ProPhoto",
    )?;
    let post_wb_camera_to_linear_prophoto =
        compensate_for_applied_white_balance(&camera_to_linear_prophoto, wb_gains)?;
    let hue_sat_map = selection.hue_sat_map.clone();

    Ok(DcpCameraTransform {
        camera_neutral,
        selection,
        source_white_xyz,
        camera_to_xyz_d50,
        camera_to_linear_prophoto,
        post_wb_camera_to_linear_prophoto,
        hue_sat_map,
    })
}

fn validate_calibration_count(profile: &DcpProfile) -> Result<(), DcpTransformError> {
    if !(1..=2).contains(&profile.calibrations.len()) {
        return Err(DcpTransformError::InvalidCalibrationCount {
            actual: profile.calibrations.len(),
        });
    }
    Ok(())
}

fn validate_cct(cct_kelvin: f64) -> Result<(), DcpTransformError> {
    if !cct_kelvin.is_finite() || cct_kelvin <= 0.0 || !cct_kelvin.recip().is_finite() {
        return Err(DcpTransformError::InvalidCct { kelvin: cct_kelvin });
    }
    Ok(())
}

fn ensure_distinct_temperatures(first: f64, second: f64) -> Result<(), DcpTransformError> {
    if first == second {
        return Err(DcpTransformError::CoincidentCalibrationTemperatures {
            first_kelvin: first,
            second_kelvin: second,
        });
    }
    Ok(())
}

fn reciprocal_temperature_weight(
    first_kelvin: f64,
    second_kelvin: f64,
    target_kelvin: f64,
) -> Result<f64, DcpTransformError> {
    validate_cct(first_kelvin)?;
    validate_cct(second_kelvin)?;
    validate_cct(target_kelvin)?;
    ensure_distinct_temperatures(first_kelvin, second_kelvin)?;
    let first = first_kelvin.recip();
    let second = second_kelvin.recip();
    Ok(((target_kelvin.recip() - first) / (second - first)).clamp(0.0, 1.0))
}

fn interpolate_matrix(first: &Matrix3, second: &Matrix3, weight: f64) -> Matrix3 {
    let mut result = [[0.0; 3]; 3];
    for row in 0..3 {
        for column in 0..3 {
            result[row][column] = lerp(first[row][column], second[row][column], weight);
        }
    }
    result
}

#[inline]
fn lerp(first: f64, second: f64, second_weight: f64) -> f64 {
    first * (1.0 - second_weight) + second * second_weight
}

fn validate_camera_neutral(neutral: [f64; 3]) -> Result<(), DcpTransformError> {
    for (channel, value) in neutral.into_iter().enumerate() {
        if !value.is_finite() || value <= 0.0 {
            return Err(DcpTransformError::InvalidCameraNeutral { channel, value });
        }
    }
    Ok(())
}

fn source_white_from_inverse(
    inverse_color_matrix: &Matrix3,
    camera_neutral: [f64; 3],
) -> Result<[f64; 3], DcpTransformError> {
    let mut xyz = apply_matrix(inverse_color_matrix, camera_neutral);
    if xyz
        .into_iter()
        .any(|value| !value.is_finite() || value <= 0.0)
        || xyz[1] <= 1.0e-15
    {
        return Err(DcpTransformError::InvalidSourceWhite);
    }
    let y = xyz[1];
    for value in &mut xyz {
        *value /= y;
    }
    if xyz
        .into_iter()
        .any(|value| !value.is_finite() || value <= 0.0)
    {
        return Err(DcpTransformError::InvalidSourceWhite);
    }
    Ok(xyz)
}

fn estimate_cct_from_xyz(xyz: [f64; 3]) -> Result<f64, DcpTransformError> {
    let sum = xyz[0] + xyz[1] + xyz[2];
    if !sum.is_finite() || sum <= 0.0 {
        return Err(DcpTransformError::InvalidSourceWhite);
    }
    let x = xyz[0] / sum;
    let y = xyz[1] / sum;
    let denominator = y - 0.1858;
    if !x.is_finite() || !y.is_finite() || denominator.abs() <= 1.0e-12 {
        return Err(DcpTransformError::InvalidSourceWhite);
    }
    let n = (x - 0.3320) / denominator;
    let cct = -449.0 * n.powi(3) + 3_525.0 * n.powi(2) - 6_823.3 * n + 5_520.33;
    validate_cct(cct)?;
    Ok(cct)
}

fn bradford_adaptation(
    source_white: [f64; 3],
    destination_white: [f64; 3],
) -> Result<Matrix3, DcpTransformError> {
    let source_cone = apply_matrix(&BRADFORD, source_white);
    let destination_cone = apply_matrix(&BRADFORD, destination_white);
    if source_cone
        .into_iter()
        .chain(destination_cone)
        .any(|value| !value.is_finite() || value.abs() <= 1.0e-15)
    {
        return Err(DcpTransformError::InvalidSourceWhite);
    }
    let scale = [
        [destination_cone[0] / source_cone[0], 0.0, 0.0],
        [0.0, destination_cone[1] / source_cone[1], 0.0],
        [0.0, 0.0, destination_cone[2] / source_cone[2]],
    ];
    let scaled = checked_matrix_multiply(&scale, &BRADFORD, "Bradford cone scaling")?;
    checked_matrix_multiply(&BRADFORD_INVERSE, &scaled, "Bradford adaptation")
}

fn determinant(matrix: &Matrix3) -> f64 {
    matrix[0][0] * (matrix[1][1] * matrix[2][2] - matrix[1][2] * matrix[2][1])
        - matrix[0][1] * (matrix[1][0] * matrix[2][2] - matrix[1][2] * matrix[2][0])
        + matrix[0][2] * (matrix[1][0] * matrix[2][1] - matrix[1][1] * matrix[2][0])
}

fn validate_matrix(matrix: &Matrix3, role: &'static str) -> Result<(), DcpTransformError> {
    let mut scale = 0.0f64;
    for (row, values) in matrix.iter().enumerate() {
        for (column, &value) in values.iter().enumerate() {
            if !value.is_finite() {
                return Err(DcpTransformError::NonFiniteMatrix { role, row, column });
            }
            scale = scale.max(value.abs());
        }
    }
    let det = determinant(matrix);
    let threshold = scale.powi(3) * 1.0e-12;
    if scale == 0.0 || !det.is_finite() || det.abs() <= threshold {
        return Err(DcpTransformError::SingularMatrix { role });
    }
    Ok(())
}

fn invert_matrix(matrix: &Matrix3, role: &'static str) -> Result<Matrix3, DcpTransformError> {
    validate_matrix(matrix, role)?;
    let inverse_determinant = determinant(matrix).recip();
    let result = [
        [
            (matrix[1][1] * matrix[2][2] - matrix[1][2] * matrix[2][1]) * inverse_determinant,
            (matrix[0][2] * matrix[2][1] - matrix[0][1] * matrix[2][2]) * inverse_determinant,
            (matrix[0][1] * matrix[1][2] - matrix[0][2] * matrix[1][1]) * inverse_determinant,
        ],
        [
            (matrix[1][2] * matrix[2][0] - matrix[1][0] * matrix[2][2]) * inverse_determinant,
            (matrix[0][0] * matrix[2][2] - matrix[0][2] * matrix[2][0]) * inverse_determinant,
            (matrix[0][2] * matrix[1][0] - matrix[0][0] * matrix[1][2]) * inverse_determinant,
        ],
        [
            (matrix[1][0] * matrix[2][1] - matrix[1][1] * matrix[2][0]) * inverse_determinant,
            (matrix[0][1] * matrix[2][0] - matrix[0][0] * matrix[2][1]) * inverse_determinant,
            (matrix[0][0] * matrix[1][1] - matrix[0][1] * matrix[1][0]) * inverse_determinant,
        ],
    ];
    validate_matrix(&result, "inverse matrix")?;
    Ok(result)
}

fn checked_matrix_multiply(
    first: &Matrix3,
    second: &Matrix3,
    role: &'static str,
) -> Result<Matrix3, DcpTransformError> {
    let mut result = [[0.0; 3]; 3];
    for row in 0..3 {
        for column in 0..3 {
            result[row][column] = first[row][0] * second[0][column]
                + first[row][1] * second[1][column]
                + first[row][2] * second[2][column];
        }
    }
    for (row, values) in result.iter().enumerate() {
        for (column, value) in values.iter().enumerate() {
            if !value.is_finite() {
                return Err(DcpTransformError::NonFiniteMatrix { role, row, column });
            }
        }
    }
    Ok(result)
}

fn validate_hue_sat_table(table: &DcpHsvTable) -> Result<(), DcpTransformError> {
    #[cfg(test)]
    HUE_SAT_VALIDATION_CALLS.with(|calls| calls.set(calls.get() + 1));

    let dimensions = table.dimensions;
    if dimensions.hue_divisions == 0
        || dimensions.saturation_divisions < 2
        || dimensions.value_divisions == 0
    {
        return Err(DcpTransformError::InvalidHueSatDimensions);
    }
    let expected = table_entry_count(dimensions)?;
    if table.entries.len() != expected {
        return Err(DcpTransformError::HueSatEntryCount {
            expected,
            actual: table.entries.len(),
        });
    }
    let saturation_divisions = usize::try_from(dimensions.saturation_divisions)
        .map_err(|_| DcpTransformError::Overflow)?;
    for (index, entry) in table.entries.iter().enumerate() {
        if !entry.hue_shift_degrees.is_finite()
            || !entry.saturation_scale.is_finite()
            || !entry.value_scale.is_finite()
            || entry.saturation_scale < 0.0
            || entry.value_scale < 0.0
        {
            return Err(DcpTransformError::InvalidHueSatEntry { index });
        }
        if index % saturation_divisions == 0 && entry.value_scale != 1.0 {
            return Err(DcpTransformError::InvalidZeroSaturationEntry { index });
        }
    }
    Ok(())
}

fn table_entry_count(dimensions: DcpTableDimensions) -> Result<usize, DcpTransformError> {
    let hue = usize::try_from(dimensions.hue_divisions).map_err(|_| DcpTransformError::Overflow)?;
    let saturation = usize::try_from(dimensions.saturation_divisions)
        .map_err(|_| DcpTransformError::Overflow)?;
    let value =
        usize::try_from(dimensions.value_divisions).map_err(|_| DcpTransformError::Overflow)?;
    hue.checked_mul(saturation)
        .and_then(|product| product.checked_mul(value))
        .ok_or(DcpTransformError::Overflow)
}

fn sample_prepared_hue_sat_map(
    prepared: &PreparedHueSatMap<'_>,
    hue: f64,
    saturation: f64,
    encoded_lookup_value: f64,
) -> DcpHsvAdjustment {
    let hue_coordinate = hue.rem_euclid(1.0) * prepared.hue_divisions as f64;
    let hue_floor = hue_coordinate.floor();
    let hue_zero = (hue_floor as usize) % prepared.hue_divisions;
    let hue_one = if prepared.hue_divisions == 1 {
        0
    } else {
        (hue_zero + 1) % prepared.hue_divisions
    };
    let hue_fraction = if prepared.hue_divisions == 1 {
        0.0
    } else {
        hue_coordinate - hue_floor
    };

    let (saturation_zero, saturation_one, saturation_fraction) =
        linear_axis(saturation, prepared.saturation_divisions);
    let (value_zero, value_one, value_fraction) =
        linear_axis(encoded_lookup_value, prepared.value_divisions);

    let mut result = [0.0f64; 3];
    for (value_index, value_weight) in [
        (value_zero, 1.0 - value_fraction),
        (value_one, value_fraction),
    ] {
        for (hue_index, hue_weight) in [(hue_zero, 1.0 - hue_fraction), (hue_one, hue_fraction)] {
            for (saturation_index, saturation_weight) in [
                (saturation_zero, 1.0 - saturation_fraction),
                (saturation_one, saturation_fraction),
            ] {
                let weight = value_weight * hue_weight * saturation_weight;
                if weight == 0.0 {
                    continue;
                }
                let index = prepared.index(value_index, hue_index, saturation_index);
                let entry = prepared.table.entries[index];
                result[0] += weight * f64::from(entry.hue_shift_degrees);
                result[1] += weight * f64::from(entry.saturation_scale);
                result[2] += weight * f64::from(entry.value_scale);
            }
        }
    }
    DcpHsvAdjustment {
        hue_shift_degrees: result[0] as f32,
        saturation_scale: result[1] as f32,
        value_scale: result[2] as f32,
    }
}

fn linear_axis(coordinate: f64, divisions: usize) -> (usize, usize, f64) {
    if divisions == 1 {
        return (0, 0, 0.0);
    }
    let scaled = coordinate.clamp(0.0, 1.0) * (divisions - 1) as f64;
    let zero = scaled.floor() as usize;
    let one = (zero + 1).min(divisions - 1);
    (zero, one, scaled - zero as f64)
}

fn encoded_value(encoding: DcpTableEncoding, linear_value: f64) -> f64 {
    match encoding {
        DcpTableEncoding::Linear => linear_value,
        DcpTableEncoding::Srgb => {
            if linear_value <= 0.003_130_8 {
                linear_value * 12.92
            } else {
                1.055 * linear_value.powf(1.0 / 2.4) - 0.055
            }
        }
    }
}

fn apply_prepared_hue_sat_map_nonnegative(
    prepared: &PreparedHueSatMap<'_>,
    rgb: [f64; 3],
    hdr: bool,
) -> SceneSafeHueSatResult {
    let (hue, saturation, value) = rgb_to_hsv_nonnegative(rgb);
    if saturation == 0.0 {
        return SceneSafeHueSatResult {
            rgb,
            status: if hdr {
                SceneSafeHueSatStatus::AppliedAtHdrBoundary
            } else {
                SceneSafeHueSatStatus::AppliedSdr
            },
        };
    }
    let lookup_value = encoded_value(prepared.table.encoding, value.min(1.0));
    let adjustment = sample_prepared_hue_sat_map(prepared, hue, saturation, lookup_value);
    let adjusted_hue = (hue + f64::from(adjustment.hue_shift_degrees) / 360.0).rem_euclid(1.0);
    // DNG constrains adjusted saturation to its normalized domain. The SDR
    // path similarly constrains value. Our documented HDR extension leaves
    // value unbounded so scene magnitude is never silently clipped.
    let adjusted_saturation = (saturation * f64::from(adjustment.saturation_scale)).clamp(0.0, 1.0);
    let scaled_value = (value * f64::from(adjustment.value_scale)).max(0.0);
    let adjusted_value = if hdr {
        scaled_value
    } else {
        scaled_value.min(1.0)
    };
    SceneSafeHueSatResult {
        rgb: hsv_to_rgb(adjusted_hue, adjusted_saturation, adjusted_value),
        status: if hdr {
            SceneSafeHueSatStatus::AppliedAtHdrBoundary
        } else {
            SceneSafeHueSatStatus::AppliedSdr
        },
    }
}

fn rgb_to_hsv_nonnegative(rgb: [f64; 3]) -> (f64, f64, f64) {
    let maximum = rgb.into_iter().fold(f64::NEG_INFINITY, f64::max);
    let minimum = rgb.into_iter().fold(f64::INFINITY, f64::min);
    let delta = maximum - minimum;
    if delta == 0.0 || maximum == 0.0 {
        return (0.0, 0.0, maximum);
    }
    let hue_sector = if maximum == rgb[0] {
        ((rgb[1] - rgb[2]) / delta).rem_euclid(6.0)
    } else if maximum == rgb[1] {
        (rgb[2] - rgb[0]) / delta + 2.0
    } else {
        (rgb[0] - rgb[1]) / delta + 4.0
    };
    (hue_sector / 6.0, delta / maximum, maximum)
}

fn hsv_to_rgb(hue: f64, saturation: f64, value: f64) -> [f64; 3] {
    if saturation == 0.0 {
        return [value; 3];
    }
    let sector = hue.rem_euclid(1.0) * 6.0;
    let index = sector.floor() as i32;
    let fraction = sector - f64::from(index);
    let p = value * (1.0 - saturation);
    let q = value * (1.0 - saturation * fraction);
    let t = value * (1.0 - saturation * (1.0 - fraction));
    match index.rem_euclid(6) {
        0 => [value, t, p],
        1 => [q, value, p],
        2 => [p, value, t],
        3 => [p, q, value],
        4 => [t, p, value],
        _ => [value, p, q],
    }
}

fn validate_rgb_finite(rgb: [f64; 3]) -> Result<(), DcpTransformError> {
    for (channel, value) in rgb.into_iter().enumerate() {
        if !value.is_finite() {
            return Err(DcpTransformError::NonFiniteRgb { channel, value });
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::super::dcp::{DcpByteOrder, DcpCalibration, DcpCreativeMetadata, DcpEmbedPolicy};
    use super::*;

    const IDENTITY: Matrix3 = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];

    fn calibration(
        illuminant: u16,
        color_matrix: Matrix3,
        forward_matrix: Option<Matrix3>,
        hue_sat_map: Option<DcpHsvTable>,
    ) -> DcpCalibration {
        DcpCalibration {
            illuminant,
            color_matrix,
            forward_matrix,
            hue_sat_map,
        }
    }

    fn profile(calibrations: Vec<DcpCalibration>) -> DcpProfile {
        DcpProfile {
            byte_order: DcpByteOrder::LittleEndian,
            unique_camera_model: Some("Synthetic Camera".to_string()),
            profile_calibration_signature: None,
            profile_name: Some("Self-authored fixture".to_string()),
            calibrations,
            embed_policy: DcpEmbedPolicy::AllowCopying,
            copyright: None,
            creative: DcpCreativeMetadata::default(),
        }
    }

    fn adjustment(hue: f32, saturation: f32, value: f32) -> DcpHsvAdjustment {
        DcpHsvAdjustment {
            hue_shift_degrees: hue,
            saturation_scale: saturation,
            value_scale: value,
        }
    }

    fn identity_table(dimensions: DcpTableDimensions, encoding: DcpTableEncoding) -> DcpHsvTable {
        let count = usize::try_from(dimensions.hue_divisions).unwrap()
            * usize::try_from(dimensions.saturation_divisions).unwrap()
            * usize::try_from(dimensions.value_divisions).unwrap();
        DcpHsvTable {
            dimensions,
            encoding,
            entries: vec![adjustment(0.0, 1.0, 1.0); count],
        }
    }

    fn assert_close(actual: f64, expected: f64, tolerance: f64) {
        assert!(
            (actual - expected).abs() <= tolerance,
            "actual {actual:.15}, expected {expected:.15}, tolerance {tolerance}"
        );
    }

    fn assert_matrix_close(actual: &Matrix3, expected: &Matrix3, tolerance: f64) {
        for row in 0..3 {
            for column in 0..3 {
                assert_close(actual[row][column], expected[row][column], tolerance);
            }
        }
    }

    fn reset_hue_sat_validation_calls() {
        HUE_SAT_VALIDATION_CALLS.with(|calls| calls.set(0));
    }

    fn hue_sat_validation_calls() -> usize {
        HUE_SAT_VALIDATION_CALLS.with(std::cell::Cell::get)
    }

    #[test]
    fn supported_illuminants_are_explicit_and_vague_or_custom_values_fail_typed() {
        let expected = [
            (17, SupportedIlluminant::StandardA, 2_856.0),
            (20, SupportedIlluminant::D55, 5_503.0),
            (21, SupportedIlluminant::D65, 6_504.0),
            (22, SupportedIlluminant::D75, 7_504.0),
            (23, SupportedIlluminant::D50, 5_003.0),
            (24, SupportedIlluminant::IsoStudioTungsten, 3_200.0),
        ];
        for (code, illuminant, cct_kelvin) in expected {
            assert_eq!(
                illuminant_specification(code).unwrap(),
                IlluminantSpecification {
                    illuminant,
                    cct_kelvin,
                }
            );
        }
        assert_eq!(
            illuminant_specification(1),
            Err(DcpTransformError::UnsupportedIlluminant {
                code: 1,
                reason: UnsupportedIlluminantReason::Vague,
            })
        );
        assert_eq!(
            illuminant_specification(255),
            Err(DcpTransformError::UnsupportedIlluminant {
                code: 255,
                reason: UnsupportedIlluminantReason::Custom,
            })
        );
    }

    #[test]
    fn reciprocal_cct_interpolation_clamps_endpoints_and_hits_midpoint() {
        let first = [[1.0, 0.1, 0.0], [0.0, 2.0, 0.1], [0.0, 0.0, 3.0]];
        let second = [[3.0, 0.3, 0.0], [0.0, 4.0, 0.3], [0.0, 0.0, 5.0]];
        let fixture = profile(vec![
            calibration(17, first, None, None),
            calibration(21, second, None, None),
        ]);

        let cool = select_calibration_at_cct(&fixture, 50_000.0).unwrap();
        assert_eq!(cool.second_calibration_weight, 1.0);
        assert_eq!(cool.color_matrix, second);
        let warm = select_calibration_at_cct(&fixture, 1_000.0).unwrap();
        assert_eq!(warm.second_calibration_weight, 0.0);
        assert_eq!(warm.color_matrix, first);

        let midpoint_cct = (0.5 * (2_856.0f64.recip() + 6_504.0f64.recip())).recip();
        let midpoint = select_calibration_at_cct(&fixture, midpoint_cct).unwrap();
        assert_close(midpoint.second_calibration_weight, 0.5, 1.0e-12);
        assert_close(midpoint.color_matrix[0][0], 2.0, 1.0e-12);
        assert_close(midpoint.color_matrix[1][1], 3.0, 1.0e-12);
    }

    #[test]
    fn reversed_illuminants_select_the_same_matrix() {
        let warm = [[1.0, 0.1, 0.0], [0.0, 1.1, 0.0], [0.0, 0.0, 1.2]];
        let cool = [[1.5, 0.0, 0.1], [0.1, 1.4, 0.0], [0.0, 0.1, 1.3]];
        let forward = profile(vec![
            calibration(17, warm, None, None),
            calibration(21, cool, None, None),
        ]);
        let reversed = profile(vec![
            calibration(21, cool, None, None),
            calibration(17, warm, None, None),
        ]);
        let target = 4_500.0;
        let selected_forward = select_calibration_at_cct(&forward, target).unwrap();
        let selected_reversed = select_calibration_at_cct(&reversed, target).unwrap();
        assert_matrix_close(
            &selected_forward.color_matrix,
            &selected_reversed.color_matrix,
            1.0e-12,
        );
        assert_close(
            selected_forward.second_calibration_weight,
            1.0 - selected_reversed.second_calibration_weight,
            1.0e-12,
        );
    }

    #[test]
    fn color_matrices_are_interpolated_before_inversion() {
        let first = [[1.0, 0.4, 0.0], [0.0, 1.0, 0.2], [0.1, 0.0, 1.0]];
        let second = [[2.0, 0.0, 0.3], [0.2, 0.8, 0.0], [0.0, 0.5, 1.4]];
        let fixture = profile(vec![
            calibration(17, first, None, None),
            calibration(21, second, None, None),
        ]);
        let midpoint_cct = (0.5 * (2_856.0f64.recip() + 6_504.0f64.recip())).recip();
        let selected = select_calibration_at_cct(&fixture, midpoint_cct).unwrap();
        let actual = invert_matrix(&selected.color_matrix, "selected").unwrap();
        let wrong = interpolate_matrix(
            &invert_matrix(&first, "first").unwrap(),
            &invert_matrix(&second, "second").unwrap(),
            0.5,
        );
        let difference = actual
            .iter()
            .flatten()
            .zip(wrong.iter().flatten())
            .map(|(a, b)| (a - b).abs())
            .fold(0.0f64, f64::max);
        assert!(
            difference > 1.0e-2,
            "wrong operation order was not distinguishable"
        );
    }

    #[test]
    fn camera_neutral_is_normalized_reciprocal_gain() {
        let neutral = camera_neutral_from_wb_gains([2.0, 1.0, 1.25]).unwrap();
        assert_eq!(neutral, [0.5, 1.0, 0.8]);
        assert_eq!(
            camera_neutral_from_wb_gains([4.0, 2.0, 2.5]).unwrap(),
            neutral
        );
        assert!(matches!(
            camera_neutral_from_wb_gains([1.0, 0.0, 1.0]),
            Err(DcpTransformError::InvalidWhiteBalanceGain { channel: 1, .. })
        ));
    }

    #[test]
    fn camera_neutral_selection_reports_bounded_fixed_point_convergence() {
        let fixture = profile(vec![
            calibration(17, IDENTITY, None, None),
            calibration(21, IDENTITY, None, None),
        ]);
        // Synthetic D65 XYZ, scaled like a normalized CameraNeutral. With an
        // identity ColorMatrix the fixed point is the cool endpoint.
        let maximum = 1.088_83;
        let neutral = [0.950_47 / maximum, 1.0 / maximum, 1.0];
        let selected = select_calibration_for_camera_neutral(&fixture, neutral).unwrap();
        // McCamy's approximation is intentionally not asserted as an exact
        // nominal D65 thermometer; it should converge very near that endpoint.
        assert_close(selected.cct_kelvin, 6_504.0, 2.0);
        assert!(selected.second_calibration_weight > 0.999);
        assert!(matches!(
            selected.origin,
            DcpSelectionOrigin::CameraNeutralIteration {
                iterations: 1..=MAX_NEUTRAL_ITERATIONS,
                converged: true,
            }
        ));
    }

    #[test]
    fn forward_matrix_uses_inverse_camera_neutral_diagonal() {
        let forward = [[0.8, 0.1, 0.0], [0.2, 0.9, 0.1], [0.0, 0.1, 0.7]];
        let fixture = profile(vec![calibration(23, IDENTITY, Some(forward), None)]);
        let transform = build_camera_transform_at_cct(&fixture, [2.0, 1.0, 1.25], 5_003.0).unwrap();
        let expected = [
            [forward[0][0] * 2.0, forward[0][1], forward[0][2] * 1.25],
            [forward[1][0] * 2.0, forward[1][1], forward[1][2] * 1.25],
            [forward[2][0] * 2.0, forward[2][1], forward[2][2] * 1.25],
        ];
        assert_matrix_close(&transform.camera_to_xyz_d50, &expected, 1.0e-12);
    }

    #[test]
    fn no_forward_matrix_adapts_inferred_source_white_to_d50() {
        // CM maps the conventional D50 XYZ white to the synthetic neutral.
        let neutral = [0.5, 1.0, 0.8];
        let color_matrix = [
            [neutral[0] / D50_XYZ[0], 0.0, 0.0],
            [0.0, neutral[1] / D50_XYZ[1], 0.0],
            [0.0, 0.0, neutral[2] / D50_XYZ[2]],
        ];
        let fixture = profile(vec![calibration(23, color_matrix, None, None)]);
        let transform = build_camera_transform_at_cct(&fixture, [2.0, 1.0, 1.25], 5_003.0).unwrap();
        let mapped_white = apply_matrix(&transform.camera_to_xyz_d50, neutral);
        for channel in 0..3 {
            assert_close(mapped_white[channel], D50_XYZ[channel], 2.0e-7);
        }
    }

    #[test]
    fn post_wb_matrix_cancels_preapplied_gain_and_double_wb_does_not() {
        let profile_matrix = [[0.8, 0.2, 0.1], [0.1, 1.1, -0.1], [0.05, 0.3, 0.9]];
        let gains = [2.0, 1.0, 1.5];
        let effective = compensate_for_applied_white_balance(&profile_matrix, gains).unwrap();
        let camera_sample = [0.17, 0.42, 1.25];
        let white_balanced = [
            gains[0] * camera_sample[0],
            gains[1] * camera_sample[1],
            gains[2] * camera_sample[2],
        ];
        let expected = apply_matrix(&profile_matrix, camera_sample);
        let actual = apply_matrix(&effective, white_balanced);
        for channel in 0..3 {
            assert_close(actual[channel], expected[channel], 1.0e-12);
        }

        let deliberately_double_balanced = apply_matrix(&profile_matrix, white_balanced);
        assert!(
            deliberately_double_balanced
                .into_iter()
                .zip(expected)
                .any(|(a, b)| (a - b).abs() > 0.1),
            "fixture must detect the old double-WB operation order"
        );
    }

    #[test]
    fn dual_hue_sat_tables_use_the_matrix_selection_weight() {
        let dimensions = DcpTableDimensions {
            hue_divisions: 1,
            saturation_divisions: 2,
            value_divisions: 1,
        };
        let mut warm = identity_table(dimensions, DcpTableEncoding::Linear);
        warm.entries[1] = adjustment(-10.0, 0.8, 1.2);
        let mut cool = identity_table(dimensions, DcpTableEncoding::Linear);
        cool.entries[1] = adjustment(30.0, 1.2, 0.8);
        let fixture = profile(vec![
            calibration(17, IDENTITY, None, Some(warm)),
            calibration(21, IDENTITY, None, Some(cool)),
        ]);
        let midpoint_cct = (0.5 * (2_856.0f64.recip() + 6_504.0f64.recip())).recip();
        let selected = select_calibration_at_cct(&fixture, midpoint_cct).unwrap();
        let entry = selected.hue_sat_map.unwrap().entries[1];
        assert_close(f64::from(entry.hue_shift_degrees), 10.0, 1.0e-6);
        assert_close(f64::from(entry.saturation_scale), 1.0, 1.0e-6);
        assert_close(f64::from(entry.value_scale), 1.0, 1.0e-6);
    }

    #[test]
    fn prepared_hue_sat_map_matches_checked_convenience_apis() {
        let dimensions = DcpTableDimensions {
            hue_divisions: 2,
            saturation_divisions: 2,
            value_divisions: 2,
        };
        let mut table = identity_table(dimensions, DcpTableEncoding::Srgb);
        for value in 0..2 {
            for hue in 0..2 {
                let index = (value * 2 + hue) * 2 + 1;
                table.entries[index] = adjustment(
                    -15.0 + 20.0 * hue as f32,
                    0.8 + 0.3 * value as f32,
                    0.7 + 0.2 * hue as f32,
                );
            }
        }
        let prepared = PreparedHueSatMap::new(&table).unwrap();

        let hsv = [0.93, 0.72, 0.31];
        assert_eq!(prepared.sample_sdr(hsv), sample_hue_sat_map(&table, hsv));

        let sdr = [0.72, 0.21, 0.08];
        assert_eq!(prepared.apply_sdr(sdr), apply_hue_sat_map_sdr(&table, sdr));
        for rgb in [[-0.1, 0.3, 0.2], [0.72, 0.21, 0.08], [2.4, 0.8, 0.2]] {
            assert_eq!(
                prepared.apply_scene_safe(rgb),
                apply_hue_sat_map_scene_safe(&table, rgb)
            );
        }

        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<PreparedHueSatMap<'_>>();
    }

    #[test]
    fn prepared_hue_sat_map_rejects_malformed_tables_and_bad_pixel_input() {
        let mut malformed = identity_table(
            DcpTableDimensions {
                hue_divisions: 2,
                saturation_divisions: 2,
                value_divisions: 1,
            },
            DcpTableEncoding::Linear,
        );
        malformed.entries.pop();
        assert_eq!(
            PreparedHueSatMap::new(&malformed).unwrap_err(),
            DcpTransformError::HueSatEntryCount {
                expected: 4,
                actual: 3,
            }
        );
        assert_eq!(
            sample_hue_sat_map(&malformed, [0.0, 0.0, 0.0]),
            Err(DcpTransformError::HueSatEntryCount {
                expected: 4,
                actual: 3,
            })
        );

        let table = identity_table(
            DcpTableDimensions {
                hue_divisions: 1,
                saturation_divisions: 2,
                value_divisions: 1,
            },
            DcpTableEncoding::Linear,
        );
        let prepared = PreparedHueSatMap::new(&table).unwrap();
        assert!(matches!(
            prepared.apply_scene_safe([0.2, f64::NAN, 0.3]),
            Err(DcpTransformError::NonFiniteRgb { channel: 1, .. })
        ));
        assert_eq!(
            prepared.sample_sdr([0.0, 1.01, 0.5]),
            Err(DcpTransformError::HsvInputOutOfRange {
                channel: 1,
                value: 1.01,
            })
        );
    }

    #[test]
    fn prepared_pixel_loop_validates_the_lut_only_once() {
        let table = identity_table(
            DcpTableDimensions {
                hue_divisions: 4,
                saturation_divisions: 3,
                value_divisions: 2,
            },
            DcpTableEncoding::Srgb,
        );
        reset_hue_sat_validation_calls();
        let prepared = PreparedHueSatMap::new(&table).unwrap();
        assert_eq!(hue_sat_validation_calls(), 1);

        for index in 0..1_024 {
            let value = index as f64 / 511.0;
            let rgb = [value, value * 0.6, value * 0.2];
            prepared.apply_scene_safe(rgb).unwrap();
            prepared
                .sample_sdr([index as f64 / 1_024.0, 0.75, 0.5])
                .unwrap();
        }
        assert_eq!(
            hue_sat_validation_calls(),
            1,
            "prepared pixel evaluation rescanned the LUT"
        );

        // The checked one-shot API intentionally prepares/validates each call.
        apply_hue_sat_map_scene_safe(&table, [0.4, 0.2, 0.1]).unwrap();
        assert_eq!(hue_sat_validation_calls(), 2);
    }

    #[test]
    fn trilinear_sampling_wraps_hue_from_359_degrees_to_zero() {
        let dimensions = DcpTableDimensions {
            hue_divisions: 4,
            saturation_divisions: 2,
            value_divisions: 1,
        };
        let mut table = identity_table(dimensions, DcpTableEncoding::Linear);
        // Saturated entry for hue bin 0 is 0 degrees; bin 3 is 90 degrees.
        table.entries[1].hue_shift_degrees = 0.0;
        table.entries[7].hue_shift_degrees = 90.0;
        let at_zero = sample_hue_sat_map(&table, [0.0, 1.0, 0.5]).unwrap();
        let at_359 = sample_hue_sat_map(&table, [359.0 / 360.0, 1.0, 0.5]).unwrap();
        assert_close(f64::from(at_zero.hue_shift_degrees), 0.0, 1.0e-6);
        assert!(
            f64::from(at_359.hue_shift_degrees) < 2.0,
            "cyclic interpolation should approach bin zero, got {:?}",
            at_359
        );
    }

    #[test]
    fn unit_dimensions_and_srgb_value_encoding_are_supported() {
        let one = identity_table(
            DcpTableDimensions {
                hue_divisions: 1,
                saturation_divisions: 2,
                value_divisions: 1,
            },
            DcpTableEncoding::Linear,
        );
        assert_eq!(
            sample_hue_sat_map(&one, [0.77, 0.91, 0.37]).unwrap(),
            adjustment(0.0, 1.0, 1.0)
        );

        let invalid_saturation_axis = identity_table(
            DcpTableDimensions {
                hue_divisions: 1,
                saturation_divisions: 1,
                value_divisions: 1,
            },
            DcpTableEncoding::Linear,
        );
        assert_eq!(
            sample_hue_sat_map(&invalid_saturation_axis, [0.0, 0.0, 0.0]),
            Err(DcpTransformError::InvalidHueSatDimensions)
        );

        let dimensions = DcpTableDimensions {
            hue_divisions: 1,
            saturation_divisions: 2,
            value_divisions: 2,
        };
        let mut linear = identity_table(dimensions, DcpTableEncoding::Linear);
        linear.entries[3].saturation_scale = 2.0;
        let mut srgb = linear.clone();
        srgb.encoding = DcpTableEncoding::Srgb;
        let linear_sample = sample_hue_sat_map(&linear, [0.0, 1.0, 0.25]).unwrap();
        let srgb_sample = sample_hue_sat_map(&srgb, [0.0, 1.0, 0.25]).unwrap();
        assert_close(f64::from(linear_sample.saturation_scale), 1.25, 1.0e-6);
        assert!(
            f64::from(srgb_sample.saturation_scale) > f64::from(linear_sample.saturation_scale)
        );
    }

    #[test]
    fn zero_saturation_is_neutral_and_value_scale_rule_is_enforced() {
        let dimensions = DcpTableDimensions {
            hue_divisions: 2,
            saturation_divisions: 2,
            value_divisions: 2,
        };
        let mut table = identity_table(dimensions, DcpTableEncoding::Linear);
        for value in 0..2 {
            for hue in 0..2 {
                let index = (value * 2 + hue) * 2 + 1;
                table.entries[index] = adjustment(120.0, 1.7, 0.6);
            }
        }
        assert_eq!(apply_hue_sat_map_sdr(&table, [0.4; 3]).unwrap(), [0.4; 3]);

        table.entries[0].value_scale = 0.9;
        assert_eq!(
            sample_hue_sat_map(&table, [0.0, 0.0, 0.0]),
            Err(DcpTransformError::InvalidZeroSaturationEntry { index: 0 })
        );
    }

    #[test]
    fn sdr_clamps_scaled_saturation_and_value_while_hdr_keeps_value_range() {
        let dimensions = DcpTableDimensions {
            hue_divisions: 1,
            saturation_divisions: 2,
            value_divisions: 1,
        };
        let mut table = identity_table(dimensions, DcpTableEncoding::Linear);
        table.entries[1] = adjustment(0.0, 3.0, 2.0);

        let sdr = [0.8, 0.0, 0.0];
        assert_eq!(apply_hue_sat_map_sdr(&table, sdr).unwrap(), [1.0, 0.0, 0.0]);
        assert_eq!(
            apply_hue_sat_map_scene_safe(&table, sdr).unwrap(),
            SceneSafeHueSatResult {
                rgb: [1.0, 0.0, 0.0],
                status: SceneSafeHueSatStatus::AppliedSdr,
            }
        );

        let hdr = apply_hue_sat_map_scene_safe(&table, [2.0, 1.0, 1.0]).unwrap();
        assert_eq!(hdr.status, SceneSafeHueSatStatus::AppliedAtHdrBoundary);
        assert_eq!(hdr.rgb, [3.0, 0.0, 0.0]);
    }

    #[test]
    fn scene_safe_policy_preserves_signed_and_hdr_values_without_clipping() {
        let table = identity_table(
            DcpTableDimensions {
                hue_divisions: 2,
                saturation_divisions: 2,
                value_divisions: 2,
            },
            DcpTableEncoding::Srgb,
        );
        let signed = [-0.1, 0.2, 0.3];
        assert_eq!(
            apply_hue_sat_map_scene_safe(&table, signed).unwrap(),
            SceneSafeHueSatResult {
                rgb: signed,
                status: SceneSafeHueSatStatus::BypassedSigned,
            }
        );

        let hdr = [2.5, 1.25, 0.625];
        let result = apply_hue_sat_map_scene_safe(&table, hdr).unwrap();
        assert_eq!(result.status, SceneSafeHueSatStatus::AppliedAtHdrBoundary);
        for channel in 0..3 {
            assert_close(result.rgb[channel], hdr[channel], 1.0e-12);
        }
        assert!(matches!(
            apply_hue_sat_map_sdr(&table, hdr),
            Err(DcpTransformError::HsvInputOutOfRange { .. })
        ));
    }

    #[test]
    fn malformed_dual_luts_fail_instead_of_cross_indexing() {
        let first = identity_table(
            DcpTableDimensions {
                hue_divisions: 1,
                saturation_divisions: 2,
                value_divisions: 1,
            },
            DcpTableEncoding::Linear,
        );
        let second = identity_table(
            DcpTableDimensions {
                hue_divisions: 2,
                saturation_divisions: 2,
                value_divisions: 1,
            },
            DcpTableEncoding::Linear,
        );
        let fixture = profile(vec![
            calibration(17, IDENTITY, None, Some(first)),
            calibration(21, IDENTITY, None, Some(second)),
        ]);
        assert_eq!(
            select_calibration_at_cct(&fixture, 4_500.0),
            Err(DcpTransformError::MismatchedHueSatDimensions)
        );

        let orphan = profile(vec![
            calibration(17, IDENTITY, None, None),
            calibration(
                21,
                IDENTITY,
                None,
                Some(identity_table(
                    DcpTableDimensions {
                        hue_divisions: 1,
                        saturation_divisions: 2,
                        value_divisions: 1,
                    },
                    DcpTableEncoding::Linear,
                )),
            ),
        ]);
        assert_eq!(
            select_calibration_at_cct(&orphan, 4_500.0),
            Err(DcpTransformError::HueSatMapOnlyOnSecondCalibration)
        );
    }

    #[test]
    fn creative_metadata_never_enters_the_technical_transform() {
        let mut fixture = profile(vec![calibration(23, IDENTITY, None, None)]);
        fixture.creative.tone_curve = Some(vec![[0.0, 0.0], [1.0, 0.5]]);
        fixture.creative.look_table = Some(identity_table(
            DcpTableDimensions {
                hue_divisions: 1,
                saturation_divisions: 2,
                value_divisions: 1,
            },
            DcpTableEncoding::Linear,
        ));
        let transform = build_camera_transform_at_cct(&fixture, [1.0; 3], 5_003.0).unwrap();
        assert!(transform.hue_sat_map.is_none());
    }

    #[test]
    fn finite_and_invertibility_checks_cover_constructed_profiles() {
        let singular = [[1.0, 0.0, 0.0], [2.0, 0.0, 0.0], [0.0, 0.0, 1.0]];
        let fixture = profile(vec![calibration(23, singular, None, None)]);
        assert!(matches!(
            select_calibration_at_cct(&fixture, 5_003.0),
            Err(DcpTransformError::SingularMatrix {
                role: "ColorMatrix1"
            })
        ));

        let mut non_finite = IDENTITY;
        non_finite[1][2] = f64::NAN;
        let fixture = profile(vec![calibration(23, non_finite, None, None)]);
        assert!(matches!(
            select_calibration_at_cct(&fixture, 5_003.0),
            Err(DcpTransformError::NonFiniteMatrix {
                role: "ColorMatrix1",
                row: 1,
                column: 2,
            })
        ));
    }
}
