# iAi Develop Color Engine — Architecture & Reconstruction Plan

## IMPLEMENTATION STATUS — 2026-08-11

- Branch đang triển khai: `codex/develop-engine-2-implementation`.
- Phase 0 — measurement foundation đã triển khai và commit: reference metrics, Middlebury observation harness, Cube++ known-illuminant probe, banding/acutance/performance baseline. Giới hạn còn ghi rõ: không có lịch sử/iAi-old executable để so sánh; Middlebury không phải sensor RAW ColorChecker; GPU parity ignored test hiện vẫn đỏ (`max 19/255`, `p99 6/255`).
- Phase 1 — đang thực hiện. Đã commit seam provenance bit-exact, parser DCP bounded, DCP transform dual-illuminant/WB/HueSatMap, prepared LUT hot path, camera ICC adapter bounded, external DCP provenance probe, deterministic resolver và sửa rawler zero-matrix fallback.
- Phase 1 còn lại trước checkpoint: discovery/manifest I/O; nối DCP vào RAW; mở JPEG-match mặc định `none` chỉ cho profile-backed path; provenance đầy đủ ở `SceneSource`; Cube++/profile integration gate; sau đó mới đánh giá tắt JPEG-match rộng hơn. ICC chưa được phép làm RAW default vì adapter hiện chỉ định nghĩa input `[0,1]`, trong khi scene RAW phải giữ signed/HDR.
- Phase 3/2/6/4/5/7/8: chưa code trong đợt này. Tiếp tục theo đúng thứ tự ở mục `RECOMMENDED IMPLEMENTATION ORDER`.
- Handoff chi tiết cho phiên kế tiếp: `CLAUDE_HANDOFF_COLOR_ENGINE_2026-08-11.md`.

Ngày điều tra: 2026-08-11
Phạm vi: CHỈ phân tích + lập kế hoạch. Không sửa code, không commit, không đổi working tree.
Nguồn iAi: `C:\Users\Admin\Documents\IAI_DEVELOP_ENGINE_2` (branch `codex/develop-engine-2-implementation`, HEAD `5669c58`).
Nguồn ART (tham khảo kỹ thuật, KHÔNG chép code): `C:\Users\Admin\Pictures\1111\ART` (nhánh RawTherapee: `rtengine`/`rtgui`).

> Tài liệu này bổ sung và **hiệu chỉnh** `DEVELOP_ENGINE_2_MASTER_PLAN.md`. Master plan đã đúng về chiến lược tổng thể (Hybrid Rebuild). Tài liệu này tập trung vào 3 câu hỏi user đặt ra ở phiên này — **(A) ảnh RAW mở lên xấu**, **(B) Color Mixer**, **(C) Temperature/WB** — và vào yêu cầu **giữ độ mượt realtime**. Mọi kết luận đều có bằng chứng `file:line` đã trace trực tiếp trong phiên này.

---

## A. EXECUTIVE SUMMARY

### A.1 iAi đang sai ở đâu (tóm tắt có bằng chứng)

Điều quan trọng nhất trước tiên: **hai chẩn đoán của các phiên trước đều chưa trúng gốc, và một chẩn đoán "hiển nhiên" mà tôi suýt ghi vào cũng SAI.** Cụ thể:

1. Phiên ~08-10 kết luận "output đúng, chỉ là display bug / From System" — **SAI** (chính memory đã tự bác).
2. Phiên ~08-11 kết luận "thiếu ~15% bão hòa trung gian" và vá bằng `enrich_scene_chroma` (vibrance nhúng scene master) — **chỉ là band-aid**; user vẫn thấy xấu.
3. Cám dỗ trong phiên này: "scene master lưu **linear ProPhoto** (`raw.rs:1361-1363`) nhưng WB/tone/gamut xử lý như **linear sRGB** → lệch primaries → nhợt/ám xanh". Nghe rất khớp triệu chứng (và khớp việc "chỉ RAW sai, JPEG đúng"), NHƯNG khi trace tiếp `build_scene_tone_for_scene` (`develop_scene.rs:808-817`) thì thấy nó **đã gộp ma trận ProPhoto→sRGB vào `wb_ev`** (`tone.wb_ev = wb_ev · to_srgb`). Vậy đường render thật **không lệch primaries**. Bài học: phải trace tới nơi, đừng dừng ở "có vẻ đúng".

Sau khi loại các giả thuyết sai, **các nguyên nhân THẬT của "ảnh RAW xấu" là**:

| # | Nguyên nhân gốc (đã verify) | Bằng chứng | Vì sao gây "nhợt/mờ đục/ám xanh" |
|---|---|---|---|
| RC1 | **Không có camera profile thật (DCP/ICC).** Màu camera chỉ dựa trên 1 ma trận chung của rawloader/rawler, single-illuminant, không HueSatMap. | `raw.rs:367-368` `cam_to_xyz_normalized()` → `camera_to_srgb_matrix()` | Màu nền sai lệch, đặc biệt foliage/da/trời; ma trận đơn không mô hình được illuminant/nonlinear camera. |
| RC2 | **"Default look" = fit theo JPEG nhúng của máy** (một chồng 3 phép fit + 1 vibrance): baseline RGB-gains → 3×3 color-matrix fit → `enrich_scene_chroma` → per-channel curve fit. Mặc định BẬT CẢ HAI (`(true,true)`). | `raw.rs:522-552`; `jpeg_match_mode()` `raw.rs:319-324`; `fit_camera_color_matrix` `develop_scene.rs:525-591`; `fit_camera_rgb_curve` `:485-518`; `CHROMA_ENRICH=0.85` `raw.rs:1440` | Đang **đuổi theo một ảnh JPEG tiêu dùng đã tone-map/nén-gamut/áp picture-style** bằng các fit tuyến tính bị chặn biên. Fit không bao giờ khớp hoàn toàn → dư cast, bạc màu; khi preview JPEG thiếu/yếu thì look tệ hẳn. Đây là **triết lý nền sai**, không phải tham số cần chỉnh. |
| RC3 | **Không gian làm việc thực chất là linear sRGB.** Scene lưu ProPhoto nhưng **bước đầu tiên** của render đã ép về sRGB rồi mới WB/tone/mixer/saturation/gamut. | `develop_scene.rs:808-817` (gộp `to_srgb`), `scene_to_working` `:1154-1196`, `gamut_clip_chroma`→sRGB `:1007-1012` | Toàn bộ chỉnh sửa nằm trong gamut sRGB nhỏ. Màu camera bão hòa (đỏ/cam/lá/hoàng hôn) bị nén/kẹp ngay từ bước 1 → không thể "kéo mạnh mà vẫn đẹp"; lưu ProPhoto **không mua được gì**. |
| RC4 | **Chuỗi tone RAW rất phức tạp và mang tính "đoán".** sigmoid cố định + auto-tone thích ứng + per-channel + blend max-RGB + nén chroma highlight + scene-contrast + restore chroma shadow + display-curve + camera-curve fit. | `develop_scene.rs:1143-1265`; `estimate_auto_tone`/`adaptive_map_ev` `:652-731`; commit `593fe57 "Tame adaptive RAW tone mapping"` | Nhiều tầng tone chồng nhau, `contrast` còn bị bỏ qua trong `sigmoid_params(_contrast)` (`:380`). Auto-tone nén dải có thể làm ảnh **phẳng/mờ đục**. |
| RC5 | **Ảnh mềm hơn JPEG ~1.2×** (đã đo phiên trước). AHD demosaic + `CAPTURE_SHARPEN=false`. | `raw.rs:1262` `CAPTURE_SHARPEN=false` | Không phải màu, nhưng góp phần cảm giác "mờ đục". |

Kết luận A.1: **"Xấu" của RAW là do RECIPE RENDER RAW (đặc trưng hóa camera + default-look bám-JPEG + không gian sРGB + tone nhiều tầng), KHÔNG phải do display transform** (display/monitor path dùng chung cho JPEG lẫn RAW, và JPEG bình thường → loại). Hai phiên trước đoán sai vì **chỉ đo mean-RGB/white-point (bất biến trên trục xám)** nên không phát hiện được lỗi ở kênh màu bão hòa.

### A.2 Vì sao ART đẹp hơn (nguyên lý)

ART không "đuổi theo JPEG". ART render **scene-referred, wide-gamut, có đặc trưng hóa camera thật**:
- **Camera characterization**: `xyz_cam` + DCP (dual-illuminant, HueSatMap, look/tone tables) — `rawimagesource.cc:559-594`, `dcp.cc convertColorSpace/setStep2ApplyState` (`:1300-1319`).
- **Working space rộng, có thể cấu hình** (ProPhoto mặc định), và **giữ nguyên** để chỉnh sửa — `ICCStore::workingSpaceMatrix` dùng xuyên suốt (`ipsaturation.cc:52`).
- **Temperature vật lý**: locus Planck (blackbody) 2000–4000K + daylight >4000K, tích phân với hàm phối màu CIE — `colortemp.cc:131-290`; có cả `temp2mul` và `mul2temp` (`:350-414`) nên đọc/hiển thị được Kelvin tuyệt đối.
- **Saturation/vibrance** neo theo luminance của working-profile, vibrance là hàm mũ trên residual có dấu — `ipsaturation.cc:43-82`.
- **Preview = cùng pipeline, khác scale**: chạy `ImProcFunctions::process` ở `skip/scale` thấp cho overview, full-res chỉ khi zoom 100% — `improccoordinator.cc:147-336`.

### A.3 Vì sao iAi bản cũ mượt hơn

Regression realtime đến từ **loạt commit 2026-08-08** (`dfa5080`→`0cb4387`, ~15 commit) tái cấu trúc RAW thành **scene-linear per-pixel**: unclamped linear working stage, grade/mixer/effects/detail/locals chạy linear trước output-encode, adaptive tone, gamut-map OKLCh mỗi pixel. Việc này **cải thiện ý đồ màu** nhưng làm mỗi fragment nặng thêm (OKLCh + gamut binary-search 18 vòng), buộc phải thêm cơ chế **proxy khi kéo / exact khi nhả** — sinh ra **divergence "nháy/nhảy sáng"** (proxy sáng hơn exact) mà chính memory ghi nhận. Bản cũ (trước 08-08) render RAW ở display-domain đơn giản hơn → nhẹ, mượt, nhưng màu kém "đúng".

### A.4 Chiến lược tổng thể

Giữ nguyên chủ trương **Hybrid Rebuild** của master plan, nhưng **tái ưu tiên** theo đúng nỗi đau user:

1. **P0 — Đo trước, sửa sau.** Hai phiên đoán sai bằng mắt. Bắt buộc dựng **instrumentation + tham chiếu đo được** (ColorChecker/known-illuminant) trước khi đụng recipe. Không tune look bằng mắt trên corpus không có ground-truth.
2. **P0/P1 — Thay "bám-JPEG" bằng đặc trưng hóa camera thật** (DCP/ICC + default tone thiết kế), đưa "Match Camera Preview" thành **tùy chọn có confidence**, mặc định TẮT.
3. **P1 — Mở working space thật sự rộng** cho toán chỉnh màu (không ép sRGB ở bước 1); gamut-compress chỉ ở biên output.
4. **P1 — Temperature nâng cấp**: Planckian+daylight, Kelvin tuyệt đối (temp2mul/mul2temp), vẫn 1 ma trận/khung.
5. **P2 — Color Mixer**: nâng từ sRGB lên perceptual wide-gamut, giữ UI/feel.
6. **Xuyên suốt — Bảo toàn realtime**: giữ kiến trúc proxy/cache hiện có (đã rất tốt), **khử divergence proxy↔exact** bằng cách cho hai đường **cùng model màu** (chỉ khác độ phân giải), theo đúng nguyên lý ART preview.

---

## B. CURRENT ARCHITECTURE (iAi thực tế hôm nay)

### B.1 Đường đi RAW (đã trace)

```text
rawloader/rawler decode                              raw.rs:249,266,329
  → active area + black/white normalize (per-kênh)   raw.rs:202,360-363
  → as-shot WB gain trên mosaic (green=1)            raw.rs:116-127,355
  → correct isolated bayer defects                   raw.rs:393
  → opposed-channel highlight inpaint (pre-demosaic)  raw.rs:397,1055
  → AHD demosaic (≤ AHD_MAX_PIXELS) / Malvar/bilinear raw.rs:404-457,778
  → camera RGB → linear sRGB (ma trận rawloader)      raw.rs:367-368,241
  → **linear sRGB → linear ProPhoto**, lưu f16        raw.rs:1361-1363  (write_scene)
  → [CAPTURE_SHARPEN=false]                           raw.rs:494,1262
  → EXIF orientation                                  raw.rs:500
  ── DEFAULT-LOOK "match camera JPEG" (mặc định BẬT) ──
  → baseline_rgb_gains_for_scene (fit 3 gain)         raw.rs:526; develop_scene.rs:467
  → fit_camera_color_matrix (fit 3×3, bám JPEG)       raw.rs:533; develop_scene.rs:525
  → enrich_scene_chroma (vibrance nhúng, 0.85)        raw.rs:546,1451
  → fit_camera_rgb_curve (fit per-channel curve)      raw.rs:548; develop_scene.rs:485
  → render_default_look → bake px16 (RGBA16)          raw.rs:556; develop_scene.rs:1708
  → Canvas.icc_profile = sRGB (pixel đã bake sRGB)    raw.rs:561-564
  → metadata.develop_working_space = LinearProPhoto   raw.rs:567-568
     (scene master giữ Arc<SceneSource> để Develop render live)  raw.rs:559
```

Đường **render mỗi pixel** (khi mở Develop / commit) — `SceneToneData` trong `develop_scene.rs`:

```text
scene ProPhoto f16
  → wb_ev = CAT16_WB(sRGB) · 2^EV · (ProPhoto→sRGB)   :802-817,855-862  (gộp primaries ✔)
  → tone_eq regional gain (nếu H/S/W/B bật)           :1156-1160
  → apply_scene_grade (split-tone)                    :1106-1123,1161
  → per-channel sigmoid tone_map                      :1143-1148,1163-1167
  → blend về max-RGB ratio (giữ hue)                  :1168-1188
  → compress_highlight_chroma                          :1040-1072,1185
  → apply_scene_contrast (gamma quanh mid-gray)       :1089-1103,1189
  → restore_shadow_chroma (nếu Perceptual)            :1077-1085,1190
  → filmlike_clip → gamut_clip_chroma(→sRGB, OKLCh)   :1018-1034,1007-1012,1200
  → linear_to_srgb (OETF)                             :1201-1203
  → apply_display_curves (luma/RGB point curves)      :1217-1246
  → clamp [0,1] → RGBA16 tile                          :1205
```

Compositor GPU dựng lại phần lớn toán này trong WGSL (twin viết tay): `compositor.wgsl` (`dev_white_balance`, `dev_exposure_linear`, `dev_tone_delta`, masks, `dev_scale_chroma_around_luma`...). Display cuối: `cms.rs build_display_lut` (17³ 3D LUT soft-proof + monitor ICC, mặc định identity), áp trên surface. **Không double-gamma** (test `raw.rs:1573`).

### B.2 Đường raster (JPEG/PNG/TIFF)

`SceneSource::from_display_tiles` (`develop_scene.rs:233`) → linearize sRGB, look = `Identity`, `color_pipeline.working = LinearSrgb` (`:327-328`). Không lệch primaries, không bám-JPEG. **Đây là lý do JPEG trông ổn còn RAW thì không** — hai đường có recipe rất khác nhau.

### B.3 Representation & concurrency

- Scene master: RGBA interleaved, RGB = f16 bits (ProPhoto cho RAW / sRGB cho raster), unclamped, cho âm & >1. `SceneSource` `:177-192`.
- Compute: Rust/WGSL f32; output RGBA16 TileMap → compositor.
- CPU: Rayon theo hàng/tile; worker single-flight, loại stale bằng job id.
- Preview realtime (`develop_preview.rs`): proxy tone-independent được cache theo layer/viewport; drag dùng proxy, nhả dùng exact per-pixel shader; throttle recompose 24–140ms (`:15-32`), throttle rebuild proxy 48–400ms (`:230-256`).

---

## C. ART ARCHITECTURE FINDINGS (nguyên lý, không chép code)

### C.1 Decode / RAW / demosaic
`RawImageSource` tách sensor-correct → demosaic → color-convert. `simpleprocess.cc` điều phối output theo stage; `improccoordinator.cc` điều phối preview/cache. Nguyên lý học được: **tách rõ các domain (sensor → camera RGB → working)**; highlight reconstruction xảy ra ở **camera/RAW domain trước** mọi creative transform (phân biệt *reconstruction* với *compression*).

### C.2 Color management (điểm ART mạnh nhất)
`camera RGB → XYZ (xyz_cam / DCP) → CAT → working RGB` (`rawimagesource.cc:559-594`). DCP mang **dual-illuminant + HueSatMap + look/tone** (`dcp.cc`). Working profile là **first-class**, mọi op đọc `workingSpaceMatrix`. Boundary input→working và working→monitor/output là **tường minh** (`iprgb2out.cc`). → iAi cần: profile-aware boundary + đặc trưng hóa camera thật, thay vì "bám-JPEG".

### C.3 Temperature (colortemp.cc)
- `blackbody_spect` (Planck) cho 2000–4000K; `daylight_spect` cho 4000–25000K; tích phân phổ × CIE colour-match → XYZ (`:131-290`).
- `temp2mul(temp, green, equal, r,g,b)` và `mul2temp(...)` (`:350-414`): **hai chiều** — áp WB và **suy ngược ra Kelvin tuyệt đối** để hiển thị.
- Học: (1) dùng **Planckian** cho vùng ấm (tungsten), không chỉ daylight; (2) phơi **Kelvin + tint tuyệt đối**; (3) as-shot WB của RAW đọc ra được nhiệt độ thật.

### C.4 Saturation / Vibrance (ipsaturation.cc)
`l = rgbLuminance(r,g,b, ws)` (working-profile luma); `rl=r-l`; vibrance = `SGN·|rl|^vib` (mũ trên residual); saturation = `l + s·rl`. → iAi `scale_linear_chroma_around_luma` (`develop/color.rs:187`) **đã theo đúng nguyên lý này**; khác biệt còn lại là **không gian** (ART = working wide; iAi = sRGB).

### C.5 Tone / curves
`iptonecurve.cc` có base curve, film-like clipping, contrast curve, nhiều mode; RGB curves tách riêng (`iprgbcurves`). Học: **master tone theo luminance/perceptual có mục tiêu hue/chroma rõ**; RGB curves là creative per-channel (được phép đổi hue, phải ghi nhãn).

### C.6 Preview / performance
`improccoordinator.cc`: cùng `ImProcFunctions::process`, chỉ đổi `scale/skip`; `highDetailNeeded` chỉ bật khi crop `skip==1` (100%) hoặc `M_HIGHQUAL` (`:163-336`). OpenMP + SIMD + buffer reuse. → **Nguyên lý vàng cho realtime iAi**: preview & final **cùng model/thứ tự**, chỉ khác resolution — thì preview không đổi màu khi settle (khử đúng divergence hiện tại).

---

## D. GIT REGRESSION ANALYSIS

| Mốc | Trạng thái | Hệ quả |
|---|---|---|
| `8749fa6` 2026-07-18 | Initial iAi (rebrand pkb). Develop RAW **display-domain đơn giản**. | Nhẹ, mượt, màu "tạm". |
| `dfa5080`→`0cb4387` **2026-08-08** (~15 commit) | **Scene-linear RAW rework**: `Add unclamped linear color working stage`, `Run RAW color mixer/effects/detail/local in linear`, `Grade RAW global color before output encoding`, `Adapt RAW tone curves to scene range`, `Tame adaptive RAW tone mapping`, `Match RAW baseline colour to camera preview`, `Restore chroma in colored shadows`, `Add scene-linear split grading`. | (+) Ý đồ màu tốt hơn. (−) Per-pixel nặng (OKLCh + gamut 18 vòng) → cần proxy/exact → **divergence nháy/nhảy sáng**. (−) Default-look **bám-JPEG** → mờ đục. `7a11a43` thêm +109 dòng WGSL; `390682b` +147 dòng scene tone. |
| `1d83111` 2026-08-09 | Checkpoint + Engine 2 plan. | Base cho worktree Engine 2. |
| Engine 2 branch → `5669c58` | Foundation graph + `enrich_scene_chroma` (`973e8e5`) + fix click-flash Mixer (`a2128e9`). | Vá triệu chứng, chưa trị gốc. |

**Cần khôi phục nguyên lý (không phải code) từ bản cũ**: đường preview **nhẹ, một-model** (bản cũ display-domain vốn không có divergence vì proxy và exact cùng model). **Cần giữ từ bản mới**: scene-linear, unclamped, OKLCh mixer, gamut-map hue-preserving. → Hợp nhất: **một model màu chung cho cả proxy và exact**, proxy chỉ giảm resolution.

---

## E. ROOT CAUSE ANALYSIS

| Ưu tiên | Vấn đề (symptom) | Root cause | Bằng chứng | Impact |
|---|---|---|---|---|
| **P0** | RAW nhợt/mờ đục/ám xanh ngay khi mở | **RC2** Default-look bám-JPEG (fit 3×3 + curve + vibrance) trên nền **RC1** không có camera profile thật | `raw.rs:319-324,522-552`; `develop_scene.rs:525-591,485-518` | Sai/bạc màu toàn cục, nhất là foliage/da/trời |
| **P0** | Kéo màu bão hòa "đục", không "trong" | **RC3** Toán màu chạy trong **sRGB** (ProPhoto bị ép về sRGB ở bước 1) | `develop_scene.rs:808-817,1007-1012` | Gamut nhỏ → kẹp/nén sớm; "kéo mạnh" mất chất |
| **P0** | Preview↔commit "nhảy sáng"/nháy | Proxy (drag) và exact (release) **khác model** (proxy chroma sáng hơn) | `develop_preview.rs:97-135,458-548`; memo mục 2 | Mất tin cậy: màu preview ≠ commit |
| **P1** | Ảnh phẳng/mờ đục ở midtone | **RC4** Tone nhiều tầng + auto-tone nén dải; `contrast` bị bỏ trong sigmoid | `develop_scene.rs:380,652-731,1143-1265` | Contrast/độ trong midtone kém |
| **P1** | Warm/tungsten hơi lệch; không có Kelvin | Temperature chỉ dùng **daylight locus**, không Planckian; không `mul2temp` | `cat16.rs:78-176` | WB cực ấm hơi sai; UX thiếu Kelvin tuyệt đối |
| **P1** | Ảnh mềm ~1.2× JPEG | **RC5** AHD + `CAPTURE_SHARPEN=false` | `raw.rs:1262` | Cảm giác "mờ" |
| **P2** | Drift CPU/WGSL theo thời gian | Twin toán viết tay hai nơi | `compositor.wgsl` vs `develop_scene.rs` | Bảo trì + parity risk |
| **P2** | 1 NEF decode đen | rawloader/rawler fail file cụ thể | memo mục 3 | Mất ảnh (hiếm) |

> Lưu ý phản-chẩn-đoán: **KHÔNG** liệt "lệch primaries ProPhoto/sRGB" là root cause — đã verify `wb_ev` gộp `to_srgb` (`develop_scene.rs:808-817`) nên đường render không lệch. Đừng để phiên sau "sửa" nó rồi làm hỏng.

---

## F. TARGET COLOR ARCHITECTURE (pipeline đề xuất)

```text
Input
 ├─ RAW: sensor normalize → mosaic correct → highlight RECONSTRUCT (camera domain)
 │        → demosaic → **camera characterize (DCP/ICC dual-illuminant + HueSatMap, fallback matrix)**
 │        → scene master **linear wide-gamut** (ProPhoto giữ nguyên, KHÔNG ép sRGB)
 └─ Raster: embedded ICC decode → scene (Identity), wide working
        ↓  (mọi buffer mang metadata: primaries/white/transfer/scene|display/range/precision)
WB / CAT16|CAT02  (Temperature Planckian+daylight, Kelvin tuyệt đối)   [scene, working-space]
Exposure  (2^EV thuần)                                                  [scene]
Highlight reconstruct residual / Tone zones (log2-Y, edge-aware)        [scene]
        ↓
**Scene → Display render transform**  (iAi Natural v1: sigmoid neo mid-gray, hue-preserving)
        ↓  [display-linear reference, white=100nit SDR]
Perceptual color: Saturation / Vibrance / **Color Mixer** / Grading / Curves  [OKLCh/JzCzHz, wide]
        ↓
Output gamut compression (profile-aware: sRGB/P3/ICC boundary)          [output]
        ↓                              ↓
Display transform (monitor ICC+encode)   Export transform (output ICC+encode)  — encode 1 lần
```

Ràng buộc: reconstruction trước demosaic; WB/Exposure trước tone; **gamut compress chỉ ở biên output** (không kẹp giữa các node); monitor transform **không** đi ngược vào graph. Working-space là **wide** cho toàn bộ toán chỉnh sửa; sRGB chỉ là một *output target*.

Quyết định không gian (v1): giữ **linear ProPhoto** làm scene/working master (đã có hạ tầng), nhưng **thực sự chỉnh sửa trong đó** (không ép sРGB ở bước 1). OKLab hiện dùng đường `linear_srgb→oklab`; cần bản **OKLab-from-working** (ProPhoto→XYZ→LMS→OKLab) để mixer/gamut đúng trong wide-gamut. Benchmark ACEScg + JzCzHz cho HDR sau.

---

## G. TARGET REALTIME ARCHITECTURE

Giữ kiến trúc cache/proxy hiện có (rất tốt: base tone-independent, memo theo WB+EV, throttle, ROI). **Thay đổi cốt lõi = xóa divergence** theo nguyên lý ART:

- **Interactive path (đang kéo)**: cùng graph/kernels/constants, chỉ giảm **resolution** (proxy/mipmap) và số vòng spatial; **cùng model màu** (không có "đường chroma proxy sáng hơn" riêng). Cancel frame cũ, reuse buffer, chỉ render ROI hiển thị.
- **Final path (nhả/zoom100%/export)**: full-res, full-precision, cùng model.
- **Bất biến bắt buộc**: đổi resolution KHÔNG được đổi color model/stage-order/transfer. Nhả chuột → refine viewport exact trước, rồi full-image nền. Badge Interactive/Refining/Full quality giữ nguyên.

Chi phí per-fragment (OKLCh + gamut 18 vòng) là lý do phải proxy. Hai lối xử lý (chọn ở Phase 6, đo trước):
1. **Cusp-LUT gamut** thay binary-search (đã có benchmark tại `gamut_map.rs:200-286`) → rẻ hơn nhiều/fragment, cho exact-per-pixel realtime mà vẫn 1 model.
2. Nếu vẫn nặng: giữ proxy nhưng **proxy dùng đúng cùng OKLCh/gamut** ở độ phân giải thấp (khác pixel-count, không khác math) → divergence → 0.

Target đo được (chốt sau khi có máy đo ở Phase 0): slider→frame p95 < 50ms trên RAW 24MP; không full-res render khi đang kéo; 0 lần "nhảy sáng" khi nhả.

---

## H. COLOR MIXER REDESIGN

Hiện trạng (đã đọc kỹ, **chất lượng tốt**): `mixer.rs` + `develop/color.rs`.
- V2 mặc định: raised-cosine crossfade giữa các band-center OKLCh; chỉ 2 band kề chồng nhau; tổng trọng số = 1; non-negative (`mixer_basis_v2` `:118-182`).
- Phân loại theo OKLab-hue của **màu display** người dùng thấy (`classification`), chỉnh trong **linear sRGB** (`apply_color_linear_classified` `:44-135`).
- Hue giữ L/C, Sat giữ L/h, Lum giữ C/h (test `v2_axes_preserve...` `:518-560`); bảo vệ neutral bằng chroma-confidence liên tục; lum-guard chống đen/trắng.
- Realtime: mixer tính CPU trên proxy low-res, GPU tái dựng qua gate-LUT (`:189-193,304-308`).

Điểm cần nâng (không đập UI, giữ 8 band + 3 trục):
1. **Không gian**: chuyển classify+edit sang **perceptual wide-gamut** (OKLCh-from-working / JzCzHz) thay vì sRGB → "kéo mạnh mà vẫn đẹp", da tự nhiên, không bạc.
2. **Gamut**: sau tổng edit, **một** lần gamut-compress theo profile output (không kẹp mỗi band); dùng cusp-aware để không tạo cusp discontinuity.
3. **Feather/overlap**: giữ raised-cosine (đã sum-to-one, liền mạch 0/360 — test `:509-514`); có thể tăng band mượt bằng basis chuẩn hóa rộng hơn nếu đo thấy chuyển band gắt.
4. **Skin**: weight liên tục theo hue/chroma/lightness (đã có nền), **không** hard-mask; đảm bảo Vibrance đổ vào chroma thấp/trung (đã đúng `vib_w` `:124`).
5. **Realtime/parity**: cho proxy và exact **cùng OKLCh model** (mục G) để mixer preview = commit.
6. **Interpolation/banding**: LUT gate 0..1 + curve; nâng độ phân giải LUT nếu đo thấy banding trên ramp; giữ classify theo display, edit theo working (chống band-shift do tone).

Bất biến kiểm thử: hue-rotate không đổi L/C; sat=0 tiến đều về neutral; ramp hue liên tục qua cusp; da giữ hue dưới exposure/contrast; mixer preview↔commit ΔE nhỏ.

---

## I. COLOR TEMPERATURE REDESIGN

Hiện trạng (**đã đúng nguyên lý CAT16**, không phải cộng/trừ RGB): `cat16.rs` — Temp→mired quanh D65, Tint→Duv, dựng 1 ma trận linear-sRGB→linear-sRGB von-Kries trong không gian CAT16, luma-preserving, (0,0)=identity. Eyedropper Gauss-Newton (`neutralize` `:189`).

Nâng cấp (giữ UI ±200, thêm hiển thị Kelvin):
1. **Locus**: thêm **Planckian (blackbody)** cho vùng ấm (<~4000K) + **daylight** cho vùng lạnh, chọn theo CCT — như ART (`colortemp.cc:260-290`). Hiện chỉ daylight → tungsten hơi lệch.
2. **Kelvin tuyệt đối**: dựng `temp↔mul`/`temp↔matrix` hai chiều (kiểu `mul2temp`) để **hiển thị + nhập K/tint tuyệt đối**, và để **as-shot WB của RAW đọc ra Kelvin thật** (hiện slider chỉ là delta ±200 quanh as-shot).
3. **Không gian áp WB**: áp CAT trong **scene wide-gamut** (sau khi mở working thật), 1 ma trận/khung — CPU & GPU 1 phép nhân (giữ hiệu năng).
4. **Quan hệ với các stage**: WB trước Exposure/Tone/Mixer (đúng thứ tự hiện tại). Grading **không** đổi WB metadata (tách "technical WB" khỏi "creative balance").
5. **Biên/gamut**: cap điều kiện CAT (tránh nổ gain ở CCT cực trị); fallback hữu hạn (đã có `.max(1e-6)` `:159-161`).
6. **RAW vs bitmap**: RAW áp trên scene-linear đã đặc trưng hóa camera; bitmap áp trên scene linear hóa từ ICC nhúng. Cùng model, khác nguồn white.

---

## J. IMAGE DECODER / DISPLAY REDESIGN (từ file tới màn hình)

Mục tiêu: RAW mở lên **trong, đúng màu, đúng gamma** ngang ART, **không** phụ thuộc JPEG nhúng.

1. **Decode**: giữ rawloader (primary) + rawler (fallback CR3/nhiều máy). Bổ sung: linearization table nếu có, masked pixels, per-kênh black/white (đã có `raw_levels` `:202`). Điều tra file NEF decode-đen (`HP0917`).
2. **Highlight reconstruction**: giữ opposed-channel inpaint (`:1055,1180`), bổ sung confidence map + multi-scale (P2); tách khỏi highlight-*compression* của tone.
3. **Camera characterize (THAY RC1+RC2)**: đọc **DCP/ICC** (dual-illuminant, ForwardMatrix/ColorMatrix, HueSatMap, look/tone) từ spec DNG/ICC — clean-room. Fallback = ma trận rawloader hiện tại + flag provenance. **"Match Camera Preview" → tùy chọn, mặc định TẮT.**
4. **Working master**: giữ ProPhoto f16 **và chỉnh sửa trong đó** (mục F). Sửa OKLab/gamut để nhận working-space (mục H).
5. **Default render `iAi Natural v1`**: sigmoid neo mid-gray (đã có, `SIGMOID_BASE_C`), hue-preserving intensity, chroma giảm theo compression/gamut; **middle gray ổn định từ profile baseline + robust scene stats** (thay auto-tone "đoán"); Contrast phải **thực sự** vào tone (sửa `sigmoid_params(_contrast)` bỏ contrast `:380`, hoặc chuyển Contrast sang node pivoted rõ ràng).
6. **Softness (RC5)**: đánh giá capture-sharpen **nhẹ** (1 vòng, gain ~0.35) có guard chống bead; hoặc để Detail stage. Đo acutance vs JPEG, không bật mù quáng.
7. **Display/monitor**: giữ `build_display_lut` (soft-proof + monitor ICC). Đảm bảo scene render **không** double-encode; monitor transform chỉ là view (không bake vào document). Giữ test `raw.rs:1573`.
8. **Metadata**: làm rõ tách bạch: `canvas.icc_profile` = profile của **pixel đã bake** (sRGB), `metadata.develop_working_space` = primaries của **scene master** (ProPhoto). Ghi chú để không ai nhầm là "double".

---

## K. CPU / GPU PLAN

| Stage | Hiện tại | Đề xuất | CPU/GPU | Lý do |
|---|---|---|---|---|
| RAW decode/demosaic/reconstruct | CPU (rayon) | CPU | **CPU** | I/O-bound, thuật toán rẽ nhánh; 1 lần/ảnh |
| Camera characterize (DCP/matrix/HueSat) | CPU (fit JPEG) | CPU bake vào scene master | **CPU** | 1 lần/ảnh; HueSatMap = 3D interp |
| WB/CAT | CPU+WGSL (twin) | 1 ma trận, sinh từ 1 schema | **HYBRID** | 1 phép nhân/pixel; rẻ cả hai |
| Exposure | CPU+WGSL | 1 nhân | **HYBRID** | rẻ |
| Tone zones + render transform | CPU+WGSL (twin) | LUT log2 dùng chung + edge-aware pyramid | **HYBRID** | LUT rẻ; spatial base cache CPU |
| Saturation/Vibrance | CPU+WGSL | perceptual wide | **HYBRID** | pointwise |
| Color Mixer | CPU proxy + WGSL gate | cùng OKLCh model 2 đường | **HYBRID** | mục G/H |
| Gamut compress | CPU 18-vòng; WGSL hull xấp xỉ | **cusp-LUT profile-aware** (texture) | **GPU (LUT) / CPU (LUT-gen)** | khử divergence + rẻ/fragment |
| Curves (master/RGB) | CPU LUT + WGSL | LUT ≥4096 hoặc spline | **HYBRID** | tránh banding |
| Display/monitor ICC | CPU LUT 17³ → GPU sample | giữ | **GPU sample** | cache theo profile-hash |
| Detail/sharpen/denoise | CPU commit | tiled spatial dùng chung | **CPU→GPU sau** | halo/seam cần cẩn thận |

Nguyên tắc: **sinh constants/LUT/matrix từ MỘT schema dùng chung** cho CPU & WGSL (khử twin drift), chạy conformance vectors chung. Fuse các pass pointwise thành 1 shader; spatial tách pass. Không cho fast-math phá monotonic/finite nếu chưa đo.

---

## L. FILES TO MODIFY (đề xuất, chưa code)

| File | Module/hàm | Trách nhiệm hiện tại | Vấn đề | Sửa dự kiến | Rủi ro |
|---|---|---|---|---|---|
| `src/formats/raw.rs` | `decode_raw_from` `:329`; `jpeg_match_mode` `:319`; `enrich_scene_chroma` `:1451`; `write_scene` `:1360` | decode + default-look bám JPEG + store ProPhoto | RC1/RC2/RC5 | Thêm đường DCP/ICC; đổi mặc định match=OFF; gỡ dần vibrance-patch khi có profile; đánh giá capture-sharpen | Đổi look mở RAW → cần golden + đối chiếu |
| `src/core/develop_scene.rs` | `build_scene_tone_for_scene` `:802`; `SceneToneData` `:1087-1265`; `estimate_auto_tone` `:652`; `sigmoid_params` `:380` | tone/render transform; auto-tone; primaries compose | RC3/RC4; contrast bị bỏ | Chỉnh math sang wide-working (OKLab-from-working); tách default-render khỏi auto-tone "đoán"; đưa Contrast vào tone | Regression tone toàn cục |
| `src/core/develop/mixer.rs`, `develop/color.rs` | mixer V2 + apply | mixer sRGB | RC3 (mixer) | Nâng classify+edit lên perceptual wide; gamut 1 lần | Đổi kết quả mixer → golden |
| `src/core/cat16.rs` | `wb_matrix`, `neutralize` | CAT16 daylight | daylight-only, không Kelvin | Thêm Planckian; temp↔matrix hai chiều; Kelvin readout | Đổi hành vi Temp slider |
| `src/core/gamut_map.rs` | `map_to_output_gamut` | OKLCh binary sRGB/P3 | GPU-heavy, sRGB-target cứng | Cusp-LUT + profile-aware boundary | Sai số LUT vs binary |
| `src/core/perceptual_color.rs` | `working_rgb_to_perceptual` | OKLab từ linear-sРGB | cần wide-working | Thêm OKLab-from-working đúng | Parity |
| `src/app/render/develop_preview.rs` | `build_develop_gpu_preview`, `raw_color_runs_per_pixel` | proxy/exact split | divergence | Một-model 2-resolution | Perf/lag nếu bỏ proxy sai cách |
| `src/gpu/compositor.wgsl` + `compositor.rs` | develop twin | mirror toán | twin drift | Sinh từ schema chung; cusp-LUT | Shader parity đa GPU |
| `src/core/cms.rs` | `build_display_lut` | monitor/proof | ổn | Cache theo profile-hash; giữ view-only | Ít |

### File mới đề xuất
| File mới | Trách nhiệm | Lý do |
|---|---|---|
| `core/camera_profile/` (dcp.rs, icc_camera.rs) | Đọc DCP/ICC dual-illuminant + HueSatMap (clean-room từ spec DNG/ICC) | Thay RC1/RC2 |
| `core/color_pipeline/schema.rs` | 1 nguồn constants/matrix/LUT cho CPU+WGSL | Khử twin drift |
| `tests/color_reference/` | Harness đo ColorChecker/known-illuminant, ΔE, banding, acutance | P0 đo trước sửa |

---

## M. FILES TO PRESERVE (tuyệt đối không đụng ngoài phạm vi)

- Toàn bộ **UI/UX Develop**, slider range, action flow, history, masks, presets (`src/ui/develop.rs`, `src/app/actions/develop.rs`).
- `DevelopSettings` public contract + serde versioning (`develop/settings.rs`) — chỉ thêm field, không phá cũ.
- **Kiến trúc cache/proxy realtime** (`develop_preview.rs` khung caching, throttle, ROI) — chỉ chỉnh model màu bên trong, giữ khung.
- **Đường raster/Identity** (`from_display_tiles`) và parity goldens — đang đúng, JPEG ổn.
- Document/serialization `.iai` (`formats/iai.rs`), `develop_engine_version`, project compatibility.
- CMS monitor/proof groundwork (`cms.rs`) — chỉ cache, không đổi ngữ nghĩa view.
- Vector/print/text/psd… (ngoài phạm vi color engine).

---

## N. IMPLEMENTATION PHASES (mỗi phase có checkpoint riêng ở mục dưới)

```text
PHASE 0 — Baseline, instrumentation & reference (đo trước, sửa sau)
PHASE 1 — Camera characterization thật (DCP/ICC) + default-look OFF-JPEG
PHASE 2 — Working-space thật rộng cho toán màu (bỏ ép sRGB bước 1) + OKLab/gamut wide
PHASE 3 — Default render "iAi Natural v1" + Contrast/tone làm lại (bỏ auto-tone đoán)
PHASE 4 — Temperature Planckian + Kelvin tuyệt đối
PHASE 5 — Color Mixer lên perceptual wide + gamut 1-lần
PHASE 6 — Realtime unification (một-model 2-resolution) + cusp-LUT gamut + schema CPU/WGSL
PHASE 7 — Detail/softness + scopes (waveform/parade/vectorscope) + soft-proof/gamut warning
PHASE 8 — Validation, golden, migration/default, cleanup twin & JPEG-match
```

Thứ tự ưu tiên theo nỗi đau user: **P0 → P1 → P3 → P2 → P6** giải quyết "ảnh xấu + nhảy sáng" sớm nhất; P4/P5 nâng chất; P7/P8 hoàn thiện.

Mỗi phase: commit nhỏ revertable, giữ legacy renderer + project compatibility hoạt động suốt, tự chạy quality gate sau mỗi vertical slice.

---

## PHASE CHECKPOINTS

**PHASE 0 — Baseline/instrumentation**
- Visual: 0 thay đổi output (chỉ thêm đo).
- Đo: harness render iAi-current/iAi-old/ART trên corpus; ΔE00/ΔEOK, hue-error, chroma/L drift, banding, acutance; có ít nhất 1 ColorChecker/known-illuminant.
- Regression: toàn suite hiện tại xanh; goldens ổn.
- DoD: "xấu" được **định lượng** (biết chính xác kênh/hue/tone nào lệch), không còn đoán bằng mắt.

**PHASE 1 — Camera characterization**
- Visual: RAW có DCP → màu neutral/accurate; không cần JPEG nhúng.
- Đo: ΔE trên ColorChecker < ngưỡng chốt; foliage/skin/sky ổn qua nhiều illuminant.
- Regression: file không DCP → fallback matrix + flag, không tệ hơn hiện tại; JPEG-match OFF không làm ảnh tệ hơn ON cũ trên corpus.
- DoD: default-look không phụ thuộc JPEG nhúng; provenance rõ.

**PHASE 2 — Working-space wide**
- Visual: màu bão hòa "trong" hơn, kéo Saturation/Mixer không bạc sớm.
- Đo: gamut coverage tăng; hue-preserved qua cusp; neutral bit-ổn.
- Regression: neutral/parity goldens giữ; export sRGB không đổi ngoài chủ đích.
- DoD: toán màu chạy wide, gamut-compress chỉ ở biên output.

**PHASE 3 — Default render + Contrast/tone**
- Visual: midtone "pop" tự nhiên, không phẳng/mờ; grey giữ grey; da tự nhiên.
- Đo: monotone + derivative bounded; mid-gray ổn định; Contrast thực sự đổi độ dốc.
- Regression: blind review thắng baseline không category loss.
- DoD: `iAi Natural v1` + Neutral/Filmic recipe; auto-tone "đoán" gỡ bỏ.

**PHASE 4 — Temperature**
- Visual: warm/tungsten tự nhiên; Kelvin hiển thị đúng; as-shot RAW đọc đúng K.
- Đo: temp↔mul roundtrip; neutral luma bất biến; monotone theo slider.
- Regression: (0,0)=identity vẫn đúng; eyedropper vẫn hội tụ.
- DoD: Planckian+daylight, Kelvin tuyệt đối, 1 ma trận/khung.

**PHASE 5 — Color Mixer**
- Visual: kéo mạnh hue/sat/lum vẫn đẹp, chuyển band mượt, da bảo toàn.
- Đo: hue giữ L/C; sat=0 → neutral đều; ramp liên tục 0/360 & cusp.
- Regression: mixer preview↔commit ΔE nhỏ (khử divergence).
- DoD: mixer perceptual wide + gamut 1-lần, UI không đổi.

**PHASE 6 — Realtime unification**
- Visual: KHÔNG "nhảy sáng" khi nhả; kéo mượt.
- Đo: slider→frame p95 < ngưỡng (chốt ở P0 theo máy); 0 full-res render khi kéo; proxy↔exact ΔE ~ lượng tử hóa.
- Regression: pan/zoom/histogram/export không đổi.
- DoD: một-model 2-resolution; cusp-LUT; CPU/WGSL sinh từ schema chung + conformance.

**PHASE 7 — Detail/scopes**
- Visual: acutance ~JPEG không bead; scopes đúng tap/profile.
- Đo: acutance đo được; scope tái tạo điều kiện synthetic đã biết.
- DoD: capture-sharpen tùy chọn có guard; waveform/parade/vectorscope + gamut/soft-proof warning.

**PHASE 8 — Validation/migration/cleanup**
- Visual: project cũ mở KHÔNG đổi màu ngầm; project mới đi UI→preview→Apply→save→reopen→export bằng engine mới.
- Đo: reopen matrix mọi version; settled preview == export trong tolerance.
- DoD: gỡ twin/JPEG-match sau khi có golden + adoption + rollback; báo cáo signed.

---

## TEST MATRIX (thiết kế, chưa chạy gì đổi hệ thống)

Bộ ảnh (mỗi loại có RAW + JPEG/PNG/TIFF nếu có): portrait/skin đa sắc tộc; landscape; màu bão hòa mạnh (hoa/vải/neon-LED); shadow sâu; highlight cháy có màu; tungsten; daylight; mixed lighting; grayscale/ramp; wide-gamut; sky/foliage/sunset.

So sánh ở 3 tap (scene / display / output-encoded), chuẩn hóa crop/orientation/profile, KHÔNG ép cùng pixel nếu default look khác:
```text
iAi current  |  iAi old (pre-2026-08-08)  |  ART  |  target architecture
```
Metric: ΔE00/ΔEOK, hue-angle error, chroma/L drift, clipping/gamut fraction, banding (variance bước kề), halo/ringing, acutance (Laplacian), preview↔settle↔export delta, render time, RAM/VRAM.

Công cụ đã có (CPU-only, chạy được máy này — dùng cho P0):
- `tests/raw_look_probe.rs` (`IAI_RAW_LOOK_FILE`, `IAI_RAW_LOOK_OUT`).
- `tests/raw_corpus_probe.rs` (`IAI_RAW_CORPUS="C:/Users/Admin/Pictures/anh-raw"`).
- Python PIL+numpy đo sat/hue/sharpness (đã dùng phiên trước).
> **Thiếu ground-truth đo được** (ColorChecker/known-illuminant) — P0 phải bổ sung, nếu không sẽ lại tune bằng mắt và đoán sai lần 3.

---

## RISKS & MITIGATION

| Risk | P/I | Mitigation |
|---|---|---|
| Đổi look mở RAW làm project cũ đổi màu ngầm | H/H | `develop_engine_version`, exact legacy renderer, reopen matrix, upgrade tường minh |
| Tune default-look bằng mắt (đã sai 2 lần) | H/H | **P0 bắt buộc đo**, ColorChecker, blind review, numeric gate, versioned recipe |
| Bỏ proxy sai cách → lag trở lại | M/H | Giữ khung cache; một-model 2-resolution; đo p95 trước/sau; cusp-LUT trước khi bỏ proxy |
| Divergence không hết | M/H | Cùng kernels/order, chỉ khác resolution; conformance CPU/GPU |
| DCP/ICC sai (clean-room) | M/H | Từ spec DNG/ICC + reference CMS test; fallback matrix có flag |
| Camera coverage RAW hẹp | H/M | rawloader+rawler; fallback; corpus staged; báo rõ máy chưa verify |
| Gamut wide → banding f16 | M/H | promotion f32 node quan trọng; test gradient; LUT ≥4096 |
| CPU/WGSL drift | H/H | schema chung + conformance vectors + device CI |
| Cusp-LUT sai số vs binary | M/M | benchmark `gamut_map.rs:200`; error-bound gate |
| ART contamination/license | L/H | clean-room: spec/reviewer tách người viết; provenance log; KHÔNG import resource/constants ART |
| VRAM/RAM 45MP | H/H | tiled residency, eviction budget, streaming export |

---

## PRIORITIES

```text
P0 (bắt buộc, làm trước mọi thứ):
  - Instrumentation + reference đo được (ColorChecker/known-illuminant)   [Phase 0]
  - Bằng chứng định lượng "xấu" trước khi sửa recipe
P0 (trị gốc "ảnh xấu"):
  - Camera characterization thật (DCP/ICC), default-look OFF-JPEG          [Phase 1]
  - Khử divergence proxy↔exact ("nhảy sáng")                              [Phase 6 core, kéo sớm]
P1:
  - Working-space wide cho toán màu                                        [Phase 2]
  - Default render iAi Natural v1 + Contrast/tone làm lại                  [Phase 3]
  - Temperature Planckian + Kelvin tuyệt đối                              [Phase 4]
P2:
  - Color Mixer perceptual wide + gamut 1-lần                             [Phase 5]
  - Softness/capture-sharpen + scopes + soft-proof                       [Phase 7]
P3:
  - Cleanup twin (schema chung), gỡ JPEG-match/legacy sau golden          [Phase 6/8]
  - Điều tra NEF decode-đen; ACEScg/JzCzHz/HDR benchmark
```

---

## DECISION PRINCIPLES ÁP DỤNG

Thứ tự: Correctness → Image quality → Realtime → Architecture → Maintainability → Memory → Complexity. **Không hy sinh realtime vô lý**: mọi thay đổi màu phải đi kèm chứng minh không tăng p95 latency quá ngưỡng, và một-model 2-resolution bảo đảm preview=commit.

---

## RECOMMENDED IMPLEMENTATION ORDER

```text
1. PHASE 0 — Baseline verification + instrumentation + reference đo được.
             (Không đổi output. Định lượng "xấu". Chốt ngưỡng ΔE/latency theo máy.)
2. PHASE 1 — Camera characterization thật (DCP/ICC) + default-look OFF-JPEG (fallback matrix có flag).
3. PHASE 3 — Default render "iAi Natural v1" + Contrast/tone làm lại (bỏ auto-tone đoán).
4. PHASE 2 — Working-space thật rộng cho toán màu (OKLab/gamut wide, bỏ ép sRGB bước 1).
5. PHASE 6 — Realtime unification (một-model 2-resolution) + cusp-LUT gamut + schema CPU/WGSL.
6. PHASE 4 — Temperature Planckian + Kelvin tuyệt đối.
7. PHASE 5 — Color Mixer perceptual wide + gamut 1-lần.
8. PHASE 7 — Detail/softness + scopes + soft-proof/gamut warning.
9. PHASE 8 — Validation + golden + migration/default + cleanup twin & JPEG-match.

Ghi chú thứ tự: đưa PHASE 3 lên trước PHASE 2 vì default-render + Contrast trị "phẳng/mờ đục"
midtone mà user cảm nhận rõ nhất, và chạy được ngay trên working sRGB hiện tại;
PHASE 2 mở wide-gamut ngay sau để "kéo mạnh vẫn đẹp". PHASE 6 kéo sớm vì "nhảy sáng" là
lỗi mất-niềm-tin, độc lập tương đối với màu.
```

DO NOT IMPLEMENT YET.
Waiting for owner approval.

Chỉ khi owner nói rõ: `DUYỆT KẾ HOẠCH — BẮT ĐẦU CODE` thì phiên tiếp theo mới được triển khai.
