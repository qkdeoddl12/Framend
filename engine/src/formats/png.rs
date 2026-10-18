//! Bounded static PNG framing and critical-chunk/CRC checks; no pixel decompression.
//! APNG frame semantics are not supported by this carver.
use crate::{Candidate, Format, source::Source};
use std::{
    collections::HashSet,
    io::{self, BufReader, Read},
};

pub(crate) const MAGIC: &[u8] = b"\x89PNG\r\n\x1a\n";
const MAX_CHUNKS: usize = 65_536;
const fn crc_table() -> [u32; 256] {
    let mut table = [0; 256];
    let mut index = 0;
    while index < 256 {
        let mut value = index as u32;
        let mut bit = 0;
        while bit < 8 {
            value = (value >> 1) ^ (0xedb88320 & 0u32.wrapping_sub(value & 1));
            bit += 1;
        }
        table[index] = value;
        index += 1;
    }
    table
}
const CRC: [u32; 256] = crc_table();
fn update(mut crc: u32, bytes: &[u8]) -> u32 {
    for &byte in bytes {
        crc = (crc >> 8) ^ CRC[((crc ^ u32::from(byte)) & 255) as usize];
    }
    crc
}

pub(super) fn parse(source: &Source, offset: u64) -> io::Result<Option<Candidate>> {
    let limit = source
        .fingerprint
        .size
        .saturating_sub(offset)
        .min(super::MAX_PHOTO);
    if limit < 45 {
        return Ok(None);
    }
    let mut reader = BufReader::with_capacity(64 * 1024, source.region(offset, limit));
    match inner(&mut reader, limit) {
        Ok(Some((length, width, height))) => Ok(Some(Candidate {
            offset,
            length,
            format: Format::Png,
            width: Some(width),
            height: Some(height),
            camera: None,
        })),
        Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => Ok(None),
        Ok(None) => Ok(None),
        Err(error) => Err(error),
    }
}
fn inner(reader: &mut impl Read, limit: u64) -> io::Result<Option<(u64, u32, u32)>> {
    let mut signature = [0; 8];
    reader.read_exact(&mut signature)?;
    if signature != MAGIC {
        return Ok(None);
    }
    let mut position = 8u64;
    let mut dimensions = (0, 0);
    let mut metadata = HashSet::new();
    let mut color = 0;
    let mut depth = 0;
    let mut palette = false;
    let mut data = false;
    let mut data_ended = false;
    let mut data_bytes = 0u64;
    let mut buffer = [0; 64 * 1024];
    for index in 0..MAX_CHUNKS {
        if limit - position < 12 {
            return Ok(None);
        }
        let mut header = [0; 8];
        reader.read_exact(&mut header)?;
        let length = u32::from_be_bytes(header[..4].try_into().map_err(io::Error::other)?) as u64;
        let kind: [u8; 4] = header[4..].try_into().map_err(io::Error::other)?;
        if length > 0x7fffffff
            || length + 12 > limit - position
            || !kind.iter().all(u8::is_ascii_alphabetic)
            || kind[2].is_ascii_lowercase()
        {
            return Ok(None);
        }
        if index == 0 && (kind != *b"IHDR" || length != 13) {
            return Ok(None);
        }
        if index > 0 && kind == *b"IHDR" {
            return Ok(None);
        }
        if kind == *b"PLTE"
            && (palette
                || data
                || metadata.contains(b"bKGD")
                || metadata.contains(b"tRNS")
                || length == 0
                || length > 768
                || !length.is_multiple_of(3)
                || matches!(color, 0 | 4)
                || (color == 3 && length / 3 > 1u64 << depth))
        {
            return Ok(None);
        }
        if kind == *b"IDAT" && (data_ended || (color == 3 && !palette)) {
            return Ok(None);
        }
        if !matches!(&kind, b"IHDR" | b"PLTE" | b"IDAT" | b"IEND") && kind[0].is_ascii_uppercase() {
            return Ok(None);
        }
        if matches!(&kind, b"acTL" | b"fcTL" | b"fdAT") || (kind == *b"sPLT" && data) {
            return Ok(None);
        }
        // Registered single-instance ancillary chunks have ordering constraints too.
        let before_palette = matches!(
            &kind,
            b"cHRM" | b"gAMA" | b"iCCP" | b"sBIT" | b"sRGB" | b"cICP" | b"mDCV" | b"cLLI"
        );
        let before_data = matches!(&kind, b"bKGD" | b"hIST" | b"tRNS" | b"pHYs" | b"eXIf");
        if (before_palette && (palette || data))
            || (before_data && data)
            || (kind == *b"hIST" && !palette)
            || (matches!(&kind, b"bKGD" | b"tRNS") && color == 3 && !palette)
            || ((before_palette || before_data || kind == *b"tIME") && !metadata.insert(kind))
        {
            return Ok(None);
        }
        let mut crc = update(u32::MAX, &kind);
        let mut remaining = length;
        while remaining > 0 {
            let count = remaining.min(buffer.len() as u64) as usize;
            reader.read_exact(&mut buffer[..count])?;
            crc = update(crc, &buffer[..count]);
            if kind == *b"IHDR" {
                // IHDR is the first chunk and exactly thirteen bytes.
                dimensions = (
                    u32::from_be_bytes(buffer[..4].try_into().map_err(io::Error::other)?),
                    u32::from_be_bytes(buffer[4..8].try_into().map_err(io::Error::other)?),
                );
                depth = buffer[8];
                color = buffer[9];
                let valid_depth = match color {
                    0 => matches!(depth, 1 | 2 | 4 | 8 | 16),
                    2 | 4 | 6 => matches!(depth, 8 | 16),
                    3 => matches!(depth, 1 | 2 | 4 | 8),
                    _ => false,
                };
                if dimensions.0 == 0
                    || dimensions.1 == 0
                    || dimensions.0 > 0x7fffffff
                    || dimensions.1 > 0x7fffffff
                    || !valid_depth
                    || buffer[10] != 0
                    || buffer[11] != 0
                    || buffer[12] > 1
                {
                    return Ok(None);
                }
            }
            remaining -= count as u64;
        }
        let mut stored_crc = [0; 4];
        reader.read_exact(&mut stored_crc)?;
        if u32::from_be_bytes(stored_crc) != crc ^ u32::MAX {
            return Ok(None);
        }
        position += length + 12;
        if kind == *b"IEND" {
            return Ok((length == 0 && data && data_bytes > 0).then_some((
                position,
                dimensions.0,
                dimensions.1,
            )));
        }
        if kind == *b"PLTE" {
            palette = true;
        }
        if kind == *b"IDAT" {
            data = true;
            data_bytes += length;
        } else if data {
            data_ended = true;
        }
    }
    Ok(None)
}
