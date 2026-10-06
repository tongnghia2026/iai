//! "Da cổ": the neck's skin given the detail a face restore model draws
//! there, and its tone and light evened.
//!
//! The model ("Chi tiết mặt (AI)") is run once more on a framing moved down
//! and widened until the neck is in view: it only draws well with the face
//! in sight. Of what it draws, only the detail on the neck's skin is taken:
//! below the jaw, inside the skin. The face keeps every pixel, so a face
//! whose detail was swapped before is not sharpened twice. Skin that was
//! laid flat when a garment was put on, or mended by hand with Clone or
//! Smudge, reads as skin again.

use rayon::prelude::*;

use super::ai_detail::{landmarks, Frame, EDGE_FADE};
use super::analysis::{smoothstep, FaceModel, PortraitModel, SkinLayers};
use super::geometry::{loop_points, signed_distance, Region, FACE_OVAL};
use crate::core::ai::retouch::{FaceRestorer, RestoredFace};

const SIDE: f32 = RestoredFace::SIDE as f32;
/// The neck's framing shows the face at this share of its usual size. Tried
/// on dressed ID photos: larger and the neck's lower half is out of view,
/// smaller and the model draws coarse, stubble-like grain.
const FACE_SHARE: f32 = 0.8;
/// The row of the usual square the neck's framing starts at: the forehead
/// stays in sight.
const TOP: f32 = 100.0;
/// The neck starts at the face outline and is full this far below it, in
/// face extents.
const JAW_FEATHER: f32 = 0.06;
/// Rows of the usual square between which what lies beside the face starts
/// to count: from the mouth's line down, below the ears.
const BESIDE_FROM: (f32, f32) = (350.0, 390.0);
/// Beside a garment laid over the person the neck is all of the skin this
/// far from the garment, in face extents.
const BESIDE_GARMENT: f32 = 0.05;
/// The most the model's detail may differ from the photo's, 0..1 of white:
/// pores and fine lines pass, a crease or a glint it draws along a seam of
/// mended skin is held back.
const SWING: f32 = 0.05;

/// The neck of one face: how far each pixel of its skin region lies where
/// the neck is, and the model's detail there.
pub struct NeckDetail {
    region: Region,
    area: Vec<u8>,
    frame: Frame,
}

/// [`NeckDetail`] at one photo pixel: how far it is neck skin (0..1), the
/// model's RGB detail and the photo's own that it replaces.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct NeckAt {
    pub weight: f32,
    pub model: [f32; 3],
    pub photo: [f32; 3],
}

impl NeckAt {
    /// What the neck's detail adds to a pixel still holding `kept` of the
    /// photo's own: the model's in its place, no further from it than
    /// [`SWING`].
    pub(super) fn swap(&self, kept: f32) -> [f32; 3] {
        let add: [f32; 3] = std::array::from_fn(|k| self.model[k] - kept * self.photo[k]);
        let size = add.iter().fold(0.0f32, |size, v| size.max(v.abs()));
        if size <= 1e-6 {
            return add;
        }
        let held = SWING * (size / SWING).tanh() / size;
        add.map(|v| v * held)
    }
}

impl NeckDetail {
    /// How far pixel `i` of the face's skin region is neck skin as found:
    /// where the neck lies, as deep inside the skin as `inside` (0..255)
    /// says, so that what the model makes of a collar's or the hair's edge
    /// stays off it.
    pub(super) fn found(&self, i: usize, inside: u8) -> f32 {
        self.area
            .get(i)
            .map_or(0.0, |&area| area as f32 * inside as f32 / (255.0 * 255.0))
    }

    /// The neck's skin as found over the region of `skin`, for the brush to
    /// paint on from.
    pub fn mask(&self, skin: &SkinLayers) -> Vec<u8> {
        if skin.region != self.region {
            return vec![0; skin.region.len()];
        }
        (0..self.area.len())
            .into_par_iter()
            .map(|i| (self.found(i, skin.interior[i]) * 255.0).round() as u8)
            .collect()
    }

    /// [`mask`](Self::mask) for a person under a garment laid over them, its
    /// alpha over the skin's region being `cover`. Beside the garment and
    /// under it the neck is all of the skin, not only what lies deep inside
    /// it: the skin's own edge is out of sight there, and what shows of the
    /// neck runs to the garment's edge.
    pub fn mask_beside(&self, skin: &SkinLayers, cover: &[u8]) -> Vec<u8> {
        let mut mask = self.mask(skin);
        if skin.region != self.region || cover.len() != mask.len() {
            return mask;
        }
        let reach = (BESIDE_GARMENT * skin.extent).max(2.0);
        let beside = beside_garment(&self.area, &skin.mask, cover, self.region.w as usize, reach);
        for (found, beside) in mask.iter_mut().zip(beside) {
            *found = (*found).max(beside);
        }
        mask
    }

    /// The neck at image pixel (x, y), which is neck skin by `weight`
    /// (0..1). Past what the model saw there is no detail to swap.
    pub(super) fn at(&self, x: u32, y: u32, weight: f32) -> NeckAt {
        let (model, photo) = self
            .frame
            .at(x, y, None)
            .map_or(([0.0; 3], [0.0; 3]), |s| (s.model, s.photo));
        NeckAt {
            weight,
            model,
            photo,
        }
    }
}

/// How far each pixel is neck for lying beside a garment or under it: where
/// the neck is (`area`), all of the `skin` no farther than about `reach`
/// pixels from what the garment covers (`cover`, its alpha), each a plane
/// `width` across.
fn beside_garment(area: &[u8], skin: &[u8], cover: &[u8], width: usize, reach: f32) -> Vec<u8> {
    let plane: Vec<f32> = cover.iter().map(|&a| a as f32 / 255.0).collect();
    let near = crate::core::seam::soft(&plane, width, cover.len() / width.max(1), reach);
    (0..cover.len())
        .into_par_iter()
        .map(|i| {
            let beside = smoothstep(0.02, 0.25, near[i]);
            (area[i] as f32 * skin[i] as f32 / 255.0 * beside).round() as u8
        })
        .collect()
}

/// The neck's framing for a face whose usual one is `close`: the face at
/// `share` of its size there, the view starting at row `top` of the usual
/// square.
fn framing(close: [f32; 4], share: f32, top: f32) -> [f32; 4] {
    let [a, b, tx, ty] = close;
    let mid = SIDE * 0.5;
    [
        share * a,
        share * b,
        share * (tx - mid) + mid,
        share * (ty - top),
    ]
}

/// How much of photo point (x, y) the square of `framing` covers: 1 well
/// inside it, fading to 0 at its edge as the detail does.
fn cover(framing: [f32; 4], x: f32, y: f32) -> f32 {
    let [a, b, tx, ty] = framing;
    let (u, v) = (a * x - b * y + tx, b * x + a * y + ty);
    let last = SIDE - 1.0;
    (u.min(v).min(last - u).min(last - v) / EDGE_FADE).clamp(0.0, 1.0)
}

/// How far each pixel of `region` lies where the neck is: outside the face
/// `outline`, below the jaw and in view of the neck's `framing`. `close` is
/// the face's usual framing and `extent` its size. What of it is skin, and
/// how deep inside the skin, is the skin layers' to say.
fn neck_area(
    region: Region,
    outline: &[[f32; 2]],
    extent: f32,
    close: [f32; 4],
    framing: [f32; 4],
) -> Vec<u8> {
    let w = region.w as usize;
    let feather = (JAW_FEATHER * extent).max(1.0);
    let [a, b, _, ty] = close;
    (0..region.len())
        .into_par_iter()
        .map(|i| {
            let x = region.x as f32 + (i % w) as f32 + 0.5;
            let y = region.y as f32 + (i / w) as f32 + 0.5;
            let seen = cover(framing, x, y);
            let below = smoothstep(BESIDE_FROM.0, BESIDE_FROM.1, b * x + a * y + ty);
            if below <= 0.0 || seen <= 0.0 {
                return 0;
            }
            let outside = smoothstep(0.0, feather, -signed_distance(outline, x, y));
            (outside * below * seen * 255.0).round() as u8
        })
        .collect()
}

fn neck_detail(
    restorer: &mut FaceRestorer,
    rgba: &[u8],
    width: u32,
    height: u32,
    face: &FaceModel,
    share: f32,
    top: f32,
) -> Result<NeckDetail, String> {
    let close = FaceRestorer::framing(&landmarks(face))?;
    let framing = framing(close, share, top);
    let region = face.skin.region;
    let outline = loop_points(&face.mesh.points, &FACE_OVAL);
    let area = neck_area(region, &outline, face.extent, close, framing);
    let restored = restorer.restore_framed(rgba, width, height, framing)?;
    Ok(NeckDetail {
        region,
        area,
        frame: Frame::new(&restored),
    })
}

/// Run the model on the neck of each face of `model` that is `wanted` and
/// has none yet (a couple of seconds a face on the CPU) and keep the result
/// on the face.
pub fn analyze_necks(rgba: &[u8], model: &PortraitModel, wanted: &[bool]) {
    let faces: Vec<&FaceModel> = model
        .faces
        .iter()
        .zip(wanted.iter().chain(std::iter::repeat(&true)))
        .filter(|(face, &on)| on && face.neck.get().is_none())
        .map(|(face, _)| face)
        .collect();
    if faces.is_empty() {
        return;
    }
    let mut restorer = match FaceRestorer::load() {
        Ok(restorer) => restorer,
        Err(error) => {
            for face in faces {
                let _ = face.neck.set(Err(error.clone()));
            }
            return;
        }
    };
    for face in faces {
        let neck = neck_detail(
            &mut restorer,
            rgba,
            model.width,
            model.height,
            face,
            FACE_SHARE,
            TOP,
        );
        let _ = face.neck.set(neck);
    }
}

#[cfg(test)]
mod tests {
    use super::super::effects::{render, PortraitSettings};
    use super::*;

    #[test]
    fn the_necks_framing_shows_the_face_smaller_and_starts_lower() {
        // The usual square is the photo itself.
        let close = [1.0, 0.0, 0.0, 0.0];
        let f = framing(close, 0.8, 100.0);
        let to = |x: f32, y: f32| (f[0] * x - f[1] * y + f[2], f[1] * x + f[0] * y + f[3]);
        // Row 100 of the usual square is the top, its middle column stays.
        let top = to(256.0, 100.0);
        assert!(
            (top.0 - 256.0).abs() < 1e-4 && top.1.abs() < 1e-4,
            "{top:?}"
        );
        // 640 rows of the usual square are in view: well below the chin.
        let bottom = to(256.0, 740.0);
        assert!((bottom.1 - 512.0).abs() < 1e-3, "{bottom:?}");
        // Covered in full well inside, not at all past the edge.
        assert_eq!(cover(f, 256.0, 400.0), 1.0);
        assert_eq!(cover(f, 256.0, 90.0), 0.0);
        assert!(cover(f, 256.0, 115.0) > 0.0 && cover(f, 256.0, 115.0) < 1.0);
    }

    #[test]
    fn the_neck_lies_below_the_jaw_and_never_inside_the_face() {
        // A face as the usual square frames it, the photo being that square:
        // an oval from the forehead (row 130) to the chin (row 460).
        let outline: Vec<[f32; 2]> = (0..36)
            .map(|k| {
                let t = k as f32 / 36.0 * std::f32::consts::TAU;
                [256.0 + 125.0 * t.cos(), 295.0 + 165.0 * t.sin()]
            })
            .collect();
        let region = Region {
            x: 0,
            y: 0,
            w: 512,
            h: 900,
        };
        let close = [1.0, 0.0, 0.0, 0.0];
        let area = neck_area(region, &outline, 330.0, close, framing(close, 0.8, 100.0));
        let at = |x: u32, y: u32| area[region.index_at(x, y).unwrap()];
        // Not a pixel inside the outline, whatever its row.
        for (i, &level) in area.iter().enumerate() {
            let (x, y) = ((i % 512) as f32 + 0.5, (i / 512) as f32 + 0.5);
            if signed_distance(&outline, x, y) >= 0.0 {
                assert_eq!(level, 0, "inside the face at {x}, {y}");
            }
        }
        assert_eq!(at(256, 455), 0, "the chin");
        assert_eq!(at(256, 520), 255, "under the chin");
        assert_eq!(at(256, 650), 255, "down the neck");
        assert_eq!(at(100, 420), 255, "beside the jaw");
        assert_eq!(at(100, 300), 0, "an ear");
        assert_eq!(at(256, 60), 0, "over the forehead");
        // The area fades out where the model's view ends.
        assert!(at(256, 725) > 0 && at(256, 725) < 255, "{}", at(256, 725));
        assert_eq!(at(256, 760), 0, "below the view");
    }

    #[test]
    fn beside_a_garment_and_under_it_the_neck_is_all_of_the_skin() {
        // A neck everywhere, skin in the left 60 columns of 100, a garment
        // over the rows from 50 down.
        let (w, h) = (100usize, 100usize);
        let area = vec![255u8; w * h];
        let skin: Vec<u8> = (0..w * h)
            .map(|i| if i % w < 60 { 255 } else { 0 })
            .collect();
        let cover: Vec<u8> = (0..w * h)
            .map(|i| if i / w >= 50 { 255 } else { 0 })
            .collect();
        let beside = beside_garment(&area, &skin, &cover, w, 5.0);
        let at = |x: usize, y: usize| beside[y * w + x];
        assert_eq!(at(30, 48), 255, "skin at the garment's edge");
        assert_eq!(at(30, 70), 255, "skin under the garment");
        assert_eq!(at(30, 20), 0, "skin far from the garment");
        assert_eq!(at(80, 48), 0, "no skin, at the garment's edge");
        // It fades out over the reach.
        assert!(at(30, 42) > 0 && at(30, 42) < 255, "{}", at(30, 42));
    }

    #[test]
    fn the_models_detail_is_held_near_the_photos() {
        let at = |model: f32, photo: f32| NeckAt {
            weight: 1.0,
            model: [model, model * 0.5, 0.0],
            photo: [photo, 0.0, 0.0],
        };
        // Pores pass nearly whole.
        let pores = at(0.02, 0.0).swap(1.0);
        assert!((pores[0] - 0.02).abs() < 0.002, "{pores:?}");
        // The photo's own detail goes as far as the pixel still holds it.
        let swapped = at(0.02, 0.02).swap(1.0);
        assert!(swapped[0].abs() < 1e-6, "{swapped:?}");
        let half = at(0.02, 0.02).swap(0.5);
        assert!((half[0] - 0.01).abs() < 0.001, "{half:?}");
        // A glint is held at the limit, its colour unchanged.
        let glint = at(0.3, 0.0).swap(1.0);
        assert!(glint[0] <= SWING && glint[0] > 0.9 * SWING, "{glint:?}");
        assert!((glint[1] - 0.5 * glint[0]).abs() < 1e-6);
        assert_eq!(at(0.0, 0.0).swap(1.0), [0.0; 3]);
    }

    /// Opt-in visual probe: IAI_PORTRAIT_NECK_PROBE is a folder of photos
    /// (a dressed photo's person layer among them). Each gets
    /// `neck_<name>.png`, from the mouth down: as shot | "Da cổ" 50 | 100 |
    /// where it acts, tinted green. IAI_NECK_SHOWN (`share:top` pairs, `;`
    /// between them) adds the model's detail laid whole on the neck for
    /// other framings.
    #[test]
    #[ignore]
    fn probe_neck() {
        let Ok(dir) = std::env::var("IAI_PORTRAIT_NECK_PROBE") else {
            return;
        };
        let ways: Vec<(f32, f32)> = std::env::var("IAI_NECK_SHOWN")
            .unwrap_or_default()
            .split(';')
            .filter_map(|way| {
                let (share, top) = way.split_once(':')?;
                Some((share.trim().parse().ok()?, top.trim().parse().ok()?))
            })
            .collect();
        let dir = std::path::PathBuf::from(dir);
        let mut restorer = FaceRestorer::load().unwrap();
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            let name = path.file_name().unwrap().to_string_lossy().to_string();
            let skip = name.starts_with("neck_")
                || name.starts_with("xong_")
                || name.starts_with('_')
                || name.contains(".ao.")
                || name.contains(".nguoi.");
            if skip || !(name.ends_with(".jpg") || name.ends_with(".png")) {
                continue;
            }
            let rgba = image::open(&path).unwrap().to_rgba8();
            let (w, h) = rgba.dimensions();
            let rgba = rgba.into_raw();
            let model = match super::super::analyze(&rgba, w, h, false, None, &|_| {}) {
                Ok(model) => model,
                Err(error) => {
                    println!("{name}: {error}");
                    continue;
                }
            };
            let face = &model.faces[0];
            let enabled = vec![true; model.faces.len()];
            let skin = &face.skin;
            let r = skin.region;
            let chin = face.mesh.points.iter().map(|p| p[1]).fold(0.0, f32::max);
            let y0 = (chin - 0.25 * face.extent).max(r.y as f32) as u32;
            let y1 = ((chin + 0.9 * face.extent) as u32).min(r.y + r.h).min(h);
            let view = Region {
                x: r.x,
                y: y0,
                w: r.w,
                h: y1.saturating_sub(y0),
            };
            if view.is_empty() {
                println!("{name}: no neck in view");
                continue;
            }
            let rendered = |settings: &PortraitSettings| {
                let mut out = rgba.clone();
                if let Some((u, px)) = render(&rgba, &model, settings, &enabled, &[]) {
                    for y in 0..u.h as usize {
                        let o = ((u.y as usize + y) * w as usize + u.x as usize) * 4;
                        out[o..o + u.w as usize * 4]
                            .copy_from_slice(&px[y * u.w as usize * 4..(y + 1) * u.w as usize * 4]);
                    }
                }
                out
            };
            let with = |neck: f32| PortraitSettings {
                neck,
                ..PortraitSettings::NEUTRAL
            };
            // Before the model's layer exists the slider changes nothing.
            assert!(
                rendered(&with(100.0)) == rgba,
                "{name}: neck before analysis"
            );
            let started = std::time::Instant::now();
            analyze_necks(&rgba, &model, &enabled);
            let seconds = started.elapsed().as_secs_f32();
            let neck = match face.neck.get() {
                Some(Ok(neck)) => neck,
                other => {
                    println!("{name}: {:?}", other.and_then(|n| n.as_ref().err()));
                    continue;
                }
            };
            let weight = |neck: &NeckDetail, i: usize| neck.found(i, skin.interior[i]);
            let mut tinted = rgba.clone();
            let mut covered = 0usize;
            for i in 0..r.len() {
                let (x, y) = (
                    r.x + (i % r.w as usize) as u32,
                    r.y + (i / r.w as usize) as u32,
                );
                let o = ((y * w + x) * 4) as usize;
                let weight = weight(neck, i);
                covered += usize::from(weight > 0.5);
                tinted[o] = (tinted[o] as f32 * (1.0 - 0.6 * weight)) as u8;
                tinted[o + 2] = (tinted[o + 2] as f32 * (1.0 - 0.6 * weight)) as u8;
            }
            let half = rendered(&with(50.0));
            let full = rendered(&with(100.0));
            // The face keeps every pixel.
            let outline = loop_points(&face.mesh.points, &FACE_OVAL);
            let mut face_changed = 0usize;
            for y in face.region.y..face.region.y + face.region.h {
                for x in face.region.x..face.region.x + face.region.w {
                    let o = ((y * w + x) * 4) as usize;
                    let inside = signed_distance(&outline, x as f32 + 0.5, y as f32 + 0.5) >= 0.0;
                    face_changed += usize::from(inside && full[o..o + 3] != rgba[o..o + 3]);
                }
            }
            println!(
                "{name}: {covered} px of neck, {face_changed} px of the face changed, {seconds:.1} s"
            );
            // A flat photo with its garment's and its person's alpha beside
            // it (`<name>.ao.png`, `<name>.nguoi.png`): as it is finished
            // once set right by hand, whole, `xong_<name>.jpg`.
            let plane = |of: &str| {
                let path = dir.join(format!("{name}.{of}.png"));
                let plane = image::open(path).ok()?.to_luma8().into_raw();
                (plane.len() == (w * h) as usize).then_some(plane)
            };
            if let (Some(garment), Some(person)) = (plane("ao"), plane("nguoi")) {
                let cover: Vec<u8> = (r.y..r.y + r.h)
                    .flat_map(|y| (r.x..r.x + r.w).map(move |x| (x, y)))
                    .map(|(x, y)| garment[(y * w + x) as usize])
                    .collect();
                let edits = vec![super::super::effects::FaceEdits {
                    neck: Some(std::sync::Arc::new(neck.mask_beside(skin, &cover))),
                    ..Default::default()
                }];
                let mut out = rgba.clone();
                if let Some((u, px)) = render(&rgba, &model, &with(60.0), &enabled, &edits) {
                    for y in 0..u.h as usize {
                        let o = ((u.y as usize + y) * w as usize + u.x as usize) * 4;
                        out[o..o + u.w as usize * 4]
                            .copy_from_slice(&px[y * u.w as usize * 4..(y + 1) * u.w as usize * 4]);
                    }
                }
                let alone = |alpha: &[u8]| -> Vec<u8> {
                    alpha.iter().flat_map(|&a| [0, 0, 0, a]).collect()
                };
                let shade = crate::core::seam::shade(&alone(&person), &alone(&garment), None, w, h);
                if let Some(shade) = shade {
                    for (px, shade) in out.chunks_exact_mut(4).zip(shade.chunks_exact(4)) {
                        let a = shade[3] as f32 / 255.0;
                        for c in 0..3 {
                            let left = 1.0 - a * (1.0 - shade[c] as f32 / 255.0);
                            px[c] = (px[c] as f32 * left).round() as u8;
                        }
                    }
                }
                let mut pair = image::RgbImage::new(w * 2 + 12, h);
                for (k, pixels) in [&rgba, &out].into_iter().enumerate() {
                    for y in 0..h {
                        for x in 0..w {
                            let o = ((y * w + x) * 4) as usize;
                            pair.put_pixel(
                                k as u32 * (w + 12) + x,
                                y,
                                image::Rgb([pixels[o], pixels[o + 1], pixels[o + 2]]),
                            );
                        }
                    }
                }
                pair.save(dir.join(format!("xong_{name}.jpg"))).unwrap();
            }
            let mut views = vec![rgba.clone(), half, full, tinted];
            for &(share, top) in &ways {
                let other = neck_detail(&mut restorer, &rgba, w, h, face, share, top).unwrap();
                let mut out = rgba.clone();
                for i in 0..r.len() {
                    let (x, y) = (
                        r.x + (i % r.w as usize) as u32,
                        r.y + (i / r.w as usize) as u32,
                    );
                    let weight = weight(&other, i);
                    if weight <= 0.0 {
                        continue;
                    }
                    let at = other.at(x, y, weight);
                    let o = ((y * w + x) * 4) as usize;
                    for c in 0..3 {
                        let v = out[o + c] as f32 / 255.0 + weight * (at.model[c] - at.photo[c]);
                        out[o + c] = (v * 255.0).round().clamp(0.0, 255.0) as u8;
                    }
                }
                views.push(out);
            }
            let zoom = (360.0 / view.w as f32).ceil().max(1.0) as u32;
            let cell = view.w * zoom + 8;
            let mut sheet = image::RgbaImage::new(cell * views.len() as u32, view.h * zoom);
            for (k, pixels) in views.iter().enumerate() {
                for y in 0..view.h * zoom {
                    for x in 0..view.w * zoom {
                        let o = (((view.y + y / zoom) * w + view.x + x / zoom) * 4) as usize;
                        sheet.put_pixel(
                            k as u32 * cell + x,
                            y,
                            image::Rgba([pixels[o], pixels[o + 1], pixels[o + 2], 255]),
                        );
                    }
                }
            }
            sheet.save(dir.join(format!("neck_{name}.png"))).unwrap();
        }
    }
}
