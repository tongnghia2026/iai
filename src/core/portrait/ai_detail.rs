//! "Chi tiết mặt (AI)": the fine detail a face restore model (GFPGAN) draws on
//! a face, kept as a layer the slider swaps in for the photo's own. Only the
//! detail is taken: colour, light and the face's shape stay the photo's, so
//! a soft or noisy phone photo gains clean detail without becoming the
//! model's idea of the person.

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

/// The detail of one face, over the model's square (the model works at that
/// size, so there is no more to keep however large the face is in the photo).
pub struct FaceDetail {
    /// Photo pixel (x, y) lies at (a·x − b·y + tx, b·x + a·y + ty) in the
    /// square.
    to_square: [f32; 4],
    /// Samples averaged per photo pixel along each axis: more than one when
    /// a photo pixel covers several of the square's.
    taps: usize,
    /// The model's RGB detail and the photo's own, which it replaces.
    model: Vec<[f32; 3]>,
    photo: Vec<[f32; 3]>,
    /// How far inside the face outline each pixel lies: there the detail is
    /// swapped whatever the pixel is (eyes, brows, lips, glasses, beard);
    /// outside it only on skin.
    face: Vec<f32>,
}

/// [`FaceDetail`] at one photo pixel.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct DetailAt {
    pub model: [f32; 3],
    pub photo: [f32; 3],
    pub face: f32,
}

impl FaceDetail {
    pub(super) fn at(&self, x: u32, y: u32) -> Option<DetailAt> {
        let [a, b, tx, ty] = self.to_square;
        let last = (SIDE - 1) as f32;
        let n = self.taps;
        let mut sum = DetailAt {
            model: [0.0; 3],
            photo: [0.0; 3],
            face: 0.0,
        };
        let mut seen = false;
        for row in 0..n {
            for col in 0..n {
                let px = x as f32 + (col as f32 + 0.5) / n as f32 - 0.5;
                let py = y as f32 + (row as f32 + 0.5) / n as f32 - 0.5;
                let (u, v) = (a * px - b * py + tx, b * px + a * py + ty);
                if u < 0.0 || v < 0.0 || u > last || v > last {
                    continue;
                }
                seen = true;
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
                        sum.model[c] += self.model[i][c] * weight;
                        sum.photo[c] += self.photo[i][c] * weight;
                    }
                    sum.face += self.face[i] * weight;
                }
            }
        }
        // Samples off the square count as no detail.
        let norm = 1.0 / (n * n) as f32;
        seen.then(|| DetailAt {
            model: sum.model.map(|v| v * norm),
            photo: sum.photo.map(|v| v * norm),
            face: sum.face * norm,
        })
    }
}

/// Run the model on each face of `model` that is `wanted` and has no detail
/// yet (one run per face, a few seconds each on the CPU) and keep the result
/// on the face.
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
    let to_square = restored.square_from_photo();
    let [a, b, tx, ty] = to_square;
    let scale = restored.scale();

    // The face outline, in the square.
    let outline: Vec<[f32; 2]> = loop_points(&face.mesh.points, &FACE_OVAL)
        .into_iter()
        .map(|[x, y]| [a * x - b * y + tx, b * x + a * y + ty])
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
        OUTLINE_FEATHER * face.extent * scale,
    );
    Ok(FaceDetail {
        to_square,
        taps: (scale.ceil() as usize).clamp(1, 4),
        model: square_detail(&restored.restored),
        photo: square_detail(&restored.source),
        face: inside,
    })
}

/// The RGB detail of a [`SIDE`]-square image: what a [`DETAIL_SIGMA`] blur
/// removes, faded out at the square's edge.
fn square_detail(rgb: &[[f32; 3]]) -> Vec<[f32; 3]> {
    let mut low: Vec<[f32; 4]> = rgb.par_iter().map(|c| [c[0], c[1], c[2], 0.0]).collect();
    blur4(&mut low, SIDE, SIDE, DETAIL_SIGMA);
    let last = SIDE - 1;
    rgb.par_iter()
        .zip(low.par_iter())
        .enumerate()
        .map(|(i, (c, l))| {
            let (u, v) = (i % SIDE, i / SIDE);
            let edge = u.min(v).min(last - u).min(last - v) as f32;
            let fade = (edge / EDGE_FADE).min(1.0);
            [c[0] - l[0], c[1] - l[1], c[2] - l[2]].map(|d| d * fade)
        })
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

    #[test]
    fn detail_is_the_fine_pattern_and_nothing_where_the_square_is_flat() {
        let detail = square_detail(&checker());
        let at = |u: usize, v: usize| detail[v * SIDE + u];
        // The checker survives the split nearly whole, in its own channel.
        assert!(at(128, 256)[0].abs() > 0.08, "{:?}", at(128, 256));
        assert!(at(128, 256)[1].abs() < 1e-6);
        assert!(at(400, 256)[0].abs() < 0.005, "{:?}", at(400, 256));
        // It fades out at the square's edge.
        assert_eq!(at(128, 0), [0.0; 3]);
        assert!(at(128, 12)[0].abs() < 0.6 * at(128, 256)[0].abs());
    }

    #[test]
    fn detail_is_read_at_photo_pixels_through_the_squares_frame() {
        let model = square_detail(&checker());
        // A face twice as large in the photo as in the square, the square's
        // corner at photo pixel (100, 40).
        let large = FaceDetail {
            to_square: [0.5, 0.0, -50.0, -20.0],
            taps: 1,
            model: model.clone(),
            photo: vec![[0.0; 3]; SIDE * SIDE],
            face: vec![1.0; SIDE * SIDE],
        };
        let here = large.at(100 + 2 * 128, 40 + 2 * 256).unwrap();
        assert_eq!(here.model, model[256 * SIDE + 128]);
        assert_eq!((here.photo, here.face), ([0.0; 3], 1.0));
        assert_eq!(large.at(99, 300), None, "left of the square");
        assert_eq!(large.at(100 + 2 * 512, 300), None, "right of it");

        // A face a quarter the size: each photo pixel averages the 4x4
        // square pixels it covers, where the checker cancels out.
        let small = FaceDetail {
            to_square: [4.0, 0.0, 0.0, 0.0],
            taps: 4,
            ..large
        };
        let mean = small.at(32, 64).unwrap();
        assert!(mean.model[0].abs() < 0.01, "{:?}", mean.model);
    }
}
