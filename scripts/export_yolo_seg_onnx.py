#!/usr/bin/env python3
"""Export a standard Ultralytics YOLO11-seg model to ONNX for iAi's Select Subject.

iAi's YOLO decoder expects the canonical 640 segmentation export:
    input  "images"  : [1, 3, 640, 640]  float32, RGB, value/255 (letterboxed)
    output "output0" : [1, 116, 8400]     (4 box + 80 class + 32 mask coeff)
    output "output1" : [1, 32, 160, 160]  mask prototypes

Usage:
    pip install "ultralytics>=8.3" onnx onnxslim
    python scripts/export_yolo_seg_onnx.py

Then copy the produced `yolo11n-seg.onnx` to iAi's model folder as `yolo11-seg.onnx`:
    Windows : %APPDATA%\\IAI\\models\\yolo11-seg.onnx
    Linux   : ~/.local/share/iai/models/yolo11-seg.onnx

The app can also download a mirror automatically; this script is the reproducible
source of truth for the exact export contract. Ultralytics YOLO is AGPL-3.0, which
is compatible with iAi's AGPL-3.0-or-later license.
"""

from ultralytics import YOLO

# Swap for yolo11s-seg.pt / yolo11m-seg.pt for higher accuracy (larger file).
model = YOLO("yolo11n-seg.pt")
model.export(format="onnx", opset=12, imgsz=640, simplify=True)
print("Exported yolo11n-seg.onnx — copy it to the iAi models folder as yolo11-seg.onnx")
