//! Text layers: editable text placed on the page.

use iai_canvas::canvas::text_layer::rasterize_into_layer;
use iai_doc::model::text::{GlyphStyle, TextAlign, TextData, TextFontFamily};
use iai_history::command::LayerStructureCommand;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::common::{Bounds, parse_color};
use crate::{
    CmdError, Command, CommandMeta, Effect, EngineEvent, Plan, Session, Validate, layer_index,
};

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Align {
    #[default]
    Left,
    Center,
    Right,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct TextParams {
    /// Document id (see `doc.list`); the document shown when omitted.
    pub doc: Option<u32>,
    /// The text; "\n" starts a new line. Vietnamese is fine.
    #[schemars(length(min = 1, max = 20000))]
    pub text: String,
    /// The top-left corner of the text, in document pixels.
    #[schemars(range(min = -100000, max = 100000))]
    pub x: i32,
    #[schemars(range(min = -100000, max = 100000))]
    pub y: i32,
    /// Letter size in pixels.
    #[serde(default = "size_48")]
    #[schemars(range(min = 1.0, max = 5000.0))]
    pub size_px: f32,
    /// A font installed on this computer, e.g. "Arial", "Times New Roman";
    /// Segoe UI when omitted.
    #[schemars(length(min = 1, max = 200))]
    pub font: Option<String>,
    /// "#RRGGBB" or "#RRGGBBAA"; black when omitted.
    pub color: Option<String>,
    #[serde(default)]
    pub align: Align,
    #[serde(default)]
    pub bold: bool,
    #[serde(default)]
    pub italic: bool,
    #[serde(default)]
    pub underline: bool,
    /// Space between lines, as a multiple of the size.
    #[serde(default = "line_1_2")]
    #[schemars(range(min = 0.5, max = 5.0))]
    pub line_height: f32,
    /// The layer's name; the text's first words when omitted.
    #[schemars(length(min = 1, max = 200))]
    pub name: Option<String>,
    /// One face of the font by its style name ("Bold", "Light Italic",
    /// "Black"; `text.fonts` lists them). Without it `bold` / `italic`
    /// take the font's own bold / italic face when it has one.
    #[schemars(length(min = 1, max = 100))]
    pub face: Option<String>,
    /// More (or, below 0, less) space between letters, pixels.
    #[schemars(range(min = -200.0, max = 2000.0))]
    pub tracking: Option<f32>,
    /// Letters widened (above 1) or narrowed, 0.3 to 3.
    #[schemars(range(min = 0.3, max = 3.0))]
    pub stretch_x: Option<f32>,
    /// Turned clockwise, degrees.
    #[schemars(range(min = -360.0, max = 360.0))]
    pub turn_deg: Option<f32>,
}

fn size_48() -> f32 {
    48.0
}

fn line_1_2() -> f32 {
    1.2
}

impl Validate for TextParams {
    fn validate(&self) -> Result<(), CmdError> {
        if let Some(c) = &self.color {
            parse_color("color", c)?;
        }
        if self.text.trim().is_empty() {
            return Err(CmdError::bad_param("text", "some text"));
        }
        Ok(())
    }
}

#[derive(Debug, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TextAdded {
    pub doc: u32,
    pub layer: u32,
    /// Where the text's pixels lie on the page.
    pub bounds: Bounds,
    /// text.add with a `font`: whether it is on this computer (false: the
    /// letters are drawn in another font).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub font_found: Option<bool>,
    /// text.add: the face the letters are drawn in ("Regular", "Bold").
    #[serde(skip_serializing_if = "Option::is_none")]
    pub face_used: Option<String>,
    /// text.add: whether that face draws every Vietnamese letter.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vietnamese: Option<bool>,
}

/// The face of `family` called `style`, if the font has it.
fn face_named(family: &TextFontFamily, style: &str) -> Option<TextFontFamily> {
    use iai_text::text::SystemFonts;
    let want = style.trim();
    family
        .faces()
        .into_iter()
        .find(|f| f.style_name().eq_ignore_ascii_case(want))
}

/// The family and face to draw in, and whether bold / italic are then
/// still to be faked: a named face, else the font's own bold / italic.
pub fn choose_face(
    family: TextFontFamily,
    face: Option<&str>,
    bold: bool,
    italic: bool,
) -> Result<(TextFontFamily, bool, bool), CmdError> {
    if let Some(style) = face {
        return match face_named(&family, style) {
            Some(f) => Ok((f, false, false)),
            None => {
                use iai_text::text::SystemFonts;
                let have: Vec<String> = family
                    .faces()
                    .iter()
                    .map(|f| f.style_name().to_string())
                    .collect();
                Err(CmdError::bad_param(
                    "face",
                    &format!("a face of {}: {}", family.name(), have.join(", ")),
                ))
            }
        };
    }
    let wanted = match (bold, italic) {
        (true, true) => ["Bold Italic", "BoldItalic"].as_slice(),
        (true, false) => ["Bold"].as_slice(),
        (false, true) => ["Italic", "Oblique"].as_slice(),
        (false, false) => return Ok((family, false, false)),
    };
    match wanted.iter().find_map(|w| face_named(&family, w)) {
        Some(f) => Ok((f, false, false)),
        None => Ok((family, bold, italic)),
    }
}

static ADD: CommandMeta = CommandMeta {
    id: "text.add",
    label_vi: "Thêm chữ",
    label_en: "Add Text",
    doc: "Put text on the page as a new text layer that stays editable: font, size, colour, \
          alignment, bold / italic / underline.",
    effects: &[Effect::Edit],
    featured: true,
};

pub struct AddText;

impl Command for AddText {
    type Params = TextParams;
    type Output = TextAdded;
    fn meta(&self) -> &'static CommandMeta {
        &ADD
    }
    fn plan(&self, s: &mut Session<'_>, p: TextParams) -> Result<Plan<TextAdded>, CmdError> {
        let doc = s.doc_id(p.doc)?;
        let color = match &p.color {
            Some(c) => parse_color("color", c)?,
            None => [0, 0, 0, 255],
        };
        let family = match &p.font {
            Some(name) => TextFontFamily::from_storage_name(&format!("System:{}", name.trim())),
            None => TextFontFamily::SegoeUi,
        };
        let font_found = p.font.as_ref().map(|_| iai_text::text::installed(&family));
        let (family, bold, italic) = choose_face(family, p.face.as_deref(), p.bold, p.italic)?;
        let face_used = family.style_name().to_string();
        let vietnamese = iai_text::text::covers(&family, iai_text::text::VIETNAMESE_PROBE);
        let td = TextData {
            content: p.text.clone(),
            font_px: p.size_px,
            color,
            font_family: family,
            align: match p.align {
                Align::Left => TextAlign::Left,
                Align::Center => TextAlign::Center,
                Align::Right => TextAlign::Right,
            },
            line_height: p.line_height,
            bold,
            italic,
            underline: p.underline,
            tracking_px: p.tracking.unwrap_or(0.0),
            stretch_x: p.stretch_x.unwrap_or(1.0),
            rotation_deg: p.turn_deg.unwrap_or(0.0),
            ..TextData::default()
        };
        let canvas = s.canvas_mut(Some(doc))?;
        let (cw, ch) = (canvas.width, canvas.height);
        let mut cmd = LayerStructureCommand::capture_before("Text", &canvas.layer_stack, cw, ch);
        let idx = canvas.layer_stack.add_layer(cw, ch);
        rasterize_into_layer(canvas, idx, (p.x, p.y), td);
        if let Some(name) = p.name.as_deref().map(str::trim).filter(|n| !n.is_empty())
            && let Some(l) = canvas.layer_stack.layers.get_mut(idx)
        {
            l.name = name.to_string();
        }
        cmd.capture_after(&canvas.layer_stack, cw, ch);
        canvas.record(Box::new(cmd));
        canvas.layer_revision = canvas.layer_revision.saturating_add(1);
        canvas.flatten_full();
        let layer = canvas
            .layer_stack
            .layers
            .get(idx)
            .ok_or_else(|| CmdError::internal("new layer"))?;
        let out = TextAdded {
            doc,
            layer: layer.id,
            bounds: Bounds {
                x: layer.offset.0,
                y: layer.offset.1,
                w: layer.width,
                h: layer.height,
            },
            font_found,
            face_used: Some(face_used),
            vietnamese: Some(vietnamese),
        };
        s.emit(EngineEvent::Structure { doc });
        Ok(Plan::Done(out))
    }
}

/// A font, exactly as a type layer names it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Font {
    /// One the app carries: SegoeUi, Arial, Calibri, Tahoma, DejaVuSans,
    /// LiberationSans.
    Builtin { name: String },
    /// A font family installed on this computer.
    System { name: String },
    /// One face (style) of an installed family.
    Face { family: String, style: String },
}

impl Font {
    fn of(family: &TextFontFamily) -> Self {
        match family {
            TextFontFamily::System(name) => Font::System { name: name.clone() },
            TextFontFamily::SystemFace { family, style } => Font::Face {
                family: family.clone(),
                style: style.clone(),
            },
            builtin => Font::Builtin {
                name: builtin.storage_name(),
            },
        }
    }

    fn family(&self) -> TextFontFamily {
        match self {
            Font::Builtin { name } => TextFontFamily::from_storage_name(name),
            Font::System { name } => TextFontFamily::System(name.clone()),
            Font::Face { family, style } => TextFontFamily::SystemFace {
                family: family.clone(),
                style: style.clone(),
            },
        }
    }
}

/// How one character differs from the type's own style.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct GlyphSpec {
    /// [r, g, b, a].
    pub color: [u8; 4],
    pub size_px: f32,
    pub font: Font,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
}

/// The path a type layer is set along (see `text.fit_to_path`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct PathSpec {
    /// The path, flattened, relative to the type's origin.
    pub points: Vec<[f32; 2]>,
    pub closed: bool,
    /// Where the text stands along it, 0 to 1.
    pub start: f32,
    /// Baseline's distance from the path, pixels.
    pub offset: f32,
    /// On the other side, reading the other way.
    pub flip: bool,
    /// The vector layer the text follows (it moves with that outline).
    #[serde(default)]
    pub source: Option<u32>,
    /// That outline's fingerprint when last followed.
    #[serde(default)]
    pub source_sig: u64,
}

/// Everything a type layer is.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct TypeSpec {
    /// The text; "\n" starts a new line.
    #[schemars(length(max = 20000))]
    pub text: String,
    #[schemars(range(min = 0.1, max = 100000.0))]
    pub size_px: f32,
    /// [r, g, b, a].
    pub color: [u8; 4],
    pub font: Font,
    pub align: Align,
    /// Space between lines, as a multiple of the size.
    #[schemars(range(min = 0.1, max = 20.0))]
    pub line_height: f32,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    /// Extra space between letters, in pixels.
    pub tracking_px: f32,
    /// 0 to 1.
    #[schemars(range(min = 0.0, max = 1.0))]
    pub opacity: f32,
    /// Horizontal stretch (1 = as drawn).
    pub stretch_x: f32,
    /// Turned clockwise, in degrees.
    pub rotation: f32,
    pub flip_x: bool,
    pub flip_y: bool,
    /// One entry per character of `text` when some characters differ from
    /// the rest; empty when none does.
    #[serde(default)]
    pub glyphs: Vec<GlyphSpec>,
    /// Set along a path rather than in lines.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<PathSpec>,
}

impl TypeSpec {
    /// The spec of `data`, losing nothing.
    pub fn of(data: &TextData) -> Self {
        TypeSpec {
            text: data.content.clone(),
            size_px: data.font_px,
            color: data.color,
            font: Font::of(&data.font_family),
            align: match data.align {
                TextAlign::Left => Align::Left,
                TextAlign::Center => Align::Center,
                TextAlign::Right => Align::Right,
            },
            line_height: data.line_height,
            bold: data.bold,
            italic: data.italic,
            underline: data.underline,
            tracking_px: data.tracking_px,
            opacity: data.opacity,
            stretch_x: data.stretch_x,
            rotation: data.rotation_deg,
            flip_x: data.flip_x,
            flip_y: data.flip_y,
            glyphs: data
                .glyph_styles
                .iter()
                .map(|g| GlyphSpec {
                    color: g.color,
                    size_px: g.font_px,
                    font: Font::of(&g.font_family),
                    bold: g.bold,
                    italic: g.italic,
                    underline: g.underline,
                })
                .collect(),
            path: data.path.as_ref().map(|p| PathSpec {
                points: p.points.clone(),
                closed: p.closed,
                start: p.start,
                offset: p.offset,
                flip: p.flip,
                source: p.source,
                source_sig: p.source_sig,
            }),
        }
    }

    /// The type layer's data.
    pub fn data(&self) -> TextData {
        TextData {
            content: self.text.clone(),
            font_px: self.size_px,
            color: self.color,
            font_family: self.font.family(),
            align: match self.align {
                Align::Left => TextAlign::Left,
                Align::Center => TextAlign::Center,
                Align::Right => TextAlign::Right,
            },
            line_height: self.line_height,
            bold: self.bold,
            italic: self.italic,
            underline: self.underline,
            tracking_px: self.tracking_px,
            opacity: self.opacity,
            stretch_x: self.stretch_x,
            rotation_deg: self.rotation,
            flip_x: self.flip_x,
            flip_y: self.flip_y,
            fill: None,
            glyph_styles: self
                .glyphs
                .iter()
                .map(|g| GlyphStyle {
                    color: g.color,
                    font_px: g.size_px,
                    font_family: g.font.family(),
                    bold: g.bold,
                    italic: g.italic,
                    underline: g.underline,
                })
                .collect(),
            path: self.path.as_ref().map(|p| iai_doc::model::text::TextPath {
                points: p.points.clone(),
                closed: p.closed,
                start: p.start,
                offset: p.offset,
                flip: p.flip,
                source: p.source,
                source_sig: p.source_sig,
            }),
        }
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct SetTextParams {
    /// Document id (see `doc.list`); the document shown when omitted.
    pub doc: Option<u32>,
    /// The type layer to write; a new one, above the active layer, when
    /// omitted.
    pub layer: Option<u32>,
    /// What the type is.
    #[serde(rename = "type")]
    pub spec: TypeSpec,
    /// Where the text starts, [x, y] in document pixels (its top-left before
    /// any turn).
    pub origin: [i32; 2],
    /// The undo step's name.
    #[schemars(length(min = 1, max = 80))]
    pub label: Option<String>,
}

impl Validate for SetTextParams {
    fn validate(&self) -> Result<(), CmdError> {
        let s = &self.spec;
        let numbers = [
            s.size_px,
            s.line_height,
            s.tracking_px,
            s.opacity,
            s.stretch_x,
            s.rotation,
        ];
        if !numbers.into_iter().all(f32::is_finite) {
            return Err(CmdError::bad_param("type", "finite numbers"));
        }
        if s.text.trim().is_empty() {
            return Err(CmdError::bad_param("type", "some text"));
        }
        Ok(())
    }
}

static SET: CommandMeta = CommandMeta {
    id: "text.set",
    label_vi: "Đặt chữ",
    label_en: "Set Text",
    doc: "Write a type layer whole, as the Type tool does when it commits: the text and every \
          style of it (font, size, colour, per-character styles, tracking, stretch, turn, \
          mirror). Changes the type layer `layer`, or makes a new one above the active layer. \
          `doc.inspect` gives a layer's current text. One undo step.",
    effects: &[Effect::Edit],
    featured: false,
};

pub struct SetText;

impl Command for SetText {
    type Params = SetTextParams;
    type Output = TextAdded;
    fn meta(&self) -> &'static CommandMeta {
        &SET
    }
    fn plan(&self, s: &mut Session<'_>, p: SetTextParams) -> Result<Plan<TextAdded>, CmdError> {
        let doc = s.doc_id(p.doc)?;
        let canvas = s.canvas_mut(Some(doc))?;
        let (cw, ch) = (canvas.width, canvas.height);
        let target = match p.layer {
            Some(id) => {
                let idx = layer_index(canvas, Some(id))?;
                let is_type =
                    canvas.layer_stack.layers.get(idx).is_some_and(|l| {
                        matches!(l.layer_type, iai_doc::layer::LayerType::Text(_))
                    });
                if !is_type {
                    return Err(super::common::not_enabled(
                        "that layer is not a type layer",
                        "Layer này không phải layer chữ",
                    ));
                }
                Some(idx)
            }
            None => None,
        };
        let label = p.label.as_deref().unwrap_or(match target {
            Some(_) => "Edit Text",
            None => "Type",
        });
        let mut cmd = LayerStructureCommand::capture_before(label, &canvas.layer_stack, cw, ch);
        let origin = (p.origin[0], p.origin[1]);
        let idx = match target {
            Some(idx) => idx,
            None => {
                let idx = canvas.layer_stack.add_layer(cw, ch);
                if let Some(layer) = canvas.layer_stack.layers.get_mut(idx) {
                    layer.name = "Text".to_string();
                    layer.offset = origin;
                }
                idx
            }
        };
        canvas.layer_stack.active_idx = idx;
        // A gradient across the letters stays when the words are edited
        // (the Type tool writes the type whole).
        let mut td = p.spec.data();
        if let Some(iai_doc::layer::LayerType::Text(old)) = target
            .and_then(|i| canvas.layer_stack.layers.get(i))
            .map(|l| &l.layer_type)
        {
            td.fill = old.fill.clone();
        }
        rasterize_into_layer(canvas, idx, origin, td);
        cmd.capture_after(&canvas.layer_stack, cw, ch);
        canvas.record(Box::new(cmd));
        canvas.layer_revision = canvas.layer_revision.saturating_add(1);
        canvas.flatten_full();
        let layer = canvas
            .layer_stack
            .layers
            .get(idx)
            .ok_or_else(|| CmdError::internal("type layer"))?;
        let out = TextAdded {
            doc,
            layer: layer.id,
            bounds: Bounds {
                x: layer.offset.0,
                y: layer.offset.1,
                w: layer.width,
                h: layer.height,
            },
            font_found: None,
            face_used: None,
            vietnamese: None,
        };
        s.emit(EngineEvent::Structure { doc });
        Ok(Plan::Done(out))
    }
}

// ---- text.fit_frame --------------------------------------------------------

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct FitFrameParams {
    /// Document id (see `doc.list`); the document shown when omitted.
    pub doc: Option<u32>,
    /// The type layer; the active one when omitted.
    pub layer: Option<u32>,
    /// The frame's width, document pixels.
    #[schemars(range(min = 1.0, max = 100000.0))]
    pub width: f32,
    /// The frame's height, document pixels; only the width counts when omitted.
    #[schemars(range(min = 1.0, max = 100000.0))]
    pub height: Option<f32>,
}

impl Validate for FitFrameParams {
    fn validate(&self) -> Result<(), CmdError> {
        let ok = |v: f32| v.is_finite() && v >= 1.0;
        if !ok(self.width) {
            return Err(CmdError::bad_param("width", "at least 1 pixel"));
        }
        if self.height.is_some_and(|h| !ok(h)) {
            return Err(CmdError::bad_param("height", "at least 1 pixel"));
        }
        Ok(())
    }
}

static FIT_FRAME: CommandMeta = CommandMeta {
    id: "text.fit_frame",
    label_vi: "Co chữ vừa khung",
    label_en: "Fit Text To Frame",
    doc: "Make a type layer's letters larger or smaller, all in proportion, until the text \
          just fills a frame of `width` (and `height`) pixels: a name made to fill a banner, a \
          title its column. Its top-left corner stays. The text stays editable. One undo step.",
    effects: &[Effect::Edit],
    featured: false,
};

pub struct FitTextToFrame;

impl Command for FitTextToFrame {
    type Params = FitFrameParams;
    type Output = TextAdded;
    fn meta(&self) -> &'static CommandMeta {
        &FIT_FRAME
    }
    fn plan(&self, s: &mut Session<'_>, p: FitFrameParams) -> Result<Plan<TextAdded>, CmdError> {
        let doc = s.doc_id(p.doc)?;
        let canvas = s.canvas_mut(Some(doc))?;
        let (cw, ch) = (canvas.width, canvas.height);
        let idx = layer_index(canvas, p.layer)?;
        let (mut td, offset) = match canvas.layer_stack.layers.get(idx) {
            Some(l) => match &l.layer_type {
                iai_doc::layer::LayerType::Text(td) => (td.clone(), l.offset),
                _ => {
                    return Err(super::common::not_enabled(
                        "that layer is not a type layer",
                        "Layer này không phải layer chữ",
                    ));
                }
            },
            None => return Err(CmdError::internal("layer")),
        };
        let delta = iai_text::text::rasterize_placed(&td).map_or((0, 0), |(_, d)| d);
        let anchor = (
            offset.0.saturating_sub(delta.0),
            offset.1.saturating_sub(delta.1),
        );
        // Letters do not grow quite in proportion (hinting, rounding), so
        // measure again after each step.
        for _ in 0..4 {
            let Some((r, _)) = iai_text::text::rasterize_placed(&td) else {
                return Err(super::common::not_enabled(
                    "the text has nothing to show",
                    "Layer chữ này trống",
                ));
            };
            let mut k = p.width / r.width.max(1) as f32;
            if let Some(h) = p.height {
                k = k.min(h / r.height.max(1) as f32);
            }
            if (k - 1.0).abs() < 0.005 {
                break;
            }
            td.font_px = (td.font_px * k).clamp(1.0, 4000.0);
            td.tracking_px *= k;
        }
        let label = "Fit Text To Frame";
        let mut cmd = LayerStructureCommand::capture_before(label, &canvas.layer_stack, cw, ch);
        canvas.layer_stack.active_idx = idx;
        rasterize_into_layer(canvas, idx, anchor, td);
        cmd.capture_after(&canvas.layer_stack, cw, ch);
        canvas.record(Box::new(cmd));
        canvas.layer_revision = canvas.layer_revision.saturating_add(1);
        canvas.flatten_full();
        let layer = canvas
            .layer_stack
            .layers
            .get(idx)
            .ok_or_else(|| CmdError::internal("type layer"))?;
        let out = TextAdded {
            doc,
            layer: layer.id,
            bounds: Bounds {
                x: layer.offset.0,
                y: layer.offset.1,
                w: layer.width,
                h: layer.height,
            },
            font_found: None,
            face_used: None,
            vietnamese: None,
        };
        s.emit(EngineEvent::Structure { doc });
        s.notice("Co chữ vừa khung".to_string(), label.to_string());
        Ok(Plan::Done(out))
    }
}

// ---- text.fit_to_path ------------------------------------------------------

/// A circle to set text round: centre and radius, document pixels.
#[derive(Debug, Clone, Copy, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct CircleSpec {
    pub cx: f32,
    pub cy: f32,
    #[schemars(range(min = 1.0, max = 100000.0))]
    pub r: f32,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct FitToPathParams {
    /// Document id (see `doc.list`); the document shown when omitted.
    pub doc: Option<u32>,
    /// The type layer; the active one when omitted.
    pub layer: Option<u32>,
    /// A vector layer whose outline the text follows (its first line or
    /// shape). The layer stays; hide or delete it if it should not show.
    pub path: Option<u32>,
    /// Or a circle to go round, clockwise from the right.
    pub circle: Option<CircleSpec>,
    /// Where the text stands along the path, 0 to 1 of its length (on a
    /// circle 0 is the right, 0.25 the bottom, 0.5 the left, 0.75 the top):
    /// where it starts (left aligned), its middle (centred) or its end
    /// (right). Kept when omitted; else the middle of a line, the top of a
    /// circle (its bottom when `flip`).
    #[schemars(range(min = 0.0, max = 1.0))]
    pub start: Option<f32>,
    /// The baseline's distance from the path, pixels: positive lifts the
    /// letters off it on their tops' side (kept when omitted, else 0).
    pub offset: Option<f32>,
    /// On the other side of the path, reading the other way: along the
    /// bottom of a circle the right way up, tops towards the middle.
    pub flip: Option<bool>,
    /// How the text sits at `start`; centred when the text is first put
    /// on a path.
    pub align: Option<Align>,
    /// Take the text off its path, back to a straight line.
    #[serde(default)]
    pub release: bool,
}

impl Validate for FitToPathParams {
    fn validate(&self) -> Result<(), CmdError> {
        if self.path.is_some() && self.circle.is_some() {
            return Err(CmdError::bad_param(
                "circle",
                "a path or a circle, not both",
            ));
        }
        Ok(())
    }
}

static FIT: CommandMeta = CommandMeta {
    id: "text.fit_to_path",
    label_vi: "Đặt chữ theo đường",
    label_en: "Fit Text To Path",
    doc: "Set a type layer along a curve, a shape's outline or a circle (Illustrator's Type on \
          a Path, CorelDRAW's Fit Text To Path): one text layer, still editable, each letter \
          standing on the path and turned with it. Text round the top of a round logo: \
          `circle` with the default start; along its bottom the right way up: `flip: true` \
          (with `start: 0.25` when turning over text already on the circle: it turns over in \
          place). Call it again on the same layer to move it along (`start`), lift it \
          (`offset`), turn it over (`flip`) or `release` it. One undo step.",
    effects: &[Effect::Edit],
    featured: false,
};

pub struct FitTextToPath;

impl Command for FitTextToPath {
    type Params = FitToPathParams;
    type Output = TextAdded;
    fn meta(&self) -> &'static CommandMeta {
        &FIT
    }
    fn plan(&self, s: &mut Session<'_>, p: FitToPathParams) -> Result<Plan<TextAdded>, CmdError> {
        use iai_doc::model::text::TextPath;
        let doc = s.doc_id(p.doc)?;
        let canvas = s.canvas_mut(Some(doc))?;
        let (cw, ch) = (canvas.width, canvas.height);
        // From the menu: the picked type layer, and the picked vector layer
        // as its path.
        let picked = |want: fn(&iai_doc::layer::LayerType) -> bool| {
            canvas
                .layer_stack
                .layers
                .iter()
                .filter(|l| l.selected && want(&l.layer_type))
                .map(|l| l.id)
                .next()
        };
        let picked_text = picked(|t| matches!(t, iai_doc::layer::LayerType::Text(_)));
        let picked_vector = picked(|t| matches!(t, iai_doc::layer::LayerType::Vector(_)));
        let idx = layer_index(canvas, p.layer.or(picked_text))?;
        let (mut td, offset) = match canvas.layer_stack.layers.get(idx) {
            Some(l) => match &l.layer_type {
                iai_doc::layer::LayerType::Text(td) => (td.clone(), l.offset),
                _ => {
                    return Err(super::common::not_enabled(
                        "that layer is not a type layer",
                        "Layer này không phải layer chữ",
                    ));
                }
            },
            None => return Err(CmdError::internal("layer")),
        };
        // The text's anchor: its upright origin on the page.
        let delta = iai_text::text::rasterize_placed(&td).map_or((0, 0), |(_, d)| d);
        let anchor = (
            offset.0.saturating_sub(delta.0),
            offset.1.saturating_sub(delta.1),
        );
        let rel = |x: f32, y: f32| [x - anchor.0 as f32, y - anchor.1 as f32];
        let new_points: Option<(Vec<[f32; 2]>, bool, Option<(u32, u64)>)> =
            if let Some(c) = p.circle {
                let n = ((c.r * std::f32::consts::TAU / 1.5).ceil() as usize).clamp(64, 4096);
                Some((
                    (0..n)
                        .map(|k| {
                            let a = k as f32 / n as f32 * std::f32::consts::TAU;
                            rel(c.cx + c.r * a.cos(), c.cy + c.r * a.sin())
                        })
                        .collect(),
                    true,
                    None,
                ))
            } else if let Some(id) = p.path.or_else(|| {
                let on_path = matches!(
                    canvas.layer_stack.layers.get(idx).map(|l| &l.layer_type),
                    Some(iai_doc::layer::LayerType::Text(t)) if t.path.is_some()
                );
                (!on_path && !p.release).then_some(picked_vector).flatten()
            }) {
                let pi = layer_index(canvas, Some(id))?;
                let layer = canvas
                    .layer_stack
                    .layers
                    .get(pi)
                    .ok_or_else(|| CmdError::internal("layer"))?;
                let (pts, closed) =
                    iai_canvas::text_follow::text_path_points(layer).ok_or_else(|| {
                        if iai_canvas::shaping::layer_canvas_path(layer).is_none() {
                            super::common::not_enabled(
                                "the path must be a vector layer",
                                "Đường phải là một layer vector",
                            )
                        } else {
                            CmdError::bad_param("path", "a layer with a line or a shape")
                        }
                    })?;
                let sig = iai_canvas::text_follow::path_signature(layer).unwrap_or(0);
                Some((
                    pts.iter().map(|q| rel(q[0], q[1])).collect(),
                    closed,
                    Some((id, sig)),
                ))
            } else {
                None
            };
        if p.release {
            if td.path.is_none() {
                return Err(super::common::not_enabled(
                    "the text is not on a path",
                    "Chữ này không nằm theo đường nào",
                ));
            }
            td.path = None;
        } else {
            let first = td.path.is_none();
            let mut path = match (new_points, td.path.take()) {
                (Some((points, closed, link)), old) => TextPath {
                    points,
                    closed,
                    source: link.map(|l| l.0),
                    source_sig: link.map_or(0, |l| l.1),
                    start: old.as_ref().map_or(0.5, |o| o.start),
                    offset: old.as_ref().map_or(0.0, |o| o.offset),
                    flip: old.as_ref().is_some_and(|o| o.flip),
                },
                (None, Some(old)) => old,
                (None, None) => {
                    return Err(CmdError::bad_param(
                        "path",
                        "a vector layer (`path`) or a `circle` to set the text along",
                    ));
                }
            };
            if let Some(f) = p.flip {
                path.flip = f;
            }
            if let Some(o) = p.offset {
                path.offset = o;
            }
            match p.start {
                Some(st) => path.start = st.clamp(0.0, 1.0),
                None if first || p.circle.is_some() => {
                    path.start = match (path.closed, path.flip) {
                        (true, false) => 0.75,
                        (true, true) => 0.25,
                        _ => 0.5,
                    };
                }
                None => {}
            }
            if first && p.align.is_none() {
                td.align = TextAlign::Center;
            }
            // Turning and stretching do not apply along a path.
            td.rotation_deg = 0.0;
            td.stretch_x = 1.0;
            td.flip_x = false;
            td.flip_y = false;
            td.path = Some(path);
        }
        if let Some(a) = p.align {
            td.align = match a {
                Align::Left => TextAlign::Left,
                Align::Center => TextAlign::Center,
                Align::Right => TextAlign::Right,
            };
        }
        let label = if p.release {
            "Straighten Text"
        } else {
            "Fit Text To Path"
        };
        let mut cmd = LayerStructureCommand::capture_before(label, &canvas.layer_stack, cw, ch);
        canvas.layer_stack.active_idx = idx;
        rasterize_into_layer(canvas, idx, anchor, td);
        cmd.capture_after(&canvas.layer_stack, cw, ch);
        canvas.record(Box::new(cmd));
        canvas.layer_revision = canvas.layer_revision.saturating_add(1);
        canvas.flatten_full();
        let layer = canvas
            .layer_stack
            .layers
            .get(idx)
            .ok_or_else(|| CmdError::internal("type layer"))?;
        let out = TextAdded {
            doc,
            layer: layer.id,
            bounds: Bounds {
                x: layer.offset.0,
                y: layer.offset.1,
                w: layer.width,
                h: layer.height,
            },
            font_found: None,
            face_used: None,
            vietnamese: None,
        };
        s.emit(EngineEvent::Structure { doc });
        s.notice(label.to_string(), label.to_string());
        Ok(Plan::Done(out))
    }
}

// ---- text.fonts --------------------------------------------------------------

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct FontsParams {
    /// Only fonts whose name has these letters ("roboto", "times").
    #[schemars(length(min = 1, max = 100))]
    pub query: Option<String>,
    /// Only fonts that draw every Vietnamese letter.
    #[serde(default)]
    pub vietnamese: bool,
    /// Only fonts of this kind: "sans", "serif", "mono", "script" (hand
    /// lettering, brush) or "display" (heavy, decorated headlines).
    pub class: Option<FontClass>,
    /// The most to list (40 when omitted).
    #[schemars(range(min = 1, max = 200))]
    pub limit: Option<u32>,
}

impl Validate for FontsParams {}

/// What kind of letters a font has, as its name tells.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum FontClass {
    Sans,
    Serif,
    Mono,
    Script,
    Display,
}

/// A font's kind from its name (the names of the fonts Windows and
/// designers install; a font not known is taken for a sans).
pub fn font_class(name: &str) -> FontClass {
    let n = name.to_lowercase();
    let has = |words: &[&str]| words.iter().any(|w| n.contains(w));
    if has(&[
        "mono",
        "courier",
        "consol",
        "code",
        "lucida console",
        "typewriter",
    ]) {
        FontClass::Mono
    } else if has(&[
        "script",
        "brush",
        "hand",
        "dancing",
        "pacifico",
        "lobster",
        "comic",
        "segoe print",
        "vivaldi",
        "freestyle",
        "kunstler",
        "mistral",
        "signature",
        "calligraph",
        "satisfy",
        "great vibes",
        "ink free",
        "gabriola",
        "edwardian",
        "palace",
        "vladimir",
    ]) {
        FontClass::Script
    } else if has(&[
        "impact",
        "stencil",
        "showcard",
        "jokerman",
        "chiller",
        "old english",
        "algerian",
        "broadway",
        "bauhaus",
        "black",
        "poster",
        "display",
        "cooper",
        "ravie",
        "snap",
    ]) {
        FontClass::Display
    } else if (has(&["serif"]) && !has(&["sans"]))
        || has(&[
            "times",
            "georgia",
            "garamond",
            "cambria",
            "book antiqua",
            "palatino",
            "baskerville",
            "bodoni",
            "cormorant",
            "merriweather",
            "playfair",
            "constantia",
            "century schoolbook",
            "roman",
            "lora",
            "noto serif",
            "sylfaen",
        ])
    {
        FontClass::Serif
    } else {
        FontClass::Sans
    }
}

#[derive(Debug, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct FontOut {
    /// The name `text.add` takes as `font`.
    pub name: String,
    /// Its kind, by its name.
    pub class: FontClass,
    /// Its faces, the names `text.add` takes as `face`.
    pub faces: Vec<String>,
    /// Whether it draws every Vietnamese letter.
    pub vietnamese: bool,
}

#[derive(Debug, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct FontsList {
    pub fonts: Vec<FontOut>,
    /// More matched than were listed.
    pub more: bool,
}

static FONTS: CommandMeta = CommandMeta {
    id: "text.fonts",
    label_vi: "Danh sách font",
    label_en: "List Fonts",
    doc: "The fonts installed on this computer (for everyone and for this person), each with its \
          faces (Regular, Bold, Light Italic…) and whether it draws every Vietnamese letter. \
          Narrow it with `query` (letters of the name), `vietnamese: true` and `class` (sans, \
          serif, mono, script, display).",
    effects: &[Effect::Query],
    featured: false,
};

pub struct ListFonts;

impl Command for ListFonts {
    type Params = FontsParams;
    type Output = FontsList;
    fn meta(&self) -> &'static CommandMeta {
        &FONTS
    }
    fn plan(&self, _s: &mut Session<'_>, p: FontsParams) -> Result<Plan<FontsList>, CmdError> {
        use iai_text::text::SystemFonts;
        let limit = p.limit.unwrap_or(40) as usize;
        let query = p.query.as_deref().map(|q| q.trim().to_lowercase());
        let mut fonts = Vec::new();
        let mut more = false;
        let mut seen = std::collections::HashSet::new();
        for family in TextFontFamily::all() {
            let name = family.name().to_string();
            if !seen.insert(name.to_lowercase()) {
                continue;
            }
            if let Some(q) = &query
                && !name.to_lowercase().contains(q.as_str())
            {
                continue;
            }
            let class = font_class(&name);
            if p.class.is_some_and(|c| c != class) {
                continue;
            }
            if fonts.len() >= limit {
                more = true;
                break;
            }
            let vietnamese = iai_text::text::covers_once(family, iai_text::text::VIETNAMESE_PROBE);
            if p.vietnamese && !vietnamese {
                continue;
            }
            fonts.push(FontOut {
                class,
                faces: family
                    .faces()
                    .iter()
                    .map(|f| f.style_name().to_string())
                    .collect(),
                name,
                vietnamese,
            });
        }
        Ok(Plan::Done(FontsList { fonts, more }))
    }
}
