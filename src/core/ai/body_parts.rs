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

/// Soft groups of classes the portrait masks read, in [`PartLabels::groups_at`] order.
pub const PART_GROUPS: usize = 8;
pub const GROUP_FACE_SKIN: usize = 0;
pub const GROUP_BODY_SKIN: usize = 1;
pub const GROUP_HAIR: usize = 2;
pub const GROUP_LIPS: usize = 3;
pub const GROUP_TEETH: usize = 4;
pub const GROUP_TONGUE: usize = 5;
pub const GROUP_GLASSES: usize = 6;
pub const GROUP_BACKDROP: usize = 7;

fn group_of(class: usize) -> Option<usize> {
    match class {
        3 => Some(GROUP_FACE_SKIN),
        5..=8 | 11 | 12 | 14..=17 | 20..=22 => Some(GROUP_BODY_SKIN),
        4 => Some(GROUP_HAIR),
        24 | 25 => Some(GROUP_LIPS),
        26 | 27 => Some(GROUP_TEETH),
        28 => Some(GROUP_TONGUE),
        2 => Some(GROUP_GLASSES),
        0 => Some(GROUP_BACKDROP),
        _ => None,
    }
}

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

    /// The closest upright 3:4 crop holding `frame` ([x0, y0, x1, y1], a
    /// selection drawn around the person) with a small margin, when it is
    /// closer than this crop — the model then sees the head at a finer scale.
    fn closer(self, frame: [f32; 4]) -> Self {
        let [x0, y0, x1, y1] = frame;
        let width = (x1 - x0).max((y1 - y0) * INPUT_W as f32 / INPUT_H as f32) * 1.08;
        if !(width > 1.0 && width < self.width) {
            return self;
        }
        Self {
            cx: (x0 + x1) * 0.5,
            cy: (y0 + y1) * 0.5,
            width,
            angle: 0.0,
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
    /// Softmax probability of each part group per crop pixel, 0..255.
    groups: Vec<[u8; PART_GROUPS]>,
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

    /// Group probabilities (0..1) at image coordinates, bilinear; zero outside the crop.
    pub fn groups_at(&self, x: f32, y: f32) -> [f32; PART_GROUPS] {
        let [u, v] = self.crop.to_crop(x, y);
        let (px, py) = (u - 0.5, v - 0.5);
        if px < -0.5 || py < -0.5 || px > INPUT_W as f32 - 0.5 || py > INPUT_H as f32 - 0.5 {
            return [0.0; PART_GROUPS];
        }
        let px = px.clamp(0.0, (INPUT_W - 1) as f32);
        let py = py.clamp(0.0, (INPUT_H - 1) as f32);
        let (x0, y0) = (px as usize, py as usize);
        let (x1, y1) = ((x0 + 1).min(INPUT_W - 1), (y0 + 1).min(INPUT_H - 1));
        let (fx, fy) = (px - x0 as f32, py - y0 as f32);
        let at = |xx: usize, yy: usize| &self.groups[yy * INPUT_W + xx];
        let (a, b, c, d) = (at(x0, y0), at(x1, y0), at(x0, y1), at(x1, y1));
        let mut out = [0.0; PART_GROUPS];
        for (g, value) in out.iter_mut().enumerate() {
            let top = a[g] as f32 * (1.0 - fx) + b[g] as f32 * fx;
            let bottom = c[g] as f32 * (1.0 - fx) + d[g] as f32 * fx;
            *value = (top * (1.0 - fy) + bottom * fy) / 255.0;
        }
        out
    }

    /// Axis-aligned image bounds of the crop: [x0, y0, x1, y1].
    pub fn bounds(&self) -> [f32; 4] {
        let corners = [
            self.crop.to_image(0.0, 0.0),
            self.crop.to_image(INPUT_W as f32, 0.0),
            self.crop.to_image(0.0, INPUT_H as f32),
            self.crop.to_image(INPUT_W as f32, INPUT_H as f32),
        ];
        let mut b = [f32::MAX, f32::MAX, f32::MIN, f32::MIN];
        for [x, y] in corners {
            b[0] = b[0].min(x);
            b[1] = b[1].min(y);
            b[2] = b[2].max(x);
            b[3] = b[3].max(y);
        }
        b
    }

    /// Whether image point (x, y) lies inside this crop.
    pub fn covers(&self, x: f32, y: f32) -> bool {
        let [u, v] = self.crop.to_crop(x, y);
        u >= 0.0 && v >= 0.0 && u < INPUT_W as f32 && v < INPUT_H as f32
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

    fn run(&mut self, input: Vec<f32>) -> Result<(Vec<u8>, Vec<[u8; PART_GROUPS]>), String> {
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
                let peak = logits[best * plane + i];
                let mut total = 0.0f32;
                let mut sums = [0.0f32; PART_GROUPS];
                for class in 0..CLASS_COUNT {
                    let e = (logits[class * plane + i] - peak).exp();
                    total += e;
                    if let Some(group) = group_of(class) {
                        sums[group] += e;
                    }
                }
                let groups = sums.map(|s| (s / total * 255.0).round() as u8);
                (best as u8, groups)
            })
            .unzip())
    }

    /// Segment the head-and-shoulders region around one face.
    pub fn segment_face(
        &mut self,
        rgba: &[u8],
        width: u32,
        height: u32,
        mesh: &FaceMesh,
    ) -> Result<PartLabels, String> {
        self.segment_face_within(rgba, width, height, mesh, None)
    }

    /// [`segment_face`](Self::segment_face), looking only at `frame`
    /// ([x0, y0, x1, y1]) when that is closer than the crop around the face.
    pub fn segment_face_within(
        &mut self,
        rgba: &[u8],
        width: u32,
        height: u32,
        mesh: &FaceMesh,
        frame: Option<[f32; 4]>,
    ) -> Result<PartLabels, String> {
        let crop = match frame {
            Some(f) => PartCrop::around(mesh).closer(f),
            None => PartCrop::around(mesh),
        };
        let input = sample_crop(rgba, width, height, &crop);
        let (labels, groups) = match self.run(input.clone()) {
            Ok(result) => result,
            // DirectML can accept the graph yet fail to allocate at run time.
            Err(_) if self.on_gpu => {
                *self = Self::load(false)?;
                self.run(input)?
            }
            Err(error) => return Err(error),
        };
        let groups = soften(&groups);
        let (centre, extent, _) = mesh.frame();
        let mut result = PartLabels {
            crop,
            labels,
            groups,
            face: (centre, extent),
            agreement: 0.0,
        };
        result.agreement = agreement(&result, mesh);
        Ok(result)
    }
}

/// Soften the part odds by about one model pixel (two [1 2 1] passes each
/// way), so outlines scaled up to the photo follow smooth curves instead of
/// the model grid's stair steps.
fn soften(groups: &[[u8; PART_GROUPS]]) -> Vec<[u8; PART_GROUPS]> {
    let mut data: Vec<[f32; PART_GROUPS]> =
        groups.iter().map(|g| g.map(|v| v as f32 / 255.0)).collect();
    let pass = |data: &[[f32; PART_GROUPS]], horizontal: bool| -> Vec<[f32; PART_GROUPS]> {
        (0..data.len())
            .into_par_iter()
            .map(|i| {
                let (x, y) = (i % INPUT_W, i / INPUT_W);
                let (a, b) = if horizontal {
                    (
                        y * INPUT_W + x.saturating_sub(1),
                        y * INPUT_W + (x + 1).min(INPUT_W - 1),
                    )
                } else {
                    (
                        y.saturating_sub(1) * INPUT_W + x,
                        (y + 1).min(INPUT_H - 1) * INPUT_W + x,
                    )
                };
                let mut out = [0.0f32; PART_GROUPS];
                for (g, value) in out.iter_mut().enumerate() {
                    *value = (data[a][g] + 2.0 * data[i][g] + data[b][g]) * 0.25;
                }
                out
            })
            .collect()
    };
    for _ in 0..2 {
        data = pass(&data, true);
        data = pass(&data, false);
    }
    data.into_iter()
        .map(|g| g.map(|v| (v.clamp(0.0, 1.0) * 255.0).round() as u8))
        .collect()
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
    fn soften_turns_grid_steps_into_ramps() {
        let mut groups = vec![[0u8; PART_GROUPS]; INPUT_W * INPUT_H];
        for (i, g) in groups.iter_mut().enumerate() {
            if i % INPUT_W >= INPUT_W / 2 {
                g[GROUP_HAIR] = 255;
            }
        }
        let soft = soften(&groups);
        let row = 100 * INPUT_W;
        let edge = INPUT_W / 2;
        assert_eq!(soft[row + 10][GROUP_HAIR], 0);
        assert_eq!(soft[row + INPUT_W - 10][GROUP_HAIR], 255);
        let ramp: Vec<u8> = (edge - 2..edge + 2)
            .map(|x| soft[row + x][GROUP_HAIR])
            .collect();
        assert!(ramp.windows(2).all(|p| p[0] < p[1]), "{ramp:?}");
        assert!(ramp[0] > 0 && ramp[3] < 255, "{ramp:?}");
    }

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

    #[test]
    fn a_close_selection_gives_a_finer_upright_crop() {
        let around = PartCrop {
            cx: 500.0,
            cy: 600.0,
            width: 900.0,
            angle: 0.2,
        };
        // A head-sized selection, taller than 3:4: the crop holds its height.
        let close = around.closer([400.0, 300.0, 700.0, 800.0]);
        assert_eq!((close.cx, close.cy, close.angle), (550.0, 550.0, 0.0));
        assert!((close.width - 500.0 * 0.75 * 1.08).abs() < 1e-3);
        let [x0, y0] = close.to_image(0.0, 0.0);
        let [x1, y1] = close.to_image(INPUT_W as f32, INPUT_H as f32);
        assert!(x0 <= 400.0 && y0 <= 300.0 && x1 >= 700.0 && y1 >= 800.0);
        // A selection wider than the face's own crop changes nothing.
        let wide = around.closer([0.0, 0.0, 2000.0, 2000.0]);
        assert_eq!((wide.cx, wide.width, wide.angle), (500.0, 900.0, 0.2));
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
