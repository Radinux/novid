#!/usr/bin/env bash
# Build and install facelock: daemon, PAM module, CLI and GTK app.
#
#   ./install.sh                      build and install (or upgrade)
#   ./install.sh --uninstall          remove; keeps enrolled faces and config
#   ./install.sh --uninstall --purge  also delete faces, sealed keyring secret
#                                     and /etc/facelock
#   -y, --yes                         don't ask for confirmation
#
# Arch and derivatives: builds packages with makepkg, installs them with pacman.
# Debian / Ubuntu: installs dependencies with apt, builds with cargo and copies
# the files (listed in a manifest for --uninstall).
#
# Run it as your normal user from a clone of the repository; sudo is used for
# the steps that need root. Face authentication is not turned on anywhere by
# installing: choose where to use it in the app.
set -euo pipefail

ROOT="$(cd "$(dirname "$(readlink -f "$0")")" && pwd)"
cd "$ROOT"

# ONNX models (OpenCV Zoo), checked by SHA-256.
MODELS_URL="https://github.com/opencv/opencv_zoo/raw/main/models"
YUNET="face_detection_yunet_2026may.onnx"
YUNET_SHA="ebafce4e3c118d6554634be5c27ab333b4c047a9a8c3faf1d7cf93101c22f0f0"
SFACE="face_recognition_sface_2021dec.onnx"
SFACE_SHA="0ba9fbfa01b5270c96627c4ef784da859931e02f04419c829e83484087c34e79"

# ONNX Runtime for Debian/Ubuntu when the system has none (Arch packages it).
ORT_VERSION="1.24.1"
ORT_SHA_x64="9142552248b735920f9390027e4512a2cacf8946a1ffcbe9071a5c210531026f"
ORT_SHA_aarch64="0f56edd68f7602df790b68b874a46b115add037e88385c6c842bb763b39b9f89"
ORT_DIR="/usr/lib/facelock/onnxruntime"
ORT_DROPIN="/etc/systemd/system/facelockd.service.d/onnxruntime.conf"

MIN_RUST_MINOR=92 # ort needs Rust >= 1.92
MANIFEST="/var/lib/facelock-install/manifest"

ACTION=install
PURGE=0
YES=0

msg() { printf '\033[1;34m==>\033[0m %s\n' "$*"; }
warn() { printf '\033[1;33m==> warning:\033[0m %s\n' "$*" >&2; }
die() { printf '\033[1;31m==> error:\033[0m %s\n' "$*" >&2; exit 1; }

confirm() {
  [ "$YES" = 1 ] && return 0
  local answer
  read -r -p "$1 [Y/n] " answer
  case "$answer" in "" | [YyOo]*) return 0 ;; *) return 1 ;; esac
}

usage() { sed -n '2,16p' "$0" | sed 's/^# \{0,1\}//'; exit "${1:-0}"; }

for arg in "$@"; do
  case "$arg" in
    --uninstall) ACTION=uninstall ;;
    --purge) PURGE=1 ;;
    -y | --yes) YES=1 ;;
    -h | --help) usage ;;
    *) warn "unknown option $arg"; usage 1 ;;
  esac
done
[ "$PURGE" = 1 ] && [ "$ACTION" != uninstall ] && die "--purge only goes with --uninstall"
[ "$(id -u)" = 0 ] && die "run this as your normal user (sudo is asked when needed)"
command -v sudo >/dev/null || die "sudo is required"
# pacman/apt prompts read stdin; without a terminal they abort after the build.
[ "$YES" = 0 ] && [ ! -t 0 ] && die "no terminal to answer the prompts; rerun with --yes"

[ -r /etc/os-release ] || die "cannot identify the distribution (/etc/os-release missing)"
# shellcheck disable=SC1091
. /etc/os-release
case " ${ID:-} ${ID_LIKE:-} " in
  *" arch "*) FAMILY=arch ;;
  *" debian "* | *" ubuntu "*) FAMILY=debian ;;
  *) die "unsupported distribution '${ID:-?}' (Arch or Debian/Ubuntu derivatives only)" ;;
esac

# -- shared ---------------------------------------------------------------

fetch_models() {
  mkdir -p models
  local name sha
  for pair in "$YUNET:$YUNET_SHA" "$SFACE:$SFACE_SHA"; do
    name="${pair%%:*}"
    sha="${pair##*:}"
    if [ -f "models/$name" ] && echo "$sha  models/$name" | sha256sum -c --status; then
      continue
    fi
    local sub=face_detection_yunet
    [ "$name" = "$SFACE" ] && sub=face_recognition_sface
    msg "Downloading model $name"
    curl -fL# --retry 3 -o "models/$name.part" "$MODELS_URL/$sub/$name"
    echo "$sha  models/$name.part" | sha256sum -c --status || { rm -f "models/$name.part"; die "checksum mismatch for $name"; }
    mv "models/$name.part" "models/$name"
  done
}

# Removes facelock from /etc/pam.d so nothing points to a missing module.
disable_pam() {
  if [ -x /usr/bin/facelockd ]; then
    sudo /usr/bin/facelockd --disable-pam || warn "could not clean /etc/pam.d; check it for pam_facelock.so lines"
  fi
}

purge_state() {
  msg "Deleting enrolled faces, sealed keyring secret and configuration"
  sudo rm -rf /var/lib/facelock /etc/facelock
}

# -- Arch -----------------------------------------------------------------

arch_install() {
  fetch_models
  msg "Installing build tools"
  local noconfirm=()
  [ "$YES" = 1 ] && noconfirm=(--noconfirm)
  sudo pacman -S --needed "${noconfirm[@]}" base-devel
  msg "Building and installing the packages (makepkg)"
  # No --needed: a rebuilt package with the same version must still replace
  # the installed one.
  (cd dist/arch && makepkg -sfi "${noconfirm[@]}")
}

arch_uninstall() {
  local pkgs=()
  for p in facelock-gtk facelock; do pacman -Q "$p" >/dev/null 2>&1 && pkgs+=("$p"); done
  if [ "${#pkgs[@]}" -gt 0 ]; then
    disable_pam # older packages have no pre_remove hook
    msg "Removing ${pkgs[*]}"
    local noconfirm=()
    [ "$YES" = 1 ] && noconfirm=(--noconfirm)
    sudo pacman -R "${noconfirm[@]}" "${pkgs[@]}"
  else
    msg "facelock is not installed"
  fi
}

# -- Debian / Ubuntu ------------------------------------------------------

deb_deps() {
  msg "Installing dependencies (apt)"
  local yes=()
  [ "$YES" = 1 ] && yes=(-y)
  sudo apt-get update
  sudo apt-get install "${yes[@]}" --no-install-recommends \
    build-essential pkg-config clang libclang-dev libdbus-1-dev libpam0g-dev libcrypt-dev \
    gettext curl ca-certificates \
    dbus polkitd systemd \
    python3-gi gir1.2-gtk-4.0 gir1.2-adw-1 \
    gtk-update-icon-cache desktop-file-utils
}

rust_ok() {
  command -v cargo >/dev/null && command -v rustc >/dev/null || return 1
  local minor
  minor="$(rustc --version | awk '{split($2, v, "."); print v[2]}')"
  [ "${minor:-0}" -ge "$MIN_RUST_MINOR" ]
}

deb_rust() {
  [ -f "$HOME/.cargo/env" ] && . "$HOME/.cargo/env"
  rust_ok && return
  if command -v rustup >/dev/null; then
    msg "Updating Rust (rustup)"
    rustup update stable
  else
    warn "Rust >= 1.$MIN_RUST_MINOR is needed; the distribution's version is too old or missing."
    confirm "Install Rust for your user with rustup (https://rustup.rs)?" || die "Rust is required"
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal
    . "$HOME/.cargo/env"
  fi
  rust_ok || die "Rust >= 1.$MIN_RUST_MINOR still not available"
}

deb_check_adw() {
  python3 - <<'EOF' || warn "libadwaita >= 1.6 and GTK >= 4.16 are needed for the app (Debian 13, Ubuntu 25.04 or newer)"
import gi
gi.require_version("Gtk", "4.0"); gi.require_version("Adw", "1")
from gi.repository import Adw, Gtk
raise SystemExit(0 if (Adw.get_minor_version() >= 6 and Gtk.get_minor_version() >= 16) else 1)
EOF
}

STAGED=""
inst() { # mode src dest
  sudo install -Dm"$1" "$2" "$3"
  STAGED+="$3"$'\n'
}

deb_onnxruntime() {
  if ldconfig -p | grep -q 'libonnxruntime\.so'; then
    msg "Using the system ONNX Runtime"
    return
  fi
  local arch sha
  case "$(uname -m)" in
    x86_64) arch=x64; sha="$ORT_SHA_x64" ;;
    aarch64) arch=aarch64; sha="$ORT_SHA_aarch64" ;;
    *) die "no ONNX Runtime build for $(uname -m)" ;;
  esac
  local tmp name="onnxruntime-linux-$arch-$ORT_VERSION"
  tmp="$(mktemp -d)"
  msg "Downloading ONNX Runtime $ORT_VERSION"
  curl -fL# --retry 3 -o "$tmp/$name.tgz" \
    "https://github.com/microsoft/onnxruntime/releases/download/v$ORT_VERSION/$name.tgz"
  echo "$sha  $tmp/$name.tgz" | sha256sum -c --status || { rm -rf "$tmp"; die "ONNX Runtime checksum mismatch"; }
  tar -xzf "$tmp/$name.tgz" -C "$tmp"
  inst 644 "$tmp/$name/lib/libonnxruntime.so.$ORT_VERSION" "$ORT_DIR/libonnxruntime.so.$ORT_VERSION"
  inst 644 "$tmp/$name/lib/libonnxruntime_providers_shared.so" "$ORT_DIR/libonnxruntime_providers_shared.so"
  rm -rf "$tmp"
  # The daemon loads it through ort's ORT_DYLIB_PATH.
  printf '[Service]\nEnvironment=ORT_DYLIB_PATH=%s/libonnxruntime.so.%s\n' "$ORT_DIR" "$ORT_VERSION" > "$ROOT/target/onnxruntime.conf"
  inst 644 "$ROOT/target/onnxruntime.conf" "$ORT_DROPIN"
}

deb_install() {
  deb_deps
  deb_check_adw
  deb_rust
  fetch_models

  msg "Building (cargo)"
  cargo build --release --locked -p facelockd -p facelock -p pam-facelock

  local pam_unix pamdir
  pam_unix="$(find /usr/lib /lib -name pam_unix.so -path '*/security/*' 2>/dev/null | head -n1)"
  [ -n "$pam_unix" ] || die "cannot find the PAM module directory (pam_unix.so)"
  pamdir="$(dirname "$pam_unix")"

  # Stop a running daemon so its binary can be replaced.
  sudo systemctl stop facelockd.service 2>/dev/null || true

  msg "Installing files"
  local t="$ROOT/target/release"
  inst 755 "$t/facelockd" /usr/bin/facelockd
  inst 755 "$t/facelock" /usr/bin/facelock
  inst 755 "$t/libpam_facelock.so" "$pamdir/pam_facelock.so"
  inst 644 dist/systemd/facelockd.service /usr/lib/systemd/system/facelockd.service
  inst 644 dist/dbus/org.facelock.Daemon1.service /usr/share/dbus-1/system-services/org.facelock.Daemon1.service
  inst 644 dist/dbus/org.facelock.Daemon1.conf /usr/share/dbus-1/system.d/org.facelock.Daemon1.conf
  inst 644 dist/polkit/org.facelock.policy /usr/share/polkit-1/actions/org.facelock.policy
  inst 644 "models/$YUNET" "/usr/share/facelock/models/$YUNET"
  inst 644 "models/$SFACE" "/usr/share/facelock/models/$SFACE"
  # Keep an existing configuration.
  [ -e /etc/facelock/config.toml ] || sudo install -Dm644 dist/config.toml /etc/facelock/config.toml

  inst 755 gtk/facelock-gtk /usr/bin/facelock-gtk
  for f in gtk/facelock_gtk/*.py; do
    inst 644 "$f" "/usr/share/facelock-gtk/facelock_gtk/$(basename "$f")"
  done
  inst 644 gtk/data/org.facelock.Facelock.desktop /usr/share/applications/org.facelock.Facelock.desktop
  inst 644 gtk/data/icons/org.facelock.Facelock.svg /usr/share/icons/hicolor/scalable/apps/org.facelock.Facelock.svg
  inst 644 gtk/data/icons/facelock-keyring-symbolic.svg /usr/share/icons/hicolor/symbolic/apps/facelock-keyring-symbolic.svg
  for po in gtk/po/*.po; do
    local lang mo
    lang="$(basename "$po" .po)"
    mo="$ROOT/target/locale/$lang/facelock-gtk.mo"
    mkdir -p "$(dirname "$mo")"
    msgfmt -o "$mo" "$po"
    inst 644 "$mo" "/usr/share/locale/$lang/LC_MESSAGES/facelock-gtk.mo"
  done

  deb_onnxruntime

  # Upgrade: drop files a previous install had that this one no longer ships.
  if [ -f "$MANIFEST" ]; then
    comm -23 <(sort -u "$MANIFEST") <(printf '%s' "$STAGED" | sort -u) | while IFS= read -r f; do
      [ -n "$f" ] && sudo rm -f -- "$f"
    done
  fi
  printf '%s' "$STAGED" | sudo install -Dm644 /dev/stdin "$MANIFEST"

  msg "Reloading system services"
  sudo systemctl daemon-reload
  sudo systemctl reload dbus.service 2>/dev/null || true
  sudo gtk-update-icon-cache -qtf /usr/share/icons/hicolor 2>/dev/null || true
  sudo update-desktop-database -q /usr/share/applications 2>/dev/null || true
}

deb_uninstall() {
  if [ ! -f "$MANIFEST" ]; then
    msg "facelock is not installed (no $MANIFEST)"
    return
  fi
  disable_pam
  sudo systemctl stop facelockd.service 2>/dev/null || true
  msg "Removing files"
  while IFS= read -r f; do
    [ -n "$f" ] && sudo rm -f -- "$f"
  done < "$MANIFEST"
  sudo rm -f "$MANIFEST"
  for d in /usr/share/facelock-gtk/facelock_gtk /usr/share/facelock-gtk /usr/share/facelock/models \
    /usr/share/facelock "$ORT_DIR" /usr/lib/facelock "$(dirname "$ORT_DROPIN")" /var/lib/facelock-install; do
    sudo rmdir --ignore-fail-on-non-empty "$d" 2>/dev/null || true
  done
  sudo systemctl daemon-reload
  sudo systemctl reload dbus.service 2>/dev/null || true
  sudo gtk-update-icon-cache -qtf /usr/share/icons/hicolor 2>/dev/null || true
  sudo update-desktop-database -q /usr/share/applications 2>/dev/null || true
}

# -- main -----------------------------------------------------------------

case "$ACTION:$FAMILY" in
  install:arch) arch_install ;;
  install:debian) deb_install ;;
  uninstall:arch) arch_uninstall ;;
  uninstall:debian) deb_uninstall ;;
esac

if [ "$ACTION" = uninstall ]; then
  [ "$PURGE" = 1 ] && purge_state
  msg "facelock removed."
else
  msg "facelock installed."
  echo "    Open “Face Unlock” (facelock-gtk) to add your face and choose where to use it."
  echo "    Your password keeps working everywhere as a fallback."
fi
