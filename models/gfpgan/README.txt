Thư mục: PHỤC HỒI KHUÔN MẶT (Face Restore)
==========================================

Bỏ 1 file .onnx phục hồi khuôn mặt vào đây.

Chuẩn kỹ thuật (contract) model phải đạt:
- Input : float32 NCHW [1,3,512,512], ảnh mặt đã căn (RGB), chuẩn hoá về [-1,1].
- Output: float32 [1,3,512,512] RGB. App tự nhận cả model xuất [0,1] (như
          GFPGAN) lẫn [-1,1] (như RestoreFormer++).

Model đã kiểm nghiệm tương thích:
- GFPGAN v1.4  — giấy phép Apache-2.0 (dùng thương mại được).
    Nguồn: https://github.com/TencentARC/GFPGAN
- RestoreFormer++  — giấy phép Apache-2.0 (thương mại được), nét hơn GFPGAN.
    Bản .onnx (~294 MB):
    https://huggingface.co/datasets/Gourieff/ReActor
      -> models/facerestore_models/RestoreFormer_PP.onnx

KHÔNG khuyến nghị cho bản thương mại (chỉ phi thương mại):
- CodeFormer (S-Lab License 1.0), GPEN (chỉ học thuật/phi thương mại).

Lưu ý: hầu hết model phục hồi mặt huấn luyện trên FFHQ — cân nhắc điều khoản
dữ liệu gốc trước khi dùng thương mại. Bạn tự chịu trách nhiệm giấy phép.
