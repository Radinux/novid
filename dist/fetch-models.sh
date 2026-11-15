#!/bin/sh
# Downloads the ONNX models into models/ and checks them by SHA-256:
# YuNet and SFace from OpenCV Zoo, MiniFASNet (photo check for colour
# webcams) from yakhyo/face-anti-spoofing.
# Already present, valid files are kept.
set -eu
cd "$(dirname "$0")/.."

zoo="https://github.com/opencv/opencv_zoo/raw/main/models"
fas="https://github.com/yakhyo/face-anti-spoofing/releases/download/weights"
mkdir -p models
while read -r name url sha; do
  if [ -f "models/$name" ] && echo "$sha  models/$name" | sha256sum -c --status; then
    continue
  fi
  echo "==> Downloading model $name"
  curl -fL# --retry 3 -o "models/$name.part" "$url/$name"
  if ! echo "$sha  models/$name.part" | sha256sum -c --status; then
    rm -f "models/$name.part"
    echo "==> error: checksum mismatch for $name" >&2
    exit 1
  fi
  mv "models/$name.part" "models/$name"
done <<LIST
face_detection_yunet_2026may.onnx $zoo/face_detection_yunet ebafce4e3c118d6554634be5c27ab333b4c047a9a8c3faf1d7cf93101c22f0f0
face_recognition_sface_2021dec.onnx $zoo/face_recognition_sface 0ba9fbfa01b5270c96627c4ef784da859931e02f04419c829e83484087c34e79
MiniFASNetV2.onnx $fas b32929adc2d9c34b9486f8c4c7bc97c1b69bc0ea9befefc380e4faae4e463907
MiniFASNetV1SE.onnx $fas ebab7f90c7833fbccd46d3a555410e78d969db5438e169b6524be444862b3676
LIST
