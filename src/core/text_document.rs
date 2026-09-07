#![allow(dead_code)]
//! Document-mode text model — the "Word-basic" flowing-text document.
//!
//! This is the phase-1 foundation for the lightweight word processor (see
//! `docs/planning/KE_HOACH_TRINH_SOAN_VAN_BAN_2026-09-01.md`). It is deliberately
//! **engine-agnostic**: it holds paragraphs, runs and page setup as plain data
//! and knows nothing about `cosmic-text`, egui or the tile compositor. Layout,
//! editing and rendering (phase 2+) consume this model; keeping the model pure
//! lets it be unit-tested in isolation and reused by PDF/.iai export later.
//!
//! It is intentionally SEPARATE from the graphical [`crate::core::text`] text box
//! (`TextData`), which bakes styled glyphs into raster tiles. A flowing document
//! is a sequence of paragraphs that re-flow to the page width instead.
//!
//! Mapping to the chosen layout engine (phase 2, verified against cosmic-text
//! 0.19 in `src/bin/text_spike.rs`): a [`Run`] becomes one rich-text span whose
//! `Attrs` carry `.family(Family::Name(font.name()))`, `.weight(BOLD)` when bold,
//! `.style(Italic)` when italic, `.color(..)`, and `.metrics(size_px, leading)`;
//! [`ParagraphAlign`] maps to `cosmic_text::Align`. Underline is drawn by the
//! renderer, not carried in shaping.

use crate::core::color::Color;
use crate::core::geometry::Point;
use crate::core::page::{Page, PageId};
use crate::core::text::TextFontFamily;
use crate::core::units::{to_pixels, Unit};

/// Canonical DPI for turning the model's physical units (mm / pt) into the
/// pixel space the canvas and layout engine work in. Callers that render at a
/// different zoom pass their own dpi to the `*_px` helpers.
pub const DEFAULT_DPI: f32 = 96.0;

// ---------------------------------------------------------------------------
// Character style
// ---------------------------------------------------------------------------

/// Style shared by one contiguous stretch of characters (a [`Run`]).
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct CharStyle {
    /// Font identity; reuses [`TextFontFamily`] so the existing system-font index
    /// and Vietnamese handling are shared with the graphical text tool.
    pub font: TextFontFamily,
    /// Point size (as shown in a word processor's size box).
    pub size_pt: f32,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    /// Straight sRGB ink colour.
    pub color: Color,
}

impl Default for CharStyle {
    /// Times New Roman 13pt black — the default body face for Vietnamese office
    /// documents (Nghị định 30/2020), which is what this app is built for.
    fn default() -> Self {
        Self {
            font: TextFontFamily::System("Times New Roman".to_string()),
            size_pt: 13.0,
            bold: false,
            italic: false,
            underline: false,
            color: Color::BLACK,
        }
    }
}

impl CharStyle {
    /// Font size in pixels at `dpi` (1pt = 1/72in).
    pub fn size_px(&self, dpi: f32) -> f32 {
        to_pixels(self.size_pt, Unit::Points, dpi, 0.0)
    }
}

// ---------------------------------------------------------------------------
// Runs and paragraphs
// ---------------------------------------------------------------------------

/// A contiguous stretch of text sharing one [`CharStyle`].
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Run {
    pub text: String,
    pub style: CharStyle,
}

impl Run {
    pub fn new(text: impl Into<String>, style: CharStyle) -> Self {
        Self {
            text: text.into(),
            style,
        }
    }
}

/// Horizontal alignment of a paragraph. Richer than [`crate::core::text::TextAlign`]
/// (the graphical text box) because a document needs justification.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub enum ParagraphAlign {
    #[default]
    Left,
    Center,
    Right,
    Justify,
}

/// List decoration for a paragraph.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub enum ListKind {
    #[default]
    None,
    Bullet,
    Numbered,
}

/// Paragraph-level style. Spacing / indents are in points to match the units a
/// word processor exposes; they convert to pixels at render time.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ParagraphStyle {
    pub align: ParagraphAlign,
    /// Line spacing as a multiple of single spacing (1.0, 1.5, 2.0, ...).
    pub line_spacing: f32,
    pub space_before_pt: f32,
    pub space_after_pt: f32,
    /// First-line indent, added to the left edge for the first line only.
    pub indent_first_pt: f32,
    pub indent_left_pt: f32,
    pub indent_right_pt: f32,
    pub list: ListKind,
}

impl Default for ParagraphStyle {
    fn default() -> Self {
        Self {
            align: ParagraphAlign::Left,
            line_spacing: 1.0,
            space_before_pt: 0.0,
            space_after_pt: 0.0,
            indent_first_pt: 0.0,
            indent_left_pt: 0.0,
            indent_right_pt: 0.0,
            list: ListKind::None,
        }
    }
}

/// Base64 (de)serialisation for the raw image bytes, so an [`ImageBlock`] is a
/// self-contained JSON value that round-trips through the `.iai` manifest.
mod image_b64 {
    use base64::Engine;
    use serde::Deserialize;

    pub fn serialize<S: serde::Serializer>(bytes: &[u8], s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&base64::engine::general_purpose::STANDARD.encode(bytes))
    }

    pub fn deserialize<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Vec<u8>, D::Error> {
        let text = String::deserialize(d)?;
        base64::engine::general_purpose::STANDARD
            .decode(text.as_bytes())
            .map_err(serde::de::Error::custom)
    }
}

/// How an image relates to the surrounding text — mirrors Word's layout
/// options. `Inline` flows in the paragraph; the rest float at a page position.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub enum ImageWrap {
    /// In line with text: the image behaves like one character in a paragraph.
    #[default]
    Inline,
    /// Floating, drawn behind the text; the text is not moved.
    BehindText,
    /// Floating, drawn in front of the text; the text is not moved.
    InFrontOfText,
    /// Floating; text wraps around the image's box (square).
    Square,
    /// Top and Bottom: the image occupies its own movable flow block, so text
    /// appears only above and below it.
    TopBottom,
}

impl ImageWrap {
    /// True for every mode except `Inline` (i.e. positioned on the page).
    pub fn is_floating(self) -> bool {
        !matches!(self, ImageWrap::Inline)
    }

    /// True when text must avoid the image (its box for `Square`, its full-width
    /// vertical band for `TopBottom`).
    pub fn excludes_text(self) -> bool {
        matches!(self, ImageWrap::Square | ImageWrap::TopBottom)
    }
}

/// An image (letterhead, signature, stamp). When `wrap` is `Inline` it can be
/// anchored inside a paragraph; otherwise it floats at `(page, x_mm, y_mm)`
/// from that page's top-left corner. The encoded bytes travel with the document
/// so it embeds directly into PDF and needs no external file.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ImageBlock {
    /// Encoded image bytes (PNG or JPEG), base64 in the manifest.
    #[serde(with = "image_b64")]
    pub data: Vec<u8>,
    /// Natural pixel size, kept for the aspect ratio.
    pub natural_w: u32,
    pub natural_h: u32,
    /// Displayed width in millimetres; height follows the aspect ratio.
    pub width_mm: f32,
    /// Horizontal placement retained for legacy standalone image paragraphs.
    /// A true inline image follows the surrounding glyphs instead.
    pub align: ParagraphAlign,
    /// Relationship to the text. `Inline` (default) behaves like one character.
    #[serde(default)]
    pub wrap: ImageWrap,
    /// Floating anchor: 0-based page and offset (mm) from its top-left corner.
    #[serde(default)]
    pub page: usize,
    #[serde(default)]
    pub x_mm: f32,
    #[serde(default)]
    pub y_mm: f32,
}

/// One image that behaves like a character inside a text paragraph. The byte
/// offset is measured in the paragraph's UTF-8 text (which excludes the image
/// itself), allowing text before and after the image to retain rich styles.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct InlineImage {
    pub byte_offset: usize,
    pub image: ImageBlock,
}

impl ImageBlock {
    /// An inline image (the default relationship): flows in the paragraph.
    pub fn inline(
        data: Vec<u8>,
        natural_w: u32,
        natural_h: u32,
        width_mm: f32,
        align: ParagraphAlign,
    ) -> Self {
        Self {
            data,
            natural_w,
            natural_h,
            width_mm,
            align,
            wrap: ImageWrap::Inline,
            page: 0,
            x_mm: 0.0,
            y_mm: 0.0,
        }
    }

    /// Displayed height in millimetres, from `width_mm` and the aspect ratio.
    pub fn height_mm(&self) -> f32 {
        if self.natural_w == 0 {
            return 0.0;
        }
        self.width_mm * self.natural_h as f32 / self.natural_w as f32
    }
}

/// One paragraph: either styled text or a single block image, plus
/// paragraph-level formatting. A paragraph with no runs (or only empty runs)
/// and no image is a legal empty line.
#[derive(Debug, Clone, PartialEq, Default, serde::Serialize, serde::Deserialize)]
pub struct Paragraph {
    pub runs: Vec<Run>,
    pub style: ParagraphStyle,
    /// When set, this paragraph is an image block; `runs` are ignored.
    #[serde(default)]
    pub image: Option<ImageBlock>,
    /// Images embedded between characters in this paragraph. Legacy block
    /// images remain supported through `image` for backward compatibility.
    #[serde(default)]
    pub inline_images: Vec<InlineImage>,
}

impl Paragraph {
    /// An empty paragraph with default formatting (a blank line).
    pub fn empty() -> Self {
        Self::default()
    }

    /// A paragraph holding one styled run of `text` with default paragraph
    /// formatting. Empty `text` yields a runless (blank) paragraph.
    pub fn plain(text: impl Into<String>, style: CharStyle) -> Self {
        let text = text.into();
        let runs = if text.is_empty() {
            Vec::new()
        } else {
            vec![Run::new(text, style)]
        };
        Self {
            runs,
            style: ParagraphStyle::default(),
            image: None,
            inline_images: Vec::new(),
        }
    }

    /// A paragraph that is a single block image.
    pub fn image(block: ImageBlock) -> Self {
        Self {
            runs: Vec::new(),
            style: ParagraphStyle::default(),
            image: Some(block),
            inline_images: Vec::new(),
        }
    }

    /// True when this paragraph is an image block rather than text.
    pub fn is_image(&self) -> bool {
        self.image.is_some()
    }

    /// Concatenated plain text of every run.
    pub fn text(&self) -> String {
        self.runs.iter().map(|r| r.text.as_str()).collect()
    }

    /// Number of Unicode scalar values across all runs.
    pub fn char_len(&self) -> usize {
        self.runs.iter().map(|r| r.text.chars().count()).sum()
    }

    /// True when the paragraph carries no visible text and no image.
    pub fn is_empty(&self) -> bool {
        self.image.is_none()
            && self.inline_images.is_empty()
            && self.runs.iter().all(|r| r.text.is_empty())
    }

    /// Canonicalise: drop empty runs and merge adjacent runs with identical
    /// style. Keeps equality/diffing meaningful and layout input minimal.
    pub fn normalize(&mut self) {
        let mut merged: Vec<Run> = Vec::with_capacity(self.runs.len());
        for run in self.runs.drain(..) {
            if run.text.is_empty() {
                continue;
            }
            match merged.last_mut() {
                Some(last) if last.style == run.style => last.text.push_str(&run.text),
                _ => merged.push(run),
            }
        }
        self.runs = merged;
        let text = self.text();
        for inline in &mut self.inline_images {
            inline.byte_offset = inline.byte_offset.min(text.len());
            while !text.is_char_boundary(inline.byte_offset) {
                inline.byte_offset -= 1;
            }
            inline.image.wrap = ImageWrap::Inline;
        }
        self.inline_images.sort_by_key(|inline| inline.byte_offset);
    }
}

// ---------------------------------------------------------------------------
// Page setup
// ---------------------------------------------------------------------------

/// Physical paper size in millimetres.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct PaperSize {
    pub width_mm: f32,
    pub height_mm: f32,
}

impl PaperSize {
    pub const A4: Self = Self {
        width_mm: 210.0,
        height_mm: 297.0,
    };
    pub const A5: Self = Self {
        width_mm: 148.0,
        height_mm: 210.0,
    };
    /// US Letter, 8.5 × 11 in.
    pub const LETTER: Self = Self {
        width_mm: 215.9,
        height_mm: 279.4,
    };

    pub fn width_px(&self, dpi: f32) -> f32 {
        to_pixels(self.width_mm, Unit::Millimeters, dpi, 0.0)
    }

    pub fn height_px(&self, dpi: f32) -> f32 {
        to_pixels(self.height_mm, Unit::Millimeters, dpi, 0.0)
    }
}

/// Page margins in millimetres.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Margins {
    pub top_mm: f32,
    pub right_mm: f32,
    pub bottom_mm: f32,
    pub left_mm: f32,
}

impl Margins {
    pub fn uniform(mm: f32) -> Self {
        Self {
            top_mm: mm,
            right_mm: mm,
            bottom_mm: mm,
            left_mm: mm,
        }
    }
}

impl Default for Margins {
    /// Vietnamese office-document margins (Nghị định 30/2020): a wider left
    /// margin for binding, 20 mm elsewhere.
    fn default() -> Self {
        Self {
            top_mm: 20.0,
            right_mm: 20.0,
            bottom_mm: 20.0,
            left_mm: 30.0,
        }
    }
}

/// Paper size + margins for a document. The text column is `paper − margins`.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct PageSetup {
    pub paper: PaperSize,
    pub margins: Margins,
}

impl Default for PageSetup {
    fn default() -> Self {
        Self {
            paper: PaperSize::A4,
            margins: Margins::default(),
        }
    }
}

impl PageSetup {
    pub fn content_width_mm(&self) -> f32 {
        self.paper.width_mm - self.margins.left_mm - self.margins.right_mm
    }

    pub fn content_height_mm(&self) -> f32 {
        self.paper.height_mm - self.margins.top_mm - self.margins.bottom_mm
    }

    pub fn content_width_px(&self, dpi: f32) -> f32 {
        to_pixels(self.content_width_mm(), Unit::Millimeters, dpi, 0.0)
    }

    pub fn content_height_px(&self, dpi: f32) -> f32 {
        to_pixels(self.content_height_mm(), Unit::Millimeters, dpi, 0.0)
    }

    /// Text column rectangle in page-local pixels: `(x, y, width, height)` with
    /// the origin at the top-left margin corner.
    pub fn content_rect_px(&self, dpi: f32) -> (f32, f32, f32, f32) {
        (
            to_pixels(self.margins.left_mm, Unit::Millimeters, dpi, 0.0),
            to_pixels(self.margins.top_mm, Unit::Millimeters, dpi, 0.0),
            self.content_width_px(dpi),
            self.content_height_px(dpi),
        )
    }

    /// Bridge to the reserved multi-page slot: build the physical sheet as a
    /// [`Page`] in document-space pixels (white paper, no bleed). Text margins
    /// stay in [`PageSetup`]; `Page::margin` is a separate print safety margin.
    pub fn to_page(&self, id: PageId, origin: Point, dpi: f32) -> Page {
        Page {
            id,
            origin,
            size: (self.paper.width_px(dpi), self.paper.height_px(dpi)),
            bleed: 0.0,
            margin: 0.0,
            background: Some([255, 255, 255, 255]),
        }
    }

    pub fn validate(&self) -> Result<(), String> {
        let p = self.paper;
        let m = self.margins;
        let finite = [
            p.width_mm,
            p.height_mm,
            m.top_mm,
            m.right_mm,
            m.bottom_mm,
            m.left_mm,
        ]
        .iter()
        .all(|v| v.is_finite());
        if !finite {
            return Err("page setup has non-finite dimensions".into());
        }
        if p.width_mm <= 0.0 || p.height_mm <= 0.0 {
            return Err("paper size must be positive".into());
        }
        if m.top_mm < 0.0 || m.right_mm < 0.0 || m.bottom_mm < 0.0 || m.left_mm < 0.0 {
            return Err("margins must be >= 0".into());
        }
        if m.left_mm + m.right_mm >= p.width_mm {
            return Err("horizontal margins exceed paper width".into());
        }
        if m.top_mm + m.bottom_mm >= p.height_mm {
            return Err("vertical margins exceed paper height".into());
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Document
// ---------------------------------------------------------------------------

/// A flowing-text document: an ordered list of paragraphs plus page setup and
/// the styles new text inherits. Always holds at least one paragraph so an
/// editor cursor has a home.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct TextDocument {
    pub paragraphs: Vec<Paragraph>,
    pub page: PageSetup,
    pub default_char: CharStyle,
    pub default_para: ParagraphStyle,
    /// Floating images (Word-style wrapping): anchored to a page position rather
    /// than the text flow. Inline images stay on their owning paragraph.
    #[serde(default)]
    pub floating_images: Vec<ImageBlock>,
}

impl Default for TextDocument {
    fn default() -> Self {
        Self {
            paragraphs: vec![Paragraph::empty()],
            page: PageSetup::default(),
            default_char: CharStyle::default(),
            default_para: ParagraphStyle::default(),
            floating_images: Vec::new(),
        }
    }
}

impl TextDocument {
    pub fn new() -> Self {
        Self::default()
    }

    /// Build a document from plain text, one paragraph per `\n`-separated line,
    /// each a single default-styled run. Never yields zero paragraphs.
    pub fn from_plain_text(s: &str) -> Self {
        let style = CharStyle::default();
        let paragraphs: Vec<Paragraph> = s
            .split('\n')
            .map(|line| Paragraph::plain(line, style.clone()))
            .collect();
        Self {
            paragraphs,
            ..Self::default()
        }
    }

    /// Round-trips with [`TextDocument::from_plain_text`]: paragraphs joined by
    /// `\n`.
    pub fn plain_text(&self) -> String {
        self.paragraphs
            .iter()
            .map(|p| p.text())
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Total Unicode scalar values across all paragraphs (excludes the implicit
    /// paragraph breaks).
    pub fn char_count(&self) -> usize {
        self.paragraphs.iter().map(|p| p.char_len()).sum()
    }

    /// True when the document is a single blank paragraph.
    pub fn is_empty(&self) -> bool {
        self.paragraphs.len() == 1 && self.paragraphs[0].is_empty()
    }

    /// Normalise every paragraph and guarantee the one-paragraph invariant.
    pub fn normalize(&mut self) {
        for p in &mut self.paragraphs {
            p.normalize();
        }
        if self.paragraphs.is_empty() {
            self.paragraphs.push(Paragraph::empty());
        }
    }

    /// Replace the retired inline-image mode with Word-style Top and Bottom
    /// blocks. Text on either side of an object-character anchor becomes two
    /// paragraphs with the original rich runs preserved. Legacy image-only
    /// paragraphs keep their position and are simply relabelled.
    ///
    /// Returns the number of converted images.
    pub fn migrate_inline_images_to_top_bottom(&mut self) -> usize {
        self.normalize();
        let mut migrated = 0;
        let mut paragraphs = Vec::with_capacity(self.paragraphs.len());
        for mut paragraph in self.paragraphs.drain(..) {
            if let Some(image) = paragraph.image.as_mut() {
                if image.wrap == ImageWrap::Inline {
                    image.wrap = ImageWrap::TopBottom;
                    paragraph.style.align = image.align;
                    migrated += 1;
                }
                paragraphs.push(paragraph);
                continue;
            }
            if paragraph.inline_images.is_empty() {
                paragraphs.push(paragraph);
                continue;
            }

            let source_text = paragraph.text();
            let anchors = std::mem::take(&mut paragraph.inline_images);
            let mut start = 0usize;
            for mut inline in anchors {
                let at = inline.byte_offset.min(source_text.len());
                if at > start {
                    paragraphs.push(Paragraph {
                        runs: slice_runs(&paragraph.runs, start, at),
                        style: paragraph.style.clone(),
                        image: None,
                        inline_images: Vec::new(),
                    });
                }
                inline.image.wrap = ImageWrap::TopBottom;
                let mut image_paragraph = Paragraph::image(inline.image);
                image_paragraph.style = paragraph.style.clone();
                image_paragraph.style.align = image_paragraph
                    .image
                    .as_ref()
                    .map_or(ParagraphAlign::Center, |image| image.align);
                paragraphs.push(image_paragraph);
                migrated += 1;
                start = at;
            }
            if start < source_text.len() {
                paragraphs.push(Paragraph {
                    runs: slice_runs(&paragraph.runs, start, source_text.len()),
                    style: paragraph.style,
                    image: None,
                    inline_images: Vec::new(),
                });
            }
        }
        self.paragraphs = paragraphs;
        self.normalize();
        migrated
    }

    pub fn validate(&self) -> Result<(), String> {
        self.page.validate()?;
        if self.paragraphs.is_empty() {
            return Err("document must have at least one paragraph".into());
        }
        for (paragraph_index, paragraph) in self.paragraphs.iter().enumerate() {
            let text = paragraph.text();
            for inline in &paragraph.inline_images {
                if inline.byte_offset > text.len() || !text.is_char_boundary(inline.byte_offset) {
                    return Err(format!(
                        "paragraph {paragraph_index} has an invalid inline-image anchor"
                    ));
                }
                if inline.image.wrap != ImageWrap::Inline {
                    return Err(format!(
                        "paragraph {paragraph_index} has a non-inline image in inline_images"
                    ));
                }
            }
        }
        Ok(())
    }
}

/// Copy a UTF-8 byte range from rich runs without flattening their styles.
fn slice_runs(runs: &[Run], start: usize, end: usize) -> Vec<Run> {
    let mut out = Vec::new();
    let mut run_start = 0usize;
    for run in runs {
        let run_end = run_start + run.text.len();
        let overlap_start = start.max(run_start);
        let overlap_end = end.min(run_end);
        if overlap_start < overlap_end {
            out.push(Run::new(
                &run.text[overlap_start - run_start..overlap_end - run_start],
                run.style.clone(),
            ));
        }
        run_start = run_end;
        if run_start >= end {
            break;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn approx(a: f32, b: f32) {
        assert!((a - b).abs() < 0.01, "expected {b}, got {a}");
    }

    #[test]
    fn char_style_default_is_vietnamese_body_face() {
        let s = CharStyle::default();
        assert_eq!(s.font.name(), "Times New Roman");
        assert_eq!(s.size_pt, 13.0);
        assert_eq!(s.color, Color::BLACK);
        assert!(!s.bold && !s.italic && !s.underline);
    }

    #[test]
    fn point_size_converts_to_pixels() {
        // 13pt at 96dpi = 13 * 96 / 72.
        approx(CharStyle::default().size_px(96.0), 13.0 * 96.0 / 72.0);
        // 72pt at 72dpi is exactly 72px.
        let mut s = CharStyle::default();
        s.size_pt = 72.0;
        approx(s.size_px(72.0), 72.0);
    }

    #[test]
    fn plain_paragraph_holds_one_run() {
        let p = Paragraph::plain("Xin chào", CharStyle::default());
        assert_eq!(p.runs.len(), 1);
        assert_eq!(p.text(), "Xin chào");
        assert_eq!(p.char_len(), 8);
        assert!(!p.is_empty());
    }

    #[test]
    fn empty_text_makes_a_blank_paragraph() {
        let p = Paragraph::plain("", CharStyle::default());
        assert!(p.runs.is_empty());
        assert!(p.is_empty());
        assert_eq!(p.char_len(), 0);
    }

    #[test]
    fn inline_image_anchor_round_trips_and_normalizes_to_utf8_boundary() {
        let mut p = Paragraph::plain("a Việt", CharStyle::default());
        p.inline_images.push(InlineImage {
            byte_offset: 5, // inside the multi-byte 'ệ' scalar
            image: ImageBlock::inline(vec![1, 2, 3], 20, 10, 12.0, ParagraphAlign::Left),
        });
        p.normalize();
        assert!(p.text().is_char_boundary(p.inline_images[0].byte_offset));
        assert_eq!(p.inline_images[0].image.wrap, ImageWrap::Inline);

        let json = serde_json::to_string(&p).unwrap();
        let back: Paragraph = serde_json::from_str(&json).unwrap();
        assert_eq!(back, p);
        assert!(!back.is_empty());
    }

    #[test]
    fn normalize_merges_same_style_and_drops_empties() {
        let s = CharStyle::default();
        let mut bold = s.clone();
        bold.bold = true;
        let mut p = Paragraph {
            runs: vec![
                Run::new("Xin ", s.clone()),
                Run::new("", s.clone()),
                Run::new("chào ", s.clone()),
                Run::new("Việt", bold.clone()),
            ],
            style: ParagraphStyle::default(),
            image: None,
            inline_images: Vec::new(),
        };
        p.normalize();
        assert_eq!(p.runs.len(), 2, "same-style runs merge, empties drop");
        assert_eq!(p.runs[0].text, "Xin chào ");
        assert_eq!(p.runs[0].style, s);
        assert_eq!(p.runs[1].text, "Việt");
        assert!(p.runs[1].style.bold);
        assert_eq!(p.text(), "Xin chào Việt");
    }

    #[test]
    fn paragraph_align_and_list_default_sensibly() {
        assert_eq!(ParagraphAlign::default(), ParagraphAlign::Left);
        assert_eq!(ListKind::default(), ListKind::None);
        let ps = ParagraphStyle::default();
        assert_eq!(ps.line_spacing, 1.0);
        assert_eq!(ps.align, ParagraphAlign::Left);
    }

    #[test]
    fn a4_content_column_is_paper_minus_margins() {
        let setup = PageSetup::default(); // A4 + VN margins
        approx(setup.content_width_mm(), 210.0 - 30.0 - 20.0);
        approx(setup.content_height_mm(), 297.0 - 20.0 - 20.0);
        // Content rect origin sits at the top-left margin.
        let (x, y, w, h) = setup.content_rect_px(96.0);
        approx(x, to_pixels(30.0, Unit::Millimeters, 96.0, 0.0));
        approx(y, to_pixels(20.0, Unit::Millimeters, 96.0, 0.0));
        approx(w, to_pixels(160.0, Unit::Millimeters, 96.0, 0.0));
        approx(h, to_pixels(257.0, Unit::Millimeters, 96.0, 0.0));
    }

    #[test]
    fn page_setup_bridges_to_physical_page() {
        let setup = PageSetup::default();
        let page = setup.to_page(PageId::IMPLICIT, Point::new(0.0, 0.0), 96.0);
        approx(page.size.0, 210.0 * 96.0 / 25.4);
        approx(page.size.1, 297.0 * 96.0 / 25.4);
        assert_eq!(page.background, Some([255, 255, 255, 255]));
        assert!(page.validate().is_ok());
    }

    #[test]
    fn page_setup_validation_rejects_bad_geometry() {
        assert!(PageSetup::default().validate().is_ok());
        let mut bad = PageSetup::default();
        bad.margins.left_mm = 200.0; // > A4 width once added to right
        assert!(bad.validate().is_err());
        let mut nan = PageSetup::default();
        nan.paper.width_mm = f32::NAN;
        assert!(nan.validate().is_err());
    }

    #[test]
    fn document_default_has_one_blank_paragraph() {
        let doc = TextDocument::new();
        assert_eq!(doc.paragraphs.len(), 1);
        assert!(doc.is_empty());
        assert_eq!(doc.char_count(), 0);
        assert!(doc.validate().is_ok());
    }

    #[test]
    fn plain_text_round_trips_through_paragraphs() {
        let src = "CỘNG HÒA XÃ HỘI CHỦ NGHĨA VIỆT NAM\nĐộc lập – Tự do – Hạnh phúc\n\nHỢP ĐỒNG";
        let doc = TextDocument::from_plain_text(src);
        assert_eq!(doc.paragraphs.len(), 4); // blank line becomes an empty paragraph
        assert_eq!(doc.plain_text(), src);
        assert!(doc.paragraphs[2].is_empty());
        assert!(!doc.is_empty());
    }

    #[test]
    fn empty_string_still_makes_one_paragraph() {
        let doc = TextDocument::from_plain_text("");
        assert_eq!(doc.paragraphs.len(), 1);
        assert!(doc.is_empty());
    }

    #[test]
    fn image_block_round_trips_through_json_as_base64() {
        let block = ImageBlock::inline(
            vec![0x89, b'P', b'N', b'G', 1, 2, 3, 250, 0, 42],
            400,
            200,
            50.0,
            ParagraphAlign::Center,
        );
        let json = serde_json::to_string(&block).unwrap();
        assert!(json.contains("\"data\""));
        assert!(!json.contains("[137,")); // bytes are base64, not a number array
        let back: ImageBlock = serde_json::from_str(&json).unwrap();
        assert_eq!(back, block);
        approx(block.height_mm(), 25.0); // 50mm * 200/400
    }

    #[test]
    fn floating_image_round_trips_and_defaults_to_inline() {
        // A pre-v10 inline image (no wrap/pos fields) still deserialises.
        let legacy =
            r#"{"data":"AQID","natural_w":10,"natural_h":10,"width_mm":30.0,"align":"Center"}"#;
        let back: ImageBlock = serde_json::from_str(legacy).unwrap();
        assert_eq!(back.wrap, ImageWrap::Inline);
        assert_eq!(back.page, 0);

        // A floating image round-trips its wrap + position through the document.
        let mut fb = ImageBlock::inline(vec![9, 9], 100, 50, 40.0, ParagraphAlign::Left);
        fb.wrap = ImageWrap::InFrontOfText;
        fb.page = 2;
        fb.x_mm = 25.0;
        fb.y_mm = 60.0;
        let doc = TextDocument {
            floating_images: vec![fb.clone()],
            ..TextDocument::default()
        };
        let json = serde_json::to_string(&doc).unwrap();
        let back: TextDocument = serde_json::from_str(&json).unwrap();
        assert_eq!(back.floating_images, vec![fb]);
    }

    #[test]
    fn image_paragraph_is_not_empty_and_carries_no_text() {
        let block = ImageBlock::inline(vec![1, 2, 3], 10, 10, 30.0, ParagraphAlign::Center);
        let p = Paragraph::image(block);
        assert!(p.is_image());
        assert!(!p.is_empty());
        assert_eq!(p.text(), "");
        assert_eq!(p.char_len(), 0);
    }

    #[test]
    fn legacy_inline_image_paragraph_migrates_to_top_bottom() {
        let block = ImageBlock::inline(vec![1, 2, 3], 10, 5, 30.0, ParagraphAlign::Center);
        let mut doc = TextDocument {
            paragraphs: vec![Paragraph::image(block.clone())],
            ..TextDocument::default()
        };

        assert_eq!(doc.migrate_inline_images_to_top_bottom(), 1);
        assert!(doc.paragraphs[0].image.is_some());
        assert_eq!(doc.paragraphs[0].style.align, ParagraphAlign::Center);
        let migrated = doc.paragraphs[0].image.as_ref().unwrap();
        assert_eq!(migrated.data, block.data);
        assert_eq!(migrated.wrap, ImageWrap::TopBottom);
        assert!(doc.validate().is_ok());
    }

    #[test]
    fn inline_anchor_migrates_between_rich_text_without_losing_styles() {
        let mut bold = CharStyle::default();
        bold.bold = true;
        let plain = CharStyle::default();
        let block = ImageBlock::inline(vec![4, 5, 6], 20, 10, 15.0, ParagraphAlign::Right);
        let paragraph = Paragraph {
            runs: vec![
                Run::new("Trước ", bold.clone()),
                Run::new("sau", plain.clone()),
            ],
            style: ParagraphStyle::default(),
            image: None,
            inline_images: vec![InlineImage {
                byte_offset: "Trước ".len(),
                image: block,
            }],
        };
        let mut doc = TextDocument {
            paragraphs: vec![paragraph],
            ..TextDocument::default()
        };

        assert_eq!(doc.migrate_inline_images_to_top_bottom(), 1);
        assert_eq!(doc.paragraphs.len(), 3);
        assert_eq!(doc.paragraphs[0].text(), "Trước ");
        assert!(doc.paragraphs[0].runs[0].style.bold);
        assert_eq!(
            doc.paragraphs[1].image.as_ref().unwrap().wrap,
            ImageWrap::TopBottom
        );
        assert_eq!(doc.paragraphs[1].style.align, ParagraphAlign::Right);
        assert_eq!(doc.paragraphs[2].text(), "sau");
        assert_eq!(doc.paragraphs[2].runs[0].style, plain);
    }

    #[test]
    fn validation_rejects_broken_inline_image_anchors() {
        let mut doc = TextDocument::from_plain_text("Việt");
        doc.paragraphs[0].inline_images.push(InlineImage {
            byte_offset: 3, // inside a multi-byte scalar
            image: ImageBlock::inline(vec![1], 1, 1, 10.0, ParagraphAlign::Left),
        });
        assert!(doc.validate().is_err());

        doc.normalize();
        assert!(doc.validate().is_ok());
    }

    #[test]
    fn paragraph_without_image_field_deserialises_to_none() {
        // A v9-era paragraph JSON has no `image` key; serde(default) fills None.
        let p: Paragraph =
            serde_json::from_str(r#"{"runs":[],"style":{"align":"Left","line_spacing":1.0,"space_before_pt":0.0,"space_after_pt":0.0,"indent_first_pt":0.0,"indent_left_pt":0.0,"indent_right_pt":0.0,"list":"None"}}"#)
                .unwrap();
        assert!(p.image.is_none());
    }
}
