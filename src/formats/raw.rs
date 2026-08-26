//! RAW importer — decode a camera RAW file to a scene-referred Develop master
//! plus a 16-bit sRGB document.
//!
//! Pipeline: rawloader decodes the sensor mosaic + metadata; we black/white-level
//! normalize, apply the as-shot white balance, demosaic, recover clipped
//! highlights, and convert the camera colour space to linear sRGB via the
//! embedded camera→XYZ matrix. That linear result is kept UNCLAMPED as an f16
//! [`SceneSource`] (highlight headroom above 1.0 and out-of-gamut values below
//! 0.0 survive) and attached to the Canvas for the scene-referred Develop
//! session. The visible document tiles are the *default look* — the neutral
//! sigmoid render from `develop_scene` — so the image on screen equals a
//! Develop session at neutral settings, non-destructively.

use super::Importer;
use crate::core::camera_profile::dcp_transform::PreparedHueSatMap;
use crate::core::camera_profile::resolver::{
    self, CameraIdentityRef, DecoderFallback, EmbeddedDcpCandidate, ProfileBlob, ResolveRequest,
    ResolvedCameraCharacterization,
};
use crate::core::camera_profile::{
    discovery, embedded_dng, resolve_decoder_matrix, JpegMatchMode, RawDecoderBackend,
    RawRenderRecipeVersion, RawSceneCharacterization,
};
use crate::core::canvas::Canvas;
use crate::core::develop_scene::{
    f16_bits_to_f32, f32_to_f16_bits, render_default_look, SceneSource,
};
use rawloader::{Orientation, RawImage, RawImageData};
use rayon::prelude::*;
use std::path::Path;

/// File extensions handled by rawloader's bundled decoders. Anything outside this
/// set falls through to the generic image importers.
const RAW_EXTS: &[&str] = &[
    "cr2", "crw", "cr3", // Canon (cr3 via the rawler fallback)
    "nef", "nrw", // Nikon
    "arw", "sr2", "srf", // Sony
    "raf", // Fuji
    "orf", // Olympus
    "rw2", // Panasonic
    "pef", // Pentax
    "srw", // Samsung
    "dng", // Adobe / generic
    "dcr", "dcs", "kdc", // Kodak
    "mrw", // Minolta
    "erf", // Epson
    "mef", // Mamiya
    "mos", // Leaf
    "iiq", // Phase One
    "3fr", // Hasselblad
    "ari", // Arri
    "x3f", // Sigma
];

pub struct RawImporter;

/// Whether `path` has a camera-RAW extension this importer handles. Used by the
/// open flow to route RAW files into the Develop stage.
pub fn is_raw_path(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_lowercase())
        .map(|e| RAW_EXTS.contains(&e.as_str()))
        .unwrap_or(false)
}

impl Importer for RawImporter {
    fn extensions(&self) -> &[&str] {
        RAW_EXTS
    }

    fn import(&self, path: &Path) -> Result<Canvas, String> {
        decode_raw(path)
    }
}

// The old display baseline (luma S-curve + gamut fit baked at decode) is gone:
// the default look now comes from the scene-referred sigmoid render in
// `develop_scene::render_default_look`, applied to the unclamped linear master —
// non-destructive, and re-rendered live by the Develop session.

#[derive(Clone, Copy)]
struct ActiveArea {
    top: usize,
    left: usize,
    width: usize,
    height: usize,
}

#[derive(Clone, Copy, Debug)]
struct RawLevels {
    black: [f32; 4],
    observed_white: [f32; 4],
    effective_white: [f32; 4],
    denom: [f32; 4],
}

fn active_area(raw: &RawImage) -> Result<ActiveArea, String> {
    let (w, h) = (raw.width, raw.height);
    if w == 0 || h == 0 {
        return Err("RAW có kích thước bằng 0".into());
    }

    // Active (cropped) area. crops order is [top, right, bottom, left].
    let width = w.saturating_sub(raw.crops[3]).saturating_sub(raw.crops[1]);
    let height = h.saturating_sub(raw.crops[0]).saturating_sub(raw.crops[2]);
    if width == 0 || height == 0 {
        return Err("RAW không có vùng ảnh hợp lệ".into());
    }
    Ok(ActiveArea {
        top: raw.crops[0],
        left: raw.crops[3],
        width,
        height,
    })
}

fn white_balance_gains(wbc: [f32; 4], mono: bool) -> [f32; 4] {
    if mono {
        return [1.0; 4];
    }
    let gref = if wbc[1] > 0.0 { wbc[1] } else { 1.0 };
    let mut gain = [1.0f32; 4];
    for c in 0..4 {
        gain[c] = if wbc[c] > 0.0 { wbc[c] / gref } else { 1.0 };
    }
    gain
}

/// Recover an absolute as-shot source white from the camera neutral implied by
/// decoder WB multipliers and the decoder camera→XYZ matrix. This is metadata
/// only: the gains are already baked into the scene master exactly as before.
fn as_shot_white_balance(
    cam2xyz: &[[f32; 4]; 3],
    gains: [f32; 4],
    mono: bool,
) -> Option<crate::core::cat16::WhiteBalance> {
    if mono
        || gains[..3]
            .iter()
            .any(|gain| !gain.is_finite() || *gain <= 0.0)
    {
        return None;
    }
    let neutral = [1.0 / gains[0], 1.0 / gains[1], 1.0 / gains[2]];
    let xyz = [
        cam2xyz[0][0] * neutral[0] + cam2xyz[0][1] * neutral[1] + cam2xyz[0][2] * neutral[2],
        cam2xyz[1][0] * neutral[0] + cam2xyz[1][1] * neutral[1] + cam2xyz[1][2] * neutral[2],
        cam2xyz[2][0] * neutral[0] + cam2xyz[2][1] * neutral[1] + cam2xyz[2][2] * neutral[2],
    ];
    crate::core::cat16::white_balance_from_xyz(xyz)
}

#[inline]
fn raw_value(data: &RawImageData, idx: usize) -> f32 {
    match data {
        RawImageData::Integer(v) => v.get(idx).copied().unwrap_or(0) as f32,
        RawImageData::Float(v) => v.get(idx).copied().unwrap_or(0.0),
    }
}

fn observed_channel_maxima(raw: &RawImage, area: ActiveArea) -> [f32; 4] {
    let mut maxv = [0.0f32; 4];
    let mut global = 0.0f32;
    match raw.cpp {
        1 => {
            let mono = !raw.cfa.is_valid();
            for r in area.top..area.top + area.height {
                for c in area.left..area.left + area.width {
                    let v = raw_value(&raw.data, r * raw.width + c);
                    global = global.max(v);
                    let ch = if mono {
                        0
                    } else {
                        raw.cfa.color_at(r, c).min(3)
                    };
                    maxv[ch] = maxv[ch].max(v);
                }
            }
        }
        3 => {
            for r in area.top..area.top + area.height {
                for c in area.left..area.left + area.width {
                    let src = (r * raw.width + c) * 3;
                    for ch in 0..3 {
                        let v = raw_value(&raw.data, src + ch);
                        global = global.max(v);
                        maxv[ch] = maxv[ch].max(v);
                    }
                }
            }
            maxv[3] = maxv[1];
        }
        _ => {
            for i in 0..raw.width.saturating_mul(raw.height).saturating_mul(raw.cpp) {
                global = global.max(raw_value(&raw.data, i));
            }
            maxv = [global; 4];
        }
    }
    for v in &mut maxv {
        if *v <= 0.0 {
            *v = global;
        }
    }
    maxv
}

fn choose_effective_white_level(reported: f32, black: f32, observed: f32) -> f32 {
    let observed = observed.max(black + 1.0);
    if !reported.is_finite() || reported <= black + 1.0 {
        return observed;
    }

    // Some decoders/cameras report a 16-bit container maximum (65535) for 12/14-bit
    // sensor data. Trusting that value under-normalizes the RAW by two or more
    // stops. Only fall back when the metadata clearly looks like a container max,
    // so genuinely underexposed 16-bit files do not get auto-brightened.
    let observed_span = observed - black;
    let reported_span = reported - black;
    if reported >= 60_000.0 && observed_span <= 20_000.0 && reported_span > observed_span * 2.5 {
        observed
    } else {
        reported
    }
}

fn raw_levels(raw: &RawImage, area: ActiveArea) -> RawLevels {
    let observed_white = observed_channel_maxima(raw, area);
    let mut black = [0.0f32; 4];
    let mut effective_white = [1.0f32; 4];
    let mut denom = [1.0f32; 4];
    for c in 0..4 {
        black[c] = raw.blacklevels[c] as f32;
        effective_white[c] =
            choose_effective_white_level(raw.whitelevels[c] as f32, black[c], observed_white[c]);
        denom[c] = (effective_white[c] - black[c]).max(1.0);
    }
    RawLevels {
        black,
        observed_white,
        effective_white,
        denom,
    }
}

#[inline]
fn luma_lin(c: [f32; 3]) -> f32 {
    0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2]
}

#[inline]
fn camera_to_linear_srgb(m: &[[f32; 3]; 3], cam: [f32; 3]) -> [f32; 3] {
    [
        m[0][0] * cam[0] + m[0][1] * cam[1] + m[0][2] * cam[2],
        m[1][0] * cam[0] + m[1][1] * cam[1] + m[1][2] * cam[2],
        m[2][0] * cam[0] + m[2][1] * cam[1] + m[2][2] * cam[2],
    ]
}

/// Decoded sensor data plus the front-end that supplied its compatibility
/// matrix. The old adapter erased this distinction before profile resolution.
struct DecodedRaw {
    image: RawImage,
    backend: RawDecoderBackend,
}

/// Select the RAW front-end and decode into the shared `DecodedRaw` model.
/// Primary decoder: rawloader. Fallback: rawler, which adds Canon CR3 and a much
/// larger camera database. Files rawloader already decodes never reach the
/// fallback, so their output is byte-for-byte unchanged.
fn decode_front_end(path: &Path) -> Result<DecodedRaw, String> {
    match rawloader::decode_file(path) {
        Ok(image) => Ok(DecodedRaw {
            image,
            backend: RawDecoderBackend::Rawloader,
        }),
        Err(primary) => {
            decode_via_rawler(path).map_err(|fallback| format!("rawloader: {primary}; {fallback}"))
        }
    }
}

fn decode_raw(path: &Path) -> Result<Canvas, String> {
    decode_raw_from(decode_front_end(path)?, path)
}

/// Where a channel's effective white level came from. The Q1 sensor-preprocessing
/// audit must record when reported metadata was NOT trusted: silently swapping in
/// the observed sensor maximum brightens a genuinely underexposed frame, so the
/// substitution has to remain visible in provenance rather than be assumed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WhiteLevelSource {
    /// Reported white level trusted and used as-is.
    Reported,
    /// Reported ≤ black (missing/degenerate); observed sensor maximum used.
    MissingReplacedByObserved,
    /// Reported looked like a 16-bit container maximum; observed maximum used.
    ContainerMaxReplacedByObserved,
}

/// Provenance the shared decoder boundary can guarantee for black levels. When
/// masked areas are present we record that fact without claiming whether a
/// decoder preferred fixed camera constants or measured those samples.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BlackLevelSource {
    DecoderSupplied,
    DecoderSuppliedWithMaskedAreas,
}

/// Whether an optional sensor fact is present in the shared rawloader/rawler
/// model used by the renderer. `NotExposedBySharedModel` is deliberately
/// different from `ReportedAbsent`: the former means iAi cannot tell whether
/// the file contains the fact and must not infer a value for a correction.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SensorMetadataAvailability {
    Reported,
    ReportedAbsent,
    NotExposedBySharedModel,
}

/// Why a sensor correction stage is enabled or disabled for this decode.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SensorCorrectionReason {
    /// Existing isolated-site correction on an ordinary Bayer mosaic.
    BayerDefectBaseline,
    /// The source is mono, already demosaiced, or has no valid Bayer CFA.
    NotBayerMosaic,
    /// The shared decoder model exposes no metadata/diagnostic that would make
    /// applying the correction safer than leaving clean image detail alone.
    MissingMetadataOrDiagnostic,
}

/// One Q1 sensor correction stage and its bounded scratch estimate.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SensorCorrectionStage {
    pub enabled: bool,
    pub reason: SensorCorrectionReason,
    /// Conservative upper bound for temporary bytes owned by this stage. The
    /// defect stage normally allocates far less because it records only sites
    /// that pass both outlier checks.
    pub estimated_scratch_bytes: usize,
}

/// Decode-specific Q1 plan. Keeping disabled stages explicit prevents a future
/// implementation from silently applying green blur or inventing PDAF data.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SensorCorrectionPlan {
    pub isolated_bayer_defects: SensorCorrectionStage,
    pub green_equilibration: SensorCorrectionStage,
}

fn sensor_correction_plan(
    width: usize,
    height: usize,
    cpp: usize,
    cfa_valid: bool,
    is_mono: bool,
) -> SensorCorrectionPlan {
    let bayer = cpp == 1 && cfa_valid && !is_mono;
    let defect_candidates = width
        .saturating_sub(8)
        .saturating_mul(height.saturating_sub(8));
    SensorCorrectionPlan {
        isolated_bayer_defects: SensorCorrectionStage {
            enabled: bayer,
            reason: if bayer {
                SensorCorrectionReason::BayerDefectBaseline
            } else {
                SensorCorrectionReason::NotBayerMosaic
            },
            estimated_scratch_bytes: if bayer {
                defect_candidates.saturating_mul(std::mem::size_of::<(usize, f32)>())
            } else {
                0
            },
        },
        // Q1 audit found no green-split diagnostic, ISO model, gain map, or
        // camera correction metadata. Stay bit-exact until one exists and is
        // backed by clean/no-op plus affected-sensor crop tests.
        green_equilibration: SensorCorrectionStage {
            enabled: false,
            reason: SensorCorrectionReason::MissingMetadataOrDiagnostic,
            estimated_scratch_bytes: 0,
        },
    }
}

/// Classify one channel's white-level decision, mirroring
/// [`choose_effective_white_level`] branch-for-branch so the audit's provenance
/// stays exactly in step with the value the decode actually normalizes by. A
/// unit test pins the two together.
fn white_level_source(reported: f32, black: f32, observed: f32) -> WhiteLevelSource {
    let observed = observed.max(black + 1.0);
    if !reported.is_finite() || reported <= black + 1.0 {
        return WhiteLevelSource::MissingReplacedByObserved;
    }
    let observed_span = observed - black;
    let reported_span = reported - black;
    if reported >= 60_000.0 && observed_span <= 20_000.0 && reported_span > observed_span * 2.5 {
        WhiteLevelSource::ContainerMaxReplacedByObserved
    } else {
        WhiteLevelSource::Reported
    }
}

/// Read-only sensor metadata a RAW decode exposes, with provenance — the Q1
/// "normalized RAW master" foundation. Downstream preprocessing stages (optical
/// black, defect/PDAF correction, green equilibration, lens shading) read these
/// facts; recording exactly what the decoder returns keeps later work from
/// assuming fields the corpus does not actually carry.
#[derive(Clone, Debug)]
pub struct RawSensorMetadata {
    pub backend: RawDecoderBackend,
    pub make: String,
    pub model: String,
    pub width: usize,
    pub height: usize,
    /// Samples per pixel: 1 = Bayer mosaic or monochrome, 3 = linear/demosaiced.
    pub cpp: usize,
    pub cfa_name: String,
    pub cfa_valid: bool,
    pub is_mono: bool,
    /// Active image rectangle after the decoder crop: [top, left, width, height].
    pub active_area: [usize; 4],
    /// rawloader crop margins [top, right, bottom, left].
    pub crop_margins: [usize; 4],
    pub black_levels: [f32; 4],
    pub black_level_source: BlackLevelSource,
    pub reported_white_levels: [f32; 4],
    pub observed_white_maxima: [f32; 4],
    pub effective_white_levels: [f32; 4],
    pub white_level_source: [WhiteLevelSource; 4],
    /// As-shot white-balance multipliers from the decoder (0 where absent).
    pub wb_coeffs: [f32; 4],
    pub wb_availability: SensorMetadataAvailability,
    /// Count of masked optical-black regions the decoder exposed (0 = none).
    pub black_area_count: usize,
    pub optical_black_availability: SensorMetadataAvailability,
    /// These facts are not present in the shared decoder boundary today. The
    /// explicit provenance prevents downstream stages from guessing them.
    pub gain_map_availability: SensorMetadataAvailability,
    pub pdaf_mask_availability: SensorMetadataAvailability,
    pub iso_availability: SensorMetadataAvailability,
    pub lens_data_availability: SensorMetadataAvailability,
    pub correction_plan: SensorCorrectionPlan,
    pub orientation: String,
}

/// Decode `path` far enough to report [`RawSensorMetadata`] — it reads the mosaic
/// and derives the level/area facts but skips demosaic and the full render, so it
/// is cheap enough to audit a whole corpus. It changes no rendered pixel.
pub fn probe_sensor_metadata(path: &Path) -> Result<RawSensorMetadata, String> {
    let decoded = decode_front_end(path)?;
    let raw = &decoded.image;
    let area = active_area(raw)?;
    let levels = raw_levels(raw, area);
    let is_mono = raw.cpp == 1 && !raw.cfa.is_valid();
    let black_area_count = raw.blackareas.len();
    let mut source = [WhiteLevelSource::Reported; 4];
    for c in 0..4 {
        source[c] = white_level_source(
            raw.whitelevels[c] as f32,
            raw.blacklevels[c] as f32,
            levels.observed_white[c],
        );
    }
    Ok(RawSensorMetadata {
        backend: decoded.backend,
        make: raw.clean_make.trim().to_string(),
        model: raw.clean_model.trim().to_string(),
        width: raw.width,
        height: raw.height,
        cpp: raw.cpp,
        cfa_name: raw.cfa.name.clone(),
        cfa_valid: raw.cfa.is_valid(),
        is_mono,
        active_area: [area.top, area.left, area.width, area.height],
        crop_margins: raw.crops,
        black_levels: levels.black,
        black_level_source: if black_area_count > 0 {
            BlackLevelSource::DecoderSuppliedWithMaskedAreas
        } else {
            BlackLevelSource::DecoderSupplied
        },
        reported_white_levels: raw.whitelevels.map(|value| value as f32),
        observed_white_maxima: levels.observed_white,
        effective_white_levels: levels.effective_white,
        white_level_source: source,
        wb_coeffs: raw.wb_coeffs,
        wb_availability: if raw.wb_coeffs[..3].iter().all(|gain| *gain > 0.0) {
            SensorMetadataAvailability::Reported
        } else {
            SensorMetadataAvailability::ReportedAbsent
        },
        black_area_count,
        optical_black_availability: if black_area_count > 0 {
            SensorMetadataAvailability::Reported
        } else {
            SensorMetadataAvailability::ReportedAbsent
        },
        gain_map_availability: SensorMetadataAvailability::NotExposedBySharedModel,
        pdaf_mask_availability: SensorMetadataAvailability::NotExposedBySharedModel,
        iso_availability: SensorMetadataAvailability::NotExposedBySharedModel,
        lens_data_availability: SensorMetadataAvailability::NotExposedBySharedModel,
        correction_plan: sensor_correction_plan(
            raw.width,
            raw.height,
            raw.cpp,
            raw.cfa.is_valid(),
            is_mono,
        ),
        orientation: format!("{:?}", raw.orientation),
    })
}

/// Decode a RAW with rawler and adapt it into a `rawloader::RawImage`, so the
/// single shared decode body below handles both front-ends. rawler's public
/// data model maps onto rawloader's one-to-one: same mosaic enum shape, a
/// `[[f32; 3]; 4]` `xyz_to_cam`, an EXIF-numbered orientation, and a CFA whose
/// `name` is the pattern string rawloader's `CFA::new` expects.
fn decode_via_rawler(path: &Path) -> Result<DecodedRaw, String> {
    let img = rawler::decode_file(path).map_err(|e| format!("rawler: {e:?}"))?;

    // rawler's camera database stores characterization in `color_matrix`;
    // `xyz_to_cam` is deprecated and is commonly left as an all-zero default.
    // Select a matrix deterministically before adapting into rawloader's data
    // model, otherwise supported rawler-only cameras can render black.
    let mono = matches!(
        img.photometric,
        rawler::rawimage::RawPhotometricInterpretation::BlackIsZero
    );
    let xyz_to_cam = select_rawler_xyz_to_cam(&img.color_matrix, img.xyz_to_cam, mono)?;

    let to_u16_4 = |a: [f32; 4]| a.map(|v| v.round().clamp(0.0, u16::MAX as f32) as u16);
    let whitelevels = to_u16_4(img.whitelevel.as_bayer_array());
    let blacklevels = to_u16_4(img.blacklevel.as_bayer_array());
    // Preserve rawler's masked optical-black rectangles across the compatibility
    // adapter. The pixel path already uses rawler's resolved black levels; this
    // only keeps the provenance/audit data that the old adapter discarded.
    let blackareas = img
        .blackareas
        .iter()
        .map(|rect| {
            (
                rect.p.y as u64,
                rect.p.x.saturating_add(rect.d.w) as u64,
                rect.p.y.saturating_add(rect.d.h) as u64,
                rect.p.x as u64,
            )
        })
        .collect();

    // rawloader crops are margins [top, right, bottom, left]. Prefer the default
    // crop, then the active area, else the whole frame.
    let crops = img
        .crop_area
        .or(img.active_area)
        .map(|r| {
            let [top, left, bottom, right] = r.as_tlbr_offsets(img.width, img.height);
            [top, right, bottom, left]
        })
        .unwrap_or([0, 0, 0, 0]);

    let cfa = rawloader::CFA::new(&img.camera.cfa.name);
    let orientation = rawloader::Orientation::from_u16(img.orientation.to_u16());
    let data = match img.data {
        rawler::rawimage::RawImageData::Integer(v) => RawImageData::Integer(v),
        rawler::rawimage::RawImageData::Float(v) => RawImageData::Float(v),
    };

    let image = RawImage {
        make: img.make,
        model: img.model,
        clean_make: img.clean_make,
        clean_model: img.clean_model,
        width: img.width,
        height: img.height,
        cpp: img.cpp,
        wb_coeffs: img.wb_coeffs,
        whitelevels,
        blacklevels,
        xyz_to_cam,
        cfa,
        crops,
        orientation,
        data,
        blackareas,
    };
    Ok(DecodedRaw {
        image,
        backend: RawDecoderBackend::Rawler,
    })
}

fn select_rawler_xyz_to_cam(
    color_matrices: &std::collections::HashMap<
        rawler::imgop::xyz::Illuminant,
        rawler::imgop::xyz::FlatColorMatrix,
    >,
    legacy: [[f32; 3]; 4],
    mono: bool,
) -> Result<[[f32; 3]; 4], String> {
    use rawler::imgop::xyz::Illuminant;

    fn usable_flat(matrix: &[f32]) -> bool {
        matches!(matrix.len(), 9 | 12)
            && matrix.iter().all(|value| value.is_finite())
            && matrix
                .chunks_exact(3)
                .all(|row| row.iter().map(|value| value.abs()).sum::<f32>() > 1.0e-8)
    }

    fn usable_legacy(matrix: &[[f32; 3]; 4]) -> bool {
        matrix.iter().flatten().all(|value| value.is_finite())
            && matrix[..3]
                .iter()
                .all(|row| row.iter().map(|value| value.abs()).sum::<f32>() > 1.0e-8)
    }

    fn rank(illuminant: Illuminant) -> (u8, u16) {
        let priority = match illuminant {
            Illuminant::D65 => 0,
            Illuminant::D55 => 1,
            Illuminant::D50 => 2,
            Illuminant::Daylight => 3,
            Illuminant::D75 => 4,
            Illuminant::A => 5,
            _ => 6,
        };
        (priority, u16::from(illuminant))
    }

    if let Some((_, matrix)) = color_matrices
        .iter()
        .filter(|(_, matrix)| usable_flat(matrix))
        .min_by_key(|(illuminant, _)| rank(**illuminant))
    {
        let mut selected = [[0.0f32; 3]; 4];
        for (row, values) in matrix.chunks_exact(3).enumerate() {
            selected[row].copy_from_slice(values);
        }
        return Ok(selected);
    }

    if usable_legacy(&legacy) {
        return Ok(legacy);
    }

    if mono {
        let mut identity = [[0.0f32; 3]; 4];
        for channel in 0..3 {
            identity[channel][channel] = 1.0;
        }
        return Ok(identity);
    }

    Err("rawler: camera has no usable deterministic XYZ-to-camera matrix".into())
}

/// Independently gated embedded-JPEG matching stages. Keeping matrix and curves
/// separate is important: either can be A/B-tested without changing sensor
/// normalization, demosaic, camera WB, or the decoder characterization matrix.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct JpegMatchPlan {
    apply_gain: bool,
    apply_matrix: bool,
    apply_curves: bool,
    preview_safe: bool,
}
///
/// A profile-backed DCP render is already colour-accurate, so it defaults to no
/// embedded-JPEG fit (`none`). The decoder-matrix fallback keeps the shipping
/// brightness+colour match. The `IAI_RAW_JPEG_MATCH` override still wins:
/// `on`/`full` force both colour stages; `matrix` and `curves` isolate one;
/// `off` keeps only the brightness baseline; `none` drops all matching. Camera
/// characterization work continues to replace this heuristic (master plan
/// §14/§33).
fn jpeg_match_plan_for(value: Option<&str>, dcp_selected: bool) -> JpegMatchPlan {
    match value.map(str::trim) {
        Some("on") | Some("full") => JpegMatchPlan {
            apply_gain: true,
            apply_matrix: true,
            apply_curves: true,
            preview_safe: false,
        },
        Some("safe") | Some("preview-safe") => JpegMatchPlan {
            apply_gain: true,
            apply_matrix: true,
            apply_curves: false,
            preview_safe: true,
        },
        Some("matrix") => JpegMatchPlan {
            apply_gain: true,
            apply_matrix: true,
            apply_curves: false,
            preview_safe: false,
        },
        Some("curves") | Some("curve") => JpegMatchPlan {
            apply_gain: true,
            apply_matrix: false,
            apply_curves: true,
            preview_safe: false,
        },
        Some("off") => JpegMatchPlan {
            apply_gain: true,
            apply_matrix: false,
            apply_curves: false,
            preview_safe: false,
        },
        Some("none") => JpegMatchPlan {
            apply_gain: false,
            apply_matrix: false,
            apply_curves: false,
            preview_safe: false,
        },
        // A resolved DCP supplies the colour characterization, so drop the
        // embedded-JPEG COLOUR fit — but keep the brightness baseline so the
        // render opens at the camera's exposure instead of a dark scene-linear
        // level (the owner-validated "gain-only" DCP look).
        _ if dcp_selected => JpegMatchPlan {
            apply_gain: true,
            apply_matrix: false,
            apply_curves: false,
            preview_safe: false,
        },
        _ => JpegMatchPlan {
            apply_gain: true,
            apply_matrix: true,
            apply_curves: true,
            preview_safe: false,
        },
    }
}

fn jpeg_match_plan(dcp_selected: bool) -> JpegMatchPlan {
    let value = std::env::var("IAI_RAW_JPEG_MATCH").ok();
    jpeg_match_plan_for(value.as_deref(), dcp_selected)
}

/// Build the iAi scene + canvas from a decoded rawloader mosaic. Shared by both
/// the rawloader and rawler front-ends.
fn decode_raw_from(decoded: DecodedRaw, path: &Path) -> Result<Canvas, String> {
    let DecodedRaw {
        image: raw,
        backend,
    } = decoded;
    let (w, h) = (raw.width, raw.height);
    let area = active_area(&raw)?;
    if w == 0 || h == 0 {
        return Err("RAW có kích thước bằng 0".into());
    }

    // Active (cropped) area. crops order is [top, right, bottom, left].
    let (cw, ch) = (area.width, area.height);
    if cw == 0 || ch == 0 {
        return Err("RAW không có vùng ảnh hợp lệ".into());
    }
    let max = crate::core::canvas::MAX_DIMENSION as usize;
    if cw > max || ch > max {
        return Err(format!("Ảnh RAW quá lớn ({cw}x{ch}), tối đa {max}x{max}"));
    }

    let mono = raw.cpp == 1 && !raw.cfa.is_valid();

    // White balance: prefer the as-shot coefficients (RGBE), fall back to a
    // neutral D65 estimate when the file carries none. Normalize so green = 1.
    let wbc = if raw.wb_coeffs[0] > 0.0 && raw.wb_coeffs[1] > 0.0 && raw.wb_coeffs[2] > 0.0 {
        raw.wb_coeffs
    } else {
        raw.neutralwb()
    };
    let gain = white_balance_gains(wbc, mono);

    // Per-colour black point and dynamic range. When whitelevels are missing,
    // invalid, or a 16-bit container maximum is reported for 12/14-bit data, fall
    // back to the observed active-area channel maximum.
    let levels = raw_levels(&raw, area);
    let normalize = |val: f32, c: usize| -> f32 {
        ((val - levels.black[c]) / levels.denom[c]).max(0.0) * gain[c]
    };

    // Camera RGB → linear sRGB. cam_to_xyz_normalized is [XYZ][RGBE]; the E column
    // is zero for ordinary 3-colour sensors, so a 3×3 (RGB) compose is exact.
    let cam2xyz = raw.cam_to_xyz_normalized();
    let mut as_shot_white = as_shot_white_balance(&cam2xyz, gain, mono);
    // Bit-exact decoder compatibility matrix, kept for the legacy sRGB writer
    // and used as the resolver's guaranteed final tier.
    let decoder_srgb = resolve_decoder_matrix(backend, &raw.clean_make, &raw.clean_model, &cam2xyz)
        .camera_to_linear_srgb;

    // DCP characterization is only meaningful for ordinary three-colour RGB
    // sensors. Monochrome and four-colour (RGBE/CMY) sensors keep the legacy
    // sRGB path, so no DCP candidates are offered for them.
    let dcp_eligible = !mono && camera_matrix_is_three_colour(&cam2xyz);
    // Explicit env override wins; otherwise fall back to a per-camera DCP in the
    // default profile directory beside the executable. Both flow through the
    // resolver's explicit tier (required camera-match), so a wrong file is
    // rejected, not applied.
    let explicit_profile = if dcp_eligible {
        load_explicit_dcp_override()
            .or_else(|| load_default_camera_dcp(raw.clean_make.as_str(), raw.clean_model.as_str()))
    } else {
        None
    };
    let explicit_blob: Option<ProfileBlob> = explicit_profile.as_ref().map(|p| p.blob());
    // Owned profile bytes (embedded + manifest) must outlive the borrowed
    // candidates and the resolution below.
    let embedded_profiles = if dcp_eligible {
        load_embedded_dng_dcps(path)
    } else {
        Vec::new()
    };
    let embedded_dcps: Vec<_> = embedded_profiles
        .iter()
        .enumerate()
        .map(|(index, bytes)| EmbeddedDcpCandidate {
            blob: ProfileBlob {
                bytes,
                locator: "<embedded DNG profile>",
            },
            profile_index: index as u16,
        })
        .collect();
    let manifest_profiles = if dcp_eligible {
        load_manifest_dcp_profiles(raw.clean_make.as_str(), raw.clean_model.as_str())
    } else {
        Vec::new()
    };
    let manifest_dcps: Vec<_> = manifest_profiles
        .iter()
        .filter_map(|profile| profile.as_dcp_candidate())
        .collect();

    let resolution = resolver::resolve(ResolveRequest {
        camera: CameraIdentityRef {
            make: raw.clean_make.as_str(),
            model: raw.clean_model.as_str(),
        },
        wb_gains: [gain[0] as f64, gain[1] as f64, gain[2] as f64],
        explicit_dcp: explicit_blob,
        embedded_dcps: &embedded_dcps,
        manifest_dcps: &manifest_dcps,
        // Scene ICC stays gated: RAW retains signed/HDR values the bounded ICC
        // adapter cannot accept, and it must never be silently clamped.
        trusted_scene_iccs: &[],
        decoder_fallback: DecoderFallback {
            backend,
            camera_to_xyz: &cam2xyz,
        },
    });
    // Clone the provenance now so it can be moved onto the scene after the pixel
    // loops, while the writer keeps borrowing the resolved transform.
    let resolver_provenance = resolution.provenance.clone();
    if let ResolvedCameraCharacterization::Dcp { transform, .. } = &resolution.characterization {
        if let Some(white) = &mut as_shot_white {
            // The profile-aware neutral iteration is a better CCT estimate than
            // the compatibility decoder matrix; retain the independently
            // measured Duv coordinate from the camera neutral.
            white.cct_kelvin = transform.selection.cct_kelvin as f32;
        }
    }

    let writer = match &resolution.characterization {
        ResolvedCameraCharacterization::Dcp { transform, .. } if dcp_eligible => {
            // Quantize the post-WB camera→linear-ProPhoto matrix to f32 once and
            // apply it directly to camera RGB; never route DCP through sRGB.
            let matrix = quantize_matrix_f32(&transform.post_wb_camera_to_linear_prophoto);
            let hue_sat = transform
                .hue_sat_map
                .as_ref()
                .and_then(|table| PreparedHueSatMap::new(table).ok());
            SceneWriter::Dcp { matrix, hue_sat }
        }
        _ => SceneWriter::LegacySrgb {
            cam2srgb: decoder_srgb,
        },
    };
    let dcp_selected = matches!(writer, SceneWriter::Dcp { .. });
    let writer = &writer;
    // Resolve every decode-time taste knob once into a named recipe. The
    // shipping default remains byte-identical; `technical-neutral-v2` is an
    // opt-in Q1 audit path until its look has owner GUI approval.
    let raw_render_recipe = RawRenderRecipe::resolve();

    let crop_top = area.top;
    let crop_left = area.left;
    let cfa = &raw.cfa;
    let sensor_corrections = sensor_correction_plan(w, h, raw.cpp, cfa.is_valid(), mono);
    let mut out = vec![0u16; cw * ch * 4];

    match raw.cpp {
        // Bayer mosaic (or monochrome): build a normalized, white-balanced mono
        // plane over the full sensor, then bilinear-demosaic the active area.
        1 => {
            let mut plane: Vec<f32> = match &raw.data {
                RawImageData::Integer(v) => (0..w * h)
                    .into_par_iter()
                    .map(|i| normalize(v[i] as f32, cfa.color_at(i / w, i % w)))
                    .collect(),
                RawImageData::Float(v) => (0..w * h)
                    .into_par_iter()
                    .map(|i| normalize(v[i], cfa.color_at(i / w, i % w)))
                    .collect(),
            };
            // Camera pipelines such as ART/ACR suppress isolated dead/hot
            // sensels before interpolation. Without this, one defective Bayer
            // sample expands into a small black/coloured dot after demosaic.
            apply_isolated_bayer_defect_stage(
                &mut plane,
                w,
                h,
                cfa,
                sensor_corrections.isolated_bayer_defects,
            );
            // Reconstruct clipped highlights on the mosaic, before demosaic.
            if !mono {
                inpaint_opposed_bayer(&mut plane, w, h, cfa, gain);
            }
            let plane = plane;

            // Whole-sensor AHD demosaic, computed once (skipped for mono and past
            // the pixel cap). The per-pixel loop below just reads it back; a None
            // here means the loop falls to Malvar/bilinear per pixel instead.
            // The cap is overridable (`IAI_AHD_MAX_PIXELS`) so AHD's cleaner
            // diagonal-edge handling can be used on full-frame RAW that would
            // otherwise fall back to Malvar (which zippers at high-contrast edges).
            let ahd_cap = env_usize("IAI_AHD_MAX_PIXELS", AHD_MAX_PIXELS);
            let ahd_rgb: Option<Vec<[f32; 3]>> =
                if DEMOSAIC == DemosaicMethod::Ahd && !mono && w.saturating_mul(h) <= ahd_cap {
                    Some(demosaic_ahd(&plane, w, h, cfa, &cam2xyz))
                } else {
                    None
                };

            out.par_chunks_mut(cw * 4)
                .enumerate()
                .for_each(|(oy, row)| {
                    let fr = oy + crop_top;
                    for ox in 0..cw {
                        let fc = ox + crop_left;
                        let dst = ox * 4;
                        if mono {
                            let e = f32_to_f16_bits(plane[fr * w + fc]);
                            row[dst] = e;
                            row[dst + 1] = e;
                            row[dst + 2] = e;
                            row[dst + 3] = 0x3c00; // 1.0
                            continue;
                        }
                        let cam = if let Some(ref rgb) = ahd_rgb {
                            rgb[fr * w + fc]
                        } else if DEMOSAIC == DemosaicMethod::Bilinear {
                            // 3×3 per-colour average = bilinear interpolation of the
                            // missing channels (edges clamp to the sensor bounds).
                            let mut sum = [0.0f32; 3];
                            let mut cnt = [0.0f32; 3];
                            for dr in -1i32..=1 {
                                for dc in -1i32..=1 {
                                    let nr = (fr as i32 + dr).clamp(0, h as i32 - 1) as usize;
                                    let nc = (fc as i32 + dc).clamp(0, w as i32 - 1) as usize;
                                    let col = chroma_channel(cfa.color_at(nr, nc));
                                    if col < 3 {
                                        sum[col] += plane[nr * w + nc];
                                        cnt[col] += 1.0;
                                    }
                                }
                            }
                            [
                                sum[0] / cnt[0].max(1.0),
                                sum[1] / cnt[1].max(1.0),
                                sum[2] / cnt[2].max(1.0),
                            ]
                        } else {
                            // Malvar, or AHD that fell back past the pixel cap.
                            demosaic_malvar(&plane, w, h, cfa, fr, fc)
                        };
                        writer.write(&mut row[dst..dst + 4], cam);
                    }
                });
        }
        // Already demosaiced RGB (e.g. linear DNG) — still in camera colour space.
        3 => {
            let opposed = opposed_rgb(&raw.data, w, h, &levels, gain);
            out.par_chunks_mut(cw * 4)
                .enumerate()
                .for_each(|(oy, row)| {
                    let fr = oy + crop_top;
                    for ox in 0..cw {
                        let fc = ox + crop_left;
                        let src = (fr * w + fc) * 3;
                        let mut cam = match &raw.data {
                            RawImageData::Integer(v) => [
                                normalize(v[src] as f32, 0),
                                normalize(v[src + 1] as f32, 1),
                                normalize(v[src + 2] as f32, 2),
                            ],
                            RawImageData::Float(v) => [
                                normalize(v[src], 0),
                                normalize(v[src + 1], 1),
                                normalize(v[src + 2], 2),
                            ],
                        };
                        if let Some(op) = &opposed {
                            op.reconstruct(fr * w + fc, &mut cam);
                        }
                        writer.write(&mut row[ox * 4..ox * 4 + 4], cam);
                    }
                });
        }
        other => return Err(format!("RAW {other} kênh/điểm ảnh chưa hỗ trợ")),
    }

    // Default false-colour suppression on the linear scene: a chroma median that
    // drops the Malvar demosaic's isolated edge colour specks (the reddish-brown
    // dotted rim on high-contrast skin↔dark edges) while keeping green — and thus
    // luminance detail — exact. Runs FIRST, before the colour NR and sharpen.
    // Skipped for monochrome (no chroma).
    let fc_iters = env_usize("IAI_SCENE_FALSE_COLOR", SCENE_FALSE_COLOR_ITERS);
    if !mono && fc_iters > 0 {
        suppress_false_color(&mut out, cw, ch, fc_iters);
    }

    // Default colour-noise reduction on the linear scene, before capture sharpen
    // (so the sharpener never re-amplifies chroma speckle) and before any chroma
    // enrichment. Skipped for monochrome (no chroma to clean).
    let scene_color_nr = raw_render_recipe.scene_color_nr;
    if !mono && scene_color_nr > 1e-4 {
        denoise_scene_chroma(&mut out, cw, ch, scene_color_nr);
    }

    // Capture sharpening on the linear scene, after demosaic and before the
    // master is frozen (before orientation too, but the pass is isotropic so
    // the order is irrelevant).
    if raw_render_recipe.capture_sharpen_gain > 1e-4 {
        capture_sharpen(
            &mut out,
            cw,
            ch,
            raw_render_recipe.capture_sharpen_gain,
            raw_render_recipe.capture_sharpen_dark_ratio,
            raw_render_recipe.capture_sharpen_floor,
        );
    }

    // Apply EXIF orientation so portraits aren't sideways. The buffer holds f16
    // bits at this point; orientation only moves 4-u16 pixels, so it is agnostic.
    let (out, fw, fh) = apply_orientation(out, cw, ch, raw.orientation);

    // Baseline exposure: lift the scene so the default render matches the camera's
    // embedded-JPEG brightness. A scene-referred RAW otherwise opens flatter and
    // darker than that preview (the camera bakes its picture-style tone into the
    // JPEG), which reads as the image "jumping dark" once the full decode replaces
    // the instant preview. Best-effort: files without a preview are left as-is.
    // Decide the embedded-JPEG match policy first: a profile-backed DCP render
    // is already colour-accurate, so it defaults to no JPEG fit, while the
    // decoder-matrix fallback keeps the shipping brightness+colour match. An
    // explicit IAI_RAW_JPEG_MATCH override still wins.
    let jpeg_match = jpeg_match_plan(dcp_selected);

    // Always consume any cached preview stats so stale preview data cannot leak
    // into a later decode, but only read/compute them when a mode needs them.
    let cached_preview = crate::formats::raw_preview::take_cached_stats(path);
    let preview_stats = if jpeg_match.apply_gain
        || jpeg_match.apply_matrix
        || jpeg_match.apply_curves
        || jpeg_match.preview_safe
    {
        cached_preview.or_else(|| {
            std::fs::read(path)
                .ok()
                .and_then(|bytes| crate::formats::raw_preview::preview_stats_from_bytes(&bytes))
        })
    } else {
        None
    };

    let mut scene = SceneSource {
        width: fw as u32,
        height: fh as u32,
        half: out,
        alpha: None,
        look: crate::core::develop_scene::BaseLook::Raw,
        color_pipeline: crate::core::working_color::ColorPipelineMetadata::default(),
        camera_profile: Some(RawSceneCharacterization {
            resolution: resolver_provenance,
            jpeg_match: JpegMatchMode::from_stages(
                jpeg_match.apply_gain,
                jpeg_match.apply_matrix,
                jpeg_match.apply_curves,
                jpeg_match.preview_safe,
            ),
            raw_render_recipe: raw_render_recipe.version,
        }),
        as_shot_white_balance: as_shot_white,
        camera_rgb_curve: None,
    };
    if let Some(target) = preview_stats {
        if jpeg_match.apply_gain {
            let gain =
                crate::core::develop_scene::baseline_rgb_gains_for_scene(&scene, target.mean_rgb);
            if gain.iter().any(|g| (g - 1.0).abs() > 0.01) {
                scale_scene_rgb(&mut scene.half, gain);
            }
        }
        if jpeg_match.apply_matrix {
            let matrix = crate::core::develop_scene::fit_camera_color_matrix(
                &scene,
                &target.thumbnail_rgb,
                target.thumbnail_width,
                target.thumbnail_height,
            );
            if camera_color_matrix_is_material(matrix) {
                transform_scene_rgb(&mut scene.half, matrix);
            }
        }
        if jpeg_match.preview_safe {
            // The spatial matrix changes the display mean slightly. Re-fit only
            // a scalar exposure afterwards so brightness returns to the camera
            // preview without introducing a per-channel cast.
            let post_gain = fit_preview_luma_gain_for_scene(
                &scene,
                target.mean_luma,
                target.thumbnail_width,
                target.thumbnail_height,
            );
            if (post_gain - 1.0).abs() > 0.005 {
                scale_scene(&mut scene.half, post_gain);
            }

            // Match only aggregate display chroma with a bounded, hue- and
            // luminance-preserving scale. Unlike the removed RGB histogram
            // curves this has no spatial filter and cannot soften detail.
            let chroma_strength = fit_preview_chroma_strength(
                &scene,
                &target.thumbnail_rgb,
                target.thumbnail_width,
                target.thumbnail_height,
            );
            enrich_scene_chroma(&mut scene.half, chroma_strength);
        }
        if jpeg_match.apply_curves {
            // Restore the camera-preview midtone saturation before fitting the
            // per-channel curves. Technical-neutral keeps this at identity.
            enrich_scene_chroma(&mut scene.half, raw_render_recipe.chroma_enrich);
            scene.camera_rgb_curve = Some(crate::core::develop_scene::fit_camera_rgb_curve(
                &scene,
                &target.histogram,
            ));
        }
    }

    // Clean warm default look for the paths WITHOUT the full embedded-JPEG colour
    // fit — the DCP "gain-only" path (the owner's profiled cameras) and the
    // no-match path. The full-fit path already reaches the camera-JPEG colour
    // numerically, so it is left alone. Everything here runs on the already-
    // denoised master, AFTER the baseline brightness match, and bakes into the one
    // scene master so the GPU preview and the CPU commit inherit it identically.
    // Hue-preserving and saturation-protected; neutrals stay neutral.
    if !jpeg_match.apply_matrix && !jpeg_match.apply_curves {
        let brightness = raw_render_recipe.scene_brightness;
        if (brightness - 1.0).abs() > 1e-4 {
            scale_scene(&mut scene.half, brightness);
        }
        enrich_scene_chroma_shadow(
            &mut scene.half,
            raw_render_recipe.scene_chroma_base,
            raw_render_recipe.scene_chroma_shadow,
            raw_render_recipe.chroma_shadow_low_ev,
            raw_render_recipe.chroma_shadow_high_ev,
        );
        warm_scene(&mut scene.half, raw_render_recipe.scene_warm);
    }

    // The unclamped linear master + its neutral default-look render.
    let px16 = render_default_look(&scene);

    let mut canvas = Canvas::from_rgba16(px16, fw as u32, fh as u32);
    canvas.develop_source = Some(std::sync::Arc::new(scene));
    // The rendered pixels are sRGB — tag the document accordingly.
    canvas.icc_profile = crate::core::canvas::IccProfile {
        name: crate::core::cms::WorkingProfile::Srgb.name().to_string(),
        data: crate::core::cms::srgb_icc_bytes(),
    };
    let cam = format!("{} {}", raw.clean_make.trim(), raw.clean_model.trim());
    canvas.metadata.source_profile = cam.trim().to_string();
    canvas.metadata.develop_working_space =
        crate::core::working_color::WorkingColorSpace::LinearProPhoto;
    canvas.metadata.color_pipeline_version = 2;
    Ok(canvas)
}

/// Demosaic algorithm for Bayer sensors.
#[derive(Clone, Copy, PartialEq)]
enum DemosaicMethod {
    /// 3×3 per-colour average — fastest, softest, most colour fringing.
    Bilinear,
    /// Malvar-He-Cutler (2004) gradient-corrected linear interpolation: a
    /// per-pixel correction from the known channel's Laplacian added to bilinear.
    /// Sharp, near-AHD, O(1) memory.
    Malvar,
    /// Adaptive Homogeneity-Directed (Hirakawa-Parks). Interpolates green both
    /// horizontally and vertically, then picks per pixel the direction that stays
    /// locally more HOMOGENEOUS in CIELab — the one whose interpolation crossed
    /// fewer edges. Removes the zipper/maze and colour moiré that even Malvar
    /// leaves on fine directional detail. Whole-sensor (~50 bytes/px transient),
    /// so it falls back to Malvar past [`AHD_MAX_PIXELS`].
    Ahd,
}
const DEMOSAIC: DemosaicMethod = DemosaicMethod::Ahd;

/// Above this sensor pixel count the whole-sensor AHD demosaic falls back to the
/// sharp O(1)-memory Malvar path. AHD is worth it well past the old 12 MP cap:
/// measured on a 36 MP frame in release it adds only ~0.2 s to a ~7.7 s decode,
/// and it avoids the diagonal-edge zipper / reddish false-colour Malvar leaves at
/// high-contrast edges (the camera JPEG and Photoshop are clean because they
/// demosaic this well). So full-frame and most high-res RAW now take the AHD
/// path; only very large sensors (past ~64 MP, where the ~50 B/px transient gets
/// heavy) fall back. Debug builds keep a low cap because the unoptimised AHD math
/// can otherwise hold Develop on its loading screen for minutes. Overridable via
/// `IAI_AHD_MAX_PIXELS`.
const AHD_MAX_PIXELS: usize = if cfg!(debug_assertions) {
    4_000_000
} else {
    64_000_000
};

/// Demosaic one Bayer pixel with the Malvar-He-Cutler 5×5 gradient-corrected
/// kernels. Samples the normalized+WB mono `plane` (edge-clamped); returns camera
/// RGB. Same-colour neighbours at ±2 form the gradient correction, so it queries the
/// true CFA pattern via `cfa` (modular, valid at any coord).
#[inline]
fn demosaic_malvar(
    plane: &[f32],
    w: usize,
    h: usize,
    cfa: &rawloader::CFA,
    r: usize,
    c: usize,
) -> [f32; 3] {
    let at = |dr: i32, dc: i32| -> f32 {
        let nr = (r as i32 + dr).clamp(0, h as i32 - 1) as usize;
        let nc = (c as i32 + dc).clamp(0, w as i32 - 1) as usize;
        plane[nr * w + nc]
    };
    let center = at(0, 0);
    let diag = at(-1, -1) + at(-1, 1) + at(1, -1) + at(1, 1);
    let cc = cfa.color_at(r, c);

    let (red, green, blue);
    if cc == 1 {
        // Green site: known. The two other colours lie horizontally vs vertically.
        green = center;
        let h2 = at(0, -2) + at(0, 2);
        let v2 = at(-2, 0) + at(2, 0);
        let horiz = at(0, -1) + at(0, 1);
        let vert = at(-1, 0) + at(1, 0);
        // Same-colour-horizontal kernel (c5, h±1=4, diag=-1, h±2=-1, v±2=+0.5) and its transpose.
        let chan_h = (5.0 * center + 4.0 * horiz - diag - h2 + 0.5 * v2) / 8.0;
        let chan_v = (5.0 * center + 4.0 * vert - diag - v2 + 0.5 * h2) / 8.0;
        if cfa.color_at(r, c + 1) == 0 {
            red = chan_h; // red neighbours are horizontal
            blue = chan_v;
        } else {
            blue = chan_h;
            red = chan_v;
        }
    } else {
        // Red or Blue site. Green via the cross kernel (c4, cross±1=2, far±2=-1)…
        let cross = at(0, -1) + at(0, 1) + at(-1, 0) + at(1, 0);
        let far = at(0, -2) + at(0, 2) + at(-2, 0) + at(2, 0);
        green = (4.0 * center + 2.0 * cross - far) / 8.0;
        // …opposite colour (diagonal neighbours) via the diagonal kernel (c6, diag=2, far=-1.5).
        let opposite = (6.0 * center + 2.0 * diag - 1.5 * far) / 8.0;
        if cc == 0 {
            red = center;
            blue = opposite;
        } else {
            blue = center;
            red = opposite;
        }
    }
    // Gradient correction can overshoot; the negative side is unphysical.
    [red.max(0.0), green.max(0.0), blue.max(0.0)]
}

/// Edge-clamped flat index into a `w×h` sensor plane.
#[inline]
fn ahd_idx(w: usize, h: usize, r: i32, c: i32) -> usize {
    let rr = r.clamp(0, h as i32 - 1) as usize;
    let cc = c.clamp(0, w as i32 - 1) as usize;
    rr * w + cc
}

/// Hamilton-Adams directional green plane: at each red/blue site estimate green
/// along one axis, correcting the neighbour average with the same-colour Laplacian
/// at ±2. Green sites keep their measured value. `horizontal` selects the axis.
fn ahd_green(
    plane: &[f32],
    w: usize,
    h: usize,
    cfa: &rawloader::CFA,
    horizontal: bool,
) -> Vec<f32> {
    (0..w * h)
        .into_par_iter()
        .map(|i| {
            let (r, c) = ((i / w) as i32, (i % w) as i32);
            if cfa.color_at(r as usize, c as usize) == 1 {
                return plane[i];
            }
            let center = plane[i];
            let (n1, n2, f1, f2) = if horizontal {
                (
                    ahd_idx(w, h, r, c - 1),
                    ahd_idx(w, h, r, c + 1),
                    ahd_idx(w, h, r, c - 2),
                    ahd_idx(w, h, r, c + 2),
                )
            } else {
                (
                    ahd_idx(w, h, r - 1, c),
                    ahd_idx(w, h, r + 1, c),
                    ahd_idx(w, h, r - 2, c),
                    ahd_idx(w, h, r + 2, c),
                )
            };
            ((plane[n1] + plane[n2]) * 0.5 + (2.0 * center - plane[f1] - plane[f2]) * 0.25).max(0.0)
        })
        .collect()
}

/// Reconstruct full camera RGB for one directional green plane by interpolating
/// the colour DIFFERENCES (X − G), which are smooth across edges: green sites take
/// R and B from their horizontal vs vertical neighbour pairs; red/blue sites take
/// the opposite colour from the four diagonals. The known channel stays measured.
fn ahd_reconstruct(
    plane: &[f32],
    green: &[f32],
    w: usize,
    h: usize,
    cfa: &rawloader::CFA,
) -> Vec<[f32; 3]> {
    (0..w * h)
        .into_par_iter()
        .map(|i| {
            let (r, c) = ((i / w) as i32, (i % w) as i32);
            let g = green[i];
            match cfa.color_at(r as usize, c as usize) {
                1 => {
                    let l = ahd_idx(w, h, r, c - 1);
                    let rt = ahd_idx(w, h, r, c + 1);
                    let u = ahd_idx(w, h, r - 1, c);
                    let d = ahd_idx(w, h, r + 1, c);
                    let horiz = g + 0.5 * ((plane[l] - green[l]) + (plane[rt] - green[rt]));
                    let vert = g + 0.5 * ((plane[u] - green[u]) + (plane[d] - green[d]));
                    // The horizontal neighbours carry one colour, the vertical the other.
                    if cfa.color_at(r as usize, (c + 1) as usize) == 0 {
                        [horiz.max(0.0), g, vert.max(0.0)]
                    } else {
                        [vert.max(0.0), g, horiz.max(0.0)]
                    }
                }
                col => {
                    let mut sum = 0.0;
                    for (dr, dc) in [(-1i32, -1i32), (-1, 1), (1, -1), (1, 1)] {
                        let j = ahd_idx(w, h, r + dr, c + dc);
                        sum += plane[j] - green[j];
                    }
                    let opp = (g + sum * 0.25).max(0.0);
                    if col == 0 {
                        [plane[i], g, opp] // red site: R measured, B interpolated
                    } else {
                        [opp, g, plane[i]] // blue site: B measured, R interpolated
                    }
                }
            }
        })
        .collect()
}

/// CIELab of a camera-RGB triple via the normalized cam→XYZ matrix (rows sum to 1,
/// so a neutral maps to L*=100, a*=b*=0). AHD only compares neighbours, so absolute
/// accuracy is irrelevant and the equal-energy white (1,1,1) is used directly.
#[inline]
fn cam_to_lab(cam: [f32; 3], m: &[[f32; 4]; 3]) -> [f32; 3] {
    let x = (m[0][0] * cam[0] + m[0][1] * cam[1] + m[0][2] * cam[2]).max(0.0);
    let y = (m[1][0] * cam[0] + m[1][1] * cam[1] + m[1][2] * cam[2]).max(0.0);
    let z = (m[2][0] * cam[0] + m[2][1] * cam[1] + m[2][2] * cam[2]).max(0.0);
    let f = |t: f32| {
        if t > 0.008856 {
            t.cbrt()
        } else {
            7.787 * t + 16.0 / 116.0
        }
    };
    let (fx, fy, fz) = (f(x), f(y), f(z));
    [116.0 * fy - 16.0, 500.0 * (fx - fy), 200.0 * (fy - fz)]
}

/// Adaptive Homogeneity-Directed demosaic (see [`DemosaicMethod::Ahd`]). Returns
/// full camera RGB over the whole sensor. Transient memory is ~50 bytes/px, so the
/// caller gates this by [`AHD_MAX_PIXELS`].
fn demosaic_ahd(
    plane: &[f32],
    w: usize,
    h: usize,
    cfa: &rawloader::CFA,
    cam2xyz: &[[f32; 4]; 3],
) -> Vec<[f32; 3]> {
    // (1) Directional green, then (2) full RGB candidate per direction.
    let rgb_h = {
        let gh = ahd_green(plane, w, h, cfa, true);
        ahd_reconstruct(plane, &gh, w, h, cfa)
    };
    let rgb_v = {
        let gv = ahd_green(plane, w, h, cfa, false);
        ahd_reconstruct(plane, &gv, w, h, cfa)
    };

    // (3) CIELab of each candidate for the homogeneity metric.
    let lab_h: Vec<[f32; 3]> = rgb_h.par_iter().map(|&p| cam_to_lab(p, cam2xyz)).collect();
    let lab_v: Vec<[f32; 3]> = rgb_v.par_iter().map(|&p| cam_to_lab(p, cam2xyz)).collect();

    // (4) Per-pixel homogeneity count in each direction, using dcraw's adaptive Lab
    // thresholds: the tighter of the two directions' local luminance/chroma gradients.
    let homo: Vec<[u8; 2]> = (0..w * h)
        .into_par_iter()
        .map(|i| {
            let (r, c) = ((i / w) as i32, (i % w) as i32);
            // up, down, left, right
            let nb = [
                ahd_idx(w, h, r - 1, c),
                ahd_idx(w, h, r + 1, c),
                ahd_idx(w, h, r, c - 1),
                ahd_idx(w, h, r, c + 1),
            ];
            let mut ld_h = [0.0f32; 4];
            let mut cd_h = [0.0f32; 4];
            let mut ld_v = [0.0f32; 4];
            let mut cd_v = [0.0f32; 4];
            for k in 0..4 {
                let j = nb[k];
                ld_h[k] = (lab_h[i][0] - lab_h[j][0]).abs();
                cd_h[k] = (lab_h[i][1] - lab_h[j][1]).powi(2) + (lab_h[i][2] - lab_h[j][2]).powi(2);
                ld_v[k] = (lab_v[i][0] - lab_v[j][0]).abs();
                cd_v[k] = (lab_v[i][1] - lab_v[j][1]).powi(2) + (lab_v[i][2] - lab_v[j][2]).powi(2);
            }
            // Horizontal image → horizontal gradient (left,right = 2,3); vertical
            // image → vertical gradient (up,down = 0,1).
            let leps = ld_h[2].max(ld_h[3]).min(ld_v[0].max(ld_v[1]));
            let ceps = cd_h[2].max(cd_h[3]).min(cd_v[0].max(cd_v[1]));
            let mut hh = 0u8;
            let mut hv = 0u8;
            for k in 0..4 {
                if ld_h[k] <= leps && cd_h[k] <= ceps {
                    hh += 1;
                }
                if ld_v[k] <= leps && cd_v[k] <= ceps {
                    hv += 1;
                }
            }
            [hh, hv]
        })
        .collect();
    drop(lab_h);
    drop(lab_v);

    // (5) Pick, per pixel, the direction more homogeneous over a 3×3 window; tie →
    // average both candidates.
    (0..w * h)
        .into_par_iter()
        .map(|i| {
            let (r, c) = ((i / w) as i32, (i % w) as i32);
            let (mut sh, mut sv) = (0u32, 0u32);
            for dr in -1..=1 {
                for dc in -1..=1 {
                    let j = ahd_idx(w, h, r + dr, c + dc);
                    sh += homo[j][0] as u32;
                    sv += homo[j][1] as u32;
                }
            }
            if sh > sv {
                rgb_h[i]
            } else if sv > sh {
                rgb_v[i]
            } else {
                [
                    (rgb_h[i][0] + rgb_v[i][0]) * 0.5,
                    (rgb_h[i][1] + rgb_v[i][1]) * 0.5,
                    (rgb_h[i][2] + rgb_v[i][2]) * 0.5,
                ]
            }
        })
        .collect()
}

// Highlight reconstruction — "inpaint opposed". A blown sensor channel clips at
// its white level while the other channels keep real data; because the
// white-balance gains differ per channel, the clipped pixel would render with a
// colour cast (magenta skies). Neutralising it toward the brightest channel (the
// old recovery) also destroys the true chroma of bright subjects. Instead we
// measure, over the UNCLIPPED pixels bordering each clipped region, the chromatic
// offset between the clipped channel and the mean of the other two (in cube-root
// space, which evens the offset across brightness), then inpaint every clipped
// sample as `mean(others) + offset`. Reconstructed values exceed the clip level
// and survive into the unclamped f16 scene master, so Develop's
// Exposure/Highlights can pull the texture and colour back.
const HIGHLIGHT_RECOVERY: bool = true;
/// Normalized (pre-gain) sample value treated as clipped.
const CLIP_THRESH: f32 = 0.98;
/// Window radius (sensor px) for border-candidate detection and channel means.
const OPPOSED_RADIUS: i32 = 2;

#[inline]
fn croot(v: f32) -> f32 {
    v.max(0.0).cbrt()
}

/// Fold the second green (CFA colour 3) in with green; R/G/B pass through.
#[inline]
fn chroma_channel(col: usize) -> usize {
    if col == 3 {
        1
    } else {
        col
    }
}

/// Per-channel clip thresholds in white-balanced units (the plane and camera
/// triples hold `normalized × gain`, so saturation sits at `CLIP_THRESH × gain`).
#[inline]
fn clip_levels(gain: [f32; 4]) -> [f32; 4] {
    let mut clips = [f32::INFINITY; 4];
    for c in 0..4 {
        if gain[c] > 0.0 {
            clips[c] = CLIP_THRESH * gain[c];
        }
    }
    clips
}

/// Separable ±`r` box dilation of a per-site clipped-channel bitmask: bit `k` of
/// the result is set when any site within the box is clipped in chroma channel
/// `k`. Turns the border-candidate test into a single bit check per site.
fn dilate_bitmask(clipped: &[u8], w: usize, h: usize, r: i32) -> Vec<u8> {
    let mut horiz = vec![0u8; w * h];
    horiz.par_chunks_mut(w).enumerate().for_each(|(row, out)| {
        let base = row * w;
        for (col, slot) in out.iter_mut().enumerate() {
            let lo = (col as i32 - r).max(0) as usize;
            let hi = ((col as i32 + r) as usize).min(w - 1);
            let mut m = 0u8;
            for c in lo..=hi {
                m |= clipped[base + c];
            }
            *slot = m;
        }
    });
    let mut near = vec![0u8; w * h];
    near.par_chunks_mut(w).enumerate().for_each(|(row, out)| {
        let lo = (row as i32 - r).max(0) as usize;
        let hi = ((row as i32 + r) as usize).min(h - 1);
        for (col, slot) in out.iter_mut().enumerate() {
            let mut m = 0u8;
            for rr in lo..=hi {
                m |= horiz[rr * w + col];
            }
            *slot = m;
        }
    });
    near
}

/// Cube-root "opposed" reference at a Bayer site: per-channel means over the
/// ±[`OPPOSED_RADIUS`] window, then the mean of the two channels OTHER than `k`.
fn opposed_refavg_bayer(
    plane: &[f32],
    w: usize,
    h: usize,
    cfa: &rawloader::CFA,
    row: i32,
    col: i32,
    k: usize,
) -> f32 {
    let mut sum = [0.0f32; 3];
    let mut cnt = [0.0f32; 3];
    for dr in -OPPOSED_RADIUS..=OPPOSED_RADIUS {
        for dc in -OPPOSED_RADIUS..=OPPOSED_RADIUS {
            let rr = (row + dr).clamp(0, h as i32 - 1) as usize;
            let cc = (col + dc).clamp(0, w as i32 - 1) as usize;
            let ch = chroma_channel(cfa.color_at(rr, cc));
            if ch < 3 {
                sum[ch] += plane[rr * w + cc];
                cnt[ch] += 1.0;
            }
        }
    }
    let m = |ch: usize| croot(sum[ch] / cnt[ch].max(1.0));
    0.5 * (m((k + 1) % 3) + m((k + 2) % 3))
}

/// Replace only extreme, isolated Bayer sensels using nearby samples of the
/// same CFA colour. The fast four-neighbour precheck keeps normal texture and
/// real dark lines untouched; the wider median confirmation rejects edges.
fn correct_isolated_bayer_defects(plane: &mut [f32], w: usize, h: usize, cfa: &rawloader::CFA) {
    if w < 9 || h < 9 {
        return;
    }
    let src: &[f32] = plane;
    let updates: Vec<(usize, f32)> = (4..h - 4)
        .into_par_iter()
        .flat_map_iter(|row| {
            (4..w - 4).filter_map(move |col| {
                let i = row * w + col;
                let channel = cfa.color_at(row, col);
                let center = src[i];
                let mut nearest = [0.0f32; 4];
                let mut count = 0usize;
                for (dr, dc) in [(-2i32, 0i32), (2, 0), (0, -2), (0, 2)] {
                    let rr = (row as i32 + dr) as usize;
                    let cc = (col as i32 + dc) as usize;
                    if cfa.color_at(rr, cc) == channel {
                        nearest[count] = src[rr * w + cc];
                        count += 1;
                    }
                }
                if count < 2 {
                    return None;
                }
                let lo = nearest[..count]
                    .iter()
                    .copied()
                    .fold(f32::INFINITY, f32::min);
                let hi = nearest[..count]
                    .iter()
                    .copied()
                    .fold(f32::NEG_INFINITY, f32::max);
                let obvious_dead = lo > 0.005 && center < lo * 0.20;
                let obvious_hot = center > hi * 4.0 + 0.02;
                if !obvious_dead && !obvious_hot {
                    return None;
                }

                let mut neighbours = Vec::with_capacity(24);
                for dr in -4i32..=4 {
                    for dc in -4i32..=4 {
                        if dr == 0 && dc == 0 {
                            continue;
                        }
                        let rr = (row as i32 + dr) as usize;
                        let cc = (col as i32 + dc) as usize;
                        if cfa.color_at(rr, cc) == channel {
                            neighbours.push(src[rr * w + cc]);
                        }
                    }
                }
                neighbours.sort_unstable_by(f32::total_cmp);
                let median = neighbours[neighbours.len() / 2];
                let q1 = neighbours[neighbours.len() / 4];
                let q3 = neighbours[neighbours.len() * 3 / 4];
                // A real edge has a broad same-colour neighbourhood and must
                // not be mistaken for a defective sensor site.
                if q3 - q1 > median.abs() * 0.35 + 0.01 {
                    return None;
                }
                let confirmed =
                    (median > 0.005 && center < median * 0.20) || center > median * 4.0 + 0.02;
                confirmed.then_some((i, median))
            })
        })
        .collect();
    for (i, value) in updates {
        plane[i] = value;
    }
}

/// Stage boundary for isolated Bayer defects. The disabled path returns before
/// touching the buffer, which is pinned by a bit-exact neutral no-op test.
fn apply_isolated_bayer_defect_stage(
    plane: &mut [f32],
    w: usize,
    h: usize,
    cfa: &rawloader::CFA,
    stage: SensorCorrectionStage,
) {
    if !stage.enabled {
        return;
    }
    debug_assert_eq!(stage.reason, SensorCorrectionReason::BayerDefectBaseline);
    correct_isolated_bayer_defects(plane, w, h, cfa);
}

/// Opposed highlight reconstruction on the white-balanced Bayer plane, BEFORE
/// demosaic — the interpolators then see smooth reconstructed values instead of
/// a flat cap. No-op when nothing is clipped.
fn inpaint_opposed_bayer(
    plane: &mut [f32],
    w: usize,
    h: usize,
    cfa: &rawloader::CFA,
    gain: [f32; 4],
) {
    if !HIGHLIGHT_RECOVERY {
        return;
    }
    let clips = clip_levels(gain);
    let src: &[f32] = plane;

    // Clip state per site as a channel bitmask (a Bayer site carries one colour).
    let clipped: Vec<u8> = (0..w * h)
        .into_par_iter()
        .map(|i| {
            let col = cfa.color_at(i / w, i % w);
            let k = chroma_channel(col);
            if col < 4 && k < 3 && src[i] >= clips[col] {
                1u8 << k
            } else {
                0
            }
        })
        .collect();
    if !clipped.par_iter().any(|&c| c != 0) {
        return;
    }
    let near = dilate_bitmask(&clipped, w, h, OPPOSED_RADIUS);

    // Per-channel chromatic offset, measured on the border candidates: sites
    // unclipped in their own channel but adjacent to a clipped site of the SAME
    // channel.
    let (sums, cnts) = (0..h)
        .into_par_iter()
        .map(|row| {
            let mut s = [0.0f64; 3];
            let mut n = [0u64; 3];
            for col in 0..w {
                let i = row * w + col;
                if clipped[i] != 0 {
                    continue;
                }
                let k = chroma_channel(cfa.color_at(row, col));
                if k >= 3 || near[i] & (1 << k) == 0 {
                    continue;
                }
                let ra = opposed_refavg_bayer(src, w, h, cfa, row as i32, col as i32, k);
                s[k] += (croot(src[i]) - ra) as f64;
                n[k] += 1;
            }
            (s, n)
        })
        .reduce(
            || ([0.0f64; 3], [0u64; 3]),
            |(mut sa, mut na), (sb, nb)| {
                for k in 0..3 {
                    sa[k] += sb[k];
                    na[k] += nb[k];
                }
                (sa, na)
            },
        );
    let mut chrom = [0.0f32; 3];
    for k in 0..3 {
        if cnts[k] > 0 {
            chrom[k] = (sums[k] / cnts[k] as f64) as f32;
        }
    }

    // Inpaint every clipped site from the ORIGINAL plane values, never darker
    // than the measured (capped) sample; apply the updates afterwards.
    let clipped: &[u8] = &clipped;
    let updates: Vec<(usize, f32)> = (0..h)
        .into_par_iter()
        .flat_map_iter(|row| {
            (0..w).filter_map(move |col| {
                let i = row * w + col;
                if clipped[i] == 0 {
                    return None;
                }
                let k = clipped[i].trailing_zeros() as usize;
                let ra = opposed_refavg_bayer(src, w, h, cfa, row as i32, col as i32, k);
                let rec = (ra + chrom[k]).max(0.0).powi(3);
                Some((i, src[i].max(rec)))
            })
        })
        .collect();
    for (i, v) in updates {
        plane[i] = v;
    }
}

/// Opposed reconstruction state for already-demosaiced (cpp = 3) RAWs: per-pixel
/// clipped-channel bitmask plus the global cube-root chromatic offsets. All
/// channels are present per pixel, so the opposed reference is the pixel's own
/// other two channels.
struct OpposedRgb {
    clipped: Vec<u8>,
    chrom: [f32; 3],
}

impl OpposedRgb {
    /// Inpaint the clipped channels of one white-balanced camera pixel.
    #[inline]
    fn reconstruct(&self, idx: usize, cam: &mut [f32; 3]) {
        let bits = self.clipped[idx];
        if bits == 0 {
            return;
        }
        let orig = *cam;
        for k in 0..3 {
            if bits & (1 << k) != 0 {
                let ra = 0.5 * (croot(orig[(k + 1) % 3]) + croot(orig[(k + 2) % 3]));
                let rec = (ra + self.chrom[k]).max(0.0).powi(3);
                cam[k] = cam[k].max(rec);
            }
        }
    }
}

/// Scan a cpp = 3 RAW for clipped channels and measure the opposed chromatic
/// offsets from the unclipped pixels bordering each clipped region. `None` when
/// nothing is clipped (or recovery is disabled).
fn opposed_rgb(
    data: &RawImageData,
    w: usize,
    h: usize,
    levels: &RawLevels,
    gain: [f32; 4],
) -> Option<OpposedRgb> {
    if !HIGHLIGHT_RECOVERY {
        return None;
    }
    let clips = clip_levels(gain);
    let norm = |idx: usize, c: usize| -> f32 {
        ((raw_value(data, idx) - levels.black[c]) / levels.denom[c]).max(0.0) * gain[c]
    };
    let clipped: Vec<u8> = (0..w * h)
        .into_par_iter()
        .map(|i| {
            let mut bits = 0u8;
            for c in 0..3 {
                if norm(i * 3 + c, c) >= clips[c] {
                    bits |= 1 << c;
                }
            }
            bits
        })
        .collect();
    if !clipped.par_iter().any(|&b| b != 0) {
        return None;
    }
    let near = dilate_bitmask(&clipped, w, h, OPPOSED_RADIUS);
    let (sums, cnts) = (0..h)
        .into_par_iter()
        .map(|row| {
            let mut s = [0.0f64; 3];
            let mut n = [0u64; 3];
            for col in 0..w {
                let i = row * w + col;
                let cand = near[i] & !clipped[i];
                if cand == 0 {
                    continue;
                }
                let px = [norm(i * 3, 0), norm(i * 3 + 1, 1), norm(i * 3 + 2, 2)];
                for k in 0..3 {
                    if cand & (1 << k) != 0 {
                        let ra = 0.5 * (croot(px[(k + 1) % 3]) + croot(px[(k + 2) % 3]));
                        s[k] += (croot(px[k]) - ra) as f64;
                        n[k] += 1;
                    }
                }
            }
            (s, n)
        })
        .reduce(
            || ([0.0f64; 3], [0u64; 3]),
            |(mut sa, mut na), (sb, nb)| {
                for k in 0..3 {
                    sa[k] += sb[k];
                    na[k] += nb[k];
                }
                (sa, na)
            },
        );
    let mut chrom = [0.0f32; 3];
    for k in 0..3 {
        if cnts[k] > 0 {
            chrom[k] = (sums[k] / cnts[k] as f64) as f32;
        }
    }
    Some(OpposedRgb { clipped, chrom })
}

// Capture sharpening — a small, variance-guarded unsharp pass on the LINEAR
// demosaiced scene, compensating the softness of the optical low-pass filter and
// demosaic chain (darktable's "capture sharpen" idea). Runs before the scene
// master is frozen, so the default look and every Develop render inherit it.
// Luminance-only and ratio-preserving: each pixel's RGB scales by one factor, so
// hue/chroma stay put and no per-channel fringing appears. The guard gates on
// RELATIVE local contrast, leaving flat/noisy areas alone instead of amplifying
// their noise. All constants are taste knobs.
// Enabled at a GENTLE setting: one unsharp iteration at a modest gain, so a RAW
// opens with the crisp pore/hair micro-detail every RAW converter applies as a
// capture-sharpen baseline (AHD demosaic + no sharpening renders ~1.2x softer
// than the camera JPEG, which reads as "bệt/mờ"). The earlier default — TWO
// iterations at gain 0.55 — compounded overshoot into dark beads along hair/skin
// edges; halving the iterations and dropping the gain keeps the acutance without
// the beads. Luma-only + the relative-contrast guard leave flat skin/noise
// untouched. The explicit Detail▸Sharpening slider still stacks on top.
const CAPTURE_SHARPEN: bool = true;
const CS_ITERATIONS: usize = 1;
/// Gaussian radius of the unsharp blur, sensor px.
const CS_SIGMA: f32 = 0.7;
/// Per-iteration unsharp gain. Restores acutance toward the camera JPEG (a
/// scene-referred demosaic opens ~25% softer natively). Runs AFTER the default
/// colour NR and is luma-only with a relative-contrast guard, so it lifts real
/// edges without re-amplifying the chroma speckle NR removed. Override:
/// `IAI_CS_GAIN`. The Detail ▸ Sharpening slider still adds more on top.
const CS_GAIN: f32 = 0.60;
/// Relative-contrast guard: fully closed below LO, fully open above HI.
const CS_GUARD_LO: f32 = 0.04;
const CS_GUARD_HI: f32 = 0.15;
/// Level floor in the relative-contrast denominator (damps deep-shadow blowup).
const CS_GUARD_FLOOR: f32 = 0.02;
/// Dark-side (undershoot) gain multiplier, and a floor on the per-pixel factor.
/// Unsharp undershoot dims the dark side of a high-contrast edge; at a strong
/// gain it pushes already-dark edge pixels toward black — the dotted rim a
/// photographer sees along a soft skin↔dark-bokeh edge. Damping the dark-side
/// gain and flooring the factor removes those specks while the bright side keeps
/// the full gain, so real acutance is preserved. Overrides: `IAI_CS_DARK_RATIO`
/// / `IAI_CS_FLOOR`.
///
/// `dark_ratio = 0` = NO dark-side undershoot at all (pure bright-side / overshoot
/// sharpen): the dark side of every edge is left exactly as decoded, so the
/// sharpener can never dim an edge pixel toward black/brown. Measured to bring
/// edge dark-spikes back to the raw-decode baseline while the native acutance is
/// unchanged (the undershoot contributed almost nothing to real sharpness). The
/// floor still bounds the dark side under any nonzero override.
const CS_DARK_RATIO: f32 = 0.0;
const CS_FLOOR: f32 = 0.85;

/// Separable 5-tap blur over an f32 plane, edge-clamped.
fn blur_plane_5(src: &[f32], w: usize, h: usize, k: &[f32; 5]) -> Vec<f32> {
    let mut tmp = vec![0.0f32; w * h];
    tmp.par_chunks_mut(w).enumerate().for_each(|(y, out)| {
        let base = y * w;
        for (x, slot) in out.iter_mut().enumerate() {
            let mut acc = 0.0;
            for (t, kv) in k.iter().enumerate() {
                let xx = (x as i32 + t as i32 - 2).clamp(0, w as i32 - 1) as usize;
                acc += src[base + xx] * kv;
            }
            *slot = acc;
        }
    });
    let mut dst = vec![0.0f32; w * h];
    dst.par_chunks_mut(w).enumerate().for_each(|(y, out)| {
        for (x, slot) in out.iter_mut().enumerate() {
            let mut acc = 0.0;
            for (t, kv) in k.iter().enumerate() {
                let yy = (y as i32 + t as i32 - 2).clamp(0, h as i32 - 1) as usize;
                acc += tmp[yy * w + x] * kv;
            }
            *slot = acc;
        }
    });
    dst
}

/// Capture-sharpen an RGBA f16 scene buffer in place (see the constants above).
/// `gain` is the per-iteration unsharp gain (default [`CS_GAIN`]); `dark_ratio`
/// damps the dark-side undershoot and `floor` bounds how far a pixel may be
/// dimmed, so no edge pixel is crushed toward black.
fn capture_sharpen(half: &mut [u16], w: usize, h: usize, gain: f32, dark_ratio: f32, floor: f32) {
    if w < 4 || h < 4 {
        return;
    }
    let mut k = [0.0f32; 5];
    for (i, kv) in k.iter_mut().enumerate() {
        let d = i as f32 - 2.0;
        *kv = (-d * d / (2.0 * CS_SIGMA * CS_SIGMA)).exp();
    }
    let ks: f32 = k.iter().sum();
    for kv in &mut k {
        *kv /= ks;
    }

    for _ in 0..CS_ITERATIONS {
        let luma: Vec<f32> = half
            .par_chunks(4)
            .map(|px| {
                luma_lin([
                    f16_bits_to_f32(px[0]),
                    f16_bits_to_f32(px[1]),
                    f16_bits_to_f32(px[2]),
                ])
            })
            .collect();
        let blur = blur_plane_5(&luma, w, h, &k);
        half.par_chunks_mut(w * 4).enumerate().for_each(|(y, row)| {
            for x in 0..w {
                let i = y * w + x;
                let l = luma[i];
                if l <= 1e-6 {
                    continue;
                }
                let d = l - blur[i];
                let guard = crate::core::develop::smootherstep(
                    CS_GUARD_LO,
                    CS_GUARD_HI,
                    d.abs() / (blur[i].max(0.0) + CS_GUARD_FLOOR),
                );
                if guard <= 0.0 {
                    continue;
                }
                // Bright side (d>0, overshoot) keeps the full gain for acutance;
                // the dark side (d<0, undershoot) is damped and floored so an edge
                // pixel is never dimmed toward black (the dotted-rim artifact).
                let eff_gain = if d < 0.0 { gain * dark_ratio } else { gain };
                let factor = ((l + eff_gain * guard * d) / l).clamp(floor, 2.0);
                for ch in 0..3 {
                    let v = f16_bits_to_f32(row[x * 4 + ch]);
                    row[x * 4 + ch] = f32_to_f16_bits(v * factor);
                }
            }
        });
    }
}

/// Write a camera-space RGB triple as UNCLAMPED linear sRGB, f16 bits — one
/// pixel of the scene-referred Develop master. Headroom above 1.0 and
/// out-of-gamut values below 0.0 survive; display rendering happens later in
/// `develop_scene`.
#[inline]
/// True when the decoder camera→XYZ matrix has a zero fourth (E) column, i.e. an
/// ordinary three-colour RGB sensor rather than an RGBE/CMY four-colour sensor.
/// The DCP camera transform is a 3×3 RGB map, so it is only offered for these.
fn camera_matrix_is_three_colour(cam2xyz: &[[f32; 4]; 3]) -> bool {
    cam2xyz.iter().all(|row| row[3] == 0.0)
}

/// Quantize a f64 3×3 matrix to f32 once, before the per-pixel loop.
fn quantize_matrix_f32(matrix: &[[f64; 3]; 3]) -> [[f32; 3]; 3] {
    let mut out = [[0.0f32; 3]; 3];
    for row in 0..3 {
        for column in 0..3 {
            out[row][column] = matrix[row][column] as f32;
        }
    }
    out
}

/// Load the explicit `IAI_CAMERA_PROFILE` override DCP, if one is configured and
/// readable. A missing or unreadable override silently yields `None` so the
/// resolver falls through to its next tier.
fn load_explicit_dcp_override() -> Option<discovery::ExplicitProfile> {
    let path = discovery::explicit_profile_override_path()?;
    discovery::load_explicit_dcp(&path).ok()
}

/// Load the per-camera DCP from the default profile directory beside the
/// executable (`camera_profiles/<make>__<model>.dcp`), if present and readable.
/// The resolver's explicit tier re-verifies the camera match before applying it.
fn load_default_camera_dcp(make: &str, model: &str) -> Option<discovery::ExplicitProfile> {
    let path = discovery::default_camera_dcp_path(make, model)?;
    discovery::load_explicit_dcp(&path).ok()
}

/// Extract the technical camera profile embedded in a DNG's IFD0, re-serialized
/// as a standalone DCP. Only `.dng` inputs are probed; a non-DNG, a DNG without a
/// technical profile, or any structural error yields an empty list.
fn load_embedded_dng_dcps(path: &Path) -> Vec<Vec<u8>> {
    let is_dng = path
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("dng"));
    if !is_dng {
        return Vec::new();
    }
    match embedded_dng::extract_technical_dcp_from_file(path) {
        Ok(Some(blob)) => vec![blob],
        _ => Vec::new(),
    }
}

/// Load the DCP-kind profiles from the `IAI_CAMERA_PROFILE_MANIFEST` manifest
/// that exactly match this camera. Scene ICC records stay gated for generic RAW,
/// so they are skipped here. A missing/unreadable manifest yields an empty list;
/// the resolver then falls through to the decoder matrix.
fn load_manifest_dcp_profiles(make: &str, model: &str) -> Vec<discovery::LoadedManifestProfile> {
    let Some(manifest_path) = discovery::manifest_override_path() else {
        return Vec::new();
    };
    let Ok(manifest) = discovery::load_manifest_file(&manifest_path) else {
        return Vec::new();
    };
    let root = manifest_path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let camera = CameraIdentityRef { make, model };
    let mut loaded = Vec::new();
    for matched in manifest.matching_profiles(camera) {
        if matched.kind() != discovery::ProfileKind::Dcp {
            continue;
        }
        if let Ok(profile) = discovery::load_matched_profile(root, &matched) {
            loaded.push(profile);
        }
    }
    loaded
}

/// Per-pixel writer for the RAW scene master. Both variants write UNCLAMPED f16
/// so highlight headroom above 1.0 and out-of-gamut values below 0.0 survive.
enum SceneWriter<'a> {
    /// Bit-exact legacy path: camera RGB → linear sRGB (decoder matrix) → linear
    /// ProPhoto → f16. Unchanged from the pre-resolver decode.
    LegacySrgb { cam2srgb: [[f32; 3]; 3] },
    /// DCP path: camera RGB → linear ProPhoto (quantized post-WB matrix) →
    /// scene-safe technical HueSatMap → f16. Never routed through linear sRGB.
    Dcp {
        matrix: [[f32; 3]; 3],
        hue_sat: Option<PreparedHueSatMap<'a>>,
    },
}

impl SceneWriter<'_> {
    #[inline]
    fn write(&self, dst: &mut [u16], cam: [f32; 3]) {
        match self {
            SceneWriter::LegacySrgb { cam2srgb } => write_scene(dst, cam2srgb, cam),
            SceneWriter::Dcp { matrix, hue_sat } => write_scene_dcp(dst, matrix, *hue_sat, cam),
        }
    }
}

fn write_scene(dst: &mut [u16], m: &[[f32; 3]; 3], cam: [f32; 3]) {
    let srgb = camera_to_linear_srgb(m, cam);
    let working =
        crate::core::working_color::WorkingColorSpace::LinearProPhoto.from_linear_srgb(srgb);
    dst[0] = f32_to_f16_bits(working[0]);
    dst[1] = f32_to_f16_bits(working[1]);
    dst[2] = f32_to_f16_bits(working[2]);
    dst[3] = 0x3c00; // 1.0
}

/// DCP scene writer. `cam` is camera-native RGB with the mosaic white balance
/// already applied, so the post-WB matrix maps it straight to linear ProPhoto.
fn write_scene_dcp(
    dst: &mut [u16],
    matrix: &[[f32; 3]; 3],
    hue_sat: Option<PreparedHueSatMap<'_>>,
    cam: [f32; 3],
) {
    let mut rgb = [
        matrix[0][0] * cam[0] + matrix[0][1] * cam[1] + matrix[0][2] * cam[2],
        matrix[1][0] * cam[0] + matrix[1][1] * cam[1] + matrix[1][2] * cam[2],
        matrix[2][0] * cam[0] + matrix[2][1] * cam[1] + matrix[2][2] * cam[2],
    ];
    if let Some(map) = hue_sat {
        // Signed values bypass (HSV undefined); HDR values keep their magnitude;
        // the LUT is never allowed to clamp the scene.
        if let Ok(result) = map.apply_scene_safe([rgb[0] as f64, rgb[1] as f64, rgb[2] as f64]) {
            rgb = [
                result.rgb[0] as f32,
                result.rgb[1] as f32,
                result.rgb[2] as f32,
            ];
        }
    }
    dst[0] = f32_to_f16_bits(rgb[0]);
    dst[1] = f32_to_f16_bits(rgb[1]);
    dst[2] = f32_to_f16_bits(rgb[2]);
    dst[3] = 0x3c00; // 1.0
}

/// Subsample scene-linear RGB from the f16 RGBA master for exposure analysis.
/// Caps the work at ~16k samples — plenty for a stable mean, and the bisection
/// re-renders them through the (powf-bearing) tone transform each step.
fn subsample_scene_rgb(scene: &[u16]) -> Vec<[f32; 3]> {
    let px = scene.len() / 4;
    if px == 0 {
        return Vec::new();
    }
    let step = (px / 16_000).max(1);
    let mut out = Vec::with_capacity(px / step + 1);
    let mut i = 0;
    while i < px {
        let b = i * 4;
        out.push([
            f16_bits_to_f32(scene[b]),
            f16_bits_to_f32(scene[b + 1]),
            f16_bits_to_f32(scene[b + 2]),
        ]);
        i += step;
    }
    out
}

/// Multiply the scene-linear RGB master by `k` (a baseline exposure, linear in
/// scene space). Alpha (index 3) is untouched. Headroom above 1.0 is preserved —
/// the display sigmoid's shoulder rolls the highlights off later.
fn scale_scene(scene: &mut [u16], k: f32) {
    scale_scene_rgb(scene, [k; 3]);
}

fn scale_scene_rgb(scene: &mut [u16], gain: [f32; 3]) {
    scene.par_chunks_mut(4).for_each(|px| {
        px[0] = f32_to_f16_bits(f16_bits_to_f32(px[0]) * gain[0]);
        px[1] = f32_to_f16_bits(f16_bits_to_f32(px[1]) * gain[1]);
        px[2] = f32_to_f16_bits(f16_bits_to_f32(px[2]) * gain[2]);
    });
}

fn camera_color_matrix_is_material(matrix: [[f32; 3]; 3]) -> bool {
    matrix.iter().enumerate().any(|(row, values)| {
        values
            .iter()
            .enumerate()
            .any(|(col, &v)| (v - f32::from(row == col)).abs() > 0.002)
    })
}

fn transform_scene_rgb(scene: &mut [u16], matrix: [[f32; 3]; 3]) {
    scene.par_chunks_mut(4).for_each(|px| {
        let src = [
            f16_bits_to_f32(px[0]),
            f16_bits_to_f32(px[1]),
            f16_bits_to_f32(px[2]),
        ];
        for row in 0..3 {
            px[row] = f32_to_f16_bits(
                matrix[row][0] * src[0] + matrix[row][1] * src[1] + matrix[row][2] * src[2],
            );
        }
    });
}

/// Peak strength of the default-look midtone vibrance. Measured against the
/// embedded camera JPEG (the target look): a neutral scene-referred render
/// reproduces the JPEG's mean, white point and per-channel tone, but its
/// midtones land ~15% less saturated because the camera bakes a saturation
/// step the per-channel tone-curve match cannot recreate (that match only
/// reshapes each channel's marginal, never the cross-channel spread chroma is
/// made of). This reads as the "nhợt nhạt / mờ đục" (pale, muddy) cast a
/// photographer sees against the camera preview. 0 = off.
const CHROMA_ENRICH: f32 = 0.85;

/// Evaluate the luminance-preserving chroma operator for one scene-linear pixel.
/// Kept pure so the preview-fit search uses exactly the same math as the full
/// scene pass below.
fn enrich_rgb_chroma(rgb: [f32; 3], strength: f32) -> [f32; 3] {
    if strength.abs() < 1e-4 {
        return rgb;
    }
    const LW: [f32; 3] = [0.22, 0.69, 0.09];
    const CENTER: f32 = -2.47;
    const SIGMA: f32 = 1.4;
    let luma = LW[0] * rgb[0] + LW[1] * rgb[1] + LW[2] * rgb[2];
    if luma <= 1e-5 {
        return rgb;
    }
    let mx = rgb[0].max(rgb[1]).max(rgb[2]);
    let mn = rgb[0].min(rgb[1]).min(rgb[2]);
    let sat = if mx > 1e-6 { (mx - mn) / mx } else { 0.0 };
    let protect = (1.0 - sat).clamp(0.0, 1.0);
    let e = luma.max(1e-5).log2();
    let d = (e - CENTER) / SIGMA;
    let mid = (-0.5 * d * d).exp();
    let factor = 1.0 + strength * mid * protect;
    rgb.map(|channel| (luma + (channel - luma) * factor).max(0.0))
}

fn preview_sample_coord(index: u32, source_len: u32, sample_len: u32) -> u32 {
    ((((index as u64 * 2 + 1) * source_len as u64) / (sample_len.max(1) as u64 * 2))
        .min(source_len.saturating_sub(1) as u64)) as u32
}

/// Scalar post-matrix exposure fit against the embedded preview. The bounded
/// geometric search changes brightness only; it cannot introduce a colour cast.
fn fit_preview_luma_gain_for_scene(
    scene: &SceneSource,
    target_luma: f32,
    sample_w: u32,
    sample_h: u32,
) -> f32 {
    if sample_w == 0 || sample_h == 0 || !target_luma.is_finite() {
        return 1.0;
    }
    let tone = crate::core::develop_scene::build_scene_tone_for_scene(&Default::default(), scene);
    let mean = |gain: f32| -> f32 {
        let mut sum = 0.0f64;
        let mut count = 0u64;
        for y in 0..sample_h {
            let sy = preview_sample_coord(y, scene.height, sample_h);
            for x in 0..sample_w {
                let sx = preview_sample_coord(x, scene.width, sample_w);
                let rgb = scene.get_rgb(sx, sy).map(|v| v * gain);
                let d = tone.scene_to_display(rgb, None);
                sum += (0.2126 * d[0] + 0.7152 * d[1] + 0.0722 * d[2]) as f64;
                count += 1;
            }
        }
        if count == 0 {
            0.0
        } else {
            (sum / count as f64) as f32
        }
    };
    let target = target_luma.clamp(0.02, 0.98);
    let (mut lo, mut hi) = (0.25f32, 4.0f32);
    if mean(lo) >= target {
        return lo;
    }
    if mean(hi) <= target {
        return hi;
    }
    for _ in 0..16 {
        let mid = (lo * hi).sqrt();
        if mean(mid) < target {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    (lo * hi).sqrt()
}

fn display_oklab_chroma(encoded: [f32; 3]) -> f32 {
    let linear = encoded.map(crate::core::develop::srgb_to_linear);
    let lab = crate::core::perceptual_color::linear_srgb_to_oklab(linear);
    lab.a.hypot(lab.b)
}

/// Fit the existing chroma enrichment strength to the camera preview while
/// bounding the requested chroma change to -15%/+25%. This resists extreme
/// picture styles and bad/misaligned preview samples.
fn fit_preview_chroma_strength(
    scene: &SceneSource,
    target: &[[u8; 3]],
    sample_w: u32,
    sample_h: u32,
) -> f32 {
    if sample_w == 0 || sample_h == 0 || target.len() != sample_w as usize * sample_h as usize {
        return 0.0;
    }
    let tone = crate::core::develop_scene::build_scene_tone_for_scene(&Default::default(), scene);
    let mut selected = Vec::with_capacity(target.len());
    let mut target_sum = 0.0f64;
    for y in 0..sample_h {
        let sy = preview_sample_coord(y, scene.height, sample_h);
        for x in 0..sample_w {
            let t = target[(y * sample_w + x) as usize].map(|v| v as f32 / 255.0);
            let luma = 0.2126 * t[0] + 0.7152 * t[1] + 0.0722 * t[2];
            if !(0.03..=0.97).contains(&luma) {
                continue;
            }
            let sx = preview_sample_coord(x, scene.width, sample_w);
            selected.push(scene.get_rgb(sx, sy));
            target_sum += display_oklab_chroma(t) as f64;
        }
    }
    if selected.len() < 24 {
        return 0.0;
    }
    let mean_for = |strength: f32| -> f32 {
        selected
            .iter()
            .map(|&rgb| {
                display_oklab_chroma(tone.scene_to_display(enrich_rgb_chroma(rgb, strength), None))
            })
            .sum::<f32>()
            / selected.len() as f32
    };
    let base = mean_for(0.0).max(1e-5);
    let measured_target = (target_sum / selected.len() as f64) as f32;
    let target = measured_target.clamp(base * 0.85, base * 1.25);
    let (mut lo, mut hi) = (-0.25f32, 1.5f32);
    if mean_for(lo) >= target {
        return lo;
    }
    if mean_for(hi) <= target {
        return hi;
    }
    for _ in 0..14 {
        let mid = 0.5 * (lo + hi);
        if mean_for(mid) < target {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    0.5 * (lo + hi)
}

/// Luminance-anchored vibrance on the LINEAR scene master, applied once at
/// import so the default look, the GPU preview (scene master upload) and the
/// CPU commit inherit one identical enrichment — no per-stage mirroring, and
/// neutrals stay bit-exact (a grey pixel has zero chroma, so it is untouched;
/// the neutral/parity goldens are unaffected). Midtone-weighted and
/// saturation-protected so it lifts the muddy midtones toward the camera look
/// WITHOUT pushing the already-saturated deep shadows further (they measured
/// slightly over the JPEG), and hue-preserving because every channel scales
/// around the same held luma.
fn enrich_scene_chroma(scene: &mut [u16], strength: f32) {
    if strength.abs() < 1e-4 {
        return;
    }
    scene.par_chunks_mut(4).for_each(|px| {
        let rgb = [
            f16_bits_to_f32(px[0]),
            f16_bits_to_f32(px[1]),
            f16_bits_to_f32(px[2]),
        ];
        let enriched = enrich_rgb_chroma(rgb, strength);
        for (dst, value) in px[..3].iter_mut().zip(enriched) {
            *dst = f32_to_f16_bits(value);
        }
    });
}

/// Default false-colour-suppression iterations applied to every RAW scene master
/// at import (0 = off). The Malvar demosaic (the fallback for images past the AHD
/// pixel cap — i.e. most full-frame RAW) fringes at high-contrast edges: it emits
/// isolated colour specks (e.g. a reddish-brown dotted rim along a skin↔dark
/// edge) that the camera JPEG / Photoshop do not, because they run a chroma
/// median. This is a Freeman-style median on the R−G / B−G colour-difference
/// planes: green (the densest Bayer channel, so the cleanest) is kept exactly, so
/// luminance detail is preserved, while the colour differences are median-filtered
/// to drop the isolated edge specks. Runs FIRST, before the à-trous colour NR and
/// capture sharpen. With AHD now demosaicing full-frame RAW (which removes most
/// of the edge zipper at the source), this is a light residual clean-up for any
/// isolated specks left behind. Override: `IAI_SCENE_FALSE_COLOR`.
const SCENE_FALSE_COLOR_ITERS: usize = 2;

/// Read an integer tuning knob from the environment, falling back to `default`.
fn env_usize(name: &str, default: usize) -> usize {
    std::env::var(name)
        .ok()
        .and_then(|value| value.trim().parse::<usize>().ok())
        .unwrap_or(default)
}

/// 3×3 median of an f32 plane, edge-clamped, parallel over rows.
fn median3x3_plane(src: &[f32], w: usize, h: usize) -> Vec<f32> {
    let mut out = vec![0.0f32; w * h];
    out.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
        for (x, slot) in row.iter_mut().enumerate() {
            let mut v = [0.0f32; 9];
            let mut k = 0;
            for dy in -1i32..=1 {
                for dx in -1i32..=1 {
                    let ny = (y as i32 + dy).clamp(0, h as i32 - 1) as usize;
                    let nx = (x as i32 + dx).clamp(0, w as i32 - 1) as usize;
                    v[k] = src[ny * w + nx];
                    k += 1;
                }
            }
            v.sort_unstable_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
            *slot = v[4];
        }
    });
    out
}

/// False-colour suppression on the linear RGBA-f16 scene master. Keeps green
/// exactly and median-filters the R−G / B−G colour-difference planes, so isolated
/// demosaic colour specks at edges are removed while luminance detail is untouched
/// (green carries most of it, and a median preserves real colour edges — only the
/// outliers are replaced). `iterations` widens the effective support for denser
/// speckle. Neutral pixels have R−G = B−G = 0, so they are unaffected.
fn suppress_false_color(scene: &mut [u16], w: usize, h: usize, iterations: usize) {
    if iterations == 0 || w < 3 || h < 3 {
        return;
    }
    let n = w * h;
    let g: Vec<f32> = (0..n).map(|i| f16_bits_to_f32(scene[i * 4 + 1])).collect();
    let mut cr: Vec<f32> = (0..n)
        .map(|i| f16_bits_to_f32(scene[i * 4]) - g[i])
        .collect();
    let mut cb: Vec<f32> = (0..n)
        .map(|i| f16_bits_to_f32(scene[i * 4 + 2]) - g[i])
        .collect();
    for _ in 0..iterations {
        cr = median3x3_plane(&cr, w, h);
        cb = median3x3_plane(&cb, w, h);
    }
    scene.par_chunks_mut(4).enumerate().for_each(|(i, px)| {
        px[0] = f32_to_f16_bits((g[i] + cr[i]).max(0.0));
        px[2] = f32_to_f16_bits((g[i] + cb[i]).max(0.0));
        // Green (px[1]) and alpha (px[3]) are left exactly as decoded.
    });
}

/// Default colour-noise reduction strength applied to every RAW scene master at
/// import (0 = off). A scene-referred demosaic leaves ~1.8× the embedded JPEG's
/// hi-frequency chroma noise (the camera JPEG runs its own colour NR), which the
/// photographer sees as "lấm tấm" speckle and, once colour is pushed toward the
/// camera look, as "loang màu" chroma blotch. This bakes a clean-up into the
/// master BEFORE capture sharpen and before any chroma enrichment, so speckle is
/// never re-amplified and the later warmth/chroma steps stay clean. Mirrors the
/// Detail-stage colour NR semantics; the user's Detail ▸ Colour NR slider still
/// adds more on top.
const SCENE_COLOR_NR: f32 = 0.85;
/// Per-à-trous-level chroma attenuation at full strength: kill the finest colour
/// speckle outright, keep progressively more of the coarser (real-colour) scales
/// so saturated regions are not desaturated. Same shape as the Detail stage's
/// `CHROMA_NR_ATTEN`.
const SCENE_CHROMA_NR_ATTEN: [f32; 3] = [1.0, 0.85, 0.55];

/// One separable à-trous B3-spline smoothing pass at hole spacing `1 << level`,
/// edge-clamped, parallel over rows. Plain (not edge-aware) taps: a single-pixel
/// colour speck must be smoothed, not "protected" as an edge.
fn atrous_smooth_plane(src: &[f32], w: usize, h: usize, level: usize) -> Vec<f32> {
    const B3: [f32; 5] = [1.0 / 16.0, 4.0 / 16.0, 6.0 / 16.0, 4.0 / 16.0, 1.0 / 16.0];
    let step = 1i64 << level;
    let mut tmp = vec![0.0f32; w * h];
    tmp.par_chunks_mut(w).enumerate().for_each(|(y, out)| {
        let base = y * w;
        for (x, slot) in out.iter_mut().enumerate() {
            let mut acc = 0.0f32;
            for (t, &kv) in B3.iter().enumerate() {
                let o = (t as i64 - 2) * step;
                let sx = (x as i64 + o).clamp(0, w as i64 - 1) as usize;
                acc += src[base + sx] * kv;
            }
            *slot = acc;
        }
    });
    let mut dst = vec![0.0f32; w * h];
    dst.par_chunks_mut(w).enumerate().for_each(|(y, out)| {
        for (x, slot) in out.iter_mut().enumerate() {
            let mut acc = 0.0f32;
            for (t, &kv) in B3.iter().enumerate() {
                let o = (t as i64 - 2) * step;
                let sy = (y as i64 + o).clamp(0, h as i64 - 1) as usize;
                acc += tmp[sy * w + x] * kv;
            }
            *slot = acc;
        }
    });
    dst
}

/// Whole-frame colour-noise reduction on the linear RGBA-f16 scene master. Splits
/// each pixel into luminance + chroma offsets (the chroma part carries zero luma
/// by construction, so this cannot shift brightness or luma detail), à-trous
/// shrinks the finest chroma detail levels, and recomposes. Neutral pixels have
/// zero chroma so they are untouched.
fn denoise_scene_chroma(scene: &mut [u16], w: usize, h: usize, strength: f32) {
    if strength.abs() < 1e-4 || w < 8 || h < 8 {
        return;
    }
    const LW: [f32; 3] = [0.22, 0.69, 0.09];
    let n = w * h;
    // Decode once into luma + three chroma planes.
    let mut luma = vec![0.0f32; n];
    let mut chroma = [vec![0.0f32; n], vec![0.0f32; n], vec![0.0f32; n]];
    scene
        .par_chunks(4)
        .zip(luma.par_iter_mut())
        .enumerate()
        .for_each(|(_, (px, l))| {
            let rgb = [
                f16_bits_to_f32(px[0]),
                f16_bits_to_f32(px[1]),
                f16_bits_to_f32(px[2]),
            ];
            *l = LW[0] * rgb[0] + LW[1] * rgb[1] + LW[2] * rgb[2];
        });
    for (ch, plane) in chroma.iter_mut().enumerate() {
        plane
            .par_iter_mut()
            .enumerate()
            .for_each(|(i, slot)| *slot = f16_bits_to_f32(scene[i * 4 + ch]) - luma[i]);
    }
    // À-trous shrink each chroma plane in place: subtract the attenuated fine
    // detail of each level (memory-light: only prev/next/accumulator live).
    for plane in chroma.iter_mut() {
        let mut prev = plane.clone();
        for (level, &atten) in SCENE_CHROMA_NR_ATTEN.iter().enumerate() {
            let next = atrous_smooth_plane(&prev, w, h, level);
            let k = strength * atten;
            plane
                .par_iter_mut()
                .zip(prev.par_iter())
                .zip(next.par_iter())
                .for_each(|((v, &p), &nx)| *v -= k * (p - nx));
            prev = next;
        }
    }
    // Recompose luma + denoised chroma back into the scene master.
    scene.par_chunks_mut(4).enumerate().for_each(|(i, px)| {
        let l = luma[i];
        for ch in 0..3 {
            px[ch] = f32_to_f16_bits((l + chroma[ch][i]).max(0.0));
        }
    });
}

// ── Clean warm default-look shaping (non-full-fit RAW paths) ──────────────────
//
// These shape the DCP "gain-only" and no-match paths toward the embedded-JPEG
// look on the already-denoised master. All are env-overridable so the headless
// look probe can fit them to the measured target (see tests/raw_look_probe.rs)
// without a rebuild; the const is the shipped default.

/// Small pre-tone brightness lift toward the embedded-JPEG tone: the gain-only
/// render lands a few % darker (measured Oklab L 0.636 vs the JPEG's 0.659).
/// 1.0 = off. Override: `IAI_SCENE_BRIGHTNESS`.
const SCENE_BRIGHTNESS: f32 = 1.10;
/// Flat overall vibrance restored on the clean master (measured overall/midtone
/// chroma ~5% low vs the JPEG). Override: `IAI_SCENE_CHROMA_BASE`.
const SCENE_CHROMA_BASE: f32 = 0.10;
/// Extra vibrance in the deep shadows, where the camera matrix + tone curve
/// desaturate the most (measured −40% shadow chroma vs the JPEG). Ramps in below
/// `CHROMA_SHADOW_HIGH_EV` to full strength by `CHROMA_SHADOW_LOW_EV`. The window
/// is kept in the true shadows so it restores the muddy darks without inflating
/// midtone saturation. Override: `IAI_SCENE_CHROMA_SHADOW`.
const SCENE_CHROMA_SHADOW: f32 = 1.10;
/// Shadow window (EV of scene luma) for the shadow vibrance ramp: fitted to the
/// display shadow zone (Oklab L < 0.25) so the boost lands where the render lost
/// colour, without spilling into the midtones. Overrides:
/// `IAI_SCENE_SHADOW_LOW_EV` / `IAI_SCENE_SHADOW_HIGH_EV`.
const CHROMA_SHADOW_LOW_EV: f32 = -6.0;
const CHROMA_SHADOW_HIGH_EV: f32 = -3.3;
/// Luma-preserving warm tilt (R up / B down). Small by default: the measured skin
/// hue already matches the JPEG, so this only nudges neutrals off the profile
/// matrix's cool-olive cast. 0 = off. Override: `IAI_SCENE_WARM`.
const SCENE_WARM: f32 = 0.0;

/// All non-technical decode-time shaping which is currently baked into a RAW
/// scene master. Centralising it behind a named version is the Q1 migration
/// boundary: callers can audit a genuinely neutral master without coordinating
/// seven independent environment variables, while the default v1 remains exact.
#[derive(Clone, Copy, Debug, PartialEq)]
struct RawRenderRecipe {
    version: RawRenderRecipeVersion,
    scene_color_nr: f32,
    capture_sharpen_gain: f32,
    capture_sharpen_dark_ratio: f32,
    capture_sharpen_floor: f32,
    chroma_enrich: f32,
    scene_brightness: f32,
    scene_chroma_base: f32,
    scene_chroma_shadow: f32,
    chroma_shadow_low_ev: f32,
    chroma_shadow_high_ev: f32,
    scene_warm: f32,
}

impl RawRenderRecipe {
    /// Exact pre-Q1 shipping behaviour, including the existing tuning env vars.
    fn legacy_baked_v1() -> Self {
        Self {
            version: RawRenderRecipeVersion::LegacyBaked1,
            scene_color_nr: env_f32("IAI_SCENE_COLOR_NR", SCENE_COLOR_NR),
            capture_sharpen_gain: if CAPTURE_SHARPEN {
                env_f32("IAI_CS_GAIN", CS_GAIN)
            } else {
                0.0
            },
            capture_sharpen_dark_ratio: env_f32("IAI_CS_DARK_RATIO", CS_DARK_RATIO),
            capture_sharpen_floor: env_f32("IAI_CS_FLOOR", CS_FLOOR),
            chroma_enrich: env_f32("IAI_SCENE_CHROMA_ENRICH", CHROMA_ENRICH),
            scene_brightness: env_f32("IAI_SCENE_BRIGHTNESS", SCENE_BRIGHTNESS),
            scene_chroma_base: env_f32("IAI_SCENE_CHROMA_BASE", SCENE_CHROMA_BASE),
            scene_chroma_shadow: env_f32("IAI_SCENE_CHROMA_SHADOW", SCENE_CHROMA_SHADOW),
            chroma_shadow_low_ev: env_f32("IAI_SCENE_SHADOW_LOW_EV", CHROMA_SHADOW_LOW_EV),
            chroma_shadow_high_ev: env_f32("IAI_SCENE_SHADOW_HIGH_EV", CHROMA_SHADOW_HIGH_EV),
            scene_warm: env_f32("IAI_SCENE_WARM", SCENE_WARM),
        }
    }

    /// Technical decode only: sensor correction, demosaic, false-colour
    /// suppression, camera characterization, WB and level normalization remain;
    /// global denoise/sharpen and creative brightness/chroma/warmth are identity.
    fn technical_neutral_v2() -> Self {
        Self {
            version: RawRenderRecipeVersion::TechnicalNeutral2,
            scene_color_nr: 0.0,
            capture_sharpen_gain: 0.0,
            capture_sharpen_dark_ratio: 0.0,
            capture_sharpen_floor: 1.0,
            chroma_enrich: 0.0,
            scene_brightness: 1.0,
            scene_chroma_base: 0.0,
            scene_chroma_shadow: 0.0,
            chroma_shadow_low_ev: CHROMA_SHADOW_LOW_EV,
            chroma_shadow_high_ev: CHROMA_SHADOW_HIGH_EV,
            scene_warm: 0.0,
        }
    }

    fn resolve() -> Self {
        match std::env::var("IAI_RAW_RENDER_RECIPE")
            .ok()
            .as_deref()
            .map(str::trim)
        {
            Some("technical") | Some("technical-neutral") | Some("v2") => {
                Self::technical_neutral_v2()
            }
            _ => Self::legacy_baked_v1(),
        }
    }
}

/// Read a float tuning knob from the environment, falling back to `default`.
fn env_f32(name: &str, default: f32) -> f32 {
    std::env::var(name)
        .ok()
        .and_then(|value| value.trim().parse::<f32>().ok())
        .unwrap_or(default)
}

/// Shadow-weighted vibrance on the linear scene master: a flat `base` everywhere
/// plus extra `shadow` where the tone/matrix chain desaturated the darks. Luma is
/// held constant (hue-preserving), and already-vivid pixels are protected, so
/// neutrals and saturated colour are left alone while muddy shadows regain chroma.
fn enrich_scene_chroma_shadow(
    scene: &mut [u16],
    base: f32,
    shadow: f32,
    low_ev: f32,
    high_ev: f32,
) {
    if base.abs() < 1e-4 && shadow.abs() < 1e-4 {
        return;
    }
    const LW: [f32; 3] = [0.22, 0.69, 0.09];
    scene.par_chunks_mut(4).for_each(|px| {
        let rgb = [
            f16_bits_to_f32(px[0]),
            f16_bits_to_f32(px[1]),
            f16_bits_to_f32(px[2]),
        ];
        let luma = LW[0] * rgb[0] + LW[1] * rgb[1] + LW[2] * rgb[2];
        if luma <= 1e-5 {
            return;
        }
        let mx = rgb[0].max(rgb[1]).max(rgb[2]);
        let mn = rgb[0].min(rgb[1]).min(rgb[2]);
        let sat = if mx > 1e-6 { (mx - mn) / mx } else { 0.0 };
        let protect = (1.0 - sat).clamp(0.0, 1.0);
        // Shadow weight: 1 in the deep shadows, ramping to 0 by the high EV.
        let e = luma.max(1e-5).log2();
        let sw = 1.0 - crate::core::develop::smootherstep(low_ev, high_ev, e);
        let strength = base + shadow * sw;
        let factor = 1.0 + strength * protect;
        for c in 0..3 {
            px[c] = f32_to_f16_bits((luma + (rgb[c] - luma) * factor).max(0.0));
        }
    });
}

/// Luma-preserving warm tilt: scale R up and B down by `w`, then rescale to hold
/// the original luminance so only the colour balance shifts, not brightness.
fn warm_scene(scene: &mut [u16], w: f32) {
    if w.abs() < 1e-4 {
        return;
    }
    const LW: [f32; 3] = [0.22, 0.69, 0.09];
    scene.par_chunks_mut(4).for_each(|px| {
        let rgb = [
            f16_bits_to_f32(px[0]),
            f16_bits_to_f32(px[1]),
            f16_bits_to_f32(px[2]),
        ];
        let l0 = LW[0] * rgb[0] + LW[1] * rgb[1] + LW[2] * rgb[2];
        if l0 <= 1e-6 {
            return;
        }
        let mut out = [rgb[0] * (1.0 + w), rgb[1], rgb[2] * (1.0 - w)];
        let l1 = LW[0] * out[0] + LW[1] * out[1] + LW[2] * out[2];
        if l1 > 1e-6 {
            let k = l0 / l1;
            for c in &mut out {
                *c *= k;
            }
        }
        for c in 0..3 {
            px[c] = f32_to_f16_bits(out[c].max(0.0));
        }
    });
}

/// Standard sRGB opto-electronic transfer function (linear → encoded). Kept for
/// the decode diagnosis tests.
#[cfg(test)]
fn linear_to_srgb(c: f32) -> f32 {
    if c <= 0.0031308 {
        12.92 * c
    } else {
        1.055 * c.powf(1.0 / 2.4) - 0.055
    }
}

/// Remap an RGBA16 buffer to upright orientation per the EXIF tag. Expressed as an
/// optional transpose followed by horizontal/vertical flips in the destination.
fn apply_orientation(
    src: Vec<u16>,
    w: usize,
    h: usize,
    o: Orientation,
) -> (Vec<u16>, usize, usize) {
    let (swap, fx, fy) = match o {
        Orientation::Normal | Orientation::Unknown => (false, false, false),
        Orientation::HorizontalFlip => (false, true, false),
        Orientation::Rotate180 => (false, true, true),
        Orientation::VerticalFlip => (false, false, true),
        Orientation::Transpose => (true, false, false),
        Orientation::Rotate90 => (true, true, false),
        Orientation::Transverse => (true, true, true),
        Orientation::Rotate270 => (true, false, true),
    };
    if !swap && !fx && !fy {
        return (src, w, h);
    }
    let (dw, dh) = if swap { (h, w) } else { (w, h) };
    let mut dst = vec![0u16; dw * dh * 4];
    for sy in 0..h {
        for sx in 0..w {
            let (mut dx, mut dy) = if swap { (sy, sx) } else { (sx, sy) };
            if fx {
                dx = dw - 1 - dx;
            }
            if fy {
                dy = dh - 1 - dy;
            }
            let s = (sy * w + sx) * 4;
            let d = (dy * dw + dx) * 4;
            dst[d..d + 4].copy_from_slice(&src[s..s + 4]);
        }
    }
    (dst, dw, dh)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jpeg_match_plan_splits_matrix_and_curves_without_env_state() {
        assert_eq!(
            jpeg_match_plan_for(Some("matrix"), false),
            JpegMatchPlan {
                apply_gain: true,
                apply_matrix: true,
                apply_curves: false,
                preview_safe: false,
            }
        );
        assert_eq!(
            jpeg_match_plan_for(Some("curves"), false),
            JpegMatchPlan {
                apply_gain: true,
                apply_matrix: false,
                apply_curves: true,
                preview_safe: false,
            }
        );
        assert_eq!(
            jpeg_match_plan_for(Some("off"), false),
            JpegMatchPlan {
                apply_gain: true,
                apply_matrix: false,
                apply_curves: false,
                preview_safe: false,
            }
        );
        assert_eq!(
            jpeg_match_plan_for(None, true),
            JpegMatchPlan {
                apply_gain: true,
                apply_matrix: false,
                apply_curves: false,
                preview_safe: false,
            }
        );
        assert_eq!(
            jpeg_match_plan_for(None, false),
            JpegMatchPlan {
                apply_gain: true,
                apply_matrix: true,
                apply_curves: true,
                preview_safe: false,
            }
        );
        assert_eq!(
            jpeg_match_plan_for(Some("safe"), false),
            JpegMatchPlan {
                apply_gain: true,
                apply_matrix: true,
                apply_curves: false,
                preview_safe: true,
            }
        );
    }

    #[test]
    fn preview_safe_fit_recovers_brightness_and_chroma_without_rgb_curves() {
        let (w, h) = (24u32, 24u32);
        let mut scene = SceneSource::new(w, h);
        for y in 0..h {
            for x in 0..w {
                let fx = x as f32 / (w - 1) as f32;
                let fy = y as f32 / (h - 1) as f32;
                scene.set_rgb(
                    x,
                    y,
                    [
                        0.035 + 0.30 * fx,
                        0.045 + 0.24 * fy,
                        0.040 + 0.20 * (1.0 - fx * fy),
                    ],
                );
            }
        }
        let tone =
            crate::core::develop_scene::build_scene_tone_for_scene(&Default::default(), &scene);
        let expected_gain = 1.35f32;
        let expected_chroma = 0.70f32;
        let mut target = Vec::with_capacity((w * h) as usize);
        let mut target_luma = 0.0f32;
        for y in 0..h {
            for x in 0..w {
                let rgb = scene.get_rgb(x, y).map(|v| v * expected_gain);
                let d = tone.scene_to_display(enrich_rgb_chroma(rgb, expected_chroma), None);
                target_luma += 0.2126 * d[0] + 0.7152 * d[1] + 0.0722 * d[2];
                target.push(d.map(|v| (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8));
            }
        }
        target_luma /= (w * h) as f32;

        let fitted_gain = fit_preview_luma_gain_for_scene(&scene, target_luma, w, h);
        assert!((1.05..=1.6).contains(&fitted_gain));
        let fitted_luma = (0..h)
            .flat_map(|y| (0..w).map(move |x| (x, y)))
            .map(|(x, y)| {
                let d = tone.scene_to_display(scene.get_rgb(x, y).map(|v| v * fitted_gain), None);
                0.2126 * d[0] + 0.7152 * d[1] + 0.0722 * d[2]
            })
            .sum::<f32>()
            / (w * h) as f32;
        assert!((fitted_luma - target_luma).abs() < 0.005);
        scale_scene(&mut scene.half, fitted_gain);
        let fitted_chroma = fit_preview_chroma_strength(&scene, &target, w, h);
        assert!(
            (fitted_chroma - expected_chroma).abs() < 0.20,
            "chroma {fitted_chroma} vs {expected_chroma}"
        );
    }

    #[test]
    fn white_level_source_stays_in_step_with_the_chooser() {
        // (reported, black, observed): trusted, missing/degenerate, container-max,
        // and high-observed (reported kept despite the 16-bit container value).
        let cases = [
            (16_383.0_f32, 512.0_f32, 15_000.0_f32),
            (0.0, 512.0, 9_000.0),
            (65_535.0, 512.0, 12_000.0),
            (65_535.0, 2_000.0, 40_000.0),
        ];
        for (reported, black, observed) in cases {
            let effective = choose_effective_white_level(reported, black, observed);
            let observed_floor = observed.max(black + 1.0);
            match white_level_source(reported, black, observed) {
                WhiteLevelSource::Reported => assert_eq!(
                    effective, reported,
                    "reported-trusted must equal the reported level"
                ),
                WhiteLevelSource::MissingReplacedByObserved
                | WhiteLevelSource::ContainerMaxReplacedByObserved => assert_eq!(
                    effective, observed_floor,
                    "a fallback source must equal the observed maximum"
                ),
            }
        }
    }

    #[test]
    fn rawler_fallback_prefers_a_valid_d65_matrix_deterministically() {
        use rawler::imgop::xyz::Illuminant;

        let tungsten = vec![0.9, 0.1, 0.0, 0.2, 0.7, 0.1, 0.0, 0.1, 0.9];
        let d65 = vec![0.6, 0.3, 0.1, 0.1, 0.8, 0.1, 0.0, 0.2, 0.8];
        let mut matrices = std::collections::HashMap::new();
        matrices.insert(Illuminant::A, tungsten);
        matrices.insert(Illuminant::D65, d65.clone());

        let selected = select_rawler_xyz_to_cam(&matrices, [[0.0; 3]; 4], false).unwrap();
        assert_eq!(selected[0], d65[0..3]);
        assert_eq!(selected[1], d65[3..6]);
        assert_eq!(selected[2], d65[6..9]);

        // HashMap insertion/iteration order is not part of the decision.
        let mut reversed = std::collections::HashMap::new();
        reversed.insert(Illuminant::D65, d65);
        reversed.insert(
            Illuminant::A,
            vec![0.9, 0.1, 0.0, 0.2, 0.7, 0.1, 0.0, 0.1, 0.9],
        );
        assert_eq!(
            select_rawler_xyz_to_cam(&reversed, [[0.0; 3]; 4], false).unwrap(),
            selected
        );
    }

    #[test]
    fn rawler_fallback_uses_legacy_or_mono_identity_but_never_a_zero_rgb_matrix() {
        let matrices = std::collections::HashMap::new();
        let legacy = [
            [0.7, 0.2, 0.1],
            [0.1, 0.8, 0.1],
            [0.0, 0.1, 0.9],
            [0.0, 0.0, 0.0],
        ];
        assert_eq!(
            select_rawler_xyz_to_cam(&matrices, legacy, false).unwrap(),
            legacy
        );
        assert!(select_rawler_xyz_to_cam(&matrices, [[0.0; 3]; 4], false).is_err());
        assert_eq!(
            select_rawler_xyz_to_cam(&matrices, [[0.0; 3]; 4], true).unwrap(),
            [
                [1.0, 0.0, 0.0],
                [0.0, 1.0, 0.0],
                [0.0, 0.0, 1.0],
                [0.0, 0.0, 0.0],
            ]
        );
    }

    fn srgb_to_linear_for_test(c: f32) -> f32 {
        if c <= 0.04045 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    }

    fn quantize_u8(v: f32) -> u8 {
        (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8
    }

    #[test]
    fn linear_to_srgb_matches_standard_curve() {
        let cases = [
            (0.0, 0.0),
            (0.0031308, 0.040449936),
            (0.18, 0.46135613),
            (0.5, 0.73535698),
            (1.0, 0.99999994),
        ];
        for (linear, expected) in cases {
            assert!(
                (linear_to_srgb(linear) - expected).abs() < 1e-5,
                "linear_to_srgb({linear})"
            );
        }
    }

    #[test]
    fn srgb_texture_roundtrip_has_no_missing_or_double_gamma() {
        // Uploading sRGB bytes to an Rgba8UnormSrgb texture, sampling in WGSL, and
        // writing to an sRGB target should return the same display byte for a
        // single opaque normal layer. Missing or double gamma would fail this.
        for byte in [0u8, 1, 8, 16, 64, 128, 200, 254, 255] {
            let encoded = byte as f32 / 255.0;
            let sampled_linear = srgb_to_linear_for_test(encoded);
            let output_encoded = linear_to_srgb(sampled_linear);
            assert_eq!(quantize_u8(output_encoded), byte, "byte {byte}");
        }
    }

    #[test]
    fn white_level_uses_observed_when_metadata_is_container_max() {
        let white = choose_effective_white_level(65_535.0, 512.0, 16_200.0);
        assert!(
            (white - 16_200.0).abs() < 1e-5,
            "container white should fall back to observed sensor white"
        );
    }

    #[test]
    fn white_level_trusts_plausible_camera_white() {
        let white = choose_effective_white_level(15_360.0, 512.0, 12_000.0);
        assert!(
            (white - 15_360.0).abs() < 1e-5,
            "plausible camera white should not auto-brighten an underexposed frame"
        );
    }

    #[test]
    fn white_balance_gains_normalize_to_green() {
        let gain = white_balance_gains([2.4, 1.2, 1.8, 0.0], false);
        assert!((gain[0] - 2.0).abs() < 1e-6);
        assert!((gain[1] - 1.0).abs() < 1e-6);
        assert!((gain[2] - 1.5).abs() < 1e-6);
        assert_eq!(gain[3], 1.0);
        assert_eq!(white_balance_gains([2.4, 1.2, 1.8, 0.0], true), [1.0; 4]);
    }

    #[test]
    fn camera_neutral_recovers_absolute_as_shot_kelvin() {
        let identity_cam2xyz = [
            [1.0, 0.0, 0.0, 0.0],
            [0.0, 1.0, 0.0, 0.0],
            [0.0, 0.0, 1.0, 0.0],
        ];
        // CIE illuminant A chromaticity, normalized to Y=1. Camera WB gains
        // are the reciprocal neutral normalized to green.
        let (x, y) = (0.447_57f32, 0.407_45f32);
        let xyz = [x / y, 1.0, (1.0 - x - y) / y];
        let gains = [1.0 / xyz[0], 1.0, 1.0 / xyz[2], 1.0];
        let white = as_shot_white_balance(&identity_cam2xyz, gains, false).unwrap();
        assert!(
            (white.cct_kelvin - 2856.0).abs() < 80.0,
            "as-shot {white:?}"
        );
        assert!(white.duv.abs() < 0.003, "as-shot {white:?}");
        assert!(as_shot_white_balance(&identity_cam2xyz, gains, true).is_none());
    }

    #[test]
    fn camera_to_srgb_matrix_composes_xyz_rows() {
        let identity_cam2xyz = [
            [1.0, 0.0, 0.0, 0.0],
            [0.0, 1.0, 0.0, 0.0],
            [0.0, 0.0, 1.0, 0.0],
        ];
        let m = resolve_decoder_matrix(
            RawDecoderBackend::Rawloader,
            "test",
            "identity",
            &identity_cam2xyz,
        )
        .camera_to_linear_srgb;
        let xyz_to_srgb = [
            [3.2404542f32, -1.5371385, -0.4985314],
            [-0.9692660, 1.8760108, 0.0415560],
            [0.0556434, -0.2040259, 1.0572252],
        ];
        for y in 0..3 {
            for x in 0..3 {
                assert!(
                    (m[y][x] - xyz_to_srgb[y][x]).abs() < 1e-6,
                    "matrix[{y}][{x}]"
                );
            }
        }
    }

    #[test]
    fn scene_master_keeps_headroom_and_default_look_is_sigmoid() {
        // The decode path stores UNCLAMPED linear f16: headroom above 1.0 must
        // survive into the SceneSource, and the default document render must be
        // the neutral sigmoid look (mid-grey anchored), not a clipped encode.
        let mut dst = [0u16; 4];
        let identity = [[1.0f32, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];
        write_scene(&mut dst, &identity, [2.5, 0.1845, -0.05]);
        let stored = [
            crate::core::develop_scene::f16_bits_to_f32(dst[0]),
            crate::core::develop_scene::f16_bits_to_f32(dst[1]),
            crate::core::develop_scene::f16_bits_to_f32(dst[2]),
        ];
        let recovered =
            crate::core::working_color::WorkingColorSpace::LinearProPhoto.to_linear_srgb(stored);
        assert!(
            (recovered[0] - 2.5).abs() < 0.01,
            "headroom clipped: {recovered:?}"
        );
        let neg = recovered[2];
        assert!(neg < 0.0, "out-of-gamut value clipped: {neg}");
        assert_eq!(dst[3], 0x3c00, "alpha must be f16 1.0");
    }

    #[derive(Clone, Copy)]
    struct DiagPick {
        label: &'static str,
        score: f32,
        x: usize,
        y: usize,
    }

    fn update_pick(pick: &mut DiagPick, score: f32, x: usize, y: usize) {
        if score.is_finite() && score > pick.score {
            pick.score = score;
            pick.x = x;
            pick.y = y;
        }
    }

    fn closeness(v: f32, target: f32, width: f32) -> f32 {
        (1.0 - ((v - target).abs() / width.max(1e-6))).clamp(0.0, 1.0)
    }

    fn hue_closeness(h: f32, target: f32, width: f32) -> f32 {
        let d = (h - target).abs().min(1.0 - (h - target).abs());
        (1.0 - d / width.max(1e-6)).clamp(0.0, 1.0)
    }

    fn orientation_dims(area: ActiveArea, o: Orientation) -> (usize, usize) {
        let swap = matches!(
            o,
            Orientation::Transpose
                | Orientation::Rotate90
                | Orientation::Transverse
                | Orientation::Rotate270
        );
        if swap {
            (area.height, area.width)
        } else {
            (area.width, area.height)
        }
    }

    fn active_to_final_xy(x: usize, y: usize, area: ActiveArea, o: Orientation) -> (usize, usize) {
        let (swap, fx, fy) = match o {
            Orientation::Normal | Orientation::Unknown => (false, false, false),
            Orientation::HorizontalFlip => (false, true, false),
            Orientation::Rotate180 => (false, true, true),
            Orientation::VerticalFlip => (false, false, true),
            Orientation::Transpose => (true, false, false),
            Orientation::Rotate90 => (true, true, false),
            Orientation::Transverse => (true, true, true),
            Orientation::Rotate270 => (true, false, true),
        };
        let (dw, dh) = orientation_dims(area, o);
        let (mut dx, mut dy) = if swap { (y, x) } else { (x, y) };
        if fx {
            dx = dw - 1 - dx;
        }
        if fy {
            dy = dh - 1 - dy;
        }
        (dx, dy)
    }

    fn diag_camera_rgb_at(
        raw: &RawImage,
        levels: RawLevels,
        gain: [f32; 4],
        sensor_r: usize,
        sensor_c: usize,
        apply_wb: bool,
    ) -> [f32; 3] {
        let mono = raw.cpp == 1 && !raw.cfa.is_valid();
        match raw.cpp {
            1 => {
                let at = |dr: i32, dc: i32| -> f32 {
                    let rr = (sensor_r as i32 + dr).clamp(0, raw.height as i32 - 1) as usize;
                    let cc = (sensor_c as i32 + dc).clamp(0, raw.width as i32 - 1) as usize;
                    let ch = if mono {
                        0
                    } else {
                        raw.cfa.color_at(rr, cc).min(3)
                    };
                    let v = ((raw_value(&raw.data, rr * raw.width + cc) - levels.black[ch])
                        / levels.denom[ch])
                        .max(0.0);
                    if apply_wb {
                        v * gain[ch]
                    } else {
                        v
                    }
                };
                if mono {
                    let v = at(0, 0);
                    return [v, v, v];
                }

                let center = at(0, 0);
                let diag = at(-1, -1) + at(-1, 1) + at(1, -1) + at(1, 1);
                let cc = raw.cfa.color_at(sensor_r, sensor_c);
                let (red, green, blue);
                if cc == 1 {
                    green = center;
                    let h2 = at(0, -2) + at(0, 2);
                    let v2 = at(-2, 0) + at(2, 0);
                    let horiz = at(0, -1) + at(0, 1);
                    let vert = at(-1, 0) + at(1, 0);
                    let chan_h = (5.0 * center + 4.0 * horiz - diag - h2 + 0.5 * v2) / 8.0;
                    let chan_v = (5.0 * center + 4.0 * vert - diag - v2 + 0.5 * h2) / 8.0;
                    if raw.cfa.color_at(sensor_r, sensor_c + 1) == 0 {
                        red = chan_h;
                        blue = chan_v;
                    } else {
                        blue = chan_h;
                        red = chan_v;
                    }
                } else {
                    let cross = at(0, -1) + at(0, 1) + at(-1, 0) + at(1, 0);
                    let far = at(0, -2) + at(0, 2) + at(-2, 0) + at(2, 0);
                    green = (4.0 * center + 2.0 * cross - far) / 8.0;
                    let opposite = (6.0 * center + 2.0 * diag - 1.5 * far) / 8.0;
                    if cc == 0 {
                        red = center;
                        blue = opposite;
                    } else {
                        blue = center;
                        red = opposite;
                    }
                }
                [red.max(0.0), green.max(0.0), blue.max(0.0)]
            }
            3 => {
                let src = (sensor_r * raw.width + sensor_c) * 3;
                [0usize, 1, 2].map(|ch| {
                    let v = ((raw_value(&raw.data, src + ch) - levels.black[ch])
                        / levels.denom[ch])
                        .max(0.0);
                    if apply_wb {
                        v * gain[ch]
                    } else {
                        v
                    }
                })
            }
            _ => [0.0; 3],
        }
    }

    // NOTE: highlight reconstruction is a global two-pass ("inpaint opposed"), so
    // the per-pixel diagnosis below shows the UNRECOVERED white-balanced value.
    fn diag_display_srgb_at(
        raw: &RawImage,
        levels: RawLevels,
        gain: [f32; 4],
        cam2srgb: &[[f32; 3]; 3],
        sensor_r: usize,
        sensor_c: usize,
    ) -> [f32; 3] {
        let cam = diag_camera_rgb_at(raw, levels, gain, sensor_r, sensor_c, true);
        let tone = crate::core::develop_scene::build_scene_tone(&Default::default());
        tone.scene_to_display(camera_to_linear_srgb(cam2srgb, cam), None)
    }

    fn fmt3(v: [f32; 3]) -> String {
        format!("[{:.4}, {:.4}, {:.4}]", v[0], v[1], v[2])
    }

    fn gpu_roundtrip_byte(byte: u8) -> u8 {
        quantize_u8(linear_to_srgb(srgb_to_linear_for_test(byte as f32 / 255.0)))
    }

    #[test]
    fn raw_pipeline_diagnosis() {
        let Some(sample) = std::env::var_os("IAI_RAW_SAMPLE") else {
            return;
        };
        let path = std::path::PathBuf::from(sample);
        if !path.exists() {
            return;
        }

        let raw = rawloader::decode_file(&path).expect("decode raw for diagnosis");
        let area = active_area(&raw).expect("active area");
        let mono = raw.cpp == 1 && !raw.cfa.is_valid();
        let wbc = if raw.wb_coeffs[0] > 0.0 && raw.wb_coeffs[1] > 0.0 && raw.wb_coeffs[2] > 0.0 {
            raw.wb_coeffs
        } else {
            raw.neutralwb()
        };
        let gain = white_balance_gains(wbc, mono);
        let levels = raw_levels(&raw, area);
        let cam2xyz = raw.cam_to_xyz_normalized();
        let cam2srgb = resolve_decoder_matrix(
            RawDecoderBackend::Rawloader,
            &raw.clean_make,
            &raw.clean_model,
            &cam2xyz,
        )
        .camera_to_linear_srgb;
        let (fw, fh) = orientation_dims(area, raw.orientation);

        let mut picks = [
            DiagPick {
                label: "white highlight",
                score: -1.0,
                x: 2,
                y: 2,
            },
            DiagPick {
                label: "white fold",
                score: -1.0,
                x: 2,
                y: 2,
            },
            DiagPick {
                label: "skin midtone",
                score: -1.0,
                x: 2,
                y: 2,
            },
            DiagPick {
                label: "dark hair",
                score: -1.0,
                x: 2,
                y: 2,
            },
            DiagPick {
                label: "bright green leaf",
                score: -1.0,
                x: 2,
                y: 2,
            },
            DiagPick {
                label: "dark green gap",
                score: -1.0,
                x: 2,
                y: 2,
            },
            DiagPick {
                label: "red brown ceramic",
                score: -1.0,
                x: 2,
                y: 2,
            },
            DiagPick {
                label: "near black",
                score: -1.0,
                x: 2,
                y: 2,
            },
        ];

        let step = (area.width.max(area.height) / 700).max(4);
        let y_end = area.height.saturating_sub(2);
        let x_end = area.width.saturating_sub(2);
        for y in (2..y_end).step_by(step) {
            for x in (2..x_end).step_by(step) {
                let sensor_r = area.top + y;
                let sensor_c = area.left + x;
                let srgb = diag_display_srgb_at(&raw, levels, gain, &cam2srgb, sensor_r, sensor_c);
                let l = luma_lin(srgb);
                let (h, s, _) = crate::core::color::rgb_to_hsv(srgb[0], srgb[1], srgb[2]);
                let neutral = 1.0 - s.clamp(0.0, 1.0);
                update_pick(&mut picks[0], closeness(l, 0.86, 0.18) * neutral, x, y);
                update_pick(&mut picks[1], closeness(l, 0.58, 0.20) * neutral, x, y);
                update_pick(
                    &mut picks[2],
                    hue_closeness(h, 0.07, 0.08)
                        * closeness(l, 0.52, 0.28)
                        * closeness(s, 0.32, 0.32),
                    x,
                    y,
                );
                update_pick(
                    &mut picks[3],
                    closeness(l, 0.12, 0.12) * (1.0 - (s * 0.8).min(1.0)),
                    x,
                    y,
                );
                update_pick(
                    &mut picks[4],
                    hue_closeness(h, 0.33, 0.13) * closeness(l, 0.50, 0.28) * s,
                    x,
                    y,
                );
                update_pick(
                    &mut picks[5],
                    hue_closeness(h, 0.33, 0.13) * closeness(l, 0.20, 0.16) * s,
                    x,
                    y,
                );
                let red_hue = hue_closeness(h, 0.04, 0.10).max(hue_closeness(h, 0.98, 0.08));
                update_pick(&mut picks[6], red_hue * closeness(l, 0.32, 0.24) * s, x, y);
                update_pick(&mut picks[7], closeness(l, 0.035, 0.05) * neutral, x, y);
            }
        }

        eprintln!("RAW pipeline diagnosis: {}", path.display());
        eprintln!(
            "raw={}x{} cpp={} active={}x{}+{},{} final={}x{} orientation={:?}",
            raw.width,
            raw.height,
            raw.cpp,
            area.width,
            area.height,
            area.left,
            area.top,
            fw,
            fh,
            raw.orientation
        );
        eprintln!(
            "camera='{} {}' wb_coeffs={:?} wb_gain={:?}",
            raw.clean_make.trim(),
            raw.clean_model.trim(),
            wbc,
            gain
        );
        eprintln!(
            "black={:?} reported_white={:?} observed_white={:?} effective_white={:?} denom={:?}",
            levels.black,
            raw.whitelevels,
            levels.observed_white,
            levels.effective_white,
            levels.denom
        );
        eprintln!("cam_to_xyz={cam2xyz:?}");
        eprintln!("cam_to_linear_srgb={cam2srgb:?}");
        eprintln!("profile: no DCP/ICC camera profile found; using rawloader normalized camera matrix + neutral scene-referred sigmoid render; output document is tagged sRGB");
        let default_tone = crate::core::develop_scene::build_scene_tone(&Default::default());
        eprintln!(
            "samples are auto-selected proxies from the RAW render, not hand-labelled regions:"
        );

        for pick in picks {
            let sensor_r = area.top + pick.y;
            let sensor_c = area.left + pick.x;
            let cfa_ch = if raw.cpp == 1 {
                if mono {
                    0
                } else {
                    raw.cfa.color_at(sensor_r, sensor_c)
                }
            } else {
                0
            };
            let raw_center = raw_value(
                &raw.data,
                if raw.cpp == 3 {
                    (sensor_r * raw.width + sensor_c) * 3
                } else {
                    sensor_r * raw.width + sensor_c
                },
            );
            let norm = diag_camera_rgb_at(&raw, levels, gain, sensor_r, sensor_c, false);
            let wb = diag_camera_rgb_at(&raw, levels, gain, sensor_r, sensor_c, true);
            let linear = camera_to_linear_srgb(&cam2srgb, wb);
            let srgb = default_tone.scene_to_display(linear, None);
            let (fx, fy) = active_to_final_xy(pick.x, pick.y, area, raw.orientation);
            let q16 = |v: f32| (v.clamp(0.0, 1.0) * 65535.0 + 0.5) as u16;
            let cpu16 = [q16(srgb[0]), q16(srgb[1]), q16(srgb[2])];
            let cpu8 = [
                crate::core::tile::dither16_to_u8(
                    cpu16[0],
                    (fx as u32) % crate::core::tile::TILE_SIZE,
                    (fy as u32) % crate::core::tile::TILE_SIZE,
                    0,
                ),
                crate::core::tile::dither16_to_u8(
                    cpu16[1],
                    (fx as u32) % crate::core::tile::TILE_SIZE,
                    (fy as u32) % crate::core::tile::TILE_SIZE,
                    1,
                ),
                crate::core::tile::dither16_to_u8(
                    cpu16[2],
                    (fx as u32) % crate::core::tile::TILE_SIZE,
                    (fy as u32) % crate::core::tile::TILE_SIZE,
                    2,
                ),
            ];
            let gpu8 = [
                gpu_roundtrip_byte(cpu8[0]),
                gpu_roundtrip_byte(cpu8[1]),
                gpu_roundtrip_byte(cpu8[2]),
            ];
            eprintln!(
                "{:18} active=({:4},{:4}) final=({:4},{:4}) raw_ch={} raw={:.1} norm={} wb={} linear={} srgb={} cpu16={:?} cpu8={:?} gpu8={:?}",
                pick.label,
                pick.x,
                pick.y,
                fx,
                fy,
                cfa_ch,
                raw_center,
                fmt3(norm),
                fmt3(wb),
                fmt3(linear),
                fmt3(srgb),
                cpu16,
                cpu8,
                gpu8
            );
        }
    }

    /// Synthetic Bayer plane with fixed chroma ratios (r = 0.75·g, b = 0.5·g) and
    /// a horizontal green gradient whose bright end saturates at the sensor cap.
    fn opposed_test_plane(w: usize, h: usize, cfa: &rawloader::CFA) -> Vec<f32> {
        let g_true = |x: usize| 0.2 + 1.6 * x as f32 / (w - 1) as f32;
        (0..w * h)
            .map(|i| {
                let (r, c) = (i / w, i % w);
                let g = g_true(c);
                let v = match chroma_channel(cfa.color_at(r, c)) {
                    0 => 0.75 * g,
                    2 => 0.5 * g,
                    _ => g,
                };
                v.min(1.0) // sensor saturation
            })
            .collect()
    }

    #[test]
    fn opposed_inpaint_reconstructs_clipped_green_with_plausible_chroma() {
        let (w, h) = (64usize, 16usize);
        let cfa = rawloader::CFA::new("RGGB");
        let mut plane = opposed_test_plane(w, h, &cfa);
        let original = plane.clone();
        inpaint_opposed_bayer(&mut plane, w, h, &cfa, [1.0; 4]);

        // Green site (odd col on even row for RGGB) where the true green is 1.19
        // but the sensor capped it at 1.0: the opposed reconstruction must push it
        // back above the clip and near the truth, instead of leaving the cap (old
        // neutralize could never exceed the brightest capped channel).
        let (row, col) = (8usize, 39usize);
        assert_eq!(chroma_channel(cfa.color_at(row, col)), 1, "site is green");
        let g_true = 0.2 + 1.6 * col as f32 / (w - 1) as f32;
        assert!(g_true > 1.05, "test site must be clipped: {g_true}");
        let rec = plane[row * w + col];
        assert!(
            rec > 1.02,
            "reconstructed green above the clip level: {rec}"
        );
        assert!(
            (rec - g_true).abs() / g_true < 0.12,
            "reconstruction near truth: rec={rec} true={g_true}"
        );

        // The red site next to it is unclipped (0.75·g < 0.98) and must be
        // bit-identical — reconstruction only touches clipped samples.
        let (rr, rc) = (8usize, 38usize);
        assert_eq!(chroma_channel(cfa.color_at(rr, rc)), 0, "site is red");
        assert_eq!(plane[rr * w + rc], original[rr * w + rc]);

        // Far from any clipping, everything stays bit-identical.
        for r in 0..h {
            for c in 0..8 {
                assert_eq!(plane[r * w + c], original[r * w + c]);
            }
        }
    }

    #[test]
    fn opposed_inpaint_without_clipping_is_noop() {
        let (w, h) = (32usize, 16usize);
        let cfa = rawloader::CFA::new("RGGB");
        let mut plane: Vec<f32> = (0..w * h)
            .map(|i| 0.1 + 0.7 * ((i % w) as f32 / (w - 1) as f32))
            .collect();
        let original = plane.clone();
        inpaint_opposed_bayer(&mut plane, w, h, &cfa, [1.0; 4]);
        assert_eq!(plane, original);
    }

    #[test]
    fn opposed_rgb_reconstructs_clipped_channel_from_pixel_chroma() {
        // Already-demosaiced (cpp=3) variant: same ratios, green clips right of
        // centre. The stats pass must flag only green and its reconstruction must
        // recover a plausible above-clip value from the pixel's other channels.
        let (w, h) = (48usize, 8usize);
        let g_true = |x: usize| 0.2 + 1.6 * x as f32 / (w - 1) as f32;
        let mut data = Vec::with_capacity(w * h * 3);
        for i in 0..w * h {
            let g = g_true(i % w);
            data.push((0.75 * g).min(1.0));
            data.push(g.min(1.0));
            data.push((0.5 * g).min(1.0));
        }
        let data = RawImageData::Float(data);
        let levels = RawLevels {
            black: [0.0; 4],
            observed_white: [1.0; 4],
            effective_white: [1.0; 4],
            denom: [1.0; 4],
        };
        let op = opposed_rgb(&data, w, h, &levels, [1.0; 4]).expect("clipping present");

        let col = 29usize; // g_true ≈ 1.19: green clipped, red (0.89 = 0.75·g) not
        let idx = 4 * w + col;
        assert_eq!(
            op.clipped[idx], 0b010,
            "only green flagged: {}",
            op.clipped[idx]
        );
        let truth = g_true(col);
        let mut cam = [(0.75 * truth).min(1.0), 1.0, 0.5 * truth];
        op.reconstruct(idx, &mut cam);
        assert!(cam[1] > 1.02, "green pushed above the clip: {}", cam[1]);
        assert!(
            (cam[1] - truth).abs() / truth < 0.15,
            "reconstruction near truth: rec={} true={truth}",
            cam[1]
        );
        assert!(
            (cam[0] - (0.75 * truth).min(1.0)).abs() < 1e-6,
            "red untouched"
        );

        // No clipping anywhere → no state at all.
        let flat = RawImageData::Float(vec![0.4f32; w * h * 3]);
        assert!(opposed_rgb(&flat, w, h, &levels, [1.0; 4]).is_none());
    }

    /// Build an f16 RGBA scene buffer from a per-pixel linear gray value.
    fn scene_buf_from(vals: &[f32]) -> Vec<u16> {
        let mut out = Vec::with_capacity(vals.len() * 4);
        for &v in vals {
            let e = f32_to_f16_bits(v);
            out.extend_from_slice(&[e, e, e, 0x3c00]);
        }
        out
    }

    #[test]
    fn capture_sharpen_boosts_edge_acutance() {
        let (w, h) = (64usize, 16usize);
        let vals: Vec<f32> = (0..w * h)
            .map(|i| if i % w < w / 2 { 0.15 } else { 0.55 })
            .collect();
        let mut buf = scene_buf_from(&vals);
        capture_sharpen(&mut buf, w, h, CS_GAIN, CS_DARK_RATIO, CS_FLOOR);
        let at =
            |x: usize, y: usize| crate::core::develop_scene::f16_bits_to_f32(buf[(y * w + x) * 4]);

        // Acutance: the step between the two pixels flanking the edge must grow
        // (undershoot on the dark side, overshoot on the bright side).
        let step_before = 0.55 - 0.15;
        let step_after = at(w / 2, 8) - at(w / 2 - 1, 8);
        assert!(
            step_after > step_before + 0.02,
            "edge contrast increased: before={step_before} after={step_after}"
        );
        // Far from the edge the image is flat — untouched.
        assert!((at(4, 8) - 0.15).abs() < 1e-3, "flat left side untouched");
        assert!(
            (at(w - 4, 8) - 0.55).abs() < 1e-3,
            "flat right side untouched"
        );
    }

    #[test]
    fn capture_sharpen_does_not_crush_dark_edge_toward_black() {
        // Bright block beside a dark block. The unsharp undershoot must NOT dim
        // the dark-side edge pixel toward black (the dotted-rim artifact a strong
        // gain produced on soft skin↔dark edges); the floor bounds the darkening.
        let (w, h) = (64usize, 16usize);
        let dark = 0.06f32;
        let vals: Vec<f32> = (0..w * h)
            .map(|i| if i % w < w / 2 { 0.55 } else { dark })
            .collect();
        let mut buf = scene_buf_from(&vals);
        capture_sharpen(&mut buf, w, h, CS_GAIN, CS_DARK_RATIO, CS_FLOOR);
        let at =
            |x: usize, y: usize| crate::core::develop_scene::f16_bits_to_f32(buf[(y * w + x) * 4]);
        // First dark pixel (the edge's dark side) keeps at least `floor` of its
        // value — no near-black speck.
        let dark_edge = at(w / 2, 8);
        assert!(
            dark_edge >= dark * CS_FLOOR - 1e-3,
            "dark edge floored, not crushed: {dark_edge} (>= {})",
            dark * CS_FLOOR
        );
        // The bright side still overshoots (acutance preserved).
        assert!(at(w / 2 - 1, 8) > 0.55, "bright side still sharpened");
    }

    #[test]
    fn capture_sharpen_guard_spares_flat_noise() {
        // Flat midtone with sub-percent deterministic noise: relative contrast sits
        // far below the guard threshold, so the pass must leave it bit-identical
        // (no noise amplification).
        let (w, h) = (32usize, 32usize);
        let mut seed = 0x12345678u32;
        let mut rand = move || {
            seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
            (seed >> 16) as f32 / 65535.0 - 0.5
        };
        let vals: Vec<f32> = (0..w * h).map(|_| 0.3 + 0.008 * rand()).collect();
        let mut buf = scene_buf_from(&vals);
        let original = buf.clone();
        capture_sharpen(&mut buf, w, h, CS_GAIN, CS_DARK_RATIO, CS_FLOOR);
        assert_eq!(buf, original, "flat noise must not be sharpened");
    }

    #[test]
    fn bayer_defect_filter_removes_isolated_dead_and_hot_sites_but_keeps_lines() {
        let (w, h) = (20usize, 20usize);
        let cfa = rawloader::CFA::new("RGGB");
        let mut plane = vec![0.4f32; w * h];
        plane[8 * w + 8] = 0.0;
        plane[12 * w + 12] = 4.0;
        // A real two-pixel-wide dark feature has dark same-colour neighbours
        // and must survive the isolated-site precheck.
        for y in 4..16 {
            plane[y * w + 5] = 0.03;
            plane[y * w + 6] = 0.03;
        }
        correct_isolated_bayer_defects(&mut plane, w, h, &cfa);
        assert!((plane[8 * w + 8] - 0.4).abs() < 1e-6);
        assert!((plane[12 * w + 12] - 0.4).abs() < 1e-6);
        assert_eq!(plane[10 * w + 5], 0.03);
        assert_eq!(plane[10 * w + 6], 0.03);
    }

    #[test]
    fn sensor_correction_plan_is_explicit_and_bounded() {
        let bayer = sensor_correction_plan(20, 20, 1, true, false);
        assert!(bayer.isolated_bayer_defects.enabled);
        assert_eq!(
            bayer.isolated_bayer_defects.reason,
            SensorCorrectionReason::BayerDefectBaseline
        );
        assert_eq!(
            bayer.isolated_bayer_defects.estimated_scratch_bytes,
            12 * 12 * std::mem::size_of::<(usize, f32)>()
        );
        assert!(!bayer.green_equilibration.enabled);
        assert_eq!(
            bayer.green_equilibration.reason,
            SensorCorrectionReason::MissingMetadataOrDiagnostic
        );
        assert_eq!(bayer.green_equilibration.estimated_scratch_bytes, 0);

        for plan in [
            sensor_correction_plan(20, 20, 1, false, true),
            sensor_correction_plan(20, 20, 3, false, false),
        ] {
            assert!(!plan.isolated_bayer_defects.enabled);
            assert_eq!(
                plan.isolated_bayer_defects.reason,
                SensorCorrectionReason::NotBayerMosaic
            );
            assert_eq!(plan.isolated_bayer_defects.estimated_scratch_bytes, 0);
        }
    }

    #[test]
    fn disabled_sensor_correction_stage_is_bit_exact_noop() {
        let (w, h) = (20usize, 20usize);
        let cfa = rawloader::CFA::new("RGGB");
        let mut plane = vec![0.4f32; w * h];
        plane[8 * w + 8] = 0.0;
        plane[12 * w + 12] = 4.0;
        let original = plane.clone();
        apply_isolated_bayer_defect_stage(
            &mut plane,
            w,
            h,
            &cfa,
            SensorCorrectionStage {
                enabled: false,
                reason: SensorCorrectionReason::NotBayerMosaic,
                estimated_scratch_bytes: 0,
            },
        );
        assert_eq!(plane, original, "disabled correction must touch no sample");
    }

    #[test]
    fn automatic_capture_sharpen_is_enabled_and_gentle() {
        // A RAW must open with a crisp baseline, but gently enough to avoid the
        // dark hair/skin beads the old two-iteration/high-gain setting produced.
        assert!(
            CAPTURE_SHARPEN,
            "RAW should open with a capture-sharpen baseline"
        );
        assert_eq!(
            CS_ITERATIONS, 1,
            "one iteration keeps overshoot from compounding"
        );
        // One iteration + the relative-contrast guard + colour NR running first
        // keep this from beading even though the gain restores real acutance.
        assert!(
            CS_GAIN <= 0.7,
            "gain must stay moderate to avoid edge beads"
        );
    }

    #[test]
    fn raw_render_recipe_versions_pin_shipping_and_technical_boundaries() {
        let shipping = RawRenderRecipe::legacy_baked_v1();
        assert_eq!(shipping.version, RawRenderRecipeVersion::LegacyBaked1);
        assert_eq!(shipping.scene_color_nr, SCENE_COLOR_NR);
        assert_eq!(shipping.capture_sharpen_gain, CS_GAIN);
        assert_eq!(shipping.chroma_enrich, CHROMA_ENRICH);
        assert_eq!(shipping.scene_brightness, SCENE_BRIGHTNESS);
        assert_eq!(shipping.scene_chroma_base, SCENE_CHROMA_BASE);
        assert_eq!(shipping.scene_chroma_shadow, SCENE_CHROMA_SHADOW);
        assert_eq!(shipping.scene_warm, SCENE_WARM);

        let neutral = RawRenderRecipe::technical_neutral_v2();
        assert_eq!(neutral.version, RawRenderRecipeVersion::TechnicalNeutral2);
        assert_eq!(neutral.scene_color_nr, 0.0);
        assert_eq!(neutral.capture_sharpen_gain, 0.0);
        assert_eq!(neutral.chroma_enrich, 0.0);
        assert_eq!(neutral.scene_brightness, 1.0);
        assert_eq!(neutral.scene_chroma_base, 0.0);
        assert_eq!(neutral.scene_chroma_shadow, 0.0);
        assert_eq!(neutral.scene_warm, 0.0);
    }

    /// Build an f16 RGBA scene buffer from per-pixel linear RGB triples.
    fn scene_buf_rgb(vals: &[[f32; 3]]) -> Vec<u16> {
        let mut out = Vec::with_capacity(vals.len() * 4);
        for v in vals {
            out.extend_from_slice(&[
                f32_to_f16_bits(v[0]),
                f32_to_f16_bits(v[1]),
                f32_to_f16_bits(v[2]),
                0x3c00,
            ]);
        }
        out
    }

    const LW_TEST: [f32; 3] = [0.22, 0.69, 0.09];
    fn luma_of(px: &[u16]) -> f32 {
        LW_TEST[0] * f16_bits_to_f32(px[0])
            + LW_TEST[1] * f16_bits_to_f32(px[1])
            + LW_TEST[2] * f16_bits_to_f32(px[2])
    }
    fn chroma_of(px: &[u16]) -> f32 {
        let r = f16_bits_to_f32(px[0]);
        let g = f16_bits_to_f32(px[1]);
        let b = f16_bits_to_f32(px[2]);
        r.max(g).max(b) - r.min(g).min(b)
    }

    #[test]
    fn denoise_scene_chroma_smooths_chroma_and_holds_luma() {
        // Flat luminance with a checkerboard chroma speck (offset chosen to carry
        // ~zero luma) → colour NR must attenuate the speckle but leave luma intact.
        let (w, h) = (16usize, 16usize);
        let base = 0.4f32;
        let off = [1.0f32, 0.0, -LW_TEST[0] / LW_TEST[2]]; // LW·off ≈ 0
        let s = 0.05f32;
        let mut vals = Vec::with_capacity(w * h);
        for y in 0..h {
            for x in 0..w {
                let sgn = if (x + y) % 2 == 0 { 1.0 } else { -1.0 };
                vals.push([
                    base + sgn * s * off[0],
                    base + sgn * s * off[1],
                    base + sgn * s * off[2],
                ]);
            }
        }
        let mut buf = scene_buf_rgb(&vals);
        let luma_before: Vec<f32> = buf.chunks_exact(4).map(luma_of).collect();
        let chroma_before: f32 = buf.chunks_exact(4).map(chroma_of).sum::<f32>() / (w * h) as f32;
        denoise_scene_chroma(&mut buf, w, h, 0.85);
        let chroma_after: f32 = buf.chunks_exact(4).map(chroma_of).sum::<f32>() / (w * h) as f32;
        assert!(
            chroma_after < chroma_before * 0.5,
            "colour speckle attenuated: before={chroma_before} after={chroma_after}"
        );
        for (px, &l0) in buf.chunks_exact(4).zip(&luma_before) {
            assert!(
                (luma_of(px) - l0).abs() < 3e-3,
                "luma preserved: {l0} -> {}",
                luma_of(px)
            );
        }
    }

    #[test]
    fn suppress_false_color_fixes_speck_keeps_green_and_neutral() {
        // A uniform warm field (surround R−G=+0.1, B−G=−0.1) with one isolated
        // false-colour speck. The median must pull the speck's colour differences
        // back to the surround while leaving green (luma) exact, and must not tint
        // a neutral field.
        let (w, h) = (9usize, 9usize);
        let base = [0.5f32, 0.4, 0.3];
        let mut vals = vec![base; w * h];
        vals[4 * w + 4] = [0.3, 0.7, 0.3]; // green false-colour speck
        let mut buf = scene_buf_rgb(&vals);
        let g_before: Vec<f32> = buf.chunks_exact(4).map(|p| f16_bits_to_f32(p[1])).collect();
        suppress_false_color(&mut buf, w, h, 2);
        let sp = &buf[(4 * w + 4) * 4..(4 * w + 4) * 4 + 4];
        let (r, g, b) = (
            f16_bits_to_f32(sp[0]),
            f16_bits_to_f32(sp[1]),
            f16_bits_to_f32(sp[2]),
        );
        assert!(
            ((r - g) - 0.1).abs() < 0.03,
            "R−G pulled to surround: {}",
            r - g
        );
        assert!(
            ((b - g) + 0.1).abs() < 0.03,
            "B−G pulled to surround: {}",
            b - g
        );
        for (px, &g0) in buf.chunks_exact(4).zip(&g_before) {
            assert!(
                (f16_bits_to_f32(px[1]) - g0).abs() < 1e-3,
                "green (luma) left exact"
            );
        }
        // Neutral field is untouched.
        let mut neutral = scene_buf_rgb(&vec![[0.3f32, 0.3, 0.3]; w * h]);
        let before = neutral.clone();
        suppress_false_color(&mut neutral, w, h, 2);
        assert_eq!(neutral, before, "neutral stays neutral");
    }

    #[test]
    fn enrich_scene_chroma_shadow_holds_neutral_and_favours_shadows() {
        // p0 neutral, p1 a low-sat coloured DEEP shadow, p2 the same colour scaled
        // into the midtones. Neutral stays neutral; both gain chroma; the deep
        // shadow gains proportionally more (higher shadow weight).
        let p1 = [0.0072f32, 0.006, 0.0048];
        let scale = 33.0f32;
        let p2 = [p1[0] * scale, p1[1] * scale, p1[2] * scale];
        let neutral = [0.05f32, 0.05, 0.05];
        let mut buf = scene_buf_rgb(&[neutral, p1, p2]);
        let c1_before = chroma_of(&buf[4..8]);
        let c2_before = chroma_of(&buf[8..12]);
        enrich_scene_chroma_shadow(
            &mut buf,
            SCENE_CHROMA_BASE,
            SCENE_CHROMA_SHADOW,
            CHROMA_SHADOW_LOW_EV,
            CHROMA_SHADOW_HIGH_EV,
        );
        assert!(chroma_of(&buf[0..4]) < 1e-4, "neutral stays neutral");
        let g1 = chroma_of(&buf[4..8]) / c1_before;
        let g2 = chroma_of(&buf[8..12]) / c2_before;
        assert!(g1 > 1.02 && g2 > 1.0, "both lifted: shadow={g1} mid={g2}");
        assert!(g1 > g2 + 0.05, "shadow lifted more than mid: {g1} vs {g2}");
    }

    #[test]
    fn warm_scene_warms_and_holds_luma() {
        let gray = [0.3f32, 0.3, 0.3];
        let mut buf = scene_buf_rgb(&[gray]);
        let l0 = luma_of(&buf[0..4]);
        warm_scene(&mut buf, 0.05);
        let (r, g, b) = (
            f16_bits_to_f32(buf[0]),
            f16_bits_to_f32(buf[1]),
            f16_bits_to_f32(buf[2]),
        );
        assert!(r > 0.3 && b < 0.3, "warmer: r={r} b={b}");
        let _ = g;
        assert!((luma_of(&buf[0..4]) - l0).abs() < 3e-3, "luma held: {l0}");
    }

    #[test]
    fn malvar_demosaic_beats_bilinear() {
        // Mosaic a known smooth (curved) gray pattern, then demosaic with Malvar and
        // with plain bilinear and compare to ground truth. Linear (bilinear) interp
        // loses curvature; Malvar's gradient correction recovers it → less error.
        let (w, h) = (40usize, 40usize);
        let cfa = rawloader::CFA::new("RGGB");
        let gt = |x: usize, y: usize| -> f32 {
            0.5 + 0.25 * ((x as f32) * 0.62).sin() * ((y as f32) * 0.31).cos()
        };
        // Gray, so the sampled CFA value equals gt regardless of which colour it is.
        let plane: Vec<f32> = (0..w * h).map(|i| gt(i % w, i / w)).collect();

        let (mut e_malvar, mut e_bilin) = (0.0f64, 0.0f64);
        for y in 2..h - 2 {
            for x in 2..w - 2 {
                let truth = gt(x, y);
                let m = demosaic_malvar(&plane, w, h, &cfa, y, x);
                let mut sum = [0.0f32; 3];
                let mut cnt = [0.0f32; 3];
                for dr in -1i32..=1 {
                    for dc in -1i32..=1 {
                        let nr = (y as i32 + dr) as usize;
                        let nc = (x as i32 + dc) as usize;
                        let col = cfa.color_at(nr, nc);
                        if col < 3 {
                            sum[col] += plane[nr * w + nc];
                            cnt[col] += 1.0;
                        }
                    }
                }
                let bl = [
                    sum[0] / cnt[0].max(1.0),
                    sum[1] / cnt[1].max(1.0),
                    sum[2] / cnt[2].max(1.0),
                ];
                for c in 0..3 {
                    e_malvar += (m[c] - truth).abs() as f64;
                    e_bilin += (bl[c] - truth).abs() as f64;
                }
            }
        }
        assert!(
            e_malvar < e_bilin,
            "Malvar should reconstruct with less error than bilinear: malvar={e_malvar:.3} bilinear={e_bilin:.3}"
        );
    }

    #[test]
    fn ahd_reduces_chroma_moire_vs_malvar() {
        // A GRAY image (any colour in the output is a demosaic artifact) that
        // oscillates at high horizontal frequency but is constant along columns:
        // the CORRECT interpolation direction is vertical. AHD's homogeneity test
        // should pick it and leave far less chroma (colour moiré) than Malvar's
        // fixed, non-directional kernel.
        let (w, h) = (64usize, 64usize);
        let cfa = rawloader::CFA::new("RGGB");
        let gt = |x: usize| 0.5 + 0.35 * ((x as f32) * 2.3).sin(); // near-Nyquist, gray
        let plane: Vec<f32> = (0..w * h).map(|i| gt(i % w)).collect();
        // Identity-ish normalized cam→XYZ so Lab tracks the gray directly.
        let cam2xyz = [
            [1.0, 0.0, 0.0, 0.0],
            [0.0, 1.0, 0.0, 0.0],
            [0.0, 0.0, 1.0, 0.0],
        ];
        let ahd = demosaic_ahd(&plane, w, h, &cfa, &cam2xyz);
        let chroma = |p: [f32; 3]| (p[0] - p[1]).abs() + (p[2] - p[1]).abs();
        let (mut e_ahd, mut e_malvar) = (0.0f64, 0.0f64);
        for y in 3..h - 3 {
            for x in 3..w - 3 {
                e_ahd += chroma(ahd[y * w + x]) as f64;
                e_malvar += chroma(demosaic_malvar(&plane, w, h, &cfa, y, x)) as f64;
            }
        }
        assert!(
            e_ahd < e_malvar,
            "AHD should leave less colour moiré than Malvar on gray high-frequency \
             detail: ahd={e_ahd:.3} malvar={e_malvar:.3}"
        );
    }

    #[test]
    fn is_raw_path_routes_by_extension() {
        assert!(is_raw_path(Path::new("foo.cr2")));
        assert!(is_raw_path(Path::new("FOO.CR2"))); // case-insensitive
        assert!(is_raw_path(Path::new("/a/b/c.NEF")));
        assert!(is_raw_path(Path::new("shot.arw")));
        assert!(!is_raw_path(Path::new("pic.png")));
        assert!(!is_raw_path(Path::new("pic.jpg")));
        assert!(!is_raw_path(Path::new("noext")));
    }

    // Diagnosis harness for the Develop colour/light report. Runs only when both
    // IAI_RAW_SAMPLE (a RAW file) and IAI_DEV_OUT (an output dir) are set. Dumps
    // PNGs + prints the 8-bit-path vs 16-bit-commit difference so the preview↔commit
    // gap and the flat-vs-base-curve question can be inspected without the GUI.
    #[test]
    fn develop_diagnosis() {
        use crate::core::develop::{apply_to_tilemap_direct, DevelopSettings};

        let (Some(sample), Some(outdir)) = (
            std::env::var_os("IAI_RAW_SAMPLE"),
            std::env::var_os("IAI_DEV_OUT"),
        ) else {
            return;
        };
        let path = std::path::PathBuf::from(sample);
        if !path.exists() {
            return;
        }
        let out = std::path::PathBuf::from(outdir);
        let canvas = RawImporter.import(&path).expect("decode");
        let (w, h) = (canvas.width, canvas.height);
        let tiles16 = canvas.layer_stack.layers[0].tiles.clone();
        assert!(tiles16.has_hdr(), "RAW doc must carry the 16-bit master");

        let save = |buf: &[u8], name: &str| {
            let img = image::RgbaImage::from_raw(w, h, buf.to_vec()).expect("dims");
            let scale = 1400.0 / w.max(h) as f32;
            let (pw, ph) = if scale < 1.0 {
                ((w as f32 * scale) as u32, (h as f32 * scale) as u32)
            } else {
                (w, h)
            };
            let small =
                image::imageops::resize(&img, pw, ph, image::imageops::FilterType::Triangle);
            small.save(out.join(name)).expect("save");
        };
        let to_u8 = |px16: &[u16]| -> Vec<u8> { px16.iter().map(|&v| (v >> 8) as u8).collect() };

        // Representative "kéo màu + ánh sáng" edit.
        let mut s = DevelopSettings::default();
        s.exposure = 8.0;
        s.contrast = 40.0;
        s.shadows = 60.0;
        s.highlights = -40.0;
        s.vibrance = 40.0;
        s.mixer_luminance[5] = 60.0; // a blue-ish band (sky)

        // 16-bit commit (the real Develop commit path).
        let c16 = apply_to_tilemap_direct(&tiles16, &s, None);
        let commit16 = to_u8(&c16.flatten16());
        // 8-bit path (what the GPU shader mirrors, precision-wise).
        let mut t8 = tiles16.clone();
        t8.drop_hdr();
        let c8 = apply_to_tilemap_direct(&t8, &s, None);
        let commit8 = c8.flatten();

        let (mut sumd, mut maxd, mut n) = (0u64, 0u32, 0u64);
        for (a, b) in commit16.chunks_exact(4).zip(commit8.chunks_exact(4)) {
            for k in 0..3 {
                let d = (a[k] as i32 - b[k] as i32).unsigned_abs();
                sumd += d as u64;
                maxd = maxd.max(d);
                n += 1;
            }
        }
        eprintln!(
            "8bit-path vs 16bit-commit: mean|Δ|={:.3}/255  max|Δ|={}/255",
            sumd as f64 / n as f64,
            maxd
        );

        // Flat R1 render vs a base contrast/black-point curve (what ACR applies as a
        // default "look" before any slider) — to show the flat-vs-reference gap.
        save(&canvas.export_flat(), "flat_r1.png");
        let mut base = DevelopSettings::default();
        base.contrast = 55.0;
        base.blacks = -22.0;
        base.vibrance = 18.0;
        let cb = apply_to_tilemap_direct(&tiles16, &base, None);
        save(&to_u8(&cb.flatten16()), "base_curve.png");
        save(&commit16, "commit16.png");
        save(&commit8, "commit8.png");

        // 1:1 native crops, so pixel-level / regional artifacts are visible (the
        // 1400px downscales above average them away).
        let save_crop = |flat: &[u8], cx: u32, cy: u32, cw: u32, ch: u32, name: &str| {
            let cw = cw.min(w);
            let ch = ch.min(h);
            let cx = cx.min(w - cw);
            let cy = cy.min(h - ch);
            let mut crop = vec![0u8; (cw * ch * 4) as usize];
            for y in 0..ch {
                let src = (((cy + y) * w + cx) * 4) as usize;
                let dst = (y * cw * 4) as usize;
                crop[dst..dst + (cw * 4) as usize]
                    .copy_from_slice(&flat[src..src + (cw * 4) as usize]);
            }
            image::RgbaImage::from_raw(cw, ch, crop)
                .expect("crop dims")
                .save(out.join(name))
                .expect("save crop");
        };
        // Wires against sky (demosaic zipper / sharpening beads).
        let (wx, wy) = ((w as f32 * 0.40) as u32, (h as f32 * 0.12) as u32);
        save_crop(&canvas.export_flat(), wx, wy, 320, 210, "crop_1to1.png");
        let mut sharp = DevelopSettings::default();
        sharp.sharpening = 90.0;
        let cs = apply_to_tilemap_direct(&tiles16, &sharp, None);
        save_crop(&cs.flatten(), wx, wy, 320, 210, "crop_sharp.png");

        // Combined LOCAL tone (Contrast + lifted Blacks + Shadows) on a dark region —
        // reproduces the "loang"/hard-boundary blotch the user sees when all three are
        // pushed together (the local-adaptation regional proxy amplified). Crop the
        // dark shop area (bottom-left) which has dark interior + lit boundaries.
        let mut lt = DevelopSettings::default();
        lt.contrast = 120.0;
        lt.blacks = 200.0;
        lt.shadows = 200.0;
        let clt = apply_to_tilemap_direct(&tiles16, &lt, None);
        let clt8 = to_u8(&clt.flatten16());
        save(&clt8, "localtone_full.png");
        save_crop(
            &clt8,
            (w as f32 * 0.02) as u32,
            (h as f32 * 0.60) as u32,
            480,
            320,
            "crop_localtone_a.png",
        );
        save_crop(
            &clt8,
            (w as f32 * 0.66) as u32,
            (h as f32 * 0.60) as u32,
            480,
            320,
            "crop_localtone_b.png",
        );
        eprintln!("wrote diagnosis PNGs + crops -> {}", out.to_string_lossy());
    }

    // End-to-end decode smoke test. Runs only when IAI_RAW_SAMPLE points at a real
    // RAW file (kept out of the repo), so the normal test suite stays portable.
    // Set IAI_RAW_PREVIEW to also dump a downscaled PNG for visual inspection.
    #[test]
    fn raw_decode_smoke() {
        let Some(sample) = std::env::var_os("IAI_RAW_SAMPLE") else {
            return;
        };
        let path = std::path::PathBuf::from(sample);
        if !path.exists() {
            return;
        }

        let canvas = RawImporter
            .import(&path)
            .expect("RAW decode should succeed");
        assert!(
            canvas.width > 0 && canvas.height > 0,
            "non-empty dimensions"
        );
        assert_eq!(
            canvas.bit_depth,
            crate::core::canvas::BitDepth::Sixteen,
            "RAW must open as a 16-bit document"
        );

        let mut flat = canvas.export_flat(); // 8-bit RGBA
        if flat.is_empty() {
            flat = canvas.layer_stack.layers[0].tiles.flatten();
        }
        assert_eq!(flat.len(), (canvas.width * canvas.height * 4) as usize);
        let mut sum = 0.0f64;
        for px in flat.chunks_exact(4) {
            sum += (px[0] as f64 + px[1] as f64 + px[2] as f64) / 3.0;
        }
        let mean = sum / (canvas.width * canvas.height) as f64;
        assert!(
            mean > 3.0 && mean < 252.0,
            "decoded image should not be near-black or near-white (mean={mean:.1})"
        );

        if let Some(preview) = std::env::var_os("IAI_RAW_PREVIEW") {
            let img = image::RgbaImage::from_raw(canvas.width, canvas.height, flat)
                .expect("buffer matches dimensions");
            let scale = 1400.0 / canvas.width.max(canvas.height) as f32;
            let (pw, ph) = if scale < 1.0 {
                (
                    (canvas.width as f32 * scale) as u32,
                    (canvas.height as f32 * scale) as u32,
                )
            } else {
                (canvas.width, canvas.height)
            };
            let small =
                image::imageops::resize(&img, pw, ph, image::imageops::FilterType::Triangle);
            small.save(&preview).expect("save preview");
            eprintln!("wrote preview {pw}x{ph} -> {}", preview.to_string_lossy());
        }
    }
}
