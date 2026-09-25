use std::io::{ErrorKind, Read, Write};
use std::time::{Duration, Instant};

use serialport::{DataBits, Parity, SerialPort, StopBits};

use crate::device::UpgDevice;
use crate::protocol::cbw_csw::*;
use crate::protocol::commands::CMD_SET_UART_ARGS;
use crate::transport::{CswPolicy, UpgTransport};

const SOH: u8 = 0x01;
const STX: u8 = 0x02;
const ACK: u8 = 0x06;
const DC1_SEND: u8 = 0x11;
const DC2_RECV: u8 = 0x12;
const NAK: u8 = 0x15;
const CAN: u8 = 0x18;
const SIG_A: u8 = 0x41;
const SIG_C: u8 = 0x43;

const LONG_FRAME_DATA: usize = 1024;
const SHORT_FRAME_DATA: usize = 176;
/// Device side `TRANS_DATA_BUFF_MAX_SIZE`: each direction switch below this
/// boundary corresponds to one transport task.
const DEVICE_SLICE: usize = 64 * 1024;

const MAX_FRAME_RETRIES: usize = 12;
const HANDSHAKE_INTERVAL: Duration = Duration::from_millis(200);
const PORT_POLL_SLICE: Duration = Duration::from_millis(50);

#[derive(Clone, Debug)]
pub struct UartOptions {
    pub baudrate: u32,
    /// Optional higher baudrate negotiated with `SET_UART_ARGS` after connect.
    pub max_baudrate: Option<u32>,
    pub connect_timeout: Duration,
    pub ack_timeout: Duration,
    pub data_timeout: Duration,
    /// When the UART upgrade protocol does not answer, try to enter upgrade
    /// mode by sending console commands and answering boot keywords.
    pub auto_enter: bool,
    /// How long to wait for the device to enter UART upgrade mode.
    pub enter_timeout: Duration,
}

impl Default for UartOptions {
    fn default() -> Self {
        Self {
            baudrate: 115200,
            max_baudrate: None,
            connect_timeout: Duration::from_secs(6),
            ack_timeout: Duration::from_secs(2),
            data_timeout: Duration::from_secs(30),
            auto_enter: true,
            enter_timeout: Duration::from_secs(20),
        }
    }
}

#[derive(Clone, Debug)]
pub struct SerialPortInfo {
    pub port_name: String,
    pub port_type: String,
    pub vid: Option<u16>,
    pub pid: Option<u16>,
    pub serial_number: Option<String>,
    pub manufacturer: Option<String>,
    pub product: Option<String>,
}

/// Layout of the UART protocol frame is defined by the bootloader:
/// - short: `SOH blk 255-blk len data(<=176) crc16-be`
/// - long:  `STX blk 255-blk data(==1024) crc16-be`
pub struct UartTransport {
    port: Box<dyn SerialPort>,
    port_name: String,
    baudrate: u32,
    options: UartOptions,
    send_blk: u8,
    recv_blk: u8,
    tag: u32,
}

impl UartTransport {
    pub fn open(path: &str, options: UartOptions) -> Result<Self, String> {
        let port = serialport::new(path, options.baudrate)
            .data_bits(DataBits::Eight)
            .parity(Parity::None)
            .stop_bits(StopBits::One)
            .timeout(PORT_POLL_SLICE)
            .open()
            .map_err(|e| format!("Failed to open serial port '{}': {}", path, e))?;

        Ok(Self::with_port(port, path, options))
    }

    fn with_port(port: Box<dyn SerialPort>, port_name: &str, options: UartOptions) -> Self {
        Self {
            port,
            port_name: port_name.to_string(),
            baudrate: options.baudrate,
            options,
            send_blk: 1,
            recv_blk: 0,
            tag: 1,
        }
    }

    pub fn port_name(&self) -> &str {
        &self.port_name
    }

    pub fn baudrate(&self) -> u32 {
        self.baudrate
    }

    /// Wait for the bootloader to announce itself and answer our `SIG_C` poll.
    pub fn handshake(&mut self) -> Result<(), String> {
        self.handshake_with_timeout(self.options.connect_timeout)
    }

    pub fn handshake_with_timeout(&mut self, timeout: Duration) -> Result<(), String> {
        self.flush_input(Duration::from_millis(100));
        let deadline = Instant::now() + timeout;
        let mut last_probe = Instant::now() - HANDSHAKE_INTERVAL;

        while Instant::now() < deadline {
            if last_probe.elapsed() >= HANDSHAKE_INTERVAL {
                self.write_raw(&[SIG_C])?;
                last_probe = Instant::now();
            }
            match self.read_byte(PORT_POLL_SLICE) {
                Ok(ACK) => return Ok(()),
                Ok(SIG_A) => {
                    let _ = self.write_raw(&[ACK]);
                }
                Ok(CAN) | Ok(_) => {}
                Err(_) => {}
            }
        }
        Err(format!(
            "UART device did not answer on '{}' within {:?}",
            self.port_name, timeout
        ))
    }

    /// Try to bring an already-running board into UART upgrade mode.
    ///
    /// Strategy:
    /// 1. Send console commands for the application shell (`aicupg gotobl`)
    ///    and for the bootloader shell (`aicupg uart 0`).
    /// 2. Poll the protocol handshake while answering `AIBURNFORCE` /
    ///    `AIBURNID` boot keywords with ACK, so a board that is power-cycled
    ///    or reset during the wait can also enter upgrade mode.
    pub fn enter_upgrade(&mut self, timeout: Duration) -> Result<(), String> {
        eprintln!(
            "No UART upgrade protocol yet; trying to enter upgrade mode on '{}' ...",
            self.port_name
        );
        self.flush_input(Duration::from_millis(150));
        let _ = self.write_raw(b"\r\n");
        self.capture_console(Duration::from_millis(250));
        eprintln!("  >> sending application console command: aicupg gotobl");
        let _ = self.write_raw(b"aicupg gotobl\r");
        self.capture_console(Duration::from_millis(900));
        eprintln!("  >> sending bootloader console command: aicupg uart 0");
        let _ = self.write_raw(b"aicupg uart 0\r");
        self.capture_console(Duration::from_millis(900));
        eprintln!(
            "  >> waiting up to {:?} for upgrade mode (reset the board now if it does not reboot)",
            timeout
        );
        self.wait_protocol(timeout)
    }

    fn wait_protocol(&mut self, timeout: Duration) -> Result<(), String> {
        let deadline = Instant::now() + timeout;
        let mut last_probe = Instant::now() - HANDSHAKE_INTERVAL;
        let mut line = String::new();

        while Instant::now() < deadline {
            if last_probe.elapsed() >= HANDSHAKE_INTERVAL {
                let _ = self.write_raw(&[SIG_C]);
                last_probe = Instant::now();
            }
            match self.read_byte(PORT_POLL_SLICE) {
                Ok(ACK) => {
                    self.flush_console_line(&mut line);
                    return Ok(());
                }
                Ok(SIG_A) => {
                    let _ = self.write_raw(&[ACK]);
                }
                Ok(0x16) => {
                    self.answer_boot_keyword();
                }
                Ok(b'\r') => {}
                Ok(b'\n') => self.flush_console_line(&mut line),
                Ok(byte) => {
                    push_console_char(&mut line, byte);
                    if line.len() >= 160 {
                        self.flush_console_line(&mut line);
                    }
                }
                Err(_) => {}
            }
        }
        Err(format!(
            "Device on '{}' did not enter UART upgrade mode within {:?}. \
             Make sure this is the console UART (115200 8N1), run `aicupg gotobl` \
             on the device, or reset the board while this tool waits",
            self.port_name, timeout
        ))
    }

    /// Answer an `AIBURNFORCE\n` / `AIBURNID\n` boot keyword so the bootloader
    /// stays in upgrade mode.
    fn answer_boot_keyword(&mut self) {
        let mut keyword = Vec::new();
        while keyword.len() < 32 {
            match self.read_byte(Duration::from_millis(30)) {
                Ok(byte) => {
                    keyword.push(byte);
                    if byte == b'\n' {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
        let text = String::from_utf8_lossy(&keyword);
        if text.contains("AIBURN") {
            eprintln!("  << device requested '{}'; replying ACK", text.trim());
            let _ = self.write_raw(&[ACK]);
        } else if !text.trim().is_empty() {
            eprintln!("  << {}", text.trim());
        }
    }

    fn flush_console_line(&mut self, line: &mut String) {
        let text = line.trim();
        if !text.is_empty() {
            eprintln!("  << {}", text);
        }
        line.clear();
    }

    fn not_answered_error(&self, timeout: Duration) -> String {
        format!(
            "UART device did not answer on '{}' within {:?}. Check that this is the \
             console UART (115200 8N1), then either run `aicupg gotobl` on the device, \
             reset the board while this tool waits, or allow auto-enter mode",
            self.port_name, timeout
        )
    }

    fn capture_console(&mut self, duration: Duration) {
        let deadline = Instant::now() + duration;
        let mut line = String::new();
        while Instant::now() < deadline {
            match self.read_byte(Duration::from_millis(10)) {
                Ok(0x16) => self.answer_boot_keyword(),
                Ok(b'\r') => {}
                Ok(b'\n') => self.flush_console_line(&mut line),
                Ok(byte) => push_console_char(&mut line, byte),
                Err(_) => {}
            }
        }
        self.flush_console_line(&mut line);
    }

    fn reconnect_inner(&mut self, timeout: Duration) -> Result<(), String> {
        self.flush_input(Duration::from_millis(100));
        eprintln!(
            "Waiting for UART device to reconnect on '{}' ...",
            self.port_name
        );
        let deadline = Instant::now() + timeout;
        let mut last_probe = Instant::now() - HANDSHAKE_INTERVAL;
        while Instant::now() < deadline {
            if last_probe.elapsed() >= HANDSHAKE_INTERVAL {
                self.write_raw(&[SIG_C])?;
                last_probe = Instant::now();
            }
            match self.read_byte(PORT_POLL_SLICE) {
                Ok(ACK) => {
                    eprintln!("UART device reconnected on '{}'.", self.port_name);
                    return Ok(());
                }
                Ok(SIG_A) => {
                    let _ = self.write_raw(&[ACK]);
                }
                Ok(_) | Err(_) => {}
            }
        }
        Err(format!(
            "Timed out waiting for UART device reconnect on '{}'",
            self.port_name
        ))
    }

    fn next_tag(&mut self) -> u32 {
        let tag = self.tag;
        self.tag = self.tag.wrapping_add(1).max(1);
        tag
    }

    fn configure_port_baudrate(&mut self, baudrate: u32) -> Result<(), String> {
        self.port.set_baud_rate(baudrate).map_err(|e| {
            format!(
                "Failed to switch '{}' to {} bps: {}",
                self.port_name, baudrate, e
            )
        })?;
        let _ = self.port.clear(serialport::ClearBuffer::All);
        self.baudrate = baudrate;
        Ok(())
    }

    /// Perform `SET_UART_ARGS` and follow the device to the new baudrate.
    ///
    /// The device applies the new baudrate right before it transmits the CSW
    /// of the response, so the host must switch between the RESP data read
    /// and the CSW read.
    fn set_uart_args(&mut self, baudrate: u32) -> Result<(), String> {
        let cmd = CmdHeader::new(CMD_SET_UART_ARGS, 4);
        self.write_txn(cmd.to_bytes(), CswPolicy::Required)?;
        self.write_txn(&baudrate.to_le_bytes(), CswPolicy::Required)?;

        let tag = self.next_tag();
        self.send_buffer(AicCbw::new_read(tag, RESP_MIN_HDR_LEN as u32).to_bytes())?;
        let _resp = self.recv_buffer(RESP_MIN_HDR_LEN)?;

        // Give the bootloader a moment to reconfigure its UART, then follow it
        // before requesting the CSW.
        std::thread::sleep(Duration::from_millis(20));
        self.configure_port_baudrate(baudrate)?;
        std::thread::sleep(Duration::from_millis(20));
        let _csw = self.read_csw_for(tag, CswPolicy::Required)?;
        eprintln!("UART baudrate switched to {} bps", baudrate);
        Ok(())
    }

    // ── Raw serial helpers ─────────────────────────────────────────────

    fn write_raw(&mut self, data: &[u8]) -> Result<(), String> {
        self.port
            .write_all(data)
            .map_err(|e| format!("UART write failed on '{}': {}", self.port_name, e))?;
        let _ = self.port.flush();
        Ok(())
    }

    fn flush_input(&mut self, timeout: Duration) {
        let deadline = Instant::now() + timeout;
        let mut buf = [0u8; 256];
        while Instant::now() < deadline {
            match self.port.read(&mut buf) {
                Ok(0) => break,
                Ok(_) => continue,
                Err(e) if e.kind() == ErrorKind::TimedOut || e.kind() == ErrorKind::WouldBlock => {
                    break
                }
                Err(_) => break,
            }
        }
    }

    fn read_byte(&mut self, timeout: Duration) -> Result<u8, String> {
        self.read_byte_until(Instant::now() + timeout)
    }

    fn read_byte_until(&mut self, deadline: Instant) -> Result<u8, String> {
        let mut byte = [0u8; 1];
        loop {
            match self.port.read(&mut byte) {
                Ok(1) => return Ok(byte[0]),
                Ok(_) => {}
                Err(e) if e.kind() == ErrorKind::TimedOut || e.kind() == ErrorKind::WouldBlock => {
                    if Instant::now() >= deadline {
                        return Err("serial read timed out".to_string());
                    }
                }
                Err(e) if e.kind() == ErrorKind::Interrupted => {}
                Err(e) => return Err(format!("UART read failed on '{}': {}", self.port_name, e)),
            }
        }
    }

    fn read_exact_until(&mut self, len: usize, deadline: Instant) -> Result<Vec<u8>, String> {
        let mut out = Vec::with_capacity(len);
        while out.len() < len {
            out.push(self.read_byte_until(deadline)?);
        }
        Ok(out)
    }

    // ── Framing ────────────────────────────────────────────────────────

    fn switch_to_send(&mut self) -> Result<(), String> {
        self.switch_direction(DC1_SEND, "send")
    }

    fn switch_to_recv(&mut self) -> Result<(), String> {
        self.switch_direction(DC2_RECV, "recv")
    }

    fn switch_direction(&mut self, command: u8, label: &str) -> Result<(), String> {
        let mut last_err = String::new();
        for _ in 0..MAX_FRAME_RETRIES {
            self.write_raw(&[command])?;
            match self.read_byte(self.options.ack_timeout) {
                Ok(ACK) => return Ok(()),
                Ok(other) => {
                    last_err = format!(
                        "unexpected 0x{:02x} while switching to {} mode",
                        other, label
                    )
                }
                Err(e) => {
                    last_err = e;
                }
            }
        }
        Err(format!(
            "Failed to switch UART to {} mode: {}",
            label, last_err
        ))
    }

    fn send_frame(&mut self, data: &[u8]) -> Result<(), String> {
        let frame = pack_frame(self.send_blk, data);
        let mut last_err = String::new();
        for _ in 0..MAX_FRAME_RETRIES {
            self.write_raw(&frame)?;
            match self.read_byte(self.options.ack_timeout) {
                Ok(ACK) => {
                    self.send_blk = self.send_blk.wrapping_add(1);
                    return Ok(());
                }
                Ok(NAK) => last_err = "device replied NAK".to_string(),
                Ok(SIG_A) => {
                    let _ = self.write_raw(&[ACK]);
                    last_err = "device announced SIG_A".to_string();
                }
                Ok(other) => last_err = format!("unexpected 0x{:02x} while waiting for ACK", other),
                Err(e) => last_err = e,
            }
        }
        Err(format!(
            "Frame not acknowledged after {} tries: {}",
            MAX_FRAME_RETRIES, last_err
        ))
    }

    fn send_buffer(&mut self, data: &[u8]) -> Result<(), String> {
        if data.is_empty() {
            return Ok(());
        }
        self.switch_to_send()?;
        for slice in frame_slices(data) {
            self.send_frame(slice)?;
        }
        Ok(())
    }

    fn recv_frame(&mut self) -> Result<Vec<u8>, String> {
        let deadline = Instant::now() + self.options.data_timeout;
        loop {
            let first = match self.read_byte_until(deadline) {
                Ok(byte) => byte,
                Err(e) => {
                    let _ = self.write_raw(&[NAK]);
                    return Err(format!(
                        "Timed out waiting for a frame on '{}': {}",
                        self.port_name, e
                    ));
                }
            };
            match first {
                SOH | STX => {}
                ACK | NAK | CAN | SIG_A => continue,
                other => {
                    eprintln!("  << UART: ignoring unexpected byte 0x{:02x}", other);
                    continue;
                }
            }

            let is_long = first == STX;
            let header = match self.read_exact_until(if is_long { 2 } else { 3 }, deadline) {
                Ok(header) => header,
                Err(e) => {
                    let _ = self.write_raw(&[NAK]);
                    return Err(e);
                }
            };
            let blk = header[0];
            if blk != 255 - header[1] {
                eprintln!(
                    "  << UART: bad block complement (blk={}, inv={})",
                    blk, header[1]
                );
                let _ = self.write_raw(&[NAK]);
                continue;
            }
            let data_len = if is_long {
                LONG_FRAME_DATA
            } else {
                header[2] as usize
            };
            if data_len > SHORT_FRAME_DATA && !is_long {
                let _ = self.write_raw(&[NAK]);
                continue;
            }

            let body = match self.read_exact_until(data_len + 2, deadline) {
                Ok(body) => body,
                Err(e) => {
                    let _ = self.write_raw(&[NAK]);
                    return Err(e);
                }
            };
            let crc_expected = u16::from_be_bytes([body[data_len], body[data_len + 1]]);
            let crc_actual = crc16_ccitt(&body[..data_len]);
            if crc_actual != crc_expected {
                eprintln!(
                    "  << UART: CRC16 mismatch (0x{:04x} != 0x{:04x}), requesting retransmit",
                    crc_actual, crc_expected
                );
                let _ = self.write_raw(&[NAK]);
                continue;
            }

            self.write_raw(&[ACK])?;
            self.recv_blk = blk;
            return Ok(body[..data_len].to_vec());
        }
    }

    fn recv_buffer(&mut self, len: usize) -> Result<Vec<u8>, String> {
        if len == 0 {
            return Ok(Vec::new());
        }
        self.switch_to_recv()?;
        let mut out = Vec::with_capacity(len);
        let mut switched_at = 0usize;
        while out.len() < len {
            if out.len() - switched_at >= DEVICE_SLICE {
                self.switch_to_recv()?;
                switched_at = out.len();
            }
            let chunk = self.recv_frame()?;
            if out.len() + chunk.len() > len {
                return Err(format!(
                    "Device sent more data than expected ({} + {} > {})",
                    out.len(),
                    chunk.len(),
                    len
                ));
            }
            out.extend_from_slice(&chunk);
        }
        Ok(out)
    }

    fn read_csw_for(&mut self, tag: u32, policy: CswPolicy) -> Result<Option<AicCsw>, String> {
        let bytes = match self.recv_buffer(13) {
            Ok(bytes) => bytes,
            Err(e) if policy == CswPolicy::AllowMissing => {
                eprintln!(
                    "  << UART CSW missing accepted by transaction policy: {}",
                    e
                );
                return Ok(None);
            }
            Err(e) => return Err(e),
        };
        let csw =
            AicCsw::from_bytes(&bytes).ok_or_else(|| "Failed to parse UART CSW".to_string())?;
        if csw.tag_val() != tag {
            return Err(format!(
                "UART CSW tag mismatch: got {}, expected {}",
                csw.tag_val(),
                tag
            ));
        }
        if !csw.is_ok() {
            return Err(format!(
                "UART CSW failed: status={} residue={}",
                csw.status_val(),
                csw.data_residue_val()
            ));
        }
        Ok(Some(csw))
    }
}

impl UpgTransport for UartTransport {
    fn write_txn(&mut self, payload: &[u8], policy: CswPolicy) -> Result<Option<AicCsw>, String> {
        let tag = self.next_tag();
        self.send_buffer(AicCbw::new_write(tag, payload.len() as u32).to_bytes())?;
        if !payload.is_empty() {
            self.send_buffer(payload)?;
        }
        self.read_csw_for(tag, policy)
    }

    fn read_txn(&mut self, read_len: u32, policy: CswPolicy) -> Result<Vec<u8>, String> {
        let tag = self.next_tag();
        self.send_buffer(AicCbw::new_read(tag, read_len).to_bytes())?;
        let data = match self.recv_buffer(read_len as usize) {
            Ok(data) => data,
            Err(e) if policy == CswPolicy::AllowMissing => {
                eprintln!(
                    "  << UART data missing accepted by transaction policy: {}",
                    e
                );
                return Ok(Vec::new());
            }
            Err(e) => return Err(e),
        };
        let csw = self.read_csw_for(tag, policy)?;
        if csw.is_none() && policy == CswPolicy::Required {
            return Err("UART CSW unexpectedly missing".to_string());
        }
        Ok(data)
    }

    fn reconnect(&mut self, timeout: Duration) -> Result<(), String> {
        self.reconnect_inner(timeout)
    }

    fn drain_rx(&mut self, timeout: Duration, _max_bytes: usize) -> Result<(), String> {
        self.flush_input(timeout);
        Ok(())
    }

    fn transport_name(&self) -> &'static str {
        "UART"
    }

    fn max_write_chunk(&self, block_size: u32) -> usize {
        // Keep each write transaction inside one device transport buffer and
        // well below the 64 KiB slice boundary.
        (block_size as usize)
            .saturating_mul(16)
            .clamp(4096, 64 * 1024)
    }

    fn set_uart_baudrate(&mut self, baudrate: u32) -> Result<(), String> {
        if baudrate == 0 {
            return Ok(());
        }
        if baudrate == self.baudrate {
            return Ok(());
        }
        self.set_uart_args(baudrate)
    }
}

impl UpgDevice<UartTransport> {
    pub fn list_ports() -> Result<Vec<SerialPortInfo>, String> {
        let ports = serialport::available_ports()
            .map_err(|e| format!("Failed to enumerate serial ports: {}", e))?;
        let mut infos = Vec::new();
        for port in ports {
            if cfg!(target_os = "macos") && port.port_name.starts_with("/dev/tty.") {
                continue;
            }
            let (port_type, vid, pid, serial_number, manufacturer, product) = match &port.port_type
            {
                serialport::SerialPortType::UsbPort(info) => (
                    "usb".to_string(),
                    Some(info.vid),
                    Some(info.pid),
                    info.serial_number.clone(),
                    info.manufacturer.clone(),
                    info.product.clone(),
                ),
                serialport::SerialPortType::PciPort => {
                    ("pci".to_string(), None, None, None, None, None)
                }
                serialport::SerialPortType::BluetoothPort => {
                    ("bluetooth".to_string(), None, None, None, None, None)
                }
                serialport::SerialPortType::Unknown => {
                    ("unknown".to_string(), None, None, None, None, None)
                }
            };
            infos.push(SerialPortInfo {
                port_name: port.port_name,
                port_type,
                vid,
                pid,
                serial_number,
                manufacturer,
                product,
            });
        }
        Ok(infos)
    }

    /// Open a specific serial port, handshake and verify with `GET_HWINFO`.
    ///
    /// With `auto_enter` enabled the tool first probes briefly, then tries to
    /// trigger UART upgrade mode on the device before waiting for the protocol.
    pub fn open_port(path: &str, options: UartOptions) -> Result<Self, String> {
        let transport = UartTransport::open(path, options.clone())?;
        let mut device = Self::new(transport);
        eprintln!(
            "Connecting to ArtInChip UART device on '{}' at {} bps ...",
            path, options.baudrate
        );
        let probe_timeout = if options.auto_enter {
            Duration::from_millis(1500)
        } else {
            options.connect_timeout
        };
        if device
            .transport_mut()
            .handshake_with_timeout(probe_timeout)
            .is_err()
        {
            if !options.auto_enter {
                return Err(device
                    .transport_mut()
                    .not_answered_error(options.connect_timeout));
            }
            device
                .transport_mut()
                .enter_upgrade(options.enter_timeout)?;
        }
        device.get_hwinfo()?;
        eprintln!("UART device connected on '{}'.", path);
        if let Some(speed) = options.max_baudrate {
            if speed > options.baudrate {
                device.set_max_baudrate(speed)?;
            }
        }
        Ok(device)
    }

    /// Probe every serial port for an ArtInChip bootloader and return the first
    /// responsive device.
    ///
    /// Auto-probing never sends console commands to arbitrary ports. If no
    /// device answers and `auto_enter` is enabled, the trigger is only used
    /// when exactly one USB serial port is present.
    pub fn open_auto(options: UartOptions) -> Result<Self, String> {
        let ports = Self::list_ports()?;
        if ports.is_empty() {
            return Err("No serial ports found".to_string());
        }
        let mut last_err = String::new();
        for port in &ports {
            let mut probe = options.clone();
            probe.connect_timeout = Duration::from_millis(1500);
            probe.auto_enter = false;
            let probe_port = match UartTransport::open(&port.port_name, probe.clone()) {
                Ok(transport) => transport,
                Err(e) => {
                    last_err = e;
                    continue;
                }
            };
            let mut device = Self::new(probe_port);
            eprintln!("Probing '{}' ...", port.port_name);
            if device
                .transport_mut()
                .handshake_with_timeout(probe.connect_timeout)
                .is_err()
            {
                continue;
            }
            if device.get_hwinfo().is_err() {
                continue;
            }
            eprintln!("ArtInChip UART device found on '{}'.", port.port_name);
            if let Some(speed) = options.max_baudrate {
                if speed > options.baudrate {
                    device.set_max_baudrate(speed)?;
                }
            }
            return Ok(device);
        }

        if options.auto_enter {
            let candidates = ports
                .iter()
                .filter(|port| port.port_type == "usb")
                .collect::<Vec<_>>();
            if candidates.len() == 1 {
                eprintln!(
                    "Trying to enter upgrade mode on the only USB serial port '{}' ...",
                    candidates[0].port_name
                );
                let mut retry = options.clone();
                retry.connect_timeout = Duration::from_millis(1500);
                return Self::open_port(&candidates[0].port_name, retry);
            }
            if candidates.len() > 1 {
                return Err(
                    "No UART device answered and multiple USB serial ports are present; \
                     specify the port explicitly with `--uart <PORT>`"
                        .to_string(),
                );
            }
        }

        Err(format!(
            "No ArtInChip UART device responded (last error: {})",
            last_err
        ))
    }

    /// Negotiate a higher UART baudrate with the bootloader (`SET_UART_ARGS`).
    pub fn set_max_baudrate(&mut self, baudrate: u32) -> Result<(), String> {
        self.transport_mut().set_uart_baudrate(baudrate)?;
        if let Err(e) = self.get_hwinfo() {
            eprintln!("Warning: device probe after baudrate switch failed: {}", e);
        }
        Ok(())
    }
}

/// Accumulate console text while dropping runs of echoed `SIG_C` probe bytes.
fn push_console_char(line: &mut String, byte: u8) {
    let probe_echo = line.bytes().all(|existing| existing == SIG_C);
    if probe_echo {
        if byte == SIG_C {
            if line.len() < 32 {
                line.push('C');
            }
            return;
        }
        line.clear();
    }
    if byte.is_ascii_graphic() || byte == b' ' {
        line.push(byte as char);
    }
}

pub(crate) fn frame_slices(data: &[u8]) -> Vec<&[u8]> {
    let mut slices = Vec::new();
    let mut offset = 0;
    while offset < data.len() {
        let remaining = data.len() - offset;
        let len = if remaining >= LONG_FRAME_DATA {
            LONG_FRAME_DATA
        } else {
            remaining.min(SHORT_FRAME_DATA)
        };
        slices.push(&data[offset..offset + len]);
        offset += len;
    }
    slices
}

pub(crate) fn pack_frame(blk: u8, data: &[u8]) -> Vec<u8> {
    let mut frame = Vec::with_capacity(data.len() + 6);
    if data.len() == LONG_FRAME_DATA {
        frame.push(STX);
        frame.push(blk);
        frame.push(!blk);
        frame.extend_from_slice(data);
    } else {
        frame.push(SOH);
        frame.push(blk);
        frame.push(!blk);
        frame.push(data.len() as u8);
        frame.extend_from_slice(data);
    }
    let crc = crc16_ccitt(data);
    frame.push((crc >> 8) as u8);
    frame.push((crc & 0xff) as u8);
    frame
}

pub(crate) fn crc16_ccitt(data: &[u8]) -> u16 {
    let mut crc: u16 = 0;
    for &byte in data {
        crc ^= (byte as u16) << 8;
        for _ in 0..8 {
            if crc & 0x8000 != 0 {
                crc = (crc << 1) ^ 0x1021;
            } else {
                crc <<= 1;
            }
        }
    }
    crc
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc16_matches_xmodem_reference() {
        assert_eq!(crc16_ccitt(b"123456789"), 0x31C3);
        assert_eq!(crc16_ccitt(b""), 0x0000);
    }

    #[test]
    fn console_helper_drops_probe_echo_run() {
        let mut line = String::new();
        push_console_char(&mut line, b'C');
        push_console_char(&mut line, b'C');
        assert_eq!(line, "CC");
        push_console_char(&mut line, b'h');
        assert_eq!(line, "h");
        push_console_char(&mut line, b'i');
        assert_eq!(line, "hi");
    }

    #[test]
    fn packs_short_frame_with_big_endian_crc() {
        let frame = pack_frame(3, &[0xAA, 0xBB]);
        let crc = crc16_ccitt(&[0xAA, 0xBB]);
        assert_eq!(frame[0], SOH);
        assert_eq!(frame[1], 3);
        assert_eq!(frame[2], 255 - 3);
        assert_eq!(frame[3], 2);
        assert_eq!(&frame[4..6], &[0xAA, 0xBB]);
        assert_eq!(frame[6], (crc >> 8) as u8);
        assert_eq!(frame[7], (crc & 0xFF) as u8);
    }

    #[test]
    fn packs_long_frame_for_full_block() {
        let data = vec![0x5A; LONG_FRAME_DATA];
        let frame = pack_frame(9, &data);
        assert_eq!(frame[0], STX);
        assert_eq!(frame[1], 9);
        assert_eq!(frame[2], 255 - 9);
        assert_eq!(frame.len(), LONG_FRAME_DATA + 5);
        assert_eq!(&frame[3..3 + LONG_FRAME_DATA], &data[..]);
    }

    #[test]
    fn frame_slices_use_long_frames_then_short_ones() {
        let data = vec![0u8; 2000];
        let slices = frame_slices(&data);
        assert_eq!(
            slices.iter().map(|s| s.len()).collect::<Vec<_>>(),
            vec![1024, 176, 176, 176, 176, 176, 96]
        );
        assert_eq!(slices.iter().map(|s| s.len()).sum::<usize>(), 2000);

        let exact = vec![0u8; LONG_FRAME_DATA];
        assert_eq!(
            frame_slices(&exact)
                .iter()
                .map(|s| s.len())
                .collect::<Vec<_>>(),
            vec![1024]
        );

        let tiny = [0u8; 1];
        assert_eq!(frame_slices(&tiny).len(), 1);
        assert!(frame_slices(&[]).is_empty());
    }
}

#[cfg(test)]
mod sim_tests {
    use super::*;

    use std::collections::VecDeque;
    use std::io;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};
    use std::thread;

    use serialport::{ClearBuffer, DataBits, FlowControl, SerialPort, StopBits};

    struct Pipe {
        to_dev: Mutex<VecDeque<u8>>,
        to_host: Mutex<VecDeque<u8>>,
        stop: AtomicBool,
    }

    impl Pipe {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                to_dev: Mutex::new(VecDeque::new()),
                to_host: Mutex::new(VecDeque::new()),
                stop: AtomicBool::new(false),
            })
        }

        fn push_to_host(&self, data: &[u8]) {
            self.to_host.lock().unwrap().extend(data);
        }

        fn pop_to_dev(&self, timeout: Duration) -> Option<u8> {
            let deadline = Instant::now() + timeout;
            loop {
                if let Some(byte) = self.to_dev.lock().unwrap().pop_front() {
                    return Some(byte);
                }
                if self.stop.load(Ordering::SeqCst) || Instant::now() >= deadline {
                    return None;
                }
                thread::sleep(Duration::from_millis(1));
            }
        }
    }

    struct MockPort {
        pipe: Arc<Pipe>,
    }

    impl io::Read for MockPort {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            let mut queue = self.pipe.to_host.lock().unwrap();
            if queue.is_empty() {
                return Err(io::Error::new(io::ErrorKind::TimedOut, "mock empty"));
            }
            let len = buf.len().min(queue.len());
            for slot in buf.iter_mut().take(len) {
                *slot = queue.pop_front().unwrap();
            }
            Ok(len)
        }
    }

    impl io::Write for MockPort {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.pipe.to_dev.lock().unwrap().extend(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    impl SerialPort for MockPort {
        fn name(&self) -> Option<String> {
            Some("mock".to_string())
        }

        fn baud_rate(&self) -> serialport::Result<u32> {
            Ok(115200)
        }

        fn data_bits(&self) -> serialport::Result<DataBits> {
            Ok(DataBits::Eight)
        }

        fn flow_control(&self) -> serialport::Result<FlowControl> {
            Ok(FlowControl::None)
        }

        fn parity(&self) -> serialport::Result<Parity> {
            Ok(Parity::None)
        }

        fn stop_bits(&self) -> serialport::Result<StopBits> {
            Ok(StopBits::One)
        }

        fn timeout(&self) -> Duration {
            PORT_POLL_SLICE
        }

        fn set_baud_rate(&mut self, _baud_rate: u32) -> serialport::Result<()> {
            Ok(())
        }

        fn set_data_bits(&mut self, _data_bits: DataBits) -> serialport::Result<()> {
            Ok(())
        }

        fn set_flow_control(&mut self, _flow_control: FlowControl) -> serialport::Result<()> {
            Ok(())
        }

        fn set_parity(&mut self, _parity: Parity) -> serialport::Result<()> {
            Ok(())
        }

        fn set_stop_bits(&mut self, _stop_bits: StopBits) -> serialport::Result<()> {
            Ok(())
        }

        fn set_timeout(&mut self, _timeout: Duration) -> serialport::Result<()> {
            Ok(())
        }

        fn write_request_to_send(&mut self, _level: bool) -> serialport::Result<()> {
            Ok(())
        }

        fn write_data_terminal_ready(&mut self, _level: bool) -> serialport::Result<()> {
            Ok(())
        }

        fn read_clear_to_send(&mut self) -> serialport::Result<bool> {
            Ok(true)
        }

        fn read_data_set_ready(&mut self) -> serialport::Result<bool> {
            Ok(true)
        }

        fn read_ring_indicator(&mut self) -> serialport::Result<bool> {
            Ok(false)
        }

        fn read_carrier_detect(&mut self) -> serialport::Result<bool> {
            Ok(true)
        }

        fn bytes_to_read(&self) -> serialport::Result<u32> {
            Ok(self.pipe.to_host.lock().unwrap().len() as u32)
        }

        fn bytes_to_write(&self) -> serialport::Result<u32> {
            Ok(self.pipe.to_dev.lock().unwrap().len() as u32)
        }

        fn clear(&self, _buffer_to_clear: ClearBuffer) -> serialport::Result<()> {
            Ok(())
        }

        fn try_clone(&self) -> serialport::Result<Box<dyn SerialPort>> {
            Ok(Box::new(MockPort {
                pipe: self.pipe.clone(),
            }))
        }

        fn set_break(&self) -> serialport::Result<()> {
            Ok(())
        }

        fn clear_break(&self) -> serialport::Result<()> {
            Ok(())
        }
    }

    /// Minimal bootloader-side model of `uart_proto_layer.c`.
    fn run_device(pipe: Arc<Pipe>) {
        pipe.push_to_host(&[CAN]);
        let mut recv_blk: u8 = 0;
        let mut send_blk: u8 = 0;

        loop {
            if pipe.stop.load(Ordering::SeqCst) {
                return;
            }
            let Some(byte) = pipe.pop_to_dev(Duration::from_millis(50)) else {
                continue;
            };
            match byte {
                SIG_C => pipe.push_to_host(&[ACK]),
                DC1_SEND => {
                    pipe.push_to_host(&[ACK]);
                    let Some(cbw) = dev_read_frame(&pipe, &mut recv_blk) else {
                        continue;
                    };
                    if cbw.len() != 31 {
                        continue;
                    }
                    let flags = cbw[12];
                    let payload_len = u32::from_le_bytes(cbw[8..12].try_into().unwrap()) as usize;
                    if flags == 0 {
                        let mut payload = Vec::with_capacity(payload_len);
                        while payload.len() < payload_len {
                            let Some(frame) = dev_read_frame(&pipe, &mut recv_blk) else {
                                continue;
                            };
                            payload.extend_from_slice(&frame);
                        }
                        dev_wait_switch(&pipe, DC2_RECV);
                        dev_send_csw(&pipe, &mut send_blk, &cbw);
                    } else {
                        let data = vec![0x5Au8; payload_len];
                        dev_wait_switch(&pipe, DC2_RECV);
                        for slice in frame_slices(&data) {
                            dev_send_frame(&pipe, &mut send_blk, slice);
                        }
                        dev_wait_switch(&pipe, DC2_RECV);
                        dev_send_csw(&pipe, &mut send_blk, &cbw);
                    }
                }
                _ => {}
            }
        }
    }

    fn dev_read_frame(pipe: &Pipe, recv_blk: &mut u8) -> Option<Vec<u8>> {
        loop {
            let first = pipe.pop_to_dev(Duration::from_secs(2))?;
            let (is_long, header_len) = match first {
                SOH => (false, 3usize),
                STX => (true, 2usize),
                DC1_SEND | DC2_RECV => {
                    // Direction switch commands are acknowledged like the
                    // bootloader's CMD_RECV state does.
                    pipe.push_to_host(&[ACK]);
                    continue;
                }
                _ => continue,
            };
            let mut header = Vec::with_capacity(header_len);
            for _ in 0..header_len {
                header.push(pipe.pop_to_dev(Duration::from_secs(2))?);
            }
            let blk = header[0];
            if blk != 255 - header[1] {
                pipe.push_to_host(&[NAK]);
                continue;
            }
            let data_len = if is_long {
                LONG_FRAME_DATA
            } else {
                header[2] as usize
            };
            let mut body = Vec::with_capacity(data_len + 2);
            for _ in 0..data_len + 2 {
                body.push(pipe.pop_to_dev(Duration::from_secs(2))?);
            }
            let crc_expected = u16::from_be_bytes([body[data_len], body[data_len + 1]]);
            if crc16_ccitt(&body[..data_len]) != crc_expected {
                pipe.push_to_host(&[NAK]);
                continue;
            }
            if *recv_blk != 0 && blk != recv_blk.wrapping_add(1) && *recv_blk != blk {
                pipe.push_to_host(&[NAK]);
                continue;
            }
            *recv_blk = blk;
            pipe.push_to_host(&[ACK]);
            return Some(body[..data_len].to_vec());
        }
    }

    fn dev_send_frame(pipe: &Pipe, send_blk: &mut u8, data: &[u8]) {
        let frame = pack_frame(*send_blk, data);
        loop {
            pipe.push_to_host(&frame);
            match pipe.pop_to_dev(Duration::from_secs(2)) {
                Some(ACK) => {
                    *send_blk = send_blk.wrapping_add(1);
                    return;
                }
                Some(NAK) => continue,
                _ => return,
            }
        }
    }

    fn dev_send_csw(pipe: &Pipe, send_blk: &mut u8, cbw: &[u8]) {
        let mut csw = [0u8; 13];
        csw[0..4].copy_from_slice(&AIC_USB_SIGN_USBS.to_le_bytes());
        csw[4..8].copy_from_slice(&cbw[4..8]);
        dev_send_frame(pipe, send_blk, &csw);
    }

    fn dev_wait_switch(pipe: &Pipe, expected: u8) {
        loop {
            match pipe.pop_to_dev(Duration::from_secs(2)) {
                Some(byte) if byte == expected => {
                    pipe.push_to_host(&[ACK]);
                    return;
                }
                Some(_) => continue,
                None => return,
            }
        }
    }

    /// Device model that behaves like an application console until it sees an
    /// `aicupg ...` console command, then reboots into upgrade mode.
    fn run_device_console_entry(pipe: Arc<Pipe>) {
        let mut line = Vec::<u8>::new();
        loop {
            if pipe.stop.load(Ordering::SeqCst) {
                return;
            }
            let Some(byte) = pipe.pop_to_dev(Duration::from_millis(50)) else {
                continue;
            };
            if byte == b'\r' || byte == b'\n' {
                let text = String::from_utf8_lossy(&line).to_string();
                line.clear();
                if text.contains("aicupg gotobl") || text.contains("aicupg uart 0") {
                    pipe.push_to_host(b"\r\naicupg: rebooting into upgrade mode\r\n");
                    thread::sleep(Duration::from_millis(100));
                    run_device(pipe);
                    return;
                }
            } else {
                line.push(byte);
            }
        }
    }

    /// Device model that emits an `AIBURNFORCE` boot keyword and only enters
    /// upgrade mode after the host acknowledges it.
    fn run_device_keyword_entry(pipe: Arc<Pipe>) {
        let keyword = [
            0x16, b'A', b'I', b'B', b'U', b'R', b'N', b'F', b'O', b'R', b'C', b'E', b'\n',
        ];
        let deadline = Instant::now() + Duration::from_secs(3);
        while Instant::now() < deadline {
            // The real bootloader sends this once per boot; repeat until the
            // host acknowledges so the test is not affected by flush races.
            pipe.push_to_host(&keyword);
            let wait_until = Instant::now() + Duration::from_millis(300);
            while Instant::now() < wait_until {
                match pipe.pop_to_dev(Duration::from_millis(20)) {
                    Some(ACK) => {
                        run_device(pipe);
                        return;
                    }
                    Some(_) => continue,
                    None => break,
                }
            }
        }
    }

    fn test_options() -> UartOptions {
        UartOptions {
            baudrate: 115200,
            max_baudrate: None,
            connect_timeout: Duration::from_secs(2),
            ack_timeout: Duration::from_millis(500),
            data_timeout: Duration::from_secs(2),
            auto_enter: true,
            enter_timeout: Duration::from_secs(4),
        }
    }

    #[test]
    fn enter_upgrade_sends_console_commands() {
        let pipe = Pipe::new();
        let device_pipe = pipe.clone();
        let handle = thread::spawn(move || run_device_console_entry(device_pipe));

        let options = test_options();
        let mut transport = UartTransport::with_port(
            Box::new(MockPort { pipe: pipe.clone() }),
            "mock",
            options.clone(),
        );

        transport.enter_upgrade(options.enter_timeout).unwrap();
        let csw = transport
            .write_txn(b"after entry", CswPolicy::Required)
            .unwrap()
            .unwrap();
        assert!(csw.is_ok());

        pipe.stop.store(true, Ordering::SeqCst);
        handle.join().unwrap();
    }

    #[test]
    fn enter_upgrade_answers_boot_keyword() {
        let pipe = Pipe::new();
        let device_pipe = pipe.clone();
        let handle = thread::spawn(move || run_device_keyword_entry(device_pipe));

        let options = test_options();
        let mut transport = UartTransport::with_port(
            Box::new(MockPort { pipe: pipe.clone() }),
            "mock",
            options.clone(),
        );

        transport.enter_upgrade(options.enter_timeout).unwrap();
        let data = transport.read_txn(16, CswPolicy::Required).unwrap();
        assert_eq!(data.len(), 16);

        pipe.stop.store(true, Ordering::SeqCst);
        handle.join().unwrap();
    }

    #[test]
    fn transport_roundtrips_against_device_simulator() {
        let pipe = Pipe::new();
        let device_pipe = pipe.clone();
        let handle = thread::spawn(move || run_device(device_pipe));

        let options = UartOptions {
            baudrate: 115200,
            max_baudrate: None,
            connect_timeout: Duration::from_secs(2),
            ack_timeout: Duration::from_millis(500),
            data_timeout: Duration::from_secs(2),
            auto_enter: false,
            enter_timeout: Duration::from_secs(2),
        };
        let mut transport =
            UartTransport::with_port(Box::new(MockPort { pipe: pipe.clone() }), "mock", options);

        transport.handshake().unwrap();

        let csw = transport
            .write_txn(b"hello device", CswPolicy::Required)
            .unwrap()
            .unwrap();
        assert!(csw.is_ok());

        let data = transport.read_txn(3000, CswPolicy::Required).unwrap();
        assert_eq!(data.len(), 3000);
        assert!(data.iter().all(|byte| *byte == 0x5A));

        let csw = transport
            .write_txn(&vec![0u8; LONG_FRAME_DATA], CswPolicy::Required)
            .unwrap()
            .unwrap();
        assert!(csw.is_ok());

        pipe.stop.store(true, Ordering::SeqCst);
        handle.join().unwrap();
    }
}
