# Color pipeline test-image manifest

The committed Phase 0 corpus is synthetic and generated from numeric vectors, so it has no third-party copyright or license dependency.

| ID | Kind | Source/license | Purpose |
|---|---|---|---|
| neutral-ramp | Procedural RGB values | IAI project, MIT | Neutral-axis, exposure monotonicity |
| hue-patches | Procedural RGB values | IAI project, MIT | Red/orange/yellow/green/cyan/blue/magenta classification |
| signed-hdr | Procedural RGB values | IAI project, MIT | Negative-channel and HDR finite-output behavior |
| colorchecker-classic-d50 | 24 published CIE L*a*b* values, ICC D50 | X-Rite values distributed by Colour Science, BSD-3-Clause | ΔE00/ΔEOK, hue/chroma/lightness reference |
| middlebury-checker24s | External registered RGB8 chart images; cache only, not committed | Middlebury Color Datasets; permission to use/publish with BMVC 2009 citation | 24-camera D50 reference-comparison baseline, current-tone regression, and separately labelled camera-JPEG observation/likeness |
| cubepp-20_2660 | External Canon EOS 550D CR2 + SpyderCube annotations; cache only, not committed | Cube++ DOI 10.5281/zenodo.4153431, CC BY 4.0 | Real sensor decode, camera-linear illuminant angular error, embedded-WB and post-import neutral residual |
| local-canon-eos-550d-dcp | External Adobe Standard DCP from the machine's licensed Camera Raw installation; never copied or committed | Local Adobe installation/product terms; no redistribution claim | Ignored parser/provenance validation for the exact Cube++ camera model |

Middlebury, Cube++, and the optional local DCP hashes, transfer/registration
assumptions, and limitations are recorded in
`COLOR_ENGINE_REFERENCE_PROVENANCE.md`. Cube++
provides a real sensor RAW and known camera-space illuminant, but not a spectral
ColorChecker reference; camera-profile delta-E remains open. External photographs
and RAW files remain uncommitted. Before adding any, record the original
URL/owner, exact license, download date, file hash, illuminant/profile metadata,
and redistribution permission here.
