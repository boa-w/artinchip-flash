use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OfficialCommand {
    ListDevices,
    ImageInfo,
    ExtractImage,
    UpgradeImage,
    DeviceLog,
    ShellCommand,
    ContinueBoot,
    GoToBootloader,
    WriteMemory,
    ReadMemory,
    WriteLong,
    ReadLong,
    MemTest,
    Exec,
    HexDump,
    Fill,
    Clear,
    DumpPartition,
    ListMedia,
    ListPartitions,
    FlashErase,
    RamBoot,
    JtagUnlockData,
    JtagUnlock,
    Raw,
    /// `bdefuse list` — list eFuse information (hidden in `upgcmd --help`,
    /// syntax from upgcmd V2.1.0 binary help strings, verified as a valid
    /// top-level command by probe: unknown commands print "not found").
    BdefuseList,
    /// `bdefuse select <id>` — select the eFuse.
    BdefuseSelect,
    /// `bdefuse read <start> <length> <file>` — read eFuse data to file.
    BdefuseRead,
    /// `bdefuse dump <start> <length>` — hex-dump eFuse data to console.
    BdefuseDump,
    /// `bdefuse write <start> <length> <file>` — write eFuse from file (OTP!).
    BdefuseWrite,
    /// `bdefuse writehex <start> <hexdata>` — write eFuse from hex data (OTP!).
    BdefuseWriteHex,
    /// `auzwritefuse <authorization file>` — write eFuse from an
    /// authorization file. Verified as valid top-level command by probe
    /// (`auzwitefuse` spelling is rejected); file format is undocumented.
    AuzWriteFuse,
    /// `bdreboot` — reboot the device (bulk-data session verb, zero args).
    /// Verified as a valid top-level command by probe (unknown commands print
    /// "not found", `bdreboot` proceeds to device-open). The AiBurn manual
    /// exits erase mode by rebooting.
    BdReboot,
}

impl OfficialCommand {
    pub const ALL: [OfficialCommand; 33] = [
        OfficialCommand::ListDevices,
        OfficialCommand::ImageInfo,
        OfficialCommand::ExtractImage,
        OfficialCommand::UpgradeImage,
        OfficialCommand::DeviceLog,
        OfficialCommand::ShellCommand,
        OfficialCommand::ContinueBoot,
        OfficialCommand::GoToBootloader,
        OfficialCommand::WriteMemory,
        OfficialCommand::ReadMemory,
        OfficialCommand::WriteLong,
        OfficialCommand::ReadLong,
        OfficialCommand::MemTest,
        OfficialCommand::Exec,
        OfficialCommand::HexDump,
        OfficialCommand::Fill,
        OfficialCommand::Clear,
        OfficialCommand::DumpPartition,
        OfficialCommand::ListMedia,
        OfficialCommand::ListPartitions,
        OfficialCommand::FlashErase,
        OfficialCommand::RamBoot,
        OfficialCommand::JtagUnlockData,
        OfficialCommand::JtagUnlock,
        OfficialCommand::Raw,
        OfficialCommand::BdefuseList,
        OfficialCommand::BdefuseSelect,
        OfficialCommand::BdefuseRead,
        OfficialCommand::BdefuseDump,
        OfficialCommand::BdefuseWrite,
        OfficialCommand::BdefuseWriteHex,
        OfficialCommand::AuzWriteFuse,
        OfficialCommand::BdReboot,
    ];

    pub fn label(self) -> &'static str {
        match self {
            OfficialCommand::ListDevices => "List devices",
            OfficialCommand::ImageInfo => "Image info",
            OfficialCommand::ExtractImage => "Extract image",
            OfficialCommand::UpgradeImage => "Upgrade image",
            OfficialCommand::DeviceLog => "Device log",
            OfficialCommand::ShellCommand => "Shell command",
            OfficialCommand::ContinueBoot => "Continue boot",
            OfficialCommand::GoToBootloader => "Go to bootloader",
            OfficialCommand::WriteMemory => "Write memory",
            OfficialCommand::ReadMemory => "Read memory",
            OfficialCommand::WriteLong => "Write 32-bit",
            OfficialCommand::ReadLong => "Read 32-bit",
            OfficialCommand::MemTest => "Memory test",
            OfficialCommand::Exec => "Exec",
            OfficialCommand::HexDump => "Hexdump",
            OfficialCommand::Fill => "Fill memory",
            OfficialCommand::Clear => "Clear memory",
            OfficialCommand::DumpPartition => "Dump partition",
            OfficialCommand::ListMedia => "List media",
            OfficialCommand::ListPartitions => "List partitions",
            OfficialCommand::FlashErase => "Flash erase",
            OfficialCommand::RamBoot => "RAM boot",
            OfficialCommand::JtagUnlockData => "JTAG unlock data",
            OfficialCommand::JtagUnlock => "JTAG unlock",
            OfficialCommand::Raw => "Raw upgcmd",
            OfficialCommand::BdefuseList => "eFuse list (bdefuse)",
            OfficialCommand::BdefuseSelect => "eFuse select (bdefuse)",
            OfficialCommand::BdefuseRead => "eFuse read (bdefuse)",
            OfficialCommand::BdefuseDump => "eFuse dump (bdefuse)",
            OfficialCommand::BdefuseWrite => "eFuse write (bdefuse)",
            OfficialCommand::BdefuseWriteHex => "eFuse write hex (bdefuse)",
            OfficialCommand::AuzWriteFuse => "eFuse authorized write",
            OfficialCommand::BdReboot => "Reboot device (bd)",
        }
    }
}

#[derive(Clone, Debug)]
pub struct OfficialArgs {
    pub command: OfficialCommand,
    pub image: Option<PathBuf>,
    pub output: Option<PathBuf>,
    pub input: Option<PathBuf>,
    pub media: String,
    pub partition: String,
    pub shell: String,
    pub fwc_name: String,
    pub ram_address: String,
    pub address: String,
    pub length: String,
    pub value: String,
    pub round: String,
    pub skip: String,
    pub fwc_list: String,
    pub raw_args: String,
    pub verbose: bool,
    pub device_log: bool,
    pub progress: bool,
    /// `upgcmd --dev bus:port` — operate on a specific USB upgrade device.
    /// Mirrors official `upgcmd -d`; empty means "first device".
    pub device: String,
    /// `upgcmd --uart COMx` — route the command over UART.
    /// Mirrors official `upgcmd -u`; empty means USB.
    pub uart_port: String,
    /// `upgcmd --baudrate <baud>` — max UART transmission baudrate.
    /// Mirrors official `upgcmd -b`; empty means default.
    pub baudrate: String,
    /// `bdefuse select` target id ("Select the eFuse"). Only used by
    /// [`OfficialCommand::BdefuseSelect`]; empty means unset.
    pub efuse_id: String,
}

impl Default for OfficialArgs {
    fn default() -> Self {
        Self {
            command: OfficialCommand::ListDevices,
            image: None,
            output: None,
            input: None,
            media: "0".to_string(),
            partition: String::new(),
            shell: String::new(),
            fwc_name: "image.target.kernel".to_string(),
            ram_address: "0x41000000".to_string(),
            address: "0x40000000".to_string(),
            length: "0x100".to_string(),
            value: "0x0".to_string(),
            round: "1".to_string(),
            skip: String::new(),
            fwc_list: String::new(),
            raw_args: String::new(),
            verbose: false,
            device_log: false,
            progress: true,
            device: String::new(),
            uart_port: String::new(),
            baudrate: String::new(),
            efuse_id: String::new(),
        }
    }
}

pub fn build_args(args: &OfficialArgs) -> Result<Vec<String>, String> {
    let mut out = Vec::new();
    if args.verbose {
        out.push("--verbose".to_string());
    }
    if args.device_log {
        out.push("--log".to_string());
    }
    if args.progress {
        out.push("--progress".to_string());
    }
    // Transport selectors mirror official `upgcmd -d/-u/-b` and must precede
    // the command word (`upgcmd options command arguments...`).
    if !args.device.trim().is_empty() {
        out.push("--dev".to_string());
        out.push(args.device.trim().to_string());
    }
    if !args.uart_port.trim().is_empty() {
        out.push("--uart".to_string());
        out.push(args.uart_port.trim().to_string());
    }
    if !args.baudrate.trim().is_empty() {
        out.push("--baudrate".to_string());
        out.push(args.baudrate.trim().to_string());
    }

    match args.command {
        OfficialCommand::ListDevices => out.push("--list".to_string()),
        OfficialCommand::ImageInfo => {
            out.push("--imagefile".to_string());
            out.push(required_path(args.image.as_deref(), "image file")?);
        }
        OfficialCommand::ExtractImage => {
            out.push("--extract".to_string());
            out.push(required_path(args.image.as_deref(), "image file")?);
        }
        OfficialCommand::UpgradeImage => {
            out.push("image".to_string());
            out.push(required_path(args.image.as_deref(), "image file")?);
            if !args.fwc_list.trim().is_empty() {
                out.push(args.fwc_list.trim().to_string());
            }
        }
        OfficialCommand::DeviceLog => out.push("log".to_string()),
        OfficialCommand::ShellCommand => {
            out.push("shcmd".to_string());
            out.push(required_text(&args.shell, "shell command")?);
        }
        OfficialCommand::ContinueBoot => out.push("continue".to_string()),
        OfficialCommand::GoToBootloader => out.push("gotobl".to_string()),
        OfficialCommand::WriteMemory => {
            out.push("write".to_string());
            out.push(required_text(&args.address, "address")?);
            out.push(required_path(args.input.as_deref(), "input file")?);
            if !args.skip.trim().is_empty() {
                out.push(args.skip.trim().to_string());
            }
            if !args.length.trim().is_empty() {
                out.push(args.length.trim().to_string());
            }
        }
        OfficialCommand::ReadMemory => {
            out.push("read".to_string());
            out.push(required_text(&args.address, "address")?);
            out.push(required_text(&args.length, "length")?);
            out.push(required_path(args.output.as_deref(), "output file")?);
        }
        OfficialCommand::WriteLong => {
            out.push("writel".to_string());
            out.push(required_text(&args.address, "address")?);
            out.push(required_text(&args.value, "value")?);
        }
        OfficialCommand::ReadLong => {
            out.push("readl".to_string());
            out.push(required_text(&args.address, "address")?);
        }
        OfficialCommand::MemTest => {
            out.push("memtest".to_string());
            out.push(required_text(&args.address, "start address")?);
            out.push(required_text(&args.length, "size")?);
            out.push(required_text(&args.round, "round")?);
        }
        OfficialCommand::Exec => {
            out.push("exec".to_string());
            out.push(required_text(&args.address, "address")?);
        }
        OfficialCommand::HexDump => {
            out.push("hexdump".to_string());
            out.push(required_text(&args.address, "address")?);
            out.push(required_text(&args.length, "length")?);
        }
        OfficialCommand::Fill => {
            out.push("fill".to_string());
            out.push(required_text(&args.address, "address")?);
            out.push(required_text(&args.length, "length")?);
            out.push(required_text(&args.value, "value")?);
        }
        OfficialCommand::Clear => {
            out.push("clear".to_string());
            out.push(required_text(&args.address, "address")?);
            out.push(required_text(&args.length, "length")?);
        }
        OfficialCommand::DumpPartition => {
            out.push("dump".to_string());
            out.push(required_text(&args.media, "media")?);
            out.push(required_text(&args.partition, "partition")?);
            out.push(required_path(args.output.as_deref(), "output file")?);
        }
        OfficialCommand::ListMedia => {
            out.push("lsmedia".to_string());
            if let Some(image) = &args.image {
                out.push(path_arg(image));
            }
        }
        OfficialCommand::ListPartitions => {
            out.push("lspart".to_string());
            out.push(required_text(&args.media, "media")?);
        }
        OfficialCommand::FlashErase => {
            out.push("flasherase".to_string());
            out.push(required_text(&args.media, "media id")?);
            if let Some(image) = &args.image {
                out.push(path_arg(image));
            }
        }
        OfficialCommand::RamBoot => {
            out.push("ramboot".to_string());
            out.push(required_text(&args.fwc_name, "FWC name")?);
            out.push(required_text(&args.ram_address, "RAM address")?);
            out.push(required_path(args.image.as_deref(), "image file")?);
        }
        OfficialCommand::JtagUnlockData => {
            out.push("jtagunlockdata".to_string());
            out.push(required_path(args.output.as_deref(), "output file")?);
        }
        OfficialCommand::JtagUnlock => {
            out.push("jtagunlock".to_string());
            out.extend(split_raw_args(&args.raw_args));
        }
        OfficialCommand::Raw => out.extend(split_raw_args(&args.raw_args)),
        // eFuse family: argument shapes mirror the upgcmd V2.1.0 binary help
        // strings verbatim (`bdefuse ...` / `auzwritefuse <authorization file>`).
        // Start/length reuse the generic address/length fields; the GUI labels
        // them accordingly. eFuse writes are OTP-irreversible (0→1 only).
        OfficialCommand::BdefuseList => {
            out.push("bdefuse".to_string());
            out.push("list".to_string());
        }
        OfficialCommand::BdefuseSelect => {
            out.push("bdefuse".to_string());
            out.push("select".to_string());
            out.push(required_text(&args.efuse_id, "eFuse id")?);
        }
        OfficialCommand::BdefuseRead => {
            out.push("bdefuse".to_string());
            out.push("read".to_string());
            out.push(required_text(&args.address, "start")?);
            out.push(required_text(&args.length, "length")?);
            out.push(required_path(args.output.as_deref(), "output file")?);
        }
        OfficialCommand::BdefuseDump => {
            out.push("bdefuse".to_string());
            out.push("dump".to_string());
            out.push(required_text(&args.address, "start")?);
            out.push(required_text(&args.length, "length")?);
        }
        OfficialCommand::BdefuseWrite => {
            out.push("bdefuse".to_string());
            out.push("write".to_string());
            out.push(required_text(&args.address, "start")?);
            out.push(required_text(&args.length, "length")?);
            out.push(required_path(args.input.as_deref(), "input file")?);
        }
        OfficialCommand::BdefuseWriteHex => {
            out.push("bdefuse".to_string());
            out.push("writehex".to_string());
            out.push(required_text(&args.address, "start")?);
            out.push(required_text(&args.value, "hex data")?);
        }
        OfficialCommand::AuzWriteFuse => {
            out.push("auzwritefuse".to_string());
            out.push(required_path(args.input.as_deref(), "authorization file")?);
        }
        // Bulk-data session verb, zero arguments (verified valid by probe:
        // unknown commands print "not found", `bdreboot` proceeds to
        // device-open). The manual exits erase mode by rebooting.
        OfficialCommand::BdReboot => {
            out.push("bdreboot".to_string());
        }
    }

    Ok(out)
}

pub fn run_upgcmd(upgcmd: &Path, args: &[String]) -> Result<String, String> {
    if upgcmd.as_os_str().is_empty() {
        return Err("upgcmd compatibility path is not configured".to_string());
    }
    let output = Command::new(upgcmd)
        .args(args)
        .output()
        .map_err(|e| format!("Failed to run '{}': {}", upgcmd.display(), e))?;

    let mut text = String::new();
    if !output.stdout.is_empty() {
        text.push_str(&String::from_utf8_lossy(&output.stdout));
    }
    if !output.stderr.is_empty() {
        if !text.is_empty() && !text.ends_with('\n') {
            text.push('\n');
        }
        text.push_str(&String::from_utf8_lossy(&output.stderr));
    }
    if output.status.success() {
        Ok(text)
    } else {
        Err(format!(
            "upgcmd exited with {}:\n{}",
            output.status,
            text.trim_end()
        ))
    }
}

pub fn run_adb_enter_upgrade(aiburn_dir: &Path) -> Result<String, String> {
    let adb = adb_path(aiburn_dir)?;
    run_program_capture(&adb, &["shell", "aicupg"])
}

pub fn open_manual(aiburn_dir: &Path) -> Result<(), String> {
    if aiburn_dir.as_os_str().is_empty() {
        return Err(
            "Manual is only available when AiBurn compatibility dir is configured".to_string(),
        );
    }
    let manual = aiburn_dir.join("aiburn_manual.pdf");
    if !manual.exists() {
        return Err(format!("Manual not found: {}", manual.display()));
    }
    #[cfg(windows)]
    {
        Command::new("cmd")
            .args(["/C", "start", "", &manual.to_string_lossy()])
            .spawn()
            .map_err(|e| format!("Failed to open '{}': {}", manual.display(), e))?;
        Ok(())
    }
    #[cfg(target_os = "macos")]
    {
        Command::new("open")
            .arg(&manual)
            .spawn()
            .map_err(|e| format!("Failed to open '{}': {}", manual.display(), e))?;
        Ok(())
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        Command::new("xdg-open")
            .arg(&manual)
            .spawn()
            .map_err(|e| format!("Failed to open '{}': {}", manual.display(), e))?;
        Ok(())
    }
    #[cfg(all(not(windows), not(unix)))]
    {
        Err(format!(
            "Opening manuals is not implemented: {}",
            manual.display()
        ))
    }
}

fn adb_path(aiburn_dir: &Path) -> Result<PathBuf, String> {
    let bundled = if cfg!(windows) {
        aiburn_dir.join("adb").join("adb.exe")
    } else {
        aiburn_dir.join("adb").join("adb")
    };
    if bundled.exists() {
        return Ok(bundled);
    }
    find_in_path(if cfg!(windows) { "adb.exe" } else { "adb" })
        .ok_or_else(|| "adb not found in compatibility directory or PATH".to_string())
}

fn find_in_path(name: &str) -> Option<PathBuf> {
    let paths = std::env::var_os("PATH")?;
    std::env::split_paths(&paths)
        .map(|dir| dir.join(name))
        .find(|candidate| candidate.exists())
}

fn required_path(path: Option<&Path>, label: &str) -> Result<String, String> {
    path.map(path_arg)
        .ok_or_else(|| format!("Missing {}", label))
}

fn run_program_capture(path: &Path, args: &[&str]) -> Result<String, String> {
    let output = Command::new(path)
        .args(args)
        .output()
        .map_err(|e| format!("Failed to run '{}': {}", path.display(), e))?;
    let mut text = String::new();
    if !output.stdout.is_empty() {
        text.push_str(&String::from_utf8_lossy(&output.stdout));
    }
    if !output.stderr.is_empty() {
        if !text.is_empty() && !text.ends_with('\n') {
            text.push('\n');
        }
        text.push_str(&String::from_utf8_lossy(&output.stderr));
    }
    if output.status.success() {
        Ok(text)
    } else {
        Err(format!(
            "{} exited with {}:\n{}",
            path.display(),
            output.status,
            text.trim_end()
        ))
    }
}

fn required_text(value: &str, label: &str) -> Result<String, String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        Err(format!("Missing {}", label))
    } else {
        Ok(trimmed.to_string())
    }
}

fn path_arg(path: &Path) -> String {
    path.to_string_lossy().to_string()
}

fn split_raw_args(args: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut in_quotes = false;
    let mut escape = false;
    for ch in args.chars() {
        if escape {
            cur.push(ch);
            escape = false;
            continue;
        }
        match ch {
            '\\' if in_quotes => escape = true,
            '"' => in_quotes = !in_quotes,
            ch if ch.is_whitespace() && !in_quotes => {
                if !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
            }
            _ => cur.push(ch),
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// Build the `upgcmd flasherase` argument list for the pre-burn erase stage.
///
/// Native full-chip erase is not implemented (the UPG erase command is not
/// reverse-engineered; see docs/与AiBurn功能对照.md): the burn frontends run
/// this official backend *before* opening the device natively. Transport
/// selectors use the same `upgcmd -d/-u/-b`透传 as [`build_args`]; empty
/// strings mean "default". `media` is required by `flasherase`; callers fall
/// back to the image header's `media_dev_id` when the user did not pick one.
pub fn pre_burn_erase_args(
    media: &str,
    image: Option<&Path>,
    device: &str,
    uart_port: &str,
    baudrate: &str,
) -> Result<Vec<String>, String> {
    let media = media.trim();
    if media.is_empty() {
        return Err("Missing media id for full-chip erase".to_string());
    }
    let args = OfficialArgs {
        command: OfficialCommand::FlashErase,
        media: media.to_string(),
        image: image.map(|p| p.to_path_buf()),
        device: device.to_string(),
        uart_port: uart_port.to_string(),
        baudrate: baudrate.to_string(),
        ..Default::default()
    };
    build_args(&args)
}

/// Run the pre-burn full-chip erase via the official `upgcmd` backend.
///
/// Must be called *before* the native burn opens the device, so the
/// subprocess owns the USB/UART connection without lock contention.
pub fn run_pre_burn_erase(
    upgcmd: &Path,
    media: &str,
    image: Option<&Path>,
    device: &str,
    uart_port: &str,
    baudrate: &str,
) -> Result<String, String> {
    let args = pre_burn_erase_args(media, image, device, uart_port, baudrate)?;
    run_upgcmd(upgcmd, &args)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transport_selectors_precede_command() {
        let mut args = OfficialArgs::default();
        args.command = OfficialCommand::ListDevices;
        args.device = "1:2".to_string();
        args.uart_port = "COM3".to_string();
        args.baudrate = "921600".to_string();
        let out = build_args(&args).unwrap();
        assert_eq!(
            out,
            vec![
                "--progress",
                "--dev", "1:2",
                "--uart", "COM3",
                "--baudrate", "921600",
                "--list",
            ]
            .into_iter()
            .map(str::to_string)
            .collect::<Vec<_>>()
        );
    }

    #[test]
    fn empty_transport_selectors_are_omitted() {
        let args = OfficialArgs::default();
        let out = build_args(&args).unwrap();
        assert!(!out.iter().any(|a| a == "--dev" || a == "--uart" || a == "--baudrate"));
    }

    #[test]
    fn pre_burn_erase_passes_media_image_and_selectors() {
        let out = pre_burn_erase_args(
            "0",
            Some(Path::new("fw.img")),
            "1:2",
            "COM3",
            "921600",
        )
        .unwrap();
        assert_eq!(
            out,
            vec![
                "--progress",
                "--dev", "1:2",
                "--uart", "COM3",
                "--baudrate", "921600",
                "flasherase", "0", "fw.img",
            ]
            .into_iter()
            .map(str::to_string)
            .collect::<Vec<_>>()
        );
    }

    #[test]
    fn pre_burn_erase_rejects_empty_media() {
        assert!(pre_burn_erase_args("", None, "", "", "").is_err());
    }

    fn bdefuse_args(command: OfficialCommand) -> OfficialArgs {
        let mut args = OfficialArgs::default();
        args.command = command;
        // Keep transport selectors empty so only the command words show.
        args.address = "0xC0".to_string();
        args.length = "64".to_string();
        args.value = "00FF".to_string();
        args.efuse_id = "0".to_string();
        args.input = Some(PathBuf::from("in.bin"));
        args.output = Some(PathBuf::from("out.bin"));
        args
    }

    #[test]
    fn bdefuse_commands_mirror_binary_help_shapes() {
        // Shapes mirror the upgcmd V2.1.0 binary help strings verbatim.
        let cases = [
            (
                OfficialCommand::BdefuseList,
                vec!["--progress", "bdefuse", "list"],
            ),
            (
                OfficialCommand::BdefuseSelect,
                vec!["--progress", "bdefuse", "select", "0"],
            ),
            (
                OfficialCommand::BdefuseRead,
                vec!["--progress", "bdefuse", "read", "0xC0", "64", "out.bin"],
            ),
            (
                OfficialCommand::BdefuseDump,
                vec!["--progress", "bdefuse", "dump", "0xC0", "64"],
            ),
            (
                OfficialCommand::BdefuseWrite,
                vec!["--progress", "bdefuse", "write", "0xC0", "64", "in.bin"],
            ),
            (
                OfficialCommand::BdefuseWriteHex,
                vec!["--progress", "bdefuse", "writehex", "0xC0", "00FF"],
            ),
            (
                OfficialCommand::AuzWriteFuse,
                vec!["--progress", "auzwritefuse", "in.bin"],
            ),
        ];
        for (command, expected) in cases {
            let out = build_args(&bdefuse_args(command)).unwrap();
            assert_eq!(
                out,
                expected.into_iter().map(str::to_string).collect::<Vec<_>>(),
                "{:?}",
                command
            );
        }
    }

    #[test]
    fn bdefuse_requires_its_arguments() {
        let mut empty = OfficialArgs::default();
        // Address/length/value carry non-empty defaults; clear them so the
        // "missing argument" paths are actually exercised.
        empty.address = String::new();
        empty.length = String::new();
        empty.value = String::new();
        empty.command = OfficialCommand::BdefuseSelect;
        assert!(build_args(&empty).is_err());
        empty.command = OfficialCommand::BdefuseRead;
        assert!(build_args(&empty).is_err());
        empty.command = OfficialCommand::BdefuseWriteHex;
        assert!(build_args(&empty).is_err());
        empty.command = OfficialCommand::AuzWriteFuse;
        assert!(build_args(&empty).is_err());
    }

    #[test]
    fn bdreboot_takes_no_arguments() {
        let mut args = OfficialArgs::default();
        args.command = OfficialCommand::BdReboot;
        let out = build_args(&args).unwrap();
        assert_eq!(
            out,
            vec!["--progress", "bdreboot"]
                .into_iter()
                .map(str::to_string)
                .collect::<Vec<_>>()
        );
    }
}