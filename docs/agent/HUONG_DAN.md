# Cho trợ lý AI điều khiển iAi

## Nút «Việc nhanh» (không cần AI, không cần mạng)

Việc đã rõ ràng thì iAi tự làm trên cả thư mục ảnh, vài giây tới vài phút, không tốn lượt AI, máy
không có Antigravity cũng dùng được:

1. Bấm **Việc nhanh** ở cuối hàng menu trên cùng (cạnh «Nhờ AI làm»).
2. Bấm **Chọn thư mục…** chọn thư mục ảnh của khách (cùng thư mục với «Nhờ AI làm»).
3. Chọn việc:
   - **Làm sạch ảnh chụp giấy tờ**: mọi ảnh trong thư mục thành bản trắng, chữ rõ (kiểu Xám hoặc
     Đen trắng);
   - **Ảnh thẻ cho nhiều người** (Gói Pro): mỗi ảnh là một người, chọn cỡ, nền, giấy và số tấm
     mỗi người; tờ in mở thành tab mới, PDF lưu sẵn;
   - các **kỹ năng đã lưu** làm việc trên một thư mục (AI làm xong việc nào thì iAi tự lưu).
4. Bấm **Làm**. Trong lúc chờ vẫn làm việc khác được; muốn thôi thì bấm **Dừng**. Kết quả nằm
   trong thư mục `iAi_ket_qua` cạnh ảnh (bấm **Mở thư mục kết quả**). Ảnh gốc không bị sửa.

Việc cần nhìn rồi phán đoán (ảnh nào mặt trước, mặt sau, của ai; mỗi người số tấm khác nhau)
thì dùng «Nhờ AI làm».

## Nút «Nhờ AI làm» (không phải gõ lệnh)

Máy có **Antigravity** (ứng dụng AI của Google, dùng Gemini) thì giao việc ngay trong iAi:

1. Bấm **Nhờ AI làm** ở cuối hàng menu trên cùng.
2. Lần đầu: bấm **Nối với Antigravity** (iAi tự cài, Antigravity tự khởi động lại một lần).
3. Chọn việc (In căn cước lên A4, Ảnh thẻ cho nhiều người, Làm sạch ảnh chụp giấy tờ, hay
   Việc khác tự viết), bấm **Chọn thư mục…** chọn thư mục ảnh của khách, dặn thêm nếu cần
   (ví dụ "chị Lan 8 tấm"), rồi bấm **Gửi cho Gemini**.
4. Gemini làm ngay trên iAi: bạn thấy ảnh mở ra, được cắt, được xếp; hộp "Nhờ AI làm" ghi
   đang làm tới bước nào. Muốn thôi thì bấm **Dừng**.
5. Xong: kết quả mở sẵn trong iAi và nằm trong thư mục `iAi_ket_qua` cạnh ảnh của khách
   (bấm **Mở thư mục kết quả**). Ảnh gốc không bị sửa.

Mặc định Gemini chỉ dùng công cụ của iAi (AI offline trên máy). Ca khó — thay áo (sơ mi trắng,
vest, áo dài), xóa chữ hay vật che mặt, nền quá rối — thì tích **Dùng AI online cho ca khó**:
Gemini nhờ trợ lý vẽ ảnh của Antigravity vẽ lại riêng những ảnh đó rồi làm tiếp như thường. Ảnh
vẽ lại có thể lệch nét mặt một chút: xem kỹ trước khi in. Căn cước không bao giờ được vẽ lại.

Sửa một ảnh lẻ: mở ảnh trong iAi, chọn **Sửa ảnh đang mở (AI online)**, viết sửa gì (ví dụ "thay
áo sơ mi trắng, xóa chữ trên áo") rồi gửi. Ảnh Gemini vẽ về thành **lớp mới** ngay trên ảnh đó:
che mặt nạ (mask) để giữ lại phần nào của ảnh gốc. Một bản ảnh vẽ nằm trong thư mục `iAi_ket_qua`
cạnh ảnh.

Ảnh thẻ bị hụt vai (khách chụp quá sát) hay cần thay áo: làm ảnh thẻ như thường, rồi bấm **Thay
áo, lấp vai bằng AI…** ngay dưới các mũi tên chỉnh khung (hoặc chọn việc **Thay áo, lấp vai ảnh
thẻ đang mở** trong "Nhờ AI làm"). Chọn áo, hoặc để **Giữ nguyên áo** thì Gemini chỉ vẽ tiếp phần
vai, thân bị thiếu. Ảnh vẽ về được iAi xoay, co giãn cho khuôn mặt khớp đúng ảnh gốc rồi đặt
thành lớp mới có mặt nạ: **từ cằm trở lên vẫn là ảnh gốc** (mặt, tóc, nền không bị vẽ lại), chỉ
phần áo và vai là của AI. Chỗ cổ áo, vai chưa ưng thì tô mặt nạ của lớp mới.

Cuộc trò chuyện vẫn lưu trong Antigravity nếu bạn muốn xem lại Gemini đã làm gì.

## Nói việc với trợ lý AI

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

## Kỹ năng đã lưu

Mỗi việc AI làm xong và đạt, iAi **tự lưu thành một kỹ năng** — bạn không phải làm gì. Lần
sau bạn nhờ việc tương tự (kể cả với một AI khác), AI tìm trong kho trước, có rồi thì lấy ra
chạy ngay, không phải mò lại từ đầu: nhanh hơn, tốn ít lượt AI hơn, kết quả lần nào cũng như
nhau.

- Kho nằm ở thư mục `Documents\iAi_Ky_nang` (mỗi kỹ năng một thư mục con, có file `SKILL.md`
  đọc được bằng mắt).
- Xem danh sách trong **Help ▸ Cho AI điều khiển iAi…**, phần **Kỹ năng đã lưu**: tên, đã dùng
  mấy lần, lần dùng gần nhất. Bấm **Mở thư mục kỹ năng** để xem file.
- Không muốn giữ một kỹ năng: bấm dòng **Xóa kỹ năng…** dưới danh sách, chọn kỹ năng, bấm
  **Xóa**. iAi cất nó sang thư mục `.da_xoa` trong kho, cần thì lấy lại được.
- Việc giống hệt lần trước thì iAi chỉ ghi thêm một lần dùng; cùng việc nhưng khác cỡ ảnh, màu
  nền… thì iAi thêm chỗ chọn vào kỹ năng cũ, không đẻ thêm kỹ năng mới.
- Có sẵn các kỹ năng mẫu: **ảnh thẻ hàng loạt → tờ in → PDF**, **thiệp 20/10 tô màu cho cả
  lớp** (cần Python trên máy), **thêm lớp cỏ hoa lên mọi trang PDF**, **in căn cước lên A4**
  và **ảnh thẻ nhiều người, mỗi người số tấm riêng**.

## Căn cước và ảnh thẻ khách gửi

Khách gửi một mớ ảnh: căn cước chụp nghiêng (mặt trước, mặt sau), ảnh chụp màn hình căn cước
điện tử, ảnh chân dung. Tải hết vào một thư mục rồi nói với AI, ví dụ:

> Thư mục `C:\Users\...\Downloads\khach_A`: in căn cước lên A4; ảnh thẻ 3x4 nền trắng, chị
> Lan 4 tấm, anh Minh 6 tấm.

AI xem cả thư mục trong một tấm ảnh ghép, tự chia ảnh nào là mặt trước, mặt sau, của ai; mỗi
thẻ được tự tìm bốn góc và cắt thẳng đúng cỡ thật; mặt trước và mặt sau nằm một hàng, ảnh màn
hình xoay ngang, bốn hàng một trang A4. Ảnh chân dung thành ảnh thẻ, xếp chung lên giấy ảnh,
mỗi người đúng số tấm. Kết quả là hai file PDF trong thư mục con `iAi_ket_qua`. Thư mục của
khách phải nằm trong thư mục bạn cho AI dùng (ví dụ Downloads).
