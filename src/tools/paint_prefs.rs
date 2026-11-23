//! The painting tools' options, kept in `prefs.json` from one session to the
//! next (Photoshop remembers its tool options): the Brush's tip, opacity,
//! flow, spacing, smoothing, mode and airbrush (the Pencil draws with the
//! Brush's), the Eraser's own tip, and Dodge's and Burn's.

use super::ToolManager;
use super::brush::{BRUSH_PRESETS, BrushMode, MAX_TIP_SIZE, PRESET_CUSTOM};
use super::brush_dynamics::BrushDynamics;
use super::dodge_burn::{DodgeBurnTool, ToneRange};
use serde::{Deserialize, Serialize};

/// The key under which `prefs.json` keeps them.
const KEY: &str = "paint_tools";

/// One tool's tip and paint amounts.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct TipPrefs {
    pub size: f32,
    pub hardness: f32,
    pub opacity: f32,
    pub flow: f32,
    pub spacing: f32,
    pub smoothing: f32,
    /// Index into `BRUSH_PRESETS`, `None` when hand-edited.
    pub preset: Option<usize>,
}

impl Default for TipPrefs {
    fn default() -> Self {
        Self {
            size: 20.0,
            hardness: 0.8,
            opacity: 1.0,
            flow: 1.0,
            spacing: 0.25,
            smoothing: 0.0,
            preset: None,
        }
    }
}

impl TipPrefs {
    /// Within the ranges the options bar allows (a hand-edited file cannot
    /// set a tip the tools refuse).
    fn sanitized(&self) -> Self {
        let share =
            |v: f32, lo: f32, hi: f32, d: f32| if v.is_finite() { v.clamp(lo, hi) } else { d };
        Self {
            size: share(self.size, 1.0, MAX_TIP_SIZE, 20.0),
            hardness: share(self.hardness, 0.0, 1.0, 0.8),
            opacity: share(self.opacity, 0.01, 1.0, 1.0),
            flow: share(self.flow, 0.01, 1.0, 1.0),
            spacing: share(self.spacing, 0.01, 2.0, 0.25),
            smoothing: share(self.smoothing, 0.0, 1.0, 0.0),
            preset: self.preset.filter(|&i| i < BRUSH_PRESETS.len()),
        }
    }
}

/// The Brush's dynamics, as saved.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct DynamicsPrefs {
    pub scatter: f32,
    pub size_jitter: f32,
    pub flow_jitter: f32,
    pub glow: f32,
}

impl DynamicsPrefs {
    fn of(d: BrushDynamics) -> Self {
        Self {
            scatter: d.scatter,
            size_jitter: d.size_jitter,
            flow_jitter: d.flow_jitter,
            glow: d.glow,
        }
    }

    fn dynamics(&self) -> BrushDynamics {
        BrushDynamics {
            scatter: self.scatter,
            size_jitter: self.size_jitter,
            flow_jitter: self.flow_jitter,
            glow: self.glow,
        }
        .sanitized()
    }
}

/// Dodge's or Burn's options.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ToneToolPrefs {
    pub size: f32,
    pub hardness: f32,
    pub exposure: f32,
    /// 0 Shadows, 1 Midtones, 2 Highlights.
    pub range: u8,
    pub protect_tones: bool,
    pub spacing: f32,
}

impl Default for ToneToolPrefs {
    fn default() -> Self {
        Self::of(&DodgeBurnTool::dodge())
    }
}

impl ToneToolPrefs {
    fn of(t: &DodgeBurnTool) -> Self {
        Self {
            size: t.size,
            hardness: t.hardness,
            exposure: t.exposure,
            range: t.range.to_u8(),
            protect_tones: t.protect_tones,
            spacing: t.spacing,
        }
    }

    /// Within the ranges the options bar allows.
    fn apply_to(&self, t: &mut DodgeBurnTool) {
        let d = Self::default();
        let share =
            |v: f32, lo: f32, hi: f32, d: f32| if v.is_finite() { v.clamp(lo, hi) } else { d };
        t.size = share(self.size, 1.0, MAX_TIP_SIZE, d.size);
        t.hardness = share(self.hardness, 0.0, 1.0, d.hardness);
        t.exposure = share(self.exposure, 0.01, 1.0, d.exposure);
        t.range = ToneRange::from_u8(self.range);
        t.protect_tones = self.protect_tones;
        t.spacing = share(self.spacing, 0.01, 2.0, d.spacing);
    }
}

/// Everything remembered.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PaintPrefs {
    pub brush: TipPrefs,
    /// `BrushMode::name()`.
    pub brush_mode: String,
    pub airbrush: bool,
    /// The Brush's scatter and jitter (absent from older files: off).
    pub brush_dynamics: DynamicsPrefs,
    pub eraser: TipPrefs,
    pub dodge: ToneToolPrefs,
    pub burn: ToneToolPrefs,
}

impl PaintPrefs {
    /// What the tools hold now.
    pub fn of(tools: &ToolManager) -> Self {
        let b = &tools.brush().settings;
        let e = tools.eraser();
        let preset = |i: usize| (i != PRESET_CUSTOM).then_some(i);
        Self {
            brush: TipPrefs {
                size: b.size,
                hardness: b.hardness,
                opacity: b.opacity,
                flow: b.flow,
                spacing: b.spacing,
                smoothing: b.smoothing,
                preset: preset(tools.brush().preset_idx),
            },
            brush_mode: b.mode.name().to_string(),
            airbrush: tools.brush().airbrush,
            brush_dynamics: DynamicsPrefs::of(tools.brush().dynamics),
            eraser: TipPrefs {
                size: e.size,
                hardness: e.hardness,
                opacity: e.opacity,
                flow: e.flow,
                spacing: e.spacing,
                smoothing: e.smoothing,
                preset: preset(e.preset_idx),
            },
            dodge: ToneToolPrefs::of(&tools.dodge),
            burn: ToneToolPrefs::of(&tools.burn),
        }
    }

    /// Give the tools these options.
    pub fn apply(&self, tools: &mut ToolManager) {
        let b = self.brush.sanitized();
        let brush = tools.brush_mut();
        brush.settings.size = b.size;
        brush.settings.hardness = b.hardness;
        brush.settings.opacity = b.opacity;
        brush.settings.flow = b.flow;
        brush.settings.spacing = b.spacing;
        brush.settings.smoothing = b.smoothing;
        brush.settings.mode = BrushMode::from_name(&self.brush_mode).unwrap_or_default();
        brush.preset_idx = b.preset.unwrap_or(PRESET_CUSTOM);
        brush.airbrush = self.airbrush;
        brush.dynamics = self.brush_dynamics.dynamics();
        let e = self.eraser.sanitized();
        let eraser = tools.eraser_mut();
        eraser.size = e.size;
        eraser.hardness = e.hardness;
        eraser.opacity = e.opacity;
        eraser.flow = e.flow;
        eraser.spacing = e.spacing;
        eraser.smoothing = e.smoothing;
        eraser.preset_idx = e.preset.unwrap_or(PRESET_CUSTOM);
        self.dodge.apply_to(&mut tools.dodge);
        self.burn.apply_to(&mut tools.burn);
    }

    /// The options saved last time, if any (none in tests: the prefs store
    /// reads nothing there).
    pub fn load() -> Option<Self> {
        let value = crate::core::prefs_store::read().remove(KEY)?;
        serde_json::from_value(value).ok()
    }

    pub fn save(&self) {
        if let Ok(value) = serde_json::to_value(self) {
            crate::core::prefs_store::update(|prefs| {
                prefs.insert(KEY.to_string(), value);
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_options_come_back_as_they_were() {
        let mut tools = ToolManager::new();
        {
            let b = tools.brush_mut();
            b.settings.size = 340.0;
            b.settings.hardness = 0.25;
            b.settings.opacity = 0.4;
            b.settings.mode = BrushMode::Blend(crate::core::blend::BlendMode::Multiply);
            b.airbrush = true;
            b.preset_idx = 1;
            b.dynamics.scatter = 1.5;
            b.dynamics.flow_jitter = 0.3;
            b.dynamics.glow = 0.6;
        }
        tools.eraser_mut().size = 77.0;
        tools.burn.exposure = 0.12;
        tools.burn.range = ToneRange::Highlights;
        tools.dodge.protect_tones = false;
        let saved = PaintPrefs::of(&tools);
        let json = serde_json::to_value(&saved).unwrap();
        let back: PaintPrefs = serde_json::from_value(json).unwrap();
        let mut fresh = ToolManager::new();
        back.apply(&mut fresh);
        assert_eq!(PaintPrefs::of(&fresh), saved);
        assert_eq!(fresh.brush().settings.mode.name(), "Multiply");
        assert_eq!(fresh.brush().dynamics.scatter, 1.5);
        assert_eq!(fresh.brush().dynamics.flow_jitter, 0.3);
        assert_eq!(fresh.brush().dynamics.glow, 0.6);
        assert_eq!(fresh.burn.range, ToneRange::Highlights);
        assert!(!fresh.dodge.protect_tones && fresh.burn.protect_tones);
    }

    #[test]
    fn a_hand_edited_file_cannot_set_a_tip_the_tools_refuse() {
        let json = serde_json::json!({
            "brush": { "size": 1.0e9, "hardness": -3.0, "preset": 99 },
            "brush_mode": "Sparkly",
            "brush_dynamics": { "scatter": 99.0, "size_jitter": -1.0 },
        });
        let prefs: PaintPrefs = serde_json::from_value(json).unwrap();
        let mut tools = ToolManager::new();
        prefs.apply(&mut tools);
        let b = tools.brush();
        assert_eq!(b.settings.size, MAX_TIP_SIZE);
        assert_eq!(b.settings.hardness, 0.0);
        assert_eq!(b.preset_idx, PRESET_CUSTOM);
        assert_eq!(b.settings.mode, BrushMode::Normal);
        assert_eq!(
            b.dynamics.scatter,
            crate::tools::brush_dynamics::MAX_SCATTER
        );
        assert_eq!(b.dynamics.size_jitter, 0.0);
    }
}
