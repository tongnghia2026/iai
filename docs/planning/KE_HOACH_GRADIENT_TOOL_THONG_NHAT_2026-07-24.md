# Kế hoạch thống nhất Gradient Tool pixel/vector — 2026-07-24

## 1. Mục tiêu

Chỉ có một Gradient Tool (`G`) và một Gradient Editor. Công cụ tự suy ra backend
từ mục tiêu chỉnh sửa đang active:

- mask đang active: **Pixel / Mask**;
- `Path` hoặc `Shape`: **Vector**;
- `Raster`: **Pixel**;
- `Text`, `SmartObject`, `Adjustment`, `Group`: **Disabled** và báo rõ lý do.

Mode là dữ liệu suy ra từ document/layer hiện tại, không phải một nút mode để người
dùng tự bật. Vì vậy đổi layer trong Layers panel, canvas auto-select hoặc đổi document
sẽ cập nhật mode ngay ở frame tiếp theo và không thể bị lệch state.

Active layer/paint target là nguồn sự thật. Không dò màu pixel tại điểm bấm để chọn
backend, vì gradient pixel phải bắt đầu được cả trên vùng raster trong suốt. Khi
multi-select, Gradient Tool chỉ tác động active layer và UI phải ghi rõ target đó.

## 2. Hành vi UI đã chốt

Options bar luôn có:

- badge `Vector`, `Pixel`, hoặc `Mask`;
- thanh gradient mở cùng một Gradient Editor;
- loại gradient;
- Reverse.

Phần theo backend:

- Pixel: Linear, Radial, Angle, Reflected, Diamond; Opacity, Dither, Blend Mode,
  Lock Alpha/selection/channel gate giữ nguyên semantics hiện có.
- Vector: Linear và Radial trong đợt này; on-canvas origin/axis/stop handles; không
  hiện Dither và pixel Blend Mode.
- Unsupported/locked: controls bị disable và status bar nói chính xác nguyên nhân.

Với vector:

- layer đã có gradient: Gradient Editor đọc trực tiếp ramp của layer;
- layer đang Solid/None: chỉ chọn tool chưa được phép sửa document; lần drag đầu
  tạo gradient từ working ramp;
- drag vùng trống tạo/thay gradient geometry;
- kéo handle chỉnh transform gradient hiện có;
- đóng editor/Done tạo một undo step; Esc khôi phục baseline;
- đổi target khi editor đang mở phải chốt phiên cũ theo đúng `doc_id + layer_id`,
  tuyệt đối không áp dữ liệu sang layer mới.

Với pixel:

- editor sửa preset của Gradient Tool;
- drag/release áp raster như hiện tại và tạo đúng một stroke history;
- đổi sang vector không tự ghi preset pixel vào object nếu người dùng chưa drag hay
  sửa gradient của object.

## 3. Kiến trúc

### 3.1. Target resolver dùng chung

Thêm một resolver thuần dữ liệu:

```text
GradientTarget
├── Pixel { doc_id, layer_id, target: Pixels | Mask }
├── VectorPath { doc_id, layer_id }
├── VectorShape { doc_id, layer_id }
└── Unsupported { reason }
```

`App::active_gradient_target()` là điểm duy nhất phân loại. UI, pointer router,
overlay và action handler đều dùng kết quả này; không tự kiểm tra `LayerType` rải rác.

### 3.2. Ramp dùng chung, renderer vẫn tách

Chuyển stop/ramp dùng chung vào `src/core/gradient.rs`:

- `GradientStop { offset, color: ColorValue }`;
- `GradientRamp`, 2..=8 stops, sorted, validation và sampling chung;
- giữ RGB/CMYK process color, alpha và reverse không mất dữ liệu;
- adapter UI chuyển sang RGBA chỉ để preview; action phải mang `ColorValue` khi màu
  gốc là CMYK.

Hai backend vẫn độc lập:

- Pixel backend tạo LUT và ghi tile/mask/ink plane.
- Vector backend cập nhật `Paint::Gradient`, transform và raster cache dẫn xuất.

Không đưa pixel options như Dither/Blend Mode vào model vector. Không rasterize
vector chỉ để tái sử dụng pixel backend.

### 3.3. Gesture dùng chung

Gradient Tool chỉ quản lý gesture hình học:

- start/current;
- Shift constraint;
- loại gradient;
- preview line/handle hit.

App router quyết định backend:

- Pixel release gọi pixel apply;
- Vector drag live-preview model/transform và release commit gateway command.

Mọi vector action phải pin `doc_id + layer_id`; kết quả worker/bake trễ của target cũ
phải bị loại bỏ.

## 4. Shape vẫn phải là primitive vector

Không tự convert Rectangle/Ellipse/Polygon/Star thành Path khi áp gradient.

Nâng `ShapeData` dùng style vector chuẩn (`VectorStyle` hoặc một wrapper dùng chung
`Paint`/`StrokeStyle`) thay cho chỉ `fill_color`/`stroke_color`. Tạo một adapter chuẩn
`ShapeData -> VectorObjectData` và dùng nó cho:

- raster cache;
- crisp canvas overlay;
- PDF native;
- hit/display bounds.

Geometry Shape, corner radius, sides và star inner-radius vẫn giữ nguyên, nên người
dùng tiếp tục chỉnh được primitive sau khi thêm gradient.

Tương thích file:

- writer mới lưu style gradient đầy đủ;
- reader ưu tiên style mới;
- file `.iai` cũ chỉ có `fill`, `fill_color`, `stroke_width`, `stroke_color` được
  nâng thành `VectorStyle` solid khi mở;
- save/open không được âm thầm convert Shape thành Path.

Thêm `ChangeShapeStyle` command nhỏ, lưu before/after style và dựng lại raster cache;
không snapshot toàn canvas.

## 5. Các giai đoạn triển khai

### G1 — Target router và UI mode

- Thêm `GradientTarget` và resolver.
- Thêm mode/target vào `UiData`.
- Options bar đổi controls theo target.
- Đồng bộ khi chọn layer từ panel, canvas, đổi document, undo/redo và đổi paint target.
- Chưa thay renderer.

Điều kiện qua cổng: test ma trận LayerType/PaintTarget đạt; đổi layer liên tục không
làm thay đổi document.

### G2 — GradientRamp dùng chung

- Tách model stop/ramp chung.
- Pixel LUT dùng sampler chung.
- Vector `Paint::Gradient` dùng ramp chung.
- Gradient Editor giới hạn 8 stops và không làm mất CMYK/alpha.
- Migration `.iai` cho Path gradient hiện có.

Điều kiện qua cổng: golden sampling pixel không đổi; round-trip RGB/CMYK/alpha đạt.

### G3 — Path backend trong Gradient Tool

- Route press/drag/release theo `GradientTarget`.
- Solid Path: drag đầu tạo Linear/Radial gradient.
- Gradient Path: hiển thị và chỉnh handle khi Gradient Tool active.
- Editor stop live-preview; Done/release là một undo step; Esc phục hồi.
- Loại bỏ menu Stops riêng sau khi tính năng tương đương đã đạt.

Điều kiện qua cổng: Path không đổi loại layer, undo/redo chính xác, zoom lớn không
ghost/jitter, PDF vẫn native.

### G4 — Shape style/vector adapter

- Nâng Shape style và reader/writer tương thích ngược.
- Dùng adapter chuẩn cho render/display/PDF.
- Thêm `ChangeShapeStyle`.
- Route Gradient Tool vào Shape mà vẫn giữ primitive handles.

Điều kiện qua cổng: Rectangle/Ellipse/Polygon/Star vẫn chỉnh geometry được sau khi
thêm gradient; save/open/undo/PDF đều giữ gradient.

### G5 — Hoàn thiện chuyển mode và dọn UI

- Commit/cancel phiên editor đúng target khi đổi layer/document.
- Tách working preset pixel khỏi ramp đang thuộc vector object.
- Badge/status/disabled reason hoàn chỉnh.
- Xóa model/action/UI gradient trùng đã không còn dùng.
- Cập nhật tài liệu giao ca và shortcut/help.

Điều kiện qua cổng: chuyển qua lại Pixel -> Path -> Shape -> Mask không rò state,
không ghi nhầm layer và không tạo undo entry khi chỉ chọn layer.

### G6 — Regression, performance và release

- Chạy `cargo fmt --check`, targeted tests, `cargo test --lib`.
- Build `cargo build --release --bin iai`.
- Test canvas 100%, 200%, 400%; RGB, CMYK và mask.
- So sánh pixel gradient trước/sau bằng golden image/hash.
- Kiểm tra worker bake không block UI và kết quả stale không được nhận.

## 6. Danh sách test bắt buộc

### Tự động

1. Resolver ưu tiên active mask hơn LayerType vector.
2. Raster body -> Pixel; Path/Shape body -> Vector; unsupported -> Disabled.
3. Chọn layer/mở document không tạo history và không sửa dữ liệu.
4. Ramp validation: 2..=8 stops, sort/order, reverse, alpha.
5. RGB và CMYK `ColorValue` round-trip không đổi.
6. Pixel Linear/Radial/Angle/Reflected/Diamond giữ output hiện tại.
7. Pixel selection, lock alpha, channel gate, CMYK ink planes và mask không hồi quy.
8. Solid Path drag tạo gradient; undo trả lại Solid.
9. Path handle drag và stop edit mỗi gesture chỉ tạo một undo step.
10. Shape gradient không đổi `LayerType::Shape`, geometry/primitive handles giữ nguyên.
11. `.iai` cũ/mới round-trip cho Path và Shape gradient.
12. PDF native giữ Linear/Radial gradient và đúng z-order; fallback vẫn an toàn.
13. Đổi target khi editor/worker đang hoạt động không áp kết quả vào layer mới.

### Test tay trước khi báo người dùng

1. Chọn Raster -> badge Pixel -> kéo gradient.
2. Chọn Path Solid -> badge Vector -> kéo để tạo gradient.
3. Chọn Path có gradient -> sửa stop và kéo handles.
4. Chọn Rectangle/Ellipse/Star -> áp gradient rồi tiếp tục resize/radius/sides.
5. Chọn mask của Raster/Path/Shape -> badge Mask -> gradient chỉ tác động mask.
6. Chuyển nhanh Raster/Path/Shape/Mask trong lúc Gradient Editor mở.
7. Undo/redo từng thao tác; save/reopen `.iai`.
8. Zoom 200%/400%, kiểm tra canvas không răng cưa, ghost hoặc giật handle.
9. Export PDF và kiểm tra vector/gradient bằng viewer ở zoom lớn.

## 7. Ngoài phạm vi đợt này

- Angle/Reflected/Diamond vector và PDF tương ứng.
- Gradient mesh, conical/diamond native PDF.
- Gradient cho editable Text hoặc SmartObject.
- Nhiều object cùng nhận một gradient transform trong một gesture.

Các mục này chỉ được mở sau khi Linear/Radial chung cho Raster/Path/Shape/Mask đạt
đầy đủ tiêu chí trên.

## 8. Kết quả triển khai 2026-07-24

Đã hoàn thành hướng kiến trúc được duyệt:

- `LayerType` chỉ còn một nhánh vector:
  `Vector(VectorGeometry::Primitive | VectorGeometry::Path)`.
- Shape Tool vẫn được giữ để tạo/chỉnh Rectangle, Ellipse, Line, Polygon và Star;
  Shape không bị đổi thành Path khi áp gradient.
- Primitive Shape và Path dùng chung `VectorStyle`, `Paint::Gradient`, stop, transform,
  command style, hiển thị sắc nét và pipeline PDF.
- Gradient Tool tự suy ra target từ active layer/paint target:
  Raster -> Pixel, Vector -> Vector, active mask -> Mask.
- Options bar hiển thị badge Pixel/Vector/Mask; Vector chỉ có Linear/Radial và không
  hiện Dither/Opacity của raster backend.
- Gradient Editor dùng ramp của object vector khi object đã có gradient; với Solid,
  editor chỉ sửa preset cho đến lần kéo đầu tiên.
- Gradient vector giữ nguyên primitive geometry, hỗ trợ handles trên canvas, undo/redo,
  lưu/mở `.iai`, CMYK/alpha stop và native PDF shading.
- Chuyển layer/document khi editor đang mở chốt đúng phiên cũ trước khi đổi target.
- Chỉnh radius/sides/star-inner của Shape có gradient không làm gradient bị đổi về màu đặc.
- Reader `.iai` vẫn mở được tag Shape/Path cũ; writer giữ tag tương thích nhưng model
  trong bộ nhớ đã hợp nhất.

Kết quả tự động cuối đợt: 896 test — 892 pass, 4 diagnostic ignored, 0 fail.
`cargo build --release --bin iai` hoàn tất thành công.

Trên Windows, nếu linker báo `LNK1201` khi ghi PDB test, chạy:

```powershell
$env:CARGO_PROFILE_TEST_DEBUG='0'
cargo test --lib --no-fail-fast
```

## 9. Bổ sung hiệu năng on-canvas handles — đã hoàn tất

- Overlay gradient thuộc riêng Gradient Tool, không còn lưu lại khi chuyển tool.
- Pointer-move trên Path/Shape chỉ thay đổi model và vị trí handle.
- Không queue worker bake hoặc raster lại Primitive trong lúc giữ chuột.
- Thả chuột mới raster cache cuối một lần; undo/redo vẫn là một gesture.
- Display bake hoàn tất luôn ép một follow-up frame, không phụ thuộc input kế tiếp.
- Trước commit, xóa crisp-display cache/worker/suppression cũ và ép full composite từ raster cuối.
- UI layer kind phân biệt lại `Shape` và `Path`; Convert to Curves giữ nguyên gradient style.
- Gradient Tool vector đánh dấu old/new bounds dirty và App composite ngay trong release event.
- Có test end-to-end press/drag/release không dùng follow-up input.
- Regression hiện tại: 902 test, 898 pass, 4 ignored, 0 fail.
