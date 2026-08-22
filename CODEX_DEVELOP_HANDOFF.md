# Develop Engine 2 — Handoff cho Claude

> Đây là tài liệu giao ca hiện hành duy nhất cho phần việc Develop Engine 2 còn lại. Không dùng lại checklist cũ để làm lại các phase đã được nghiệm thu.

- **Cập nhật:** 2026-08-19 (bổ sung: khử nhiễu tone-adaptive; output-sharpen; Defringe ẩn)
- **Trạng thái:** phần đã nghiệm thu vẫn **OK**; 2 feature mới (output-sharpen `757a040`, denoise `7bdd843`) **chờ owner test GUI**
- **Repo MAIN:** `C:\Users\Admin\Documents\IAI_DEVELOP_ENGINE_2`
- **Branch:** `codex/develop-engine-2-implementation`
- **HEAD hiện tại:** `7bdd843` trên nền đã nghiệm thu `61856b8`
- **Implementation/report baseline:** `61856b8`
- **Remote:** chưa cấu hình; chỉ commit local, không push

> **Bổ sung 2026-08-19 (Claude) — hướng "nâng chất lượng ảnh RAW":**
> 1. **Defringe** (`eb564f6`): owner GUI-test **KHÔNG OK**. Đã **ẩn slider** (`43035f4`)
>    vì thiếu ảnh mẫu có viền để tune (cùng blocker với bug 7.C). Field/thuật toán/test
>    GIỮ LẠI để bật lại khi có sample. Đừng tự tune mù.
> 2. **Output sizing & sharpening khi xuất** (`757a040`): thêm vào hộp thoại Export
>    (định dạng ảnh raster) tùy chọn "Resize (longest side)" + "Output sharpening"
>    (Off/Low/Standard/High). Xuất = flatten → thu nhỏ Lanczos → làm nét ở cỡ cuối.
>    Mặc định TẮT ⇒ mọi export cũ + iai/PDF/SVG **byte-identical**. Bản resize/sharpen
>    ghi 8-bit. Engine `src/core/output_sharpen.rs`; wiring `formats/mod.rs`
>    `prepare_output_canvas`; test `output_sharpen::tests` + `formats::output_prep_tests`.
>    Các gate xanh (lib 1369/golden 4/parity 3). **Chờ owner GUI-test.**
> 3. **Khử nhiễu tone-adaptive (chuyên nghiệp)** (`7bdd843`): nâng cấp 2 slider
>    Noise Reduction + Color Noise Reduction sẵn có — ngưỡng garrote luma + attenuation
>    chroma nay nhân theo "shadow weight" (per-pixel theo ĐỘ SÁNG cục bộ, không phải
>    sigma đo được) ⇒ khử hạt/blotch VÙNG TỐI mạnh hơn, highlight + cạnh giữ nguyên.
>    Keyed theo brightness ⇒ **resolution-invariant, preview khớp commit (không nhảy)**.
>    Highlight bit-exact như cũ, default 0 ⇒ default render bất động. CPU-only 0 parity.
>    `src/core/develop/detail.rs::nr_shadow_weight` + 2 khối NR. Test `nr_tests`. Gate
>    lib 1370/golden 4/parity 3. **Chờ owner GUI-test.** (Bước sâu hơn = profiled denoise
>    trên scene-linear RAW master — CHƯA làm vì đụng default look đã nghiệm thu; chờ owner.)
>
> `dist/iai.exe` build lại từ `7bdd843` (chờ test). Bản engine đã nghiệm thu dựng lại
> từ `c9adb0c` nếu rollback. Hai bug phụ thuộc dữ liệu (7.C, HP0917 NEF) và cleanup
> legacy vẫn giữ nguyên trạng thái chờ như dưới.

## 1. Trạng thái giao ca

Các hạng mục sau đã hoàn tất, đã có test tự động, release artifact và đã được owner nghiệm thu GUI:

| Hạng mục | Trạng thái |
|---|---|
| Phase 0–6 | Hoàn tất từ các ca trước |
| Phase 7.A — live RGB histogram, waveform, vectorscope | Hoàn tất và nghiệm thu |
| Phase 7.B — soft proof, gamut warning, intent/BPC/paper-white/ink-black | Hoàn tất và nghiệm thu |
| Bug C — preview Detail + Local không còn nhảy sáng/soft | Hoàn tất và nghiệm thu |
| Temp/Tint UI và calibration consistency | Hoàn tất và nghiệm thu |
| Phase 8 — regression, migration/export roundtrip, CPU/GPU parity, performance, release build | Hoàn tất |

Không làm lại, refactor rộng hoặc đổi hành vi các phần trên nếu không có yêu cầu mới và repro cụ thể.

## 2. Mốc commit đã nghiệm thu

Theo thứ tự:

1. `11e8181` — `feat(develop): show live color scopes`
2. `8eef677` — `feat(develop): preview soft proof and gamut warnings`
3. `01a73ca` — `feat(develop): preview detail and local edits`
4. `0c11bd9` — `test(develop): lock migration and export roundtrips`
5. `61856b8` — `docs: report Develop Engine 2 validation`

Claude phải bắt đầu từ HEAD mới nhất của branch và kiểm tra worktree sạch trước khi sửa.

## 3. Bằng chứng validation đã chốt

- `cargo test --lib`: **1363 passed, 0 failed, 6 ignored**.
- `cargo test --test develop_color_golden`: **4 passed**.
- `cargo test --test develop_cpu_gpu_parity`: **3 passed**, gồm cả headless GPU parity.
- Performance proxy Detail + Local ở 160k pixels: **p95 40.35 ms**; ngưỡng khóa `< 50 ms`.
- Release artifact: `dist/iai.exe`.
- Kích thước: **67,606,016 bytes**.
- SHA-256: `FEF12B253DFA18D401159973E08605AF8D49A04671F268624A3B999051BBB7B9`.
- Owner đã chạy GUI trên artifact và xác nhận **OK** ngày 2026-08-19.

Báo cáo chi tiết: `DEVELOP_ENGINE_2_IMPLEMENTATION_REPORT.md`.

## 4. Phần việc còn lại cho Claude

Hiện không có code task nào nên tự ý triển khai ngay. Hai lỗi còn lại đều phụ thuộc dữ liệu đầu vào của owner; cleanup kiến trúc phụ thuộc quyết định riêng.

### 4.1 Phase 7.C — grain/texture ở cạnh tương phản cao

**Trạng thái:** chờ native crop hoặc source RAW có lỗi.

**Không được tuning mù** demosaic/CA/denoise dựa trên ảnh screenshot thu nhỏ.

Khi owner cung cấp fixture:

1. Lưu fixture theo quy ước test data hiện có; không commit RAW riêng tư nếu chưa được phép.
2. Reproduce bằng đường render thật và biến môi trường `IAI_RAW_LOOK_CROP`.
3. Đo trên crop trước khi đổi thuật toán: edge profile, chroma residual, zipper/maze hoặc metric tương đương.
4. Phân loại đúng nguồn lỗi: demosaic, CA alignment, sharpening hay denoise.
5. Viết regression/golden định lượng trước hoặc cùng patch.
6. Giữ CPU/WGSL parity nếu đường GPU bị ảnh hưởng.
7. Chạy lại golden, parity, lib tests và kiểm tra ảnh crop trước/sau.

Definition of done:

- Có fixture tái hiện được lỗi.
- Metric và crop cho thấy giảm artifact, không làm mềm chi tiết thật.
- Không tạo regression trên golden/parity hiện có.

### 4.2 HP0917 NEF decode-black

**Trạng thái:** chờ chính file `HP0917.NEF` hoặc một fixture tối thiểu tái hiện lỗi.

Khi có file:

1. Ghi nhận metadata và hash; xác nhận quyền lưu/commit fixture.
2. Trace decode qua `rawloader`/`rawler`, black/white level, CFA, active area và normalization.
3. Thêm regression test tái hiện ảnh đen trước khi sửa.
4. Sửa ở lớp decode/normalization đúng nguyên nhân; không thêm ngoại lệ theo filename.
5. Chạy lib tests, RAW golden, CPU/GPU parity và mở GUI xác nhận.

Definition of done:

- NEF render ra dữ liệu ảnh hợp lệ.
- Có regression chống tái phát.
- Không làm thay đổi CR2/NEF fixtures đang pass.

### 4.3 Twin/JPEG-match/legacy cleanup

**Trạng thái:** chưa được phép thực hiện trong ca này.

Chỉ mở việc cleanup khi owner đưa yêu cầu rõ ràng và đủ các điều kiện:

- golden coverage tương ứng đã khóa;
- có bằng chứng adoption đường mới;
- có rollback plan;
- owner xác nhận phạm vi xóa/migration.

Cho tới lúc đó phải giữ legacy renderer, twin/JPEG-match behavior và compatibility của project cũ. Không coi cleanup này là việc “còn sót” cần tự động hoàn thành.

## 5. Trình tự bắt đầu cho Claude

Chạy read-only trước:

```powershell
Set-Location C:\Users\Admin\Documents\IAI_DEVELOP_ENGINE_2
git status --short --branch
git log --oneline -8
Get-Content -Raw CODEX_DEVELOP_HANDOFF.md
Get-Content -Raw DEVELOP_ENGINE_2_IMPLEMENTATION_REPORT.md
```

Nếu chưa có native crop/RAW fixture hoặc quyết định cleanup, dừng ở việc xác nhận blocker; không tạo patch suy đoán.

Khi có input hợp lệ, tạo commit nhỏ theo từng bug, cập nhật tài liệu này và báo cáo validation. Không push vì repo chưa có remote.

Sau mọi thay đổi code, dùng target directory hiện có và chạy tuần tự `-j 1` để tránh OOM:

```powershell
cargo fmt --all -- --check
git diff --check
cargo test --target-dir target/phase1-foundation --locked -j 1 --lib
cargo test --target-dir target/phase1-foundation --locked -j 1 --test develop_color_golden
cargo test --target-dir target/phase1-foundation --locked -j 1 --test develop_cpu_gpu_parity -- --test-threads=1
```

Nếu tạo release mới, đóng app đang chạy trước khi thay `dist/iai.exe`, build với `-j 1`, rồi ghi lại kích thước và SHA-256 mới. Không ghi đè artifact đã nghiệm thu nếu chỉ đang thử nghiệm.

## 6. Quy tắc bất biến

- Chỉ làm trong `C:\Users\Admin\Documents\IAI_DEVELOP_ENGINE_2`.
- Không chạm repo backup `C:\Users\Admin\Documents\IAI`.
- Không sửa `C:\Users\Admin\Documents\IAI_REFERENCE_ONLY`.
- Chỉ thay đổi Develop-mode; vector/text/PDF/scan phải giữ nguyên.
- Giữ clean-room implementation: không sao chép mã proprietary từ phần mềm tham chiếu.
- Measure first, patch second; mọi sửa ảnh phải có repro và kiểm chứng định lượng.
- Nếu sửa RAW render, kiểm tra cả CPU và WGSL/GPU khi có đường tương ứng.
- Không xóa legacy renderer hoặc compatibility path khi chưa đủ điều kiện ở mục 4.3.
- Không push, force-push, reset hard hoặc rewrite history.
- Dùng `apply_patch` cho chỉnh sửa mã/tài liệu thủ công; không dùng PowerShell `Set-Content` để sửa source.
- `DEVELOP_COLOR_ENGINE_RECONSTRUCTION_PLAN.md` và `DEVELOP_ENGINE_2_MASTER_PLAN.md` chỉ cung cấp rationale/kiến trúc; trạng thái và thứ tự việc còn lại lấy theo handoff này.

## 7. Code map cho hai bug dữ liệu-dependent

- `src/formats/raw.rs` — decode, pipeline RAW và scene master.
- `src/app/render/develop_preview.rs` — preview Detail/Local và đường tương tác.
- `src/gpu/compositor.wgsl` + `src/gpu/compositor.rs` — đường GPU/WGSL tương ứng.
- `tests/develop_color_golden.rs` — RAW/color golden và helper metric.
- `tests/develop_cpu_gpu_parity.rs` — khóa parity CPU/GPU.
- Vị trí fixture/decoder adapter phải được xác định bằng `rg` theo convention hiện tại; chỉ thêm fixture khi đã xác nhận quyền lưu.

Tìm symbol bằng `rg` trước khi sửa vì tên/hàm có thể đã thay đổi sau baseline.

## 8. Cảnh báo đã biết, không phải blocker

- Một số warning Rust cũ (`dead_code`, unused fields/methods) vẫn tồn tại.
- WGPU có thể in warning backend trong môi trường headless; parity test vẫn pass.
- Bài performance 240k-pixel từng đo p95 63.36 ms; ngưỡng acceptance được khóa ở proxy 160k-pixel phản ánh workload mục tiêu và đã pass 40.35 ms.

## 9. Input cần xin owner nếu tiếp tục

1. Native crop/source RAW cho artifact cạnh tương phản cao, kèm vị trí crop và settings Develop.
2. File `HP0917.NEF` hoặc fixture tái hiện decode-black.
3. Nếu muốn cleanup: quyết định riêng về twin/JPEG-match/legacy, phạm vi migration và rollback plan.

Không có một trong các input trên thì trạng thái đúng là **đã nghiệm thu phần triển khai hiện tại, chờ dữ liệu/quyết định cho phần còn lại**.
