# KẾ HOẠCH CANVAS HYBRID VECTOR–RASTER

Ngày soạn: 2026-07-30  
Trạng thái: Chờ duyệt trước khi code  
Phạm vi: Hiển thị/chỉnh sửa trên canvas; chưa thay đổi định dạng `.iai` và pipeline export trong các phase đầu.

## 1. Bối cảnh và vấn đề

iAi hiện lưu Path dưới dạng vector nhưng hiển thị Path thông qua raster cache:

```text
VectorObjectData
→ CPU flatten + tính coverage chống răng cưa
→ RGBA/TileMap ở độ phân giải tài liệu
→ GPU tile atlas
→ canvas
```

Khi zoom lớn, raster độ phân giải tài liệu không còn đủ sắc nét. App hiện tạo thêm một display raster theo zoom bucket:

```text
VectorObjectData
→ CPU rasterize lại ở 2×/4×/8×...
→ display raster supersample
→ overlay lên canvas
```

Việc này không làm UI lag rõ vì chạy trên worker, nhưng:

- Dùng CPU cao, có thể lên 50–60%.
- Bản nét cuối xuất hiện chậm với hàng trăm vector layer.
- Display cache hiện gom cả một vector run; một object thay đổi có thể khiến cả run được rasterize lại.
- Job cũ không bị hủy giữa chừng khi view/model tiếp tục thay đổi.

## 2. Mục tiêu kiến trúc

Canvas trở thành hybrid tự động theo từng layer:

```text
Vector được GPU renderer hỗ trợ ─┐
                                 ├→ compositor theo đúng z-order → màn hình
Raster/photo/brush ──────────────┤
Vector chưa được hỗ trợ → raster fallback ┘
```

Người dùng không phải chọn “canvas pixel” hoặc “canvas vector”. Renderer tự quyết định:

- Layer vector đủ điều kiện: vẽ trực tiếp bằng GPU.
- Layer raster: dùng tile compositor hiện tại.
- Vector có feature chưa được GPU hỗ trợ: dùng raster cache hiện tại.

Anti-aliasing không bị bỏ. Nó được thực hiện ngay trong GPU render thay vì chạy một CPU supersample job nền.

## 3. Mục tiêu sản phẩm

Sau khi hoàn thành:

- Pan/zoom/move/rotate/scale Path không tạo display raster AA nền.
- Geometry không đổi thì không tessellate lại.
- Chỉ Path bị sửa node/geometry mới rebuild mesh.
- Vector vẫn nét ở mọi mức zoom hợp lệ.
- Layer raster và vector xen kẽ đúng z-order.
- Fill rule, opacity, blend, mask và clipping không bị sai âm thầm.
- Feature chưa hỗ trợ phải fallback chính xác, không biến mất.
- Export và file `.iai` giữ nguyên hành vi cho tới phase export riêng.

## 4. Không làm trong giai đoạn đầu

- Không chuyển toàn bộ app thành trình biên tập vector thuần.
- Không tự động rasterize dữ liệu vector nguồn.
- Không xóa rasterizer CPU hiện tại.
- Không sửa format `.iai`.
- Không viết lại PDF/SVG exporter cùng lúc với renderer canvas.
- Không hỗ trợ tất cả gradient/mask/blend/effect ngay trong proof of concept.
- Không bỏ `path_display` trước khi GPU path đạt tiêu chí thay thế.

## 5. Nguyên tắc kỹ thuật bắt buộc

### 5.1 Dữ liệu nguồn không phụ thuộc renderer

`VectorObjectData` tiếp tục là nguồn sự thật. GPU mesh và raster cache đều là dữ liệu dẫn xuất, có thể xóa và dựng lại.

### 5.2 Fallback theo từng layer

Mỗi vector layer phải trả về một trong hai kết quả:

```rust
GpuVector
RasterFallback
```

Không được “cố vẽ” nếu feature chưa hỗ trợ vì có thể làm sai hình.

### 5.3 Không render hai bản cùng lúc

Nếu một Path được GPU vector renderer vẽ, raster twin của chính Path đó phải bị loại khỏi composite tại đúng vị trí. Nếu không sẽ xuất hiện halo/double-edge.

### 5.4 Đúng hình trước, nhanh sau

Mỗi phase phải có ảnh so sánh/reference tests trước khi mở rộng tối ưu.

### 5.5 Có cờ tắt khẩn cấp

Trong thời gian phát triển phải có feature flag hoặc runtime debug flag:

```text
gpu_vector_canvas = off/on
```

Tắt cờ phải đưa app về pipeline hiện tại.

## 6. Quyết định dependency

Repo hiện chưa có tessellator path. Dependency dự kiến:

```toml
lyon_tessellation
lyon_path
```

Không thêm dependency trước khi bắt đầu Phase 1 và được người dùng duyệt.

Lý do ưu tiên Lyon:

- Có fill tessellation với fill rule.
- Có stroke tessellation với join/cap.
- Phù hợp Rust và output triangle mesh dễ đưa vào wgpu.
- Giảm rủi ro tự viết tessellator cho holes/self-intersection.

Nếu thử nghiệm Lyon không giữ đúng semantics hiện tại, dừng Phase 1 để báo cáo; không tự thay bằng renderer lớn khác.

## 7. Kiến trúc module dự kiến

Các file mới dự kiến:

```text
src/gpu/vector/
├── mod.rs
├── eligibility.rs       # quyết định GPU hay fallback
├── mesh.rs              # vertex/index và tessellation
├── cache.rs             # cache mesh theo revision/fingerprint
├── renderer.rs          # pipeline/buffer/draw
└── vector.wgsl          # vertex + fragment shader
```

Các file hiện có dự kiến tích hợp:

- `Cargo.toml`: dependency Lyon ở Phase 1.
- `src/gpu/mod.rs`: sở hữu `VectorRenderer`, gọi prepare/draw.
- `src/gpu/compositor.rs`: tạo kế hoạch các raster/vector run theo z-order.
- `src/app/render/composite.rs`: cung cấp scene/layer stack và fallback policy.
- `src/core/vector/object.rs`: truy cập geometry/style; tránh thêm GPU state.
- `src/core/vector/flatten.rs`: tiếp tục dùng cho raster fallback; không bắt buộc dùng cho Lyon.
- `src/app/path_display.rs`: chỉ vô hiệu hóa dần khi GPU renderer đã thay thế đúng.
- `src/gpu/compositor.wgsl`: không nhồi vector shader vào file này nếu không cần.

Không đặt cache GPU vào `Layer` hoặc `VectorObjectData`.

## 8. Revision và cache key

Mục tiêu cuối:

```text
GeometryKey:
    document_id
    layer_id
    geometry_revision/fingerprint
    stroke_geometry_revision/fingerprint

StyleKey:
    fill/stroke paint
    opacity

DrawKey:
    layer transform
    view transform
```

Quy tắc invalidation:

| Thay đổi | Rebuild mesh | Upload style | Đổi uniform |
|---|---:|---:|---:|
| Pan/zoom | Không | Không | Có |
| Move/rotate/scale affine | Không | Không | Có |
| Đổi màu fill | Không | Có | Có thể |
| Đổi gradient stops | Không | Có | Có thể |
| Đổi node/path | Có | Có thể | Có |
| Đổi stroke width/join/cap/dash | Có | Có | Có |

Phase đầu có thể dùng fingerprint ổn định thay vì thêm trường serialized revision. Không thay file format chỉ để tạo cache key.

## 9. Các phase triển khai

## Phase 0 — Baseline, instrumentation và fixture

### Mục tiêu

Đo pipeline hiện tại, tạo bộ tài liệu tái hiện cố định, và **trả lời hai câu hỏi quyết định** trước khi cam kết thêm Lyon và đụng vào renderer. Phase 0 không chỉ để ghi số; nó phải kết luận rõ hai điều dưới đây, và kết quả có thể làm thay đổi hoặc hoãn toàn bộ hướng GPU.

### Hai câu hỏi quyết định phải trả lời trước khi sang Phase 1

**Câu hỏi A — Đường tắt rẻ có đủ tốt không?**

Hai nguyên nhân lag đã biết của pipeline hiện tại là: (1) job bake cũ không bị hủy khi view/model tiếp tục đổi, và (2) một object thay đổi làm rasterize lại **cả run**. Trước khi xây GPU renderer, đo xem chỉ sửa hai điểm này lấy được bao nhiêu phần trăm lợi ích:

- Ước lượng (không cần hoàn thiện mức production) chi phí nếu **hủy job đang chạy** khi có job mới thay thế.
- Ước lượng chi phí nếu cache display theo **từng object** thay vì cả run, để một object đổi chỉ bake lại chính nó.
- Ghi lại CPU peak và thời gian tới-khi-nét-hoàn-tất **trước và sau** hai thay đổi giả định này, trên cùng fixture.

Kết luận cần có: *"Đường tắt rẻ — không đụng compositor, có thể đảo ngược — lấy lại được khoảng X% lợi ích."* Nếu X đủ lớn, cân nhắc **hoãn** cả bộ GPU renderer và chỉ làm đường tắt.

**Câu hỏi B — Compositor có nhận được một vector run không? (spike)**

Rủi ro lớn nhất của cả kiến trúc nằm ở việc chèn một texture vector vào giữa ping/pong accumulator đúng z-order mà **không** phải viết lại logic Mode A/B + partial composite. Làm một **spike tối thiểu**, tách rời, chỉ để trả lời khả thi/không:

- Chèn **một texture giả** (không cần Lyon, không cần vector thật) vào accumulator tại một layer index ở giữa stack.
- Blend nó đúng thứ tự với các raster run trên/dưới, trong cả Mode A và Mode B.
- Không cần đẹp, không cần AA, không giữ lại code này cho production.

Kết luận cần có: *"Có/không thể chèn một run vào accumulator mà không viết lại compositor; nếu có thì cần đụng những hàm nào."* Nếu spike cho thấy phải viết lại compositor → **dừng và báo cáo** trước khi làm POC Phase 1.

### Công việc

- Thêm timing debug cho:
  - `path_display` bake.
  - Số Path/object trong bake.
  - Tổng số pixel supersample.
  - Thời gian flatten/coverage/paint/tile split nếu tách đo được an toàn.
  - Số job bị supersede **và số job bị hủy giữa chừng** (hiện đang không hủy).
- Tạo fixture/generator:
  - 10 bông hoa.
  - 100, 300 và 500 vector layer.
  - Fill đơn sắc, có holes, stroke và gradient ở các fixture riêng.
  - **Text→Curves**: một fixture chữ đã chuyển thành Path (fill NonZero có holes) làm ca thử thực tế cho Phase 1.
- Ghi baseline:
  - Zoom 100%, 400%, 1600%.
  - Pan.
  - Move.
  - Scale/rotate.
  - Edit node.
  - CPU peak và thời gian từ lúc ngừng thao tác tới khi nét hoàn tất.
- Chạy hai thí nghiệm cho **Câu hỏi A** và spike cho **Câu hỏi B** ở trên.

### Metric so sánh (định nghĩa TRƯỚC khi có POC)

Từ Phase 1 trở đi mọi checkpoint đều hỏi "GPU có khớp rasterizer CPU không". Phải chốt cách so **ngay bây giờ**, vì GPU MSAA sẽ không bao giờ trùng pixel-khít với coverage rasterizer CPU ở vùng viền; không định nghĩa trước thì checkpoint sẽ thành cảm tính.

- **Nguồn chân lý (reference):** rasterizer CPU hiện tại (`src/core/vector/raster.rs`) render cùng scene ở cùng độ phân giải.
- **Không gian so:** so trong không gian màu tuyến tính (linear), không so thẳng byte sRGB, để tránh báo lệch giả ở viền.
- **Vùng đặc (interior, cách viền ≥ 1px):** sai số tối đa mỗi kênh ≤ một ngưỡng nhỏ đã chốt (ví dụ 2/255). Sai ở đây là sai thật.
- **Vùng viền (dải AA):** không đòi khớp pixel. Cho phép lệch trong một dải rộng cố định (ví dụ ±1px), đo bằng độ phủ (coverage) hoặc SSIM cục bộ thay vì diff tuyệt đối.
- **Ngưỡng cụ thể** (2/255, ±1px, …) được chốt bằng số trong Phase 0 và **giữ cố định** cho mọi phase sau; nếu về sau phải nới, phải ghi lý do.

Bẫy gamma cần nhớ: texture compositor là **sRGB**. Coverage AA phải được tính/resolve trong đúng không gian mà rasterizer hiện tại dùng, nếu không viền sẽ tối/sáng lệch → ra halo dù hình đúng.

### Thực tế CI và test (chốt kỳ vọng từ đầu)

Test render GPU offscreen **gần như không chạy được trên CI headless** nếu không có adapter phần mềm (WARP/lavapipe/llvmpipe), và ngân sách GitHub Actions đang eo hẹp (job macOS bill ×10). Vì vậy chốt trước:

- **CI (blocking):** chỉ giữ `naga` WGSL parse test + unit test (model→Lyon path, fill-rule mapping, eligibility/fallback, cache key/invalidation, mesh cache eviction) — những thứ chạy được không cần GPU.
- **Local/manual:** mọi snapshot render, so z-order, transform invariance, AA-ở-nhiều-zoom chạy **cục bộ** trên máy có GPU thật; kết quả ghi vào `docs/`. Không đặt chúng làm cổng CI blocking để tránh flaky và đốt phút Actions.
- Chỉ khi bật được software adapter ổn định trên CI mới cân nhắc chuyển một phần snapshot lên CI.

### File dự kiến

- `src/app/path_display.rs`
- `src/core/vector/raster.rs` chỉ nếu instrumentation không làm ảnh hưởng release.
- `src/gpu/compositor.rs` chỉ cho spike Câu hỏi B, tách sau một feature/debug flag và không giữ lại trong đường production.
- `tests/` hoặc một debug fixture generator riêng.
- Tài liệu kết quả benchmark mới trong `docs/`.

### Tiêu chí hoàn thành

- Có số liệu baseline tái chạy được.
- **Câu hỏi A có kết luận bằng số** (đường tắt rẻ lấy lại ~X% lợi ích).
- **Câu hỏi B có kết luận khả thi/không** cho việc chèn run vào accumulator, kèm danh sách hàm cần đụng.
- **Metric so sánh và các ngưỡng đã được chốt bằng số** trong `docs/`.
- Instrumentation tắt hoàn toàn trong release hoặc không tạo overhead đáng kể.
- Chưa thay đổi hình ảnh (spike và thí nghiệm không được giữ lại trong đường production).

### Điểm duyệt

Dừng và báo cáo: (1) số liệu baseline, (2) kết luận Câu hỏi A, (3) kết luận Câu hỏi B, (4) metric và ngưỡng đã chốt. Chỉ thêm Lyon và sang Phase 1 sau khi cả bốn được duyệt — hoặc chuyển hướng sang đường tắt rẻ nếu Câu hỏi A cho thấy điều đó đã đủ tốt.

## Phase 1 — GPU vector proof of concept độc lập

### Phạm vi hỗ trợ

- `VectorGeometry::Path`.
- Fill màu đặc.
- Fill rule NonZero và EvenOdd.
- Opacity object/layer bằng 1 trong slice đầu; opacity khác 1 có thể bổ sung sau khi blend đúng.
- Không stroke.
- Không gradient.
- Không mask/clip.
- Blend mode Normal.
- Không group effect/isolation.

### Công việc

- Thêm Lyon sau khi được duyệt.
- Chuyển Path hiện tại sang Lyon path.
- Tessellate fill thành vertex/index.
- Tạo GPU vertex/index buffer.
- Tạo pipeline `vector.wgsl`.
- Vẽ vào một offscreen target/test harness, chưa chen vào compositor production.
- So sánh output với rasterizer CPU hiện tại.

### Vertex tối thiểu

```rust
struct VectorVertex {
    position: [f32; 2],
}
```

Màu và transform dùng uniform trong slice đầu.

### Anti-alias

Proof of concept thử theo thứ tự:

1. MSAA 4× trên offscreen target.
2. Resolve sang texture sample-count 1.
3. Nếu chất lượng/chi phí không đạt, dừng báo cáo trước khi thử analytic AA.

Không đổi sample count của toàn bộ compositor ngay từ đầu.

### Test

- Rectangle, ellipse-converted path, cubic curve.
- Concave fill.
- Hole EvenOdd.
- Hole NonZero với winding đúng/ngược.
- Transform scale/rotate/flip.
- Snapshot ở zoom 25%, 100%, 800%, 6400%.
- Shader WGSL parse test bằng `naga`.

### Tiêu chí hoàn thành

- Hình cơ bản khớp reference trong tolerance đã định.
- Zoom/pan/transform không tessellate lại.
- Không có CPU display bake trong harness.
- Có báo cáo VRAM, draw cost và giới hạn phát hiện được.

### Điểm duyệt

Dừng báo cáo kết quả POC. Chưa tích hợp vào canvas nếu fill rule hoặc AA chưa đúng.

## Phase 2 — Tích hợp một vector run an toàn vào canvas

### Mục tiêu

Vẽ GPU vector xen kẽ raster đúng z-order trong trường hợp đơn giản.

### Scene planning

Chuyển stack thành run:

```text
RasterRun
GpuVectorRun
RasterRun
GpuVectorRun
...
```

Mỗi `GpuVectorRun` chỉ gồm layer liên tiếp đủ điều kiện. Layer không đủ điều kiện nằm trong `RasterRun`.

### Công việc

- Viết `eligibility.rs`.
- Compositor loại raster twin của GPU vector layer.
- Vẽ raster run vào accumulator.
- Vẽ GPU vector run tại đúng vị trí.
- Tiếp tục blend các run phía trên.
- Giữ fallback hoàn toàn cho:
  - mask,
  - clip,
  - group effect,
  - opacity/blend chưa hỗ trợ,
  - gradient/stroke.

### Rủi ro chính

Compositor hiện dùng ping/pong và một pass theo layer. GPU vector phải tham gia accumulator đúng semantics. Không được vẽ toàn bộ vector overlay lên cuối frame vì sẽ sai z-order.

### Test

- Vector trên raster.
- Vector dưới raster trong suốt.
- Raster–vector–raster xen kẽ.
- Nhiều vector liên tiếp.
- Active layer ở đầu/giữa/cuối.
- Bật/tắt visibility.
- Reorder layer.
- Undo/redo.
- Chuyển document/tab.

### Tiêu chí hoàn thành

- Không halo/double-edge.
- Không sai z-order.
- Feature không hỗ trợ fallback và nhìn giống trước.
- Cờ debug off trả về pipeline cũ.

### Điểm duyệt

Dừng để test thủ công trên fixture 100–500 layer trước khi thêm stroke/gradient.

## Phase 3 — Cache mesh và transform tương tác

### Mục tiêu

Loại bỏ rebuild CPU khi pan/zoom/move/rotate/scale.

### Công việc

- Cache vertex/index buffer theo geometry fingerprint.
- LRU/byte budget cho GPU mesh cache.
- View transform trong global uniform.
- Object/layer transform trong per-object uniform hoặc storage buffer.
- Move/rotate/scale chỉ cập nhật transform.
- Khi node edit:
  - chỉ layer bị sửa rebuild mesh,
  - mesh cũ có thể tiếp tục hiển thị trong lúc rebuild nếu không gây ghost,
  - hoặc rebuild đồng bộ nếu tessellation đủ nhỏ và có budget.
- Multi-selection:
  - dùng transform chung hoặc batch uniform,
  - không rasterize từng Path theo pointer event.

### Test/đo

- Đếm tessellation:
  - pan = 0,
  - zoom = 0 trong cùng mesh policy,
  - move = 0,
  - rotate/scale affine = 0,
  - sửa node một Path = 1 Path.
- Fixture 500 layer.
- Kiểm tra cache eviction và device recovery.

### Tiêu chí hoàn thành

- Không gọi display bake cho Path được GPU hỗ trợ.
- CPU nền không tăng do pan/zoom/move các Path đó.
- Không leak GPU buffer.

## Phase 4 — Stroke

### Thứ tự hỗ trợ

1. Solid stroke.
2. Width.
3. Butt/round/square cap.
4. Miter/bevel/round join.
5. Miter limit.
6. Dash và dash offset.
7. Vector brush variable width: phase riêng nếu Lyon stroke không biểu diễn đúng.

### Quy tắc

- Stroke feature nào chưa khớp thì cả layer fallback, không chỉ bỏ stroke.
- So sánh chặt với rasterizer hiện tại.

### Test

- Open/closed contours.
- Zero-length segment.
- Sharp cusp.
- Flip transform.
- Non-uniform scale.
- Dash qua điểm đóng contour.

## Phase 5 — Gradient và opacity/blend cơ bản

### Gradient

- Linear gradient.
- Radial gradient.
- Gradient transform.
- Stops và alpha.
- Stop update không rebuild mesh.

### Blend

- Layer/object opacity.
- Blend Normal trước.
- Các blend mode khác chỉ bật khi accumulator semantics được chứng minh đúng.

### Test

- Gradient dưới transform object và view.
- Alpha stops.
- Gradient xen kẽ raster.
- Color-space/CMYK preview phải có quyết định riêng; chưa hỗ trợ thì fallback.

## Phase 6 — Primitive Shape

`VectorGeometry::Primitive` có thể:

- Tessellate trực tiếp từ primitive để giữ hiệu quả; hoặc
- Chuyển sang Path tạm thời chỉ trong renderer.

Không thay model editable của Shape.

Test rectangle, ellipse, rounded rectangle, polygon/star và các primitive hiện có.

## Phase 7 — Mask, clipping, PowerClip và group

Đây là phase rủi ro cao, làm từng lát:

1. Vector clip đơn giản.
2. Raster mask trên vector.
3. Vector mask.
4. PowerClip.
5. Group opacity/isolation.
6. Blend/adjustment xuyên qua vector–raster runs.

Mỗi lát cần:

- định nghĩa semantics,
- reference image,
- fallback,
- test z-order.

Không bật GPU vector cho trường hợp chưa có test.

## Phase 8 — Thay thế `path_display`

Chỉ thực hiện khi các Path phổ biến đã được GPU renderer hỗ trợ.

### Công việc

- GPU-supported Path không tham gia `active_path_display`.
- Không spawn display bake cho chúng.
- Xóa suppression/recomposite tương ứng.
- Giữ `path_display` cho fallback layer trong thời gian chuyển tiếp.
- Chỉ xóa module cũ khi không còn consumer và test chứng minh đầy đủ.

### Tiêu chí hoàn thành

- Fixture vector phổ biến không chạy CPU supersample worker.
- CPU spike 50–60% do display AA biến mất đối với GPU-supported Path.

## Phase 9 — Export hybrid, dự án riêng

Không cần để hoàn thành hybrid canvas.

### `.iai`

Giữ nguyên vector model và raster layer. GPU mesh không serialize.

### PNG/JPEG/WebP

Giữ pipeline raster export hiện tại:

```text
vector rasterize ở resolution xuất + raster layers → flat image
```

Có thể tối ưu sau, không phụ thuộc canvas renderer.

### PDF

Mục tiêu sau:

```text
alternating raster/vector runs đúng z-order
```

- Path hỗ trợ: ghi native PDF path.
- Raster/photo/brush: nhúng image.
- Effect không biểu diễn được: rasterize cục bộ run/group.

### SVG

- Path/shape hỗ trợ: native SVG.
- Raster: embedded image.
- Effect/màu không tương thích: fallback cục bộ.

## 10. Chiến lược anti-alias

### Bản đầu

MSAA 4× cho target vector/run, resolve về texture sample count 1.

Lý do:

- Dễ xác minh.
- Ít thuật toán shader tùy biến.
- Phù hợp proof of concept.

### Cần đo

- VRAM của target MSAA.
- Bandwidth khi nhiều vector run.
- Số resolve/pass.
- Chất lượng thin stroke.

### Phương án sau nếu MSAA không đạt

- Geometry AA fringe.
- Analytic coverage shader.
- Hybrid: MSAA cho fill, analytic cho stroke.

Không làm analytic AA trước khi POC MSAA có số liệu.

## 11. Performance budget

Fixture 500 vector layer, viewport 1920×1080, release build:

- Pan: 0 tessellation.
- Zoom: 0 tessellation đối với geometry cache hợp lệ.
- Move/rotate/scale: 0 tessellation.
- Edit một Path: chỉ 1 Path rebuild.
- Không chạy `path_display` bake cho Path đủ điều kiện.
- CPU nền cho AA không duy trì ở 50–60%.
- Mục tiêu tương tác: 60 FPS nếu GPU cho phép; ngưỡng chấp nhận ban đầu 30 FPS ở fixture stress.
- Mesh cache có byte budget và eviction quan sát được.

Các con số frame time cuối cùng phải dựa trên baseline Phase 0, không tuyên bố đạt trước khi đo.

## 12. Correctness matrix

Mỗi feature có ba trạng thái:

| Feature | GPU native | Raster fallback | Chưa được phép |
|---|---:|---:|---:|
| Solid fill | Phase 1 | Có | Không |
| Fill rules/holes | Phase 1 | Có | Không |
| Basic stroke | Phase 4 | Có | Không |
| Dash | Phase 4 | Có | Không |
| Linear/radial gradient | Phase 5 | Có | Không |
| Normal opacity | Phase 5 | Có | Không |
| Advanced blend | Sau Phase 5 | Có | Không |
| Primitive Shape | Phase 6 | Có | Không |
| Mask/PowerClip/group | Phase 7 | Có | Không |
| Unsupported effect | Không | Có | Không |

“Chưa được phép” nghĩa là không được silently render thiếu/sai.

## 13. Bộ test bắt buộc

### Unit tests

- Vector model → Lyon path.
- Fill rule mapping.
- Eligibility/fallback.
- Cache key/invalidation.
- Mesh cache eviction.

### GPU/render tests

- WGSL parse.
- Offscreen render snapshots.
- Raster/vector z-order.
- Transform invariance.
- AA ở nhiều zoom.

### Integration tests

- Create/edit/undo/redo.
- Move/scale/rotate.
- Node edit.
- Layer visibility/reorder.
- Save/reopen `.iai`.
- Document/tab switch.
- Device/GPU state recreation nếu test harness cho phép.

### Manual tests

- 100/300/500 layer flower fixture.
- Zoom liên tục.
- Pan trong lúc zoom.
- Move nhiều selection.
- CMYK document.
- Mask/group/blend documents.
- So sánh bật/tắt `gpu_vector_canvas`.

## 14. Rủi ro và cách khóa rủi ro

### Sai z-order

Khóa bằng run planner và test raster–vector–raster; không dùng top overlay.

### Halo/double-edge

Khóa bằng quy tắc một Path chỉ có một representation trong một frame.

### Fill/stroke khác rasterizer hiện tại

Khóa bằng snapshot/reference tests và fallback.

### MSAA tốn VRAM/bandwidth

Chỉ MSAA target cần thiết; đo trước khi áp toàn compositor.

### Cache stale

Key theo document/layer/fingerprint; clear khi document close, device loss và renderer recreation.

### CMYK/colour management

GPU path chưa chứng minh color pipeline thì fallback. Không giả định RGBA preview là đủ.

### Scope phình quá lớn

Mỗi phase có điểm dừng bắt buộc. Không gộp Phase 1–4 thành một PR.

## 15. Thứ tự commit/PR đề xuất

1. `perf(vector): add hybrid-canvas baseline fixtures and telemetry`
2. `feat(vector-gpu): add offscreen solid-fill tessellation poc`
3. `feat(vector-gpu): composite eligible vector runs with raster fallback`
4. `perf(vector-gpu): cache meshes and make transforms uniform-only`
5. `feat(vector-gpu): support path strokes`
6. `feat(vector-gpu): support gradients and normal opacity`
7. `feat(vector-gpu): support parametric vector primitives`
8. Các commit mask/clip/group tách riêng.
9. `perf(vector): retire supersampled display bake for gpu-native paths`

Mỗi commit phải build/test độc lập và không chứa thay đổi export không liên quan.

## 16. Điểm bắt đầu được khuyến nghị

Cuộc hội thoại code mới bắt đầu từ Phase 0 rồi tự triển khai liên tục toàn bộ
các phase theo đúng thứ tự trong tài liệu. Các điểm duyệt trong kế hoạch trở
thành checkpoint kỹ thuật nội bộ: phải chạy test, ghi nhận kết quả và chỉ đi
tiếp khi phase hiện tại đạt tiêu chí hoặc đã có fallback an toàn.

Không cần chờ người dùng duyệt giữa các phase. Chỉ dừng sớm nếu gặp blocker
thật sự không thể giải quyết an toàn, cần quyền mới, hoặc phát hiện kiến trúc
trong kế hoạch chắc chắn gây sai dữ liệu/hình ảnh mà không có fallback.

## 17. Prompt bàn giao sang cuộc hội thoại mới

Sao chép nguyên đoạn sau:

```text
Hãy đọc toàn bộ file
KE_HOACH_CANVAS_HYBRID_VECTOR_RASTER_2026-07-30.md
trong repo iAi. Đây là nguồn yêu cầu chính; tôi đã tự chỉnh sửa tài liệu nên
hãy đọc bản hiện tại trên filesystem, không dựa vào bản cũ trong hội thoại.

Hãy triển khai LIÊN TỤC từ Phase 0 đến hết tất cả phase trong kế hoạch, theo
đúng thứ tự. Không dừng để chờ tôi duyệt hoặc test giữa các phase vì tôi sẽ
không có mặt. Mỗi điểm duyệt trong tài liệu là checkpoint kỹ thuật nội bộ:
hãy tự chạy test, benchmark và kiểm tra fallback; nếu đạt thì chủ động làm
phase tiếp theo.

Phase 0 vẫn phải hoàn thành đầy đủ baseline, fixture 100/300/500 vector layer
(kể cả Text→Curves), Câu hỏi A, Câu hỏi B và metric so sánh rasterizer CPU như
tài liệu yêu cầu. Ghi kết quả vào docs rồi tiếp tục Phase 1, không chờ phản hồi.
Nếu đường tắt rẻ ở Câu hỏi A có lợi, có thể triển khai nó như một lớp tối ưu
bổ sung, nhưng không được dùng nó làm lý do bỏ dở canvas hybrid GPU.

Giữ nguyên nguyên tắc an toàn:
- Trước khi sửa, kiểm tra worktree; bảo toàn mọi thay đổi có sẵn của tôi.
- Không silently render sai: feature chưa hỗ trợ phải raster fallback.
- Không bỏ path_display trước khi GPU path tương ứng đạt tiêu chí thay thế.
- Không thay đổi export hoặc format .iai ngoài phase đã nêu.
- Không đưa snapshot GPU dễ flaky thành cổng CI; CI blocking chỉ dùng các test
  ổn định như unit test và naga/WGSL parse. Snapshot GPU chạy local/manual và
  ghi kết quả vào docs.
- Không tự commit, push, release hoặc sửa file ngoài phạm vi nếu không cần.

Hãy tự xử lý lỗi build/test và tiếp tục cho tới khi toàn bộ kế hoạch hoàn tất.
Chỉ dừng sớm nếu có blocker thật sự không thể giải quyết an toàn, thiếu quyền
bắt buộc, hoặc phát hiện thiết kế chắc chắn làm sai/mất dữ liệu mà không thể
fallback. Nếu một feature nâng cao chưa thể GPU-native một cách đúng đắn, giữ
raster fallback, ghi rõ giới hạn và tiếp tục các phần độc lập còn lại; không
được giả vờ rằng feature đó đã hoàn thành.

Sau khi hoàn thành toàn bộ công việc:
1. Chạy toàn bộ test ổn định và các benchmark/fixture liên quan.
2. Ghi báo cáo kiến trúc, số liệu trước/sau, fallback còn lại và test manual
   vào docs.
3. Trả cho tôi một DANH SÁCH TEST THỦ CÔNG tuần tự, chia theo từng tính năng,
   có thao tác, kết quả mong đợi và dấu hiệu lỗi, để khi quay lại tôi có thể
   test lần lượt từ cơ bản đến nâng cao.
4. Tóm tắt file đã sửa, test đã chạy, kết quả, giới hạn còn lại và đường dẫn
   tới các tài liệu/báo cáo.
```
