# Giao ca — Phase 1: Bộ so sánh A/B ART ↔ iAi

Ngày: 2026-08-27
Nhánh: `feat/vector-core-foundation`
Liên quan: [KE_HOACH_HOAN_THIEN_DEVELOP_IAI_2026-08-27.md](KE_HOACH_HOAN_THIEN_DEVELOP_IAI_2026-08-27.md) §6 (Phase 1),
[KE_HOACH_LIGHT_MIXER_SO_SANH_ART_2026-08-26.md](KE_HOACH_LIGHT_MIXER_SO_SANH_ART_2026-08-26.md)

## 1. Tóm tắt

Phase 0 đã **commit** (`fix(develop): expose engine diagnostics and activate midtones`).
Phase 1 đã dựng xong **harness so sánh A/B ART ↔ iAi có thể tái lập** và chạy thử
trên corpus. ART chỉ dùng làm **black-box oracle**; không sao chép code / hằng số /
LUT / profile / asset của ART vào iAi.

## 2. Phát hiện quan trọng (khác với giả định trong kế hoạch)

Kế hoạch (§0.3 mục 5–6) giả định "ART chưa có executable build sẵn" và dự phòng
export thủ công. Thực tế **đã có bản portable kèm CLI headless**:

- `C:\Users\Admin\Pictures\1111\ART_1.26.7_Win64_portable\ART-cli.exe`
- Chạy headless ~3.5 giây/ảnh, tự nạp DCP/ICC/DLL trong thư mục portable.

Nhờ vậy A/B được **tự động hoá hoàn toàn** (không cần owner export tay).

Ngoài ra corpus `Pictures\anh-raw` còn có sẵn sidecar `.arp` (recipe ART của owner)
cho 5 ảnh — là "reference look" thật, để dành cho vòng sau (`-s`).

## 3. Harness

File: [tests/raw_q1_art_ab.rs](../../tests/raw_q1_art_ab.rs) — integration test
`#[ignore]` (không phá tính hermetic của `cargo test`).

Lệnh chạy (PowerShell):

```powershell
$env:IAI_RAW_CORPUS='C:\Users\Admin\Pictures\anh-raw'
$env:IAI_ART_CLI='C:\Users\Admin\Pictures\1111\ART_1.26.7_Win64_portable'
$env:IAI_Q1_OUT='C:\Users\Admin\Documents\IAI\target\q1_ab'
# tuỳ chọn thu hẹp cho vòng review đầu:
$env:IAI_Q1_FILES='_DLL6009,_KKK5695,DSC02534,HUY_7933,KKK_0778,IMG_9961'
$env:IAI_Q1_WIDTH='1100'
# $env:IAI_Q1_RECIPES='neutral,exp_p1,...'   # lọc recipe nếu muốn
cargo test --release --test raw_q1_art_ab -- --ignored --nocapture
```

Biến môi trường:

| Biến | Ý nghĩa |
|---|---|
| `IAI_RAW_CORPUS` | thư mục RAW (bắt buộc) |
| `IAI_ART_CLI` | đường dẫn `ART-cli.exe` hoặc thư mục portable (bắt buộc) |
| `IAI_Q1_OUT` | thư mục kết quả (mặc định `target/q1_ab`) |
| `IAI_Q1_FILES` | lọc theo chuỗi con trong tên file (mặc định: tất cả) |
| `IAI_Q1_MAX_FILES` | giới hạn số file |
| `IAI_Q1_RECIPES` | lọc recipe theo id |
| `IAI_Q1_WIDTH` | chiều rộng chuẩn hoá (mặc định 1400) |

### Cách hoạt động

Với mỗi (RAW × recipe):

1. **iAi (ứng viên Develop3)**: `RawImporter` → `apply_scene_to_tilemap(scene,
   settings=Develop3+recipe)` → box-resample về chiều rộng chuẩn.
2. **ART (oracle)**: ghi `.arp` (base + fragment recipe) → gọi `ART-cli.exe`
   (`-p base -p fragment -n -b16 -q -Y`) → PNG 16-bit → box-resample về **đúng
   cùng lưới** để căn pixel.
3. **Chỉ số** (KHÔNG chỉ một con số delta trung bình):
   - MAD (mean |Δ| kênh, 0..255) **tổng và theo shadow/midtone/highlight**;
   - hue drift OKLab (°) trên vùng có màu;
   - neutral chroma delta trên vùng gần trung tính;
   - mỗi bên: OKLab L/C, chroma theo band, clip %, acutance.
4. **Contact sheet**: cặp cạnh nhau **có nhãn** (iAi trái | ART phải) và bản
   **blind** (đảo trái/phải theo hash ổn định, giấu tên engine).

### Cơ sở màu đã khoá

- WB: **camera as-shot** cho cả hai.
- Crop: không.
- iAi: engine **Develop3**, working/output theo scene, output sRGB.
- ART: input `(cameraICC)`, working `Rec2020`, output `RTv2_sRGB`, tone
  auto-matched (mặc định ART), resize long-edge để nhẹ đĩa.

## 4. Recipe chuẩn (đã khoá)

| id | family | iAi | ART |
|---|---|---|---|
| neutral | baseline | default look | default (auto tone) |
| exp_m2/m1/p1/p2 | exposure | exposure ∓20/∓10 (=∓2/∓1 EV) | Compensation ∓2/∓1 EV |
| light_shadows_up | light | shadows +120 (60% dải) | ToneEqualizer Band1 +60 (60% dải) |
| light_highlights_down | light | highlights −120 (60% dải) | ToneEqualizer Band3 −60 (60% dải) |
| color_sat_up | color | saturation +40 (nhẹ) | Saturation +25 (nhẹ) |
| detail_sharpen | detail | sharpening 75 | USM Amount 400 |

**Trung thực về giới hạn black-box**: Exposure khớp chính xác (EV) hai bên; Light
zone khớp theo **cùng tỉ lệ dải** (60%). Color là cú đẩy **nhẹ** (không parity
per-unit — saturation iAi mạnh hơn/đơn vị) để xem "da/màu có tự nhiên khi tăng nhẹ".
Recipe của cả hai được ghi nguyên văn trong manifest.

> **Lưu ý Detail ở kích thước contact-sheet**: sự khác biệt sharpening/nhiễu gần
> như **không thấy** khi ảnh đã giảm kích thước — cặp `detail_sharpen` chủ yếu lặp
> lại khác biệt look nền. So sánh Detail đúng nghĩa cần **crop 100%**; để dành cho
> giai đoạn Detail (S3/S6) khi có công cụ preview theo pixel nguồn.

> Ghi chú Mixer theo band (đỏ/cam/lá/aqua/lam): tạm dùng **saturation toàn cục**
> làm trục màu cho vòng đầu. A/B Mixer theo từng band cần hiệu chỉnh đường HSL của
> ART cho khớp tâm hue; để tránh "mô phỏng ART sai" (vi phạm nguyên tắc black-box),
> việc này để làm ở bước tiếp theo sau khi calibrate đường HSL trong GUI ART.

## 5. Sản phẩm (trong `IAI_Q1_OUT`)

- `index.html` — contact sheet **có nhãn** (iAi trái, ART phải), kèm chỉ số.
- `blind.html` — contact sheet **blind** (A/B, giấu engine).
- `blind_key.csv` — đáp án blind (mở riêng, không link từ blind.html).
- `pairs/*.png`, `blind/<recipe>/*.png` — ảnh cặp cạnh nhau.
- `iai_png/*.png`, `art_png/*.png` — render đơn từng bên (để hash/kiểm chứng).
- `art_arp/*.arp` — recipe ART đã dùng (tái lập được).
- `q1_ab_manifest.json` — manifest đầy đủ: camera, output size, WB, input profile
  provenance, engine, RAW recipe, working/output profile, recipe slider hai bên,
  **sha256 của RAW + render iAi + render ART**.
- `q1_ab_metrics.csv` / `.json` — chỉ số từng cặp.
- `q1_ab_missing.txt` — báo cáo reference thiếu/lỗi.

## 5b. Quan sát sơ bộ (KHÔNG phải chấm chính thức — verdict là của owner)

Nhìn nhanh trên 6 file (mô tả **đặc tính**, không phán "đẹp/xấu"):

- **neutral**: iAi có look **sáng hơn, ấm hơn, tương phản/độ bão hoà cao hơn**
  (kiểu "đã hoàn thiện"); ART **trung tính, phẳng, tối hơn** (kiểu "gốc thô").
- **exposure ±EV**: khớp chính xác; iAi có **shoulder highlight mềm hơn** (váy
  trắng ở +2EV giữ chi tiết lông vũ tốt hơn một chút).
- **light_highlights_down**: iAi **giữ trắng trung tính**; ART bị **ám hồng/tím**
  ở vùng highlight váy — điểm khác biệt rõ.
- **color_sat_up**: đã hạ về mức nhẹ (xem lưu ý per-unit ở trên).
- **detail_sharpen**: không đánh giá được ở kích thước này (xem lưu ý Detail).

Đây chỉ là ghi chú định hướng; owner chấm mới là kết luận.

## 6. VIỆC CỦA OWNER (cổng dừng bắt buộc của Phase 1)

Theo kế hoạch §0.3 mục 7: đã có đủ cặp **neutral + Exposure + Light-zone +
color/Mixer + Detail**. **Dừng lại và nhờ owner chấm bằng mắt** trước khi sang
Phase 2 hoặc tuning look.

Mở `target\q1_ab\index.html` (có nhãn) hoặc `blind.html` (blind, khách quan hơn).
Với mỗi recipe, cho biết bên nào đẹp hơn (hoặc hoà), chú ý: da, highlight (váy/áo
trắng), chuyển vùng sáng/tối, độ nét, và màu có bị giả không.

## 7. Trạng thái commit

- Phase 0: đã commit.
- Harness Phase 1 (`tests/raw_q1_art_ab.rs` + doc này): sẽ commit sau khi chạy
  full-corpus subset xong và eyeball vài cặp. Chưa push (theo quy tắc chỉ commit
  cục bộ).
