#!/usr/bin/env bash
# Builds the facelock and facelock-gtk .deb packages into target/deb.
#
# Run on the oldest Debian/Ubuntu release to support (the binaries need its
# glibc or newer). Needs cargo (Rust >= 1.92), dpkg-dev, clang, libpam0g-dev,
# libcrypt-dev, gettext and curl; the ONNX models must be in models/
# (dist/fetch-models.sh). ONNX Runtime is bundled in /usr/lib/facelock.
#
#   DEB_REVISION=2 dist/debian/build-deb.sh
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
cd "$ROOT"
HERE="$ROOT/dist/debian"

ORT_VERSION="1.24.1"
ORT_SHA_amd64="9142552248b735920f9390027e4512a2cacf8946a1ffcbe9071a5c210531026f"
ORT_SHA_arm64="0f56edd68f7602df790b68b874a46b115add037e88385c6c842bb763b39b9f89"

VERSION="$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -n1)-${DEB_REVISION:-1}"
ARCH="$(dpkg --print-architecture)"
MULTIARCH="$(dpkg-architecture -qDEB_HOST_MULTIARCH)"
OUT="$ROOT/target/deb"
WORK="$ROOT/target/deb-build"
MAINTAINER="Radinux <Radinux@users.noreply.github.com>"
HOMEPAGE="https://github.com/Radinux/novid"

case "$ARCH" in
  amd64) ort_arch=x64; ort_sha="$ORT_SHA_amd64" ;;
  arm64) ort_arch=aarch64; ort_sha="$ORT_SHA_arm64" ;;
  *) echo "no ONNX Runtime build for $ARCH" >&2; exit 1 ;;
esac
for m in face_detection_yunet_2026may.onnx face_recognition_sface_2021dec.onnx; do
  [ -f "models/$m" ] || { echo "models/$m missing: run dist/fetch-models.sh" >&2; exit 1; }
done

rm -rf "$WORK" && mkdir -p "$WORK" "$OUT"

echo "==> Building (cargo)"
cargo build --release --locked -p facelockd -p facelock -p pam-facelock
t="$ROOT/target/release"

ort="onnxruntime-linux-$ort_arch-$ORT_VERSION"
if ! echo "$ort_sha  target/$ort.tgz" | sha256sum -c --status 2>/dev/null; then
  echo "==> Downloading ONNX Runtime $ORT_VERSION"
  curl -fL# --retry 3 -o "target/$ort.tgz" \
    "https://github.com/microsoft/onnxruntime/releases/download/v$ORT_VERSION/$ort.tgz"
  echo "$ort_sha  target/$ort.tgz" | sha256sum -c --status || { echo "ONNX Runtime checksum mismatch" >&2; exit 1; }
fi
tar -xzf "target/$ort.tgz" -C "$WORK"

# -- facelock --------------------------------------------------------------

p="$WORK/facelock"
install -Dm755 "$t/facelockd" "$p/usr/bin/facelockd"
install -Dm755 "$t/facelock" "$p/usr/bin/facelock"
install -Dm644 "$t/libpam_facelock.so" "$p/usr/lib/$MULTIARCH/security/pam_facelock.so"
install -Dm644 dist/systemd/facelockd.service "$p/usr/lib/systemd/system/facelockd.service"
install -Dm644 dist/dbus/org.facelock.Daemon1.service "$p/usr/share/dbus-1/system-services/org.facelock.Daemon1.service"
install -Dm644 dist/dbus/org.facelock.Daemon1.conf "$p/usr/share/dbus-1/system.d/org.facelock.Daemon1.conf"
install -Dm644 dist/polkit/org.facelock.policy "$p/usr/share/polkit-1/actions/org.facelock.policy"
install -Dm644 dist/config.toml "$p/etc/facelock/config.toml"
install -Dm644 -t "$p/usr/share/facelock/models" models/face_detection_yunet_2026may.onnx models/face_recognition_sface_2021dec.onnx
install -Dm644 "$WORK/$ort/lib/libonnxruntime.so.$ORT_VERSION" "$p/usr/lib/facelock/libonnxruntime.so.$ORT_VERSION"
ln -s "libonnxruntime.so.$ORT_VERSION" "$p/usr/lib/facelock/libonnxruntime.so"
install -Dm644 "$WORK/$ort/LICENSE" "$p/usr/share/doc/facelock/LICENSE.onnxruntime"
install -Dm644 "$HERE/copyright" "$p/usr/share/doc/facelock/copyright"
strip --strip-unneeded "$p/usr/bin/facelockd" "$p/usr/bin/facelock" "$p/usr/lib/$MULTIARCH/security/pam_facelock.so"

# Library dependencies of what we link (ONNX Runtime is loaded at run time).
mkdir -p "$WORK/shlibs/debian"
printf 'Source: facelock\n\nPackage: facelock\nArchitecture: any\n' > "$WORK/shlibs/debian/control"
shlibs="$(cd "$WORK/shlibs" && dpkg-shlibdeps -O -e"$p/usr/bin/facelockd" -e"$p/usr/bin/facelock" \
  -e"$p/usr/lib/$MULTIARCH/security/pam_facelock.so" -e"$p/usr/lib/facelock/libonnxruntime.so.$ORT_VERSION" \
  2>/dev/null | sed -n 's/^shlibs:Depends=//p')"

mkdir -p "$p/DEBIAN"
cat > "$p/DEBIAN/control" <<EOF
Package: facelock
Version: $VERSION
Architecture: $ARCH
Maintainer: $MAINTAINER
Installed-Size: $(du -sk --exclude=DEBIAN "$p" | cut -f1)
Depends: $shlibs, dbus, polkitd, systemd, libpam-runtime
Suggests: facelock-gtk
Section: admin
Priority: optional
Homepage: $HOMEPAGE
Description: IR face authentication with liveness detection
 Unlock sudo, the GDM login and lock screen, and polkit prompts with your
 face, using a Windows Hello style infrared camera. Your password keeps
 working everywhere as a fallback. Optionally unlocks the GNOME keyring at
 login with the password sealed by the TPM.
 .
 This package has the daemon, the PAM module and the command-line client.
EOF
echo /etc/facelock/config.toml > "$p/DEBIAN/conffiles"
install -m755 "$HERE/facelock.postinst" "$p/DEBIAN/postinst"
install -m755 "$HERE/facelock.prerm" "$p/DEBIAN/prerm"
install -m755 "$HERE/facelock.postrm" "$p/DEBIAN/postrm"

# -- facelock-gtk ----------------------------------------------------------

g="$WORK/facelock-gtk"
install -Dm755 gtk/facelock-gtk "$g/usr/bin/facelock-gtk"
install -Dm644 -t "$g/usr/share/facelock-gtk/facelock_gtk" gtk/facelock_gtk/*.py
install -Dm644 gtk/data/org.facelock.Facelock.desktop "$g/usr/share/applications/org.facelock.Facelock.desktop"
install -Dm644 gtk/data/icons/org.facelock.Facelock.svg "$g/usr/share/icons/hicolor/scalable/apps/org.facelock.Facelock.svg"
install -Dm644 gtk/data/icons/facelock-keyring-symbolic.svg "$g/usr/share/icons/hicolor/symbolic/apps/facelock-keyring-symbolic.svg"
for po in gtk/po/*.po; do
  lang="$(basename "$po" .po)"
  mkdir -p "$g/usr/share/locale/$lang/LC_MESSAGES"
  msgfmt -o "$g/usr/share/locale/$lang/LC_MESSAGES/facelock-gtk.mo" "$po"
done
install -Dm644 "$HERE/copyright" "$g/usr/share/doc/facelock-gtk/copyright"

mkdir -p "$g/DEBIAN"
cat > "$g/DEBIAN/control" <<EOF
Package: facelock-gtk
Version: $VERSION
Architecture: all
Maintainer: $MAINTAINER
Installed-Size: $(du -sk --exclude=DEBIAN "$g" | cut -f1)
Depends: facelock (= $VERSION), python3, python3-gi, gir1.2-gtk-4.0 (>= 4.16), gir1.2-adw-1 (>= 1.6)
Section: admin
Priority: optional
Homepage: $HOMEPAGE
Description: GTK app to manage facelock face profiles
 Guided enrollment, a live recognition test, and switches for where face
 authentication is used (sudo, login screen, polkit, keyring unlock).
EOF

# -- build -----------------------------------------------------------------

for pkg in "$p" "$g"; do
  (cd "$pkg" && find . -type f ! -path './DEBIAN/*' -printf '%P\0' | sort -z | xargs -0 md5sum > DEBIAN/md5sums)
  dpkg-deb --root-owner-group -Zxz --build "$pkg" "$OUT"
done
ls -l "$OUT"
