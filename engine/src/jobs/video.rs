use crate::{
    Format, formats, native_video,
    platform::GuardedDirectory,
    process,
    source::{Source, invalid},
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    ffi::{OsStr, OsString},
    fs,
    io::{self, BufRead, BufReader, Read},
    path::{Path, PathBuf},
    time::Duration,
};

const HASH_LIMIT: u64 = 128 * 1024 * 1024;
type Artifacts = Vec<(PathBuf, crate::SourceFingerprint)>;

pub(super) fn resolve_tool(explicit: Option<PathBuf>, name: &str) -> io::Result<Option<PathBuf>> {
    fn executable(path: &Path) -> bool {
        let Ok(metadata) = fs::metadata(path) else {
            return false;
        };
        if !metadata.is_file() {
            return false;
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            metadata.permissions().mode() & 0o111 != 0
        }
        #[cfg(windows)]
        {
            true
        }
    }
    if let Some(path) = explicit {
        if !executable(&path) {
            return Err(invalid("Explicit media tool is missing or not executable"));
        }
        return Ok(Some(fs::canonicalize(path)?));
    }
    let mut directories: Vec<PathBuf> = std::env::var_os("PATH")
        .map(|path| std::env::split_paths(&path).collect())
        .unwrap_or_default();
    directories.extend(["/opt/homebrew/bin", "/usr/local/bin", "/usr/bin"].map(PathBuf::from));
    let filename = if cfg!(windows) {
        format!("{name}.exe")
    } else {
        name.to_owned()
    };
    for directory in directories {
        let path = directory.join(&filename);
        if executable(&path) {
            return Ok(Some(fs::canonicalize(path)?));
        }
    }
    Ok(None)
}

fn args(values: &[&str]) -> Vec<OsString> {
    values.iter().map(OsString::from).collect()
}

fn input_options(format: &str) -> Vec<OsString> {
    let mut options = args(&[
        "-max_alloc",
        "268435456",
        "-protocol_whitelist",
        "file,pipe",
    ]);
    if matches!(format, "mov" | "mp4") {
        options.extend(args(&[
            "-enable_drefs",
            "0",
            "-use_absolute_path",
            "0",
            "-f",
            "mov",
        ]));
    } else {
        options.extend(args(&[
            "-f",
            match format {
                "jpeg" => "jpeg_pipe",
                "png" => "png_pipe",
                "wav" => "wav",
                _ => "mov",
            },
        ]));
    }
    options
}

fn hash(source: &Source) -> io::Result<String> {
    source.unchanged()?;
    let mut digest = Sha256::new();
    let mut stream = source.region(0, source.fingerprint.size);
    let mut buffer = [0; 64 * 1024];
    loop {
        let count = stream.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        digest.update(&buffer[..count]);
    }
    source.unchanged()?;
    Ok(format!("{:x}", digest.finalize()))
}

fn framehash(bytes: impl Read) -> io::Result<Value> {
    let mut reader = BufReader::new(bytes);
    let mut line = Vec::new();
    let mut timebase = None;
    let mut frames = 0u64;
    let mut start: Option<f64> = None;
    let mut end: Option<f64> = None;
    let mut duration = 0.0;
    loop {
        line.clear();
        let count = reader.by_ref().take(65537).read_until(b'\n', &mut line)?;
        if count == 0 {
            break;
        }
        if count > 65536 {
            return Err(invalid("Frame hash line exceeds limit"));
        }
        let text = std::str::from_utf8(&line)
            .map_err(|_| invalid("Invalid frame hash text"))?
            .trim();
        if let Some(value) = text.strip_prefix("#tb 0:") {
            let (numerator, denominator) = value
                .trim()
                .split_once('/')
                .ok_or_else(|| invalid("Missing frame time base"))?;
            let numerator = numerator
                .parse::<u32>()
                .map_err(|_| invalid("Invalid frame time base"))?;
            let denominator = denominator
                .parse::<u32>()
                .map_err(|_| invalid("Invalid frame time base"))?;
            if numerator == 0 || denominator == 0 {
                return Err(invalid("Invalid frame time base"));
            }
            timebase = Some(f64::from(numerator) / f64::from(denominator));
        } else if !text.is_empty() && !text.starts_with('#') {
            let fields: Vec<_> = text.split(',').map(str::trim).collect();
            if fields.len() != 6
                || fields[0] != "0"
                || fields[5].len() != 64
                || !fields[5].bytes().all(|b| b.is_ascii_hexdigit())
            {
                return Err(invalid("Invalid frame hash row"));
            }
            let base = timebase.ok_or_else(|| invalid("Missing frame time base"))?;
            let pts = fields[2]
                .parse::<i64>()
                .map_err(|_| invalid("Invalid frame timestamp"))? as f64
                * base;
            let length = fields[3]
                .parse::<u64>()
                .map_err(|_| invalid("Invalid frame duration"))? as f64
                * base;
            fields[1]
                .parse::<i64>()
                .map_err(|_| invalid("Invalid decode timestamp"))?;
            if fields[4].parse::<u64>().is_err()
                || !(pts + length).is_finite()
                || (pts + length).abs() > i64::MAX as f64
                || !(duration + length).is_finite()
                || duration + length > i64::MAX as f64
            {
                return Err(invalid("Frame timing exceeds limit"));
            }
            frames = frames
                .checked_add(1)
                .ok_or_else(|| invalid("Frame count overflow"))?;
            duration += length;
            start = Some(start.map_or(pts, |value| value.min(pts)));
            end = Some(end.map_or(pts + length, |value| value.max(pts + length)));
        }
    }
    Ok(json!({"frames":frames,"duration":duration,"start":start,"end":end}))
}

fn evidence(
    directory: Option<(&GuardedDirectory, &str)>,
    suffix: &str,
    bytes: &[u8],
    artifacts: &mut Artifacts,
) -> io::Result<()> {
    if let Some((directory, prefix)) = directory {
        let name = format!("{prefix}-{suffix}");
        directory.write_new(OsStr::new(&name), bytes)?;
        let path = directory.path().join(name);
        let file = Source::open(&path)?;
        artifacts.push((path, file.fingerprint));
    }
    Ok(())
}

fn validate(
    path: &Path,
    format: &str,
    ffmpeg: &Path,
    ffprobe: &Path,
    directory: Option<(&GuardedDirectory, &str)>,
) -> io::Result<(Value, Artifacts)> {
    let source = Source::open(path)?;
    let mut artifacts = Vec::new();
    let mut probe_args = args(&["-v", "error"]);
    probe_args.extend(input_options(format));
    probe_args.extend(args(&["-show_streams", "-of", "json"]));
    probe_args.push(path.as_os_str().to_owned());
    let probe = process::run(
        ffprobe,
        &probe_args,
        None,
        2 * 1024 * 1024,
        Duration::from_secs(60),
    )?;
    source.unchanged()?;
    evidence(directory, "probe.log", &probe.stderr, &mut artifacts)?;
    evidence(directory, "probe.json", &probe.stdout, &mut artifacts)?;
    let failed = |reason: &str| json!({"status":"failed","tracks":[],"diagnostics":reason});
    if !probe.status.success() {
        return Ok((failed("Media probe failed"), artifacts));
    }
    let data: Value =
        serde_json::from_slice(&probe.stdout).map_err(|_| invalid("Invalid probe metadata"))?;
    let Some(streams) = data["streams"]
        .as_array()
        .filter(|streams| !streams.is_empty() && streams.len() <= 16)
    else {
        return Ok((failed("Missing tracks or track limit exceeded"), artifacts));
    };
    let mut tracks = Vec::new();
    let mut indices = std::collections::HashSet::new();
    let mut all_passed = true;
    for stream in streams {
        let kind = stream["codec_type"].as_str().unwrap_or("");
        if !matches!(kind, "video" | "audio") {
            continue;
        }
        let Some(index) = stream["index"]
            .as_u64()
            .filter(|index| *index <= 1024 && indices.insert(*index))
        else {
            return Ok((failed("Invalid track index"), artifacts));
        };
        if kind == "video" {
            let width = stream["width"].as_u64().unwrap_or(0);
            let height = stream["height"].as_u64().unwrap_or(0);
            if width == 0
                || height == 0
                || width > 16384
                || height > 16384
                || width * height > 80_000_000
            {
                return Ok((
                    failed("Video dimensions exceed validation limits"),
                    artifacts,
                ));
            }
        } else {
            let channels = stream["channels"].as_u64().unwrap_or(0);
            let rate = stream["sample_rate"]
                .as_str()
                .and_then(|rate| rate.parse::<u32>().ok())
                .unwrap_or(0);
            if channels == 0 || channels > 64 || rate == 0 || rate > 384000 {
                return Ok((
                    failed("Audio parameters exceed validation limits"),
                    artifacts,
                ));
            }
        }
        let mut decode_args = args(&[
            "-nostdin",
            "-v",
            "error",
            "-xerror",
            "-err_detect",
            "explode",
            "-threads",
            "2",
        ]);
        decode_args.extend(input_options(format));
        decode_args.push("-i".into());
        decode_args.push(path.as_os_str().to_owned());
        decode_args.extend(args(&[
            "-map",
            &format!("0:{index}"),
            "-threads",
            "2",
            "-fps_mode",
            "passthrough",
            "-f",
            "framehash",
            "-hash",
            "sha256",
            "pipe:1",
        ]));
        let hashes_name = directory.map(|(_, prefix)| format!("{prefix}-track-{index}.framehash"));
        let hashes_file =
            if let (Some((directory, _)), Some(name)) = (directory, hashes_name.as_ref()) {
                Some(directory.create_new(OsStr::new(name), 1024 * 1024)?)
            } else {
                None
            };
        let destination = hashes_file.as_ref().map(fs::File::try_clone).transpose()?;
        super::verify_snapshots(&artifacts)?;
        let decoded = process::run(
            ffmpeg,
            &decode_args,
            destination,
            HASH_LIMIT,
            Duration::from_secs(3600),
        )?;
        source.unchanged()?;
        super::verify_snapshots(&artifacts)?;
        evidence(
            directory,
            &format!("track-{index}.log"),
            &decoded.stderr,
            &mut artifacts,
        )?;
        let measurement = if let (Some((directory, _)), Some(name), Some(file)) =
            (directory, hashes_name, hashes_file)
        {
            directory.verify_file(OsStr::new(&name), &file)?;
            drop(file);
            let hashes = Source::open(&directory.path().join(&name))?;
            if hash(&hashes)? != decoded.stdout_sha256 {
                return Err(invalid("Frame hash evidence changed during decoding"));
            }
            let measured = framehash(hashes.region(0, hashes.fingerprint.size));
            hashes.unchanged()?;
            artifacts.push((directory.path().join(&name), hashes.fingerprint));
            measured
        } else {
            framehash(decoded.stdout.as_slice())
        };
        let measurement = match measurement {
            Ok(value) => Some(value),
            Err(error)
                if !decoded.status.success() && error.kind() == io::ErrorKind::InvalidData =>
            {
                None
            }
            Err(error) => return Err(error),
        };
        let passed = decoded.status.success()
            && decoded.stderr.is_empty()
            && measurement
                .as_ref()
                .is_some_and(|value| value["frames"].as_u64().is_some_and(|frames| frames > 0));
        let samples = if kind == "audio" {
            let rate = stream["sample_rate"]
                .as_str()
                .and_then(|rate| rate.parse::<f64>().ok())
                .unwrap_or(0.0);
            let samples = measurement
                .as_ref()
                .and_then(|value| value["duration"].as_f64())
                .unwrap_or(0.0)
                * rate;
            if !samples.is_finite() || samples > i64::MAX as f64 {
                return Err(invalid("Decoded sample count exceeds limit"));
            }
            samples.round() as u64
        } else {
            0
        };
        all_passed &= passed;
        tracks.push(json!({"index":index,"type":kind,"codec":stream["codec_name"],"status":if passed {"decoded"} else {"failed"},"decoded_samples":samples,"declared_duration":stream["duration"],"decoded":measurement}));
    }
    if tracks.is_empty()
        || (format == "wav" && !tracks.iter().any(|track| track["type"] == "audio"))
        || (matches!(format, "jpeg" | "png")
            && !tracks.iter().any(|track| track["type"] == "video"))
    {
        all_passed = false;
    }
    source.unchanged()?;
    super::verify_snapshots(&artifacts)?;
    Ok((
        json!({"status":if all_passed {"decoded"} else {"failed"},"tracks":tracks}),
        artifacts,
    ))
}

#[cfg(test)]
fn inspect(
    path: &Path,
    format: &str,
    ffmpeg: Option<&Path>,
    ffprobe: Option<&Path>,
) -> io::Result<String> {
    inspect_inner(path, format, ffmpeg, ffprobe, None).map(|(status, _)| status)
}

pub(super) fn inspect_with_evidence(
    path: &Path,
    format: &str,
    ffmpeg: Option<&Path>,
    ffprobe: Option<&Path>,
    directory: &GuardedDirectory,
    prefix: &str,
) -> io::Result<(String, Artifacts)> {
    inspect_inner(path, format, ffmpeg, ffprobe, Some((directory, prefix)))
}

fn inspect_inner(
    path: &Path,
    format: &str,
    ffmpeg: Option<&Path>,
    ffprobe: Option<&Path>,
    directory: Option<(&GuardedDirectory, &str)>,
) -> io::Result<(String, Artifacts)> {
    let source = Source::open(path)?;
    let structural = match format {
        "jpeg" => Some(Format::Jpeg),
        "png" => Some(Format::Png),
        "wav" => Some(Format::Wav),
        "mov" | "mp4" => None,
        _ => return Ok(("unverified".into(), Vec::new())),
    };
    if let Some(format) = structural {
        if formats::parse(&source, 0, format)?
            .is_none_or(|candidate| candidate.length != source.fingerprint.size)
        {
            source.unchanged()?;
            return Ok(("failed".into(), Vec::new()));
        }
    }
    source.unchanged()?;
    let (Some(ffmpeg), Some(ffprobe)) = (ffmpeg, ffprobe) else {
        return Ok((
            if structural.is_some() {
                "structure_only"
            } else {
                "unverified"
            }
            .into(),
            Vec::new(),
        ));
    };
    let (result, mut artifacts) = validate(path, format, ffmpeg, ffprobe, directory)?;
    evidence(
        directory,
        "validation.json",
        &serde_json::to_vec_pretty(&result)?,
        &mut artifacts,
    )?;
    source.unchanged()?;
    super::verify_snapshots(&artifacts)?;
    Ok((
        result["status"].as_str().unwrap_or("failed").to_owned(),
        artifacts,
    ))
}

fn validate_segments(
    report: &native_video::SalvageReport,
    output_size: u64,
    input_size: u64,
) -> io::Result<()> {
    if report.segment_map.is_empty()
        || report.segment_map.len() > 256
        || report.segments != report.segment_map.len() as u64
        || !report.partial
        || report.frames == 0
        || !matches!(report.codec.as_str(), "h264" | "hevc")
        || report.fps_num == 0
        || report.fps_den == 0
        || report.frame_count_basis != "structural_access_unit_starts"
        || report.timing_basis != "estimated_reference_average"
    {
        return Err(invalid("Invalid native salvage report"));
    }
    let (mut output, mut extents, mut frames) = (0u64, 0usize, 0u64);
    for (index, segment) in report.segment_map.iter().enumerate() {
        if segment.id != index + 1
            || segment.output_offset != output
            || segment.output_length == 0
            || segment.output_length > output_size.saturating_sub(output)
            || segment.extent_start != extents
            || segment.extent_count == 0
            || segment.extent_count > report.extents.len().saturating_sub(extents)
            || segment.frames == 0
            || segment.frames > report.frames.saturating_sub(frames)
            || segment.sha256.len() != 64
            || !segment
                .sha256
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            || !matches!(
                segment.end_reason.as_str(),
                "unknown_bytes" | "picture_mismatch" | "range_end"
            )
        {
            return Err(invalid("Invalid salvage segment"));
        }
        output += segment.output_length;
        extents += segment.extent_count;
        frames += segment.frames;
    }
    if output != output_size || extents != report.extents.len() || frames != report.frames {
        return Err(invalid("Salvage segment totals disagree"));
    }
    let (mut end, mut retained) = (0u64, 0u64);
    for extent in &report.extents {
        if extent.length == 0
            || extent.offset < end
            || extent.offset > input_size
            || extent.length > input_size - extent.offset
        {
            return Err(invalid("Invalid retained source extent"));
        }
        end = extent.offset + extent.length;
        retained += extent.length;
    }
    if report.skipped_bytes != input_size - retained {
        return Err(invalid("Invalid skipped-byte evidence"));
    }
    Ok(())
}

fn remember_evidence(
    directory: &GuardedDirectory,
    snapshots: &mut Vec<(PathBuf, crate::SourceFingerprint)>,
    name: &str,
) -> io::Result<()> {
    super::verify_snapshots(snapshots)?;
    directory.verify()?;
    if snapshots.len() >= 16384 {
        return Err(invalid("Repair evidence file limit exceeded"));
    }
    let path = directory.path().join(name);
    let source = Source::open(&path)?;
    snapshots.push((path, source.fingerprint));
    directory.verify()
}

pub(super) fn repair(
    source_path: PathBuf,
    reference_path: PathBuf,
    output: PathBuf,
    ffmpeg: Option<PathBuf>,
    ffprobe: Option<PathBuf>,
    emit: &mut dyn FnMut(Value) -> io::Result<()>,
) -> io::Result<()> {
    let source = Source::open(&source_path)?;
    let reference = Source::open(&reference_path)?;
    if source.fingerprint.device == reference.fingerprint.device
        && source.fingerprint.inode == reference.fingerprint.inode
    {
        return Err(invalid("Source and reference must be different files"));
    }
    let ffmpeg = resolve_tool(ffmpeg, "ffmpeg")?
        .ok_or_else(|| invalid("ffmpeg is required for video repair"))?;
    let ffprobe = resolve_tool(ffprobe, "ffprobe")?
        .ok_or_else(|| invalid("ffprobe is required for video repair"))?;
    let ffmpeg_source = Source::open(&ffmpeg)?;
    let ffprobe_source = Source::open(&ffprobe)?;
    let required = source
        .fingerprint
        .size
        .checked_mul(3)
        .and_then(|size| size.checked_add(1024 * 1024 * 1024 + 64 * 1024 * 1024))
        .ok_or_else(|| invalid("Repair output size overflow"))?;
    let parent = GuardedDirectory::open(&output)?;
    parent.check_space(required)?;
    let session = parent.create_session("Framend-repair")?;
    session.write_new(OsStr::new("INCOMPLETE.txt"), b"Partial salvage. This marker remains until every segment and its evidence passes validation.\n")?;
    let result = (|| {
        let version = process::run(
            &ffmpeg,
            &args(&["-version"]),
            None,
            128 * 1024,
            Duration::from_secs(10),
        )?;
        let probe_version = process::run(
            &ffprobe,
            &args(&["-version"]),
            None,
            128 * 1024,
            Duration::from_secs(10),
        )?;
        if !version.status.success() || !probe_version.status.success() {
            return Err(invalid("Cannot identify media tools"));
        }
        session.write_new(OsStr::new("tools.json"), &serde_json::to_vec_pretty(&json!({"ffmpeg_path":ffmpeg,"ffprobe_path":ffprobe,"ffmpeg_sha256":hash(&ffmpeg_source)?,"ffprobe_sha256":hash(&ffprobe_source)?,"ffmpeg_version":String::from_utf8_lossy(&version.stdout),"ffprobe_version":String::from_utf8_lossy(&probe_version.stdout)}))?)?;
        let mut evidence_snapshots = Vec::new();
        remember_evidence(&session, &mut evidence_snapshots, "tools.json")?;
        emit(
            json!({"event":"progress","phase":"reference_validation","completed":0,"total":null}),
        )?;
        let (reference_validation, reference_artifacts) = validate(
            &reference_path,
            "mov",
            &ffmpeg,
            &ffprobe,
            Some((&session, "reference")),
        )?;
        evidence_snapshots.extend(reference_artifacts);
        session.write_new(
            OsStr::new("reference-validation.json"),
            &serde_json::to_vec_pretty(&reference_validation)?,
        )?;
        if reference_validation["status"] != "decoded"
            || !reference_validation["tracks"]
                .as_array()
                .is_some_and(|tracks| tracks.iter().any(|track| track["type"] == "video"))
        {
            return Err(invalid("Reference failed complete track decoding"));
        }
        remember_evidence(
            &session,
            &mut evidence_snapshots,
            "reference-validation.json",
        )?;
        source.unchanged()?;
        reference.unchanged()?;
        session.verify()?;
        let aggregate_path = session.path().join("salvaged.es");
        let report = native_video::salvage(
            &source_path,
            &reference_path,
            &aggregate_path,
            |completed, total| {
                emit(
                    json!({"event":"progress","phase":"salvage","completed":completed,"total":total}),
                )
            },
        )?;
        let aggregate = Source::open(&aggregate_path)?;
        validate_segments(&report, aggregate.fingerprint.size, source.fingerprint.size)?;
        if report.input_sha256 != hash(&source)? || report.reference_sha256 != hash(&reference)? {
            return Err(invalid("Salvage input hash disagreement"));
        }
        session.write_new(
            OsStr::new("native-report.json"),
            &serde_json::to_vec_pretty(&report)?,
        )?;
        remember_evidence(&session, &mut evidence_snapshots, "native-report.json")?;
        let mut results = Vec::new();
        let mut snapshots = Vec::new();
        let mut failed = 0;
        for segment in &report.segment_map {
            session.verify()?;
            super::verify_snapshots(&snapshots)?;
            super::verify_snapshots(&evidence_snapshots)?;
            source.unchanged()?;
            reference.unchanged()?;
            aggregate.unchanged()?;
            ffmpeg_source.unchanged()?;
            ffprobe_source.unchanged()?;
            let prefix = format!("segment-{:04}", segment.id);
            let elementary_name = format!("{prefix}.es");
            let elementary_path = session.path().join(&elementary_name);
            let mut elementary_file =
                session.create_new(OsStr::new(&elementary_name), segment.output_length)?;
            let mut stream = aggregate.region(segment.output_offset, segment.output_length);
            if io::copy(&mut stream, &mut elementary_file)? != segment.output_length {
                return Err(invalid("Incomplete segment copy"));
            }
            elementary_file.sync_all()?;
            session.verify_file(OsStr::new(&elementary_name), &elementary_file)?;
            drop(elementary_file);
            let elementary = Source::open(&elementary_path)?;
            if hash(&elementary)? != segment.sha256 {
                return Err(invalid("Segment hash disagreement"));
            }
            let movie_name = format!("{prefix}.mp4");
            let movie_path = session.path().join(&movie_name);
            let movie_file = session.create_new(OsStr::new(&movie_name), segment.output_length)?;
            let mut mux_args = args(&[
                "-nostdin",
                "-v",
                "error",
                "-max_alloc",
                "268435456",
                "-protocol_whitelist",
                "file,pipe",
                "-fflags",
                "+genpts",
                "-r",
                &format!("{}/{}", report.fps_num, report.fps_den),
                "-f",
                &report.codec,
                "-i",
            ]);
            mux_args.push(elementary_path.as_os_str().to_owned());
            mux_args.extend(args(&[
                "-map",
                "0:v:0",
                "-c:v",
                "copy",
                "-avoid_negative_ts",
                "make_zero",
                "-movflags",
                "+frag_keyframe+empty_moov+default_base_moof",
                "-f",
                "mp4",
                "pipe:1",
            ]));
            emit(
                json!({"event":"progress","phase":"mux","completed":segment.id - 1,"total":report.segments}),
            )?;
            let mux_limit = segment
                .output_length
                .checked_mul(4)
                .and_then(|n| n.checked_add(64 * 1024 * 1024))
                .ok_or_else(|| invalid("Mux size overflow"))?;
            session.check_space(mux_limit)?;
            let mux = process::run(
                &ffmpeg,
                &mux_args,
                Some(movie_file.try_clone()?),
                mux_limit,
                Duration::from_secs(3600),
            )?;
            session.verify_file(OsStr::new(&movie_name), &movie_file)?;
            drop(movie_file);
            elementary.unchanged()?;
            aggregate.unchanged()?;
            session.verify()?;
            session.write_new(OsStr::new(&format!("{prefix}-mux.log")), &mux.stderr)?;
            remember_evidence(
                &session,
                &mut evidence_snapshots,
                &format!("{prefix}-mux.log"),
            )?;
            let movie = Source::open(&movie_path)?;
            if hash(&movie)? != mux.stdout_sha256 {
                return Err(invalid("Mux output changed during writing"));
            }
            let (validation, artifacts) = if mux.status.success() && mux.stderr.is_empty() {
                validate(
                    &movie_path,
                    "mp4",
                    &ffmpeg,
                    &ffprobe,
                    Some((&session, &prefix)),
                )?
            } else {
                (
                    json!({"status":"failed","tracks":[],"diagnostics":"Segment mux failed"}),
                    Vec::new(),
                )
            };
            evidence_snapshots.extend(artifacts);
            let passed = validation["status"] == "decoded";
            if !passed {
                failed += 1;
            }
            let movie_hash = hash(&movie)?;
            let record = json!({"name":movie_name,"path":movie_path,"bytes":movie.fingerprint.size,"sha256":movie_hash,"inspection":validation["status"],"validation":validation,"partial":true,"segment":segment,"elementary_sha256":segment.sha256,"error":if passed {None} else {Some("Segment failed complete decoding; elementary bytes retained.")},"warnings":["Partial video-only salvage; audio and original timestamps were not recovered.","Timing is estimated from the reference average frame rate; physical scan order does not establish recording chronology."]});
            session.write_new(
                OsStr::new(&format!("{prefix}-result.json")),
                &serde_json::to_vec_pretty(&record)?,
            )?;
            remember_evidence(
                &session,
                &mut evidence_snapshots,
                &format!("{prefix}-result.json"),
            )?;
            emit(json!({"event":"saved","result":record}))?;
            source.unchanged()?;
            reference.unchanged()?;
            aggregate.unchanged()?;
            elementary.unchanged()?;
            movie.unchanged()?;
            super::verify_snapshots(&evidence_snapshots)?;
            results.push(record);
            snapshots.push((elementary_path, elementary.fingerprint));
            snapshots.push((movie_path, movie.fingerprint));
        }
        source.unchanged()?;
        reference.unchanged()?;
        aggregate.unchanged()?;
        ffmpeg_source.unchanged()?;
        ffprobe_source.unchanged()?;
        super::verify_snapshots(&snapshots)?;
        session.verify()?;
        session.write_new(OsStr::new("results.json"), &serde_json::to_vec_pretty(&json!({"schema":1,"operation":"repair","partial":true,"source":source.fingerprint,"reference":reference.fingerprint,"source_path":source_path,"reference_path":reference_path,"source_sha256":report.input_sha256,"reference_sha256":report.reference_sha256,"results":results,"failed":failed}))?)?;
        remember_evidence(&session, &mut evidence_snapshots, "results.json")?;
        if failed == 0 {
            session.remove_file(OsStr::new("INCOMPLETE.txt"))?;
        }
        emit(
            json!({"event":"complete","count":results.len(),"failed":failed,"directory":session.path()}),
        )?;
        source.unchanged()?;
        reference.unchanged()?;
        aggregate.unchanged()?;
        ffmpeg_source.unchanged()?;
        ffprobe_source.unchanged()?;
        super::verify_snapshots(&snapshots)?;
        super::verify_snapshots(&evidence_snapshots)?;
        session.verify()?;
        Ok(())
    })();
    if let Err(error) = &result {
        let _ = session.write_new(
            OsStr::new("INCOMPLETE.txt"),
            b"Repair did not complete validation and delivery.\n",
        );
        let _ = session.write_new(OsStr::new("error.txt"), error.to_string().as_bytes());
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn framehash_requires_real_frames_and_valid_timing() {
        let hash = "a".repeat(64);
        let valid = format!("#tb 0: 1/30\n0, 0, 0, 1, 128, {hash}\n");
        assert_eq!(framehash(valid.as_bytes()).unwrap()["frames"], 1);
        assert_eq!(framehash(&b"#tb 0: 1/30\n"[..]).unwrap()["frames"], 0);
        assert!(framehash(format!("#tb 0: 0/30\n0, 0, 0, 1, 128, {hash}\n").as_bytes()).is_err());
        assert!(framehash(format!("#tb 0: 1/30\n0, 0, NaN, 1, 128, {hash}\n").as_bytes()).is_err());
        assert!(framehash(&b"#tb 0: 1/30\n0, 0, 0, 1, 128, bad\n"[..]).is_err());
    }

    #[test]
    fn explicit_missing_tool_is_an_error() {
        assert!(resolve_tool(Some(PathBuf::from("/definitely/missing/ffmpeg")), "ffmpeg").is_err());
    }

    #[test]
    fn inspection_distinguishes_structural_and_unknown_content() {
        use base64::Engine;
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("pixel.png");
        let png = base64::engine::general_purpose::STANDARD.decode("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAAAAAA6fptVAAAACklEQVR4nGOoBwAAgQCA05RTSgAAAABJRU5ErkJggg==").unwrap();
        fs::write(&path, &png).unwrap();
        assert_eq!(inspect(&path, "png", None, None).unwrap(), "structure_only");
        assert_eq!(inspect(&path, "raf", None, None).unwrap(), "unverified");
        assert_eq!(inspect(&path, "mov", None, None).unwrap(), "unverified");
        if let (Some(ffmpeg), Some(ffprobe)) = (
            resolve_tool(None, "ffmpeg").unwrap(),
            resolve_tool(None, "ffprobe").unwrap(),
        ) {
            assert_eq!(
                inspect(&path, "png", Some(&ffmpeg), Some(&ffprobe)).unwrap(),
                "decoded"
            );
            let wave = temp.path().join("audio.wav");
            let mut arguments = args(&[
                "-nostdin",
                "-v",
                "error",
                "-f",
                "lavfi",
                "-i",
                "sine=frequency=1000:sample_rate=8000",
                "-t",
                "0.02",
                "-c:a",
                "pcm_s16le",
            ]);
            arguments.push(wave.as_os_str().to_owned());
            assert!(
                process::run(
                    &ffmpeg,
                    &arguments,
                    None,
                    1024 * 1024,
                    Duration::from_secs(10)
                )
                .unwrap()
                .status
                .success()
            );
            assert_eq!(
                inspect(&wave, "wav", Some(&ffmpeg), Some(&ffprobe)).unwrap(),
                "decoded"
            );
        }
        fs::write(&path, b"broken").unwrap();
        assert_eq!(inspect(&path, "png", None, None).unwrap(), "failed");
    }

    fn real_movie(temp: &Path, ffmpeg: &Path) -> (PathBuf, PathBuf) {
        let reference = temp.join("reference.mov");
        let mut arguments = args(&[
            "-nostdin",
            "-v",
            "error",
            "-f",
            "lavfi",
            "-i",
            "testsrc=size=32x32:rate=30",
            "-t",
            "0.4",
            "-c:v",
            "libx264",
            "-pix_fmt",
            "yuv420p",
            "-bf",
            "0",
            "-g",
            "3",
            "-an",
        ]);
        arguments.push(reference.as_os_str().to_owned());
        let output = process::run(
            ffmpeg,
            &arguments,
            None,
            1024 * 1024,
            Duration::from_secs(30),
        )
        .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let source = temp.join("damaged.mov");
        let mut bytes = fs::read(&reference).unwrap();
        let mut cursor = 0;
        let mut mdat = None;
        while cursor + 8 <= bytes.len() {
            let length = u32::from_be_bytes(bytes[cursor..cursor + 4].try_into().unwrap()) as usize;
            assert!(length >= 8 && cursor + length <= bytes.len());
            if &bytes[cursor + 4..cursor + 8] == b"mdat" {
                mdat = Some(bytes[cursor..cursor + length].to_vec());
            }
            cursor += length;
        }
        bytes.extend(mdat.unwrap());
        fs::write(&source, bytes).unwrap();
        (source, reference)
    }

    #[test]
    fn audio_only_mp4_decodes_for_normal_recovery() {
        let Some(ffmpeg) = resolve_tool(None, "ffmpeg").unwrap() else {
            return;
        };
        let Some(ffprobe) = resolve_tool(None, "ffprobe").unwrap() else {
            return;
        };
        let temp = tempfile::tempdir().unwrap();
        let audio = temp.path().join("audio.m4a");
        let mut arguments = args(&[
            "-nostdin",
            "-v",
            "error",
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=1000:sample_rate=8000",
            "-t",
            "0.1",
            "-c:a",
            "aac",
        ]);
        arguments.push(audio.as_os_str().to_owned());
        let output = process::run(
            &ffmpeg,
            &arguments,
            None,
            1024 * 1024,
            Duration::from_secs(10),
        )
        .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            inspect(&audio, "mov", Some(&ffmpeg), Some(&ffprobe)).unwrap(),
            "decoded"
        );
        let reference = validate(&audio, "mov", &ffmpeg, &ffprobe, None).unwrap().0;
        assert!(
            reference["tracks"]
                .as_array()
                .unwrap()
                .iter()
                .all(|track| track["type"] == "audio")
        );
    }

    #[test]
    fn real_tools_repair_keeps_partial_evidence_and_decodes_every_segment() {
        let Some(ffmpeg) = resolve_tool(None, "ffmpeg").unwrap() else {
            return;
        };
        let Some(ffprobe) = resolve_tool(None, "ffprobe").unwrap() else {
            return;
        };
        let temp = tempfile::tempdir().unwrap();
        let output = temp.path().canonicalize().unwrap();
        let (source, reference) = real_movie(&output, &ffmpeg);
        let mut events = Vec::new();
        repair(
            source,
            reference,
            output,
            Some(ffmpeg),
            Some(ffprobe),
            &mut |event| {
                events.push(event);
                Ok(())
            },
        )
        .unwrap();
        let complete = events.last().unwrap();
        assert_eq!(complete["event"], "complete");
        assert_eq!(complete["failed"], 0);
        let directory = Path::new(complete["directory"].as_str().unwrap());
        assert!(!directory.join("INCOMPLETE.txt").exists());
        assert!(directory.join("native-report.json").exists());
        let saved: Vec<_> = events
            .iter()
            .filter(|event| event["event"] == "saved")
            .collect();
        assert_eq!(saved.len(), 2);
        for event in saved {
            let result = &event["result"];
            assert_eq!(result["partial"], true);
            assert_eq!(result["inspection"], "decoded");
            let file = Source::open(Path::new(result["path"].as_str().unwrap())).unwrap();
            assert_eq!(hash(&file).unwrap(), result["sha256"]);
            assert!(
                result["validation"]["tracks"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .all(|track| track["decoded"]["frames"].as_u64().unwrap() > 0)
            );
        }
    }

    #[cfg(unix)]
    fn script(temp: &Path, name: &str, contents: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let path = temp.join(name);
        fs::write(&path, format!("#!/bin/sh\n{contents}\n")).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
        path
    }

    #[cfg(unix)]
    #[test]
    fn failed_track_decode_is_failed_and_input_replacement_aborts() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("movie.mov");
        fs::write(&path, b"fixture").unwrap();
        let probe = script(
            temp.path(),
            "probe",
            "printf '%s' '{\"streams\":[{\"index\":0,\"codec_type\":\"video\",\"codec_name\":\"h264\",\"width\":32,\"height\":32}]}'",
        );
        let decoder = script(temp.path(), "decoder", "exit 7");
        assert_eq!(
            validate(&path, "mov", &decoder, &probe, None).unwrap().0["status"],
            "failed"
        );
        let malformed_probe = script(temp.path(), "malformed-probe", "printf '{'");
        assert!(validate(&path, "mov", &decoder, &malformed_probe, None).is_err());
        let mutating = script(
            temp.path(),
            "mutating",
            "for value in \"$@\"; do previous=$current; current=$value; if [ \"$previous\" = '-i' ]; then rm \"$current\"; printf replacement > \"$current\"; fi; done\nexit 7",
        );
        assert!(validate(&path, "mov", &mutating, &probe, None).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn changing_prior_probe_evidence_during_decode_aborts() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let path = root.join("movie.mov");
        fs::write(&path, b"fixture").unwrap();
        let directory = GuardedDirectory::new_session(&root, "Evidence").unwrap();
        let probe = script(
            &root,
            "probe",
            "printf '%s' '{\"streams\":[{\"index\":0,\"codec_type\":\"video\",\"codec_name\":\"h264\",\"width\":32,\"height\":32}]}'",
        );
        let evidence = directory.path().join("file-probe.json");
        let quoted = format!("'{}'", evidence.to_str().unwrap().replace('\'', "'\\''"));
        let decoder = script(
            &root,
            "decoder",
            &format!("printf changed > {quoted}\nexit 7"),
        );
        assert!(
            inspect_with_evidence(
                &path,
                "mov",
                Some(&decoder),
                Some(&probe),
                &directory,
                "file"
            )
            .is_err()
        );
    }

    #[test]
    fn failed_completion_delivery_restores_incomplete_marker() {
        let Some(ffmpeg) = resolve_tool(None, "ffmpeg").unwrap() else {
            return;
        };
        let Some(ffprobe) = resolve_tool(None, "ffprobe").unwrap() else {
            return;
        };
        let temp = tempfile::tempdir().unwrap();
        let output = temp.path().canonicalize().unwrap();
        let (source, reference) = real_movie(&output, &ffmpeg);
        let mut directory = None;
        let error = repair(
            source,
            reference,
            output,
            Some(ffmpeg),
            Some(ffprobe),
            &mut |event| {
                if event["event"] == "complete" {
                    directory = event["directory"].as_str().map(PathBuf::from);
                    return Err(io::Error::new(io::ErrorKind::BrokenPipe, "consumer closed"));
                }
                Ok(())
            },
        )
        .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::BrokenPipe);
        assert!(directory.unwrap().join("INCOMPLETE.txt").exists());
    }

    #[test]
    fn completion_callback_tampering_aborts_and_restores_incomplete_marker() {
        let Some(ffmpeg) = resolve_tool(None, "ffmpeg").unwrap() else {
            return;
        };
        let Some(ffprobe) = resolve_tool(None, "ffprobe").unwrap() else {
            return;
        };
        let temp = tempfile::tempdir().unwrap();
        let output = temp.path().canonicalize().unwrap();
        let (source, reference) = real_movie(&output, &ffmpeg);
        for target in ["results.json", "segment-0001.mp4"] {
            let mut directory = None;
            let result = repair(
                source.clone(),
                reference.clone(),
                output.clone(),
                Some(ffmpeg.clone()),
                Some(ffprobe.clone()),
                &mut |event| {
                    if event["event"] == "complete" {
                        let session = PathBuf::from(event["directory"].as_str().unwrap());
                        fs::write(session.join(target), b"callback changed output").unwrap();
                        directory = Some(session);
                    }
                    Ok(())
                },
            );
            assert!(
                result.is_err(),
                "completion callback changed {target} without aborting"
            );
            assert!(
                directory.unwrap().join("INCOMPLETE.txt").exists(),
                "missing incomplete marker after changing {target}"
            );
        }
    }

    #[test]
    fn filesystem_sidecars_and_unrelated_files_are_not_repair_evidence() {
        let Some(ffmpeg) = resolve_tool(None, "ffmpeg").unwrap() else {
            return;
        };
        let Some(ffprobe) = resolve_tool(None, "ffprobe").unwrap() else {
            return;
        };
        let temp = tempfile::tempdir().unwrap();
        let output = temp.path().canonicalize().unwrap();
        let (source, reference) = real_movie(&output, &ffmpeg);
        let mut completed = None;
        repair(
            source,
            reference,
            output.clone(),
            Some(ffmpeg),
            Some(ffprobe),
            &mut |event| {
                if event["event"] == "progress" && event["phase"] == "reference_validation" {
                    let session = fs::read_dir(&output)
                        .unwrap()
                        .map(|entry| entry.unwrap().path())
                        .find(|path| {
                            path.file_name()
                                .unwrap()
                                .to_string_lossy()
                                .starts_with("Framend-repair-")
                        })
                        .unwrap();
                    fs::write(session.join("._INCOMPLETE.txt"), b"filesystem sidecar").unwrap();
                    fs::write(session.join("unrelated.note"), b"not created by repair").unwrap();
                }
                if event["event"] == "complete" {
                    let session = PathBuf::from(event["directory"].as_str().unwrap());
                    fs::remove_file(session.join("._INCOMPLETE.txt")).unwrap();
                    fs::remove_file(session.join("unrelated.note")).unwrap();
                    completed = Some(session);
                }
                Ok(())
            },
        )
        .unwrap();
        assert!(!completed.unwrap().join("INCOMPLETE.txt").exists());
    }

    #[cfg(unix)]
    #[test]
    fn failed_segment_is_isolated_and_output_replacement_aborts() {
        let Some(ffmpeg) = resolve_tool(None, "ffmpeg").unwrap() else {
            return;
        };
        let Some(ffprobe) = resolve_tool(None, "ffprobe").unwrap() else {
            return;
        };
        let temp = tempfile::tempdir().unwrap();
        let output = temp.path().canonicalize().unwrap();
        let (source, reference) = real_movie(&output, &ffmpeg);
        let quoted_tool = format!("'{}'", ffmpeg.to_str().unwrap().replace('\'', "'\\''"));
        let failing = script(
            &output,
            "selective-ffmpeg",
            &format!("case \"$*\" in *segment-0001.es*) exit 9;; esac\nexec {quoted_tool} \"$@\""),
        );
        let mut events = Vec::new();
        repair(
            source.clone(),
            reference.clone(),
            output.clone(),
            Some(failing),
            Some(ffprobe.clone()),
            &mut |event| {
                events.push(event);
                Ok(())
            },
        )
        .unwrap();
        let complete = events.last().unwrap();
        assert_eq!(complete["count"], 2);
        assert_eq!(complete["failed"], 1);
        assert!(
            Path::new(complete["directory"].as_str().unwrap())
                .join("INCOMPLETE.txt")
                .exists()
        );
        let saved: Vec<_> = events
            .iter()
            .filter(|event| event["event"] == "saved")
            .collect();
        assert_eq!(saved[0]["result"]["inspection"], "failed");
        assert_eq!(saved[1]["result"]["inspection"], "decoded");
        let replacing = script(
            &output,
            "replacing-ffmpeg",
            &format!(
                "for value in \"$@\"; do case \"$value\" in *.es) movie=\"${{value%.es}}.mp4\"; rm \"$movie\"; printf replacement > \"$movie\";; esac; done\nexec {quoted_tool} \"$@\""
            ),
        );
        let mut replaced_events = Vec::new();
        assert!(
            repair(
                source,
                reference,
                output,
                Some(replacing),
                Some(ffprobe),
                &mut |event| {
                    replaced_events.push(event);
                    Ok(())
                }
            )
            .is_err()
        );
        assert!(
            !replaced_events
                .iter()
                .any(|event| event["event"] == "complete")
        );
    }
}
