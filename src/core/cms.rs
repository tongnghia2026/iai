//! ICC colour management via Little CMS (lcms2).
//!
//! Single source of truth for ICC profile creation, byte (de)serialization and
//! pixel-space transforms. The document working/display space is sRGB: imported
//! pixels are converted *into* sRGB here, and sRGB (or another working profile)
//! ICC bytes are produced here for embedding on export. Keeping every lcms2 call
//! behind this module means the rest of the app never touches the C API.

use lcms2::{
    CIExyY, CIExyYTRIPLE, Flags, InfoType, Intent, Locale, PixelFormat, Profile, ThreadContext,
    ToneCurve, Transform,
};

/// Rendering intent for document/profile conversions. Relative colorimetric is
/// the conventional choice for RGB→RGB working-space conversion (perceptual is
/// reserved for gamut-compressing print output, which arrives with print/Phase C).
pub const DEFAULT_INTENT: Intent = Intent::RelativeColorimetric;

/// A named working/display profile the user can pick (Assign Profile, etc.).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkingProfile {
    Srgb,
    AdobeRgb,
    DisplayP3,
    ProPhoto,
}

impl WorkingProfile {
    pub fn name(self) -> &'static str {
        match self {
            WorkingProfile::Srgb => "sRGB IEC61966-2.1",
            WorkingProfile::AdobeRgb => "Adobe RGB (1998)",
            WorkingProfile::DisplayP3 => "Display P3",
            WorkingProfile::ProPhoto => "ProPhoto RGB",
        }
    }

    pub fn profile(self) -> Profile {
        match self {
            WorkingProfile::Srgb => srgb_profile(),
            WorkingProfile::AdobeRgb => adobe_rgb_profile(),
            WorkingProfile::DisplayP3 => display_p3_profile(),
            WorkingProfile::ProPhoto => prophoto_profile(),
        }
    }

    pub fn all() -> &'static [WorkingProfile] {
        &[
            WorkingProfile::Srgb,
            WorkingProfile::AdobeRgb,
            WorkingProfile::DisplayP3,
            WorkingProfile::ProPhoto,
        ]
    }
}

/// Built-in sRGB working-space profile.
pub fn srgb_profile() -> Profile {
    Profile::new_srgb()
}

/// Adobe RGB (1998), synthesized from its published D65 white point, primaries
/// and 2.2 gamma so we don't have to ship a binary `.icc` resource.
pub fn adobe_rgb_profile() -> Profile {
    let white = CIExyY {
        x: 0.3127,
        y: 0.3290,
        Y: 1.0,
    };
    let primaries = CIExyYTRIPLE {
        Red: CIExyY {
            x: 0.6400,
            y: 0.3300,
            Y: 1.0,
        },
        Green: CIExyY {
            x: 0.2100,
            y: 0.7100,
            Y: 1.0,
        },
        Blue: CIExyY {
            x: 0.1500,
            y: 0.0600,
            Y: 1.0,
        },
    };
    let gamma = ToneCurve::new(2.19921875);
    Profile::new_rgb(&white, &primaries, &[&gamma, &gamma, &gamma])
        .unwrap_or_else(|_| srgb_profile())
}

/// Display P3 (D65, P3 primaries). The synthesized profile uses a 2.2 curve;
/// image import/export still delegates the exact profile transform to lcms.
pub fn display_p3_profile() -> Profile {
    rgb_profile(
        (0.3127, 0.3290),
        [(0.680, 0.320), (0.265, 0.690), (0.150, 0.060)],
        2.2,
    )
}

/// ProPhoto RGB (D50 primaries, 1.8 transfer curve).
pub fn prophoto_profile() -> Profile {
    rgb_profile(
        (0.3457, 0.3585),
        [(0.7347, 0.2653), (0.1596, 0.8404), (0.0366, 0.0001)],
        1.8,
    )
}

fn rgb_profile(white_xy: (f64, f64), xy: [(f64, f64); 3], gamma: f64) -> Profile {
    let white = CIExyY {
        x: white_xy.0,
        y: white_xy.1,
        Y: 1.0,
    };
    let primary = |p: (f64, f64)| CIExyY {
        x: p.0,
        y: p.1,
        Y: 1.0,
    };
    let primaries = CIExyYTRIPLE {
        Red: primary(xy[0]),
        Green: primary(xy[1]),
        Blue: primary(xy[2]),
    };
    let curve = ToneCurve::new(gamma);
    Profile::new_rgb(&white, &primaries, &[&curve, &curve, &curve])
        .unwrap_or_else(|_| srgb_profile())
}

/// Load an embedded ICC profile from raw bytes.
pub fn profile_from_bytes(bytes: &[u8]) -> Option<Profile> {
    Profile::new_icc(bytes).ok()
}

/// True if the ICC profile's data colour space is RGB (vs CMYK/Gray/Lab/…).
pub fn profile_is_rgb(icc: &[u8]) -> bool {
    profile_from_bytes(icc)
        .map(|p| p.color_space() == lcms2::ColorSpaceSignature::RgbData)
        .unwrap_or(false)
}

/// Convert an sRGB RGBA8 buffer to another RGB device/working profile in place
/// (alpha preserved). `dst_icc` must be an RGB profile. Returns false on failure.
pub fn convert_srgb_to_rgb_profile(buf: &mut [u8], dst_icc: &[u8], intent: Intent) -> bool {
    match profile_from_bytes(dst_icc) {
        Some(dst) => convert_rgba8(buf, &srgb_profile(), &dst, intent),
        None => false,
    }
}

/// True if the ICC profile's data colour space is CMYK.
pub fn profile_is_cmyk(icc: &[u8]) -> bool {
    profile_from_bytes(icc)
        .map(|p| p.color_space() == lcms2::ColorSpaceSignature::CmykData)
        .unwrap_or(false)
}

/// Convert a packed sRGB RGB8 buffer (no alpha) to CMYK8 (4 bytes/pixel: C,M,Y,K)
/// through a CMYK device profile. Used for separations / CMYK export. Returns
/// `None` if `cmyk_icc` isn't a usable CMYK profile.
pub fn srgb_rgb_to_cmyk8(rgb: &[u8], cmyk_icc: &[u8], intent: Intent) -> Option<Vec<u8>> {
    if rgb.is_empty() || rgb.len() % 3 != 0 {
        return None;
    }
    let n = rgb.len() / 3;
    let cmyk = profile_from_bytes(cmyk_icc)?;
    let srgb = srgb_profile();
    let t: Transform<[u8; 3], [u8; 4]> = Transform::new(
        &srgb,
        PixelFormat::RGB_8,
        &cmyk,
        PixelFormat::CMYK_8,
        intent,
    )
    .ok()?;
    let src: &[[u8; 3]] = bytemuck::cast_slice(rgb);
    let mut dst = vec![[0u8; 4]; n];
    t.transform_pixels(src, &mut dst);
    Some(bytemuck::cast_slice(&dst).to_vec())
}

/// Convert a packed CMYK8 buffer (4 bytes/pixel: C,M,Y,K) to packed sRGB RGB8
/// (no alpha) through a CMYK device profile — the display/mirror direction of
/// [`srgb_rgb_to_cmyk8`]. Display now goes through [`CmykConverter`]; kept as
/// the reference implementation its tests check against.
#[cfg(test)]
pub fn cmyk8_to_srgb_rgb(cmyk: &[u8], cmyk_icc: &[u8], intent: Intent) -> Option<Vec<u8>> {
    if cmyk.is_empty() || cmyk.len() % 4 != 0 {
        return None;
    }
    let n = cmyk.len() / 4;
    let profile = profile_from_bytes(cmyk_icc)?;
    if profile.color_space() != lcms2::ColorSpaceSignature::CmykData {
        return None;
    }
    let srgb = srgb_profile();
    let t: Transform<[u8; 4], [u8; 3]> = Transform::new(
        &profile,
        PixelFormat::CMYK_8,
        &srgb,
        PixelFormat::RGB_8,
        intent,
    )
    .ok()?;
    let src: &[[u8; 4]] = bytemuck::cast_slice(cmyk);
    let mut dst = vec![[0u8; 3]; n];
    t.transform_pixels(src, &mut dst);
    Some(bytemuck::cast_slice(&dst).to_vec())
}

// ── CMYK editing (ink planes) ────────────────────────────────────────────────
//
// A CMYK document stores C,M,Y,K ink bytes as ground truth and keeps an sRGB
// mirror for display/compositing. The built-in "Generic CMYK (naive)" space is
// a max-K GCR chosen so the RGB→CMYK→RGB round trip is *exactly* lossless in
// u8 (picking a colour and painting it shows that exact colour back). It is
// not a print-accurate rendering — real output should use an ICC device profile.

/// Naive max-K GCR, RGB→CMYK. With `max = max(r,g,b)`:
/// `k = 255 - max`, `c = round(255·(max−r)/max)` (m,y likewise; the `1/(1−k)`
/// factor of the classic formula cancels into unit-free integer math).
#[inline]
pub fn naive_rgb_to_cmyk(rgb: [u8; 3]) -> [u8; 4] {
    let [r, g, b] = rgb;
    let max = r.max(g).max(b);
    if max == 0 {
        return [0, 0, 0, 255];
    }
    let m = max as u32;
    let enc = |v: u8| (((m - v as u32) * 255 + m / 2) / m) as u8;
    [enc(r), enc(g), enc(b), 255 - max]
}

/// Naive max-K GCR, CMYK→RGB: `r = round((255−c)·(255−k)/255)` (g,b likewise).
/// Exact inverse of [`naive_rgb_to_cmyk`]: the encode error `|e| ≤ 0.5` shrinks
/// by `max/255 < 1` on decode, so the result always rounds back to the input.
#[inline]
pub fn naive_cmyk_to_rgb(cmyk: [u8; 4]) -> [u8; 3] {
    let [c, m, y, k] = cmyk;
    let max = (255 - k) as u32;
    let dec = |v: u8| (((255 - v as u32) * max + 127) / 255) as u8;
    [dec(c), dec(m), dec(y)]
}

/// Two-way RGB↔CMYK converter with prebuilt transforms, built once per
/// document/stroke and used for batch (tile/bbox) conversion. Do NOT share one
/// instance across rayon workers — lcms2 `Transform`'s thread-safety bounds are
/// unverified here; build one per thread instead if that ever becomes needed.
pub enum CmykConverter {
    /// Built-in "Generic CMYK (naive)" max-K GCR (exactly invertible, not print-accurate).
    Naive,
    /// ICC CMYK device profile, both directions prebuilt.
    Icc {
        to_cmyk: Transform<[u8; 3], [u8; 4]>,
        to_rgb: Transform<[u8; 4], [u8; 3]>,
    },
}

impl CmykConverter {
    /// Build from ICC bytes; `None` if the profile isn't CMYK or a transform fails.
    pub fn from_icc_bytes(cmyk_icc: &[u8], intent: Intent) -> Option<Self> {
        let profile = profile_from_bytes(cmyk_icc)?;
        if profile.color_space() != lcms2::ColorSpaceSignature::CmykData {
            return None;
        }
        let srgb = srgb_profile();
        let to_cmyk: Transform<[u8; 3], [u8; 4]> = Transform::new(
            &srgb,
            PixelFormat::RGB_8,
            &profile,
            PixelFormat::CMYK_8,
            intent,
        )
        .ok()?;
        let to_rgb: Transform<[u8; 4], [u8; 3]> = Transform::new(
            &profile,
            PixelFormat::CMYK_8,
            &srgb,
            PixelFormat::RGB_8,
            intent,
        )
        .ok()?;
        Some(CmykConverter::Icc { to_cmyk, to_rgb })
    }

    pub fn rgb_to_cmyk_one(&self, rgb: [u8; 3]) -> [u8; 4] {
        match self {
            CmykConverter::Naive => naive_rgb_to_cmyk(rgb),
            CmykConverter::Icc { to_cmyk, .. } => {
                let mut out = [[0u8; 4]];
                to_cmyk.transform_pixels(&[rgb], &mut out);
                out[0]
            }
        }
    }

    pub fn cmyk_to_rgb_one(&self, cmyk: [u8; 4]) -> [u8; 3] {
        match self {
            CmykConverter::Naive => naive_cmyk_to_rgb(cmyk),
            CmykConverter::Icc { to_rgb, .. } => {
                let mut out = [[0u8; 3]];
                to_rgb.transform_pixels(&[cmyk], &mut out);
                out[0]
            }
        }
    }

    /// Batch RGB→CMYK. `rgb` and `out` must be the same pixel count.
    pub fn rgb_to_cmyk_slice(&self, rgb: &[[u8; 3]], out: &mut [[u8; 4]]) {
        debug_assert_eq!(rgb.len(), out.len());
        match self {
            CmykConverter::Naive => {
                for (src, dst) in rgb.iter().zip(out.iter_mut()) {
                    *dst = naive_rgb_to_cmyk(*src);
                }
            }
            CmykConverter::Icc { to_cmyk, .. } => to_cmyk.transform_pixels(rgb, out),
        }
    }

    /// Batch CMYK→RGB. `cmyk` and `out` must be the same pixel count.
    pub fn cmyk_to_rgb_slice(&self, cmyk: &[[u8; 4]], out: &mut [[u8; 3]]) {
        debug_assert_eq!(cmyk.len(), out.len());
        match self {
            CmykConverter::Naive => {
                for (src, dst) in cmyk.iter().zip(out.iter_mut()) {
                    *dst = naive_cmyk_to_rgb(*src);
                }
            }
            CmykConverter::Icc { to_rgb, .. } => to_rgb.transform_pixels(cmyk, out),
        }
    }
}

// ── Generic CMYK ICC synthesis ───────────────────────────────────────────────
//
// Exported CMYK PDFs used to embed raw DeviceCMYK with no colour space, so every
// viewer/press interpreted the ink with its OWN default CMYK→RGB model — visibly
// different from the app's on-screen preview. To make the export match the app we
// tag the DeviceCMYK content with a CMYK ICC profile whose device→display
// transform reproduces the app's converter. The built-in "Generic CMYK (naive)"
// space has no vendor profile, so synthesize one: a standard ICC v4 CMYK output
// profile whose AToB0 CLUT maps naive ink straight to the Lab of
// [`naive_cmyk_to_rgb`]. A 17⁴ grid reproduces the model within ~1/255.

/// Grid points per CMYK axis in the synthesized profile's AToB0 CLUT.
const GENERIC_CMYK_GRID: usize = 17;

/// The synthesized "Generic CMYK (naive)" ICC profile bytes, built once and
/// cached (~0.5 MB). Empty if lcms could not assemble the profile — callers then
/// skip tagging and fall back to plain DeviceCMYK (no worse than before).
pub fn generic_cmyk_icc_bytes() -> &'static [u8] {
    static BYTES: std::sync::OnceLock<Vec<u8>> = std::sync::OnceLock::new();
    BYTES.get_or_init(|| build_generic_cmyk_icc().unwrap_or_default())
}

fn build_generic_cmyk_icc() -> Option<Vec<u8>> {
    use foreign_types_shared::ForeignType;
    use lcms2::{
        ColorSpaceSignature, GlobalContext, Pipeline, ProfileClassSignature, Stage, Tag,
        TagSignature, CIEXYZ, MLU,
    };

    let d50 = CIExyY {
        x: 0.3457,
        y: 0.3585,
        Y: 1.0,
    };
    let srgb = srgb_profile();
    let lab = Profile::new_lab4_context(GlobalContext::new(), &d50).ok()?;
    // sRGB(RGB_8) → Lab_16 yields lcms's own 16-bit Lab encoding, matched to the
    // 16-bit CLUT the profile stores (no hand-rolled Lab encoding to get wrong).
    let to_lab: Transform<[u8; 3], [u16; 3]> = Transform::new(
        &srgb,
        PixelFormat::RGB_8,
        &lab,
        PixelFormat::Lab_16,
        Intent::RelativeColorimetric,
    )
    .ok()?;

    let n = GENERIC_CMYK_GRID;
    let node = |i: usize| (i as f32 / (n - 1) as f32 * 255.0).round() as u8;
    // CLUT node order: first input channel (C) slowest, last (K) fastest.
    let mut rgb = Vec::with_capacity(n * n * n * n);
    for ci in 0..n {
        for mi in 0..n {
            for yi in 0..n {
                for ki in 0..n {
                    rgb.push(naive_cmyk_to_rgb([node(ci), node(mi), node(yi), node(ki)]));
                }
            }
        }
    }
    let mut lab_nodes = vec![[0u16; 3]; rgb.len()];
    to_lab.transform_pixels(&rgb, &mut lab_nodes);
    let table: Vec<u16> = lab_nodes.into_iter().flatten().collect();

    let clut = Stage::new_clut::<u16>(n, 4, 3, Some(&table)).ok()?;
    // lcms's mAB (lutAToB) writer only serializes the full canonical structure:
    // A-curves, CLUT, M-curves, Matrix, B-curves. All but the CLUT are identity.
    let ident = ToneCurve::new(1.0);
    let acurves = Stage::new_tone_curves(&[&ident, &ident, &ident, &ident]).ok()?;
    let mcurves = Stage::new_tone_curves(&[&ident, &ident, &ident]).ok()?;
    let bcurves = Stage::new_tone_curves(&[&ident, &ident, &ident]).ok()?;
    let matrix = Stage::new_matrix(
        &[1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0],
        3,
        3,
        Some(&[0.0, 0.0, 0.0]),
    )
    .ok()?;
    let pipeline = Pipeline::new(4, 3).ok()?;
    // SAFETY: each stage is freshly allocated above and moved in via into_ptr();
    // cmsPipelineInsertStage takes ownership, and the pipeline frees them on drop.
    unsafe {
        for stage in [
            acurves.into_ptr(),
            clut.into_ptr(),
            mcurves.into_ptr(),
            matrix.into_ptr(),
            bcurves.into_ptr(),
        ] {
            if lcms2_sys::cmsPipelineInsertStage(
                pipeline.as_ptr(),
                lcms2_sys::StageLoc::AT_END,
                stage,
            ) == 0
            {
                return None;
            }
        }
    }

    let mut profile = Profile::new_placeholder();
    profile.set_device_class(ProfileClassSignature::OutputClass);
    profile.set_color_space(ColorSpaceSignature::CmykData);
    profile.set_pcs(ColorSpaceSignature::LabData);
    profile.set_version(4.3);
    profile.set_header_rendering_intent(Intent::RelativeColorimetric);
    let mut desc = MLU::new(1);
    desc.set_text_ascii("Generic CMYK (naive)", Locale::none());
    profile.write_tag(TagSignature::ProfileDescriptionTag, Tag::MLU(&desc));
    let mut cprt = MLU::new(1);
    cprt.set_text_ascii("iAi", Locale::none());
    profile.write_tag(TagSignature::CopyrightTag, Tag::MLU(&cprt));
    let wtpt = CIEXYZ {
        X: 0.9642,
        Y: 1.0,
        Z: 0.8249,
    };
    profile.write_tag(TagSignature::MediaWhitePointTag, Tag::CIEXYZ(&wtpt));
    if !profile.write_tag(TagSignature::AToB0Tag, Tag::Pipeline(&pipeline)) {
        return None;
    }
    profile.icc().ok()
}

/// Serialize a profile to ICC bytes for embedding on export.
pub fn icc_bytes(p: &Profile) -> Vec<u8> {
    p.icc().unwrap_or_default()
}

/// sRGB ICC bytes — used to tag/embed the working space.
pub fn srgb_icc_bytes() -> Vec<u8> {
    icc_bytes(&srgb_profile())
}

/// Human-readable profile description, or a fallback string.
pub fn profile_name(p: &Profile) -> String {
    p.info(InfoType::Description, Locale::none())
        .unwrap_or_else(|| "Unknown profile".to_string())
}

/// Heuristic: does this profile description look like sRGB? Used to skip a
/// needless (slightly lossy) sRGB→sRGB round-trip on import.
pub fn name_is_srgb(name: &str) -> bool {
    name.to_ascii_lowercase().contains("srgb")
}

/// Convert an RGBA8 buffer from `src` to `dst` in place; the alpha channel is
/// passed through unchanged by lcms2. Returns `false` (leaving the buffer
/// untouched) if the transform can't be built.
pub fn convert_rgba8(buf: &mut [u8], src: &Profile, dst: &Profile, intent: Intent) -> bool {
    if buf.len() % 4 != 0 {
        return false;
    }
    let transform: Transform<[u8; 4], [u8; 4]> =
        match Transform::new(src, PixelFormat::RGBA_8, dst, PixelFormat::RGBA_8, intent) {
            Ok(t) => t,
            Err(_) => return false,
        };
    let px: &mut [[u8; 4]] = bytemuck::cast_slice_mut(buf);
    transform.transform_in_place(px);
    true
}

/// 16-bit RGB-profile conversion with alpha passed through by lcms. This is the
/// precision-preserving path for tagged TIFF/PNG and profile roundtrip tests.
pub fn convert_rgba16(buf: &mut [u16], src: &Profile, dst: &Profile, intent: Intent) -> bool {
    if buf.len() % 4 != 0 {
        return false;
    }
    let transform: Transform<[u16; 4], [u16; 4]> =
        match Transform::new(src, PixelFormat::RGBA_16, dst, PixelFormat::RGBA_16, intent) {
            Ok(transform) => transform,
            Err(_) => return false,
        };
    let pixels: &mut [[u16; 4]] = bytemuck::cast_slice_mut(buf);
    transform.transform_in_place(pixels);
    true
}

// ── Soft proofing (Phase B) ──────────────────────────────────────────────────
//
// A soft-proof simulates, on the sRGB display, how the image will look once
// rendered through a target device profile (e.g. a CMYK printer). Doing this
// per-pixel per-frame on the CPU is far too slow, so we bake the proof transform
// into a small RGB→RGB 3D lookup table that the GPU blit shader samples. The LUT
// maps sRGB display values → (proof device) → back to sRGB; with `gamut_warn` set,
// out-of-gamut inputs are flagged with a neutral-gray alarm colour.

/// Soft-proof 3D LUT edge size (samples per axis). 17³ = 4913 entries: smooth
/// enough for display proofing and cheap to regenerate when the setup changes.
pub const PROOF_LUT_SIZE: usize = 17;

/// Identity RGBA8 3D LUT (no colour change), laid out for a wgpu 3D texture:
/// R fastest, then G, then B (depth). Bound as the default so the proof sampler
/// is always valid even when proofing is off.
pub fn identity_lut(size: usize) -> Vec<u8> {
    let n = (size - 1).max(1) as f32;
    let mut out = vec![0u8; size * size * size * 4];
    let mut i = 0;
    for b in 0..size {
        for g in 0..size {
            for r in 0..size {
                out[i] = (r as f32 / n * 255.0).round() as u8;
                out[i + 1] = (g as f32 / n * 255.0).round() as u8;
                out[i + 2] = (b as f32 / n * 255.0).round() as u8;
                out[i + 3] = 255;
                i += 4;
            }
        }
    }
    out
}

/// Identity sRGB sample grid (R fastest, then G, then B) for LUT building.
fn identity_grid(size: usize) -> Vec<[u8; 3]> {
    let n = (size - 1).max(1) as f32;
    let mut grid = Vec::with_capacity(size * size * size);
    for b in 0..size {
        for g in 0..size {
            for r in 0..size {
                grid.push([
                    (r as f32 / n * 255.0).round() as u8,
                    (g as f32 / n * 255.0).round() as u8,
                    (b as f32 / n * 255.0).round() as u8,
                ]);
            }
        }
    }
    grid
}

fn pack_rgba(grid: &[[u8; 3]]) -> Vec<u8> {
    let mut out = vec![0u8; grid.len() * 4];
    for (i, px) in grid.iter().enumerate() {
        out[i * 4] = px[0];
        out[i * 4 + 1] = px[1];
        out[i * 4 + 2] = px[2];
        out[i * 4 + 3] = 255;
    }
    out
}

/// Build the display-correction RGBA8 3D LUT (`size`³ entries) applied in the
/// blit shader. Two optional stages, composed in order:
///   1. **Soft proof** (`proof_icc`): sRGB → proof device → sRGB, with a 50% gray
///      out-of-gamut alarm when `gamut_warn`.
///   2. **Display CMS** (`monitor_icc`): sRGB → monitor device, so colours show
///      correctly on a profiled (e.g. wide-gamut) display.
/// Pass `None` for a stage to skip it. Returns `None` if a profile/transform
/// can't be built. Layout matches [`identity_lut`].
pub fn build_display_lut(
    proof_icc: Option<&[u8]>,
    gamut_warn: bool,
    monitor_icc: Option<&[u8]>,
    size: usize,
) -> Option<Vec<u8>> {
    build_document_display_lut(None, proof_icc, gamut_warn, monitor_icc, size)
}

/// Build the display LUT when the canvas pixels are encoded in an explicitly
/// assigned RGB document profile. The first transform converts document RGB to
/// the sRGB display/soft-proof connection space; proof and monitor transforms
/// are then composed exactly as in [`build_display_lut`].
pub fn build_document_display_lut(
    document_icc: Option<&[u8]>,
    proof_icc: Option<&[u8]>,
    gamut_warn: bool,
    monitor_icc: Option<&[u8]>,
    size: usize,
) -> Option<Vec<u8>> {
    let mut grid = identity_grid(size);

    if let Some(document_icc) = document_icc {
        let document = profile_from_bytes(document_icc)?;
        if !name_is_srgb(&profile_name(&document)) {
            let srgb = srgb_profile();
            let transform: Transform<[u8; 3], [u8; 3]> = Transform::new(
                &document,
                PixelFormat::RGB_8,
                &srgb,
                PixelFormat::RGB_8,
                DEFAULT_INTENT,
            )
            .ok()?;
            transform.transform_in_place(&mut grid);
        }
    }

    if let Some(proof_icc) = proof_icc {
        let mut ctx = ThreadContext::new();
        if gamut_warn {
            // Neutral 50% gray alarm (16-bit encoded; unused channels = 0).
            let mut codes = [0u16; 16];
            codes[0] = 0x8080;
            codes[1] = 0x8080;
            codes[2] = 0x8080;
            ctx.set_alarm_codes(codes);
        }
        let srgb = Profile::new_srgb_context(&ctx);
        let proof = Profile::new_icc_context(&ctx, proof_icc).ok()?;
        let flags = if gamut_warn {
            Flags::SOFT_PROOFING | Flags::GAMUT_CHECK
        } else {
            Flags::SOFT_PROOFING
        };
        let t = Transform::new_proofing_context(
            &ctx,
            &srgb,
            PixelFormat::RGB_8,
            &srgb,
            PixelFormat::RGB_8,
            &proof,
            DEFAULT_INTENT,
            Intent::RelativeColorimetric,
            flags,
        )
        .ok()?;
        t.transform_in_place(&mut grid);
    }

    if let Some(monitor_icc) = monitor_icc {
        let srgb = srgb_profile();
        let monitor = Profile::new_icc(monitor_icc).ok()?;
        let t: Transform<[u8; 3], [u8; 3]> = Transform::new(
            &srgb,
            PixelFormat::RGB_8,
            &monitor,
            PixelFormat::RGB_8,
            DEFAULT_INTENT,
        )
        .ok()?;
        t.transform_in_place(&mut grid);
    }

    Some(pack_rgba(&grid))
}

/// The OS default display ICC profile as `(name, bytes)`. Windows reads it via
/// `GetICMProfile`; other platforms return `None` (load it manually instead).
#[cfg(target_os = "windows")]
pub fn system_display_profile() -> Option<(String, Vec<u8>)> {
    use core::ffi::c_void;
    use std::os::windows::ffi::OsStringExt;

    #[link(name = "user32")]
    extern "system" {
        fn GetDC(hwnd: *mut c_void) -> *mut c_void;
        fn ReleaseDC(hwnd: *mut c_void, hdc: *mut c_void) -> i32;
    }
    #[link(name = "gdi32")]
    extern "system" {
        fn GetICMProfileW(hdc: *mut c_void, size: *mut u32, name: *mut u16) -> i32;
    }

    unsafe {
        let hdc = GetDC(std::ptr::null_mut());
        if hdc.is_null() {
            return None;
        }
        let mut size: u32 = 260; // MAX_PATH chars (incl. NUL)
        let mut buf = vec![0u16; size as usize];
        let ok = GetICMProfileW(hdc, &mut size, buf.as_mut_ptr());
        ReleaseDC(std::ptr::null_mut(), hdc);
        if ok == 0 {
            return None;
        }
        buf.truncate(size.saturating_sub(1) as usize); // drop trailing NUL
        let path = std::path::PathBuf::from(std::ffi::OsString::from_wide(&buf));
        let bytes = std::fs::read(&path).ok()?;
        let name = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("Display")
            .to_string();
        Some((name, bytes))
    }
}

#[cfg(not(target_os = "windows"))]
pub fn system_display_profile() -> Option<(String, Vec<u8>)> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn srgb_to_srgb_is_identity() {
        let mut buf = vec![10u8, 120, 240, 255, 0, 0, 0, 128];
        let before = buf.clone();
        let p = srgb_profile();
        assert!(convert_rgba8(&mut buf, &p, &p, DEFAULT_INTENT));
        // sRGB→sRGB is a no-op (allow ±1 for lcms rounding).
        for (a, b) in before.iter().zip(buf.iter()) {
            assert!((*a as i32 - *b as i32).abs() <= 1, "{a} vs {b}");
        }
    }

    #[test]
    fn adobe_rgb_to_srgb_shifts_and_preserves_alpha() {
        // A saturated green in Adobe RGB maps to a different sRGB number, and the
        // alpha byte must be carried through untouched.
        let mut buf = vec![20u8, 200, 40, 173];
        let src = adobe_rgb_profile();
        let dst = srgb_profile();
        assert!(convert_rgba8(&mut buf, &src, &dst, DEFAULT_INTENT));
        assert_eq!(buf[3], 173, "alpha must be preserved");
        let changed = buf[0] != 20 || buf[1] != 200 || buf[2] != 40;
        assert!(changed, "Adobe RGB → sRGB should change RGB values");
    }

    #[test]
    fn srgb_icc_bytes_roundtrip() {
        let bytes = srgb_icc_bytes();
        assert!(!bytes.is_empty(), "sRGB profile must serialize");
        let reloaded = profile_from_bytes(&bytes).expect("reload sRGB icc");
        assert!(name_is_srgb(&profile_name(&reloaded)));
    }

    #[test]
    fn identity_lut_endpoints() {
        let size = 9;
        let lut = identity_lut(size);
        assert_eq!(lut.len(), size * size * size * 4);
        // First entry = black, last = white, alpha always 255.
        assert_eq!(&lut[0..4], &[0, 0, 0, 255]);
        let last = lut.len() - 4;
        assert_eq!(&lut[last..], &[255, 255, 255, 255]);
    }

    #[test]
    fn proof_lut_against_srgb_is_near_identity() {
        // Proofing sRGB on an sRGB device is (near) a no-op, so the LUT should be
        // close to identity. Validates the lcms2 proofing path end to end.
        let size = 9;
        let icc = srgb_icc_bytes();
        let lut =
            build_display_lut(Some(&icc), false, None, size).expect("build proof display lut");
        assert_eq!(lut.len(), size * size * size * 4);
        let ident = identity_lut(size);
        let max_diff = lut
            .iter()
            .zip(ident.iter())
            .map(|(a, b)| (*a as i32 - *b as i32).abs())
            .max()
            .unwrap_or(0);
        assert!(
            max_diff <= 4,
            "sRGB proof drifted from identity by {max_diff}"
        );
    }

    #[test]
    fn proof_lut_gamut_warning_builds() {
        // The gamut-check path (alarm codes + GAMUT_CHECK flag) must build a LUT.
        let icc = srgb_icc_bytes();
        let lut = build_display_lut(Some(&icc), true, None, 9);
        assert!(lut.is_some(), "gamut-warning LUT should build");
    }

    #[test]
    fn srgb_is_not_cmyk_and_cmyk_export_guards() {
        assert!(!profile_is_cmyk(&srgb_icc_bytes()));
        assert!(profile_is_rgb(&srgb_icc_bytes()));
        // Bad input lengths are rejected before any transform.
        assert!(srgb_rgb_to_cmyk8(&[], &srgb_icc_bytes(), DEFAULT_INTENT).is_none());
        assert!(srgb_rgb_to_cmyk8(&[1, 2], &srgb_icc_bytes(), DEFAULT_INTENT).is_none());
    }

    #[test]
    fn naive_gcr_black_white_primaries() {
        assert_eq!(naive_rgb_to_cmyk([0, 0, 0]), [0, 0, 0, 255]);
        assert_eq!(naive_rgb_to_cmyk([255, 255, 255]), [0, 0, 0, 0]);
        assert_eq!(naive_rgb_to_cmyk([255, 0, 0]), [0, 255, 255, 0]);
        assert_eq!(naive_rgb_to_cmyk([0, 255, 0]), [255, 0, 255, 0]);
        assert_eq!(naive_rgb_to_cmyk([0, 0, 255]), [255, 255, 0, 0]);
        // Neutral gray carries only K ink under max-K GCR.
        assert_eq!(naive_rgb_to_cmyk([128, 128, 128]), [0, 0, 0, 127]);
        assert_eq!(naive_cmyk_to_rgb([0, 0, 0, 127]), [128, 128, 128]);
    }

    #[test]
    fn naive_gcr_rgb_roundtrip_is_exact() {
        // Exhaustive over two channels, dense sampling on the third: the decode
        // error shrinks strictly below 0.5 (see naive_cmyk_to_rgb docs), so any
        // failure would be a formula/rounding bug, not a sampling gap.
        for r in 0..=255u8 {
            for g in 0..=255u8 {
                for b in [0u8, 1, 17, 85, 127, 128, 200, 254, 255] {
                    let px = [r, g, b];
                    let back = naive_cmyk_to_rgb(naive_rgb_to_cmyk(px));
                    assert_eq!(back, px, "roundtrip drifted for {px:?}");
                }
            }
        }
    }

    #[test]
    fn cmyk_converter_naive_matches_free_functions() {
        let conv = CmykConverter::Naive;
        let rgb = [[13u8, 200, 99], [0, 0, 0], [255, 255, 255]];
        let mut ink = [[0u8; 4]; 3];
        conv.rgb_to_cmyk_slice(&rgb, &mut ink);
        let mut back = [[0u8; 3]; 3];
        conv.cmyk_to_rgb_slice(&ink, &mut back);
        for i in 0..3 {
            assert_eq!(ink[i], naive_rgb_to_cmyk(rgb[i]));
            assert_eq!(back[i], rgb[i]);
            assert_eq!(conv.rgb_to_cmyk_one(rgb[i]), ink[i]);
            assert_eq!(conv.cmyk_to_rgb_one(ink[i]), rgb[i]);
        }
    }

    #[test]
    fn generic_cmyk_icc_reproduces_naive_model() {
        // The synthesized "Generic CMYK (naive)" profile must be a valid CMYK
        // profile whose DeviceCMYK→sRGB transform matches naive_cmyk_to_rgb — this
        // is what makes an exported CMYK PDF render the same colours as the app.
        let icc = generic_cmyk_icc_bytes();
        assert!(!icc.is_empty(), "generic CMYK profile must build");
        assert!(profile_is_cmyk(icc), "synthesized profile must be CMYK");

        let cmyk = profile_from_bytes(icc).expect("reload synthesized CMYK profile");
        let srgb = srgb_profile();
        let t: Transform<[u8; 4], [u8; 3]> = Transform::new(
            &cmyk,
            PixelFormat::CMYK_8,
            &srgb,
            PixelFormat::RGB_8,
            DEFAULT_INTENT,
        )
        .expect("build CMYK→sRGB transform");
        let samples = [0u8, 32, 64, 96, 128, 160, 192, 224, 255];
        let mut max = 0i32;
        for &c in &samples {
            for &m in &samples {
                for &y in &samples {
                    for &k in &samples {
                        let want = naive_cmyk_to_rgb([c, m, y, k]);
                        let mut got = [[0u8; 3]];
                        t.transform_pixels(&[[c, m, y, k]], &mut got);
                        for i in 0..3 {
                            max = max.max((want[i] as i32 - got[0][i] as i32).abs());
                        }
                    }
                }
            }
        }
        assert!(
            max <= 4,
            "synthesized CMYK profile drifts from naive by {max}"
        );
    }

    #[test]
    fn cmyk_icc_paths_reject_non_cmyk_profiles() {
        // sRGB is not CMYK: both the one-shot decode and the converter must refuse.
        let srgb = srgb_icc_bytes();
        assert!(cmyk8_to_srgb_rgb(&[0, 0, 0, 255], &srgb, DEFAULT_INTENT).is_none());
        assert!(CmykConverter::from_icc_bytes(&srgb, DEFAULT_INTENT).is_none());
        // Bad input lengths are rejected before any transform.
        assert!(cmyk8_to_srgb_rgb(&[], &srgb, DEFAULT_INTENT).is_none());
        assert!(cmyk8_to_srgb_rgb(&[1, 2, 3], &srgb, DEFAULT_INTENT).is_none());
    }

    #[test]
    fn display_lut_to_srgb_monitor_is_near_identity() {
        // Display-CMS to an sRGB "monitor" is a near no-op.
        let size = 9;
        let lut =
            build_display_lut(None, false, Some(&srgb_icc_bytes()), size).expect("display lut");
        let ident = identity_lut(size);
        let max_diff = lut
            .iter()
            .zip(ident.iter())
            .map(|(a, b)| (*a as i32 - *b as i32).abs())
            .max()
            .unwrap_or(0);
        assert!(max_diff <= 4, "sRGB-monitor display LUT drifted {max_diff}");
    }
}

/// Target device a soft-proof (View ▸ Proof) simulates on the sRGB display.
#[derive(Debug, Clone, PartialEq)]
pub enum ProofTarget {
    Srgb,
    AdobeRgb,
    /// A loaded device profile (e.g. a CMYK printer .icc): display name + bytes.
    Custom {
        name: String,
        icc: Vec<u8>,
    },
}

impl Default for ProofTarget {
    fn default() -> Self {
        ProofTarget::AdobeRgb
    }
}

impl ProofTarget {
    pub fn label(&self) -> String {
        match self {
            ProofTarget::Srgb => "sRGB".to_string(),
            ProofTarget::AdobeRgb => "Adobe RGB (1998)".to_string(),
            ProofTarget::Custom { name, .. } => name.clone(),
        }
    }

    /// ICC bytes of the proof target profile (built into the proof LUT).
    pub fn icc_bytes(&self) -> Vec<u8> {
        match self {
            ProofTarget::Srgb => crate::core::cms::srgb_icc_bytes(),
            ProofTarget::AdobeRgb => {
                crate::core::cms::icc_bytes(&crate::core::cms::adobe_rgb_profile())
            }
            ProofTarget::Custom { icc, .. } => icc.clone(),
        }
    }
}
