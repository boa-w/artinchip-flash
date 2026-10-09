use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::thread;
use std::time::Duration;

use crate::log_verbose;
use crate::protocol::cbw_csw::*;
use crate::protocol::commands::*;
use crate::transport::{CswPolicy, UpgTransport};

const CHUNK_SIZE: u32 = 1024 * 1024;
const UPDATER_PROBE_DELAY: Duration = Duration::from_millis(30);
const OFFICIAL_UPG_CFG_RESERVED: [u8; 31] = [
    0xea, 0x00, 0x00, 0xbc, 0xf5, 0x44, 0x04, 0x50, 0xf5, 0x44, 0x04, 0x01, 0x00, 0x00, 0x00, 0x18,
    0x73, 0xdf, 0x05, 0x50, 0xf5, 0x44, 0x04, 0x40, 0xfe, 0xf1, 0x00, 0x18, 0x73, 0xdf, 0x05,
];

#[derive(Clone, Debug)]
pub struct BurnOptions {
    pub selected_parts: Vec<String>,
    pub reset_after_burn: bool,
    pub burn_timeout: Duration,
    /// Cooperative cancellation. When the flag is set, the burn loop aborts
    /// at the next chunk boundary and returns a "cancelled" error, leaving
    /// the device in upgrade mode so the user can retry.
    pub cancel: Option<Arc<AtomicBool>>,
    /// Experimental force upgrade (AiBurn "强制升级").
    ///
    /// Uses `UPG_MODE_BURN_IMG_FORCE` instead of `UPG_MODE_FULL_DISK_UPGRADE`
    /// and skips the post-burn reset (the two are mutually exclusive).
    /// Requires the device-side force-upgrade switch (Luban/Luban-Lite
    /// config, official manual §2.1.4); not verified on hardware.
    pub force_upgrade: bool,
}

impl Default for BurnOptions {
    fn default() -> Self {
        Self {
            selected_parts: ["spl", "env", "os"]
                .iter()
                .map(|part| (*part).to_string())
                .collect(),
            reset_after_burn: true,
            burn_timeout: Duration::from_secs(60),
            cancel: None,
            force_upgrade: false,
        }
    }
}

/// Upgrade-mode byte for `SET_UPG_CFG` derived from burn options.
///
/// Pure helper so frontends and tests can assert the mode without a device.
pub fn upg_mode(options: &BurnOptions) -> u8 {
    if options.force_upgrade {
        UPG_MODE_BURN_IMG_FORCE
    } else {
        UPG_MODE_FULL_DISK_UPGRADE
    }
}

/// Human-readable upgrade-mode name for stage logs.
pub fn upg_mode_name(options: &BurnOptions) -> &'static str {
    if options.force_upgrade {
        "force upgrade mode (BURN_IMG_FORCE, experimental)"
    } else {
        "full-disk upgrade mode"
    }
}

pub fn is_cancelled(options: &BurnOptions) -> bool {
    options
        .cancel
        .as_ref()
        .is_some_and(|flag| flag.load(Ordering::SeqCst))
}

fn check_cancelled(
    options: &BurnOptions,
    callback: &mut Option<&mut BurnCallback<'_>>,
) -> Result<(), String> {
    if is_cancelled(options) {
        emit(callback, BurnEvent::Log("Burn cancelled by user".to_string()));
        return Err("Burn cancelled by user".to_string());
    }
    Ok(())
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

#[derive(Clone, Debug)]
struct UpgResponse {
    payload: Vec<u8>,
}

/// Transport independent UPG device.
///
/// Holds the command/burn protocol; the transport only moves CBW/CSW
/// transactions between host and device.
pub struct UpgDevice<T: UpgTransport> {
    transport: T,
}

impl<T: UpgTransport> UpgDevice<T> {
    pub(crate) fn new(transport: T) -> Self {
        Self { transport }
    }

    pub fn transport_name(&self) -> &'static str {
        self.transport.transport_name()
    }

    pub(crate) fn transport_mut(&mut self) -> &mut T {
        &mut self.transport
    }

    // ── High-level protocol helpers ────────────────────────────────────

    /// Send only a command header (no payload, no response read).
    /// Used as first step in multi-transaction commands.
    fn send_hdr(&mut self, cmd: u8, data_len: u32) -> Result<(), String> {
        let hdr = CmdHeader::new(cmd, data_len);
        self.transport
            .write_txn(hdr.to_bytes(), CswPolicy::Required)?;
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
        let csw = self.transport.write_txn(payload, policy)?;
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
        self.transport
            .write_txn(&(payload.len() as u32).to_le_bytes(), CswPolicy::Required)?;
        self.transport.write_txn(payload, CswPolicy::Required)?;
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
                log_verbose!("  << No UPG response accepted by transaction policy: {}", e);
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
                    log_verbose!("  << No UPG payload accepted by transaction policy: {}", e);
                    Vec::new()
                }
                Err(e) => return Err(e),
            }
        } else {
            Vec::new()
        };
        Ok(UpgResponse { payload })
    }

    fn read_txn_policy(&mut self, read_len: u32, policy: CswPolicy) -> Result<Vec<u8>, String> {
        self.transport.read_txn(read_len, policy)
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
            log_verbose!(
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

    /// Structured device info lines shared by CLI printing and GUI display.
    ///
    /// Previously `show_info()` printed directly, forcing GUI to capture
    /// stdout. Callers now render these lines themselves.
    pub fn device_info_lines(&mut self) -> Result<Vec<String>, String> {
        let hwinfo = self.get_hwinfo()?;
        let chipid = hwinfo.chipid_val();
        let mut lines = vec![
            format!("Magic:        {}", hwinfo.magic_str()),
            format!("Init mode:    {:#x}", hwinfo.init_mode()),
            format!("Current mode: {:#x}", hwinfo.curr_mode()),
            format!("Boot stage:   {}", hwinfo.boot_stage()),
            format!(
                "Chip ID:      {:08x} {:08x} {:08x} {:08x}",
                chipid[0], chipid[1], chipid[2], chipid[3]
            ),
        ];
        if let Ok(media) = self.get_storage_media() {
            lines.push(format!("Storage media: {}", media));
        }
        Ok(lines)
    }

    pub fn device_info_text(&mut self) -> Result<String, String> {
        Ok(self.device_info_lines()?.join("\n"))
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
        let csw = self.transport.write_txn(chunk, policy)?;
        if policy == CswPolicy::AllowMissing && csw.is_none() {
            let _ = self
                .transport
                .drain_rx(Duration::from_millis(50), 64 * 1024);
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
        // Device shell buffer is 128 B and the string must arrive in one
        // packet (basic_cmd.c run_shell_str_cmd_write_input_data).
        let payload = shell_request(cmd_str)?;
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

    /// Parsed `GET_STORAGE_MEDIA` list (68-byte `struct storage_media`
    /// entries). The legacy [`UpgDevice::get_storage_media`] string form
    /// stays for existing callers.
    pub fn list_storage_media(&mut self) -> Result<Vec<StorageMedia>, String> {
        // One entry is 68 B; ask for a small batch (device answers however
        // many it detected, `medias[5]` at most).
        let resp = self.cmd_hdr_resp(CMD_GET_STORAGE_MEDIA, 5 * STORAGE_MEDIA_SIZE)?;
        Ok(StorageMedia::parse_list(&resp.payload))
    }

    /// `WRITE` (0x02): write `data` to device memory at `addr`.
    pub fn write_memory(&mut self, addr: u32, data: &[u8]) -> Result<(), String> {
        let payload = write_mem_request(addr, data);
        let _resp = self.cmd_hdr_data_resp(CMD_WRITE, &payload, 0)?;
        Ok(())
    }

    /// `READ` (0x03): read `len` bytes of device memory at `addr`.
    pub fn read_memory(&mut self, addr: u32, len: u32) -> Result<Vec<u8>, String> {
        let payload = read_mem_request(addr, len);
        let resp = self.cmd_hdr_data_resp(CMD_READ, &payload, len as usize)?;
        if resp.payload.len() != len as usize {
            return Err(format!(
                "READ returned {} bytes, expected {}",
                resp.payload.len(),
                len
            ));
        }
        Ok(resp.payload)
    }

    /// `EXEC` (0x04): call the function at `addr` (no arguments).
    pub fn exec_address(&mut self, addr: u32) -> Result<(), String> {
        let payload = exec_request(addr);
        let _resp = self.cmd_hdr_data_resp(CMD_EXEC, &payload, 0)?;
        Ok(())
    }

    /// `GET_STORAGE_GEOME` (0x1A): 24-byte geometry for `media`.
    pub fn get_storage_geometry(
        &mut self,
        media: &StorageMedia,
    ) -> Result<StorageGeometry, String> {
        let resp =
            self.cmd_hdr_data_resp(CMD_GET_STORAGE_GEOME, &media.to_bytes(), STORAGE_GEOMETRY_SIZE)?;
        StorageGeometry::from_bytes(&resp.payload).ok_or_else(|| {
            format!(
                "Geometry response too short: {} bytes",
                resp.payload.len()
            )
        })
    }

    /// `ERASE_STORAGE` (0x1B): synchronous erase described by `args`
    /// (units follow the geometry command: bytes on Flash, sectors on MMC).
    pub fn erase_storage(&mut self, args: &StorageEraseArgs) -> Result<(), String> {
        let _resp = self.cmd_hdr_data_resp(CMD_ERASE_STORAGE, &args.to_bytes(), 0)?;
        Ok(())
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
        for line in self.device_info_lines()? {
            println!("  {}", line);
        }
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
        check_cancelled(options, &mut callback)?;
        let classified = classify_components(metas, &options.selected_parts);
        for line in burn_plan_lines(&classified) {
            emit(&mut callback, BurnEvent::Log(line));
        }
        log_verbose!("{}", burn_plan_lines(&classified).join("\n"));
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
                check_cancelled(options, &mut callback)?;
                let allow_final_response_no_csw = index == updater_last_index;
                self.send_component(
                    img_data,
                    options,
                    component,
                    allow_final_response_no_csw,
                    &mut overall_sent,
                    total_bytes,
                    &mut callback,
                )?;
                if index < updater_last_index {
                    thread::sleep(UPDATER_PROBE_DELAY);
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
            emit(
                &mut callback,
                BurnEvent::Stage("Wait for bootloader reconnect".to_string()),
            );
            if let Err(e) = self.transport.reconnect(options.burn_timeout) {
                emit(
                    &mut callback,
                    BurnEvent::Log(format!(
                        "Warning: updater reconnect was not observed: {}",
                        e
                    )),
                );
            }
            emit(
                &mut callback,
                BurnEvent::Stage("Probe bootloader after reconnect".to_string()),
            );
            if let Err(e) = self.get_hwinfo() {
                return Err(format!("Bootloader probe after reconnect failed: {}", e));
            }
        } else {
            emit(
                &mut callback,
                BurnEvent::Log(
                    "No updater components found; continuing with target stage on current connection."
                        .to_string(),
                ),
            );
        }

        emit(
            &mut callback,
            BurnEvent::Stage(format!("Set {} upgrade mode", upg_mode_name(options))),
        );
        self.set_upg_cfg(upg_mode(options))?;

        if let Some(info) = classified
            .iter()
            .find(|c| c.kind == ComponentKind::ImageInfo)
        {
            self.send_component(
                img_data,
                options,
                info,
                false,
                &mut overall_sent,
                total_bytes,
                &mut callback,
            )?;
        } else {
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
            check_cancelled(options, &mut callback)?;
            self.send_component(
                img_data,
                options,
                component,
                false,
                &mut overall_sent,
                total_bytes,
                &mut callback,
            )?;
        }

        emit(&mut callback, BurnEvent::Stage("End upgrade".to_string()));
        self.set_upg_end()?;
        // Force upgrade and post-burn reset are mutually exclusive: the
        // device stays in upgrade mode so the forced image can be verified.
        let do_reset = options.reset_after_burn && !options.force_upgrade;
        if options.force_upgrade {
            emit(
                &mut callback,
                BurnEvent::Log(
                    "Force upgrade: skipping reset (mutually exclusive)".to_string(),
                ),
            );
        }
        if do_reset {
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
        options: &BurnOptions,
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

        emit(
            callback,
            BurnEvent::ComponentStarted {
                name: name.to_string(),
                partition: meta.partition_str().to_string(),
                size,
            },
        );
        emit(
            callback,
            BurnEvent::Log(format!(
                "Meta: {} (partition={}, offset={:#x}, size={}, crc=0x{:08x})",
                name,
                meta.partition_str(),
                offset,
                size,
                crc_expected
            )),
        );

        self.set_fwc_meta(meta)?;

        let block_size = self.get_block_size().unwrap_or(2048);
        emit(
            callback,
            BurnEvent::Log(format!("Block size: {}", block_size)),
        );

        self.start_fwc_data(size)?;

        let mut data_sent = 0usize;
        let base_chunk = if component.kind == ComponentKind::Updater {
            (block_size as usize).saturating_mul(512).max(512)
        } else {
            CHUNK_SIZE as usize
        };
        let transport_chunk = self.transport.max_write_chunk(block_size);
        let chunk_max = base_chunk.min(transport_chunk).max(1);
        log_verbose!(
            "    Write chunk: {} bytes ({})",
            chunk_max,
            self.transport.transport_name()
        );
        while data_sent < size {
            check_cancelled(options, callback)?;
            let chunk_end = (data_sent + chunk_max).min(size);
            let chunk_offset = offset + data_sent;
            let chunk_size = chunk_end - data_sent;
            let chunk_data = &img_data[chunk_offset..chunk_offset + chunk_size];

            self.write_fwc_data_chunk(chunk_data, CswPolicy::Required)?;
            data_sent += chunk_size;
            *overall_sent += chunk_size;
            log_verbose!(
                "    {}: {}/{} ({:.1}%)",
                name,
                data_sent,
                size,
                (data_sent as f64 / size as f64) * 100.0
            );
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
            emit(
                callback,
                BurnEvent::Log(format!(
                    "WARNING: {} CRC mismatch, expected=0x{:08x}, actual=0x{:08x}",
                    name, crc_expected, actual_crc
                )),
            );
        } else {
            emit(
                callback,
                BurnEvent::Log(format!("CRC OK (0x{:08x})", actual_crc)),
            );
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

fn burn_plan_lines(components: &[FirmwareComponent<'_>]) -> Vec<String> {
    let mut lines = vec!["AiBurn-style component plan:".to_string()];
    for component in components {
        lines.push(format!(
            "  {:?}: {} partition={} selected={}",
            component.kind,
            component.meta.name_str(),
            component.meta.partition_str(),
            component.selected
        ));
    }
    lines
}

fn emit(callback: &mut Option<&mut BurnCallback<'_>>, event: BurnEvent) {
    if let Some(callback) = callback.as_deref_mut() {
        callback(event);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::cbw_csw::{AicCsw, AIC_UPG_SIGN_UPGR, AIC_USB_SIGN_USBS};
    use crate::transport::UpgTransport;

    struct MockTransport {
        writes: usize,
        reads: usize,
        panic_on_use: bool,
        max_chunk: usize,
        /// `SET_UPG_CFG` mode bytes observed on the wire (32-byte cfg payload).
        upg_modes: Vec<u8>,
        /// Whether any write payload contained the `reset` shell command.
        saw_reset: bool,
        /// Every payload passed to `write_txn`, in order (cmd header first,
        /// then the command payload), so tests can assert exact wire bytes.
        payloads: Vec<Vec<u8>>,
    }

    impl MockTransport {
        fn ok_csw() -> AicCsw {
            let mut bytes = [0u8; 13];
            bytes[0..4].copy_from_slice(&AIC_USB_SIGN_USBS.to_le_bytes());
            AicCsw::from_bytes(&bytes).unwrap()
        }

        fn ok_resp_header() -> Vec<u8> {
            let mut hdr = vec![0u8; 16];
            hdr[0..4].copy_from_slice(&AIC_UPG_SIGN_UPGR.to_le_bytes());
            hdr[4] = 1;
            hdr[5] = 1;
            hdr[6] = 0; // wildcard command: accepted for any request
            hdr[7] = 0; // status OK
            hdr
        }
    }

    impl UpgTransport for MockTransport {
        fn write_txn(
            &mut self,
            payload: &[u8],
            _policy: CswPolicy,
        ) -> Result<Option<AicCsw>, String> {
            if self.panic_on_use {
                panic!("mock transport must not be touched after cancellation");
            }
            self.writes += 1;
            self.payloads.push(payload.to_vec());
            if payload.len() == 32 && payload[1..] == OFFICIAL_UPG_CFG_RESERVED {
                self.upg_modes.push(payload[0]);
            }
            if payload.windows(5).any(|w| w == b"reset") {
                self.saw_reset = true;
            }
            Ok(Some(Self::ok_csw()))
        }

        fn read_txn(&mut self, read_len: u32, _policy: CswPolicy) -> Result<Vec<u8>, String> {
            if self.panic_on_use {
                panic!("mock transport must not be touched after cancellation");
            }
            self.reads += 1;
            if read_len as usize == 16 {
                return Ok(Self::ok_resp_header());
            }
            if read_len as usize == 4 {
                // Block-size payload.
                return Ok(512u32.to_le_bytes().to_vec());
            }
            Ok(vec![0u8; read_len as usize])
        }

        fn reconnect(&mut self, _timeout: Duration) -> Result<(), String> {
            Ok(())
        }

        fn transport_name(&self) -> &'static str {
            "mock"
        }

        fn max_write_chunk(&self, _block_size: u32) -> usize {
            self.max_chunk
        }
    }

    fn make_meta(name: &str, partition: &str, offset: u32, size: u32, crc: u32) -> FwcMeta {
        let mut bytes = vec![0u8; 512];
        bytes[0..8].copy_from_slice(b"FWC_META");
        bytes[8..8 + name.len().min(64)].copy_from_slice(&name.as_bytes()[..name.len().min(64)]);
        bytes[72..72 + partition.len().min(64)]
            .copy_from_slice(&partition.as_bytes()[..partition.len().min(64)]);
        bytes[136..140].copy_from_slice(&offset.to_le_bytes());
        bytes[140..144].copy_from_slice(&size.to_le_bytes());
        bytes[144..148].copy_from_slice(&crc.to_le_bytes());
        FwcMeta::from_bytes(&bytes).unwrap()
    }

    #[test]
    fn is_cancelled_reflects_flag() {
        let plain = BurnOptions::default();
        assert!(!is_cancelled(&plain));
        let flag = Arc::new(AtomicBool::new(false));
        let armed = BurnOptions {
            cancel: Some(flag.clone()),
            ..Default::default()
        };
        assert!(!is_cancelled(&armed));
        flag.store(true, Ordering::SeqCst);
        assert!(is_cancelled(&armed));
    }

    #[test]
    fn pre_cancelled_burn_does_not_touch_transport() {
        let img = vec![0xABu8; 4096];
        let crc = crc32fast::hash(&img[0..2048]);
        let metas = vec![make_meta("image.target.os", "os", 0, 2048, crc)];
        let flag = Arc::new(AtomicBool::new(true));
        let options = BurnOptions {
            selected_parts: vec!["os".to_string()],
            reset_after_burn: false,
            burn_timeout: Duration::from_secs(5),
            cancel: Some(flag),
            force_upgrade: false,
        };
        let mut dev = UpgDevice::new(MockTransport {
            writes: 0,
            reads: 0,
            panic_on_use: true,
            max_chunk: 512,
            upg_modes: Vec::new(),
            saw_reset: false,
            payloads: Vec::new(),
        });
        let mut events = Vec::new();
        let mut cb = |e: BurnEvent| events.push(format!("{:?}", e));
        let err = dev
            .burn_image_with_options(&img, &metas, &options, Some(&mut cb))
            .expect_err("pre-cancelled burn must fail");
        assert!(err.contains("cancelled"), "unexpected error: {}", err);
    }

    #[test]
    fn cancel_mid_chunk_aborts_burn() {
        let img = vec![0xABu8; 4096];
        let crc = crc32fast::hash(&img[0..2048]);
        let metas = vec![make_meta("image.target.os", "os", 0, 2048, crc)];
        let flag = Arc::new(AtomicBool::new(false));
        let options = BurnOptions {
            selected_parts: vec!["os".to_string()],
            reset_after_burn: false,
            burn_timeout: Duration::from_secs(5),
            cancel: Some(flag.clone()),
            force_upgrade: false,
        };
        let mut dev = UpgDevice::new(MockTransport {
            writes: 0,
            reads: 0,
            panic_on_use: false,
            max_chunk: 512,
            upg_modes: Vec::new(),
            saw_reset: false,
            payloads: Vec::new(),
        });
        let mut progress_events = 0usize;
        let mut cb = |event: BurnEvent| {
            if matches!(event, BurnEvent::ComponentProgress { .. }) {
                progress_events += 1;
                if progress_events >= 1 {
                    flag.store(true, Ordering::SeqCst);
                }
            }
        };
        let err = dev
            .burn_image_with_options(&img, &metas, &options, Some(&mut cb))
            .expect_err("mid-burn cancel must fail");
        assert!(err.contains("cancelled"), "unexpected error: {}", err);
        assert!(
            dev.transport_mut().writes >= 1,
            "expected at least one chunk before cancel"
        );
    }

    #[test]
    fn upg_mode_selects_force_upgrade_byte() {
        assert_eq!(upg_mode(&BurnOptions::default()), UPG_MODE_FULL_DISK_UPGRADE);
        let force = BurnOptions {
            force_upgrade: true,
            ..Default::default()
        };
        assert_eq!(upg_mode(&force), UPG_MODE_BURN_IMG_FORCE);
    }

    #[test]
    fn default_burn_uses_full_disk_mode_and_resets() {
        let img = vec![0xABu8; 1024];
        let crc = crc32fast::hash(&img[0..1024]);
        let metas = vec![make_meta("image.target.os", "os", 0, 1024, crc)];
        let options = BurnOptions {
            selected_parts: vec!["os".to_string()],
            reset_after_burn: true,
            burn_timeout: Duration::from_secs(5),
            cancel: None,
            force_upgrade: false,
        };
        let mut dev = UpgDevice::new(MockTransport {
            writes: 0,
            reads: 0,
            panic_on_use: false,
            max_chunk: 4096,
            upg_modes: Vec::new(),
            saw_reset: false,
            payloads: Vec::new(),
        });
        dev.burn_image_with_options(&img, &metas, &options, None)
            .expect("mock burn must succeed");
        assert_eq!(dev.transport_mut().upg_modes, vec![UPG_MODE_FULL_DISK_UPGRADE]);
        assert!(
            dev.transport_mut().saw_reset,
            "default burn must reset the device"
        );
    }

    #[test]
    fn force_upgrade_uses_force_mode_and_skips_reset() {
        let img = vec![0xABu8; 1024];
        let crc = crc32fast::hash(&img[0..1024]);
        let metas = vec![make_meta("image.target.os", "os", 0, 1024, crc)];
        // Even with reset_after_burn=true, force upgrade wins (mutually exclusive).
        let options = BurnOptions {
            selected_parts: vec!["os".to_string()],
            reset_after_burn: true,
            burn_timeout: Duration::from_secs(5),
            cancel: None,
            force_upgrade: true,
        };
        let mut dev = UpgDevice::new(MockTransport {
            writes: 0,
            reads: 0,
            panic_on_use: false,
            max_chunk: 4096,
            upg_modes: Vec::new(),
            saw_reset: false,
            payloads: Vec::new(),
        });
        let mut logs = Vec::new();
        let mut cb = |e: BurnEvent| {
            if let BurnEvent::Log(line) = e {
                logs.push(line);
            }
        };
        dev.burn_image_with_options(&img, &metas, &options, Some(&mut cb))
            .expect("mock force burn must succeed");
        assert_eq!(dev.transport_mut().upg_modes, vec![UPG_MODE_BURN_IMG_FORCE]);
        assert!(
            !dev.transport_mut().saw_reset,
            "force upgrade must skip the post-burn reset"
        );
        assert!(
            logs.iter().any(|l| l.contains("skipping reset")),
            "expected a skipping-reset note, got: {:?}",
            logs
        );
    }

    fn mock_device() -> UpgDevice<MockTransport> {
        UpgDevice::new(MockTransport {
            writes: 0,
            reads: 0,
            panic_on_use: false,
            max_chunk: 4096,
            upg_modes: Vec::new(),
            saw_reset: false,
            payloads: Vec::new(),
        })
    }

    #[test]
    fn write_memory_sends_addr_len_data() {
        let mut dev = mock_device();
        dev.write_memory(0x41000000, &[0xAA, 0xBB]).unwrap();
        // writes[0] = 16-byte cmd header, writes[1] = command payload.
        assert_eq!(dev.transport_mut().payloads.len(), 2);
        assert_eq!(
            dev.transport_mut().payloads[1],
            vec![0x00, 0x00, 0x00, 0x41, 0x02, 0x00, 0x00, 0x00, 0xAA, 0xBB]
        );
    }

    #[test]
    fn read_memory_sends_addr_len_and_returns_len_bytes() {
        // NOTE: len 0x10 would collide with the mock's 16-byte RESP-header
        // special case; 0x20 exercises the generic zero-fill path.
        let mut dev = mock_device();
        let data = dev.read_memory(0x40000000, 0x20).unwrap();
        assert_eq!(data, vec![0u8; 0x20]);
        assert_eq!(
            dev.transport_mut().payloads[1],
            vec![0x00, 0x00, 0x00, 0x40, 0x20, 0x00, 0x00, 0x00]
        );
    }

    #[test]
    fn exec_address_sends_four_byte_addr() {
        let mut dev = mock_device();
        dev.exec_address(0x41000100).unwrap();
        assert_eq!(
            dev.transport_mut().payloads[1],
            vec![0x00, 0x01, 0x00, 0x41]
        );
    }

    #[test]
    fn run_shell_rejects_overlong_commands() {
        let mut dev = mock_device();
        assert!(dev.run_shell(&"x".repeat(128)).is_err());
        // Rejected before touching the transport.
        assert!(dev.transport_mut().payloads.is_empty());
    }

    #[test]
    fn storage_geometry_and_erase_wire_shapes() {
        let mut dev = mock_device();
        let media = StorageMedia::new("spi-nand", 0);
        let geo = dev.get_storage_geometry(&media).unwrap();
        assert_eq!(geo.total_size, 0);
        let payloads = &dev.transport_mut().payloads;
        assert_eq!(payloads.len(), 2);
        assert_eq!(payloads[1].len(), STORAGE_MEDIA_SIZE);
        assert_eq!(&payloads[1][..9], b"spi-nand\0");

        let mut dev = mock_device();
        dev.erase_storage(&StorageEraseArgs {
            media: StorageMedia::new("spi-nand", 0),
            start: 0,
            size: 0x0800_0000,
            flag: 0,
        })
        .unwrap();
        let payloads = &dev.transport_mut().payloads;
        assert_eq!(payloads.len(), 2);
        assert_eq!(payloads[1].len(), STORAGE_ERASE_ARGS_SIZE);
        assert_eq!(
            u64::from_le_bytes(payloads[1][80..88].try_into().unwrap()),
            0x0800_0000
        );
    }

    #[test]
    fn list_storage_media_parses_68_byte_chunks() {
        let mut dev = mock_device();
        // Mock zero-fills the 5*68-byte read: 5 empty entries (chunking
        // math: 340/68 = 5, no partial tail).
        assert_eq!(
            dev.list_storage_media().unwrap(),
            vec![StorageMedia::new("", 0); 5]
        );
    }
}
