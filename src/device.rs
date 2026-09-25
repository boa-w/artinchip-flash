use std::thread;
use std::time::Duration;

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
            if let Err(e) = self.transport.reconnect(options.burn_timeout) {
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
        let base_chunk = if component.kind == ComponentKind::Updater {
            (block_size as usize).saturating_mul(512).max(512)
        } else {
            CHUNK_SIZE as usize
        };
        let transport_chunk = self.transport.max_write_chunk(block_size);
        let chunk_max = base_chunk.min(transport_chunk).max(1);
        eprintln!(
            "    Write chunk: {} bytes ({})",
            chunk_max,
            self.transport.transport_name()
        );
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
