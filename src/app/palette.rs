//! Application glue for document-owned process swatches.

use crate::app::state::App;
use crate::core::gateway::ChangeKind;
use crate::core::palette::{DocumentSwatch, ReplaceDocumentPalette, MAX_DOCUMENT_SWATCHES};
use crate::core::vector::color::ColorValue;

impl App {
    fn commit_document_palette(&mut self, palette: Vec<DocumentSwatch>) -> bool {
        let canvas = &mut self.docs.documents[self.docs.active_doc_idx].canvas;
        if palette == canvas.metadata.swatches {
            return false;
        }
        match canvas.execute(
            Box::new(ReplaceDocumentPalette::new(palette)),
            ChangeKind::DocumentMetadata,
        ) {
            Ok(_) => {
                if let Some(window) = &self.win.window {
                    window.request_redraw();
                }
                true
            }
            Err(error) => {
                self.shell.status_msg = format!("Không thể sửa bảng màu: {error}");
                false
            }
        }
    }

    pub fn add_document_swatch(&mut self, color: ColorValue) {
        // In a CMYK document, an RGB foreground/quick-palette colour becomes a
        // real process swatch through the document's own converter. This avoids
        // saving only an sRGB preview while presenting it as production CMYK.
        let color = {
            let canvas = &self.docs.documents[self.docs.active_doc_idx].canvas;
            if canvas.is_cmyk() && !color.is_cmyk() {
                let Some(converter) = canvas.cmyk_converter() else {
                    self.shell.status_msg =
                        "Không thể tạo swatch CMYK: profile tài liệu không hợp lệ".to_string();
                    return;
                };
                let [r, g, b, a] = color.to_rgba8();
                let [c, m, y, k] = converter.rgb_to_cmyk_one([r, g, b]);
                ColorValue::cmyka(
                    c as f32 / 255.0,
                    m as f32 / 255.0,
                    y as f32 / 255.0,
                    k as f32 / 255.0,
                    a as f32 / 255.0,
                )
            } else {
                color
            }
        };
        let current = &self.docs.documents[self.docs.active_doc_idx]
            .canvas
            .metadata
            .swatches;
        if current.len() >= MAX_DOCUMENT_SWATCHES {
            self.shell.status_msg = format!("Bảng màu đã đạt tối đa {MAX_DOCUMENT_SWATCHES} màu");
            return;
        }
        if current.iter().any(|swatch| swatch.color == color) {
            self.shell.status_msg = "Màu này đã có trong bảng màu tài liệu".to_string();
            return;
        }
        let base = swatch_default_name(color);
        let name = unique_swatch_name(current, &base);
        let mut palette = current.clone();
        palette.push(DocumentSwatch::new(name.clone(), color));
        if self.commit_document_palette(palette) {
            self.shell.status_msg = format!("Đã thêm swatch: {name}");
        }
    }

    pub fn rename_document_swatch(&mut self, index: usize, name: String) {
        let name = name.trim();
        if name.is_empty() {
            self.shell.status_msg = "Tên swatch không được để trống".to_string();
            return;
        }
        let mut palette = self.docs.documents[self.docs.active_doc_idx]
            .canvas
            .metadata
            .swatches
            .clone();
        let Some(swatch) = palette.get_mut(index) else {
            return;
        };
        swatch.name = name.to_string();
        if self.commit_document_palette(palette) {
            self.shell.status_msg = format!("Đã đổi tên swatch: {name}");
        }
    }

    pub fn remove_document_swatch(&mut self, index: usize) {
        let mut palette = self.docs.documents[self.docs.active_doc_idx]
            .canvas
            .metadata
            .swatches
            .clone();
        if index >= palette.len() {
            return;
        }
        let removed = palette.remove(index).name;
        if self.commit_document_palette(palette) {
            self.shell.status_msg = format!("Đã xóa swatch: {removed}");
        }
    }
}

fn unique_swatch_name(swatches: &[DocumentSwatch], base: &str) -> String {
    if !swatches.iter().any(|swatch| swatch.name == base) {
        return base.to_string();
    }
    for suffix in 2..=MAX_DOCUMENT_SWATCHES + 1 {
        let candidate = format!("{base} {suffix}");
        if !swatches.iter().any(|swatch| swatch.name == candidate) {
            return candidate;
        }
    }
    base.to_string()
}

fn swatch_default_name(color: ColorValue) -> String {
    match color {
        ColorValue::Rgb { .. } => {
            let [r, g, b, _] = color.to_rgba8();
            format!("RGB #{r:02X}{g:02X}{b:02X}")
        }
        ColorValue::Cmyk { c, m, y, k, .. } => {
            let percent = |value: f32| (value.clamp(0.0, 1.0) * 100.0).round() as u8;
            format!(
                "CMYK C{} M{} Y{} K{}",
                percent(c),
                percent(m),
                percent(y),
                percent(k)
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_names_retain_process_space() {
        assert_eq!(
            swatch_default_name(ColorValue::rgb(1.0, 0.0, 0.0)),
            "RGB #FF0000"
        );
        assert_eq!(
            swatch_default_name(ColorValue::cmyk(0.0, 1.0, 1.0, 0.0)),
            "CMYK C0 M100 Y100 K0"
        );
    }

    #[test]
    fn add_current_in_cmyk_document_creates_a_real_cmyk_swatch() {
        let mut app = App::new();
        app.docs.documents[0]
            .canvas
            .convert_to_cmyk(crate::core::canvas::CmykProfile::Naive)
            .unwrap();
        app.add_document_swatch(ColorValue::rgb(1.0, 0.0, 0.0));
        let swatches = &app.docs.documents[0].canvas.metadata.swatches;
        assert_eq!(swatches.len(), 1);
        assert!(swatches[0].color.is_cmyk());
        assert_eq!(swatches[0].name, "CMYK C0 M100 Y100 K0");
        app.docs.documents[0]
            .canvas
            .undo()
            .expect("undo add swatch");
        assert!(app.docs.documents[0].canvas.metadata.swatches.is_empty());
    }
}
