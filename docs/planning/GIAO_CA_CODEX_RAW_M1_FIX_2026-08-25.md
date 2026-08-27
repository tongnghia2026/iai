# Giao ca cho Claude — M1 đã nghiệm thu, tiếp tục kế hoạch RAM/chất lượng

**Ngày:** 2026-08-25
**Repo:** `C:\Users\Admin\Documents\IAI` — nhánh `feat/vector-core-foundation`
**Kế hoạch gốc (đọc trước):** `docs/planning/KE_HOACH_GIAM_RAM_MO_NHIEU_RAW_2026-08-25.md`
**Corpus test:** `C:\Users\Admin\Pictures\anh-raw` (20 RAW + 1 XMP + vài `.arp`)

> **TRẠNG THÁI CUỐI:** Owner đã GUI-test bản M1 mới và xác nhận **OK** ngày 2026-08-25. Hai hồi quy Open Image/CPU trong giao ca ban đầu đã được khắc phục. Không quay lại phương án bake cả 20 RAW hoặc eager full-decode cả filmstrip. Các mục “Bug A/B” phía cuối file chỉ còn là lịch sử để tra cứu.

## A. Trạng thái hiện tại — đã hoàn tất và nghiệm thu

### Commit cục bộ, chưa push

- **`dd4e80d` — Memory M0:** telemetry và baseline memory theo class/owner.
- **`2bdb64b` — M1 ban đầu:** single active RAW working set; bản này có hồi quy và đã được hai commit sau thay thế về hành vi.
- **`69364dc` — Fix M1:** rehydrate RAW deferred trước transition cần scene source, evict theo transition thay vì mỗi frame, chặn bake nguồn thumbnail và sửa test harness RAW look.
- **`0d7f46d` — M1 preview-first hoàn chỉnh, bản owner đã nghiệm thu:**
  - Khi mở nhiều RAW, dùng embedded JPEG preview và downscale tối đa 2048 px cho filmstrip.
  - Chỉ full-decode RAW đang active/được chọn; RAW nền giữ trạng thái deferred.
  - Tối đa một job full-decode RAW tương tác tại một thời điểm; pool decode dùng xấp xỉ nửa logical CPU.
  - Preview channel bounded capacity 2 để tránh hàng đợi bitmap làm tăng RAM.
  - Filmstrip hiển thị trạng thái Preview / queued / decoding.
  - **Open Image chỉ commit ảnh active**, rồi đóng các placeholder transient còn lại. Đây là contract UX đã được owner chấp nhận, tương tự Photoshop chỉ mở ảnh đang chọn.
  - Cancel/commit dọn preview receiver đến muộn, tránh giữ bitmap hoặc hồi sinh session đã đóng.
  - Single RAW vẫn có fallback direct-load/full-decode.

### Kết quả xác minh

- `cargo fmt --check` — xanh.
- `cargo test --locked` — xanh; lib **1440 passed, 0 failed, 6 ignored**, các integration test đều xanh.
- `cargo build --release --bin iai` — thành công.
- Portable đã copy: `dist/iAi-portable/iai.exe`.
- SHA-256 bản đã test: `336739459F6F18EC55AA0B21A040784F14694A78CC7AB7AF7CBD98318571951C`.
- GUI owner acceptance ngày 2026-08-25 — **ĐẠT**:
  - Mở filmstrip 20 RAW không còn chờ khoảng 3 phút/eager-decode cả loạt.
  - Không còn CPU 100% kéo dài khi load từng ảnh hoặc khi idle.
  - RAM không còn tăng lên khoảng 8 GB do Open Image tạo 20 raster; mục tiêu dưới 3 GB được chấp nhận.
  - Open Image mở/commit đúng ảnh đang active; không bake đồng loạt 20 ảnh.

## B. Contract M1 không được phá

1. `raw_preview_docs` là route từ path tới document deferred, **không đồng nghĩa job đang chạy**; `loading_keys` mới là nguồn sự thật cho in-flight decode.
2. Chỉ ảnh active được giữ full RAW/develop source. Chuyển filmstrip phải lưu develop settings cũ, rehydrate ảnh mới, rồi khôi phục đúng settings.
3. Multi-RAW phải preview-first; không đưa eager full-decode 20 ảnh trở lại vì sẽ tái tạo CPU/RAM/time regression.
4. Open Image chỉ commit active selection và giải phóng transient placeholders còn lại. Không khôi phục `develop_bake_all` cho toàn filmstrip nếu chưa có yêu cầu UX mới từ owner.
5. Decode RAW tương tác phải được serialize và giới hạn CPU. Không đổi sang spawn nhiều full-decode song song.
6. Late preview/decode result phải kiểm tra session/document còn hợp lệ trước khi attach.

## C. Việc Claude làm tiếp — theo kế hoạch chính

Kế hoạch gốc là nguồn sự thật. Thứ tự hiện tại sau khi M0/M1 đã hoàn tất:

1. **Hoàn tất Quality Q0 — Reference harness và baseline nhìn thấy được.** Q0 chưa hoàn thành; không tune look hoặc thay constant trước baseline.
   - Chạy/tận dụng `raw_look_probe`, `dcp_reference_probe`, `raw_corpus_probe`, `develop_cpu_gpu_parity`, `develop_color_golden`, `color_reference_probe`, `color_profile_roundtrip`.
   - Khóa ART reference commit/build/config/profile; ART chỉ là black-box oracle, tuyệt đối không copy code/asset GPL.
   - Tách ba lớp so sánh: technical-neutral, default pleasing look và slider sweep `-100/-50/0/+50/+100`.
   - Audit profile resolution/provenance theo từng camera trong corpus.
   - Lưu command, profile hash, crop 100%, contact sheet và metric theo đúng mục Q0 của kế hoạch chính.
2. **Quality Q1 — Sensor preprocessing và normalized RAW master**, chỉ bắt đầu sau khi Q0 có baseline đủ để phát hiện regression. Tách technical normalization khỏi creative look và version recipe nếu output mặc định thay đổi.
3. **Memory M2 — Loại bỏ nhân bản canvas không cần thiết**, sau Q1 theo thứ tự mục 7 của kế hoạch chính.
4. Sau đó tiếp tục đúng chuỗi: **Q2 + M4 → Q3 → Q4 → M3 → Q5/Q6 → M5 → Q7 → M6**.

### Gate trước commit tiếp theo

- Không sửa ngoài phạm vi và không ghi đè thay đổi của owner.
- `cargo fmt --check` và `cargo test --locked` phải xanh.
- Commit nhỏ, có thể bisect; chỉ commit cục bộ, **không push** cho tới khi owner yêu cầu.
- Nếu thay đổi behavior M1, phải có benchmark corpus 20 RAW và GUI acceptance mới của owner.
- Không cần chạy lại baseline 12–13 GB cũ nếu chưa thay ownership; số đó đã được M0 ghi nhận để chứng minh retention, không phải leak.

### Lệnh bắt đầu đề xuất

```powershell
git status --short
git log --oneline -6
cargo test --locked --test raw_look_probe
cargo test --locked --test dcp_reference_probe
cargo test --locked --test develop_cpu_gpu_parity
```

### Prompt bàn giao ngắn cho Claude

> Đọc toàn bộ `docs/planning/KE_HOACH_GIAM_RAM_MO_NHIEU_RAW_2026-08-25.md` và phần A–C của file giao ca này. M1 preview-first tại `0d7f46d` đã được owner GUI-test OK; không sửa lại M1 và không eager-decode/bake cả 20 RAW. Kiểm tra worktree, sau đó hoàn tất Quality Q0 bằng baseline iAi–ART tách technical-neutral/default-look/slider sweep, audit profile provenance và tận dụng toàn bộ probe hiện có. Không copy code/asset GPL từ ART, không push. Chỉ sau khi Q0 có artefact/baseline mới chuyển sang Q1, rồi M2 theo mục 7 kế hoạch chính.

## 0. Ràng buộc bắt buộc
- **CHỈ commit cục bộ, KHÔNG push** cho tới khi owner bảo (Actions free gần cạn). Xong việc = `cargo fmt --check` + `cargo test --locked` + commit + báo cáo.
- **KHÔNG copy code/asset GPL từ ART** (`C:\Users\Admin\Pictures\1111\ART`) — chỉ đọc/black-box.
- Owner là end-user, GUI-test bằng mắt. **Đừng hỏi câu kỹ thuật**; tự quyết, báo cáo kết quả + các bước test.
- Bảo toàn 3 file planning untracked trong `docs/planning/` (2 kế hoạch + file giao ca này). Không sửa việc ngoài phạm vi.
- Build+copy để owner test: `cargo build --release --bin iai` rồi `cp -f target/release/iai.exe dist/iAi-portable/iai.exe`.

## Phụ lục 1. Giao ca ban đầu trước khi sửa xong (lịch sử)
- **`dd4e80d` — Memory Milestone M0** (telemetry, KHÔNG đổi hành vi, an toàn):
  - `src/core/mem_report.rs`: `MemClass` (12 lớp), `MemReport` (by-class + by-owner, JSON), `process_working_set()` (Windows/Linux FFI trực tiếp kiểu `core/hw.rs`).
  - Accessor: `TileMap::resident_bytes` (dedup Arc), `SceneSource::resident_bytes`, `Selection::resident_bytes`, `Canvas::account_memory`/`estimated_resident_bytes`, `Document::account_memory`, `CommandHistory/HistoryGate/Canvas::…memory_bytes`.
  - Harness: `tests/raw_corpus_probe.rs::raw_corpus_memory_baseline` (ignored). **Baseline đã đo: 20 RAW = 13 362 MiB logical ≈ 13 529 MiB working-set (logical/ws = 104% → là retention, không leak); thả hết → 57.6 MiB.**
- **`2bdb64b` — Memory Milestone M1** (single active RAW working set — **CÓ HỒI QUY, xem mục 2**):
  - `Document.deferred_raw: bool` (session-only) — ảnh RAW nền đã evict xuống thumbnail.
  - `Canvas::downscaled_thumbnail(max_dim)` (area-average; bỏ tile16/develop_source/selection) + 2 unit test. PURE, đã test, an toàn.
  - `App::evict_raw_document(idx)` / `evict_background_raws()` (trong `src/app/file_ops/open.rs`): hạ ảnh RAW transient **không phải active** xuống thumbnail, drop `develop_source`, đăng ký `jobs.raw_preview_docs[path]=id` để decode sau swap về đúng doc. Guard: chỉ transient develop entry, còn `develop_source`, không active, không live preview, không đang decode. Giữ active + **1 MRU** (A/B nhanh).
  - `App::ensure_raw_resident(idx)`: re-decode off-thread → `poll_loads`→`attach_loaded_doc`→`replace_preview_with_full` (đã tự re-enter develop preview với settings đã lưu). Đã set `deferred_raw=false` trong `replace_preview_with_full`.
  - Hook: `poll_loads` (cuối) + `enter_pending_develop` (cuối) gọi `evict_background_raws`; `switch_to_doc` (cuối) + `develop_session_activate` (early-return nếu target deferred) gọi/chờ re-decode.

## Phụ lục 2. GUI-test bản cũ 21:23 (lịch sử, đã khắc phục)
1. **RAM < 3GB — ĐẠT** (mục tiêu M1 OK). ✅
2. **"Open Image" KHÔNG phản ứng** — HỒI QUY. ❌
3. **CPU chạy 100%** (mạnh/kéo dài) — nghi hồi quy. ❌

## Phụ lục 3. Bug A — “Open Image” bản cũ (đã khắc phục)
**Gốc:** `App::develop_bake_all_start_next` (`src/app/actions/develop.rs` ~dòng 280–330) là luồng "Open Image": nó **bake TẤT CẢ ảnh trong `develop_session`** (`state.pending` = mọi doc trong session). Với mỗi doc nó đọc `doc.canvas` + `canvas.develop_source` (dòng ~304, ~323–327).
**Xung đột với M1:** M1 đã evict 18/20 ảnh → chúng có `develop_source = None` và `canvas` chỉ là **thumbnail nhỏ**. Nên bake các ảnh deferred này lấy nguồn SAI (thumbnail, không có scene master) → kết quả hỏng và/hoặc state machine bake kẹt → owner thấy "không phản ứng".
**Hướng sửa (chọn 1, khuyến nghị #1):**
1. **Trước khi bake, re-hydrate tuần tự từng ảnh deferred** rồi mới bake nó, xong lại evict (đúng tinh thần M3 "batch tuần tự: decode→process→release"). Tức `develop_bake_all_start_next` khi gặp doc `deferred_raw` phải: chạy full decode đồng bộ (hoặc đưa vào queue re-decode và chờ) → có `develop_source` → bake → drop. KHÔNG bake thumbnail.
2. Hoặc **"Open Image" chỉ bake ảnh ACTIVE** (các ảnh khác giữ nguyên là RAW develop entry), nếu owner đồng ý đổi hành vi (hiện Open Image = commit cả filmstrip).
3. Tối thiểu (chống hỏng ngay): trong vòng bake, **nếu doc `deferred_raw` thì `continue`/skip** để không bake thumbnail — nhưng như vậy ảnh deferred sẽ KHÔNG được commit (mất dữ liệu người dùng mong đợi) → chỉ là chặn tạm, không phải fix đúng.

## Phụ lục 4. Bug B — CPU 100% bản cũ (đã khắc phục)
Cần phân biệt: CPU 100% **chỉ trong lúc giải mã 20 ảnh** (BÌNH THƯỜNG — 20 demosaic tuần tự) hay **kéo dài cả khi đứng yên** (BUG). Bảo owner/đo lại khi app idle sau khi mở xong.
**Giả thuyết ưu tiên nếu là bug kéo dài:**
1. **Vòng bake × evict thrash:** khi "Open Image" bake, `poll_loads` vẫn gọi `evict_background_raws` (poll_loads KHÔNG guard `develop_bake_all`). Nếu bake/`ensure_raw_resident` re-activate ảnh deferred → re-decode → evict lại → lặp. **Sửa:** guard `evict_background_raws()` để **no-op khi `self.dev.develop_bake_all.is_some()`** (và có thể khi có re-decode đang bay). `enter_pending_develop` đã early-return lúc bake nên evict ở đó đã bị bỏ; nhưng poll_loads thì chưa — thêm guard.
2. **Evict chạy mỗi frame:** `enter_pending_develop` được gọi mỗi redraw (`src/app/input/redraw.rs:48`) và mình gọi `evict_background_raws` ở cuối → chạy mỗi frame. Bản thân nó rẻ (idempotent) nhưng không nên chạy per-frame. **Sửa:** chỉ evict tại các transition điểm: sau khi một batch load xong (poll_loads khi `pending_loads` vừa rỗng) và sau một lần chuyển filmstrip — KHÔNG trong per-frame `enter_pending_develop`. Thêm cờ `jobs.needs_bg_evict` set tại transition, tiêu thụ 1 lần.
3. **Redraw storm:** kiểm tra không có đường nào `request_redraw` liên tục do M1 (evict/`ensure_raw_resident`/status_msg). `ensure_raw_resident` được guard bởi `loading_keys` nên chỉ bắn 1 lần/decode — nhưng xác nhận lại.

**Cách chẩn đoán nhanh:** thêm log tạm đếm số lần gọi `evict_background_raws` / `ensure_raw_resident` / số decode thread spawn mỗi giây; chạy release, mở 20 ảnh, để idle, xem có tăng liên tục không.

## Phụ lục 5. Kế hoạch sửa ban đầu (đã hoàn tất, không thực hiện lại)
1. **Guard evict khi đang bake** (`develop_bake_all.is_some()`) ở MỌI nơi gọi `evict_background_raws` (nhất là trong `poll_loads`). — chặn thrash.
2. **Đổi evict từ per-frame sang transition-driven** (cờ tiêu thụ-1-lần). — bỏ tải per-frame.
3. **Sửa "Open Image" bake để re-hydrate ảnh deferred trước khi bake** (mục 3, cách #1). Bake tuần tự: decode→bake→(giữ kết quả raster)→next. Sau bake xong cả session, các doc thành raster (không còn develop_source, không bị evict nữa). — fix Bug A đúng.
4. Sau khi sửa: `cargo fmt --check` + `cargo test --lib` (hiện 1436 pass) + build release + copy dist + **báo owner GUI-test lại** đúng 6 bước ở mục 7. **Đừng push.**
5. Nếu muốn hoàn thiện đúng tinh thần M1 hơn (owner sẽ thích): **lazy initial decode** — khi mở loạt RAW, chỉ full-decode ảnh active; các ảnh khác tạo placeholder thumbnail NGAY (từ embedded preview `raw_preview::extract`, đã có thread extract sẵn ở `start_load_paths` dòng ~231–238 nhưng đang bỏ bitmap) + `deferred_raw=true`, decode khi activate. Giảm cả **thời gian mở** (không decode 20 ảnh). Đây là bước lớn hơn, làm SAU khi Bug A/B xanh.

## Phụ lục 6. Bản đồ file M1
- `src/app/file_ops/open.rs`: `evict_raw_document`, `evict_background_raws`, `ensure_raw_resident`, hook trong `poll_loads`, `deferred_raw=false` trong `replace_preview_with_full`.
- `src/app/actions/develop.rs`: hook cuối `enter_pending_develop`; early-return deferred trong `develop_session_activate`; **`develop_bake_all_start_next` = nơi Bug A** (~280–330).
- `src/app/docmgr.rs`: `switch_to_doc` gọi `ensure_raw_resident`.
- `src/core/document.rs`: field `deferred_raw`.
- `src/core/canvas/mod.rs`: `downscaled_thumbnail` + `downscale_rgba` + 2 test.
- `src/app/input/redraw.rs:48`: nơi `enter_pending_develop` được gọi mỗi frame.

## Phụ lục 7. Lệnh build/test/repro cũ
```powershell
# unit test nhanh
cargo test --lib
# baseline RAM (nặng ~12GB, ~5 phút)
$env:IAI_RAW_CORPUS='C:\Users\Admin\Pictures\anh-raw'
cargo test --release --test raw_corpus_probe raw_corpus_memory_baseline -- --ignored --nocapture
# build cho owner test
cargo build --release --bin iai
Copy-Item -Force target/release/iai.exe dist/iAi-portable/iai.exe
```
**6 bước GUI owner test (sau khi sửa):** (1) mở 20 RAW → Task Manager RAM ~1.5–2GB; (2) **"Open Image" phải chạy, không treo, ảnh commit đúng look**; (3) **CPU về bình thường khi idle**; (4) chuyển ảnh filmstrip → tải lại + GIỮ tham số Develop; (5) A/B 2 ảnh nhanh; (6) Cancel đóng đúng, không crash/ảnh trắng.

## Phụ lục 8. Phương án rollback cũ (không còn cần thiết)
Nếu muốn owner có bản chạy tốt ngay: `git revert 2bdb64b` (bỏ M1, giữ M0) → RAM về 12GB nhưng "Open Image" & CPU bình thường. **Chỉ làm nếu owner cần dùng gấp**; mặc định nên SỬA (RAM<3GB đã đạt, chỉ còn 2 bug).
