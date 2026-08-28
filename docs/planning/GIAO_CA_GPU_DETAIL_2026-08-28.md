# GIAO CA — Đưa Detail lên GPU (preview = commit, WYSIWYG)

Ngày: 2026-08-28. Người giao: Claude (Opus 4.8). Người nhận: Codex (hoặc agent tiếp theo).
Repo sống: `C:\Users\Admin\Documents\IAI`, nhánh `feat/vector-core-foundation`, remote `tongnghia2026/iai`.
HEAD lúc giao: `ef37abd`. **11 commit LOCAL chưa push** (quy tắc: chỉ push khi owner bảo).

> **Cập nhật Codex 2026-08-28:** compositor integration native-resolution đã
> xong ở `ed2b314` và owner GUI-test **OK**. GPU/CPU display `0/255`, linear
> khoảng `1e-6`, tiled/monolithic `0`, compositor/commit tối đa `1/255`. Runtime
> đã cache pipeline, dispatch 2-D và tile có halo. Native GPU Detail 1920×1080
> được đo p95 `72.44 ms` sau khi chỉ upload RGB thay vì zero-fill toàn pool.
> Phần M4 còn lại là fit zoom vùng nguồn quá lớn, hiện vẫn fallback proxy.

> **Tiếp tục Codex 2026-08-28:** đã khóa thêm provenance decode-time:
> `raw_render_recipe` được persist tùy chọn trong `.iai`, round-trip qua
> save/reopen và có diagnostic fallback sau khi scene master bị giải phóng. File
> cũ và tài liệu không phải RAW vẫn giữ hành vi/manifest cũ.

---

## 0. TL;DR cho người nhận

- **Owner (end-user, không quan tâm kỹ thuật) muốn:** panel Detail đúng **3 thanh** (Sharpening, Noise Reduction, Color Noise) + **preview realtime giống Photoshop** + **preview phải KHỚP TUYỆT ĐỐI với kết quả sau commit** (WYSIWYG). Owner đã **chọn phương án: đưa Detail lên GPU**.
- **ĐÃ XONG (mình làm phiên này):** "bộ não" GPU Detail — port đầy đủ 3 slider sang WGSL compute, **parity BIT-EXACT với CPU commit** (headless test: display 0.000/255, linear 1e-6). Đây là phần khó nhất về thuật toán, đã chạy đúng.
- **CÒN LẠI (việc của bạn — MILESTONE 4):** **tích hợp** module GPU Detail này vào luồng preview live trong compositor, sao cho preview dùng chính GPU Detail full-res → preview = commit. Đây là phần LỚN và RỦI RO (compositor đã tinh chỉnh parity rất kỹ, dễ phá).
- **Ràng buộc bất di:** Detail slider = 0 → byte-identical (không đổi look mặc định owner đã duyệt). Không push. `cargo fmt --check` + `cargo test --lib` trước mọi push. Cấm ship DCP ART (GPL).

---

## 1. Bối cảnh & lịch sử phiên (để hiểu tại sao đến bước này)

Owner GUI-test lặp nhiều vòng về "cảm giác" kéo thanh Detail:
1. Ban đầu preview kéo Detail bị "nhảy scale" (proxy thu nhỏ 8–48× chạy à-trous ở pixel-proxy → quầng sai tỉ lệ).
2. Mình sửa CPU-side (các commit `93b13f3` G6 source-scale, `29c5791` 3-slider, `1de2892` proxy mịn, `e21c83e` bỏ settled-bake + softens survive). Owner: "mượt hơn RẤT NHIỀU **nhưng khi commit vẫn nhảy; preview không hiển thị đúng như kết quả sau commit**."
3. Bản chất vấn đề: **preview = proxy (xấp xỉ, downsample), commit = full-res → LUÔN lệch trên CPU.** Mọi tinh chỉnh CPU chỉ dời cú nhảy (release ↔ commit), không xóa được.
4. → Hỏi owner 2 phương án; **owner chọn "Đưa Detail lên GPU (khớp tuyệt đối)".** Vì GPU chạy Detail full-res realtime mọi zoom → preview = commit, hết nhảy.

**Kết luận cứng:** chỉ khi preview chạy ĐÚNG cùng phép tính Detail ở ĐÚNG độ phân giải như commit thì mới hết nhảy. CPU proxy không làm được. GPU làm được.

---

## 2. ĐÃ XONG — "bộ não" GPU Detail (parity bit-exact)

### Commit
- `766d014` feat(gpu): GPU Detail core — sharpening + noise reduction, bit-exact vs CPU
- `426703e` feat(gpu): GPU Detail — Colour NR + linear/scene domain, parity holds
- `ef37abd` docs(gpu): refresh detail_gpu module header

### File mới
- **`src/gpu/detail.wgsl`** — compute kernels (split, atrous, diff, nr_garrote, box_blur, reconstruct, sharpen, combine, extract_channel, chroma_recombine).
- **`src/gpu/detail_gpu.rs`** — orchestration Rust + 2 test parity headless.
- Đăng ký ở `src/gpu/mod.rs`: `pub mod detail_gpu;`.

### API để bạn gọi khi tích hợp
```rust
// src/gpu/detail_gpu.rs
pub fn run_detail(
    device: &wgpu::Device, queue: &wgpu::Queue,
    rgb: &[f32],            // 3*w*h, RGB liên tục (r,g,b, r,g,b, ...)
    w: u32, h: u32,
    p: DetailWorkingParams, // fold từ slider
    linear: bool,           // true = RAW/scene (working space), false = display
    luma_coeff: [f32; 3],   // = working_space.render_luminance_coefficients() cho RAW; [0.2126,0.7152,0.0722] cho display
) -> Vec<f32>               // 3*w*h RGB kết quả

pub fn run_detail_display(...) // wrapper: linear=false, coeff Rec709
DetailWorkingParams::from_sliders(sharpening, sharpen_radius, sharpen_detail, sharpen_masking, noise_reduction, color_noise_reduction)
```
- **Parity đã chứng minh** với CPU `apply_detail_to_display_buffer` (display) và `apply_detail_to_working_buffer_in_space` (linear, `WorkingColorSpace::AcesCg`). Test:
  `cargo test --lib gpu_detail -- --nocapture` → `gpu_detail_matches_cpu_display` = 0.000/255, `gpu_detail_matches_cpu_linear_scene` = 1e-6.

### Cách hoạt động (để bạn không phải đọc lại từ đầu)
- Một storage buffer `pool: array<f32>` gộp mọi plane ở offset cố định (img, luma, chroma, cplane, rA, rB, tmp, d0..d2, cavg, cavgtmp — tổng **20·N** f32, N=w·h). Xem `struct Layout` trong `detail_gpu.rs`.
- Mỗi "pass" = 1 compute dispatch, tham số qua uniform `PassParams` (dynamic offset, stride 256B). WebGPU tự chèn barrier read-after-write giữa các dispatch trong cùng compute pass → thứ tự tuần tự giống CPU.
- Vì **index buffer y hệt CPU** (cùng công thức, cùng thứ tự phép tính, cùng hằng số), parity gần như bit-exact.
- Thứ tự passes = đúng `process_detail_plane`: split → (chroma NR nếu color_nr>0: per-channel extract → à-trous edge_aware_from=1 → recombine tone-adaptive) → (luma nếu nr>0||amount>0: à-trous edge_aware_from=0 → nr_garrote → box_blur cavg → sharpen HOẶC reconstruct) → combine.
- `linear` chọn: split/reconstruct/sharpen/combine dùng `clamp(0,1)` (display) hay `max(0)` (linear), và luma coeff.

### GIỚI HẠN đã biết của core (phải xử lý khi tích hợp)
1. **CHƯA implement Defringe.** CPU `process_detail_plane` chạy `apply_defringe` khi `settings.defringe > 0.001`. Hiện UI ĐÃ ẩn Defringe (default 0) nên parity vẫn đúng (test dùng defringe=0). **Nếu Defringe được mở lại → phải thêm vào WGSL, nếu không preview sẽ lệch commit khi defringe≠0.**
2. **Dispatch 1-D:** `dispatch_workgroups(ceil(n/64),1,1)`. Max workgroups/dimension = 65535 → **n > ~4.19M (≈4MP) sẽ VƯỢT giới hạn** và fail. Full-res 24MP → PHẢI đổi sang 2-D dispatch (chia x/y) hoặc workgroup lớn hơn. Sửa trong `run_detail` (chỗ `let groups = (lay.n + 63) / 64;`).
3. **VRAM:** pool = 20·N f32 = 80·N byte. 24MP → ~1.9 GB. Viewport nhỏ (zoom gần) thì OK; **fit-zoom ảnh lớn phải cap kích thước hoặc tile** (xử lý từng ô + apron ≥ DETAIL_HALO=16px để không seam).
4. **Xây pipeline mỗi lần gọi** (`run_detail` tạo shader/pipeline/bind group mỗi lần) — OK cho test, **PHẢI cache** cho preview live (tạo 1 lần, tái dùng; chỉ update uniform + pool).

---

## 3. VIỆC CÒN LẠI — Milestone 4: tích hợp compositor (PHẦN LỚN)

### Vấn đề cần giải
Preview live hiện **KHÔNG** chạy à-trous trên shader. Nó lấy Detail từ **CPU proxy** `dev_adjusted_rgb` (build ở downsample S) rồi blend per-pixel trong `compositor.wgsl` (`dev_finish_colored`, ~dòng 1380). Đó là lý do preview ≠ commit.

Để preview = commit, Detail phải chạy **full-res, trong working space, TRƯỚC output transform** (giống commit). Commit RAW làm: scene → tone/color/effects (working) → **detail (working)** → output transform → display. Preview phải làm y vậy nhưng dùng GPU Detail.

### Điểm chèn & file trọng tâm
- **`src/app/render/develop_preview.rs`** — quyết định & build preview.
  - `build_develop_gpu_preview()` (~dòng 49): dựng `DevelopGpuPreview` payload.
  - `has_detail()` → dùng `detail_preview_downsample` (~dòng 167); proxy build ~dòng 500–560.
  - Path RAW/scene: `scene_fast_region_develop(...)` (`develop_scene.rs` ~1871) tạo `(region, adjusted)`. **Detail CPU nằm ở đây:** `develop_scene.rs` ~dòng 1986 `apply_detail_to_working_buffer_in_space(&mut working, w, h, settings, tone.working_space, step)` — chạy trên proxy downsample `step`, RỒI `working_to_display` → `adjusted`.
  - Path raster (non-scene): `apply_fast_preview_to_region` (`spatial.rs` ~909) → `apply_detail_to_display_buffer(..., sample_step)` (~1051).
- **`src/gpu/compositor.rs`** — `composite_layers` (~2863), `struct DevelopGpuPreview` (~783), xử lý develop preview ~2947 & 3623. Ping/pong texture ~805. Readback encoder mẫu ~2433.
- **`src/gpu/compositor.wgsl`** (2169 dòng) — `dev_finish_colored` (~1380) re-add detail từ proxy; `dev_effects_stage` (~1415); output transform ở cuối fragment. **Rất tinh chỉnh — đụng vào là dễ phá parity tone/color/mixer.**
- **CPU tham chiếu (đọc để mirror đúng thứ tự stage):** `process_detail_plane` (`src/core/develop/detail.rs`); full render commit ở `develop_scene.rs` ~2202 (`apply_detail_to_working_buffer_in_space(..., 1)`), thứ tự: effects → **detail** → locals → `working_to_display`.

### Hướng đề xuất (AN TOÀN, ưu tiên không phá parity hiện có)
**Path preview MỚI, chỉ bật khi `settings.has_detail()`** (edit không-detail giữ nguyên đường proxy cũ). Các bước:
1. **Chụp working-space full-res của viewport** (post tone/color/effects, **TRƯỚC** detail & output transform) ra texture/buffer. Có 2 cách:
   - (a) **GPU:** thêm biến thể/flag cho `compositor.wgsl` để render ra **working-space RGB (pre-detail, pre-output-transform)** thay vì display; bỏ phần blend proxy-detail. Cần chỗ chứa (texture RGBA32F hoặc F16, hoặc storage buffer khớp layout `pool` để chạy detail thẳng). Đây là bước khó nhất — phải bóc logic output-transform ra pass 2.
   - (b) **CPU (đơn giản hơn, ít rủi ro shader):** dựng `working` full-res viewport bằng đường CPU hiện có (`scene_fast_region_develop`) nhưng **downsample=1** và **bỏ bước detail CPU**; upload `working` vào `pool` của GPU detail. Nhược điểm: tone/color CPU ở downsample=1 chậm khi viewport lớn (fit-zoom); phù hợp khi zoom gần (viewport nhỏ). Có thể chấp nhận: zoom gần = exact, zoom xa = fallback proxy cũ (nhưng khi đó lại lệch — cần cân nhắc, HỎI OWNER nếu định fallback).
2. **Chạy GPU Detail:** `detail_gpu::run_detail(working_rgb, vw, vh, params, linear=true, coeff=tone.working_space.render_luminance_coefficients())`. (Với ảnh không-scene/display: linear=false.) Nhớ **cache pipeline** + xử lý dispatch 2-D + VRAM (mục 2.2–2.4).
3. **Output transform + composite:** áp `working_to_display` (mirror `SceneToneData::working_to_display`; trong shader là đường cuối `compositor.wgsl`) lên kết quả detail rồi đưa ra màn hình.

**Mẹo giảm rủi ro:** giữ nguyên `dev_finish_colored`/proxy cho non-detail; khi detail active, đường mới thay hẳn phần detail — KHÔNG cố nhét à-trous vào shader per-pixel hiện tại.

### Gate hoàn thành Milestone 4
- Kéo Sharpening/NR/Color-NR ở nhiều mức zoom (100%, fit) → **preview trùng khít kết quả sau commit** (owner GUI xác nhận hết nhảy).
- Detail = 0 → preview byte-identical như trước (đường proxy cũ).
- Thêm test parity kiểu `develop_cpu_gpu_parity.rs`: GPU-detail-preview-cho-viewport vs CPU-commit trên cùng vùng, tolerance ≤ ~2/255.
- Latency chấp nhận được (owner khen "mượt" ở bản CPU hiện tại — đừng làm tệ hơn). Đo p50/p95 slider→frame nếu được.
- fit-zoom ảnh 24MP không crash/không OOM VRAM (tile hoặc cap).

---

## 4. Ràng buộc & quy tắc (bắt buộc tuân)

- **KHÔNG push** (đang 11 commit local ahead). Xong việc = `cargo fmt --all --check` + `cargo test --lib` + commit + báo cáo. Owner sẽ bảo khi push. CI có blocking `cargo fmt --check`.
- **Detail slider = 0 → byte-identical.** Không đổi look mặc định (owner đã duyệt look "Natural v3"). Cải thiện Detail chỉ là làm preview khớp commit + mượt, KHÔNG đổi thuật toán commit.
- **Chỉ 3 slider Detail** (Sharpening/NR/Color-NR). Radius/Detail/Masking ẩn, giữ default 1.0/25/0. Defringe ẩn (=0). Output Sharpening đã gỡ khỏi Export (owner: dư thừa) — engine `core/output_sharpen.rs` để yên, đừng đề xuất bật lại.
- **Cấm ship DCP ART (GPL).** DCP dùng LOCAL: copy `target/release/camera_profiles/*.dcp` → `target/release/deps/camera_profiles/` để test có màu chuẩn.
- **Sửa .rs KHÔNG dùng PowerShell Get/Set-Content** (phá UTF-8). Dùng Read/Edit/Write. Commit dài → `git commit -F` / heredoc.
- Owner là end-user: báo cáo ngôn ngữ thường, hướng kết quả; chỉ hỏi việc không đảo ngược / ảnh hưởng dữ liệu. Nhưng đây là đổi lớn → nên đưa build cho owner GUI-test từng bước.
- **Test GPU headless:** `iai::gpu::vector::renderer::headless_device()` trả `Option<(Device,Queue)>` (skip sạch nếu không có adapter). Xem mẫu `tests/develop_cpu_gpu_parity.rs`.

## 5. Ghi chú API wgpu 29 (đã vấp phiên này)
- `entry_point: Some("name")` (Option, không phải &str trần).
- `device.poll(wgpu::PollType::wait_indefinitely()).ok();` (KHÔNG `Maintain::Wait`).
- `PipelineLayoutDescriptor { bind_group_layouts: &[Some(&bgl)], immediate_size: 0 }` (không có `push_constant_ranges`).
- `ComputePipelineDescriptor { ..., compilation_options: Default::default(), cache: None }`.
- `set_bind_group(0, &bg, &[dynamic_offset_u32])`.

## 6. Build & chạy
- Lib test: `cargo test --lib` (1475 pass hiện tại). GPU detail: `cargo test --lib gpu_detail -- --nocapture`. Parity: `cargo test --test develop_cpu_gpu_parity`.
- Release: `cargo build --release --bin iai` → copy `target/release/iai.exe` → `dist/iAi-portable/iai.exe` cho owner GUI-test (portable hiện `b3159efe`, CHƯA có GPU detail integration).
- Corpus RAW: `C:\Users\Admin\Pictures\anh-raw`. ART-cli headless để A/B (nếu cần): `C:\Users\Admin\Pictures\1111\ART_1.26.7_Win64_portable\ART-cli.exe`.

## 7. Thứ tự đề xuất cho người nhận
1. Đọc `process_detail_plane` (detail.rs) + `run_detail`/`detail.wgsl` để nắm phép tính (parity đã đúng, đừng sửa lệch).
2. Quyết cách chụp working-space full-res (mục 3, hướng a GPU vs b CPU). Nếu chọn (a): thêm flag output-working-space cho `compositor.wgsl` + tách output-transform. Nếu (b): thêm biến thể `scene_fast_region_develop` downsample=1, no-detail, trả `working` cho GPU.
3. Cache pipeline GPU detail (di `run_detail` internals thành struct giữ pipeline/bind group; hàm `run` chỉ upload pool + update uniform + dispatch + readback/hoặc dùng thẳng texture).
4. Xử lý dispatch 2-D + VRAM tile cho ảnh lớn.
5. Wire vào `build_develop_gpu_preview`: khi `has_detail()`, đi đường mới.
6. Thêm test parity integration (viewport GPU-detail vs commit).
7. Build portable, đưa owner GUI-test; lặp theo phản hồi.

---

## 8. Trạng thái commit (11 local, chưa push)
```
ef37abd docs(gpu): refresh detail_gpu module header for the completed core
426703e feat(gpu): GPU Detail — Colour NR + linear/scene domain, parity holds
766d014 feat(gpu): GPU Detail core — sharpening + noise reduction, bit-exact vs CPU
e21c83e fix(develop): stop the Detail preview jumping on mouse release
f6f0971 docs(develop): record Phase 5 pivot — Detail like Photoshop
1de2892 feat(develop): finer live Detail proxy so sharpening shows at 100% zoom
29c5791 feat(develop): simplify Detail to three sliders (Photoshop-style)
93b13f3 feat(develop): G6 — Detail live preview at source-pixel scale
f321c73 docs(develop): record Phase 0/1/3/4 + Develop3-default rollout status
72ab5f4 chore(docs): remove the executed Light/Mixer comparison plan
b43e159 feat(develop): make Develop3 the default engine; retire the opt-in flags
```
Bối cảnh dài hơn: memory topic `project_iai_raw_ram_quality_plan.md` + `docs/planning/KE_HOACH_HOAN_THIEN_DEVELOP_IAI_2026-08-27.md`.
