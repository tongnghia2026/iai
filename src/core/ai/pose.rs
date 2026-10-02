//! MediaPipe Pose (BlazePose GHUM, heavy): 33 body landmarks per person.
//!
//! There is no person detector: the first look is a square around the body
//! as it would hang below a face, and, as MediaPipe's own tracking loop does,
//! each later look is recropped from the model's two alignment points (the
//! hip centre and a point setting the body's size and lean). Run at 256x256.

use std::path::PathBuf;

use rayon::prelude::*;

use super::face_mesh::FaceMesh;

pub const LANDMARK_COUNT: usize = 33;
/// Body landmarks, then the two alignment points and four unused ones.
const OUTPUT_POINTS: usize = 39;
const ALIGN_CENTRE: usize = 33;
const ALIGN_SCALE: usize = 34;
const INPUT_SIDE: usize = 256;
const HEATMAP_SIDE: usize = 64;
const HEATMAP_KERNEL: usize = 7;
const MIN_REFINE_CONFIDENCE: f32 = 0.5;
const ROI_SCALE: f32 = 1.25;
const MIN_PRESENCE: f32 = 0.5;
/// Recrops after the first look.
const PASSES: usize = 2;
const MODEL_DIR: &str = "pose";
const MODEL_FILE: &str = "pose_landmarks_detector.onnx";

/// Landmark indices (the person's own left and right).
pub const NOSE: usize = 0;
pub const LEFT_SHOULDER: usize = 11;
pub const RIGHT_SHOULDER: usize = 12;
pub const LEFT_ELBOW: usize = 13;
pub const RIGHT_ELBOW: usize = 14;
pub const LEFT_WRIST: usize = 15;
pub const RIGHT_WRIST: usize = 16;
pub const LEFT_HIP: usize = 23;
pub const RIGHT_HIP: usize = 24;
pub const LEFT_KNEE: usize = 25;
pub const RIGHT_KNEE: usize = 26;
pub const LEFT_ANKLE: usize = 27;
pub const RIGHT_ANKLE: usize = 28;

pub fn model_path() -> Option<PathBuf> {
    super::retouch::model_roots()
        .into_iter()
        .map(|root| root.join(MODEL_DIR).join(MODEL_FILE))
        .find(|path| path.is_file())
}

/// One person's landmarks: image x, y, and how likely each point is visible
/// and inside the image (0..1).
#[derive(Clone, Debug)]
pub struct Pose {
    pub points: Vec<[f32; 4]>,
    pub presence: f32,
}

impl Pose {
    pub fn at(&self, k: usize) -> [f32; 2] {
        [self.points[k][0], self.points[k][1]]
    }

    /// Whether landmark `k` is likely seen in the image.
    pub fn seen(&self, k: usize) -> bool {
        self.points[k][2] >= 0.5 && self.points[k][3] >= 0.5
    }
}

/// A square of the image, turned by `angle`: the crop's up runs along
/// (sin angle, -cos angle) in the image.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Roi {
    cx: f32,
    cy: f32,
    side: f32,
    angle: f32,
}

impl Roi {
    /// The square MediaPipe takes from its alignment points: centred on the
    /// hip centre, twice the distance to the scale point (times
    /// [`ROI_SCALE`]) wide, turned so that point is straight up.
    fn from_alignment(centre: [f32; 2], scale: [f32; 2]) -> Self {
        let (dx, dy) = (scale[0] - centre[0], scale[1] - centre[1]);
        Self {
            cx: centre[0],
            cy: centre[1],
            side: 2.0 * dx.hypot(dy) * ROI_SCALE,
            angle: std::f32::consts::FRAC_PI_2 - (-dy).atan2(dx),
        }
    }

    /// The first guess: the body hanging below `mesh`'s face, upright with
    /// the face.
    fn below_face(mesh: &FaceMesh) -> Self {
        let ([fx, fy], e, angle) = mesh.frame();
        let (sin, cos) = angle.sin_cos();
        // Hip centre about 3.6 face extents below the face centre; the scale
        // point a little above the head.
        let (hip, reach) = (3.6 * e, 4.8 * e);
        let centre = [fx - sin * hip, fy + cos * hip];
        Self {
            cx: centre[0],
            cy: centre[1],
            side: 2.0 * reach * ROI_SCALE,
            angle,
        }
    }

    fn to_image(self, u: f32, v: f32) -> [f32; 2] {
        let (sin, cos) = self.angle.sin_cos();
        let k = self.side / INPUT_SIDE as f32;
        let du = (u - INPUT_SIDE as f32 * 0.5) * k;
        let dv = (v - INPUT_SIDE as f32 * 0.5) * k;
        [self.cx + cos * du - sin * dv, self.cy + sin * du + cos * dv]
    }
}

fn sigmoid(v: f32) -> f32 {
    1.0 / (1.0 + (-v).exp())
}

/// Bilinear RGB (0..1) at image point (x, y); black outside the image.
fn sample(rgba: &[u8], width: u32, height: u32, x: f32, y: f32) -> [f32; 3] {
    let (w, h) = (width as f32, height as f32);
    if x < 0.0 || y < 0.0 || x >= w || y >= h {
        return [0.0; 3];
    }
    let (px, py) = ((x - 0.5).clamp(0.0, w - 1.0), (y - 0.5).clamp(0.0, h - 1.0));
    let (x0, y0) = (px as usize, py as usize);
    let (x1, y1) = (
        (x0 + 1).min(width as usize - 1),
        (y0 + 1).min(height as usize - 1),
    );
    let (fx, fy) = (px - x0 as f32, py - y0 as f32);
    let at = |xx: usize, yy: usize, c: usize| rgba[(yy * width as usize + xx) * 4 + c] as f32;
    std::array::from_fn(|c| {
        let top = at(x0, y0, c) * (1.0 - fx) + at(x1, y0, c) * fx;
        let bottom = at(x0, y1, c) * (1.0 - fx) + at(x1, y1, c) * fx;
        (top * (1.0 - fy) + bottom * fy) / 255.0
    })
}

/// NHWC input in 0..1, area-averaged when the square is larger than the
/// model's input.
fn sample_roi(rgba: &[u8], width: u32, height: u32, roi: Roi) -> Vec<f32> {
    let taps = ((roi.side / INPUT_SIDE as f32).ceil() as usize).clamp(1, 6);
    let norm = 1.0 / (taps * taps) as f32;
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
                        let [x, y] = roi.to_image(u, v);
                        let rgb = sample(rgba, width, height, x, y);
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

/// Moves each landmark to the confidence-weighted centre of the heatmap
/// around it, as MediaPipe's landmark refinement does.
fn refine(points: &mut [[f32; 5]], heatmap: &[f32]) {
    let half = (HEATMAP_KERNEL - 1) / 2;
    let scale = HEATMAP_SIDE as f32 / INPUT_SIDE as f32;
    for (k, p) in points.iter_mut().enumerate() {
        let (col, row) = (
            (p[0] * scale).floor() as isize,
            (p[1] * scale).floor() as isize,
        );
        // Points off the heatmap stay as they are.
        if col < 0 || row < 0 || col >= HEATMAP_SIDE as isize || row >= HEATMAP_SIDE as isize {
            continue;
        }
        let (mut sum, mut wc, mut wr, mut best) = (0.0f32, 0.0f32, 0.0f32, 0.0f32);
        for r in (row - half as isize)..=(row + half as isize) {
            for c in (col - half as isize)..=(col + half as isize) {
                if r < 0 || c < 0 || r >= HEATMAP_SIDE as isize || c >= HEATMAP_SIDE as isize {
                    continue;
                }
                let i = (r as usize * HEATMAP_SIDE + c as usize) * OUTPUT_POINTS + k;
                let confidence = sigmoid(heatmap[i]);
                sum += confidence;
                wc += c as f32 * confidence;
                wr += r as f32 * confidence;
                best = best.max(confidence);
            }
        }
        if best >= MIN_REFINE_CONFIDENCE && sum > 0.0 {
            p[0] = (wc / sum) / scale;
            p[1] = (wr / sum) / scale;
        }
    }
}

pub struct PoseModel {
    session: ort::session::Session,
    pub on_gpu: bool,
}

impl PoseModel {
    pub fn load(prefer_gpu: bool) -> Result<Self, String> {
        let path = model_path().ok_or_else(|| {
            format!("thiếu model khung xương (models\\{MODEL_DIR}\\{MODEL_FILE})")
        })?;
        let (session, on_gpu) = super::ort_ep::build_session(&path, prefer_gpu)?;
        Ok(Self { session, on_gpu })
    }

    /// Landmarks (image x, y, raw visibility and presence) of one square,
    /// and the pose presence score.
    fn run(
        &mut self,
        rgba: &[u8],
        width: u32,
        height: u32,
        roi: Roi,
    ) -> Result<(Vec<[f32; 5]>, f32), String> {
        let input = sample_roi(rgba, width, height, roi);
        let side = INPUT_SIDE as i64;
        let tensor = ort::value::Tensor::<f32>::from_array(([1i64, side, side, 3], input))
            .map_err(|e| format!("Pose input tensor: {e}"))?;
        let outputs = self
            .session
            .run(ort::inputs!["input_1" => tensor])
            .map_err(|e| format!("Pose inference: {e}"))?;
        let (_, landmarks) = outputs["Identity"]
            .try_extract_tensor::<f32>()
            .map_err(|e| format!("Pose landmarks output: {e}"))?;
        let (_, flag) = outputs["Identity_1"]
            .try_extract_tensor::<f32>()
            .map_err(|e| format!("Pose presence output: {e}"))?;
        let (_, heatmap) = outputs["Identity_3"]
            .try_extract_tensor::<f32>()
            .map_err(|e| format!("Pose heatmap output: {e}"))?;
        if landmarks.len() < OUTPUT_POINTS * 5
            || flag.is_empty()
            || heatmap.len() < HEATMAP_SIDE * HEATMAP_SIDE * OUTPUT_POINTS
        {
            return Err("Pose output contract mismatch".to_string());
        }
        let mut points: Vec<[f32; 5]> = landmarks
            .chunks_exact(5)
            .take(OUTPUT_POINTS)
            .map(|p| [p[0], p[1], p[2], p[3], p[4]])
            .collect();
        refine(&mut points, heatmap);
        for p in &mut points {
            let [x, y] = roi.to_image(p[0], p[1]);
            p[0] = x;
            p[1] = y;
        }
        // The presence flag comes out as a probability already.
        Ok((points, flag[0]))
    }

    /// The pose of the person whose face is `mesh`, or `None` when the model
    /// does not see one there.
    pub fn detect(
        &mut self,
        rgba: &[u8],
        width: u32,
        height: u32,
        mesh: &FaceMesh,
    ) -> Result<Option<Pose>, String> {
        let mut roi = Roi::below_face(mesh);
        let mut found = None;
        for _ in 0..=PASSES {
            let (points, presence) = self.run(rgba, width, height, roi)?;
            if presence < MIN_PRESENCE {
                break;
            }
            let at = |k: usize| [points[k][0], points[k][1]];
            roi = Roi::from_alignment(at(ALIGN_CENTRE), at(ALIGN_SCALE));
            found = Some(Pose {
                points: points
                    .iter()
                    .take(LANDMARK_COUNT)
                    .map(|p| [p[0], p[1], sigmoid(p[3]), sigmoid(p[4])])
                    .collect(),
                presence,
            });
        }
        Ok(found)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Opt-in: IAI_POSE_PROBE is a folder of photos; writes `pose_iai.json`
    /// ({name: [[x, y, visibility, presence] * 33]}, the largest face's
    /// person) to compare with MediaPipe's own pipeline, and prints timings.
    #[test]
    #[ignore]
    fn probe_pose() {
        let Ok(dir) = std::env::var("IAI_POSE_PROBE") else {
            return;
        };
        let dir = PathBuf::from(dir);
        let mut names: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().to_string())
            .filter(|n| n.ends_with(".jpg"))
            .collect();
        names.sort();
        let mut model = PoseModel::load(false).unwrap();
        let mut out = serde_json::Map::new();
        for name in names {
            let image = image::open(dir.join(&name)).unwrap().to_rgba8();
            let (width, height) = image.dimensions();
            let rgba = image.into_raw();
            let meshes = super::super::face_mesh::detect(&rgba, width, height).unwrap_or_default();
            let Some(mesh) = meshes
                .iter()
                .max_by(|a, b| a.frame().1.total_cmp(&b.frame().1))
            else {
                println!("{name}: no face");
                continue;
            };
            let started = std::time::Instant::now();
            let pose = model.detect(&rgba, width, height, mesh).unwrap();
            println!(
                "{name}: {} in {} ms",
                pose.as_ref().map_or("no pose".to_string(), |p| format!(
                    "presence {:.2}",
                    p.presence
                )),
                started.elapsed().as_millis()
            );
            let poses: Vec<Vec<[f32; 4]>> = pose.into_iter().map(|p| p.points).collect();
            out.insert(name, serde_json::to_value(poses).unwrap());
        }
        std::fs::write(
            dir.join("pose_iai.json"),
            serde_json::to_string(&out).unwrap(),
        )
        .unwrap();
    }

    #[test]
    fn alignment_square_is_centred_and_turned_toward_the_scale_point() {
        // Upright: the scale point straight above the hip centre.
        let up = Roi::from_alignment([100.0, 200.0], [100.0, 120.0]);
        assert_eq!((up.cx, up.cy), (100.0, 200.0));
        assert!((up.side - 200.0).abs() < 1e-3 && up.angle.abs() < 1e-6);
        let top = up.to_image(INPUT_SIDE as f32 * 0.5, 0.0);
        assert!((top[0] - 100.0).abs() < 1e-3 && (top[1] - 100.0).abs() < 1e-3);
        // Leaning right: the crop's top runs toward the scale point.
        let lean = Roi::from_alignment([0.0, 0.0], [30.0, -40.0]);
        let top = lean.to_image(INPUT_SIDE as f32 * 0.5, 0.0);
        let length = top[0].hypot(top[1]);
        assert!((top[0] / length - 0.6).abs() < 1e-3 && (top[1] / length + 0.8).abs() < 1e-3);
    }
}
