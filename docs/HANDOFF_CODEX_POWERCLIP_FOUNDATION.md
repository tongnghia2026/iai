# Giao ca Codex → Claude: Path display và nền Clipping/PowerClip

Ngày cập nhật: 2026-07-23  
Nhánh: `feat/vector-core-foundation`  
HEAD: `005a01b`

## 1. Trạng thái workspace

- Working tree sạch đối với file tracked.
- Hai mục untracked của người dùng, **không được sửa/xóa/commit**:
  - `KE_HOACH_PHAT_TRIEN_VECTOR_IAI.txt`
  - `dist/`
- Các commit trong ca này đều chỉ ở local, chưa push:
  - `ed2289f feat(vector): crisp active Path display at canvas zoom`
  - `fff2fbd fix(vector): tile crisp Path preview within GPU limits`
  - `005a01b feat(vector): add PowerClip attachment foundation`

## 2. Path display theo độ phân giải màn hình

### Đã làm

- `src/core/vector/display.rs`
  - Chọn zoom bucket 2×/4×/8×/16×.
  - Clone `VectorObjectData`, scale transform/stroke rồi dùng lại rasterizer hiện có.
  - Raster chỉ là display derivative, không thay `Layer.tiles`, export hay model.
- `src/app/path_display.rs`
  - Cache theo document, layer, object, offset và zoom bucket.
  - Chỉ dùng cho Path active ở Move/Node, và Pen sau khi Enter đã commit.
  - Không rebuild khi đang node drag/path transform.
  - Conservative fallback nếu có opacity, mask, blend/group effect hoặc painted
    layer nằm phía trên.
- `src/ui/mod.rs`
  - Upload raster thành texture overlay, clip theo canvas viewport.
  - Overlay nằm ở canvas-tool background order, không đè dialog/panel.

### Crash đã gặp và đã sửa

User test Path lớn sinh texture `2247×2256`; GPU adapter chỉ cho texture tối đa
`2048`, egui/wgpu panic tại `Context::load_texture`.

Fix `fff2fbd`:

- Chia display raster thành tile tối đa `1024×1024`.
- Mỗi tile là texture riêng, cùng ghép về một vị trí canvas.
- Regression test dùng đúng kích thước crash `2247×2256`, kết quả 9 tile.
- Sau Pen Enter, Path crisp hiện ngay, không cần đổi qua Move.

### Nợ còn lại được user xác nhận

- PDF native-vector đã hết răng cưa.
- Canvas vẫn còn răng cưa nhìn thấy bằng mắt dù display raster đã cải thiện.
- User đồng ý để fix sau.
- Không nên coi hướng overlay raster là vector screen renderer hoàn chỉnh. Bản
  atlas document-resolution vẫn nằm bên dưới, còn display raster là lớp phủ tạm.
- Hướng dài hạn vẫn là tessellation/GPU vector thật; nếu thêm dependency như
  `lyon_tessellation` phải báo user trước theo quy ước kế hoạch.

## 3. Bước 6 — nền Clipping/PowerClip đã hoàn thành

Commit: `005a01b`

Đây là phạm vi nền T6.1–T6.3, **chưa phải UI PowerClip hoàn chỉnh**.

### T6.1 — relation và invariant

- `Layer` có field additive:

  ```rust
  pub clip_parent_id: Option<u32>
  ```

- `parent_id` tiếp tục chỉ mang nghĩa Group membership.
- Logic mới ở `src/core/canvas/clip_ops.rs`:
  - `can_attach_clipped_child`
  - `attach_clipped_child`
  - `release_clipped_child`
  - `create_clipped_pixel_child`
  - `repair_clip_relations`
- Chống self-link, cycle, missing parent và Group làm clip frame/content.
- Reorder giữ relation theo ID.
- Xóa frame tự release child.
- Paste chỉ giữ clipping nếu frame được copy trong cùng block và remap ID.
- Duplicate Group remap clip child sang frame copy, không trỏ về frame gốc.
- Pixel child mới dùng cùng Group parent với frame nhưng quan hệ clipping vẫn nằm
  riêng trong `clip_parent_id`.

### T6.2 — command qua gateway

File mới: `src/core/command_clip.rs`.

- `CreateClippedPixelChild`
- `ReleaseClippedChild`
- `CreateOrAttachRasterMask`

Create child/mask nhận optional initial `TileMap`, để auto-create attachment và
nét Brush/Eraser đầu tiên có thể là một undo transaction. Command chạy qua
`Canvas::execute`; fail không vào history.

Hiện các command mới đã có model tests nhưng **chưa được nối vào Brush/Eraser UI**.

### T6.3 — `.iai` persistence

- Manifest layer có optional key `clip_parent`, lưu theo layer index giống
  `parent`.
- Có Path hoặc clipping relation thì file được stamp v4.
- File cũ không có key mặc định `None`.
- Load xong gọi `repair_clip_relations()` để bỏ relation corrupt/cyclic/dangling.
- PDF project `.iai` cũng stamp v4 nếu bất kỳ page nào có clipping.

## 4. Kiểm thử tại thời điểm giao

Đã chạy:

```text
cargo fmt --check
cargo check
cargo test --lib
```

Kết quả:

```text
840 tests total
836 passed
0 failed
4 ignored
```

Hai warning cũ, không thuộc ca này:

- `src/ui/library.rs:220`: f32 literal fallback.
- `src/ui/library.rs:222`: f32 literal fallback.

Test mới khóa:

- Zoom bucket và display raster scale.
- Texture tiling cho kích thước crash 2247×2256.
- Clip relation độc lập Group và an toàn qua reorder.
- Cycle reject; delete frame release child.
- Paste/duplicate Group remap clip IDs.
- Create child + initial pixels undo/redo nguyên tử.
- Mask create/attach và release qua gateway.
- `.iai` PowerClip relation round-trip và stamp v4.

## 5. Việc nên làm tiếp theo

Theo `KE_HOACH_PHAT_TRIEN_VECTOR_IAI.txt`, Bước 6 nền đã đủ mảnh T6.1–T6.3. Việc
tiếp theo nên là audit **cổng Foundation Freeze**, không tuyên bố freeze ngay:

1. Chạy M1 end-to-end thủ công cả RGB và CMYK:
   Pen → Path → Node edit → Fill/Outline → save/reopen `.iai` → PDF.
2. Rà đủ round-trip/property tests cho các contract nền.
3. Giả lập trên giấy và ghi kết luận contract cho:
   - Polygon/Star
   - Boolean
   - Text → Curves
   - Multi-page/Artboard
4. Rà ADR/UX-0 còn thiếu, đặc biệt target semantics:
   Group member vs clipped content vs Raster Mask.
5. Chỉ khi gate đủ mới tuyên bố Foundation Freeze.

Sau freeze, lát PowerClip tính năng đầu tiên hợp lý:

- Nối `CreateClippedPixelChild` vào Pixel Brush khi target là vector.
- Nối `CreateOrAttachRasterMask` vào Eraser.
- CPU/GPU compositor thực sự nhân alpha content với clip-frame alpha.
- Layers panel hiển thị nesting/target rõ ràng.
- Select/Edit Contents, Release/Extract, Lock Contents.

Không được báo PowerClip “dùng được” trước khi compositor clipping và UI targeting
được nối; commit hiện tại mới khóa model/command/persistence foundation.

## 6. Quy ước tiếp tục

- Chỉ commit local, không push trừ khi user yêu cầu.
- Trước commit: `cargo fmt` và `cargo test --lib`.
- Không đụng hai mục untracked của user.
- Không tự thêm dependency cho tessellation/Boolean; phải trình bày tác động và
  hỏi user trước.
- Giữ kỷ luật thêm module/file mới cho logic nặng; không nhồi thêm vào
  `layer.rs`, `ui/mod.rs` hoặc `iai.rs` nếu có thể tách.
