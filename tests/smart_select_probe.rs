//! Manual probe: replays scripted Smart Select (W) drags on real photos and
//! writes overlay PNGs so the selection's growth can be inspected.
//!
//! Run: `SMART_SELECT_PROBE_OUT=<dir> cargo test --release --test smart_select_probe -- --ignored --nocapture`

use iai::core::document::{Document, DocumentId};
use iai::core::selection::SelectionMode;
use iai::tools::smart_select::SmartSelectTool;
use iai::tools::{PointerEvent, Tool, ToolCtx};

struct Stroke {
    points: &'static [(f32, f32)],
    mode: SelectionMode,
}

struct Scenario {
    name: &'static str,
    image: &'static str,
    radius: f32,
    strokes: &'static [Stroke],
}

const SCENARIOS: &[Scenario] = &[
    Scenario {
        name: "rock",
        image: "C:/Windows/Web/Wallpaper/Theme1/img1.jpg",
        radius: 30.0,
        strokes: &[Stroke {
            points: &[
                (1260.0, 600.0),
                (1380.0, 560.0),
                (1430.0, 470.0),
                (1500.0, 620.0),
                (1570.0, 680.0),
            ],
            mode: SelectionMode::Add,
        }],
    },
    Scenario {
        name: "runner",
        image: "C:/Windows/Web/Wallpaper/Theme1/img1.jpg",
        radius: 8.0,
        strokes: &[Stroke {
            points: &[(566.0, 715.0), (570.0, 760.0), (560.0, 800.0)],
            mode: SelectionMode::Add,
        }],
    },
    Scenario {
        name: "sky",
        image: "C:/Windows/Web/Wallpaper/Theme1/img1.jpg",
        radius: 30.0,
        strokes: &[Stroke {
            points: &[(150.0, 450.0), (500.0, 520.0), (900.0, 420.0)],
            mode: SelectionMode::Add,
        }],
    },
    Scenario {
        name: "leaf",
        image: "C:/Windows/Web/Wallpaper/Theme2/img12.jpg",
        radius: 30.0,
        strokes: &[Stroke {
            points: &[
                (1290.0, 200.0),
                (1310.0, 500.0),
                (1370.0, 850.0),
                (1430.0, 1040.0),
            ],
            mode: SelectionMode::Add,
        }],
    },
    Scenario {
        name: "redflower",
        image: "C:/Windows/Web/Wallpaper/Theme2/img7.jpg",
        radius: 30.0,
        strokes: &[
            Stroke {
                points: &[
                    (920.0, 200.0),
                    (1000.0, 350.0),
                    (1150.0, 380.0),
                    (1300.0, 240.0),
                ],
                mode: SelectionMode::Add,
            },
            Stroke {
                points: &[(700.0, 420.0), (960.0, 470.0), (990.0, 600.0)],
                mode: SelectionMode::Add,
            },
        ],
    },
    Scenario {
        name: "tulip",
        image: "C:/Windows/Web/Wallpaper/Theme2/img10.jpg",
        radius: 30.0,
        strokes: &[Stroke {
            points: &[
                (1300.0, 250.0),
                (1320.0, 420.0),
                (1360.0, 560.0),
                (1440.0, 300.0),
            ],
            mode: SelectionMode::Add,
        }],
    },
    Scenario {
        name: "cave_sand",
        image: "C:/Windows/Web/Screen/img100.jpg",
        radius: 120.0,
        strokes: &[Stroke {
            points: &[(1600.0, 1550.0), (2200.0, 1650.0), (2800.0, 1600.0)],
            mode: SelectionMode::Add,
        }],
    },
    Scenario {
        name: "cave_sky",
        image: "C:/Windows/Web/Screen/img100.jpg",
        radius: 60.0,
        strokes: &[Stroke {
            points: &[(2000.0, 700.0), (2300.0, 500.0), (2600.0, 900.0)],
            mode: SelectionMode::Add,
        }],
    },
    Scenario {
        name: "cave_dark",
        image: "C:/Windows/Web/Screen/img100.jpg",
        radius: 80.0,
        strokes: &[Stroke {
            points: &[(400.0, 1200.0), (600.0, 1800.0)],
            mode: SelectionMode::Add,
        }],
    },
    Scenario {
        name: "orangeflower",
        image: "C:/Windows/Web/Wallpaper/Theme2/img8.jpg",
        radius: 30.0,
        strokes: &[Stroke {
            points: &[
                (700.0, 300.0),
                (900.0, 450.0),
                (1150.0, 560.0),
                (1400.0, 600.0),
            ],
            mode: SelectionMode::Add,
        }],
    },
];

/// Mouse-move samples along the polyline, ~`step` px apart (a fast drag).
fn sample_polyline(points: &[(f32, f32)], step: f32) -> Vec<(f32, f32)> {
    let mut out = vec![points[0]];
    for pair in points.windows(2) {
        let (a, b) = (pair[0], pair[1]);
        let len = ((b.0 - a.0).powi(2) + (b.1 - a.1).powi(2)).sqrt();
        let n = (len / step).ceil().max(1.0) as usize;
        for i in 1..=n {
            let t = i as f32 / n as f32;
            out.push((a.0 + (b.0 - a.0) * t, a.1 + (b.1 - a.1) * t));
        }
    }
    out
}

fn overlay(rgba: &[u8], mask: &[u8], w: u32, h: u32, strokes: &[Vec<(f32, f32)>]) -> Vec<u8> {
    let mut out = vec![0u8; (w * h * 3) as usize];
    for i in 0..(w * h) as usize {
        let (r, g, b) = (
            rgba[i * 4] as f32,
            rgba[i * 4 + 1] as f32,
            rgba[i * 4 + 2] as f32,
        );
        let m = mask[i] as f32 / 255.0;
        let dim = 0.35 + 0.65 * m;
        out[i * 3] = (r * dim) as u8;
        out[i * 3 + 1] = (g * dim) as u8;
        out[i * 3 + 2] = (b * dim) as u8;
    }
    // Marching-ants style outline.
    for y in 1..h - 1 {
        for x in 1..w - 1 {
            let i = (y * w + x) as usize;
            let on = mask[i] >= 128;
            let edge = [i - 1, i + 1, i - w as usize, i + w as usize]
                .iter()
                .any(|&j| (mask[j] >= 128) != on);
            if on && edge {
                let c = if (x + y) / 4 % 2 == 0 { 255 } else { 0 };
                out[i * 3..i * 3 + 3].copy_from_slice(&[c, c, c]);
            }
        }
    }
    for s in strokes {
        for &(x, y) in s {
            for dy in -1i32..=1 {
                for dx in -1i32..=1 {
                    let px = x as i32 + dx;
                    let py = y as i32 + dy;
                    if px >= 0 && py >= 0 && (px as u32) < w && (py as u32) < h {
                        let i = (py as u32 * w + px as u32) as usize * 3;
                        out[i..i + 3].copy_from_slice(&[255, 40, 200]);
                    }
                }
            }
        }
    }
    out
}

#[test]
#[ignore = "manual probe: needs Windows wallpapers and an output dir"]
fn smart_select_drag_probe() {
    let out_dir = std::env::var("SMART_SELECT_PROBE_OUT").expect("set SMART_SELECT_PROBE_OUT");
    let only = std::env::var("SMART_SELECT_PROBE_ONLY").ok();
    std::fs::create_dir_all(&out_dir).unwrap();

    for sc in SCENARIOS {
        if only
            .as_deref()
            .is_some_and(|o| !o.split(',').any(|n| n == sc.name))
        {
            continue;
        }
        let img = image::open(sc.image).expect("open image").to_rgba8();
        let (w, h) = img.dimensions();
        let rgba = img.into_raw();
        let mut doc = Document::new(DocumentId(1), w, h);
        doc.canvas = iai::core::canvas::Canvas::from_rgba(rgba.clone(), w, h);

        let mut tool = SmartSelectTool::new();
        tool.brush_size = sc.radius;

        let mut drawn: Vec<Vec<(f32, f32)>> = Vec::new();
        let mut stamps_ms: Vec<f64> = Vec::new();
        let mut frame = 0usize;
        let t_all = std::time::Instant::now();
        for stroke in sc.strokes {
            let moves = sample_polyline(stroke.points, 14.0);
            drawn.push(moves.clone());
            let ev = |p: (f32, f32)| {
                let mut e = PointerEvent::new(p.0, p.1);
                e.selection_mode = stroke.mode;
                e
            };
            let mut prev = ev(moves[0]);
            {
                let mut ctx = ToolCtx::new(&mut doc, [0; 4], [255; 4], 1.0, 0.0, 0.0);
                let t = std::time::Instant::now();
                tool.on_press(prev, &mut ctx);
                stamps_ms.push(t.elapsed().as_secs_f64() * 1e3);
            }
            let snap_every = (moves.len() / 4).max(1);
            for (k, &p) in moves.iter().enumerate().skip(1) {
                let e = ev(p);
                let mut ctx = ToolCtx::new(&mut doc, [0; 4], [255; 4], 1.0, 0.0, 0.0);
                let t = std::time::Instant::now();
                tool.on_drag(e, &prev, &mut ctx);
                // Two pointer events per frame.
                if k % 2 == 0 || k + 1 == moves.len() {
                    tool.on_frame(&mut ctx);
                }
                stamps_ms.push(t.elapsed().as_secs_f64() * 1e3);
                prev = e;
                if k % snap_every == 0 {
                    let partial: Vec<Vec<(f32, f32)>> = {
                        let mut v = drawn.clone();
                        v.last_mut().unwrap().truncate(k + 1);
                        v
                    };
                    let png = overlay(&rgba, &doc.canvas.selection.mask, w, h, &partial);
                    let path = format!("{out_dir}/{}_f{frame:02}.png", sc.name);
                    let im = image::RgbImage::from_raw(w, h, png).unwrap();
                    image::imageops::thumbnail(&im, w / 2, h / 2)
                        .save(&path)
                        .unwrap();
                    frame += 1;
                }
            }
            let mut ctx = ToolCtx::new(&mut doc, [0; 4], [255; 4], 1.0, 0.0, 0.0);
            tool.on_release(prev, &mut ctx);
        }
        let total = t_all.elapsed().as_secs_f64() * 1e3;
        let mask = &doc.canvas.selection.mask;
        let selected = mask.iter().filter(|&&v| v >= 128).count();
        let png = overlay(&rgba, mask, w, h, &drawn);
        let im = image::RgbImage::from_raw(w, h, png).unwrap();
        im.save(format!("{out_dir}/{}_final.png", sc.name)).unwrap();
        image::imageops::thumbnail(&im, w / 2, h / 2)
            .save(format!("{out_dir}/{}_final_half.png", sc.name))
            .unwrap();
        let press_ms = stamps_ms[0];
        stamps_ms.remove(0);
        let max_ms = stamps_ms.iter().cloned().fold(0.0, f64::max);
        let avg_ms = stamps_ms.iter().sum::<f64>() / stamps_ms.len() as f64;
        println!(
            "{:<14} {}x{} selected={:>8} ({:.1}%) press={:.0}ms events={} avg={:.1}ms max={:.1}ms total={:.0}ms",
            sc.name,
            w,
            h,
            selected,
            100.0 * selected as f64 / (w * h) as f64,
            press_ms,
            stamps_ms.len(),
            avg_ms,
            max_ms,
            total
        );
    }
}
