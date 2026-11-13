# facelock

Infrared face authentication for Linux, with liveness detection. Unlock
`sudo`, the GDM login and lock screen, and polkit prompts with your face — and,
optionally, your GNOME keyring at login.

- **IR liveness**: works with Windows Hello–style infrared cameras. The IR
  emitter must strobe and light your face, so photos and screens are refused.
- **Never locks you out**: when the face path is unavailable (no camera, lid
  closed, SSH session, daemon down) PAM falls back to your password.
- **Keyring unlock (opt-in)**: your login password is sealed with the TPM
  (`systemd-creds`) and handed to `pam_gnome_keyring` only after a face match
  at GDM login.
- **GTK app**: guided enrollment, live recognition test, and toggles for where
  the face is used.

## Components

| Path | What it is |
| --- | --- |
| `crates/facelock-core` | Camera capture, YuNet face detection, SFace embeddings, head pose, IR liveness, enrollment and verification |
| `crates/facelockd` | System D-Bus service (polkit-guarded); manages the PAM files and the sealed keyring secret |
| `crates/pam-facelock` | `pam_facelock.so` |
| `crates/facelock-cli` | `facelock` command-line client |
| `gtk/` | `facelock-gtk`, the GTK4 / libadwaita app (Python) |
| `dist/` | systemd, D-Bus and polkit files, default config, Arch PKGBUILD |

## Install

Requirements: an infrared camera, systemd, and Arch Linux or Debian 13 /
Ubuntu 25.04 (or derivatives). A TPM2 is needed only for the keyring option.

```sh
git clone https://github.com/Radinux/facelock.git
cd facelock
./install.sh
```

Run it as your normal user; it asks for sudo when needed.

- **Arch** and derivatives: builds the `facelock` and `facelock-gtk` packages
  with `makepkg` and installs them with `pacman`.
- **Debian / Ubuntu**: installs the dependencies with `apt`, installs Rust with
  rustup if the packaged version is too old, builds with `cargo`, and installs
  ONNX Runtime from the official release when the system has none.

The ONNX models (YuNet, SFace) are downloaded from
[OpenCV Zoo](https://github.com/opencv/opencv_zoo) and checked by SHA-256.

Installing does not turn face authentication on anywhere. Open **Face Unlock**
to add your face, then choose where to use it.

### Uninstall

```sh
./install.sh --uninstall          # keeps enrolled faces and the config
./install.sh --uninstall --purge  # also deletes them
```

facelock is removed from `/etc/pam.d` first, so no PAM stack is left pointing
to a missing module (on Arch, `pacman -R facelock` does the same).

## Configuration

`/etc/facelock/config.toml` — camera device, similarity threshold, liveness
checks, retry cooldown. Every key is optional; see `dist/config.toml` for the
defaults.

## Security notes

- Face recognition is weaker than a good password against someone who looks
  very much like you (a twin or a close sibling). Raise `threshold` in the
  config to trade convenience for margin.
- With the keyring option, root on the running machine can unseal your login
  password. The sealed file is useless on another machine or boot chain.
- Regular (RGB) webcams are refused unless you allow them; they offer no
  liveness check and never unlock the keyring.

## License

GPL-3.0-or-later. The ONNX models keep their own licenses (YuNet: MIT,
SFace: Apache-2.0).
