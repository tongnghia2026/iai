//! Portrait retouching in the style of dedicated portrait tools: the face mesh
//! places the features, the Sapiens2 part model (when installed) outlines skin,
//! teeth and hair, and classic frequency-split processing does the retouch so
//! real skin texture survives.

pub mod analysis;
mod blur;
pub mod brush;
pub mod effects;
pub mod geometry;
mod skin_mask;

pub use analysis::{analyze, Clip, FaceModel, PortraitModel, SkinLayers, TRUSTED_AGREEMENT};
pub use effects::{render, render_masks, FaceEdits, PortraitSettings};
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
            let old = analyze(&rgba, width, height, false, None, &|_| {}).unwrap();
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
                let r = face.skin.region;
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
                    face: face.region,
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
                let a_old = skin_mask::audit(&input, &old.faces[i].skin.mask);
                let fresh: Vec<u8> = fresh
                    .map(|m| m.iter().map(|&v| (v * 255.0).round() as u8).collect())
                    .unwrap_or_else(|| old.faces[i].skin.mask.clone());
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
                image::imageops::replace(&mut sheet, &tint(&old.faces[i].skin.mask), 0, 0);
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
                image::GrayImage::from_raw(r.w, r.h, old.faces[i].skin.mask.clone())
                    .unwrap()
                    .save(dir.join(format!("pr_{name}_old{i}.png")))
                    .unwrap();
            }
            if std::env::var("IAI_PORTRAIT_SKIN_RENDER").is_ok() {
                let fresh = analyze(&rgba, width, height, false, None, &|_| {}).unwrap();
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
                    let (u, px) = render(&rgba, model, &strong, &enabled, &[]).unwrap();
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

    /// Opt-in: IAI_PORTRAIT_BROW_PROBE is a folder of photos; per face 0, a
    /// close-up of the brows: photo, brow mask, the default retouch, and the
    /// brow sliders pushed both ways.
    #[test]
    #[ignore]
    fn probe_brows() {
        let Ok(dir) = std::env::var("IAI_PORTRAIT_BROW_PROBE") else {
            return;
        };
        let dir = std::path::PathBuf::from(dir);
        let mut names: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().to_string())
            .filter(|n| n.ends_with(".jpg") && !n.starts_with("pb_"))
            .collect();
        names.sort();
        for name in names {
            let image = image::open(dir.join(&name)).unwrap().to_rgba8();
            let (width, height) = image.dimensions();
            let rgba = image.into_raw();
            let model = analyze(&rgba, width, height, false, None, &|_| {}).unwrap();
            let face = &model.faces[0];
            let e = face.extent;
            let brows = geometry::RIGHT_BROW
                .iter()
                .chain(&geometry::LEFT_BROW)
                .map(|&k| {
                    [
                        face.mesh.points[k as usize][0],
                        face.mesh.points[k as usize][1],
                    ]
                });
            let area = Region::around(brows, [0.12 * e; 4], width, height);
            let enabled = [true];
            let paste = |settings: &PortraitSettings| {
                let (u, px) = render(&rgba, &model, settings, &enabled, &[]).unwrap();
                image::RgbImage::from_fn(area.w, area.h, |x, y| {
                    let (ix, iy) = (area.x + x, area.y + y);
                    let k = ((iy - u.y) * u.w + ix - u.x) as usize * 4;
                    image::Rgb([px[k], px[k + 1], px[k + 2]])
                })
            };
            let b = &face.brows;
            let mask = image::RgbImage::from_fn(area.w, area.h, |x, y| {
                let (v, a) = b
                    .region
                    .index_at(area.x + x, area.y + y)
                    .map_or((0, 0), |k| (b.hair[k], b.area[k]));
                image::Rgb([v, v, v.max(a / 2)])
            });
            let tiles = [
                crop_rgb(&rgba, width, area),
                mask,
                paste(&PortraitSettings::default()),
                paste(&PortraitSettings {
                    brows: 70.0,
                    ..PortraitSettings::NEUTRAL
                }),
                paste(&PortraitSettings {
                    brows: -70.0,
                    ..PortraitSettings::NEUTRAL
                }),
                paste(&PortraitSettings {
                    brow_sharpen: 80.0,
                    ..PortraitSettings::NEUTRAL
                }),
            ];
            let mut tiles = tiles.to_vec();
            if std::env::var("IAI_PORTRAIT_BROW_PARTS").is_ok() {
                let mut seg = crate::core::ai::body_parts::Segmenter::load(false).unwrap();
                let parts = seg.segment_face(&rgba, width, height, &face.mesh).unwrap();
                tiles.push(image::RgbImage::from_fn(area.w, area.h, |x, y| {
                    let g = parts.groups_at((area.x + x) as f32 + 0.5, (area.y + y) as f32 + 0.5);
                    let v = |k: usize| (g[k] * 255.0) as u8;
                    image::Rgb([
                        v(crate::core::ai::body_parts::GROUP_HAIR),
                        v(crate::core::ai::body_parts::GROUP_FACE_SKIN),
                        0,
                    ])
                }));
            }
            let mut sheet = image::RgbImage::new(area.w, (area.h + 6) * tiles.len() as u32);
            for (k, tile) in tiles.iter().enumerate() {
                image::imageops::replace(&mut sheet, tile, 0, ((area.h + 6) * k as u32) as i64);
            }
            let k = (700.0 / area.w as f32).min(2.0);
            image::imageops::resize(
                &sheet,
                (sheet.width() as f32 * k) as u32,
                (sheet.height() as f32 * k) as u32,
                image::imageops::FilterType::Lanczos3,
            )
            .save(dir.join(format!("pb_{}.png", name.trim_end_matches(".jpg"))))
            .unwrap();
            println!("{name}: brows {}x{} e {e:.0}", area.w, area.h);
        }
    }

    /// Opt-in: IAI_PORTRAIT_NOSE_PROBE is a folder of photos; per face 0, a
    /// close-up of the nose: photo, contour field (grey = 0), and the contour
    /// at 60 and 100 over the default retouch.
    #[test]
    #[ignore]
    fn probe_nose() {
        let Ok(dir) = std::env::var("IAI_PORTRAIT_NOSE_PROBE") else {
            return;
        };
        let dir = std::path::PathBuf::from(dir);
        let mut names: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().to_string())
            .filter(|n| n.ends_with(".jpg"))
            .collect();
        names.sort();
        for name in names {
            let image = image::open(dir.join(&name)).unwrap().to_rgba8();
            let (width, height) = image.dimensions();
            let rgba = image.into_raw();
            let model = analyze(&rgba, width, height, false, None, &|_| {}).unwrap();
            let face = &model.faces[0];
            let e = face.extent;
            let nose = geometry::NOSE_BRIDGE
                .iter()
                .chain(&geometry::NOSE_WINGS)
                .map(|&k| {
                    [
                        face.mesh.points[k as usize][0],
                        face.mesh.points[k as usize][1],
                    ]
                });
            let area = Region::around(nose, [0.15 * e; 4], width, height);
            let enabled = [true];
            let paste = |settings: &PortraitSettings| {
                let (u, px) = render(&rgba, &model, settings, &enabled, &[]).unwrap();
                image::RgbImage::from_fn(area.w, area.h, |x, y| {
                    let k = ((area.y + y - u.y) * u.w + area.x + x - u.x) as usize * 4;
                    image::Rgb([px[k], px[k + 1], px[k + 2]])
                })
            };
            let field = image::RgbImage::from_fn(area.w, area.h, |x, y| {
                let v = face
                    .region
                    .index_at(area.x + x, area.y + y)
                    .map_or(0, |f| face.nose[f]);
                let g = (128 + v as i32) as u8;
                image::Rgb([g, g, g])
            });
            let mut photo = crop_rgb(&rgba, width, area);
            if std::env::var("IAI_PORTRAIT_NOSE_MARKS").is_ok() {
                for (k, &m) in [9u16, 8, 168, 6, 197, 195, 5, 4, 1, 19, 94, 2]
                    .iter()
                    .enumerate()
                {
                    let p = face.mesh.points[m as usize];
                    let (px, py) = (p[0] - area.x as f32, p[1] - area.y as f32);
                    println!("  {name} mark {k} = point {m}: {px:.0},{py:.0}");
                    let colour = image::Rgb([(k * 20) as u8, 255 - (k * 20) as u8, 0]);
                    for dy in -4i32..=4 {
                        for dx in -4i32..=4 {
                            let (x, y) = (px as i32 + dx, py as i32 + dy);
                            if x >= 0 && y >= 0 && (x as u32) < area.w && (y as u32) < area.h {
                                photo.put_pixel(x as u32, y as u32, colour);
                            }
                        }
                    }
                }
            }
            let tiles = [
                photo,
                field,
                paste(&PortraitSettings {
                    nose_bridge: 60.0,
                    ..PortraitSettings::default()
                }),
                paste(&PortraitSettings {
                    nose_bridge: 100.0,
                    ..PortraitSettings::default()
                }),
            ];
            let mut sheet = image::RgbImage::new((area.w + 6) * tiles.len() as u32, area.h);
            for (k, tile) in tiles.iter().enumerate() {
                image::imageops::replace(&mut sheet, tile, ((area.w + 6) * k as u32) as i64, 0);
            }
            let k = (1400.0 / sheet.width() as f32).min(2.0);
            image::imageops::resize(
                &sheet,
                (sheet.width() as f32 * k) as u32,
                (sheet.height() as f32 * k) as u32,
                image::imageops::FilterType::Lanczos3,
            )
            .save(dir.join(format!("pn_{}.png", name.trim_end_matches(".jpg"))))
            .unwrap();
        }
    }

    /// Opt-in: IAI_PORTRAIT_ANALYSE names one photo; analyses it alone (for
    /// timing and peak memory) and prints each face's regions.
    #[test]
    #[ignore]
    fn probe_analyse() {
        let Ok(path) = std::env::var("IAI_PORTRAIT_ANALYSE") else {
            return;
        };
        let image = image::open(&path).unwrap().to_rgba8();
        let (width, height) = image.dimensions();
        let rgba = image.into_raw();
        let started = std::time::Instant::now();
        let model = analyze(&rgba, width, height, false, None, &|_| {}).unwrap();
        println!(
            "{width}x{height}: analyse {} ms {:?}",
            started.elapsed().as_millis(),
            model.timings
        );
        for face in &model.faces {
            let (r, s) = (face.region, face.skin.region);
            println!(
                "  face {}x{} skin {}x{} at {},{}",
                r.w, r.h, s.w, s.h, s.x, s.y
            );
        }
    }

    /// Opt-in: IAI_PORTRAIT_HAIR_PROBE is a folder of photos; per photo a
    /// sheet of the hair region (photo | Sáng tóc +50 | +100 | -100) and the
    /// same at the forehead hairline.
    #[test]
    #[ignore]
    fn probe_hair_tone() {
        let Ok(dir) = std::env::var("IAI_PORTRAIT_HAIR_PROBE") else {
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
            let model = analyze(&rgba, width, height, false, None, &|_| {}).unwrap();
            let Some(face) = model.faces.first() else {
                continue;
            };
            if face.hair_region.is_empty() {
                println!("{name}: no hair");
                continue;
            }
            let enabled = vec![true; model.faces.len()];
            let mut views = vec![rgba.clone()];
            for amount in [50.0, 100.0, -100.0] {
                let s = PortraitSettings {
                    hair_brightness: amount,
                    ..PortraitSettings::NEUTRAL
                };
                let started = std::time::Instant::now();
                let (u, px) = render(&rgba, &model, &s, &enabled, &[]).unwrap();
                println!(
                    "{name}: hair {amount} render {} ms",
                    started.elapsed().as_millis()
                );
                let mut out = rgba.clone();
                for row in 0..u.h as usize {
                    let o = ((u.y as usize + row) * width as usize + u.x as usize) * 4;
                    let k = row * u.w as usize * 4;
                    out[o..o + u.w as usize * 4].copy_from_slice(&px[k..k + u.w as usize * 4]);
                }
                views.push(out);
            }
            let (centre, extent, _) = face.mesh.frame();
            let hr = face.hair_region;
            let line = Region::around(
                [
                    [centre[0] - 0.45 * extent, centre[1] - 0.95 * extent],
                    [centre[0] + 0.45 * extent, centre[1] - 0.35 * extent],
                ]
                .into_iter(),
                [0.0; 4],
                width,
                height,
            );
            for (tag, r, side) in [("hair", hr, 420u32), ("line", line, 600u32)] {
                let k = side as f32 / r.w as f32;
                let (tw, th) = (side, (r.h as f32 * k) as u32);
                let mut sheet = image::RgbImage::new(tw * views.len() as u32, th);
                for (i, v) in views.iter().enumerate() {
                    let tile = image::imageops::resize(
                        &crop_rgb(v, width, r),
                        tw,
                        th,
                        image::imageops::FilterType::Triangle,
                    );
                    image::imageops::replace(&mut sheet, &tile, (i as u32 * tw) as i64, 0);
                }
                sheet
                    .save(dir.join(format!("pr_{name}_{tag}.png")))
                    .unwrap();
            }
        }
    }

    /// Opt-in: IAI_PORTRAIT_CLIP_PROBE is a folder with photos and `clips.txt`
    /// (`name.jpg x0,y0,x1,y1` per line): hair masks without and with that
    /// rectangle selected, side by side over the rectangle.
    #[test]
    #[ignore]
    fn probe_clip() {
        let Ok(dir) = std::env::var("IAI_PORTRAIT_CLIP_PROBE") else {
            return;
        };
        let dir = std::path::PathBuf::from(dir);
        let list = std::fs::read_to_string(dir.join("clips.txt")).unwrap();
        for line in list.lines().filter(|l| !l.trim().is_empty()) {
            let (name, rect) = line.split_once(' ').unwrap();
            let v: Vec<u32> = rect.split(',').map(|p| p.trim().parse().unwrap()).collect();
            let image = image::open(dir.join(name)).unwrap().to_rgba8();
            let (width, height) = image.dimensions();
            let rgba = image.into_raw();
            let r = Region {
                x: v[0],
                y: v[1],
                w: v[2].min(width) - v[0],
                h: v[3].min(height) - v[1],
            };
            let clip = Clip {
                region: r,
                mask: vec![255; r.len()],
            };
            let tile = |model: &PortraitModel| {
                let f = &model.faces[0];
                let hr = f.hair_region;
                image::RgbImage::from_fn(r.w, r.h, |x, y| {
                    let (ix, iy) = (r.x + x, r.y + y);
                    let o = ((iy * width + ix) * 4) as usize;
                    let inside = ix >= hr.x && iy >= hr.y && ix < hr.x + hr.w && iy < hr.y + hr.h;
                    let m = if inside {
                        f.hair_mask()[((iy - hr.y) * hr.w + ix - hr.x) as usize] as f32 / 255.0
                            * 0.7
                    } else {
                        0.0
                    };
                    let tint = [150.0, 60.0, 255.0];
                    image::Rgb(std::array::from_fn(|k| {
                        (rgba[o + k] as f32 * (1.0 - m) + tint[k] * m) as u8
                    }))
                })
            };
            let auto = analyze(&rgba, width, height, false, None, &|_| {}).unwrap();
            let close = analyze(&rgba, width, height, false, Some(clip), &|_| {}).unwrap();
            println!(
                "{name}: hair region auto {}x{} close {}x{}, prepare {} / {} ms",
                auto.faces[0].hair_region.w,
                auto.faces[0].hair_region.h,
                close.faces[0].hair_region.w,
                close.faces[0].hair_region.h,
                auto.timings[3],
                close.timings[3]
            );
            let mut sheet =
                image::RgbImage::from_pixel(r.w * 2 + 8, r.h, image::Rgb([255, 255, 255]));
            image::imageops::replace(&mut sheet, &tile(&auto), 0, 0);
            image::imageops::replace(&mut sheet, &tile(&close), (r.w + 8) as i64, 0);
            sheet
                .save(dir.join(format!("pc_{}.png", name.trim_end_matches(".jpg"))))
                .unwrap();
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
            let model = analyze(&rgba, width, height, false, None, &|_| {}).unwrap();
            let analyse_ms = started.elapsed().as_millis();
            let enabled = vec![true; model.faces.len()];
            let started = std::time::Instant::now();
            let (union, pixels) =
                render(&rgba, &model, &PortraitSettings::default(), &enabled, &[]).unwrap();
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
            let (su, sp) = render(&rgba, &model, &strong, &enabled, &[]).unwrap();
            let (mu, mp) = render_masks(&rgba, &model, &enabled, &[]).unwrap();
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
                let sr = f.skin.region;
                for yy in 0..sr.h {
                    for xx in 0..sr.w {
                        let i = (yy * sr.w + xx) as usize;
                        let (x, y) = (sr.x + xx, sr.y + yy);
                        let feature = f
                            .region
                            .index_at(x, y)
                            .is_some_and(|k| f.nose[k] != 0 || f.lips[k] != 0 || f.iris[k] != 0);
                        if f.skin.mask[i] > 200 && !feature {
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
                for yy in 0..sr.h {
                    for xx in 0..sr.w {
                        if f.skin.mask[(yy * sr.w + xx) as usize] < 200 || f.hair.is_empty() {
                            continue;
                        }
                        let (x, y) = (sr.x + xx, sr.y + yy);
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
                let face_interior = face.skin.region.crop(&face.skin.interior, r);
                let mut scores: Vec<u8> = face
                    .spot_score
                    .iter()
                    .zip(&face_interior)
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
                let face_skin = face.skin.region.crop(&face.skin.mask, r);
                let masks = image::RgbImage::from_fn(r.w, r.h, |x, y| {
                    let k = (y * r.w + x) as usize;
                    let spot = face.spot_score[k] as f32 / analysis::BLEMISH_SCALE > 0.81;
                    image::Rgb([
                        face_skin[k].max(if spot { 255 } else { 0 }),
                        face.skin.under_eye[k].max(face.eye_white[k]).max(if spot {
                            255
                        } else {
                            0
                        }),
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
                let sr = face.skin.region;
                println!("    skin region at {},{} {}x{}", sr.x, sr.y, sr.w, sr.h);
                image::GrayImage::from_raw(sr.w, sr.h, face.skin.mask.clone())
                    .unwrap()
                    .save(dir.join(format!("pr_{name}_skin{i}.png")))
                    .unwrap();
                image::GrayImage::from_raw(sr.w, sr.h, face.skin.interior.clone())
                    .unwrap()
                    .save(dir.join(format!("pr_{name}_inside{i}.png")))
                    .unwrap();
                if i == 0 && std::env::var("IAI_PORTRAIT_PROBE_PARTS").is_ok() {
                    let mut seg = crate::core::ai::body_parts::Segmenter::load(false).unwrap();
                    let parts = seg.segment_face(&rgba, width, height, &face.mesh).unwrap();
                    let sr = face.skin.region;
                    let dump = |group: usize, tag: &str| {
                        image::GrayImage::from_fn(sr.w, sr.h, |x, y| {
                            let (px, py) = ((sr.x + x) as f32 + 0.5, (sr.y + y) as f32 + 0.5);
                            let g = parts.groups_at(px, py);
                            let v = if parts.covers(px, py) {
                                g[group] * 255.0
                            } else {
                                40.0
                            };
                            image::Luma([v.round() as u8])
                        })
                        .save(dir.join(format!("pr_{name}_{tag}{i}.png")))
                        .unwrap();
                    };
                    dump(crate::core::ai::body_parts::GROUP_HAIR, "phair");
                    dump(crate::core::ai::body_parts::GROUP_FACE_SKIN, "pskin");
                    dump(crate::core::ai::body_parts::GROUP_BODY_SKIN, "pbody");
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
