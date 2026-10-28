//! Levels read off a photo's histograms: what Auto Levels stretches.

use iai_doc::layer::LevelsParams;

/// Histograms of red, green, blue and luma, of the pixels that show
/// (alpha above 0).
pub fn histograms(rgba: &[u8]) -> [[u32; 256]; 4] {
    let mut histogram = [[0_u32; 256]; 4];
    for px in rgba.chunks_exact(4) {
        if px[3] == 0 {
            continue;
        }
        histogram[0][px[0] as usize] += 1;
        histogram[1][px[1] as usize] += 1;
        histogram[2][px[2] as usize] += 1;
        let luma = (px[0] as u32 * 77 + px[1] as u32 * 150 + px[2] as u32 * 29) >> 8;
        histogram[3][luma.min(255) as usize] += 1;
    }
    histogram
}

/// The darkest and the lightest level of `histogram` once `clip_percent`
/// (0 to 10) of the pixels are let go at each end; none when there is no
/// range left.
pub fn black_and_white(histogram: &[u32], clip_percent: f32) -> Option<(u8, u8)> {
    let total: u32 = histogram.iter().sum();
    if total == 0 {
        return None;
    }
    let clip = (total as f32 * (clip_percent.clamp(0.0, 10.0) / 100.0)).round() as u32;
    let mut acc = 0_u32;
    let mut black = 0_u8;
    for (i, count) in histogram.iter().copied().enumerate() {
        acc = acc.saturating_add(count);
        if acc > clip {
            black = i as u8;
            break;
        }
    }
    acc = 0;
    let mut white = 255_u8;
    for (i, count) in histogram.iter().copied().enumerate().rev() {
        acc = acc.saturating_add(count);
        if acc > clip {
            white = i as u8;
            break;
        }
    }
    (black < white).then_some((black, white))
}

/// Auto Levels as Photoshop does it: the master channel stretched on the
/// luma (`monochromatic`), or red, green and blue each stretched on its own
/// (which also takes a colour cast away). Levels `[master, red, green, blue]`;
/// none when the photo has no tonal range to stretch.
pub fn auto_levels(
    histograms: &[[u32; 256]; 4],
    monochromatic: bool,
    clip_percent: f32,
) -> Option<[LevelsParams; 4]> {
    let mut channels = [LevelsParams::default(); 4];
    if monochromatic {
        let (black, white) = black_and_white(&histograms[3], clip_percent)?;
        channels[0].in_black = black;
        channels[0].in_white = white;
        return Some(channels);
    }
    let mut any = false;
    for (plane, channel) in [(0, 1), (1, 2), (2, 3)] {
        if let Some((black, white)) = black_and_white(&histograms[plane], clip_percent) {
            channels[channel].in_black = black;
            channels[channel].in_white = white;
            any = true;
        }
    }
    any.then_some(channels)
}

/// Auto Color's last step: a gamma for each of red, green and blue, on top of
/// their stretch in `channels` (`[master, red, green, blue]`), so that the
/// three channels' means meet at their average: a grey comes out grey and a
/// cast in the midtones goes. Each gamma stays within 0.5 to 2; a channel all
/// dark or all light is left as it is.
pub fn neutral_midtones(histograms: &[[u32; 256]; 4], channels: &mut [LevelsParams; 4]) {
    let mean = |plane: usize, p: &LevelsParams| -> Option<f64> {
        let (black, white) = (f64::from(p.in_black), f64::from(p.in_white));
        if white <= black {
            return None;
        }
        let (mut sum, mut count) = (0.0, 0u64);
        for (level, &n) in histograms[plane].iter().enumerate() {
            let x = ((level as f64 - black) / (white - black)).clamp(0.0, 1.0);
            sum += x * f64::from(n);
            count += u64::from(n);
        }
        (count > 0).then(|| sum / count as f64)
    };
    let Some(means) = (0..3)
        .map(|plane| mean(plane, &channels[plane + 1]))
        .collect::<Option<Vec<f64>>>()
    else {
        return;
    };
    let usable = |m: f64| (0.02..0.98).contains(&m);
    let target = means.iter().sum::<f64>() / 3.0;
    if !usable(target) {
        return;
    }
    for (plane, &m) in means.iter().enumerate() {
        if usable(m) {
            // m^(1/gamma) = target.
            channels[plane + 1].gamma = (m.ln() / target.ln()).clamp(0.5, 2.0) as f32;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auto_color_takes_a_cast_out_of_the_midtones() {
        // Greys from 20 to 235 with blue a little lifted in the middle.
        let rgba: Vec<u8> = (20..=235u8)
            .flat_map(|v| {
                let lift = ((v as f32 / 255.0 * std::f32::consts::PI).sin() * 30.0) as u8;
                [v, v, v.saturating_add(lift), 255]
            })
            .collect();
        let h = histograms(&rgba);
        let mut channels = auto_levels(&h, false, 0.0).unwrap();
        neutral_midtones(&h, &mut channels);
        let (r, g, b) = (channels[1].gamma, channels[2].gamma, channels[3].gamma);
        assert!(b < 1.0, "blue is darkened in the middle: {b}");
        assert!(
            r > 1.0 && g > 1.0,
            "red and green lifted to meet it: {r} {g}"
        );
        assert!((r - g).abs() < 0.05);
    }

    #[test]
    fn a_dull_photo_is_stretched_end_to_end_per_channel_or_on_its_luma() {
        // Greys from 60 to 180, and a red cast.
        let rgba: Vec<u8> = (0..121u8)
            .flat_map(|i| [60 + i + 20, 60 + i, 60 + i, 255])
            .chain([0, 0, 0, 0])
            .collect();
        let h = histograms(&rgba);
        assert_eq!(h[0].iter().sum::<u32>(), 121, "clear pixels are left out");
        let per = auto_levels(&h, false, 0.0).unwrap();
        assert_eq!((per[1].in_black, per[1].in_white), (80, 200));
        assert_eq!((per[2].in_black, per[2].in_white), (60, 180));
        assert_eq!((per[0].in_black, per[0].in_white), (0, 255));
        let mono = auto_levels(&h, true, 0.0).unwrap();
        assert!(
            mono[0].in_black > 60 && mono[0].in_white < 200,
            "{:?}",
            mono[0]
        );
        assert_eq!((mono[1].in_black, mono[1].in_white), (0, 255));
        // One flat grey has nothing to stretch.
        let flat = histograms(&[128, 128, 128, 255].repeat(10));
        assert_eq!(auto_levels(&flat, false, 0.1), None);
    }
}
