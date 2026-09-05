# iAi Offline AI Retouch models

The application never downloads a model automatically for Auto Retouch. Put
the files below under `%APPDATA%\IAI\models\<directory>\` (or
`$HOME/.local/share/iai/models/` on Linux). Development and portable builds
also search `<executable>/models/` and `<working-directory>/models/`. Missing
files are reported in the AI panel and the pipeline continues with its
deterministic CPU image-retouch fallback.

Model binaries are deliberately excluded from the public Git repository: some
exceed GitHub's normal per-file limit, and every artifact remains governed by
its upstream terms. The repository publishes the locked hashes, export records
and adapter contracts needed to reproduce and audit them. A portable binary
bundle must also include `LICENSE`, `THIRD_PARTY.md`, and the complete
`licenses/` directory. GFPGAN's full upstream notice contains additional
third-party terms; review those terms before commercial model redistribution.

| Directory | File | Purpose | Source / license |
|---|---|---|---|
| `face-detector` | `face_detection_yunet_2026may.onnx` | face boxes + 5 landmarks | OpenCV Zoo YuNet; MIT |
| `bisenet` | `bisenet_face_parsing.onnx` | aligned 19-class masks | Official face-parsing checkpoint; MIT |
| `body-parsing` | `selfie_multiclass_256x256.onnx` | full-image background/hair/body-skin/clothes/other masks | Google MediaPipe SelfieMulticlass; Apache-2.0 |
| `nafnet` | `NAFNet-width32.onnx` | denoise | Official NAFNet-SIDD-width32 checkpoint; MIT |
| `iat` | `IAT.onnx` | colour/exposure | Official IAT exposure checkpoint; Apache-2.0 |
| `gfpgan` | `GFPGANv1.4.onnx` | face restoration | Tencent ARC GFPGAN; Apache-2.0 code / model terms |
| `realesrgan` | `RealESRGANv3-general-x4v3.onnx` | selective detail | xinntao Real-ESRGAN; BSD-3-Clause code / model terms |
| `realesrgan` | `RealESRGAN_x2plus.onnx` | optional final x2 upscale | xinntao Real-ESRGAN RRDBNet; BSD-3-Clause |
| `realesrgan` | `RealESRGAN_x4plus.onnx` | optional final x4 upscale | xinntao Real-ESRGAN RRDBNet; BSD-3-Clause |

`models/retouch-manifest.json` records the exact SHA-256 values for this release
bundle. Before a runner reports a model as available, the app streams the file
through SHA-256 and compares it with the locked export digest; validation is
cached by path, size and modification time. Tensor shapes/ranges are then
checked by the adapter during inference. The runner is lazy and stages are
unloaded before the next stage, so CPU-only machines do not keep every model in
memory.

## Current integration status

The current build has these active ONNX adapters. On Windows it first tries
the DirectML execution provider on the high-performance wgpu adapter; session
creation falls back to CPU per model when DirectML is unavailable or
incompatible. DirectML sessions use sequential graph execution and disable ORT
memory patterns as required by the provider.

- YuNet + BiSeNet face masks: YuNet consumes dynamic BGR float32 input in the
  OpenCV `blobFromImage` range, decodes stride 8/16/32 outputs, applies NMS and
  returns five landmarks per face. Every detected face is similarity-aligned to
  512x512 before the official 19-class BiSeNet runs. The masks for face/skin,
  hair, eyes+brows, lips and clothes near the aligned face are inverse-warped
  into the original image, with multi-face union by maximum weight. V2 unions
  skin/ear/nose/neck as Skin Extended, then applies region-specific
  expand/contract, resolution-aware feathering and colour-guided edge refine.
  Phase1-3 retains each face's similarity transform and six compressed ROI
  masks separately before forming the full-resolution float unions. The cache
  has a 384 MB process budget; oversized 24 MP masks remain valid for the
  current run but are not retained afterward.
  The YuNet artifact is
  from OpenCV Zoo commit `47534e27c9851bb1128ccc0102f1145e27f23f98`;
  BiSeNet is exported from upstream commit
  `d2e684cf1588b46145635e8fe7bcc29544e5537e` and its official full checkpoint.
  BiSeNet's worst PyTorch/ONNX Runtime max absolute error was `8.34e-6`.
- MediaPipe SelfieMulticlass full-image parsing: official 256x256 float32 model,
  converted to ONNX with NHWC RGB `[0,1]` input and six output classes:
  background, hair, body skin, face skin, clothes and other/accessories. These
  masks cover the whole canvas instead of the face-aligned crop. TFLite versus
  ONNX validation measured max absolute error `4.07e-5` and mean absolute error
  `6.04e-7`; source and ONNX SHA-256 hashes are locked in its export descriptor.
- IAT learned luminance + adaptive white balance: dynamic `[1,3,H,W]` float32
  RGB in `[0,1]` to the same shape and range. IAT's predicted RGB chroma is not
  pasted into the result: testing found that its exposure checkpoint could
  strengthen a pre-existing green cast. The adapter retains only the learned
  per-pixel luminance decision. A Gray-Edge + Shades-of-Gray illuminant
  estimator then computes explicit linear-RGB white-balance gains, while the
  semantic skin mask limits excess saturation. Four looks are available:
  Fresh (default), Natural, Warm and Cool. The benchmark records detected cast,
  R/G/B gains and confidence so color changes can be audited.
  `scripts/export_retouch_onnx.py` exports the official
  `best_Epoch_exposure.pth` from upstream commit
  `c76472265247f47cea57649af28b15018bb64cb1`. ONNX Runtime was checked against
  PyTorch on a deterministic tensor (max absolute error `8.94e-7`, mean
  absolute error `8.37e-8`) and on a second dynamic input size. The generated
  artifact SHA-256 is recorded in `models/retouch-manifest.json`.
- NAFNet-SIDD-width32 denoise: dynamic `[1,3,H,W]` float32 RGB in `[0,1]`,
  with H/W constrained to multiples of 16. The runtime uses 512px tiles,
  64px overlap, replicated edge padding and normalized feather weights. The
  Phase1-3 pipeline estimates source noise first and can attenuate or skip
  NAFNet for an already-clean image; disabling `Auto noise estimate` forces the
  selected bounded Denoise amount. Progress and cancellation are reported at
  tile boundaries. The
  exporter loads the official `params` checkpoint from upstream commit
  `2b4af71ebe098a92a75910c233a3965a3e93ede4` strictly. ONNX Runtime was checked
  at two sizes (worst max absolute error `6.56e-7`).
- Real-ESRGAN General x4v3 selective detail: dynamic float32 NCHW RGB input and
  a 4x output. The runtime expands the detail-mask ROI for context, processes
  256px tiles with 32px overlap, resizes each x4 result back to its source tile,
  normalizes feather weights, locally colour-matches each semantic ROI and only
  then applies the refined semantic/detail mask.
  The official `params` checkpoint from commit
  `a4abfb2979a7bbff3f69f58f58ae324608821e27` was loaded strictly; the worst
  PyTorch/ONNX Runtime max absolute error was `4.77e-6`.
- GFPGAN v1.4 face texture: fixed `[1,3,512,512]` float32 RGB input in
  `[-1,1]` and output in `[0,1]`, exported from the official `params_ema`
  checkpoint at commit `7552a7791caad982045a7bbe5634bbf1cd5c8679`.
  The adapter reuses YuNet's five-point alignment. It does not paste the whole
  generated face: it subtracts a low-pass copy from GFPGAN's output, transfers
  only that high-frequency texture to the aligned source crop, inverse-warps it
  and blends it through the semantic face masks. Protect Identity caps the
  blend amount. PyTorch/ONNX Runtime max absolute error was `4.47e-6`.
- IAT uses its native dynamic NCHW contract up to a 1536 px long edge. Larger
  images are reduced to a bounded preview for inference and the learned smooth
  luminance field is resized to source resolution. This prevents
  12/24 MP images from allocating full-resolution transformer activations.
- Real-ESRGAN RRDB x2/x4 final upscale: dynamic float32 NCHW models exported
  from the official `RealESRGAN_x2plus` and `RealESRGAN_x4plus` checkpoints
  using the BasicSR RRDBNet contract. Runtime processing is sequential in
  256px tiles with 32px overlap and cross-fades directly into one final RGBA
  buffer, avoiding full-size float accumulation buffers. The x2/x4 maximum
  PyTorch/ONNX Runtime errors were `1.73e-6` and `1.91e-6`. Output is preflighted
  against a 150-million-pixel safety limit; an unsafe request is skipped instead
  of allocating a full-frame Lanczos fallback.

The status reports every CPU fallback stage separately, and only names a neural
model after its checksum passed and its ONNX session completed successfully.
Every retouch stage/region has an independent checkbox. Disabled stages do not
load their model and do not modify their region, which allows a later run to
target only hair, clothes, face, eyes, lips, skin, denoise or colour/exposure.

This fallback is intentional: a missing, modified or incompatible model must
not crash the editor or discard the user's image. The release manifest and the
contracts above identify the exact artifacts validated for this build.
