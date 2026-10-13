//! Color-model-aware measurement scopes for Develop Engine 2.
//!
//! Scopes read a declared display-domain tap. Linear taps are converted from
//! their actual working primaries and encoded once; encoded sRGB taps are used
//! directly. Scene-domain buffers are rejected because they still require the
//! recipe's render transform and therefore have no meaningful display scope.

use super::{BufferContract, ColorModel, ReferenceDomain};
use crate::core::develop::linear_to_srgb;
use crate::core::working_color::WorkingColorSpace;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScopeResolution {
    pub horizontal_bins: usize,
    pub value_bins: usize,
    pub vectorscope_bins: usize,
}

/// Rectangular, coordinate-preserving source samples for live scopes.
///
/// Histogram proxies may discard transparent pixels because their position is
/// irrelevant. Waveforms and parades cannot: dropping one sample shifts every
/// following x coordinate. This proxy therefore keeps a full sampled raster
/// plus an inclusion mask. It is built once per Develop session and rendered
/// through the current settings at the histogram/scopes throttle cadence.
#[derive(Debug, Clone, PartialEq)]
pub struct ScopeSourceProxy {
    pub width: usize,
    pub height: usize,
    pub pixels: Vec<[f32; 3]>,
    pub included: Vec<bool>,
}

impl ScopeSourceProxy {
    pub(crate) fn sample(
        width: u32,
        height: u32,
        pixel_budget: u64,
        mut sample: impl FnMut(u32, u32) -> ([f32; 3], bool),
    ) -> Self {
        if width == 0 || height == 0 {
            return Self {
                width: 0,
                height: 0,
                pixels: Vec::new(),
                included: Vec::new(),
            };
        }
        let total = width as u64 * height as u64;
        let step = (((total / pixel_budget.max(1)).max(1) as f64).sqrt().ceil() as u32).max(1);
        let sampled_width = width.div_ceil(step) as usize;
        let sampled_height = height.div_ceil(step) as usize;
        let len = sampled_width.saturating_mul(sampled_height);
        let mut pixels = Vec::with_capacity(len);
        let mut included = Vec::with_capacity(len);
        let mut y = 0;
        while y < height {
            let mut x = 0;
            while x < width {
                let (pixel, include) = sample(x, y);
                pixels.push(pixel);
                included.push(include);
                x = x.saturating_add(step);
            }
            y = y.saturating_add(step);
        }
        Self {
            width: sampled_width,
            height: sampled_height,
            pixels,
            included,
        }
    }
}

impl Default for ScopeResolution {
    fn default() -> Self {
        Self {
            horizontal_bins: 256,
            value_bins: 256,
            vectorscope_bins: 256,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScopeError {
    EmptyImage,
    DimensionMismatch,
    CoverageMismatch,
    SceneTapNeedsRenderTransform,
    InvalidResolution,
    NonFiniteSample { index: usize },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DevelopScopes {
    pub horizontal_bins: usize,
    pub value_bins: usize,
    pub vectorscope_bins: usize,
    /// Luma waveform, indexed `[x_bin * value_bins + value_bin]`.
    pub waveform: Vec<u32>,
    /// RGB parade planes, each with the same layout as [`Self::waveform`].
    pub parade: [Vec<u32>; 3],
    /// Cb/Cr density, indexed `[cr_bin * vectorscope_bins + cb_bin]`.
    /// Cr increases upward, so row zero is the positive-Cr edge.
    pub vectorscope: Vec<u32>,
    pub sample_count: u64,
    /// Samples outside the declared tap's sRGB display cube before clamping.
    pub out_of_display_gamut: u64,
}

impl DevelopScopes {
    pub fn waveform_at(&self, x: usize, value: usize) -> u32 {
        x.checked_mul(self.value_bins)
            .and_then(|offset| offset.checked_add(value))
            .and_then(|index| self.waveform.get(index))
            .copied()
            .unwrap_or(0)
    }

    pub fn parade_at(&self, channel: usize, x: usize, value: usize) -> u32 {
        self.parade
            .get(channel)
            .and_then(|plane| {
                x.checked_mul(self.value_bins)
                    .and_then(|offset| offset.checked_add(value))
                    .and_then(|index| plane.get(index))
            })
            .copied()
            .unwrap_or(0)
    }

    pub fn vectorscope_at(&self, cb: usize, cr: usize) -> u32 {
        cr.checked_mul(self.vectorscope_bins)
            .and_then(|offset| offset.checked_add(cb))
            .and_then(|index| self.vectorscope.get(index))
            .copied()
            .unwrap_or(0)
    }
}

/// Measure waveform, RGB parade and vectorscope in one pass over a typed tap.
pub fn analyze_display_scopes(
    pixels: &[[f32; 3]],
    width: usize,
    height: usize,
    contract: BufferContract,
    resolution: ScopeResolution,
) -> Result<DevelopScopes, ScopeError> {
    analyze_display_scopes_masked(pixels, None, width, height, contract, resolution)
}

/// Masked variant used by the runtime scope proxy. Excluded samples retain
/// their raster position but do not contribute to density or sample counts.
pub fn analyze_display_scopes_masked(
    pixels: &[[f32; 3]],
    included: Option<&[bool]>,
    width: usize,
    height: usize,
    contract: BufferContract,
    resolution: ScopeResolution,
) -> Result<DevelopScopes, ScopeError> {
    if width == 0 || height == 0 {
        return Err(ScopeError::EmptyImage);
    }
    if pixels.len()
        != width
            .checked_mul(height)
            .ok_or(ScopeError::DimensionMismatch)?
    {
        return Err(ScopeError::DimensionMismatch);
    }
    if included.is_some_and(|mask| mask.len() != pixels.len()) {
        return Err(ScopeError::CoverageMismatch);
    }
    if contract.domain != ReferenceDomain::Display {
        return Err(ScopeError::SceneTapNeedsRenderTransform);
    }
    let bins_valid = |n: usize| (2..=4096).contains(&n);
    if !bins_valid(resolution.horizontal_bins)
        || !bins_valid(resolution.value_bins)
        || !bins_valid(resolution.vectorscope_bins)
    {
        return Err(ScopeError::InvalidResolution);
    }

    let plane_len = resolution
        .horizontal_bins
        .checked_mul(resolution.value_bins)
        .ok_or(ScopeError::InvalidResolution)?;
    let vector_len = resolution
        .vectorscope_bins
        .checked_mul(resolution.vectorscope_bins)
        .ok_or(ScopeError::InvalidResolution)?;
    let mut result = DevelopScopes {
        horizontal_bins: resolution.horizontal_bins,
        value_bins: resolution.value_bins,
        vectorscope_bins: resolution.vectorscope_bins,
        waveform: vec![0; plane_len],
        parade: std::array::from_fn(|_| vec![0; plane_len]),
        vectorscope: vec![0; vector_len],
        sample_count: 0,
        out_of_display_gamut: 0,
    };

    for (index, &pixel) in pixels.iter().enumerate() {
        if included.is_some_and(|mask| !mask[index]) {
            continue;
        }
        if pixel.iter().any(|v| !v.is_finite()) {
            return Err(ScopeError::NonFiniteSample { index });
        }
        result.sample_count = result.sample_count.saturating_add(1);
        let (encoded, out_of_gamut) = to_encoded_srgb(pixel, contract.color);
        result.out_of_display_gamut += u64::from(out_of_gamut);
        let rgb = encoded.map(|v| v.clamp(0.0, 1.0));
        let x = index % width;
        let x_bin = x * resolution.horizontal_bins / width;
        for channel in 0..3 {
            let value_bin = quantize(rgb[channel], resolution.value_bins);
            result.parade[channel][x_bin * resolution.value_bins + value_bin] += 1;
        }

        // Rec.709/sRGB luma on the encoded display tap, matching conventional
        // photo-editor waveform and parade readouts.
        let luma = 0.2126 * rgb[0] + 0.7152 * rgb[1] + 0.0722 * rgb[2];
        let luma_bin = quantize(luma, resolution.value_bins);
        result.waveform[x_bin * resolution.value_bins + luma_bin] += 1;

        // Normalized Rec.709 colour difference. Neutral maps to the centre;
        // saturated primaries spread toward the expected vectorscope targets.
        let cb = (rgb[2] - luma) / (2.0 * (1.0 - 0.0722));
        let cr = (rgb[0] - luma) / (2.0 * (1.0 - 0.2126));
        let cb_bin = quantize(cb + 0.5, resolution.vectorscope_bins);
        let cr_bin = quantize(0.5 - cr, resolution.vectorscope_bins);
        result.vectorscope[cr_bin * resolution.vectorscope_bins + cb_bin] += 1;
    }
    Ok(result)
}

fn quantize(value: f32, bins: usize) -> usize {
    (value.clamp(0.0, 1.0) * (bins - 1) as f32).round() as usize
}

fn to_encoded_srgb(pixel: [f32; 3], color: ColorModel) -> ([f32; 3], bool) {
    let linear = match color {
        ColorModel::EncodedSrgb => {
            let out = pixel.iter().any(|v| !(0.0..=1.0).contains(v));
            return (pixel, out);
        }
        ColorModel::LinearProPhoto => WorkingColorSpace::LinearProPhoto.to_linear_srgb(pixel),
        ColorModel::LinearAcesCg => WorkingColorSpace::AcesCg.to_linear_srgb(pixel),
        ColorModel::LinearSrgb => pixel,
    };
    let out = linear.iter().any(|v| !(0.0..=1.0).contains(v));
    (linear.map(linear_to_srgb), out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::develop::srgb_to_linear;
    use crate::core::develop2::Precision;

    fn resolution(horizontal_bins: usize) -> ScopeResolution {
        ScopeResolution {
            horizontal_bins,
            value_bins: 256,
            vectorscope_bins: 256,
        }
    }

    #[test]
    fn grayscale_ramp_makes_diagonal_waveform_and_center_vector() {
        let pixels = [[0.0; 3], [1.0 / 3.0; 3], [2.0 / 3.0; 3], [1.0; 3]];
        let scopes =
            analyze_display_scopes(&pixels, 4, 1, BufferContract::DISPLAY_SINK, resolution(4))
                .unwrap();
        for (x, value) in [0, 85, 170, 255].into_iter().enumerate() {
            assert_eq!(scopes.waveform_at(x, value), 1);
        }
        let center = 128;
        assert_eq!(scopes.vectorscope_at(center, center), 4);
        assert_eq!(scopes.sample_count, 4);
    }

    #[test]
    fn rgb_bars_land_on_exact_parade_endpoints() {
        let pixels = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];
        let scopes =
            analyze_display_scopes(&pixels, 3, 1, BufferContract::DISPLAY_SINK, resolution(3))
                .unwrap();
        for x in 0..3 {
            for channel in 0..3 {
                let expected = if x == channel { 255 } else { 0 };
                assert_eq!(scopes.parade_at(channel, x, expected), 1);
            }
        }
        assert_eq!(scopes.vectorscope.iter().sum::<u32>(), 3);
    }

    #[test]
    fn declared_linear_and_encoded_taps_measure_identically() {
        let encoded = [[0.21, 0.48, 0.83], [0.72, 0.33, 0.11]];
        let linear = encoded.map(|p| p.map(srgb_to_linear));
        let a = analyze_display_scopes(&encoded, 2, 1, BufferContract::DISPLAY_SINK, resolution(2))
            .unwrap();
        let b =
            analyze_display_scopes(&linear, 2, 1, BufferContract::DISPLAY_LINEAR, resolution(2))
                .unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn wide_working_neutral_uses_its_declared_primaries() {
        let srgb = [0.18; 3];
        for (model, space) in [
            (
                ColorModel::LinearProPhoto,
                WorkingColorSpace::LinearProPhoto,
            ),
            (ColorModel::LinearAcesCg, WorkingColorSpace::AcesCg),
        ] {
            let wide = [space.from_linear_srgb(srgb)];
            let wide_scopes = analyze_display_scopes(
                &wide,
                1,
                1,
                BufferContract::display_linear(model),
                resolution(2),
            )
            .unwrap();
            let srgb_scopes = analyze_display_scopes(
                &[srgb],
                1,
                1,
                BufferContract::DISPLAY_LINEAR,
                resolution(2),
            )
            .unwrap();
            let peak = |plane: &[u32]| plane.iter().position(|&count| count > 0).unwrap();
            assert!(
                peak(&wide_scopes.waveform).abs_diff(peak(&srgb_scopes.waveform)) <= 1,
                "{model:?} neutral waveform drifted by more than one bin"
            );
            for channel in 0..3 {
                assert!(
                    peak(&wide_scopes.parade[channel]).abs_diff(peak(&srgb_scopes.parade[channel]))
                        <= 1,
                    "{model:?} neutral channel {channel} drifted by more than one bin"
                );
            }
            let wide_vector = peak(&wide_scopes.vectorscope);
            let srgb_vector = peak(&srgb_scopes.vectorscope);
            let n = wide_scopes.vectorscope_bins;
            assert!((wide_vector % n).abs_diff(srgb_vector % n) <= 1);
            assert!((wide_vector / n).abs_diff(srgb_vector / n) <= 1);
            assert_eq!(wide_scopes.out_of_display_gamut, 0);
        }
    }

    #[test]
    fn scene_tap_is_rejected_until_rendered() {
        let contract = BufferContract {
            color: ColorModel::LinearProPhoto,
            domain: ReferenceDomain::Scene,
            precision: Precision::F32,
            signed: true,
            bounded: false,
        };
        assert_eq!(
            analyze_display_scopes(&[[0.18; 3]], 1, 1, contract, resolution(2)),
            Err(ScopeError::SceneTapNeedsRenderTransform)
        );
    }

    #[test]
    fn out_of_gamut_and_nonfinite_samples_are_reported() {
        let scopes = analyze_display_scopes(
            &[[1.2, -0.1, 0.4]],
            1,
            1,
            BufferContract::DISPLAY_LINEAR,
            resolution(2),
        )
        .unwrap();
        assert_eq!(scopes.out_of_display_gamut, 1);
        assert_eq!(
            analyze_display_scopes(
                &[[f32::NAN, 0.0, 0.0]],
                1,
                1,
                BufferContract::DISPLAY_SINK,
                resolution(2),
            ),
            Err(ScopeError::NonFiniteSample { index: 0 })
        );
    }

    #[test]
    fn masked_runtime_samples_keep_columns_and_skip_transparency() {
        let pixels = [[0.0; 3], [1.0; 3], [0.5; 3], [0.25; 3]];
        let included = [true, false, true, true];
        let scopes = analyze_display_scopes_masked(
            &pixels,
            Some(&included),
            2,
            2,
            BufferContract::DISPLAY_SINK,
            resolution(2),
        )
        .unwrap();
        assert_eq!(scopes.sample_count, 3);
        assert_eq!(scopes.waveform_at(0, 0), 1);
        assert_eq!(scopes.waveform_at(0, 128), 1);
        assert_eq!(scopes.waveform_at(1, 64), 1);
        assert_eq!(scopes.waveform_at(1, 255), 0);
    }

    #[test]
    fn runtime_accessors_are_bounded_for_bad_ui_coordinates() {
        let scopes = analyze_display_scopes(
            &[[0.5; 3]],
            1,
            1,
            BufferContract::DISPLAY_SINK,
            resolution(2),
        )
        .unwrap();
        assert_eq!(scopes.waveform_at(usize::MAX, usize::MAX), 0);
        assert_eq!(scopes.parade_at(usize::MAX, usize::MAX, usize::MAX), 0);
        assert_eq!(scopes.vectorscope_at(usize::MAX, usize::MAX), 0);
    }
}
