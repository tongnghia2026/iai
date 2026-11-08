<div align="center">

<img src="logo_iAi.png" alt="iAi" width="320" />

# iAi

**A high-performance, GPU-accelerated raster & vector image editor written in Rust.**

[![Rust](https://img.shields.io/badge/Rust-2024-orange.svg)](https://www.rust-lang.org/)
[![Release](https://img.shields.io/github/v/release/tongnghia2026/iai?include_prereleases&label=Windows%20build)](https://github.com/tongnghia2026/iai/releases)
[![wgpu](https://img.shields.io/badge/wgpu-29-blue.svg)](https://wgpu.rs/)
[![License: AGPL v3](https://img.shields.io/badge/license-AGPL--3.0--or--later-blue.svg)](LICENSE)

</div>

---

## Overview

iAi is a desktop image editor built from the ground up in Rust. It pairs a
photo/raster engine (layers, painting, retouch, adjustments, RAW develop) with a
vector engine (Bézier paths, shapes, boolean shaping, PowerClip, connectors) on a
single GPU-accelerated canvas, so vector art and raster pixels live and render
together in the same document.

Under the window sits a command engine: every edit on a document is a named
command that the menus, scripts and AI agents all call the same way. iAi can
run as an **MCP server** (`iai.exe mcp`) so an AI assistant can open, edit and
save documents through it, or run a script of commands headless (`iai.exe run`).

## Features

- **Layers** — raster and vector layers, non-destructive adjustment layers, masks, blend modes, opacity, groups
- **Painting** — brush, pencil, eraser, gradient, shapes, text; pressure-aware dab engine
- **Vector** — pen/Bézier paths, freehand line, primitive shapes, node editing with Corel-style Node / Move option bars, boolean shaping (union / intersect / difference, combine / break apart), outline & stroke, contour / offset path, a vector brush, **PowerClip** (clip content into a frame), **connector** lines, a gradient editor (linear / radial, colour stops on the canvas), text on a path and text converted to editable curves — all rendered on the GPU (Lyon tessellation + MSAA) alongside the raster content, and drawn sharp at any zoom
- **Design effects** — glow and shadow, blend, extrude, fade, envelope, grid / scatter / repeat around a centre, knife, rounded corners, distribute and align, and tables (merge cells, fill from Excel / CSV)
- **Text** — Type layers with system fonts, text that fits a frame, free transform
- **Selection** — marquee, ellipse, lasso, polygonal lasso, wand, smart select, and a refine-selection edge brush
- **Retouch** — clone, repair brush, patch, smudge, dodge & burn, smart fill (content-aware) & warp
- **Adjustments** — levels, curves, hue/saturation, colour balance, exposure, black & white, photo filter, gradient map, channel mixer, and more
- **Develop** — a raw/raster develop panel: exposure, contrast, highlights/shadows/whites/blacks, tone curve, HSL colour mixer, detail & effects
- **Colour management** — ICC profiles, soft-proofing, display CMS, CMYK separations, print
- **Artboards & pages** — multi-artboard documents; import multi-page PDFs and manage their pages (insert blank / image / PDF pages, rename, reorder, delete), including clearing or repeating positioned text over a chosen consecutive page count and repeating images across every PDF page
- **Formats** — imports PNG, JPEG, TIFF, WebP, BMP, PSD, RAW (broad camera coverage, including Canon CR3) and multi-page PDF; exports PNG, JPEG, TIFF, WebP, BMP, multi-page PDF, and vector **SVG** (web / cut plotters), plus the native `.iai` project format (raster + vector, 16-bit)
- **16-bit** editing — RAW / 16-bit PNG / TIFF decode to a 16-bit master that survives Develop, global adjustments, the common raster edits (paint, fill, crop, flip, rotate, resize, merge, filters) and a `.iai` save/reopen round-trip; display is 8-bit-dithered (see the [bit-depth capability matrix](docs/bit-depth-and-color-capability.md))
- **GPU-accelerated** hybrid compositing (wgpu / WGSL) — raster tiles and tessellated vector geometry drawn on the same surface
- **AI** — subject masking (Select Subject) and content-aware fill / repair on local ONNX models, with DirectML acceleration. The published builds add the **Pro pack** (see [below](#the-pro-pack))
- **Updates** — the Windows build checks this repository's [Releases](https://github.com/tongnghia2026/iai/releases) and offers each new version, with a short note of what changed
- **Flight recorder** — a journal of what was done and where the app stalled, kept on the user's machine, to find the cause of a freeze or crash

## Installation

**Windows users** can download a ready-made build from [Releases](https://github.com/tongnghia2026/iai/releases); it updates itself.

iAi is built from source with Cargo. The steps below take you from a clean machine to a running build on **Windows**, **macOS** and **Linux**, including installing Git and the Rust toolchain.

> **Requirements at a glance:** Git · Rust (stable, 1.85 or newer — edition 2024) · a C/C++ toolchain · a GPU with Vulkan, Metal, DX12 or OpenGL support. Some AI features download a model on first use.

> **Platform support.** Windows is the primary target — full CI (build + test) and the only one with a published build. macOS and Linux are **compile-checked in CI on every push** and build from the same source; they are not yet runtime-verified, so there are no prebuilt binaries for them yet.

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

Use `cargo check` for a quick compile test. Before a release build, run the tests of every crate:

```bash
cargo test --workspace --lib --no-default-features
cargo xtask lints
```

> The first use of some **AI features** (Select Subject / Smart Fill) downloads
> its ONNX model (tens of MB) to your local application-data directory. An internet
> connection is required for that first download only.

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
| RAW decode      | `rawloader`, `rawler` (CR3 + wider camera DB), through `crates/iai-raw` |
| Parallelism     | `rayon`                                       |
| AI inference    | `ort` (ONNX Runtime)                          |
| Colour mgmt     | `lcms2` (ICC / soft-proof)                    |
| File dialogs    | `rfd`                                         |
| Networking      | `reqwest` (updates & model downloads)         |
| Text rasterizer | `ab_glyph`                                    |

## Project Structure

The root is a Cargo workspace: the app is the root package, and the engine is
split into crates, each depending only on crates of its own layer or below
(checked by `cargo xtask layers`).

```
src/                 The app: window, event loop, input, tools, UI panels and dialogs
crates/
├── iai-geom/        Points, rectangles, transforms, units, snapping
├── iai-platform/    Memory, cores, disk, atomic file writes
├── iai-diag/        The flight recorder
├── iai-raster/      256-px copy-on-write tiles, blend modes, 8/16-bit
├── iai-color/       Colour spaces, ICC / CMYK (lcms2), soft-proof
├── iai-history/     Undo: commands, tile deltas, memory budget
├── iai-doc/         The document: layers, selection, pages, text and shape data
├── iai-text/        Fonts, Type layers, glyphs to paths
├── iai-vector/      Boolean ops, stroking, contour, curve fitting, display list
├── iai-imaging/     Filters, Refine Selection, Smart Select, warps, scan clean-up
├── iai-develop/     RAW / photo develop engines, camera profiles
├── iai-gpu/         wgpu compositor, vector renderer, GPU filters
├── iai-canvas/      A page's layers, selection and history with its pixels
├── iai-formats/     .iai, PNG, JPEG, TIFF, WebP, BMP, PSD, PDF, SVG, RAW
├── iai-layout/      Mail merge (CSV / Excel into templates)
├── iai-print/       Printing straight to a Windows printer
├── iai-ai/          Local ONNX models (DirectML with a CPU fallback)
├── iai-engine/      The command engine the GUI, scripts and agents call
├── iai-automation/  MCP over stdio, the bridge to a running app, permissions
├── iai-raw/         The RAW decoders (rawloader, rawler) behind one interface
└── iai-raw-api/     That interface: what a decoded RAW file is to the app
xtask/               cargo xtask: lints, layers, export, model bundles
docs/agent/          Guide for driving iAi from an AI agent (MCP)
```

## Contributing

Contributions are welcome. The codebase is organized around a stable rendering core, so new tools, filters, formats, UI improvements and bug fixes can be added without touching the engine internals. For a bug, open a GitHub issue with reproduction steps, OS/GPU, the complete status message and a sample image that you have permission to share. Please discuss larger changes before submitting a pull request.

## License

The iAi source code is released under the [GNU Affero General Public License
v3.0 or later](LICENSE). © 2026 tongnghia2026. You are free to use, modify,
sell and distribute it, provided that recipients — and users who interact with
a modified version over a network — can obtain the complete corresponding
source under the same license.

Two folders carry terms of their own: `crates/iai-raw`, the layer over the two
LGPL RAW decoders, is under `LGPL-2.1-or-later`, and `crates/iai-raw-api`, the
interface to it, under `MIT`.

Linked dependencies, runtime binaries and AI models retain their own terms;
see [third-party notices](THIRD_PARTY.md) before distributing a binary or model
bundle.

### Commercial license

iAi is also available under a separate **commercial license** for organizations
that cannot meet the AGPL's source-sharing requirements — for example, shipping
iAi inside a closed-source product. The commercial license is offered by the
copyright holder; open an issue on the repository to ask about terms.

Contributions are accepted under [CONTRIBUTING.md](CONTRIBUTING.md) and the
[Contributor License Agreement](CLA.md), which keep this dual-licensing option
available.

### The Pro pack

This repository is the core of iAi, whole: `cargo run --release` builds an
editor with everything listed above except the Pro pack.

The Pro pack ("Gói Pro") is, in the published builds: the **AI** window — ID
photos (cut out, framed from the eyes and chin, on white or blue), portrait
retouch, print sheets, ID cards sorted from a folder and laid out on A4, and
Gemini / ChatGPT editing such as a change of clothes; and **"Nhờ AI làm"**
(ask the AI), where an AI agent does a job in iAi for you — filling in forms
from ID cards, or redrawing a logo or a design from a picture as editable
vectors, with PDF / SVG / DXF vector import and EPS / CMYK export for print. It is a
separate work of the author, is not in this repository and is not released
under the AGPL. The published builds are made by the author from this source
together with the Pro pack; there it runs for a 7-day trial, then asks for a Pro
key: a one-time purchase, one key per machine, bought from inside the app
(Help ▸ "Gói Pro"). The rest of iAi stays free of charge.

In the source, the Pro pack is the `pro` Cargo feature: the places where the
core calls into it are marked `#[cfg(feature = "pro")]`, and the modules they
name are the ones that are not here.

The published builds do not link the two LGPL libraries that decode camera RAW
files into `iai.exe`: they load them from `iai_raw.dll`, a file beside it
whose source is [`crates/iai-raw`](crates/iai-raw) here, and which you may
replace with a build of your own (see that folder's README, and the
[third-party notices](THIRD_PARTY.md)).

## Support the project

If iAi is useful to you, you can support its development:

- **PayPal** — [paypal.me/TongNghia3999](https://paypal.me/TongNghia3999)
- **Bank transfer (ACB, Vietnam)** — account `18278961`, TONG VAN NGHIA

Buying a packaged build or a commercial license also directly supports the work.

## Contact

- **Email** — minhnghia718@gmail.com
- **Facebook** — https://www.facebook.com/minhnghia7189/
- **YouTube** — https://www.youtube.com/@TongNghia3999
