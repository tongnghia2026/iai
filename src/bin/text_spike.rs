//! Phase 0 spike (headless): de-risk the document-mode text engine before
//! committing to `cosmic-text`. Two checks, no window and no compositor:
//!
//!   1. Static page: shape + word-wrap a few Vietnamese paragraphs (including
//!      stacked diacritics) and raster them to a PNG for visual inspection.
//!   2. Editor round-trip: feed VN syllables into `Editor` the way egui's
//!      `Ime::Commit` would, plus Enter/Backspace, then re-shape and render —
//!      proving edits keep VN correct through the shaping cache.
//!
//! Run: cargo run --bin text_spike -- <out_dir>

use cosmic_text::{
    Action, Attrs, Buffer, Color, Edit, Editor, Family, FontSystem, Metrics, Shaping, SwashCache,
};

/// Alpha-blend one swash coverage rect onto an opaque RGBA canvas (white bg).
fn blend(
    pixels: &mut [u8],
    img_w: usize,
    img_h: usize,
    pad: i32,
    rect: (i32, i32, u32, u32, Color),
) {
    let (x, y, w, h, color) = rect;
    let a = color.a() as f32 / 255.0;
    if a <= 0.0 {
        return;
    }
    let (sr, sg, sb) = (color.r() as f32, color.g() as f32, color.b() as f32);
    for dy in 0..h as i32 {
        for dx in 0..w as i32 {
            let px = x + dx + pad;
            let py = y + dy + pad;
            if px < 0 || py < 0 || px as usize >= img_w || py as usize >= img_h {
                continue;
            }
            let idx = (py as usize * img_w + px as usize) * 4;
            for (c, s) in [sr, sg, sb].iter().enumerate() {
                let d = pixels[idx + c] as f32;
                pixels[idx + c] = (s * a + d * (1.0 - a)).round() as u8;
            }
        }
    }
}

fn main() {
    let out_dir = std::env::args().nth(1).unwrap_or_else(|| ".".to_string());

    let t_start = std::time::Instant::now();
    // FontSystem indexes the OS font dir once; it is the shared, one-time cost
    // for the whole document, not per page.
    let mut font_system = FontSystem::new();
    let t_fonts = t_start.elapsed();
    let font_faces = font_system.db().faces().count();
    let mut swash_cache = SwashCache::new();

    let font_size = 22.0_f32;
    let line_height = font_size * 1.4;
    let metrics = Metrics::new(font_size, line_height);
    let col_w = 720.0_f32; // ~A4 text column at 96dpi minus margins
    let col_h = 4000.0_f32;
    let pad = 12i32;
    let times = Attrs::new().family(Family::Name("Times New Roman"));

    // ---- Check 1: static VN page (hard diacritics + wrapping paragraph) ----
    let mut buffer = Buffer::new(&mut font_system, metrics);
    buffer.set_size(Some(col_w), Some(col_h));
    let text = "Kiểm tra dấu chồng: Ể ể Ỡ ỡ Ữ ữ Ệ ệ Ộ ộ Ợ ợ Ậ ậ Ẫ ẫ Ọ ọ Ỉ ỉ Ự ự\n\
                CỘNG HÒA XÃ HỘI CHỦ NGHĨA VIỆT NAM\n\
                Độc lập – Tự do – Hạnh phúc\n\
                HỢP ĐỒNG THUÊ NHÀ\n\
                \n\
                Hôm nay, ngày … tháng … năm 2026, tại Thành phố Hồ Chí Minh, chúng \
                tôi gồm có Bên cho thuê (Bên A) và Bên thuê nhà (Bên B) cùng nhau \
                thỏa thuận ký kết hợp đồng thuê nhà ở với những điều khoản sau đây, \
                nhằm bảo đảm quyền lợi và nghĩa vụ của cả hai bên. Đoạn văn dài cố ý \
                để kiểm tra tự động xuống dòng theo bề rộng cột (word-wrap).";
    buffer.set_text(text, &times, Shaping::Advanced, None);
    buffer.shape_until_scroll(&mut font_system, false);

    let (mut lines1, mut glyphs1, mut bottom1) = (0usize, 0usize, 0.0f32);
    for run in buffer.layout_runs() {
        lines1 += 1;
        glyphs1 += run.glyphs.len();
        bottom1 = bottom1.max(run.line_top + line_height);
    }
    let img_w = col_w.ceil() as usize + pad as usize * 2;
    let img_h1 = (bottom1.ceil() as usize + pad as usize * 2).max(64);
    let mut px1 = vec![255u8; img_w * img_h1 * 4];
    let ink = Color::rgb(0x1a, 0x1a, 0x1a);
    let t_draw0 = std::time::Instant::now();
    buffer.draw(&mut font_system, &mut swash_cache, ink, |x, y, w, h, c| {
        blend(&mut px1, img_w, img_h1, pad, (x, y, w, h, c));
    });
    let t_draw1 = t_draw0.elapsed();
    let page_png = format!("{out_dir}/spike_page.png");
    image::RgbaImage::from_raw(img_w as u32, img_h1 as u32, px1)
        .unwrap()
        .save(&page_png)
        .unwrap();

    // ---- Check 2: editor round-trip (simulated IME + keyboard edits) ----
    let mut ed_buffer = Buffer::new(&mut font_system, metrics);
    ed_buffer.set_size(Some(col_w), Some(col_h));
    let mut editor = Editor::new(ed_buffer);
    // Each string is one composed syllable an external Telex IME (Unikey/EVKey)
    // would hand egui via Ime::Commit — we insert it verbatim.
    for syllable in [
        "Cộng ", "hòa ", "Xã ", "hội ", "chủ ", "nghĩa ", "Việt ", "Namx",
    ] {
        editor.insert_string(syllable, None);
    }
    editor.action(&mut font_system, Action::Backspace); // drop the stray 'x'
    editor.action(&mut font_system, Action::Enter); // new paragraph
    editor.insert_string("Độc lập – Tự do – Hạnh phúc", None);
    editor.shape_as_needed(&mut font_system, false);

    let roundtrip: String = editor.with_buffer(|buf| {
        buf.lines
            .iter()
            .map(|l| l.text())
            .collect::<Vec<_>>()
            .join("\\n")
    });

    let (mut glyphs2, mut bottom2) = (0usize, 0.0f32);
    editor.with_buffer(|buf| {
        for run in buf.layout_runs() {
            glyphs2 += run.glyphs.len();
            bottom2 = bottom2.max(run.line_top + line_height);
        }
    });
    let img_h2 = (bottom2.ceil() as usize + pad as usize * 2).max(64);
    let mut px2 = vec![255u8; img_w * img_h2 * 4];
    let cursor = Color::rgb(0x00, 0x66, 0xff);
    let sel = Color::rgba(0x00, 0x66, 0xff, 0x40);
    editor.draw(
        &mut font_system,
        &mut swash_cache,
        ink,
        cursor,
        sel,
        ink,
        |x, y, w, h, c| {
            blend(&mut px2, img_w, img_h2, pad, (x, y, w, h, c));
        },
    );
    let editor_png = format!("{out_dir}/spike_editor.png");
    image::RgbaImage::from_raw(img_w as u32, img_h2 as u32, px2)
        .unwrap()
        .save(&editor_png)
        .unwrap();

    // ---- Check 3: full model -> layout -> multi-page A4 raster ----
    use iai::core::text_document::{
        CharStyle, Paragraph, ParagraphAlign, ParagraphStyle, Run, TextDocument,
    };
    use iai::core::text_layout::DocumentLayout;

    let body = CharStyle::default();
    let mut bold = CharStyle::default();
    bold.bold = true;
    let centered = |text: &str, style: CharStyle| Paragraph {
        runs: vec![Run::new(text, style)],
        style: ParagraphStyle {
            align: ParagraphAlign::Center,
            ..Default::default()
        },
    };
    let justified = |text: String| Paragraph {
        runs: vec![Run::new(text, body.clone())],
        style: ParagraphStyle {
            align: ParagraphAlign::Justify,
            space_after_pt: 6.0,
            ..Default::default()
        },
    };

    let mut paras = vec![
        centered("CỘNG HÒA XÃ HỘI CHỦ NGHĨA VIỆT NAM", bold.clone()),
        centered("Độc lập – Tự do – Hạnh phúc", body.clone()),
        Paragraph::empty(),
        centered("HỢP ĐỒNG THUÊ NHÀ", bold.clone()),
        Paragraph::empty(),
    ];
    for i in 1..=16 {
        paras.push(justified(format!(
            "Điều {i}: Bên A và Bên B cùng thống nhất các điều khoản dưới đây, được \
             lập thành văn bản dài cố ý nhằm lấp đầy chiều cao trang giấy và kiểm tra \
             việc tự động chảy chữ sang trang tiếp theo khi nội dung vượt quá một trang."
        )));
    }
    let doc = TextDocument {
        paragraphs: paras,
        ..Default::default()
    };
    let layout = DocumentLayout::build(&doc, 96.0, &mut font_system);
    let (dpw, dph) = layout.page_px();
    for p in 0..layout.page_count() {
        let img = layout.render_page(p, &mut font_system, &mut swash_cache);
        let path = format!("{out_dir}/spike_docpage_{p}.png");
        image::RgbaImage::from_raw(dpw as u32, dph as u32, img)
            .unwrap()
            .save(&path)
            .unwrap();
    }

    println!("--- cosmic-text VN spike ---");
    println!("font faces indexed : {font_faces}");
    println!("FontSystem::new    : {t_fonts:?}  (one-time, shared)");
    println!("[page]  lines={lines1} glyphs={glyphs1} raster={t_draw1:?} -> {page_png}");
    println!("[editor] glyphs={glyphs2} -> {editor_png}");
    println!("[editor] model text: {roundtrip}");
    println!(
        "[doc]   pages={} lines={} page_px={:?} -> {out_dir}/spike_docpage_*.png",
        layout.page_count(),
        layout.line_count(),
        layout.page_px()
    );
}
