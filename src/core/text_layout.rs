#![allow(dead_code)]
//! Document-mode layout & pagination — the phase-2/3 bridge from the pure
//! [`TextDocument`] model to the `cosmic-text` engine.
//!
//! This is the ONLY place the document model meets `cosmic-text`: it maps a
//! [`CharStyle`] to `Attrs` and a [`ParagraphAlign`] to `Align`, shapes each
//! paragraph into a wrapped [`cosmic_text::Buffer`] at the page's text-column
//! width, then flows the visual lines down the page — starting a new page when
//! a line would overflow the content height. A paragraph may straddle a page
//! break. Rendering rasterises each placed line straight to an RGBA buffer via
//! swash; it never touches the tile compositor, so a text page costs only the
//! shaped buffers plus the shared glyph cache (see the phase-0 measurements in
//! `src/bin/text_spike.rs`).

use cosmic_text::{
    Align, Attrs, Buffer, Color as CtColor, Family, FontSystem, Metrics, Shaping, Style,
    SwashCache, Weight,
};

use crate::core::text_document::{CharStyle, ParagraphAlign, TextDocument};

/// Build the `Attrs` for one run: font family, weight, slant, colour, and the
/// per-run metrics (size + leading) so mixed sizes on a line resolve correctly.
fn attrs_for(style: &CharStyle, px_per_pt: f32, line_spacing: f32) -> Attrs<'_> {
    let size_px = style.size_pt * px_per_pt;
    let leading = size_px * line_spacing.max(0.1);
    Attrs::new()
        .family(Family::Name(style.font.name()))
        .weight(if style.bold {
            Weight::BOLD
        } else {
            Weight::NORMAL
        })
        .style(if style.italic {
            Style::Italic
        } else {
            Style::Normal
        })
        .color(CtColor::rgba(
            style.color.r,
            style.color.g,
            style.color.b,
            style.color.a,
        ))
        .metrics(Metrics::new(size_px, leading))
}

fn align_for(align: ParagraphAlign) -> Option<Align> {
    match align {
        ParagraphAlign::Left => None, // engine default (left / start)
        ParagraphAlign::Center => Some(Align::Center),
        ParagraphAlign::Right => Some(Align::Right),
        ParagraphAlign::Justify => Some(Align::Justified),
    }
}

/// One shaped paragraph plus the page + top-offset of each of its visual lines.
struct ParaLayout {
    buffer: Buffer,
    /// `(page index, top y within the content area)` per visual line, in the
    /// same order as `buffer.layout_runs()`.
    line_pages: Vec<(usize, f32)>,
}

/// A fully flowed document: every visual line assigned to a page and position.
/// Owns the shaped buffers so rendering re-reads them without re-shaping.
pub struct DocumentLayout {
    pub dpi: f32,
    /// Text column rectangle in page-local pixels: `(x, y, w, h)`.
    content_rect: (f32, f32, f32, f32),
    /// Whole-sheet pixel size at `dpi`.
    page_px: (usize, usize),
    pages: usize,
    paras: Vec<ParaLayout>,
}

impl DocumentLayout {
    /// Shape and flow `doc` at `dpi`. `font_system` is the app-wide shared font
    /// index (one per process), matching how the real app will call this.
    pub fn build(doc: &TextDocument, dpi: f32, font_system: &mut FontSystem) -> Self {
        let (cx, cy, cw, ch) = doc.page.content_rect_px(dpi);
        let pw = doc.page.paper.width_px(dpi).ceil().max(1.0) as usize;
        let ph = doc.page.paper.height_px(dpi).ceil().max(1.0) as usize;
        let px_per_pt = dpi / 72.0;
        let default_attrs = attrs_for(&doc.default_char, px_per_pt, doc.default_para.line_spacing);

        let mut paras = Vec::with_capacity(doc.paragraphs.len());
        let mut page = 0usize;
        let mut y = 0.0f32; // running top within the current page's content area

        for para in &doc.paragraphs {
            let ls = para.style.line_spacing;
            let base_pt = para
                .runs
                .first()
                .map(|r| r.style.size_pt)
                .unwrap_or(doc.default_char.size_pt);
            let base_px = base_pt * px_per_pt;
            let metrics = Metrics::new(base_px, base_px * ls.max(0.1));

            let mut buffer = Buffer::new(font_system, metrics);
            buffer.set_size(Some(cw), None); // unbounded height: lay out every line
            let align = align_for(para.style.align);
            if para.runs.is_empty() {
                buffer.set_text("", &default_attrs, Shaping::Advanced, align);
            } else {
                let spans: Vec<(&str, Attrs)> = para
                    .runs
                    .iter()
                    .map(|r| (r.text.as_str(), attrs_for(&r.style, px_per_pt, ls)))
                    .collect();
                buffer.set_rich_text(spans, &default_attrs, Shaping::Advanced, align);
            }
            buffer.shape_until_scroll(font_system, false);

            y += para.style.space_before_pt * px_per_pt;

            let mut line_pages = Vec::new();
            for run in buffer.layout_runs() {
                let lh = run.line_height;
                // Break to a new page when the line won't fit, unless the page is
                // already empty (a single over-tall line still has to go somewhere).
                if y > 0.0 && y + lh > ch {
                    page += 1;
                    y = 0.0;
                }
                line_pages.push((page, y));
                y += lh;
            }

            y += para.style.space_after_pt * px_per_pt;
            paras.push(ParaLayout { buffer, line_pages });
        }

        Self {
            dpi,
            content_rect: (cx, cy, cw, ch),
            page_px: (pw, ph),
            pages: page + 1,
            paras,
        }
    }

    pub fn page_count(&self) -> usize {
        self.pages
    }

    /// Whole-sheet pixel size `(width, height)` at the build dpi.
    pub fn page_px(&self) -> (usize, usize) {
        self.page_px
    }

    pub fn line_count(&self) -> usize {
        self.paras.iter().map(|p| p.line_pages.len()).sum()
    }

    /// Every placed line as `(page, top_y_in_content, line_height)`, in reading
    /// order. Exposed for tests and, later, hit-testing.
    pub fn placed_lines(&self) -> Vec<(usize, f32, f32)> {
        let mut out = Vec::new();
        for para in &self.paras {
            for (li, run) in para.buffer.layout_runs().enumerate() {
                let (page, top) = para.line_pages[li];
                out.push((page, top, run.line_height));
            }
        }
        out
    }

    /// Rasterise one page to a straight-sRGB RGBA8 buffer (white paper). Length
    /// is `page_px.0 * page_px.1 * 4`.
    pub fn render_page(
        &self,
        page: usize,
        font_system: &mut FontSystem,
        cache: &mut SwashCache,
    ) -> Vec<u8> {
        let (pw, ph) = self.page_px;
        let mut buf = vec![255u8; pw * ph * 4];
        let (cx, cy, _, _) = self.content_rect;
        let default_ink = CtColor::rgb(0x1a, 0x1a, 0x1a);

        for para in &self.paras {
            for (li, run) in para.buffer.layout_runs().enumerate() {
                let (lp, top) = para.line_pages[li];
                if lp != page {
                    continue;
                }
                // Move the line so its top sits at `cy + top`; the engine places
                // glyphs relative to the baseline `run.line_y`.
                let base_y = cy + top + (run.line_y - run.line_top);
                for glyph in run.glyphs {
                    let color = glyph.color_opt.unwrap_or(default_ink);
                    let phys = glyph.physical((cx, base_y), 1.0);
                    cache.with_pixels(font_system, phys.cache_key, color, |gx, gy, col| {
                        blend_px(&mut buf, pw, ph, phys.x + gx, phys.y + gy, col);
                    });
                }
            }
        }
        buf
    }
}

/// Alpha-blend one covered pixel onto an opaque RGBA canvas.
fn blend_px(buf: &mut [u8], w: usize, h: usize, x: i32, y: i32, color: CtColor) {
    if x < 0 || y < 0 || x as usize >= w || y as usize >= h {
        return;
    }
    let a = color.a() as f32 / 255.0;
    if a <= 0.0 {
        return;
    }
    let idx = (y as usize * w + x as usize) * 4;
    let src = [color.r() as f32, color.g() as f32, color.b() as f32];
    for (c, s) in src.iter().enumerate() {
        let d = buf[idx + c] as f32;
        buf[idx + c] = (s * a + d * (1.0 - a)).round() as u8;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::text_document::{CharStyle, Paragraph, ParagraphStyle, Run};

    const DPI: f32 = 96.0;

    fn para(text: &str) -> Paragraph {
        Paragraph::plain(text, CharStyle::default())
    }

    #[test]
    fn short_document_is_one_page() {
        let mut fs = FontSystem::new();
        let doc = TextDocument::from_plain_text("Xin chào\nHợp đồng thuê nhà");
        let layout = DocumentLayout::build(&doc, DPI, &mut fs);
        assert_eq!(layout.page_count(), 1);
        assert_eq!(layout.line_count(), 2);
    }

    #[test]
    fn long_paragraph_wraps_to_multiple_lines() {
        let mut fs = FontSystem::new();
        let long = "Đoạn văn rất dài cố ý để buộc engine tự động xuống dòng theo bề \
                    rộng cột văn bản của khổ giấy A4 với lề tiêu chuẩn, lặp đi lặp lại \
                    nhiều lần cho chắc chắn tràn quá một dòng đơn."
            .repeat(3);
        let doc = TextDocument {
            paragraphs: vec![para(&long)],
            ..Default::default()
        };
        let layout = DocumentLayout::build(&doc, DPI, &mut fs);
        assert!(
            layout.line_count() > 3,
            "expected wrapping, got {} lines",
            layout.line_count()
        );
    }

    #[test]
    fn overflowing_text_flows_onto_new_pages() {
        let mut fs = FontSystem::new();
        // Far more short paragraphs than fit on one A4 page.
        let paragraphs: Vec<Paragraph> = (0..200).map(|i| para(&format!("Dòng số {i}"))).collect();
        let doc = TextDocument {
            paragraphs,
            ..Default::default()
        };
        let layout = DocumentLayout::build(&doc, DPI, &mut fs);
        assert!(
            layout.page_count() >= 2,
            "200 lines should overflow one page, got {} page(s)",
            layout.page_count()
        );
    }

    #[test]
    fn placed_lines_respect_content_height() {
        let mut fs = FontSystem::new();
        let doc = TextDocument::from_plain_text(&"Một dòng\n".repeat(120));
        let layout = DocumentLayout::build(&doc, DPI, &mut fs);
        let (_, _, _, ch) = layout.content_rect;
        for (page, top, lh) in layout.placed_lines() {
            assert!(top >= 0.0, "line top must be non-negative");
            // A line either starts a page (top == 0) or fits within the column.
            assert!(
                top == 0.0 || top + lh <= ch + 1.0,
                "line on page {page} at top {top} (+{lh}) overflows content height {ch}"
            );
        }
    }

    #[test]
    fn render_page_produces_ink_on_paper() {
        let mut fs = FontSystem::new();
        let mut cache = SwashCache::new();
        let doc = TextDocument::from_plain_text("CỘNG HÒA XÃ HỘI CHỦ NGHĨA VIỆT NAM");
        let layout = DocumentLayout::build(&doc, DPI, &mut fs);
        let (pw, ph) = layout.page_px();
        let img = layout.render_page(0, &mut fs, &mut cache);
        assert_eq!(img.len(), pw * ph * 4);
        // Some pixels must be darker than paper white where the text sits.
        let inked = img.chunks_exact(4).any(|p| p[0] < 200);
        assert!(inked, "expected rasterised text on the page");
    }

    #[test]
    fn styled_runs_map_to_attrs_without_panic() {
        let mut fs = FontSystem::new();
        let mut bold = CharStyle::default();
        bold.bold = true;
        bold.italic = true;
        let doc = TextDocument {
            paragraphs: vec![Paragraph {
                runs: vec![
                    Run::new("Bên A ", CharStyle::default()),
                    Run::new("(đậm nghiêng)", bold),
                ],
                style: ParagraphStyle {
                    align: ParagraphAlign::Justify,
                    ..Default::default()
                },
            }],
            ..Default::default()
        };
        let layout = DocumentLayout::build(&doc, DPI, &mut fs);
        assert_eq!(layout.page_count(), 1);
        assert!(layout.line_count() >= 1);
    }
}
