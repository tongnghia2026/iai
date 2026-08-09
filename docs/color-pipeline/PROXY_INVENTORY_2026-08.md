# Develop preview/proxy inventory — August 2026

| Path | Resolution policy | Role | Settled/commit risk |
|---|---|---|---|
| `develop_scene::build_scene_color_base_box` | `fast_preview_downsample`, historically near 1/6 | Builds the scene Color Mixer preview proxy for the visible region | Color detail and classification may differ from the full-resolution commit |
| `develop_scene::build_scene_region_base` | `TONE_DOWNSAMPLE` | Edge-aware regional exposure plane for Highlights/Shadows | CPU and WGSL must use identical sample coordinates and interpolation |
| `develop::build_color_lowpass` | Per-tile full-size result with a filtered color base | Committed display-domain Color Mixer path | Must be compared with the scene and GPU preview paths separately |
| `gpu::CompositorState::plan_layer_proxy` | Power-of-two LOD selected from zoom | General compositing optimization for zoomed-out layers | Not a Develop quality proxy; disabled for partial/mode-A paths and must not be mistaken for 1/6 Color Mixer processing |
| `develop_scene::build_scene_histogram_proxy` | Approximately 60,000 samples | Histogram only | Does not directly alter rendered pixels |

The current shader consumes uploaded regional-luma and color proxies in `dev_region_e_at` and `dev_color_proxy_at`. The CPU commit uses `render_scene_display_inner` for scene RAW and `apply_to_tilemap_direct` for the remaining legacy stages. A headless Develop shader/readback entry point is still required for quantitative GPU parity.
