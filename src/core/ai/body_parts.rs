//! Sapiens2 body-part segmentation (trial).
//!
//! Each face found by the face mesh gets a head-and-shoulders crop, levelled by
//! the eye line and shaped 3:4 like the person crops Sapiens2 was trained on,
//! run at 512x384. The mesh also cross-checks the result: when the pixels under
//! its landmarks are not labelled as face, lips, teeth, glasses or hair, the
//! segmentation of that face is not trusted.

use std::path::PathBuf;

use rayon::prelude::*;

use super::face_mesh::FaceMesh;

pub const CLASS_COUNT: usize = 29;
const INPUT_H: usize = 512;
const INPUT_W: usize = 384;
const MODEL_DIR: &str = "sapiens2-seg";
const MODEL_FILE: &str = "sapiens2_seg_0.4b_512x384.onnx";
/// Crop width in face extents (forehead to chin); height follows at 4:3.
const CROP_WIDTH_FACES: f32 = 3.0;
/// The face centre sits this far down the crop, leaving room for neck and shoulders.
const FACE_CENTRE_DOWN: f32 = 0.4;
const MEAN: [f32; 3] = [0.485, 0.456, 0.406];
const STD: [f32; 3] = [0.229, 0.224, 0.225];

pub const CLASS_NAMES: [&str; CLASS_COUNT] = [
    "Nền",
    "Phụ kiện",
    "Kính",
    "Mặt + cổ",
    "Tóc",
    "Bàn chân trái",
    "Bàn tay trái",
    "Cẳng tay trái",
    "Cẳng chân trái",
    "Giày trái",
    "Tất trái",
    "Bắp tay trái",
    "Đùi trái",
    "Quần/váy",
    "Bàn chân phải",
    "Bàn tay phải",
    "Cẳng tay phải",
    "Cẳng chân phải",
    "Giày phải",
    "Tất phải",
    "Bắp tay phải",
    "Đùi phải",
    "Thân",
    "Áo",
    "Môi dưới",
    "Môi trên",
    "Răng dưới",
    "Răng trên",
    "Lưỡi",
];
const GLASSES: u8 = 2;
const FACE_NECK: u8 = 3;
const HAIR: u8 = 4;
const LOWER_LIP: u8 = 24;
const TONGUE: u8 = 28;

pub fn model_path() -> Option<PathBuf> {
    super::retouch::model_roots()
        .into_iter()
        .map(|root| root.join(MODEL_DIR).join(MODEL_FILE))
        .find(|path| path.is_file())
}

/// A 3:4 rectangle around one person's head, rotated by `angle`.
#[derive(Clone, Copy, Debug)]
struct PartCrop {
    cx: f32,
    cy: f32,
    width: f32,
    angle: f32,
}

impl PartCrop {
    fn around(mesh: &FaceMesh) -> Self {
        let ([fx, fy], extent, angle) = mesh.frame();
        let width = extent * CROP_WIDTH_FACES;
        let down = width * INPUT_H as f32 / INPUT_W as f32 * (0.5 - FACE_CENTRE_DOWN);
        let (sin, cos) = angle.sin_cos();
        Self {
            cx: fx - sin * down,
            cy: fy + cos * down,
            width,
            angle,
        }
    }

    fn scale(&self) -> f32 {
        self.width / INPUT_W as f32
    }

    /// Continuous crop coordinates (0..INPUT_W, 0..INPUT_H) to image coordinates.
    fn to_image(&self, u: f32, v: f32) -> [f32; 2] {
        let (sin, cos) = self.angle.sin_cos();
        let du = (u - INPUT_W as f32 * 0.5) * self.scale();
        let dv = (v - INPUT_H as f32 * 0.5) * self.scale();
        [self.cx + cos * du - sin * dv, self.cy + sin * du + cos * dv]
    }

    fn to_crop(&self, x: f32, y: f32) -> [f32; 2] {
        let (sin, cos) = self.angle.sin_cos();
        let (dx, dy) = (x - self.cx, y - self.cy);
        [
            (cos * dx + sin * dy) / self.scale() + INPUT_W as f32 * 0.5,
            (-sin * dx + cos * dy) / self.scale() + INPUT_H as f32 * 0.5,
        ]
    }
}

/// Per-pixel class labels for one head crop.
#[derive(Clone, Debug)]
pub struct PartLabels {
    crop: PartCrop,
    labels: Vec<u8>,
    /// Face centre and forehead-to-chin extent from the mesh.
    face: ([f32; 2], f32),
    /// Share of the face mesh's landmarks that land on face-like classes.
    pub agreement: f32,
}

impl PartLabels {
    fn label_at(&self, x: f32, y: f32) -> Option<u8> {
        let [u, v] = self.crop.to_crop(x, y);
        if u < 0.0 || v < 0.0 || u >= INPUT_W as f32 || v >= INPUT_H as f32 {
            return None;
        }
        Some(self.labels[v as usize * INPUT_W + u as usize])
    }
}

pub struct Segmenter {
    session: ort::session::Session,
    pub on_gpu: bool,
}

impl Segmenter {
    pub fn load(prefer_gpu: bool) -> Result<Self, String> {
        let path = model_path()
            .ok_or_else(|| format!("thiếu model tách vùng (models\\{MODEL_DIR}\\{MODEL_FILE})"))?;
        let (session, on_gpu) = super::ort_ep::build_session(&path, prefer_gpu)?;
        Ok(Self { session, on_gpu })
    }

    fn run(&mut self, input: Vec<f32>) -> Result<Vec<u8>, String> {
        let tensor = ort::value::Tensor::<f32>::from_array((
            [1i64, 3, INPUT_H as i64, INPUT_W as i64],
            input,
        ))
        .map_err(|e| format!("Sapiens2 input tensor: {e}"))?;
        let outputs = self
            .session
            .run(ort::inputs!["pixel_values" => tensor])
            .map_err(|e| format!("Sapiens2 inference: {e}"))?;
        let (_, logits) = outputs["logits"]
            .try_extract_tensor::<f32>()
            .map_err(|e| format!("Sapiens2 logits: {e}"))?;
        let plane = INPUT_H * INPUT_W;
        if logits.len() < plane * CLASS_COUNT {
            return Err("Sapiens2 output contract mismatch".to_string());
        }
        Ok((0..plane)
            .into_par_iter()
            .map(|i| {
                let mut best = 0;
                for class in 1..CLASS_COUNT {
                    if logits[class * plane + i] > logits[best * plane + i] {
                        best = class;
                    }
                }
                best as u8
            })
            .collect())
    }

    /// Segment the head-and-shoulders region around one face.
    pub fn segment_face(
        &mut self,
        rgba: &[u8],
        width: u32,
        height: u32,
        mesh: &FaceMesh,
    ) -> Result<PartLabels, String> {
        let crop = PartCrop::around(mesh);
        let input = sample_crop(rgba, width, height, &crop);
        let labels = match self.run(input.clone()) {
            Ok(labels) => labels,
            // DirectML can accept the graph yet fail to allocate at run time.
            Err(_) if self.on_gpu => {
                *self = Self::load(false)?;
                self.run(input)?
            }
            Err(error) => return Err(error),
        };
        let (centre, extent, _) = mesh.frame();
        let mut result = PartLabels {
            crop,
            labels,
            face: (centre, extent),
            agreement: 0.0,
        };
        result.agreement = agreement(&result, mesh);
        Ok(result)
    }
}

fn agreement(parts: &PartLabels, mesh: &FaceMesh) -> f32 {
    let (mut seen, mut agreed) = (0usize, 0usize);
    // The first 468 points cover the face surface; the rest are irises.
    for p in mesh.points.iter().take(468) {
        if let Some(label) = parts.label_at(p[0], p[1]) {
            seen += 1;
            if matches!(label, GLASSES | FACE_NECK | HAIR) || (LOWER_LIP..=TONGUE).contains(&label)
            {
                agreed += 1;
            }
        }
    }
    if seen == 0 {
        0.0
    } else {
        agreed as f32 / seen as f32
    }
}

/// NCHW input, ImageNet-normalised; supersampled when the crop is larger.
fn sample_crop(rgba: &[u8], width: u32, height: u32, crop: &PartCrop) -> Vec<f32> {
    let taps = (crop.scale().ceil() as usize).clamp(1, 6);
    let plane = INPUT_H * INPUT_W;
    let mut rows: Vec<[f32; 3]> = vec![[0.0; 3]; plane];
    rows.par_chunks_mut(INPUT_W)
        .enumerate()
        .for_each(|(row, line)| {
            for (col, out) in line.iter_mut().enumerate() {
                let mut sum = [0.0f32; 3];
                for sy in 0..taps {
                    for sx in 0..taps {
                        let u = col as f32 + (sx as f32 + 0.5) / taps as f32;
                        let v = row as f32 + (sy as f32 + 0.5) / taps as f32;
                        let [x, y] = crop.to_image(u, v);
                        let rgb = super::face_mesh::sample_rgb(rgba, width, height, x, y);
                        for c in 0..3 {
                            sum[c] += rgb[c];
                        }
                    }
                }
                let norm = 1.0 / (255.0 * (taps * taps) as f32);
                for c in 0..3 {
                    out[c] = (sum[c] * norm - MEAN[c]) / STD[c];
                }
            }
        });
    let mut input = vec![0.0f32; plane * 3];
    for (i, rgb) in rows.iter().enumerate() {
        for c in 0..3 {
            input[c * plane + i] = rgb[c];
        }
    }
    input
}

fn class_color(class: u8) -> [u8; 4] {
    match class {
        1 => [150, 110, 255, 150],
        2 => [0, 255, 255, 170],
        3 => [255, 150, 90, 120],
        4 => [140, 90, 20, 170],
        6 | 15 => [255, 230, 0, 150],
        13 | 23 => [60, 90, 220, 120],
        22 => [255, 200, 160, 120],
        24 | 25 => [255, 0, 130, 190],
        26 | 27 => [0, 255, 120, 200],
        28 => [255, 0, 0, 200],
        0 => [0, 0, 0, 0],
        _ => [255, 190, 140, 120],
    }
}

/// Transparent canvas-size layer colouring each class inside every trusted
/// crop; a face the mesh disagrees with is hatched red instead.
pub fn render_overlay(width: u32, height: u32, parts: &[PartLabels], trusted: f32) -> Vec<u8> {
    let mut buffer = vec![0u8; width as usize * height as usize * 4];
    let mut ordered: Vec<&PartLabels> = parts.iter().collect();
    ordered.sort_by_key(|part| part.agreement >= trusted);
    for part in ordered {
        let corners = [
            part.crop.to_image(0.0, 0.0),
            part.crop.to_image(INPUT_W as f32, 0.0),
            part.crop.to_image(0.0, INPUT_H as f32),
            part.crop.to_image(INPUT_W as f32, INPUT_H as f32),
        ];
        let x0 = corners
            .iter()
            .map(|c| c[0])
            .fold(f32::MAX, f32::min)
            .max(0.0) as usize;
        let y0 = corners
            .iter()
            .map(|c| c[1])
            .fold(f32::MAX, f32::min)
            .max(0.0) as usize;
        let x1 = (corners.iter().map(|c| c[0]).fold(f32::MIN, f32::max).ceil() as usize)
            .min(width as usize);
        let y1 = (corners.iter().map(|c| c[1]).fold(f32::MIN, f32::max).ceil() as usize)
            .min(height as usize);
        let doubtful = part.agreement < trusted;
        let ([fx, fy], extent) = part.face;
        buffer
            .par_chunks_mut(width as usize * 4)
            .enumerate()
            .skip(y0)
            .take(y1.saturating_sub(y0))
            .for_each(|(y, line)| {
                for x in x0..x1 {
                    let Some(label) = part.label_at(x as f32 + 0.5, y as f32 + 0.5) else {
                        continue;
                    };
                    let color = if doubtful {
                        let near_face = (x as f32 - fx).hypot(y as f32 - fy) < extent * 0.7;
                        if near_face && (x + y) / 6 % 2 == 0 {
                            [255, 0, 0, 150]
                        } else {
                            continue;
                        }
                    } else if label == 0 {
                        continue;
                    } else {
                        class_color(label)
                    };
                    let pixel = &mut line[x * 4..x * 4 + 4];
                    pixel.fill(0);
                    super::face_mesh::blend_over(pixel, color, 1.0);
                }
            });
    }
    buffer
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn part_crop_round_trips_between_image_and_crop_space() {
        let crop = PartCrop {
            cx: 300.0,
            cy: 200.0,
            width: 600.0,
            angle: -0.4,
        };
        for (u, v) in [(0.0, 0.0), (100.0, 400.0), (383.0, 511.0)] {
            let [x, y] = crop.to_image(u, v);
            let [u2, v2] = crop.to_crop(x, y);
            assert!((u - u2).abs() < 1e-3 && (v - v2).abs() < 1e-3);
        }
    }

    /// Opt-in: set IAI_BODY_PARTS_PROBE to a folder of images; writes
    /// `parts_<name>.png` overlays and prints timing and mesh agreement.
    #[test]
    #[ignore]
    fn probe_body_parts() {
        let Ok(dir) = std::env::var("IAI_BODY_PARTS_PROBE") else {
            return;
        };
        let dir = PathBuf::from(dir);
        for gpu in [true, false] {
            let started = std::time::Instant::now();
            let mut segmenter = match Segmenter::load(gpu) {
                Ok(segmenter) => segmenter,
                Err(error) => {
                    println!("load gpu={gpu}: {error}");
                    continue;
                }
            };
            println!(
                "load gpu={gpu} -> on_gpu={} in {} ms",
                segmenter.on_gpu,
                started.elapsed().as_millis()
            );
            let names = ["t_blake.png", "t_group00.jpg", "t_old10045.png"];
            for name in names {
                let image = image::open(dir.join(name)).unwrap().to_rgba8();
                let (width, height) = image.dimensions();
                let meshes =
                    super::super::face_mesh::detect(image.as_raw(), width, height).unwrap();
                let mut parts = Vec::new();
                let mut line = format!("  {name}:");
                for mesh in &meshes {
                    let started = std::time::Instant::now();
                    match segmenter.segment_face(image.as_raw(), width, height, mesh) {
                        Ok(part) => {
                            line.push_str(&format!(
                                " [{} ms, agree {:.0}%, gpu={}]",
                                started.elapsed().as_millis(),
                                part.agreement * 100.0,
                                segmenter.on_gpu
                            ));
                            parts.push(part);
                        }
                        Err(error) => line.push_str(&format!(" [error {error}]")),
                    }
                }
                println!("{line}");
                if !gpu || segmenter.on_gpu {
                    let mut composed = image.clone().into_raw();
                    let overlay = render_overlay(width, height, &parts, 0.8);
                    for (pixel, over) in composed.chunks_exact_mut(4).zip(overlay.chunks_exact(4)) {
                        super::super::face_mesh::blend_over(
                            pixel,
                            [over[0], over[1], over[2], 255],
                            over[3] as f32 / 255.0,
                        );
                    }
                    image::RgbaImage::from_raw(width, height, composed)
                        .unwrap()
                        .save(dir.join(format!("parts_{gpu}_{name}.png")))
                        .unwrap();
                }
            }
        }
    }
}
