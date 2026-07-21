// PSD/PSB composite importer with an in-house parser.
//
// The previous implementation used the `psd` crate, which panicked on
// ZIP-compressed image data (silently killing the decode worker), read 16-bit
// files as 8-bit garbage and rejected PSB. This parser reads the flattened
// composite from the ImageData section and supports:
//   • PSD and PSB (version 2: 8-byte section lengths, 4-byte RLE row counts)
//   • 8-bit and 16-bit channels (16-bit opens as a 16-bit document)
//   • RAW, RLE (PackBits), ZIP and ZIP-with-prediction compression
//   • RGB and Grayscale colour modes, optional alpha channel
//   • embedded ICC profiles (resource 1039) through the shared
//     colour-managed import path, same rules as PNG/TIFF
// Unsupported colour modes (CMYK, Lab, Indexed, Bitmap, Duotone,
// Multichannel) and 32-bit depth return a specific message instead of a
// panic or wrong pixels.
//
// When the Layer & Mask section carries an editable layer stack it is rebuilt in
// full: layers (name/opacity/blend/visibility/offset), per-layer masks, and
// nested groups (lsct section dividers). Any parse trouble, a 16-bit + non-sRGB
// file, or a section-less file falls back to the flat composite, so nothing
// regresses to an error. PSD stores layers bottom→top, matching our stack order.

use super::psd_adjust;
use super::psd_text;
use super::{ExportOptions, Exporter, Importer};
use crate::core::blend::BlendMode;
use crate::core::canvas::{BitDepth, Canvas};
use crate::core::layer::{AdjustmentType, Layer, LayerMask};
use crate::core::tile::TileMap;
use std::collections::{HashMap, HashSet};
use std::io::Read as _;
use std::path::Path;

pub struct PsdImporter;

impl Importer for PsdImporter {
    fn extensions(&self) -> &[&str] {
        &["psd", "psb"]
    }

    fn import(&self, path: &Path) -> Result<Canvas, String> {
        let data = std::fs::read(path).map_err(|e| e.to_string())?;
        import_bytes(&data)
    }
}

fn import_bytes(data: &[u8]) -> Result<Canvas, String> {
    let mut r = Reader::new(data);
    let header = parse_header(&mut r)?;
    check_supported(&header)?;

    let max = crate::core::canvas::MAX_DIMENSION;
    if header.width > max || header.height > max {
        return Err(format!(
            "PSD quá lớn: {}×{} (giới hạn {max}×{max})",
            header.width, header.height
        ));
    }

    // Colour Mode Data section (palette for indexed/duotone — modes we reject).
    let cmd_len = r.u32()?;
    r.skip(cmd_len as u64)?;

    // Image Resources — scan for the embedded ICC profile (resource 1039).
    let res_len = r.u32()?;
    let resources = r.take(res_len as usize)?;
    let icc = find_icc_resource(resources);

    // Same rule as PNG/TIFF import: keep 16-bit precision only when the source is
    // untagged or already sRGB; a tagged non-sRGB source takes the colour-managed
    // 8-bit path (colour correctness over precision).
    let icc_is_srgb = match icc.as_deref() {
        None => true,
        Some(bytes) => crate::core::cms::profile_from_bytes(bytes)
            .map(|p| crate::core::cms::name_is_srgb(&crate::core::cms::profile_name(&p)))
            .unwrap_or(false),
    };

    // Layer & Mask Info: rebuild the editable layer stack (layers, masks, nested
    // groups) when the file carries one. Any parse trouble returns None/Err and
    // falls through to the flat composite below, so a quirky file still opens.
    let lm_len = r.len_word(header.is_psb)?;
    let lm_bytes = r.take(lm_len as usize)?;
    if let Ok(Some(canvas)) = import_layer_stack(lm_bytes, &header, icc.as_deref(), icc_is_srgb) {
        return Ok(canvas);
    }

    // Image Data section: the flattened composite (single-layer files, or fallback
    // when there is no usable Layer & Mask section).
    let compression = r.u16()?;
    let channels = decode_channels(&mut r, &header, compression)?;

    match header.depth {
        8 => {
            let mut px = assemble_rgba8(&channels, &header)?;
            let (tag, source) = super::apply_input_profile(&mut px, icc.as_deref());
            let mut canvas = Canvas::from_rgba(px, header.width, header.height);
            canvas.icc_profile = tag;
            canvas.metadata.source_profile = source;
            Ok(canvas)
        }
        16 => {
            let px16 = assemble_rgba16(&channels, &header)?;
            if icc_is_srgb {
                let mut canvas = Canvas::from_rgba16(px16, header.width, header.height);
                canvas.icc_profile = crate::core::canvas::IccProfile {
                    name: crate::core::cms::WorkingProfile::Srgb.name().to_string(),
                    data: crate::core::cms::srgb_icc_bytes(),
                };
                let source = icc
                    .as_deref()
                    .and_then(crate::core::cms::profile_from_bytes)
                    .map(|p| crate::core::cms::profile_name(&p))
                    .unwrap_or_default();
                canvas.metadata.source_profile = source;
                Ok(canvas)
            } else {
                let mut px: Vec<u8> = px16.iter().map(|&v| (v >> 8) as u8).collect();
                let (tag, source) = super::apply_input_profile(&mut px, icc.as_deref());
                let mut canvas = Canvas::from_rgba(px, header.width, header.height);
                canvas.icc_profile = tag;
                canvas.metadata.source_profile = source;
                Ok(canvas)
            }
        }
        _ => unreachable!("check_supported gates depth"),
    }
}

struct Header {
    is_psb: bool,
    channels: usize,
    width: u32,
    height: u32,
    depth: u16,
    color_mode: u16,
}

/// Big-endian bounds-checked reader over the raw file bytes.
struct Reader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8], String> {
        let end = self.pos.checked_add(n).ok_or_else(err_truncated)?;
        if end > self.data.len() {
            return Err(err_truncated());
        }
        let s = &self.data[self.pos..end];
        self.pos = end;
        Ok(s)
    }

    fn skip(&mut self, n: u64) -> Result<(), String> {
        let n = usize::try_from(n).map_err(|_| err_truncated())?;
        self.take(n).map(|_| ())
    }

    fn u16(&mut self) -> Result<u16, String> {
        let b = self.take(2)?;
        Ok(u16::from_be_bytes([b[0], b[1]]))
    }

    fn u32(&mut self) -> Result<u32, String> {
        let b = self.take(4)?;
        Ok(u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
    }

    fn u64(&mut self) -> Result<u64, String> {
        let b = self.take(8)?;
        Ok(u64::from_be_bytes([
            b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7],
        ]))
    }

    fn u8(&mut self) -> Result<u8, String> {
        Ok(self.take(1)?[0])
    }

    fn i16(&mut self) -> Result<i16, String> {
        self.u16().map(|v| v as i16)
    }

    fn i32(&mut self) -> Result<i32, String> {
        self.u32().map(|v| v as i32)
    }

    /// Section length that is 8 bytes in PSB, 4 in PSD.
    fn len_word(&mut self, is_psb: bool) -> Result<u64, String> {
        if is_psb {
            self.u64()
        } else {
            self.u32().map(u64::from)
        }
    }

    fn remaining(&self) -> usize {
        self.data.len() - self.pos
    }

    fn rest(&self) -> &'a [u8] {
        &self.data[self.pos..]
    }
}

fn err_truncated() -> String {
    "PSD: file bị cắt cụt hoặc hỏng".to_string()
}

fn parse_header(r: &mut Reader) -> Result<Header, String> {
    if r.take(4)? != b"8BPS" {
        return Err("Không phải file PSD (thiếu chữ ký 8BPS)".to_string());
    }
    let version = r.u16()?;
    if version != 1 && version != 2 {
        return Err(format!("PSD: version {version} không hợp lệ"));
    }
    r.skip(6)?;
    let channels = r.u16()? as usize;
    let height = r.u32()?;
    let width = r.u32()?;
    let depth = r.u16()?;
    let color_mode = r.u16()?;
    if width == 0 || height == 0 {
        return Err("PSD: kích thước ảnh bằng 0".to_string());
    }
    if channels == 0 || channels > 56 {
        return Err(format!("PSD: số kênh {channels} không hợp lệ"));
    }
    Ok(Header {
        is_psb: version == 2,
        channels,
        width,
        height,
        depth,
        color_mode,
    })
}

fn check_supported(h: &Header) -> Result<(), String> {
    let mode_name = match h.color_mode {
        1 | 3 => None,
        0 => Some("Bitmap (1-bit)"),
        2 => Some("Indexed color"),
        4 => Some("CMYK"),
        7 => Some("Multichannel"),
        8 => Some("Duotone"),
        9 => Some("Lab"),
        m => return Err(format!("PSD: color mode {m} không hợp lệ")),
    };
    if let Some(name) = mode_name {
        return Err(format!(
            "PSD {name} chưa được hỗ trợ — hãy chuyển sang RGB (Image ▸ Mode) rồi lưu lại"
        ));
    }
    let required = if h.color_mode == 3 { 3 } else { 1 };
    if h.channels < required {
        return Err("PSD: thiếu kênh màu".to_string());
    }
    match h.depth {
        8 | 16 => Ok(()),
        32 => Err(
            "PSD 32-bit/kênh chưa được hỗ trợ — hãy chuyển sang 8 hoặc 16-bit rồi lưu lại"
                .to_string(),
        ),
        d => Err(format!("PSD: bit depth {d} chưa được hỗ trợ")),
    }
}

/// Scan the Image Resources section for the ICC profile (resource ID 1039).
fn find_icc_resource(mut res: &[u8]) -> Option<Vec<u8>> {
    while res.len() >= 12 {
        if &res[0..4] != b"8BIM" {
            return None;
        }
        let id = u16::from_be_bytes([res[4], res[5]]);
        // Pascal name string, padded so (length byte + name) is even.
        let name_len = res[6] as usize;
        let name_total = (1 + name_len + 1) & !1;
        let size_off = 6 + name_total;
        if res.len() < size_off + 4 {
            return None;
        }
        let size = u32::from_be_bytes([
            res[size_off],
            res[size_off + 1],
            res[size_off + 2],
            res[size_off + 3],
        ]) as usize;
        let data_off = size_off + 4;
        if res.len() < data_off + size {
            return None;
        }
        if id == 1039 {
            return Some(res[data_off..data_off + size].to_vec());
        }
        let advance = data_off + ((size + 1) & !1);
        if advance > res.len() {
            return None;
        }
        res = &res[advance..];
    }
    None
}

/// Decode the ImageData section into one plane of bytes per channel
/// (`height × width × depth/8` bytes each, big-endian samples for 16-bit).
fn decode_channels(r: &mut Reader, h: &Header, compression: u16) -> Result<Vec<Vec<u8>>, String> {
    let bytes_per_sample = (h.depth / 8) as usize;
    let row_bytes = (h.width as usize)
        .checked_mul(bytes_per_sample)
        .ok_or_else(err_truncated)?;
    let rows = h.height as usize;
    let chan_bytes = row_bytes.checked_mul(rows).ok_or_else(err_truncated)?;

    match compression {
        // RAW: channels stored back to back.
        0 => (0..h.channels)
            .map(|_| r.take(chan_bytes).map(|s| s.to_vec()))
            .collect(),
        // RLE (PackBits): row-byte-count table, then packed scanlines.
        1 => {
            let n_counts = rows.checked_mul(h.channels).ok_or_else(err_truncated)?;
            let mut counts = Vec::with_capacity(n_counts);
            for _ in 0..n_counts {
                counts.push(if h.is_psb {
                    r.u32()? as usize
                } else {
                    r.u16()? as usize
                });
            }
            let mut channels = Vec::with_capacity(h.channels);
            for ch in 0..h.channels {
                let mut plane = Vec::with_capacity(chan_bytes);
                for row in 0..rows {
                    let packed = r.take(counts[ch * rows + row])?;
                    unpack_bits_row(packed, &mut plane, row_bytes)?;
                }
                channels.push(plane);
            }
            Ok(channels)
        }
        // ZIP (2) / ZIP with prediction (3): one zlib stream of all planes.
        2 | 3 => {
            let mut inflated = Vec::new();
            flate2::read::ZlibDecoder::new(r.rest())
                .read_to_end(&mut inflated)
                .map_err(|e| format!("PSD: lỗi giải nén ZIP: {e}"))?;
            let expected = chan_bytes
                .checked_mul(h.channels)
                .ok_or_else(err_truncated)?;
            if inflated.len() < expected {
                return Err(err_truncated());
            }
            inflated.truncate(expected);
            if compression == 3 {
                undo_prediction(&mut inflated, h.depth, row_bytes);
            }
            Ok(inflated
                .chunks_exact(chan_bytes)
                .map(|c| c.to_vec())
                .collect())
        }
        c => Err(format!("PSD: kiểu nén {c} không được hỗ trợ")),
    }
}

/// Decode one PackBits-compressed scanline, appending exactly `row_bytes`
/// bytes to `out`.
fn unpack_bits_row(src: &[u8], out: &mut Vec<u8>, row_bytes: usize) -> Result<(), String> {
    let target = out.len() + row_bytes;
    let mut i = 0;
    while out.len() < target {
        let Some(&header) = src.get(i) else {
            return Err(err_truncated());
        };
        i += 1;
        let n = header as i8;
        if n >= 0 {
            let count = n as usize + 1;
            let lit = src.get(i..i + count).ok_or_else(err_truncated)?;
            let keep = count.min(target - out.len());
            out.extend_from_slice(&lit[..keep]);
            i += count;
        } else if n != -128 {
            let count = (1 - n as isize) as usize;
            let &b = src.get(i).ok_or_else(err_truncated)?;
            i += 1;
            let keep = count.min(target - out.len());
            out.resize(out.len() + keep, b);
        }
    }
    Ok(())
}

/// Reverse the per-scanline delta encoding used by ZIP-with-prediction.
/// 8-bit rows accumulate bytes; 16-bit rows accumulate big-endian u16 lanes.
fn undo_prediction(buf: &mut [u8], depth: u16, row_bytes: usize) {
    if row_bytes == 0 {
        return;
    }
    for row in buf.chunks_exact_mut(row_bytes) {
        if depth == 8 {
            for i in 1..row.len() {
                row[i] = row[i].wrapping_add(row[i - 1]);
            }
        } else {
            let mut prev = u16::from_be_bytes([row[0], row[1]]);
            let mut i = 2;
            while i + 1 < row.len() {
                let cur = u16::from_be_bytes([row[i], row[i + 1]]).wrapping_add(prev);
                row[i..i + 2].copy_from_slice(&cur.to_be_bytes());
                prev = cur;
                i += 2;
            }
        }
    }
}

/// Interleave 8-bit planar channels into RGBA. Grayscale replicates the single
/// plane; the channel after the colour channels (if present) is treated as
/// alpha, matching the previous importer and common PSD readers.
fn assemble_rgba8(channels: &[Vec<u8>], h: &Header) -> Result<Vec<u8>, String> {
    let n = (h.width as usize) * (h.height as usize);
    let (cr, cg, cb, alpha) = match h.color_mode {
        3 => (&channels[0], &channels[1], &channels[2], channels.get(3)),
        _ => (&channels[0], &channels[0], &channels[0], channels.get(1)),
    };
    let mut out = vec![0u8; n * 4];
    for i in 0..n {
        out[i * 4] = cr[i];
        out[i * 4 + 1] = cg[i];
        out[i * 4 + 2] = cb[i];
        out[i * 4 + 3] = alpha.map_or(255, |a| a[i]);
    }
    Ok(out)
}

/// Interleave 16-bit planar channels (big-endian samples) into RGBA u16.
fn assemble_rgba16(channels: &[Vec<u8>], h: &Header) -> Result<Vec<u16>, String> {
    let n = (h.width as usize) * (h.height as usize);
    let sample = |plane: &Vec<u8>, i: usize| u16::from_be_bytes([plane[i * 2], plane[i * 2 + 1]]);
    let (cr, cg, cb, alpha) = match h.color_mode {
        3 => (&channels[0], &channels[1], &channels[2], channels.get(3)),
        _ => (&channels[0], &channels[0], &channels[0], channels.get(1)),
    };
    let mut out = vec![0u16; n * 4];
    for i in 0..n {
        out[i * 4] = sample(cr, i);
        out[i * 4 + 1] = sample(cg, i);
        out[i * 4 + 2] = sample(cb, i);
        out[i * 4 + 3] = alpha.map_or(u16::MAX, |a| sample(a, i));
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Layer & Mask section — editable layer stack (layers, masks, nested groups).
// ---------------------------------------------------------------------------

/// One parsed layer record plus its decoded channel planes.
struct RawLayer {
    name: String,
    top: i32,
    left: i32,
    bottom: i32,
    right: i32,
    opacity: u8,
    blend: [u8; 4],
    visible: bool,
    /// (channel id, byte length incl. the 2-byte compression tag).
    channels: Vec<(i16, u64)>,
    mask: Option<MaskRect>,
    /// lsct section-divider type: 0 = normal layer, 1 = open folder, 2 = closed
    /// folder, 3 = bounding divider ("</Layer group>").
    section: u8,
    /// Some when the layer is an adjustment layer whose parameters we decoded
    /// (`levl`/`hue2`/… → an editable iAi adjustment). `is_adjustment` stays true
    /// even for recognised-but-unmapped types so they are not read as raster.
    adjustment: Option<AdjustmentType>,
    is_adjustment: bool,
    /// Some when the layer carries a `TySh` type-tool block we decoded into
    /// editable text; the layer's rasterized pixels are still imported for the
    /// initial appearance (see the consumption loop).
    text: Option<crate::core::text::TextData>,
    /// channel id → decoded plane (raw bytes, big-endian samples for 16-bit).
    planes: HashMap<i16, Vec<u8>>,
}

struct MaskRect {
    top: i32,
    left: i32,
    bottom: i32,
    right: i32,
    default_color: u8,
    disabled: bool,
}

impl RawLayer {
    fn width(&self) -> u32 {
        (self.right - self.left).max(0) as u32
    }
    fn height(&self) -> u32 {
        (self.bottom - self.top).max(0) as u32
    }
}

/// Rebuild the editable layer stack from the Layer & Mask Info section. Returns
/// `Ok(None)` when there is no usable layer info (caller falls back to the flat
/// composite); an `Err` is likewise treated as "use the composite".
fn import_layer_stack(
    lm: &[u8],
    header: &Header,
    icc: Option<&[u8]>,
    icc_is_srgb: bool,
) -> Result<Option<Canvas>, String> {
    if lm.is_empty() {
        return Ok(None);
    }
    // 16-bit + a non-sRGB profile needs the colour-managed 8-bit composite path;
    // the per-layer path only handles sRGB/untagged 16-bit.
    if header.depth == 16 && !icc_is_srgb {
        return Ok(None);
    }

    let mut s = Reader::new(lm);
    let layer_info_len = s.len_word(header.is_psb)? as usize;
    if layer_info_len == 0 {
        return Ok(None); // mask-only / adjustment-only section: no pixel layers
    }
    let li = s.take(layer_info_len.min(s.remaining()))?;
    parse_layer_info(li, header, icc)
}

/// Parse a layer-info blob — 2-byte layer count, then the layer records, then
/// the channel image data — into a layered canvas. Shared by the PSD Layer &
/// Mask section and by TIFF's embedded ImageSourceData (`Layr`/`Lr16`). `li`
/// must begin at the layer count. `Ok(None)` = nothing usable (fall back to the
/// flat composite).
fn parse_layer_info(
    li: &[u8],
    header: &Header,
    icc: Option<&[u8]>,
) -> Result<Option<Canvas>, String> {
    let mut r = Reader::new(li);

    let count = r.i16()?.unsigned_abs() as usize;
    if count == 0 {
        return Ok(None);
    }
    let mut layers: Vec<RawLayer> = Vec::with_capacity(count);
    for _ in 0..count {
        layers.push(parse_layer_record(&mut r, header)?);
    }

    // Channel image data follows the records, in layer-then-channel order.
    let bps = (header.depth / 8) as usize;
    for layer in &mut layers {
        let plan: Vec<(i16, u64, (u32, u32))> = layer
            .channels
            .iter()
            .map(|&(id, len)| (id, len, channel_dims(layer, id)))
            .collect();
        for (id, len, (rw, rh)) in plan {
            let blob = r.take(len as usize)?;
            if rw == 0 || rh == 0 {
                continue;
            }
            if blob.len() < 2 {
                return Err(err_truncated());
            }
            let comp = u16::from_be_bytes([blob[0], blob[1]]);
            let plane = decode_plane(&blob[2..], comp, rw, rh, bps, header.is_psb)?;
            layer.planes.insert(id, plane);
        }
    }

    let app_layers = build_app_layers(&layers, header, icc);
    if app_layers.is_empty() {
        return Ok(None);
    }
    Ok(Some(assemble_canvas(app_layers, header, icc)))
}

/// Parse a Photoshop layer block lifted from a TIFF's ImageSourceData tag (37724)
/// into a layered canvas — the same editable stack a PSD would yield. `block` is
/// the `Layr`/`Lr16` block payload; `depth` is 8 or 16 (32-bit is rejected by the
/// caller). The payload may or may not carry a leading 4-byte layer-info length
/// (writers differ), so both framings are tried; a wrong guess fails the per-record
/// `8BIM` signature check and is discarded. `None` → caller uses the flat image.
pub(crate) fn import_tiff_photoshop_layers(
    block: &[u8],
    depth: u16,
    width: u32,
    height: u32,
    icc: Option<&[u8]>,
    icc_is_srgb: bool,
) -> Option<Canvas> {
    if !matches!(depth, 8 | 16) || (depth == 16 && !icc_is_srgb) {
        return None;
    }
    let header = Header {
        is_psb: false,
        channels: 3,
        width,
        height,
        depth,
        color_mode: 3, // RGB — the near-universal case for layered TIFFs
    };
    // Framing A: payload begins at the 2-byte layer count.
    if let Ok(Some(canvas)) = parse_layer_info(block, &header, icc) {
        return Some(canvas);
    }
    // Framing B: payload begins with a 4-byte layer-info length.
    if block.len() >= 4 {
        if let Ok(Some(canvas)) = parse_layer_info(&block[4..], &header, icc) {
            return Some(canvas);
        }
    }
    None
}

/// Channel dimensions: colour/alpha channels use the layer rect; the user (-2)
/// and real (-3) mask channels use the mask rect.
fn channel_dims(layer: &RawLayer, id: i16) -> (u32, u32) {
    if id == -2 || id == -3 {
        return match &layer.mask {
            Some(m) => (
                (m.right - m.left).max(0) as u32,
                (m.bottom - m.top).max(0) as u32,
            ),
            None => (0, 0),
        };
    }
    (layer.width(), layer.height())
}

fn parse_layer_record(r: &mut Reader, header: &Header) -> Result<RawLayer, String> {
    let top = r.i32()?;
    let left = r.i32()?;
    let bottom = r.i32()?;
    let right = r.i32()?;
    let nch = r.u16()? as usize;
    if nch > 64 {
        return Err("PSD: layer có số kênh bất thường".to_string());
    }
    let mut channels = Vec::with_capacity(nch);
    for _ in 0..nch {
        let id = r.i16()?;
        let len = r.len_word(header.is_psb)?;
        channels.push((id, len));
    }
    if r.take(4)? != b"8BIM" {
        return Err("PSD: chữ ký blend của layer sai".to_string());
    }
    let bk = r.take(4)?;
    let blend = [bk[0], bk[1], bk[2], bk[3]];
    let opacity = r.u8()?;
    let _clipping = r.u8()?;
    let flags = r.u8()?;
    let _filler = r.u8()?;

    let extra_len = r.u32()? as usize;
    let extra = r.take(extra_len)?;
    let mut es = Reader::new(extra);

    // Layer mask / adjustment data.
    let mask_len = es.u32()? as usize;
    let mut mask = None;
    if mask_len >= 18 {
        let mb = es.take(mask_len)?;
        let mut ms = Reader::new(mb);
        let mtop = ms.i32()?;
        let mleft = ms.i32()?;
        let mbottom = ms.i32()?;
        let mright = ms.i32()?;
        let default_color = ms.u8()?;
        let mflags = ms.u8()?;
        mask = Some(MaskRect {
            top: mtop,
            left: mleft,
            bottom: mbottom,
            right: mright,
            default_color,
            disabled: (mflags & 0x02) != 0,
        });
    } else if mask_len > 0 {
        es.skip(mask_len as u64)?;
    }

    // Layer blending ranges (skipped).
    let br_len = es.u32()? as usize;
    es.skip(br_len as u64)?;

    // Legacy Pascal name, padded so (1 + len) is a multiple of 4.
    let name_len = es.u8()? as usize;
    let name_bytes = es.take(name_len)?;
    let pad = (4 - ((1 + name_len) % 4)) % 4;
    es.skip(pad as u64)?;
    let mut name = latin1(name_bytes);

    // Additional layer info: Unicode name ('luni') overrides the Pascal name;
    // section divider ('lsct'/'lsdk') marks group folders; adjustment blocks
    // ('levl'/'hue2'/…) mark — and, when we can map them, define — an adjustment
    // layer; the type-tool block ('TySh') carries editable text.
    let mut section = 0u8;
    let mut adjustment: Option<AdjustmentType> = None;
    let mut is_adjustment = false;
    let mut text = None;
    while es.remaining() >= 12 {
        let sig = es.take(4)?;
        if sig != b"8BIM" && sig != b"8B64" {
            break;
        }
        let key = *array4(es.take(4)?);
        let len = es.u32()? as usize;
        if len > es.remaining() {
            break;
        }
        let data = es.take(len)?;
        match &key {
            b"luni" => {
                if let Some(n) = parse_unicode_name(data) {
                    if !n.is_empty() {
                        name = n;
                    }
                }
            }
            b"lsct" | b"lsdk" => {
                if data.len() >= 4 {
                    section = u32::from_be_bytes([data[0], data[1], data[2], data[3]]) as u8;
                }
            }
            b"TySh" => {
                if text.is_none() {
                    text = psd_text::parse_type_tool(data).map(|t| t.td);
                }
            }
            _ if psd_adjust::is_adjustment_key(&key) => {
                is_adjustment = true;
                if adjustment.is_none() {
                    adjustment = psd_adjust::parse_adjustment(&key, data);
                }
            }
            _ => {}
        }
        // Additional-info blocks are padded to an even length.
        if len % 2 == 1 && es.remaining() > 0 {
            let _ = es.u8();
        }
    }

    Ok(RawLayer {
        name,
        top,
        left,
        bottom,
        right,
        opacity,
        blend,
        visible: (flags & 0x02) == 0,
        channels,
        mask,
        section,
        adjustment,
        is_adjustment,
        text,
        planes: HashMap::new(),
    })
}

fn array4(s: &[u8]) -> &[u8; 4] {
    // Callers pass exactly 4 bytes (from `take(4)`).
    s.try_into().expect("take(4) yields 4 bytes")
}

/// Decode one channel plane (`rw × rh × bps` bytes) from its compressed blob.
fn decode_plane(
    data: &[u8],
    comp: u16,
    rw: u32,
    rh: u32,
    bps: usize,
    is_psb: bool,
) -> Result<Vec<u8>, String> {
    let row_bytes = (rw as usize) * bps;
    let plane_len = row_bytes * (rh as usize);
    match comp {
        0 => {
            if data.len() < plane_len {
                return Err(err_truncated());
            }
            Ok(data[..plane_len].to_vec())
        }
        1 => {
            let mut rd = Reader::new(data);
            let rows = rh as usize;
            let mut counts = Vec::with_capacity(rows);
            for _ in 0..rows {
                counts.push(if is_psb {
                    rd.u32()? as usize
                } else {
                    rd.u16()? as usize
                });
            }
            let mut plane = Vec::with_capacity(plane_len);
            for &c in &counts {
                let packed = rd.take(c)?;
                unpack_bits_row(packed, &mut plane, row_bytes)?;
            }
            Ok(plane)
        }
        2 | 3 => {
            let mut inflated = Vec::new();
            flate2::read::ZlibDecoder::new(data)
                .read_to_end(&mut inflated)
                .map_err(|e| format!("PSD: giải nén layer lỗi: {e}"))?;
            if inflated.len() < plane_len {
                return Err(err_truncated());
            }
            inflated.truncate(plane_len);
            if comp == 3 {
                undo_prediction(&mut inflated, (bps * 8) as u16, row_bytes);
            }
            Ok(inflated)
        }
        c => Err(format!("PSD: layer nén kiểu {c} không hỗ trợ")),
    }
}

/// UTF-16BE 'luni' Unicode layer name (u32 count + code units).
fn parse_unicode_name(d: &[u8]) -> Option<String> {
    if d.len() < 4 {
        return None;
    }
    let n = u32::from_be_bytes([d[0], d[1], d[2], d[3]]) as usize;
    let mut u = Vec::with_capacity(n);
    for i in 0..n {
        let o = 4 + i * 2;
        if o + 1 >= d.len() {
            break;
        }
        u.push(u16::from_be_bytes([d[o], d[o + 1]]));
    }
    while u.last() == Some(&0) {
        u.pop();
    }
    String::from_utf16(&u).ok()
}

/// Best-effort Latin-1 decode of the legacy Pascal name.
fn latin1(b: &[u8]) -> String {
    b.iter().map(|&c| c as char).collect()
}

fn name_or(name: &str, default: &str) -> String {
    let t = name.trim();
    if t.is_empty() {
        default.to_string()
    } else {
        t.to_string()
    }
}

/// Map a PSD 4-char blend key to the app's blend mode (unsupported keys →
/// Normal).
fn map_blend(k: &[u8; 4]) -> BlendMode {
    match k {
        b"diss" => BlendMode::Dissolve,
        b"dark" => BlendMode::Darken,
        b"mul " => BlendMode::Multiply,
        b"idiv" => BlendMode::ColorBurn,
        b"lite" => BlendMode::Lighten,
        b"scrn" => BlendMode::Screen,
        b"div " => BlendMode::ColorDodge,
        b"over" => BlendMode::Overlay,
        b"sLit" => BlendMode::SoftLight,
        b"hLit" => BlendMode::HardLight,
        b"lLit" => BlendMode::LinearLight,
        b"diff" => BlendMode::Difference,
        b"smud" => BlendMode::Exclusion,
        b"hue " => BlendMode::Hue,
        b"sat " => BlendMode::Saturation,
        b"colr" => BlendMode::Color,
        b"lum " => BlendMode::Luminosity,
        _ => BlendMode::Normal, // norm, pass, and modes the app lacks
    }
}

/// Turn parsed records (bottom→top) into app layers, reconstructing group
/// nesting. In file order a group is `[</Layer group> divider, children…,
/// folder header]`; the divider (type 3) opens a pending group id and the folder
/// header (type 1/2) emits the group layer and closes it — yielding the app's
/// `[children…, header]` contiguous run with children re-parented to the header.
fn build_app_layers(raw: &[RawLayer], header: &Header, icc: Option<&[u8]>) -> Vec<Layer> {
    let (cw, ch) = (header.width, header.height);
    let mut out: Vec<Layer> = Vec::new();
    let mut stack: Vec<u32> = Vec::new();
    let mut group_ids: HashSet<u32> = HashSet::new();
    let mut next_id: u32 = 0;

    for rl in raw {
        match rl.section {
            3 => {
                // Bounding divider = bottom bracket → open a pending group.
                let gid = next_id;
                next_id += 1;
                stack.push(gid);
            }
            1 | 2 => {
                // Folder header = top bracket → emit the group and close it.
                let gid = stack.pop().unwrap_or_else(|| {
                    let v = next_id;
                    next_id += 1;
                    v
                });
                let mut g = Layer::new_group(gid, &name_or(&rl.name, "Group"), cw, ch);
                g.opacity = (rl.opacity as f32) / 255.0;
                g.blend_mode = map_blend(&rl.blend);
                g.visible = rl.visible;
                g.expanded = rl.section == 1;
                g.parent_id = stack.last().copied();
                if let Some(m) = build_layer_mask(rl, header) {
                    g.mask = Some(m);
                    g.mask_active = true;
                }
                group_ids.insert(gid);
                out.push(g);
            }
            _ => {
                // Adjustment layer we decoded → an editable iAi adjustment,
                // canvas-sized, carrying its own mask/opacity/blend/visibility.
                if let Some(adj) = rl.adjustment.clone() {
                    let id = next_id;
                    next_id += 1;
                    let mut l = Layer::new_adjustment(id, adj, cw, ch);
                    if !rl.name.is_empty() {
                        l.name = rl.name.clone();
                    }
                    l.opacity = (rl.opacity as f32) / 255.0;
                    l.blend_mode = map_blend(&rl.blend);
                    l.visible = rl.visible;
                    l.parent_id = stack.last().copied();
                    if let Some(m) = build_canvas_mask(rl, header, cw, ch) {
                        l.mask = Some(m);
                        l.mask_active = true;
                    }
                    out.push(l);
                    continue;
                }
                // A recognised adjustment layer we can't map yet (e.g. Selective
                // Colour): skip it rather than import an empty raster that would
                // punch a transparent hole in the composite.
                if rl.is_adjustment {
                    continue;
                }
                if rl.width() == 0 || rl.height() == 0 {
                    continue; // empty layer — no pixels to import
                }
                let id = next_id;
                next_id += 1;
                if let Some(mut l) = build_raster_layer(id, rl, header, icc) {
                    l.parent_id = stack.last().copied();
                    // A type-tool ('TySh') layer keeps Photoshop's rasterized
                    // pixels for a pixel-perfect look, but becomes an editable iAi
                    // text layer (re-rasterized from `TextData` only on edit).
                    if let Some(td) = rl.text.clone() {
                        l.layer_type = crate::core::layer::LayerType::Text(td);
                    }
                    out.push(l);
                }
            }
        }
    }

    // Drop parent references to groups that never materialised (unbalanced
    // dividers) so no child points at a missing header.
    for l in &mut out {
        if let Some(p) = l.parent_id {
            if !group_ids.contains(&p) {
                l.parent_id = None;
            }
        }
    }
    out
}

fn build_raster_layer(
    id: u32,
    rl: &RawLayer,
    header: &Header,
    icc: Option<&[u8]>,
) -> Option<Layer> {
    let (w, h) = (rl.width(), rl.height());
    let mut layer = Layer::new(id, &name_or(&rl.name, "Layer"), w, h);
    if header.depth == 16 {
        let px16 = assemble_layer_rgba16(rl, header)?;
        layer.tiles = TileMap::from_rgba16(&px16, w, h);
    } else {
        let mut px = assemble_layer_rgba8(rl, header)?;
        // Colour-manage each layer the same way the composite path does.
        super::apply_input_profile(&mut px, icc);
        layer.tiles = TileMap::from_rgba(&px, w, h);
    }
    layer.opacity = (rl.opacity as f32) / 255.0;
    layer.blend_mode = map_blend(&rl.blend);
    layer.visible = rl.visible;
    layer.offset = (rl.left, rl.top);
    if let Some(m) = build_layer_mask(rl, header) {
        layer.mask = Some(m);
        layer.mask_active = true;
    }
    Some(layer)
}

fn assemble_layer_rgba8(rl: &RawLayer, header: &Header) -> Option<Vec<u8>> {
    let n = (rl.width() as usize) * (rl.height() as usize);
    let mut out = vec![0u8; n * 4];
    let alpha = rl.planes.get(&-1);
    let alpha_at = |i: usize| alpha.and_then(|a| a.get(i)).copied().unwrap_or(255);
    if header.color_mode == 3 {
        let (r, g, b) = (rl.planes.get(&0)?, rl.planes.get(&1)?, rl.planes.get(&2)?);
        if r.len() < n || g.len() < n || b.len() < n {
            return None;
        }
        for i in 0..n {
            out[i * 4] = r[i];
            out[i * 4 + 1] = g[i];
            out[i * 4 + 2] = b[i];
            out[i * 4 + 3] = alpha_at(i);
        }
    } else {
        let gray = rl.planes.get(&0)?;
        if gray.len() < n {
            return None;
        }
        for i in 0..n {
            out[i * 4] = gray[i];
            out[i * 4 + 1] = gray[i];
            out[i * 4 + 2] = gray[i];
            out[i * 4 + 3] = alpha_at(i);
        }
    }
    Some(out)
}

fn assemble_layer_rgba16(rl: &RawLayer, header: &Header) -> Option<Vec<u16>> {
    let n = (rl.width() as usize) * (rl.height() as usize);
    let s = |p: &[u8], i: usize| -> u16 {
        p.get(i * 2..i * 2 + 2)
            .map(|b| u16::from_be_bytes([b[0], b[1]]))
            .unwrap_or(0)
    };
    let mut out = vec![0u16; n * 4];
    let alpha = rl.planes.get(&-1);
    let alpha_at = |i: usize| alpha.map_or(u16::MAX, |a| s(a, i));
    if header.color_mode == 3 {
        let (r, g, b) = (rl.planes.get(&0)?, rl.planes.get(&1)?, rl.planes.get(&2)?);
        if r.len() < n * 2 || g.len() < n * 2 || b.len() < n * 2 {
            return None;
        }
        for i in 0..n {
            out[i * 4] = s(r, i);
            out[i * 4 + 1] = s(g, i);
            out[i * 4 + 2] = s(b, i);
            out[i * 4 + 3] = alpha_at(i);
        }
    } else {
        let gray = rl.planes.get(&0)?;
        if gray.len() < n * 2 {
            return None;
        }
        for i in 0..n {
            let v = s(gray, i);
            out[i * 4] = v;
            out[i * 4 + 1] = v;
            out[i * 4 + 2] = v;
            out[i * 4 + 3] = alpha_at(i);
        }
    }
    Some(out)
}

/// Build a layer-aligned mask from the PSD mask channel: a layer-sized field
/// filled with the mask's default colour, with the mask rect blitted at its
/// offset relative to the layer origin.
fn build_layer_mask(rl: &RawLayer, header: &Header) -> Option<LayerMask> {
    let m = rl.mask.as_ref()?;
    let mw = (m.right - m.left).max(0) as u32;
    let mh = (m.bottom - m.top).max(0) as u32;
    if mw == 0 || mh == 0 {
        return None;
    }
    let plane = rl.planes.get(&-2).or_else(|| rl.planes.get(&-3))?;
    let (lw, lh) = (rl.width().max(1), rl.height().max(1));
    let mut mask = if m.default_color >= 128 {
        LayerMask::new_white(lw, lh)
    } else {
        LayerMask::new_black(lw, lh)
    };
    let bps = (header.depth / 8) as usize;
    let sample = |i: usize| -> u8 {
        if bps == 2 {
            plane.get(i * 2).copied().unwrap_or(0) // high byte of the BE u16
        } else {
            plane.get(i).copied().unwrap_or(0)
        }
    };
    let (ox, oy) = (m.left - rl.left, m.top - rl.top);
    for my in 0..mh {
        let ly = oy + my as i32;
        if ly < 0 || ly >= lh as i32 {
            continue;
        }
        for mx in 0..mw {
            let lx = ox + mx as i32;
            if lx < 0 || lx >= lw as i32 {
                continue;
            }
            let v = sample((my * mw + mx) as usize);
            mask.tiles.set_pixel(lx as u32, ly as u32, v, v, v, 255);
        }
    }
    mask.enabled = !m.disabled;
    Some(mask)
}

/// Canvas-sized mask for a bounds-less layer (an adjustment layer). The PSD mask
/// rect is in absolute image coordinates, so its pixels blit straight onto a
/// canvas-sized mask — unlike [`build_layer_mask`], which works relative to a
/// raster layer's own origin.
fn build_canvas_mask(rl: &RawLayer, header: &Header, cw: u32, ch: u32) -> Option<LayerMask> {
    let m = rl.mask.as_ref()?;
    let mw = (m.right - m.left).max(0) as u32;
    let mh = (m.bottom - m.top).max(0) as u32;
    if mw == 0 || mh == 0 {
        return None;
    }
    let plane = rl.planes.get(&-2).or_else(|| rl.planes.get(&-3))?;
    let mut mask = if m.default_color >= 128 {
        LayerMask::new_white(cw, ch)
    } else {
        LayerMask::new_black(cw, ch)
    };
    let bps = (header.depth / 8) as usize;
    let sample = |i: usize| -> u8 {
        if bps == 2 {
            plane.get(i * 2).copied().unwrap_or(0) // high byte of the BE u16
        } else {
            plane.get(i).copied().unwrap_or(0)
        }
    };
    for my in 0..mh {
        let cy = m.top + my as i32;
        if cy < 0 || cy >= ch as i32 {
            continue;
        }
        for mx in 0..mw {
            let cx = m.left + mx as i32;
            if cx < 0 || cx >= cw as i32 {
                continue;
            }
            let v = sample((my * mw + mx) as usize);
            mask.tiles.set_pixel(cx as u32, cy as u32, v, v, v, 255);
        }
    }
    mask.enabled = !m.disabled;
    Some(mask)
}

fn assemble_canvas(layers: Vec<Layer>, header: &Header, icc: Option<&[u8]>) -> Canvas {
    let (w, h) = (header.width, header.height);
    let mut canvas = Canvas::new(w, h);
    let top = layers.len().saturating_sub(1);
    canvas.layer_stack.layers = layers;
    canvas.layer_stack.active_idx = top;
    canvas.layer_stack.repair_next_id();
    if header.depth == 16 {
        canvas.bit_depth = BitDepth::Sixteen;
    }
    // The per-layer path always lands in the sRGB working space (8-bit layers
    // colour-managed above; 16-bit only taken when already sRGB).
    canvas.icc_profile = crate::core::canvas::IccProfile {
        name: crate::core::cms::WorkingProfile::Srgb.name().to_string(),
        data: crate::core::cms::srgb_icc_bytes(),
    };
    canvas.metadata.source_profile = icc
        .and_then(crate::core::cms::profile_from_bytes)
        .map(|p| crate::core::cms::profile_name(&p))
        .unwrap_or_default();
    canvas.pixels = if Canvas::fits_flat_buffer(w, h) {
        canvas.layer_stack.flatten(w, h)
    } else {
        Vec::new()
    };
    canvas
}

pub struct PsdExporter;

impl Exporter for PsdExporter {
    fn extensions(&self) -> &[&str] {
        &["psd"]
    }

    fn export(&self, _canvas: &Canvas, _path: &Path, _opts: &ExportOptions) -> Result<(), String> {
        Err("PSD export not yet supported. Please use PNG or iAi format.".to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;

    /// Build a minimal PSD/PSB file around the given ImageData section bytes.
    fn build_psd(
        version: u16,
        channels: u16,
        w: u32,
        h: u32,
        depth: u16,
        mode: u16,
        resources: &[u8],
        image_data: &[u8],
    ) -> Vec<u8> {
        let mut v = Vec::new();
        v.extend_from_slice(b"8BPS");
        v.extend_from_slice(&version.to_be_bytes());
        v.extend_from_slice(&[0u8; 6]);
        v.extend_from_slice(&channels.to_be_bytes());
        v.extend_from_slice(&h.to_be_bytes());
        v.extend_from_slice(&w.to_be_bytes());
        v.extend_from_slice(&depth.to_be_bytes());
        v.extend_from_slice(&mode.to_be_bytes());
        v.extend_from_slice(&0u32.to_be_bytes()); // colour mode data
        v.extend_from_slice(&(resources.len() as u32).to_be_bytes());
        v.extend_from_slice(resources);
        if version == 2 {
            v.extend_from_slice(&0u64.to_be_bytes()); // layer & mask (PSB)
        } else {
            v.extend_from_slice(&0u32.to_be_bytes());
        }
        v.extend_from_slice(image_data);
        v
    }

    /// PackBits-encode one row as a single literal run (rows ≤ 128 bytes).
    fn packbits_literal(row: &[u8]) -> Vec<u8> {
        assert!(!row.is_empty() && row.len() <= 128);
        let mut v = vec![(row.len() - 1) as u8];
        v.extend_from_slice(row);
        v
    }

    fn zlib(data: &[u8]) -> Vec<u8> {
        let mut enc = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
        enc.write_all(data).unwrap();
        enc.finish().unwrap()
    }

    fn pixel(canvas: &Canvas, x: u32, y: u32) -> (u8, u8, u8, u8) {
        canvas.layer_stack.layers[0].tiles.get_pixel(x, y)
    }

    #[test]
    fn raw_rgb8_no_alpha() {
        // 2×1: red then blue. Planar: R=[255,0] G=[0,0] B=[0,255].
        let mut img = vec![0u8, 0]; // compression RAW
        img.extend_from_slice(&[255, 0, 0, 0, 0, 255]);
        let psd = build_psd(1, 3, 2, 1, 8, 3, &[], &img);
        let canvas = import_bytes(&psd).unwrap();
        assert_eq!((canvas.width, canvas.height), (2, 1));
        assert_eq!(pixel(&canvas, 0, 0), (255, 0, 0, 255));
        assert_eq!(pixel(&canvas, 1, 0), (0, 0, 255, 255));
    }

    #[test]
    fn raw_rgba8_alpha_channel() {
        let mut img = vec![0u8, 0];
        img.extend_from_slice(&[10, 20, 30, 40, 50, 60, 128, 255]); // R,G,B,A planes 2×1
        let psd = build_psd(1, 4, 2, 1, 8, 3, &[], &img);
        let canvas = import_bytes(&psd).unwrap();
        assert_eq!(pixel(&canvas, 0, 0), (10, 30, 50, 128));
        assert_eq!(pixel(&canvas, 1, 0), (20, 40, 60, 255));
    }

    #[test]
    fn rle_rgb8() {
        // 2×2 all-literal PackBits, PSD u16 row counts.
        let planes: [[u8; 4]; 3] = [[1, 2, 3, 4], [5, 6, 7, 8], [9, 10, 11, 12]];
        let mut img = vec![0u8, 1]; // compression RLE
        let mut packed_rows = Vec::new();
        let mut counts = Vec::new();
        for plane in &planes {
            for row in plane.chunks(2) {
                let packed = packbits_literal(row);
                counts.extend_from_slice(&(packed.len() as u16).to_be_bytes());
                packed_rows.extend_from_slice(&packed);
            }
        }
        img.extend_from_slice(&counts);
        img.extend_from_slice(&packed_rows);
        let psd = build_psd(1, 3, 2, 2, 8, 3, &[], &img);
        let canvas = import_bytes(&psd).unwrap();
        assert_eq!(pixel(&canvas, 0, 0), (1, 5, 9, 255));
        assert_eq!(pixel(&canvas, 1, 1), (4, 8, 12, 255));
    }

    #[test]
    fn rle_repeat_run() {
        // 4×1 grayscale, one repeat run: header -3 → repeat next byte 4×.
        let mut img = vec![0u8, 1];
        let packed = [(-3i8) as u8, 77];
        img.extend_from_slice(&(packed.len() as u16).to_be_bytes());
        img.extend_from_slice(&packed);
        let psd = build_psd(1, 1, 4, 1, 8, 1, &[], &img);
        let canvas = import_bytes(&psd).unwrap();
        assert_eq!(pixel(&canvas, 0, 0), (77, 77, 77, 255));
        assert_eq!(pixel(&canvas, 3, 0), (77, 77, 77, 255));
    }

    #[test]
    fn psb_rle_u32_counts() {
        // Same as rle_rgb8 but version 2: u64 layer length + u32 RLE counts.
        let planes: [[u8; 2]; 3] = [[1, 2], [3, 4], [5, 6]];
        let mut img = vec![0u8, 1];
        let mut packed_rows = Vec::new();
        let mut counts = Vec::new();
        for plane in &planes {
            let packed = packbits_literal(plane);
            counts.extend_from_slice(&(packed.len() as u32).to_be_bytes());
            packed_rows.extend_from_slice(&packed);
        }
        img.extend_from_slice(&counts);
        img.extend_from_slice(&packed_rows);
        let psb = build_psd(2, 3, 2, 1, 8, 3, &[], &img);
        let canvas = import_bytes(&psb).unwrap();
        assert_eq!(pixel(&canvas, 0, 0), (1, 3, 5, 255));
        assert_eq!(pixel(&canvas, 1, 0), (2, 4, 6, 255));
    }

    #[test]
    fn raw_rgb16_preserves_precision() {
        // 1×1, 16-bit BE samples: R=0x1234 G=0x5678 B=0x9ABC.
        let mut img = vec![0u8, 0];
        img.extend_from_slice(&[0x12, 0x34, 0x56, 0x78, 0x9A, 0xBC]);
        let psd = build_psd(1, 3, 1, 1, 16, 3, &[], &img);
        let canvas = import_bytes(&psd).unwrap();
        let px16 = canvas.layer_stack.layers[0].tiles.flatten16();
        assert_eq!(&px16[0..4], &[0x1234, 0x5678, 0x9ABC, u16::MAX]);
    }

    #[test]
    fn zip_without_prediction() {
        // 2×1 RGB, zlib-compressed planar data.
        let planar = [255u8, 0, 0, 255, 0, 0];
        let mut img = vec![0u8, 2];
        img.extend_from_slice(&zlib(&planar));
        let psd = build_psd(1, 3, 2, 1, 8, 3, &[], &img);
        let canvas = import_bytes(&psd).unwrap();
        assert_eq!(pixel(&canvas, 0, 0), (255, 0, 0, 255));
        assert_eq!(pixel(&canvas, 1, 0), (0, 255, 0, 255));
    }

    #[test]
    fn zip_with_prediction_16bit() {
        // 2×1 16-bit gray. Actual samples 0x0100, 0x0105 → delta row
        // [0x0100, 0x0005] before compression.
        let delta = [0x01u8, 0x00, 0x00, 0x05];
        let mut img = vec![0u8, 3];
        img.extend_from_slice(&zlib(&delta));
        let psd = build_psd(1, 1, 2, 1, 16, 1, &[], &img);
        let canvas = import_bytes(&psd).unwrap();
        let px16 = canvas.layer_stack.layers[0].tiles.flatten16();
        assert_eq!(px16[0], 0x0100);
        assert_eq!(px16[4], 0x0105);
    }

    #[test]
    fn zip_with_prediction_8bit() {
        // 3×1 gray, samples 10,20,30 → delta [10,10,10].
        let delta = [10u8, 10, 10];
        let mut img = vec![0u8, 3];
        img.extend_from_slice(&zlib(&delta));
        let psd = build_psd(1, 1, 3, 1, 8, 1, &[], &img);
        let canvas = import_bytes(&psd).unwrap();
        assert_eq!(pixel(&canvas, 2, 0), (30, 30, 30, 255));
    }

    fn expect_err(data: &[u8]) -> String {
        match import_bytes(data) {
            Err(e) => e,
            Ok(_) => panic!("expected an import error"),
        }
    }

    #[test]
    fn cmyk_rejected_with_clear_message() {
        let img = vec![0u8, 0];
        let psd = build_psd(1, 4, 1, 1, 8, 4, &[], &img);
        let err = expect_err(&psd);
        assert!(err.contains("CMYK"), "{err}");
    }

    #[test]
    fn depth32_rejected_with_clear_message() {
        let img = vec![0u8, 0];
        let psd = build_psd(1, 3, 1, 1, 32, 3, &[], &img);
        let err = expect_err(&psd);
        assert!(err.contains("32-bit"), "{err}");
    }

    #[test]
    fn truncated_file_is_error_not_panic() {
        let img = vec![0u8, 0, 1, 2]; // RAW but far too short for 2×2×3
        let psd = build_psd(1, 3, 2, 2, 8, 3, &[], &img);
        assert!(import_bytes(&psd).is_err());
        // Header cut mid-way.
        assert!(import_bytes(&psd[..10]).is_err());
    }

    #[test]
    fn icc_resource_is_found() {
        // Resource block: 8BIM, id 1039, empty name (padded), ICC payload.
        let icc_payload = [1u8, 2, 3, 4];
        let mut res = Vec::new();
        res.extend_from_slice(b"8BIM");
        res.extend_from_slice(&1039u16.to_be_bytes());
        res.extend_from_slice(&[0, 0]); // empty pascal name, padded to 2
        res.extend_from_slice(&(icc_payload.len() as u32).to_be_bytes());
        res.extend_from_slice(&icc_payload);
        assert_eq!(find_icc_resource(&res), Some(icc_payload.to_vec()));
    }

    // --- Layer & Mask section (editable layer stack) ---------------------------

    /// Build one layer record + its RAW (uncompressed) channel image data.
    /// `channels` = (id, plane bytes); `section` is the lsct group type (0 = none);
    /// `mask` = (top,left,bottom,right, default_color) for a layer mask (the -2
    /// channel plane must be supplied in `channels`).
    #[allow(clippy::too_many_arguments)]
    fn layer_record(
        rect: (i32, i32, i32, i32),
        name: &str,
        opacity: u8,
        blend: &[u8; 4],
        section: u8,
        mask: Option<(i32, i32, i32, i32, u8)>,
        channels: &[(i16, Vec<u8>)],
    ) -> (Vec<u8>, Vec<u8>) {
        let (top, left, bottom, right) = rect;
        let mut rec = Vec::new();
        for v in [top, left, bottom, right] {
            rec.extend_from_slice(&v.to_be_bytes());
        }
        rec.extend_from_slice(&(channels.len() as u16).to_be_bytes());
        for (id, plane) in channels {
            rec.extend_from_slice(&id.to_be_bytes());
            rec.extend_from_slice(&((2 + plane.len()) as u32).to_be_bytes());
        }
        rec.extend_from_slice(b"8BIM");
        rec.extend_from_slice(blend);
        rec.push(opacity);
        rec.push(0); // clipping
        rec.push(0); // flags: visible
        rec.push(0); // filler

        let mut extra = Vec::new();
        if let Some((mt, ml, mb, mr, dc)) = mask {
            let mut mblock = Vec::new();
            for v in [mt, ml, mb, mr] {
                mblock.extend_from_slice(&v.to_be_bytes());
            }
            mblock.push(dc); // default colour
            mblock.push(0); // mask flags
            mblock.extend_from_slice(&[0, 0]); // pad to 20
            extra.extend_from_slice(&(mblock.len() as u32).to_be_bytes());
            extra.extend_from_slice(&mblock);
        } else {
            extra.extend_from_slice(&0u32.to_be_bytes()); // mask len
        }
        extra.extend_from_slice(&0u32.to_be_bytes()); // blending ranges len
        let nb = name.as_bytes();
        extra.push(nb.len() as u8);
        extra.extend_from_slice(nb);
        let pad = (4 - ((1 + nb.len()) % 4)) % 4;
        extra.extend(std::iter::repeat(0u8).take(pad));
        if section != 0 {
            extra.extend_from_slice(b"8BIM");
            extra.extend_from_slice(b"lsct");
            extra.extend_from_slice(&4u32.to_be_bytes());
            extra.extend_from_slice(&(section as u32).to_be_bytes());
        }
        rec.extend_from_slice(&(extra.len() as u32).to_be_bytes());
        rec.extend_from_slice(&extra);

        let mut cdata = Vec::new();
        for (_id, plane) in channels {
            cdata.extend_from_slice(&0u16.to_be_bytes()); // RAW compression
            cdata.extend_from_slice(plane);
        }
        (rec, cdata)
    }

    /// Assemble a full PSD around `records` (file order = bottom→top) with a
    /// dummy flat composite so the fallback path still decodes.
    fn build_layered_psd(w: u32, h: u32, records: &[(Vec<u8>, Vec<u8>)]) -> Vec<u8> {
        let mut layer_info = Vec::new();
        layer_info.extend_from_slice(&(records.len() as i16).to_be_bytes());
        for (rec, _) in records {
            layer_info.extend_from_slice(rec);
        }
        for (_, cd) in records {
            layer_info.extend_from_slice(cd);
        }
        let mut lm = Vec::new();
        lm.extend_from_slice(&(layer_info.len() as u32).to_be_bytes());
        lm.extend_from_slice(&layer_info);

        let mut v = Vec::new();
        v.extend_from_slice(b"8BPS");
        v.extend_from_slice(&1u16.to_be_bytes());
        v.extend_from_slice(&[0u8; 6]);
        v.extend_from_slice(&3u16.to_be_bytes()); // composite channels
        v.extend_from_slice(&h.to_be_bytes());
        v.extend_from_slice(&w.to_be_bytes());
        v.extend_from_slice(&8u16.to_be_bytes()); // depth
        v.extend_from_slice(&3u16.to_be_bytes()); // RGB
        v.extend_from_slice(&0u32.to_be_bytes()); // colour mode data
        v.extend_from_slice(&0u32.to_be_bytes()); // image resources
        v.extend_from_slice(&(lm.len() as u32).to_be_bytes());
        v.extend_from_slice(&lm);
        // Dummy composite: RAW, 3 zero planes.
        v.extend_from_slice(&0u16.to_be_bytes());
        v.extend(std::iter::repeat(0u8).take((w * h * 3) as usize));
        v
    }

    #[test]
    fn imports_two_flat_layers() {
        // 2×1 doc. Bottom "Base" = red|blue; top "Top" = solid green, 50% multiply.
        let base = layer_record(
            (0, 0, 1, 2),
            "Base",
            255,
            b"norm",
            0,
            None,
            &[(0, vec![255, 0]), (1, vec![0, 0]), (2, vec![0, 255])],
        );
        let top = layer_record(
            (0, 0, 1, 2),
            "Top",
            128,
            b"mul ",
            0,
            None,
            &[(0, vec![0, 0]), (1, vec![255, 255]), (2, vec![0, 0])],
        );
        let psd = build_layered_psd(2, 1, &[base, top]);
        let canvas = import_bytes(&psd).unwrap();

        let layers = &canvas.layer_stack.layers;
        assert_eq!(layers.len(), 2, "two layers imported, not the composite");
        assert_eq!(layers[0].name, "Base");
        assert_eq!(layers[1].name, "Top");
        assert!((layers[1].opacity - 128.0 / 255.0).abs() < 1e-3);
        assert_eq!(layers[1].blend_mode, BlendMode::Multiply);
        assert_eq!(layers[1].offset, (0, 0));
        assert_eq!(layers[1].tiles.get_pixel(0, 0), (0, 255, 0, 255));
        assert_eq!(layers[0].tiles.get_pixel(1, 0), (0, 0, 255, 255));
    }

    #[test]
    fn imports_nested_group() {
        // File order bottom→top: [</Layer group> divider, child, folder header].
        let divider = layer_record((0, 0, 0, 0), "", 255, b"norm", 3, None, &[]);
        let child = layer_record(
            (0, 0, 1, 2),
            "Child",
            255,
            b"norm",
            0,
            None,
            &[(0, vec![9, 9]), (1, vec![9, 9]), (2, vec![9, 9])],
        );
        let folder = layer_record((0, 0, 0, 0), "G", 255, b"pass", 1, None, &[]);
        let psd = build_layered_psd(2, 1, &[divider, child, folder]);
        let canvas = import_bytes(&psd).unwrap();

        let layers = &canvas.layer_stack.layers;
        assert_eq!(layers.len(), 2, "child + group header (divider skipped)");
        assert_eq!(layers[0].name, "Child");
        assert!(layers[1].is_group());
        assert_eq!(layers[1].name, "G");
        assert!(layers[1].expanded, "lsct type 1 = open folder");
        assert_eq!(
            layers[0].parent_id,
            Some(layers[1].id),
            "child re-parented to the group header"
        );
    }

    #[test]
    fn imports_layer_mask() {
        // A 2×1 layer with a mask: left pixel masked out (0), right revealed (255).
        let masked = layer_record(
            (0, 0, 1, 2),
            "Masked",
            255,
            b"norm",
            0,
            Some((0, 0, 1, 2, 255)),
            &[
                (0, vec![10, 20]),
                (1, vec![30, 40]),
                (2, vec![50, 60]),
                (-2, vec![0, 255]),
            ],
        );
        let psd = build_layered_psd(2, 1, &[masked]);
        let canvas = import_bytes(&psd).unwrap();

        let layer = &canvas.layer_stack.layers[0];
        assert!(layer.mask_active);
        let mask = layer.mask.as_ref().expect("mask imported");
        assert_eq!(mask.tiles.get_pixel(0, 0).0, 0, "left pixel masked");
        assert_eq!(mask.tiles.get_pixel(1, 0).0, 255, "right pixel revealed");
    }

    /// A zero-bounds adjustment layer: no channels, a single additional-info
    /// block carrying the adjustment parameters.
    fn adjustment_layer_record(name: &str, key: &[u8; 4], block: &[u8]) -> (Vec<u8>, Vec<u8>) {
        let mut rec = Vec::new();
        for v in [0i32, 0, 0, 0] {
            rec.extend_from_slice(&v.to_be_bytes());
        }
        rec.extend_from_slice(&0u16.to_be_bytes()); // 0 channels
        rec.extend_from_slice(b"8BIM");
        rec.extend_from_slice(b"norm");
        rec.push(255); // opacity
        rec.push(0); // clipping
        rec.push(0); // flags: visible
        rec.push(0); // filler

        let mut extra = Vec::new();
        extra.extend_from_slice(&0u32.to_be_bytes()); // mask len
        extra.extend_from_slice(&0u32.to_be_bytes()); // blending ranges len
        let nb = name.as_bytes();
        extra.push(nb.len() as u8);
        extra.extend_from_slice(nb);
        let pad = (4 - ((1 + nb.len()) % 4)) % 4;
        extra.extend(std::iter::repeat(0u8).take(pad));
        extra.extend_from_slice(b"8BIM");
        extra.extend_from_slice(key);
        extra.extend_from_slice(&(block.len() as u32).to_be_bytes());
        extra.extend_from_slice(block);
        if block.len() % 2 == 1 {
            extra.push(0); // even-padding
        }
        rec.extend_from_slice(&(extra.len() as u32).to_be_bytes());
        rec.extend_from_slice(&extra);
        (rec, Vec::new())
    }

    fn levl_block() -> Vec<u8> {
        let mut d = Vec::new();
        d.extend_from_slice(&2u16.to_be_bytes());
        for i in 0..29 {
            let rec = if i == 0 {
                [10i16, 245, 0, 255, 130]
            } else {
                [0i16, 255, 0, 255, 100]
            };
            for v in rec {
                d.extend_from_slice(&v.to_be_bytes());
            }
        }
        d
    }

    #[test]
    fn imports_levels_adjustment_layer() {
        let base = layer_record(
            (0, 0, 1, 2),
            "Base",
            255,
            b"norm",
            0,
            None,
            &[
                (0, vec![100, 100]),
                (1, vec![100, 100]),
                (2, vec![100, 100]),
            ],
        );
        let adj = adjustment_layer_record("My Levels", b"levl", &levl_block());
        let psd = build_layered_psd(2, 1, &[base, adj]);
        let canvas = import_bytes(&psd).unwrap();

        let layers = &canvas.layer_stack.layers;
        assert_eq!(
            layers.len(),
            2,
            "base raster + adjustment layer, not dropped"
        );
        assert_eq!(layers[1].name, "My Levels");
        match &layers[1].layer_type {
            crate::core::layer::LayerType::Adjustment(AdjustmentType::Levels { channels }) => {
                assert_eq!(channels[0].in_black, 10);
                assert_eq!(channels[0].in_white, 245);
                assert!((channels[0].gamma - 1.30).abs() < 1e-4);
            }
            _ => panic!("expected a Levels adjustment layer"),
        }
        // Adjustment layers are canvas-sized so they cover the whole document.
        assert_eq!((layers[1].width, layers[1].height), (2, 1));
    }

    /// Version-1 Curves, master channel only (bitmask bit 0), lifting the midtone.
    fn curv_block() -> Vec<u8> {
        let mut d = Vec::new();
        d.push(0); // is_map = points
        d.extend_from_slice(&1u16.to_be_bytes()); // version
        d.extend_from_slice(&0b0001u32.to_be_bytes()); // master only
        d.extend_from_slice(&3u16.to_be_bytes()); // point count
        for (out, inp) in [(0u16, 0u16), (160, 128), (255, 255)] {
            d.extend_from_slice(&out.to_be_bytes());
            d.extend_from_slice(&inp.to_be_bytes());
        }
        d
    }

    #[test]
    fn imports_curves_adjustment_layer() {
        let base = layer_record(
            (0, 0, 1, 2),
            "Base",
            255,
            b"norm",
            0,
            None,
            &[
                (0, vec![100, 100]),
                (1, vec![100, 100]),
                (2, vec![100, 100]),
            ],
        );
        let adj = adjustment_layer_record("My Curves", b"curv", &curv_block());
        let psd = build_layered_psd(2, 1, &[base, adj]);
        let canvas = import_bytes(&psd).unwrap();

        let layers = &canvas.layer_stack.layers;
        assert_eq!(layers.len(), 2, "base raster + curves layer, not dropped");
        assert_eq!(layers[1].name, "My Curves");
        match &layers[1].layer_type {
            crate::core::layer::LayerType::Adjustment(AdjustmentType::Curves { channels }) => {
                assert_eq!(channels[0].len(), 3);
                assert!((channels[0][1].0 - 128.0 / 255.0).abs() < 1e-4);
                assert!((channels[0][1].1 - 160.0 / 255.0).abs() < 1e-4);
            }
            _ => panic!("expected a Curves adjustment layer"),
        }
        assert_eq!((layers[1].width, layers[1].height), (2, 1));
    }

    /// A minimal `TySh` block: identity transform + a text descriptor whose only
    /// item is the `Txt ` string. Enough to exercise the import wiring.
    fn tysh_block(text: &str) -> Vec<u8> {
        let mut d = Vec::new();
        d.extend_from_slice(&1u16.to_be_bytes()); // version
                                                  // identity transform (xx, xy, yx, yy, tx, ty)
        for x in [1.0f64, 0.0, 0.0, 1.0, 0.0, 0.0] {
            d.extend_from_slice(&x.to_be_bytes());
        }
        d.extend_from_slice(&50u16.to_be_bytes()); // text version
        d.extend_from_slice(&16u32.to_be_bytes()); // descriptor version
        d.extend_from_slice(&1u32.to_be_bytes()); // unicode name len (1 = NUL only)
        d.extend_from_slice(&0u16.to_be_bytes()); // NUL
        d.extend_from_slice(&4u32.to_be_bytes()); // classID "TxLr"
        d.extend_from_slice(b"TxLr");
        d.extend_from_slice(&1u32.to_be_bytes()); // item count
        d.extend_from_slice(&4u32.to_be_bytes()); // key "Txt "
        d.extend_from_slice(b"Txt ");
        d.extend_from_slice(b"TEXT");
        let units: Vec<u16> = text.encode_utf16().collect();
        d.extend_from_slice(&(units.len() as u32 + 1).to_be_bytes());
        for u in units {
            d.extend_from_slice(&u.to_be_bytes());
        }
        d.extend_from_slice(&0u16.to_be_bytes()); // NUL
        d
    }

    /// A raster layer that also carries a `TySh` type-tool block, appended to the
    /// record's extra-data section (with the extra-length field patched to match).
    fn text_layer_record(name: &str, text: &str) -> (Vec<u8>, Vec<u8>) {
        let channels: &[(i16, Vec<u8>)] =
            &[(0, vec![200, 200]), (1, vec![40, 40]), (2, vec![40, 40])];
        let (mut rec, cdata) = layer_record((0, 0, 1, 2), name, 255, b"norm", 0, None, channels);

        // Build the 8BIM/TySh additional-info block (even-padded).
        let block = tysh_block(text);
        let mut tysh = Vec::new();
        tysh.extend_from_slice(b"8BIM");
        tysh.extend_from_slice(b"TySh");
        tysh.extend_from_slice(&(block.len() as u32).to_be_bytes());
        tysh.extend_from_slice(&block);
        if block.len() % 2 == 1 {
            tysh.push(0);
        }

        // The extra-length u32 sits right after the 4-byte-aligned per-layer header:
        // 16 (bounds) + 2 (channel count) + 6*n (channel infos) + 4 (8BIM) + 4
        // (blend) + 4 (opacity/clip/flags/filler). Patch it to include the block.
        let extra_len_pos = 16 + 2 + 6 * channels.len() + 4 + 4 + 4;
        let old = u32::from_be_bytes([
            rec[extra_len_pos],
            rec[extra_len_pos + 1],
            rec[extra_len_pos + 2],
            rec[extra_len_pos + 3],
        ]);
        let new = old + tysh.len() as u32;
        rec[extra_len_pos..extra_len_pos + 4].copy_from_slice(&new.to_be_bytes());
        rec.extend_from_slice(&tysh);
        (rec, cdata)
    }

    #[test]
    fn imports_text_layer_as_editable() {
        let base = layer_record(
            (0, 0, 1, 2),
            "BG",
            255,
            b"norm",
            0,
            None,
            &[(0, vec![1, 1]), (1, vec![1, 1]), (2, vec![1, 1])],
        );
        let text = text_layer_record("ignored-pascal", "Hello world");
        let psd = build_layered_psd(2, 1, &[base, text]);
        let canvas = import_bytes(&psd).unwrap();

        let layers = &canvas.layer_stack.layers;
        assert_eq!(layers.len(), 2, "base + text layer");
        match &layers[1].layer_type {
            crate::core::layer::LayerType::Text(td) => {
                assert_eq!(td.content, "Hello world");
            }
            other => panic!("expected an editable Text layer, got {other:?}"),
        }
        // The rasterized pixels are still imported for the initial appearance.
        assert_eq!((layers[1].width, layers[1].height), (2, 1));
    }

    /// Build a bare layer-info blob (count + one record + its channel data), the
    /// payload a TIFF `Layr` block carries.
    fn one_layer_info_blob() -> Vec<u8> {
        let l = layer_record(
            (0, 0, 1, 2),
            "TiffLayer",
            255,
            b"norm",
            0,
            None,
            &[(0, vec![11, 22]), (1, vec![33, 44]), (2, vec![55, 66])],
        );
        let mut blob = Vec::new();
        blob.extend_from_slice(&1i16.to_be_bytes()); // layer count
        blob.extend_from_slice(&l.0); // record
        blob.extend_from_slice(&l.1); // channel image data
        blob
    }

    #[test]
    fn tiff_layer_block_count_first_framing() {
        let block = one_layer_info_blob();
        let canvas =
            import_tiff_photoshop_layers(&block, 8, 2, 1, None, true).expect("layered canvas");
        assert_eq!(canvas.layer_stack.layers.len(), 1);
        assert_eq!(canvas.layer_stack.layers[0].name, "TiffLayer");
        assert_eq!(
            canvas.layer_stack.layers[0].tiles.get_pixel(0, 0),
            (11, 33, 55, 255)
        );
    }

    #[test]
    fn tiff_layer_block_length_prefixed_framing() {
        // Some writers prefix the layer info with its 4-byte length; the parser
        // must fall through framing A (count-first) to framing B.
        let inner = one_layer_info_blob();
        let mut block = Vec::new();
        block.extend_from_slice(&(inner.len() as u32).to_be_bytes());
        block.extend_from_slice(&inner);
        let canvas =
            import_tiff_photoshop_layers(&block, 8, 2, 1, None, true).expect("layered canvas");
        assert_eq!(canvas.layer_stack.layers.len(), 1);
        assert_eq!(canvas.layer_stack.layers[0].name, "TiffLayer");
    }

    #[test]
    fn tiff_layer_block_rejects_garbage_and_32bit() {
        assert!(import_tiff_photoshop_layers(&[0xAB; 40], 8, 2, 1, None, true).is_none());
        assert!(
            import_tiff_photoshop_layers(&one_layer_info_blob(), 32, 2, 1, None, true).is_none()
        );
    }

    #[test]
    fn unmapped_adjustment_layer_is_skipped_not_a_hole() {
        // 'selc' (Selective Colour) is recognised as an adjustment but not decoded
        // yet — the layer must be dropped, never imported as an empty (transparent)
        // raster that would punch a hole in the composite.
        let base = layer_record(
            (0, 0, 1, 2),
            "Base",
            255,
            b"norm",
            0,
            None,
            &[
                (0, vec![100, 100]),
                (1, vec![100, 100]),
                (2, vec![100, 100]),
            ],
        );
        let adj = adjustment_layer_record("Selective Colour 1", b"selc", &[0u8; 8]);
        let psd = build_layered_psd(2, 1, &[base, adj]);
        let canvas = import_bytes(&psd).unwrap();
        let layers = &canvas.layer_stack.layers;
        assert_eq!(
            layers.len(),
            1,
            "only the base raster; unmapped adjustment skipped"
        );
        assert_eq!(layers[0].name, "Base");
    }
}
