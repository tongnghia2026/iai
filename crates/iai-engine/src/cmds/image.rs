//! The whole page: crop, straighten a card shot at a slant, resize, rotate,
//! flip.

use iai_canvas::canvas::MAX_DIMENSION;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::common::{RectPx, not_enabled};
use crate::{
    CmdError, Command, CommandMeta, Effect, EngineEvent, ErrorCode, Plan, Session, Validate,
};

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
    /// Instead of `rect`: a frame turned, with the picture moved under it,
    /// or a result of another size (the Crop tool's straighten and its crop
    /// to a print size).
    pub frame: Option<CropFrame>,
    /// Throw away what lies outside (default); false keeps it, hidden, so a
    /// later crop or move can bring it back.
    #[serde(default = "yes")]
    pub delete_cropped: bool,
    /// What the crop adds past the page is filled with this colour
    /// ("#RRGGBB"), as on a document with a Background; clear when left out.
    pub fill: Option<String>,
    /// Pixels per inch to record on the result (a crop to a print size).
    #[schemars(range(min = 1.0, max = 100000.0))]
    pub dpi: Option<f32>,
}

/// A crop frame of the Crop tool, in document pixels (fractions allowed).
#[derive(Debug, Clone, Copy, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct CropFrame {
    /// The frame's centre.
    pub x: f32,
    pub y: f32,
    /// Its width and height, at least one pixel each.
    pub w: f32,
    pub h: f32,
    /// How far the frame is turned, in degrees.
    #[serde(default)]
    pub angle: f64,
    /// How far the picture was moved under the frame.
    #[serde(default)]
    pub dx: f32,
    #[serde(default)]
    pub dy: f32,
    /// The result's size in pixels, the picture scaled to it; the frame's own
    /// size when left out.
    pub out_w: Option<u32>,
    pub out_h: Option<u32>,
}

fn yes() -> bool {
    true
}

impl Validate for CropParams {
    fn validate(&self) -> Result<(), CmdError> {
        if self.rect.is_some() && self.frame.is_some() {
            return Err(CmdError::bad_param("frame", "either `rect` or `frame`"));
        }
        if let Some(f) = &self.frame {
            let finite = [f.x, f.y, f.w, f.h, f.dx, f.dy]
                .iter()
                .all(|v| v.is_finite())
                && f.angle.is_finite();
            if !finite || f.w < 1.0 || f.h < 1.0 {
                return Err(CmdError::bad_param(
                    "frame",
                    "finite numbers, a width and a height of one pixel or more",
                ));
            }
        }
        Ok(())
    }
}

static CROP: CommandMeta = CommandMeta {
    id: "image.crop",
    label_vi: "Cắt ảnh",
    label_en: "Crop",
    doc: "Crop the page to a rectangle, or to the selection's bounds when `rect` is left out; \
          or to a `frame` turned by `angle` degrees and scaled to `outW` × `outH` (straightening, \
          a crop to a print size). Every layer is cropped; one undo step.",
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
        let fill = p
            .fill
            .as_deref()
            .map(|c| super::common::parse_color("fill", c))
            .transpose()?;
        let canvas = s.canvas_mut(Some(doc))?;
        if let Some(f) = p.frame {
            let out_w = f.out_w.unwrap_or(f.w.round() as u32);
            let out_h = f.out_h.unwrap_or(f.h.round() as u32);
            if out_w == 0 || out_h == 0 || out_w > MAX_DIMENSION || out_h > MAX_DIMENSION {
                return Err(CmdError::bad_param(
                    "frame",
                    &format!("a result of 1 to {MAX_DIMENSION} pixels a side"),
                ));
            }
            let angle = f.angle.to_radians() as f32;
            let done = match fill {
                Some(color) => canvas.crop_transformed_with_background(
                    f.x,
                    f.y,
                    f.w,
                    f.h,
                    out_w,
                    out_h,
                    f.dx,
                    f.dy,
                    angle,
                    p.delete_cropped,
                    color,
                ),
                None => canvas.crop_transformed(
                    f.x,
                    f.y,
                    f.w,
                    f.h,
                    out_w,
                    out_h,
                    f.dx,
                    f.dy,
                    angle,
                    p.delete_cropped,
                ),
            };
            if !done {
                return Err(not_enabled("the crop was refused", "Không cắt được"));
            }
            if let Some(dpi) = p.dpi {
                canvas.metadata.resolution_ppi = dpi.max(1.0);
            }
            return sized(s, doc).map(Plan::Done);
        }
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
        let done = match fill {
            Some(color) => {
                canvas.crop_with_background(rect.x, rect.y, rect.w, rect.h, p.delete_cropped, color)
            }
            None => canvas.crop(rect.x, rect.y, rect.w, rect.h, p.delete_cropped),
        };
        if !done {
            return Err(not_enabled("the crop was refused", "Không cắt được"));
        }
        if let Some(dpi) = p.dpi {
            canvas.metadata.resolution_ppi = dpi.max(1.0);
        }
        if p.rect.is_none() {
            canvas.deselect();
        }
        sized(s, doc).map(Plan::Done)
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct PerspectiveCropParams {
    /// Document id (see `doc.list`); the document shown when omitted.
    pub doc: Option<u32>,
    /// The card's four corners `[x, y]` in document pixels, in any order
    /// round it (e.g. read off `doc.preview` and scaled up); each side is
    /// then fitted to the edge near it. Left out, the card is looked for.
    #[schemars(length(min = 4, max = 4))]
    pub corners: Option<Vec<[f32; 2]>>,
    /// The flat card's width and height in millimetres, given together; an
    /// ID card lying down (85.6 × 53.98) when left out.
    #[schemars(range(min = 5.0, max = 2000.0))]
    pub width_mm: Option<f32>,
    #[schemars(range(min = 5.0, max = 2000.0))]
    pub height_mm: Option<f32>,
    /// Pixels per inch of the result.
    #[serde(default = "dpi_300")]
    #[schemars(range(min = 72, max = 1200))]
    pub dpi: u32,
}

fn dpi_300() -> u32 {
    300
}

/// An ID card's size, in millimetres.
pub const ID_CARD_MM: (f32, f32) = (85.6, 53.98);

impl Validate for PerspectiveCropParams {
    fn validate(&self) -> Result<(), CmdError> {
        if self.corners.as_ref().is_some_and(|c| c.len() != 4) {
            return Err(CmdError::bad_param("corners", "four [x, y] corners"));
        }
        if self.width_mm.is_some() != self.height_mm.is_some() {
            return Err(CmdError::bad_param(
                "widthMm",
                "widthMm and heightMm together",
            ));
        }
        Ok(())
    }
}

/// The page after a perspective crop, and the corners it was cut along.
#[derive(Debug, Clone, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Straightened {
    pub doc: u32,
    pub width_px: u32,
    pub height_px: u32,
    pub dpi: f32,
    /// Top-left, top-right, bottom-right, bottom-left, in the photo's pixels.
    pub corners: Vec<[f32; 2]>,
    /// When the card was looked for: how much what was found looks like
    /// one, 0 to 1 (below 0.3, check the result with `doc.preview`).
    pub found: Option<f32>,
}

static PERSPECTIVE_CROP: CommandMeta = CommandMeta {
    id: "image.perspective_crop",
    label_vi: "Cắt phối cảnh",
    label_en: "Perspective Crop",
    doc: "Cut a card (an ID card by default) or sheet photographed at a slant out of the page \
          and lay it flat, at its size in millimetres: its corners are found, or given and \
          fitted to the edges. The card's long sides come out on top and at the bottom; turn \
          it with `image.rotate` if it is upside down. One undo step.",
    effects: &[Effect::Edit],
    featured: true,
};

pub struct PerspectiveCrop;

impl Command for PerspectiveCrop {
    type Params = PerspectiveCropParams;
    type Output = Straightened;
    fn meta(&self) -> &'static CommandMeta {
        &PERSPECTIVE_CROP
    }
    fn plan(
        &self,
        s: &mut Session<'_>,
        p: PerspectiveCropParams,
    ) -> Result<Plan<Straightened>, CmdError> {
        use iai_imaging::card_find::{find_card, snap_corners};
        let doc = s.doc_id(p.doc)?;
        let (width_mm, height_mm) = match (p.width_mm, p.height_mm) {
            (Some(w), Some(h)) => (w, h),
            _ => ID_CARD_MM,
        };
        let aspect = width_mm.max(height_mm) / width_mm.min(height_mm);
        let px = |mm: f32| (mm / 25.4 * p.dpi as f32).round().max(1.0) as u32;
        let (out_w, out_h) = (px(width_mm), px(height_mm));
        if out_w > MAX_DIMENSION || out_h > MAX_DIMENSION {
            return Err(CmdError::bad_param(
                "dpi",
                &format!("a result of at most {MAX_DIMENSION} pixels a side"),
            ));
        }
        let canvas = s.canvas_mut(Some(doc))?;
        let (w, h) = (canvas.width, canvas.height);
        let rgba = canvas.flatten_for_export();
        let (corners, found) = match &p.corners {
            Some(given) => {
                let rough = [0, 1, 2, 3].map(|i| {
                    let c = given.get(i).copied().unwrap_or_default();
                    (c[0], c[1])
                });
                (snap_corners(&rgba, w, h, rough), None)
            }
            None => {
                let card = find_card(&rgba, w, h, aspect).ok_or_else(|| {
                    CmdError::new(
                        ErrorCode::NotFound,
                        "no card found in the photo: give its `corners`, read off `doc.preview`",
                    )
                    .vi("Không tìm thấy thẻ trong ảnh")
                })?;
                (card.corners, Some(card.score))
            }
        };
        // Long sides on top: a card asked for standing up starts at its
        // top-right corner.
        let quad = if width_mm >= height_mm {
            corners
        } else {
            [corners[1], corners[2], corners[3], corners[0]]
        };
        if !canvas.crop_perspective(quad, out_w, out_h, true) {
            return Err(not_enabled(
                "the corners make no card shape",
                "Bốn góc không tạo thành hình thẻ",
            ));
        }
        canvas.metadata.resolution_ppi = p.dpi as f32;
        let size = sized(s, doc)?;
        Ok(Plan::Done(Straightened {
            doc,
            width_px: size.width_px,
            height_px: size.height_px,
            dpi: size.dpi,
            corners: quad.iter().map(|&(x, y)| [x, y]).collect(),
            found,
        }))
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
        s.notice(
            format!("Image Size: {nw}×{nh} px"),
            format!("Image Size: {nw}×{nh} px"),
        );
        sized(s, doc).map(Plan::Done)
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct CanvasSizeParams {
    /// Document id (see `doc.list`); the document shown when omitted.
    pub doc: Option<u32>,
    /// The page's new width and height in pixels.
    #[schemars(range(min = 1, max = 30000))]
    pub width_px: u32,
    #[schemars(range(min = 1, max = 30000))]
    pub height_px: u32,
}

impl Validate for CanvasSizeParams {}

static CANVAS_SIZE: CommandMeta = CommandMeta {
    id: "image.canvas_size",
    label_vi: "Đổi khung ảnh",
    label_en: "Canvas Size",
    doc: "Make the page larger or smaller without resampling, as Image ▸ Canvas Size does: the \
          picture stays at the top-left corner, what is added is clear, what falls outside is \
          cut off. One undo step.",
    effects: &[Effect::Edit],
    featured: false,
};

pub struct CanvasSize;

impl Command for CanvasSize {
    type Params = CanvasSizeParams;
    type Output = Sized;
    fn meta(&self) -> &'static CommandMeta {
        &CANVAS_SIZE
    }
    fn plan(&self, s: &mut Session<'_>, p: CanvasSizeParams) -> Result<Plan<Sized>, CmdError> {
        let doc = s.doc_id(p.doc)?;
        let canvas = s.canvas_mut(Some(doc))?;
        if p.width_px > MAX_DIMENSION || p.height_px > MAX_DIMENSION {
            return Err(CmdError::bad_param(
                "widthPx",
                &format!("a size that keeps both sides at most {MAX_DIMENSION} pixels"),
            ));
        }
        if !canvas.resize(p.width_px, p.height_px) {
            return Err(not_enabled(
                "the canvas size was refused",
                "Không đổi được khung ảnh",
            ));
        }
        s.notice(
            format!("Canvas Size: {}×{} px", p.width_px, p.height_px),
            format!("Canvas Size: {}×{} px", p.width_px, p.height_px),
        );
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
