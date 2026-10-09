#![cfg_attr(windows, windows_subsystem = "windows")]

use std::path::{Path, PathBuf};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    mpsc::{self, Receiver},
    Arc,
};
use std::thread;
use std::time::{Duration, Instant};

use artinchip_flash::app_config::{
    append_image_history, compat_tool_path, load_image_history, AppConfig,
};
use artinchip_flash::build_info;
use artinchip_flash::burn_stats::{self, BurnOutcome};
use artinchip_flash::i18n::{command_label, tr, Language, Msg};
use artinchip_flash::image::parser::{self, ImageSummary, MetaSummary};
use artinchip_flash::official::{self, OfficialArgs, OfficialCommand};
use artinchip_flash::sdcard;
use artinchip_flash::services::{self, UartSpec};
use artinchip_flash::standalone;
use artinchip_flash::update::{self, UpdateChannel};
use artinchip_flash::uart::{SerialPortInfo, UartDevice, UartMonitor, UartOptions};
use artinchip_flash::usb::device::{AicDevice, BurnEvent, BurnOptions, DeviceInfo};
use artinchip_flash::verbosity;
use eframe::egui;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Tab {
    Burn,
    Tools,
    Settings,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TransportKind {
    Usb,
    Uart,
}

impl TransportKind {
    fn from_config(value: &str) -> Self {
        if value.eq_ignore_ascii_case("uart") {
            TransportKind::Uart
        } else {
            TransportKind::Usb
        }
    }

    fn config_value(self) -> &'static str {
        match self {
            TransportKind::Usb => "usb",
            TransportKind::Uart => "uart",
        }
    }
}

enum WorkerEvent {
    Burn(BurnEvent),
    ToolOutput(String),
    Error(String),
    Done,
}

#[derive(Clone, Copy, Debug)]
enum NotReadyContext {
    Scan,
    BurnCancelled,
    DeviceInfoCancelled,
}

type DeviceScanResult = (Result<Vec<DeviceInfo>, String>, bool);

struct GuiApp {
    config: AppConfig,
    tab: Tab,
    transport: TransportKind,
    devices: Vec<DeviceInfo>,
    selected_device: Option<usize>,
    serial_ports: Vec<SerialPortInfo>,
    selected_serial_port: Option<usize>,
    image_summary: Option<ImageSummary>,
    selected_parts: Vec<String>,
    image_history: Vec<(PathBuf, String)>,
    log_lines: Vec<String>,
    burn_progress: f32,
    component_progress: f32,
    active_component: String,
    busy: bool,
    cancel_flag: Option<Arc<AtomicBool>>,
    burn_started_at: Option<Instant>,
    overall_sent: usize,
    overall_total: usize,
    burn_rate_bps: f64,
    burn_failed: bool,
    burn_error: Option<String>,
    /// Elapsed time frozen at burn completion/cancel, so the status line
    /// stops ticking. `None` before the first burn of this session.
    burn_finished_elapsed: Option<Duration>,
    worker_is_burn: bool,
    auto_started_for_device: bool,
    rx: Option<Receiver<WorkerEvent>>,
    scan_rx: Option<Receiver<DeviceScanResult>>,
    serial_scan_rx: Option<Receiver<Result<Vec<SerialPortInfo>, String>>>,
    device_scan_in_progress: bool,
    monitor: Option<UartMonitor>,
    monitor_input: String,
    official_args: OfficialArgs,
    settings_path: PathBuf,
    log_started_at: Instant,
    update_rx: Option<Receiver<Result<update::UpdateStatus, String>>>,
    update_status: String,
    update_check_in_progress: bool,
}

impl GuiApp {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        install_cjk_font(&cc.egui_ctx);
        let config = AppConfig::load_default();
        verbosity::set_verbose(config.verbose);
        let settings_path = config.app_dir.join("config.ini");
        let image_history = load_image_history(&config.app_dir);
        let transport = TransportKind::from_config(&config.transport);
        let mut app = Self {
            selected_parts: config.selected_parts.clone(),
            official_args: OfficialArgs {
                image: config.image_path.clone(),
                ..Default::default()
            },
            config,
            tab: Tab::Burn,
            transport,
            devices: Vec::new(),
            selected_device: None,
            serial_ports: Vec::new(),
            selected_serial_port: None,
            image_summary: None,
            image_history,
            log_lines: Vec::new(),
            burn_progress: 0.0,
            component_progress: 0.0,
            active_component: String::new(),
            busy: false,
            cancel_flag: None,
            burn_started_at: None,
            overall_sent: 0,
            overall_total: 0,
            burn_rate_bps: 0.0,
            burn_failed: false,
            burn_error: None,
            burn_finished_elapsed: None,
            worker_is_burn: false,
            auto_started_for_device: false,
            rx: None,
            scan_rx: None,
            serial_scan_rx: None,
            device_scan_in_progress: false,
            monitor: None,
            monitor_input: String::new(),
            settings_path,
            log_started_at: Instant::now(),
            update_rx: None,
            update_status: String::new(),
            update_check_in_progress: false,
        };
        if let Some(path) = app.config.image_path.clone() {
            app.load_image_summary(path, false);
        }
        app.start_device_scan(cc.egui_ctx.clone(), false);
        // Auto update check on startup (24h throttle, non-blocking).
        if app.config.auto_check_update && !update_recently_checked(app.config.last_update_check_unix) {
            app.start_update_check(cc.egui_ctx.clone());
        }
        app
    }

    fn lang(&self) -> Language {
        Language::from_code(&self.config.language)
    }

    fn t(&self, msg: Msg) -> &'static str {
        tr(self.lang(), msg)
    }

    fn start_device_scan(&mut self, ctx: egui::Context, allow_auto_burn: bool) {
        if self.device_scan_in_progress {
            return;
        }
        if self.transport == TransportKind::Uart {
            self.start_serial_scan(ctx);
            return;
        }
        let (tx, rx) = mpsc::channel();
        self.scan_rx = Some(rx);
        self.device_scan_in_progress = true;
        thread::spawn(move || {
            let result = AicDevice::scan_devices();
            let _ = tx.send((result, allow_auto_burn));
            ctx.request_repaint();
        });
    }

    fn start_serial_scan(&mut self, ctx: egui::Context) {
        if self.device_scan_in_progress {
            return;
        }
        let (tx, rx) = mpsc::channel();
        self.serial_scan_rx = Some(rx);
        self.device_scan_in_progress = true;
        thread::spawn(move || {
            let result = UartDevice::list_ports();
            let _ = tx.send(result);
            ctx.request_repaint();
        });
    }

    fn start_update_check(&mut self, ctx: egui::Context) {
        if self.update_check_in_progress {
            return;
        }
        let channel = UpdateChannel::from_str(&self.config.update_channel);
        let (tx, rx) = mpsc::channel();
        self.update_rx = Some(rx);
        self.update_check_in_progress = true;
        self.update_status = self.t(Msg::CheckingUpdate).to_string();
        thread::spawn(move || {
            let result = update::check(channel);
            let _ = tx.send(result);
            ctx.request_repaint();
        });
    }

    fn apply_update_result(&mut self, result: Result<update::UpdateStatus, String>) {
        self.update_check_in_progress = false;
        match result {
            Ok(status) => {
                self.config.last_update_check_unix = current_unix_secs();
                let _ = self.config.save_to(&self.settings_path);
                if status.update_available {
                    self.update_status = format!(
                        "{}: {} -> {}",
                        self.t(Msg::UpdateAvailable),
                        status.current_version,
                        status.latest_tag
                    );
                    self.log(format!(
                        "{}: {} ({}). {}: {}",
                        self.t(Msg::UpdateAvailable),
                        status.current_version,
                        status.latest_tag,
                        self.t(Msg::OpenReleasePage),
                        status.html_url
                    ));
                    if !status.notes_preview.is_empty() {
                        for line in status.notes_preview.lines().take(5) {
                            self.log(format!("  {}", line));
                        }
                    }
                } else {
                    self.update_status = format!(
                        "{} ({})",
                        self.t(Msg::UpToDate),
                        status.current_version
                    );
                    self.log(format!(
                        "{}: {}",
                        self.t(Msg::UpToDate),
                        status.current_version
                    ));
                }
            }
            Err(e) => {
                self.update_status = e.clone();
                self.log(format!("{}: {}", self.t(Msg::CheckUpdate), e));
            }
        }
    }

    fn apply_serial_scan(&mut self, result: Result<Vec<SerialPortInfo>, String>) {
        match result {
            Ok(ports) => {
                self.serial_ports = ports;
                if self.serial_ports.is_empty() {
                    self.selected_serial_port = None;
                    self.log(format!(
                        "{}: {}",
                        self.t(Msg::Scan),
                        self.t(Msg::NoSerialPorts)
                    ));
                } else {
                    let configured = self.config.serial_port.clone();
                    self.selected_serial_port = self
                        .serial_ports
                        .iter()
                        .position(|port| !configured.is_empty() && port.port_name == configured)
                        .or(Some(0));
                    if configured.is_empty() {
                        if let Some(port) = self.serial_ports.first() {
                            self.config.serial_port = port.port_name.clone();
                        }
                    }
                    let summary = match self.lang() {
                        Language::ZhCn => {
                            format!("扫描完成：发现 {} 个串口", self.serial_ports.len())
                        }
                        Language::En => {
                            format!("Scan complete: {} serial port(s)", self.serial_ports.len())
                        }
                    };
                    self.log(summary);
                }
            }
            Err(e) => self.log(format!("{}: {}", self.t(Msg::SerialScanFailed), e)),
        }
    }

    fn apply_device_scan(
        &mut self,
        result: Result<Vec<DeviceInfo>, String>,
        allow_auto_burn: bool,
    ) {
        match result {
            Ok(devices) => {
                self.devices = devices;
                if self.devices.is_empty() {
                    self.selected_device = None;
                    self.auto_started_for_device = false;
                    self.log(format!(
                        "{}: {}",
                        self.t(Msg::Scan),
                        self.t(Msg::NoDeviceAvailable)
                    ));
                } else {
                    let ready_count = self.devices.iter().filter(|device| device.ready).count();
                    let not_ready_count = self.devices.len().saturating_sub(ready_count);
                    self.selected_device = self
                        .devices
                        .iter()
                        .position(|device| device.ready)
                        .or(Some(0));
                    self.log(scan_summary(
                        self.lang(),
                        self.devices.len(),
                        ready_count,
                        not_ready_count,
                    ));
                    let not_ready_devices = self
                        .devices
                        .iter()
                        .filter(|device| !device.ready)
                        .cloned()
                        .collect::<Vec<_>>();
                    for device in &not_ready_devices {
                        self.log_not_ready_device(device, NotReadyContext::Scan);
                    }
                    if allow_auto_burn
                        && self.config.auto_burn
                        && !self.auto_started_for_device
                        && self.image_summary.is_some()
                        && ready_count > 0
                    {
                        self.auto_started_for_device = true;
                        self.start_burn();
                    } else if allow_auto_burn
                        && self.config.auto_burn
                        && self.image_summary.is_some()
                        && ready_count == 0
                    {
                        self.log(auto_burn_skipped(self.lang()));
                    }
                }
            }
            Err(e) => self.log(format!("{}: {}", self.t(Msg::ScanFailed), e)),
        }
    }

    fn load_image(&mut self, path: PathBuf) {
        self.load_image_summary(path, true);
    }

    fn load_image_summary(&mut self, path: PathBuf, update_history: bool) {
        match services::load_image_summary(&path) {
            Ok(summary) => {
                self.config.image_path = Some(path.clone());
                self.official_args.image = Some(path.clone());
                self.image_summary = Some(summary);
                self.sync_selected_parts_from_image();
                self.log(format!(
                    "{} {}",
                    self.t(Msg::ParseImageHeaderFrom),
                    path.display()
                ));
                if update_history {
                    let _ = append_image_history(&self.config.app_dir, &path);
                    self.image_history = load_image_history(&self.config.app_dir);
                }
            }
            Err(e) => self.log(format!("{}: {}", self.t(Msg::ImageParseFailed), e)),
        }
    }

    fn sync_selected_parts_from_image(&mut self) {
        let Some(summary) = &self.image_summary else {
            return;
        };
        if self.selected_parts.is_empty() {
            self.selected_parts = self.config.selected_parts.clone();
        }
        for meta in target_metas(summary) {
            let key = part_key(meta);
            if self
                .config
                .selected_parts
                .iter()
                .any(|p| p == &key || p == &meta.partition)
            {
                continue;
            }
        }
        if self.selected_parts.is_empty() {
            self.selected_parts = target_metas(summary)
                .map(part_key)
                .filter(|key| ["spl", "env", "os"].contains(&key.as_str()))
                .collect();
        }
    }

    fn start_monitor(&mut self) {
        if self.monitor.is_some() {
            return;
        }
        let selected = self.selected_serial_port_name();
        let resolved = if selected.is_empty() || selected.eq_ignore_ascii_case("auto") {
            self.serial_ports
                .iter()
                .find(|port| port.port_type == "usb")
                .or_else(|| self.serial_ports.first())
                .map(|port| port.port_name.clone())
        } else {
            Some(selected)
        };
        let Some(path) = resolved else {
            self.log(self.t(Msg::NoSerialPorts));
            return;
        };
        match UartMonitor::start(&path, self.config.serial_baud.max(1200)) {
            Ok(monitor) => {
                self.monitor = Some(monitor);
                self.log(match self.lang() {
                    Language::ZhCn => format!("已连接串口监视：{}", path),
                    Language::En => format!("UART monitor connected: {}", path),
                });
            }
            Err(e) => self.log_error(e),
        }
    }

    fn stop_monitor(&mut self) {
        if let Some(mut monitor) = self.monitor.take() {
            monitor.close();
            self.log(match self.lang() {
                Language::ZhCn => "串口监视已断开".to_string(),
                Language::En => "UART monitor disconnected".to_string(),
            });
        }
    }

    fn monitor_trigger_upgrade(&mut self) {
        if let Some(monitor) = &self.monitor {
            monitor.trigger_upgrade();
            self.log(match self.lang() {
                Language::ZhCn => {
                    "已发送 `aicupg gotobl` / `aicupg uart 0`；若设备未自动重启，请手动复位"
                        .to_string()
                }
                Language::En => {
                    "Sent `aicupg gotobl` / `aicupg uart 0`; reset the board if it does not reboot"
                        .to_string()
                }
            });
        }
    }

    fn monitor_send(&mut self, text: &str) {
        if let Some(monitor) = &self.monitor {
            monitor.send_line(text);
            self.log(format!("> {}", text));
        }
    }

    fn start_burn(&mut self) {
        if self.busy {
            return;
        }
        if self.transport == TransportKind::Uart {
            self.stop_monitor();
        }
        let Some(path) = self.config.image_path.clone() else {
            self.log(self.t(Msg::SelectImageFirst));
            return;
        };
        let selected_device = self
            .selected_device
            .and_then(|idx| self.devices.get(idx).cloned());
        let use_uart = self.transport == TransportKind::Uart;
        if !use_uart {
            if let Some(device) = &selected_device {
                if !device.ready {
                    self.log_not_ready_device(device, NotReadyContext::BurnCancelled);
                    return;
                }
            } else if !self.devices.is_empty() && self.devices.iter().all(|device| !device.ready) {
                self.log(burn_cancelled_no_ready(self.lang()));
                return;
            }
        }
        let selected_parts = self.selected_parts.clone();
        let force_upgrade = self.config.force_upgrade;
        let erase_all = self.config.erase_all;
        let erase_media_cfg = self.config.erase_media.clone();
        let upgcmd_path = self.config.upgcmd_path.clone();
        let erase_baud = if self.config.serial_speed > 0 {
            self.config.serial_speed.to_string()
        } else {
            String::new()
        };
        // Transport selectors for the pre-burn erase (`upgcmd -d/-u/-b`透传):
        // USB passes the selected device as `bus:port`, UART passes the port.
        let erase_device = selected_device
            .as_ref()
            .map(|d| {
                let port = if d.port_path.is_empty() {
                    d.address.to_string()
                } else {
                    d.port_path.clone()
                };
                format!("{}:{}", d.bus_number, port)
            })
            .unwrap_or_default();
        let erase_uart = if use_uart {
            let port = self.selected_serial_port_name();
            if port.eq_ignore_ascii_case("auto") || port.trim().is_empty() {
                String::new()
            } else {
                port
            }
        } else {
            String::new()
        };
        let reset_after_burn = !force_upgrade;
        let timeout = Duration::from_secs(self.config.burn_timeout_secs.max(1));
        let adb_scan = self.config.adb_scan && !use_uart;
        let aiburn_dir = self.config.aiburn_dir.clone();
        let lang = self.lang();
        let uart_port = self.selected_serial_port_name();
        let uart_options = UartOptions {
            baudrate: self.config.serial_baud.max(1200),
            max_baudrate: (self.config.serial_speed > self.config.serial_baud)
                .then_some(self.config.serial_speed),
            auto_enter: self.config.serial_auto_enter,
            ..Default::default()
        };
        let (tx, rx) = mpsc::channel();
        self.rx = Some(rx);
        self.busy = true;
        self.burn_progress = 0.0;
        self.component_progress = 0.0;
        self.active_component.clear();
        self.overall_sent = 0;
        self.overall_total = 0;
        self.burn_rate_bps = 0.0;
        self.burn_failed = false;
        self.burn_error = None;
        self.burn_finished_elapsed = None;
        self.worker_is_burn = true;
        let cancel = Arc::new(AtomicBool::new(false));
        self.cancel_flag = Some(cancel.clone());
        self.burn_started_at = Some(Instant::now());
        self.log(format!(
            "{} {} ...",
            self.t(Msg::BurnImageFile),
            path.display()
        ));
        if use_uart && self.config.serial_auto_enter {
            self.log(match lang {
                Language::ZhCn => {
                    "连接 UART 时若设备未处于升级模式，将自动发送 `aicupg gotobl` / `aicupg uart 0`；必要时请复位设备".to_string()
                }
                Language::En => {
                    "If the device is not in upgrade mode, the tool will send `aicupg gotobl` / `aicupg uart 0`; reset the board if needed".to_string()
                }
            });
        }

        thread::spawn(move || {
            let result = (|| -> Result<(), String> {
                if adb_scan {
                    let _ = tx.send(WorkerEvent::ToolOutput(
                        tr(lang, Msg::StartAdbScan).to_string(),
                    ));
                    match official::run_adb_enter_upgrade(&aiburn_dir) {
                        Ok(text) if !text.trim().is_empty() => {
                            let _ = tx.send(WorkerEvent::ToolOutput(text));
                        }
                        Ok(_) => {}
                        Err(e) => {
                            let _ = tx.send(WorkerEvent::ToolOutput(format!(
                                "{}: {}",
                                tr(lang, Msg::AdbScanFailed),
                                e
                            )));
                        }
                    }
                    thread::sleep(Duration::from_millis(700));
                }
                let (data, header, metas, _summary) = parser::read_image(&path)?;
                if force_upgrade {
                    let _ = tx.send(WorkerEvent::ToolOutput(
                        tr(lang, Msg::ForceUpgradeNote).to_string(),
                    ));
                }
                // Pre-burn full-chip erase via the official backend (same
                // `upgcmd flasherase`透传 as the Tools page). Native erase is
                // not implemented, so this runs *before* opening the device.
                if erase_all {
                    let media = if erase_media_cfg.trim().is_empty() {
                        header.media_dev_id().to_string()
                    } else {
                        erase_media_cfg.trim().to_string()
                    };
                    let _ = tx.send(WorkerEvent::ToolOutput(format!(
                        "flasherase media {} ...",
                        media
                    )));
                    match official::run_pre_burn_erase(
                        &upgcmd_path,
                        &media,
                        Some(&path),
                        &erase_device,
                        &erase_uart,
                        &erase_baud,
                    ) {
                        Ok(text) => {
                            let _ = tx.send(WorkerEvent::ToolOutput(text));
                        }
                        Err(e) => {
                            return Err(format!("Pre-burn erase failed, burn aborted: {}", e));
                        }
                    }
                }
                let options = BurnOptions {
                    selected_parts,
                    reset_after_burn,
                    burn_timeout: timeout,
                    cancel: Some(cancel),
                    force_upgrade,
                };
                let mut callback = |event| {
                    let _ = tx.send(WorkerEvent::Burn(event));
                };
                if use_uart {
                    let mut dev = open_uart_backend(&uart_port, uart_options)?;
                    dev.burn_image_with_options(&data, &metas, &options, Some(&mut callback))?;
                } else {
                    let mut dev = if let Some(device) = selected_device {
                        AicDevice::open_by_location(device.bus_number, device.address)?
                    } else {
                        AicDevice::open_first()?
                    };
                    dev.burn_image_with_options(&data, &metas, &options, Some(&mut callback))?;
                }
                Ok(())
            })();
            if let Err(e) = result {
                let _ = tx.send(WorkerEvent::Error(e));
            }
            let _ = tx.send(WorkerEvent::Done);
        });
    }

    fn stop_burn(&mut self) {
        if let Some(flag) = &self.cancel_flag {
            flag.store(true, Ordering::SeqCst);
            self.log(match self.lang() {
                Language::ZhCn => "已请求停止烧录，等待当前数据块完成后中止…",
                Language::En => "Stop requested; aborting after the current chunk…",
            });
        }
    }

    fn start_read_device_info(&mut self) {
        if self.busy {
            return;
        }
        if self.transport == TransportKind::Uart {
            self.stop_monitor();
        }
        let selected_device = self
            .selected_device
            .and_then(|idx| self.devices.get(idx).cloned());
        let use_uart = self.transport == TransportKind::Uart;
        if !use_uart {
            if let Some(device) = &selected_device {
                if !device.ready {
                    self.log_not_ready_device(device, NotReadyContext::DeviceInfoCancelled);
                    return;
                }
            } else if !self.devices.is_empty() && self.devices.iter().all(|device| !device.ready) {
                self.log(device_info_cancelled_no_ready(self.lang()));
                return;
            }
        }
        let read_log = self.config.read_device_log;
        let lang = self.lang();
        let uart_port = self.selected_serial_port_name();
        let uart_options = UartOptions {
            baudrate: self.config.serial_baud.max(1200),
            max_baudrate: (self.config.serial_speed > self.config.serial_baud)
                .then_some(self.config.serial_speed),
            auto_enter: self.config.serial_auto_enter,
            ..Default::default()
        };
        let (tx, rx) = mpsc::channel();
        self.rx = Some(rx);
        self.busy = true;
        self.worker_is_burn = false;
        thread::spawn(move || {
            let result = (|| -> Result<String, String> {
                if use_uart {
                    let mut dev = open_uart_backend(&uart_port, uart_options)?;
                    let mut text = dev.device_info_text()?;
                    if read_log {
                        text.push_str("\n\n");
                        text.push_str(tr(lang, Msg::DeviceLogDivider));
                        text.push('\n');
                        text.push_str(&dev.get_device_log()?);
                    }
                    return Ok(text);
                }
                let mut dev = if let Some(device) = selected_device {
                    AicDevice::open_by_location(device.bus_number, device.address)?
                } else {
                    AicDevice::open_first()?
                };
                let mut text = dev.device_info_text()?;
                if read_log {
                    text.push_str("\n\n");
                    text.push_str(tr(lang, Msg::DeviceLogDivider));
                    text.push('\n');
                    text.push_str(&dev.get_device_log()?);
                }
                Ok(text)
            })();
            match result {
                Ok(text) => {
                    let _ = tx.send(WorkerEvent::ToolOutput(text));
                }
                Err(e) => {
                    let _ = tx.send(WorkerEvent::Error(e));
                }
            }
            let _ = tx.send(WorkerEvent::Done);
        });
    }

    fn start_official_command(&mut self) {
        if self.busy {
            return;
        }
        let upgcmd = self.config.upgcmd_path.clone();
        let args = match official::build_args(&self.official_args) {
            Ok(args) => args,
            Err(e) => {
                self.log(e);
                return;
            }
        };
        let (tx, rx) = mpsc::channel();
        self.rx = Some(rx);
        self.busy = true;
        self.worker_is_burn = false;
        self.log(format!(
            "{}: {} {}",
            self.t(Msg::RunCommand),
            upgcmd.display(),
            args.join(" ")
        ));
        thread::spawn(move || {
            match official::run_upgcmd(&upgcmd, &args) {
                Ok(text) => {
                    let _ = tx.send(WorkerEvent::ToolOutput(text));
                }
                Err(e) => {
                    let _ = tx.send(WorkerEvent::Error(e));
                }
            }
            let _ = tx.send(WorkerEvent::Done);
        });
    }

    fn start_sd_list(&mut self) {
        if self.busy {
            return;
        }
        let (tx, rx) = mpsc::channel();
        self.rx = Some(rx);
        self.busy = true;
        self.worker_is_burn = false;
        self.log(self.t(Msg::SdListNote));
        thread::spawn(move || {
            match sdcard::list_disks() {
                Ok(disks) => {
                    let mut text = String::from(
                        "Physical disks (read-only; writing boot cards is not implemented):\n",
                    );
                    if disks.is_empty() {
                        text.push_str("No physical disks found.\n");
                    }
                    for disk in &disks {
                        text.push_str(&disk.summary());
                        text.push('\n');
                    }
                    text.push_str(
                        "Note: use official AiBurn to write a boot card; double-check the target id first.\n",
                    );
                    let _ = tx.send(WorkerEvent::ToolOutput(text));
                }
                Err(e) => {
                    let _ = tx.send(WorkerEvent::Error(e));
                }
            }
            let _ = tx.send(WorkerEvent::Done);
        });
    }

    fn record_burn_stats(&mut self) {
        let outcome = if !self.burn_failed {
            BurnOutcome::Success
        } else if let Some(err) = &self.burn_error {
            burn_stats::classify_error(err)
        } else {
            BurnOutcome::Failure
        };
        if let Err(e) = burn_stats::record(&self.config.app_dir, outcome) {
            self.log(format!("Warning: could not record burn stats: {}", e));
        }
    }

    fn poll_worker(&mut self, ctx: &egui::Context) {
        self.poll_device_scan(ctx);
        self.poll_update_check(ctx);

        let monitor_lines: Vec<String> = self
            .monitor
            .as_ref()
            .map(|monitor| monitor.poll())
            .unwrap_or_default();
        if !monitor_lines.is_empty() {
            for line in monitor_lines {
                self.log(line);
            }
            ctx.request_repaint();
        }

        let mut done = false;
        if let Some(rx) = self.rx.take() {
            while let Ok(event) = rx.try_recv() {
                match event {
                    WorkerEvent::Burn(event) => self.apply_burn_event(event),
                    WorkerEvent::ToolOutput(text) => {
                        for line in text.lines() {
                            self.log(line);
                        }
                    }
                    WorkerEvent::Error(e) => {
                        if self.worker_is_burn {
                            self.burn_failed = true;
                            self.burn_error = Some(e.clone());
                        }
                        self.log_error(e);
                    }
                    WorkerEvent::Done => {
                        let was_burn = self.worker_is_burn;
                        self.busy = false;
                        self.cancel_flag = None;
                        self.worker_is_burn = false;
                        done = true;
                        if was_burn {
                            // Freeze the elapsed counter at completion/cancel
                            // so the status line stops ticking on later
                            // repaints; then record the outcome.
                            self.burn_finished_elapsed =
                                self.burn_started_at.map(|s| s.elapsed());
                            self.burn_started_at = None;
                            self.record_burn_stats();
                        }
                    }
                }
                ctx.request_repaint();
            }
            if !done {
                self.rx = Some(rx);
            }
        }
    }

    fn poll_device_scan(&mut self, ctx: &egui::Context) {
        let mut done = false;
        if let Some(rx) = self.scan_rx.take() {
            while let Ok((result, allow_auto_burn)) = rx.try_recv() {
                self.apply_device_scan(result, allow_auto_burn);
                self.device_scan_in_progress = false;
                done = true;
                ctx.request_repaint();
            }
            if !done {
                self.scan_rx = Some(rx);
            }
        }
        let mut serial_done = false;
        if let Some(rx) = self.serial_scan_rx.take() {
            while let Ok(result) = rx.try_recv() {
                self.apply_serial_scan(result);
                self.device_scan_in_progress = false;
                serial_done = true;
                ctx.request_repaint();
            }
            if !serial_done {
                self.serial_scan_rx = Some(rx);
            }
        }
    }

    fn poll_update_check(&mut self, ctx: &egui::Context) {
        let mut done = false;
        if let Some(rx) = self.update_rx.take() {
            while let Ok(result) = rx.try_recv() {
                self.apply_update_result(result);
                done = true;
                ctx.request_repaint();
            }
            if !done {
                self.update_rx = Some(rx);
            }
        }
    }

    fn apply_burn_event(&mut self, event: BurnEvent) {
        match event {
            BurnEvent::Log(line) => self.log(line),
            BurnEvent::Stage(stage) => self.log(stage),
            BurnEvent::ComponentStarted {
                name,
                partition,
                size,
            } => {
                self.active_component = name.clone();
                self.component_progress = 0.0;
                self.log(format!(
                    "{} {} {}={} {}={} ...",
                    self.t(Msg::Meta),
                    name,
                    self.t(Msg::PartitionField),
                    partition,
                    self.t(Msg::SizeField),
                    size
                ));
            }
            BurnEvent::ComponentProgress { name, sent, total } => {
                self.active_component = name;
                self.component_progress = progress(sent, total);
            }
            BurnEvent::OverallProgress { sent, total } => {
                self.burn_progress = progress(sent, total);
                self.overall_sent = sent;
                self.overall_total = total;
                self.refresh_burn_rate();
            }
            BurnEvent::ComponentFinished { name } => {
                self.log(format!("{}: {}", self.t(Msg::BurnComponentSuccess), name))
            }
            BurnEvent::Finished => {
                self.burn_progress = 1.0;
                self.refresh_burn_rate();
                self.log(self.t(Msg::BurnOnlineSuccess));
            }
        }
    }

    fn refresh_burn_rate(&mut self) {
        if let Some(started) = self.burn_started_at {
            let elapsed = started.elapsed().as_secs_f64();
            if elapsed > 0.0 && self.overall_sent > 0 {
                self.burn_rate_bps = self.overall_sent as f64 / elapsed;
            }
        }
    }

    fn burn_status_line(&self) -> String {
        // Live while burning; frozen at the completion value afterwards.
        let elapsed = self
            .burn_started_at
            .map(|started| started.elapsed())
            .or(self.burn_finished_elapsed)
            .unwrap_or_default();
        format!(
            "{}: {}  {}: {}  {}: {}/{}",
            self.t(Msg::Elapsed),
            format_elapsed(elapsed),
            self.t(Msg::Rate),
            format_rate(self.burn_rate_bps),
            self.t(Msg::Overall),
            self.overall_sent,
            self.overall_total
        )
    }

    fn log(&mut self, line: impl Into<String>) {
        let elapsed = self.log_started_at.elapsed();
        let minutes = elapsed.as_secs() / 60;
        let seconds = elapsed.as_secs() % 60;
        self.log_lines
            .push(format!("[{:02}:{:02}] {}", minutes, seconds, line.into()));
        if self.log_lines.len() > 1500 {
            let overflow = self.log_lines.len() - 1500;
            self.log_lines.drain(0..overflow);
        }
    }

    fn log_error(&mut self, error: String) {
        self.log(format!("{}: {}", self.t(Msg::ErrorPrefix), error));
        if error.contains("macOS has not configured it for USB transfers")
            || error.contains("kUSBCurrentConfiguration")
            || error.contains("IOUSBHostInterface")
        {
            self.log(macos_recovery_hint(self.lang()));
        }
    }

    fn log_not_ready_device(&mut self, device: &DeviceInfo, context: NotReadyContext) {
        let status = device
            .status
            .as_deref()
            .map(|status| localize_device_status(self.lang(), status))
            .unwrap_or_else(|| readiness_label(self.lang(), false).to_string());
        self.log(format!(
            "{}: {} {} ({})",
            not_ready_context_label(self.lang(), context),
            format_device_ref(device),
            readiness_label(self.lang(), false),
            status
        ));
        self.log(macos_recovery_hint(self.lang()));
    }

    fn ui_top_bar(&mut self, ui: &mut egui::Ui) {
        let burn = self.t(Msg::TabBurn);
        let tools = self.t(Msg::TabTools);
        let settings = self.t(Msg::TabSettings);
        let scan = self.t(Msg::Scan);
        let device_info = self.t(Msg::DeviceInfo);
        ui.horizontal(|ui| {
            selectable_tab(ui, &mut self.tab, Tab::Burn, burn);
            selectable_tab(ui, &mut self.tab, Tab::Tools, tools);
            selectable_tab(ui, &mut self.tab, Tab::Settings, settings);
            ui.separator();
            ui.add_enabled_ui(!self.device_scan_in_progress, |ui| {
                if ui.button(scan).clicked() {
                    self.start_device_scan(ui.ctx().clone(), true);
                }
            });
            ui.add_enabled_ui(!self.busy, |ui| {
                if ui.button(device_info).clicked() {
                    self.start_read_device_info();
                }
            });
        });
    }

    fn ui_burn(&mut self, ui: &mut egui::Ui) {
        ui.heading(format!(
            "{} {}",
            self.t(Msg::AppTitle),
            build_info::VERSION_WITH_BUILD
        ));
        let lang = self.lang();
        self.ui_transport(ui);
        if self.transport == TransportKind::Usb {
            ui.horizontal(|ui| {
                ui.label(self.t(Msg::Device));
                egui::ComboBox::from_id_salt("device_select")
                    .selected_text(self.selected_device_label())
                    .show_ui(ui, |ui| {
                        for (idx, device) in self.devices.iter().enumerate() {
                            ui.selectable_value(
                                &mut self.selected_device,
                                Some(idx),
                                format!(
                                    "{}:{}  {:04x}:{:04x}  {}  {}",
                                    device.bus_number,
                                    device.port_path_or_address(),
                                    device.vendor_id,
                                    device.product_id,
                                    device.speed,
                                    readiness_label(lang, device.ready)
                                ),
                            );
                        }
                    });
            });
        } else {
            self.ui_serial_selector(ui);
            self.ui_monitor(ui);
        }

        // Former Image tab, folded in: file/history/header/extract live here
        // so the whole open → verify → burn flow stays on one page.
        egui::CollapsingHeader::new(self.t(Msg::TabImage))
            .id_salt("burn_image_section")
            .default_open(true)
            .show(ui, |ui| self.ui_image_section(ui));
        egui::CollapsingHeader::new(self.t(Msg::Partitions))
            .id_salt("burn_parts_section")
            .default_open(true)
            .show(ui, |ui| self.ui_partition_selector(ui));
        egui::CollapsingHeader::new(self.t(Msg::TabBurn))
            .id_salt("burn_control_section")
            .default_open(true)
            .show(ui, |ui| self.ui_burn_control(ui));
    }

    /// Image file row, history, component extraction and the full header
    /// grid. The old one-line summary is intentionally dropped: every field
    /// it showed (platform/product/version/media) is in the grid below.
    fn ui_image_section(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label(self.t(Msg::Image));
            let mut text = self
                .config
                .image_path
                .as_ref()
                .map(|p| p.display().to_string())
                .unwrap_or_default();
            let response = ui.add(
                egui::TextEdit::singleline(&mut text)
                    .desired_width(f32::INFINITY)
                    .hint_text(self.t(Msg::SelectImageFile)),
            );
            if response.lost_focus() && !text.trim().is_empty() {
                let path = PathBuf::from(text.trim());
                if Some(&path) != self.config.image_path.as_ref() {
                    self.load_image(path);
                }
            }
            if ui.button(self.t(Msg::Browse)).clicked() {
                if let Some(path) = rfd::FileDialog::new()
                    .add_filter(self.t(Msg::ArtInChipImageFilter), &["img"])
                    .pick_file()
                {
                    self.load_image(path);
                }
            }
        });

        if !self.image_history.is_empty() {
            egui::ComboBox::from_id_salt("history")
                .selected_text(self.t(Msg::ImageHistory))
                .show_ui(ui, |ui| {
                    let items = self.image_history.clone();
                    for (path, ts) in items {
                        if ui.button(format!("{}  {}", ts, path.display())).clicked() {
                            self.load_image(path);
                        }
                    }
                });
        }

        ui.horizontal(|ui| {
            if ui.button(self.t(Msg::OpenImage)).clicked() {
                if let Some(path) = rfd::FileDialog::new()
                    .add_filter(self.t(Msg::ArtInChipImageFilter), &["img"])
                    .pick_file()
                {
                    self.load_image(path);
                }
            }
            ui.add_enabled_ui(self.config.image_path.is_some(), |ui| {
                if ui.button(self.t(Msg::ExtractComponents)).clicked() {
                    if let Some(image) = self.config.image_path.clone() {
                        if let Some(dir) = rfd::FileDialog::new().pick_folder() {
                            match parser::extract_components(&image, &dir) {
                                Ok(files) => self.log(format!(
                                    "{}: {} ({})",
                                    self.t(Msg::ExtractedComponentsTo),
                                    dir.display(),
                                    files.len()
                                )),
                                Err(e) => self.log(format!("{}: {}", self.t(Msg::ExtractFailed), e)),
                            }
                        }
                    }
                }
            });
        });

        if let Some(summary) = &self.image_summary {
            egui::Grid::new("image_header")
                .striped(true)
                .show(ui, |ui| {
                    row(ui, self.t(Msg::Magic), &summary.magic);
                    row(ui, self.t(Msg::Platform), &summary.platform);
                    row(ui, self.t(Msg::Product), &summary.product);
                    row(ui, self.t(Msg::Version), &summary.version);
                    row(ui, self.t(Msg::MediaType), &summary.media_type);
                    row(ui, self.t(Msg::MediaId), &summary.media_id);
                    row(
                        ui,
                        self.t(Msg::MediaDev),
                        &format!("{:#x}", summary.media_dev_id),
                    );
                    row(
                        ui,
                        self.t(Msg::MetaOffset),
                        &format!("{:#x}", summary.meta_offset),
                    );
                    row(ui, self.t(Msg::MetaSize), &summary.meta_size.to_string());
                    row(
                        ui,
                        self.t(Msg::FileOffset),
                        &format!("{:#x}", summary.file_offset),
                    );
                    row(ui, self.t(Msg::FileSize), &summary.file_size.to_string());
                });
        }
    }

    fn ui_burn_control(&mut self, ui: &mut egui::Ui) {
        ui.add(egui::ProgressBar::new(self.burn_progress).text(self.t(Msg::Overall)));
        ui.add(egui::ProgressBar::new(self.component_progress).text(self.active_component.clone()));
        ui.label(self.burn_status_line());
        let auto_burn = self.t(Msg::AutoBurn);
        let adb_scan = self.t(Msg::AdbScan);
        let read_device_log = self.t(Msg::ReadDeviceLog);
        let erase_all = self.t(Msg::EraseAll);
        let force_upgrade = self.t(Msg::ForceUpgrade);
        let media_label = self.t(Msg::Media);
        let burn = self.t(Msg::TabBurn);
        let stop = self.t(Msg::Stop);
        let mut stop_requested = false;
        ui.horizontal(|ui| {
            ui.add_enabled_ui(!self.busy, |ui| {
                if ui.button(burn).clicked() {
                    self.start_burn();
                }
            });
            ui.add_enabled_ui(self.busy && self.cancel_flag.is_some(), |ui| {
                if ui.button(stop).clicked() {
                    stop_requested = true;
                }
            });
            ui.checkbox(&mut self.config.auto_burn, auto_burn);
            ui.checkbox(&mut self.config.adb_scan, adb_scan);
            ui.checkbox(&mut self.config.read_device_log, read_device_log);
        });
        ui.horizontal(|ui| {
            ui.add_enabled_ui(!self.busy, |ui| {
                ui.checkbox(&mut self.config.erase_all, erase_all);
                ui.checkbox(&mut self.config.force_upgrade, force_upgrade);
            });
            ui.label(media_label);
            ui.add(
                egui::TextEdit::singleline(&mut self.config.erase_media)
                    .hint_text("auto")
                    .desired_width(64.0),
            );
        });
        if self.config.erase_all {
            ui.label(
                egui::RichText::new(self.t(Msg::EraseAllNote))
                    .small()
                    .weak(),
            );
        }
        if self.config.force_upgrade {
            ui.label(
                egui::RichText::new(self.t(Msg::ForceUpgradeNote))
                    .small()
                    .weak(),
            );
        }
        if stop_requested {
            self.stop_burn();
        }
    }

    fn ui_transport(&mut self, ui: &mut egui::Ui) {
        let mut transport = self.transport;
        let mut scan_requested = false;
        let scanning = self.device_scan_in_progress;
        ui.horizontal(|ui| {
            ui.label(self.t(Msg::Transport));
            egui::ComboBox::from_id_salt("transport_select")
                .selected_text(match transport {
                    TransportKind::Usb => self.t(Msg::TransportUsb),
                    TransportKind::Uart => self.t(Msg::TransportUart),
                })
                .show_ui(ui, |ui| {
                    ui.selectable_value(
                        &mut transport,
                        TransportKind::Usb,
                        self.t(Msg::TransportUsb),
                    );
                    ui.selectable_value(
                        &mut transport,
                        TransportKind::Uart,
                        self.t(Msg::TransportUart),
                    );
                });
            if transport == TransportKind::Uart {
                ui.separator();
                ui.label(self.t(Msg::BaudRate));
                ui.add(
                    egui::DragValue::new(&mut self.config.serial_baud)
                        .range(1200..=6_000_000)
                        .speed(100.0),
                );
                ui.label(self.t(Msg::MaxBaudRate));
                ui.add(
                    egui::DragValue::new(&mut self.config.serial_speed)
                        .range(0..=6_000_000)
                        .speed(100.0),
                );
                ui.separator();
                ui.add_enabled_ui(!scanning, |ui| {
                    if ui.button(self.t(Msg::Refresh)).clicked() {
                        scan_requested = true;
                    }
                });
            }
        });
        if transport != self.transport {
            if self.transport == TransportKind::Uart {
                self.stop_monitor();
            }
            self.transport = transport;
            self.config.transport = transport.config_value().to_string();
            scan_requested = true;
        }
        if scan_requested {
            let ctx = ui.ctx().clone();
            self.start_device_scan(ctx, true);
        }
        if self.transport == TransportKind::Uart {
            ui.label(
                egui::RichText::new(self.t(Msg::UartModeHint))
                    .small()
                    .weak(),
            );
        }
    }

    fn ui_monitor(&mut self, ui: &mut egui::Ui) {
        let connect_label = self.t(Msg::ConnectMonitor);
        let disconnect_label = self.t(Msg::DisconnectMonitor);
        let trigger_label = self.t(Msg::EnterUpgradeMode);
        let send_label = self.t(Msg::Send);
        let auto_label = self.t(Msg::AutoEnterUpgrade);
        let input_hint = self.t(Msg::MonitorInputHint);
        let monitor_active = self.monitor.is_some();

        let mut connect = false;
        let mut disconnect = false;
        let mut trigger = false;
        let mut send_text: Option<String> = None;

        ui.horizontal(|ui| {
            if monitor_active {
                if ui.button(disconnect_label).clicked() {
                    disconnect = true;
                }
                if ui.button(trigger_label).clicked() {
                    trigger = true;
                }
                let response = ui.add(
                    egui::TextEdit::singleline(&mut self.monitor_input)
                        .hint_text(input_hint)
                        .desired_width(f32::INFINITY),
                );
                let enter = response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                if ui.button(send_label).clicked() || enter {
                    let text = self.monitor_input.trim().to_string();
                    if !text.is_empty() {
                        self.monitor_input.clear();
                        send_text = Some(text);
                    }
                }
            } else if ui.button(connect_label).clicked() {
                connect = true;
            }
            ui.checkbox(&mut self.config.serial_auto_enter, auto_label);
        });
        ui.label(egui::RichText::new(self.t(Msg::MonitorHint)).small().weak());

        if connect {
            self.start_monitor();
        }
        if disconnect {
            self.stop_monitor();
        }
        if trigger {
            self.monitor_trigger_upgrade();
        }
        if let Some(text) = send_text {
            self.monitor_send(&text);
        }
        if self
            .monitor
            .as_ref()
            .is_some_and(|monitor| monitor.is_finished())
        {
            self.stop_monitor();
        }
    }

    fn ui_serial_selector(&mut self, ui: &mut egui::Ui) {
        let selected_text = self.selected_serial_port_label();
        let auto_label = self.t(Msg::SerialPortAuto);
        ui.horizontal(|ui| {
            ui.label(self.t(Msg::SerialPort));
            egui::ComboBox::from_id_salt("serial_port_select")
                .selected_text(selected_text)
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut self.selected_serial_port, None, auto_label);
                    for (idx, port) in self.serial_ports.iter().enumerate() {
                        ui.selectable_value(
                            &mut self.selected_serial_port,
                            Some(idx),
                            &port.port_name,
                        );
                    }
                });
        });
        let port_name = self
            .selected_serial_port
            .and_then(|idx| self.serial_ports.get(idx))
            .map(|port| port.port_name.clone())
            .unwrap_or_default();
        if port_name != self.config.serial_port {
            self.config.serial_port = port_name;
        }
    }

    fn selected_serial_port_name(&self) -> String {
        match self.selected_serial_port {
            Some(idx) => self
                .serial_ports
                .get(idx)
                .map(|port| port.port_name.clone())
                .unwrap_or_default(),
            None => self.config.serial_port.clone(),
        }
    }

    fn selected_serial_port_label(&self) -> String {
        match self.selected_serial_port {
            Some(idx) => self
                .serial_ports
                .get(idx)
                .map(|port| port.port_name.clone())
                .unwrap_or_else(|| self.t(Msg::SerialPortAuto).to_string()),
            None => self.t(Msg::SerialPortAuto).to_string(),
        }
    }

    fn ui_partition_selector(&mut self, ui: &mut egui::Ui) {
        let Some(summary) = &self.image_summary else {
            ui.label(self.t(Msg::NoImageLoaded));
            return;
        };
        let summary = summary.clone();
        let column_burn = self.t(Msg::ColumnBurn);
        let column_name = self.t(Msg::ColumnName);
        let column_partition = self.t(Msg::ColumnPartition);
        let column_size = self.t(Msg::ColumnSize);
        let column_offset = self.t(Msg::ColumnOffset);
        let column_crc = self.t(Msg::ColumnCrc);
        ui.label(self.t(Msg::Partitions));
        egui::Grid::new("parts_grid")
            .striped(true)
            .min_col_width(90.0)
            .show(ui, |ui| {
                ui.label(column_burn);
                ui.label(column_name);
                ui.label(column_partition);
                ui.label(column_size);
                ui.label(column_offset);
                ui.label(column_crc);
                ui.end_row();
                for meta in &summary.metas {
                    let key = part_key(meta);
                    let is_target = meta.name.starts_with("image.target.");
                    let locked = !is_target;
                    let mut checked = locked
                        || self.selected_parts.iter().any(|part| {
                            part == &key || part == &meta.partition || part == &meta.name
                        });
                    ui.add_enabled_ui(!locked, |ui| {
                        if ui.checkbox(&mut checked, "").changed() {
                            if checked {
                                if !self.selected_parts.contains(&key) {
                                    self.selected_parts.push(key.clone());
                                }
                            } else {
                                self.selected_parts.retain(|part| {
                                    part != &key && part != &meta.partition && part != &meta.name
                                });
                            }
                            self.config.selected_parts = self.selected_parts.clone();
                        }
                    });
                    ui.label(&meta.name);
                    ui.label(meta.partition_display());
                    ui.label(meta.size.to_string());
                    ui.label(format!("{:#x}", meta.offset));
                    ui.label(format!("0x{:08x}", meta.crc));
                    ui.end_row();
                }
            });
    }

    fn ui_tools(&mut self, ui: &mut egui::Ui) {
        let lang = self.lang();
        ui.heading(self.t(Msg::OfficialTools));
        ui.horizontal(|ui| {
            ui.label("upgcmd");
            let mut path_text = self.config.upgcmd_path.display().to_string();
            let response =
                ui.add(egui::TextEdit::singleline(&mut path_text).desired_width(f32::INFINITY));
            if response.lost_focus() {
                self.config.upgcmd_path = PathBuf::from(path_text.trim());
            }
            if ui.button(self.t(Msg::Browse)).clicked() {
                let dialog = rfd::FileDialog::new();
                #[cfg(windows)]
                let dialog = dialog.add_filter("upgcmd", &["exe"]);
                if let Some(path) = dialog.pick_file() {
                    self.config.upgcmd_path = path;
                }
            }
        });
        ui.horizontal(|ui| {
            ui.label(self.t(Msg::Command));
            egui::ComboBox::from_id_salt("official_command")
                .selected_text(command_label(lang, self.official_args.command))
                .show_ui(ui, |ui| {
                    for command in OfficialCommand::ALL {
                        ui.selectable_value(
                            &mut self.official_args.command,
                            command,
                            command_label(lang, command),
                        );
                    }
                });
            let verbose = tr(lang, Msg::Verbose);
            let device_log = tr(lang, Msg::DeviceLog);
            let progress = tr(lang, Msg::Progress);
            ui.checkbox(&mut self.official_args.verbose, verbose);
            ui.checkbox(&mut self.official_args.device_log, device_log);
            ui.checkbox(&mut self.official_args.progress, progress);
        });
        // Transport selectors mirror official `upgcmd -d/-u/-b`; empty = default.
        let target_device = tr(lang, Msg::Device);
        let serial_port_label = tr(lang, Msg::SerialPort);
        let baud_rate_label = tr(lang, Msg::BaudRate);
        ui.horizontal(|ui| {
            ui.label(target_device);
            ui.add(
                egui::TextEdit::singleline(&mut self.official_args.device)
                    .hint_text("bus:port")
                    .desired_width(80.0),
            );
            ui.label(serial_port_label);
            ui.add(
                egui::TextEdit::singleline(&mut self.official_args.uart_port)
                    .hint_text("COMx")
                    .desired_width(80.0),
            );
            ui.label(baud_rate_label);
            ui.add(
                egui::TextEdit::singleline(&mut self.official_args.baudrate)
                    .hint_text("921600")
                    .desired_width(80.0),
            );
        });

        self.ui_official_args(ui);
        ui.separator();
        ui.horizontal(|ui| {
            ui.add_enabled_ui(!self.busy, |ui| {
                if ui.button(self.t(Msg::Run)).clicked() {
                    self.start_official_command();
                }
            });
            match official::build_args(&self.official_args) {
                Ok(args) => {
                    ui.label(format!("{}: {}", self.t(Msg::ArgsPrefix), args.join(" ")));
                }
                Err(e) => {
                    ui.colored_label(egui::Color32::from_rgb(180, 40, 40), e);
                }
            }
        });
        ui.separator();
        ui.horizontal(|ui| {
            if ui.button(self.t(Msg::EnvCheck)).clicked() {
                let report = standalone::environment_report(self.config.image_path.as_deref());
                for line in report.lines() {
                    self.log(line);
                }
            }
            if ui.button(self.t(Msg::Driver)).clicked() {
                match standalone::install_driver() {
                    Ok(()) => self.log(self.t(Msg::StartedDriverInstaller)),
                    Err(e) => self.log(e),
                }
            }
            if ui.button(self.t(Msg::Manual)).clicked() {
                match official::open_manual(&self.config.aiburn_dir) {
                    Ok(()) => self.log(self.t(Msg::OpenedManual)),
                    Err(e) => self.log(e),
                }
            }
        });
        ui.horizontal(|ui| {
            ui.add_enabled_ui(!self.busy, |ui| {
                if ui.button(self.t(Msg::SdList)).clicked() {
                    self.start_sd_list();
                }
            });
            ui.label(
                egui::RichText::new(match self.lang() {
                    Language::ZhCn => "只读枚举启动卡目标盘；写卡仍需官方 AiBurn（见启动卡设计文档）",
                    Language::En => {
                        "Read-only boot-card target probe; writing still needs official AiBurn (see boot-card design doc)"
                    }
                })
                .small()
                .weak(),
            );
        });
    }

    fn ui_official_args(&mut self, ui: &mut egui::Ui) {
        match self.official_args.command {
            OfficialCommand::ListDevices
            | OfficialCommand::DeviceLog
            | OfficialCommand::ContinueBoot
            | OfficialCommand::GoToBootloader => {}
            OfficialCommand::ImageInfo
            | OfficialCommand::ExtractImage
            | OfficialCommand::UpgradeImage
            | OfficialCommand::ListMedia
            | OfficialCommand::FlashErase
            | OfficialCommand::RamBoot => {
                let label = self.t(Msg::Image);
                let browse = self.t(Msg::Browse);
                path_picker(ui, label, browse, &mut self.official_args.image, false);
            }
            _ => {}
        }
        match self.official_args.command {
            OfficialCommand::WriteMemory
            | OfficialCommand::BdefuseWrite
            | OfficialCommand::AuzWriteFuse => {
                let label = self.t(Msg::Input);
                let browse = self.t(Msg::Browse);
                path_picker(ui, label, browse, &mut self.official_args.input, false);
            }
            OfficialCommand::DumpPartition
            | OfficialCommand::ReadMemory
            | OfficialCommand::BdefuseRead
            | OfficialCommand::JtagUnlockData => {
                let label = self.t(Msg::Output);
                let browse = self.t(Msg::Browse);
                path_picker(ui, label, browse, &mut self.official_args.output, true);
            }
            _ => {}
        }
        match self.official_args.command {
            OfficialCommand::DumpPartition
            | OfficialCommand::ListPartitions
            | OfficialCommand::FlashErase => {
                ui.horizontal(|ui| {
                    ui.label(self.t(Msg::Media));
                    ui.text_edit_singleline(&mut self.official_args.media);
                });
            }
            _ => {}
        }
        if self.official_args.command == OfficialCommand::DumpPartition {
            ui.horizontal(|ui| {
                ui.label(self.t(Msg::ColumnPartition));
                ui.text_edit_singleline(&mut self.official_args.partition);
            });
        }
        if self.official_args.command == OfficialCommand::ShellCommand {
            ui.horizontal(|ui| {
                ui.label(self.t(Msg::Shell));
                ui.text_edit_singleline(&mut self.official_args.shell);
            });
        }
        if self.official_args.command == OfficialCommand::UpgradeImage {
            let hint = self.t(Msg::FwcListHint);
            ui.horizontal(|ui| {
                ui.label(self.t(Msg::FwcList));
                ui.add(
                    egui::TextEdit::singleline(&mut self.official_args.fwc_list)
                        .hint_text(hint)
                        .desired_width(f32::INFINITY),
                );
            });
        }
        if matches!(
            self.official_args.command,
            OfficialCommand::WriteMemory
                | OfficialCommand::ReadMemory
                | OfficialCommand::WriteLong
                | OfficialCommand::ReadLong
                | OfficialCommand::MemTest
                | OfficialCommand::Exec
                | OfficialCommand::HexDump
                | OfficialCommand::Fill
                | OfficialCommand::Clear
                | OfficialCommand::BdefuseRead
                | OfficialCommand::BdefuseDump
                | OfficialCommand::BdefuseWrite
                | OfficialCommand::BdefuseWriteHex
        ) {
            ui.horizontal(|ui| {
                ui.label(self.t(Msg::Address));
                ui.text_edit_singleline(&mut self.official_args.address);
                if matches!(
                    self.official_args.command,
                    OfficialCommand::ReadMemory
                        | OfficialCommand::MemTest
                        | OfficialCommand::HexDump
                        | OfficialCommand::Fill
                        | OfficialCommand::Clear
                        | OfficialCommand::WriteMemory
                        | OfficialCommand::BdefuseRead
                        | OfficialCommand::BdefuseDump
                        | OfficialCommand::BdefuseWrite
                ) {
                    ui.label(self.t(Msg::Length));
                    ui.text_edit_singleline(&mut self.official_args.length);
                }
                if matches!(
                    self.official_args.command,
                    OfficialCommand::WriteLong
                        | OfficialCommand::Fill
                        | OfficialCommand::BdefuseWriteHex
                ) {
                    ui.label(self.t(Msg::Value));
                    ui.text_edit_singleline(&mut self.official_args.value);
                }
                if self.official_args.command == OfficialCommand::MemTest {
                    ui.label(self.t(Msg::Round));
                    ui.text_edit_singleline(&mut self.official_args.round);
                }
            });
        }
        if self.official_args.command == OfficialCommand::BdefuseSelect {
            ui.horizontal(|ui| {
                ui.label(self.t(Msg::EfuseId));
                ui.text_edit_singleline(&mut self.official_args.efuse_id);
            });
        }
        if self.official_args.command == OfficialCommand::WriteMemory {
            let optional = self.t(Msg::Optional);
            ui.horizontal(|ui| {
                ui.label(self.t(Msg::Skip));
                ui.add(
                    egui::TextEdit::singleline(&mut self.official_args.skip)
                        .hint_text(optional)
                        .desired_width(120.0),
                );
            });
        }
        if self.official_args.command == OfficialCommand::RamBoot {
            ui.horizontal(|ui| {
                ui.label(self.t(Msg::Fwc));
                ui.text_edit_singleline(&mut self.official_args.fwc_name);
                ui.label(self.t(Msg::Ram));
                ui.text_edit_singleline(&mut self.official_args.ram_address);
            });
        }
        if matches!(
            self.official_args.command,
            OfficialCommand::Raw | OfficialCommand::JtagUnlock
        ) {
            ui.horizontal(|ui| {
                ui.label(self.t(Msg::Args));
                ui.add(
                    egui::TextEdit::singleline(&mut self.official_args.raw_args)
                        .desired_width(f32::INFINITY),
                );
            });
        }
    }

    fn ui_settings(&mut self, ui: &mut egui::Ui) {
        ui.heading(self.t(Msg::TabSettings));
        ui.horizontal(|ui| {
            ui.label(self.t(Msg::AppDataDir));
            let mut text = self.config.app_dir.display().to_string();
            let response =
                ui.add(egui::TextEdit::singleline(&mut text).desired_width(f32::INFINITY));
            if response.lost_focus() && !text.trim().is_empty() {
                let path = PathBuf::from(text.trim());
                if path != self.config.app_dir {
                    self.config.app_dir = path.clone();
                    self.settings_path = path.join("config.ini");
                    self.image_history = load_image_history(&self.config.app_dir);
                }
            }
            if ui.button(self.t(Msg::Browse)).clicked() {
                if let Some(path) = rfd::FileDialog::new().pick_folder() {
                    self.config.app_dir = path.clone();
                    self.settings_path = path.join("config.ini");
                    self.image_history = load_image_history(&self.config.app_dir);
                }
            }
        });
        ui.horizontal(|ui| {
            ui.label(self.t(Msg::AiBurnDir));
            let mut text = self.config.aiburn_dir.display().to_string();
            let response =
                ui.add(egui::TextEdit::singleline(&mut text).desired_width(f32::INFINITY));
            if response.lost_focus() {
                let path = PathBuf::from(text.trim());
                if path != self.config.aiburn_dir {
                    self.config.aiburn_dir = path.clone();
                    self.config.upgcmd_path = compat_tool_path(&path);
                }
            }
            if ui.button(self.t(Msg::Browse)).clicked() {
                if let Some(path) = rfd::FileDialog::new().pick_folder() {
                    self.config.aiburn_dir = path.clone();
                    self.config.upgcmd_path = compat_tool_path(&path);
                }
            }
        });
        ui.horizontal(|ui| {
            ui.label(self.t(Msg::Language));
            let mut lang = self.lang();
            egui::ComboBox::from_id_salt("language_select")
                .selected_text(lang.native_name())
                .show_ui(ui, |ui| {
                    for candidate in Language::ALL {
                        ui.selectable_value(&mut lang, candidate, candidate.native_name());
                    }
                });
            self.config.language = lang.code().to_string();
            ui.label(self.t(Msg::TimeoutSeconds));
            ui.add(egui::DragValue::new(&mut self.config.burn_timeout_secs).range(1..=3600));
            ui.label(self.t(Msg::Retry));
            ui.add(egui::DragValue::new(&mut self.config.retry_count).range(1..=20));
        });
        let verbose_log = self.t(Msg::VerboseLog);
        let block_error_log = self.t(Msg::BlockErrorLog);
        let auto_burn = self.t(Msg::AutoBurnWhenReady);
        let adb_scan = self.t(Msg::AdbScan);
        let read_device_log = self.t(Msg::ReadDeviceLog);
        ui.checkbox(&mut self.config.verbose, verbose_log);
        ui.checkbox(&mut self.config.block_error_log, block_error_log);
        ui.checkbox(&mut self.config.auto_burn, auto_burn);
        ui.checkbox(&mut self.config.adb_scan, adb_scan);
        ui.checkbox(&mut self.config.read_device_log, read_device_log);
        verbosity::set_verbose(self.config.verbose);
        ui.separator();
        self.ui_burn_stats(ui);
        ui.separator();
        self.ui_update_settings(ui);
        ui.horizontal(|ui| {
            if ui.button(self.t(Msg::LoadAiBurnIni)).clicked() {
                match AppConfig::load_from(&self.settings_path) {
                    Ok(config) => {
                        self.config = config;
                        self.selected_parts = self.config.selected_parts.clone();
                        self.log(self.t(Msg::LoadedAiBurnIni));
                    }
                    Err(e) => self.log(e),
                }
            }
            if ui.button(self.t(Msg::SaveAiBurnIni)).clicked() {
                self.config.selected_parts = self.selected_parts.clone();
                match self.config.save_to(&self.settings_path) {
                    Ok(()) => self.log(format!(
                        "{} {}",
                        self.t(Msg::Saved),
                        self.settings_path.display()
                    )),
                    Err(e) => self.log(e),
                }
            }
        });
    }

    fn ui_burn_stats(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.heading(self.t(Msg::BurnStats));
            if ui.button(self.t(Msg::Refresh)).clicked() {
                // Stats are read fresh on every frame; just repaint.
                ui.ctx().request_repaint();
            }
            if ui.button(self.t(Msg::ClearStats)).clicked() {
                match burn_stats::clear(&self.config.app_dir) {
                    Ok(()) => self.log(self.t(Msg::ClearStats)),
                    Err(e) => self.log_error(e),
                }
            }
        });
        let stats = burn_stats::load(&self.config.app_dir);
        if stats.days.is_empty() {
            ui.label(match self.lang() {
                Language::ZhCn => "暂无烧写统计，完成一次烧录后自动计数。",
                Language::En => "No burn statistics yet; counted automatically after a burn.",
            });
            return;
        }
        egui::Grid::new("burn_stats_grid")
            .striped(true)
            .min_col_width(70.0)
            .show(ui, |ui| {
                ui.label(match self.lang() {
                    Language::ZhCn => "日期",
                    Language::En => "date",
                });
                ui.label(match self.lang() {
                    Language::ZhCn => "成功",
                    Language::En => "success",
                });
                ui.label(match self.lang() {
                    Language::ZhCn => "失败",
                    Language::En => "failure",
                });
                ui.label(match self.lang() {
                    Language::ZhCn => "取消",
                    Language::En => "cancelled",
                });
                ui.label(match self.lang() {
                    Language::ZhCn => "成功率",
                    Language::En => "rate",
                });
                ui.end_row();
                for (date, day) in &stats.days {
                    ui.monospace(date);
                    ui.label(day.success.to_string());
                    ui.label(day.failure.to_string());
                    ui.label(day.cancelled.to_string());
                    ui.label(
                        day.success_rate()
                            .map(|r| format!("{:.1}%", r * 100.0))
                            .unwrap_or_else(|| "--".to_string()),
                    );
                    ui.end_row();
                }
            });
        let total = stats.total();
        ui.label(format!(
            "{}: {} / {} ({})",
            match self.lang() {
                Language::ZhCn => "累计",
                Language::En => "total",
            },
            total.success,
            total.success + total.failure,
            total
                .success_rate()
                .map(|r| format!("{:.1}%", r * 100.0))
                .unwrap_or_else(|| "--".to_string()),
        ));
    }

    fn ui_update_settings(&mut self, ui: &mut egui::Ui) {
        let channel_label = self.t(Msg::UpdateChannel);
        let auto_label = self.t(Msg::AutoCheckUpdate);
        let check_label = self.t(Msg::CheckUpdate);
        let open_label = self.t(Msg::OpenReleasePage);
        ui.horizontal(|ui| {
            ui.label(channel_label);
            let mut channel = self.config.update_channel.clone();
            egui::ComboBox::from_id_salt("update_channel_select")
                .selected_text(channel.clone())
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut channel, "stable".to_string(), "stable");
                    ui.selectable_value(&mut channel, "nightly".to_string(), "nightly");
                });
            self.config.update_channel = channel;
            ui.checkbox(&mut self.config.auto_check_update, auto_label);
        });
        ui.horizontal(|ui| {
            ui.add_enabled_ui(!self.update_check_in_progress, |ui| {
                if ui.button(check_label).clicked() {
                    self.start_update_check(ui.ctx().clone());
                }
            });
            if ui.button(open_label).clicked() {
                let channel = UpdateChannel::from_str(&self.config.update_channel);
                let url = match channel {
                    UpdateChannel::Nightly => update::nightly_url(),
                    UpdateChannel::Stable => update::releases_url(),
                };
                if let Err(e) = update::open_url(&url) {
                    self.log(e);
                }
            }
            if !self.update_status.is_empty() {
                ui.label(&self.update_status);
            }
        });
        ui.label(format!(
            "{} {}",
            build_info::VERSION_WITH_BUILD,
            if update::is_portable_install() {
                "(portable)"
            } else {
                "(managed install)"
            }
        ));
    }

    fn ui_log(&mut self, ui: &mut egui::Ui) {
        let mut copy_requested = false;
        let mut save_requested = false;
        ui.horizontal(|ui| {
            ui.label(self.t(Msg::Log));
            if ui.button(self.t(Msg::CopyLog)).clicked() {
                copy_requested = true;
            }
            if ui.button(self.t(Msg::SaveLog)).clicked() {
                save_requested = true;
            }
            if ui.button(self.t(Msg::Clear)).clicked() {
                self.log_lines.clear();
            }
        });
        if copy_requested {
            self.copy_log(ui.ctx());
        }
        if save_requested {
            self.save_log_to_file();
        }
        egui::ScrollArea::vertical()
            .stick_to_bottom(true)
            .max_height(ui.available_height())
            .show(ui, |ui| {
                for line in &self.log_lines {
                    ui.add(
                        egui::Label::new(egui::RichText::new(line).monospace())
                            .selectable(true),
                    );
                }
            });
    }

    fn copy_log(&mut self, ctx: &egui::Context) {
        let text = self.log_lines.join("\n");
        ctx.copy_text(text);
        self.log(format!(
            "{} ({} lines)",
            self.t(Msg::LogCopied),
            self.log_lines.len()
        ));
    }

    fn save_log_to_file(&mut self) {
        let default = services::default_log_path(&self.config.app_dir);
        let dialog = rfd::FileDialog::new()
            .set_file_name(default.file_name().and_then(|n| n.to_str()).unwrap_or(
                "artinchip-flash.log",
            ));
        let dialog = match default.parent() {
            Some(dir) => dialog.set_directory(dir),
            None => dialog,
        };
        let Some(path) = dialog.save_file() else {
            return;
        };
        match services::write_log_file(&path, &self.log_lines) {
            Ok(()) => self.log(format!("{} {}", self.t(Msg::LogSavedTo), path.display())),
            Err(e) => self.log_error(e),
        }
    }

    fn selected_device_label(&self) -> String {
        self.selected_device
            .and_then(|idx| self.devices.get(idx))
            .map(|device| {
                format!(
                    "{}:{}  {:04x}:{:04x}  {}",
                    device.bus_number,
                    device.port_path_or_address(),
                    device.vendor_id,
                    device.product_id,
                    readiness_label(self.lang(), device.ready)
                )
            })
            .unwrap_or_else(|| self.t(Msg::NoDevice).to_string())
    }
}

impl eframe::App for GuiApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        ctx.send_viewport_cmd(egui::ViewportCommand::Title(app_window_title(self.lang())));
        self.poll_worker(ctx);
        // Keep the rate/elapsed label ticking while a burn is in flight,
        // even between chunk progress events.
        if self.busy {
            ctx.request_repaint_after(Duration::from_millis(500));
        }
        egui::TopBottomPanel::top("top").show(ctx, |ui| self.ui_top_bar(ui));
        // VSCode-terminal style: the user can drag the separator to resize
        // the log area; the inner scroll view fills whatever height results.
        egui::TopBottomPanel::bottom("log_panel")
            .resizable(true)
            .default_height(220.0)
            .min_height(80.0)
            .show(ctx, |ui| self.ui_log(ui));
        egui::CentralPanel::default().show(ctx, |ui| {
            // The merged burn page is taller than the window: scroll the tab
            // content while the log stays pinned at the bottom.
            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .show(ui, |ui| match self.tab {
                    Tab::Burn => self.ui_burn(ui),
                    Tab::Tools => self.ui_tools(ui),
                    Tab::Settings => self.ui_settings(ui),
                });
        });
    }
}

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title(app_window_title(Language::from_code("zh_cn")))
            .with_icon(app_icon())
            .with_inner_size([1120.0, 760.0]),
        ..Default::default()
    };
    eframe::run_native(
        &app_window_title(Language::from_code("zh_cn")),
        options,
        Box::new(|cc| Ok(Box::new(GuiApp::new(cc)))),
    )
}

fn app_icon() -> egui::IconData {
    let image = image::load_from_memory(include_bytes!("../../assets/artinchip-flash.png"))
        .expect("embedded application icon must be a valid PNG")
        .into_rgba8();
    let (width, height) = image.dimensions();
    egui::IconData {
        rgba: image.into_raw(),
        width,
        height,
    }
}

fn app_window_title(lang: Language) -> String {
    format!(
        "{} {}",
        tr(lang, Msg::AppTitle),
        build_info::VERSION_WITH_BUILD
    )
}

fn selectable_tab(ui: &mut egui::Ui, current: &mut Tab, tab: Tab, label: &str) {
    if ui.selectable_label(*current == tab, label).clicked() {
        *current = tab;
    }
}

fn row(ui: &mut egui::Ui, key: &str, value: &str) {
    ui.label(key);
    ui.monospace(value);
    ui.end_row();
}

fn path_picker(
    ui: &mut egui::Ui,
    label: &str,
    browse_label: &str,
    path: &mut Option<PathBuf>,
    save: bool,
) {
    ui.horizontal(|ui| {
        ui.label(label);
        let mut text = path
            .as_ref()
            .map(|p| p.display().to_string())
            .unwrap_or_default();
        ui.add(egui::TextEdit::singleline(&mut text).desired_width(f32::INFINITY));
        if ui.button(browse_label).clicked() {
            let picked = if save {
                rfd::FileDialog::new().save_file()
            } else {
                rfd::FileDialog::new().pick_file()
            };
            if let Some(picked) = picked {
                *path = Some(picked);
            }
        }
    });
}

fn target_metas(summary: &ImageSummary) -> impl Iterator<Item = &MetaSummary> {
    summary
        .metas
        .iter()
        .filter(|meta| services::is_target_component(&meta.name))
}

fn part_key(meta: &MetaSummary) -> String {
    services::partition_key(&meta.name).to_string()
}

fn progress(sent: usize, total: usize) -> f32 {
    services::progress_ratio(sent, total)
}

fn format_rate(bps: f64) -> String {
    if bps <= 0.0 {
        return "--".to_string();
    }
    const MIB: f64 = 1024.0 * 1024.0;
    const KIB: f64 = 1024.0;
    if bps >= MIB {
        format!("{:.2} MiB/s", bps / MIB)
    } else if bps >= KIB {
        format!("{:.1} KiB/s", bps / KIB)
    } else {
        format!("{:.0} B/s", bps)
    }
}

fn format_elapsed(elapsed: Duration) -> String {
    let secs = elapsed.as_secs();
    format!("{:02}:{:02}", secs / 60, secs % 60)
}

trait DeviceLabel {
    fn port_path_or_address(&self) -> String;
}

impl DeviceLabel for DeviceInfo {
    fn port_path_or_address(&self) -> String {
        if self.port_path.is_empty() {
            self.address.to_string()
        } else {
            self.port_path.clone()
        }
    }
}

fn open_uart_backend(port: &str, options: UartOptions) -> Result<UartDevice, String> {
    // Shared implementation lives in `services`.
    services::open_uart(&UartSpec::new(
        port,
        options.baudrate,
        options.max_baudrate,
        options.auto_enter,
    ))
}

fn current_unix_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn update_recently_checked(last_unix: u64) -> bool {
    const DAY_SECS: u64 = 24 * 3600;
    last_unix > 0 && current_unix_secs().saturating_sub(last_unix) < DAY_SECS
}

fn scan_summary(lang: Language, detected: usize, ready: usize, not_ready: usize) -> String {
    match lang {
        Language::ZhCn => format!(
            "扫描完成：检测到 {} 个 ArtInChip 设备，{} 个就绪，{} 个未就绪",
            detected, ready, not_ready
        ),
        Language::En => format!(
            "Scan complete: detected {} ArtInChip device(s), {} ready, {} not ready",
            detected, ready, not_ready
        ),
    }
}

fn readiness_label(lang: Language, ready: bool) -> &'static str {
    match (lang, ready) {
        (Language::ZhCn, true) => "就绪",
        (Language::ZhCn, false) => "未就绪",
        (Language::En, true) => "ready",
        (Language::En, false) => "not-ready",
    }
}

fn auto_burn_skipped(lang: Language) -> &'static str {
    match lang {
        Language::ZhCn => "已跳过自动烧录：当前没有就绪的 ArtInChip 设备",
        Language::En => "Auto burn skipped: no ready ArtInChip device",
    }
}

fn not_ready_context_label(lang: Language, context: NotReadyContext) -> &'static str {
    match (lang, context) {
        (Language::ZhCn, NotReadyContext::Scan) => "扫描发现未就绪设备",
        (Language::ZhCn, NotReadyContext::BurnCancelled) => "烧录已取消",
        (Language::ZhCn, NotReadyContext::DeviceInfoCancelled) => "读取设备信息已取消",
        (Language::En, NotReadyContext::Scan) => "Scan found a not-ready device",
        (Language::En, NotReadyContext::BurnCancelled) => "Burn cancelled",
        (Language::En, NotReadyContext::DeviceInfoCancelled) => "Device info cancelled",
    }
}

fn burn_cancelled_no_ready(lang: Language) -> &'static str {
    match lang {
        Language::ZhCn => "烧录已取消：检测到 ArtInChip 设备，但当前没有设备就绪",
        Language::En => "Burn cancelled: detected ArtInChip devices are not ready",
    }
}

fn device_info_cancelled_no_ready(lang: Language) -> &'static str {
    match lang {
        Language::ZhCn => "读取设备信息已取消：检测到 ArtInChip 设备，但当前没有设备就绪",
        Language::En => "Device info cancelled: detected ArtInChip devices are not ready",
    }
}

fn format_device_ref(device: &DeviceInfo) -> String {
    format!(
        "bus={} path={} address={} vid=0x{:04x} pid=0x{:04x} speed={}",
        device.bus_number,
        device.port_path_or_address(),
        device.address,
        device.vendor_id,
        device.product_id,
        device.speed
    )
}

fn localize_device_status(lang: Language, status: &str) -> String {
    if lang == Language::ZhCn
        && (status.contains("kUSBCurrentConfiguration") || status.contains("IOUSBHostInterface"))
    {
        return format!(
            "macOS 已枚举 VID/PID，但还没有为设备创建 USB 配置或接口；原始状态：{}",
            status
        );
    }
    status.to_string()
}

fn macos_recovery_hint(lang: Language) -> &'static str {
    match lang {
        Language::ZhCn => {
            "处理建议：关闭其它 artinchip-flash/AiBurn 实例，拔插或断电重启板子，重新进入升级模式；优先直连 Mac，先避开 hub/dock。"
        }
        Language::En => {
            "Recovery hint: close other artinchip-flash/AiBurn instances, unplug or power-cycle the board, enter upgrade mode again, and test with a direct Mac connection before using hubs/docks."
        }
    }
}

fn install_cjk_font(ctx: &egui::Context) {
    let Some((font_path, font_data)) = load_cjk_font() else {
        eprintln!(
            "Warning: No CJK font found on this system. Chinese characters might not display correctly."
        );
        return;
    };

    let mut fonts = egui::FontDefinitions::default();
    fonts
        .font_data
        .insert("cjk_font".to_owned(), egui::FontData::from_owned(font_data));

    println!("Loading CJK font from: {}", font_path);
    fonts
        .families
        .get_mut(&egui::FontFamily::Proportional)
        .unwrap()
        .insert(0, "cjk_font".to_owned());
    fonts
        .families
        .get_mut(&egui::FontFamily::Monospace)
        .unwrap()
        .insert(0, "cjk_font".to_owned());

    ctx.set_fonts(fonts);
}

fn load_cjk_font() -> Option<(&'static str, Vec<u8>)> {
    let candidates = [
        "/System/Library/Fonts/PingFang.ttc",
        "/System/Library/Fonts/STHeiti Light.ttc",
        "/System/Library/Fonts/Supplemental/Songti.ttc",
        "/Library/Fonts/Arial Unicode.ttf",
        r"C:\Windows\Fonts\msyh.ttc",
        r"C:\Windows\Fonts\msyh.ttf",
        r"C:\Windows\Fonts\NotoSansSC-VF.ttf",
        r"C:\Windows\Fonts\Deng.ttf",
        r"C:\Windows\Fonts\simhei.ttf",
        r"C:\Windows\Fonts\simsun.ttc",
        "/usr/share/fonts/truetype/noto/NotoSansCJK-Regular.ttc",
        "/usr/share/fonts/wenquanyi/wqy-zenhei.ttc",
    ];

    candidates.iter().find_map(|path| {
        if Path::new(path).exists() {
            std::fs::read(path).ok().map(|data| (*path, data))
        } else {
            None
        }
    })
}

#[allow(dead_code)]
fn _path_exists(path: &Path) -> bool {
    path.exists()
}
