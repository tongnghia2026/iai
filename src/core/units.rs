// Unit conversion — used by the Crop tool, Canvas Size, Image Size.
// To add a unit: add a Unit variant and a case in to_pixels/from_pixels.

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Unit {
    Pixels,
    Centimeters,
    Millimeters,
    Inches,
    Points,
    Picas,
    Percent,
}

impl Unit {
    pub fn name(&self) -> &str {
        match self {
            Unit::Pixels => "px",
            Unit::Centimeters => "cm",
            Unit::Millimeters => "mm",
            Unit::Inches => "in",
            Unit::Points => "pt",
            Unit::Picas => "pc",
            Unit::Percent => "%",
        }
    }

    pub fn all() -> Vec<Unit> {
        vec![
            Unit::Pixels,
            Unit::Centimeters,
            Unit::Millimeters,
            Unit::Inches,
            Unit::Points,
            Unit::Picas,
            Unit::Percent,
        ]
    }
}

/// Convert a value from a unit to pixels.
pub fn to_pixels(value: f32, unit: Unit, dpi: f32, canvas_size: f32) -> f32 {
    match unit {
        Unit::Pixels => value,
        Unit::Inches => value * dpi,
        Unit::Centimeters => value * dpi / 2.54,
        Unit::Millimeters => value * dpi / 25.4,
        Unit::Points => value * dpi / 72.0,
        Unit::Picas => value * dpi / 6.0,
        Unit::Percent => value / 100.0 * canvas_size,
    }
}

/// Convert pixels to a unit.
pub fn from_pixels(px: f32, unit: Unit, dpi: f32, canvas_size: f32) -> f32 {
    match unit {
        Unit::Pixels => px,
        Unit::Inches => px / dpi,
        Unit::Centimeters => px / dpi * 2.54,
        Unit::Millimeters => px / dpi * 25.4,
        Unit::Points => px / dpi * 72.0,
        Unit::Picas => px / dpi * 6.0,
        Unit::Percent => {
            if canvas_size > 0.0 {
                px / canvas_size * 100.0
            } else {
                0.0
            }
        }
    }
}

/// Parse a dimension whose unit suffix is optional. This is shared by the New
/// Canvas, Crop, and Perspective Crop numeric fields so typing `10 cm`, `15mm`,
/// or `8 in` can also drive their adjacent unit selector.
pub fn parse_dimension(text: &str) -> Option<(f32, Option<Unit>)> {
    let text = text.trim();
    let unit_start = text
        .char_indices()
        .find_map(|(index, ch)| {
            (ch.is_ascii_alphabetic() || ch == '%' || ch == '"').then_some(index)
        })
        .unwrap_or(text.len());
    let (number, unit) = text.split_at(unit_start);

    let mut number: String = number
        .chars()
        .filter(|ch| !ch.is_whitespace())
        .map(|ch| if ch == '−' { '-' } else { ch })
        .collect();
    if number.contains(',') && !number.contains('.') {
        number = number.replace(',', ".");
    }
    let value = number.parse::<f32>().ok()?;

    let unit = match unit.trim().to_ascii_lowercase().as_str() {
        "" => None,
        "px" | "pixel" | "pixels" => Some(Unit::Pixels),
        "cm" | "centimeter" | "centimeters" | "centimetre" | "centimetres" => {
            Some(Unit::Centimeters)
        }
        "mm" | "millimeter" | "millimeters" | "millimetre" | "millimetres" => {
            Some(Unit::Millimeters)
        }
        "in" | "inh" | "inch" | "inches" | "\"" => Some(Unit::Inches),
        "pt" | "point" | "points" => Some(Unit::Points),
        "pc" | "pica" | "picas" => Some(Unit::Picas),
        "%" | "percent" => Some(Unit::Percent),
        _ => return None,
    };

    Some((value, unit))
}

/// Pretty-format a value for display in the UI.
pub fn format_value(value: f32, unit: Unit) -> String {
    match unit {
        Unit::Pixels => format!("{:.0}", value),
        Unit::Percent => format!("{:.1}%", value),
        _ => format!("{:.2}", value),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_roundtrip() {
        let dpi = 300.0;
        let canvas = 1000.0;
        for unit in Unit::all() {
            let original = 100.0f32;
            let px = to_pixels(original, unit, dpi, canvas);
            let back = from_pixels(px, unit, dpi, canvas);
            assert!(
                (back - original).abs() < 0.01,
                "Roundtrip failed for {:?}: {} -> {} -> {}",
                unit,
                original,
                px,
                back
            );
        }
    }

    #[test]
    fn dimension_parser_accepts_unit_suffixes_and_decimal_comma() {
        assert_eq!(
            parse_dimension("10 cm"),
            Some((10.0, Some(Unit::Centimeters)))
        );
        assert_eq!(
            parse_dimension("15mm"),
            Some((15.0, Some(Unit::Millimeters)))
        );
        assert_eq!(parse_dimension("8 inh"), Some((8.0, Some(Unit::Inches))));
        assert_eq!(
            parse_dimension("10,5 inches"),
            Some((10.5, Some(Unit::Inches)))
        );
        assert_eq!(parse_dimension("12.25"), Some((12.25, None)));
        assert_eq!(parse_dimension("10 bananas"), None);
    }
}
