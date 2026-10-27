//! Text layers: editable text placed on the page.

use iai_canvas::canvas::text_layer::rasterize_into_layer;
use iai_doc::model::text::{TextAlign, TextData, TextFontFamily};
use iai_history::command::LayerStructureCommand;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::common::{Bounds, parse_color};
use crate::{CmdError, Command, CommandMeta, Effect, EngineEvent, Plan, Session, Validate};

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Deserialize, JsonSchema)]
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
