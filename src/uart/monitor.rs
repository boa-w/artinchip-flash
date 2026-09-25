use std::io::{BufRead, ErrorKind, Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::Duration;

use serialport::{DataBits, Parity, StopBits};

use super::UartDevice;

const POLL_TIMEOUT: Duration = Duration::from_millis(50);
const BOOT_KEYWORD_MARKER: u8 = 0x16;
const ACK: u8 = 0x06;

#[derive(Debug)]
pub enum MonitorCommand {
    Line(String),
    TriggerUpgrade,
    Close,
}

/// Interactive UART monitor: streams device output as text lines and can send
/// console commands. Used by the CLI `uart-monitor` command and the GUI panel.
pub struct UartMonitor {
    tx: Sender<MonitorCommand>,
    rx: Receiver<String>,
    handle: Option<JoinHandle<()>>,
    finished: Arc<AtomicBool>,
}

impl UartMonitor {
    pub fn start(path: &str, baud: u32) -> Result<Self, String> {
        let port = serialport::new(path, baud)
            .data_bits(DataBits::Eight)
            .parity(Parity::None)
            .stop_bits(StopBits::One)
            .timeout(POLL_TIMEOUT)
            .open()
            .map_err(|e| format!("Failed to open serial port '{}': {}", path, e))?;

        let mut reader = port
            .try_clone()
            .map_err(|e| format!("Failed to clone serial port '{}': {}", path, e))?;
        let mut writer = port;

        let (tx, cmd_rx) = mpsc::channel::<MonitorCommand>();
        let (text_tx, rx) = mpsc::channel::<String>();
        let finished = Arc::new(AtomicBool::new(false));
        let thread_finished = finished.clone();

        let handle = thread::spawn(move || {
            let mut line = Vec::<u8>::new();
            let mut keyword: Option<Vec<u8>> = None;
            let mut buf = [0u8; 512];

            let flush_line = |line: &mut Vec<u8>, text_tx: &Sender<String>| {
                if line.is_empty() {
                    return;
                }
                let text = String::from_utf8_lossy(line).to_string();
                line.clear();
                if !text.trim().is_empty() {
                    let _ = text_tx.send(text);
                }
            };

            loop {
                while let Ok(command) = cmd_rx.try_recv() {
                    match command {
                        MonitorCommand::Line(text) => {
                            let _ = writer.write_all(text.as_bytes());
                            let _ = writer.write_all(b"\r");
                            let _ = writer.flush();
                        }
                        MonitorCommand::TriggerUpgrade => {
                            let steps: &[(&[u8], u64)] = &[
                                (b"\r\n", 300),
                                (b"aicupg gotobl\r", 800),
                                (b"aicupg uart 0\r", 800),
                            ];
                            for (data, delay) in steps {
                                let _ = writer.write_all(data);
                                let _ = writer.flush();
                                thread::sleep(Duration::from_millis(*delay));
                            }
                            let _ = text_tx.send(
                                "[monitor] sent `aicupg gotobl` / `aicupg uart 0`; reset the board if it does not reboot into upgrade mode"
                                    .to_string(),
                            );
                        }
                        MonitorCommand::Close => {
                            thread_finished.store(true, Ordering::SeqCst);
                            return;
                        }
                    }
                }

                match reader.read(&mut buf) {
                    Ok(n) if n > 0 => {
                        for &byte in &buf[..n] {
                            if byte == BOOT_KEYWORD_MARKER {
                                keyword = Some(Vec::new());
                                continue;
                            }
                            if let Some(pending) = keyword.as_mut() {
                                pending.push(byte);
                                if byte == b'\n' || pending.len() > 24 {
                                    let text = String::from_utf8_lossy(pending).to_string();
                                    keyword = None;
                                    if text.contains("AIBURN") {
                                        let _ = writer.write_all(&[ACK]);
                                        let _ = writer.flush();
                                        let _ = text_tx.send(format!(
                                            "[monitor] acknowledged boot keyword '{}'",
                                            text.trim()
                                        ));
                                    } else if !text.trim().is_empty() {
                                        let _ = text_tx.send(format!("[device] {}", text.trim()));
                                    }
                                }
                                continue;
                            }
                            match byte {
                                b'\r' | b'\n' => flush_line(&mut line, &text_tx),
                                b'\t' => line.push(b' '),
                                other if other.is_ascii_graphic() || other == b' ' => {
                                    line.push(other)
                                }
                                _ => {}
                            }
                        }
                    }
                    Ok(_) => {}
                    Err(e)
                        if e.kind() == ErrorKind::TimedOut || e.kind() == ErrorKind::WouldBlock => {
                    }
                    Err(e) => {
                        let _ = text_tx.send(format!("[monitor] serial read failed: {}", e));
                        thread_finished.store(true, Ordering::SeqCst);
                        return;
                    }
                }
            }
        });

        Ok(Self {
            tx,
            rx,
            handle: Some(handle),
            finished,
        })
    }

    pub fn sender(&self) -> Sender<MonitorCommand> {
        self.tx.clone()
    }

    pub fn send_line(&self, line: &str) {
        let _ = self.tx.send(MonitorCommand::Line(line.to_string()));
    }

    pub fn trigger_upgrade(&self) {
        let _ = self.tx.send(MonitorCommand::TriggerUpgrade);
    }

    pub fn poll(&self) -> Vec<String> {
        self.rx.try_iter().collect()
    }

    pub fn is_finished(&self) -> bool {
        self.finished.load(Ordering::SeqCst)
    }

    pub fn close(&mut self) {
        let _ = self.tx.send(MonitorCommand::Close);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

impl Drop for UartMonitor {
    fn drop(&mut self) {
        self.close();
    }
}

fn resolve_monitor_port(path: &str) -> Result<String, String> {
    if !path.eq_ignore_ascii_case("auto") {
        return Ok(path.to_string());
    }
    let ports = UartDevice::list_ports()?;
    ports
        .iter()
        .find(|port| port.port_type == "usb")
        .or_else(|| ports.first())
        .map(|port| port.port_name.clone())
        .ok_or_else(|| "No serial port found".to_string())
}

/// CLI entry point for `uart-monitor`: mirror device output and forward stdin
/// lines to the device.
pub fn run_cli_monitor(path: &str, baud: u32, enter_upgrade: bool) -> Result<(), String> {
    let port_name = resolve_monitor_port(path)?;
    let monitor = UartMonitor::start(&port_name, baud)?;
    println!(
        "Monitoring '{}' at {} bps. Type a line and press Enter to send it to the device.",
        port_name, baud
    );
    println!("Press Ctrl+C to exit.");
    if enter_upgrade {
        println!("Requesting UART upgrade mode (aicupg gotobl / aicupg uart 0) ...");
        monitor.trigger_upgrade();
    }

    let sender = monitor.sender();
    thread::spawn(move || {
        let stdin = std::io::stdin();
        for line in stdin.lock().lines() {
            match line {
                Ok(line) => {
                    if sender.send(MonitorCommand::Line(line)).is_err() {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
        let _ = sender.send(MonitorCommand::Close);
    });

    loop {
        let mut printed = false;
        for line in monitor.poll() {
            println!("{}", line);
            printed = true;
        }
        if printed {
            let _ = std::io::stdout().flush();
        }
        if monitor.is_finished() {
            for line in monitor.poll() {
                println!("{}", line);
            }
            return Ok(());
        }
        thread::sleep(Duration::from_millis(20));
    }
}
