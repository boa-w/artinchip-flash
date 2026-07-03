# aic-flash

Cross-platform CLI flasher for ArtInChip SoCs.  Communicates with the
device over USB using the CBW/CSW-based UPG protocol (reverse-engineered
from the Luban-Lite SDK).

## Build

Prerequisites: [Rust] 1.70+.

Platform notes:

- Windows: the GUI can install a WinUSB binding for VID `33C3`, PID `6677`
  through `pnputil` with UAC elevation.
- Linux: install the native build dependencies for libusb, for example
  `libusb-1.0-0-dev`, `libudev-dev`, and `pkg-config` on Debian/Ubuntu. The
  GUI can install a udev rule for non-root USB access.
- macOS: install Rust and, when needed by your toolchain, `libusb` through
  Homebrew. No kernel driver install is normally required.

```sh
cargo build --release
```

The CLI binary is placed at `target/release/aic-flash`.

To build the GUI:

```sh
cargo build --release --bin aic-flash-gui
```

The GUI binary is placed at `target/release/aic-flash-gui`.

[Rust]: https://rustup.rs

## Installers

Nightly builds publish both portable archives and native installers:

- Windows: `aic-flash-windows-x64-setup.exe` is the recommended installer with
  a full setup wizard and completion page. `aic-flash-windows-x64.msi` is also
  published for MSI-based deployment. Both install the CLI, GUI, README, Start
  Menu shortcuts, and appear in Windows Apps/Programs as
  `aic-flash ArtInChip Flasher`. `aic-flash-windows-x64.zip` is the portable
  package.
- macOS: `aic-flash-macos-arm64.pkg` installs `aic-flash-gui.app` to
  `/Applications` and the CLI to `/usr/local/bin/aic-flash`.
  `aic-flash-macos-arm64.tar.gz` is the portable package.
- Linux: `aic-flash-linux-x64.deb` installs the CLI/GUI to `/usr/bin`, adds a
  desktop entry, and installs the udev rule for `33c3:6677`.
  `aic-flash-linux-x64.tar.gz` is the portable package.

Unsigned macOS and Windows installers may show the normal first-run security
prompt until signing/notarization is configured.

## Usage

```
aic-flash scan          # list connected ArtInChip devices
aic-flash info          # query connected device (HWINFO, storage media)
aic-flash info <img>    # parse .img file header and META entries
aic-flash env-check [img]        # check config, USB access, and optional image
aic-flash install-usb-access     # install WinUSB binding or Linux udev rule
aic-flash burn <img>    # burn firmware image to device
aic-flash burn <img> --no-reset  # burn without resetting
```

## GUI

The GUI implements the AiBurn-compatible workflow natively. It stores its own
configuration under the platform user configuration directory:

- Windows: `%APPDATA%\aic-flash\config.ini`
- macOS: `~/Library/Application Support/aic-flash/config.ini`
- Linux: `$XDG_CONFIG_HOME/aic-flash/config.ini` or
  `~/.config/aic-flash/config.ini`

If an official `C:\ArtInChip\AiBurn\AiBurn.ini` exists on Windows it can still
be imported for compatibility, but the core burn flow does not require the
official package.

```sh
cargo run --bin aic-flash-gui
```

Implemented GUI features:

- USB device scan and device info display for VID `0x33C3`, PID `0x6677`.
- AiBurn-compatible image loading, header display, image history, component
  table, component extraction, and target partition selection.
- AiBurn-style online burn flow with updater stage, reconnect wait,
  `FULL_DISK_UPGRADE`, `image.info`, selected target components, upgrade end,
  progress events, CRC checks, and optional reset.
- Standalone environment check for USB access, config directory writability,
  selected image parsing, and driver readiness.
- Built-in USB access setup: Windows WinUSB INF installation through `pnputil`,
  Linux udev rule installation, and macOS no-driver status reporting.
- Settings compatible with the original `AiBurn.ini` fields:
  `auto_burn`, `is_verbose`, `read_device_log`, `adb_scan`, `retry_cnt`,
  `block_err_log`, `burn_timeout`, `language`, `image_path`, and
  `selected_parts`.
- Real GUI internationalization with Simplified Chinese (`zh_cn`) and English
  (`en`), controlled by the `language` setting.
- An advanced tools page. Native environment check and driver install are
  built in; `upgcmd.exe` remains available as an optional compatibility
  backend for advanced commands not yet migrated.

On Windows the optional compatibility path defaults to `C:\ArtInChip\AiBurn`.
On macOS and Linux it is empty by default. If you provide a compatibility
directory there, the GUI looks for `upgcmd` rather than `upgcmd.exe`. Normal
image parsing and online burning work without that directory.

The same standalone checks are available without the GUI:

```sh
aic-flash env-check firmware.img
aic-flash install-usb-access
```

### Linux USB permissions

For non-root access, install the udev rule from the GUI Driver button or run
the equivalent manually:

```sh
sudo tee /etc/udev/rules.d/99-aic-flash.rules >/dev/null <<'EOF'
SUBSYSTEM=="usb", ATTR{idVendor}=="33c3", ATTR{idProduct}=="6677", TAG+="uaccess", MODE="0666"
EOF
sudo udevadm control --reload-rules
sudo udevadm trigger
```

Reconnect the device after installing the rule.

### macOS USB notes

No kernel driver is normally required. If the device opens in the GUI but a
transaction times out immediately, reconnect the board and close other USB
debugging tools before retrying.

### Examples

```sh
# Scan for devices
aic-flash scan

# Inspect a firmware image
aic-flash info firmware_d21x_demo128-nand.img

# Flash the device
aic-flash burn firmware_d21x_demo128-nand.img
```

### Typical burn output

```
Image: artinchip d21x_demo128-nand v1.0.0 (4 components, 8388608 bytes)
  Magic:        AIC.FW
  Init mode:    0x0
  Current mode: 0x4
  Boot stage:   2
  Chip ID:      ...
Setting upgrade mode to FULL_DISK_UPGRADE...
  Meta: SPL (offset=0x800, size=131072, crc=0x...)
    Block size: 2048
    SPL: 131072/131072 (100.0%)
    CRC OK (0x...)
  Meta: U-Boot (offset=0x20800, size=524288, crc=0x...)
    ...
Burn completed successfully!
Device reset.
```

## Protocol

The USB protocol is fully documented in the Luban-Lite SDK
(`application/baremetal/bootloader/include/`) under Apache 2.0:

| Layer | File | Notes |
|-------|------|-------|
| Transport | `data_trans_layer.h` | CBW (USBC, 31 B) / CSW (USBS, 13 B), EP 0x02/0x81 |
| Application | `aicupg.h` | cmd_header (UPGC, 16 B), resp_header (UPGR, 16 B) |
| Commands | `basic_cmd.c`, `fwc_cmd.c` | GET_HWINFO, SET_FWC_META, SEND_FWC_DATA, ... |
| Image | `mk_image.py` | 2048 B header (AIC.FW), 512 B META entries |

- VID = `0x33C3`, PID = `0x6677`
- Bulk endpoints, no alternative setting
- Checksum: `magic + (reserved<<24|cmd<<16|ver<<8|protocol) + data_length`
