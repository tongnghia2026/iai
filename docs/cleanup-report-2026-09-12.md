# Báo cáo portable và dọn workspace — 12/09/2026

Phạm vi: `C:\Users\Admin\Documents\IAI`.

## Portable mới

- Thư mục: `dist/iAi-portable`.
- Chỉ còn một executable chính: `iai.exe`.
- Kích thước executable: `73.247.232` byte.
- SHA-256: `17B93F1458FB407E5DE1F32EE375A2879DB0160FFBE35AB163380B8AF7DAC48F`.
- Toàn gói: 61 tệp, `760.780.062` byte.
- Model, license và extension đã được đồng bộ với source hiện hành và đối chiếu
  SHA-256 từng tệp, không có sai khác.
- `BUILD_INFO.txt`, `HUONG_DAN.txt` và `README_TEST.txt` đã dùng tên chạy thống
  nhất `iai.exe`; hướng dẫn không còn nói model phải tải từ mạng.

## Đã xóa

Các mục dưới đây đã được resolve thành đường dẫn tuyệt đối nằm trong workspace,
kiểm tra không có junction/reparse point trước khi xóa:

- Gói cũ `output/iAi_Auto_Retouch_Phase2_ColorFix_2026-09-05`.
- Target test cũ `target/crop-input-fix`.
- Cache Cargo `target/debug`, `target/release` và `target/flycheck0`.
- Ba log Q0/Q1 cũ ngay dưới `target`.
- Năm executable thử nghiệm cũ trong portable:
  - `iai-inline-text-fix-test.exe`;
  - `iai-large-canvas-fixes-test.exe`;
  - `iai-retouch-ui.exe`;
  - `iai-square-wrap-fix-test.exe`;
  - `iai-top-bottom-image-test.exe`.
- Hai thư mục rỗng `output` và `target/tmp`.

Tổng dung lượng tệp đã biết được loại bỏ là ít nhất `32.110.477.071` byte,
xấp xỉ 29,90 GiB; con số này chưa cộng dung lượng ba log nhỏ.

## Cố ý giữ lại

- `target/document-insert-test` (`2.710.691.382` byte): PID 8952 đang chạy
  `release/iai.exe`; không tự kết thúc để tránh mất dữ liệu chưa lưu. Có thể xóa
  toàn bộ target này sau khi đóng phiên đó vì portable đã nhận đúng executable.
- `.pnpm-store` (40.960 byte): chứa reparse point nên lượt xóa an toàn đã dừng và
  mục này được giữ nguyên, không đi theo liên kết ra ngoài workspace.
- `tmp` (`3.286.384.311` byte): gồm checkpoint, source xuất model và dữ liệu đối
  chiếu chất lượng chưa có xác nhận đã hết giá trị tái lập.
- `dist/camera_profiles` (`1.021.912` byte): có profile riêng khác bộ portable.
- Tài liệu kế hoạch/kiến trúc và source chưa commit: giữ nguyên.

## Xác minh

- Hash `dist/iAi-portable/iai.exe` trùng Release nguồn.
- Header executable là `MZ` hợp lệ.
- Portable không còn tham chiếu tên của năm executable cũ.
- Các mục cache/gói cũ trong danh sách xóa không còn tồn tại.
- Build Release, test WebView 12/12, offline asset test, `cargo check`, fmt và
  diff-check đã đạt trước khi hợp nhất portable.
