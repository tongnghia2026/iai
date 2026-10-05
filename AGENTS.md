# Quy ước làm việc của chủ dự án

- Sau mỗi lần hoàn tất thay đổi code cần chủ dự án kiểm thử thủ công, phải build
  sẵn bản chạy được và xác nhận build thành công trước khi mời chủ dự án test.
- Khi bàn giao bản test, cung cấp lệnh chạy hoặc đường dẫn file thực tế, đầy đủ;
  không dùng tên file giữ chỗ như `<file.iai>`.
- Khi chủ dự án báo app bị lag, treo, tự tắt hoặc "vừa gặp lỗi": đọc "hộp đen"
  trước khi hỏi lại chủ. `%APPDATA%\IAI\diagnostics\INDEX.log` có mỗi sự cố một
  dòng; thư mục phiên tương ứng có `journal.log` (chủ đã thao tác gì, theo giờ),
  `incidents.log` (app kẹt ở hàm / dòng code nào) và `watcher.log`. Dòng `MARK`
  trong journal là chỗ chủ bấm Ctrl+Shift+F12 để đánh dấu lỗi. Code: `src/diag/`.
