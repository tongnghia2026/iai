//! Where a garment laid on a photo meets its wearer: the shade each casts on
//! the other, as a layer of its own to lie over the garment and under the
//! hair that falls over it, multiplied into what is under it.
//!
//! Neither the garment nor the person is changed, and the shade follows their
//! outlines only, not their colours: the layer is simply made again when
//! either has moved or changed shape.

use rayon::prelude::*;

use super::garment::Garment;

/// The neck's width in eye-to-chin lengths: a collar's opening, as wide as
/// the neck it was fitted on, gives the photo's scale by it.
const NECK_SPAN: f32 = 0.83;
/// The eye-to-chin length of a photo whose garment shows no collar, as a
/// share of its height: that of an ID photo.
const PLAIN_UNIT: f32 = 0.23;

/// How wide each shade is, in eye-to-chin lengths, and how dark at its
/// darkest: the line where the garment touches the skin, the soft shade
/// around it, the person's on the garment beside them, the chin's and
/// neck's thrown down on it, and the hair's on it.
const TOUCH: (f32, f32) = (0.012, 0.40);
const AROUND: (f32, f32) = (0.07, 0.34);
const BESIDE: (f32, f32) = (0.05, 0.20);
const THROWN: (f32, f32) = (0.09, 0.14);
const HAIR: (f32, f32) = (0.04, 0.40);
/// How far down the thrown shade falls, in eye-to-chin lengths.
const DROP: f32 = 0.06;
/// What full shade leaves of a colour, the layer's own colour there: skin
/// keeps more of its red.
const ON_SKIN: [f32; 3] = [0.55, 0.36, 0.30];
const ON_CLOTH: [f32; 3] = [0.42, 0.43, 0.47];
/// No pixel is shaded more than this.
const DEEPEST: f32 = 0.9;
/// Shades are smooth: past this width of the touch line, in pixels, they are
/// worked out on every second pixel.
const COARSE_FROM: f32 = 3.0;

/// A plane averaged over `step × step` blocks.
fn shrunk(plane: &[f32], width: usize, height: usize, step: usize) -> (Vec<f32>, usize, usize) {
    if step <= 1 {
        return (plane.to_vec(), width, height);
    }
    let (sw, sh) = (width.div_ceil(step), height.div_ceil(step));
    let mut out = vec![0.0f32; sw * sh];
    out.par_chunks_mut(sw).enumerate().for_each(|(sy, row)| {
        for (sx, cell) in row.iter_mut().enumerate() {
            let (mut sum, mut count) = (0.0, 0usize);
            for y in sy * step..((sy + 1) * step).min(height) {
                for x in sx * step..((sx + 1) * step).min(width) {
                    sum += plane[y * width + x];
                    count += 1;
                }
            }
            *cell = sum / count.max(1) as f32;
        }
    });
    (out, sw, sh)
}

/// Each row of `plane` averaged over `2 * radius + 1` pixels, three times
/// over: close to a Gaussian.
fn soften_rows(plane: &mut [f32], width: usize, radius: usize) {
    if radius == 0 || width == 0 {
        return;
    }
    plane.par_chunks_mut(width).for_each(|row| {
        let mut line = vec![0.0f32; width];
        for _ in 0..3 {
            let (mut sum, mut count) = (0.0f32, 0usize);
            for value in row.iter().take(radius.min(width)) {
                sum += value;
                count += 1;
            }
            for (x, out) in line.iter_mut().enumerate() {
                if x + radius < width {
                    sum += row[x + radius];
                    count += 1;
                }
                if x > radius {
                    sum -= row[x - radius - 1];
                    count -= 1;
                }
                *out = sum / count as f32;
            }
            row.copy_from_slice(&line);
        }
    });
}

fn turned(plane: &[f32], width: usize, height: usize) -> Vec<f32> {
    let mut out = vec![0.0f32; plane.len()];
    out.par_chunks_mut(height).enumerate().for_each(|(x, col)| {
        for (y, value) in col.iter_mut().enumerate() {
            *value = plane[y * width + x];
        }
    });
    out
}

/// A plane blurred about as a Gaussian of deviation `sigma` would.
fn soft(plane: &[f32], width: usize, height: usize, sigma: f32) -> Vec<f32> {
    // Three passes of a box `2r + 1` wide have a variance of `r (r + 1)`.
    let radius = (((4.0 * sigma * sigma + 1.0).sqrt() - 1.0) * 0.5).round() as usize;
    let mut rows = plane.to_vec();
    soften_rows(&mut rows, width, radius);
    let mut columns = turned(&rows, width, height);
    soften_rows(&mut columns, height, radius);
    turned(&columns, height, width)
}

/// A plane read between its cells; its rim runs on outside it.
fn read(plane: &[f32], width: usize, height: usize, x: f32, y: f32) -> f32 {
    let (fx, fy) = (
        x.clamp(0.0, width as f32 - 1.0),
        y.clamp(0.0, height as f32 - 1.0),
    );
    let (x0, y0) = (fx as usize, fy as usize);
    let (x1, y1) = ((x0 + 1).min(width - 1), (y0 + 1).min(height - 1));
    let (tx, ty) = (fx - x0 as f32, fy - y0 as f32);
    let upper = plane[y0 * width + x0] * (1.0 - tx) + plane[y0 * width + x1] * tx;
    let lower = plane[y1 * width + x0] * (1.0 - tx) + plane[y1 * width + x1] * tx;
    upper * (1.0 - ty) + lower * ty
}

/// The shade between a garment and its wearer, the photo's straight-alpha
/// RGBA, to be multiplied into the photo. `person` is what shows of them
/// under the garment, `garment` the garment as it lies and `hair` the hair
/// over it, each such a picture too. `None` when nothing is shaded.
///
/// The widths follow the garment's collar as it lies now, so the same layers
/// always give the same shade.
pub fn shade(
    person: &[u8],
    garment: &[u8],
    hair: Option<&[u8]>,
    width: u32,
    height: u32,
) -> Option<Vec<u8>> {
    if garment.len() != width as usize * height as usize * 4 {
        return None;
    }
    let worn = Garment {
        rgba: garment.to_vec(),
        width,
        height,
    };
    let unit = match worn.collar() {
        Some(collar) => (collar.right[0] - collar.left[0]) / NECK_SPAN,
        None => height as f32 * PLAIN_UNIT,
    };
    shade_at(person, garment, hair, width, height, unit)
}

/// `shade` for a face whose eye-to-chin length is `unit`.
fn shade_at(
    person: &[u8],
    garment: &[u8],
    hair: Option<&[u8]>,
    width: u32,
    height: u32,
    unit: f32,
) -> Option<Vec<u8>> {
    let (w, h) = (width as usize, height as usize);
    let size = w * h * 4;
    if w == 0 || h == 0 || person.len() != size || garment.len() != size {
        return None;
    }
    let hair = hair.filter(|hair| hair.len() == size);
    let unit = unit.max(8.0);
    let alpha = |rgba: &[u8]| -> Vec<f32> {
        rgba.par_chunks_exact(4)
            .map(|px| px[3] as f32 / 255.0)
            .collect()
    };
    let cloth = alpha(garment);
    let bare: Vec<f32> = person
        .par_chunks_exact(4)
        .zip(&cloth)
        .map(|(px, cloth)| px[3] as f32 / 255.0 * (1.0 - cloth))
        .collect();
    if !cloth.iter().any(|&a| a > 0.0) || !bare.iter().any(|&a| a > 0.0) {
        return None;
    }

    let step = if unit * TOUCH.0 >= COARSE_FROM { 2 } else { 1 };
    let (cloth_small, sw, sh) = shrunk(&cloth, w, h, step);
    let (bare_small, ..) = shrunk(&bare, w, h, step);
    let field = |plane: &[f32], sigma: f32| soft(plane, sw, sh, sigma * unit / step as f32);
    let touch = field(&cloth_small, TOUCH.0);
    let around = field(&cloth_small, AROUND.0);
    let beside = field(&bare_small, BESIDE.0);
    let thrown = field(&bare_small, THROWN.0);
    let strands = hair.map(|hair| field(&shrunk(&alpha(hair), w, h, step).0, HAIR.0));
    let at = |plane: &[f32], x: usize, y: usize, down: f32| {
        read(
            plane,
            sw,
            sh,
            (x as f32 + 0.5) / step as f32 - 0.5,
            (y as f32 + 0.5 - down) / step as f32 - 0.5,
        )
    };

    let mut layer = vec![0u8; size];
    layer
        .par_chunks_mut(w * 4)
        .enumerate()
        .for_each(|(y, row)| {
            for x in 0..w {
                let i = y * w + x;
                let (skin, cloth) = (bare[i], cloth[i]);
                let shown = skin + cloth;
                if shown <= 0.0 {
                    continue;
                }
                let full = |value: f32, gain: f32| (value * gain).clamp(0.0, 1.0);
                let on_skin = skin
                    * (TOUCH.1 * full(at(&touch, x, y, 0.0), 2.0).powf(1.5)
                        + AROUND.1 * full(at(&around, x, y, 0.0), 2.0).powi(2));
                let mut on_cloth = BESIDE.1 * full(at(&beside, x, y, 0.0), 2.0).powi(2)
                    + THROWN.1 * full(at(&thrown, x, y, DROP * unit), 1.5);
                if let (Some(strands), Some(hair)) = (&strands, hair) {
                    let over = hair[i * 4 + 3] as f32 / 255.0;
                    on_cloth += HAIR.1 * full(at(strands, x, y, 0.0), 1.6) * (1.0 - over);
                }
                on_cloth *= cloth;
                let depth = (on_skin + on_cloth).min(DEEPEST);
                let a = (depth * shown.min(1.0) * 255.0).round();
                if a < 1.0 {
                    continue;
                }
                let share = on_skin / (on_skin + on_cloth);
                for c in 0..3 {
                    let left = ON_SKIN[c] * share + ON_CLOTH[c] * (1.0 - share);
                    row[x * 4 + c] = (left * 255.0).round() as u8;
                }
                row[x * 4 + 3] = a as u8;
            }
        });

    layer.chunks_exact(4).any(|px| px[3] > 0).then_some(layer)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SKIN: [u8; 4] = [220, 170, 140, 255];
    const SHIRT: [u8; 4] = [240, 240, 240, 255];
    const UNIT: f32 = 100.0;

    /// A neck 80 wide down a 200 × 200 photo, and a shirt over its lower
    /// half with an opening 60 wide and 50 deep.
    fn dressed() -> (Vec<u8>, Vec<u8>) {
        let (w, h) = (200usize, 200usize);
        let mut person = vec![0u8; w * h * 4];
        let mut garment = vec![0u8; w * h * 4];
        for y in 0..h {
            for x in 0..w {
                let o = (y * w + x) * 4;
                if (60..140).contains(&x) {
                    person[o..o + 4].copy_from_slice(&SKIN);
                }
                let open = (70..130).contains(&x) && y < 150;
                if y >= 100 && !open {
                    garment[o..o + 4].copy_from_slice(&SHIRT);
                }
            }
        }
        (person, garment)
    }

    fn px(layer: &[u8], x: usize, y: usize) -> [u8; 4] {
        let o = (y * ((layer.len() / 4) as f64).sqrt() as usize + x) * 4;
        [layer[o], layer[o + 1], layer[o + 2], layer[o + 3]]
    }

    #[test]
    fn the_garment_shades_the_skin_beside_its_edge_and_none_far_from_it() {
        let (person, garment) = dressed();
        let piece = shade_at(&person, &garment, None, 200, 200, UNIT).unwrap();
        // Skin in the opening: darkest against the garment's edge, lighter
        // toward the middle of the opening.
        let (edge, inner) = (px(&piece, 70, 130), px(&piece, 80, 130));
        assert!(edge[3] > inner[3] && inner[3] > 0, "{edge:?} {inner:?}");
        assert!(edge[3] > 100, "{edge:?}");
        // Multiplied in, it leaves the skin more of its red than its blue.
        assert!(edge[0] > edge[2] + 40, "{edge:?}");
        // The neck well over the garment keeps its light.
        assert_eq!(px(&piece, 100, 40)[3], 0);
        // Nothing is laid on the backdrop.
        assert_eq!(px(&piece, 30, 80)[3], 0);
        assert_eq!(px(&piece, 150, 60)[3], 0);
    }

    #[test]
    fn the_person_shades_the_garment_beside_them_and_none_far_from_them() {
        let (person, garment) = dressed();
        let piece = shade_at(&person, &garment, None, 200, 200, UNIT).unwrap();
        // The garment under the opening takes the neck's shade, in grey.
        let under = px(&piece, 100, 153);
        assert!(under[3] > 20, "{under:?}");
        assert!(under[0].abs_diff(under[2]) < 20, "{under:?}");
        // The shoulder far from the neck takes none.
        assert_eq!(px(&piece, 5, 190)[3], 0);
    }

    #[test]
    fn hair_over_the_garment_shades_it_beside_itself() {
        let (person, garment) = dressed();
        let mut hair = vec![0u8; 200 * 200 * 4];
        for y in 100..200 {
            for x in 10..30 {
                hair[(y * 200 + x) * 4..(y * 200 + x) * 4 + 4].copy_from_slice(&[20, 20, 20, 255]);
            }
        }
        let plain = shade_at(&person, &garment, None, 200, 200, UNIT).unwrap();
        let with = shade_at(&person, &garment, Some(&hair), 200, 200, UNIT).unwrap();
        assert_eq!(px(&plain, 32, 180)[3], 0);
        assert!(px(&with, 32, 180)[3] > 40, "{:?}", px(&with, 32, 180));
        assert_eq!(px(&with, 50, 180)[3], 0);
    }

    #[test]
    fn a_photo_with_no_garment_on_it_has_no_shade() {
        let (person, garment) = dressed();
        assert!(shade(&person, &vec![0u8; garment.len()], None, 200, 200).is_none());
        assert!(shade(&vec![0u8; person.len()], &garment, None, 200, 200).is_none());
    }

    #[test]
    fn a_large_photo_is_shaded_as_a_small_one_is() {
        // Twice the size works on every second pixel: the shade is the same.
        let (person, garment) = dressed();
        let double = |rgba: &[u8]| {
            let mut out = vec![0u8; 400 * 400 * 4];
            for y in 0..400 {
                for x in 0..400 {
                    let (from, to) = (((y / 2) * 200 + x / 2) * 4, (y * 400 + x) * 4);
                    out[to..to + 4].copy_from_slice(&rgba[from..from + 4]);
                }
            }
            out
        };
        // 300 is past COARSE_FROM, 150 is not.
        let small = shade_at(&person, &garment, None, 200, 200, 150.0).unwrap();
        let large = shade_at(&double(&person), &double(&garment), None, 400, 400, 300.0).unwrap();
        for (x, y) in [(72, 130), (80, 130), (100, 153), (90, 110)] {
            let (a, b) = (px(&small, x, y)[3], px(&large, x * 2, y * 2)[3]);
            assert!(a.abs_diff(b) <= 12, "({x}, {y}): {a} and {b}");
        }
    }
}
