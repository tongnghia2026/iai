# Kế hoạch: Chỉnh dáng mặt và dáng người (Chỉnh chân dung, đợt 10)

Ngày lập: **2026-10-02** · Nhánh: `feat/vector-core-foundation` · Nối tiếp
`KE_HOACH_CHAN_DUNG_KIEU_EVOTO_2026-09-29.md` (Phase 2 "Chỉnh dáng mặt" và
Phase 3 "Dáng người") và đợt 9 (`KE_HOACH_KHOANH_VUNG_VA_CO_TO_MASK_2026-10-01.md`,
hoàn tất 02/10).

Quy ước checklist: `[ ]` chưa làm · `[~]` đã code, chưa qua cổng · `[x]` xong
và qua cổng · `[!]` bị chặn.

## 0. Bối cảnh

- Đợt 9 xong (chủ test OK 02/10). Em đề xuất làm "đồng bộ hàng loạt" trước; **chủ
  chọn làm Chỉnh dáng mặt và Chỉnh dáng người** (02/10).
- Rủi ro chính đã nói với chủ: uốn dáng dễ **làm cong nền phía sau** (cửa, kệ,
  đường chân trời cạnh má/eo) và cần thử nhiều ảnh.
- Sẵn có để dùng lại:
  - 478 mốc mặt MediaPipe (`core::ai::face_mesh`), viền mặt `FACE_OVAL`, mắt,
    mũi, môi, mống mắt (`core::portrait::geometry`).
  - Sapiens2 tách 29 vùng người (`core::ai::body_parts`): mặt+cổ, tóc, thân,
    áo, quần/váy, bắp tay / cẳng tay / bàn tay, đùi / cẳng chân / bàn chân
    (trái, phải), nền. Hiện chỉ chạy trên khung đầu-vai (3e × 4e) và một lần
    nhìn rộng (6e).
  - Lưới dịch chuyển ngược + lấy mẫu song tuyến của công cụ Warp (Liquify)
    (`core::warp::WarpMesh`).
  - Hộp thoại Chỉnh chân dung: xem trước ở luồng nền, Áp dụng ra layer "Chân
    dung", công thức lưu trên layer + file `.iai`, mở lại chỉnh tiếp.

## 1. Quyết định đã khóa (chủ duyệt 2026-10-02)

1. Hai mục mới trong hộp thoại Chỉnh chân dung: **"Dáng mặt"** và **"Dáng
   người"**. Mọi thanh mặc định **0** = không đổi dáng.
2. Thứ tự xử lý: chỉnh da/màu/tóc như hiện nay **rồi mới uốn dáng**; cùng một
   layer "Chân dung"; các thanh dáng lưu vào công thức, mở lại chỉnh tiếp được.
3. Cách uốn: **điểm điều khiển** lấy từ mốc mặt (dáng mặt) hoặc khung người
   (dáng người) dời tới vị trí mới; một **vòng neo cố định** quanh vùng ảnh
   hưởng nên nền ở xa không nhúc nhích; trong vòng, ảnh uốn theo kiểu "giữ
   hình cứng nhất có thể" (MLS rigid) để đường thẳng gần mặt/người cong ít
   nhất. Tính trên lưới thô rồi nội suy → nhanh, mượt.
4. Mức tối đa mỗi thanh được giới hạn ở mức còn tự nhiên (Evoto/PTS cũng vậy).
5. **Pha dáng mặt không thêm model mới.** Dáng người thử bằng Sapiens2 đã có
   trước; chỉ khi không đạt mới đề xuất thêm model khung xương MediaPipe Pose
   (Apache-2.0) — **sẽ hỏi chủ trước khi tải**.
6. Vá nền bằng AI (LaMa) cho phần nền lộ ra khi mặt/người thon lại là **pha
   tùy chọn**, chỉ làm khi bản cơ bản qua cổng mà chủ thấy nền còn cong.
7. Ảnh thử: dùng ảnh thử đang có + tải thêm khoảng 10 ảnh chân dung / toàn
   thân công khai (Wikimedia Commons, giấy phép tự do) có nền nhiều đường
   thẳng — **xin chủ đồng ý tải**.

## 2. Các pha

### Pha A0 — Lõi uốn ảnh theo điểm điều khiển

- [x] Module `core::portrait::reshape`: từ danh sách cặp điểm (gốc → đích) và
      vòng neo, dựng trường dịch chuyển **ngược** (điểm ra lấy màu từ đâu) trên
      lưới thô (ô ≈ e/100, tối thiểu 4 px) chỉ trong khung ảnh hưởng; MLS rigid
      tính ngược (đích → gốc) nên không phải đảo trường.
- [x] Vẽ: lấy mẫu song tuyến như Warp; cộng dồn nhiều mặt (mỗi mặt một trường,
      khung riêng); nhân độ phủ vùng chọn (`Clip`) vào **độ dời** (không trộn
      ảnh uốn với ảnh gốc → không bóng ma ở mép vùng chọn).
- [x] Probe `probe_reshape` (IAI_PORTRAIT_RESHAPE_PROBE): mỗi thanh ở 100 +
      lưới ô vuông vẽ lên ảnh rồi uốn (Mặt thon + Mắt to) để xem đường thẳng.
- Kết quả lần 1: viền hàm bị **răng cưa** (36 điểm viền dời mà giữa hai điểm
  trường bị kéo về các điểm đứng yên khác) → thêm điểm nội suy dọc mọi viền
  (mặt, mắt, mày, môi, sống mũi; cách nhau e/150) → hết răng cưa, lưới cong
  mượt. Dựng trường 9–53 ms; uốn 6–180 ms (mặt lớn chiếm cả ảnh 20 MP:
  ~180 ms); điểm không dời chép thẳng.
- Cổng (nội bộ): điểm dời đúng tới đích, ngoài vùng không đổi, mép vùng về 0
  (test `field_takes_colour_from_where_a_point_came_from_and_nothing_past_the_ring`).
  Nền sát hàm vẫn dời vài px khi kéo mạnh (bản chất của uốn; PTS cũng vậy) —
  nền cách mặt từ ~0,15e trở ra gần như đứng yên. **Đạt nội bộ.**

### Pha A1 — Dáng mặt

- [x] Thanh (đều hai chiều −100..100):
  - **Mặt thon**: viền hàm/má (nửa dưới `FACE_OVAL`, từ ngang tai tới cằm) dời
    vào trục giữa mặt, mạnh nhất ở xương hàm, nhẹ dần lên thái dương và về cằm.
  - **Cằm**: các điểm cằm dời theo trục mặt (dài/ngắn cằm).
  - **Mắt to**: viền mắt + mống mắt phóng quanh tâm mắt (trái/phải như nhau).
  - **Mũi**: cánh mũi dời vào/ra trục mũi (nhỏ/hẹp lại hoặc rộng).
  - **Miệng**: khóe miệng dời ra/vào (rộng/hẹp).
  - **Trán**: viền trán (nửa trên `FACE_OVAL`) dời lên/xuống theo trục mặt.
  - (Sau nếu cần) **Cân đối hai mắt**: kéo cỡ và độ cao hai mắt về trung bình.
- [x] Điểm không thuộc thanh nào được giữ làm neo (ví dụ kéo Mặt thon thì mắt,
      mũi, miệng đứng yên); mặt nghiêng: mức dời mỗi bên theo bề rộng thấy
      được của bên đó (bên khuất dời ít).
- [x] Probe trên 14 ảnh (5 cũ + 9 ảnh CC0 mới tải từ Wikimedia Commons, có
      tường gạch, kệ sách, khung cửa, kính mắt, cận mặt): tự nhiên, không vỡ.
      Mức 100: hàm/má thon ~9% khoảng cách tới giữa mặt; cằm dài 0,05e; mắt
      to 14%; cánh mũi hẹp 15%; khóe miệng ra 12% nửa miệng; trán cao 0,06e.
- Cổng: chủ test — ở mức vừa (≈ 50) mặt đổi rõ mà tự nhiên, không thấy nền
  cong; 100 vẫn chấp nhận được; mặt nghiêng không vỡ. **Đạt — chủ test OK
  02/10.**

### Pha A2 — Ghép vào hộp thoại

- [x] Mục "Dáng mặt" trong hộp thoại (ngay sau "Da"), xem trước ở luồng nền
      như các thanh khác; "Hiện vùng nhận diện" hiện trên ảnh đã uốn. Khi đang
      tô vùng, xem trước tạm không uốn (cọ tô theo mặt gốc).
- [x] Áp dụng: layer "Chân dung" phủ cả vùng uốn; các thanh dáng nằm trong
      `PortraitSettings` nên công thức tự lưu; mở lại → thanh về như cũ.
- [x] Ảnh nhóm: mỗi mặt bật/tắt như hiện nay, mặt khác giữ yên (viền của chúng
      làm neo); vùng chọn giảm dần độ dời theo mép vùng chọn.
- Test app `face_shape_warps_the_jaw_only_and_is_kept_in_the_recipe`: Áp dụng
  Mặt thon → điểm ở hàm đổi, góc ảnh không đổi, công thức giữ thanh.
- Cổng: chủ test cả luồng trên ảnh thật. **Đạt — chủ test OK 02/10.**

### Pha A3 — Thêm thanh dáng mặt (chủ yêu cầu 02/10)

Cùng lõi `core::portrait::reshape` (thêm trường vào `FaceShape` +
`PortraitSettings`, điểm dời trong `face_controls`), mỗi thanh hai chiều
−100..100:

- [x] **Miệng cười / mếu** (thanh "Cười"): mọi điểm viền ngoài + viền trong
      môi dời **lên** theo trục mặt (cười) hoặc **xuống** (mếu) theo
      smoothstep(0,3..1) của khoảng cách tới giữa miệng / nửa bề rộng miệng —
      khóe dời 0,16 nửa bề rộng ở 100, giữa môi đứng yên. Probe: cười / mếu rõ,
      tự nhiên; chưa cần nâng gò má.
- [x] **Môi dày / mỏng** (thanh "Môi dày"): viền ngoài môi trên dời lên, môi
      dưới dời xuống (dày) hoặc ngược lại (mỏng), mỗi môi 0,4 độ dày của nó
      (đo ở giữa) × (1 − (khoảng cách / nửa bề rộng)²); viền trong
      `MOUTH_INNER` giữ yên (nay là điểm neo + nội suy dọc viền cho mọi thanh;
      "Rộng miệng" dời cả viền trong theo cùng quy tắc).
- [x] **Mắt nghiêng**: xoay viền mỗi mắt tối đa 12° quanh **giữa hai khóe
      mắt** (không phải tâm mống — mắt liếc thì tâm mống lệch) — đuôi mắt
      (33 / 263) lên, đầu mắt (133 / 362) xuống (xếch) hoặc ngược lại; hai mắt
      đối xứng. Mống mắt dời theo tâm của nó, không xoay → giữ tròn. (Probe:
      9° còn khó thấy → 12°.)
- [x] **Bóp mặt** ("bóp cả khuôn mặt" theo chiều ngang; phải = hẹp lại):
      toàn bộ viền `FACE_OVAL` **và** mắt, mày, mũi, miệng co / giãn ngang
      quanh trục giữa mặt cùng tỉ lệ (8 % ở 100) — khác "Mặt thon" (chỉ
      hàm/má). Mống mắt dời theo tâm (giữ tròn). Vòng neo giữ nền như cũ;
      probe lưới: cột cửa, tường cạnh má gần như thẳng.
- [x] Probe `probe_reshape` thêm 4 thanh (cả chiều âm), lưới khi bóp mặt, và
      ảnh cận miệng / mắt `rz_*.png`; bỏ qua ảnh không thấy mặt. Test app
      `face_shape_warps_the_face_only_and_is_kept_in_the_recipe` thêm 4 thanh
      (khóe miệng đổi, góc ảnh giữ, công thức giữ đủ thanh dáng).
- Cổng: chủ test — cười/mếu tự nhiên, môi không vỡ viền, mắt nghiêng không méo
  mống mắt, bóp mặt không cong nền gần má. **Đạt — chủ test OK 02/10.**

### Pha A4 — Sắp xếp lại bố cục hộp thoại (chủ yêu cầu 02/10)

- [x] Chia thành các **nhóm thu gọn được**, mũi tên ▸/▾ ở tiêu đề: Tô vùng,
      Da, Dáng mặt (tiêu đề nhỏ Khuôn mặt: Mặt thon, Bóp mặt, Cằm, Trán · Mắt &
      mũi: Mắt to, Mắt nghiêng, Mũi thon · Miệng: Rộng miệng, Cười, Môi dày),
      Mắt (trắng mắt, sáng / màu / phủ màu tròng), Môi & răng (đậm / sáng /
      màu / phủ màu môi, trắng răng), Lông mày, Tóc, Chi tiết.
- [x] **Mặc định tất cả đóng** (mỗi lần mở hộp thoại); bấm mở một nhóm thì
      **nhóm đang mở tự đóng**. Cọ "Tô vùng" chỉ bật khi nhóm Tô vùng mở —
      đóng nhóm / mở nhóm khác thì cọ tắt (nên bỏ dòng "đang tô: xem trước
      chưa uốn" ở Dáng mặt).
- [x] Tiêu đề nhóm có **chấm xanh** bên phải khi trong nhóm có thanh khác 0
      (thanh chọn màu không tính; Tô vùng: khi đã có nét tô).
- [x] Nút Mặc định / Về 0, Xem trước, Hiện vùng nhận diện, Áp dụng / Hủy giữ
      ở dưới cùng, ngoài vùng cuộn, luôn thấy.
- Cổng: chủ test — hộp thoại gọn, mở/đóng nhóm mượt, không mất thanh nào.
  **Đạt — chủ test OK 02/10.**

### Pha B0 — Phân tích dáng người (cổng giữ/bỏ cách làm)

- [x] Khung người: từ mặt kéo xuống theo tỉ lệ người (≈ 8 lần chiều cao đầu),
      cắt theo vùng ảnh / vùng chọn; chạy Sapiens2 trên 1–2 khung đứng (nửa
      trên, nửa dưới) để người không quá nhỏ ở 512×384.
- [x] Từ các vùng: **bóng người** (mọi lớp trừ nền), thân, tay, chân; suy ra
      đường vai, eo (chỗ thân hẹp nhất giữa nách và hông), hông, trục bắp tay /
      cẳng tay / đùi / cẳng chân (trục chính của từng vùng), cổ (giữa cằm và
      vai).
- [x] Probe trên ảnh nửa người, toàn thân đứng, ngồi, áo rộng, váy dài, tay
      chống hông.
- Cổng: đường eo/hông/vai và trục tay chân đúng ở ≥ 8/10 ảnh thử. Không đạt →
  đề xuất chủ cho tải MediaPipe Pose (khung xương 33 điểm) ghép với vùng
  Sapiens2.
- Lần 1 (02/10 chiều): `Segmenter::segment_body` — khung người đứng thẳng
  3:4 quanh mặt (3,5e mỗi bên, 2,2e trên, 10e dưới tâm mặt, cắt theo ảnh /
  vùng chọn), 1 hoặc 2 khung chồng; ~1,6 s/khung trên CPU. Probe
  `probe_body_labels` (IAI_PORTRAIT_BODY_PROBE) trên 15 ảnh (8 ảnh CC0 mới:
  đứng chống hông, ngồi ×3, áo phông, váy ngắn, dang tay, áo kẻ — nguồn trong
  `SOURCES.txt`; 2 ảnh không bắt được mặt). Kết quả: **bóng người rất chuẩn**,
  tay / chân **để trần** tách đúng từng đoạn (bắp tay, cẳng tay, bàn tay, đùi,
  cẳng chân); nhưng tay / chân **trong tay áo, quần, váy, áo khoác** chỉ ra
  "Áo" / "Quần/váy" → không có khuỷu, gối, không tách được tay khỏi eo khi tay
  buông sát người (9/13 ảnh có mặt). Hai khung chồng không nét hơn đáng kể
  (khung bị chiều ngang 7e giữ rộng). **Không đạt** với Sapiens2 một mình →
  đề xuất chủ tải MediaPipe Pose (Google, Apache-2.0, `pose_landmarker_heavy`,
  chuyển ONNX như Face Mesh bằng `tmp/model-export-env`); khung xương cho vai,
  khuỷu, cổ tay, hông, gối, cổ chân, Sapiens2 cho đường viền.
- Lần 2 (02/10 chiều, chủ đồng ý tải): `pose_landmarker_heavy.task` chính
  thức → `pose_landmarks_detector.onnx` (tf2onnx opset 17, lệch TFLite
  ≤ 0,007 px; `models/pose/` + `%APPDATA%\IAI\models\pose\`). Module
  `core::ai::pose`: không dùng model tìm người — khung đầu suy từ mặt (hông
  ~3,6e dưới tâm mặt), rồi 2 lần cắt lại theo 2 điểm căn của chính model như
  vòng theo dõi của MediaPipe; tinh chỉnh theo heatmap như MediaPipe;
  60–200 ms CPU. So với pipeline Python chính thức của Google trên 15 ảnh:
  lệch trung vị 1,5–4 % chiều dài thân; chỗ khác nhau (hông bị váy che, tay
  áo) bản iAi hợp lý hơn; iAi bắt được cả ảnh ngồi Google bỏ sót.
- `core::portrait::body`: `Silhouette` (vùng người Sapiens2 nối liền từ
  giữa hai vai — bỏ người khác) + `measure(pose, silhouette, chin)` →
  `BodyShape`: trục thân, vai (mép ngoài qua khớp vai), cổ (chỗ hẹp nhất
  dưới cằm, bề rộng 0,2–0,7 vai), eo (hẹp nhất trong dải 0,55–0,78 từ vai
  xuống hông, kéo về 0,64; bỏ khi < 0,5 vai = tay che trước bụng), hông
  (rộng nhất 0,9–1,15), từng đoạn tay / chân (khớp → khớp, bề ngang ở giữa;
  vùng tay = chuỗi khớp tay bán kính 0,12 thân, không tính vào eo). Probe
  `probe_body_shape` (IAI_PORTRAIT_BODY_SHAPE_PROBE) → `bs_*.png`.
- **Cổng: Đạt (nội bộ, 02/10)** — 13 ảnh có mặt: vai đúng 12/13, trục tay
  chân đúng 11/13 (sai: người ngồi co sát khung), eo hợp lý 9/11 ảnh đo được
  (bỏ đúng ở ảnh tay che bụng / ngồi). Hạn chế: áo khoác mở / áo rộng thì
  "eo" là mép áo ngoài; ảnh không bắt được mặt thì không có dáng người.

### Pha B1 — Dáng người

- [~] Thanh (mục **"Dáng người"** của hộp thoại, sau Dáng mặt; mức ở 100):
  - **Eo thon**: hai mép thân ở dải eo dời vào trục 15 % (mạnh nhất ở đường
    eo, nhạt dần 0,3 thân lên ngực, 0,35 xuống hông); tay buông sát eo dời
    theo mép eo, tay cách eo thì đứng yên.
  - **Vai**: mép ngoài vai + đường từ cổ ra vai dời vào / ra 10 % nửa bề
    rộng vai; khớp vai, khuỷu, cổ tay đi theo (1 ; 0,75 ; 0,5).
  - **Cổ**: đầu (viền mặt, đỉnh tóc, hai bên tóc) nâng lên 30 % chiều dài cổ,
    mép cổ dãn đều từ cằm xuống vai.
  - **Tay thon** / **Chân thon**: mép tay / chân dời vào trục xương 18 % /
    12 %, chỉ mép nào là đường viền người (mép chạm thân / chân kia đứng yên);
    nhạt dần ở vai, hông, cổ tay, cổ chân.
  - **Chân dài** (một chiều 0..100): kéo dãn cả hàng ngang phía dưới hông
    tới 12 % (vào dần quanh hông); chỉ khi đứng (đùi gần thẳng đứng), thấy cổ
    chân, và dài tới mức bàn chân còn trong ảnh.
- [~] Neo: trục thân + mép thân ngoài dải đang chỉnh + vòng neo chữ nhật quanh
      bóng người (cách 0,35 vai); điểm điều khiển gom theo vị trí để hai thanh
      cùng chạm một điểm thì cộng dồn (không hai đích cho một điểm).
- [~] Phân tích dáng người chạy **khi kéo thanh dáng người đầu tiên** (vài
      giây, hộp thoại báo "Đang phân tích dáng người…"), không làm chậm ai chỉ
      chỉnh mặt; mặt quá to (cận mặt) bỏ qua. Áp dụng chờ phân tích xong.
- [~] Uốn ghép tầng: mặt trước, người sau, chân dài cuối (`warp_region` nay
      nhận các tầng nối tiếp) → cổ dài vẫn mang đúng mặt đã thon.
- [~] Probe `probe_body_shape` thêm `bw_*.png` (ảnh | eo | vai | cổ | tay |
      chân | chân dài | lưới); 13 ảnh: thay đổi đúng chỗ, khung cửa / chân trời
      thẳng; 10–850 ms mỗi thanh trên ảnh 20 MP. Test app
      `body_shape_analyses_bodies_on_first_use_and_narrows_the_waist`.
- Cổng: chủ test — ở mức vừa người thon tự nhiên, nền cạnh eo/tay không cong
  thấy rõ. **Chủ test 02/10 tối: "chưa được chính xác" — TẠM GÁC** (chủ ít
  dùng, để khi rảnh làm tiếp). Khi quay lại: hỏi chủ ảnh / thanh nào sai,
  dùng probe `probe_body_shape` (`bs_*`, `bw_*`) để dò.

### Pha C (tùy chọn) — Giữ nền thẳng bằng vá AI

- [ ] Uốn chỉ người (theo bóng người mềm), phần nền lộ ra khi người thon lại vá
      bằng LaMa từ nền gốc (đã có cho Smart Fill / Repair Brush).
- Chỉ làm khi chủ thấy nền còn cong ở Pha A/B.

## 3. Rủi ro

| Rủi ro | Xử lý |
|---|---|
| Nền cong cạnh má/eo (cửa, kệ, chân trời) | Vòng neo sát + MLS rigid + giới hạn mức; probe đo độ cong; Pha C vá nền |
| Mốc mặt lệch ở mặt nghiêng / bị che (tay, tóc) | Mức dời theo bề rộng thấy được mỗi bên; bỏ mặt có mốc kém tin cậy |
| Kính mắt bị méo khi Mắt to | Chấp nhận ở mức vừa; ghi chú trong gợi ý thanh |
| Sapiens2 khung cả người quá nhỏ ở 512×384 | Chạy 2 khung dọc; không đạt thì đề xuất MediaPipe Pose |
| Tay ép sát thân, váy rộng che eo | Neo chung; eo đọc theo bóng người, không theo da |
| Ảnh lớn chậm | Lưới thô, chỉ tính trong khung ảnh hưởng, luồng nền |

## 4. Changelog

- **2026-10-02 (tối)** — Chủ test Dáng người: chưa chính xác, **tạm gác** (ít
  dùng). Đợt 10 dừng ở đây; việc tiếp theo chủ chọn: **chỉnh ảnh thẻ** (phiên
  mới).

- **2026-10-02 (tối)** — B1 code xong: 6 thanh dáng người (Eo thon, Vai,
  Cổ, Tay thon, Chân thon, Chân dài), phân tích dáng người khi cần, uốn ghép
  tầng. Chờ chủ test.

- **2026-10-02 (chiều)** — B0: Sapiens2 một mình không đạt (tay/chân trong
  quần áo); chủ đồng ý tải MediaPipe Pose; ghép khung xương + bóng người →
  **B0 đạt nội bộ** (`446aad6`). Tiếp: B1 (thanh dáng người).

- **2026-10-02 (chiều)** — Chủ test A3 + A4 OK (`d87076e`). Bắt đầu Pha B0.

- **2026-10-02 (chiều)** — Pha A3 + A4 code xong: 4 thanh Cười, Môi dày, Mắt
  nghiêng, Bóp mặt; hộp thoại chia nhóm thu gọn (mặc định đóng, mở một đóng
  các nhóm khác, chấm xanh ở nhóm đang chỉnh). Probe 10/11 ảnh (1 ảnh không
  thấy mặt như trước) đạt nội bộ; chờ chủ test.

- **2026-10-02** — Lập kế hoạch theo lựa chọn của chủ (dáng mặt + dáng người);
  chưa code. Chờ chủ duyệt mục 1 (nhất là tải ảnh thử, và Pose chỉ khi cần).
- **2026-10-02** — Chủ test Dáng mặt (A0–A2) OK. Chủ yêu cầu thêm: miệng cười
  / mếu, môi dày / mỏng, mắt nghiêng, bóp cả khuôn mặt theo chiều ngang (Pha
  A3) và sắp xếp lại bố cục thành nhóm thu gọn, mặc định đóng, mở một đóng
  các nhóm khác (Pha A4). **Làm ở phiên mới** (context phiên này đầy): A3 →
  A4 → rồi mới tới B0 (dáng người).
- **2026-10-02** — Chủ duyệt toàn bộ. Tải 12 ảnh CC0 (Unsplash qua Wikimedia
  Commons; giữ 11, nguồn ghi trong `SOURCES.txt` ở thư mục probe). Pha A0 +
  A1 + A2 code xong (`4880b1a`); probe + test đạt; chờ chủ test Dáng mặt.
