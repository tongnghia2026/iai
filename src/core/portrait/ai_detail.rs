//! "Chi tiết mặt (AI)": the fine detail a face restore model (GFPGAN) draws on
//! a face and its hair, kept as a layer the slider swaps in for the photo's
//! own. Only the detail is taken: colour, light and the face's shape stay
//! the photo's, so a soft or noisy phone photo gains clean detail without
//! becoming the model's idea of the person.

use rayon::prelude::*;

use super::analysis::{FaceModel, PortraitModel};
use super::blur::blur4;
use super::geometry::{loop_points, stamp_polygon, Region, FACE_OVAL, LEFT_EYE, RIGHT_EYE};
use crate::core::ai::retouch::{FaceRestorer, RestoredFace};

const SIDE: usize = RestoredFace::SIDE;
/// Detail is what a blur this wide removes, in pixels of the model's square.
const DETAIL_SIGMA: f32 = 8.0;
/// The detail fades out over this many pixels at the square's edge.
const EDGE_FADE: f32 = 24.0;
/// The soft edge of the face outline, in face extents.
const OUTLINE_FEATHER: f32 = 0.06;
const NOSE_TIP: usize = 1;
const MOUTH_CORNERS: [usize; 2] = [61, 291];
/// A second, wider run is worth its seconds once this share of the hair
/// lies past the model's own framing.
const HAIR_OUTSIDE: f32 = 0.03;
/// The wider framing shows the face no smaller than this, of its usual size
/// (the model still draws hair well down to here).
const WIDEST: f32 = 0.6;
/// The middle of the usual square, which holds the face: the wider framing
/// keeps it in view.
const FACE_CORE: (f32, f32) = (128.0, 384.0);

/// One run of the model: the detail it drew over its square, and the photo's
/// own there.
struct Frame {
    /// Photo pixel (x, y) lies at (a·x − b·y + tx, b·x + a·y + ty) in the
    /// square.
    to_square: [f32; 4],
    /// Samples averaged per photo pixel along each axis: more than one when
    /// a photo pixel covers several of the square's.
    taps: usize,
    model: Vec<[f32; 3]>,
    photo: Vec<[f32; 3]>,
}

/// A [`Frame`] read at one photo pixel: the two details, already faded
/// toward the square's edge, how much of the pixel the frame covers (0..1)
/// and the pixel's share of an extra map over the square.
struct Sample {
    model: [f32; 3],
    photo: [f32; 3],
    cover: f32,
    extra: f32,
}

impl Frame {
    fn new(restored: &RestoredFace) -> Self {
        Self {
            to_square: restored.square_from_photo(),
            taps: (restored.scale().ceil() as usize).clamp(1, 4),
            model: square_detail(&restored.restored),
            photo: square_detail(&restored.source),
        }
    }

    fn to_square(&self, x: f32, y: f32) -> (f32, f32) {
        let [a, b, tx, ty] = self.to_square;
        (a * x - b * y + tx, b * x + a * y + ty)
    }

    fn at(&self, x: u32, y: u32, extra: Option<&[f32]>) -> Option<Sample> {
        let last = (SIDE - 1) as f32;
        let n = self.taps;
        let mut sum = Sample {
            model: [0.0; 3],
            photo: [0.0; 3],
            cover: 0.0,
            extra: 0.0,
        };
        let mut seen = false;
        for row in 0..n {
            for col in 0..n {
                let px = x as f32 + (col as f32 + 0.5) / n as f32 - 0.5;
                let py = y as f32 + (row as f32 + 0.5) / n as f32 - 0.5;
                let (u, v) = self.to_square(px, py);
                if u < 0.0 || v < 0.0 || u > last || v > last {
                    continue;
                }
                seen = true;
                let fade = (u.min(v).min(last - u).min(last - v) / EDGE_FADE).min(1.0);
                let (u0, v0) = (u as usize, v as usize);
                let (u1, v1) = ((u0 + 1).min(SIDE - 1), (v0 + 1).min(SIDE - 1));
                let (fu, fv) = (u - u0 as f32, v - v0 as f32);
                let corners = [
                    (v0 * SIDE + u0, (1.0 - fu) * (1.0 - fv)),
                    (v0 * SIDE + u1, fu * (1.0 - fv)),
                    (v1 * SIDE + u0, (1.0 - fu) * fv),
                    (v1 * SIDE + u1, fu * fv),
                ];
                for (i, weight) in corners {
                    for c in 0..3 {
                        sum.model[c] += self.model[i][c] * weight * fade;
                        sum.photo[c] += self.photo[i][c] * weight * fade;
                    }
                    if let Some(extra) = extra {
                        sum.extra += extra[i] * weight;
                    }
                }
                sum.cover += fade;
            }
        }
        // Samples off the square count as no detail.
        let norm = 1.0 / (n * n) as f32;
        seen.then(|| Sample {
            model: sum.model.map(|v| v * norm),
            photo: sum.photo.map(|v| v * norm),
            cover: sum.cover * norm,
            extra: sum.extra * norm,
        })
    }
}

/// The detail of one face. The model works at the size of its square, so
/// there is no more to keep however large the face is in the photo.
pub struct FaceDetail {
    /// The framing the model was trained on: the face, and the hair near it.
    close: Frame,
    /// How far inside the face outline each pixel of that square lies:
    /// there the detail is swapped whatever the pixel is (eyes, brows, lips,
    /// glasses, beard); outside it only on skin and hair.
    face: Vec<f32>,
    /// A wider framing, run when hair reaches past the close one.
    wide: Option<Frame>,
}

/// [`FaceDetail`] at one photo pixel: the model's RGB detail, the photo's
/// own that it replaces, and how far inside the face outline the pixel lies.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct DetailAt {
    pub model: [f32; 3],
    pub photo: [f32; 3],
    pub face: f32,
}

impl FaceDetail {
    /// The detail for the face and its skin.
    pub(super) fn at(&self, x: u32, y: u32) -> Option<DetailAt> {
        let s = self.close.at(x, y, Some(&self.face))?;
        Some(DetailAt {
            model: s.model,
            photo: s.photo,
            face: s.extra,
        })
    }

    /// The detail for hair: the close framing where it reaches, the wide
    /// one past it.
    pub(super) fn hair_at(&self, x: u32, y: u32) -> Option<DetailAt> {
        let near = self.close.at(x, y, Some(&self.face));
        let far = self.wide.as_ref().and_then(|wide| wide.at(x, y, None));
        if near.is_none() && far.is_none() {
            return None;
        }
        let mut out = DetailAt {
            model: [0.0; 3],
            photo: [0.0; 3],
            face: 0.0,
        };
        let mut covered = 0.0;
        if let Some(near) = near {
            (out.model, out.photo, out.face) = (near.model, near.photo, near.extra);
            covered = near.cover;
        }
        if let Some(far) = far {
            let rest = 1.0 - covered;
            for c in 0..3 {
                out.model[c] += far.model[c] * rest;
                out.photo[c] += far.photo[c] * rest;
            }
        }
        Some(out)
    }
}

/// Run the model on each face of `model` that is `wanted` and has no detail
/// yet (a few seconds a face on the CPU, twice that when its hair needs the
/// wider framing) and keep the result on the face.
pub fn analyze_details(rgba: &[u8], model: &PortraitModel, wanted: &[bool]) {
    let faces: Vec<&FaceModel> = model
        .faces
        .iter()
        .zip(wanted.iter().chain(std::iter::repeat(&true)))
        .filter(|(face, &on)| on && face.ai_detail.get().is_none())
        .map(|(face, _)| face)
        .collect();
    if faces.is_empty() {
        return;
    }
    let mut restorer = match FaceRestorer::load() {
        Ok(restorer) => restorer,
        Err(error) => {
            for face in faces {
                let _ = face.ai_detail.set(Err(error.clone()));
            }
            return;
        }
    };
    for face in faces {
        let detail = face_detail(&mut restorer, rgba, model.width, model.height, face);
        let _ = face.ai_detail.set(detail);
    }
}

/// The five points the model aligns a face by: eye centres, nose tip and
/// mouth corners, the image's left one first.
fn landmarks(face: &FaceModel) -> [[f32; 2]; 5] {
    let points = &face.mesh.points;
    let at = |k: usize| [points[k][0], points[k][1]];
    let centre = |ring: &[u16]| {
        let sum = ring.iter().fold([0.0f32; 2], |sum, &k| {
            let p = at(k as usize);
            [sum[0] + p[0], sum[1] + p[1]]
        });
        sum.map(|v| v / ring.len() as f32)
    };
    [
        centre(&RIGHT_EYE),
        centre(&LEFT_EYE),
        at(NOSE_TIP),
        at(MOUTH_CORNERS[0]),
        at(MOUTH_CORNERS[1]),
    ]
}

fn face_detail(
    restorer: &mut FaceRestorer,
    rgba: &[u8],
    width: u32,
    height: u32,
    face: &FaceModel,
) -> Result<FaceDetail, String> {
    let restored = restorer.restore(rgba, width, height, &landmarks(face))?;
    let close = Frame::new(&restored);

    // The face outline, in the square.
    let outline: Vec<[f32; 2]> = loop_points(&face.mesh.points, &FACE_OVAL)
        .into_iter()
        .map(|[x, y]| {
            let (u, v) = close.to_square(x, y);
            [u, v]
        })
        .collect();
    let square = Region {
        x: 0,
        y: 0,
        w: SIDE as u32,
        h: SIDE as u32,
    };
    let mut inside = vec![0.0f32; SIDE * SIDE];
    stamp_polygon(
        &mut inside,
        square,
        &outline,
        0.0,
        OUTLINE_FEATHER * face.extent * restored.scale(),
    );

    // Hair past the close framing: one more run on a wider one. Its failing
    // only leaves that hair as it is.
    let wide = hair_bounds(&close, face)
        .and_then(|bounds| wider_framing(close.to_square, bounds))
        .and_then(|framing| restorer.restore_framed(rgba, width, height, framing).ok())
        .map(|restored| Frame::new(&restored));
    Ok(FaceDetail {
        close,
        face: inside,
        wide,
    })
}

/// Where the face's hair lies in the close square, as [u0, v0, u1, v1],
/// when enough of it lies past the part the close framing covers in full.
fn hair_bounds(close: &Frame, face: &FaceModel) -> Option<[f32; 4]> {
    let (r, hair) = (face.hair_region, face.hair_mask());
    if r.is_empty() || hair.len() != r.len() {
        return None;
    }
    let step = ((r.len() as f32 / 20_000.0).sqrt().ceil() as usize).max(1);
    let (lo, hi) = (EDGE_FADE, (SIDE - 1) as f32 - EDGE_FADE);
    let (mut total, mut outside) = (0usize, 0usize);
    let mut bounds = [f32::MAX, f32::MAX, f32::MIN, f32::MIN];
    for row in (0..r.h as usize).step_by(step) {
        for col in (0..r.w as usize).step_by(step) {
            if hair[row * r.w as usize + col] < 128 {
                continue;
            }
            let (u, v) = close.to_square((r.x as usize + col) as f32, (r.y as usize + row) as f32);
            total += 1;
            if u < lo || v < lo || u > hi || v > hi {
                outside += 1;
            }
            bounds = [
                bounds[0].min(u),
                bounds[1].min(v),
                bounds[2].max(u),
                bounds[3].max(v),
            ];
        }
    }
    (total > 0 && outside as f32 > HAIR_OUTSIDE * total as f32).then_some(bounds)
}

/// A framing that takes in `bounds` (of the close square, given by
/// `close`), as far as [`WIDEST`] allows, with the face still in view.
fn wider_framing(close: [f32; 4], bounds: [f32; 4]) -> Option<[f32; 4]> {
    let (core_lo, core_hi) = FACE_CORE;
    let [u0, v0, u1, v1] = [
        bounds[0].min(core_lo),
        bounds[1].min(core_lo),
        bounds[2].max(core_hi),
        bounds[3].max(core_hi),
    ];
    let room = SIDE as f32 - 2.0 * EDGE_FADE;
    let scale = (room / (u1 - u0).max(v1 - v0)).clamp(WIDEST, 1.0);
    // What the wider square shows of the close one, half its side.
    let half = SIDE as f32 * 0.5 / scale;
    let centre = |lo: f32, hi: f32| ((lo + hi) * 0.5).clamp(core_hi - half, core_lo + half);
    let (cu, cv) = (centre(u0, u1), centre(v0, v1));
    let mid = SIDE as f32 * 0.5;
    if scale > 0.98 && (cu - mid).abs() < 4.0 && (cv - mid).abs() < 4.0 {
        return None;
    }
    let [a, b, tx, ty] = close;
    Some([
        scale * a,
        scale * b,
        scale * (tx - cu) + mid,
        scale * (ty - cv) + mid,
    ])
}

/// The RGB detail of a [`SIDE`]-square image: what a [`DETAIL_SIGMA`] blur
/// removes.
fn square_detail(rgb: &[[f32; 3]]) -> Vec<[f32; 3]> {
    let mut low: Vec<[f32; 4]> = rgb.par_iter().map(|c| [c[0], c[1], c[2], 0.0]).collect();
    blur4(&mut low, SIDE, SIDE, DETAIL_SIGMA);
    rgb.par_iter()
        .zip(low.par_iter())
        .map(|(c, l)| [c[0] - l[0], c[1] - l[1], c[2] - l[2]])
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A square whose left half is a fine red checker, the right half flat.
    fn checker() -> Vec<[f32; 3]> {
        (0..SIDE * SIDE)
            .map(|i| {
                let (u, v) = (i % SIDE, i / SIDE);
                let wave = if (u / 2 + v / 2) % 2 == 0 { 0.1 } else { -0.1 };
                [0.5 + if u < SIDE / 2 { wave } else { 0.0 }, 0.5, 0.5]
            })
            .collect()
    }

    fn frame(to_square: [f32; 4], taps: usize, model: Vec<[f32; 3]>) -> Frame {
        Frame {
            to_square,
            taps,
            model,
            photo: vec![[0.0; 3]; SIDE * SIDE],
        }
    }

    #[test]
    fn detail_is_the_fine_pattern_and_nothing_where_the_square_is_flat() {
        let detail = square_detail(&checker());
        let at = |u: usize, v: usize| detail[v * SIDE + u];
        // The checker survives the split nearly whole, in its own channel.
        assert!(at(128, 256)[0].abs() > 0.08, "{:?}", at(128, 256));
        assert!(at(128, 256)[1].abs() < 1e-6);
        assert!(at(400, 256)[0].abs() < 0.005, "{:?}", at(400, 256));
    }

    #[test]
    fn detail_is_read_at_photo_pixels_through_the_squares_frame() {
        let model = square_detail(&checker());
        // A face twice as large in the photo as in the square, the square's
        // corner at photo pixel (100, 40).
        let large = FaceDetail {
            close: frame([0.5, 0.0, -50.0, -20.0], 1, model.clone()),
            face: vec![1.0; SIDE * SIDE],
            wide: None,
        };
        let here = large.at(100 + 2 * 128, 40 + 2 * 256).unwrap();
        assert_eq!(here.model, model[256 * SIDE + 128]);
        assert_eq!((here.photo, here.face), ([0.0; 3], 1.0));
        assert_eq!(large.at(99, 300), None, "left of the square");
        assert_eq!(large.at(100 + 2 * 512, 300), None, "right of it");
        // The detail fades out at the square's edge.
        let edge = large.at(100 + 2 * 128, 40).unwrap();
        assert_eq!(edge.model, [0.0; 3]);
        let near = large.at(100 + 2 * 128, 40 + 2 * 12).unwrap();
        assert!((near.model[0] - 0.5 * model[12 * SIDE + 128][0]).abs() < 1e-6);

        // A face a quarter the size: each photo pixel averages the 4x4
        // square pixels it covers, where the checker cancels out.
        let small = FaceDetail {
            close: frame([4.0, 0.0, 0.0, 0.0], 4, model),
            ..large
        };
        let mean = small.at(32, 64).unwrap();
        assert!(mean.model[0].abs() < 0.01, "{:?}", mean.model);
    }

    #[test]
    fn hair_past_the_close_framing_reads_the_wide_one() {
        let flat = |v: f32| vec![[v, 0.0, 0.0]; SIDE * SIDE];
        // The close square is photo pixels 0..512; the wide one shows twice
        // as much around the same centre.
        let detail = FaceDetail {
            close: frame([1.0, 0.0, 0.0, 0.0], 1, flat(0.2)),
            face: vec![0.0; SIDE * SIDE],
            wide: Some(frame([0.5, 0.0, 128.0, 128.0], 1, flat(0.1))),
        };
        // Well inside the close square: all its own.
        assert_eq!(detail.hair_at(256, 256).unwrap().model[0], 0.2);
        // Halfway through its edge fade: half each.
        let mixed = detail.hair_at(256, 12).unwrap().model[0];
        assert!((mixed - 0.15).abs() < 1e-6, "{mixed}");
        // Above it: the wide one's alone, which the face's own detail
        // (`at`) does not reach.
        assert_eq!(detail.hair_at(256, 0).unwrap().model[0], 0.1);
        assert!((detail.hair_at(256, 600).unwrap().model[0] - 0.1).abs() < 1e-6);
        assert_eq!(detail.at(256, 600), None);
        assert_eq!(detail.hair_at(256, 2000), None);
    }

    #[test]
    fn the_wider_framing_takes_in_tall_hair_and_keeps_the_face() {
        let close = [1.0, 0.0, 0.0, 0.0];
        let to =
            |f: [f32; 4], x: f32, y: f32| (f[0] * x - f[1] * y + f[2], f[1] * x + f[0] * y + f[3]);
        // Hair reaching 100 px above the close square.
        let wide = wider_framing(close, [100.0, -100.0, 412.0, 300.0]).unwrap();
        let top = to(wide, 256.0, -100.0);
        assert!(top.1 >= EDGE_FADE - 0.5, "hair top inside: {top:?}");
        for corner in [(128.0, 128.0), (384.0, 384.0)] {
            let (u, v) = to(wide, corner.0, corner.1);
            assert!((0.0..=511.0).contains(&u) && (0.0..=511.0).contains(&v));
        }
        // Hair far longer than the model can take: as wide as allowed.
        let long = wider_framing(close, [0.0, 0.0, 512.0, 1500.0]).unwrap();
        assert!((long[0] - WIDEST).abs() < 1e-6);
        let core = to(long, 256.0, 128.0);
        assert!(core.1 >= 0.0, "face still in view: {core:?}");
        // Nothing to gain over the close framing: no second run.
        assert_eq!(wider_framing(close, [150.0, 140.0, 360.0, 370.0]), None);
    }
}
