//! The whole page: crop, resize, rotate, flip.

use iai_canvas::canvas::MAX_DIMENSION;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::common::{not_enabled, RectPx};
use crate::{CmdError, Command, CommandMeta, Effect, EngineEvent, Plan, Session, Validate};

/// The page's size after the change.
#[derive(Debug, Clone, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Sized {
    pub doc: u32,
    pub width_px: u32,
    pub height_px: u32,
    pub dpi: f32,
}

fn sized(s: &mut Session<'_>, doc: u32) -> Result<Sized, CmdError> {
    let c = s.canvas(Some(doc))?;
    let out = Sized {
        doc,
        width_px: c.width,
        height_px: c.height,
        dpi: c.metadata.resolution_ppi,
    };
    s.emit(EngineEvent::Structure { doc });
    s.emit(EngineEvent::Selection { doc });
    Ok(out)
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct CropParams {
    /// Document id (see `doc.list`); the document shown when omitted.
    pub doc: Option<u32>,
    /// What to keep, in document pixels. May reach past the page: the page
    /// grows there (clear). Leave it out to crop to the selection.
    pub rect: Option<RectPx>,
    /// Throw away what lies outside (default); false keeps it, hidden, so a
    /// later crop or move can bring it back.
    #[serde(default = "yes")]
    pub delete_cropped: bool,
}

fn yes() -> bool {
    true
}

impl Validate for CropParams {}

static CROP: CommandMeta = CommandMeta {
    id: "image.crop",
    label_vi: "Cắt ảnh",
    label_en: "Crop",
    doc: "Crop the page to a rectangle, or to the selection's bounds when `rect` is left out. \
          Every layer is cropped; one undo step.",
    effects: &[Effect::Edit],
    featured: true,
};

pub struct Crop;

impl Command for Crop {
    type Params = CropParams;
    type Output = Sized;
    fn meta(&self) -> &'static CommandMeta {
        &CROP
    }
    fn plan(&self, s: &mut Session<'_>, p: CropParams) -> Result<Plan<Sized>, CmdError> {
        let doc = s.doc_id(p.doc)?;
        let canvas = s.canvas_mut(Some(doc))?;
        let rect = match p.rect {
            Some(r) => r,
            None => {
                if !canvas.selection.active {
                    return Err(not_enabled(
                        "give `rect`, or select what to keep first",
                        "Hãy chọn vùng cần giữ trước",
                    ));
                }
                let (x0, y0, x1, y1) = canvas.selection.bounding_box();
                let b = super::common::Bounds::from_corners(x0, y0, x1, y1)
                    .ok_or_else(|| not_enabled("the selection is empty", "Vùng chọn trống"))?;
                RectPx {
                    x: b.x,
                    y: b.y,
                    w: b.w,
                    h: b.h,
                }
            }
        };
        if rect.w > MAX_DIMENSION || rect.h > MAX_DIMENSION {
            return Err(CmdError::bad_param(
                "rect",
                &format!("a size of at most {MAX_DIMENSION} pixels"),
            ));
        }
        if !canvas.crop(rect.x, rect.y, rect.w, rect.h, p.delete_cropped) {
            return Err(not_enabled("the crop was refused", "Không cắt được"));
        }
        if p.rect.is_none() {
            canvas.deselect();
        }
        sized(s, doc).map(Plan::Done)
    }
}

/// How pixels are made at the new size.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ResizeParams {
    /// Document id (see `doc.list`); the document shown when omitted.
    pub doc: Option<u32>,
    /// New width; with only one side given, the other follows the shape.
    #[schemars(range(min = 1, max = 30000))]
    pub width_px: Option<u32>,
    #[schemars(range(min = 1, max = 30000))]
    pub height_px: Option<u32>,
    /// New resolution (pixels per inch). Alone, it changes the print size only.
    #[schemars(range(min = 1.0, max = 9600.0))]
    pub dpi: Option<f32>,
}

impl Validate for ResizeParams {
    fn validate(&self) -> Result<(), CmdError> {
        if self.width_px.is_none() && self.height_px.is_none() && self.dpi.is_none() {
            return Err(CmdError::bad_param("(params)", "widthPx, heightPx or dpi"));
        }
        Ok(())
    }
}

static RESIZE: CommandMeta = CommandMeta {
    id: "image.resize",
    label_vi: "Đổi cỡ ảnh",
    label_en: "Image Size",
    doc: "Resample the page and every layer to a new size in pixels (give one side to keep the \
          shape), and/or set its resolution (dpi). One undo step.",
    effects: &[Effect::Edit],
    featured: true,
};

pub struct Resize;

impl Command for Resize {
    type Params = ResizeParams;
    type Output = Sized;
    fn meta(&self) -> &'static CommandMeta {
        &RESIZE
    }
    fn plan(&self, s: &mut Session<'_>, p: ResizeParams) -> Result<Plan<Sized>, CmdError> {
        let doc = s.doc_id(p.doc)?;
        let canvas = s.canvas_mut(Some(doc))?;
        let (w, h) = (canvas.width.max(1), canvas.height.max(1));
        let ratio = w as f64 / h as f64;
        let (nw, nh) = match (p.width_px, p.height_px) {
            (Some(nw), Some(nh)) => (nw, nh),
            (Some(nw), None) => (nw, ((f64::from(nw) / ratio).round() as u32).max(1)),
            (None, Some(nh)) => (((f64::from(nh) * ratio).round() as u32).max(1), nh),
            (None, None) => (w, h),
        };
        if nw > MAX_DIMENSION || nh > MAX_DIMENSION {
            return Err(CmdError::bad_param(
                "widthPx",
                &format!("a size that keeps both sides at most {MAX_DIMENSION} pixels"),
            ));
        }
        let dpi = p.dpi.unwrap_or(canvas.metadata.resolution_ppi);
        if (nw, nh) == (w, h) {
            // Only the print size changes: no pixel is touched.
            let cmd = iai_history::command::ResolutionCommand::new(dpi.max(1.0));
            canvas
                .execute(
                    Box::new(cmd),
                    iai_history::gateway::ChangeKind::DocumentMetadata,
                )
                .map_err(|e| CmdError::internal(e.message))?;
        } else if !canvas.resize_image(nw, nh, dpi) {
            return Err(not_enabled("the resize was refused", "Không đổi cỡ được"));
        }
        sized(s, doc).map(Plan::Done)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, JsonSchema)]
pub enum Angle {
    /// A quarter turn clockwise.
    #[serde(rename = "90")]
    Cw90,
    /// A quarter turn counter-clockwise.
    #[serde(rename = "-90")]
    Ccw90,
    #[serde(rename = "180")]
    Half,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct RotateParams {
    /// Document id (see `doc.list`); the document shown when omitted.
    pub doc: Option<u32>,
    /// "90" (clockwise), "-90" or "180".
    pub angle: Angle,
}
impl Validate for RotateParams {}

static ROTATE: CommandMeta = CommandMeta {
    id: "image.rotate",
    label_vi: "Xoay ảnh",
    label_en: "Rotate Image",
    doc: "Turn the whole page a quarter or half turn. No pixel is resampled.",
    effects: &[Effect::Edit],
    featured: false,
};

pub struct Rotate;

impl Command for Rotate {
    type Params = RotateParams;
    type Output = Sized;
    fn meta(&self) -> &'static CommandMeta {
        &ROTATE
    }
    fn plan(&self, s: &mut Session<'_>, p: RotateParams) -> Result<Plan<Sized>, CmdError> {
        let doc = s.doc_id(p.doc)?;
        let canvas = s.canvas_mut(Some(doc))?;
        match p.angle {
            Angle::Cw90 => canvas.rotate_90_cw(),
            Angle::Ccw90 => canvas.rotate_90_ccw(),
            Angle::Half => {
                canvas.begin_undo_group("Rotate 180");
                canvas.rotate_90_cw();
                canvas.rotate_90_cw();
                canvas.end_undo_group();
            }
        }
        sized(s, doc).map(Plan::Done)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Axis {
    /// Left and right swap.
    Horizontal,
    /// Top and bottom swap.
    Vertical,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct FlipParams {
    /// Document id (see `doc.list`); the document shown when omitted.
    pub doc: Option<u32>,
    pub axis: Axis,
}
impl Validate for FlipParams {}

static FLIP: CommandMeta = CommandMeta {
    id: "image.flip",
    label_vi: "Lật ảnh",
    label_en: "Flip Image",
    doc: "Mirror the whole page left to right or top to bottom.",
    effects: &[Effect::Edit],
    featured: false,
};

pub struct Flip;

impl Command for Flip {
    type Params = FlipParams;
    type Output = Sized;
    fn meta(&self) -> &'static CommandMeta {
        &FLIP
    }
    fn plan(&self, s: &mut Session<'_>, p: FlipParams) -> Result<Plan<Sized>, CmdError> {
        let doc = s.doc_id(p.doc)?;
        let canvas = s.canvas_mut(Some(doc))?;
        match p.axis {
            Axis::Horizontal => canvas.flip_horizontal(),
            Axis::Vertical => canvas.flip_vertical(),
        }
        sized(s, doc).map(Plan::Done)
    }
}
