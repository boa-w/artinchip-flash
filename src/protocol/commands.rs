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
}
