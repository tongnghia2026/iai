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
        let td = TextData {
            content: p.text.clone(),
            font_px: p.size_px,
            color,
            font_family: match &p.font {
                Some(name) => TextFontFamily::from_storage_name(&format!("System:{name}")),
                None => TextFontFamily::SegoeUi,
            },
            align: match p.align {
                Align::Left => TextAlign::Left,
                Align::Center => TextAlign::Center,
                Align::Right => TextAlign::Right,
            },
            line_height: p.line_height,
            bold: p.bold,
            italic: p.italic,
            underline: p.underline,
            ..TextData::default()
        };
        let canvas = s.canvas_mut(Some(doc))?;
        let (cw, ch) = (canvas.width, canvas.height);
        let mut cmd = LayerStructureCommand::capture_before("Text", &canvas.layer_stack, cw, ch);
        let idx = canvas.layer_stack.add_layer(cw, ch);
        rasterize_into_layer(canvas, idx, (p.x, p.y), td);
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
        rasterize_into_layer(canvas, idx, origin, p.spec.data());
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
        };
        s.emit(EngineEvent::Structure { doc });
        Ok(Plan::Done(out))
    }
}
