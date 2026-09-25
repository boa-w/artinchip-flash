# artinchip-flash

Cross-platform CLI/GUI flasher for ArtInChip SoCs. Communicates with the
device over USB (CBW/CSW-based UPG protocol) or over UART (framed transport
tunnelling the same UPG protocol), reverse-engineered from the Luban-Lite SDK.

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

The CLI binary is placed at `target/release/artinchip-flash`.

To build the GUI:

```sh
cargo build --release --bin artinchip-flash-gui
```

The GUI binary is placed at `target/release/artinchip-flash-gui`.

[Rust]: https://rustup.rs

## Installers

Nightly builds publish both portable archives and native installers:

- Windows: `artinchip-flash-windows-x64-setup.exe` is the recommended installer with
  a full setup wizard and completion page. `artinchip-flash-windows-x64.msi` is also
  published for MSI-based deployment. Both install the CLI, GUI, README, Start
  Menu shortcuts, and appear in Windows Apps/Programs as
  `artinchip-flash ArtInChip Flasher`. `artinchip-flash-windows-x64.zip` is the portable
  package.
- macOS: `artinchip-flash-macos-arm64.pkg` installs `artinchip-flash-gui.app` to
  `/Applications` and the CLI to `/usr/local/bin/artinchip-flash`.
  `artinchip-flash-macos-arm64.tar.gz` is the portable package.
- Linux: `artinchip-flash-linux-x64.deb` installs the CLI/GUI to `/usr/bin`, adds a
  desktop entry, and installs the udev rule for `33c3:6677`.
  `artinchip-flash-linux-x64.tar.gz` is the portable package.

Unsigned macOS and Windows installers may show the normal first-run security
prompt until signing/notarization is configured.

## Usage

```
artinchip-flash scan          # list connected ArtInChip USB devices
artinchip-flash usb-list      # list every USB device seen through libusb
artinchip-flash serial-list   # list serial ports usable for UART updates
artinchip-flash info          # query connected USB device (HWINFO, storage media)
artinchip-flash info <img>    # parse .img file header and META entries
artinchip-flash info --uart /dev/ttyUSB0        # query a device over UART
artinchip-flash env-check [img]        # check config, USB access, and optional image
artinchip-flash install-usb-access     # install WinUSB binding or Linux udev rule
artinchip-flash burn <img>    # burn firmware image to device over USB
artinchip-flash burn <img> --no-reset  # burn without resetting
artinchip-flash burn <img> --uart /dev/ttyUSB0                 # burn over UART
artinchip-flash burn <img> --uart auto --speed 1500000         # probe ports, then switch baud
artinchip-flash uart-monitor /dev/ttyUSB0                      # interactive UART console
artinchip-flash uart-monitor /dev/ttyUSB0 --enter-upg          # trigger upgrade mode, then monitor
```

## UART firmware update

The device bootloader must be built with UART upgrading enabled
(`CONFIG_AICUPG_UART_ENABLE=y`, which selects `AIC_UART_DRV`). The board in
this repository already ships this in
`target/configs/d21x_d70t-128-nand_baremetal_bootloader_defconfig`.

Enter UART upgrade mode on the device with one of:

- `aicupg gotobl` on the running application console (reboots to the
  bootloader; UART mode is selected when no USB host is attached), or
- `aicupg uart 0` on the bootloader console.

The tool can also do this automatically: when the UART upgrade protocol does
not answer, it sends `aicupg gotobl` and `aicupg uart 0` to the console and
then waits for the device, answering `AIBURNFORCE` / `AIBURNID` boot keywords
so a board that is power-cycled or reset during the wait can also enter
upgrade mode. Auto-enter is enabled by default and can be disabled with
`--no-enter-upg` (CLI) or the GUI checkbox.

Then run:

```sh
artinchip-flash serial-list
artinchip-flash burn firmware.img --uart /dev/ttyUSB0
# macOS ports are usually /dev/cu.usbserial-XXXX
```

Options:

| Option | Meaning |
|--------|---------|
| `--uart <PORT>` | Serial port, or `auto` to probe every port |
| `--baud <BAUD>` | Initial baudrate used to reach the bootloader (default 115200) |
| `--speed <BAUD>` | Negotiate a higher baudrate via `SET_UART_ARGS` before burning |
| `--no-enter-upg` | Do not try to trigger UART upgrade mode automatically |

An interactive console is available for manual bring-up:

```sh
artinchip-flash uart-monitor /dev/ttyUSB0
# type a line and press Enter to send it, Ctrl+C to exit
artinchip-flash uart-monitor auto --enter-upg
```

`uart-monitor` also answers `AIBURNFORCE` / `AIBURNID` boot keywords, so you
can start it, power-cycle the board, and watch it request upgrade mode.

UART transfer is stop-and-wait framed (short `SOH` / long `STX` frames with
CRC16-CCITT), so it is slower than USB; `--speed` can significantly reduce
burn time when your adapter supports it.

## GUI

The GUI implements the AiBurn-compatible workflow natively. It stores its own
configuration under the platform user configuration directory:

- Windows: `%APPDATA%\artinchip-flash\config.ini`
- macOS: `~/Library/Application Support/artinchip-flash/config.ini`
- Linux: `$XDG_CONFIG_HOME/artinchip-flash/config.ini` or
  `~/.config/artinchip-flash/config.ini`

If an official `C:\ArtInChip\AiBurn\AiBurn.ini` exists on Windows it can still
be imported for compatibility, but the core burn flow does not require the
official package.

```sh
cargo run --bin artinchip-flash-gui
```

Implemented GUI features:

- USB device scan and device info display for VID `0x33C3`, PID `0x6677`.
- Transport selector (USB or UART) with serial port list/refresh, initial
  baudrate and optional max baudrate negotiation.
- UART interactive monitor: stream device console output, send commands,
  trigger upgrade mode (`aicupg gotobl` / `aicupg uart 0`) with one click, and
  auto-enter upgrade mode when the protocol does not answer.
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
  `selected_parts`; plus `transport`, `serial_port`, `serial_baud`, and
  `serial_speed` for UART updates.
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
artinchip-flash env-check firmware.img
artinchip-flash install-usb-access
```

### Linux USB permissions

For non-root access, install the udev rule from the GUI Driver button or run
the equivalent manually:

```sh
sudo tee /etc/udev/rules.d/99-artinchip-flash.rules >/dev/null <<'EOF'
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
artinchip-flash scan

# Inspect a firmware image
artinchip-flash info firmware_d21x_demo128-nand.img

# Flash the device
artinchip-flash burn firmware_d21x_demo128-nand.img
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
| Transport (USB) | `data_trans_layer.h` | CBW (USBC, 31 B) / CSW (USBS, 13 B), EP 0x02/0x81 |
| Transport (UART) | `uart_proto_layer.c` | `SOH`/`STX` framing, CRC16-CCITT, ACK/NAK, `DC1_SEND`/`DC2_RECV` |
| Application | `aicupg.h` | cmd_header (UPGC, 16 B), resp_header (UPGR, 16 B) |
| Commands | `basic_cmd.c`, `fwc_cmd.c` | GET_HWINFO, SET_FWC_META, SEND_FWC_DATA, SET_UART_ARGS, ... |
| Image | `mk_image.py` | 2048 B header (AIC.FW), 512 B META entries |

- VID = `0x33C3`, PID = `0x6677`
- Bulk endpoints, no alternative setting
- Checksum: `magic + (reserved<<24|cmd<<16|ver<<8|protocol) + data_length`
- UART: 8N1, device sends `CAN` on init, host polls `SIG_C` and waits for `ACK`;
  each logical buffer (CBW, payload, data, CSW) switches direction first
  (`DC1_SEND` to send, `DC2_RECV` to receive)
