# Kế hoạch hoàn thiện Develop iAI

Ngày lập: 2026-08-27  
Baseline mã nguồn: `0c0edb5` (`feat(develop): add opt-in light mixer v3`)  
Baseline chạy thử: `dist/iAi-portable/iai.exe`, Develop3 bật bằng
`IAI_LIGHT_MIXER_V3=1`.

Trạng thái 2026-08-27: **Phase 0 đã được owner kiểm thử toàn bộ bằng mắt và xác
nhận OK**. Implementation gồm engine badge, pipeline provenance tooltip,
Midtones activation cho graph/raster và chống áp Midtones lần hai ở RAW. Toàn bộ
thay đổi Phase 0 hiện đã được stage nhưng **chưa commit** vì phiên Codex bị dừng
giữa bước commit. Người tiếp quản phải chốt Phase 0 trước khi bắt đầu Phase 1.

## Cập nhật tiến độ — 2026-08-27 (đợt Light/Mixer + rollout)

Đã push toàn bộ lên `origin/feat/vector-core-foundation`.

- **Phase 0 (khóa baseline, engine badge, Midtones): ✅ XONG** — commit `2d515be`, owner GUI-OK.
- **Phase 1 (harness A/B ART↔iAi): ✅ XONG** — commit `56cdbbb`; owner chấm "iAI đẹp hơn, ART thật hơn" → **quyết giữ look đẹp làm mặc định**. (Chưa mở rộng corpus §6 — không chặn.)
- **Phase 3 (Light sạch/mượt): ✅ XONG (owner GUI-OK)** — Exposure drag mịn, tone-eq zones khớp reach ART, **Blacks = band scene-linear kiểu ART + fill-light phủ rộng** (`1a0fe0d`, `3e88963`); ease + soft-knee. Công tắc `IAI_BLACK_SPREAD` để tinh chỉnh độ phủ.
- **Phase 4 (Color Mixer liên tục): ✅ code + parity** — guided control planes (Hue/Sat/Lum), owner GUI-OK "Mixer ổn". Parity GPU↔commit có proxy = ≤2/255.
- **Phase 8 bước 2 (Develop3 = mặc định): ✅ XONG** — commit `b43e159`. `default_engine_version()`→Develop3; **gỡ 2 cờ opt-in** `IAI_LIGHT_MIXER_V3` + `IAI_LIGHT_SMOOTH` (smooth-Light giờ luôn bật cho Develop3). Tài liệu/project cũ GIỮ engine đã serialize (thiếu field → Scene1) nên mở lại KHÔNG đổi look. 1472 lib test, parity 3/3.
- **Phase 5 — G6 (Detail preview source-scale, P0): ✅ code + test (chờ owner GUI-OK)** — preview kéo trước đây chạy à-trous bán kính pixel-proxy trên proxy giảm 8–48× → Sharpen/NR "sai scale" (quầng rộng 8–192px nguồn) rồi nhảy về nét mịn khi thả (bước nhảy scale). Fix: luồng preview (`spatial.rs` raster + `develop_scene.rs` scene) truyền hệ số downsample vào Detail; `preview_level_survive(S)=min(2^l/S,1)` co từng tầng wavelet về pixel-nguồn (tầng mịn nhất — quầng giả xấu nhất — bị co mạnh nhất). **KHÔNG đụng luồng commit/Apply** (`preview_scale=1` → nhân 1.0 → byte-identical; slider=0 vẫn early-return). Chỉ preview lúc kéo bớt phóng đại, bám sát bake settled. Test `preview_scale_folds_detail_back_to_source_scale`; 1473 lib test, parity 3/3. Commit `93b13f3`.

- **Phase 5 — ĐỔI HƯỚNG theo owner (2026-08-28): làm Detail "giống Photoshop/PTS", KHÔNG theo split kỹ thuật 3-tầng của kế hoạch gốc.** Owner: *"mấy thứ này khá dư thừa; Detail chỉ có 3 thanh kéo (Sharpening, Noise Reduction, Color Noise); cần preview REALTIME giống PTS; tập trung UX, kỹ thuật ko quan tâm; đọc code ART tham khảo."* Đã làm (LOCAL, chờ GUI):
  - `29c5791` **Detail = đúng 3 thanh**: ẩn Sharpen Radius/Detail/Masking (giữ default 1.0/25/0 = combo Lightroom → look ko đổi), Defringe ẩn sẵn; **Texture+Definition dời sang mục Effects**; **gỡ Output Sharpening khỏi Export dialog** (owner thấy dư thừa; `export_output_sharpen`=0, engine `core/output_sharpen.rs` để yên).
  - `1de2892` **preview realtime**: bỏ sàn 8× downsample cho path Detail (min→1; budget pixel vẫn chặn cost) → zoom gần crop nhỏ ⇒ proxy mịn gần 1:1 ⇒ thấy Sharpen/NR THẬT realtime, khớp settled bake (kết hợp G6); zoom xa vẫn coarse; fast-preview thường giữ sàn 8×. Throttle Detail giữ (proxy nhỏ ~107k px ≈ 40fps).
  - **CÒN (nếu owner muốn realtime hơn nữa mọi zoom):** đưa Detail lên GPU (WGSL à-trous — hiện Detail CPU-only). Tier-split capture/creative + noise-aware sharpen của kế hoạch gốc TẠM GÁC (phụ thuộc Phase 2 profile, và owner muốn UX đơn giản hơn là kỹ thuật). `dist/iAi-portable/iai.exe` sha `17fc4038…`.

## Cập nhật tiến độ — 2026-08-28 (GPU Detail GUI-OK + performance gate)

- **Phase 5 — GPU Detail native viewport: ✅ owner GUI-OK** — commit `ed2b314` nối
  đúng ba slider Detail vào preview native-resolution: RAW chạy trong working
  space trước Local/output transform; raster chạy đúng boundary commit. Shader
  dùng trực tiếp plane native thay vì tái tổ hợp proxy delta. Detail = 0 giữ
  nguyên đường cũ. Core có pipeline cache, dispatch 2-D và tiling 16 px không seam.
- **Parity khóa lại:** GPU/CPU display `0/255`; linear scene khoảng `1e-6`;
  tiled/monolithic `0`; native compositor/commit tối đa `1/255`.
- **Phase 6 — số đo trên máy owner:** Develop shader 1920×1080 p95 `7.86 ms`;
  CPU Detail+Local proxy 160k p95 `48.60 ms`; native GPU Detail 1920×1080 ban
  đầu p95 `149.76 ms`, sau khi bỏ upload pool zero `20·N` và chỉ upload RGB
  `3·N` còn p95 `72.44 ms` (gate `<100 ms`). Probe nằm trong
  `tests/perf_develop.rs` và vẫn `#[ignore]` vì phụ thuộc phần cứng.
- **Phase 7 phải sửa trạng thái cũ:** monitor ICC, soft-proof, gamut warning và
  display/export parity đã có; guard `cms_display_parity_probe` đã chứng minh
  đường LUT/LCMS đúng. Gap còn lại chỉ là tự chọn/refresh ICC theo cửa sổ khi
  chuyển giữa nhiều màn hình.
- **Ưu tiên owner:** tiếp tục từ dễ tới khó. Chuẩn hóa profile hợp pháp để phát
  hành được chuyển xuống cuối vì hiện chỉ dùng cá nhân, chưa công khai. DCP ART
  vẫn chỉ được dùng local và tuyệt đối không đưa vào bản phát hành công khai.

**CÒN LẠI:** hoàn tất blind-review ART trên corpus/recipe khóa (đặc biệt Mixer
từng band + Detail crop 100%); GPU-exact cho fit zoom ảnh lớn (hiện native
viewport exact, vùng quá lớn fallback proxy); persist `raw_render_recipe` và
đóng băng look/migration; Phase 8 bước 3–5 (bug corpus → recipe freeze → chỉ dọn
Legacy/Scene1 sau khi có migration); CMS per-window multi-monitor; các phần
capture/creative/output Detail nâng cao chỉ quay lại nếu UX thực tế cần. Phase 2
canonical/licensed camera-profile package làm sau cùng theo quyết định owner.

---

## 0. Handoff cho Claude

### 0.1 Trạng thái repository phải giữ nguyên

- Workspace: `C:\Users\Admin\Documents\IAI`.
- Branch: `feat/vector-core-foundation`.
- HEAD hiện tại: `0c0edb5` — `feat(develop): add opt-in light mixer v3`.
- Phase 0 gồm 12 file đang nằm trong Git index; không reset, checkout hoặc bỏ
  các thay đổi này.
- Trước khi làm tiếp, chạy `git status --short` và `git diff --cached --stat`.
- Nếu index đúng với danh sách bên dưới, commit riêng Phase 0 với message đề nghị:
  `fix(develop): expose engine diagnostics and activate midtones`.
- Không gộp code Phase 1 vào commit Phase 0.

Các file Phase 0 đang staged:

- `docs/planning/KE_HOACH_HOAN_THIEN_DEVELOP_IAI_2026-08-27.md`
- `src/app/actions/ui_data.rs`
- `src/core/develop/pipeline.rs`
- `src/core/develop/settings.rs`
- `src/core/develop/tests.rs`
- `src/core/develop/tone.rs`
- `src/core/develop2/mod.rs`
- `src/core/develop_scene.rs`
- `src/ui/develop.rs`
- `src/ui/viewmodel.rs`
- `tests/develop_cpu_gpu_parity.rs`
- `tests/raw_q0_baseline_probe.rs`

### 0.2 Phase 0 đã được kiểm tra

- `cargo fmt --all -- --check`: pass.
- `cargo test --lib`: 1.466 pass, 0 fail, 6 ignored.
- `cargo test --test develop_cpu_gpu_parity`: 3/3 pass, có Midtones Develop3.
- `cargo test --test raw_q0_baseline_probe --no-run`: compile pass.
- `cargo build --release --bin iai`: pass.
- Portable đã copy vào `dist/iAi-portable/iai.exe`.
- SHA-256 portable: `B606AFF3AA67F074C6C1ABDE14BBD6DB58B705C1432B82D79AACAB3BFA26F29D`.
- Owner đã test RAW, raster, badge/tooltip, Midtones và Apply; toàn bộ OK.
- Launcher test: `dist/iAi-portable/Chay-Develop3-Test.bat`.

Hai warning `f32` tại `src/ui/library.rs:220` và `:222`, cùng unused import test
trong `src/app/text_ops.rs`, là warning có sẵn và không thuộc Phase 0.

### 0.3 Việc Claude làm tiếp ngay: Phase 1

Mục tiêu Phase 1 là tạo bằng chứng A/B ART–iAI có thể tái lập, **chưa tuning Light,
Mixer hoặc Detail**. Thứ tự bắt buộc:

1. Chốt commit Phase 0 như mục 0.1.
2. Audit `tests/raw_q0_baseline_probe.rs` và artifacts hiện có trong `target/q0`.
3. Khóa một manifest A/B ghi tối thiểu:
   - file/camera;
   - crop và kích thước output;
   - WB/input profile;
   - Develop engine và RAW recipe;
   - working/output profile;
   - recipe slider;
   - hash của input/reference.
4. Hoàn thiện harness để tạo:
   - báo cáo reference thiếu/thừa;
   - ảnh iAI và ART cùng kích thước/alignment;
   - contact sheet side-by-side có nhãn nhưng có thêm bản blind không hiện engine;
   - metric tổng và metric theo shadow/midtone/highlight;
   - CSV/JSON provenance đầy đủ;
   - không chỉ dùng một con số mean encoded-channel delta.
5. ART source nằm tại `C:\Users\Admin\Pictures\1111\ART`; hiện chưa có executable
   build sẵn. Chỉ dùng ART như black-box oracle. Không copy/dịch code, hằng số,
   LUT, profile hoặc asset GPL vào iAI.
6. Nếu build ART trên Windows không khả thi vì thiếu dependency, không thay thế
   ART bằng mô phỏng. Hoàn thiện harness và ghi chính xác lệnh/tên file reference
   để owner export từ ART thủ công.
7. Khi đã có ít nhất các cặp neutral + Exposure + Light-zone + Mixer + Detail,
   sinh contact sheet rồi **dừng và yêu cầu owner chấm bằng mắt**. Không chuyển
   sang Phase 2 hoặc tuning look trước khi owner trả kết quả.

### 0.4 Những điều chưa được phép dọn hoặc đổi

- Không xóa `dist/camera_profiles` hoặc `dist/iAi-portable/camera_profiles`: hai
  bộ khác nội dung và có profile duy nhất; việc hợp nhất thuộc Phase 2.
- Không đổi Develop3 thành mặc định trước cổng rollout ở Phase 8.
- Không sửa constants Light/Mixer/Detail để “trông giống ART” trước khi Phase 1
  có corpus và contact sheet khóa.
- Không ghi đè baseline/golden mà không lưu recipe, engine/profile và lý do.

## 1. Mục tiêu

Hoàn thiện Develop theo hướng:

1. Phản hồi Light, Color Mixer và Detail mượt, dễ dự đoán và không tạo màu giả.
2. Preview đang kéo và kết quả sau Apply có cùng ý nghĩa hình ảnh ở mọi mức zoom.
3. RAW có nền màu, nhiễu và chi tiết đủ ổn định trước khi đi vào các slider sáng tạo.
4. Màu trên canvas, export và màn hình được quản lý bằng một hợp đồng CMS rõ ràng.
5. Mọi thay đổi look được đo trên corpus cố định và so sánh black-box với ART;
   không sao chép code, hằng số, profile hay tài sản có giấy phép không tương thích.
6. Có thể rollout từng phần, giữ khả năng mở tài liệu cũ và rollback bằng engine version.

## 2. Những gì đã có

- Master RAW tuyến tính dải rộng, giữ headroom đến biên output.
- Develop2/Develop3 có engine version và đường tương thích tài liệu cũ.
- Develop3 đã có Midtones, tone-zone mới, highlight chroma roll-off và guided
  control planes cho Color Mixer.
- Color Mixer làm việc trong không gian perceptual, có neutral/chroma guard.
- CPU/GPU parity đã có test headless.
- Detail có wavelet ba mức, luminance NR, chroma NR, masking và sharpen.
- Q0 có corpus RAW thực, neutral/default-look render, slider sweep và contact sheet.
- Release tại baseline qua 1.462 unit test; parity integration qua 3/3 test.

## 3. Bảng thiếu sót và mức ưu tiên

| ID | Thiếu sót | Hậu quả nhìn thấy | Mức ưu tiên |
|---|---|---|---|
| G0 | Develop3 còn opt-in và project cũ có thể ghim engine cũ | Dễ test nhầm Develop2, kết luận sai về code mới | P0 |
| G1 | Chưa có A/B ART trên cùng RAW và cùng recipe | Test kỹ thuật đạt nhưng chưa biết hình có đẹp hơn | P0 |
| G2 | Hai bộ `camera_profiles` khác nhau và chưa có manifest chuẩn | Màu thay đổi theo nơi chạy; khó tái lập | P0 |
| G3 | Camera characterization còn ít; sensor metadata/correction thiếu | Màu da, highlight và shadow noise không ổn định | P1 |
| G4 | Light dùng tổng tone-zone Gaussian và regional-E đơn giản | Chuyển vùng có thể cục bộ, gắt hoặc tạo halo | P1 |
| G5 | Mixer vẫn là tám band cố định | Khó chọn dải hue hẹp/rộng; vùng màu phản ứng không đều | P1 |
| G6 | Detail preview chạy cùng bán kính pixel trên proxy giảm 8–48 lần | Lúc kéo và lúc thả xử lý khác scale vật lý | P0 |
| G7 | Capture NR/detail, creative detail và output sharpen chưa tách rõ | Dễ sharpen noise hoặc làm ảnh nhựa | P1 |
| G8 | Preview/proxy có throttle dài và có thể dùng geometry cũ | Kéo slider/zoom có độ trễ và bước nhảy | P1 |
| G9 | Chưa có monitor ICC/soft-proof hoàn chỉnh trong Develop | Canvas và export có thể không giống nhau | P2 |
| G10 | Midtones chưa xuất hiện trong mọi active/signature path | Một số raster/graph path có thể coi edit là no-op | P0 |
| G11 | Chưa có release gate định lượng về chất lượng chủ quan | Dễ chỉnh constants theo một vài ảnh | P0 |

## 4. Nguyên tắc kiến trúc bắt buộc

1. Chỉ một scene master tuyến tính; không bake sRGB/gamma giữa pipeline.
2. Chỉ một output transform tại biên hiển thị/export.
3. Một slider phải có cùng ý nghĩa giữa preview, full quality và Apply.
4. Bán kính không gian được khai báo theo pixel nguồn hoặc đơn vị ảnh, không theo
   pixel proxy không quy đổi.
5. Tone edit bảo toàn neutral; color edit không tự ý đổi luminance ngoài hợp đồng.
6. Không hard-code “taste” để bù lỗi profile hoặc demosaic.
7. Mỗi thay đổi làm đổi look phải tăng recipe/engine version hoặc có migration rõ ràng.
8. ART chỉ là black-box oracle. Không dịch hoặc sao chép implementation GPL.
9. Mỗi phase có commit riêng, golden output riêng và đường rollback riêng.

## 5. Phase 0 — Khóa baseline và tránh test nhầm engine

### Việc làm

- Hiển thị engine đang dùng trong Develop diagnostics/status: Scene1, Develop2
  hoặc Develop3.
- Ghi engine version, RAW recipe, profile camera và output transform vào log Q0.
- Sửa toàn bộ active/signature path để Midtones-only edit luôn kích hoạt ToneZones.
- Thêm launcher A/B rõ ràng:
  - Develop2 baseline;
  - Develop3 candidate.
- Không tự động đổi engine của preset/tài liệu cũ.

### File trọng tâm

- `src/core/develop/settings.rs`
- `src/core/develop/pipeline.rs`
- `src/core/develop2/mod.rs`
- `src/ui/develop.rs`
- `src/app/render/develop_preview.rs`

### Gate hoàn thành

- Mỗi ảnh test cho biết chính xác engine/profile/recipe đang chạy.
- Midtones-only có hiệu lực ở RAW, raster, preview GPU và Apply CPU.
- Direct executable và launcher cho ra đúng engine mong đợi.

## 6. Phase 1 — Bộ A/B ART và tiêu chí thị giác

### Corpus tối thiểu

- Giữ 20 RAW hiện có.
- Bổ sung:
  - ColorChecker daylight và tungsten;
  - chân dung da sáng, da trung bình và da tối;
  - ảnh sân khấu LED đỏ/xanh;
  - phong cảnh lá cây và trời chuyển sắc;
  - high-key váy trắng;
  - low-key ISO cao;
  - ảnh có specular highlight;
  - ít nhất một X-Trans thật.

### Quy trình black-box

1. Build ART độc lập.
2. Khóa input profile, WB, crop, kích thước và output profile.
3. Render neutral và các recipe slider chuẩn từ cả ART và iAI.
4. Không yêu cầu byte parity; đo khác biệt và duyệt blind contact sheet.
5. Lưu provenance của RAW và profile; không đưa asset không rõ giấy phép vào repo.

### Recipe chuẩn

- Exposure: `-2, -1, +1, +2 EV`.
- Mỗi Light zone: `-100, -50, +50, +100`.
- Mixer Hue/Sat/Lum cho red, orange, green, aqua, blue ở ba mức.
- Detail/Sharpen/NR ở 25%, 50%, 75%.
- Tổ hợp stress: lift shadow + warm WB + tăng orange saturation + highlight recovery.

### Chỉ số

- Neutral drift và hue drift trong OKLab/JzAzBz.
- Clip %, gamut compression %, highlight chroma continuity.
- Halo score hai phía cạnh sáng–tối.
- Banding/gradient derivative continuity.
- Acutance, overshoot, noise residual và chroma bleed.
- Preview-versus-commit mean/max delta.
- Blind review theo cặp, không hiện tên engine.

### Gate hoàn thành

- Mỗi thay đổi look có trước/sau trên cùng corpus.
- Có `q0_art_pairing.csv` và contact sheet ART–iAI.
- Không merge tuning chỉ dựa trên synthetic unit test.

## 7. Phase 2 — Chuẩn hóa camera profile và RAW foundation

### Việc làm

- Chọn một thư mục profile canonical có manifest, version và checksum.
- So sánh, kiểm định và hợp nhất có chủ đích hai bộ profile hiện tại; không chọn
  profile chỉ theo tên file.
- Kiểm tra DCP illuminants, ColorMatrix, ForwardMatrix, HueSatMap, LookTable và
  tone curve trước khi đưa vào package.
- Mở rộng profile cho các camera có trong corpus trước, không chạy theo số lượng.
- Chuẩn hóa Make/Model alias và ghi profile-resolution trace.
- Bổ sung sensor plan có điều kiện metadata:
  - black/white level theo kênh;
  - masked/optical black nếu decoder cung cấp;
  - defect pixel;
  - green equilibration;
  - gain map/lens shading;
  - PDAF correction;
  - highlight reconstruction;
  - noise model theo ISO khi đủ dữ liệu.
- Xây policy demosaic theo CFA, kích thước, ISO và chất lượng yêu cầu.
- Không dùng embedded-JPEG fit để che lỗi matrix/profile; chỉ dùng như fallback có log.

### Gate hoàn thành

- Cùng RAW cho cùng scene master bất kể chạy từ `target/release` hay portable.
- ColorChecker profile-backed đạt ngưỡng ΔE đã khóa theo từng camera/illuminant.
- Không profile: fallback được ghi rõ và không tạo màu neon/neutral cast.
- Package profile tái lập được từ manifest.

## 8. Phase 3 — Light Engine sạch và mượt

### Thiết kế đề xuất

- Giữ UI năm vùng nhưng biên dịch thành một control field liên tục trên log-EV.
- Tách ba lớp:
  1. global exposure/contrast;
  2. edge-aware illumination field;
  3. residual detail không bị tone field nuốt.
- Dùng basis có tổng trọng số được chuẩn hóa để nhiều slider không cộng gain quá mức.
- Regularization phải scale-aware và có giới hạn halo.
- Highlight reconstruction/chroma roll-off xảy ra trước output gamut mapping, không
  dựa trên RGB đã clip.
- Giữ hue của vùng sáng màu và neutral axis trong shadow.
- Tính regional field một lần và chia sẻ CPU/GPU/preview khi có thể.

### Không làm

- Không tiếp tục chỉ đổi centre/width/gain bằng mắt trên vài ảnh.
- Không thêm local contrast để che tone transition gắt.
- Không clamp từng kênh trước khi hoàn tất highlight/chroma handling.

### Gate hoàn thành

- Ramp log-luminance có đạo hàm liên tục khi từng slider đi qua 0.
- Không có bright/dark halo nhìn thấy ở cạnh stress chuẩn.
- Tổ hợp năm slider vẫn monotonic và không sinh NaN/negative invalid.
- Hue drift vùng highlight màu thấp hơn baseline Develop3.
- Blind review thắng hoặc hòa ART ở đa số Light recipe đã khóa.

## 9. Phase 4 — Color Mixer liên tục

### Thiết kế đề xuất

- Giữ tám nút UI để dễ dùng nhưng lưu/biên dịch thành ba curve tuần hoàn liên tục:
  Hue, Saturation và Luminance.
- Cho phép điều chỉnh width/feather hoặc thêm điểm điều khiển nâng cao; không khóa
  mọi ảnh vào đúng tám tâm swatch.
- Classification và mutation dùng cùng scene-working perceptual value.
- Saturation response phụ thuộc chroma hiện tại, có neutral confidence liên tục.
- Hue rotation phải giảm mềm gần neutral, shadow noise floor và highlight gamut hull.
- Luminance control có spatial support rộng hơn Hue/Sat và bảo toàn chroma tương đối.
- Guided control field scale-aware; không tái dựng chroma detail bằng nhiều lớp gate
  khác nhau giữa preview và commit.
- Gamut mapping cuối phải hue-preserving và có diagnostic về compression.

### Gate hoàn thành

- Hue wheel sweep không có bước gãy tại ranh giới tám band.
- Màu neutral không đổi; màu ít bão hòa vẫn có thể desaturate một cách hữu ích.
- Skin red/orange không bị tách mảng khi kéo mạnh.
- Gradient xanh trời/lá cây không banding hoặc halo.
- CPU/GPU/preview dùng cùng curve và cùng control field.

## 10. Phase 5 — Tách lại Detail pipeline

### Ba tầng bắt buộc

1. **Capture correction** trước creative tone:
   defect/false color, RAW chroma noise, capture sharpening có điều kiện sensor.
2. **Creative detail** trong Develop:
   texture, clarity/local contrast và sharpening có mask.
3. **Output sharpening** sau resize/output intent:
   screen, print hoặc export kích thước cụ thể.

### Việc làm

- Thay fixed threshold bằng noise estimate/noise profile khi có thể.
- Tăng số scale hoặc dùng scale layout theo kích thước nguồn và viewing scale.
- Tách edge, texture và noise confidence; không sharpen sensor floor.
- Thêm deconvolution như một chế độ riêng, không trộn vào Amount hiện tại.
- Chroma NR phải giữ ranh giới màu và không đổi luma.
- Bán kính preview được quy đổi về pixel nguồn; không chạy 1/2/4 proxy pixel như
  thể đó là 1/2/4 source pixel.

### Gate hoàn thành

- Preview 100% và Apply không có bước nhảy scale nhìn thấy.
- Flat patch giảm noise nhưng edge/chữ nhỏ không bệt.
- Không có halo sáng/tối quá ngưỡng đã khóa.
- ISO cao không bị sharpen noise khi Detail tăng nhẹ.
- Output resize khác nhau nhận sharpening phù hợp thay vì một bake cố định.

## 11. Phase 6 — Preview và độ trễ tương tác

### Việc làm

- Định nghĩa ba tier rõ ràng:
  - interactive approximation;
  - settled preview;
  - full-quality Apply.
- Mỗi tier phải khai báo sai số cho phép so với full quality.
- Cache theo settings signature, engine/profile version, viewport và source revision.
- Không tái dùng stale proxy quá thời gian tối đa sau khi gesture dừng.
- Chuyển các pass toàn ảnh nặng sang worker/GPU nhưng không đổi thuật toán.
- Đo latency trên 12/24/45 MP và GPU phổ thông.

### Mục tiêu ban đầu

- Slider-to-frame p50 ≤ 50 ms; p95 ≤ 100 ms ở proxy tương tác.
- Settled preview bắt đầu trong ≤ 150 ms sau pointer release.
- Không có frame cũ xuất hiện sau frame mới.
- Sai số settled preview so với Apply phải nhỏ hơn ngưỡng thị giác đã khóa;
  với Detail cần thêm scale-structure metric, không chỉ RGB delta.

## 12. Phase 7 — CMS và tính nhất quán hiển thị

### Việc làm

- Phân biệt rõ input profile, scene working space, display working space,
  monitor profile và output profile.
- Nạp monitor ICC theo hệ điều hành; cache transform theo profile hash/intent.
- Thêm soft proof, black-point compensation và gamut warning.
- Canvas và export dùng chung rendering intent contract.
- Test màn hình sRGB và wide-gamut; kiểm tra ảnh có embedded ICC.

### Gate hoàn thành

- Canvas screenshot qua monitor transform khớp export mở trong viewer quản lý màu.
- Đổi monitor profile không thay scene master hoặc histogram scene-referred.
- Export ICC được nhúng đúng và round-trip trong sai số đã định.

## 13. Phase 8 — Rollout Develop3 và dọn renderer cũ

### Điều kiện để Develop3 thành mặc định

- Phase 0–6 đạt gate bắt buộc.
- Blind review trên corpus không có blocker về da, highlight, shadow noise hoặc detail.
- Mở/save/reopen preset và project cũ không đổi hình ngoài migration được chấp thuận.
- Có release note và nút rollback engine trong giai đoạn thử nghiệm.

### Trình tự

1. Develop3 opt-in nội bộ.
2. Develop3 default cho session RAW mới, vẫn giữ Develop2 cho tài liệu đã serialize.
3. Thu thập bug corpus và sửa theo recipe có thể tái lập.
4. Đóng băng look, tăng recipe version.
5. Chỉ xóa code Legacy/Scene1 khi không còn document/preset cần mở và đã có công cụ
   migration. Không xóa chỉ vì đường mới “có vẻ ổn”.

## 14. Thứ tự triển khai đề nghị

| Sprint | Công việc | Lý do |
|---|---|---|
| S0 | G0, G10, engine diagnostics | Ngăn test nhầm và no-op Midtones |
| S1 | ART A/B harness + corpus bổ sung | Có thước đo trước khi tuning tiếp |
| S2 | Canonical camera-profile package | Ổn định đầu vào giữa các máy/build |
| S3 | Detail preview source-scale | Sửa lỗi cảm nhận rõ nhất khi kéo |
| S4 | Light control field + regularization | Cải thiện chuyển vùng sáng/tối |
| S5 | Mixer curve liên tục | Cải thiện chọn màu và feather |
| S6 | Capture/creative/output Detail split | Nâng chất lượng chi tiết thực |
| S7 | Preview latency/cache | Hoàn thiện cảm giác tương tác |
| S8 | Monitor/output CMS | Khóa tính nhất quán hiển thị |
| S9 | Develop3 default + migration | Phát hành có kiểm soát |

Không chạy song song S4/S5/S6 trước khi S1 và S2 ổn định; nếu đầu vào/profile thay
đổi giữa lúc tuning thì mọi đánh giá slider sẽ mất giá trị.

## 15. Quy tắc cho mỗi pull request/commit

- Chỉ một mục tiêu hình ảnh hoặc kiến trúc chính.
- Có ảnh/corpus tái lập và recipe trước/sau.
- Có unit test cho invariant, parity test và ít nhất một visual metric.
- `cargo fmt --check`.
- `cargo test --lib`.
- Test integration liên quan, đặc biệt `develop_cpu_gpu_parity`.
- Build release và kiểm tra executable/package hash.
- Ghi rõ engine/recipe version có đổi hay không.
- Ghi rollback: commit, feature flag hoặc engine version.
- Không ghi đè golden nếu chưa giải thích thay đổi hình ảnh.

## 16. Definition of Done tổng thể

Develop được coi là hoàn thiện khi:

1. Người test luôn biết engine/profile/recipe đang dùng.
2. Cùng RAW có kết quả tái lập trên developer build và portable.
3. Preview đang kéo, settled preview và Apply không đổi scale/semantics.
4. Light không halo, không gãy gradient và giữ hue vùng sáng.
5. Mixer phản hồi liên tục qua hue wheel, giữ neutral và không tách mảng da.
6. Detail giảm noise đúng tầng, không sharpen sensor floor và có output sharpening.
7. Canvas/export đúng theo monitor/output ICC.
8. ART A/B và blind review được lưu cho toàn bộ recipe chuẩn.
9. Project/preset cũ mở đúng hoặc có migration/rollback rõ ràng.
10. Không còn feature flag thử nghiệm cần nhớ bằng tay trong bản phát hành chính thức.

## 17. Ghi nhận dọn dẹp ngày 2026-08-27

Đã loại khỏi portable vì không còn được runtime/script tham chiếu:

- `iai_old_delete_me.exe`
- `iai-develop2-backup-20260826.exe`
- `DEVELOP_ENGINE_2_CONTINUATION.md`
- `DEVELOP_ENGINE_2_IMPLEMENTATION_REPORT.md`

Giữ lại có chủ đích:

- `iai.exe`: release Develop3-capable hiện tại.
- `Chay-Develop3-Test.bat`: launcher bật Develop3.
- DirectML và VC runtime DLL: dependency phân phối.
- `LICENSE`, `THIRD_PARTY.md`, `HUONG_DAN.txt`: tài liệu phân phối cần thiết.
- `extension/`: thành phần browser extension hiện hành.
- Hai bộ `camera_profiles`: chưa xóa vì nội dung khác nhau và có profile duy nhất;
  sẽ hợp nhất có kiểm định ở Phase 2 rồi mới dọn bộ staging thừa.
