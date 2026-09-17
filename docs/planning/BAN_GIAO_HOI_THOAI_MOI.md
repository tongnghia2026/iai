# Bàn giao để tiếp tục ở hội thoại mới

Cập nhật: **2026-09-15 (Asia/Bangkok)** — kiểm toán trạng thái sau đợt Codex.

## Nguồn sự thật

Trước khi sửa code, đọc toàn bộ:

1. `AGENTS.md` ở thư mục gốc.
2. `docs/planning/KE_HOACH_CANVAS_EDITOR_DOCX_2026-09-08.md` — kế hoạch chuẩn,
   **mục 0, Pha 3.6** và changelog đầy đủ.
3. File bàn giao này — checkpoint ngắn để khởi động lại nhanh.

Không dùng kế hoạch cũ `KE_HOACH_TRINH_SOAN_VAN_BAN_2026-09-01.md` để quyết định
việc tiếp theo.

## Tình trạng repo

- Nhánh `feat/vector-core-foundation`; HEAD `5917523` đi trước origin 5 commit
  (bao chữ Square, Trên-và-dưới, sửa canvas lớn) — chưa push.
- **Toàn bộ Canvas Editor/WebView, `.iai` v12, converter, sửa Crop, Esc/Enter hộp
  thoại và menu "Text" đều CHƯA COMMIT** (~33 file sửa + `src/app/document_webview.rs`,
  `src/core/canvas_editor_conversion.rs`, `web/document-editor/`).
- Trước khi commit: thêm `/.pnpm-store/` vào `.gitignore` (Pha 3.6 D4), không dùng
  `git add -A` khi chưa sửa.

## Checkpoint đã nghiệm thu (chủ dự án GUI-test)

- Pha 1 WebView2 offline, Pha 2 lifecycle tab/Close/Exit/autosave snapshot.
- Save/Save As/reopen `.iai` v12 và toolbar định dạng cơ bản (2026-09-11).
- Crop W → Tab → H → Tab → DPI giữ số; Ctrl+W đóng tab ảnh (2026-09-11).

## Kiểm tra tự động ngày 2026-09-15

- `cargo test --lib --features canvas-editor-webview` → `1660 passed; 0 failed;
  10 ignored`; fmt, diff-check, `cargo check --bin iai` không feature đạt.
- Web: `npx --no-install tsc --noEmit`, `vitest run` (12 passed), `eslint` đạt
  (pnpm không có trên PATH).
- Release có feature (kèm sửa preview Print): `C:\Users\Admin\Documents\IAI\target\canvas-editor-test\release\iai.exe`
  (`73,238,016` byte), SHA-256
  `E7526A641CF35B246DBF98F0900201BD8D071A9E1D73373E8C7C6B140AFE1686`. Bản này còn
  các lỗi Pha 3.6 — không dùng cho tài liệu văn bản thật.
- Chủ dự án đã TẠM DỪNG phần soạn thảo văn bản (2026-09-15); B3 giữ tự chuyển file cũ.
- Lưu ý: `target\release\iai.exe` ngày 2026-09-12 được build **không** bật feature
  (editor cũ) và đã dính lỗi A4, A6.

## Việc tiếp theo

1. Chờ chủ dự án chốt thứ tự Pha 3.6 và chính sách file văn bản cũ (B3).
2. Sửa nhóm A trước, mỗi lỗi có test hồi quy; sửa A1 và C2 cùng lúc.
3. Sửa B1, B4, B6 và C1–C4; build Release có feature sang `--target-dir` riêng và
   đưa đường dẫn thật cho chủ dự án test.
4. Sau khi Pha 3.6 đạt: GUI-test bảng màu và nhóm Insert, rồi nhóm Table; Pha 4 PDF
   phải xong trước khi Canvas Editor thay hẳn editor cũ (xuất PDF và Trộn thư là
   workflow hợp đồng của chủ dự án).

## Kỷ luật tránh hồi quy

- Không reset, checkout hoặc xóa thay đổi chưa commit.
- Sau mọi thay đổi code cần chủ dự án test: chạy test/check/fmt phù hợp, build sẵn
  Release có feature WebView và cung cấp đường dẫn thật theo `AGENTS.md`.
- Hai cảnh báo `f32` trong `src/ui/library.rs` và cảnh báo import test-only
  `GlyphStyle` là cảnh báo đã biết.
- Có hồi quy ở tính năng cũ → dừng tính năng mới, khóa hồi quy trước.

## Lệnh build chuẩn

```powershell
cargo build --release --features canvas-editor-webview --bin iai
```

Nếu `target\release\iai.exe` đang bị một phiên iAi khóa, không tự kết thúc tiến
trình vì có thể mất dữ liệu chưa lưu. Build sang target riêng và bàn giao đúng
đường dẫn, ví dụ:

```powershell
cargo build --release --features canvas-editor-webview --bin iai --target-dir target\canvas-editor-test
```
