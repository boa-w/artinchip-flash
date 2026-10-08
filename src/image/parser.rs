use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use crate::protocol::commands::{FwcMeta, FWC_META_SIZE};

pub const FW_HEADER_SIZE: usize = 2048;

/// ArtInChip firmware image header (struct image_header_pack, 2048 bytes)
///
/// Core header (struct image_header_upgrade, 348 bytes):
///   [0..8)     magic       "AIC.FW"
///   [8..72)    platform    64 bytes
///   [72..136)   product     64 bytes
///   [136..200)  version     64 bytes
///   [200..264)  media_type  64 bytes
///   [264..268)  media_dev_id  u32 LE
///   [268..332)  media_id    64 bytes (nand_array_org)
///   [332..336)  meta_offset  u32 LE
///   [336..340)  meta_size    u32 LE
///   [340..344)  file_offset  u32 LE
///   [344..348)  file_size    u32 LE
///   [348..2048) pad         1700 zero bytes
#[derive(Clone, Debug)]
pub struct FwHeader {
    bytes: Vec<u8>,
}

impl FwHeader {
    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        if bytes.len() < FW_HEADER_SIZE {
            return None;
        }
        Some(Self {
            bytes: bytes[..FW_HEADER_SIZE].to_vec(),
        })
    }

    pub fn magic_str(&self) -> &str {
        std::str::from_utf8(&self.bytes[0..8])
            .unwrap_or("")
            .trim_end_matches('\0')
    }

    pub fn platform_str(&self) -> &str {
        std::str::from_utf8(&self.bytes[8..72])
            .unwrap_or("")
            .trim_end_matches('\0')
    }

    pub fn product_str(&self) -> &str {
        std::str::from_utf8(&self.bytes[72..136])
            .unwrap_or("")
            .trim_end_matches('\0')
    }

    pub fn version_str(&self) -> &str {
        std::str::from_utf8(&self.bytes[136..200])
            .unwrap_or("")
            .trim_end_matches('\0')
    }

    pub fn media_type_str(&self) -> &str {
        std::str::from_utf8(&self.bytes[200..264])
            .unwrap_or("")
            .trim_end_matches('\0')
    }

    pub fn media_dev_id(&self) -> u32 {
        u32::from_le_bytes(self.bytes[264..268].try_into().unwrap())
    }

    pub fn media_id_str(&self) -> &str {
        std::str::from_utf8(&self.bytes[268..332])
            .unwrap_or("")
            .trim_end_matches('\0')
    }

    pub fn meta_offset_val(&self) -> u32 {
        u32::from_le_bytes(self.bytes[332..336].try_into().unwrap())
    }

    pub fn meta_size_val(&self) -> u32 {
        u32::from_le_bytes(self.bytes[336..340].try_into().unwrap())
    }

    pub fn file_offset_val(&self) -> u32 {
        u32::from_le_bytes(self.bytes[340..344].try_into().unwrap())
    }

    pub fn file_size_val(&self) -> u32 {
        u32::from_le_bytes(self.bytes[344..348].try_into().unwrap())
    }
}

#[derive(Clone, Debug)]
pub struct ImageSummary {
    pub path: Option<PathBuf>,
    pub total_size: usize,
    pub magic: String,
    pub platform: String,
    pub product: String,
    pub version: String,
    pub media_type: String,
    pub media_id: String,
    pub media_dev_id: u32,
    pub meta_offset: u32,
    pub meta_size: u32,
    pub file_offset: u32,
    pub file_size: u32,
    pub metas: Vec<MetaSummary>,
}

#[derive(Clone, Debug)]
pub struct MetaSummary {
    pub index: usize,
    pub magic: String,
    pub name: String,
    pub partition: String,
    pub offset: u32,
    pub size: u32,
    pub crc: u32,
    pub ram: u32,
    pub attr: String,
}

impl ImageSummary {
    pub fn from_parts(
        path: Option<PathBuf>,
        total_size: usize,
        header: &FwHeader,
        metas: &[FwcMeta],
    ) -> Self {
        Self {
            path,
            total_size,
            magic: header.magic_str().to_string(),
            platform: header.platform_str().to_string(),
            product: header.product_str().to_string(),
            version: header.version_str().to_string(),
            media_type: header.media_type_str().to_string(),
            media_id: header.media_id_str().to_string(),
            media_dev_id: header.media_dev_id(),
            meta_offset: header.meta_offset_val(),
            meta_size: header.meta_size_val(),
            file_offset: header.file_offset_val(),
            file_size: header.file_size_val(),
            metas: metas
                .iter()
                .enumerate()
                .map(|(index, meta)| MetaSummary {
                    index,
                    magic: meta.magic_str().to_string(),
                    name: meta.name_str().to_string(),
                    partition: meta.partition_str().to_string(),
                    offset: meta.offset_val(),
                    size: meta.size_val(),
                    crc: meta.crc_val(),
                    ram: meta.ram_val(),
                    attr: meta.attr_str().to_string(),
                })
                .collect(),
        }
    }
}

pub fn parse_image(data: &[u8]) -> Result<(FwHeader, Vec<FwcMeta>, &[u8]), String> {
    let (header, metas, payload_offset) = parse_header_and_metas(data)?;
    if data.len() < payload_offset {
        return Err(format!(
            "File too short for image payload: need {} bytes, have {}",
            payload_offset,
            data.len()
        ));
    }
    Ok((header, metas, &data[payload_offset..]))
}

fn parse_header_and_metas(data: &[u8]) -> Result<(FwHeader, Vec<FwcMeta>, usize), String> {
    if data.len() < FW_HEADER_SIZE {
        return Err("File too small to contain image header".to_string());
    }

    let header = FwHeader::from_bytes(&data[..FW_HEADER_SIZE])
        .ok_or_else(|| "Failed to parse firmware header".to_string())?;

    if header.magic_str() != "AIC.FW" {
        return Err(format!(
            "Invalid image magic: '{}' (expected 'AIC.FW')",
            header.magic_str()
        ));
    }

    let meta_offset = header.meta_offset_val() as usize;
    let meta_size = header.meta_size_val() as usize;
    let meta_count = if meta_size > 0 {
        meta_size / FWC_META_SIZE
    } else {
        0
    };
    let file_offset = header.file_offset_val() as usize;

    if meta_offset == 0 || meta_count == 0 {
        return Err("No META entries in image header".to_string());
    }

    let meta_end = meta_offset + meta_count * FWC_META_SIZE;
    if data.len() < meta_end {
        return Err(format!(
            "File too short for META entries: need {} bytes, have {}",
            meta_end,
            data.len()
        ));
    }

    let mut metas = Vec::with_capacity(meta_count);
    for i in 0..meta_count {
        let entry_offset = meta_offset + i * FWC_META_SIZE;
        let entry_bytes = &data[entry_offset..entry_offset + FWC_META_SIZE];
        let meta = FwcMeta::from_bytes(entry_bytes)
            .ok_or_else(|| format!("Failed to parse META entry {}", i))?;
        metas.push(meta);
    }

    let payload_offset = if file_offset > 0 {
        file_offset
    } else {
        meta_end
    };
    Ok((header, metas, payload_offset))
}

pub fn read_image(path: &Path) -> Result<(Vec<u8>, FwHeader, Vec<FwcMeta>, ImageSummary), String> {
    let data = fs::read(path).map_err(|e| format!("Error reading '{}': {}", path.display(), e))?;
    let (header, metas, _payload) = parse_image(&data)?;
    let summary = ImageSummary::from_parts(Some(path.to_path_buf()), data.len(), &header, &metas);
    Ok((data, header, metas, summary))
}

pub fn read_image_summary(path: &Path) -> Result<ImageSummary, String> {
    let mut file =
        File::open(path).map_err(|e| format!("Error reading '{}': {}", path.display(), e))?;
    let total_size = file
        .metadata()
        .map_err(|e| format!("Error reading metadata for '{}': {}", path.display(), e))?
        .len() as usize;

    let mut header_bytes = vec![0u8; FW_HEADER_SIZE];
    file.read_exact(&mut header_bytes).map_err(|e| {
        format!(
            "Error reading image header from '{}': {}",
            path.display(),
            e
        )
    })?;
    let header = FwHeader::from_bytes(&header_bytes)
        .ok_or_else(|| "Failed to parse firmware header".to_string())?;
    if header.magic_str() != "AIC.FW" {
        return Err(format!(
            "Invalid image magic: '{}' (expected 'AIC.FW')",
            header.magic_str()
        ));
    }

    let meta_offset = header.meta_offset_val() as usize;
    let meta_size = header.meta_size_val() as usize;
    if meta_offset == 0 || meta_size == 0 {
        return Err("No META entries in image header".to_string());
    }
    let meta_end = meta_offset
        .checked_add(meta_size)
        .ok_or_else(|| "META offset/size overflow".to_string())?;
    if total_size < meta_end {
        return Err(format!(
            "File too short for META entries: need {} bytes, have {}",
            meta_end, total_size
        ));
    }

    let mut meta_bytes = vec![0u8; meta_size];
    file.seek(SeekFrom::Start(meta_offset as u64))
        .map_err(|e| format!("Error seeking META entries in '{}': {}", path.display(), e))?;
    file.read_exact(&mut meta_bytes).map_err(|e| {
        format!(
            "Error reading META entries from '{}': {}",
            path.display(),
            e
        )
    })?;

    let meta_count = meta_size / FWC_META_SIZE;
    let mut metas = Vec::with_capacity(meta_count);
    for i in 0..meta_count {
        let entry_offset = i * FWC_META_SIZE;
        let entry_bytes = &meta_bytes[entry_offset..entry_offset + FWC_META_SIZE];
        let meta = FwcMeta::from_bytes(entry_bytes)
            .ok_or_else(|| format!("Failed to parse META entry {}", i))?;
        metas.push(meta);
    }

    Ok(ImageSummary::from_parts(
        Some(path.to_path_buf()),
        total_size,
        &header,
        &metas,
    ))
}

pub fn summarize_image(data: &[u8]) -> Result<ImageSummary, String> {
    let (header, metas, _payload) = parse_image(data)?;
    Ok(ImageSummary::from_parts(None, data.len(), &header, &metas))
}

pub fn extract_components(image_path: &Path, output_dir: &Path) -> Result<Vec<PathBuf>, String> {
    let (data, _header, metas, _summary) = read_image(image_path)?;
    fs::create_dir_all(output_dir)
        .map_err(|e| format!("Failed to create '{}': {}", output_dir.display(), e))?;

    let mut written = Vec::new();
    for meta in metas {
        let offset = meta.offset_val() as usize;
        let size = meta.size_val() as usize;
        let end = offset
            .checked_add(size)
            .ok_or_else(|| format!("{} offset/size overflow", meta.name_str()))?;
        if end > data.len() {
            return Err(format!(
                "{} image range out of bounds: offset={:#x}, size={}, image_len={}",
                meta.name_str(),
                offset,
                size,
                data.len()
            ));
        }

        let file_name = sanitize_file_name(meta.name_str());
        let out = output_dir.join(file_name);
        fs::write(&out, &data[offset..end])
            .map_err(|e| format!("Failed to write '{}': {}", out.display(), e))?;
        written.push(out);
    }
    Ok(written)
}

pub fn format_image_info(data: &[u8]) -> Result<String, String> {
    let (header, metas, _payload) = parse_image(data)?;

    let mut out = Vec::new();
    out.push("=== Image Header ===".to_string());
    out.push(format!("  Magic:       {}", header.magic_str()));
    out.push(format!("  Platform:    {}", header.platform_str()));
    out.push(format!("  Product:     {}", header.product_str()));
    out.push(format!("  Version:     {}", header.version_str()));
    out.push(format!("  Media type:  {}", header.media_type_str()));
    out.push(format!("  Media dev:   {:#x}", header.media_dev_id()));
    out.push(format!("  Meta offset: {:#x}", header.meta_offset_val()));
    out.push(format!(
        "  Meta count:  {}",
        meta_size_to_count(header.meta_size_val())
    ));
    out.push(format!("  File offset: {:#x}", header.file_offset_val()));
    out.push(format!("  File size:   {}", header.file_size_val()));

    out.push(String::new());
    out.push(format!("=== META Entries ({}) ===", metas.len()));
    for (i, meta) in metas.iter().enumerate() {
        out.push(format!(
            "  [{:2}] {} (partition: {}, offset={:#x}, size={}, crc=0x{:08x}, ram={:#x}, attr='{}')",
            i,
            meta.name_str(),
            meta.partition_str(),
            meta.offset_val(),
            meta.size_val(),
            meta.crc_val(),
            meta.ram_val(),
            meta.attr_str(),
        ));
    }

    Ok(out.join("\n"))
}

pub fn print_image_info(data: &[u8]) -> Result<(), String> {
    println!("{}", format_image_info(data)?);
    Ok(())
}

fn meta_size_to_count(sz: u32) -> u32 {
    if sz > 0 {
        sz / FWC_META_SIZE as u32
    } else {
        0
    }
}

fn sanitize_file_name(name: &str) -> String {
    let mut out = String::with_capacity(name.len() + 4);
    for ch in name.chars() {
        if ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-') {
            out.push(ch);
        } else {
            out.push('_');
        }
    }
    if out.is_empty() {
        "component.bin".to_string()
    } else {
        format!("{}.bin", out)
    }
}
