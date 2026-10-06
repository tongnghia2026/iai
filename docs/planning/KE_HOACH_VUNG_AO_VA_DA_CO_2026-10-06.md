# Kế hoạch: vùng "Áo" (làm nét + cân sáng) và nút "Làm lại da cổ" (06/10/2026)

**Trạng thái: CHỜ CHỦ DUYỆT. Chưa viết dòng code nào.** Nối tiếp
`KE_HOACH_THAY_AO_OFFLINE_2026-10-05.md` (đợt 31, 32 chủ test OK). Kế hoạch này thay cho "đợt 33"
ghi ở đó.

## 1. Việc chủ giao

Lời chủ 06/10: "phần mặt, tóc,.... đã ok; tôi cần thêm 1 model nhận diện, khoanh vùng áo để làm
nét và cân bằng ánh sáng; ngoài ra: sau khi thay áo xong user điều chỉnh smudge, clone,... các
phần ở da cổ thì cần thêm 1 nút chạy lại chi tiết da, cân bằng ánh sáng chỉ ở vùng cổ; hãy lên
kế hoạch cho tôi đọc".

Tôi hiểu thành hai việc:

- **A. Vùng "Áo"**: app nhận ra cái áo trong ảnh như đang nhận ra da, tóc, lông mày; rồi làm nét
  và cân sáng riêng cho áo. Tôi hiểu là áp dụng cho **cả áo khách đang mặc sẵn lẫn áo ghép** —
  nếu chủ chỉ cần một trong hai thì báo, việc sẽ gọn hơn.
- **B. Nút "Làm lại da cổ"**: sau khi mặc áo, chủ sửa tay chỗ cổ bằng Smudge, Clone… thì bấm
  một nút để app trả lại vân da và làm đều sáng, **chỉ trong vùng cổ**, không đụng lại mặt.

## 2. Tóm tắt

1. **Không phải tải model mới để nhận diện áo.** Model Sapiens2 đang dùng cho da và tóc vốn đã
   có sẵn nhãn "Áo", "Quần / váy", "Phụ kiện"; app chỉ chưa đem ra dùng. Tôi đã chạy thử trên 4
   ảnh khách thật, áo được khoanh gọn (mục 3). Áo ghép thì còn dễ hơn: nó là layer riêng nên
   vùng áo chính xác tuyệt đối.
2. **Làm nét áo**: đề xuất dùng bộ làm nét sẵn có của Develop, giới hạn trong vùng áo — tức
   thì, không bịa chi tiết, chữ trên bảng tên và logo không bị méo. Bản dùng AI (Real-ESRGAN)
   tôi sẽ làm ảnh so sánh để chủ nhìn rồi mới quyết có thêm hay không, vì nó chậm trên máy tiệm.
3. **Cân sáng áo**: nâng phần áo khuất đèn cho đều với phần sáng, giữ nguyên hoa văn và nếp
   vải; thêm một thanh chỉnh tay sáng / tối riêng cho áo. Áo ghép còn được tự khớp độ sáng với
   mặt và nhận chung "Màu studio" (hiện nay màu studio chỉ áp lên người, không áp lên áo ghép).
4. **Nút "Làm lại da cổ"** nằm ở hàng "Áo": một lần bấm, vài giây, một bước Ctrl+Z.
5. Làm thành **ba đợt**: ảnh thử trước cho chủ xem → vùng "Áo" trong app → nút da cổ.

## 3. Hiện trạng (đã kiểm 06/10)

- **Thử nhận diện áo**: chạy riêng model Sapiens2 đang cài trên 4 ảnh trong
  `C:\Users\Admin\Downloads\ht` (3 áo sơ mi trắng, 1 áo thun xanh có logo và bảng tên). Kết quả:
  áo được khoanh gọn cả hai vai và cổ áo; tóc dài xõa trước áo không bị lẫn vào áo; bảng tên
  được nhận là "Phụ kiện"; khoảng da hở trong cổ áo được nhận là "Thân" (da), tách khỏi áo. Mỗi
  ảnh khoảng 1,7 giây bằng CPU — và lúc chỉnh chân dung app **đã chạy model này sẵn rồi**, nên
  thêm vùng áo không làm chờ lâu hơn. Ảnh xem: `tmp\vung-ao\nhan-dien-ao-thu.jpg` (áo tô màu
  xanh lơ, da cam / hồng, tóc tím, phụ kiện vàng).
  - Giới hạn của lần thử: mới 4 ảnh, model chạy trên cả khung ảnh chứ chưa qua đúng đường cắt
    của app; chưa thử vest tối trên nền tối, áo dài, áo cùng màu với nền, trẻ em.
- **Áo ghép hiện nay** (`tmp\vung-ao\co-va-ao-hien-nay.jpg`, cắt từ ảnh ghép thử đợt 31): áo mềm
  hơn mặt vì phôi áo phải phóng 1,4–2,4 lần; áo trắng gần như loá mất nếp vải; da tô ở khoảng
  hở cổ là màu phẳng, không có vân da, không có bóng dưới cằm.
- **Các bước làm đẹp hiện có chỉ chạy trên da, tóc, mắt, môi, lông mày.** Áo khách mặc sẵn chỉ
  hưởng phần "Sửa màu & sáng" chung cả ảnh; áo ghép (layer "Áo") không hưởng gì cả.
- **Model làm nét bằng AI**: thư mục `models\realesrgan` hiện đang trống (chỉ có file hướng
  dẫn). Số đo cũ ngày 03/10: bản Real-ESRGAN đẹp nhất mất khoảng 27 giây cho một vùng 360×270 —
  ước cho cả vùng áo của một ảnh thẻ là vài phút (ước lượng, chưa đo). Bản nhẹ nhanh hơn nhiều
  nhưng hôm đó thử trên tóc thì bị bệt; trên vải chưa thử.

## 4. Phần A — Vùng "Áo"

### 4.1. Nhận diện

- **Áo khách mặc sẵn**: lấy nhãn "Áo" + "Quần / váy" + "Phụ kiện" của Sapiens2, rồi gọt mép cho
  sát bằng chính đường viền tách người và màu ảnh (cách đang làm với tóc), để mép vùng áo không
  lem ra nền hay lên cổ.
- **Áo ghép**: vùng áo = đúng layer "Áo". Không chạy model, không sai mép.
- Nhận sai thì sửa tay được: mục "Tô vùng" có thêm lựa chọn **"Áo"** bên cạnh Da / Tóc / Lông
  mày; "Hiện vùng nhận diện" tô thêm vùng áo bằng một màu riêng.

### 4.2. Làm nét áo

Thanh **"Nét áo"** (0–100).

- Cách đề xuất: bộ làm nét của Develop ▸ Detail (đã đo khớp Camera Raw), chỉ chạy trong vùng áo,
  không tạo viền sáng ở mép áo – nền. Nét lên ở cổ áo, ve áo, hàng cúc, đường may, sợi vải.
  Tức thì; lần nào cũng ra như nhau; chữ bảng tên, logo, phù hiệu giữ đúng nét thật.
- Với **áo ghép**: mức nét tự tăng theo độ phóng (áo phóng càng nhiều thì bù nét càng nhiều),
  và mép áo được làm mượt lại cho hết răng cưa.
- Bản AI (Real-ESRGAN): "vẽ" lại sợi vải cho ảnh rất mờ, nhưng chậm, và AI loại này hay làm méo
  chữ nhỏ (bảng tên, logo đồng phục). Tôi **không đưa vào app ngay**; ở đợt 33 tôi làm ảnh so
  sánh hai cách kèm thời gian chạy thật, chủ nhìn rồi quyết.

### 4.3. Cân sáng áo

- Thanh **"Đều sáng áo"** (0–100): một bên vai tối, phần áo khuất đèn, áo sậm dần xuống dưới →
  nâng cho đều với phần được chiếu sáng. Chỉ sửa độ sáng theo mảng lớn nên hoa văn, sọc, nếp
  gấp, ranh giới áo vest – sơ mi – cà vạt giữ nguyên.
- Thanh **"Sáng áo"** (hai chiều): trái tối hơn, phải sáng hơn, chỉ riêng áo — không đụng mặt,
  tóc, nền.
- Áo trắng bị loá: kéo lại nếp vải (sẽ xem trên ảnh thử có đáng làm không).
- Riêng **áo ghép**: tự khớp độ sáng của áo với mặt khách, và nhận chung "Màu studio" đang chọn
  để người và áo cùng một tông.

### 4.4. Giao diện

- Bảng Chỉnh chân dung có thêm nhóm **"Áo"** (đặt sau nhóm "Tóc") với ba thanh: Nét áo, Đều
  sáng áo, Sáng áo. Không gắn chú thích nổi lên thanh.
- "Làm ảnh thẻ tự động" và "Tự động làm đẹp" áp luôn mức mặc định của ba thanh này — không thêm
  cú bấm nào vào quy trình. Mức mặc định tôi đề xuất sau khi có ảnh thử, chủ chốt.
- "Công thức" đã lưu từ trước đọc ba thanh mới là 0 (ảnh cũ mở lại không tự đổi).

### 4.5. Ảnh đã thay áo

Ba thanh nhóm "Áo" tác động lên layer "Áo". Bản áo chưa chỉnh được giữ lại bên dưới (ẩn), nên
kéo thanh lại nhiều lần hay bấm "Chỉnh áo" (dời / phóng / xoay) thì áo không bị mờ dần hay nét
chồng nét. Áo vẫn là một layer riêng, chủ vẫn sửa tay tùy ý như đã chốt ở đợt 32.

## 5. Phần B — Nút "Làm lại da cổ"

**Vị trí**: hàng "Áo" của ô Ảnh thẻ, cạnh "Chỉnh áo" và "Bỏ áo". Bảng này vẫn mở trong lúc chủ
dùng Smudge, Clone.

**Cách dùng**: mặc áo → sửa tay chỗ cổ (Smudge, Clone, Repair…) → bấm **"Làm lại da cổ"**.

**App làm gì khi bấm:**

1. Tìm lại **vùng cổ** trên ảnh đang thấy: phần da từ dưới đường hàm xuống tới mép áo, gồm cả
   khoảng da hở trong cổ áo; trừ tóc và trừ phần áo che. Mép vùng được làm mềm ở đường hàm để
   **mặt không bị đụng lại** (mặt đã làm đẹp rồi, làm lần nữa sẽ bệt).
2. **Cân sáng**: xóa các mảng sáng – tối loang lổ do Clone / Smudge để lại; đưa màu da cổ về
   đúng màu da mặt của khách; giữ cổ tối hơn mặt một chút như thật. Nếu cổ bị phẳng lì (do tô
   lại) thì thêm bóng nhẹ dưới cằm.
3. **Chi tiết da**: đo độ mịn / vân da ở má (như đang hiển thị), rồi trả lại vân da cho những
   chỗ ở cổ bị Smudge làm trơn, tới khi cổ và mặt cùng một độ mịn.
4. Ghi thẳng vào layer chủ vừa sửa ("Chân dung" nếu có, không thì "Người"), **một bước Ctrl+Z**.

**Chi tiết khác:**

- Chạy nền khoảng vài giây (phải tìm lại mặt và cổ vì điểm ảnh đã đổi), app không khựng; có
  dòng báo "Đang làm lại da cổ…".
- **Có vùng chọn** thì chỉ làm trong vùng chọn — chủ khoanh đúng chỗ vừa sửa cũng được, kể cả
  chỗ da ngực ở áo cổ sâu.
- Ảnh không thay áo cũng bấm được (vùng cổ tính tới mép áo đang mặc).
- Nút này **không tự vẽ lại** chỗ còn sót áo cũ hay lỗ thủng — đó vẫn là việc của Clone /
  Repair. Nó chỉ làm cho chỗ đã sửa tay "liền" lại với da xung quanh.
- AI vẽ chi tiết mặt (GFPGAN) không dùng được cho cổ: nó chỉ biết vẽ khuôn mặt. Vân da cổ lấy
  theo da thật của chính khách hoặc bộ "Vân da" sẵn có — chọn cách nào xem trên ảnh thử.

## 6. Các đợt

Mỗi đợt xong đều build Release và đưa đường dẫn file chạy thật cho chủ test như lệ thường.

### Đợt 33 — Ảnh thử (chưa đổi giao diện)

- [ ] Vùng áo trên 6 ảnh trong `Downloads\ht` + các ảnh khó chủ đưa thêm (vest tối, áo dài,
      trẻ em): ảnh tô màu vùng nhận diện.
- [ ] Làm nét áo: tấm so sánh *gốc / Develop / AI bản nhẹ / AI bản đẹp*, ghi thời gian chạy
      thật trên máy tiệm — cho cả áo mặc sẵn lẫn áo ghép.
- [ ] Cân sáng áo: tấm so sánh trước / sau ở hai mức; áo ghép trước / sau khi khớp sáng và
      nhận Màu studio.
- [ ] Da cổ: tấm so sánh trước / sau trên file chủ đã sửa tay (mục 7, câu 3).
- [ ] Chủ xem ảnh, chốt: có dùng AI làm nét không; mức mặc định của ba thanh; cách lấy vân da cổ.

### Đợt 34 — Vùng "Áo" trong app

- [ ] Vùng áo trong phần nhận diện; "Tô vùng ▸ Áo"; màu trong "Hiện vùng nhận diện".
- [ ] Nhóm "Áo" với ba thanh; mặc định theo chốt ở đợt 33; chạy trong "Làm ảnh thẻ tự động".
- [ ] Áo ghép: ba thanh tác động lên layer "Áo", tự bù nét theo độ phóng, mượt mép, khớp sáng,
      nhận Màu studio; giữ bản áo gốc để chỉnh lại không hỏng dần.
- [ ] Test tự động + ảnh probe giao diện; chủ test.

### Đợt 35 — Nút "Làm lại da cổ"

- [ ] Vùng cổ (đường hàm → mép áo, trừ tóc, theo vùng chọn nếu có).
- [ ] Cân sáng + trả vân da trong vùng cổ; bóng dưới cằm khi cổ phẳng.
- [ ] Nút ở hàng "Áo", chạy nền, một bước hoàn tác; ghi vào "hộp đen" thời gian chạy.
- [ ] Test tự động; chủ test.

Đợt 34 và 35 không phụ thuộc nhau — chủ cần nút da cổ trước thì đổi thứ tự được.

### Để sau (từ "đợt 33" cũ, chỉ làm khi chủ bảo)

- App tự tô da cổ đẹp hơn ngay lúc mặc áo (cổ áo cũ che cổ, áo cổ sâu thiếu xương đòn).
- Trẻ em: tự thu nhỏ vai áo theo người.
- Đổi màu tóc sau khi mặc áo thì layer "Tóc trên áo" đổi theo.

## 7. Việc cần chủ quyết

1. Duyệt ba đợt 33–35 không? Làm theo thứ tự trên hay nút da cổ trước?
2. "Làm nét + cân sáng áo": tôi làm cho **cả áo mặc sẵn lẫn áo ghép** — đúng ý chủ không?
3. Cho tôi đường dẫn **2–3 file đã mặc áo và đã sửa tay chỗ cổ** (lưu `.iai` ngay sau khi
   Smudge / Clone, trước khi làm gì khác) để thử nút da cổ trên đúng kiểu sửa của chủ. Không có
   thì tôi phải tự giả lập vết sửa, kém sát thực tế.
4. Hôm 05/10 chủ nói quy trình thay áo "vẫn có 1 vài điểm bị vấp". Hai việc trong kế hoạch này
   đã là hết các điểm đó chưa, hay còn điểm nào khác?
5. Ảnh khó cho vùng áo (vest tối trên nền tối, áo dài, áo trùng màu nền): chủ có sẵn thì cho
   đường dẫn thư mục.

## 8. Rủi ro và điều chưa biết

- Nhận diện áo mới thử trên 4 ảnh dễ (áo sáng trên nền xanh / trắng). Áo tối trên nền tối, áo
  trùng màu nền, khăn quàng, tóc dài phủ kín vai có thể khoanh thiếu — vì vậy có "Tô vùng ▸ Áo"
  và phải thử thêm ở đợt 33.
- "Đều sáng áo" phải phân biệt *tối do khuất đèn* với *tối do vải sẫm màu* (vest đen cạnh sơ mi
  trắng). Cách làm là chỉ sửa theo mảng lớn và theo từng loại vải; nếu trên ảnh thử vẫn làm bạc
  áo sẫm thì mức mặc định sẽ để thấp.
- Làm nét kiểu Develop không cứu được ảnh áo mờ nặng (rung tay, mất nét) — chỉ AI mới "vẽ" lại,
  với cái giá là chậm và có thể méo chữ. Chủ quyết sau khi xem ảnh so sánh.
- Thời gian bản AI là số ước từ một lần đo cũ trên tóc; đợt 33 mới có số thật.
- Khớp sáng áo ghép với mặt là ước đoán từ độ sáng da và cổ áo trắng; phôi áo chụp ánh sáng
  khác hẳn (đèn gắt, bóng đổ ngược hướng) thì chỉ khớp được mức sáng, không đổi được hướng sáng.
- Nút da cổ dựa vào việc app tìm lại được mặt sau khi chủ sửa tay — bình thường không vấn đề vì
  chủ chỉ sửa ở cổ. Sửa quá rộng lên hàm thì vùng cổ có thể lệch; khi đó dùng vùng chọn.
- Vân da trả lại là vân "hợp lý", không phải đúng từng lỗ chân lông cũ của khách ở chỗ đó.

## 9. Ghi chú kỹ thuật cho phiên sau

- Nhãn Sapiens2 (`src/core/ai/body_parts.rs`, `CLASS_NAMES`): 23 "Áo", 13 "Quần/váy", 1 "Phụ
  kiện", 22 "Thân" (da trần), 3 "Mặt + cổ" (không tách riêng cổ → vùng cổ phải cắt bằng đường
  hàm của face mesh). `PART_GROUPS` hiện có 8 nhóm, chưa có nhóm áo; `garment::Figure.other`
  đang tính "áo" bằng 1 − tổng các nhóm. Thêm nhóm thứ 9 `GROUP_CLOTHES` từ softmax.
- Mẫu để làm theo cho vùng áo: cách dựng mask tóc + `hair_region` + `hair_base` trong
  `portrait/analysis.rs`; `masked_blur`, `gate_by_colour`, `edge_aware_base`; lượt tóc trong
  `effects::retouch`. Thêm `FaceEdits.clothes`, `brush` thêm mục tiêu thứ tư, `SavedFace.clothes`
  trong `recipe.rs` (field mới `#[serde(default)]`).
- Thanh mới trong `PortraitSettings`: `clothes_sharpen`, `clothes_even`, `clothes_brightness`
  (đều `#[serde(default)]`, `NEUTRAL` = 0, có trong `unit()`); "Sáng áo" dùng lại `skin_tone` /
  `midtoned` (Midtones của Develop) như "Sáng da".
- Làm nét: `develop::local_detail_boost` trong vùng áo, dải nền tính bằng `masked_blur` theo
  mask áo để không có viền ở mép.
- Đều sáng áo: khớp mặt bậc thấp (phẳng / bậc hai) của ln độ sáng theo từng cụm màu vải, giới
  hạn biên độ; không dùng nguyên `even_light_gain` (nó giả định một mức "được chiếu" duy nhất,
  đúng cho da, sai cho áo nhiều màu).
- Áo ghép: xem trước layer thứ hai bằng `preview_layer_tiles(layer_id, tiles)` (đã có, đang dùng
  cho layer nguồn). Bù nét theo `Placement.scale`; mượt mép alpha trong `Cloth::laid`. Giữ bản
  áo gốc: cân nhắc layer ẩn kiểu nguồn của "Chân dung" hoặc lưu trong recipe của layer — chọn
  khi đọc kỹ `apply_portrait` / `Worn`. Màu studio: `looks::grade` trên layer "Áo".
- Da cổ: dùng lại `PortraitModel::skin_layers_from` với mask = vùng cổ, đọc `mean` / `lit` /
  độ lớn dải `src − low1` từ da mặt của ảnh hiện tại; vân da: `pores()` hoặc mượn dải mịn từ má
  (kiểu donor của "Xóa mụn"). Chạy trên luồng nền (bài học hộp đen 05/10), ghi dòng `perf`.
  Layer đích: "Chân dung" đang hiện → "Người" → layer đang chọn. `Fitting` cũ không dùng lại
  được vì điểm ảnh đã đổi; face mesh + Sapiens2 chạy lại.
- Real-ESRGAN: checkpoint `.pth` ở `tmp/model-checkpoints` (general-x4v3, x2plus, x4plus), môi
  trường xuất ONNX ở `tmp/model-export-env`; đường chạy sẵn `run_realesrgan_detail` trong
  `core/ai/retouch.rs`. Giấy phép BSD-3. Chỉ dùng cho ảnh so sánh ở đợt 33 cho tới khi chủ chốt.
- Lệnh thử nhanh ngoài app (06/10): Python + onnxruntime đọc
  `%APPDATA%\iAi\models\sapiens2-seg\sapiens2_seg_0.4b_512x384.onnx`, đầu vào `pixel_values`
  1×3×512×384 chuẩn hóa ImageNet, đầu ra `logits` 1×29×512×384.
- Test: `app::portrait_ops` chạy riêng `--test-threads=1`; test model thật của garment cũng vậy.
