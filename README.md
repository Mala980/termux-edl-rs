# termux-edl-rs

Rust rewrite of the **termux-edl** branch of [bkerler/edl](https://github.com/bkerler/edl)
([Haris131/termux-edl @ `termux-edl`](https://github.com/Haris131/termux-edl)) — a Qualcomm
Sahara / Firehose EDL client designed to run on **Android/Termux** and **Linux
arm64 / armv7**. GPLv3, same as the original (port of B.Kerler's GPLv3 code).

## What works

- USB transport via libusb (rusb) — no pyusb needed; serial (ttyUSB) transport also supported.
- Sahara handshake & chip info (v1/v2 + V3 extended chip info via cmd `0x0A`).
- Firehose programmer upload over Sahara (32/64-bit read-data).
- Firehose: configure, read / read-buffer, program, erase, nop, reset, storageinfo.
- GPT parsing/printing (`--printgpt`, `--gpt`), partition read/write (`--r`, `--w`, `--rl`,
  `--wl`, `--rf`, `--ws`, `--e`, `--es`, `--rs`).
- Loader database (`Loaders/` directory, `<hwid>_<pkhash>_*.bin` naming, incl. msmid
  conversion via soc tables).
- Unit tests for HDLC CRC/escape, XML parsing, GPT parsing, loader-db conversion.

## Not (yet) ported

- Streaming (nandprg/enandprg) raw sector protocol (detected, but not implemented).
- OEM modules (Xiaomi EDL auth, Nothing verify), peek/poke/memtbl/secureboot/pbl/qfp,
  memory-debug dump, TCP server, qfil rawprogram XML writer.

## Build (via GitHub CI — recommended on-device)

Binaries for `aarch64-linux-android` (Termux), `armv7-linux-androideabi` (Termux 32-bit),
`aarch64/armv7/x86_64-unknown-linux-musl` are built by GitHub Actions on every push and
attached to `v*` tag releases (`.github/workflows/build.yml`).

## Build locally

```bash
cargo build --release
# cross-compile for Termux:
rustup target add aarch64-linux-android
# needs the Android NDK linker configured in .cargo/config.toml
```

## Usage (Termux)

```bash
# one-time: allow USB access
pkg install libusb android-tools
termux-setup-storage
termux-usb -l                       # find your device
termux-usb -r /dev/bus/usb/001/048  # claim raw access (run as needed)

# list EDL devices (root adb-less mode works only with proper usb device node access)
edl-rs --list

# print partition table (auto-detects 05c6:9008)
edl-rs --printgpt

# with a specific loader
edl-rs --loader Loaders/000b80e100020000_467f3020c4cc788d_fhprg.bin --printgpt

# read a partition
edl-rs --r boot_a boot_a.img

# write a partition
edl-rs --w boot_a boot_a.img

# reset device
edl-rs --reset
```

On regular Linux, EDL devices need udev permission:

```bash
sudo sh -c "echo 'SUBSYSTEM==\"usb\", ATTRS{idVendor}==\"05c6\", ATTRS{idProduct}==\"9008\", MODE=\"0666\"' >> /etc/udev/rules.d/99-edl.rules"
```

## Credits

- (c) B.Kerler 2018–2025 — original `edl` (GPLv3).
- Haris131 — termux-edl branch (Termux USB transport ideas).
- Rust port: Mala980.
