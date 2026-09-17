iAi — Thư mục model AI cho Auto Retouch
=======================================

Mỗi tính năng có MỘT thư mục riêng ở đây. Muốn dùng model nào, tải file .onnx
đúng chuẩn của tính năng đó rồi BỎ VÀO đúng thư mục là chạy — không cài gì thêm.

Ở bản cài cho người dùng, các thư mục này nằm tại:
  %APPDATA%\IAI\models\        (Windows)
  ~/.local/share/iai/models/   (Linux)
App tự tạo sẵn các thư mục + file README này ở đó khi mở AI Auto Retouch.

QUAN TRỌNG:
- App KHÔNG kèm sẵn model và KHÔNG tự tải. Bạn tự tải model và tự chịu trách
  nhiệm về giấy phép sử dụng của model (nhất là khi dùng cho mục đích thương
  mại). Nhiều model chỉ cho phép dùng phi thương mại.
- Model "chuẩn" (đúng checksum) sẽ hiện tên và được tin cậy. Model bạn tự thả
  vào chạy ở chế độ "tùy chỉnh (chưa kiểm định)": nếu sai định dạng, app không
  hỏng — chỉ báo lỗi và quay về xử lý CPU.

Các thư mục mở cho model tùy chỉnh:
- gfpgan/      -> Phục hồi khuôn mặt   (xem gfpgan/README.txt)
- realesrgan/  -> Tăng nét & phóng to  (xem realesrgan/README.txt)

Các thư mục model lõi (nên giữ đúng bản chuẩn, không nên thay):
- face-detector/, bisenet/, body-parsing/, nafnet/, iat/
