//! Portrait retouching in the style of dedicated portrait tools: the face mesh
//! places the features, the Sapiens2 part model (when installed) outlines skin,
//! teeth and hair, and classic frequency-split processing does the retouch so
//! real skin texture survives.

pub mod analysis;
mod blur;
pub mod effects;
pub mod geometry;
mod skin_mask;

pub use analysis::{analyze, FaceModel, PortraitModel, TRUSTED_AGREEMENT};
pub use effects::{render, render_masks, PortraitSettings};
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

    /// Opt-in: set IAI_PORTRAIT_SKIN_PROBE to a folder of photos; compares the
    /// part model's skin mask with the colour-model one (holes, leaks, time)
    /// and writes an old | new overlay sheet per face.
    #[test]
    #[ignore]
    fn probe_skin_mask() {
        let Ok(dir) = std::env::var("IAI_PORTRAIT_SKIN_PROBE") else {
            return;
        };
        let dir = std::path::PathBuf::from(dir);
        let mut names: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().to_string())
            .filter(|n| n.ends_with(".jpg") && !n.starts_with("pr_"))
            .filter(|n| {
                std::env::var("IAI_PORTRAIT_SKIN_ONLY").map_or(true, |only| n.contains(&only))
            })
            .collect();
        names.sort();
        let mut segmenter = crate::core::ai::body_parts::Segmenter::load(false).unwrap();
        for name in names {
            let image = image::open(dir.join(&name)).unwrap().to_rgba8();
            let (width, height) = image.dimensions();
            let rgba = image.into_raw();
            skin_mask::set_legacy(true);
            let old = analyze(&rgba, width, height, false, &|_| {}).unwrap();
            skin_mask::set_legacy(false);
            println!(
                "{name}: {width}x{height}, prepare old {} ms",
                old.timings[3]
            );
            let new = &old;
            let owners: Vec<([f32; 2], f32)> = new
                .faces
                .iter()
                .map(|f| {
                    let (c, s, _) = f.mesh.frame();
                    (c, s)
                })
                .collect();
            for (i, face) in new.faces.iter().enumerate() {
                let r = face.region;
                let parts = segmenter
                    .segment_face(&rgba, width, height, &face.mesh)
                    .unwrap();
                let trusted = parts.agreement >= TRUSTED_AGREEMENT;
                let src: Vec<[f32; 3]> = (0..r.len())
                    .map(|k| {
                        let o = ((r.y as usize + k / r.w as usize) * width as usize
                            + r.x as usize
                            + k % r.w as usize)
                            * 4;
                        [
                            rgba[o] as f32 / 255.0,
                            rgba[o + 1] as f32 / 255.0,
                            rgba[o + 2] as f32 / 255.0,
                        ]
                    })
                    .collect();
                let input = skin_mask::SkinInputs {
                    src: &src,
                    region: r,
                    extent: face.extent,
                    points: &face.mesh.points,
                    parts: trusted.then_some(&parts),
                    owners: &owners,
                    index: i,
                    open_sides: [r.x > 0, r.y > 0, r.x + r.w < width, r.y + r.h < height],
                };
                let started = std::time::Instant::now();
                let fresh = skin_mask::skin_mask(&input);
                let mask_ms = started.elapsed().as_millis();
                let a_old = skin_mask::audit(&input, &old.faces[i].skin);
                let fresh: Vec<u8> = fresh
                    .map(|m| m.iter().map(|&v| (v * 255.0).round() as u8).collect())
                    .unwrap_or_else(|| old.faces[i].skin.clone());
                let a_new = skin_mask::audit(&input, &fresh);
                if let Some((gw, gh, map, summary)) = skin_mask::evidence_map(&input) {
                    println!("    {summary}");
                    image::RgbImage::from_raw(gw as u32, gh as u32, map)
                        .unwrap()
                        .save(dir.join(format!("pr_{name}_ev{i}.png")))
                        .unwrap();
                }
                let pct = |a: Option<(f32, f32)>| {
                    a.map_or("-".to_string(), |(hole, leak)| {
                        format!("holes {:.1}% leaks {:.2}%", hole * 100.0, leak * 100.0)
                    })
                };
                println!(
                    "  face {i}: {}x{} at {},{} e {:.0} trusted {trusted} | old {} | new {} | mask {mask_ms} ms",
                    r.w,
                    r.h,
                    r.x,
                    r.y,
                    face.extent,
                    pct(a_old),
                    pct(a_new),
                );
                let tint = |mask: &[u8]| {
                    image::RgbImage::from_fn(r.w, r.h, |x, y| {
                        let k = (y * r.w + x) as usize;
                        let m = mask[k] as f32 / 255.0 * 0.55;
                        let c = src[k];
                        image::Rgb([
                            ((c[0] * (1.0 - m) + m) * 255.0) as u8,
                            (c[1] * (1.0 - m) * 255.0) as u8,
                            ((c[2] * (1.0 - m) + m * 0.2) * 255.0) as u8,
                        ])
                    })
                };
                let mut sheet = image::RgbImage::new(r.w * 2 + 8, r.h);
                image::imageops::replace(&mut sheet, &tint(&old.faces[i].skin), 0, 0);
                image::imageops::replace(&mut sheet, &tint(&fresh), (r.w + 8) as i64, 0);
                let k = (2000.0 / sheet.width() as f32).min(1.0);
                image::imageops::resize(
                    &sheet,
                    (sheet.width() as f32 * k) as u32,
                    (sheet.height() as f32 * k) as u32,
                    image::imageops::FilterType::Triangle,
                )
                .save(dir.join(format!("pr_{name}_cmp{i}.jpg")))
                .unwrap();
                image::GrayImage::from_raw(r.w, r.h, fresh.clone())
                    .unwrap()
                    .save(dir.join(format!("pr_{name}_new{i}.png")))
                    .unwrap();
                image::GrayImage::from_raw(r.w, r.h, old.faces[i].skin.clone())
                    .unwrap()
                    .save(dir.join(format!("pr_{name}_old{i}.png")))
                    .unwrap();
            }
            if std::env::var("IAI_PORTRAIT_SKIN_RENDER").is_ok() {
                let fresh = analyze(&rgba, width, height, false, &|_| {}).unwrap();
                println!("  prepare new {} ms", fresh.timings[3]);
                let strong = PortraitSettings {
                    smooth: 80.0,
                    even_tone: 60.0,
                    brighten: 40.0,
                    shine: 40.0,
                    ..PortraitSettings::default()
                };
                let enabled = vec![true; old.faces.len()];
                let r = old.faces[0].region;
                for (tag, model) in [("old", &old), ("new", &fresh)] {
                    let (u, px) = render(&rgba, model, &strong, &enabled).unwrap();
                    let mut full = rgba.clone();
                    for row in 0..u.h as usize {
                        let o = ((u.y as usize + row) * width as usize + u.x as usize) * 4;
                        let s = row * u.w as usize * 4;
                        full[o..o + u.w as usize * 4].copy_from_slice(&px[s..s + u.w as usize * 4]);
                    }
                    crop_rgb(&full, width, r)
                        .save(dir.join(format!("pr_{name}_r{tag}.png")))
                        .unwrap();
                }
            }
        }
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
                nose_bridge: 80.0,
                iris_hue: 200.0,
                iris_tint: 70.0,
                lip_hue: 340.0,
                lip_tint: 60.0,
                hair_brightness: 30.0,
                hair_hue: 15.0,
                hair_tint: 60.0,
                ..PortraitSettings::NEUTRAL
            };
            let (su, sp) = render(&rgba, &model, &strong, &enabled).unwrap();
            let (mu, mp) = render_masks(&rgba, &model, &enabled).unwrap();
            let mut tinted = rgba.clone();
            for row in 0..mu.h as usize {
                let o = ((mu.y as usize + row) * width as usize + mu.x as usize) * 4;
                let s = row * mu.w as usize * 4;
                tinted[o..o + mu.w as usize * 4].copy_from_slice(&mp[s..s + mu.w as usize * 4]);
            }
            let r0 = if model.faces[0].hair_region.is_empty() {
                model.faces[0].region
            } else {
                model.faces[0].hair_region
            };
            let k = (900.0 / r0.w as f32).min(1.0);
            image::imageops::resize(
                &crop_rgb(&tinted, width, r0),
                (r0.w as f32 * k) as u32,
                (r0.h as f32 * k) as u32,
                image::imageops::FilterType::Triangle,
            )
            .save(dir.join(format!("pr_{name}_areas.jpg")))
            .unwrap();
            let mut styled = rgba.clone();
            for row in 0..su.h as usize {
                let o = ((su.y as usize + row) * width as usize + su.x as usize) * 4;
                let s = row * su.w as usize * 4;
                styled[o..o + su.w as usize * 4].copy_from_slice(&sp[s..s + su.w as usize * 4]);
            }
            // Leak check: how much the styled render moved pixels that no
            // styled area covers (plain skin away from nose, lips and eyes;
            // non-hair pixels of the hair region).
            {
                let f = &model.faces[0];
                let diff = |x: u32, y: u32| {
                    let o = ((y * width + x) * 4) as usize;
                    (0..3)
                        .map(|k| (styled[o + k] as f32 - rgba[o + k] as f32).abs())
                        .sum::<f32>()
                        / 3.0
                };
                let (mut skin_sum, mut skin_n, mut bg_sum, mut bg_n) = (0.0f64, 0u64, 0.0f64, 0u64);
                for yy in 0..f.region.h {
                    for xx in 0..f.region.w {
                        let i = (yy * f.region.w + xx) as usize;
                        if f.skin[i] > 200 && f.nose[i] == 0 && f.lips[i] == 0 && f.iris[i] == 0 {
                            let (x, y) = (f.region.x + xx, f.region.y + yy);
                            let hx = x as i64 - f.hair_region.x as i64;
                            let hy = y as i64 - f.hair_region.y as i64;
                            let in_hair = !f.hair.is_empty()
                                && hx >= 0
                                && hy >= 0
                                && (hx as u32) < f.hair_region.w
                                && (hy as u32) < f.hair_region.h
                                && f.hair[(hy as u32 * f.hair_region.w + hx as u32) as usize] > 0;
                            if !in_hair {
                                skin_sum += diff(x, y) as f64;
                                skin_n += 1;
                            }
                        }
                    }
                }
                for yy in 0..f.hair_region.h {
                    for xx in 0..f.hair_region.w {
                        let i = (yy * f.hair_region.w + xx) as usize;
                        if !f.hair.is_empty() && f.hair[i] == 0 {
                            bg_sum += diff(f.hair_region.x + xx, f.hair_region.y + yy) as f64;
                            bg_n += 1;
                        }
                    }
                }
                println!(
                    "    leak: plain skin {:.3} levels over {skin_n} px, non-hair {:.3} levels over {bg_n} px",
                    skin_sum / skin_n.max(1) as f64,
                    bg_sum / bg_n.max(1) as f64
                );
                let mut on_skin = 0u64;
                for yy in 0..f.region.h {
                    for xx in 0..f.region.w {
                        if f.skin[(yy * f.region.w + xx) as usize] < 200 || f.hair.is_empty() {
                            continue;
                        }
                        let (x, y) = (f.region.x + xx, f.region.y + yy);
                        if x >= f.hair_region.x
                            && y >= f.hair_region.y
                            && x < f.hair_region.x + f.hair_region.w
                            && y < f.hair_region.y + f.hair_region.h
                        {
                            let k = ((y - f.hair_region.y) * f.hair_region.w + x - f.hair_region.x)
                                as usize;
                            if f.hair[k] > 10 {
                                on_skin += 1;
                            }
                        }
                    }
                }
                println!("    solid skin pixels also marked hair: {on_skin}");
            }
            let face0 = &model.faces[0];
            let r0 = if face0.hair_region.is_empty() {
                face0.region
            } else {
                face0.hair_region
            };
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
                image::GrayImage::from_raw(r.w, r.h, face.skin.clone())
                    .unwrap()
                    .save(dir.join(format!("pr_{name}_skin{i}.png")))
                    .unwrap();
                image::GrayImage::from_raw(r.w, r.h, face.interior.clone())
                    .unwrap()
                    .save(dir.join(format!("pr_{name}_inside{i}.png")))
                    .unwrap();
                if i == 0 && std::env::var("IAI_PORTRAIT_PROBE_PARTS").is_ok() {
                    let mut seg = crate::core::ai::body_parts::Segmenter::load(false).unwrap();
                    let parts = seg.segment_face(&rgba, width, height, &face.mesh).unwrap();
                    let dump = |group: usize, tag: &str| {
                        image::GrayImage::from_fn(r.w, r.h, |x, y| {
                            let g = parts.groups_at((r.x + x) as f32 + 0.5, (r.y + y) as f32 + 0.5);
                            image::Luma([(g[group] * 255.0).round() as u8])
                        })
                        .save(dir.join(format!("pr_{name}_{tag}{i}.png")))
                        .unwrap();
                    };
                    dump(crate::core::ai::body_parts::GROUP_HAIR, "phair");
                    dump(crate::core::ai::body_parts::GROUP_FACE_SKIN, "pskin");
                }
                if !face.hair.is_empty() {
                    let hr = face.hair_region;
                    image::GrayImage::from_raw(hr.w, hr.h, face.hair.clone())
                        .unwrap()
                        .save(dir.join(format!("pr_{name}_hair{i}.png")))
                        .unwrap();
                    println!("    hair region at {},{} {}x{}", hr.x, hr.y, hr.w, hr.h);
                }
                let mut full = rgba.clone();
                for row in 0..union.h as usize {
                    let o = ((union.y as usize + row) * width as usize + union.x as usize) * 4;
                    let s = row * union.w as usize * 4;
                    full[o..o + union.w as usize * 4]
                        .copy_from_slice(&pixels[s..s + union.w as usize * 4]);
                }
                crop_rgb(&full, width, r)
                    .save(dir.join(format!("pr_{name}_after{i}.png")))
                    .unwrap();
                crop_rgb(&rgba, width, r)
                    .save(dir.join(format!("pr_{name}_before{i}.png")))
                    .unwrap();
                crop_rgb(&styled, width, r)
                    .save(dir.join(format!("pr_{name}_strong{i}.png")))
                    .unwrap();
            }
        }
    }
}
