//! Read-only physical-disk enumeration (`sd-list`).
//!
//! Official AiBurn's "制作启动卡" page enumerates SD/MMC disks, writes a
//! GPT/MBR layout, formats, and writes the MMC image — all requiring
//! administrator rights. Raw-disk writing is intentionally **not** implemented
//! here (Windows `\\.\PhysicalDriveN` + GPT work is tracked in
//! `docs/启动卡设计.md`); this module only provides the safe read-only half:
//! list the physical disks so users can confirm the target before using the
//! official tool.
//!
//! No new dependencies: Linux reads `/sys/block`, Windows shells out to
//! PowerShell `Get-Disk` (CSV) with a `wmic` fallback, macOS parses
//! `diskutil list`. All parsers are pure functions covered by unit tests.

use std::process::Command;

#[cfg(any(target_os = "linux", test))]
use std::path::Path;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PhysicalDisk {
    /// Stable id: `PhysicalDriveN` (Windows), `sda`/`mmcblk0` (Linux),
    /// `disk2` (macOS).
    pub id: String,
    pub model: String,
    pub size_bytes: u64,
    /// Bus/type hint: `USB`, `SATA`, `NVMe`, `MMC`, `SD`, or empty.
    pub bus_type: String,
    pub removable: Option<bool>,
}

impl PhysicalDisk {
    pub fn summary(&self) -> String {
        let size = format_size(self.size_bytes);
        let bus = if self.bus_type.is_empty() {
            String::new()
        } else {
            format!(" bus={}", self.bus_type)
        };
        let rem = match self.removable {
            Some(true) => " removable=yes".to_string(),
            Some(false) => String::new(),
            None => String::new(),
        };
        let model = if self.model.is_empty() {
            String::new()
        } else {
            format!(" {}", self.model)
        };
        format!("  {}{} size={}{}{}", self.id, model, size, bus, rem)
    }
}

pub fn format_size(bytes: u64) -> String {
    const GIB: f64 = 1024.0 * 1024.0 * 1024.0;
    const MIB: f64 = 1024.0 * 1024.0;
    const KIB: f64 = 1024.0;
    let value = bytes as f64;
    if value >= GIB {
        format!("{:.1} GiB", value / GIB)
    } else if value >= MIB {
        format!("{:.1} MiB", value / MIB)
    } else if value >= KIB {
        format!("{:.1} KiB", value / KIB)
    } else {
        format!("{} B", bytes)
    }
}

/// Parse a decimal size with optional `KB/MB/GB/TB` (SI) or
/// `KIB/MIB/GIB/TIB` suffix, as printed by `diskutil`/`Get-Disk`.
/// Returns bytes, or `None` when unparsable.
pub fn parse_human_size(text: &str) -> Option<u64> {
    // Strip footnotes like `500.1 GB (500107862016 Bytes)`.
    let primary = text.split('(').next().unwrap_or(text).trim();
    let mut parts = primary.split_whitespace();
    let number: f64 = parts.next()?.replace(',', "").parse().ok()?;
    let unit = parts.next().unwrap_or("B").to_ascii_uppercase();
    let mult: f64 = match unit.as_str() {
        "B" | "BYTE" | "BYTES" => 1.0,
        "KB" | "K" => 1_000.0,
        "MB" | "M" => 1_000_000.0,
        "GB" | "G" => 1_000_000_000.0,
        "TB" | "T" => 1_000_000_000_000.0,
        "KIB" | "KI" => 1024.0,
        "MIB" | "MI" => 1_048_576.0,
        "GIB" | "GI" => 1_073_741_824.0,
        "TIB" | "TI" => 1_099_511_627_776.0,
        _ => return None,
    };
    Some((number * mult) as u64)
}

/// Platform entry point (read-only; never writes).
pub fn list_disks() -> Result<Vec<PhysicalDisk>, String> {
    #[cfg(windows)]
    {
        list_windows_disks()
    }
    #[cfg(target_os = "linux")]
    {
        list_linux_disks(Path::new("/sys/block"))
    }
    #[cfg(target_os = "macos")]
    {
        list_macos_disks()
    }
    #[cfg(all(not(windows), not(target_os = "linux"), not(target_os = "macos")))]
    {
        Err("Physical-disk enumeration is not implemented for this platform".to_string())
    }
}

// ── Linux: /sys/block ────────────────────────────────────────────────

/// List disks from a `sysfs` block root (default `/sys/block`).
/// `sys_root` is a parameter so tests can inject a temp dir.
#[cfg(any(target_os = "linux", test))]
pub fn list_linux_disks(sys_root: &Path) -> Result<Vec<PhysicalDisk>, String> {
    let entries = std::fs::read_dir(sys_root)
        .map_err(|e| format!("Failed to read '{}': {}", sys_root.display(), e))?;
    let mut disks = Vec::new();
    let mut names: Vec<String> = entries
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().to_string())
        .filter(|n| {
            n.starts_with("sd")
                || n.starts_with("hd")
                || n.starts_with("vd")
                || n.starts_with("nvme")
                || n.starts_with("mmcblk")
        })
        .collect();
    names.sort();
    for name in names {
        let base = sys_root.join(&name);
        let sectors = read_trimmed(&base.join("size"))
            .ok()
            .and_then(|t| t.parse::<u64>().ok())
            .unwrap_or(0);
        let removable = read_trimmed(&base.join("removable"))
            .ok()
            .map(|t| t.trim() == "1");
        let model = read_trimmed(&base.join("device/model"))
            .or_else(|_| read_trimmed(&base.join("device/name")))
            .unwrap_or_default();
        let bus_type = if name.starts_with("mmcblk") {
            "MMC".to_string()
        } else if name.starts_with("nvme") {
            "NVMe".to_string()
        } else {
            String::new()
        };
        disks.push(PhysicalDisk {
            id: name,
            model: model.trim().to_string(),
            size_bytes: sectors.saturating_mul(512),
            bus_type,
            removable,
        });
    }
    Ok(disks)
}

#[cfg(any(target_os = "linux", test))]
fn read_trimmed(path: &Path) -> Result<String, String> {
    std::fs::read_to_string(path)
        .map(|t| t.trim().to_string())
        .map_err(|e| format!("Failed to read '{}': {}", path.display(), e))
}

// ── Windows: Get-Disk CSV, wmic fallback ────────────────────────────

/// Parse `Get-Disk | Select Number,FriendlyName,Size,BusType,IsSystem |
/// ConvertTo-Csv -NoTypeInformation` output.
pub fn parse_windows_get_disk_csv(text: &str) -> Vec<PhysicalDisk> {
    let mut out = Vec::new();
    for line in text.lines().map(str::trim).filter(|l| !l.is_empty()) {
        if line.starts_with('"') && line.contains("Number") && line.contains("FriendlyName") {
            continue; // header
        }
        let fields = split_csv_line(line);
        if fields.len() < 3 {
            continue;
        }
        // Columns: Number, FriendlyName, Size, [BusType, IsSystem...]
        let number = fields[0].trim().trim_matches('"');
        if number.parse::<u32>().is_err() {
            continue;
        }
        let model = fields[1].trim().trim_matches('"').to_string();
        let size_bytes = fields[2]
            .trim()
            .trim_matches('"')
            .parse::<u64>()
            .unwrap_or(0);
        let bus_type = fields.get(3).map(|s| s.trim().trim_matches('"').to_string()).unwrap_or_default();
        out.push(PhysicalDisk {
            id: format!("PhysicalDrive{}", number),
            model,
            size_bytes,
            bus_type,
            removable: None,
        });
    }
    out
}

/// Parse `wmic diskdrive get DeviceID,Model,Size /format:csv` output.
pub fn parse_windows_wmic_csv(text: &str) -> Vec<PhysicalDisk> {
    let mut out = Vec::new();
    for line in text.lines().map(str::trim).filter(|l| !l.is_empty()) {
        if line.starts_with("Node,") || line.starts_with("Node,DeviceID") {
            continue; // header
        }
        let fields = split_csv_line(line);
        // Node,DeviceID,Model,Size
        if fields.len() < 4 {
            continue;
        }
        let device_id = fields[1].trim();
        let model = fields[2].trim().to_string();
        let size_bytes = fields[3].trim().parse::<u64>().unwrap_or(0);
        // `\\.\PHYSICALDRIVE2` -> `PhysicalDrive2`
        let id = device_id
            .rsplit('\\')
            .next()
            .unwrap_or(device_id)
            .to_string();
        if !id.to_ascii_uppercase().starts_with("PHYSICALDRIVE") {
            continue;
        }
        out.push(PhysicalDisk {
            id,
            model,
            size_bytes,
            bus_type: String::new(),
            removable: None,
        });
    }
    out
}

fn split_csv_line(line: &str) -> Vec<String> {
    let mut fields = Vec::new();
    let mut cur = String::new();
    let mut in_quotes = false;
    for ch in line.chars() {
        match ch {
            '"' => in_quotes = !in_quotes,
            ',' if !in_quotes => fields.push(std::mem::take(&mut cur)),
            _ => cur.push(ch),
        }
    }
    fields.push(cur);
    fields
}

#[cfg(windows)]
fn list_windows_disks() -> Result<Vec<PhysicalDisk>, String> {
    // Primary: PowerShell Get-Disk as CSV (locale-independent numbers).
    let ps = Command::new("powershell")
        .args([
            "-NoProfile",
            "-Command",
            "Get-Disk | Select-Object Number,FriendlyName,Size,BusType | ConvertTo-Csv -NoTypeInformation",
        ])
        .output();
    if let Ok(output) = ps {
        let text = String::from_utf8_lossy(&output.stdout).to_string();
        let disks = parse_windows_get_disk_csv(&text);
        if !disks.is_empty() {
            return Ok(disks);
        }
    }
    // Fallback: wmic CSV.
    let wmic = Command::new("wmic")
        .args(["diskdrive", "get", "DeviceID,Model,Size", "/format:csv"])
        .output()
        .map_err(|e| format!("Failed to enumerate disks (powershell + wmic): {}", e))?;
    let text = String::from_utf8_lossy(&wmic.stdout).to_string();
    let disks = parse_windows_wmic_csv(&text);
    if disks.is_empty() {
        return Err("No physical disks found (tried Get-Disk and wmic)".to_string());
    }
    Ok(disks)
}

// ── macOS: diskutil list ─────────────────────────────────────────────

/// Parse `diskutil list` human output. Picks up `/dev/diskN (…):` headers and
/// the `SIZE` column of the first section row (`0: … *500.1 GB disk2`).
pub fn parse_macos_diskutil(text: &str) -> Vec<PhysicalDisk> {
    let mut out = Vec::new();
    let mut current: Option<PhysicalDisk> = None;
    for raw in text.lines() {
        let line = raw.trim();
        if let Some(rest) = line.strip_prefix("/dev/") {
            if let Some(prev) = current.take() {
                out.push(prev);
            }
            let id = rest.split_whitespace().next().unwrap_or("").to_string();
            let bus_type = if rest.to_ascii_lowercase().contains("external") {
                "USB".to_string()
            } else {
                String::new()
            };
            current = Some(PhysicalDisk {
                id,
                model: String::new(),
                size_bytes: 0,
                bus_type,
                removable: None,
            });
        } else if line.starts_with("0:") {
            // e.g. `0: FDisk_partition_scheme *500.1 GB disk2`
            if let Some(disk) = current.as_mut() {
                if let Some(size_token) = extract_macos_size(line) {
                    if let Some(bytes) = parse_human_size(&size_token) {
                        disk.size_bytes = bytes;
                    }
                }
            }
        }
    }
    if let Some(prev) = current.take() {
        out.push(prev);
    }
    out.into_iter().filter(|d| !d.id.is_empty()).collect()
}

/// Extract the `*500.1 GB`-style size token from a `diskutil` section row.
fn extract_macos_size(line: &str) -> Option<String> {
    let star = line.find('*')?;
    let after = line[star + 1..].trim();
    let mut parts = after.split_whitespace();
    Some(format!("{} {}", parts.next()?, parts.next().unwrap_or("B")))
}

#[cfg(target_os = "macos")]
fn list_macos_disks() -> Result<Vec<PhysicalDisk>, String> {
    let output = Command::new("diskutil")
        .args(["list"])
        .output()
        .map_err(|e| format!("Failed to run 'diskutil list': {}", e))?;
    let text = String::from_utf8_lossy(&output.stdout).to_string();
    let disks = parse_macos_diskutil(&text);
    if disks.is_empty() {
        return Err("No disks found in 'diskutil list' output".to_string());
    }
    Ok(disks)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_windows_get_disk_csv() {
        let text = "\"Number\",\"FriendlyName\",\"Size\",\"BusType\"\n\"0\",\"Samsung SSD 970\",\"512110190592\",\"NVMe\"\n\"1\",\"Generic SD Card\",\"31914983424\",\"USB\"\n";
        let disks = parse_windows_get_disk_csv(text);
        assert_eq!(disks.len(), 2);
        assert_eq!(disks[0].id, "PhysicalDrive0");
        assert_eq!(disks[0].size_bytes, 512110190592);
        assert_eq!(disks[0].bus_type, "NVMe");
        assert_eq!(disks[1].id, "PhysicalDrive1");
        assert_eq!(disks[1].model, "Generic SD Card");
    }

    #[test]
    fn parses_windows_wmic_csv() {
        let text = "Node,DeviceID,Model,Size\nHOST,\\\\.\\PHYSICALDRIVE0,Samsung SSD 970,512110190592\nHOST,\\\\.\\PHYSICALDRIVE1,Generic SD Card,31914983424\n";
        let disks = parse_windows_wmic_csv(text);
        assert_eq!(disks.len(), 2);
        assert_eq!(disks[1].id, "PHYSICALDRIVE1");
        assert_eq!(disks[1].size_bytes, 31914983424);
    }

    #[test]
    fn parses_macos_diskutil() {
        let text = "/dev/disk2 (external, physical):\n   #:                       TYPE NAME                    SIZE       IDENTIFIER\n   0:     FDisk_partition_scheme                        *31.9 GB     disk2\n/dev/disk0 (internal, physical):\n   #:                       TYPE NAME                    SIZE       IDENTIFIER\n   0:      GUID_partition_scheme                        *500.1 GB   disk0\n";
        let disks = parse_macos_diskutil(text);
        assert_eq!(disks.len(), 2);
        assert_eq!(disks[0].id, "disk2");
        assert_eq!(disks[0].bus_type, "USB");
        assert!(disks[0].size_bytes > 30_000_000_000);
        assert_eq!(disks[1].id, "disk0");
    }

    #[test]
    fn parses_human_sizes() {
        assert_eq!(parse_human_size("500.1 GB"), Some(500_100_000_000));
        assert_eq!(parse_human_size("31.9 GB"), Some(31_900_000_000));
        assert_eq!(parse_human_size("512 MiB"), Some(512 * 1024 * 1024));
        assert_eq!(parse_human_size("1024"), Some(1024));
        assert_eq!(parse_human_size("nonsense"), None);
    }

    #[test]
    fn linux_lists_injected_sysfs() {
        let unique = format!(
            "artinchip-flash-sys-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        let root = std::env::temp_dir().join(unique);
        let sda = root.join("sda");
        let mmc = root.join("mmcblk0");
        std::fs::create_dir_all(sda.join("device")).unwrap();
        std::fs::create_dir_all(mmc.join("device")).unwrap();
        std::fs::write(sda.join("size"), "62514288\n").unwrap();
        std::fs::write(sda.join("removable"), "0\n").unwrap();
        std::fs::write(sda.join("device/model"), "Samsung SSD\n").unwrap();
        std::fs::write(mmc.join("size"), "62357504\n").unwrap();
        std::fs::write(mmc.join("removable"), "1\n").unwrap();
        std::fs::write(mmc.join("device/name"), "SD32G\n").unwrap();
        let disks = list_linux_disks(&root).unwrap();
        assert_eq!(disks.len(), 2);
        assert_eq!(disks[0].id, "mmcblk0");
        assert_eq!(disks[0].bus_type, "MMC");
        assert_eq!(disks[0].removable, Some(true));
        assert_eq!(disks[1].id, "sda");
        assert_eq!(disks[1].size_bytes, 62514288 * 512);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn summary_line_mentions_id_and_size() {
        let disk = PhysicalDisk {
            id: "PhysicalDrive1".to_string(),
            model: "Generic SD".to_string(),
            size_bytes: 32 * 1024 * 1024 * 1024,
            bus_type: "USB".to_string(),
            removable: Some(true),
        };
        let line = disk.summary();
        assert!(line.contains("PhysicalDrive1"));
        assert!(line.contains("GiB"));
        assert!(line.contains("USB"));
    }
}
