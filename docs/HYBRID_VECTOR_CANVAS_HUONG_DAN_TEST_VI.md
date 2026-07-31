# Hướng dẫn test thủ công: Canvas lai vector–raster (tiếng Việt)

Làm lần lượt theo thứ tự. **Lưu một bản nháp trước** những bước có sửa/đảo lớp.
Trừ khi bước đó ghi rõ bật cờ thử nghiệm, kết quả mong đợi là **đường vẽ raster
cũ (fallback)**.

## Hiện tại trong app có gì (2026-07-31)

Renderer GPU (Phase 1) đã xong + test tự động trên GPU, **và đã được NỐI vào
canvas thật (Phase 2)** — nhưng nằm sau **cờ an toàn** (mặc định TẮT). Khi cờ tắt,
app vẽ **y như cũ**. Ngoài ra, thứ đã đổi sẵn trong app (không cần cờ) là "đường
tắt" của Phase 0: bản vẽ nét-sắc giờ bake **theo từng đối tượng** + huỷ job cũ
giữa chừng (bài 5, 11).

**Quy tắc GPU vector hiện áp dụng (bản đầu, an toàn):** chỉ các lớp Path **TĨNH,
KHÔNG phải lớp đang chọn** mới vẽ bằng GPU. Lớp **đang chọn/đang sửa** vẫn vẽ theo
đường raster cũ (để xem sửa node/kéo trực tiếp cho đúng). Các thứ nâng cao (mask,
group, gradient, độ mờ, blend khác Normal, nét đứt, nét không-bo-tròn, CMYK,
Shape tham số, crop) đều tự lùi về raster.

Cách tự kiểm renderer GPU (cần GPU thật, vài giây — chạy 1 luồng cho nhanh):

```bash
cargo test --test vector_gpu_render -- --ignored --test-threads=1 --nocapture
```

Mong đợi: 4 test đều pass, mỗi hình in `interior=… ok` / `... ok`.

## Bật GPU vector cho canvas (cờ khẩn cấp)

Đặt biến môi trường **trước khi mở app**, rồi mở app:

- PowerShell: `$env:IAI_GPU_VECTOR_CANVAS=1; .\dist\iAi-portable\iai.exe`
- Tắt lại: đóng app, mở lại **không** đặt biến (hoặc `$env:IAI_GPU_VECTOR_CANVAS=0`).

Nguyên tắc: **tắt cờ phải trả app về đúng đường vẽ cũ**. Nếu bật cờ thấy sai, tắt
cờ là hết sai ngay — đó là lưới an toàn.

## Test GPU nhanh (làm trước, để xác nhận nối đúng)

1. Mở app **có** đặt cờ. Tạo 2–3 lớp Path tô đặc (Pen → Path, hoặc chữ → đường
   cong). Bấm chọn một lớp **khác** (để các lớp Path kia thành "không active").
2. Zoom lên 800%–3200% vào một lớp Path **không đang chọn**.
   - **Đúng:** viền lớp đó **sắc nét ngay lập tức** (không phải chờ "bake"), CPU
     không nhảy vọt. So với tắt cờ: khi tắt cờ, lúc mới zoom thường thấy mờ một
     nhịp rồi mới nét.
3. Chọn lần lượt sang lớp Path khác. Lớp vừa bỏ chọn phải vẫn hiện đúng chỗ, đúng
   thứ tự chồng; lớp mới chọn chuyển sang đường raster (vẫn đúng hình).
4. **Dấu hiệu lỗi cần báo:** viền đôi/quầng sáng quanh nét, lớp Path nhảy vị trí
   khi bật cờ, mất lớp, sai thứ tự (Path nổi đè sai), hoặc khác biệt màu rõ ở vùng
   ĐẶC (không phải chỉ ở mép). Nếu gặp → tắt cờ, app phải hết lỗi.

---

## 1. Cờ khẩn cấp và đường fallback không đổi

1. Mở app **không** đặt cờ. Mở tài liệu có lớp raster, chữ, Shape và Path.
2. Zoom 25% → 100% → 800% → mức cao nhất; vừa zoom vừa kéo (pan).
3. **Đúng:** mọi lớp còn hiện, thứ tự không đổi, viền Path dần sắc nét (qua bản
   bake cũ).
4. **Dấu hiệu lỗi:** mất Path, viền đôi/quầng sáng, đảo thứ tự lớp, khung đen
   hoặc trong suốt.

## 2. Path đặc và quy tắc tô (fill rule)

1. Tạo: hình chữ nhật đổi-thành-đường-cong, elip đổi-thành-cong, một hình lõm, và
   hai hình ghép có lỗ (một NonZero, một EvenOdd).
2. Đặt màu tô đặc, không viền.
3. Xem ở 25%, 100%, 800%, 6400%; lật và xoay từng hình.
4. **Đúng:** lỗ và chiều cuốn giữ nguyên trước/sau khi biến đổi; đổi zoom không
   làm đổi phần tô.
5. **Lỗi:** lỗ bị tô đầy, mất viền, nứt hình tam giác, hoặc viền lệch quá 1 pixel
   màn hình.

## 3. Chữ → đường cong (Text → Curves)

1. Gõ chữ có lỗ, ví dụ `B8Oa`, rồi đổi thành đường cong.
2. So bản đổi với chữ gốc ở 100%, 800%, 6400%.
3. Sửa một node, Undo, rồi Redo.
4. **Đúng:** các lỗ chữ vẫn là lỗ; Undo/Redo khôi phục đúng hình.
5. **Lỗi:** cuốn ngược chiều, mất glyph, còn sót lớp phủ cũ trước khi sửa.

## 4. Thứ tự chồng (z-order)

1. Dựng chồng: `raster – vector – raster trong suốt – vector – raster`.
2. Bật/tắt từng lớp, dời lớp vector giữa xuống đáy rồi lên đỉnh, Undo/Redo.
3. Lặp lại với lớp đang chọn nằm ở đáy, giữa, đỉnh.
4. **Đúng:** hình hiện đúng như bảng Layers, không có lớp vector "nổi đè" lên trên.
5. **Lỗi:** vector luôn nằm trên raster, viền đôi, hoặc khung còn giữ thứ tự cũ.

## 5. Biến đổi và hành vi cache

1. Tạo 100 lớp Path đặc và chọn hết.
2. Pan/zoom liên tục, rồi di chuyển, xoay, co giãn đều, co giãn méo, và lật cụm chọn.
3. Sửa **đúng một** node trên **một** Path.
4. **Đúng:** thao tác vẫn mượt; chỉ việc sửa node mới đổi hình; ngừng tay là hiện
   bản sắc nét.
5. **Lỗi:** CPU làm việc dài khi chỉ pan, hình trôi vị trí, còn kẹt vị trí cũ,
   hoặc RAM bò lên không dừng.

## 6. Nét viền (stroke)

1. Test đường mở và đóng với đầu nét butt/round/square và mối nối miter/round/bevel.
2. Gồm góc nhọn, đoạn dài 0, co giãn méo, và một đường đóng nét-đứt.
3. **Đúng:** hình nét như cũ. Nét-đứt phải lùi về raster cả lớp. **Nét đầu/nối
   KHÔNG bo tròn cũng phải lùi về raster** (bộ vẽ CPU luôn vẽ bo tròn nên GPU chỉ
   nhận nét bo tròn).
4. **Lỗi:** mất nét âm thầm, cụt đầu nét, nét-đứt biến thành liền, hoặc hiện phần
   tô mà thiếu nét.

## 7. Gradient và độ mờ

1. Áp gradient thẳng và tròn có mốc alpha; biến đổi gradient và vật thể riêng rẽ.
2. Đặt độ mờ (opacity) vật thể và lớp dưới 100%, đặt raster trên và dưới.
3. **Đúng:** mọi trường hợp dùng raster fallback và giống hệt bản trước khi đổi.
4. **Lỗi:** bị thay bằng màu đặc, sai alpha, gradient trôi theo khung nhìn, hoặc
   đổi kết quả hoà trộn.

## 8. Hình tham số (Shape)

1. Tạo chữ nhật, chữ nhật bo góc, elip, đường thẳng, đa giác, sao mà **không** đổi
   thành đường cong.
2. Sửa tay nắm tham số và kiểu.
3. **Đúng:** vẫn sửa được và ảnh raster như cũ.
4. **Lỗi:** Shape bị đổi phá huỷ, mất tay nắm, hoặc cache raster bị chặn.

## 9. Mask, cắt xén, group và blend

1. Test: mask lớp trên một Path, mask vector, PowerClip, group lồng nhau, độ
   mờ/cô lập của group, và vài blend khác Normal.
2. Đảo thứ tự con raster/vector và bật/tắt mask/group.
3. **Đúng:** mọi ca nâng cao vẫn dùng raster fallback và giống hành vi app bình thường.
4. **Lỗi:** nội dung không bị cắt, bỏ qua mask, blend thành Normal, group áp alpha
   theo từng con, hoặc nội dung biến mất.

## 10. CMYK và quản lý màu

1. Mở tài liệu CMYK có màu vector RGB và CMYK, rồi bật soft proof.
2. So các mức zoom và bật/tắt proof.
3. **Đúng:** màu vector CMYK vẫn raster fallback; preview chỉ đổi theo đúng đường
   quản-lý-màu hiện có.
4. **Lỗi:** CMYK vẽ như RGB không hồ sơ, đổi tông bất ngờ, hoặc đường GPU bị bật
   cho màu CMYK.

## 11. Fixture nặng (stress)

1. Chạy bộ sinh hoa ở 100, 300, 500 lớp, gồm lỗ ghép và một lớp chữ-đã-đổi-cong.
2. Ở mỗi cỡ, test zoom, pan-lúc-zoom, di chuyển nhiều, xoay/scale, sửa node,
   bật/tắt hiện, đảo thứ tự.
3. **Đúng:** không mất nội dung hay treo; raster fallback luôn sẵn. Ghi lại CPU
   đỉnh, thời gian từ lúc ngừng thao tác tới lúc hiện sắc, và RAM ổn định.
4. **Lỗi:** kết quả worker về cho bản sửa cũ, hình mờ vĩnh viễn, RAM vô hạn, mất
   thiết bị GPU, hoặc sai thứ tự chồng.

## 12. Lưu file, tab, và phục hồi

1. Lưu `.iai`, đóng, mở lại, so với bản trước khi lưu.
2. Chuyển nhanh giữa hai tab trong khi đang chờ bake nét-sắc.
3. Nếu được, tái tạo GPU (ngủ/thức máy hoặc driver reset) rồi mở lại tài liệu.
4. **Đúng:** dữ liệu vector nguồn không đổi, không lưu dữ liệu lưới GPU, việc cũ
   không rơi nhầm sang tài liệu khác, và raster fallback dựng lại được khung nhìn.
5. **Lỗi:** đổi ý nghĩa file, phủ nhầm tab, mất lớp sau khi mở lại, hoặc không
   phục hồi được bằng đường raster.
