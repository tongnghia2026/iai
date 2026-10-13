# Phase 6 — Full-resolution settled preview and parity

## Preview states

The Develop header now reports the actual quality state:

- **Interactive**: GPU/proxy frame while controls are moving.
- **Refining…**: the pointer was released (120 ms paint-first delay), a
  keyboard edit reached its 650 ms quiet period, or the full-resolution bake
  is running.
- **Full quality**: the displayed tiles came from the exact commit pipeline.

Every non-neutral GPU edit schedules the full-resolution bake. A newer edit cancels/replaces the pending result by job id. When the result lands, the GPU overlay is disabled and the baked tiles remain visible; pressing Apply with unchanged settings reuses those exact tiles.

Full-resolution bakes are single-flight. If the user edits again while one is
running, its receiver stays alive until the worker exits, its stale result is
discarded by job id, and only then may the newest settled settings start. This
prevents uncancellable Rayon jobs from accumulating and competing with the UI.

No full-resolution bake starts while the primary pointer is down on a Develop
control. Dragging remains on the interactive GPU/proxy route; release schedules
one bake for only the final settings.

RAW GPU preview uploads `SceneToneData::rgb`, not merely the user point-curve
tables. That composed table contains the per-image camera RGB curve fitted from
the embedded preview. Omitting it made the camera look disappear on the first
interactive adjustment and reappear during CPU refine. The headless parity gate
now includes a non-identity camera curve plus Color Mixer edits and requires
`max <= 2/255`, `P99 <= 1/255` against commit.

Develop sliders clamp their allocation to the visible panel clip width. This
keeps gradient tracks and numeric fields inside the right panel even when a
vertical scroll area's virtual content width was enlarged by another control.

## Proxy inventory

- RAW tone, Color Mixer v2, saturation/vibrance and curves run per pixel in the scene GPU shader while interactive.
- Regional Highlights/Shadows/Whites/Blacks use the shared downsampled exposure plane.
- Texture/Clarity/Defog/Vignette may use the fast interactive proxy.
- Non-RAW colour edits may use the regional colour proxy interactively.
- Detail and local masks already use the CPU/full-resolution path.
- Regardless of interactive route, the resting frame is replaced by the exact full-resolution commit route.

## Colour quality

`Color Smoothing` is now explicit and defaults to `0`:

- `0`: full-resolution direct colour transform; no chroma-detail attenuation.
- `1–99`: blend between direct quality and guided reconstruction.
- `100`: legacy region-guided smoothing/deblocking.

Legacy smoothing behavior remains covered by its block-noise and off-hue-speck tests, now with the option explicitly enabled. The default-quality regression test verifies direct and tile paths agree within one 8-bit code value.

## Automated gates

- Full Develop suite, including proxy and direct quality paths.
- Settled pixel evaluator equals commit at 16-bit precision.
- Actual headless WGPU/CPU parity remains within the established tolerance.
- WGSL parsing and CPU/WGSL mirror tests remain mandatory.

Manual verification must confirm that the badge reaches **Full quality** and that pressing Apply produces no visible change.
