# Third-party software and model notices

iAi source code is licensed under the MIT License in [`LICENSE`](LICENSE).
Dependencies, runtime binaries, browser/API services, and AI model artifacts
remain under their own licenses and are not relicensed by iAi.

This inventory is based on the checked-in `Cargo.lock` and the model hashes in
`models/retouch-manifest.json`. It is a practical distribution checklist, not
legal advice.

## Rust dependencies

Most of the Rust dependency graph is under MIT, Apache-2.0, BSD, ISC, Zlib,
Unicode, CC0, or another permissive license. The authoritative versions are
locked in `Cargo.lock`; run `cargo metadata --locked --format-version 1` to
inspect the complete resolved graph.

Two direct RAW-decoding crates declare LGPL-2.1:

| Component | Version | License | Upstream |
|---|---:|---|---|
| rawloader | 0.37.1 | LGPL-2.1 | https://github.com/pedrocr/rawloader |
| rawler | 0.7.2 | LGPL-2.1 | https://github.com/dnglab/dnglab |

Windows release builds link Rust dependencies into `iAi.exe`. Anyone
redistributing that executable must keep the LGPL notice, permit replacement
or modification of the LGPL components, and provide the complete
machine-readable source/build inputs needed to rebuild and relink the exact
binary. For official builds, the corresponding source is the tagged/committed
revision of https://github.com/tongnghia2026/iai together with `Cargo.lock` and
the Cargo build instructions in `README.md`. Do not add terms that prohibit
reverse engineering for debugging modifications to the LGPL components.

The full LGPL-2.1 text is in [`licenses/LGPL-2.1.txt`](licenses/LGPL-2.1.txt).

## AI runtimes

| Component | License | Notice |
|---|---|---|
| ort Rust bindings | MIT OR Apache-2.0 | https://github.com/pykeio/ort |
| Microsoft ONNX Runtime | MIT | https://github.com/microsoft/onnxruntime |
| Microsoft DirectML | MIT | https://github.com/microsoft/DirectML |

The corresponding common license texts and DirectML copyright notice are in
`licenses/Apache-2.0.txt` and `licenses/DirectML-MIT.txt`.

## Offline Auto Retouch model artifacts

Neural-network binaries are intentionally excluded from Git. The manifest and
export/validation descriptors are public, but each user or binary distributor
must obtain and distribute the model under its upstream terms.

| Artifact | Upstream license | Local notice |
|---|---|---|
| OpenCV Zoo YuNet face detector | MIT (model-specific) | `licenses/YuNet-MIT.txt` |
| BiSeNet face parsing | MIT | `licenses/BiSeNet-MIT.txt` |
| MediaPipe SelfieMulticlass | Apache-2.0 | `licenses/Apache-2.0.txt` |
| NAFNet-SIDD-width32 | MIT plus notices included by upstream | `licenses/NAFNet-MIT.txt` |
| IAT exposure checkpoint | Apache-2.0 | `licenses/Apache-2.0.txt` |
| GFPGAN v1.4 | Apache-2.0 except upstream-listed third-party components | `licenses/GFPGAN.txt` |
| Real-ESRGAN General/x2/x4 | BSD-3-Clause | `licenses/Real-ESRGAN-BSD-3-Clause.txt` |

GFPGAN's upstream license file contains additional licenses, including terms
for third-party components. Keep that complete file with any portable bundle.
The project does not assert that every converted checkpoint is cleared for
every commercial use; obtain permission or legal review before commercial
redistribution when the upstream weight terms are unclear.

Exact sources, commits, tensor contracts, and SHA-256 values are documented in
[`docs/AI_MODELS.md`](docs/AI_MODELS.md) and
[`models/retouch-manifest.json`](models/retouch-manifest.json).

## Models downloaded by other features

| Model | License / restriction | Use |
|---|---|---|
| LaMa / big-lama | Apache-2.0 upstream; verify the selected host's model card | Smart Fill / inpainting |
| CodeFormer | NTU S-Lab License 1.0; redistribution and commercial use require checking its terms | Face restoration |

CodeFormer is not covered by iAi's MIT license. Do not bundle it in a
commercial distribution without permission or a documented license review.

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
