# Kế hoạch: mask da bằng thuật toán màu + loang (Chỉnh chân dung, đợt 8)

Ngày lập: **2026-09-30** · Nhánh: `feat/vector-core-foundation` · Nối tiếp
`KE_HOACH_CHAN_DUNG_KIEU_EVOTO_2026-09-29.md` (Phase 1 đã xong, đợt 1→7).

Quy ước checklist: `[ ]` chưa làm · `[~]` đã code, chưa qua cổng · `[x]` xong
và qua cổng · `[!]` bị chặn (ghi lý do ở Changelog).

## 0. Bối cảnh và vấn đề

- Chủ test đợt 6 (30/09), gửi ảnh layer "Chân dung" của một ảnh thẻ nam, tóc
  mái. Điểm trắng trên layer = điểm **không đổi** (layer chỉ chứa điểm đổi,
  gồm cả chỗ đổi < 1 mức màu, nên các "vết nứt" mảnh trên má/cổ phần lớn không
  phải lỗ mask). Lỗi thật:
  1. **Lỗ lớn ở trán dưới mái tóc**: nghi Sapiens2 (512×384, mỗi ô ≈ e/128
     px ảnh) coi phần trán dưới mái là tóc → mask da = 0 → không làm mịn/sáng.
  2. **Viền an toàn quanh lông mày, môi (và mắt) quá rộng**: `build_face`
     trừ đa giác lông mày (grow 0,015e, feather 0,02e), mắt (0,02e/0,02e), môi
     lấy từ Sapiens2 → da sát các đặc điểm không được xử lý.
  3. Vùng cùng màu da nhưng model không gán nhãn da thì bị bỏ.
- Chủ hỏi "tự viết thuật toán thay AI được không" → chốt hướng: **AI chỉ định
  hướng, thuật toán màu quyết định mép** (4 ý ở mục 2). Chủ dặn: làm kế hoạch
  trước, sửa ở hội thoại mới.
- Việc đầu tiên của phiên mới: xin chủ **ảnh gốc tấm đó (nếu chủ cho dùng) +
  ảnh bật "Hiện vùng nhận diện"** để xác nhận nguyên nhân lỗ trán.

## 1. Quyết định đã khóa

1. Offline, không model mới. MediaPipe Face Mesh (mốc) + Sapiens2 (hàng rào)
   giữ nguyên; phần mới là thuật toán cổ điển trên ảnh gốc, đủ độ phân giải.
2. **Chỉ thay mask da** (`skin` trong `FaceModel`). Không đổi công thức hiệu ứng
   (làm mịn, đều màu, mụn…), không đổi phần tóc đợt 6 (Shadows/Blacks của
   Develop), mắt/tròng/răng giữ như cũ trừ khi nêu ở mục 3-D.
3. Code module riêng `src/core/portrait/skin_mask.rs`; `analysis.rs` chỉ gọi.
   Tái dùng: cắt đồ thị của `core::quick_select` (Smart Select W, chủ đã duyệt
   "giống PTS"), guided filter màu của `core::refine` (Refine Selection).
4. Có **công tắc tạm** (hằng/biến môi trường cho probe) chọn mask cũ/mới để so
   trên cùng ảnh; gỡ công tắc khi qua cổng.
5. Không đạt → revert gọn (mỗi pha một commit riêng, chưa push).
6. Không dùng ảnh cá nhân trong Pictures của chủ. Ảnh thử: NASA public domain
   (đã có trong scratchpad cũ: kim, meir, nelson, artemis) + ảnh public domain
   bổ sung (Wikimedia): da ngăm, tóc vàng/bạc, tóc mái, nền be/gỗ, đeo kính,
   râu, ánh đèn vàng. Tải với User-Agent chung, không gửi thông tin chủ.

## 2. Hướng giải (đã thống nhất với chủ)

1. Mốc MediaPipe cho vị trí mắt, lông mày, môi, mũi, viền mặt → lấy mẫu màu da
   ở má, trán, sống mũi, cằm của chính ảnh.
2. Từ mẫu, tính từng điểm ảnh gốc "giống da bao nhiêu"; chịu được bóng và lóa
   vì mẫu lấy ở nhiều vùng sáng tối.
3. Loang / cắt đồ thị (như Smart Select) lấy trọn vùng da liền mạch quanh mặt
   và cổ; mắt, lông mày, môi, lỗ mũi cắt sát theo mốc thay vì chừa dải rộng;
   làm mềm mép bằng bộ Refine.
4. Sapiens2 chỉ làm hàng rào: chặn da loang ra nền, áo, tóc cùng màu.

## 3. Thiết kế chi tiết

Đơn vị: `e` = chiều cao mặt (trán–cằm) theo mốc; toạ độ trong `Region` của mặt
(đã có, nới thêm phía dưới để phủ cổ nếu cần).

### 3-A. Lấy mẫu màu da theo mốc

- Vùng mẫu (đa giác từ mốc, co vào để tránh mép): má trái/phải (dưới quầng
  mắt, trên rãnh mũi–má, quanh mốc `CHEEKS`), sống mũi (giữa 6–197–195), cằm
  (trên 152), trán (giữa lông mày và mốc 10, chỉ giữ điểm Sapiens2 KHÔNG chắc
  tóc, vì mái có thể che).
- Loại điểm lạ trong mẫu: quá tối/lóa (ngoài phân vị 3–97% độ sáng của mẫu),
  đốm mụn (điểm `spot` sẵn có), râu (điểm tối hơn nền da cục bộ rõ rệt).
- Ít nhất vài nghìn điểm; nếu một vùng không đủ (tóc che trán, kính che sống
  mũi) thì bỏ vùng đó.

### 3-B. Mô hình màu → bản đồ "giống da"

- Không gian: sắc độ tách khỏi độ sáng (vd. Lab: a*, b* chặt; L* lỏng, hoặc tỉ
  lệ màu r/(r+g+b), g/(r+g+b) + độ sáng) để bóng và lóa vẫn là da.
- Da: GMM 3–5 thành phần (hoặc histogram 2D sắc độ làm mịn) học từ mẫu 3-A;
  thành phần độ sáng dạng dải rộng (chấp nhận từ bóng sâu tới lóa, trừ trắng
  cháy).
- Không-da: mẫu từ tóc chắc (Sapiens2 ≥ 0,9, xa mặt), nền/áo (Sapiens2), bên
  trong đa giác mắt/lông mày/lòng miệng.
- Điểm số từng điểm = log-tỉ-lệ (da / không-da), cắt trần như `MAX_COST` của
  quick_select. Xuất ảnh xám để kiểm tra (probe).

### 3-C. Loang / cắt đồ thị

- Tái dùng maxflow + chi phí biên theo tương phản của `quick_select.rs` (tách
  phần lõi thành hàm `pub(crate)` dùng chung, không đổi hành vi Smart Select —
  test cũ `maxflow_matches_brute_force_min_cut` phải vẫn qua).
- Hạt cứng DA: vùng mẫu 3-A. Hạt cứng KHÔNG-DA: đa giác mắt/lông mày co vào,
  lòng miệng, lỗ mũi tối, tóc chắc ở xa trán, mọi điểm ngoài hàng rào 3-D.
- Chi phí vùng = điểm số 3-B; chi phí biên = tương phản màu (EdgeCache).
- Chỉ giữ phần **liên thông** với hạt da (không nhảy sang vùng cùng màu ở xa:
  tay, nền be).
- Ảnh lớn: giải thô ở ~1/4 rồi tinh ở dải quanh mép (coarse-to-fine như Smart
  Select) để thời gian thêm ≤ ~1–1,5 s/mặt trên máy chủ.

### 3-D. Hàng rào Sapiens2 + cắt đặc điểm sát

- Vùng cho phép = (Sapiens2 mặt+cổ+da thân, nở rộng ~e/15) ∪ (viền mặt
  MediaPipe nở ~0,05e). Ngoài = không-da cứng.
- Tóc: chỉ chặn nơi Sapiens2 chắc là tóc **và** màu không giống da (điểm 3-B
  thấp) → trán dưới mái được mở lại vì đúng màu da.
- Mắt: đa giác mắt grow nhỏ (~0,005e, feather ~0,01e) thay cho 0,02e/0,02e.
- Lông mày: bỏ dải an toàn đa giác; thay bằng "sợi lông mày" = điểm trong đa
  giác lông mày nở nhẹ mà tối hơn da cục bộ (giống mask `brows` hiện có) → da
  giữa và quanh sợi vẫn được xử lý.
- Môi: mép môi theo màu (môi đỏ/sẫm hơn da) trong dải hẹp quanh viền môi
  MediaPipe; lòng miệng/răng loại cứng. (Đợt 5 cho thấy tách màu thẳng ở miệng
  bị răng sáng đánh lừa → chỉ dùng mô hình da-vs-môi, loại điểm quá sáng/ít
  bão hoà.)
- Lỗ mũi: giữ cách hiện tại (chỉ phần tối trong vùng lỗ mũi).

### 3-E. Làm mềm mép và tích hợp

- Mép mask qua guided filter màu nhỏ (`refine::guided_color`, bán kính ~2–3
  px) + mờ dần cạnh khung (`side_fade`) như cũ. Không dùng lại cách "tách mép
  theo màu quyết từng điểm" của đợt 5 (chủ chê bệt) — ở đây graph cut quyết
  vùng, guided filter chỉ làm mềm.
- `interior`, `low1/low2` (masked blur), mụn, quầng thâm… tự dùng mask mới.
- "Hiện vùng nhận diện" hiển thị mask mới; probe ghi thêm `pr_*_skinmap`
  (điểm số 3-B) và `pr_*_skin_old/new` để so.

## 4. Các pha

- [x] **A. Công cụ đo** — probe: chỉ số (1) **lỗ** = số điểm trong viền mặt
      MediaPipe, ngoài mắt/lông mày/môi/lỗ mũi, có màu giống mẫu da mà mask <
      0,5; (2) **rò** = điểm mask > 0,5 mà Sapiens2 chắc tóc/nền/áo và màu
      không giống da; (3) thời gian. Chạy trên bộ ảnh thử với mask CŨ làm mốc.
- [x] **B. Mẫu + mô hình màu** (3-A, 3-B) → ảnh xám "giống da" hợp lý trên
      mọi ảnh thử (kể cả ảnh đèn vàng, da ngăm).
- [x] **C. Cắt đồ thị + hàng rào + hạt cứng** (3-C, 3-D phần hàng rào/tóc).
- [x] **D. Cắt đặc điểm sát** (3-D mắt, lông mày, môi, lỗ mũi).
- [x] **E. Làm mềm + tích hợp** vào `build_face` (công tắc cũ/mới), Hiện vùng
      nhận diện, test đơn vị (vd. ảnh tổng hợp: mặt màu da có "mái" màu da đậm
      che trán → mask phải phủ phần trán lộ; lông mày tổng hợp → da quanh sợi
      được giữ).
- [x] **F. Đo + tối ưu + build**: `cargo fmt --check`, `cargo test --lib`
      (release), build Release có Canvas Editor (`--target-dir
      target\portrait-test` nếu exe chính bị khoá), đưa đường dẫn exe thật.
- **Cổng nghiệm thu (chủ test)**: trên ảnh bị lỗ trán + vài ảnh khác — không
  còn lỗ ở vùng da liền mạch cùng màu; viền chừa quanh lông mày/môi/mắt chỉ
  còn vài px; không lan ra tóc/nền/áo cùng màu; thời gian phân tích thêm ≤ ~1,5
  s; các thanh cũ vẫn cho kết quả như trước ở vùng da đã đúng.
- **Gỡ nếu không đạt**: xoá `src/core/portrait/skin_mask.rs`, trả `build_face`
  về `refine_skin` (Sapiens2 + guided filter), trả phần tách maxflow về như cũ.

## 5. Rủi ro và cách xử lý

| Rủi ro | Xử lý |
|---|---|
| Tóc vàng/bạc, nền be/gỗ, áo màu da trùng màu da | Hàng rào Sapiens2 + chỉ giữ vùng liên thông + chi phí biên theo mép |
| Bóng sâu (cằm, cổ), lóa trán | Mô hình tách sắc độ khỏi độ sáng; mẫu nhiều vùng sáng tối |
| Ánh đèn màu (vàng/xanh) | Mẫu lấy từ chính ảnh nên tự theo ánh đèn |
| Râu, lông tơ | Râu: điểm tối trong dải hàm/môi trên → không-da mềm; không cố "làm mịn" râu |
| Trang điểm đậm, má hồng | GMM nhiều thành phần; mẫu cả má |
| Kính, tay chạm mặt | Kính: Sapiens2 lớp kính = không-da; tay: liên thông + hàng rào (tay là da thân → cho phép? quyết định khi thử) |
| Ảnh đen trắng / sắc độ yếu | Phát hiện độ bão hoà thấp → dùng mask cũ (Sapiens2) |
| Chậm trên ảnh 50+ MP | Coarse-to-fine; đo từng pha |

## 6. Changelog

- **2026-09-30** — Lập kế hoạch theo đề xuất đã chủ duyệt; chưa code. Phiên
  mới: xin ảnh "Hiện vùng nhận diện", làm pha A trước.
- **2026-10-01** — Code xong A→F, chờ chủ test (cổng nghiệm thu). Module
  `src/core/portrait/skin_mask.rs`; `build_face` gọi nó, lùi về mask cũ khi ảnh
  quá ít màu (ảnh đen trắng). Probe `probe_skin_mask`
  (`IAI_PORTRAIT_SKIN_PROBE=<thư mục>`, thêm `IAI_PORTRAIT_SKIN_RENDER=1` để
  xuất ảnh chỉnh cũ/mới, `IAI_PORTRAIT_SKIN_ONLY=<tên>` để chạy 1 ảnh) in lỗ/rò/
  thời gian, ghi `pr_*_cmp` (cũ | mới), `pr_*_ev` (bản đồ giống da). Bộ ảnh thử
  public domain: 4 phi hành gia Artemis II, Kim, Meir, Nelson + chân dung Quốc
  hội Mỹ (Judy Chu, Mazie Hirono — tóc mái; Lauren Underwood — da ngăm, đeo
  kính; Hakeem Jeffries — da ngăm).
  Khác thiết kế ban đầu, rút ra khi đo:
  1. **Không gian màu = Lab a\*b\*** (không phải tỉ lệ log r/g, b/g): đo trên 11
     mặt, sắc độ Lab của da gần như không đổi từ L\* 25 tới 85 (góc màu 45–60°),
     còn tỉ lệ log tăng mạnh khi tối → mô hình cũ bỏ cả vùng bóng. Độ nới dọc
     hướng sắc độ 20% (sáng) → 45% (L\* < 15–35).
  2. Ở điểm gần đen (L\* < 3–15) hoặc cháy sáng, màu không đọc được → bỏ bằng
     chứng màu, để Sapiens2 quyết (nửa mặt khuất sáng của ảnh Artemis L\* 2–6).
  3. **Lông mày không dùng mô hình màu trong bước cắt**: lông mày vàng/nhạt
     trùng màu da làm mất thái dương, lông mày rậm thì Sapiens2 (không có lớp
     lông mày) kéo vào da. Thay bằng kiểm tra từng điểm: sợi = tối hơn da ngay
     quanh lông mày (vùng lông mày loại trừ, bán kính e/30) hoặc tối hơn lân cận
     nhỏ (e/150) → da giữa các sợi vẫn được xử lý, hết dải viền đậm.
  4. Mắt: lòng mắt chặn cứng, lông mi = sợi tối (vùng mi nở 0,012e), guard
     0,005e/0,01e; không mô hình màu mắt (mống mắt nâu trùng da trong bóng).
  5. Môi: mô hình màu môi/răng/lòng miệng chỉ có hiệu lực trong 0,03e quanh môi.
  6. Mẫu "không phải da" (tóc, nền, áo) bị bỏ khi trùng màu da **và** sáng như
     da mẫu (trán dưới mái AI nhận nhầm là tóc); tóc nâu sẫm vẫn là tóc.
  7. Lấp "lỗ kín" trong vùng da ≤ 0,02e² không chạm mắt/lông mày/miệng khi
     Sapiens2 cũng đọc là da (bóng áo xanh hắt dưới cằm Jeffries).
  8. Tròng kính: Sapiens2 "kính" = trung lập (da sau tròng kính được xử lý).
  9. Mép: guided filter luma bán kính e/100 (sắc ở mép thật, mềm nơi phẳng),
     không lan quá ~e/60 theo sợi tóc sáng.
  Kết quả probe (lỗ % mask cũ → mới): Artemis 0,7/2,4/0,8/0,9 → 0,4/1,9/0,4/0,0;
  Jeffries 0,4 → 0,0; Judy 5,9 → 4,3; Kim 0,2 → 0,0; Lauren 33 → 7; Mazie 3,5 →
  2,7; Meir 0,7 → 0,0; Nelson 0,9 → 2,1 (tóc vàng thưa ở thái dương); rò ≤
  0,06%. Thời gian mask 0,07–1,8 s/mặt (ảnh 9 MP vùng mặt), "chuẩn bị" tăng
  ~0,2–0,7 s so với mask cũ. 1818 test pass. Công tắc cũ/mới (`set_legacy`)
  còn giữ cho probe; gỡ khi chủ duyệt.
- **2026-10-01** — **Chủ test đạt** (ảnh thẻ nam tóc mái bị lỗ trán trước đây:
  "Hiện vùng nhận diện" phủ kín trán dưới mái, sát lông mày/môi/mắt). Qua cổng
  nghiệm thu; công tắc cũ/mới chỉ còn trong bản test (`#[cfg(test)]`, cho
  probe so sánh), bản chạy thật luôn dùng mask mới (mask cũ chỉ khi ảnh quá ít
  màu).
