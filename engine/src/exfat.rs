//! Read-only, metadata-led exFAT recovery. This module does not guess lost chains.
//!
//! Structures and checksums follow Microsoft's exFAT 1.00 specification:
//! https://learn.microsoft.com/en-us/windows/win32/fileio/exfat-specification
use crate::{
    SourceFingerprint,
    source::{Source, invalid},
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::HashSet,
    ffi::CString,
    fs::File,
    io::{self, Write},
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::ffi::OsStrExt,
    },
    path::{Component, Path},
};

const MAX_CHAIN_CLUSTERS: u64 = 4_194_304;
const MAX_TOTAL_CHAIN_CLUSTERS: u64 = 16_777_216;
const MAX_EXTENTS: usize = 262_144;
const MAX_DIRECTORY_BYTES: u64 = 32 * 1024 * 1024;
const MAX_TOTAL_DIRECTORY_BYTES: u64 = 128 * 1024 * 1024;
const MAX_BITMAP_BYTES: u64 = 64 * 1024 * 1024;
const MAX_BITMAP_WORK: u64 = 256 * 1024 * 1024;
const MAX_ENTRIES: u64 = 1_000_000;
const MAX_DIRECTORIES: usize = 4096;
const MAX_CANDIDATES: usize = 100_000;
const MAX_WARNINGS: usize = 1000;
const MAX_CLAIMS: usize = 1_000_000;
const MAX_DEPTH: usize = 32;
const METADATA_OWNER: u64 = u64::MAX;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Extent {
    pub offset: u64,
    pub length: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExfatCandidate {
    pub id: u64,
    pub name: String,
    pub length: u64,
    pub extents: Vec<Extent>,
    pub deleted: bool,
    pub recoverable: bool,
    pub reason: Option<String>,
    pub allocated_clusters: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExfatScan {
    pub source: SourceFingerprint,
    pub candidates: Vec<ExfatCandidate>,
    pub warnings: Vec<String>,
}

/// Discover MOV/MP4 entries whose on-disk metadata can describe their byte order.
/// `recoverable` only means extraction is supported; it is not a decode verdict.
pub fn analyze(path: &Path) -> io::Result<ExfatScan> {
    let source = Source::open(path)?;
    let scan = analyze_source(&source)?;
    source.unchanged()?;
    Ok(scan)
}

/// Revalidate the original scan, create a new output, and copy the logical bytes.
/// Errors retain any partial output for the enclosing recovery session to report.
pub fn extract(
    path: &Path,
    output: &Path,
    expected_source: &SourceFingerprint,
    candidate: &ExfatCandidate,
    mut progress: impl FnMut(u64, u64) -> io::Result<()>,
) -> io::Result<String> {
    let source = Source::open(path)?;
    if &source.fingerprint != expected_source {
        return Err(invalid("Source changed; scan the image again"));
    }
    validate_extents(
        &candidate.extents,
        candidate.length,
        source.fingerprint.size,
    )?;
    let scan = analyze_source(&source)?;
    let found = scan
        .candidates
        .iter()
        .find(|entry| entry.id == candidate.id);
    if found != Some(candidate) || !candidate.recoverable {
        return Err(invalid(
            "Candidate no longer matches verified exFAT metadata",
        ));
    }
    source.unchanged()?;
    progress(0, candidate.length)?;
    let mut output = create_output(output, candidate.length)?;
    let mut buffer = vec![0; 1024 * 1024];
    let mut copied = 0;
    let mut hash = Sha256::new();
    for extent in &candidate.extents {
        let mut position = 0;
        while position < extent.length {
            let count = (extent.length - position).min(buffer.len() as u64) as usize;
            source.read_at(extent.offset + position, &mut buffer[..count])?;
            output.write_all(&buffer[..count])?;
            hash.update(&buffer[..count]);
            copied += count as u64;
            position += count as u64;
            progress(copied, candidate.length)?;
        }
    }
    output.sync_all()?;
    source.unchanged()?;
    Ok(format!("{:x}", hash.finalize()))
}

fn unsupported(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::Unsupported, message)
}

fn le16(bytes: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([bytes[at], bytes[at + 1]])
}
fn le32(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]])
}
fn le64(bytes: &[u8], at: usize) -> u64 {
    u64::from(le32(bytes, at)) | (u64::from(le32(bytes, at + 4)) << 32)
}

#[derive(Debug, PartialEq, Eq)]
struct Geometry {
    base: u64,
    end: u64,
    fat_offset: u64,
    fat_length: u64,
    heap: u64,
    cluster_size: u64,
    cluster_count: u32,
    root: u32,
}

fn boot(
    source: &Source,
    base: u64,
    available: u64,
    offset: u64,
    sector: u64,
) -> io::Result<(Geometry, u16)> {
    if available < offset + sector * 12 {
        return Err(invalid("Truncated exFAT boot region"));
    }
    let mut bytes = vec![0; (sector * 12) as usize];
    source.read_at(base + offset, &mut bytes)?;
    if bytes[..3] != [0xeb, 0x76, 0x90]
        || &bytes[3..11] != b"EXFAT   "
        || le16(&bytes, 510) != 0xaa55
        || bytes[11..64].iter().any(|&byte| byte != 0)
        || !(9..=12).contains(&bytes[108])
        || (1u64 << bytes[108]) != sector
        || bytes[109] > 25 - bytes[108]
    {
        return Err(invalid("Invalid exFAT boot signature or sector geometry"));
    }
    let checksum =
        bytes[..(sector * 11) as usize]
            .iter()
            .enumerate()
            .fold(0u32, |sum, (index, &byte)| {
                if [106, 107, 112].contains(&index) {
                    sum
                } else {
                    sum.rotate_right(1).wrapping_add(u32::from(byte))
                }
            });
    if bytes[(sector * 11) as usize..]
        .chunks_exact(4)
        .any(|value| le32(value, 0) != checksum)
        || (1..=8).any(|index| le32(&bytes, ((index + 1) * sector - 4) as usize) != 0xaa550000)
    {
        return Err(invalid("Invalid exFAT boot checksum or extended signature"));
    }
    if bytes[110] != 1 {
        return Err(unsupported("Unsupported layout: TexFAT / multiple FATs"));
    }
    if le16(&bytes, 104) != 0x100 {
        return Err(unsupported(
            "Unsupported exFAT revision; only 1.00 is supported",
        ));
    }
    let flags = le16(&bytes, 106);
    if flags & 1 != 0 || !(bytes[112] <= 100 || bytes[112] == 255) {
        return Err(invalid("Invalid exFAT active FAT or percentage"));
    }
    let volume_bytes = le64(&bytes, 72)
        .checked_mul(sector)
        .ok_or_else(|| invalid("exFAT volume length overflow"))?;
    let fat_offset = u64::from(le32(&bytes, 80)) * sector;
    let fat_length = u64::from(le32(&bytes, 84)) * sector;
    let heap = u64::from(le32(&bytes, 88)) * sector;
    let cluster_count = le32(&bytes, 92);
    let cluster_size = sector << bytes[109];
    let heap_length = u64::from(cluster_count) * cluster_size;
    let root = le32(&bytes, 96);
    if volume_bytes < 1024 * 1024
        || volume_bytes > available
        || fat_offset < 24 * sector
        || fat_length < (u64::from(cluster_count) + 2) * 4
        || fat_offset + fat_length > heap
        || heap > volume_bytes
        || cluster_count == 0
        || cluster_count > 0xfffffff5
        || heap_length > volume_bytes - heap
        || u64::from(cluster_count) != ((volume_bytes - heap) / cluster_size).min(0xfffffff5)
        || root < 2
        || root > cluster_count + 1
    {
        return Err(invalid("Invalid exFAT volume, FAT, heap, or root geometry"));
    }
    Ok((
        Geometry {
            base,
            end: base + volume_bytes,
            fat_offset: base + fat_offset,
            fat_length,
            heap: base + heap,
            cluster_size,
            cluster_count,
            root,
        },
        flags,
    ))
}

fn load_boot(source: &Source, base: u64, available: u64) -> io::Result<(Geometry, Vec<String>)> {
    let mut primary = None;
    let mut backup = None;
    let mut special_error = None;
    for shift in 9..=12 {
        let sector = 1 << shift;
        for (offset, target) in [(0, &mut primary), (12 * sector, &mut backup)] {
            match boot(source, base, available, offset, sector) {
                Ok(value) => {
                    if target.is_some() {
                        return Err(invalid("Ambiguous exFAT boot geometry"));
                    }
                    *target = Some(value);
                }
                Err(error) if error.kind() == io::ErrorKind::Unsupported => {
                    if offset == 0 {
                        return Err(error);
                    }
                    special_error = Some(error)
                }
                Err(_) => {}
            }
        }
    }
    let mut warnings = Vec::new();
    if let Some((geometry, flags)) = primary {
        if let Some((backup, _)) = backup {
            if geometry != backup {
                return Err(invalid("Main and backup exFAT geometry disagree"));
            }
        } else {
            warnings.push("Main boot verified; backup boot is invalid".to_owned());
        }
        if flags & 6 != 0 {
            warnings.push(
                "exFAT volume dirty/media-failure flags are set; metadata may be inconsistent"
                    .to_owned(),
            );
        }
        return Ok((geometry, warnings));
    }
    if let Some((geometry, _)) = backup {
        warnings.push("Using verified backup exFAT boot region; main boot is invalid and backup volume flags are stale".to_owned());
        return Ok((geometry, warnings));
    }
    Err(special_error.unwrap_or_else(|| invalid("No valid exFAT main or backup boot region")))
}

fn volumes(source: &Source) -> io::Result<Vec<(u64, u64)>> {
    let mut first = [0; 512];
    source.read_at(0, &mut first)?;
    if &first[3..11] == b"EXFAT   " {
        return Ok(vec![(0, source.fingerprint.size)]);
    }
    // Damaged volume boot code may otherwise resemble arbitrary MBR entries.
    if load_boot(source, 0, source.fingerprint.size).is_ok() {
        return Ok(vec![(0, source.fingerprint.size)]);
    }
    if le16(&first, 510) == 0xaa55 {
        let mut partitions = Vec::new();
        for entry in first[446..510].chunks_exact(16) {
            if entry[4] == 0xee {
                return Err(unsupported("Unsupported layout: GPT partition table"));
            }
            if entry[4] == 0 {
                continue;
            }
            if entry[4] != 7 {
                return Err(unsupported(
                    "Unsupported layout: MBR partition is not exFAT (type 0x07)",
                ));
            }
            let start = u64::from(le32(entry, 8)) * 512;
            let length = u64::from(le32(entry, 12)) * 512;
            if start < 512 || length == 0 || start + length > source.fingerprint.size {
                return Err(invalid("Invalid MBR partition bounds"));
            }
            partitions.push((start, length));
        }
        if !partitions.is_empty() {
            partitions.sort_unstable();
            if partitions
                .windows(2)
                .any(|pair| pair[0].0 + pair[0].1 > pair[1].0)
            {
                return Err(invalid("Overlapping MBR partitions"));
            }
            return Ok(partitions);
        }
    }
    Err(unsupported(
        "Unsupported layout: expected exFAT volume or MBR exFAT partition; FAT32/GPT are unsupported",
    ))
}

struct Parser<'a> {
    source: &'a Source,
    geometry: Geometry,
    fat_start: u64,
    fat_cache: Vec<u8>,
    chain_work: u64,
}

impl Parser<'_> {
    fn cluster_offset(&self, cluster: u32) -> io::Result<u64> {
        if cluster < 2 || cluster > self.geometry.cluster_count + 1 {
            return Err(invalid("Cluster index outside exFAT heap"));
        }
        Ok(self.geometry.heap + u64::from(cluster - 2) * self.geometry.cluster_size)
    }

    fn next(&mut self, cluster: u32) -> io::Result<u32> {
        let relative = u64::from(cluster) * 4;
        if relative + 4 > self.geometry.fat_length {
            return Err(invalid("Truncated FAT"));
        }
        let block = relative / 65536 * 65536;
        if self.fat_start != block || self.fat_cache.is_empty() {
            self.fat_cache
                .resize((self.geometry.fat_length - block).min(65536) as usize, 0);
            self.source
                .read_at(self.geometry.fat_offset + block, &mut self.fat_cache)?;
            self.fat_start = block;
        }
        Ok(le32(&self.fat_cache, (relative - block) as usize))
    }

    fn chain(
        &mut self,
        start: u32,
        length: Option<u64>,
        contiguous: bool,
        max_bytes: u64,
    ) -> io::Result<Vec<Extent>> {
        let mut extents = Vec::new();
        self.chain_into(start, length, contiguous, max_bytes, &mut extents)?;
        Ok(extents)
    }

    // Preserve the proven prefix on error: a damaged active stream still claims
    // its traversed clusters even though the complete stream cannot be extracted.
    fn chain_into(
        &mut self,
        start: u32,
        length: Option<u64>,
        contiguous: bool,
        max_bytes: u64,
        extents: &mut Vec<Extent>,
    ) -> io::Result<()> {
        let size = self.geometry.cluster_size;
        if length.is_some_and(|length| length == 0 || length > max_bytes) {
            return Err(invalid(
                "Invalid stream length or configured traversal limit exceeded",
            ));
        }
        let needed = length.map(|length| length.div_ceil(size));
        if needed.is_some_and(|count| count > u64::from(self.geometry.cluster_count)) {
            return Err(invalid("Stream length exceeds exFAT heap"));
        }
        if contiguous {
            let length = length.ok_or_else(|| invalid("Missing contiguous stream length"))?;
            let offset = self.cluster_offset(start)?;
            if u64::from(start - 2) + length.div_ceil(size) > u64::from(self.geometry.cluster_count)
            {
                return Err(invalid("Contiguous stream exceeds exFAT heap"));
            }
            extents.push(Extent { offset, length });
            return Ok(());
        }
        let mut seen = HashSet::new();
        let mut cluster = start;
        let mut total = 0;
        loop {
            self.chain_work += 1;
            if self.chain_work > MAX_TOTAL_CHAIN_CLUSTERS {
                return Err(unsupported("Total FAT traversal limit exceeded"));
            }
            let offset = self.cluster_offset(cluster)?;
            if !seen.insert(cluster) {
                return Err(invalid("Cyclic FAT chain"));
            }
            if seen.len() as u64 > MAX_CHAIN_CLUSTERS || total >= max_bytes {
                return Err(unsupported("FAT chain traversal limit exceeded"));
            }
            let amount = length.map_or(size, |length| (length - total).min(size));
            if let Some(last) = extents
                .last_mut()
                .filter(|last| last.offset + last.length == offset)
            {
                last.length += amount;
            } else {
                if extents.len() >= MAX_EXTENTS {
                    return Err(unsupported("File extent limit exceeded"));
                }
                extents.push(Extent {
                    offset,
                    length: amount,
                });
            }
            total += amount;
            let next = self.next(cluster)?;
            if next == u32::MAX {
                if length.is_some_and(|length| length != total) {
                    return Err(invalid("FAT chain shorter than stream length"));
                }
                break;
            }
            if length == Some(total) {
                return Err(invalid("FAT chain continues beyond declared stream length"));
            }
            if next < 2 || next > self.geometry.cluster_count + 1 {
                return Err(invalid("Invalid FAT chain terminator or next cluster"));
            }
            cluster = next;
        }
        Ok(())
    }

    fn read_stream(&self, extents: &[Extent], limit: u64) -> io::Result<Vec<u8>> {
        let length: u64 = extents.iter().map(|extent| extent.length).sum();
        if length > limit {
            return Err(unsupported("Metadata stream exceeds memory limit"));
        }
        let mut bytes = vec![0; length as usize];
        read_logical(self.source, extents, 0, &mut bytes)?;
        Ok(bytes)
    }
}

fn read_logical(
    source: &Source,
    extents: &[Extent],
    mut position: u64,
    mut bytes: &mut [u8],
) -> io::Result<()> {
    for extent in extents {
        if position >= extent.length {
            position -= extent.length;
            continue;
        }
        let count = (extent.length - position).min(bytes.len() as u64) as usize;
        source.read_at(extent.offset + position, &mut bytes[..count])?;
        bytes = &mut bytes[count..];
        if bytes.is_empty() {
            return Ok(());
        }
        position = 0;
    }
    Err(invalid("Logical stream is shorter than requested bytes"))
}

fn physical_position(extents: &[Extent], mut position: u64) -> io::Result<u64> {
    for extent in extents {
        if position < extent.length {
            return Ok(extent.offset + position);
        }
        position -= extent.length;
    }
    Err(invalid("Directory entry lies outside its stream"))
}

struct FileEntry {
    name: String,
    first: u32,
    length: u64,
    valid_length: u64,
    contiguous: bool,
    deleted: bool,
    directory: bool,
}

fn file_entry(entries: &[u8]) -> io::Result<FileEntry> {
    if entries.len() < 96 {
        return Err(invalid("File entry set lacks stream/name entries"));
    }
    let deleted = entries[0] == 5;
    let mut sum = 0u16;
    for (index, &original) in entries.iter().enumerate() {
        if index == 2 || index == 3 {
            continue;
        }
        let byte = if deleted && index % 32 == 0 {
            original | 0x80
        } else {
            original
        };
        sum = sum.rotate_right(1).wrapping_add(u16::from(byte));
    }
    if sum != le16(entries, 2) {
        return Err(invalid("Invalid file entry set checksum"));
    }
    if entries
        .chunks_exact(32)
        .any(|entry| (entry[0] & 0x80 == 0) != deleted)
        || entries[32] & 0x7f != 0x40
        || entries[33] & 1 == 0
        || entries[33] & !3 != 0
    {
        return Err(invalid("Invalid stream entry or mixed InUse state"));
    }
    let name_length = entries[35] as usize;
    let names = name_length.div_ceil(15);
    if name_length == 0 || entries.len() < (2 + names) * 32 {
        return Err(invalid("Invalid exFAT filename length"));
    }
    let mut name = Vec::with_capacity(name_length);
    for entry in entries[64..(2 + names) * 32].chunks_exact(32) {
        if entry[0] & 0x7f != 0x41 || entry[1] & 3 != 0 {
            return Err(invalid("Invalid filename entry order/flags"));
        }
        for at in (2..32).step_by(2) {
            if name.len() < name_length {
                name.push(le16(entry, at));
            }
        }
    }
    if entries[(2 + names) * 32..]
        .chunks_exact(32)
        .any(|entry| entry[0] & 0x60 != 0x60)
    {
        return Err(unsupported("Unsupported critical secondary exFAT entry"));
    }
    let name = String::from_utf16(&name).map_err(|_| invalid("Invalid UTF-16 filename"))?;
    if name == "."
        || name == ".."
        || name
            .chars()
            .any(|character| character < ' ' || "\"*/:<>?\\|".contains(character))
    {
        return Err(invalid("Invalid exFAT filename characters"));
    }
    Ok(FileEntry {
        name,
        first: le32(entries, 52),
        length: le64(entries, 56),
        valid_length: le64(entries, 40),
        contiguous: entries[33] & 2 != 0,
        deleted,
        directory: le16(entries, 4) & 0x10 != 0,
    })
}

fn allocated_count(
    geometry: &Geometry,
    bitmap: &[u8],
    extents: &[Extent],
    work: &mut u64,
) -> io::Result<u64> {
    let mut total = 0;
    for extent in extents {
        let mut first = (extent.offset - geometry.heap) / geometry.cluster_size;
        let end = first + extent.length.div_ceil(geometry.cluster_size);
        *work += (end - first).div_ceil(8);
        if *work > MAX_BITMAP_WORK {
            return Err(unsupported(
                "Total allocation bitmap scan-work limit exceeded",
            ));
        }
        while first < end && first % 8 != 0 {
            total += u64::from(bitmap[(first / 8) as usize] >> (first % 8) & 1);
            first += 1;
        }
        let aligned_end = end / 8 * 8;
        if first < aligned_end {
            total += bitmap[(first / 8) as usize..(aligned_end / 8) as usize]
                .iter()
                .map(|byte| u64::from(byte.count_ones()))
                .sum::<u64>();
            first = aligned_end;
        }
        while first < end {
            total += u64::from(bitmap[(first / 8) as usize] >> (first % 8) & 1);
            first += 1;
        }
    }
    Ok(total)
}

fn cluster_count(geometry: &Geometry, extents: &[Extent]) -> u64 {
    extents
        .iter()
        .map(|extent| extent.length.div_ceil(geometry.cluster_size))
        .sum()
}

fn container_header(source: &Source, extents: &[Extent], length: u64) -> io::Result<()> {
    if length < 24 {
        return Err(invalid("Video stream too short for a container header"));
    }
    let mut position = 0;
    // Permit a few leading QuickTime free/skip/wide atoms without scanning payload.
    for _ in 0..8 {
        let mut header = [0; 16];
        read_logical(source, extents, position, &mut header[..8])?;
        let short = u32::from_be_bytes([header[0], header[1], header[2], header[3]]);
        let (size, header_length) = if short == 1 {
            read_logical(source, extents, position + 8, &mut header[8..])?;
            (
                u64::from_be_bytes([
                    header[8], header[9], header[10], header[11], header[12], header[13],
                    header[14], header[15],
                ]),
                16,
            )
        } else {
            (u64::from(short), 8)
        };
        if size < header_length || size > length - position {
            return Err(invalid("Invalid MOV/MP4 opening atom size"));
        }
        if &header[4..8] == b"ftyp" {
            if size < header_length + 8 || (size - header_length) % 4 != 0 {
                return Err(invalid("Invalid ftyp atom length"));
            }
            let mut brand = [0; 4];
            read_logical(source, extents, position + header_length, &mut brand)?;
            if !brand
                .iter()
                .all(|byte| byte.is_ascii_graphic() || *byte == b' ')
            {
                return Err(invalid("Invalid ftyp major brand"));
            }
            return Ok(());
        }
        if ![b"free", b"skip", b"wide"]
            .iter()
            .any(|kind| kind.as_slice() == &header[4..8])
        {
            return Err(invalid(
                "MOV/MP4 container header not verified (ftyp required)",
            ));
        }
        position += size;
        if position > length - 8 {
            break;
        }
    }
    Err(invalid(
        "MOV/MP4 container header not found within opening atoms",
    ))
}

fn warning(warnings: &mut Vec<String>, message: String) {
    if warnings.len() < MAX_WARNINGS {
        warnings.push(message);
    } else if warnings.len() == MAX_WARNINGS {
        warnings.push("Additional warnings omitted at reporting limit".to_owned());
    }
}

struct Directory {
    name: String,
    extents: Vec<Extent>,
    depth: usize,
}

#[derive(Clone)]
struct Claim {
    start: u64,
    end: u64,
    owner: u64,
}

fn add_claims(
    claims: &mut Vec<Claim>,
    extents: &[Extent],
    geometry: &Geometry,
    owner: u64,
) -> io::Result<()> {
    if claims.len() + extents.len() > MAX_CLAIMS {
        return Err(unsupported("Allocation ownership limit exceeded"));
    }
    claims.extend(extents.iter().map(|extent| Claim {
        start: extent.offset,
        end: extent.offset + extent.length.div_ceil(geometry.cluster_size) * geometry.cluster_size,
        owner,
    }));
    Ok(())
}

fn analyze_source(source: &Source) -> io::Result<ExfatScan> {
    let mut scan = ExfatScan {
        source: source.fingerprint.clone(),
        candidates: Vec::new(),
        warnings: Vec::new(),
    };
    for (base, length) in volumes(source)? {
        let (geometry, warnings) = load_boot(source, base, length)?;
        scan.warnings.extend(warnings);
        let mut parser = Parser {
            source,
            geometry,
            fat_start: u64::MAX,
            fat_cache: Vec::new(),
            chain_work: 0,
        };
        scan_volume(&mut parser, &mut scan)?;
    }
    Ok(scan)
}

fn scan_volume(parser: &mut Parser<'_>, scan: &mut ExfatScan) -> io::Result<()> {
    let root = parser.chain(parser.geometry.root, None, false, MAX_DIRECTORY_BYTES)?;
    let root_bytes = parser.read_stream(&root, MAX_DIRECTORY_BYTES)?;
    let mut bitmap_entry = None;
    let mut upcase_entry = None;
    let mut index = 0;
    while index + 32 <= root_bytes.len() {
        let entry = &root_bytes[index..index + 32];
        if entry[0] == 0 {
            break;
        }
        if entry[0] == 0x81
            && (entry[1] & 1 != 0
                || bitmap_entry
                    .replace((le32(entry, 20), le64(entry, 24)))
                    .is_some())
        {
            return Err(invalid(
                "Invalid or duplicate exFAT allocation bitmap entry",
            ));
        }
        if entry[0] == 0x82
            && upcase_entry
                .replace((le32(entry, 20), le64(entry, 24), le32(entry, 4)))
                .is_some()
        {
            return Err(invalid("Duplicate exFAT up-case table"));
        }
        index += if entry[0] == 0x85 || entry[0] == 5 {
            (usize::from(entry[1]) + 1) * 32
        } else {
            32
        };
    }
    let (start, length) = bitmap_entry.ok_or_else(|| invalid("Missing exFAT allocation bitmap"))?;
    if length < u64::from(parser.geometry.cluster_count).div_ceil(8) || length > MAX_BITMAP_BYTES {
        return Err(unsupported(
            "Allocation bitmap missing cluster bits or exceeds 64 MiB limit",
        ));
    }
    let bitmap_extents = parser.chain(start, Some(length), false, MAX_BITMAP_BYTES)?;
    let bitmap = parser.read_stream(&bitmap_extents, MAX_BITMAP_BYTES)?;
    let upcase = if let Some((start, length, checksum)) = upcase_entry {
        if !(2..=131_072).contains(&length) || length % 2 != 0 {
            return Err(invalid("Invalid exFAT up-case table length"));
        }
        let extents = parser.chain(start, Some(length), false, 131_072)?;
        let bytes = parser.read_stream(&extents, 131_072)?;
        let actual = bytes.iter().fold(0u32, |sum, &byte| {
            sum.rotate_right(1).wrapping_add(u32::from(byte))
        });
        if actual != checksum {
            return Err(invalid("Invalid exFAT up-case table checksum"));
        }
        extents
    } else {
        Vec::new()
    };
    let mut bitmap_work = 0;
    for extents in [&root, &bitmap_extents, &upcase] {
        if allocated_count(&parser.geometry, &bitmap, extents, &mut bitmap_work)?
            != cluster_count(&parser.geometry, extents)
        {
            return Err(invalid(
                "Root directory or allocation bitmap occupies unallocated clusters",
            ));
        }
    }
    let mut claims = Vec::new();
    add_claims(&mut claims, &root, &parser.geometry, METADATA_OWNER)?;
    add_claims(
        &mut claims,
        &bitmap_extents,
        &parser.geometry,
        METADATA_OWNER,
    )?;
    add_claims(&mut claims, &upcase, &parser.geometry, METADATA_OWNER)?;
    let mut directories = vec![Directory {
        name: String::new(),
        extents: root,
        depth: 0,
    }];
    let mut visited = HashSet::new();
    let mut total_bytes = 0;
    let mut total_entries = 0u64;
    let mut candidate_extents = 0usize;
    let first_candidate = scan.candidates.len();
    while let Some(directory) = directories.pop() {
        let first_offset = directory.extents[0].offset;
        if !visited.insert(first_offset) {
            warning(
                &mut scan.warnings,
                format!("Repeated/cyclic directory skipped: {}", directory.name),
            );
            continue;
        }
        if visited.len() > MAX_DIRECTORIES || directory.depth > MAX_DEPTH {
            return Err(unsupported(
                "exFAT directory traversal/depth limit exceeded",
            ));
        }
        let bytes = if directory.depth == 0 {
            root_bytes.clone()
        } else {
            parser.read_stream(&directory.extents, MAX_DIRECTORY_BYTES)?
        };
        total_bytes += bytes.len() as u64;
        if total_bytes > MAX_TOTAL_DIRECTORY_BYTES {
            return Err(unsupported("Total directory byte limit exceeded"));
        }
        let mut position = 0;
        while position + 32 <= bytes.len() {
            total_entries += 1;
            if total_entries > MAX_ENTRIES {
                return Err(unsupported("Directory entry limit exceeded"));
            }
            let kind = bytes[position];
            if kind == 0 {
                break;
            }
            if kind & 0xe0 == 0x80 && ![0x81, 0x82, 0x83, 0x85].contains(&kind) {
                return Err(unsupported(
                    "Unsupported critical primary exFAT directory entry",
                ));
            }
            if kind != 0x85 && kind != 5 {
                position += 32;
                continue;
            }
            let set_length = (usize::from(bytes[position + 1]) + 1) * 32;
            let id = physical_position(&directory.extents, position as u64)?;
            if position + set_length > bytes.len() {
                warning(
                    &mut scan.warnings,
                    format!("Truncated directory entry set at {id}"),
                );
                break;
            }
            let parsed = file_entry(&bytes[position..position + set_length]);
            total_entries += (set_length / 32 - 1) as u64;
            if total_entries > MAX_ENTRIES {
                return Err(unsupported("Directory entry limit exceeded"));
            }
            position += set_length;
            let entry = match parsed {
                Ok(entry) => entry,
                Err(error) => {
                    warning(
                        &mut scan.warnings,
                        format!("Directory entry {id} skipped: {error}"),
                    );
                    continue;
                }
            };
            let name = if directory.name.is_empty() {
                entry.name.clone()
            } else {
                format!("{}/{}", directory.name, entry.name)
            };
            let video = !entry.directory
                && entry.name.rsplit('.').next().is_some_and(|extension| {
                    extension.eq_ignore_ascii_case("mov") || extension.eq_ignore_ascii_case("mp4")
                });
            if entry.directory && entry.deleted {
                warning(
                    &mut scan.warnings,
                    format!("Deleted directory not traversed: {name}"),
                );
                continue;
            }
            if entry.deleted && !video {
                continue;
            }
            if entry.length == 0 && entry.first == 0 && !video {
                continue;
            }
            let maximum = if entry.directory {
                MAX_DIRECTORY_BYTES
            } else {
                parser.geometry.end - parser.geometry.heap
            };
            let mut extents = Vec::new();
            let chain_result = parser.chain_into(
                entry.first,
                Some(entry.length),
                entry.contiguous,
                maximum,
                &mut extents,
            );
            let mut reason = None;
            if entry.valid_length != entry.length {
                reason = Some(
                    "ValidDataLength differs from DataLength; initialized bytes cannot be inferred"
                        .to_owned(),
                );
            }
            if entry.directory && entry.length % parser.geometry.cluster_size != 0 {
                reason = Some("Directory length is not cluster aligned".to_owned());
            }
            if let Err(error) = chain_result {
                reason = Some(error.to_string());
                if !entry.deleted && extents.is_empty() {
                    // Invalid lengths can prevent traversal altogether. A valid
                    // FirstCluster is still evidence of this active owner's claim.
                    if let Ok(offset) = parser.cluster_offset(entry.first) {
                        add_claims(
                            &mut claims,
                            &[Extent {
                                offset,
                                length: parser.geometry.cluster_size,
                            }],
                            &parser.geometry,
                            if entry.directory { METADATA_OWNER } else { id },
                        )?;
                    }
                }
            }
            let allocated = allocated_count(&parser.geometry, &bitmap, &extents, &mut bitmap_work)?;
            let clusters = cluster_count(&parser.geometry, &extents);
            if reason.is_none() && entry.deleted && allocated > 0 {
                reason = Some("Deleted stream has allocated/reassigned clusters; original bytes are uncertain".to_owned());
            }
            if reason.is_none() && !entry.deleted && allocated != clusters {
                reason = Some(
                    "Active stream includes clusters missing from allocation bitmap".to_owned(),
                );
            }
            if !entry.deleted && !extents.is_empty() {
                add_claims(
                    &mut claims,
                    &extents,
                    &parser.geometry,
                    if entry.directory { METADATA_OWNER } else { id },
                )?;
            }
            if entry.directory {
                if let Some(reason) = reason {
                    warning(
                        &mut scan.warnings,
                        format!("Invalid directory {name}: {reason}"),
                    );
                } else {
                    if directories.len() >= MAX_DIRECTORIES {
                        return Err(unsupported("Directory queue limit exceeded"));
                    }
                    directories.push(Directory {
                        name,
                        extents,
                        depth: directory.depth + 1,
                    });
                }
                continue;
            }
            if !video {
                if let Some(reason) = reason {
                    warning(
                        &mut scan.warnings,
                        format!("Invalid active file {name}: {reason}"),
                    );
                }
                continue;
            }
            candidate_extents += extents.len();
            if candidate_extents > MAX_CLAIMS {
                return Err(unsupported("Total video extent limit exceeded"));
            }
            if reason.is_none() {
                if let Err(error) = container_header(parser.source, &extents, entry.length) {
                    reason = Some(error.to_string());
                }
            }
            if scan.candidates.len() >= MAX_CANDIDATES {
                return Err(unsupported("Video candidate limit exceeded"));
            }
            scan.candidates.push(ExfatCandidate {
                id,
                name,
                length: entry.length,
                extents,
                deleted: entry.deleted,
                recoverable: reason.is_none(),
                reason,
                allocated_clusters: allocated,
            });
        }
    }
    // Include non-video files and metadata in cross-link detection. Bitmap bits alone
    // cannot identify ownership if file-system metadata is internally inconsistent.
    claims.sort_unstable_by_key(|claim| claim.start);
    let mut maximum = 0;
    let prefix_ends: Vec<_> = claims
        .iter()
        .map(|claim| {
            maximum = maximum.max(claim.end);
            maximum
        })
        .collect();
    for candidate in &mut scan.candidates[first_candidate..] {
        for extent in &candidate.extents {
            let first = prefix_ends.partition_point(|&end| end <= extent.offset);
            let end = extent.offset + extent.length;
            if claims[first..]
                .iter()
                .take_while(|claim| claim.start < end)
                .any(|claim| claim.end > extent.offset && claim.owner != candidate.id)
            {
                candidate.recoverable = false;
                candidate.reason =
                    Some("Stream overlaps another active file or file-system metadata".to_owned());
                break;
            }
        }
    }
    if scan.candidates[first_candidate..]
        .iter()
        .any(|candidate| candidate.deleted)
    {
        warning(&mut scan.warnings, "Deleted entries are residual metadata; a free bitmap and plausible header do not prove that data was never overwritten".to_owned());
    }
    Ok(())
}

fn validate_extents(extents: &[Extent], length: u64, source_size: u64) -> io::Result<()> {
    if length == 0 || extents.is_empty() || extents.len() > MAX_EXTENTS {
        return Err(invalid("Invalid extraction length or extent count"));
    }
    let mut total = 0u64;
    let mut ranges = Vec::with_capacity(extents.len());
    for extent in extents {
        let end = extent
            .offset
            .checked_add(extent.length)
            .ok_or_else(|| invalid("Extent overflow"))?;
        if extent.length == 0 || end > source_size {
            return Err(invalid("Extraction extent outside image"));
        }
        total = total
            .checked_add(extent.length)
            .ok_or_else(|| invalid("Extraction length overflow"))?;
        ranges.push((extent.offset, end));
    }
    if total != length {
        return Err(invalid(
            "Extraction extent sum differs from declared length",
        ));
    }
    ranges.sort_unstable();
    if ranges.windows(2).any(|pair| pair[0].1 > pair[1].0) {
        return Err(invalid("Overlapping extraction extents"));
    }
    Ok(())
}

fn create_output(path: &Path, length: u64) -> io::Result<File> {
    let path = if path.is_absolute() {
        path.to_owned()
    } else {
        std::env::current_dir()?.join(path)
    };
    let name = path
        .file_name()
        .ok_or_else(|| invalid("Output filename missing"))?;
    let parent = path
        .parent()
        .ok_or_else(|| invalid("Output parent missing"))?;
    let mut directory = File::open("/")?;
    // Pin each directory descriptor: a symlink or concurrent path replacement must
    // not redirect the exclusive file creation outside the chosen session.
    for component in parent.components() {
        match component {
            Component::RootDir | Component::CurDir => continue,
            Component::Normal(name) => {
                let name =
                    CString::new(name.as_bytes()).map_err(|_| invalid("NUL in output path"))?;
                // SAFETY: directory and name remain alive; a successful fd is owned here.
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
                // SAFETY: openat returned a new, exclusively owned file descriptor.
                directory = unsafe { File::from_raw_fd(fd) };
            }
            _ => return Err(invalid("Parent traversal in output path is not allowed")),
        }
    }
    let mut stats = std::mem::MaybeUninit::<libc::statvfs>::uninit();
    // SAFETY: stats points to writable storage; fstatvfs initializes it on success.
    if unsafe { libc::fstatvfs(directory.as_raw_fd(), stats.as_mut_ptr()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: the preceding successful fstatvfs initialized every field.
    let stats = unsafe { stats.assume_init() };
    let free = u128::from(stats.f_bavail) * u128::from(stats.f_frsize);
    if free < u128::from(length) {
        return Err(io::Error::other(
            "Insufficient free space for recovered video",
        ));
    }
    let name = CString::new(name.as_bytes()).map_err(|_| invalid("NUL in output filename"))?;
    // SAFETY: valid directory fd/name; create_new semantics protect all existing paths.
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
    // SAFETY: openat returned a new, exclusively owned file descriptor.
    Ok(unsafe { File::from_raw_fd(fd) })
}
