# Handoff — Phase 6 large RAW performance

Ngày: 2026-08-09  
Workspace: `C:\Users\Admin\Documents\IAI`

## Trạng thái hiện tại

Phase 0–5 đã làm xong và được người dùng nghiệm thu theo từng phase. Phase 6
chưa đóng vì preview ảnh RAW lớn vẫn lag khi kéo slider.

Người dùng vừa xác nhận:

- Interactive preview → CPU refine không còn nhảy màu.
- Preview/refine → Open Image không còn lỗi màu đã báo trước đó.
- Ảnh lớn vẫn lag khi kéo slider.
- Việc embedded JPEG chờ decode bị thay bằng default RAW look xấu hơn vẫn chưa
  được xử lý.

Color Mixer còn một vấn đề cảm giác/độ mạnh thanh kéo đã được người dùng chủ
động hoãn: hoàn thành pipeline trước rồi quay lại tinh chỉnh.

## Sửa lỗi parity vừa hoàn thành

Nguyên nhân nhảy màu RAW là GPU realtime chỉ upload RGB point curves từ
`DevelopSettings`, bỏ sót `SceneToneData::rgb`. Bảng sau đã gồm camera RGB curve
fit từ embedded JPEG và user RGB curves. Vì vậy camera look biến mất khi GPU
preview bật và quay lại khi CPU refine.

Đã sửa tại:

- `src/gpu/compositor.rs`: scene preview upload `tone.rgb` đã compose.
- `tests/develop_cpu_gpu_parity.rs`: headless GPU test có camera curve không
  trung tính cùng Color Mixer.
- Kết quả actual WGPU: max `1/255`, P99 `1/255` so với commit.

Các thay đổi performance/UI đã có:

- `src/app/actions/develop.rs`: full-resolution bake single-flight; không drop
  receiver của Rayon job cũ; refine chỉ bắt đầu sau khi nhả chuột.
- `src/ui/intent.rs`, `src/app/state.rs`, `src/app/develop_window.rs`,
  `src/app/actions/ui_dialogs.rs`: truyền trạng thái pointer-down.
- `src/ui/widgets.rs`: slider clamp theo visible clip width.
- `src/ui/develop.rs`: footer Reset/Open Image/Cancel có vùng và kích thước cố định.

## Kiểm thử gần nhất

- `cargo test`: 1200 unit tests passed; integration/doc tests passed; các test
  benchmark/GPU thủ công ignored như thiết kế.
- `cargo test --test develop_cpu_gpu_parity headless_gpu_preview_matches_committed_scene -- --ignored --nocapture`:
  max `1/255`, P99 `1/255`.
- `cargo fmt --check`: passed.
- `git diff --check`: passed (chỉ có cảnh báo LF/CRLF).

Chạy ứng dụng bằng:

```powershell
cargo run --release --bin iai
```

## Việc tiếp theo — không hỏi test bằng mắt trước khi code xong

1. Instrument release timings trên đường kéo slider:
   `flush_develop_gpu_preview`, `recomposite`, `build_develop_gpu_preview`,
   proxy build/finish, buffer upload và CPU settled bake.
2. Xác định bottleneck thực tế trên RAW lớn. Không giả định CPU refine là nguyên
   nhân duy nhất: GPU đang upload/recompose texture/proxy và có thể xử lý quá
   nhiều pointer event hơn refresh rate.
3. Coalesce slider changes: tối đa một GPU preview update mỗi display frame và
   luôn dùng settings mới nhất.
4. Với preview tương tác, render theo viewport/mip phù hợp zoom nhưng bắt buộc
   giữ nguyên color/tone math; proxy chỉ được giảm spatial resolution.
5. Cache scene texture, LUT và buffer; chỉ upload phần thực sự thay đổi.
6. CPU refine tiếp tục single-flight, chỉ chạy sau pointer release và không được
   tranh tài nguyên với drag.
7. Thêm benchmark/regression cho RAW lớn trước khi xin người dùng test bằng mắt.
8. Sau khi lag đạt, xử lý default RAW look so với embedded JPEG bằng camera
   profile/DCP hoặc phương pháp fit tốt hơn; histogram theo từng kênh hiện không
   đủ để tái tạo picture style.

## Nguyên tắc làm việc người dùng yêu cầu

- Hoàn thành trọn một phase rồi mới báo test; không dừng giữa phase.
- Chỉ dừng khi thực sự cần kiểm bằng mắt và phải đưa danh sách từng bước test.
- Không đánh dấu Phase 6 hoàn thành chỉ vì automated tests qua; lag ảnh lớn là
  acceptance blocker.
- Bảo toàn toàn bộ dirty working tree hiện tại; đây là công việc của các phase
  trước, không reset hoặc xóa thay đổi.

