//! Minimal pure-Rust MS-CFB (OLE2 Compound File Binary) reader for legacy Office files.

use crate::{
    errors::{DomainError, ERROR_INVALID_DOCUMENT},
    read::ooxml::{MAX_EXTRACTED_TEXT_BYTES, MAX_XML_ENTRY_BYTES},
};

pub(super) const OLE2_SIGNATURE: [u8; 8] = [0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1];

const CFB_FREESECT: u32 = 0xFFFF_FFFF;
const CFB_ENDOFCHAIN: u32 = 0xFFFF_FFFE;
const CFB_MAX_REG_SECT: u32 = 0xFFFF_FFFA;
const CFB_MINI_SECTOR_SIZE: usize = 64;

struct CfbDirectoryEntry {
    name: String,
    object_type: u8,
    start_sector: u32,
    stream_size: u64,
}

pub(super) struct CfbArchive<'a> {
    bytes: &'a [u8],
    sector_size: usize,
    mini_stream_cutoff: u64,
    fat: Vec<u32>,
    minifat: Vec<u32>,
    mini_stream: Vec<u8>,
    entries: Vec<CfbDirectoryEntry>,
}

impl<'a> CfbArchive<'a> {
    pub(super) fn parse(bytes: &'a [u8]) -> Result<Self, DomainError> {
        if bytes.len() < 512 || !bytes.starts_with(&OLE2_SIGNATURE) {
            return Err(DomainError::new(
                ERROR_INVALID_DOCUMENT,
                "invalid OLE2 compound file header",
            ));
        }
        let major_version = read_u16_le(bytes, 26)?;
        let sector_shift = read_u16_le(bytes, 30)?;
        let sector_size = match sector_shift {
            9 => 512usize,
            12 => 4096usize,
            _ => {
                return Err(DomainError::new(
                    ERROR_INVALID_DOCUMENT,
                    format!("unsupported OLE2 sector shift {sector_shift}"),
                ));
            }
        };
        let first_dir_sector = read_u32_le(bytes, 48)?;
        let mini_stream_cutoff = u64::from(read_u32_le(bytes, 56)?);
        let first_minifat_sector = read_u32_le(bytes, 60)?;
        let num_minifat_sectors = read_u32_le(bytes, 64)?;
        let first_difat_sector = read_u32_le(bytes, 68)?;
        let num_difat_sectors = read_u32_le(bytes, 72)?;

        let mut fat_sectors = Vec::new();
        for i in 0..109usize {
            let sid = read_u32_le(bytes, 76 + i * 4)?;
            if sid <= CFB_MAX_REG_SECT {
                fat_sectors.push(sid);
            }
        }
        let mut difat_sid = first_difat_sector;
        let max_difat = usize::try_from(num_difat_sectors).unwrap_or(0).min(1024);
        for _ in 0..max_difat {
            if difat_sid > CFB_MAX_REG_SECT {
                break;
            }
            let sector = read_cfb_sector(bytes, sector_size, difat_sid)?;
            let entries_per_sector = sector_size / 4 - 1;
            for i in 0..entries_per_sector {
                let sid = read_u32_le(sector, i * 4)?;
                if sid <= CFB_MAX_REG_SECT {
                    fat_sectors.push(sid);
                }
            }
            difat_sid = read_u32_le(sector, entries_per_sector * 4)?;
        }

        let mut fat = Vec::with_capacity(fat_sectors.len() * (sector_size / 4));
        for fat_sid in fat_sectors {
            let sector = read_cfb_sector(bytes, sector_size, fat_sid)?;
            for chunk in sector.as_chunks::<4>().0 {
                fat.push(u32::from_le_bytes(*chunk));
            }
        }

        let dir_bytes = read_fat_chain(bytes, sector_size, &fat, first_dir_sector, None)?;
        let mut entries = Vec::with_capacity(dir_bytes.len() / 128);
        for chunk in dir_bytes.as_chunks::<128>().0 {
            let name_len = usize::from(read_u16_le(chunk, 64)?).min(64);
            let object_type = chunk[66];
            let start_sector = read_u32_le(chunk, 116)?;
            let raw_size = read_u64_le(chunk, 120)?;
            let stream_size = if major_version == 3 {
                raw_size & 0xFFFF_FFFF
            } else {
                raw_size
            };
            let name_bytes = &chunk[..name_len];
            let utf16 = name_bytes
                .as_chunks::<2>()
                .0
                .iter()
                .map(|pair| u16::from_le_bytes(*pair))
                .take_while(|&unit| unit != 0);
            let name: String = char::decode_utf16(utf16)
                .map(|res| res.unwrap_or(char::REPLACEMENT_CHARACTER))
                .collect();
            entries.push(CfbDirectoryEntry {
                name,
                object_type,
                start_sector,
                stream_size,
            });
        }

        let mut minifat = Vec::new();
        if num_minifat_sectors > 0 && first_minifat_sector <= CFB_MAX_REG_SECT {
            let minifat_bytes =
                read_fat_chain(bytes, sector_size, &fat, first_minifat_sector, None)?;
            for chunk in minifat_bytes.as_chunks::<4>().0 {
                minifat.push(u32::from_le_bytes(*chunk));
            }
        }

        let mini_stream = if let Some(root) = entries.first()
            && root.object_type == 5
            && root.start_sector <= CFB_MAX_REG_SECT
            && root.stream_size > 0
        {
            read_fat_chain(
                bytes,
                sector_size,
                &fat,
                root.start_sector,
                Some(root.stream_size),
            )?
        } else {
            Vec::new()
        };

        Ok(Self {
            bytes,
            sector_size,
            mini_stream_cutoff,
            fat,
            minifat,
            mini_stream,
            entries,
        })
    }

    pub(super) fn read_stream(&self, name: &str) -> Result<Option<Vec<u8>>, DomainError> {
        let Some(entry) = self
            .entries
            .iter()
            .find(|entry| entry.object_type == 2 && entry.name.eq_ignore_ascii_case(name))
        else {
            return Ok(None);
        };
        if entry.stream_size > MAX_XML_ENTRY_BYTES {
            return Err(DomainError::new(
                ERROR_INVALID_DOCUMENT,
                format!("OLE2 stream {name} exceeds the size limit"),
            ));
        }
        if entry.stream_size == 0 || entry.start_sector > CFB_MAX_REG_SECT {
            return Ok(Some(Vec::new()));
        }
        if entry.stream_size < self.mini_stream_cutoff {
            let data = read_mini_chain(
                &self.mini_stream,
                &self.minifat,
                entry.start_sector,
                entry.stream_size,
            )?;
            Ok(Some(data))
        } else {
            let data = read_fat_chain(
                self.bytes,
                self.sector_size,
                &self.fat,
                entry.start_sector,
                Some(entry.stream_size),
            )?;
            Ok(Some(data))
        }
    }
}

fn read_cfb_sector(bytes: &[u8], sector_size: usize, sid: u32) -> Result<&[u8], DomainError> {
    let offset = (usize::try_from(sid).unwrap_or(usize::MAX))
        .checked_add(1)
        .and_then(|idx| idx.checked_mul(sector_size))
        .ok_or_else(|| DomainError::new(ERROR_INVALID_DOCUMENT, "OLE2 sector offset overflow"))?;
    let end = offset
        .checked_add(sector_size)
        .ok_or_else(|| DomainError::new(ERROR_INVALID_DOCUMENT, "OLE2 sector offset overflow"))?;
    bytes
        .get(offset..end)
        .ok_or_else(|| DomainError::new(ERROR_INVALID_DOCUMENT, "OLE2 sector lies past EOF"))
}

fn read_fat_chain(
    bytes: &[u8],
    sector_size: usize,
    fat: &[u32],
    start: u32,
    limit: Option<u64>,
) -> Result<Vec<u8>, DomainError> {
    let max_bytes = usize::try_from(limit.unwrap_or(MAX_XML_ENTRY_BYTES))
        .unwrap_or(MAX_EXTRACTED_TEXT_BYTES)
        .min(MAX_EXTRACTED_TEXT_BYTES);
    let mut out = Vec::new();
    let mut visited = vec![false; fat.len()];
    let mut sid = start;

    while sid != CFB_ENDOFCHAIN && sid != CFB_FREESECT && sid <= CFB_MAX_REG_SECT {
        let idx = usize::try_from(sid).unwrap_or(usize::MAX);
        if idx >= visited.len() || visited[idx] {
            return Err(DomainError::new(
                ERROR_INVALID_DOCUMENT,
                "corrupt or cyclic OLE2 FAT chain",
            ));
        }
        visited[idx] = true;
        let sector = read_cfb_sector(bytes, sector_size, sid)?;
        let remaining = max_bytes.saturating_sub(out.len());
        if remaining == 0 {
            break;
        }
        out.extend_from_slice(&sector[..sector.len().min(remaining)]);
        if limit.is_some() && out.len() >= max_bytes {
            break;
        }
        sid = fat[idx];
    }

    Ok(out)
}

fn read_mini_chain(
    mini_stream: &[u8],
    minifat: &[u32],
    start: u32,
    size: u64,
) -> Result<Vec<u8>, DomainError> {
    let max_bytes = usize::try_from(size)
        .unwrap_or(MAX_EXTRACTED_TEXT_BYTES)
        .min(MAX_EXTRACTED_TEXT_BYTES);
    let mut out = Vec::with_capacity(max_bytes);
    let mut visited = vec![false; minifat.len()];
    let mut sid = start;

    while sid != CFB_ENDOFCHAIN && sid != CFB_FREESECT && sid <= CFB_MAX_REG_SECT {
        let idx = usize::try_from(sid).unwrap_or(usize::MAX);
        if idx >= visited.len() || visited[idx] {
            return Err(DomainError::new(
                ERROR_INVALID_DOCUMENT,
                "corrupt or cyclic OLE2 MiniFAT chain",
            ));
        }
        visited[idx] = true;
        let offset = idx
            .checked_mul(CFB_MINI_SECTOR_SIZE)
            .ok_or_else(|| DomainError::new(ERROR_INVALID_DOCUMENT, "OLE2 mini offset overflow"))?;
        let end = offset.saturating_add(CFB_MINI_SECTOR_SIZE);
        let chunk = mini_stream.get(offset..end).ok_or_else(|| {
            DomainError::new(ERROR_INVALID_DOCUMENT, "OLE2 mini sector lies past stream")
        })?;
        let remaining = max_bytes.saturating_sub(out.len());
        if remaining == 0 {
            break;
        }
        out.extend_from_slice(&chunk[..chunk.len().min(remaining)]);
        if out.len() >= max_bytes {
            break;
        }
        sid = minifat[idx];
    }

    Ok(out)
}

pub(super) fn read_u16_le(bytes: &[u8], offset: usize) -> Result<u16, DomainError> {
    let slice = bytes
        .get(offset..offset + 2)
        .ok_or_else(|| DomainError::new(ERROR_INVALID_DOCUMENT, "truncated .doc binary field"))?;
    Ok(u16::from_le_bytes([slice[0], slice[1]]))
}

pub(super) fn read_u32_le(bytes: &[u8], offset: usize) -> Result<u32, DomainError> {
    let slice = bytes
        .get(offset..offset + 4)
        .ok_or_else(|| DomainError::new(ERROR_INVALID_DOCUMENT, "truncated .doc binary field"))?;
    Ok(u32::from_le_bytes([slice[0], slice[1], slice[2], slice[3]]))
}

fn read_u64_le(bytes: &[u8], offset: usize) -> Result<u64, DomainError> {
    let slice = bytes
        .get(offset..offset + 8)
        .ok_or_else(|| DomainError::new(ERROR_INVALID_DOCUMENT, "truncated .doc binary field"))?;
    Ok(u64::from_le_bytes([
        slice[0], slice[1], slice[2], slice[3], slice[4], slice[5], slice[6], slice[7],
    ]))
}
