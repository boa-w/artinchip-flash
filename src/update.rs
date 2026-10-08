//! Update checking against GitHub Releases.
//!
//! Phase 1 is check-only + guided download (no self-replacement). This is
//! deliberate: the project ships both portable archives and native installers
//! (`msi/setup/deb/pkg`), and only portable checkouts can safely replace
//! their own binary. Installer users are directed to the release page.
//!
//! Uses the mainstream `self_update` crate for the GitHub listing API so we
//! don't hand-roll pagination, auth-token handling or rate-limit errors.

use crate::build_info;

pub const REPO_OWNER: &str = "boa-w";
pub const REPO_NAME: &str = "artinchip-flash";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UpdateChannel {
    Stable,
    Nightly,
}

impl UpdateChannel {
    pub fn from_str(value: &str) -> Self {
        if value.eq_ignore_ascii_case("nightly") {
            UpdateChannel::Nightly
        } else {
            UpdateChannel::Stable
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            UpdateChannel::Stable => "stable",
            UpdateChannel::Nightly => "nightly",
        }
    }
}

#[derive(Clone, Debug)]
pub struct UpdateStatus {
    pub channel: UpdateChannel,
    pub current_version: String,
    pub current_commit: String,
    pub latest_tag: String,
    pub latest_version: Option<String>,
    pub html_url: String,
    pub notes_preview: String,
    pub update_available: bool,
}

impl UpdateStatus {
    pub fn summary_line(&self) -> String {
        match self.channel {
            UpdateChannel::Stable => {
                let latest = self.latest_tag.as_str();
                if self.update_available {
                    format!(
                        "Update available: {} -> {} ({})",
                        self.current_version, latest, self.html_url
                    )
                } else {
                    format!(
                        "Up to date: {} (latest {})",
                        self.current_version, latest
                    )
                }
            }
            UpdateChannel::Nightly => {
                if self.update_available {
                    format!("Nightly update available: {}", self.html_url)
                } else {
                    "Nightly up to date".to_string()
                }
            }
        }
    }

    pub fn to_json(&self) -> String {
        serde_json::json!({
            "channel": self.channel.as_str(),
            "current_version": self.current_version,
            "current_commit": self.current_commit,
            "latest_tag": self.latest_tag,
            "latest_version": self.latest_version,
            "html_url": self.html_url,
            "update_available": self.update_available,
        })
        .to_string()
    }
}

/// Check for updates. Never panics on network failure: errors are `String`.
pub fn check(channel: UpdateChannel) -> Result<UpdateStatus, String> {
    match channel {
        UpdateChannel::Stable => check_stable(),
        UpdateChannel::Nightly => check_nightly(),
    }
}

fn check_stable() -> Result<UpdateStatus, String> {
    let listing = self_update::backends::github::ReleaseList::configure()
        .repo_owner(REPO_OWNER)
        .repo_name(REPO_NAME)
        .build()
        .map_err(|e| map_backend_error(e.to_string()))?
        .fetch()
        .map_err(|e| map_backend_error(e.to_string()))?;

    let current = build_info::VERSION.to_string();
    let current_parsed =
        semver::Version::parse(current.trim_start_matches('v')).unwrap_or(semver::Version::new(0, 0, 0));

    // `Releases` skips non-semver tags (e.g. `nightly`) internally; iterate what remains.
    let mut best: Option<(semver::Version, String, String, String)> = None;
    for release in listing.into_vec() {
        let tag_version = release.version().to_string();
        let Ok(parsed) = semver::Version::parse(tag_version.trim_start_matches('v')) else {
            continue;
        };
        // Skip prereleases for the stable channel unless they are newer stable? Keep it
        // simple: prerelease versions never win the stable channel.
        if !parsed.pre.is_empty() {
            continue;
        }
        let is_better = best.as_ref().is_none_or(|(v, _, _, _)| parsed > *v);
        if is_better {
            best = Some((
                parsed,
                tag_version,
                release
                    .release_notes_url()
                    .unwrap_or_default()
                    .to_string(),
                release.body().unwrap_or_default().to_string(),
            ));
        }
    }

    let Some((latest_parsed, latest_version, html_url, body)) = best else {
        return Err("No stable releases found (only prereleases/nightly exist yet). Create a `v*` tag release first.".to_string());
    };

    Ok(UpdateStatus {
        channel: UpdateChannel::Stable,
        current_version: current.clone(),
        current_commit: build_info::COMMIT.to_string(),
        latest_tag: format!("v{latest_parsed}"),
        latest_version: Some(latest_version),
        html_url: if html_url.is_empty() {
            releases_url()
        } else {
            html_url
        },
        notes_preview: first_lines(&body, 8),
        update_available: latest_parsed > current_parsed,
    })
}

fn check_nightly() -> Result<UpdateStatus, String> {
    // Nightly is a moving non-semver tag which `self_update` listings skip by
    // design, so there is no reliable commit comparison yet. Report the nightly
    // page and only claim an update when the local build has no commit
    // (dev/unknown builds). Once CI embeds nightly SHAs in assets, extend this
    // to compare them.
    let current_commit = build_info::COMMIT.to_string();
    Ok(UpdateStatus {
        channel: UpdateChannel::Nightly,
        current_version: build_info::VERSION.to_string(),
        current_commit: current_commit.clone(),
        latest_tag: "nightly".to_string(),
        latest_version: None,
        html_url: nightly_url(),
        notes_preview: String::new(),
        update_available: current_commit == "unknown" || current_commit.is_empty(),
    })
}

/// True when the current executable lives in a user-writable portable checkout.
///
/// Installer locations (`Program Files`, `/usr/bin`, `/Applications`,
/// `/opt`) must NOT self-replace; they need a re-run of the installer.
pub fn is_portable_install() -> bool {
    let Ok(exe) = std::env::current_exe() else {
        return false;
    };
    let path = exe.to_string_lossy().to_lowercase();
    for managed in [
        "program files",
        "/usr/bin",
        "/usr/local/bin",
        "/applications/",
        "/opt/",
    ] {
        if path.contains(managed) {
            return false;
        }
    }
    // Writable parent dir ~= portable checkout we are allowed to replace later.
    exe.parent()
        .map(|parent| !parent.as_os_str().is_empty() && writable(parent))
        .unwrap_or(false)
}

fn writable(dir: &std::path::Path) -> bool {
    use std::io::Write;
    let probe = dir.join(".artinchip-flash-write-test");
    match std::fs::File::create(&probe) {
        Ok(mut file) => {
            let _ = file.write_all(b"ok");
            drop(file);
            let _ = std::fs::remove_file(&probe);
            true
        }
        Err(_) => false,
    }
}

pub fn releases_url() -> String {
    format!("https://github.com/{REPO_OWNER}/{REPO_NAME}/releases")
}

pub fn nightly_url() -> String {
    format!("https://github.com/{REPO_OWNER}/{REPO_NAME}/releases/tag/nightly")
}

/// Open a URL with the OS default handler without adding an `open` dependency.
pub fn open_url(url: &str) -> Result<(), String> {
    #[cfg(windows)]
    {
        std::process::Command::new("cmd")
            .args(["/C", "start", "", url])
            .spawn()
            .map_err(|e| format!("Failed to open '{}': {}", url, e))?;
        Ok(())
    }
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("open")
            .arg(url)
            .spawn()
            .map_err(|e| format!("Failed to open '{}': {}", url, e))?;
        Ok(())
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        std::process::Command::new("xdg-open")
            .arg(url)
            .spawn()
            .map_err(|e| format!("Failed to open '{}': {}", url, e))?;
        Ok(())
    }
    #[cfg(all(not(windows), not(unix)))]
    {
        Err(format!("Opening URLs is not implemented: {}", url))
    }
}

fn first_lines(body: &str, max: usize) -> String {
    body.lines().take(max).collect::<Vec<_>>().join("\n")
}

fn map_backend_error(message: String) -> String {
    if message.contains("RateLimited") || message.contains("rate limit") || message.contains("403") {
        return format!(
            "GitHub API rate limited (60 req/h unauthenticated). Set GH_TOKEN/GITHUB_TOKEN and retry. Detail: {}",
            message
        );
    }
    if message.contains("NoReleaseFound") || message.contains("404") {
        return format!(
            "No releases found. The project currently only publishes `nightly`; create a `v*` tag release first. Detail: {}",
            message
        );
    }
    format!("Update check failed: {}. See {}", message, releases_url())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn channel_parsing_defaults_to_stable() {
        assert_eq!(UpdateChannel::from_str("nightly"), UpdateChannel::Nightly);
        assert_eq!(UpdateChannel::from_str("STABLE"), UpdateChannel::Stable);
        assert_eq!(UpdateChannel::from_str("bogus"), UpdateChannel::Stable);
    }

    #[test]
    fn summary_and_json_shape() {
        let status = UpdateStatus {
            channel: UpdateChannel::Stable,
            current_version: "0.1.0".to_string(),
            current_commit: "abc".to_string(),
            latest_tag: "v0.2.0".to_string(),
            latest_version: Some("0.2.0".to_string()),
            html_url: "https://example.test/r".to_string(),
            notes_preview: String::new(),
            update_available: true,
        };
        assert!(status.summary_line().contains("0.1.0 -> v0.2.0"));
        let json = status.to_json();
        assert!(json.contains("\"update_available\":true"));
    }

    #[test]
    fn releases_urls_point_at_repo() {
        assert!(releases_url().contains(REPO_NAME));
        assert!(nightly_url().ends_with("/nightly"));
    }
}
