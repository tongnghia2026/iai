use super::*;

/// The map from the unit square onto `quad` (top-left, top-right,
/// bottom-right, bottom-left), and back.
struct Homography {
    m: [f64; 9],
    inv: [f64; 9],
}

impl Homography {
    fn onto(quad: [(f32, f32); 4]) -> Self {
        let p = quad.map(|(x, y)| (x as f64, y as f64));
        let (x0, y0, x1, y1, x2, y2, x3, y3) = (
            p[0].0, p[0].1, p[1].0, p[1].1, p[2].0, p[2].1, p[3].0, p[3].1,
        );
        let (dx1, dx2, dx3) = (x1 - x2, x3 - x2, x0 - x1 + x2 - x3);
        let (dy1, dy2, dy3) = (y1 - y2, y3 - y2, y0 - y1 + y2 - y3);
        let den = dx1 * dy2 - dy1 * dx2;
        let g = (dx3 * dy2 - dy3 * dx2) / den;
        let h = (dx1 * dy3 - dy1 * dx3) / den;
        let m = [
            x1 - x0 + g * x1,
            x3 - x0 + h * x3,
            x0,
            y1 - y0 + g * y1,
            y3 - y0 + h * y3,
            y0,
            g,
            h,
            1.0,
        ];
        let det = m[0] * (m[4] * m[8] - m[5] * m[7]) - m[1] * (m[3] * m[8] - m[5] * m[6])
            + m[2] * (m[3] * m[7] - m[4] * m[6]);
        let inv = [
            (m[4] * m[8] - m[5] * m[7]) / det,
            (m[2] * m[7] - m[1] * m[8]) / det,
            (m[1] * m[5] - m[2] * m[4]) / det,
            (m[5] * m[6] - m[3] * m[8]) / det,
            (m[0] * m[8] - m[2] * m[6]) / det,
            (m[2] * m[3] - m[0] * m[5]) / det,
            (m[3] * m[7] - m[4] * m[6]) / det,
            (m[1] * m[6] - m[0] * m[7]) / det,
            (m[0] * m[4] - m[1] * m[3]) / det,
        ];
        Self { m, inv }
    }

    fn apply(m: &[f64; 9], x: f64, y: f64) -> (f64, f64) {
        let w = m[6] * x + m[7] * y + m[8];
        (
            (m[0] * x + m[1] * y + m[2]) / w,
            (m[3] * x + m[4] * y + m[5]) / w,
        )
    }

    fn to_card(&self, x: f64, y: f64) -> (f64, f64) {
        Self::apply(&self.inv, x, y)
    }

    #[allow(dead_code)]
    fn to_photo(&self, u: f64, v: f64) -> (f64, f64) {
        Self::apply(&self.m, u, v)
    }
}

/// Where the corners of an ID card fall in a `w` × `h` photo taken by a
/// camera looking at it through the middle of the photo: turned `spin`
/// degrees on the table, tipped `tilt_x` (top away) and `tilt_y` (right side
/// away), its width `fill` of the photo's.
fn card_seen(w: u32, h: u32, spin: f64, tilt_x: f64, tilt_y: f64, fill: f64) -> [(f32, f32); 4] {
    let focal = w.max(h) as f64;
    let depth = focal * 85.6 / (fill * w as f64);
    let (sz, cz) = spin.to_radians().sin_cos();
    let (sx, cx) = tilt_x.to_radians().sin_cos();
    let (sy, cy) = tilt_y.to_radians().sin_cos();
    [
        (-42.8, -26.99),
        (42.8, -26.99),
        (42.8, 26.99),
        (-42.8, 26.99),
    ]
    .map(|(x, y)| {
        let (x, y) = (x * cz - y * sz, x * sz + y * cz);
        let (y, z) = (y * cx, -y * sx);
        let (x, z) = (x * cy + z * sy, z * cy - x * sy);
        let z = z + depth;
        (
            (focal * x / z + w as f64 / 2.0) as f32,
            (focal * y / z + h as f64 / 2.0) as f32,
        )
    })
}

/// Cheap repeatable noise, 0 to 1.
fn noise(x: u32, y: u32, seed: u32) -> f32 {
    let mut n = x.wrapping_mul(374_761_393)
        ^ y.wrapping_mul(668_265_263)
        ^ seed.wrapping_mul(2_246_822_519);
    n = (n ^ (n >> 13)).wrapping_mul(1_274_126_177);
    ((n ^ (n >> 16)) & 0xffff) as f32 / 65535.0
}

/// An ID card's face at `(u, v)` of it (0 to 1 each way): pale, with a
/// photo, lines of text and a band, the edges inside a real card has.
fn card_at(u: f64, v: f64) -> Option<[f32; 3]> {
    // Rounded corners, 3 mm of 85.6 × 54.
    let (rx, ry) = (3.0 / 85.6, 3.0 / 54.0);
    let cu = u.clamp(rx, 1.0 - rx);
    let cv = v.clamp(ry, 1.0 - ry);
    let (du, dv) = ((u - cu) / rx, (v - cv) / ry);
    if !(0.0..=1.0).contains(&u) || !(0.0..=1.0).contains(&v) || du * du + dv * dv > 1.0 {
        return None;
    }
    let photo = (0.06..0.3).contains(&u) && (0.3..0.85).contains(&v);
    let text = (0.36..0.9).contains(&u) && ((v * 14.0) as i32 % 2 == 1) && v > 0.25 && v < 0.85;
    let band = (0.08..0.16).contains(&v);
    Some(if photo {
        [120.0, 100.0, 90.0]
    } else if text {
        [60.0, 70.0, 80.0]
    } else if band {
        [200.0, 60.0, 60.0]
    } else {
        [205.0 + 20.0 * u as f32, 228.0, 218.0 - 20.0 * v as f32]
    })
}

enum Ground {
    Red,
    Dark,
}

/// A photo `w` × `h` of a card lying at `quad`, 2×2 samples a pixel.
fn photo(w: u32, h: u32, quad: [(f32, f32); 4], ground: Ground, finger: bool) -> Vec<u8> {
    let map = Homography::onto(quad);
    let mut out = vec![0u8; (w * h * 4) as usize];
    for y in 0..h {
        for x in 0..w {
            let mut sum = [0.0f32; 3];
            for (sx, sy) in [(0.25, 0.25), (0.75, 0.25), (0.25, 0.75), (0.75, 0.75)] {
                let (px, py) = (x as f64 + sx, y as f64 + sy);
                let (u, v) = map.to_card(px, py);
                let n = noise(x, y, 7) * 30.0 - 15.0;
                let mut c = match card_at(u, v) {
                    Some(c) => c,
                    None => match ground {
                        Ground::Red => [190.0 + n, 35.0 + n * 0.5, 40.0 + n * 0.5],
                        Ground::Dark => [45.0 + n, 42.0 + n, 40.0 + n],
                    },
                };
                // A thumb over the lower-left of the card.
                let (fx, fy) = (px as f32 - quad[3].0 - 30.0, py as f32 - quad[3].1 + 10.0);
                if finger && (fx / 70.0).powi(2) + (fy / 45.0).powi(2) <= 1.0 {
                    c = [215.0, 160.0, 130.0];
                }
                for i in 0..3 {
                    sum[i] += c[i] / 4.0;
                }
            }
            let o = ((y * w + x) * 4) as usize;
            out[o..o + 4].copy_from_slice(&[
                sum[0].clamp(0.0, 255.0) as u8,
                sum[1].clamp(0.0, 255.0) as u8,
                sum[2].clamp(0.0, 255.0) as u8,
                255,
            ]);
        }
    }
    out
}

fn assert_near(found: [(f32, f32); 4], want: [(f32, f32); 4], within: f32) {
    for (f, w) in found.iter().zip(&want) {
        let off = (f.0 - w.0).hypot(f.1 - w.1);
        assert!(
            off <= within,
            "found {found:?}, wanted {want:?} (off by {off:.2})"
        );
    }
}

#[test]
fn a_card_on_a_red_stool_is_found_to_the_pixel() {
    let quad = card_seen(960, 1280, 2.0, 15.0, -8.0, 0.75);
    let rgba = photo(960, 1280, quad, Ground::Red, false);
    let found = find_card(&rgba, 960, 1280, ID_CARD_ASPECT).expect("a card");
    assert_near(found.corners, quad, 1.5);
    assert!(found.score > 0.3, "{}", found.score);
}

#[test]
fn a_card_at_a_slant_on_a_dark_table_is_found() {
    // Seen from low down: the far side shorter than the near one.
    let quad = card_seen(960, 1100, -3.0, 40.0, 10.0, 0.7);
    let rgba = photo(960, 1100, quad, Ground::Dark, false);
    let found = find_card(&rgba, 960, 1100, ID_CARD_ASPECT).expect("a card");
    assert_near(found.corners, quad, 2.0);
}

#[test]
fn a_thumb_over_an_edge_does_not_move_it() {
    let quad = card_seen(960, 1280, -1.0, 10.0, 4.0, 0.8);
    let rgba = photo(960, 1280, quad, Ground::Dark, true);
    let found = find_card(&rgba, 960, 1280, ID_CARD_ASPECT).expect("a card");
    assert_near(found.corners, quad, 2.0);
}

#[test]
fn a_card_standing_on_end_comes_out_lying_down() {
    // Long sides upright: the left one is taken for the top.
    let quad = card_seen(1000, 1100, 89.0, 12.0, 0.0, 0.48);
    let rgba = photo(1000, 1100, quad, Ground::Red, false);
    let found = find_card(&rgba, 1000, 1100, ID_CARD_ASPECT).expect("a card");
    assert_near(found.corners, order(quad), 2.0);
}

#[test]
fn a_photo_without_a_card_finds_none() {
    let (w, h) = (640u32, 480u32);
    let rgba: Vec<u8> = (0..w * h)
        .flat_map(|i| {
            let (x, y) = (i % w, i / w);
            let v = (60.0 + 100.0 * x as f32 / w as f32 + 20.0 * noise(x, y, 3)) as u8;
            [v, v, v, 255]
        })
        .collect();
    assert_eq!(find_card(&rgba, w, h, ID_CARD_ASPECT), None);
}

#[test]
fn corners_read_roughly_off_a_preview_are_snapped_onto_the_card() {
    let quad = card_seen(960, 1280, 2.0, 15.0, -8.0, 0.75);
    let rgba = photo(960, 1280, quad, Ground::Red, false);
    // Off by up to 14 px, and given from another corner and the other way round.
    let nudge = [(-12.0, 9.0), (14.0, -6.0), (-8.0, -12.0), (10.0, 11.0)];
    let mut rough = [(0.0f32, 0.0f32); 4];
    for k in 0..4 {
        rough[k] = (
            quad[(k + 3) % 4].0 + nudge[k].0,
            quad[(k + 3) % 4].1 + nudge[k].1,
        );
    }
    rough.reverse();
    assert_near(snap_corners(&rgba, 960, 1280, rough), quad, 1.5);
}

#[test]
fn corners_are_ordered_from_the_top_left_round_clockwise() {
    let want = [(10.0, 10.0), (100.0, 12.0), (98.0, 70.0), (8.0, 68.0)];
    for start in 0..4 {
        let mut turned = want;
        turned.rotate_left(start);
        assert_eq!(order(turned), want);
        turned.reverse();
        assert_eq!(order(turned), want);
    }
}

/// Run the finder over the photos of `IAI_CARD_PROBE_DIR`, and draw what it
/// found into `IAI_CARD_PROBE_OUT` (the card cut out, and the photo with the
/// corners marked).
#[test]
#[ignore = "probe: needs IAI_CARD_PROBE_DIR"]
fn probe_card_find() {
    let Some(dir) = std::env::var_os("IAI_CARD_PROBE_DIR") else {
        return;
    };
    let out = std::path::PathBuf::from(std::env::var_os("IAI_CARD_PROBE_OUT").unwrap_or_default());
    let _ = std::fs::create_dir_all(&out);
    let mut files: Vec<_> = std::fs::read_dir(dir)
        .expect("the folder")
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("jpg")))
        .collect();
    files.sort();
    for (n, path) in files.iter().enumerate() {
        let img = image::open(path).expect("a photo").to_rgba8();
        let (w, h) = img.dimensions();
        let started = std::time::Instant::now();
        let found = find_card(img.as_raw(), w, h, ID_CARD_ASPECT);
        let took = started.elapsed();
        println!(
            "{n:2} {w}x{h} {:?} in {took:?}",
            found.map(|f| (f.score, f.corners))
        );
        if std::env::var_os("IAI_CARD_PROBE_TOP").is_some() {
            let full = Photo::new(img.as_raw(), w, h).expect("a photo");
            let small = Small::of(&full).expect("a copy");
            let edges = Edges::of(&small);
            let quads = scored_quads(&edges, &edges.lines(), ID_CARD_ASPECT);
            let only = std::env::var("IAI_CARD_PROBE_ONLY")
                .ok()
                .and_then(|v| v.parse::<usize>().ok());
            let many = if only == Some(n) { 30 } else { 4 };
            for q in quads.iter().take(many) {
                let c = q
                    .corners
                    .map(|(x, y)| ((x * small.scale) as i32, (y * small.scale) as i32));
                let con: Vec<f32> = (0..4)
                    .map(|k| edges.contrast(q.corners[k], q.corners[(k + 1) % 4]))
                    .collect();
                println!(
                    "     {:.3} base {:.3} drawn {} {c:?} contrast {con:.2?}",
                    q.score, q.base, q.drawn
                );
            }
        }
        let Some(found) = found else { continue };
        let map = Homography::onto(found.corners);
        let (cw, ch) = (1011u32, 638u32);
        let card = image::RgbaImage::from_fn(cw, ch, |x, y| {
            let (px, py) = map.to_photo((x as f64 + 0.5) / cw as f64, (y as f64 + 0.5) / ch as f64);
            let (px, py) = (
                px.clamp(0.0, (w - 1) as f64) as u32,
                py.clamp(0.0, (h - 1) as f64) as u32,
            );
            *img.get_pixel(px, py)
        });
        card.save(out.join(format!("{n:02}_the.png")))
            .expect("written");
        let mut marked = img.clone();
        for k in 0..4 {
            let (a, b) = (found.corners[k], found.corners[(k + 1) % 4]);
            for i in 0..=400 {
                let t = i as f32 / 400.0;
                let (x, y) = (a.0 + (b.0 - a.0) * t, a.1 + (b.1 - a.1) * t);
                for d in -2..=2i32 {
                    let (xx, yy) = (
                        (x as i32 + d).clamp(0, w as i32 - 1),
                        (y as i32 + d).clamp(0, h as i32 - 1),
                    );
                    marked.put_pixel(xx as u32, yy as u32, image::Rgba([0, 255, 0, 255]));
                }
            }
        }
        marked
            .save(out.join(format!("{n:02}_khung.png")))
            .expect("written");
    }
}
