//! `.iai` document-palette payload.

use crate::core::palette::{
    validate_palette, DocumentSwatch, MAX_DOCUMENT_SWATCHES, MAX_SWATCH_NAME_BYTES,
};
use crate::core::vector::color::ColorValue;
use serde_json::{json, Value};

pub fn palette_to_json(swatches: &[DocumentSwatch]) -> Value {
    Value::Array(
        swatches
            .iter()
            .map(|swatch| {
                json!({
                    "name": swatch.name,
                    "color": color_to_json(swatch.color),
                })
            })
            .collect(),
    )
}

pub fn json_to_palette(value: Option<&Value>) -> Vec<DocumentSwatch> {
    let Some(entries) = value.and_then(Value::as_array) else {
        return Vec::new();
    };
    if entries.len() > MAX_DOCUMENT_SWATCHES {
        return Vec::new();
    }
    let mut palette = Vec::with_capacity(entries.len());
    for entry in entries {
        let Some(name) = entry.get("name").and_then(Value::as_str) else {
            return Vec::new();
        };
        if name.trim().is_empty() || name.len() > MAX_SWATCH_NAME_BYTES {
            return Vec::new();
        }
        let Some(color) = entry.get("color").and_then(json_to_color) else {
            return Vec::new();
        };
        palette.push(DocumentSwatch::new(name, color));
    }
    if validate_palette(&palette).is_err() {
        Vec::new()
    } else {
        palette
    }
}

fn color_to_json(color: ColorValue) -> Value {
    match color {
        ColorValue::Rgb { r, g, b, a } => json!({
            "space": "rgb",
            "values": [r, g, b, a],
        }),
        ColorValue::Cmyk { c, m, y, k, a } => json!({
            "space": "cmyk",
            "values": [c, m, y, k, a],
        }),
    }
}

fn json_to_color(value: &Value) -> Option<ColorValue> {
    let values = value.get("values")?.as_array()?;
    let channel = |index: usize| values.get(index)?.as_f64().map(|v| v as f32);
    let color = match value.get("space").and_then(Value::as_str) {
        Some("cmyk") if values.len() == 5 => ColorValue::cmyka(
            channel(0)?,
            channel(1)?,
            channel(2)?,
            channel(3)?,
            channel(4)?,
        ),
        Some("rgb") if values.len() == 4 => {
            ColorValue::rgba(channel(0)?, channel(1)?, channel(2)?, channel(3)?)
        }
        _ => return None,
    };
    color.validate().ok()?;
    Some(color)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn palette_json_preserves_rgb_cmyk_and_names() {
        let palette = vec![
            DocumentSwatch::new("Brand Red", ColorValue::rgb(1.0, 0.1, 0.2)),
            DocumentSwatch::new("Cut K", ColorValue::cmyk(0.0, 0.0, 0.0, 1.0)),
        ];
        assert_eq!(json_to_palette(Some(&palette_to_json(&palette))), palette);
    }

    #[test]
    fn malformed_palette_is_rejected_as_a_unit() {
        let bad = json!([{"name": "", "color": {"space": "rgb", "values": [0,0,0,1]}}]);
        assert!(json_to_palette(Some(&bad)).is_empty());
    }
}
