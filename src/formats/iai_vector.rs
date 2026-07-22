//! `.iai` serialization for Path layers (Bước 4 / T4.3), kept OUT of the large
//! `iai.rs` per the pre-planned layout (Mục 3.10): `iai.rs` gains exactly one
//! dispatch branch on read and one on write.
//!
//! Envelope shape (Mục 3.12 — do NOT lock "one payload = one object"): the layer
//! payload is a LIST of objects, `{ "schema", "objects": [...] }`. MVP writes and
//! reads exactly one object, but a future multi-object-per-layer build can read
//! several WITHOUT a format-version bump. The stored geometry is object-local
//! `f32` (page-relative, Mục 3.13), so nothing here is tied to a canvas size.
//!
//! Only the MODEL is serialized — never selection, hover, cache generation or the
//! flattened polyline (Mục 5.2). The baked PNG at `layer_{i}.png` is a
//! preview/recovery fallback, not the source of truth (Mục 3.2 / 5.3).
//!
//! Parser safety (Mục 5.4): every decoded object is run through
//! [`VectorObjectData::validate`] (finite coords, node/contour limits) and the
//! object count is capped; anything out of bounds makes the whole payload decode
//! to `None`, so the layer falls back to its baked PNG rather than trusting a
//! malformed model.

use crate::core::geometry::Point;
use crate::core::vector::affine::AffineTransform;
use crate::core::vector::color::ColorValue;
use crate::core::vector::object::VectorObjectData;
use crate::core::vector::path::{Contour, FillRule, Node, NodeKind, PathData};
use crate::core::vector::style::{LineCap, LineJoin, Paint, StrokeStyle, VectorStyle};
use serde_json::{json, Value};

/// Payload schema version (independent of the `.iai` container version). Bumped
/// only if the Path payload shape itself changes incompatibly.
const PATH_SCHEMA: u64 = 1;
/// Max objects per Path layer accepted from an untrusted file.
const MAX_OBJECTS: usize = 100_000;

// ── Write ────────────────────────────────────────────────────────────────────

/// Serialize a Path layer's model to the manifest `"path"` value: a one-object
/// list under the versioned envelope.
pub fn layer_path_to_json(obj: &VectorObjectData) -> Value {
    json!({
        "schema": PATH_SCHEMA,
        "objects": [object_to_json(obj)],
    })
}

fn object_to_json(obj: &VectorObjectData) -> Value {
    json!({
        "fill_rule": fill_rule_str(obj.path.fill_rule),
        "transform": affine_to_json(&obj.transform),
        "style": style_to_json(&obj.style),
        "contours": obj.path.contours.iter().map(contour_to_json).collect::<Vec<_>>(),
    })
}

fn contour_to_json(c: &Contour) -> Value {
    json!({
        "closed": c.closed,
        "nodes": c.nodes.iter().map(node_to_json).collect::<Vec<_>>(),
    })
}

fn node_to_json(n: &Node) -> Value {
    json!({
        "anchor": pt_to_json(n.anchor),
        "in": n.in_handle.map(pt_to_json),
        "out": n.out_handle.map(pt_to_json),
        "kind": node_kind_u8(n.kind),
    })
}

fn style_to_json(s: &VectorStyle) -> Value {
    json!({
        "fill": paint_to_json(s.fill),
        "fill_overprint": s.fill_overprint,
        "stroke": paint_to_json(s.stroke),
        "stroke_style": stroke_style_to_json(&s.stroke_style),
        "stroke_overprint": s.stroke_overprint,
        "opacity": s.opacity,
    })
}

fn stroke_style_to_json(s: &StrokeStyle) -> Value {
    json!({
        "width": s.width,
        "cap": cap_u8(s.cap),
        "join": join_u8(s.join),
        "miter_limit": s.miter_limit,
    })
}

fn paint_to_json(p: Paint) -> Value {
    match p {
        Paint::None => json!({ "kind": "none" }),
        Paint::Solid(c) => json!({ "kind": "solid", "color": color_to_json(c) }),
    }
}

fn color_to_json(c: ColorValue) -> Value {
    match c {
        ColorValue::Rgb { r, g, b, a } => json!({ "space": "rgb", "v": [r, g, b, a] }),
        ColorValue::Cmyk { c, m, y, k, a } => json!({ "space": "cmyk", "v": [c, m, y, k, a] }),
    }
}

fn affine_to_json(t: &AffineTransform) -> Value {
    json!([t.a, t.b, t.c, t.d, t.e, t.f])
}

fn pt_to_json(p: Point) -> Value {
    json!([p.x, p.y])
}

fn fill_rule_str(r: FillRule) -> &'static str {
    match r {
        FillRule::NonZero => "nonzero",
        FillRule::EvenOdd => "evenodd",
    }
}

fn node_kind_u8(k: NodeKind) -> u8 {
    match k {
        NodeKind::Cusp => 0,
        NodeKind::Smooth => 1,
        NodeKind::Symmetric => 2,
    }
}

fn cap_u8(c: LineCap) -> u8 {
    match c {
        LineCap::Butt => 0,
        LineCap::Round => 1,
        LineCap::Square => 2,
    }
}

fn join_u8(j: LineJoin) -> u8 {
    match j {
        LineJoin::Miter => 0,
        LineJoin::Round => 1,
        LineJoin::Bevel => 2,
    }
}

// ── Read ─────────────────────────────────────────────────────────────────────

/// Decode a Path layer's model from the manifest `"path"` value. Returns the
/// first (MVP: only) object, or `None` if the payload is absent, malformed, over
/// a safety limit, or fails validation — in which case the caller keeps the baked
/// PNG fallback.
pub fn json_to_layer_path(v: &Value) -> Option<VectorObjectData> {
    let objects = v.get("objects")?.as_array()?;
    if objects.is_empty() || objects.len() > MAX_OBJECTS {
        return None;
    }
    let obj = json_to_object(&objects[0])?;
    obj.validate().ok()?;
    Some(obj)
}

fn json_to_object(v: &Value) -> Option<VectorObjectData> {
    let fill_rule = match v.get("fill_rule").and_then(Value::as_str) {
        Some("evenodd") => FillRule::EvenOdd,
        _ => FillRule::NonZero,
    };
    let transform = json_to_affine(v.get("transform")?)?;
    let style = json_to_style(v.get("style")?)?;
    let contours = v
        .get("contours")?
        .as_array()?
        .iter()
        .map(json_to_contour)
        .collect::<Option<Vec<_>>>()?;
    let path = PathData::new(contours, fill_rule);
    Some(VectorObjectData::new(path, style, transform))
}

fn json_to_contour(v: &Value) -> Option<Contour> {
    let closed = v.get("closed").and_then(Value::as_bool).unwrap_or(false);
    let nodes = v
        .get("nodes")?
        .as_array()?
        .iter()
        .map(json_to_node)
        .collect::<Option<Vec<_>>>()?;
    Some(Contour::new(nodes, closed))
}

fn json_to_node(v: &Value) -> Option<Node> {
    let anchor = json_to_pt(v.get("anchor")?)?;
    let in_handle = v.get("in").and_then(json_to_pt);
    let out_handle = v.get("out").and_then(json_to_pt);
    let kind = match v.get("kind").and_then(Value::as_u64) {
        Some(1) => NodeKind::Smooth,
        Some(2) => NodeKind::Symmetric,
        _ => NodeKind::Cusp,
    };
    Some(Node {
        anchor,
        in_handle,
        out_handle,
        kind,
    })
}

fn json_to_style(v: &Value) -> Option<VectorStyle> {
    let d = VectorStyle::default();
    let fill = v.get("fill").and_then(json_to_paint).unwrap_or(d.fill);
    let stroke = v.get("stroke").and_then(json_to_paint).unwrap_or(d.stroke);
    let stroke_style = v
        .get("stroke_style")
        .and_then(json_to_stroke_style)
        .unwrap_or(d.stroke_style);
    Some(VectorStyle {
        fill,
        fill_overprint: v
            .get("fill_overprint")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        stroke,
        stroke_style,
        stroke_overprint: v
            .get("stroke_overprint")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        opacity: v.get("opacity").and_then(Value::as_f64).unwrap_or(1.0) as f32,
    })
}

fn json_to_stroke_style(v: &Value) -> Option<StrokeStyle> {
    Some(StrokeStyle {
        width: v.get("width").and_then(Value::as_f64).unwrap_or(1.0) as f32,
        cap: match v.get("cap").and_then(Value::as_u64) {
            Some(1) => LineCap::Round,
            Some(2) => LineCap::Square,
            _ => LineCap::Butt,
        },
        join: match v.get("join").and_then(Value::as_u64) {
            Some(1) => LineJoin::Round,
            Some(2) => LineJoin::Bevel,
            _ => LineJoin::Miter,
        },
        miter_limit: v.get("miter_limit").and_then(Value::as_f64).unwrap_or(4.0) as f32,
    })
}

fn json_to_paint(v: &Value) -> Option<Paint> {
    match v.get("kind").and_then(Value::as_str) {
        Some("solid") => Some(Paint::Solid(json_to_color(v.get("color")?)?)),
        _ => Some(Paint::None),
    }
}

fn json_to_color(v: &Value) -> Option<ColorValue> {
    let arr = v.get("v")?.as_array()?;
    let f = |i: usize| arr.get(i).and_then(Value::as_f64).map(|n| n as f32);
    match v.get("space").and_then(Value::as_str) {
        Some("cmyk") => Some(ColorValue::Cmyk {
            c: f(0)?,
            m: f(1)?,
            y: f(2)?,
            k: f(3)?,
            a: f(4).unwrap_or(1.0),
        }),
        _ => Some(ColorValue::Rgb {
            r: f(0)?,
            g: f(1)?,
            b: f(2)?,
            a: f(3).unwrap_or(1.0),
        }),
    }
}

fn json_to_affine(v: &Value) -> Option<AffineTransform> {
    let arr = v.as_array()?;
    if arr.len() != 6 {
        return None;
    }
    let f = |i: usize| arr[i].as_f64().map(|n| n as f32);
    Some(AffineTransform {
        a: f(0)?,
        b: f(1)?,
        c: f(2)?,
        d: f(3)?,
        e: f(4)?,
        f: f(5)?,
    })
}

fn json_to_pt(v: &Value) -> Option<Point> {
    let arr = v.as_array()?;
    if arr.len() < 2 {
        return None;
    }
    Some(Point::new(arr[0].as_f64()? as f32, arr[1].as_f64()? as f32))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> VectorObjectData {
        let path = PathData::new(
            vec![
                Contour::new(
                    vec![
                        Node::sharp(Point::new(0.0, 0.0)),
                        Node::with_handles(
                            Point::new(10.0, 0.0),
                            Point::new(8.0, -2.0),
                            Point::new(12.0, 2.0),
                            NodeKind::Smooth,
                        ),
                        Node::sharp(Point::new(10.0, 10.0)),
                    ],
                    true,
                ),
                Contour::new(
                    vec![
                        Node::sharp(Point::new(2.0, 2.0)),
                        Node::sharp(Point::new(4.0, 2.0)),
                    ],
                    false,
                ),
            ],
            FillRule::EvenOdd,
        );
        let mut style = VectorStyle::filled(ColorValue::cmyk(0.1, 0.2, 0.3, 0.4));
        style.stroke = Paint::Solid(ColorValue::rgba(0.0, 0.5, 1.0, 0.8));
        style.stroke_style = StrokeStyle {
            width: 3.5,
            cap: LineCap::Round,
            join: LineJoin::Bevel,
            miter_limit: 2.0,
        };
        style.stroke_overprint = true;
        style.opacity = 0.75;
        VectorObjectData::new(path, style, AffineTransform::translate(25.0, -5.0))
    }

    #[test]
    fn round_trips_geometry_style_transform() {
        let obj = sample();
        let json = layer_path_to_json(&obj);
        let back = json_to_layer_path(&json).expect("decode");
        assert_eq!(back, obj, "model must round-trip byte-for-byte");
    }

    #[test]
    fn cmyk_color_survives_verbatim() {
        let obj = VectorObjectData::from_path(PathData::new(
            vec![Contour::new(vec![Node::sharp(Point::new(0.0, 0.0))], false)],
            FillRule::NonZero,
        ));
        let mut o = obj;
        o.style.fill = Paint::Solid(ColorValue::cmyk(0.0, 0.0, 0.0, 1.0));
        let back = json_to_layer_path(&layer_path_to_json(&o)).expect("decode");
        assert_eq!(
            back.style.fill,
            Paint::Solid(ColorValue::cmyk(0.0, 0.0, 0.0, 1.0))
        );
    }

    #[test]
    fn envelope_is_an_object_list() {
        let json = layer_path_to_json(&sample());
        assert_eq!(json["schema"], PATH_SCHEMA);
        assert!(json["objects"].is_array());
        assert_eq!(json["objects"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn rejects_missing_and_malformed() {
        assert!(json_to_layer_path(&json!({})).is_none());
        assert!(json_to_layer_path(&json!({ "objects": [] })).is_none());
        // A non-finite coordinate (serde renders NaN as null) decodes to None
        // rather than being trusted.
        let bad = json!({
            "objects": [{
                "fill_rule": "nonzero",
                "transform": [1.0, 0.0, 0.0, 1.0, 0.0, 0.0],
                "style": style_to_json(&VectorStyle::default()),
                "contours": [{ "closed": false, "nodes": [{ "anchor": [f64::NAN, 0.0] }] }],
            }],
        });
        assert!(json_to_layer_path(&bad).is_none());
    }
}
