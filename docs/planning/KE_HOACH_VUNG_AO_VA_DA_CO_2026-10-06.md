# Kế hoạch: menu "Áo" và thanh "Da cổ" (06/10/2026 — sửa lần 2 theo ý chủ)

**Trạng thái (06/10): ĐỢT 33 "NÉT ÁO" ĐÃ CODE XONG — CHỜ CHỦ TEST.** Chủ bảo làm nét áo trước
(mục 1, lời thứ ba). Phần còn lại của menu "Áo" (đợt 34) và thanh "Da cổ" (đợt 35) chưa làm.
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
- Test: 5 bài lõi (vùng áo, cỡ cho xem, đo độ nhòe mép, ảnh AI trả về giữ tông của ảnh, chỉ
  điểm ảnh áo đổi); 1 bài nhãn "đồ đang mặc"; 1 bài bảng (menu "Áo", ảnh đã ghép áo); 1 bài
  chạy model thật trong app (kéo thanh → chạy nền → Áp dụng: áo đổi, mặt không đổi điểm nào).
  Lệnh xem ảnh thử: `IAI_PORTRAIT_CLOTHES_PROBE=<thư mục ảnh>` với
  `cargo test --lib -- --ignored probe_clothes --nocapture` (ảnh tên `..._x1.6.png` = đã phóng
  1,6 lần).
- Ảnh trước / sau đi qua đúng đường xử lý của app: `tmp\vung-ao\net-ao-truoc-sau-chu-theu.jpg`,
  `net-ao-truoc-sau-hang-cuc.jpg`, `net-ao-truoc-sau-ao-trang.jpg`.
- Chưa làm trong đợt này: "Tô vùng ▸ Áo" (sửa tay vùng áo) — sang đợt 34.
- [ ] Chủ test.

### Đợt 34 — Phần còn lại của menu "Áo"

- [ ] "Tô vùng ▸ Áo": sửa tay vùng áo khi app nhận sai.
- [ ] Thanh "Sáng áo" và "Đều sáng áo"; ảnh thử trước / sau ở hai mức cho chủ xem trước.
- [ ] Vùng áo trên ảnh khó (vest tối, áo dài, áo trùng màu nền, trẻ em): ảnh tô màu vùng áo.
- [ ] Test tự động + ảnh probe giao diện; chủ test.
- [ ] (Nếu chủ cần) Sáng áo / Đều sáng áo cho layer áo ghép.

### Đợt 35 — Thanh "Da cổ"

- [ ] Ảnh thử trên file chủ đã sửa tay; dò cỡ khung cho vân da đẹp nhất; ba mức thanh.
- [ ] Vùng da cổ (đường hàm → mép áo, trừ tóc).
- [ ] Lượt chạy riêng của model "Chi tiết mặt (AI)" cho cổ, chạy nền một lần khi thanh rời 0.
- [ ] Cân sáng vùng cổ; thanh "Da cổ" trong menu "Da"; ghi thời gian chạy vào "hộp đen".
- [ ] Test tự động (có bài kiểm mặt không đổi điểm ảnh nào); chủ test.

Đợt 34 và 35 không phụ thuộc nhau — chủ muốn có thanh "Da cổ" trước thì đổi thứ tự được.

### Để sau, chỉ làm khi chủ bảo

- App tự tô da cổ đẹp hơn ngay lúc mặc áo (cổ áo cũ che cổ, áo cổ sâu thiếu xương đòn).
- Trẻ em: tự thu nhỏ vai áo theo người.
- Đổi màu tóc sau khi mặc áo thì layer "Tóc trên áo" đổi theo.

## 8. Việc cần chủ quyết

1. ~~Menu "Áo" trước hay "Da cổ" trước?~~ Chủ chốt 06/10: làm nét áo trước (đã làm, đợt 33).
   Sau khi test "Nét áo": làm tiếp đợt 34 hay đợt 35 trước?
2. Cho tôi đường dẫn ảnh mẫu để thử cho sát thực tế:
   - vài **ảnh khách tự chụp điện thoại, áo mờ**, và 2–3 **ảnh cũ phục hồi** — "Nét áo" mới thử
     trên ảnh của một khách;
   - 2–3 **file `.iai` đã mặc áo và đã sửa tay chỗ cổ** (lưu ngay sau khi Smudge / Clone).
3. "Sáng áo" và "Đều sáng áo" có cần tác động lên **áo ghép** không, hay áo ghép để nguyên
   hoàn toàn? (Tôi đang để là có, làm cuối.)
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
- Còn lại cho đợt 34: `FaceEdits.clothes`, mục tiêu cọ thứ tư trong `brush.rs`,
  `SavedFace.clothes` trong `recipe.rs` (`#[serde(default)]`); mask áo hiện chỉ có sau khi
  thanh "Nét áo" được dùng (làm lười), cọ cần mask ngay → tách phần dựng mask khỏi phần chạy
  model.
- Thanh trong `PortraitSettings` (đều `#[serde(default)]`, `NEUTRAL` và mặc định = 0, có trong
  `unit()`): `clothes_sharpen` (đã có); còn `clothes_brightness` (hai chiều; dùng lại
  `skin_tone` / `midtoned`), `clothes_even`, `neck` ("Da cổ").
- "Đều sáng áo": khớp mặt bậc thấp của ln độ sáng theo từng cụm màu vải, giới hạn biên độ;
  không dùng nguyên `even_light_gain` (nó giả định một mức "được chiếu" duy nhất).
- Số đo "Nét áo" 06/10 (CPU máy tiệm, vùng áo 1043×650 của ảnh thẻ): general-x4v3 4,7 s ở cỡ
  nguyên / 1,2 s ở nửa cỡ; x2plus 15,5 s / 3,6 s; x4plus nửa cỡ 15,4 s. Bài học: cho xem ở nửa
  cỡ một ảnh còn nguyên cỡ gốc làm hỏng chữ cao ~12 px; tỷ lệ năng lượng các dải tần không
  tách được ảnh phóng lớn khỏi ảnh nét (đã thử, bỏ) → dùng mức phóng app biết + đo độ nhòe mép
  (`edge_blur`: tỷ số gradient mạnh nhất qua hai mức blur). Model ONNX ở
  `models/realesrgan` của repo (không vào git); giấy phép BSD-3. Nếu chữ bảng tên bị méo:
  loại lớp "Phụ kiện" khỏi vùng AI.
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
