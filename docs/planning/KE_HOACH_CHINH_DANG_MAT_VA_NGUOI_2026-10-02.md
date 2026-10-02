# Kế hoạch: Chỉnh dáng mặt và dáng người (Chỉnh chân dung, đợt 10)

Ngày lập: **2026-10-02** · Nhánh: `feat/vector-core-foundation` · Nối tiếp
`KE_HOACH_CHAN_DUNG_KIEU_EVOTO_2026-09-29.md` (Phase 2 "Chỉnh dáng mặt" và
Phase 3 "Dáng người") và đợt 9 (`KE_HOACH_KHOANH_VUNG_VA_CO_TO_MASK_2026-10-01.md`,
hoàn tất 02/10).

Quy ước checklist: `[ ]` chưa làm · `[~]` đã code, chưa qua cổng · `[x]` xong
và qua cổng · `[!]` bị chặn.

## 0. Bối cảnh

- Đợt 9 xong (chủ test OK 02/10). Em đề xuất làm "đồng bộ hàng loạt" trước; **chủ
  chọn làm Chỉnh dáng mặt và Chỉnh dáng người** (02/10).
- Rủi ro chính đã nói với chủ: uốn dáng dễ **làm cong nền phía sau** (cửa, kệ,
  đường chân trời cạnh má/eo) và cần thử nhiều ảnh.
- Sẵn có để dùng lại:
  - 478 mốc mặt MediaPipe (`core::ai::face_mesh`), viền mặt `FACE_OVAL`, mắt,
    mũi, môi, mống mắt (`core::portrait::geometry`).
  - Sapiens2 tách 29 vùng người (`core::ai::body_parts`): mặt+cổ, tóc, thân,
    áo, quần/váy, bắp tay / cẳng tay / bàn tay, đùi / cẳng chân / bàn chân
    (trái, phải), nền. Hiện chỉ chạy trên khung đầu-vai (3e × 4e) và một lần
    nhìn rộng (6e).
  - Lưới dịch chuyển ngược + lấy mẫu song tuyến của công cụ Warp (Liquify)
    (`core::warp::WarpMesh`).
  - Hộp thoại Chỉnh chân dung: xem trước ở luồng nền, Áp dụng ra layer "Chân
    dung", công thức lưu trên layer + file `.iai`, mở lại chỉnh tiếp.

## 1. Quyết định đề xuất (chờ chủ duyệt)

1. Hai mục mới trong hộp thoại Chỉnh chân dung: **"Dáng mặt"** và **"Dáng
   người"**. Mọi thanh mặc định **0** = không đổi dáng.
2. Thứ tự xử lý: chỉnh da/màu/tóc như hiện nay **rồi mới uốn dáng**; cùng một
   layer "Chân dung"; các thanh dáng lưu vào công thức, mở lại chỉnh tiếp được.
3. Cách uốn: **điểm điều khiển** lấy từ mốc mặt (dáng mặt) hoặc khung người
   (dáng người) dời tới vị trí mới; một **vòng neo cố định** quanh vùng ảnh
   hưởng nên nền ở xa không nhúc nhích; trong vòng, ảnh uốn theo kiểu "giữ
   hình cứng nhất có thể" (MLS rigid) để đường thẳng gần mặt/người cong ít
   nhất. Tính trên lưới thô rồi nội suy → nhanh, mượt.
4. Mức tối đa mỗi thanh được giới hạn ở mức còn tự nhiên (Evoto/PTS cũng vậy).
5. **Pha dáng mặt không thêm model mới.** Dáng người thử bằng Sapiens2 đã có
   trước; chỉ khi không đạt mới đề xuất thêm model khung xương MediaPipe Pose
   (Apache-2.0) — **sẽ hỏi chủ trước khi tải**.
6. Vá nền bằng AI (LaMa) cho phần nền lộ ra khi mặt/người thon lại là **pha
   tùy chọn**, chỉ làm khi bản cơ bản qua cổng mà chủ thấy nền còn cong.
7. Ảnh thử: dùng ảnh thử đang có + tải thêm khoảng 10 ảnh chân dung / toàn
   thân công khai (Wikimedia Commons, giấy phép tự do) có nền nhiều đường
   thẳng — **xin chủ đồng ý tải**.

## 2. Các pha

### Pha A0 — Lõi uốn ảnh theo điểm điều khiển

- [ ] Module `core::portrait::reshape`: từ danh sách cặp điểm (gốc → đích) và
      vòng neo, dựng trường dịch chuyển **ngược** (điểm ra lấy màu từ đâu) trên
      lưới thô (ô ≈ e/100, tối thiểu 4 px) chỉ trong khung ảnh hưởng; MLS rigid
      tính ngược (đích → gốc) nên không phải đảo trường.
- [ ] Vẽ: lấy mẫu song tuyến như Warp; cộng dồn nhiều mặt (mỗi mặt một trường,
      khung riêng); nhân độ phủ vùng chọn (`Clip`) vào **độ dời** (không trộn
      ảnh uốn với ảnh gốc → không bóng ma ở mép vùng chọn).
- [ ] Probe `probe_reshape`: ảnh lưới ô vuông + ảnh chân dung có nền nhiều
      đường thẳng; đo độ cong lớn nhất của đường thẳng nền và độ dời ở vòng
      neo (= 0); thời gian (mục tiêu < 150 ms cho ảnh 20 MP một mặt).
- Cổng (nội bộ, trước khi đưa chủ): dời điểm đúng tới đích, ngoài vòng neo
  không đổi một điểm ảnh, đường thẳng cạnh mặt cong < 2 px ở mức vừa.

### Pha A1 — Dáng mặt

- [ ] Thanh (đa số hai chiều −100..100):
  - **Mặt thon**: viền hàm/má (nửa dưới `FACE_OVAL`, từ ngang tai tới cằm) dời
    vào trục giữa mặt, mạnh nhất ở xương hàm, nhẹ dần lên thái dương và về cằm.
  - **Cằm**: các điểm cằm dời theo trục mặt (dài/ngắn cằm).
  - **Mắt to**: viền mắt + mống mắt phóng quanh tâm mắt (trái/phải như nhau).
  - **Mũi**: cánh mũi dời vào/ra trục mũi (nhỏ/hẹp lại hoặc rộng).
  - **Miệng**: khóe miệng dời ra/vào (rộng/hẹp).
  - **Trán**: viền trán (nửa trên `FACE_OVAL`) dời lên/xuống theo trục mặt.
  - (Sau nếu cần) **Cân đối hai mắt**: kéo cỡ và độ cao hai mắt về trung bình.
- [ ] Điểm không thuộc thanh nào được giữ làm neo (ví dụ kéo Mặt thon thì mắt,
      mũi, miệng đứng yên); mặt nghiêng: mức dời mỗi bên theo bề rộng thấy
      được của bên đó (bên khuất dời ít).
- [ ] Probe trên 13 ảnh chân dung đã có + ảnh mới có nền đường thẳng; ảnh trước
      / sau ở 50 và 100 mỗi thanh.
- Cổng: chủ test — ở mức vừa (≈ 50) mặt đổi rõ mà tự nhiên, không thấy nền
  cong; 100 vẫn chấp nhận được; mặt nghiêng không vỡ.

### Pha A2 — Ghép vào hộp thoại

- [ ] Mục "Dáng mặt" trong hộp thoại (sau "Da"/"Mắt & răng"…), xem trước ở
      luồng nền như các thanh khác; "Hiện vùng nhận diện" hiện trên ảnh đã uốn.
- [ ] Áp dụng: layer "Chân dung" phủ cả vùng uốn (điểm ảnh đổi chỗ phải phủ
      kín); công thức lưu các thanh dáng; mở lại → thanh về như cũ.
- [ ] Ảnh nhóm: mỗi mặt bật/tắt như hiện nay; vùng chọn giới hạn uốn.
- Test app: kéo thanh → xem trước khác ảnh gốc trong khung mặt, ngoài vòng neo
  y nguyên; Áp dụng / hoàn tác / mở lại.
- Cổng: chủ test cả luồng trên ảnh thật.

### Pha B0 — Phân tích dáng người (cổng giữ/bỏ cách làm)

- [ ] Khung người: từ mặt kéo xuống theo tỉ lệ người (≈ 8 lần chiều cao đầu),
      cắt theo vùng ảnh / vùng chọn; chạy Sapiens2 trên 1–2 khung đứng (nửa
      trên, nửa dưới) để người không quá nhỏ ở 512×384.
- [ ] Từ các vùng: **bóng người** (mọi lớp trừ nền), thân, tay, chân; suy ra
      đường vai, eo (chỗ thân hẹp nhất giữa nách và hông), hông, trục bắp tay /
      cẳng tay / đùi / cẳng chân (trục chính của từng vùng), cổ (giữa cằm và
      vai).
- [ ] Probe trên ảnh nửa người, toàn thân đứng, ngồi, áo rộng, váy dài, tay
      chống hông.
- Cổng: đường eo/hông/vai và trục tay chân đúng ở ≥ 8/10 ảnh thử. Không đạt →
  đề xuất chủ cho tải MediaPipe Pose (khung xương 33 điểm) ghép với vùng
  Sapiens2.

### Pha B1 — Dáng người

- [ ] Thanh:
  - **Eo thon**: hai mép bóng người ở dải eo dời vào trục thân, nhạt dần lên
    ngực và xuống hông.
  - **Tay thon**: hai mép bắp tay (và cẳng tay, nhẹ hơn) dời vào trục tay.
  - **Chân dài**: kéo dãn theo chiều dọc phần dưới hông (cả bề ngang ảnh nên
    đường thẳng đứng không cong); phần cuối ảnh bị đẩy ra ngoài khung — chỉ
    cho dài tới mức còn chỗ dưới bàn chân.
  - **Chân thon**: mép đùi / cẳng chân dời vào trục chân.
  - **Vai**: hai đầu vai dời vào/ra (hẹp/rộng vai).
  - **Cổ**: kéo dãn dải giữa cằm và vai theo chiều dọc (cổ cao), đầu dời lên.
- [ ] Neo: bóng người nới rộng một khoảng làm vòng neo; tay ép sát thân thì eo
      và tay dùng chung neo để không xé nhau.
- Cổng: chủ test — ở mức vừa người thon tự nhiên, nền cạnh eo/tay không cong
  thấy rõ.

### Pha C (tùy chọn) — Giữ nền thẳng bằng vá AI

- [ ] Uốn chỉ người (theo bóng người mềm), phần nền lộ ra khi người thon lại vá
      bằng LaMa từ nền gốc (đã có cho Smart Fill / Repair Brush).
- Chỉ làm khi chủ thấy nền còn cong ở Pha A/B.

## 3. Rủi ro

| Rủi ro | Xử lý |
|---|---|
| Nền cong cạnh má/eo (cửa, kệ, chân trời) | Vòng neo sát + MLS rigid + giới hạn mức; probe đo độ cong; Pha C vá nền |
| Mốc mặt lệch ở mặt nghiêng / bị che (tay, tóc) | Mức dời theo bề rộng thấy được mỗi bên; bỏ mặt có mốc kém tin cậy |
| Kính mắt bị méo khi Mắt to | Chấp nhận ở mức vừa; ghi chú trong gợi ý thanh |
| Sapiens2 khung cả người quá nhỏ ở 512×384 | Chạy 2 khung dọc; không đạt thì đề xuất MediaPipe Pose |
| Tay ép sát thân, váy rộng che eo | Neo chung; eo đọc theo bóng người, không theo da |
| Ảnh lớn chậm | Lưới thô, chỉ tính trong khung ảnh hưởng, luồng nền |

## 4. Changelog

- **2026-10-02** — Lập kế hoạch theo lựa chọn của chủ (dáng mặt + dáng người);
  chưa code. Chờ chủ duyệt mục 1 (nhất là tải ảnh thử, và Pose chỉ khi cần).
