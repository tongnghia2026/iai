# Third-party software and model notices

iAi source code is licensed under the GNU Affero General Public License v3.0
(or later) in [`LICENSE`](LICENSE).
Dependencies, runtime binaries, browser/API services, and AI model artifacts
remain under their own licenses and are not relicensed by iAi.

This inventory is based on the checked-in `Cargo.lock` and on the models the
published builds use. It is a practical distribution checklist, not legal
advice.

## Rust dependencies

Most of the Rust dependency graph is under MIT, Apache-2.0, BSD, ISC, Zlib,
Unicode, CC0, or another permissive license. The authoritative versions are
locked in `Cargo.lock`; run `cargo metadata --locked --format-version 1` to
inspect the complete resolved graph.

Two RAW-decoding crates declare LGPL-2.1:

| Component | Version | License | Upstream |
|---|---:|---|---|
| rawloader | 0.37.1 | LGPL-2.1 | https://github.com/pedrocr/rawloader |
| rawler | 0.7.2 | LGPL-2.1 | https://github.com/dnglab/dnglab |

iAi reaches them only through [`crates/iai-raw`](crates/iai-raw), a thin layer
over the two that is itself under `LGPL-2.1-or-later`. It is built one of two
ways:

- **A build of this repository** links `crates/iai-raw`, and the two crates
  with it, into the executable. That build is free software as a whole (AGPL);
  its corresponding source is this repository together with `Cargo.lock` and
  the Cargo build instructions in `README.md`.
- **The builds published on the Releases page** hold the Pro pack, which is
  not open source. They do not link the two crates or `crates/iai-raw` into
  `iai.exe`: they load them at run time from a file of its own beside it,
  `iai_raw.dll`, through the C interface described in
  [`crates/iai-raw-api`](crates/iai-raw-api) (`MIT`, text in its folder). Whoever
  has such a build may modify the library and put their own `iai_raw.dll` in
  place of the published one;
  [`crates/iai-raw/README.md`](crates/iai-raw/README.md) says how. The
  corresponding source of a published `iai_raw.dll` is `crates/iai-raw` and
  `crates/iai-raw-api` at the commit the release is tagged on, with the
  versions `Cargo.lock` pins.

Anyone redistributing either kind of build must keep this notice and the LGPL
text with it, and must not add terms that prohibit modifying the LGPL
components or reverse engineering for debugging such modifications.

The full LGPL-2.1 text is in [`licenses/LGPL-2.1.txt`](licenses/LGPL-2.1.txt).

## AI runtimes

| Component | License | Notice |
|---|---|---|
| ort Rust bindings | MIT OR Apache-2.0 | https://github.com/pykeio/ort |
| Microsoft ONNX Runtime | MIT | https://github.com/microsoft/onnxruntime |
| Microsoft DirectML | MIT | https://github.com/microsoft/DirectML |

The corresponding common license texts and DirectML copyright notice are in
`licenses/Apache-2.0.txt` and `licenses/DirectML-MIT.txt`.

## Model artifacts of the Pro pack

The Pro pack of the published builds (ID photo, portrait retouch; see
`README.md`) runs these models. Neural-network binaries are not in Git; each
user or binary distributor must obtain and distribute a model under its
upstream terms.

| Artifact | Upstream license | Local notice |
|---|---|---|
| OpenCV Zoo YuNet face detector | MIT (model-specific) | `licenses/YuNet-MIT.txt` |
| GFPGAN v1.4 | Apache-2.0 except upstream-listed third-party components | `licenses/GFPGAN.txt` |
| Real-ESRGAN General/x2/x4 | BSD-3-Clause | `licenses/Real-ESRGAN-BSD-3-Clause.txt` |
| MediaPipe Face Mesh V2 (face landmarker) | Apache-2.0 | `licenses/Apache-2.0.txt` |
| Meta Sapiens2 seg 0.4B | Sapiens2 License (use restrictions apply) | `licenses/Sapiens2-LICENSE.md` |

The Sapiens2 License must travel with any copy of the Sapiens2 weights and
forbids surveillance, biometric processing, identifying people and deepfakes.
GFPGAN's upstream license file contains additional licenses, including terms
for third-party components. Keep that complete file with any portable bundle.
The project does not assert that every converted checkpoint is cleared for
every commercial use; obtain permission or legal review before commercial
redistribution when the upstream weight terms are unclear.

## Models downloaded by other features

| Model | License / restriction | Use |
|---|---|---|
| LaMa / big-lama | Apache-2.0 upstream; verify the selected host's model card | Smart Fill / inpainting |
| BiRefNet Tiny | MIT | Select Subject (default) and Làm ảnh thẻ — foreground matte |
| YOLO11-seg | AGPL-3.0 (Ultralytics) — compatible with iAi's AGPL | Select Subject — object instance masks |

Both Select Subject models are commercial-safe: BiRefNet is MIT and YOLO11-seg is
AGPL (matching iAi's own license).

## Services and user credentials

Gemini, OpenAI, ChatGPT Web, and Gemini Web are external services governed by
their respective terms. API modes use the user's own key. Keys are stored in
the user's application-data directory and must never be committed.

## Binary distribution checklist

When publishing or sharing a portable build:

1. Include `LICENSE`, `THIRD_PARTY.md`, and the complete `licenses/` directory.
2. Identify the exact public source commit used for the executable.
3. Keep `Cargo.lock`, build instructions, and LGPL rebuild/relink rights
   available for as long as the binary is distributed.
4. Include each bundled model's source, hash, and license notice.
5. Do not publish API keys, user images, caches, checkpoints, or temporary
   export environments.
