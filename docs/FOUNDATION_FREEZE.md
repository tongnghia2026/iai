# 🧊 FOUNDATION FREEZE — nền tảng vector iAi

**Tuyên bố đóng băng: 2026-07-23** · Nhánh: `feat/vector-core-foundation`

Kể từ mốc này, **10 contract nền tảng dưới đây được ĐÓNG BĂNG**. Lớp tính năng
(Giai đoạn 3 trở đi) chỉ được **THÊM**: thêm variant vào enum đã chừa chỗ, thêm
command mới, thêm file tool/ui. **Đổi một contract đã đóng băng** đòi hỏi: ADR +
migration + lý do đo được (Mục 3.11). Nếu một tính năng *ép* đổi
`PathData`/`ColorValue`/schema thì đó là dấu hiệu nền thiếu — quay lại ADR, KHÔNG vá
lên nền.

## Điều kiện cổng — đã đạt cả ba

| # | Điều kiện (Mục 3.11) | Bằng chứng |
|---|----------------------|-----------|
| 1 | 10 contract có test round-trip + property | `iai_vector.rs` (round-trip + property sweep 72 object), `iai.rs` (Canvas RGB/CMYK + page_id round-trip), `command_vector.rs` (undo/redo), `path.rs`/`color.rs`/`style.rs`/`page.rs` (validate/round-trip) |
| 2 | M1 chạy end-to-end RGB **và** CMYK | Test tự động `m1_end_to_end_rgb_*` + `m1_end_to_end_cmyk_*` (Create→Node→Style→Transform→undo/redo→`.iai`→PDF); **user GUI-test xác nhận OK 2026-07-23** |
| 3 | Giả lập 4 tính năng tương lai không đòi đổi contract | [FOUNDATION_FREEZE_PAPER_SIM.md](FOUNDATION_FREEZE_PAPER_SIM.md) — cả 4 additive; contract #10 đã vật chất hóa ([ADR_PAGE_OWNERSHIP.md](ADR_PAGE_OWNERSHIP.md)) |

## 10 contract đã đóng băng

| # | Contract | Định nghĩa | Test chính |
|---|----------|-----------|-----------|
| 1 | `AffineTransform` + `PathData/Contour/Node/FillRule` | `core/vector/affine.rs`, `path.rs` | round-trip, transform, validate |
| 2 | `ColorValue` (Rgb/Cmyk/opacity) | `core/vector/color.rs` | ink verbatim, pure-K, validate |
| 3 | `VectorStyle/StrokeStyle/Paint` | `core/vector/style.rs` | default, adapter, validate |
| 4 | Coordinate space (local→transform→layer→offset, page-relative) | `core/vector/object.rs` | layer_bounds, local_bounds |
| 5 | `LayerType::Path` + discriminator clip (`clip_parent_id`) | `core/layer.rs`, `canvas/clip_ops.rs` | clip round-trip ≠ group |
| 6 | Command vector qua gateway | `core/command_vector.rs` | undo/redo mọi lệnh |
| 7 | Cache/generation + atlas invalidation | `core/vector/raster.rs`, `canvas/*` | rasterize/rebuild |
| 8 | Schema `.iai` Path (`{schema, objects:[...]}`) + version v4 | `formats/iai_vector.rs`, `iai.rs` | round-trip + property |
| 9 | Rasterize policy (opt-in `RasterizeVectorLayer`) | `core/command_vector.rs` | rasterize→undo |
| 10 | Page/Artboard identity (`PageId`/`Page`/`Layer::page_id`) + page-relative coords | `core/page.rs`, `layer.rs`, `iai.rs` | page_id round-trip |

## Ba đòn bẩy additive (vì sao mở rộng không đụng nền)

- `PathData` = danh sách `Contour` + `fill_rule` → mọi hình phẳng (kể cả lỗ, nhiều
  mảnh) biểu diễn được: Polygon/Star, Boolean, Text→Curves đều ra `PathData`.
- `ColorValue`/`Paint` là enum chừa chỗ → Spot/Gradient/Pattern là **thêm variant**.
- Envelope `.iai` là **danh-sách-object** + mọi key optional-mặc-định → nhiều
  object/1 layer, page ownership, master pages… đều thêm không bump phá tương thích.

## Lớp tính năng mở tiếp (chỉ THÊM trên nền đã băng)

Pick/Node nâng cao (break/join/multi-select/align) · Polygon/Star · PowerClip UX +
compositor clipping · Boolean · Contour/offset · gradient/dash · Vector Brush ·
Text→Curves · PDF vector nâng cao · Artboards/đa trang · Trace.

> Trạng thái test lúc đóng băng: **843 lib test pass, 0 fail, 4 ignored**, fmt sạch.
