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

use std::collections::BTreeMap;
use std::io::Write;
use std::path::Path;

use cosmic_text::{
    Align, Attrs, Buffer, Color as CtColor, Family, FontSystem, Metrics, Shaping, Style,
    SwashCache, UnderlineStyle, Weight,
};
use lopdf::{dictionary, Dictionary, Document, Object, Stream, StringFormat};

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
        .underline(if style.underline {
            UnderlineStyle::Single
        } else {
            UnderlineStyle::None
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

/// One placed glyph for PDF text emission (position in PDF points, bottom-up).
struct Glyph {
    font: usize, // index into the export's font table
    gid: u16,
    x: f32,
    y: f32,
    size: f32,
    color: [u8; 4],
}

/// A filled rectangle (underline) for PDF emission, in PDF points with the
/// origin at the page's bottom-left, matching the glyph coordinate frame.
struct FillRect {
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    color: [u8; 4],
}

impl DocumentLayout {
    /// Write the document as a fresh multi-page PDF with **real, selectable
    /// text**: each used face is embedded once as a Type0 / Identity-H CID font
    /// and glyphs are drawn by shaped position. The result is vector (sharp at
    /// any zoom, no rasterised pixels), small, and copy/searchable — Vietnamese
    /// included, via a ToUnicode CMap. Works from a layout built at any dpi.
    pub fn write_text_pdf(&self, font_system: &mut FontSystem, path: &Path) -> Result<(), String> {
        let pages: Vec<usize> = (0..self.pages).collect();
        self.write_text_pdf_pages(font_system, path, &pages)
    }

    /// Write only the selected zero-based page positions, preserving the order
    /// supplied by the unified File → Export PDF dialog.
    pub fn write_text_pdf_pages(
        &self,
        font_system: &mut FontSystem,
        path: &Path,
        selected_pages: &[usize],
    ) -> Result<(), String> {
        if selected_pages.is_empty() {
            return Err("Không có trang văn bản nào để xuất".to_string());
        }
        if let Some(page) = selected_pages.iter().find(|&&page| page >= self.pages) {
            return Err(format!("Trang {} không tồn tại", *page + 1));
        }
        let s = 72.0 / self.dpi; // built-dpi px -> PDF points
        let (cx, cy, _, _) = self.content_rect;
        let (pw_px, ph_px) = self.page_px;
        let page_w = pw_px as f32 * s;
        let page_h = ph_px as f32 * s;

        let mut font_ids = Vec::new(); // unique face ids, insertion order (type inferred)
        let mut font_weight = Vec::new();
        let mut to_unicode: Vec<BTreeMap<u16, String>> = Vec::new();
        let mut pages: Vec<(Vec<Glyph>, Vec<FillRect>)> = Vec::new();

        for &page in selected_pages {
            let mut glyphs = Vec::new();
            let mut rects = Vec::new();
            for para in &self.paras {
                for (li, run) in para.buffer.layout_runs().enumerate() {
                    let (lp, top) = para.line_pages[li];
                    if lp != page {
                        continue;
                    }
                    let baseline = (cy + top + (run.line_y - run.line_top)) * s;
                    for g in run.glyphs {
                        let fi = match font_ids.iter().position(|id| *id == g.font_id) {
                            Some(i) => i,
                            None => {
                                font_ids.push(g.font_id);
                                font_weight.push(g.font_weight);
                                to_unicode.push(BTreeMap::new());
                                font_ids.len() - 1
                            }
                        };
                        if let Some(src) = run.text.get(g.start..g.end) {
                            if !src.is_empty() {
                                to_unicode[fi]
                                    .entry(g.glyph_id)
                                    .or_insert_with(|| src.to_string());
                            }
                        }
                        let color = g
                            .color_opt
                            .map(|c| [c.r(), c.g(), c.b(), c.a()])
                            .unwrap_or([26, 26, 26, 255]);
                        glyphs.push(Glyph {
                            font: fi,
                            gid: g.glyph_id,
                            x: (cx + g.x) * s,
                            y: page_h - baseline,
                            size: g.font_size * s,
                            color,
                        });
                    }
                    // Underline decorations as filled rectangles (vector), matching
                    // the on-screen renderer: same offset/thickness from the font.
                    for span in run.decorations {
                        let underline = span.data.text_decoration.underline;
                        if underline == UnderlineStyle::None {
                            continue;
                        }
                        let Some(gs) = run.glyphs.get(span.glyph_range.clone()) else {
                            continue;
                        };
                        if gs.is_empty() {
                            continue;
                        }
                        let x_min = gs.iter().fold(f32::INFINITY, |m, g| m.min(g.x));
                        let x_max = gs.iter().fold(f32::NEG_INFINITY, |m, g| m.max(g.x + g.w));
                        let width = x_max - x_min;
                        if width <= 0.0 {
                            continue;
                        }
                        let fs_px = span.font_size;
                        let thickness =
                            (span.data.underline_metrics.thickness * fs_px).max(1.0) * s;
                        let color = span
                            .data
                            .text_decoration
                            .underline_color_opt
                            .or(span.color_opt)
                            .map(|c| [c.r(), c.g(), c.b(), c.a()])
                            .unwrap_or([26, 26, 26, 255]);
                        // Underline top edge, top-down in points, then flip to PDF's
                        // bottom-up frame (rectangle y is its bottom edge).
                        let uy = baseline - span.data.underline_metrics.offset * fs_px * s;
                        let x = (cx + x_min) * s;
                        let w = width * s;
                        let mut push = |uy_top: f32| {
                            rects.push(FillRect {
                                x,
                                y: page_h - (uy_top + thickness),
                                w,
                                h: thickness,
                                color,
                            });
                        };
                        push(uy);
                        if underline == UnderlineStyle::Double {
                            push(uy + thickness * 2.0);
                        }
                    }
                }
            }
            pages.push((glyphs, rects));
        }

        let mut doc = Document::with_version("1.5");
        let pages_id = doc.new_object_id();

        let mut type0_ids = Vec::with_capacity(font_ids.len());
        for (i, id) in font_ids.iter().enumerate() {
            let font = font_system
                .get_font(*id, font_weight[i])
                .ok_or_else(|| "failed to load a document font for embedding".to_string())?;
            let data = font.data().to_vec();
            let length1 = data.len() as i64;
            let base = format!("IAIFont{i}");

            let font_file = doc.add_object(
                Stream::new(
                    dictionary! { "Length1" => length1, "Filter" => "FlateDecode" },
                    deflate(&data),
                )
                .with_compression(false),
            );
            let descriptor = doc.add_object(dictionary! {
                "Type" => "FontDescriptor",
                "FontName" => Object::Name(base.clone().into_bytes()),
                "Flags" => 4,
                "FontBBox" => vec![0.into(), (-250).into(), 1000.into(), 1000.into()],
                "ItalicAngle" => 0,
                "Ascent" => 800,
                "Descent" => (-250),
                "CapHeight" => 700,
                "StemV" => 80,
                "FontFile2" => font_file,
            });
            let cid = doc.add_object(dictionary! {
                "Type" => "Font",
                "Subtype" => "CIDFontType2",
                "BaseFont" => Object::Name(base.clone().into_bytes()),
                "CIDSystemInfo" => dictionary! {
                    "Registry" => Object::String(b"Adobe".to_vec(), StringFormat::Literal),
                    "Ordering" => Object::String(b"Identity".to_vec(), StringFormat::Literal),
                    "Supplement" => 0,
                },
                "FontDescriptor" => descriptor,
                "CIDToGIDMap" => "Identity",
                "DW" => 1000,
            });
            let cmap = build_to_unicode(&to_unicode[i]);
            let to_uni = doc.add_object(
                Stream::new(
                    dictionary! { "Filter" => "FlateDecode" },
                    deflate(cmap.as_bytes()),
                )
                .with_compression(false),
            );
            let type0 = doc.add_object(dictionary! {
                "Type" => "Font",
                "Subtype" => "Type0",
                "BaseFont" => Object::Name(base.into_bytes()),
                "Encoding" => "Identity-H",
                "DescendantFonts" => vec![cid.into()],
                "ToUnicode" => to_uni,
            });
            type0_ids.push(type0);
        }

        let mut font_dict = Dictionary::new();
        for (i, type0) in type0_ids.iter().enumerate() {
            font_dict.set(format!("F{i}"), *type0);
        }
        let resources = doc.add_object(dictionary! { "Font" => font_dict });

        let mut kids: Vec<Object> = Vec::with_capacity(pages.len());
        for (glyphs, rects) in &pages {
            let content = page_content(glyphs, rects);
            let content_id = doc.add_object(
                Stream::new(
                    dictionary! { "Filter" => "FlateDecode" },
                    deflate(content.as_bytes()),
                )
                .with_compression(false),
            );
            let page_id = doc.add_object(dictionary! {
                "Type" => "Page",
                "Parent" => pages_id,
                "MediaBox" => vec![0.into(), 0.into(), Object::Real(page_w), Object::Real(page_h)],
                "Contents" => content_id,
                "Resources" => resources,
            });
            kids.push(page_id.into());
        }

        let count = kids.len() as i64;
        doc.objects.insert(
            pages_id,
            Object::Dictionary(dictionary! {
                "Type" => "Pages",
                "Kids" => kids,
                "Count" => count,
            }),
        );
        let catalog = doc.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages_id });
        doc.trailer.set("Root", catalog);
        doc.change_producer("iAi Document");
        doc.save(path)
            .map(|_| ())
            .map_err(|e| format!("Could not write PDF: {e}"))
    }
}

fn deflate(bytes: &[u8]) -> Vec<u8> {
    let mut encoder = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
    let _ = encoder.write_all(bytes);
    encoder.finish().unwrap_or_default()
}

/// Place each glyph by absolute text matrix, re-emitting `Tf`/`rg` only when the
/// font, size or colour changes, then fill any underline rectangles (path fills
/// must sit outside the `BT`/`ET` text object).
fn page_content(glyphs: &[Glyph], rects: &[FillRect]) -> String {
    let mut out = String::new();
    let mut cur: Option<(usize, u32, [u8; 4])> = None;
    let mut open = false;
    for g in glyphs {
        let key = (g.font, g.size.to_bits(), g.color);
        if cur != Some(key) {
            if open {
                out.push_str("ET\n");
            }
            out.push_str("BT\n");
            out.push_str(&format!("/F{} {:.2} Tf\n", g.font, g.size));
            out.push_str(&format!(
                "{:.4} {:.4} {:.4} rg\n",
                g.color[0] as f32 / 255.0,
                g.color[1] as f32 / 255.0,
                g.color[2] as f32 / 255.0
            ));
            open = true;
            cur = Some(key);
        }
        out.push_str(&format!(
            "1 0 0 1 {:.2} {:.2} Tm <{:04X}> Tj\n",
            g.x, g.y, g.gid
        ));
    }
    if open {
        out.push_str("ET\n");
    }
    let mut fill: Option<[u8; 4]> = None;
    for r in rects {
        if fill != Some(r.color) {
            out.push_str(&format!(
                "{:.4} {:.4} {:.4} rg\n",
                r.color[0] as f32 / 255.0,
                r.color[1] as f32 / 255.0,
                r.color[2] as f32 / 255.0
            ));
            fill = Some(r.color);
        }
        out.push_str(&format!(
            "{:.2} {:.2} {:.2} {:.2} re f\n",
            r.x, r.y, r.w, r.h
        ));
    }
    out
}

/// A ToUnicode CMap mapping glyph ids back to source text (for copy / search).
fn build_to_unicode(map: &BTreeMap<u16, String>) -> String {
    let mut s = String::new();
    s.push_str("/CIDInit /ProcSet findresource begin\n12 dict begin\nbegincmap\n");
    s.push_str("/CIDSystemInfo << /Registry (Adobe) /Ordering (UCS) /Supplement 0 >> def\n");
    s.push_str("/CMapName /Adobe-Identity-UCS def\n/CMapType 2 def\n");
    s.push_str("1 begincodespacerange\n<0000> <FFFF>\nendcodespacerange\n");
    let entries: Vec<_> = map.iter().collect();
    for chunk in entries.chunks(100) {
        s.push_str(&format!("{} beginbfchar\n", chunk.len()));
        for (gid, src) in chunk {
            let utf16: String = src.encode_utf16().map(|u| format!("{u:04X}")).collect();
            s.push_str(&format!("<{gid:04X}> <{utf16}>\n"));
        }
        s.push_str("endbfchar\n");
    }
    s.push_str("endcmap\nCMapName currentdict /CMap defineresource pop\nend\nend\n");
    s
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
    fn wider_line_spacing_uses_more_pages() {
        let mut fs = FontSystem::new();
        let body = "Một dòng văn bản\n".repeat(60);
        let mut tight = TextDocument::from_plain_text(&body);
        for p in &mut tight.paragraphs {
            p.style.line_spacing = 1.0;
        }
        let mut loose = TextDocument::from_plain_text(&body);
        for p in &mut loose.paragraphs {
            p.style.line_spacing = 2.5;
        }
        let tight_pages = DocumentLayout::build(&tight, DPI, &mut fs).page_count();
        let loose_pages = DocumentLayout::build(&loose, DPI, &mut fs).page_count();
        assert!(
            loose_pages > tight_pages,
            "2.5x spacing ({loose_pages}p) should need more pages than 1.0x ({tight_pages}p)"
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

    #[test]
    fn underlined_run_emits_a_fill_and_stays_valid() {
        let mut fs = FontSystem::new();
        let mut ul = CharStyle::default();
        ul.underline = true;
        let doc = TextDocument {
            paragraphs: vec![Paragraph {
                runs: vec![Run::new("Chữ ký bên A", ul)],
                style: ParagraphStyle::default(),
            }],
            ..Default::default()
        };
        let layout = DocumentLayout::build(&doc, DPI, &mut fs);
        let path =
            std::env::temp_dir().join(format!("iai_underline_pdf_{}_test.pdf", std::process::id()));
        layout.write_text_pdf(&mut fs, &path).unwrap();
        let reloaded = lopdf::Document::load(&path).expect("re-parse underlined PDF");
        // The single page's content stream must contain a rectangle-fill op for
        // the underline (`re` ... `f`), decoded from its FlateDecode stream.
        let (_, page_id) = reloaded.get_pages().into_iter().next().unwrap();
        let content = reloaded.get_page_content(page_id).expect("page content");
        let text = String::from_utf8_lossy(&content);
        assert!(
            text.contains("re f"),
            "expected an underline rectangle fill"
        );
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn write_text_pdf_embeds_a_valid_pdf() {
        let mut fs = FontSystem::new();
        let doc = TextDocument::from_plain_text("Cộng hòa xã hội\nĐộc lập – Tự do");
        let layout = DocumentLayout::build(&doc, DPI, &mut fs);
        let path = std::env::temp_dir().join("iai_write_text_pdf_test.pdf");
        layout.write_text_pdf(&mut fs, &path).unwrap();
        let bytes = std::fs::read(&path).unwrap();
        assert!(bytes.starts_with(b"%PDF"), "missing PDF header");
        assert!(
            bytes.len() > 2000,
            "an embedded font should make it non-trivial"
        );
        let reloaded = lopdf::Document::load(&path).expect("re-parse written PDF");
        assert_eq!(reloaded.get_pages().len(), 1);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn write_text_pdf_pages_respects_unified_export_scope() {
        let mut fs = FontSystem::new();
        let doc = TextDocument::from_plain_text(&"Một dòng văn bản\n".repeat(180));
        let layout = DocumentLayout::build(&doc, DPI, &mut fs);
        assert!(layout.page_count() > 1);
        let path = std::env::temp_dir().join(format!(
            "iai_text_pdf_scope_{}_test.pdf",
            std::process::id()
        ));
        layout.write_text_pdf_pages(&mut fs, &path, &[1]).unwrap();
        let reloaded = lopdf::Document::load(&path).expect("re-parse selected-page PDF");
        assert_eq!(reloaded.get_pages().len(), 1);
        let _ = std::fs::remove_file(path);
    }
}
