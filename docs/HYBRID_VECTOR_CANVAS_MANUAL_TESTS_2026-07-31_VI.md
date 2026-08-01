# Test thủ công tuần tự — Canvas hybrid vector (bản 2026-07-31)

Tài liệu này để test **đường GPU** sau khi bật cờ, gồm Phase 2 (nối vào canvas),
Phase 3 (cache mesh GPU + transform chỉ đổi uniform) và Phase 6 (Shape/Primitive
vẽ bằng GPU). Test lần lượt từ trên xuống, **cơ bản → nâng cao**. Trước các bước
sửa/đảo layer, lưu một bản `.iai` nháp để hoàn tác an toàn.

Nguyên tắc vàng: **bật cờ và tắt cờ phải cho ra hình GIỐNG HỆT nhau**. Cờ GPU chỉ
được nhanh hơn / nét hơn, không được đổi hình. Nếu thấy khác hình giữa on/off →
đó là lỗi, ghi lại bước nào.

## 0. Chuẩn bị

Chạy test tự động trên GPU thật (vài giây, không đụng gì tới máy):

```bash
cargo test --test vector_gpu_render -- --ignored --nocapture --test-threads=1
```

Mong đợi: 6 test đều `ok` (hoặc bỏ qua sạch nếu máy không có GPU adapter). Đáng
chú ý:
- `gpu_mesh_cache_is_transform_invariant` → in `2 entries, 112 bytes, 0 evictions`
  (Phase 3: pan/zoom/move/rotate/scale KHÔNG tessellate lại, sửa node = 1 lần).
- `gpu_primitive_matches_cpu_reference` → rect / rounded / ellipse / polygon /
  star đều khớp CPU (Phase 6).

Đường GPU hiện được bật mặc định, nên chạy app bình thường:

```powershell
cargo run
```

(Muốn về pipeline raster cũ để đối chứng/khẩn cấp: đóng app, đặt
`$env:IAI_GPU_VECTOR_CANVAS = "0"`, rồi mở lại. Xóa biến sẽ bật lại GPU mặc định.)

Trong dist portable đã kèm sẵn 1 file `.bat` không? Nếu không, chỉ cần đặt biến
môi trường như trên trước khi chạy.

## 1. Cờ tắt = y hệt bản cũ (đối chứng)

1. Mở app với `IAI_GPU_VECTOR_CANVAS=0`. Mở tài liệu có đủ raster, text, Shape, Path.
2. Zoom 25% → 100% → 800% → cao nhất; vừa zoom vừa pan.
3. Mong đợi: mọi layer hiện đủ, đúng thứ tự, nét Path lên dần như trước.
4. Dấu hiệu lỗi: mất Path, viền đôi/hào quang, đảo thứ tự, khung đen/trong suốt.

Giữ tài liệu này để so sánh với cờ bật ở các bước sau.

## 2. Path đặc + fill rule (cờ BẬT)

1. Tạo: 1 chữ nhật→curves, 1 ellipse→curves, 1 path lõm, 2 path có lỗ (một
   NonZero, một EvenOdd). Tô màu RGB đặc, không nét.
2. Xem ở 25%, 100%, 800%, 6400%. Lật (flip) và xoay từng object.
3. Mong đợi: lỗ và winding đúng trước/sau biến đổi; **hình khớp y như khi tắt
   cờ**; không đổi fill khi zoom.
4. Dấu hiệu lỗi: lỗ bị tô đầy, mất contour, nứt tam giác, viền lệch > 1 pixel màn
   hình so với bản tắt cờ.

## 3. Z-order xen kẽ raster ↔ vector

1. Dựng chồng: `raster – vector – raster mờ – vector – raster`.
2. Tắt/bật từng layer; kéo vector giữa xuống đáy rồi lên đỉnh; undo/redo.
3. Đặt layer đang chọn (active) lần lượt ở đáy, giữa, đỉnh.
4. Mong đợi: hình khớp đúng bảng Layers, **không** có lớp vector phủ đè lên trên
   cùng, không viền đôi.
5. Dấu hiệu lỗi: vector luôn nổi trên raster, viền đôi, hoặc khung giữ thứ tự cũ.

## 4. Transform KHÔNG giật + cache (Phase 3 — trọng tâm)

1. Tạo ~100 Path đặc, chọn tất cả.
2. Pan liên tục, zoom liên tục, rồi move / xoay / scale đều / scale lệch / lật.
3. Mong đợi: **mượt, CPU nền không tăng vọt** khi chỉ pan/zoom/move/xoay/scale
   (mesh GPU tái dùng, không tessellate lại, không upload lại buffer). Dừng tay
   là nét ngay.
4. Sửa đúng **một** node trên **một** Path.
5. Mong đợi: chỉ path đó dựng lại mesh; các path khác không hề bị dựng lại.
6. Dấu hiệu lỗi: cứ pan là CPU chạy nặng kéo dài, hình trôi/lệch vị trí, giữ vị
   trí cũ, hoặc RAM tăng không dừng.

## 5. Shape / Primitive vẽ bằng GPU (Phase 6 — mới)

1. Tạo **không convert-to-curves**: chữ nhật, chữ nhật bo góc, ellipse, đa giác
   (polygon), sao (star). Tô RGB đặc.
2. So sánh hình với khi tắt cờ ở 100%, 800%, 6400%.
3. Kéo, xoay, scale các shape này (nhưng **không** đang sửa handle của chúng).
4. Mong đợi: hình khớp y như bản tắt cờ; nét ở zoom lớn; **các tay nắm
   (handle) tham số vẫn còn** — model Shape không bị phá thành đường cong.
5. Shape **đang active nhưng không sửa** vẫn đi GPU và phải nét ngay. Khi đang
   sửa handle, shape đó tạm về raster/overlay cũ là đúng thiết kế. Nhả tay → GPU
   vẽ lại.
6. Dấu hiệu lỗi: shape bị convert phá huỷ, mất handle, lệch vị trí so với tắt cờ,
   hoặc raster twin lẫn với bản GPU (viền đôi).

## 6. Nét (stroke)

1. Path có nét đặc màu RGB với mọi cap/join lưu trong model đều đi GPU, nhưng hình
   hiện tại vẫn là viên nang tròn để khớp rasterizer CPU. So với tắt cờ phải khớp.
2. Path có **nét đứt (dash)** hoặc vector brush vẫn về raster fallback cả layer.
3. Mong đợi: hình khớp bản tắt cờ ở mọi trường hợp; dash không biến thành nét
   liền; không bao giờ hiện fill mà thiếu nét của nó.
4. Dấu hiệu lỗi: nét mất, cap bị cắt, dash thành liền, hoặc viền đôi.

## 7. Gradient RGB + opacity Normal GPU; mask / group / CMYK fallback

1. Path RGB đặc, blend Normal: đặt object opacity 50%, layer opacity 50%.
   Mong đợi đi GPU và alpha hiệu dụng 25%, khớp hình khi tắt cờ.
2. Tạo gradient RGB linear và radial, 2–8 stop, có alpha stop; kéo/xoay/scale
   gradient transform rồi zoom/pan. Mong đợi đi GPU, stop/alpha/vị trí khớp bản
   tắt cờ và không tessellate lại khi chỉ đổi stop.
3. Gradient có ít nhất một stop CMYK; layer mask trên Path; vector mask;
   PowerClip; group lồng nhau + group opacity; blend khác Normal; tài liệu CMYK
   có soft proof. Mong đợi các ca này raster fallback và khớp bản tắt cờ.
3. Dấu hiệu lỗi: gradient thành màu đặc, sai alpha, gradient trôi theo view, mask
   bị bỏ qua, group alpha áp theo từng con, CMYK ra RGB sai màu, hoặc GPU bật
   nhầm cho các ca này (viền đôi/hào quang).

## 8. Stress 100 / 300 / 500 layer

1. Dựng vườn hoa 100, 300, 500 layer (gồm lỗ compound và một layer text→curves).
2. Mỗi cỡ: zoom, pan-trong-lúc-zoom, multi-move, xoay/scale, sửa node, tắt/bật
   visibility, đảo thứ tự.
3. Mong đợi: không mất nội dung, không crash; mượt; dừng tay là nét. RAM ổn định
   (cache có ngân sách 96 MiB, tự đẩy LRU). So hình với tắt cờ.
4. Dấu hiệu lỗi: kết quả worker của bản sửa cũ nhảy vào, hình mờ vĩnh viễn, RAM
   tăng vô hạn, mất GPU (device loss), hoặc sai z-order.

## 8A. Active layer GPU / kết thúc CPU AA (Phase 8)

1. Trong vườn hoa, chọn lần lượt một Path ở đáy, giữa và đỉnh stack; không kéo
   handle hay mở phiên chỉnh style.
2. Zoom/pan liên tục trên 100%, rồi dừng ở 800% và 1600%.
3. Mong đợi: layer đang chọn sắc như các layer khác, đúng z-order; không có hiệu
   ứng quét nét tuần tự từ dưới lên và CPU không tiếp tục chạy worker AA nền.
4. Bắt đầu kéo node, handle Shape, gradient, transform hoặc scrub style.
5. Mong đợi: đúng layer đang sửa tạm fallback raster; không mất hình/viền đôi.
   Nhả tay hoặc commit/cancel thì active layer trở lại GPU ngay.
6. Dấu hiệu lỗi: active layer dưới GPU layer bị mờ, overlay nổi sai z-order, hoặc
   zoom/pan lại khởi động quét toàn bộ Repeat layers.

## 9. Lưu file / tab / phục hồi thiết bị

1. Lưu `.iai`, đóng, mở lại, so với bản trước khi lưu.
2. Chuyển nhanh giữa 2 tab khi đang có bake nét chờ.
3. Nếu được: ép mất/tạo lại GPU (sleep/wake hoặc driver recovery) rồi mở lại.
4. Mong đợi: dữ liệu vector nguồn không đổi, **không** serialize mesh GPU, việc cũ
   không nhảy sang tài liệu khác, mất GPU thì dựng lại view qua raster rồi GPU
   quay lại bình thường.
5. Dấu hiệu lỗi: đổi ngữ nghĩa file, overlay nhầm tab, mất layer sau khi mở lại,
   hoặc không phục hồi được sau device loss.

---

### Tóm tắt phạm vi bản này

| Tính năng | Trạng thái khi cờ BẬT |
|---|---|
| Path fill đặc, fill-rule/lỗ | GPU |
| Nét đặc RGB — cap thật (butt/tròn/vuông) + join thật (miter/bevel/tròn) + miter limit | GPU (Phase 4) |
| Shape/Primitive (rect/bo góc/ellipse/polygon/star), màu đặc | GPU (Phase 6) |
| Transform (pan/zoom/move/xoay/scale) | GPU, 0 tessellate lại (Phase 3) |
| Layer active, đang idle | GPU (Phase 8) |
| Layer đang có live edit/pending preview | Raster fallback tạm thời |
| Dash + dash offset | GPU (Phase 4) |
| Vector brush | Raster fallback |
| Object/layer opacity, blend Normal | GPU (Phase 5) |
| Gradient RGB linear/radial + alpha stops | GPU (Phase 5) |
| Multiply/Screen/Overlay/Darken/Lighten/Dodge/Burn/Hard Light/Soft Light/Linear Light/Difference/Exclusion | GPU (Phase 5, isolated run) |
| Hue/Saturation/Color/Luminosity (non-separable) | GPU (Phase 5, isolated run) |
| Gradient có stop CMYK; Dissolve | Raster fallback |
| Raster mask thường trên vector | GPU (Phase 7, lát 2) |
| PowerClip có frame + derived mask hợp lệ | GPU (Phase 7, lát 3) |
| Group Normal opacity, chỉ direct GPU vectors | GPU (Phase 7, lát 4) |
| Vector mask / mixed/nested/masked/blended group | Raster fallback |
| CMYK paint / soft-proof | Raster fallback |

Bất cứ ô "Raster fallback" nào mà **khác hình so với khi tắt cờ** đều là lỗi cần
báo. Bất cứ ô "GPU" nào mà chậm đi hoặc kém nét hơn tắt cờ cũng cần báo.
## Phase 4/5 — dash và blend mode

**LƯU Ý ĐỔI HÌNH (đã duyệt):** từ bản 2026-08-01 nét vẽ TÔN TRỌNG kiểu đầu/góc thật.
Bản cũ vẽ mọi nét thành bo tròn; giờ nét mặc định (đầu Butt + góc Miter) sẽ ra
**đầu cắt phẳng + góc nhọn**. Đây là ĐÚNG (giống CorelDRAW). Nét trong file cũ có
đặt đầu/góc ≠ tròn sẽ trông khác bản trước — đó là hành vi mong muốn, không phải lỗi.

Cap/join — cả bật lẫn tắt cờ phải cho hình GIỐNG NHAU (CPU và GPU cùng tô một outline):
1. Vẽ một nét dày (≥12px) HỞ có một góc vuông. Lần lượt đổi đầu nét **Butt → Round →
   Square**: Butt cắt phẳng ngay điểm cuối, Round bo nửa tròn, Square nhô ra thêm nửa
   độ rộng. Đổi góc **Miter → Round → Bevel**: Miter nhọn, Round bo, Bevel vát.
2. Với một góc rất nhọn, tăng/giảm **miter limit**: dưới ngưỡng ra mũi nhọn, quá ngưỡng
   tự chuyển sang vát (bevel). Bật/tắt cờ phải khớp nhau.
3. Zoom sâu: nét vẫn nét ở mọi mức, đầu/góc không rung hay đổi kiểu.

Dash và blend:
1. Tạo một Path hở và một Path kín, đặt dash pattern lẻ/chẵn và thay đổi dash offset.
2. Zoom, xoay và scale; kỳ vọng dash không biến mất, không nối nhầm qua khoảng trống và
   không phát sinh CPU sharp-bake khi layer idle.
3. Đặt một vector màu sáng giữa hai raster layer tương phản. Lần lượt thử Multiply,
   Screen, Overlay, Darken, Lighten, Color Dodge/Burn, Hard/Soft/Linear Light,
   Difference và Exclusion.
4. Với từng mode, bật/tắt `IAI_GPU_VECTOR_CANVAS`; kỳ vọng màu, opacity và z-order giữ
   nguyên. Hai vector blend liền nhau phải tác động tuần tự, không bị gộp thành một run.
5. Bốn mode non-separable **Hue, Saturation, Color, Luminosity** giờ cũng chạy GPU:
   dùng một vector nhiều màu (ví dụ vàng cam) trên nền ảnh raster nhiều sắc độ,
   bật/tắt cờ và kỳ vọng hình GIỐNG HỆT bản raster — Hue giữ độ sáng nền, Color
   giữ độ sáng nền, Luminosity giữ màu nền. Sai sắc/độ sáng giữa on/off là lỗi.
6. Chỉ còn **Dissolve** đi raster fallback: bật/tắt cờ phải cho hình giống nhau,
   không được mất layer hoặc đổi z-order.

## Phase 7 — lát 1: group pass-through

1. Bật `IAI_GPU_VECTOR_CANVAS=1`, tạo một raster layer dưới cùng.
2. Tạo group Normal, opacity 100%, không mask; đặt một vector RGB vào group.
3. Zoom/pan qua 100%, bật/tắt group và đổi thứ tự vector/raster quanh group.
4. Kỳ vọng: hình và z-order không đổi, không mờ/lặp; vector con vẫn GPU-native.
5. Đổi opacity group xuống dưới 100%, thêm mask hoặc blend khác Normal.
6. Kỳ vọng: cả subtree rơi về raster an toàn; kết quả giống khi tắt flag.

## Phase 7 — lát 2: raster mask trên vector

1. Tạo vector RGB nằm giữa hai raster layer có màu tương phản.
2. Thêm raster mask cho vector; vẽ vùng trắng, đen và xám mềm trên mask.
3. Zoom/pan qua 100%, di chuyển vector, bật/tắt và invert mask.
4. Kỳ vọng: trắng hiện đủ, đen ẩn đủ, xám blend mềm; mask đi cùng vector và
   z-order không đổi, không có halo hoặc hình vector thứ hai.
5. Đặt một vector GPU khác ngay trên/dưới layer có mask.
6. Kỳ vọng: mask chỉ tác động đúng layer của nó. Tắt flag phải cho cùng hình.
7. Chuyển sang vector mask hoặc đặt vector vào group có opacity/blend/mask.
8. Kỳ vọng: raster fallback an toàn; chưa dùng đường GPU của lát này.

## Phase 7 — lát 3: PowerClip

1. Tạo raster frame có alpha rõ ràng, rồi tạo/attach một vector RGB làm nội dung
   PowerClip; đặt thêm raster layer phía trên để kiểm tra z-order.
2. Zoom/pan qua 100%; di chuyển riêng content, rồi di chuyển riêng frame.
3. Kỳ vọng: vector chỉ hiện trong alpha của frame, cửa sổ clip bám frame trong
   lúc kéo, không nháy full-vector và layer phía trên luôn nằm trên cùng.
4. Sửa alpha/biên của frame rồi thả chuột.
5. Kỳ vọng: derived mask cập nhật và PowerClip đổi theo frame, không giữ mask cũ.
6. Release clip hoặc xóa frame/mask làm quan hệ tạm thiếu dữ liệu.
7. Kỳ vọng: không render sai hay crash; quan hệ không hợp lệ rơi về raster.

## Phase 7 — lát 4: group opacity isolation

1. Tạo group top-level, blend Normal, opacity khoảng 50%; đặt hai vector RGB đặc
   chồng lên nhau trong group và raster layer ở cả dưới lẫn trên group.
2. Zoom/pan qua 100%, đổi opacity group và di chuyển từng vector.
3. Kỳ vọng: vùng hai vector chồng nhau được composite trong group trước, rồi mới
   nhận opacity group một lần; không tối/nhạt kép và z-order ngoài group giữ đúng.
4. Thêm raster child, mask/PowerClip child, nested group, group mask hoặc đổi blend.
5. Kỳ vọng: toàn group chuyển về synthetic raster fallback và giống khi tắt flag.
6. Khi đang live-edit active vector trong group, kỳ vọng fallback tạm thời an toàn;
   thả chuột xong mới trở lại GPU-native.
