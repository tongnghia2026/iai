//! Free Transform as a command (`layer.free_transform`): the session Ctrl+T
//! starts, set to the scale, turn, move or corners asked for, and committed
//! the way the window's tool commits it, with the same pixels.

use iai_canvas::free_transform::{self, NoSession, TransformMode};
use iai_geom::geometry::InterpolationMode;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::common::not_enabled;
use crate::{
    Apply, CmdError, Command, CommandMeta, Effect, EngineEvent, ErrorCode, Job, Plan, Session,
    Validate,
};

/// How pixels are read where they land between source pixels.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Resample {
    /// Smooth (the default).
    #[default]
    Bilinear,
    /// Hard pixel edges, for pixel art and masks.
    Nearest,
}

impl Resample {
    fn mode(self) -> InterpolationMode {
        match self {
            Resample::Bilinear => InterpolationMode::Bilinear,
            Resample::Nearest => InterpolationMode::NearestNeighbor,
        }
    }
}

fn one() -> f32 {
    1.0
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct FreeTransformParams {
    /// Document id (see `doc.list`); the document shown when omitted.
    pub doc: Option<u32>,
    /// The layers to transform (a group takes its members along); the picked
    /// layers, or the active one, when omitted.
    pub layers: Option<Vec<u32>>,
    /// Width factor (1 = as it is, negative mirrors it).
    #[serde(default = "one")]
    #[schemars(range(min = -100.0, max = 100.0))]
    pub scale_x: f32,
    /// Height factor.
    #[serde(default = "one")]
    #[schemars(range(min = -100.0, max = 100.0))]
    pub scale_y: f32,
    /// Turn, in degrees clockwise.
    #[serde(default)]
    #[schemars(range(min = -3600.0, max = 3600.0))]
    pub angle: f32,
    /// Move, in pixels.
    #[serde(default)]
    #[schemars(range(min = -1000000.0, max = 1000000.0))]
    pub dx: f32,
    #[serde(default)]
    #[schemars(range(min = -1000000.0, max = 1000000.0))]
    pub dy: f32,
    /// The point it scales and turns about, [x, y] in page pixels; the box's
    /// middle when omitted.
    pub pivot: Option<[f32; 2]>,
    /// Where the box's corners go, top-left, top-right, bottom-right,
    /// bottom-left, each [x, y]: a distort or a perspective. Scale, angle and
    /// move are then not used.
    pub corners: Option<[[f32; 2]; 4]>,
    /// The box, [x0, y0, x1, y1] in page pixels; what the layers hold when
    /// omitted.
    pub bounds: Option<[f32; 4]>,
    #[serde(default)]
    pub resample: Resample,
}

impl Validate for FreeTransformParams {
    fn validate(&self) -> Result<(), CmdError> {
        let finite = |v: f32| v.is_finite();
        let mut all = vec![self.scale_x, self.scale_y, self.angle, self.dx, self.dy];
        all.extend(self.pivot.iter().flatten());
        all.extend(self.corners.iter().flatten().flatten());
        all.extend(self.bounds.iter().flatten());
        if !all.into_iter().all(finite) {
            return Err(CmdError::bad_param("scaleX", "finite numbers"));
        }
        if self.scale_x.abs() < free_transform::MIN_LIVE_SCALE
            || self.scale_y.abs() < free_transform::MIN_LIVE_SCALE
        {
            return Err(CmdError::bad_param(
                "scaleX",
                "a factor no smaller than 0.02 either way",
            ));
        }
        if let Some([x0, y0, x1, y1]) = self.bounds
            && (x1 <= x0 || y1 <= y0)
        {
            return Err(CmdError::bad_param("bounds", "x1 > x0 and y1 > y0"));
        }
        Ok(())
    }
}

#[derive(Debug, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Transformed {
    pub doc: u32,
    /// The layers transformed.
    pub layers: Vec<u32>,
    /// Whether anything changed.
    pub changed: bool,
    /// A type layer became pixels: the gesture (a skew, a distort of turned
    /// type, a size past the font's) cannot be written as type.
    pub rasterized_text: bool,
}

static FREE_TRANSFORM: CommandMeta = CommandMeta {
    id: "layer.free_transform",
    label_vi: "Free Transform",
    label_en: "Free Transform",
    doc: "Free Transform (Ctrl+T) the picked layers, or `layers`: scale (scaleX/scaleY, \
          negative to mirror), turn (angle, degrees) and move (dx/dy) about `pivot`, or send \
          the box's four `corners` somewhere for a distort or a perspective (not with a path \
          among them). Paths, type and shapes stay editable where the gesture allows, masks \
          follow, and a picture transformed again is resampled from its first pixels. Not for \
          the Background or locked layers. One undo step.",
    effects: &[Effect::Edit],
    featured: false,
};

pub struct FreeTransform;

impl Command for FreeTransform {
    type Params = FreeTransformParams;
    type Output = Transformed;
    fn meta(&self) -> &'static CommandMeta {
        &FREE_TRANSFORM
    }
    fn plan(
        &self,
        s: &mut Session<'_>,
        p: FreeTransformParams,
    ) -> Result<Plan<Transformed>, CmdError> {
        let doc = s.doc_id(p.doc)?;
        // Refused where layer commands are (a PDF opened page by page).
        s.canvas(Some(doc))?;
        let d = s.document_mut(Some(doc))?;
        let bounds = p.bounds.map(|[x0, y0, x1, y1]| ((x0, y0), (x1, y1)));
        let mut ts = free_transform::session(
            &d.canvas,
            &mut d.transform_lineage,
            p.layers.as_deref(),
            bounds,
        )
        .map_err(|why| match why {
            NoSession::NothingToTransform => not_enabled(
                "no layer to transform: pick an unlocked layer that is not the Background",
                "Không có layer nào để biến đổi (chọn layer không khóa, không phải nền)",
            ),
        })?;
        if p.corners.is_some() && ts.has_path() {
            return Err(not_enabled(
                "a path stays a path only under scale, turn and move: rasterize it for a distort",
                "Đường path chỉ giữ được nét khi phóng / thu / xoay / dời; rasterize trước nếu cần \
                 kéo méo",
            ));
        }
        ts.scale_x = p.scale_x;
        ts.scale_y = p.scale_y;
        ts.angle_deg = p.angle;
        ts.translate_x = p.dx;
        ts.translate_y = p.dy;
        if let Some([x, y]) = p.pivot {
            ts.pivot_cx = x;
            ts.pivot_cy = y;
        }
        if let Some(corners) = p.corners {
            ts.quad = Some(corners.map(|[x, y]| (x, y)));
            ts.mode = TransformMode::Distort;
        }
        let layers: Vec<u32> = ts.layer_states.iter().map(|ls| ls.layer_id).collect();
        if let Some(changed) = free_transform::move_only(&mut d.canvas, &ts) {
            if changed {
                d.canvas.layer_revision = d.canvas.layer_revision.saturating_add(1);
                d.canvas.flatten_full();
                s.emit(EngineEvent::Structure { doc });
                s.notice("Đã dời layer", "Layers moved");
            } else {
                s.notice("Không có gì thay đổi", "Nothing changed");
            }
            return Ok(Plan::Done(Transformed {
                doc,
                layers,
                changed,
                rasterized_text: false,
            }));
        }
        let doc_id = d.id;
        let interpolation = p.resample.mode();
        Ok(Plan::Job(Job {
            doc: Some(doc),
            label_vi: "Free Transform".into(),
            work: Box::new(move |cx| {
                cx.progress(0.1, "Đang biến đổi…");
                let result = free_transform::bake_transform_commit(doc_id, ts, interpolation)
                    .map_err(|e| CmdError::new(ErrorCode::TooLarge, e.clone()).vi(e))?;
                cx.check_cancelled()?;
                Ok(Box::new(move |s: &mut Session<'_>| {
                    let rasterized_text = result.rasterized_text;
                    let d = s.document_mut(Some(doc))?;
                    free_transform::land(&mut d.canvas, &mut d.transform_lineage, result);
                    d.canvas.refresh_clip_masks();
                    d.canvas.layer_revision = d.canvas.layer_revision.saturating_add(1);
                    d.canvas.flatten_full();
                    s.emit(EngineEvent::Structure { doc });
                    if rasterized_text {
                        s.notice(
                            "Đã biến đổi — chữ đã chuyển thành ảnh (không sửa lại được bằng Type)",
                            "Transformed; the type became pixels",
                        );
                    } else {
                        s.notice("Đã biến đổi", "Transformed");
                    }
                    Ok(Transformed {
                        doc,
                        layers,
                        changed: true,
                        rasterized_text,
                    })
                }) as Apply<Transformed>)
            }),
        }))
    }
}
