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
            "[debug]\n\
auto_burn={}\n\
is_verbose={}\n\
read_device_log={}\n\
adb_scan={}\n\
retry_cnt={}\n\
block_err_log={}\n\
\n\
[system]\n\
burn_timeout={}\n\
language={}\n\
\n\
[common]\n\
image_path={}\n\
selected_parts=\"{}\"\n\
app_dir={}\n\
aiburn_dir={}\n\
upgcmd_path={}\n",
            bool_to_int(self.auto_burn),
            bool_to_int(self.verbose),
            bool_to_int(self.read_device_log),
            bool_to_int(self.adb_scan),
            self.retry_count.max(1),
            bool_to_int(self.block_error_log),
            self.burn_timeout_secs.max(1),
            self.language,
            image_path,
            selected,
            self.app_dir.to_string_lossy().replace('\\', "/"),
            self.aiburn_dir.to_string_lossy().replace('\\', "/"),
            self.upgcmd_path.to_string_lossy().replace('\\', "/")
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

        cfg.save_to(&path).unwrap();
        let loaded = AppConfig::load_from(&path).unwrap();

        assert_eq!(loaded.app_dir, dir);
        assert_eq!(loaded.aiburn_dir, cfg.aiburn_dir);
        assert_eq!(loaded.upgcmd_path, cfg.upgcmd_path);
        assert_eq!(loaded.selected_parts, cfg.selected_parts);

        let _ = fs::remove_dir_all(path.parent().unwrap());
    }
}
