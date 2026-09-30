//! One-off analysis of a photo for portrait retouching: find faces, fit the
//! face mesh, segment parts when the Sapiens2 model is present, then precompute
//! each face's masks and frequency split so slider changes only recombine.

use rayon::prelude::*;

use super::blur::{blur4, masked_blur};
use super::geometry::*;
use crate::core::ai::body_parts::{self, PartLabels, Segmenter};
use crate::core::ai::face_mesh::{self, FaceMesh};

/// Share of mesh landmarks that must land on face-like part classes before the
/// part masks are trusted over the mesh-only fallback.
pub const TRUSTED_AGREEMENT: f32 = 0.8;
/// Fixed-point scale of the stored blemish score.
pub(super) const BLEMISH_SCALE: f32 = 32.0;

pub struct FaceModel {
    pub mesh: FaceMesh,
    /// Mesh/part agreement; `None` when the part model was not run.
    pub agreement: Option<f32>,
    pub region: Region,
    pub extent: f32,
    pub(super) skin: Vec<u8>,
    pub(super) eye_white: Vec<u8>,
    pub(super) iris: Vec<u8>,
    pub(super) under_eye: Vec<u8>,
    pub(super) teeth: Vec<u8>,
    pub(super) interior: Vec<u8>,
    pub(super) blemish: Vec<u8>,
    pub(super) low1: Vec<[u16; 3]>,
    pub(super) low2: Vec<[u16; 3]>,
    pub(super) broad: Vec<u16>,
    pub(super) skin_mean: [f32; 3],
    pub(super) cheek_luma: f32,
}

impl FaceModel {
    pub fn trusted(&self) -> bool {
        self.agreement.is_some_and(|a| a >= TRUSTED_AGREEMENT)
    }
}

pub struct PortraitModel {
    pub width: u32,
    pub height: u32,
    pub faces: Vec<FaceModel>,
    /// Whether the Sapiens2 part model ran (else every face uses mesh masks).
    pub parts_used: bool,
    pub parts_on_gpu: bool,
    /// Why the part model was skipped, if it was.
    pub parts_note: Option<String>,
    /// Milliseconds spent finding faces, loading and running the part model,
    /// and preparing the faces.
    pub timings: [u128; 4],
}

pub(super) fn luma(c: [f32; 3]) -> f32 {
    0.299 * c[0] + 0.587 * c[1] + 0.114 * c[2]
}

fn smoothstep(edge0: f32, edge1: f32, x: f32) -> f32 {
    let t = ((x - edge0) / (edge1 - edge0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

fn to_u16(c: [f32; 3]) -> [u16; 3] {
    c.map(|v| (v.clamp(0.0, 1.0) * 65535.0).round() as u16)
}

fn to_u8(v: f32) -> u8 {
    (v.clamp(0.0, 1.0) * 255.0).round() as u8
}

/// Largest value within `radius` pixels along each axis (a square dilation).
fn max_filter(values: &[f32], width: usize, height: usize, radius: usize) -> Vec<f32> {
    if radius == 0 {
        return values.to_vec();
    }
    let mut rows = vec![0.0f32; values.len()];
    rows.par_chunks_mut(width)
        .enumerate()
        .for_each(|(y, line)| {
            let source = &values[y * width..(y + 1) * width];
            for (x, cell) in line.iter_mut().enumerate() {
                let lo = x.saturating_sub(radius);
                let hi = (x + radius + 1).min(width);
                *cell = source[lo..hi].iter().copied().fold(0.0, f32::max);
            }
        });
    let mut out = vec![0.0f32; values.len()];
    out.par_chunks_mut(width).enumerate().for_each(|(y, line)| {
        let lo = y.saturating_sub(radius);
        let hi = (y + radius + 1).min(height);
        for (x, cell) in line.iter_mut().enumerate() {
            *cell = (lo..hi).map(|yy| rows[yy * width + x]).fold(0.0, f32::max);
        }
    });
    out
}

/// Analyse `rgba` (straight RGBA, `width * height`). `progress` receives short
/// Vietnamese status lines for the dialog.
pub fn analyze(
    rgba: &[u8],
    width: u32,
    height: u32,
    prefer_gpu: bool,
    progress: &(dyn Fn(String) + Sync),
) -> Result<PortraitModel, String> {
    progress("Đang tìm khuôn mặt…".to_string());
    let mut timings = [0u128; 4];
    let started = std::time::Instant::now();
    let meshes = face_mesh::detect(rgba, width, height)?;
    timings[0] = started.elapsed().as_millis();
    if meshes.is_empty() {
        return Err("không tìm thấy khuôn mặt nào".to_string());
    }
    let mut parts: Vec<Option<PartLabels>> = (0..meshes.len()).map(|_| None).collect();
    let mut parts_on_gpu = false;
    let mut parts_note = None;
    if body_parts::model_path().is_some() {
        progress("Đang nạp model tách vùng…".to_string());
        let started = std::time::Instant::now();
        match Segmenter::load(prefer_gpu) {
            Ok(mut segmenter) => {
                timings[1] = started.elapsed().as_millis();
                let started = std::time::Instant::now();
                for (i, mesh) in meshes.iter().enumerate() {
                    progress(format!("Đang tách vùng mặt {}/{}…", i + 1, meshes.len()));
                    match segmenter.segment_face(rgba, width, height, mesh) {
                        Ok(labels) => parts[i] = Some(labels),
                        Err(error) => parts_note = Some(error),
                    }
                }
                parts_on_gpu = segmenter.on_gpu;
                timings[2] = started.elapsed().as_millis();
            }
            Err(error) => parts_note = Some(error),
        }
    } else {
        parts_note = Some("chưa có model tách vùng Sapiens2 — dùng mốc mặt".to_string());
    }
    let parts_used = parts.iter().any(Option::is_some);
    let owners: Vec<([f32; 2], f32)> = meshes
        .iter()
        .map(|mesh| {
            let (centre, extent, _) = mesh.frame();
            (centre, extent)
        })
        .collect();
    let started = std::time::Instant::now();
    let mut faces = Vec::with_capacity(meshes.len());
    let count = meshes.len();
    for (i, (mesh, part)) in meshes.into_iter().zip(parts).enumerate() {
        progress(format!("Đang chuẩn bị mặt {}/{}…", i + 1, count));
        faces.push(build_face(
            rgba,
            width,
            height,
            mesh,
            part.as_ref(),
            i,
            &owners,
        ));
    }
    timings[3] = started.elapsed().as_millis();
    Ok(PortraitModel {
        width,
        height,
        faces,
        parts_used,
        parts_on_gpu,
        parts_note,
        timings,
    })
}

fn build_face(
    rgba: &[u8],
    width: u32,
    height: u32,
    mesh: FaceMesh,
    parts: Option<&PartLabels>,
    index: usize,
    owners: &[([f32; 2], f32)],
) -> FaceModel {
    let (_, extent, angle) = mesh.frame();
    let e = extent;
    let region = Region::around(
        mesh.points.iter().map(|p| [p[0], p[1]]),
        [0.15 * e, 0.15 * e, 0.15 * e, 0.45 * e],
        width,
        height,
    );
    let (w, h) = (region.w as usize, region.h as usize);
    let n = region.len();
    let pixel_xy = |i: usize| {
        (
            region.x as f32 + (i % w) as f32 + 0.5,
            region.y as f32 + (i / w) as f32 + 0.5,
        )
    };
    let src: Vec<[f32; 3]> = (0..n)
        .into_par_iter()
        .map(|i| {
            let x = region.x as usize + i % w;
            let y = region.y as usize + i / w;
            let o = (y * width as usize + x) * 4;
            [
                rgba[o] as f32 / 255.0,
                rgba[o + 1] as f32 / 255.0,
                rgba[o + 2] as f32 / 255.0,
            ]
        })
        .collect();
    let agreement = parts.map(|p| p.agreement);
    let trusted = agreement.is_some_and(|a| a >= TRUSTED_AGREEMENT);
    let points = &mesh.points;

    // Features that are never skin.
    let mut exclude = vec![0.0f32; n];
    for eye in [&RIGHT_EYE[..], &LEFT_EYE[..]] {
        stamp_polygon(
            &mut exclude,
            region,
            &loop_points(points, eye),
            0.02 * e,
            0.02 * e,
        );
    }
    for brow in [&RIGHT_BROW[..], &LEFT_BROW[..]] {
        stamp_polygon(
            &mut exclude,
            region,
            &loop_points(points, brow),
            0.015 * e,
            0.02 * e,
        );
    }
    let wing_a = points[NOSE_WINGS[0] as usize];
    let wing_b = points[NOSE_WINGS[1] as usize];
    let below = points[SUBNASALE as usize];
    let nostril_centre = [
        (wing_a[0] + wing_b[0]) * 0.5 * 0.6 + below[0] * 0.4,
        (wing_a[1] + wing_b[1]) * 0.5 * 0.6 + below[1] * 0.4,
    ];
    let half_width = ((wing_b[0] - wing_a[0]).hypot(wing_b[1] - wing_a[1]) * 0.5).max(1.0);
    let (sin, cos) = angle.sin_cos();
    let nostrils: Vec<[f32; 2]> = (0..24)
        .map(|k| {
            let t = k as f32 / 24.0 * std::f32::consts::TAU;
            let (u, v) = (t.cos() * half_width * 0.95, t.sin() * half_width * 0.42);
            [
                nostril_centre[0] + cos * u - sin * v,
                nostril_centre[1] + sin * u + cos * v,
            ]
        })
        .collect();
    stamp_polygon(&mut exclude, region, &nostrils, 0.0, 0.03 * e);

    // Cheek reference colour from the source.
    let cheek_samples: Vec<[f32; 3]> = CHEEKS
        .iter()
        .filter_map(|&k| {
            let p = points[k as usize];
            let (x, y) = (p[0] - region.x as f32, p[1] - region.y as f32);
            (x >= 0.0 && y >= 0.0 && (x as usize) < w && (y as usize) < h)
                .then(|| src[y as usize * w + x as usize])
        })
        .collect();
    let cheek_colour = if cheek_samples.is_empty() {
        [0.8, 0.6, 0.5]
    } else {
        let mut sum = [0.0f32; 3];
        for c in &cheek_samples {
            for k in 0..3 {
                sum[k] += c[k] / cheek_samples.len() as f32;
            }
        }
        sum
    };

    // Mesh-only fallback: the face outline limited to skin-coloured pixels.
    let mut oval = vec![0.0f32; n];
    if !trusted {
        stamp_polygon(
            &mut oval,
            region,
            &loop_points(points, &FACE_OVAL),
            -0.01 * e,
            0.05 * e,
        );
        stamp_polygon(
            &mut exclude,
            region,
            &loop_points(points, &LIPS_OUTER),
            0.01 * e,
            0.02 * e,
        );
    }
    let cheek_chroma = {
        let y = luma(cheek_colour);
        cheek_colour.map(|v| v - y)
    };
    let own_centre = owners[index];
    let skin: Vec<f32> = (0..n)
        .into_par_iter()
        .map(|i| {
            let (x, y) = pixel_xy(i);
            // Pixels nearer another face (in face sizes) belong to that face.
            let own = ((x - own_centre.0[0]).hypot(y - own_centre.0[1])) / own_centre.1;
            if owners
                .iter()
                .enumerate()
                .any(|(j, (c, s))| j != index && (x - c[0]).hypot(y - c[1]) / s < own)
            {
                return 0.0;
            }
            let base = match parts {
                Some(p) if trusted => p.groups_at(x, y)[body_parts::GROUP_FACE_SKIN],
                _ => {
                    let c = src[i];
                    let yl = luma(c);
                    let distance = (0..3)
                        .map(|k| (c[k] - yl - cheek_chroma[k]).powi(2))
                        .sum::<f32>()
                        .sqrt();
                    oval[i] * (1.0 - smoothstep(0.05, 0.12, distance))
                }
            };
            base * (1.0 - exclude[i])
        })
        .collect();

    // Eyes: whites and irises.
    let mut eye_area = vec![0.0f32; n];
    let mut irises = vec![0.0f32; n];
    for (eye, (centre, rim)) in [(&RIGHT_EYE[..], RIGHT_IRIS), (&LEFT_EYE[..], LEFT_IRIS)] {
        stamp_polygon(
            &mut eye_area,
            region,
            &loop_points(points, eye),
            -0.004 * e,
            0.01 * e,
        );
        let c = points[centre as usize];
        let radius = rim
            .iter()
            .map(|&k| {
                let p = points[k as usize];
                (p[0] - c[0]).hypot(p[1] - c[1])
            })
            .sum::<f32>()
            / 4.0;
        stamp_disc(&mut irises, region, [c[0], c[1]], radius * 1.05, 0.008 * e);
    }
    let eye_white: Vec<u8> = (0..n)
        .into_par_iter()
        .map(|i| {
            let y = luma(src[i]);
            to_u8(eye_area[i] * (1.0 - irises[i]) * smoothstep(0.28, 0.5, y))
        })
        .collect();
    let mut eye_lids = vec![0.0f32; n];
    for eye in [&RIGHT_EYE[..], &LEFT_EYE[..]] {
        stamp_polygon(
            &mut eye_lids,
            region,
            &loop_points(points, eye),
            0.0,
            0.01 * e,
        );
    }
    let iris: Vec<u8> = (0..n)
        .into_par_iter()
        .map(|i| to_u8(irises[i] * eye_lids[i]))
        .collect();

    // Under-eye bands hanging below each lower lid.
    let down = [-sin, cos];
    let mut bands = vec![0.0f32; n];
    for lower in [&RIGHT_EYE_LOWER[..], &LEFT_EYE_LOWER[..]] {
        let lid = loop_points(points, lower);
        let last = (lid.len() - 1) as f32;
        let mut band: Vec<[f32; 2]> = lid
            .iter()
            .map(|p| [p[0] + down[0] * 0.015 * e, p[1] + down[1] * 0.015 * e])
            .collect();
        for (k, p) in lid.iter().enumerate().rev() {
            let t = k as f32 / last * 2.0 - 1.0;
            let depth = 0.015 * e + 0.11 * e * (1.0 - 0.45 * t * t);
            band.push([p[0] + down[0] * depth, p[1] + down[1] * depth]);
        }
        stamp_polygon(&mut bands, region, &band, 0.0, 0.05 * e);
    }
    let under_eye: Vec<u8> = (0..n)
        .into_par_iter()
        .map(|i| to_u8(bands[i] * skin[i]))
        .collect();

    // Teeth: bright pixels inside the mouth.
    let mut mouth = vec![0.0f32; n];
    if !trusted {
        stamp_polygon(
            &mut mouth,
            region,
            &loop_points(points, &MOUTH_INNER),
            0.0,
            0.01 * e,
        );
    }
    let teeth: Vec<u8> = (0..n)
        .into_par_iter()
        .map(|i| {
            let c = src[i];
            let y = luma(c);
            let (x, py) = pixel_xy(i);
            let base = match parts {
                Some(p) if trusted => p.groups_at(x, py)[body_parts::GROUP_TEETH],
                _ => {
                    let max = c[0].max(c[1]).max(c[2]);
                    let min = c[0].min(c[1]).min(c[2]);
                    let saturation = if max > 0.0 { (max - min) / max } else { 0.0 };
                    mouth[i] * (1.0 - smoothstep(0.2, 0.45, saturation))
                }
            };
            to_u8(base * smoothstep(0.25, 0.5, y))
        })
        .collect();

    // Frequency split, weighted to skin so hair and features do not bleed in.
    let r1 = (e / 220.0).max(1.0);
    let r2 = (e / 28.0).max(3.0);
    let r3 = (e / 7.0).max(6.0);
    let low1 = masked_blur(&src, &skin, w, h, r1);
    let low2 = masked_blur(&src, &skin, w, h, r2);
    let broad: Vec<u16> = masked_blur(&src, &skin, w, h, r3)
        .into_par_iter()
        .map(|c| (luma(c).clamp(0.0, 1.0) * 65535.0).round() as u16)
        .collect();

    let (mut weight, mut mean) = (0.0f64, [0.0f64; 3]);
    for i in 0..n {
        let m = skin[i] as f64;
        weight += m;
        for k in 0..3 {
            mean[k] += low2[i][k] as f64 * m;
        }
    }
    let skin_mean = if weight > 1.0 {
        mean.map(|v| (v / weight) as f32)
    } else {
        cheek_colour
    };
    let cheek_values: Vec<f32> = CHEEKS
        .iter()
        .filter_map(|&k| {
            let p = points[k as usize];
            let (x, y) = (p[0] - region.x as f32, p[1] - region.y as f32);
            (x >= 0.0 && y >= 0.0 && (x as usize) < w && (y as usize) < h)
                .then(|| luma(low2[y as usize * w + x as usize]))
        })
        .collect();
    let cheek_luma = if cheek_values.is_empty() {
        luma(skin_mean)
    } else {
        cheek_values.iter().sum::<f32>() / cheek_values.len() as f32
    };

    // How deep inside the skin each pixel sits: 1 well inside, falling to 0 at
    // the outline, so smoothing fades out before it can halo the edges.
    let mut spread_mask: Vec<[f32; 4]> = skin.iter().map(|&m| [m, 0.0, 0.0, 0.0]).collect();
    blur4(&mut spread_mask, w, h, r2);
    let interior: Vec<f32> = spread_mask
        .par_iter()
        .zip(skin.par_iter())
        .map(|(b, &m)| smoothstep(0.55, 0.92, b[0]) * m)
        .collect();

    // Blemishes: compact spots darker or redder than a ring around them in
    // every direction (creases and outlines fail the ring test along their
    // length), in units of the face's own skin texture spread.
    let (mut spread, mut spread_weight) = (0.0f64, 0.0f64);
    for i in 0..n {
        let m = interior[i] as f64;
        let d = luma(low1[i]) - luma(low2[i]);
        spread += (d * d) as f64 * m;
        spread_weight += m;
    }
    let sigma = ((spread / spread_weight.max(1.0)).sqrt() as f32).max(1.0 / 255.0);
    let tone: Vec<[f32; 2]> = low1
        .par_iter()
        .map(|c| [luma(*c), c[0] - (c[1] + c[2]) * 0.5])
        .collect();
    let radii = [(e / 110.0).max(2.0), (e / 55.0).max(3.0)];
    let directions: Vec<[f32; 2]> = (0..8)
        .map(|k| {
            let a = k as f32 * std::f32::consts::FRAC_PI_4;
            [a.cos(), a.sin()]
        })
        .collect();
    let score: Vec<[f32; 4]> = (0..n)
        .into_par_iter()
        .map(|i| {
            if interior[i] < 0.8 {
                return [0.0; 4];
            }
            let (x, y) = ((i % w) as f32, (i / w) as f32);
            let centre = tone[i];
            let mut best = 0.0f32;
            for radius in radii {
                let (mut dark, mut red) = (f32::MAX, f32::MAX);
                for d in &directions {
                    let (sx, sy) = (x + d[0] * radius, y + d[1] * radius);
                    if sx < 0.0 || sy < 0.0 || sx >= w as f32 || sy >= h as f32 {
                        return [0.0; 4];
                    }
                    let ring = tone[sy as usize * w + sx as usize];
                    dark = dark.min(ring[0] - centre[0]);
                    red = red.min(centre[1] - ring[1]);
                }
                best = best.max((dark.max(0.0) + 0.5 * red.max(0.0)) / sigma);
            }
            [best, 0.0, 0.0, 0.0]
        })
        .collect();
    // The ring test peaks at a spot's centre; spread each peak over the spot's
    // radius, then soften the rim.
    let raw: Vec<f32> = score.iter().map(|s| s[0]).collect();
    let spread = max_filter(&raw, w, h, radii[0].round() as usize);
    let mut soft: Vec<[f32; 4]> = spread.iter().map(|&v| [v, 0.0, 0.0, 0.0]).collect();
    blur4(&mut soft, w, h, (radii[0] / 3.0).max(1.0));
    let blemish: Vec<u8> = soft
        .par_iter()
        .map(|s| (s[0] * BLEMISH_SCALE).round().clamp(0.0, 255.0) as u8)
        .collect();

    FaceModel {
        mesh,
        agreement,
        region,
        extent,
        skin: skin.into_par_iter().map(to_u8).collect(),
        interior: interior.into_par_iter().map(to_u8).collect(),
        eye_white,
        iris,
        under_eye,
        teeth,
        blemish,
        low1: low1.into_par_iter().map(to_u16).collect(),
        low2: low2.into_par_iter().map(to_u16).collect(),
        broad,
        skin_mean,
        cheek_luma,
    }
}
