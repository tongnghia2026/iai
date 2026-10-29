use super::*;

/// Cheap repeatable noise, 0 to 1.
fn noise(n: u32) -> f32 {
    let mut n = n.wrapping_mul(2_654_435_761) ^ 0x5bd1_e995;
    n = (n ^ (n >> 15)).wrapping_mul(2_246_822_519);
    ((n ^ (n >> 13)) & 0xffff) as f32 / 65535.0
}

const CARD_W: usize = 428;
const CARD_H: usize = 270;
/// Where the holder's own details go on the made-up card, in its pixels.
const OWN: [f32; 4] = [20.0, 90.0, 130.0, 240.0];

/// A made-up card, upright and flat, 5 px a millimetre: blocks of print on
/// a pale ground, and in `OWN` what `holder` makes different.
fn card(holder: u32) -> Vec<u8> {
    let mut px = vec![226u8; CARD_W * CARD_H];
    let mut block = |x0: usize, y0: usize, bw: usize, bh: usize, v: u8| {
        for y in y0..(y0 + bh).min(CARD_H) {
            for x in x0..(x0 + bw).min(CARD_W) {
                px[y * CARD_W + x] = v;
            }
        }
    };
    for k in 0..140u32 {
        let x = (noise(4 * k) * (CARD_W - 20) as f32) as usize + 6;
        let y = (noise(4 * k + 1) * (CARD_H - 20) as f32) as usize + 6;
        let (bw, bh) = (
            3 + (noise(4 * k + 2) * 30.0) as usize,
            2 + (noise(4 * k + 3) * 12.0) as usize,
        );
        block(x, y, bw, bh, (30.0 + noise(9 * k + 7) * 140.0) as u8);
    }
    let [x0, y0, x1, y1] = OWN.map(|v| v as usize);
    block(x0, y0, x1 - x0, y1 - y0, 200);
    for k in 0..30u32 {
        let x = x0 + (noise(holder * 977 + 3 * k) * (x1 - x0 - 12) as f32) as usize;
        let y = y0 + (noise(holder * 977 + 3 * k + 1) * (y1 - y0 - 12) as f32) as usize;
        block(x, y, 4 + (noise(holder * 31 + k) * 10.0) as usize, 6, 60);
    }
    px
}

fn print_of_card() -> CardPrint {
    CardPrint::new(card(1), CARD_W as u32, CARD_H as u32, &[OWN]).expect("a print")
}

/// A `w` × `h` photo of another holder's card lying at `quad` (the card's
/// top-left, top-right, bottom-right, bottom-left), on a cloth of green and
/// white stripes as pale as the card, and the corners as they fall.
fn photo(w: u32, h: u32, quad: [(f32, f32); 4]) -> Vec<u8> {
    let face = card(7);
    let to = quad.map(|(x, y)| (x as f64, y as f64));
    let from = [
        (-0.5, -0.5),
        (CARD_W as f64 - 0.5, -0.5),
        (CARD_W as f64 - 0.5, CARD_H as f64 - 0.5),
        (-0.5, CARD_H as f64 - 0.5),
    ];
    let back = Projective::fit(&to, &from).expect("a map");
    let mut out = vec![0u8; (w * h * 4) as usize];
    for y in 0..h {
        for x in 0..w {
            let stripe = (x / 40) % 2 == 0;
            let n = (noise(x * 7919 + y * 104_729) * 16.0) as u8;
            let mut c = if stripe {
                [70 + n, 170 + n, 70]
            } else {
                [232, 236 - n, 232]
            };
            if let Some((u, v)) = back.map((x as f64, y as f64))
                && (0.0..CARD_W as f64 - 1.0).contains(&u)
                && (0.0..CARD_H as f64 - 1.0).contains(&v)
            {
                let g = face[v.round() as usize * CARD_W + u.round() as usize];
                c = [g, g, g.saturating_add(6)];
            }
            let o = ((y * w + x) * 4) as usize;
            out[o..o + 4].copy_from_slice(&[c[0], c[1], c[2], 255]);
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
fn a_perspective_is_fitted_exactly_to_four_points_and_closely_to_many() {
    let from = [(0.0, 0.0), (100.0, 0.0), (100.0, 60.0), (0.0, 60.0)];
    let to = [(12.0, 30.0), (140.0, 22.0), (150.0, 118.0), (8.0, 100.0)];
    let h = Projective::fit(&from, &to).expect("a map");
    for (a, b) in from.iter().zip(&to) {
        let m = h.map(*a).expect("a point");
        assert!((m.0 - b.0).hypot(m.1 - b.1) < 1e-6, "{m:?} {b:?}");
    }
    // Many points of the same map, each a little off.
    let many: Vec<(f64, f64)> = (0..60)
        .map(|i| ((i % 10) as f64 * 11.0, (i / 10) as f64 * 12.0))
        .collect();
    let seen: Vec<(f64, f64)> = many
        .iter()
        .enumerate()
        .map(|(i, p)| {
            let m = h.map(*p).expect("a point");
            (
                m.0 + (noise(i as u32) as f64 - 0.5) * 0.4,
                m.1 + (noise(i as u32 + 99) as f64 - 0.5) * 0.4,
            )
        })
        .collect();
    let fitted = Projective::fit(&many, &seen).expect("a map");
    for p in &many {
        let (a, b) = (
            h.map(*p).expect("a point"),
            fitted.map(*p).expect("a point"),
        );
        assert!((a.0 - b.0).hypot(a.1 - b.1) < 0.3, "{a:?} {b:?}");
    }
}

#[test]
fn most_of_many_matches_agreeing_are_told_from_the_rest() {
    let from = [(0.0, 0.0), (100.0, 0.0), (100.0, 60.0), (0.0, 60.0)];
    let to = [(12.0, 30.0), (140.0, 22.0), (150.0, 118.0), (8.0, 100.0)];
    let h = Projective::fit(&from, &to).expect("a map");
    let pairs: Vec<Pair> = (0..200u32)
        .map(|i| {
            let p = (noise(i) as f64 * 100.0, noise(i + 1000) as f64 * 60.0);
            // Two in five matched to somewhere else altogether.
            let q = if i % 5 < 2 {
                (
                    noise(i + 2000) as f64 * 160.0,
                    noise(i + 3000) as f64 * 130.0,
                )
            } else {
                h.map(p).expect("a point")
            };
            (p, q)
        })
        .collect();
    let (found, n) = ransac(&pairs, 1.0).expect("a map");
    assert!((118..=125).contains(&n), "{n}");
    for p in from {
        let (a, b) = (h.map(p).expect("a point"), found.map(p).expect("a point"));
        assert!((a.0 - b.0).hypot(a.1 - b.1) < 0.05, "{a:?} {b:?}");
    }
}

#[test]
fn a_card_upside_down_on_a_striped_cloth_is_found_by_its_print_the_right_way_up() {
    // Upside down, a little turned, its far side shorter.
    let reads = [
        (780.0, 860.0),
        (232.0, 880.0),
        (250.0, 520.0),
        (742.0, 505.0),
    ];
    let rgba = photo(1000, 1300, reads);
    let found = find_by_print(&rgba, 1000, 1300, &[print_of_card()]).expect("the card");
    assert_eq!(found.print, 0);
    assert_near(found.corners, reads, 2.0);
}

#[test]
fn a_card_partly_out_of_the_photo_is_found_whole() {
    // Its right end off the photo.
    let reads = [
        (420.0, 300.0),
        (1150.0, 330.0),
        (1135.0, 790.0),
        (400.0, 760.0),
    ];
    let rgba = photo(1000, 1100, reads);
    let found = find_by_print(&rgba, 1000, 1100, &[print_of_card()]).expect("the card");
    assert_near(found.corners, reads, 4.0);
}

#[test]
fn a_photo_without_the_print_finds_none() {
    let (w, h) = (900u32, 700u32);
    let rgba: Vec<u8> = (0..w * h)
        .flat_map(|i| {
            let (x, y) = (i % w, i / w);
            let v =
                if (x / 40) % 2 == 0 { 90 } else { 230 } + (noise(i) * 20.0) as u8 - (y % 3) as u8;
            [v, v, v, 255]
        })
        .collect();
    assert_eq!(find_by_print(&rgba, w, h, &[print_of_card()]), None);
}

#[test]
fn corners_found_by_the_edges_are_put_the_way_the_card_reads() {
    let reads = [
        (780.0, 860.0),
        (232.0, 880.0),
        (250.0, 520.0),
        (742.0, 505.0),
    ];
    let rgba = photo(1000, 1300, reads);
    // As the edge finder gives them: the long side higher up first.
    let lying = [reads[2], reads[3], reads[0], reads[1]];
    let found = upright(&rgba, 1000, 1300, lying, &[print_of_card()]).expect("the card");
    assert_eq!(found.corners, reads);
}
