//! SVG export (Phase 9(e)) — vector-first, for the web and cut plotters.
//!
//! The layer stack is split into z-ordered runs (`collect_svg_runs`): every
//! vector-representable layer is written as native `<path>` elements (crisp at
//! any scale, ready for a cutter) at its own depth, and each stretch of raster /
//! non-vector layers between them embeds as a transparent PNG. So a Path or Text
//! layer sitting BENEATH an image stays crisp instead of being baked flat — the
//! z-order is preserved by interleaving image and path runs. SVG's coordinate
//! system is the canvas' own (top-left origin, y-down), so no axis flip is needed.
//!
//! SVG is an sRGB format: CMYK and spot paints are written as their sRGB preview
//! (spot has no SVG colour space). A pure-vector document produces a file with no
//! embedded image at all.

use crate::core::canvas::Canvas;
use crate::core::cms::naive_cmyk_to_rgb;
use crate::core::print::{layer_vector_objects_srgb, PdfPaintColor, PdfVectorObject};
use crate::core::vector::path::PathData;
use crate::core::vector::style::{Gradient, GradientKind, LineCap, LineJoin};
use base64::Engine;

/// One z-ordered stretch of the document: a run of raster / non-vector layers
/// flattened into a transparent image, or a run of layers kept as native paths.
enum SvgRun {
    Raster(Vec<u8>),
    Vectors(Vec<PdfVectorObject>),
}

/// Partition the layer stack into z-ordered raster / vector runs (bottom-to-top).
/// Each vector-representable layer stays crisp at its own z-position; every other
/// layer joins a raster run flattened just for that stretch. This keeps a Path or
/// Text layer that sits BENEATH an image crisp instead of baking it into one flat
/// base (which is all the top-run-only PDF collector could do).
fn collect_svg_runs(canvas: &Canvas) -> Vec<SvgRun> {
    let mut runs: Vec<SvgRun> = Vec::new();
    let mut raster_ids: Vec<u32> = Vec::new();
    let n = canvas.layer_stack.layers.len();
    for i in 0..n {
        let layer = &canvas.layer_stack.layers[i];
        // Group containers carry no pixels of their own; their children are
        // classified individually. Hidden layers contribute nothing.
        if layer.is_group() || !canvas.layer_stack.is_effectively_visible(i) {
            continue;
        }
        match layer_vector_objects_srgb(canvas, i) {
            Some(objects) => {
                flush_raster_run(&mut runs, &mut raster_ids, canvas);
                match runs.last_mut() {
                    Some(SvgRun::Vectors(v)) => v.extend(objects),
                    _ => runs.push(SvgRun::Vectors(objects)),
                }
            }
            None => raster_ids.push(layer.id),
        }
    }
    flush_raster_run(&mut runs, &mut raster_ids, canvas);
    runs
}

/// Flatten the accumulated non-vector layer ids into one transparent raster run
/// (dropping it when it has no ink), then clear the accumulator. Only the run's
/// own leaf layers are shown; group containers stay visible so a run member
/// inside a plain group still composites with its group effect.
fn flush_raster_run(runs: &mut Vec<SvgRun>, raster_ids: &mut Vec<u32>, canvas: &Canvas) {
    if raster_ids.is_empty() {
        return;
    }
    let keep: std::collections::HashSet<u32> = raster_ids.drain(..).collect();
    let mut stack = canvas.layer_stack.clone();
    for layer in &mut stack.layers {
        if !layer.is_group() && !keep.contains(&layer.id) {
            layer.visible = false;
        }
    }
    let rgba = stack.flatten(canvas.width, canvas.height);
    if rgba.chunks_exact(4).any(|px| px[3] != 0) {
        runs.push(SvgRun::Raster(rgba));
    }
}

/// Build an SVG document (as a `String`) for `canvas`. The page is sized to the
/// document's physical dimensions (mm from its DPI) with a pixel `viewBox`, so it
/// drops into a cutter or browser at the right real-world size.
pub fn build_svg(canvas: &Canvas) -> Result<String, String> {
    let (w, h) = (canvas.width, canvas.height);
    if w == 0 || h == 0 {
        return Err("Empty canvas, cannot export SVG".into());
    }
    if !Canvas::fits_flat_buffer(w, h) {
        return Err("Canvas too large to export as SVG".into());
    }
    let dpi = canvas.metadata.resolution_ppi.max(1.0);
    let mm_w = w as f32 / dpi * 25.4;
    let mm_h = h as f32 / dpi * 25.4;

    // Z-ordered raster / vector runs (bottom-to-top). Every vector layer stays a
    // crisp path at its own depth; the raster runs between them embed as images,
    // so a vector below an image no longer gets baked flat and lost.
    let runs = collect_svg_runs(canvas);

    let mut defs = String::new();
    let mut body = String::new();
    // Gradient defs need ids unique across the whole document, not per run.
    let mut grad_index = 0usize;

    for run in &runs {
        match run {
            SvgRun::Raster(rgba) => {
                let png = rgba_to_png(rgba, w, h)?;
                let b64 = base64::engine::general_purpose::STANDARD.encode(&png);
                body.push_str(&format!(
                    "  <image x=\"0\" y=\"0\" width=\"{w}\" height=\"{h}\" \
                     preserveAspectRatio=\"none\" xlink:href=\"data:image/png;base64,{b64}\"/>\n"
                ));
            }
            SvgRun::Vectors(objects) => {
                for object in objects {
                    append_svg_object(&mut defs, &mut body, grad_index, object);
                    grad_index += 1;
                }
            }
        }
    }

    let mut svg = String::new();
    svg.push_str("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
    svg.push_str(&format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" xmlns:xlink=\"http://www.w3.org/1999/xlink\" \
         width=\"{mm_w:.3}mm\" height=\"{mm_h:.3}mm\" viewBox=\"0 0 {w} {h}\">\n"
    ));
    if !defs.is_empty() {
        svg.push_str("  <defs>\n");
        svg.push_str(&defs);
        svg.push_str("  </defs>\n");
    }
    svg.push_str(&body);
    svg.push_str("</svg>\n");
    Ok(svg)
}

fn append_svg_object(defs: &mut String, body: &mut String, index: usize, o: &PdfVectorObject) {
    let d = svg_path_d(&o.path);
    if d.is_empty() {
        return;
    }

    let fill_attr = if let Some(gradient) = o.fill_gradient {
        append_svg_gradient(defs, index, &gradient);
        format!("fill=\"url(#IaiGrad{index})\"")
    } else if let Some(color) = o.fill {
        format!("fill=\"{}\"", svg_color(color))
    } else {
        "fill=\"none\"".to_string()
    };
    let fill_rule = if o.even_odd {
        " fill-rule=\"evenodd\""
    } else {
        ""
    };

    let mut stroke = String::new();
    if let (Some(color), true) = (o.stroke, o.stroke_width_px > 0.0) {
        stroke.push_str(&format!(
            " stroke=\"{}\" stroke-width=\"{:.3}\" stroke-linecap=\"{}\" stroke-linejoin=\"{}\"",
            svg_color(color),
            o.stroke_width_px,
            svg_cap(o.stroke_cap),
            svg_join(o.stroke_join),
        ));
        if matches!(o.stroke_join, LineJoin::Miter) {
            stroke.push_str(&format!(
                " stroke-miterlimit=\"{:.3}\"",
                o.stroke_miter_limit.max(1.0)
            ));
        }
        if !o.stroke_dash.is_empty() {
            let dash = o
                .stroke_dash
                .iter()
                .map(|v| format!("{v:.3}"))
                .collect::<Vec<_>>()
                .join(",");
            stroke.push_str(&format!(" stroke-dasharray=\"{dash}\""));
            if o.stroke_dash_offset.abs() > 1e-4 {
                stroke.push_str(&format!(
                    " stroke-dashoffset=\"{:.3}\"",
                    o.stroke_dash_offset
                ));
            }
        }
    }

    body.push_str(&format!(
        "  <path d=\"{d}\" {fill_attr}{fill_rule}{stroke}/>\n"
    ));
}

/// Path geometry as an SVG `d` string (absolute `M`/`L`/`C`/`Z`), in canvas
/// pixels — SVG shares the canvas' top-left, y-down space, so no transform.
fn svg_path_d(path: &PathData) -> String {
    let mut d = String::new();
    for contour in &path.contours {
        if contour.nodes.is_empty() {
            continue;
        }
        let node_count = contour.nodes.len();
        let first = contour.nodes[0].anchor;
        if !d.is_empty() {
            d.push(' ');
        }
        d.push_str(&format!("M{:.3} {:.3}", first.x, first.y));
        for segment in 0..contour.segment_count() {
            let Some((_p0, p1, p2, p3)) = contour.segment(segment) else {
                continue;
            };
            let straight = contour.nodes[segment].out_handle.is_none()
                && contour.nodes[(segment + 1) % node_count]
                    .in_handle
                    .is_none();
            if straight {
                d.push_str(&format!(" L{:.3} {:.3}", p3.x, p3.y));
            } else {
                d.push_str(&format!(
                    " C{:.3} {:.3} {:.3} {:.3} {:.3} {:.3}",
                    p1.x, p1.y, p2.x, p2.y, p3.x, p3.y
                ));
            }
        }
        if contour.closed {
            d.push_str(" Z");
        }
    }
    d
}

/// A resolved PDF paint as an SVG `#rrggbb` colour. CMYK and spot inks are shown
/// through their sRGB preview (SVG has no CMYK / spot colour space).
fn svg_color(color: PdfPaintColor) -> String {
    let q = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    let [r, g, b] = match color {
        PdfPaintColor::Rgb(rgb) => [q(rgb[0]), q(rgb[1]), q(rgb[2])],
        PdfPaintColor::Cmyk(c) => naive_cmyk_to_rgb([q(c[0]), q(c[1]), q(c[2]), q(c[3])]),
        PdfPaintColor::Spot { alt, tint, .. } => naive_cmyk_to_rgb([
            q(alt[0] * tint),
            q(alt[1] * tint),
            q(alt[2] * tint),
            q(alt[3] * tint),
        ]),
    };
    format!("#{r:02X}{g:02X}{b:02X}")
}

fn svg_cap(cap: LineCap) -> &'static str {
    match cap {
        LineCap::Butt => "butt",
        LineCap::Round => "round",
        LineCap::Square => "square",
    }
}

fn svg_join(join: LineJoin) -> &'static str {
    match join {
        LineJoin::Miter => "miter",
        LineJoin::Round => "round",
        LineJoin::Bevel => "bevel",
    }
}

/// Emit a `<linearGradient>` / `<radialGradient>` for `gradient` into `defs`. The
/// object's gradient transform already maps unit gradient space into canvas
/// pixels, so `gradientUnits="userSpaceOnUse"` with the unit geometry + that
/// matrix reproduces the exact placement (matching the PDF shading).
fn append_svg_gradient(defs: &mut String, index: usize, gradient: &Gradient) {
    let t = gradient.transform;
    let (tag, geom) = match gradient.kind {
        GradientKind::Linear => ("linearGradient", "x1=\"0\" y1=\"0\" x2=\"1\" y2=\"0\""),
        GradientKind::Radial => ("radialGradient", "cx=\"0\" cy=\"0\" r=\"1\""),
    };
    defs.push_str(&format!(
        "    <{tag} id=\"IaiGrad{index}\" gradientUnits=\"userSpaceOnUse\" {geom} \
         gradientTransform=\"matrix({:.6} {:.6} {:.6} {:.6} {:.6} {:.6})\">\n",
        t.a, t.b, t.c, t.d, t.e, t.f
    ));
    for stop in gradient.active_stops() {
        let [r, g, b, a] = stop.color.to_rgba8();
        defs.push_str(&format!(
            "      <stop offset=\"{:.4}\" stop-color=\"#{r:02X}{g:02X}{b:02X}\" \
             stop-opacity=\"{:.4}\"/>\n",
            stop.offset.clamp(0.0, 1.0),
            a as f32 / 255.0
        ));
    }
    defs.push_str(&format!("    </{tag}>\n"));
}

/// Encode straight-alpha RGBA8 as an in-memory PNG for embedding.
fn rgba_to_png(pixels: &[u8], w: u32, h: u32) -> Result<Vec<u8>, String> {
    let mut buf = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut buf, w, h);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().map_err(|e| e.to_string())?;
        writer.write_image_data(pixels).map_err(|e| e.to_string())?;
    }
    Ok(buf)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::command_vector::apply_object_to_layer;
    use crate::core::geometry::Point;
    use crate::core::vector::affine::AffineTransform;
    use crate::core::vector::color::ColorValue;
    use crate::core::vector::object::VectorObjectData;
    use crate::core::vector::path::{Contour, FillRule, Node, PathData};
    use crate::core::vector::style::VectorStyle;

    fn square(x0: f32, y0: f32, x1: f32, y1: f32) -> PathData {
        PathData::new(
            vec![Contour::new(
                vec![
                    Node::sharp(Point::new(x0, y0)),
                    Node::sharp(Point::new(x1, y0)),
                    Node::sharp(Point::new(x1, y1)),
                    Node::sharp(Point::new(x0, y1)),
                ],
                true,
            )],
            FillRule::NonZero,
        )
    }

    #[test]
    fn pure_vector_document_emits_paths_and_no_image() {
        // A transparent canvas with one red vector square = pure vector art.
        let mut canvas = crate::core::canvas::Canvas::from_rgba(vec![0; 40 * 40 * 4], 40, 40);
        let index = canvas.add_layer();
        apply_object_to_layer(
            &mut canvas.layer_stack.layers[index],
            VectorObjectData::new(
                square(5.0, 5.0, 35.0, 35.0),
                VectorStyle::filled(ColorValue::rgb(1.0, 0.0, 0.0)),
                AffineTransform::IDENTITY,
            ),
        );
        let svg = build_svg(&canvas).expect("svg");
        assert!(svg.contains("<svg"), "has an svg root");
        assert!(svg.contains("viewBox=\"0 0 40 40\""), "pixel viewBox");
        assert!(
            svg.contains("<path d=\"M5.000 5.000"),
            "vector path emitted"
        );
        assert!(svg.contains("fill=\"#FF0000\""), "red fill as sRGB hex");
        assert!(
            !svg.contains("<image"),
            "a fully vector document embeds no raster"
        );
        assert!(svg.ends_with("</svg>\n"));
    }

    #[test]
    fn raster_underneath_is_embedded_as_a_png() {
        // Opaque white background (raster) with a vector square on top.
        let mut canvas = crate::core::canvas::Canvas::from_rgba(vec![255; 24 * 24 * 4], 24, 24);
        let index = canvas.add_layer();
        apply_object_to_layer(
            &mut canvas.layer_stack.layers[index],
            VectorObjectData::new(
                square(4.0, 4.0, 20.0, 20.0),
                VectorStyle::filled(ColorValue::rgb(0.0, 0.0, 1.0)),
                AffineTransform::IDENTITY,
            ),
        );
        let svg = build_svg(&canvas).expect("svg");
        assert!(
            svg.contains("<image") && svg.contains("data:image/png;base64,"),
            "the raster background is embedded"
        );
        assert!(
            svg.contains("fill=\"#0000FF\""),
            "vector stays a crisp path"
        );
    }

    #[test]
    fn vector_below_an_image_stays_a_crisp_path_and_z_order_is_kept() {
        // Stack bottom→top: white raster bg, a red vector square, an opaque image
        // layer, then a blue vector square on top. The buried red vector must
        // survive as a crisp <path> (not baked into a flat base), and the emitted
        // order must be image, path, image, path (two interleaved runs).
        let mut canvas = crate::core::canvas::Canvas::from_rgba(vec![255; 30 * 30 * 4], 30, 30);
        let red = canvas.add_layer();
        apply_object_to_layer(
            &mut canvas.layer_stack.layers[red],
            VectorObjectData::new(
                square(2.0, 2.0, 12.0, 12.0),
                VectorStyle::filled(ColorValue::rgb(1.0, 0.0, 0.0)),
                AffineTransform::IDENTITY,
            ),
        );
        // An opaque raster layer above the red vector.
        let img = canvas.add_layer();
        canvas.layer_stack.layers[img].tiles =
            crate::core::tile::TileMap::from_rgba(&vec![200u8; 30 * 30 * 4], 30, 30);
        let blue = canvas.add_layer();
        apply_object_to_layer(
            &mut canvas.layer_stack.layers[blue],
            VectorObjectData::new(
                square(18.0, 18.0, 28.0, 28.0),
                VectorStyle::filled(ColorValue::rgb(0.0, 0.0, 1.0)),
                AffineTransform::IDENTITY,
            ),
        );

        let svg = build_svg(&canvas).expect("svg");
        assert!(
            svg.contains("fill=\"#FF0000\""),
            "the buried red vector stays a crisp path: {svg}"
        );
        assert!(svg.contains("fill=\"#0000FF\""), "the top blue vector too");
        // Two embedded images (bg+red-run raster below, and the image layer),
        // interleaved with the two vector paths, in z-order.
        let img_pos = svg
            .match_indices("<image")
            .map(|(i, _)| i)
            .collect::<Vec<_>>();
        let red_pos = svg.find("#FF0000").unwrap();
        let blue_pos = svg.find("#0000FF").unwrap();
        assert_eq!(img_pos.len(), 2, "one raster run per non-vector stretch");
        assert!(img_pos[0] < red_pos, "bg image before the red path");
        assert!(
            red_pos < img_pos[1],
            "red path before the upper image (z-order kept)"
        );
        assert!(img_pos[1] < blue_pos, "upper image before the blue path");
    }

    #[test]
    fn physical_size_follows_document_dpi() {
        // 96 px at 96 DPI = 1 inch = 25.4 mm.
        let mut canvas = crate::core::canvas::Canvas::from_rgba(vec![0; 96 * 96 * 4], 96, 96);
        canvas.metadata.resolution_ppi = 96.0;
        let svg = build_svg(&canvas).expect("svg");
        assert!(
            svg.contains("width=\"25.400mm\""),
            "physical width in mm: {svg}"
        );
        assert!(svg.contains("height=\"25.400mm\""));
    }
}
