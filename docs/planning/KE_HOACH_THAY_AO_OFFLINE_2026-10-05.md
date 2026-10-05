# Kế hoạch: thay áo cho ảnh thẻ, chạy offline (05/10/2026)

**Trạng thái: CHỜ CHỦ DUYỆT.** Phiên 05/10 chỉ tìm hiểu và lập kế hoạch, chưa viết dòng code nào.

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

## 5. Các đợt (chờ duyệt)

Mỗi đợt xong đều build Release cho chủ test như lệ thường.

### Đợt 31 — Thử nghiệm, chưa đụng giao diện (1 phiên)

Mục đích: có ảnh thật cho chủ xem trước khi bỏ công làm giao diện.

- [ ] **Thử hướng A**: dựng bản thử ghép áo (chỉ chạy bằng lệnh thử, chưa có nút) trên 5–10 ảnh
  thẻ thật với 3 áo mẫu (sơ mi, vest + cà vạt, áo dài). Gửi chủ ảnh trước / sau.
- [ ] **Đo hướng B**: tải stable-diffusion.cpp + FLUX.2 klein 4B bản nén (model khoảng 3 GB, bộ
  đọc lời tả khoảng 3 GB, bộ giải mã ảnh khoảng 0,3 GB — tải từ GitHub và Hugging Face vào thư
  mục riêng ngoài kho mã), chạy trên chính các ảnh đó. Báo chủ: mỗi ảnh mất bao lâu, ảnh ra
  thế nào, tỷ lệ phải chạy lại.
- Cần từ chủ: một thư mục 5–10 ảnh thẻ (dùng lại `C:\Users\Admin\Downloads\ht` nếu chủ đồng ý)
  và vài ảnh đã thay áo bằng Gemini / ChatGPT mà chủ ưng (hoặc bộ phôi áo có sẵn) để lấy áo mẫu.
- Đạt khi: chủ xem ảnh và chọn làm tiếp A, A + B, hay dừng.

### Đợt 32 — Thư viện áo và "Lưu áo từ ảnh"

- [ ] Thư mục thư viện áo trên máy (nhóm: Sơ mi / Vest / Áo dài / Khác), xem dạng ô ảnh nhỏ.
- [ ] Nút "Lưu áo từ ảnh" (ảnh đang mở): app tự cắt áo, tự tìm điểm cổ – vai, hỏi tên và nhóm.
  Dùng được cho ảnh Gemini / ChatGPT vừa làm xong lẫn file PNG có sẵn.
- [ ] Xóa, đổi tên áo trong thư viện.

### Đợt 33 — Ghép áo vào ảnh thẻ

- [ ] Hàng "Áo" trong ô Ảnh thẻ: "Giữ áo gốc" | "Chọn áo…" (mở thư viện).
- [ ] App tự đặt áo theo cổ – vai, áo thành layer "Áo" riêng; đổi áo khác thì thay tại chỗ.
- [ ] Xóa phần áo cũ lòi ra ngoài áo mới; tóc xõa trước vai nằm trên áo; bóng nhẹ dưới cằm;
  độ sáng áo khớp ảnh.
- [ ] Nút chỉnh tay: lên / xuống / trái / phải, to / nhỏ, xoay (như hàng "Khung"), "Đặt lại".
- [ ] Cùng chạy được với "Xếp ảnh in" và "Xếp cả thư mục" như ảnh thẻ thường.

### Đợt 34 — Hoàn thiện

- [ ] Áo cổ thấp, hoặc áo cũ cổ cao che cổ: vẽ da cổ / ngực theo đúng màu da của khách.
- [ ] Đổi màu áo, màu cà vạt ngay trên áo mẫu (một áo mẫu ra nhiều màu, đỡ phải lưu nhiều áo).
- [ ] Trẻ em: tự thu nhỏ vai áo theo người.

### Đợt 35 — (Tùy chọn, chỉ làm nếu chủ duyệt sau đợt 31) "Thay áo bằng AI — offline"

- [ ] Gói tải thêm khoảng 6 GB, không nhét vào bản portable mặc định.
- [ ] Nút chạy nền có thanh tiến độ và Hủy; trong lúc chạy vẫn làm việc khác được trong app.
- [ ] Chỉ lấy phần áo dán lại; mặt và tóc giữ nguyên. Kết quả ưng thì "Lưu áo từ ảnh" được luôn.

## 6. Việc cần chủ quyết

1. Duyệt đề xuất "A làm chính, B đo thử trước" không?
2. Chủ có sẵn bộ phôi áo (PNG / PSD) hay ảnh đã thay áo bằng Gemini mà ưng không? Nếu có, cho
   đường dẫn thư mục; nếu chưa, đợt 31 tôi sẽ nhờ chủ làm 3–5 ảnh bằng Gemini lúc có mạng.
3. Tiệm có tính nâng card màn hình (≥ 8 GB) không? Nếu không thì hướng B gần như chỉ là "chữa
   cháy vài phút một ảnh"; nếu có thì B (và cả FASHN VTON 1.5) đáng làm nghiêm túc.

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
