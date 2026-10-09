//! Conflicting-service detection for `env-check`.
//!
//! Official AiBurn setup warns about (and can stop/disable) background
//! services that grab USB devices — notably VMware/VirtualBox USB helpers.
//! This module implements the *detection + manual-fix hint* half without any
//! new dependencies:
//!
//! - Windows: parse one `sc query type= service state= all` snapshot and
//!   filter for known USB-grabbing services by name/display heuristics.
//! - Linux/macOS: look for running VMware/VirtualBox processes via `pgrep`
//!   (best effort); otherwise emit the equivalent manual hint.
//!
//! The tool deliberately does **not** stop/disable anything automatically:
//! that needs elevation and can break a running VM. `report()` prints the
//! exact `sc stop` / `sc config ... start= disabled` commands instead.

use std::process::Command;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ConflictService {
    /// Windows service name (e.g. `VMUSBArbService`).
    pub name: String,
    /// Display name from `sc query` (may be empty when unknown).
    pub display: String,
    /// Raw state word from `sc query` (e.g. `RUNNING`, `STOPPED`).
    pub state: String,
}

impl ConflictService {
    pub fn running(&self) -> bool {
        self.state.eq_ignore_ascii_case("running")
    }

    pub fn fix_hint(&self) -> String {
        format!(
            "  sc stop {} & sc config {} start= disabled  (run as Administrator; needs reboot of dependent VMs)",
            self.name, self.name
        )
    }
}

/// Heuristic: service names/displays known to grab USB devices.
///
/// Kept intentionally narrow (USB arbitration helpers only) to avoid false
/// positives. Matching is case-insensitive substring search, so minor vendor
/// renames (`VMUSBArbService` vs `vmusb*`) still match.
fn is_usb_conflict(name: &str, display: &str) -> bool {
    let hay = format!("{} {}", name, display).to_ascii_lowercase();
    // VMware USB Arbitration Service.
    if hay.contains("vmusbarb") || hay.contains("vmware usb") {
        return true;
    }
    // VirtualBox USB monitor / system service.
    if hay.contains("vboxusb") || hay.contains("virtualbox usb") {
        return true;
    }
    // Generic "USB arbitration" helpers from other hypervisors.
    if hay.contains("usb arbitra") {
        return true;
    }
    false
}

/// Parse `sc query type= service state= all` output into
/// `(service_name, display_name, state)` triples.
///
/// `sc` block shape (both orders seen in the wild):
///
/// ```text
/// SERVICE_NAME: VMUSBArbService
/// DISPLAY_NAME: VMware USB Arbitration Service
///         TYPE               : 10  WIN32_OWN_PROCESS
///         STATE              : 4  RUNNING
/// ```
pub fn parse_sc_query(output: &str) -> Vec<(String, String, String)> {
    let mut out = Vec::new();
    let mut name = String::new();
    let mut display = String::new();
    let mut state = String::new();

    let flush = |name: &mut String,
                 display: &mut String,
                 state: &mut String,
                 out: &mut Vec<(String, String, String)>| {
        if !name.is_empty() {
            out.push((
                std::mem::take(name),
                std::mem::take(display),
                std::mem::take(state),
            ));
        } else {
            display.clear();
            state.clear();
        }
    };

    for raw in output.lines() {
        let line = raw.trim();
        if let Some(rest) = line.strip_prefix("SERVICE_NAME:") {
            flush(&mut name, &mut display, &mut state, &mut out);
            name = rest.trim().to_string();
        } else if let Some(rest) = line.strip_prefix("DISPLAY_NAME:") {
            display = rest.trim().to_string();
        } else if let Some(rest) = line.strip_prefix("STATE") {
            // `STATE              : 4  RUNNING`
            if let Some(after_colon) = rest.split_once(':') {
                let word = after_colon
                    .1
                    .split_whitespace()
                    .find(|w| w.chars().any(|c| c.is_ascii_alphabetic()))
                    .unwrap_or("")
                    .to_string();
                if !word.is_empty() {
                    state = word;
                }
            }
        }
    }
    flush(&mut name, &mut display, &mut state, &mut out);
    out
}

/// Filter a parsed `sc query` snapshot down to USB conflicts.
pub fn filter_conflicts(parsed: Vec<(String, String, String)>) -> Vec<ConflictService> {
    parsed
        .into_iter()
        .filter(|(name, display, _)| is_usb_conflict(name, display))
        .map(|(name, display, state)| ConflictService {
            name,
            display,
            state: if state.is_empty() {
                "UNKNOWN".to_string()
            } else {
                state
            },
        })
        .collect()
}

#[cfg(windows)]
fn query_windows_services() -> Result<Vec<ConflictService>, String> {
    let output = Command::new("sc")
        .args(["query", "type=", "service", "state=", "all"])
        .output()
        .map_err(|e| format!("Failed to run 'sc query': {}", e))?;
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    if !output.status.success() && text.trim().is_empty() {
        return Err(format!("'sc query' exited with {}", output.status));
    }
    Ok(filter_conflicts(parse_sc_query(&text)))
}

#[cfg(not(windows))]
fn query_unix_processes() -> Vec<String> {
    // Best effort: surface running hypervisor helpers that commonly hold USB.
    let probe = Command::new("pgrep")
        .args(["-a", "-i", "vmware|virtualbox|vbox"])
        .output();
    let Ok(output) = probe else {
        return Vec::new();
    };
    if !output.status.success() {
        return Vec::new();
    }
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .take(10)
        .map(str::to_string)
        .collect()
}

/// One report block appended to `env-check` output.
pub fn report() -> String {
    #[cfg(windows)]
    {
        match query_windows_services() {
            Ok(conflicts) if conflicts.is_empty() => {
                "Conflicting services: none detected (VMware/VirtualBox USB helpers)".to_string()
            }
            Ok(conflicts) => {
                let mut lines = vec![format!(
                    "Conflicting services: {} detected (may grab USB devices):",
                    conflicts.len()
                )];
                for svc in &conflicts {
                    let detail = if svc.display.is_empty() {
                        svc.name.clone()
                    } else {
                        format!("{} ({})", svc.display, svc.name)
                    };
                    lines.push(format!(
                        "  {} state={}",
                        detail,
                        if svc.state.is_empty() {
                            "UNKNOWN"
                        } else {
                            &svc.state
                        }
                    ));
                    if svc.running() {
                        lines.push(svc.fix_hint());
                    } else {
                        lines.push(format!(
                            "  stopped; if it interferes later: sc config {} start= disabled (Administrator)",
                            svc.name
                        ));
                    }
                }
                lines.join("\n")
            }
            Err(e) => format!("Conflicting services: check skipped ({})", e),
        }
    }
    #[cfg(target_os = "linux")]
    {
        let procs = query_unix_processes();
        if procs.is_empty() {
            "Conflicting services: none detected (no VMware/VirtualBox USB processes via pgrep); if USB open fails, quit hypervisors holding the board, then reconnect."
                .to_string()
        } else {
            let mut lines = vec![
                "Conflicting services: possible hypervisor USB holders detected:".to_string(),
            ];
            for proc in &procs {
                lines.push(format!("  {}", proc));
            }
            lines.push(
                "Quit the hypervisor or release the USB device, then reconnect the board."
                    .to_string(),
            );
            lines.join("\n")
        }
    }
    #[cfg(target_os = "macos")]
    {
        let procs = query_unix_processes();
        if procs.is_empty() {
            "Conflicting services: none detected (no VMware/VirtualBox USB processes via pgrep); if USB open fails, quit hypervisors and other USB tools, then reconnect directly (avoid hubs)."
                .to_string()
        } else {
            let mut lines = vec![
                "Conflicting services: possible hypervisor USB holders detected:".to_string(),
            ];
            for proc in &procs {
                lines.push(format!("  {}", proc));
            }
            lines.push(
                "Quit the hypervisor or release the USB device, then reconnect the board."
                    .to_string(),
            );
            lines.join("\n")
        }
    }
    #[cfg(all(not(windows), not(target_os = "linux"), not(target_os = "macos")))]
    {
        "Conflicting services: check with your hypervisor's USB settings; quit VM software holding VID 33C3/PID 6677, then reconnect."
            .to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "\
SERVICE_NAME: VMUSBArbService\r\n\
DISPLAY_NAME: VMware USB Arbitration Service\r\n\
        TYPE               : 10  WIN32_OWN_PROCESS\r\n\
        STATE              : 4  RUNNING\r\n\
\r\n\
SERVICE_NAME: wuauserv\r\n\
DISPLAY_NAME: Windows Update\r\n\
        TYPE               : 20  WIN32_SHARE_PROCESS\r\n\
        STATE              : 1  STOPPED\r\n\
\r\n\
SERVICE_NAME: VBoxSDS\r\n\
DISPLAY_NAME: VirtualBox system service\r\n\
        TYPE               : 10  WIN32_OWN_PROCESS\r\n\
        STATE              : 1  STOPPED\r\n";

    #[test]
    fn parses_sc_blocks() {
        let parsed = parse_sc_query(SAMPLE);
        assert_eq!(parsed.len(), 3);
        assert_eq!(parsed[0].0, "VMUSBArbService");
        assert_eq!(parsed[0].1, "VMware USB Arbitration Service");
        assert_eq!(parsed[0].2, "RUNNING");
        assert_eq!(parsed[1].2, "STOPPED");
    }

    #[test]
    fn filters_only_usb_conflicts() {
        let conflicts = filter_conflicts(parse_sc_query(SAMPLE));
        // VMUSBArbService matches (VMware USB); VBoxSDS display has no "USB"
        // word so it is intentionally *not* flagged — narrow heuristic.
        assert_eq!(conflicts.len(), 1);
        assert_eq!(conflicts[0].name, "VMUSBArbService");
        assert!(conflicts[0].running());
        assert!(conflicts[0].fix_hint().contains("sc stop VMUSBArbService"));
    }

    #[test]
    fn matches_vbox_usb_variants() {
        let parsed = vec![
            (
                "VBoxUSBMon".to_string(),
                "VirtualBox USB Monitor".to_string(),
                "RUNNING".to_string(),
            ),
            (
                "wuauserv".to_string(),
                "Windows Update".to_string(),
                "RUNNING".to_string(),
            ),
        ];
        let conflicts = filter_conflicts(parsed);
        assert_eq!(conflicts.len(), 1);
        assert_eq!(conflicts[0].name, "VBoxUSBMon");
    }

    #[test]
    fn empty_output_yields_no_conflicts() {
        assert!(filter_conflicts(parse_sc_query("")).is_empty());
        assert!(filter_conflicts(parse_sc_query("[SC] EnumQueryServicesStatus:OpenService FAILED 5:\n")).is_empty());
    }
}
