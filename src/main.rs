use std::fs;
use std::path::PathBuf;

use artinchip_flash::build_info;
use artinchip_flash::device::{BurnOptions, UpgDevice};
use artinchip_flash::image;
use artinchip_flash::protocol::commands::FwcMeta;
use artinchip_flash::standalone;
use artinchip_flash::transport::UpgTransport;
use artinchip_flash::uart::transport::UartOptions;
use artinchip_flash::uart::UartDevice;
use artinchip_flash::usb;
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(
    name = "artinchip-flash",
    version = build_info::VERSION,
    long_version = build_info::LONG_VERSION,
    about = "Cross-platform flasher for ArtInChip SoCs"
)]
struct Cli {
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
}

fn main() {
    let cli = Cli::parse();

    match cli.command {
        Commands::Scan => cmd_scan(),
        Commands::UsbList => cmd_usb_list(),
        Commands::SerialList => cmd_serial_list(),
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
            uart,
            baud,
            speed,
            no_enter_upg,
        } => cmd_burn(image, no_reset, uart, baud, speed, !no_enter_upg),
        Commands::UartMonitor {
            port,
            baud,
            enter_upg,
        } => cmd_uart_monitor(port, baud, enter_upg),
        Commands::EnvCheck { image } => cmd_env_check(image),
        Commands::InstallUsbAccess => cmd_install_usb_access(),
    }
}

fn uart_options(baud: u32, speed: Option<u32>, auto_enter: bool) -> UartOptions {
    UartOptions {
        baudrate: baud,
        max_baudrate: speed.filter(|speed| *speed > baud),
        auto_enter,
        ..Default::default()
    }
}

fn open_uart(
    port: &str,
    baud: u32,
    speed: Option<u32>,
    auto_enter: bool,
) -> Result<UartDevice, String> {
    let options = uart_options(baud, speed, auto_enter);
    if port.eq_ignore_ascii_case("auto") {
        UartDevice::open_auto(options)
    } else {
        UartDevice::open_port(port, options)
    }
}

fn cmd_uart_monitor(port: String, baud: u32, enter_upg: bool) {
    if let Err(e) = artinchip_flash::uart::run_cli_monitor(&port, baud, enter_upg) {
        eprintln!("{}", e);
        std::process::exit(1);
    }
}

fn cmd_serial_list() {
    match UartDevice::list_ports() {
        Ok(ports) => {
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

fn cmd_usb_list() {
    match usb::device::AicDevice::list_usb_devices() {
        Ok(devices) => {
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

fn cmd_scan() {
    match usb::device::AicDevice::scan_devices() {
        Ok(devices) if devices.is_empty() => {
            eprintln!("No ArtInChip device found (VID=0x33C3, PID=0x6677)");
            std::process::exit(1);
        }
        Ok(devices) => {
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
        // Parse local image file
        match fs::read(&path) {
            Ok(data) => {
                if let Err(e) = image::parser::print_image_info(&data) {
                    eprintln!("Error parsing image: {}", e);
                    std::process::exit(1);
                }
            }
            Err(e) => {
                eprintln!("Error reading '{}': {}", path.display(), e);
                std::process::exit(1);
            }
        }
        return;
    }

    let result = if let Some(port) = uart {
        open_uart(&port, baud, speed, auto_enter).and_then(|mut dev| {
            println!("=== Device Info ({}) ===", dev.transport_name());
            dev.show_info()?;
            if let Ok(media) = dev.get_storage_media() {
                println!("  Storage media: {}", media);
            }
            Ok(())
        })
    } else {
        usb::device::AicDevice::open_first().and_then(|mut dev| {
            println!("=== Device Info (USB) ===");
            dev.show_info()?;
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

fn cmd_burn(
    image_path: PathBuf,
    no_reset: bool,
    uart: Option<String>,
    baud: u32,
    speed: Option<u32>,
    auto_enter: bool,
) {
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

    let options = BurnOptions {
        reset_after_burn: !no_reset,
        ..Default::default()
    };

    let result = if let Some(port) = uart {
        open_uart(&port, baud, speed, auto_enter)
            .and_then(|dev| burn_with_device(dev, &img_data, &metas, &options))
    } else {
        usb::device::AicDevice::open_first()
            .and_then(|dev| burn_with_device(dev, &img_data, &metas, &options))
    };

    if let Err(e) = result {
        eprintln!("Burn failed: {}", e);
        std::process::exit(1);
    }

    println!("Burn completed successfully!");
}

fn burn_with_device<T: UpgTransport>(
    mut dev: UpgDevice<T>,
    img_data: &[u8],
    metas: &[FwcMeta],
    options: &BurnOptions,
) -> Result<(), String> {
    println!("Transport: {}", dev.transport_name());
    if let Err(e) = dev.show_info() {
        eprintln!("Warning: could not read device info: {}", e);
    }
    dev.burn_image_with_options(img_data, metas, options, None)
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
