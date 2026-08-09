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
    "cr2", "crw", // Canon
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

/// XYZ (D65) → linear sRGB (D65), the standard primaries matrix.
const XYZ_TO_SRGB: [[f32; 3]; 3] = [
    [3.2404542, -1.5371385, -0.4985314],
    [-0.9692660, 1.8760108, 0.0415560],
    [0.0556434, -0.2040259, 1.0572252],
];

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

fn camera_to_srgb_matrix(cam2xyz: &[[f32; 4]; 3]) -> [[f32; 3]; 3] {
    let mut cam2srgb = [[0.0f32; 3]; 3];
    for i in 0..3 {
        for j in 0..3 {
            let mut s = 0.0;
            for k in 0..3 {
                s += XYZ_TO_SRGB[i][k] * cam2xyz[k][j];
            }
            cam2srgb[i][j] = s;
        }
    }
    cam2srgb
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

fn decode_raw(path: &Path) -> Result<Canvas, String> {
    let raw = rawloader::decode_file(path).map_err(|e| e.to_string())?;

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
    let cam2srgb = camera_to_srgb_matrix(&cam2xyz);

    let crop_top = area.top;
    let crop_left = area.left;
    let cfa = &raw.cfa;
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
            if !mono {
                correct_isolated_bayer_defects(&mut plane, w, h, cfa);
            }
            // Reconstruct clipped highlights on the mosaic, before demosaic.
            if !mono {
                inpaint_opposed_bayer(&mut plane, w, h, cfa, gain);
            }
            let plane = plane;

            // Whole-sensor AHD demosaic, computed once (skipped for mono and past
            // the pixel cap). The per-pixel loop below just reads it back; a None
            // here means the loop falls to Malvar/bilinear per pixel instead.
            let ahd_rgb: Option<Vec<[f32; 3]>> = if DEMOSAIC == DemosaicMethod::Ahd
                && !mono
                && w.saturating_mul(h) <= AHD_MAX_PIXELS
            {
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
                        write_scene(&mut row[dst..dst + 4], &cam2srgb, cam);
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
                        write_scene(&mut row[ox * 4..ox * 4 + 4], &cam2srgb, cam);
                    }
                });
        }
        other => return Err(format!("RAW {other} kênh/điểm ảnh chưa hỗ trợ")),
    }

    // Capture sharpening on the linear scene, after demosaic and before the
    // master is frozen (before orientation too, but the pass is isotropic so
    // the order is irrelevant).
    if CAPTURE_SHARPEN {
        capture_sharpen(&mut out, cw, ch);
    }

    // Apply EXIF orientation so portraits aren't sideways. The buffer holds f16
    // bits at this point; orientation only moves 4-u16 pixels, so it is agnostic.
    let (out, fw, fh) = apply_orientation(out, cw, ch, raw.orientation);

    // Baseline exposure: lift the scene so the default render matches the camera's
    // embedded-JPEG brightness. A scene-referred RAW otherwise opens flatter and
    // darker than that preview (the camera bakes its picture-style tone into the
    // JPEG), which reads as the image "jumping dark" once the full decode replaces
    // the instant preview. Best-effort: files without a preview are left as-is.
    let preview_stats = crate::formats::raw_preview::take_cached_stats(path).or_else(|| {
        std::fs::read(path)
            .ok()
            .and_then(|bytes| crate::formats::raw_preview::preview_stats_from_bytes(&bytes))
    });
    let mut scene = SceneSource {
        width: fw as u32,
        height: fh as u32,
        half: out,
        alpha: None,
        look: crate::core::develop_scene::BaseLook::Raw,
        color_pipeline: crate::core::working_color::ColorPipelineMetadata::default(),
        camera_rgb_curve: None,
        auto_tone: std::sync::OnceLock::new(),
    };
    if let Some(target) = preview_stats {
        let gain =
            crate::core::develop_scene::baseline_rgb_gains_for_scene(&scene, target.mean_rgb);
        if gain.iter().any(|g| (g - 1.0).abs() > 0.01) {
            scale_scene_rgb(&mut scene.half, gain);
            scene.auto_tone = std::sync::OnceLock::new();
        }
        let matrix = crate::core::develop_scene::fit_camera_color_matrix(
            &scene,
            &target.thumbnail_rgb,
            target.thumbnail_width,
            target.thumbnail_height,
        );
        if camera_color_matrix_is_material(matrix) {
            transform_scene_rgb(&mut scene.half, matrix);
            scene.auto_tone = std::sync::OnceLock::new();
        }
        scene.camera_rgb_curve = Some(crate::core::develop_scene::fit_camera_rgb_curve(
            &scene,
            &target.histogram,
        ));
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

/// Above this sensor pixel count AHD's whole-frame transient buffers and Lab
/// pass are too costly for an interactive open, so use the sharp O(1)-memory
/// Malvar path. Debug builds need a lower cap because the unoptimised AHD math
/// can otherwise hold Develop on its loading screen for several minutes.
const AHD_MAX_PIXELS: usize = if cfg!(debug_assertions) {
    4_000_000
} else {
    12_000_000
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
// Disabled by default: two strong unsharp iterations create dark undershoot
// beads along hair/skin boundaries. Sharpening belongs to the explicit Detail
// stage, where the user can control it and preview the result.
const CAPTURE_SHARPEN: bool = false;
const CS_ITERATIONS: usize = 2;
/// Gaussian radius of the unsharp blur, sensor px.
const CS_SIGMA: f32 = 0.7;
/// Per-iteration unsharp gain.
const CS_GAIN: f32 = 0.55;
/// Relative-contrast guard: fully closed below LO, fully open above HI.
const CS_GUARD_LO: f32 = 0.04;
const CS_GUARD_HI: f32 = 0.15;
/// Level floor in the relative-contrast denominator (damps deep-shadow blowup).
const CS_GUARD_FLOOR: f32 = 0.02;

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
fn capture_sharpen(half: &mut [u16], w: usize, h: usize) {
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
                let factor = ((l + CS_GAIN * guard * d) / l).clamp(0.5, 2.0);
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
fn write_scene(dst: &mut [u16], m: &[[f32; 3]; 3], cam: [f32; 3]) {
    let srgb = camera_to_linear_srgb(m, cam);
    let working =
        crate::core::working_color::WorkingColorSpace::LinearProPhoto.from_linear_srgb(srgb);
    dst[0] = f32_to_f16_bits(working[0]);
    dst[1] = f32_to_f16_bits(working[1]);
    dst[2] = f32_to_f16_bits(working[2]);
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
    fn camera_to_srgb_matrix_composes_xyz_rows() {
        let identity_cam2xyz = [
            [1.0, 0.0, 0.0, 0.0],
            [0.0, 1.0, 0.0, 0.0],
            [0.0, 0.0, 1.0, 0.0],
        ];
        let m = camera_to_srgb_matrix(&identity_cam2xyz);
        for y in 0..3 {
            for x in 0..3 {
                assert!(
                    (m[y][x] - XYZ_TO_SRGB[y][x]).abs() < 1e-6,
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
        let cam2srgb = camera_to_srgb_matrix(&cam2xyz);
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
        capture_sharpen(&mut buf, w, h);
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
        capture_sharpen(&mut buf, w, h);
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
    fn automatic_capture_sharpen_stays_disabled() {
        assert!(!CAPTURE_SHARPEN);
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
