# Bàn giao cho Codex — Hybrid Vector Canvas (2026-07-31)

Bản giao ca để **Codex code tiếp** track "canvas hybrid vector–raster GPU" của iAi.
Đọc file này + `KE_HOACH_CANVAS_HYBRID_VECTOR_RASTER_2026-07-30.md` (nguồn yêu cầu
chính) rồi tiếp tục. Đây là bản hiện trạng trên filesystem — tin nó, không dựa vào
trí nhớ hội thoại cũ.

## 1. Đang ở đâu

Branch `feat/vector-core-foundation`. **12 commit local CHƯA PUSH.** 4 commit trên
cùng là track này:

| commit | nội dung |
|---|---|
| `a9c8e9e` | Phase 1 — GPU vector renderer (Lyon tessellation + wgpu MSAA4×→resolve), verify khớp `core::vector::raster` |
| `e7cd7d8` | Phase 2 — nối vào compositor sau flag `IAI_GPU_VECTOR_CANVAS` (mặc định TẮT = raster y hệt) |
| `3ff335a` | Phase 3 (cache GPU buffer, transform chỉ-uniform) + Phase 6 (primitive Shape) |
| `d0628f0` | Phase 4 partial (nét đặc bất kỳ cap/join → GPU) + Phase 8 partial (bỏ bake-nét-CPU trùng lặp cho layer GPU) |

Trạng thái test: **1014 test thư viện + 6 GPU snapshot test PASS, `cargo fmt` sạch.**

GUI-test: Phase 1/2 user đã test OK. **Phase 3/6/4/8-partial ĐANG CHỜ user GUI-test**
(user vừa test bản `d0628f0` để xem bông hoa Repeat hết bị "quét làm nét lại từng
layer" khi zoom). **Đừng coi các phase này là đã chốt cho tới khi user xác nhận.**

## 2. Kiến trúc hiện có (đọc trước khi sửa)

- `src/gpu/vector/` — toàn bộ track:
  - `renderer.rs` — pipeline vẽ; `GpuMesh` (buffer đã upload + fill/stroke range),
    `VectorDraw`, `CanvasView`, `encode_run`, `render_offscreen` (POC readback).
    Màu truyền vào shader dưới dạng **linear** (target sRGB re-encode ra đúng byte
    CPU). MSAA output **premultiplied**; accumulator compositor là **straight
    alpha** → un-premult trong `vector_composite.wgsl`.
  - `mesh.rs` — `tessellate(object, tol)` qua Lyon. **Nét LUÔN tessellate tròn**
    (round cap/join) để khớp `raster::stroke_coverage` (CPU luôn vẽ viên nang tròn,
    bỏ qua cap/join). KHÔNG đổi lại thành honor cap/join trừ khi đồng thời nâng CPU
    reference.
  - `eligibility.rs` — quyết định GPU hay raster fallback. Hiện GPU-eligible =
    solid RGB fill/stroke + không dash + không brush + geometry hợp lệ (cap/join
    KHÔNG còn chặn). Fallback: gradient, CMYK, dash, opacity≠1, mask, group,
    powerclip, blend≠Normal, brush, primitive-invalid.
  - `composite.rs` — `VectorCompositeStage`: vẽ 1 run eligible vào MSAA→resolve→
    composite vào ping/pong. `GpuMeshCache` (ByteLru 96 MiB, key = (fingerprint,
    zoom bucket)) → pan/zoom-trong-bucket/move/xoay/scale KHÔNG re-tessellate/upload.
  - `cache.rs` — `geometry_fingerprint`, `ByteLru<K>` (byte-budget LRU, unit-test
    không cần GPU), `MeshCache` (legacy, chỉ test dùng).
  - `scene.rs` — `plan_runs` (gom layer thành RasterRun/GpuVectorRun theo z-order).
  - `telemetry.rs` — debug counters (mesh tessellations/uploads/evictions/bytes).
  - `vector.wgsl` (draw), `vector_composite.wgsl` (blend run vào accumulator).
- `src/gpu/compositor.rs` — sở hữu `vector_stage: Option<VectorCompositeStage>`
  (chỉ `Some` khi flag on). `composite_layers`:
  - `can_gpu_vector = vector_stage.is_some() && !canvas_space && render_scale==1 &&
    crop_preview.is_none()`.
  - `gpu_eligible[i] = stack_idx != active_idx && !transform_previews.any(id) &&
    layer_eligibility(l,true)==GpuVector`.
  - **Twin-suppression**: layer GPU thì BỎ tile pass của nó (1 representation).
  - Run present → ép full composite (bỏ partial + backdrop cache).
  - `self.gpu_drawn_layer_ids: Vec<u32>` — ghi layer nào GPU vẽ frame này (clear khi
    GPU tắt). **`path_display` đọc field này** để bỏ bake nét CPU trùng.
- `src/app/path_display.rs` — bake nét CPU (supersample). `active_vector_display_inner`
  giờ DỪNG run overlay tại layer GPU đầu tiên (top-down) qua `gpu_drawn_layer_ids`
  (chỉ khi `runtime_enabled()`), giữ z-order.

## 3. GIỚI HẠN CÒN LẠI CẦN XỬ (ưu tiên user vừa gặp)

**Active layer nằm DƯỚI layer GPU → hơi mờ (doc-res) khi zoom** cho tới khi bỏ chọn:
overlay nó lên trên sẽ sai z-order nên `path_display` phải dừng tại layer GPU đầu.
Cách xử triệt để = **Phase 8 đầy đủ: cho LỚP ACTIVE cũng đi GPU** khi không đang sửa
(feed pending geometry của node/shape/move drag vào GPU thay vì luôn fallback raster).
Cần lấy pending từ app state (`self.edit.*`). Đây là việc tiếp theo giá trị nhất.

## 4. NEXT (theo thứ tự plan; giữ raster fallback cho cái chưa đúng)

1. **Phase 8 đầy đủ** — active-layer đi GPU (xử giới hạn ở §3) → xoá hẳn CPU AA
   supersample cho path GPU-native; đây mới là chỗ CPU 50–60% biến mất.
2. **Phase 5 — gradient + opacity/blend cơ bản.** Additive (gradient hiện fallback).
   Cần WGSL thật (linear/radial stop eval + gradient transform + alpha stops) +
   snapshot vs CPU gradient rasterizer. CMYK gradient GIỮ fallback. Opacity≠1: nhân
   alpha trong composite. Blend≠Normal chỉ bật khi chứng minh accumulator đúng.
3. **Phase 7 — mask/clip/PowerClip/group.** RỦI RO CAO, làm TỪNG LÁT, mỗi lát cần
   semantics + reference image + fallback + test z-order. Không bật GPU cho ca chưa
   có test.
4. **Phase 9 — export** (độc lập, plan nói không cần cho hybrid canvas).

Ngoài ra (không thuộc phase, tùy chọn): nâng CPU `stroke_coverage` để honor
butt/square cap + miter/bevel join THẬT (hiện luôn tròn), rồi cho GPU honor theo —
nhưng đổi hình raster hiện có, cần user duyệt trước.

## 5. QUY TẮC AN TOÀN (bắt buộc)

- **CHỈ commit cục bộ, KHÔNG push** (Actions gần cạn; user chốt 2026-07-21). Xong
  việc = `cargo fmt --check` + test + commit `-F` (message dài) + báo cáo. Đừng
  push, đừng hỏi push. Commit msg kết bằng `Co-Authored-By:` như các commit trước.
- **Không silently render sai:** feature chưa hỗ trợ PHẢI raster fallback qua
  `eligibility.rs`. Một Path chỉ 1 representation/frame (không halo/double-edge).
- **Không bỏ `path_display`/đổi export/`.iai`** ngoài phase đã nêu.
- **Flag mặc định TẮT** phải giữ raster pipeline y hệt từng byte.
- Đừng sửa `.rs` bằng PowerShell Get/Set-Content (phá UTF-8) — dùng công cụ edit.
- `dist/` untracked, đừng commit.

## 6. Build / test / GUI-test workflow

```bash
cargo fmt --check
cargo test --lib                      # 1014 pass, CI-safe (không cần GPU)
cargo test --test vector_gpu_render -- --ignored --nocapture --test-threads=1   # 6 GPU snapshot (máy có GPU)
cargo build --release
```

GUI-test (user tự bấm): copy `target/release/iai.exe` → `dist/iAi-portable/iai_phase3_6.exe`
(app có thể đang giữ `iai.exe` → đừng kill, ghi ra tên `iai_phase3_6.exe`). User chạy
`dist/iAi-portable/Chay-GPU-vector-ON.bat` (đặt env `IAI_GPU_VECTOR_CANVAS=1` rồi mở
bản mới). **Test GPU PHẢI `--test-threads=1`** (nhiều device song song bị driver
serialize, tưởng treo). GPU snapshot là local/manual, KHÔNG đặt làm cổng CI blocking.

## 7. Tài liệu liên quan

- `KE_HOACH_CANVAS_HYBRID_VECTOR_RASTER_2026-07-30.md` — plan 9 phase (nguồn yêu cầu).
- `docs/HYBRID_VECTOR_CANVAS_REPORT_2026-07-30.md` — báo cáo kiến trúc Phase 0–6 +
  status các phase còn lại.
- `docs/HYBRID_VECTOR_CANVAS_MANUAL_TESTS_2026-07-31_VI.md` — checklist test tay
  (flag on), cơ bản → nâng cao.
- Memory (auto): `project_iai_hybrid_canvas.md` (topic), `MEMORY.md` (index).

## 8. Việc user đang chờ ngay lúc bàn giao

User vừa test bản `d0628f0` (fix bông hoa Repeat hết "quét làm nét từng layer" khi
zoom). **Nếu user báo còn lỗi → ưu tiên xử trước khi đi phase mới.** Nếu OK → làm
Phase 8 đầy đủ (§3/§4) rồi tới Phase 5.
