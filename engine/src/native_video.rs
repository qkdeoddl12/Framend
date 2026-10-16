//! Conservative, reference-assisted recovery of length-prefixed AVC/HEVC NALs.
//! This is a structural video-only salvage, never a claim of complete recovery.
use crate::source::{Source, invalid};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::{
    ffi::CString,
    fs::File,
    io::{self, Read, Write},
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::ffi::OsStrExt,
    },
    path::{Component, Path},
};

const WINDOW: usize = 1024 * 1024;
const MAX_MOOV: u64 = 16 * 1024 * 1024;
const MAX_NAL: u64 = 64 * 1024 * 1024;
const MAX_EXTENTS: usize = 1_000_000;
const MAX_BOXES: usize = 100_000;

#[derive(Debug, Serialize)]
pub struct RetainedExtent {
    /// Original length prefix and NAL payload; output substitutes an Annex B prefix.
    pub offset: u64,
    pub length: u64,
}

#[derive(Debug, Serialize)]
pub struct SalvageReport {
    pub codec: String,
    pub fps_num: u32,
    pub fps_den: u32,
    pub frames: u64,
    pub extents: Vec<RetainedExtent>,
    pub skipped_bytes: u64,
    pub input_sha256: String,
    pub reference_sha256: String,
    pub warnings: Vec<String>,
    /// True even when independent decoding later succeeds.
    pub partial: bool,
    /// Structural picture starts, not a decoder's count of intact pictures.
    pub frame_count_basis: String,
    pub timing_basis: String,
    pub segments: u64,
}

/// Write an exclusive new elementary stream using only retained input NAL payloads
/// and the explicitly identified reference parameter sets. No decoder is invoked.
pub fn salvage(
    input: &Path,
    reference: &Path,
    output: &Path,
    mut progress: impl FnMut(u64, u64) -> io::Result<()>,
) -> io::Result<SalvageReport> {
    let input = Source::open(input)?;
    let reference = Source::open(reference)?;
    if input.fingerprint.device == reference.fingerprint.device
        && input.fingerprint.inode == reference.fingerprint.inode
    {
        return Err(invalid("The input and reference must be different files."));
    }
    let config = read_reference(&reference)?;
    let total = input
        .fingerprint
        .size
        .checked_mul(2)
        .and_then(|v| v.checked_add(reference.fingerprint.size))
        .ok_or_else(|| invalid("Source is too large."))?;
    progress(0, total)?;
    let input_sha256 = hash(&input, 0, total, &mut progress)?;
    let reference_sha256 = hash(&reference, input.fingerprint.size, total, &mut progress)?;
    input.unchanged()?;
    reference.unchanged()?;
    let (ranges, raw_scan) = media_ranges(&input)?;
    let mut writer = create_output(output)?;
    let mut reader = Window::new(&input);
    let mut copy = vec![0; WINDOW];
    let mut extents: Vec<RetainedExtent> = Vec::new();
    let mut frames = 0;
    let mut segments = 0;
    let mut retained = 0;
    let mut discarded_gaps = 0;
    let mut last_progress = 0;
    let base = input.fingerprint.size + reference.fingerprint.size;
    for (start, end) in ranges {
        let mut pos = start;
        let mut active = false;
        let mut picture = None;
        while pos < end {
            if pos.saturating_sub(last_progress) >= WINDOW as u64 {
                progress(base + pos, total)?;
                input.unchanged()?;
                reference.unchanged()?;
                last_progress = pos;
            }
            let mut candidate = candidate(&mut reader, pos, end, &config)?;
            if candidate.as_ref().is_some_and(|nal| nal.incompatible) {
                // Audio/random bytes can imitate a short parameter set. A
                // contradiction is evidence only when a coherent NAL chain
                // corroborates this byte alignment.
                if chain(&mut reader, pos, end, &config, false, true)? {
                    return Err(invalid(
                        "In-band parameter set conflicts with the reference; compatibility cannot be established.",
                    ));
                }
                candidate = None;
            }
            let Some(nal) = candidate else {
                if active {
                    discarded_gaps += 1;
                }
                active = false;
                picture = None;
                pos += 1;
                continue;
            };
            if !active {
                if !nal.safe_start() || !viable_chain(&mut reader, pos, end, &config)? {
                    // Do not leap over unconfirmed bytes: a real prefix may occur inside.
                    pos += 1;
                    continue;
                }
                for param in &config.parameters {
                    writer.write_all(&[0, 0, 0, 1])?;
                    writer.write_all(param)?;
                }
                active = true;
                segments += 1;
            }
            if let Some(slice) = &nal.slice {
                if slice.first {
                    if picture.as_ref() == Some(&slice.identity) && config.codec == Codec::Avc {
                        active = false;
                        picture = None;
                        pos += 1;
                        continue;
                    }
                    frames += 1;
                    picture = Some(slice.identity);
                } else if picture
                    .as_ref()
                    .is_none_or(|id| !slice.same_picture(id, config.codec))
                {
                    active = false;
                    picture = None;
                    pos += 1;
                    continue;
                }
            } else if nal.aud {
                picture = None;
            }
            if extents.len() >= MAX_EXTENTS {
                return Err(invalid(
                    "Retained extent limit exceeded; output remains incomplete.",
                ));
            }
            writer.write_all(&[0, 0, 0, 1])?;
            let mut offset = pos + config.length_size as u64;
            let mut remaining = nal.length;
            while remaining > 0 {
                let amount = remaining.min(copy.len() as u64) as usize;
                input.read_at(offset, &mut copy[..amount])?;
                writer.write_all(&copy[..amount])?;
                offset += amount as u64;
                remaining -= amount as u64;
            }
            let length = nal.length + config.length_size as u64;
            if let Some(last) = extents.last_mut().filter(|e| e.offset + e.length == pos) {
                last.length += length;
            } else {
                extents.push(RetainedExtent {
                    offset: pos,
                    length,
                });
            }
            retained += length;
            pos += length;
        }
    }
    if frames == 0 {
        return Err(invalid(
            "No viable IDR-led video NAL chain found; no frames were recovered.",
        ));
    }
    writer.sync_all()?;
    input.unchanged()?;
    reference.unchanged()?;
    progress(total, total)?;
    input.unchanged()?;
    reference.unchanged()?;
    let mut warnings = vec![
        "PARTIAL video-only salvage: audio and original timing are not restored.".into(),
        "Frame rate is estimated from reference stts/mdhd; original timestamps and duration are unknown.".into(),
        "Frames count structural picture starts, not decoded pictures; full independent decoding is required. A final picture may lack later slices.".into(),
        "Reference parameter sets do not prove the missing stream used identical settings; semantic reference compatibility is not established.".into(),
        "Unknown byte gaps end a segment; dependent pictures are discarded until another viable IDR. HEVC CRA/BLA pictures cannot start a segment.".into(),
    ];
    if raw_scan {
        warnings
            .push("No usable mdat box: scanned the bounded file as length-prefixed data.".into());
    }
    if discarded_gaps > 0 {
        warnings.push(format!(
            "{discarded_gaps} retained segments ended at invalid, unknown, or truncated bytes."
        ));
    }
    if config.variable_rate {
        warnings.push(
            "Reference has variable sample durations; its average frame rate is only an estimate."
                .into(),
        );
    }
    Ok(SalvageReport {
        codec: config.codec.name().into(),
        fps_num: config.fps_num,
        fps_den: config.fps_den,
        frames,
        extents,
        skipped_bytes: input.fingerprint.size - retained,
        input_sha256,
        reference_sha256,
        warnings,
        partial: true,
        frame_count_basis: "structural_access_unit_starts".into(),
        timing_basis: "estimated_reference_average".into(),
        segments,
    })
}

fn hash(
    source: &Source,
    base: u64,
    total: u64,
    progress: &mut impl FnMut(u64, u64) -> io::Result<()>,
) -> io::Result<String> {
    let mut hash = Sha256::new();
    let mut buffer = vec![0; WINDOW];
    let mut stream = source.region(0, source.fingerprint.size);
    let mut offset = 0;
    loop {
        let read = stream.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hash.update(&buffer[..read]);
        offset += read as u64;
        progress(base + offset, total)?;
    }
    source.unchanged()?;
    Ok(format!("{:x}", hash.finalize()))
}

fn create_output(path: &Path) -> io::Result<File> {
    let path = if path.is_absolute() {
        path.to_owned()
    } else {
        std::env::current_dir()?.join(path)
    };
    let name = path
        .file_name()
        .ok_or_else(|| invalid("Output filename missing."))?;
    let parent = path
        .parent()
        .ok_or_else(|| invalid("Output parent missing."))?;
    let mut directory = File::open("/")?;
    // Pin each ancestor, so neither a symlink nor a path-replacement race can
    // redirect the exclusive creation outside the selected physical directory.
    for component in parent.components() {
        match component {
            Component::RootDir | Component::CurDir => continue,
            Component::Normal(name) => {
                let name =
                    CString::new(name.as_bytes()).map_err(|_| invalid("NUL in output path."))?;
                // SAFETY: the directory fd and NUL-terminated name are live;
                // every successful descriptor is immediately owned by File.
                let fd = unsafe {
                    libc::openat(
                        directory.as_raw_fd(),
                        name.as_ptr(),
                        libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                    )
                };
                if fd < 0 {
                    return Err(io::Error::last_os_error());
                }
                // SAFETY: fd is a new, exclusively owned descriptor.
                directory = unsafe { File::from_raw_fd(fd) };
            }
            _ => return Err(invalid("Parent traversal in output path is not allowed.")),
        }
    }
    let name = CString::new(name.as_bytes()).map_err(|_| invalid("NUL in output filename."))?;
    // SAFETY: live parent fd and name; O_EXCL protects every existing path.
    let fd = unsafe {
        libc::openat(
            directory.as_raw_fd(),
            name.as_ptr(),
            libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            0o600,
        )
    };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: successful openat returns a new, exclusively owned descriptor.
    Ok(unsafe { File::from_raw_fd(fd) })
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Codec {
    Avc,
    Hevc,
}
impl Codec {
    fn name(self) -> &'static str {
        match self {
            Self::Avc => "h264",
            Self::Hevc => "hevc",
        }
    }
}

struct Config {
    codec: Codec,
    length_size: usize,
    parameters: Vec<Vec<u8>>,
    avc_sps: Vec<AvcSps>,
    avc_pps: Vec<AvcPps>,
    hevc_sps: Vec<HevcSps>,
    hevc_pps: Vec<HevcPps>,
    fps_num: u32,
    fps_den: u32,
    variable_rate: bool,
}

fn be16(data: &[u8], at: usize) -> io::Result<u16> {
    let b = data
        .get(at..at + 2)
        .ok_or_else(|| invalid("Truncated 16-bit field."))?;
    Ok(u16::from_be_bytes([b[0], b[1]]))
}
fn be32(data: &[u8], at: usize) -> io::Result<u32> {
    let b = data
        .get(at..at + 4)
        .ok_or_else(|| invalid("Truncated 32-bit field."))?;
    Ok(u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
}
fn be64(data: &[u8], at: usize) -> io::Result<u64> {
    Ok(((be32(data, at)? as u64) << 32) | be32(data, at + 4)? as u64)
}

#[derive(Clone, Copy)]
struct Atom<'a> {
    kind: &'a [u8],
    data: &'a [u8],
}
fn atoms(mut data: &[u8]) -> io::Result<Vec<Atom<'_>>> {
    let mut out = Vec::new();
    while !data.is_empty() {
        if out.len() >= MAX_BOXES || data.len() < 8 {
            return Err(invalid("Invalid or excessive reference boxes."));
        }
        let short = be32(data, 0)?;
        let (size, header) = match short {
            0 => (data.len() as u64, 8),
            1 => (be64(data, 8)?, 16),
            _ => (short as u64, 8),
        };
        if size < header as u64 || size > data.len() as u64 {
            return Err(invalid("Reference box exceeds its parent."));
        }
        out.push(Atom {
            kind: &data[4..8],
            data: &data[header..size as usize],
        });
        data = &data[size as usize..];
    }
    Ok(out)
}
fn child<'a>(data: &'a [u8], kind: &[u8; 4]) -> io::Result<&'a [u8]> {
    let matches: Vec<_> = atoms(data)?
        .into_iter()
        .filter(|a| a.kind == kind)
        .collect();
    if matches.len() != 1 {
        return Err(invalid(
            "Required unique reference box is missing or duplicated.",
        ));
    }
    Ok(matches[0].data)
}

fn read_reference(source: &Source) -> io::Result<Config> {
    let mut pos = 0;
    let mut moov = None;
    let mut count = 0;
    while pos < source.fingerprint.size {
        count += 1;
        if count > MAX_BOXES {
            return Err(invalid("Too many reference boxes."));
        }
        let (kind, header, size) = source_atom(source, pos)?;
        if kind == *b"moov" {
            if moov.is_some() || size - header > MAX_MOOV {
                return Err(invalid("Duplicate or oversized moov box."));
            }
            let mut bytes = vec![0; (size - header) as usize];
            source.read_at(pos + header, &mut bytes)?;
            moov = Some(bytes);
        }
        pos += size;
    }
    let moov = moov.ok_or_else(|| invalid("Reference has no moov box."))?;
    let mut config = None;
    for track in atoms(&moov)?.into_iter().filter(|a| a.kind == b"trak") {
        let mdia = child(track.data, b"mdia")?;
        let hdlr = child(mdia, b"hdlr")?;
        if hdlr.get(8..12) != Some(b"vide") {
            continue;
        }
        if config.is_some() {
            return Err(invalid("Multiple reference video tracks are unsupported."));
        }
        let minf = child(mdia, b"minf")?;
        let dref = child(child(minf, b"dinf")?, b"dref")?;
        if be32(dref, 0)? != 0 || be32(dref, 4)? != 1 {
            return Err(invalid("Only one local reference data entry is supported."));
        }
        let entries = atoms(dref.get(8..).ok_or_else(|| invalid("Truncated dref."))?)?;
        if entries.len() != 1 || entries[0].kind != b"url " || entries[0].data != [0, 0, 0, 1] {
            return Err(invalid("External media references are not permitted."));
        }
        let stbl = child(minf, b"stbl")?;
        let stsd = child(stbl, b"stsd")?;
        if be32(stsd, 0)? != 0 || be32(stsd, 4)? != 1 {
            return Err(invalid("Reference must have one video sample description."));
        }
        let entries = atoms(stsd.get(8..).ok_or_else(|| invalid("Truncated stsd."))?)?;
        if entries.len() != 1 || entries[0].data.len() < 78 || be16(entries[0].data, 6)? != 1 {
            return Err(invalid(
                "Invalid visual sample entry or data reference index.",
            ));
        }
        let entry = entries[0];
        let mut parsed = match entry.kind {
            b"avc1" | b"avc3" => parse_avcc(child(&entry.data[78..], b"avcC")?)?,
            b"hvc1" | b"hev1" => parse_hvcc(child(&entry.data[78..], b"hvcC")?)?,
            _ => {
                return Err(invalid(
                    "Reference codec is unsupported; AVC or HEVC is required.",
                ));
            }
        };
        let mdhd = child(mdia, b"mdhd")?;
        let scale = match mdhd.first() {
            Some(0) => be32(mdhd, 12)?,
            Some(1) => be32(mdhd, 20)?,
            _ => return Err(invalid("Unsupported mdhd version.")),
        };
        let stts = child(stbl, b"stts")?;
        let entries = be32(stts, 4)? as usize;
        if scale == 0
            || be32(stts, 0)? != 0
            || entries == 0
            || entries > MAX_BOXES
            || stts.len() != 8 + 8 * entries
        {
            return Err(invalid("Invalid reference timing table."));
        }
        let mut samples = 0u64;
        let mut duration = 0u64;
        let first_delta = be32(stts, 12)?;
        for i in 0..entries {
            let n = be32(stts, 8 + i * 8)? as u64;
            let delta = be32(stts, 12 + i * 8)? as u64;
            if n == 0 || delta == 0 {
                return Err(invalid("Zero reference sample count or duration."));
            }
            samples = samples
                .checked_add(n)
                .ok_or_else(|| invalid("Timing overflow."))?;
            duration = duration
                .checked_add(n * delta)
                .ok_or_else(|| invalid("Timing overflow."))?;
            parsed.variable_rate |= delta != first_delta as u64;
        }
        let numerator = samples
            .checked_mul(scale as u64)
            .ok_or_else(|| invalid("Frame rate overflow."))?;
        let divisor = gcd(numerator, duration);
        parsed.fps_num = u32::try_from(numerator / divisor)
            .map_err(|_| invalid("Frame rate is out of range."))?;
        parsed.fps_den = u32::try_from(duration / divisor)
            .map_err(|_| invalid("Frame rate is out of range."))?;
        if numerator / duration > 1000 {
            return Err(invalid("Reference frame rate exceeds 1000 fps."));
        }
        config = Some(parsed);
    }
    config.ok_or_else(|| invalid("Reference has no supported video track."))
}
fn gcd(mut a: u64, mut b: u64) -> u64 {
    while b != 0 {
        let r = a % b;
        a = b;
        b = r;
    }
    a
}

fn source_atom(source: &Source, pos: u64) -> io::Result<([u8; 4], u64, u64)> {
    let mut bytes = [0; 16];
    source.read_at(pos, &mut bytes[..8])?;
    let short = be32(&bytes, 0)?;
    let mut header = 8;
    let size = match short {
        0 => source.fingerprint.size - pos,
        1 => {
            source.read_at(pos + 8, &mut bytes[8..])?;
            header = 16;
            be64(&bytes, 8)?
        }
        _ => short as u64,
    };
    if size < header
        || pos
            .checked_add(size)
            .is_none_or(|end| end > source.fingerprint.size)
    {
        return Err(invalid("Invalid top-level box length."));
    }
    Ok(([bytes[4], bytes[5], bytes[6], bytes[7]], header, size))
}

fn media_ranges(source: &Source) -> io::Result<(Vec<(u64, u64)>, bool)> {
    let mut pos = 0;
    let mut ranges = Vec::new();
    let mut count = 0;
    while pos < source.fingerprint.size {
        count += 1;
        if count > MAX_BOXES {
            return Err(invalid("Too many input boxes."));
        }
        match source_atom(source, pos) {
            Ok((kind, header, size)) => {
                if kind == *b"mdat" {
                    ranges.push((pos + header, pos + size));
                }
                pos += size;
            }
            Err(_) => {
                // A torn mdat commonly keeps its original, now oversized, length.
                if source.fingerprint.size - pos >= 8 {
                    let mut bytes = [0; 8];
                    source.read_at(pos, &mut bytes)?;
                    if &bytes[4..] == b"mdat" {
                        let header = if be32(&bytes, 0)? == 1 { 16 } else { 8 };
                        if source.fingerprint.size - pos >= header {
                            ranges.push((pos + header, source.fingerprint.size));
                        }
                    }
                }
                break;
            }
        }
    }
    if ranges.is_empty() {
        Ok((vec![(0, source.fingerprint.size)], true))
    } else {
        Ok((ranges, false))
    }
}

fn empty_config(codec: Codec, length_size: usize) -> io::Result<Config> {
    if ![1, 2, 4].contains(&length_size) {
        return Err(invalid("Reserved NAL length prefix size."));
    }
    Ok(Config {
        codec,
        length_size,
        parameters: Vec::new(),
        avc_sps: Vec::new(),
        avc_pps: Vec::new(),
        hevc_sps: Vec::new(),
        hevc_pps: Vec::new(),
        fps_num: 0,
        fps_den: 1,
        variable_rate: false,
    })
}

fn take_parameter(data: &[u8], cursor: &mut usize) -> io::Result<Vec<u8>> {
    let count = be16(data, *cursor)? as usize;
    *cursor += 2;
    if count == 0 {
        return Err(invalid("Empty reference parameter set."));
    }
    let bytes = data
        .get(*cursor..*cursor + count)
        .ok_or_else(|| invalid("Truncated reference parameter set."))?;
    *cursor += count;
    Ok(bytes.to_vec())
}

fn parse_avcc(data: &[u8]) -> io::Result<Config> {
    if data.len() < 7 || data[0] != 1 || data[4] & 0xfc != 0xfc || data[5] & 0xe0 != 0xe0 {
        return Err(invalid("Invalid AVC configuration record."));
    }
    let mut config = empty_config(Codec::Avc, (data[4] & 3) as usize + 1)?;
    let mut cursor = 6;
    for _ in 0..(data[5] & 31) {
        let nal = take_parameter(data, &mut cursor)?;
        config.avc_sps.push(parse_avc_sps(&nal)?);
        config.parameters.push(nal);
    }
    let pps_count = *data
        .get(cursor)
        .ok_or_else(|| invalid("Missing AVC PPS count."))?;
    cursor += 1;
    for _ in 0..pps_count {
        let nal = take_parameter(data, &mut cursor)?;
        config.avc_pps.push(parse_avc_pps(&nal)?);
        config.parameters.push(nal);
    }
    if config.avc_sps.is_empty() || config.avc_pps.is_empty() {
        return Err(invalid("AVC reference needs SPS and PPS."));
    }
    for pps in &config.avc_pps {
        if !config.avc_sps.iter().any(|s| s.id == pps.sps) {
            return Err(invalid("AVC PPS refers to an absent SPS."));
        }
    }
    if duplicate_ids(config.avc_sps.iter().map(|s| s.id))
        || duplicate_ids(config.avc_pps.iter().map(|p| p.id))
    {
        return Err(invalid("Duplicate AVC parameter-set ids."));
    }
    // High-profile avcC may append chroma/bit-depth fields. SPS extensions are
    // unsupported, and must not be silently discarded from a configuration.
    if cursor < data.len()
        && (data.len() - cursor < 4
            || data[cursor] & 0xfc != 0xfc
            || data[cursor + 1] & 0xf8 != 0xf8
            || data[cursor + 2] & 0xf8 != 0xf8
            || data[cursor + 3] != 0
            || cursor + 4 != data.len())
    {
        return Err(invalid("Unsupported AVC configuration extension."));
    }
    Ok(config)
}

fn duplicate_ids(ids: impl Iterator<Item = u32>) -> bool {
    let mut seen = Vec::new();
    for id in ids {
        if seen.contains(&id) {
            return true;
        }
        seen.push(id);
    }
    false
}

fn parse_hvcc(data: &[u8]) -> io::Result<Config> {
    if data.len() < 23 || data[0] != 1 {
        return Err(invalid("Invalid HEVC configuration record."));
    }
    let mut config = empty_config(Codec::Hevc, (data[21] & 3) as usize + 1)?;
    let arrays = data[22] as usize;
    if arrays > 64 {
        return Err(invalid("Excessive HEVC parameter arrays."));
    }
    let mut cursor = 23;
    let mut vps_ids = Vec::new();
    for _ in 0..arrays {
        let kind = *data
            .get(cursor)
            .ok_or_else(|| invalid("Truncated HEVC parameter array."))?
            & 63;
        cursor += 1;
        let count = be16(data, cursor)? as usize;
        cursor += 2;
        if count > 256 || config.parameters.len() + count > 256 {
            return Err(invalid("Too many HEVC parameter sets."));
        }
        for _ in 0..count {
            let nal = take_parameter(data, &mut cursor)?;
            if hevc_header(&nal)? != kind {
                return Err(invalid("HEVC array type differs from NAL header."));
            }
            match kind {
                32 => {
                    if nal.len() < 6 {
                        return Err(invalid("Truncated HEVC VPS."));
                    }
                    vps_ids.push((nal[2] >> 4) as u32);
                    config.parameters.push(nal);
                }
                33 => {
                    config.hevc_sps.push(parse_hevc_sps(&nal)?);
                    config.parameters.push(nal);
                }
                34 => {
                    config.hevc_pps.push(parse_hevc_pps(&nal)?);
                    config.parameters.push(nal);
                }
                39 | 40 => {} // Ancillary reference SEI is not target metadata.
                _ => return Err(invalid("Unsupported HEVC configuration NAL type.")),
            }
        }
    }
    if cursor != data.len()
        || vps_ids.is_empty()
        || config.hevc_sps.is_empty()
        || config.hevc_pps.is_empty()
    {
        return Err(invalid(
            "HEVC reference needs valid VPS, SPS and PPS arrays.",
        ));
    }
    if duplicate_ids(vps_ids.iter().copied())
        || duplicate_ids(config.hevc_sps.iter().map(|s| s.id))
        || duplicate_ids(config.hevc_pps.iter().map(|p| p.id))
    {
        return Err(invalid("Duplicate HEVC parameter-set ids."));
    }
    for sps in &config.hevc_sps {
        if !vps_ids.contains(&sps.vps) {
            return Err(invalid("HEVC SPS refers to an absent VPS."));
        }
    }
    for pps in &config.hevc_pps {
        if !config.hevc_sps.iter().any(|s| s.id == pps.sps) {
            return Err(invalid("HEVC PPS refers to an absent SPS."));
        }
    }
    config.parameters.sort_by_key(|nal| (nal[0] >> 1) & 63);
    Ok(config)
}

struct Bits {
    data: Vec<u8>,
    position: usize,
}
impl Bits {
    fn from_ebsp(data: &[u8]) -> io::Result<Self> {
        Self::from_ebsp_with_tail(data, false)
    }
    fn from_ebsp_with_tail(data: &[u8], partial: bool) -> io::Result<Self> {
        let mut bytes = Vec::with_capacity(data.len());
        let mut zeroes = 0;
        let mut pos = 0;
        while pos < data.len() {
            let byte = data[pos];
            if zeroes >= 2 && byte == 3 {
                // A bounded slice-header prefix can end inside 00 00 03 xx.
                // Do not mistake the uninspected payload suffix for EOF.
                if partial && pos + 1 == data.len() {
                    break;
                }
                if data.get(pos + 1).is_none_or(|v| *v > 3) {
                    return Err(invalid("Invalid emulation-prevention byte."));
                }
                zeroes = 0;
                pos += 1;
                continue;
            }
            bytes.push(byte);
            zeroes = if byte == 0 { zeroes + 1 } else { 0 };
            pos += 1;
        }
        Ok(Self {
            data: bytes,
            position: 0,
        })
    }
    fn n(&mut self, count: u32) -> io::Result<u32> {
        if count > 32 || self.position + count as usize > self.data.len() * 8 {
            return Err(invalid("Truncated bitstream field."));
        }
        let mut value = 0;
        for _ in 0..count {
            value = (value << 1)
                | ((self.data[self.position / 8] >> (7 - self.position % 8)) & 1) as u32;
            self.position += 1;
        }
        Ok(value)
    }
    fn skip(&mut self, count: usize) -> io::Result<()> {
        if self
            .position
            .checked_add(count)
            .is_none_or(|v| v > self.data.len() * 8)
        {
            return Err(invalid("Truncated bitstream."));
        }
        self.position += count;
        Ok(())
    }
    fn ue(&mut self) -> io::Result<u32> {
        let mut zeroes = 0;
        while self.n(1)? == 0 {
            zeroes += 1;
            if zeroes > 30 {
                return Err(invalid("Excessive Exp-Golomb field."));
            }
        }
        Ok(((1u32 << zeroes) - 1) + self.n(zeroes)?)
    }
    fn se(&mut self) -> io::Result<i32> {
        let value = self.ue()?;
        Ok(if value & 1 == 0 {
            -((value / 2) as i32)
        } else {
            (value / 2 + 1) as i32
        })
    }
    fn bounded_ue(&mut self, max: u32) -> io::Result<u32> {
        let value = self.ue()?;
        if value > max {
            Err(invalid("Bitstream field exceeds supported bounds."))
        } else {
            Ok(value)
        }
    }
}

struct AvcSps {
    id: u32,
    frame_bits: u32,
    poc_kind: u32,
    poc_bits: u32,
    delta_always_zero: bool,
    macroblocks: u32,
}
struct AvcPps {
    id: u32,
    sps: u32,
    bottom_poc: bool,
}
fn parse_avc_sps(nal: &[u8]) -> io::Result<AvcSps> {
    if nal.len() < 5 || nal[0] & 0x9f != 7 || nal[0] & 0x60 == 0 {
        return Err(invalid("Invalid AVC SPS NAL."));
    }
    let mut bits = Bits::from_ebsp(&nal[1..])?;
    let profile = bits.n(8)?;
    let constraints = bits.n(8)?;
    bits.n(8)?;
    if constraints & 3 != 0 {
        return Err(invalid("Reserved AVC SPS constraint bits."));
    }
    let id = bits.bounded_ue(31)?;
    if [100, 110, 122, 244, 44, 83, 86, 118, 128, 138, 139, 134, 135].contains(&profile) {
        let chroma = bits.bounded_ue(3)?;
        if chroma == 3 && bits.n(1)? != 0 {
            return Err(invalid("AVC separate colour planes are unsupported."));
        }
        bits.bounded_ue(6)?;
        bits.bounded_ue(6)?;
        bits.n(1)?;
        if bits.n(1)? != 0 {
            for i in 0..if chroma == 3 { 12 } else { 8 } {
                if bits.n(1)? != 0 {
                    let mut last = 8i32;
                    let mut next = 8i32;
                    for _ in 0..if i < 6 { 16 } else { 64 } {
                        if next != 0 {
                            let delta = bits.se()?;
                            next = (last + delta).rem_euclid(256);
                        }
                        if next != 0 {
                            last = next;
                        }
                    }
                }
            }
        }
    } else if ![66, 77, 88].contains(&profile) {
        return Err(invalid("Unsupported AVC profile."));
    }
    let frame_bits = bits.bounded_ue(12)? + 4;
    let poc_kind = bits.bounded_ue(2)?;
    let mut poc_bits = 0;
    let mut delta_always_zero = false;
    if poc_kind == 0 {
        poc_bits = bits.bounded_ue(12)? + 4;
    }
    if poc_kind == 1 {
        delta_always_zero = bits.n(1)? != 0;
        bits.se()?;
        bits.se()?;
        let cycle = bits.bounded_ue(255)?;
        for _ in 0..cycle {
            bits.se()?;
        }
    }
    bits.bounded_ue(16)?;
    bits.n(1)?;
    let width = bits.bounded_ue(1023)? + 1;
    let height = bits.bounded_ue(1023)? + 1;
    if bits.n(1)? == 0 {
        return Err(invalid("Interlaced AVC is unsupported by native salvage."));
    }
    bits.n(1)?;
    if bits.n(1)? != 0 {
        for _ in 0..4 {
            bits.bounded_ue(16_384)?;
        }
    }
    bits.n(1)?; // vui_parameters_present_flag; timing comes from stts, not VUI.
    Ok(AvcSps {
        id,
        frame_bits,
        poc_kind,
        poc_bits,
        delta_always_zero,
        macroblocks: width * height,
    })
}
fn parse_avc_pps(nal: &[u8]) -> io::Result<AvcPps> {
    if nal.len() < 2 || nal[0] & 0x9f != 8 || nal[0] & 0x60 == 0 {
        return Err(invalid("Invalid AVC PPS NAL."));
    }
    let mut bits = Bits::from_ebsp(&nal[1..])?;
    let id = bits.bounded_ue(255)?;
    let sps = bits.bounded_ue(31)?;
    bits.n(1)?;
    let bottom_poc = bits.n(1)? != 0;
    if bits.ue()? != 0 {
        return Err(invalid("AVC slice groups/FMO are unsupported."));
    }
    bits.bounded_ue(31)?;
    bits.bounded_ue(31)?;
    bits.n(1)?;
    bits.n(2)?;
    bits.se()?;
    bits.se()?;
    bits.se()?;
    bits.n(1)?;
    bits.n(1)?;
    if bits.n(1)? != 0 {
        return Err(invalid("Redundant AVC pictures are unsupported."));
    }
    Ok(AvcPps {
        id,
        sps,
        bottom_poc,
    })
}

struct HevcSps {
    id: u32,
    vps: u32,
    poc_bits: u32,
    ctbs: u32,
    separate_plane: bool,
}
struct HevcPps {
    id: u32,
    sps: u32,
    dependent: bool,
    output_flag: bool,
    extra_bits: u32,
}
fn hevc_header(nal: &[u8]) -> io::Result<u8> {
    if nal.len() < 2 || nal[0] & 0x80 != 0 || nal[0] & 1 != 0 || nal[1] >> 3 != 0 || nal[1] & 7 == 0
    {
        return Err(invalid("Invalid or nonzero-layer HEVC NAL header."));
    }
    Ok((nal[0] >> 1) & 63)
}
fn parse_hevc_sps(nal: &[u8]) -> io::Result<HevcSps> {
    if hevc_header(nal)? != 33 {
        return Err(invalid("Expected HEVC SPS."));
    }
    let mut bits = Bits::from_ebsp(&nal[2..])?;
    let vps = bits.n(4)?;
    let layers = bits.n(3)?;
    if layers > 6 {
        return Err(invalid("Invalid HEVC sublayer count."));
    }
    bits.n(1)?;
    bits.skip(96)?;
    let mut flags = Vec::new();
    for _ in 0..layers {
        flags.push((bits.n(1)?, bits.n(1)?));
    }
    if layers > 0 {
        bits.skip((8 - layers) as usize * 2)?;
    }
    for (profile, level) in flags {
        if profile != 0 {
            bits.skip(88)?;
        }
        if level != 0 {
            bits.skip(8)?;
        }
    }
    let id = bits.bounded_ue(15)?;
    let chroma = bits.bounded_ue(3)?;
    let separate_plane = chroma == 3 && bits.n(1)? != 0;
    let width = bits.bounded_ue(16_384)?;
    let height = bits.bounded_ue(16_384)?;
    if width == 0 || height == 0 {
        return Err(invalid("Zero HEVC dimensions."));
    }
    if bits.n(1)? != 0 {
        for _ in 0..4 {
            bits.bounded_ue(16_384)?;
        }
    }
    bits.bounded_ue(8)?;
    bits.bounded_ue(8)?;
    let poc_bits = bits.bounded_ue(12)? + 4;
    let start = if bits.n(1)? != 0 { 0 } else { layers };
    for _ in start..=layers {
        bits.bounded_ue(16)?;
        bits.bounded_ue(16)?;
        bits.ue()?;
    }
    let min_cb = bits.bounded_ue(3)? + 3;
    let diff = bits.bounded_ue(3)?;
    if min_cb + diff > 6 {
        return Err(invalid("Unsupported HEVC coding-tree size."));
    }
    let size = 1u32 << (min_cb + diff);
    let ctbs = width.div_ceil(size) * height.div_ceil(size);
    Ok(HevcSps {
        id,
        vps,
        poc_bits,
        ctbs,
        separate_plane,
    })
}
fn parse_hevc_pps(nal: &[u8]) -> io::Result<HevcPps> {
    if hevc_header(nal)? != 34 {
        return Err(invalid("Expected HEVC PPS."));
    }
    let mut bits = Bits::from_ebsp(&nal[2..])?;
    Ok(HevcPps {
        id: bits.bounded_ue(63)?,
        sps: bits.bounded_ue(15)?,
        dependent: bits.n(1)? != 0,
        output_flag: bits.n(1)? != 0,
        extra_bits: bits.n(3)?,
    })
}

/// A source window avoids a system call for every byte when searching for a prefix.
struct Window<'a> {
    source: &'a Source,
    start: u64,
    data: Vec<u8>,
}
impl<'a> Window<'a> {
    fn new(source: &'a Source) -> Self {
        Self {
            source,
            start: 0,
            data: Vec::new(),
        }
    }
    fn get(&mut self, offset: u64, length: usize) -> io::Result<&[u8]> {
        let end = offset
            .checked_add(length as u64)
            .ok_or_else(|| invalid("Read overflow."))?;
        if length > WINDOW || end > self.source.fingerprint.size {
            return Err(invalid("Scan read exceeds bounds."));
        }
        if offset < self.start || end > self.start + self.data.len() as u64 {
            self.start = offset;
            let amount = (self.source.fingerprint.size - offset).min(WINDOW as u64) as usize;
            self.data.resize(amount, 0);
            self.source.read_at(offset, &mut self.data)?;
        }
        let at = (offset - self.start) as usize;
        Ok(&self.data[at..at + length])
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct Picture {
    pps: u32,
    frame: u32,
    poc: i32,
    delta: i32,
    idr_id: u32,
    reference: bool,
    nal_type: u8,
    temporal: u8,
}
struct Slice {
    first: bool,
    safe: bool,
    identity: Picture,
}
impl Slice {
    fn same_picture(&self, other: &Picture, codec: Codec) -> bool {
        if codec == Codec::Avc {
            self.identity == *other
        } else {
            self.identity.pps == other.pps
                && self.identity.nal_type == other.nal_type
                && self.identity.temporal == other.temporal
                && (self.identity.poc == i32::MIN || self.identity.poc == other.poc)
        }
    }
}
struct Nal {
    length: u64,
    slice: Option<Slice>,
    aud: bool,
    incompatible: bool,
}
impl Nal {
    fn safe_start(&self) -> bool {
        self.slice.as_ref().is_some_and(|s| s.safe && s.first)
    }
}

fn candidate(
    reader: &mut Window<'_>,
    pos: u64,
    end: u64,
    config: &Config,
) -> io::Result<Option<Nal>> {
    let prefix = config.length_size;
    if end - pos < prefix as u64 + 2 {
        return Ok(None);
    }
    let length = reader
        .get(pos, prefix)?
        .iter()
        .fold(0u64, |n, b| (n << 8) | *b as u64);
    if length == 0 || length > MAX_NAL || length + prefix as u64 > end - pos {
        return Ok(None);
    }
    let bytes = reader.get(pos + prefix as u64, length.min(128) as usize)?;
    let kind = match config.codec {
        Codec::Avc => {
            if bytes[0] & 0x80 != 0 {
                return Ok(None);
            }
            bytes[0] & 31
        }
        Codec::Hevc => match hevc_header(bytes) {
            Ok(t) => t,
            Err(_) => return Ok(None),
        },
    };
    let vcl = match config.codec {
        Codec::Avc => matches!(kind, 1 | 5),
        Codec::Hevc => matches!(kind,0..=9|16..=21),
    };
    if vcl {
        if length < 5 {
            return Ok(None);
        }
        let slice = match config.codec {
            Codec::Avc => avc_slice(bytes, config, length > bytes.len() as u64),
            Codec::Hevc => hevc_slice(bytes, config, length > bytes.len() as u64),
        };
        return Ok(slice.ok().map(|slice| Nal {
            length,
            slice: Some(slice),
            aud: false,
            incompatible: false,
        }));
    }
    let parameter = match config.codec {
        Codec::Avc => matches!(kind, 7 | 8),
        Codec::Hevc => matches!(kind, 32..=34),
    };
    if parameter {
        if length > u16::MAX as u64 {
            return Ok(None);
        }
        let bytes = reader.get(pos + prefix as u64, length as usize)?;
        if !config.parameters.iter().any(|p| p.as_slice() == bytes) {
            let valid = match (config.codec, kind) {
                (Codec::Avc, 7) => parse_avc_sps(bytes).is_ok(),
                (Codec::Avc, 8) => parse_avc_pps(bytes).is_ok(),
                (Codec::Hevc, 32) => bytes.len() >= 6,
                (Codec::Hevc, 33) => parse_hevc_sps(bytes).is_ok(),
                (Codec::Hevc, 34) => parse_hevc_pps(bytes).is_ok(),
                _ => false,
            };
            if valid {
                return Ok(Some(Nal {
                    length,
                    slice: None,
                    aud: false,
                    incompatible: true,
                }));
            }
            return Ok(None);
        }
        return Ok(Some(Nal {
            length,
            slice: None,
            aud: false,
            incompatible: false,
        }));
    }
    let aud = match config.codec {
        Codec::Avc => kind == 9,
        Codec::Hevc => kind == 35,
    };
    if aud {
        let valid = match config.codec {
            Codec::Avc => length == 2 && bytes[0] == 9 && bytes[1] & 31 == 16,
            Codec::Hevc => length == 3 && bytes[2] >> 5 <= 2 && bytes[2] & 31 == 16,
        };
        return Ok(valid.then_some(Nal {
            length,
            slice: None,
            aud: true,
            incompatible: false,
        }));
    }
    // Retain ordinary SEI only inside a confirmed chain. Reject unknown/reserved
    // NALs rather than assuming any plausible length makes a video record.
    let sei = match config.codec {
        Codec::Avc => kind == 6 && bytes[0] & 0x60 == 0,
        Codec::Hevc => matches!(kind, 39 | 40),
    };
    Ok(
        (sei && length >= 3 && length <= WINDOW as u64).then_some(Nal {
            length,
            slice: None,
            aud: false,
            incompatible: false,
        }),
    )
}

fn avc_slice(nal: &[u8], config: &Config, partial: bool) -> io::Result<Slice> {
    let kind = nal[0] & 31;
    let reference = nal[0] & 0x60 != 0;
    let mut bits = Bits::from_ebsp_with_tail(&nal[1..], partial)?;
    let first_mb = bits.ue()?;
    let slice_type = bits.bounded_ue(9)? % 5;
    // SP/SI and data partitioning need additional recovery logic.
    if slice_type > 2 || kind == 5 && (!reference || slice_type != 2) {
        return Err(invalid("Unsupported AVC slice type."));
    }
    let pps_id = bits.bounded_ue(255)?;
    let pps = config
        .avc_pps
        .iter()
        .find(|p| p.id == pps_id)
        .ok_or_else(|| invalid("Unknown AVC slice PPS."))?;
    let sps = config
        .avc_sps
        .iter()
        .find(|s| s.id == pps.sps)
        .ok_or_else(|| invalid("Unknown AVC slice SPS."))?;
    if first_mb >= sps.macroblocks {
        return Err(invalid("AVC slice address exceeds picture."));
    }
    let frame = bits.n(sps.frame_bits)?;
    let idr_id = if kind == 5 {
        bits.bounded_ue(65_535)?
    } else {
        0
    };
    if kind == 5 && frame != 0 {
        return Err(invalid("AVC IDR frame_num must be zero."));
    }
    let mut poc = 0;
    let mut delta = 0;
    if sps.poc_kind == 0 {
        poc = bits.n(sps.poc_bits)? as i32;
        if pps.bottom_poc {
            delta = bits.se()?;
        }
    }
    if sps.poc_kind == 1 && !sps.delta_always_zero {
        poc = bits.se()?;
        if pps.bottom_poc {
            delta = bits.se()?;
        }
    }
    Ok(Slice {
        first: first_mb == 0,
        safe: kind == 5,
        identity: Picture {
            pps: pps_id,
            frame,
            poc,
            delta,
            idr_id,
            reference,
            nal_type: kind,
            temporal: 0,
        },
    })
}

fn hevc_slice(nal: &[u8], config: &Config, partial: bool) -> io::Result<Slice> {
    let kind = hevc_header(nal)?;
    let temporal = nal[1] & 7;
    let mut bits = Bits::from_ebsp_with_tail(&nal[2..], partial)?;
    let first = bits.n(1)? != 0;
    if (16..=21).contains(&kind) {
        bits.n(1)?;
        if temporal != 1 {
            return Err(invalid("HEVC IRAP must use temporal layer zero."));
        }
    }
    let pps_id = bits.bounded_ue(63)?;
    let pps = config
        .hevc_pps
        .iter()
        .find(|p| p.id == pps_id)
        .ok_or_else(|| invalid("Unknown HEVC slice PPS."))?;
    let sps = config
        .hevc_sps
        .iter()
        .find(|s| s.id == pps.sps)
        .ok_or_else(|| invalid("Unknown HEVC slice SPS."))?;
    let mut dependent = false;
    if !first {
        if pps.dependent {
            dependent = bits.n(1)? != 0;
        }
        let address_bits = 32 - (sps.ctbs - 1).leading_zeros();
        let address = bits.n(address_bits)?;
        if address == 0 || address >= sps.ctbs {
            return Err(invalid("HEVC slice segment address is invalid."));
        }
    }
    let safe = matches!(kind, 19 | 20);
    let mut poc = if dependent { i32::MIN } else { 0 };
    if !dependent {
        bits.n(pps.extra_bits)?;
        let slice_type = bits.bounded_ue(2)?;
        if safe && slice_type != 2 {
            return Err(invalid("HEVC IDR must contain an I slice."));
        }
        if pps.output_flag {
            bits.n(1)?;
        }
        if sps.separate_plane {
            bits.n(2)?;
        }
        if !safe {
            poc = bits.n(sps.poc_bits)? as i32;
        }
    }
    Ok(Slice {
        first,
        safe,
        identity: Picture {
            pps: pps_id,
            frame: 0,
            poc,
            delta: 0,
            idr_id: 0,
            reference: true,
            nal_type: kind,
            temporal,
        },
    })
}

fn viable_chain(
    reader: &mut Window<'_>,
    start: u64,
    end: u64,
    config: &Config,
) -> io::Result<bool> {
    chain(reader, start, end, config, true, false)
}

fn chain(
    reader: &mut Window<'_>,
    start: u64,
    end: u64,
    config: &Config,
    require_idr: bool,
    allow_incompatible: bool,
) -> io::Result<bool> {
    let mut pos = start;
    let mut vcl = 0;
    let mut pictures = 0;
    let mut picture = None;
    for _ in 0..128 {
        let Some(nal) = candidate(reader, pos, end, config)? else {
            return Ok(false);
        };
        if (pos == start && require_idr && !nal.safe_start())
            || (nal.incompatible && !allow_incompatible)
        {
            return Ok(false);
        }
        if let Some(slice) = nal.slice {
            if slice.first {
                pictures += 1;
                picture = Some(slice.identity);
            } else if picture
                .as_ref()
                .is_none_or(|id| !slice.same_picture(id, config.codec))
            {
                return Ok(false);
            }
            vcl += 1;
        } else if nal.aud {
            picture = None;
        }
        if vcl >= 3 && pictures >= 2 {
            return Ok(true);
        }
        pos += nal.length + config.length_size as u64;
        if pos >= end || pos - start > 2 * MAX_NAL {
            return Ok(false);
        }
    }
    Ok(false)
}
