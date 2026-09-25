#![allow(dead_code)]

pub const AIC_USB_SIGN_USBC: u32 = 0x43425355;
pub const AIC_USB_SIGN_USBS: u32 = 0x53425355;
pub const AIC_UPG_SIGN_UPGC: u32 = 0x43475055;
pub const AIC_UPG_SIGN_UPGR: u32 = 0x52475055;

pub const TRANS_LAYER_CMD_WRITE: u8 = 0x01;
pub const TRANS_LAYER_CMD_READ: u8 = 0x02;

pub const RESP_MIN_HDR_LEN: usize = 16;
pub const RESP_LEGACY_HDR_LEN: usize = 24;
pub const CMD_HDR_LEN: usize = 16;

/// CBW - Command Block Wrapper (31 bytes)
pub struct AicCbw {
    bytes: [u8; 31],
}

impl AicCbw {
    pub fn new_write(tag: u32, data_len: u32) -> Self {
        let mut b = [0u8; 31];
        b[0..4].copy_from_slice(&AIC_USB_SIGN_USBC.to_le_bytes());
        b[4..8].copy_from_slice(&tag.to_le_bytes());
        b[8..12].copy_from_slice(&data_len.to_le_bytes());
        // flags = 0x00 (host->dev)
        b[14] = 1; // cb_length
        b[15] = TRANS_LAYER_CMD_WRITE;
        Self { bytes: b }
    }

    pub fn new_read(tag: u32, data_len: u32) -> Self {
        let mut b = [0u8; 31];
        b[0..4].copy_from_slice(&AIC_USB_SIGN_USBC.to_le_bytes());
        b[4..8].copy_from_slice(&tag.to_le_bytes());
        b[8..12].copy_from_slice(&data_len.to_le_bytes());
        b[12] = 0x80; // flags = dev->host
        b[14] = 1; // cb_length
        b[15] = TRANS_LAYER_CMD_READ;
        Self { bytes: b }
    }

    pub fn to_bytes(&self) -> &[u8] {
        &self.bytes
    }
}

/// CSW - Command Status Wrapper (13 bytes)
#[derive(Clone, Debug)]
pub struct AicCsw {
    bytes: [u8; 13],
}

impl AicCsw {
    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        if bytes.len() < 13 {
            return None;
        }
        let mut b = [0u8; 13];
        b.copy_from_slice(&bytes[..13]);
        Some(Self { bytes: b })
    }

    pub fn signature(&self) -> u32 {
        u32::from_le_bytes(self.bytes[0..4].try_into().unwrap())
    }

    pub fn tag_val(&self) -> u32 {
        u32::from_le_bytes(self.bytes[4..8].try_into().unwrap())
    }

    pub fn data_residue_val(&self) -> u32 {
        u32::from_le_bytes(self.bytes[8..12].try_into().unwrap())
    }

    pub fn status_val(&self) -> u8 {
        self.bytes[12]
    }

    pub fn is_ok(&self) -> bool {
        self.signature() == AIC_USB_SIGN_USBS && self.status_val() == 0
    }
}

/// UPG Command Header (16 bytes)
pub struct CmdHeader {
    pub bytes: [u8; CMD_HDR_LEN],
}

impl CmdHeader {
    pub fn new(command: u8, data_length: u32) -> Self {
        let mut b = [0u8; CMD_HDR_LEN];
        b[0..4].copy_from_slice(&AIC_UPG_SIGN_UPGC.to_le_bytes()); // "UPGC"
        b[4] = 0x01; // protocol
        b[5] = 0x01; // version
        b[6] = command;
        // b[7] = reserved
        b[8..12].copy_from_slice(&data_length.to_le_bytes());

        // checksum = magic + (reserved<<24|command<<16|version<<8|protocol) + data_length
        let mut sum: u32 = 0;
        sum = sum.wrapping_add(AIC_UPG_SIGN_UPGC);
        sum = sum.wrapping_add((command as u32) << 16 | (0x01u32 << 8) | 0x01u32);
        sum = sum.wrapping_add(data_length);
        b[12..16].copy_from_slice(&sum.to_le_bytes());

        Self { bytes: b }
    }

    pub fn to_bytes(&self) -> &[u8] {
        &self.bytes
    }
}

/// UPG Response Header.
///
/// Official AiBurn logs show reads that expect a 16-byte RESP header, while
/// older reverse-engineered code treated it as 24 bytes. Keep parsing based on
/// the stable 16-byte prefix and let callers tolerate legacy padding.
#[derive(Clone, Debug)]
pub struct RespHeader {
    bytes: [u8; RESP_MIN_HDR_LEN],
}

impl RespHeader {
    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        if bytes.len() < RESP_MIN_HDR_LEN {
            return None;
        }
        let mut b = [0u8; RESP_MIN_HDR_LEN];
        b.copy_from_slice(&bytes[..RESP_MIN_HDR_LEN]);
        Some(Self { bytes: b })
    }

    pub fn magic(&self) -> u32 {
        u32::from_le_bytes(self.bytes[0..4].try_into().unwrap())
    }

    pub fn protocol(&self) -> u8 {
        self.bytes[4]
    }

    pub fn version(&self) -> u8 {
        self.bytes[5]
    }

    pub fn command(&self) -> u8 {
        self.bytes[6]
    }

    pub fn status_val(&self) -> u8 {
        self.bytes[7]
    }

    pub fn data_length_val(&self) -> u32 {
        u32::from_le_bytes(self.bytes[8..12].try_into().unwrap())
    }

    pub fn checksum_val(&self) -> u32 {
        u32::from_le_bytes(self.bytes[12..16].try_into().unwrap())
    }

    pub fn is_ok(&self) -> bool {
        self.magic() == AIC_UPG_SIGN_UPGR && self.status_val() == 0
    }
}

/// HWINFO response data
pub struct HwInfo {
    bytes: Vec<u8>,
}

impl HwInfo {
    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        if bytes.len() < 104 {
            return None;
        }
        Some(Self {
            bytes: bytes.to_vec(),
        })
    }

    pub fn magic_str(&self) -> &str {
        std::str::from_utf8(&self.bytes[0..8])
            .unwrap_or("")
            .trim_end_matches('\0')
    }

    pub fn chipid_val(&self) -> [u32; 4] {
        let mut ids = [0u32; 4];
        for (i, id) in ids.iter_mut().enumerate() {
            let off = 48 + i * 4;
            *id = u32::from_le_bytes(self.bytes[off..off + 4].try_into().unwrap());
        }
        ids
    }

    pub fn init_mode(&self) -> u32 {
        u32::from_le_bytes(self.bytes[8..12].try_into().unwrap())
    }

    pub fn curr_mode(&self) -> u32 {
        u32::from_le_bytes(self.bytes[12..16].try_into().unwrap())
    }

    pub fn boot_stage(&self) -> u32 {
        u32::from_le_bytes(self.bytes[16..20].try_into().unwrap())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_csw_status_and_tag() {
        let mut bytes = [0u8; 13];
        bytes[0..4].copy_from_slice(&AIC_USB_SIGN_USBS.to_le_bytes());
        bytes[4..8].copy_from_slice(&7u32.to_le_bytes());
        bytes[12] = 0;

        let csw = AicCsw::from_bytes(&bytes).unwrap();
        assert!(csw.is_ok());
        assert_eq!(csw.tag_val(), 7);
        assert_eq!(csw.status_val(), 0);
    }

    #[test]
    fn parses_minimal_upg_response_header() {
        let mut bytes = [0u8; RESP_MIN_HDR_LEN];
        bytes[0..4].copy_from_slice(&AIC_UPG_SIGN_UPGR.to_le_bytes());
        bytes[4] = 1;
        bytes[5] = 1;
        bytes[6] = 0x16;
        bytes[7] = 0;
        bytes[8..12].copy_from_slice(&64u32.to_le_bytes());

        let resp = RespHeader::from_bytes(&bytes).unwrap();
        assert!(resp.is_ok());
        assert_eq!(resp.command(), 0x16);
        assert_eq!(resp.data_length_val(), 64);
    }

    #[test]
    fn builds_official_compatible_upg_command_header() {
        let hdr = CmdHeader::new(0x12, 30480);
        assert_eq!(hdr.to_bytes().len(), 16);
        assert_eq!(
            hdr.to_bytes(),
            [
                0x55, 0x50, 0x47, 0x43, 0x01, 0x01, 0x12, 0x00, 0x10, 0x77, 0x00, 0x00, 0x66, 0xc8,
                0x59, 0x43,
            ]
        );
    }

    #[test]
    fn builds_official_compatible_cbw_command_length() {
        let write = AicCbw::new_write(0xc8, 16);
        assert_eq!(write.to_bytes()[14], 1);
        assert_eq!(write.to_bytes()[15], TRANS_LAYER_CMD_WRITE);

        let read = AicCbw::new_read(0xc9, 16);
        assert_eq!(read.to_bytes()[12], 0x80);
        assert_eq!(read.to_bytes()[14], 1);
        assert_eq!(read.to_bytes()[15], TRANS_LAYER_CMD_READ);
    }
}
