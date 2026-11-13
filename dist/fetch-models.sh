#!/bin/sh
# Downloads the ONNX models (OpenCV Zoo) into models/ and checks them by SHA-256.
# Already present, valid files are kept.
set -eu
cd "$(dirname "$0")/.."

base="https://github.com/opencv/opencv_zoo/raw/main/models"
mkdir -p models
while read -r sub name sha; do
  if [ -f "models/$name" ] && echo "$sha  models/$name" | sha256sum -c --status; then
    continue
  fi
  echo "==> Downloading model $name"
  curl -fL# --retry 3 -o "models/$name.part" "$base/$sub/$name"
  if ! echo "$sha  models/$name.part" | sha256sum -c --status; then
    rm -f "models/$name.part"
    echo "==> error: checksum mismatch for $name" >&2
    exit 1
  fi
  mv "models/$name.part" "models/$name"
done <<'LIST'
face_detection_yunet face_detection_yunet_2026may.onnx ebafce4e3c118d6554634be5c27ab333b4c047a9a8c3faf1d7cf93101c22f0f0
face_recognition_sface face_recognition_sface_2021dec.onnx 0ba9fbfa01b5270c96627c4ef784da859931e02f04419c829e83484087c34e79
LIST
