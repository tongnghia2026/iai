# Giao ca — Quality Milestone Q5, slice-1 (hợp đồng slider + guard) — 2026-08-26

Repo `Documents/IAI`, nhánh `feat/vector-core-foundation`. Commit LOCAL
`998fc32` (CHƯA PUSH, theo quy tắc chỉ commit cục bộ). Kế hoạch gốc:
`docs/planning/KE_HOACH_GIAM_RAM_MO_NHIEU_RAW_2026-08-25.md` §"Quality
Milestone Q5" (Công việc #1 + #9).

## Đã làm (an toàn, KHÔNG đổi gì render — byte-identical)

Thêm **`tests/develop_q5_slider_contract_probe.rs`** — bộ test HERMETIC (chạy
`cargo test` thường, không cần RAW corpus, KHÔNG sửa `src`, KHÔNG đổi default
look). Cùng kiểu với Q0 slider-sweep (`6a68c27`) và Q1 audit (`9b8f05b`): viết
**hợp đồng cho từng slider** dưới dạng test chạy được, khóa hành vi tốt hiện tại
làm golden TRƯỚC khi tinh chỉnh Q5. 12 test, tất cả PASS. `cargo fmt` sạch.

Hợp đồng đã khóa (đọc doc-comment đầu file để có bản đầy đủ):

- **Exposure** = nhân `2^EV` thuần (KHÔNG phải brightness gamma) — chứng minh
  bằng đẳng thức bit-identical: kéo Exposure +E EV ≡ nhân pixel sẵn cho `2^E`
  rồi Exposure 0. `±EXPOSURE_LIMIT` → `±5 EV`. Mid-grey đơn điệu, phủ hết dải.
- **Tone-equalizer** (Highlights/Shadows/Whites/Blacks) = các **vùng EV tách
  biệt**, không bóp per-channel: mỗi control chỉ sáng/tối vùng của nó (tâm EV so
  với xám: shadows −3, blacks −4.6, highlights +2.5, whites +4.5), rò sang vùng
  đối diện < 30%, đơn điệu theo giá trị control, neutral = 0 offset.
- **Nâng Shadows/Blacks dương có noise-confidence gate** (giảm dần dưới sàn cảm
  biến ~−9 EV, hết hẳn ~−11 EV) còn "đè sâu" (âm) KHÔNG bị gate.
- **Contrast** pivot ở xám 18%, không ám màu neutral. KHÔNG slider nào ám neutral
  (max chroma xám = 0.00000).
- **Vibrance** ưu tiên màu nhạt/xỉn, chừa màu đã rực (Δchroma muted 0.036 vs
  vivid 0.000).
- Toàn envelope: output finite + trong gamut ở cả 2 evaluator.

## 🔴 PHÁT HIỆN Q5 (defect thật, đã đo chính xác) — cần OWNER quyết cho sửa

Q0 từng dự đoán "saturation ±100% vỡ gamut → cần gamut compression". Nay tái hiện
& đo được trên **đường RAW thật** (`eval_scene_pixel_for_scene`, có OKLCh gamut
compression ở boundary): **kéo Saturation toàn cục quá ~+50% trên màu gần
nguyên (đỏ/lam/lục) đẩy màu ra xa hull sRGB, rồi 1 lần nén-gamut ở boundary GẬP
NGƯỢC lại** → màu vừa **xỉn đi** vừa **đổi tông**:

```
saturation sweep (chroma OKLab tại 0/25/50/75/100%, trên đường RAW thật)
  red      [0.195, 0.239, 0.199, 0.151, 0.112]  peak +50% rồi rớt 53%  | hue-swing 58°  ← đỏ hoá cam-vàng
  blue     [0.203, 0.247, 0.285, 0.232, 0.180]  rớt 37%                | hue-swing 13°
  green    [0.219, 0.240, 0.218, 0.190, 0.191]  rớt 20%                | hue-swing 13°
  sky      [0.107, 0.139, 0.170, 0.194, 0.173]  rớt 11%                | hue-swing 12°
  magenta/yellow/cyan/skin: ổn (rớt < 4%)
```

Nghĩa là: kéo Saturation lên +100%, một màu **đỏ bão hoà biến thành cam-vàng
XỈN HƠN cả khi để yên**. Đây là lỗi hành vi slider ở dải cao, KHÔNG phải look mặc
định (ở Saturation = 0 mọi thứ y nguyên — ảnh mới mở KHÔNG đổi 1 bit). Test
`saturation_near_hull_chroma_foldback_is_bounded` GHI LẠI toàn bộ sweep + chặn
regression (drop < 60%, hue-swing < 65°) — chặn để KHÔNG tệ đi thêm, KHÔNG phải
để hợp thức hoá lỗi.

Nguồn gốc: global Saturation hiện scale chroma **theo bán kính RGB tuyến tính**
quanh luma (`scale_linear_chroma_around_luma`, boundary_managed → `1 + req`,
tối đa ×2.5), không đi theo đường hằng-hue trong không gian tri giác. Đẩy mạnh →
vượt gamut nhiều → gamut-mapper phải nén cứng → gập ngược chroma + xoay hue.

## ✅ ĐÃ SỬA (slice-2, commit LOCAL `560408b`) — owner chỉ đạo "làm luôn, không cần công tắc"

Sửa THẲNG defect trên (owner: "kéo màu hiện tại ko đẹp"). Gốc: global Saturation
scale chroma theo **bán kính RGB tuyến tính** (`scale_linear_chroma_around_luma`,
`y+(c−y)*scale`, tới ×2.5) → đẩy mạnh vượt hull → `filmlike_clip`+gamut-map làm
hỏng hue/chroma.

**Fix:** nhánh **positive boundary_managed** (đường RAW) giờ scale chroma **dọc
đường hằng-hue+lightness OKLCh** (`color.chroma *= 1+req`, giữ L và hue). Boundary
OKLCh clamp (`working_to_display`→`map_to_output_gamut`) vốn giữ hue+lightness →
fit về **chroma tối đa trong gamut ĐÚNG hue** → màu bão hoà tới sát hull, **hue
KHOÁ, không xỉn dưới base**. Desaturation + đường raster/PTS knee GIỮ NGUYÊN (Q0
compat golden byte-identical).

- **CPU/GPU parity:** mirror `dev_scale_linear_chroma` trong `compositor.wgsl`
  bằng OKLab↔working converter sẵn có (twin đã dùng cho mixer V2). Test headless
  GPU/commit (có case saturation): **max 1/255, p99 1/255**. Raster Hue/Sat
  adjustment (`adj_scale_chroma_around_luma`) KHÔNG đụng.
- **Đo lại trên Q5 contract sweep:** hue-swing **58° → 0.3°**; dip xấu nhất
  **53% → 19%** (không dưới base). Màu nhạt (da/trời) lên mượt monotonic; màu
  gần-nguyên (đỏ/lam/lục/vàng) plateau đúng ở hull.
- **Look-versioning:** ảnh mở mới ở Saturation=0 vẫn byte-identical (color stage
  return sớm); chỉ project ĐÃ lưu có Saturation≠0 mới re-render khác (owner chấp
  nhận — hành vi cũ vốn là lỗi). Không thêm recipe-version vì owner bảo không cần
  công tắc.
- Contract test siết ENFORCE fix (hue<3° mọi mức, không xỉn dưới base; bound dip
  <25% / hue<3° chống regression). Unit test
  `boundary_managed_saturation_beats_the_srgb_hull_knee` sửa: assert hue giữ vs
  input thay vì "overshoots gamut". **1450 lib pass, fmt sạch.**

Build release `dist/iAi-portable/iai.exe` cho owner GUI-test (profile ART tự nạp
cho 16/20 ảnh). **CHỜ OWNER GUI-TEST.**

## ➡️ NEXT (sau khi owner GUI-test slice-2)

- Owner GUI-test bản `dist/iAi-portable/iai.exe`: kéo Saturation lên cao trên ảnh
  có màu đỏ/lam rực xem còn xỉn/đổi tông không (kỳ vọng: đậm dần rồi đứng ở hull,
  không hoá cam, không bạc). Preview khi kéo phải khớp lúc thả (đã đảm bảo parity).
- Tùy chọn siết thêm (không bắt buộc): thêm soft hull-limiter TRƯỚC boundary để
  dip 19% → ~0 (hiện 19% đã chấp nhận được vì đều trên near-primary đã rất đậm).
- Color Mixer per-band chroma đã chạy OKLCh sẵn (V2) — có thể soi lại cùng kiểu
  nếu owner thấy band-saturation cũng gập, nhưng có thể không cần.

Chưa làm trong Q5 (theo plan): Highlights/Whites negative "khai thác kênh còn dữ
liệu" (đã có `negative_highlights_recovers_coloured_highlight_chroma` trong unit
test, chưa gộp vào bộ contract); clarity/texture (Q6). Các Q6/Q7/M3–M6 vẫn còn.

## Lệnh

```bash
cargo test --test develop_q5_slider_contract_probe -- --nocapture   # xem sweep + hợp đồng
cargo test --test develop_slider_sweep_probe                        # Q0 golden (vẫn xanh)
```
