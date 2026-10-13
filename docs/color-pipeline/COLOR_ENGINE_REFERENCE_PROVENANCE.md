# Color Engine reference provenance

This record separates measured colour accuracy from resemblance to a camera's
consumer JPEG. Reference assets are external and are never committed to the
repository.

## ColorChecker Classic reference values

- Data: pre-November-2014 ColorChecker Classic 24 CIE L*a*b*, ICC D50, row-major.
- Upstream: Colour Science `chromaticity_coordinates.py`, which identifies the
  values as X-Rite's published 2015 data matching the 2005 chart.
- Upstream licence: BSD-3-Clause.
- URL: <https://colour.readthedocs.io/en/v0.4.4/_modules/colour/characterisation/datasets/colour_checkers/chromaticity_coordinates.html>
- Local use: numeric test/reference data in `core::color_reference`; no renderer
  constants or third-party processing code are imported.

The CIEDE2000 implementation is checked against all 34 supplementary vectors
published by Sharma, Wu, and Dalal at
<https://hajim.rochester.edu/ece/sites/gsharma/ciede2000/>.

## Middlebury registered ColorChecker dataset

- Dataset: `checker24s-RAW-JPG.zip`, smoothed registered 24-patch charts.
- Upstream: Middlebury Color Datasets, Chakrabarti, Scharstein, and Zickler.
- Permission: the dataset page explicitly grants permission to use and publish
  the images and requests citation of the BMVC 2009 paper.
- URL: <https://vision.middlebury.edu/color/data/>
- Archive SHA-256: `420534C8D56CFDD896241B2E96ED34E69DF8E6DE6A519E56489FE7DE7826DF74`.
- Extracted-tree fingerprint: `9C9193C91F4CD8A4BEC3051D21CE9F4E7B287A0587D17990D3BB883EA0DF54B3`.
  The harness hashes every relative path, byte length, and file byte in sorted
  order with the domain prefix `iai-reference-tree-sha256-v1`, so an intact
  archive cannot mask a modified extracted cache.
- Local cache: `target/color-reference-cache/` (ignored; not committed).
- Contents verified 2026-08-11: 24 camera directories, 240 dcraw-linear PNGs,
  240 camera-JPEG PNGs, and one RGB noise-standard-deviation text file per PNG.
  Every image is RGB8, 390x260, with a registered 6x4 grid of 65x65 patches.

The `raw.png` files are outputs rendered by dcraw into standard linear RGB. They
are not sensor RAW files and cannot measure iAi `RawImporter` accuracy. The
`jpg.png` files contain camera-JPEG code values in PNG containers and are decoded
under an explicit sRGB-transfer assumption. The Phase-0 harness therefore uses:

1. `d50_reference_dcraw_linear`: a D50 reference comparison after fitting only
   one scalar exposure from the six neutral patches;
2. `camera_jpeg_vs_d50_observation`: the same observation for the camera picture
   style, never an accuracy target; and
3. `iai_tone_vs_camera_jpeg`: a separate pairwise likeness score after one
   scalar fit from the four mid-neutral patches. A low score means only that the
   current tone resembles the camera picture style, not that either is accurate.

The harness reports source-pixel clipping separately from exposure-normalized
patch means that leave the nominal RGB cube. Those quantities must not be
conflated. It also writes aggregate JSON, summary CSV, and per-patch CSV when
`IAI_COLOR_REFERENCE_OUT` is set.

The controlled baseline uses `wb1i1e3`: fixed tungsten white balance, 3200 K
illumination, nominal exposure. Results retain a metamerism caveat because the
physical chart was photographed at 3200 K while the published reference is D50.

## Cube++ sensor-RAW illuminant fixture

- Dataset: Cube++, DOI `10.5281/zenodo.4153431`, CC BY 4.0.
- Record: <https://zenodo.org/records/4153431>.
- Technical description:
  <https://github.com/Visillect/CubePlusPlus/blob/master/description/description.md>.
- Source archive: `source_CR2_0.zip`, 13,796,343,517 bytes, Zenodo MD5
  `1f5dd086122ec5dee4463fbfe21105ef` (manifest verification; the full archive is
  not cached).
- Primary fixture: `20_2660.CR2`, Canon EOS 550D, ISO 100, daylight/natural,
  24,867,695 bytes, SHA-256
  `0D5DE5728CAC4855572ACC46B47B07C2598CAB5DF79B19207713F09BA23C2BDD`, ZIP
  CRC32 `12B7EC46`.
- The extracted entry was checked against the archive central-directory CRC32.
  It remains external under `target/color-reference-cache/` and is not committed.
- Published camera-RGB illuminant chromaticity (mean):
  `[0.2014899864, 0.4709663297, 0.3275436839]`; the two measured gray faces
  differ by `0.8782` degrees and both exceed the dataset's illuminance criterion.

The sensor probe registers the documented 2592x1728 camera-output rectangle
inside the decoder's active area, samples the two marked gray triangles before
WB/matrix, subtracts per-channel black, rejects saturated samples, and reports
recovery angular error. This is a valid camera-space illuminant/WB reference,
not a ColorChecker spectral reference: it cannot support camera-to-XYZ delta-E
claims. On the Phase-0 fixture, direct mosaic recovery is `0.0523` degrees from
the published mean, while embedded as-shot WB is `2.4337` degrees away and the
iAi no-JPEG-match scene leaves `2.4416`/`3.0544` degrees neutral error on the two
faces. Rawloader reports white level `15831`; Cube++ metadata reports `11767`, so
the decoder-level exposure disagreement is retained as a measured finding.

## Optional local DCP parser reference

- Profile: `Canon EOS 550D Adobe Standard.dcp`, 55,844 bytes.
- Source: the machine's installed Adobe Camera Raw camera-profile directory.
  This is a locally licensed proprietary asset, not a redistributable test
  fixture; it is never copied into the repository or cache.
- SHA-256:
  `4401EFB46ED414D6153FA37ADF93BF9E038F9314D42896826AB6CB07B4E09103`.
- Parsed identity: `UniqueCameraModel = Canon EOS 550D`, profile name
  `Adobe Standard`, dual illuminants Standard A (`17`) and D65 (`21`). The file
  has a creative look table but no technical HueSatMap; the Phase-1 technical
  transform deliberately does not apply that creative table.
- Harness: ignored `tests/dcp_reference_probe.rs`, driven only by
  `IAI_DCP_REFERENCE_FILE`; optional hash/model environment variables pin
  provenance. This verifies compatibility with a real external container, not
  permission to redistribute it and not ColorChecker accuracy.

## Remaining measured-reference gap

Cube++ closes the real sensor-input/known-illuminant gap for WB diagnostics.
There is still no sensor RAW ColorChecker/spectral reference in the corpus, so
camera-profile delta-E cannot be signed yet. The owner's local 20-file RAW corpus
is therefore used only for coverage, timing, and no-reference/camera-look
observations.
