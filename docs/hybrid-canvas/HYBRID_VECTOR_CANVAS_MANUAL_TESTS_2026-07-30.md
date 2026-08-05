# Sequential manual tests: hybrid vector canvas

Run these tests in order. Save a disposable copy before tests that edit or
reorder layers. Unless a step explicitly enables the experimental flag, the
raster fallback is the expected renderer.

## What is in production right now (2026-07-31)

The GPU vector renderer (Phase 1) is complete and **verified by automated local
GPU tests**, but it is **not yet wired into the live canvas** — so with the flag
unset the app renders exactly as before. What *did* change in production is the
Phase 0 "cheap shortcut": the crisp-display bake is now **per-object** (only the
edited object re-bakes, not the whole run) and a superseded bake is **cancelled
mid-run**. Tests 5 and 11 exercise that. Everything else below confirms the
raster fallback is unchanged.

To verify the GPU renderer itself (needs a real GPU, a few seconds):

```text
cargo test --test vector_gpu_render -- --ignored --nocapture
```

Expected: `gpu_fill_matches_cpu_reference`, `gpu_stroke_matches_cpu_reference`,
and `gpu_run_preserves_intra_run_z_order` all pass (or skip cleanly if no GPU
adapter is found). Each fixture prints `interior=… ok`.

## 1. Emergency switch and unchanged fallback

1. Launch normally with `IAI_GPU_VECTOR_CANVAS` unset. Open a document containing
   raster, text, Shape, and Path layers.
2. Zoom from 25% through 100%, 800%, and the highest practical zoom; pan while
   zooming.
3. Expected: every layer remains visible, ordering is unchanged, and Path edges
   eventually become crisp through the existing display bake.
4. Failure signs: missing Path, duplicate/halo edge, reordered content, black or
   transparent frame.

## 2. Basic solid paths and fill rules

1. Create a rectangle converted to curves, an ellipse converted to curves, a
   concave path, and two compound paths with holes (NonZero and EvenOdd).
2. Set solid RGB fills and no outline.
3. Inspect at 25%, 100%, 800%, and 6400%; flip and rotate each object.
4. Expected: holes and winding match before/after transforms; no fill changes
   when zooming.
5. Failure signs: filled-in counter, vanished contour, triangular crack, or edge
   shift greater than one screen pixel.

## 3. Text→Curves

1. Type text containing counters, for example `B8Oa`, then convert it to curves.
2. Compare the converted result with the original text at 100%, 800%, and 6400%.
3. Edit one node, undo, and redo.
4. Expected: counters remain holes and undo/redo restores the exact geometry.
5. Failure signs: reversed winding, missing glyph, stale pre-edit overlay.

## 4. Z-order

1. Build `raster – vector – translucent raster – vector – raster`.
2. Toggle each layer, reorder the middle vector to bottom and top, then undo/redo.
3. Repeat with the active layer at the bottom, middle, and top.
4. Expected: appearance matches the Layers panel exactly with no vector top
   overlay.
5. Failure signs: vector always appears above raster, double edge, or partial
   frame retaining the old order.

## 5. Transform and cache behavior

1. Create 100 solid Path layers and multi-select them.
2. Pan and zoom continuously, then move, rotate, uniform-scale, non-uniform-scale,
   and flip the selection.
3. Edit exactly one node on one Path.
4. Expected: interactions remain responsive; only node editing changes geometry;
   stopping interaction produces the sharp fallback display.
5. Failure signs: repeated long CPU work while only panning, geometry drift,
   stale old position, or memory increasing without settling.

## 6. Stroke coverage

1. Test open and closed paths with butt/round/square caps and
   miter/round/bevel joins.
2. Include a sharp cusp, zero-length segment, non-uniform scale, and a dashed
   closed contour.
3. Expected: current raster appearance remains unchanged. Dash must fall back as
   a whole layer.
4. Failure signs: stroke silently missing, cap clipped, dash becoming solid, or
   fill shown without its unsupported stroke.

## 7. Gradient and opacity

1. Apply linear and radial gradients with alpha stops; transform the gradient
   and object independently.
2. Set object and layer opacity below 100%, placing raster above and below.
3. Expected: all cases use raster fallback and match the pre-change renderer.
4. Failure signs: solid-color substitution, wrong alpha, gradient moving with
   the view, or changed blend result.

## 8. Primitive Shapes

1. Create rectangle, rounded rectangle, ellipse, line, polygon, and star without
   converting to curves.
2. Edit their parametric handles and styles.
3. Expected: editability and raster output are unchanged.
4. Failure signs: Shape converted destructively, handles disappear, or raster
   cache is suppressed.

## 9. Masks, clipping, groups, and blend

1. Test a layer mask on a Path, a vector mask, PowerClip, nested group,
   group opacity/isolation, and several non-Normal blend modes.
2. Reorder raster/vector children and toggle mask/group visibility.
3. Expected: every advanced case remains on raster fallback and matches normal
   app behavior.
4. Failure signs: unclipped content, mask ignored, blend becomes Normal, group
   alpha applied per child, or content disappears.

## 10. CMYK and colour management

1. Open a CMYK document with RGB and CMYK vector paints, then enable soft proof.
2. Compare zoom levels and toggle proofing.
3. Expected: CMYK vector paint stays raster fallback and preview changes only as
   the existing colour-management pipeline dictates.
4. Failure signs: CMYK rendered as unprofiled RGB, unexpected hue shift, or GPU
   path enabled for a CMYK paint.

## 11. Stress fixtures

1. Run the flower generator concept at 100, 300, and 500 layers, including
   compound holes and a converted-text layer.
2. At each size test zoom, pan-during-zoom, multi-move, rotate/scale, node edit,
   visibility, and reorder.
3. Expected: no missing content or crash; raster fallback always remains
   available. Record CPU peak, time from interaction stop to sharp display, and
   steady memory.
4. Failure signs: worker results arriving for an older edit, permanently blurry
   display, unbounded memory, device loss, or wrong z-order.

## 12. Persistence, tabs, and recovery

1. Save as `.iai`, close, reopen, and compare with the pre-save document.
2. Switch rapidly between two tabs while a sharp display bake is pending.
3. If possible, recreate the GPU/device by sleep/wake or driver recovery and
   reopen the document.
4. Expected: vector source data is unchanged, no mesh data is serialized, stale
   work never lands on another document, and raster fallback reconstructs the
   view.
5. Failure signs: changed file semantics, wrong-tab overlay, missing layer after
   reopen, or failure to recover through raster rendering.

