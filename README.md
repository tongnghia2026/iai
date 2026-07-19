<div align="center">

<img src="logo_iAi.png" alt="iAi" width="320" />

# iAi

**A high-performance, GPU-accelerated image editor written in Rust.**

[![Rust](https://img.shields.io/badge/Rust-2021-orange.svg)](https://www.rust-lang.org/)
[![wgpu](https://img.shields.io/badge/wgpu-29-blue.svg)](https://wgpu.rs/)
[![License: MIT](https://img.shields.io/badge/license-MIT-green.svg)](LICENSE)

</div>

---

## Overview

iAi is a desktop raster image editor built from the ground up in Rust.

## Features

- **Layers** — raster & non-destructive adjustment layers, masks, blend modes, opacity
- **Painting** — brush, pencil, eraser, gradient, shapes, text; pressure-aware dab engine
- **Selection** — marquee, ellipse, lasso, polygonal lasso, wand, smart select, and a refine-selection edge brush
- **Retouch** — clone, repair brush, patch, smudge, dodge & burn, smart fill (content-aware) & warp
- **Adjustments** — levels, curves, hue/saturation, colour balance, exposure, black & white, photo filter, gradient map, channel mixer, and more
- **Develop** — a raw/raster develop panel: exposure, contrast, highlights/shadows/whites/blacks, tone curve, HSL colour mixer, detail & effects
- **Colour management** — ICC profiles, soft-proofing, display CMS, CMYK separations, print
- **Formats** — PNG, JPEG, TIFF, WebP, PSD, RAW and multi-page PDF import, PDF export, plus the native `.iai` project format
- **16-bit** import & Develop — RAW and 16-bit PNG/TIFF decode to a 16-bit master that survives Develop and the global adjustments (see the [bit-depth capability matrix](docs/bit-depth-and-color-capability.md))
- **GPU-accelerated** compositing (wgpu / WGSL)
- **AI** — subject/background masking, inpainting fill, face restoration, and a Gemini-powered retouch panel

## Installation

iAi is built from source with Cargo. The steps below take you from a clean machine to a running build on **Windows**, **macOS** and **Linux**, including installing Git and the Rust toolchain.

> **Requirements at a glance:** Git · Rust (stable, edition 2021) · a C/C++ toolchain · a GPU with Vulkan, Metal, DX12 or OpenGL support. The first use of an AI feature downloads its model (internet required).

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

> The first use of an **AI feature** (Select Subject / Smart Fill) downloads
> its ONNX model (tens of MB) to your local application-data directory. An internet
> connection is required for that first download only.

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
| Windowing       | `winit`                                       |
| UI              | `egui`, `egui-wgpu`, `egui-winit`, `egui-phosphor` |
| Image I/O       | `image`, `png`, `tiff`, `hayro` (PSD & `.iai` use native readers/writers) |
| Parallelism     | `rayon`                                       |
| AI inference    | `ort` (ONNX Runtime)                          |
| Colour mgmt     | `lcms2` (ICC / soft-proof)                    |
| RAW decode      | `rawloader`                                   |
| File dialogs    | `rfd`                                         |
| Networking      | `reqwest`, `tungstenite` (AI API & model downloads) |
| Text rasterizer | `ab_glyph`                                    |

## Project Structure

```
src/
├── app/       Application state, event loop, input, rendering, file ops
├── core/      Canvas, layers, selection, history, color, geometry
├── gpu/       wgpu state, compositor, WGSL shaders
├── tools/     Brush, eraser, selection, crop, clone, warp, text, …
├── formats/   PNG/JPEG/TIFF/WebP/PSD/RAW/PDF/iAi importers & exporters
├── ui/        Menubar, panels, toolbar, dialogs, overlays
└── extension/ Internal tool/filter trait seams (compile-time, not a plugin SDK)
```

## Contributing

Contributions are welcome. The codebase is organized around a stable rendering core, so new tools, filters, formats, UI improvements and bug fixes can be added without touching the engine internals. Please open an issue to discuss larger changes before submitting a pull request.

## License

Released under the [MIT License](LICENSE). © 2026 tongnghia2026.
