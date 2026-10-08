use std::fs;
use std::path::PathBuf;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::time::Instant;

use artinchip_flash::build_info;
use artinchip_flash::device::{BurnEvent, BurnOptions, UpgDevice};
use artinchip_flash::image;
use artinchip_flash::official;
use artinchip_flash::protocol::commands::FwcMeta;
use artinchip_flash::services::{self, UartSpec};
use artinchip_flash::standalone;
use artinchip_flash::transport::UpgTransport;
use artinchip_flash::update::{self, UpdateChannel};
use artinchip_flash::usb;
use artinchip_flash::verbosity;
use clap::{Parser, Subcommand};

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
    /// Machine-readable JSON output (scan, update)
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
    let result = if let Some(port) = uart {
        open_uart_shared(&port, baud, speed, auto_enter)
            .and_then(|dev| burn_with_device(dev, &img_data, &metas, &options, json, started))
    } else {
        usb::device::AicDevice::open_first()
            .and_then(|dev| burn_with_device(dev, &img_data, &metas, &options, json, started))
    };

    let elapsed = started.elapsed();
    if let Err(e) = result {
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
            println!("{}", burn_event_json_timed(&event, elapsed));
        } else {
            match event {
                BurnEvent::Log(line) | BurnEvent::Stage(line) => eprintln!("{}", line),
                BurnEvent::ComponentStarted { name, partition, size } => {
                    eprintln!("Meta {} partition={} size={} ...", name, partition, size)
                }
                BurnEvent::ComponentProgress { name, sent, total } => {
                    eprintln!(
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
                    eprintln!(
                        "  Overall: {}/{} ({:.1}%) {} {}",
                        sent,
                        total,
                        services::progress_ratio(sent, total) * 100.0,
                        format_rate(sent as f64 / elapsed.as_secs_f64().max(0.001)),
                        format_elapsed(elapsed),
                    )
                }
                BurnEvent::ComponentFinished { name } => {
                    eprintln!("Component done: {}", name)
                }
                BurnEvent::Finished => eprintln!("Burn finished {}", format_elapsed(elapsed)),
            }
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
