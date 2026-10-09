//! Shared service layer used by both CLI (`src/main.rs`) and GUI.
//!
//! Before this module the two frontends each reimplemented UART open logic,
//! image summary loading and partition-key mapping. Frontends should call
//! these helpers instead of duplicating them.

use std::path::{Path, PathBuf};

use crate::device::UpgDevice;
use crate::image::parser::{self, ImageSummary};
use crate::transport::UpgTransport;
use crate::uart::{UartDevice, UartOptions};
use crate::usb::device::AicDevice;

/// How the caller wants to reach the device over UART.
#[derive(Clone, Debug)]
pub struct UartSpec {
    pub port: String,
    pub baud: u32,
    pub speed: Option<u32>,
    pub auto_enter: bool,
}

impl UartSpec {
    pub fn new(port: &str, baud: u32, speed: Option<u32>, auto_enter: bool) -> Self {
        Self {
            port: port.to_string(),
            baud,
            speed,
            auto_enter,
        }
    }

    pub fn options(&self) -> UartOptions {
        UartOptions {
            baudrate: self.baud,
            max_baudrate: self.speed.filter(|speed| *speed > self.baud),
            auto_enter: self.auto_enter,
            ..Default::default()
        }
    }
}

/// Open a UART device. `"auto"` probes every serial port.
///
/// Single implementation shared by CLI and GUI (previously duplicated).
pub fn open_uart(spec: &UartSpec) -> Result<UartDevice, String> {
    let options = spec.options();
    if spec.port.eq_ignore_ascii_case("auto") || spec.port.trim().is_empty() {
        UartDevice::open_auto(options)
    } else {
        UartDevice::open_port(&spec.port, options)
    }
}

/// Open a device over either transport with one return type, so CLI commands
/// that run identically on USB and UART don't duplicate every call site.
/// `uart` is `None` for USB, `Some(port)` (or `"auto"`) for UART.
pub fn open_native_device(
    uart: Option<&str>,
    baud: u32,
    speed: Option<u32>,
    auto_enter: bool,
) -> Result<UpgDevice<Box<dyn UpgTransport>>, String> {
    if let Some(port) = uart {
        Ok(open_uart(&UartSpec::new(port, baud, speed, auto_enter))?.into_boxed())
    } else {
        Ok(AicDevice::open_first()?.into_boxed())
    }
}

/// Parse a user-supplied integer: `0x`-prefixed hex, decimal, or decimal
/// with a `k`/`m` suffix (`4k` = 4096, `1m` = 1048576). Underscores are
/// rejected; surrounding whitespace is trimmed.
pub fn parse_u32(text: &str) -> Result<u32, String> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Err("expected an address/length value, got empty string".to_string());
    }
    if let Some(hex) = trimmed
        .strip_prefix("0x")
        .or_else(|| trimmed.strip_prefix("0X"))
    {
        if hex.is_empty() {
            return Err(format!("invalid hex value '{}'", text));
        }
        return u32::from_str_radix(hex, 16)
            .map_err(|_| format!("invalid hex value '{}'", text));
    }
    let (digits, mult) = if let Some(base) = trimmed
        .strip_suffix('k')
        .or_else(|| trimmed.strip_suffix('K'))
    {
        (base, 1024u32)
    } else if let Some(base) = trimmed
        .strip_suffix('m')
        .or_else(|| trimmed.strip_suffix('M'))
    {
        (base, 1024 * 1024u32)
    } else {
        (trimmed, 1u32)
    };
    if digits.is_empty() {
        return Err(format!("invalid numeric value '{}'", text));
    }
    let value = digits
        .parse::<u32>()
        .map_err(|_| format!("invalid numeric value '{}'", text))?;
    value.checked_mul(mult).ok_or_else(|| {
        format!("numeric value '{}' overflows u32 after suffix", text)
    })
}

/// Classic 16-bytes-per-row hexdump (`addr: hex...  |ascii|`).
pub fn format_hexdump(base_addr: u32, data: &[u8]) -> String {
    let mut out = String::new();
    for (row, chunk) in data.chunks(16).enumerate() {
        let addr = base_addr.wrapping_add((row * 16) as u32);
        out.push_str(&format!("{:08x}:", addr));
        for i in 0..16 {
            if i == 8 {
                out.push(' ');
            }
            if let Some(byte) = chunk.get(i) {
                out.push_str(&format!(" {:02x}", byte));
            } else {
                out.push_str("   ");
            }
        }
        out.push_str("  |");
        for byte in chunk {
            out.push(if byte.is_ascii_graphic() || *byte == b' ' {
                *byte as char
            } else {
                '.'
            });
        }
        out.push_str("|\n");
    }
    out
}

/// Resolve the monitor port the same way CLI and GUI do.
pub fn resolve_monitor_port_name(configured: &str, available: &[String]) -> Option<String> {
    if !configured.is_empty() && !configured.eq_ignore_ascii_case("auto") {
        return Some(configured.to_string());
    }
    available
        .iter()
        .find(|_| true)
        .cloned()
        .or_else(|| available.first().cloned())
}

/// Load only the lightweight image summary (header + META, no payload).
pub fn load_image_summary(path: &Path) -> Result<ImageSummary, String> {
    parser::read_image_summary(path)
}

/// Full image load (header + metas + payload) for burn flows.
pub fn load_image_for_burn(
    path: &Path,
) -> Result<
    (
        Vec<u8>,
        parser::FwHeader,
        Vec<crate::protocol::commands::FwcMeta>,
        ImageSummary,
    ),
    String,
> {
    parser::read_image(path)
}

/// Partition key shared by CLI/GUI selection logic.
///
/// Mirrors the historical `image.target.<name>` stripping so both frontends
/// stay consistent.
pub fn partition_key(meta_name: &str) -> &str {
    meta_name
        .strip_prefix("image.target.")
        .unwrap_or(meta_name)
}

/// Returns true when `meta_name` is a user-selectable burn target.
pub fn is_target_component(meta_name: &str) -> bool {
    meta_name.starts_with("image.target.")
}

/// Overall burn progress in `[0.0, 1.0]`, shared so CLI percentage lines and
/// GUI progress bars agree.
pub fn progress_ratio(sent: usize, total: usize) -> f32 {
    if total == 0 {
        0.0
    } else {
        (sent as f32 / total as f32).clamp(0.0, 1.0)
    }
}

/// Timestamp stamp for log file names: `YYYYMMDD-HHMMSS` in UTC.
///
/// Implemented without a date dependency (Howard Hinnant's civil-from-days).
/// `secs` is Unix time; callers pass "now".
pub fn log_file_stamp(secs: u64) -> String {
    let days = (secs / 86_400) as i64;
    let secs_of_day = (secs % 86_400) as i64;
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = doy - (153 * mp + 2) / 5 + 1; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 }; // [1, 12]
    let year = if m <= 2 { y + 1 } else { y };
    format!(
        "{:04}{:02}{:02}-{:02}{:02}{:02}",
        year,
        m,
        d,
        secs_of_day / 3600,
        (secs_of_day % 3600) / 60,
        secs_of_day % 60
    )
}

/// Default per-run log path: `<base_dir>/logs/artinchip-flash-<stamp>.log`.
pub fn default_log_path(base_dir: &Path) -> PathBuf {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    base_dir.join("logs").join(format!(
        "artinchip-flash-{}.log",
        log_file_stamp(secs)
    ))
}

/// Write log lines to `path` (creating parent directories), shared by the CLI
/// `--log-file` flag and the GUI "save log" button.
pub fn write_log_file(path: &Path, lines: &[String]) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("Failed to create '{}': {}", parent.display(), e))?;
        }
    }
    let mut text = lines.join("\n");
    text.push('\n');
    std::fs::write(path, text)
        .map_err(|e| format!("Failed to write '{}': {}", path.display(), e))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn partition_key_strips_target_prefix() {
        assert_eq!(partition_key("image.target.kernel"), "kernel");
        assert_eq!(partition_key("image.info"), "image.info");
        assert!(is_target_component("image.target.rootfs"));
        assert!(!is_target_component("image.info"));
    }

    #[test]
    fn progress_ratio_clamps() {
        assert_eq!(progress_ratio(0, 0), 0.0);
        assert_eq!(progress_ratio(5, 10), 0.5);
        assert_eq!(progress_ratio(20, 10), 1.0);
    }

    #[test]
    fn uart_spec_filters_non_faster_speed() {
        let spec = UartSpec::new("auto", 115200, Some(9600), true);
        assert_eq!(spec.options().max_baudrate, None);
        let spec = UartSpec::new("COM3", 115200, Some(1_500_000), true);
        assert_eq!(spec.options().max_baudrate, Some(1_500_000));
    }

    #[test]
    fn log_file_stamp_uses_utc_calendar() {
        assert_eq!(log_file_stamp(0), "19700101-000000");
        assert_eq!(log_file_stamp(1_704_067_200), "20240101-000000");
        assert_eq!(log_file_stamp(1_704_067_200 + 3661), "20240101-010101");
    }

    #[test]
    fn default_log_path_shape() {
        let path = default_log_path(Path::new("cfg"));
        let text = path.to_string_lossy().replace('\\', "/");
        assert!(text.starts_with("cfg/logs/artinchip-flash-"), "{}", text);
        assert!(text.ends_with(".log"), "{}", text);
        assert!(!text.contains(':'), "{}", text);
    }

    #[test]
    fn parse_u32_accepts_hex_dec_and_suffixes() {
        assert_eq!(parse_u32("0x40000000").unwrap(), 0x40000000);
        assert_eq!(parse_u32("0Xff").unwrap(), 0xff);
        assert_eq!(parse_u32("4096").unwrap(), 4096);
        assert_eq!(parse_u32("  64 ").unwrap(), 64);
        assert_eq!(parse_u32("4k").unwrap(), 4096);
        assert_eq!(parse_u32("4K").unwrap(), 4096);
        assert_eq!(parse_u32("1m").unwrap(), 1024 * 1024);
        assert!(parse_u32("").is_err());
        assert!(parse_u32("0x").is_err());
        assert!(parse_u32("zz").is_err());
        assert!(parse_u32("12x").is_err());
        assert!(parse_u32("0x1_000").is_err());
        assert!(parse_u32("4294967296").is_err());
        assert!(parse_u32("4096m").is_err()); // suffix overflow
    }

    #[test]
    fn format_hexdump_shapes_rows() {
        let data: Vec<u8> = (0u8..32).collect();
        let text = format_hexdump(0x40000000, &data);
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 2);
        assert!(lines[0].starts_with("40000000:"));
        assert!(lines[0].contains("00 01 02 03 04 05 06 07  08 09 0a 0b 0c 0d 0e 0f"));
        assert!(lines[0].ends_with('|'));
        assert!(lines[1].starts_with("40000010:"));
        // Short tail row pads the hex area but keeps the ascii column tight.
        let tail = format_hexdump(0x40000000, &[0x41, 0x00, 0xff]);
        assert!(tail.contains(" 41 00 ff"));
        assert!(tail.contains("|A..|"));
        assert_eq!(format_hexdump(0x0, &[]), "");
    }

    #[test]
    fn write_log_file_round_trips_lines() {
        let unique = format!(
            "artinchip-flash-log-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        let dir = std::env::temp_dir().join(unique);
        let path = dir.join("logs").join("run.log");
        let lines = vec!["[00:01] hello".to_string(), "line2".to_string()];
        write_log_file(&path, &lines).unwrap();
        let back = std::fs::read_to_string(&path).unwrap();
        assert_eq!(back, "[00:01] hello\nline2\n");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
