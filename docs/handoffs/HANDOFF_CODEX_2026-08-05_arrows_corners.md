# Bàn giao cho Codex — Mũi tên + Kiểu góc chữ nhật (HOÀN TẤT 2026-08-05)

> **Trạng thái cuối: HOÀN TẤT A/B/C.** Người dùng đã GUI-test và xác nhận OK.
> HEAD bàn giao cho Claude: **`41feb53`**. Các commit local: Stage A `695fa32`,
> Stage B `5df8750`, Stage C + sửa cache zoom `ecf0b8e`, UI gọn `0f0867a`, icon góc
> kiểu Corel `0f3148c`, sửa Convert to Curves `41feb53`. Chưa push.

Bản giao ca để **Codex code tiếp** hai tính năng vector cho iAi. Người dùng (nghề
in ấn/quảng cáo) chốt làm: (1) **kiểu góc hình chữ nhật kiểu CorelDRAW** — bo tròn,
tròn lõm ngược (scallop), vát chéo (chamfer); (2) **công cụ Mũi tên / Đường nối**
có **đầu mũi tên** và **đường nối bẻ góc vuông** để vẽ sơ đồ phòng ban / gia phả.

Đọc file này + đối chiếu code hiện tại (tin filesystem, không tin trí nhớ hội thoại
cũ). Bản đồ code gốc do 6 agent lập nằm ở scratchpad tạm (đã hết) — mọi điểm móc
`file:line` cần thiết đã chép vào đây.

Nhánh: `feat/vector-core-foundation`.

---

## 1. Đang ở đâu

- **GIAI ĐOẠN A (kiểu góc chữ nhật) XONG** — commit local **`695fa32`** (CHƯA PUSH).
  `cargo check` sạch; `from_shape` 12 test pass (gồm 4 test góc mới), `shape_ops`
  6 pass, `formats::iai` 27 pass. Đây là tính năng độc lập, dùng được ngay.
- **GIAI ĐOẠN B (đầu mũi tên) — XONG** — commit local **`5df8750`**.
- **GIAI ĐOẠN C (công cụ Mũi tên/Đường nối) — XONG** — commit local **`ecf0b8e`**.
- **GUI TEST A/B/C — OK**, người dùng xác nhận ngày 2026-08-05, gồm lỗi hiển thị
  khi zoom 9× đã hết sau khi bổ sung arrow style vào GPU geometry fingerprint.
- Trước đó: origin ở `1b3d5a5` (đã push 2026-08-05). Từ `1b3d5a5` → `41feb53` là
  các commit local chưa push (dọn docs + Stage A/B/C + UI polish + bugfix). **Đừng push
  cho tới khi người dùng bảo** (quy tắc Actions budget).

### Cây làm việc: code SẠCH (đã commit Stage A/B/C). Build và test được.

---

## 2. Nguyên tắc kiến trúc (bắt buộc tuân theo)

- **KHÔNG tách file.** Code vector mới vào module có sẵn; sửa file lớn = 1 variant
  + 1 dispatch + 1 dòng đăng ký. Công cụ mới = 1 file `src/tools/*.rs` + 1 file
  `src/app/*_ops.rs` (mirror cặp `vector_brush.rs` / `vector_brush_ops.rs`).
- **Cộng thêm, không sửa nền.** Foundation đã freeze (`docs/FOUNDATION_FREEZE.md`).
- **ĐỪNG sửa `.rs` bằng Get/Set-Content** (phá UTF-8). Dùng editor thường.
- Trước push: `cargo fmt --check && cargo test --locked --lib` (CI có fmt-check chặn).
- Test GPU cần `--test-threads=1`.

---

## 3. Giai đoạn A đã làm gì (tham chiếu, đã xong)

Mẫu để lặp lại cho B/C. Các file đã sửa ở `695fa32`:

- `core/vector/from_shape.rs`: thêm `enum RectCorner {Round,Scallop,Chamfer}`
  (+`from_u8`/`to_u8`/`label`, Default=Round) và
  `rect_path_corners(x0,y0,x1,y1, radii:[f32;4], corners:[RectCorner;4]) -> PathData`.
  **`rect_path` GIỮ NGUYÊN** làm đường nhanh uniform-Round. Chamfer = bỏ 2 handle →
  đoạn thẳng chéo; Scallop = đẩy handle vuông góc vào trong → cung lõm quanh đỉnh V.
- `core/shape.rs`: thêm field `corner_type: RectCorner` vào `ShapeData`; thread qua
  `from_canvas_span_with_style` (literal), `rebuilt` (carry-over), `to_vector_object`
  (Rectangle: Round→`rect_path`, khác→`rect_path_corners`). Thêm helper
  `sdf_rect_corners(...)` dùng CHUNG cho `render()` và `coverage_parts` — Chamfer =
  `sdf_polygon` trên 8 đỉnh cắt; Scallop = `max(sdf_box, maxᵢ(r − dist(p,Vᵢ)))`
  (box trừ 4 đĩa góc). Cả hai 1-Lipschitz → giữ tối ưu skip-run của `render()`.
- `tools/shape.rs`: field `corner_type: RectCorner` + default Round.
- `app/shape_ops.rs`: thread `corner_type` trong `begin_new_shape`,
  `styled_shape_target` (retro-apply), và overlay preview (`active_shape_overlay`).
- `formats/iai.rs`: serde thêm khóa `"corner_type"` u8 (đọc `.unwrap_or(0)` → cũ =
  Round). **PDF KHÔNG cần đổi** (corner chỉ đổi PathData; `append_pdf_path` tự phát
  `l`/`c` theo handle).
- UI: `ui/viewmodel.rs` (`shape_corner_type: u8` + default), `ui/intent.rs`
  (`set_shape_corner_type: Option<u8>`), `ui/topoptions.rs` (`shape_options`: combo
  "Corner:" trước ô Radius, trong nhánh `if kind == 0`), `app/actions/ui_data.rs`
  (populate), `app/actions/ui_tools.rs` (handler → `update_selected_shape_style`).
- Literal `ShapeData {…}` khác đã thêm field: `app/powerclip_ops.rs`,
  `app/vector_boolean.rs` (test helpers).

### Giai đoạn A: người dùng đã GUI-test OK. Checklist đã kiểm:
- Vẽ chữ nhật, đổi combo Corner + Radius → thấy bo tròn / lõm / vát trên canvas,
  cả lúc đang chọn (active raster) lẫn khi bỏ chọn (GPU vector).
- Convert to Curves giữ đúng hình. Xuất PDF sắc nét đúng kiểu góc. Save/mở `.iai`.

---

## 4. GIAI ĐOẠN B — Đầu mũi tên (arrowhead) trên nét — XONG (`5df8750`)

Đã hoàn tất model, hình học CPU/GPU dùng chung, raster bounds, `.iai`, PDF và UI
Start/End/Size. File cũ mặc định `None`. Arrowhead chỉ sinh trên contour hở và sinh từ
centerline chưa dash. Test winding, tangent suy biến, serde và toàn bộ lib đều pass.

**Mục tiêu:** thêm đầu mũi tên đầu/cuối cho nét (Triangle/Stealth/Circle/Diamond +
cỡ), hiện trên canvas (CPU+GPU) và xuất PDF. Cho **path hở** (line/connector). Model
= field cộng thêm trên `StrokeStyle`; hình học sinh trong `stroke.rs` (nơi CPU+GPU
dùng chung) nên parity tự động.

### B1. Model — `src/core/vector/style.rs`
`StrokeStyle` hiện = `{width, cap:LineCap, join:LineJoin, miter_limit, dash}` (dòng
~199-217), có `Default` (207-217). `LineCap`(148), `LineJoin`(157), `Paint`(167).
Thêm (mặc định None → mọi object hiện tại byte-identical):
```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ArrowHead { #[default] None, Triangle, Stealth, Circle, Diamond }

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ArrowStyle { pub kind: ArrowHead, pub size: f32 } // size = bội của bề rộng nét
impl Default for ArrowStyle { fn default() -> Self { Self { kind: ArrowHead::None, size: 3.0 } } }
```
Thêm `pub start_arrow: ArrowStyle` + `pub end_arrow: ArrowStyle` vào `StrokeStyle`
(+ vào `Default`, + `validate` (~285-300) kiểm `size` hữu hạn & ≥0). Thêm `to_u8`/
`from_u8` cho `ArrowHead` (mirror `LineCap`).

### B2. Hình học — `src/core/vector/stroke.rs` (CHỖ DÙNG CHUNG CPU+GPU)
`stroke_outline_contours(polylines,&closed,half,cap,join,miter,tol) -> Vec<Vec<Point>>`
(31) dựng outline nét (fill NonZero, mọi ring qua `push_ring` để dương hướng).
`add_cap(end, neighbour, ...)` (193-237) đã tính **tiếp tuyến đầu mút**:
`u = norm(sub(end, neighbour)); nm = perp(u)`. Helper có sẵn: `norm`,`perp`(329),
`along`(333),`sub`,`push_ring`(291),`circle`(270).

Thêm hàm anh em:
```rust
pub fn arrowhead_contours(
    polylines: &[Vec<Point>], closed: &[bool], half: f32,
    start: ArrowStyle, end: ArrowStyle, tol: f32,
) -> Vec<Vec<Point>>
```
Cho mỗi contour `!closed && len>=2`: tiếp tuyến start = `norm(sub(pl[0],pl[1]))`,
end = `norm(sub(pl[n-1],pl[n-2]))`. `L = size*(2*half)`. Dựng đa giác đầu mũi tên ở
điểm mút `p` theo `(u, perp(u))`, phát qua `push_ring`:
- Triangle: đỉnh `along(p,u,L)`, 2 chân `p ± perp*(k*half)` (k≈2–3).
- Stealth: như Triangle + khấc lõm sau ở `along(p,u,L*0.4)` (ring 4 điểm).
- Diamond: đỉnh, 2 bên ở `along(p,u,L*0.5)±perp`, đuôi ở `p`.
- Circle: `circle(along(p,u,L*0.5), L*0.5, tol)`.
Bỏ qua `ArrowHead::None`. **Chỉ contour HỞ.**

### B3. CPU fill — `src/core/vector/raster.rs`
Nhánh stroke (~263-290) gọi `stroke_outline_contours` rồi `fill_coverage`. Thêm:
`outline.extend(arrowhead_contours(&flat_undashed_lines, &closed, half, ss.start_arrow, ss.end_arrow, FLATTEN_TOL))`
**TRƯỚC** `fill_coverage`. **BẪY QUAN TRỌNG:** sinh arrowhead từ polyline **CHƯA cắt
nét đứt** (`local`/`flat` + cờ `closed` gốc), KHÔNG từ `stroke_lines`/`dashed`
(nếu không → mỗi đoạn đứt mọc 1 mũi tên). **`raster_layout` (126-142)** phải nới
`cap_join_extra` thêm tầm với mũi tên (`~max(start.size,end.size)*width`) nếu không
tip bị cắt ở mép raster (lỗi hình dễ gặp nhất).

### B4. GPU fill — `src/gpu/vector/mesh.rs`
`stroke_outline_lyon_path` (59-90) gọi `stroke_outline_contours` rồi biến mỗi ring
thành subpath lyon kín. Thêm `contours.extend(arrowhead_contours(&lines, &line_closed,
ss.width*0.5, ss.start_arrow, ss.end_arrow, tol))` TRƯỚC vòng ring→subpath. NonZero
tessellator tự tô. → parity CPU/GPU tự động (như cap/join).

### B5. Serde `.iai` — `src/formats/iai_vector.rs`
`stroke_style_to_json` (98-107) / `json_to_stroke_style` (302-329, **literal đủ field,
KHÔNG `..default()`** → phải sửa tay). Thêm `start_arrow`/`end_arrow` (kind u8 + size)
đọc `.unwrap_or(default)`. Encoder này DÙNG CHUNG cho cả Path lẫn Shape → Line-shape
connector tự round-trip. Thêm helper u8 cạnh `cap_u8`/`join_u8` (154-168). **Cảnh báo
compile:** 2 literal test (`iai_vector.rs` ~437-443 và ~612-618) cũng liệt kê đủ field
→ phải cập nhật.

### B6. PDF export — `src/core/print.rs` (ĐỪNG bỏ — người dùng in PDF)
PDF phát native stroke ops (không dùng outline baked) → arrowhead KHÔNG tự vào PDF.
`collect_pdf_vectors` (~537-748): sau khi push object line (~727-740), nếu là path HỞ
có arrow≠None, tính tam giác/đa giác đầu mũi tên từ **anchor mút + tiếp tuyến đoạn
đầu/cuối trong không gian canvas-pixel** (`emitted_path` post-transform, không phải
object-local), cỡ `arrow_size*stroke_width_px`, push thêm 1 `PdfVectorObject` chỉ-fill
(`fill: <stroke rgb>`, `stroke: None`, `even_odd:false`). Đặt TRONG khối đã-promote
(cùng cổng opaque/eligibility — nếu không, connector mờ sẽ mũi tên vector còn nét
raster → lệch).

### B7. UI — `src/ui/topoptions.rs` + `src/app/path_style.rs` + intent/viewmodel
Đầu mũi tên là thuộc tính PATH-STYLE (mỗi lựa chọn = 1 undo, mẫu cap/join).
- `PathStyleData` (`src/ui/mod.rs` ~159-187, `#[derive(Clone,Copy)]`): thêm
  `arrow_start:u8, arrow_end:u8, arrow_size:f32`.
- Populate ở `App::active_path_style_vm` (`path_style.rs` ~205-283, cạnh cap:272/join:277).
- `ToolActions` (`intent.rs` ~333): `set_path_arrow_start/end:Option<u8>`,
  `set_path_arrow_size:Option<f32>`.
- `topoptions.rs`: 2 combo (mẫu compare-after-combo cap/join) + 1 scrub cỡ (mẫu
  stroke-width: `.changed()`→set, `drag_stopped()||lost_focus()`→`commit_path_style=true`)
  trong `path_style_quick` (~sau join) và/hoặc `path_style_options`. **id_salt riêng
  từng surface** (`quick_arrow_start` vs `path_arrow_start` vs `appearance_arrow_start`).
- Dispatch `ui_tools.rs` (~622-627): `App::path_set_arrow_start/end` (mirror
  `path_set_cap`/`path_set_join` ~757-776 → `commit_path_style_change`, 1 undo) và
  `path_set_arrow_size` (mirror `path_set_stroke_width` ~730 → preview live + commit).
- (Tuỳ) mirror vào panel Appearance `src/ui/panels.rs` (686-722).

### B8. Test B
- `stroke.rs`: ring arrowhead vượt quá điểm mút theo tiếp tuyến; None = geometry
  không đổi; **mọi ring qua `push_ring` (dương hướng)** — ring ngược sẽ ĐỤC LỖ thay
  vì thêm mũi tên (bẫy winding).
- `mesh.rs`: Triangle-end thêm đỉnh so với None (mẫu `cap_style_changes_gpu_stroke_geometry`).
- Round-trip serde arrow (mở rộng property test `iai_vector.rs` ~559).
- Tiếp tuyến suy biến: nếu 2 điểm cuối trùng → `norm`=None; đi vào trong tới điểm
  không trùng đầu tiên (nếu không mũi tên biến mất).

---

## 5. GIAI ĐOẠN C — Công cụ Mũi tên / Đường nối — XONG (`ecf0b8e`)

Đã có nút toolbar, công cụ kéo-thả một lần, live overlay, options Width/Arrow Head/Route,
commit Path qua gateway, selection, undo/redo và CMYK reconcile. Bốn route đã có:
Straight, Elbow H-V, Elbow V-H, Elbow Center. Không gán phím tắt để tránh xung đột.

Sửa bổ sung trong cùng commit: GPU `geometry_fingerprint` hash cả kind/size của đầu/cuối
mũi tên. Đây là nguyên nhân mesh cũ chỉ thay đổi khi zoom vượt bucket 8× sang 16× (thấy
rõ tại zoom 9×). Raster bounds cũng không còn nới dư khi arrow kind là `None`.

**Mục tiêu:** kéo A→B ra 1 nét (mũi tên) với `end_arrow` (dùng B). Có chế độ
**bẻ góc vuông (elbow)** cho sơ đồ phòng ban/gia phả. Làm **1-drag** (không dùng hệ
modal đa-click) — routing elbow chỉ là cách sinh path giữa 2 điểm kéo.

### C1. Hình học đường nối — `src/core/vector/from_shape.rs`
Thêm `elbow_connector_path(sx,sy,ex,ey, mode) -> PathData`: 1 Contour HỞ gồm các
`Node::sharp` waypoint vuông góc. Kiểu: `Straight` (2 điểm, = `line_path`), `ElbowHV`
(ngang rồi dọc: waypoint (ex,sy)), `ElbowVH` (dọc rồi ngang: (sx,ey)), `ElbowCenter`
(chữ Z: (mx,sy)-(mx,ey) với mx=(sx+ex)/2 — kiểu org-chart). Giữ thuần toạ độ.

### C2. Công cụ — checklist wiring ĐẦY ĐỦ (mẫu VectorBrush)
Enum bắt buộc: sửa 4 chỗ, build sẽ báo đúng chỗ thiếu.
1. `src/extension/tool.rs`: thêm `Arrow,` vào `enum ToolId` (sau `VectorBrush,`, ~L41).
2. `src/tools/mod.rs` (compiler ép 3 match): `pub mod arrow;` (~L31); field
   `arrow: arrow::ArrowTool,` (~L194) + `new()` init (~L236); arm cho `name()` (~L98),
   `tool_dyn` (~L296), `tool_dyn_mut` (~L330); thêm `| ToolId::Arrow` vào
   `allowed_in_cmyk` (~L133, vì commit Path qua gateway); accessor `arrow()`/`arrow_mut()`
   (~L551).
3. `src/tools/arrow.rs` (FILE MỚI, copy `vector_brush.rs`): `struct ArrowTool` giữ
   start/end điểm + `width` + `end_arrow`(u8) + `route`(u8) + màu; `impl Tool`
   (`tool_id()->ToolId::Arrow`, `id()->"arrow"`, on_press/on_drag/on_release bắt drag);
   `take_arrow_object(fg) -> Option<VectorObjectData>` dựng qua `elbow_connector_path`
   + `VectorStyle` (fill None, stroke Solid màu, `stroke_style.end_arrow=…`).
4. `src/app/arrow_ops.rs` (FILE MỚI, copy `vector_brush_ops.rs`): `commit_arrow()` =
   `canvas.execute(Box::new(CreatePathLayer::new(object,"Arrow")), ChangeKind::LayerStructure)`
   → chọn layer mới → `reconcile_path_ink()` → `layer_revision+=1` →
   `apply_canvas_event(CanvasEvent::LayerStructureChanged)`. Đăng ký `pub mod arrow_ops;`
   ở `src/app/mod.rs` (~L29).
5. `src/app/input/pointer.rs`: nhánh release sau khối VectorBrush (~L765): gọi
   `on_release` rồi `self.commit_arrow(); self.edit.input.painting=false;`. Preview khi
   kéo: arm `ToolId::Arrow` ở ~L1305 (mẫu VectorBrush on_drag).
6. `src/ui/toolbar.rs`: `const ARROW_GROUP: &[(ToolId,&str,&str)] = &[(ToolId::Arrow,
   ph::ARROW_UP_RIGHT, "Arrow / Connector")];` (~sau L47) rồi thêm `ARROW_GROUP,` vào
   `COMMON_BOTTOM_GROUPS` (~L103-111, luôn hiện). (Xét thêm vào test creation-tools
   toolbar.rs:936.)
7. Options bar (nếu có tuỳ chọn): `intent.rs` `set_arrow_*`; `viewmodel.rs` `arrow_*`
   + default; `ui_data.rs` populate + gate preview theo `active_id()==Arrow`;
   `topoptions.rs` dispatch `ToolId::Arrow => arrow_options(...)` + hàm `arrow_options`
   (width, end_arrow combo, route combo, màu). `ui_tools.rs` tiêu thụ `set_arrow_*`.
8. (Tuỳ) `keyboard.rs`: phím tắt — **hầu hết chữ cái đã bị chiếm** (xem match dài
   L127-1117); ưu tiên KHÔNG phím tắt (toolbar-only) hoặc chọn phím trống. Nếu
   1-drag thì KHÔNG cần đụng modal Escape/Enter.
9. (Tuỳ) `state.rs` sync_cursor (~L2782): `ToolId::Arrow => Crosshair` trước `_ =>`.
10. (Tuỳ) `src/ui/mod.rs` (~1150): `arrow_overlay` vẽ rubber-band (mẫu
    `vector_brush_overlay`).

### Bẫy C
- allowed_in_cmyk là `matches!` mặc-định-false → QUÊN = công cụ bị chặn ở doc CMYK.
- Commit PHẢI qua gateway (`CreatePathLayer` + `LayerStructureChanged`), kèm
  `reconcile_path_ink()` + `layer_revision+=1`, nếu không hỏng undo/atlas/CMYK.
- Compiler chỉ ép 4 chỗ (enum + name/tool_dyn/tool_dyn_mut); quên toolbar group =
  công cụ vô hình. Quên arrow_options = không có tuỳ chọn.

---

## 6. Kiểm thử & bàn giao lại

**Kết quả cuối:** `cargo fmt --check` sạch; `cargo check --locked` pass;
`cargo test --locked --lib`: **1058 pass, 4 ignored, 0 fail** (1062 test). Người dùng đã
GUI-test OK. Theo yêu cầu cuối, không cần tạo/copy bản portable.

- Đã chạy `cargo fmt`, test module liên quan và `cargo test --locked --lib` toàn bộ.
  Người dùng yêu cầu không cần tạo/copy bản portable cho lượt hoàn tất này.
- **ĐỪNG push** cho tới khi người dùng bảo. Gom commit, báo cáo ngôn ngữ thường
  (người dùng là end-user, không hỏi câu kỹ thuật).
- Thứ tự đề xuất: hoàn tất **B** (mũi tên hiện được trên nét/Line trước), rồi **C**
  (công cụ) vì C dùng `end_arrow` của B.

## 7. Móc code nhanh (grep)
- Corner types (Stage A, tham chiếu): `RectCorner`, `rect_path_corners`,
  `sdf_rect_corners`, `corner_type`.
- Stroke dùng chung: `stroke_outline_contours` (`core/vector/stroke.rs`).
- Wiring tool mẫu: `ToolId::VectorBrush` (grep toàn repo ra đủ chỗ phải đụng).
- PDF vector: `collect_pdf_vectors`, `PdfVectorObject`, `append_vector_content`
  (`core/print.rs`).

## 8. Giao ca cho Claude — trạng thái sau GUI polish

- Người dùng đã GUI-test và xác nhận OK cho Arrow/Connector, lỗi zoom 9× và bộ icon
  Corner kiểu Corel.
- Top-options đã gom Stroke/Arrow vào palette icon; Shape và Arrow options có icon trực
  quan. Round/Scallop/Chamfer là ba nút hình học đặt cạnh nhau; click phải trên nút hoặc
  trên Rectangle đang chọn mở Corner palette.
- Bug phát hiện sau GUI-test: Convert to Curves từng gọi `rect_path` nên ép Scallop/
  Chamfer về Round. Commit `41feb53` đổi sang `rect_path_corners` với đúng
  `[effective_radius; 4]` và `[corner_type; 4]`; test khóa hình học cả ba kiểu đã thêm.
- Người dùng đã test lại và báo OK trước khi yêu cầu giao ca.
- Không còn hạng mục mở trong kế hoạch arrows/corners. Claude nên tin filesystem/HEAD,
  giữ working tree sạch, không push nếu chưa được người dùng yêu cầu.
