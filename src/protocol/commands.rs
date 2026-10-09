#![allow(dead_code)]

pub const CMD_GET_HWINFO: u8 = 0x00;
pub const CMD_GET_TRACEINFO: u8 = 0x01;
pub const CMD_WRITE: u8 = 0x02;
pub const CMD_READ: u8 = 0x03;
pub const CMD_EXEC: u8 = 0x04;
pub const CMD_RUN_SHELL_STR: u8 = 0x05;
pub const CMD_GET_MEM_BUF: u8 = 0x08;
pub const CMD_FREE_MEM_BUF: u8 = 0x09;
pub const CMD_SET_UPG_CFG: u8 = 0x0A;
pub const CMD_SET_UPG_END: u8 = 0x0B;
pub const CMD_GET_LOG_SIZE: u8 = 0x0C;
pub const CMD_GET_LOG_DATA: u8 = 0x0D;
pub const CMD_SET_FWC_META: u8 = 0x10;
pub const CMD_GET_BLOCK_SIZE: u8 = 0x11;
pub const CMD_SEND_FWC_DATA: u8 = 0x12;
pub const CMD_GET_FWC_CRC: u8 = 0x13;
pub const CMD_GET_FWC_BURN_RESULT: u8 = 0x14;
pub const CMD_GET_FWC_RUN_RESULT: u8 = 0x15;
pub const CMD_GET_STORAGE_MEDIA: u8 = 0x16;
pub const CMD_GET_PARTITION_TABLE: u8 = 0x17;
pub const CMD_READ_FWC_DATA: u8 = 0x18;
pub const CMD_SET_UART_ARGS: u8 = 0x19;
pub const CMD_GET_STORAGE_GEOME: u8 = 0x1A;
pub const CMD_ERASE_STORAGE: u8 = 0x1B;

pub const UPG_MODE_FULL_DISK_UPGRADE: u8 = 0x00;
pub const UPG_MODE_PARTITION_UPGRADE: u8 = 0x01;
pub const UPG_MODE_BURN_USER_ID: u8 = 0x02;
pub const UPG_MODE_DUMP_PARTITION: u8 = 0x03;
pub const UPG_MODE_BURN_IMG_FORCE: u8 = 0x04;
pub const UPG_MODE_BURN_FROZEN: u8 = 0x05;

pub const FWC_META_SIZE: usize = 512;

/// FWC Meta entry (512 bytes)
#[derive(Clone, Debug)]
pub struct FwcMeta {
    pub bytes: Vec<u8>,
}

impl FwcMeta {
    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        if bytes.len() < FWC_META_SIZE {
            return None;
        }
        Some(Self {
            bytes: bytes[..FWC_META_SIZE].to_vec(),
        })
    }

    pub fn magic_str(&self) -> &str {
        std::str::from_utf8(&self.bytes[0..8])
            .unwrap_or("")
            .trim_end_matches('\0')
    }

    pub fn name_str(&self) -> &str {
        std::str::from_utf8(&self.bytes[8..72])
            .unwrap_or("")
            .trim_end_matches('\0')
    }

    /// Partition name: NUL-padded C string at field `[0..34)`.
    ///
    /// The 64-byte partition field (bytes `[72..136)`) is a **struct**, not a
    /// plain C string — newer Luban SDK `mk_image.py` packs:
    ///
    /// ```text
    /// [0..34)   part name, NUL-padded ("spl"; empty for RAM/updater entries)
    /// [34..38)  u32 LE partition start in 64 KiB units
    /// [38..40)  u16 LE partition size in 64 KiB units
    /// [40..64)  media type, NUL-padded ("spi-nand"; updater entries store
    ///           "ram" here with an empty name)
    /// ```
    ///
    /// Evidence: `d13x_D50T-2-Lite_page_2k_block_128k_v1.0.0.img`
    /// (starts 0,16,20,24,152,600 and sizes 16,4,4,64,224,640 match
    /// `partition.json`'s `1m(spl),256k(env),256k(env_r),4m(os),…` exactly,
    /// including the `_r` gaps of on-flash-only partitions).
    ///
    /// Reading the whole 64 bytes as one string leaks the middle integers
    /// into the UI (control-char tofu like `spl□spi-nand`, or `""` when a
    /// size byte is invalid UTF-8, e.g. rodata/data). Callers that only need
    /// the name should use this; UI labels should use
    /// [`FwcMeta::partition_display`].
    pub fn partition_str(&self) -> &str {
        self.partition_name()
    }

    /// Partition name part of the field (`[0..34)` C string).
    pub fn partition_name(&self) -> &str {
        cstr(&self.bytes[72..106])
    }

    /// Media type part of the field (`[40..64)` C string, e.g. `"spi-nand"`).
    /// Updater/RAM entries store `"ram"` here with an empty name; legacy
    /// images with a plain-string partition leave this empty.
    pub fn partition_media(&self) -> &str {
        cstr(&self.bytes[112..136])
    }

    /// Partition start in 64 KiB units (u32 LE at `[34..38)`).
    pub fn partition_start_64k(&self) -> u32 {
        u32::from_le_bytes(self.bytes[106..110].try_into().unwrap())
    }

    /// Partition size in 64 KiB units (u16 LE at `[38..40)`).
    pub fn partition_size_64k(&self) -> u16 {
        u16::from_le_bytes(self.bytes[110..112].try_into().unwrap())
    }

    /// Human-readable partition label for UI/CLI output.
    ///
    /// - name + media → `"spl@spi-nand"`
    /// - media only (updater/RAM entries) → `"ram"`
    /// - name only (legacy plain-string images) → `"spl"`
    /// - neither → `""`
    pub fn partition_display(&self) -> String {
        match (self.partition_name(), self.partition_media()) {
            ("", "") => String::new(),
            ("", media) => media.to_string(),
            (name, "") => name.to_string(),
            (name, media) => format!("{}@{}", name, media),
        }
    }

    pub fn offset_val(&self) -> u32 {
        u32::from_le_bytes(self.bytes[136..140].try_into().unwrap())
    }

    pub fn size_val(&self) -> u32 {
        u32::from_le_bytes(self.bytes[140..144].try_into().unwrap())
    }

    pub fn crc_val(&self) -> u32 {
        u32::from_le_bytes(self.bytes[144..148].try_into().unwrap())
    }

    pub fn ram_val(&self) -> u32 {
        u32::from_le_bytes(self.bytes[148..152].try_into().unwrap())
    }

    pub fn attr_str(&self) -> &str {
        std::str::from_utf8(&self.bytes[152..216])
            .unwrap_or("")
            .trim_end_matches('\0')
    }

    pub fn to_bytes(&self) -> &[u8] {
        &self.bytes
    }
}

/// NUL-padded C string slice: invalid UTF-8 yields `""` rather than failing
/// the whole entry (middle struct bytes may be non-text).
fn cstr(bytes: &[u8]) -> &str {
    std::str::from_utf8(bytes).unwrap_or("").trim_end_matches('\0')
}

/// Write `bytes` as a NUL-padded C string field of exactly `len` bytes.
/// Longer inputs are truncated to `len - 1` bytes so the field stays
/// NUL-terminated (mirrors the device-side `strcpy` into fixed buffers for
/// our short `media_type`/`part`/`attr` values).
fn put_cstr(out: &mut [u8], bytes: &[u8]) {
    let len = out.len();
    let copy_len = bytes.len().min(len.saturating_sub(1));
    out[..copy_len].copy_from_slice(&bytes[..copy_len]);
    // `out` starts zeroed by callers; the byte after the copy stays NUL.
}

pub const STORAGE_MEDIA_SIZE: usize = 68;
pub const STORAGE_GEOMETRY_SIZE: usize = 24;
pub const STORAGE_ERASE_ARGS_SIZE: usize = 96;
pub const MEDIA_PARTITION_SIZE: usize = 300;

/// Device-side `struct storage_media` (68 B, `__packed`):
/// `char media_type[64]` (`"mmc"`/`"spi-nand"`/`"spi-nor"`) + `u32 media_dev_id`.
///
/// Evidence: `aicupg.h` + `fwc_cmd.c` `get_storage_media_cmd_*` /
/// `get_partition_table_cmd_*` / `get_storage_geome_cmd_*` /
/// `erase_storage_cmd_*` (all `memcpy` a 68-byte media prefix).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StorageMedia {
    pub media_type: String,
    pub media_dev_id: u32,
}

impl StorageMedia {
    pub fn new(media_type: &str, media_dev_id: u32) -> Self {
        Self {
            media_type: media_type.to_string(),
            media_dev_id,
        }
    }

    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = vec![0u8; STORAGE_MEDIA_SIZE];
        put_cstr(&mut out[..64], self.media_type.as_bytes());
        out[64..68].copy_from_slice(&self.media_dev_id.to_le_bytes());
        out
    }

    /// Split a `GET_STORAGE_MEDIA` payload into 68-byte entries.
    /// A trailing partial chunk is dropped.
    pub fn parse_list(payload: &[u8]) -> Vec<StorageMedia> {
        payload
            .chunks_exact(STORAGE_MEDIA_SIZE)
            .map(|chunk| StorageMedia {
                media_type: cstr(&chunk[..64]).to_string(),
                media_dev_id: u32::from_le_bytes(chunk[64..68].try_into().unwrap()),
            })
            .collect()
    }
}

/// Device-side `struct storage_geometry` (24 B, `__packed`):
/// `u64 total_size` + `u32 erase_size, write_size, oob_size, rsvd`.
///
/// Evidence: `fwc_cmd.c get_storage_geome_cmd_read_output_data`
/// (`siz=sizeof(storage_geometry)`, 24 data bytes). Units: total/erase in
/// bytes on Flash, sectors on MMC; write in bytes; oob NAND-only.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StorageGeometry {
    pub total_size: u64,
    pub erase_size: u32,
    pub write_size: u32,
    pub oob_size: u32,
}

impl StorageGeometry {
    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        if bytes.len() < STORAGE_GEOMETRY_SIZE {
            return None;
        }
        Some(Self {
            total_size: u64::from_le_bytes(bytes[0..8].try_into().unwrap()),
            erase_size: u32::from_le_bytes(bytes[8..12].try_into().unwrap()),
            write_size: u32::from_le_bytes(bytes[12..16].try_into().unwrap()),
            oob_size: u32::from_le_bytes(bytes[16..20].try_into().unwrap()),
        })
    }
}

/// Device-side `struct storage_erase_args` (96 B, `__packed`):
/// `struct storage_media media` (68) + `u32 pad` (alignment filler) +
/// `struct storage_erase` (24: `u64 start, size, flag`).
///
/// Evidence: `fwc_cmd.c:1374-1378` + `erase_storage_cmd_write_input_data`
/// (`memcpy(erase_info, buf, sizeof(*erase_info))`, erase runs synchronously
/// in the read path). Units follow the geometry command (bytes on Flash,
/// sectors on MMC).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StorageEraseArgs {
    pub media: StorageMedia,
    pub start: u64,
    pub size: u64,
    pub flag: u64,
}

impl StorageEraseArgs {
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = vec![0u8; STORAGE_ERASE_ARGS_SIZE];
        out[..STORAGE_MEDIA_SIZE].copy_from_slice(&self.media.to_bytes());
        // [68..72) stays zero (pad).
        out[72..80].copy_from_slice(&self.start.to_le_bytes());
        out[80..88].copy_from_slice(&self.size.to_le_bytes());
        out[88..96].copy_from_slice(&self.flag.to_le_bytes());
        out
    }
}

/// Host-side replica of device `struct media_partition` for
/// `READ_FWC_DATA` (0x18): `struct storage_media media` (68) +
/// `struct aic_partition part` (`char name[144]` + `int index` +
/// `u64 start` + `u64 size` + pointer `next`, 168 B with `next = 0`) +
/// `char attr[64]`: 300 B total, naturally aligned (u64 `start` lands at
/// offset 216).
///
/// Evidence: `fwc_cmd.c read_fwc_data_cmd_write_input_data`
/// (`memcpy(&fwc->mpart, buf, sizeof(media_partition))`, then
/// `meta.partition = mpart.part.name`, `meta.offset = mpart.part.start`,
/// `meta.size = mpart.part.size`). `start`/`size` are byte offsets.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MediaPartitionArgs {
    pub media: StorageMedia,
    pub part_name: String,
    pub part_index: i32,
    pub part_start: u64,
    pub part_size: u64,
    pub attr: String,
}

impl MediaPartitionArgs {
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = vec![0u8; MEDIA_PARTITION_SIZE];
        out[..68].copy_from_slice(&self.media.to_bytes());
        put_cstr(&mut out[68..212], self.part_name.as_bytes());
        out[212..216].copy_from_slice(&self.part_index.to_le_bytes());
        out[216..224].copy_from_slice(&self.part_start.to_le_bytes());
        out[224..232].copy_from_slice(&self.part_size.to_le_bytes());
        // [232..236) stays zero (null `next` pointer).
        put_cstr(&mut out[236..300], self.attr.as_bytes());
        out
    }
}

/// `WRITE` (0x02) payload: `u32 LE addr` + `u32 LE len` + `len` data bytes.
///
/// Evidence: `basic_cmd.c write_cmd_write_input_data` (`memcpy` addr/len,
/// then `hw_friendly_memcpy` of the remainder; multi-packet accumulation).
pub fn write_mem_request(addr: u32, data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(8 + data.len());
    out.extend_from_slice(&addr.to_le_bytes());
    out.extend_from_slice(&(data.len() as u32).to_le_bytes());
    out.extend_from_slice(data);
    out
}

/// `READ` (0x03) payload: `u32 LE addr` + `u32 LE len` (exactly 8 B).
///
/// Evidence: `basic_cmd.c read_cmd_write_input_data`.
pub fn read_mem_request(addr: u32, len: u32) -> Vec<u8> {
    let mut out = Vec::with_capacity(8);
    out.extend_from_slice(&addr.to_le_bytes());
    out.extend_from_slice(&len.to_le_bytes());
    out
}

/// `EXEC` (0x04) payload: `u32 LE addr` (function pointer, exactly 4 B).
///
/// Evidence: `basic_cmd.c exec_cmd_write_input_data` (executes during the
/// write phase; `addr == 0` fails).
pub fn exec_request(addr: u32) -> Vec<u8> {
    addr.to_le_bytes().to_vec()
}

/// Device-side shell buffer is 128 B (`MAX_SHELL_CMD_STR_LEN`); `cmdlen >= 128`
/// fails, and the whole string must arrive in one packet.
///
/// Evidence: `basic_cmd.c run_shell_str_cmd_write_input_data`.
pub const MAX_SHELL_CMD_LEN: usize = 127;

/// `RUN_SHELL_STR` (0x05) payload: `u32 LE cmdlen` + `cmdlen` command bytes.
pub fn shell_request(cmd: &str) -> Result<Vec<u8>, String> {
    let bytes = cmd.as_bytes();
    if bytes.len() > MAX_SHELL_CMD_LEN {
        return Err(format!(
            "Shell command too long: {} bytes (max {})",
            bytes.len(),
            MAX_SHELL_CMD_LEN
        ));
    }
    let mut out = Vec::with_capacity(4 + bytes.len());
    out.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
    out.extend_from_slice(bytes);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a 512-byte META with the given partition-field struct parts.
    fn meta_with_partition(name_field: &[u8], mid: [u8; 6], media_field: &[u8]) -> FwcMeta {
        let mut bytes = vec![0u8; FWC_META_SIZE];
        bytes[0..8].copy_from_slice(b"META\x00\x00\x00\x00");
        bytes[8..8 + "image.target.spl".len()].copy_from_slice(b"image.target.spl");
        bytes[72..72 + name_field.len()].copy_from_slice(name_field);
        bytes[106..112].copy_from_slice(&mid);
        bytes[112..112 + media_field.len()].copy_from_slice(media_field);
        FwcMeta::from_bytes(&bytes).unwrap()
    }

    #[test]
    fn partition_field_parses_as_struct() {
        // Real bytes: image.target.spl entry of
        // d13x_D50T-2-Lite_page_2k_block_128k_v1.0.0.img.
        let meta = meta_with_partition(b"spl\0", [0, 0, 0, 0, 0x10, 0], b"spi-nand\0");
        assert_eq!(meta.partition_name(), "spl");
        assert_eq!(meta.partition_str(), "spl");
        assert_eq!(meta.partition_media(), "spi-nand");
        assert_eq!(meta.partition_display(), "spl@spi-nand");
        assert_eq!(meta.partition_start_64k(), 0);
        assert_eq!(meta.partition_size_64k(), 16); // 16 * 64KiB = 1m(spl)
    }

    #[test]
    fn partition_middle_bytes_match_mtd_layout() {
        // image.target.os: start 24, size 64 (4m), from the same image.
        let meta = meta_with_partition(b"os\0", [0x18, 0, 0, 0, 0x40, 0], b"spi-nand\0");
        assert_eq!(meta.partition_display(), "os@spi-nand");
        assert_eq!(meta.partition_start_64k(), 24);
        assert_eq!(meta.partition_size_64k(), 64);
    }

    #[test]
    fn partition_non_utf8_size_byte_does_not_blank_the_name() {
        // image.target.rodata: size_lo 0x98 is a lone UTF-8 continuation
        // byte; the old whole-field parse returned "" for the row.
        let meta = meta_with_partition(b"rodata\0", [0x98, 0, 0, 0, 0xE0, 0], b"spi-nand\0");
        assert_eq!(meta.partition_name(), "rodata");
        assert_eq!(meta.partition_display(), "rodata@spi-nand");
        assert_eq!(meta.partition_size_64k(), 0xE0); // 224 * 64KiB = 14m
    }

    #[test]
    fn updater_entry_with_empty_name_shows_media_only() {
        // image.updater.* entries: empty name, "ram" in the media slot.
        let meta = meta_with_partition(b"\0", [0, 0, 0, 0, 0, 0], b"ram\0");
        assert_eq!(meta.partition_name(), "");
        assert_eq!(meta.partition_media(), "ram");
        assert_eq!(meta.partition_display(), "ram");
    }

    #[test]
    fn legacy_plain_string_partition_still_works() {
        // Pre-struct images: plain C string at the field start, rest zeros.
        let meta = meta_with_partition(b"ram\0", [0, 0, 0, 0, 0, 0], b"\0");
        assert_eq!(meta.partition_name(), "ram");
        assert_eq!(meta.partition_media(), "");
        assert_eq!(meta.partition_display(), "ram");
    }

    #[test]
    fn storage_media_round_trips_68_bytes() {
        let media = StorageMedia::new("spi-nand", 1);
        let bytes = media.to_bytes();
        assert_eq!(bytes.len(), STORAGE_MEDIA_SIZE);
        assert_eq!(&bytes[..9], b"spi-nand\0");
        assert_eq!(u32::from_le_bytes(bytes[64..68].try_into().unwrap()), 1);
        let parsed = StorageMedia::parse_list(&bytes);
        assert_eq!(parsed, vec![media]);
        // Two entries plus a trailing partial chunk: partial tail dropped.
        let mut two = bytes.clone();
        two.extend_from_slice(&bytes);
        two.extend_from_slice(&[0u8; 10]);
        assert_eq!(StorageMedia::parse_list(&two).len(), 2);
    }

    #[test]
    fn storage_geometry_parses_24_bytes() {
        let mut bytes = vec![0u8; STORAGE_GEOMETRY_SIZE];
        bytes[0..8].copy_from_slice(&0x0800_0000u64.to_le_bytes());
        bytes[8..12].copy_from_slice(&0x20000u32.to_le_bytes());
        bytes[12..16].copy_from_slice(&0x800u32.to_le_bytes());
        bytes[16..20].copy_from_slice(&64u32.to_le_bytes());
        let geo = StorageGeometry::from_bytes(&bytes).unwrap();
        assert_eq!(geo.total_size, 0x0800_0000);
        assert_eq!(geo.erase_size, 0x20000);
        assert_eq!(geo.write_size, 0x800);
        assert_eq!(geo.oob_size, 64);
        assert!(StorageGeometry::from_bytes(&bytes[..23]).is_none());
    }

    #[test]
    fn erase_args_pack_to_96_bytes() {
        let args = StorageEraseArgs {
            media: StorageMedia::new("spi-nand", 0),
            start: 0,
            size: 0x0800_0000,
            flag: 0,
        };
        let bytes = args.to_bytes();
        assert_eq!(bytes.len(), STORAGE_ERASE_ARGS_SIZE);
        assert_eq!(&bytes[..9], b"spi-nand\0");
        assert_eq!(&bytes[68..72], &[0, 0, 0, 0]); // pad
        assert_eq!(u64::from_le_bytes(bytes[72..80].try_into().unwrap()), 0);
        assert_eq!(
            u64::from_le_bytes(bytes[80..88].try_into().unwrap()),
            0x0800_0000
        );
        assert_eq!(&bytes[88..96], &[0; 8]);
    }

    #[test]
    fn media_partition_packs_to_300_bytes() {
        let args = MediaPartitionArgs {
            media: StorageMedia::new("spi-nand", 0),
            part_name: "os".to_string(),
            part_index: 3,
            part_start: 0x600000,
            part_size: 0x400000,
            attr: "mtd".to_string(),
        };
        let bytes = args.to_bytes();
        assert_eq!(bytes.len(), MEDIA_PARTITION_SIZE);
        assert_eq!(&bytes[68..71], b"os\0");
        assert_eq!(i32::from_le_bytes(bytes[212..216].try_into().unwrap()), 3);
        assert_eq!(
            u64::from_le_bytes(bytes[216..224].try_into().unwrap()),
            0x600000
        );
        assert_eq!(
            u64::from_le_bytes(bytes[224..232].try_into().unwrap()),
            0x400000
        );
        assert_eq!(&bytes[232..236], &[0, 0, 0, 0]); // null next
        assert_eq!(&bytes[236..240], b"mtd\0");
    }

    #[test]
    fn mem_request_builders_match_handler_layouts() {
        assert_eq!(
            write_mem_request(0x41000000, &[1, 2, 3]),
            vec![0x00, 0x00, 0x00, 0x41, 0x03, 0x00, 0x00, 0x00, 1, 2, 3]
        );
        assert_eq!(
            read_mem_request(0x41000000, 0x100),
            vec![0x00, 0x00, 0x00, 0x41, 0x00, 0x01, 0x00, 0x00]
        );
        assert_eq!(exec_request(0x41000100), vec![0x00, 0x01, 0x00, 0x41]);
        assert_eq!(
            shell_request("reset").unwrap(),
            vec![0x05, 0x00, 0x00, 0x00, b'r', b'e', b's', b'e', b't']
        );
        assert!(shell_request(&"x".repeat(127)).is_ok());
        assert!(shell_request(&"x".repeat(128)).is_err());
    }
}
