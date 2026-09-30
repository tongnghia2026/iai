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
    /// Spot strength (score, fixed point) and soft disc coverage.
    pub(super) spot_score: Vec<u8>,
    pub(super) spot_cover: Vec<u8>,
    /// Offset (dx, dy) to clean skin whose texture heals each spot pixel, and
    /// the local skin colour around spots used to match that texture.
    pub(super) donor: Vec<[i16; 2]>,
    pub(super) heal_base: Vec<[u16; 3]>,
    pub(super) lips: Vec<u8>,
    pub(super) brows: Vec<u8>,
    /// Nose contour: +127 lights the bridge, -127 shades its sides.
    pub(super) nose: Vec<i8>,
    /// Hair lies outside the face region, so it has its own (head and
    /// shoulders) region and mask; empty without the part model.
    pub hair_region: Region,
    pub(super) hair: Vec<u8>,
    /// Eyes, lashes, brows and lips: where sharpening applies.
    pub(super) detail: Vec<u8>,
    /// Plain small blur of the photo, the sharpening reference.
    pub(super) soft: Vec<[u16; 3]>,
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

/// Guided filter of `mask` with a grey `guide`: snaps a soft mask onto the
/// photo's own edges. Never grows the mask where it had nothing nearby.
fn guided(mask: &[f32], guide: &[f32], w: usize, h: usize, radius: f32, eps: f32) -> Vec<f32> {
    let mut stats: Vec<[f32; 4]> = mask
        .par_iter()
        .zip(guide.par_iter())
        .map(|(&p, &g)| [g, p, g * g, g * p])
        .collect();
    blur4(&mut stats, w, h, radius);
    let mut coefficients: Vec<[f32; 4]> = stats
        .par_iter()
        .map(|m| {
            let variance = (m[2] - m[0] * m[0]).max(0.0);
            let covariance = m[3] - m[0] * m[1];
            let a = covariance / (variance + eps);
            [a, m[1] - a * m[0], m[1], 0.0]
        })
        .collect();
    blur4(&mut coefficients, w, h, radius);
    coefficients
        .par_iter()
        .zip(guide.par_iter())
        .map(|(c, &g)| ((c[0] * g + c[1]).clamp(0.0, 1.0)) * smoothstep(0.0, 0.05, c[2]))
        .collect()
}

/// Fade toward the region sides flagged in `open_sides` (left, top, right,
/// bottom): those sides lie inside the image, so an effect must not end there
/// in a hard line.
fn side_fade(i: usize, w: usize, h: usize, open_sides: [bool; 4], fade: f32) -> f32 {
    let (x, y) = ((i % w) as f32 + 0.5, (i / w) as f32 + 0.5);
    let distances = [x, y, w as f32 - x, h as f32 - y];
    open_sides
        .iter()
        .zip(distances)
        .filter(|(open, _)| **open)
        .map(|(_, d)| smoothstep(0.0, fade, d))
        .product()
}

/// Near a mask's outline, keep only pixels whose colour matches the mask's
/// solid core: a chroma model (mean and covariance of two colour-difference
/// axes) from pixels deep inside, then a Mahalanobis cut. The core itself is
/// left alone, so off-colour spots inside (a red pimple) still count.
fn gate_by_colour(mask: &[f32], colours: &[[f32; 3]], w: usize, h: usize, radius: f32) -> Vec<f32> {
    let n = w * h;
    let mut core: Vec<[f32; 4]> = mask.iter().map(|&m| [m, 0.0, 0.0, 0.0]).collect();
    blur4(&mut core, w, h, radius);
    let core: Vec<f32> = core
        .par_iter()
        .map(|c| smoothstep(0.7, 0.95, c[0]))
        .collect();
    let chroma = |c: [f32; 3]| {
        let y = luma(c);
        [c[2] - y, c[0] - y]
    };
    let (mut total, mut mean, mut cov) = (0.0f64, [0.0f64; 2], [0.0f64; 3]);
    for i in 0..n {
        if core[i] > 0.9 && mask[i] > 0.9 {
            let [u, v] = chroma(colours[i]);
            total += 1.0;
            mean[0] += u as f64;
            mean[1] += v as f64;
            cov[0] += (u * u) as f64;
            cov[1] += (u * v) as f64;
            cov[2] += (v * v) as f64;
        }
    }
    if total <= 200.0 {
        return mask.to_vec();
    }
    let (mu, mv) = (mean[0] / total, mean[1] / total);
    let floor = 1e-5;
    let (suu, suv, svv) = (
        (cov[0] / total - mu * mu).max(floor) as f32,
        (cov[1] / total - mu * mv) as f32,
        (cov[2] / total - mv * mv).max(floor) as f32,
    );
    let (mu, mv) = (mu as f32, mv as f32);
    let det = (suu * svv - suv * suv).max(1e-12);
    (0..n)
        .into_par_iter()
        .map(|i| {
            let [u, v] = chroma(colours[i]);
            let (du, dv) = (u - mu, v - mv);
            let d2 = (svv * du * du - 2.0 * suv * du * dv + suu * dv * dv) / det;
            let gate = 1.0 - smoothstep(9.0, 25.0, d2);
            mask[i] * (core[i] + (1.0 - core[i]) * gate)
        })
        .collect()
}

/// Tighten the soft, low-resolution skin mask onto the photo: a guided filter
/// (luma guide) snaps its outline to real edges such as the hairline, pixels
/// near the outline must also have the face's skin colour, and the mask fades
/// out toward region sides inside the image.
fn refine_skin(
    mask: Vec<f32>,
    src: &[[f32; 3]],
    w: usize,
    h: usize,
    e: f32,
    open_sides: [bool; 4],
) -> Vec<f32> {
    let n = w * h;
    let guide: Vec<f32> = src.par_iter().map(|&c| luma(c)).collect();
    let snapped = guided(&mask, &guide, w, h, (e / 80.0).max(2.0), 0.0015);

    let gated = gate_by_colour(&snapped, src, w, h, (e / 28.0).max(3.0));
    let fade = (0.06 * e).max(4.0);
    (0..n)
        .into_par_iter()
        .map(|i| gated[i] * side_fade(i, w, h, open_sides, fade))
        .collect()
}

/// A blemish found by the ring test, in region pixels.
struct Spot {
    x: f32,
    y: f32,
    score: f32,
    radius: f32,
}

/// Visit the pixels within `reach` of (cx, cy) with their distance.
fn for_disc(w: usize, h: usize, cx: f32, cy: f32, reach: f32, mut visit: impl FnMut(usize, f32)) {
    let x0 = (cx - reach).floor().max(0.0) as usize;
    let y0 = (cy - reach).floor().max(0.0) as usize;
    let x1 = ((cx + reach).ceil() as usize).min(w);
    let y1 = ((cy + reach).ceil() as usize).min(h);
    for y in y0..y1 {
        for x in x0..x1 {
            let d = (x as f32 + 0.5 - cx).hypot(y as f32 + 0.5 - cy);
            if d <= reach {
                visit(y * w + x, d);
            }
        }
    }
}

/// Ring-test peaks become spots. Peaks packed densely (stubble, nose pores)
/// are skin texture rather than blemishes, so they are damped.
fn find_spots(ring: &[(f32, usize)], radii: &[f32; 2], w: usize, h: usize, e: f32) -> Vec<Spot> {
    // The most permissive slider threshold; weaker peaks never matter.
    const FLOOR: f32 = 0.3;
    let raw: Vec<f32> = ring.iter().map(|r| r.0).collect();
    let local = max_filter(&raw, w, h, 2);
    let mut peaks: Vec<Spot> = (0..raw.len())
        .filter(|&i| raw[i] >= FLOOR && raw[i] >= local[i])
        .map(|i| Spot {
            x: (i % w) as f32 + 0.5,
            y: (i / w) as f32 + 0.5,
            score: raw[i],
            radius: radii[ring[i].1] * 0.9,
        })
        .collect();
    peaks.sort_by(|a, b| b.score.total_cmp(&a.score));
    let mut kept: Vec<Spot> = Vec::new();
    for p in peaks {
        if kept.len() >= 3000 {
            break;
        }
        if kept
            .iter()
            .all(|k| (k.x - p.x).hypot(k.y - p.y) > k.radius.max(p.radius) * 0.8)
        {
            kept.push(p);
        }
    }
    let crowd = e / 12.0;
    let neighbours: Vec<usize> = kept
        .iter()
        .map(|a| {
            kept.iter()
                .filter(|b| (a.x - b.x).hypot(a.y - b.y) < crowd)
                .count()
                - 1
        })
        .collect();
    for (spot, count) in kept.iter_mut().zip(neighbours) {
        spot.score *= (1.0 - count.saturating_sub(5) as f32 * 0.12).clamp(0.2, 1.0);
    }
    kept
}

/// Per spot, pick nearby clean skin to borrow texture from (toward the face
/// centre first), and paint soft discs of spot strength, coverage and donor
/// offset. Also the local skin colour around spots, measured without them.
#[allow(clippy::type_complexity)]
fn heal_spots(
    spots: &[Spot],
    src: &[[f32; 3]],
    interior: &[f32],
    w: usize,
    h: usize,
    centre: [f32; 2],
    small_radius: f32,
) -> (Vec<u8>, Vec<u8>, Vec<[i16; 2]>, Vec<[u16; 3]>) {
    let n = w * h;
    let mut score = vec![0u8; n];
    let mut cover = vec![0u8; n];
    if spots.is_empty() {
        return (score, cover, Vec::new(), Vec::new());
    }
    let mut taken = vec![0.0f32; n];
    for s in spots {
        for_disc(w, h, s.x, s.y, s.radius * 1.4, |i, _| taken[i] = 1.0);
    }
    let clean = |x: f32, y: f32| {
        if x < 0.0 || y < 0.0 || x >= w as f32 || y >= h as f32 {
            return false;
        }
        let i = y as usize * w + x as usize;
        interior[i] >= 0.8 && taken[i] < 0.5
    };
    let mut donor = vec![[0i16; 2]; n];
    let mut owner = vec![0.0f32; n];
    for s in spots {
        let (mut bx, mut by) = (centre[0] - s.x, centre[1] - s.y);
        let length = bx.hypot(by);
        if length > 1e-3 {
            bx /= length;
            by /= length;
        } else {
            (bx, by) = (1.0, 0.0);
        }
        let reach = s.radius * 2.5 + 2.0;
        let mut offset = [0i16; 2];
        for degrees in [0.0f32, 60.0, -60.0, 120.0, -120.0, 180.0] {
            let (sin, cos) = degrees.to_radians().sin_cos();
            let (dx, dy) = ((bx * cos - by * sin) * reach, (bx * sin + by * cos) * reach);
            let (qx, qy) = (s.x + dx, s.y + dy);
            let r = s.radius;
            if [(0.0, 0.0), (r, 0.0), (-r, 0.0), (0.0, r), (0.0, -r)]
                .iter()
                .all(|(ox, oy)| clean(qx + ox, qy + oy))
            {
                offset = [dx.round() as i16, dy.round() as i16];
                break;
            }
        }
        let feather = (s.radius * 0.35).max(1.5);
        for_disc(w, h, s.x, s.y, s.radius + feather, |i, d| {
            let c = smoothstep(s.radius + feather, s.radius - feather, d);
            if c <= 0.0 {
                return;
            }
            cover[i] = cover[i].max(to_u8(c));
            if s.score > owner[i] {
                owner[i] = s.score;
                score[i] = (s.score * BLEMISH_SCALE).round().clamp(0.0, 255.0) as u8;
                donor[i] = offset;
            }
        });
    }
    let mut colour: Vec<[f32; 4]> = (0..n)
        .into_par_iter()
        .map(|i| {
            let weight = interior[i] * (1.0 - taken[i]) + 1e-4;
            let c = src[i];
            [c[0] * weight, c[1] * weight, c[2] * weight, weight]
        })
        .collect();
    blur4(&mut colour, w, h, (small_radius * 1.5).max(2.0));
    let heal_base = colour
        .into_par_iter()
        .map(|a| to_u16([a[0] / a[3], a[1] / a[3], a[2] / a[3]]))
        .collect();
    (score, cover, donor, heal_base)
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
    let mut nostril_area = vec![0.0f32; n];
    stamp_polygon(&mut nostril_area, region, &nostrils, 0.0, 0.03 * e);

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
    let raw_skin: Vec<f32> = (0..n)
        .into_par_iter()
        .map(|i| {
            let (x, y) = pixel_xy(i);
            match parts {
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
            }
        })
        .collect();
    let open_sides = [
        region.x > 0,
        region.y > 0,
        region.x + region.w < width,
        region.y + region.h < height,
    ];
    let refined = refine_skin(raw_skin, &src, w, h, e, open_sides);
    let cheek_luma_src = luma(cheek_colour).max(0.05);
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
            // Only the dark nostril holes leave the skin, not the nose around them.
            let hole =
                nostril_area[i] * (1.0 - smoothstep(0.6, 0.85, luma(src[i]) / cheek_luma_src));
            refined[i] * (1.0 - exclude[i]) * (1.0 - hole)
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
    // length), in units of the face's own skin texture spread. Only inside the
    // face outline (not ears or the jaw edge) and away from the nose wings.
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
    let mut zone = vec![0.0f32; n];
    stamp_polygon(
        &mut zone,
        region,
        &loop_points(points, &FACE_OVAL),
        -0.04 * e,
        0.02 * e,
    );
    let mut wings = vec![0.0f32; n];
    for &k in &NOSE_WINGS {
        let p = points[k as usize];
        stamp_disc(&mut wings, region, [p[0], p[1]], 0.06 * e, 0.02 * e);
    }
    let radii = [(e / 110.0).max(2.0), (e / 55.0).max(3.0)];
    let directions: Vec<[f32; 2]> = (0..8)
        .map(|k| {
            let a = k as f32 * std::f32::consts::FRAC_PI_4;
            [a.cos(), a.sin()]
        })
        .collect();
    let ring: Vec<(f32, usize)> = (0..n)
        .into_par_iter()
        .map(|i| {
            if interior[i] < 0.8 || zone[i] * (1.0 - wings[i]) < 0.5 {
                return (0.0, 0);
            }
            let (x, y) = ((i % w) as f32, (i / w) as f32);
            let centre = tone[i];
            let (mut best, mut which) = (0.0f32, 0usize);
            for (k, radius) in radii.iter().enumerate() {
                let (mut dark, mut red, mut brightest) = (f32::MAX, f32::MAX, f32::MIN);
                for d in &directions {
                    let (sx, sy) = (x + d[0] * radius, y + d[1] * radius);
                    if sx < 0.0 || sy < 0.0 || sx >= w as f32 || sy >= h as f32 {
                        return (0.0, 0);
                    }
                    let ring = tone[sy as usize * w + sx as usize];
                    dark = dark.min(ring[0] - centre[0]);
                    red = red.min(centre[1] - ring[1]);
                    brightest = brightest.max(ring[0] - centre[0]);
                }
                // A spot sits in even skin; one beside a crease or shadow edge
                // sees a lopsided ring, and healing it would lift the crease.
                let depth = dark.max(0.0);
                let even = depth / (depth + (brightest - dark) + 1e-6);
                let score = (depth + 0.5 * red.max(0.0)) / sigma * smoothstep(0.3, 0.55, even);
                if score > best {
                    best = score;
                    which = k;
                }
            }
            (best, which)
        })
        .collect();
    let spots = find_spots(&ring, &radii, w, h, e);
    let own = owners[index].0;
    let (spot_score, spot_cover, donor, heal_base) = heal_spots(
        &spots,
        &src,
        &interior,
        w,
        h,
        [own[0] - region.x as f32, own[1] - region.y as f32],
        radii[0],
    );

    // Lips, brows and the sharpening zone.
    let mut lip_shape = vec![0.0f32; n];
    let mut mouth_hole = vec![0.0f32; n];
    if !trusted {
        stamp_polygon(
            &mut lip_shape,
            region,
            &loop_points(points, &LIPS_OUTER),
            0.0,
            0.012 * e,
        );
        stamp_polygon(
            &mut mouth_hole,
            region,
            &loop_points(points, &MOUTH_INNER),
            0.0,
            0.01 * e,
        );
    }
    let lips: Vec<u8> = (0..n)
        .into_par_iter()
        .map(|i| {
            let (x, y) = pixel_xy(i);
            match parts {
                Some(p) if trusted => to_u8(p.groups_at(x, y)[body_parts::GROUP_LIPS]),
                _ => to_u8(lip_shape[i] * (1.0 - mouth_hole[i])),
            }
        })
        .collect();
    let mut brow_shape = vec![0.0f32; n];
    let mut detail_shape = vec![0.0f32; n];
    for brow in [&RIGHT_BROW[..], &LEFT_BROW[..]] {
        let outline = loop_points(points, brow);
        stamp_polygon(&mut brow_shape, region, &outline, 0.012 * e, 0.015 * e);
        stamp_polygon(&mut detail_shape, region, &outline, 0.02 * e, 0.02 * e);
    }
    for eye in [&RIGHT_EYE[..], &LEFT_EYE[..]] {
        stamp_polygon(
            &mut detail_shape,
            region,
            &loop_points(points, eye),
            0.035 * e,
            0.02 * e,
        );
    }
    stamp_polygon(
        &mut detail_shape,
        region,
        &loop_points(points, &LIPS_OUTER),
        0.01 * e,
        0.015 * e,
    );
    // Brow hairs are the pixels darker than the skin around them.
    let brows: Vec<u8> = (0..n)
        .into_par_iter()
        .map(|i| {
            let darker = luma(low2[i]) - luma(src[i]);
            to_u8(brow_shape[i] * smoothstep(0.015, 0.08, darker))
        })
        .collect();
    let detail: Vec<u8> = detail_shape.into_par_iter().map(to_u8).collect();
    let mut soft4: Vec<[f32; 4]> = src.iter().map(|c| [c[0], c[1], c[2], 0.0]).collect();
    blur4(&mut soft4, w, h, (e / 350.0).max(1.0));
    let soft: Vec<[u16; 3]> = soft4
        .into_par_iter()
        .map(|c| to_u16([c[0], c[1], c[2]]))
        .collect();

    // Nose contour: light down the bridge, shade along both sides of it.
    let across = [cos, sin];
    let ribbon = |line: &[u16], shift: f32, half: f32| -> Vec<[f32; 2]> {
        let centre: Vec<[f32; 2]> = line
            .iter()
            .map(|&k| {
                let p = points[k as usize];
                [p[0] + across[0] * shift, p[1] + across[1] * shift]
            })
            .collect();
        let mut outline: Vec<[f32; 2]> = centre
            .iter()
            .map(|p| [p[0] - across[0] * half, p[1] - across[1] * half])
            .collect();
        outline.extend(
            centre
                .iter()
                .rev()
                .map(|p| [p[0] + across[0] * half, p[1] + across[1] * half]),
        );
        outline
    };
    let mut ridge = vec![0.0f32; n];
    stamp_polygon(
        &mut ridge,
        region,
        &ribbon(&NOSE_BRIDGE, 0.0, 0.022 * e),
        0.0,
        0.03 * e,
    );
    let mut flanks = vec![0.0f32; n];
    for side in [-1.0f32, 1.0] {
        stamp_polygon(
            &mut flanks,
            region,
            &ribbon(&NOSE_BRIDGE[..4], side * 0.075 * e, 0.025 * e),
            0.0,
            0.04 * e,
        );
    }
    let nose: Vec<i8> = (0..n)
        .into_par_iter()
        .map(|i| ((ridge[i] - flanks[i]).clamp(-1.0, 1.0) * 127.0).round() as i8)
        .collect();

    // Hair: the part model's hair over its whole head-and-shoulders crop.
    let (hair_region, hair) = match parts {
        Some(p) if trusted => {
            let [x0, y0, x1, y1] = p.bounds();
            let hr = Region::around([[x0, y0], [x1, y1]].into_iter(), [0.0; 4], width, height);
            let (hw, hh) = (hr.w as usize, hr.h as usize);
            let at = |i: usize| {
                (
                    hr.x as f32 + (i % hw) as f32 + 0.5,
                    hr.y as f32 + (i / hw) as f32 + 0.5,
                )
            };
            let colours: Vec<[f32; 3]> = (0..hr.len())
                .into_par_iter()
                .map(|i| {
                    let (x, y) = at(i);
                    let o = (y as usize * width as usize + x as usize) * 4;
                    [
                        rgba[o] as f32 / 255.0,
                        rgba[o + 1] as f32 / 255.0,
                        rgba[o + 2] as f32 / 255.0,
                    ]
                })
                .collect();
            let guide: Vec<f32> = colours.par_iter().map(|&c| luma(c)).collect();
            let own_centre = owners[index];
            let raw: Vec<f32> = (0..hr.len())
                .into_par_iter()
                .map(|i| {
                    let (x, y) = at(i);
                    let own = (x - own_centre.0[0]).hypot(y - own_centre.0[1]) / own_centre.1;
                    if owners
                        .iter()
                        .enumerate()
                        .any(|(j, (c, s))| j != index && (x - c[0]).hypot(y - c[1]) / s < own)
                    {
                        return 0.0;
                    }
                    // Keep only confident hair, and never where the model sees
                    // skin: faint hair odds spread over blond or grey-haired
                    // foreheads and would dye the face.
                    let g = p.groups_at(x, y);
                    let skin = g[body_parts::GROUP_FACE_SKIN] + g[body_parts::GROUP_BODY_SKIN];
                    smoothstep(0.35, 0.65, g[body_parts::GROUP_HAIR])
                        * (1.0 - smoothstep(0.2, 0.5, skin))
                })
                .collect();
            let snapped = guided(&raw, &guide, hw, hh, (e / 200.0).max(2.0), 0.001);
            // Strand edges blend into the background; only hair-coloured
            // pixels there may be dyed, or a blue backdrop turns pink.
            let snapped = gate_by_colour(&snapped, &colours, hw, hh, (e / 40.0).max(3.0));
            let open = [
                hr.x > 0,
                hr.y > 0,
                hr.x + hr.w < width,
                hr.y + hr.h < height,
            ];
            let fade = (0.05 * e).max(4.0);
            let hair = (0..hr.len())
                .into_par_iter()
                .map(|i| to_u8(snapped[i] * side_fade(i, hw, hh, open, fade)))
                .collect();
            (hr, hair)
        }
        _ => (
            Region {
                x: 0,
                y: 0,
                w: 0,
                h: 0,
            },
            Vec::new(),
        ),
    };

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
        spot_score,
        spot_cover,
        donor,
        heal_base,
        lips,
        brows,
        nose,
        hair_region,
        hair,
        detail,
        soft,
        low1: low1.into_par_iter().map(to_u16).collect(),
        low2: low2.into_par_iter().map(to_u16).collect(),
        broad,
        skin_mean,
        cheek_luma,
    }
}
