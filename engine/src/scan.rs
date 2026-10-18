use crate::{
    Event, Format,
    formats::{PNG_MAGIC, RAF_MAGIC, parse},
    source::{Source, invalid},
};
use std::{
    io,
    path::Path,
    time::{Duration, Instant},
};

const CHUNK: usize = 4 * 1024 * 1024;
const MAX_CANDIDATES: usize = 100_000;

pub fn scan(path: &Path, mut emit: impl FnMut(Event) -> io::Result<()>) -> io::Result<()> {
    let source = Source::open(path)?;
    let total = source.fingerprint.size;
    emit(Event::Started {
        source: source.fingerprint.clone(),
    })?;
    let mut buffer = vec![0; CHUNK + 15];
    let mut scanned = 0u64;
    let mut skip_until = 0u64;
    let mut count = 0;
    let mut last_progress = Instant::now();
    while scanned < total {
        let count_bytes = (total - scanned).min(buffer.len() as u64) as usize;
        source.read_at(scanned, &mut buffer[..count_bytes])?;
        let search_end = CHUNK.min(count_bytes);
        let mut i = skip_until.saturating_sub(scanned).min(search_end as u64) as usize;
        while i < search_end {
            let tail = &buffer[i..count_bytes];
            let format = if tail.starts_with(RAF_MAGIC) {
                Some(Format::Raf)
            } else if tail.starts_with(PNG_MAGIC) {
                Some(Format::Png)
            } else if tail.len() >= 12 && &tail[..4] == b"RIFF" && &tail[8..12] == b"WAVE" {
                Some(Format::Wav)
            } else if tail.starts_with(&[0xff, 0xd8, 0xff]) {
                Some(Format::Jpeg)
            } else {
                None
            };
            if let Some(format) = format {
                if let Some(candidate) = parse(&source, scanned + i as u64, format)? {
                    skip_until = candidate.offset + candidate.length;
                    count += 1;
                    if count > MAX_CANDIDATES {
                        return Err(invalid("후보 수가 안전 제한 100,000개를 넘었습니다."));
                    }
                    emit(Event::Candidate { candidate })?;
                    i = (skip_until - scanned).min(search_end as u64) as usize;
                    continue;
                }
            }
            i += 1;
        }
        scanned = (scanned + search_end as u64).max(skip_until).min(total);
        if last_progress.elapsed() >= Duration::from_millis(100) || scanned == total {
            emit(Event::Progress { scanned, total })?;
            last_progress = Instant::now();
        }
    }
    source.unchanged()?;
    emit(Event::Complete {
        count,
        directory: None,
    })
}
