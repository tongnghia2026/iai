# Color engine reconstruction baseline - 2026-08-11

Scope: Phase 0 instrumentation and measured-reference vertical slice for
`DEVELOP_COLOR_ENGINE_RECONSTRUCTION_PLAN.md`. This slice changes no rendered
pixels. External images and generated reports live under ignored `target/`.

## Reproducible reference results

### Middlebury registered ColorChecker RGB

Selection: 24 cameras, `wb1i1e3` (fixed tungsten WB, 3200 K, nominal exposure),
inner 60% of every patch, shared-vector 10% trim, one scalar neutral exposure.
The source is dcraw-rendered linear RGB rather than sensor RAW; these numbers are
D50 reference comparisons, not iAi decoder accuracy.

| Scoreboard | Images | Mean delta-E00 | Other result |
|---|---:|---:|---:|
| dcraw-linear vs D50, all observations | 24 | 7.4048 | global p95 17.3054; max 28.8467; mean hue error 13.847 degrees |
| dcraw-linear vs D50, source clip <=1% per chromatic channel | 6 | 6.9388 | quality subset |
| camera JPEG vs D50 observation | 24 | 6.6903 | global p95 14.4505; picture style is not truth |
| current iAi tone-only vs D50, all observations | 24 | 7.5729 | global p95 16.5952; max 29.9565 |
| current iAi tone-only vs D50, unclipped subset | 6 | 7.0843 | regression baseline |
| current iAi tone vs camera JPEG | 24 | 5.8194 | likeness only, after mid-neutral scalar fit |

Largest dcraw/D50 offenders are concentrated in the Sony DSC-F828 blue/magenta
patches: blue flower `28.8467`, magenta `27.8880`, blue sky `27.3916`, and
purplish blue `24.5504`. That camera also puts white and neutral-8 among the top
offenders (`24.5454` and `24.2733`), so the evidence records both chromatic and
neutral-axis failure and does not isolate a single cause.

Generated artifacts:

- `target/phase0/checker24s/checker24s_summary.csv`
- `target/phase0/checker24s/checker24s_per_patch.csv`
- `target/phase0/checker24s/checker24s_aggregate.json`

### Cube++ real sensor RAW / known illuminant

Fixture: `20_2660.CR2`, Canon EOS 550D, ISO 100, natural daylight. Direct mosaic
sampling after black subtraction recovers the published SpyderCube illuminant
within `0.0850` degrees (left), `0.0262` degrees (right), and `0.0523` degrees
(mean). Registration and decode are therefore sound enough for a WB baseline.

| Stage | Left neutral/illuminant error | Right error |
|---|---:|---:|
| Camera-linear recovery vs published ground truth | 0.0850 degrees | 0.0262 degrees |
| Embedded as-shot WB vs published mean | 2.4337 degrees | same camera metadata |
| iAi scene, JPEG match disabled | 2.4416 degrees | 3.0544 degrees |
| iAi encoded output, JPEG match disabled | 2.4583 degrees | 3.0820 degrees |
| ART 1.26.7 neutral TIFF16/as-shot | 6.9981 degrees | 5.1385 degrees |

The ART row measures neutral residual for this one as-shot fixture; it is not a
general ranking of ART and iAi color accuracy. Neither engine was given the
published ground-truth WB. The fixture cannot measure camera-to-XYZ delta-E.

Decoder metadata also disagrees: rawloader exposes white level `15831`, while
Cube++ records `11767`; ART's camera constants choose `13480`. This can shift
scene exposure even when chromaticity is unchanged and must be resolved before
tuning the default tone.

ART command used no default/sidecar profile flags and produced neutral TIFF16 in
`RTv2_sRGB`:

```text
ART-cli.exe -q -o <target/phase0/art> -t -b16 -Y -V -c 20_2660.CR2
```

- ART version: `1.26.7`
- ART executable SHA-256:
  `94E24C5093A291CEEB183CF397979CE1F6D7092DA09BD9942C743CC7512098DA`
- Output TIFF SHA-256:
  `88018DE39A0A77D1BE2C8124089520A3CA9ACF058DF6012D359F7F4F8C31335F`

### Local portrait RAW/JPEG observation

On the existing Sony ARW portrait probe, every observation is center-cropped to
the embedded JPEG aspect and measured on the same 1080x1616 pixel grid. This
makes the Laplacian scale comparable, but the images are not spatially
registered, so all values remain no-reference observations:

| iAi mode | OKLab L | OKLab C | Clip | Acutance |
|---|---:|---:|---:|---:|
| camera JPEG | 0.5426 | 0.0485 | 0.137% | 0.0008915 |
| full JPEG match | 0.5444 | 0.0473 | 1.528% | 0.0008231 |
| gain only | 0.4902 | 0.0407 | 0.003% | 0.0008689 |
| no JPEG match | 0.3656 | 0.0304 | 0.002% | 0.0006491 |

This quantifies why simply disabling JPEG matching is not yet an acceptable
default: the no-match path is substantially darker and less chromatic. Phase 1
must replace it with real characterization, then Phase 3 supplies a designed
render transform.

### Renderer banding baseline

A deterministic 4096-sample linear-neutral ramp rendered through the current
16-bit default-look path produces 3073 distinct luma codes, zero reversals, a
longest equal-code plateau of 2 samples, output endpoints 0 and 59576, and a
maximum normalized step of `0.001120474`. The CI test freezes this measured
behavior; it is a regression baseline, not a claim that 3073 levels are ideal.

## Realtime and parity baseline

Release hot-path probe on Cube++ `20_2660.CR2` (5196x3462). Image-sized setup
stages use 15 samples, per-frame stages use 30, and scene-tone construction uses
100. These are signed CPU-stage baselines, but are not an end-to-end
slider-to-present measurement:

| Stage | p95 |
|---|---:|
| build scene tone | 0.02 ms |
| histogram re-bin | 6.70 ms |
| EV/WB region finish | 16.76 ms |
| fit-view tone low-pass | 16.05 ms |
| fit-view color proxy | 9.08 ms |
| 100%-zoom tone low-pass | 6.53 ms |
| 100%-zoom color proxy | 2.03 ms |

The conservative serial sum of histogram, fit-view tone, and color stages is
`31.83 ms`. The Phase-6 machine-local target remains provisionally p95 `<50 ms`
until an input-event-to-present probe measures it directly; it must also perform
zero full-resolution renders while dragging. Full-resolution CPU commit timings
remain a separate benchmark and must not be used as slider latency.

The ignored real headless GPU/commit parity test currently fails:

```text
GPU/commit max=19/255 p99=6/255
required max<=2/255 p99<=1/255
```

This is a measured Phase-6 blocker, not hidden by the ordinary green test suite.

## Gates and unresolved inputs

The complete locked lib/integration run passes: 1222 passed, 0 failed, 5
ignored in the library target, with every ordinary integration target green.

- CIEDE2000: all 34 Sharma supplementary vectors, absolute error `<1e-4`.
- Reference extraction: 24 patches per image, 24 cameras, 240 raw/JPEG pairs,
  finite values, fixed orientation/order, archive and extracted-tree hashes
  recorded in provenance.
- Phase-0 golden aggregate rerun tolerance: at most `0.05` delta-E00.
- Provisional Phase-1 ColorChecker targets (to be finalized on a sensor chart):
  median delta-E00 `<=4`, mean `<=5`, p95 `<=10`, neutral mean `<=3`, median
  chromatic hue error `<=5` degrees.
- GPU/commit: max `<=2/255`, p99 `<=1/255`.
- Realtime on this machine: provisional fit-view slider-to-frame p95 `<50 ms`;
  it becomes blocking only after Phase 6 adds end-to-end event/present timing.

The historical iAi commits named in the reconstruction plan are absent from this
18-commit repository snapshot. `iAi-old` is therefore recorded as unavailable;
it must not be simulated. An archived executable or original Git history is
required to fill that comparator. A sensor RAW ColorChecker with recorded
illuminant is still required before signing camera-profile delta-E, although
Cube++ now closes the real-sensor known-illuminant/WB requirement.
