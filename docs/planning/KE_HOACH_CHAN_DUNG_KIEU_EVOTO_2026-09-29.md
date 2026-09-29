# Kế hoạch chuẩn: Chỉnh chân dung kiểu Evoto (iAi)

> **Tài liệu chuẩn để theo dõi triển khai.** Mọi phiên làm việc liên quan
> retouch chân dung / mốc mặt phải đọc file này trước khi sửa code và cập nhật
> checklist/changelog trước khi kết thúc phiên.

## 0. Trạng thái

- Ngày lập kế hoạch: **2026-09-29**.
- Nhánh: `feat/vector-core-foundation`.
- Trạng thái: **Phase 0 ĐẠT** (chủ test 29/09: "khá ổn") → giữ MediaPipe cho mốc mặt.
- Việc kế tiếp: chủ test nút Phase 0b (Sapiens2 + kiểm chéo) → giữ/bỏ, rồi
  vào Phase 1.
- Không push nếu chủ chưa yêu cầu. Sau mỗi phase: build Release + đường dẫn
  `.exe` thật rồi mới mời chủ test.

Quy ước checklist: `[ ]` chưa làm · `[~]` đã code, chưa qua cổng nghiệm thu ·
`[x]` xong và qua cổng · `[!]` bị chặn (ghi lý do vào Changelog).

## 1. Quyết định đã khóa

1. **Offline hoàn toàn**: không cloud, không phí theo ảnh, ảnh không rời máy.
2. **AI chỉ để "tìm chỗ"** (mặt, mốc mặt, vùng da/tóc); **phần xử lý là kỹ thuật
   cổ điển** (tách tần số, chỉnh màu theo mask, warp theo mốc). Không dùng AI
   vẽ lại khuôn mặt cho việc làm mịn da → giữ vân da thật, tránh "da nhựa".
3. **Chỉ dùng model cho phép thương mại.** Mốc mặt = MediaPipe Face Mesh V2
   (Apache-2.0, model card Google), tìm mặt = YuNet (MIT). Dài hạn thay dần
   BiSeNet (train trên CelebAMask-HQ, dữ liệu phi thương mại) bằng mask tạo
   từ mốc mặt + MediaPipe selfie multiclass (Apache-2.0).
4. **Tên và giao diện trung tính**, tham khảo quy trình Evoto nhưng không sao
   chép tên/UI.
5. **Không đề xuất lại** 2 control "Chi tiết da" / "Tạo khối" (chủ đã chê 18/09).
6. **Code tách module**: nhận diện ở `src/core/ai/face_mesh.rs`; tính năng
   chân dung sau này ở module riêng (`src/core/portrait/…`). Thử không đạt thì
   gỡ sạch theo danh sách ở mục 3.
7. Kết quả chỉnh luôn ra **layer mới** (ảnh gốc không bị đụng), theo ngữ nghĩa
   PTS: Áp dụng = nướng vào layer; "công thức" (thông số) lưu riêng để dùng lại.

## 2. Kiến trúc

1. **Nhận diện** — `face_mesh::detect(rgba, w, h)`:
   - YuNet tìm mặt trên bản thu nhỏ ≤640px, ở 3 hướng (thẳng, xoay 90° hai
     chiều) để bắt cả ảnh nằm ngang.
   - Mỗi mặt: cắt vuông, xoay cho đường mắt nằm ngang, lề 25% mỗi bên (đúng
     chuẩn model card), đưa vào Face Mesh 256×256 → 478 điểm (gồm 10 điểm mống
     mắt). Lượt 2 cắt lại quanh các điểm của lượt 1 cho khít hơn (giống vòng
     theo dõi của MediaPipe).
   - Lọc mặt trùng giữa các hướng, giữ bản có độ tin cậy cao nhất.
2. **Tách vùng điểm ảnh** — Sapiens2 seg (nếu Phase 0b đạt): 29 lớp ở
   1024×768 — mặt+cổ, tóc, môi trên/dưới, răng trên/dưới, lưỡi, kính, tay/
   chân/thân từng đoạn, quần áo. Chạy 1 lần mỗi ảnh (cache), chạy nền.
3. **Hợp nhất (app chịu trách nhiệm)** — mỗi model lo đúng mảng mạnh:

   | Việc | Nguồn chính | App kiểm/bù bằng |
   |---|---|---|
   | Tìm mặt, đếm người, xoay | YuNet (3 hướng) | độ tin cậy Face Mesh |
   | Hình học: mắt, tròng, lông mày, mũi, đường môi, hướng mặt | MediaPipe | — |
   | Mép da mặt/cổ, tóc mái che trán, tay che mặt | Sapiens2 | viền mặt MediaPipe |
   | Lòng trắng mắt, mống mắt, quầng thâm | MediaPipe (đa giác) | màu điểm ảnh |
   | Răng, môi, lưỡi | Sapiens2 | đường môi trong MediaPipe |
   | Da thân, quần áo, tóc | Sapiens2 | — |
   | Viền hàm để thon mặt | MediaPipe | nắn theo mép mặt Sapiens2 |
   | Kính (tránh làm mịn/lóa) | Sapiens2 | — |

   Quy tắc: mask da = Sapiens2 (mặt+cổ ∪ da thân) − đa giác mắt/lông mày/lỗ
   mũi của MediaPipe − môi/răng/tóc/kính của Sapiens2, rồi làm mềm mép. Hai
   nguồn lệch nhau quá ngưỡng (vd tay che nửa mặt) → vùng đó bị loại khỏi chỉnh
   và mặt được đánh dấu "cần xem lại". Thiếu Sapiens2 (máy yếu / chưa tải) →
   tự lùi về mask từ đa giác MediaPipe + màu da (kém hơn nhưng vẫn chạy).
4. **Hiệu ứng trên GPU (wgpu)**, xem trước ngay khi kéo thanh trượt; bản xem
   trước thu nhỏ như Develop, bấm Áp dụng mới tính full-res.
5. **Công thức chân dung** (`PortraitRecipe`, serde): chỉ thông số, không
   pixel → lưu preset và đồng bộ sang ảnh khác; mỗi ảnh tự nhận diện mặt lại.

## 3. Các phase

### Phase 0 — Thử MediaPipe Face Mesh (cổng quyết định giữ/bỏ)

- [x] Tải bundle chính thức `face_landmarker.task` (Google), lấy
      `face_landmarks_detector.tflite`, chuyển ONNX (tf2onnx, opset 17). So với
      TFLite gốc: lệch tối đa 0,0002 px. Chuẩn hoá input 0..1 (đọc từ metadata).
- [x] Module `src/core/ai/face_mesh.rs` + hàm `retouch::detect_face_seeds`.
- [x] Đo so với pipeline Python chính thức của Google trên 7 ảnh mẫu:
      ảnh chân dung thường lệch 1,2–2,0% (NME theo khoảng cách 2 khoé mắt);
      iAi tìm được các mặt Google bỏ sót (ảnh cũ 3 mặt nhỏ ~70px, 2 bé người
      châu Á, mặt trong canvas 4000×3000, ảnh xoay 80°). ~100–220 ms/ảnh CPU.
- [x] Nút **AI Panel ▸ AI Auto Retouch ▸ "Thử nhận diện mốc mặt (MediaPipe)"**
      → thêm layer "Mốc mặt MediaPipe (thử)" (Ctrl+Z được).
- **Cổng nghiệm thu**: chủ thử trên 10–20 ảnh thật (chân dung, ảnh thẻ, ảnh
  nhóm, nghiêng mặt, đeo kính). Viền mắt, lông mày, môi, mũi, cằm phải bám đúng.
  Không đạt → gỡ theo danh sách dưới đây.
- **Danh sách gỡ nếu bỏ MediaPipe:**
  - Xoá `src/core/ai/face_mesh.rs`, `src/app/actions/face_mesh_trial.rs`.
  - Bỏ dòng `pub mod face_mesh;` (`src/core/ai/mod.rs`), `mod face_mesh_trial;`
    (`src/app/actions.rs`), field `face_mesh_trial` (`src/ui/intent.rs`), nút
    trong `offline_retouch_section` (`src/ui/ai_panel.rs`), nhánh gọi
    `do_face_mesh_trial` (`src/app/actions/ui_dialogs.rs`).
  - `retouch.rs`: bỏ `FaceSeed` + `detect_face_seeds`, trả `model_roots` về
    `fn`; `ai.rs`: trả `place_ai_result_named` về `fn`.
  - Xoá thư mục model `models/face-mesh/` và `%APPDATA%\IAI\models\face-mesh\`.

### Phase 0b — Thử Sapiens2 seg (cổng giữ/bỏ)

- [x] Tải `facebook/sapiens2-seg-0.4b` (1,6 GB, giấy phép Sapiens2), xuất ONNX
      cỡ cố định 512×384 (exporter dynamo; exporter cũ vấp InstanceNorm). So
      với PyTorch: argmax khớp 100%.
- [x] Module `src/core/ai/body_parts.rs`: mỗi mặt (từ Face Mesh) một khung
      3:4 quanh đầu-vai rộng 3× chiều cao mặt, xoay theo đường mắt; kiểm chéo =
      tỉ lệ 468 điểm mốc rơi vào lớp mặt/môi/răng/lưỡi/kính/tóc (ngưỡng 80%).
- [x] Đo trên máy chủ: DirectML (GTX 1050 2 GB) không nạp được → tự về CPU;
      CPU ~1,5 s/mặt + nạp model ~2 s. (PyTorch CPU cùng cỡ: 6–11 s; 1024×768
      qua ONNX: ~9,5 s — không dùng.)
- [x] 7 mặt ảnh mẫu: 6 mặt khớp 100%, mặt ảnh in báo (chấm lưới in) khớp 2% →
      bị đánh dấu đúng (Sapiens2 nhận nhầm là quần áo, MediaPipe vẫn đúng).
- [~] Nút **"Thử tách vùng + kiểm chéo (Sapiens2)"** (dưới nút mốc mặt) → chạy
      nền, thêm 2 layer "Vùng Sapiens2 (thử)" + "Mốc mặt MediaPipe (thử)".
- **Cổng**: chủ xem trên ảnh thật — mép tóc/da/môi/răng đúng, thời gian chịu
  được. Không đạt → gỡ: `src/core/ai/body_parts.rs`,
  `src/app/actions/body_parts_trial.rs`, dòng `pub mod body_parts;`,
  `mod body_parts_trial;`, field `body_parts_trial` (intent.rs), nút trong
  ai_panel.rs, nhánh gọi trong ui_dialogs.rs, dòng `poll_body_parts_trial` trong
  `src/app/input/redraw.rs`, `FaceMesh::frame` + `pub(super)` của
  `sample_rgb`/`blend_over` trong face_mesh.rs; xoá
  `%APPDATA%\IAI\models\sapiens2-seg\` và `tmp/model-sources/sapiens2-seg-0.4b/`.
- Nguồn tái xuất: `tmp/model-sources/sapiens2-seg-0.4b/` (safetensors +
  config), env `tmp/model-export-env` (transformers 5.17, torch 2.14 CPU).

### Phase 1 — Da, mắt, răng cho 1 ảnh

- [ ] Hộp thoại **"Chỉnh chân dung…"** (cách mở giống Develop): danh sách mặt
      (Tất cả / Mặt 1 / Mặt 2…, bấm vào mặt trên ảnh để chọn), xem trước trực
      tiếp trên canvas, Áp dụng → layer mới.
- [ ] Mask hình học từ mốc (mục 2.2) + test tự động trên ảnh mẫu.
- [ ] **Da**: Làm mịn giữ vân (tách tần số: chỉ làm mịn lớp màu/khối), Đều màu
      da, Giảm bóng dầu, Sáng da.
- [ ] **Mụn & thâm**: Tự xoá mụn (tìm đốm tương phản cục bộ trên lớp chi tiết
      trong mask da → vá bằng engine Repair Brush sẵn có), Quầng thâm (nâng
      sáng/khử ám màu dải dưới mắt).
- [ ] **Mắt**: Trắng lòng trắng, Sáng mống mắt. **Răng**: Trắng răng (chỉ các
      điểm sáng, ít bão hoà trong miệng).
- [ ] Tìm mặt nhỏ trong ảnh nhóm lớn: dò theo ô (tile) khi ảnh > 640px.
- [ ] Ghi model vào `docs/AI_MODELS.md` + `THIRD_PARTY.md`; bản portable kèm
      `models/face-mesh/`.
- **Cổng**: kéo thanh trượt mượt trên ảnh 24MP; Áp dụng < 2 giây; chủ test bộ
  20–30 ảnh thật, so với ảnh chụp màn hình kết quả Evoto (bản chỉnh miễn phí).

### Phase 2 — Chỉnh dáng mặt (Liquify theo mốc)

- [ ] Thanh trượt: Mặt thon, Cằm, Mắt to, Cân đối 2 mắt, Mũi nhỏ/hẹp, Môi,
      Trán.
- [ ] Mốc → trường dịch chuyển trên lưới warp sẵn có (`src/core/warp.rs`), giới
      hạn ảnh hưởng trong vùng mặt để nền không cong.
- [ ] Tuỳ chọn vá nền bị méo bằng LaMa (đã có cho Smart Fill).
- **Cổng**: ở mức vừa phải không làm cong đường thẳng ở nền; chủ test.

### Phase 3 — Dáng người

- [ ] Thử MediaPipe Pose Landmarker (Apache-2.0) theo đúng cách Phase 0 (module
      riêng, nút thử, cổng giữ/bỏ).
- [ ] Thanh trượt: Eo, Tay, Chân dài, Vai, Cổ (warp theo khung xương).

### Phase 4 — Công thức + đồng bộ hàng loạt

- [ ] Lưu/nạp công thức chân dung (preset).
- [ ] "Áp công thức cho các tab đang mở" và "Chạy cả thư mục → xuất JPEG",
      chạy nền, có tiến độ + Huỷ; mỗi ảnh tự nhận mặt lại.
- **Cổng**: 100 ảnh chạy không lỗi, RAM ổn định.

### Phase 5 — Tuỳ chọn, làm sau

- [ ] Lọc ảnh: nhắm mắt (tỉ lệ mở mắt từ mốc), ảnh mờ, ảnh trùng.
- [ ] Đổi màu tóc, son/má nhẹ, dọn nền.
- Không nằm trong kế hoạch (quá nặng/generative): relight, mở rộng khung bằng
  AI, đổi biểu cảm.

## 4. Rủi ro đã biết

- Face Mesh được huấn luyện cho video selfie: mặt nghiêng mạnh (>60°) hoặc bị
  che nhiều (tay, tóc, khẩu trang) có thể bám kém → cho phép loại mặt khỏi
  danh sách chỉnh.
- YuNet chạy ở 640px: mặt rất nhỏ trong ảnh nhóm lớn có thể bị sót (Phase 1 xử
  lý bằng dò theo ô).
- Model ONNX do mình tự chuyển từ TFLite: giữ checksum
  (`58b89dbb…ed4cd`) và script chuyển đổi khi đưa vào chính thức.

## 5. Model thay thế / bổ sung đã khảo sát (29/09)

Chủ cho phép dùng model phi thương mại nếu tốt hơn (app miễn phí, nhận
donate). Kết luận:

- **Mốc mặt: giữ MediaPipe.** Các model đứng đầu bảng xếp hạng WFLW (STAR,
  PIPNet, SPIGA, POPoS) chỉ 98 điểm, không có mống mắt, dữ liệu train chỉ cho
  nghiên cứu; hơn MediaPipe chủ yếu ở mặt nghiêng mạnh/bị che. InsightFace
  2d106det: 106 điểm, phi thương mại, không hơn trên mặt chính diện.
  Điểm yếu MediaPipe cần xử lý ở Phase 2: đường viền hàm ở mặt nghiêng bám
  theo mặt lưới 3D, không luôn trùng mép mặt thật → nắn theo mép mask.
- **Mask điểm ảnh (quyết định chất lượng kiểu Evoto): ứng viên Sapiens2**
  (Meta, 04/2026): phân vùng 29 lớp ở 1024×768 (mặt+cổ, tóc, môi trên/dưới,
  răng trên/dưới, lưỡi, kính, tay/chân/thân, quần áo…), kèm pose 308 điểm,
  normal/albedo (mở đường relight), matting. Giấy phép Sapiens2 **cho phép
  thương mại**, cấm giám sát/sinh trắc/deepfake. Nặng: bản nhỏ nhất có đầu
  seg = 0,4B tham số, 1,26 TFLOP/ảnh. Máy chủ: GTX 1050 2 GB + i5-13400F →
  GPU dễ thiếu VRAM, CPU ước 10–20 s/ảnh (phải đo).
- Dự phòng nếu Sapiens2 quá chậm: model face-parsing nhẹ hơn (SegFace,
  SegFormer face-parsing; dữ liệu CelebAMask-HQ phi thương mại) — để người
  dùng tự tải như ô model tuỳ chỉnh, không đóng gói.

## 6. Changelog

- **2026-09-29** — Lập kế hoạch. Phase 0: chuyển model, module `face_mesh`,
  đo so với Google (tốt hơn ở ảnh nhóm/ảnh cũ/ảnh xoay), nút thử trong AI
  Panel; chờ chủ test.
- **2026-09-29 (khuya)** — Chủ test Phase 0 "khá ổn" → ĐẠT. Khảo sát model
  thay thế (mục 5): giữ MediaPipe; đề xuất thử Sapiens2 cho mask.
- **2026-09-29 (khuya)** — Chủ chốt hướng kết hợp: mỗi model lo mảng mạnh, app
  hợp nhất (bảng ở mục 2.3). Thêm Phase 0b thử Sapiens2.
- **2026-09-30** — Phase 0b code xong: ONNX 512×384, CPU ~1,5 s/mặt trên máy
  chủ (GPU 2 GB không đủ), kiểm chéo bắt đúng mặt ảnh in báo; chờ chủ test.
