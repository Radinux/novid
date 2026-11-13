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
# Debian / Ubuntu: installs the build dependencies with apt, builds .deb
# packages (dist/debian/build-deb.sh) and installs them with apt.
#
# Run it as your normal user from a clone of the repository; sudo is used for
# the steps that need root. Face authentication is not turned on anywhere by
# installing: choose where to use it in the app.
set -euo pipefail

ROOT="$(cd "$(dirname "$(readlink -f "$0")")" && pwd)"
cd "$ROOT"

MIN_RUST_MINOR=92 # ort needs Rust >= 1.92

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

fetch_models() { "$ROOT/dist/fetch-models.sh"; }

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
  # pacman doesn't restart services; a running daemon would keep the old binary.
  sudo systemctl try-restart facelockd.service
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
  msg "Installing build dependencies (apt)"
  local yes=()
  [ "$YES" = 1 ] && yes=(-y)
  sudo apt-get update
  sudo apt-get install "${yes[@]}" --no-install-recommends \
    build-essential dpkg-dev pkg-config clang libclang-dev libdbus-1-dev libpam0g-dev libcrypt-dev \
    gettext curl ca-certificates
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

deb_install() {
  deb_deps
  deb_rust
  fetch_models
  msg "Building the packages"
  rm -rf target/deb
  dist/debian/build-deb.sh
  msg "Installing the packages (apt)"
  local yes=()
  [ "$YES" = 1 ] && yes=(-y)
  # --reinstall: a rebuilt package with the same version must still replace
  # the installed one.
  sudo apt-get install "${yes[@]}" --reinstall "$ROOT"/target/deb/facelock_*.deb "$ROOT"/target/deb/facelock-gtk_*.deb
}

deb_uninstall() {
  local pkgs=()
  for p in facelock-gtk facelock; do dpkg -s "$p" >/dev/null 2>&1 && pkgs+=("$p"); done
  if [ "${#pkgs[@]}" -eq 0 ]; then
    msg "facelock is not installed"
    return
  fi
  msg "Removing ${pkgs[*]}"
  local yes=()
  [ "$YES" = 1 ] && yes=(-y)
  # The prerm script takes facelock out of /etc/pam.d first.
  if [ "$PURGE" = 1 ]; then
    sudo apt-get purge "${yes[@]}" "${pkgs[@]}"
  else
    sudo apt-get remove "${yes[@]}" "${pkgs[@]}"
  fi
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
