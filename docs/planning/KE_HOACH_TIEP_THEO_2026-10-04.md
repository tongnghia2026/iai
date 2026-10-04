# Kế hoạch tiếp theo sau các đợt ảnh thẻ / chân dung (04/10/2026)

Bàn giao cho phiên làm việc mới. Lịch sử chi tiết các đợt 1–15 nằm ở
`docs/planning/KE_HOACH_ANH_THE_2026-10-02.md`.

## 0. Trạng thái khi bàn giao

- Nhánh `feat/vector-core-foundation`; mọi thứ **commit local, chưa push** (push gần nhất
  02/10 @ `d1f4acb`). Chỉ push khi chủ bảo.
- Bản test: `target\release\iai.exe` (build có `--features canvas-editor-webview`).
- Bản portable `dist\iAi-portable` vẫn là bản 02/10 — thiếu mọi thứ làm từ 03/10.
- Chủ đã test OK: đợt 8–14 (Chi tiết mặt AI, Sửa màu & sáng, mặc định mới, thanh màu hai
  chiều, Sáng da = Midtones, Xếp ảnh in + trang 13×18 hỗn hợp, viền cắt 2 px).
- **Chờ chủ test — đợt 15 (04/10):** hết vạch mờ ở mép ảnh thẻ; "Trắng mắt" / "Trắng răng"
  không ngả xanh trên ảnh ám màu.

Cách chia việc chủ đã chốt: ảnh quá khó → AI Image Studio (Gemini / ChatGPT); ảnh chụp đẹp
hoặc không có mạng → Làm ảnh thẻ + Chỉnh chân dung + Xếp ảnh in.

## 1. Việc đề nghị làm tiếp (theo thứ tự)

### 1.1 Tấm 4×6 nét hơn — cần chủ đồng ý trước khi làm

Hiện "Làm ảnh thẻ" cắt ảnh về 661×898 px (3×4 @ 600 ppi). Tấm 4×6 trên trang in được phóng
1,58 lần từ ảnh đó (≈ 380 ppi) rồi cắt bớt ~5% mỗi bên → kém nét hơn tấm 3×4.

Cách đề nghị: cho "Làm ảnh thẻ" cắt ở **1043×1417 px**, vẫn là 2,8×3,8 cm (≈ 946 ppi).

- Tấm 4×6 lấy thẳng điểm ảnh (cắt giữa 1043 → 945, không phóng).
- Tấm 3×4 thu nhỏ 1043 → 661 khi xếp trang (nét hơn hiện tại).
- Khung hình, tỉ lệ mặt giữ nguyên như chủ đã duyệt.
- Phải đổi: `output_size()` / `PRINT_PPI` trong `src/core/id_photo.rs`, dòng chữ trong hộp
  thoại ("600 ppi · 661×898 px"), các test đang so 661×898. `PhotoKind::detect` nhận cỡ theo
  cm nên không phải sửa.
- **Điểm phải hỏi chủ:** "661×898 @ 600 ppi" là cỡ chủ đã chốt ngày 02/10 (trùng preset Crop
  "Ảnh thẻ 3×4"). Đổi cỡ tệp thì ảnh thẻ lưu riêng sẽ là 1043×1417. Ảnh gốc nhỏ (mặt dưới
  ~900 px) thì không được lợi gì.
- Kiểm: probe `IAI_PRINT_SHEET_PROBE` (`tmp/anh-the/xep-in`), so độ nét tấm 4×6 trước / sau.

### 1.2 Push + làm mới portable — chỉ khi chủ bảo

- Trước khi push: `cargo fmt --check` và `cargo test --lib` (xem mục 3 về cách chạy test).
- Portable: quy trình trong ghi chú `project_iai_portable_package.md` (exe có Canvas Editor,
  LICENSE / THIRD_PARTY / docs, `BUILD_INFO.txt` ghi ngày, commit, SHA-256).
- Có portable mới thì chủ mới test được "hiển thị trong trẻo" trên máy khác (mục 2).

### 1.3 Lưu công thức chân dung + áp hàng loạt (Phase 4 của kế hoạch kiểu Evoto)

`docs/planning/KE_HOACH_CHAN_DUNG_KIEU_EVOTO_2026-09-29.md`, mục Phase 4: lưu / nạp bộ thông
số; "áp cho các tab đang mở"; "chạy cả thư mục → xuất JPEG" (chạy nền, tiến độ, Hủy). Việc
lớn; hỏi chủ có cần cho việc ở tiệm không rồi mới làm.

### 1.4 Lỗi nhỏ ghi từ 25/09 — kiểm lại còn hay không rồi mới sửa

- Phím `[` `]` (và `Shift`) đổi nhầm cỡ Brush khi đang cầm Smudge / Dodge / Burn / Pencil.
- Edit ▸ Smart Fill (AI) chưa dùng `refine_fill` như Repair Brush.
- Overlay egui bán trong suốt bị tối; nhiều overlay không chia `pixels_per_point` (lệch khi
  DPI ≠ 100%).

### 1.5 Dọn thư mục `target` (38 GB) — cần chủ nói rõ "xóa"

Phần lớn là bản build thử cũ: `target\portrait-a3`, `portrait-b1`, `portrait-test`, `probe`,
`tmp`, `debug`. Giữ `target\release`. Chưa xóa gì; chủ mới trả lời "ok" chung chung nên
phải hỏi lại trước khi xóa.

## 2. Đang chờ chủ test (đã build từ trước)

- Select ▸ Color Range: con trỏ ống hút, bỏ picker (`d2fe039`).
- Select Subject chạy GPU, tự lùi CPU khi lỗi.
- Ba lỗi canvas lớn (xuất RAW lớn ra JPEG, hút màu toàn ảnh, Alt+Delete).
- Hiển thị trong trẻo trên máy khác (cần portable mới).

## 3. Ghi chú kỹ thuật cho phiên sau

- Test: `cargo test --lib -- --skip app::portrait_ops` (~5 phút) rồi
  `cargo test --lib app::portrait_ops -- --test-threads=1` (~4–5 phút). Chạy chung thì các
  bài chân dung hết giờ chờ ("analysis hung") vì máy quá tải.
- Build Release ~12–14 phút; kiểm `tasklist` xem `iai.exe` có đang mở không, có thì build
  sang `--target-dir` khác, không kill.
- Probe có sẵn (đặt biến môi trường là thư mục ảnh `.jpg`, chạy `-- --ignored --nocapture`):
  `IAI_PRINT_SHEET_PROBE` (trang in), `IAI_PORTRAIT_EYE_PROBE`, `IAI_PORTRAIT_SKIN_TONE_PROBE`,
  `IAI_PORTRAIT_FIX_PROBE`, `IAI_ID_PHOTO_PROBE`. Ảnh thử trong `tmp/anh-the/`.
- Sửa file `.rs` bằng script: ghi script ra file rồi chạy; heredoc trong Bash làm hỏng dấu
  `\` (đã dính hai lần).
- Chưa thử trên ảnh thật: đau mắt đỏ (mới thử mắt đỏ giả lập). Chưa nhìn tận mắt bố cục
  nhóm "Xếp ảnh in" trong hộp thoại Chỉnh chân dung (chủ đã test OK).

## 4. Chủ đã gác — không tự làm, không nhắc lại nhiều

Dáng người (chưa chính xác), mở RAW nhanh như PTS, trình soạn văn bản (Canvas Editor), PSD.
