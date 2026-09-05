<div align="center">

<img src="logo_iAi.png" alt="iAi" width="320" />

# iAi

**A high-performance, GPU-accelerated raster & vector image editor written in Rust.**

[![Rust](https://img.shields.io/badge/Rust-2021-orange.svg)](https://www.rust-lang.org/)
[![wgpu](https://img.shields.io/badge/wgpu-29-blue.svg)](https://wgpu.rs/)
[![License: MIT](https://img.shields.io/badge/license-MIT-green.svg)](LICENSE)

</div>

---

## Overview

iAi is a desktop image editor built from the ground up in Rust. It pairs a
photo/raster engine (layers, painting, retouch, adjustments, RAW develop) with a
vector engine (Bézier paths, shapes, boolean shaping, PowerClip, connectors) on a
single GPU-accelerated canvas, so vector art and raster pixels live and render
together in the same document.

## Features

- **Layers** — raster and vector layers, non-destructive adjustment layers, masks, blend modes, opacity, groups
- **Painting** — brush, pencil, eraser, gradient, shapes, text; pressure-aware dab engine
- **Vector** — pen/Bézier paths, primitive shapes, node editing, boolean shaping (union / intersect / difference), outline & stroke, a vector brush, **PowerClip** (clip content into a frame), **connector** lines, and text converted to editable curves — all rendered on the GPU (Lyon tessellation + MSAA) alongside the raster content
- **Selection** — marquee, ellipse, lasso, polygonal lasso, wand, smart select, and a refine-selection edge brush
- **Retouch** — clone, repair brush, patch, smudge, dodge & burn, smart fill (content-aware) & warp
- **Adjustments** — levels, curves, hue/saturation, colour balance, exposure, black & white, photo filter, gradient map, channel mixer, and more
- **Develop** — a raw/raster develop panel: exposure, contrast, highlights/shadows/whites/blacks, tone curve, HSL colour mixer, detail & effects
- **Colour management** — ICC profiles, soft-proofing, display CMS, CMYK separations, print
- **Artboards & pages** — multi-artboard documents; import multi-page PDFs and manage their pages (insert blank / image / PDF pages, rename, reorder, delete), including clearing or repeating positioned text over a chosen consecutive page count and repeating images across every PDF page
- **Formats** — imports PNG, JPEG, TIFF, WebP, BMP, PSD, RAW (broad camera coverage, including Canon CR3) and multi-page PDF; exports PNG, JPEG, TIFF, WebP, BMP, multi-page PDF, and vector **SVG** (web / cut plotters), plus the native `.iai` project format (raster + vector, 16-bit)
- **16-bit** editing — RAW / 16-bit PNG / TIFF decode to a 16-bit master that survives Develop, global adjustments, the common raster edits (paint, fill, crop, flip, rotate, resize, merge, filters) and a `.iai` save/reopen round-trip; display is 8-bit-dithered (see the [bit-depth capability matrix](docs/bit-depth-and-color-capability.md))
- **GPU-accelerated** hybrid compositing (wgpu / WGSL) — raster tiles and tessellated vector geometry drawn on the same surface
- **AI** — offline Auto Retouch with independent face/hair/skin/eyes/lips/clothes stages, DirectML acceleration, semantic masks, denoise, restoration, upscale and automatic colour correction; plus subject masking, content-aware fill and Gemini/ChatGPT editing

## Installation

iAi is built from source with Cargo. The steps below take you from a clean machine to a running build on **Windows**, **macOS** and **Linux**, including installing Git and the Rust toolchain.

> **Requirements at a glance:** Git · Rust (stable, edition 2021) · a C/C++ toolchain · a GPU with Vulkan, Metal, DX12 or OpenGL support. Some AI features download a model on first use; offline Auto Retouch instead uses separately supplied, checksum-locked models described in [`docs/AI_MODELS.md`](docs/AI_MODELS.md).

> **Platform support.** Windows is the primary target — full CI (build + test) and the only one with a release build. macOS and Linux are **compile-checked in CI on every push** and build from the same source; they are not yet runtime-verified, so there are no prebuilt binaries for them yet.

### Windows

1. **Install Git** — either run one command in PowerShell, or download the installer.
   ```powershell
   winget install --id Git.Git -e
   ```
   (Or get it from <https://git-scm.com/download/win>.)

2. **Install the MSVC C++ build tools** (Rust's default toolchain links with MSVC):
   ```powershell
   winget install --id Microsoft.VisualStudio.2022.BuildTools -e
   ```
   In the Visual Studio Installer, enable the **“Desktop development with C++”** workload (it includes the MSVC compiler and the Windows SDK).

3. **Install Rust** via rustup:
   ```powershell
   winget install --id Rustlang.Rustup -e
   rustup default stable
   ```
   (Or download `rustup-init.exe` from <https://rustup.rs/>.)

4. Make sure your **GPU drivers** are up to date. No other system libraries are required.

> Restart your terminal after installing, so `git` and `cargo` are on your `PATH`.

### macOS

1. **Install the Xcode Command Line Tools** (this provides both **Git** and the **clang/Metal** toolchain):
   ```bash
   xcode-select --install
   ```

2. **Install Rust** via rustup:
   ```bash
   curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
   ```
   Then restart your terminal (or `source "$HOME/.cargo/env"`).

No additional libraries are needed — rendering uses Metal, which ships with macOS.

### Linux

Install Git, a C/C++ toolchain, and the windowing / Vulkan / file-dialog libraries, then install Rust.

**Debian / Ubuntu**
```bash
sudo apt update
sudo apt install -y git curl build-essential pkg-config \
    libxkbcommon-dev libwayland-dev \
    libx11-dev libxcursor-dev libxrandr-dev libxi-dev \
    libvulkan1 mesa-vulkan-drivers \
    xdg-desktop-portal xdg-desktop-portal-gtk
```

**Fedora**
```bash
sudo dnf install -y git curl gcc gcc-c++ make pkgconf-pkg-config \
    libxkbcommon-devel wayland-devel \
    libX11-devel libXcursor-devel libXrandr-devel libXi-devel \
    vulkan-loader mesa-vulkan-drivers \
    xdg-desktop-portal xdg-desktop-portal-gtk
```

**Arch Linux**
```bash
sudo pacman -S --needed git curl base-devel \
    libxkbcommon wayland \
    libx11 libxcursor libxrandr libxi \
    vulkan-icd-loader mesa \
    xdg-desktop-portal xdg-desktop-portal-gtk
```

Then **install Rust** via rustup (all distros):
```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source "$HOME/.cargo/env"
```

> Native Open/Save dialogs use the **XDG Desktop Portal**, so a portal backend
> (`xdg-desktop-portal-gtk`, `-kde`, or `-gnome`) must be installed and running.
> Pick the Vulkan driver that matches your GPU: `mesa-vulkan-drivers` for AMD/Intel,
> or the proprietary NVIDIA driver for NVIDIA cards.

### Build & Run (all platforms)

```bash
# Clone the repository
git clone https://github.com/tongnghia2026/iai.git
cd iai

# Build & run in release mode (strongly recommended for performance)
cargo run --release
```

The first build downloads and compiles all dependencies, so it can take a few minutes; later builds are incremental and fast.

### Update To The Latest Code

If you already cloned the repository and want to test the newest code:

```bash
cd iai
git status
git pull --rebase origin main
cargo fetch
cargo run --release
```

If your local copy has throwaway test changes and you want it to exactly match GitHub:

```bash
cd iai
git fetch origin
git checkout main
git reset --hard origin/main
cargo run --release
```

Use `cargo check` for a quick compile test, or `cargo test` before making a release build.

> The first use of some **AI features** (Select Subject / Smart Fill) downloads
> its ONNX model (tens of MB) to your local application-data directory. An internet
> connection is required for that first download only.

> **Offline Auto Retouch models are not stored in Git.** Model binaries exceed
> GitHub's ordinary file limit and retain their upstream licenses. The public
> repository contains their exact source revisions, tensor contracts and
> SHA-256 values in [`docs/AI_MODELS.md`](docs/AI_MODELS.md) and
> [`models/retouch-manifest.json`](models/retouch-manifest.json).

> **AI Gemini / ChatGPT panel.** The free *Web* mode drives your **own** logged-in
> Gemini or ChatGPT tab through the **IAI Bridge browser extension** (in
> [`extension/`](extension/README.md)) — no embedded web view, so it behaves the
> same on Windows, macOS and Linux. It attaches the canvas, fills the prompt,
> submits, and pulls the generated image back into a layer; see
> [`extension/README.md`](extension/README.md) to load it. Driving the chat web UI
> is unofficial; for a fully supported path use the paid **API** mode instead.

---

## Tech Stack

| Area            | Crate(s)                          |
| --------------- | --------------------------------- |
| Rendering       | `wgpu`, `bytemuck`                            |
| Vector geometry | `lyon` (tessellation), `i_overlay` (boolean ops) |
| Windowing       | `winit`                                       |
| UI              | `egui`, `egui-wgpu`, `egui-winit`, `egui-phosphor` |
| Image I/O       | `image` (PNG/JPEG/WebP/BMP), `png`, `tiff` (PSD & `.iai` use native readers/writers) |
| PDF             | `hayro` (render / import), `lopdf` (write)     |
| RAW decode      | `rawloader`, `rawler` (CR3 + wider camera DB) |
| Parallelism     | `rayon`                                       |
| AI inference    | `ort` (ONNX Runtime)                          |
| Colour mgmt     | `lcms2` (ICC / soft-proof)                    |
| File dialogs    | `rfd`                                         |
| Networking      | `reqwest`, `tungstenite` (AI API, bridge & model downloads) |
| Text rasterizer | `ab_glyph`                                    |

## Project Structure

```
src/
├── app/       Application state, event loop, input, rendering, file ops
├── core/      Canvas, layers, selection, history, colour, geometry, vector objects, SVG
├── gpu/       wgpu state, raster compositor, vector (Lyon/MSAA) renderer, WGSL shaders
├── tools/     Brush, eraser, selection, crop, clone, warp, text, pen, node, shape, gradient, …
├── formats/   PNG/JPEG/TIFF/WebP/BMP/PSD/RAW/PDF/SVG/iAi importers & exporters
├── ui/        Menubar, panels, toolbar, tab bar, dialogs, overlays
└── extension/ Internal tool/filter trait seams (compile-time, not a plugin SDK)
```

## Contributing

Contributions are welcome. The codebase is organized around a stable rendering core, so new tools, filters, formats, UI improvements and bug fixes can be added without touching the engine internals. For a bug, open a GitHub issue with reproduction steps, OS/GPU, the complete status message and a sample image that you have permission to share. Please discuss larger changes before submitting a pull request.

## License

The iAi source code is released under the [MIT License](LICENSE). © 2026
tongnghia2026. Linked dependencies, runtime binaries and AI models retain
their own terms; see [third-party notices](THIRD_PARTY.md) before distributing
a binary or model bundle.
