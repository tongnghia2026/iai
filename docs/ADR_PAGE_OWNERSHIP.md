# ADR: Page / Artboard ownership (contract #10)

Ngày: 2026-07-23 · Trạng thái: **ĐÃ CHỐT + vật chất hóa** · Liên quan: Mục 3.13,
[FOUNDATION_FREEZE_PAPER_SIM.md](FOUNDATION_FREEZE_PAPER_SIM.md)

## Bối cảnh

Cổng Foundation Freeze đóng băng 10 contract. Contract #10 = "Page/Artboard
identity + page-relative coords". Trước hôm nay, phần *identity* mới chỉ là ý định
trong kế hoạch (Mục 3.13), chưa tồn tại trong code — không thể "đóng băng" một
contract chưa có. Tài liệu này chốt quyết định và ghi lại những gì đã vật chất hóa.

## Quyết định

**1. `page_id` sống ở `Layer`, KHÔNG ở `VectorObjectData`.**
Một page/artboard gom **nhiều layer đủ loại** (raster/text/shape/path). Nếu đặt
page ownership trên `VectorObjectData` thì chỉ vector có, còn ảnh raster nằm trên
artboard sẽ không có chỗ khai báo. Vậy quyền sở hữu page phải **đồng nhất trên mọi
layer** → nằm ở `Layer::page_id`.

**2. Kiểu contract:**
- `PageId(u32)` — id mờ như `Layer::id`; `PageId::IMPLICIT == PageId(0)` là page MVP.
- `Page { id, origin, size, bleed, margin, background }` đặt trong document-space.
  `origin + size` bao được **cả** Affinity Artboard (page = vùng của document lớn)
  **lẫn** Publisher page (page = canvas render độc lập). `bleed/margin/background`
  là ý định in.

**3. MVP = một page ngầm.** Mọi layer mặc định `PageId::IMPLICIT`;
`Page::implicit(w,h)` = origin (0,0), size = canvas, không bleed/margin/background.
Khi đó page-space == canvas-space → **hành vi hôm nay không đổi**.

## Đã vật chất hóa (2026-07-23)

- `src/core/page.rs`: `PageId`, `Page`, `Page::implicit/rect/validate` + test.
- `src/core/layer.rs`: `Layer::page_id: PageId` (mặc định IMPLICIT ở mọi
  constructor; `duplicate` sao chép nguyên page — khác clip là remap).
- `src/formats/iai.rs`: ghi/đọc khóa `"page"` per-layer (chỉ ghi khi khác IMPLICIT
  → doc một-trang mặc định giữ nguyên manifest); test round-trip id khác 0.

## Khe additive còn để dành (KHÔNG phá nền khi thêm)

Những phần dưới đây xây SAU, thuần cộng-thêm, vì tọa độ đã page-relative trừu tượng
và `.iai` dung nạp key mới (đã chứng minh bằng `parent`/`clip_parent`/`page`):

1. **Container đa trang**: `Document.pages: Vec<Page>` + envelope `pages` trong
   `.iai` (vắng → suy ra một page ngầm từ canvas).
2. **Export/PDF theo từng page** và UI Artboard.
3. **Master Pages** (`master_ref` optional trên page thường; MVP None) — Mục 3.13.

Không cái nào đòi đổi `PathData`/`ColorValue`/`VectorStyle`/tọa độ đã đóng băng.

## Hệ quả

Contract #10 giờ tồn tại thật (kiểu + quyền sở hữu trên layer + persistence + test)
→ đủ điều kiện đóng băng cùng 9 contract còn lại. Xem
[FOUNDATION_FREEZE.md](FOUNDATION_FREEZE.md).
