# Báo cáo dọn thư mục IAI — 06/09/2026

Phạm vi: `C:\Users\Admin\Documents\IAI`. Đối chiếu tại commit `a8db445`.
Git sạch trước khi dọn. Không sửa/xóa mã nguồn, cấu hình hay tài liệu có sẵn.

## Đã xóa

35.869 tệp, tổng dung lượng tệp **69.450.497.516 byte ≈ 64,68 GiB**.
Đây là tổng kích thước tệp, không phải phép đo dung lượng vật lý thu hồi trên ổ đĩa.

| Đường dẫn tương đối | Số tệp | Nội dung |
|---|---:|---|
| `target/debug/incremental` | 13.299 | Cache biên dịch tăng dần |
| `target/debug/deps` | 6.603 | Thư viện và chương trình kiểm thử được Cargo sinh ra |
| `target/debug/build` | 1.755 | Kết quả build dependency |
| `target/debug/.fingerprint` | 7.624 | Metadata cache Cargo |
| `target/release/incremental` | 0 | Thư mục cache rỗng |
| `target/release/deps` | 2.622 | Thư viện và chương trình kiểm thử được Cargo sinh ra |
| `target/release/build` | 769 | Kết quả build dependency |
| `target/release/.fingerprint` | 3.195 | Metadata cache Cargo |
| `_tu-dong-in-fix/__pycache__` | 2 | Bytecode Python có thể tạo lại |

Đã kiểm tra đường dẫn nằm trong workspace, không có tệp được Git theo dõi trong các mục xóa và không có junction/symlink thư mục trong các cây này. Không có tiến trình Cargo/Rust/Python đang chạy lúc xóa. Lần build/test sau sẽ mất thêm thời gian để tạo lại cache.

## Giữ nguyên để bạn duyệt trước khi xóa

Dung lượng dưới đây là MiB (1 MiB = 1.048.576 byte). Chưa xóa bất kỳ mục nào trong bảng.

| Mã | Mục | Dung lượng | Nhận định và đề xuất |
|---|---|---:|---|
| A | `output/iAi_Auto_Retouch_Phase2_ColorFix_2026-09-05` | 721,85 MiB | Bản test cũ từ `861c4bb`, là tổ tiên của bản hiện tại. 31/34 tệp trùng SHA-256 với portable mới; 3 tệp còn lại là executable cũ, BUILD_INFO và README_TEST khác. Đề xuất xóa nếu không cần bản đối chiếu/quay lại. |
| B | `tmp/model-export-env`, `tmp/model-checkpoints`, `tmp/model-sources` | 3.056,46 MiB | Môi trường Python, 6 checkpoint và source upstream phục vụ xuất ONNX. App dùng model đã xuất trong `models`, nhưng script xuất model vẫn cần những đầu vào này. Đề xuất xóa chỉ khi không cần tái xuất/chỉnh model; sau này phải tải và dựng lại. |
| C | `tmp/retouch-quality`, `tmp/pdfs`, 5 ảnh PNG ngay dưới `tmp` | 14,69 MiB | Có ảnh gốc, ảnh before/after, mask và số đo chất lượng. Chưa có bằng chứng bạn đã duyệt hết; giữ để xác nhận đã kết thúc kiểm thử. Không bao gồm log CI mới nhất. |
| D | `_photo_plan`, phần còn lại của `_tu-dong-in-fix` | khoảng 3,66 MiB | Script tạo ảnh thẻ, contact sheet và chương trình in tự động. Đầu ra PDF mà script ảnh thẻ tham chiếu hiện không còn trong workspace; chưa thể kết luận tác vụ đã hoàn tất hay script không còn được dùng. Giữ cả lock của AutoPrint để bạn xác nhận. |
| E | 8 tài liệu `docs/planning/GIAO_CA_*.md` và `docs/develop-grading/codex-plan.md` | 0,09 MiB | Nhiều bước đã hoàn tất hoặc bị thay thế, nhưng còn quyết định, cách tái lập và việc chờ đánh giá. Đề xuất chỉ xóa sau khi đồng ý bỏ tài liệu giao ca lịch sử; cần chỉnh các liên kết tham chiếu cùng lần xóa. Danh sách chính xác ở dưới. |
| F | `dist/camera_profiles` | khoảng 1,03 MiB | Bộ profile khác nội dung với `dist/iAi-portable/camera_profiles` và `target/release/camera_profiles`; còn camera riêng như EOS 700D, Sony ILCE-7RM2. Đề xuất giữ, không coi là bản trùng. |
| G | `tmp/body-parsing` | 31,31 MiB | ONNX và bản TFLite đầu vào dùng chuyển đổi/đối chiếu. Model dùng trong app đã có riêng, nhưng TFLite còn giá trị tái kiểm chứng. Giữ chờ quyết định có cần chuỗi xuất model. |

Nhóm E gồm:

- `docs/planning/GIAO_CA_CODEX_RAW_M1_FIX_2026-08-25.md`
- `docs/planning/GIAO_CA_GPU_DETAIL_2026-08-28.md`
- `docs/planning/GIAO_CA_Q0_BASELINE_2026-08-26.md`
- `docs/planning/GIAO_CA_Q1_ART_AB_HARNESS_2026-08-27.md`
- `docs/planning/GIAO_CA_Q1_SENSOR_AUDIT_2026-08-26.md`
- `docs/planning/GIAO_CA_Q5_SLIDER_CONTRACT_2026-08-26.md`
- `docs/planning/GIAO_CA_Q6_DETAIL_NR_2026-08-26.md`
- `docs/planning/GIAO_CA_Q7_CMS_PARITY_2026-08-26.md`
- `docs/develop-grading/codex-plan.md`

## Những phần đã làm xong và những phần còn lại

Đây là đối chiếu tài liệu tiến độ và Git hiện có, không phải một vòng nghiệm thu lại toàn bộ tính năng.

- **Bản vá con trỏ và panel editor:** đã có trong `a8db445` và portable hiện tại. BUILD_INFO ghi build/check và các test hồi quy đã đạt; tương tác chuột native vẫn chưa được xác nhận thủ công trong hồ sơ này.
- **Offline Auto Retouch:** đã tích hợp vào `861c4bb`, có manifest và 9 model ONNX, hướng dẫn kiểm thử màu và mask. Giữ `README_TEST.txt`, kế hoạch retouch và tài liệu model vì vẫn dùng để nghiệm thu/duy trì; chưa đủ bằng chứng kết luận mọi giai đoạn kế hoạch đều xong.
- **Develop3:** kế hoạch cập nhật 29/08 ghi phần code cốt lõi, GPU Detail viewport, migration Develop2 và các gate tự động đã hoàn tất. Còn GUI CMS hai màn hình, blind review ART, GPU exact khi fit ảnh rất lớn và công việc phát hành/profile. Vì vậy giữ kế hoạch Develop và RAM/RAW.
- **Soạn thảo văn bản:** MVP, trộn thư, định dạng và ảnh nổi đã hoàn tất theo kế hoạch và lịch sử commit. Kế hoạch vẫn ghi Square wrap chưa làm, cùng các tùy chọn bảng, đầu/chân trang, DOCX. Giữ kế hoạch văn bản.
- **Vector/foundation:** đã có tính năng và các quyết định kiến trúc được đóng băng, nhưng kế hoạch gốc còn mô tả quá khứ và định hướng chưa được nghiệm thu toàn bộ. Giữ kế hoạch vector, ADR và FOUNDATION_FREEZE để không mất hợp đồng kiến trúc.
- **`docs/develop2` không phải mã engine dư:** tài liệu bên trong đã cập nhật migration Develop2 → Develop3 và hợp đồng graph hiện hành. Giữ mặc dù tên thư mục cũ.

## Xác minh sau dọn

- Cả 9 đường dẫn đã dọn không còn tồn tại.
- Git vẫn sạch sau khi xóa cache; chỉ báo cáo này được thêm mới sau đó.
- Tiến trình `iai-retouch-ui` PID 43716 vẫn chạy từ `dist/iAi-portable`.
- SHA-256 của portable và `target/release/iai.exe` trước/sau dọn đều là `E78987C26844FEEDFF40C351A4C2C35B2E3665195DEFDE0FA3048208C05B5C2C`.
- Giữ nguyên model runtime, camera profile, executable hiện có, logo, license, cấu hình, source, test và script chính.
- Không chạy lại build/test vì chỉ xóa cache sinh tự động; chạy lại sẽ tạo lại phần vừa dọn.

Bạn có thể duyệt bằng mã nhóm, ví dụ: **“Xóa A và E; giữ các nhóm khác.”**
