# Giao ca: hiển thị vector NÉT CĂNG khi phóng to canvas + việc còn lại

Ngày: 2026-07-23 · Nhánh: `feat/vector-core-foundation` · HEAD: `6428234`

Người giao (Claude/Opus) đã làm xong loạt slice Node/Shape trong phiên này. Ca này
bàn giao cho Codex làm tiếp **nhiệm vụ chính: canvas hiển thị Path nét căng ở mọi
mức zoom** (hiện đang bị "răng cưa/pixel" khi phóng to), rồi các việc còn lại.

---

## 0. Trạng thái repo khi giao ca

- Full lib test: **826 pass, 0 fail**, `cargo fmt --check` sạch.
- Working tree SẠCH (chỉ còn 2 mục untracked của người dùng — ĐỪNG đụng/commit):
  `KE_HOACH_PHAT_TRIEN_VECTOR_IAI.txt` và `dist/`.
- Các commit phiên này (ĐỀU CỤC BỘ — **CHƯA PUSH**, cố ý; xem mục Quy ước):
  - `dfdb623` live Path transform mượt (GPU inverse-homography) + Free Transform
    overlay hết đè dialog + PDF hợp nhất (`core::print::collect_pdf_vectors`/
    `pdf_raster_base` ẩn Path khỏi raster base → hết halo, cả 4 đường export + hybrid).
  - `9b8d1bb` Node tool: kéo tay-nắm Bézier (`ops::apply_handle_move` coupling
    Cusp/Smooth/Symmetric) + double-click anchor đổi kind.
  - `d73675c` Node tool: bấm Path khác để sửa (`node_click_select_path`) + Alt+bấm
    cạnh đổi thẳng↔cong (`node_convert_segment`).
  - `6428234` Convert Shape to Curves (`core/vector/from_shape.rs` + menu
    Layer ▸ Convert to Curves).
- Người dùng đã GUI-test các slice trên và xác nhận OK.

---

## 1. NHIỆM VỤ CHÍNH — hiển thị Path nét căng khi zoom

### 1.1. Nguyên nhân gốc (đã xác định)

Path hiển thị trên canvas qua RASTER cache (`Layer.tiles`) ở **độ phân giải tài
liệu**. Compositor upload tile vào GPU atlas và lấy mẫu bằng **`mag_filter:
FilterMode::Nearest`** (`src/gpu/compositor.rs:948`). Vì vậy zoom-in = phóng to pixel
tài liệu theo kiểu nearest → cạnh fill bị bậc thang/pixel. Đây là HẠN CHẾ NỀN TẢNG,
không phải bug: raster cache đúng cho export/thumbnail nhưng không phải là biểu diễn
độ-phân-giải-màn-hình.

Lưu ý: đường VIỀN (outline) của Node tool ĐÃ nét căng — nó vẽ bằng egui painter ở
screen-space (`src/ui/mod.rs` khối vẽ `NodeOverlay`, dùng `to_screen_pos`). Cái còn
pixel là phần TÔ (fill) và hiển thị Path khi KHÔNG ở Node tool.

Đừng đổi `mag_filter` sang `Linear` toàn cục: sampler dùng chung mọi layer → sẽ làm
mờ ảnh raster (photo) khi zoom, không mong muốn.

### 1.2. Ba hướng (kèm tradeoff)

**Hướng A — Re-raster Path ở ĐỘ PHÂN GIẢI VIEW rồi vẽ overlay (KHUYẾN NGHỊ cho MVP).**
Dùng lại rasterizer đã có (`core/vector/raster.rs::rasterize`) nhưng render đối tượng
ở scale = mức zoom hiện tại (có cap, ví dụ ≤ 4–8×, hoặc giới hạn theo RAM), rồi vẽ
buffer đó như một TEXTURE egui overlay ở screen-space, phủ lên phiên bản atlas bị mờ.
- ƯU: tái dùng rasterizer ĐÃ đúng fill-rule/holes/AA/stroke/CMYK-mirror → nhất quán
  với export; KHÔNG đụng GPU compositor/atlas (atlas là doc-space, tránh sửa sâu);
  KHÔNG thêm dependency.
- NHƯỢC: overlay opaque sẽ che layer nằm TRÊN Path → **MVP giới hạn: chỉ vẽ crisp
  cho Path đang ACTIVE (đối tượng đang sửa) khi zoom > 1** (tránh vấn đề z-order; đây
  cũng là lúc người dùng cần nét nhất). Có re-raster khi đổi zoom/model → THROTTLE
  theo `model_generation` + "bucket" của zoom (đừng re-raster mỗi frame). Cache
  display-raster keyed `(layer_id, model_gen, zoom_bucket)`.
- Delivery: egui `Context::load_texture`/`TextureHandle` + `painter.image(...)` đặt
  đúng canvas-rect của layer (map local→screen như overlay Node đang làm). Xem khối
  `NodeOverlay` trong `src/ui/mod.rs` để biết cách map + clip theo canvas viewport.

**Hướng B — Tessellate fill thành tam giác, vẽ vector thật ở screen-res (chuẩn dài hạn).**
Flatten path (`core/vector/flatten.rs` đã có) rồi tessellate CÓ fill-rule + holes
thành mesh, vẽ qua egui `Shape::mesh`/`Shape::Path` hoặc GPU. `Shape::convex_polygon`
của egui KHÔNG xử lý lõm-có-lỗ; fill của egui cũng không even-odd đa-contour. → cần
tessellator thật.
- ƯU: vector thật, nét ở mọi zoom, không tốn RAM theo zoom², dùng lại cho cả preview
  lẫn (tương lai) render GPU.
- NHƯỢC: **CẦN QUYẾT ĐỊNH THÊM DEPENDENCY** (`lyon_tessellation`) HOẶC tự viết
  triangulation (ear-clipping/scanline có fill-rule) — công sức lớn hơn, phải test kỹ
  holes/self-intersection. Thêm crate là quyết định cần HỎI NGƯỜI DÙNG trước (Mục 10
  của KE_HOACH: thay dependency phải trình bày tác động trước).

**Hướng C — Bilinear cho riêng Path (nửa vời).** Cho Path layer dùng sampler Linear
(giảm bậc thang thành mờ). Rẻ nhưng KHÔNG nét căng thật, và cần tách sampler theo
loại layer trong compositor. Không khuyến nghị làm mục tiêu chính.

### 1.3. Khuyến nghị

1. Làm **Hướng A** trước (MVP: crisp cho Path active khi zoom-in) — không dependency,
   không đụng compositor, tái dùng rasterizer đúng. Đây là "dùng được ngay" và hợp
   kỷ luật Mục 3.10 (thêm file/hàm, không refactor lớn).
2. Nếu người dùng muốn nét căng cho MỌI Path (không chỉ active) và/hoặc vector thật →
   **HỎI NGƯỜI DÙNG** về việc thêm `lyon` rồi làm **Hướng B**. Đừng tự thêm crate.

### 1.4. File liên quan

- `src/gpu/compositor.rs:948` — `mag_filter: Nearest` (nguồn "pixel khi zoom").
- `src/core/vector/raster.rs` — `rasterize(&VectorObjectData) -> Option<Raster{rgba,w,h,offset}>`
  (đã đúng fill-rule/holes/AA/stroke; rayon-parallel). Dùng cho Hướng A ở scale cao.
- `src/core/vector/flatten.rs` — `flatten_path`/`flatten_contour` (cho Hướng B).
- `src/ui/mod.rs` — khối vẽ `NodeOverlay` (mẫu map canvas→screen + clip viewport +
  vẽ overlay). Chèn overlay Path crisp ở đây.
- `src/app/vector_transform.rs` — `active_path_layer()`, `path_layer_hit_at()`,
  `VectorObjectData::local_bounds`, view zoom/offset (lấy scale + canvas-rect).
- `src/app/render*` / `src/ui/viewmodel.rs` — nơi dựng dữ liệu overlay cho UI (thêm
  trường "path display raster" nếu đi Hướng A qua viewmodel).
- Tham khảo tiền lệ: Shape layer từng xử lý mượt bằng "outline vector + bake
  off-thread" (xem lịch sử `3d4bf1c`→`a5b2e38`); Node/Pen overlay vẽ nét căng qua
  egui painter.

### 1.5. Tiêu chí nghiệm thu

- Zoom 400–1600% một Path fill (có cả lỗ, ví dụ chữ "O") → cạnh trong/ngoài nét, tôn
  trọng fill-rule/holes, KHÔNG bậc thang.
- Không regression: raster photo zoom vẫn như cũ; export PDF/PNG không đổi (đây chỉ là
  hiển thị); Move/Node/scale/rotate vẫn mượt (re-raster display THROTTLE, không mỗi
  frame).
- CMYK: hiển thị đúng (dùng mirror RGB của rasterizer).
- Có test: rasterize ở scale N cho buffer đúng kích thước ~N×; throttle không re-raster
  khi zoom/model không đổi; (Hướng B) tessellation có test holes/even-odd.
- `cargo fmt --check` sạch + full lib test xanh.

---

## 2. VIỆC CÒN LẠI KHÁC (sau nhiệm vụ chính)

- **Bước 6 — Clipping/PowerClip (phần NỀN)** [KE_HOACH T6.1–T6.3]: discriminator quan
  hệ (LayerRelation `GroupMember|ClippedInside` HOẶC `clip_parent_id`) + invariant
  cycle/reorder/duplicate/delete trong `layer.rs` (TỐI THIỂU), logic ở FILE MỚI
  `src/core/canvas/clip_ops.rs`; command CreateClippedPixelChild/ReleaseClippedChild
  + CreateOrAttachRasterMask (atomic create + nét đầu) qua gateway; save/load giữ
  nesting/relation. ĐÂY LÀ BƯỚC NỀN CUỐI trước Foundation Freeze.
- **Cổng FOUNDATION FREEZE** [KE_HOACH Mục 3.11 / cuối Mục 15]: chỉ tuyên bố khi (a)
  10 contract nền có round-trip + property test, (b) M1 chạy end-to-end RGB+CMYK
  (Pen→Path→sửa node→Fill/Outline→màu→save .iai→mở→PDF — hiện ĐÃ đủ mảnh), (c)
  "giả lập trên giấy" Polygon/Star + Boolean + Text→Curves + đa trang/Artboard xác
  nhận KHÔNG đòi đổi contract. Đạt → ĐÓNG BĂNG, từ đó tính năng CHỈ THÊM.
- Node còn thiếu (nhỏ, tuỳ chọn): break/join contour qua UI, multi-select node +
  marquee, align nodes (`ops::align_nodes` đã có, chưa nối UI).
- Giai đoạn 4 còn lại: thêm primitive MỚI Polygon/Star (ShapeKind hiện chỉ
  Rectangle/Ellipse/Line) + hợp nhất Shape dùng VectorStyle/ColorValue.

---

## 3. QUY ƯỚC WORKSPACE (BẮT BUỘC)

- **CHỈ commit CỤC BỘ, KHÔNG PUSH.** Người dùng chốt: gom cả loạt commit vector, chỉ
  push khi user bảo (Actions gần cạn). Xong việc = fmt + test + commit cục bộ + báo
  cáo bằng ngôn ngữ thường (hướng kết quả), ĐỪNG hỏi push, ĐỪNG hỏi câu hỏi kỹ thuật.
- Trước khi commit: `cargo fmt` + `cargo test --lib` phải xanh.
- Commit message dài → `git commit -F <file>`; kết thúc bằng dòng
  `Co-Authored-By:` theo chuẩn.
- ĐỪNG sửa file `.rs` bằng Get/Set-Content PowerShell (phá UTF-8) — dùng công cụ edit.
- ĐỪNG đụng/commit `KE_HOACH_PHAT_TRIEN_VECTOR_IAI.txt` và `dist/` (untracked của user).
- KỶ LUẬT MỤC 3.10: mỗi phase chỉ THÊM file/hàm; sửa file lớn giới hạn (1 variant enum
  + 1 nhánh dispatch + 1 dòng đăng ký module). Logic nặng → file mới trong cây
  `src/core/vector/` hoặc `src/core/canvas/`.
- User là end-user: quyết định kỹ thuật tự làm, chỉ HỎI khi việc không đảo ngược /
  ảnh hưởng dữ liệu của họ (ví dụ: thêm dependency `lyon` → nên báo/hỏi trước).
