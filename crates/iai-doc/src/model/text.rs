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
        if let Some(face) = s.strip_prefix("SystemFace:")
            && let Some((family, style)) = face.split_once('\u{1f}')
        {
            return TextFontFamily::SystemFace {
                family: family.to_string(),
                style: style.to_string(),
            };
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

/// Text set along a path: Illustrator's Type on a Path, CorelDRAW's Fit
/// Text To Path. The letters stand on the path one by one, each turned to
/// the path's direction where it stands.
#[derive(Debug, Clone, PartialEq)]
pub struct TextPath {
    /// The path, flattened, in the text's own space: canvas pixels with the
    /// text's upright origin (the layer offset less its placement delta) at
    /// (0, 0). The text reads in the direction the points run, its tops on
    /// the left of that direction (outward along a clockwise circle).
    pub points: Vec<[f32; 2]>,
    /// The path goes round (a circle): the letters wrap past the end.
    pub closed: bool,
    /// Where the text stands along the path, 0 to 1 of its length: where it
    /// starts (left aligned), its middle (centred) or where it ends (right).
    pub start: f32,
    /// The baseline's distance from the path, pixels: positive lifts the
    /// letters off the path on their tops' side.
    pub offset: f32,
    /// On the other side of the path, reading the other way (along the
    /// bottom of a circle, upright).
    pub flip: bool,
    /// The vector layer the path was taken from: when that layer's outline
    /// changes the text follows it (CorelDRAW keeps Fit Text To Path live).
    /// `None` for a circle or a path the text no longer follows.
    pub source: Option<u32>,
    /// The source outline's fingerprint when `points` were last taken from
    /// it (see `iai_canvas::design::path_signature`).
    pub source_sig: u64,
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
    /// Set along a path instead of in straight lines (stretch, rotation and
    /// flips do not apply then).
    pub path: Option<TextPath>,
    /// Filled with a gradient across the letters instead of their colours
    /// (gold lettering on a price list, plan M3.3); the words stay
    /// editable. `None`: the colours as they are.
    pub fill: Option<TextFill>,
}

/// A gradient across a text's letters: colours at shares of the way across
/// the letters' own box, along `angle_deg` (0: left to right, 90: top to
/// bottom). It goes with the text: moved, edited or resized, it spans the
/// letters again.
#[derive(Debug, Clone, PartialEq)]
pub struct TextFill {
    /// Straight RGBA and its place, 0..1, in order; 2 to 8 of them.
    pub stops: Vec<([u8; 4], f32)>,
    pub angle_deg: f32,
}

impl TextFill {
    /// The colour at `t` (0..1) of the way.
    pub fn at(&self, t: f32) -> [u8; 4] {
        let t = t.clamp(0.0, 1.0);
        let Some(first) = self.stops.first() else {
            return [0, 0, 0, 255];
        };
        if t <= first.1 {
            return first.0;
        }
        for w in self.stops.windows(2) {
            let ((a, ta), (b, tb)) = (w[0], w[1]);
            if t <= tb {
                let k = if tb > ta { (t - ta) / (tb - ta) } else { 1.0 };
                let mix = |i: usize| {
                    (f32::from(a[i]) + (f32::from(b[i]) - f32::from(a[i])) * k).round() as u8
                };
                return [mix(0), mix(1), mix(2), mix(3)];
            }
        }
        self.stops.last().map_or(first.0, |l| l.0)
    }

    /// The share of the way of point `(x, y)` in the box `(x0, y0, x1, y1)`.
    pub fn share(&self, x: f32, y: f32, b: (f32, f32, f32, f32)) -> f32 {
        let (c, s) = (
            self.angle_deg.to_radians().cos(),
            self.angle_deg.to_radians().sin(),
        );
        let (cx, cy) = ((b.0 + b.2) / 2.0, (b.1 + b.3) / 2.0);
        let half = ((b.2 - b.0) * c.abs() + (b.3 - b.1) * s.abs()) / 2.0;
        if half <= 0.0 {
            return 0.5;
        }
        (((x - cx) * c + (y - cy) * s) / half + 1.0) / 2.0
    }
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
            path: None,
            fill: None,
        }
    }
}
