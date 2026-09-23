# Kế hoạch chuẩn: Hoàn thiện Preferences + phím tắt tùy biến (iAi)

> **Tài liệu chuẩn để theo dõi triển khai.** Mọi phiên làm việc liên quan
> Preferences / phím tắt phải đọc file này trước khi sửa code và cập nhật
> checklist/changelog trước khi kết thúc phiên.

## 0. Trạng thái

- Ngày chốt kế hoạch: **2026-09-23**.
- Nhánh hiện tại: `feat/vector-core-foundation`.
- Trạng thái tổng thể: **MỚI CHỐT KẾ HOẠCH — chưa viết code.** Chủ dự án chọn
  phạm vi phím tắt "thực dụng" và yêu cầu có nút reset phím tắt về mặc định.
- Việc kế tiếp: chờ chủ duyệt tài liệu này → bắt đầu **Phase 1**.
- Không push nếu chủ dự án chưa yêu cầu. Sau mỗi phase: build Release + đưa
  đường dẫn `.exe` thật rồi mới mời chủ test (quy ước dự án).

Quy ước trạng thái checklist:

- `[ ]` chưa làm.
- `[~]` đang làm / đã code nhưng chưa qua cổng nghiệm thu.
- `[x]` hoàn thành và đã qua cổng nghiệm thu.
- `[!]` bị chặn; ghi nguyên nhân + quyết định vào Changelog.

## 1. Quyết định đã khóa

1. **Phím mở Preferences = `Ctrl+K`** (giống Photoshop). Giữ `Ctrl+,` chạy
   song song để không phá thói quen cũ. `Ctrl+K` hiện đang trống trong
   `keyboard.rs` nên không đụng phím nào khác.
2. **Phạm vi phím tắt tùy biến = "thực dụng":**
   - Cho đổi: **phím CHỌN CÔNG CỤ** (B, E, V, I, G, C, Z, H, P, A, U, T, S, J,
     O, M, L, W…) và **phím LỆNH MENU** (Ctrl+N/O/S, Ctrl+Shift+S, Ctrl+W,
     Ctrl+P, Ctrl+Z/Shift+Z, Ctrl+A/C/X/V, Ctrl+T, Ctrl+J, Ctrl+L,
     Ctrl+Shift+L, Ctrl+B, Ctrl+U, Ctrl+Shift+U, Ctrl+I, Ctrl+R, Ctrl+0/1,
     Ctrl+Shift+A (Develop), Ctrl+K/Ctrl+, (Preferences)…).
   - **CỐ ĐỊNH (chỉ đọc):** phím phụ thuộc ngữ cảnh — Enter/Esc/Space, mũi tên
     (nudge layer & selection), `[` `]` (cỡ/độ cứng brush), Delete/Backspace,
     `+`/`=` (nhân bản), Shift+F5/F6/F7, Ctrl+Y/Ctrl+Shift+Y (proof/gamut),
     Ctrl+E/Ctrl+Shift+E (merge), Ctrl+G nhóm, Ctrl+Q convert-to-curves, X/D
     swap/reset màu. Các phím này gắn với trạng thái công cụ/transform/pen/
     tool-modal, đổi tự do dễ gây hồi quy.
3. **Bắt buộc có nút "Khôi phục phím tắt mặc định"** (xem mục 4). Đây là lưới an
   toàn để người dùng lỡ đổi sai mà không biết sai ở đâu vẫn về được mặc định.
4. **Không thêm thư viện mới.** Dùng lại `serde` + `serde_json` đã có, và mở
   rộng file cấu hình sẵn có `%APPDATA%/IAI/prefs.json`
   (`src/ui/theme.rs:201`), cùng base-dir với `ai.json` (`src/core/ai/settings.rs`).
5. **Giao diện Preferences kiểu PTS:** cột danh mục bên trái + vùng nội dung bên
   phải, thay cho các `CollapsingHeader` tĩnh hiện tại.
6. **1 nguồn sự thật cho phím tắt:** nhãn menu, bảng tra cứu Help và dialog
   Preferences đều đọc chuỗi phím TỪ keymap, không gõ tay ở 4 chỗ như hiện nay.
7. **Tự phục hồi:** `keymap`/`prefs` hỏng hoặc thiếu field → tự lùi về mặc định,
   không làm treo phím tắt. Mỗi field dùng `#[serde(default)]`.
8. Không đổi hành vi mặc định của bất kỳ phím nào: keymap mặc định = đúng
   bindings hôm nay. Người dùng không mở Preferences thì trải nghiệm y hệt.

## 2. Mục tiêu và phạm vi

### 2.1 Mục tiêu

- Preferences từ "vỏ" (nhãn tĩnh, "coming soon", số liệu sai) thành bảng cài đặt
  **thật**: đổi là lưu, mở lại vẫn còn, áp dụng ngay.
- Mở bằng `Ctrl+K`.
- Người dùng đổi được phím chọn công cụ + phím lệnh menu theo ý, có phát hiện
  trùng và **reset về mặc định**.

### 2.2 Ngoài phạm vi (đợt này)

- Đổi phím ngữ cảnh (mục 1.2 phần CỐ ĐỊNH).
- Theme sáng (app đang dark-only theo thiết kế — `src/ui/theme.rs`).
- Đồng bộ cài đặt qua cloud, nhiều profile workspace.

## 3. Hiện trạng code (đã kiểm tra 2026-09-23)

- `preferences_dialog` toàn nhãn tĩnh + danh sách phím CHỈ ĐỌC:
  `src/ui/dialogs/session.rs:10`. "Undo history: 100 steps" là **sai** — undo
  tính theo ngân sách RAM ở `src/core/hw.rs:95`.
- Router dialog: `src/ui/dialogs.rs:188`.
- Menu File có Preferences (2 chỗ, cả bản menu thu gọn): `src/ui/menubar.rs:193`
  và `:348`; helper nhãn phím `menu_item` ở `:1672`; bảng tra cứu Help
  `keyboard_shortcuts_list` ở `:1694`.
- Phím tắt hard-code trong `match` khổng lồ: `src/app/input/keyboard.rs` (dùng
  `KeyCode` + cờ `ctrl_held/shift_held/alt_held`, nhiều chốt ngữ cảnh ở đầu hàm).
- Hạ tầng lưu cấu hình mẫu: `src/ui/theme.rs:201-247` (`UiPrefs`, `prefs_path`,
  load/save), `src/core/ai/settings.rs` (`AiSettings`).
- Cài đặt "thật" đang cứng/không persist: autosave 90s `src/app/autosave.rs:15`;
  `snap_enabled` mặc định tắt `src/app/state.rs:471`; chưa có UI scale.
- Luồng UI dialog: đọc `UiData` (`src/ui/viewmodel.rs`) → ghi `UiActions`
  (`src/ui/intent.rs`) → áp dụng ở `src/app/actions/ui_*.rs`
  (vd `ui_dialogs.rs:491`, `ui_chrome.rs`). Cài đặt mới phải nối đủ 3 lớp này.

## 4. Yêu cầu NÚT RESET (chủ dự án nhấn mạnh)

- **Reset toàn bộ phím tắt:** nút "Khôi phục phím tắt mặc định" luôn hiện ở đáy
  trang Phím tắt. 1 lần bấm → đưa TOÀN BỘ keymap về mặc định gốc. Có hộp xác
  nhận ngắn (tránh bấm nhầm) + thông báo trạng thái sau khi reset.
- **Reset từng dòng:** mỗi dòng phím tắt có nút hoàn tác nhỏ → chỉ dòng đó về
  mặc định. Dòng nào đang khác mặc định thì đánh dấu (đậm/nhãn "đã đổi").
- **Reset toàn bộ Preferences:** nút "Khôi phục cài đặt mặc định" (mọi mục, có
  xác nhận) — mạng lưới an toàn cấp cao nhất.
- **Tự phục hồi khi file hỏng:** đọc keymap lỗi/không parse được → tự dùng mặc
  định thay vì mất phím tắt (không cần người dùng biết file ở đâu).
- (Tùy chọn) hiện đường dẫn file cấu hình + nút mở thư mục để hỗ trợ khi cần.

## 5. Kiến trúc đề xuất

### 5.1 `AppSettings` — nguồn sự thật cho cài đặt

- File mới `src/core/settings.rs` (hoặc mở rộng `UiPrefs` trong `theme.rs`), một
  struct `AppSettings` serde, lưu vào `prefs.json`, giữ tương thích `theme_mode`.
- Nhóm field dự kiến (mỗi field `#[serde(default)]`):
  - general: `default_unit` (dùng `core::units::Unit`), `ui_scale: f32`.
  - files: `autosave_enabled: bool`, `autosave_interval_secs: u32`.
  - tools: `snap_default: bool`.
  - ai: `ai_use_gpu: bool` (khớp DirectML Select Subject/Smart Fill).
  - `keymap: KeyMap`.
- Load 1 lần lúc khởi động (chỗ đang gọi `load_theme_mode`, `state.rs:1671`),
  giữ trên `App`; save best-effort khi đổi (đúng khuôn `save_theme_mode`).

### 5.2 Engine keymap

- `src/app/commands.rs` (mới): enum `Command` liệt kê mọi hành động gán được,
  mỗi biến thể có **id chuỗi ổn định** (lưu JSON), tên hiển thị, nhóm, và chord
  mặc định.
- `KeyChord { ctrl, shift, alt, key }` + parse/format `"Ctrl+Shift+K"`.
- `KeyMap` = map `Command → KeyChord`, có `default()` = đúng bindings hôm nay;
  serialize dạng `{ "tool.brush": "B", "file.save": "Ctrl+S", ... }`.
- Dispatcher `App::run_command(cmd, event_loop)`: chứa **thân xử lý** hiện nằm
  trong `keyboard.rs` (tách ra để tập trung hành vi).
- `keyboard.rs` viết lại: GIỮ nguyên các chốt ngữ cảnh đầu hàm (flow-text,
  welcome, library Ctrl+A, blocking modal, preview-dialog allow-list, tool-modal
  allow-list, quyền Ctrl+Z của transform/warp/pen) và các phím CỐ ĐỊNH; phần còn
  lại → dựng chord từ event + cờ modifier → tra keymap → `run_command`.

### 5.3 UI Preferences

- Viết lại `preferences_dialog` (`session.rs`): cột danh mục trái + nội dung
  phải. Danh mục: **Tổng quát · Giao diện · Hiệu năng · Tệp & Tự lưu · Công cụ &
  Con trỏ · AI · Phím tắt**.
- Trang Phím tắt: ô tìm kiếm + bảng gom nhóm; mỗi dòng: tên lệnh · chord hiện
  tại · nút "sửa" (bắt tổ hợp phím kế tiếp) · nút reset dòng. Trùng phím →
  tô cảnh báo + chặn/nhắc. Phím CỐ ĐỊNH hiện chỉ đọc, nhãn "cố định".
- Nhãn menu (`menu_item`) + bảng Help (`keyboard_shortcuts_list`) đọc chord từ
  keymap.

## 6. Các giai đoạn

### Phase 1 — Nền cài đặt + khung UI kiểu PTS + Ctrl+K ✅ **XONG (chủ test OK 23/09)**

- [x] `AppSettings` + mở rộng `prefs.json` (load/save merge, `#[serde(default)]`,
      `sanitize()` kẹp khoảng; unit test xanh). File `src/core/settings.rs`.
- [x] Mở Preferences bằng `Ctrl+K` (giữ `Ctrl+,`) — nối cả đường raw
      `keyboard.rs` lẫn `consume_shortcut` trong `ui/mod.rs`; nhãn menu 2 chỗ đổi
      sang `Ctrl+K`.
- [x] Viết lại dialog thành cột danh mục trái (7 mục) + nội dung phải
      (`src/ui/dialogs/session.rs`). Cửa sổ kéo được + giới hạn chiều cao theo màn
      hình + nút OK/Hoàn tác (baseline egui temp). LƯU Ý: KHÔNG dùng `ui.separator()`
      dọc trong layout ngang — nó giãn hết chiều cao, đẩy cửa sổ tràn màn hình.
- [x] Nối cài đặt thật rủi ro thấp, lưu + áp dụng ngay: ~~UI scale~~ (CHỦ YÊU CẦU
      BỎ sau test lần 1 — đã gỡ hoàn toàn), đơn vị mặc định (thước + New/Open/PDF),
      bật/tắt + chu kỳ autosave, snap mặc định (persist + áp dụng phiên), GPU cho
      AI (`ort_ep::set_ai_use_gpu`).
- [x] Thay nhãn giả bằng số liệu thật (ngân sách undo từ `hw::history_budget_bytes`,
      tên/loại/back-end GPU từ `hw::gpu`, theme "Tối"). Bỏ mọi "coming soon".
- **Cổng nghiệm thu:** ✅ chủ test OK 23/09 (đóng/mở giữ cài đặt, Ctrl+K đúng, cửa
  sổ nằm gọn màn hình, không hồi quy phím cũ). Build Release OK. **Commit local
  (chưa push).**

### Phase 2 — Engine keymap (KHÔNG đổi hành vi)

- [x] `Command` + `KeyChord`/`KeyName` + `KeyMap` (default) — `src/app/commands.rs`.
      Bảng TABLE là nguồn sự thật duy nhất; 5 unit test xanh (không trùng chord,
      id ổn định, round-trip). Serde-ready cho Phase 3.
- [x] Bảng Help (`menubar.rs::keyboard_shortcuts_list`) + trang Phím tắt trong
      Preferences (`session.rs`) đọc chord/tên từ engine. Sửa luôn nhãn Help cũ
      "Ctrl+," → "Ctrl+K". (Nhãn từng menu item trong menu bar để Phase 3 nối
      keymap ĐỘNG cùng lúc bật rebinding, tránh sửa 2 lần.)
- [~] **Định tuyến DISPATCH + `run_command`: CỐ Ý HOÃN sang Phase 3.** Lý do:
      viết lại `keyboard.rs` (1200 dòng, đầy chốt ngữ cảnh) là thay đổi RỦI RO
      hồi quy nhưng KHÔNG có lợi ích nhìn thấy được ở Phase 2 (hành vi phải giống
      hệt). Làm chung với trình sửa phím tắt Phase 3 thì mới test được đầu-cuối
      (đổi phím → thấy hiệu lực). Theo luật của chủ: "làm fix an toàn, hỏi trước
      khi đổi rủi ro".
- **Cổng nghiệm thu Phase 2:** KHÔNG đụng dispatch nên toàn bộ phím **giống hệt**
  trước (bảo đảm theo cấu trúc); `cargo test --lib` xanh (1654); Build Release OK.
  → **CHỜ CHỦ TEST + quyết định có làm tiếp dispatch/editor ở Phase 3.**

### Phase 3 — Trình sửa phím tắt + RESET

- [ ] Bảng phím tắt: tìm kiếm, gom nhóm, bắt phím, phát hiện trùng.
- [ ] **Nút "Khôi phục phím tắt mặc định"** (reset toàn bộ, có xác nhận).
- [ ] **Reset từng dòng** + đánh dấu dòng đã đổi.
- [ ] Lưu keymap vào `prefs.json`; tự phục hồi khi hỏng.
- [ ] (Khuyến nghị) nút "Khôi phục toàn bộ cài đặt mặc định".
- **Cổng nghiệm thu:** đổi 1 phím tool + 1 phím lệnh → có hiệu lực + lưu; tạo
  trùng → cảnh báo; bấm reset → về mặc định; làm hỏng file → app tự lùi mặc
  định. Build Release OK.

### Phase 4 (tùy chọn, sau khi chủ duyệt)

- [ ] Import/Export keymap; thêm tùy chọn con trỏ chuột; các cài đặt khác.

## 7. Rủi ro & giảm thiểu

- **Hồi quy phím tắt** khi tách `keyboard.rs`: giữ nguyên chốt ngữ cảnh, làm
  Phase 2 "no-behaviour-change", test tay theo danh sách trước khi mở rebinding.
- **Người dùng tự khóa mình** bằng phím trùng/sai: nút reset toàn bộ + reset
  dòng + tự phục hồi file hỏng (mục 4).
- **UI scale / autosave sai giá trị:** kẹp khoảng hợp lệ (vd scale 0.8–2.0,
  autosave 30–600s).

## 8. Danh sách file dự kiến chạm

- Mới: `src/core/settings.rs` (hoặc mở rộng `UiPrefs`), `src/app/commands.rs`.
- Sửa: `src/ui/dialogs/session.rs`, `src/ui/dialogs.rs`,
  `src/app/input/keyboard.rs`, `src/ui/menubar.rs`, `src/ui/theme.rs`,
  `src/app/state.rs`, `src/app/autosave.rs`, và lớp plumbing
  `src/ui/viewmodel.rs` · `src/ui/intent.rs` · `src/app/actions/ui_*.rs`.

## 9. Changelog

- 2026-09-23: Lập kế hoạch. Chủ chốt phạm vi phím tắt "thực dụng" + yêu cầu nút
  reset phím tắt về mặc định. Chưa viết code.
- 2026-09-23 (bản sửa sau test lần 1): Chủ test bản đầu → **BỎ hẳn "Tỉ lệ giao diện"**
  (gỡ field `ui_scale` + hoàn nguyên mọi hiệu chỉnh ppp ở render/hit-test/fit/pan/warp,
  các file này trở lại y nguyên bản gốc). Sửa cửa sổ Preferences: **di chuyển được**
  (bỏ anchor, dùng pivot + default_pos để canh giữa lần đầu), **giới hạn chiều cao**
  theo màn hình (ScrollArea `auto_shrink([false,true])` + `max_height` co theo screen)
  nên không tràn khỏi màn hình, **thêm nút OK + Hoàn tác** luôn hiện ở đáy (Hoàn tác/Esc
  khôi phục về mốc lúc mở). Trang Phím tắt ghi rõ "chỉ để xem, đổi phím tắt ở bước kế
  tiếp" (đó là Phase 3). Còn 4 cài đặt thật: đơn vị mặc định, autosave, snap, AI GPU.
- 2026-09-23: **Phase 1 code xong (chờ chủ test).** Thêm `core::settings::AppSettings`
  (prefs.json merge-write, serde default + sanitize, 4 unit test xanh). Preferences
  mở bằng `Ctrl+K`/`Ctrl+,`; dialog dựng lại kiểu PTS (7 mục). Nối 5 cài đặt thật
  áp dụng ngay + persist: UI scale, đơn vị mặc định, autosave (bật/tắt + chu kỳ),
  snap mặc định, GPU cho AI. Số liệu thật thay nhãn giả (undo budget, GPU, theme).
  UI scale làm qua egui `zoom_factor`, đồng thời hiệu chỉnh ppp ở render + ui_chrome_hit
  + fit/constrain_pan + warp anchor để không lệch canvas/panel; **no-op tuyệt đối khi
  scale = 1.0** (mặc định) nên không đụng trải nghiệm cũ. Build Release OK; `cargo fmt`
  + `cargo test --lib settings` xanh. **Chưa push** (chờ chủ duyệt). Phase 2/3 chưa làm.
