# Kế hoạch: "Document mode" — trình soạn thảo văn bản nhẹ trong iAi

> Trạng thái (2026-09-03): **MVP + Trộn thư + nhiều bản vá editor + REDESIGN ẢNH
> kiểu Word — TẤT CẢ ĐÃ PUSH** (nhánh `feat/vector-core-foundation`, tới `3be6eb4`).
> **Việc kế tiếp cho hội thoại MỚI: BAO CHỮ (Square wrap)** — xem mục 0.4 bên dưới.
> Chủ dự án (end-user) muốn trình soạn thảo kiểu Word cơ bản, nhấn mạnh **nhẹ máy**.

---

## 0. TÌNH TRẠNG HIỆN TẠI (2026-09-02) — đọc trước

**MVP đã xong, chủ GUI-test OK, đã push** lên `github tongnghia2026/iai` nhánh
`feat/vector-core-foundation` (tới commit `5f73c46`). Trình soạn thảo là **tài
liệu hạng nhất** (`DocumentKind::FlowText`, tab riêng; menu **Soạn thảo văn bản →
Tài liệu văn bản mới…**).

**ĐÃ LÀM (Pha 0–3 + xuất/nhập + hơn thế):**
- Gõ tiếng Việt WYSIWYG trên trang A4, đa trang, preview **nét device-resolution**.
- Định dạng ký tự: **đậm / nghiêng / gạch chân / màu chữ** (theo vùng chọn,
  Ctrl+B/I/U + toolbar). Căn lề đoạn (trái/giữa/phải/đều). **Giãn dòng** cả tài liệu.
- **Chèn ảnh** khối (logo/chữ ký/con dấu) — placeholder U+FFFC + overlay egui.
- **Danh sách chấm/số** hanging-indent (mẹo `BufferLine::layout` ép width từng dòng;
  list-kind lưu ở **AttrsList defaults metadata** để sống qua edit).
- **Lưu/mở `.iai`** (serde, format v9), **xuất PDF chữ-vector chọn-được** (nhúng
  font Type0/Identity-H; ảnh = XObject; underline/màu/list giữ đúng).
- Đã fix ext-bridge (SO_REUSEADDR) — xem memory `project_iai_ai_web_bridge`.

**Bản đồ code:** model `src/core/text_document.rs`; layout+PDF `src/core/text_layout.rs`
(chỗ DUY NHẤT gặp cosmic-text ở đường xuất, dùng per-para buffer); editor in-app
`src/ui/document_mode.rs` (thread-local, 1 cosmic-text `Editor`/`Buffer`, đồng bộ
2 chiều với model qua `FlowTextViewModel`/UiActions). Chi tiết + gotcha đầy đủ ở
memory `project_iai_wordprocessor_plan`.

### 0.1 Trộn thư (mail-merge), Pha 6 ✅ ĐÃ PUSH (chủ GUI-test OK)
- Nút **"Trộn thư"** → chọn **Excel (.xlsx/.xls/.ods) hoặc CSV** → hộp thoại soát
  trường + mẫu tên tệp → **xuất hàng loạt: mỗi dòng = một PDF chữ-vector** (chạy
  nền, thanh tiến độ, tên trùng tự thêm (2),(3)…). Chỗ giữ chỗ `{{Tên cột}}`;
  trường thiếu cột → giữ nguyên `{{…}}`; ô trống → rỗng; giữ định dạng từng run.
- Lõi `src/core/mail_merge.rs` (fixture .xlsx + e2e merge→PDF). App-side
  `src/app/file_ops/mail_merge.rs`. Deps: `csv`, `calamine`(dates), `chrono`.

### 0.2 Bản vá editor sau góp ý chủ ✅ ĐÃ PUSH
- Đổi màu chữ **không còn mất vùng chọn** (chặn cú click đóng popup rơi vào trang
  qua `ctx.any_popup_open()`).
- **Giãn dòng theo VÙNG CHỌN** (per-đoạn): multiplier×1000 gói vào metadata bit
  cao (list ở byte thấp), `refresh_line_spacing` đặt `metrics_opt` per-dòng.
- Ảnh inline: hàng công cụ (căn/cỡ/lên-xuống/xoá) + **kéo ảnh trực tiếp**.

### 0.3 Redesign ẢNH kiểu Word ✅ ĐÃ PUSH (chủ chọn mức ĐẦY ĐỦ, kể cả bao chữ)
- **Bảng Layer** khi ở FlowText → liệt kê đối tượng: "Văn bản" + mỗi ảnh (inline
  & nổi) 1 dòng, bấm để focus (`flow_text_objects_panel` trong `panels.rs`;
  `DocumentIntent.flow_text_focus` → `document_mode::request_focus`). Cửa sổ "Bố
  cục" cũ đã bỏ.
- **3 chế độ vị trí ảnh** (combo "Kiểu ảnh"): **Cùng dòng / Nổi trên chữ / Nổi sau
  chữ**. Model: `ImageBlock.wrap: ImageWrap{Inline,BehindText,InFrontOfText,
  Square,TopBottom}` + `page,x_mm,y_mm` (serde default); `TextDocument.
  floating_images: Vec<ImageBlock>`. Editor: `d.floating`/`selected_floating`/
  `float_drag`; `handle_floating_pointer` (chọn/kéo-move/kéo-góc-resize, chạy
  TRƯỚC text). Ảnh sau chữ: render_page nền TRONG SUỐT (`blend_px` ghi alpha) +
  vẽ giấy trắng riêng + thứ tự vẽ (paper→behind→chữ→inline→front). PDF:
  `ImgPlace.behind`, vẽ behind trước BT/in-front sau ET.

### 0.4 ➜ VIỆC KẾ TIẾP: BAO CHỮ (Square wrap) — chưa làm
Chủ đã chốt muốn **chữ chạy vòng quanh ảnh** (Square/tight của Word). Đây là phần
NẶNG & rủi ro nhất; làm ở hội thoại mới, **chủ test bản trung gian**.
- **Cách:** `ImageWrap::Square` áp exclusion-rect. Mẹo per-line như list: lấy
  hàm thuần `square_wrap(floating, page, cx, cy, cw, ly, line_h) -> Option<(x_off,
  width)>` (ảnh bên trái→đẩy text sang phải+hẹp; bên phải→hẹp; giữa/hẹp quá→None).
  Gộp với list qua `line_layout(is_list, cw, wrap) -> (offset,width)`.
- **Điểm cắm (mỗi chỗ đang xử lý list_indent, thêm wrap_offset song song):**
  (1) relayout mới `relayout_wrap_lines` (2-pass reflow, GATE no-op khi ko có ảnh
  Square) — chạy cạnh `relayout_list_lines`; (2) `render_page` thêm param
  `floating` + offset per-dòng; (3) selection rect trong `window_ui`; (4)
  `list_indent_at_y` (hit-test click); (5) caret; (6) `text_layout` (PDF flow
  per-para — khác editor, khó hơn, làm sau cùng).
- **GATE bắt buộc:** khi tài liệu KHÔNG có ảnh `Square`, mọi hàm trả về như cũ →
  0 thay đổi cho tài liệu hiện có (rào rủi ro). `TopBottom` có thể bỏ/để sau.

### 0.5 CÒN LẠI khác (tùy chọn, KHÔNG gấp)
1. **Đầu/chân trang + số trang.** 2. **Bảng (tables)** — Pha 4. 3. **Xuất `.docx`**
   (`docx-rs` + `zip`). 4. per-run font/cỡ; subset font nhúng PDF; đo RAM/CPU.
5. Mail-merge nâng cao: gộp 1 PDF; lọc/chọn dòng.

**ĐÃ CHỐT BỎ:** AutoPrint khỏi kế hoạch này; split-view; egui_kittest (chủ tự test).

**Quy ước:** commit local, push khi chủ bảo; `cargo fmt --all --check` +
`cargo test --lib` trước push; comment tiếng Anh tối thiểu, đừng đụng chuỗi UI VN.

---

## 1. Mục tiêu & bối cảnh

Tích hợp một trình soạn thảo văn bản kiểu **Word cơ bản** vào iAi, tận dụng
engine sẵn có (text, font, đa-trang/artboard, xuất PDF, hệ in). Ứng dụng thực tế
của chủ: soạn **hợp đồng theo yêu cầu từng khách** + nghề **in ấn** (đã có hệ
AutoPrint Epson). Ràng buộc số 1: **không được nặng máy** — một tài liệu chữ
thuần phải nhẹ gần như editor thường, không được ngốn RAM như một canvas ảnh.

## 2. Nguyên tắc kiến trúc (chốt)

**Trang chữ đi qua đường layout-chữ nhẹ — KHÔNG đụng tile compositor / master
16-bit / tile-atlas. Raster chỉ được cấp phát khi trang thật sự chứa ảnh** (tái
dùng cơ chế trang-lười-biếng `Vec<Option<Canvas>>` đã có).

Hệ quả hiệu năng mong đợi: hợp đồng 50 trang ≈ vài chục MB trên nền ~200MB của
app, idle 0% CPU (đã fix bug spin ở commit `a495981`). Cỡ Word, không phải cỡ
photo-editor.

## 3. Hiện trạng code (đã điều tra — điểm neo, khỏi dò lại)

- **Text hiện tại = "chữ đồ hoạ", nướng thành pixel.**
  - `src/core/text.rs`: `TextData` (chuỗi + style per-glyph) → rasterize vào tiles
    của layer khi commit. `layout_text()` → `TextLayout { lines }` chỉ layout **một
    hộp**, xuống dòng thủ công (không word-wrap, không flow). Dùng `ab_glyph`
    (KHÔNG shaping) + tự hack NFC cho dấu tiếng Việt (`normalize_nfc_with_style_src`).
  - `src/app/text_ops.rs`, `src/tools/text_tool.rs`: đường soạn/commit + input/IME.
  - → Đây là "text box đồ hoạ", KHÔNG phải "đoạn văn chảy". Giữ nguyên cho
    chữ-poster; document mode dùng model MỚI.
- **Đa-trang đã "lười biếng" sẵn** (mẫu tái dùng chính):
  - `src/core/document.rs`: `Document { canvas: Canvas, pages: Vec<Option<Canvas>>,
    pdf_document, edited_pages: HashMap<usize, PdfCachedPage>, ... }`. Trang PDF chỉ
    **materialize Canvas khi bị sửa** (`Option<Canvas>` = `None` tới khi cần).
- **Contract trang/artboard đã dành sẵn (foundation #10):**
  - `src/core/page.rs`: `Page { id: PageId, origin, size, bleed, margin, background }`
    + `PageId`. Mọi `Layer` mang `page_id`; toạ độ đã **page-relative abstract f32**
    → thêm trang là **additive**, không dời object. Xem `docs/ADR_PAGE_OWNERSHIP.md`
    (giữ chỗ cho "pages list trên document + envelope `.iai`").
  - `Canvas.artboards: Vec<page::Page>` (`src/core/canvas/mod.rs`).
- **Compositor raster** (`src/gpu/compositor.rs`): tile-based (`TileMap` thưa,
  `TileAtlas`) + LOD proxy (cap 256MB, LRU). Cấp phát theo nội dung; có baseline
  GPU/device cố định. → Document mode **né** đường này cho trang chữ.
- **Xuất/in đa-trang đã có**: `src/core/print.rs` (`MultiPageInput`), xuất PDF,
  `src/formats/pdf.rs`, `src/app/file_ops/save_export.rs`. `ExportFormat` enum:
  Png/Jpeg/Webp/Tiff/Bmp/Iai/Pdf; SVG xuất qua đường riêng (`src/core/svg.rs`).
- **Font**: `src/core/text.rs` có index font hệ thống + xử lý tiếng Việt + nạp
  font on-demand — **tái dùng** cho document mode.

## 4. Quyết định kỹ thuật then chốt: engine layout chữ

KHÔNG tự viết xuống-dòng/shaping/bidi. Chọn 1 trong 2:

- **`cosmic-text` ⭐ (khuyên cho MVP).** Kèm sẵn **buffer soạn thảo** (`Editor`:
  con trỏ, bôi chọn, chèn/xoá, word-wrap, hit-test) + shaping thật (swash) xử lý
  dấu tiếng Việt natively. → làm sẵn ~50% "Word cơ bản".
- `parley` (Linebender, chung hệ Lyon/vello). Thấp hơn, typography kiểm soát kỹ
  hơn nhưng phải tự viết editor. Để dành nếu sau cần typography cao cấp.

Text `ab_glyph` + hack NFC hiện tại sẽ được **thay bằng shaping thật** ở đường
document (sạch hơn, đúng tiếng Việt hơn).

## 5. Các pha

### Pha 0 — Spike khử rủi ro (1–2 ngày) — LÀM TRƯỚC
Dựng `cosmic-text` render 1 buffer nhiều đoạn, wrap theo bề rộng; **kiểm dấu
tiếng Việt** (Ể, ỡ, ữ, ệ…) và **IME Telex**; vẽ glyph qua egui painter hoặc một
pipeline wgpu nhỏ — **không** qua compositor. Chốt cửa: (1) VN hiển thị đúng,
(2) IME gõ được, (3) đường vẽ nhẹ chạy được + đo RAM/CPU thực tế của trang chữ.

### Pha 1 — Mô hình tài liệu
- `TextDocument` = đoạn → run (style ký tự) + style đoạn (canh lề, giãn dòng,
  thụt đầu, list). **Tách** khỏi `TextData` đồ hoạ.
- Loại trang MỚI "văn bản chảy" trên slot multi-page dành sẵn: khổ giấy
  (A4/Letter) + lề — tái dùng `page::Page{origin,size,bleed,margin,background}`.
- Tái dùng `Document.pages: Vec<Option<Canvas>>`: Canvas của trang = `None` tới
  khi có raster.

### Pha 2 — Soạn thảo (cảm giác "Word")
Con trỏ/bôi chọn/gõ/IME/copy-paste (cosmic-text lo phần lớn; nối vào đường
input+IME đã có ở text tool — coi chừng đá nhau). Định dạng ký tự (đậm/nghiêng/
gạch, font, cỡ, màu) + đoạn (canh lề, giãn dòng, bullet/số).

### Pha 3 — Chảy chữ qua nhiều trang
Auto-phân trang: chữ tràn chiều cao vùng-văn-bản → sang trang kế (tạo trang khi
cần). Header/footer + số trang.

### Pha 4 — Bảng (tables)
Hàng/cột, viền ô, chữ trong ô (cỡ vừa).

### Pha 5 — Xuất/nhập (tái dùng sẵn)
- **PDF**: dùng lại hệ xuất PDF/in đa-trang; nhúng **text thật (chọn được)**,
  đừng nướng raster.
- **`.iai`**: mở rộng envelope (slot additive dành sẵn).
- **`.docx`** (sau): `docx-rs` + `zip` (đã có `zip`).

### Pha 6 — Trộn thư (mail-merge)
Trường biến + nguồn dữ liệu (CSV/Excel) → xuất **hàng loạt** hợp đồng/PDF. Đúng
nghề "hợp đồng theo yêu cầu khách" của chủ. Dùng lại doc model + xuất PDF.

## 6. MVP tối thiểu (chốt phạm vi, tránh phình)
**Pha 0–3 + xuất PDF/.iai:** chữ chảy đa trang A4, định dạng ký tự/đoạn, list,
con trỏ/bôi chọn/IME, chèn ảnh inline (raster lười biếng). **Hoãn:** bảng, docx,
mail-merge, header/footer.

## 7. Hiệu năng (ràng buộc số 1 của chủ)
- Trang chữ: KHÔNG tile-atlas, KHÔNG master 16-bit, KHÔNG compositor → RAM ≈
  text + atlas font.
- Chèn ảnh mới cấp Canvas cho đúng trang đó (`Vec<Option<Canvas>>`).
- Idle 0% (đã fix `a495981`); gõ chữ → 1 frame → ngủ; cache glyph để re-layout rẻ.
- GPU device/cửa sổ là chi phí **một lần dùng chung** — gộp text + ảnh KHÔNG nhân
  đôi tải GPU; chỉ cần đừng cấp raster cho trang chữ.

## 8. Rủi ro cần canh
1. Dấu tiếng Việt + IME Telex trong cosmic-text — **kiểm ở Pha 0** trước khi cam kết.
2. Text **chọn được** trong PDF xuất ra (nhúng font/text, đừng raster hoá).
3. Nối `Editor` cosmic-text với đường input/IME egui hiện có mà không xung đột
   (đã có logic IME/keyboard phức tạp cho text tool — xem `src/app/input/mod.rs`,
   `src/app/text_ops.rs`).
4. Scope creep — giữ MVP chặt.

## 9. Bắt đầu từ đâu (cho hội thoại mới)
1. Đọc lại file này + `docs/ADR_PAGE_OWNERSHIP.md` + `src/core/page.rs`,
   `src/core/document.rs`, `src/core/text.rs` (mục 3 ở trên là bản đồ).
2. Làm **Pha 0 (spike)**: thêm `cosmic-text` vào `Cargo.toml`, dựng một demo/bin
   thử render + IME tiếng Việt, đo RAM/CPU. Báo cáo số đo trước khi vào Pha 1.
3. Quy ước làm việc: commit local, `cargo fmt --all --check` + `cargo test --lib`
   trước push, **push khi chủ bảo**. Comment tiếng Anh, tối thiểu; đừng đụng chuỗi
   UI tiếng Việt.
