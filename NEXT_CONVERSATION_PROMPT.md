# Prompt cho hội thoại triển khai Develop Engine 2

Bạn đang làm việc trong bản sao source iAi dành riêng cho Develop Engine 2. Hãy đóng vai Principal Engineer / Color Scientist / RAW Image Processing Architect và triển khai theo tài liệu `DEVELOP_ENGINE_2_MASTER_PLAN.md`.

## Mục tiêu

Thực hiện một phiên coding tự chủ end-to-end. Tôi sẽ đi ngủ và không thể test hoặc xác nhận từng phase. Hãy tự audit, code, chạy quality gates và sửa lỗi liên tục; sáng hôm sau tôi chỉ nghiệm thu một lần.

## Yêu cầu bắt buộc

1. Đọc toàn bộ `DEVELOP_ENGINE_2_MASTER_PLAN.md`, đặc biệt Appendix D, trước khi sửa code.
2. Kiểm tra `git status`, branch, commit baseline, `AGENTS.md` và toàn bộ source liên quan. Tạo branch triển khai riêng; không làm mất checkpoint hiện tại.
3. Chọn đúng **Option B — Hybrid Rebuild**: giữ UI/UX, state, history, masks, document integration, serialization và legacy compatibility; xây Develop Engine 2 bằng typed/versioned processing graph.
4. Tuyệt đối clean-room. Có thể đọc ART tại `C:\Users\Admin\Pictures\1111\ART` để hiểu problem/behavior, nhưng không copy, port, dịch, đổi tên code, constants đặc thù, LUT, profile, resource, comment hay class layout của ART. Ưu tiên paper/spec CIE/ICC/DNG và implementation độc lập.
5. Không chờ tôi test giữa các phase. Tự chạy formatter, compile, unit/integration tests, CPU/GPU parity, golden/color/profile/project compatibility và benchmarks sau mỗi vertical slice. Chủ động sửa mọi regression trong phạm vi.
6. Không thực hiện một big-bang commit. Tạo commit nhỏ theo milestone để rollback được, nhưng tiếp tục làm xuyên suốt cho tới handoff cuối.
7. Không xóa legacy renderer trong lượt đầu. Project cũ phải giữ đúng look bằng engine versioning; project mới dùng Develop2 khi end-to-end path đã đạt gate.
8. Preview và export phải dùng cùng semantics/stage order/constants; proxy chỉ được giảm sampling/resolution. Không clamp sớm, không xử lý scene trên gamma-encoded sRGB, không dùng intermediate 8-bit trong core.
9. Tôn trọng các thay đổi có sẵn. Không reset, checkout bỏ, overwrite hoặc xóa công việc không thuộc nhiệm vụ.
10. Chỉ dừng hỏi tôi khi có blocker thật sự về quyền, dữ liệu/license hoặc product decision không thể suy ra an toàn. Nếu một phase quá lớn, hoàn thành milestone production-coherent tối đa với build/test xanh; không để placeholder/stub rồi tuyên bố xong.

## Đầu ra cuối phiên

- Code đã triển khai và các commits theo milestone.
- `DEVELOP_ENGINE_2_IMPLEMENTATION_REPORT.md` ghi architecture thực tế, files, migrations, test/benchmark commands và kết quả, known gaps, rollback.
- Working tree sạch hoặc giải thích chính xác mọi file còn lại.
- Một checklist manual test duy nhất, ngắn và theo thứ tự để tôi test vào buổi sáng.
- Nói rõ phần nào của master plan đã hoàn thành, phần nào chưa, không phóng đại.

Bắt đầu ngay bằng baseline verification, sau đó thực hiện batch order trong Appendix D. Không chỉ lập kế hoạch lại; hãy triển khai và kiểm chứng trong phạm vi khả thi của phiên.
