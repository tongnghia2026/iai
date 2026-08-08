//! Colour-mixer selection model: the periodic-RBF Lagrange basis that maps the
//! 8 band sliders to a smooth per-hue curve, the saturation-confidence weighting,
//! and the per-pixel band adjustments. A pixel is selected by its UCS-22 hue
//! through the curve, then weighted by HSV saturation so neutrals take no edit.

use super::*;

/// Static interpolation basis for the mixer curves: the periodic-RBF Lagrange
/// matrix mapping the 8 node VALUES to `MIXER_CURVE_RES` curve samples —
/// `curve[i] = Σ_k lagrange[i][k] · node_k`, with nodes at the UCS hue of the
/// band swatches. Row-of-unit-vector k is exactly the "band k edited alone"
/// gate shape. Node positions never change, so this is built once.
struct MixerBasis {
    lagrange: Vec<[f32; MIXER_BANDS]>,
}

/// Periodic RBF kernel (positive-definite on the circle): a truncated cosine
/// series in the exponent, matching the reference module's interpolation.
fn rbf_kernel(d: f32) -> f32 {
    let mut s = 0.0f32;
    for l in 0..MIXER_RBF_TERMS {
        let lf = l as f32;
        s += (-(lf * lf) / MIXER_RBF_SMOOTHING).exp() * (lf * d).cos();
    }
    s.exp()
}

/// Hue of LUT entry `i`, radians in [−π, π) — the layout `curve_sample` indexes.
fn curve_entry_hue(i: usize) -> f32 {
    (i as f32) * std::f32::consts::TAU / (MIXER_CURVE_RES as f32) - std::f32::consts::PI
}

fn mixer_basis() -> &'static MixerBasis {
    static BASIS: std::sync::OnceLock<MixerBasis> = std::sync::OnceLock::new();
    BASIS.get_or_init(|| {
        let node_hues: Vec<f32> = MIXER_COLORS
            .iter()
            .map(|c| {
                crate::core::ucs::ucs_hue_rad(
                    c[0] as f32 / 255.0,
                    c[1] as f32 / 255.0,
                    c[2] as f32 / 255.0,
                )
            })
            .collect();
        // K[i][j] = kernel(node_i − node_j), then invert (f64 Gauss-Jordan with
        // partial pivoting — K is symmetric positive-definite on distinct nodes).
        let mut a = [[0.0f64; MIXER_BANDS * 2]; MIXER_BANDS];
        for i in 0..MIXER_BANDS {
            for j in 0..MIXER_BANDS {
                a[i][j] = rbf_kernel(node_hues[i] - node_hues[j]) as f64;
            }
            a[i][MIXER_BANDS + i] = 1.0;
        }
        for col in 0..MIXER_BANDS {
            let piv = (col..MIXER_BANDS)
                .max_by(|&x, &y| a[x][col].abs().partial_cmp(&a[y][col].abs()).unwrap())
                .unwrap();
            a.swap(col, piv);
            let p = a[col][col];
            for v in a[col].iter_mut() {
                *v /= p;
            }
            for row in 0..MIXER_BANDS {
                if row != col {
                    let f = a[row][col];
                    for k in 0..MIXER_BANDS * 2 {
                        a[row][k] -= f * a[col][k];
                    }
                }
            }
        }
        let lagrange = (0..MIXER_CURVE_RES)
            .map(|i| {
                let hue = curve_entry_hue(i);
                let mut row = [0.0f32; MIXER_BANDS];
                for (k, slot) in row.iter_mut().enumerate() {
                    let mut v = 0.0f64;
                    for (j, &nh) in node_hues.iter().enumerate() {
                        v += rbf_kernel(hue - nh) as f64 * a[j][MIXER_BANDS + k];
                    }
                    *slot = v as f32;
                }
                row
            })
            .collect();
        MixerBasis { lagrange }
    })
}

/// The three interpolated slider curves over UCS hue, plus the re-gate LUT.
pub(crate) struct MixerCurves {
    pub(crate) hue: Vec<f32>,
    pub(crate) sat: Vec<f32>,
    pub(crate) lum: Vec<f32>,
    /// 0..1 membership of the EDITED bands' hues for the full-res anti-bleed
    /// re-gate — the WGSL twin samples this exact table (uploaded per frame).
    pub(crate) gate: Vec<f32>,
}

/// Interpolate the band sliders into smooth periodic curves; `None` when no
/// band slider is engaged (callers then skip the mixer entirely).
pub(crate) fn build_mixer_curves_opt(settings: &DevelopSettings) -> Option<MixerCurves> {
    let any = settings.mixer_hue.iter().any(|v| v.abs() > 0.001)
        || settings.mixer_saturation.iter().any(|v| v.abs() > 0.001)
        || settings.mixer_luminance.iter().any(|v| v.abs() > 0.001);
    if !any {
        return None;
    }
    let basis = mixer_basis();
    let edited = mixer_edit_mask(settings);
    let mut hue = vec![0.0f32; MIXER_CURVE_RES];
    let mut sat = vec![0.0f32; MIXER_CURVE_RES];
    let mut lum = vec![0.0f32; MIXER_CURVE_RES];
    let mut gate = vec![0.0f32; MIXER_CURVE_RES];
    for i in 0..MIXER_CURVE_RES {
        let l = &basis.lagrange[i];
        let mut h = 0.0f32;
        let mut s = 0.0f32;
        let mut b = 0.0f32;
        let mut g = 0.0f32;
        for k in 0..MIXER_BANDS {
            h += l[k] * settings.mixer_hue[k];
            s += l[k] * settings.mixer_saturation[k];
            b += l[k] * settings.mixer_luminance[k];
            if edited.is_some_and(|e| e[k]) {
                g += l[k];
            }
        }
        hue[i] = h;
        sat[i] = s;
        lum[i] = b;
        gate[i] = g.clamp(0.0, 1.0);
    }
    // Clamp the Saturation and Luminance curves to the SPAN of their slider
    // node values (the Sat/Lum curves are clipped to their node span). The periodic-RBF
    // interpolation overshoots/undershoots between nodes, so a single positive
    // node (e.g. Red +Lum) dips slightly NEGATIVE at a neighbouring hue —
    // which silently DARKENS/greys colours the user never touched (warm
    // highlights under a red edit went grey). Clamping to [min_node, max_node]
    // kills the sign-flip while leaving the intended in-span shape. Hue is left
    // free (an offset, not a gain — it is left unclipped).
    clamp_curve_to_node_span(&mut sat, &settings.mixer_saturation);
    clamp_curve_to_node_span(&mut lum, &settings.mixer_luminance);
    Some(MixerCurves {
        hue,
        sat,
        lum,
        gate,
    })
}

/// Clamp an interpolated mixer curve to the `[min, max]` span of the slider
/// values that built it, so RBF over/undershoot cannot flip the correction's
/// sign at hues between the edited nodes.
fn clamp_curve_to_node_span(curve: &mut [f32], nodes: &[f32; MIXER_BANDS]) {
    let mut lo = 0.0f32;
    let mut hi = 0.0f32;
    for &n in nodes {
        lo = lo.min(n);
        hi = hi.max(n);
    }
    for v in curve.iter_mut() {
        *v = v.clamp(lo, hi);
    }
}

/// Sample a periodic curve LUT at hue `h` (radians, any range): linear
/// interpolation with wrap-around, mirrored by the WGSL gate-LUT sampler.
pub(crate) fn curve_sample(lut: &[f32], h: f32) -> f32 {
    let n = lut.len();
    let t = (h + std::f32::consts::PI).rem_euclid(std::f32::consts::TAU) / std::f32::consts::TAU
        * n as f32;
    let i = (t as usize).min(n - 1);
    let f = t - i as f32;
    let a = lut[i];
    let b = lut[(i + 1) % n];
    a + (b - a) * f
}

/// HSV saturation (delta/max): luma-normalised colour confidence — high for
/// dark saturated colours, ≈0 for greys of any lightness.
pub(crate) fn hsv_saturation(r: f32, g: f32, b: f32) -> f32 {
    let max = r.max(g).max(b);
    let delta = max - r.min(g).min(b);
    if max > 1e-4 && delta > 1e-6 {
        delta / max
    } else {
        0.0
    }
}

/// Logistic saturation weight (0..1) around a shifted midpoint. Mirrored by
/// WGSL `dev_satweight`.
fn mixer_satweight(x: f32) -> f32 {
    1.0 / (1.0 + (-MIXER_SAT_STEEP * x).exp())
}

/// Full colour-confidence weight for one pixel: the logistic HSV-saturation
/// weight (shifted midpoint per edit type) × the absolute-delta gate (see
/// `MIXER_DELTA_LO`). Mirrored by WGSL `dev_mixer_weight`.
pub(crate) fn mixer_weight(r: f32, g: f32, b: f32, shift: f32) -> f32 {
    let delta = r.max(g).max(b) - r.min(g).min(b);
    mixer_satweight(hsv_saturation(r, g, b) - shift)
        * smootherstep(MIXER_DELTA_LO, MIXER_DELTA_HI, delta)
}

/// Lower confidence floor for negative Saturation. A pale colour cast still
/// has a meaningful hue and must be removable, while exact/near greys remain
/// protected by the absolute-delta gate.
pub(crate) fn mixer_desat_weight(r: f32, g: f32, b: f32) -> f32 {
    let delta = r.max(g).max(b) - r.min(g).min(b);
    mixer_satweight(hsv_saturation(r, g, b) - 0.01) * smootherstep(0.002, 0.025, delta)
}

/// Per-pixel selective-colour mixer contribution: the periodic hue curves
/// sampled at the pixel's UCS hue, weighted by its saturation (Luminance
/// additionally keeps the shadow/highlight guard against black speckles and
/// highlight wash). Returns hue/sat/luminance control deltas. Runs on the CPU
/// for every path (the GPU preview consumes the CPU-baked `adjusted` proxy),
/// so the model is GPU-parity-exact by construction; only the spatial re-gate
/// is mirrored, via the shared gate LUT.
pub(crate) fn mixer_adjustments_for_color(
    curves: &MixerCurves,
    r: f32,
    g: f32,
    b: f32,
    luma: f32,
) -> (f32, f32, f32) {
    let h = crate::core::ucs::ucs_hue_rad(r, g, b);
    let w = mixer_weight(r, g, b, MIXER_SAT_SHIFT);
    let lum_guard = smootherstep(LUM_BLACK_LO, LUM_BLACK_HI, luma)
        * (1.0 - smootherstep(LUM_WHITE_LO, LUM_WHITE_HI, luma));
    let wl = mixer_weight(r, g, b, MIXER_BRIGHT_SHIFT) * lum_guard;
    let sat = curve_sample(&curves.sat, h);
    let sat_w = if sat < 0.0 {
        mixer_desat_weight(r, g, b)
    } else {
        w
    };
    (
        (curve_sample(&curves.hue, h) * w).clamp(-CONTROL_LIMIT, CONTROL_LIMIT),
        (sat * sat_w).clamp(-CONTROL_LIMIT, CONTROL_LIMIT),
        (curve_sample(&curves.lum, h) * wl).clamp(-CONTROL_LIMIT, CONTROL_LIMIT),
    )
}

/// Test-only view of the per-band membership: the Lagrange basis (the shape a
/// lone edit of band k takes) sampled at the pixel's UCS hue, times the colour
/// confidence weight — i.e. "how much of band k's edit would this pixel take".
#[cfg(test)]
pub(crate) fn mixer_band_memberships(r: f32, g: f32, b: f32) -> [f32; MIXER_BANDS] {
    let basis = mixer_basis();
    let h = crate::core::ucs::ucs_hue_rad(r, g, b);
    let w = mixer_weight(r, g, b, MIXER_SAT_SHIFT);
    let n = MIXER_CURVE_RES;
    let t = (h + std::f32::consts::PI).rem_euclid(std::f32::consts::TAU) / std::f32::consts::TAU
        * n as f32;
    let i = (t as usize).min(n - 1);
    let f = t - i as f32;
    let mut out = [0.0f32; MIXER_BANDS];
    for (k, slot) in out.iter_mut().enumerate() {
        let a = basis.lagrange[i][k];
        let bb = basis.lagrange[(i + 1) % n][k];
        *slot = ((a + (bb - a) * f) * w).clamp(0.0, 1.0);
    }
    out
}

/// Test-only view of the (hue/sat, luminance) edit weights of one pixel.
#[cfg(test)]
pub(crate) fn mixer_edit_weights(r: f32, g: f32, b: f32, luma: f32) -> (f32, f32) {
    let w = mixer_weight(r, g, b, MIXER_SAT_SHIFT);
    let lum_guard = smootherstep(LUM_BLACK_LO, LUM_BLACK_HI, luma)
        * (1.0 - smootherstep(LUM_WHITE_LO, LUM_WHITE_HI, luma));
    (w, mixer_weight(r, g, b, MIXER_BRIGHT_SHIFT) * lum_guard)
}

pub(crate) fn rgb_chroma(r: f32, g: f32, b: f32) -> f32 {
    r.max(g).max(b) - r.min(g).min(b)
}
