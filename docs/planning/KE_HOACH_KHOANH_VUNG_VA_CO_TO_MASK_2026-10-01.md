# Kế hoạch: khoanh vùng trước khi phân tích + cọ tô thêm/bớt mask (Chỉnh chân dung, đợt 9)

Ngày lập: **2026-10-01** · Nhánh: `feat/vector-core-foundation` · Nối tiếp
`KE_HOACH_MASK_DA_THUAT_TOAN_MAU_2026-09-30.md` (đợt 8, chủ test đạt).

Quy ước checklist: `[ ]` chưa làm · `[~]` đã code, chưa qua cổng · `[x]` xong
và qua cổng · `[!]` bị chặn.

## 0. Bối cảnh

- Chủ test đợt 8 đạt, gửi ảnh "Hiện vùng nhận diện" (ảnh thẻ nam tóc mái): vùng
  **tóc** lấy chưa hết — tóc mai hai bên thái dương, viền mép mái sát trán/lông
  mày, viền ngoài đỉnh đầu và các sợi tóc bay trên nền.
- Phân tích (probe trên ảnh thử Judy Chu, Jonny Kim, Lauren Underwood):
  1. **Tóc mai bị bỏ do quy tắc "tối hơn da"**: trọng số tóc ngoài lõi =
     `1 − smoothstep(ref − 0.4, ref − 0.15, luma)` với `ref` = độ sáng da quanh
     đó. Hai bên mặt da nằm trong bóng (ref ≈ 0,36) nên tóc đen 0,19 chỉ được
     0,03 dù Sapiens2 báo 83% là tóc. `ref` còn bị kéo tối vì xác suất da mềm
     của Sapiens2 loang lên chân tóc.
  2. **Viền ngoài**: vùng tóc nhân `on_person` (Sapiens2 tóc+da) nên dừng đúng
     ở mép thô của model (512×384); điểm viền pha màu nền sáng hơn nên hệ số
     "tối" cũng loại.
  3. **Mép mái sát lông mày/mắt**: vùng loại trừ lông mày (nở 0,02e + mềm
     0,03e) và mắt (0,03e + 0,03e) rộng.
  4. Tóc sau tai mà Sapiens2 không thấy (xác suất < 0,2) thì không tự bắt được.
- **Chủ đề xuất (01/10)**: thay vì bắt AI quét toàn ảnh, cho **khoanh vùng
  trước khi AI chạy**; AI lấy không hết thì **người dùng tô thêm/bớt**, rồi mới
  kéo các thanh sáng tối, màu sắc, mạnh yếu.

## 1. Quyết định đã khóa (chủ duyệt 2026-10-01)

1. Làm theo thứ tự **Pha 0 → Pha 1 → Pha 2**; mỗi pha một bản build cho chủ test.
2. Khoanh vùng **dùng lại các công cụ chọn sẵn có** (Marquee, Lasso, Smart
   Select W…): có vùng chọn khi mở Chỉnh chân dung thì chỉ phân tích trong vùng
   đó; không có thì tự động như hiện nay. Không thêm công cụ vẽ khung mới.
3. Cọ tô theo khuôn **Refine Selection** (công cụ riêng tự bật khi mở hộp thoại,
   trả công cụ cũ khi đóng): chế độ Thêm / Bớt / Thông minh (bám mép màu), cỡ cọ
   `[` `]`, Alt = đảo Thêm↔Bớt, Ctrl+Z trong hộp thoại.
4. Mask tô được: **Da** và **Tóc** trước; Môi, Răng, Quầng thâm sau nếu cần.
5. Không đổi công thức các hiệu ứng; cọ chỉ sửa mask mà hiệu ứng đọc.

## 2. Các pha

### Pha 0 — Sửa nhanh vùng tóc tự động (nhỏ)

- [x] Hệ số "tối hơn da" theo **tỉ lệ** thay cho hiệu cố định: tóc đủ khi
      `luma < 0,55·ref`, không tính khi `luma > 0,85·ref`.
- [x] `ref` lấy từ **mask da mới** (đợt 8) trong vùng mặt, ngoài vùng mặt mới
      dùng Sapiens2; điểm là da theo mask mới thì không là tóc.
- [x] Viền ngoài: tách sợi tóc khỏi nền kiểu Refine Edge, **theo màu RGB** (không
      chỉ độ sáng: tóc nâu tối trên nền xanh đậm gần cùng độ sáng): chiếu màu
      điểm lên đoạn màu nền → màu tóc tại chỗ; chỉ khi nền trơn và màu nền/tóc
      cách xa đủ. Thêm nhóm "Nền" (class 0) vào nhóm vùng Sapiens2. Màu tóc lấy
      từ lõi vùng tóc của model (mép model hay lấn ra nền). Ở dải tóc giáp nền,
      phép tách màu thay quy tắc "tối hơn da" → hết quầng màu quanh tóc.
- [x] Thu hẹp vùng loại trừ quanh lông mày/mắt (0,005e/0,015e và 0,01e/0,015e);
      chỗ model chắc chắn là tóc (lọn tóc vắt qua đuôi lông mày) thì không loại.
- [x] Probe 13 ảnh: tóc mai/chân tóc được bắt (Judy, Kim, Mazie, Lauren,
      Jeffries); quầng màu viền tóc trên nền xanh hết (Nelson, Lauren); không rò
      sang kệ sách (Mazie). Tính trên lưới thô (ô ≈ e/150) → bớt RAM, nhanh hơn.
      Còn lại (để cọ tô Pha 2): tóc bạc sáng hơn da trên tường xám (tóc mai
      bạc không tự bắt), nền lọt qua tóc mà model chắc chắn là tóc.
- Cổng: tóc mai hai bên, mép mái được tô; không lan ra nền/áo. **Đạt — chủ test
  OK 01/10.**

### Pha 2b — Da cổ / ngực không bị cắt ngang

- [x] Chủ test cọ Thông minh OK; gửi ảnh: da ở cổ áo chữ V bị cắt thẳng ngang
      (đáy khung phân tích mặt = cằm + 0,45e). Sửa: khi model tách vùng tin
      cậy, đáy khung kéo xuống tới hàng cuối Sapiens2 còn thấy da (mặt + thân,
      > 0,5) trong bề ngang mặt, + 0,1e (`skin_reach`) — chỉ ảnh hở cổ mới dài
      ra (Judy, Mazie, c01), áo vest/cao cổ giữ khung cũ nên không chậm thêm.
      Màu da trung bình (Đều màu da) vẫn chỉ đọc tới cằm + 0,45e
      (`tone_rows`). Cọ tô Da với tới vùng này theo.

### Pha 2c — Da vai, tay, bàn tay không bị khung chữ nhật cắt

- Chủ test Pha 1 OK, gửi ảnh mẫu (cô gái chống cằm, áo hai dây): mask da dừng
  ở **khung chữ nhật** = bề ngang mặt + 0,15e mỗi bên (Pha 2b chỉ kéo đáy
  xuống) → vai, cánh tay, phần bàn tay ngoài khung không được chọn, có mép
  dọc thấy rõ.
- Tái hiện trên ảnh thử công cộng (Amy Adams vai trần; cô dâu ren; bà Romand
  áo hai dây, tay trần buông dọc người, 30 MP): đúng mép dọc cắt vai; thêm hai
  lỗi chưa thấy trước đó — (1) **vai/tay bên kia dây áo bị bỏ hẳn** vì mask
  chỉ giữ da nối liền với mẫu da trên mặt, dây áo cắt rời; (2) ảnh nửa người
  thì **tay bị cắt ngang ở đáy khung của Sapiens2** (khung 3e × 4e quanh đầu).
- [~] **Da có khung riêng** (`SkinLayers.region`): khung mặt + mọi điểm
  Sapiens2 thấy da (mặt + thân > 0,35, đúng ngưỡng hàng rào của mask) **có màu
  giống da mặt** (độ lệch sắc độ r,g < 0,09 — loại găng tay trắng, áo hồng),
  không thuộc mặt khác, nới 0,1e. Mask, tách tần số, hiệu ứng da, "Hiện vùng
  nhận diện", lớp phủ và cọ tô Da chạy trên khung da; mắt, môi, mụn, lông
  mày, sống mũi giữ khung mặt (khung mặt quay về đáy cằm + 0,45e). Không có
  model tin cậy → khung da = khung mặt như cũ.
- [~] **Da qua dây áo**: ô da mà Sapiens2 chắc ≥ 0,8 (và cắt đồ thị cũng nhận
  là da) được giữ dù không nối với mặt.
- [~] **Nhìn rộng lần hai**: khi da (có màu da) chạm cạnh trái/phải/đáy khung
  Sapiens2 mà ảnh còn tiếp, chạy Sapiens2 thêm một lần trên khung đứng rộng
  6e, từ 1e trên tâm mặt tới 6e dưới (trong ảnh và trong vùng chọn); hai lần
  nhìn hòa vào nhau ở 24 điểm ảnh model sát mép khung đầu. Tóc vẫn theo khung
  đầu.
- [~] Tốc độ/RAM: mờ rộng (σ ≥ 20) chạy trên lưới khối 2–8 px rồi nội suy (lệch
  < 0,02 trong lòng ảnh); bỏ sớm các mảng float lớn. Ảnh 20 MP (Amy phóng 2×,
  khung da 3721×4742): chuẩn bị ~4,0 s (cũ 3,5 s), đỉnh RAM 2,1 GB (cũ
  2,4 GB). Ảnh 30 MP có tay: thêm ~1,7 s cho lần nhìn rộng.
- Probe: 8 ảnh cũ (Kim, Lauren, Nelson, Mazie, Jeffries, Meir, Judy, Artemis)
  mask y như trước; Kim (găng tay phi hành gia) không còn chạy lần nhìn rộng.
  Tay áo ren mờ của cô dâu vẫn không nhận là da (Sapiens2 đọc là áo) — chấp
  nhận.
- Cổng: ảnh chân dung vai/tay trần — mask da phủ hết vai, tay, bàn tay, không
  còn mép thẳng; không lan sang áo/nền. **Đạt — chủ test OK 01/10.**

### Pha 6 — Lông mày: mask mượt, mặc định giữ nguyên

- Chủ test 2c OK, yêu cầu: mask lông mày hơi thô, vùng chuyển không mượt;
  **mặc định lông mày giữ nguyên như ảnh gốc**, người dùng tự kéo đậm nhạt,
  độ nét… từ 0 nếu muốn.
- Soát (probe `probe_brows`, 10 mặt): mask cũ = đa giác mốc mặt tô gần đặc,
  mép răng cưa; lông mày bạc (Nelson) gần như không bắt (chỉ tính điểm tối hơn
  da); kéo "Lông mày" vẽ ra dải cứng; mặc định làm mịn da và "Tăng nét" (20)
  vẫn đụng lông mày.
- [~] Mask mới (`BrowLayers` vùng riêng trong khung mặt): độ lệch màu của từng
  điểm so với da quanh lông mày (nội suy qua lông mày); mỗi bên lông mày tự
  học mức lệch của da trơn cạnh nó và của lõi lông mày → bắt được lông mày
  đậm, bạc, nhạt. **Hình lông mày** = nơi sợi tụ lại (làm mờ cỡ e/45, chuẩn
  hóa theo lõi từng bên) → mép mềm. Vùng tìm lệch lên trên và quá đuôi, gần
  như không xuống dưới (phấn mắt), không vào giữa hai mày (nếp nhăn); bỏ chỗ
  Sapiens2 thấy tóc (mái) — Sapiens2 luôn đọc lông mày là da mặt.
- [~] Hình lông mày được **loại khỏi chỉnh da và "Tăng nét"** → mặc định lông
  mày y ảnh gốc.
- [~] Mục **"Lông mày"** riêng trong hộp thoại, mặc định 0: Đậm nhạt (phải:
  đậm sợi + phủ nhẹ như chì kẻ; trái: kéo tông lông mày về màu da bên dưới,
  giữ vân sợi), Độ nét, Màu lông mày + Phủ màu lông mày. "Hiện vùng nhận
  diện" tô vàng hình lông mày mềm.
- Còn lại: một sợi tóc mái dày sát đuôi mày (Mazie) vẫn bị tính là lông mày —
  Sapiens2 đọc chỗ đó là da; chưa có cọ "Tô vùng → Lông mày".
- Cổng: mặc định lông mày y gốc; kéo đậm/nhạt/nét/màu ra kết quả tự nhiên,
  không có viền cứng. **Đạt — chủ test OK 01/10.**

### Pha 7 — "Sống mũi cao" chuyển mượt

- Chủ test lông mày OK, gửi ảnh: dải sáng sống mũi có mép dọc rõ, không tan
  vào da.
- Nguyên nhân: vùng sáng là dải đa giác đỉnh phẳng, dốc ngắn (mềm 0,03e); hai
  dải tối bên là hai hình chữ nhật đầu cụt.
- [~] Trường tạo khối mới (`nose_contour`): khoảng cách tới đường sống mũi
  (6→197→195→5→4) và vị trí dọc theo nó; sáng = chuông hẹp (σ 0,02e), tối hai
  bên = chuông rộng (cách 0,075e, σ 0,028e); cả hai hiện dần dưới chân mày và
  tắt dần về chóp mũi. Mạnh hơn chút (sáng 0,18, tối 0,06) bù cho dáng mềm.
  Probe `probe_nose` (IAI_PORTRAIT_NOSE_PROBE).
- Chủ test lần 1: mềm hơn nhưng **vẫn cụt hai đầu** (bắt đầu dưới tầm mắt,
  dừng trên đầu mũi), muốn dài hơn, mềm hơn, **bớt tối hai bên**.
- [~] Lần 2: đường sống mũi kéo từ tầm chân mày (8, 168) xuống chóp mũi (1);
  sáng hiện dần từ chân mày, đầy từ giữa hai mắt, còn 1/3 trên chóp mũi; sáng
  rộng hơn (σ 0,026e); tối hai bên rộng hơn (σ 0,035e, cách 0,08e), chỉ dọc
  phần xương sống mũi, mạnh bằng nửa (0,03).
- Chủ test lần 2: đoạn giữa hai mắt (khoanh tròn) **sáng quá mạnh và loang
  rộng ra hai bên** (chỗ đó mặt phẳng và rộng, vệt rộng đủ mạnh đủ thành
  quầng).
- [~] Lần 3: vệt sáng thu hẹp về phía trên (σ 0,015e ở đầu → 0,026e từ giữa
  sống mũi) và hiện chậm hơn: giữa hai mắt còn khoảng một nửa, mạnh nhất ở
  giữa sống mũi.
- Cổng: kéo "Sống mũi cao" tới 100: vệt sáng dài từ giữa chân mày tới chóp
  mũi, không mép, không cụt, giữa hai mắt nhẹ và hẹp; hai bên chỉ tối nhẹ.
  **Đạt — chủ test OK 01/10.**

### Pha 1 — Khoanh vùng trước khi phân tích (chủ cho làm sau Pha 2)

- [x] Mở Chỉnh chân dung khi đang có vùng chọn → chỉ dò mặt trong vùng đó (ảnh
      nhóm: chọn đúng người cần chỉnh) và dòng trạng thái ghi "Trong vùng
      chọn". Dò mặt trên ảnh cắt quanh vùng chọn (+25 % mỗi phía, mặt nhỏ dễ
      thấy hơn), giữ mặt có tâm trong vùng chọn; không có → báo "không tìm thấy
      khuôn mặt nào trong vùng chọn".
- [x] Sapiens2 chạy trên khung ôm sát vùng chọn (giữ tỉ lệ 3:4 của model, thẳng
      đứng, lề 8 %) khi khung đó nhỏ hơn khung 3 lần bề rộng mặt
      (`PartCrop::closer`). Probe khoanh quanh đầu: khung nhỏ hơn 1,8–2,3 lần
      (Kim 2987→1296 px), bắt thêm sợi tóc bay (Kim, Meir), bỏ lòng áo tối sau
      gáy Meir bị nhận nhầm; chuẩn bị còn nhanh hơn.
- [x] Mọi hiệu ứng (và "Hiện vùng nhận diện", lớp phủ khi tô) nhân độ phủ vùng
      chọn tại điểm đó (`Clip`) — mép mềm theo vùng chọn như Photoshop; Áp dụng
      chỉ tạo điểm ảnh trong vùng chọn.
- Cổng: khoanh sát đầu cho mép tóc rõ hơn tự động; ảnh nhóm chỉ xử lý người
  được khoanh. **Đạt — chủ test OK 01/10.**

### Pha 2 — Cọ tô thêm/bớt mask

- [x] Hộp thoại có mục **"Tô vùng"** (đầu hộp thoại): Tắt / Da / Tóc, chế độ
      Thông minh / Thêm / Bớt, cỡ cọ, độ cứng, nút hoàn tác/làm lại; khi tô, lớp
      phủ màu (da đỏ, tóc tím) của vùng đang tô hiện trên canvas (texture GPU,
      vá từng vùng nét tô), ảnh xem trước vẫn là ảnh đã chỉnh.
- [x] Dùng lại công cụ **Refine Brush** (vòng con trỏ, `[` `]`, Shift+`[` `]`,
      Alt+chuột phải kéo cỡ, Alt đảo Thêm↔Bớt, Alt+Thông minh = trả lại vùng app
      tìm): khi không có phiên Refine Selection, nét tô xếp hàng ở
      `Canvas::mask_brush` cho hộp thoại xử lý mỗi khung hình. Mỗi nét chỉ tô
      một mặt (mặt gần điểm bắt đầu).
- [x] **Thông minh làm lại (chủ: "cọ không thông minh", đề xuất thử Color
      Range)**. Thử trên ảnh thật (Meir tóc xoăn mảnh trên áo trắng, Nelson sợi
      bạc trên nền xanh): cọ cũ (`refine_edge_stamp`) còn **khoét lỗ** vùng tóc
      đã có; Color Range lấy màu ở điểm bấm thì sợi mảnh trên nền sáng gần như
      không ăn, còn trên Nelson **chọn lan cả nền xanh** (thước đo Color Range
      thiên về độ sáng mà tóc nâu tối và nền xanh đậm gần cùng độ sáng). Chốt:
      **Color Range chấm theo cả hai phía** — quanh cọ lấy mẫu màu vùng đang tô
      (mask ≥ 0,9) và phần còn lại (mask ≤ 0,1, chỉ điểm phẳng để sợi tóc bay
      không bị tính là nền; đo bằng bước lệch lớn nhất với điểm kề nên sợi rộng
      1 px cũng nhận ra), bỏ mẫu hai phía trùng nhau (nền lộ giữa các sợi trong
      mask), mỗi điểm dưới cọ lấy tỉ lệ khoảng cách Lab tới mẫu gần nhất mỗi
      phía → sợi mờ được tô mờ, nền giữ 0, màu không giống bên nào (da cạnh tóc)
      bị loại. Chỉ cộng thêm, tính trên mask lúc bắt đầu nét → **tô lại để sợi mờ
      đậm hơn**; Alt + Thông minh = bớt phần giống nền.
- [x] Sau mỗi nét: tóc dùng ngay; da tính lại cả khối `SkinLayers` (mask,
      interior, quầng mắt, tách tần số có trọng số, màu da) trên luồng nền rồi
      xem trước lại. Chấm mụn giữ theo lần phân tích (bớt da thì hết xóa mụn ở
      đó; thêm da không dò mụn mới).
- [x] Ctrl+Z / Ctrl+Shift+Z trong hộp thoại; Hủy bỏ hết nét tô; Áp dụng dùng
      mask đã tô (đợi da tính xong) và trả công cụ cũ.
- Cổng: tô thêm phần tóc/da AI bỏ sót và bớt phần lấy thừa bằng vài nét; kéo
  thanh sau khi tô cho kết quả đúng vùng đã tô. **Đạt — chủ test OK 01/10**
  (chủ bất ngờ vì Thông minh tự nhận sợi tóc mảnh khi tô lại vùng tóc).

### Pha 3 — Lưu mask đã tô, mở lại chỉnh tiếp

- [~] Áp dụng lưu "công thức" vào layer "Chân dung" (`Layer.portrait`,
      `core::portrait::recipe`): thanh trượt, mặt nào bật, mask Da/Tóc đã tô
      (chỉ mặt có tô), vùng chọn lúc phân tích, id + kích thước layer ảnh gốc.
- [~] Mở lại: chọn layer "Chân dung" **hoặc** layer ảnh ngay dưới nó rồi vào
      Image ▸ Chỉnh chân dung… → phân tích lại ảnh gốc (giữ vùng chọn cũ nếu
      không có vùng chọn mới), thanh trượt + mặt bật/tắt về như lần trước, mask
      đã tô đặt lại lên vùng mới (ghép mặt theo vị trí, lệch < 0,35 cỡ mặt) và
      cọ tô tiếp từ đó. Dòng trạng thái: "Chỉnh tiếp layer "Chân dung"". Trong
      lúc chỉnh layer cũ tạm ẩn (xem trước vẽ trên ảnh gốc), Hủy thì hiện lại y
      cũ; Áp dụng **cập nhật đúng layer đó** (một bước hoàn tác), không thêm
      layer mới. Layer khóa → báo mở khóa; mất layer gốc → báo.
- [~] Lưu trong file `.iai`: khóa "portrait" của layer trong manifest + ảnh
      xám `layer_N_portrait_*.png`; bản iAi cũ bỏ qua (mở vẫn thấy điểm ảnh).
      Tự lưu khôi phục dùng cùng định dạng.
- Test: `reopening_the_portrait_layer_restores_and_updates_it` (ảnh thật: tô
  tóc → OK → mở lại từ layer kết quả và từ ảnh gốc → Hủy/OK/hoàn tác),
  `portrait_recipe_round_trips_with_its_masks` (.iai).
- Cổng: Áp dụng, đóng mở file, mở lại → thanh trượt và vùng tô còn nguyên; kéo
  tiếp rồi Áp dụng → layer cũ được cập nhật.

### Pha 5 — (sau) Cọ Thông minh cho Refine Selection

- [ ] Chủ 01/10: đưa thuật toán "Color Range hai phía" của cọ Thông minh
      (`core::portrait::brush`) sang Refine Brush của Refine Selection (thay
      `refine_edge_stamp` ở chế độ Smart; mẫu lấy từ vùng chọn đang tinh chỉnh).

### Pha 4 — "Sáng tóc" bằng thanh Blacks của Develop

- Chủ test 01/10: tăng sáng tóc hiện khá yếu → áp thẳng thanh **Blacks** của
  Develop cho vùng tóc cho nhanh. Ảnh chủ gửi (kéo tóc sáng/bạc mạnh) còn một
  **viền cam ở chân tóc** giáp trán — soát khi làm.
- Nguyên nhân viền cam (tái hiện trên Jonny Kim, Judy, Mazie, Amy): cách cũ
  nâng độ sáng trong miền hiển thị rồi **nhân thêm độ đậm màu** khi nâng
  (`apply_luma_target`), điểm pha tóc + da ở chân tóc (và cả tóc vàng của Amy)
  thành cam gắt; tóc đen thành xám bạc phẳng.
- [~] Kéo phải = đúng thanh **Blacks** của Develop3 (bộ cân tông theo vùng,
  miền tuyến tính, look giữ nguyên ảnh): Sáng tóc +100 = Blacks +200 (tối đa),
  +50 = Blacks +100; đọc ở tông vùng `hair_base` như Develop → tóc sáng tự
  nhiên, giữ màu và vân sợi, hết viền cam. Kéo trái (tối hơn) giữ cách cũ
  (Shadows + Blacks miền hiển thị) vì Blacks âm gần như không đụng tóc nâu.
  Probe `probe_hair_tone` (IAI_PORTRAIT_HAIR_PROBE), 7 ảnh; tốc độ như cũ.
- Cổng: kéo Sáng tóc lên mạnh — tóc sáng hơn tự nhiên, không viền cam ở chân
  tóc, không cam hóa tóc vàng.

## 3. Rủi ro

| Rủi ro | Xử lý |
|---|---|
| Viền ngoài tách theo độ sáng rò sang nền tối có vân (kệ sách) | Dải hẹp ~e/20 quanh mép Sapiens2, chỉ khi tóc/nền tương phản đủ; probe ảnh Mazie |
| Cọ tô trên ảnh 50 MP chậm | Chỉ tính lại trong khung nét tô; xem trước theo luồng nền sẵn có |
| Xung đột công cụ canvas khi hộp thoại mở | Theo đúng khuôn Refine Selection (khóa modal, trả công cụ cũ) |

## 4. Changelog

- **2026-10-01** — Lập kế hoạch theo đề xuất của chủ (khoanh vùng + cọ tô) và
  kết quả phân tích vùng tóc; chưa code.
- **2026-10-01** — Chủ duyệt kế hoạch (thứ tự Pha 0 → 1 → 2; cọ tô Da + Tóc
  trước). Làm ở hội thoại mới, bắt đầu Pha 0.
- **2026-10-01** — Pha 0 code xong (tỉ lệ tối, ref từ mask da mới, tách viền
  theo màu, nhóm Nền, thu hẹp loại trừ lông mày/mắt); probe đạt; chờ chủ test.
- **2026-10-01** — Chủ test Pha 0 OK; thêm Pha 4 (Sáng tóc theo Blacks của
  Develop, làm sau); chủ cho làm Pha 2 trước Pha 1. Pha 2 code xong, chờ chủ
  test.
- **2026-10-01** — Chủ: cọ Thông minh chưa thông minh, thử Color Range. Thử 3
  cách trên ảnh thật, chốt "Color Range hai phía" (xem Pha 2); chờ chủ test.
- **2026-10-01** — Chủ test cọ Thông minh OK; báo da cổ/ngực bị cắt ngang → Pha
  2b (khung da kéo theo da model thấy); chờ chủ test.
- **2026-10-01** — Chủ test Pha 2b OK; thêm Pha 5 (cọ Thông minh cho Refine
  Selection, làm sau). Bắt đầu Pha 1.
- **2026-10-01** — Pha 1 code xong (dò mặt trong vùng chọn, khung Sapiens2 ôm
  sát, cắt hiệu ứng theo vùng chọn); probe + test đạt; chờ chủ test.
- **2026-10-01** — Chủ test Pha 1 OK; báo da vai/tay bị khung chữ nhật cắt →
  Pha 2c, làm ở hội thoại mới (context phiên này đầy).
- **2026-10-01** — Pha 2c code xong (khung da riêng có cổng màu, giữ da qua dây
  áo, nhìn rộng lần hai khi da chạm mép khung model, mờ rộng trên lưới khối);
  probe đạt; chờ chủ test.
- **2026-10-01** — Chủ test 2c OK; yêu cầu lông mày → Pha 6 code xong (mask
  hình lông mày mềm, mặc định giữ nguyên, mục "Lông mày" từ 0); chờ chủ test.
- **2026-10-01** — Chủ test Pha 6 OK; báo "Sống mũi cao" chuyển không mượt →
  Pha 7 code xong (trường tạo khối dạng chuông); chờ chủ test.
- **2026-10-01** — Chủ test Pha 7: còn cụt hai đầu, tối hai bên nhiều → kéo
  dài từ chân mày tới chóp mũi, mềm hơn, tối hai bên giảm một nửa; chờ chủ
  test.
- **2026-10-01** — Chủ test lần 2: giữa hai mắt sáng mạnh, loang → thu hẹp và
  giảm đoạn đầu; chờ chủ test.
- **2026-10-01** — Chủ test Pha 7 lần 3 OK. Bàn giao sang hội thoại mới
  (context đầy). Việc còn lại theo chủ dặn: Pha 3 lưu mask đã tô, Pha 4 "Sáng
  tóc" theo Blacks của Develop, Pha 5 cọ Thông minh cho Refine Selection;
  nhỏ: sợi tóc mái dày sát đuôi mày còn bị tính là lông mày, chưa có cọ Tô
  vùng → Lông mày.
- **2026-10-01** — Chủ giao Pha 3 + 4 (hội thoại mới). Pha 4 code xong
  (`cd49239`: Sáng tóc = Blacks của Develop3, hết viền cam); Pha 3 code xong
  (`da15cee`: lưu công thức vào layer "Chân dung" + file .iai, mở lại chỉnh
  tiếp, Áp dụng cập nhật tại chỗ). Test đạt; chờ chủ test.
