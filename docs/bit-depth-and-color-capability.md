# Bit depth & colour mode — capability matrix

**Status: the 16-bit _data_ path is now broad; the on-screen _display_ path is
still 8-bit. Do not describe IAI as "16-bit end to end" — the render pipeline
quantizes on the way to the screen.**

This document records what actually survives at 16 bits today, so nobody has to
infer it from the code, and so a fallback is never mistaken for a native path.
It is a statement of current behaviour, not a roadmap.

First written during the architecture-hardening milestone (2026-07-17), when
16-bit was an import-and-Develop capability only. Updated 2026-07-19 after the
B1/B2 work: `.iai` now round-trips 16 bits, the mode flag persists, and the
common raster edits (paint, fill, gradient, crop, flip, 90° rotate, resize /
rotate-by-angle, merge, flatten, filters, Smart Fill) preserve the master instead
of dropping it. The remaining 8-bit paths are the colour-mode conversions and the
render/display path (see below for why the last is intentional).

## The model

A tile ([`core::tile::Tile`]) holds an **8-bit mirror** (`pixels`, always
present) and an **optional 16-bit master** (`pixels16`). The 8-bit mirror is what
the compositor, the GPU atlas and every preview read. The 16-bit master is the
precision source when one exists.

`TileMap::has_hdr()` reports "every tile has a master". A single 8-bit write into
one tile drops that tile's master and, with it, the whole layer's 16-bit status.

Historically this meant **16-bit was a property a document could lose, silently,
by being edited**. B2 closed most of that: an 8-bit edit snapshots the tiles
first, and `repromote_after_paint` then rebuilds each touched tile's master —
restoring the untouched pixels to their exact 16-bit values and up-converting the
changed ones (`v*257`). So `has_hdr()` now survives the common edits. The only
path that still drops it does so deliberately: colour-mode conversion leaves the
RGB domain.

## Legend

| Term | Meaning |
| --- | --- |
| **Native** | Operates on the 16-bit master directly; precision preserved. |
| **Preserved** | An 8-bit operation runs, but the surrounding untouched 16-bit is restored (repromote) and only the pixels the op actually changed become 8-bit-sourced. `has_hdr()` survives; the document stays 16-bit. |
| **Quantized** | Runs at 8 bits; the 16-bit master is dropped or rebuilt from 8-bit values. Precision is gone and does not come back. |
| **Fallback** | A 16-bit path exists but this case takes an 8-bit one. |
| **Blocked** | Refused outright. |
| **N/A** | Bit depth is not meaningful here. |

## Matrix

### Import / export

| Operation | Status | Notes |
| --- | --- | --- |
| PNG 16-bit import | Native | `import_canvas_managed` keeps precision. |
| TIFF 16-bit import | Native | As above. |
| RAW import | Native | Decodes to a 16-bit master. |
| PNG export | Native / Fallback | 16-bit RGBA out **only when precision survives**; otherwise 8-bit. |
| TIFF export | Native / Fallback | 16-bit path is **single-layer 16-bit documents only**. Multi-layer → 8-bit. |
| JPEG / WebP export | Quantized | Formats are 8-bit. Expected, not a gap. |
| **`.iai` layer payload** | **Native (B1)** | A layer with a master is written as a **16-bit RGBA PNG**; load rebuilds the master via `from_rgba16`. No format-version bump — a 16-bit PNG still decodes as 8-bit in older builds. The `bit_depth` mode flag is persisted too (B2.1), so a reopened document stays editable at 16 bits. |
| PSD | Quantized | 8-bit payload. |

### Editing

| Operation | Status | Notes |
| --- | --- | --- |
| Develop (RAW/scene chain) | Native | The main reason the 16-bit master exists. |
| Canvas 16-bit I/O (`write_region16`) | Native | The explicit 16-bit write path. |
| Global adjustments (Levels/Curves) | Native | 16-bit paths present. |
| Paint / brush / eraser | Preserved (B2.1) | 8-bit `write_region`, then `end_stroke` repromotes; untouched pixels keep 16-bit. Gated on `has_hdr()` (not just the mode flag), so it holds even for a reopened 16-bit `.iai`. |
| Fill / gradient | Preserved (B2.1) | Same stroke path as paint. |
| Filters / Smart Fill | Preserved | The shared commit (`commit_layer_tiles_change`) repromotes from the before-state, so a selection-limited op keeps the rest of the layer at 16-bit. The pixels it actually changes are 8-bit-computed. |
| Crop | Native (B2.2) | `blit_region_from` copies the 16-bit master region (`flatten16_region_into` → `write_region16`); the fill/border is promoted so `has_hdr()` holds. |
| Flip / 90° rotate | Native (B2.3) | `flip_h/flip_v/rotate_90_*` permute `pixels16` alongside the mirror. |
| Resize / rotate-by-angle / perspective | Native (B2.4) | `resample_into_tiles` samples with `sample_bilinear16` and writes `write_region16`. |
| Layer merge / flatten | Preserved | `merge_down`/`merge_selected` call `ensure_16bit_layer_masters`; `merge_visible` and `flatten_all` use the `merge_all16` path in 16-bit mode. |
| Isolated / effected groups | Fallback | Explicit 8-bit fallback for groups with opacity/blend/mask. |

### Rendering

The display path is 8-bit **by design**, and making the atlas 16-bit would not
change that — see "Why the display stays 8-bit" below.

| Surface | Status | Notes |
| --- | --- | --- |
| GPU tile atlas | Quantized (intentional) | `TILE_ATLAS_FORMAT = Rgba8UnormSrgb`. Fed by the mirror, which is **ordered-dithered** from the master (`quantize_dither`, Bayer-8), so gradients do not posterize on screen. |
| CPU composite | Quantized (intentional) | Reads the dithered 8-bit mirror. |

### Colour mode

| Operation | Status | Notes |
| --- | --- | --- |
| RGB → CMYK (`convert_to_cmyk`) | Quantized + **history cleared** | Calls `drop_hdr()` on every layer, then `cmd_history.clear()`. Both are deliberate: the ink planes are re-encoded from the mirror, and history snapshots would point at layers that no longer exist in the same domain. **The conversion is not undoable**, and since the gateway work the document correctly reports dirty afterwards (a cleared history cannot prove the content matches the file). |
| CMYK → RGB (`convert_to_rgb_mode`) | Quantized + history cleared | Same reasoning: ink planes dropped, RGB mirror becomes ground truth. |
| Per-ink Levels/Curves | Native (ink domain) | Operates on ink planes. |
| CMYK export / print (DeviceCMYK) | Supported | Separate from bit depth. |

## The honest summary

16-bit in IAI is now a **document data mode**, not just an import capability:

1. It arrives via RAW / 16-bit PNG / 16-bit TIFF.
2. It survives Develop, the global adjustments, and now the common raster edits
   (paint, fill, gradient, crop, flip, 90° rotate, resize / rotate-by-angle,
   merge, flatten) and a `.iai` save/reopen/edit round-trip.
3. It is dropped only where that is deliberate: **colour-mode conversion** (CMYK
   leaves the RGB domain).
4. It is **not seen at full precision on screen** — but that is by design, not a
   gap (next section).

Claiming "16-bit end to end" is still wrong: the display path is 8-bit.

## Why the display stays 8-bit (and a 16-bit atlas would not help)

- The **presentation surface is 8-bit sRGB** (`src/gpu/mod.rs` picks the first
  `is_srgb()` surface format — `Bgra8UnormSrgb`/`Rgba8UnormSrgb`). Whatever the
  atlas format, the final present is truncated to 8 bits, and most monitors are
  8-bit panels anyway. Real 16-bit-on-screen needs a 10/16-bit HDR surface and
  display — a separate, hardware-gated project, not an atlas format change.
- The 8-bit mirror the atlas uploads is **ordered-dithered** from the master
  (`quantize_dither`), which was added specifically so stretched RAW skies do not
  band. The on-screen artifact a 16-bit atlas would target is already mitigated.
- The intermediate composite targets are already `Rgba16Float`, so precision is
  kept where it is cheap; only the source-tile atlas and the final surface are
  8-bit.

So a 16-bit atlas is high cost (no sRGB variant for 16-bit → manual sRGB in every
atlas shader, VRAM ×2) for a benefit the 8-bit surface throws away. Deliberately
not done.

## If you work on this

- `TileMap::has_hdr()` is the honest check; use it rather than assuming.
- Adding a 16-bit path means the *write* path, not just the read: an 8-bit
  `write_region` anywhere in an operation quietly ends 16-bit for that tile —
  unless the op snapshots first and repromotes (the B2 pattern) or writes 16-bit
  directly (`write_region16` / `sample_bilinear16` / `flatten16_region_into`).
- The RGB-domain editing paths all preserve 16-bit now. If you add a new one that
  replaces a layer's tiles from an 8-bit computation, route it through
  `commit_layer_tiles_change` (which repromotes) or repromote yourself.
- Any `.iai` change must stay backward compatible with existing files (the 16-bit
  payload rides in a normal PNG and the `bit_depth` key is optional).
