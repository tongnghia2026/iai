# Kế hoạch ARTBOARD / ĐA TRANG THẬT cho iAi — 2026-08-22

> # ✅ ĐÃ HOÀN TẤT 100% — KHÔNG CÒN VIỆC PHẢI LÀM TRONG FILE NÀY
> **Cập nhật 2026-08-25:** Toàn bộ kế hoạch này đã được **code xong, nối UI, có test, owner GUI-test OK và ĐÃ PUSH** lên origin.
> - Artboard các lát **A→H + Master Pages (I)**: XONG.
> - **8 gap #1–#8** (bông cắt vào Export PDF, spot plate, gradient CMYK shading, SVG z-order, PDF ExtGState opacity, PowerClip Fit/Center/Fill/Stretch, PowerClip Edit-Contents, connector động): XONG — "KHÔNG CÒN GAP".
> - Mục #10 (Develop 'Profile' picker) là ghi chú NGOÀI phạm vi vector, không thuộc kế hoạch này.
>
> ⚠️ **Đây là tài liệu lịch sử/thiết kế — giữ để tham khảo, KHÔNG phải backlog.** Đừng coi các mục dưới là việc cần làm; chúng đã nằm trong code rồi.
>
> ---

> Nguồn: rà soát 4-chiều trên code thật (workflow `vector-artboard-audit`, 7 agent, 0 lỗi) +
> tổng hợp kiến trúc + một vòng phản biện (adversarial critique). Bám đúng
> `docs/FOUNDATION_FREEZE.md` + `docs/ADR_PAGE_OWNERSHIP.md` — MỌI thay đổi là ADDITIVE.

---

## 1. Kết luận rà soát: "chỉ còn Artboard?" — GẦN ĐÚNG

- ✅ **Mọi tính năng vector bạn coi là XONG đều thật sự hoàn chỉnh + đã nối vào menu/UI + có test.**
  Đã trace tận code: batch style text & vector, Boolean/Shaping (Weld/Trim/Intersect/Exclude),
  PowerClip đặt/tách, Clipping mask, PDF vector CMYK, overprint, bleed/trim marks (trong hộp thoại In),
  spot color, SVG export — **không có tính năng nào code xong mà bị bỏ quên không nối UI.**
- ✅ **Artboard/đa trang đúng là mảng lớn nhất còn thiếu.** Khe "additive" mới có ở tầng LÁ
  (`PageId` + kiểu `Page` + `Layer.page_id` + tag `"page"` trong `.iai`); còn **cái thùng chứa**
  mà ADR chừa sẵn — `Document.pages`, envelope trong `.iai`, render/label từng trang, vòng đời trang,
  xuất từng trang, giao diện Artboard — **chưa dựng gì.** `core/page.rs` hiện là code chết
  (chỉ dùng trong test của chính nó).
- ⚠️ **NHƯNG còn vài chỗ thiếu KHÁC ngoài Artboard** — vài cái đụng thẳng nghề in. Xem mục 2.

---

## 2. Việc-làm-SAU (gap backlog) — xếp theo ưu tiên

### Nhóm IN ẤN (giá trị cao — nên làm sớm, phần lớn gọn)
| # | Việc | Vì sao quan trọng | Công |
|---|------|-------------------|------|
| 1 | **Đưa bông cắt + bleed/trim box vào File ▸ Export PDF** (cả 1 trang lẫn nhiều trang) | Hiện CHỈ hộp thoại *In* mới ra PDF chuẩn nhà in. `PdfExporter::export` và `build_pdf_multipage_encoded` đều `PrintLayout::default()` cứng ⇒ **PDF xuất ra không có bông cắt / không bleed** → gửi RIP là mất. | vừa |
| 2 | **Tấm phim spot trong Export Separations** | `export_cmyk_separations` chỉ ghi 4 bản C/M/Y/K; thiết kế có mực spot **không có tấm spot riêng** (spot bị quy về CMYK gần đúng). Máy spot đã có sẵn cho đường PDF-vector, chỉ thiếu ở đây. | nhỏ |
| 3 | **Gradient DeviceCMYK shading trong PDF CMYK** | Trang CMYK: gradient bị nướng vào raster thay vì ghi shading vector ⇒ gradient trên job CMYK kém nét/không scale. | vừa |
| 4 | **SVG giữ đúng thứ tự lớp nhiều tầng** | `build_svg` chỉ giữ vector tầng trên-cùng; vector nằm dưới ảnh bị nướng vào PNG (mất nét/mất z-order). | vừa |
| 5 | **PDF ExtGState cho fill/stroke mờ (opacity)** | Object vector có độ mờ chưa được gắn transparency state ở đường PDF native ⇒ có thể xuất sai. Ca hiếm. | vừa (ưu tiên thấp) |

### Nhóm VECTOR nâng cao (trung bình)
| # | Việc | Vì sao | Công |
|---|------|--------|------|
| 6 | **PowerClip: Center / Fit / Fill / Stretch** | Hiện chỉ đưa-vào/tách khung, **không có** lệnh tự canh giữa / vừa khung — workflow PowerClip cốt lõi kiểu Corel. | vừa |
| 7 | **PowerClip: sửa nội dung tại chỗ (Edit Contents) + khoá nội dung** | Đặt vào khung rồi chỉ có Tách/di-chuyển-cả-khung; không sửa được nội dung bên trong. | vừa |
| 8 | **Connector động (dính vào hình + tự đổi đường khi kéo hình)** | Connector hiện bị nướng thành Path tĩnh, **không dính** vào ô; kéo ô là phải vẽ lại. Sơ đồ/tổ chức cần cái này. | lớn |

### Để RẤT sau
| # | Việc | Ghi chú |
|---|------|---------|
| 9 | **Master Pages** | Layout dùng chung (header/footer/nền) kế thừa qua các trang. Thuộc mảng Artboard, là lát cuối cùng (I). |
| 10 | *(ngoài vector)* **Develop 'Profile' picker** | Dropdown profile trong Develop đang là stub 1 look cứng. Không liên quan vector — ghi cho đủ. |

---

## 3. Kế hoạch ARTBOARD — mô hình + tác động nền

### Quyết định mô hình (rẻ nhất, additive kiểu gì cũng an toàn)
**Giữ NGUYÊN một `Canvas`/`layer_stack` dùng chung. Thêm hình học từng trang qua `Document.pages`.
Lọc/cắt layer theo `Layer.page_id`.** → tôn trọng đúng contract đã chừa, **không đụng** hình dạng
"một-stack" đã băng của `Canvas`.

### Tác động nền (KHÔNG cần ADR kiến trúc mới — ADR_PAGE_OWNERSHIP đã chừa 4 khe này)
Chỉ cần **3 ghi chú ngắn (addendum, không phải contract mới):**
1. **Ghi chú MIGRATION `.iai`**: bump version; luật *"thiếu envelope ⇒ suy ra 1 trang ngầm từ width/height/dpi"*.
   **Dùng key MỚI tên `"artboards"`** — TRÁNH đụng key `"pages"` đã có của tính năng import-PDF-nhiều-trang
   (2 khái niệm "trang" khác nhau!). Doc 1 trang ngầm = **KHÔNG ghi block `artboards`** ⇒ file cũ round-trip byte-đúng.
2. **ADR addendum khi làm Master Pages** (thêm field `master_ref` vào `Page` + luật override).
3. **Ghi chú 1 dòng** chốt mô hình "shared layer_stack + page mang geometry + cắt theo page_id" để
   tương lai không ai đi làm mỗi Page một Canvas riêng (cái đó MỚI đụng contract đã băng).

> ⚠️ Lưu ý phản biện: hôm nay *page-space == canvas-space* (đảm bảo MVP của ADR). Đa-artboard làm
> *canvas-space = cả workspace*, page-0 ≠ canvas-space. Đổi này **ý nghĩa** contract #4 (hệ toạ độ) &
> #10 (toạ độ theo trang) dù **kiểu dữ liệu không đổi** → nên viết addendum đàng hoàng, đừng coi là "1 dòng".

### 8 lát chính (A→H) + 1 lát tuỳ chọn (I) — mỗi lát chạy được & GUI-test được
| Lát | Kết quả người dùng thấy | Rủi ro | File chính |
|-----|-------------------------|--------|-----------|
| **A. Document có danh sách trang (nền ẩn)** | Không đổi gì nhìn thấy; bên trong doc có 1 trang = canvas hiện tại. App vẫn chạy y hệt. | Thấp-TB | `core/document.rs`, `core/page.rs`, `core/layer.rs` |
| **B. `.iai` nhớ được các trang (tương thích ngược)** | Lưu/mở giữ nguyên artboard; **file cũ mở vẫn hoàn hảo**. | TB (định dạng) | `formats/iai.rs` |
| **C. Thấy trang như artboard thật (giấy + bleed + lề)** | 1 trang vẽ thành khung giấy có màu nền + viền bleed + đường lề an toàn. Vẫn 1 trang. | TB (chỉ render) | `ui/mod.rs`, `app/render/composite.rs`, `app/render/view.rs` |
| **D. Nhiều artboard cạnh nhau trên workspace cuộn tự do** | Canvas thành mặt bàn cuộn/zoom, hiện nhiều artboard; lệnh tạm "Thêm Artboard" để thả cái thứ 2 + "Fit tất cả". | **Lớn** | `app/render/view.rs`, `ui/mod.rs`, `app/render/composite.rs`, `core/document.rs` |
| **E. Bảng Artboard: thêm/đặt tên/sắp xếp/xoá** | Panel liệt kê từng artboard có thumbnail, quản lý cả job nhiều trang. | TB | `ui/panels.rs`, `ui/mod.rs`, `app/actions/ui_layers.rs` |
| **F. Kéo/đổi cỡ/sắp lại artboard ngay trên canvas** | Click chọn artboard, kéo dời, kéo góc đổi cỡ; nội dung đi theo. | **Lớn** | `tools/crop.rs`, `ui/mod.rs`, `ui/toolbar.rs`, `app/actions/ui_layers.rs` |
| **G. Menu riêng + "Thêm Artboard" có preset cỡ** | Menu Trang/Artboard gom mọi lệnh; "Thêm Artboard" mở hộp cỡ dùng lại preset A4/màn hình như New Canvas. | Nhỏ-TB | `ui/menubar.rs`, `ui/dialogs/document.rs`, `app/actions/ui_dialogs.rs` |
| **H. Xuất mỗi artboard = 1 trang PDF / 1 SVG, CÓ bông cắt** | Xuất ra PDF mỗi artboard 1 trang đúng cỡ + bleed + bông cắt; xuất tất cả hoặc chỉ cái đang chọn. **Đồng thời khép luôn gap #1.** | TB-Lớn | `core/print.rs`, `formats/pdf.rs`, `core/svg.rs`, `app/file_ops/save_export.rs`, `ui/dialogs/document.rs` |
| **I. (tuỳ chọn) Master Pages** | Artboard kế thừa nền/header/footer chung; sửa 1 lần áp mọi trang. | Lớn (nhưng additive) | `core/page.rs`, `core/document.rs`, `formats/iai.rs`, `ui/panels.rs`, `app/render/composite.rs` |

---

## 4. BẪY KỸ THUẬT — phải thiết kế từ ĐẦU (từ vòng phản biện)

Những điểm này KHÔNG phải "làm sau" — nếu bỏ qua sẽ đẻ bug khó gỡ:

1. **UNDO cho vòng đời trang (rủi ro bug cao nhất).** Stack undo (`cmd_history: HistoryGate`) nằm
   **BÊN TRONG `Canvas`** (canvas/mod.rs), niêm phong để history + checkpoint-đã-lưu + cờ dirty không lệch.
   `Document.pages` là field MỚI **NGOÀI** Canvas ⇒ thêm/xoá/đổi-tên/kéo/đổi-cỡ artboard (lát A/E/F) sẽ
   **đi vòng qua cổng** → không undo được / lệch cờ dirty. **Phải định nghĩa đường "page-command" ghi qua
   đúng gateway** ngay từ lát A.
2. **DIRECT PRINT từng artboard.** Lát H mới lo PDF/SVG; hộp thoại *In* / `print_gdi.rs` vẫn in mỗi Canvas.
   Nhà in cần "in tất cả artboard / in cái đang chọn", mỗi cái đúng cỡ + bông cắt. Thêm 1 lát in hoặc gộp vào H.
3. **Giới hạn Boolean / PowerClip / Clip-mask / Align trong MỘT artboard.** Vì dùng chung 1 layer_stack
   chỉ phân biệt bằng `page_id`, không có gì ngăn một phép Boolean/Align **trộn object giữa 2 artboard**.
   Phải thêm luật lọc theo `page_id` cho các phép này.
4. **Tương thích XUÔI (build CŨ mở file MỚI).** Lát B lo ngược (mới đọc cũ). Nhưng build cũ mở file
   đa-artboard sẽ **lờ envelope mới và khi lưu lại làm MẤT** toàn bộ hình học trang. Cần guard version /
   cảnh báo để build cũ không âm thầm dập phẳng job nhiều trang.
5. **Đụng độ với PDF-navigator có sẵn.** Một Document có thể ĐANG là PDF-import nhiều trang, xem bằng
   "◀ Trang k/N ▶" (`edited_pages`, `export_multipage_pdf` duyệt các TAB). Navigator đổi nội dung canvas
   theo từng trang, còn artboard muốn hiện tất cả cùng lúc — **2 khái niệm "trang" xung đột ở tầng UX/dữ liệu,
   không chỉ ở tên key.** Phải định nghĩa: doc PDF-import có được có artboard không? "Thêm Artboard" nghĩa gì ở đó?
6. **Pixel của artboard lệch gốc nằm ở đâu?** `Canvas` là MỘT buffer RGBA cố định w×h + 1 atlas GPU theo đúng
   rect đó. Mô hình "1 Canvas dùng chung" ⇒ hoặc phải nới `Canvas.width/height` ra cả bounding-box workspace
   (buffer/atlas to, phần lớn rỗng), hoặc cần mapping mới. **Phải chốt trước lát D.** Nhớ: **mọi đổi-cỡ-canvas
   PHẢI đi qua `apply_canvas_event`** (bug crop-lần-2 cũ).
7. **Test round-trip `.iai` đa-artboard NGAY trong lát B.** Cách tạo artboard thứ 2 bằng GUI phải tới lát D
   mới có ⇒ lát B không GUI-verify được lúc ra mắt. **Bắt buộc test code-level lưu/mở đa-trang** để không giấu
   bug định dạng tới tận lát D.
8. **Sửa mô tả lát H cho đúng code.** `build_pdf_multipage_encoded` (print.rs:~1802) **hiện KHÔNG nhận
   tham số PrintLayout nào** — nó `PrintLayout::default()` cứng bên trong (~:1810). Nên đây là **thêm tham số
   (đổi signature)**, không phải "đổi default truyền vào". Ngoài ra `PdfExporter::export` (pdf.rs:~1004/1020/1040)
   là **chỗ hardcode RIÊNG** cho File▸Export PDF 1-trang, lát H cũng phải sửa. Lưu ý `build_pdf_multipage_encoded`
   dùng chung với tính năng export PDF-import ⇒ đổi signature không được làm hồi quy đường đó.

---

## 5. Đề xuất thứ tự triển khai
- **Trước tiên (nhanh, giá trị in ấn tức thì, độc lập với Artboard):** gap #1 (bông cắt vào File▸Export) +
  #2 (spot plate). Có thể làm ngay, không phải chờ Artboard.
- **Rồi Artboard theo A→H** (I để cuối, tuỳ chọn), thiết kế sẵn 8 bẫy ở mục 4 từ lát A.
- Các gap #3–#8 xen vào khi thuận (ví dụ #3 gradient CMYK đi kèm lát H).

*(Kế hoạch tổng gốc: `docs/planning/KE_HOACH_PHAT_TRIEN_VECTOR_IAI.txt` — Phase 9 xuất-in đã xong; Artboard là group C.)*
