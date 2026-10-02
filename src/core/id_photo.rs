//! "Làm ảnh thẻ" (Image ▸ Làm ảnh thẻ…): frame a 3×4 ID photo — 2.8×3.8 cm at
//! 600 ppi, as shops cut it — from the face's eye line and chin, levelled by
//! the eyes, and cut the person out onto white.
//!
//! The proportions come from the owner's reference print (661×898 px): eye
//! line 33.6% and chin 56.7% of the way down, face centred. The frame is that
//! reference widened a little (the owner trims tighter by hand when wanted).

use super::ai::face_mesh::{self, FaceMesh};
use super::canvas::Canvas;
use super::command::LayerStructureCommand;
use super::portrait::{Clip, Region};
use super::tile::TileMap;

pub const PRINT_CM: [f32; 2] = [2.8, 3.8];
pub const PRINT_PPI: f32 = 600.0;
pub const DEFAULT_WIDEN: f32 = 0.10;
pub const MAX_WIDEN: f32 = 0.40;
pub const ORIGINAL_LAYER: &str = "Ảnh gốc";
const BACKGROUND_STEP: &str = "Nền trắng";
const UNDO_LABEL: &str = "Làm ảnh thẻ";

/// Eye line and chin, as fractions of the reference frame's height.
const EYE_DOWN: f32 = 0.3359;
const CHIN_DOWN: f32 = 0.5669;
/// Least room above the head, and how far the frame may rise to make it.
const MIN_HEAD_ROOM: f32 = 0.06;
const MAX_HEAD_RAISE: f32 = 0.08;
/// Tilts below this are landmark noise, not worth a resample.
const MIN_TILT_DEGREES: f32 = 1.0;

const RIGHT_EYE_OUTER: usize = 33;
const LEFT_EYE_OUTER: usize = 263;
const RIGHT_IRIS: usize = 468;
const LEFT_IRIS: usize = 473;
const CHIN: usize = 152;
const RIGHT_CHEEK: usize = 234;
const LEFT_CHEEK: usize = 454;
const NOSE_TIP: usize = 1;

/// Output size in pixels of the print size at `PRINT_PPI` (661×898).
pub fn output_size() -> (u32, u32) {
    let px = |cm: f32| (cm / 2.54 * PRINT_PPI).round() as u32;
    (px(PRINT_CM[0]), px(PRINT_CM[1]))
}

#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct IdPhotoOptions {
    pub crop: bool,
    /// How much larger than the reference framing (0.10 = 10%).
    pub widen: f32,
    pub straighten: bool,
    pub white_background: bool,
    /// Open Chỉnh chân dung on the result.
    pub then_portrait: bool,
}

impl Default for IdPhotoOptions {
    fn default() -> Self {
        Self {
            crop: true,
            widen: DEFAULT_WIDEN,
            straighten: true,
            white_background: true,
            then_portrait: true,
        }
    }
}

/// The landmarks the framing is measured from.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FaceMarks {
    /// Midpoint of the two irises.
    pub eyes: [f32; 2],
    pub chin: [f32; 2],
    /// Midpoint of the face outline at the cheeks.
    pub cheeks: [f32; 2],
    pub nose: [f32; 2],
    /// Slope of the line through the outer eye corners (radians).
    pub tilt: f32,
    /// Cheek-to-cheek width.
    pub width: f32,
}

impl FaceMarks {
    pub fn from_mesh(mesh: &FaceMesh) -> Self {
        let p = |i: usize| [mesh.points[i][0], mesh.points[i][1]];
        let mid = |a: [f32; 2], b: [f32; 2]| [(a[0] + b[0]) * 0.5, (a[1] + b[1]) * 0.5];
        let (right, left) = (p(RIGHT_EYE_OUTER), p(LEFT_EYE_OUTER));
        let (right_cheek, left_cheek) = (p(RIGHT_CHEEK), p(LEFT_CHEEK));
        Self {
            eyes: mid(p(RIGHT_IRIS), p(LEFT_IRIS)),
            chin: p(CHIN),
            cheeks: mid(right_cheek, left_cheek),
            nose: p(NOSE_TIP),
            tilt: (left[1] - right[1]).atan2(left[0] - right[0]),
            width: (left_cheek[0] - right_cheek[0]).hypot(left_cheek[1] - right_cheek[1]),
        }
    }
}

/// A `width × height` box centred on `centre`, its rows running at `angle`
/// (radians, image space, y down).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Frame {
    pub centre: [f32; 2],
    pub width: f32,
    pub height: f32,
    pub angle: f32,
}

impl Frame {
    /// Unit vectors along the rows and down the columns.
    fn axes(&self) -> ([f32; 2], [f32; 2]) {
        let (sin, cos) = self.angle.sin_cos();
        ([cos, sin], [-sin, cos])
    }

    /// Image point at (u, v) from the frame's top-left corner.
    pub fn at(&self, u: f32, v: f32) -> [f32; 2] {
        let (along, down) = self.axes();
        let (du, dv) = (u - self.width * 0.5, v - self.height * 0.5);
        [
            self.centre[0] + along[0] * du + down[0] * dv,
            self.centre[1] + along[1] * du + down[1] * dv,
        ]
    }

    /// (u, v) from the frame's top-left corner of image point `p`.
    pub fn local(&self, p: [f32; 2]) -> [f32; 2] {
        let (along, down) = self.axes();
        let d = [p[0] - self.centre[0], p[1] - self.centre[1]];
        [
            d[0] * along[0] + d[1] * along[1] + self.width * 0.5,
            d[0] * down[0] + d[1] * down[1] + self.height * 0.5,
        ]
    }

    /// Top-left, top-right, bottom-right, bottom-left.
    pub fn corners(&self) -> [[f32; 2]; 4] {
        [
            self.at(0.0, 0.0),
            self.at(self.width, 0.0),
            self.at(self.width, self.height),
            self.at(0.0, self.height),
        ]
    }

    /// Whether the bottom edge (shoulders) lies on the photo: the top and the
    /// upper sides are background, which turns white anyway.
    fn bottom_inside(&self, width: u32, height: u32) -> bool {
        let c = self.corners();
        [c[2], c[3]]
            .iter()
            .all(|p| p[0] >= -1.0 && p[0] <= width as f32 + 1.0 && p[1] <= height as f32 + 1.0)
    }

    fn raised(mut self, by: f32) -> Self {
        let (_, down) = self.axes();
        self.centre[0] -= down[0] * by;
        self.centre[1] -= down[1] * by;
        self
    }
}

/// The reference framing of `face`, `widen` larger around the middle of eyes
/// and chin, its rows along the eye line when `straighten`. `aspect` is
/// width / height.
pub fn frame_for(face: &FaceMarks, widen: f32, straighten: bool, aspect: f32) -> Frame {
    let angle = if straighten && face.tilt.abs() >= MIN_TILT_DEGREES.to_radians() {
        face.tilt
    } else {
        0.0
    };
    let (sin, cos) = angle.sin_cos();
    let (along, down) = ([cos, sin], [-sin, cos]);
    let rel = |p: [f32; 2]| {
        let d = [p[0] - face.eyes[0], p[1] - face.eyes[1]];
        [
            d[0] * along[0] + d[1] * along[1],
            d[0] * down[0] + d[1] * down[1],
        ]
    };
    let chin = rel(face.chin)[1].max(1.0);
    let reference = chin / (CHIN_DOWN - EYE_DOWN);
    let scale = 1.0 + widen.max(-0.5);
    let middle = chin * 0.5;
    let top = middle - (middle + EYE_DOWN * reference) * scale;
    let height = reference * scale;
    // Halfway between the eyes and the cheeks: steadier on a turned head.
    let u = 0.5 * rel(face.cheeks)[0];
    let v = top + height * 0.5;
    Frame {
        centre: [
            face.eyes[0] + along[0] * u + down[0] * v,
            face.eyes[1] + along[1] * u + down[1] * v,
        ],
        width: height * aspect,
        height,
        angle,
    }
}

/// `frame_for` as wide as `options` ask while the shoulders stay on the
/// photo, never tighter than the reference. Returns the widening used.
fn fit_frame(
    face: &FaceMarks,
    options: &IdPhotoOptions,
    aspect: f32,
    width: u32,
    height: u32,
) -> (Frame, f32) {
    let mut widen = options.widen.clamp(0.0, MAX_WIDEN);
    let mut frame = frame_for(face, widen, options.straighten, aspect);
    while widen > 0.0 && !frame.bottom_inside(width, height) {
        widen = (widen - 0.01).max(0.0);
        frame = frame_for(face, widen, options.straighten, aspect);
    }
    (frame, widen)
}

/// The person cut out of `region` of the photo: its pixels (edge colours
/// cleaned of the old background) and soft mask.
pub struct Cutout {
    pub region: Region,
    pub rgba: Vec<u8>,
    pub mask: Vec<u8>,
    /// The region as photographed, for the "Ảnh gốc" layer.
    pub original: Vec<u8>,
}

pub struct IdPhotoPlan {
    pub frame: Option<Frame>,
    pub cutout: Option<Cutout>,
    /// Short Vietnamese remarks for the status line.
    pub notes: Vec<String>,
}

/// Find the face (in `clip` when given; the largest when several), frame it
/// and, when asked, cut the person out with `segment` (an RGBA region in, a
/// soft mask of the same size out). `progress` gets short status lines.
pub fn prepare(
    rgba: &[u8],
    width: u32,
    height: u32,
    clip: Option<&Clip>,
    options: &IdPhotoOptions,
    segment: &mut dyn FnMut(&[u8], u32, u32) -> Result<Vec<u8>, String>,
    progress: &dyn Fn(String),
) -> Result<IdPhotoPlan, String> {
    if width == 0 || height == 0 || rgba.len() != width as usize * height as usize * 4 {
        return Err("ảnh không hợp lệ".to_string());
    }
    if !options.crop && !options.white_background {
        return Err("chưa chọn việc nào (cắt khung / nền trắng)".to_string());
    }
    progress("Đang tìm khuôn mặt…".to_string());
    let meshes = match clip {
        Some(clip) => super::portrait::analysis::faces_in(rgba, width, height, clip)?,
        None => face_mesh::detect(rgba, width, height)?,
    };
    let face = meshes
        .iter()
        .max_by(|a, b| a.frame().1.total_cmp(&b.frame().1))
        .map(FaceMarks::from_mesh);
    let mut notes = Vec::new();
    if face.is_none() && options.crop {
        return Err(if clip.is_some() {
            "không tìm thấy khuôn mặt nào trong vùng chọn".to_string()
        } else {
            "không tìm thấy khuôn mặt nào".to_string()
        });
    }
    if meshes.len() > 1 {
        notes.push(format!(
            "ảnh có {} khuôn mặt, đã lấy mặt lớn nhất (muốn người khác thì khoanh vùng chọn quanh người đó)",
            meshes.len()
        ));
    }

    let (out_w, out_h) = output_size();
    let aspect = out_w as f32 / out_h as f32;
    let mut frame = None;
    if let (true, Some(face)) = (options.crop, face.as_ref()) {
        let (fitted, widen) = fit_frame(face, options, aspect, width, height);
        if widen + 0.005 < options.widen {
            notes.push(format!(
                "ảnh gốc chật nên khung chỉ rộng hơn mẫu {:.0}%",
                widen * 100.0
            ));
        }
        frame = Some(fitted);
    }

    let cutout = if options.white_background {
        progress("Đang tách người khỏi nền…".to_string());
        let region = match &frame {
            // Room for the frame to rise over a tall hairdo.
            Some(f) => {
                let grow = f.height * (MAX_HEAD_RAISE + 0.07);
                Region::around(f.corners().into_iter(), [grow; 4], width, height)
            }
            None => Region {
                x: 0,
                y: 0,
                w: width,
                h: height,
            },
        };
        if region.is_empty() {
            return Err("khung nằm ngoài ảnh".to_string());
        }
        let mut pixels = copy_region(rgba, width, region);
        let mut mask = segment(&pixels, region.w, region.h)?;
        if mask.len() != region.len() {
            return Err("mask tách nền sai kích thước".to_string());
        }
        if let Some(face) = &face {
            keep_person(&mut mask, region, face.nose);
        }
        if !mask.iter().any(|&m| m >= 128) {
            return Err("không tách được người khỏi nền".to_string());
        }
        progress("Đang làm sạch viền tóc…".to_string());
        let original = pixels.clone();
        decontaminate(&mut pixels, &mask, region.w, region.h);
        Some(Cutout {
            region,
            rgba: pixels,
            mask,
            original,
        })
    } else {
        None
    };

    if let (Some(f), Some(cut), Some(face)) = (frame.as_mut(), &cutout, &face) {
        if let Some(top) = head_top(f, face, &cut.mask, cut.region) {
            let want = MIN_HEAD_ROOM * f.height - top;
            if want > 0.0 {
                *f = f.raised(want.min(MAX_HEAD_RAISE * f.height));
            }
        }
    }
    if let Some(f) = &frame {
        if !f.bottom_inside(width, height) {
            notes.push("ảnh gốc thiếu phần vai, chỗ thiếu để trắng".to_string());
        }
    }
    Ok(IdPhotoPlan {
        frame,
        cutout,
        notes,
    })
}

fn copy_region(rgba: &[u8], width: u32, region: Region) -> Vec<u8> {
    let row = region.w as usize * 4;
    let mut out = vec![0u8; row * region.h as usize];
    for (y, line) in out.chunks_exact_mut(row).enumerate() {
        let o = ((region.y as usize + y) * width as usize + region.x as usize) * 4;
        line.copy_from_slice(&rgba[o..o + row]);
    }
    out
}

/// Distance from the frame's top down to the highest masked pixel above the
/// eyes within a face width or so of the centre line.
fn head_top(frame: &Frame, face: &FaceMarks, mask: &[u8], region: Region) -> Option<f32> {
    let reach = face.width * 0.75;
    let eye_v = frame.local(face.eyes)[1];
    let mut top: Option<f32> = None;
    for (i, &m) in mask.iter().enumerate() {
        if m < 128 {
            continue;
        }
        let x = region.x as f32 + (i as u32 % region.w) as f32 + 0.5;
        let y = region.y as f32 + (i as u32 / region.w) as f32 + 0.5;
        let [u, v] = frame.local([x, y]);
        if v < eye_v && (u - frame.width * 0.5).abs() <= reach && top.is_none_or(|t| v < t) {
            top = Some(v);
        }
    }
    top
}

/// Keep only the masked blob holding `seed` (image space) and its soft rim,
/// dropping other people or objects the segmenter picked up.
fn keep_person(mask: &mut [u8], region: Region, seed: [f32; 2]) {
    let (w, h) = (region.w as usize, region.h as usize);
    let sx = (seed[0] - region.x as f32).floor();
    let sy = (seed[1] - region.y as f32).floor();
    if sx < 0.0 || sy < 0.0 || sx as usize >= w || sy as usize >= h {
        return;
    }
    // The nose may sit just off the solid mask on a poor segmentation; start
    // from the nearest solid pixel close by.
    let (sx, sy) = (sx as usize, sy as usize);
    let near = (w.max(h) / 50).max(4);
    let mut start = None;
    let mut best = usize::MAX;
    for y in sy.saturating_sub(near)..(sy + near + 1).min(h) {
        for x in sx.saturating_sub(near)..(sx + near + 1).min(w) {
            let d = x.abs_diff(sx).pow(2) + y.abs_diff(sy).pow(2);
            if mask[y * w + x] >= 128 && d < best {
                best = d;
                start = Some(y * w + x);
            }
        }
    }
    let Some(start) = start else {
        return;
    };
    let mut keep = vec![false; w * h];
    keep[start] = true;
    let mut stack = vec![start];
    while let Some(i) = stack.pop() {
        let (x, y) = (i % w, i / w);
        let mut visit = |j: usize| {
            if !keep[j] && mask[j] >= 128 {
                keep[j] = true;
                stack.push(j);
            }
        };
        if x > 0 {
            visit(i - 1);
        }
        if x + 1 < w {
            visit(i + 1);
        }
        if y > 0 {
            visit(i - w);
        }
        if y + 1 < h {
            visit(i + w);
        }
    }
    let rim = (w.max(h) / 150).max(2);
    let near_keep = dilate(&keep, w, h, rim);
    for (m, near) in mask.iter_mut().zip(near_keep) {
        if !near {
            *m = 0;
        }
    }
}

/// Square dilation by `r` of a boolean mask (separable running counts).
fn dilate(src: &[bool], w: usize, h: usize, r: usize) -> Vec<bool> {
    let mut rows = vec![false; w * h];
    for y in 0..h {
        let line = &src[y * w..(y + 1) * w];
        let mut count = line[..r.min(w)].iter().filter(|&&b| b).count();
        for x in 0..w {
            if x + r < w && line[x + r] {
                count += 1;
            }
            rows[y * w + x] = count > 0;
            if x >= r && line[x - r] {
                count -= 1;
            }
        }
    }
    let mut out = vec![false; w * h];
    for x in 0..w {
        let mut count = (0..r.min(h)).filter(|&y| rows[y * w + x]).count();
        for y in 0..h {
            if y + r < h && rows[(y + r) * w + x] {
                count += 1;
            }
            out[y * w + x] = count > 0;
            if y >= r && rows[(y - r) * w + x] {
                count -= 1;
            }
        }
    }
    out
}

/// Recolour the partly masked rim (hair) so it carries no tint of the old
/// background and can sit on white. Where the mask is dense the compositing
/// equation `I = α·F + (1−α)·B` is solved for F (B the local clean
/// background), which at most doubles the noise. Thin hair would blow the
/// noise up there, so it takes blur-fusion foreground estimation instead
/// (Germer et al. 2021): with local means F̂, B̂,
/// `F = F̂ + α·(I − α·F̂ − (1−α)·B̂)` — first with wide means, then with narrow
/// ones around the first estimate — which divides by nothing.
pub fn decontaminate(rgba: &mut [u8], mask: &[u8], width: u32, height: u32) {
    use rayon::prelude::*;
    let (w, h) = (width as usize, height as usize);
    if w == 0 || h == 0 || mask.len() != w * h || rgba.len() != w * h * 4 {
        return;
    }
    let side = w.max(h);
    let cell = (side / 75).max(4);
    let alpha = |m: u8| m as f32 / 255.0;
    let (Some(fore), Some(back)) = (
        Field::weighted(rgba, mask, w, h, cell, |m| alpha(m)),
        Field::weighted(rgba, mask, w, h, cell, |m| {
            if m <= CLEAN_BACKGROUND {
                1.0
            } else {
                0.0
            }
        }),
    ) else {
        return;
    };
    let fuse = |i: [f32; 3], f: [f32; 3], b: [f32; 3], a: f32| -> [u8; 3] {
        let mut out = [0u8; 3];
        for c in 0..3 {
            let v = f[c] + a * (i[c] - a * f[c] - (1.0 - a) * b[c]);
            out[c] = v.round().clamp(0.0, 255.0) as u8;
        }
        out
    };
    let rim = |m: u8| m > 0 && m < 255;
    let mut stage = rgba.to_vec();
    stage
        .par_chunks_mut(w * 4)
        .enumerate()
        .for_each(|(y, row)| {
            for x in 0..w {
                let m = mask[y * w + x];
                if !rim(m) {
                    continue;
                }
                let px = &mut row[x * 4..x * 4 + 3];
                let i = [px[0] as f32, px[1] as f32, px[2] as f32];
                px.copy_from_slice(&fuse(i, fore.at(x, y), back.at(x, y), alpha(m)));
            }
        });
    let r = (side / 400).max(3);
    rgba.par_chunks_mut(w * 4).enumerate().for_each(|(y, row)| {
        for x in 0..w {
            let m = mask[y * w + x];
            if !rim(m) {
                continue;
            }
            let (mut sum, mut weight) = ([0.0f32; 3], 0.0f32);
            for yy in y.saturating_sub(r)..(y + r + 1).min(h) {
                for xx in x.saturating_sub(r)..(x + r + 1).min(w) {
                    let a = alpha(mask[yy * w + xx]);
                    let o = (yy * w + xx) * 4;
                    for c in 0..3 {
                        sum[c] += a * stage[o + c] as f32;
                    }
                    weight += a;
                }
            }
            let o = (y * w + x) * 4;
            let near = if weight > 1e-3 {
                [sum[0] / weight, sum[1] / weight, sum[2] / weight]
            } else {
                [stage[o] as f32, stage[o + 1] as f32, stage[o + 2] as f32]
            };
            let px = &mut row[x * 4..x * 4 + 3];
            let i = [px[0] as f32, px[1] as f32, px[2] as f32];
            let (a, b) = (alpha(m), back.at(x, y));
            let fused = fuse(i, near, b, a);
            let solved = (a - DENSE_LO) / (DENSE_HI - DENSE_LO);
            let solved = solved.clamp(0.0, 1.0);
            let solved = solved * solved * (3.0 - 2.0 * solved);
            for c in 0..3 {
                let exact = ((i[c] - (1.0 - a) * b[c]) / a).clamp(0.0, 255.0);
                let v = solved * exact + (1.0 - solved) * fused[c] as f32;
                px[c] = v.round().clamp(0.0, 255.0) as u8;
            }
        }
    });
}

/// Mask values counted as clean background, and the mask range over which
/// the exact solve takes over from blur-fusion.
const CLEAN_BACKGROUND: u8 = 13;
const DENSE_LO: f32 = 0.35;
const DENSE_HI: f32 = 0.6;

/// A smooth colour field: the `weight`-weighted mean colour on a coarse grid,
/// each cell averaged over the smallest window holding enough weight.
struct Field {
    cells: Vec<[f32; 3]>,
    cols: usize,
    rows: usize,
    cell: usize,
}

impl Field {
    fn weighted(
        rgba: &[u8],
        mask: &[u8],
        w: usize,
        h: usize,
        cell: usize,
        weight: impl Fn(u8) -> f32,
    ) -> Option<Self> {
        let (cols, rows) = (w.div_ceil(cell), h.div_ceil(cell));
        let mut sums = vec![[0.0f32; 4]; cols * rows];
        for y in 0..h {
            for x in 0..w {
                let i = y * w + x;
                let k = weight(mask[i]);
                if k <= 0.0 {
                    continue;
                }
                let s = &mut sums[(y / cell) * cols + x / cell];
                for c in 0..3 {
                    s[c] += k * rgba[i * 4 + c] as f32;
                }
                s[3] += k;
            }
        }
        let total = sums.iter().fold([0.0f32; 4], |a, s| {
            [a[0] + s[0], a[1] + s[1], a[2] + s[2], a[3] + s[3]]
        });
        if total[3] < 1.0 {
            return None;
        }
        let need = (cell * cell) as f32;
        let mut cells = vec![[f32::NAN; 3]; cols * rows];
        for radius in [3usize, 12, 48] {
            let blurred = box_sum(&sums, cols, rows, radius);
            for (out, s) in cells.iter_mut().zip(&blurred) {
                if out[0].is_nan() && s[3] >= need {
                    *out = [s[0] / s[3], s[1] / s[3], s[2] / s[3]];
                }
            }
        }
        for out in cells.iter_mut().filter(|c| c[0].is_nan()) {
            *out = [
                total[0] / total[3],
                total[1] / total[3],
                total[2] / total[3],
            ];
        }
        Some(Self {
            cells,
            cols,
            rows,
            cell,
        })
    }

    /// Bilinear between cell centres.
    fn at(&self, x: usize, y: usize) -> [f32; 3] {
        let fx = ((x as f32 + 0.5) / self.cell as f32 - 0.5).clamp(0.0, (self.cols - 1) as f32);
        let fy = ((y as f32 + 0.5) / self.cell as f32 - 0.5).clamp(0.0, (self.rows - 1) as f32);
        let (x0, y0) = (fx as usize, fy as usize);
        let (x1, y1) = ((x0 + 1).min(self.cols - 1), (y0 + 1).min(self.rows - 1));
        let (tx, ty) = (fx - x0 as f32, fy - y0 as f32);
        let c = |cx: usize, cy: usize| self.cells[cy * self.cols + cx];
        let mut out = [0.0; 3];
        for (k, value) in out.iter_mut().enumerate() {
            let top = c(x0, y0)[k] * (1.0 - tx) + c(x1, y0)[k] * tx;
            let bottom = c(x0, y1)[k] * (1.0 - tx) + c(x1, y1)[k] * tx;
            *value = top * (1.0 - ty) + bottom * ty;
        }
        out
    }
}

/// Sum over the (2r+1)² window around each cell, clipped at the edges.
fn box_sum(src: &[[f32; 4]], cols: usize, rows: usize, r: usize) -> Vec<[f32; 4]> {
    let add = |a: [f32; 4], b: [f32; 4]| [a[0] + b[0], a[1] + b[1], a[2] + b[2], a[3] + b[3]];
    let mut across = vec![[0.0f32; 4]; cols * rows];
    for y in 0..rows {
        for x in 0..cols {
            let (x0, x1) = (x.saturating_sub(r), (x + r + 1).min(cols));
            across[y * cols + x] = src[y * cols + x0..y * cols + x1]
                .iter()
                .fold([0.0; 4], |a, &b| add(a, b));
        }
    }
    let mut out = vec![[0.0f32; 4]; cols * rows];
    for y in 0..rows {
        let (y0, y1) = (y.saturating_sub(r), (y + r + 1).min(rows));
        for x in 0..cols {
            out[y * cols + x] = (y0..y1).fold([0.0; 4], |a, yy| add(a, across[yy * cols + x]));
        }
    }
    out
}

/// Apply `plan` as one undo step: the person twice on top — "Ảnh gốc" (the
/// untouched photo behind a black mask, to paint details back in) under the
/// cut-out — then the crop (levelled, resampled to the print size, 600 ppi),
/// then the cut-out's mask pressed into its alpha like Ctrl+J with a
/// selection ("Layer 1", no mask), the background turned white and the older
/// layers hidden.
pub fn apply(canvas: &mut Canvas, plan: IdPhotoPlan) -> Result<(), String> {
    if plan.frame.is_none() && plan.cutout.is_none() {
        return Err("không có gì để làm".to_string());
    }
    canvas.begin_undo_group(UNDO_LABEL);
    let result = apply_steps(canvas, plan);
    canvas.end_undo_group();
    canvas.layer_revision += 1;
    result
}

fn apply_steps(canvas: &mut Canvas, plan: IdPhotoPlan) -> Result<(), String> {
    let added = plan.cutout.map(|cut| add_person_layers(canvas, cut));

    if let Some(frame) = plan.frame {
        let (out_w, out_h) = output_size();
        let cropped = canvas.crop_transformed_with_background(
            frame.centre[0],
            frame.centre[1],
            frame.width,
            frame.height,
            out_w,
            out_h,
            0.0,
            0.0,
            -frame.angle,
            true,
            [255, 255, 255, 255],
        );
        if !cropped {
            return Err("không cắt được ảnh".to_string());
        }
        canvas.metadata.resolution_ppi = PRINT_PPI;
    }

    if let Some(added) = added {
        finish_layers(canvas, added);
    }
    Ok(())
}

/// Multiply the layer's alpha by its mask and drop the mask; the colours
/// under transparent pixels stay, so later retouching blurs nothing dark in.
fn press_mask_into_alpha(layer: &mut crate::core::layer::Layer) {
    let Some(mask) = layer.mask.take() else {
        return;
    };
    let (w, h) = (layer.width, layer.height);
    let mut rgba = layer.tiles.flatten();
    let values = mask.tiles.flatten();
    if values.len() != rgba.len() || rgba.len() != w as usize * h as usize * 4 {
        layer.mask = Some(mask);
        return;
    }
    for (px, m) in rgba.chunks_exact_mut(4).zip(values.chunks_exact(4)) {
        px[3] = ((px[3] as u32 * m[0] as u32 + 127) / 255) as u8;
    }
    layer.tiles = TileMap::from_rgba(&rgba, w, h);
    layer.mask_active = false;
    layer.paint_target = crate::core::layer::PaintTarget::Pixels;
}

/// Ids of the "Ảnh gốc" and cut-out layers.
type PersonLayers = [u32; 2];

fn add_person_layers(canvas: &mut Canvas, cut: Cutout) -> PersonLayers {
    let mut cmd = LayerStructureCommand::capture_before(
        UNDO_LABEL,
        &canvas.layer_stack,
        canvas.width,
        canvas.height,
    );
    let stack = &mut canvas.layer_stack;
    for layer in &mut stack.layers {
        layer.selected = false;
    }
    stack.active_idx = stack.layers.len().saturating_sub(1);
    let (w, h) = (cut.region.w, cut.region.h);
    let mut add = |name: &str, rgba: &[u8], mask: TileMap| {
        // add_layer inserts above the active layer and makes it active.
        let idx = stack.add_layer(w, h);
        let layer = &mut stack.layers[idx];
        if !name.is_empty() {
            layer.name = name.to_string();
        }
        layer.parent_id = None;
        layer.tiles = TileMap::from_rgba(rgba, w, h);
        layer.offset = (cut.region.x as i32, cut.region.y as i32);
        layer.add_mask(true);
        if let Some(m) = &mut layer.mask {
            m.tiles = mask;
        }
        layer.id
    };
    let original = add(ORIGINAL_LAYER, &cut.original, TileMap::new_black(w, h));
    // Carried as a mask through the crop's resample, pressed into alpha after.
    let mask_rgba: Vec<u8> = cut.mask.iter().flat_map(|&m| [m, m, m, 255]).collect();
    let person = add("", &cut.rgba, TileMap::from_rgba(&mask_rgba, w, h));
    if let Some(layer) = stack.layers.get_mut(stack.active_idx) {
        layer.selected = true;
    }
    cmd.capture_after(&canvas.layer_stack, canvas.width, canvas.height);
    canvas.record(Box::new(cmd));
    [original, person]
}

/// Press the cut-out's mask into its alpha, fill the background layer (the
/// bottom one when none is marked) with white and hide every other layer
/// under the person: their pixels are in the person layers already.
fn finish_layers(canvas: &mut Canvas, added: PersonLayers) {
    let mut cmd = LayerStructureCommand::capture_before(
        BACKGROUND_STEP,
        &canvas.layer_stack,
        canvas.width,
        canvas.height,
    );
    let (w, h) = (canvas.width, canvas.height);
    let stack = &mut canvas.layer_stack;
    if let Some(cut) = stack.layers.iter_mut().find(|l| l.id == added[1]) {
        press_mask_into_alpha(cut);
    }
    let background = stack
        .layers
        .iter()
        .position(|l| l.is_background)
        .or_else(|| {
            stack
                .layers
                .first()
                .filter(|l| l.is_raster() && l.parent_id.is_none() && !added.contains(&l.id))
                .map(|_| 0)
        });
    for (i, layer) in stack.layers.iter_mut().enumerate() {
        if added.contains(&layer.id) {
            continue;
        }
        if Some(i) == background {
            layer.tiles = TileMap::new_white(w, h);
            (layer.width, layer.height, layer.offset) = (w, h, (0, 0));
            layer.mask = None;
            layer.mask_active = false;
            layer.visible = true;
        } else {
            layer.visible = false;
        }
    }
    if background.is_none() {
        let person_id = added[1];
        let id = stack.add_layer(w, h);
        let mut white = stack.layers.remove(id);
        white.name = "Background".to_string();
        white.parent_id = None;
        white.tiles = TileMap::new_white(w, h);
        white.selected = false;
        stack.layers.insert(0, white);
        if let Some(idx) = stack.layers.iter().position(|l| l.id == person_id) {
            stack.active_idx = idx;
            stack.layers[idx].selected = true;
        }
    }
    cmd.capture_after(&canvas.layer_stack, canvas.width, canvas.height);
    canvas.record(Box::new(cmd));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::layer::LayerMask;

    /// Landmarks measured on the owner's reference print (661×898).
    fn reference_face() -> FaceMarks {
        FaceMarks {
            eyes: [329.12, 301.64],
            chin: [327.42, 509.06],
            cheeks: [328.36, 335.09],
            nose: [329.32, 383.07],
            tilt: 0.0109,
            width: 262.8,
        }
    }

    /// The reference face moved by (dx, dy), clear of a bigger photo's edges.
    fn moved(face: FaceMarks, dx: f32, dy: f32) -> FaceMarks {
        let m = |p: [f32; 2]| [p[0] + dx, p[1] + dy];
        FaceMarks {
            eyes: m(face.eyes),
            chin: m(face.chin),
            cheeks: m(face.cheeks),
            nose: m(face.nose),
            ..face
        }
    }

    fn mask_value(mask: &LayerMask, x: u32, y: u32) -> u8 {
        (mask.sample(x, y) * 255.0).round() as u8
    }

    fn aspect() -> f32 {
        let (w, h) = output_size();
        w as f32 / h as f32
    }

    #[test]
    fn print_size_is_the_shops_3x4() {
        assert_eq!(output_size(), (661, 898));
    }

    #[test]
    fn reference_face_reproduces_the_reference_frame() {
        let frame = frame_for(&reference_face(), 0.0, true, aspect());
        assert_eq!(frame.angle, 0.0, "a 0.6° tilt is noise");
        assert!((frame.height - 898.0).abs() < 2.0, "{frame:?}");
        assert!((frame.width - 661.0).abs() < 2.0, "{frame:?}");
        let [x0, y0] = frame.corners()[0];
        assert!(x0.abs() < 3.0 && y0.abs() < 2.0, "{frame:?}");
    }

    #[test]
    fn widening_keeps_the_face_middle_in_place() {
        let face = reference_face();
        let tight = frame_for(&face, 0.0, false, aspect());
        let wide = frame_for(&face, 0.1, false, aspect());
        assert!((wide.height / tight.height - 1.1).abs() < 1e-4);
        let middle = [
            (face.eyes[0] + face.chin[0]) * 0.5,
            (face.eyes[1] + face.chin[1]) * 0.5,
        ];
        let a = tight.local(middle)[1] / tight.height;
        let b = wide.local(middle)[1] / wide.height;
        assert!((a - b).abs() < 1e-4, "{a} vs {b}");
    }

    #[test]
    fn a_tilted_face_is_framed_level() {
        let face = reference_face();
        let angle = 6f32.to_radians();
        let pivot = [400.0, 500.0];
        let turn = |p: [f32; 2]| {
            let (s, c) = angle.sin_cos();
            let d = [p[0] - pivot[0], p[1] - pivot[1]];
            [
                pivot[0] + c * d[0] - s * d[1],
                pivot[1] + s * d[0] + c * d[1],
            ]
        };
        let tilted = FaceMarks {
            eyes: turn(face.eyes),
            chin: turn(face.chin),
            cheeks: turn(face.cheeks),
            nose: turn(face.nose),
            tilt: face.tilt + angle,
            width: face.width,
        };
        let level = frame_for(&face, 0.1, false, aspect());
        let frame = frame_for(&tilted, 0.1, true, aspect());
        assert!((frame.angle - tilted.tilt).abs() < 1e-6);
        // The eyes land where the level face's do, give or take the reference
        // face's own 0.6° tilt.
        let a = level.local(face.eyes);
        let b = frame.local(tilted.eyes);
        assert!(
            (a[0] - b[0]).abs() < 4.0 && (a[1] - b[1]).abs() < 4.0,
            "{a:?} vs {b:?}"
        );
        assert!((frame.height - level.height).abs() < 0.5);
        // Without straightening the frame stays upright.
        assert_eq!(frame_for(&tilted, 0.1, false, aspect()).angle, 0.0);
    }

    #[test]
    fn frame_local_inverts_at() {
        let frame = Frame {
            centre: [100.0, 200.0],
            width: 60.0,
            height: 80.0,
            angle: 0.3,
        };
        for (u, v) in [(0.0, 0.0), (60.0, 0.0), (13.0, 71.0)] {
            let [x, y] = frame.at(u, v);
            let back = frame.local([x, y]);
            assert!((back[0] - u).abs() < 1e-3 && (back[1] - v).abs() < 1e-3);
        }
    }

    /// A synthetic photo: the reference face placed at `(dx, dy)` on a grey
    /// `w × h` canvas, with a person-shaped mask (head disc + shoulders).
    fn person_mask(face: &FaceMarks, w: u32, h: u32, head_top: f32) -> Vec<u8> {
        let mut mask = vec![0u8; (w * h) as usize];
        let centre = [face.eyes[0], (head_top + face.chin[1]) * 0.5];
        let radius = (face.chin[1] - head_top) * 0.5;
        for y in 0..h {
            for x in 0..w {
                let (fx, fy) = (x as f32 + 0.5, y as f32 + 0.5);
                let in_head = (fx - centre[0]).hypot(fy - centre[1]) <= radius;
                let in_body = fy >= face.chin[1] && (fx - centre[0]).abs() <= radius * 1.8;
                if in_head || in_body {
                    mask[(y * w + x) as usize] = 255;
                }
            }
        }
        mask
    }

    #[test]
    fn tall_hair_raises_the_frame() {
        let face = moved(reference_face(), 100.0, 200.0);
        let (w, h) = (900u32, 1400u32);
        let frame = frame_for(&face, 0.1, false, aspect());
        let region = Region { x: 0, y: 0, w, h };
        // Hair reaching 2% of the frame height below its top.
        let hair = frame.at(0.0, frame.height * 0.02)[1];
        let mask = person_mask(&face, w, h, hair);
        let top = head_top(&frame, &face, &mask, region).unwrap();
        assert!((top - frame.height * 0.02).abs() < 2.0, "{top}");
        // A normal head leaves room above it.
        let mask = person_mask(&face, w, h, frame.at(0.0, frame.height * 0.15)[1]);
        let top = head_top(&frame, &face, &mask, region).unwrap();
        assert!(top > MIN_HEAD_ROOM * frame.height);
    }

    #[test]
    fn widening_gives_way_when_the_photo_is_short() {
        let face = moved(reference_face(), 300.0, 0.0);
        let wide = frame_for(&face, 0.3, false, aspect());
        let reference = frame_for(&face, 0.0, false, aspect());
        let height = ((wide.corners()[2][1] + reference.corners()[2][1]) * 0.5) as u32;
        let options = IdPhotoOptions {
            widen: 0.3,
            straighten: false,
            ..Default::default()
        };
        let (frame, widen) = fit_frame(&face, &options, aspect(), 1000, height);
        assert!(widen > 0.0 && widen < 0.3, "{widen}");
        assert!(frame.bottom_inside(1000, height));
        // Too short even for the reference: keep the reference.
        let (frame, widen) = fit_frame(&face, &options, aspect(), 1000, 600);
        assert_eq!(widen, 0.0);
        assert_eq!(frame, reference);
    }

    #[test]
    fn keep_person_drops_a_separate_blob() {
        let (w, h) = (120u32, 80u32);
        let region = Region { x: 10, y: 20, w, h };
        let mut mask = vec![0u8; (w * h) as usize];
        for y in 10..70 {
            for x in 10..50 {
                mask[y * w as usize + x] = 255;
            }
            for x in 80..110 {
                mask[y * w as usize + x] = 255;
            }
        }
        // Soft rims next to each blob.
        mask[40 * w as usize + 50] = 90;
        mask[40 * w as usize + 79] = 90;
        keep_person(&mut mask, region, [10.0 + 30.0, 20.0 + 40.0]);
        assert_eq!(mask[40 * w as usize + 30], 255);
        assert_eq!(mask[40 * w as usize + 50], 90, "the person's own rim stays");
        assert_eq!(mask[40 * w as usize + 90], 0);
        assert_eq!(mask[40 * w as usize + 79], 0);
    }

    #[test]
    fn decontaminate_recovers_hair_colour_over_blue() {
        let (w, h) = (64u32, 64u32);
        let fg = [40.0f32, 20.0, 10.0];
        let bg = [30.0f32, 90.0, 220.0];
        let mut rgba = vec![0u8; (w * h * 4) as usize];
        let mut mask = vec![0u8; (w * h) as usize];
        for y in 0..h as usize {
            for x in 0..w as usize {
                let i = y * w as usize + x;
                let alpha = if x < 24 {
                    1.0
                } else if x < 40 {
                    1.0 - (x - 24) as f32 / 16.0
                } else {
                    0.0
                };
                mask[i] = (alpha * 255.0).round() as u8;
                for c in 0..3 {
                    rgba[i * 4 + c] = (alpha * fg[c] + (1.0 - alpha) * bg[c]).round() as u8;
                }
                rgba[i * 4 + 3] = 255;
            }
        }
        let before = rgba.clone();
        decontaminate(&mut rgba, &mask, w, h);
        // Laid on white, the rim shows the hair, not the blue it was shot on.
        let on_white = |px: &[u8], a: f32, c: usize| a * px[c] as f32 + (1.0 - a) * 255.0;
        for x in [26usize, 30, 34, 38] {
            let i = 32 * w as usize + x;
            let a = mask[i] as f32 / 255.0;
            for c in 0..3 {
                let ideal = a * fg[c] + (1.0 - a) * 255.0;
                let got = on_white(&rgba[i * 4..], a, c);
                assert!((got - ideal).abs() <= 15.0, "x {x} c {c}: {got} vs {ideal}");
            }
            if (28..36).contains(&x) {
                let raw = on_white(&before[i * 4..], a, 2);
                let ideal = a * fg[2] + (1.0 - a) * 255.0;
                assert!((raw - ideal).abs() > 30.0, "test needs a visible tint");
            }
        }
    }

    #[test]
    fn apply_stacks_person_over_the_original_on_white_in_one_undo() {
        let (w, h) = (1000u32, 1300u32);
        let mut pixels = vec![0u8; (w * h * 4) as usize];
        for px in pixels.chunks_exact_mut(4) {
            px.copy_from_slice(&[90, 120, 200, 255]);
        }
        let mut canvas = Canvas::from_rgba(pixels.clone(), w, h);
        // An earlier retouch layer: its pixels are in the person layers now.
        let extra = canvas.layer_stack.add_layer(w, h);
        canvas.layer_stack.layers[extra].tiles = TileMap::new_solid(w, h, 200, 10, 10, 255);
        canvas.layer_stack.layers[extra].name = "Chân dung".to_string();
        let face = reference_face();
        let frame = frame_for(&face, 0.1, false, aspect());
        let region = Region {
            x: 100,
            y: 50,
            w: 700,
            h: 1100,
        };
        let mut mask = vec![0u8; region.len()];
        for y in 300..1100usize {
            for x in 150..550usize {
                mask[y * 700 + x] = 255;
            }
        }
        let cut = copy_region(&pixels, w, region);
        let plan = IdPhotoPlan {
            frame: Some(frame),
            cutout: Some(Cutout {
                region,
                rgba: cut.clone(),
                mask,
                original: cut,
            }),
            notes: Vec::new(),
        };
        let layers_before = canvas.layer_stack.layers.len();
        apply(&mut canvas, plan).unwrap();
        assert_eq!((canvas.width, canvas.height), output_size());
        assert_eq!(canvas.metadata.resolution_ppi, PRINT_PPI);
        let layers = &canvas.layer_stack.layers;
        let names: Vec<&str> = layers.iter().map(|l| l.name.as_str()).collect();
        assert_eq!(names.len(), layers_before + 2, "{names:?}");
        assert_eq!(&names[names.len() - 2..], &[ORIGINAL_LAYER, "Layer 1"]);
        assert_eq!(canvas.layer_stack.active_idx, layers.len() - 1);
        // The background is white now and the old retouch layer hidden.
        assert!(layers[0].is_background && layers[0].visible);
        assert_eq!(layers[0].tiles.get_pixel(300, 400), (255, 255, 255, 255));
        assert!(!layers[1].visible);
        // "Ảnh gốc" waits behind a black mask; the cut-out is a plain layer
        // whose alpha holds the person only, like Ctrl+J with a selection.
        let original = &layers[layers.len() - 2];
        assert_eq!(mask_value(original.mask.as_ref().unwrap(), 330, 700), 0);
        let person = layers.last().unwrap();
        assert_eq!((person.width, person.height), output_size());
        assert!(person.mask.is_none());
        let scale = frame.height / output_size().1 as f32;
        let [u, v] = frame.local([120.0, 100.0]);
        let (ox, oy) = ((u / scale) as u32, (v / scale) as u32);
        let hidden = person.tiles.get_pixel(ox, oy);
        assert_eq!(hidden.3, 0, "at {ox},{oy}");
        assert_eq!(
            &[hidden.0, hidden.1, hidden.2],
            &[90, 120, 200],
            "colour kept"
        );
        assert_eq!(person.tiles.get_pixel(330, 700).3, 255);
        canvas.ensure_pixels();
        let at = |x: u32, y: u32| {
            let i = (y as usize * canvas.width as usize + x as usize) * 4;
            [canvas.pixels[i], canvas.pixels[i + 1], canvas.pixels[i + 2]]
        };
        assert_eq!(at(5, 5), [255, 255, 255]);
        assert_eq!(at(ox, oy), [255, 255, 255]);
        assert_eq!(at(330, 700), [90, 120, 200]);

        assert!(canvas.undo().is_some());
        assert_eq!((canvas.width, canvas.height), (w, h));
        let layers = &canvas.layer_stack.layers;
        assert_eq!(layers.len(), layers_before);
        assert!(layers[1].visible);
        assert_eq!(layers[0].tiles.get_pixel(300, 400), (90, 120, 200, 255));
    }

    /// Opt-in visual probe: set IAI_ID_PHOTO_PROBE to a folder of photos;
    /// each gets `idphoto_<name>.png` (the finished 661×898 print) beside it.
    #[test]
    #[ignore]
    fn probe_id_photos() {
        let Ok(dir) = std::env::var("IAI_ID_PHOTO_PROBE") else {
            return;
        };
        let widen: f32 = std::env::var("IAI_ID_PHOTO_WIDEN")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(DEFAULT_WIDEN);
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            let name = path.file_name().unwrap().to_string_lossy().to_string();
            let ext = path
                .extension()
                .map(|e| e.to_string_lossy().to_lowercase())
                .unwrap_or_default();
            if name.starts_with("idphoto_") || !["jpg", "jpeg", "png"].contains(&ext.as_str()) {
                continue;
            }
            let image = image::open(&path).unwrap().to_rgba8();
            let (w, h) = image.dimensions();
            let started = std::time::Instant::now();
            let options = IdPhotoOptions {
                widen,
                ..Default::default()
            };
            let mut segment = |px: &[u8], sw: u32, sh: u32| {
                super::super::select_subject::segment_blocking(
                    super::super::select_subject::SelectSubjectModel::BiRefNetTiny,
                    px,
                    sw,
                    sh,
                    false,
                )
            };
            let plan = match prepare(image.as_raw(), w, h, None, &options, &mut segment, &|_| {}) {
                Ok(plan) => plan,
                Err(e) => {
                    println!("{name}: {e}");
                    continue;
                }
            };
            let notes = plan.notes.join("; ");
            let frame = plan.frame;
            let mut canvas = Canvas::from_rgba(image.into_raw(), w, h);
            apply(&mut canvas, plan).unwrap();
            canvas.ensure_pixels();
            image::RgbaImage::from_raw(canvas.width, canvas.height, canvas.pixels.clone())
                .unwrap()
                .save(path.with_file_name(format!("idphoto_{name}.png")))
                .unwrap();
            println!(
                "{name}: {w}x{h} -> {}x{} in {} ms, frame {frame:?} {notes}",
                canvas.width,
                canvas.height,
                started.elapsed().as_millis()
            );
        }
    }
}
