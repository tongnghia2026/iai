# Giao ca — Quality Milestone Q6 (detail / denoise / sharpen) — 2026-08-26

Repo `Documents/IAI`, nhánh `feat/vector-core-foundation`. Commit LOCAL
`7123623` (CHƯA PUSH). Kế hoạch: `KE_HOACH_GIAM_RAM_MO_NHIEU_RAW_2026-08-25.md`
§"Quality Milestone Q6". Nối tiếp Q5 (`560408b`, owner GUI-OK).

## Bối cảnh: engine Detail vốn đã khá đầy đủ

Audit `src/core/develop/detail.rs`: đã có phân lớp 3 tầng sharpen (capture ở
`raw.rs` — Q2/Q4; creative/detail ở đây; output-resize GÁC), à-trous 3 mức
edge-aware, **Luma NR** garrote tone-adaptive (dọn shadow mạnh hơn, giữ cạnh),
**Chroma NR** à-trous shrinkage tone-adaptive, defringe green↔magenta, và
Effects (clarity/texture/dehaze/vignette). **Detail KHÔNG có bản GPU** (preview
qua CPU refine bake — `compositor.wgsl:1903`) → sửa CPU là đủ, KHÔNG lo parity.

## Đã làm: bộ guard Q6 + sửa 1 defect

Thêm module test HERMETIC `q6_detail_contract` trong `detail.rs` (5 test, chạy
`cargo test` thường) khóa các bất biến Q6 và ĐO chỗ yếu:
- Sharpening: cạnh không mềm đi, có overshoot có kiểm soát (đang halo-safe kiểu
  Q2/Q4, rất nhẹ trên cạnh sạch), không ám màu neutral.
- Masking: giảm sharpen ở vùng phẳng nhiễu (variance masked < 0.7× no-mask).
- Colour NR: dọn chroma-speckle (variance −96%) mà không dịch luma.
- Mọi slider detail giữ neutral trung tính + finite.

**🔴 Defect đo được:** Chroma NR chạy à-trous **HOÀN TOÀN non-edge-aware** → ở
mức Colour NR cao, **cạnh màu thật bị bôi nhòe ~14px** — biên magenta↔green chỉ
giữ **68% bước chroma tại ±3px** ("bệt màu thật", đúng Q6 §4).

**✅ Fix (scale-aware + edge-aware, CPU-only):** `atrous_decompose` giờ nhận
`edge_aware_from: usize` (mức đầu tiên smooth edge-aware). Chroma NR dùng
`CHROMA_NR_EDGE_AWARE_FROM = 1`:
- **Mức 0** (mịn nhất, nơi speckle đơn lẻ) GIỮ non-edge-aware → speck mạnh vẫn
  bị bắt vào detail và dọn sạch kể cả sát cạnh;
- **Mức 1+** smooth EDGE-AWARE → biên màu thật nằm trong residual bảo-toàn-cạnh,
  không bị bôi qua.
Luma NR KHÔNG đổi (`0` = full edge-aware = `true` cũ, byte-identical).

**Kết quả:** cạnh màu thật giữ **68% → 82%** bước chroma tại ±3px; dọn speckle
Y HỆT (chroma variance −96%); luma không đổi. **Mặc định Colour NR = 0 → ảnh mở
mới byte-identical;** chỉ ảnh bật Colour NR mới đổi (giống Q5 saturation).

Test `colour_nr_real_edge_bleed_is_measured` siết bound `kept > 0.78` (baseline
~82%) chống regression về kiểu bôi nhòe cũ. 1455 lib pass, fmt sạch.

## ➡️ NEXT (owner GUI-test rồi tiếp)

- Owner GUI-test `dist/iAi-portable/iai.exe`: mở ảnh có nhiễu màu + cạnh màu rõ,
  kéo **Giảm nhiễu màu (Color Noise Reduction)** lên cao — kỳ vọng: hết đốm màu
  mà biên màu (vd áo đỏ trên nền xanh) KHÔNG bị lem/nhòe như trước.
- Tùy chọn tinh chỉnh thêm (chưa làm, không bắt buộc): range-sigma riêng cho
  chroma (hiện dùng chung `WAVELET_RANGE_SIGMA=0.12` của luma) để đẩy edge-kept
  82% → ~90%+; và Q6 §1 lớp **Output sharpen theo kích thước export** (đang GÁC).
- Q6 phần còn lại theo plan: capture-sharpen theo ISO metadata (Q1 đã audit:
  decoder KHÔNG lộ ISO → khó), deconvolution (optional). Sau đó Q7 (display/
  export color parity) / M3–M6.

## Lệnh

```bash
cargo test --lib q6_detail_contract -- --nocapture   # xem đo lường + guard Q6
```
