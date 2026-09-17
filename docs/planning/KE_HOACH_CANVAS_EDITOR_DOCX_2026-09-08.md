# Kế hoạch chuẩn: Canvas Editor + `docx-rs` cho Document mode của iAi

> **Tài liệu chuẩn để theo dõi triển khai.** Mọi phiên làm việc liên quan trình
> soạn thảo phải đọc file này trước khi sửa code và cập nhật checklist/changelog
> trước khi kết thúc phiên.

## 0. Trạng thái

- Ngày chốt kiến trúc: **2026-09-08**.
- Nhánh hiện tại: `feat/vector-core-foundation`.
- Trạng thái tổng thể: **Pha 1 đã đạt; Pha 2 đã đạt cổng tạo → gõ → Save → đóng
  → mở lại. Pha 3: backing Legacy/CanvasEditor, reader/writer `.iai` v12 và
  Save/Save As/reopen đã qua GUI-test; converter Legacy → Canvas Editor còn chờ
  đối chiếu file thực tế. Pha 3.5: toolbar định dạng cơ bản đã nghiệm thu; bảng
  màu và nhóm Insert đã code, chờ GUI-test. Kiểm toán ngày 2026-09-15 phát hiện
  21 lỗi an toàn dữ liệu/hồi quy (Pha 3.6). Toàn bộ worktree Canvas Editor vẫn
  CHƯA COMMIT**.
- Việc kế tiếp duy nhất: **Pha 3.6 — khóa an toàn dữ liệu và hồi quy, theo thứ
  tự chủ dự án chốt sau báo cáo 2026-09-15**; chỉ sau đó mới GUI-test bảng màu/
  Insert và làm nhóm Table. Cổng chống hồi quy tab ảnh (W/H/DPI Crop và Ctrl+W)
  đã đạt ngày 2026-09-11 nhưng lỗi B6 (nhập W/H ở chế độ Free/Ratio) mới phát hiện.
- Kế hoạch cũ `KE_HOACH_TRINH_SOAN_VAN_BAN_2026-09-01.md` chỉ còn giá trị lịch sử.
- Không xóa Document mode `cosmic-text` trước khi Pha 6 đạt đủ điều kiện.
- Không push nếu chủ dự án chưa yêu cầu.

Quy ước trạng thái:

- `[ ]` chưa làm.
- `[~]` đang làm hoặc đã code nhưng chưa đạt cổng nghiệm thu.
- `[x]` hoàn thành và đã qua cổng nghiệm thu.
- `[!]` bị chặn; phải ghi nguyên nhân và quyết định tiếp theo vào Changelog.

## 1. Quyết định đã khóa

1. **Canvas Editor 1.0.x, MIT** là giao diện soạn thảo và engine bố cục trang.
2. Canvas Editor chạy **hoàn toàn offline** trong WebView2 con của cửa sổ iAi;
   không tải JS, CSS, font hoặc telemetry từ CDN.
3. Dùng **`wry`** để đặt WebView con vào vùng Document mode của cửa sổ
   `winit`; giai đoạn đầu chỉ cam kết Windows.
4. **`docx-rs` 0.4.x, MIT** là thư viện Rust đọc/ghi DOCX. Không tự viết ZIP,
   OPC và WordprocessingML từ đầu.
5. **Không sao chép, dịch hoặc chuyển thể source ONLYOFFICE (AGPL).** Chỉ được
   dùng ONLYOFFICE như chương trình đối chiếu đầu ra/hiển thị kiểu black-box.
   Nguồn triển khai là ECMA-376, tài liệu OOXML và thư viện permissive.
6. Dữ liệu chuẩn khi đang làm việc là **Canvas Editor JSON nằm trong `.iai`**.
   DOCX chỉ là định dạng Import/Export, không phải định dạng autosave/canonical.
7. Giữ `DocumentKind::FlowText` để tránh lan thay đổi sang tab/menu/app shell.
   Backing state mới phải phân biệt rõ Legacy và Canvas Editor payload.
8. Loader phải mở được `.iai` FlowText v11 trở xuống và chuyển sang payload mới
   một lần. Không ghi đè file cũ cho tới khi người dùng Save/Save As.
9. Engine cũ tiếp tục là fallback sau feature flag trong thời gian chuyển đổi.
   Chỉ xóa sau khi migration, PDF, DOCX và GUI smoke test đều đạt.
10. Không thay đổi đường ảnh/vector/PDF-project của iAi trong dự án này.

## 2. Mục tiêu và phạm vi

### 2.1 Mục tiêu

- Có trải nghiệm soạn thảo kiểu Word theo trang mà iAi không phải tự duy trì
  caret, IME, selection, phân trang, bảng và ảnh bao chữ.
- Gõ tiếng Việt Telex ổn định trên Windows 10/11 và màn hình nhiều mức DPI.
- Lưu/mở `.iai` offline, khôi phục đúng nội dung và dirty state.
- Import/Export DOCX đủ tốt cho hợp đồng và văn bản hành chính thông thường.
- Giữ PDF chữ chọn được nếu đường xuất PDF mới có thể ánh xạ đủ dữ liệu.
- Giảm test GUI tự viết; tập trung test bridge và converter thuần dữ liệu.

### 2.2 Không thuộc phạm vi phiên bản đầu

- Tương thích 100% mọi tính năng Microsoft Word.
- Macro/VBA, ActiveX, SmartArt, chart nhúng và OLE.
- Equation Office Math phức tạp.
- Đồng biên tập thời gian thực/CRDT.
- Linux/macOS trước khi bản Windows ổn định.
- White-label, source hoặc server của ONLYOFFICE.

## 3. Kiến trúc đích

```text
┌──────────────────────────── iAi/Rust ────────────────────────────┐
│ winit + wgpu + egui                                              │
│                                                                  │
│  App/Document state ◄── JSON IPC ──► Document WebView (wry)     │
│          │                              │                        │
│          │                              └─ Canvas Editor 1.0.x   │
│          │                                                       │
│          ├─ .iai v12: Canvas Editor JSON                         │
│          ├─ DOCX adapter: docx-rs                                │
│          └─ PDF adapter: quyết định tại cổng Pha 4                │
└──────────────────────────────────────────────────────────────────┘
```

Điểm neo code hiện tại:

- Dependencies: `Cargo.toml` (`winit`, `wgpu`, `egui`, `cosmic-text`).
- OS window/runtime: `src/app/window_runtime.rs`.
- Event loop/focus/IME/DPI: `src/app/input/mod.rs`.
- Vùng editor hiện tại: `src/ui/mod.rs` → `src/ui/document_mode.rs`.
- Canonical legacy model: `src/core/text_document.rs`.
- Document state: `src/core/document.rs`.
- `.iai` FlowText load/save: `src/formats/iai.rs`.
- App open/save/export: `src/app/file_ops/open.rs` và
  `src/app/file_ops/save_export.rs`.
- PDF legacy: `src/core/text_layout.rs` và `src/formats/pdf.rs`.

### 3.1 Payload `.iai` mới

Dự kiến bump `IAI_FORMAT_VERSION` từ 11 lên 12 khi bắt đầu ghi payload mới:

```json
{
  "version": 12,
  "kind": "flow_text_document",
  "editor": "canvas-editor",
  "editor_schema_version": 1,
  "document": {
    "header": [],
    "main": [],
    "footer": []
  }
}
```

Rust giữ envelope typed; trường `document` ban đầu có thể là
`serde_json::Value` để không sao chép toàn bộ schema TypeScript. Trước khi nhận
payload phải kiểm giới hạn kích thước, version và các trường gốc bắt buộc.

### 3.2 Hợp đồng IPC tối thiểu

Mọi message có `protocol_version`, `request_id` và `type`.

Rust → WebView:

- `load_document { document }`
- `request_snapshot`
- `set_theme { theme }`
- `set_read_only { value }`
- `focus_editor`

WebView → Rust:

- `ready { editor_version, protocol_version }`
- `document_changed { revision }`
- `snapshot { request_id, revision, document }`
- `save_requested`
- `error { code, message }`

Không gửi toàn bộ document ở mỗi phím. WebView chỉ phát revision/dirty event;
Rust yêu cầu snapshot khi Save, autosave, đóng tab hoặc export.

## 4. Các pha và checklist

### Pha 0 — Chốt và bảo toàn baseline `[x]`

- [x] Chọn Canvas Editor làm UI/engine bố cục.
- [x] Chọn `docx-rs` làm reader/writer DOCX.
- [x] Chốt không dùng source ONLYOFFICE trong mã MIT.
- [x] Giữ engine legacy làm fallback.
- [x] Tạo kế hoạch chuẩn này và đánh dấu kế hoạch cũ là lịch sử.
- [x] Ghi lại kết quả `cargo test --lib` baseline ngay trước khi code Pha 1.

Cổng P0: kế hoạch có quyết định, phạm vi, cổng nghiệm thu và rollback rõ ràng.

### Pha 1 — Spike WebView2 + Canvas Editor offline `[x]`

Mục tiêu: chứng minh editor chạy được trong đúng cửa sổ iAi trước khi đụng model
hoặc DOCX.

- [x] Pin một phiên bản Canvas Editor 1.0.x cụ thể; lưu version và hash.
- [x] Thêm `wry` dưới target Windows; không đổi event-loop sang Tauri/tao.
- [x] Tạo web workspace nhỏ, dự kiến `web/document-editor/`.
- [x] Bundle JS/CSS/font thành asset offline; không dùng URL/CDN khi runtime.
- [x] Tạo `src/app/document_webview.rs` quản lý create/show/hide/resize/destroy.
- [x] Đặt child WebView đúng central viewport của FlowText.
- [x] Căn giữa khung giấy trong vùng WebView; chủ dự án đã xác nhận trên GUI.
- [x] Khi chuyển tab/cửa sổ/minimize/modal, WebView ẩn/hiện đúng và không đè
  popup `egui`.
- [x] IPC `ready` và `ping/pong` hoạt động.
- [x] Gõ thử bộ tiếng Việt: `Tiếng Việt — Nguyễn Thị Thu — Ắ Ề Ễ Ự`.
- [x] Kiểm Ctrl+C/V/Z/Y, selection, chuột, wheel, Tab và Alt menu.
- [x] Kiểm resize và 100/125/150/200% DPI.
- [x] Đóng/mở tab 20 lần không crash, không còn process/WebView rác.
- [x] Feature flag cho phép quay lại editor legacy.

Cổng P1:

- Editor chạy offline, không có request mạng.
- IME tiếng Việt và focus đạt trên Windows.
- Resize/DPI đúng; không crash khi chuyển tab/đóng tài liệu.
- Nếu P1 thất bại, dừng hướng WebView và không sửa model hiện tại.

Ước lượng: 1–2 ngày.

### Pha 2 — Bridge và lifecycle tài liệu `[~]`

- [x] Định nghĩa IPC envelope/version ở Rust và TypeScript.
- [x] `load_document` và `request_snapshot` round-trip không đổi JSON; unit test
  và runtime Release đã được chủ dự án xác nhận.
- [x] Dirty revision chỉ đổi khi nội dung đổi, không đổi khi zoom/selection;
  runtime đã cache document/revision/dirty độc lập theo `DocumentId`. GUI-test
  đầu tiên phát hiện race trước khi event dirty được host xử lý; GUI-test kế tiếp
  phát hiện callback Switch tái nhập snapshot gate; lần sau xác nhận bridge còn
  ở trạng thái `not ready`. Các nhánh đã sửa; chủ dự án đã xác nhận tạo/chuyển
  tab, nội dung độc lập và revision không đổi khi chỉ zoom/selection đạt.
- [x] Ctrl+S và menu Save gọi snapshot có revision/timeout, đưa payload đã
  validation vào core rồi ghi `.iai` v12 atomically; code/test tự động và
  GUI-test Save/Save As/reopen đã đạt ngày 2026-09-11.
- [x] Đóng tab/app khi dirty phải snapshot xong trước dialog quyết định; sau khi
  bổ sung deferred Exit và ẩn child WebView khi hiện close modal, chủ dự án đã
  xác nhận Close/Cancel và đóng app hoạt động đúng trên bản Release ngày
  2026-09-09.
- [x] Autosave FlowText dùng snapshot có timeout, cache theo `DocumentId` và
  không block UI thread; code/test tự động và GUI-test chu kỳ 90 giây đã đạt.
  Ghi recovery xuống đĩa cố ý chờ writer `.iai` v12 ở Pha 3.
- [~] Lỗi JS/WebView và snapshot timeout hiện trong status và dialog modal;
  request snapshot cùng ý định Switch/Close/Exit đang chờ bị hủy an toàn, không
  đóng tài liệu hoặc ghi đè file. Code/test tự động đã đạt, còn chờ GUI-test hồi
  quy lifecycle trên bản Release mới.
- [x] Theme và read-only state đồng bộ từ iAi; code/test tự động và GUI-test
  toolbar đã đạt ngày 2026-09-11.
- [x] Không log nội dung văn bản nhạy cảm trong release build; Rust chỉ log
  trạng thái/mã lỗi, không log payload snapshot.

Cổng P2: tạo → gõ → Save → đóng → mở lại trong cùng phiên giữ nguyên nội dung;
crash/timeout bridge không làm ghi file rỗng.

Ước lượng: 1–2 ngày.

### Pha 3 — `.iai` v12 và migration editor cũ `[~]`

- [x] Thêm backing state phân biệt `Legacy(TextDocument)` và
  `CanvasEditor(CanvasEditorDocument)`.
- [x] Bump format v12 chỉ khi writer payload mới đã sẵn sàng; writer legacy tiếp
  tục đóng dấu v11.
- [x] Viết reader/writer `.iai` v12 và validation/size limits: manifest tối đa 1
  MiB, payload tối đa 8 MiB, giới hạn depth/node; ghi đè dùng atomic replace.
- [~] Viết converter một chiều Legacy → Canvas Editor JSON; code/test dữ liệu đã
  đạt, còn chờ GUI-test với file legacy thực tế.
- [~] Đã map chữ/run, paragraph, list, line spacing, page setup và ảnh; paragraph
  spacing/indent được giữ trong metadata nhưng Canvas Editor chưa render.
- [~] Đã map các kiểu ảnh legacy và sinh warning cho encoding/thuộc tính chưa thể
  biểu diễn; còn chờ đối chiếu hiển thị trên fixture thực tế.
- [~] V11 trở xuống mở bằng loader cũ; atomic writer v12 không phá file nguồn
  khi ghi lỗi. Save/Save As thực tế đã nối snapshot WebView vào core, chỉ đánh
  dấu saved sau khi writer thành công và đã qua GUI-test Save/reopen; còn chờ ca
  lỗi ghi file và đối chiếu file legacy thực tế.
- [x] V12 round-trip giữ nguyên unknown fields trong Canvas payload.
- [x] Test v10/v11 và v12 tạo trong test đã đạt; GUI mở file v12 Save/reopen đã
  được chủ dự án nghiệm thu ngày 2026-09-11.
- [ ] Autosave/recovery nhận biết schema version.

Cổng P3:

- File legacy mẫu mở không mất chữ/ảnh/style thuộc phạm vi.
- File v12 save/reopen cho JSON tương đương về ngữ nghĩa.
- Build cũ phải từ chối v12 thay vì mở rồi làm mất nội dung.

Ước lượng: 2–4 ngày.

### Pha 3.5 — Giao diện tính năng soạn thảo `[~]`

Mục tiêu: đưa các command Canvas Editor cần dùng hằng ngày ra giao diện trực
quan trong WebView trước khi phát triển PDF/DOCX; menu File vẫn do app shell iAi
quản lý.

- [x] Toolbar cơ bản: Undo/Redo, font, cỡ chữ theo point, đậm/nghiêng/gạch chân,
  màu chữ, căn trái/giữa/phải/đều, bullet/numbering, giãn dòng và zoom. Code,
  typecheck/lint/unit test/bundle và GUI-test Release đã đạt ngày 2026-09-11.
- [x] Đồng bộ trạng thái nút theo selection/caret bằng `rangeStyleChange`; toolbar
  bị khóa theo read-only nhưng zoom vẫn dùng được.
- [~] Nhóm Insert: ảnh, bảng, hyperlink, đường phân cách và ngắt trang. Code,
  validation, test web và bundle đã đạt; còn chờ GUI-test Release và Save/reopen.
- [ ] Nhóm Table theo ngữ cảnh: thêm/xóa hàng cột, merge/split cell, border và
  căn dọc.
- [ ] Thiết lập trang: A4/Letter, dọc/ngang, lề trang và cột.
- [ ] Tìm kiếm/thay thế, format painter, superscript/subscript, strikeout và
  highlight.
- [ ] Tooltip/phím tắt/keyboard navigation đầy đủ; kiểm layout toolbar ở cửa sổ
  hẹp và DPI 100/125/150/200%.
- [x] Cổng chống hồi quy app shell: sau khi rời tab văn bản, WebView phải trả
  focus cho cửa sổ chính; nhập W/H/DPI Crop và Ctrl+W trên tab ảnh phải hoạt
  động. Code/test tự động và GUI-test Release của chủ dự án đều đã đạt.
- [ ] Chốt thiết kế menu/toolbar cuối cùng theo GUI-test của chủ dự án.

Cổng P3.5: người dùng có thể định dạng một văn bản hành chính cơ bản mà không
cần gọi command ẩn; trạng thái toolbar đúng theo selection, read-only và theme;
không làm hồi quy focus/IME/Ctrl+W/Save.

Ước lượng: 2–4 ngày.

### Pha 3.6 — Khóa an toàn dữ liệu và hồi quy sau kiểm toán 2026-09-15 `[ ]`

Nguồn: rà soát đa tác tử có kiểm chứng phản biện trên worktree chưa commit; bằng
chứng file:dòng và kịch bản tái hiện nằm ở Changelog 2026-09-15. Không làm tính
năng mới cho tới khi nhóm A đạt. Khi sửa C2 phải sửa A1 cùng lúc (C2 đang che
một phần đường tái hiện của A1).

Nhóm A — mất dữ liệu hoặc kẹt ứng dụng:

- [ ] A1. Revision lõi lùi được: WebView tạo lại seed revision 0
  (`ensure_document`, `BridgeDocumentState::from_document`) và
  `replace_canvas_editor_document` nhận revision thấp hơn → tab văn bản chưa lưu
  có thể thành "sạch", đóng tab/thoát app không hỏi. Giữ revision lõi đơn điệu;
  seed bridge từ revision/dirty của core; Close completion xét cả
  `Document::is_modified()`.
- [ ] A2. Mở file hoặc tạo tab mới đặt thẳng `active_doc_idx`
  (`activate_new_document`, `do_new_tab`, `pdf_session`, `impose`, `actions/ai`)
  không qua snapshot gate → mất phần gõ chưa snapshot (tối đa 90 giây); mở `.iai`
  khác nạp đè editor khi tab cũ chưa snapshot.
- [ ] A3. Tài liệu vượt 8 MiB (hai ảnh điện thoại ~3,5 MB hoặc dán một ảnh chụp
  màn hình lớn — đường dán không kiểm kích thước) → Save/Switch/Close/Exit đều lỗi,
  không thoát được; renderer WebView2 treo cũng kẹt tương tự. Cần ngân sách tổng
  khi chèn/dán ảnh (hoặc nén lại) và lối thoát khi snapshot lỗi lặp lại trên bridge
  đã verified (hủy WebView, dùng nội dung đã commit, hỏi lưu/bỏ như bình thường).
- [ ] A4. Giới hạn `manifest.json` 1 MiB (`read_manifest`) áp cả cho v10/v11, nơi
  ảnh FlowText legacy nằm base64 trong manifest → file văn bản cũ có ảnh lớn không
  mở được, kể cả file do chính bản này ghi. Bản `target/release` ngày 2026-09-12
  đã dính. Chỉ áp cap cho v12.
- [ ] A5. Save trong vòng hỏi-khi-thoát không ghi tab văn bản khi WebView chưa tồn
  tại (`request_document_webview_save_snapshot` nhánh `None` trả `true`) → chỉ
  còn Cancel hoặc bỏ thay đổi. Khi không có WebView phải ghi nội dung đã commit
  trong core.
- [ ] A6. Enter trong hộp "File đã thay đổi trên đĩa" chọn Reload, xóa chỉnh sửa
  chưa lưu và lịch sử undo. Khi `reload_will_discard_changes` Enter phải là Giữ.

Nhóm B — tính năng đã nghiệm thu bị hỏng với tài liệu Canvas Editor:

- [ ] B1. File → Export → PDF rơi xuống exporter raster artboard: ghi trang trắng
  1×1 px và báo "Đã xuất PDF", hoặc lỗi "Trang 2 không tồn tại". Tối thiểu phải từ
  chối rõ ràng; đường xuất thật thuộc Pha 4.
- [ ] B2. Không còn lối vào Trộn thư cho tài liệu Canvas (nút cũ nằm trong toolbar
  legacy không được vẽ; toolbar web không có lệnh).
- [ ] B3. File legacy tự chuyển sang Canvas Editor không hỏi: `line_spacing` chép
  vào `rowMargin` (Canvas Editor coi là khoảng cộng, không phải hệ số) nên giãn
  dòng khác; thụt đầu dòng/khoảng đoạn chỉ nằm trong metadata, không hiển thị; ảnh
  nổi giữ tọa độ tuyệt đối nên lệch chữ; Ctrl+S ghi đè thành v12 mà bản cũ không
  mở được. **Cần quyết định của chủ dự án về chính sách file cũ.**
- [ ] B4. File v12 mở trong build không bật feature hoặc máy thiếu WebView2 → vùng
  trống + báo "Đã mở văn bản"; lỗi migration còn bật cờ fallback toàn phiên làm các
  tab Canvas khác cũng trống. Phải hiện thông báo rõ, không tuyên bố fallback legacy.
- [ ] B5. Bảng Layer trống và thanh trang (số trang, ◀/▶) không tác dụng với tài
  liệu Canvas; lấy thông tin từ snapshot/IPC hoặc ẩn các bề mặt này.
- [ ] B6. Crop: nhập W/H khi đang Free/Ratio ép sang FixedSize với cạnh còn lại cũ
  (800/600 mặc định hoặc sai đơn vị) và gọi `init_bounds` → khung nhảy, kích thước
  xuất sai. Khi chuyển mode phải lấy cạnh còn lại từ vùng chọn hiện tại.

Nhóm C — trải nghiệm editor web:

- [ ] C1. Toolbar gọi `command.executeFocus()` không tham số sau mỗi lệnh → mất
  vùng chọn, caret nhảy về cuối tài liệu (cả zoom, Cancel màu/dialog, chèn ảnh).
- [ ] C2. Mở tài liệu là bị đánh dấu đã sửa: `contentChange` của `setValue` chạy
  trong `setTimeout(0)` sau khi `loadingDocument` đã tắt.
- [ ] C3. Toolbar và dialog thiếu thuộc tính `editor-component` → mousedown toàn
  cục reset range style: chọn cỡ/font/giãn dòng trùng giá trị hiển thị sai không
  có tác dụng; OK màu khi chưa chọn ô biến chữ màu thành đen.
- [ ] C4. Font Noto Sans nhúng chỉ là tập con tiếng Việt (115 code point, không có
  A–Z, số, dấu câu) → một từ trộn hai font, dàn trang phụ thuộc font máy.
- [ ] C5. Ô Link `type=url required` chặn `example.com` trước khi
  `normalizeHyperlinkUrl` tự thêm `https://`.

Nhóm D — nhỏ và vệ sinh repo:

- [ ] D1. Máy thiếu WebView2: hộp lỗi hiện lại mỗi lần quay về tab văn bản vì cờ
  lỗi bị xóa mỗi frame ở tab ảnh.
- [ ] D2. Gradient Editor (cửa sổ không modal) nuốt Esc/Enter của các hộp thoại
  vẽ sau nó (Close, Exit, Reload, PDF import, CMYK).
- [~] D3. Esc khi đang mở danh sách máy in đóng cả hộp Print; lần sau danh sách vẫn
  mở sẵn. Đã sửa 2026-09-15 cùng lỗi preview Print không cập nhật khi đổi máy in
  (xem Changelog); chờ GUI-test.
- [ ] D4. `.pnpm-store` chưa bị ignore và chứa junction tới `web/document-editor` →
  `git add -A` sẽ stage khoảng 19.400 file. Thêm `/.pnpm-store/` vào `.gitignore`
  trước khi commit.
- [ ] D5. (Khuyến nghị quy trình) CI chưa build/test feature
  `canvas-editor-webview` và web bundle; `dist/` được `include_bytes!` nên phải
  commit cùng source.

Cổng P3.6: nhóm A, B1, B4, B6, C1, C2 có test hồi quy tự động; full
`cargo test --lib` (có và không feature) và web test đạt; chủ dự án GUI-test lại
Save/Close/Exit nhiều tab, mở file văn bản cũ có ảnh, Export PDF, Crop W/H/DPI và
chọn chữ → định dạng.

### Pha 4 — PDF và in `[ ]`

- [ ] Spike hai đường: Canvas Editor print/PDF và adapter sang PDF text-vector
  hiện tại.
- [ ] Kiểm chữ trong PDF có select/search/copy được.
- [ ] Kiểm font tiếng Việt, ảnh, bảng qua trang, header/footer và số trang.
- [ ] Chọn một đường xuất chính; ghi quyết định và lý do vào Changelog.
- [ ] Không raster toàn trang nếu mục tiêu PDF chữ-selectable chưa được chủ dự
  án chấp nhận thay đổi.
- [ ] Print preview và giấy A4 đúng lề ở 100% scale.

Cổng P4: PDF/in đạt fixture chuẩn và không hồi quy đường PDF ảnh/vector khác.

Ước lượng: 2–5 ngày tùy đường được chọn.

### Pha 5A — Export DOCX bằng `docx-rs` `[ ]`

- [ ] Pin `docx-rs` 0.4.x cụ thể và kiểm toàn bộ license dependencies.
- [ ] Tạo module độc lập, dự kiến `src/formats/docx/`.
- [ ] Canvas JSON → `docx-rs` cho text/run/style.
- [ ] Paragraph alignment, indent, spacing và page break.
- [ ] Bullet/numbering nhiều cấp cơ bản.
- [ ] Table, border, width, colspan/rowspan thuộc phạm vi.
- [ ] Inline/floating image thuộc phạm vi.
- [ ] Page size/margin/section/header/footer/page number.
- [ ] Hyperlink và bookmark cơ bản.
- [ ] Export luôn dùng temp file + atomic replace; lỗi không phá file đích.
- [ ] Mở các file xuất bằng Word, LibreOffice và ONLYOFFICE để đối chiếu.

Cổng P5A: bộ fixture văn bản hành chính xuất ra mở không báo repair và giữ đúng
nội dung, trang, bảng, ảnh thuộc compatibility matrix.

Ước lượng: 3–6 ngày.

### Pha 5B — Import DOCX bằng `docx-rs` `[ ]`

- [ ] `docx-rs` AST → Canvas Editor JSON cho cùng subset của P5A.
- [ ] Giải style inheritance/theme color cần thiết cho tài liệu thông thường.
- [ ] Trích ảnh nhúng an toàn; chặn external relationship mặc định.
- [ ] Giới hạn tổng dung lượng giải nén, số part, kích thước ảnh và XML depth.
- [ ] Bỏ qua macro/ActiveX/OLE, ghi warning rõ cho người dùng.
- [ ] Liệt kê các feature không hỗ trợ sau import.
- [ ] Mặc định Save As `.iai`; không tự ghi đè DOCX nguồn.
- [ ] Export lại DOCX chỉ cam kết giữ subset đã công bố.
- [ ] Test DOCX từ Word, LibreOffice và ONLYOFFICE bằng black-box fixtures.

Cổng P5B: import không crash với file hỏng/không tin cậy; subset hỗ trợ giữ đúng
qua DOCX → Canvas JSON → DOCX.

Ước lượng: 5–10 ngày.

### Pha 6 — Ổn định và chuyển mặc định `[ ]`

- [ ] Chạy toàn bộ Rust tests, web unit tests và GUI smoke checklist.
- [ ] Đo thời gian mở app, mở editor, RAM idle, tài liệu 10/50/100 trang.
- [ ] Kiểm installer có WebView2 runtime hoặc hướng dẫn lỗi rõ ràng.
- [ ] Kiểm không có network request trong runtime offline.
- [ ] Cập nhật `THIRD_PARTY.md` và thêm đầy đủ license text.
- [ ] Viết hướng dẫn build web assets có lockfile/reproducible command.
- [ ] Canvas Editor trở thành mặc định sau khi chủ GUI-test xác nhận.
- [ ] Giữ fallback legacy ít nhất một bản phát hành.
- [ ] Chỉ sau đó mới lập PR riêng để bỏ code/dependency `cosmic-text` không còn
  dùng; không trộn việc xóa lớn vào PR tích hợp.

Cổng P6: chủ dự án GUI-test OK, không mất dữ liệu fixture, installer offline đạt,
license/notice đầy đủ và rollback đã thử.

Ước lượng: 3–5 ngày sau khi các pha trước đạt.

## 5. Compatibility matrix DOCX v1

| Tính năng | Import | Export | Mức cam kết ban đầu |
|---|---:|---:|---|
| Unicode/tiếng Việt | Có | Có | Bắt buộc |
| Font/cỡ/đậm/nghiêng/gạch chân/màu | Có | Có | Bắt buộc |
| Căn lề/thụt dòng/giãn dòng | Có | Có | Bắt buộc |
| Bullet/numbering cơ bản | Có | Có | Bắt buộc |
| Page break, A4, margin | Có | Có | Bắt buộc |
| Bảng và merge cell cơ bản | Có | Có | Bắt buộc |
| Ảnh inline | Có | Có | Bắt buộc |
| Header/footer/số trang | Có | Có | Bắt buộc |
| Hyperlink/bookmark | Có | Có | Nên có |
| Floating image/wrap | Một phần | Một phần | Cảnh báo khi lệch |
| Comment/track changes | Chưa | Chưa | Hoãn |
| Footnote/endnote | Chưa | Chưa | Hoãn |
| TOC/field phức tạp | Chưa | Chưa | Hoãn |
| Equation/SmartArt/chart/OLE/macro | Không | Không | Loại khỏi phạm vi |

Mỗi thay đổi mức hỗ trợ phải cập nhật bảng này và fixture tương ứng.

## 6. Chiến lược test tối thiểu nhưng đủ an toàn

### 6.1 Tự động

- Converter phải là hàm thuần; unit test không khởi động WebView.
- IPC có contract test cho version, timeout, message sai và snapshot cũ.
- `.iai` có round-trip và backward migration tests.
- DOCX có structural assertions thay vì snapshot toàn bộ ZIP: text, style,
  relationship, media, numbering, section và header/footer.
- Mọi parser test có file hỏng/truncated/ZIP bomb giả lập ở kích thước nhỏ.

### 6.2 Fixture chuẩn

Dự kiến đặt ở `tests/fixtures/docx/`, tối thiểu:

1. `01-vietnamese-basic.docx`
2. `02-character-formatting.docx`
3. `03-paragraph-list.docx`
4. `04-page-section-header-footer.docx`
5. `05-table-merged-cells.docx`
6. `06-inline-image.docx`
7. `07-floating-image-wrap.docx`
8. `08-hyperlink-bookmark.docx`
9. `09-multipage-contract.docx`
10. `10-unsupported-features.docx`
11. `11-corrupt-truncated.docx`
12. `12-large-document.docx`

Mỗi fixture cần file `.md` cùng tên ghi ứng dụng tạo file, feature kỳ vọng và
những sai khác được chấp nhận. Không dùng source ONLYOFFICE để tạo converter.

### 6.3 Lệnh bắt buộc trước mỗi commit

```text
cargo fmt --all --check
cargo test --lib
```

Khi pha DOCX bắt đầu, bổ sung test target DOCX; khi web workspace xuất hiện, bổ
sung `pnpm lint`, `pnpm typecheck` và `pnpm test` đúng theo scripts đã pin.

## 7. Rủi ro và cách khóa

| Rủi ro | Cách khóa |
|---|---|
| WebView child HWND luôn nằm trên wgpu/egui | Toolbar văn bản ở trong web; ẩn WebView khi modal cần phủ vùng editor |
| Focus/phím tắt bị gửi cả Rust lẫn JS | Quy định ownership theo focus; test Ctrl/Alt/IME ở P1 |
| Mất snapshot khi đóng tab/app | Handshake snapshot có timeout trước close; không ghi file rỗng |
| CDN hoặc dependency đổi ngầm | Pin version + lockfile + bundle offline + hash assets |
| Schema Canvas Editor thay đổi | IPC/schema version và migration riêng |
| DOCX round-trip mất feature lạ | DOCX không canonical; warning + Save As + compatibility matrix |
| File DOCX độc hại | Limits ZIP/XML/image, chặn external relationship và macro/OLE |
| AGPL ảnh hưởng MIT | Không dùng code ONLYOFFICE; chỉ black-box interoperability testing |
| PDF bị raster, mất selectable text | Cổng P4 bắt buộc kiểm select/search/copy |
| Xóa engine cũ quá sớm | Feature flag + giữ fallback ít nhất một release |

## 8. Quy tắc làm việc cho các phiên sau

1. Đọc mục 0, pha đang làm và Changelog cuối file trước khi sửa code.
2. Chỉ làm một pha/cổng tại một thời điểm; không code DOCX khi P1/P2 chưa đạt.
3. Đổi `[ ]` → `[~]` khi bắt đầu; chỉ đổi `[x]` sau khi ghi bằng chứng test.
4. Nếu đổi quyết định đã khóa, thêm ADR hoặc ghi rõ quyết định, lý do và ảnh
   hưởng migration trong Changelog.
5. Không chỉnh/xóa thay đổi không liên quan đang có trong worktree.
6. Commit local theo lát cắt nhỏ; push chỉ khi chủ dự án yêu cầu.
7. Cuối mỗi phiên cập nhật: trạng thái tổng thể, việc kế tiếp duy nhất, checklist,
   lệnh test/kết quả và Changelog.

## 9. Changelog triển khai

### 2026-09-08 — Khởi tạo kế hoạch

- Chốt Canvas Editor làm UI/engine bố cục và `docx-rs` làm DOCX adapter.
- Chốt WebView2 qua `wry`, Windows-first và bundle offline.
- Chốt Canvas Editor JSON trong `.iai` là canonical; DOCX chỉ import/export.
- Chốt cấm sao chép/chuyển thể source ONLYOFFICE AGPL vào mã MIT.
- Chưa thay đổi code hoặc dependency; việc kế tiếp là baseline + Pha 1 spike.

### 2026-09-08 — Pha 1 spike offline và IPC `[~]`

- Baseline ngay trước khi code: `cargo test --lib` → `1625 passed; 0 failed;
  10 ignored` trong `249.33s`.
- Pin `@hufe921/canvas-editor` `1.0.2`, integrity khóa pnpm
  `sha512-uAdI70JPqakd9+8TmSO1Fv77xDG86vCXaAVfqwMChJnAiz9Ub77l2ep0t8/lnydoIjdFqywcs+dea7x2f9vNEg==`;
  pin `wry` `0.56.1`, checksum Cargo
  `375becb4aded9913f736443cf88000c6311478db69814cc06070465e4cc44c98`.
- Thêm feature mặc định-tắt `canvas-editor-webview`; `wry` chỉ là dependency
  Windows tùy chọn. Khi tắt feature hoặc khởi tạo WebView2 lỗi, Document mode
  tiếp tục dùng editor `cosmic-text` hiện có.
- Tạo `web/document-editor/`, bundle JS/CSS và Noto Sans Vietnamese vào binary
  bằng `include_bytes!`; runtime chỉ phục vụ năm asset cố định qua custom protocol
  `iai-editor`. CSP chặn `connect-src`, frame, object và form; navigation, cửa sổ
  mới và download ra ngoài origin offline đều bị từ chối. Không dùng CDN và
  không thêm source ONLYOFFICE.
- SHA-256 bundle đã chạy: `index.html`
  `2ECCD328E4E19B222F292691F611D37D29519FFD8A59D76A4CBE7F7A3B161717`,
  `editor.css`
  `3D13040790CDC1D704AB128E495426A7701131EDD488623A1F892A1070FCFD7C`,
  `editor.js`
  `71E735AFED1E5755BCA8B284FA0AF2D75C2A20D870613D596F73B8FB59A111D2`,
  font WOFF
  `D2E105791E742B90799D015F86ECBC376B29A31AEB605F77937BB846DFA36985`,
  font WOFF2
  `0F0C8A4858F1F5B2701331CF0A471EE00824ED92D33ECB21C9248DED975E297F`.
- IPC Pha 1 có envelope versioned, giới hạn message 16 KiB, kiểm
  `request_id`, kiểm đúng editor `1.0.2`, `ready` và ping/pong. WebView lấy bounds
  central viewport, đổi logical → physical theo scale factor, ẩn khi popup/modal,
  occluded hoặc minimize, và bị drop khi không còn tài liệu FlowText.
- Kiểm web: `pnpm lint`, `pnpm typecheck`, `pnpm test`, `pnpm build` đều đạt;
  Vitest `1 passed`; Vite tạo bundle offline thành công (chỉ cảnh báo chunk lớn).
- Kiểm Rust theo feature:
  `cargo test --features canvas-editor-webview app::document_webview::tests` →
  `5 passed; 0 failed`; `cargo check --features canvas-editor-webview` đạt.
  Test bao phủ hợp đồng ready/pong, message sai version/quá cỡ, asset protocol
  offline, khóa navigation và phép đổi bounds ở DPI 125%.
- Smoke thật bằng
  `cargo run --features canvas-editor-webview --bin iai -- <fixture-flowtext.iai>`:
  log lần lượt `Canvas Editor 1.0.2 ready; checking IPC ping/pong...` và
  `Canvas Editor 1.0.2 ready — IPC ping/pong OK`; quan sát một process con
  `msedgewebview2.exe`. Sau khi đóng app: `IAI_PROCESS_COUNT_AFTER_CLOSE=0` và
  `SMOKE_WEBVIEW_PROCESS_COUNT_AFTER_CLOSE=0`.
- Hậu kiểm mặc định: `cargo fmt --all --check` đạt; `cargo test --lib` →
  `1625 passed; 0 failed; 10 ignored` trong `261.67s`. Ba cảnh báo compiler đã
  tồn tại từ baseline, không phát sinh từ spike.
- Cổng P1 chưa đóng: còn thiếu xác nhận bản sửa căn giữa, đủ bốn mức DPI và 20
  vòng đóng/mở tab.

### 2026-09-08 — Sửa căn giữa khung giấy, chờ xác nhận GUI `[~]`

- Chủ dự án báo smoke test GUI đạt, bao gồm nhập tiếng Việt và các thao tác đã
  yêu cầu; lỗi quan sát được duy nhất là khung giấy cố định sát trái thay vì căn
  giữa vùng Document mode.
- Nguyên nhân: Canvas Editor tạo wrapper khổ giấy cố định làm con trực tiếp,
  nhưng host CSS chưa cấp `margin-inline: auto`; vùng host đồng thời đang khóa
  overflow. Đã thêm căn giữa wrapper, khoảng đệm dọc và cho phép scroll khi khổ
  giấy lớn hơn viewport.
- `pnpm lint`, `pnpm typecheck`, `pnpm test`, `pnpm build` đều đạt; Vitest
  `1 passed`. `editor.css` mới có SHA-256
  `3D13040790CDC1D704AB128E495426A7701131EDD488623A1F892A1070FCFD7C`.
- `cargo test --features canvas-editor-webview app::document_webview::tests` →
  `5 passed; 0 failed`.
- Bản test Release mới build thành công bằng
  `cargo build --release --features canvas-editor-webview --bin iai` trong
  `10m 17s`: `target/release/iai.exe`, `73,085,952` byte, SHA-256
  `4E670D2DFD1C8B465CEE1F02AB8AEFEDDA4A00AC085083E07C2C5D0F36CC9A2C`.
- Chưa đóng mục căn giữa cho tới khi chủ dự án xác nhận trực tiếp trên bản
  Release mới.

### 2026-09-08 — Đóng Pha 1, bắt đầu bridge Pha 2 `[~]`

- Chủ dự án xác nhận bản Release sửa căn giữa hoạt động đúng và yêu cầu tiếp tục
  theo kế hoạch. Pha 1 được đóng theo kết quả GUI-test của chủ dự án; fallback
  `cosmic-text`, feature flag và chính sách offline giữ nguyên.
- Mở rộng IPC version 1 ở cả Rust và TypeScript với `load_document`,
  `request_snapshot`, `document_changed`, `snapshot`, `save_requested`,
  `focus_editor` và `error`. Mọi response giữ nguyên `request_id`; revision là
  số tăng đơn điệu từ listener `contentChange` của Canvas Editor.
- Rust kiểm root Canvas JSON (`main`, `header`, `footer`), giới hạn control
  message `16 KiB` và snapshot `8 MiB`; không log payload tài liệu. Ngay sau
  ready/ping, host nạp probe tiếng Việt rồi yêu cầu snapshot và so sánh JSON cùng
  revision; thành công sẽ báo `IPC load/snapshot round-trip OK`.
- `pnpm lint`, `pnpm typecheck`, `pnpm test`, `pnpm build` đều đạt; Vitest
  `2 passed`. Bundle `editor.js` mới có SHA-256
  `1AB11E84DAF84D9618B251B1250D642F5D150F228B8EBAC2898F141D7021052E`.
- `cargo fmt --all --check` đạt;
  `cargo test --features canvas-editor-webview app::document_webview::tests` →
  `7 passed; 0 failed`. Baseline feature-tắt `cargo test --lib` →
  `1625 passed; 0 failed; 10 ignored` trong `121.60s`.
- Bản test Release build thành công bằng
  `cargo build --release --features canvas-editor-webview --bin iai` trong
  `5m 52s`: `target/release/iai.exe`, `73,113,088` byte, SHA-256
  `8F9BB3A92652E4B83D543B4B5A6A85DD518EB467DA9E95B16E5EB5FB2FCBA617`.
- Lát cắt này chưa thay canonical model, chưa ghi `.iai` v12 và chưa thay luồng
  Save legacy. Bước tiếp theo chỉ được nối state/Save sau khi runtime round-trip
  được xác nhận.

### 2026-09-09 — Save snapshot gate có timeout, chờ xác nhận GUI `[~]`

- Ctrl+S, menu Save và Save As của FlowText khi Canvas Editor đang hoạt động nay
  yêu cầu snapshot IPC trước. Snapshot chỉ được nhận khi `request_id` và revision
  khớp; yêu cầu đồng thời bị từ chối để tránh đảo thứ tự dữ liệu.
- Host đặt timeout 2 giây bằng event-loop deadline, không block UI thread. Khi
  timeout, WebView chưa ready hoặc snapshot cũ, status nói rõ không có file nào
  được ghi. Vì writer `.iai` v12 chưa tồn tại, Save hiện cố ý dừng sau khi cache
  snapshot thay vì rơi xuống writer FlowText legacy; nếu WebView2 khởi tạo thất
  bại thì fallback `cosmic-text` vẫn dùng luồng Save cũ.
- Không thay bundle web, canonical model hay version `.iai`; không dùng CDN và
  không log payload snapshot. Không sửa hai thay đổi có sẵn ngoài phạm vi là
  `docs/planning/KE_HOACH_TRINH_SOAN_VAN_BAN_2026-09-01.md` và
  `docs/cleanup-report-2026-09-06.md`.
- `cargo fmt --all --check` và `git diff --check` đạt;
  `cargo test --features canvas-editor-webview app::document_webview::tests` →
  `8 passed; 0 failed`. Baseline feature-tắt `cargo test --lib` →
  `1625 passed; 0 failed; 10 ignored` trong `36.38s`.
- Bản test Release build thành công bằng
  `cargo build --release --features canvas-editor-webview --bin iai` trong
  `5m 59s`: `target/release/iai.exe`, `73,115,648` byte, SHA-256
  `49778D7A9BF1C9A1517E5844D9C62BEDE0E7C7BEED6D5F0EF385FAB41222C32B`.
- Cần chủ dự án xác nhận trên GUI: lúc mở Document mode, status đạt
  `IPC load/snapshot round-trip OK`; sau khi gõ rồi nhấn Ctrl+S hoặc menu Save,
  status đạt `snapshot captured at revision …` và ứng dụng vẫn phản hồi.

### 2026-09-09 — Cache theo tab và snapshot trước Close/Exit `[~]`

- Chủ dự án xác nhận smoke test Release của lát cắt trước đạt. Runtime
  `load_document`/`request_snapshot` và Save snapshot gate được nghiệm thu; mục
  round-trip Pha 2 chuyển sang `[x]`.
- Thay state bridge đơn bằng cache `DocumentId → document/revision/dirty`.
  Chuyển khỏi tab có thay đổi chưa snapshot sẽ chờ snapshot đúng revision; khi
  quay lại, host nạp đúng payload đã cache của tab đó. Snapshot chỉ cập nhật bản
  sao RAM, không xóa dirty-state so với file.
- Dirty-state Canvas Editor nay tham gia dấu modified trên tab, Close và
  app-exit. Close/Exit chỉ hiện dialog sau khi payload mới nhất đã snapshot;
  timeout 2 giây hủy thao tác thay vì đóng mất dữ liệu. State cache được xóa khi
  tab thực sự bị đóng.
- Vì writer `.iai` v12 chưa có, `Save & Close` và `Save & Exit` không được phép
  đóng sau snapshot RAM; dialog giữ nguyên để người dùng Cancel hoặc chủ động bỏ
  thay đổi. Fallback feature-tắt và fallback khi WebView2 lỗi vẫn đi luồng cũ.
- `cargo fmt --all --check`, `git diff --check`, `cargo check --lib` và
  `cargo check --features canvas-editor-webview --bin iai` đạt;
  `cargo test --features canvas-editor-webview app::document_webview::tests` →
  `10 passed; 0 failed`. Baseline feature-tắt `cargo test --lib` →
  `1625 passed; 0 failed; 10 ignored` trong `224.21s`.
- Bản test Release build thành công bằng
  `cargo build --release --features canvas-editor-webview --bin iai` trong
  `5m 38s`: `target/release/iai.exe`, `73,132,544` byte, SHA-256
  `7888612F07CF8A48CCEB2DC46D89B010A4EE2068A2FADD903885A29EEADA6466`.
- Cần chủ dự án xác nhận GUI: hai tab giữ nội dung độc lập khi chuyển qua lại;
  tab đã gõ có dấu modified; Close và nút đóng cửa sổ hiện dialog mà không mất
  nội dung; `Save & Close`/`Save & Exit` chưa đóng ứng dụng trong lát cắt này.

### 2026-09-09 — Sửa race snapshot-first sau GUI-test thất bại `[~]`

- Chủ dự án kiểm thử bản Release trước và xác nhận lát cắt lifecycle chưa đạt:
  tab A/B dùng chung nội dung, đóng tab dirty không hỏi và nút đóng cửa sổ thoát
  ngay thay vì hiện dialog Exit. Vì vậy các mục dirty/tab lifecycle và Close/Exit
  vẫn giữ `[~]`, chưa được nghiệm thu.
- Nguyên nhân là thao tác Switch/Close/Exit có thể kiểm tra cache dirty phía Rust
  trước khi message `document_changed` đang chờ trong WebView được host xử lý,
  rồi đi nhầm luồng tài liệu sạch. Bản sửa nay luôn yêu cầu snapshot cho tab
  FlowText đang active trước cả ba thao tác, không còn phụ thuộc dirty cache đã
  kịp cập nhật hay chưa.
- Snapshot trả về có revision mới hơn revision host vừa quan sát được được xem là
  dữ liệu có thẩm quyền. Phía JavaScript còn đối chiếu JSON chuẩn hóa với
  snapshot quan sát gần nhất để tăng revision nếu content listener bỏ sót thay
  đổi; sau `load_document` baseline này được đặt lại theo đúng payload vừa nạp.
- Sau snapshot, host mới quyết định chuyển tab, hiện dialog Close hoặc tiếp tục
  luồng Exit. Nhánh tiếp tục Exit tách riêng để không tự yêu cầu snapshot lần hai;
  timeout/WebView chưa ready vẫn hủy thao tác thay vì làm mất dữ liệu.
- Web checks đạt: `npm run lint`, `npm run typecheck`,
  `npm test -- --run` → `3 passed`; `npm run build` thành công và sinh bundle
  offline `web/document-editor/dist/editor.js`, SHA-256
  `DE791FF81CECA3CAA41C883BA9F185810ADB63ADCAD72BA63793F9DA6E50F802`.
- Rust checks đạt: `cargo fmt --all --check`, `git diff --check`;
  `cargo test --features canvas-editor-webview app::document_webview::tests` →
  `10 passed; 0 failed`; baseline feature-tắt `cargo test --lib` →
  `1625 passed; 0 failed; 10 ignored` trong `36.18s`.
- Lần build Release đầu bị Windows từ chối thay `target/release/iai.exe` do một
  tiến trình bản test cũ không còn cửa sổ vẫn giữ khóa file. Sau khi xác định và
  dừng đúng PID 8184, lệnh
  `cargo build --release --features canvas-editor-webview --bin iai` thành công
  trong `2m 39s`: `target/release/iai.exe`, `73,133,056` byte, SHA-256
  `D790DD1C2BF01238C9A0C57341157A0497875AB5701A3DEBAF89C9DE1C79B7D1`.
- Chờ chủ dự án kiểm thử lại đúng các ca A/B độc lập, Close dirty và Exit/Cancel.
  Writer `.iai` v12 vẫn chưa được triển khai nên `Save & Close`/`Save & Exit`
  tiếp tục cố ý không đóng; chưa nối autosave và không thay fallback cosmic-text.

### 2026-09-09 — Chặn tái nhập snapshot gate khi Switch/Exit `[~]`

- GUI-test bản sửa trước tiếp tục chưa đạt: click tài liệu mới hoặc tab khác không
  chuyển được, chỉ nút X còn phản hồi; sau đó ứng dụng cũng không thể thoát.
- Nguyên nhân xác định được là callback `LifecycleCompletion::Switch` gọi lại
  entrypoint `switch_to_doc`, entrypoint này lại yêu cầu snapshot mới. Mỗi
  snapshot hoàn tất vì vậy tự tạo snapshot kế tiếp vô hạn; Exit bị từ chối do đã
  có request pending.
- Tách `switch_to_doc_confirmed` làm đường hoàn tất không qua snapshot gate.
  Callback Switch và dirty-tab sweep khi Exit dùng đường này; chỉ thao tác chuyển
  tab ban đầu của người dùng mới yêu cầu snapshot. Thêm test hồi quy xác nhận
  đường confirmed chuyển thẳng tới đúng `DocumentId` đích.
- Khi Exit của tài liệu sạch hoàn tất snapshot, callback nay yêu cầu thêm một
  redraw để pha action kế tiếp tiêu thụ `exit_requested` và gọi event-loop exit;
  trước đó cờ có thể nằm chờ nếu không còn event mới.
- `cargo fmt --all --check` và `git diff --check` đạt. Nhóm quản lý tài liệu →
  `8 passed; 0 failed`; bridge WebView → `10 passed; 0 failed`; baseline
  feature-tắt `cargo test --lib` → `1626 passed; 0 failed; 10 ignored` trong
  `263.33s`.
- Bản test Release build thành công bằng
  `cargo build --release --features canvas-editor-webview --bin iai` trong
  `9m 30s`: `target/release/iai.exe`, `73,133,056` byte, SHA-256
  `05D0CF3B788EE2B0DC6731F5AF48BD9BBC0FF4818DB2608D73A380769FD7E818`.
- Chờ chủ dự án kiểm thử lại việc tự chuyển sang tab vừa tạo, click chuyển tab,
  nội dung độc lập, Close dirty và Exit/Cancel. Các checklist liên quan vẫn giữ
  `[~]`; chưa triển khai autosave hay writer `.iai` v12.

### 2026-09-09 — Giữ click khi bridge chưa ready và chấp nhận JSON chuẩn hóa `[~]`

- Chủ dự án xác nhận status sau khi click là `Canvas Editor tab is not ready`.
  Như vậy tabbar đã phát action nhưng host chủ động hủy action do probe chưa
  verified hoặc WebView chưa bind đúng active `DocumentId`.
- Switch intent nay được giữ theo cặp source/target `DocumentId` nếu probe,
  `load_document` hoặc snapshot khác còn pending. Ngay khi bridge verified và
  bind đúng source, host tự yêu cầu snapshot rồi chuyển tab; người dùng không
  phải click lại. Nếu một Switch cùng source đang chạy, click mới chỉ retarget
  completion tới tab đích mới nhất, không tạo snapshot thứ hai.
- Probe không còn đòi JSON snapshot bằng tuyệt đối payload gửi vào, vì Canvas
  Editor hợp lệ có thể bổ sung metadata mặc định. Parser vẫn bắt buộc schema và
  revision đúng; probe đối chiếu chuỗi nội dung vùng `main`, kể cả khi editor
  tách chuỗi thành nhiều element hoặc thêm style/type metadata.
- Thêm hai test hồi quy: probe chấp nhận metadata chuẩn hóa nhưng giữ nguyên nội
  dung; switch intent chờ qua Probe và retarget đúng snapshot Switch đang chạy.
  `cargo test --features canvas-editor-webview app::document_webview::tests` →
  `12 passed; 0 failed`; nhóm `app::docmgr::tests` → `8 passed; 0 failed`.
  Baseline feature-tắt gần nhất vẫn đạt `1626 passed; 0 failed; 10 ignored`;
  `cargo fmt --all --check` và `git diff --check` đạt.
- Lần build đầu gặp khóa `iai.exe` thoáng qua rồi tiến trình giữ khóa tự kết
  thúc; không phải lỗi code. Build lại bằng
  `cargo build --release --features canvas-editor-webview --bin iai` thành công
  trong `6m 15s`: `target/release/iai.exe`, `73,134,080` byte, SHA-256
  `2F7DFFBE57D401EF7C540A0EF8840196E353CD1DF65D779BEFF4A27795ACBF69`.
- Chờ GUI-test lại việc tạo/chọn/chuyển tab, nội dung độc lập và Exit/Cancel;
  checklist lifecycle giữ `[~]`. Chưa nối autosave/writer `.iai` v12, không đổi
  offline bundle, feature flag hoặc fallback cosmic-text.

### 2026-09-09 — GUI xác nhận queued-switch và cache theo tab đạt `[~]`

- Chủ dự án xác nhận bản Release SHA-256
  `2F7DFFBE57D401EF7C540A0EF8840196E353CD1DF65D779BEFF4A27795ACBF69`
  hoạt động đúng: tạo tab mới, click chuyển qua lại và nội dung theo tab đã đạt.
- Log runtime là thông tin chẩn đoán bình thường, không phải lỗi:
  `ready` → `ping/pong OK` → `IPC load/snapshot round-trip OK`. Các revision
  `23`, `35`, `24` được cache trước ba lần chuyển tab và revision `36` được cache
  trước Close; không có panic, timeout, stale snapshot hoặc IPC error.
- Ghi nhận phần queued-switch/probe chuẩn hóa và cache độc lập theo tab đã qua
  GUI-test. Mục dirty revision vẫn `[~]` cho đến khi kiểm riêng zoom/selection;
  mục Close/Exit vẫn `[~]` vì bằng chứng hiện tại chưa có
  `cached revision … before app exit` và kết quả Cancel giữ nguyên nội dung.
- Việc kế tiếp chỉ là hoàn tất hai kiểm tra GUI còn lại trên chính bản Release
  này; chưa nối autosave, writer `.iai` v12, DOCX hoặc thay fallback cosmic-text.

### 2026-09-09 — Giữ yêu cầu Exit qua snapshot/bridge đang bận `[~]`

- Chủ dự án kiểm thử và báo nút đóng cửa sổ không hiện dialog Exit, ứng dụng bị
  mắc ở trạng thái không thể đóng. Log không có dòng
  `cached revision … before app exit`, nên lỗi xảy ra trước completion Exit chứ
  không phải do người dùng chọn sai nút trong dialog.
- Nguyên nhân là `CloseRequested` chỉ đến một lần nhưng snapshot gate trước đây
  kết thúc sớm khi bridge chưa verified, WebView chưa bind đúng tab active hoặc
  đang có snapshot khác. Yêu cầu đóng vì vậy bị mất sau khi event đã được host
  nhận, trong khi cửa sổ vẫn bị giữ mở để bảo vệ dữ liệu.
- Thêm trạng thái deferred Exit tồn tại qua probe/load/snapshot đang chạy. Exit
  xóa intent chuyển tab đang chờ, đợi lifecycle completion hiện tại hoàn tất và
  chỉ yêu cầu snapshot ở frame ổn định khi WebView cùng host trỏ tới đúng
  `DocumentId`. Nếu snapshot Save/Switch/Close của chính tài liệu active đã gửi
  đi, Exit chuyển mục đích completion của request đó thành Exit và dùng ngay
  payload trả về; pending Switch vì vậy không thể chuyển sang tab khác làm mắc
  luồng đóng. Timeout vẫn không đóng mất dữ liệu.
- Thêm test hồi quy cho điều kiện deferred Exit: chỉ chạy khi request thực sự đã
  được giữ, không có snapshot pending, bridge verified và active document hai
  phía khớp nhau; test riêng xác nhận snapshot Switch đang bay được promote sang
  Exit nhưng giữ nguyên request/deadline. `cargo fmt --all` đạt;
  `cargo test --features canvas-editor-webview app::document_webview::tests` →
  `14 passed; 0 failed`; nhóm `app::docmgr::tests` → `8 passed; 0 failed`.
  Baseline feature-tắt gần nhất vẫn đạt
  `1626 passed; 0 failed; 10 ignored`.
- Bản test Release build thành công bằng
  `cargo build --release --features canvas-editor-webview --bin iai` trong
  `10m 29s`: `target/release/iai.exe`, `73,134,592` byte, SHA-256
  `4C2BFF6E0BFD97C23E040715370696E0E4516809A85C552DF8BCE9B4E227117D`.
- Chờ chủ dự án xác nhận: đóng cửa sổ với tab dirty phải hiện Exit; chọn Cancel
  giữ nguyên nội dung và app tiếp tục phản hồi; đóng cửa sổ khi không dirty phải
  thoát được. Khi Exit dirty chạy đúng, log phải có
  `cached revision … before app exit`. Chưa nối autosave/writer `.iai` v12,
  không đổi bundle offline, feature flag hay fallback cosmic-text.

### 2026-09-09 — Nghiệm thu Close/Exit và nối autosave snapshot `[x]`

- Chủ dự án xác nhận bản Release SHA-256
  `B1F7FB03D97117BD9B0E772A5550BEB19C740D1F548122D513BB27C2D799F1BC`
  đã hiện đúng dialog khi đóng tab dirty, cho phép Cancel/Close và đóng ứng dụng
  bình thường. Checklist Close/Exit của Pha 2 chuyển sang `[x]`.
- Nguyên nhân dialog Close từng không thấy là child WebView2 nằm trên egui nhưng
  `show_close_dialog` bị thiếu trong `is_blocking_modal`; dialog đã được tạo phía
  sau WebView. Bổ sung cờ modal và test hồi quy xác nhận WebView phải ẩn trong
  khi chính dialog vẫn được phép xử lý action.
- Autosave FlowText nay yêu cầu snapshot bất đồng bộ khi tài liệu dirty và chu
  kỳ 90 giây đến hạn. Snapshot dùng chung timeout 2 giây, validation revision và
  cache theo `DocumentId`; writer recovery xuống đĩa vẫn chờ `.iai` v12 ở Pha 3.
- Save, Close và Exit có thể dùng lại/nâng mục đích snapshot autosave đang bay,
  nên không phát request chồng và không làm mất thao tác lifecycle. Switch vẫn
  được giữ rồi chạy sau khi autosave completion trả về.
- `cargo fmt --all`, `git diff --check` đạt; bridge WebView → `16 passed; 0
  failed`; nhóm quản lý tài liệu → `9 passed; 0 failed`; baseline
  `cargo test --lib` → `1627 passed; 0 failed; 10 ignored` trong `112.00s`.
- Bản test Release build thành công bằng
  `cargo build --release --features canvas-editor-webview --bin iai` trong
  `9m 51s`: `target/release/iai.exe`, `73,136,128` byte, SHA-256
  `D5995CD69AD4136622F3512D901CC6D50E3A84DDCB6389EE7FC40A95C1C278E2`.
- Chủ dự án xác nhận GUI-test đạt: chỉ zoom/selection không làm tab dirty;
  autosave cache snapshot sau chu kỳ 90 giây; Close/Exit vẫn hiện dialog và không
  kẹt. Dirty revision và autosave vì vậy được chuyển sang `[x]`.

### 2026-09-09 — Đồng bộ theme và read-only vào Canvas Editor `[~]`

- Theme ứng dụng được ánh xạ sang IPC `set_theme`; Canvas Editor áp dụng theme
  sau khi bridge ready và chỉ nhận lại khi giá trị thực sự đổi. iAi hiện chỉ có
  theme tối; protocol/CSS đã chừa đường cho theme sáng về sau.
- `FlowTextDocumentState` có trạng thái `read_only` theo phiên, không tăng
  revision và không làm tài liệu dirty. Khi mở `.iai`, trạng thái được lấy từ
  thuộc tính read-only của file; bridge chuyển đúng `EditorMode.READONLY` hoặc
  `EditorMode.EDIT` theo tab đang hoạt động.
- Web editor đạt lint, typecheck, `vitest run` (`3 passed; 0 failed`) và
  `vite build`; bundle offline trong `web/document-editor/dist` đã được build
  lại. Rust đạt nhóm `core::document::tests` (`27 passed; 0 failed`), nhóm
  `app::document_webview::tests` (`17 passed; 0 failed`) và baseline
  `cargo test --lib` (`1628 passed; 0 failed; 10 ignored`) trong `63.12s`.
- Bản test Release build thành công bằng
  `cargo build --release --features canvas-editor-webview --bin iai` trong
  `5m 25s`: `target/release/iai.exe`, `73,148,416` byte, SHA-256
  `2DAF0419BB8A9DEAE774F80538EFBD4B3839CCBA1D4AEF519AA0AFD5AA8E1B3F`.
- Chờ GUI-test: nền editor phải khớp theme tối, không ló viền/nền sáng; tab mở từ
  file `.iai` có thuộc tính Windows Read-only phải chặn nhập liệu, còn tab bình
  thường vẫn nhập được; chuyển qua lại giữa hai tab phải khôi phục đúng mode.

### 2026-09-09 — Dialog lỗi bridge/timeout không làm kẹt lifecycle `[~]`

- Lỗi do Canvas Editor gửi về nay giữ nguyên `request_id`, được đưa từ bridge
  vào dialog `Canvas Editor Error` thay vì chỉ nằm trong status. Lỗi IPC sai
  định dạng, snapshot sai/stale, snapshot timeout và lỗi WebView khởi tạo đều đi
  qua cùng đường thông báo.
- Khi snapshot lỗi/timeout, host xóa request snapshot cùng các ý định
  Switch/Close/Exit đang chờ, hủy lifecycle completion trong cùng lượt IPC và
  giữ nguyên tài liệu đang mở; không file nào bị ghi đè. Dialog là modal nên
  WebView2 bị ẩn, nút OK/Esc/Enter vẫn đóng được; nút đóng cửa sổ bị chặn cho tới
  khi người dùng xác nhận lỗi rồi có thể thử lại.
- Bridge WebView đạt `19 passed; 0 failed`; test modal/lifecycle đạt; baseline
  `cargo test --lib` đạt `1629 passed; 0 failed; 10 ignored` trong `36.06s`.
  `cargo fmt --all --check` và `git diff --check` đạt; chỉ còn các cảnh báo
  `GlyphStyle`/`f32` có sẵn ngoài lát cắt này.
- Bản test Release build thành công bằng
  `cargo build --release --features canvas-editor-webview --bin iai` trong
  `5m 30s`: `target/release/iai.exe`, `73,159,168` byte, SHA-256
  `3708865712D1DC15248199904B2E3B69FBC56E6A3D8FE72A7A6ED71E44B90F82`.
- Chờ GUI-test hồi quy: tạo/gõ/chuyển/đóng tab và đóng app vẫn hoạt động như bản
  đã nghiệm thu; theme/read-only giữ đúng theo từng tab. Nếu runtime tự phát sinh
  lỗi bridge, dialog phải nằm trên editor, OK đóng được và tab/app vẫn phản hồi.

### 2026-09-09 — Nền tảng backing và định dạng `.iai` v12 `[~]`

- `FlowTextDocumentState` nay phân biệt rõ `Legacy(TextDocument)` và
  `CanvasEditor(CanvasEditorDocument)`. Payload Canvas Editor là JSON canonical,
  không chiếu qua model legacy nên unknown fields không bị mất; clone dùng
  `Arc`, revision/read-only/layout tiếp tục là state riêng có kiểm soát.
- Loader nhận v10/v11 bằng đường legacy hiện hữu và chỉ nhận v12 cho
  `flow_text_document` có `editor: canvas-editor`. V12 tách payload sang
  `document.json`; manifest bị giới hạn 1 MiB, payload 8 MiB, JSON depth 64 và
  200.000 node để chặn cấp phát/đệ quy không giới hạn.
- Writer legacy tiếp tục đóng dấu v11. Writer Canvas Editor mới đóng dấu v12,
  giữ unknown fields và ghi file tạm rồi thay đích bằng atomic replace có
  `MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH` trên Windows; test ghi đè
  cùng một file rồi mở lại đã đạt.
- Khi mở file v12, payload canonical được dùng để seed cache theo tab của
  WebView thay vì tài liệu trắng. Đồng bộ snapshot ngược về core và gọi writer
  từ Save/Save As được giữ cho lát cắt kế tiếp để không đánh dấu saved trước khi
  ghi file thành công.
- Test `formats::iai::tests` đạt `40 passed; 0 failed`; nhóm core document đạt
  `28 passed; 0 failed`; bridge WebView đạt `20 passed; 0 failed`; baseline
  `cargo test --lib` đạt `1632 passed; 0 failed; 10 ignored` trong `245.05s`.
  `cargo fmt --all --check`, `git diff --check` và build có feature Canvas Editor
  đều đạt.
- Bản test Release build thành công bằng
  `cargo build --release --features canvas-editor-webview --bin iai` trong
  `9m 45s`: `target/release/iai.exe`, `73,187,840` byte, SHA-256
  `147273FEF1DF80E61762990036D2B1D357729C6D110A0029BD2C585F585D8C56`.
- Đây là lát cắt nền tảng: Save trên Canvas Editor vẫn chỉ cache snapshot như
  bản trước, chưa ghi v12. Việc kế tiếp là đưa snapshot đã validation vào backing
  core rồi mới kích hoạt Save/Save As v12.

### 2026-09-09 — Nối snapshot core và Save/Save As `.iai` v12 `[~]`

- Mọi snapshot Autosave/Save/Switch/Close/Exit hợp lệ nay được bọc thành
  `CanvasEditorDocument` rồi đẩy vào đúng `FlowTextDocumentState` theo
  `DocumentId` và revision trước khi chạy lifecycle completion. Validation lỗi
  giữ tài liệu mở và không gọi writer.
- Ctrl+S/menu Save ghi đè `.iai` hiện có; Save As luôn mở hộp chọn file rồi ghi
  payload Canvas Editor v12. Core và cache WebView chỉ được đánh dấu sạch sau
  khi atomic writer thành công đúng revision; nếu editor đã có revision mới hơn,
  dirty state vẫn được giữ.
- Save & Close/Save & Exit giữ continuation trong lúc chờ snapshot hoặc file
  dialog. Thứ tự action Save & Close được sửa để Save thực sự chạy trước Close;
  lỗi ghi file hoặc hủy dialog sẽ mở lại xác nhận và không đóng tab/app.
- Test bridge đạt `22 passed; 0 failed`; test định dạng đạt
  `40 passed; 0 failed`; baseline có feature Canvas Editor đạt
  `1654 passed; 0 failed; 10 ignored`. Cả `cargo check` mặc định và có feature,
  `cargo fmt --all` đều đạt; chỉ còn warning `GlyphStyle`/`f32` có sẵn ngoài lát
  cắt.
- Trạng thái giữ `[~]` cho đến khi GUI-test Save/Save As, mở lại v12,
  Save & Close và Save & Exit trên bản Release đạt.
- Bản test Release build thành công bằng
  `cargo build --release --features canvas-editor-webview --bin iai` trong
  `5m 58s`: `target/release/iai.exe`, `73,185,792` byte, SHA-256
  `3BE52CF84A44E4F7BE153A8A4B3356B24668D31FF2244838A16933B52C7693D4`.

### 2026-09-10 — Khôi phục Ctrl+W và converter Legacy → Canvas `[~]`

- WebView bắt `Ctrl+W`/`Cmd+W` ở capture phase và gửi `close_requested` về host,
  nên phím tắt vẫn hoạt động khi caret/focus nằm trong Canvas Editor. Host giữ ý
  định đóng qua probe/snapshot đang chạy, dùng lại dialog Save/Discard/Cancel và
  ưu tiên Exit > Close > Switch để không mất thao tác đóng.
- Converter một chiều map run chữ, font/cỡ/màu/emphasis, paragraph alignment và
  line spacing, nhóm bullet/numbering liên tiếp, page size/margin và ảnh
  inline/block/floating. Spacing/indent chưa render được vẫn được giữ trong
  metadata `extension.iai_legacy_paragraph`; encoding ảnh không hỗ trợ sinh
  warning thay vì âm thầm mất dữ liệu.
- WebView giữ các root field ngoài dữ liệu editor, gồm `_iai.page_setup`, qua mỗi
  snapshot và áp dụng paper size/margin khi load. Chuyển đổi chỉ commit vào core
  sau khi bridge sẵn sàng; lỗi khởi tạo/probe vẫn giữ backing legacy để fallback.
- Test web đạt typecheck, lint, bundle production và `5 passed; 0 failed`.
  Test converter Rust đạt `2 passed`; bridge đạt `23 passed`; baseline có feature
  Canvas Editor đạt `1657 passed; 0 failed; 10 ignored` trong `185.25s`.
  `cargo check --bin iai`, `cargo fmt --all -- --check` và `git diff --check`
  đều đạt; còn hai warning literal `f32` có sẵn ở `src/ui/library.rs` ngoài lát
  cắt này.
- Bản test Release build thành công bằng
  `cargo build --release --features canvas-editor-webview --bin iai` trong
  `11m 13s`: `target/release/iai.exe`, `73,223,680` byte, SHA-256
  `BC2AB02DE708CC8463FD5EC1D6A09BD77B45E63E37A41A89E6693551B876FB17`.
- Chờ GUI-test Ctrl+W trên tab dirty/clean, Save & Close/Cancel, mở lại v12 và
  đối chiếu một file legacy thực tế trước khi đạt cổng P3.

### 2026-09-10 — Pha 3.5: toolbar định dạng cơ bản `[~]`

- Bổ sung toolbar nằm trong WebView gồm Undo/Redo, font, cỡ chữ theo point,
  Bold/Italic/Underline, màu chữ, căn trái/giữa/phải/đều, bullet/numbering, giãn
  dòng và zoom. Nút toolbar giữ selection trước khi gọi command và trả focus về
  editor để không làm gián đoạn nhập văn bản.
- `rangeStyleChange` đồng bộ trạng thái nút/font/cỡ/màu/đoạn theo caret hoặc vùng
  chọn; `pageScaleChange` cập nhật phần trăm zoom. Chế độ read-only vô hiệu hóa
  mọi control sửa nội dung nhưng vẫn cho phép zoom.
- Toolbar dùng biến màu chung cho theme sáng/tối, tự cuộn ngang ở WebView hẹp và
  có label/title/aria cho các control. Nhóm Insert, Table, Page Setup và
  Search/Replace được tách thành checklist các lát cắt tiếp theo của Pha 3.5.
- Web đạt typecheck, lint, production bundle và `8 passed; 0 failed`; baseline
  Rust có feature Canvas Editor đạt `1657 passed; 0 failed; 10 ignored` trong
  `265.68s`. `cargo check`, `cargo fmt --all -- --check` và `git diff --check`
  đều đạt; chỉ còn warning `GlyphStyle`/`f32` có sẵn ngoài lát cắt này.
- Bản test Release build thành công bằng
  `cargo build --release --features canvas-editor-webview --bin iai` trong
  `10m 42s`: `target/release/iai.exe`, `73,235,968` byte, SHA-256
  `DF6D94044B7F04E62FB3781C8B6806828F4F1C9D4A90CF88901D2CC428A7ECF6`.
- Chờ GUI-test toolbar ở theme sáng/tối, selection/caret, read-only, cửa sổ hẹp
  và hồi quy IME/Ctrl+W/Save trước khi đánh dấu lát cắt hoàn thành.

### 2026-09-10 — Khóa hồi quy focus Crop và Ctrl+W tab ảnh `[~]`

- Nguyên nhân focus: WebView được giữ sống để cache các tab văn bản và trước đây
  chỉ gọi `set_visible(false)` khi chuyển sang tab ảnh; Windows không bảo đảm tự
  trả keyboard focus từ child HWND ẩn về parent HWND. Host nay phát hiện đúng
  chuyển trạng thái visible → hidden do active tab không còn là FlowText, gọi
  `Window::focus_window` và Win32 `SetFocus` cho cửa sổ chính; không giành focus
  khi minimize/occluded.
- Nguyên nhân Ctrl+W: lớp lọc input trả sớm khi TextEdit hoặc `egui` đã consumed
  keyboard event, trong khi whitelist có nhiều Ctrl shortcut nhưng thiếu Ctrl+W.
  Ctrl+W nay là lifecycle shortcut được cho đi qua cả hai lớp và tiếp tục dùng
  `close_doc` chung cho tab ảnh/văn bản.
- Thêm hai regression test thuần cho ownership Ctrl+W và điều kiện trả focus.
  Baseline có feature Canvas Editor đạt `1659 passed; 0 failed; 10 ignored` trong
  `275.09s`; `cargo check` mặc định/có feature, `cargo fmt --all -- --check` và
  `git diff --check` đều đạt. Parser nhập kích thước/đơn vị Crop hiện hữu tiếp tục
  được baseline kiểm tra, không bị sửa ngoài nguyên nhân focus.
- Bản test Release build thành công bằng
  `cargo build --release --features canvas-editor-webview --bin iai` trong
  `11m 20s`: `target/release/iai.exe`, `73,236,480` byte, SHA-256
  `DADBF219614E78AB52B2F03179013EB050221D003684D53C29EA032F5D277660`.
- Tạm dừng tính năng văn bản mới cho đến khi chủ dự án xác nhận hai ca GUI: nhập
  Crop sau khi chuyển từ tab văn bản sang tab ảnh và Ctrl+W đóng tab ảnh.

### 2026-09-11 — Cách ly WebView khỏi tab ảnh sau GUI-test chưa đạt `[~]`

- GUI-test của chủ dự án xác nhận bản chỉ trả focus ở chuyển tiếp
  `visible -> hidden` vẫn chưa cho nhập W/H/DPI Crop. Trường hợp chuyển tab bất
  đồng bộ có thể làm WebView đã ẩn trước lúc host quan sát chuyển tiếp, nên nhánh
  trả focus cũ không chạy.
- Khi tài liệu hiện hành không phải FlowText, host nay hủy hẳn child WebView thay
  vì giữ một HWND ẩn để cache. Sau khi hủy, host gọi lại focus cho parent HWND nếu
  cửa sổ không thu nhỏ/occluded; khi quay lại tab văn bản, WebView được tạo lại từ
  snapshot đã commit trong core. Nhờ vậy tab ảnh không còn WebView con có thể giữ
  quyền nhận phím.
- Cổng Ctrl+W toàn cục cho mọi loại tài liệu vẫn được giữ nguyên; parser và thuật
  toán Crop không thay đổi.
- Regression test mới cho điều kiện trả focus khi giải phóng WebView đạt; toàn bộ
  baseline có feature đạt `1659 passed; 0 failed; 10 ignored` trong `51.01s`.
  `cargo check` mặc định/có feature, `cargo fmt --all -- --check` và
  `git diff --check` đều đạt (còn hai cảnh báo `f32` cũ trong `src/ui/library.rs`).
- File Release mặc định chưa thể ghi đè vì PID `45596` đang giữ
  `target/release/iai.exe`; không tự kết thúc tiến trình để tránh mất dữ liệu chưa
  lưu. Bản test độc lập build thành công bằng
  `cargo build --release --features canvas-editor-webview --bin iai --target-dir target\crop-input-fix`
  trong `7m 42s`: `target/crop-input-fix/release/iai.exe`, `73,235,456` byte,
  SHA-256 `32292844AAF7E5334A05C82ED75CEB9963B93465A121B39D4B1012D16DF9E862`.
- Tiếp tục tạm dừng tính năng văn bản mới cho đến khi chủ dự án GUI-test đạt nhập
  W/H/DPI Crop và Ctrl+W đóng tab ảnh bằng đúng bản Release độc lập trên.

### 2026-09-11 — Giữ giá trị W/H khi Tab rời ô Crop `[x]`

- GUI-test tiếp theo cho thấy bàn phím đã nhập được nhưng W/H trở về `0` khi nhấn
  Tab. Nguyên nhân không còn là focus: thanh Crop ghi “W × H × Resolution”, nhưng
  model vẫn ở `CropMode::Free`; action lưu số vào `fixed_w/fixed_h`, còn UiData ở
  khung hình kế tiếp lại đọc kích thước vùng chọn Free và trả `0` khi chưa có vùng
  chọn.
- Thêm API miền `set_typed_width`/`set_typed_height`: mọi W/H do người dùng nhập
  đều sở hữu kích thước đầu ra và chuyển Crop sang `FixedSize`. Action W/H nay gọi
  API này trước khi đồng bộ lại vùng chọn, nên mất focus hoặc Tab không thể đổi
  nguồn dữ liệu hiển thị.
- Regression test xác nhận nhập kích thước từ cả Free và Ratio đều chuyển sang
  FixedSize và giữ đúng W/H. Toàn bộ baseline đạt
  `1660 passed; 0 failed; 10 ignored` trong `101.13s`; `cargo check` mặc định/có
  feature, `cargo fmt --all -- --check` và `git diff --check` đều đạt.
- Lúc bàn giao, PID `46972` vẫn đang chạy file cũ
  `target/release/iai.exe`; chủ dự án phải lưu/đóng phiên đó và chạy đúng bản test
  độc lập. Release mới build thành công bằng
  `cargo build --release --features canvas-editor-webview --bin iai --target-dir target\crop-input-fix`
  trong `10m 35s`: `target/crop-input-fix/release/iai.exe`, `73,234,944` byte,
  SHA-256 `83A4C9F1B120DD4AC4E2FE59BD290A3DAF1300DCB720CEDFC891DAD75372C161`.
- Chủ dự án đã GUI-test đạt bằng bản Release trên: W → Tab → H → Tab → DPI giữ
  đúng số và Ctrl+W đóng tab ảnh. Cổng hồi quy được đóng; bước tiếp theo quay lại
  nghiệm thu toolbar văn bản cơ bản và Save/reopen trước khi làm nhóm Insert.

### 2026-09-11 — Nghiệm thu toolbar/Save và triển khai nhóm Insert `[~]`

- Chủ dự án xác nhận toolbar định dạng văn bản cơ bản và Save/Save As/reopen đã
  đạt GUI-test; checklist P2/P3/P3.5 liên quan được đóng. Cổng tiếp theo chuyển
  sang nhóm Insert đúng thứ tự kế hoạch.
- Bảng màu chữ không còn áp dụng trực tiếp: popover giữ màu nháp, có swatch và
  màu tùy chỉnh; chỉ nút OK mới gọi lệnh đổi màu, còn Cancel/click ra ngoài/Escape
  hoàn nguyên màu nháp và giữ nguyên nội dung.
- Bổ sung Insert ảnh, bảng, hyperlink, đường phân cách và ngắt trang. Ảnh được
  kiểm MIME/kích thước tối đa 5 MiB, giữ tỉ lệ và thu vừa khung trang; bảng giới
  hạn 1–20 hàng, 1–12 cột; hyperlink chỉ nhận HTTP/HTTPS/mailto và tự bổ sung
  HTTPS cho địa chỉ thiếu scheme. Dialog bảng/link có OK/Cancel và bị khóa trong
  read-only.
- Test web đạt typecheck, lint, production bundle và `11 passed; 0 failed`.
  Baseline Rust với feature Canvas Editor đạt `1660 passed; 0 failed; 10 ignored`
  trong `270.84s`; test offline asset ban đầu bắt URL trong placeholder hyperlink,
  sau khi bỏ chuỗi đó thì test riêng và baseline đầy đủ đều đạt. `cargo check`,
  `cargo fmt --all -- --check` và `git diff --check` đạt; còn cảnh báo
  `GlyphStyle`/`f32` cũ ngoài lát cắt này.
- Bản test Release build thành công bằng
  `cargo build --release --features canvas-editor-webview --bin iai --target-dir target\document-insert-test`
  trong `12m 21s`: `target/document-insert-test/release/iai.exe`, `73,247,232`
  byte, SHA-256
  `65BBB51E37C19B40B68837A84A32B04478F3988C92BAF792FA5026B56F330CA2`.
- Chờ GUI-test bảng màu OK/Cancel và năm thao tác Insert, sau đó Save/reopen file
  thực tế. Nếu đạt, tiếp tục nhóm Table theo ngữ cảnh.

### 2026-09-11 — Không dùng hộp chọn màu hệ thống che OK/Cancel `[~]`

- GUI-test phát hiện nút “Màu khác” mở hộp chọn màu riêng của WebView2 và hộp
  này che nút OK của popover bên dưới, khiến không thể xác nhận thao tác.
- Loại bỏ hoàn toàn `input type=color`. Màu tùy chỉnh nay được chọn bằng mã HEX
  và ba thanh R/G/B nằm ngay trong popover; preview và các giá trị kênh cập nhật
  đồng bộ. Thanh OK/Cancel được ghim ở đáy, còn popover có giới hạn chiều cao và
  tự cuộn ở cửa sổ thấp nên luôn có đường tới nút xác nhận.
- Web test/typecheck/lint/bundle đạt `12 passed; 0 failed`; test Rust bảo đảm
  asset offline đạt, `cargo check`, fmt và diff-check đều đạt. Baseline đầy đủ
  ngay trước lát cắt này vẫn là `1660 passed; 0 failed; 10 ignored`; thay đổi chỉ
  nằm trong UI/bundle WebView và có test RGB/HEX mới.
- Release mới build thành công bằng target test hiện hành trong `10m 14s`:
  `target/document-insert-test/release/iai.exe`, `73,247,232` byte, SHA-256
  `D02CA48A24684026D474A70007EDFBA358CE743136A2FD0314FE3CAD0098280F`.
- Chờ chủ dự án GUI-test lại Màu khác → chỉnh HEX/RGB → OK và Cancel trên đúng
  bản Release mới này.

### 2026-09-11 — Thay thanh RGB bằng bảng màu chọn nhanh `[~]`

- Theo phản hồi GUI-test, bỏ ba thanh kéo R/G/B để không bắt người dùng chỉnh
  từng kênh thủ công. Popover nay có bảng 60 ô: một hàng thang xám và năm mức
  sáng–đậm cho 10 nhóm sắc độ; bấm ô cập nhật màu nháp/preview ngay, OK mới áp
  dụng và Cancel vẫn hoàn nguyên. Ô HEX được giữ cho nhu cầu nhập mã chính xác.
- Thêm phép đổi HSL → HEX thuần dữ liệu và regression test các màu gốc đỏ/lục/
  lam. Web test/typecheck/lint/bundle đạt `12 passed; 0 failed`; test asset
  offline, `cargo check`, fmt và diff-check đều đạt.
- Release mới build thành công trong `10m 33s` tại
  `target/document-insert-test/release/iai.exe`, `73,247,232` byte, SHA-256
  `17B93F1458FB407E5DE1F32EE375A2879DB0160FFBE35AB163380B8AF7DAC48F`.
- Chờ chủ dự án GUI-test chọn nhanh một ô màu → OK và chọn màu khác → Cancel.

### 2026-09-12 — Hợp nhất portable và dọn artifact cũ `[x]`

- Hợp nhất Release mới vào `dist/iAi-portable/iai.exe`, đồng bộ model/license/
  extension và tài liệu runtime. Portable chỉ còn một executable chính, 61 tệp,
  `760.780.062` byte; SHA-256 executable là
  `17B93F1458FB407E5DE1F32EE375A2879DB0160FFBE35AB163380B8AF7DAC48F`.
- Xóa năm executable thử nghiệm portable cũ, gói Auto Retouch cũ, target Crop,
  cache Cargo debug/release/flycheck và log Q0/Q1; loại bỏ ít nhất 29,90 GiB dữ
  liệu sinh lại được/lỗi thời.
- Không xóa `target/document-insert-test` vì PID 8952 đang chạy từ đó; không xóa
  `.pnpm-store` vì có reparse point; giữ `tmp`, profile camera riêng và tài liệu
  kiến trúc do còn giá trị tái lập. Chi tiết tại
  `docs/cleanup-report-2026-09-12.md`.

### 2026-09-12 (tối) — Menu "Text" và Esc/Enter cho hộp thoại `[~]`

> Mục này được bổ sung hồi tố ngày 2026-09-15 từ log phiên Codex và diff worktree;
> phiên gốc không cập nhật kế hoạch.

- Yêu cầu của chủ dự án: đổi menu "Soạn thảo văn bản" thành `Text`; gán Esc cho
  cửa sổ Print và kiểm tra mọi hộp thoại còn thiếu Esc/Enter.
- Thêm helper `consume_dialog_enter_escape` trong `src/ui/dialogs.rs` (Enter bị bỏ
  qua khi egui đang nhận nhập liệu). Đã áp dụng cho Print, Preferences, Reload
  file, PDF import (Enter chỉ xác nhận khi có ít nhất một trang được chọn), Smart
  Fill, Gradient Editor, Feather/Modify/Stroke, xóa preset, các hộp thoại tài liệu
  và cửa sổ Develop (Enter = "Open Image" khi không gõ trong ô nhập).
- Bản build của phiên này là `cargo build --release` **không bật**
  `canvas-editor-webview`, ghi vào `target/release/iai.exe` (`72,108,544` byte,
  SHA-256 `902C1B97245E9EB04627B618BAFFC8118A2B9AB8F4181EE7D37629795F7352FF`). Vì
  vậy file này mở Document mode bằng editor `cosmic-text` cũ, không phải Canvas
  Editor; portable `dist/iAi-portable/iai.exe` (có Canvas Editor) lại chưa có thay
  đổi Esc/Enter.
- Chưa có kết quả GUI-test của chủ dự án cho lát cắt này.

### 2026-09-15 — Kiểm toán trạng thái và rà lỗi có kiểm chứng `[~]`

- Tổng hợp lại trạng thái từ git, kế hoạch và log phiên Codex 03/09–12/09. Worktree
  có ~33 file sửa cùng `src/app/document_webview.rs`,
  `src/core/canvas_editor_conversion.rs`, `web/document-editor/` — tất cả chưa
  commit. HEAD `5917523` đi trước `origin/feat/vector-core-foundation` 5 commit.
- Kiểm tự động trên worktree: `cargo test --lib --features canvas-editor-webview`
  → `1660 passed; 0 failed; 10 ignored` (`263.53s`); `cargo fmt --all -- --check`,
  `git diff --check`, `cargo check --bin iai` không feature đều đạt; web
  `tsc --noEmit`, `vitest run` (`12 passed`), `eslint` đạt.
- Release có feature build thành công trong `11m 28s`:
  `target/canvas-editor-test/release/iai.exe`, `73,242,624` byte, SHA-256
  `91BC31C3D6901FA14D1943C5B3CE6E30B30CF6A5D1CC2F5F6124A8AF840F8E6D`. Bản này chứa
  đủ Canvas Editor và Esc/Enter nhưng cũng chứa lỗi Pha 3.6.
- Rà soát 6 hướng: bridge/lifecycle, dữ liệu/định dạng, web editor, hộp thoại,
  build không feature, khoảng trống sản phẩm. Mỗi hướng có một tác tử phản biện đọc
  lại code/thư viện để bác bỏ. Sau khi gộp trùng còn 21 lỗi xác nhận, đưa vào Pha
  3.6 (A1–A6, B1–B6, C1–C5, D1–D4) cùng khuyến nghị D5.
- Bằng chứng chính: `src/core/document.rs:207` (revision nhận giá trị thấp hơn),
  `src/app/document_webview.rs:277-284, 362, 710, 1206, 1299, 1642-1668`,
  `src/app/file_ops/open.rs:525, 655`, `src/app/file_ops/save_export.rs:864, 906`,
  `src/formats/iai.rs:96`, `src/ui/dialogs/session.rs:244`,
  `src/tools/crop.rs:290` + `src/app/actions/ui_tools.rs:876`,
  `web/document-editor/src/toolbar.ts:133, 446`, `main.ts:85`, `index.html:14, 140`,
  `src/core/canvas_editor_conversion.rs:236`, `src/ui/panels.rs:1913`.
- Bị bác sau phản biện (không đưa vào kế hoạch): thiếu `sync_all` trước
  `MoveFileExW` (không phải hồi quy so với HEAD), Enter trong Develop khi dropdown mở
  (ComboBox egui không chọn bằng Enter), Enter trong hộp CMYK (hành động chính có
  cảnh báo), mất focus khi ẩn WebView (SW_HIDE trả focus về parent).
- Chưa sửa code trong phiên này; chờ chủ dự án chốt thứ tự và chính sách file cũ B3.
- Quyết định của chủ dự án cùng ngày: **tạm dừng toàn bộ phần soạn thảo văn bản**,
  quay lại khi rảnh (Pha 3.6 giữ nguyên làm điểm bắt đầu). **B3: giữ cách tự chuyển
  file legacy sang Canvas Editor như hiện tại** — khi làm lại chỉ xử lý sai lệch bố
  cục (giãn dòng, thụt lề, ảnh nổi), không thêm hỏi/giữ editor cũ cho file cũ.

### 2026-09-15 — Sửa preview hộp Print không cập nhật khi đổi máy in `[~]`

> Ngoài phạm vi Canvas Editor; ghi tại đây vì đi kèm D3 của Pha 3.6.

- Chủ dự án báo: preview trong hộp Print không cập nhật sau khi đổi máy in.
- Đã loại trừ bằng đo thực tế: `available_printers()` qua GDI trả đúng khổ riêng
  của cả 13 máy in trên máy chủ dự án (`0,64 s`), ảnh preview không phụ thuộc máy
  in, egui vẽ lại ngay sau cú click.
- Nguyên nhân: khổ giấy từng máy chỉ đọc một lần cho cả phiên và không đọc lại khi
  đổi máy in, nên preview giữ số cũ khi driver đổi mặc định ngoài iAi hoặc khi một
  máy từng được chỉnh bằng "Print Settings…" (khổ đã vá còn nằm trong danh sách dù
  cài đặt app-local đã bị xóa). Yêu cầu đọc lại đến lúc đang có lượt đọc khác bị
  bỏ qua âm thầm.
- Sửa: đổi máy in hoặc mở hộp Print gọi `refresh_selected_printer()`; yêu cầu đến
  lúc đang bận được ghi vào `BackgroundJobs::printer_refresh_queued` và chạy sau khi
  lượt trước xong. D3: khi danh sách máy in đang mở, Esc chỉ đóng danh sách, Enter
  không kích hoạt Print; đóng hộp thoại luôn xóa trạng thái danh sách mở.
- `cargo fmt --all`, `cargo check --bin iai` (không feature) và
  `cargo check --features canvas-editor-webview --bin iai` đạt;
  `cargo test --lib --features canvas-editor-webview` → `1660 passed; 0 failed;
  10 ignored`.
- Release có feature build thành công trong `5m 55s`, ghi đè bản kiểm toán cùng
  thư mục: `target/canvas-editor-test/release/iai.exe`, `73,238,016` byte, SHA-256
  `E7526A641CF35B246DBF98F0900201BD8D071A9E1D73373E8C7C6B140AFE1686`. Chờ chủ dự
  án GUI-test: đổi qua lại vài máy in (ví dụ EPSON L8050 (A4) 10×15 cm → EPSON
  L18050 (A3) 13×18 cm → Microsoft Print to PDF A4), khung giấy và dòng "Paper"
  phải đổi theo; Esc khi đang mở danh sách máy in chỉ đóng danh sách.
