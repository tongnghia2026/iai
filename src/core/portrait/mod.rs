//! Portrait retouching in the style of dedicated portrait tools: the face mesh
//! places the features, the Sapiens2 part model (when installed) outlines skin,
//! teeth and hair, and classic frequency-split processing does the retouch so
//! real skin texture survives.

pub mod analysis;
mod blur;
pub mod effects;
pub mod geometry;

pub use analysis::{analyze, FaceModel, PortraitModel, TRUSTED_AGREEMENT};
pub use effects::{render, PortraitSettings};
pub use geometry::Region;

#[cfg(test)]
mod tests {
    use super::*;

    fn crop_rgb(rgba: &[u8], width: u32, r: Region) -> image::RgbImage {
        image::RgbImage::from_fn(r.w, r.h, |x, y| {
            let o = (((r.y + y) * width + r.x + x) * 4) as usize;
            image::Rgb([rgba[o], rgba[o + 1], rgba[o + 2]])
        })
    }

    /// Opt-in: set IAI_PORTRAIT_PROBE to a folder of photos; writes before/after
    /// and mask sheets per face and prints timings.
    #[test]
    #[ignore]
    fn probe_portrait() {
        let Ok(dir) = std::env::var("IAI_PORTRAIT_PROBE") else {
            return;
        };
        let dir = std::path::PathBuf::from(dir);
        let mut names: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().to_string())
            .filter(|n| n.ends_with(".jpg") && !n.starts_with("pr_"))
            .collect();
        names.sort();
        for name in names {
            let image = image::open(dir.join(&name)).unwrap().to_rgba8();
            let (width, height) = image.dimensions();
            let rgba = image.into_raw();
            let started = std::time::Instant::now();
            let model = analyze(&rgba, width, height, false, &|_| {}).unwrap();
            let analyse_ms = started.elapsed().as_millis();
            let enabled = vec![true; model.faces.len()];
            let started = std::time::Instant::now();
            let (union, pixels) =
                render(&rgba, &model, &PortraitSettings::default(), &enabled).unwrap();
            let render_ms = started.elapsed().as_millis();
            println!(
                "{name}: {width}x{height}, {} face(s), analyse {analyse_ms} ms {:?}, render {render_ms} ms, union {}x{}, parts={} {:?}",
                model.faces.len(),
                model.timings,
                union.w,
                union.h,
                model.parts_used,
                model.parts_note
            );
            let strong = PortraitSettings {
                lip_saturation: 60.0,
                lip_hue: 50.0,
                lip_brightness: -20.0,
                brows: 60.0,
                sharpen: 60.0,
                ..PortraitSettings::NEUTRAL
            };
            let (su, sp) = render(&rgba, &model, &strong, &enabled).unwrap();
            let mut styled = rgba.clone();
            for row in 0..su.h as usize {
                let o = ((su.y as usize + row) * width as usize + su.x as usize) * 4;
                let s = row * su.w as usize * 4;
                styled[o..o + su.w as usize * 4].copy_from_slice(&sp[s..s + su.w as usize * 4]);
            }
            let r0 = model.faces[0].region;
            let mut sheet = image::RgbImage::new(r0.w * 2 + 8, r0.h);
            image::imageops::replace(&mut sheet, &crop_rgb(&rgba, width, r0), 0, 0);
            image::imageops::replace(
                &mut sheet,
                &crop_rgb(&styled, width, r0),
                (r0.w + 8) as i64,
                0,
            );
            let k = (1800.0 / sheet.width() as f32).min(1.0);
            image::imageops::resize(
                &sheet,
                (sheet.width() as f32 * k) as u32,
                (sheet.height() as f32 * k) as u32,
                image::imageops::FilterType::Triangle,
            )
            .save(dir.join(format!("pr_{name}_styled.jpg")))
            .unwrap();
            let mut after = rgba.clone();
            for row in 0..union.h as usize {
                let o = ((union.y as usize + row) * width as usize + union.x as usize) * 4;
                let s = row * union.w as usize * 4;
                after[o..o + union.w as usize * 4]
                    .copy_from_slice(&pixels[s..s + union.w as usize * 4]);
            }
            for (i, face) in model.faces.iter().enumerate() {
                let r = face.region;
                println!(
                    "  face {i}: region {}x{} extent {:.0} agreement {:?}",
                    r.w, r.h, face.extent, face.agreement
                );
                let mut scores: Vec<u8> = face
                    .spot_score
                    .iter()
                    .zip(&face.interior)
                    .filter(|(_, &m)| m > 200)
                    .map(|(&b, _)| b)
                    .collect();
                scores.sort_unstable();
                let pct = |q: f32| {
                    scores
                        .get(((scores.len() as f32 - 1.0) * q) as usize)
                        .map_or(0.0, |&v| v as f32 / analysis::BLEMISH_SCALE)
                };
                println!(
                    "    region at {},{}; zoom at {},{} side {}",
                    r.x,
                    r.y,
                    (face.mesh.points[205][0] as u32).saturating_sub((face.extent * 0.2) as u32),
                    (face.mesh.points[205][1] as u32)
                        .saturating_sub((face.extent * 0.4) as u32 * 2 / 3),
                    (face.extent * 0.4) as u32
                );
                println!(
                    "    blemish score p50 {:.2} p90 {:.2} p99 {:.2} p99.9 {:.2} max {:.2}",
                    pct(0.5),
                    pct(0.9),
                    pct(0.99),
                    pct(0.999),
                    pct(1.0)
                );
                let heat = image::GrayImage::from_fn(r.w, r.h, |x, y| {
                    let k = (y * r.w + x) as usize;
                    image::Luma([face.spot_score[k].saturating_mul(2)])
                });
                heat.save(dir.join(format!("pr_{name}_heat{i}.png")))
                    .unwrap();
                let cheek = face.mesh.points[205];
                let side = (face.extent * 0.4) as u32;
                let zoom = Region {
                    x: (cheek[0] as u32).saturating_sub(side / 2),
                    y: (cheek[1] as u32).saturating_sub(side * 2 / 3),
                    w: side,
                    h: side,
                };
                let mut detail = image::RgbImage::new(side * 2 + 8, side);
                image::imageops::replace(&mut detail, &crop_rgb(&rgba, width, zoom), 0, 0);
                image::imageops::replace(
                    &mut detail,
                    &crop_rgb(&after, width, zoom),
                    (side + 8) as i64,
                    0,
                );
                detail
                    .save(dir.join(format!("pr_{name}_zoom{i}.png")))
                    .unwrap();
                let before = crop_rgb(&rgba, width, r);
                let retouched = crop_rgb(&after, width, r);
                let mut sheet = image::RgbImage::new(r.w * 2 + 8, r.h);
                image::imageops::replace(&mut sheet, &before, 0, 0);
                image::imageops::replace(&mut sheet, &retouched, (r.w + 8) as i64, 0);
                let scale = (1800.0 / sheet.width() as f32).min(1.0);
                let sheet = image::imageops::resize(
                    &sheet,
                    (sheet.width() as f32 * scale) as u32,
                    (sheet.height() as f32 * scale) as u32,
                    image::imageops::FilterType::Triangle,
                );
                sheet
                    .save(dir.join(format!("pr_{name}_face{i}.jpg")))
                    .unwrap();
                let masks = image::RgbImage::from_fn(r.w, r.h, |x, y| {
                    let k = (y * r.w + x) as usize;
                    let spot = face.spot_score[k] as f32 / analysis::BLEMISH_SCALE > 0.81;
                    image::Rgb([
                        face.skin[k].max(if spot { 255 } else { 0 }),
                        face.under_eye[k]
                            .max(face.eye_white[k])
                            .max(if spot { 255 } else { 0 }),
                        face.teeth[k].max(face.iris[k]),
                    ])
                });
                let masks = image::imageops::resize(
                    &masks,
                    (r.w as f32 * scale * 2.0) as u32 / 2,
                    (r.h as f32 * scale * 2.0) as u32 / 2,
                    image::imageops::FilterType::Triangle,
                );
                masks
                    .save(dir.join(format!("pr_{name}_mask{i}.jpg")))
                    .unwrap();
            }
        }
    }
}
