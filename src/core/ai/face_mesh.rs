//! MediaPipe Face Mesh V2: 478 facial landmarks per face (trial).
//!
//! Faces are seeded by YuNet. Each face is cropped square with the eye line
//! levelled and a 25% margin per side (the model card's input contract), then
//! run at 256x256. A second pass recrops around the first pass's landmarks, as
//! MediaPipe's own tracking loop does, which tightens the fit.

use std::path::PathBuf;

use rayon::prelude::*;

pub const LANDMARK_COUNT: usize = 478;
const INPUT_SIDE: usize = 256;
const MARGIN_SCALE: f32 = 1.5;
const MIN_PRESENCE: f32 = 0.5;
const MODEL_DIR: &str = "face-mesh";
const MODEL_FILE: &str = "face_landmarks_detector.onnx";
const RIGHT_EYE_OUTER: usize = 33;
const LEFT_EYE_OUTER: usize = 263;

#[derive(Clone, Debug)]
pub struct FaceMesh {
    /// Image-space x, y and relative depth z (same scale as x).
    pub points: Vec<[f32; 3]>,
    pub presence: f32,
}

pub fn model_path() -> Option<PathBuf> {
    super::retouch::model_roots()
        .into_iter()
        .map(|root| root.join(MODEL_DIR).join(MODEL_FILE))
        .find(|path| path.is_file())
}

impl FaceMesh {
    /// Face centre, forehead-to-chin extent and eye-line angle (radians).
    pub fn frame(&self) -> ([f32; 2], f32, f32) {
        let crop = Crop::from_points(&self.points);
        ([crop.cx, crop.cy], crop.side / MARGIN_SCALE, crop.angle)
    }
}

/// A square crop of the image, rotated by `angle` around its centre.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Crop {
    cx: f32,
    cy: f32,
    side: f32,
    angle: f32,
}

impl Crop {
    fn from_seed(seed: &super::retouch::FaceSeed) -> Self {
        let [x, y, width, height] = seed.rect;
        let [left, right] = [seed.keypoints[0], seed.keypoints[1]];
        Self {
            cx: x + width * 0.5,
            cy: y + height * 0.5,
            side: width.max(height) * MARGIN_SCALE,
            angle: (right[1] - left[1]).atan2(right[0] - left[0]),
        }
    }

    /// Tight box around `points` in the frame levelled by the outer eye corners.
    fn from_points(points: &[[f32; 3]]) -> Self {
        let left = points[RIGHT_EYE_OUTER];
        let right = points[LEFT_EYE_OUTER];
        let angle = (right[1] - left[1]).atan2(right[0] - left[0]);
        let (sin, cos) = angle.sin_cos();
        let (mut u0, mut u1, mut v0, mut v1) = (f32::MAX, f32::MIN, f32::MAX, f32::MIN);
        for p in points {
            let u = p[0] * cos + p[1] * sin;
            let v = -p[0] * sin + p[1] * cos;
            u0 = u0.min(u);
            u1 = u1.max(u);
            v0 = v0.min(v);
            v1 = v1.max(v);
        }
        let (u, v) = ((u0 + u1) * 0.5, (v0 + v1) * 0.5);
        Self {
            cx: u * cos - v * sin,
            cy: u * sin + v * cos,
            side: (u1 - u0).max(v1 - v0) * MARGIN_SCALE,
            angle,
        }
    }

    /// Map continuous crop coordinates (0..INPUT_SIDE) to image coordinates.
    fn to_image(&self, u: f32, v: f32) -> [f32; 2] {
        let scale = self.side / INPUT_SIDE as f32;
        let (sin, cos) = self.angle.sin_cos();
        let du = (u - INPUT_SIDE as f32 * 0.5) * scale;
        let dv = (v - INPUT_SIDE as f32 * 0.5) * scale;
        [self.cx + cos * du - sin * dv, self.cy + sin * du + cos * dv]
    }
}

/// Bilinear RGB sample at continuous image coordinates, edges replicated.
pub(super) fn sample_rgb(rgba: &[u8], width: u32, height: u32, x: f32, y: f32) -> [f32; 3] {
    let px = (x - 0.5).clamp(0.0, (width - 1) as f32);
    let py = (y - 0.5).clamp(0.0, (height - 1) as f32);
    let x0 = px.floor() as usize;
    let y0 = py.floor() as usize;
    let x1 = (x0 + 1).min(width as usize - 1);
    let y1 = (y0 + 1).min(height as usize - 1);
    let fx = px - x0 as f32;
    let fy = py - y0 as f32;
    let at = |xx: usize, yy: usize, c: usize| rgba[(yy * width as usize + xx) * 4 + c] as f32;
    let mut out = [0.0; 3];
    for (c, value) in out.iter_mut().enumerate() {
        let top = at(x0, y0, c) * (1.0 - fx) + at(x1, y0, c) * fx;
        let bottom = at(x0, y1, c) * (1.0 - fx) + at(x1, y1, c) * fx;
        *value = top * (1.0 - fy) + bottom * fy;
    }
    out
}

/// NHWC float input in 0..1. Large crops are supersampled so the 256px input
/// is area-averaged rather than aliased.
fn sample_crop(rgba: &[u8], width: u32, height: u32, crop: &Crop) -> Vec<f32> {
    let taps = ((crop.side / INPUT_SIDE as f32).ceil() as usize).clamp(1, 6);
    let norm = 1.0 / (255.0 * (taps * taps) as f32);
    let mut input = vec![0.0f32; INPUT_SIDE * INPUT_SIDE * 3];
    input
        .par_chunks_mut(INPUT_SIDE * 3)
        .enumerate()
        .for_each(|(row, line)| {
            for col in 0..INPUT_SIDE {
                let mut sum = [0.0f32; 3];
                for sy in 0..taps {
                    for sx in 0..taps {
                        let u = col as f32 + (sx as f32 + 0.5) / taps as f32;
                        let v = row as f32 + (sy as f32 + 0.5) / taps as f32;
                        let [x, y] = crop.to_image(u, v);
                        let rgb = sample_rgb(rgba, width, height, x, y);
                        for c in 0..3 {
                            sum[c] += rgb[c];
                        }
                    }
                }
                for c in 0..3 {
                    line[col * 3 + c] = sum[c] * norm;
                }
            }
        });
    input
}

/// Run one crop; returns crop-space landmarks and the face presence score.
fn run_crop(
    session: &mut ort::session::Session,
    input: Vec<f32>,
) -> Result<(Vec<[f32; 3]>, f32), String> {
    let side = INPUT_SIDE as i64;
    let tensor = ort::value::Tensor::<f32>::from_array(([1i64, side, side, 3], input))
        .map_err(|e| format!("Face Mesh input tensor: {e}"))?;
    let input_name = session
        .inputs()
        .first()
        .map(|input| input.name().to_string())
        .unwrap_or_else(|| "input_12".to_string());
    let outputs = session
        .run(ort::inputs![input_name.as_str() => tensor])
        .map_err(|e| format!("Face Mesh inference: {e}"))?;
    let (_, landmarks) = outputs["Identity"]
        .try_extract_tensor::<f32>()
        .map_err(|e| format!("Face Mesh landmarks output: {e}"))?;
    let (_, flag) = outputs["Identity_1"]
        .try_extract_tensor::<f32>()
        .map_err(|e| format!("Face Mesh presence output: {e}"))?;
    if landmarks.len() < LANDMARK_COUNT * 3 || flag.is_empty() {
        return Err("Face Mesh output contract mismatch".to_string());
    }
    let points = landmarks
        .chunks_exact(3)
        .take(LANDMARK_COUNT)
        .map(|p| [p[0], p[1], p[2]])
        .collect();
    Ok((points, 1.0 / (1.0 + (-flag[0]).exp())))
}

/// YuNet seeds from the upright image and from quarter turns either way, so
/// sideways faces are found too. Detection runs on a copy no larger than
/// YuNet's own working size; seeds come back in full-image coordinates.
fn detect_seeds_all_orientations(
    rgba: &[u8],
    width: u32,
    height: u32,
) -> Result<Vec<super::retouch::FaceSeed>, String> {
    const DETECT_LONG_EDGE: u32 = 640;
    let source = image::ImageBuffer::<image::Rgba<u8>, &[u8]>::from_raw(width, height, rgba)
        .ok_or_else(|| "Face Mesh: ảnh không hợp lệ".to_string())?;
    let scale = (DETECT_LONG_EDGE as f32 / width.max(height) as f32).min(1.0);
    let small = if scale < 1.0 {
        image::imageops::resize(
            &source,
            ((width as f32 * scale).round() as u32).max(1),
            ((height as f32 * scale).round() as u32).max(1),
            image::imageops::FilterType::Triangle,
        )
    } else {
        image::RgbaImage::from_raw(width, height, rgba.to_vec())
            .ok_or_else(|| "Face Mesh: ảnh không hợp lệ".to_string())?
    };
    let (sw, sh) = (small.width() as f32, small.height() as f32);
    let clockwise = image::imageops::rotate90(&small);
    let counter = image::imageops::rotate270(&small);
    // Each view maps its own coordinates back to the small upright image.
    let views: [(&image::RgbaImage, &dyn Fn([f32; 2]) -> [f32; 2]); 3] = [
        (&small, &|p: [f32; 2]| p),
        (&clockwise, &|p: [f32; 2]| [p[1], sh - p[0]]),
        (&counter, &|p: [f32; 2]| [sw - p[1], p[0]]),
    ];
    let mut seeds = Vec::new();
    for (view, to_upright) in views {
        for seed in super::retouch::detect_face_seeds(view.as_raw(), view.width(), view.height())? {
            let [x, y, w, h] = seed.rect;
            let a = to_upright([x, y]);
            let b = to_upright([x + w, y + h]);
            let full = |p: [f32; 2]| [p[0] / scale, p[1] / scale];
            let [x0, y0] = full([a[0].min(b[0]), a[1].min(b[1])]);
            let [x1, y1] = full([a[0].max(b[0]), a[1].max(b[1])]);
            seeds.push(super::retouch::FaceSeed {
                rect: [x0, y0, x1 - x0, y1 - y0],
                keypoints: seed.keypoints.map(|k| full(to_upright(k))),
                score: seed.score,
            });
        }
    }
    Ok(seeds)
}

/// Drop fits of the same face from different seeds, keeping the most confident.
fn dedupe(mut meshes: Vec<FaceMesh>) -> Vec<FaceMesh> {
    meshes.sort_by(|a, b| b.presence.total_cmp(&a.presence));
    let mut kept: Vec<(FaceMesh, Crop)> = Vec::new();
    for mesh in meshes {
        let crop = Crop::from_points(&mesh.points);
        let duplicate = kept.iter().any(|(_, other)| {
            let reach = crop.side.min(other.side) / MARGIN_SCALE * 0.5;
            (crop.cx - other.cx).hypot(crop.cy - other.cy) < reach
        });
        if !duplicate {
            kept.push((mesh, crop));
        }
    }
    kept.into_iter().map(|(mesh, _)| mesh).collect()
}

/// Find every face YuNet sees and fit 478 landmarks to each.
pub fn detect(rgba: &[u8], width: u32, height: u32) -> Result<Vec<FaceMesh>, String> {
    if width == 0 || height == 0 || rgba.len() != width as usize * height as usize * 4 {
        return Err("Face Mesh: ảnh không hợp lệ".to_string());
    }
    let path = model_path()
        .ok_or_else(|| format!("thiếu model mốc mặt (models\\{MODEL_DIR}\\{MODEL_FILE})"))?;
    let seeds = detect_seeds_all_orientations(rgba, width, height)?;
    let mut session = ort::session::Session::builder()
        .map_err(|e| format!("Face Mesh ORT builder: {e}"))?
        .commit_from_file(&path)
        .map_err(|e| format!("Face Mesh load model: {e}"))?;
    let mut meshes = Vec::new();
    for seed in &seeds {
        let mut crop = Crop::from_seed(seed);
        let mut fitted = None;
        for _pass in 0..2 {
            let input = sample_crop(rgba, width, height, &crop);
            let (crop_points, presence) = run_crop(&mut session, input)?;
            if presence < MIN_PRESENCE {
                break;
            }
            let depth_scale = crop.side / INPUT_SIDE as f32;
            let points: Vec<[f32; 3]> = crop_points
                .iter()
                .map(|p| {
                    let [x, y] = crop.to_image(p[0], p[1]);
                    [x, y, p[2] * depth_scale]
                })
                .collect();
            crop = Crop::from_points(&points);
            fitted = Some(FaceMesh { points, presence });
        }
        meshes.extend(fitted);
    }
    Ok(dedupe(meshes))
}

/// Straight-alpha "over" of one colour with coverage into an RGBA pixel.
pub(super) fn blend_over(pixel: &mut [u8], color: [u8; 4], coverage: f32) {
    let a = color[3] as f32 / 255.0 * coverage;
    if a <= 0.0 {
        return;
    }
    let dst_a = pixel[3] as f32 / 255.0;
    let out_a = a + dst_a * (1.0 - a);
    for c in 0..3 {
        let value = (color[c] as f32 * a + pixel[c] as f32 * dst_a * (1.0 - a)) / out_a;
        pixel[c] = value.round().clamp(0.0, 255.0) as u8;
    }
    pixel[3] = (out_a * 255.0).round() as u8;
}

/// Anti-aliased capsule from `a` to `b` (a disc when they coincide).
fn draw_segment(
    buffer: &mut [u8],
    width: u32,
    height: u32,
    a: [f32; 2],
    b: [f32; 2],
    radius: f32,
    color: [u8; 4],
) {
    let pad = radius + 1.0;
    let x0 = (a[0].min(b[0]) - pad).floor().max(0.0) as i64;
    let y0 = (a[1].min(b[1]) - pad).floor().max(0.0) as i64;
    let x1 = ((a[0].max(b[0]) + pad).ceil() as i64).min(width as i64 - 1);
    let y1 = ((a[1].max(b[1]) + pad).ceil() as i64).min(height as i64 - 1);
    let (dx, dy) = (b[0] - a[0], b[1] - a[1]);
    let length_sq = dx * dx + dy * dy;
    for y in y0..=y1 {
        for x in x0..=x1 {
            let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
            let t = if length_sq > 0.0 {
                (((px - a[0]) * dx + (py - a[1]) * dy) / length_sq).clamp(0.0, 1.0)
            } else {
                0.0
            };
            let (ex, ey) = (px - (a[0] + t * dx), py - (a[1] + t * dy));
            let coverage = (radius + 0.5 - (ex * ex + ey * ey).sqrt()).clamp(0.0, 1.0);
            if coverage > 0.0 {
                let index = (y as usize * width as usize + x as usize) * 4;
                blend_over(&mut buffer[index..index + 4], color, coverage);
            }
        }
    }
}

/// Transparent canvas-size layer with every landmark dotted and the main
/// features (face outline, brows, eyes, irises, nose, lips) traced in colour.
pub fn render_overlay(width: u32, height: u32, meshes: &[FaceMesh]) -> Vec<u8> {
    let mut buffer = vec![0u8; width as usize * height as usize * 4];
    let features: [(&[(u16, u16)], [u8; 4]); 9] = [
        (FACE_OVAL, [0, 220, 255, 255]),
        (LEFT_EYEBROW, [255, 210, 0, 255]),
        (RIGHT_EYEBROW, [255, 210, 0, 255]),
        (LEFT_EYE, [60, 255, 90, 255]),
        (RIGHT_EYE, [60, 255, 90, 255]),
        (LEFT_IRIS, [255, 120, 0, 255]),
        (RIGHT_IRIS, [255, 120, 0, 255]),
        (NOSE, [150, 170, 255, 255]),
        (LIPS, [255, 60, 150, 255]),
    ];
    for mesh in meshes {
        let crop = Crop::from_points(&mesh.points);
        let face_size = crop.side / MARGIN_SCALE;
        let line = (face_size / 400.0).clamp(0.6, 6.0);
        let dot = (face_size / 300.0).clamp(0.7, 6.0);
        for p in &mesh.points {
            draw_segment(
                &mut buffer,
                width,
                height,
                [p[0], p[1]],
                [p[0], p[1]],
                dot,
                [255, 255, 255, 200],
            );
        }
        for (edges, color) in features {
            for &(from, to) in edges {
                let a = mesh.points[from as usize];
                let b = mesh.points[to as usize];
                draw_segment(
                    &mut buffer,
                    width,
                    height,
                    [a[0], a[1]],
                    [b[0], b[1]],
                    line,
                    color,
                );
            }
        }
    }
    buffer
}

// Feature outlines from MediaPipe's FaceLandmarksConnections (Apache-2.0).
#[rustfmt::skip]
const FACE_OVAL: &[(u16, u16)] = &[
    (10, 338), (338, 297), (297, 332), (332, 284), (284, 251), (251, 389), (389, 356), (356, 454),
    (454, 323), (323, 361), (361, 288), (288, 397), (397, 365), (365, 379), (379, 378),
    (378, 400), (400, 377), (377, 152), (152, 148), (148, 176), (176, 149), (149, 150),
    (150, 136), (136, 172), (172, 58), (58, 132), (132, 93), (93, 234), (234, 127), (127, 162),
    (162, 21), (21, 54), (54, 103), (103, 67), (67, 109), (109, 10),
];
#[rustfmt::skip]
const LIPS: &[(u16, u16)] = &[
    (61, 146), (146, 91), (91, 181), (181, 84), (84, 17), (17, 314), (314, 405), (405, 321),
    (321, 375), (375, 291), (61, 185), (185, 40), (40, 39), (39, 37), (37, 0), (0, 267),
    (267, 269), (269, 270), (270, 409), (409, 291), (78, 95), (95, 88), (88, 178), (178, 87),
    (87, 14), (14, 317), (317, 402), (402, 318), (318, 324), (324, 308), (78, 191), (191, 80),
    (80, 81), (81, 82), (82, 13), (13, 312), (312, 311), (311, 310), (310, 415), (415, 308),
];
#[rustfmt::skip]
const LEFT_EYE: &[(u16, u16)] = &[
    (263, 249), (249, 390), (390, 373), (373, 374), (374, 380), (380, 381), (381, 382),
    (382, 362), (263, 466), (466, 388), (388, 387), (387, 386), (386, 385), (385, 384),
    (384, 398), (398, 362),
];
#[rustfmt::skip]
const RIGHT_EYE: &[(u16, u16)] = &[
    (33, 7), (7, 163), (163, 144), (144, 145), (145, 153), (153, 154), (154, 155), (155, 133),
    (33, 246), (246, 161), (161, 160), (160, 159), (159, 158), (158, 157), (157, 173), (173, 133),
];
#[rustfmt::skip]
const LEFT_EYEBROW: &[(u16, u16)] = &[
    (276, 283), (283, 282), (282, 295), (295, 285), (300, 293), (293, 334), (334, 296),
    (296, 336),
];
#[rustfmt::skip]
const RIGHT_EYEBROW: &[(u16, u16)] = &[
    (46, 53), (53, 52), (52, 65), (65, 55), (70, 63), (63, 105), (105, 66), (66, 107),
];
#[rustfmt::skip]
const LEFT_IRIS: &[(u16, u16)] = &[
    (474, 475), (475, 476), (476, 477), (477, 474),
];
#[rustfmt::skip]
const RIGHT_IRIS: &[(u16, u16)] = &[
    (469, 470), (470, 471), (471, 472), (472, 469),
];
#[rustfmt::skip]
const NOSE: &[(u16, u16)] = &[
    (168, 6), (6, 197), (197, 195), (195, 5), (5, 4), (4, 1), (1, 19), (19, 94), (94, 2),
    (98, 97), (97, 2), (2, 326), (326, 327), (327, 294), (294, 278), (278, 344), (344, 440),
    (440, 275), (275, 4), (4, 45), (45, 220), (220, 115), (115, 48), (48, 64), (64, 98),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crop_from_points_recovers_a_tilted_face_frame() {
        let frame = Crop {
            cx: 100.0,
            cy: 50.0,
            side: 256.0,
            angle: 0.5,
        };
        let centre = frame.to_image(128.0, 128.0);
        assert!((centre[0] - 100.0).abs() < 1e-3 && (centre[1] - 50.0).abs() < 1e-3);
        let mut points: Vec<[f32; 3]> = (0..LANDMARK_COUNT)
            .map(|i| {
                let u = 60.0 + (i % 18) as f32 * 8.0;
                let v = 60.0 + (i / 18 % 18) as f32 * 8.0;
                let [x, y] = frame.to_image(u, v);
                [x, y, 0.0]
            })
            .collect();
        for (index, u) in [(RIGHT_EYE_OUTER, 80.0), (LEFT_EYE_OUTER, 176.0)] {
            let [x, y] = frame.to_image(u, 100.0);
            points[index] = [x, y, 0.0];
        }
        let fitted = Crop::from_points(&points);
        assert!((fitted.angle - 0.5).abs() < 1e-3);
        assert!((fitted.cx - 100.0).abs() < 1e-2 && (fitted.cy - 50.0).abs() < 1e-2);
        assert!((fitted.side - 136.0 * MARGIN_SCALE).abs() < 1e-2);
    }

    /// Opt-in accuracy probe against the official MediaPipe Python pipeline:
    /// set IAI_FACE_MESH_PROBE to a folder holding `ref.json` (image name ->
    /// faces -> 478 [x, y]) and the images; overlays are written beside them.
    #[test]
    #[ignore]
    fn probe_against_mediapipe_reference() {
        let Ok(dir) = std::env::var("IAI_FACE_MESH_PROBE") else {
            return;
        };
        let dir = PathBuf::from(dir);
        let reference: std::collections::BTreeMap<String, Vec<Vec<[f32; 2]>>> =
            serde_json::from_str(&std::fs::read_to_string(dir.join("ref.json")).unwrap()).unwrap();
        for (name, ref_faces) in &reference {
            let image = image::open(dir.join(name)).unwrap().to_rgba8();
            let (width, height) = image.dimensions();
            let started = std::time::Instant::now();
            let meshes = detect(image.as_raw(), width, height).unwrap();
            let elapsed = started.elapsed().as_millis();
            let mut composed = image.clone().into_raw();
            let overlay = render_overlay(width, height, &meshes);
            for (pixel, over) in composed.chunks_exact_mut(4).zip(overlay.chunks_exact(4)) {
                blend_over(
                    pixel,
                    [over[0], over[1], over[2], 255],
                    over[3] as f32 / 255.0,
                );
            }
            image::RgbaImage::from_raw(width, height, composed)
                .unwrap()
                .save(dir.join(format!("out_{name}.png")))
                .unwrap();
            let presences: Vec<String> = meshes
                .iter()
                .map(|m| format!("{:.2}", m.presence))
                .collect();
            let mut line = format!(
                "{name}: iAi {} face(s) [{}], MediaPipe {} face(s), {elapsed} ms",
                meshes.len(),
                presences.join(" "),
                ref_faces.len()
            );
            for ref_face in ref_faces {
                let ref_centre = ref_face.iter().fold([0.0f32; 2], |acc, p| {
                    [acc[0] + p[0] / 478.0, acc[1] + p[1] / 478.0]
                });
                let closest = meshes.iter().min_by(|a, b| {
                    let dist = |m: &FaceMesh| {
                        let p = m.points[1];
                        (p[0] - ref_centre[0]).powi(2) + (p[1] - ref_centre[1]).powi(2)
                    };
                    dist(a).total_cmp(&dist(b))
                });
                let Some(mesh) = closest else {
                    line.push_str(" | ref face unmatched");
                    continue;
                };
                let a = ref_face[RIGHT_EYE_OUTER];
                let b = ref_face[LEFT_EYE_OUTER];
                let interocular = ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2)).sqrt();
                let mean = mesh
                    .points
                    .iter()
                    .zip(ref_face)
                    .map(|(p, r)| ((p[0] - r[0]).powi(2) + (p[1] - r[1]).powi(2)).sqrt())
                    .sum::<f32>()
                    / LANDMARK_COUNT as f32;
                line.push_str(&format!(
                    " | NME {:.2}% (eye gap {interocular:.0}px, presence {:.2})",
                    mean / interocular * 100.0,
                    mesh.presence
                ));
            }
            println!("{line}");
        }
    }
}
