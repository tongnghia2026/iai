# Kế hoạch: Làm ảnh thẻ 3×4 tự động (02/10/2026)

## Chủ chốt (02/10)

- Cỡ duy nhất: **3×4 = 2,8×3,8 cm @ 600 ppi = 661×898 px** (đúng ảnh mẫu của chủ,
  trùng preset "Ảnh thẻ 3×4" có sẵn trong Crop).
- **Cắt luôn** (không bắt bấm Enter); không ưng thì Ctrl+Z rồi tự crop.
- Khung **rộng hơn mẫu ~10%** (mặc định, có thanh kéo chỉnh, nhớ lần sau).
- **Tự xoay thẳng** theo đường mắt (có ô tích, mặc định bật).
- Có ô tích **crop / không crop**; có ô tích **tách người ra layer riêng + nền trắng**.
- Vùng chọn (tùy): có vùng chọn thì chỉ lấy mặt trong vùng chọn.

## Chuẩn đo từ ảnh mẫu (`tmp/anh-the/mau_chu_3x4.jpg`, 661×898)

Đo bằng Face Mesh của iAi (mống mắt 468/473, cằm 152):

| Mốc | y (px) | % chiều cao |
|---|---|---|
| Đỉnh tóc | 108 | 12,0% |
| Đường mắt (giữa 2 mống mắt) | 301,6 | 33,6% |
| Cằm | 509,1 | 56,7% |

Mắt→cằm = 23,1% chiều cao; mặt nằm giữa theo chiều ngang (lệch < 0,3%).

## Cách tính khung

1. Face Mesh tìm mặt (trong vùng chọn nếu có; nhiều mặt → lấy mặt lớn nhất, báo lại).
2. Trục mặt: đường mắt (góc θ). Xoay thẳng bật → khung xoay theo θ (bỏ qua khi lệch < 0,5°).
3. d = khoảng mắt→cằm đo theo trục dọc của mặt. Chiều cao khung chuẩn H₀ = d / 0,231;
   mắt ở 33,6% từ trên. Nới rộng s (mặc định 1,10) quanh điểm giữa mắt–cằm: H = s·H₀.
   Rộng = H × 661/898. Tâm ngang = trung bình (giữa 2 mắt, giữa 2 gò má 234/454).
4. Chống cụt tóc: có mask người → đỉnh đầu phải cách mép trên ≥ 6% H; thiếu thì
   đẩy khung lên (tối đa 8% H).
5. Khung tràn ra ngoài ảnh ở 2 bên/dưới → thu nhỏ s dần (không dưới 1,0); vẫn tràn →
   phần thiếu để trắng và báo.

## Tách nền trắng

- Select Subject (BiRefNet, model sẵn có; thiếu thì tải như Select Subject) chạy trên
  vùng quanh khung (độ phân giải tốt hơn chạy cả ảnh).
- Layer **"Người"** = ảnh đang thấy (gộp các layer hiện) + **mask** từ AI (tô sửa được);
  màu viền tóc được khử ám nền cũ (decontaminate) để đặt lên nền trắng không viền màu.
- Layer **"Nền trắng"** ngay dưới; các layer cũ giữ nguyên bên dưới.

## Thứ tự áp dụng (1 bước Ctrl+Z "Làm ảnh thẻ")

Thêm layer "Người" → crop/xoay/thu về 661×898 (mọi layer) → thêm "Nền trắng" → đặt 600 ppi.

## Chất lượng thu nhỏ

Crop có resample hiện lấy mẫu song tuyến 1 điểm → thu nhỏ 2–5 lần bị răng cưa/nhiễu.
Sửa: khi thu nhỏ, lấy trung bình nhiều điểm trong ô (supersampling) — lợi cả cho
Crop preset 3×4 chủ vẫn dùng tay.

## Các bước

- [x] P1 Lõi `src/core/id_photo.rs`: tính khung, chống cụt tóc, co khung khi ảnh chật,
  giữ đúng người (bỏ người/vật rời), khử ám viền tóc (giải đúng chỗ tóc dày +
  blur-fusion chỗ tóc mỏng), áp dụng 1 bước undo; 10 test + probe
  (`IAI_ID_PHOTO_PROBE=<thư mục> cargo test --lib id_photo::tests::probe -- --ignored --nocapture`).
- [x] P2 App `src/app/id_photo_ops.rs`: chạy nền (Face Mesh + BiRefNet Tiny, GPU→CPU),
  thiếu model thì tải rồi tự chạy, áp dụng khi xong (đóng hộp thoại, báo trạng thái).
- [x] P3 Hộp thoại Image ▸ Làm ảnh thẻ… (`src/ui/dialogs/id_photo.rs`), nhớ tùy chọn
  trong prefs.json (khóa `id_photo`).
- [x] P4 Crop thu nhỏ: `resample_into_tiles_footprint` lấy trung bình lưới điểm trong ô
  (áp cho Crop có xoay/thu nhỏ, kể cả Crop preset tay); test sọc 1 px thu 4 lần.
- [x] Build Release, chủ test 02/10: **OK**.

## Đợt 2 (chủ chỉnh sau test, 02/10 tối)

- [x] Bỏ layer "Nền trắng": **Background tô trắng** (các layer cũ khác ẩn); người nhân
  2 layer: **"Người"** (tách nền, mask AI) trên **"Ảnh gốc"** (ảnh gốc, mask đen — tô
  trắng để lấy lại chi tiết AI cắt mất).
- [x] BiRefNet **không thử GPU nữa** (cờ `gpu` trong ModelSpec; YOLO vẫn GPU) — hết
  khựng do lùi GPU→CPU, áp cho cả Select Subject.
- [x] Xong thì **tự mở Chỉnh chân dung** trên layer "Người" (ô tích, mặc định bật).
  Layer kết quả "Chân dung" nay nhận mask của layer nguồn (không lộ nền cũ).
- [x] Sửa lỗi cũ phát hiện khi làm: mask "Hide All" (đen) bị Crop có thu nhỏ/xoay
  biến thành trắng (lộ hết) — `LayerMask::new_black` nay là tile đặc.
- [x] Build Release, chủ test đợt 2: **OK**.

## Đợt 3 (chủ chỉnh sau test đợt 2, 02/10 tối)

- [x] Tách nền bằng **BiRefNet Full** (`birefnet-general-epoch_244.onnx`, ~928 MB, CPU;
  đưa lại vào danh sách Select Subject) — giữ sợi tóc tốt hơn Tiny rõ rệt; chậm hơn
  ~7–8 s/ảnh trên máy dev.
- [x] Layer người kiểu **Ctrl+J với vùng chọn**: "Layer 1" KHÔNG mask (mask AI ép vào
  alpha sau khi crop; màu dưới chỗ trong suốt giữ nguyên). "Ảnh gốc" (mask đen) giữ.
- [x] **Màu studio** trong Chỉnh chân dung (`src/core/portrait/looks.rs`): 6 bộ màu
  (Trong trẻo, Hồng hào, Trắng sáng, Ấm áp, Tự nhiên, Film nhẹ) — LUT 33³, giữ trắng
  cho áo/nền trắng; nhóm "Màu studio" có ô màu mẫu + thanh "Độ đậm"; Áp dụng tạo layer
  "Màu studio: <tên>" ngay trên "Chân dung", độ đậm = opacity; mở lại thì cập nhật/bỏ.
- [ ] Build Release, chủ test đợt 3.

## Kết quả đo thử (02/10)

- Ảnh mẫu của chủ phóng 2× đặt vào khung trắng 1800×2400, chạy với độ rộng 0%:
  khung ra lệch < 2 px so với mẫu, ảnh ra trùng mẫu (sai khác trung bình 2/255).
- 8 ảnh chân dung thật (tmp/anh-the/probe): khăn xếp, mũ, tóc xù ngược sáng, đầu
  nghiêng 11° đều ra đúng khung; 2 ảnh chụp cận mặt (thiếu vai) ra khung trắng phần
  thiếu + có báo — đúng thiết kế.
- Thời gian (bản debug, CPU): 16–37 s/ảnh 20 MP; bản Release nhanh hơn nhiều.

## Còn mở / chờ chủ

- Phím tắt riêng cho "Làm ảnh thẻ" (bảng phím tắt bắt buộc có phím mặc định).
- Râu dài: AI coi đáy râu là cằm → mặt nhỏ hơn chuẩn một chút.
- Chưa có chống cụt tóc khi tắt "nền trắng" (cần mask người).
