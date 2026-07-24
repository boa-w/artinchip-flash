use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
#[cfg(target_os = "macos")]
use std::process::Command;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use fs2::FileExt;
use rusb::{DeviceHandle, UsbContext};

use crate::protocol::cbw_csw::*;
use crate::protocol::commands::*;

const AIC_VID: u16 = 0x33C3;
const AIC_PID: u16 = 0x6677;
const BULK_OUT_EP: u8 = 0x02;
const BULK_IN_EP: u8 = 0x81;
const TIMEOUT_MS: Duration = Duration::from_secs(30);
const SHORT_TIMEOUT: Duration = Duration::from_millis(500);
const CHUNK_SIZE: u32 = 1024 * 1024;
const BULK_WRITE_CHUNK: usize = 64 * 1024;
const DEFAULT_BURN_TIMEOUT: Duration = Duration::from_secs(60);
const UPDATER_PROBE_DELAY: Duration = Duration::from_millis(30);
const RECONNECT_SETTLE_DELAY: Duration = Duration::from_millis(120);
const START_WRITE_RETRY_DELAY: Duration = Duration::from_millis(100);
const START_WRITE_RETRY_TIMEOUT: Duration = Duration::from_secs(5);
const OFFICIAL_UPG_CFG_RESERVED: [u8; 31] = [
    0xea, 0x00, 0x00, 0xbc, 0xf5, 0x44, 0x04, 0x50, 0xf5, 0x44, 0x04, 0x01, 0x00, 0x00, 0x00, 0x18,
    0x73, 0xdf, 0x05, 0x50, 0xf5, 0x44, 0x04, 0x40, 0xfe, 0xf1, 0x00, 0x18, 0x73, 0xdf, 0x05,
];
const DEFAULT_SELECTED_PARTS: &[&str] = &["spl", "env", "os"];

#[derive(Clone, Debug)]
pub struct DeviceInfo {
    pub bus_number: u8,
    pub address: u8,
    pub vendor_id: u16,
    pub product_id: u16,
    pub port_path: String,
    pub speed: String,
    pub ready: bool,
    pub status: Option<String>,
}

#[derive(Clone, Debug)]
pub struct UsbDeviceInfo {
    pub bus_number: u8,
    pub address: u8,
    pub vendor_id: u16,
    pub product_id: u16,
    pub port_path: String,
    pub speed: String,
    pub class_code: u8,
    pub subclass_code: u8,
    pub protocol_code: u8,
}

#[derive(Clone, Debug)]
pub struct BurnOptions {
    pub selected_parts: Vec<String>,
    pub reset_after_burn: bool,
    pub burn_timeout: Duration,
}

impl Default for BurnOptions {
    fn default() -> Self {
        Self {
            selected_parts: DEFAULT_SELECTED_PARTS
                .iter()
                .map(|part| (*part).to_string())
                .collect(),
            reset_after_burn: true,
            burn_timeout: DEFAULT_BURN_TIMEOUT,
        }
    }
}

#[derive(Clone, Debug)]
pub enum BurnEvent {
    Log(String),
    Stage(String),
    ComponentStarted {
        name: String,
        partition: String,
        size: usize,
    },
    ComponentProgress {
        name: String,
        sent: usize,
        total: usize,
    },
    OverallProgress {
        sent: usize,
        total: usize,
    },
    ComponentFinished {
        name: String,
    },
    Finished,
}

pub type BurnCallback<'a> = dyn FnMut(BurnEvent) + Send + 'a;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CswPolicy {
    Required,
    AllowMissing,
}

#[derive(Clone, Debug)]
struct UpgResponse {
    payload: Vec<u8>,
}

pub struct AicDevice {
    handle: DeviceHandle<rusb::Context>,
    _access_lock: UsbAccessLock,
    tag: u32,
    in_buf: Vec<u8>,
    bus_number: u8,
    address: u8,
}

impl AicDevice {
    pub fn list_usb_devices() -> Result<Vec<UsbDeviceInfo>, String> {
        let context = rusb::Context::new().map_err(|e| format!("Failed to init USB: {}", e))?;
        let devices = context
            .devices()
            .map_err(|e| format!("Failed to list USB devices: {}", e))?;

        let mut found = Vec::new();
        for device in devices.iter() {
            let desc = device
                .device_descriptor()
                .map_err(|e| format!("Failed to get device descriptor: {}", e))?;
            let port_path = device
                .port_numbers()
                .map(|ports| {
                    ports
                        .iter()
                        .map(u8::to_string)
                        .collect::<Vec<_>>()
                        .join("-")
                })
                .unwrap_or_default();
            found.push(UsbDeviceInfo {
                bus_number: device.bus_number(),
                address: device.address(),
                vendor_id: desc.vendor_id(),
                product_id: desc.product_id(),
                port_path,
                speed: format!("{:?}", device.speed()),
                class_code: desc.class_code(),
                subclass_code: desc.sub_class_code(),
                protocol_code: desc.protocol_code(),
            });
        }
        Ok(found)
    }

    pub fn scan_devices() -> Result<Vec<DeviceInfo>, String> {
        #[cfg(target_os = "macos")]
        {
            if let Ok(devices) = scan_devices_macos_ioreg() {
                return Ok(devices);
            }
        }
        scan_devices_libusb()
    }

    pub fn scan_devices_libusb() -> Result<Vec<DeviceInfo>, String> {
        scan_devices_libusb()
    }

    pub fn scan_devices_fast() -> Result<Vec<DeviceInfo>, String> {
        #[cfg(target_os = "macos")]
        {
            scan_devices_macos_ioreg()
        }
        #[cfg(not(target_os = "macos"))]
        {
            scan_devices_libusb()
        }
    }

    pub fn open_first() -> Result<Self, String> {
        Self::open_matching(None)
    }

    pub fn open_by_location(bus_number: u8, address: u8) -> Result<Self, String> {
        Self::open_matching(Some((bus_number, address)))
    }

    fn open_matching(location: Option<(u8, u8)>) -> Result<Self, String> {
        Self::open_matching_with_recovery(location, false)
    }

    fn open_matching_with_recovery(
        location: Option<(u8, u8)>,
        recover_endpoints: bool,
    ) -> Result<Self, String> {
        let access_lock = UsbAccessLock::try_acquire()?;
        Self::open_matching_with_lock(location, recover_endpoints, access_lock)
    }

    fn open_matching_with_lock(
        location: Option<(u8, u8)>,
        recover_endpoints: bool,
        access_lock: UsbAccessLock,
    ) -> Result<Self, String> {
        #[cfg(target_os = "macos")]
        if let Some(err) = macos_preflight_open_error(location) {
            return Err(err);
        }

        let context = rusb::Context::new().map_err(|e| format!("Failed to init USB: {}", e))?;

        let devices = context
            .devices()
            .map_err(|e| format!("Failed to list USB devices: {}", e))?;

        let mut matched_count = 0usize;
        let mut last_open_error = None;

        for device in devices.iter() {
            let desc = device
                .device_descriptor()
                .map_err(|e| format!("Failed to get device descriptor: {}", e))?;
            if desc.vendor_id() == AIC_VID && desc.product_id() == AIC_PID {
                if let Some((bus, address)) = location {
                    if device.bus_number() != bus || device.address() != address {
                        continue;
                    }
                }
                matched_count += 1;
                #[cfg(target_os = "macos")]
                if let Err(e) = macos_check_device_ready(device.address()) {
                    let err = format!(
                        "Found ArtInChip device at bus {} address {}, but macOS has not configured it for USB transfers: {}",
                        device.bus_number(),
                        device.address(),
                        e
                    );
                    if location.is_some() {
                        return Err(err);
                    }
                    last_open_error = Some(err);
                    continue;
                }
                let handle = match device.open() {
                    Ok(handle) => handle,
                    Err(e) => {
                        let err = format!(
                            "Found ArtInChip device at bus {} address {}, but failed to open it: {}",
                            device.bus_number(),
                            device.address(),
                            format_usb_open_error(e, device.address())
                        );
                        if location.is_some() {
                            return Err(err);
                        }
                        last_open_error = Some(err);
                        continue;
                    }
                };
                if let Ok(active) = handle.kernel_driver_active(0) {
                    if active {
                        let _ = handle.detach_kernel_driver(0);
                    }
                }
                if let Ok(desc) = device.config_descriptor(0) {
                    for iface in desc.interfaces() {
                        for desc in iface.descriptors() {
                            eprintln!(
                                "  Interface {}: {} endpoints",
                                desc.interface_number(),
                                desc.num_endpoints()
                            );
                            for ep in desc.endpoint_descriptors() {
                                eprintln!(
                                    "    EP 0x{:02x} {} max_packet={}",
                                    ep.address(),
                                    if ep.direction() == rusb::Direction::In {
                                        "IN"
                                    } else {
                                        "OUT"
                                    },
                                    ep.max_packet_size()
                                );
                            }
                        }
                    }
                }

                handle
                    .claim_interface(0)
                    .map_err(|e| format!("Failed to claim interface: {}", e))?;

                let mut dev = Self {
                    handle,
                    _access_lock: access_lock,
                    tag: 1,
                    in_buf: Vec::new(),
                    bus_number: device.bus_number(),
                    address: device.address(),
                };
                if recover_endpoints {
                    let _ = dev.handle.clear_halt(BULK_OUT_EP);
                    let _ = dev.handle.clear_halt(BULK_IN_EP);
                    eprintln!(
                        "  Cleared halt on EP 0x{:02x} and 0x{:02x}",
                        BULK_OUT_EP, BULK_IN_EP
                    );
                    dev.drain_in_endpoint(Duration::from_millis(50), 64 * 1024)?;
                }
                return Ok(dev);
            }
        }
        if let Some(err) = last_open_error {
            if matched_count > 1 {
                return Err(format!(
                    "{} ({} matching devices were detected)",
                    err, matched_count
                ));
            }
            return Err(err);
        }
        match location {
            Some((bus, address)) => Err(format!(
                "No ArtInChip device found at bus {} address {} (VID=0x33C3, PID=0x6677)",
                bus, address
            )),
            None => Err("No ArtInChip device found (VID=0x33C3, PID=0x6677)".to_string()),
        }
    }

    fn reopen(&mut self) -> Result<(), String> {
        let replacement = Self::open_matching_with_lock(None, false, self._access_lock.clone())?;
        *self = replacement;
        Ok(())
    }

    fn has_device_at(bus_number: u8, address: u8) -> Result<bool, String> {
        let context = rusb::Context::new().map_err(|e| format!("Failed to init USB: {}", e))?;
        let devices = context
            .devices()
            .map_err(|e| format!("Failed to list USB devices: {}", e))?;

        for device in devices.iter() {
            let desc = device
                .device_descriptor()
                .map_err(|e| format!("Failed to get device descriptor: {}", e))?;
            if desc.vendor_id() == AIC_VID
                && desc.product_id() == AIC_PID
                && device.bus_number() == bus_number
                && device.address() == address
            {
                return Ok(true);
            }
        }
        Ok(false)
    }

    fn wait_reconnect(&mut self, timeout: Duration) -> Result<(), String> {
        eprintln!("Waiting for ArtInChip device to reconnect...");
        let old_bus = self.bus_number;
        let old_address = self.address;
        let deadline = Instant::now() + timeout;
        let mut last_err = String::new();
        let mut old_device_gone = false;
        while Instant::now() < deadline {
            if !old_device_gone {
                match Self::has_device_at(old_bus, old_address) {
                    Ok(false) => {
                        old_device_gone = true;
                        eprintln!("  Previous device {}:{} disappeared", old_bus, old_address);
                    }
                    Ok(true) => {
                        thread::sleep(Duration::from_millis(100));
                        continue;
                    }
                    Err(e) => last_err = e,
                }
            }

            if old_device_gone {
                match self.reopen() {
                    Ok(()) => {
                        eprintln!(
                            "Device reconnected at {}:{}.",
                            self.bus_number, self.address
                        );
                        thread::sleep(RECONNECT_SETTLE_DELAY);
                        return Ok(());
                    }
                    Err(e) => last_err = e,
                }
            }
            thread::sleep(Duration::from_millis(100));
        }
        Err(format!(
            "Timed out waiting for device reconnect: {}",
            last_err
        ))
    }

    fn drain_in_endpoint(&mut self, timeout: Duration, max_bytes: usize) -> Result<(), String> {
        let mut total = 0usize;
        let mut buf = [0u8; 512];
        loop {
            match self.handle.read_bulk(BULK_IN_EP, &mut buf, timeout) {
                Ok(n) => {
                    if n == 0 {
                        break;
                    }
                    total += n;
                    eprintln!("  Flushed {} stale bytes from IN EP", n);
                    if total >= max_bytes {
                        eprintln!("  Stopped IN flush after {} bytes", total);
                        break;
                    }
                }
                Err(rusb::Error::Timeout) => break,
                Err(e) => return Err(format!("IN EP flush error: {}", e)),
            }
        }
        Ok(())
    }

    // ── Low-level transactions ─────────────────────────────────────────

    /// Full write transaction: CBW → data → CSW
    fn write_txn(&mut self, payload: &[u8]) -> Result<AicCsw, String> {
        self.write_txn_policy(payload, CswPolicy::Required)?
            .ok_or_else(|| "CSW unexpectedly missing".to_string())
    }

    fn write_txn_policy(
        &mut self,
        payload: &[u8],
        policy: CswPolicy,
    ) -> Result<Option<AicCsw>, String> {
        let tag = self.next_tag();

        let cbw = AicCbw::new_write(tag, payload.len() as u32);
        let cbw_bytes = cbw.to_bytes();
        eprintln!(
            "  >> WRITE CBW tag={} len={} cbw={:02x?}",
            tag,
            payload.len(),
            cbw_bytes
        );
        self.write_bulk(cbw_bytes)?;
        if !payload.is_empty() {
            eprintln!("  >> DATA len={}", payload.len());
            self.write_bulk_data_phase(payload)?;
        }
        let csw = self.read_csw(tag, policy)?;
        if let Some(csw) = &csw {
            eprintln!(
                "  << CSW tag={} status={} residue={} sig=0x{:08x}",
                csw.tag_val(),
                csw.status_val(),
                csw.data_residue_val(),
                csw.signature()
            );
            self.check_csw(csw, tag)?;
        }
        Ok(csw)
    }

    fn write_txn_reconnect_once(&mut self, payload: &[u8]) -> Result<AicCsw, String> {
        match self.write_txn(payload) {
            Ok(csw) => Ok(csw),
            Err(e) if e.contains("Bulk write failed at 0/31") || e.contains("Pipe") => {
                eprintln!("  >> WRITE retry after reconnect: {}", e);
                self.wait_reconnect(Duration::from_secs(10))?;
                self.write_txn(payload)
            }
            Err(e) => Err(e),
        }
    }

    /// Full read transaction: CBW → data → CSW
    fn read_txn_policy(&mut self, read_len: u32, policy: CswPolicy) -> Result<Vec<u8>, String> {
        let tag = self.next_tag();

        let cbw = AicCbw::new_read(tag, read_len);
        eprintln!(
            "  >> READ CBW tag={} len={} cbw={:02x?}",
            tag,
            read_len,
            cbw.to_bytes()
        );
        self.write_bulk(cbw.to_bytes())?;

        let data = self.read_exact_from_in(read_len as usize, TIMEOUT_MS)?;
        eprintln!("  << DATA {} bytes", data.len());

        let csw = self.read_csw(tag, policy)?;
        if let Some(csw) = &csw {
            eprintln!(
                "  << CSW tag={} status={} residue={} sig=0x{:08x}",
                csw.tag_val(),
                csw.status_val(),
                csw.data_residue_val(),
                csw.signature()
            );
            self.check_csw(csw, tag)?;
        } else if policy == CswPolicy::Required {
            return Err("CSW unexpectedly missing".to_string());
        }
        Ok(data)
    }

    fn next_tag(&mut self) -> u32 {
        let tag = self.tag;
        self.tag = self.tag.wrapping_add(1).max(1);
        tag
    }

    fn write_bulk(&self, data: &[u8]) -> Result<(), String> {
        self.write_bulk_with_timeout(data, TIMEOUT_MS)
    }

    fn write_bulk_with_timeout(&self, data: &[u8], timeout: Duration) -> Result<(), String> {
        let mut written = 0usize;
        while written < data.len() {
            let end = (written + BULK_WRITE_CHUNK).min(data.len());
            let n = self
                .handle
                .write_bulk(BULK_OUT_EP, &data[written..end], timeout)
                .map_err(|e| format!("Bulk write failed at {}/{}: {}", written, data.len(), e))?;
            if n == 0 {
                return Err("Bulk write made no progress".to_string());
            }
            written += n;
        }
        Ok(())
    }

    fn write_bulk_data_phase(&self, payload: &[u8]) -> Result<(), String> {
        let deadline = Instant::now() + START_WRITE_RETRY_TIMEOUT;
        let mut attempts = 0usize;
        loop {
            match self.write_bulk_with_timeout(payload, Duration::from_secs(1)) {
                Ok(()) => return Ok(()),
                Err(e)
                    if is_bulk_write_start_timeout(&e, payload.len())
                        && Instant::now() < deadline =>
                {
                    attempts += 1;
                    eprintln!(
                        "  >> DATA start retry #{} after endpoint settle: {}",
                        attempts, e
                    );
                    thread::sleep(START_WRITE_RETRY_DELAY);
                }
                Err(e) => return Err(e),
            }
        }
    }

    fn read_bulk_to_buffer(&mut self, timeout: Duration) -> Result<usize, rusb::Error> {
        let mut buf = [0u8; 64 * 1024];
        let n = self.handle.read_bulk(BULK_IN_EP, &mut buf, timeout)?;
        if n > 0 {
            eprintln!("  << IN EP raw {} bytes: {:02x?}", n, &buf[..n.min(128)]);
            self.in_buf.extend_from_slice(&buf[..n]);
        }
        Ok(n)
    }

    fn read_exact_from_in(&mut self, len: usize, timeout: Duration) -> Result<Vec<u8>, String> {
        let deadline = Instant::now() + timeout;
        while self.in_buf.len() < len {
            let now = Instant::now();
            if now >= deadline {
                if let Some(err) = self.unexpected_csw_error("Bulk read timed out") {
                    return Err(err);
                }
                return Err(format!(
                    "Bulk read timed out with {}/{} bytes buffered",
                    self.in_buf.len(),
                    len
                ));
            }
            let remaining = deadline.saturating_duration_since(now);
            match self.read_bulk_to_buffer(remaining.min(TIMEOUT_MS)) {
                Ok(0) => {}
                Ok(n) => eprintln!(
                    "  << DATA buffered {} bytes (buffer={}/{})",
                    n,
                    self.in_buf.len(),
                    len
                ),
                Err(rusb::Error::Timeout) => {
                    if let Some(err) = self.unexpected_csw_error("Bulk read timed out") {
                        return Err(err);
                    }
                    return Err(format!(
                        "Bulk read timed out with {}/{} bytes buffered",
                        self.in_buf.len(),
                        len
                    ));
                }
                Err(e) => return Err(format!("Bulk read failed: {}", e)),
            }
        }
        Ok(self.in_buf.drain(..len).collect())
    }

    fn unexpected_csw_error(&self, context: &str) -> Option<String> {
        if self.in_buf.len() < 13 || self.find_csw_signature() != Some(0) {
            return None;
        }
        let csw = AicCsw::from_bytes(&self.in_buf[..13])?;
        Some(format!(
            "{}: device returned CSW instead of DATA (tag={}, status={}, residue={}, buffered={})",
            context,
            csw.tag_val(),
            csw.status_val(),
            csw.data_residue_val(),
            self.in_buf.len()
        ))
    }

    fn find_csw_signature(&self) -> Option<usize> {
        let sig = AIC_USB_SIGN_USBS.to_le_bytes();
        self.in_buf.windows(4).position(|w| w == sig)
    }

    fn read_csw(&mut self, expected_tag: u32, policy: CswPolicy) -> Result<Option<AicCsw>, String> {
        let deadline = Instant::now()
            + if policy == CswPolicy::AllowMissing {
                SHORT_TIMEOUT
            } else {
                TIMEOUT_MS
            };
        loop {
            if let Some(pos) = self.find_csw_signature() {
                if pos > 0 {
                    eprintln!("  << Dropping {} non-CSW stale bytes before USBS", pos);
                    self.in_buf.drain(..pos);
                }
                if self.in_buf.len() < 13 {
                    if let Err(e) = self.fill_until(deadline, 13) {
                        if policy == CswPolicy::AllowMissing {
                            eprintln!("  << Incomplete CSW accepted by transaction policy: {}", e);
                            self.in_buf.clear();
                            return Ok(None);
                        }
                        return Err(e);
                    }
                    continue;
                }
                let csw = AicCsw::from_bytes(&self.in_buf[..13])
                    .ok_or_else(|| "Failed to parse CSW".to_string())?;
                eprintln!(
                    "  << CSW candidate sig=0x{:08x} tag={} status={} residue={}",
                    csw.signature(),
                    csw.tag_val(),
                    csw.status_val(),
                    csw.data_residue_val()
                );
                self.in_buf.drain(..13);

                if csw.tag_val() == expected_tag {
                    return Ok(Some(csw));
                }

                eprintln!(
                    "  << Discarding stale CSW tag={} while expecting tag={}",
                    csw.tag_val(),
                    expected_tag
                );
                continue;
            }

            if !self.in_buf.is_empty() && self.in_buf.len() > 3 {
                let keep = self.in_buf.split_off(self.in_buf.len() - 3);
                let dropped = std::mem::replace(&mut self.in_buf, keep).len();
                eprintln!("  << Dropping {} bytes without CSW signature", dropped);
            }

            let now = Instant::now();
            if now >= deadline {
                if policy == CswPolicy::AllowMissing {
                    eprintln!("  << No CSW before timeout; accepted by transaction policy");
                    return Ok(None);
                }
                return Err(format!("No CSW for tag {} before timeout", expected_tag));
            }
            match self
                .read_bulk_to_buffer(deadline.saturating_duration_since(now).min(SHORT_TIMEOUT))
            {
                Ok(_) => {}
                Err(rusb::Error::Timeout) if policy == CswPolicy::AllowMissing => {
                    eprintln!("  << No CSW after short timeout; accepted by transaction policy");
                    return Ok(None);
                }
                Err(rusb::Error::Pipe | rusb::Error::NoDevice)
                    if policy == CswPolicy::AllowMissing =>
                {
                    eprintln!(
                        "  << Device disconnected before CSW; accepted by transaction policy"
                    );
                    return Ok(None);
                }
                Err(rusb::Error::Timeout) => {}
                Err(e) => return Err(format!("IN EP read failed: {}", e)),
            }
        }
    }

    fn fill_until(&mut self, deadline: Instant, min_len: usize) -> Result<(), String> {
        while self.in_buf.len() < min_len {
            let now = Instant::now();
            if now >= deadline {
                return Err(format!(
                    "Timed out waiting for {} buffered bytes (have {})",
                    min_len,
                    self.in_buf.len()
                ));
            }
            match self
                .read_bulk_to_buffer(deadline.saturating_duration_since(now).min(SHORT_TIMEOUT))
            {
                Ok(_) => {}
                Err(rusb::Error::Timeout) => {}
                Err(e) => return Err(format!("IN EP read failed: {}", e)),
            }
        }
        Ok(())
    }

    fn check_csw(&self, csw: &AicCsw, expected_tag: u32) -> Result<(), String> {
        if csw.tag_val() != expected_tag {
            return Err(format!(
                "CSW tag mismatch: got {}, expected {}",
                csw.tag_val(),
                expected_tag
            ));
        }
        if !csw.is_ok() {
            return Err(format!(
                "CSW failed: status={} residue={}",
                csw.status_val(),
                csw.data_residue_val()
            ));
        }
        Ok(())
    }

    fn build_cmd_hdr(&self, cmd: u8, data_len: u32) -> Vec<u8> {
        let hdr = CmdHeader::new(cmd, data_len);
        hdr.to_bytes().to_vec()
    }

    // ── High-level protocol helpers ────────────────────────────────────

    /// Send only a command header (no payload, no response read).
    /// Used as first step in multi-transaction commands.
    fn send_hdr(&mut self, cmd: u8, data_len: u32) -> Result<(), String> {
        self.write_txn_reconnect_once(&self.build_cmd_hdr(cmd, data_len))?;
        Ok(())
    }

    fn cmd_hdr_data_resp(
        &mut self,
        cmd: u8,
        payload: &[u8],
        resp_extra: usize,
    ) -> Result<UpgResponse, String> {
        self.cmd_hdr_data_resp_policy(cmd, payload, resp_extra, CswPolicy::Required)
    }

    fn cmd_hdr_data_resp_policy(
        &mut self,
        cmd: u8,
        payload: &[u8],
        resp_extra: usize,
        policy: CswPolicy,
    ) -> Result<UpgResponse, String> {
        self.send_hdr(cmd, payload.len() as u32)?;
        let csw = self.write_txn_policy(payload, policy)?;
        if policy == CswPolicy::AllowMissing && csw.is_none() {
            return Ok(UpgResponse {
                payload: Vec::new(),
            });
        }
        self.read_upg_response(cmd, resp_extra, policy)
    }

    fn cmd_hdr_len_prefixed_data_resp(
        &mut self,
        cmd: u8,
        payload: &[u8],
        resp_extra: usize,
    ) -> Result<UpgResponse, String> {
        self.send_hdr(cmd, (payload.len() + 4) as u32)?;
        self.write_txn(&(payload.len() as u32).to_le_bytes())?;
        self.write_txn(payload)?;
        self.read_upg_response(cmd, resp_extra, CswPolicy::Required)
    }

    fn cmd_hdr_resp(&mut self, cmd: u8, resp_extra: usize) -> Result<UpgResponse, String> {
        self.send_hdr(cmd, 0)?;
        self.read_upg_response(cmd, resp_extra, CswPolicy::Required)
    }

    fn read_upg_response(
        &mut self,
        cmd: u8,
        expected_payload_len: usize,
        policy: CswPolicy,
    ) -> Result<UpgResponse, String> {
        // Official AiBurn reads the 16-byte RESP packet first, then reads the
        // optional data packet separately. Reading header+payload in one CBW
        // makes some bootloaders answer the READ CBW with a failed CSW only.
        let header_data = match self.read_txn_policy(RESP_MIN_HDR_LEN as u32, policy) {
            Ok(data) => data,
            Err(e) if policy == CswPolicy::AllowMissing => {
                eprintln!("  << No UPG response accepted by transaction policy: {}", e);
                return Ok(UpgResponse {
                    payload: Vec::new(),
                });
            }
            Err(e) => return Err(e),
        };
        let header = self.parse_resp_header(cmd, &header_data)?;
        let declared_len = header.data_length_val() as usize;
        let payload_len = if declared_len > 0 {
            declared_len
        } else {
            expected_payload_len
        };
        let payload = if payload_len > 0 {
            match self.read_txn_policy(payload_len as u32, policy) {
                Ok(data) => data,
                Err(e) if policy == CswPolicy::AllowMissing => {
                    eprintln!("  << No UPG payload accepted by transaction policy: {}", e);
                    Vec::new()
                }
                Err(e) => return Err(e),
            }
        } else {
            Vec::new()
        };
        Ok(UpgResponse { payload })
    }

    /// Parse the 16-byte UPG response packet.
    fn parse_resp_header(&self, expected_cmd: u8, data: &[u8]) -> Result<RespHeader, String> {
        if data.len() < RESP_MIN_HDR_LEN {
            return Err("Response too short".to_string());
        }
        let resp = RespHeader::from_bytes(data)
            .ok_or_else(|| "Failed to parse response header".to_string())?;
        if !resp.is_ok() {
            return Err(format!(
                "Command 0x{:02x} failed, status: {}, magic=0x{:08x}",
                expected_cmd,
                resp.status_val(),
                resp.magic()
            ));
        }
        if resp.command() != 0 && resp.command() != expected_cmd {
            eprintln!(
                "  << Warning: response command 0x{:02x} does not match request 0x{:02x}",
                resp.command(),
                expected_cmd
            );
        }
        Ok(resp)
    }

    // ── Public API ─────────────────────────────────────────────────────

    pub fn get_hwinfo(&mut self) -> Result<HwInfo, String> {
        let resp = self.cmd_hdr_resp(CMD_GET_HWINFO, 104)?;
        HwInfo::from_bytes(&resp.payload).ok_or_else(|| {
            format!(
                "Failed to parse HWINFO ({} payload bytes)",
                resp.payload.len()
            )
        })
    }

    pub fn device_info_text(&mut self) -> Result<String, String> {
        let hwinfo = self.get_hwinfo()?;
        let chipid = hwinfo.chipid_val();
        let mut lines = Vec::new();
        lines.push(format!("Magic:        {}", hwinfo.magic_str()));
        lines.push(format!("Init mode:    {:#x}", hwinfo.init_mode()));
        lines.push(format!("Current mode: {:#x}", hwinfo.curr_mode()));
        lines.push(format!("Boot stage:   {}", hwinfo.boot_stage()));
        lines.push(format!(
            "Chip ID:      {:08x} {:08x} {:08x} {:08x}",
            chipid[0], chipid[1], chipid[2], chipid[3]
        ));
        if let Ok(media) = self.get_storage_media() {
            lines.push(format!("Storage media: {}", media));
        }
        Ok(lines.join("\n"))
    }

    pub fn set_upg_cfg(&mut self, mode: u8) -> Result<(), String> {
        let mut cfg = [0u8; 32];
        cfg[0] = mode;
        cfg[1..].copy_from_slice(&OFFICIAL_UPG_CFG_RESERVED);
        let _resp = self.cmd_hdr_len_prefixed_data_resp(CMD_SET_UPG_CFG, &cfg, 0)?;
        Ok(())
    }

    pub fn set_fwc_meta(&mut self, meta: &FwcMeta) -> Result<(), String> {
        let _resp = self.cmd_hdr_data_resp(CMD_SET_FWC_META, meta.to_bytes(), 0)?;
        Ok(())
    }

    pub fn get_block_size(&mut self) -> Result<u32, String> {
        let resp = self.cmd_hdr_resp(CMD_GET_BLOCK_SIZE, 4)?;
        if resp.payload.len() < 4 {
            return Err(format!(
                "Block size response too short: {} bytes",
                resp.payload.len()
            ));
        }
        let block_size = u32::from_le_bytes(resp.payload[0..4].try_into().unwrap());
        Ok(block_size)
    }

    fn start_fwc_data(&mut self, total_len: usize) -> Result<(), String> {
        self.send_hdr(CMD_SEND_FWC_DATA, total_len as u32)
    }

    fn write_fwc_data_chunk(&mut self, chunk: &[u8], policy: CswPolicy) -> Result<(), String> {
        let csw = self.write_txn_policy(chunk, policy)?;
        if policy == CswPolicy::AllowMissing && csw.is_none() {
            let _ = self.drain_in_endpoint(Duration::from_millis(50), 64 * 1024);
        }
        Ok(())
    }

    fn finish_fwc_data(&mut self, policy: CswPolicy) -> Result<(), String> {
        let _resp = self.read_upg_response(CMD_SEND_FWC_DATA, 0, policy)?;
        Ok(())
    }

    pub fn set_upg_end(&mut self) -> Result<(), String> {
        let mut payload = [0u8; 36];
        payload[0..4].copy_from_slice(&32u32.to_le_bytes());
        let _resp =
            self.cmd_hdr_data_resp_policy(CMD_SET_UPG_END, &payload, 0, CswPolicy::AllowMissing)?;
        Ok(())
    }

    pub fn run_shell(&mut self, cmd_str: &str) -> Result<(), String> {
        let cmd_bytes = cmd_str.as_bytes();
        let len = cmd_bytes.len() as u32;
        let mut payload = len.to_le_bytes().to_vec();
        payload.extend_from_slice(cmd_bytes);
        let _resp = self.cmd_hdr_data_resp(CMD_RUN_SHELL_STR, &payload, 0)?;
        Ok(())
    }

    pub fn reset(&mut self) -> Result<(), String> {
        self.run_shell("reset")
    }

    pub fn get_storage_media(&mut self) -> Result<String, String> {
        let resp = self.cmd_hdr_resp(CMD_GET_STORAGE_MEDIA, 64)?;
        let media = String::from_utf8_lossy(&resp.payload)
            .trim_end_matches('\0')
            .to_string();
        Ok(media)
    }

    pub fn get_device_log(&mut self) -> Result<String, String> {
        let size_resp = self.cmd_hdr_resp(CMD_GET_LOG_SIZE, 4)?;
        if size_resp.payload.len() < 4 {
            return Err(format!(
                "Log size response too short: {} bytes",
                size_resp.payload.len()
            ));
        }
        let size = u32::from_le_bytes(size_resp.payload[0..4].try_into().unwrap()) as usize;
        if size == 0 {
            return Ok(String::new());
        }
        let data_resp = self.cmd_hdr_resp(CMD_GET_LOG_DATA, size)?;
        Ok(String::from_utf8_lossy(&data_resp.payload).to_string())
    }

    pub fn show_info(&mut self) -> Result<(), String> {
        let hwinfo = self.get_hwinfo()?;
        let chipid = hwinfo.chipid_val();
        println!("  Magic:        {}", hwinfo.magic_str());
        println!("  Init mode:    {:#x}", hwinfo.init_mode());
        println!("  Current mode: {:#x}", hwinfo.curr_mode());
        println!("  Boot stage:   {}", hwinfo.boot_stage());
        println!(
            "  Chip ID:      {:08x} {:08x} {:08x} {:08x}",
            chipid[0], chipid[1], chipid[2], chipid[3]
        );
        Ok(())
    }

    pub fn burn_image(
        &mut self,
        img_data: &[u8],
        metas: &[FwcMeta],
        _header: &crate::image::parser::FwHeader,
    ) -> Result<(), String> {
        let options = BurnOptions::default();
        self.burn_image_with_options(img_data, metas, &options, None)
    }

    pub fn burn_image_with_options(
        &mut self,
        img_data: &[u8],
        metas: &[FwcMeta],
        options: &BurnOptions,
        mut callback: Option<&mut BurnCallback<'_>>,
    ) -> Result<(), String> {
        let classified = classify_components(metas, &options.selected_parts);
        print_burn_plan(&classified);
        emit(
            &mut callback,
            BurnEvent::Stage("Build component plan".to_string()),
        );

        let total_bytes = classified
            .iter()
            .filter(|c| c.kind == ComponentKind::ImageInfo || c.selected)
            .map(|c| c.meta.size_val() as usize)
            .sum::<usize>();
        let mut overall_sent = 0usize;

        let updater_count = classified
            .iter()
            .filter(|c| c.kind == ComponentKind::Updater)
            .count();
        if updater_count > 0 {
            eprintln!("Start burn online: sending updater components...");
            emit(
                &mut callback,
                BurnEvent::Stage("Send updater components".to_string()),
            );
            let updater_components: Vec<_> = classified
                .iter()
                .filter(|c| c.kind == ComponentKind::Updater)
                .collect();
            let updater_last_index = updater_components.len().saturating_sub(1);
            for (index, component) in updater_components.iter().enumerate() {
                let allow_final_response_no_csw = index == updater_last_index;
                self.send_component(
                    img_data,
                    component,
                    allow_final_response_no_csw,
                    &mut overall_sent,
                    total_bytes,
                    &mut callback,
                )?;
                if index < updater_last_index {
                    thread::sleep(UPDATER_PROBE_DELAY);
                    eprintln!("Probing bootloader between updater components...");
                    emit(
                        &mut callback,
                        BurnEvent::Stage("Probe bootloader between updater components".to_string()),
                    );
                    if let Err(e) = self.get_hwinfo() {
                        return Err(format!(
                            "Bootloader probe between updater components failed: {}",
                            e
                        ));
                    }
                }
            }
            eprintln!("Updater stage complete; waiting for bootloader upgrade reconnect...");
            emit(
                &mut callback,
                BurnEvent::Stage("Wait for bootloader reconnect".to_string()),
            );
            if let Err(e) = self.wait_reconnect(options.burn_timeout) {
                eprintln!("Warning: updater reconnect was not observed: {}", e);
                emit(
                    &mut callback,
                    BurnEvent::Log(format!(
                        "Warning: updater reconnect was not observed: {}",
                        e
                    )),
                );
            }
            eprintln!("Probing bootloader after reconnect...");
            emit(
                &mut callback,
                BurnEvent::Stage("Probe bootloader after reconnect".to_string()),
            );
            if let Err(e) = self.get_hwinfo() {
                return Err(format!("Bootloader probe after reconnect failed: {}", e));
            }
        } else {
            eprintln!(
                "No updater components found; continuing with target stage on current connection."
            );
        }

        eprintln!("Setting upgrade mode to FULL_DISK_UPGRADE...");
        emit(
            &mut callback,
            BurnEvent::Stage("Set full-disk upgrade mode".to_string()),
        );
        self.set_upg_cfg(UPG_MODE_FULL_DISK_UPGRADE)?;

        if let Some(info) = classified
            .iter()
            .find(|c| c.kind == ComponentKind::ImageInfo)
        {
            self.send_component(
                img_data,
                info,
                false,
                &mut overall_sent,
                total_bytes,
                &mut callback,
            )?;
        } else {
            eprintln!("Warning: no image.info component found");
            emit(
                &mut callback,
                BurnEvent::Log("Warning: no image.info component found".to_string()),
            );
        }

        let selected: Vec<_> = classified
            .iter()
            .filter(|c| c.kind == ComponentKind::Target && c.selected)
            .collect();
        if selected.is_empty() {
            return Err("No selected target components to burn".to_string());
        }
        for component in selected {
            self.send_component(
                img_data,
                component,
                false,
                &mut overall_sent,
                total_bytes,
                &mut callback,
            )?;
        }

        eprintln!("Ending upgrade...");
        emit(&mut callback, BurnEvent::Stage("End upgrade".to_string()));
        self.set_upg_end()?;
        if options.reset_after_burn {
            emit(&mut callback, BurnEvent::Stage("Reset device".to_string()));
            if let Err(e) = self.reset() {
                emit(
                    &mut callback,
                    BurnEvent::Log(format!("Warning: reset failed: {}", e)),
                );
            }
        }
        emit(&mut callback, BurnEvent::Finished);

        Ok(())
    }

    fn send_component(
        &mut self,
        img_data: &[u8],
        component: &FirmwareComponent<'_>,
        allow_final_no_csw: bool,
        overall_sent: &mut usize,
        overall_total: usize,
        callback: &mut Option<&mut BurnCallback<'_>>,
    ) -> Result<(), String> {
        let meta = component.meta;
        let name = meta.name_str();
        let size = meta.size_val() as usize;
        let offset = meta.offset_val() as usize;
        let crc_expected = meta.crc_val();
        let end = offset
            .checked_add(size)
            .ok_or_else(|| format!("{} offset/size overflow", name))?;
        if end > img_data.len() {
            return Err(format!(
                "{} image range out of bounds: offset={:#x}, size={}, image_len={}",
                name,
                offset,
                size,
                img_data.len()
            ));
        }

        eprintln!(
            "  Meta: {} (partition={}, offset={:#x}, size={}, crc=0x{:08x})",
            name,
            meta.partition_str(),
            offset,
            size,
            crc_expected
        );
        emit(
            callback,
            BurnEvent::ComponentStarted {
                name: name.to_string(),
                partition: meta.partition_str().to_string(),
                size,
            },
        );

        self.set_fwc_meta(meta)?;

        let block_size = self.get_block_size().unwrap_or(2048);
        eprintln!("    Block size: {}", block_size);

        self.start_fwc_data(size)?;

        let mut data_sent = 0usize;
        let chunk_max = if component.kind == ComponentKind::Updater {
            (block_size as usize).saturating_mul(512).max(512)
        } else {
            CHUNK_SIZE as usize
        };
        while data_sent < size {
            let chunk_end = (data_sent + chunk_max).min(size);
            let chunk_offset = offset + data_sent;
            let chunk_size = chunk_end - data_sent;
            let chunk_data = &img_data[chunk_offset..chunk_offset + chunk_size];

            self.write_fwc_data_chunk(chunk_data, CswPolicy::Required)?;
            data_sent += chunk_size;
            *overall_sent += chunk_size;
            let pct = (data_sent as f64 / size as f64) * 100.0;
            eprintln!("    {}: {}/{} ({:.1}%)", name, data_sent, size, pct);
            emit(
                callback,
                BurnEvent::ComponentProgress {
                    name: name.to_string(),
                    sent: data_sent,
                    total: size,
                },
            );
            emit(
                callback,
                BurnEvent::OverallProgress {
                    sent: *overall_sent,
                    total: overall_total,
                },
            );
        }

        let finish_policy = if allow_final_no_csw {
            CswPolicy::AllowMissing
        } else {
            CswPolicy::Required
        };
        self.finish_fwc_data(finish_policy)?;

        let actual_crc = crc32fast::hash(&img_data[offset..end]);
        if actual_crc != crc_expected {
            eprintln!(
                "    WARNING: CRC mismatch! expected=0x{:08x}, actual=0x{:08x}",
                crc_expected, actual_crc
            );
            emit(
                callback,
                BurnEvent::Log(format!(
                    "WARNING: {} CRC mismatch, expected=0x{:08x}, actual=0x{:08x}",
                    name, crc_expected, actual_crc
                )),
            );
        } else {
            eprintln!("    CRC OK (0x{:08x})", actual_crc);
        }
        emit(
            callback,
            BurnEvent::ComponentFinished {
                name: name.to_string(),
            },
        );
        Ok(())
    }
}

impl Drop for AicDevice {
    fn drop(&mut self) {
        let _ = self.handle.release_interface(0);
    }
}

fn is_bulk_write_start_timeout(err: &str, len: usize) -> bool {
    err.contains(&format!(
        "Bulk write failed at 0/{}: Operation timed out",
        len
    ))
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ComponentKind {
    Updater,
    ImageInfo,
    Target,
    Other,
}

struct FirmwareComponent<'a> {
    meta: &'a FwcMeta,
    kind: ComponentKind,
    selected: bool,
}

fn classify_components<'a>(
    metas: &'a [FwcMeta],
    selected_parts: &[String],
) -> Vec<FirmwareComponent<'a>> {
    metas
        .iter()
        .map(|meta| {
            let name = meta.name_str();
            let kind = if name.starts_with("image.updater.") {
                ComponentKind::Updater
            } else if name == "image.info" {
                ComponentKind::ImageInfo
            } else if name.starts_with("image.target.") {
                ComponentKind::Target
            } else {
                ComponentKind::Other
            };
            let selected =
                kind != ComponentKind::Target || target_part_selected(meta, selected_parts);
            FirmwareComponent {
                meta,
                kind,
                selected,
            }
        })
        .collect()
}

fn target_part_selected(meta: &FwcMeta, selected_parts: &[String]) -> bool {
    let partition = meta.partition_str();
    let target_name = meta
        .name_str()
        .strip_prefix("image.target.")
        .unwrap_or_else(|| meta.name_str());
    selected_parts
        .iter()
        .any(|part| partition == part || target_name == part || meta.name_str() == part)
}

fn print_burn_plan(components: &[FirmwareComponent<'_>]) {
    eprintln!("AiBurn-style component plan:");
    for component in components {
        eprintln!(
            "  {:?}: {} partition={} selected={}",
            component.kind,
            component.meta.name_str(),
            component.meta.partition_str(),
            component.selected
        );
    }
}

fn emit(callback: &mut Option<&mut BurnCallback<'_>>, event: BurnEvent) {
    if let Some(callback) = callback.as_deref_mut() {
        callback(event);
    }
}

fn scan_devices_libusb() -> Result<Vec<DeviceInfo>, String> {
    let context = rusb::Context::new().map_err(|e| format!("Failed to init USB: {}", e))?;
    let devices = context
        .devices()
        .map_err(|e| format!("Failed to list USB devices: {}", e))?;

    let mut found = Vec::new();
    for device in devices.iter() {
        let desc = device
            .device_descriptor()
            .map_err(|e| format!("Failed to get device descriptor: {}", e))?;
        if desc.vendor_id() != AIC_VID || desc.product_id() != AIC_PID {
            continue;
        }
        let port_path = device
            .port_numbers()
            .map(|ports| {
                ports
                    .iter()
                    .map(u8::to_string)
                    .collect::<Vec<_>>()
                    .join("-")
            })
            .unwrap_or_default();
        found.push(DeviceInfo {
            bus_number: device.bus_number(),
            address: device.address(),
            vendor_id: desc.vendor_id(),
            product_id: desc.product_id(),
            port_path,
            speed: format!("{:?}", device.speed()),
            ready: true,
            status: None,
        });
    }
    Ok(found)
}

#[cfg(target_os = "macos")]
fn scan_devices_macos_ioreg() -> Result<Vec<DeviceInfo>, String> {
    let output = Command::new("ioreg")
        .args(["-r", "-c", "IOUSBHostDevice", "-l", "-w", "0", "-d", "2"])
        .output()
        .map_err(|e| format!("Failed to run ioreg: {}", e))?;
    if !output.status.success() {
        return Err(format!("ioreg exited with {}", output.status));
    }
    let text = String::from_utf8(output.stdout)
        .map_err(|e| format!("ioreg output is not UTF-8: {}", e))?;

    let mut found = Vec::new();
    let mut current: Option<(IoregUsbDevice, String)> = None;
    for line in text.lines() {
        if line.contains("<class IOUSBHostDevice") {
            if let Some((device, _)) = current.take() {
                push_ioreg_device(&mut found, Some(device));
            }
            current = Some((
                IoregUsbDevice::default(),
                ioreg_device_property_prefix(line),
            ));
            continue;
        }
        let Some((device, property_prefix)) = current.as_mut() else {
            continue;
        };
        if line.contains("<class IOUSBHostInterface") {
            device.has_interface = true;
            continue;
        }
        if line == format!("{} }}", property_prefix) {
            if let Some((device, _)) = current.take() {
                push_ioreg_device(&mut found, Some(device));
            }
            continue;
        }
        if !line.starts_with(property_prefix.as_str()) {
            continue;
        }

        if let Some(value) = parse_ioreg_number(line, "idVendor") {
            device.vendor_id = Some(value as u16);
        } else if let Some(value) = parse_ioreg_number(line, "idProduct") {
            device.product_id = Some(value as u16);
        } else if parse_ioreg_number(line, "kUSBCurrentConfiguration").is_some() {
            device.configured = true;
        } else if let Some(value) = parse_ioreg_number(line, "kUSBAddress")
            .or_else(|| parse_ioreg_number(line, "USB Address"))
        {
            device.address = Some(value as u8);
        } else if let Some(value) = parse_ioreg_number(line, "locationID") {
            device.location_id = Some(value as u32);
        } else if let Some(value) = parse_ioreg_number(line, "USBSpeed") {
            device.usb_speed = Some(value as u8);
        }
    }
    if let Some((device, _)) = current {
        push_ioreg_device(&mut found, Some(device));
    }
    Ok(found)
}

#[cfg(target_os = "macos")]
#[derive(Default)]
struct IoregUsbDevice {
    vendor_id: Option<u16>,
    product_id: Option<u16>,
    address: Option<u8>,
    location_id: Option<u32>,
    usb_speed: Option<u8>,
    configured: bool,
    has_interface: bool,
}

#[cfg(target_os = "macos")]
fn push_ioreg_device(found: &mut Vec<DeviceInfo>, device: Option<IoregUsbDevice>) {
    let Some(device) = device else {
        return;
    };
    if device.vendor_id != Some(AIC_VID) || device.product_id != Some(AIC_PID) {
        return;
    }
    let status = macos_not_ready_status(device.configured, device.has_interface);
    found.push(DeviceInfo {
        bus_number: 0,
        address: device.address.unwrap_or(0),
        vendor_id: AIC_VID,
        product_id: AIC_PID,
        port_path: device
            .location_id
            .map(location_id_to_port_path)
            .unwrap_or_default(),
        speed: device
            .usb_speed
            .map(usb_speed_label)
            .unwrap_or("Unknown")
            .to_string(),
        ready: status.is_none(),
        status,
    });
}

#[cfg(target_os = "macos")]
fn ioreg_device_property_prefix(device_line: &str) -> String {
    let indent = device_line.split("+-o ").next().unwrap_or_default();
    format!("{}  |", indent)
}

#[cfg(target_os = "macos")]
fn parse_ioreg_number(line: &str, key: &str) -> Option<u64> {
    let pattern = format!("\"{}\" = ", key);
    let value = line.split_once(&pattern)?.1.trim();
    value
        .split(|ch: char| !ch.is_ascii_hexdigit() && ch != 'x' && ch != 'X')
        .next()
        .and_then(|raw| {
            if let Some(hex) = raw.strip_prefix("0x").or_else(|| raw.strip_prefix("0X")) {
                u64::from_str_radix(hex, 16).ok()
            } else {
                raw.parse().ok()
            }
        })
}

#[cfg(target_os = "macos")]
fn location_id_to_port_path(location_id: u32) -> String {
    let path = location_id >> 16;
    let text = format!("{:04x}", path);
    let ports = text.trim_start_matches('0');
    if ports.is_empty() {
        String::new()
    } else {
        ports
            .chars()
            .map(|ch| ch.to_string())
            .collect::<Vec<_>>()
            .join("-")
    }
}

#[cfg(target_os = "macos")]
fn usb_speed_label(speed: u8) -> &'static str {
    match speed {
        1 => "Low/Full",
        2 => "Full",
        3 => "High",
        4 => "Super",
        _ => "Unknown",
    }
}

#[cfg(target_os = "macos")]
fn macos_preflight_open_error(location: Option<(u8, u8)>) -> Option<String> {
    let devices = scan_devices_macos_ioreg().ok()?;
    let matching = devices
        .iter()
        .filter(|device| {
            location
                .map(|(bus, address)| device.bus_number == bus && device.address == address)
                .unwrap_or(true)
        })
        .collect::<Vec<_>>();
    if matching.is_empty() || matching.iter().any(|device| device.ready) {
        return None;
    }
    matching.first().map(|device| {
        format!(
            "Found ArtInChip device at bus {} address {}, but macOS has not configured it for USB transfers: {}",
            device.bus_number,
            device.address,
            device.status.as_deref().unwrap_or("not ready")
        )
    })
}

#[cfg(target_os = "macos")]
fn macos_check_device_ready(address: u8) -> Result<(), String> {
    let output = Command::new("ioreg")
        .args(["-r", "-c", "IOUSBHostDevice", "-l", "-w", "0", "-d", "2"])
        .output()
        .map_err(|e| format!("failed to inspect IOKit USB state: {}", e))?;
    if !output.status.success() {
        return Err(format!("ioreg exited with {}", output.status));
    }
    let text = String::from_utf8(output.stdout)
        .map_err(|e| format!("ioreg output is not UTF-8: {}", e))?;

    let Some(block) = find_macos_aic_device_block(&text, address) else {
        return Ok(());
    };
    if let Some(status) = macos_not_ready_status(
        block.contains("\"kUSBCurrentConfiguration\" = "),
        block.contains("<class IOUSBHostInterface"),
    ) {
        return Err(status);
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn macos_not_ready_status(configured: bool, has_interface: bool) -> Option<String> {
    if configured && has_interface {
        return None;
    }

    let mut reasons = Vec::new();
    if !configured {
        reasons.push("missing kUSBCurrentConfiguration");
    }
    if !has_interface {
        reasons.push("no IOUSBHostInterface endpoints");
    }
    Some(format!(
        "{}. Reconnect the board in upgrade mode, preferably directly to the Mac without a hub; if it persists, power-cycle the board or reboot macOS.",
        reasons.join(", ")
    ))
}

#[cfg(target_os = "macos")]
fn find_macos_aic_device_block(text: &str, address: u8) -> Option<String> {
    let mut current = String::new();
    for line in text.lines() {
        if line.starts_with("+-o ") && line.contains("<class IOUSBHostDevice") {
            if macos_block_matches_aic(&current, address) {
                return Some(current);
            }
            current.clear();
        }
        if !current.is_empty() || line.starts_with("+-o ") {
            current.push_str(line);
            current.push('\n');
        }
    }
    if macos_block_matches_aic(&current, address) {
        Some(current)
    } else {
        None
    }
}

#[cfg(target_os = "macos")]
fn macos_block_matches_aic(block: &str, address: u8) -> bool {
    block.contains("\"idVendor\" = 13251")
        && block.contains("\"idProduct\" = 26231")
        && (block.contains(&format!("\"kUSBAddress\" = {}", address))
            || block.contains(&format!("\"USB Address\" = {}", address)))
}

#[derive(Clone)]
struct UsbAccessLock {
    _file: Arc<File>,
}

impl UsbAccessLock {
    fn try_acquire() -> Result<Self, String> {
        let path = usb_access_lock_path()?;
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .open(&path)
            .map_err(|e| format!("Failed to open USB access lock '{}': {}", path.display(), e))?;

        match file.try_lock_exclusive() {
            Ok(()) => {
                let _ = file.set_len(0);
                let mut file_for_pid = file
                    .try_clone()
                    .map_err(|e| format!("Failed to clone USB access lock: {}", e))?;
                let _ = writeln!(file_for_pid, "pid={}", std::process::id());
                Ok(Self {
                    _file: Arc::new(file),
                })
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => Err(format!(
                "Another artinchip-flash instance is using the ArtInChip USB device{} Close the other CLI/GUI instance and retry. Lock: {}",
                lock_owner_hint(&path),
                path.display()
            )),
            Err(e) => Err(format!(
                "Failed to lock ArtInChip USB access '{}': {}",
                path.display(),
                e
            )),
        }
    }
}

fn lock_owner_hint(path: &std::path::Path) -> String {
    match std::fs::read_to_string(path) {
        Ok(owner) if !owner.trim().is_empty() => format!(" ({}).", owner.trim()),
        _ => ".".to_string(),
    }
}

fn usb_access_lock_path() -> Result<PathBuf, String> {
    let dir = platform_config_dir();
    std::fs::create_dir_all(&dir)
        .map_err(|e| format!("Failed to create '{}': {}", dir.display(), e))?;
    Ok(dir.join("artinchip-flash-usb-33c3-6677.lock"))
}

fn platform_config_dir() -> PathBuf {
    crate::standalone::default_app_dir()
}

fn format_usb_open_error(err: rusb::Error, _address: u8) -> String {
    #[cfg(target_os = "macos")]
    {
        if err == rusb::Error::Other {
            let state = macos_check_device_ready(_address)
                .err()
                .map(|reason| format!("; IOKit state: {}", reason))
                .unwrap_or_default();
            return format!(
                "Other error (macOS IOKit refused USBDeviceOpen{}; close other artinchip-flash/AiBurn instances, then unplug and reconnect the board)",
                state
            );
        }
    }
    format!("{}", err)
}
