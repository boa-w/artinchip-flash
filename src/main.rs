use std::fs;
use std::path::PathBuf;

use artinchip_flash::build_info;
use artinchip_flash::image;
use artinchip_flash::standalone;
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
    /// Scan for connected ArtInChip devices
    Scan,
    /// List all USB devices visible through libusb
    UsbList,
    /// Show the currently selected project, output directory, and toolchain
    Info {
        /// Optional .img file to parse instead of querying a device
        #[arg(value_name = "IMAGE")]
        image: Option<PathBuf>,
    },
    /// Burn firmware image to device
    Burn {
        /// Path to the firmware image file (.img)
        #[arg(value_name = "IMAGE")]
        image: PathBuf,
        /// Do not reset device after burn
        #[arg(long)]
        no_reset: bool,
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
        Commands::Info { image } => cmd_info(image),
        Commands::Burn { image, no_reset } => cmd_burn(image, no_reset),
        Commands::EnvCheck { image } => cmd_env_check(image),
        Commands::InstallUsbAccess => cmd_install_usb_access(),
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

fn cmd_info(image: Option<PathBuf>) {
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
    } else {
        // Query device
        match usb::device::AicDevice::open_first() {
            Ok(mut dev) => {
                println!("=== Device Info ===");
                if let Err(e) = dev.show_info() {
                    eprintln!("Error reading device info: {}", e);
                    std::process::exit(1);
                }
                // Also try storage media
                match dev.get_storage_media() {
                    Ok(media) => println!("  Storage media: {}", media),
                    Err(_) => {}
                }
            }
            Err(e) => {
                eprintln!("{}", e);
                std::process::exit(1);
            }
        }
    }
}

fn cmd_burn(image: PathBuf, no_reset: bool) {
    // 1. Read image file
    let img_data = match fs::read(&image) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("Error reading '{}': {}", image.display(), e);
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

    // 3. Connect to device
    let mut dev = match usb::device::AicDevice::open_first() {
        Ok(d) => d,
        Err(e) => {
            eprintln!("Failed to connect: {}", e);
            std::process::exit(1);
        }
    };

    // 4. Show device info
    if let Err(e) = dev.show_info() {
        eprintln!("Warning: could not read device info: {}", e);
    }

    // 5. Burn!
    let options = usb::device::BurnOptions {
        reset_after_burn: !no_reset,
        ..Default::default()
    };
    if let Err(e) = dev.burn_image_with_options(&img_data, &metas, &options, None) {
        eprintln!("Burn failed: {}", e);
        std::process::exit(1);
    }

    println!("Burn completed successfully!");
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
