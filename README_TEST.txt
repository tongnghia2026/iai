iAi Offline AI Retouch — Phase 2 Color Fix

1. Chạy iAi.exe trong thư mục bản test, mở một ảnh chân dung rồi mở Ai Image Studio → AI Auto Retouch.
2. Các mức mặc định đều là 100. Đây là mức an toàn tối đa của từng stage; Protect Identity và safety ceiling vẫn bảo vệ ảnh gốc.
3. Kiểm tra riêng màu: bấm “Tắt tất cả” → tích “Color / Exposure” → chọn Look màu “Tươi sáng” → Run. App tự nhận diện ám xanh/đỏ/vàng và cân bằng trắng trước khi tạo màu sáng, trẻ trung.
4. Có bốn Look màu: Tươi sáng (mặc định), Tự nhiên, Ấm trẻ và Mát trong. IAT quyết định độ sáng theo từng vùng; hệ Auto White Balance quyết định cân bằng kênh màu nên IAT không còn làm ảnh ám xanh nặng hơn.
5. Khi chạy Color xong, status phải có color-cast, wb-gains R/G/B và color-confidence. Ảnh mẫu ám xanh phải báo color-cast=green và gain G thấp hơn R/B.
6. Muốn chạy riêng một vùng: bấm “Tắt tất cả”, tích lại Hair/Skin/Eyes/Lips/Clothes hoặc stage cần chạy, rồi bấm Run Auto Retouch. Stage bỏ tích không nạp model và không sửa pixel.
7. Ví dụ mặt đã nét, chỉ sửa tóc: Tắt tất cả → tích Hair → Run. Có thể chạy trên layer AI Auto Retouch hiện tại để tạo kết quả mới.
8. “Tự động dùng GPU mạnh (DirectML)” mặc định bật. Dòng GPU trong panel cho biết card được nhận diện. Nếu DirectML không hỗ trợ một model, app tự dùng CPU cho model đó.
9. Bật “Tạo layer Preview Masks” để kiểm tra toàn ảnh: nền xanh đậm; tóc xanh; da đỏ; quần áo vàng; vùng khác/phụ kiện tím; mắt xanh lá; môi hồng.
10. So Before/After bằng nút trong panel. Layer Preview Masks có nút Ẩn/Hiện riêng.
11. Auto noise estimate có thể tự giảm hoặc bỏ qua Denoise trên ảnh sạch dù thanh Denoise là 100. Bỏ tích Auto noise estimate nếu cần ép NAFNet chạy để so A/B.
12. Upscale mặc định Off. x2/x4 vượt 150 triệu pixel sẽ tự bị bỏ qua để tránh hết RAM.
13. Khi chạy xong, kiểm tra status: provider=DirectML GPU hoặc ONNX Runtime CPU, danh sách model thật đã chạy, changed, mean-delta và timing từng stage.
14. Nếu mask tóc/quần áo/nền vẫn sai trên ảnh thực tế, gửi ảnh gốc và ảnh chụp layer Preview Masks. Khi đó mới đánh giá thay model Body Parsing theo đúng kế hoạch.
15. Nếu lỗi, gửi nguyên status bar và log trong thư mục logs/.

Model đi kèm (9/9): YuNet, BiSeNet, MediaPipe SelfieMulticlass Body Parsing,
NAFNet, IAT, GFPGAN, Real-ESRGAN General x4v3, RRDB x2 và RRDB x4.
Nguồn, license, tensor contract và checksum nằm trong docs/AI_MODELS.md và
models/retouch-manifest.json.
