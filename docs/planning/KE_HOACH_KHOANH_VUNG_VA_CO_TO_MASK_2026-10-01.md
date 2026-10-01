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

## 1. Quyết định đã khóa (chủ duyệt 2026-10-01)

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

- [x] Hệ số "tối hơn da" theo **tỉ lệ** thay cho hiệu cố định: tóc đủ khi
      `luma < 0,55·ref`, không tính khi `luma > 0,85·ref`.
- [x] `ref` lấy từ **mask da mới** (đợt 8) trong vùng mặt, ngoài vùng mặt mới
      dùng Sapiens2; điểm là da theo mask mới thì không là tóc.
- [x] Viền ngoài: tách sợi tóc khỏi nền kiểu Refine Edge, **theo màu RGB** (không
      chỉ độ sáng: tóc nâu tối trên nền xanh đậm gần cùng độ sáng): chiếu màu
      điểm lên đoạn màu nền → màu tóc tại chỗ; chỉ khi nền trơn và màu nền/tóc
      cách xa đủ. Thêm nhóm "Nền" (class 0) vào nhóm vùng Sapiens2. Màu tóc lấy
      từ lõi vùng tóc của model (mép model hay lấn ra nền). Ở dải tóc giáp nền,
      phép tách màu thay quy tắc "tối hơn da" → hết quầng màu quanh tóc.
- [x] Thu hẹp vùng loại trừ quanh lông mày/mắt (0,005e/0,015e và 0,01e/0,015e);
      chỗ model chắc chắn là tóc (lọn tóc vắt qua đuôi lông mày) thì không loại.
- [x] Probe 13 ảnh: tóc mai/chân tóc được bắt (Judy, Kim, Mazie, Lauren,
      Jeffries); quầng màu viền tóc trên nền xanh hết (Nelson, Lauren); không rò
      sang kệ sách (Mazie). Tính trên lưới thô (ô ≈ e/150) → bớt RAM, nhanh hơn.
      Còn lại (để cọ tô Pha 2): tóc bạc sáng hơn da trên tường xám (tóc mai
      bạc không tự bắt), nền lọt qua tóc mà model chắc chắn là tóc.
- Cổng: tóc mai hai bên, mép mái được tô; không lan ra nền/áo. **Đạt — chủ test
  OK 01/10.**

### Pha 2b — Da cổ / ngực không bị cắt ngang

- [x] Chủ test cọ Thông minh OK; gửi ảnh: da ở cổ áo chữ V bị cắt thẳng ngang
      (đáy khung phân tích mặt = cằm + 0,45e). Sửa: khi model tách vùng tin
      cậy, đáy khung kéo xuống tới hàng cuối Sapiens2 còn thấy da (mặt + thân,
      > 0,5) trong bề ngang mặt, + 0,1e (`skin_reach`) — chỉ ảnh hở cổ mới dài
      ra (Judy, Mazie, c01), áo vest/cao cổ giữ khung cũ nên không chậm thêm.
      Màu da trung bình (Đều màu da) vẫn chỉ đọc tới cằm + 0,45e
      (`tone_rows`). Cọ tô Da với tới vùng này theo.

### Pha 1 — Khoanh vùng trước khi phân tích (chủ cho làm sau Pha 2)

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

- [x] Hộp thoại có mục **"Tô vùng"** (đầu hộp thoại): Tắt / Da / Tóc, chế độ
      Thông minh / Thêm / Bớt, cỡ cọ, độ cứng, nút hoàn tác/làm lại; khi tô, lớp
      phủ màu (da đỏ, tóc tím) của vùng đang tô hiện trên canvas (texture GPU,
      vá từng vùng nét tô), ảnh xem trước vẫn là ảnh đã chỉnh.
- [x] Dùng lại công cụ **Refine Brush** (vòng con trỏ, `[` `]`, Shift+`[` `]`,
      Alt+chuột phải kéo cỡ, Alt đảo Thêm↔Bớt, Alt+Thông minh = trả lại vùng app
      tìm): khi không có phiên Refine Selection, nét tô xếp hàng ở
      `Canvas::mask_brush` cho hộp thoại xử lý mỗi khung hình. Mỗi nét chỉ tô
      một mặt (mặt gần điểm bắt đầu).
- [x] **Thông minh làm lại (chủ: "cọ không thông minh", đề xuất thử Color
      Range)**. Thử trên ảnh thật (Meir tóc xoăn mảnh trên áo trắng, Nelson sợi
      bạc trên nền xanh): cọ cũ (`refine_edge_stamp`) còn **khoét lỗ** vùng tóc
      đã có; Color Range lấy màu ở điểm bấm thì sợi mảnh trên nền sáng gần như
      không ăn, còn trên Nelson **chọn lan cả nền xanh** (thước đo Color Range
      thiên về độ sáng mà tóc nâu tối và nền xanh đậm gần cùng độ sáng). Chốt:
      **Color Range chấm theo cả hai phía** — quanh cọ lấy mẫu màu vùng đang tô
      (mask ≥ 0,9) và phần còn lại (mask ≤ 0,1, chỉ điểm phẳng để sợi tóc bay
      không bị tính là nền; đo bằng bước lệch lớn nhất với điểm kề nên sợi rộng
      1 px cũng nhận ra), bỏ mẫu hai phía trùng nhau (nền lộ giữa các sợi trong
      mask), mỗi điểm dưới cọ lấy tỉ lệ khoảng cách Lab tới mẫu gần nhất mỗi
      phía → sợi mờ được tô mờ, nền giữ 0, màu không giống bên nào (da cạnh tóc)
      bị loại. Chỉ cộng thêm, tính trên mask lúc bắt đầu nét → **tô lại để sợi mờ
      đậm hơn**; Alt + Thông minh = bớt phần giống nền.
- [x] Sau mỗi nét: tóc dùng ngay; da tính lại cả khối `SkinLayers` (mask,
      interior, quầng mắt, tách tần số có trọng số, màu da) trên luồng nền rồi
      xem trước lại. Chấm mụn giữ theo lần phân tích (bớt da thì hết xóa mụn ở
      đó; thêm da không dò mụn mới).
- [x] Ctrl+Z / Ctrl+Shift+Z trong hộp thoại; Hủy bỏ hết nét tô; Áp dụng dùng
      mask đã tô (đợi da tính xong) và trả công cụ cũ.
- Cổng: tô thêm phần tóc/da AI bỏ sót và bớt phần lấy thừa bằng vài nét; kéo
  thanh sau khi tô cho kết quả đúng vùng đã tô. **Đạt — chủ test OK 01/10**
  (chủ bất ngờ vì Thông minh tự nhận sợi tóc mảnh khi tô lại vùng tóc).

### Pha 3 — (sau, tùy chủ) lưu mask đã tô

- [ ] Lưu mask đã tô cùng layer "Chân dung" để mở lại chỉnh tiếp; nền cho đồng
      bộ hàng loạt (Phase 5 kế hoạch Evoto).

### Pha 5 — (sau) Cọ Thông minh cho Refine Selection

- [ ] Chủ 01/10: đưa thuật toán "Color Range hai phía" của cọ Thông minh
      (`core::portrait::brush`) sang Refine Brush của Refine Selection (thay
      `refine_edge_stamp` ở chế độ Smart; mẫu lấy từ vùng chọn đang tinh chỉnh).

### Pha 4 — (sau) "Sáng tóc" bằng thanh Blacks của Develop

- [ ] Chủ test 01/10: tăng sáng tóc hiện khá yếu → áp thẳng thanh **Blacks** của
      Develop cho vùng tóc cho nhanh. Ảnh chủ gửi (kéo tóc sáng/bạc mạnh) còn
      một **viền cam ở chân tóc** giáp trán — soát khi làm.

## 3. Rủi ro

| Rủi ro | Xử lý |
|---|---|
| Viền ngoài tách theo độ sáng rò sang nền tối có vân (kệ sách) | Dải hẹp ~e/20 quanh mép Sapiens2, chỉ khi tóc/nền tương phản đủ; probe ảnh Mazie |
| Cọ tô trên ảnh 50 MP chậm | Chỉ tính lại trong khung nét tô; xem trước theo luồng nền sẵn có |
| Xung đột công cụ canvas khi hộp thoại mở | Theo đúng khuôn Refine Selection (khóa modal, trả công cụ cũ) |

## 4. Changelog

- **2026-10-01** — Lập kế hoạch theo đề xuất của chủ (khoanh vùng + cọ tô) và
  kết quả phân tích vùng tóc; chưa code.
- **2026-10-01** — Chủ duyệt kế hoạch (thứ tự Pha 0 → 1 → 2; cọ tô Da + Tóc
  trước). Làm ở hội thoại mới, bắt đầu Pha 0.
- **2026-10-01** — Pha 0 code xong (tỉ lệ tối, ref từ mask da mới, tách viền
  theo màu, nhóm Nền, thu hẹp loại trừ lông mày/mắt); probe đạt; chờ chủ test.
- **2026-10-01** — Chủ test Pha 0 OK; thêm Pha 4 (Sáng tóc theo Blacks của
  Develop, làm sau); chủ cho làm Pha 2 trước Pha 1. Pha 2 code xong, chờ chủ
  test.
- **2026-10-01** — Chủ: cọ Thông minh chưa thông minh, thử Color Range. Thử 3
  cách trên ảnh thật, chốt "Color Range hai phía" (xem Pha 2); chờ chủ test.
- **2026-10-01** — Chủ test cọ Thông minh OK; báo da cổ/ngực bị cắt ngang → Pha
  2b (khung da kéo theo da model thấy); chờ chủ test.
- **2026-10-01** — Chủ test Pha 2b OK; thêm Pha 5 (cọ Thông minh cho Refine
  Selection, làm sau). Bắt đầu Pha 1.
