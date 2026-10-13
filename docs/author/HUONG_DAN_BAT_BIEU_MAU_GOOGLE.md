# Bật biểu mẫu nhận tin từ người dùng iAi (làm một lần, khoảng 10 phút)

Sau khi bật, mỗi lần người dùng bấm "Gửi" trong app ("Góp ý, báo lỗi", "Cần tính năng
riêng", hoặc thông tin đơn vị tài trợ), chủ nhận được:

- một email tới minhnghia718@gmail.com, kèm file nếu có (logo, nhật ký lỗi);
- một dòng trong một bảng Google Sheet, để theo dõi;
- file đính kèm nằm trong thư mục Google Drive "iAi - file người dùng gửi".

Không có mật khẩu nào nằm trong app. Chưa bật thì app vẫn chạy: lời nhắn được lưu thành
một file trên Desktop của người dùng để họ tự gửi.

## Các bước

1. Mở trình duyệt, đăng nhập tài khoản Google **minhnghia718@gmail.com**.
2. Vào địa chỉ `https://sheets.new` để tạo một bảng tính mới. Đặt tên bảng (góc trên bên
   trái) là `iAi - tin từ người dùng`.
3. Trên thanh menu của bảng tính, bấm **Tiện ích mở rộng ▸ Apps Script**. Một thẻ mới mở ra,
   có sẵn vài dòng mã mẫu.
4. Xóa hết mã mẫu trong khung soạn. Mở file `docs/author/form.gs` (nằm cạnh file hướng dẫn
   này), chép toàn bộ nội dung và dán vào khung soạn. Bấm biểu tượng **Lưu** (hình đĩa mềm).
5. Trên thanh công cụ, ở ô chọn hàm (cạnh nút "Gỡ lỗi"), chọn **setup**, rồi bấm **Chạy**.
   Google hỏi quyền:
   - bấm **Xem lại quyền**, chọn tài khoản minhnghia718@gmail.com;
   - nếu hiện "Google chưa xác minh ứng dụng này": bấm **Nâng cao ▸ Đi tới … (không an
     toàn)** — đây là đoạn mã của chính chủ, chạy trong tài khoản của chủ;
   - bấm **Cho phép**.
   Vài giây sau hộp thư có email "[iAi] Biểu mẫu đã sẵn sàng", và bảng tính có hàng tiêu đề.
6. Bấm nút **Triển khai** (góc trên bên phải) **▸ Tùy chọn triển khai mới**.
   - Bấm biểu tượng bánh răng cạnh "Chọn loại", chọn **Ứng dụng web**.
   - "Thực thi dưới dạng": **Tôi**.
   - "Người có quyền truy cập": **Bất kỳ ai**.
   - Bấm **Triển khai**.
7. Google hiện **URL ứng dụng web**, dạng `https://script.google.com/macros/s/…/exec`. Bấm
   **Sao chép**.
8. Dán địa chỉ đó cho Claude. Claude ghi nó vào `docs/author/news.json` (mục `form_url`); sau
   lần push kế tiếp, mọi máy đang dùng app tự biết địa chỉ này, không cần ra bản app mới.

## Thử

- Dán địa chỉ ở bước 7 vào trình duyệt: phải hiện dòng "iAi: biểu mẫu đang chạy."
- Trong app: Help ▸ "Góp ý, báo lỗi", viết một dòng, bấm "Gửi". Kiểm tra hộp thư và bảng tính.

## Khi cần sửa mã sau này

Dán mã mới vào Apps Script, Lưu, rồi **Triển khai ▸ Quản lý các tùy chọn triển khai ▸ biểu
tượng bút chì ▸ Phiên bản: Phiên bản mới ▸ Triển khai**. Làm cách này thì địa chỉ giữ nguyên.
(Tạo "Tùy chọn triển khai mới" sẽ ra địa chỉ khác, phải báo lại cho Claude.)

## Giới hạn

- Tài khoản Gmail thường gửi được khoảng 100 email mỗi ngày qua Apps Script; mã tự dừng nhận ở
  80 tin một ngày (sửa số `MAX_PER_DAY` ở đầu file). Tin bị từ chối thì app lưu ra Desktop
  của người gửi.
- Địa chỉ biểu mẫu là công khai nên có thể có người gửi rác. Rác chỉ tới hộp thư và bảng
  tính; không gì tự lên "Danh sách nhà tài trợ" vì danh sách do chủ duyệt.
- Muốn tắt hẳn: **Triển khai ▸ Quản lý các tùy chọn triển khai ▸ Lưu trữ**.
