# Cổng Foundation Freeze — Giả lập trên giấy 4 tính năng tương lai

Ngày: 2026-07-23 · Nhánh: `feat/vector-core-foundation`

Tài liệu này là hạng mục **thứ 3** của cổng Foundation Freeze (Mục 3.11 của
`KE_HOACH_PHAT_TRIEN_VECTOR_IAI.txt`): "giả lập trên giấy" các tính năng tương lai
để chứng minh chúng **CHỈ THÊM (additive)**, không đòi sửa 10 contract nền tảng.
Nếu một tính năng đòi đổi contract thì phải sửa nền TRƯỚC khi freeze.

Mọi kết luận dưới đây được đối chiếu trực tiếp với code contract thật (không suy
diễn từ trí nhớ):
`core/vector/path.rs`, `color.rs`, `style.rs`, `object.rs`,
`core/command_vector.rs`, `formats/iai_vector.rs`, `core/layer.rs`.

---

## 0. 10 contract nền tảng (nơi định nghĩa)

| # | Contract | Định nghĩa tại |
|---|----------|----------------|
| 1 | `AffineTransform` + `PathData/Contour/Node/FillRule` | `core/vector/affine.rs`, `path.rs` |
| 2 | `ColorValue` (Rgb/Cmyk/opacity) | `core/vector/color.rs` |
| 3 | `VectorStyle`/`StrokeStyle`/`Paint` | `core/vector/style.rs` |
| 4 | Coordinate space (local→transform→layer→offset, page-relative) | `object.rs` docstring |
| 5 | `LayerType::Path(VectorObjectData)` + discriminator clip (`clip_parent_id`) | `core/layer.rs` |
| 6 | Command vector qua gateway (Create/Delete/Replace/ChangeStyle/ChangeTransform/Rasterize) | `core/command_vector.rs` |
| 7 | Cache/generation + layer_revision/atlas invalidation | `core/vector/raster.rs`, `canvas/*` |
| 8 | Schema `.iai` Path (`{schema, objects:[...]}`) + version v4 | `formats/iai_vector.rs`, `iai.rs` |
| 9 | Rasterize policy (opt-in `RasterizeVectorLayer`) | `command_vector.rs` |
| 10 | Page/Artboard identity + page-relative coords (MVP: 1 page ngầm) | *chỉ khóa invariant — xem §1.D* |

**Ba đòn bẩy additive** khiến hầu hết tính năng tương lai không đụng nền:

- `PathData` là **danh sách `Contour`** (compound path + lỗ nằm ngay đây, không
  phải layer ẩn) với `fill_rule` NonZero/EvenOdd → mọi hình phẳng, kể cả có lỗ,
  đều biểu diễn được.
- `ColorValue` và `Paint` là **enum chừa chỗ mở rộng**: thêm `Spot`,
  `Gradient(..)`, `Pattern(..)` là thêm variant, không migrate object cũ.
- Envelope `.iai` là **danh sách object** `{schema, objects:[...]}` (MVP ghi 1
  object) → bản sau đọc nhiều object/1 layer KHÔNG cần bump version; mọi key trong
  manifest là optional-mặc-định (đã chứng minh khi thêm `parent`, `clip_parent`).

---

## 1. Giả lập từng tính năng

### A. Polygon / Star (primitive mới — Phase 4)

**Biểu diễn:** một đa giác đều n cạnh = một `Contour` đóng gồm n `Node::sharp`
(không tay nắm). Ngôi sao = một `Contour` đóng gồm 2n node xen kẽ bán kính
ngoài/trong. `PathData` hiện tại chứa được cả hai **không đổi một dòng contract**.

**Đường tích hợp:** giống hệt `Convert to Curves` đã có (`from_shape.rs` sinh
`rect_path`/`ellipse_path`/`line_path`) — thêm `polygon_path`/`star_path` là **hàm
mới**, không đụng struct. Nếu muốn Polygon/Star là **Shape tham số sống** (kéo số
cạnh), đó là thêm một variant `ShapeKind` (raster Shape, nằm ngoài lớp vector) —
cũng additive, và `Convert to Curves` bắc cầu về `PathData` khi cần node-edit.

**Kết luận:** ✅ Additive. Không đổi contract #1–#10.

### B. Boolean (Union / Intersect / Difference / XOR)

**Biểu diễn:** đầu vào là N `PathData`, đầu ra là **một** `PathData` đa-contour +
`fill_rule`. Kết quả boolean (kể cả hình có lỗ, nhiều mảnh rời) chính là một
`PathData` nhiều `Contour` — **đã biểu diễn được**.

**Đường tích hợp:** một command feature-layer sinh `PathData` mới rồi đi qua
`ReplacePathGeometry` (hoặc `CreatePathLayer` + `DeletePathLayer` cho các object
nguồn) — **toàn bộ dùng command đã có**. Thuật toán clip đa giác là chuyện của lớp
tính năng; nếu cần thêm crate (vd `lyon`/clipping) thì theo quy ước kế hoạch phải
**hỏi user trước** — đây là quyết định dependency, KHÔNG phải đổi contract.

**Kết luận:** ✅ Additive. Kiểu dữ liệu kết quả (đa-contour `PathData` + fill_rule)
đã nằm trong nền đóng băng.

### C. Text → Curves

**Biểu diễn:** đường viền glyph → `PathData` đa-contour (mỗi glyph góp ≥1 contour
đóng; lỗ chữ "O"/"A" xử lý bằng winding + `fill_rule`). Font TrueType dùng Bézier
**bậc hai**; `PathData` chỉ lưu **bậc ba** → nâng bậc hai lên bậc ba là phép biến
đổi **chính xác không mất mát** (`c1=p0+⅔(p1−p0)`, `c2=p2+⅔(p1−p2)`) làm lúc
convert. Không cần thêm kiểu segment mới.

**Giới hạn parser:** `MAX_TOTAL_NODES = 4_000_000`, `MAX_NODES_PER_CONTOUR = 1e6`,
`MAX_CONTOURS = 65_536` (path.rs) — rất rộng cho biển hiệu/bao bì (một glyph ~20–100
node → ~80k glyph vẫn trong hạn).

**Nhiều màu:** một object = một `VectorStyle` (một fill). Text nhiều màu → **nhiều
object**, mà envelope `.iai` đã là danh-sách-object nên additive, không bump
version.

**Kết luận:** ✅ Additive. `PathData` (bậc ba, đa-contour, fill_rule) biểu diễn đủ
đường viền chữ; bậc hai nâng thành bậc ba lúc convert.

### D. Đa trang / Artboard (Mục 3.13) — tính năng cần soi kỹ nhất

**Điểm mấu chốt đã khóa từ đầu:** tọa độ vector là **trừu tượng `f32`
page-relative**, KHÔNG dính kích thước canvas (`affine.rs`/`path.rs` lưu số học
thuần; `object.rs` docstring ghi rõ "page-space == canvas-space" ở MVP 1 page
ngầm). Đây chính là điều khiến thứ **duy nhất** có thể ép migrate diện rộng —
*diễn giải lại tọa độ* — đã bị loại bỏ sẵn.

**Thêm page về sau là additive:**
- Định danh page = **field optional** (`page_id`, serde-mặc-định → page 0) trên
  Layer (hoặc VectorObjectData). File cũ thiếu key → page ngầm 0. Đã chứng minh
  bằng cách `parent`/`clip_parent` được thêm không phá format.
- Document thêm **mảng `pages`** (MVP: một page mô tả origin (0,0), size = canvas,
  identity, bleed/margin/background) — vắng key → một page ngầm.
- PDF export duyệt từng page; palette/style/asset đã ở cấp document, không nhân
  bản theo trang.

**Một quyết định ADR còn mở (điều kiện tiên quyết freeze, KHÔNG phải sửa code
ngay):** chốt **page_id sống ở Layer hay ở VectorObjectData**, và hình dạng struct
`Page = { id, origin+size trong document-space, bleed, margin, background }`. Vì
việc thêm là additive-serde-default, KHÔNG cần dựng model page bây giờ; chỉ cần
**ghi quyết định** để lần thêm sau có kỷ luật. Mô hình hợp nhất (Mục 3.13) bao được
cả Affinity Artboard (page = vùng trong document lớn) lẫn Publisher page (page =
canvas render độc lập).

**Kết luận:** ✅ Additive về mặt tọa độ/format (invariant page-relative đã giữ).
⚠️ Một ADR "nơi đặt page_id" cần ghi trước khi tuyên bố freeze — đây là quyết định
tài liệu, không ép đổi struct nền lúc này.

---

## 2. Trạng thái cổng freeze

| Điều kiện (Mục 3.11) | Trạng thái |
|----------------------|-----------|
| (1) 10 contract có test round-trip + property | ✅ round-trip schema + property sweep (`iai_vector.rs`), round-trip Canvas RGB+CMYK + page_id (`iai.rs`), undo/redo command (`command_vector.rs`), validate core. |
| (2) M1 chạy end-to-end RGB **và** CMYK | ✅ Test tự động (`m1_end_to_end_*`) + **user GUI-test xác nhận OK 2026-07-23**. |
| (3) Giả lập 4 tính năng tương lai | ✅ Tài liệu này — cả 4 additive; contract #10 đã vật chất hóa (xem ADR). |

**✅ ĐÃ TUYÊN BỐ FOUNDATION FREEZE (2026-07-23).** Cả hai việc chốt cổng đã xong:
user GUI-test M1 OK, và ADR "page ownership" đã chốt **+ vật chất hóa** (`PageId`/
`Page`/`Layer::page_id` + persistence) — xem
[ADR_PAGE_OWNERSHIP.md](ADR_PAGE_OWNERSHIP.md). Tuyên bố chính thức + danh sách 10
contract đóng băng: [FOUNDATION_FREEZE.md](FOUNDATION_FREEZE.md).

Từ đây lớp tính năng (Pick/Node nâng cao, Polygon/Star, PowerClip UX, Boolean,
Text→Curves, gradient/dash, Artboard) CHỈ THÊM, không sửa nền.
