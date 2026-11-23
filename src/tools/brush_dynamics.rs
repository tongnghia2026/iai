//! Brush dynamics (Photoshop's Scattering and Shape Dynamics / Transfer
//! jitter): each dab of a Brush stroke may land off the stroke path and come
//! out smaller or with less paint than the options bar says, by a random
//! amount drawn per dab. All off (the default) paints exactly as before.
//!
//! The random numbers come from a small seeded generator, so a stroke is
//! reproducible from its seed (tests rely on that) and needs no global state.

use super::brush::BrushSettings;

/// Largest Scatter: dabs land up to this many tip diameters off the path
/// (Photoshop allows 1000 %; past 5 diameters a stroke is just loose dabs).
pub const MAX_SCATTER: f32 = 5.0;

/// Smallest tip a jittered dab shrinks to, px.
const MIN_JITTER_SIZE: f32 = 1.0;

/// Least paint a jittered dab keeps (a flow of 0 lays nothing).
const MIN_JITTER_FLOW: f32 = 0.01;

/// How far and how much each dab may vary. 0 everywhere is off.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct BrushDynamics {
    /// Largest distance a dab lands off the path, across it, in tip
    /// diameters (0..=`MAX_SCATTER`).
    pub scatter: f32,
    /// Share of the size a dab may lose (0..1).
    pub size_jitter: f32,
    /// Share of the flow a dab may lose (0..1).
    pub flow_jitter: f32,
}

impl BrushDynamics {
    /// Within the ranges the options bar allows; a non-finite value is off.
    pub fn sanitized(self) -> Self {
        let clamp = |v: f32, hi: f32| if v.is_finite() { v.clamp(0.0, hi) } else { 0.0 };
        Self {
            scatter: clamp(self.scatter, MAX_SCATTER),
            size_jitter: clamp(self.size_jitter, 1.0),
            flow_jitter: clamp(self.flow_jitter, 1.0),
        }
    }

    /// Whether every dab paints as the options bar says.
    pub fn is_off(&self) -> bool {
        self.scatter <= 0.0 && self.size_jitter <= 0.0 && self.flow_jitter <= 0.0
    }

    /// One dab aimed at `(x, y)` on a path heading along `dir` (any length;
    /// `None` or zero when the path has no heading, e.g. the press dab or an
    /// airbrush dab): where it lands and the settings it paints with.
    pub fn vary(
        &self,
        settings: &BrushSettings,
        rng: &mut DabRng,
        x: f32,
        y: f32,
        dir: Option<(f32, f32)>,
    ) -> (f32, f32, BrushSettings) {
        let mut s = settings.clone();
        let (mut px, mut py) = (x, y);
        if self.scatter > 0.0 {
            // Across the path, either side; any way when there is no path.
            let (nx, ny) = match dir.and_then(unit) {
                Some((dx, dy)) => (-dy, dx),
                None => {
                    let a = rng.next_f32() * std::f32::consts::TAU;
                    (a.cos(), a.sin())
                }
            };
            let off = (rng.next_f32() * 2.0 - 1.0) * self.scatter * settings.size;
            px += nx * off;
            py += ny * off;
        }
        if self.size_jitter > 0.0 {
            let shrink = 1.0 - self.size_jitter * rng.next_f32();
            s.size = (settings.size * shrink).max(MIN_JITTER_SIZE.min(settings.size));
        }
        if self.flow_jitter > 0.0 {
            let thin = 1.0 - self.flow_jitter * rng.next_f32();
            s.flow = (settings.flow * thin).max(MIN_JITTER_FLOW.min(settings.flow));
        }
        (px, py, s)
    }
}

/// `v` scaled to length 1, `None` when it has no direction.
fn unit((x, y): (f32, f32)) -> Option<(f32, f32)> {
    let len = (x * x + y * y).sqrt();
    (len.is_finite() && len > 1e-6).then(|| (x / len, y / len))
}

/// SplitMix64: tiny, fast, and good enough to pick where a dab lands.
#[derive(Clone, Debug)]
pub struct DabRng(u64);

impl DabRng {
    pub fn new(seed: u64) -> Self {
        Self(seed)
    }

    fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// Uniform in `[0, 1)`.
    pub fn next_f32(&mut self) -> f32 {
        // The top 24 bits: exactly representable in an f32.
        (self.next_u64() >> 40) as f32 / (1u64 << 24) as f32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings() -> BrushSettings {
        BrushSettings {
            size: 20.0,
            flow: 0.8,
            ..BrushSettings::default()
        }
    }

    #[test]
    fn off_paints_every_dab_as_the_options_bar_says() {
        let d = BrushDynamics::default();
        assert!(d.is_off());
        let mut rng = DabRng::new(7);
        let (x, y, s) = d.vary(&settings(), &mut rng, 3.0, 4.0, Some((1.0, 0.0)));
        assert_eq!((x, y), (3.0, 4.0));
        assert_eq!((s.size, s.flow), (20.0, 0.8));
    }

    #[test]
    fn the_generator_is_reproducible_and_stays_in_range() {
        let (mut a, mut b) = (DabRng::new(42), DabRng::new(42));
        let mut c = DabRng::new(43);
        let mut differs = false;
        for _ in 0..1000 {
            let v = a.next_f32();
            assert!((0.0..1.0).contains(&v));
            assert_eq!(v, b.next_f32());
            differs |= v != c.next_f32();
        }
        assert!(differs, "another seed draws other numbers");
    }

    #[test]
    fn scatter_lands_across_the_path_within_reach_on_both_sides() {
        let d = BrushDynamics {
            scatter: 2.0,
            ..BrushDynamics::default()
        };
        let mut rng = DabRng::new(1);
        let reach = 2.0 * 20.0;
        let (mut above, mut below) = (false, false);
        for _ in 0..500 {
            // A path heading right: dabs move up or down only.
            let (x, y, _) = d.vary(&settings(), &mut rng, 50.0, 50.0, Some((5.0, 0.0)));
            assert!((x - 50.0).abs() < 1e-4, "stays on the path's line across");
            assert!((y - 50.0).abs() <= reach + 1e-3);
            above |= y < 40.0;
            below |= y > 60.0;
        }
        assert!(above && below, "dabs land on both sides of the path");
    }

    #[test]
    fn scatter_without_a_heading_spreads_any_way_within_reach() {
        let d = BrushDynamics {
            scatter: 1.0,
            ..BrushDynamics::default()
        };
        let mut rng = DabRng::new(9);
        let mut off_x = false;
        let mut off_y = false;
        for _ in 0..200 {
            let (x, y, _) = d.vary(&settings(), &mut rng, 0.0, 0.0, None);
            assert!((x * x + y * y).sqrt() <= 20.0 + 1e-3);
            off_x |= x.abs() > 2.0;
            off_y |= y.abs() > 2.0;
        }
        assert!(off_x && off_y);
    }

    #[test]
    fn jitter_only_ever_lessens_size_and_flow_and_does_vary() {
        let d = BrushDynamics {
            size_jitter: 0.5,
            flow_jitter: 1.0,
            ..BrushDynamics::default()
        };
        let mut rng = DabRng::new(3);
        let mut sizes = Vec::new();
        for _ in 0..300 {
            let (x, y, s) = d.vary(&settings(), &mut rng, 1.0, 1.0, Some((0.0, 1.0)));
            assert_eq!((x, y), (1.0, 1.0), "no scatter: the dab stays put");
            assert!((10.0..=20.0).contains(&s.size), "size {}", s.size);
            assert!((MIN_JITTER_FLOW..=0.8).contains(&s.flow), "flow {}", s.flow);
            sizes.push(s.size);
        }
        let lo = sizes.iter().copied().fold(f32::MAX, f32::min);
        let hi = sizes.iter().copied().fold(f32::MIN, f32::max);
        assert!(hi - lo > 5.0, "sizes vary ({lo}..{hi})");
    }

    #[test]
    fn a_hand_edited_value_cannot_go_past_the_options_bar() {
        let d = BrushDynamics {
            scatter: 1.0e9,
            size_jitter: -2.0,
            flow_jitter: f32::NAN,
        }
        .sanitized();
        assert_eq!(d.scatter, MAX_SCATTER);
        assert_eq!(d.size_jitter, 0.0);
        assert_eq!(d.flow_jitter, 0.0);
    }
}
