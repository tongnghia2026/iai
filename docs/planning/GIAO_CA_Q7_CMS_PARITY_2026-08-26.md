# Giao ca — Quality Milestone Q7 (display/export color management & parity) — 2026-08-26

Repo `Documents/IAI`, nhánh `feat/vector-core-foundation`. Commit LOCAL
`70287df` (CHƯA PUSH). Kế hoạch §"Quality Milestone Q7". Nối tiếp Q6 (`7123623`).

## Kết luận: đường màu display/export VỐN ĐÃ ĐÚNG (khác Q5/Q6 — KHÔNG cần fix)

Audit `src/core/cms.rs` (lcms2) + đường LUT hiển thị (`build_document_display_lut`
→ upload `apply_proof_settings` → sample trong blit shader `gpu/mod.rs:1923`).
Kiến trúc: transform màn hình (Display CMS) + soft-proof bake thành **LUT 3D 17³
RGBA8** (do lcms dựng), blit shader **encode linear→sRGB, sample trilinear (half-
texel đúng), decode sRGB→linear** — CHỈ ảnh hưởng preview; export đi đường lcms
riêng (`convert_srgb_to_rgb_profile`).

Thêm guard HERMETIC `tests/cms_display_parity_probe.rs` (5 test, ~0.04s, byte-
identical — chỉ test, KHÔNG sửa src, KHÔNG cần GUI-test). Tái hiện đúng cách blit
shader sample LUT bằng Rust rồi so với lcms trực tiếp:

- **Parity GPU-LUT vs CPU-lcms (§6):** max **3.67/255**, mean **0.35/255** trên
  cả AdobeRGB + DisplayP3 → LUT 17³ trilinear bám lcms tới mức mắt không thấy.
- **Không double/missing gamma (§7):** sRGB-trên-sRGB near-identity kể cả shadow
  sâu (0.03) — nếu thiếu/thừa gamma sẽ vỡ ngay.
- **Transform màn hình wide-gamut có thật:** dời green bão hoà 109/255; double-
  apply lệch 30/255 → guard bắt double-transform không "mù".
- **Export KHÔNG dính transform màn hình (§3):** xuất sRGB→sRGB byte no-op, dù
  CÙNG pixel đó lên màn hình wide-gamut đi qua LUT lớn → chứng minh monitor
  transform là preview-only, không lọt vào pixel xuất file.

Giá trị: đường màu đã đúng → deliverable Q7 = **guard chống hồi quy** (double
gamma, LUT thô đi, rò transform vào export, lệch parity) — đúng thứ §6/§7 yêu cầu.

## ✅ Gap Q7 per-window đã đóng — 2026-08-28

`system_display_profile_for_hwnd` resolve `MonitorFromWindow` →
`MONITORINFOEXW.szDevice` → `CreateDCW` → `GetICMProfileW`, nên không còn lấy
profile màn hình chính bằng virtual-screen DC. Main và Develop giữ hai texture
LUT riêng để có thể đồng thời nằm trên hai màn hình calibrate khác nhau.
`WindowEvent::Moved` refresh riêng từng cửa sổ, byte-identical profile là no-op;
chỉ chế độ **From System** tự đổi, còn **Load Profile...** giữ nguyên profile tay.
`cargo test --lib`: **1478 passed, 0 failed, 6 ignored**.
`cargo build --release --bin iai`: pass; portable SHA-256
`675226F9909C15E92B44D0A6F64A826552A3A844EEE2C92CE65DB9AFBA5E0DA0`.

## ➡️ NEXT (theo plan §7, phần chưa làm)

Memory M3 (batch tuần tự + spill) / M4 (tiled AHD demosaic + peak scratch) / M5
(viewport/crop preview dùng exact pipeline) / M6 (global budget manager). Lưu ý:
**RAM goal đã đạt qua M1** (owner GUI-OK, <3GB) → M-track ưu tiên THẤP. Q-track
(Q0–Q7) coi như xong phần chất lượng cốt lõi. Còn tùy chọn nhỏ: Q6 output-sharpen
theo export (GÁC), Q5 soft hull limiter.

## Lệnh

```bash
cargo test --test cms_display_parity_probe -- --nocapture   # xem số parity + guard
```
