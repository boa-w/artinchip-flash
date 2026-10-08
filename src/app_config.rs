use std::fs;
use std::path::{Path, PathBuf};

use crate::standalone;

#[derive(Clone, Debug)]
pub struct AppConfig {
    pub auto_burn: bool,
    pub verbose: bool,
    pub read_device_log: bool,
    pub adb_scan: bool,
    pub retry_count: u32,
    pub block_error_log: bool,
    pub burn_timeout_secs: u64,
    pub language: String,
    pub image_path: Option<PathBuf>,
    pub selected_parts: Vec<String>,
    pub app_dir: PathBuf,
    pub aiburn_dir: PathBuf,
    pub upgcmd_path: PathBuf,
    pub transport: String,
    pub serial_port: String,
    pub serial_baud: u32,
    pub serial_speed: u32,
    pub serial_auto_enter: bool,
    pub update_channel: String,
    pub auto_check_update: bool,
    pub last_update_check_unix: u64,
    /// Official `AiBurn.ini` compat: show burn-statistics button. Read on load,
    /// preserved on save; the statistics window itself is on the roadmap.
    pub show_statistic: bool,
    /// Official `AiBurn.ini` compat: stats DB initialized flag. Preserved.
    pub db_inited: bool,
}

impl Default for AppConfig {
    fn default() -> Self {
        let aiburn_dir = default_compat_dir();
        let app_dir = standalone::default_app_dir();
        let upgcmd_path = default_compat_tool_path(&aiburn_dir);
        Self {
            auto_burn: false,
            verbose: false,
            read_device_log: false,
            adb_scan: false,
            retry_count: 1,
            block_error_log: false,
            burn_timeout_secs: 60,
            language: "zh_cn".to_string(),
            image_path: None,
            selected_parts: vec!["spl".to_string(), "env".to_string(), "os".to_string()],
            app_dir,
            upgcmd_path,
            aiburn_dir,
            transport: "usb".to_string(),
            serial_port: String::new(),
            serial_baud: 115200,
            serial_speed: 0,
            serial_auto_enter: true,
            update_channel: "stable".to_string(),
            auto_check_update: true,
            last_update_check_unix: 0,
            show_statistic: false,
            db_inited: false,
        }
    }
}

impl AppConfig {
    pub fn load_default() -> Self {
        let cfg = Self::default();
        let _ = standalone::migrate_legacy_app_data();
        let project_ini = standalone::config_path();
        if project_ini.exists() {
            if let Ok(mut loaded) = Self::load_from(&project_ini) {
                loaded.app_dir = cfg.app_dir;
                return loaded;
            }
        }
        let official_ini = cfg.aiburn_dir.join("AiBurn.ini");
        if official_ini.exists() {
            if let Ok(mut loaded) = Self::load_from(&official_ini) {
                loaded.app_dir = cfg.app_dir;
                return loaded;
            }
        }
        cfg
    }

    pub fn load_from(path: &Path) -> Result<Self, String> {
        let text = fs::read_to_string(path)
            .map_err(|e| format!("Failed to read '{}': {}", path.display(), e))?;
        let mut cfg = Self::default();
        if let Some(parent) = path.parent() {
            if path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.eq_ignore_ascii_case("config.ini"))
            {
                cfg.app_dir = parent.to_path_buf();
            } else if compat_tool_path(parent).exists() {
                cfg.aiburn_dir = parent.to_path_buf();
                cfg.upgcmd_path = compat_tool_path(parent);
            }
        }

        let mut section = String::new();
        for raw in text.lines() {
            let line = raw.trim();
            if line.is_empty() || line.starts_with(';') || line.starts_with('#') {
                continue;
            }
            if line.starts_with('[') && line.ends_with(']') {
                section = line[1..line.len() - 1].to_ascii_lowercase();
                continue;
            }
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            cfg.apply_ini_value(&section, key.trim(), unquote(value.trim()));
        }
        Ok(cfg)
    }

    pub fn save_to(&self, path: &Path) -> Result<(), String> {
        let selected = self.selected_parts.join(",");
        let image_path = self
            .image_path
            .as_ref()
            .map(|p| p.to_string_lossy().replace('\\', "/"))
            .unwrap_or_default();
        let text = format!(
            "[debug]\nauto_burn={}\nis_verbose={}\nread_device_log={}\nadb_scan={}\nretry_cnt={}\nblock_err_log={}\nshow_statistic={}\n\n[system]\nburn_timeout={}\nlanguage={}\ndb_inited={}\n\n[common]\nimage_path={}\nselected_parts=\"{}\"\napp_dir={}\naiburn_dir={}\nupgcmd_path={}\ntransport={}\nserial_port={}\nserial_baud={}\nserial_speed={}\nserial_auto_enter={}\nupdate_channel={}\nauto_check_update={}\nlast_update_check_unix={}\n",
            bool_to_int(self.auto_burn),
            bool_to_int(self.verbose),
            bool_to_int(self.read_device_log),
            bool_to_int(self.adb_scan),
            self.retry_count.max(1),
            bool_to_int(self.block_error_log),
            bool_to_int(self.show_statistic),
            self.burn_timeout_secs.max(1),
            self.language,
            bool_to_int(self.db_inited),
            image_path,
            selected,
            self.app_dir.to_string_lossy().replace('\\', "/"),
            self.aiburn_dir.to_string_lossy().replace('\\', "/"),
            self.upgcmd_path.to_string_lossy().replace('\\', "/"),
            self.transport,
            self.serial_port,
            self.serial_baud.max(1200),
            self.serial_speed,
            bool_to_int(self.serial_auto_enter),
            self.update_channel,
            bool_to_int(self.auto_check_update),
            self.last_update_check_unix
        );
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .map_err(|e| format!("Failed to create '{}': {}", parent.display(), e))?;
        }
        fs::write(path, text).map_err(|e| format!("Failed to write '{}': {}", path.display(), e))
    }

    fn apply_ini_value(&mut self, section: &str, key: &str, value: &str) {
        let key = key.to_ascii_lowercase();
        match (section, key.as_str()) {
            ("debug", "auto_burn") => self.auto_burn = parse_bool(value),
            ("debug", "is_verbose") => self.verbose = parse_bool(value),
            ("debug", "read_device_log") => self.read_device_log = parse_bool(value),
            ("debug", "adb_scan") => self.adb_scan = parse_bool(value),
            ("debug", "retry_cnt") => {
                self.retry_count = value.parse::<u32>().unwrap_or(self.retry_count).max(1)
            }
            ("debug", "block_err_log") => self.block_error_log = parse_bool(value),
            ("debug", "show_statistic") => self.show_statistic = parse_bool(value),
            ("system", "db_inited") => self.db_inited = parse_bool(value),
            ("system", "burn_timeout") => {
                self.burn_timeout_secs = value
                    .parse::<u64>()
                    .unwrap_or(self.burn_timeout_secs)
                    .max(1)
            }
            ("system", "language") => self.language = value.to_string(),
            ("common", "image_path") => {
                if !value.is_empty() {
                    self.image_path = Some(PathBuf::from(value));
                }
            }
            ("common", "selected_parts") => {
                self.selected_parts = value
                    .split(',')
                    .map(|part| part.trim().to_string())
                    .filter(|part| !part.is_empty())
                    .collect();
            }
            ("common", "app_dir") => {
                if !value.is_empty() {
                    self.app_dir = PathBuf::from(value);
                }
            }
            ("common", "aiburn_dir") => {
                if !value.is_empty() {
                    self.aiburn_dir = PathBuf::from(value);
                }
            }
            ("common", "upgcmd_path") => {
                if !value.is_empty() {
                    self.upgcmd_path = PathBuf::from(value);
                }
            }
            ("common", "transport") => {
                self.transport = if value.eq_ignore_ascii_case("uart") {
                    "uart".to_string()
                } else {
                    "usb".to_string()
                };
            }
            ("common", "serial_port") | ("common", "uart_port") => {
                self.serial_port = value.to_string();
            }
            ("common", "serial_baud") | ("common", "uart_baud") => {
                self.serial_baud = value.parse::<u32>().unwrap_or(self.serial_baud).max(1200);
            }
            ("common", "serial_speed") | ("common", "uart_speed") => {
                self.serial_speed = value.parse::<u32>().unwrap_or(self.serial_speed);
            }
            ("common", "serial_auto_enter") | ("common", "uart_auto_enter") => {
                self.serial_auto_enter = parse_bool(value);
            }
            ("common", "update_channel") => {
                self.update_channel = if value.eq_ignore_ascii_case("nightly") {
                    "nightly".to_string()
                } else {
                    "stable".to_string()
                };
            }
            ("common", "auto_check_update") => {
                self.auto_check_update = parse_bool(value);
            }
            ("common", "last_update_check_unix") => {
                self.last_update_check_unix = value.parse::<u64>().unwrap_or(0);
            }
            _ => {}
        }
    }
}

fn default_compat_dir() -> PathBuf {
    #[cfg(windows)]
    {
        PathBuf::from(r"C:\ArtInChip\AiBurn")
    }
    #[cfg(not(windows))]
    {
        PathBuf::new()
    }
}

pub fn compat_tool_name() -> &'static str {
    if cfg!(windows) {
        "upgcmd.exe"
    } else {
        "upgcmd"
    }
}

pub fn compat_tool_path(dir: &Path) -> PathBuf {
    dir.join(compat_tool_name())
}

fn default_compat_tool_path(dir: &Path) -> PathBuf {
    if dir.as_os_str().is_empty() {
        PathBuf::new()
    } else {
        compat_tool_path(dir)
    }
}

pub fn load_image_history(app_dir: &Path) -> Vec<(PathBuf, String)> {
    let path = app_dir.join("img_history.txt");
    let Ok(text) = fs::read_to_string(path) else {
        return Vec::new();
    };
    text.lines()
        .take(50)
        .filter_map(|line| {
            let (path, timestamp) = line.rsplit_once(',')?;
            Some((PathBuf::from(path.trim()), timestamp.trim().to_string()))
        })
        .collect()
}

pub fn append_image_history(app_dir: &Path, image: &Path) -> Result<(), String> {
    fs::create_dir_all(app_dir)
        .map_err(|e| format!("Failed to create '{}': {}", app_dir.display(), e))?;
    let path = app_dir.join("img_history.txt");
    let timestamp = current_timestamp();
    let line = format!(
        "{}, {}\n",
        image.to_string_lossy().replace('\\', "/"),
        timestamp
    );
    let normalized_image = image.to_string_lossy().replace('\\', "/");
    let old = fs::read_to_string(&path).unwrap_or_default();
    let old = old
        .lines()
        .filter(|old_line| !old_line.trim_start().starts_with(&normalized_image))
        .take(49)
        .collect::<Vec<_>>()
        .join("\n");
    let new_text = if old.is_empty() {
        line
    } else {
        format!("{}{}\n", line, old)
    };
    fs::write(&path, new_text).map_err(|e| format!("Failed to write '{}': {}", path.display(), e))
}

fn parse_bool(value: &str) -> bool {
    matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "1" | "true" | "yes" | "on"
    )
}

fn bool_to_int(value: bool) -> u8 {
    if value {
        1
    } else {
        0
    }
}

fn unquote(value: &str) -> &str {
    value
        .strip_prefix('"')
        .and_then(|v| v.strip_suffix('"'))
        .unwrap_or(value)
}

fn current_timestamp() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};

    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format!("unix_{}", secs)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compat_tool_name_matches_platform() {
        if cfg!(windows) {
            assert_eq!(compat_tool_name(), "upgcmd.exe");
        } else {
            assert_eq!(compat_tool_name(), "upgcmd");
        }
    }

    #[test]
    fn empty_default_compat_dir_does_not_create_fake_tool_path() {
        let path = default_compat_tool_path(Path::new(""));
        assert!(path.as_os_str().is_empty());
    }

    #[test]
    fn saves_and_loads_project_paths() {
        let unique = format!(
            "artinchip-flash-test-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        let dir = std::env::temp_dir().join(unique);
        let path = dir.join("config.ini");
        let mut cfg = AppConfig::default();
        cfg.app_dir = dir.clone();
        cfg.aiburn_dir = dir.join("compat");
        cfg.upgcmd_path = compat_tool_path(&cfg.aiburn_dir);
        cfg.selected_parts = vec!["spl".to_string(), "os".to_string()];
        cfg.transport = "uart".to_string();
        cfg.serial_port = "/dev/ttyUSB0".to_string();
        cfg.serial_baud = 921600;
        cfg.serial_speed = 1500000;
        cfg.serial_auto_enter = false;
        cfg.update_channel = "nightly".to_string();
        cfg.auto_check_update = false;
        cfg.last_update_check_unix = 1234567890;
        cfg.show_statistic = true;
        cfg.db_inited = true;

        cfg.save_to(&path).unwrap();
        let loaded = AppConfig::load_from(&path).unwrap();

        assert_eq!(loaded.app_dir, dir);
        assert_eq!(loaded.aiburn_dir, cfg.aiburn_dir);
        assert_eq!(loaded.upgcmd_path, cfg.upgcmd_path);
        assert_eq!(loaded.selected_parts, cfg.selected_parts);
        assert_eq!(loaded.transport, "uart");
        assert_eq!(loaded.serial_port, "/dev/ttyUSB0");
        assert_eq!(loaded.serial_baud, 921600);
        assert_eq!(loaded.serial_speed, 1500000);
        assert!(!loaded.serial_auto_enter);
        assert_eq!(loaded.update_channel, "nightly");
        assert!(!loaded.auto_check_update);
        assert_eq!(loaded.last_update_check_unix, 1234567890);
        assert!(loaded.show_statistic);
        assert!(loaded.db_inited);

        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn loads_official_aiburn_ini_sample() {
        // Sample taken from the AiBurn manual (§2.5.2): only official keys exist.
        let unique = format!(
            "artinchip-flash-ini-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        let dir = std::env::temp_dir().join(unique);
        let path = dir.join("AiBurn.ini");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            &path,
            "[common]\nimage_path=F:/img files/aic_per1_mmc_v1.0.0.img\n\n\
             [debug]\nauto_burn=0\nshow_statistic=0\nis_verbose=0\nretry_cnt=1\n\n\
             [system]\ndb_inited=1\n",
        )
        .unwrap();

        let loaded = AppConfig::load_from(&path).unwrap();
        assert_eq!(
            loaded.image_path,
            Some(PathBuf::from("F:/img files/aic_per1_mmc_v1.0.0.img"))
        );
        assert!(!loaded.auto_burn);
        assert!(!loaded.show_statistic);
        assert!(!loaded.verbose);
        assert_eq!(loaded.retry_count, 1);
        assert!(loaded.db_inited);

        let _ = fs::remove_dir_all(&dir);
    }
}
