# Kế hoạch: menu "Áo" và thanh "Da cổ" (06/10/2026 — sửa lần 2 theo ý chủ)

**Trạng thái (06/10 tối): đợt 33–37 CHỦ TEST OK HẾT. VIỆC KẾ TIẾP (chủ bảo làm ở hội thoại
mới): đợt 38 — ba việc cho ô "Áo" (nút mở file áo, tự quay về đúng ảnh, nút "Đổi áo khác");
rồi đợt 39 — viền áo khớp với da (chưa có thiết kế, làm ảnh thử trước). Xem mục 7.**
Nối tiếp `KE_HOACH_THAY_AO_OFFLINE_2026-10-05.md` (đợt 31, 32 chủ test OK); kế hoạch này thay
cho "đợt 33" ghi ở đó.

## 1. Việc chủ giao

Lời chủ 06/10, lần đầu: "phần mặt, tóc,.... đã ok; tôi cần thêm 1 model nhận diện, khoanh vùng
áo để làm nét và cân bằng ánh sáng; ngoài ra: sau khi thay áo xong user điều chỉnh smudge,
clone,... các phần ở da cổ thì cần thêm 1 nút chạy lại chi tiết da, cân bằng ánh sáng chỉ ở
vùng cổ; hãy lên kế hoạch cho tôi đọc".

Lời chủ 06/10, sau khi đọc bản đầu: "tạo thêm 1 menu áo riêng, trong đó có thanh kéo tăng nét,
thanh điều chỉnh ánh sáng áo,... phần da cổ tạo thêm 1 thanh kéo ở menu da; phần chạy lại chi
tiết da, làm nét da cổ tận dụng luôn model chi tiết mặt (AI) - nhưng thay vì cho chạy lại toàn
bộ khuôn mặt thì chỉ chạy vùng da cổ (tại vì da mặt trước đó đã được làm nét rồi, nếu chạy thêm
lần nữa sẽ bị làm nét lần 2); đối với phần tăng nét cho áo thì chủ yếu dùng để chạy nét đối với
áo khách mặc mà họ tự chụp bằng điện thoại nên mờ - hoặc những tấm ảnh cũ phục hồi, còn áo ghép
thì đã nét sẵn nên không cần làm nét nữa; điều chỉnh lại kế hoạch cho tôi đọc".

Lời chủ 06/10, lần ba: "làm nét áo trước, tại vì khách hàng không phải lúc nào cũng thay, ghép
áo, họ muốn giữ nguyên áo đang mặc nhưng họ lại chụp bằng điện thoại độ phân giải thấp".

Lời chủ 06/10, sau khi test đợt 33: "đã test ok; tiếp tục đợt 34".

Lời chủ 06/10, sau khi test đợt 34: "đã test ok; lưu bộ nhớ qua hội thoại mới làm tiếp phần còn
lại".

Lời chủ 06/10 chiều, trả lời hai câu tôi hỏi khi đang làm đợt 35:
- File `.iai` đã mặc áo và đã sửa tay chỗ cổ: "không có, nhưng nên tạo thêm tính năng brush tô
  để lấy thêm hoặc xóa bớt vùng da cổ nếu tự động nhận diện sai".
- "Sáng áo" / "Đều sáng áo" cho áo ghép: "Có, mở ra cho phép tác động cả áo ghép, user có nhu
  cầu thì có thể tùy ý chỉnh theo ý thích".

Lời chủ 06/10 chiều muộn, sau khi test đợt 35 + 36: "đã test ok; nhưng bị vấp 1 chỗ, sau khi
tự động ghép xong - người dùng thấy chưa đạt - họ chỉnh lại cổ áo cho đạt - smud, ctrl+t xoay,
tô lại vùng da cổ auto lấy thiếu.... xong muốn chạy lại da cổ thì bị khóa - phải bấm chạy lại
ảnh thẻ thì hệ thống lại ghép và chạy lại từ đầu".

Lời chủ 06/10 tối, sau khi test đợt 37: "đã test ok; ở ngay cạnh ô thay áo có thêm nút mở thư
mục để user trỏ tới file áo để mở lên; sau khi bấm nút lấy áo đang chọn xong thì phải tự nhảy
về ảnh đang cần thay áo, nếu chỉnh nhiều ảnh thì quay về ảnh mới mở gần nhất; có thêm nút đổi
áo khác khi bấm vào thì tự nhảy qua tab file áo để chọn lại mẫu áo khác; phần da cổ đã tạm ổn -
tôi ước có thêm tính năng tự động chạy lại phần viền áo cho nó khớp với da sau khi người dùng
đã tự chỉnh tay xong, hiện tại nhìn khá giả trân - áo chỉ là 1 layer chồng bên trên chứ chưa
thật sự liên kết như mặc áo thật; phiên này chỉ lưu bộ nhớ, cập nhật kế hoạch; qua hội thoại
mới làm tiếp".

## 2. Chủ đã chốt gì ở lần sửa này

| Việc | Bản đầu | Nay (theo chủ) |
|---|---|---|
| Chỗ đặt các thanh của áo | Nhóm "Áo" | **Menu "Áo" riêng**: thanh tăng nét, thanh ánh sáng áo… |
| Da cổ | Một nút ở hàng "Áo" | **Một thanh kéo "Da cổ" trong menu "Da"** |
| Cách làm chi tiết da cổ | Vân da tự tạo / mượn da má | **Dùng luôn model "Chi tiết mặt (AI)", chỉ lấy vùng da cổ** |
| Tăng nét áo dùng cho | Cả áo mặc sẵn lẫn áo ghép | **Chỉ áo khách mặc sẵn** (ảnh điện thoại mờ, ảnh cũ phục hồi); áo ghép không làm nét |

## 3. Tóm tắt

1. **Khoanh vùng áo không cần tải model mới**: Sapiens2 đang dùng cho da và tóc đã có sẵn nhãn
   "Áo". Đã thử trên ảnh khách thật.
2. **Thanh "Da cổ" làm được đúng như chủ nói**: tôi đã thử cho model "Chi tiết mặt (AI)" nhìn
   một khung có cả cổ, trên 2 ảnh đã mặc áo — da cổ đang phẳng lì có lại vân da mịn, mỗi lượt
   khoảng 1,8 giây. App chỉ lấy phần chi tiết ở cổ; **mặt giữ nguyên từng điểm ảnh** nên không
   bị nét lần hai. Ảnh xem: `tmp\vung-ao\da-co-thu-AI.jpg`.
3. **Model "Chi tiết mặt (AI)" không dùng được cho áo**: cũng trong lần thử đó nó vẽ méo hoa văn
   cổ áo và vẽ viền lạ lên áo trắng. Vậy tăng nét áo phải đi đường khác — đây là **chỗ duy nhất
   có thể phải thêm một model** (loại phục hồi ảnh Real-ESRGAN), vì việc chủ cần là cứu áo mờ
   trong ảnh điện thoại và ảnh cũ. Thêm hay không, bản nào, quyết bằng ảnh thử ở đợt 33.
4. Làm thành **ba đợt**: ảnh thử → menu "Áo" → thanh "Da cổ".

## 4. Đã kiểm 06/10

- **Khoanh vùng áo**: chạy model Sapiens2 đang cài trên 4 ảnh trong `C:\Users\Admin\Downloads\ht`
  (3 sơ mi trắng, 1 áo thun xanh có logo và bảng tên). Áo được khoanh gọn hai vai và cổ áo; tóc
  dài xõa trước áo không lẫn vào áo; bảng tên ra nhãn "Phụ kiện"; da hở trong cổ áo ra nhãn da.
  Khoảng 1,7 giây mỗi ảnh — và lúc chỉnh chân dung app đã chạy model này sẵn rồi, nên không chờ
  thêm. Ảnh xem: `tmp\vung-ao\nhan-dien-ao-thu.jpg`.
  - Giới hạn: mới 4 ảnh dễ; chưa thử vest tối trên nền tối, áo dài, áo trùng màu nền, trẻ em.
- **Model "Chi tiết mặt (AI)" trên da cổ**: chạy model đang cài (`models\gfpgan`) trên 2 ảnh
  ghép thử của đợt 31, với khung hình dời xuống cho cổ lọt vào.
  - Mặt chiếm khoảng 80% cỡ thường của khung: da cổ có vân mịn, sạch, hợp với da mặt.
  - Mặt chỉ còn 60%: vân da cổ ra thô, lốm đốm như râu — không dùng được. Tức là **cỡ và vị trí
    khung phải chọn kỹ**; sẽ dò trên ảnh thật ở đợt 33.
  - Trong cả hai khung, model làm hỏng phần áo nằm trong khung (hoa văn, mép cổ áo) → vùng lấy
    chi tiết phải dừng trước mép áo, và model này không dùng cho áo.
  - Giới hạn: mới 2 ảnh, cổ là da tô phẳng của app chứ chưa phải cổ chủ đã Smudge / Clone.
- **Áo ghép hiện nay** (`tmp\vung-ao\co-va-ao-hien-nay.jpg`): da tô ở khoảng hở cổ là màu
  phẳng, không vân da, không bóng dưới cằm — đúng chỗ thanh "Da cổ" sẽ xử lý.
- **Luật sẵn có của app (đợt 25)**: layer "Chân dung" đã bị sửa tay thì khi bấm "Tự động làm
  đẹp" app coi nó là ảnh mới, **mọi thanh bắt đầu từ 0**. Thanh "Da cổ" dựa vào đúng luật này.
- **Model phục hồi ảnh cho áo** — *sửa lại điều tôi ghi sai ở bản trước*: ba model Real-ESRGAN
  **đã có sẵn trên máy** trong `Documents\IAI\models\realesrgan` (và trong bản portable); chỉ
  thư mục ở `%APPDATA%` là trống. Bản chạy `target\release\iai.exe` tự tìm thấy chúng, không
  phải tải hay chép gì.
- **Thử làm nét áo trên ảnh khách chụp điện thoại** (`khach_1.jpg`, áo thun có chữ thêu), vùng
  áo của một ảnh thẻ 1043 px, chạy bằng CPU máy tiệm:
  - Làm nét kiểu thường (Develop): **làm nổi hạt nén ảnh, viền quanh chữ** — không dùng được
    cho ảnh điện thoại.
  - Real-ESRGAN bản nhẹ: 1–5 giây, nét nhưng vải bệt như nhựa, mất đường may.
  - Real-ESRGAN bản x2: 4–15 giây tùy cỡ cho xem; nét, giữ được đường may và nếp vải, chữ thêu
    đọc rõ và không méo. **Đây là bản được chọn.**
  - Real-ESRGAN bản x4: chậm gấp bốn bản x2, không đẹp hơn.
  - Ảnh xem: `tmp\vung-ao\net-ao-so-sanh-cac-cach.jpg`.
- **Một điều học được khi thử**: cho AI xem áo ở nửa cỡ thì ảnh thẻ (đã phóng lớn từ ảnh nhỏ)
  ra đẹp, nhưng ảnh còn nguyên cỡ gốc lại bị **hỏng chữ nhỏ trên áo**. Vì vậy app tự chọn cỡ
  theo từng ảnh (mục 5.2).

## 5. Menu "Áo"

Một menu riêng trong bảng Chỉnh chân dung, đặt sau "Tóc". Ba thanh, không gắn chú thích nổi.

### 5.1. Khoanh vùng

- Áo khách mặc sẵn: nhãn "Áo" + "Quần / váy" + "Phụ kiện" của Sapiens2, gọt mép cho sát theo
  đường viền tách người và màu ảnh (cách đang làm với tóc).
- Nhận sai thì sửa tay: "Tô vùng" có thêm lựa chọn **"Áo"**; "Hiện vùng nhận diện" tô vùng áo
  bằng một màu riêng.

### 5.2. Thanh "Nét áo" (0–100)

- **Chỉ tác động lên áo khách đang mặc trong ảnh.** Ảnh đã ghép áo thì thanh này mờ đi, kèm một
  dòng "Áo ghép đã nét sẵn".
- Dùng cho: ảnh khách tự chụp bằng điện thoại bị mờ, ảnh cũ phục hồi.
- **Cách làm đã chọn (đợt 33)**: model Real-ESRGAN bản x2 có sẵn trên máy. Chạy kiểu "Chi
  tiết mặt (AI)": thanh rời số 0 thì app tìm áo rồi chạy model một lần ở nền, sau đó kéo thanh
  là thấy ngay. Màu và sáng tối của áo vẫn là của ảnh; chỉ phần chi tiết lấy từ AI. Mặt, tóc,
  da cổ, nền không bị đụng.
- **App tự chọn cỡ cho AI xem áo**:
  - ảnh thẻ app vừa làm ra từ ảnh nhỏ (app biết đã phóng lớn bao nhiêu lần) → thu lại đúng
    bằng mức phóng: nét lên rõ, khoảng 6–7 giây;
  - ảnh còn nguyên cỡ gốc và mép còn sắc → giữ nguyên cỡ: chữ nhỏ không hỏng, hiệu quả nhẹ hơn
    (sạch hạt nén, mép gọn), tới khoảng 15 giây;
  - ảnh mép nhòe (mờ nét, ảnh cũ) → thu nhỏ theo độ nhòe đo được, không dưới 30% cỡ ảnh.
- Thiếu model thì menu báo "Nét áo cần model Real-ESRGAN (models\realesrgan) — chưa cài".

### 5.3. Thanh "Sáng áo" (hai chiều) và "Đều sáng áo" (0–100)

- **Sáng áo**: trái tối hơn, phải sáng hơn — chỉ riêng áo, không đụng mặt, tóc, nền. Kéo
  sang trái là giảm sáng đều (áo trắng bị loá hiện lại nếp vải; kéo nhiều thì áo trắng ngả
  xám). Kéo sang phải thì áo sáng dần về trắng mà không cháy, nếp vải vẫn còn. Hết thanh là
  khoảng 1,25 khẩu.
- **Đều sáng áo**: một bên vai tối, áo sậm dần xuống dưới → nâng cho đều với phần được chiếu
  sáng. App đo **độ dốc sáng** trên cả cái áo (bỏ qua mép vải, hoa văn, nếp gấp) rồi bù lại
  theo đúng độ dốc đó, nên hoa văn, sọc, ranh giới vest – sơ mi – cà vạt giữ nguyên. Phần
  khuất được nâng tối đa khoảng 1,3 khẩu.
- Hai thanh này không phải chờ AI: app chỉ cần tìm vùng áo (gần như tức thì với ảnh thẻ).
- Với **áo ghép** (đợt 36, theo lời chủ 06/10 chiều): hai thanh này chỉnh **cả cái áo ghép**
  (layer "Áo"), không cần tìm vùng. "Nét áo" vẫn mờ đi vì áo ghép đã nét sẵn. Chi tiết ở mục
  5.5.

### 5.4. Mặc định

Cả ba thanh bắt đầu từ **0** (áo giữ nguyên như chụp), chủ kéo khi cần — vì tăng nét áo chỉ
dùng cho ảnh mờ, ảnh cũ. "Công thức" đã lưu từ trước đọc ba thanh là 0. Sau khi dùng thật, chủ
muốn mức nào tự chạy trong "Làm ảnh thẻ tự động" thì đổi mặc định sau.

### 5.5. Hai thanh sáng trên áo ghép (đợt 36)

- Ảnh đã ghép áo: "Sáng áo" và "Đều sáng áo" tác động lên layer "Áo"; layer "Người" không bị
  hai thanh này đụng tới (áo cũ của khách đã bị xóa khỏi layer đó). Dòng ghi chú trong menu:
  "Ảnh đã ghép áo: hai thanh sáng chỉnh áo ghép; áo ghép đã nét sẵn."
- **Kéo lại không hỏng dần**: app giữ bản áo trước khi chỉnh sáng. Mở lại Auto retouch thì hai
  thanh đứng đúng mức đã áp dụng, và mức mới luôn tính từ áo gốc. Kéo về 0 là áo trở lại như
  lúc ghép.
- "Áp dụng" khi **chỉ có áo đổi**: chỉ layer "Áo" đổi, không thêm layer "Chân dung" (một bước
  Ctrl+Z, tên bước "Sáng áo"). Có chỉnh cả người lẫn áo thì cả hai nằm chung một bước Ctrl+Z.
- Bấm **"Chỉnh áo"** (dời / phóng / xoay) lúc đang xem trước: mức sáng đang xem được ghi vào
  áo trước rồi mới chỉnh hình. Sau khi áo đã bị dời / phóng / xoay (hoặc bị sửa tay), nó được
  coi là áo mới: hai thanh bắt đầu lại từ 0.
- Giới hạn: bản áo gốc chỉ được giữ **trong phiên làm việc**. Lưu file, đóng, mở lại thì hai
  thanh bắt đầu từ 0 trên áo đang có (mức sáng đã áp dụng vẫn nằm trong áo).
- "Tô vùng ▸ Áo" mờ đi với ảnh đã ghép áo: cả layer là áo, không có vùng để tô.
- Ảnh xem trên bốn áo của tiệm (gốc / Sáng áo −60 / +60 / Đều sáng áo 100):
  `tmp\vung-ao\sang-ao-ghep-vest-nam.jpg`, `sang-ao-ghep-vest-nu.jpg`,
  `sang-ao-ghep-so-mi-trang.jpg`, `sang-ao-ghep-ao-dai.jpg`. Áo trắng kéo −60 ngả xám như đã
  ghi ở mục 5.3; "Đều sáng áo" trên áo chụp studio vốn đều sáng nên đổi rất ít.

## 6. Thanh "Da cổ" trong menu "Da"

Một thanh **"Da cổ"** (0–100, mặc định 0), đặt cuối menu "Da".

### 6.1. Kéo thanh thì app làm gì

1. **Tìm vùng da cổ**: phần da từ dưới đường hàm xuống tới mép áo, gồm cả khoảng da hở trong
   cổ áo; trừ tóc; dừng trước mép áo. Mép vùng mềm dần ở đường hàm.
2. **Chạy model "Chi tiết mặt (AI)" một lần** (khi thanh rời số 0, chạy nền vài giây) trên một
   khung hình có cả cổ. Model vẫn phải nhìn thấy khuôn mặt thì mới chạy đúng, nhưng app **chỉ
   lấy phần chi tiết nằm trong vùng da cổ**. Mặt, tóc, áo không nhận gì từ lượt chạy này — mặt
   không bị làm nét lần hai.
3. **Cân sáng vùng cổ**: xóa mảng sáng – tối loang lổ do Clone / Smudge, đưa màu da cổ về đúng
   màu da mặt, giữ cổ tối hơn mặt một chút như thật.
4. Thanh càng cao thì chi tiết AI và độ đều sáng ở cổ càng nhiều; 0 là cổ giữ nguyên.

### 6.2. Cách dùng sau khi thay áo

Mặc áo → sửa tay chỗ cổ trên layer "Chân dung" (Smudge, Clone, Repair…) → bấm "Tự động làm đẹp"
để chỉnh tiếp (các thanh đều về 0 vì ảnh đã làm đẹp rồi) → kéo **"Da cổ"** → "Áp dụng". Chỉ
vùng cổ đổi; một bước Ctrl+Z.

### 6.3. Chi tiết khác

- Ảnh mới (chưa sửa tay) cũng kéo được: hiện nay khung của model chỉ tới ngay dưới cằm, nên ảnh
  điện thoại mờ thường ra **mặt nét mà cổ vẫn mềm** — thanh này làm cổ theo kịp mặt.
- Có vùng chọn thì chỉ làm trong vùng chọn (luật chung của bảng).
- Vùng da nhận sai thì sửa bằng "Tô vùng ▸ Da" như hiện nay.
- Thanh này **không tự vẽ lại** chỗ còn sót áo cũ hay lỗ thủng — đó vẫn là việc của Clone /
  Repair. Nó làm cho chỗ đã sửa tay "liền" với da xung quanh.
- Chỉ một thanh như chủ yêu cầu. Nếu khi test chủ muốn chỉnh riêng "chi tiết" và "đều sáng" ở
  cổ thì tách thành hai thanh sau.

### 6.4. Cọ "Tô vùng ▸ Da cổ" (theo lời chủ 06/10 chiều)

- Lựa chọn thứ năm của cọ, sau "Áo". Vùng da cổ hiện màu **xanh dương**. Tô Thêm / Bớt / Thông
  minh, Ctrl+Z trong bảng như các vùng khác.
- Bấm lần đầu app tìm da cổ vài giây (cùng lượt chạy AI với thanh "Da cổ"), có dòng "Đang tìm
  da cổ và tạo chi tiết bằng AI…".
- **Tô thêm được cả chỗ app không coi là da** (ví dụ mảng da tô phẳng bị bỏ sót): thanh "Da
  cổ" tác động lên đúng chỗ đã tô, không cần tô thêm ở "Tô vùng ▸ Da". Chỗ nằm ngoài tầm nhìn
  của model (rất xa dưới cổ) chỉ được đều màu / đều sáng, không có vân da AI.
- Vùng đã tô được lưu theo layer "Chân dung" và trong file `.iai`.
- Chưa tô lần nào thì vùng da cổ tự đi theo vùng da (kể cả khi vùng da vừa được sửa bằng cọ).

### 6.5. Kết quả dò và ảnh xem (06/10)

- **Cỡ khung**: mặt bằng 80% cỡ thường của model, khung bắt đầu từ ngang trán. 90%: nửa dưới
  cổ ngoài tầm nhìn, da tô phẳng không có vân. 70% và 60%: vân thô, có đường gợn.
- **Ranh vùng cổ** lấy theo vùng da của app (lùi vào trong mép da), không theo nhãn "áo" của
  model tách vùng: trên layer "Người" sau khi mặc áo, nhãn đó không chắc ở chỗ da tô phẳng và
  làm sót cả mảng cổ.
- **Chi tiết AI bị giữ trong ±5% độ sáng** so với ảnh: lỗ chân lông và nếp mảnh qua được; vệt
  trắng và nếp gắt model vẽ dọc đường nối da thật – da tô bị ghìm lại.
- Rắc hạt vào ảnh trước khi cho model xem (để ép ra vân trên da phẳng): model khử hạt chứ không
  vẽ thành lỗ chân lông → không dùng.
- Thử trên 4 layer "Người" thật (mặc áo qua đúng quy trình của app) và 1 ảnh khách mặc áo sẵn:
  mặt không đổi điểm ảnh nào ở cả 5. Ảnh xem, mỗi tấm bốn ô: gốc / Da cổ 50 / 100 / vùng tác
  động tô xanh: `tmp\vung-ao\da-co-ao-ghep-co-kin.jpg` (cổ bị áo cũ che, da tô phẳng cả
  mảng), `da-co-ao-ghep-nu.jpg`, `da-co-ao-ghep-vest.jpg`, `da-co-ao-khach-mac-san.jpg`.
- Điều thấy được: ở 100 da tô phẳng có vân da, đường nối da thật – da tô mờ hẳn; cổ phụ nữ ở
  100 hơi sần (50 thì ổn). Vết sẫm đậm do sửa tay để lại chỉ nhạt bớt, không mất.

## 7. Các đợt

Mỗi đợt xong đều build Release và đưa đường dẫn file chạy thật cho chủ test như lệ thường.

### Đợt 33 — Menu "Áo" với thanh "Nét áo" (làm trước theo lời chủ)

Làm 06/10. Lõi: `src/core/portrait/clothes.rs`; chạy model: `Upscaler` trong
`src/core/ai/retouch.rs`; app: `src/app/portrait_ops.rs`; bảng: `src/ui/dialogs/portrait.rs`.

- [x] Thử các cách làm nét trên ảnh khách thật và chọn cách (mục 4).
- [x] Vùng áo: lấy từ nhãn Sapiens2 đã chạy lúc phân tích (không chạy thêm model); áo dài quá
      khung nhìn quanh mặt thì nhìn thêm một lượt cả người; trừ da, tóc; chia theo từng người
      khi ảnh có nhiều người.
- [x] Menu **"Áo"** (sau "Tóc") với thanh **"Nét áo"**, mặc định 0. Lần đầu kéo thanh app chạy
      nền, có dòng báo "Đang tìm áo và làm nét bằng AI…"; app không khựng.
- [x] Ảnh đã ghép áo: thanh mờ đi, dòng "Ảnh đã ghép áo: áo ghép đã nét sẵn."
- [x] "Hiện vùng nhận diện" tô vùng áo màu xanh lục (sau khi đã kéo "Nét áo").
- [x] App tự chọn cỡ cho AI xem (mục 5.2); thời gian chạy ghi vào "hộp đen" (dòng `perf`).
- [x] "Công thức" lưu và nạp thanh mới; công thức cũ đọc là 0.
- [x] Menu nào vừa mở thì tự cuộn vào tầm nhìn (không trượt): thêm menu "Áo" làm nội dung
      menu cuối "Xếp ảnh in" bị đẩy xuống dưới vùng cuộn trên màn hình thấp. Áp dụng cho mọi
      menu của bảng; menu dài hơn vùng nhìn thì cuộn tới đầu menu.
- Test: 5 bài lõi (vùng áo, cỡ cho xem, đo độ nhòe mép, ảnh AI trả về giữ tông của ảnh, chỉ
  điểm ảnh áo đổi); 1 bài nhãn "đồ đang mặc"; 1 bài bảng (menu "Áo", ảnh đã ghép áo); 1 bài
  chạy model thật trong app trên `tmp/anh-the/am-mau/khach_1.jpg` (kéo thanh → chạy nền → Áp
  dụng: áo đổi, mặt không đổi điểm nào). Cả bộ: nhóm nhẹ 2002 qua; nhóm `app::portrait_ops`
  (chạy từng bài) 21 qua.
  Lệnh xem ảnh thử: `IAI_PORTRAIT_CLOTHES_PROBE=<thư mục ảnh>` với
  `cargo test --lib -- --ignored probe_clothes --nocapture` (ảnh tên `..._x1.6.png` = đã phóng
  1,6 lần).
- Ảnh trước / sau đi qua đúng đường xử lý của app: `tmp\vung-ao\net-ao-truoc-sau-chu-theu.jpg`,
  `net-ao-truoc-sau-hang-cuc.jpg`, `net-ao-truoc-sau-ao-trang.jpg`.
- Chưa làm trong đợt này: "Tô vùng ▸ Áo" (sửa tay vùng áo) — sang đợt 34.
- [x] Chủ test 06/10: **"đã test ok"**.

### Đợt 34 — Phần còn lại của menu "Áo"

Làm 06/10, ngay sau khi chủ bảo làm tiếp.

- [x] **Tách "vùng áo" khỏi phần chạy AI**: vùng áo giờ được tìm riêng (ảnh thẻ: gần như tức
      thì), nên cọ và hai thanh sáng không phải chờ "Nét áo".
- [x] **"Tô vùng ▸ Áo"**: thêm lựa chọn "Áo" cạnh Da / Tóc / Lông mày; tô Thêm / Bớt / Thông
      minh như các vùng khác, Ctrl+Z trong bảng, vùng áo hiện màu xanh lục. Bấm lần đầu app
      tìm áo (có dòng "Đang tìm áo…"). Vùng áo đã tô được lưu theo layer "Chân dung" và theo
      file `.iai`, mở lại vẫn còn.
- [x] **Thanh "Sáng áo"** (hai chiều) và **"Đều sáng áo"** (mục 5.3), mặc định 0, có trong
      "Công thức".
- [x] "Hiện vùng nhận diện" tô vùng áo ngay khi tích (không cần kéo "Nét áo" trước).
- [x] Ảnh thử trước / sau qua đúng đường xử lý của app, trên ảnh cố ý làm tối bên phải:
      `tmp\vung-ao\sang-ao-ao-mot-mau.jpg`, `sang-ao-vest.jpg`, `sang-ao-ao-trang.jpg` — mỗi tấm
      bốn ô: gốc / Đều sáng áo 100 / Sáng áo −60 / Sáng áo +60. Độ dốc app đo khớp độ dốc tôi
      cố ý tạo (áo trắng: tạo 0,56, đo 0,57).
- Test thêm: 3 bài lõi (đo độ dốc sáng qua ranh giới áo sẫm – áo sáng và bù lại; sáng hơn
  không cháy, tối hơn giảm đúng số khẩu; chỉ điểm ảnh áo đổi), file `.iai` giữ vùng áo đã tô,
  bảng có đủ ba thanh và lựa chọn cọ "Áo", 1 bài chạy model thật trong app (chọn cọ Áo → app
  tìm áo → tô bớt một mảng → "Sáng áo" chỉ đổi phần áo còn lại, mặt không đổi → Áp dụng → mở
  lại vẫn còn vùng đã tô).
- Chưa làm: vùng áo trên ảnh khó (vest tối trên nền tối, áo dài, áo trùng màu nền, trẻ em) —
  chưa có ảnh mẫu; hai thanh sáng cho layer áo ghép (mục 8, câu 3).
- Biết trước: kéo "Sáng áo" mạnh thì mép áo (sát cổ, sát nền) có thể hiện một viền mảnh vì
  mép vùng áo là mép mềm — sửa bằng "Tô vùng ▸ Áo", hoặc tôi làm mép vùng áo bám màu ảnh ở đợt
  sau nếu chủ thấy vướng.
- [x] Chủ test 06/10 trên `target\release\iai.exe` (build 10:49): **"đã test ok"**. Chủ không
  nêu gì thêm về viền mép áo hay áo ghép.

### Đợt 35 — Thanh "Da cổ" và cọ "Tô vùng ▸ Da cổ"

Làm 06/10. Lõi: `src/core/portrait/neck.rs`; nối vào phần da: `effects.rs` (`skin_colour`);
app: `portrait_ops.rs`, `portrait_brush.rs`; bảng: `ui/dialogs/portrait.rs`.

- [x] Ảnh thử, dò cỡ khung, ba mức thanh (mục 6.5). Chủ không có file đã sửa tay nên thử trên
      layer "Người" thật sau khi mặc áo và một ảnh tôi giả lập sửa tay.
- [x] Vùng da cổ: dưới đường hàm, trong vùng da, trong tầm nhìn của model.
- [x] Lượt chạy riêng của model "Chi tiết mặt (AI)" cho cổ, chạy nền một lần khi thanh rời 0
      (hoặc khi cọ chọn "Da cổ"); không chạy cùng lúc với lượt của mặt.
- [x] Đều màu, đều sáng, san mảng loang ở cổ; thanh "Da cổ" cuối menu "Da"; thời gian chạy ghi
      vào "hộp đen" (dòng `perf`: `portrait neck detail made in … ms`).
- [x] Cọ "Tô vùng ▸ Da cổ" (mục 6.4); vùng đã tô lưu theo layer và trong `.iai`.
- [x] "Công thức" lưu và nạp thanh mới; công thức cũ đọc là 0.
- Test: 3 bài lõi (khung cổ; vùng cổ không bao giờ lấn vào trong đường viền mặt; chi tiết AI bị
  ghìm gần ảnh), file `.iai` giữ vùng da cổ đã tô, bảng có thanh "Da cổ" và lựa chọn cọ, 1 bài
  chạy model thật trong app trên `tmp/anh-the/am-mau/khach_1.jpg` (kéo thanh → chạy nền → cọ
  bớt một mảng, thêm một mảng ngoài vùng da → Áp dụng: cổ đổi, mảng bớt giữ nguyên, mảng thêm
  có đổi, **không điểm ảnh nào trong đường viền mặt đổi** → mở lại vẫn còn vùng đã tô).
  Lệnh xem ảnh thử: `IAI_PORTRAIT_NECK_PROBE=<thư mục ảnh>` với
  `cargo test --lib -- --ignored probe_neck --nocapture`.
- [x] Chủ test 06/10: **"đã test ok"**.

### Đợt 36 — "Sáng áo" / "Đều sáng áo" cho áo ghép

Làm 06/10, ngay sau lời chủ. Lõi: `LaidGarment` trong `src/core/portrait/clothes.rs`; app:
`portrait_ops.rs` (`WornGarment`, `Applied`), `garment_ops.rs` (`Relit`).

- [x] Hai thanh sáng tác động lên layer "Áo"; xem trước, Áp dụng, Hủy (mục 5.5).
- [x] Giữ bản áo gốc trong phiên để kéo lại không hỏng dần.
- [x] Chỉ áo đổi thì không thêm layer "Chân dung"; "Chỉnh áo" ghi mức sáng đang xem trước.
- Test: 1 bài lõi (áo sáng / tối / đều sáng, phần trong suốt giữ nguyên), 2 bài trong app trên
  ảnh khách thật mặc một áo dựng sẵn (xem trước; tắt xem trước; Áp dụng chỉ đổi layer áo; mở
  lại kéo nửa mức ra đúng kết quả tính từ áo gốc; về 0 là áo như lúc ghép; chỉnh cả người lẫn
  áo là một bước Ctrl+Z; "Chỉnh áo" giữ mức sáng đang xem), bảng (hai thanh sáng bật, "Nét
  áo" mờ, dòng ghi chú).
  Lệnh xem ảnh thử: `IAI_GARMENT_LIGHT_PROBE=<thư mục layer áo .png>` với
  `cargo test --lib -- --ignored probe_garment_light --nocapture` (layer áo lấy bằng
  `IAI_GARMENT_LAYERS=1` của lệnh `probe_dressed_photos`).
- [x] Chủ test 06/10: **"đã test ok"**, kèm một chỗ vấp → đợt 37.

### Đợt 37 — Chỉnh tiếp sau khi sửa tay, không làm lại ảnh thẻ

Làm 06/10, ngay sau lời chủ. Nhật ký thao tác của app ("hộp đen", phiên 13:47) cho thấy đúng
chỗ vấp: ở ô **Ảnh thẻ**, sau "Làm ảnh thẻ tự động" app mặc áo và xem trước phần chỉnh chân
dung; chủ vừa đụng công cụ khác (chọn layer, Smudge, Move, Eraser…) là app áp dụng phần đang
xem (luật đợt 27) và các thanh khóa lại. Ở ô Ảnh thẻ lúc đó chỉ còn nút "Làm ảnh thẻ tự động"
— bấm là làm lại từ đầu, mất phần sửa tay. (Nút "Tự động làm đẹp" có sẵn bên ô **Chân dung**
làm được việc này, nhưng phải đổi ô mới thấy.)

- [x] Ô Ảnh thẻ có thêm nút **"Chỉnh tiếp ảnh này"**, ngay dưới "Làm ảnh thẻ tự động". Nút hiện
      khi ảnh đang mở đã ghép áo hoặc đã có layer "Chân dung", và không có việc gì đang chạy.
      Bấm là các thanh mở lại trên ảnh đang có: **không làm lại ảnh thẻ, không ghép lại áo**.
      Ảnh đã sửa tay thì các thanh bắt đầu từ 0 (luật đợt 25) — kéo "Da cổ" rồi "Áp dụng".
- [x] Ảnh đã ghép áo: dù layer đang chọn là "Áo" (vừa "Chỉnh áo" / Ctrl+T xong) hay "Tóc trên
      áo", "Chỉnh tiếp ảnh này" và "Tự động làm đẹp" đều tự nhắm vào layer đang hiện người
      (layer "Chân dung" mới nhất, hoặc "Người") — trước đây app báo "không tìm thấy khuôn mặt".
- [x] Sửa kèm: ảnh ghép áo không phải ảnh vừa ghép sau cùng trong phiên (chủ làm nhiều tab), sau
      khi đã áp dụng chỉnh chân dung thì layer "Người" bị ẩn và app **không còn nhận ra ảnh đó
      đã ghép áo** ("Chỉnh áo", "Bỏ áo", hai thanh sáng áo ghép không dùng được). Nay app nhận
      ra miễn là còn một layer của người đang hiện bên dưới layer "Áo".
- Test: bảng (nút hiện đúng lúc, bấm thì xin chỉnh tiếp chứ không xin làm ảnh thẻ), nhận ra
  ảnh ghép áo có layer "Người" ẩn dưới layer chỉnh, 1 bài trong app trên ảnh khách thật (áp
  dụng chỉnh → sửa tay layer "Chân dung" → chọn layer "Áo" → chỉnh tiếp: phiên mở đúng trên
  layer "Chân dung", các thanh ở 0, áo ghép vẫn thuộc phiên).
- Còn nguyên (đúng luật đợt 27, chủ đã duyệt): mỗi lần đụng công cụ ngoài bảng thì phần đang
  xem được áp dụng và các thanh khóa lại; muốn kéo tiếp thì bấm "Chỉnh tiếp ảnh này" (chờ app
  nhận diện lại vài giây).
- [x] Chủ test 06/10 tối trên `target\release\iai.exe` (build 15:35): **"đã test ok"**.

### Đợt 38 — Ô "Áo" tiện hơn (làm 06/10 tối — CHỦ TEST OK)

Ba việc chủ nêu 06/10 tối, đều ở hàng "Áo" của ô Ảnh thẻ (`garment_row` trong
`src/ui/dialogs/id_photo.rs`, app `src/app/garment_ops.rs`):

- [x] **Nút "Mở file áo"** (có hình thư mục) ở hàng Áo: mở hộp chọn file, chọn một hay nhiều
      file áo của tiệm → mỗi file mở thành một tab; file đang mở sẵn thì app chỉ nhảy tới tab
      đó. Hộp chọn file mở sẵn **thư mục của file áo dùng lần trước**. App nhớ 12 file áo gần
      nhất qua các lần mở app (`prefs.json`, mục `garment_sheets`). Bấm nút mà hủy hộp chọn
      file thì không có gì đổi.
- [x] **Sau "Lấy áo đang chọn" (hay kéo thả áo vào ô) app quay về đúng ảnh**: ảnh **vừa xem
      ngay trước khi sang tab file áo** — ảnh vừa mở chính là ảnh đó. Gốc lỗi cũ (nhật ký 06/10
      lúc 14:58:09): chủ vừa mở ảnh ở tab 3 rồi sang file áo, bấm "Lấy áo đang chọn" thì app
      mặc áo lên **tab 1** vì app ưu tiên "ảnh đã mặc áo lần trước" → đã bỏ luật ưu tiên đó.
      Ảnh quay về đã tách nền (đã làm ảnh thẻ) thì mặc áo ngay như trước; ảnh mới mở, chưa
      làm ảnh thẻ thì app nhảy về và nhắc "Bấm Làm ảnh thẻ tự động — áo sẽ được mặc luôn".
      App bỏ qua các tab không phải ảnh khách: file áo (đã từng lấy áo ở đó, hoặc mở bằng nút
      "Mở file áo", kể cả ở lần mở app trước), trang xếp ảnh in (các ảnh in nằm trong nhóm),
      file trên 16 layer chưa mặc áo, file chữ và PDF.
- [x] **Nút "Đổi áo khác"** (hiện khi ô đang giữ áo hoặc ảnh đang mặc áo): nhảy sang tab file
      áo xem gần nhất. File áo đã đóng thì app mở lại file đó; chưa nhớ file nào thì mở hộp
      chọn file. Chọn áo khác xong bấm "Lấy áo đang chọn" là app quay về ảnh vừa rời.
- Việc tôi tự quyết: (1) "ảnh mới mở gần nhất" = ảnh **được xem gần nhất** không phải file áo
  (ảnh vừa mở là ảnh vừa xem; chủ bấm sang một ảnh cũ rồi mới sang file áo thì áo vào ảnh
  cũ đó — đúng với cái chủ đang nhìn). (2) Vẫn mặc áo ngay khi quay về. (3) Mở nhiều file
  áo thì "Đổi áo khác" về file áo xem gần nhất. (4) Lúc sang file áo app tự cầm công cụ
  **Move** để bấm chọn áo (đang cầm Smudge / Eraser mà bấm vào file áo sẽ làm hỏng áo mẫu);
  quay về ảnh thì vẫn là Move. (5) "Đổi áo khác" rời ảnh như bấm sang tab khác: phần chỉnh
  chân dung đang xem trước được áp dụng (luật đợt 27); "Mở file áo" chỉ áp dụng khi chủ đã
  chọn file xong. (6) Hai nút mờ đi trong lúc đang làm ảnh thẻ / đang mặc áo.
- Hàng Áo nay có hai dòng nút: dòng trên là việc với cái áo đang có ("Chỉnh áo", "Đổi áo
  khác", "Bỏ áo"), dòng dưới là lấy áo ("Mở file áo", "Lấy áo đang chọn").
- Test: 7 bài mới trong `garment_ops.rs` (áo vào ảnh xem gần nhất chứ không vào ảnh đã mặc
  áo trước đó; ảnh chưa tách nền chỉ được nhảy về, áo chờ trong ô; bỏ qua file áo / trang in
  / file nhiều layer; "Đổi áo khác" sang đúng file áo rồi lấy áo thì quay về ảnh; ảnh đã mặc
  áo không bị nhận nhầm là file áo; nhớ file áo theo thứ tự mới nhất trước; file áo mở từ ô
  được nhận ra và mở lại khi đã đóng), bài bảng (hai nút mới hỏi đúng việc, mờ đúng lúc), bài
  luật đợt 27.
- [x] Chủ test 06/10 tối (bản build 18:28): **"đã test ok"**. Nhật ký 20:07–20:09: "Mở file áo"
      mở `Ao Nu.psd` thành tab mới → "Lấy áo đang chọn" quay về đúng ảnh và mặc áo (tab trang
      xếp ảnh in nằm giữa được bỏ qua) → "Đổi áo khác" sang lại tab file áo → lấy áo khác,
      quay về ảnh; ảnh mới mở chưa làm ảnh thẻ thì áo chờ trong ô và được mặc sau "Làm ảnh
      thẻ tự động".

Sửa kèm trong buổi 06/10 (không thuộc việc thay áo): sau khi đóng hộp "Print Settings…" của
máy in TOSHIBA, mọi công cụ bỏ qua chuyển động chuột (khung crop "đơ") tới khi chuyển cửa sổ
khác rồi quay lại — driver đóng hộp mà không trả bàn phím cho cửa sổ app. Đã sửa (`df37cfc`,
`0294a77`), chủ test OK 19:12.

### Đợt 39 — Viền áo khớp với da (CHƯA CÓ THIẾT KẾ — làm ảnh thử trước)

Điều chủ thấy: áo ghép "nhìn khá giả trân — chỉ là một layer chồng bên trên chứ chưa thật sự
liên kết như mặc áo thật". Điều chủ ước: sau khi tự chỉnh tay xong (dời / xoay áo, Smudge cổ,
tô da), có tính năng **tự động chạy lại phần viền áo cho khớp với da**. "Da cổ" thì chủ đánh
giá tạm ổn.

Chưa biết cách nào cho ra kết quả thật mắt → theo lệ các đợt, **làm ảnh thử trước rồi mới
viết code**. Các hướng tôi định thử (ý của tôi, chủ chưa duyệt):

1. **Bóng tiếp xúc**: bóng mềm của mép cổ áo đổ lên da cổ (và của cằm lên áo), hướng theo ánh
   sáng đo được trên mặt — thứ làm áo "nằm trên người" rõ nhất. Hiện app chỉ làm sậm nhẹ da ở
   vành khoảng hở cổ lúc mặc áo (`RIM_SHADE` trong `core/garment.rs`), không theo vị trí áo
   sau khi chủ dời / xoay.
2. **Mép áo**: mép layer áo đang sắc như cắt giấy → làm mềm theo độ nét của ảnh, bỏ viền sáng
   / viền nền còn dính ở mép áo của file tiệm.
3. **Khớp sáng và màu giữa áo và người**: áo studio sáng đều, ảnh khách lệch sáng / ám màu →
   đưa hướng sáng và tông của áo về gần ảnh (đã có sẵn phép đo độ dốc sáng `light_of` và hai
   thanh sáng áo ghép để tận dụng).
4. **Da tô dưới mép áo** tính lại theo vị trí áo hiện tại (sau khi áo bị dời, chỗ hở ra đang là
   dải da sậm phẳng).

Câu hỏi cho chủ khi bắt đầu đợt này (không chặn việc làm ảnh thử): chỗ nào làm chủ thấy "giả"
nhất — mép áo sắc quá, thiếu bóng, hay lệch sáng / lệch màu? Cho xin 2–3 ảnh chủ đã chỉnh tay
xong mà vẫn thấy giả để thử đúng ca.

Nút bấm dự kiến: một nút / một thanh trong menu "Áo" (ví dụ "Khớp viền áo"), chạy lại được
sau mỗi lần chủ chỉnh tay, dùng chung đường "Chỉnh tiếp ảnh này". Áo vẫn là layer riêng để
"Chỉnh áo" tiếp được.

### Để sau, chỉ làm khi chủ bảo

- App tự tô da cổ đẹp hơn ngay lúc mặc áo (cổ áo cũ che cổ, áo cổ sâu thiếu xương đòn).
- Trẻ em: tự thu nhỏ vai áo theo người.
- Đổi màu tóc sau khi mặc áo thì layer "Tóc trên áo" đổi theo.

## 8. Việc cần chủ quyết

1. ~~Menu "Áo" trước hay "Da cổ" trước?~~ Chủ chốt 06/10: nét áo trước (đợt 33), rồi đợt 34,
   35, 36 — đều đã test OK.
2. Cho tôi đường dẫn ảnh mẫu để thử cho sát thực tế: vài **ảnh khách tự chụp điện thoại, áo
   mờ**, và 2–3 **ảnh cũ phục hồi** — "Nét áo" mới thử trên ảnh của một khách.
   ~~File `.iai` đã mặc áo và đã sửa tay chỗ cổ~~: chủ trả lời 06/10 là không có → "Da cổ" thử
   trên layer "Người" thật và ảnh giả lập sửa tay; chủ thêm yêu cầu cọ tô vùng da cổ (đã làm).
3. ~~"Sáng áo" và "Đều sáng áo" có cần tác động lên áo ghép không?~~ Chủ chốt 06/10: **có** →
   đợt 36 (đã làm).
4. ~~Có đồng ý thêm một model cho nét áo không?~~ Không phải thêm: model đã có sẵn trên máy.
5. Hôm 05/10 chủ nói quy trình thay áo "vẫn có 1 vài điểm bị vấp" — hai việc này đã hết các
   điểm đó chưa?

## 9. Rủi ro và điều chưa biết

- **"Nét áo" mới thử trên ảnh của một khách** (áo thun có chữ thêu) và hai ảnh áo trắng. Vải
  hoa văn nhỏ, áo dài, vest sọc, bảng tên chữ rất nhỏ chưa thử — AI loại này có thể vẽ sai
  họa tiết nhỏ. Ảnh mờ quá nặng thì vẫn nên đưa qua AI Image Studio.
- Việc "đo độ nhòe mép" để chọn cỡ mới kiểm bằng ảnh làm nhòe nhân tạo, chưa có ảnh cũ thật.
- Ảnh thẻ mở lại ở phiên sau (app không còn nhớ đã phóng lớn bao nhiêu) thì "Nét áo" chạy ở
  cỡ nguyên: vẫn an toàn nhưng hiệu quả nhẹ hơn lúc vừa làm ảnh thẻ xong.
- Ảnh rất lớn (áo chiếm trên khoảng 600 nghìn điểm ảnh) bị giới hạn cỡ cho AI xem để không
  chạy quá lâu.
- "Da cổ" đã thử trên 5 ảnh thật và 1 ảnh giả lập sửa tay (mục 6.5); kết quả phụ thuộc cỡ
  khung (80% được chọn). Người cổ dài, áo cổ rất sâu, mặt nghiêng chưa thử: phần cổ xa hơn
  khoảng 0,85 chiều cao mặt dưới cằm nằm ngoài tầm nhìn của model.
- "Da cổ" ở 100 có thể làm cổ phụ nữ hơi sần; thanh là để kéo vừa mắt (mức 50 ổn trên ảnh thử).
- Ảnh sửa tay thật của chủ chưa có: mức san mảng loang (do Clone / Smudge) mới chỉnh theo ảnh
  giả lập.
- Vân da AI vẽ là vân "hợp lý", không phải đúng từng lỗ chân lông cũ của khách ở chỗ đó. Model
  đôi khi thêm vài chấm nhỏ như nốt ruồi; thanh thấp thì ít thấy.
- Khoanh vùng áo mới thử trên ảnh dễ. Áo tối trên nền tối, áo trùng màu nền, khăn quàng có thể
  khoanh thiếu — vì vậy có "Tô vùng ▸ Áo".
- "Đều sáng áo" chỉ bù **một độ dốc đều** trên cả cái áo (tối dần sang một bên, tối dần xuống
  dưới). Bóng đổ cục bộ (bóng cằm trên cổ áo, bóng tay) không được sửa. Mới thử trên ảnh tôi
  cố ý làm tối một bên, chưa có ảnh khách bị lệch sáng thật.
- Ảnh chụp đèn từ trên xuống thì áo vốn sậm dần xuống dưới: "Đều sáng áo" sẽ nâng phần dưới
  lên — đúng ý với ảnh thẻ, nhưng kéo hết thanh có thể làm áo trông phẳng.
- "Da cổ" cần app tìm lại được mặt sau khi chủ sửa tay — bình thường không vấn đề vì chủ chỉ
  sửa ở cổ.

## 10. Ghi chú kỹ thuật cho phiên sau

- Nhãn Sapiens2 (`src/core/ai/body_parts.rs`, `CLASS_NAMES`): 23 "Áo", 13 "Quần/váy", 1 "Phụ
  kiện", 22 "Thân" (da trần), 3 "Mặt + cổ" (không tách riêng cổ → vùng cổ cắt bằng đường hàm
  của face mesh). Đồ đang mặc = những lớp không thuộc nhóm nào trong 8 nhóm của `PART_GROUPS`
  (áo, quần / váy, phụ kiện, giày, tất) → `PartLabels::worn_at` = 1 − tổng các nhóm, không
  phải thêm nhóm; `worn_cut_off` cho biết áo có chạy ra ngoài khung đã nhìn không.
- **Đã làm ở đợt 33** (`portrait/clothes.rs`): `FaceModel.parts` giữ nhãn quanh mặt,
  `FaceModel.clothes: OnceLock<Result<ClothesDetail, String>>` làm lười như `ai_detail`;
  `analyze_clothes` = [nhìn cả người bằng `segment_body` nếu áo bị cắt] → `clothes_mask` (trừ
  mask da và tóc, chia theo mặt gần nhất) → `restore` (thu về cỡ `shown_scale`, chạy
  `Upscaler`, trải lại cỡ ảnh, trả tông của ảnh ở dải `TONE_SIGMA`) → `ClothesDetail::lay`
  trong `effects::retouch`. `Upscaler` (`core/ai/retouch.rs`) thử lần lượt x2plus →
  general-x4v3 → x4plus, tự đọc hệ số phóng từ đầu ra, chạy CPU. Mức phóng của ảnh thẻ:
  `Kept.enlarged` → `App::id_photo_enlarged` → `PortraitSession.enlarged`.
- **Đã làm ở đợt 34**: `ClothesArea` (vùng áo trên cả vùng nhãn đã nhìn + `bounds` + `Light`)
  nằm ở `FaceModel.clothes_area` (làm lười bằng `analyze_clothes_areas`, gọi khi một thanh áo
  rời nghỉ, khi tích "Hiện vùng nhận diện", khi cọ chọn Áo, hoặc khi layer mở lại có vùng áo
  đã tô); `ClothesDetail` chỉ còn ảnh AI, vẽ rộng hơn `bounds` 0,25 cỡ mặt để phần cọ tô thêm
  gần đó cũng có nét. `FaceEdits.clothes: Option<Arc<ClothesArea>>` (`with_mask` đo lại sáng),
  `MaskTarget::Clothes`, `SavedFace.clothes` + mục `{f}_clothes` trong `.iai`. App:
  `PortraitSession.area_rx`, `start_clothes_search`, `PortraitBrush.kept_clothes` +
  `App::clothes_found` (đặt vùng áo đã lưu lên vùng vừa tìm). `ClothesArea::lay` làm cả ba
  việc theo thứ tự: nét → đều sáng → sáng / tối.
- "Đều sáng áo" (`light_of`): **trung vị độ dốc** của ln độ sáng tuyến tính giữa các ô lưới
  thô đầy áo (bỏ qua mép vải vì chúng là thiểu số, nếp gấp nghiêng cả hai phía). Đã thử và bỏ:
  chia vải thành hai lớp theo độ sáng (Otsu) rồi khớp mặt phẳng chung — một bên khuất đèn bị
  coi là "vải khác" và nuốt mất độ dốc. Mức "được chiếu" = phân vị 75 của mặt phẳng; nâng 0,9,
  hạ 0,35, giới hạn ±0,9 ln.
- "Sáng áo" (`relit`): tối = nhân ánh sáng tuyến tính; sáng = 1 − (1 − y)^(2^khẩu) trên độ
  sáng, giữ tỷ lệ kênh màu. Không dùng Midtones của Develop như "Sáng da": áo trắng (phổ biến
  nhất ở ảnh thẻ) gần như không nhúc nhích với Midtones.
- Thanh trong `PortraitSettings` (đều `#[serde(default)]`, `NEUTRAL` và mặc định = 0, có trong
  `unit()`): `clothes_sharpen`, `clothes_brightness`, `clothes_even` (đã có); còn `neck`
  ("Da cổ").
- Số đo "Nét áo" 06/10 (CPU máy tiệm, vùng áo 1043×650 của ảnh thẻ): general-x4v3 4,7 s ở cỡ
  nguyên / 1,2 s ở nửa cỡ; x2plus 15,5 s / 3,6 s; x4plus nửa cỡ 15,4 s. Bài học: cho xem ở nửa
  cỡ một ảnh còn nguyên cỡ gốc làm hỏng chữ cao ~12 px; tỷ lệ năng lượng các dải tần không
  tách được ảnh phóng lớn khỏi ảnh nét (đã thử, bỏ) → dùng mức phóng app biết + đo độ nhòe mép
  (`edge_blur`: tỷ số gradient mạnh nhất qua hai mức blur). Model ONNX ở
  `models/realesrgan` của repo (không vào git); giấy phép BSD-3. Nếu chữ bảng tên bị méo:
  loại lớp "Phụ kiện" khỏi vùng AI.
- **"Da cổ" — đã làm ở đợt 35** (`portrait/neck.rs`): `FaceModel.neck:
  OnceLock<Result<NeckDetail, String>>` làm lười bằng `analyze_necks` (một lượt
  `FaceRestorer::restore_framed`, khung = `framing(close, FACE_SHARE 0,8, TOP 100)` với `close`
  từ `FaceRestorer::framing(landmarks)`; không chạy lượt của mặt). `NeckDetail` = `area`
  (hình học trên vùng da: ngoài đường viền mặt `FACE_OVAL`, dưới hàng 350–390 của khung
  thường, trong tầm khung cổ) + `Frame` (dùng chung với `ai_detail.rs`). Trọng số cổ tự động =
  `area × skin.interior` (`found`), đã tô = mask của cọ (`FaceEdits.neck`, trên vùng da).
  Trong `effects.rs`: `skin_colour` (phần da của `retouch_pixel` tách ra) chạy hai lượt ở điểm
  ảnh cổ — thường, rồi có cổ — và phần chênh được cộng **không nhân mask da** (nên cọ thêm được
  ngoài vùng da). Có cổ = `on_neck` (đều màu thêm `NECK_TONE` 0,7, đều sáng thêm `NECK_LIGHT`
  0,6), san dải giữa `low2` và `broad` (`NECK_LEVEL` 0,7), đổi chi tiết bằng `NeckAt::swap`
  (ghìm ở `SWING` 0,05); chỗ "Chi tiết mặt (AI)" đã đổi dưới cằm chỉ lấy phần còn lại
  (`DetailAt.cover`). App: `PortraitSession.neck_rx`, `start_neck_analysis`, `lacks_neck`,
  `portrait_neck_note`, `key.4` bit 8; cọ: `MaskTarget::Neck`, `neck_found` (tạo `MaskPaint`
  từ `NeckDetail::mask` khi cổ được tìm), `kept_neck`; lưu: `SavedFace.neck`, mục `{f}_neck`.
- **Áo ghép — đã làm ở đợt 36**: `LaidGarment::read / relit` (đo độ dốc sáng bằng
  `ClothesArea::new` với mask = alpha, rồi `relit` từng điểm ảnh có alpha). Phiên giữ
  `WornGarment` (`original` để trả lại khi hủy, `base` = áo trước mọi lần chỉnh, `start` =
  mức đang nằm trong layer, `shown`); render xem trước tính áo cùng luồng với người và trả
  `(Rendered, RelitGarment)`; layer "Người" nhận `settings.without_clothes()`. Bản gốc giữ ở
  `GarmentSession.relit: HashMap<(DocumentId, layer), Relit { base, look, made }>` — `made` là
  `content_hash` của layer sau khi chỉnh, lệch là coi như áo mới (đã dời / xoay / sửa tay).
  `apply_portrait` trả `Applied::{Added, Updated, Garment}`; `settle_garment_light` (gọi từ
  `adjust_garment`) ghi mức đang xem thành một bước riêng rồi thả áo, `take_garment_again`
  nhận lại khi hết Free Transform. Bản gốc không lưu vào `.iai`.
- Luật thanh về 0 cho layer đã sửa tay: `begin_portrait` (`retouched` → `PortraitSettings::
  NEUTRAL`), `reopen_target` / `as_made` trong `portrait_ops.rs`.
- Thử nhanh ngoài app (06/10, Python + onnxruntime): Sapiens2
  `%APPDATA%\iAi\models\sapiens2-seg\…512x384.onnx` (đầu vào `pixel_values` 1×3×512×384, chuẩn
  hóa ImageNet); model mặt `%APPDATA%\iAi\models\gfpgan\RestoreFormer_PP.onnx` (đầu vào `input`
  1×3×512×512 trong −1..1, đầu ra thứ nhất −1..1). Khung thử: mắt trái / phải về (193, 240) /
  (319, 240) × hệ số cỡ, rồi dời theo chiều dọc.
- Test: `app::portrait_ops` chạy riêng `--test-threads=1`.
- **Đợt 38** (`garment_ops.rs`): `photo_to_dress` = tài liệu đầu tiên trong `doc_mru` (trừ file
  nguồn) mà `may_be_dressed`: đang mặc áo, hoặc không quá `MOST_PHOTO_LAYERS` (16) layer,
  không có nhóm, không phải `is_garment_sheet`, không phải file chữ / PDF / nhiều trang. Luật
  cũ "ưu tiên `GarmentSession.worn`" đã bỏ. File áo được biết qua `GarmentSession.sheets` (id
  tài liệu đã lấy áo, `note_garment_sheet` — ảnh đang mặc áo thì không ghi) và `sheet_files`
  (`OnceCell<SheetFiles>`: đọc `prefs.json` mục `garment_sheets` khi cần lần đầu, khóa so sánh
  là `normalized_path_key`). `pick_garment_sheets` → `FileDialogResult::OpenedGarmentSheets` →
  `open_garment_sheets` (`yield_portrait`, nhớ file, cầm Move, `start_load_paths`);
  `change_garment` → `open_garment_sheet` (file áo đang mở, theo `doc_mru`) / mở lại file /
  hộp chọn file. `DialogIntent.open_garment_sheet` không vượt qua phiên chỉnh chân dung (chưa
  mở gì), `change_garment` thì có. `load_pref` / `save_pref` của `ui/dialogs.rs` nay
  `pub(crate)` để phần app dùng. Bài test mở file thật qua `poll_loads` sẽ ghi vào
  `catalog.json` của chủ → các bài ở đây không gọi `poll_loads`.
