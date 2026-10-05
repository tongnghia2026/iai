# Kế hoạch: thay áo cho ảnh thẻ, chạy offline (05/10/2026)

**Trạng thái: CHỦ ĐÃ DUYỆT 05/10, đang làm.** Đợt 31 (app tự căn áo + ảnh ghép thử) xong phần
code, **chờ chủ xem ảnh ghép thử** rồi mới sang đợt 32. Cách làm chủ chốt: mở sẵn file áo
Photoshop của tiệm, kéo áo vào khung "Áo", app tự căn — **không dùng model tạo ảnh** (mục "Kho
áo có sẵn của chủ" và mục 5); phần tìm model ở mục 3 giữ lại làm tư liệu.

## 1. Việc chủ giao

Lời chủ 05/10: "lên kế hoạch nâng cấp thêm tính năng thay áo cho ảnh thẻ; khách tới chụp mà có
nhu cầu thay áo sơ mi, áo dài, vest,.... thì mình cũng làm offline được luôn; tại vì dùng gemini
và chatgpt gặp hôm rớt mạng làm không được; tìm xem có model nào phù hợp không; báo cáo, lên kế
hoạch cho tôi duyệt; không code trong phiên này".

Hiện nay thay áo chỉ có ở bảng AI Image Studio (gửi ảnh lên Gemini / ChatGPT kèm lời tả: sơ mi 5
màu, áo dài 6 màu, vest 3 màu + cà vạt). Ô Ảnh thẻ của bảng Auto retouch (chạy offline) chưa có.

## 2. Máy của tiệm (đo 05/10)

- CPU Intel i5-13400F (10 nhân, 16 luồng; dòng "F" không có card đồ họa tích hợp), RAM 32 GB.
- Card màn hình GTX 1050, **2 GB**. Ổ C còn trống 147 GB.

Hệ quả: các model "tạo ảnh" cần card từ 8 GB trở lên. Ở máy này chúng chỉ chạy được bằng CPU,
tức là chậm (tính bằng phút, không phải giây).

## 3. Kết quả tìm model

| Model | Làm được gì | Giấy phép (iAi có bán bản build) | Cần máy | Trên máy tiệm | Kết luận |
|---|---|---|---|---|---|
| **FLUX.2 klein 4B** (Black Forest Labs, 01/2026) | Sửa ảnh theo lời tả, nhận thêm ảnh áo mẫu làm tham chiếu; 4 bước | Apache-2.0 — dùng thương mại được | Card 8 GB: khoảng 15–30 giây / ảnh 1024² (theo bài hướng dẫn, chưa tự đo) | Chỉ CPU: **ước 3–8 phút / ảnh, CHƯA ĐO** (xem mục 7) | Ứng viên duy nhất đáng thử cho "AI tạo ảnh offline" |
| **FASHN VTON 1.5** (01/2026) | Chuyên "mặc thử": ảnh người + ảnh áo → người mặc áo đó | Apache-2.0. Bộ tách người đi kèm lại dùng giấy phép NVIDIA SegFormer (không hợp bản bán) — phải thay bằng bộ tách iAi đã có | Card khoảng 8 GB; ảnh ra 576×864; 30 bước | CPU: ước hàng chục phút — không thực tế | Chỉ đáng xét nếu sau này có card ≥ 8 GB |
| Leffa (Meta) | Mặc thử | MIT | Card lớn (chưa tra con số chính xác) | Không chạy nổi | Loại vì máy |
| IDM-VTON, CatVTON, OOTDiffusion, FitDiT, StableVITON | Mặc thử | CC BY-NC-SA — **cấm dùng thương mại** | Card 8–16 GB trở lên | Không chạy nổi | Loại vì giấy phép |
| Stable Diffusion 1.5 inpainting (+ LCM) | Vẽ lại vùng áo theo lời tả | OpenRAIL-M — bán được, có điều kiện sử dụng | CPU chạy được | Ước 30–60 giây, chưa đo | Model đời 2022: cổ áo, cà vạt, hàng cúc hay méo; không khuyên |
| HivisionIDPhotos (app ảnh thẻ nguồn mở) | Tách nền, xếp ảnh in | Apache-2.0 | — | — | Mục thay trang phục vẫn ghi "đang phát triển" — không có gì để mượn |

Chương trình chạy model: **stable-diffusion.cpp** (MIT, có bản Windows dựng sẵn, chạy được bằng
CPU, hỗ trợ FLUX.2 klein và ảnh tham chiếu). iAi sẽ gọi nó như một chương trình phụ nằm cạnh
`iai.exe`, không trộn vào mã iAi.

**Kết luận phần tìm model:** chưa có model nào thay áo offline nhanh và đẹp như Gemini trên một
máy có card 2 GB. Có một model giấy phép sạch chạy được bằng CPU (FLUX.2 klein 4B) nhưng mỗi ảnh
mất vài phút; muốn nhanh phải có card ≥ 8 GB.

## 4. Hai hướng làm và đề xuất

### Hướng A — Ghép áo mẫu (không dùng AI tạo ảnh)

Cách các tiệm vẫn làm bằng Photoshop ("ghép phôi áo"), nhưng app tự làm:

1. Có một **thư viện áo** (ảnh PNG nền trong suốt: sơ mi, vest + cà vạt, áo dài…).
2. Sau khi "Làm ảnh thẻ tự động", chủ bấm chọn một áo. App đã biết sẵn cằm, cổ, hai vai, tóc
   (các bộ nhận diện đang dùng cho Chỉnh chân dung) nên **tự đặt áo đúng cổ – vai, đúng cỡ**.
3. Áo cũ bị che / xóa, tóc xõa trước vai vẫn nằm trên áo, có bóng nhẹ dưới cằm, độ sáng áo khớp
   với ảnh. Áo nằm trên **một layer riêng** nên nhích, phóng, xoay hay đổi áo khác đều được.

- Ưu: tức thì (dưới 1 giây), chạy trên mọi máy, không cần mạng, không tải thêm gì; kết quả ổn
  định, lần nào cũng như nhau; **mặt khách không bị đụng tới**.
- Nhược: đẹp đến đâu tùy bộ áo mẫu; chỉ hợp ảnh chụp thẳng (ảnh thẻ vốn vậy); áo cổ thấp phải
  vẽ thêm da cổ / ngực cho khớp màu da; nếp vải không "ôm" theo từng người như AI.

**Áo mẫu lấy ở đâu** — ý chính của kế hoạch: nút **"Lưu áo từ ảnh"**. Mỗi lần chủ thay áo bằng
Gemini / ChatGPT lúc có mạng mà ưng, bấm một nút là app cắt riêng cái áo trong ảnh đó, ghi kèm
điểm cổ – vai, cất vào thư viện. Thư viện lớn dần từ chính việc làm hằng ngày của tiệm, đúng
kiểu áo chủ thích; hôm rớt mạng thì dùng lại. Bộ phôi áo PNG chủ đang có (nếu có) cũng nạp vào
được bằng cùng nút đó.

### Hướng B — AI tạo ảnh offline (FLUX.2 klein 4B)

App gửi vùng thân người + lời tả (hoặc ảnh áo mẫu) cho model chạy ngay trên máy, rồi **chỉ lấy
phần áo** dán lại vào ảnh — mặt, tóc giữ nguyên điểm ảnh gốc nên không lo "ra người khác".

- Ưu: tự nhiên hơn ghép (nếp vải, vai, cổ theo đúng người); tả áo gì cũng được, không cần áo mẫu.
- Nhược trên máy tiệm: vài phút mỗi ảnh và chiếm hết CPU trong lúc chạy; phải tải thêm khoảng
  6 GB; đôi khi ra kết quả kỳ, phải chạy lại; chất lượng dưới Gemini / ChatGPT.
- Nếu sau này tiệm có card 8–12 GB: còn khoảng 15–30 giây / ảnh, lúc đó B mới thật sự tiện.

### Đề xuất

**Làm A làm đường chính** (dùng được ngay trên máy hiện tại, hôm rớt mạng vẫn giao ảnh trong vài
giây). **B chỉ đo thử trước** trên vài ảnh thật để chủ nhìn tận mắt thời gian và chất lượng rồi
mới quyết có làm hay không. Hai hướng dùng chung thư viện áo nên công làm A không bị bỏ đi nếu
sau này thêm B.

### Ý của chủ 05/10: tự huấn luyện model từ kho ảnh thẻ mẫu

Lời chủ: "tôi có rất nhiều ảnh thẻ mẫu; tôi muốn tự training 1 model chuyên ghép áo được không".
Tôi đã trả lời (chủ chưa phản hồi):

- **Huấn luyện từ số không**: không làm nổi — model mặc thử nhỏ nhất (FASHN VTON 1.5) học từ 18
  triệu cặp ảnh trên dàn máy lớn.
- **Dạy thêm cho model có sẵn (LoRA)** bằng ảnh của tiệm: làm được. Tài liệu của hãng ghi FLUX.2
  klein 4B cần card từ 12 GB và 32 GB RAM, chạy 1–3 giờ; card 2 GB của tiệm không huấn luyện
  được → phải thuê máy trên mạng vài giờ hoặc mua card. Được lợi về **chất lượng** (ra đúng kiểu
  vest, áo dài, ánh sáng ảnh thẻ của tiệm), **không được lợi về tốc độ**: chạy vẫn là model gốc,
  trên máy tiệm vẫn vài phút một ảnh.
- **Dùng kho ảnh làm thư viện áo cho hướng A, không cần huấn luyện**: app tự cắt áo từ từng ảnh
  mẫu kèm điểm cổ – vai; với khách mới, app chọn trong hàng trăm áo cái nào khớp cổ – vai nhất
  rồi mới ghép. Càng nhiều ảnh mẫu thì càng dễ có áo vừa, và chạy tức thì trên máy hiện tại.
  Đây là cách tận dụng kho ảnh có lợi nhất lúc này, và bộ áo đã cắt chính là dữ liệu cần có nếu
  sau này dạy thêm cho model.

Điều chưa biết, cần chủ cho biết: có khoảng bao nhiêu ảnh; ảnh là ảnh đã mặc sẵn vest / sơ mi /
áo dài hay có cả **cặp** "ảnh gốc khách mặc áo thường + ảnh đã thay áo" (cặp như vậy quý nhất
cho việc dạy model). Ảnh khách là dữ liệu cá nhân: nếu đưa lên máy thuê hoặc phát hành model
kèm app thì chỉ dùng phần từ cằm trở xuống (không có mặt).

### Kho áo có sẵn của chủ (05/10) — đổi hẳn điểm xuất phát

Lời chủ: "đây là kho áo tôi có sẵn; trước đây tôi dùng để ghép thủ công trong pts; có tận dụng
được cái này mà không cần AI không" — `C:\Users\Admin\Documents\D_DATA\ÁO SƠ MI GHÉP ẢNH THẺ`.

Tôi đã mở xem (chỉ đọc, không sửa gì): 494 file, 1,9 GB, trong đó 471 file Photoshop.

- **Áo**: mỗi file chứa nhiều áo, **mỗi áo là một lớp ảnh riêng trên nền trong suốt**, đã khoét
  sẵn khoảng hở cổ; RGB 8 bit, 4 kênh (có kênh trong suốt), không có lớp thông minh, lớp chữ
  hay nhóm, không lớp nào bị ẩn → đọc thẳng được. Mỗi áo rộng khoảng 600–700 px.
  - Sơ mi nam: `SO MI VIP NAM.psd` 115 áo (đã gồm 27 áo của `Áo sơ mi nam.psd`), `AO Nam.psd` ~18.
  - Sơ mi nữ: `Áo sơ mi nữ.psd` ~30 (có hai bản trùng), `Ao Nu.psd` ~6.
  - Comple nam: `Comple nam.psd` 61, `Mau comple.psd` 27, `Mau comple_nam+nu.psd` 30.
  - Comple nữ: 19. Áo dài: 64, áo dài the 8, áo bà ba 2.
  - Quân phục / ngành: `Quân phục.psd` 33, `Quan Phuc.psd` 80, `quanphuc 1…7` (có file trùng).
- **Phụ kiện**: dây chuyền, trang sức, huy chương – huy hiệu, cầu vai, khăn xếp, lông mi, ghế.
- **Tóc**: `Toc nam.psd` 45 kiểu; tóc nữ khoảng 435 file, mỗi file một kiểu (bối, dài, ngắn,
  ngang vai).
- Hai file (`Untitled-4.psd`, `z8033…-Recovered.psd`) cất lớp theo kiểu khác, chưa đọc.

Kết luận: **dùng được, và không cần model tạo ảnh nào**. Đây đúng là thư viện áo của hướng A,
tốt hơn phương án lấy áo từ ảnh Gemini: có sẵn vài trăm áo đã cắt sạch. App chỉ dùng lại bộ tìm
mặt – cổ – vai đang chạy trong "Làm ảnh thẻ tự động" để biết đặt áo ở đâu; không tải thêm gì,
không phụ thuộc card. Điểm cổ – vai của **từng áo** tìm bằng hình học từ chính hình dáng lớp
(hai đỉnh cổ áo và chỗ lõm ở giữa), không cần AI.

Điều phải nói trước:

- Áo rộng 600–700 px, ảnh thẻ app làm ra rộng 1043 px → áo phải phóng lên khoảng 1,3–1,5 lần:
  đủ cho ảnh 3×4, 4×6, nhưng soi kỹ sẽ mềm hơn mặt một chút.
- Cổ áo cũ của khách có thể lộ ra trong khoảng hở cổ của áo mới (chỗ trước đây phải tẩy tay
  trong Photoshop). App phải tự tô da ở đó — phần khó nhất, sẽ thử trên ảnh thật trước.
- Bộ phôi là hàng sưu tầm (có thư mục ghi nguồn một trang chia sẻ PSD), không rõ bản quyền:
  **kho áo chỉ nằm trên máy tiệm, không đưa vào kho mã công khai hay bản iAi phát hành**. Bản
  phát hành không kèm áo nào; ai dùng thì mở file áo của chính mình.

### Cách làm chủ chốt 05/10: mở sẵn file áo, kéo áo vào một khung, app tự căn

Lời chủ: "mẫu áo thì có quá nhiều, bây giờ tôi muốn mở sẵn file áo lên cho khách lựa chọn;
khách lựa xong chúng ta kéo mẫu áo vào 1 khung sau đó là việc của app tự động căn chỉnh được
không". → Bỏ bước "nhập kho" và "thư viện ô ảnh" tôi đề xuất trước đó: chính các file Photoshop
là bảng mẫu cho khách xem.

**Đã kiểm 05/10:** bộ đọc PSD sẵn có của app (`src/formats/psd.rs`) mở được file áo của chủ với
**đủ từng lớp**: `Áo sơ mi nam.psd` 28 lớp, `Comple nam.psd` 62, `Áo dài.psd` 65,
`SO MI VIP NAM.psd` 116; mỗi file 1–4 giây ở bản debug. Thử bằng lệnh test tạm (đã gỡ), chưa
nhìn trên cửa sổ thật. Không phải viết bộ đọc nào.

Luồng làm việc:

1. Mở file áo (ví dụ `Comple nam.psd`) thành một tab như mọi ảnh khác, để sẵn cả ngày. Khách
   nhìn cả bảng áo trên màn hình như trong Photoshop.
2. Ô Ảnh thẻ của bảng Auto retouch có thêm **khung "Áo"** ("Kéo áo vào đây"). Bảng này vốn nổi
   và vẫn mở khi đổi tab.
3. Khách chọn xong, chủ **kéo cái áo đó từ tab file áo thả vào khung "Áo"** (hoặc bấm đúp vào
   áo). File áo không bị xê dịch, không bị tính là đã sửa.
4. App tự quay về ảnh thẻ, **đặt áo đúng cổ – vai – cỡ** thành layer "Áo" riêng. Khung hiện hình
   cái áo đang dùng; thả áo khác vào thì thay tại chỗ.
5. Khung "Áo" là một lựa chọn như Nền / Mẫu / Cỡ: áo nằm trong khung được giữ cho các ảnh sau
   (cả lớp học mặc cùng một áo thì chỉ kéo một lần) cho tới khi bấm "Bỏ áo". Ảnh chưa làm thì
   "Làm ảnh thẻ tự động" mặc áo luôn; ảnh làm rồi thì thả áo là mặc ngay.

## 5. Các đợt (sửa lần hai 05/10 theo cách làm của chủ — chờ duyệt)

Mỗi đợt xong đều build Release cho chủ test như lệ thường.

### Đợt 31 — Tự căn áo, và ảnh ghép thử cho chủ xem trước

Phần quyết định được hay không là app căn có đúng không, nên làm và cho chủ xem phần này trước
khi làm khung kéo thả.

Chủ duyệt 05/10 ("ok duyệt; bắt đầu làm đi"). Làm ngay trong ngày, lõi ở `src/core/garment.rs`:

- [x] Điểm neo của áo từ hình dáng lớp (`Garment::collar`): hai đỉnh cổ áo và đáy khoảng hở
  cổ. Thử trên 5 file của chủ: tìm được ở 27/27 sơ mi nam, 30/30 sơ mi nữ, 61/61 comple nam,
  19/19 comple nữ, 64/64 áo dài.
- [x] Cổ của khách (`neck_of`): đi dọc đường viền thân từ hàm xuống, chỗ hẹp nhất là cổ, chỗ
  bắt đầu loe ra là chân cổ (hoặc mép cổ áo đang mặc) — đỉnh cổ áo mới đặt vào đó. Bên nào tóc
  che thì lấy số đo bên kia; tóc che cả hai thì lấy theo tỷ lệ khuôn mặt.
- [x] Đặt áo (`Placement::fit`): dời + phóng theo bề ngang cổ, nghiêng theo vai tối đa 4°. Áo
  có hai đỉnh cổ lệch nhau không bị coi là áo vẽ nghiêng. Áo ngắn hơn khung ảnh: kéo dài phần
  dưới cổ tối đa 25%, chưa đủ thì phóng cả áo tối đa 35%.
- [x] Mặc (`dress`): xóa áo cũ dưới đường cổ áo mới (chỉ giữ tóc); áo cũ nhô lên trên đường đó
  (đỉnh cổ áo, cổ áo khoác) cũng xóa, chỗ nằm trên cổ thì thay bằng da; khoảng hở cổ của áo mới
  được tô da lấy từ chính cổ của khách, sậm dần sát mép áo; tóc xõa nằm trên áo, mép tóc phân
  biệt với áo cũ bằng màu (nhãn tóc của model lệch vài điểm ảnh).
- [x] **Ảnh ghép thử**: 6 ảnh thẻ trong `C:\Users\Admin\Downloads\ht` × 10 áo (2 áo mỗi file),
  chạy qua đúng luồng "Làm ảnh thẻ tự động" rồi mới mặc áo. Kết quả ở `tmp\thay-ao\ket-qua\`
  (thư mục không vào kho mã): mỗi ảnh một tấm `_<tên>.jpg` gồm ảnh gốc và 10 áo, kèm từng ảnh
  cỡ đầy đủ.
- Test: 9 bài trong `core::garment` (tìm cổ áo, khoảng hở cổ, cổ người, bên bị tóc che, đặt áo
  tới đáy ảnh, mặc áo, áo cũ nhô lên, tóc phân biệt bằng màu, tô lan màu). Lệnh xem thử:
  `IAI_GARMENT_PROBE` (thư mục ảnh) + `IAI_GARMENT_PSD` (các file áo, cách nhau `;`) +
  `IAI_GARMENT_OUT` (+ `IAI_GARMENT_PICK`) với `cargo test --lib -- --ignored probe_dressed_photos`.
- Thời gian mặc một áo ở bản debug khoảng 1,4 giây (bản Release sẽ nhanh hơn nhiều; chưa đo).

Còn thô, để đợt 33:

- Da tô ở khoảng hở cổ là màu phẳng: áo cổ sâu (sơ mi nữ, comple nữ) không có xương đòn, nếp
  cổ; nhìn cỡ ảnh thẻ thì ổn, soi 100% thì thấy.
- Khách mặc áo cổ cao che hết cổ: cổ được vẽ lại bằng da phẳng, thẳng theo bề ngang cổ còn lộ.
- Áo phải phóng 1,4–2,4 lần (không phải 1,3–1,5 như tôi ước ban đầu: phôi áo vẽ cho ảnh 3×4
  ở 300 ppi, ảnh thẻ của app lớn hơn nhiều) → mép áo hơi răng cưa, vải mềm hơn mặt.
- Còn vài mẩu nhỏ quanh cổ ở ảnh khó: khe nền giữa cổ và tóc, đốm áo cũ sát đỉnh cổ áo.
- Chưa có: bóng dưới cằm, khớp sáng / màu áo với ảnh, nút chỉnh tay.
- Một áo trong `Comple nam.psd` có dòng chữ tên tiệm ảnh in sẵn ở góc dưới — của file gốc.

- Đạt khi: chủ xem ảnh ghép thử và thấy đáng làm tiếp. **Chờ chủ xem.**

### Đợt 32 — Khung "Áo" và kéo thả

- [ ] Khung "Áo" trong ô Ảnh thẻ; nhận áo kéo từ tab file áo, hoặc bấm đúp vào áo.
- [ ] Thả xong app về tab ảnh thẻ và mặc áo; layer "Áo" riêng; thả áo khác thì thay.
- [ ] File áo giữ nguyên sau khi kéo (áo về chỗ cũ, không hỏi lưu khi đóng).
- [ ] Nút chỉnh tay: lên / xuống / trái / phải, to / nhỏ, xoay (như hàng "Khung"), "Đặt lại",
  "Bỏ áo".
- [ ] Áo trong khung giữ cho ảnh sau; chạy được với "Xếp ảnh in" và "Xếp cả thư mục".

### Đợt 33 — Hoàn thiện cho giống ghép tay

- [ ] Tô da ở khoảng hở cổ khi cổ áo cũ lộ ra hoặc áo cũ che mất cổ, đúng màu da của khách.
- [ ] Bóng nhẹ dưới cằm; độ sáng và sắc màu áo khớp với ảnh; làm nét áo sau khi phóng.
- [ ] Trẻ em: tự thu nhỏ vai áo theo người.

### Để sau, chỉ làm khi chủ bảo

- Tóc và phụ kiện trong cùng kho (dây chuyền, huy chương, cầu vai, khăn xếp): kéo vào cùng
  kiểu, app tự đặt theo điểm neo.
- Thư viện ô ảnh, "Lưu áo từ ảnh" (cắt áo từ ảnh Gemini / ChatGPT): bỏ khỏi kế hoạch, chủ đã
  có đủ áo và muốn dùng thẳng file Photoshop.
- Hướng B (FLUX.2 klein 4B offline) và việc dạy thêm model: **gác**; đo thử khi chủ muốn hoặc
  khi tiệm có card ≥ 8 GB.

## 6. Việc cần chủ quyết

1. Duyệt các đợt 31–33 ở trên không?
2. Thư mục ảnh thẻ để ghép thử: dùng `C:\Users\Admin\Downloads\ht` được không, hay thư mục khác?

## 7. Rủi ro và điều chưa biết

- **Thời gian hướng B trên máy tiệm là ước lượng, chưa đo.** Cơ sở: bản chạy CPU của FLUX.2
  klein 4B (dự án iris.c) ghi 48 giây cho ảnh 512² trên Ryzen 7800X3D và 218 giây trên i5 laptop
  4 nhân, chỉ tính tạo ảnh từ lời tả. i5-13400F nằm giữa hai máy đó, và sửa ảnh có ảnh gốc làm
  tham chiếu thì khối lượng tính gấp 2–3 lần. Đợt 31 sẽ đo thật.
- Chất lượng hướng A phụ thuộc bộ áo mẫu: áo cắt từ ảnh nghiêng vai hay bị tóc che thì ghép
  không đẹp — app sẽ báo khi áo không đủ điều kiện để lưu.
- Khách mặc áo to hơn áo mới nhiều (áo khoác phồng, mũ trùm): phần lòi ra phải xóa sạch; làm
  được vì ảnh thẻ đã tách người khỏi nền, nhưng cần thử trên ảnh thật.
- Áo dài, áo cổ sen, đồng phục: FLUX.2 klein có vẽ đúng kiểu Việt Nam hay không thì chưa biết;
  hướng A không gặp vấn đề này vì áo lấy từ ảnh thật của tiệm.
- Giấy phép: FLUX.2 klein **4B** là Apache-2.0; bản **9B** cấm thương mại — không được nhầm.

## 8. Ghi chú kỹ thuật cho phiên sau

- Có sẵn để dùng lại: tách người (BiRefNet, `core/id_photo.rs` đã để người trên layer riêng),
  478 điểm mặt (`core/ai/face_mesh.rs`), 33 điểm thân gồm hai vai (`core/ai/pose.rs`), 29 lớp
  thân thể gồm áo và tóc (`core/ai/body_parts.rs`, Sapiens2), khử màu nền dính vào tóc
  (`id_photo::decontaminate`), bóp lưới (`core/portrait/reshape.rs`), LaMa, Real-ESRGAN.
- Đọc file Photoshop: **đã có sẵn** — `src/formats/psd.rs` (`PsdImporter`, bộ đọc tự viết) dựng
  lại đủ chồng lớp (tên, vị trí, độ mờ, mask, nhóm); đã thử trên 4 file áo của chủ. (Ghi chú
  trước đó của tôi "chưa có thư viện đọc PSD" là sai: tôi chỉ tìm trong `Cargo.toml`.) Hai
  file `Untitled-4.psd`, `z8033…-Recovered.psd` chưa thử.
- Kéo thả: clipboard lớp vốn dùng chung giữa các tab (`app/actions/clipboard.rs`); Move tool có
  tự chọn lớp dưới con trỏ. Kéo từ canvas vào khung của bảng nổi là việc mới: phải nhận biết
  lúc thả trên khung, hủy cú dời lớp, không làm file áo thành "đã sửa". Viết test giả lập chuột
  trước (xem ghi chú bẫy nhập liệu egui).
- File áo của chủ không vào repo, không vào `dist/iAi-portable`; áo đã thả vào khung được chép
  vào tài liệu ảnh thẻ (và nhớ trong `%APPDATA%/IAI/…` để dùng cho ảnh sau).
- Áo mẫu: PNG + tệp nhỏ ghi điểm neo (hai mép cổ, hõm cổ, hai vai) và nhóm. Đặt áo bằng phép
  đồng dạng theo cổ – vai, sau đó bóp lưới nhẹ theo độ dốc vai.
- Thứ tự lớp: nền → người (đã xóa phần thân dưới đường cổ nằm ngoài áo) → áo → tóc phía trước.
- Hướng B: gọi `sd-cli` của stable-diffusion.cpp như tiến trình con (tách khỏi app: treo hay
  lỗi không kéo app theo — bài học tiến trình kẹt lúc nạp model 04/10). Chạy trên vùng cắt
  thân + đầu khoảng 512×640, rồi phóng lại bằng Real-ESRGAN và dán theo mask áo.
- FASHN VTON 1.5: nếu dùng, bỏ `fashn-human-parser` (giấy phép NVIDIA SegFormer), thay bằng
  Sapiens2 đang có.

## 9. Nguồn (tra 05/10/2026)

- FLUX.2 klein 4B, Apache-2.0: https://huggingface.co/black-forest-labs/FLUX.2-klein-4B ;
  https://bfl.ai/models/flux-2-klein
- Thời gian chạy bằng CPU (iris.c, MIT): https://github.com/antirez/iris.c
- stable-diffusion.cpp (MIT): https://github.com/leejet/stable-diffusion.cpp
- FLUX.2 klein trên card 8 GB: https://www.promptzone.com/tara_suzuki/how-to-run-flux-on-8gb-vram-in-2026-the-gguf-low-vram-guide-46k8.md
- FASHN VTON 1.5: https://github.com/fashn-AI/fashn-vton-1.5 ;
  https://huggingface.co/fashn-ai/fashn-vton-1.5 ; https://huggingface.co/fashn-ai/fashn-human-parser
- Giấy phép các model mặc thử khác: https://fashn.ai/blog/so-you-want-to-build-a-virtual-try-on-app-a-developers-guide-to-not-getting ;
  https://github.com/yisol/idm-vton ; https://huggingface.co/BoyuanJiang/FitDiT ;
  https://huggingface.co/franciszzj/Leffa/tree/main
- HivisionIDPhotos: https://github.com/Zeyi-Lin/HivisionIDPhotos
