# Kế hoạch: xử lý 16 mục QC nền tảng (07/10/2026)

**Trạng thái (07/10 chiều): kế hoạch mới lập, CHƯA làm mục nào. Chủ đọc xong thì hội thoại mới
bắt đầu từ Đợt QC-1.** Phiên lập kế hoạch này không sửa một dòng code nào.

## 0. Hội thoại mới đọc gì trước

1. `AGENTS.md` ở thư mục gốc (build sẵn bản chạy được trước khi mời chủ test; đưa đường dẫn
   thật; chủ báo sự cố thì đọc hộp đen trước).
2. File này — mục 2 (bảng các đợt), mục 3 (câu hỏi chờ chủ) rồi tới đợt đang làm.
3. `git status` + `git log -5` để đối chiếu, vì chủ có thể đã giao việc khác xen giữa.

Quy ước giữ nguyên như mọi đợt trước:

- Mỗi đợt xong: `cargo fmt --check` + `cargo test --lib` → commit local → build Release
  `cargo build --release --features canvas-editor-webview` → mời chủ test bằng đường dẫn
  `C:\Users\Admin\Documents\IAI\target\release\iai.exe`. Chỉ push khi chủ bảo.
- Không gọi nhiều agent.
- Hồi quy phần cũ thì dừng việc mới, sửa hồi quy trước.
- Đợt 41 của ảnh thẻ ("Sống mũi cao" 30, Select Subject giữ model) vẫn đang chờ chủ test —
  kế hoạch này không đụng tới các file đó trừ khi ghi rõ.

Số dòng trong file này đo tại commit `526a402`. Code sẽ trôi, nên trước khi sửa chỗ nào thì
grep lại tên hàm, đừng tin số dòng.

## 1. Việc chủ giao

Lời chủ 07/10: "bạn là QC hãy kiểm tra 1 lượt toàn bộ code, toàn bộ kiến trúc nền tảng của app
iai xem có chỗ nào cần fix hay refactor không, chỉ kiểm tra xong báo cáo, ko sửa code; ko dc
gọi nhiều agent; cho tôi biết độ lớn của app luôn".

Lời chủ 07/10, sau khi đọc báo cáo: "ok; phiên này hãy lập 1 kế hoạch để xử lý các vấn đề
trên; sau đó push lên github và chúng ta sẽ chuyển qua cuộc trò chuyện mới đọc kế hoạch và
triển khai".

Phạm vi của lần QC: đọc tĩnh. Đọc kỹ khoảng 15 file nền (khởi động, lưu file, tự lưu, cầu nối
extension, việc nền, vòng lặp khung hình) và quét mẫu toàn bộ 348 file; không đọc từng dòng
của cả 290 nghìn dòng. Vì vậy mỗi đợt dưới đây đều mở đầu bằng một bước "xác minh lại".

Độ lớn app lúc QC: 348 file Rust, khoảng 290.000 dòng (190.000 dòng chạy thật, 100.000 dòng
kiểm thử, 2.200 bài test); `iai.exe` 74,3 MB; model AI 693 MB; 623 commit từ 18/07/2026.

## 2. Các đợt, theo thứ tự làm

Nhóm A sửa lỗi thật, mỗi đợt có phần chủ test. Nhóm B là dọn và tách code, không đổi cách
app chạy, làm xen kẽ khi rảnh.

| Đợt | Mục QC | Làm gì | Chủ thấy gì khác | Cỡ việc |
|---|---|---|---|---|
| **QC-1** | 1 | Mọi file lưu / xuất đều ghi ra file tạm rồi mới đổi tên | Không thấy gì; ảnh gốc không hỏng khi mất điện giữa lúc lưu | Nhỏ |
| **QC-2** | 5b, 6 | Lưu lỗi thì hiện hộp báo; vá ba lỗ hổng tự lưu | Hộp báo khi lưu hỏng; dòng nhắc khi ảnh quá lớn không được tự lưu | Nhỏ – vừa |
| **QC-3** | 4 | Cầu nối extension: không cho hai cửa sổ giành cổng, có hạn giờ, chặn trang web lạ | Mở hai cửa sổ iAi thì cửa sổ sau báo rõ; extension hết "lúc được lúc không" | Vừa |
| **QC-4** | 7a, 14 | Một chỗ duy nhất đọc / ghi `prefs.json`, có bản dự phòng | Không thấy gì; cài đặt không tự về mặc định khi file hỏng | Vừa |
| **QC-5** | 3 | Kiểm tự động trên GitHub chạy cho đúng nhánh và đúng bản có Canvas Editor | Không thấy gì trong app | Nhỏ |
| **QC-6** | 5a | Lưu / Export chạy ở luồng nền | Lưu ảnh lớn không còn khựng cửa sổ | Lớn |
| **QC-7** | 8 (+7b nếu chủ muốn) | Kiểm mã băm model tải về | Không thấy gì; file tải lỗi bị từ chối thay vì chạy sai | Nhỏ |
| QC-8 | 13, 16 | Gỡ code chết, dọn cảnh báo, dọn kho code | Không | Vừa, làm rải |
| QC-9 | 10 | Khung chung cho việc chạy nền | Không | Lớn, làm rải |
| QC-10 | 11 | Một lối duy nhất để lấy "tài liệu đang mở" | Không | Vừa, làm rải |
| QC-11 | 9 | Tách các hàm trên 1.000 dòng | Không | Lớn, làm rải |
| QC-12 | 12, 15 | Trạng thái giao diện, thư viện ngoài | Không | Chỉ làm khi tiện |

Mục 2 của báo cáo (122 commit chưa đẩy lên GitHub) đã xử lý ngay trong phiên lập kế hoạch
này bằng lần push 07/10. Đề nghị cho về sau: push sau mỗi đợt chủ test OK. Chủ quyết.

## 3. Câu hỏi chờ chủ

Mỗi câu có sẵn cách làm mặc định, nên chủ không trả lời thì các đợt vẫn chạy được.

| # | Câu hỏi | Mặc định nếu chủ không nói gì | Ảnh hưởng đợt |
|---|---|---|---|
| H1 | Cho kiểm tự động trên GitHub chạy mỗi lần push nhánh đang làm? Lỗi thì GitHub gửi email cho chủ. | Có, vì kho code đã công khai nên không tốn phút trả tiền | QC-5 |
| H2 | Có kéo nhánh `main` lên bằng nhánh đang làm không? `main` đứng yên từ 05/09. | Chưa, để nguyên tới khi chủ bảo | QC-5 |
| H3 | Ảnh trên 100 triệu điểm ảnh: chỉ hiện dòng nhắc "ảnh này không được tự lưu", hay nâng ngưỡng theo RAM của máy? | Chỉ hiện dòng nhắc | QC-2 |
| H4 | Khóa API AI: mã hóa theo tài khoản Windows? Đổi lại là chép `ai.json` sang máy khác phải nhập lại khóa. | Không làm | QC-7 |
| H5 | `logo.iai` (2,8 MB), `README_TEST.txt`, `iAi_Offline_AI_Retouch_Plan_for_Codex.txt` ở thư mục gốc: xóa, hay dời vào `docs/`? | Dời vào `docs/`, không xóa | QC-8 |
| H6 | Số phiên bản vẫn là 0.1.0. Có muốn đánh số theo từng bản giao không? | Để nguyên | QC-8 |

## 4. Nhóm A — sửa lỗi

### Đợt QC-1 — ghi file an toàn (mục 1)

**Lỗi.** File `.iai` được ghi ra file tạm rồi đổi tên (`write_iai_archive`,
`src/formats/iai.rs`). Mọi định dạng khác ghi thẳng lên file đích:

- `src/formats/png.rs:48` và `src/formats/tiff.rs:165`: `File::create(path)` xóa trắng file
  cũ ngay, rồi mới mã hóa dần.
- `src/formats/jpeg.rs:82`, `src/formats/pdf.rs` (ba chỗ `fs::write`), `src/formats/webp.rs:35`.
- `src/file_io.rs` `save` (`image::save_buffer`).
- `src/app/file_ops/save_export.rs`: SVG (dòng 666), PDF (1047, 1122, 1202, 1272).
- `src/app/actions/ui_color_print.rs:459`: PDF.

Ctrl+S trên ảnh JPG / PNG một lớp đi qua `save_to` → `format_registry.export(canvas, path)`,
tức là ghi đè thẳng ảnh gốc của khách. Mất điện, đầy đĩa hoặc lỗi mã hóa giữa chừng là mất
ảnh gốc. `.iai` thì thiếu bước ép dữ liệu xuống đĩa trước khi đổi tên.

**Cách sửa.**

1. Thêm `src/core/atomic_file.rs` (lớp `core`, để `formats`, `app` và `core::settings` cùng
   dùng được):
   - Dời `replace_file_atomically` từ `formats/iai.rs` sang đây.
   - `write_atomic(dest, |tmp: &Path| -> Result<(), String>)`: tạo đường dẫn tạm **cùng thư
     mục và giữ nguyên đuôi file** (ví dụ `anh.~iai1234.jpg`), chạy thân hàm, mở lại file tạm
     để `sync_all`, đổi tên đè lên đích. Thân hàm lỗi hoặc đổi tên lỗi thì xóa file tạm và
     trả lỗi; file đích không bị đụng tới.
   - `write_bytes_atomic(dest, &[u8])` cho các chỗ đang `fs::write`.
   - Phải giữ đuôi file vì `webp.rs` và `file_io::save` để thư viện `image` đoán định dạng
     theo đuôi.
2. `FormatRegistry::export` (`src/formats/mod.rs`): chọn exporter theo đường dẫn đích như
   cũ, nhưng với mọi đuôi khác `.iai` thì gọi `exporter.export(target, tmp, opts)` bên trong
   `write_atomic`. Một chỗ sửa này phủ PNG / JPEG / WebP / TIFF / PDF.
3. `write_iai_archive`: `zip.finish()` trả lại `File` → gọi `sync_all()` trước khi đổi tên.
4. Thay từng `fs::write` liệt kê ở trên bằng `write_bytes_atomic`; `file_io::save` bọc trong
   `write_atomic`.
5. Đổi tên thất bại vì file đích đang bị chương trình khác giữ thì báo lỗi rõ. **Không** lùi
   về ghi thẳng.

**Xác minh lại trước khi sửa.** Grep `fs::write(` và `File::create(` trong `src/formats`,
`src/app`, `src/file_io.rs`; có thể còn chỗ ghi file của người dùng chưa liệt kê (xuất tách
bản CMYK, mail merge xuất hàng loạt).

**Test tự động.**

- `write_atomic`: thành công thì đích có nội dung mới và không còn file tạm; thân hàm trả lỗi
  thì đích giữ nguyên từng byte và không còn file tạm; tên tạm giữ đúng đuôi.
- `FormatRegistry::export` lên một file đã có sẵn, với exporter giả luôn trả lỗi: file cũ còn
  nguyên.
- Các test ICC / 16-bit round-trip hiện có trong `formats/mod.rs` phải qua không sửa.

**Chủ test.** Mở JPG → sửa → Ctrl+S; Export PNG / PDF đè lên file đã có; lưu `.iai`. Mở cùng
file đó trong một chương trình khác rồi lưu đè: phải hiện lỗi, file cũ còn nguyên.

**Xong khi.** Không còn chỗ nào trong đường lưu / xuất ghi thẳng lên file của người dùng.

### Đợt QC-2 — báo lỗi lưu rõ ràng, vá tự lưu (mục 5b, 6)

**Lỗi.**

- Lưu hỏng chỉ ghi `"Error: …"` vào dòng trạng thái (`save_to`, `do_export`,
  `src/app/file_ops/save_export.rs`). Người dùng dễ tưởng đã lưu.
- `src/app/autosave.rs`:
  - `MAX_BACKGROUND_AUTOSAVE_PIXELS` = 100 triệu: ảnh lớn hơn bị bỏ qua, không báo.
  - `autosave_active_project`: tài liệu nhiều trang (PDF, artboard) chỉ được chép khi đang là
    tab hiện hành, và `write_autosave` ghi ngay trên luồng giao diện.
  - `poll_autosave_job` và `write_autosave` nuốt lỗi ghi (`Err(_) => {}`).

**Cách sửa.**

1. Hộp báo lỗi lưu: thêm một trường kiểu `save_error: Option<String>` vào `UiState`, vẽ như
   hộp `document_editor_error` đang có (một nút "Đã hiểu"). Mọi nhánh `Err` của lưu / xuất
   đặt trường này, vẫn giữ dòng trạng thái.
2. Lỗi tự lưu: ghi vào hộp đen bằng `crate::diag::note("autosave", …)`. Hỏng ba lần liên
   tiếp thì hiện một dòng trạng thái "Tự lưu đang lỗi: …", mỗi phiên một lần.
3. Ảnh quá ngưỡng: mỗi tài liệu hiện một lần dòng "Ảnh này quá lớn nên không được tự lưu —
   nhớ Ctrl+S" và ghi hộp đen (theo H3).
4. Tài liệu nhiều trang: duyệt mọi tab giống `start_next_image_autosave`, tab hiện hành
   trước, thay vì chỉ tab hiện hành.
5. Đưa `write_autosave` sang luồng nền **chỉ khi** đo thấy chụp nhanh các trang là rẻ (tile
   dùng chung kiểu copy-on-write như `export_snapshot`). Đo bằng dòng `perf` trong hộp đen
   trước và sau; không rẻ thì ghi lại số đo và để nguyên.

**Test tự động.** Đường dẫn tự lưu không ghi được → có ghi hộp đen, file tự lưu tốt trước đó
còn nguyên. Tài liệu nhiều trang ở tab nền có sửa → được chép. Các test đang có trong
`autosave.rs` qua không sửa.

**Chủ test.** Rút USB giữa lúc lưu ra USB (hoặc lưu vào thư mục chỉ đọc) → có hộp báo. Còn
lại không đổi gì nhìn thấy được.

### Đợt QC-3 — cầu nối extension (mục 4)

**Lỗi** (`src/app/ext_bridge.rs`).

- `bind_bridge_listener` bật `SO_REUSEADDR`. Trên Windows, cờ này cho phép hai tiến trình
  cùng mở cổng 47821. App cho mở nhiều cửa sổ (xem đầu `autosave.rs`), nên mở hai cửa sổ iAi
  — hoặc còn một tiến trình test kẹt — thì extension nối vào bên nào không đoán được, và
  không bên nào báo lỗi.
- `server_loop`: mỗi lúc một kết nối; `tungstenite::accept` chờ bắt tay không hạn giờ; sau
  bắt tay không có hạn chờ `hello`; không xem tiêu đề `Origin`. Một trang web bất kỳ mở
  `ws://127.0.0.1:47821` rồi im lặng sẽ chặn extension thật. Nó không lấy được ảnh vì thiếu
  mã bí mật.
- `gen_token` trộn giờ hệ thống, số tiến trình và một địa chỉ bộ nhớ, không phải số ngẫu
  nhiên của hệ điều hành.
- Test `server_completes_websocket_handshake` dùng đúng cổng thật 47821.

**Cách sửa.**

1. Mở cổng: chỉ bật `SO_REUSEADDR` khi `cfg(not(windows))`. Trên Windows bật
   `SO_EXCLUSIVEADDRUSE` (xem `socket2` 0.6 có hàm sẵn không; không có thì `setsockopt` qua
   `windows-sys`, thêm feature `Win32_Networking_WinSock`).
2. Mở cổng thất bại: báo lên bảng AI "Cổng 47821 đang do cửa sổ iAi khác giữ — extension chỉ
   nối với cửa sổ mở trước", và luồng máy chủ thử lại mỗi vài giây để cửa sổ còn lại tự nhận
   cổng khi cửa sổ kia đóng.
3. Hạn giờ: đặt hạn đọc khoảng 5 giây trước `accept`; quá hạn thì bỏ kết nối. Sau bắt tay,
   5 giây không có `hello` đúng thì đóng. Chú thích hiện có nói hạn đọc ngắn từng làm hỏng
   bắt tay — phải thử với extension thật.
4. `Origin`: dùng `tungstenite::accept_hdr`. Từ chối origin bắt đầu bằng `http://` hoặc
   `https://`; cho qua `chrome-extension://…` và trường hợp không có `Origin`. WebSocket được
   mở từ `extension/background.js` (service worker), nên origin thật là `chrome-extension://`
   — ghi origin nhận được vào hộp đen một lần để kiểm lại trên Chrome, Edge, Brave.
5. Tách `server_loop` để nhận sẵn một `TcpListener`. Test dùng cổng 0 (hệ điều hành tự cấp),
   bỏ hẳn việc test đụng cổng thật.
6. Mã bí mật: cài đặt mới thì sinh bằng nguồn ngẫu nhiên của hệ điều hành. **Giữ nguyên** file
   `ext_token.txt` đang có, để chủ không phải dán lại mã vào extension.

**Test tự động.** Hàm xét `Origin` (cho / chặn). Hai listener cùng cổng: cái thứ hai phải
lỗi. Kết nối mở rồi im lặng bị đóng sau hạn giờ, kết nối kế tiếp vẫn vào được.

**Chủ test.** Sửa ảnh qua Gemini và ChatGPT một vòng đầy đủ. Mở hai cửa sổ iAi: cửa sổ sau
hiện dòng báo; đóng cửa sổ đầu thì cửa sổ sau nối được sau vài giây.

### Đợt QC-4 — `prefs.json` một cửa (mục 7a, 14)

**Lỗi.** Sáu chỗ tự đọc–sửa–ghi cùng một file `prefs.json`, ghi thẳng và bỏ qua lỗi:
`src/core/settings.rs` (`save`), `src/ui/theme.rs:228`, `src/ui/dialogs.rs:79 / 107 / 148`,
`src/ui/develop.rs:94`. File hỏng thì `AppSettings::load` lặng lẽ trả mặc định: phím tắt, đơn
vị, chu kỳ tự lưu về như mới cài. `core/settings.rs` còn gọi ngược lên
`crate::ui::theme::prefs_path`, trái sơ đồ phân lớp ghi ở đầu `src/lib.rs`.

**Cách sửa.**

1. Thêm `src/core/prefs_store.rs`:
   - `prefs_path()` dời về đây; `ui::theme` gọi xuống.
   - `read() -> serde_json::Map`.
   - `update(|map| …)`: khóa trong tiến trình, đọc lại file, áp thay đổi, ghi bằng
     `write_bytes_atomic` của QC-1. Trước khi ghi đè, chép bản đang đọc được ra
     `prefs.json.bak`.
   - Đọc mà file không phân tích được: đổi tên file hỏng thành `prefs.corrupt-<giờ>.json`,
     nạp `prefs.json.bak` nếu bản đó đọc được, ghi hộp đen, hiện một dòng trạng thái "File
     cài đặt bị hỏng, đã khôi phục bản gần nhất".
2. Chuyển cả sáu chỗ sang `update`. Không đổi tên khóa, không đổi hình dạng dữ liệu.
3. Tiện tay dùng `write_bytes_atomic` cho các file cấu hình khác: `src/core/presets.rs` (ba
   chỗ), `src/app/library.rs:70` (danh mục file gần đây), `src/core/ai/settings.rs:56`.

**Test tự động.** Hai lần `update` trên hai khóa khác nhau: giữ đủ cả hai. File hỏng: nạp từ
`.bak`, file hỏng được đổi tên. Không có file: trả mặc định, không tạo rác. Các test
`AppSettings` đang có qua không sửa.

**Chủ test.** Đổi giao diện sáng / tối, một phím tắt, đóng mở các mục của bảng Develop → tắt
mở lại app → còn nguyên.

### Đợt QC-5 — kiểm tự động đúng bản đang giao (mục 3)

**Lỗi** (`.github/workflows/ci.yml`). Chỉ chạy khi push `main` hoặc có PR; việc hằng ngày
nằm trên `feat/vector-core-foundation`, nên phần lớn commit không qua kiểm. Các bước build /
test không có `--features canvas-editor-webview`, nên `src/app/document_webview.rs` (2.380
dòng) chưa từng được biên dịch ở đó, trong khi bản giao cho chủ luôn build có feature này.

**Cách sửa** (theo H1).

1. Thêm nhánh đang làm vào `on.push.branches`. Thêm `paths-ignore` cho `docs/**` và `**.md`
   để push chỉ có tài liệu không chạy kiểm. Thêm `concurrency` để lần push mới hủy lần đang
   chạy dở.
2. Job Windows: thêm `cargo build --locked --features canvas-editor-webview` và
   `cargo test --locked --lib --features canvas-editor-webview`. Giữ một bước `cargo check
   --locked` không feature để chắc bản không feature vẫn biên dịch.
3. Trước khi bật: chạy tại máy `cargo fmt --check`, `cargo clippy --locked --all-targets` và
   `cargo test --locked` cho xanh, tránh email đỏ ngay lần đầu.
4. Kiểm lại ở trang Billing của GitHub rằng kho công khai không bị tính phút; nếu có tính thì
   dừng và hỏi chủ.

**Xong khi.** Một lần push nhánh đang làm cho ra dấu xanh trên GitHub, có cả bước build kèm
feature.

### Đợt QC-6 — lưu và Export không khựng (mục 5a)

Đợt rủi ro nhất của nhóm A vì đụng đường lưu. Chỉ làm sau QC-1 và QC-2.

**Lỗi.** `save_to` và `do_export` mã hóa file ngay trên luồng giao diện. Ảnh lớn nhiều lớp
thì cửa sổ đứng trong lúc lưu. Tự lưu ảnh đơn đã chạy nền từ trước (`spawn_image_autosave`),
lưu tay thì chưa.

**Cách sửa — bước đầu chỉ cho ảnh đơn** (`.iai` một canvas và các định dạng ảnh qua
`FormatRegistry`):

1. Chụp nhanh canvas bằng `export_snapshot()` như tự lưu, kèm dấu nội dung
   `content_fingerprint` (đang là hàm riêng trong `autosave.rs` — dời ra chỗ dùng chung).
2. Luồng nền ghi file bằng đường ghi an toàn của QC-1.
3. Thêm `SaveJob { doc_id, path, fingerprint, done }` vào `DocumentSession`, một việc mỗi
   lúc. Kết quả về:
   - Thành công: cập nhật `path`, `file_modified_at`, xóa file tự lưu. **Chỉ đánh dấu "đã
     lưu" khi dấu nội dung hiện tại bằng dấu lúc chụp**; người dùng đã vẽ thêm trong lúc lưu
     thì tài liệu vẫn tính là chưa lưu.
   - Thất bại: hộp báo của QC-2.
4. Trong lúc đang lưu: dòng trạng thái "Đang lưu…"; Ctrl+S lần nữa trên cùng tài liệu thì bỏ
   qua; đóng tab hoặc thoát app thì chờ việc lưu xong (như `EXIT_WAIT_FOR_AUTOSAVE`).
5. Tài liệu nhiều trang, dự án PDF và văn bản Canvas Editor giữ đường lưu cũ ở bước này.

**Đo.** Lấy thời gian khựng ở dòng `perf` của hộp đen khi lưu một ảnh A3 300 dpi nhiều lớp,
trước và sau.

**Test tự động.** Lưu nền rồi sửa tiếp trước khi xong → tài liệu vẫn "chưa lưu". Lưu nền lỗi
→ file đích cũ còn nguyên, tài liệu vẫn "chưa lưu". Đóng tab giữa lúc lưu → file ra đủ.

**Chủ test.** Lưu ảnh lớn: cửa sổ vẫn kéo, vẫn zoom được. Lưu xong đóng ngay: không bị hỏi
lại. Lưu rồi vẽ tiếp ngay: lúc đóng vẫn bị hỏi lưu.

**Ngoài phạm vi.** Chỗ khựng khi crop đổi cỡ (đã điều tra 05/10, chưa sửa) là việc riêng,
vẫn chờ chủ bảo.

### Đợt QC-7 — kiểm mã băm model tải về (mục 8)

**Lỗi.** `src/core/select_subject.rs` (`start_download`) và `src/core/lama.rs` tải file ONNX
rồi chỉ so độ dài. Bộ Auto Retouch (`src/core/ai/retouch.rs`) đã có kiểm SHA-256.

**Cách sửa.**

1. Thêm `sha256` vào mô tả model của hai chỗ trên; kiểm file `.part` trước khi đổi tên. Sai
   thì xóa và báo "File tải về không đúng — thử lại". Dùng lại hàm băm của `retouch.rs`.
2. Giá trị mã băm lấy từ trang file của nguồn tải (Hugging Face ghi SHA256 cho từng file) và
   **đối chiếu với file đang chạy tốt trên máy chủ**. Hai bên không khớp thì dừng lại hỏi,
   không tự chọn.
3. Model đã cài sẵn: không băm lại mỗi lần mở app.

Khóa API (H4): mặc định không làm.

## 5. Nhóm B — dọn và tách code

Luật chung của nhóm B: **không đổi cách app chạy**. Mỗi commit một việc nhỏ, chuyển code
nguyên văn. Trước và sau đều chạy `cargo test --lib`. Phần giao diện thì chụp ảnh probe
(`src/ui/snapshot.rs`) trước và sau rồi so. Có gì lệch thì bỏ commit đó.

### QC-8 — gỡ code chết, dọn cảnh báo, dọn kho code (mục 13, 16)

- Xóa `src/event_bus.rs` và trường `bus` trong `BackgroundJobs`: được tạo ra nhưng không chỗ
  nào dùng.
- Gỡ `#![allow(dead_code)]` từng module một (khoảng 40 module, gồm cả `src/tools/` và
  `src/core/vector/`): biên dịch, xóa thứ thật sự không dùng; thứ cố ý giữ thì gắn
  `#[allow(dead_code)]` riêng kèm một dòng lý do.
- Clippy (khoảng 300 cảnh báo): sửa theo từng loại cảnh báo, mỗi loại một commit. Hết thì đổi
  job clippy trong `ci.yml` sang chặn (`-- -D warnings`).
- Kho code (theo H5): dời `README_TEST.txt`, `iAi_Offline_AI_Retouch_Plan_for_Codex.txt` và
  `logo.iai` vào `docs/`. **Giữ** `web/document-editor/dist/editor.js` trong git: app nhúng
  nó bằng `include_bytes!` (`src/app/document_webview.rs:27`) để build không cần Node. Báo
  cáo QC ghi nhầm chỗ này là nên bỏ.
- 100 bài test đang `#[ignore]`: rà lý do từng bài; bài nào chỉ vì nặng thì giữ, bài nào vì
  hỏng thì sửa hoặc xóa.

### QC-9 — khung chung cho việc chạy nền (mục 10)

Hiện có khoảng 35 trường `pending_*` trong `BackgroundJobs`, 40 hàm `poll_*` gọi tay trong
`src/app/input/redraw.rs`, 60 chỗ `thread::spawn`. Thêm một việc nền là sửa ba bốn chỗ; quên
một chỗ thì kết quả treo.

1. Thêm `src/app/jobs.rs`: `Job<T>` gồm kênh nhận, `Option<DocumentId>`, cờ hủy, giờ bắt
   đầu, nhãn. `spawn_job(nhãn, doc, f)` đặt tên luồng, bọc `catch_unwind` để luồng nền sập
   thành `Err` chứ không im lặng, và đánh thức vòng lặp sự kiện khi xong. `poll()` trả
   `Pending` / `Done(T)` / `Failed(String)`.
2. Chuyển từng việc một, mỗi việc một commit, từ dễ tới khó: làm mới danh sách máy in → chọn
   ảnh cho văn bản → nạp lại từ đĩa → mở file → PDF → AI.
3. Khi chuyển "nạp lại từ đĩa" (`confirm_reload_open_file`, `src/app/file_ops/open.rs`): đổi
   từ số thứ tự tab sang `DocumentId`, đúng quy ước ghi ở đầu `background_jobs.rs`. Hiện có
   chốt so đường dẫn nên không áp nhầm tab, chỉ bị bỏ qua khi tab dịch chỗ.
4. `doc_id: u32` trong `RepairAiJob` và `ext_bridge` đổi sang `DocumentId` cho đồng nhất.

### QC-10 — một lối lấy "tài liệu đang mở" (mục 11)

- Thêm `App::active_doc()`, `active_doc_mut()`, `doc_by_id()`. Thay
  `self.docs.documents[self.docs.active_doc_idx]` (1.033 chỗ) theo từng file, mỗi file một
  commit.
- `docs.current_file` trùng với `doc.path` của tab hiện hành, phải giữ đồng bộ tay ở 36 chỗ.
  Trước khi bỏ: thêm `debug_assert_eq!` ở đầu mỗi khung hình, chạy đủ bộ test và để chủ dùng
  thử một đợt. Không lần nào lệch thì thay trường bằng một hàm lấy từ tab hiện hành. Có lệch
  thì ghi lại trường hợp đó và giữ nguyên.

### QC-11 — tách hàm trên 1.000 dòng (mục 9)

Thứ tự theo mức hay phải sửa và mức rủi ro:

1. `ui/menubar.rs` `build` (1.683 dòng) → mỗi menu một hàm.
2. `app/actions/ui_data.rs` `collect_ui_data` (1.600) → mỗi bảng một hàm dựng dữ liệu.
3. Xử lý phím (`app/input/keyboard.rs`, 1.037) → đi qua bảng `commands::Command` / `KeyMap`
   đã có.
4. Xử lý chuột (`app/input/pointer.rs`, 1.068) → mỗi công cụ / chế độ một hàm.
5. `ui/document_mode.rs` `window_ui` (1.035).
6. `gpu/compositor.rs` `composite_layers` (1.205) → làm cuối; dựa vào các test so CPU / GPU
   đang có.

Hơn 45 hàm trên 250 dòng còn lại: tách khi có việc đụng tới, không mở đợt riêng.

### QC-12 — trạng thái giao diện và thư viện (mục 12, 15)

- `UiState` (khoảng 150 trường): không làm một lần. Mỗi khi sửa một hộp thoại thì gom các
  trường của nó vào một struct riêng.
- `ort` đang ở `2.0.0-rc.12`: khi có bản 2.0 chính thức thì nâng trong một đợt riêng, thử lại
  đủ 9 model Auto Retouch, Select Subject, Repair và đường lùi về CPU khi GPU lỗi.
- `wry` ghim `=0.56.1`: để nguyên tới khi quay lại việc Canvas Editor.
- Hai bộ giải mã RAW (`rawloader`, `rawler`): cố ý, `rawler` lo CR3 và máy ảnh mới. Không
  làm gì.

## 6. Việc không làm trong kế hoạch này

- Không thêm tính năng mới.
- Không đổi định dạng `.iai` hay tên khóa trong `prefs.json`.
- Không sửa chỗ khựng khi crop đổi cỡ (chờ chủ bảo).
- Không đề xuất lại các việc chủ đã chốt bỏ: in hàng loạt, kéo layer qua trang, áp công thức
  chân dung hàng loạt.

## 7. Nhật ký

- 07/10 chiều: lập kế hoạch. `cargo fmt --check` sạch. Chưa làm đợt nào.
