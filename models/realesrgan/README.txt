Thư mục: TĂNG NÉT & PHÓNG TO (Upscale / Detail)
===============================================

Bỏ file .onnx họ ESRGAN / Real-ESRGAN (kiến trúc RRDBNet) vào đây.

Chuẩn kỹ thuật (contract):
- Input : float32 NCHW [1,3,H,W] RGB, giá trị [0,1].
- Output: float32 [1,3,H*scale,W*scale] RGB [0,1]  (scale x2 hoặc x4).

Model đã kiểm nghiệm tương thích:
- Real-ESRGAN (general x4v3, x2plus, x4plus) — BSD-3-Clause (thương mại được).
    Nguồn: https://github.com/xinntao/Real-ESRGAN
- Model cộng đồng cùng kiến trúc RRDBNet (OpenModelDB) cũng chạy được, NHƯNG
  giấy phép mỗi model mỗi khác — tự kiểm tra trước khi dùng thương mại.

Model transformer (SwinIR/HAT, Apache-2.0) chất lượng cao hơn nhưng ĐỊNH DẠNG
KHÁC, hiện chưa cắm thẳng vào ô này được.

Bạn tự tải model và tự chịu trách nhiệm về giấy phép.
