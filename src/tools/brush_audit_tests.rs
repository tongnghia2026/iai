//! Tests of the brush audit (plan `KE_HOACH_AUDIT_CO_VE_BRUSH_2026-11-16`):
//! Photoshop's behaviour for the Brush, Pencil and Eraser, tool by tool.

use super::brush::{BrushMode, RoundTip, StrokeBuffer};
use super::{PointerEvent, ToolCtx, ToolId, ToolManager};
use crate::core::blend::BlendMode;
use crate::core::canvas::Canvas;
use crate::core::document::{Document, DocumentId};

/// A document of `w`×`h` white Background and, on top, an active layer
/// filled with `fill` (straight RGBA).
fn doc_with_layer(w: u32, h: u32, fill: [u8; 4]) -> (Document, usize) {
    let mut canvas = Canvas::from_rgba(vec![255u8; (w * h * 4) as usize], w, h);
    let idx = canvas.layer_stack.add_layer(w, h);
    canvas.layer_stack.active_idx = idx;
    if fill[3] > 0
        && let Some(tiles) = canvas.layer_stack.layers[idx].get_paint_tiles_mut()
    {
        for y in 0..h {
            for x in 0..w {
                tiles.set_pixel(x, y, fill[0], fill[1], fill[2], fill[3]);
            }
        }
    }
    (Document::from_canvas(DocumentId(1), canvas, None), idx)
}

fn ev(x: f32, y: f32) -> PointerEvent {
    PointerEvent::new(x, y)
}

fn shift_ev(x: f32, y: f32) -> PointerEvent {
    PointerEvent {
        shift: true,
        ..PointerEvent::new(x, y)
    }
}

/// Press at the first point, drag through the rest a pixel at a time,
/// release at the last.
fn stroke(tools: &mut ToolManager, doc: &mut Document, pts: &[PointerEvent]) {
    let Some(first) = pts.first() else {
        return;
    };
    let mut events = vec![*first];
    let mut last = *first;
    for p in &pts[1..] {
        let n = ((p.canvas_x - last.canvas_x).abs())
            .max((p.canvas_y - last.canvas_y).abs())
            .ceil()
            .max(1.0) as usize;
        for k in 1..=n {
            let f = k as f32 / n as f32;
            events.push(PointerEvent {
                canvas_x: last.canvas_x + (p.canvas_x - last.canvas_x) * f,
                canvas_y: last.canvas_y + (p.canvas_y - last.canvas_y) * f,
                ..*p
            });
        }
        last = *p;
    }
    stroke_events(tools, doc, &events);
}

/// Press on the first event, a drag for each of the others as given,
/// release on the last.
fn stroke_events(tools: &mut ToolManager, doc: &mut Document, events: &[PointerEvent]) {
    let (Some(first), Some(last)) = (events.first(), events.last()) else {
        return;
    };
    {
        let mut ctx = ToolCtx::new(doc, [0, 0, 0, 255], [255, 255, 255, 255], 1.0, 0.0, 0.0);
        tools.on_press(*first, &mut ctx);
        for e in &events[1..] {
            tools.on_drag(*e, &mut ctx);
        }
        tools.on_release(*last, &mut ctx);
    }
    doc.canvas.end_stroke();
}

fn px(doc: &Document, idx: usize, x: u32, y: u32) -> (u8, u8, u8, u8) {
    doc.canvas.layer_stack.layers[idx].tiles.get_pixel(x, y)
}

fn brush(size: f32, hardness: f32) -> ToolManager {
    let mut tools = ToolManager::new();
    tools.select(ToolId::Brush);
    let s = &mut tools.brush_mut().settings;
    s.size = size;
    s.hardness = hardness;
    s.color = [0, 0, 0, 255];
    tools
}

#[test]
fn the_soft_tip_is_photoshops_bell() {
    // Measured from Photoshop 2020: a soft tip is 10^-(t^2), 10 % at its
    // radius, 50 % at 0.55 r, and reaches on to ~1.65 r.
    let tip = RoundTip::new(500.0, 0.0);
    for t in [0.0f32, 0.25, 0.55, 1.0, 1.4] {
        let want = 10f32.powf(-t * t);
        let got = tip.alpha_at(t * 500.0);
        assert!((got - want).abs() < 0.01, "t {t}: {got} vs {want}");
    }
    assert!((tip.reach() / 500.0 - 1.65).abs() < 0.02);
    // Hardness 50: the 50 % contour at 0.88 r (Photoshop: 0.877 – 0.882).
    let half = RoundTip::new(500.0, 0.5);
    let t50 = (0..2000)
        .map(|i| i as f32 / 1000.0)
        .find(|&t| half.alpha_at(t * 500.0) < 0.5)
        .unwrap();
    assert!((t50 - 0.88).abs() < 0.01, "{t50}");
    // A hard tip: full to ~1.4 px inside its rim, a ~2 px edge, nothing
    // past 2.1 px outside.
    let hard = RoundTip::new(100.0, 1.0);
    assert_eq!(hard.alpha_at(98.5), 1.0);
    assert!((hard.alpha_at(99.85) - 0.5).abs() < 0.1);
    assert_eq!(hard.alpha_at(102.2), 0.0);
}

#[test]
fn a_tiny_hard_tip_paints_soft_like_photoshops() {
    // 1–3 px tips paint the same hard or soft.
    let (soft, hard) = (RoundTip::new(1.5, 0.0), RoundTip::new(1.5, 1.0));
    for d in [0.0f32, 0.7, 1.4, 2.5] {
        assert_eq!(soft.alpha_at(d), hard.alpha_at(d));
    }
}

#[test]
fn a_layer_locked_whole_takes_no_paint() {
    for tool in [ToolId::Brush, ToolId::Pencil, ToolId::Eraser] {
        let (mut doc, idx) = doc_with_layer(40, 40, [10, 200, 30, 255]);
        doc.canvas.layer_stack.layers[idx].locked = true;
        assert!(StrokeBuffer::begin(&doc.canvas).is_none());
        let mut tools = brush(12.0, 1.0);
        tools.select(tool);
        stroke(&mut tools, &mut doc, &[ev(10.0, 20.0), ev(30.0, 20.0)]);
        assert_eq!(px(&doc, idx, 20, 20), (10, 200, 30, 255), "{tool:?}");
    }
}

#[test]
fn the_eraser_paints_the_background_colour_on_locked_transparency() {
    let (mut doc, idx) = doc_with_layer(40, 40, [10, 200, 30, 255]);
    doc.canvas.layer_stack.layers[idx].lock_alpha = true;
    let mut tools = ToolManager::new();
    tools.select(ToolId::Eraser);
    tools.eraser_mut().size = 12.0;
    tools.eraser_mut().hardness = 1.0;
    tools.eraser_mut().bg_color = [250, 240, 230, 255];
    stroke(&mut tools, &mut doc, &[ev(10.0, 20.0), ev(30.0, 20.0)]);
    assert_eq!(px(&doc, idx, 20, 20), (250, 240, 230, 255));
    // Without the lock it erases to transparency.
    let (mut doc, idx) = doc_with_layer(40, 40, [10, 200, 30, 255]);
    stroke(&mut tools, &mut doc, &[ev(10.0, 20.0), ev(30.0, 20.0)]);
    assert_eq!(px(&doc, idx, 20, 20).3, 0);
}

#[test]
fn shift_click_draws_a_line_from_where_the_last_stroke_ended() {
    let (mut doc, idx) = doc_with_layer(80, 40, [0, 0, 0, 0]);
    let mut tools = brush(6.0, 1.0);
    stroke(&mut tools, &mut doc, &[ev(10.0, 20.0)]);
    assert_eq!(px(&doc, idx, 40, 20).3, 0);
    stroke(&mut tools, &mut doc, &[shift_ev(70.0, 20.0)]);
    for x in [10, 25, 40, 55, 70] {
        assert_eq!(px(&doc, idx, x, 20).3, 255, "x {x}");
    }
    // One stroke, one undo step.
    doc.canvas.undo();
    assert_eq!(px(&doc, idx, 40, 20).3, 0);
    assert_eq!(px(&doc, idx, 10, 20).3, 255);
}

#[test]
fn shift_drag_keeps_to_the_horizontal() {
    let (mut doc, idx) = doc_with_layer(80, 60, [0, 0, 0, 0]);
    let mut tools = brush(8.0, 1.0);
    stroke(
        &mut tools,
        &mut doc,
        &[
            shift_ev(10.0, 30.0),
            shift_ev(40.0, 33.0),
            shift_ev(70.0, 45.0),
        ],
    );
    assert_eq!(px(&doc, idx, 69, 30).3, 255);
    assert_eq!(px(&doc, idx, 70, 45).3, 0, "the drag kept to y = 30");
}

#[test]
fn modes_mix_the_paint_with_what_is_under_it() {
    let paint = |mode: BrushMode, under: [u8; 4], colour: [u8; 4]| {
        let (mut doc, idx) = doc_with_layer(30, 30, under);
        let mut tools = brush(20.0, 1.0);
        tools.brush_mut().settings.mode = mode;
        tools.brush_mut().settings.color = colour;
        stroke(&mut tools, &mut doc, &[ev(15.0, 15.0)]);
        px(&doc, idx, 15, 15)
    };
    let grey = [128, 128, 128, 255];
    // Multiply: 50 % red over mid grey → darker red.
    let (r, g, b, a) = paint(
        BrushMode::Blend(BlendMode::Multiply),
        grey,
        [255, 128, 128, 255],
    );
    assert_eq!((r, a), (128, 255));
    assert!((63..=65).contains(&g) && (63..=65).contains(&b), "{g} {b}");
    // Screen lightens.
    let (r, ..) = paint(BrushMode::Blend(BlendMode::Screen), grey, [128, 0, 0, 255]);
    assert!(r > 180, "{r}");
    // On a see-through pixel every blend mode paints Normal.
    let (r, g, b, a) = paint(
        BrushMode::Blend(BlendMode::Multiply),
        [0, 0, 0, 0],
        [200, 10, 10, 255],
    );
    assert_eq!((r, g, b, a), (200, 10, 10, 255));
    // Behind: an opaque pixel keeps its colour; a see-through one takes paint.
    assert_eq!(
        paint(BrushMode::Behind, grey, [255, 0, 0, 255]),
        (128, 128, 128, 255)
    );
    assert_eq!(
        paint(BrushMode::Behind, [0, 0, 0, 0], [255, 0, 0, 255]),
        (255, 0, 0, 255)
    );
    // Clear paints transparency.
    assert_eq!(paint(BrushMode::Clear, grey, [255, 0, 0, 255]).3, 0);
}

#[test]
fn smoothing_ends_the_stroke_where_the_pointer_let_go() {
    let (mut doc, idx) = doc_with_layer(120, 40, [0, 0, 0, 0]);
    let mut tools = brush(8.0, 1.0);
    tools.brush_mut().settings.smoothing = 1.0;
    stroke(&mut tools, &mut doc, &[ev(10.0, 20.0), ev(110.0, 20.0)]);
    // An 80 px string would have left the paint 80 px short; the release
    // finishes it.
    assert_eq!(px(&doc, idx, 108, 20).3, 255);
}

#[test]
fn smoothing_draws_the_same_however_often_the_mouse_reports() {
    let draw = |step: f32| {
        let (mut doc, idx) = doc_with_layer(160, 100, [0, 0, 0, 0]);
        let mut tools = brush(6.0, 0.5);
        tools.brush_mut().settings.smoothing = 0.5;
        let mut pts = Vec::new();
        let mut t = 0.0f32;
        while t <= 1.0 {
            let x = 10.0 + 140.0 * t;
            let y = 50.0 + 35.0 * (t * 9.0).sin();
            pts.push(ev(x, y));
            t += step;
        }
        stroke_events(&mut tools, &mut doc, &pts);
        (0..100u32)
            .flat_map(|y| (0..160u32).map(move |x| (x, y)))
            .map(|(x, y)| px(&doc, idx, x, y).3 as u64)
            .sum::<u64>()
    };
    let (coarse, fine) = (draw(1.0 / 40.0), draw(1.0 / 400.0));
    let ratio = coarse as f64 / fine as f64;
    // The old per-event average cut corners twice as hard at the coarse
    // rate; the string gives the same stroke within a few percent.
    assert!((ratio - 1.0).abs() < 0.05, "{coarse} vs {fine}");
}

#[test]
fn the_airbrush_builds_up_while_the_pointer_rests() {
    let rest = |airbrush: bool| {
        let (mut doc, idx) = doc_with_layer(40, 40, [0, 0, 0, 0]);
        let mut tools = brush(20.0, 1.0);
        tools.brush_mut().settings.flow = 0.1;
        tools.brush_mut().airbrush = airbrush;
        let mut ctx = ToolCtx::new(&mut doc, [0, 0, 0, 255], [255; 4], 1.0, 0.0, 0.0);
        tools.on_press(ev(20.0, 20.0), &mut ctx);
        for _ in 0..6 {
            std::thread::sleep(std::time::Duration::from_millis(40));
            tools.on_frame(&mut ctx);
        }
        tools.on_release(ev(20.0, 20.0), &mut ctx);
        px(&doc, idx, 20, 20).3
    };
    let (still, air) = (rest(false), rest(true));
    assert!(still < 30, "{still}");
    assert!(air > still + 60, "airbrush {air} vs {still}");
}

#[test]
fn a_one_pixel_pencil_line_is_one_pixel_per_column() {
    let (mut doc, idx) = doc_with_layer(200, 80, [0, 0, 0, 0]);
    let mut tools = brush(1.0, 1.0);
    tools.select(ToolId::Pencil);
    stroke(&mut tools, &mut doc, &[ev(10.3, 10.7), ev(190.6, 70.2)]);
    for x in 10..=190u32 {
        let n = (0..80u32).filter(|&y| px(&doc, idx, x, y).3 > 0).count();
        assert_eq!(n, 1, "column {x}");
    }
    // Whole pixels only.
    assert!((0..80u32).all(|y| matches!(px(&doc, idx, 100, y).3, 0 | 255)));
}

#[test]
fn spacing_never_drops_under_a_pixel() {
    let s = super::brush::BrushSettings {
        size: 50.0,
        spacing: 0.01,
        ..Default::default()
    };
    assert_eq!(s.dab_spacing(), 1.0);
}

#[test]
fn scatter_throws_dabs_off_the_path_and_undoes_as_one_stroke() {
    let (mut doc, idx) = doc_with_layer(200, 120, [0, 0, 0, 0]);
    let mut tools = brush(8.0, 1.0);
    tools.brush_mut().dynamics.scatter = 4.0;
    tools.brush_mut().dynamics.size_jitter = 0.5;
    stroke(&mut tools, &mut doc, &[ev(20.0, 60.0), ev(180.0, 60.0)]);
    // A dab of 8 px on the path's line covers y 56..64: paint farther out
    // came from scatter, on both sides.
    let painted = |doc: &Document, ys: std::ops::Range<u32>| {
        ys.flat_map(|y| (0..200).map(move |x| (x, y)))
            .any(|(x, y)| px(doc, idx, x, y).3 > 0)
    };
    assert!(painted(&doc, 0..52), "dabs land above the path");
    assert!(painted(&doc, 68..120), "dabs land below the path");
    doc.canvas.undo();
    assert!(
        !painted(&doc, 0..120),
        "one undo takes the whole stroke away"
    );
}

#[test]
fn without_dynamics_the_stroke_stays_on_its_path() {
    let (mut doc, idx) = doc_with_layer(200, 120, [0, 0, 0, 0]);
    let mut tools = brush(8.0, 1.0);
    stroke(&mut tools, &mut doc, &[ev(20.0, 60.0), ev(180.0, 60.0)]);
    for x in (0..200).step_by(7) {
        assert_eq!(px(&doc, idx, x, 40).3, 0);
        assert_eq!(px(&doc, idx, x, 80).3, 0);
    }
    assert_eq!(px(&doc, idx, 100, 60).3, 255);
}

#[test]
fn glow_paints_a_faint_halo_round_the_stroke_and_undoes_with_it() {
    let (mut doc, idx) = doc_with_layer(160, 100, [0, 0, 0, 0]);
    let mut tools = brush(10.0, 1.0);
    tools.brush_mut().dynamics.glow = 1.0;
    stroke(&mut tools, &mut doc, &[ev(30.0, 50.0), ev(130.0, 50.0)]);
    let core = px(&doc, idx, 80, 50).3;
    // The 10 px dab reaches y 45..55; the glow (50 px) reaches past it.
    let halo = px(&doc, idx, 80, 60).3;
    assert_eq!(core, 255, "the stroke itself is painted as ever");
    assert!(halo > 0 && halo < 200, "a faint glow beside it: {halo}");
    assert_eq!(px(&doc, idx, 80, 92).3, 0, "nothing past the glow");
    doc.canvas.undo();
    assert_eq!(px(&doc, idx, 80, 60).3, 0, "one undo takes the glow too");
}
