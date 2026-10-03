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
- [x] Build Release, chủ test đợt 3: **OK** (riêng BiRefNet Full: chủ thấy chậm và tách
  không đẹp bằng bản Quality).

## Đợt 4 (02/10 tối)

- [x] Tách nền về lại **BiRefNet Tiny ("Quality")**; bỏ BiRefNet Full khỏi app.
- [x] Chỉnh chân dung: ảnh mới **luôn bắt đầu từ thông số Mặc định** (trước đây hộp thoại
  giữ thông số ảnh trước → bóp mặt, son, màu studio bị áp sang người khác). Mở lại layer
  "Chân dung" cũ vẫn khôi phục thông số của chính nó.
- [x] Build Release, chủ test đợt 4.

## Đợt 5 (03/10)

- [x] Đảo quy trình theo chủ: **tách nền trên toàn ảnh trước** (như Select Subject; trước
  đây chạy BiRefNet trên vùng cắt quanh mặt → model không thấy cả người, tách kém hơn)
  → tìm mặt (chỉ nhận mặt nằm trên người đã tách) → crop → Chỉnh chân dung.
- [x] Màu studio **gộp chung vào layer "Chân dung"** (không còn layer màu riêng); "Độ đậm"
  = mức pha màu vào layer đó.
- [x] Build Release, chủ test đợt 5: tóc mỏng ở cổ bị **mảng trắng cạnh vuông**.
- [x] Nguyên nhân: bước lọc bỏ người/vật khác (`keep_person`) chỉ giữ khối mask ≥50% cộng
  một dải nới HÌNH VUÔNG ~1/150 cạnh vùng → sợi tóc mỏng xa hơn bị xóa, cạnh thẳng (đo:
  29–31% điểm tóc mỏng trên ảnh tóc dài). Sửa: nối khối qua mọi giá trị mask ≥8 (3%), chỉ
  xóa khối tách rời hẳn; đo lại: 0 điểm tóc mỏng bị xóa. (Không phải do thứ tự model.)
- [ ] Build Release, chủ test + chủ gửi ảnh Select Subject để so.

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

## Đợt 6 (03/10): Tạo khối + Vân da trong Chỉnh chân dung

Chủ: ảnh độ phân giải thấp / nhiều mụn / thiếu sáng nhiều noise → kéo Làm mịn da cao thì
mặt mất khối; muốn thêm "tạo khối" khi làm mịn và "tạo vân da" bù cho ảnh ĐT bị bệt.

- Nguyên nhân: làm mịn bỏ 85% tầng giữa (r1≈e/220 … r2≈e/28), tầng này chứa cả khối mặt
  (sống mũi, cánh mũi, nếp má, hốc mắt) — thấy rõ mũi biến mất ở làm mịn 100.
- [x] Tách tầng giữa tại ≈e/80 (band `form` trong SkinLayers): **"Tạo khối"** giữ sáng-tối
  của nửa thô (khối), không giữ màu (mảng đỏ mụn vẫn đều), nửa mịn (sần/noise) vẫn bỏ; thêm
  nhẹ khối lớn (broad − huge, huge ≈ e/2,5). Mặc định 30.
- [x] Làm mịn gần 100 bỏ thêm tầng vân mịn (noise ĐT): 0,15·s + 0,45·s³.
- [x] **"Vân da"**: vân lỗ chân lông tổng hợp (value noise, chu kỳ ≈ e/350, ≥1 px), chỉ trên
  da, mạnh ở tông giữa, trung bình 0 (không đổi độ sáng). Mặc định 0.
- Thử: ảnh mẫu chủ + 2 ảnh hạ độ phân giải/thêm noise (`tmp/anh-the/form`, probe
  `IAI_PORTRAIT_FORM_PROBE`): làm mịn 100 mất mũi → Tạo khối 80 khối trở lại, noise vẫn sạch;
  Vân da 50 hết cảm giác nhựa.
- Chưa làm: chi tiết da bằng AI (GFPGAN có sẵn trong models, Auto Retouch đã có đường
  "texture transfer") — làm nếu chủ thấy Vân da tổng hợp chưa đủ.
- [x] Build Release, chủ test đợt 6: **OK**.

## Đợt 7 (03/10): bấm đúp layer "Chân dung" để chỉnh tiếp

- [x] Bấm đúp dòng layer "Chân dung" trong bảng Layer (trừ icon mắt/mask) → chọn layer đó và
  mở lại Chỉnh chân dung với đúng thông số/mặt/mask đã tô — như layer điều chỉnh. Đổi tên
  vẫn ở menu chuột phải. (`layer_is_portrait` trong view model, `reopen_portrait_layer`.)
- [x] Build Release, chủ test đợt 7: **OK**.

## Kết thúc (03/10 tối)

Chủ test OK toàn bộ. Chủ bỏ, không làm: phím tắt riêng, râu dài = cằm, chống cụt tóc khi tắt
nền trắng, so ảnh Select Subject; portable + push chờ chủ bảo riêng.
**Việc kế tiếp (phiên mới):** "Chi tiết da AI" bằng GFPGAN có sẵn trong Chỉnh chân dung.
