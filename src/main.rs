use std::fs;
use std::path::PathBuf;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::time::Instant;

use artinchip_flash::build_info;
use artinchip_flash::burn_stats::{self, BurnOutcome};
use artinchip_flash::device::{BurnEvent, BurnOptions, UpgDevice};
use artinchip_flash::image;
use artinchip_flash::official;
use artinchip_flash::protocol::commands::FwcMeta;
use artinchip_flash::sdcard;
use artinchip_flash::services::{self, UartSpec};
use artinchip_flash::standalone;
use artinchip_flash::transport::UpgTransport;
use artinchip_flash::update::{self, UpdateChannel};
use artinchip_flash::usb;
use artinchip_flash::verbosity;
use clap::{Args, Parser, Subcommand};

#[derive(Parser)]
#[command(
    name = "artinchip-flash",
    version = build_info::VERSION,
    long_version = build_info::LONG_VERSION,
    about = "Cross-platform flasher for ArtInChip SoCs"
)]
struct Cli {
    /// Verbose transport-level logging (CBW/CSW bytes, UART framing)
    #[arg(long, global = true)]
    verbose: bool,
    /// Machine-readable JSON output (scan, usb-list, serial-list, stats, sd-list, update)
    #[arg(long, global = true)]
    json: bool,
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Scan for connected ArtInChip USB devices
    Scan,
    /// List all USB devices visible through libusb
    UsbList,
    /// List serial ports that can be used for UART firmware updates
    SerialList,
    /// Burn firmware image to device
    Burn {
        /// Path to the firmware image file (.img)
        #[arg(value_name = "IMAGE")]
        image: PathBuf,
        /// Do not reset device after burn
        #[arg(long)]
        no_reset: bool,
        /// Experimental force upgrade (BURN_IMG_FORCE, skips post-burn reset).
        /// Requires the device-side force-upgrade switch; unverified hardware.
        #[arg(long)]
        force_upgrade: bool,
        /// Erase the whole chip via `upgcmd flasherase` before burning.
        /// Needs --upgcmd-path (or the Windows default AiBurn install).
        #[arg(long)]
        erase_all: bool,
        /// Media id for --erase-all; defaults to the image media_dev_id
        #[arg(long, value_name = "ID")]
        erase_media: Option<String>,
        /// Path to official upgcmd(.exe) used by --erase-all
        #[arg(long, value_name = "PATH")]
        upgcmd_path: Option<PathBuf>,
        /// Save the burn event log to a file (in addition to console output)
        #[arg(long, value_name = "PATH")]
        log_file: Option<PathBuf>,
        /// Update over UART: port name, or "auto" to probe every serial port
        #[arg(
            long,
            value_name = "PORT",
            num_args = 0..=1,
            default_missing_value = "auto"
        )]
        uart: Option<String>,
        /// Initial UART baudrate used to reach the bootloader
        #[arg(long, default_value_t = 115200)]
        baud: u32,
        /// Negotiate a higher UART baudrate before burning (SET_UART_ARGS)
        #[arg(long, value_name = "BAUD")]
        speed: Option<u32>,
        /// Do not try to trigger UART upgrade mode when the protocol does not answer
        #[arg(long)]
        no_enter_upg: bool,
    },
    /// Show device information, or parse an .img file
    Info {
        /// Optional .img file to parse instead of querying a device
        #[arg(value_name = "IMAGE")]
        image: Option<PathBuf>,
        /// Query over UART: port name, or "auto" to probe every serial port
        #[arg(
            long,
            value_name = "PORT",
            num_args = 0..=1,
            default_missing_value = "auto"
        )]
        uart: Option<String>,
        /// Initial UART baudrate used to reach the bootloader
        #[arg(long, default_value_t = 115200)]
        baud: u32,
        /// Negotiate a higher UART baudrate before reading device info
        #[arg(long, value_name = "BAUD")]
        speed: Option<u32>,
        /// Do not try to trigger UART upgrade mode when the protocol does not answer
        #[arg(long)]
        no_enter_upg: bool,
    },
    /// Interactive UART monitor: view device output and send console commands
    UartMonitor {
        /// Serial port, or "auto" to use the first USB serial port
        #[arg(value_name = "PORT", default_value = "auto")]
        port: String,
        /// Baudrate of the device console
        #[arg(long, default_value_t = 115200)]
        baud: u32,
        /// Immediately send `aicupg gotobl` / `aicupg uart 0`
        #[arg(long)]
        enter_upg: bool,
    },
    /// Check local config, USB access, and optional image parsing
    EnvCheck {
        /// Optional .img file to parse during the check
        #[arg(value_name = "IMAGE")]
        image: Option<PathBuf>,
    },
    /// Install platform USB access support (WinUSB INF or Linux udev rule)
    InstallUsbAccess,
    /// Write a file to device memory (native UPG WRITE, no upgcmd needed)
    Write {
        /// Memory address (hex like 0x41000000, decimal, or 4k/1m suffix)
        #[arg(value_name = "ADDR")]
        address: String,
        /// Input file whose bytes are written
        #[arg(value_name = "FILE")]
        input: PathBuf,
        /// Skip this many input bytes before writing
        #[arg(long, value_name = "N")]
        skip: Option<String>,
        /// Write at most this many bytes
        #[arg(long, value_name = "N")]
        length: Option<String>,
        #[command(flatten)]
        transport: TransportArgs,
    },
    /// Read device memory to a file (native UPG READ)
    Read {
        /// Memory address
        #[arg(value_name = "ADDR")]
        address: String,
        /// Byte count
        #[arg(value_name = "LEN")]
        length: String,
        /// Output file
        #[arg(value_name = "FILE")]
        output: PathBuf,
        #[command(flatten)]
        transport: TransportArgs,
    },
    /// Write one 32-bit little-endian word (native)
    Writel {
        /// Memory address
        #[arg(value_name = "ADDR")]
        address: String,
        /// 32-bit value
        #[arg(value_name = "VALUE")]
        value: String,
        #[command(flatten)]
        transport: TransportArgs,
    },
    /// Read one 32-bit little-endian word (native)
    Readl {
        /// Memory address
        #[arg(value_name = "ADDR")]
        address: String,
        #[command(flatten)]
        transport: TransportArgs,
    },
    /// Call the function at an address with no arguments (native UPG EXEC)
    Exec {
        /// Function address
        #[arg(value_name = "ADDR")]
        address: String,
        #[command(flatten)]
        transport: TransportArgs,
    },
    /// Hexdump device memory (native UPG READ plus local formatting)
    Hexdump {
        /// Memory address
        #[arg(value_name = "ADDR")]
        address: String,
        /// Byte count
        #[arg(value_name = "LEN")]
        length: String,
        #[command(flatten)]
        transport: TransportArgs,
    },
    /// Fill memory with a repeated 32-bit pattern (native, host-side loop)
    Fill {
        /// Memory address
        #[arg(value_name = "ADDR")]
        address: String,
        /// Byte count
        #[arg(value_name = "LEN")]
        length: String,
        /// 32-bit fill pattern
        #[arg(value_name = "VALUE")]
        value: String,
        #[command(flatten)]
        transport: TransportArgs,
    },
    /// Zero device memory (native, fill with 0)
    Clear {
        /// Memory address
        #[arg(value_name = "ADDR")]
        address: String,
        /// Byte count
        #[arg(value_name = "LEN")]
        length: String,
        #[command(flatten)]
        transport: TransportArgs,
    },
    /// Host-side RAM test: save, pattern-check, restore, re-verify (native).
    /// Destructive by nature; avoid bootloader-reserved RAM.
    Memtest {
        /// Memory address
        #[arg(value_name = "ADDR")]
        address: String,
        /// Byte count
        #[arg(value_name = "SIZE")]
        size: String,
        /// Pattern rounds (default 1)
        #[arg(long, default_value_t = 1)]
        rounds: u32,
        #[command(flatten)]
        transport: TransportArgs,
    },
    /// Run a bootloader shell command (native UPG RUN_SHELL_STR, max 127 B)
    #[command(visible_alias = "sh")]
    Shcmd {
        /// Shell words, joined with spaces (e.g. shcmd md 0x40000000 4)
        #[arg(value_name = "SHELL", required = true)]
        shell: Vec<String>,
        #[command(flatten)]
        transport: TransportArgs,
    },
    /// Print the device log buffer (native UPG GET_LOG_*)
    Log {
        #[command(flatten)]
        transport: TransportArgs,
    },
    /// Show per-day burn statistics (success/failure/cancelled)
    Stats {
        /// Clear all recorded statistics instead of showing them
        #[arg(long)]
        clear: bool,
    },
    /// List physical disks (read-only; for boot-card target confirmation).
    /// Writing boot cards is not implemented — use official AiBurn for that.
    SdList,
    /// Check for updates from GitHub Releases
    Update {
        /// Update channel: stable (default, semver `v*` releases) or nightly
        #[arg(long, default_value = "stable")]
        channel: String,
        /// Open the release page in the default browser when an update exists
        #[arg(long)]
        open: bool,
    },
}

/// Transport selectors shared by every native device command (USB default,
/// UART opt-in with the same flags as `burn`/`info`).
#[derive(Args, Clone, Debug)]
struct TransportArgs {
    /// Query over UART: port name, or "auto" to probe every serial port
    #[arg(
        long,
        value_name = "PORT",
        num_args = 0..=1,
        default_missing_value = "auto"
    )]
    uart: Option<String>,
    /// Initial UART baudrate used to reach the bootloader
    #[arg(long, default_value_t = 115200)]
    baud: u32,
    /// Negotiate a higher UART baudrate before the command
    #[arg(long, value_name = "BAUD")]
    speed: Option<u32>,
    /// Do not try to trigger UART upgrade mode when the protocol does not answer
    #[arg(long)]
    no_enter_upg: bool,
}

impl TransportArgs {
    fn open(&self) -> Result<UpgDevice<Box<dyn UpgTransport>>, String> {
        services::open_native_device(
            self.uart.as_deref(),
            self.baud,
            self.speed,
            !self.no_enter_upg,
        )
    }
}

fn main() {
    let cli = Cli::parse();
    verbosity::set_verbose(cli.verbose || std::env::var("ARTINCHIP_FLASH_VERBOSE").is_ok());

    match cli.command {
        Commands::Scan => cmd_scan(cli.json),
        Commands::UsbList => cmd_usb_list(cli.json),
        Commands::SerialList => cmd_serial_list(cli.json),
        Commands::Info {
            image,
            uart,
            baud,
            speed,
            no_enter_upg,
        } => cmd_info(image, uart, baud, speed, !no_enter_upg),
        Commands::Burn {
            image,
            no_reset,
            force_upgrade,
            erase_all,
            erase_media,
            upgcmd_path,
            log_file,
            uart,
            baud,
            speed,
            no_enter_upg,
        } => cmd_burn(BurnFlags {
            image,
            no_reset,
            force_upgrade,
            erase_all,
            erase_media,
            upgcmd_path,
            log_file,
            uart,
            baud,
            speed,
            auto_enter: !no_enter_upg,
            json: cli.json,
        }),
        Commands::UartMonitor {
            port,
            baud,
            enter_upg,
        } => cmd_uart_monitor(port, baud, enter_upg),
        Commands::EnvCheck { image } => cmd_env_check(image),
        Commands::InstallUsbAccess => cmd_install_usb_access(),
        Commands::Write { address, input, skip, length, transport } => {
            cmd_write(address, input, skip, length, transport)
        }
        Commands::Read { address, length, output, transport } => {
            cmd_read(address, length, output, transport)
        }
        Commands::Writel { address, value, transport } => cmd_writel(address, value, transport),
        Commands::Readl { address, transport } => cmd_readl(address, transport),
        Commands::Exec { address, transport } => cmd_exec(address, transport),
        Commands::Hexdump { address, length, transport } => {
            cmd_hexdump(address, length, transport)
        }
        Commands::Fill { address, length, value, transport } => {
            cmd_fill(address, length, value, transport)
        }
        Commands::Clear { address, length, transport } => cmd_clear(address, length, transport),
        Commands::Memtest { address, size, rounds, transport } => {
            cmd_memtest(address, size, rounds, transport)
        }
        Commands::Shcmd { shell, transport } => cmd_shcmd(shell, transport),
        Commands::Log { transport } => cmd_log(transport),
        Commands::Stats { clear } => cmd_stats(clear, cli.json),
        Commands::SdList => cmd_sd_list(cli.json),
        Commands::Update { channel, open } => cmd_update(&channel, open, cli.json),
    }
}

fn open_uart_shared(port: &str, baud: u32, speed: Option<u32>, auto_enter: bool) -> Result<artinchip_flash::uart::UartDevice, String> {
    services::open_uart(&UartSpec::new(port, baud, speed, auto_enter))
}

/// Default official `upgcmd` path used by `--erase-all` when the caller did
/// not pass `--upgcmd-path`. Mirrors the GUI compat default: Windows tries
/// `C:\ArtInChip\AiBurn\upgcmd.exe`, other platforms have no default and the
/// caller must pass the flag explicitly.
fn default_upgcmd_path() -> PathBuf {
    #[cfg(windows)]
    {
        artinchip_flash::app_config::compat_tool_path(std::path::Path::new(
            r"C:\ArtInChip\AiBurn",
        ))
    }
    #[cfg(not(windows))]
    {
        PathBuf::new()
    }
}

fn cmd_uart_monitor(port: String, baud: u32, enter_upg: bool) {
    if let Err(e) = artinchip_flash::uart::run_cli_monitor(&port, baud, enter_upg) {
        eprintln!("{}", e);
        std::process::exit(1);
    }
}

fn cmd_serial_list(json: bool) {
    match artinchip_flash::uart::UartDevice::list_ports() {
        Ok(ports) => {
            if json {
                let items: Vec<serde_json::Value> = ports
                    .iter()
                    .map(|port| {
                        serde_json::json!({
                            "port": port.port_name,
                            "type": port.port_type,
                            "vid": port.vid,
                            "pid": port.pid,
                            "product": port.product,
                        })
                    })
                    .collect();
                println!("{}", serde_json::json!({ "ports": items }));
                return;
            }
            if ports.is_empty() {
                println!("No serial ports found.");
                return;
            }
            println!("Serial ports:");
            for port in ports {
                let usb = match (port.vid, port.pid) {
                    (Some(vid), Some(pid)) => format!(" usb={:04x}:{:04x}", vid, pid),
                    _ => String::new(),
                };
                let product = port
                    .product
                    .as_deref()
                    .map(|product| format!(" {}", product))
                    .unwrap_or_default();
                println!(
                    "  {:<32} type={:<9}{}{}",
                    port.port_name, port.port_type, usb, product
                );
            }
        }
        Err(e) => {
            eprintln!("{}", e);
            std::process::exit(1);
        }
    }
}

fn cmd_usb_list(json: bool) {
    match usb::device::AicDevice::list_usb_devices() {
        Ok(devices) => {
            if json {
                let items: Vec<serde_json::Value> = devices
                    .iter()
                    .map(|device| {
                        serde_json::json!({
                            "bus": device.bus_number,
                            "address": device.address,
                            "vid": format!("{:04x}", device.vendor_id),
                            "pid": format!("{:04x}", device.product_id),
                            "class": device.class_code,
                            "speed": device.speed,
                            "path": device.port_path,
                        })
                    })
                    .collect();
                println!("{}", serde_json::json!({ "devices": items }));
                return;
            }
            if devices.is_empty() {
                println!("No USB devices found.");
                return;
            }
            println!("Visible USB devices:");
            for device in devices {
                let marker = if device.vendor_id == 0x33c3 && device.product_id == 0x6677 {
                    "  <-- ArtInChip upgrade"
                } else {
                    ""
                };
                println!(
                    "  bus={:<3} address={:<3} {:04x}:{:04x} class={:02x}/{:02x}/{:02x} speed={:<10} path={}{}",
                    device.bus_number,
                    device.address,
                    device.vendor_id,
                    device.product_id,
                    device.class_code,
                    device.subclass_code,
                    device.protocol_code,
                    device.speed,
                    device.port_path,
                    marker
                );
            }
        }
        Err(e) => {
            eprintln!("{}", e);
            std::process::exit(1);
        }
    }
}

fn cmd_scan(json: bool) {
    match usb::device::AicDevice::scan_devices() {
        Ok(devices) if devices.is_empty() => {
            if json {
                println!("{}", serde_json::json!({ "devices": [] }));
                return;
            }
            eprintln!("No ArtInChip device found (VID=0x33C3, PID=0x6677)");
            std::process::exit(1);
        }
        Ok(devices) => {
            if json {
                let items: Vec<serde_json::Value> = devices
                    .iter()
                    .map(|device| {
                        serde_json::json!({
                            "bus": device.bus_number,
                            "address": device.address,
                            "path": device.port_path,
                            "vid": format!("{:04x}", device.vendor_id),
                            "pid": format!("{:04x}", device.product_id),
                            "speed": device.speed,
                            "ready": device.ready,
                            "status": device.status,
                        })
                    })
                    .collect();
                println!("{}", serde_json::json!({ "devices": items }));
                return;
            }
            println!("Detected {} ArtInChip device(s):", devices.len());
            for device in &devices {
                println!(
                    "  bus={} address={} path={} vid=0x{:04x} pid=0x{:04x} speed={} status={}",
                    device.bus_number,
                    device.address,
                    device.port_path,
                    device.vendor_id,
                    device.product_id,
                    device.speed,
                    if device.ready { "ready" } else { "not-ready" }
                );
                if let Some(status) = &device.status {
                    println!("    {}", status);
                }
            }
        }
        Err(e) => {
            eprintln!("Failed to scan USB devices: {}", e);
            std::process::exit(1);
        }
    }
}

fn cmd_info(
    image: Option<PathBuf>,
    uart: Option<String>,
    baud: u32,
    speed: Option<u32>,
    auto_enter: bool,
) {
    if let Some(path) = image {
        // Parse local image file (shared formatter, no direct lib printing).
        match fs::read(&path) {
            Ok(data) => match image::parser::format_image_info(&data) {
                Ok(text) => println!("{}", text),
                Err(e) => {
                    eprintln!("Error parsing image: {}", e);
                    std::process::exit(1);
                }
            },
            Err(e) => {
                eprintln!("Error reading '{}': {}", path.display(), e);
                std::process::exit(1);
            }
        }
        return;
    }

    let result = if let Some(port) = uart {
        open_uart_shared(&port, baud, speed, auto_enter).and_then(|mut dev| {
            println!("=== Device Info ({}) ===", dev.transport_name());
            for line in dev.device_info_lines()? {
                println!("  {}", line);
            }
            if let Ok(media) = dev.get_storage_media() {
                println!("  Storage media: {}", media);
            }
            Ok(())
        })
    } else {
        usb::device::AicDevice::open_first().and_then(|mut dev| {
            println!("=== Device Info (USB) ===");
            for line in dev.device_info_lines()? {
                println!("  {}", line);
            }
            if let Ok(media) = dev.get_storage_media() {
                println!("  Storage media: {}", media);
            }
            Ok(())
        })
    };

    if let Err(e) = result {
        eprintln!("{}", e);
        std::process::exit(1);
    }
}

/// CLI `burn` flags bundled so `cmd_burn` stays under the argument limit.
struct BurnFlags {
    image: PathBuf,
    no_reset: bool,
    force_upgrade: bool,
    erase_all: bool,
    erase_media: Option<String>,
    upgcmd_path: Option<PathBuf>,
    log_file: Option<PathBuf>,
    uart: Option<String>,
    baud: u32,
    speed: Option<u32>,
    auto_enter: bool,
    json: bool,
}

fn cmd_burn(flags: BurnFlags) {
    let BurnFlags {
        image: image_path,
        no_reset,
        force_upgrade,
        erase_all,
        erase_media,
        upgcmd_path,
        log_file,
        uart,
        baud,
        speed,
        auto_enter,
        json,
    } = flags;
    // 1. Read image file
    let img_data = match fs::read(&image_path) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("Error reading '{}': {}", image_path.display(), e);
            std::process::exit(1);
        }
    };

    // 2. Parse image
    let (header, metas, _payload) = match image::parser::parse_image(&img_data) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("Error parsing image: {}", e);
            std::process::exit(1);
        }
    };

    println!(
        "Image: {} {} v{} ({} components, {} bytes total)",
        header.platform_str(),
        header.product_str(),
        header.version_str(),
        metas.len(),
        img_data.len()
    );

    if force_upgrade {
        eprintln!(
            "Note: --force-upgrade is experimental (BURN_IMG_FORCE, no post-burn reset); \
             the device must enable force upgrade (official manual §2.1.4)."
        );
    }

    // 2b. Optional pre-burn full-chip erase via the official backend.
    // Native erase is not implemented (UPG erase command unconfirmed), so this
    // reuses `upgcmd flasherase` *before* the native burn opens the device.
    if erase_all {
        let media = erase_media
            .as_deref()
            .map(str::trim)
            .filter(|m| !m.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| header.media_dev_id().to_string());
        let upgcmd = upgcmd_path.unwrap_or_else(default_upgcmd_path);
        if upgcmd.as_os_str().is_empty() {
            eprintln!(
                "Error: --erase-all needs --upgcmd-path (no default upgcmd on this platform)"
            );
            std::process::exit(1);
        }
        let (erase_uart, erase_baud) = match (&uart, speed) {
            (Some(port), _) if !port.eq_ignore_ascii_case("auto") => {
                (port.clone(), speed.map(|b| b.to_string()).unwrap_or_default())
            }
            (Some(_), _) => (String::new(), speed.map(|b| b.to_string()).unwrap_or_default()),
            (None, _) => (String::new(), String::new()),
        };
        eprintln!("Erasing chip (flasherase media {}) ...", media);
        match official::run_pre_burn_erase(
            &upgcmd,
            &media,
            Some(&image_path),
            "",
            &erase_uart,
            &erase_baud,
        ) {
            Ok(text) => {
                for line in text.lines() {
                    eprintln!("  [flasherase] {}", line);
                }
            }
            Err(e) => {
                eprintln!("Pre-burn erase failed, aborting burn: {}", e);
                std::process::exit(1);
            }
        }
    }

    let cancel = Arc::new(AtomicBool::new(false));
    {
        let flag = cancel.clone();
        // Ignore double-install errors (tests / nested calls).
        let _ = ctrlc::set_handler(move || {
            flag.store(true, Ordering::SeqCst);
        });
    }
    let options = BurnOptions {
        reset_after_burn: !no_reset && !force_upgrade,
        force_upgrade,
        cancel: Some(cancel),
        ..Default::default()
    };

    eprintln!("Press Ctrl+C to cancel the burn (aborts at the next chunk).");
    let started = Instant::now();
    let mut log_lines: Vec<String> = Vec::new();
    let result = if let Some(port) = uart {
        open_uart_shared(&port, baud, speed, auto_enter).and_then(|dev| {
            burn_with_device(
                dev,
                &img_data,
                &metas,
                &options,
                json,
                started,
                &mut log_lines,
            )
        })
    } else {
        usb::device::AicDevice::open_first().and_then(|dev| {
            burn_with_device(
                dev,
                &img_data,
                &metas,
                &options,
                json,
                started,
                &mut log_lines,
            )
        })
    };

    let elapsed = started.elapsed();
    if let Some(path) = &log_file {
        if log_lines.is_empty() {
            eprintln!("Warning: no burn events captured, --log-file not written");
        } else {
            match services::write_log_file(path, &log_lines) {
                Ok(()) => eprintln!("Burn event log saved to {}", path.display()),
                Err(e) => eprintln!("Warning: could not save burn log: {}", e),
            }
        }
    }
    if let Err(e) = result {
        // Record the outcome for `stats` before exiting (best effort only;
        // a stats failure must not mask the burn result).
        let outcome = burn_stats::classify_error(&e);
        let app_dir = standalone::default_app_dir();
        if let Err(stat_err) = burn_stats::record(&app_dir, outcome) {
            eprintln!("Warning: could not record burn stats: {}", stat_err);
        }
        if e.contains("cancelled") {
            eprintln!(
                "Burn cancelled after {:.1}s (device stays in upgrade mode; retry when ready).",
                elapsed.as_secs_f64()
            );
            std::process::exit(130);
        }
        eprintln!("Burn failed after {:.1}s: {}", elapsed.as_secs_f64(), e);
        std::process::exit(1);
    }

    if let Err(e) = burn_stats::record(&standalone::default_app_dir(), BurnOutcome::Success) {
        eprintln!("Warning: could not record burn stats: {}", e);
    }

    eprintln!(
        "Burn completed successfully in {:.1}s (avg {}).",
        elapsed.as_secs_f64(),
        format_rate(img_data.len() as f64 / elapsed.as_secs_f64().max(0.001))
    );
    println!("Burn completed successfully!");
}

fn burn_with_device<T: UpgTransport>(
    mut dev: UpgDevice<T>,
    img_data: &[u8],
    metas: &[FwcMeta],
    options: &BurnOptions,
    json: bool,
    started: Instant,
    log_lines: &mut Vec<String>,
) -> Result<(), String> {
    println!("Transport: {}", dev.transport_name());
    match dev.device_info_lines() {
        Ok(lines) => {
            for line in lines {
                println!("  {}", line);
            }
        }
        Err(e) => eprintln!("Warning: could not read device info: {}", e),
    }
    // Library emits structured events; CLI renders them (previously the library
    // printed directly, which GUI could not reuse).
    let mut callback = |event: BurnEvent| {
        let elapsed = started.elapsed();
        if json {
            let value = burn_event_json_timed(&event, elapsed);
            println!("{}", value);
            log_lines.push(value.to_string());
        } else {
            let line = match event {
                BurnEvent::Log(line) | BurnEvent::Stage(line) => line,
                BurnEvent::ComponentStarted { name, partition, size } => {
                    format!("Meta {} partition={} size={} ...", name, partition, size)
                }
                BurnEvent::ComponentProgress { name, sent, total } => {
                    format!(
                        "  {}: {}/{} ({:.1}%) {} {}",
                        name,
                        sent,
                        total,
                        services::progress_ratio(sent, total) * 100.0,
                        format_rate(sent as f64 / elapsed.as_secs_f64().max(0.001)),
                        format_elapsed(elapsed),
                    )
                }
                BurnEvent::OverallProgress { sent, total } => {
                    format!(
                        "  Overall: {}/{} ({:.1}%) {} {}",
                        sent,
                        total,
                        services::progress_ratio(sent, total) * 100.0,
                        format_rate(sent as f64 / elapsed.as_secs_f64().max(0.001)),
                        format_elapsed(elapsed),
                    )
                }
                BurnEvent::ComponentFinished { name } => {
                    format!("Component done: {}", name)
                }
                BurnEvent::Finished => format!("Burn finished {}", format_elapsed(elapsed)),
            };
            eprintln!("{}", line);
            log_lines.push(line);
        }
    };
    dev.burn_image_with_options(img_data, metas, options, Some(&mut callback))
}

fn format_rate(bps: f64) -> String {
    if !bps.is_finite() || bps <= 0.0 {
        return "--".to_string();
    }
    const MIB: f64 = 1024.0 * 1024.0;
    const KIB: f64 = 1024.0;
    if bps >= MIB {
        format!("{:.2} MiB/s", bps / MIB)
    } else if bps >= KIB {
        format!("{:.1} KiB/s", bps / KIB)
    } else {
        format!("{:.0} B/s", bps)
    }
}

fn format_elapsed(elapsed: std::time::Duration) -> String {
    format!("[{:02}:{:02}]", elapsed.as_secs() / 60, elapsed.as_secs() % 60)
}

#[allow(dead_code)]
fn burn_event_json(event: &BurnEvent) -> serde_json::Value {
    burn_event_json_timed(event, std::time::Duration::ZERO)
}

fn burn_event_json_timed(event: &BurnEvent, elapsed: std::time::Duration) -> serde_json::Value {
    let elapsed_secs = elapsed.as_secs_f64();
    match event {
        BurnEvent::Log(line) => serde_json::json!({ "type": "log", "line": line, "elapsed_secs": elapsed_secs }),
        BurnEvent::Stage(line) => serde_json::json!({ "type": "stage", "line": line, "elapsed_secs": elapsed_secs }),
        BurnEvent::ComponentStarted { name, partition, size } => {
            serde_json::json!({ "type": "component_started", "name": name, "partition": partition, "size": size, "elapsed_secs": elapsed_secs })
        }
        BurnEvent::ComponentProgress { name, sent, total } => {
            serde_json::json!({ "type": "component_progress", "name": name, "sent": sent, "total": total, "elapsed_secs": elapsed_secs, "rate_bps": rate_bps(*sent, elapsed) })
        }
        BurnEvent::OverallProgress { sent, total } => {
            serde_json::json!({ "type": "overall_progress", "sent": sent, "total": total, "elapsed_secs": elapsed_secs, "rate_bps": rate_bps(*sent, elapsed) })
        }
        BurnEvent::ComponentFinished { name } => {
            serde_json::json!({ "type": "component_finished", "name": name, "elapsed_secs": elapsed_secs })
        }
        BurnEvent::Finished => serde_json::json!({ "type": "finished", "elapsed_secs": elapsed_secs }),
    }
}

fn rate_bps(sent: usize, elapsed: std::time::Duration) -> f64 {
    let secs = elapsed.as_secs_f64().max(0.001);
    sent as f64 / secs
}

/// Run `f` against an opened native device; print the error and exit 1.
fn with_native_device(
    transport: &TransportArgs,
    f: impl FnOnce(&mut UpgDevice<Box<dyn UpgTransport>>) -> Result<(), String>,
) {
    match transport.open().and_then(|mut dev| {
        eprintln!("Transport: {}", dev.transport_name());
        f(&mut dev)
    }) {
        Ok(()) => {}
        Err(e) => {
            eprintln!("Error: {}", e);
            std::process::exit(1);
        }
    }
}

fn must_parse_u32(label: &str, text: &str) -> u32 {
    match services::parse_u32(text) {
        Ok(value) => value,
        Err(e) => {
            eprintln!("Invalid {} ('{}'): {}", label, text, e);
            std::process::exit(1);
        }
    }
}

fn cmd_write(
    address: String,
    input: PathBuf,
    skip: Option<String>,
    length: Option<String>,
    transport: TransportArgs,
) {
    let addr = must_parse_u32("address", &address);
    let skip_bytes = skip.as_deref().map(|s| must_parse_u32("skip", s)).unwrap_or(0) as usize;
    let file = match fs::read(&input) {
        Ok(data) => data,
        Err(e) => {
            eprintln!("Error reading '{}': {}", input.display(), e);
            std::process::exit(1);
        }
    };
    if skip_bytes > file.len() {
        eprintln!(
            "Skip {} exceeds input size {}",
            skip_bytes,
            file.len()
        );
        std::process::exit(1);
    }
    let mut data = &file[skip_bytes..];
    if let Some(len_text) = length.as_deref() {
        let max = must_parse_u32("length", len_text) as usize;
        data = &data[..data.len().min(max)];
    }
    if data.is_empty() {
        eprintln!("Nothing to write (input empty after skip/length)");
        std::process::exit(1);
    }
    let len = data.len();
    with_native_device(&transport, |dev| {
        dev.write_memory(addr, data)?;
        println!("Wrote {} bytes to {:#x}", len, addr);
        Ok(())
    });
}

fn cmd_read(address: String, length: String, output: PathBuf, transport: TransportArgs) {
    let addr = must_parse_u32("address", &address);
    let len = must_parse_u32("length", &length);
    if len == 0 {
        eprintln!("Length must be > 0");
        std::process::exit(1);
    }
    with_native_device(&transport, |dev| {
        let data = dev.read_memory(addr, len)?;
        match fs::write(&output, &data) {
            Ok(()) => println!(
                "Read {} bytes from {:#x} to {}",
                data.len(),
                addr,
                output.display()
            ),
            Err(e) => return Err(format!("Error writing '{}': {}", output.display(), e)),
        }
        Ok(())
    });
}

fn cmd_writel(address: String, value: String, transport: TransportArgs) {
    let addr = must_parse_u32("address", &address);
    let val = must_parse_u32("value", &value);
    with_native_device(&transport, |dev| {
        dev.write_memory(addr, &val.to_le_bytes())?;
        println!("Wrote {:#x} to {:#x}", val, addr);
        Ok(())
    });
}

fn cmd_readl(address: String, transport: TransportArgs) {
    let addr = must_parse_u32("address", &address);
    with_native_device(&transport, |dev| {
        let data = dev.read_memory(addr, 4)?;
        let val = u32::from_le_bytes(data[..4].try_into().unwrap());
        println!("{:#x}: {:#x} ({})", addr, val, val);
        Ok(())
    });
}

fn cmd_exec(address: String, transport: TransportArgs) {
    let addr = must_parse_u32("address", &address);
    with_native_device(&transport, |dev| {
        dev.exec_address(addr)?;
        println!("Executed {:#x}", addr);
        Ok(())
    });
}

fn cmd_hexdump(address: String, length: String, transport: TransportArgs) {
    let addr = must_parse_u32("address", &address);
    let len = must_parse_u32("length", &length);
    if len == 0 {
        eprintln!("Length must be > 0");
        std::process::exit(1);
    }
    with_native_device(&transport, |dev| {
        let data = dev.read_memory(addr, len)?;
        print!("{}", services::format_hexdump(addr, &data));
        Ok(())
    });
}

fn cmd_fill(address: String, length: String, value: String, transport: TransportArgs) {
    let addr = must_parse_u32("address", &address);
    let len = must_parse_u32("length", &length);
    let val = must_parse_u32("value", &value);
    with_native_device(&transport, |dev| {
        dev.fill_memory(addr, len, val)?;
        println!("Filled {} bytes at {:#x} with {:#x}", len, addr, val);
        Ok(())
    });
}

fn cmd_clear(address: String, length: String, transport: TransportArgs) {
    let addr = must_parse_u32("address", &address);
    let len = must_parse_u32("length", &length);
    with_native_device(&transport, |dev| {
        dev.fill_memory(addr, len, 0)?;
        println!("Cleared {} bytes at {:#x}", len, addr);
        Ok(())
    });
}

fn cmd_memtest(address: String, size: String, rounds: u32, transport: TransportArgs) {
    let addr = must_parse_u32("address", &address);
    let len = must_parse_u32("size", &size);
    eprintln!(
        "Warning: memtest writes patterns in place at {:#x} ({} bytes, {} rounds); avoid bootloader-reserved RAM.",
        addr, len, rounds
    );
    with_native_device(&transport, |dev| {
        dev.memtest_memory(addr, len, rounds)?;
        println!("memtest {:#x}+{:#x} rounds={}: PASS", addr, len, rounds);
        Ok(())
    });
}

fn cmd_shcmd(shell: Vec<String>, transport: TransportArgs) {
    let line = shell.join(" ");
    if line.trim().is_empty() {
        eprintln!("Shell command must not be empty");
        std::process::exit(1);
    }
    with_native_device(&transport, |dev| {
        dev.run_shell(&line)?;
        println!("OK");
        Ok(())
    });
}

fn cmd_log(transport: TransportArgs) {
    with_native_device(&transport, |dev| {
        let log = dev.get_device_log()?;
        if log.is_empty() {
            println!("(device log empty)");
        } else {
            print!("{}", log);
            if !log.ends_with('\n') {
                println!();
            }
        }
        Ok(())
    });
}

fn cmd_env_check(image: Option<PathBuf>) {
    println!("{}", standalone::environment_report(image.as_deref()));
}

fn cmd_install_usb_access() {
    match standalone::install_driver() {
        Ok(()) => println!("USB access setup completed."),
        Err(e) => {
            eprintln!("USB access setup failed: {}", e);
            std::process::exit(1);
        }
    }
}

fn cmd_stats(clear: bool, json: bool) {
    let app_dir = standalone::default_app_dir();
    if clear {
        match burn_stats::clear(&app_dir) {
            Ok(()) => println!("Burn statistics cleared."),
            Err(e) => {
                eprintln!("Failed to clear burn statistics: {}", e);
                std::process::exit(1);
            }
        }
        return;
    }
    let stats = burn_stats::load(&app_dir);
    if json {
        println!("{}", stats.to_json());
    } else {
        println!("{}", burn_stats::format_table(&stats));
        println!("File: {}", burn_stats::stats_path(&app_dir).display());
    }
}

fn cmd_sd_list(json: bool) {
    match sdcard::list_disks() {
        Ok(disks) => {
            if json {
                let items: Vec<serde_json::Value> = disks
                    .iter()
                    .map(|disk| {
                        serde_json::json!({
                            "id": disk.id,
                            "model": disk.model,
                            "size_bytes": disk.size_bytes,
                            "bus_type": disk.bus_type,
                            "removable": disk.removable,
                        })
                    })
                    .collect();
                println!("{}", serde_json::json!({ "disks": items }));
                return;
            }
            if disks.is_empty() {
                println!("No physical disks found.");
                return;
            }
            println!("Physical disks (read-only; writing boot cards is not implemented):");
            for disk in &disks {
                println!("{}", disk.summary());
            }
            println!("Note: use official AiBurn to write a boot card; double-check the target id first.");
        }
        Err(e) => {
            eprintln!("Failed to list physical disks: {}", e);
            std::process::exit(1);
        }
    }
}

fn cmd_update(channel: &str, open: bool, json: bool) {
    let channel = UpdateChannel::from_str(channel);
    match update::check(channel) {
        Ok(status) => {
            if json {
                println!("{}", status.to_json());
            } else {
                println!("{}", status.summary_line());
                println!("Channel: {}", status.channel.as_str());
                println!("Current: {} ({})", status.current_version, status.current_commit);
                println!("Latest:  {}", status.latest_tag);
                println!("Page:    {}", status.html_url);
                if !status.notes_preview.is_empty() {
                    println!("--- release notes (preview) ---");
                    println!("{}", status.notes_preview);
                }
                if status.update_available {
                    if update::is_portable_install() {
                        println!("This looks like a portable checkout; download the matching archive from the page above.");
                    } else {
                        println!("Installer-managed location detected; re-run the matching installer (msi/setup/deb/pkg) instead of replacing binaries.");
                    }
                }
            }
            if open && status.update_available {
                if let Err(e) = update::open_url(&status.html_url) {
                    eprintln!("{}", e);
                    std::process::exit(1);
                }
            }
            if status.update_available {
                std::process::exit(10);
            }
        }
        Err(e) => {
            if json {
                println!(
                    "{}",
                    serde_json::json!({ "error": e, "channel": channel.as_str() })
                );
            } else {
                eprintln!("{}", e);
            }
            std::process::exit(1);
        }
    }
}
