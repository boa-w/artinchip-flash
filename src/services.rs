//! Shared service layer used by both CLI (`src/main.rs`) and GUI.
//!
//! Before this module the two frontends each reimplemented UART open logic,
//! image summary loading and partition-key mapping. Frontends should call
//! these helpers instead of duplicating them.

use std::path::Path;

use crate::image::parser::{self, ImageSummary};
use crate::uart::{UartDevice, UartOptions};

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
}
