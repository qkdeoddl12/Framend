use crate::{Candidate, Format, source::Source};
use std::io::{self, BufReader, Read};

mod png;
mod wav;
pub(crate) use png::MAGIC as PNG_MAGIC;

pub(crate) const MAX_PHOTO: u64 = 512 * 1024 * 1024;
pub(crate) const RAF_MAGIC: &[u8] = b"FUJIFILMCCD-RAW ";

pub(crate) fn parse(source: &Source, offset: u64, format: Format) -> io::Result<Option<Candidate>> {
    match format {
        Format::Jpeg => jpeg(source, offset, MAX_PHOTO),
        Format::Raf => raf(source, offset),
        Format::Png => png::parse(source, offset),
        Format::Wav => wav::parse(source, offset),
    }
}

fn byte(reader: &mut impl Read) -> io::Result<u8> {
    let mut b = [0];
    reader.read_exact(&mut b)?;
    Ok(b[0])
}

// Segment lengths are essential: EXIF can contain a whole thumbnail JPEG.
fn jpeg(source: &Source, offset: u64, limit: u64) -> io::Result<Option<Candidate>> {
    if offset >= source.fingerprint.size {
        return Ok(None);
    }
    let length = (source.fingerprint.size - offset).min(limit);
    let mut reader = BufReader::with_capacity(64 * 1024, source.region(offset, length));
    let result = jpeg_inner(&mut reader, length);
    let parsed = match result {
        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => None,
        other => other?,
    };
    Ok(parsed.map(|(length, width, height)| Candidate {
        offset,
        length,
        format: Format::Jpeg,
        width: Some(width),
        height: Some(height),
        camera: None,
    }))
}

fn jpeg_inner(reader: &mut impl Read, limit: u64) -> io::Result<Option<(u64, u32, u32)>> {
    if byte(reader)? != 0xff || byte(reader)? != 0xd8 {
        return Ok(None);
    }
    let mut position = 2u64;
    let mut dimensions = None;
    let mut entropy = false;
    let mut seen_scan = false;
    let mut segments = 0;
    let mut scratch = vec![0; 65533];
    while position < limit && segments < 65536 {
        let first = byte(reader)?;
        position += 1;
        if first != 0xff {
            if entropy {
                continue;
            }
            return Ok(None);
        }
        let mut marker = byte(reader)?;
        position += 1;
        while marker == 0xff && position < limit {
            marker = byte(reader)?;
            position += 1;
        }
        if entropy && (marker == 0 || (0xd0..=0xd7).contains(&marker)) {
            continue;
        }
        if marker == 0xd9 {
            return Ok(if seen_scan {
                dimensions.map(|(w, h)| (position, w, h))
            } else {
                None
            });
        }
        if marker == 0xd8 || marker == 0 || marker == 0x01 || (0xd0..=0xd7).contains(&marker) {
            return Ok(None);
        }
        entropy = false;
        let size = u16::from_be_bytes([byte(reader)?, byte(reader)?]) as usize;
        position += 2;
        if size < 2 || position + (size - 2) as u64 > limit {
            return Ok(None);
        }
        let payload = &mut scratch[..size - 2];
        reader.read_exact(payload)?;
        position += payload.len() as u64;
        segments += 1;
        if matches!(marker, 0xc0..=0xc3 | 0xc5..=0xc7 | 0xc9..=0xcb | 0xcd..=0xcf) {
            if payload.len() < 6 {
                return Ok(None);
            }
            let height = u16::from_be_bytes([payload[1], payload[2]]) as u32;
            let width = u16::from_be_bytes([payload[3], payload[4]]) as u32;
            let components = payload[5] as usize;
            if width == 0 || height == 0 || components == 0 || payload.len() != 6 + 3 * components {
                return Ok(None);
            }
            dimensions = Some((width, height));
        }
        if marker == 0xda {
            if dimensions.is_none()
                || payload.len() < 6
                || payload.len() != 4 + 2 * payload[0] as usize
            {
                return Ok(None);
            }
            entropy = true;
            seen_scan = true;
        }
    }
    Ok(None)
}

fn raf(source: &Source, offset: u64) -> io::Result<Option<Candidate>> {
    if source.fingerprint.size.saturating_sub(offset) < 108 {
        return Ok(None);
    }
    let mut header = [0; 108];
    source.read_at(offset, &mut header)?;
    if &header[..16] != RAF_MAGIC {
        return Ok(None);
    }
    let mut ranges = Vec::with_capacity(3);
    for index in [84, 92, 100] {
        let start = u32::from_be_bytes([
            header[index],
            header[index + 1],
            header[index + 2],
            header[index + 3],
        ]) as u64;
        let size = u32::from_be_bytes([
            header[index + 4],
            header[index + 5],
            header[index + 6],
            header[index + 7],
        ]) as u64;
        if start < 108
            || size == 0
            || start + size > MAX_PHOTO
            || start + size > source.fingerprint.size - offset
        {
            return Ok(None);
        }
        ranges.push((start, start + size));
    }
    let preview = ranges[0];
    ranges.sort_unstable();
    if ranges.windows(2).any(|r| r[0].1 > r[1].0) {
        return Ok(None);
    }
    let Some(length) = ranges.iter().map(|r| r.1).max() else {
        return Ok(None);
    };
    // Reject unsupported/damaged previews so a false header cannot hide other photos.
    let Some(preview_candidate) = jpeg(source, offset + preview.0, preview.1 - preview.0)? else {
        return Ok(None);
    };
    if preview_candidate.length != preview.1 - preview.0 {
        return Ok(None);
    }
    // shortcut: valid extents and a JPEG preview do not establish RAW decoder validity.
    let camera = String::from_utf8_lossy(&header[28..60])
        .trim_matches('\0')
        .trim()
        .to_owned();
    Ok(Some(Candidate {
        offset,
        length,
        format: Format::Raf,
        width: None,
        height: None,
        camera: if camera.is_empty() {
            None
        } else {
            Some(camera)
        },
    }))
}
