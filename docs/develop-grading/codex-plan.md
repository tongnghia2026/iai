# Develop colour/light grading — Codex work plan (#1 → #10)

> Mục tiêu: kéo màu và kéo ánh sáng đẹp hơn, học kỹ thuật từ engine ART
> (RawTherapee fork). Mỗi mục dưới đây là **một ticket độc lập** cho Codex làm
> dần. Làm theo thứ tự số (đã xếp theo hiệu quả/công sức). Không cần làm hết một
> lần — mỗi ticket tự đứng được và commit riêng.

Chẩn đoán gốc (đã phân tích): iAi làm **nửa Ánh sáng đúng** (scene-linear:
`develop_scene.rs`) nhưng **chỉnh Màu quá muộn** — sau khi đã encode sang sRGB
gamma và kẹp cứng `[0,1]`, nên tăng màu là đụng tường (bệt/lệch tông/banding).
Đường cong tông (sigmoid) lại chạy **riêng từng kênh R/G/B** với hệ số giữ-tông
cố định 35%, nên **xoay tông ở vùng trung gian** (đỏ→cam, trời→cyan, da→vàng).
ART làm mọi thứ trên **một ảnh tuyến tính chưa kẹp** và **chỉ encode/kẹp một lần
ở cuối**, map tông theo độ sáng/tỉ lệ (giữ tông), chỉ vùng chói mới nhạt về trắng.

---

## Luật chung cho MỌI ticket (đọc trước khi bắt đầu)

1. **Twin CPU ↔ GPU bắt buộc khớp.** Chuỗi scene có 2 bản sao phải giống hệt nhau
   về toán:
   - CPU: `SceneToneData::scene_to_display` — `src/core/develop_scene.rs:636`
   - GPU: `dev_scene_display` — `src/gpu/compositor.wgsl:804`
   Stage Màu:
   - CPU: `apply_color` — `src/core/develop/color.rs:11`
   - GPU: phần recombine màu quanh `dev_scene_display` / proxy (`compositor.wgsl`
     ~838–1000, các hàm `dev_*` màu). Sửa CPU mà quên WGSL = **preview lệch commit**.
2. **Neutral phải là no-op tuyệt đối.** `DevelopSettings::is_neutral()` = ảnh không
   đổi một bit. Mọi thay đổi phải giữ: settings mặc định → output y hệt input
   (đặc biệt look `Identity` cho ảnh không-RAW). Có test cho việc này trong
   `src/core/develop/tests.rs` — đừng phá.
3. **Đừng đụng các phần đang tốt:** cân bằng trắng CAT16 (`src/core/cat16.rs`),
   xoay tông Oklab (`rotate_oklab_hue`), chọn dải UCS (`src/core/ucs.rs`).
   **KHÔNG port bảng Munsell của ART** — trong nhánh ART đó nó là code chết.
4. **Quy trình đóng mỗi ticket:**
   `cargo fmt` → `cargo test --locked` → build & mở app xem mắt thường
   → `git commit` (local). **KHÔNG push** (ngân sách GitHub Actions — theo quy
   ước dự án; user sẽ tự bảo khi nào push).
5. Test GPU chạy `--test-threads=1`. File .rs là UTF-8 — sửa bằng editor bình
   thường, đừng dùng lệnh shell ghi đè cả file.
6. Mỗi ticket nên kèm **1–2 test số học** trong `develop/tests.rs` chốt hành vi
   mới (ví dụ: "saturation không làm đổi luminance quá X").

Thứ tự & phụ thuộc gợi ý:
- Đợt A (nhanh, thấy đẹp ngay): **#1, #2, #3, #4, #5**. `#3` làm chung với `#2`.
- Đợt B (khả năng mới): **#6**.
- Đợt C (ánh sáng): **#7, #8, #9**.
- Đợt D (refactor gốc): **#10** — làm cuối; nó *thay thế* phần lớn #2–#5 nên nếu
  làm #10 thì #2–#5 coi như được gộp vào.

---

## #1 — Sigmoid: hệ số giữ-tông biến thiên theo tông (thay cho 35% cố định)

**Lợi ích:** hết xoay tông vùng trung gian — da/trời/lá không lệch màu; chỉ điểm
chói mới nhạt về trắng tự nhiên. Đây là **thắng lợi lớn nhất, code ít nhất**.

**Hiện tại:** `SIGMOID_HUE_PRESERVE = 0.35` (const, `develop_scene.rs:60`) pha cố
định giữa đường per-channel `pc` và đường tỉ lệ max-RGB (`develop_scene.rs:649‑660`).
GPU dùng cùng số qua `dev_effects[25]` (`compositor.wgsl:818`).

**Việc:**
1. Thêm 2 taste-knob const trong `develop_scene.rs`:
   `HUE_PRESERVE_LOW = 0.90` (giữ tông mạnh ở tối/giữa),
   `HUE_PRESERVE_HIGH = 0.20` (buông ở gần trắng).
2. Trong `scene_to_display`, sau khi có `n = max(v)` và `s = tone_map(n)/n`,
   tính độ sáng hiển thị của pixel `hw = smootherstep(0.5, 1.0, tone_map(n))`
   rồi `blend = HUE_PRESERVE_LOW + (HUE_PRESERVE_HIGH - HUE_PRESERVE_LOW) * hw`.
   Dùng `blend` thay cho `SIGMOID_HUE_PRESERVE` ở 3 dòng pha.
3. Mirror y hệt trong `dev_scene_display` (WGSL): tính `hw` từ
   `dev_scene_lut(n)` và blend tại chỗ, **bỏ phụ thuộc** `dev_effects[25]` (hoặc
   giữ [25] làm `HUE_PRESERVE_LOW` truyền vào, thêm [27]=`HUE_PRESERVE_HIGH` — cách
   nào cũng được, miễn 2 twin khớp). Cách gọn nhất: hardcode 2 const ở cả 2 twin.

**Tham chiếu ART:** `iptonecurve.cc` — chế độ `SatAndValueBlending` giảm bù màu
S→0 chỉ khi tiến gần trắng.

**Acceptance:** ảnh RAW mặc định, kéo màu bão hòa (trời xanh/áo đỏ): tông giữa
không đổi hue; chỉ vùng gần cháy mới nhạt về trắng. Preview GPU == commit CPU.
Neutral vẫn no-op.

**Effort:** Nhỏ • **Risk:** Thấp • **Depends:** none.

---

## #2 — Chỉnh cả stage Màu trong ánh sáng tuyến tính (linearize vào → encode ra)

**Lợi ích:** Saturation/Vibrance/Mixer màu **nở đều, tươi**, hết đục/tối đi. Đây
là **thay đổi màu quan trọng nhất** — đúng lý do ART đẹp hơn ở các slider màu.

**Hiện tại:** `apply_color` (`develop/color.rs:11‑61`) và các hàm con
(`apply_mixer_brightness`, `scale_chroma_around_luma`, `saturate_around_luma`)
chạy trên giá trị **gamma sRGB đã kẹp `[0,1]`**. Chỉ `rotate_oklab_hue` là linear
đúng.

**Việc:**
1. Đầu `apply_color`, đưa `(*r,*g,*b)` về linear:
   `let (rl,gl,bl) = (srgb_to_linear(*r), srgb_to_linear(*g), srgb_to_linear(*b));`
   (dùng helper sẵn có; nếu `srgb_to_linear_scalar`/`linear_to_srgb_scalar` ở
   `core/color.rs:337,346` đang private thì đổi thành `pub` hoặc dùng bản vec3
   đã export).
2. Chạy saturation + vibrance + mixer chroma/luminance **trên giá trị linear**.
   Dùng công thức của ART (giống iAi nhưng ở linear): `out = l + sat*(c - l)` với
   `l` = luminance **tuyến tính** (xem #3), vibrance = đường lũy thừa trên phần
   `(c - l)`.
3. Cuối hàm, encode lại: `*r = linear_to_srgb(rl); …`. Clamp `[0,1]` một lần ở cuối.
4. **Mirror WGSL:** phần màu trong `compositor.wgsl` (proxy `adjusted`/recombine)
   phải linearize→edit→encode cùng cách. Proxy `adjusted` do CPU dựng bằng chính
   `apply_color`, nên chỉ cần CPU đúng thì proxy đúng; nhưng đường GPU full-res
   (nếu có nhánh tính trực tiếp) phải khớp.
5. Retune: `SAT_POSITIVE_SCALE`, hệ số `0.88` của vibrance, `BRIGHT_KNEE`… có thể
   phải chỉnh lại vì đổi domain — canh sao cho ở mức slider vừa phải cảm giác gần
   như cũ, chỉ khác là hết đục/gắt ở mức cao.

**Tham chiếu ART:** `ipsaturation.cc:65‑79` (`l + sat*(c-l)` ở working-RGB tuyến
tính), `apply_vibrance` (`ipsaturation.cc:30‑39`, đường lũy thừa).

**Acceptance:** kéo Saturation/Vibrance mạnh: màu tươi lên đều, **không tối đi**,
không lệch hue, không banding sớm. Preview == commit. Neutral no-op.

**Effort:** Vừa • **Risk:** Vừa • **Depends:** none (nên làm cùng #3).

---

## #3 — Neo chroma theo luminance TUYẾN TÍNH thật (không phải Rec.709-trên-gamma)

**Lợi ích:** tăng màu **không còn làm ảnh tối/xỉn**; "giữ nguyên độ sáng" thành
thật. Ticket nhỏ, đi kèm #2.

**Hiện tại:** `luminance_f32` (`core/color.rs:264`) = `0.2126r+0.7152g+0.0722b`
áp lên **giá trị gamma**. `saturate_around_luma` (`core/color.rs:514`) neo quanh
luma sai này.

**Việc:** khi stage Màu đã ở linear (sau #2), tính anchor luminance bằng trọng số
Rec.709 (hoặc ma trận working-space) trên **RGB tuyến tính**, khớp
`Color::rgbLuminance(r,g,b,ws)` của ART. Giữ luma gamma **chỉ** cho các mask
bảo vệ shadow/highlight nếu cần.

**Tham chiếu ART:** `ipsaturation.cc:65` (`Color::rgbLuminance(..., ws)`).

**Acceptance:** saturate một ô màu xám-trung tính không đổi độ sáng đo được;
saturate ảnh low-key không bị tối thêm. Có test: `|luma_after - luma_before| < eps`
cho vài màu mẫu.

**Effort:** Nhỏ • **Risk:** Thấp • **Depends:** #2.

---

## #4 — Vùng sáng chói nhạt dần về trắng khi bị nén (filmlike_clip + apply_sat)

**Lợi ích:** trời/điểm chói **trôi mượt về trắng**, hết viền màu gắt/neon — một
phần lớn của "vẻ đắt tiền" kiểu film.

**Hiện tại:** ở biên gamut chỉ có `gamut_clip_chroma` (`develop_scene.rs:587`) kéo
chroma về luma vừa đủ lọt cube — nhưng **không chủ động bleach** vùng đang bị nén.

**Việc:**
1. Thêm `filmlike_clip` kiểu ART trước/cùng `gamut_clip_chroma`: sort 3 kênh,
   clip kênh max về 1.0, nội suy kênh giữa theo tỉ lệ để **giữ hue** khi kênh
   sáng nhất chạm trần.
2. Thêm ý tưởng `apply_sat` của `iplogenc`: gate theo pixel đang bị tone-map kéo
   **xuống** (`tone_map(v) < v`, tức bị nén), rồi giảm chroma của nó về một sàn
   khi càng nén — để highlight bão hòa **fade về trắng đúng lúc roll-off**.
3. Mirror WGSL trong `dev_scene_display` / `dev_gamut_clip_chroma`.

**Tham chiếu ART:** `filmlike_clip` (trong `color.cc`/`iptonecurve.cc`),
`iplogenc.cc` hàm `apply_sat` (gate `f < 1`).

**Acceptance:** vùng cực sáng bão hòa (mặt trời, đèn màu) chuyển về trắng sạch,
không fringe màu. Preview == commit.

**Effort:** Nhỏ • **Risk:** Thấp • **Depends:** nên sau #1 (cùng khu tone map).

---

## #5 — Giữ / tăng nhẹ màu ở vùng tối sau khi map tông

**Lợi ích:** vùng tối **giữ được màu**, hết xám nâu đục — ART đẹp một phần nhờ
màu trong bóng đổ phong phú.

**Hiện tại:** toe per-channel làm nhạt màu vùng tối; không có bù chroma cho shadow.

**Việc:** theo `PerceptualToneCurve` của ART: nhân chroma tối đa ~1.2× theo một
S-curve trong vùng tối (cửa sổ luma ~0.15–0.50 của ART). Trong iAi: một phép nhân
chroma nhỏ theo **luminance sau tone-map**, chỉ khi luma thấp và chroma đủ ý nghĩa
— đặt trong `gamut_clip_chroma` hoặc `apply_luma_target` (`develop/tone.rs:129`).
Mirror WGSL.

**Tham chiếu ART:** `curves.cc` `PerceptualToneCurve` (hành vi shadow chroma).

**Acceptance:** ảnh có bóng đổ màu (áo, tường) giữ được sắc thay vì xám nâu; vùng
tối trung tính KHÔNG bị nhuốm màu (gate theo chroma). Preview == commit. Neutral no-op.

**Effort:** Nhỏ • **Risk:** Vừa • **Depends:** #1 (cùng khu).

---

## #6 — THÊM: split-tone / grade 3 vùng (ám nóng vùng sáng, ám lạnh vùng tối)

**Lợi ích:** mở khóa **"kéo màu điện ảnh"** — độ tương phản MÀU giữa các vùng
sáng/tối. iAi hiện **không làm được** trừ khi tự vẽ 3 đường cong R/G/B. Đây là
**khả năng mới đáng giá nhất** cho việc "kéo màu".

**Việc:**
1. Thêm control vào `DevelopSettings` (`develop/settings.rs:170`): tối thiểu
   `grade_shadow_hue/…_strength` và `grade_highlight_hue/…_strength` (thêm vào
   `#[serde(default)]`, `is_neutral`, `same_image_effect`). Giữ tương thích file cũ.
2. Áp trong **scene-linear, TRƯỚC sigmoid** (`develop_scene.rs`, sau tone-eq,
   trước `tone_map`): tái dùng trọng số gaussian theo vùng EV của tone-equalizer
   (`tone_eq_offset_ev`/các zone `TONE_EQ_*`) làm membership shadow/highlight; mỗi
   vùng thêm một offset RGB (hoặc a/b) nhỏ theo hue/strength người dùng.
3. Mirror WGSL `dev_scene_display`.
4. UI: 2 ô màu + slider cường độ trong panel Develop (đặt theo layout hiện có,
   trung tính — không chép nhãn Corel/Adobe).

**Tham chiếu ART:** `ipcolorcorrection.cc` (slope/offset/power theo vùng tông,
tách shadows/mids/highlights).

**Acceptance:** đặt highlight ấm + shadow lạnh → ảnh có color-contrast rõ, mượt,
không viền vùng. Neutral (strength 0) no-op. Preview == commit. File .iai cũ mở
được.

**Effort:** Vừa • **Risk:** Thấp • **Depends:** none (độc lập; hợp nếu sau #1).

---

## #7 — Đường cong tông tự thích ứng theo ảnh (auto black / white / grey EV)

**Lợi ích:** ảnh tối hết **đục/mờ sữa**, ảnh sáng hết **cháy gắt** — mọi ảnh có độ
nảy và điểm đen đúng. Cải thiện ánh sáng **luôn bật, diện rộng**.

**Hiện tại:** điểm trắng của sigmoid ghim cứng `SCENE_EV_MAX = +6` với trục cố
định (`develop_scene.rs:46‑47`). Ảnh mà highlight thật < +6 EV → phẳng/sữa; > +6 EV
→ cháy.

**Việc:** thêm dò kiểu `getAutoLog` của ART: subsample scene master (đã có
`build_scene_region_base`), lấy `vmin*0.5` / `vmax*1.5`, `DR = log2(vmax/vmin)`,
`grey` = mean trong cửa sổ thích ứng; map **black EV→0** và **white EV→1** của
chính ảnh **trước** sigmoid. Cung cấp như baseline tự động (giống
`baseline_exposure_gain` đã có ở `develop_scene.rs:389`), có thể override bằng slider.

**Tham chiếu ART:** `iplogenc.cc` `ImProcFunctions::getAutoLog` (đặt sourceGray/
blackEv/whiteEv/DR từ dữ liệu ảnh).

**Acceptance:** mở loạt ảnh tối/sáng khác nhau → tất cả có điểm đen thật và
highlight không cháy. Không đổi ảnh đã "đúng". Preview == commit.

**Effort:** Lớn • **Risk:** Vừa • **Depends:** none (nhưng tương tác với #1/#8).

---

## #8 — Tách Contrast khỏi vai highlight của sigmoid

**Lợi ích:** tăng Contrast **không làm cháy/bẹt vùng sáng** nữa; contrast chỉ đổi
độ dốc vùng giữa, như ART.

**Hiện tại:** Contrast scale luôn độ dốc sigmoid `c = 1.7 * 2^(0.7u)`
(`sigmoid_params`, `develop_scene.rs:349‑351`) → tăng contrast làm cứng luôn vai
highlight. (Lưu ý: có sẵn `contrast_curve` tanh ở `develop/tone.rs:191`, và
comment WGSL ghi `CONTRAST_K=2.4` trong khi CPU `tone.rs:179` là `3.0` — kiểm tra
lại cho khớp khi đụng vào.)

**Việc:** cố định `c` ở mức trung tính (1.7) cho vai highlight ổn định; chuyển
Contrast sang **một đường cong riêng, ghim tại pivot 0.1845** kiểu
`get_contrast_curve` của ART (power trong log-space quanh mid-grey, giải sao cho
pivot đứng yên), áp như luma-target **sau** tone map (dùng cơ chế
`apply_luma_target` sẵn có). Mirror WGSL.

**Tham chiếu ART:** `iptonecurve.cc:160‑225` (filmic rolloff) vs `:355‑374`
(`get_contrast_curve` ghim pivot) — hai stage **độc lập**.

**Acceptance:** tăng Contrast → vùng sáng giữ gradation (không thành trắng bệt),
đen không nghẹt; mid-grey 0.1845 đứng yên. Preview == commit. Neutral no-op.

**Effort:** Vừa • **Risk:** Vừa • **Depends:** nên sau #7 (cùng khu tone).

---

## #9 — Texture/Clarity có nền không gian thật (bỏ phép toán điểm)

**Lợi ích:** da/tóc/lá/vải **nét thật**, hết vẻ "nhựa" bẹt, không quầng sáng
(halo). Cải thiện phía ánh sáng/chi tiết.

**Hiện tại:** Texture là point-op soft-contrast không có high-pass thật
(`develop/detail.rs:406‑426`); Clarity một scale dễ halo (`develop/spatial.rs`).

**Việc:** theo `textureBoost` của ART: dựng low-pass bán kính nhỏ của **luminance**
(đã có `guided_lowpass_plane`), cộng `strength*(Y - lowpass)` **chỉ vào LUMA** qua
`apply_luma_target`, giữ nguyên chroma. Tùy chọn: Clarity hai-scale (fine = Y−mid,
coarse = mid−base) để "punch" mà không đụng chi tiết mịn. Đồng thời (từ phân tích
tone-eq) cân nhắc siết guided-filter vùng-E (`SCENE_TONE_GUIDED_EPS 0.25` →
~0.33 EV như ART) để bớt halo ở biên tương phản vừa.

**Tham chiếu ART:** `iptextureboost.cc` (fine-detail term), `iptoneequalizer.cc`
(guided regularization).

**Acceptance:** Texture/Clarity tăng độ nét vi mô không quầng, không dịch tông
tổng; biên backlit không halo. Preview == commit.

**Effort:** Vừa • **Risk:** Vừa • **Depends:** none.

---

## #10 — NORTH STAR: gộp Develop về một không gian tuyến tính dải rộng, encode 1 lần

**Lợi ích:** xóa hẳn "đường nối" scene-linear ↔ display-gamma và **cả lớp lỗi
clamp/gamma** cùng lúc — bản sửa gốc để màu và ánh sáng thành một khối render nhất
quán. Làm việc này thì #2–#5 (và một phần #8/#9) được **gộp vào**.

**Hiện tại:** `scene_to_display` **hạ cánh cứng** về display ở
`develop_scene.rs:661‑666` (`gamut_clip_chroma` → `linear_to_srgb` → `clamp`)
**trước khi** Màu/Effects/Detail/Locals chạy (`apply_scene_to_tilemap`
`develop_scene.rs:972‑988` → engine display-domain `pipeline.rs`).

**Việc (làm dần, có thể chia PR nhỏ):**
1. Cho nửa Light **emit buffer tuyến tính chưa kẹp** (hoặc ít nhất giữ +1 stop
   headroom, ví dụ Rec2020-linear như default của ART) thay vì sRGB đã clamp.
2. Chạy Màu (và lý tưởng cả Mixer, Effects) **trên buffer tuyến tính đó**.
3. `gamut_clip_chroma` + `linear_to_srgb` + `clamp` **chỉ MỘT lần ở cuối cùng**
   (giống `iprgb2out.cc:80‑82` của ART).
4. Cập nhật đồng bộ: CPU bake, twin WGSL, mixer, effects, detail.
5. **Giữ bất biến:** look `Identity`/PTS phải vẫn no-op bit-stable ở neutral (đây
   là ràng buộc khó nhất — hiện handoff display-domain đang bảo đảm nó).

**Tham chiếu ART:** `improcfun.cc:582‑645` (toàn bộ pass trên 1 master linear,
đổi mode RGB/Lab/XYZ nhưng không encode giữa chừng), `iprgb2out.cc` (encode cuối).

**Acceptance:** kéo màu mạnh không đụng tường sớm, highlight/màu tươi giữ
gradation, không banding; toàn bộ test hiện có xanh; neutral & Identity vẫn no-op
tuyệt đối; preview == commit trên mọi kích thước layer.

**Effort:** Lớn • **Risk:** Cao • **Depends:** nên làm **cuối cùng**; supersedes #2/#3/#4/#5.

---

### Ghi chú cho Codex
- File cần đụng nhiều nhất: `src/core/develop_scene.rs`, `src/core/develop/color.rs`,
  `src/core/develop/tone.rs`, `src/gpu/compositor.wgsl`, `src/core/color.rs`.
- Test: `src/core/develop/tests.rs` (unit), `tests/perf_develop.rs` (perf).
- Sau mỗi ticket: `cargo fmt && cargo test --locked` (GPU test `--test-threads=1`),
  build mở app xem mắt, rồi `git commit` local — **không push**.
- Nếu một ticket đổi taste (độ mạnh slider), báo lại để user xem mắt trước khi chốt.
