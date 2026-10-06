# Kế hoạch: menu "Áo" và thanh "Da cổ" (06/10/2026 — sửa lần 2 theo ý chủ)

**Trạng thái: CHỜ CHỦ DUYỆT. Chưa viết dòng code nào.** Nối tiếp
`KE_HOACH_THAY_AO_OFFLINE_2026-10-05.md` (đợt 31, 32 chủ test OK). Kế hoạch này thay cho "đợt 33"
ghi ở đó.

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
- **Model phục hồi ảnh cho áo**: thư mục `models\realesrgan` đang trống. Số đo cũ 03/10 (trên
  tóc): bản đẹp nhất khoảng 27 giây cho vùng 360×270; bản nhẹ nhanh hơn nhiều nhưng tóc bị bệt.
  Trên vải chưa thử lần nào.

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
- Cách làm — hai ứng viên, chọn bằng ảnh thử:
  1. **Bộ làm nét của Develop** chạy riêng trong vùng áo. Tức thì, không bịa chi tiết, chữ bảng
     tên và logo giữ đúng. Yếu điểm: áo mờ nặng thì chỉ nét mép, không "có lại" sợi vải.
  2. **Model phục hồi ảnh (Real-ESRGAN)**: vẽ lại chi tiết vải cho ảnh mờ — đúng loại ảnh chủ
     nêu. Chạy kiểu "Chi tiết mặt (AI)": thanh rời số 0 thì app chạy model một lần ở nền, sau đó
     kéo thanh là thấy ngay; app chỉ lấy phần chi tiết, màu và sáng tối vẫn của ảnh. Yếu điểm:
     phải thêm một file model (bản nhẹ khoảng 5 MB, bản đẹp khoảng 67 MB; giấy phép cho bán),
     chạy bằng CPU nên có thể chậm, và loại AI này hay làm méo chữ nhỏ (bảng tên, logo).
- Tôi chưa chọn trước: đợt 33 làm tấm so sánh cả hai trên **ảnh mờ thật của chủ**, ghi thời
  gian chạy thật trên máy tiệm. Nếu bản AI nhẹ vừa nhanh vừa đẹp thì dùng nó; nếu không thì
  dùng Develop, AI để dành cho ảnh mờ nặng.

### 5.3. Thanh "Sáng áo" (hai chiều) và "Đều sáng áo" (0–100)

- **Sáng áo**: trái tối hơn, phải sáng hơn — chỉ riêng áo, không đụng mặt, tóc, nền.
- **Đều sáng áo**: một bên vai tối, phần áo khuất đèn → nâng cho đều với phần được chiếu sáng.
  Chỉ sửa theo mảng lớn nên hoa văn, sọc, nếp gấp, ranh giới vest – sơ mi – cà vạt giữ nguyên.
- Với **áo ghép**: hai thanh này tác động lên layer "Áo" (chỉnh sáng áo cho hợp với mặt). Áo
  ghép không bị làm nét, không bị đổi gì khác. *Phần này làm cuối cùng và chỉ làm nếu chủ cần —
  xem mục 8, câu 3.*

### 5.4. Mặc định

Cả ba thanh bắt đầu từ **0** (áo giữ nguyên như chụp), chủ kéo khi cần — vì tăng nét áo chỉ
dùng cho ảnh mờ, ảnh cũ. "Công thức" đã lưu từ trước đọc ba thanh là 0. Sau khi dùng thật, chủ
muốn mức nào tự chạy trong "Làm ảnh thẻ tự động" thì đổi mặc định sau.

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

## 7. Các đợt

Mỗi đợt xong đều build Release và đưa đường dẫn file chạy thật cho chủ test như lệ thường.

### Đợt 33 — Ảnh thử (chưa đổi giao diện)

- [ ] Vùng áo trên ảnh khó (vest tối, áo dài, áo trùng màu nền, trẻ em): ảnh tô màu vùng áo.
- [ ] "Nét áo" trên ảnh điện thoại mờ và ảnh cũ của chủ: tấm so sánh *gốc / Develop / AI bản
      nhẹ / AI bản đẹp*, kèm thời gian chạy thật; soi riêng chữ bảng tên và logo.
- [ ] "Sáng áo", "Đều sáng áo": trước / sau ở hai mức.
- [ ] "Da cổ": trên file chủ đã sửa tay; dò cỡ khung cho vân da đẹp nhất; ba mức thanh.
- [ ] Chủ xem ảnh, chốt: "Nét áo" dùng cách nào (có thêm model không); cỡ khung của "Da cổ".

### Đợt 34 — Menu "Áo"

- [ ] Vùng áo trong phần nhận diện; "Tô vùng ▸ Áo"; màu trong "Hiện vùng nhận diện".
- [ ] Menu "Áo": Nét áo (chỉ áo mặc sẵn; mờ đi khi ảnh đã ghép áo), Sáng áo, Đều sáng áo.
- [ ] Test tự động + ảnh probe giao diện; chủ test.
- [ ] (Nếu chủ cần) Sáng áo / Đều sáng áo cho layer áo ghép.

### Đợt 35 — Thanh "Da cổ"

- [ ] Vùng da cổ (đường hàm → mép áo, trừ tóc).
- [ ] Lượt chạy riêng của model "Chi tiết mặt (AI)" cho cổ, chạy nền một lần khi thanh rời 0.
- [ ] Cân sáng vùng cổ; thanh "Da cổ" trong menu "Da"; ghi thời gian chạy vào "hộp đen".
- [ ] Test tự động (có bài kiểm mặt không đổi điểm ảnh nào); chủ test.

Đợt 34 và 35 không phụ thuộc nhau. Thanh "Da cổ" gọn hơn và model đã có sẵn — chủ muốn có
trước thì đổi thứ tự được.

### Để sau, chỉ làm khi chủ bảo

- App tự tô da cổ đẹp hơn ngay lúc mặc áo (cổ áo cũ che cổ, áo cổ sâu thiếu xương đòn).
- Trẻ em: tự thu nhỏ vai áo theo người.
- Đổi màu tóc sau khi mặc áo thì layer "Tóc trên áo" đổi theo.

## 8. Việc cần chủ quyết

1. Duyệt ba đợt 33–35 không? Menu "Áo" trước hay thanh "Da cổ" trước?
2. Cho tôi đường dẫn ảnh mẫu để thử cho sát thực tế:
   - 3–5 **ảnh khách tự chụp điện thoại, áo mờ**, và 2–3 **ảnh cũ phục hồi**;
   - 2–3 **file `.iai` đã mặc áo và đã sửa tay chỗ cổ** (lưu ngay sau khi Smudge / Clone).
3. "Sáng áo" và "Đều sáng áo" có cần tác động lên **áo ghép** không, hay áo ghép để nguyên
   hoàn toàn? (Tôi đang để là có, làm cuối.)
4. Nếu ảnh thử cho thấy áo mờ phải dùng AI mới cứu được: chủ có đồng ý **thêm một model** cho
   việc này không? (Xem ảnh so sánh ở đợt 33 rồi quyết cũng được.)
5. Hôm 05/10 chủ nói quy trình thay áo "vẫn có 1 vài điểm bị vấp" — hai việc này đã hết các
   điểm đó chưa?

## 9. Rủi ro và điều chưa biết

- **"Nét áo" cho ảnh mờ nặng là phần chưa chắc nhất.** Làm nét kiểu Develop không cứu được ảnh
  mất nét; AI cứu được nhưng chưa có số đo thời gian trên vải, và có thể méo chữ nhỏ. Đợt 33
  mới có câu trả lời; có thể kết quả là "ảnh mờ quá mức này thì vẫn nên đưa qua AI Image Studio".
- "Da cổ" mới thử trên 2 ảnh, và kết quả phụ thuộc cỡ khung (80% đẹp, 60% xấu). Người cổ dài,
  áo cổ sâu, mặt nghiêng có thể cần khung khác — phải dò ở đợt 33.
- Vân da AI vẽ là vân "hợp lý", không phải đúng từng lỗ chân lông cũ của khách ở chỗ đó. Model
  đôi khi thêm vài chấm nhỏ như nốt ruồi; thanh thấp thì ít thấy.
- Khoanh vùng áo mới thử trên ảnh dễ. Áo tối trên nền tối, áo trùng màu nền, khăn quàng có thể
  khoanh thiếu — vì vậy có "Tô vùng ▸ Áo".
- "Đều sáng áo" phải phân biệt *tối do khuất đèn* với *tối do vải sẫm màu* (vest đen cạnh sơ mi
  trắng); nếu trên ảnh thử nó làm bạc áo sẫm thì giới hạn lại biên độ.
- "Da cổ" cần app tìm lại được mặt sau khi chủ sửa tay — bình thường không vấn đề vì chủ chỉ
  sửa ở cổ.

## 10. Ghi chú kỹ thuật cho phiên sau

- Nhãn Sapiens2 (`src/core/ai/body_parts.rs`, `CLASS_NAMES`): 23 "Áo", 13 "Quần/váy", 1 "Phụ
  kiện", 22 "Thân" (da trần), 3 "Mặt + cổ" (không tách riêng cổ → vùng cổ cắt bằng đường hàm
  của face mesh). `PART_GROUPS` có 8 nhóm, chưa có nhóm áo (`garment::Figure.other` đang lấy
  1 − tổng các nhóm) → thêm nhóm thứ 9 `GROUP_CLOTHES` từ softmax.
- Vùng áo: làm theo mẫu mask tóc (`hair_region`, `hair_base` trong `portrait/analysis.rs`;
  `masked_blur`, `gate_by_colour`, `edge_aware_base`; lượt tóc trong `effects::retouch`). Thêm
  `FaceEdits.clothes`, mục tiêu cọ thứ tư trong `brush.rs`, `SavedFace.clothes` trong
  `recipe.rs` (`#[serde(default)]`).
- Thanh mới trong `PortraitSettings` (đều `#[serde(default)]`, `NEUTRAL` và mặc định = 0, có
  trong `unit()`): `clothes_sharpen`, `clothes_brightness` (hai chiều; dùng lại `skin_tone` /
  `midtoned`), `clothes_even`, `neck` ("Da cổ").
- "Đều sáng áo": khớp mặt bậc thấp của ln độ sáng theo từng cụm màu vải, giới hạn biên độ;
  không dùng nguyên `even_light_gain` (nó giả định một mức "được chiếu" duy nhất).
- "Nét áo" kiểu Develop: `develop::local_detail_boost`, nền tính bằng `masked_blur` theo mask
  áo. Kiểu AI: theo đúng công thức "thay dải chi tiết" của `ai_detail.rs` (ảnh + a·(AI −
  kept·ảnh)), chạy lười qua `OnceLock` như `FaceModel.ai_detail`; đường chạy model sẵn có
  `run_realesrgan_detail` trong `core/ai/retouch.rs`; checkpoint `.pth` ở `tmp/model-checkpoints`
  (general-x4v3, x2plus, x4plus), môi trường xuất ONNX ở `tmp/model-export-env`; giấy phép
  BSD-3. Loại "Phụ kiện" (bảng tên) khỏi vùng AI nếu chữ bị méo.
- "Da cổ": thêm `FaceDetail.neck: Option<Frame>` làm bằng `FaceRestorer::restore_framed` với
  khung dời xuống (`ai_detail.rs` đã có `wider_framing` cho tóc, `WIDEST` = 0,6 — cổ cần khoảng
  0,8, xem mục 4). Trong `retouch_pixel`: phần cổ = mask da × (1 − trong đường viền mặt) × dưới
  hàm; chi tiết lấy từ khung cổ thay cho `detail.at`; **không cộng** với "Chi tiết mặt (AI)" ở
  chỗ hai khung chồng nhau (lấy phần lớn hơn). Cân sáng cổ: `even_light_gain` + phần đều màu
  của `skin_result`, nhân với trọng số cổ và thanh `neck`. Model chạy trên ảnh hiện tại của
  layer (sau khi sửa tay), CPU, luồng nền như `start_detail_analysis` trong `portrait_ops.rs`.
- Luật thanh về 0 cho layer đã sửa tay: `begin_portrait` (`retouched` → `PortraitSettings::
  NEUTRAL`), `reopen_target` / `as_made` trong `portrait_ops.rs`.
- Áo ghép (nếu làm phần sáng): xem trước layer thứ hai bằng `preview_layer_tiles(layer_id,
  tiles)`; giữ bản áo gốc để kéo lại thanh không hỏng dần.
- Thử nhanh ngoài app (06/10, Python + onnxruntime): Sapiens2
  `%APPDATA%\iAi\models\sapiens2-seg\…512x384.onnx` (đầu vào `pixel_values` 1×3×512×384, chuẩn
  hóa ImageNet); model mặt `%APPDATA%\iAi\models\gfpgan\RestoreFormer_PP.onnx` (đầu vào `input`
  1×3×512×512 trong −1..1, đầu ra thứ nhất −1..1). Khung thử: mắt trái / phải về (193, 240) /
  (319, 240) × hệ số cỡ, rồi dời theo chiều dọc.
- Test: `app::portrait_ops` chạy riêng `--test-threads=1`.
