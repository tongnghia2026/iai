# Hybrid vector–raster canvas: implementation report

Date: 2026-07-30

## Safety state

The raster tile compositor remains the production renderer and the source
vector model is unchanged. `IAI_GPU_VECTOR_CANVAS=1` is the opt-in emergency
switch read by the new subsystem, but production compositing does not suppress
any raster twin until a complete GPU draw transaction is connected. Therefore
the current build cannot silently omit, double-render, or reorder a vector
layer. Export and `.iai` serialization are unchanged.

## Phase 0

Debug builds now count display-bake requests, superseded requests, completed
jobs, object count, supersampled pixels, and aggregate bake time in
`gpu::vector::telemetry`. Release builds compile these writes to no-ops.

The reproducible fixture in `tests/hybrid_canvas_bench.rs` creates 10, 100, 300,
and 500 flower paths. Each path has a counter/hole; alternating NonZero and
EvenOdd rules cover the same compound-path property used by Text→Curves glyphs.

Release results on the development machine:

| Layers | Whole-run tessellation | One changed object | Avoided work |
|---:|---:|---:|---:|
| 10 | 129 µs | 10 µs | 91.85% |
| 100 | 1,064 µs | 10 µs | 99.04% |
| 300 | 3,180 µs | 10 µs | 99.68% |
| 500 | 3,960 µs | 10 µs | 99.73% |

This measures the repeatable geometry workload, not end-to-end CPU raster
coverage/paint time or process CPU peak. It demonstrates that per-object
invalidation avoids about 99.7% of geometry work at 500 layers. Mid-job
cancelation is not implemented: the existing display worker owns a monolithic
raster call, so safe cancelation needs cancellation checkpoints inside the CPU
rasterizer.

Question A conclusion: per-object geometry caching has a large measured benefit;
job cancellation remains an independent optimization. This does not replace the
hybrid renderer.

Question B conclusion: the existing compositor can accept alternating runs only
inside its per-visible-layer ping/pong loop. The functions that must change are
`CompositorState::visible_layers_and_boundary`,
`CompositorState::layer_draws_this_frame`,
`CompositorState::composite_layers`, its partial-composite parity pre-count, and
backdrop-cache signature/snapshot handling. Adding a final overlay would be
incorrect. The pure `scene::plan_runs` spike proves ordering and grouping
without changing production pixels.

Comparison thresholds fixed for later local GPU snapshots:

- Reference: `core::vector::raster`, same scene and output dimensions.
- Convert sRGB samples to linear light before comparison.
- Interior (at least 1 px from an edge): maximum error 2/255 per linear channel.
- AA band: correspondence within ±1 px and local SSIM at least 0.98.
- No missing coverage outside the ±1 px band.
- Threshold changes require a dated rationale in this document.

## Implemented hybrid foundation

- Lyon path conversion preserving cubic segments and contour closure.
- Lyon fill tessellation with NonZero/EvenOdd mapping.
- Solid stroke tessellation with width, butt/round/square caps,
  miter/round/bevel joins, and miter limit.
- Stable geometry fingerprint excluding paint and affine/view transforms.
- Byte-budgeted LRU mesh cache and per-document clearing.
- Eligibility that falls back the whole layer for disabled mode, masks,
  PowerClip, groups, non-Normal blend, opacity, CMYK, gradients, dash, vector
  brush, primitives not yet qualified, and invalid geometry.
- Alternating raster/GPU run planner preserving stack order.
- Dedicated WGSL vertex/fragment shader and blocking Naga parse test.
- Debug-only display-bake telemetry.

Pan, zoom, fill-color changes, opacity changes, and affine transforms do not
alter the geometry fingerprint. Stroke geometry changes do.

## Features deliberately still on raster fallback

The following are not claimed GPU-native because the production vector draw
pass is not connected to both Mode A and Mode B accumulator paths:

- all vector layers in production;
- gradients and opacity;
- parametric primitives;
- dash strokes and vector brushes;
- masks, clipping, PowerClip, groups, isolation, advanced blend, and CMYK;
- `path_display` retirement.

Keeping these paths rasterized is the required safe behavior. `path_display`
has not been disabled. Phase 9 export remains a separate project and was not
modified.

---

# Continuation: 2026-07-31 (Phase 1 GPU renderer + Phase 2 semantics)

The prior session stopped at the CPU-side scaffolding (Lyon tessellation, mesh
cache, eligibility, run planner, a WGSL shader that only *parsed*). It never
built the actual GPU draw pass. This continuation implements and verifies it.

## Phase 1 — the real off-screen GPU renderer (DONE, verified on GPU)

`src/gpu/vector/renderer.rs` is a self-contained wgpu renderer:

- One pipeline built from `vector.wgsl`; per-draw uniforms via dynamic offset.
- MSAA 4× colour target (`Rgba8UnormSrgb`, matching the canvas texture),
  resolved to a sample-count-1 texture, read back tight.
- Draws a list of `VectorDraw { mesh, object_to_canvas, fill, stroke }`. Fill and
  stroke of one object are separate uniform blocks so each gets its colour.
- `headless_device()` requests an adapter with no surface, so the render tests
  run locally without a window and skip gracefully where no adapter exists.

### Colour space and alpha (the two correctness traps)

1. **Gamma.** `ColorValue::Rgb` stores sRGB-encoded bytes (`to_rgba8` = `round(v*255)`).
   The shader receives the fill colour as *linear* = `srgb_to_linear(v)`; the sRGB
   render target re-encodes on store, so an opaque interior pixel comes out
   byte-identical to the CPU rasteriser. AA (MSAA coverage) is resolved in linear
   light — the Phase 0 metric's requirement.
2. **Premultiplied vs straight alpha.** MSAA resolve over a transparent clear is
   inherently premultiplied, so the shader emits premultiplied colour and the
   target is a consistent premultiplied surface (overlapping shapes in one run
   source-over correctly). **The production compositor accumulator is *straight*
   alpha, blended in *sRGB* space** (`compositor.wgsl` `fs_main`, blend mode 0).
   A resolved vector run must therefore be un-premultiplied (`unpremultiply_srgb`,
   done in linear) before it is composited as a run. Interior/opaque pixels are
   identical under both conventions; only the ±1 px AA band differs, which the
   plan permits.

### Verification (local, real GPU — `tests/vector_gpu_render.rs`, `#[ignore]`)

Interior + hole/exterior compared against `core::vector::raster` per the fixed
Phase 0 thresholds (interior ≤ 2/255 linear per channel; the ±1 px AA band
excluded). All pass on the dev GPU:

| Fixture | 1× | 8× |
|---|---|---|
| solid square | ok | ok |
| concave (notch) | ok | ok |
| compound hole NonZero | ok | ok |
| compound hole EvenOdd | ok | ok |
| ellipse (4 cubics) | ok | ok |
| rotated + non-uniform scaled | ok | ok |
| flipped (negative scale) | ok | ok |
| thick stroke, round cap (`gpu_stroke_matches_cpu_reference`) | ok | — |
| two overlapping fills in one run (`gpu_run_preserves_intra_run_z_order`) | ok | — |

The stroke test confirms Lyon stroke tessellation matches the CPU capsule model
for round caps; the intra-run test confirms the later draw occludes the earlier
one (Phase 2 intra-run z-order) on the real renderer path.

### CI-safe unit tests (blocking, no GPU) — `cargo test gpu::vector --lib`

13 tests: fill-rule mapping, stroke range, cache key/eviction, run planner
z-order, eligibility/fallback (incl. the new round-stroke rule), WGSL parse,
uniform 16-byte alignment, sRGB round-trip, un-premultiply, affine columns.

### Correctness fix in eligibility

The CPU rasteriser (`stroke_coverage`) draws *every* stroke as round capsules —
it ignores the cap/join style. `eligibility.rs` now falls back
(`FallbackReason::StrokeStyle`) any visible stroke whose cap ≠ Round or join ≠
Round, so a stroke can only go GPU-native when it will match the reference.

## Phase 2 — production wiring: SHIPPED behind the default-off flag

The vector run is now composited into the live ping/pong accumulator at its
z-position. It is gated by `IAI_GPU_VECTOR_CANVAS`; with the flag unset the
`vector_stage` is never built, the eligibility scan never runs, and the raster
pipeline is byte-for-byte unchanged (verified: full suite green with the flag
off). New files: `gpu/vector/composite.rs` (`VectorCompositeStage`) and
`gpu/vector/vector_composite.wgsl`.

How it works (`CompositorState::composite_layers`):

1. **`GpuState`/`CompositorState` own the stage** (`vector_stage: Option<…>`),
   built in `new` only when the flag is on, rebuilt with the whole GPU context on
   device loss.
2. **Simple-mode gating.** A present GPU vector run forces `use_partial = false`
   and disables the backdrop cache, sidestepping the partial-composite parity
   pre-count and cache-staleness traps at the cost of a full recomposite
   (Phase 3 re-optimises). Only the full viewport path is used
   (`!canvas_space && render_scale == 1 && crop_preview.is_none()`); otherwise the
   frame falls back to raster.
3. **Only static, non-active vector layers go GPU.** The ACTIVE layer stays on the
   raster + crisp-overlay path, because node/style/shape drags update a pending
   raster preview (the tiles) rather than the committed model the GPU reads.
   Free-transform-preview layers are excluded by id. A live multi-Move of other
   selected layers is followed by the offset **drift correction**
   (`layer.offset − model raster origin`) in `composite_run`, so those shapes
   track the pointer.
4. **Twin suppression = skip the tile pass** for each GPU layer; it contributes
   only through the vector run. One representation per layer → no halo.
5. **Vector run pass:** consecutive eligible layers form one run, rendered to an
   owned MSAA target, resolved (premultiplied), then composited over the current
   accumulator buffer into the other buffer by `vector_composite.wgsl`, which
   un-premultiplies and runs the same straight-alpha sRGB Normal blend as
   `compositor.wgsl` (mode 0). The run is one ping/pong step (one parity flip).
6. **Meshes cache per (geometry fingerprint, zoom bucket)** so pan/zoom/move never
   re-tessellate; the bucket re-tessellates finer as the run is magnified so
   curves stay crisp.
7. **Coordinates:** `object_to_canvas = translate(drift) ∘ object.transform`;
   `canvas_to_clip` folds `zoom`/`view_offset` (default viewport mode).

Validation: `tests/vector_gpu_render.rs::gpu_composite_run_over_background`
exercises the real `composite_run` (a vector run over a known background → correct
placement + straight-alpha blend). Remaining verification is manual/GUI (the doc
`HYBRID_VECTOR_CANVAS_HUONG_DAN_TEST_VI.md`, Vietnamese).

### Deliberately still raster (fallback), by design

- the ACTIVE (edited) layer, and any free-transform / crop preview;
- `canvas_space` (Mode A large-canvas) and low-res interactive previews;
- mask, clip, PowerClip, group, non-Normal blend, opacity ≠ 1, gradient, dash,
  non-round stroke, CMYK, primitives (Shape) — the run planner isolates only the
  eligible contiguous static Path layers.

Then Phases 3–8 proceed as the plan describes (GPU-buffer mesh cache; gradients
need a shader stop table; primitives convert to a temporary path; the active-layer
live-edit path could later go GPU by feeding the pending geometry; mask/clip/group
is the high-risk slice-by-slice phase; `path_display` retirement is last).

## Commands

```text
# blocking / CI-safe
cargo test gpu::vector --lib
cargo test --test hybrid_canvas_bench --no-run
cargo check --all-targets

# local / manual (real GPU; #[ignore])
cargo test --test vector_gpu_render -- --ignored --nocapture
cargo test --release --test hybrid_canvas_bench -- --ignored --nocapture
```

GPU snapshots are intentionally `#[ignore]` (local/manual), never a blocking CI
gate. The Phase 1 render tests were run and pass on the development GPU this
session.

---

## Phase 3 — GPU mesh cache + uniform-only transforms: SHIPPED (2026-07-31)

**Problem it fixes.** Phase 2 cached the *CPU* mesh (no re-tessellation on
pan/zoom/move), but `renderer.encode_run` re-created the GPU vertex/index buffers
**every frame** (`upload_meshes` → `create_buffer_init` per object). For a
500-layer stress scene that is ~1000 buffer allocations per frame during a pure
pan. Phase 3 caches the *GPU buffers* too, so pan/zoom(within a bucket)/move/
rotate/scale upload nothing.

**What changed.**

- `renderer.rs`: a mesh handed to a draw is now a **`GpuMesh`** — the uploaded
  vertex/index buffers plus the fill/stroke index ranges plus a byte size —
  instead of a CPU `VectorMesh`. `VectorDraw.mesh: &GpuMesh`. `record` reads
  `draw.mesh.buffers` directly; `upload_meshes` is gone. Uploading happens once,
  in `GpuMesh::upload`.
- `cache.rs`: a generic, value-free **`ByteLru<K>`** holds the byte-budget +
  LRU-eviction policy (unit-tested with no GPU). It never evicts a key the caller
  marks protected, and never evicts the key it just inserted.
- `composite.rs`: `VectorCompositeStage` now owns a **`GpuMeshCache`** =
  `HashMap<(fingerprint, bucket), GpuMesh>` + `ByteLru`. `composite_run` runs in
  three passes: (A) compute each object's `(fingerprint, bucket)` key + tolerance
  and collect the frame's working set; (B) `ensure` every key resident —
  tessellate + upload only on a miss, protecting the working set from eviction;
  (C) build the draws from the resident cached meshes. Budget:
  `MESH_CACHE_BYTE_BUDGET = 96 MiB` of source vertex/index bytes.
- `telemetry.rs`: `mesh_frame(tessellations, uploads, evictions, bytes, entries)`
  publishes the per-frame cache activity + size (debug-only). The stage also
  exposes `last_frame_tessellations/uploads`, `cache_len/bytes/evictions`.

**Invalidation.** A node/geometry/style edit changes the fingerprint → only that
one path's mesh is rebuilt. Device loss rebuilds the whole stage (empty cache).
The cache is content-keyed (no document id), so identical shapes across documents
share a mesh correctly, and stale entries from a closed document are LRU-evicted.

**Verification.** `cargo test gpu::vector --lib` (20 tests incl. 5 `ByteLru`
cases) is green on CI-safe hardware. The local GPU test
`gpu_mesh_cache_is_transform_invariant` drives `composite_run` across frames and
asserts **(tessellations, uploads) == (0, 0)** for pan, zoom-in-bucket, move,
rotate and non-uniform scale, and **== (1, 1)** for a node edit (ending at 2 cache
entries, 0 evictions). Passes on the development GPU this session.

**Still fallback / not yet done in Phase 3.** The **active (edited) layer stays
raster** — moving it to GPU during a pure move/rotate would need the pending live
geometry fed to the GPU, deferred to the `path_display`-retirement work (Phase 8).
Object-scale no longer re-tessellates (tolerance is keyed by zoom bucket only), a
deliberate trade for "scale = 0 tessellation"; view zoom keeps curves crisp.

## Phase 6 — parametric primitives (Shape): SHIPPED (2026-07-31)

`VectorGeometry::Primitive(ShapeData)` (rectangle, rounded rectangle, ellipse,
line, polygon, star) is now GPU-native. It is drawn by converting the shape to the
**exact same `PathData` its raster twin uses** — `ShapeData::to_vector_object`,
the same conversion `path_display` already relies on — so the GPU output matches
the CPU reference and the flag toggles cleanly.

- `eligibility.rs`: `layer_eligibility` gates a Primitive by `style_eligibility`
  (a primitive's geometry is always valid and never has a brush, so only its style
  decides — no per-frame path allocation). Solid RGB fill / round stroke = GPU;
  gradient / CMYK / dash / non-round stroke / opacity ≠ 1 = fallback, same rules
  as a Path.
- `compositor.rs`: the run collector materialises converted primitives into an
  owned `Vec<VectorObjectData>` (capacity reserved so references stay valid), then
  borrows them alongside the Path objects in z-order. Each converted primitive is
  passed with **its own raster origin as the offset**, so the drag-drift
  correction is a no-op and the shape draws exactly where its (now-suppressed)
  raster twin sat. Shape coordinates are layer-local; `to_vector_object(offset)`
  maps them to canvas space.

**Verification.** `solid_primitive_is_eligible_but_gradient_primitive_falls_back`
(CI-safe) proves the eligibility gate. The local GPU test
`gpu_primitive_matches_cpu_reference` renders converted rectangle / rounded-rect /
ellipse / polygon / star primitives and asserts each matches `raster::rasterize`
within the fixed Phase 0 interior/exterior thresholds. Passes on the development
GPU this session.

## Phase 8 — active idle vector + retire its CPU AA bake: SHIPPED (2026-07-31)

The active layer now joins the native GPU run whenever no live edit owns pending
state. `App::active_vector_gpu_idle` is the conservative gate shared with
`path_display`: Move painting, free/path transform, gradient drag, node drag,
Path style session, Shape drag/style scrub, and an in-flight Shape bake keep the
active layer on its existing raster preview. Once idle, compositor eligibility
allows the active layer and `path_display` stops before it, so a GPU-native Path
or Shape no longer launches the supersampled CPU worker merely because it is
selected.

The GPU decision remains centralized in
`CompositorState::will_draw_vector_layer_on_gpu`, including the runtime stage,
viewport mode, crop/transform-preview guards, and `eligibility.rs` fallback.
Unsupported gradients, opacity/blend, masks/groups, dash/brush and CMYK still use
the raster twin. The module is intentionally retained for those fallback layers
and for live edit previews; only its GPU-native consumer path is retired.

Ordinary small canvases now use viewport-space Mode B while the default-off GPU
flag is enabled. Mode A composites at document resolution and magnifies only in
the final blit, so it cannot host sharp view-dependent vectors. With the flag
off, the legacy Mode A/B choice is unchanged. The Repeat flower GUI checkpoint
confirmed that zoom/pan above 100% no longer sweeps layers through CPU bakes.

Verification: current-frame policy regression tests, active-idle/style-session
gate test, the full library suite, and all six serialized local GPU snapshots.

## Remaining phases (honest status, 2026-07-31)

- **Phase 4 stroke — partially shipped.** Every solid stroke is GPU-native and
  tessellated as round capsules regardless of stored cap/join, matching the CPU
  reference `stroke_coverage` exactly. Dash and vector brush remain raster
  fallback. Honouring butt/square/miter/bevel visually still requires upgrading
  the CPU reference and is a separate appearance-changing decision.
- **Phase 5 gradient / opacity / blend — gradient + Normal opacity shipped.**
  Linear and radial RGB gradients are evaluated in `vector.wgsl` with the model's
  object→gradient inverse transform, up to eight sorted stops, clamp semantics,
  and alpha-stop interpolation matching the CPU reference. Stop colours interpolate
  in sRGB, then convert to linear for the sRGB render target. A gradient containing
  any CMYK stop remains whole-layer raster fallback. Stop/style changes update
  uniforms without changing the geometry fingerprint. Normal object and layer
  opacity are GPU-native; the draw alpha is
  `paint alpha × object opacity × layer opacity`, verified by a real-GPU
  `0.5 × 0.5 = 0.25` snapshot. Blend modes other than Normal remain fallback.
  Real-GPU snapshots compare linear/radial transformed gradients and alpha stops
  directly against `core::vector::raster`.
- **Phase 7 mask / clip / PowerClip / group — sliced implementation started.**
  Vector children of a pass-through group (Normal, 100%, no enabled mask) may
  now remain GPU-native because CPU semantics are exactly inline compositing.
  Stack-aware eligibility walks every ancestor; missing, hidden, isolated,
  blended, translucent, or masked groups retain whole-layer raster fallback.
  A byte-exact inline CPU reference and interleaved z-order planner test lock
  this first group slice. The second slice supports an ordinary raster layer
  mask on one GPU vector layer: masked layers are isolated into their own run,
  the R8 mask texture is cached by tile revision, and the composite samples it
  nearest/clamp-to-edge in layer-local space. A real-GPU hard/soft-mask snapshot
  locks coverage and sRGB source-over. PowerClip, vector masks, and group
  isolation remain fallback until later reference-tested slices.
  slice-by-slice, each slice needs a defined semantics + reference image; unsafe
  to land without manual verification.
- **Phase 8 retire `path_display` — shipped for GPU-native paths.** Active idle
  Paths/Shapes now go GPU and never enter the CPU AA worker. `path_display`
  remains only for unsupported/fallback features and live edit previews.
- **Phase 9 export — not started, intentionally.** The plan marks it independent
  of the hybrid canvas.
