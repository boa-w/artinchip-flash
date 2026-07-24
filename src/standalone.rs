use std::fs;
use std::path::{Path, PathBuf};
#[cfg(any(windows, target_os = "linux"))]
use std::process::Command;

use crate::build_info;
use crate::image::parser;
use crate::usb::device::AicDevice;

#[cfg(windows)]
const AIC_WINUSB_INF: &str = r#"; artinchip-flash WinUSB driver binding for ArtInChip upgrade devices
[Version]
Signature="$Windows NT$"
Class=USBDevice
ClassGuid={88bae032-5a81-49f0-bc3d-a4ff138216d6}
Provider=%ProviderName%
DriverVer=06/30/2026,1.0.0.0

[Manufacturer]
%ProviderName%=Standard,NTamd64,NTx86

[Standard.NTamd64]
%DeviceName%=Device_Install,USB\VID_33C3&PID_6677

[Standard.NTx86]
%DeviceName%=Device_Install,USB\VID_33C3&PID_6677

[Device_Install]
Include=winusb.inf
Needs=WINUSB.NT

[Device_Install.Services]
Include=winusb.inf
Needs=WINUSB.NT.Services

[Device_Install.HW]
AddReg=Device_AddReg

[Device_AddReg]
HKR,,DeviceInterfaceGUIDs,0x10000,"{d70f0b35-5a30-4b41-b44f-a2e010c06977}"

[Strings]
ProviderName="artinchip-flash"
DeviceName="ArtInChip USB Upgrade Device"
"#;

pub const AIC_USB_VID: u16 = 0x33C3;
pub const AIC_USB_PID: u16 = 0x6677;
pub const APP_NAME: &str = "artinchip-flash";
const LEGACY_APP_NAME: &str = "aic-flash";

pub fn default_app_dir() -> PathBuf {
    app_dir_for(APP_NAME)
}

fn legacy_app_dir() -> PathBuf {
    app_dir_for(LEGACY_APP_NAME)
}

fn app_dir_for(app_name: &str) -> PathBuf {
    #[cfg(windows)]
    {
        if let Some(appdata) = std::env::var_os("APPDATA") {
            return PathBuf::from(appdata).join(app_name);
        }
    }
    #[cfg(target_os = "macos")]
    {
        if let Some(home) = home_dir() {
            return home
                .join("Library")
                .join("Application Support")
                .join(app_name);
        }
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        if let Some(xdg) = std::env::var_os("XDG_CONFIG_HOME") {
            return PathBuf::from(xdg).join(app_name);
        }
        if let Some(home) = home_dir() {
            return home.join(".config").join(app_name);
        }
    }
    if let Some(appdata) = std::env::var_os("APPDATA") {
        return PathBuf::from(appdata).join(app_name);
    }
    if let Some(home) = std::env::var_os("USERPROFILE") {
        return PathBuf::from(home).join(format!(".{app_name}"));
    }
    PathBuf::from(format!(".{app_name}"))
}

pub fn migrate_legacy_app_data() -> Result<(), String> {
    let source = legacy_app_dir();
    let destination = default_app_dir();
    if source == destination || !source.exists() {
        return Ok(());
    }
    fs::create_dir_all(&destination).map_err(|e| {
        format!(
            "Failed to create migration destination '{}': {}",
            destination.display(),
            e
        )
    })?;
    for file_name in ["config.ini", "img_history.txt"] {
        let source_file = source.join(file_name);
        let destination_file = destination.join(file_name);
        if source_file.is_file() && !destination_file.exists() {
            fs::copy(&source_file, &destination_file).map_err(|e| {
                format!(
                    "Failed to migrate '{}' to '{}': {}",
                    source_file.display(),
                    destination_file.display(),
                    e
                )
            })?;
        }
    }
    Ok(())
}

#[cfg(any(unix, target_os = "macos"))]
fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
}

pub fn ensure_app_dir() -> Result<PathBuf, String> {
    let dir = default_app_dir();
    fs::create_dir_all(&dir).map_err(|e| format!("Failed to create '{}': {}", dir.display(), e))?;
    Ok(dir)
}

pub fn config_path() -> PathBuf {
    default_app_dir().join("config.ini")
}

pub fn image_history_path() -> PathBuf {
    default_app_dir().join("img_history.txt")
}

pub fn environment_report(image: Option<&Path>) -> String {
    let mut lines = Vec::new();
    lines.push("artinchip-flash standalone environment check".to_string());
    lines.push(format!("Version: {}", build_info::VERSION));
    lines.push(format!("Build: {}", build_info::BUILD));
    lines.push(format!("Commit: {}", build_info::COMMIT));
    lines.push(format!("Platform: {}", platform_name()));
    lines.push(format!("Config dir: {}", default_app_dir().display()));

    match ensure_app_dir() {
        Ok(dir) => {
            let probe = dir.join(".write-test");
            match fs::write(&probe, b"ok").and_then(|_| fs::remove_file(&probe)) {
                Ok(()) => lines.push("Config dir writable: OK".to_string()),
                Err(e) => lines.push(format!("Config dir writable: FAILED ({})", e)),
            }
        }
        Err(e) => lines.push(format!("Config dir: FAILED ({})", e)),
    }

    match AicDevice::scan_devices() {
        Ok(devices) if devices.is_empty() => {
            lines.push("USB device: not connected (VID=33C3 PID=6677)".to_string());
        }
        Ok(devices) => {
            lines.push(format!("USB device: {} detected", devices.len()));
            for device in &devices {
                lines.push(format!(
                    "  bus={} address={} path={} speed={} status={}",
                    device.bus_number,
                    device.address,
                    device.port_path,
                    device.speed,
                    if device.ready { "ready" } else { "not-ready" }
                ));
                if let Some(status) = &device.status {
                    lines.push(format!("    {}", status));
                }
            }
            if devices.iter().any(|device| device.ready) {
                match AicDevice::open_first() {
                    Ok(_) => lines.push("USB access: OK".to_string()),
                    Err(e) => lines.push(format!("USB access: FAILED ({})", e)),
                }
            } else {
                lines.push(
                    "USB access: SKIPPED (device detected but not ready for USB transfers)"
                        .to_string(),
                );
            }
        }
        Err(e) => lines.push(format!("USB scan: FAILED ({})", e)),
    }

    if let Some(image) = image {
        match parser::read_image(image) {
            Ok((_data, _header, metas, summary)) => lines.push(format!(
                "Image: OK ({} {} v{}, {} components)",
                summary.platform,
                summary.product,
                summary.version,
                metas.len()
            )),
            Err(e) => lines.push(format!("Image: FAILED ({})", e)),
        }
    } else {
        lines.push("Image: not selected".to_string());
    }

    lines.push(driver_help_text().to_string());
    lines.extend(usb_permission_hint().lines().map(str::to_string));

    lines.join("\n")
}

pub fn install_driver() -> Result<(), String> {
    install_usb_access()
}

#[cfg(windows)]
fn install_usb_access() -> Result<(), String> {
    let dir = ensure_app_dir()?.join("driver");
    fs::create_dir_all(&dir).map_err(|e| format!("Failed to create '{}': {}", dir.display(), e))?;
    let inf = dir.join("aic-winusb.inf");
    fs::write(&inf, AIC_WINUSB_INF)
        .map_err(|e| format!("Failed to write '{}': {}", inf.display(), e))?;
    install_driver_inf(&inf)
}

#[cfg(windows)]
fn install_driver_inf(inf: &Path) -> Result<(), String> {
    let command = format!(
        "Start-Process -FilePath 'pnputil.exe' -ArgumentList @('/add-driver',{},'/install') -Verb RunAs -Wait",
        powershell_single_quoted(&inf.to_string_lossy())
    );
    let status = Command::new("powershell")
        .args([
            "-NoProfile",
            "-ExecutionPolicy",
            "Bypass",
            "-Command",
            &command,
        ])
        .status()
        .map_err(|e| format!("Failed to start pnputil: {}", e))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("pnputil elevation command exited with {}", status))
    }
}

#[cfg(windows)]
fn powershell_single_quoted(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

#[cfg(target_os = "linux")]
fn install_usb_access() -> Result<(), String> {
    let dir = ensure_app_dir()?.join("driver");
    fs::create_dir_all(&dir).map_err(|e| format!("Failed to create '{}': {}", dir.display(), e))?;
    let rules = dir.join("99-artinchip-flash.rules");
    fs::write(&rules, linux_udev_rule())
        .map_err(|e| format!("Failed to write '{}': {}", rules.display(), e))?;

    let script = format!(
        "cp {} /etc/udev/rules.d/99-artinchip-flash.rules && udevadm control --reload-rules && udevadm trigger",
        sh_single_quoted(&rules.to_string_lossy())
    );
    let status = Command::new("sh")
        .args(["-c", &format!("pkexec sh -c {}", sh_single_quoted(&script))])
        .status()
        .or_else(|_| {
            Command::new("sh")
                .args(["-c", &format!("sudo sh -c {}", sh_single_quoted(&script))])
                .status()
        })
        .map_err(|e| format!("Failed to install udev rule: {}", e))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!(
            "udev rule install exited with {}. You can run manually: sudo cp {} /etc/udev/rules.d/99-artinchip-flash.rules && sudo udevadm control --reload-rules && sudo udevadm trigger",
            status,
            rules.display()
        ))
    }
}

#[cfg(target_os = "macos")]
fn install_usb_access() -> Result<(), String> {
    Ok(())
}

#[cfg(all(not(windows), not(target_os = "linux"), not(target_os = "macos")))]
fn install_usb_access() -> Result<(), String> {
    Err("USB access installer is not implemented for this platform".to_string())
}

#[cfg(target_os = "linux")]
fn sh_single_quoted(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}

#[cfg(windows)]
fn driver_help_text() -> &'static str {
    "Driver installer: built-in pnputil WinUSB INF"
}

#[cfg(target_os = "linux")]
fn driver_help_text() -> &'static str {
    "USB permissions: built-in udev rule installer"
}

#[cfg(target_os = "macos")]
fn driver_help_text() -> &'static str {
    "USB permissions: macOS normally needs no driver install"
}

#[cfg(all(not(windows), not(target_os = "linux"), not(target_os = "macos")))]
fn driver_help_text() -> &'static str {
    "USB permissions: no installer for this platform"
}

#[cfg(target_os = "linux")]
pub fn linux_udev_rule() -> &'static str {
    "SUBSYSTEM==\"usb\", ATTR{idVendor}==\"33c3\", ATTR{idProduct}==\"6677\", TAG+=\"uaccess\", MODE=\"0666\"\n"
}

#[cfg(not(target_os = "linux"))]
pub fn linux_udev_rule() -> &'static str {
    "SUBSYSTEM==\"usb\", ATTR{idVendor}==\"33c3\", ATTR{idProduct}==\"6677\", TAG+=\"uaccess\", MODE=\"0666\"\n"
}

fn platform_name() -> &'static str {
    #[cfg(windows)]
    {
        "Windows"
    }
    #[cfg(target_os = "macos")]
    {
        "macOS"
    }
    #[cfg(target_os = "linux")]
    {
        "Linux"
    }
    #[cfg(all(not(windows), not(target_os = "macos"), not(target_os = "linux")))]
    {
        std::env::consts::OS
    }
}

#[cfg(windows)]
fn usb_permission_hint() -> &'static str {
    "Hint: use the Driver button or `artinchip-flash install-usb-access` to bind WinUSB when USB open/claim fails."
}

#[cfg(target_os = "linux")]
fn usb_permission_hint() -> &'static str {
    "Hint: use the Driver button or `artinchip-flash install-usb-access`, then reconnect the device."
}

#[cfg(target_os = "macos")]
fn usb_permission_hint() -> &'static str {
    "Hint: macOS usually needs no driver. If USB access fails, close other artinchip-flash/AiBurn instances, unplug and reconnect the board, and avoid USB hubs while testing."
}

#[cfg(all(not(windows), not(target_os = "linux"), not(target_os = "macos")))]
fn usb_permission_hint() -> &'static str {
    "Hint: configure USB permissions for this platform manually."
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn linux_rule_targets_artinchip_upgrade_vid_pid() {
        let rule = linux_udev_rule();
        assert!(rule.contains("33c3"));
        assert!(rule.contains("6677"));
        assert!(rule.contains("uaccess"));
    }

    #[test]
    fn app_paths_end_with_project_name() {
        assert_eq!(
            default_app_dir()
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap(),
            "artinchip-flash"
        );
        assert_eq!(config_path().file_name().unwrap(), "config.ini");
        assert_eq!(image_history_path().file_name().unwrap(), "img_history.txt");
    }
}
