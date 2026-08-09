# DEVELOP ENGINE 2 — MASTER PLAN

Ngày audit: 2026-08-09  
Phạm vi: nghiên cứu clean-room ART và audit source iAi hiện tại; không triển khai code  
Nguồn ART: `C:\Users\Admin\Pictures\1111\ART`  
Ưu tiên: color correctness > image quality > display accuracy > predictability > non-destructive > precision > architecture > performance > simplicity

## 1. Executive Summary

iAi không nên bỏ UI, state, lịch sử, mask, document integration hay mô hình thao tác Develop hiện tại. Tuy nhiên, nếu bỏ qua chi phí migration, lõi hiện tại **chưa phải nền móng cuối cùng mà một RAW/color engine chuyên nghiệp nên tiếp tục xây trực tiếp lên**. Nó đã tiến xa hơn một chuỗi filter sRGB đơn giản: RAW hiện được giữ thành scene-linear, unclamped, linear ProPhoto, lưu RGBA16F; Exposure của đường scene là `2^EV`; có CAT16, tone equalizer theo EV, OKLab/OKLCh, gamut mapping, monitor LUT và CPU/GPU parity tests. Đây là tài sản có giá trị và phải giữ lại như prototype/compatibility implementation.

Điểm yếu nền tảng còn lại là hai engine có semantics khác nhau cùng tồn tại, stage boundaries chưa thành public contracts, scene core vẫn chuyển sớm về linear-sRGB trong tone builder, một số creative/tone/color operation clamp về `[0,1]`, RAW input chỉ có camera matrix cơ bản và camera-preview matching, CPU/WGSL là hai bản viết tay, output-gamut logic mới chủ yếu mô hình hóa sRGB/P3, và preview/export parity dựa nhiều vào cơ chế bake thay thế hơn là một graph dùng chung hoàn toàn.

**Quyết định: Option B — Hybrid Rebuild.** Giữ shell/UI/state/document/masks/history/serialization; tạo `Develop Engine 2` như processing graph độc lập, versioned, chạy song song với legacy. Tái sử dụng có chọn lọc các module iAi đã tốt bằng cách bọc chúng sau contract mới; thay dần implementation, không “big-bang commit”. Không port code ART.

Mục tiêu kỹ thuật cuối:

```text
Input/RAW mosaic
  -> sensor corrections + linearization
  -> demosaic + capture-domain restoration
  -> camera characterization (matrix/DCP/ICC)
  -> scene-linear wide-gamut master (f32 compute, f16/f32 storage policy)
  -> scene graph: WB/CAT -> exposure -> highlight reconstruction -> tone zones
  -> scene-to-display rendering transform
  -> perceptual/creative graph: mixer, vibrance, grading, curves, locals
  -> output-gamut compression
  -> display/export color transform
  -> encode/quantize once
```

## 2. Final Recommendation: Refactor vs Hybrid vs Rebuild

| Option | Đánh giá | Quyết định |
|---|---|---|
| A — Evolutionary refactor | Không đủ. Tiếp tục thêm nhánh vào `develop_scene.rs`, `pipeline.rs` và WGSL sẽ củng cố duplicated semantics. | Loại |
| B — Hybrid rebuild | Giữ contract người dùng và phần integration tốt; dựng graph mới, adapter versioned và chuyển từng control. | **Chọn** |
| C — Clean core rebuild toàn bộ | Color core mới là cần thiết, nhưng bỏ cả các module scene/perceptual/gamut/test mới đang tốt là lãng phí và tăng rủi ro. | Không chọn ở cấp toàn sản phẩm |

Trả lời câu hỏi quan trọng nhất: **Không**, kiến trúc Develop hiện tại, xét nguyên trạng và bỏ qua migration cost, chưa nên là nền móng cuối cùng. Nhưng các thuật toán/module mới của nó là nguyên liệu tốt cho Engine 2. Vì vậy, “đập lõi” ở đây nghĩa là thay orchestration và contracts, không xóa sạch mọi code xử lý ảnh.

Ngưỡng chuyển từ B sang C: nếu Phase 1 chứng minh `SceneSource`, tile/document adapter hoặc GPU compositor không thể tách khỏi assumptions sRGB/display/legacy mà không làm graph mới phụ thuộc ngược, xây package core mới hoàn toàn và chỉ giữ adapter ở biên.

## 3. Current iAi Develop Architecture

### 3.1 Source map và trace thực tế

| Path | Module/function | Vai trò thực tế | Phân loại |
|---|---|---|---|
| `src/ui/develop.rs:71`, `:118` | `build`, `develop_panel_contents` | UI panel, sliders, curve editor, local masks; ghi `UiActions` | KEEP |
| `src/core/develop/settings.rs:221` | `DevelopSettings` | Public parameter/state contract; defaults V2 nhưng serde fallback Legacy | KEEP + REFACTOR versioning |
| `src/app/actions/develop.rs:400`, `:511`, `:739`, `:1042`, `:1172` | begin/update/spawn/poll/commit | Session orchestration, debounce, worker, apply/cancel | REFACTOR quanh graph scheduler |
| `src/core/develop_scene.rs:177` | `SceneSource` | Scene-linear master f16, working-space metadata, base look | KEEP concept; REFACTOR storage contract |
| `src/formats/raw.rs:249` | `decode_raw` | normalize/WB, defect correction, highlight inpaint, AHD/Malvar, camera matrix, ProPhoto scene, default look | REPLACE RAW pipeline incrementally |
| `src/core/develop_scene.rs:802` | `build_scene_tone_for_scene` | compose working→linear-sRGB with CAT16/exposure; camera curve composition | REFACTOR; không ép core về sRGB sớm |
| `src/core/develop_scene.rs:1144`, `:1189`, `:1200` | scene→working→display | tone, color, gamut, encode, curves | REPLACE bằng stage contracts |
| `src/core/develop/pipeline.rs:19`, `:53` | legacy raster paths | u8/u16 TileMap route; nhiều clamp display-domain | KEEP compatibility only, rồi REMOVE |
| `src/core/perceptual_color.rs` | OKLab/OKLCh + Jz prototype | shared perceptual conversion | KEEP, harden domain contracts |
| `src/core/gamut_map.rs:34` | output gamut map | OKLCh chroma search cho sRGB/P3 | KEEP + REFACTOR profile-aware |
| `src/core/cms.rs:587`, `:600`, `:677` | display/proof LUT, monitor ICC | CMS boundary | KEEP + REFACTOR precision/cache |
| `src/app/render/develop_preview.rs:48` | GPU preview builder | proxies/resources/recomposition | REFACTOR thành graph executor |
| `src/gpu/compositor.wgsl` | shader monolith | bản GPU của nhiều math Develop | REPLACE orchestration; tái sử dụng kernels đã test |
| `src/core/develop_scene.rs:1734` | `apply_scene_to_tilemap` | full-resolution bake về TileMap | REFACTOR output sink |

### 3.2 Pipeline hiện tại

RAW:

```text
rawloader decode
 -> active area + black/white normalization
 -> as-shot WB gain trên mosaic
 -> defect correction
 -> opposed-channel highlight inpaint
 -> AHD (theo pixel cap) hoặc Malvar/bilinear
 -> camera RGB -> XYZ-derived matrix -> linear sRGB -> linear ProPhoto
 -> optional capture sharpen
 -> f16 SceneSource, unclamped
 -> embedded-JPEG brightness/RGB matrix/curve matching
 -> CAT16 + 2^EV
 -> regional EV tone equalizer
 -> sigmoid/tone mode + contrast + grading
 -> color/mixer/effects/detail/locals
 -> sRGB-target gamut map
 -> sRGB transfer + point/RGB curves
 -> RGBA16 TileMap / monitor LUT / compositor
```

Raster nhỏ/vừa được linearize thành `SceneSource` với `BaseLook::Identity`. Raster quá lớn có thể rơi về legacy `pipeline.rs`; legacy đi qua decode sRGB↔linear, LUT tone, clamp lặp lại và TileMap. Interactive GPU dùng scene/proxy resources; sau debounce, full-quality CPU bake thay resting frame. Commit tái sử dụng settled bake nếu settings không đổi.

### 3.3 Representation và concurrency

- Scene master: interleaved RGBA, RGB là IEEE f16 bits, alpha có thể tách UNORM16; cho phép âm và >1.
- Compute: Rust/WGSL f32; output TileMap RGBA16 rồi compositor/display.
- CPU: Rayon theo hàng/tile; workers single-flight, stale result loại bằng job id.
- GPU: WGPU shader monolith, LUTs/proxies; math được mirror thủ công.
- Preview: giảm resolution/proxy trong drag; release chạy exact CPU bake. Màu settled/commit được gate bằng tests, nhưng interactive không phải mọi lúc là cùng sampling math.

## 4. Current iAi Weaknesses

| Vấn đề | Chứng cứ source | Tác hại | Thay thế |
|---|---|---|---|
| Hai engine | `develop_scene.rs:23-28`; `pipeline.rs` vẫn active cho oversized | cùng slider có semantics theo route; test matrix phình | một graph, legacy chỉ adapter/version |
| Working-space boundary bị rò | `build_scene_tone_for_scene` compose working→linear-sRGB trước core | gamut ProPhoto mất ý nghĩa ở nhiều stage | kernels nhận `ColorContext`; chỉ convert khi thuật toán cần |
| Clamp sớm ở legacy | `pipeline.rs:430-446`, `458-460`, `790-879` | mất headroom/negative, hue shift, không recover được | signed float graph, clamp tại defined display/output boundary |
| Display curves nằm sau encode | `develop_scene.rs:1189-1234` | master/RGB curves có semantics display-encoded, dễ band/hue shift | khai báo rõ curve domain; master perceptual trước output encode, channel curves có mode/version |
| RAW characterization mỏng | `raw.rs:287-290`, `1272-1278` | một matrix không mô hình illuminant/look/camera profile tốt | dual-illuminant DCP/ICC + CAT + calibration database |
| Default look học từ embedded JPEG | `raw.rs:424-464` | camera JPEG không phải ground truth; nguy cơ picture-style/scene bias | profile-based baseline; matching chỉ opt-in/fallback có confidence |
| Demosaic phụ thuộc pixel cap | `raw.rs:323-375` | ảnh lớn có chất lượng khác do kích thước | quality preset explicit; tile/halo AHD-quality hoặc thuật toán production mới |
| Gamut target hard-coded | `gamut_clip_chroma` gọi sRGB | P3/custom ICC không có boundary nhất quán | profile-aware boundary model/cache |
| CPU/WGSL twins thủ công | docs Phase 2 và shader | drift lâu dài | shared IR/constants/LUT generation + conformance vectors |
| Tone/color node coupling | `SceneToneData` chứa WB, exposure, tone, grade, curves | invalidation thô; khó test stage | immutable typed nodes + node hashes |
| f16 master duy nhất | `SceneSource.half` | dải rất tối/gradient qua nhiều transform có thể thiếu precision | policy f16 storage/f32 tiles; f32 for analysis/critical nodes |
| Histogram ambiguity | proxy qua settings/render chain | người dùng không biết pre/post tone/output | explicit measurement tap metadata |
| Locals/materialized bake | actions + scene apply | recompute rộng, khó GPU hóa từng node | mask graph + ROI/dirty-tile invalidation |

Không thấy đường scene RAW hiện tại thực hiện `RGB += exposure`, xử lý Exposure gamma-domain, hay dùng intermediate 8-bit. Những lỗi đó chủ yếu thuộc legacy/biên raster. Không được mô tả iAi hiện tại như một engine 8-bit đơn giản.

## 5. ART Architecture Overview

ART chia UI (`rtgui`) khỏi engine (`rtengine`). `ProcParams` là state; `ImProcCoordinator::updatePreviewImage` điều phối preview/cache; `SimpleProcess` điều phối output; `ImageSource`/`RawImageSource` quản lý decode/RAW; `Imagefloat` là buffer RGB planar float; `ImProcFunctions::process` thực thi bốn stage. `ICCStore`, DCP, LittleCMS và monitor transform tạo lớp color management riêng. OpenMP, SIMD macro và buffer reuse được dùng rộng.

Trace chứng cứ (chỉ mô tả, không sao chép code):

| ART path | Class/function | Hành vi quan sát |
|---|---|---|
| `rtengine/simpleprocess.cc:180-259` | process load stage | chọn WB, preprocess, demosaic, auto-WB, getImage, gán working profile |
| `rtengine/simpleprocess.cc:296-319` | `stage_denoise` | input/working conversion quanh film-negative, rồi denoise |
| `rtengine/simpleprocess.cc:322-355` | `stage_transform` | analysis, stage 0, lens/geometric transform |
| `rtengine/simpleprocess.cc:391-430` | `stage_finish` | DCP state, stage 1/2/3, resize, output sharpen, RGB→output |
| `rtengine/improcfun.cc:574-648` | `process` | stage order thật, không dựa vào tên UI |
| `rtengine/improccoordinator.cc:147-605` | preview coordinator | invalidation, preprocess/demosaic, buffers, navigator, monitor/proof transform |
| `rtengine/rawimagesource.cc:1690`, `:2139` | preprocess/demosaic | RAW preprocessing và chọn demosaic |
| `rtengine/rawimagesource.cc:3732` | color-space conversion | camera ICC/DCP/matrix sang working RGB |
| `rtengine/dcp.cc:1216`, `:1342`, `:1391` | DCP apply/state/tile | camera matrices, HueSatMap/look/tone stages |
| `rtengine/iprgb2out.cc:232`, `:338` | monitor/output | working→monitor/soft-proof/output conversion |

## 6. ART Processing Pipeline

Pipeline output dựng từ call graph:

```text
load metadata/raw
 -> choose WB
 -> preprocess (sensor/lens/raw corrections)
 -> demosaic
 -> get working image / camera color conversion
 -> optional denoise
 -> analysis
 -> Stage 0: dehaze -> dynamic-range compression
 -> geometric/lens transform
 -> Stage 1: channel mixer -> exposure -> HSL equalizer
             -> tone equalizer -> ProPhoto-blue handling
 -> Stage 2: optional early DCP look -> masks
             -> sharpening -> impulse denoise -> defringe
             -> color correction -> guided smoothing
 -> Stage 3: gradients -> texture boost -> grain -> log encode
             -> saturation/vibrance -> late DCP look
             -> film simulation (configurable around tone curve)
             -> tone curve -> RGB curves -> Lab adjustments -> soft light
             -> local contrast -> B&W
 -> resize -> output sharpening
 -> working RGB -> output ICC
```

Preview dùng cùng `ImProcFunctions::process` theo Pipeline/Stage nhưng scale, buffers, stop/mask behavior và preview sharpening có khác biệt. Đây là nguyên lý đáng học: shared stage implementation, explicit preview policy; không nên bê nguyên stage order vì một số order là lịch sử/feature interaction của ART.

## 7. ART Color Science Findings

### Exposure

`rtengine/ipexposure.cc:29-76` dùng multiplier `2^expcomp`, sau đó trừ black offset và chặn âm. Nguyên lý EV là đúng; black offset và clamp gắn chung vào exposure là legacy coupling. iAi nên tách Exposure, flare/black calibration và negative policy thành node khác nhau.

### Saturation/vibrance

`rtengine/ipsaturation.cc:43-82` tính luminance bằng matrix working profile, scale chroma quanh luminance; vibrance dùng response phi tuyến trên signed channel residual. Nó tránh HSV đơn giản và tôn trọng working profile, nhưng vẫn đặt floor dương. Học nguyên lý luma/chroma separation và adaptive response; không port implementation.

### Tone curves/contrast

`rtengine/iptonecurve.cc:577+` hỗ trợ base curve, film-like clipping, contrast curve và nhiều curve mode; curve input được điều chỉnh qua transfer-domain. `iprgbcurves.cc:64+` là channel LUT riêng. Bài học: master tone curve cần modes với mục tiêu hue/chroma rõ; RGB curves là creative per-channel operation và phải được ghi nhãn như vậy.

### Highlights

`rawimagesource.cc:2283`, `hilite_recon.cc:328`, `:1545` cho thấy recovery ở RAW/camera domain và nhiều strategy, trước các creative transforms. Bài học quan trọng là phân biệt reconstruction (khôi phục dữ liệu/kênh clip) với compression/roll-off (render highlight).

### Color management và appearance

ART duy trì working profile matrices, camera ICC/DCP, output/monitor/proof transforms. Color correction có YUV/HSL/RGB/JzAzBz paths (`ipcolorcorrection.cc`); CIECAM02 tồn tại trong `ciecam02.cc`. Giá trị lớn nhất là explicit profile boundary và chọn space theo operation. Không nên mang toàn bộ mode surface/legacy complexity sang iAi.

### Spatial quality/performance

Tone equalizer chuẩn hóa theo pivot/EV rồi chạy edge-aware; local contrast, denoise, sharpening tách module. `Imagefloat` planar float, OpenMP/SIMD và scale-aware preview giúp throughput. Học tiling/halo/cache và shared operations; không sao chép SIMD/macros hay class layout.

## 8. Best Principles Learned From ART

| Nhóm | Problem | Mathematical principle | Expected behavior | Clean-room iAi proposal | Loại |
|---|---|---|---|---|---|
| RAW highlight | một/vài kênh sensor clip | infer chroma/ratios từ neighborhood và kênh còn tin cậy | highlight có màu liên tục, không zipper | confidence map + multi-scale opposed-color reconstruction ở mosaic/camera RGB | A/B |
| Exposure | EV phải có nghĩa vật lý | linear radiometric scale `2^EV` | +1 EV gấp đôi signal | node scene-linear độc lập, không black clamp | A |
| Tone equalizer | kéo vùng sáng/tối mà giữ texture | additive offsets trong log luminance với edge-aware base | no halo, local ratio ổn | bilateral/guided pyramid trên log2(Y) | A/B |
| Saturation | tăng màu không đổi brightness mạnh | separate achromatic axis/chroma; adaptive gain | neutral giữ neutral; màu yếu tăng nhiều hơn | OKLCh/JzCzHz adaptive chroma node + skin/gamut confidence | A/B |
| Tone curve | RGB curve dễ đổi hue | map luminance/perceptual lightness rồi reconstruct chroma | monotone tone, stable hue | master curve modes; RGB curves explicit creative | A |
| Camera profiles | matrix đơn không đủ | dual-illuminant interpolation, CAT, 3D HueSat/look tables | camera color ổn theo illuminant | reader/spec độc lập cho DCP/ICC; profile cache | A/B |
| CMS | pixel math đúng chưa đủ | characterized transforms + rendering intent | preview/export đáng tin | input→working and working→monitor/output boundaries | A |
| Preview | full render chậm | same math, lower resolution/cache | preview không đổi màu khi settle | graph kernels identical, proxy changes sampling only | A |

## 9. ART Legacy / Things Not Worth Copying

- Thang giá trị nội bộ 65535 trong float và lookup conventions đặc thù: C — legacy.
- Exposure kết hợp black subtraction rồi clamp âm: B/C; tách node.
- Stage order chứa nhiều feature-specific switches (DCP/film simulation trước hoặc sau curve): B; iAi dùng graph constraints và versioned recipes.
- Nhiều color models/modes chồng nhau YUV/HSL/Lab/Jz/CIECAM: C/E nếu không có user problem và benchmark rõ.
- Macro OpenMP/SIMD, raw pointer class layout và coordinator mutable state: C; không phù hợp Rust/WGPU.
- ProPhoto-blue special case: B/C; thay bằng profile-aware gamut/color rendering tổng quát.
- Preview-specific early stop/mask wiring: B; iAi cần graph tap và deterministic partial evaluation.
- Resources/profile/LUT/camera constants của ART: tuyệt đối không nhập vào iAi.

## 10. Copyright & Clean-Room Boundary

Tài liệu này chỉ ghi problem, behavior, principle và vị trí trace. Không chứa đoạn code ART, constants đặc thù, LUT/profile/resource hoặc bản dịch function.

Quy trình bắt buộc khi coding:

1. Nhóm nghiên cứu chốt independent spec và test vectors từ paper/ICC/CIE/DNG specs.
2. Người implement làm từ spec, không mở source ART trong lúc viết implementation tương ứng.
3. Review provenance cho mỗi node; ghi nguồn public-standard/paper.
4. So sánh ART chỉ bằng black-box images/metrics; không pixel-clone.
5. Không đưa tên class/function ART vào API iAi ngoài phần trace lịch sử của tài liệu.

Nguồn chuẩn cần ưu tiên trong implementation: CIE colorimetry/CAT, ICC v4/iccMAX, Adobe DNG/DCP specification, IEC sRGB, ITU BT.2100 khi HDR, ACES publications, papers gốc của demosaic/denoise/tone mapping.

## 11. ART Features Worth Adding to iAi

| Feature | ART có | iAi có | Giá trị | Complexity | Có nên thêm |
|---|---:|---:|---:|---:|---|
| Dual-illuminant DCP + HueSatMap | Có | Chưa đầy đủ | Rất cao | Cao | P0/P1: Có |
| Advanced RAW highlight recovery modes | Có | Có bản opposed/inpaint cơ bản | Rất cao | Cao | P1: Có |
| Lens CA/distortion/vignette profiles | Có | Hạn chế | Cao | Cao | P2: Có |
| Defringe/false-color suppression | Có | Một phần | Cao | Vừa | P2: Có |
| Noise profiling + chroma/luma denoise | Có | Chưa ở mức RAW chuyên nghiệp | Rất cao | Rất cao | P2: Có |
| Tone equalizer mask/colormap | Có | Backend zones có | Cao | Vừa | Có, UI bổ sung sau core |
| Soft proof + gamut warning | Có | CMS/LUT nền tảng có | Cao | Vừa | P1: Có |
| Channel mixer 3×3 | Có | Không rõ tool tương đương | Vừa/cao | Thấp | Có |
| Advanced local color correction | Có | Local settings cơ bản | Cao | Cao | P3: Có |
| Output sharpening by size | Có | Chưa hoàn chỉnh | Cao | Vừa | P2: Có |
| CIECAM02 tool surface | Có | Không | Không chắc | Cao | Chưa; benchmark CAM16/Jz trước |
| Film simulation/CLUT catalog | Có | Có LUT ecosystem khác | Tùy workflow | Cao/licensing | Không lấy resource; chỉ plugin/profile riêng sau |
| Waveform/RGB parade/vectorscope | UI reference | Histogram chủ yếu | Cao | Vừa | P2: Có |

## 12. Existing iAi Features to Keep

- UI/UX Develop, slider ranges và action flow hiện có như public contract.
- `DevelopSettings`, local mask state, session/history/document integration.
- `SceneSource` concept: immutable scene master, explicit working metadata, signed headroom.
- Exact EV path `exposure_multiplier` cho scene engine.
- CAT16 module, linear ProPhoto decision hiện tại (giữ cho V2.0; benchmark lại cho HDR).
- OKLab/OKLCh perceptual core và Color Mixer V2 raised-cosine memberships.
- Output gamut mapping tests, monitor ICC discovery/LUT, proofing groundwork.
- Rayon, single-flight preview job, stale-id cancellation, settled-frame reuse.
- Golden/parity/profile/working-space/performance tests đã có.

## 13. Existing iAi Features to Rework Internally

Tất cả UI giữ nguyên trừ khi ghi khác:

| Control | Mapping đề xuất | Domain/algorithm | Hue/chroma & gamut | Extreme behavior |
|---|---|---|---|---|
| Exposure | slider→EV, ±5 mặc định | scene-linear multiply `exp2(EV)` | không đổi chromaticity; không clamp | preserve over-range; warning, không tự recovery |
| Contrast | signed strength, pivot từ scene stats | log-luminance sigmoid/slope around mid-gray | reconstruct ratio/perceptual chroma | monotone, bounded slope, shoulder/toe |
| Highlights | signed EV zone | edge-aware log-Y high zone | reconstruction trước; compression giữ hue | không đảo tone/kênh |
| Shadows | signed EV zone | log-Y low zone + noise confidence | chroma gain giảm gần noise floor | lift không làm bạc/halo |
| Whites/Blacks | endpoint density/zone EV | scene white/black zones, không offset RGB | neutral axis protected | no hard crush/clip |
| Temperature/Tint | mired/Δuv internal | CAT16/CAT02 from source white to working white | one matrix in scene linear | cap CAT condition; finite fallback |
| Saturation | signed perceptual chroma gain | OKLCh SDR; JzCzHz candidate HDR | gamut-aware compression | max không tạo cusp discontinuity |
| Vibrance | adaptive gain | low-chroma boost by L/C/h confidence | skin protection continuous, not hard mask | saturated pixels approach zero extra gain |
| Clarity | mid-scale local contrast | edge-aware multiscale log-Y | separate chroma; halo limiter | bounded overshoot |
| Texture | high-frequency detail bands | multiscale luminance/detail | chroma noise guard | no ringing/noise explosion |
| Dehaze | transmission/contrast family | scene-linear/log-luma, atmospheric prior | neutral/chroma confidence | avoid black crush/color cast |
| Tone Curve | x/y normalized display perceptual | monotone cubic on OKLab L or luminance | preserve C/h until gamut | endpoints/versioned extrapolation |
| RGB Curves | encoded or linear explicit mode | per-channel creative curves | hue shift is intentional | finite, monotone optional, no hidden clamp before node exit policy |
| HSL/Color Mixer | same current bands | periodic OKLCh basis | normalized overlap; profile-aware gamut | continuous at 0/360 |
| Color Balance/Grading | wheel→opponent vector | scene-linear LMS for balance; perceptual split-tone creative | zero-luma vectors, gamut compression | smooth tonal masks |
| Highlight Recovery | strength/mode | confidence-based mosaic/camera reconstruction | infer ratios, neutral fallback | cannot invent detail: confidence flag |
| Shadow Recovery | amount | tone lift tied to noise model | suppress chroma amplification at floor | smooth guard |
| Local Contrast | amount/radius | edge-aware log-Y pyramid | chroma unchanged by default | halo/ringing limits |

## 14. Components to Remove

- Legacy `pipeline.rs` as a shipping renderer sau khi mọi project version cũ có deterministic compatibility renderer hoặc baked fallback.
- Duplicated hand-written formulas across Rust/WGSL sau khi shared graph constants/IR hoạt động.
- Hard-coded sRGB output inside `gamut_clip_chroma`.
- Camera embedded-JPEG fit như default characterization bắt buộc; giữ tùy chọn “Match Camera Preview” nếu có confidence.
- Pixel-cap quality fallback ngầm trong demosaic.
- Display-domain repeated clamps và conversions nằm giữa core nodes.
- Monolithic `SceneToneData` sau khi từng node có typed immutable params.

## 15. Proposed Develop Engine 2

Các package logic:

```text
develop2/
  graph/          typed DAG, validation, node hashes, recipes
  color/          spaces, CAT, profile transforms, gamut boundary
  raw/            sensor normalize, defect/CA, demosaic, reconstruction
  nodes/          exposure, tone, color, detail, local, output
  execution/      CPU tiles, GPU compute, cache, cancellation
  io/             UI/state/project adapters, input/output sinks
  scopes/         histogram/waveform/vectorscope taps
  validation/     probes, conformance vectors, golden metadata
```

Mỗi buffer mang metadata runtime: primaries/profile id, white point, transfer, scene/display flag, alpha association, numeric range, precision và extent. Graph validation từ chối nối hai node sai representation.

## 16. Processing Graph

```text
Decode metadata
   ├─ RAW -> SensorLinear -> MosaicCorrect -> HighlightReconstruct -> Demosaic
   └─ Raster -> InputICCDecode
              ↓
Camera/Input Transform -> SceneMaster
              ↓
WB/CAT -> Exposure -> Scene Denoise/Defect residual -> Tone Zones
              ↓
Scene-to-Display Render Transform
              ↓
Perceptual Tone/Color -> Mixer/HSL -> Grading -> Local graph
              ↓
Detail/Output Sharpen -> Output Gamut Compression
              ↓                         ↓
         Display transform          Export transform
         Monitor ICC + encode       Output ICC + encode
```

Constraints: reconstruction trước demosaic khi có lợi và có mosaic; WB/exposure trước scene tone; denoise trước detail enhancement; creative grading sau technical render mặc định; gamut compression sau creative color nhưng trước output encoding; monitor transform không bao giờ đi ngược vào processing graph.

## 17. Scene-Referred Architecture

Scene master là radiometric-relative, linear, wide gamut, unbounded. Negative values được giữ nếu hữu hạn; node nào không hỗ trợ signed input phải khai báo và dùng reversible domain guard, không clamp im lặng. Alpha thẳng hoặc premultiplied phải được metadata hóa; color operations chạy trên unassociated color với epsilon policy.

Exposure, WB, camera transform, reconstruction, scene denoise, tone-zone offsets và phần technical của highlight/shadow nằm ở đây. Không gọi histogram display là scene histogram.

## 18. Display-Referred Architecture

Scene-to-display transform tạo display-linear reference RGB với reference white/black/nits explicit. Master perceptual curve, creative saturation/mixer/grading, display locals và output preparation hoạt động sau boundary theo recipe. SDR v2.0 dùng 100-nit reference và sRGB/P3 outputs; HDR chỉ bật khi có nits/transfer/metadata end-to-end.

Encoding transfer chỉ tại display/export sink. Monitor ICC là view transform, không bake vào document pixels.

## 19. Working Color Space

| Space | Dùng tốt cho | Không dùng làm |
|---|---|---|
| Linear sRGB | compatibility, display math SDR | RAW scene master |
| Display P3 | output/display target | scene core |
| Rec.2020 | interchange/HDR container | perceptual edits trực tiếp |
| Linear ProPhoto | photographic scene RGB, hiện có | output without compression |
| ACEScg | scene/VFX/HDR candidate | direct display/output |
| XYZ | PCS/matrix bridge | convolution/detail |
| Lab | ICC metrics/ΔE2000 | HDR scene math |
| OKLab/OKLCh | SDR perceptual color/mixer/gamut | raw sensor/reconstruction |
| JzAzBz/JzCzHz | HDR/high-luminance prototype | default cho đến khi validated |
| ICtCp | HDR signal/chroma metrics | SDR generic working space |
| LMS | chromatic adaptation/opponent operations | storage master |

Quyết định v2.0: giữ **linear ProPhoto** cho scene master mới để tương thích công việc hiện tại; mọi node nhận `WorkingSpace` context. Benchmark ACEScg lại khi HDR/DCP corpus đầy đủ. Không có một space duy nhất cho toàn pipeline.

## 20. Input Color Management

Raster: đọc embedded ICC; nếu thiếu dùng format policy và cảnh báo, không đoán wide-gamut. Decode transfer bằng CMS chuẩn rồi convert một lần vào scene/compatibility working space.

RAW: black/white level theo kênh, linearization table, masked pixels nếu có, WB metadata, camera matrix/DCP/ICC, dual illuminant interpolation, CAT tới working white, optional baseline exposure. DCP look/tone phải tách khỏi technical camera characterization và versioned. Unsupported profile phải rơi về matrix với provenance flag.

## 21. Display Color Management

```text
display-linear reference
 -> output/view gamut compression for monitor target
 -> document/output profile transform
 -> monitor ICC transform (intent + BPC policy)
 -> OS surface encoding
```

Export bỏ monitor leg và dùng output ICC. LUT cache key gồm source profile hash, destination hash, intent, BPC, bit depth và reference encoding. Monitor hot-change làm invalid view cache, không invalid scene/tone caches. Soft proof là nhánh view; gamut warning đo delta giữa pre-proof và proof/output boundary.

## 22. Exposure Engine

- Purpose: thay đổi scene exposure vật lý.
- Input/output: signed scene-linear wide RGB → cùng representation.
- Math: `gain = exp2(EV)`, RGB *= gain; alpha untouched.
- Precision: f32 compute; preserve f16-storable over-range hoặc promote tile.
- Clip/negative/gamut: none; sanitize only non-finite by diagnostic policy.
- Dependencies: sau camera/WB convention được chốt; trước tone/render.
- Validation: +1 EV ratio 2 trong tolerance, neutral/chromaticity invariant, ±5 EV finite.

## 23. Tone Engine

Tone engine gồm hai phần: scene-zone exposure field và rendering transform. Field dùng log2 luminance, edge-aware base và smooth basis functions. Rendering transform là monotone sigmoid/toe/shoulder anchored ở middle gray; slope/white placement tách biệt. Chroma response phụ thuộc compression ratio và gamut excursion, không là hằng số desaturation.

## 24. Highlight Engine

1. Sensor reconstruction: phát hiện clip per-channel bằng white-level confidence; reconstruct mosaic/camera RGB từ spatial/color ratios; trả confidence.
2. Scene highlight compression: shoulder trên intensity/luminance, giữ ratios trong vùng tin cậy.
3. Display gamut resolution: compress chroma theo output cusp.

Không dùng highlight slider để giả làm reconstruction. Với confidence thấp, chuyển mềm về neutral highlight, không tạo texture giả.

## 25. Shadow Engine

Shadow lift là EV offset trên edge-aware log-luminance. Noise estimate từ RAW/flat-region điều khiển chroma/detail gain. Black floor và toe độc lập. Negative scene RGB không bị clamp chỉ vì nâng shadow. Tại cực đại, noise/chroma amplification tiến tới giới hạn liên tục.

## 26. Contrast Engine

Contrast điều khiển local slope quanh pivot/mid-gray và thay toe/shoulder bù để duy trì range. Luminance/perceptual mapping rồi reconstruct chroma; không nhân RGB quanh 0.5. Curve phải monotone, derivative bounded, neutral-preserving; hue error được đo trước/sau gamut map.

## 27. Saturation Engine

OKLCh cho SDR: `C' = C * response(amount,L,C,h)`. Response giảm gần black/white, giảm khi sát cusp và có smooth skin confidence. Giảm saturation phải tiến đều tới neutral. L giữ trong tolerance trước gamut map; hue giữ trừ undefined hue gần neutral.

## 28. Vibrance Engine

Vibrance ưu tiên màu chroma thấp/trung bình: gain là hàm giảm của normalized chroma-to-cusp, điều chế bởi lightness và skin confidence. Không dùng HSV.S. Skin protection là weight liên tục theo hue/chroma/lightness và không khóa da tuyệt đối. Extreme positive không làm màu vốn saturated vượt cusp; extreme negative không đảo hue.

## 29. Curves Engine

- Master curve mặc định: monotone cubic trên perceptual lightness, endpoint `[0,0]`, `[1,1]`, flat extrapolation theo v2.0; optional luminance legacy.
- Scene curve tương lai: log exposure domain, chỉ khi UI/recipe khai báo rõ.
- RGB curves: per-channel creative, domain encoded-reference hoặc linear explicit; version cũ giữ old mode.
- LUT resolution tối thiểu 4096 hoặc analytic spline trên CPU/GPU; 256 chỉ dùng nếu error-bound chứng minh đạt gradient budget.

## 30. HSL / Color Mixer

Giữ UI bands hiện tại. Backend V2 dùng circular normalized basis; classify và edit trong cùng cylindrical perceptual model. Hue edit cố giữ L/C, saturation cố giữ L/h, luminance cố giữ C/h; confidence giảm về zero gần neutral/black/white. Output gamut map xử lý sau tổng creative edits, không clamp mỗi band.

## 31. Color Grading

Ba tonal ranges tùy UI hiện tại, weights sum-to-one và overlap mượt. Controls map tới opponent/perceptual vectors; balance dịch centers trong log luminance. Offset phải tách luminance/chroma để wheel không đổi exposure ngoài chủ đích. Có chế độ technical WB riêng; grading không thay WB metadata.

## 32. Gamut Management

Ba policy: preserve (scene), compress (creative/display), encode (sink). Boundary model theo actual target profile: analytic matrix/TRC profiles, sampled 3D boundary/cache cho LUT ICC. Mapper giữ hue/lightness ưu tiên nhưng cho phép trade-off có kiểm soát gần cusp nơi OKLCh không hoàn hảo. Không-gamut warning lấy trước mapper; clipping warning lấy sau render nhưng trước encode.

## 33. Default iAi Rendering Transform

Tên: `iAi Natural v1`.

Đặc tính:

- Middle gray ổn định; adaptive exposure chỉ từ robust scene statistics và profile baseline.
- Toe mềm, giữ black separation; shoulder bắt đầu theo estimated scene white, không theo từng channel riêng.
- Midtone slope vừa phải tạo “pop” nhưng không dùng local sharpening giả.
- Hue-preserving intensity mapping; chroma giảm theo compression/gamut excursion, mạnh hơn ở specular extreme.
- Skin confidence giảm oversaturation, không ép hue về một màu mẫu.
- Default sharpening chia capture/output, không oversharpen khi zoom-fit.
- No camera-JPEG matching mặc định khi có characterized profile; fallback matching có flag và bounded transform.

Ba recipe: Natural (default), Neutral (measurement), Filmic (creative). Không clone Adobe; benchmark theo thuộc tính exposure shoulder, black separation, midtone slope, chroma/hue/skin stability, gamut smoothness và acutance.

## 34. Precision Architecture

| Type | Policy |
|---|---|
| uint8 | chỉ input/output legacy/preview UI |
| uint16 | encoded export/TileMap compatibility; không core math |
| float16 | cache/storage scene tiles khi error budget đạt |
| float32 | mọi compute, LUT generation, spatial accumulators tối thiểu |
| float64 | profile fitting, matrices/least squares, offline reference |

Mỗi node phải test NaN, Inf, denormal, signed zero, negative, HDR over-range. Sanitizer ghi telemetry/node id; không silently biến NaN thành black ở giữa graph. Dither tại quantization cuối, không giữa nodes.

## 35. CPU Architecture

Tile DAG 256–512 px với halo theo node; work-stealing/Rayon, immutable params, scratch arenas theo worker, planar/SoA views cho kernels nặng. Node cache key = source revision + node type/version + canonical params + color context + scale/tile/halo. SIMD qua portable abstraction sau scalar reference. Deterministic mode cố định reduction/order cho tests.

## 36. GPU Architecture

Compute graph thay shader monolith: fused pointwise passes, separate spatial passes, persistent scene textures, storage format policy RGBA16F/RGBA32F. Constants/matrices/LUT metadata sinh từ một schema dùng chung; shader conformance chạy cùng vectors CPU. Pipeline cache theo graph signature. Không cho fast-math phá monotonic/finite contracts nếu chưa đo.

## 37. Preview Architecture

Preview và export dùng cùng node implementations/IR và constants. Interactive được phép:

- giảm resolution/mipmap;
- giảm iteration spatial có declared quality level;
- reuse stale upstream caches;
- chỉ render visible ROI.

Không được đổi color model, stage order hay transfer. Sau release render exact ở viewport trước, rồi background full image. Badge `Interactive/Refining/Full quality` hiện có giữ lại. Apply phải không đổi frame nếu exact cache hợp lệ.

## 38. Histogram & Scopes

| Scope | Tap mặc định | Mục đích |
|---|---|---|
| RAW RGB histogram | post-normalize, pre-WB | sensor clipping |
| Scene luminance histogram | post exposure/WB, pre-render | exposure/headroom |
| Display RGB histogram | post creative, pre-output encode | editing result |
| Waveform | display-linear hoặc encoded selectable | spatial tone |
| RGB parade | display post-render, pre-monitor | channel balance/clipping |
| Vectorscope | output-referred perceptual/Y'CbCr declared | hue/saturation |
| Clipping warning | post render, target gamut | output clipping |
| Gamut warning | pre-map vs target boundary | compressed/out-of-gamut pixels |

UI phải ghi tên tap/profile; monitor transform không tham gia scopes trừ “display diagnostic” riêng.

## 39. Existing UI Integration

Luồng giữ nguyên:

```text
UI control -> DevelopSettings -> DevelopRecipeAdapter(version)
 -> typed node params -> graph snapshot -> preview/export sink
```

Slider range không đổi. Adapter mapping mới theo `engine_version`; tooltip có thể giải thích EV/mode nhưng không redesign panel. Các selector mới chỉ thêm khi cần: Engine (developer-hidden), Tone Mapping hiện có, curve mode, highlight recovery mode, scope tap. Undo/history lưu settings snapshot, không lưu cache.

## 40. Project Compatibility

- Thêm `develop_engine_version`: `Legacy1`, `Scene1`, `Develop2`.
- File cũ thiếu field mở bằng đúng renderer đã tạo look cũ; không auto đổi màu.
- New documents dùng Develop2; “Upgrade rendering” là thao tác explicit, tạo history checkpoint và side-by-side preview.
- Preset có `semantic_version`, working-space/profile assumptions và node versions.
- Nếu về sau bỏ binary legacy, cung cấp one-time compatibility bake kèm original settings; chỉ làm sau telemetry/adoption policy.
- Serialization dùng defaults chỉ để chọn legacy, không làm settings cũ vô tình vào math mới.

## 41. Testing Architecture

Numerical: exact EV, neutral preservation, monotonicity/derivative bounds, matrix/profile roundtrip, deterministic CPU/GPU, no NaN/Inf, alpha invariants.

Color corpus: ColorChecker nhiều illuminant, diverse skin, foliage, sky, sunset, neon/LED, saturated fabrics/flowers, deep shadow, clipped colored highlights; source/license manifest bắt buộc.

Stress: ±5 EV, extreme WB, saturation/vibrance max, extreme curves, signed RGB, 16+ stop HDR, P3/ProPhoto boundary, transparent pixels.

Metrics: ΔE00/ΔEOK, hue-angle error, chroma/lightness drift, clipping/gamut fraction, SSIM/PSNR cho structural kernels, gradient adjacent-step variance/banding, halo/ringing score. Không dùng SSIM/PSNR một mình để đánh giá màu.

Golden metadata gồm input hash, engine/node versions, profile hashes, build/device, precision, tap và tolerance. Golden change cần signed rationale.

## 42. A/B Validation

Harness render cùng crop/settings semantic qua iAi legacy, Scene1, Develop2, ART và reference ACR thủ công nếu license cho phép. Normalize crop/orientation/profile và so ở scene/display/output taps; không ép cùng pixel nếu default look khác.

Scorecard blind review:

- highlight detail/chroma/roll-off;
- skin hue/chroma under exposure/contrast;
- shadow noise/color stability;
- hue ramps and cusp continuity;
- midtone separation/local contrast;
- gradient/banding;
- preview-settle-export delta;
- render time, peak RAM/VRAM.

Acceptance không phải “giống Photoshop”; là đạt thresholds và preference rate trên corpus mà không có category regression.

## 43. Migration Strategy

Hai engine chạy song song sau hidden developer flag. Mỗi node chuyển theo vertical slice: adapter + CPU reference + GPU + scopes + compatibility + tests. Không xóa legacy khi chưa có golden coverage, user acceptance và rollback build. Rollback luôn là chọn renderer version cũ, không revert document data.

## 44. Detailed Implementation Phases

### Phase 0 — Freeze/provenance (2–3 tuần)

- Goal: đóng baseline của worktree hiện tại, license manifest, node inventory.
- Modules: tests/docs/current scene and legacy entry points.
- Compatibility: không đổi output.
- Risks: current uncommitted state khó tái tạo.
- Tests/benchmarks: toàn suite hiện có + real corpus baseline.
- Rollback: documentation-only.
- DoD: reproducible commits/build, source trace, golden/profile hashes.

### Phase 1 — Graph/contracts skeleton (3–5 tuần)

- Goal: typed buffer/color contracts, DAG, versioned adapter, null renderer.
- Modules: `develop2/graph`, `io`, project schema.
- Compatibility: Legacy1/Scene1 untouched.
- Risks: over-abstraction.
- Tests: graph validation, serialization, no-op parity.
- Rollback: feature flag off.
- DoD: Develop2 renders identity through UI without color change.

### Phase 2 — Input/scene foundation (6–10 tuần)

- Goal: profile-aware scene master, DCP/ICC, RAW quality policy.
- Modules: raw/color/input nodes.
- Compatibility: current raw decoder remains fallback.
- Risks: camera corpus/licensing, memory.
- Tests: camera matrices, illuminants, ColorChecker, demosaic/defect/highlight.
- Rollback: per-file Scene1 fallback.
- DoD: supported RAWs meet color/headroom targets with explicit provenance.

### Phase 3 — Exposure/tone/default render (6–8 tuần)

- Goal: `iAi Natural v1`, zone tone, highlight/shadow contracts.
- Modules: scene nodes/render transform.
- Compatibility: adapter preserves old rendering by engine version.
- Risks: subjective tuning, scene classification.
- Tests: ramps, skin/highlight/shadow corpus, monotonicity.
- Rollback: recipe switch.
- DoD: blind review and numeric gates beat Scene1 baseline without category loss.

### Phase 4 — Perceptual color/curves/gamut (5–8 tuần)

- Goal: migrate saturation, vibrance, mixer, grading, curves, profile gamut.
- Modules: perceptual/color/output nodes.
- Compatibility: legacy mixer/curve node versions retained.
- Risks: cusp performance, OKLab HDR limits.
- Tests: hue/chroma/lightness isolation, gradients, ICC targets.
- Rollback: node-version fallback.
- DoD: continuous ramps, skin/gamut targets, CPU/GPU parity.

### Phase 5 — Spatial/detail/locals (8–12 tuần)

- Goal: shared tiled spatial engine, masks, denoise/clarity/texture/dehaze.
- Modules: execution tiles, spatial nodes, mask graph.
- Compatibility: existing masks adapted exactly.
- Risks: halos/seams/VRAM.
- Tests: tile seam/halo, noise, local mask edges, 45 MP.
- Rollback: individual node legacy adapter.
- DoD: no seams, bounded halos, interactive target met.

### Phase 6 — GPU/preview/export unification (6–10 tuần)

- Goal: graph executor GPU, same math and output sinks.
- Modules: compute pipelines/cache/scheduler.
- Compatibility: CPU exact fallback.
- Risks: device limits, shader divergence.
- Tests: per-node/full graph conformance across GPUs.
- Rollback: CPU executor.
- DoD: settled preview==export within declared quantization tolerance.

### Phase 7 — Scopes/new high-value features (4–8 tuần)

- Goal: scopes, proof/gamut warnings, channel mixer, advanced recovery controls.
- Compatibility: additive UI only.
- Risks: scope performance/UI crowding.
- Tests: tap correctness/profile semantics.
- Rollback: hide feature flags.
- DoD: scopes identify known synthetic conditions exactly.

### Phase 8 — Default/migration release (4–6 tuần)

- Goal: Develop2 default, upgrade workflow, deprecation decision.
- Risks: old projects, support load.
- Tests: reopen corpus across all project versions; crash/perf soak.
- Rollback: remote/default engine switch if product supports it; otherwise release setting.
- DoD: zero silent old-project color changes, signed quality/perf report.

## 45. Risk Register

| Risk | P/I | Mitigation |
|---|---|---|
| Silent project color change | H/H | engine/node version, exact legacy renderer, reopen matrix |
| ART contamination/license | M/H | clean-room spec/reviewer separation, provenance log, no resources |
| Default-look tuning by taste | H/H | fixed corpus, blind review, numeric gates, versioned recipe |
| CPU/GPU divergence | H/H | shared schema/IR, conformance vectors, device CI |
| f16 banding/underflow | M/H | promotion policy, gradient tests, f32 critical paths |
| Profile/DCP incorrectness | M/H | standard-derived implementation, reference CMS/tests |
| RAW camera coverage | H/H | fallback matrix, confidence flags, staged camera corpus |
| Tile seams/halos | M/H | declared halo, overlap tests, full-frame reference |
| Memory/VRAM on 45+ MP | H/H | tiled residency, eviction budget, streaming export |
| Over-complex graph | M/M | small typed contracts, vertical slices, measurable invalidation |
| Interactive vs final mismatch | M/H | same kernels/order, proxy only resolution, exact viewport refine |
| Legacy never removed | H/M | adoption metrics and explicit deprecation gates |

## 46. Definition of Done

Develop Engine 2 chỉ hoàn thành khi:

- UI quen thuộc và mọi existing control có documented semantic mapping.
- Old projects mở đúng renderer, không silent color shift.
- Scene path signed/unclamped tới declared display boundary; no accidental gamma roundtrip.
- Input/monitor/output profiles được tôn trọng; preview và export có traceable transforms.
- Exposure ±1 EV chính xác; tone/curves monotone; finite/deterministic stress suite đạt.
- Skin/hue/chroma/gamut/gradient gates đạt trên licensed corpus.
- RAW reconstruction, denoise/detail và output sharpen có quality modes explicit, không phụ thuộc image-size fallback ngầm.
- CPU/GPU node parity và settled preview/export parity đạt tolerance.
- 24/45 MP đạt budgets được chốt bằng hardware matrix; UI không block.
- Scopes đo đúng tap/profile và warnings tái tạo được.
- Mọi node có purpose/input/output/space/precision/clip/negative/gamut/class/dependency/order spec.
- Provenance audit xác nhận implementation clean-room.
- Legacy chỉ bị xóa sau compatibility/adoption/rollback gates.

## 47. Final Target Architecture

```text
                         ┌──────────── UI / DevelopSettings ────────────┐
                         │ unchanged controls, history, masks, presets │
                         └──────────────────┬───────────────────────────┘
                                            │ versioned recipe adapter
                                            v
Input ─> Profile/RAW Front End ─> immutable typed Scene Master
                                            │
                              ┌─────────────v─────────────┐
                              │ Versioned Develop DAG     │
                              │ technical -> render ->    │
                              │ creative -> local/detail  │
                              └──────┬─────────────┬──────┘
                                     │             │
                          CPU tiled executor   GPU graph executor
                                     └──────┬──────┘
                                            │ shared node contracts
                          ┌─────────────────┼──────────────────┐
                          v                 v                  v
                    Scopes/taps       Display view       Export sink
                                      gamut + ICC        gamut + ICC
                                      monitor encode     file encode
```

Kiến trúc này giữ “vỏ xe” iAi, thay hệ truyền động bằng graph color-correct, versioned và testable. ART đóng vai trò bằng chứng rằng một RAW editor trưởng thành cần stage ordering, camera/profile management, highlight reconstruction, operation-specific color spaces và preview coordination. Thiết kế iAi không sao chép những stage/classes đó: nó diễn giải các vấn đề thành contracts độc lập phù hợp Rust/WGPU năm 2026 và giữ quyền phát triển rendering character riêng.

---

### Appendix A — Stage specification matrix

| Stage | Purpose | Input → output | Space/precision | Clip/negative/gamut | Class | Ordering |
|---|---|---|---|---|---|---|
| Sensor normalize | black/white/linearization | mosaic code→relative mosaic | camera, f32 | no display clamp; bad metadata flagged | scene | first RAW |
| Mosaic correction | defect/CA/clip confidence | mosaic→corrected mosaic | camera, f32 | preserve recoverable over-range | scene | pre-demosaic |
| Demosaic | reconstruct RGB | mosaic→camera RGB | camera, f32 | signed allowed from reconstruction | scene | before camera transform |
| Camera characterize | accurate color | camera RGB→working RGB | matrix/DCP/ICC, f32 | no output gamut map | scene | pre-WB/render |
| WB/CAT | adapt illuminant | working scene→working scene | LMS/matrix, f32 | signed finite | scene | pre-exposure/tone |
| Exposure | EV scale | scene→scene | linear wide RGB, f32 | none | scene | pre-render |
| Tone zones | recover regions | scene + base→scene | log Y + RGB, f32 | smooth bounds, no cube clamp | scene | pre-render |
| Render transform | scene dynamic range→display reference | scene→display-linear | wide RGB/perceptual, f32 | shoulder/toe; preserve hue | boundary | before creative display nodes |
| Perceptual color | saturation/vibrance/mixer | display-linear→display-linear | OKLCh/Jz candidate, f32 | cusp-aware | perceptual | after render, before output map |
| Creative curves/grading | user look | display ref→display ref | declared domain, f32 | node policy explicit | display/creative | before final gamut |
| Local/detail | spatial edits | image+mask→image | log-Y/RGB, f32 | halo/noise guards | mixed | after suitable global bases |
| Gamut compression | make target-reproducible | display wide→target-linear | perceptual+profile, f32 | compress, never blind channel clamp | output | immediately pre-transform/encode |
| Display transform | correct view | target ref→monitor signal | ICC/CMS, f32→surface | quantize/dither at sink | display | final preview |
| Export transform | encoded file | target ref→file pixels | ICC/CMS, f32→u16/u8/f16 | encode once, dither if integer | output | final export |

### Appendix B — Audit classification summary

| Thành phần | iAi hiện tại | ART/reference | Vấn đề | Hướng xử lý |
|---|---|---|---|---|
| Scene master | f16 linear ProPhoto, signed/HDR | planar float working RGB | metadata/storage chưa thành generic contract | KEEP/REFACTOR |
| RAW preprocess | defect + basic highlight + AHD/Malvar | nhiều correction/recovery/demosaic | coverage/profile/quality fallback | REPLACE incremental |
| Exposure | scene path exact EV; legacy eased | ART exact EV nhưng clamp black | scene tốt, dual semantics | KEEP scene; REMOVE legacy |
| Tone | sigmoid + EV zones + modes | rich curves/tone equalizer | coupling và taste constants | REFACTOR nodes |
| Color Mixer | OKLCh V2 + legacy | HSL/color correction families | tốt nhưng target gamut/domain còn hẹp | KEEP/REFACTOR |
| Curves | 256 LUT, perceptual/luma/RGB modes | multiple tone modes/LUT | resolution/domain/ordering | REFACTOR |
| Gamut | OKLCh binary sRGB/P3 | profile/CMS infrastructure | custom ICC boundary thiếu | REFACTOR |
| CMS | ICC/LUT/monitor groundwork | mature input/output/proof chain | cần end-to-end contracts/cache | KEEP/REFACTOR |
| Preview | GPU interactive + exact CPU settle | shared stages scaled preview | duplicate math/proxies | REPLACE executor |
| Performance | Rayon/WGPU/cache pieces | OpenMP/SIMD/buffer reuse | invalidation không theo DAG | NEW graph cache |
| Scopes | histogram | histogram/proof/gamut facilities | thiếu waveform/parade/vectorscope/taps | NEW |

### Appendix C — Immediate next actions

1. Không tiếp tục thêm thuật toán mới vào `develop_scene.rs`/WGSL monolith trước khi chốt graph contracts.
2. Commit/freeze riêng worktree color pipeline hiện tại và chạy lại toàn suite để tạo baseline đáng tin.
3. Viết ADR cho Engine B, buffer metadata, node versioning và curve domains.
4. Xây no-op graph vertical slice qua UI→preview→commit→export.
5. Thu thập corpus hợp pháp trước khi tuning `iAi Natural v1`.
6. Truy nguyên DCP/ICC/CIE/paper gốc cho từng node; ART chỉ còn là black-box/reference trace.

### Appendix D — Autonomous overnight implementation protocol

Mục tiêu vận hành của lần triển khai tiếp theo là hoàn thành một lượt liên tục để chủ dự án chỉ cần nghiệm thu một lần vào sáng hôm sau. “Một lần” ở đây là một phiên coding tự chủ và một handoff cuối, không phải một commit khổng lồ hoặc bỏ qua kiểm thử trung gian.

Quy tắc thực thi:

1. Coding agent đọc toàn bộ tài liệu này và audit trạng thái bản sao trước khi sửa.
2. Tạo branch riêng từ checkpoint sạch. Không sửa bản gốc đã được lưu làm baseline.
3. Triển khai lần lượt các phase nhưng tự chạy quality gate sau mỗi vertical slice; nếu gate thất bại phải sửa ngay trước khi đi tiếp.
4. Không yêu cầu người dùng test/confirm giữa các phase, trừ khi gặp blocker thật sự về quyền truy cập, dữ liệu/license hoặc một lựa chọn làm thay đổi product contract không thể suy ra an toàn.
5. Giữ UI, project compatibility và legacy renderer hoạt động trong suốt phiên. Không xóa legacy trong lượt đầu.
6. Mỗi phase là commit nhỏ, có thể revert; cuối phiên cung cấp một branch hoàn chỉnh để người dùng test một lần.
7. Ưu tiên một end-to-end Develop2 vertical slice hoàn chỉnh hơn việc tạo nhiều module placeholder. Không đánh dấu hoàn thành nếu node vẫn là stub hoặc UI chưa đi tới preview/commit/export.
8. Tự chạy formatter, unit/integration tests, CPU/GPU parity, color golden, profile roundtrip, project reopen và release benchmarks. Test bị bỏ qua do thiếu hardware/data phải được liệt kê chính xác.
9. Nếu toàn bộ phạm vi 8 phase không thể hoàn thành đúng chất lượng trong một phiên, agent phải hoàn thành tối đa một milestone production-coherent, giữ build xanh và ghi rõ phần còn lại; không hạ quality gate để tuyên bố “xong”.
10. Cuối phiên tạo `DEVELOP_ENGINE_2_IMPLEMENTATION_REPORT.md` gồm commits, files, architecture thực tế, migrations, commands/results, known gaps, manual test checklist duy nhất và rollback instructions.

Thứ tự batch bắt buộc:

```text
baseline verification
 -> graph/contracts + engine versioning
 -> identity end-to-end slice
 -> input/scene integration
 -> exposure/tone/default render
 -> perceptual color/curves/gamut
 -> spatial/local/detail integration
 -> GPU/preview/export parity
 -> scopes/high-value additions khả thi
 -> full regression + release benchmark
 -> implementation report
```

Gate bàn giao buổi sáng:

- repository build được ở cấu hình development và release;
- toàn bộ automated tests liên quan xanh hoặc có blocker môi trường được chứng minh;
- project cũ vẫn mở bằng renderer cũ và không đổi màu ngầm;
- project mới có thể đi UI → preview → Apply → save/reopen → export bằng Develop2;
- preview settled và export nằm trong tolerance đã công bố;
- không có code/resource ART trong repository;
- working tree sạch hoặc chỉ còn artifacts/report đã được giải thích;
- có một checklist manual test ngắn, theo thứ tự, đủ để chủ dự án test đúng một lượt.
