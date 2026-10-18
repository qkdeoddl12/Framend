//! The desktop protocol keeps selections opaque so JavaScript cannot round u64 identities.
use crate::{
    Candidate, Event, Format, SourceFingerprint, exfat, formats,
    platform::GuardedDirectory,
    source::{Source, invalid},
};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::HashSet,
    ffi::OsStr,
    io::{self, Write},
    path::{Path, PathBuf},
    time::Duration,
};

mod video;

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
pub enum Request {
    Scan {
        source: PathBuf,
        method: ScanMethod,
    },
    Recover {
        source: PathBuf,
        output: PathBuf,
        selections: Vec<String>,
        ffmpeg: Option<PathBuf>,
        ffprobe: Option<PathBuf>,
    },
    Repair {
        source: PathBuf,
        reference: PathBuf,
        output: PathBuf,
        ffmpeg: Option<PathBuf>,
        ffprobe: Option<PathBuf>,
    },
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScanMethod {
    Carve,
    Filesystem,
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum Selection {
    Carve {
        source: SourceFingerprint,
        candidate: Candidate,
    },
    Filesystem {
        source: SourceFingerprint,
        candidate: exfat::ExfatCandidate,
    },
}

impl Selection {
    fn source(&self) -> &SourceFingerprint {
        match self {
            Self::Carve { source, .. } | Self::Filesystem { source, .. } => source,
        }
    }
    fn length(&self) -> u64 {
        match self {
            Self::Carve { candidate, .. } => candidate.length,
            Self::Filesystem { candidate, .. } => candidate.length,
        }
    }
    fn name(&self) -> String {
        match self {
            Self::Carve { candidate, .. } => format!(
                "offset_{}.{}",
                candidate.offset,
                extension(candidate.format)
            ),
            Self::Filesystem { candidate, .. } => candidate.name.clone(),
        }
    }
}

pub fn run(request: Request, mut emit: impl FnMut(Value) -> io::Result<()>) -> io::Result<()> {
    emit(json!({"event":"started"}))?;
    match request {
        Request::Scan { source, method } => scan_job(&source, method, &mut emit),
        Request::Recover {
            source,
            output,
            selections,
            ffmpeg,
            ffprobe,
        } => recover_job(&source, &output, selections, ffmpeg, ffprobe, &mut emit),
        Request::Repair {
            source,
            reference,
            output,
            ffmpeg,
            ffprobe,
        } => video::repair(source, reference, output, ffmpeg, ffprobe, &mut emit),
    }
}

fn extension(format: Format) -> &'static str {
    match format {
        Format::Jpeg => "jpg",
        Format::Raf => "raf",
        Format::Png => "png",
        Format::Wav => "wav",
    }
}

fn scan_job(
    path: &Path,
    method: ScanMethod,
    emit: &mut dyn FnMut(Value) -> io::Result<()>,
) -> io::Result<()> {
    let source = Source::open(path)?;
    match method {
        ScanMethod::Carve => crate::scan(path, |event| {
            match event {
                Event::Candidate { candidate } => {
                    let token = Selection::Carve {
                        source: source.fingerprint.clone(),
                        candidate: candidate.clone(),
                    };
                    emit(json!({"event":"candidate","entry": {
                        "id":format!("carve:{}",candidate.offset), "name":token.name(), "format":extension(candidate.format),
                        "bytes":candidate.length,"recoverable":true,"inspection":"structure_only",
                        "warnings":["Structural candidate; content decoding has not been verified."], "selection":serde_json::to_string(&token)?
                    }}))?;
                }
                Event::Progress { scanned, total } => emit(
                    json!({"event":"progress","phase":"scan","completed":scanned,"total":total}),
                )?,
                Event::Complete { count, .. } => {
                    source.unchanged()?;
                    emit(json!({"event":"complete","count":count,"failed":0,"directory":null}))?;
                }
                _ => {}
            }
            Ok(())
        }),
        ScanMethod::Filesystem => {
            emit(json!({"event":"progress","phase":"filesystem","completed":0,"total":null}))?;
            let scan = exfat::analyze_files(path)?;
            if scan.source != source.fingerprint {
                return Err(invalid("Source changed during filesystem scan"));
            }
            let count = scan.candidates.len();
            for candidate in scan.candidates {
                let token = Selection::Filesystem {
                    source: scan.source.clone(),
                    candidate: candidate.clone(),
                };
                let mut warnings = scan.warnings.clone();
                if let Some(reason) = candidate.reason {
                    warnings.push(reason);
                }
                if candidate.deleted {
                    warnings
                        .push("Deleted entry; extraction does not prove original content.".into());
                }
                emit(json!({"event":"candidate","entry":{
                    "id":format!("filesystem:{}",candidate.id),"name":candidate.name,"format":"file", "bytes":candidate.length,
                    "recoverable":candidate.recoverable,"inspection":"unverified","warnings":warnings,"selection":serde_json::to_string(&token)?
                }}))?;
            }
            source.unchanged()?;
            emit(json!({"event":"complete","count":count,"failed":0,"directory":null}))
        }
    }
}

fn validate_selections(
    source: &Source,
    path: &Path,
    tokens: Vec<String>,
) -> io::Result<Vec<Selection>> {
    if tokens.is_empty()
        || tokens.len() > 100_000
        || tokens.iter().map(String::len).sum::<usize>() > 32 * 1024 * 1024
    {
        return Err(invalid(
            "Select between 1 and 100,000 candidates within the request limit",
        ));
    }
    let selections: Vec<Selection> = tokens
        .iter()
        .map(|s| serde_json::from_str(s).map_err(io::Error::from))
        .collect::<io::Result<_>>()?;
    let filesystem = if selections
        .iter()
        .any(|s| matches!(s, Selection::Filesystem { .. }))
    {
        Some(exfat::analyze_files(path)?)
    } else {
        None
    };
    let mut ids = HashSet::new();
    let mut ranges = Vec::new();
    for selection in &selections {
        if selection.source() != &source.fingerprint {
            return Err(invalid("Source changed; scan again"));
        }
        match selection {
            Selection::Carve { candidate, .. } => {
                if !ids.insert((0, candidate.offset))
                    || formats::parse(source, candidate.offset, candidate.format)?.as_ref()
                        != Some(candidate)
                {
                    return Err(invalid("Duplicate or altered carving selection"));
                }
                ranges.push((candidate.offset, candidate.length));
            }
            Selection::Filesystem { candidate, .. } => {
                let found = filesystem
                    .as_ref()
                    .and_then(|s| s.candidates.iter().find(|c| c.id == candidate.id));
                if !ids.insert((1, candidate.id))
                    || !candidate.recoverable
                    || found != Some(candidate)
                {
                    return Err(invalid("Duplicate or altered filesystem selection"));
                }
                ranges.extend(candidate.extents.iter().map(|e| (e.offset, e.length)));
                if ranges.len() > 1_000_000 {
                    return Err(invalid("Too many selected extents"));
                }
            }
        }
    }
    ranges.sort_unstable();
    let mut end = 0;
    for (offset, length) in ranges {
        if offset < end {
            return Err(invalid("Selected candidates overlap"));
        }
        end = offset
            .checked_add(length)
            .filter(|end| *end <= source.fingerprint.size)
            .ok_or_else(|| invalid("Selection outside source"))?;
    }
    source.unchanged()?;
    Ok(selections)
}

fn recover_job(
    source_path: &Path,
    output: &Path,
    tokens: Vec<String>,
    ffmpeg: Option<PathBuf>,
    ffprobe: Option<PathBuf>,
    emit: &mut dyn FnMut(Value) -> io::Result<()>,
) -> io::Result<()> {
    let source = Source::open(source_path)?;
    let selections = validate_selections(&source, source_path, tokens)?;
    let ffmpeg = video::resolve_tool(ffmpeg, "ffmpeg")?;
    let ffprobe = video::resolve_tool(ffprobe, "ffprobe")?;
    let tools = [ffmpeg.as_deref(), ffprobe.as_deref()]
        .into_iter()
        .flatten()
        .map(Source::open)
        .collect::<io::Result<Vec<_>>>()?;
    let required = selections.iter().try_fold(64 * 1024 * 1024u64, |sum, s| {
        sum.checked_add(s.length())
            .ok_or_else(|| invalid("Output size overflow"))
    })?;
    let parent = GuardedDirectory::open(output)?;
    parent.check_space(required)?;
    let session = parent.create_session("Framend")?;
    session.write_new(OsStr::new("INCOMPLETE.txt"), b"Incomplete until all selected files and evidence have been written and checked. Failed decoding retains this marker.\n")?;
    let result = (|| {
        let mut versions = Vec::new();
        for (name, path) in [
            ("ffmpeg", ffmpeg.as_deref()),
            ("ffprobe", ffprobe.as_deref()),
        ] {
            if let Some(path) = path {
                let version = crate::process::run(
                    path,
                    &["-version".into()],
                    None,
                    128 * 1024,
                    Duration::from_secs(10),
                )?;
                if !version.status.success() {
                    return Err(invalid("Cannot identify media validation tool"));
                }
                versions.push(json!({"name":name,"path":path,"version":String::from_utf8_lossy(&version.stdout)}));
            }
        }
        for tool in &tools {
            tool.unchanged()?;
        }
        let mut results = Vec::new();
        let mut snapshots = Vec::new();
        write_evidence(
            &session,
            "tools.json",
            &serde_json::to_vec_pretty(&versions)?,
            &mut snapshots,
        )?;
        let mut failed = 0;
        for (index, selection) in selections.iter().enumerate() {
            source.unchanged()?;
            for tool in &tools {
                tool.unchanged()?;
            }
            let original_name = selection.name();
            let ext = match selection {
                Selection::Carve { candidate, .. } => extension(candidate.format).to_owned(),
                Selection::Filesystem { .. } => Path::new(&original_name)
                    .extension()
                    .and_then(OsStr::to_str)
                    .filter(|s| {
                        !s.is_empty()
                            && s.len() <= 12
                            && s.bytes().all(|b| b.is_ascii_alphanumeric())
                    })
                    .unwrap_or("bin")
                    .to_ascii_lowercase(),
            };
            let name = format!("file_{:06}.{ext}", index + 1);
            let path = session.path().join(&name);
            let hash = match selection {
                Selection::Carve { candidate, .. } => {
                    let mut file = session.create_new(OsStr::new(&name), candidate.length)?;
                    let mut hash = Sha256::new();
                    let mut buffer = vec![0; 1024 * 1024];
                    let mut copied = 0;
                    while copied < candidate.length {
                        let count = (candidate.length - copied).min(buffer.len() as u64) as usize;
                        source.read_at(candidate.offset + copied, &mut buffer[..count])?;
                        file.write_all(&buffer[..count])?;
                        hash.update(&buffer[..count]);
                        copied += count as u64;
                        emit(
                            json!({"event":"progress","phase":"copy","completed":copied,"total":candidate.length}),
                        )?;
                        session.verify_file(OsStr::new(&name), &file)?;
                    }
                    file.sync_all()?;
                    session.verify_file(OsStr::new(&name), &file)?;
                    format!("{:x}", hash.finalize())
                }
                Selection::Filesystem { candidate, .. } => exfat::extract_file(
                    source_path,
                    &path,
                    &source.fingerprint,
                    candidate,
                    |completed, total| {
                        session.verify()?;
                        emit(
                            json!({"event":"progress","phase":"copy","completed":completed,"total":total}),
                        )
                    },
                )?,
            };
            let snapshot = Source::open(&path)?;
            if snapshot.fingerprint.size != selection.length() || hash_source(&snapshot)? != hash {
                return Err(invalid("Extracted output changed"));
            }
            let format = sniff(&snapshot)?;
            let (inspection, artifacts) = match format {
                "jpeg" | "png" | "wav" | "raf" => {
                    let kind = match format {
                        "jpeg" => Format::Jpeg,
                        "png" => Format::Png,
                        "wav" => Format::Wav,
                        _ => Format::Raf,
                    };
                    if formats::parse(&snapshot, 0, kind)?
                        .is_none_or(|c| c.length != snapshot.fingerprint.size)
                    {
                        ("failed".to_owned(), Vec::new())
                    } else {
                        video::inspect_with_evidence(
                            &path,
                            format,
                            ffmpeg.as_deref(),
                            ffprobe.as_deref(),
                            &session,
                            &format!("file_{:06}", index + 1),
                        )?
                    }
                }
                "mov" => video::inspect_with_evidence(
                    &path,
                    format,
                    ffmpeg.as_deref(),
                    ffprobe.as_deref(),
                    &session,
                    &format!("file_{:06}", index + 1),
                )?,
                _ => ("unverified".to_owned(), Vec::new()),
            };
            snapshots.extend(artifacts);
            snapshot.unchanged()?;
            source.unchanged()?;
            for tool in &tools {
                tool.unchanged()?;
            }
            let error = (inspection == "failed")
                .then_some("Content validation failed; extracted bytes retained.");
            if error.is_some() {
                failed += 1;
            }
            let item = json!({"name":original_name,"path":path,"bytes":selection.length(),"sha256":hash,"inspection":inspection,"partial":false,"error":error});
            write_evidence(
                &session,
                &format!("file_{:06}.evidence.json", index + 1),
                &serde_json::to_vec_pretty(
                    &json!({"schema":1,"selection":selection,"result":item}),
                )?,
                &mut snapshots,
            )?;
            emit(json!({"event":"saved","result":item}))?;
            snapshot.unchanged()?;
            snapshots.push((path, snapshot.fingerprint.clone()));
            results.push(item);
        }
        source.unchanged()?;
        for tool in &tools {
            tool.unchanged()?;
        }
        verify_snapshots(&snapshots)?;
        write_evidence(
            &session,
            "report.json",
            &serde_json::to_vec_pretty(
                &json!({"schema":1,"operation":"recover","source":source.fingerprint,"source_path":source_path,"results":results,"failed":failed}),
            )?,
            &mut snapshots,
        )?;
        if failed == 0 {
            session.remove_file(OsStr::new("INCOMPLETE.txt"))?;
        }
        emit(
            json!({"event":"complete","directory":session.path(),"count":results.len(),"failed":failed}),
        )?;
        source.unchanged()?;
        verify_snapshots(&snapshots)?;
        session.verify()?;
        Ok(())
    })();
    if let Err(error) = &result {
        let _ = session.write_new(OsStr::new("INCOMPLETE.txt"), b"The job did not complete successfully. Consult error.txt and the per-file evidence.\n");
        let _ = session.write_new(OsStr::new("error.txt"), error.to_string().as_bytes());
    }
    result
}

fn verify_snapshots(snapshots: &[(PathBuf, SourceFingerprint)]) -> io::Result<()> {
    for (path, fingerprint) in snapshots {
        if Source::open(path)?.fingerprint != *fingerprint {
            return Err(invalid("A recovered file changed before completion"));
        }
    }
    Ok(())
}

fn write_evidence(
    session: &GuardedDirectory,
    name: &str,
    bytes: &[u8],
    snapshots: &mut Vec<(PathBuf, SourceFingerprint)>,
) -> io::Result<()> {
    session.write_new(OsStr::new(name), bytes)?;
    let path = session.path().join(name);
    let source = Source::open(&path)?;
    if source.fingerprint.size != bytes.len() as u64
        || hash_source(&source)? != format!("{:x}", Sha256::digest(bytes))
    {
        return Err(invalid("Evidence changed while being written"));
    }
    snapshots.push((path, source.fingerprint));
    Ok(())
}

pub(super) fn hash_source(source: &Source) -> io::Result<String> {
    let mut hasher = Sha256::new();
    let mut buffer = vec![0; 1024 * 1024];
    let mut offset = 0;
    while offset < source.fingerprint.size {
        let count = (source.fingerprint.size - offset).min(buffer.len() as u64) as usize;
        source.read_at(offset, &mut buffer[..count])?;
        hasher.update(&buffer[..count]);
        offset += count as u64;
    }
    source.unchanged()?;
    Ok(format!("{:x}", hasher.finalize()))
}

fn sniff(source: &Source) -> io::Result<&'static str> {
    let mut bytes = [0; 16];
    let count = source.fingerprint.size.min(bytes.len() as u64) as usize;
    source.read_at(0, &mut bytes[..count])?;
    Ok(if bytes.starts_with(&[0xff, 0xd8, 0xff]) {
        "jpeg"
    } else if bytes.starts_with(formats::PNG_MAGIC) {
        "png"
    } else if &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WAVE" {
        "wav"
    } else if bytes.starts_with(formats::RAF_MAGIC) {
        "raf"
    } else if matches!(&bytes[4..8], b"ftyp" | b"moov" | b"mdat" | b"wide") {
        "mov"
    } else {
        "unknown"
    })
}

#[derive(Serialize)]
pub struct Preview {
    pub mime: String,
    pub base64: String,
}

pub fn preview(path: &Path, selection: &str) -> io::Result<Preview> {
    if selection.len() > 32 * 1024 * 1024 {
        return Err(invalid("Selection exceeds preview limit"));
    }
    let source = Source::open(path)?;
    let selections = validate_selections(&source, path, vec![selection.to_owned()])?;
    let Selection::Carve { candidate, .. } = &selections[0] else {
        return Err(invalid(
            "Preview currently requires a carved JPEG, PNG or RAF candidate",
        ));
    };
    let (offset, length, kind) = match candidate.format {
        Format::Jpeg | Format::Png => (candidate.offset, candidate.length, candidate.format),
        Format::Raf => {
            let mut header = [0; 8];
            source.read_at(candidate.offset + 84, &mut header)?;
            let start = u32::from_be_bytes(header[..4].try_into().unwrap()) as u64;
            let length = u32::from_be_bytes(header[4..].try_into().unwrap()) as u64;
            (candidate.offset + start, length, Format::Jpeg)
        }
        Format::Wav => return Err(invalid("Audio preview is unavailable")),
    };
    if length > 32 * 1024 * 1024 {
        return Err(invalid("Preview is larger than 32 MiB"));
    }
    let parsed = formats::parse(&source, offset, kind)?
        .ok_or_else(|| invalid("Invalid preview structure"))?;
    if parsed.length != length
        || u64::from(parsed.width.unwrap_or(0)) * u64::from(parsed.height.unwrap_or(0)) > 80_000_000
    {
        return Err(invalid("Preview exceeds image limits"));
    }
    let mut bytes = vec![0; length as usize];
    source.read_at(offset, &mut bytes)?;
    source.unchanged()?;
    Ok(Preview {
        mime: if kind == Format::Png {
            "image/png"
        } else {
            "image/jpeg"
        }
        .into(),
        base64: STANDARD.encode(bytes),
    })
}
