# Bàn giao ca cho Claude — Vector/Gradient

Ngày bàn giao: 2026-07-25

Branch: `feat/vector-core-foundation`

Commit bàn giao: commit chứa chính file này (xem `git log -1`)

## Đọc trước khi sửa code

1. `KE_HOACH_PHAT_TRIEN_VECTOR_IAI.txt` — roadmap và các invariant nền.
2. `GIAO_CA_VECTOR_2026-07-24.md` — lịch sử triển khai Phase 6 và các lỗi đã sửa.
3. `KE_HOACH_GRADIENT_TOOL_THONG_NHAT_2026-07-24.md` — quyết định hợp nhất
   Gradient Tool cho raster, mask và vector.

Không quay lại mô hình Shape/Path là hai loại layer độc lập. Model hiện tại là:

```rust
LayerType::Vector(VectorGeometry::Primitive(ShapeData))
LayerType::Vector(VectorGeometry::Path(VectorObjectData))
```

Primitive và Path dùng chung `VectorStyle`. Shape Tool vẫn cần để tạo/chỉnh primitive;
`Convert to Curves` là thao tác explicit chuyển Primitive sang Path.

## Trạng thái đã hoàn thành

- Gradient Tool tự nhận target:
  - raster body dùng Pixel backend;
  - Vector Primitive/Path dùng Vector backend;
  - active mask dùng Mask backend và có ưu tiên cao hơn loại layer.
- Gradient vector hỗ trợ Linear/Radial, nhiều stop, editor và on-canvas handles.
- Kéo handle chỉ cập nhật model/overlay khi đang drag; raster cache dựng một lần lúc
  release và toàn bộ gesture chỉ tạo một undo step.
- Handle gradient chỉ hiện và nhận input khi Gradient Tool active; đổi sang Move,
  Node, Shape hoặc tool khác thì ẩn ngay.
- Primitive có gradient vẫn sửa được tham số shape và Convert to Curves giữ nguyên
  `VectorStyle`.
- Dash array/offset, palette/swatch, CMYK, overprint, `.iai` round-trip và PDF native
  gradient shading đã được nối vào workflow hiện tại.
- Crisp vector overlay ở zoom lớn đã xử lý răng cưa/twin image; khi Move layer ở
  zoom trên 100% overlay được tạm ngưng để layer không nhảy về vị trí cũ.
- UI xác định Primitive là `Shape`, curve là `Path` bằng mapping enum trực tiếp;
  không parse chuỗi `Debug` của `LayerType`.

## Lỗi cuối cùng vừa sửa

Triệu chứng: tạo/kéo gradient mới trên vector rồi thả chuột nhưng canvas chưa đổi;
phải zoom, Move, đổi tool hoặc click layer mới thấy kết quả.

Nguyên nhân:

- `GradientTool::apply_vector_gradient` đã thay model/tiles nhưng không đánh dấu
  bounds dirty và không tăng `layer_revision`.
- Release gọi `flush_canvas()`, thấy dirty region rỗng nên không composite.

Cách sửa:

- `src/tools/gradient.rs`: lấy bounds cũ/mới, đánh dấu cả hai vùng dirty và tăng
  `canvas.layer_revision` sau command thành công.
- `src/app/input/pointer.rs`: release của Vector Gradient gọi
  `finish_vector_gradient_release()`, invalidate crisp display và áp dụng
  `CanvasEvent::LayerPixelsChanged` ngay trong cùng sự kiện release.
- Test end-to-end
  `app::input::pointer::tests::vector_gradient_release_updates_flat_canvas_without_followup_input`
  mô phỏng press → drag → release trên Star và xác nhận flat canvas đổi ngay, không
  chèn thêm zoom/Move/click.

Quy tắc rút ra: mọi mutation vector ở tầng Tool phải đánh dấu bounds cũ/mới dirty,
tăng revision phù hợp và để App invalidate display cache/overlay trong cùng gesture.

## Kết quả kiểm thử tại thời điểm bàn giao

```text
cargo test --lib --no-fail-fast
902 tests: 898 passed, 4 ignored, 0 failed

cargo build --release --bin iai
Thành công
```

Binary đã kiểm:

```text
target/release/iai.exe
Size:   59,202,560 bytes
SHA256: C70E2AC1AF2FAEFEDD84F95793D6EBE23FD0135EBAA4BFDEF385C9A04BE86886
```

Trên Windows, nếu debug linker bị khóa PDB/EXE (`LNK1201`/`LNK1104`), dùng:

```powershell
$env:CARGO_PROFILE_TEST_DEBUG='0'
cargo test --lib --no-fail-fast
```

Nếu vẫn đụng test binary đang bị khóa, có thể tạm đặt
`$env:CARGO_PROFILE_TEST_CODEGEN_UNITS='64'` để Cargo tạo artifact hash khác.

Hai cảnh báo future-incompatible ở `src/ui/library.rs` về float literal là nợ có
sẵn, không liên quan Vector/Gradient.

## Checklist smoke test thủ công

1. Tạo Rectangle/Polygon/Star, dùng Gradient Tool kéo Linear và Radial; kết quả phải
   hiện ngay khi thả chuột, không cần thao tác phụ.
2. Kéo từng gradient handle trên Shape và Path; preview phải mượt, release giữ đúng
   kết quả và Undo chỉ lùi một gesture.
3. Đổi từ Gradient sang Move/Node/Shape; đường và handle gradient phải biến mất ngay.
4. Zoom trên 100%, giữ và kéo layer bằng Move; layer không được nhảy loạn hoặc quay
   về vị trí cũ.
5. Convert Shape có gradient sang Curves; hình và gradient không đổi, Node Tool sửa
   được path.
6. Lưu/mở lại `.iai`; kiểm tra geometry, gradient stops, dash, RGB/CMYK, swatch và
   overprint.
7. Xuất PDF, zoom lớn trong PDF viewer; vector/gradient không răng cưa như bitmap
   thấp độ phân giải và không mất màu/appearance.

## Invariant phải giữ

- Model vector là nguồn sự thật; `Layer::tiles` chỉ là cache/fallback.
- Không dùng Free Transform raster để commit Path editable.
- Mỗi gesture người dùng là một logical undo step.
- Không promote một phần style sang PDF nếu làm mất appearance còn lại.
- Sau khi rebuild raster cache vector, phải reconcile CMYK ink planes.
- Kết quả async display worker cũ không được ghi lên target/document mới.
- Không đưa selection, hover, active handle hay trạng thái tool vào model tài liệu.
- Không âm thầm rasterize vector để né lỗi geometry/style.

## Việc tiếp theo theo roadmap

Bắt đầu **Phase 6B — Artistic Media / Vector Brush**, chưa chuyển thẳng sang Boolean.
Lát cắt đầu tiên nên nhỏ và kiểm thử được:

1. Viết ADR ngắn cho `VectorStroke` và pressure/width profile theo normalized arc
   length; không bake width thành bitmap.
2. Thêm Vector Brush như tool riêng, không đổi Pixel Brush/Eraser hiện tại.
3. Thu pointer samples, smoothing/stabilizer ở preview; khi release commit đúng một
   open Path và một undo step.
4. Render solid variable-width stroke trước; chưa làm texture/nozzle/preset phức tạp.
5. Path tạo ra phải chọn bằng Move, sửa geometry bằng Node, save/load `.iai` và giữ
   appearance qua undo/redo.
6. Thêm `Expand Stroke` thành closed outline trước khi Phase 7 dùng Boolean.

Definition of Done cho lát cắt đầu:

- Vector Brush tạo editable open Path, không tạo Pixel Paint.
- Pressure profile ổn định khi số sample/distance thay đổi và round-trip `.iai`.
- Preview không commit command ở mỗi pointer move.
- Release tạo đúng một command/undo; cancel không để lại object/cache bẩn.
- Node edit, Move, zoom lớn và reload tài liệu không làm stroke đổi hình.
- Có unit test model/schema và test App press → drag → release.

Sau Phase 6B mới làm **Phase 7 — Boolean/Shaping**: Weld, Trim/Difference và
Intersect cho closed paths, với selection order, target/style policy, fill rule và
tolerance được chốt rõ trước khi code.

## Các vùng code cần tra cứu

- `src/core/vector/` — geometry, style, display và raster cache.
- `src/core/layer.rs`, `src/core/shape.rs` — unified Vector layer/primitive.
- `src/core/command_vector.rs` — command và undo cho vector.
- `src/tools/gradient.rs`, `src/tools/move_tool.rs` — tool behavior đã ổn định.
- `src/app/input/pointer.rs` — routing press/drag/release và invalidation.
- `src/app/path_gradient.rs`, `src/app/path_style.rs`, `src/app/path_display.rs` —
  app glue cho style, gradient và crisp overlay.
- `src/formats/iai_vector.rs`, `src/formats/iai_palette.rs` — persistence.
- `src/formats/pdf.rs`, `src/core/print.rs` — PDF capability/promotion policy.

Trước khi bắt đầu Phase 6B, chạy `git status --short` và test baseline. Nếu phải đổi
contract nền (`PathData`, `VectorStyle`, schema), ghi ADR/migration thay vì vá ngầm.
