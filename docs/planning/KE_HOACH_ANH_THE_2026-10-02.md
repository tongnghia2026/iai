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

## Đợt 8 (03/10 tối): "Chi tiết mặt (AI)" trong Chỉnh chân dung

Chủ: bù chi tiết bằng AI cho ảnh độ phân giải thấp / ảnh ĐT bị bệt, bổ sung cho "Vân da".

- Model: GFPGAN v1.4 có sẵn (`models/gfpgan`, Apache-2.0), chạy CPU, một lần cho mỗi mặt
  (2,5–4,5 s), **lười**: chỉ chạy khi thanh rời 0 (như phân tích dáng người); mặt bật thêm
  sau thì chạy bù; Áp dụng / mở lại layer "Chân dung" tự chờ hoặc chạy lại.
- [x] `FaceRestorer` (`core/ai/retouch.rs`): căn mặt theo 5 mốc lấy từ Face Mesh (tâm hai
  mắt, đỉnh mũi, hai khóe miệng) về khung 512; mặt lớn hơn khung thì lấy trung bình vùng.
  Phần chạy model tách thành hàm dùng chung với Auto Retouch (hành vi Auto Retouch không đổi).
- [x] `core/portrait/ai_detail.rs`: mỗi mặt giữ 2 lớp chi tiết RGB ở khung 512 (~7 MB, không
  phụ thuộc cỡ ảnh): của AI và của ảnh (cái bị thay) = phần mà blur 8 px bỏ đi, mờ dần 24 px
  ở mép khung; kèm viền mặt (FACE_OVAL).
- [x] Trộn (`effects.rs`): thanh **thay** dải chi tiết của ảnh bằng dải của AI
  (`+ a·(AI − kept·ảnh)`), không cộng chồng. Màu, sáng tối, khối mặt vẫn là của ảnh nên không
  thành "mặt AI". Trên da `kept` = phần chi tiết ảnh còn lại sau Làm mịn; đốm mụn đã xóa thì
  không thay. Trong viền mặt áp cho mọi thứ (mắt, mi, lông mày, môi, kính, râu); ngoài viền
  chỉ áp trên da (cổ, tai).
- [x] Thanh "Chi tiết mặt (AI)" trong nhóm Da (sau "Vân da"), mặc định 0; dòng ghi chú khi
  AI đang chạy / thiếu model / lỗi. Layer cũ đọc 0.
- Thử (probe `IAI_PORTRAIT_DETAIL_PROBE`, ảnh ở `tmp/anh-the/ai-detail*`):
  - Bản đầu "cộng thêm chi tiết AI" (luma, 4 px, chỉ nơi ảnh thiếu) → viền mắt, nếp da nặng,
    nhiễu còn nguyên → bỏ. "Thay dải" cho mặt sạch, nét, tự nhiên.
  - Ảnh mẫu chủ (mờ): mắt, mi, sống mũi, cánh mũi, môi nét lên; ảnh nhiễu: da sạch hẳn kể cả
    khi Làm mịn thấp; mặt 138 px: được vẽ lại rõ.
  - GFPGAN gần như **không vẽ lỗ chân lông** (da rất sạch) → muốn có vân da thì kéo thêm
    "Vân da": hai thanh bổ sung nhau.
  - Ảnh vốn nét: thanh 100 thay nếp nhăn, mắt bằng bản AI vẽ (mịn hơn, hơi khác người thật)
    → chú thích ghi rõ dành cho ảnh mờ / nhiễu / nhỏ.
  - Tóc và nền ngoài mặt không đổi (ảnh nhiễu nặng: mặt sạch, tóc còn nhiễu).
- [x] Build Release, chủ test đợt 8: **OK**.

## Đợt 9 (03/10 khuya): "Sửa màu & sáng" + AI cho tóc

Chủ: khách gửi ảnh điện thoại bị ám màu (xanh, vàng do đèn và kệ hàng xung quanh), mờ, đục,
thiếu sáng, sáng không đều — 6 bộ "Màu studio" không sửa được ám màu môi trường; và cần AI
xử lý tóc cho ảnh mờ. Ảnh thử: `tmp/anh-the/am-mau` (ảnh khách + 2 ảnh giả lập), probe
`IAI_PORTRAIT_FIX_PROBE`.

- [x] Nhóm mới **"Sửa màu & sáng"** trong Chỉnh chân dung (trước "Màu studio"), mặc định 0,
  có nút **"Tự động"** (Khử ám màu 100, Cân sáng 80, Khử đục 60, Đều sáng mặt 50):
  - **Khử ám màu**: đo ám màu từ chính da mặt (`core/portrait/correct.rs`). Da mọi tông nằm
    gần một tia trong không gian (ln R/G, ln B/G); lệch ngang tia = ám màu (xanh lá, vàng,
    xanh dương, hồng) → khử hết; lệch dọc tia (cam ↔ lam) lẫn với da đậm/nhạt nên chỉ khử
    40%. Cân trắng kiểu "thế giới xám" sẽ sai vì cảnh toàn kệ xanh.
  - **Ấm / lạnh**: chỉnh tay phần còn lại (trục cam ↔ lam mà máy không tự đo chắc được).
  - **Cân sáng**: đưa độ sáng da về mức chuẩn (0,42 tuyến tính), −0,6…+2 EV, vùng sáng nén
    mềm nên trắng không cháy và vẫn là trắng.
  - **Khử đục**: trừ lớp "màn" đo ở 1% điểm tối nhất (tối đa 0,04) + thêm tương phản.
  - **Đều sáng mặt**: khớp một mặt phẳng vào ln(độ sáng) của da trong mặt, trừ độ nghiêng đó
    (trái/phải, trên/dưới); mắt, lông mày, môi đi theo da.
  - Ba thanh đầu + Ấm/lạnh nướng vào một LUT 33³ cho cả layer (như Màu studio, chạy trước
    look); layer "Chân dung" khi đó giữ cả ảnh. Layer cũ đọc 0.
- [x] **Tóc trong "Chi tiết mặt (AI)"**: GFPGAN vẽ lại cả tóc trong khung của nó, đẹp hơn
  Real-ESRGAN (x4plus: ~27 s cho vùng 360×270, general-x4v3: bệt) và không tốn thêm thời gian
  → dùng luôn cho vùng tóc (mask tóc Sapiens). Tóc vượt khung chuẩn (chỏm cao, tóc dài): chạy
  thêm một lượt GFPGAN khung rộng (mặt nhỏ tới 60%, +3–4 s), chỉ khi >3% tóc nằm ngoài.
- Thử: ảnh khách — da hết xanh-vàng, áo và hộp sữa về đúng màu; ảnh giả lập vàng-đục-mờ —
  màu về gần ảnh gốc, tóc rõ sợi tới chỏm.
- Còn hạn chế: ảnh thứ hai chủ dán trong chat không lưu thành file nên chưa thử trực tiếp;
  ám cam (đèn dây tóc) chỉ tự khử một phần — dùng thanh Ấm / lạnh; tóc dài quá khung rộng thì
  phần xa vẫn mờ; nền ngoài người không được làm nét.
- [x] Build Release, chủ test đợt 9: **OK** ("quá tuyệt").

## Đợt 10 (03/10 khuya): ẩn layer nguồn, mở lại không phân tích lại, Đều sáng da

Chủ sau khi test đợt 9:

- [x] **Bấm Áp dụng thì tắt con mắt layer nguồn** (Layer 1): có bóp mặt thì ảnh gốc bên dưới
  lòi ra quanh mặt, quên tắt là in luôn. Muốn ẩn được thì layer "Chân dung" phải chứa **cả
  ảnh** (trước chỉ chứa phần thay đổi) → giờ luôn chứa cả ảnh đã chỉnh. Mở lại layer "Chân
  dung": layer nguồn tạm hiện để vẽ xem trước, Hủy / Áp dụng lại ẩn. Một bước undo trả lại
  cả con mắt.
- [x] **Mở lại (bấm đúp) không chạy lại model**: giữ kết quả phân tích của phiên gần nhất
  (`PortraitCache` trong `portrait_ops.rs`): cùng tài liệu, cùng layer, điểm ảnh layer và vùng
  chọn y nguyên thì dùng lại ngay — kể cả chi tiết AI (GFPGAN) và dáng người đã chạy. Bỏ khi
  đóng tài liệu. Sau khi tắt app mở lại file thì vẫn phải phân tích một lần.
- [x] **"Đều sáng da"** (thay "Đều sáng mặt" của đợt 9, cùng thanh): da khuất đèn — dưới cằm,
  cổ, nửa mặt bên tối — được nâng về mức sáng của da được chiếu (phân vị 70 độ sáng dải rộng
  của da mặt); da sáng hơn mức đó dịu nhẹ. Bỏ qua chênh lệch nhỏ (≤ 0,12 ln) là khối tự nhiên
  của mặt (hốc mắt, cạnh mũi). Theo dải rộng (σ ≈ e/7) nên vân da và nét không đổi. "Tự
  động" đặt 60.
- [x] Sapiens2 cho tóc: model đã có ở máy chủ (`%APPDATA%\iAi\models\sapiens2-seg`, 1,6 GB)
  và trong bản portable, nên không cần làm gì thêm. Thiếu nó thì nhóm Tóc tắt và AI chỉ làm
  nét mặt → thêm dòng ghi chú báo rõ khi kéo thanh AI. Phương án dự phòng bằng BiSeNet (50 MB,
  có sẵn) để nhận tóc khi thiếu Sapiens2: chưa làm, làm nếu chủ định phát hành bản nhẹ.
- [x] Build Release, chủ test đợt 10: **OK** ("hoàn hảo").

## Đợt 11 (03/10 khuya): mặc định mới, sắp lại nhóm, thanh giảm màu

Chủ sau khi test đợt 10:

- [x] **Ảnh mới mở ra đã bật sẵn**: "Sửa màu & sáng" ở mức Tự động (Khử ám màu 100, Cân sáng
  80, Khử đục 60, Đều sáng da 60), Màu studio **"Trong trẻo"** (độ đậm 70), **Chi tiết mặt
  (AI) 60** (app tự chạy AI ngay sau khi phân tích; dòng trạng thái báo "đang tạo chi tiết
  AI…"). Layer cũ / file cũ vẫn đọc đúng giá trị đã lưu (thiếu thì 0).
- [x] Sắp lại nhóm: **Dáng mặt** còn Mặt thon, Bóp mặt, Cằm, Trán; **Mắt** thêm Mắt to, Mắt
  nghiêng; nhóm mới **Mũi** = Mũi thon + Sống mũi cao (chuyển từ Da); **Miệng, môi & răng**
  thêm Rộng miệng, Cười, Môi dày; **Chi tiết** = Chi tiết mặt (AI) (chuyển từ Da) + Tăng nét.
- [x] Thanh **giảm màu** (bớt bão hòa, giữ độ sáng, chạy trước phủ màu): "Giảm màu tóc",
  "Giảm màu tròng mắt", "Giảm màu lông mày". Môi đã có thanh hai chiều → đổi tên "Đậm / giảm
  màu môi" (trái = giảm màu), không thêm thanh trùng.
- [x] Có vùng chọn thì Sửa màu & sáng và Màu studio chỉ áp trong vùng chọn (như phần chỉnh da);
  trước đây look áp cả layer — giờ bật mặc định nên phải theo vùng chọn.
- [x] Build Release, chủ test đợt 11: **OK**.

## Đợt 12 (03/10 khuya): thanh màu mắt / tóc / lông mày hai chiều, mắt phủ cả lòng trắng

Chủ sau khi test đợt 11: "giảm màu tròng mắt mặc định là 0 nên không giảm thêm được, gặp mắt
đỏ là không giảm được; cho phủ luôn phần trắng mắt vì có người đau mắt đỏ toàn mắt; giảm màu
tóc cũng tương tự".

- [x] Ba thanh "Giảm màu …" (0..100, kéo phải mới giảm) đổi thành thanh **hai chiều** như
  "Đậm / giảm màu môi": **trái = giảm màu** (−100 = hết màu, giữ độ sáng), **phải = đậm màu**
  (+100 = màu đậm thêm 80%). Tên mới: "Đậm / giảm màu mắt", "Đậm / giảm màu tóc", "Đậm / giảm
  màu lông mày" (`eye_saturation`, `hair_saturation`, `brow_saturation`).
- [x] **Giảm màu mắt phủ cả mắt**: tròng mắt + toàn bộ khe mắt ngoài tròng (`FaceModel.sclera`,
  không lọc theo độ sáng như mask "Trắng mắt", nên lòng trắng đỏ sẫm vẫn được phủ). Lòng trắng
  khử đỏ theo luật riêng (`whitened`): máu chỉ làm tối kênh lục và lam, nên mức trắng thật nằm
  gần kênh sáng nhất hơn là độ sáng trung bình → nâng 75% về phía đó, không để lại mảng xám.
  Tròng mắt vẫn giảm màu giữ nguyên độ sáng (đồng tử đỏ do flash thành xám tối). Chiều đậm màu
  chỉ áp cho tròng.
- [x] **Giảm màu về đúng màu xám của ảnh**: ảnh ám màu thì "Khử ám màu" chạy sau phần chỉnh
  mặt, nên giảm màu về R=G=B rồi khử ám sẽ ra lòng trắng / tóc ngả tím-xanh (thấy rõ trên
  `khach_1.jpg`). Giờ mắt, tóc, lông mày giảm / đậm màu quanh trục xám mà cân trắng sẽ đưa về
  trung tính (`correct::grey_axis`, tính từ Khử ám màu + Ấm / lạnh đang đặt) → sau khi khử ám
  là xám thật. Không khử ám thì như cũ.
- [x] File / layer lưu bằng bản đợt 11 (`hair_fade`, `iris_fade`, `brow_fade` 0..100) mở ra
  thành nửa trái của thanh mới (`PortraitSettings::from_saved`).
- [x] Probe `IAI_PORTRAIT_EYE_PROBE` (`tmp/anh-the/mat-do`): mắt thật và mắt đỏ giả lập.
- [x] Build Release, chủ test đợt 12: **OK**.

## Đợt 13 (03/10 khuya): "Sáng da" dùng thuật toán thanh Midtones của Develop

Chủ sau khi test đợt 12: "đổi thanh tăng sáng da bằng thuật toán xử lý của thanh Midtones ở
Develop".

- [x] "Sáng da" trước là một đường cong áp từng điểm ảnh (1 − (1 − v)^p): sáng đều cả vùng tối
  lẫn vùng sáng, bệt vân da. Giờ chạy đúng tầng tone của Develop (Develop3, chỉ đặt Midtones;
  thanh 100 = Midtones +200): ánh sáng tuyến tính nhân với hệ số theo **tông của vùng da xung
  quanh** (dải thấp `low2` của da, sau "Đều sáng da"). Tông giữa đổi nhiều, vùng rất tối và
  rất sáng gần như đứng yên, lỗ chân lông và nếp da giữ nguyên tương phản (`skin_tone`,
  `midtoned` trong `effects.rs` — cùng kiểu với "Sáng tóc" dùng Blacks của Develop).
- [x] Thanh thành **hai chiều** như Midtones của Develop: trái = da tối hơn, phải = sáng hơn.
  Giá trị dương đã lưu giữ nguyên nghĩa.
- [x] Mắt, môi, lông mày, chân mũi (không phải da) đổi tông **theo tông của chính chúng**, phần
  mà mask da không phủ: môi và lòng trắng sáng / tối theo da, mi, tròng, tóc che mắt vẫn tối.
  Không làm vậy thì kéo tối sẽ lộ viền sáng quanh lông mày, môi, chân mũi (thấy trên
  `brick_woman.jpg` ở −100).
- [x] Probe `IAI_PORTRAIT_SKIN_TONE_PROBE` (`tmp/anh-the/sang-da`): 0 | 50 | 100 | −50 | −100.
- [x] Build Release, chủ test đợt 13: **OK**.

## Đợt 14 (03/10 khuya): Xếp ảnh in ngay trong Chỉnh chân dung, trang 3×4 + 4×6

Chủ sau khi test đợt 13 (kèm ảnh mẫu trang in): đưa phần "Xếp ảnh in" của AI Image Studio vào
cửa sổ Chỉnh chân dung; thêm một hàng xếp cả hai loại như mẫu; ảnh 4×6 nền trắng có viền để
nhận ra khi cắt bằng kéo. Chia việc: ảnh quá khó → AI Image Studio (Gemini / ChatGPT); ảnh
chụp đẹp hoặc không có mạng → Làm ảnh thẻ + Chỉnh chân dung là đủ.

Đo ảnh mẫu (1444×2000): đúng là giấy **13×18**, 111,5 px/cm; 6 tấm 3×4 nằm ngang (2 hàng × 3)
nền xanh `(5, 148, 242)`, dưới là 2 tấm 4×6 nằm ngang nền trắng viền đỏ; lề trái = lề trên
≈ 0,47 cm; khe ≈ 10 px ở 600 dpi; nửa dưới tờ giấy để trống.

- [x] **Trang hỗn hợp** `Sheet::Mixed` (`core/imposition.rs`): 13×18, 6 tấm 3×4 trên 2 tấm 4×6,
  canh như mẫu (hàng 4×6 nằm giữa, khối 3×4 canh trái theo nó, lề trên = lề trái). Thành nút
  thứ tư: "Xếp 13×18 — 6 tấm 3×4 + 2 tấm 4×6". Mỗi cỡ một nhóm layer riêng.
- [x] **Đổi nền theo cỡ**: ảnh đã tách người (Background một màu phẳng + người có vùng trong
  suốt bên trên, đúng kiểu "Làm ảnh thẻ" để lại) thì mỗi cỡ được đặt lên nền riêng — mặc định
  **3×4 xanh, 4×6 trắng**, đổi được bằng hai hàng nút "Nền 3×4 / Nền 4×6". Ảnh phẳng (ảnh AI
  trả về, ảnh chưa tách nền) giữ nguyên nền của nó và thanh trạng thái báo rõ.
- [x] **Viền cắt** đỏ **2 px** (0,08 mm) quanh ảnh nền trắng (nền trắng do chọn, hoặc ảnh phẳng
  có mép trắng): vẽ trong khe cắt, không lấn vào ảnh; khe < 4 px thì vẽ đè lên mép ảnh. (Bản
  đầu 4 px, chủ thấy to quá → 2 px.)
- [x] **"Xếp ảnh in" trong Chỉnh chân dung**: nhóm cuối của hộp thoại; bấm một trang = Áp dụng
  rồi xếp ra trang in mới. Cùng một đoạn giao diện với AI Image Studio
  (`ui/dialogs/print_sheet.rs`); khe cắt và nền nhớ trong prefs.json (khóa `print_sheet`).
- [x] Tấm 4×6 lấy từ ảnh đã cắt 3×4 nên bị cắt bớt ~5% mỗi bên và phóng 1,58 lần (≈ 380 ppi):
  đủ in, kém nét hơn tấm 3×4 một chút. Muốn nét hơn phải cho "Làm ảnh thẻ" cắt ở cỡ lớn — chưa
  làm.
- [x] Probe `IAI_PRINT_SHEET_PROBE` (`tmp/anh-the/xep-in`): ảnh → Làm ảnh thẻ → trang hỗn hợp.
- Thấy khi thử (chưa sửa, lỗi có từ trước của "Làm ảnh thẻ"): ảnh sát mép trên (`khach_1.jpg`)
  để lại một vạch mờ cách mép trên ảnh thẻ ~8 px — chỗ khung vượt khỏi ảnh gốc.
- [x] Build Release, chủ test đợt 14: **OK**, chỉ yêu cầu viền mảnh lại còn 2 px (đã sửa).
- [x] Build Release, chủ xem lại viền 2 px: **OK** (04/10).

## Đợt 15 (04/10): hết vạch mờ ở mép ảnh thẻ, Trắng mắt / Trắng răng theo xám của ảnh

Chủ chọn hai việc trong danh sách "còn gì đáng làm" (mục 2 và 4).

- [x] **Vạch mờ ở mép ảnh thẻ** (thấy trên `khach_1.jpg`, in ra sẽ lộ): khi khung ảnh thẻ
  vượt khỏi mép ảnh gốc, bước cắt lấy mẫu lại mặt nạ của layer với nền trắng (= hiện) cho phần
  ngoài ảnh. Đúng hàng điểm ảnh sát mép, mặt nạ bị trộn về phía trắng trong khi điểm ảnh còn
  nửa đục → lộ một vệt nền cũ (cả ở "Layer 1" lẫn "Ảnh gốc"). Sửa trong
  `Canvas::resample_footprint` (`core/canvas/geometry_ops.rs`): mặt nạ giữ nguyên giá trị ở
  mép của chính nó (`keep_edge`), chỉ phần nằm hẳn ngoài mới là trắng. Áp cho mọi lệnh Crop
  có layer mang mặt nạ. Test `a_frame_past_the_photos_edge_leaves_no_line_of_the_old_background`
  (hỏng khi bỏ bản sửa). Đo lại trên trang in từ `khach_1.jpg`: hết vạch.
- [x] **"Trắng mắt" / "Trắng răng"** giảm màu quanh trục xám của ảnh (`balanced` +
  `bleached` trong `effects.rs`), như thanh giảm màu mắt / tóc ở đợt 12: ảnh ám màu sau "Khử
  ám màu" không còn ngả xanh ở lòng trắng và răng. Ảnh không khử ám thì y như cũ.
- Việc còn lại và kế hoạch tiếp: `docs/planning/KE_HOACH_TIEP_THEO_2026-10-04.md`.
- [x] Build Release, chủ test đợt 15: **OK** (04/10).

## Đợt 16 (04/10): tấm 4×6 nét hơn, dọn bản build cũ

Chủ sau khi test đợt 15: "tiếp tục, Tấm 4×6 nét hơn" và "Dọn bản build cũ" (mục 1.1 và 1.5
của `KE_HOACH_TIEP_THEO_2026-10-04.md`).

- [x] **"Làm ảnh thẻ" cắt ở 1043×1417 px** (trước là 661×898), in ra vẫn đúng 2,8×3,8 cm
  (≈ 947 ppi): `PRINT_PX` / `PRINT_PPI` trong `core/id_photo.rs` (bỏ `output_size()`). Cao
  bằng đúng ô 4×6 ở 600 dpi (1417 px) và cùng tỉ lệ với ô 3×4. Khung hình, tỉ lệ mặt không đổi.
- [x] **Trang in** (`stamp` trong `app/actions/impose.rs`): tấm 4×6 lấy thẳng điểm ảnh của
  ảnh thẻ (cắt giữa 1043 → 945, không lấy mẫu lại); tấm 3×4 thu 1043 → 661. Ảnh thẻ cũ
  661×898 @ 600 và ảnh cắt tay vẫn xếp được như trước.
- [x] Hộp thoại ghi "2,8×3,8 cm · 1043×1417 px"; "Xếp ảnh in" vẫn nhận ra ảnh là 3×4 vì nhận
  theo cm.
- Không đổi: preset Crop "Ảnh thẻ 3×4 (2.8×3.8cm 600dpi)" vẫn 661×898 (cắt tay).
- Đo trên 4 ảnh (`IAI_ID_PHOTO_PROBE`, `tmp/anh-the/net-4x6`, dựng lại ô 4×6 / 3×4 như `stamp`
  rồi đo chi tiết mịn — trung bình |Laplacian| vùng mặt, sau / trước):
  - Ô 4×6: `gray_man` (khung 1488×2021 px trên ảnh gốc) **2,16 lần**; `beard_glasses` (cận mặt)
    **1,54 lần**; `la_woman` (khung 663×901) 1,10; `khach_1` (khung 710×964) 1,06. Ảnh gốc mà
    khung mặt chưa tới ~1000 px thì không có thêm chi tiết để lấy — đúng như dự tính.
  - Ô 3×4: 0,94–1,00 lần, nhìn bằng mắt không khác (`so_3x4_*.png`).
- [x] **Dọn `target`** (38 GB → xem `KE_HOACH_TIEP_THEO`): xóa `portrait-a3`, `portrait-b1`,
  `portrait-test`, `probe`, `tmp` và `debug`; giữ `release`. `debug` dựng lại sạch khi chạy
  test. Hai ảnh so sánh cũ nằm lẫn trong `target/portrait-test` chuyển sang `tmp/so-sanh-cu`.
- [x] Build Release, chủ test đợt 16: **OK** (04/10).
- Chủ thấy khi test: Áp dụng Chỉnh chân dung rồi mới Crop thì bấm đúp layer "Chân dung" không
  mở lại được (layer nhớ kích thước layer ảnh gốc; crop làm đổi kích thước → báo "Không còn
  layer ảnh gốc"). **Chủ chốt 04/10: bỏ qua, coi là tính năng — không sửa, đừng đề xuất lại.**
  Cách dùng: crop trước rồi chỉnh, hoặc Ctrl+Z về trước lúc crop.

## Đợt 17 (04/10): lưu / nạp công thức chân dung, hai lỗi nhỏ

Chủ sau khi test đợt 16: "kế hoạch còn lại gì thì tiếp tục làm". Hỏi lại hai điểm, chủ chọn:
phần công thức **chỉ làm lưu / nạp** (không làm chạy hàng loạt); push + portable **chưa, để sau**.

- [x] **"Công thức" trong Chỉnh chân dung** (`ui/dialogs/portrait.rs`): một hàng ngay dưới dòng
  trạng thái — ô chọn công thức đã lưu, nút "Lưu…" (gõ tên rồi Enter / Lưu; trùng tên thì ghi
  đè), nút thùng rác xóa công thức đang chọn. Công thức = toàn bộ thanh kéo của hộp thoại
  (kể cả Màu studio, Sửa màu & sáng, Dáng mặt). Ô chọn tự hiện tên công thức nào trùng khớp
  với các thanh đang đặt. Lưu trong prefs.json (khóa `portrait_presets`). Ảnh mới vẫn bắt đầu
  từ mặc định như chủ đã chốt; muốn dùng công thức thì chọn.
- [x] Phím `[` `]` đổi đúng cỡ / độ cứng công cụ đang cầm (Smudge, Dodge, Burn, Quick
  Selection); Pencil vẽ đúng cỡ, độ mờ, màu của Brush (`092b2e2`).
- [x] Edit ▸ Smart Fill (AI) giữ vân ảnh ở độ phân giải đầy đủ (`5295d34`).
- Chưa nhìn tận mắt hàng "Công thức" trên màn hình (bố cục tính theo bề rộng 320 px của hộp
  thoại); test tự động chỉ kiểm lưu / ghi đè / đọc lại và vẽ không lỗi.
- [x] Build Release (`iai-dot17.exe`); chủ đã dùng hàng "Công thức" và yêu cầu tiếp đợt 18.
  Chủ chưa nói gì về phím `[` `]`, Pencil, Smart Fill.

## Đợt 18 (04/10): công thức ngay trong "Làm ảnh thẻ", công thức có sẵn, bỏ chú thích thanh kéo

Chủ sau khi dùng đợt 17: đặt công thức ở bảng Làm ảnh thẻ luôn, cắt xong tự áp vào; làm sẵn
vài công thức (thông số tôi tự quyết, chủ sẽ chỉnh và lưu lại sau); ở Chỉnh chân dung bỏ
chú thích hiện ra mỗi khi rê chuột vào thanh kéo ("rất rối").

- [x] **"Công thức" trong Làm ảnh thẻ** (`ui/dialogs/id_photo.rs`): ô chọn ngay dưới "Xong thì
  mở Chỉnh chân dung" — "Mặc định" hoặc một công thức đã lưu; nhớ lựa chọn trong prefs.json
  (khóa `id_photo_preset`). Cắt xong, Chỉnh chân dung mở ra với sẵn các thanh của công thức
  đó (xem trước hiện ngay), chủ chỉ còn bấm Áp dụng hoặc một trang ở "Xếp ảnh in"
  (`App::begin_portrait_from`). Tôi chọn "nạp sẵn vào hộp thoại" chứ không tự bấm Áp dụng
  thay chủ, để còn chỉnh thêm và xếp trang in ngay tại đó.
- [x] **Năm công thức có sẵn** (`core/portrait/presets.rs`), không cái nào đổi dáng mặt (là
  ảnh thẻ): "Ảnh thẻ nữ" (mịn hơn, sáng da nhẹ, môi tươi), "Ảnh thẻ nam" (giữ vân da, nét
  hơn), "Trẻ em" (rất nhẹ), "Lớn tuổi" (mịn + giữ khối, quầng thâm, răng), "Nhẹ, tự nhiên".
  Cấp một lần cho mỗi máy (prefs.json khóa `portrait_presets_built_in`): chủ sửa / xóa thì
  không bị cấp lại; công thức chủ đã lưu trùng tên được giữ nguyên.
- [x] **Chỉnh chân dung không còn chú thích nổi** trên các thanh kéo, thanh cỡ cọ / độ cứng và
  tiêu đề nhóm. Còn chú thích ở nút bấm, ô tích, ô màu studio (ít khi rê qua).
- Probe `IAI_PORTRAIT_PRESET_PROBE` (`tmp/anh-the/cong-thuc`): ảnh gốc | từng công thức có
  sẵn. Đã xem trên 2 ảnh thẻ: tự nhiên, khác nhau vừa phải, không lỗi.
- [x] Build Release, chủ test đợt 18: **OK** (04/10).

## Đợt 19 (04/10): giao diện Chỉnh chân dung / Develop / Làm ảnh thẻ

Chủ sau khi test đợt 18: tên nhóm (Da, Mắt, Miệng…) chữ lớn và đậm hơn, chữ thanh kéo bên
trong nhỏ hơn; mỗi tính năng là một hàng riêng, rê chuột qua thì sáng lên; bảng Develop
cũng vậy, thêm việc cho nhập số ở từng thanh kéo; icon và thanh kéo ở Làm ảnh thẻ, Chỉnh
chân dung dùng Phosphor cho đồng bộ.

- [x] **Tiêu đề nhóm dùng chung** (`widgets::section_header`): thanh cao 30 px có nền, icon
  Phosphor, tên nhóm chữ **đậm 14 px** (font Segoe UI Bold, họ font `ui_bold` đăng ký trong
  `theme::add_bold_font`), chấm xanh khi nhóm đang có thanh được chỉnh, mũi tên bên phải; rê
  chuột thì sáng lên; bấm cả thanh để mở / đóng. Dùng cho 13 nhóm của Chỉnh chân dung và 9
  mục của Develop (trước đây Develop phải bấm đúng mũi tên nhỏ).
- [x] **Hàng thanh kéo** (`widgets::stacked_slider`, dùng chung cho Chỉnh chân dung, Develop
  và các hộp thoại khác dùng kiểu thanh này): tên thanh 11,5 px (trước 12,5), vạch ngăn mảnh
  giữa các hàng, cả hàng sáng lên khi rê chuột / đang kéo.
- [x] **Nhập số**: ô giá trị bên phải mỗi thanh nay có khung như ô nhập, rê vào đổi con trỏ
  chữ và viền xanh; bấm vào gõ số, Enter hoặc bấm ra ngoài để nhận, Esc để bỏ. (Việc bấm vào
  số để gõ đã có sẵn từ trước nhưng không có dấu hiệu nào cho thấy.)
- [x] **Làm ảnh thẻ**: thanh "Khung rộng hơn mẫu (%)" đổi sang thanh kéo của app (gõ số
  được); nút có icon Phosphor. Chỉnh chân dung: nút Áp dụng / Hủy / Mặc định / Về 0 / Lưu… /
  Tự động có icon.
- [x] Công cụ tự xem giao diện: `src/ui/snapshot.rs` (chỉ khi test) vẽ một hộp thoại ra PNG
  không cần cửa sổ hay GPU. Chạy: đặt `IAI_UI_SNAPSHOT` là thư mục rồi
  `cargo test --lib -- --ignored probe_dialog_snapshot probe_panel_snapshot`. Ảnh trước / sau
  ở `tmp/ui/truoc`, `tmp/ui/sau`.
- Tôi tự chọn icon từng nhóm (Phosphor không có hình mũi, lông mày: dùng tam giác, vòng cung).
- [x] Build Release, chủ test đợt 19: **OK** (04/10), kèm ba việc của đợt 20.

## Đợt 20 (04/10): ô nhập số bị nhảy lên mức tối đa, Develop chỉ mở một mục, icon môi

Chủ sau khi test đợt 19: bấm vào ô số của thanh kéo thì thanh nhảy lên mức tối đa và không gõ
được (cả Develop lẫn Chỉnh chân dung); Develop cũng nên chỉ mở một mục, mở mục này thì đóng
mục kia; icon cái răng đổi thành miệng hoặc môi.

- [x] **Ô nhập số** (`widgets::stacked_slider`) — hai lỗi chồng nhau, có từ khi tính năng
  gõ số được viết (trước đợt 19 không ai bấm vì ô không có khung):
  1. Cú bấm trong ô bị xử lý như bấm lên thanh: egui quên "điểm bắt đầu bấm" đúng ở khung
     hình nhả chuột (là khung hình của cú bấm), nên code tưởng không bấm trong ô và kéo thanh
     tới chỗ con trỏ = hết bên phải = mức tối đa. Sửa: ở khung hình đó lấy chính vị trí bấm.
  2. Ô nhập vừa hiện đã mất con trỏ gõ: xin focus trước khi ô được tạo, ngay trong khung
     hình của cú bấm, thì egui bắt widget đang focus mà cú bấm không trúng phải nhả focus (ô
     chưa tồn tại lúc cú bấm được định vị). Sửa: xin focus sau khi ô đã được tạo, kèm bôi đen
     cả số để gõ là thay luôn.
  Test `a_click_in_the_value_box_types_a_value_and_never_moves_the_slider` (hỏng trước khi
  sửa: giá trị 40 → 100) và `a_click_on_the_track_still_moves_the_slider`.
- [x] **Develop chỉ mở một mục** (`develop::set_section_open`): mở mục nào thì các mục khác
  đóng lại, kể cả Scopes; bấm lại mục đang mở thì đóng hết. Mặc định mở Light. Prefs cũ đang
  lưu nhiều mục mở thì giữ Light (nếu có), không thì mục trên cùng.
- [x] **Icon môi** cho nhóm "Miệng, môi & răng": Phosphor không có hình miệng / môi nên vẽ
  bằng nét cùng kiểu (`widgets::HeaderIcon::Lips`).
- [x] **Esc khi đang gõ số chỉ thoát ô nhập** (tôi làm thêm, chủ không yêu cầu): trước đây Esc
  đóng luôn Chỉnh chân dung / Develop và bỏ hết các thanh đã chỉnh. Nay Esc lần một bỏ số đang
  gõ, Esc lần nữa mới đóng hộp thoại; Enter nhận số chứ không bấm Áp dụng thay chủ
  (`widgets::typing_in_a_field` — egui bỏ focus khi có Esc trước khi giao diện của khung hình
  chạy, nên phải nhớ trạng thái của khung hình trước). Test
  `esc_and_enter_in_a_value_box_act_on_the_value_not_on_the_dialog`.
- Đã tự xem ảnh probe (`tmp/ui/sau`); hành vi bấm / gõ kiểm bằng test giả lập chuột và phím.
- [x] Build Release, chủ test đợt 20: **OK** (04/10), kèm một việc của đợt 21.

## Đợt 21 (04/10): bấm vào ô số ở Develop thì cửa sổ nháy

Chủ sau khi test đợt 20: ở cửa sổ Develop, bấm vào ô nhập số thì giao diện "nháy nhẹ một
cái", muốn đứng yên; Chỉnh chân dung thì không bị.

- [x] **Ảnh xem trước của Develop không còn nháy khi bấm ô số** — nguyên nhân riêng của
  Develop: hễ nhấn chuột lên một điều khiển, panel báo "đang kéo" và app chuyển ảnh xem trước
  sang đường dựng nhanh, nhả chuột thì dựng lại bản chính xác
  (`set_develop_controls_pointer_down`). Đúng cho việc kéo thanh; bấm vào ô số thì không kéo
  gì, chỉ thấy ảnh và dòng "Preview: …" nháy. Sửa: cú nhấn bắt đầu trong ô số hoặc trên tiêu đề
  mục được đánh dấu (`widgets::note_plain_press` / `plain_press`), và panel không báo "đang
  kéo" cho nó. Bấm tiêu đề mục để mở / đóng cũng hết nháy.
- [x] **Các hàng bên dưới không còn nhích 2 px khi ô nhập hiện ra** (cả Develop lẫn Chỉnh chân
  dung): `ui.put` kéo con trỏ bố cục của hàng lên mép dưới ô nhập; nay ô nhập nằm trong một
  `new_child` riêng, không đụng bố cục.
- Test `only_a_press_on_a_slider_track_puts_the_preview_in_drag_mode` (hỏng trước khi sửa ở cả
  hai điểm): nhấn trên rãnh thanh = "đang kéo"; bấm ô số / tiêu đề = không; vị trí các hàng
  trước và trong lúc gõ giống hệt nhau.
- Tôi chưa nhìn tận mắt hiện tượng nháy trên app thật; hai nguyên nhân trên là thứ đọc code và
  test giả lập tìm ra.
- [ ] Build Release, chủ test.

## Đợt 22 (04/10): ô số lên cùng dòng với tên thanh như PTS, đang gõ vẫn kéo được ngay

Chủ gửi ảnh mục Light của PTS: tên thanh bên trái và ô số bên phải nằm chung một dòng, thanh
kéo chạy hết chiều ngang ở dòng dưới; muốn Develop và Chỉnh chân dung như vậy để đỡ tốn chiều
ngang. Kèm một lỗi: đã bấm vào ô số rồi thì không nhấn kéo thanh được, phải Enter hoặc Esc
trước.

- [x] **Bố cục hàng thanh kéo kiểu PTS** (`widgets::stacked_slider`, dùng chung cho Develop,
  Chỉnh chân dung, Làm ảnh thẻ, Refine Selection): dòng trên là tên + ô số (ô cao 17 px, sát
  phải), dòng dưới là thanh kéo chạy từ mép trái tên tới mép phải ô số. Trước đây thanh bị ép
  giữa cột tên 92 px và ô số nên chỉ dài khoảng nửa hàng. Chiều cao hàng giữ nguyên 33 px nên
  không mục nào dài thêm. Tên quá dài ở hàng hẹp thì bị cắt trước ô số, không đè lên.
- [x] **Đang gõ trong ô số mà nhấn chỗ khác thì nhận số ngay và cú nhấn đó có tác dụng luôn**
  (kéo thanh, bấm thanh, bấm thanh khác). Hai nguyên nhân:
  1. egui chỉ cho ô nhập nhả focus khi có "cú bấm" (nhấn rồi nhả tại chỗ); nhấn rồi kéo không
     phải cú bấm nên ô vẫn mở, và hàng đang có ô mở thì bỏ qua mọi thao tác kéo. Một cú bấm đơn
     lên thanh cũng chỉ đóng ô, phải bấm lần hai thanh mới chạy. Sửa: hễ có nút chuột nhấn xuống
     ngoài ô thì hàng tự nhận số đang gõ, đóng ô, rồi xử lý cú nhấn như bình thường.
  2. Ô nhập nằm trong một `new_child`, mà `new_child` lấy một id tự động của ui cha, nên mọi
     widget phía dưới đổi id mỗi khi ô hiện / ẩn; cú nhấn làm ô đóng rơi vào id của khung hình
     trước, không còn tồn tại, và thanh bên dưới không nhận được thao tác kéo. Sửa: khi không có
     ô nhập vẫn lấy một id (`skip_ahead_auto_ids(1)`) để id các hàng đứng yên.
- Test (cả ba hỏng trước khi sửa): `a_drag_on_the_track_while_typing_takes_the_typed_value_and_drags_at_once`,
  `a_click_on_the_track_while_typing_moves_the_slider_with_that_click`,
  `a_drag_on_another_slider_while_typing_ends_the_typing_too` trong `widgets.rs`; trong bảng
  Develop thật: `a_drag_while_typing_in_a_value_box_needs_no_enter_first`.
- Đã tự xem ảnh probe (`tmp/ui/truoc` so với `tmp/ui/sau`); probe Develop có thêm
  `develop_detail.png` (mục Detail + Effects).
- Chiều rộng bảng Develop (360) và hộp thoại Chỉnh chân dung (320) chưa đổi; thanh kéo nay dài
  gần gấp đôi nên có thể thu hẹp bảng nếu chủ muốn ảnh rộng hơn.
- Code `1c9a0c6`; bản test `target/release/iai.exe` (build 04/10 17:32).
- [x] Build Release, chủ test đợt 22: **OK** (04/10), kèm việc của đợt 23 (gộp Làm ảnh thẻ vào
  Chỉnh chân dung; chủ muốn xem bản vẽ trước, duyệt rồi mới làm).

## Đợt 23 (04/10 tối): gộp "Làm ảnh thẻ" vào "Chỉnh chân dung", nền xanh, cỡ 2×3 / 4×6, cắt lại

Chủ muốn một bảng thay cho hai: trên cùng hai ô chức năng — **Ảnh thẻ** (ô tích nền xanh / nền
trắng, nam / nữ / trẻ em…) và **Chân dung** (không tách nền, y như cũ). Tôi vẽ phác, chủ duyệt
04/10 kèm các ý sau (đây là yêu cầu của chủ, không phải suy đoán của tôi):

- Lưu / xóa công thức **giữ nguyên** ở ô Chân dung; ô Ảnh thẻ chỉ chọn.
- Màu nền xanh: **#0090FF**.
- Thêm cỡ cắt **4×6** và **2×3**; Xếp ảnh in cũng có hai cỡ này. Chủ chỉ có hai loại giấy:
  **10×15** và **13×18 cm**; số tấm mỗi trang tôi tự tính.
- Sau khi app tự cắt và tự chỉnh màu, nếu khung cắt lệch thì **cho cắt lại ngay khi bảng đang mở**.

Thiết kế tôi chốt (phần chủ giao "tự tính toán"):

- **Một hộp thoại** `portrait_dialog`, hai ô ở đầu. Mục menu "Làm ảnh thẻ…" mở ở ô Ảnh thẻ
  (chưa phân tích chân dung), "Chỉnh chân dung…" mở ở ô Chân dung (phân tích ngay như cũ).
- **Ô Ảnh thẻ**: Nền (Trắng / Xanh / Giữ nền gốc) · Mẫu (các công thức, dạng ô bấm) · Cỡ (2×3 /
  3×4 / 4×6 / Không cắt) · Xoay thẳng theo mắt · Khung rộng hơn mẫu (%) · nút "Làm ảnh thẻ".
  Bỏ ô tích "Xong thì mở Chỉnh chân dung" (giờ là cùng một bảng).
- **Cỡ cắt** đều cao 1417 px để trang in nào cũng lấy được điểm ảnh gốc: 3×4 = 1043×1417
  (2,8×3,8 cm, 947 ppi, như cũ); 4×6 = 945×1417 (600 ppi); 2×3 = 945×1417 (1200 ppi, in ra 2×3 cm).
- **Cắt lại**: lần chạy đầu giữ lại mask tách nền + mốc mặt (phần AI tốn giây). Sau đó đổi Cỡ,
  Khung rộng hơn, Xoay thẳng hay bấm các nút dịch khung (lên / xuống / trái / phải / xoay) thì
  app hoàn tác bước ảnh thẻ rồi cắt lại từ mask đã giữ, không chạy AI lại; phân tích chân dung
  chạy lại sau lần chỉnh cuối, các thanh kéo giữ nguyên. Đổi Nền chỉ tô lại lớp nền.
- **Xếp ảnh in**: bảng nút 3 cỡ × 2 giấy + trang ghép 13×18 (6 tấm 3×4 + 2 tấm 4×6). Trang một
  cỡ in **đúng nền của ảnh** (chọn ở ô Nền); riêng trang ghép vẫn chọn nền riêng từng cỡ.
  (Trước đây mọi trang đều lấy nền theo ô "Nền 3×4 / Nền 4×6" của Xếp ảnh in — đổi vì giờ ảnh
  đã có nền chọn sẵn, in ra khác màu trên màn hình là dễ in hỏng.)

Việc:

- [x] Lõi xếp trang (`29766a5`): `PhotoKind::Id2x3` (472×709 px ở 600 dpi), xanh #0090FF, đủ
  trang cho hai giấy, trang một cỡ theo nền ảnh. Số tấm với khe cắt 10 px: 2×3 = 21 (10×15) /
  32 (13×18); 3×4 = 10 / 18; 4×6 = 4 / 8; trang ghép 13×18 = 6 tấm 3×4 + 2 tấm 4×6.
- [x] Lõi ảnh thẻ (`00387a9`): `IdPhotoOptions` có `size` + `backdrop`, `cut_out` thay
  `white_background` (prefs cũ vẫn đọc được); `analyse` (AI, giữ lại) tách khỏi `plan` (lập
  khung, nhanh); `Nudge` dịch / xoay người trong khung; `set_backdrop` đổi nền bằng một bước
  hoàn tác riêng chỉ đụng lớp Background.
- [x] App (`00387a9`, `id_photo_ops.rs`): giữ ảnh gốc + phân tích trong lúc bảng mở (`Kept`);
  yêu cầu mới thì hoàn tác các bước ảnh thẻ rồi áp khung mới; đổi nền chỉ tô lại; phân tích chân
  dung khởi động lại 0,6 s sau lần chỉnh cuối, từ đúng các thanh đang có. Yêu cầu tới lúc đang
  chạy thì xếp hàng, lấy cái mới nhất.
- [x] Giao diện (`00387a9`): hai ô ở đầu `portrait_dialog`; mục Ảnh thẻ ở
  `dialogs/id_photo.rs` (`id_photo_section`); hàng "Khung" (← → ↑ ↓ xoay trái / phải, Đặt lại)
  hiện sau khi ảnh đã làm, mỗi lần bấm dịch 2% chiều cao ảnh hoặc xoay 0,5°; Xếp ảnh in thành
  bảng nút cỡ × giấy. Hai mục menu cũ cùng mở bảng này, mỗi mục một ô.
- [x] Probe ảnh (`tmp/ui/sau`: `chan_dung_anh_the.png`, `chan_dung_anh_the_xong.png`,
  `xep_anh_in.png`) + test giả lập; test thật trên ảnh mẫu
  `an_id_photo_is_framed_again_from_what_was_kept` (làm → cắt lại → đổi nền → cắt lại; cắt lại
  ~1,2 s ở bản debug, không chạy AI). Test đầy đủ qua (1915 + 15).
- Ghi chú kỹ thuật: công cụ probe (`ui/snapshot.rs`) nay chạy 10 khung hình thay vì 4 — cửa sổ
  egui cao cần vài khung mới ổn định kích thước, 4 khung làm phần cuối bảng trống. Test không
  còn đọc / ghi `prefs.json` thật (`load_pref` / `save_pref` bỏ qua khi `cfg!(test)`).
- Chưa làm (không ai yêu cầu, ghi lại để khỏi quên): kéo thả khung trực tiếp trên ảnh (hiện là
  nút mũi tên); nền "Theo ảnh" cho trang ghép.
- Bản test `target/release/iai.exe` (build 04/10 18:53).
- [x] Build Release, chủ test đợt 23: **OK** (04/10), kèm việc của đợt 24.

## Đợt 24 (04/10 tối): mở bảng không tự làm đẹp, đổi tên "Auto retouch", bỏ mục menu Làm ảnh thẻ

Chủ sau khi test đợt 23: mở ô Chân dung là app tự chạy làm đẹp ngay, rồi nếu làm ảnh thẻ thì
lại làm đẹp lần hai. Chủ muốn: thêm nút "tự động làm đẹp" ở ô Chân dung, bấm mới chạy; mở bảng
lần đầu chỉ hiện bảng, không làm gì; "đổi tên thành Auto retouch, xóa menu Image ▸ chỉnh ảnh
thẻ".

- [x] **Mở bảng không phân tích gì** (`App::open_portrait_dialog`). Ô Chân dung có nút
  "Tự động làm đẹp" (`start_portrait_retouch`): bấm mới nhận diện khuôn mặt và làm đẹp theo mức
  thường dùng; khi đang chạy, nút nhường chỗ cho dòng trạng thái. Bấm ô để đổi qua lại không
  còn tự khởi động gì. Ngoại lệ giữ như cũ: đang chọn layer "Chân dung" đã áp dụng (hoặc bấm
  đúp nó ở bảng Layer) thì mở lại để chỉnh tiếp ngay.
- [x] **Ô Ảnh thẻ không đổi**: bấm "Làm ảnh thẻ" thì cắt, tách nền rồi làm đẹp theo Mẫu — nay
  là lần làm đẹp duy nhất.
- [x] **Layer không chỉnh được** (khóa, không phải ảnh, CMYK…): lý do hiện ngay dưới nút, không
  chỉ ở thanh trạng thái (`shell.portrait_error`).
- [x] **Tên**: bảng và mục menu thành "Auto retouch" (Image ▸ Auto retouch…; nút ở bảng AI cũng
  vậy); bỏ mục Image ▸ Làm ảnh thẻ…. Tôi hiểu "đổi tên" là đổi tên bảng / mục menu, còn nút mới
  giữ chữ "Tự động làm đẹp" như chủ gọi — chờ chủ xác nhận. Lưu ý: bảng AI đã có mục riêng tên
  "AI Auto Retouch" (tính năng khác).
- [x] **Nhớ ô dùng lần trước** (prefs `portrait_side`), vì chỉ còn một mục menu.
- Test: `the_dialog_opens_idle_and_retouches_only_when_asked` (app),
  `a_tile_asks_for_the_other_side_and_each_side_shows_its_own_controls` (nút chỉ có khi chưa
  chạy, bấm mới gửi yêu cầu). Probe `chan_dung_cho.png`. Test đầy đủ qua (1915 + 15).
- Code `689f844`; bản test `target/release/iai.exe` (build 04/10 19:47).
- [x] Build Release, chủ test đợt 24: chủ không báo lỗi, giao tiếp việc đợt 25 (04/10).

## Đợt 25 (04/10 tối): bấm Crop tool khi bảng Auto retouch đang mở

Chủ hỏi trước "làm cái này khó không, chỉ trả lời": tôi nêu phương án giữ bảng mở, crop xong
tự chạy lại làm đẹp. Chủ chọn cách đơn giản hơn (lời chủ): có lúc cần crop lại trước khi in vì
auto crop bị nghiêng hoặc mất góc do ảnh không đủ lớn; **bấm vào Crop tool thì hệ thống tự áp
dụng màu hiện tại luôn là hết lệch; muốn chỉnh thêm thì chạy lại mask cũng được**.

- [x] **Bấm Crop tool (thanh công cụ hoặc phím tắt) khi bảng đang mở**
  (`App::leave_portrait_for_crop`): app áp dụng phần làm đẹp đang có thành layer "Chân dung",
  đóng bảng, rồi chuyển sang Crop tool. Chưa có gì để áp dụng (bảng chưa chạy, thanh ở 0) thì
  chỉ đóng bảng. Đang nhận diện khuôn mặt hoặc đang làm ảnh thẻ thì từ chối kèm lời nhắc ở
  thanh trạng thái, không mất gì.
- [x] **Ảnh thẻ crop lại vẫn đúng cỡ in**: nếu ảnh đang là cỡ ảnh thẻ (2×3 / 3×4 / 4×6 theo
  kích thước + độ phân giải), Crop tool tự đặt Fixed Size bằng đúng số điểm ảnh và độ phân giải
  của ảnh (vd. 1043×1417 px @947). Ảnh khác thì giữ nguyên cài đặt Crop của chủ. Tôi tự thêm,
  chủ không yêu cầu — không khóa thì crop tay ra cỡ lệch, trang in phải cắt bớt.
- [x] **Crop xong muốn chỉnh thêm**: mở lại Auto retouch ▸ Tự động làm đẹp. Layer "Chân dung"
  đã bị crop không còn khớp công thức cũ nên trước đây báo "Không còn layer ảnh gốc…"; nay
  được chỉnh như một ảnh mới — nhận diện lại, **các thanh bắt đầu từ 0** vì ảnh đã làm đẹp rồi
  (`reopen_target`, `begin_portrait`). Việc này thay cho quyết định cũ "crop rồi không mở lại
  được = tính năng": vẫn không mở lại công thức cũ, nhưng chỉnh tiếp được.
- [x] **Xếp ảnh in khi bảng chưa chạy làm đẹp**: bấm một trang là xếp ảnh như đang có (trước
  phải có phiên làm đẹp sẵn sàng mới bấm được). Cần cho luồng crop lại rồi in.
- Test: `asking_for_the_crop_tool_applies_the_retouch_and_the_cropped_photo_is_retouched_anew`
  (ảnh thật), `the_crop_tool_keeps_an_id_photo_the_print_it_is`,
  `a_sheet_asked_with_no_retouch_under_way_lays_out_the_photo_as_it_is`,
  `a_sheet_can_be_asked_for_with_no_retouch_under_way` (giao diện). Test đầy đủ qua (1916 + 18).
- Chưa kiểm được bằng test: hai chỗ gọi (bấm nút công cụ, phím tắt) cần cửa sổ thật — chờ chủ thử.
- Code `ad67fcf`; bản test `target/release/iai-dot25.exe` (build 04/10 20:58 — chủ đang mở
  `iai.exe` nên bản mới chép ra tên khác).
- [x] Build Release, chủ test đợt 25: chủ không báo lỗi, nêu tiếp việc đợt 26 (04/10).

## Đợt 26 (04/10 khuya): bảng Auto retouch mở mà vẫn dùng được mọi công cụ

Lời chủ: "bên bảng AI Image Studio khi bảng đang mở vẫn có thể sử dụng được tất cả các tool
khác, tôi muốn Auto retouch cũng làm được như vậy".

Khác biệt gốc: AI Image Studio không vẽ gì tạm lên ảnh; Auto retouch thì vẽ bản xem trước thẳng
lên layer ảnh. Công cụ khác mà chạy lúc đó sẽ ghi bản xem trước vào lịch sử như ảnh thật. Nên:

- [x] **Bảng chỉ "giữ" ảnh khi có việc đang chạy** (`App::portrait_under_way`: đang xem trước
  làm đẹp, hoặc đang làm ảnh thẻ). Bảng mở mà chưa chạy gì (vừa mở, sau khi áp dụng) thì
  không khóa gì: công cụ, menu, phím tắt, đổi tab, mở file đều dùng bình thường. Khi đó Enter /
  Esc cũng không còn thuộc về bảng (để chốt / bỏ khung crop); nút "Hủy" ghi là "Đóng".
- [x] **Đang xem trước mà chọn công cụ khác** (nút trên thanh công cụ hoặc phím tắt của công
  cụ): app áp dụng phần làm đẹp đang có thành layer "Chân dung", **bảng vẫn mở**, rồi công cụ
  dùng được (`portrait_tool_picked`, thay cho `leave_portrait_for_crop` của đợt 25 — đợt 25
  đóng bảng, nay không đóng). Crop trên ảnh cỡ ảnh thẻ vẫn tự khóa cỡ.
- [x] **Hand và Zoom** là công cụ xem: chọn và dùng được ngay lúc đang xem trước, không áp dụng gì.
- [x] **Muốn chỉnh tiếp sau khi dùng công cụ**: bấm "Tự động làm đẹp" — layer "Chân dung" vừa
  áp dụng được mở lại với đúng các thanh cũ, không phân tích lại (dùng kết quả đã giữ).
- Cố ý KHÔNG làm: bấm thẳng lên ảnh lúc đang xem trước thì vẫn không có tác dụng (phải chọn
  công cụ trước). Lý do: bấm ra ngoài ô số là cách chốt số đang gõ (đợt 22); nếu cú bấm đó vừa
  áp dụng làm đẹp vừa vẽ một nét thì dễ hỏng ảnh ngoài ý muốn. Các lệnh menu khác lúc đang xem
  trước vẫn cần Áp dụng / Hủy trước như cũ.
- Test: `asking_for_the_crop_tool_…` (ảnh thật: bảng không khóa khi rảnh, khóa khi đang chạy,
  chọn công cụ thì áp dụng + bảng vẫn mở, "Tự động làm đẹp" mở lại đúng layer),
  `the_crop_tool_keeps_an_id_photo_the_print_it_is`,
  `open_and_idle_the_dialog_leaves_enter_and_esc_to_the_tools`. Test đầy đủ qua.
- Chưa kiểm được bằng test: nút công cụ, phím tắt và Hand / Zoom trên cửa sổ thật.
- Code `d03d422`; bản test `target/release/iai.exe` (build 04/10 21:33; bản tạm
  `iai-dot25.exe` của đợt 25 đã xóa).
- [x] Build Release, chủ test đợt 26: **OK** (04/10 khuya), kèm việc của đợt 27.

## Đợt 27 (04/10 khuya giao, làm phiên sau): bảng Auto retouch mở mà vẫn dùng được mọi lệnh và bảng Layer

Lời chủ: "tôi muốn quá trình mở bảng phải làm được các tool chỉnh ảnh khác bao gồm ctrl+x,
ctrl+l, ctrl+m,..... sử dụng được cả bảng layer luôn". Mục đích chủ nói rõ: tự động làm đẹp
xong, còn chỗ nào chưa ưng thì sửa tay một chút bằng công cụ khác là in được; "thay vì giữ bản
xem trước như hiện tại, nếu user muốn chỉnh thêm 1 chút ở tool khác thì áp dụng cái đã được auto
đẹp sẵn thành 1 layer cố định".

Quy tắc chung (`App::portrait_yields`, `yield_portrait`): lúc chỉ có bản xem trước của Auto
retouch đang giữ ảnh thì không khóa gì nữa; **hễ có lệnh từ ngoài bảng thì app áp dụng phần làm
đẹp đang có thành layer "Chân dung" (bảng vẫn mở), rồi lệnh chạy trên layer đó**. Các bảng xem
trước khác (Levels, Curves, Filter, Develop, Làm sạch scan) vẫn khóa như cũ.

- [x] **Phím tắt** (`portrait_key_passes`): Ctrl+X, Ctrl+V, Ctrl+L, Ctrl+M, Ctrl+U, Ctrl+B,
  Ctrl+I, Ctrl+T, Ctrl+J, Ctrl+A, Ctrl+D, Ctrl+E, Ctrl+G, Ctrl+S, Ctrl+P, Delete, phím mũi tên
  (khi đang cầm Move hoặc có vùng chọn)… đều áp dụng rồi chạy. Không áp dụng (chỉ xem / chỉ đổi
  cài đặt): phóng to thu nhỏ, Ctrl+0 / Ctrl+1, thước, Space, Ctrl+C, Ctrl+K, `[` `]`, X, D,
  Ctrl+Y. Enter / Esc vẫn là Áp dụng / Hủy của bảng. Phím không gán lệnh nào thì bỏ qua như cũ.
  Đang gõ trong một ô (ô số của thanh kéo, ô tên công thức, ô lời nhắc AI) thì Ctrl+A / Ctrl+V
  là của ô đó, không tính là lệnh.
- [x] **Menu, bảng Layer, thanh tab, thanh trang, History, Channels, AI Image Studio**: bật lại
  lúc đang xem trước. App nhận ra "lệnh ngoài bảng" bằng cách so cả gói yêu cầu của khung hình
  (`UiActions::reaches_past_retouch`): chỉ những gì chính bảng Auto retouch gửi và những gì
  thuần xem (phóng to, bật tắt bảng, rê chuột qua tab…) mới không tính; **trường mới thêm sau
  này mặc định tính là lệnh** (an toàn). Lệnh nhắm layer theo vị trí trong bảng Layer (độ mờ,
  chế độ hòa trộn, xóa, gộp, đổi tên…) được dời theo layer "Chân dung" vừa chen vào, và lệnh
  nhắm ảnh gốc được chuyển sang layer đã làm đẹp (`UiActions::retarget_layers`).
- [x] **Bấm thẳng lên ảnh** bằng công cụ đang cầm (`portrait_pressed`): áp dụng rồi công cụ chạy
  ngay cú bấm đó. Cú bấm chỉ để chốt ô số đang gõ không tính (lúc đó ô số còn giữ bàn phím nên
  ảnh chưa nhận chuột). Hand / Zoom vẫn chỉ xem, không áp dụng.
- [x] **Ctrl+Z / Edit ▸ Undo lúc đang xem trước** = bỏ phần xem trước, bảng vẫn mở, không hoàn
  tác gì của ảnh (`drop_portrait_preview`); đang bật cọ "Tô vùng" thì vẫn là hoàn tác nét tô.
  Bấm một mốc trong bảng History cũng bỏ xem trước rồi nhảy tới mốc đó. Redo lúc đó không làm gì.
- [x] **Đang nhận diện khuôn mặt hoặc đang làm ảnh thẻ** mà có lệnh ngoài bảng (kể cả chọn
  công cụ): app dừng việc đang chạy rồi cho lệnh chạy (đợt 26 từ chối kèm lời nhắc). Riêng cú
  bấm lên ảnh lúc này vẫn không có tác dụng, để một cú bấm vô tình không làm mất 4–10 giây đang
  chờ.
- [x] **Tắt app, kéo thả file vào, Ctrl+N** lúc đang xem trước: cũng áp dụng trước (trước đây
  tắt app bị chặn "Finish or cancel live preview"); tắt app thì sau đó hỏi lưu như thường.
  Riêng tắt app lúc **còn đang nhận diện khuôn mặt / đang làm ảnh thẻ** thì vẫn bị chặn như cũ
  (`portrait_on_show`): tiến trình thoát đúng lúc luồng AI đang nạp model có thể kẹt lại lúc
  thoát — đã gặp thật ở tiến trình test của đợt này.
- [x] **Tôi tự thêm (chủ không yêu cầu, đã báo)**: layer "Chân dung" đã được sửa tay sau khi áp
  dụng (tô, Levels, Curves…) thì bấm "Tự động làm đẹp" lần nữa sẽ **không** mở lại công thức cũ
  (mở lại là dựng lại từ ảnh gốc, đè mất phần sửa tay) mà làm đẹp tiếp trên chính layer đó như
  một ảnh mới, thanh từ 0 — cùng cách với layer đã crop ở đợt 25. Chưa sửa gì (hoặc đã Ctrl+Z
  hết phần sửa) thì vẫn mở lại đúng các thanh cũ như đợt 26. Nhận biết bằng dấu vân tay điểm
  ảnh lưu trong công thức (`PortraitRecipe::made`, `TileMap::content_hash`), ghi cả vào file
  `.iai`; file cũ không có dấu thì coi như chưa sửa.
- Đổi kèm: thanh công cụ không còn tự chuyển về Move lúc đang làm đẹp (việc tự chuyển đó sẽ bị
  tính là "chọn công cụ" và tự áp dụng). `ui::build` tách phần dựng một khung hình ra
  `ui::frame` để test chạy được cả giao diện không cần cửa sổ.
- Test: `a_command_from_outside_applies_the_previewed_retouch_and_runs_on_it` (ảnh thật: Ctrl+Z
  bỏ xem trước, bảng Layer đổi độ mờ, bấm bằng cọ, Ctrl+L, mắt của layer, Hand không áp dụng,
  layer đã sửa tay không bị dựng lại), `work_still_running_gives_way_to_a_command_and_ctrl_z_without_a_trace`,
  `a_key_that_edits_goes_on_once_the_retouch_gave_way_and_the_rest_leave_it`,
  `a_frame_nobody_touches_asks_nothing_from_outside_the_dialog` (dựng cả giao diện với từng
  công cụ, chuột đứng yên ở nhiều chỗ: không được tự áp dụng — đã thử gỡ chốt ở thanh công cụ
  thì test này bắt được), `only_a_command_from_outside_reaches_past_the_retouch`,
  `a_layer_asked_for_by_its_place_is_that_layer_still_once_the_retouch_has_its_own`,
  `portrait_recipe_round_trips_with_its_masks` (dấu vân tay còn đúng sau lưu / mở lại).
  Test `asking_for_the_crop_tool_…` sửa một ý: chọn công cụ lúc đang nhận diện nay dừng nhận diện.
- Ghi chú kỹ thuật: test nào mở phiên làm đẹp rồi kết thúc ngay phải dùng phiên giả
  `being_found` (không chạy luồng AI). Tiến trình test thoát lúc luồng nhận diện đang nạp model
  thì kẹt lại, không kill được, và khóa file exe test (`cargo test` báo `LNK1104`); đổi tên file
  bị khóa trong `target/debug/deps` là build lại được.
- Chưa kiểm được bằng test (cần cửa sổ thật, chờ chủ thử): bấm thật vào menu / bảng Layer /
  ảnh, và phím tắt gõ thật.
- Test đầy đủ qua (1919 + 22). Code `0b5a416`; bản test `target/release/iai.exe` (build 04/10
  23:50, đã mở thử lên được).
- [x] Build Release, chủ test đợt 27: **OK** (05/10). Chủ không nói gì thêm về ba điều tôi tự
  quyết (Ctrl+Z bỏ xem trước; có lệnh thì dừng nhận diện; layer đã sửa tay không mở lại công
  thức cũ) — giữ nguyên như đã làm.
