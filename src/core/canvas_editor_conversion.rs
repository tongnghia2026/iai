//! One-way migration from the v11 FlowText model to Canvas Editor JSON.
//!
//! The converter never mutates the source document. Properties Canvas Editor
//! cannot render are retained in an `extension` object and reported once so a
//! caller can surface a compatibility warning without silently losing data.

use base64::Engine;
use serde_json::{json, Map, Value};

use super::document::CanvasEditorDocument;
use super::text::TextFontFamily;
use super::text_document::{
    CharStyle, ImageBlock, ImageWrap, ListKind, Paragraph, ParagraphAlign, ParagraphStyle,
    TextDocument, DEFAULT_DPI,
};

const MM_PER_INCH: f32 = 25.4;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LegacyConversionWarning {
    ParagraphIndentationPreservedButNotRendered,
    ParagraphSpacingPreservedButNotRendered,
    UnsupportedImageEncoding,
}

impl LegacyConversionWarning {
    pub fn message(self) -> &'static str {
        match self {
            Self::ParagraphIndentationPreservedButNotRendered => {
                "Paragraph indentation was preserved as migration metadata but is not rendered yet"
            }
            Self::ParagraphSpacingPreservedButNotRendered => {
                "Paragraph before/after spacing was preserved as migration metadata but is not rendered yet"
            }
            Self::UnsupportedImageEncoding => {
                "An image with an unsupported encoding was preserved but may not render"
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct LegacyConversion {
    pub document: CanvasEditorDocument,
    pub warnings: Vec<LegacyConversionWarning>,
}

pub fn convert_legacy_document(source: &TextDocument) -> Result<LegacyConversion, String> {
    source.validate()?;
    let mut source = source.clone();
    source.migrate_inline_images_to_top_bottom();

    let mut warnings = Vec::new();
    let mut main = Vec::new();
    let mut paragraph_index = 0usize;
    while paragraph_index < source.paragraphs.len() {
        let paragraph = &source.paragraphs[paragraph_index];
        if paragraph.style.list == ListKind::None || paragraph.image.is_some() {
            main.extend(paragraph_elements(
                paragraph,
                &source.default_char,
                &mut warnings,
            ));
            paragraph_index += 1;
            if paragraph_index < source.paragraphs.len() {
                main.push(paragraph_break(&paragraph.style, &mut warnings));
            }
            continue;
        }

        let list_kind = paragraph.style.list;
        let mut value_list = Vec::new();
        while paragraph_index < source.paragraphs.len()
            && source.paragraphs[paragraph_index].style.list == list_kind
            && source.paragraphs[paragraph_index].image.is_none()
        {
            let item = &source.paragraphs[paragraph_index];
            value_list.extend(paragraph_elements(
                item,
                &source.default_char,
                &mut warnings,
            ));
            paragraph_index += 1;
            if paragraph_index < source.paragraphs.len()
                && source.paragraphs[paragraph_index].style.list == list_kind
                && source.paragraphs[paragraph_index].image.is_none()
            {
                value_list.push(paragraph_break(&item.style, &mut warnings));
            }
        }
        main.push(json!({
            "type": "list",
            "value": "",
            "listType": match list_kind {
                ListKind::Bullet => "ul",
                ListKind::Numbered => "ol",
                ListKind::None => unreachable!(),
            },
            "listStyle": match list_kind {
                ListKind::Bullet => "disc",
                ListKind::Numbered => "decimal",
                ListKind::None => unreachable!(),
            },
            "valueList": value_list,
        }));
        if paragraph_index < source.paragraphs.len() {
            main.push(paragraph_break(
                &source.paragraphs[paragraph_index - 1].style,
                &mut warnings,
            ));
        }
    }

    for image in &source.floating_images {
        main.push(image_element(image, &mut warnings));
    }
    if main.is_empty() {
        main.push(json!({ "value": "" }));
    }

    deduplicate_warnings(&mut warnings);
    let page = &source.page;
    let payload = json!({
        "header": [],
        "main": main,
        "footer": [],
        "_iai": {
            "schema_version": 1,
            "page_setup": {
                "width": mm_to_editor_px(page.paper.width_mm),
                "height": mm_to_editor_px(page.paper.height_mm),
                "margins": [
                    mm_to_editor_px(page.margins.top_mm),
                    mm_to_editor_px(page.margins.right_mm),
                    mm_to_editor_px(page.margins.bottom_mm),
                    mm_to_editor_px(page.margins.left_mm),
                ]
            },
            "legacy_conversion_warnings": warnings.iter().map(|warning| warning.message()).collect::<Vec<_>>(),
        }
    });

    Ok(LegacyConversion {
        document: CanvasEditorDocument::try_new(payload)?,
        warnings,
    })
}

fn paragraph_elements(
    paragraph: &Paragraph,
    default_char: &CharStyle,
    warnings: &mut Vec<LegacyConversionWarning>,
) -> Vec<Value> {
    if let Some(image) = &paragraph.image {
        let mut element = image_element(image, warnings);
        if let Some(object) = element.as_object_mut() {
            apply_paragraph_style(object, &paragraph.style, warnings);
        }
        return vec![element];
    }

    let mut elements = Vec::with_capacity(paragraph.runs.len().max(1));
    for run in &paragraph.runs {
        if run.text.is_empty() {
            continue;
        }
        let mut element = Map::new();
        element.insert("type".to_string(), json!("text"));
        element.insert("value".to_string(), json!(run.text));
        apply_character_style(&mut element, &run.style);
        apply_paragraph_style(&mut element, &paragraph.style, warnings);
        elements.push(Value::Object(element));
    }
    if elements.is_empty() {
        let mut element = Map::new();
        element.insert("type".to_string(), json!("text"));
        element.insert("value".to_string(), json!(""));
        apply_character_style(&mut element, default_char);
        apply_paragraph_style(&mut element, &paragraph.style, warnings);
        elements.push(Value::Object(element));
    }
    elements
}

fn paragraph_break(style: &ParagraphStyle, warnings: &mut Vec<LegacyConversionWarning>) -> Value {
    let mut element = Map::new();
    element.insert("value".to_string(), json!("\n"));
    apply_paragraph_style(&mut element, style, warnings);
    Value::Object(element)
}

fn apply_character_style(element: &mut Map<String, Value>, style: &CharStyle) {
    element.insert("font".to_string(), json!(font_name(&style.font)));
    element.insert(
        "size".to_string(),
        json!(points_to_editor_px(style.size_pt)),
    );
    if style.bold {
        element.insert("bold".to_string(), json!(true));
    }
    if style.italic {
        element.insert("italic".to_string(), json!(true));
    }
    if style.underline {
        element.insert("underline".to_string(), json!(true));
    }
    let color = style.color;
    let css = if color.a == 255 {
        format!("#{:02X}{:02X}{:02X}", color.r, color.g, color.b)
    } else {
        format!(
            "rgba({}, {}, {}, {:.4})",
            color.r,
            color.g,
            color.b,
            color.a as f32 / 255.0
        )
    };
    element.insert("color".to_string(), json!(css));
}

fn apply_paragraph_style(
    element: &mut Map<String, Value>,
    style: &ParagraphStyle,
    warnings: &mut Vec<LegacyConversionWarning>,
) {
    element.insert(
        "rowFlex".to_string(),
        json!(match style.align {
            ParagraphAlign::Left => "left",
            ParagraphAlign::Center => "center",
            ParagraphAlign::Right => "right",
            ParagraphAlign::Justify => "alignment",
        }),
    );
    element.insert(
        "rowMargin".to_string(),
        json!(
            if style.line_spacing.is_finite() && style.line_spacing > 0.0 {
                style.line_spacing
            } else {
                1.0
            }
        ),
    );

    let has_spacing = style.space_before_pt != 0.0 || style.space_after_pt != 0.0;
    let has_indentation =
        style.indent_first_pt != 0.0 || style.indent_left_pt != 0.0 || style.indent_right_pt != 0.0;
    if has_spacing || has_indentation {
        element.insert(
            "extension".to_string(),
            json!({
                "iai_legacy_paragraph": {
                    "space_before_pt": style.space_before_pt,
                    "space_after_pt": style.space_after_pt,
                    "indent_first_pt": style.indent_first_pt,
                    "indent_left_pt": style.indent_left_pt,
                    "indent_right_pt": style.indent_right_pt,
                }
            }),
        );
    }
    if has_spacing {
        warnings.push(LegacyConversionWarning::ParagraphSpacingPreservedButNotRendered);
    }
    if has_indentation {
        warnings.push(LegacyConversionWarning::ParagraphIndentationPreservedButNotRendered);
    }
}

fn image_element(image: &ImageBlock, warnings: &mut Vec<LegacyConversionWarning>) -> Value {
    let mime = if image.data.starts_with(b"\x89PNG\r\n\x1a\n") {
        "image/png"
    } else if image.data.starts_with(&[0xFF, 0xD8, 0xFF]) {
        "image/jpeg"
    } else {
        warnings.push(LegacyConversionWarning::UnsupportedImageEncoding);
        "application/octet-stream"
    };
    let value = format!(
        "data:{mime};base64,{}",
        base64::engine::general_purpose::STANDARD.encode(&image.data)
    );
    let width = mm_to_editor_px(image.width_mm);
    let height = mm_to_editor_px(image.height_mm());
    let mut element = json!({
        "type": "image",
        "value": value,
        "width": width,
        "height": height,
        "imgDisplay": match image.wrap {
            ImageWrap::Inline => "inline",
            ImageWrap::TopBottom => "block",
            ImageWrap::Square => "surround",
            ImageWrap::InFrontOfText => "float-top",
            ImageWrap::BehindText => "float-bottom",
        },
        "rowFlex": match image.align {
            ParagraphAlign::Left | ParagraphAlign::Justify => "left",
            ParagraphAlign::Center => "center",
            ParagraphAlign::Right => "right",
        }
    });
    if image.wrap.is_floating() {
        element["imgFloatPosition"] = json!({
            "x": mm_to_editor_px(image.x_mm),
            "y": mm_to_editor_px(image.y_mm),
            "pageNo": image.page,
        });
    }
    element
}

fn font_name(font: &TextFontFamily) -> &str {
    font.name()
}

fn points_to_editor_px(points: f32) -> f32 {
    if points.is_finite() && points > 0.0 {
        points * DEFAULT_DPI / 72.0
    } else {
        13.0 * DEFAULT_DPI / 72.0
    }
}

fn mm_to_editor_px(mm: f32) -> f32 {
    mm * DEFAULT_DPI / MM_PER_INCH
}

fn deduplicate_warnings(warnings: &mut Vec<LegacyConversionWarning>) {
    let mut unique = Vec::with_capacity(warnings.len());
    for warning in warnings.drain(..) {
        if !unique.contains(&warning) {
            unique.push(warning);
        }
    }
    *warnings = unique;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::color::Color;
    use crate::core::text_document::{Margins, PaperSize, Run};

    #[test]
    fn converts_runs_paragraphs_lists_and_page_setup() {
        let mut first = Paragraph::plain("Việt Nam", CharStyle::default());
        first.runs.push(Run::new(
            " đậm",
            CharStyle {
                bold: true,
                italic: true,
                underline: true,
                color: Color::rgb(10, 20, 30),
                ..CharStyle::default()
            },
        ));
        first.style.align = ParagraphAlign::Justify;
        first.style.line_spacing = 1.5;
        first.style.space_after_pt = 6.0;
        first.style.indent_first_pt = 18.0;
        let mut bullet = Paragraph::plain("Mục một", CharStyle::default());
        bullet.style.list = ListKind::Bullet;
        let document = TextDocument {
            paragraphs: vec![first, bullet],
            page: super::super::text_document::PageSetup {
                paper: PaperSize::A5,
                margins: Margins::uniform(15.0),
            },
            ..TextDocument::default()
        };

        let converted = convert_legacy_document(&document).unwrap();
        let payload = converted.document.payload();
        assert_eq!(payload["main"][0]["value"], "Việt Nam");
        assert_eq!(payload["main"][0]["rowFlex"], "alignment");
        assert_eq!(payload["main"][0]["rowMargin"], 1.5);
        assert_eq!(payload["main"][1]["bold"], true);
        assert_eq!(payload["main"][1]["italic"], true);
        assert_eq!(payload["main"][1]["underline"], true);
        assert_eq!(payload["main"][1]["color"], "#0A141E");
        assert_eq!(payload["main"][3]["type"], "list");
        assert_eq!(payload["main"][3]["listType"], "ul");
        assert!(
            (payload["_iai"]["page_setup"]["width"].as_f64().unwrap() - (148.0 * 96.0 / 25.4))
                .abs()
                < 0.01
        );
        assert_eq!(converted.warnings.len(), 2);
        assert!(payload["main"][0]["extension"]["iai_legacy_paragraph"].is_object());
    }

    #[test]
    fn converts_image_data_and_wrap_modes() {
        let image = ImageBlock {
            data: b"\x89PNG\r\n\x1a\ncontent".to_vec(),
            natural_w: 200,
            natural_h: 100,
            width_mm: 50.0,
            align: ParagraphAlign::Center,
            wrap: ImageWrap::Square,
            page: 2,
            x_mm: 12.0,
            y_mm: 18.0,
        };
        let document = TextDocument {
            paragraphs: vec![Paragraph::image(image)],
            ..TextDocument::default()
        };

        let converted = convert_legacy_document(&document).unwrap();
        let element = &converted.document.payload()["main"][0];
        assert_eq!(element["type"], "image");
        assert_eq!(element["imgDisplay"], "surround");
        assert_eq!(element["imgFloatPosition"]["pageNo"], 2);
        assert!(element["value"]
            .as_str()
            .unwrap()
            .starts_with("data:image/png;base64,"));
        assert!(converted.warnings.is_empty());
    }
}
