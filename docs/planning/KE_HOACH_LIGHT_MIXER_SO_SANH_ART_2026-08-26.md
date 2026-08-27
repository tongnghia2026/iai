# Kế hoạch: so sánh nhóm Light + Color Mixer (iAi vs ART) và đề xuất cải thiện

2026-08-26. Owner "chưa ưng ý" 2 nhóm **Light** và **Color Mixer** → yêu cầu đọc
code ART, so sánh, lập kế hoạch để owner duyệt trước khi làm.

**Ràng buộc bản quyền:** ART = GPLv3. Chỉ ĐỌC hiểu THUẬT TOÁN/HÀNH VI (black-box),
**KHÔNG copy code, hằng số, curve, DCP, .arp**. Mọi đề xuất là tự thiết kế lại cho
iAi (MIT), lấy cảm hứng nguyên lý.

---

## 1. ART làm gì (đã đọc code)

### 1a. Light của ART = 3 công cụ TÁCH RỜI
- **Exposure/Tone curve** (`iptonecurve.cc`): đường tông "filmic" rolloff giữ
  18% xám + **Contrast là power pivot tại 18% xám** (đẩy độ dốc, giữ xám). Có
  nhiều CHẾ ĐỘ áp curve (Standard / Perceptual / Filmlike…): chế độ *Perceptual*
  giữ hue+sat khi kéo tông (chống lệch màu). Bão hoà theo tông chạy trong
  **Jzazbz (JzCzhz)** — không gian tri giác HỖ TRỢ HDR (đậm/sáng gắt chính xác
  hơn Oklab).
- **Tone Equalizer** (`iptoneequalizer.cc`, phỏng theo darktable): **12 dải
  Gaussian trong log2(luma)**, rộng 2 EV, cách nhau 2 EV, tâm −16…+6 EV; 5 thanh
  UI (Blacks/Shadows/Midtones/Highlights/Whites) map vào 12 dải. Mỗi pixel: đo
  luma → log → correction = tổng-có-trọng-số Gaussian × hệ số dải → **nhân đều
  R,G,B** (giữ hue/sat tuyệt đối). Điểm mấu chốt: **guided-filter trên log-luma
  (regularization)** để GIỮ TƯƠNG PHẢN CỤC BỘ khi kéo mạnh → không bị "phẳng/HDR
  giả/bệt". Có "pivot" exposure toàn cục.
- **Saturation/Vibrance** (`ipsaturation.cc`): saturation = `l + s·(rgb−l)` (bán
  kính RGB tuyến tính quanh luma — **GIỐNG iAi TRƯỚC Q5**); vibrance = power-curve
  trên từng kênh chroma (`|x|^vib`, ưu tiên màu nhạt). → **iAi Q5 (OKLCh hằng-hue)
  đã TỐT HƠN chỗ này rồi.**

### 1b. Color Mixer của ART = HSL Equalizer (`iphsl.cc`)
- Chạy trong **YUV**. 3 đường cong theo HUE: **hCurve (xoay hue), sCurve (bão
  hoà), lCurve (độ sáng)** — user vẽ curve liên tục theo hue (không phải 8 dải cố
  định).
- **Mấu chốt chất lượng — guided-filter (guide = luma) làm MƯỢT KHÔNG GIAN cho
  mặt nạ điều chỉnh, edge-aware, BÁN KÍNH KHÁC NHAU mỗi kênh: sat=4, lum=25,
  hue=4.** Nghĩa là: lượng chỉnh được "trải" theo vùng luma → 1 pixel lệch hue
  giữa vùng da vẫn nhận chỉnh GIỐNG hàng xóm (không lốm đốm), và chuyển tiếp giữa
  các hue mượt theo cạnh thật. Luminance trải RỘNG nhất (25) → sáng/tối theo vùng.
- Saturation được **định hình theo bão hoà hiện tại** (power phụ thuộc sat) → bảo
  vệ gần-neutral, đậm-nhạt phản ứng khác nhau. Lum = nhân Y (giữ hue/sat). Hue =
  cộng góc.

---

## 2. iAi đang làm gì

### 2a. Light (chủ yếu `core/develop_scene.rs`)
- Exposure = nhân `2^EV` thuần (đã chuẩn, Q5). Contrast = power pivot 18% xám
  (giống ART). Tone map = **sigmoid versioned** (Natural v3). Highlights/Shadows/
  Whites/Blacks = **4 dải Gaussian tone-eq** tâm cố định (shadows −3, blacks −4.6,
  highlights +2.5, whites +4.5 EV), nhân đều RGB. Có **regional-E proxy** (box-avg
  + guided low-pass trên mặt phẳng EV) để lift theo vùng.
- **So với ART:** (i) chỉ **4 dải RỘNG** vs 12 dải mịn → kém "phẫu thuật", và Q5
  đã đo **Highlights kéo full làm 18% xám nhảy 0.458→0.687** = ăn sâu vào midtone
  (dải quá rộng). (ii) Thiếu 1 thanh **Midtones** riêng. (iii) Giữ tương phản cục
  bộ có (regional-E) nhưng chưa có test đo → có thể bị phẳng khi kéo mạnh. (iv)
  Bão hoà theo tông chạy Oklab, không Jzazbz (kém chính xác vùng sáng/đậm gắt).

### 2b. Color Mixer (`core/develop/mixer.rs` + `color.rs`, có bản GPU WGSL)
- **8 dải cố định** (Reds…Magentas), nội suy raised-cosine (chỉ 2 dải kề chồng
  nhau, tổng=1). Chạy **OKLCh** (V2): mỗi pixel phân loại theo hue CỦA NÓ → lấy
  giá trị 3 curve (hue/sat/lum) → áp, có gate theo chroma-confidence + lum-guard.
  Giữ 2 thành phần OKLCh còn lại chính xác (đã test).
- **So với ART:** iAi phân loại + áp **THEO TỪNG PIXEL (point-op)**; coherence
  không gian CHỈ đến từ đường proxy riêng (COLOR_REGION_RADIUS=12 guided low-pass
  + EDGE_SUPPRESS/EDIT_SUPPRESS re-gate) — KHÁC cách ART (làm mượt trực tiếp mặt
  nạ chỉnh bằng guided-filter theo luma, bán kính riêng mỗi kênh). Hệ quả có thể:
  chỉnh mixer bị **lốm đốm/nhiễu trên da & vùng chuyển màu**, hoặc chuyển tiếp
  hue chưa đủ "trải theo vùng" như ART, nhất là kênh **Luminance** (ART trải rất
  rộng r=25, iAi 12). Bão hoà mixer scale tuyến tính chroma (chưa định hình theo
  sat hiện tại nhiều như ART).

---

## 3. Chẩn đoán "chưa ưng ý" (giả thuyết theo code — cần owner xác nhận triệu chứng)

| # | Triệu chứng có thể thấy | Gốc kỹ thuật | Nhóm |
|---|---|---|---|
| S1 | Kéo Highlights/Shadows làm **cả ảnh (midtone) đổi theo**, thiếu "phẫu thuật" | 4 dải Gaussian quá RỘNG; thiếu Midtones | Light |
| S2 | Kéo Shadows/Highlights mạnh làm ảnh **phẳng/bệt, mất tương phản cục bộ** | regularization giữ-tương-phản chưa đủ | Light |
| S3 | Vùng sáng gắt/màu đậm bị **lệch tông hoặc "cháy" thô** khi kéo tông/đậm | tone map + bão hoà theo tông ở Oklab, thiếu shoulder mềm/Jzazbz | Light |
| S4 | Chỉnh Color Mixer bị **lốm đốm/nhiễu**, nhất là da, vùng gradient | thiếu làm-mượt-không-gian mặt nạ (guided by luma) như ART | Mixer |
| S5 | Chuyển tiếp giữa các màu (hue) **gắt/không tự nhiên** khi chỉnh mixer | coherence không gian per-pixel, kênh Lum trải hẹp | Mixer |
| S6 | Bão hoà trong mixer làm **màu đã đậm bị gắt** hoặc màu nhạt lên yếu | chưa định hình đáp ứng theo sat hiện tại | Mixer |

---

## 4. Đề xuất (menu — owner chọn cái nào làm cái đó; mỗi cái = 1 slice có guard + GUI-test)

Nguyên tắc như Q5/Q6: **mặc định byte-identical tới khi owner GUI-test + bump
version**; đổi look phải versioned + golden A/B; ưu tiên ÍT rủi ro.

### Nhóm LIGHT
- **L1 — Tone-eq mịn + thêm Midtones (sửa S1).** Thu hẹp/định hình lại 4 dải để
  Highlights/Shadows bớt ăn midtone, và/hoặc thêm dải **Midtones** (như ART 5
  thanh). Cách iAi-riêng: giữ mô hình Gaussian-EV hiện có, chỉ chỉnh tâm/rộng +
  thêm 1 zone. Guard: mở rộng test Q5 `tone_zones_are_localized` (đo rò midtone
  giảm rõ). Rủi ro: TRUNG BÌNH (đổi đáp ứng Light) → version + GUI-test.
- **L2 — Giữ tương phản cục bộ khi kéo tông (sửa S2).** Thêm/чỉnh "detail
  preservation" kiểu guided-filter trên log-luma (ý tưởng ART/darktable, tự viết)
  để lift shadow/nén highlight KHÔNG làm phẳng. Guard: test đo tương phản cục bộ
  (std của high-pass) giữ ≥ ngưỡng sau khi lift. Rủi ro: TRUNG BÌNH.
- **L3 — Shoulder highlight mềm hơn + (tuỳ chọn) bão hoà-theo-tông sang Jzazbz
  (sửa S3).** Làm rolloff highlight mượt (giảm "cháy"), và cân nhắc chuyển bão
  hoà vùng sáng sang JzCzhz cho chính xác HDR. Rủi ro: CAO hơn (đụng tone map lõi
  + có thể đổi look Natural v3) → version mới + A/B kỹ.

### Nhóm COLOR MIXER
- **C1 — Làm mượt không-gian mặt nạ mixer theo luma (sửa S4/S5) [KHUYẾN NGHỊ #1].**
  Áp guided-filter (guide=luma, edge-aware) lên LƯỢNG chỉnh của mixer trước khi
  áp, bán kính riêng mỗi kênh (Lum rộng hơn Hue/Sat) — nguyên lý ART. iAi đã có
  hạ tầng guided-filter + proxy → tận dụng, giảm lốm đốm & làm chuyển tiếp tự
  nhiên. **Lưu ý:** mixer CÓ bản GPU (WGSL) → phải giữ parity (như Q5). Rủi ro:
  TRUNG BÌNH-CAO (đụng CPU+GPU). Guard: test đo "độ lốm đốm" (variance chỉnh
  trong vùng phẳng) giảm, cạnh màu thật vẫn giữ.
- **C2 — Định hình đáp ứng bão hoà mixer theo sat hiện tại (sửa S6).** Boost mạnh
  màu nhạt, nhẹ tay màu đã đậm (power theo chroma) — nguyên lý ART sCurve. Rủi ro:
  THẤP-TRUNG BÌNH. Guard: sweep chroma trước/sau.
- **C3 — (tuỳ chọn) rộng vùng chồng lấn dải / thêm dải.** 8 dải raised-cosine đã
  mượt; ưu tiên THẤP.

---

## 5. Khuyến nghị thứ tự (nếu owner để tôi chọn)
1. **C1** (mixer mượt theo luma) — nhiều khả năng là thứ owner thấy rõ nhất
   ("chỉnh màu bị lốm đốm/gắt").
2. **L1** (tone-eq mịn + Midtones) — Light "phẫu thuật" hơn.
3. **L2** (giữ tương phản cục bộ).
4. **C2** (định hình bão hoà mixer).
5. **L3 / C3** (tuỳ chọn, rủi ro cao / lợi ích nhỏ).

**Cần owner xác nhận:** triệu chứng nào ở §3 đúng với cái đang thấy (S1–S6), để
tôi chốt thứ tự và bắt tay. Chưa code gì tới khi owner duyệt.

---

## 6. ✅ CHỐT (owner xác nhận 2026-08-26) — GIAO CODE (chưa làm, owner hết quota)

Owner xác nhận **CẢ 6 triệu chứng đều đúng**: Light **S1 + S2 + S3**; Mixer
**S4 + S5 + S6**. Owner dặn: chỉ ghi nhận + chỉnh kế hoạch thành bản **giao code
làm sau** (hết quota phiên này). **KHÔNG code trong phiên này.** Dưới đây là
handoff đủ chi tiết để phiên sau / Codex thực thi.

### Nguyên tắc chung (bắt buộc, như Q5/Q6)
- Mỗi item = 1 slice riêng, commit cục bộ (đừng push tới khi owner bảo).
- **Mặc định BYTE-IDENTICAL** cho tới khi owner GUI-test: gate sau cờ opt-in
  (`IAI_*`) HOẶC bump `RawRenderRecipeVersion` (Natural v4…) + golden A/B; ảnh
  cũ giữ look qua recipe-version.
- **PARITY CPU/GPU:** Light VÀ Color Mixer đều có bản GPU (`gpu/compositor.wgsl`:
  `dev_scene_display`, `dev_scene_color`, LUT tone-eq/sigmoid, proxy region).
  MỌI thay đổi phải sửa cả 2 + test parity headless (`tests/develop_cpu_gpu_
  parity.rs`, max ≤2/255) để preview kéo == commit (chống "nhảy look").
- Guard trước, đo defect, sửa, đo cải thiện, siết bound (đúng quy trình Q5/Q6).
- Corpus `Pictures\anh-raw`; ART tham khảo BLACK-BOX ở `Pictures\1111\ART`
  (`rtengine/iptoneequalizer.cc`, `iphsl.cc`, `iptonecurve.cc`, `ipsaturation.cc`)
  — CẤM copy code/hằng/curve.

### Thứ tự làm (khuyến nghị)

**① C1 — Mixer: làm mượt không-gian mặt nạ theo luma (S4+S5). WIN rõ nhất.**
- File: `core/develop/mixer.rs` (chọn dải), `core/develop/color.rs` +
  `develop_scene.rs` (áp mixer), proxy màu `tone_lowpass_scene_region` +
  `COLOR_REGION_RADIUS`/`guided_lowpass`, GPU `dev_scene_color`.
- Việc: ĐO trước "độ lốm đốm" — khả năng gốc là **re-gate full-res per-pixel**
  (mod.rs REGATE_*/EDGE_SUPPRESS/EDIT_SUPPRESS) trong khi proxy đã mượt → chênh.
  Áp **guided-filter (guide = luma, edge-aware) lên CHÍNH LƯỢNG chỉnh** (3 mặt
  phẳng hue_delta/sat_delta/lum_delta) trước khi áp, **bán kính riêng mỗi kênh:
  Lum RỘNG nhất, Hue/Sat hẹp hơn** (nguyên lý ART iphsl: 25/4/4, iAi tự chọn số).
  Tận dụng `guided_lowpass` sẵn có. GPU: mixer preview chạy proxy → nếu proxy đã
  guided-smooth thì preview OK; đảm bảo full-res commit dùng CÙNG guided-smooth
  (đừng re-gate per-pixel thô). Nếu GPU khó blur → có thể để refine-bake CPU lo
  bản settled, nhưng PHẢI verify không "nhảy look".
- Guard: test kiểu Q6 — variance-chỉnh trong vùng hue-nhiễu phẳng GIẢM (hết lốm
  đốm), cạnh màu thật giữ bước; hue/lum khác không đổi.

**② L1 — Light: tone-eq mịn hơn + thêm Midtones (S1).**
- File: `develop_scene.rs` (`TONE_EQ_SHADOWS/BLACKS/HIGHLIGHTS/WHITES`,
  `tone_eq_offset_ev`, LUT `tone_eq`), có thể thêm zone Midtones. GPU: tone-eq là
  LUT baked → đổi hằng/tâm/rộng chỉ đổi LUT (GPU nhận qua upload) → có thể KHÔNG
  cần sửa WGSL (VERIFY: `dev_tone_delta`/LUT). UI: cân nhắc thêm 1 thanh Midtones
  (giữ option-bar gọn — [[feedback_iai_lean_optionbar]]); hoặc chỉ thu hẹp dải
  trước, không thêm slider.
- Việc: **thu hẹp width các Gaussian** (hiện 1.3–1.6 EV) và/hoặc dịch tâm để
  Highlights/Shadows bớt ăn midtone (Q5 đo 18% xám nhảy 0.458→0.687 khi
  Highlights full — mục tiêu giảm rõ). Tuỳ chọn thêm zone Midtones tâm 0 EV.
- Guard: mở rộng `tests/develop_q5_slider_contract_probe.rs`
  `tone_zones_are_localized_and_signed` — siết rò midtone (offset tại Er=0 khi
  Highlights full) xuống ngưỡng mới; thêm assert 18% xám dịch < X.
- Gate: bump recipe version + golden + GUI-test (đổi đáp ứng Light).

**③ L2 — Light: giữ tương phản cục bộ khi kéo tông (S2).**
- File: `develop_scene.rs` proxy regional-E (`build_scene_region_base`,
  `finish_region_e`, `TONE_GUIDED_EPS`, áp gain trong `scene_to_working`).
- Việc: đảm bảo tone-eq gain áp trên **regional exposure mượt** (giữ chi tiết
  pixel) — đây đã là thiết kế; nếu vẫn phẳng, tách **local deviation** (pixel −
  regional base) và bảo toàn qua lift (ý guided-filter regularization của ART/
  darktable, TỰ VIẾT). Cân nhắc giảm `TONE_GUIDED_EPS` có kiểm soát để giữ mid-
  frequency (coi chừng "loang" — xem note eps ở mod.rs).
- Guard: test — lift Shadows mạnh trên patch có texture, assert std high-pass
  (tương phản cục bộ) giữ ≥ ngưỡng (không phẳng).

**④ C2 — Mixer: định hình đáp ứng bão hoà theo sat hiện tại (S6).**
- File: `color.rs` (nhánh mixer sat, `chroma_scale`), `mixer.rs`, GPU
  `dev_scene_color`.
- Việc: thay scale chroma tuyến tính bằng đáp ứng **power theo chroma hiện tại**
  (mạnh tay màu nhạt, nhẹ màu đã đậm), giữ bảo vệ neutral (đã có chroma gate).
  Nguyên lý ART sCurve (coeff theo sat) — tự chọn hàm.
- Guard: sweep chroma từng dải trước/sau; parity GPU.

**⑤ L3 — Light: shoulder highlight mềm + (tuỳ chọn) bão hoà-theo-tông Jzazbz (S3). RỦI RO CAO — LÀM CUỐI.**
- File: `develop_scene.rs` (`sigmoid`, `compress_highlight_chroma`,
  `LookRecipe`), GPU sigmoid LUT.
- Việc: rolloff highlight mượt hơn (giảm "cháy" thô) trong sigmoid/shoulder.
  TUỲ CHỌN lớn: thêm không gian **Jzazbz (JzCzhz)** cho bão hoà/độ đậm vùng sáng
  gắt (ART chính xác HDR hơn Oklab) — đây là việc LỚN (implement color space
  mới), cân nhắc kỹ, có thể bỏ nếu shoulder mềm là đủ.
- Gate: **engine version MỚI** + A/B kỹ (đụng look Natural v3 lõi mà owner đã
  duyệt) → phải cho owner so sánh trước-sau.

### Ghi chú
- Saturation TOÀN CỤC (không phải mixer) KHÔNG đụng: Q5 OKLCh đã tốt hơn ART.
- Nếu thiếu thời gian: làm ①②③④ trước; ⑤ (nhất là Jzazbz) để sau cùng/optional.

---

## 7. Trạng thái thực thi Codex (2026-08-26)

Đã triển khai recipe opt-in **Develop3** cho các mục C1, L1, L2, C2 và toàn bộ
L3 (shoulder + JzCzhz). Bật GUI-test bằng biến môi trường
`IAI_LIGHT_MIXER_V3=1` trước khi mở phiên Develop mới. Ảnh/project cũ vẫn giữ
engine đã serialize; mặc định không có biến môi trường vẫn là Develop2.

- **C1:** ba plane `hue/sat/lum` được guided bằng luma; Hue/Sat bán kính ngắn,
  Lum bán kính rộng. GPU nhận chính ba plane này và áp trên pixel working-space,
  không bake RGB proxy rồi re-gate thô.
- **L1:** thêm slider **Midtones** cho Develop3; H/S/W/B dùng Gaussian hẹp hơn,
  giảm rò Highlights vào 18% grey.
- **L2:** regional-E Develop3 giảm regularization từ 0.25 xuống 0.10 EV² để giữ
  cạnh/mid-frequency tốt hơn; linear gain theo regional-E tiếp tục giữ texture.
- **C2:** boost saturation mạnh hơn ở chroma thấp, giảm dần ở màu đã đậm; CPU và
  WGSL dùng cùng hàm.
- **L3:** thêm Natural/Filmic/Neutral v2 với shoulder nhẹ hơn. Highlight-chroma
  rolloff của Develop3 chuyển sang **JzCzhz**: giảm `Cz` nhưng giữ `Jz` và hue,
  thay cho nội suy RGB về xám; Develop2 giữ nguyên nhánh cũ byte-identical. CPU
  và WGSL cùng dùng transform Jzazbz hai chiều, quy ước display-linear 1.0 =
  100 cd/m².
- **Biên màu Develop3:** giữ cùng wide working space và white-balance hiện đại
  như Develop2; không rơi về Linear-sRGB/WB legacy.
- Guard mới đo variance mask, giữ cạnh luma, rò midtone, đáp ứng saturation theo
  chroma, round-trip/hue/lightness Jzazbz và parity Develop3 GPU/commit.
- Kết quả kiểm thử sau L3: **1462 passed, 0 failed, 6 ignored**; bộ parity
  headless **3/3 passed**, sai lệch preview/commit Develop3 tối đa **1/255**
  (giới hạn kế hoạch ≤2/255).
