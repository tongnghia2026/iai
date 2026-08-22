# Color/light pipeline baseline — August 2026

Algorithm identifier: `iai-scene-v1`  
Working assumption: scene-linear RGB using sRGB primaries  
Probe command: `cargo run --bin color_probe -- target/color-baseline.csv`

The command evaluates the public `eval_scene_pixel` path for a neutral setup and slider values -100, -50, +50 and +100. It covers global saturation, exposure, and every Color Mixer saturation band. Inputs include neutral values, representative hue patches, a negative-channel vector, and an HDR vector. Output is deterministic CSV suitable for version-to-version comparison.

## Initial automated acceptance checks

- Every probe result is finite and deterministic.
- Neutral greys remain neutral within `1e-5` channel spread.
- Exposure is monotone for shadow, middle-grey, and highlight probes.
- Settled CPU pixel evaluation and committed scene output differ by at most one 16-bit code value for the non-spatial test matrix.
- `compositor.wgsl` parses successfully through Naga.
- Historical headless WGPU result was maximum error `1/255`, P99 `1/255` on the
  procedural signed/HDR grid. Re-verification on 2026-08-11 now fails at maximum
  `19/255`, P99 `6/255`; see
  `COLOR_ENGINE_RECONSTRUCTION_BASELINE_2026-08-11.md`. The ignored test must not
  be described as an active passing gate until Phase 6 fixes it.

Headless parity command: `cargo test --test develop_cpu_gpu_parity headless_gpu_preview_matches_committed_scene -- --ignored --nocapture --test-threads=1`

## Baseline defect/regression matrix

| Risk observed in the legacy pipeline | Quantitative/regression coverage |
|---|---|
| Preview/commit jump | `settled_pixel_evaluator_matches_committed_scene_for_non_spatial_edits`; headless GPU parity test above |
| Red bleeding into orange/skin | `red_band_leaves_orange_skin_mostly_alone`, `reds_release_saturated_orange_but_keep_brick_and_lip_red` |
| Hue-band seams and distant-band bleed | `adjacent_band_boundaries_are_continuous`, `band_edits_do_not_reach_non_adjacent_families` |
| Highlight desaturation/ordering | `compressed_highlights_fade_smoothly_toward_white`, `highlights_and_whites_preserve_near_white_ordering` |
| Shadow colour becoming grey | `shadows_lift_dark_color_families_without_neutralizing`, `shadow_chroma_restore_preserves_luma_and_neutrals` |
| Chroma blocking/smearing | `color_region_box_deblocks_like_commit`, `color_mixer_softens_small_chroma_blocks_vs_per_pixel` |
| HDR/negative-channel instability | `scene_pixel_probe_is_finite_and_deterministic`, `hdr_headroom_recovers_under_negative_exposure` |

The procedural grid is the reproducible baseline crop: each row is a fixed lightness and each column moves through signed, neutral, saturated and HDR colors. Pixel differences are recorded numerically rather than relying on subjective screenshots. Tests named above originated from previously observed failures and now act as passing regression gates.

## Synthetic release benchmark

Command: `cargo test --release --test perf_color_pipeline benchmark_synthetic_12_24_45_mp_commit -- --ignored --nocapture --test-threads=1`

Host reported by the probe: Windows x86_64. The corpus is a procedural signed/HDR gradient with Light and Color Mixer active.

| Size | Dimensions | CPU commit |
|---|---:|---:|
| 12 MP | 4000 × 3000 | 725.657 ms |
| 24 MP | 6000 × 4000 | 1375.473 ms |
| 45 MP | 8256 × 5504 | 2644.615 ms |

These are initial wall-clock measurements, not a cross-machine performance budget. GPU time and VRAM remain unmeasured.

## Known gaps

- CPU model/RAM and GPU/VRAM metadata are not currently exposed by the benchmark; only OS and architecture are recorded.
- External Middlebury ColorChecker RGB and Cube++ sensor-RAW/known-illuminant
  fixtures are now measured from the ignored target cache; neither asset is
  committed. A sensor RAW ColorChecker/spectral reference is still missing.
- Existing uncommitted mixer and RAW-look tuning predates this baseline and is captured as part of `iai-scene-v1`; the regression matrix validates its currently intended behavior but does not establish provenance for those edits.
