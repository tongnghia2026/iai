//! Tone, colour and filters on a layer, as the Image ▸ Adjustments, Filter
//! and "Làm sạch bản scan" menus apply them: one undo step each.

use iai_canvas::canvas::Canvas;
use iai_doc::layer::{AdjustmentType, LevelsParams};
use iai_imaging::filters::FilterType;
use iai_imaging::scan_cleanup::{ScanCleanupMode, ScanCleanupParams, clean_scan_rgba};
use iai_raster::tile::TileMap;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::common::{not_enabled, parse_color};
use crate::{
    Apply, CmdError, Command, CommandMeta, Effect, EngineEvent, ErrorCode, Job, Plan, Session,
    Validate, layer_index,
};

/// What a change of a layer's pixels answers.
#[derive(Debug, Clone, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Adjusted {
    pub doc: u32,
    pub layer: u32,
    /// The name of the step in the history.
    pub step: String,
}

/// The layer `layer` (the active one when none) of `canvas`, if its pixels
/// may be changed: a raster layer, unlocked (a Background may be locked).
fn pixel_layer(canvas: &Canvas, layer: Option<u32>) -> Result<usize, CmdError> {
    let idx = layer_index(canvas, layer)?;
    let l = canvas
        .layer_stack
        .layers
        .get(idx)
        .ok_or_else(|| CmdError::internal("layer index"))?;
    if !l.is_raster() || (l.locked && !l.is_background) {
        return Err(not_enabled(
            "the layer is not a raster layer, or it is locked",
            "Layer này không phải layer ảnh, hoặc đang khóa",
        ));
    }
    Ok(idx)
}

fn no_cmyk(canvas: &Canvas) -> Result<(), CmdError> {
    if canvas.is_cmyk() {
        return Err(not_enabled(
            "not in a CMYK document",
            "Chưa làm được ở chế độ CMYK",
        ));
    }
    Ok(())
}

/// `layer`'s pixels changed from `before` to `after` in one step named `step`,
/// unless they changed meanwhile.
fn commit(
    s: &mut Session<'_>,
    doc: u32,
    layer: u32,
    before: TileMap,
    after: TileMap,
    step: &str,
) -> Result<Adjusted, CmdError> {
    let canvas = s.canvas_mut(Some(doc))?;
    let idx = layer_index(canvas, Some(layer))?;
    let same = canvas
        .layer_stack
        .layers
        .get(idx)
        .is_some_and(|l| l.tiles.same_tiles(&before));
    if !same {
        return Err(CmdError::new(
            ErrorCode::StaleRevision,
            "the layer changed while it was being worked on; run it again",
        )
        .vi("Layer đã đổi trong lúc xử lý — hãy chạy lại"));
    }
    if !canvas.commit_layer_tiles_change(layer, before, after, step) {
        return Err(CmdError::internal("the change could not be committed"));
    }
    s.emit(EngineEvent::Pixels { doc });
    s.notice(step.to_string(), step.to_string());
    Ok(Adjusted {
        doc,
        layer,
        step: step.to_string(),
    })
}

fn zero() -> f32 {
    0.0
}

fn one() -> f32 {
    1.0
}

fn full() -> u8 {
    255
}

/// Hue/Saturation's Colorize: every pixel in one hue.
#[derive(Debug, Clone, Copy, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Colorize {
    /// 0 to 360 degrees.
    #[schemars(range(min = 0.0, max = 360.0))]
    pub hue: f32,
    /// 0 to 100 (25 when left out, as Photoshop starts).
    #[serde(default = "colorize_saturation")]
    #[schemars(range(min = 0.0, max = 100.0))]
    pub saturation: f32,
    /// -100 (black) to 100 (white).
    #[serde(default = "zero")]
    #[schemars(range(min = -100.0, max = 100.0))]
    pub lightness: f32,
}

fn colorize_saturation() -> f32 {
    25.0
}

/// One colour's own Hue/Saturation sliders.
#[derive(Debug, Clone, Copy, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct HueShift {
    #[serde(default = "zero")]
    #[schemars(range(min = -180.0, max = 180.0))]
    pub hue: f32,
    #[serde(default = "zero")]
    #[schemars(range(min = -100.0, max = 100.0))]
    pub saturation: f32,
    #[serde(default = "zero")]
    #[schemars(range(min = -100.0, max = 100.0))]
    pub lightness: f32,
}

/// The colours Hue/Saturation moves on their own.
#[derive(Debug, Clone, Copy, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct HueRanges {
    #[serde(default)]
    pub reds: HueShift,
    #[serde(default)]
    pub yellows: HueShift,
    #[serde(default)]
    pub greens: HueShift,
    #[serde(default)]
    pub cyans: HueShift,
    #[serde(default)]
    pub blues: HueShift,
    #[serde(default)]
    pub magentas: HueShift,
}

impl HueRanges {
    fn sliders(&self) -> [[f32; 3]; 6] {
        [
            self.reds,
            self.yellows,
            self.greens,
            self.cyans,
            self.blues,
            self.magentas,
        ]
        .map(|s| {
            [
                s.hue.clamp(-180.0, 180.0),
                s.saturation.clamp(-100.0, 100.0),
                s.lightness.clamp(-100.0, 100.0),
            ]
        })
    }
}

/// An adjustment, as the dialogs of Image ▸ Adjustments set it.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Adjustment {
    /// Brightness and contrast, -100 to 100 each.
    BrightnessContrast {
        #[serde(default = "zero")]
        #[schemars(range(min = -100.0, max = 100.0))]
        brightness: f32,
        #[serde(default = "zero")]
        #[schemars(range(min = -100.0, max = 100.0))]
        contrast: f32,
    },
    /// Hue -180 to 180 degrees; saturation and lightness -100 to 100.
    HueSaturation {
        #[serde(default = "zero")]
        #[schemars(range(min = -180.0, max = 180.0))]
        hue: f32,
        #[serde(default = "zero")]
        #[schemars(range(min = -100.0, max = 100.0))]
        saturation: f32,
        #[serde(default = "zero")]
        #[schemars(range(min = -100.0, max = 100.0))]
        lightness: f32,
        /// Tint the whole picture one colour (Photoshop's Colorize); the
        /// three sliders above are not used then.
        colorize: Option<Colorize>,
        /// The pixels of one colour moved on their own, on top of the three
        /// sliders above.
        ranges: Option<HueRanges>,
    },
    /// Levels of all channels together: the input black and white points
    /// (0 to 255), the midtones (gamma, 0.1 to 9.99; above 1 lighter) and the
    /// output black and white points.
    #[serde(rename_all = "camelCase")]
    Levels {
        #[serde(default)]
        in_black: u8,
        #[serde(default = "full")]
        in_white: u8,
        #[serde(default = "one")]
        #[schemars(range(min = 0.1, max = 9.99))]
        gamma: f32,
        #[serde(default)]
        out_black: u8,
        #[serde(default = "full")]
        out_white: u8,
    },
    /// A curve over all channels through `points` `[in, out]` (0 to 255),
    /// two at least; e.g. `[[0,0],[128,150],[255,255]]` lightens the mids.
    Curves {
        #[schemars(length(min = 2, max = 16))]
        points: Vec<[f32; 2]>,
    },
    /// -100 to 100 each.
    Vibrance {
        #[serde(default = "zero")]
        #[schemars(range(min = -100.0, max = 100.0))]
        vibrance: f32,
        #[serde(default = "zero")]
        #[schemars(range(min = -100.0, max = 100.0))]
        saturation: f32,
    },
    /// Exposure in stops (-20 to 20), offset (-0.5 to 0.5), gamma (0.01 to 9.99).
    Exposure {
        #[serde(default = "zero")]
        #[schemars(range(min = -20.0, max = 20.0))]
        exposure: f32,
        #[serde(default = "zero")]
        #[schemars(range(min = -0.5, max = 0.5))]
        offset: f32,
        #[serde(default = "one")]
        #[schemars(range(min = 0.01, max = 9.99))]
        gamma: f32,
    },
    /// Each tone range's push towards red (cyan below 0), green (magenta),
    /// blue (yellow), -100 to 100.
    #[serde(rename_all = "camelCase")]
    ColorBalance {
        #[serde(default)]
        shadows: [f32; 3],
        #[serde(default)]
        midtones: [f32; 3],
        #[serde(default)]
        highlights: [f32; 3],
        #[serde(default = "yes")]
        preserve_luminosity: bool,
    },
    Invert,
    Desaturate,
    /// Black or white by brightness, the border at `value` (1 to 255).
    Threshold {
        #[serde(default = "mid")]
        #[schemars(range(min = 1, max = 255))]
        value: u8,
    },
    /// `levels` tones a channel (2 to 255).
    Posterize {
        #[serde(default = "four")]
        #[schemars(range(min = 2, max = 255))]
        levels: u8,
    },
    /// Grey, each colour range made lighter or darker (Photoshop's mix when
    /// left out; -200 to 300).
    BlackAndWhite {
        #[serde(default = "forty")]
        reds: f32,
        #[serde(default = "sixty")]
        yellows: f32,
        #[serde(default = "forty")]
        greens: f32,
        #[serde(default = "sixty")]
        cyans: f32,
        #[serde(default = "twenty")]
        blues: f32,
        #[serde(default = "eighty")]
        magentas: f32,
    },
    /// A coloured filter in front of the lens: `color` "#RRGGBB" (warm
    /// orange when left out), `density` 0 to 1.
    PhotoFilter {
        color: Option<String>,
        #[serde(default = "quarter")]
        #[schemars(range(min = 0.0, max = 1.0))]
        density: f32,
        #[serde(default = "yes")]
        luminosity: bool,
    },
}

fn yes() -> bool {
    true
}
fn mid() -> u8 {
    128
}
fn four() -> u8 {
    4
}
fn twenty() -> f32 {
    20.0
}
fn forty() -> f32 {
    40.0
}
fn sixty() -> f32 {
    60.0
}
fn eighty() -> f32 {
    80.0
}
fn quarter() -> f32 {
    0.25
}

impl Adjustment {
    pub(super) fn core(&self) -> Result<AdjustmentType, CmdError> {
        Ok(match self.clone() {
            Adjustment::BrightnessContrast {
                brightness,
                contrast,
            } => AdjustmentType::BrightnessContrast {
                brightness: brightness.clamp(-100.0, 100.0),
                contrast: contrast.clamp(-100.0, 100.0),
            },
            Adjustment::HueSaturation {
                hue,
                saturation,
                lightness,
                colorize,
                ranges,
            } => AdjustmentType::HueSaturation {
                hue: hue.clamp(-180.0, 180.0),
                saturation: saturation.clamp(-100.0, 100.0),
                lightness: lightness.clamp(-100.0, 100.0),
                colorize: colorize.map(|c| {
                    [
                        c.hue.rem_euclid(360.0),
                        c.saturation.clamp(0.0, 100.0),
                        c.lightness.clamp(-100.0, 100.0),
                    ]
                }),
                ranges: ranges.map(|r| r.sliders()).unwrap_or([[0.0; 3]; 6]),
            },
            Adjustment::Levels {
                in_black,
                in_white,
                gamma,
                out_black,
                out_white,
            } => {
                if in_black >= in_white {
                    return Err(CmdError::bad_param("inBlack", "below inWhite"));
                }
                let master = LevelsParams {
                    in_black,
                    in_white,
                    gamma: gamma.clamp(0.1, 9.99),
                    out_black,
                    out_white,
                };
                AdjustmentType::Levels {
                    channels: [
                        master,
                        LevelsParams::default(),
                        LevelsParams::default(),
                        LevelsParams::default(),
                    ],
                }
            }
            Adjustment::Curves { points } => {
                let mut master: Vec<(f32, f32)> = points
                    .iter()
                    .map(|p| {
                        (
                            (p[0] / 255.0).clamp(0.0, 1.0),
                            (p[1] / 255.0).clamp(0.0, 1.0),
                        )
                    })
                    .collect();
                master.sort_by(|a, b| a.0.total_cmp(&b.0));
                master.dedup_by(|a, b| (a.0 - b.0).abs() < 1e-4);
                if master.len() < 2 {
                    return Err(CmdError::bad_param("points", "two points at least, apart"));
                }
                let straight = vec![(0.0, 0.0), (1.0, 1.0)];
                AdjustmentType::Curves {
                    channels: [master, straight.clone(), straight.clone(), straight],
                }
            }
            Adjustment::Vibrance {
                vibrance,
                saturation,
            } => AdjustmentType::Vibrance {
                vibrance: vibrance.clamp(-100.0, 100.0),
                saturation: saturation.clamp(-100.0, 100.0),
            },
            Adjustment::Exposure {
                exposure,
                offset,
                gamma,
            } => AdjustmentType::Exposure {
                exposure: exposure.clamp(-20.0, 20.0),
                offset: offset.clamp(-0.5, 0.5),
                gamma: gamma.clamp(0.01, 9.99),
            },
            Adjustment::ColorBalance {
                shadows,
                midtones,
                highlights,
                preserve_luminosity,
            } => {
                let c = |v: [f32; 3]| v.map(|x| x.clamp(-100.0, 100.0));
                AdjustmentType::ColorBalance {
                    shadows: c(shadows),
                    midtones: c(midtones),
                    highlights: c(highlights),
                    preserve_luminosity,
                }
            }
            Adjustment::Invert => AdjustmentType::Invert,
            Adjustment::Desaturate => AdjustmentType::Desaturate,
            Adjustment::Threshold { value } => AdjustmentType::Threshold {
                value: value.max(1),
            },
            Adjustment::Posterize { levels } => AdjustmentType::Posterize {
                levels: levels.max(2),
            },
            Adjustment::BlackAndWhite {
                reds,
                yellows,
                greens,
                cyans,
                blues,
                magentas,
            } => {
                let c = |v: f32| v.clamp(-200.0, 300.0);
                AdjustmentType::BlackAndWhite {
                    r: c(reds),
                    y: c(yellows),
                    g: c(greens),
                    c: c(cyans),
                    b: c(blues),
                    m: c(magentas),
                }
            }
            Adjustment::PhotoFilter {
                color,
                density,
                luminosity,
            } => {
                let color = match color {
                    Some(text) => {
                        let [r, g, b, _] = parse_color("color", &text)?;
                        [r, g, b]
                    }
                    None => [236, 138, 0],
                };
                AdjustmentType::PhotoFilter {
                    color,
                    density: density.clamp(0.0, 1.0),
                    luminosity,
                }
            }
        })
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct AdjustParams {
    /// Document id (see `doc.list`); the document shown when omitted.
    pub doc: Option<u32>,
    /// Layer id (see `doc.inspect`); the active layer when omitted. It
    /// becomes the active layer.
    pub layer: Option<u32>,
    pub adjustment: Adjustment,
}

impl Validate for AdjustParams {
    fn validate(&self) -> Result<(), CmdError> {
        self.adjustment.core().map(|_| ())
    }
}

static ADJUST: CommandMeta = CommandMeta {
    id: "image.adjust",
    label_vi: "Chỉnh màu / sáng",
    label_en: "Adjust",
    doc: "Change a layer's tone or colour, as Image ▸ Adjustments does: brightness_contrast, \
          hue_saturation, levels, curves, vibrance, exposure, color_balance, invert, desaturate, \
          threshold, posterize, black_and_white, photo_filter (`adjustment.kind`). Applied to \
          the pixels, one undo step. For an automatic fix of a dull or tinted photo use \
          `image.auto_levels`.",
    effects: &[Effect::Edit],
    featured: true,
};

pub struct Adjust;

impl Command for Adjust {
    type Params = AdjustParams;
    type Output = Adjusted;

    fn meta(&self) -> &'static CommandMeta {
        &ADJUST
    }

    fn plan(&self, s: &mut Session<'_>, p: AdjustParams) -> Result<Plan<Adjusted>, CmdError> {
        let doc = s.doc_id(p.doc)?;
        let adjustment = p.adjustment.core()?;
        let step = adjustment.name().to_string();
        let canvas = s.canvas_mut(Some(doc))?;
        let idx = pixel_layer(canvas, p.layer)?;
        if canvas.is_cmyk() && !adjustment.is_ink_native() {
            return Err(not_enabled(
                "only levels and curves in a CMYK document",
                "Ở chế độ CMYK chỉ chỉnh được Levels và Curves",
            ));
        }
        canvas.layer_stack.active_idx = idx;
        let layer = canvas
            .layer_stack
            .layers
            .get(idx)
            .map(|l| l.id)
            .ok_or_else(|| CmdError::internal("layer index"))?;
        if !canvas.apply_adjustment_to_active_layer(adjustment) {
            return Err(not_enabled(
                "the adjustment could not be applied",
                "Không chỉnh được layer này",
            ));
        }
        s.emit(EngineEvent::Pixels { doc });
        s.notice(step.clone(), step.clone());
        Ok(Plan::Done(Adjusted { doc, layer, step }))
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct AutoLevelsParams {
    /// Document id (see `doc.list`); the document shown when omitted.
    pub doc: Option<u32>,
    /// Layer id (see `doc.inspect`); the active layer when omitted.
    pub layer: Option<u32>,
    /// Stretch only the brightness and keep the colours as they are; by
    /// default red, green and blue are each stretched, which also takes a
    /// colour cast away.
    #[serde(default)]
    pub monochromatic: bool,
    /// With the channels stretched: also make the midtones neutral, so a grey
    /// comes out grey (Image ▸ Auto Color, Shift+Ctrl+B).
    #[serde(default)]
    pub neutral_midtones: bool,
    /// The share of the darkest and the lightest pixels let go at each end,
    /// in percent (0 to 10).
    #[serde(default = "tenth")]
    #[schemars(range(min = 0.0, max = 10.0))]
    pub clip_percent: f32,
}

fn tenth() -> f32 {
    0.1
}

impl Validate for AutoLevelsParams {}

static AUTO_LEVELS: CommandMeta = CommandMeta {
    id: "image.auto_levels",
    label_vi: "Tự cân sáng (Auto Levels)",
    label_en: "Auto Levels",
    doc: "Stretch a layer's tones end to end as Image ▸ Auto Levels (Ctrl+Shift+L) does: a dull, \
          dark, washed-out or tinted photo (a card or paper shot with a phone) comes out clear. \
          One undo step.",
    effects: &[Effect::Edit],
    featured: true,
};

pub struct AutoLevels;

impl Command for AutoLevels {
    type Params = AutoLevelsParams;
    type Output = Adjusted;

    fn meta(&self) -> &'static CommandMeta {
        &AUTO_LEVELS
    }

    fn plan(&self, s: &mut Session<'_>, p: AutoLevelsParams) -> Result<Plan<Adjusted>, CmdError> {
        let doc = s.doc_id(p.doc)?;
        let canvas = s.canvas_mut(Some(doc))?;
        let idx = pixel_layer(canvas, p.layer)?;
        no_cmyk(canvas)?;
        let pixels = canvas
            .layer_stack
            .layers
            .get(idx)
            .map(|l| l.flatten_tiles())
            .ok_or_else(|| CmdError::internal("layer index"))?;
        let histograms = iai_imaging::levels::histograms(&pixels);
        let mut channels =
            iai_imaging::levels::auto_levels(&histograms, p.monochromatic, p.clip_percent)
                .ok_or_else(|| {
                    not_enabled(
                        "the layer has no tonal range to stretch",
                        "Ảnh không có dải sáng tối để kéo giãn",
                    )
                })?;
        if p.neutral_midtones && !p.monochromatic {
            iai_imaging::levels::neutral_midtones(&histograms, &mut channels);
        }
        canvas.layer_stack.active_idx = idx;
        let layer = canvas
            .layer_stack
            .layers
            .get(idx)
            .map(|l| l.id)
            .ok_or_else(|| CmdError::internal("layer index"))?;
        if !canvas.apply_adjustment_to_active_layer(AdjustmentType::Levels { channels }) {
            return Err(not_enabled(
                "Auto Levels could not be applied",
                "Không tự cân sáng được layer này",
            ));
        }
        s.emit(EngineEvent::Pixels { doc });
        let step = if p.neutral_midtones && !p.monochromatic {
            s.notice("Đã tự cân màu (Auto Color)", "Auto Color");
            "Auto Color"
        } else {
            s.notice("Đã tự cân sáng (Auto Levels)", "Auto Levels");
            "Auto Levels"
        };
        Ok(Plan::Done(Adjusted {
            doc,
            layer,
            step: step.to_string(),
        }))
    }
}

/// A filter, as the Filter menu sets it.
#[derive(Debug, Clone, Copy, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Filter {
    /// Soften: `radius` 0.1 to 100 pixels.
    GaussianBlur {
        #[schemars(range(min = 0.1, max = 100.0))]
        radius: f32,
    },
    /// Crisper edges (unsharp mask): `amount` 0 to 5 (1 = 100 %), `radius`
    /// 0.1 to 50 pixels.
    Sharpen {
        #[serde(default = "one")]
        #[schemars(range(min = 0.0, max = 5.0))]
        amount: f32,
        #[serde(default = "two")]
        #[schemars(range(min = 0.1, max = 50.0))]
        radius: f32,
    },
    /// The detail only, on grey: `radius` 0.1 to 250.
    HighPass {
        #[schemars(range(min = 0.1, max = 250.0))]
        radius: f32,
    },
    /// Grain: `amount` 0 to 100.
    AddNoise {
        #[schemars(range(min = 0.0, max = 100.0))]
        amount: f32,
        #[serde(default)]
        monochromatic: bool,
    },
    /// Square blocks of `cell` pixels (2 to 200).
    Pixelate {
        #[schemars(range(min = 2.0, max = 200.0))]
        cell: f32,
    },
    /// Smooth away grain: `strength` 0 to 100.
    ReduceNoise {
        #[serde(default = "fifty")]
        #[schemars(range(min = 0.0, max = 100.0))]
        strength: f32,
    },
}

fn two() -> f32 {
    2.0
}

fn fifty() -> f32 {
    50.0
}

impl Filter {
    fn core(self) -> FilterType {
        match self {
            Filter::GaussianBlur { radius } => FilterType::GaussianBlur {
                radius: radius.clamp(0.1, 100.0),
            },
            Filter::Sharpen { amount, radius } => FilterType::Sharpen {
                amount: amount.clamp(0.0, 5.0),
                radius: radius.clamp(0.1, 50.0),
            },
            Filter::HighPass { radius } => FilterType::HighPass {
                radius: radius.clamp(0.1, 250.0),
            },
            Filter::AddNoise {
                amount,
                monochromatic,
            } => FilterType::AddNoise {
                amount: amount.clamp(0.0, 100.0),
                monochromatic,
            },
            Filter::Pixelate { cell } => FilterType::Pixelate {
                cell: cell.clamp(2.0, 200.0),
            },
            Filter::ReduceNoise { strength } => FilterType::ReduceNoise {
                strength: strength.clamp(0.0, 100.0),
            },
        }
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct FilterParams {
    /// Document id (see `doc.list`); the document shown when omitted.
    pub doc: Option<u32>,
    /// Layer id (see `doc.inspect`); the active layer when omitted.
    pub layer: Option<u32>,
    pub filter: Filter,
}

impl Validate for FilterParams {}

static FILTER: CommandMeta = CommandMeta {
    id: "image.filter",
    label_vi: "Bộ lọc",
    label_en: "Filter",
    doc: "Run a filter of the Filter menu on a layer: gaussian_blur, sharpen, high_pass, \
          add_noise, pixelate, reduce_noise (`filter.kind`). Only inside the selection when \
          there is one. One undo step.",
    effects: &[Effect::Edit],
    featured: true,
};

pub struct ApplyFilter;

impl Command for ApplyFilter {
    type Params = FilterParams;
    type Output = Adjusted;

    fn meta(&self) -> &'static CommandMeta {
        &FILTER
    }

    fn plan(&self, s: &mut Session<'_>, p: FilterParams) -> Result<Plan<Adjusted>, CmdError> {
        let doc = s.doc_id(p.doc)?;
        let canvas = s.canvas(Some(doc))?;
        let idx = pixel_layer(canvas, p.layer)?;
        no_cmyk(canvas)?;
        let layer = canvas
            .layer_stack
            .layers
            .get(idx)
            .ok_or_else(|| CmdError::internal("layer index"))?;
        let (layer_id, lw, lh, (ox, oy)) = (layer.id, layer.width, layer.height, layer.offset);
        let before = layer.tiles.clone();
        let selection = canvas.selection.clone();
        let (cw, ch) = (canvas.width, canvas.height);
        let filter = p.filter.core();
        Ok(Plan::Job(Job {
            doc: Some(doc),
            label_vi: filter.name().into(),
            work: Box::new(move |cx| {
                cx.progress(0.1, filter.name());
                let after = Canvas::build_filtered_layer_tiles_with_selection(
                    &before, lw, lh, ox, oy, &filter, &selection, cw, ch,
                );
                cx.check_cancelled()?;
                Ok(Box::new(move |s: &mut Session<'_>| {
                    commit(s, doc, layer_id, before, after, filter.name())
                }) as Apply<Adjusted>)
            }),
        }))
    }
}

/// What "Làm sạch bản scan" gives.
#[derive(Debug, Default, Clone, Copy, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ScanLook {
    /// Clean grey: white paper, black writing, shadows and stains gone.
    #[default]
    Grayscale,
    /// Pure black on white, for the cleanest print of text.
    Bilevel,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ScanParams {
    /// Document id (see `doc.list`); the document shown when omitted.
    pub doc: Option<u32>,
    /// Layer id (see `doc.inspect`); the active layer when omitted.
    pub layer: Option<u32>,
    #[serde(default)]
    pub look: ScanLook,
    /// 0 (unchanged) to 1 (full effect).
    #[serde(default = "one")]
    #[schemars(range(min = 0.0, max = 1.0))]
    pub strength: f32,
}

impl Validate for ScanParams {}

static SCAN_CLEANUP: CommandMeta = CommandMeta {
    id: "image.scan_cleanup",
    label_vi: "Làm sạch bản scan",
    label_en: "Clean Up Scan",
    doc: "Make a photographed or scanned page look like a clean scan, as \"Làm sạch bản scan\" \
          does: the paper turns white, uneven light, shadows and colour casts go, the writing \
          stays black (`look` grayscale), or pure black on white (bilevel). For documents and \
          papers, not for photos of people. One undo step.",
    effects: &[Effect::Edit],
    featured: true,
};

pub struct ScanCleanup;

impl Command for ScanCleanup {
    type Params = ScanParams;
    type Output = Adjusted;

    fn meta(&self) -> &'static CommandMeta {
        &SCAN_CLEANUP
    }

    fn plan(&self, s: &mut Session<'_>, p: ScanParams) -> Result<Plan<Adjusted>, CmdError> {
        let doc = s.doc_id(p.doc)?;
        let canvas = s.canvas(Some(doc))?;
        let idx = pixel_layer(canvas, p.layer)?;
        no_cmyk(canvas)?;
        let layer = canvas
            .layer_stack
            .layers
            .get(idx)
            .ok_or_else(|| CmdError::internal("layer index"))?;
        let (layer_id, lw, lh) = (layer.id, layer.width, layer.height);
        let before = layer.tiles.clone();
        let params = ScanCleanupParams {
            mode: match p.look {
                ScanLook::Grayscale => ScanCleanupMode::Grayscale,
                ScanLook::Bilevel => ScanCleanupMode::Bilevel,
            },
            strength: p.strength.clamp(0.0, 1.0),
        };
        const STEP: &str = "Làm sạch bản scan";
        Ok(Plan::Job(Job {
            doc: Some(doc),
            label_vi: STEP.into(),
            work: Box::new(move |cx| {
                cx.progress(0.1, "Đang làm sạch bản scan…");
                let cleaned = clean_scan_rgba(&before.flatten(), lw, lh, params);
                cx.check_cancelled()?;
                let after = TileMap::from_rgba(&cleaned, lw, lh);
                Ok(Box::new(move |s: &mut Session<'_>| {
                    commit(s, doc, layer_id, before, after, STEP)
                }) as Apply<Adjusted>)
            }),
        }))
    }
}
