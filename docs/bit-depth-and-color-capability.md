# Bit depth & colour mode — capability matrix

**Status: IAI is not 16-bit end-to-end. Do not describe it as such.**

This document records what actually survives at 16 bits today, so nobody has to
infer it from the code, and so a fallback is never mistaken for a native path.
It is a statement of current behaviour, not a roadmap.

Written during the architecture-hardening milestone (2026-07-17). The refactor
deliberately changed none of this; the matrix exists so the gaps stay visible
instead of being quietly assumed away.

## The model

A tile ([`core::tile::Tile`]) holds an **8-bit mirror** (`pixels`, always
present) and an **optional 16-bit master** (`pixels16`). The 8-bit mirror is what
the compositor, the GPU atlas and every preview read. The 16-bit master is the
precision source when one exists.

`TileMap::has_hdr()` reports "every tile has a master". A single 8-bit write into
one tile drops that tile's master and, with it, the whole layer's 16-bit status.

This means **16-bit is a property a document can lose, silently, by being
edited** — not a mode it is in.

## Legend

| Term | Meaning |
| --- | --- |
| **Native** | Operates on the 16-bit master; precision preserved. |
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
| **`.iai` layer payload** | **Quantized** | **Layer pixels are written 8-bit. Bit depth does NOT round-trip: save a 16-bit document, reopen it, the master is gone.** The most consequential gap here. |
| PSD | Quantized | 8-bit payload. |

### Editing

| Operation | Status | Notes |
| --- | --- | --- |
| Develop (RAW/scene chain) | Native | The main reason the 16-bit master exists. |
| Canvas 16-bit I/O (`write_region16`) | Native | The explicit 16-bit write path. |
| Global adjustments (Levels/Curves) | Native | 16-bit paths present. |
| Paint / brush / eraser | Quantized | 8-bit `write_region` → drops that tile's master. |
| Fill / gradient | Quantized | As above. |
| Filters | Quantized | 8-bit. |
| Crop / resize / rotate | Quantized | Rebuilds tiles at 8 bits. |
| Layer merge / flatten | Quantized | Composites through the 8-bit mirror. |
| Isolated / effected groups | Fallback | Explicit 8-bit fallback for groups with opacity/blend/mask. |

### Rendering

| Surface | Status | Notes |
| --- | --- | --- |
| GPU tile atlas | Quantized | `TILE_ATLAS_FORMAT = Rgba8UnormSrgb`. Every GPU preview is 8-bit by construction. |
| CPU composite | Quantized | Reads the 8-bit mirror. |

### Colour mode

| Operation | Status | Notes |
| --- | --- | --- |
| RGB → CMYK (`convert_to_cmyk`) | Quantized + **history cleared** | Calls `drop_hdr()` on every layer, then `cmd_history.clear()`. Both are deliberate: the ink planes are re-encoded from the mirror, and history snapshots would point at layers that no longer exist in the same domain. **The conversion is not undoable**, and since the gateway work the document correctly reports dirty afterwards (a cleared history cannot prove the content matches the file). |
| CMYK → RGB (`convert_to_rgb_mode`) | Quantized + history cleared | Same reasoning: ink planes dropped, RGB mirror becomes ground truth. |
| Per-ink Levels/Curves | Native (ink domain) | Operates on ink planes. |
| CMYK export / print (DeviceCMYK) | Supported | Separate from bit depth. |

## The honest summary

16-bit in IAI is **an import-and-Develop capability**, not a document mode:

1. It arrives via RAW / 16-bit PNG / 16-bit TIFF.
2. It survives Develop and the global adjustment paths.
3. It is destroyed by most raster editing, by any colour-mode conversion, and by
   saving to `.iai`.
4. It is never seen on screen — the GPU atlas and CPU composite are both 8-bit.

Anything that claims more than that is wrong.

## If you work on this

- `TileMap::has_hdr()` is the honest check; use it rather than assuming.
- Adding a 16-bit path means the *write* path, not just the read: an 8-bit
  `write_region` anywhere in an operation quietly ends 16-bit for that tile.
- The highest-value gap is `.iai` round-tripping bit depth — everything else is
  moot while reopening a file discards the master. Any change there must stay
  backward compatible with existing `.iai` files.
