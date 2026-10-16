# Cho trợ lý AI điều khiển iAi

Trợ lý AI trên máy (Claude Desktop, Claude Code…) điều khiển được iAi: mở ảnh, tách nền,
làm ảnh thẻ, xếp ảnh in, xuất PDF… Bạn chỉ cần nói việc bằng lời, ví dụ:

> "Làm ảnh thẻ 3×4 nền xanh cho hết ảnh trong thư mục D:\Anh\Khach, xếp lên giấy 10×15,
> mỗi người 2 tấm, xuất ra PDF."

## Cài một lần

1. Mở iAi, vào **Help ▸ Cho AI điều khiển iAi…**
2. Ô "Thư mục AI được đọc ảnh và ghi kết quả": gõ thư mục chứa ảnh khách (ví dụ
   `D:\Anh`). Ngoài thư mục này AI không đụng tới file nào.
3. Bấm **Chép đoạn này**.
4. Mở Claude Desktop ▸ Settings ▸ Developer ▸ Edit Config, dán đoạn vừa chép vào file
   cấu hình (nếu file đã có phần `mcpServers` thì chỉ thêm mục `"iai"` vào đó), lưu file,
   rồi tắt hẳn và mở lại Claude Desktop.

## Mỗi lần dùng

1. Mở iAi, vào **Help ▸ Cho AI điều khiển iAi…**, bấm **Bật** (thấy "● Đang bật").
2. Nói việc với trợ lý AI. Bạn thấy từng bước hiện ra trong iAi; bước nào không ưng thì
   Ctrl+Z như thường.
3. Xong thì vào lại hộp đó bấm **Tắt** (tắt iAi cũng tự tắt).

## Điều cần biết

- AI **không được in** và **không được ra mạng**; muốn in thì bạn tự bấm In.
- Khi bạn đang làm dở một thao tác (đang biến đổi, đang gõ chữ, đang mở hộp chỉnh màu
  có xem trước, đang ở Develop), AI phải chờ bạn làm xong mới sửa ảnh được.
- Làm ảnh thẻ và xếp ảnh in là phần của gói Pro: máy phải đã mở khóa Pro hoặc đang dùng thử.
- Không muốn mở iAi: xóa chữ `"--bridge",` trong đoạn cấu hình; khi đó AI tự làm việc
  riêng, kết quả ghi ra thư mục đã cho.
