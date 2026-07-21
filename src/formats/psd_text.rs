//! PSD type-tool ('TySh') decoding (import Phase 3).
//!
//! A Photoshop text layer carries a rasterized copy of its pixels (so it still
//! composites correctly) plus a `TySh` "type tool object setting" block holding
//! the *editable* text: a transform, a text [`Descriptor`], a warp descriptor and
//! a bounding box. We keep Photoshop's raster for a pixel-perfect initial look
//! and attach a [`TextData`] parsed from `TySh`, so the migrated layer becomes an
//! editable iAi text layer (re-rasterized from `TextData` only when the user
//! edits it).
//!
//! Two nested formats are decoded here: the well-specified recursive **descriptor
//! structure** (Objc/VlLs/TEXT/doub/…), and the text engine's **EngineData** blob
//! (an ASCII token tree of `<< >>` dicts and `[ ]` arrays) which holds the font,
//! size and colour. The string itself comes from the descriptor's `Txt ` key, so
//! even if EngineData enrichment fails the text is still editable with a default
//! style. Every reader is bounds-checked and returns `None` (caller keeps the flat
//! raster) rather than guessing.
//!
//! Binary offsets follow the Adobe File Format spec; the EngineData key path
//! (EngineDict → StyleRun → RunArray → StyleSheet → StyleSheetData) is the
//! documented text-engine layout. Real-Photoshop verification is still owed.

use crate::core::text::{TextAlign, TextData, TextFontFamily};

/// The editable text parsed from a `TySh` block.
pub struct TextImport {
    pub td: TextData,
}

/// Parse a `TySh` (type tool object setting) block into editable text.
/// Returns `None` on any short/unexpected field — the caller then keeps the
/// layer's flat raster (visible but not editable).
pub fn parse_type_tool(data: &[u8]) -> Option<TextImport> {
    let mut c = Cur::new(data);
    if c.u16()? != 1 {
        return None; // only the Photoshop 6.0 version 1 layout is defined
    }
    // 6 doubles: xx, xy, yx, yy, tx, ty (affine, PostScript order).
    let m = [c.f64()?, c.f64()?, c.f64()?, c.f64()?, c.f64()?, c.f64()?];
    let _text_version = c.u16()?; // = 50
    let _text_desc_version = c.u32()?; // = 16
    let text_desc = read_descriptor(&mut c)?;
    build_text_import(&text_desc, m)
}

/// Map the text descriptor + transform onto a [`TextData`].
fn build_text_import(desc: &Descriptor, m: [f64; 6]) -> Option<TextImport> {
    let content = match desc.get("Txt ")? {
        Val::Text(s) => s.replace('\r', "\n"), // PSD breaks lines with CR
        _ => return None,
    };

    let mut td = TextData {
        content,
        ..TextData::default()
    };

    // Decompose the affine into rotation + non-uniform scale. Columns are the
    // images of the text-space basis vectors: (xx,xy) and (yx,yy).
    let (xx, xy, yx, yy) = (m[0], m[1], m[2], m[3]);
    let scale_x = (xx * xx + xy * xy).sqrt();
    let scale_y = (yx * yx + yy * yy).sqrt();
    let sy = if scale_y > 1e-6 { scale_y } else { 1.0 };
    td.rotation_deg = xy.atan2(xx).to_degrees() as f32;
    if scale_x > 1e-6 && scale_y > 1e-6 {
        td.stretch_x = (scale_x / scale_y) as f32;
    }

    // EngineData carries font/size/colour — best-effort enrichment.
    if let Some(Val::Raw(bytes)) = desc.get("EngineData") {
        if let Some(style) = engine_data::extract(bytes) {
            if let Some(fs) = style.font_size {
                // FontSize is the base point size; the transform scales it.
                td.font_px = (fs * sy).clamp(1.0, 4096.0) as f32;
            }
            if let Some(col) = style.fill_color {
                td.color = col;
            }
            if let Some(name) = style.font_name {
                let (family, bold, italic) = map_font(&name);
                td.font_family = family;
                td.bold = bold;
                td.italic = italic;
            }
            if style.faux_bold {
                td.bold = true;
            }
            if style.faux_italic {
                td.italic = true;
            }
            if let Some(a) = style.align {
                td.align = a;
            }
            if let Some(tr) = style.tracking {
                // Tracking is in 1/1000 em; convert to canvas pixels.
                let base = style.font_size.unwrap_or(td.font_px as f64);
                td.tracking_px = (tr / 1000.0 * base * sy) as f32;
            }
        }
    }

    Some(TextImport { td })
}

/// Map a PSD PostScript font name (e.g. `Arial-BoldMT`) to an iAi font family
/// plus bold/italic flags decoded from the style suffix.
fn map_font(ps_name: &str) -> (TextFontFamily, bool, bool) {
    let lower = ps_name.to_ascii_lowercase();
    let bold = lower.contains("bold");
    let italic = lower.contains("italic") || lower.contains("oblique");

    // Strip the PostScript style suffix and common vendor tails to recover a
    // human family name: "Arial-BoldMT" → "Arial", "HelveticaNeue" → "Helvetica
    // Neue" is left as-is (System() lets the resolver try it).
    let family_part = ps_name.split('-').next().unwrap_or(ps_name);
    let mut family = family_part
        .trim_end_matches("MT")
        .trim_end_matches("PS")
        .trim();
    if family.is_empty() {
        family = family_part;
    }

    let ff = match family.to_ascii_lowercase().as_str() {
        "arial" | "arialmt" | "helvetica" => TextFontFamily::Arial,
        "calibri" => TextFontFamily::Calibri,
        "tahoma" => TextFontFamily::Tahoma,
        "segoeui" | "segoe ui" => TextFontFamily::SegoeUi,
        _ => TextFontFamily::System(family.to_string()),
    };
    (ff, bold, italic)
}

// ---------------------------------------------------------------------------
// Descriptor structure (recursive key/value tree).
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
enum Val {
    Desc(Descriptor),
    List(Vec<Val>),
    Double(f64),
    UnitFloat(f64),
    Text(String),
    Enum(String),
    Int(i32),
    Bool(bool),
    Raw(Vec<u8>),
    /// A value we parsed enough to skip but don't model.
    Other,
}

#[derive(Debug, Clone, Default)]
struct Descriptor {
    items: Vec<(String, Val)>,
}

impl Descriptor {
    fn get(&self, key: &str) -> Option<&Val> {
        self.items.iter().find(|(k, _)| k == key).map(|(_, v)| v)
    }
}

/// A descriptor: Unicode name, classID, item count, then `count` key/OSType/value
/// triples. The leading `Objc`/`GlbO` OSType (when nested) is consumed by the
/// caller before entering here.
fn read_descriptor(c: &mut Cur) -> Option<Descriptor> {
    let _name = read_unicode(c)?; // "name from classID"
    let _class = read_key(c)?; // classID
    let count = c.u32()? as usize;
    if count > 100_000 {
        return None; // implausible — treat as corrupt
    }
    let mut items = Vec::with_capacity(count.min(64));
    for _ in 0..count {
        let key = read_key(c)?;
        let os = c.take(4)?;
        let os = [os[0], os[1], os[2], os[3]];
        let val = read_value(c, &os)?;
        items.push((key, val));
    }
    Some(Descriptor { items })
}

fn read_value(c: &mut Cur, os: &[u8; 4]) -> Option<Val> {
    match os {
        b"Objc" | b"GlbO" => Some(Val::Desc(read_descriptor(c)?)),
        b"VlLs" => {
            let n = c.u32()? as usize;
            if n > 1_000_000 {
                return None;
            }
            let mut v = Vec::with_capacity(n.min(64));
            for _ in 0..n {
                let os2 = c.take(4)?;
                let os2 = [os2[0], os2[1], os2[2], os2[3]];
                v.push(read_value(c, &os2)?);
            }
            Some(Val::List(v))
        }
        b"doub" => Some(Val::Double(c.f64()?)),
        b"UntF" => {
            let _unit = c.take(4)?;
            Some(Val::UnitFloat(c.f64()?))
        }
        b"TEXT" => Some(Val::Text(read_unicode(c)?)),
        b"enum" => {
            let _type = read_key(c)?;
            Some(Val::Enum(read_key(c)?))
        }
        b"long" => Some(Val::Int(c.u32()? as i32)),
        b"comp" => {
            c.take(8)?;
            Some(Val::Other)
        }
        b"bool" => Some(Val::Bool(c.u8()? != 0)),
        b"type" | b"GlbC" => {
            let _name = read_unicode(c)?;
            let _class = read_key(c)?;
            Some(Val::Other)
        }
        b"tdta" => {
            let n = c.u32()? as usize;
            Some(Val::Raw(c.take(n)?.to_vec()))
        }
        b"alis" => {
            let n = c.u32()? as usize;
            c.take(n)?;
            Some(Val::Other)
        }
        // 'obj ' (Reference) and any unknown OSType have no length we can skip
        // safely — bail so the caller falls back to the flat raster.
        _ => None,
    }
}

/// Descriptor Unicode string: a `u32` length in UTF-16 code units, then that many
/// UTF-16BE units (a trailing NUL terminator is stripped).
fn read_unicode(c: &mut Cur) -> Option<String> {
    let n = c.u32()? as usize;
    if n > 10_000_000 {
        return None;
    }
    let mut units = Vec::with_capacity(n.min(256));
    for _ in 0..n {
        units.push(c.u16()?);
    }
    if units.last() == Some(&0) {
        units.pop();
    }
    Some(String::from_utf16_lossy(&units))
}

/// Descriptor key/classID: a `u32` length; if zero, a 4-byte key, else that many
/// ASCII bytes.
fn read_key(c: &mut Cur) -> Option<String> {
    let n = c.u32()? as usize;
    let bytes = if n == 0 { c.take(4)? } else { c.take(n)? };
    Some(String::from_utf8_lossy(bytes).into_owned())
}

/// Bounds-checked big-endian cursor.
struct Cur<'a> {
    d: &'a [u8],
    p: usize,
}

impl<'a> Cur<'a> {
    fn new(d: &'a [u8]) -> Self {
        Self { d, p: 0 }
    }
    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let s = self.d.get(self.p..self.p.checked_add(n)?)?;
        self.p += n;
        Some(s)
    }
    fn u8(&mut self) -> Option<u8> {
        self.take(1).map(|b| b[0])
    }
    fn u16(&mut self) -> Option<u16> {
        self.take(2).map(|b| u16::from_be_bytes([b[0], b[1]]))
    }
    fn u32(&mut self) -> Option<u32> {
        self.take(4)
            .map(|b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
    }
    fn f64(&mut self) -> Option<f64> {
        self.take(8)
            .map(|b| f64::from_be_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]]))
    }
}

// ---------------------------------------------------------------------------
// EngineData — the text engine's ASCII token tree.
// ---------------------------------------------------------------------------

mod engine_data {
    use super::TextAlign;

    /// The style fields we lift from the first character run.
    #[derive(Default)]
    pub struct StyleExtract {
        pub font_size: Option<f64>,
        pub fill_color: Option<[u8; 4]>,
        pub font_name: Option<String>,
        pub tracking: Option<f64>,
        pub faux_bold: bool,
        pub faux_italic: bool,
        pub align: Option<TextAlign>,
    }

    #[derive(Debug)]
    enum E {
        Dict(Vec<(String, E)>),
        Arr(Vec<E>),
        Num(f64),
        Str(String),
        Bool(bool),
        Name(String),
    }

    impl E {
        fn get(&self, key: &str) -> Option<&E> {
            if let E::Dict(items) = self {
                items.iter().find(|(k, _)| k == key).map(|(_, v)| v)
            } else {
                None
            }
        }
        fn num(&self) -> Option<f64> {
            match self {
                E::Num(n) => Some(*n),
                _ => None,
            }
        }
        fn arr(&self) -> Option<&[E]> {
            match self {
                E::Arr(v) => Some(v),
                _ => None,
            }
        }
    }

    /// Parse EngineData and lift the first style run's font/size/colour and the
    /// first paragraph's justification.
    pub fn extract(bytes: &[u8]) -> Option<StyleExtract> {
        let root = parse(bytes)?;
        let mut out = StyleExtract::default();

        let engine = root.get("EngineDict")?;

        // First character-run style.
        if let Some(ssd) = engine
            .get("StyleRun")
            .and_then(|s| s.get("RunArray"))
            .and_then(|a| a.arr())
            .and_then(|a| a.first())
            .and_then(|r| r.get("StyleSheet"))
            .and_then(|s| s.get("StyleSheetData"))
        {
            out.font_size = ssd.get("FontSize").and_then(|v| v.num());
            out.tracking = ssd.get("Tracking").and_then(|v| v.num());
            out.faux_bold = matches!(ssd.get("FauxBold"), Some(E::Bool(true)));
            out.faux_italic = matches!(ssd.get("FauxItalic"), Some(E::Bool(true)));
            out.fill_color = ssd
                .get("FillColor")
                .and_then(|c| c.get("Values"))
                .and_then(|v| v.arr())
                .and_then(values_to_rgba);

            // Font index → ResourceDict/FontSet[i]/Name.
            if let Some(idx) = ssd.get("Font").and_then(|v| v.num()) {
                out.font_name = root
                    .get("ResourceDict")
                    .and_then(|r| r.get("FontSet"))
                    .and_then(|f| f.arr())
                    .and_then(|f| f.get(idx as usize))
                    .and_then(|entry| entry.get("Name"))
                    .and_then(|n| match n {
                        E::Str(s) => Some(s.clone()),
                        _ => None,
                    });
            }
        }

        // First paragraph justification (0 = left, 1 = right, 2 = centre).
        if let Some(just) = engine
            .get("ParagraphRun")
            .and_then(|p| p.get("RunArray"))
            .and_then(|a| a.arr())
            .and_then(|a| a.first())
            .and_then(|r| r.get("ParagraphSheet"))
            .and_then(|s| s.get("Properties"))
            .and_then(|p| p.get("Justification"))
            .and_then(|v| v.num())
        {
            out.align = Some(match just as i64 {
                1 => TextAlign::Right,
                2 => TextAlign::Center,
                _ => TextAlign::Left,
            });
        }

        Some(out)
    }

    /// FillColor `Values` for an RGB fill are `[alpha, r, g, b]` in 0..1.
    fn values_to_rgba(vals: &[E]) -> Option<[u8; 4]> {
        let n = |i: usize| -> Option<u8> {
            vals.get(i)
                .and_then(|v| v.num())
                .map(|f| (f.clamp(0.0, 1.0) * 255.0).round() as u8)
        };
        match vals.len() {
            // ARGB (RGB colour space).
            4 => Some([n(1)?, n(2)?, n(3)?, n(0)?]),
            // Grayscale: [alpha, gray].
            2 => {
                let g = n(1)?;
                Some([g, g, g, n(0)?])
            }
            _ => None,
        }
    }

    // --- tokenizing recursive-descent parser over the ASCII tree ---

    struct P<'a> {
        d: &'a [u8],
        p: usize,
    }

    fn parse(bytes: &[u8]) -> Option<E> {
        let mut p = P { d: bytes, p: 0 };
        p.skip_ws();
        p.value()
    }

    impl<'a> P<'a> {
        fn peek(&self) -> Option<u8> {
            self.d.get(self.p).copied()
        }

        fn skip_ws(&mut self) {
            while let Some(b) = self.peek() {
                if b == b' ' || b == b'\t' || b == b'\r' || b == b'\n' {
                    self.p += 1;
                } else {
                    break;
                }
            }
        }

        fn value(&mut self) -> Option<E> {
            self.skip_ws();
            match self.peek()? {
                b'<' => self.dict(),
                b'[' => self.array(),
                b'(' => Some(E::Str(self.string()?)),
                b'/' => Some(E::Name(self.name()?)),
                b't' | b'f' => self.boolean(),
                _ => self.number(),
            }
        }

        fn dict(&mut self) -> Option<E> {
            // consume '<<'
            if self.peek() != Some(b'<') {
                return None;
            }
            self.p += 1;
            if self.peek() != Some(b'<') {
                return None;
            }
            self.p += 1;

            let mut items = Vec::new();
            loop {
                self.skip_ws();
                match self.peek()? {
                    b'>' => {
                        self.p += 1;
                        if self.peek() == Some(b'>') {
                            self.p += 1;
                        }
                        break;
                    }
                    b'/' => {
                        let key = self.name()?;
                        let val = self.value()?;
                        items.push((key, val));
                    }
                    // Tolerate stray tokens between entries.
                    _ => {
                        self.p += 1;
                    }
                }
                if items.len() > 100_000 {
                    return None;
                }
            }
            Some(E::Dict(items))
        }

        fn array(&mut self) -> Option<E> {
            if self.peek() != Some(b'[') {
                return None;
            }
            self.p += 1;
            let mut items = Vec::new();
            loop {
                self.skip_ws();
                match self.peek()? {
                    b']' => {
                        self.p += 1;
                        break;
                    }
                    _ => items.push(self.value()?),
                }
                if items.len() > 1_000_000 {
                    return None;
                }
            }
            Some(E::Arr(items))
        }

        fn name(&mut self) -> Option<String> {
            if self.peek() != Some(b'/') {
                return None;
            }
            self.p += 1;
            let start = self.p;
            while let Some(b) = self.peek() {
                if b == b' '
                    || b == b'\t'
                    || b == b'\r'
                    || b == b'\n'
                    || b == b'/'
                    || b == b'('
                    || b == b'['
                    || b == b'<'
                    || b == b'>'
                    || b == b']'
                {
                    break;
                }
                self.p += 1;
            }
            Some(String::from_utf8_lossy(&self.d[start..self.p]).into_owned())
        }

        fn boolean(&mut self) -> Option<E> {
            if self.d[self.p..].starts_with(b"true") {
                self.p += 4;
                Some(E::Bool(true))
            } else if self.d[self.p..].starts_with(b"false") {
                self.p += 5;
                Some(E::Bool(false))
            } else {
                None
            }
        }

        fn number(&mut self) -> Option<E> {
            let start = self.p;
            while let Some(b) = self.peek() {
                if b.is_ascii_digit()
                    || b == b'.'
                    || b == b'-'
                    || b == b'+'
                    || b == b'e'
                    || b == b'E'
                {
                    self.p += 1;
                } else {
                    break;
                }
            }
            if self.p == start {
                return None;
            }
            std::str::from_utf8(&self.d[start..self.p])
                .ok()?
                .parse::<f64>()
                .ok()
                .map(E::Num)
        }

        /// A `(...)` string: raw bytes with `\` escapes, then decoded. Text-engine
        /// strings are UTF-16BE (often with a BOM); ASCII-only content is accepted
        /// too.
        fn string(&mut self) -> Option<String> {
            if self.peek() != Some(b'(') {
                return None;
            }
            self.p += 1;
            let mut raw = Vec::new();
            let mut depth = 1;
            while let Some(b) = self.peek() {
                self.p += 1;
                match b {
                    b'\\' => {
                        if let Some(esc) = self.peek() {
                            self.p += 1;
                            raw.push(match esc {
                                b'n' => b'\n',
                                b'r' => b'\r',
                                b't' => b'\t',
                                other => other,
                            });
                        }
                    }
                    b'(' => {
                        depth += 1;
                        raw.push(b'(');
                    }
                    b')' => {
                        depth -= 1;
                        if depth == 0 {
                            break;
                        }
                        raw.push(b')');
                    }
                    other => raw.push(other),
                }
            }
            Some(decode_engine_string(&raw))
        }
    }

    /// Decode a text-engine string body. Strips a UTF-16 BOM and decodes UTF-16BE
    /// when the bytes look like it (BOM or even length with NULs); otherwise falls
    /// back to a lossy UTF-8 read.
    fn decode_engine_string(raw: &[u8]) -> String {
        let body = if raw.starts_with(&[0xFE, 0xFF]) {
            &raw[2..]
        } else {
            raw
        };
        let looks_utf16 = raw.starts_with(&[0xFE, 0xFF])
            || (body.len() >= 2 && body.len() % 2 == 0 && body.iter().step_by(2).any(|&b| b == 0));
        if looks_utf16 && body.len() % 2 == 0 {
            let units: Vec<u16> = body
                .chunks_exact(2)
                .map(|c| u16::from_be_bytes([c[0], c[1]]))
                .collect();
            let mut s = String::from_utf16_lossy(&units);
            if s.ends_with('\u{0}') {
                s.pop();
            }
            s
        } else {
            String::from_utf8_lossy(body).into_owned()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- builders for synthetic PSD structures ---

    fn push_u16(v: &mut Vec<u8>, x: u16) {
        v.extend_from_slice(&x.to_be_bytes());
    }
    fn push_u32(v: &mut Vec<u8>, x: u32) {
        v.extend_from_slice(&x.to_be_bytes());
    }
    fn push_f64(v: &mut Vec<u8>, x: f64) {
        v.extend_from_slice(&x.to_be_bytes());
    }
    fn push_key(v: &mut Vec<u8>, key: &str) {
        // Length-prefixed key (non-zero length form).
        push_u32(v, key.len() as u32);
        v.extend_from_slice(key.as_bytes());
    }
    fn push_unicode(v: &mut Vec<u8>, s: &str) {
        let units: Vec<u16> = s.encode_utf16().collect();
        push_u32(v, units.len() as u32 + 1); // + NUL terminator
        for u in units {
            push_u16(v, u);
        }
        push_u16(v, 0);
    }
    fn push_text_value(v: &mut Vec<u8>, key: &str, text: &str) {
        push_key(v, key);
        v.extend_from_slice(b"TEXT");
        push_unicode(v, text);
    }
    fn push_raw_value(v: &mut Vec<u8>, key: &str, raw: &[u8]) {
        push_key(v, key);
        v.extend_from_slice(b"tdta");
        push_u32(v, raw.len() as u32);
        v.extend_from_slice(raw);
    }

    /// Build a `TySh` block with the given transform, text string and (optional)
    /// EngineData blob.
    fn tysh(m: [f64; 6], text: &str, engine: Option<&[u8]>) -> Vec<u8> {
        let mut d = Vec::new();
        push_u16(&mut d, 1); // version
        for x in m {
            push_f64(&mut d, x);
        }
        push_u16(&mut d, 50); // text version
        push_u32(&mut d, 16); // descriptor version
                              // descriptor: name, classID, count, items
        push_unicode(&mut d, ""); // name
        push_key(&mut d, "TxLr"); // classID
        let count = 1 + engine.is_some() as u32;
        push_u32(&mut d, count);
        push_text_value(&mut d, "Txt ", text);
        if let Some(e) = engine {
            push_raw_value(&mut d, "EngineData", e);
        }
        d
    }

    /// A small EngineData blob exercising the fields we lift.
    fn engine_blob() -> Vec<u8> {
        let s = "\n<<\n\t/EngineDict\n\t<<\n\t\t/StyleRun\n\t\t<<\n\t\t\t/RunArray [\n\t\t\t<<\n\t\t\t\t/StyleSheet << /StyleSheetData << /Font 0 /FontSize 200.0 /FauxBold true /Tracking 50 /FillColor << /Type 1 /Values [ 1.0 1.0 0.0 0.0 ] >> >> >>\n\t\t\t>>\n\t\t\t]\n\t\t>>\n\t\t/ParagraphRun << /RunArray [ << /ParagraphSheet << /Properties << /Justification 2 >> >> >> ] >>\n\t>>\n\t/ResourceDict << /FontSet [ << /Name (ArialMT) >> ] >>\n>>\n";
        s.as_bytes().to_vec()
    }

    #[test]
    fn parses_text_and_transform() {
        let block = tysh([1.0, 0.0, 0.0, 1.0, 100.0, 200.0], "Hello", None);
        let imp = parse_type_tool(&block).expect("text import");
        assert_eq!(imp.td.content, "Hello");
        assert!((imp.td.rotation_deg).abs() < 1e-3);
        assert!((imp.td.stretch_x - 1.0).abs() < 1e-3);
    }

    #[test]
    fn carriage_returns_become_newlines() {
        let block = tysh([1.0, 0.0, 0.0, 1.0, 0.0, 0.0], "line1\rline2", None);
        let imp = parse_type_tool(&block).expect("text import");
        assert_eq!(imp.td.content, "line1\nline2");
    }

    #[test]
    fn rotation_is_decomposed_from_transform() {
        // 90° rotation: xx=0, xy=1, yx=-1, yy=0.
        let block = tysh([0.0, 1.0, -1.0, 0.0, 0.0, 0.0], "R", None);
        let imp = parse_type_tool(&block).expect("text import");
        assert!((imp.td.rotation_deg - 90.0).abs() < 1e-3);
    }

    #[test]
    fn engine_data_enriches_font_size_colour_family_align() {
        let block = tysh(
            [1.0, 0.0, 0.0, 1.0, 0.0, 0.0],
            "Styled",
            Some(&engine_blob()),
        );
        let imp = parse_type_tool(&block).expect("text import");
        let td = imp.td;
        assert!(
            (td.font_px - 200.0).abs() < 0.5,
            "font_px was {}",
            td.font_px
        );
        assert_eq!(td.color, [255, 0, 0, 255]); // red fill
        assert_eq!(td.font_family, TextFontFamily::Arial); // ArialMT → Arial
        assert!(td.bold); // FauxBold true
        assert_eq!(td.align, TextAlign::Center); // Justification 2
        assert!(td.tracking_px > 0.0);
    }

    #[test]
    fn font_size_scales_with_transform() {
        // Vertical scale 2× should double the effective font size.
        let block = tysh([1.0, 0.0, 0.0, 2.0, 0.0, 0.0], "Big", Some(&engine_blob()));
        let imp = parse_type_tool(&block).expect("text import");
        assert!((imp.td.font_px - 400.0).abs() < 1.0);
    }

    #[test]
    fn utf16_unicode_string_round_trips() {
        let block = tysh([1.0, 0.0, 0.0, 1.0, 0.0, 0.0], "café — 日本語", None);
        let imp = parse_type_tool(&block).expect("text import");
        assert_eq!(imp.td.content, "café — 日本語");
    }

    #[test]
    fn short_or_wrong_version_yields_none() {
        assert!(parse_type_tool(&[0, 2]).is_none()); // wrong version
        assert!(parse_type_tool(&[0, 1, 0, 0]).is_none()); // truncated transform
        let mut no_txt = Vec::new();
        push_u16(&mut no_txt, 1);
        for _ in 0..6 {
            push_f64(&mut no_txt, 0.0);
        }
        push_u16(&mut no_txt, 50);
        push_u32(&mut no_txt, 16);
        push_unicode(&mut no_txt, "");
        push_key(&mut no_txt, "TxLr");
        push_u32(&mut no_txt, 0); // zero items → no "Txt "
        assert!(parse_type_tool(&no_txt).is_none());
    }

    #[test]
    fn map_font_detects_style_and_family() {
        let (fam, bold, italic) = map_font("Arial-BoldItalicMT");
        assert_eq!(fam, TextFontFamily::Arial);
        assert!(bold && italic);
        let (fam2, _, _) = map_font("TimesNewRomanPSMT");
        assert!(matches!(fam2, TextFontFamily::System(ref s) if s == "TimesNewRoman"));
    }
}
