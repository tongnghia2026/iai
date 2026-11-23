use super::brush_dynamics::{BrushDynamics, DabRng};
use super::{PointerEvent, Tool, ToolCtx, ToolResponse};
use crate::core::blend::{BlendMode, blend_pixel};
use crate::core::canvas::Canvas;

/// How a stroke's paint meets the layer (Photoshop's brush Mode): a layer
/// blend mode applied as the paint goes down, or Behind (paint only under
/// what is there) / Clear (paint transparency).
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub enum BrushMode {
    #[default]
    Normal,
    Behind,
    Clear,
    Blend(BlendMode),
}

impl BrushMode {
    /// The modes in Photoshop's order, for the options bar.
    pub fn all() -> Vec<BrushMode> {
        let mut out = vec![BrushMode::Normal, BrushMode::Behind, BrushMode::Clear];
        out.extend(
            BlendMode::all()
                .iter()
                .copied()
                .filter(|m| !matches!(m, BlendMode::Normal | BlendMode::Dissolve))
                .map(BrushMode::Blend),
        );
        out
    }

    pub fn name(self) -> &'static str {
        match self {
            BrushMode::Normal => "Normal",
            BrushMode::Behind => "Behind",
            BrushMode::Clear => "Clear",
            BrushMode::Blend(m) => m.name(),
        }
    }

    /// The mode a saved name stands for (`name()` round trip).
    pub fn from_name(name: &str) -> Option<BrushMode> {
        BrushMode::all().into_iter().find(|m| m.name() == name)
    }
}

#[derive(Debug, Clone)]
pub struct BrushSettings {
    pub size: f32,
    pub hardness: f32,
    pub opacity: f32,
    pub color: [u8; 4],
    pub spacing: f32,
    pub is_eraser: bool,
    /// Stroke smoothing strength: 0.0 = off, 1.0 = max (EMA factor).
    pub smoothing: f32,
    /// Per-dab paint amount (standard raster editors "Flow"): 0.0..1.0. Lower values build up
    /// gradually as overlapping dabs accumulate within a stroke (airbrush feel).
    pub flow: f32,
    /// Pencil tip: whole pixels, no anti-aliased edge, centred on the pixel
    /// grid (Photoshop's Pencil).
    pub aliased: bool,
    /// How the paint meets the layer. Layer pixels only: a mask, a saved
    /// channel, a single colour channel or a CMYK plate is painted Normal.
    pub mode: BrushMode,
}

/// A named brush preset — a bundle of tip parameters selectable from the
/// right-click brush popup or the options-bar dropdown (like the standard
/// brush picker). Size / opacity / colour stay under the user's control.
#[derive(Clone, Debug)]
pub struct BrushPreset {
    pub name: &'static str,
    pub hardness: f32,
    pub spacing: f32,
    pub flow: f32,
}

/// Built-in basic brushes. Soft round tips use a full radial-gradient falloff;
/// hard round keeps a crisp anti-aliased edge.
pub const BRUSH_PRESETS: &[BrushPreset] = &[
    BrushPreset {
        name: "Soft Round",
        hardness: 0.0,
        spacing: 0.10,
        flow: 0.65,
    },
    BrushPreset {
        name: "Hard Round",
        hardness: 1.0,
        spacing: 0.10,
        flow: 1.0,
    },
    BrushPreset {
        name: "Medium Round",
        hardness: 0.5,
        spacing: 0.12,
        flow: 1.0,
    },
    BrushPreset {
        name: "Soft Low Flow",
        hardness: 0.0,
        spacing: 0.05,
        flow: 0.30,
    },
    BrushPreset {
        name: "Hard Edge Sketch",
        hardness: 0.9,
        spacing: 0.05,
        flow: 1.0,
    },
    BrushPreset {
        name: "Airbrush Soft",
        hardness: 0.0,
        spacing: 0.04,
        flow: 0.12,
    },
];

/// Sentinel for "no preset selected" (custom settings).
pub const PRESET_CUSTOM: usize = usize::MAX;

/// A 0..1 channel value to a byte, rounded half up: `(v·255).round()`
/// without the library call (`as u8` saturates below 0 and above 255). Four
/// of these per pixel of every dab: the call was a large share of a dab.
#[inline(always)]
pub(crate) fn q8(v: f32) -> u8 {
    (v * 255.0 + 0.5) as u8
}

/// Largest tip a painting tool takes, in px (the options bar's ceiling).
pub const MAX_TIP_SIZE: f32 = 5000.0;

/// Photoshop's computed round tip, measured from Photoshop 2020's own dabs
/// (`tests/fixtures/brush_pts`, plan `KE_HOACH_AUDIT_CO_VE_BRUSH_2026-11-16`
/// §10). The tip covers fully out to a core radius, then falls off as the
/// bell `10^-(u²)`, `u = (d − core) / width`:
///
/// - hardness 0 is the bell alone, its 10 % point on the nominal radius, so
///   a soft dab reaches ~1.65× its radius before rounding to nothing;
/// - harder tips push the core out and narrow the bell (`core_of`,
///   `width_of`: fits within 0.004 radius of Photoshop's at every hardness);
/// - the bell is never narrower than [`EDGE_PX`], Photoshop's anti-aliased
///   edge (a hard tip's edge is ~2 px wide at any size); widening it keeps
///   the 50 % contour in place;
/// - tips under 6 px lose hardness the way Photoshop's do (1–3 px tips paint
///   the same hard or soft).
#[derive(Clone, Copy, Debug)]
pub(crate) struct RoundTip {
    core: f32,
    inv_width: f32,
    /// Distance where the tip rounds to nothing (alpha < 0.5/255).
    reach: f32,
    /// Pencil: full cover inside the radius, nothing outside, no edge.
    aliased: bool,
    radius: f32,
}

/// The bell's `u` where it falls under half a level of 255.
const BELL_END: f32 = 1.6466; // sqrt(log10(510))
/// Photoshop's anti-aliased edge: the narrowest the bell gets, px.
const EDGE_PX: f32 = 2.1;
/// The bell's `u` at 50 %: sqrt(log10(2)).
const BELL_HALF: f32 = 0.5487;

impl RoundTip {
    pub(crate) fn new(radius: f32, hardness: f32) -> Self {
        let r = radius.max(0.0);
        // Under 6 px Photoshop's tips soften: 1–3 px are the same hard or
        // soft, a 4 px hard tip paints like 40 % hardness.
        let h = hardness.clamp(0.0, 1.0) * ((r * 2.0 - 3.0) * 0.4).clamp(0.0, 1.0);
        let core_frac = 1.0 - (1.0 - h) * (1.0 - 0.639 * h);
        let width_frac = (1.0 - h) * (1.0 - 0.385 * h);
        let (core_px, width_px) = (core_frac * r, width_frac * r);
        let width = (width_px * width_px + EDGE_PX * EDGE_PX).sqrt();
        let core = core_px + BELL_HALF * (width_px - width) - 0.2;
        Self {
            core,
            inv_width: 1.0 / width,
            reach: (core + BELL_END * width).max(0.0),
            aliased: false,
            radius: r,
        }
    }

    /// The Pencil's tip: every pixel whose centre is within the radius.
    pub(crate) fn aliased(radius: f32) -> Self {
        let r = radius.max(0.5);
        Self {
            core: r,
            inv_width: 0.0,
            reach: r + 1e-3,
            aliased: true,
            radius: r,
        }
    }

    /// How far from the centre the tip paints anything, px.
    #[inline]
    pub(crate) fn reach(&self) -> f32 {
        self.reach
    }

    /// Cover at distance `d` (px) from the centre.
    #[inline]
    pub(crate) fn alpha_at(&self, d: f32) -> f32 {
        if self.aliased {
            return if d <= self.radius + 1e-3 { 1.0 } else { 0.0 };
        }
        if d <= self.core {
            return 1.0;
        }
        let u = (d - self.core) * self.inv_width;
        if u >= BELL_END {
            0.0
        } else {
            (-std::f32::consts::LN_10 * u * u).exp()
        }
    }

    /// Cover at squared distance `d2`.
    #[inline]
    pub(crate) fn alpha(&self, d2: f32) -> f32 {
        self.alpha_at(d2.max(0.0).sqrt())
    }

    /// Cover as a table over squared distance, `n` entries from 0 to
    /// `reach²`, sized so neighbouring entries sit well under a pixel apart
    /// at the edge (a coarse table stair-steps a big hard tip's rim).
    pub(crate) fn table(&self) -> (Vec<f32>, f32) {
        let reach2 = (self.reach * self.reach).max(1e-6);
        let n = ((self.reach * 6.0).ceil() as usize).clamp(256, 32768);
        let scale = (n - 1) as f32 / reach2;
        let lut = (0..n)
            .map(|k| self.alpha(k as f32 / scale))
            .collect::<Vec<f32>>();
        (lut, scale)
    }
}

/// Cover of a round tip of `radius` and `hardness` at squared distance
/// `dist2` from its centre (see [`RoundTip`]); nothing past
/// [`tip_reach`].
#[inline]
pub(crate) fn soft_round_alpha(dist2: f32, radius: f32, hardness: f32) -> f32 {
    if radius <= 0.0 {
        return 0.0;
    }
    RoundTip::new(radius, hardness).alpha(dist2)
}

/// How far from its centre a round tip of `radius` and `hardness` paints:
/// past its radius for soft tips (a soft dab's bell runs on to ~1.65 r).
#[inline]
pub(crate) fn tip_reach(radius: f32, hardness: f32) -> f32 {
    RoundTip::new(radius, hardness).reach()
}

#[inline]
fn smoothstep(edge0: f32, edge1: f32, x: f32) -> f32 {
    let t = ((x - edge0) / (edge1 - edge0).max(0.0001)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Quintic smootherstep on [0,1]: zero 1st AND 2nd derivative at both ends.
#[inline]
fn smootherstep01(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    t * t * t * (t * (t * 6.0 - 15.0) + 10.0)
}

/// The round tip iAi drew before it was measured against Photoshop: zero at
/// the radius. Only healing (Repair, Clone's heal modes) still uses it: its
/// membrane stops at the radius. Clone and Smudge paint [`RoundTip`] since
/// 20/11.
#[inline]
pub(crate) fn legacy_round_alpha(dist2: f32, radius: f32, hardness: f32) -> f32 {
    if radius <= 0.0 {
        return 0.0;
    }

    let t = (dist2.sqrt() / radius).clamp(0.0, 1.0);
    let hardness = hardness.clamp(0.0, 1.0);
    if hardness >= 0.999 {
        // ~1px anti-aliasing band in absolute pixels: the old relative
        // 0.985..1 band collapsed to a fraction of a pixel on small tips
        // (jagged edges) and grew to several pixels on huge ones.
        let band = (1.0 / radius).min(1.0);
        return 1.0 - smoothstep(1.0 - band, 1.0, t);
    }

    let core = (hardness * 0.92).clamp(0.0, 0.92);
    if t <= core {
        1.0
    } else {
        // C2 bell falloff from the solid core to the rim: flat at both ends,
        // so there is no cone tip in the centre and no visible ring where
        // the dab meets the canvas.
        let ft = ((t - core) / (1.0 - core)).clamp(0.0, 1.0);
        1.0 - smootherstep01(ft)
    }
}

/// Fraction of the tip radius where [`soft_round_alpha`] falls to 50 % — the
/// ring Photoshop's "Normal Brush Tip" cursor draws. 0.55 for a fully soft
/// tip, ~1 for a hard one.
pub(crate) fn soft_round_half_radius(hardness: f32) -> f32 {
    half_radius_of(soft_round_alpha, hardness)
}

/// [`soft_round_half_radius`] of [`legacy_round_alpha`] (the tools still on
/// the old tip): 0.5 for a fully soft tip.
pub(crate) fn legacy_half_radius(hardness: f32) -> f32 {
    half_radius_of(legacy_round_alpha, hardness)
}

fn half_radius_of(alpha: fn(f32, f32, f32) -> f32, hardness: f32) -> f32 {
    const R: f32 = 1000.0;
    let (mut lo, mut hi) = (0.0_f32, 1.0_f32);
    for _ in 0..24 {
        let mid = 0.5 * (lo + hi);
        let d = mid * R;
        if alpha(d * d, R, hardness) > 0.5 {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    0.5 * (lo + hi)
}

impl BrushSettings {
    /// Distance between dab centres along the stroke path, in canvas px.
    /// Never under a pixel: Photoshop stops there (measured: a 50 px tip at
    /// 1 % spacing paints as at 1 px, not 0.5 px, which builds up 11 % more).
    #[inline]
    pub fn dab_spacing(&self) -> f32 {
        (self.size * self.spacing).max(1.0)
    }

    /// Radius of the tip, px, within the tools' bounds.
    #[inline]
    pub(crate) fn tip_radius(&self) -> f32 {
        (self.size * 0.5).clamp(0.5, MAX_TIP_SIZE * 0.5)
    }

    /// The tip these settings paint with.
    pub(crate) fn tip(&self) -> RoundTip {
        if self.aliased {
            RoundTip::aliased(self.tip_radius())
        } else {
            RoundTip::new(self.tip_radius(), self.hardness)
        }
    }

    /// Where a dab aimed at `(cx, cy)` lands: a Pencil dab sits on the pixel
    /// grid (an odd size on a pixel's centre, an even one on its corner), so
    /// its whole pixels come out the same wherever the pointer is.
    #[inline]
    pub(crate) fn dab_centre(&self, cx: f32, cy: f32) -> (f32, f32) {
        if !self.aliased {
            return (cx, cy);
        }
        if (self.size.round() as i64).rem_euclid(2) == 1 {
            (cx.floor() + 0.5, cy.floor() + 0.5)
        } else {
            (cx.round(), cy.round())
        }
    }
}

impl Default for BrushSettings {
    fn default() -> Self {
        Self {
            size: 20.0,
            hardness: 0.8,
            opacity: 1.0,
            color: [0, 0, 0, 255],
            spacing: 0.25,
            is_eraser: false,
            smoothing: 0.0,
            flow: 1.0,
            aliased: false,
            mode: BrushMode::Normal,
        }
    }
}

/// Per-stroke paint state implementing the PS opacity/flow model: each dab
/// accumulates FLOW into this coverage buffer, and the stroke is composited
/// over the layer as it was at stroke start with OPACITY as a single ceiling.
/// Overlapping dabs inside one stroke therefore never exceed the stroke
/// opacity, while separate strokes still stack on each other.
pub struct StrokeBuffer {
    /// Distance to the next dab along the stroke path (walker state).
    pub residual: f32,
    /// Whether this stroke paints the layer mask (captured at stroke start —
    /// the target can't change mid-stroke).
    pub(crate) paint_mask: bool,
    /// Target tiles (layer pixels or mask) as they were when the stroke began.
    /// Arc clones: costs memory only for tiles the stroke actually touches.
    pub(crate) base: crate::core::tile::TileMap,
    /// Accumulated flow coverage per layer-local pixel, tile-keyed.
    pub(crate) cov: std::collections::HashMap<crate::core::tile::TilePos, Box<[f32]>>,
}

impl StrokeBuffer {
    /// Snapshot the active layer's paint target. Returns None when the layer
    /// has no paintable target (e.g. mask targeted but no mask attached), or
    /// is locked whole (Photoshop paints nothing there; the Background's
    /// lock only pins its position).
    pub fn begin(canvas: &Canvas) -> Option<Self> {
        let idx = canvas.layer_stack.active_idx;
        let layer = canvas.layer_stack.layers.get(idx)?;
        if layer.locked && !layer.is_background {
            return None;
        }
        let paint_mask = layer.paint_target == crate::core::layer::PaintTarget::Mask;
        let base = if paint_mask {
            layer.mask.as_ref()?.tiles.clone()
        } else {
            layer.tiles.clone()
        };
        Some(Self {
            residual: 0.0,
            paint_mask,
            base,
            cov: std::collections::HashMap::new(),
        })
    }
}

/// Photoshop-style stroke smoothing. The paint hangs behind the pointer on
/// a string whose length grows with Smoothing (in screen pixels, so it feels
/// the same at any zoom): it only moves once the pointer pulls the string
/// taut. That depends on where the pointer goes, not on how often the mouse
/// reports, so the stroke comes out the same on any mouse or frame rate (the
/// old per-event average did not). When the pointer rests the paint catches
/// up with it ("Stroke Catch-up"), and on release the stroke is finished to
/// the release point.
#[derive(Clone, Debug, Default)]
pub(crate) struct Smoother {
    /// Where the paint is.
    at: (f32, f32),
    /// Where the pointer is.
    pointer: (f32, f32),
    /// When the pointer last moved.
    moved: Option<std::time::Instant>,
    /// When the last catch-up step ran.
    tick: Option<std::time::Instant>,
}

/// String length at 100 % Smoothing, screen px.
const STRING_SCREEN_PX: f32 = 80.0;
/// How long the pointer must rest before the paint catches up.
const REST_BEFORE_CATCH_UP: std::time::Duration = std::time::Duration::from_millis(60);
/// Time constant of the catch-up, seconds.
const CATCH_UP_SECS: f32 = 0.08;

impl Smoother {
    pub(crate) fn begin(&mut self, x: f32, y: f32) {
        *self = Self {
            at: (x, y),
            pointer: (x, y),
            moved: Some(std::time::Instant::now()),
            tick: None,
        };
    }

    /// The pointer moved to `(x, y)`: the stretch the paint travels.
    pub(crate) fn follow(
        &mut self,
        x: f32,
        y: f32,
        smoothing: f32,
        zoom: f32,
    ) -> ((f32, f32), (f32, f32)) {
        let from = self.at;
        self.pointer = (x, y);
        self.moved = Some(std::time::Instant::now());
        self.tick = None;
        let s = smoothing.clamp(0.0, 1.0);
        if s <= 0.001 {
            self.at = (x, y);
            return (from, self.at);
        }
        let string = s * STRING_SCREEN_PX / zoom.max(1e-3);
        let (dx, dy) = (x - from.0, y - from.1);
        let d = (dx * dx + dy * dy).sqrt();
        if d > string {
            let k = (d - string) / d;
            self.at = (from.0 + dx * k, from.1 + dy * k);
        }
        (from, self.at)
    }

    /// Where the paint is now.
    pub(crate) fn at(&self) -> (f32, f32) {
        self.at
    }

    /// Whether the paint still trails the pointer.
    pub(crate) fn pending(&self, smoothing: f32) -> bool {
        let (dx, dy) = (self.pointer.0 - self.at.0, self.pointer.1 - self.at.1);
        smoothing > 0.001 && dx * dx + dy * dy > 0.0625
    }

    /// A catch-up step for this frame, once the pointer has rested: the
    /// stretch to paint.
    pub(crate) fn catch_up(&mut self, smoothing: f32) -> Option<((f32, f32), (f32, f32))> {
        if !self.pending(smoothing) {
            return None;
        }
        let now = std::time::Instant::now();
        let rested_at = self.moved? + REST_BEFORE_CATCH_UP;
        if now < rested_at {
            return None;
        }
        let since = self.tick.unwrap_or(rested_at);
        self.tick = Some(now);
        let dt = now.duration_since(since).as_secs_f32().max(0.004);
        let f = 1.0 - (-dt / CATCH_UP_SECS).exp();
        let from = self.at;
        let (dx, dy) = (self.pointer.0 - from.0, self.pointer.1 - from.1);
        self.at = if (dx * dx + dy * dy) * (1.0 - f) * (1.0 - f) < 0.25 {
            self.pointer
        } else {
            (from.0 + dx * f, from.1 + dy * f)
        };
        Some((from, self.at))
    }

    /// The pointer let go at `(x, y)`: the last stretch, if any.
    pub(crate) fn finish(&mut self, x: f32, y: f32) -> Option<((f32, f32), (f32, f32))> {
        let from = self.at;
        self.pointer = (x, y);
        self.at = (x, y);
        let (dx, dy) = (x - from.0, y - from.1);
        (dx * dx + dy * dy > 1e-6).then_some((from, (x, y)))
    }
}

/// Photoshop's Shift strokes: Shift+click paints a straight line from where
/// the tool's last stroke ended (in the same document), and a stroke begun
/// with Shift held keeps to the horizontal or the vertical, whichever way it
/// set off.
#[derive(Clone, Debug, Default)]
pub(crate) struct ShiftLines {
    /// Where the last stroke ended, and in which document.
    last_end: Option<(crate::core::document::DocumentId, (f32, f32))>,
    /// Where a Shift stroke began: its drag keeps to one axis through it.
    from: Option<(f32, f32)>,
    /// The axis, once the drag has gone far enough to tell.
    horizontal: Option<bool>,
}

impl ShiftLines {
    /// A press at `(x, y)`: with Shift, where its straight line starts.
    pub(crate) fn press(
        &mut self,
        doc: crate::core::document::DocumentId,
        x: f32,
        y: f32,
        shift: bool,
    ) -> Option<(f32, f32)> {
        self.horizontal = None;
        self.from = shift.then_some((x, y));
        if !shift {
            return None;
        }
        self.last_end.filter(|(d, _)| *d == doc).map(|(_, p)| p)
    }

    /// A drag point, held to one axis on a Shift stroke.
    pub(crate) fn constrain(&mut self, x: f32, y: f32) -> (f32, f32) {
        let Some((x0, y0)) = self.from else {
            return (x, y);
        };
        let horizontal = match self.horizontal {
            Some(h) => h,
            None => {
                let (dx, dy) = (x - x0, y - y0);
                if dx.abs().max(dy.abs()) < 3.0 {
                    return (x0, y0);
                }
                let h = dx.abs() >= dy.abs();
                self.horizontal = Some(h);
                h
            }
        };
        if horizontal { (x, y0) } else { (x0, y) }
    }

    /// The stroke ended at `(x, y)`.
    pub(crate) fn release(&mut self, doc: crate::core::document::DocumentId, x: f32, y: f32) {
        self.last_end = Some((doc, (x, y)));
        self.from = None;
        self.horizontal = None;
    }
}

/// Airbrush rate: dabs a second laid at the paint's point while the pointer
/// is down, moving or not, so holding still builds up to the Opacity.
const AIRBRUSH_DABS_PER_SEC: f32 = 25.0;

pub struct BrushTool {
    pub settings: BrushSettings,
    /// Index into [`BRUSH_PRESETS`] of the active preset, or [`PRESET_CUSTOM`]
    /// when the tip params were edited by hand.
    pub preset_idx: usize,
    /// Where the paint is, behind the pointer when smoothing.
    smoother: Smoother,
    /// Distance left until the next dab on a saved alpha channel, carried
    /// across drag events (see [`Self::paint_cpu_stroke_segment`]).
    stroke_residual: f32,
    /// Per-stroke coverage buffer (PS opacity/flow model); Some while the
    /// pointer is down.
    stroke: Option<StrokeBuffer>,
    /// Shift+click lines and Shift-drag axis.
    lines: ShiftLines,
    /// Airbrush (the options bar's toggle).
    pub airbrush: bool,
    /// When the airbrush last laid paint, and the part of a dab it owes.
    air_clock: Option<std::time::Instant>,
    air_debt: f32,
    /// Scatter and jitter (the options bar's Dynamics).
    pub dynamics: BrushDynamics,
    /// Draws the dynamics' random numbers, seeded per stroke.
    rng: DabRng,
    /// Strokes painted, mixed into each stroke's seed.
    strokes: u64,
}

impl Default for BrushTool {
    fn default() -> Self {
        Self::new()
    }
}

impl BrushTool {
    pub fn new() -> Self {
        Self {
            settings: BrushSettings::default(),
            preset_idx: PRESET_CUSTOM,
            smoother: Smoother::default(),
            stroke_residual: 0.0,
            stroke: None,
            lines: ShiftLines::default(),
            airbrush: false,
            air_clock: None,
            air_debt: 0.0,
            dynamics: BrushDynamics::default(),
            rng: DabRng::new(0),
            strokes: 0,
        }
    }

    /// Apply a built-in preset's tip parameters (hardness / spacing / flow).
    /// Size, opacity and colour are intentionally preserved.
    pub fn apply_preset(&mut self, idx: usize) {
        if let Some(p) = BRUSH_PRESETS.get(idx) {
            self.settings.hardness = p.hardness;
            self.settings.spacing = p.spacing;
            self.settings.flow = p.flow;
            self.preset_idx = idx;
        }
    }

    /// Direct (unbuffered) dab: alpha = opacity × flow per dab, dabs composite
    /// straight onto the layer. Used by tools without a per-stroke opacity
    /// model (pencil) and by one-shot stamps.
    pub fn paint_cpu_dab(settings: &BrushSettings, canvas: &mut Canvas, cx: f32, cy: f32) {
        Self::paint_cpu_dab_impl(settings, canvas, cx, cy, None)
    }

    /// Buffered dab (PS model): flow accumulates in `stroke`'s coverage buffer
    /// and the result is composited from the stroke-start snapshot with the
    /// stroke opacity as ceiling. See [`StrokeBuffer`].
    pub fn paint_cpu_dab_stroked(
        settings: &BrushSettings,
        canvas: &mut Canvas,
        cx: f32,
        cy: f32,
        stroke: &mut StrokeBuffer,
    ) {
        Self::paint_cpu_dab_impl(settings, canvas, cx, cy, Some(stroke))
    }

    fn paint_cpu_dab_impl(
        settings: &BrushSettings,
        canvas: &mut Canvas,
        cx: f32,
        cy: f32,
        stroke: Option<&mut StrokeBuffer>,
    ) {
        if Self::paint_alpha_plane_dab(settings, canvas, cx, cy) {
            return;
        }

        // CMYK document: blend the colour's ink into the active layer's ink
        // planes (direct dab), then re-project the mirror. Skipped when the
        // stroke targets a grayscale layer mask (masks carry no ink). The
        // buffered/direct RGB machinery below is bypassed entirely.
        if canvas.is_cmyk() {
            let layer_idx = canvas.layer_stack.active_idx;
            let painting_mask = match &stroke {
                Some(s) => s.paint_mask,
                None => {
                    canvas.layer_stack.layers[layer_idx].paint_target
                        == crate::core::layer::PaintTarget::Mask
                }
            };
            if !painting_mask {
                Self::paint_ink_dab(settings, canvas, cx, cy);
                return;
            }
        }

        let tip = settings.tip();
        let (cx, cy) = settings.dab_centre(cx, cy);
        let r = tip.reach();
        let r2 = r * r;
        let opacity = settings.opacity.clamp(0.0, 1.0);
        let [br, bg, bb, ba] = settings.color;
        let ba_f = ba as f32 / 255.0;

        let layer_idx = canvas.layer_stack.active_idx;
        let ox = canvas.layer_stack.layers[layer_idx].offset.0;
        let oy = canvas.layer_stack.layers[layer_idx].offset.1;
        let lw = canvas.layer_stack.layers[layer_idx].width;
        let lh = canvas.layer_stack.layers[layer_idx].height;

        let lcx = cx - ox as f32;
        let lcy = cy - oy as f32;

        let lx0 = ((lcx - r).floor().max(0.0) as u32).min(lw);
        let ly0 = ((lcy - r).floor().max(0.0) as u32).min(lh);
        let lx1 = ((lcx + r).ceil() as u32).min(lw);
        let ly1 = ((lcy + r).ceil() as u32).min(lh);

        if lx1 <= lx0 || ly1 <= ly0 {
            return;
        }

        let buffered = stroke.is_some();
        // Buffered: opacity is a per-stroke ceiling applied at composite time,
        // so only flow feeds the per-dab accumulation. Direct: legacy per-dab
        // alpha = opacity × flow.
        let max_src_a = if buffered {
            ba_f * settings.flow.clamp(0.0, 1.0)
        } else {
            ba_f * opacity * settings.flow.clamp(0.0, 1.0)
        };
        if max_src_a < 0.001 || (buffered && opacity < 0.001) {
            return;
        }
        let src_r = br as f32 / 255.0;
        let src_g = bg as f32 / 255.0;
        let src_b = bb as f32 / 255.0;
        let mask_target = (src_r * 0.2126 + src_g * 0.7152 + src_b * 0.0722).clamp(0.0, 1.0);

        let (falloff_lut, lut_scale) = tip.table();

        let tile_size = crate::core::tile::TILE_SIZE;

        let has_sel = canvas.selection.active;
        let paint_target = canvas.layer_stack.layers[layer_idx].paint_target;
        let paint_mask = match &stroke {
            Some(s) => s.paint_mask,
            None => paint_target == crate::core::layer::PaintTarget::Mask,
        };
        let lock_alpha = paint_target == crate::core::layer::PaintTarget::Pixels
            && canvas.layer_stack.layers[layer_idx].lock_alpha;
        // Channels-panel write gate: with a partial channel selection the dab
        // paints the colour's luma into the enabled channels only (alpha
        // untouched). Mask painting is grayscale and ignores the gate.
        let channel_wm = if paint_mask {
            None
        } else {
            canvas.channels.write_gate()
        };
        let sel = &canvas.selection;
        let Some(tiles) = canvas.layer_stack.layers[layer_idx].get_paint_tiles_mut() else {
            return;
        };

        let is_eraser = settings.is_eraser;

        let tx_start = lx0 / tile_size;
        let tx_end = lx1 / tile_size;
        let ty_start = ly0 / tile_size;
        let ty_end = ly1 / tile_size;
        let n_tiles = (((tx_end - tx_start) + 1) * ((ty_end - ty_start) + 1)) as usize;
        tiles.tiles.reserve(n_tiles);

        // Each touched tile is taken out of the layer (and its coverage out of
        // the stroke) for the length of the dab, so that one job owns it, and
        // put back after.
        struct TileJob<'a> {
            pos: crate::core::tile::TilePos,
            tile: std::sync::Arc<crate::core::tile::Tile>,
            /// Coverage of this tile (buffered strokes only).
            cov: Option<Box<[f32]>>,
            /// The tile as the stroke began (None: it was empty then).
            base: Option<&'a crate::core::tile::Tile>,
            tile_x0: u32,
            tile_y0: u32,
        }

        let (base_tiles, mut cov_tiles) = match stroke {
            Some(s) => (Some(&s.base.tiles), Some(&mut s.cov)),
            None => (None, None),
        };
        let tile_pixels = (tile_size * tile_size) as usize;
        // What a row of a tile that was empty when the stroke began reads as.
        static EMPTY_ROW: [u8; crate::core::tile::TILE_SIZE as usize * 4] =
            [0; crate::core::tile::TILE_SIZE as usize * 4];
        let mut jobs: Vec<TileJob> = Vec::with_capacity(n_tiles);
        for ty in ty_start..=ty_end {
            let tile_y0 = ty * tile_size;
            let py_min = tile_y0.max(ly0);
            let py_max = (tile_y0 + tile_size).min(ly1);
            if py_max <= py_min {
                continue;
            }
            let ny = lcy.clamp(py_min as f32 + 0.5, py_max as f32 - 0.5);
            let ndy2 = (ny - lcy) * (ny - lcy);
            if ndy2 > r2 {
                continue;
            }
            for tx in tx_start..=tx_end {
                let tile_x0 = tx * tile_size;
                let px_min = tile_x0.max(lx0);
                let px_max = (tile_x0 + tile_size).min(lx1);
                if px_max <= px_min {
                    continue;
                }
                let nx = lcx.clamp(px_min as f32 + 0.5, px_max as f32 - 0.5);
                if (nx - lcx) * (nx - lcx) + ndy2 > r2 {
                    continue;
                }
                let pos = crate::core::tile::TilePos {
                    x: tx as i32,
                    y: ty as i32,
                };
                // Copy-on-write, a fresh revision, the 16-bit master and ink
                // dropped: what any 8-bit write to a tile does.
                tiles.get_tile_mut(pos);
                let Some(tile) = tiles.tiles.remove(&pos) else {
                    continue;
                };
                let cov = cov_tiles.as_mut().map(|cov| {
                    cov.remove(&pos)
                        .unwrap_or_else(|| vec![0.0f32; tile_pixels].into_boxed_slice())
                });
                let base = base_tiles.and_then(|b| b.get(&pos)).map(|t| &**t);
                jobs.push(TileJob {
                    pos,
                    tile,
                    cov,
                    base,
                    tile_x0,
                    tile_y0,
                });
            }
        }

        // What every tile of the dab is painted with.
        #[derive(Clone, Copy)]
        struct Dab<'a> {
            lcx: f32,
            lcy: f32,
            r2: f32,
            lut_scale: f32,
            falloff_lut: &'a [f32],
            max_src_a: f32,
            ox: i32,
            oy: i32,
            sel: &'a crate::core::selection::Selection,
            channel_wm: Option<[bool; 4]>,
            opacity: f32,
            mask_target: f32,
            paint_mask: bool,
            is_eraser: bool,
            mode: BrushMode,
            src_r: f32,
            src_g: f32,
            src_b: f32,
            br: u8,
            bg: u8,
            bb: u8,
            lx0: u32,
            ly0: u32,
            lx1: u32,
            ly1: u32,
            tile_size: u32,
        }

        // One copy per combination of the three switches the pixel loop
        // tests most: each copy runs without the branches the others need.
        fn fill_tile<const BUFFERED: bool, const LOCK_ALPHA: bool, const HAS_SEL: bool>(
            dab: &Dab,
            job: &mut TileJob,
        ) {
            let Dab {
                lcx,
                lcy,
                r2,
                lut_scale,
                falloff_lut,
                max_src_a,
                ox,
                oy,
                sel,
                channel_wm,
                opacity,
                mask_target,
                paint_mask,
                is_eraser,
                mode,
                src_r,
                src_g,
                src_b,
                br,
                bg,
                bb,
                lx0,
                ly0,
                lx1,
                ly1,
                tile_size,
            } = *dab;
            let to8 = q8;
            let (buffered, lock_alpha, has_sel) = (BUFFERED, LOCK_ALPHA, HAS_SEL);
            let pixels = &mut std::sync::Arc::make_mut(&mut job.tile).pixels;
            let mut cov_tile = job.cov.as_deref_mut();
            let base = job.base;
            // Stands in for the coverage of an unbuffered dab, which has none.
            let mut no_cov = [0.0f32; crate::core::tile::TILE_SIZE as usize];
            let tile_x0 = job.tile_x0;
            let tile_y0 = job.tile_y0;
            let px_min = tile_x0.max(lx0);
            let py_min = tile_y0.max(ly0);
            let px_max = (tile_x0 + tile_size).min(lx1);
            let py_max = (tile_y0 + tile_size).min(ly1);
            if px_max <= px_min || py_max <= py_min {
                return;
            }
            for py in py_min..py_max {
                let dy = py as f32 + 0.5 - lcy;
                let dy2 = dy * dy;
                if dy2 > r2 {
                    continue;
                }
                // Only the pixels of this row the dab can reach, with one to
                // spare on each side: the test on `d2` below still decides.
                let half = (r2 - dy2).sqrt();
                let x_min = ((lcx - 0.5 - half).floor() - 1.0).max(px_min as f32) as u32;
                let x_max = ((lcx - 0.5 + half).ceil() + 2.0).min(px_max as f32) as u32;
                if x_max <= x_min {
                    continue;
                }
                let row_offset = (py - tile_y0) * tile_size;
                let canvas_py = (py as i32 + oy) as u32;
                // The row's pixels and coverage, so the loop below indexes
                // nothing.
                let first = (row_offset + x_min - tile_x0) as usize;
                let last = (row_offset + x_max - tile_x0) as usize;
                let Some(row_pix) = pixels.get_mut(first * 4..last * 4) else {
                    continue;
                };
                let row_cov = match cov_tile.as_deref_mut() {
                    Some(cov) => cov.get_mut(first..last),
                    None => no_cov.get_mut(..last - first),
                };
                let Some(row_cov) = row_cov else {
                    continue;
                };
                let row_base = match base {
                    Some(b) => b.pixels.get(first * 4..last * 4),
                    None => EMPTY_ROW.get(..(last - first) * 4),
                };
                let Some(row_base) = row_base else {
                    continue;
                };

                for (((px, pix), cov), bpx) in (x_min..x_max)
                    .zip(row_pix.chunks_exact_mut(4))
                    .zip(row_cov)
                    .zip(row_base.chunks_exact(4))
                {
                    let dx = px as f32 + 0.5 - lcx;
                    let d2 = dx * dx + dy2;
                    if d2 > r2 {
                        continue;
                    }
                    let idx = (d2 * lut_scale) as usize;
                    let alpha = falloff_lut.get(idx).copied().unwrap_or(0.0);

                    let mut src_a = alpha * max_src_a;
                    if src_a < 0.001 {
                        continue;
                    }

                    if has_sel {
                        let canvas_px = (px as i32 + ox) as u32;
                        src_a *= sel.sample(canvas_px, canvas_py);
                        if src_a < 0.001 {
                            continue;
                        }
                    }

                    if let Some(wm) = channel_wm {
                        // Partial channel selection: paint the colour's
                        // luma into the enabled channels (the eraser
                        // carries the background colour here). Alpha and
                        // the other channels stay — buffered dabs rewrite
                        // them from the stroke-start base, which keeps
                        // overlapping dabs idempotent.
                        if buffered {
                            *cov += src_a * (1.0 - *cov);
                            let s = (*cov * opacity).clamp(0.0, 1.0);
                            let (b_r, b_g, b_b, b_a) = (bpx[0], bpx[1], bpx[2], bpx[3]);
                            let base = [b_r, b_g, b_b];
                            for (c, &b8) in base.iter().enumerate() {
                                let out = if wm[c] {
                                    let b = b8 as f32 / 255.0;
                                    q8(b + (mask_target - b) * s)
                                } else {
                                    b8
                                };
                                pix[c] = out;
                            }
                            pix[3] = b_a;
                        } else {
                            for c in 0..3 {
                                if wm[c] {
                                    let d = pix[c] as f32 / 255.0;
                                    pix[c] = q8(d + (mask_target - d) * src_a);
                                }
                            }
                        }
                        continue;
                    }

                    if buffered {
                        // PS model: accumulate flow into the stroke's
                        // coverage, then composite base ⊕ (color, cov ×
                        // opacity). Re-deriving from the stroke-start
                        // snapshot on every dab makes overlap idempotent —
                        // one stroke can never exceed its opacity.
                        *cov += src_a * (1.0 - *cov);
                        let s = (*cov * opacity).clamp(0.0, 1.0);
                        let (b_r, b_g, b_b, b_a) = (bpx[0], bpx[1], bpx[2], bpx[3]);

                        if paint_mask {
                            let bgray = b_r as f32 / 255.0;
                            let out = bgray + (mask_target - bgray) * s;
                            let out8 = q8(out);
                            pix[0] = out8;
                            pix[1] = out8;
                            pix[2] = out8;
                            pix[3] = 255;
                        } else if is_eraser {
                            let base_a = b_a as f32 / 255.0;
                            let out_a = base_a * (1.0 - s);
                            if out_a < 0.001 {
                                pix[0] = 0;
                                pix[1] = 0;
                                pix[2] = 0;
                                pix[3] = 0;
                            } else {
                                pix[0] = b_r;
                                pix[1] = b_g;
                                pix[2] = b_b;
                                pix[3] = q8(out_a);
                            }
                        } else {
                            let base_a = b_a as f32 / 255.0;
                            let base_r = b_r as f32 / 255.0;
                            let base_g = b_g as f32 / 255.0;
                            let base_b = b_b as f32 / 255.0;
                            match mode {
                                BrushMode::Behind => {
                                    // Paint goes under what the layer has:
                                    // only its see-through part takes it.
                                    let add = s * (1.0 - base_a);
                                    let out_a = base_a + add;
                                    if lock_alpha || out_a < 0.001 {
                                        continue;
                                    }
                                    pix[0] = to8((base_r * base_a + src_r * add) / out_a);
                                    pix[1] = to8((base_g * base_a + src_g * add) / out_a);
                                    pix[2] = to8((base_b * base_a + src_b * add) / out_a);
                                    pix[3] = to8(out_a);
                                    continue;
                                }
                                BrushMode::Clear => {
                                    // Paints transparency, like the Eraser.
                                    if lock_alpha {
                                        continue;
                                    }
                                    let out_a = base_a * (1.0 - s);
                                    if out_a < 0.001 {
                                        pix.copy_from_slice(&[0, 0, 0, 0]);
                                    } else {
                                        pix.copy_from_slice(&[b_r, b_g, b_b, to8(out_a)]);
                                    }
                                    continue;
                                }
                                BrushMode::Normal | BrushMode::Blend(_) => {}
                            }
                            // Locked alpha paints only where the layer was not
                            // transparent (alpha 0 is the only byte under 0.001).
                            if lock_alpha && b_a == 0 {
                                continue;
                            }
                            // The colour the paint lays down: the brush colour
                            // blended with what is under it by the mode, as far
                            // as there is something under it (W3C compositing:
                            // on a see-through pixel every mode paints Normal).
                            let (cs_r, cs_g, cs_b) = match mode {
                                BrushMode::Blend(m) if base_a > 0.0 => {
                                    let (r, g, b) =
                                        blend_pixel(m, src_r, src_g, src_b, base_r, base_g, base_b);
                                    (
                                        src_r + (r - src_r) * base_a,
                                        src_g + (g - src_g) * base_a,
                                        src_b + (b - src_b) * base_a,
                                    )
                                }
                                _ => (src_r, src_g, src_b),
                            };
                            let dst_weight = base_a * (1.0 - s);
                            if lock_alpha {
                                let denom = s + dst_weight;
                                if denom < 0.001 {
                                    continue;
                                }
                                let inv = 1.0 / denom;
                                pix[0] = to8((cs_r * s + base_r * dst_weight) * inv);
                                pix[1] = to8((cs_g * s + base_g * dst_weight) * inv);
                                pix[2] = to8((cs_b * s + base_b * dst_weight) * inv);
                                pix[3] = b_a;
                            } else {
                                let out_a = s + dst_weight;
                                if out_a < 0.001 {
                                    continue;
                                }
                                let inv = 1.0 / out_a;
                                pix[0] = to8((cs_r * s + base_r * dst_weight) * inv);
                                pix[1] = to8((cs_g * s + base_g * dst_weight) * inv);
                                pix[2] = to8((cs_b * s + base_b * dst_weight) * inv);
                                pix[3] = to8(out_a);
                            }
                        }
                        continue;
                    }

                    if paint_mask {
                        // Standard source-over lerp so repeated strokes keep
                        // moving the mask toward the brush value (the old
                        // max/min cap froze low-opacity paint at target*a
                        // forever — soft brushes could never cover).
                        let dst = pix[0] as f32 / 255.0;
                        let out = dst + (mask_target - dst) * src_a;
                        let out8 = q8(out);
                        pix[0] = out8;
                        pix[1] = out8;
                        pix[2] = out8;
                        pix[3] = 255;
                        continue;
                    }

                    if is_eraser {
                        let dst_a8 = pix[3];
                        if dst_a8 == 0 {
                            continue;
                        }
                        let dst_a = dst_a8 as f32 / 255.0;
                        let out_a = (dst_a - src_a).clamp(0.0, 1.0);
                        if out_a < 0.001 {
                            pix[0] = 0;
                            pix[1] = 0;
                            pix[2] = 0;
                            pix[3] = 0;
                        } else {
                            pix[3] = q8(out_a);
                        }
                    } else {
                        let dst_a8 = pix[3];

                        if src_a >= 0.999 && !lock_alpha {
                            pix[0] = br;
                            pix[1] = bg;
                            pix[2] = bb;
                            pix[3] = 255;
                            continue;
                        }

                        let dst_a = dst_a8 as f32 / 255.0;
                        let inv_src_a = 1.0 - src_a;
                        let dst_weight = dst_a * inv_src_a;
                        let mut out_a = src_a + dst_weight;

                        if lock_alpha {
                            out_a = dst_a;
                        }

                        if out_a < 0.001 {
                            continue;
                        }

                        let dst_r8 = pix[0];
                        let dst_g8 = pix[1];
                        let dst_b8 = pix[2];

                        let dst_r = dst_r8 as f32 / 255.0;
                        let dst_g = dst_g8 as f32 / 255.0;
                        let dst_b = dst_b8 as f32 / 255.0;

                        let denom = src_a + dst_weight;
                        if denom < 0.001 {
                            continue;
                        }
                        let out_r = (src_r * src_a + dst_r * dst_weight) / denom;
                        let out_g = (src_g * src_a + dst_g * dst_weight) / denom;
                        let out_b = (src_b * src_a + dst_b * dst_weight) / denom;

                        // Rounded (q8): plain truncation drifts every blend
                        // toward black/transparent, leaving dark streaks.
                        pix[0] = q8(out_r);
                        pix[1] = q8(out_g);
                        pix[2] = q8(out_b);
                        pix[3] = q8(out_a);
                    }
                }
            }
        }

        let dab = Dab {
            lcx,
            lcy,
            r2,
            lut_scale,
            falloff_lut: &falloff_lut,
            max_src_a,
            ox,
            oy,
            sel,
            channel_wm,
            opacity,
            mask_target,
            paint_mask,
            is_eraser,
            mode: settings.mode,
            src_r,
            src_g,
            src_b,
            br,
            bg,
            bb,
            lx0,
            ly0,
            lx1,
            ly1,
            tile_size,
        };
        let fill = |job: &mut TileJob| match (buffered, lock_alpha, has_sel) {
            (false, false, false) => fill_tile::<false, false, false>(&dab, job),
            (false, false, true) => fill_tile::<false, false, true>(&dab, job),
            (false, true, false) => fill_tile::<false, true, false>(&dab, job),
            (false, true, true) => fill_tile::<false, true, true>(&dab, job),
            (true, false, false) => fill_tile::<true, false, false>(&dab, job),
            (true, false, true) => fill_tile::<true, false, true>(&dab, job),
            (true, true, false) => fill_tile::<true, true, false>(&dab, job),
            (true, true, true) => fill_tile::<true, true, true>(&dab, job),
        };

        if jobs.len() >= 4 {
            use rayon::prelude::*;
            jobs.par_iter_mut().for_each(fill);
        } else {
            jobs.iter_mut().for_each(fill);
        }
        for job in jobs {
            tiles.tiles.insert(job.pos, job.tile);
            if let (Some(cov_tiles), Some(cov)) = (cov_tiles.as_mut(), job.cov) {
                cov_tiles.insert(job.pos, cov);
            }
        }
        let canvas_x0 = (lx0 as i32 + ox).max(0) as u32;
        let canvas_y0 = (ly0 as i32 + oy).max(0) as u32;
        let canvas_x1 = ((lx1 as i32 + ox) as u32).min(canvas.width);
        let canvas_y1 = ((ly1 as i32 + oy) as u32).min(canvas.height);
        canvas.mark_dirty(canvas_x0, canvas_y0, canvas_x1, canvas_y1);
    }

    /// Paint one direct (unbuffered) ink dab onto the active layer of a CMYK
    /// document. The foreground colour is converted to ink once, blended into
    /// each pixel's CMYK plane by coverage (source-over on the mirror's alpha),
    /// and the touched pixels' RGB mirror is re-projected from the new ink so
    /// display/compositing stay correct. Erasers lower the mirror alpha and
    /// clear the ink where fully erased. `lock_alpha` paints only where the
    /// layer is already opaque and preserves alpha, matching the RGB brush.
    ///
    /// When the Channels panel selects a subset of C/M/Y/K plates (`write_gate`),
    /// the dab switches to *plate paint*: only the enabled ink channels move
    /// (toward the colour's ink, or toward 0 for the eraser) and alpha stays —
    /// mirroring the RGB channel-gate semantics. Undo rides the stroke's
    /// DeltaSnapshot (writes go through the tiles' COW).
    fn paint_ink_dab(settings: &BrushSettings, canvas: &mut Canvas, cx: f32, cy: f32) {
        let Some(conv) = canvas.cmyk_converter() else {
            return;
        };
        let plate_gate = canvas.channels.write_gate();
        let [br, bg, bb, ba] = settings.color;
        let fg_ink = conv.rgb_to_cmyk_one([br, bg, bb]);
        let is_eraser = settings.is_eraser;
        let max_src_a =
            (ba as f32 / 255.0) * settings.opacity.clamp(0.0, 1.0) * settings.flow.clamp(0.0, 1.0);
        if max_src_a < 0.001 {
            return;
        }

        let tip = settings.tip();
        let (cx, cy) = settings.dab_centre(cx, cy);
        let r = tip.reach();
        let r2 = r * r;

        let layer_idx = canvas.layer_stack.active_idx;
        let ox = canvas.layer_stack.layers[layer_idx].offset.0;
        let oy = canvas.layer_stack.layers[layer_idx].offset.1;
        let lw = canvas.layer_stack.layers[layer_idx].width;
        let lh = canvas.layer_stack.layers[layer_idx].height;
        let lock_alpha = canvas.layer_stack.layers[layer_idx].lock_alpha;

        let lcx = cx - ox as f32;
        let lcy = cy - oy as f32;
        let lx0 = ((lcx - r).floor().max(0.0) as u32).min(lw);
        let ly0 = ((lcy - r).floor().max(0.0) as u32).min(lh);
        let lx1 = ((lcx + r).ceil() as u32).min(lw);
        let ly1 = ((lcy + r).ceil() as u32).min(lh);
        if lx1 <= lx0 || ly1 <= ly0 {
            return;
        }

        let (falloff_lut, lut_scale) = tip.table();

        let has_sel = canvas.selection.active;
        let sel = &canvas.selection;

        let tile_size = crate::core::tile::TILE_SIZE;
        let Some(tiles) = canvas.layer_stack.layers[layer_idx].get_paint_tiles_mut() else {
            return;
        };
        let tx_start = lx0 / tile_size;
        let tx_end = (lx1 - 1) / tile_size;
        let ty_start = ly0 / tile_size;
        let ty_end = (ly1 - 1) / tile_size;

        for ty in ty_start..=ty_end {
            let tile_y0 = ty * tile_size;
            for tx in tx_start..=tx_end {
                let tile_x0 = tx * tile_size;
                let px_min = tile_x0.max(lx0);
                let py_min = tile_y0.max(ly0);
                let px_max = (tile_x0 + tile_size).min(lx1);
                let py_max = (tile_y0 + tile_size).min(ly1);
                if px_max <= px_min || py_max <= py_min {
                    continue;
                }
                let pos = crate::core::tile::TilePos {
                    x: tx as i32,
                    y: ty as i32,
                };
                let crate::core::tile::Tile { ink, pixels, .. } = tiles.get_tile_mut_ink(pos);
                let plane = ink
                    .as_mut()
                    .expect("get_tile_mut_ink guarantees an ink plane");
                let mut touched: Vec<usize> = Vec::new();
                for py in py_min..py_max {
                    let dy = py as f32 + 0.5 - lcy;
                    let dy2 = dy * dy;
                    let row = (py - tile_y0) * tile_size;
                    for px in px_min..px_max {
                        let dx = px as f32 + 0.5 - lcx;
                        let d2 = dx * dx + dy2;
                        if d2 > r2 {
                            continue;
                        }
                        let li = (d2 * lut_scale) as usize;
                        let mut src_a = falloff_lut.get(li).copied().unwrap_or(0.0) * max_src_a;
                        if src_a < 0.001 {
                            continue;
                        }
                        if has_sel {
                            let canvas_px = (px as i32 + ox) as u32;
                            let canvas_py = (py as i32 + oy) as u32;
                            src_a *= sel.sample(canvas_px, canvas_py);
                            if src_a < 0.001 {
                                continue;
                            }
                        }
                        let i = ((row + (px - tile_x0)) * 4) as usize;
                        let dst_a = pixels[i + 3] as f32 / 255.0;
                        if let Some(wm) = plate_gate {
                            // Plate paint: only the enabled ink channels move
                            // (toward the colour's ink, or toward 0 to erase);
                            // alpha and the other plates are left as-is.
                            let target = if is_eraser { [0u8; 4] } else { fg_ink };
                            for c in 0..4 {
                                if wm[c] {
                                    let d = plane[i + c] as f32 / 255.0;
                                    let t = target[c] as f32 / 255.0;
                                    plane[i + c] = crate::core::fast_math::round_u8(
                                        (d + (t - d) * src_a) * 255.0,
                                    );
                                }
                            }
                            touched.push(i);
                        } else if is_eraser {
                            let new_a = dst_a * (1.0 - src_a);
                            pixels[i + 3] = crate::core::fast_math::round_u8(new_a * 255.0);
                            if new_a < 0.001 {
                                plane[i] = 0;
                                plane[i + 1] = 0;
                                plane[i + 2] = 0;
                                plane[i + 3] = 0;
                            }
                            touched.push(i);
                        } else {
                            if lock_alpha && dst_a < 0.001 {
                                continue;
                            }
                            let new_a = if lock_alpha {
                                dst_a
                            } else {
                                src_a + dst_a * (1.0 - src_a)
                            };
                            if new_a < 0.001 {
                                continue;
                            }
                            let dw = dst_a * (1.0 - src_a);
                            for c in 0..4 {
                                let d = plane[i + c] as f32 / 255.0;
                                let s = fg_ink[c] as f32 / 255.0;
                                let out = (s * src_a + d * dw) / new_a;
                                plane[i + c] = crate::core::fast_math::round_u8(out * 255.0);
                            }
                            pixels[i + 3] = crate::core::fast_math::round_u8(new_a * 255.0);
                            touched.push(i);
                        }
                    }
                }
                if !touched.is_empty() {
                    // Re-project the mirror RGB for the changed pixels in one
                    // converter call per tile (bounds the ICC transform cost).
                    let inks: Vec<[u8; 4]> = touched
                        .iter()
                        .map(|&i| [plane[i], plane[i + 1], plane[i + 2], plane[i + 3]])
                        .collect();
                    let mut rgb = vec![[0u8; 3]; inks.len()];
                    conv.cmyk_to_rgb_slice(&inks, &mut rgb);
                    for (k, &i) in touched.iter().enumerate() {
                        pixels[i] = rgb[k][0];
                        pixels[i + 1] = rgb[k][1];
                        pixels[i + 2] = rgb[k][2];
                    }
                }
            }
        }

        let canvas_x0 = (lx0 as i32 + ox).max(0) as u32;
        let canvas_y0 = (ly0 as i32 + oy).max(0) as u32;
        let canvas_x1 = ((lx1 as i32 + ox) as u32).min(canvas.width);
        let canvas_y1 = ((ly1 as i32 + oy) as u32).min(canvas.height);
        canvas.mark_dirty(canvas_x0, canvas_y0, canvas_x1, canvas_y1);
    }

    /// Paint directly into the viewed saved alpha channel. Returns true when
    /// the active Channels-panel view consumed the dab, even if no pixel changed.
    fn paint_alpha_plane_dab(
        settings: &BrushSettings,
        canvas: &mut Canvas,
        cx: f32,
        cy: f32,
    ) -> bool {
        let crate::core::channels::ChannelView::Alpha(alpha_id) = canvas.channels.view else {
            return false;
        };
        let Some(idx) = canvas.channels.alpha_index_of(alpha_id) else {
            return true;
        };

        let tip = settings.tip();
        let (cx, cy) = settings.dab_centre(cx, cy);
        let r = tip.reach();
        let r2 = r * r;
        let w = canvas.channels.alpha[idx].width;
        let h = canvas.channels.alpha[idx].height;
        if w == 0 || h == 0 {
            return true;
        }

        let x0 = ((cx - r).floor().max(0.0) as u32).min(w);
        let y0 = ((cy - r).floor().max(0.0) as u32).min(h);
        let x1 = ((cx + r).ceil() as u32).min(w);
        let y1 = ((cy + r).ceil() as u32).min(h);
        if x1 <= x0 || y1 <= y0 {
            return true;
        }

        let [br, bg, bb, ba] = settings.color;
        let max_src_a =
            (ba as f32 / 255.0) * settings.opacity.clamp(0.0, 1.0) * settings.flow.clamp(0.0, 1.0);
        if max_src_a < 0.001 {
            return true;
        }
        let target = ((br as f32 / 255.0) * 0.2126
            + (bg as f32 / 255.0) * 0.7152
            + (bb as f32 / 255.0) * 0.0722)
            .clamp(0.0, 1.0);

        if canvas.pending_alpha_stroke.as_ref().map(|p| p.alpha_id) != Some(alpha_id) {
            let before = canvas.channels.alpha[idx].mask.clone();
            canvas.pending_alpha_stroke = Some(crate::core::channels::PendingAlphaStroke {
                alpha_id,
                before,
                bbox: None,
            });
        }

        let (falloff_lut, lut_scale) = tip.table();

        let has_sel = canvas.selection.active;
        let sel = &canvas.selection;
        let ch = &mut canvas.channels.alpha[idx];
        if ch.mask.len() < (w as usize).saturating_mul(h as usize) {
            return true;
        }

        let mut changed = false;
        for y in y0..y1 {
            let dy = y as f32 + 0.5 - cy;
            let dy2 = dy * dy;
            let row = y as usize * w as usize;
            for x in x0..x1 {
                let dx = x as f32 + 0.5 - cx;
                let d2 = dx * dx + dy2;
                if d2 > r2 {
                    continue;
                }
                let idx_lut = (d2 * lut_scale) as usize;
                let mut src_a = falloff_lut.get(idx_lut).copied().unwrap_or(0.0) * max_src_a;
                if src_a < 0.001 {
                    continue;
                }
                if has_sel {
                    src_a *= sel.sample(x, y);
                    if src_a < 0.001 {
                        continue;
                    }
                }

                let i = row + x as usize;
                let dst = ch.mask[i] as f32 / 255.0;
                let out = dst + (target - dst) * src_a;
                let out8 = q8(out);
                if ch.mask[i] != out8 {
                    ch.mask[i] = out8;
                    changed = true;
                }
            }
        }

        if changed {
            ch.revision += 1;
            canvas.mark_plane_dirty(x0, y0, x1, y1);
            if let Some(pending) = canvas.pending_alpha_stroke.as_mut() {
                pending.bbox = Some(match pending.bbox {
                    Some((bx0, by0, bx1, by1)) => {
                        (bx0.min(x0), by0.min(y0), bx1.max(x1), by1.max(y1))
                    }
                    None => (x0, y0, x1, y1),
                });
            }
        }

        true
    }

    /// Walk a stroke segment placing dab centres every `spacing` px along the
    /// polyline; `residual` is the distance from the segment start to the next
    /// dab and carries the leftover walk into the following event, so dab
    /// density does not depend on how the OS chops the stroke into mouse
    /// events and segment joints are never stamped twice.
    pub(crate) fn walk_dabs(
        spacing_base: f32,
        x0: f32,
        y0: f32,
        x1: f32,
        y1: f32,
        residual: &mut f32,
        mut dab: impl FnMut(f32, f32),
    ) {
        if !x0.is_finite() || !y0.is_finite() || !x1.is_finite() || !y1.is_finite() {
            return;
        }
        let dx = x1 - x0;
        let dy = y1 - y0;
        let dist = (dx * dx + dy * dy).sqrt();
        if dist <= 0.0 {
            return;
        }
        // Safety cap (mirrors the old 1000-step clamp): stretch spacing on
        // enormous jumps instead of stamping tens of thousands of dabs.
        let spacing = spacing_base.max(dist / 1024.0);
        let mut t = residual.max(0.0);
        while t <= dist {
            let f = t / dist;
            dab(x0 + dx * f, y0 + dy * f);
            t += spacing;
        }
        *residual = t - dist;
    }

    /// Distance-walked segment in direct (unbuffered) dab mode. Initialise
    /// `residual` to `dab_spacing()` right after stamping the press dab.
    pub fn paint_cpu_stroke_segment(
        settings: &BrushSettings,
        canvas: &mut Canvas,
        x0: f32,
        y0: f32,
        x1: f32,
        y1: f32,
        residual: &mut f32,
    ) {
        Self::walk_dabs(settings.dab_spacing(), x0, y0, x1, y1, residual, |x, y| {
            Self::paint_cpu_dab(settings, canvas, x, y)
        });
    }

    /// Distance-walked segment in buffered (per-stroke opacity) mode; the
    /// walker residual lives in the stroke buffer.
    pub fn paint_cpu_stroke_segment_stroked(
        settings: &BrushSettings,
        canvas: &mut Canvas,
        x0: f32,
        y0: f32,
        x1: f32,
        y1: f32,
        stroke: &mut StrokeBuffer,
    ) {
        let mut residual = stroke.residual;
        Self::walk_dabs(
            settings.dab_spacing(),
            x0,
            y0,
            x1,
            y1,
            &mut residual,
            |x, y| Self::paint_cpu_dab_stroked(settings, canvas, x, y, stroke),
        );
        stroke.residual = residual;
    }
}

impl Tool for BrushTool {
    fn id(&self) -> &'static str {
        "brush"
    }
    fn name(&self) -> &str {
        "Brush"
    }
    fn shortcut(&self) -> Option<char> {
        Some('B')
    }
    fn tool_id(&self) -> crate::tools::ToolId {
        crate::tools::ToolId::Brush
    }
    fn paints(&self) -> bool {
        true
    }
    fn cursor_size(&self) -> f32 {
        self.settings.size * 0.5
    }
    fn tip_hardness(&self) -> Option<f32> {
        Some(self.settings.hardness)
    }

    fn on_press(&mut self, event: PointerEvent, ctx: &mut ToolCtx) -> ToolResponse {
        ctx.canvas_mut().begin_stroke("Brush Stroke");
        let (cx, cy) = (event.canvas_x, event.canvas_y);
        // Shift+click: the stroke starts as a line from the last one's end.
        let line_from = self.lines.press(ctx.document.id, cx, cy, event.shift);
        let (sx, sy) = line_from.unwrap_or((cx, cy));
        self.smoother.begin(cx, cy);
        self.air_clock = Some(std::time::Instant::now());
        self.air_debt = 0.0;
        // Each stroke scatters its own way.
        self.strokes = self.strokes.wrapping_add(1);
        let place = (u64::from(sx.to_bits()) << 32) | u64::from(sy.to_bits());
        self.rng = DabRng::new(place ^ self.strokes.wrapping_mul(0x9E37_79B9_7F4A_7C15));
        let alpha_plane = matches!(
            ctx.canvas().channels.view,
            crate::core::channels::ChannelView::Alpha(_)
        );
        if alpha_plane {
            self.stroke = None;
            let (px, py, s) = self
                .dynamics
                .vary(&self.settings, &mut self.rng, sx, sy, None);
            Self::paint_cpu_dab(&s, ctx.canvas_mut(), px, py);
            self.stroke_residual = self.settings.dab_spacing();
        } else {
            self.stroke = StrokeBuffer::begin(ctx.canvas());
        }
        if let Some(stroke) = self.stroke.as_mut() {
            let (px, py, s) = self
                .dynamics
                .vary(&self.settings, &mut self.rng, sx, sy, None);
            Self::paint_cpu_dab_stroked(&s, ctx.canvas_mut(), px, py, stroke);
            stroke.residual = self.settings.dab_spacing();
        }
        if line_from.is_some() {
            self.paint_segment(ctx, sx, sy, cx, cy);
        }
        ToolResponse::repaint()
    }

    fn on_drag(
        &mut self,
        event: PointerEvent,
        _prev: &PointerEvent,
        ctx: &mut ToolCtx,
    ) -> ToolResponse {
        let (x, y) = self.lines.constrain(event.canvas_x, event.canvas_y);
        let ((x0, y0), (x1, y1)) = self
            .smoother
            .follow(x, y, self.settings.smoothing, ctx.zoom);
        self.paint_segment(ctx, x0, y0, x1, y1);
        ToolResponse::repaint()
    }

    fn on_frame(&mut self, ctx: &mut ToolCtx) -> ToolResponse {
        // Smoothing catches up with a resting pointer.
        let caught = self.smoother.catch_up(self.settings.smoothing);
        if let Some(((x0, y0), (x1, y1))) = caught {
            self.paint_segment(ctx, x0, y0, x1, y1);
        }
        // The airbrush keeps laying paint where the paint is.
        let airbrushing = self.airbrush && self.stroke.is_some();
        if airbrushing {
            let now = std::time::Instant::now();
            let since = self
                .air_clock
                .replace(now)
                .map_or(0.0, |t| now.duration_since(t).as_secs_f32());
            self.air_debt += since * AIRBRUSH_DABS_PER_SEC;
            let due = self.air_debt.floor();
            self.air_debt -= due;
            let (x, y) = self.smoother.at();
            if let Some(stroke) = self.stroke.as_mut() {
                // A stalled frame does not dump a burst of paint.
                for _ in 0..(due as u32).min(8) {
                    let (px, py, s) = self
                        .dynamics
                        .vary(&self.settings, &mut self.rng, x, y, None);
                    Self::paint_cpu_dab_stroked(&s, ctx.canvas_mut(), px, py, stroke);
                }
            }
        }
        // Ask for more frames while either still has work.
        if caught.is_some() || airbrushing || self.smoother.pending(self.settings.smoothing) {
            ToolResponse::repaint()
        } else {
            ToolResponse::none()
        }
    }

    fn on_release(&mut self, event: PointerEvent, ctx: &mut ToolCtx) -> ToolResponse {
        // The stroke ends where the pointer let go, smoothing or not.
        let (x, y) = self.lines.constrain(event.canvas_x, event.canvas_y);
        if let Some(((x0, y0), (x1, y1))) = self.smoother.finish(x, y) {
            self.paint_segment(ctx, x0, y0, x1, y1);
        }
        self.lines.release(ctx.document.id, x, y);
        self.stroke = None;
        self.air_clock = None;
        ToolResponse::none()
    }
}

impl BrushTool {
    /// Paint the stroke from `(x0, y0)` to `(x1, y1)` on whatever this
    /// stroke targets.
    fn paint_segment(&mut self, ctx: &mut ToolCtx, x0: f32, y0: f32, x1: f32, y1: f32) {
        let alpha_plane = matches!(
            ctx.canvas().channels.view,
            crate::core::channels::ChannelView::Alpha(_)
        );
        let Self {
            settings,
            dynamics,
            rng,
            stroke,
            stroke_residual,
            ..
        } = self;
        let spacing = settings.dab_spacing();
        let dir = Some((x1 - x0, y1 - y0));
        let canvas = ctx.canvas_mut();
        if let Some(stroke) = stroke.as_mut() {
            let mut residual = stroke.residual;
            Self::walk_dabs(spacing, x0, y0, x1, y1, &mut residual, |x, y| {
                let (px, py, s) = dynamics.vary(settings, rng, x, y, dir);
                Self::paint_cpu_dab_stroked(&s, canvas, px, py, stroke);
            });
            stroke.residual = residual;
        } else if alpha_plane {
            Self::walk_dabs(spacing, x0, y0, x1, y1, stroke_residual, |x, y| {
                let (px, py, s) = dynamics.vary(settings, rng, x, y, dir);
                Self::paint_cpu_dab(&s, canvas, px, py);
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::soft_round_alpha;
    use super::*;

    #[test]
    fn channel_gate_paints_luma_into_selected_channel_only() {
        let mut canvas = Canvas::new_blank(16, 16);
        // Gray background layer (128,128,128,255); write only Red.
        let idx = canvas.layer_stack.active_idx;
        if let Some(tiles) = canvas.layer_stack.layers[idx].get_paint_tiles_mut() {
            for y in 0..16 {
                for x in 0..16 {
                    tiles.set_pixel(x, y, 128, 128, 128, 255);
                }
            }
        }
        canvas.channels.select_color(0, false);

        let settings = BrushSettings {
            size: 8.0,
            hardness: 1.0,
            opacity: 1.0,
            flow: 1.0,
            color: [255, 255, 255, 255], // luma 255
            ..BrushSettings::default()
        };
        BrushTool::paint_cpu_dab(&settings, &mut canvas, 8.0, 8.0);
        let (r, g, b, a) = canvas.layer_stack.layers[idx].tiles.get_pixel(8, 8);
        assert_eq!(r, 255, "red plate painted to the colour's luma");
        assert_eq!((g, b, a), (128, 128, 255), "G/B/alpha untouched");
    }

    #[test]
    fn brush_paints_viewed_alpha_channel_and_undoes_as_one_stroke() {
        let mut canvas = Canvas::new_blank(8, 8);
        let alpha_idx = canvas
            .channels
            .add_alpha("Stored".into(), vec![0; 64], 8, 8);
        let alpha_id = canvas.channels.alpha[alpha_idx].id;
        canvas.channels.select_alpha(alpha_idx);

        // (A 4 px hard tip paints soft, as Photoshop's does: 8 px here.)
        let settings = BrushSettings {
            size: 8.0,
            hardness: 1.0,
            opacity: 1.0,
            flow: 1.0,
            color: [255, 255, 255, 255],
            ..BrushSettings::default()
        };
        BrushTool::paint_cpu_dab(&settings, &mut canvas, 4.0, 4.0);

        let center = 4 * 8 + 4;
        assert_eq!(canvas.channels.alpha[alpha_idx].mask[center], 255);
        assert_eq!(
            canvas.pending_alpha_stroke.as_ref().map(|p| p.alpha_id),
            Some(alpha_id)
        );
        assert!(canvas.plane_dirty.active);

        canvas.end_stroke();
        assert!(canvas.pending_alpha_stroke.is_none());
        assert_eq!(canvas.undo_count(), 1);

        canvas.undo();
        let idx = canvas.channels.alpha_index_of(alpha_id).unwrap();
        assert_eq!(canvas.channels.alpha[idx].mask[center], 0);

        canvas.redo();
        let idx = canvas.channels.alpha_index_of(alpha_id).unwrap();
        assert_eq!(canvas.channels.alpha[idx].mask[center], 255);
    }

    #[test]
    fn brush_paints_ink_on_cmyk_and_keeps_mirror_consistent() {
        use crate::core::cms::naive_cmyk_to_rgb;

        let mut canvas = Canvas::new_blank(16, 16);
        let idx = canvas.layer_stack.active_idx;
        if let Some(tiles) = canvas.layer_stack.layers[idx].get_paint_tiles_mut() {
            for y in 0..16 {
                for x in 0..16 {
                    tiles.set_pixel(x, y, 255, 255, 255, 255);
                }
            }
        }
        canvas
            .convert_to_cmyk(crate::core::canvas::CmykProfile::Naive)
            .expect("convert to CMYK");

        let settings = BrushSettings {
            size: 8.0,
            hardness: 1.0,
            opacity: 1.0,
            flow: 1.0,
            color: [255, 0, 0, 255], // red → high M,Y ink
            ..BrushSettings::default()
        };
        canvas.begin_stroke("Brush Stroke");
        BrushTool::paint_cpu_dab(&settings, &mut canvas, 8.0, 8.0);

        // Center ink is the painted red's ink; the mirror is its exact projection.
        let mut ink = [0u8; 4];
        canvas.layer_stack.layers[idx]
            .tiles
            .extract_ink_region_into(8, 8, 1, 1, &mut ink);
        assert_eq!(ink[0], 0, "red carries no cyan");
        assert!(ink[1] > 200 && ink[2] > 200, "red is mostly magenta+yellow");
        let (r, g, b, a) = canvas.layer_stack.layers[idx].tiles.get_pixel(8, 8);
        assert_eq!([r, g, b], naive_cmyk_to_rgb(ink), "mirror must project ink");
        assert_eq!(a, 255, "opaque paint over opaque stays opaque");

        // Whole dab keeps the ink/mirror invariant.
        for (py, px) in [(8u32, 8u32), (6, 6), (10, 10), (5, 9)] {
            let mut k = [0u8; 4];
            canvas.layer_stack.layers[idx]
                .tiles
                .extract_ink_region_into(px, py, 1, 1, &mut k);
            let (pr, pg, pb, pa) = canvas.layer_stack.layers[idx].tiles.get_pixel(px, py);
            if pa > 0 {
                assert_eq!([pr, pg, pb], naive_cmyk_to_rgb(k), "desync at {px},{py}");
            }
        }

        // Undo restores the white background (ink back to none).
        canvas.end_stroke();
        assert_eq!(canvas.undo_count(), 1);
        canvas.undo();
        let mut ink0 = [9u8; 4];
        canvas.layer_stack.layers[idx]
            .tiles
            .extract_ink_region_into(8, 8, 1, 1, &mut ink0);
        assert_eq!(ink0, [0, 0, 0, 0], "undo returns white (no ink)");
    }

    #[test]
    fn cmyk_plate_gate_paints_only_selected_ink_channel() {
        use crate::core::cms::naive_cmyk_to_rgb;

        let mut canvas = Canvas::new_blank(16, 16);
        let idx = canvas.layer_stack.active_idx;
        if let Some(tiles) = canvas.layer_stack.layers[idx].get_paint_tiles_mut() {
            for y in 0..16 {
                for x in 0..16 {
                    tiles.set_pixel(x, y, 255, 255, 255, 255);
                }
            }
        }
        canvas
            .convert_to_cmyk(crate::core::canvas::CmykProfile::Naive)
            .expect("convert to CMYK");
        // Select only the Cyan plate (slot 0 of 4).
        canvas.channels.select_channel_n(0, false, 4);

        let settings = BrushSettings {
            size: 8.0,
            hardness: 1.0,
            opacity: 1.0,
            flow: 1.0,
            color: [255, 0, 0, 255], // red → C=0, M=255, Y=255
            ..BrushSettings::default()
        };
        canvas.begin_stroke("Brush Stroke");
        BrushTool::paint_cpu_dab(&settings, &mut canvas, 8.0, 8.0);

        let mut ink = [0u8; 4];
        canvas.layer_stack.layers[idx]
            .tiles
            .extract_ink_region_into(8, 8, 1, 1, &mut ink);
        // Cyan moved toward red's C (=0); M/Y/K plates and alpha untouched (white).
        assert_eq!(ink, [0, 0, 0, 0], "only Cyan is gated and red's C is 0");
        let (r, g, b, a) = canvas.layer_stack.layers[idx].tiles.get_pixel(8, 8);
        assert_eq!(
            [r, g, b],
            naive_cmyk_to_rgb(ink),
            "mirror projects gated ink"
        );
        assert_eq!(a, 255, "plate paint never changes alpha");

        // Now select Magenta and paint: only M rises, C/Y/K stay.
        canvas.channels.select_channel_n(1, false, 4);
        BrushTool::paint_cpu_dab(&settings, &mut canvas, 8.0, 8.0);
        canvas.layer_stack.layers[idx]
            .tiles
            .extract_ink_region_into(8, 8, 1, 1, &mut ink);
        assert!(ink[1] > 200, "Magenta plate rose toward red's M");
        assert_eq!((ink[0], ink[2], ink[3]), (0, 0, 0), "C/Y/K untouched");
    }

    #[test]
    fn channel_gate_buffered_stroke_is_idempotent_and_capped() {
        let mut canvas = Canvas::new_blank(16, 16);
        let idx = canvas.layer_stack.active_idx;
        if let Some(tiles) = canvas.layer_stack.layers[idx].get_paint_tiles_mut() {
            for y in 0..16 {
                for x in 0..16 {
                    tiles.set_pixel(x, y, 100, 100, 100, 255);
                }
            }
        }
        canvas.channels.select_color(1, false); // Green only

        let settings = BrushSettings {
            size: 8.0,
            hardness: 1.0,
            opacity: 0.5,
            flow: 1.0,
            color: [255, 255, 255, 255], // luma 255
            ..BrushSettings::default()
        };
        let mut stroke = StrokeBuffer::begin(&canvas).expect("paintable layer");
        BrushTool::paint_cpu_dab_stroked(&settings, &mut canvas, 8.0, 8.0, &mut stroke);
        let after_one = canvas.layer_stack.layers[idx].tiles.get_pixel(8, 8);
        for _ in 0..15 {
            BrushTool::paint_cpu_dab_stroked(&settings, &mut canvas, 8.0, 8.0, &mut stroke);
        }
        let after_many = canvas.layer_stack.layers[idx].tiles.get_pixel(8, 8);

        // 50% stroke ceiling: G = 100 + (255-100)*0.5 ≈ 177, R/B/alpha kept.
        assert_eq!(after_one, after_many, "overlapping dabs stay idempotent");
        let (r, g, b, a) = after_many;
        assert!(
            (176..=179).contains(&g),
            "green capped at stroke opacity, got {g}"
        );
        assert_eq!((r, b, a), (100, 100, 255), "R/B/alpha untouched");
    }

    #[test]
    fn channel_gate_eraser_writes_background_luma() {
        let mut canvas = Canvas::new_blank(16, 16);
        let idx = canvas.layer_stack.add_layer(16, 16);
        canvas.layer_stack.active_idx = idx;
        if let Some(tiles) = canvas.layer_stack.layers[idx].get_paint_tiles_mut() {
            for y in 0..16 {
                for x in 0..16 {
                    tiles.set_pixel(x, y, 40, 50, 60, 200);
                }
            }
        }
        canvas.channels.select_color(2, false); // Blue only

        // The eraser carries the background colour in `color` (see eraser.rs).
        let settings = BrushSettings {
            size: 8.0,
            hardness: 1.0,
            opacity: 1.0,
            flow: 1.0,
            color: [255, 255, 255, 255],
            is_eraser: true,
            ..BrushSettings::default()
        };
        BrushTool::paint_cpu_dab(&settings, &mut canvas, 8.0, 8.0);
        let (r, g, b, a) = canvas.layer_stack.layers[idx].tiles.get_pixel(8, 8);
        assert_eq!(b, 255, "blue plate erased to background luma");
        assert_eq!((r, g, a), (40, 50, 200), "other channels and alpha kept");
    }

    #[test]
    fn brush_paints_a_group_mask_and_hides_the_folder_there() {
        use crate::core::layer::Layer;
        let mut canvas = Canvas::new_blank(32, 32);
        let stack = &mut canvas.layer_stack;
        for (id, rgba) in [(2, [255, 0, 0, 255]), (3, [0, 0, 255, 128])] {
            let pixels = rgba.repeat(32 * 32);
            let mut layer = Layer::from_rgba(id, "part", pixels, 32, 32);
            layer.selected = true;
            stack.layers.push(layer);
        }
        stack.set_next_id(4);
        let group = stack.create_group_from_selected(32, 32).unwrap();
        stack.layers[group].add_mask(true);
        stack.active_idx = group;

        let settings = BrushSettings {
            size: 10.0,
            hardness: 1.0,
            color: [0, 0, 0, 255],
            ..BrushSettings::default()
        };
        let mut stroke = StrokeBuffer::begin(&canvas).expect("group mask is paintable");
        BrushTool::paint_cpu_dab_stroked(&settings, &mut canvas, 8.0, 8.0, &mut stroke);
        drop(stroke);

        let stack = &canvas.layer_stack;
        let mask = stack.layers[group].mask.as_ref().unwrap();
        assert_eq!(mask.tiles.get_pixel(8, 8).0, 0, "stroke lands on the mask");
        assert_eq!(
            mask.tiles.get_pixel(24, 24).0,
            255,
            "rest of the mask untouched"
        );
        let flat = stack.flatten(32, 32);
        let px = |x: usize, y: usize| &flat[(y * 32 + x) * 4..(y * 32 + x) * 4 + 3];
        assert_eq!(
            px(8, 8),
            [255, 255, 255],
            "painted area reveals the backdrop"
        );
        assert_ne!(
            px(24, 24),
            [255, 255, 255],
            "unpainted area keeps the folder"
        );
    }

    #[test]
    fn mask_paint_accumulates_across_low_flow_dabs() {
        use crate::core::layer::{LayerMask, PaintTarget};
        let mut canvas = Canvas::new_blank(16, 16);
        let idx = canvas.layer_stack.active_idx;
        canvas.layer_stack.layers[idx].mask = Some(LayerMask::new_white(16, 16));
        canvas.layer_stack.layers[idx].paint_target = PaintTarget::Mask;

        // Soft brush, low opacity/flow, painting black onto a white mask.
        let settings = BrushSettings {
            size: 8.0,
            hardness: 0.0,
            opacity: 0.5,
            flow: 0.5,
            color: [0, 0, 0, 255],
            ..BrushSettings::default()
        };
        let value = |canvas: &Canvas| {
            let mask = canvas.layer_stack.layers[idx].mask.as_ref().unwrap();
            mask.tiles.get_pixel(8, 8).0
        };

        let start = value(&canvas);
        BrushTool::paint_cpu_dab(&settings, &mut canvas, 8.0, 8.0);
        let after_one = value(&canvas);
        for _ in 0..20 {
            BrushTool::paint_cpu_dab(&settings, &mut canvas, 8.0, 8.0);
        }
        let after_many = value(&canvas);

        assert!(after_one < start, "first dab darkens the mask");
        assert!(
            after_many < after_one,
            "repeated dabs must keep building coverage ({after_many} !< {after_one})"
        );
        assert!(
            after_many <= 5,
            "soft low-flow paint eventually covers, got {after_many}"
        );
    }

    #[test]
    fn stroke_opacity_caps_within_one_stroke_but_stacks_across_strokes() {
        let mut canvas = Canvas::new_blank(16, 16);
        let idx = canvas.layer_stack.add_layer(16, 16); // transparent layer
        canvas.layer_stack.active_idx = idx;

        let settings = BrushSettings {
            size: 8.0,
            hardness: 1.0,
            opacity: 0.5,
            flow: 1.0,
            color: [255, 0, 0, 255],
            ..BrushSettings::default()
        };
        let alpha_at = |canvas: &Canvas| canvas.layer_stack.layers[idx].tiles.get_pixel(8, 8).3;

        let mut stroke = StrokeBuffer::begin(&canvas).expect("paintable layer");
        for _ in 0..15 {
            BrushTool::paint_cpu_dab_stroked(&settings, &mut canvas, 8.0, 8.0, &mut stroke);
        }
        drop(stroke);
        let one_stroke = alpha_at(&canvas);
        assert!(
            (126..=129).contains(&one_stroke),
            "overlapping dabs in ONE 50% stroke must cap at ~50% alpha, got {one_stroke}"
        );

        let mut stroke2 = StrokeBuffer::begin(&canvas).expect("paintable layer");
        for _ in 0..15 {
            BrushTool::paint_cpu_dab_stroked(&settings, &mut canvas, 8.0, 8.0, &mut stroke2);
        }
        let two_strokes = alpha_at(&canvas);
        assert!(
            two_strokes >= 187,
            "a SECOND 50% stroke must stack on the first, got {two_strokes}"
        );
    }

    #[test]
    fn dab_layout_is_independent_of_event_chopping() {
        let settings = BrushSettings {
            size: 8.0,
            hardness: 0.5,
            opacity: 0.6,
            flow: 0.5,
            ..BrushSettings::default()
        };
        // Paint the same straight path chopped into different event segments.
        let paint = |splits: &[(f32, f32)]| -> Vec<u8> {
            let mut canvas = Canvas::new_blank(64, 16);
            let mut prev = (4.0, 8.0);
            BrushTool::paint_cpu_dab(&settings, &mut canvas, prev.0, prev.1);
            let mut residual = settings.dab_spacing();
            for &(x, y) in splits {
                BrushTool::paint_cpu_stroke_segment(
                    &settings,
                    &mut canvas,
                    prev.0,
                    prev.1,
                    x,
                    y,
                    &mut residual,
                );
                prev = (x, y);
            }
            let idx = canvas.layer_stack.active_idx;
            canvas.layer_stack.layers[idx].tiles.flatten()
        };

        let one = paint(&[(60.0, 8.0)]);
        // Includes a segment shorter than the dab spacing (1.5 px < 2 px).
        let many = paint(&[(13.0, 8.0), (14.5, 8.0), (30.0, 8.0), (60.0, 8.0)]);

        assert_eq!(one.len(), many.len());
        let max_diff = one
            .iter()
            .zip(&many)
            .map(|(a, b)| (*a as i16 - *b as i16).unsigned_abs())
            .max()
            .unwrap_or(0);
        assert!(
            max_diff <= 2,
            "chopping the stroke into events changed the paint (max byte diff {max_diff})"
        );
    }

    #[test]
    fn half_radius_marks_the_fifty_percent_contour() {
        // Photoshop's soft tip is at 50 % on 0.55 of its radius.
        assert!((soft_round_half_radius(0.0) - 0.55).abs() < 0.01);
        assert!(soft_round_half_radius(1.0) > 0.99);
        let mut prev = 0.0;
        for i in 0..=10 {
            let h = i as f32 / 10.0;
            let t = soft_round_half_radius(h);
            assert!(t >= prev, "ring must grow with hardness");
            // A hard tip's 50 % point sits in its fixed-width AA edge,
            // which depends on the tip size; its ring is full size anyway.
            if h < 1.0 {
                let r = 200.0;
                let d = t * r;
                assert!((soft_round_alpha(d * d, r, h) - 0.5).abs() < 0.02);
            }
            prev = t;
        }
    }

    #[test]
    fn soft_round_is_radial_gradient_without_hard_plateau() {
        // Photoshop's bell: 10^-(t²), so 56 % at half the radius, 10 % on
        // it, and on to nothing past ~1.65 r.
        let r = 100.0_f32;
        let at = |t: f32| soft_round_alpha((r * t).powi(2), r, 0.0);
        assert!(at(0.0) > at(0.25));
        assert!(at(0.25) > at(0.5));
        assert!(at(0.5) > at(0.75));
        assert!(at(0.75) > at(1.0));
        assert!((at(0.5) - 0.562).abs() < 0.01, "{}", at(0.5));
        assert!((at(1.0) - 0.1).abs() < 0.01, "{}", at(1.0));
        assert_eq!(at(1.7), 0.0);
        assert!((tip_reach(r, 0.0) - 165.0).abs() < 1.0);
    }

    #[test]
    fn hard_round_keeps_crisp_core_with_antialiased_edge() {
        // Full to ~1.4 px inside the rim, a ~2 px edge, nothing 2.2 px out.
        let r = 100.0_f32;
        assert_eq!(soft_round_alpha((r - 1.5).powi(2), r, 1.0), 1.0);
        let rim = soft_round_alpha(r * r, r, 1.0);
        assert!(rim > 0.2 && rim < 0.6, "{rim}");
        assert_eq!(soft_round_alpha((r + 2.2).powi(2), r, 1.0), 0.0);
    }

    /// FNV-1a over the paint target's tiles in position order.
    fn paint_target_digest(canvas: &mut Canvas) -> u64 {
        let idx = canvas.layer_stack.active_idx;
        let Some(tiles) = canvas.layer_stack.layers[idx].get_paint_tiles_mut() else {
            return 0;
        };
        let mut keys: Vec<_> = tiles.tiles.keys().copied().collect();
        keys.sort_by_key(|p| (p.y, p.x));
        let mut h = 0xcbf2_9ce4_8422_2325u64;
        for pos in keys {
            for b in pos.x.to_le_bytes().iter().chain(&pos.y.to_le_bytes()) {
                h = (h ^ *b as u64).wrapping_mul(0x100_0000_01b3);
            }
            for &b in &tiles.tiles[&pos].pixels {
                h = (h ^ b as u64).wrapping_mul(0x100_0000_01b3);
            }
        }
        h
    }

    /// Speed and pixel digest of strokes across the paths of
    /// `paint_cpu_dab_impl` (buffered / direct, eraser, lock alpha, channel
    /// gate, layer mask, soft selection). The digests must not move when the
    /// code is restructured without meaning to change what it paints.
    ///
    /// `CARGO_PROFILE_RELEASE_LTO=off CARGO_PROFILE_RELEASE_CODEGEN_UNITS=16
    ///  cargo test --release --lib --target-dir target/probe
    ///  probe_brush_strokes -- --ignored --nocapture`
    #[test]
    #[ignore = "probe: prints timings and digests"]
    fn probe_brush_strokes() {
        let (w, h) = (3000u32, 2000u32);
        let fresh = || {
            let mut canvas = Canvas::new_blank(w, h);
            let idx = canvas.layer_stack.active_idx;
            if let Some(tiles) = canvas.layer_stack.layers[idx].get_paint_tiles_mut() {
                for y in (0..h).step_by(7) {
                    for x in (0..w).step_by(5) {
                        tiles.set_pixel(x, y, (x % 251) as u8, (y % 241) as u8, 90, 200);
                    }
                }
            }
            canvas
        };
        let stroke = |canvas: &mut Canvas, settings: &BrushSettings, buffered: bool| {
            let mut buf = if buffered {
                StrokeBuffer::begin(canvas)
            } else {
                None
            };
            let n = 400;
            let t = std::time::Instant::now();
            for k in 0..n {
                let f = k as f32 / n as f32;
                let x = 100.0 + f * (w as f32 - 200.0);
                let y = h as f32 * 0.5 + (f * 12.0).sin() * h as f32 * 0.3;
                match buf.as_mut() {
                    Some(s) => BrushTool::paint_cpu_dab_stroked(settings, canvas, x, y, s),
                    None => BrushTool::paint_cpu_dab(settings, canvas, x, y),
                }
            }
            t.elapsed()
        };
        let base = BrushSettings {
            hardness: 0.3,
            opacity: 0.8,
            flow: 0.6,
            color: [220, 40, 90, 255],
            ..BrushSettings::default()
        };
        for size in [20.0f32, 150.0, 600.0] {
            let settings = BrushSettings {
                size,
                ..base.clone()
            };
            let cases: [(&str, fn(&mut Canvas), BrushSettings, bool); 7] = [
                ("buffered", |_| {}, settings.clone(), true),
                ("direct", |_| {}, settings.clone(), false),
                (
                    "eraser",
                    |_| {},
                    BrushSettings {
                        is_eraser: true,
                        ..settings.clone()
                    },
                    true,
                ),
                (
                    "lock alpha",
                    |c| {
                        let i = c.layer_stack.active_idx;
                        c.layer_stack.layers[i].lock_alpha = true;
                    },
                    settings.clone(),
                    true,
                ),
                (
                    "channel gate",
                    |c| c.channels.select_color(1, false),
                    settings.clone(),
                    true,
                ),
                (
                    "layer mask",
                    |c| {
                        let i = c.layer_stack.active_idx;
                        c.layer_stack.layers[i].add_mask(true);
                        c.layer_stack.layers[i].paint_target =
                            crate::core::layer::PaintTarget::Mask;
                    },
                    settings.clone(),
                    true,
                ),
                (
                    "soft selection",
                    |c| {
                        let (cw, ch) = (c.width, c.height);
                        c.selection.select_rect(0, 0, cw, ch);
                        for y in 0..ch {
                            for x in 0..cw {
                                c.selection.mask[(y * cw + x) as usize] =
                                    ((x / 3 + y / 5) % 256) as u8;
                            }
                        }
                    },
                    settings.clone(),
                    true,
                ),
            ];
            for (name, setup, s, buffered) in cases {
                let mut canvas = fresh();
                setup(&mut canvas);
                let mut best = std::time::Duration::MAX;
                let mut digest = 0;
                for _ in 0..3 {
                    let mut c = fresh();
                    setup(&mut c);
                    best = best.min(stroke(&mut c, &s, buffered));
                    digest = paint_target_digest(&mut c);
                }
                stroke(&mut canvas, &s, buffered);
                assert_eq!(digest, paint_target_digest(&mut canvas));
                println!(
                    "brush size {size:>4} {name:<15} best of 3: {:>8.2} ms  digest {digest:016x}",
                    best.as_secs_f64() * 1e3
                );
            }
        }
    }
}
