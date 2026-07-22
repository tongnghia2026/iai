# Giao ca: Vector Transform và PDF native-vector

Ngày cập nhật: 2026-07-22

## Mục tiêu của ca

Ca này xử lý ba lỗi liên quan:

1. Scale/rotate Path bị lag và khung transform chạy trước, raster vector chạy sau.
2. Khung Free Transform vẽ đè lên dialog/cửa sổ nổi.
3. PDF vẫn răng cưa vì nhiều đường export là raster-only, hybrid dùng bitmap overlay,
   và nhánh có vector vẫn giữ bản raster của chính Path bên dưới.

## Trạng thái hiện tại

### Live Path transform

- `src/app/vector_transform.rs` không còn gọi `request_path_bake()` trên mỗi pointer
  update.
- Trong khi kéo, Path dùng raster cache hiện có và compositor GPU dùng full inverse
  homography (`TransformPreviewUniform`) để đưa nó tới transform `pending`.
- Khung và hình preview cùng đọc transform `pending` trong một frame.
- Ma trận sample destination -> source là:

  ```text
  original_transform * inverse(pending_transform)
  ```

- Khi release, GPU preview được clear, canvas được recompose từ model, rồi
  `ChangeVectorTransform` commit transform cuối và raster hóa cache chất lượng đầy đủ
  một lần. Undo vẫn là một command và Path vẫn editable.
- Worker trong `src/app/path_bake.rs` vẫn được giữ cho node edits và style edits; chỉ
  đường nóng scale/rotate đã bỏ worker.

Test quan trọng:

- `gpu_path_preview_maps_pending_destination_back_to_original_canvas`
- `drag_handle_scales_stays_editable_and_undoes` đồng thời khẳng định không có
  `path_bake`/`path_bake_next` trong live drag.

### Z-order Free Transform

- `src/ui/mod.rs` chuyển `transform_overlay` từ `egui::Order::Foreground` sang
  `CANVAS_TOOL_OVERLAY_ORDER` (`Background`).
- Painter được clip theo giao của canvas screen rect và canvas viewport.
- Dialog vẫn dùng `Foreground`, vì vậy transform chrome không thể đè lên dialog,
  panel hoặc phần ngoài canvas.

### PDF pipeline dùng chung

Logic phân loại Path đã chuyển khỏi tầng `app` xuống `src/core/print.rs`:

- `collect_pdf_vectors(canvas) -> PdfVectorSelection`
- `PdfVectorSelection.objects`: các native PDF paths.
- `PdfVectorSelection.promoted_layer_ids`: ID phải bỏ khỏi raster base.
- `pdf_raster_base(canvas, selection)`: clone LayerStack, ẩn promoted IDs và flatten;
  không mutate document/undo/visibility thật.

Điều kiện Path được promote hiện tại (bảo toàn hình ảnh trước):

- RGB document.
- Effective visibility true.
- Path nằm trên toàn bộ visible non-Path content vì writer hiện có một raster base
  rồi mới nối vector content.
- Layer và mọi group ancestor: opacity 100%, Normal blend, không mask.
- Object opacity 100%.
- Fill/stroke hiện là solid paint được PDF writer hỗ trợ.

Không đủ điều kiện thì Path vẫn nằm trong raster; không bị mất nội dung.

### Các đường PDF đã thống nhất

1. Generic format exporter (`src/formats/pdf.rs::PdfExporter`)
   - Trước: `export_flat_up_to -> build_pdf`, raster-only.
   - Nay: collect selection -> raster base không có promoted Path ->
     `build_pdf_with_vectors`.

2. File > Export > PDF multi-page (`src/app/file_ops/save_export.rs`)
   - Dùng cùng collector và raster-base split.

3. Print / Save as PDF (`src/app/actions/ui_color_print.rs`)
   - Dùng cùng collector.
   - Cả flat-buffer và row-streamed path đều ẩn promoted Path khỏi raster trước khi
     nối vector objects.

4. Imported PDF hybrid (`src/formats/pdf.rs`, `src/core/document.rs`)
   - `HybridPageContent::Overlay` giờ mang cả `rgba` và `vectors`.
   - `PdfPageRef::safe_overlay_pdf_parts` tách edits trên pristine PDF base thành
     transparent raster overlay + native Path edits.
   - `add_vector_overlay` nối native path content vào trang gốc sau raster overlay.
   - Hybrid vẫn fallback raster page nếu source geometry/structure không an toàn.

### Loại raster twin / halo

Trước đây full canvas (đã chứa Path raster cache) được nhúng làm image, sau đó cùng
Path được vẽ vector lên trên. AA pixels của cache có thể lộ ra thành răng cưa/halo.

Hiện promoted Path bị ẩn khỏi raster base. PDF chỉ chứa native representation của
Path đó; test `promoted_path_is_removed_from_pdf_raster_base` khóa hành vi này.

### Chính sách CMYK

- `collect_pdf_vectors` trả selection rỗng cho CMYK để không trộn DeviceRGB vector
  paints vào DeviceCMYK page.
- Generic PDF exporter thử `flatten_ink` và ghi DeviceCMYK image page.
- Multi-page/Print giữ đường ink-native hiện có.
- Nếu stack không còn ink-exact thì fallback RGB raster như hành vi cũ, nhưng không
  promote Path sRGB.
- Native DeviceCMYK vector operators/output intent chưa được triển khai trong ca này.

## File đã thay đổi

- `src/app/vector_transform.rs`
- `src/ui/mod.rs`
- `src/core/print.rs`
- `src/core/document.rs`
- `src/formats/pdf.rs`
- `src/app/file_ops/save_export.rs`
- `src/app/actions/ui_color_print.rs`
- `docs/HANDOFF_CLAUDE_VECTOR_TRANSFORM_PDF.md`

## Kiểm thử đã chạy

```text
cargo check
cargo clippy --lib
cargo test vector_transform -- --nocapture
cargo test formats::pdf -- --nocapture
cargo test
```

Kết quả full suite tại thời điểm giao ca:

```text
807 passed; 0 failed; 4 ignored
perf_develop: 2 ignored manual probes
```

`cargo clippy --lib` hoàn tất với warning debt sẵn có của repo. Lệnh
`cargo clippy --all-targets` vẫn fail ở một test cũ trong
`src/core/print_gdi.rs:541` (`clippy::erasing_op` bị deny); lỗi này không nằm trong
các file thay đổi của ca và full test suite vẫn xanh.

Hai warning sẵn có, không thuộc thay đổi này:

- `src/ui/library.rs:220`: float literal f32 fallback.
- `src/ui/library.rs:222`: float literal f32 fallback.

## Test tay đề nghị

### Transform

1. Tạo Path fill lớn, scale liên tục rồi rotate nhanh.
2. Xác nhận khung và hình di chuyển cùng nhau, không còn hình đuổi theo khung.
3. Release: hình nét lại ở transform cuối; Undo/Redo đúng một bước.
4. Thử path nhiều node, stroke-only, zoom rất thấp/cao, flip qua trục.
5. Mở New Canvas, Export, Preferences, filter dialog khi box đang hiện; box không
   được xuyên lên dialog/panel và không ra ngoài canvas.

### PDF

1. Generic Export chọn PDF với Path trên raster: zoom PDF 800-1600%, mép phải mượt.
2. PDF multi-page và Print > Save PDF cho cùng kết quả.
3. Path có Bézier, even-odd hole, fill+stroke, rotate và nonuniform scale.
4. Đặt raster layer lên trên Path: Path phải fallback raster để giữ z-order.
5. Mask/opacity/blend khác Normal: phải fallback raster, không mất hiệu ứng.
6. Mở PDF nguồn, thêm Path trên trang rồi export hybrid: nền gốc và Path mới đều
   giữ vector; raster brush edit vẫn là transparent image overlay.
7. CMYK: xác nhận output là DeviceCMYK raster và màu không đổi bất ngờ.

## Giới hạn có chủ đích / việc có thể làm ở ca sau

1. Writer hiện là một raster base + một vector run trên cùng. Path xen giữa nhiều
   raster runs vẫn fallback raster. Muốn giữ mọi Path cần page scene gồm nhiều
   alternating raster/vector runs và image overlays có alpha đúng z-order.
2. Mask, non-Normal blend, opacity và gradients chưa được ánh xạ sang PDF graphics
   state/shading; hiện fallback raster để đúng hình.
3. Text/Shape editable chưa được promote; collector chỉ xử lý `LayerType::Path`.
4. CMYK vector thật cần DeviceCMYK paint operators, output intent và test prepress;
   hiện cố ý raster ink-native.
5. Live GPU preview dùng raster cache hiện có nên trong lúc phóng rất lớn có thể mềm;
   release sẽ raster lại sắc nét. Đây là tradeoff để giữ tương tác 60 FPS.

## Lưu ý workspace

Trước ca đã có file/folder untracked của người dùng:

- `KE_HOACH_PHAT_TRIEN_VECTOR_IAI.txt`
- `dist/`

Không sửa/xóa chúng. Không có commit được tạo trong ca này.
