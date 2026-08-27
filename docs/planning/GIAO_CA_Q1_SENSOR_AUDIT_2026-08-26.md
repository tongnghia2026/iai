# Giao ca — Quality Q1 (bước 1): audit metadata cảm biến (đã chạy thật)

**Ngày:** 2026-08-26
**Repo/nhánh:** `C:\Users\Admin\Documents\IAI` — `feat/vector-core-foundation`
**Kế hoạch gốc:** `KE_HOACH_GIAM_RAM_MO_NHIEU_RAW_2026-08-25.md` (mục Q1)
**Trước đó:** Q0 xong (`GIAO_CA_Q0_BASELINE_2026-08-26.md`) — neutral/default-look/slider baseline đủ.

> Q1 = "sensor preprocessing + normalized RAW master". Việc #1 trong kế hoạch: **audit metadata decoder THẬT trả về trước khi dựng stage sửa nào; không suy đoán trường không có.** Đã làm xong bước audit này; chưa đổi pixel render nào.

## 1. Đã làm — commit LOCAL `9b8f05b` (chưa push)

Thêm nền tảng Q1 (item #2) + audit (item #1), **KHÔNG đổi hành vi render**:
- `src/formats/raw.rs`: struct công khai `RawSensorMetadata` + `probe_sensor_metadata(path)` — decode tới mức đọc metadata (CFA, active area, black/white level per-channel, WB, số vùng optical-black bị che) nhưng **BỎ demosaic/render** nên rất nhanh (audit cả corpus 4.6s). Enum `WhiteLevelSource` (Reported / MissingReplacedByObserved / ContainerMaxReplacedByObserved) ghi provenance white-level (item #4). Hàm phân loại `white_level_source()` **soi gương từng nhánh** với `choose_effective_white_level` của đường render — có unit test ghim 2 hàm đồng bộ. Tách `decode_front_end()` (refactor byte-identical, không đổi output).
- `tests/raw_sensor_metadata_probe.rs`: harness `#[ignore]`, gated `IAI_RAW_CORPUS`, in bảng + ghi `target/q0/q1_sensor_metadata.csv` + `.json`.

Gate: `cargo fmt --check` sạch, **1441 lib test pass** (1440 + 1 test mới), 0 fail.

## 2. Kết quả audit THẬT (20 RAW, 2026-08-26, 0 lỗi)

| Vấn đề | Kết quả |
|---|---|
| **White level tin được?** | **CÓ — cả 20 file `src=ok` toàn kênh, 0 fallback.** Nghĩa là decoder trả white level đáng tin; KHÔNG cần đoán white từ observed-max; rủi ro "ảnh thiếu sáng bị tự kéo thành trắng" KHÔNG xảy ra trên corpus này. |
| **CFA / loại cảm biến** | Toàn **Bayer** (RGGB + 1 GBRG là Canon 5D2), **0 X-Trans, 0 mono**. → Q2 KHÔNG test được nhánh X-Trans trên corpus này (Fuji GFX 50S II là Bayer). |
| **Black level** | Per-camera hợp lý: Canon ~2047 (có ảnh 512), Nikon 600–1008, Sony 512, Fuji 1023. Kênh green-2 (index 3) = 0 ở Canon là bình thường (RGGB chỉ dùng kênh 0/1/2). |
| **Optical-black che (masked)** | **8 file Canon (6D/5D2/5D4/700D) có vùng black che** (`blackareas>0`) → có sẵn dữ liệu cho stage optical-black tương lai; Nikon/Sony/Fuji dùng black cố định (0 vùng). |
| **Backend** | 4 file qua **rawler fallback** (Nikon Z6/Z6II, Fuji RAF, Canon EOS R CR3); 16 file qua rawloader. |
| **Giới hạn** | Model decoder hiện KHÔNG lộ gain map / PDAF / lens / ISO / dark-flat → các correction đó chưa có nguồn dữ liệu từ decoder (đúng khoảng trống §4.2). |

Artefact: `target/q0/q1_sensor_metadata.csv` + `.json` (per-file đầy đủ). Chạy lại:
```powershell
$env:IAI_RAW_CORPUS='C:\Users\Admin\Pictures\anh-raw'; $env:IAI_Q0_OUT='C:\Users\Admin\Documents\IAI\target\q0'
cargo test --release --test raw_sensor_metadata_probe -- --ignored --nocapture
```

## 2b. Q1 item #7 (bước-2) — tách taste "chroma enrich" + A/B — commit `db1a0c9`

Trên corpus (tất cả jpeg_match=Full, không profile), hằng "taste" DUY NHẤT rõ ràng b vào master là `enrich_scene_chroma(CHROMA_ENRICH=0.85)` — boost saturation vùng midtone để bù việc JPEG-match hụt ~15% chroma (đúng cảm giác "nhợt/mờ đục"). Các hằng khác (warm/brightness/chroma_shadow) chỉ chạy nhánh `!apply_color` → KHÔNG áp corpus.

Đã làm `CHROMA_ENRICH` thành knob env `IAI_SCENE_CHROMA_ENRICH` (nhất quán với warm/brightness), **default GIỮ NGUYÊN (byte-identical)** → chưa đổi look mặc định. Thêm harness `tests/raw_taste_ab_probe.rs` render mỗi ảnh 2 kiểu (taste-on hiện tại vs taste-off = master kỹ thuật), xuất **montage cạnh nhau** (`target/q0/taste_ab/*.png`, trái=hiện tại phải=taste-off) + đo chroma.

**A/B thật 20 RAW:** enrichment RẤT NHẸ, midtone-weighted — **mean +0.0023 OKLab**, thường +3–5%, cao nhất +26% ở file Canon phẳng nhất (700D/6D), **~0 ở ảnh đã bão hòa** (neon Ngoc Long +1.8%, giáng sinh −0.1% — được saturation-protect). Nhìn montage: bản hiện tại ấm/đậm da+cây+trời hơn CHÚT ÍT; taste-off trung tính hơn nhưng nhợt hơn chút. **Không bản nào bị gắt/over.** → Đây là quyết định thẩm mỹ, và mấu chốt: căng thẳng này TỒN TẠI VÌ THIẾU camera profile (Q3). Owner nên xem `target/q0/taste_ab/` rồi quyết giữ/giảm.

## 2c. Q1 item #7 (bước-3) — recipe v1/v2 + A/B toàn bộ taste (Codex tiếp ca)

Đã gom các xử lý decode-time không thuần kỹ thuật vào một recipe có version:
- `legacy-baked-v1`: mặc định hiện hành, giữ nguyên hằng/env và thứ tự xử lý nên **không đổi pixel mặc định**.
- `technical-neutral-v2`: opt-in bằng `IAI_RAW_RENDER_RECIPE=technical`; tắt đồng bộ global color-NR, capture-sharpen, chroma-enrich, brightness, shadow-chroma và warmth. Sensor normalize/WB/demosaic/false-colour/camera characterization vẫn giữ.
- `RawSceneCharacterization` ghi `raw_render_recipe` trong provenance transient của `SceneSource`; đây là ranh giới migration/A-B, **chưa phải persistence `.iai`** và chưa đổi default.
- `raw_taste_ab_probe` nay so v1 với v2 và ghi cả lightness/chroma/acutance, không còn phải phối hợp nhiều env knob rời.

**A/B thật 20/20 RAW, 0 lỗi (2026-08-26):** `target/q0/taste_ab/*.png` + `target/q0/q1_taste_ab.csv`.
- Mean v1 − technical: lightness **+0.00123**, chroma **+0.00233 OKLab**.
- V1 thường thêm chroma 1–9%, cao nhất **+26.4%**; nhưng có ảnh **−3.5%** vì color-NR lấn át chroma enrichment. Vì vậy không thể coi toàn bộ baked recipe chỉ là một saturation boost.
- Mean acutance v1 thấp hơn technical khoảng **4.9%** trên montage-resample; color-NR làm giảm high-frequency nhiều hơn capture-sharpen bù lại ở metric này. Đây chỉ là proxy, vẫn cần xem crop 100% trước quyết định default.
- Kết luận: recipe boundary + corpus A/B đã sẵn; **không tự chuyển default sang v2**. Owner cần xem montage/crop GUI, rồi nếu chọn v2 phải persist recipe/version và nghiệm thu preview/commit/export parity.
- Gate sau thay đổi: `cargo fmt --check` sạch; `cargo test --locked` **1443 lib pass, 0 fail** và toàn bộ integration/doc tests xanh. Probe release A/B 20/20 pass.

## 2d. Q1 item #2/#3/#5/#6 — provenance + correction-stage contract — commit `c8ac961`

Hoàn thiện phần Q1 không đổi pixel trước gate nghiệm thu bằng mắt:
- `RawSensorMetadata` ghi provenance black level (`decoder` / `decoder+masked-areas`), WB, optical-black; gain map/PDAF/ISO/lens được ghi rõ `not-exposed` thay vì suy đoán.
- Adapter rawler giữ lại masked optical-black rectangles thay vì xóa tại compatibility boundary; pixel path không đổi vì rawler đã resolve black level trước đó.
- Tạo `SensorCorrectionPlan` với enable flag, reason và conservative scratch upper-bound cho từng stage. `isolated_bayer_defects` giữ đúng baseline cũ trên Bayer; `green_equilibration` tắt rõ với reason `no-diagnostic`.
- Defect correction có stage boundary trước demosaic và disabled-path bit-exact no-op test. Không thêm blur/green correction khi chưa có sensor diagnostic.
- Probe CSV/JSON mới có 39 cột/record provenance + correction plan.

**Probe thật 20/20 RAW, 0 lỗi:**
- Black provenance: **8 `decoder+masked-areas`**, 12 `decoder`.
- Defect stage: **20/20 enabled** (toàn corpus Bayer).
- Green equilibration: **0/20 enabled**; gain map/PDAF/ISO/lens: **20/20 `not-exposed`**.
- White level vẫn 20/20 reported/trusted, 0 fallback.
- Gate: `cargo fmt --check` sạch; `cargo test --locked` **1445 lib pass, 0 fail, 6 ignored**; toàn bộ integration/doc tests xanh.

## 3. Ý nghĩa cho các bước Q1 tiếp theo

Nền "normalized RAW master" là ĐÚNG được: black/white/WB đáng tin cho cả 20 file. Việc Q1 còn lại (behavior-changing, cần recipe-version + owner GUI-test):
- **Item #7 (quan trọng nhất):** recipe v1/v2 và A/B toàn bộ taste đã có, nhưng mặc định vẫn là `legacy-baked-v1` và các stage vẫn bake vào scene master. Bước đổi thật: giữ technical master riêng, áp look/detail recipe về sau, persist `raw_render_recipe` trong `.iai`, tăng engine/recipe version, golden parity và owner GUI-test. Không bật v2 mặc định chỉ từ metric.
- Item #3/#5/#6: stage contract, enable/reason/scratch và defect no-op test đã có. Green/PDAF/gain-map correction vẫn chủ ý tắt vì decoder không lộ metadata/diagnostic; không được tự bật trước khi có ảnh/crop test phù hợp.

**Gate thủ công hiện tại:** owner phải xem montage/crop v1-v2 và xác nhận chọn recipe nào trước khi đổi default/persist `.iai`. Nếu chưa chấp nhận v2, giữ v1 và chuyển sang Memory M2 theo thứ tự kế hoạch; không âm thầm đổi look.

## 3a. Kết quả gate GUI của owner — 2026-08-26

Owner xác nhận trên bản release `c8ac961`: embedded JPEG hiện trong lúc full RAW đang load có màu chuẩn/đẹp; khi full decode thay thế thì ảnh nhảy sang màu xấu, lem/bệt/mờ và có ảnh ám xanh. `Open Image` giữ nguyên kết quả full-decode xấu, không có lần nhảy màu thứ hai. Owner cũng xác nhận lỗi đã tồn tại trước các thay đổi Q1 hiện tại.

Kết luận định tuyến:
- M1 preview-first và Open Image parity không phải nguồn lỗi; đường full decode đã tạo scene master xấu trước khi commit.
- Corpus hiện 20/20 dùng decoder-matrix fallback + embedded-JPEG `Full` fit, không có DCP/ICC exact profile. Nhóm màu cần cô lập `JPEG colour fit` với decoder matrix trước khi sang Q3.
- Recipe v1 có global chroma NR mạnh; cả v1/v2 vẫn chạy false-colour median cố định 2 vòng. Nhóm bệt/mờ cần cô lập hai stage này trước khi đổi Q2 false-colour thành artifact-adaptive.

Gate kế tiếp dùng cùng một RAW, so ba đường full decode: technical hiện tại; technical không NR/sharpen/false-colour; và technical không JPEG colour fit. Chỉ sửa default/persist recipe sau khi owner chỉ ra biến thể loại được bệt/mờ và biến thể loại được ám xanh.

**Kết quả gate cô lập #1:** technical hiện tại và technical tắt false-colour không khác nhau. Gain-only và no-JPEG-match nét/đẹp hơn rõ, nhưng tối và nhạt hơn; gain-only chỉ sáng hơn no-match, chất lượng màu/độ nét như nhau. Cả hai vẫn nhảy xa embedded preview. Vì vậy false-colour và baseline gain được loại khỏi nguồn lỗi; nhóm `JPEG colour fit` là nguồn làm giảm chất lượng cảm nhận, còn khoảng cách màu còn lại thuộc decoder-matrix fallback/thiếu camera profile.

Đã tách `JPEG colour fit` thành hai stage độc lập, vẫn giữ default `Full` bit-for-bit theo đúng thứ tự cũ:
- `matrix`: baseline gain + spatial 3x3 colour matrix, không RGB histogram curves.
- `curves`: baseline gain + per-channel RGB histogram curves, không spatial matrix.
- `JpegMatchMode` có provenance riêng `BrightnessAndMatrix` / `BrightnessAndCurves`; parser có unit test không phụ thuộc process-env.

Gate tự động: `cargo fmt --check` sạch; `cargo test --locked` 1447 lib pass, 0 fail, 6 ignored; integration/doc tests xanh. Release build hoàn tất. Gate GUI #2 cần so matrix-only, curves-only và gain-only trước khi loại hoặc thiết kế lại stage gây lỗi.

## 4. Ràng buộc giữ nguyên
- CHỈ commit cục bộ, **KHÔNG push** tới khi owner bảo.
- KHÔNG copy code/asset GPL từ ART.
- Đổi default look phải tăng recipe/engine version + golden/A-B + báo owner GUI-test. Bảo toàn file planning untracked. DÙNG ÍT AGENT.
