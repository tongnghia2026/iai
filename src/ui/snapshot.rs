//! Test-only: draw a piece of the UI into an image without a window or a GPU,
//! to look at a layout. Fonts and theme are the app's; the triangles egui
//! emits are filled by a small software rasteriser.

use std::collections::HashMap;

use egui::epaint::{ImageData, Primitive};

/// The app's UI fonts as far as a layout needs them: the Vietnamese-capable
/// face, the bold one and the icons.
pub(crate) fn fonts() -> egui::FontDefinitions {
    let mut fonts = egui::FontDefinitions::default();
    if let Ok(data) = std::fs::read("C:/Windows/Fonts/segoeui.ttf") {
        fonts.font_data.insert(
            "ui_vietnamese".to_owned(),
            std::sync::Arc::new(egui::FontData::from_owned(data)),
        );
        for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
            if let Some(v) = fonts.families.get_mut(&family) {
                v.insert(0, "ui_vietnamese".to_owned());
            }
        }
    }
    egui_phosphor::add_to_fonts(&mut fonts, egui_phosphor::Variant::Regular);
    super::theme::add_bold_font(&mut fonts);
    fonts
}

/// Run `add` for a few frames on a `width × height` point screen at `scale`
/// pixels per point, the pointer resting at `hover` (points), and return the
/// last frame.
pub(crate) fn render(
    width: f32,
    height: f32,
    scale: f32,
    hover: Option<egui::Pos2>,
    mut add: impl FnMut(&egui::Context),
) -> image::RgbaImage {
    let ctx = egui::Context::default();
    ctx.set_fonts(fonts());
    super::theme::apply_theme(&ctx, super::theme::ThemeMode::Dark);
    ctx.set_pixels_per_point(scale);
    let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(width, height));
    let mut textures: HashMap<egui::TextureId, egui::ColorImage> = HashMap::new();
    let mut last = None;
    for _ in 0..4 {
        let mut input = egui::RawInput {
            screen_rect: Some(screen),
            ..Default::default()
        };
        if let Some(pos) = hover {
            input.events.push(egui::Event::PointerMoved(pos));
        }
        let output = ctx.run_ui(input, |ui| add(ui.ctx()));
        for (id, delta) in &output.textures_delta.set {
            let ImageData::Color(image) = &delta.image;
            match delta.pos {
                None => {
                    textures.insert(*id, (**image).clone());
                }
                Some([x0, y0]) => {
                    if let Some(atlas) = textures.get_mut(id) {
                        for y in 0..image.size[1] {
                            for x in 0..image.size[0] {
                                atlas.pixels[(y0 + y) * atlas.size[0] + x0 + x] =
                                    image.pixels[y * image.size[0] + x];
                            }
                        }
                    }
                }
            }
        }
        last = Some(output);
    }
    let output = last.expect("frames ran");
    let ppp = output.pixels_per_point;
    let (w, h) = ((width * ppp) as usize, (height * ppp) as usize);
    let fill = ctx.global_style().visuals.panel_fill;
    // Premultiplied RGBA as f32.
    let mut canvas = vec![[fill.r() as f32, fill.g() as f32, fill.b() as f32, 255.0]; w * h];
    for clipped in ctx.tessellate(output.shapes, ppp) {
        let Primitive::Mesh(mesh) = clipped.primitive else {
            continue;
        };
        let texture = textures.get(&mesh.texture_id);
        let clip = clipped.clip_rect;
        let (cx0, cy0) = (
            (clip.min.x * ppp).max(0.0) as usize,
            (clip.min.y * ppp).max(0.0) as usize,
        );
        let (cx1, cy1) = (
            ((clip.max.x * ppp).ceil() as usize).min(w),
            ((clip.max.y * ppp).ceil() as usize).min(h),
        );
        for tri in mesh.indices.chunks_exact(3) {
            let v = [
                mesh.vertices[tri[0] as usize],
                mesh.vertices[tri[1] as usize],
                mesh.vertices[tri[2] as usize],
            ];
            let p = v.map(|v| [v.pos.x * ppp, v.pos.y * ppp]);
            let area = (p[1][0] - p[0][0]) * (p[2][1] - p[0][1])
                - (p[2][0] - p[0][0]) * (p[1][1] - p[0][1]);
            if area.abs() < 1e-6 {
                continue;
            }
            let min_x = p.iter().map(|q| q[0]).fold(f32::MAX, f32::min).floor();
            let max_x = p.iter().map(|q| q[0]).fold(f32::MIN, f32::max).ceil();
            let min_y = p.iter().map(|q| q[1]).fold(f32::MAX, f32::min).floor();
            let max_y = p.iter().map(|q| q[1]).fold(f32::MIN, f32::max).ceil();
            let (x0, x1) = (
                (min_x.max(0.0) as usize).max(cx0),
                (max_x as usize).min(cx1),
            );
            let (y0, y1) = (
                (min_y.max(0.0) as usize).max(cy0),
                (max_y as usize).min(cy1),
            );
            for y in y0..y1 {
                for x in x0..x1 {
                    let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
                    let edge = |a: [f32; 2], b: [f32; 2]| {
                        (b[0] - a[0]) * (py - a[1]) - (b[1] - a[1]) * (px - a[0])
                    };
                    let bary = [
                        edge(p[1], p[2]) / area,
                        edge(p[2], p[0]) / area,
                        edge(p[0], p[1]) / area,
                    ];
                    if bary.iter().any(|&b| b < -1e-4) {
                        continue;
                    }
                    let mix = |f: &dyn Fn(usize) -> f32| -> f32 {
                        bary[0] * f(0) + bary[1] * f(1) + bary[2] * f(2)
                    };
                    let mut src =
                        [0usize, 1, 2, 3].map(|c| mix(&|k| v[k].color.to_array()[c] as f32));
                    if let Some(tex) = texture {
                        let (u, t) = (mix(&|k| v[k].uv.x), mix(&|k| v[k].uv.y));
                        let tx = ((u * tex.size[0] as f32) as usize).min(tex.size[0] - 1);
                        let ty = ((t * tex.size[1] as f32) as usize).min(tex.size[1] - 1);
                        let texel = tex.pixels[ty * tex.size[0] + tx].to_array();
                        for c in 0..4 {
                            src[c] *= texel[c] as f32 / 255.0;
                        }
                    }
                    let dst = &mut canvas[y * w + x];
                    let keep = 1.0 - src[3] / 255.0;
                    for c in 0..4 {
                        dst[c] = src[c] + dst[c] * keep;
                    }
                }
            }
        }
    }
    image::RgbaImage::from_fn(w as u32, h as u32, |x, y| {
        let px = canvas[y as usize * w + x as usize];
        image::Rgba([
            px[0].round().clamp(0.0, 255.0) as u8,
            px[1].round().clamp(0.0, 255.0) as u8,
            px[2].round().clamp(0.0, 255.0) as u8,
            255,
        ])
    })
}
