"""Export Meta Sapiens2 seg 0.4B to the fixed 512x384 ONNX used by
Image > Chỉnh chân dung (portrait retouch).

Usage:
    python scripts/export_sapiens2_seg_onnx.py <checkpoint-dir> <out.onnx>

<checkpoint-dir> is a local copy of the Hugging Face repo
facebook/sapiens2-seg-0.4b (config.json + model.safetensors). Needs torch and
transformers (tested with transformers 5.17, torch 2.14 CPU) and
onnxruntime. The dynamo exporter is required: the legacy TorchScript
exporter fails on the model's InstanceNorm. The weights are governed by the
Sapiens2 License (licenses/Sapiens2-LICENSE.md).
"""

import sys
import time

import numpy as np
import onnxruntime as ort
import torch
from transformers import AutoModelForSemanticSegmentation


class Logits(torch.nn.Module):
    def __init__(self, model):
        super().__init__()
        self.model = model

    def forward(self, pixel_values):
        return self.model(pixel_values=pixel_values).logits


def main():
    source, out = sys.argv[1], sys.argv[2]
    model = AutoModelForSemanticSegmentation.from_pretrained(
        source, attn_implementation="eager"
    ).eval()
    wrapped = Logits(model).eval()
    sample = torch.randn(1, 3, 512, 384)
    with torch.inference_mode():
        reference = wrapped(sample).numpy()
    started = time.time()
    torch.onnx.export(
        wrapped,
        (sample,),
        out,
        input_names=["pixel_values"],
        output_names=["logits"],
        opset_version=18,
        dynamo=True,
    )
    print("export", round(time.time() - started, 1), "s")
    session = ort.InferenceSession(out, providers=["CPUExecutionProvider"])
    result = session.run(None, {"pixel_values": sample.numpy()})[0]
    print(
        "max abs diff",
        float(np.abs(result - reference).max()),
        "argmax agreement",
        float((result.argmax(1) == reference.argmax(1)).mean()),
    )


if __name__ == "__main__":
    main()
