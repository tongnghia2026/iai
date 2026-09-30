# Kế hoạch: khoanh vùng trước khi phân tích + cọ tô thêm/bớt mask (Chỉnh chân dung, đợt 9)

Ngày lập: **2026-10-01** · Nhánh: `feat/vector-core-foundation` · Nối tiếp
`KE_HOACH_MASK_DA_THUAT_TOAN_MAU_2026-09-30.md` (đợt 8, chủ test đạt).

Quy ước checklist: `[ ]` chưa làm · `[~]` đã code, chưa qua cổng · `[x]` xong
và qua cổng · `[!]` bị chặn.

## 0. Bối cảnh

- Chủ test đợt 8 đạt, gửi ảnh "Hiện vùng nhận diện" (ảnh thẻ nam tóc mái): vùng
  **tóc** lấy chưa hết — tóc mai hai bên thái dương, viền mép mái sát trán/lông
  mày, viền ngoài đỉnh đầu và các sợi tóc bay trên nền.
- Phân tích (probe trên ảnh thử Judy Chu, Jonny Kim, Lauren Underwood):
  1. **Tóc mai bị bỏ do quy tắc "tối hơn da"**: trọng số tóc ngoài lõi =
     `1 − smoothstep(ref − 0.4, ref − 0.15, luma)` với `ref` = độ sáng da quanh
     đó. Hai bên mặt da nằm trong bóng (ref ≈ 0,36) nên tóc đen 0,19 chỉ được
     0,03 dù Sapiens2 báo 83% là tóc. `ref` còn bị kéo tối vì xác suất da mềm
     của Sapiens2 loang lên chân tóc.
  2. **Viền ngoài**: vùng tóc nhân `on_person` (Sapiens2 tóc+da) nên dừng đúng
     ở mép thô của model (512×384); điểm viền pha màu nền sáng hơn nên hệ số
     "tối" cũng loại.
  3. **Mép mái sát lông mày/mắt**: vùng loại trừ lông mày (nở 0,02e + mềm
     0,03e) và mắt (0,03e + 0,03e) rộng.
  4. Tóc sau tai mà Sapiens2 không thấy (xác suất < 0,2) thì không tự bắt được.
- **Chủ đề xuất (01/10)**: thay vì bắt AI quét toàn ảnh, cho **khoanh vùng
  trước khi AI chạy**; AI lấy không hết thì **người dùng tô thêm/bớt**, rồi mới
  kéo các thanh sáng tối, màu sắc, mạnh yếu.

## 1. Quyết định đề xuất (chờ chủ duyệt)

1. Làm theo thứ tự **Pha 0 → Pha 1 → Pha 2**; mỗi pha một bản build cho chủ test.
2. Khoanh vùng **dùng lại các công cụ chọn sẵn có** (Marquee, Lasso, Smart
   Select W…): có vùng chọn khi mở Chỉnh chân dung thì chỉ phân tích trong vùng
   đó; không có thì tự động như hiện nay. Không thêm công cụ vẽ khung mới.
3. Cọ tô theo khuôn **Refine Selection** (công cụ riêng tự bật khi mở hộp thoại,
   trả công cụ cũ khi đóng): chế độ Thêm / Bớt / Thông minh (bám mép màu), cỡ cọ
   `[` `]`, Alt = đảo Thêm↔Bớt, Ctrl+Z trong hộp thoại.
4. Mask tô được: **Da** và **Tóc** trước; Môi, Răng, Quầng thâm sau nếu cần.
5. Không đổi công thức các hiệu ứng; cọ chỉ sửa mask mà hiệu ứng đọc.

## 2. Các pha

### Pha 0 — Sửa nhanh vùng tóc tự động (nhỏ)

- [ ] Hệ số "tối hơn da" theo **tỉ lệ** thay cho hiệu cố định: tóc đủ khi
      `luma < 0,55·ref`, không tính khi `luma > 0,85·ref`.
- [ ] `ref` lấy từ **mask da mới** (đợt 8) trong vùng mặt, ngoài vùng mặt mới
      dùng Sapiens2; điểm là da theo mask mới thì không là tóc.
- [ ] Viền ngoài: dải ~e/20 quanh mép tóc Sapiens2, tách sợi tóc khỏi nền theo
      độ sáng tóc/nền tại chỗ (alpha = (nền − điểm)/(nền − tóc), chỉ khi tóc và
      nền đủ tương phản) — kiểu Refine Edge.
- [ ] Thu hẹp vùng loại trừ quanh lông mày/mắt (~0,005–0,01e).
- [ ] Probe: ảnh vùng tóc cũ/mới + ảnh chỉnh "Sáng tóc" mạnh; soát rò sang nền
      tối (kệ sách sau Mazie Hirono, nền xanh đậm NASA), áo tối.
- Cổng: tóc mai hai bên, mép mái được tô; không lan ra nền/áo.

### Pha 1 — Khoanh vùng trước khi phân tích

- [ ] Mở Chỉnh chân dung khi đang có vùng chọn → chỉ dò mặt trong vùng đó (ảnh
      nhóm: chọn đúng người cần chỉnh) và dòng trạng thái ghi "Phân tích trong
      vùng chọn".
- [ ] Sapiens2 chạy trên khung ôm sát vùng chọn (giữ tỉ lệ 3:4 của model) thay
      cho khung 3 lần bề rộng mặt → mép tóc/da **mịn hơn tới ~2 lần** khi khoanh
      sát đầu.
- [ ] Mask (da, tóc, môi…) cắt theo vùng chọn (mép mềm theo vùng chọn).
- Cổng: khoanh sát đầu cho mép tóc rõ hơn tự động; ảnh nhóm chỉ xử lý người
  được khoanh.

### Pha 2 — Cọ tô thêm/bớt mask

- [ ] Hộp thoại có mục **"Tô vùng"**: chọn loại mask (Da / Tóc), chế độ Thêm /
      Bớt / Thông minh, cỡ cọ, độ mềm; tự bật "Hiện vùng nhận diện" cho loại
      đang tô.
- [ ] Công cụ `PortraitBrush` (như `RefineBrush`): nét tô ghi vào lớp sửa riêng
      của từng mặt (thêm/bớt, u8) chồng lên mask phân tích; Thông minh dùng lõi
      cắt đồ thị của Smart Select để nét tô bám mép màu (tóc/nền).
- [ ] Sau mỗi nét: tính lại phần phụ thuộc mask chỉ trong khung nét tô (da:
      interior, làm mịn có trọng số; tóc: vùng tóc) → xem trước theo luồng nền
      như thanh kéo.
- [ ] Ctrl+Z / Ctrl+Shift+Z trong hộp thoại; Hủy bỏ hết nét tô; Áp dụng tạo
      layer như cũ.
- Cổng: tô thêm phần tóc/da AI bỏ sót và bớt phần lấy thừa bằng vài nét; kéo
  thanh sau khi tô cho kết quả đúng vùng đã tô.

### Pha 3 — (sau, tùy chủ) lưu mask đã tô

- [ ] Lưu mask đã tô cùng layer "Chân dung" để mở lại chỉnh tiếp; nền cho đồng
      bộ hàng loạt (Phase 5 kế hoạch Evoto).

## 3. Rủi ro

| Rủi ro | Xử lý |
|---|---|
| Viền ngoài tách theo độ sáng rò sang nền tối có vân (kệ sách) | Dải hẹp ~e/20 quanh mép Sapiens2, chỉ khi tóc/nền tương phản đủ; probe ảnh Mazie |
| Cọ tô trên ảnh 50 MP chậm | Chỉ tính lại trong khung nét tô; xem trước theo luồng nền sẵn có |
| Xung đột công cụ canvas khi hộp thoại mở | Theo đúng khuôn Refine Selection (khóa modal, trả công cụ cũ) |

## 4. Changelog

- **2026-10-01** — Lập kế hoạch theo đề xuất của chủ (khoanh vùng + cọ tô) và
  kết quả phân tích vùng tóc; chưa code.
