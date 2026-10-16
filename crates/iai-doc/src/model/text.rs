//! Text layers as data: the style, the font family a layer names and the
//! per-character overrides. Laying text out and finding fonts on the system
//! live in `core::text`.

/// Shared range for editable Type layers. Poster-sized text can legitimately
/// exceed the old 1600 px ceiling after Free Transform; every writer and size
/// control must use the same limit or reopening Type clamps the layer smaller.
pub const MIN_EDITABLE_FONT_PX: f32 = 4.0;
pub const MAX_EDITABLE_FONT_PX: f32 = 4096.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TextAlign {
    Left,
    Center,
    Right,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
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

pub fn normalized_family_key(name: &str) -> String {
    name.chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .map(|c| c.to_ascii_lowercase())
        .collect()
}

pub(crate) fn sanitize_font_id(s: &str) -> String {
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
