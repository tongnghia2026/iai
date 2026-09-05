#!/usr/bin/env python3
"""Export the official retouch checkpoints to reproducible ONNX artifacts.

The exporter deliberately lives outside the application runtime.  It imports the
checked-out upstream source, loads the original checkpoint, writes one inference
output, then verifies ONNX Runtime against PyTorch on the same deterministic input.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import sys
import types
from pathlib import Path


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def portable_path(path: Path) -> str:
    """Record a reproducible path without publishing a developer home path."""
    resolved = path.resolve()
    try:
        return resolved.relative_to(Path.cwd().resolve()).as_posix()
    except ValueError:
        return resolved.name


def export_iat(source: Path, checkpoint: Path, output: Path) -> dict[str, object]:
    import numpy as np
    import onnx
    import onnxruntime as ort
    import torch

    # Python 3.12 removed ``imp``.  The upstream file imports it but never uses it.
    sys.modules.setdefault("imp", types.ModuleType("imp"))
    sys.path.insert(0, str(source.resolve()))
    try:
        from model.IAT_main import IAT
    finally:
        sys.path.pop(0)

    torch.manual_seed(20260904)
    model = IAT(type="exp").cpu().eval()
    state = torch.load(checkpoint, map_location="cpu", weights_only=True)
    if isinstance(state, dict) and "state_dict" in state:
        state = state["state_dict"]
    model.load_state_dict(state, strict=True)

    class EnhancedImageOnly(torch.nn.Module):
        def __init__(self, inner: torch.nn.Module) -> None:
            super().__init__()
            self.inner = inner

        def forward(self, image: torch.Tensor) -> torch.Tensor:
            return self.inner(image)[2].clamp(0.0, 1.0)

    wrapper = EnhancedImageOnly(model).eval()
    sample = torch.rand((1, 3, 256, 320), dtype=torch.float32)
    output.parent.mkdir(parents=True, exist_ok=True)
    with torch.inference_mode():
        reference = wrapper(sample).numpy()
        torch.onnx.export(
            wrapper,
            sample,
            output,
            input_names=["input"],
            output_names=["output"],
            dynamic_axes={
                "input": {2: "height", 3: "width"},
                "output": {2: "height", 3: "width"},
            },
            opset_version=17,
            do_constant_folding=True,
            dynamo=False,
        )

    graph = onnx.load(output)
    onnx.checker.check_model(graph, full_check=True)
    session = ort.InferenceSession(str(output), providers=["CPUExecutionProvider"])
    actual = session.run(["output"], {"input": sample.numpy()})[0]
    delta = np.abs(reference - actual)
    dynamic_sample = torch.rand((1, 3, 192, 288), dtype=torch.float32)
    dynamic_output = session.run(["output"], {"input": dynamic_sample.numpy()})[0]
    if dynamic_output.shape != tuple(dynamic_sample.shape):
        raise RuntimeError(
            f"dynamic-shape validation failed: {dynamic_output.shape} != {tuple(dynamic_sample.shape)}"
        )

    return {
        "model": "IAT exposure correction",
        "source": portable_path(source),
        "checkpoint": portable_path(checkpoint),
        "checkpoint_sha256": sha256(checkpoint),
        "onnx": portable_path(output),
        "onnx_sha256": sha256(output),
        "opset": 17,
        "input": {"name": "input", "dtype": "float32", "layout": "NCHW", "range": [0.0, 1.0]},
        "output": {"name": "output", "dtype": "float32", "layout": "NCHW", "range": [0.0, 1.0]},
        "dynamic_axes": ["height", "width"],
        "validation": {
            "provider": session.get_providers()[0],
            "max_abs_error": float(delta.max()),
            "mean_abs_error": float(delta.mean()),
            "shape": list(actual.shape),
            "dynamic_shape": list(dynamic_output.shape),
        },
    }


def export_nafnet(source: Path, checkpoint: Path, output: Path) -> dict[str, object]:
    """Export the official NAFNet-SIDD-width32 checkpoint.

    The small module definitions below preserve the upstream parameter names but
    express LayerNorm with ordinary tensor operators, which exports cleanly to
    ONNX. Architecture derived from megvii-research/NAFNet (MIT license).
    """
    import numpy as np
    import onnx
    import onnxruntime as ort
    import torch
    import torch.nn.functional as functional

    class LayerNorm2d(torch.nn.Module):
        def __init__(self, channels: int, eps: float = 1e-6) -> None:
            super().__init__()
            self.weight = torch.nn.Parameter(torch.ones(channels))
            self.bias = torch.nn.Parameter(torch.zeros(channels))
            self.eps = eps

        def forward(self, value: torch.Tensor) -> torch.Tensor:
            mean = value.mean(dim=1, keepdim=True)
            variance = (value - mean).square().mean(dim=1, keepdim=True)
            normalized = (value - mean) / (variance + self.eps).sqrt()
            return normalized * self.weight[None, :, None, None] + self.bias[None, :, None, None]

    class SimpleGate(torch.nn.Module):
        def forward(self, value: torch.Tensor) -> torch.Tensor:
            first, second = value.chunk(2, dim=1)
            return first * second

    class NAFBlock(torch.nn.Module):
        def __init__(self, channels: int) -> None:
            super().__init__()
            doubled = channels * 2
            self.conv1 = torch.nn.Conv2d(channels, doubled, 1)
            self.conv2 = torch.nn.Conv2d(doubled, doubled, 3, padding=1, groups=doubled)
            self.conv3 = torch.nn.Conv2d(channels, channels, 1)
            self.sca = torch.nn.Sequential(
                torch.nn.AdaptiveAvgPool2d(1), torch.nn.Conv2d(channels, channels, 1)
            )
            self.sg = SimpleGate()
            self.conv4 = torch.nn.Conv2d(channels, doubled, 1)
            self.conv5 = torch.nn.Conv2d(channels, channels, 1)
            self.norm1 = LayerNorm2d(channels)
            self.norm2 = LayerNorm2d(channels)
            self.dropout1 = torch.nn.Identity()
            self.dropout2 = torch.nn.Identity()
            self.beta = torch.nn.Parameter(torch.zeros((1, channels, 1, 1)))
            self.gamma = torch.nn.Parameter(torch.zeros((1, channels, 1, 1)))

        def forward(self, value: torch.Tensor) -> torch.Tensor:
            # Upstream simplified channel attention is applied after SimpleGate.
            gated = self.sg(self.conv2(self.conv1(self.norm1(value))))
            branch = self.conv3(gated * self.sca(gated))
            intermediate = value + self.dropout1(branch) * self.beta
            branch = self.conv5(self.sg(self.conv4(self.norm2(intermediate))))
            return intermediate + self.dropout2(branch) * self.gamma

    class NAFNet(torch.nn.Module):
        def __init__(self) -> None:
            super().__init__()
            width = 32
            encoder_blocks = [2, 2, 4, 8]
            decoder_blocks = [2, 2, 2, 2]
            self.intro = torch.nn.Conv2d(3, width, 3, padding=1)
            self.ending = torch.nn.Conv2d(width, 3, 3, padding=1)
            self.encoders = torch.nn.ModuleList()
            self.decoders = torch.nn.ModuleList()
            self.ups = torch.nn.ModuleList()
            self.downs = torch.nn.ModuleList()
            channels = width
            for count in encoder_blocks:
                self.encoders.append(torch.nn.Sequential(*(NAFBlock(channels) for _ in range(count))))
                self.downs.append(torch.nn.Conv2d(channels, channels * 2, 2, stride=2))
                channels *= 2
            self.middle_blks = torch.nn.Sequential(*(NAFBlock(channels) for _ in range(12)))
            for count in decoder_blocks:
                self.ups.append(
                    torch.nn.Sequential(
                        torch.nn.Conv2d(channels, channels * 2, 1, bias=False),
                        torch.nn.PixelShuffle(2),
                    )
                )
                channels //= 2
                self.decoders.append(torch.nn.Sequential(*(NAFBlock(channels) for _ in range(count))))

        def forward(self, image: torch.Tensor) -> torch.Tensor:
            height, width = image.shape[2], image.shape[3]
            pad_height = (16 - height % 16) % 16
            pad_width = (16 - width % 16) % 16
            padded = functional.pad(image, (0, pad_width, 0, pad_height))
            value = self.intro(padded)
            skips = []
            for encoder, downsample in zip(self.encoders, self.downs):
                value = encoder(value)
                skips.append(value)
                value = downsample(value)
            value = self.middle_blks(value)
            for decoder, upsample, skip in zip(self.decoders, self.ups, reversed(skips)):
                value = decoder(upsample(value) + skip)
            return (self.ending(value) + padded)[:, :, :height, :width].clamp(0.0, 1.0)

    torch.manual_seed(20260904)
    model = NAFNet().cpu().eval()
    state = torch.load(checkpoint, map_location="cpu", weights_only=True)
    if isinstance(state, dict):
        state = state.get("params_ema", state.get("params", state))
    model.load_state_dict(state, strict=True)

    # Export with a multiple-of-16 sample. The runtime tile adapter guarantees
    # this invariant, while dynamic H/W lets edge tiles use smaller padded sizes.
    sample = torch.rand((1, 3, 64, 80), dtype=torch.float32)
    output.parent.mkdir(parents=True, exist_ok=True)
    with torch.inference_mode():
        reference = model(sample).numpy()
        torch.onnx.export(
            model,
            sample,
            output,
            input_names=["input"],
            output_names=["output"],
            dynamic_axes={
                "input": {2: "height_multiple_16", 3: "width_multiple_16"},
                "output": {2: "height_multiple_16", 3: "width_multiple_16"},
            },
            opset_version=17,
            do_constant_folding=True,
            dynamo=False,
        )

    graph = onnx.load(output)
    onnx.checker.check_model(graph, full_check=True)
    session = ort.InferenceSession(str(output), providers=["CPUExecutionProvider"])
    actual = session.run(["output"], {"input": sample.numpy()})[0]
    delta = np.abs(reference - actual)
    dynamic_sample = torch.rand((1, 3, 80, 96), dtype=torch.float32)
    with torch.inference_mode():
        dynamic_reference = model(dynamic_sample).numpy()
    dynamic_output = session.run(["output"], {"input": dynamic_sample.numpy()})[0]
    dynamic_delta = np.abs(dynamic_reference - dynamic_output)

    return {
        "model": "NAFNet-SIDD-width32",
        "source": portable_path(source),
        "checkpoint": portable_path(checkpoint),
        "checkpoint_sha256": sha256(checkpoint),
        "onnx": portable_path(output),
        "onnx_sha256": sha256(output),
        "opset": 17,
        "input": {"name": "input", "dtype": "float32", "layout": "NCHW", "range": [0.0, 1.0]},
        "output": {"name": "output", "dtype": "float32", "layout": "NCHW", "range": [0.0, 1.0]},
        "dynamic_axes": ["height_multiple_16", "width_multiple_16"],
        "validation": {
            "provider": session.get_providers()[0],
            "max_abs_error": float(delta.max()),
            "mean_abs_error": float(delta.mean()),
            "dynamic_max_abs_error": float(dynamic_delta.max()),
            "shape": list(actual.shape),
            "dynamic_shape": list(dynamic_output.shape),
        },
    }


def export_realesrgan_general(source: Path, checkpoint: Path, output: Path) -> dict[str, object]:
    """Export the official Real-ESRGAN General x4v3 compact checkpoint."""
    import numpy as np
    import onnx
    import onnxruntime as ort
    import torch
    import torch.nn.functional as functional

    class SRVGGNetCompact(torch.nn.Module):
        def __init__(self) -> None:
            super().__init__()
            features = 64
            upscale = 4
            self.upscale = upscale
            self.body = torch.nn.ModuleList(
                [torch.nn.Conv2d(3, features, 3, padding=1), torch.nn.PReLU(features)]
            )
            for _ in range(32):
                self.body.append(torch.nn.Conv2d(features, features, 3, padding=1))
                self.body.append(torch.nn.PReLU(features))
            self.body.append(torch.nn.Conv2d(features, 3 * upscale * upscale, 3, padding=1))
            self.upsampler = torch.nn.PixelShuffle(upscale)

        def forward(self, image: torch.Tensor) -> torch.Tensor:
            value = image
            for layer in self.body:
                value = layer(value)
            value = self.upsampler(value)
            base = functional.interpolate(image, scale_factor=self.upscale, mode="nearest")
            return (value + base).clamp(0.0, 1.0)

    torch.manual_seed(20260904)
    model = SRVGGNetCompact().cpu().eval()
    state = torch.load(checkpoint, map_location="cpu", weights_only=True)
    if isinstance(state, dict):
        state = state.get("params_ema", state.get("params", state))
    model.load_state_dict(state, strict=True)

    sample = torch.rand((1, 3, 48, 64), dtype=torch.float32)
    output.parent.mkdir(parents=True, exist_ok=True)
    with torch.inference_mode():
        reference = model(sample).numpy()
        torch.onnx.export(
            model,
            sample,
            output,
            input_names=["input"],
            output_names=["output"],
            dynamic_axes={
                "input": {2: "height", 3: "width"},
                "output": {2: "height_x4", 3: "width_x4"},
            },
            opset_version=17,
            do_constant_folding=True,
            dynamo=False,
        )

    graph = onnx.load(output)
    onnx.checker.check_model(graph, full_check=True)
    session = ort.InferenceSession(str(output), providers=["CPUExecutionProvider"])
    actual = session.run(["output"], {"input": sample.numpy()})[0]
    delta = np.abs(reference - actual)
    dynamic_sample = torch.rand((1, 3, 40, 56), dtype=torch.float32)
    dynamic_output = session.run(["output"], {"input": dynamic_sample.numpy()})[0]
    if dynamic_output.shape != (1, 3, 160, 224):
        raise RuntimeError(f"Real-ESRGAN dynamic shape mismatch: {dynamic_output.shape}")

    return {
        "model": "realesr-general-x4v3",
        "source": portable_path(source),
        "checkpoint": portable_path(checkpoint),
        "checkpoint_sha256": sha256(checkpoint),
        "onnx": portable_path(output),
        "onnx_sha256": sha256(output),
        "opset": 17,
        "input": {"name": "input", "dtype": "float32", "layout": "NCHW", "range": [0.0, 1.0]},
        "output": {
            "name": "output",
            "dtype": "float32",
            "layout": "NCHW",
            "range": [0.0, 1.0],
            "scale": 4,
        },
        "dynamic_axes": ["height", "width"],
        "validation": {
            "provider": session.get_providers()[0],
            "max_abs_error": float(delta.max()),
            "mean_abs_error": float(delta.mean()),
            "shape": list(actual.shape),
            "dynamic_shape": list(dynamic_output.shape),
        },
    }


def export_bisenet(source: Path, checkpoint: Path, output: Path) -> dict[str, object]:
    """Export the official 19-class face-parsing checkpoint at aligned 512x512."""
    import numpy as np
    import onnx
    import onnxruntime as ort
    import torch

    sys.path.insert(0, str(source.resolve()))
    try:
        from model import BiSeNet
    finally:
        sys.path.pop(0)

    torch.manual_seed(20260904)
    # Upstream initializes the ResNet backbone by downloading ImageNet weights.
    # The face-parsing checkpoint below contains the full network, so suppress
    # that redundant network request and immediately load every tensor strictly.
    import torch.utils.model_zoo as model_zoo

    original_load_url = model_zoo.load_url
    model_zoo.load_url = lambda *_args, **_kwargs: {}
    try:
        model = BiSeNet(n_classes=19).cpu().eval()
    finally:
        model_zoo.load_url = original_load_url
    state = torch.load(checkpoint, map_location="cpu", weights_only=True)
    if isinstance(state, dict) and "state_dict" in state:
        state = state["state_dict"]
    model.load_state_dict(state, strict=True)

    class PrimaryLogitsOnly(torch.nn.Module):
        def __init__(self, inner: torch.nn.Module) -> None:
            super().__init__()
            self.inner = inner

        def forward(self, image: torch.Tensor) -> torch.Tensor:
            return self.inner(image)[0]

    wrapper = PrimaryLogitsOnly(model).eval()
    sample = torch.rand((1, 3, 512, 512), dtype=torch.float32)
    output.parent.mkdir(parents=True, exist_ok=True)
    with torch.inference_mode():
        reference = wrapper(sample).numpy()
        torch.onnx.export(
            wrapper,
            sample,
            output,
            input_names=["input"],
            output_names=["logits"],
            opset_version=17,
            do_constant_folding=True,
            dynamo=False,
        )

    graph = onnx.load(output)
    onnx.checker.check_model(graph, full_check=True)
    session = ort.InferenceSession(str(output), providers=["CPUExecutionProvider"])
    actual = session.run(["logits"], {"input": sample.numpy()})[0]
    delta = np.abs(reference - actual)
    if actual.shape != (1, 19, 512, 512):
        raise RuntimeError(f"BiSeNet output shape mismatch: {actual.shape}")

    return {
        "model": "BiSeNet 19-class face parsing",
        "source": portable_path(source),
        "checkpoint": portable_path(checkpoint),
        "checkpoint_sha256": sha256(checkpoint),
        "onnx": portable_path(output),
        "onnx_sha256": sha256(output),
        "opset": 17,
        "input": {
            "name": "input",
            "dtype": "float32",
            "layout": "NCHW",
            "size": [512, 512],
            "normalization": "ImageNet RGB",
        },
        "output": {
            "name": "logits",
            "dtype": "float32",
            "layout": "NCHW",
            "shape": [1, 19, 512, 512],
        },
        "validation": {
            "provider": session.get_providers()[0],
            "max_abs_error": float(delta.max()),
            "mean_abs_error": float(delta.mean()),
        },
    }


def export_gfpgan(source: Path, checkpoint: Path, output: Path) -> dict[str, object]:
    """Export GFPGAN v1.4 clean architecture for one aligned 512px face."""
    import importlib.util
    import numpy as np
    import onnx
    import onnxruntime as ort
    import torch

    class NoOpRegistry:
        def register(self):
            return lambda model_class: model_class

    # Load only the two official architecture files. The normal package import
    # also imports training/data helpers and native OpenCV dependencies that are
    # irrelevant to an offline ONNX export.
    basicsr = types.ModuleType("basicsr")
    basicsr_archs = types.ModuleType("basicsr.archs")
    basicsr_arch_util = types.ModuleType("basicsr.archs.arch_util")
    basicsr_arch_util.default_init_weights = lambda *_args, **_kwargs: None
    basicsr_utils = types.ModuleType("basicsr.utils")
    basicsr_registry = types.ModuleType("basicsr.utils.registry")
    basicsr_registry.ARCH_REGISTRY = NoOpRegistry()
    gfpgan_package = types.ModuleType("gfpgan")
    gfpgan_archs = types.ModuleType("gfpgan.archs")
    gfpgan_package.__path__ = [str((source / "gfpgan").resolve())]
    gfpgan_archs.__path__ = [str((source / "gfpgan" / "archs").resolve())]
    sys.modules.update(
        {
            "basicsr": basicsr,
            "basicsr.archs": basicsr_archs,
            "basicsr.archs.arch_util": basicsr_arch_util,
            "basicsr.utils": basicsr_utils,
            "basicsr.utils.registry": basicsr_registry,
            "gfpgan": gfpgan_package,
            "gfpgan.archs": gfpgan_archs,
        }
    )

    def load_architecture(name: str, path: Path):
        spec = importlib.util.spec_from_file_location(name, path)
        if spec is None or spec.loader is None:
            raise RuntimeError(f"cannot load architecture module {path}")
        module = importlib.util.module_from_spec(spec)
        sys.modules[name] = module
        spec.loader.exec_module(module)
        return module

    load_architecture(
        "gfpgan.archs.stylegan2_clean_arch",
        source / "gfpgan" / "archs" / "stylegan2_clean_arch.py",
    )
    architecture = load_architecture(
        "gfpgan.archs.gfpganv1_clean_arch",
        source / "gfpgan" / "archs" / "gfpganv1_clean_arch.py",
    )
    model = architecture.GFPGANv1Clean(
        out_size=512,
        num_style_feat=512,
        channel_multiplier=2,
        decoder_load_path=None,
        fix_decoder=False,
        num_mlp=8,
        input_is_latent=True,
        different_w=True,
        narrow=1,
        sft_half=True,
    ).cpu().eval()
    state = torch.load(checkpoint, map_location="cpu", weights_only=True)
    if isinstance(state, dict):
        state = state.get("params_ema", state.get("params", state))
    model.load_state_dict(state, strict=True)

    class RestoredFaceOnly(torch.nn.Module):
        def __init__(self, inner: torch.nn.Module) -> None:
            super().__init__()
            self.inner = inner

        def forward(self, image: torch.Tensor) -> torch.Tensor:
            restored = self.inner(
                image,
                return_rgb=False,
                randomize_noise=False,
            )[0]
            return ((restored + 1.0) * 0.5).clamp(0.0, 1.0)

    wrapper = RestoredFaceOnly(model).eval()
    # The application adapter normalizes aligned RGB from [0,1] to [-1,1].
    sample = torch.rand((1, 3, 512, 512), dtype=torch.float32) * 2.0 - 1.0
    output.parent.mkdir(parents=True, exist_ok=True)
    with torch.inference_mode():
        reference = wrapper(sample).numpy()
        torch.onnx.export(
            wrapper,
            sample,
            output,
            input_names=["input"],
            output_names=["output"],
            opset_version=17,
            do_constant_folding=True,
            dynamo=False,
        )

    graph = onnx.load(output)
    onnx.checker.check_model(graph, full_check=True)
    session = ort.InferenceSession(str(output), providers=["CPUExecutionProvider"])
    actual = session.run(["output"], {"input": sample.numpy()})[0]
    delta = np.abs(reference - actual)
    if actual.shape != (1, 3, 512, 512):
        raise RuntimeError(f"GFPGAN output shape mismatch: {actual.shape}")

    return {
        "model": "GFPGAN v1.4",
        "source": portable_path(source),
        "checkpoint": portable_path(checkpoint),
        "checkpoint_sha256": sha256(checkpoint),
        "onnx": portable_path(output),
        "onnx_sha256": sha256(output),
        "opset": 17,
        "input": {
            "name": "input",
            "dtype": "float32",
            "layout": "NCHW",
            "shape": [1, 3, 512, 512],
            "range": [-1.0, 1.0],
        },
        "output": {
            "name": "output",
            "dtype": "float32",
            "layout": "NCHW",
            "shape": [1, 3, 512, 512],
            "range": [0.0, 1.0],
        },
        "validation": {
            "provider": session.get_providers()[0],
            "max_abs_error": float(delta.max()),
            "mean_abs_error": float(delta.mean()),
        },
    }


def export_realesrgan_rrdb(
    source: Path, checkpoint: Path, output: Path, scale: int
) -> dict[str, object]:
    """Export official Real-ESRGAN RRDBNet x2 or x4 checkpoint."""
    import numpy as np
    import onnx
    import onnxruntime as ort
    import torch
    import torch.nn.functional as functional

    if scale not in (2, 4):
        raise ValueError("RRDB scale must be 2 or 4")

    class ResidualDenseBlock(torch.nn.Module):
        def __init__(self, features: int = 64, growth: int = 32) -> None:
            super().__init__()
            self.conv1 = torch.nn.Conv2d(features, growth, 3, padding=1)
            self.conv2 = torch.nn.Conv2d(features + growth, growth, 3, padding=1)
            self.conv3 = torch.nn.Conv2d(features + growth * 2, growth, 3, padding=1)
            self.conv4 = torch.nn.Conv2d(features + growth * 3, growth, 3, padding=1)
            self.conv5 = torch.nn.Conv2d(features + growth * 4, features, 3, padding=1)
            self.lrelu = torch.nn.LeakyReLU(0.2, inplace=True)

        def forward(self, value: torch.Tensor) -> torch.Tensor:
            first = self.lrelu(self.conv1(value))
            second = self.lrelu(self.conv2(torch.cat((value, first), dim=1)))
            third = self.lrelu(self.conv3(torch.cat((value, first, second), dim=1)))
            fourth = self.lrelu(self.conv4(torch.cat((value, first, second, third), dim=1)))
            fifth = self.conv5(torch.cat((value, first, second, third, fourth), dim=1))
            return fifth * 0.2 + value

    class RRDB(torch.nn.Module):
        def __init__(self, num_feat: int = 64, num_grow_ch: int = 32) -> None:
            super().__init__()
            self.rdb1 = ResidualDenseBlock(num_feat, num_grow_ch)
            self.rdb2 = ResidualDenseBlock(num_feat, num_grow_ch)
            self.rdb3 = ResidualDenseBlock(num_feat, num_grow_ch)

        def forward(self, value: torch.Tensor) -> torch.Tensor:
            return self.rdb3(self.rdb2(self.rdb1(value))) * 0.2 + value

    def pixel_unshuffle(value: torch.Tensor, factor: int) -> torch.Tensor:
        batch, channels, height, width = value.shape
        value = value.view(
            batch, channels, height // factor, factor, width // factor, factor
        )
        return value.permute(0, 1, 3, 5, 2, 4).reshape(
            batch, channels * factor * factor, height // factor, width // factor
        )

    class RRDBNet(torch.nn.Module):
        def __init__(self, upscale: int) -> None:
            super().__init__()
            self.scale = upscale
            input_channels = 12 if upscale == 2 else 3
            self.conv_first = torch.nn.Conv2d(input_channels, 64, 3, padding=1)
            self.body = torch.nn.Sequential(*(RRDB() for _ in range(23)))
            self.conv_body = torch.nn.Conv2d(64, 64, 3, padding=1)
            self.conv_up1 = torch.nn.Conv2d(64, 64, 3, padding=1)
            self.conv_up2 = torch.nn.Conv2d(64, 64, 3, padding=1)
            self.conv_hr = torch.nn.Conv2d(64, 64, 3, padding=1)
            self.conv_last = torch.nn.Conv2d(64, 3, 3, padding=1)
            self.lrelu = torch.nn.LeakyReLU(0.2, inplace=True)

        def forward(self, image: torch.Tensor) -> torch.Tensor:
            features = pixel_unshuffle(image, 2) if self.scale == 2 else image
            features = self.conv_first(features)
            features = features + self.conv_body(self.body(features))
            features = self.lrelu(
                self.conv_up1(functional.interpolate(features, scale_factor=2, mode="nearest"))
            )
            features = self.lrelu(
                self.conv_up2(functional.interpolate(features, scale_factor=2, mode="nearest"))
            )
            return self.conv_last(self.lrelu(self.conv_hr(features))).clamp(0.0, 1.0)

    torch.manual_seed(20260904)
    model = RRDBNet(scale).cpu().eval()
    state = torch.load(checkpoint, map_location="cpu", weights_only=True)
    if isinstance(state, dict):
        state = state.get("params_ema", state.get("params", state))
    model.load_state_dict(state, strict=True)

    sample = torch.rand((1, 3, 32, 40), dtype=torch.float32)
    output.parent.mkdir(parents=True, exist_ok=True)
    with torch.inference_mode():
        reference = model(sample).numpy()
        torch.onnx.export(
            model,
            sample,
            output,
            input_names=["input"],
            output_names=["output"],
            dynamic_axes={
                "input": {2: "height", 3: "width"},
                "output": {2: f"height_x{scale}", 3: f"width_x{scale}"},
            },
            opset_version=17,
            do_constant_folding=True,
            dynamo=False,
        )

    graph = onnx.load(output)
    onnx.checker.check_model(graph, full_check=True)
    session = ort.InferenceSession(str(output), providers=["CPUExecutionProvider"])
    actual = session.run(["output"], {"input": sample.numpy()})[0]
    delta = np.abs(reference - actual)
    expected_shape = (1, 3, 32 * scale, 40 * scale)
    if actual.shape != expected_shape:
        raise RuntimeError(f"RRDB x{scale} output mismatch: {actual.shape} != {expected_shape}")

    return {
        "model": f"RealESRGAN_x{scale}plus RRDBNet",
        "source": portable_path(source),
        "checkpoint": portable_path(checkpoint),
        "checkpoint_sha256": sha256(checkpoint),
        "onnx": portable_path(output),
        "onnx_sha256": sha256(output),
        "opset": 17,
        "input": {"name": "input", "dtype": "float32", "layout": "NCHW", "range": [0.0, 1.0]},
        "output": {
            "name": "output",
            "dtype": "float32",
            "layout": "NCHW",
            "range": [0.0, 1.0],
            "scale": scale,
        },
        "validation": {
            "provider": session.get_providers()[0],
            "max_abs_error": float(delta.max()),
            "mean_abs_error": float(delta.mean()),
            "shape": list(actual.shape),
        },
    }


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument(
        "model",
        choices=[
            "iat",
            "nafnet",
            "realesrgan-general",
            "bisenet",
            "gfpgan",
            "realesrgan-rrdb",
        ],
    )
    parser.add_argument("--source", type=Path, required=True)
    parser.add_argument("--checkpoint", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--report", type=Path)
    parser.add_argument("--scale", type=int, choices=[2, 4])
    args = parser.parse_args()

    if args.model == "iat":
        report = export_iat(args.source, args.checkpoint, args.output)
    elif args.model == "nafnet":
        report = export_nafnet(args.source, args.checkpoint, args.output)
    elif args.model == "realesrgan-general":
        report = export_realesrgan_general(args.source, args.checkpoint, args.output)
    elif args.model == "bisenet":
        report = export_bisenet(args.source, args.checkpoint, args.output)
    elif args.model == "gfpgan":
        report = export_gfpgan(args.source, args.checkpoint, args.output)
    elif args.model == "realesrgan-rrdb":
        if args.scale is None:
            parser.error("--scale is required for realesrgan-rrdb")
        report = export_realesrgan_rrdb(args.source, args.checkpoint, args.output, args.scale)
    else:  # pragma: no cover - argparse prevents this branch.
        raise ValueError(args.model)

    encoded = json.dumps(report, indent=2, ensure_ascii=False)
    if args.report:
        args.report.parent.mkdir(parents=True, exist_ok=True)
        args.report.write_text(encoded + "\n", encoding="utf-8")
    print(encoded)


if __name__ == "__main__":
    main()
