# Giao ca — Quality Q0: baseline corpus + audit profile (đã chạy thật)

**Ngày:** 2026-08-26
**Repo/nhánh:** `C:\Users\Admin\Documents\IAI` — `feat/vector-core-foundation`
**Kế hoạch gốc:** `docs/planning/KE_HOACH_GIAM_RAM_MO_NHIEU_RAW_2026-08-25.md` (mục Q0)
**Giao ca trước:** `docs/planning/GIAO_CA_CODEX_RAW_M1_FIX_2026-08-25.md` (M0/M1 owner đã GUI-test OK)
**Corpus:** `C:\Users\Admin\Pictures\anh-raw` (20 RAW + xmp/arp)

> Tiếp nối đúng handoff: M1 preview-first (`0d7f46d`) KHÔNG đụng tới. Bắt đầu track chất lượng ở **Q0 — dựng baseline nhìn thấy được trước khi tune look**. Chưa đổi một hằng số look nào.

## 1. Đã làm

Thêm **1 harness mới** (không sửa `src/`, nên không thể làm hồi quy behavior):
`tests/raw_q0_baseline_probe.rs` — `#[ignore]`, gated `IAI_RAW_CORPUS`, hermetic như các probe anh em. Với mỗi RAW trong corpus:
1. **Audit provenance profile:** đọc `SceneSource.camera_profile.resolution.selected` → profile được chọn là DCP / scene-ICC / decoder-matrix fallback (kèm lý do), và `jpeg_match` mode.
2. **Render neutral default look** headless (`render_default_look`) + đo no-reference: OKLab L, chroma theo band (shadow/mid/high), clip %, acutance (Laplacian), và cast trắng (bright-mean g/r, g/b).
3. Ghi **PNG contact-sheet mỗi ảnh** (chuẩn hóa bề rộng 1400) để nhìn A/B trên màn hình.
4. **Sẵn hook ăn ART reference** (`IAI_ART_REFERENCE_DIR` → `<stem>.tif|tiff|png`) tính |Δ| kênh trung bình khi có; ART chỉ là black-box oracle, KHÔNG copy gì.

Xuất artefact máy-đọc-được: `target/q0/q0_corpus_summary.csv`, `target/q0/q0_corpus_provenance.json`, `target/q0/contact/*.png`.

**Lệnh chạy lại:**
```powershell
$env:IAI_RAW_CORPUS='C:\Users\Admin\Pictures\anh-raw'
$env:IAI_Q0_OUT='C:\Users\Admin\Documents\IAI\target\q0'
cargo test --release --test raw_q0_baseline_probe -- --ignored --nocapture
```
Kết quả chạy 2026-08-26: **20/20 RAW decode OK, 0 lỗi**, 183 s. Các render nhìn bằng mắt đều đúng ảnh (neon, đêm sao, chân dung…), metric phản ánh đúng cảnh — harness đáng tin.

## 2. Phát hiện đầu — GỐC của "sai màu / nhợt"

**0/20 ảnh có camera profile. CẢ 20 rơi xuống decoder-matrix fallback, lý do `NoProfileCandidates`; và `jpeg_match=Full` cho cả 20.**

Nghĩa là hiện tại toàn bộ màu RAW của corpus KHÔNG đến từ characterization camera (DCP/ICC) mà đến từ **khớp embedded-JPEG** (baseline brightness + ma trận 3×3 + tone curve fit theo JPEG máy) — đúng khoảng trống kế hoạch nêu ở §4.2 và rủi ro Q4. Đây là mắt xích vật lý phía sau "màu kém chân thật/nhợt/gắt": repo chưa phân phối `camera_profiles`, nên resolver không có ứng viên nào để chọn.

Camera trong corpus (đều CHƯA có profile): Nikon Z6/Z6II/D810/D750, Sony A7R3/A7R2, Canon 6D/5D2/5D3/5D4/EOS R/700D, Fujifilm GFX 50S II. → Đây chính là **danh sách coverage Đợt 1 của Q3** (mọi camera xuất hiện trong corpus).

## 3. Bảng điểm nổi bật (đọc kèm `q0_corpus_summary.csv`)

- **Clip cao (highlight vỡ / hi-chroma):** Ngoc Long HP0449 (5D3, neon) clip **28.6%**, Chi 0.116, g/r 1.245; bé linh trang HP0018 (D810) **17.3%**; `_KKK5695` (GFX 50S II) **11.0%**; giáng sinh HP-KKK8130 (D810) 7.8% kèm cast mạnh g/r 0.749 / g/b 1.572.
- **Cast trắng lệch (nghi WB/profile):** giáng sinh (g/r 0.749, g/b 1.572), ert HP0217 (g/r 0.786), beauty hương liên (g/r 0.834) — lệch xa 1.0.
- **Ảnh low-key thật (không phải bug):** DSC00080 (A7R2) L 0.177 = ảnh Ngân Hà đêm; IMG_1473 (5D2) highlight-chroma 0 = ảnh phẳng không có vùng sáng.
- **Aggregate:** mean L 0.556, C 0.053, clip **4.59%**, acutance 0.00201, g/r 0.958. Mức clip trung bình cao gợi ý highlight handling là điểm cần soi ở Q4/Q5.

## 4. Slider-sweep baseline — ĐÃ LÀM (commit `6a68c27`)

Thêm `tests/develop_slider_sweep_probe.rs` — test **hermetic** (chạy trong `cargo test` thường, KHÔNG cần corpus, KHÔNG đụng src), làm golden/regression guard luôn. Đưa bộ patch tổng hợp (bậc thang xám + primaries bão hòa + skin/sky, linear-sRGB) qua `eval_scene_pixel(.., BaseLook::Raw)` và sweep từng slider ở **±100%/±50%/0 của range thật** (exposure ±50=±5EV; contrast/highlights/shadows/whites/blacks/saturation/vibrance ±200). Ghi `target/q0/q0_slider_sweep.csv` khi có `IAI_Q0_OUT`.

**Baseline đo được 2026-08-26 (đã khóa làm golden):**
- **Giữ neutral HOÀN HẢO:** max chroma trên MỌI ô xám qua MỌI slider = **0.0000** (neutrals đối xứng theo cấu trúc). Bound < 0.002.
- **Exposure đơn điệu + đủ dải:** gray_18 OKLab L = [0.072, 0.208, 0.569, 0.946, 0.998] từ −100%→+100% (đúng ±5 EV).
- **Tone giữ hue tốt:** hue drift xấu nhất = **10.3°** (exposure trên "sky"). Bound < 13°.
- **Phát hiện cho Q5:** saturation ở ±100% đẩy **8 patch-step vỡ gamut (clip)** → cần gamut compression ở saturation (đúng rủi ro plan §Q5). Highlights/shadows đổi mid-gray đơn điệu; whites/blacks tác động chủ yếu ở đầu/đuôi tông (đúng thiết kế).

→ Q0 giờ đã có baseline cho **neutral ✓ + default-look ✓ + slider ✓** (đủ ≥5 slider chính).

## 5. Khoảng trống Q0 còn lại (cho phiên sau)

1. **Chưa có nhánh ART reference** (ART ở `C:\Users\Admin\Pictures\1111\ART` chỉ có source, KHÔNG có .exe build sẵn). Muốn có cột A/B iAi–ART: owner/agent build ART cục bộ 1 lần, render neutral TIFF vào `target/q0/art/<stem>.tif`, rồi set `IAI_ART_REFERENCE_DIR` — harness corpus đã sẵn hook. Chưa build ART thì cột này bỏ trống, KHÔNG chặn.
2. **Corpus thiếu** ColorChecker (daylight/tungsten) và X-Trans thật (GFX 50S II là Bayer). ColorChecker ΔE có sẵn ở `color_reference_probe` (Middlebury) nhưng cần tải dataset ngoài.

## 6. Ràng buộc giữ nguyên

- CHỈ commit cục bộ, **KHÔNG push** tới khi owner bảo (Actions gần cạn).
- KHÔNG copy code/asset GPL từ ART — chỉ black-box.
- Bảo toàn các file planning untracked. Không sửa ngoài phạm vi. Q0 đã có baseline đầy đủ (neutral ✓, default-look ✓, slider ✓) — Q0 coi như đủ để chuyển sang **Q1** (sensor preprocessing + normalized master), trừ nhánh ART A/B là tùy chọn (cần build ART). Đổi default look phải tăng `raw_render_recipe`/engine version + golden A/B.
- Gate trước commit: `cargo fmt --check` + `cargo test --locked` xanh.
