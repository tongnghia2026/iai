# Giao ca phát triển vector iAi — 2026-07-24

## Điểm tiếp quản

- Repository: `iai`
- Branch: `feat/vector-core-foundation`
- Commit mới nhất: `bbf1229 feat(vector): add dash and gradient styles`
- Toolchain: Rust/Cargo trên Windows.
- Lệnh kiểm tra tin cậy:

```powershell
cargo fmt --check
$env:CARGO_PROFILE_TEST_DEBUG='0'
cargo test --lib
```

Kết quả trước khi giao ca: **864 passed, 0 failed, 4 ignored**.

## Cập nhật tiếp nối — Palette/Overprint và hiển thị/PDF sắc nét

Trạng thái mới nhất trong workspace (chưa đóng commit riêng):

- Canvas zoom lớn dựng chung top visible vector run cho cả `Path` và editable `Shape`;
  không còn phụ thuộc layer đang active.
- Live Move ở zoom trên 100% tạm dùng document composite trong lúc giữ chuột, tránh
  overlay vector bất đồng bộ cũ làm preview nhảy vị trí; khi thả chuột overlay sắc nét
  được dựng lại từ transform đã commit.
- PDF native hỗ trợ top run gồm cả `Path`/`Shape`, gradient fill và dash; giữ đúng
  thứ tự z. Object có overprint được giữ trong raster fallback vì writer hiện chưa
  có PDF ExtGState overprint, tránh âm thầm làm mất semantics in.
- Color & Brush có Quick Palette theo gesture Corel:
  - click trái áp Fill;
  - click phải áp Outline;
  - ô X bỏ Fill/Outline.
- Document Swatches:
  - thêm màu hiện tại, đổi tên, xóa;
  - giữ nguyên `ColorValue` RGB/CMYK;
  - add/rename/remove qua gateway và undo/redo;
  - `.iai` round-trip trong `document_swatches`; palette có dữ liệu buộc container v4.
- UI `Overprint Fill` và `Overprint Outline` độc lập ở options bar và Color panel;
  thay đổi qua `ChangeVectorStyle`, undo/redo và `.iai` round-trip.
- Build kiểm chứng mới nhất:
  - `cargo test --lib`: **885 passed, 0 failed, 4 ignored**;
  - `cargo build --release --bin iai`: thành công;
  - binary: `target/release/iai.exe`, build lúc 2026-07-24 16:17.

Giới hạn công bố: overprint đã có model/UI/persistence/preflight fallback, nhưng PDF
native overprint và compositing plate-level chính xác vẫn thuộc backend in nâng cao.
Không promote object overprint sang PDF vector cho đến khi writer có ExtGState tương
đương và có golden separation test.

Kế hoạch tiếp theo đã chốt: hợp nhất Gradient Tool theo target active (Raster/Mask
dùng pixel backend, Path/Shape dùng vector backend), giữ một Gradient Editor nhưng
không trộn hai renderer. Chi tiết và tiêu chí test nằm trong
`KE_HOACH_GRADIENT_TOOL_THONG_NHAT_2026-07-24.md`.

## Trạng thái đã hoàn thành

Chuỗi commit gần nhất:

- `bbf1229` — Dash và linear/radial gradient.
- `7e64663` — Hoàn tất thao tác object Phase 5.
- `91cf81f` — Không vẽ cache thô phía dưới crisp overlay.
- `ab1ad50` — Làm mượt ranh giới giữa các mức zoom supersampling.
- `650ef9e` — Shape editable hiển thị sắc nét khi zoom.
- `5d8c0f5` — Polygon và Star editable.
- `a9c99d9` — Bake crisp Path overlay ngoài UI thread.
- `ac6bbe9` — Node marquee, break và join.

Phase 6 hiện có:

- Model `DashPattern`: tối đa 8 phần tử, có offset và validation.
- Model gradient: Linear/Radial, tối đa 8 stops, transform riêng, giữ `ColorValue`
  RGB/CMYK.
- Raster hiển thị solid/dash/gradient, gradient bám object-local transform.
- Options bar của Move/Node có Solid/Linear/Radial và Solid/Dashed/Dotted.
- Hai chip màu sửa màu đầu/cuối gradient; thay đổi đi qua undo/redo.
- `.iai` round-trip dash, offset, stops, transform và CMYK.
- PDF: solid Path vẫn được promote thành vector native; Path có dash/gradient được
  giữ trong raster base để không mất artwork cho đến khi PDF writer hỗ trợ native.

## Việc cần kiểm tra thủ công đầu tiên

1. Tạo Rectangle/Polygon/Star, Convert to Curves.
2. Chọn Path bằng Move hoặc Node.
3. Thử Fill `Linear` và `Radial`, đổi cả hai chip màu.
4. Bật Outline, thử `Dashed` và `Dotted`, đổi Width.
5. Undo/redo từng thao tác.
6. Lưu `.iai`, đóng/mở lại và đối chiếu hình.
7. Xuất PDF và kiểm tra gradient/dash không mất hoặc sai màu.
8. Zoom lớn để chắc crisp overlay không tái xuất hiện răng cưa hai lớp.

## Phần Phase 6 còn nên hoàn thiện

Không coi các mục sau là đã xong chỉ vì model lõi đã có:

- UI chỉnh dash array và dash offset tự do; hiện UI chỉ có ba preset.
- UI thêm/xóa/di chuyển stop; hiện UI dùng gradient hai màu.
- On-canvas gradient handles để sửa transform trực quan.
- Palette interaction và document swatch.
- UI cho overprint Fill/Outline và kiểm thử separations.
- PDF native shading/dash nếu muốn giữ gradient/dash dưới dạng vector PDF.

Ưu tiên hợp lý: hoàn thiện ba mục UI dash/gradient ở trên trước, sau đó
palette/swatch/overprint. Không thêm Pattern khi chưa có workflow thật.

## Phase kế tiếp theo kế hoạch gốc

Sau khi Phase 6 đạt DoD, kế hoạch gốc chuyển sang **Phase 6B — Artistic Media /
Vector Brush**:

- Vector Brush tạo open `Path` với appearance editable.
- Pressure/width profile lưu theo normalized arc length.
- Preview smoothing/stabilizer, commit geometry một lần.
- Solid variable-width stroke trước; texture/nozzle để sau.
- Có `Expand Stroke` trước khi dùng Boolean.

Sau Phase 6B là **Phase 7 — Boolean/Shaping**: Weld, Trim/Difference và Intersect
cho closed paths, với policy target/style và tolerance rõ ràng.

## File và quy tắc kiến trúc quan trọng

- `src/core/vector/style.rs`: Paint, StrokeStyle, DashPattern, Gradient.
- `src/core/vector/raster.rs`: raster nguồn cho Path cache.
- `src/formats/iai_vector.rs`: schema `.iai` cho Path.
- `src/core/print.rs`, `src/formats/pdf.rs`: policy PDF vector/raster.
- `src/app/path_style.rs`: app glue, preview và một undo step.
- `src/ui/topoptions.rs`: options bar Move/Node.
- `KE_HOACH_PHAT_TRIEN_VECTOR_IAI.txt`: kế hoạch nền đầy đủ.

Giữ invariant:

- Model vector là nguồn thật; `Layer::tiles` chỉ là cache/fallback.
- Không dùng Free Transform raster để commit Path editable.
- Mỗi thao tác người dùng phải là một undo step hợp lý.
- Không promote một phần style sang PDF nếu việc đó làm mất phần còn lại.
- Không sửa/xóa thay đổi không liên quan của người dùng.

## Ghi chú workspace khi đóng gói

- `KE_HOACH_PHAT_TRIEN_VECTOR_IAI.txt` đang untracked nhưng được đưa vào gói vì là
  tài liệu kế hoạch cần thiết.
- `dist/` là output build, không đưa vào ZIP.
- `target/` khoảng 24 GB, không đưa vào ZIP; máy mới chạy Cargo để build lại.
- `.git/` được đưa vào ZIP để giữ branch và lịch sử commit, giúp tiếp tục ngay.

## Cập nhật hoàn tất — Vector layer và Gradient Tool thống nhất

Đã triển khai quyết định giữ Shape Tool nhưng hợp nhất model layer:

- `LayerType::Vector(VectorGeometry::Primitive(ShapeData))`
- `LayerType::Vector(VectorGeometry::Path(VectorObjectData))`

Hai geometry cùng dùng `VectorStyle`. Gradient Tool tự route theo target active:

- Raster body: pixel backend.
- Vector Primitive/Path: vector backend.
- Active layer mask: mask/pixel backend, có ưu tiên cao hơn loại layer.

Các invariant đã khóa bằng test:

- Áp gradient lên Shape không convert Shape thành Path.
- Chỉnh geometry của Shape gradient không làm mất gradient.
- Gradient Editor tạo một undo hợp lý và giữ chính xác CMYK khi chỉ di chuyển stop.
- Đổi document lúc editor đang mở commit vào canvas cũ, không rơi sang layer trùng id.
- `.iai` round-trip Shape gradient RGB/CMYK/alpha và vẫn đọc schema Shape/Path cũ.
- PDF xuất gradient của Primitive Shape thành native axial/radial shading.

Kết quả cuối: 896 test — 892 pass, 4 diagnostic ignored, 0 fail; release build thành công.

Lệnh kiểm thử dùng trong đợt này:

```powershell
$env:CARGO_PROFILE_TEST_DEBUG='0'
cargo test --lib --no-fail-fast
cargo build --release --bin iai
```

## Cập nhật sửa lag Gradient handles — 2026-07-24

- Gradient overlay và hit-test chỉ hoạt động khi `Gradient Tool` đang active. Đổi sang Move,
  Node, Shape hoặc tool khác sẽ ẩn ngay đường/handle gradient.
- Khi kéo handle trên Vector Primitive hoặc Path, ứng dụng chỉ cập nhật vector model và overlay;
  không raster lại layer ở từng pointer event.
- Raster cache cuối được dựng đúng một lần khi thả chuột và toàn bộ gesture chỉ tạo một undo step.
- Crisp display worker được tạm ngưng trong lúc kéo để không tạo các bake lớn/stale song song.
- Có test trực tiếp cho Star để khóa lỗi hiệu năng đã thấy trên ảnh người dùng.
- Khi display worker hoàn thành sau bước thu thập UI, event loop ép thêm một frame trình bày;
  gradient cuối hiện ngay lúc thả chuột, không cần click layer hoặc đổi tool.

Khắc phục bổ sung sau test tay:

- Vector display cũ, worker cũ và trạng thái suppression được xóa trước khi commit gradient;
  compositor dựng lại từ raster cache cuối rồi mới tạo crisp overlay mới.
- Pointer release chuyển về trạng thái không-drag trước khi commit.
- UI không còn parse `Debug` của `LayerType`: Primitive được gắn nhãn `Shape`, curve được gắn
  nhãn `Path`, nên `Convert to Curves` và `Rasterize Layer` hoạt động lại.
- Convert Shape có gradient sang Curves giữ nguyên toàn bộ `VectorStyle`.

## Sửa nguyên nhân gốc Gradient Tool không cập nhật — 2026-07-25

Các bản vá display trước chỉ xử lý handle editor. Lỗi còn lại thuộc thao tác kéo gradient mới:

- `GradientTool::apply_vector_gradient` đã thay model/tiles nhưng không đánh dấu bounds dirty.
- Release path gọi `flush_canvas()`, thấy dirty rỗng nên không composite; zoom/Move sau đó mới làm
  kết quả hiện ra.
- Backend giờ đánh dấu union bounds cũ/mới và tăng `layer_revision` ngay sau command thành công.
- App release nhận biết Vector Gradient, xóa crisp overlay cũ và gọi `LayerPixelsChanged` trong
  cùng sự kiện thả chuột.
- Test mới mô phỏng đúng press → drag → release trên Star, xác nhận flat canvas đổi ngay mà
  không chèn zoom, Move, đổi tool hoặc click layer.

Kết quả regression sau sửa: 902 test — 898 pass, 4 diagnostic ignored, 0 fail.

Việc cần làm tiếp theo theo roadmap sau khi người dùng xác nhận đợt này:
Phase 6B — Artistic Media / Vector Brush; không cần bỏ Shape Tool.
