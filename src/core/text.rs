// Text layer data + glyph rasterization (ab_glyph) → RGBA buffer.
//
// A Text layer stores its `TextData` (the editable string + style). On commit
// the text is rasterized into the layer's tiles; re-editing reads the string
// back out of `TextData`.

use ab_glyph::{Font, FontVec, PxScale, ScaleFont};
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    ops::Range,
    path::{Path, PathBuf},
    sync::{Mutex, OnceLock},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TextAlign {
    Left,
    Center,
    Right,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum TextFontFamily {
    SegoeUi,
    Arial,
    Calibri,
    Tahoma,
    DejaVuSans,
    LiberationSans,
    System(String),
    SystemFace { family: String, style: String },
}

impl TextFontFamily {
    pub fn all() -> &'static [TextFontFamily] {
        available_font_families()
    }

    pub fn name(&self) -> &str {
        match self {
            TextFontFamily::SegoeUi => "Segoe UI",
            TextFontFamily::Arial => "Arial",
            TextFontFamily::Calibri => "Calibri",
            TextFontFamily::Tahoma => "Tahoma",
            TextFontFamily::DejaVuSans => "DejaVu Sans",
            TextFontFamily::LiberationSans => "Liberation Sans",
            TextFontFamily::System(name) => name.as_str(),
            TextFontFamily::SystemFace { family, .. } => family.as_str(),
        }
    }

    pub fn storage_name(&self) -> String {
        match self {
            TextFontFamily::SegoeUi => "SegoeUi".to_string(),
            TextFontFamily::Arial => "Arial".to_string(),
            TextFontFamily::Calibri => "Calibri".to_string(),
            TextFontFamily::Tahoma => "Tahoma".to_string(),
            TextFontFamily::DejaVuSans => "DejaVuSans".to_string(),
            TextFontFamily::LiberationSans => "LiberationSans".to_string(),
            TextFontFamily::System(name) => format!("System:{name}"),
            TextFontFamily::SystemFace { family, style } => {
                format!("SystemFace:{family}\u{1f}{style}")
            }
        }
    }

    pub fn egui_family_name(&self) -> String {
        format!("iai_text_{}", sanitize_font_id(&self.storage_name()))
    }

    fn builtin_candidates(&self) -> &'static [&'static str] {
        match self {
            TextFontFamily::SegoeUi => &[
                "C:/Windows/Fonts/segoeui.ttf",
                "/System/Library/Fonts/Supplemental/Arial.ttf",
                "/Library/Fonts/Arial.ttf",
            ],
            TextFontFamily::Arial => &[
                "C:/Windows/Fonts/arial.ttf",
                "/System/Library/Fonts/Supplemental/Arial.ttf",
                "/Library/Fonts/Arial.ttf",
            ],
            TextFontFamily::Calibri => &["C:/Windows/Fonts/calibri.ttf"],
            TextFontFamily::Tahoma => &["C:/Windows/Fonts/tahoma.ttf"],
            TextFontFamily::DejaVuSans => &["/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf"],
            TextFontFamily::LiberationSans => {
                &["/usr/share/fonts/truetype/liberation/LiberationSans-Regular.ttf"]
            }
            TextFontFamily::System(_) | TextFontFamily::SystemFace { .. } => &[],
        }
    }

    pub fn from_storage_name(s: &str) -> Self {
        if let Some(name) = s.strip_prefix("System:") {
            return known_family_from_name(name)
                .unwrap_or_else(|| TextFontFamily::System(name.to_string()));
        }
        if let Some(face) = s.strip_prefix("SystemFace:") {
            if let Some((family, style)) = face.split_once('\u{1f}') {
                return TextFontFamily::SystemFace {
                    family: family.to_string(),
                    style: style.to_string(),
                };
            }
        }
        match s {
            "Arial" => TextFontFamily::Arial,
            "Calibri" => TextFontFamily::Calibri,
            "Tahoma" => TextFontFamily::Tahoma,
            "DejaVuSans" | "DejaVu Sans" => TextFontFamily::DejaVuSans,
            "LiberationSans" | "Liberation Sans" => TextFontFamily::LiberationSans,
            _ => TextFontFamily::SegoeUi,
        }
    }
}

impl TextFontFamily {
    pub fn style_name(&self) -> &str {
        match self {
            Self::SystemFace { style, .. } => style,
            _ => "Regular",
        }
    }

    pub fn faces(&self) -> Vec<TextFontFamily> {
        let key = normalized_family_key(self.name());
        let mut faces: Vec<_> = system_font_entries()
            .iter()
            .filter(|entry| normalized_family_key(&entry.family) == key)
            .map(|entry| TextFontFamily::SystemFace {
                family: entry.family.clone(),
                style: entry.style.clone(),
            })
            .collect();
        faces.sort_by_key(|face| face.style_name().to_ascii_lowercase());
        faces.dedup_by(|a, b| a.style_name().eq_ignore_ascii_case(b.style_name()));
        if faces.is_empty() {
            faces.push(self.clone());
        }
        faces
    }
}

#[derive(Clone)]
struct SystemFontEntry {
    family: String,
    style: String,
    path: PathBuf,
    score: i32,
}

static SYSTEM_FONTS: OnceLock<Vec<SystemFontEntry>> = OnceLock::new();
static FONT_CACHE: OnceLock<Mutex<HashMap<String, &'static FontVec>>> = OnceLock::new();

pub fn builtin_families() -> [TextFontFamily; 6] {
    [
        TextFontFamily::SegoeUi,
        TextFontFamily::Arial,
        TextFontFamily::Calibri,
        TextFontFamily::Tahoma,
        TextFontFamily::DejaVuSans,
        TextFontFamily::LiberationSans,
    ]
}

fn available_font_families() -> &'static [TextFontFamily] {
    static AVAILABLE: OnceLock<Vec<TextFontFamily>> = OnceLock::new();
    AVAILABLE.get_or_init(|| {
        let mut out = Vec::new();
        let mut seen = HashSet::new();
        for family in builtin_families() {
            if font_path_for_family(&family).is_some() {
                seen.insert(normalized_family_key(family.name()));
                out.push(family);
            }
        }
        for entry in system_font_entries() {
            let key = normalized_family_key(&entry.family);
            if seen.insert(key) {
                out.push(TextFontFamily::System(entry.family.clone()));
            }
        }
        if out.is_empty() {
            out.extend(builtin_families());
        }
        out
    })
}

fn system_font_entries() -> &'static [SystemFontEntry] {
    SYSTEM_FONTS.get_or_init(build_system_font_entries)
}

fn build_system_font_entries() -> Vec<SystemFontEntry> {
    let mut files = Vec::new();
    for dir in system_font_dirs() {
        collect_font_files(&dir, &mut files, 0);
    }

    let mut by_family: BTreeMap<String, SystemFontEntry> = BTreeMap::new();
    for path in files {
        let Ok(bytes) = std::fs::read(&path) else {
            continue;
        };
        // No FontVec parse here: cloning + parsing every installed font makes
        // the scan cost minutes/hundreds of MB. The name-table parse below is
        // validation enough for listing; actual use re-validates per font.
        let Some(family) = font_family_name_from_bytes(&bytes) else {
            continue;
        };
        let style = font_style_name_from_bytes(&bytes).unwrap_or_else(|| "Regular".to_string());
        let key = normalized_family_key(&family);
        if key.is_empty() {
            continue;
        }
        let entry = SystemFontEntry {
            family,
            style: style.clone(),
            score: regular_font_score(&path),
            path,
        };
        let face_key = format!("{key}\u{1f}{}", style.to_ascii_lowercase());
        match by_family.get(&face_key) {
            Some(existing) if existing.score >= entry.score => {}
            _ => {
                by_family.insert(face_key, entry);
            }
        }
    }

    let mut entries: Vec<SystemFontEntry> = by_family.into_values().collect();
    entries.sort_by_key(|entry| entry.family.to_ascii_lowercase());
    entries
}

fn system_font_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Ok(windir) = std::env::var("WINDIR") {
        dirs.push(PathBuf::from(windir).join("Fonts"));
    } else {
        dirs.push(PathBuf::from("C:/Windows/Fonts"));
    }
    dirs.extend([
        PathBuf::from("/System/Library/Fonts"),
        PathBuf::from("/Library/Fonts"),
        PathBuf::from("/usr/share/fonts"),
        PathBuf::from("/usr/local/share/fonts"),
    ]);
    if let Ok(home) = std::env::var("HOME").or_else(|_| std::env::var("USERPROFILE")) {
        let home = PathBuf::from(home);
        dirs.push(home.join(".local/share/fonts"));
        dirs.push(home.join(".fonts"));
    }
    dirs
}

fn collect_font_files(dir: &Path, out: &mut Vec<PathBuf>, depth: usize) {
    if depth > 5 {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_font_files(&path, out, depth + 1);
        } else if is_supported_font_path(&path) {
            out.push(path);
        }
    }
}

fn is_supported_font_path(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| matches!(ext.to_ascii_lowercase().as_str(), "ttf" | "otf"))
        .unwrap_or(false)
}

// Builtin candidates first: they are exact known paths, so resolving the
// default families never has to wait for the full system font scan.
fn font_path_for_family(family: &TextFontFamily) -> Option<PathBuf> {
    family
        .builtin_candidates()
        .iter()
        .map(PathBuf::from)
        .find(|path| path.exists())
        .or_else(|| {
            let key = normalized_family_key(family.name());
            system_font_entries()
                .iter()
                .filter(|entry| normalized_family_key(&entry.family) == key)
                .find(|entry| match family {
                    TextFontFamily::SystemFace { style, .. } => {
                        entry.style.eq_ignore_ascii_case(style)
                    }
                    _ => true,
                })
                .map(|entry| entry.path.clone())
        })
}

pub fn font_bytes_for(family: &TextFontFamily) -> Option<Vec<u8>> {
    std::fs::read(font_path_for_family(family)?).ok()
}

/// Font bytes verified to parse as a font — for registering into egui, which
/// panics on malformed font data.
pub fn validated_font_bytes_for(family: &TextFontFamily) -> Option<Vec<u8>> {
    let bytes = font_bytes_for(family)?;
    FontVec::try_from_vec(bytes.clone()).ok()?;
    Some(bytes)
}

/// Build the system font index (a full font-directory scan) so later callers
/// (the font dropdown) don't block the UI thread. Call from a worker thread.
pub fn warm_system_font_index() {
    let _ = available_font_families();
}

fn known_family_from_name(name: &str) -> Option<TextFontFamily> {
    match normalized_family_key(name).as_str() {
        "segoeui" => Some(TextFontFamily::SegoeUi),
        "arial" => Some(TextFontFamily::Arial),
        "calibri" => Some(TextFontFamily::Calibri),
        "tahoma" => Some(TextFontFamily::Tahoma),
        "dejavusans" => Some(TextFontFamily::DejaVuSans),
        "liberationsans" => Some(TextFontFamily::LiberationSans),
        _ => None,
    }
}

fn normalized_family_key(name: &str) -> String {
    name.chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .map(|c| c.to_ascii_lowercase())
        .collect()
}

fn sanitize_font_id(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '_'
            }
        })
        .collect()
}

fn regular_font_score(path: &Path) -> i32 {
    let name = path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    let mut score = 0;
    for marker in ["regular", "roman", "book", "ui"] {
        if name.contains(marker) {
            score += 20;
        }
    }
    for marker in [
        "bold",
        "italic",
        "oblique",
        "black",
        "heavy",
        "light",
        "semibold",
        "condensed",
        "narrow",
    ] {
        if name.contains(marker) {
            score -= 30;
        }
    }
    score
}

fn read_u16(data: &[u8], offset: usize) -> Option<u16> {
    let bytes = data.get(offset..offset + 2)?;
    Some(u16::from_be_bytes([bytes[0], bytes[1]]))
}

fn read_u32(data: &[u8], offset: usize) -> Option<u32> {
    let bytes = data.get(offset..offset + 4)?;
    Some(u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
}

fn font_family_name_from_bytes(data: &[u8]) -> Option<String> {
    if data.get(0..4) == Some(b"ttcf") {
        return None;
    }
    let table_count = read_u16(data, 4)? as usize;
    let mut name_table = None;
    for i in 0..table_count {
        let rec = 12 + i * 16;
        if data.get(rec..rec + 4) == Some(b"name") {
            let offset = read_u32(data, rec + 8)? as usize;
            let length = read_u32(data, rec + 12)? as usize;
            if offset.checked_add(length)? <= data.len() {
                name_table = Some((offset, length));
            }
            break;
        }
    }

    let (table_offset, table_len) = name_table?;
    let count = read_u16(data, table_offset + 2)? as usize;
    let string_base = table_offset + read_u16(data, table_offset + 4)? as usize;
    let table_end = table_offset + table_len;
    let mut best: Option<(i32, String)> = None;

    for i in 0..count {
        let rec = table_offset + 6 + i * 12;
        if rec + 12 > table_end || rec + 12 > data.len() {
            continue;
        }
        let platform = read_u16(data, rec)?;
        let encoding = read_u16(data, rec + 2)?;
        let language = read_u16(data, rec + 4)?;
        let name_id = read_u16(data, rec + 6)?;
        if name_id != 1 && name_id != 16 {
            continue;
        }
        let length = read_u16(data, rec + 8)? as usize;
        let offset = read_u16(data, rec + 10)? as usize;
        let start = string_base.checked_add(offset)?;
        let end = start.checked_add(length)?;
        if end > data.len() || end > table_end {
            continue;
        }
        let Some(value) = decode_name_string(platform, encoding, &data[start..end]) else {
            continue;
        };
        let value = value.trim_matches(char::from(0)).trim().to_string();
        if value.is_empty() {
            continue;
        }
        let mut score = if name_id == 16 { 100 } else { 60 };
        if platform == 3 {
            score += 20;
        }
        if language == 0x0409 {
            score += 12;
        }
        if encoding == 1 || encoding == 10 {
            score += 4;
        }
        if best
            .as_ref()
            .is_none_or(|(best_score, _)| score > *best_score)
        {
            best = Some((score, value));
        }
    }

    best.map(|(_, value)| value)
}

fn font_style_name_from_bytes(data: &[u8]) -> Option<String> {
    if data.get(0..4) == Some(b"ttcf") {
        return None;
    }
    let table_count = read_u16(data, 4)? as usize;
    let mut name_table = None;
    for i in 0..table_count {
        let rec = 12 + i * 16;
        if data.get(rec..rec + 4) == Some(b"name") {
            let offset = read_u32(data, rec + 8)? as usize;
            let length = read_u32(data, rec + 12)? as usize;
            if offset.checked_add(length)? <= data.len() {
                name_table = Some((offset, length));
            }
            break;
        }
    }
    let (table_offset, table_len) = name_table?;
    let count = read_u16(data, table_offset + 2)? as usize;
    let string_base = table_offset + read_u16(data, table_offset + 4)? as usize;
    let table_end = table_offset + table_len;
    let mut best: Option<(i32, String)> = None;
    for i in 0..count {
        let rec = table_offset + 6 + i * 12;
        if rec + 12 > table_end || rec + 12 > data.len() {
            continue;
        }
        let platform = read_u16(data, rec)?;
        let encoding = read_u16(data, rec + 2)?;
        let language = read_u16(data, rec + 4)?;
        let name_id = read_u16(data, rec + 6)?;
        if name_id != 2 && name_id != 17 {
            continue;
        }
        let length = read_u16(data, rec + 8)? as usize;
        let offset = read_u16(data, rec + 10)? as usize;
        let start = string_base.checked_add(offset)?;
        let end = start.checked_add(length)?;
        if end > data.len() || end > table_end {
            continue;
        }
        let value = decode_name_string(platform, encoding, &data[start..end])?
            .trim_matches(char::from(0))
            .trim()
            .to_string();
        if value.is_empty() {
            continue;
        }
        let score = (if name_id == 17 { 100 } else { 60 })
            + (if platform == 3 { 20 } else { 0 })
            + (if language == 0x0409 { 12 } else { 0 });
        if best.as_ref().is_none_or(|(old, _)| score > *old) {
            best = Some((score, value));
        }
    }
    best.map(|(_, value)| value)
}

fn decode_name_string(platform: u16, encoding: u16, bytes: &[u8]) -> Option<String> {
    if platform == 0 || platform == 3 || encoding == 1 || encoding == 10 {
        if !bytes.len().is_multiple_of(2) {
            return None;
        }
        let units = bytes
            .chunks_exact(2)
            .map(|chunk| u16::from_be_bytes([chunk[0], chunk[1]]));
        return Some(
            std::char::decode_utf16(units)
                .map(|r| r.unwrap_or(char::REPLACEMENT_CHARACTER))
                .collect(),
        );
    }
    Some(String::from_utf8_lossy(bytes).into_owned())
}

/// Per-character overrides of the base text style.
/// Tracking/opacity stay layer-wide.
#[derive(Debug, Clone, PartialEq)]
pub struct GlyphStyle {
    pub color: [u8; 4],
    pub font_px: f32,
    pub font_family: TextFontFamily,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TextData {
    /// The editable text (may contain '\n' for multiple lines).
    pub content: String,
    /// Base font size in canvas pixels (used where no per-glyph override).
    pub font_px: f32,
    /// Base straight RGBA fill colour (used where no per-glyph override).
    pub color: [u8; 4],
    pub font_family: TextFontFamily,
    pub align: TextAlign,
    /// Line height as a multiple of `font_px`.
    pub line_height: f32,
    /// Faux bold while the font resolver only loads regular faces.
    pub bold: bool,
    /// Faux italic slant while the font resolver only loads regular faces.
    pub italic: bool,
    pub underline: bool,
    /// Additional advance between glyphs, in canvas pixels.
    pub tracking_px: f32,
    /// Layer-local text opacity, multiplied with `color[3]`.
    pub opacity: f32,
    /// Persistent horizontal glyph stretch. Free Transform uses this to keep
    /// non-uniformly scaled text editable without losing its visual shape.
    pub stretch_x: f32,
    /// Clockwise rotation around the upright raster origin, in degrees.
    pub rotation_deg: f32,
    /// Mirror the upright raster before rotation.
    pub flip_x: bool,
    pub flip_y: bool,
    /// Per-character colour/size/family overrides. Empty means every glyph uses
    /// the base style; otherwise its length equals the character count of
    /// `content` (each entry aligns 1:1 with a `char`, including '\n').
    pub glyph_styles: Vec<GlyphStyle>,
}

impl TextData {
    /// Effective style of the `i`-th character (falls back to the base style).
    pub fn glyph_style(&self, i: usize) -> GlyphStyle {
        self.glyph_styles.get(i).cloned().unwrap_or(GlyphStyle {
            color: self.color,
            font_px: self.font_px,
            font_family: self.font_family.clone(),
            bold: self.bold,
            italic: self.italic,
            underline: self.underline,
        })
    }

    /// Materialize `glyph_styles` to one explicit entry per character (seeded
    /// from the base style) so a range can be overridden in place. No-op when
    /// already the right length.
    pub fn ensure_glyph_styles(&mut self) {
        let n = self.content.chars().count();
        if self.glyph_styles.len() == n {
            return;
        }
        self.glyph_styles = (0..n).map(|i| self.glyph_style(i)).collect();
    }

    /// Drop `glyph_styles` when every entry equals the base style, keeping the
    /// common (uniform) case cheap to store and compare.
    pub fn compact_glyph_styles(&mut self) {
        let base = GlyphStyle {
            color: self.color,
            font_px: self.font_px,
            font_family: self.font_family.clone(),
            bold: self.bold,
            italic: self.italic,
            underline: self.underline,
        };
        if self.glyph_styles.iter().all(|s| *s == base) {
            self.glyph_styles.clear();
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TextLayoutRect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Default for TextData {
    fn default() -> Self {
        Self {
            content: String::new(),
            font_px: 48.0,
            color: [0, 0, 0, 255],
            font_family: TextFontFamily::SegoeUi,
            align: TextAlign::Left,
            line_height: 1.2,
            bold: false,
            italic: false,
            underline: false,
            tracking_px: 0.0,
            opacity: 1.0,
            stretch_x: 1.0,
            rotation_deg: 0.0,
            flip_x: false,
            flip_y: false,
            glyph_styles: Vec::new(),
        }
    }
}

/// A rasterized block of text in straight-alpha RGBA8.
pub struct Rasterized {
    pub rgba: Vec<u8>,
    pub width: u32,
    pub height: u32,
}

fn family_font(family: &TextFontFamily) -> Option<&'static FontVec> {
    let key = family.storage_name();
    let cache = FONT_CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    if let Ok(cache) = cache.lock() {
        if let Some(font) = cache.get(&key).copied() {
            return Some(font);
        }
    }

    let bytes = font_bytes_for(family)?;
    let font = FontVec::try_from_vec(bytes).ok()?;
    let font = Box::leak(Box::new(font));
    if let Ok(mut cache) = cache.lock() {
        cache.insert(key, font);
    }
    Some(font)
}

/// Load a sans-serif TTF from the system once and cache it. Tries a few common
/// locations per platform; returns `None` if none are found (text rendering is
/// then a no-op and the UI shows a hint).
pub fn font() -> Option<&'static FontVec> {
    font_for(&TextFontFamily::SegoeUi)
}

pub fn font_for(family: &TextFontFamily) -> Option<&'static FontVec> {
    family_font(family)
        .or_else(|| {
            builtin_families()
                .iter()
                .find_map(|fallback| family_font(fallback))
        })
        .or_else(|| TextFontFamily::all().iter().find_map(family_font))
}

/// True when a font is available (used by the UI to warn the user).
pub fn font_available() -> bool {
    font().is_some()
}

// ---- Unified text layout -------------------------------------------------
//
// All four consumers (rasterize / caret_rect / selection_rects /
// char_index_at_pos) run through `layout_text` so their glyph metrics can
// never drift apart. Per-character font size makes lines vary in height and
// baseline, so layout is computed once and reused.

/// One positioned glyph. `x` is the absolute left pen position in raster
/// pixels; `advance` is the distance to the next glyph's left (tracking
/// folded in, except after the last glyph on a line).
struct LaidGlyph {
    ch: char,
    x: f32,
    advance: f32,
    scale: f32,
    color: [u8; 4],
    font_family: TextFontFamily,
    bold: bool,
    italic: bool,
    underline: bool,
}

struct LaidLine {
    top: f32,
    baseline: f32,
    height: f32,
    start_x: f32,
    width: f32,
    start_char: usize,
    len: usize,
    glyph0: usize,
    max_font: f32,
    ascent: f32,
    descent: f32,
}

struct TextLayout {
    glyphs: Vec<LaidGlyph>,
    lines: Vec<LaidLine>,
    width: u32,
    height: u32,
}

impl TextLayout {
    /// Caret x for `col` characters into `line` (col == len → right edge).
    fn caret_x(&self, line: &LaidLine, col: usize) -> f32 {
        if col == 0 || line.len == 0 {
            line.start_x
        } else {
            let g = &self.glyphs[line.glyph0 + col.min(line.len) - 1];
            g.x + g.advance
        }
    }

    /// Map a global character index to (line index, column within line).
    fn locate(&self, char_index: usize) -> (usize, usize) {
        for (li, line) in self.lines.iter().enumerate() {
            let end = line.start_char + line.len;
            if char_index <= end {
                return (li, char_index.saturating_sub(line.start_char).min(line.len));
            }
        }
        let last = self.lines.len().saturating_sub(1);
        (last, self.lines.get(last).map(|l| l.len).unwrap_or(0))
    }
}

fn align_offset(max_w: f32, line_w: f32, align: TextAlign) -> f32 {
    match align {
        TextAlign::Left => 0.0,
        TextAlign::Center => (max_w - line_w) * 0.5,
        TextAlign::Right => max_w - line_w,
    }
}

fn layout_text(td: &TextData) -> Option<TextLayout> {
    let base_font = font_for(&td.font_family)?;
    let base_px = td.font_px.max(1.0);
    let line_height_mult = td.line_height.max(0.5);
    let base_ascent = base_font.as_scaled(PxScale::from(base_px)).ascent();
    let base_descent = base_font.as_scaled(PxScale::from(base_px)).descent();

    let chars: Vec<char> = td.content.chars().collect();
    let styles: Vec<GlyphStyle> = (0..chars.len()).map(|i| td.glyph_style(i)).collect();
    let max_px = styles
        .iter()
        .map(|style| style.font_px.max(1.0))
        .fold(base_px, f32::max);

    let bold_extra = styles
        .iter()
        .filter(|style| style.bold)
        .map(|style| (style.font_px.max(1.0) * 0.045).ceil().max(1.0))
        .fold(0.0_f32, f32::max);
    let italic_extra = styles
        .iter()
        .filter(|style| style.italic)
        .map(|style| (style.font_px.max(1.0) * 0.24).ceil())
        .fold(0.0_f32, f32::max);
    let underline_extra = styles
        .iter()
        .filter(|style| style.underline)
        .map(|style| (style.font_px.max(1.0) * 0.16).ceil())
        .fold(0.0_f32, f32::max);
    let pad = (max_px * 0.2).ceil();
    let tracking_px = td.tracking_px.clamp(-base_px * 0.5, base_px * 4.0);

    // Pass 1: per-line glyphs with line-relative x, plus line metrics.
    let mut glyphs: Vec<LaidGlyph> = Vec::with_capacity(chars.len());
    let mut lines: Vec<LaidLine> = Vec::new();
    let mut char_i = 0usize;
    while char_i <= chars.len() {
        // Collect one line up to '\n' or end.
        let start_char = char_i;
        let glyph0 = glyphs.len();
        let mut pen = 0.0f32;
        let mut prev: Option<(ab_glyph::GlyphId, TextFontFamily)> = None;
        let mut ascent = base_ascent;
        let mut descent = base_descent;
        let mut max_font = base_px;
        let mut len = 0usize;

        while char_i < chars.len() && chars[char_i] != '\n' {
            let ch = chars[char_i];
            let style = styles[char_i].clone();
            let px = style.font_px.max(1.0);
            let glyph_font = font_for(&style.font_family).unwrap_or(base_font);
            let sf = glyph_font.as_scaled(PxScale::from(px));
            let id = sf.scaled_glyph(ch).id;
            if let Some((p, prev_family)) = prev.as_ref() {
                if prev_family == &style.font_family {
                    pen += sf.kern(*p, id);
                }
            }
            let x_rel = pen;
            let adv = sf.h_advance(id);
            ascent = ascent.max(sf.ascent());
            descent = descent.min(sf.descent());
            max_font = max_font.max(px);
            glyphs.push(LaidGlyph {
                ch,
                x: x_rel,
                advance: adv,
                scale: px,
                color: style.color,
                font_family: style.font_family.clone(),
                bold: style.bold,
                italic: style.italic,
                underline: style.underline,
            });
            pen += adv;
            prev = Some((id, style.font_family));
            len += 1;
            char_i += 1;
            // Tracking folds into the just-pushed glyph unless it's the last
            // one on the line.
            if char_i < chars.len() && chars[char_i] != '\n' {
                pen += tracking_px;
                let last = glyphs.len() - 1;
                glyphs[last].advance += tracking_px;
            }
        }

        lines.push(LaidLine {
            top: 0.0,
            baseline: 0.0,
            height: 0.0,
            start_x: 0.0,
            width: pen + bold_extra,
            start_char,
            len,
            glyph0,
            max_font,
            ascent,
            descent,
        });

        if char_i < chars.len() && chars[char_i] == '\n' {
            char_i += 1; // consume newline into the boundary between lines
            if char_i == chars.len() {
                // Trailing newline → one more empty line.
                lines.push(LaidLine {
                    top: 0.0,
                    baseline: 0.0,
                    height: 0.0,
                    start_x: 0.0,
                    width: bold_extra,
                    start_char: char_i,
                    len: 0,
                    glyph0: glyphs.len(),
                    max_font: base_px,
                    ascent: base_ascent,
                    descent: base_descent,
                });
                break;
            }
        } else {
            break;
        }
    }

    // Pass 2: vertical stacking + alignment.
    let max_w = lines.iter().map(|l| l.width).fold(0.0_f32, f32::max);
    let mut baseline = 0.0f32;
    let mut last_descent = base_descent;
    for (idx, line) in lines.iter_mut().enumerate() {
        if idx == 0 {
            baseline = line.ascent;
        } else {
            baseline += line_height_mult * line.max_font;
        }
        line.baseline = baseline;
        line.top = baseline - line.ascent;
        line.height = (line.ascent - line.descent).max(line.max_font * 0.75);
        line.start_x = pad + align_offset(max_w, line.width, td.align);
        last_descent = line.descent;
        for g in line.glyph0..line.glyph0 + line.len {
            glyphs[g].x += line.start_x;
        }
    }

    let width_f = (max_w + pad * 2.0 + italic_extra + bold_extra)
        .ceil()
        .max(1.0);
    let height_f = (baseline - last_descent + pad + underline_extra)
        .ceil()
        .max(1.0);
    if !width_f.is_finite()
        || !height_f.is_finite()
        || width_f > u32::MAX as f32
        || height_f > u32::MAX as f32
    {
        return None;
    }

    Some(TextLayout {
        glyphs,
        lines,
        width: width_f as u32,
        height: height_f as u32,
    })
}

/// Map a point in raster-local pixels (same space `caret_rect` reports) to the
/// nearest character-boundary index. This is the pointer→caret mapping for the
/// editing overlay: egui's own hit-test runs on its galley, whose metrics
/// drift from the drawn raster, so clicks must be resolved with the exact
/// layout math used to draw.
pub fn char_index_at_pos(td: &TextData, x: f32, y: f32) -> Option<usize> {
    let layout = layout_text(td)?;
    if layout.lines.is_empty() {
        return Some(0);
    }
    // Nearest line to y (distance to the line's vertical band), so clicks in
    // the leading gap resolve to the closer line.
    let li = layout
        .lines
        .iter()
        .enumerate()
        .min_by(|(_, a), (_, b)| {
            let da = band_distance(y, a.top, a.top + a.height);
            let db = band_distance(y, b.top, b.top + b.height);
            da.total_cmp(&db)
        })
        .map(|(i, _)| i)?;
    let line = &layout.lines[li];

    // Snap to whichever glyph boundary is closer.
    let mut col = 0usize;
    for k in 0..line.len {
        let g = &layout.glyphs[line.glyph0 + k];
        if x < g.x + g.advance * 0.5 {
            break;
        }
        col += 1;
    }
    Some(line.start_char + col)
}

fn band_distance(v: f32, lo: f32, hi: f32) -> f32 {
    if v < lo {
        lo - v
    } else if v > hi {
        v - hi
    } else {
        0.0
    }
}

/// Character span of the same-class run (word / whitespace / punctuation)
/// around `char_idx`, for double-click word selection. Runs never cross '\n'.
pub fn word_char_span(content: &str, char_idx: usize) -> (usize, usize) {
    #[derive(PartialEq)]
    enum Kind {
        Word,
        Space,
        Other,
    }
    fn kind(c: char) -> Kind {
        if c.is_alphanumeric() || c == '_' {
            Kind::Word
        } else if c.is_whitespace() {
            Kind::Space
        } else {
            Kind::Other
        }
    }
    let chars: Vec<char> = content.chars().collect();
    if chars.is_empty() {
        return (0, 0);
    }
    let i = char_idx.min(chars.len() - 1);
    if chars[i] == '\n' {
        return (i, i);
    }
    let k = kind(chars[i]);
    let mut start = i;
    while start > 0 && chars[start - 1] != '\n' && kind(chars[start - 1]) == k {
        start -= 1;
    }
    let mut end = i + 1;
    while end < chars.len() && chars[end] != '\n' && kind(chars[end]) == k {
        end += 1;
    }
    (start, end)
}

/// Character span of the line containing `char_idx` (excluding the trailing
/// '\n'), for triple-click line selection.
pub fn line_char_span(content: &str, char_idx: usize) -> (usize, usize) {
    let chars: Vec<char> = content.chars().collect();
    let i = char_idx.min(chars.len());
    let mut start = i;
    while start > 0 && chars[start - 1] != '\n' {
        start -= 1;
    }
    let mut end = i;
    while end < chars.len() && chars[end] != '\n' {
        end += 1;
    }
    (start, end)
}

pub fn caret_rect(td: &TextData, char_index: usize) -> Option<TextLayoutRect> {
    let layout = layout_text(td)?;
    let (li, col) = layout.locate(char_index);
    let line = layout.lines.get(li)?;
    Some(TextLayoutRect {
        x: layout.caret_x(line, col),
        y: line.top,
        w: 0.0,
        h: line.height,
    })
}

pub fn selection_rects(td: &TextData, char_range: Range<usize>) -> Option<Vec<TextLayoutRect>> {
    if char_range.start >= char_range.end {
        return Some(Vec::new());
    }
    let layout = layout_text(td)?;
    let total_chars = td.content.chars().count();
    let start_sel = char_range.start.min(total_chars);
    let end_sel = char_range.end.min(total_chars);
    let mut rects = Vec::new();

    for (line_idx, line) in layout.lines.iter().enumerate() {
        let line_start = line.start_char;
        let line_end = line_start + line.len;
        let includes_newline = line_idx + 1 < layout.lines.len() && end_sel > line_end;
        let sel_start = start_sel.max(line_start).min(line_end);
        let sel_end = end_sel.min(line_end);
        if sel_start < sel_end || (includes_newline && start_sel <= line_end) {
            let col_start = sel_start.saturating_sub(line_start).min(line.len);
            let col_end = sel_end.saturating_sub(line_start).min(line.len);
            let x0 = layout.caret_x(line, col_start);
            let mut x1 = layout.caret_x(line, col_end);
            if includes_newline {
                x1 = x1.max(line.start_x + line.width + line.height * 0.5);
            }
            if x1 > x0 {
                rects.push(TextLayoutRect {
                    x: x0,
                    y: line.top,
                    w: x1 - x0,
                    h: line.height,
                });
            }
        }
    }

    Some(rects)
}

fn paint_pixel_max(
    buf: &mut [u8],
    width: u32,
    height: u32,
    px: i32,
    py: i32,
    color: [u8; 3],
    alpha: u8,
) {
    if alpha == 0 || px < 0 || py < 0 || px >= width as i32 || py >= height as i32 {
        return;
    }
    let idx = ((py as u32 * width + px as u32) * 4) as usize;
    if alpha >= buf[idx + 3] {
        buf[idx] = color[0];
        buf[idx + 1] = color[1];
        buf[idx + 2] = color[2];
        buf[idx + 3] = alpha;
    }
}

/// Rasterize `td.content` into a straight-alpha RGBA buffer. Returns `None`
/// when there is no font, the text is empty, or the result would be empty.
pub fn rasterize(td: &TextData) -> Option<Rasterized> {
    if td.content.trim().is_empty() {
        return None;
    }
    let base_font = font_for(&td.font_family)?;
    let layout = layout_text(td)?;
    let width = layout.width;
    let height = layout.height;
    let len = (width as u64)
        .checked_mul(height as u64)
        .and_then(|n| n.checked_mul(4))
        .and_then(|n| usize::try_from(n).ok())?;

    let mut buf = vec![0u8; len];
    let opacity = td.opacity.clamp(0.0, 1.0);

    for line in &layout.lines {
        let baseline_y = line.baseline;
        for k in 0..line.len {
            let g = &layout.glyphs[line.glyph0 + k];
            let font = font_for(&g.font_family).unwrap_or(base_font);
            let sf = font.as_scaled(PxScale::from(g.scale.max(1.0)));
            let glyph_base = sf.scaled_glyph(g.ch);
            let color3 = [g.color[0], g.color[1], g.color[2]];
            let ca = (g.color[3] as f32 * opacity).round().clamp(0.0, 255.0) as u8;
            let bold_offsets: &[f32] = if g.bold { &[0.0, 1.0] } else { &[0.0] };

            for offset in bold_offsets {
                let mut glyph = glyph_base.clone();
                glyph.position = ab_glyph::point(g.x + *offset, baseline_y);
                if let Some(outlined) = font.outline_glyph(glyph) {
                    let bounds = outlined.px_bounds();
                    outlined.draw(|gx, gy, coverage| {
                        let mut px = bounds.min.x as i32 + gx as i32;
                        let py = bounds.min.y as i32 + gy as i32;
                        if g.italic {
                            px += ((baseline_y - py as f32) * 0.22).round() as i32;
                        }
                        let a = (coverage * ca as f32).round().clamp(0.0, 255.0) as u8;
                        paint_pixel_max(&mut buf, width, height, px, py, color3, a);
                    });
                }
            }

            if g.underline && g.advance > 0.0 {
                let ul_alpha = (g.color[3] as f32 * opacity).round().clamp(0.0, 255.0) as u8;
                let y0 = (baseline_y + g.scale * 0.08).round() as i32;
                let thickness = (g.scale / 16.0).round().max(1.0) as i32;
                let x0 = g.x.floor() as i32;
                let x1 = (g.x + g.advance).ceil() as i32;
                for y in y0..y0 + thickness {
                    for x in x0..x1 {
                        paint_pixel_max(&mut buf, width, height, x, y, color3, ul_alpha);
                    }
                }
            }
        }
    }

    Some(Rasterized {
        rgba: buf,
        width,
        height,
    })
}

fn placed_raster_bounds(
    width: u32,
    height: u32,
    stretch_x: f32,
    angle_deg: f32,
    flip_x: bool,
    flip_y: bool,
) -> Option<(i32, i32, u32, u32)> {
    let rad = angle_deg.to_radians();
    let c = rad.cos();
    let s = rad.sin();
    let fx = if flip_x { -1.0 } else { 1.0 };
    let fy = if flip_y { -1.0 } else { 1.0 };
    let corners = [
        (0.0f32, 0.0f32),
        (width as f32, 0.0f32),
        (0.0f32, height as f32),
        (width as f32, height as f32),
    ];
    let mut min_x = f32::INFINITY;
    let mut min_y = f32::INFINITY;
    let mut max_x = f32::NEG_INFINITY;
    let mut max_y = f32::NEG_INFINITY;
    for (x, y) in corners {
        let tx = fx * x * stretch_x;
        let ty = fy * y;
        let rx = c * tx - s * ty;
        let ry = s * tx + c * ty;
        min_x = min_x.min(rx);
        min_y = min_y.min(ry);
        max_x = max_x.max(rx);
        max_y = max_y.max(ry);
    }
    if !min_x.is_finite() || !min_y.is_finite() || !max_x.is_finite() || !max_y.is_finite() {
        return None;
    }

    let ox = min_x.floor();
    let oy = min_y.floor();
    let out_w = (max_x.ceil() - ox).max(1.0);
    let out_h = (max_y.ceil() - oy).max(1.0);
    if ox < i32::MIN as f32
        || ox > i32::MAX as f32
        || oy < i32::MIN as f32
        || oy > i32::MAX as f32
        || out_w > u32::MAX as f32
        || out_h > u32::MAX as f32
    {
        return None;
    }

    Some((ox as i32, oy as i32, out_w as u32, out_h as u32))
}

fn sample_rgba_bilinear(src: &[u8], width: u32, height: u32, x: f32, y: f32) -> [u8; 4] {
    let x0 = x.floor() as i32;
    let y0 = y.floor() as i32;
    let fx = x - x0 as f32;
    let fy = y - y0 as f32;

    let samp = |px: i32, py: i32| -> [f32; 4] {
        if px < 0 || py < 0 || px >= width as i32 || py >= height as i32 {
            return [0.0; 4];
        }
        let i = ((py as u32 * width + px as u32) * 4) as usize;
        let a = src[i + 3] as f32 / 255.0;
        [
            src[i] as f32 * a / 255.0,
            src[i + 1] as f32 * a / 255.0,
            src[i + 2] as f32 * a / 255.0,
            a,
        ]
    };

    let c00 = samp(x0, y0);
    let c10 = samp(x0 + 1, y0);
    let c01 = samp(x0, y0 + 1);
    let c11 = samp(x0 + 1, y0 + 1);
    let lerp = |a: f32, b: f32, t: f32| a * (1.0 - t) + b * t;
    let a = lerp(lerp(c00[3], c10[3], fx), lerp(c01[3], c11[3], fx), fy);
    if a < f32::EPSILON {
        return [0, 0, 0, 0];
    }
    let r = lerp(lerp(c00[0], c10[0], fx), lerp(c01[0], c11[0], fx), fy) / a;
    let g = lerp(lerp(c00[1], c10[1], fx), lerp(c01[1], c11[1], fx), fy) / a;
    let b = lerp(lerp(c00[2], c10[2], fx), lerp(c01[2], c11[2], fx), fy) / a;
    let to_u8 = |v: f32| (v * 255.0).round().clamp(0.0, 255.0) as u8;
    [to_u8(r), to_u8(g), to_u8(b), to_u8(a)]
}

/// Rasterize text and apply its placement rotation. The returned delta is the
/// rotated bounding-box top-left relative to the upright raster origin.
pub fn rasterize_stretched(td: &TextData) -> Option<Rasterized> {
    let upright = rasterize(td)?;
    let stretch_x = if td.stretch_x.is_finite() && td.stretch_x > 0.001 {
        td.stretch_x
    } else {
        1.0
    };
    if (stretch_x - 1.0).abs() <= 0.001 {
        return Some(upright);
    }
    let width = ((upright.width as f32 * stretch_x).round() as u32).max(1);
    let len = (width as u64)
        .checked_mul(upright.height as u64)
        .and_then(|n| n.checked_mul(4))
        .and_then(|n| usize::try_from(n).ok())?;
    let mut rgba = vec![0; len];
    for y in 0..upright.height {
        for x in 0..width {
            let sample_x = (x as f32 + 0.5) / stretch_x - 0.5;
            let px = sample_rgba_bilinear(
                &upright.rgba,
                upright.width,
                upright.height,
                sample_x,
                y as f32,
            );
            let i = ((y * width + x) * 4) as usize;
            rgba[i..i + 4].copy_from_slice(&px);
        }
    }
    Some(Rasterized {
        rgba,
        width,
        height: upright.height,
    })
}

pub fn rasterize_placed(td: &TextData) -> Option<(Rasterized, (i32, i32))> {
    let upright = rasterize_stretched(td)?;
    let angle = td.rotation_deg.rem_euclid(360.0);
    if (!angle.is_finite() || angle <= 0.001 || (360.0 - angle) <= 0.001)
        && !td.flip_x
        && !td.flip_y
    {
        return Some((upright, (0, 0)));
    }

    let (dx, dy, out_w, out_h) = placed_raster_bounds(
        upright.width,
        upright.height,
        1.0,
        angle,
        td.flip_x,
        td.flip_y,
    )?;
    let len = (out_w as u64)
        .checked_mul(out_h as u64)
        .and_then(|n| n.checked_mul(4))
        .and_then(|n| usize::try_from(n).ok())?;
    let mut out = vec![0u8; len];
    let rad = angle.to_radians();
    let c = rad.cos();
    let s = rad.sin();
    let fx = if td.flip_x { -1.0 } else { 1.0 };
    let fy = if td.flip_y { -1.0 } else { 1.0 };

    for py in 0..out_h {
        let y = py as f32 + dy as f32;
        for px in 0..out_w {
            let x = px as f32 + dx as f32;
            let tx = c * x + s * y;
            let ty = -s * x + c * y;
            let sx = fx * tx;
            let sy = fy * ty;
            if sx < 0.0 || sy < 0.0 || sx >= upright.width as f32 || sy >= upright.height as f32 {
                continue;
            }
            let rgba = sample_rgba_bilinear(&upright.rgba, upright.width, upright.height, sx, sy);
            if rgba[3] == 0 {
                continue;
            }
            let i = ((py * out_w + px) * 4) as usize;
            out[i..i + 4].copy_from_slice(&rgba);
        }
    }

    Some((
        Rasterized {
            rgba: out,
            width: out_w,
            height: out_h,
        },
        (dx, dy),
    ))
}

/// One fill colour's worth of curves extracted from a text layer.
pub struct TextCurveGroup {
    /// The straight RGBA colour every glyph in this group was drawn with.
    pub color: [u8; 4],
    /// Those glyphs' outlines as one multi-contour path in CANVAS space. NonZero
    /// fill preserves each glyph's native winding, so counters render as holes and
    /// touching same-colour glyphs union cleanly.
    pub path: crate::core::vector::path::PathData,
}

/// Accumulate contours under their colour, preserving first-seen order so the
/// resulting layer order is deterministic.
fn add_curve_group(
    groups: &mut Vec<([u8; 4], Vec<crate::core::vector::path::Contour>)>,
    color: [u8; 4],
    mut contours: Vec<crate::core::vector::path::Contour>,
) {
    if contours.is_empty() {
        return;
    }
    match groups.iter_mut().find(|(c, _)| *c == color) {
        Some(slot) => slot.1.append(&mut contours),
        None => groups.push((color, contours)),
    }
}

/// Convert a text layer's `TextData` into editable vector curves ("Convert Text
/// to Curves"). Returns one [`TextCurveGroup`] per distinct fill colour (a single
/// group in the common single-colour case), every contour already mapped into
/// CANVAS space so it lands exactly where the rasterized text sits.
///
/// `layer_offset` is the text layer's current `Layer::offset`; the upright text
/// anchor is recovered as `layer_offset − placement_delta` — the exact inverse of
/// how [`rasterize_placed`]/`rasterize_into_layer` place the raster — so a text
/// layer moved with the Move tool converts at its moved position, and the floored
/// placement delta cancels out. The output reproduces the raster's appearance:
/// per-glyph font/size (each glyph outlined at its own scale), per-glyph colour,
/// faux-bold double strike, faux-italic shear, underline bars, and the layer's
/// stretch / rotation / flip placement. `TextData::opacity` is deliberately NOT
/// baked into the colour: the caller puts it on each object's
/// `VectorStyle::opacity`, matching how the raster pipeline multiplies it in.
///
/// KNOWN LIMITATION (multi-colour text only): each colour becomes its own Path
/// layer that alpha-composites, whereas [`rasterize`] unions all glyphs with
/// coverage-max (winner-takes-all). For single-colour text these are identical.
/// For multi-colour text they differ ONLY along the ~1px antialiased fringe where
/// glyphs of DIFFERENT colours physically overlap (heavy negative tracking, or an
/// italic-sheared glyph reaching into a differently-coloured neighbour) — solid
/// interiors still match because reading order == raster draw order. Exact
/// reproduction is impossible with independent alpha-composited layers, and the
/// only alternative (booleaning the overlaps away) would flatten the smooth glyph
/// curves to polygons — strictly worse — so this fringe is accepted.
pub fn text_to_curves(td: &TextData, layer_offset: (i32, i32)) -> Vec<TextCurveGroup> {
    use crate::core::geometry::Point;
    use crate::core::vector::from_text::{glyph_contours, GlyphSegment};
    use crate::core::vector::path::{Contour, FillRule, Node, PathData};
    use ab_glyph::OutlineCurve;

    let Some(base_font) = font_for(&td.font_family) else {
        return Vec::new();
    };
    let Some(layout) = layout_text(td) else {
        return Vec::new();
    };

    // Recover the upright raster origin, then map upright-layout points through
    // stretch → flip → rotation exactly as `rasterize_stretched`/`rasterize_placed`
    // transform the pixels. `delta` is (0,0) unless the layer is rotated/flipped.
    let delta = rasterize_placed(td).map(|(_, d)| d).unwrap_or((0, 0));
    let origin = (
        layer_offset.0 as f32 - delta.0 as f32,
        layer_offset.1 as f32 - delta.1 as f32,
    );
    let stretch = if td.stretch_x.is_finite() && td.stretch_x > 0.001 {
        td.stretch_x
    } else {
        1.0
    };
    let angle = td.rotation_deg.rem_euclid(360.0).to_radians();
    let (cos_a, sin_a) = (angle.cos(), angle.sin());
    let fx = if td.flip_x { -1.0 } else { 1.0 };
    let fy = if td.flip_y { -1.0 } else { 1.0 };
    // upright-layout (lx, ly) → canvas.
    let place = |lx: f32, ly: f32| -> Point {
        let (sx, sy) = (lx * stretch, ly);
        let (fxv, fyv) = (fx * sx, fy * sy);
        Point::new(
            origin.0 + cos_a * fxv - sin_a * fyv,
            origin.1 + sin_a * fxv + cos_a * fyv,
        )
    };

    let mut groups: Vec<([u8; 4], Vec<Contour>)> = Vec::new();

    for line in &layout.lines {
        let baseline_y = line.baseline;
        for k in 0..line.len {
            let g = &layout.glyphs[line.glyph0 + k];
            let font = font_for(&g.font_family).unwrap_or(base_font);
            let sf = font.as_scaled(PxScale::from(g.scale.max(1.0)));
            let sh = sf.h_scale_factor();
            let sv = sf.v_scale_factor();

            // A font-unit outline point (y-up) at pen `pen_x` → canvas: to
            // upright-layout pixels first (matching `rasterize`), apply the
            // faux-italic shear there, then place.
            let to_canvas = |gp: ab_glyph::Point, pen_x: f32| -> Point {
                let mut lx = pen_x + gp.x * sh;
                let ly = baseline_y - gp.y * sv;
                if g.italic {
                    lx += (baseline_y - ly) * 0.22;
                }
                place(lx, ly)
            };

            if let Some(outline) = font.outline(font.glyph_id(g.ch)) {
                // Faux bold strikes the glyph again 1px to the right (matching the
                // raster's two-pass max coverage); NonZero unions the copies.
                let pens: &[f32] = if g.bold { &[0.0, 1.0] } else { &[0.0] };
                let mut segs: Vec<GlyphSegment> = Vec::with_capacity(outline.curves.len());
                for &pen_off in pens {
                    let pen_x = g.x + pen_off;
                    segs.clear();
                    for curve in &outline.curves {
                        segs.push(match *curve {
                            OutlineCurve::Line(a, b) => {
                                GlyphSegment::Line(to_canvas(a, pen_x), to_canvas(b, pen_x))
                            }
                            OutlineCurve::Quad(a, c, b) => GlyphSegment::Quad(
                                to_canvas(a, pen_x),
                                to_canvas(c, pen_x),
                                to_canvas(b, pen_x),
                            ),
                            OutlineCurve::Cubic(a, c1, c2, b) => GlyphSegment::Cubic(
                                to_canvas(a, pen_x),
                                to_canvas(c1, pen_x),
                                to_canvas(c2, pen_x),
                                to_canvas(b, pen_x),
                            ),
                        });
                    }
                    add_curve_group(&mut groups, g.color, glyph_contours(&segs));
                }
            }

            // Underline: an axis-aligned bar (no italic shear), placed like the
            // glyphs. Mirrors `rasterize`'s underline metrics.
            if g.underline && g.advance > 0.0 {
                let y0 = (baseline_y + g.scale * 0.08).round();
                let thickness = (g.scale / 16.0).round().max(1.0);
                let x0 = g.x.floor();
                let x1 = (g.x + g.advance).ceil();
                let bar = Contour::new(
                    vec![
                        Node::sharp(place(x0, y0)),
                        Node::sharp(place(x1, y0)),
                        Node::sharp(place(x1, y0 + thickness)),
                        Node::sharp(place(x0, y0 + thickness)),
                    ],
                    true,
                );
                add_curve_group(&mut groups, g.color, vec![bar]);
            }
        }
    }

    groups
        .into_iter()
        .map(|(color, contours)| TextCurveGroup {
            color,
            path: PathData::new(contours, FillRule::NonZero),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{rasterize, rasterize_placed, TextData, TextFontFamily};

    fn text(content: &str) -> TextData {
        TextData {
            content: content.to_string(),
            font_family: TextFontFamily::DejaVuSans,
            font_px: 32.0,
            ..TextData::default()
        }
    }

    fn max_alpha(rgba: &[u8]) -> u8 {
        rgba.chunks_exact(4).map(|p| p[3]).max().unwrap_or(0)
    }

    fn alpha_sum(rgba: &[u8]) -> u64 {
        rgba.chunks_exact(4).map(|p| p[3] as u64).sum()
    }

    #[test]
    fn tracking_expands_raster_width() {
        let normal = match rasterize(&text("AB")) {
            Some(r) => r,
            None => return,
        };
        let tracked = rasterize(&TextData {
            tracking_px: 12.0,
            ..text("AB")
        })
        .expect("font was available for normal text");

        assert!(tracked.width > normal.width);
    }

    #[test]
    fn line_height_increases_multiline_height() {
        let compact = match rasterize(&TextData {
            line_height: 0.8,
            ..text("A\nA")
        }) {
            Some(r) => r,
            None => return,
        };
        let loose = rasterize(&TextData {
            line_height: 1.8,
            ..text("A\nA")
        })
        .expect("font was available for compact text");

        assert!(loose.height > compact.height);
    }

    #[test]
    fn opacity_scales_alpha() {
        let full = match rasterize(&text("A")) {
            Some(r) => r,
            None => return,
        };
        let faint = rasterize(&TextData {
            opacity: 0.35,
            ..text("A")
        })
        .expect("font was available for full-opacity text");

        assert!(max_alpha(&faint.rgba) < max_alpha(&full.rgba));
    }

    #[test]
    fn underline_adds_visible_ink() {
        let plain = match rasterize(&text("Text")) {
            Some(r) => r,
            None => return,
        };
        let underlined = rasterize(&TextData {
            underline: true,
            ..text("Text")
        })
        .expect("font was available for plain text");

        assert!(alpha_sum(&underlined.rgba) > alpha_sum(&plain.rgba));
    }

    #[test]
    fn per_char_bold_only_changes_styled_glyphs() {
        let plain = match rasterize(&text("AA")) {
            Some(r) => r,
            None => return,
        };
        let mut partial = text("AA");
        partial.glyph_styles = (0..2)
            .map(|i| super::GlyphStyle {
                color: partial.color,
                font_px: partial.font_px,
                font_family: partial.font_family.clone(),
                bold: i == 0,
                italic: partial.italic,
                underline: partial.underline,
            })
            .collect();
        let partial = rasterize(&partial).expect("font available");
        let all_bold = rasterize(&TextData {
            bold: true,
            ..text("AA")
        })
        .expect("font available");

        assert!(alpha_sum(&partial.rgba) > alpha_sum(&plain.rgba));
        assert!(alpha_sum(&partial.rgba) < alpha_sum(&all_bold.rgba));
    }

    #[test]
    fn per_char_underline_does_not_span_whole_line() {
        let plain = match rasterize(&text("AA")) {
            Some(r) => r,
            None => return,
        };
        let mut partial = text("AA");
        partial.glyph_styles = (0..2)
            .map(|i| super::GlyphStyle {
                color: partial.color,
                font_px: partial.font_px,
                font_family: partial.font_family.clone(),
                bold: partial.bold,
                italic: partial.italic,
                underline: i == 0,
            })
            .collect();
        let partial = rasterize(&partial).expect("font available");
        let all_underlined = rasterize(&TextData {
            underline: true,
            ..text("AA")
        })
        .expect("font available");

        assert!(alpha_sum(&partial.rgba) > alpha_sum(&plain.rgba));
        assert!(alpha_sum(&partial.rgba) < alpha_sum(&all_underlined.rgba));
    }

    #[test]
    fn per_char_italic_expands_raster_without_global_italic() {
        let plain = match rasterize(&text("AA")) {
            Some(r) => r,
            None => return,
        };
        let mut partial = text("AA");
        partial.glyph_styles = (0..2)
            .map(|i| super::GlyphStyle {
                color: partial.color,
                font_px: partial.font_px,
                font_family: partial.font_family.clone(),
                bold: partial.bold,
                italic: i == 0,
                underline: partial.underline,
            })
            .collect();
        let partial = rasterize(&partial).expect("font available");

        assert!(partial.width > plain.width);
    }

    #[test]
    fn rasterize_placed_rotates_90_degree_bounds() {
        let upright = match rasterize(&text("Wide text")) {
            Some(r) => r,
            None => return,
        };
        let (rotated, _) = rasterize_placed(&TextData {
            rotation_deg: 90.0,
            ..text("Wide text")
        })
        .expect("font was available for upright text");

        assert!((rotated.width as i32 - upright.height as i32).abs() <= 1);
        assert!((rotated.height as i32 - upright.width as i32).abs() <= 1);
    }

    #[test]
    fn rasterize_placed_flip_keeps_upright_bounds() {
        let upright = match rasterize(&text("Mirror")) {
            Some(r) => r,
            None => return,
        };
        let (flipped, delta) = rasterize_placed(&TextData {
            flip_x: true,
            ..text("Mirror")
        })
        .expect("font was available for upright text");

        assert_eq!(
            (flipped.width, flipped.height),
            (upright.width, upright.height)
        );
        assert_eq!(delta.0, -(upright.width as i32));
    }

    #[test]
    fn char_index_at_pos_roundtrips_caret_rect() {
        let td = TextData {
            tracking_px: 3.0,
            ..text("Hello v\nxin chào")
        };
        if super::font_for(&td.font_family).is_none() {
            return;
        }
        let total = td.content.chars().count();
        for k in 0..=total {
            let rect = super::caret_rect(&td, k).expect("caret_rect");
            // Sample just right of the boundary, vertically mid-row: the
            // nearest-boundary hit-test must map back to the same index
            // (k at '\n' boundaries maps to the line-end index, which
            // caret_rect places at the same x — accept either side).
            let idx = super::char_index_at_pos(&td, rect.x + 0.01, rect.y + rect.h * 0.5)
                .expect("char_index_at_pos");
            let back = super::caret_rect(&td, idx).expect("caret_rect back");
            assert!(
                (back.x - rect.x).abs() < 0.5 && (back.y - rect.y).abs() < 0.5,
                "boundary {k}: caret at ({}, {}) hit-tested to {idx} at ({}, {})",
                rect.x,
                rect.y,
                back.x,
                back.y
            );
        }
    }

    #[test]
    fn per_char_size_widens_raster() {
        let base = match rasterize(&text("AAAA")) {
            Some(r) => r,
            None => return,
        };
        let mut td = text("AAAA");
        td.glyph_styles = (0..4)
            .map(|_| super::GlyphStyle {
                color: td.color,
                font_px: 64.0,
                font_family: td.font_family.clone(),
                bold: td.bold,
                italic: td.italic,
                underline: td.underline,
            })
            .collect();
        let big = rasterize(&td).expect("font available");
        // Doubling every glyph's size must grow the raster in both axes.
        assert!(big.width > base.width && big.height > base.height);
    }

    #[test]
    fn per_char_color_paints_requested_rgb() {
        let mut td = text("A");
        td.color = [0, 0, 0, 255];
        td.glyph_styles = vec![super::GlyphStyle {
            color: [220, 20, 20, 255],
            font_px: td.font_px,
            font_family: td.font_family.clone(),
            bold: td.bold,
            italic: td.italic,
            underline: td.underline,
        }];
        let Some(r) = rasterize(&td) else {
            return;
        };
        // Some pixel carries the override's red channel, not the black base.
        let has_red = r
            .rgba
            .chunks_exact(4)
            .any(|p| p[3] > 40 && p[0] > 150 && p[1] < 90 && p[2] < 90);
        assert!(has_red, "override colour should appear in the raster");
    }

    #[test]
    fn caret_roundtrips_with_mixed_sizes() {
        let mut td = text("Aa Bb");
        if super::font_for(&td.font_family).is_none() {
            return;
        }
        td.glyph_styles = "Aa Bb"
            .chars()
            .enumerate()
            .map(|(i, _)| super::GlyphStyle {
                color: td.color,
                font_px: if i % 2 == 0 { 20.0 } else { 60.0 },
                font_family: td.font_family.clone(),
                bold: td.bold,
                italic: td.italic,
                underline: td.underline,
            })
            .collect();
        let total = td.content.chars().count();
        for k in 0..=total {
            let rect = super::caret_rect(&td, k).expect("caret_rect");
            let idx = super::char_index_at_pos(&td, rect.x + 0.01, rect.y + rect.h * 0.5)
                .expect("hit-test");
            let back = super::caret_rect(&td, idx).expect("caret back");
            assert!(
                (back.x - rect.x).abs() < 0.6,
                "mixed-size boundary {k}: {} vs {}",
                rect.x,
                back.x
            );
        }
    }

    #[test]
    fn word_and_line_spans() {
        let content = "xin chào\nthế giới";
        // Word under the 'i' of "xin".
        assert_eq!(super::word_char_span(content, 1), (0, 3));
        // Whitespace run between words.
        assert_eq!(super::word_char_span(content, 3), (3, 4));
        // Word on the second line ("thế").
        assert_eq!(super::word_char_span(content, 10), (9, 12));
        // Lines never leak across '\n'.
        assert_eq!(super::line_char_span(content, 1), (0, 8));
        assert_eq!(super::line_char_span(content, 10), (9, 17));
    }

    #[test]
    fn text_to_curves_positions_glyphs_over_the_raster() {
        let td = TextData {
            content: "Ag".to_string(),
            font_family: TextFontFamily::DejaVuSans,
            font_px: 64.0,
            ..TextData::default()
        };
        let Some((placed, _)) = rasterize_placed(&td) else {
            return; // no font in this environment
        };
        let offset = (100, 50);
        let groups = super::text_to_curves(&td, offset);
        assert_eq!(groups.len(), 1, "single colour → single group");
        let path = &groups[0].path;
        assert!(!path.contours.is_empty(), "letters produce contours");
        assert!(path.validate().is_ok());
        let b = path.control_bounds().expect("non-empty bounds");
        // The curves must sit within (a small margin of) the placed raster's
        // canvas rect — i.e. right where the ink was drawn, not off elsewhere.
        let m = 4.0;
        assert!(b.x >= offset.0 as f32 - m, "left edge {} too far left", b.x);
        assert!(b.y >= offset.1 as f32 - m, "top edge {} too far up", b.y);
        assert!(b.x + b.w <= offset.0 as f32 + placed.width as f32 + m);
        assert!(b.y + b.h <= offset.1 as f32 + placed.height as f32 + m);
    }

    #[test]
    fn text_to_curves_splits_by_colour() {
        let mut td = TextData {
            content: "AB".to_string(),
            font_family: TextFontFamily::DejaVuSans,
            font_px: 48.0,
            ..TextData::default()
        };
        if super::font_for(&td.font_family).is_none() {
            return;
        }
        td.glyph_styles = "AB"
            .chars()
            .enumerate()
            .map(|(i, _)| super::GlyphStyle {
                color: if i == 0 {
                    [220, 20, 20, 255]
                } else {
                    [20, 20, 220, 255]
                },
                font_px: td.font_px,
                font_family: td.font_family.clone(),
                bold: false,
                italic: false,
                underline: false,
            })
            .collect();
        let groups = super::text_to_curves(&td, (0, 0));
        assert_eq!(groups.len(), 2, "two colours → two groups");
        assert!(groups.iter().any(|g| g.color == [220, 20, 20, 255]));
        assert!(groups.iter().any(|g| g.color == [20, 20, 220, 255]));
        assert!(groups.iter().all(|g| !g.path.contours.is_empty()));
    }

    #[test]
    fn text_to_curves_empty_is_no_groups() {
        let td = TextData {
            content: "   ".to_string(),
            font_family: TextFontFamily::DejaVuSans,
            ..TextData::default()
        };
        assert!(super::text_to_curves(&td, (0, 0)).is_empty());
    }
}
