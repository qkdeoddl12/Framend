//! RIFF/WAVE length and chunk validation, without claiming audio decoding.
use crate::{Candidate, Format, source::Source};
use std::io;

pub(super) fn parse(source: &Source, offset: u64) -> io::Result<Option<Candidate>> {
    let available = source.fingerprint.size.saturating_sub(offset);
    if available < 12 {
        return Ok(None);
    }
    let mut header = [0; 12];
    source.read_at(offset, &mut header)?;
    if &header[..4] != b"RIFF" || &header[8..] != b"WAVE" {
        return Ok(None);
    }
    let length = u64::from(u32::from_le_bytes(
        header[4..8].try_into().map_err(io::Error::other)?,
    )) + 8;
    if length < 12 || length > available {
        return Ok(None);
    }
    let mut position = 12;
    let mut seen_format = false;
    let mut seen_data = false;
    let mut alignment = 0u64;
    let mut pcm = false;
    for _ in 0..65_536 {
        if position == length {
            return Ok((seen_format && seen_data).then_some(Candidate {
                offset,
                length,
                format: Format::Wav,
                width: None,
                height: None,
                camera: None,
            }));
        }
        if length - position < 8 {
            return Ok(None);
        }
        let mut chunk = [0; 8];
        source.read_at(offset + position, &mut chunk)?;
        let size = u64::from(u32::from_le_bytes(
            chunk[4..].try_into().map_err(io::Error::other)?,
        ));
        let padded = size + size % 2;
        if padded > length - position - 8 {
            return Ok(None);
        }
        if &chunk[..4] == b"fmt " {
            if seen_format || seen_data || size < 16 || size == 17 {
                return Ok(None);
            }
            let mut format = [0; 40];
            let count = size.min(format.len() as u64) as usize;
            source.read_at(offset + position + 8, &mut format[..count])?;
            let short = |at: usize| u16::from_le_bytes([format[at], format[at + 1]]);
            let word = |at: usize| {
                u32::from_le_bytes([format[at], format[at + 1], format[at + 2], format[at + 3]])
            };
            let code = short(0);
            let channels = u64::from(short(2));
            let rate = u64::from(word(4));
            let byte_rate = u64::from(word(8));
            alignment = u64::from(short(12));
            let bits = u64::from(short(14));
            if code == 0 || channels == 0 || rate == 0 || byte_rate == 0 || alignment == 0 {
                return Ok(None);
            }
            if size > 16 && u64::from(short(16)) != size - 18 {
                return Ok(None);
            }
            if code == 0xfffe && (size < 40 || short(16) < 22 || short(18) > short(14)) {
                return Ok(None);
            }
            pcm = code == 1 || code == 3;
            if pcm
                && (bits == 0
                    || bits % 8 != 0
                    || alignment != channels * (bits / 8)
                    || byte_rate != rate * alignment)
            {
                return Ok(None);
            }
            seen_format = true;
        } else if &chunk[..4] == b"data" {
            if !seen_format || seen_data || (pcm && size % alignment != 0) {
                return Ok(None);
            }
            seen_data = true;
        }
        if size % 2 == 1 {
            let mut padding = [0];
            source.read_at(offset + position + 8 + size, &mut padding)?;
            if padding[0] != 0 {
                return Ok(None);
            }
        }
        position += 8 + padded;
    }
    Ok(None)
}
