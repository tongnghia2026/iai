use super::*;

/// Cheap repeatable noise, 0 to 1.
fn noise(n: u32) -> f32 {
    let mut n = n.wrapping_mul(2_654_435_761) ^ 0x5bd1_e995;
    n = (n ^ (n >> 15)).wrapping_mul(2_246_822_519);
    ((n ^ (n >> 13)) & 0xffff) as f32 / 65535.0
}

/// A busy picture: blocks and bars of many greys on a pale ground.
fn busy(w: usize, h: usize) -> Gray {
    let mut px = vec![215u8; w * h];
    for k in 0..160u32 {
        let (x0, y0) = (
            (noise(4 * k) * w as f32) as usize,
            (noise(4 * k + 1) * h as f32) as usize,
        );
        let (bw, bh) = (
            4 + (noise(4 * k + 2) * 40.0) as usize,
            3 + (noise(4 * k + 3) * 18.0) as usize,
        );
        let v = (30.0 + noise(7 * k + 5) * 150.0) as u8;
        for y in y0..(y0 + bh).min(h) {
            for x in x0..(x0 + bw).min(w) {
                px[y * w + x] = v;
            }
        }
    }
    Gray { w, h, px }
}

#[test]
fn a_picture_turned_a_quarter_and_made_smaller_is_matched() {
    let a = busy(480, 320);
    // Turned a quarter clockwise, three quarters the size: (x, y) of `a`
    // lands at ((h - 1 - y) · s, x · s).
    let s = 0.75f32;
    let (bw, bh) = ((a.h as f32 * s) as usize, (a.w as f32 * s) as usize);
    let px = (0..bh)
        .flat_map(|v| (0..bw).map(move |u| (u, v)))
        .map(|(u, v)| {
            let x = (v as f32 + 0.5) / s - 0.5;
            let y = a.h as f32 - 1.0 - ((u as f32 + 0.5) / s - 0.5);
            a.at(x.round() as isize, y.round() as isize)
        })
        .collect();
    let b = Gray { w: bw, h: bh, px };
    let fa = features(&a, 1000, &|_, _| true);
    let fb = features(&b, 1000, &|_, _| true);
    assert!(
        fa.len() > 300 && fb.len() > 300,
        "{} {}",
        fa.len(),
        fb.len()
    );
    let found = matches(&fa, &fb, 0.8);
    let right = found
        .iter()
        .filter(|&&(i, j)| {
            let (x, y) = (fa[i].x, fa[i].y);
            let want = ((a.h as f32 - 1.0 - y + 0.5) * s - 0.5, (x + 0.5) * s - 0.5);
            (fb[j].x - want.0).hypot(fb[j].y - want.1) <= 3.0
        })
        .count();
    assert!(right >= 60, "{right} of {} right", found.len());
    assert!(
        right * 10 >= found.len() * 6,
        "{right} of {} right",
        found.len()
    );
}

#[test]
fn nothing_is_found_where_it_is_kept_out_or_flat() {
    let a = busy(400, 300);
    let left = features(&a, 800, &|x, _| x < 200.0);
    assert!(!left.is_empty());
    assert!(left.iter().all(|f| f.x < 200.0 + 2.0));
    let flat = Gray {
        w: 300,
        h: 200,
        px: vec![128; 300 * 200],
    };
    assert!(features(&flat, 500, &|_, _| true).is_empty());
}

#[test]
fn a_photo_is_made_grey_and_small() {
    let (w, h) = (300u32, 100u32);
    let rgba: Vec<u8> = (0..w * h).flat_map(|_| [255u8, 0, 0, 255]).collect();
    let (gray, scale) = Gray::of_rgba(&rgba, w, h, 150).expect("a copy");
    assert_eq!((gray.w, gray.h), (150, 50));
    assert!((scale - 2.0).abs() < 1e-6);
    assert!(
        gray.px.iter().all(|&v| (75..=77).contains(&v)),
        "{:?}",
        &gray.px[..4]
    );
}
