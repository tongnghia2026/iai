# Kế hoạch tiếp theo sau các đợt ảnh thẻ / chân dung (04/10/2026)

Bàn giao cho phiên làm việc mới. Lịch sử chi tiết các đợt 1–15 nằm ở
`docs/planning/KE_HOACH_ANH_THE_2026-10-02.md`.

## 0. Trạng thái khi bàn giao

- Nhánh `feat/vector-core-foundation`; mọi thứ **commit local, chưa push** (push gần nhất
  02/10 @ `d1f4acb`). Chỉ push khi chủ bảo.
- Bản test đợt 21: **`target\release\iai.exe`** (build 04/10 16:23, có
  `--features canvas-editor-webview`).
- Chủ test OK đợt 18 (04/10): ô "Công thức" trong Làm ảnh thẻ, năm công thức có sẵn, bỏ chú
  thích nổi trên thanh kéo.
- Chủ test OK đợt 19 (04/10): giao diện mới của Chỉnh chân dung / Develop / Làm ảnh thẻ.
- Chủ test OK đợt 20 (04/10): gõ số vào ô của thanh kéo; Develop chỉ mở một mục; icon môi.
- **Chờ chủ test — đợt 21 (04/10):** bấm vào ô số ở Develop không còn làm ảnh / giao diện
  nháy; các hàng không nhích khi ô nhập hiện ra.
- Đợt 17 chủ chưa nói rõ: phím `[` `]` với Smudge / Dodge / Burn; Pencil vẽ đúng cỡ và màu;
  Smart Fill (AI) giữ vân ảnh.
- Bản portable `dist\iAi-portable` vẫn là bản 02/10 — thiếu mọi thứ làm từ 03/10.
- Chủ đã test OK: đợt 8–15 (Chi tiết mặt AI, Sửa màu & sáng, mặc định mới, thanh màu hai
  chiều, Sáng da = Midtones, Xếp ảnh in + trang 13×18 hỗn hợp, viền cắt 2 px, hết vạch mờ ở
  mép ảnh thẻ, Trắng mắt / Trắng răng).
- Chủ test OK đợt 16 (04/10): "Làm ảnh thẻ" cắt 1043×1417 px, tấm 4×6 nét hơn (mục 1.1).
- Chủ chốt 04/10: crop sau khi Áp dụng làm layer "Chân dung" không mở lại được → **bỏ qua,
  coi là tính năng**, không sửa.

Cách chia việc chủ đã chốt: ảnh quá khó → AI Image Studio (Gemini / ChatGPT); ảnh chụp đẹp
hoặc không có mạng → Làm ảnh thẻ + Chỉnh chân dung + Xếp ảnh in.

## 1. Việc đề nghị làm tiếp (theo thứ tự)

### 1.1 Tấm 4×6 nét hơn — XONG (đợt 16, 04/10), chủ test OK

Chủ đồng ý 04/10 ("tiếp tục, Tấm 4×6 nét hơn"). "Làm ảnh thẻ" nay cắt ở **1043×1417 px**, vẫn
là 2,8×3,8 cm (≈ 947 ppi); tấm 4×6 lấy thẳng điểm ảnh, tấm 3×4 thu nhỏ khi xếp trang. Chi
tiết và số đo: mục "Đợt 16" trong `KE_HOACH_ANH_THE_2026-10-02.md`.

- Preset Crop "Ảnh thẻ 3×4 (2.8×3.8cm 600dpi)" (cắt tay) vẫn 661×898 — chưa đổi, chủ chưa yêu
  cầu.
- "Chỉnh chân dung" trên ảnh thẻ cỡ mới (2,5 lần số điểm ảnh): đã thử làm mịn da / Tạo khối /
  Vân da bằng probe `IAI_PORTRAIT_FORM_PROBE` (`tmp/anh-the/net-4x6/chan-dung`), kết quả giống
  cỡ cũ. Chưa thử "Chi tiết mặt (AI)" và Dáng mặt ở cỡ mới.

### 1.2 Push + làm mới portable — chủ trả lời 04/10: "Chưa, để sau"

- Trước khi push: `cargo fmt --check` và `cargo test --lib` (xem mục 3 về cách chạy test).
- Portable: quy trình trong ghi chú `project_iai_portable_package.md` (exe có Canvas Editor,
  LICENSE / THIRD_PARTY / docs, `BUILD_INFO.txt` ghi ngày, commit, SHA-256).
- Có portable mới thì chủ mới test được "hiển thị trong trẻo" trên máy khác (mục 2).

### 1.3 Lưu công thức chân dung — ĐÃ LÀM phần lưu / nạp (đợt 17), chờ chủ test

Chủ chọn 04/10: **chỉ lưu / nạp công thức**; phần "áp cho các tab đang mở" và "chạy cả thư
mục" KHÔNG làm (chưa cần ở tiệm) — đừng tự làm, đừng đề xuất lại trừ khi chủ hỏi. Chi tiết:
mục "Đợt 17" trong `KE_HOACH_ANH_THE_2026-10-02.md`. Nguyên văn mục cũ:

`docs/planning/KE_HOACH_CHAN_DUNG_KIEU_EVOTO_2026-09-29.md`, mục Phase 4: lưu / nạp bộ thông
số; "áp cho các tab đang mở"; "chạy cả thư mục → xuất JPEG" (chạy nền, tiến độ, Hủy). Việc
lớn; hỏi chủ có cần cho việc ở tiệm không rồi mới làm.

### 1.4 Lỗi nhỏ ghi từ 25/09 — hai lỗi ĐÃ SỬA (04/10), còn một

- [x] Phím `[` `]` (và `Shift`) nay đổi cỡ / độ cứng của đúng công cụ đang cầm: Smudge, Dodge,
  Burn, Quick Selection (trước đều đổi nhầm Brush) — `092b2e2`. Cùng lúc sửa **Pencil**: nó
  hiện thanh tùy chọn của Brush nhưng lại vẽ bằng cỡ / màu riêng không bao giờ được đặt (luôn
  5 px, màu đen); nay vẽ đúng cỡ, độ mờ và màu của Brush, nét cứng.
- [x] Edit ▸ Smart Fill (AI) nay khôi phục vân ảnh ở độ phân giải đầy đủ như Repair Brush
  (`refine_fill`) — `5295d34`. Đo trên cảnh thử: độ hạt trong vùng lấp 13,6 → 21,3 (ảnh thật
  23,2).
- [ ] Overlay egui bán trong suốt bị tối; nhiều overlay không chia `pixels_per_point` (lệch khi
  DPI ≠ 100%). Chưa làm: cần màn hình đặt tỉ lệ khác 100% để thấy và kiểm.

### 1.5 Dọn thư mục `target` — ĐÃ LÀM (04/10)

Chủ bảo 04/10 "Dọn bản build cũ". Đã xóa `target\portrait-a3`, `portrait-b1`,
`portrait-test`, `probe`, `tmp`, `debug` (≈ 35 GB); giữ `target\release`. `debug` dựng lại
sạch khi chạy test (≈ 3 GB thay cho 18 GB). Ổ C: trống 127 GB → ≈ 158 GB.

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
