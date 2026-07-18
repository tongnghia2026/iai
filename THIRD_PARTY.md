# Third-party licenses

IAI itself is MIT-licensed (see [LICENSE](LICENSE)). It builds on the
following third-party components. Run `cargo license` for the full,
authoritative dependency tree.

## Notable license obligations

| Component | License | Notes |
|---|---|---|
| [rawloader](https://crates.io/crates/rawloader) | **LGPL-2.1** | RAW camera decoding. LGPL is more restrictive than MIT: binary distributions of IAI must allow users to relink against a modified rawloader. Distributing IAI's full source (as this repository does) satisfies that; keep this in mind for any closed-source fork or static redistribution. |

## Runtime-downloaded AI models

These are fetched on demand at runtime, not bundled:

| Model | License | Used by |
|---|---|---|
| LaMa (big-lama, ONNX) | Apache-2.0 | Smart Fill (AI inpainting) |
| CodeFormer (ONNX) | **S-Lab License 1.0 — non-commercial** | Face restoration. Review before any commercial distribution or hosted service. |

The Gemini AI panel calls Google's Gemini API; usage is governed by Google's
API terms and requires the user's own API key.

## Core dependencies (permissive)

wgpu, winit, egui/egui-wgpu/egui-winit, image, png, tiff, rayon, serde,
serde_json, flate2, zip, bytemuck, pollster, rfd, reqwest, ort (ONNX
Runtime), ab_glyph, egui-phosphor (Phosphor icons), base64, tungstenite —
all MIT and/or Apache-2.0 (bytemuck also Zlib).

lcms2 / lcms2-sys bindings are MIT; the bundled Little CMS 2 library is MIT.
