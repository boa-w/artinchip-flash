use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=.git/HEAD");
    println!("cargo:rerun-if-changed=.git/index");
    println!("cargo:rerun-if-changed=assets/artinchip-flash.ico");

    #[cfg(windows)]
    {
        let mut resources = winres::WindowsResource::new();
        resources.set_icon("assets/artinchip-flash.ico");
        if let Err(error) = resources.compile() {
            panic!("failed to compile Windows icon resource: {error}");
        }
    }

    let commit =
        git_output(&["rev-parse", "--short=12", "HEAD"]).unwrap_or_else(|| "unknown".to_string());
    let full_commit = git_output(&["rev-parse", "HEAD"]).unwrap_or_else(|| "unknown".to_string());
    let dirty = Command::new("git")
        .args(["diff", "--quiet", "--ignore-submodules", "HEAD", "--"])
        .status()
        .map(|status| !status.success())
        .unwrap_or(false);
    let build = if dirty {
        format!("{}-dirty", commit)
    } else {
        commit
    };

    println!("cargo:rustc-env=ARTINCHIP_FLASH_BUILD={}", build);
    println!("cargo:rustc-env=ARTINCHIP_FLASH_COMMIT={}", full_commit);
}

fn git_output(args: &[&str]) -> Option<String> {
    let output = Command::new("git").args(args).output().ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8(output.stdout).ok()?;
    let text = text.trim();
    if text.is_empty() {
        None
    } else {
        Some(text.to_string())
    }
}
