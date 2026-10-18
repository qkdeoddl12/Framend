use crate::{
    Candidate, Event, Format, RecoveryRequest,
    formats::parse,
    platform::GuardedDirectory,
    source::{Source, invalid},
};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::{
    ffi::OsStr,
    io::{self, Read, Write},
    path::Path,
};

#[derive(Serialize)]
struct Saved {
    candidate: Candidate,
    file: String,
    sha256: String,
    validation: &'static str,
}

pub fn recover(
    path: &Path,
    output: &Path,
    request: &RecoveryRequest,
    mut emit: impl FnMut(Event) -> io::Result<()>,
) -> io::Result<()> {
    let source = Source::open(path)?;
    if source.fingerprint != request.source {
        return Err(invalid(
            "원본 이미지가 변경되었습니다. 이미지를 다시 스캔하세요.",
        ));
    }
    if request.candidates.is_empty() || request.candidates.len() > 100_000 {
        return Err(invalid("복구할 파일을 1~100,000개 선택하세요."));
    }
    if !output.is_dir() {
        return Err(invalid("저장 위치는 기존 폴더여야 합니다."));
    }
    let mut ranges: Vec<_> = request.candidates.iter().collect();
    ranges.sort_by_key(|c| c.offset);
    let mut previous_end = 0;
    for candidate in &ranges {
        if candidate.offset < previous_end {
            return Err(invalid("겹치거나 중복된 복구 요청입니다."));
        }
        if parse(&source, candidate.offset, candidate.format)?.as_ref() != Some(*candidate) {
            return Err(invalid(
                "파일 구조가 스캔 결과와 다릅니다. 다시 스캔하세요.",
            ));
        }
        previous_end = candidate
            .offset
            .checked_add(candidate.length)
            .ok_or_else(|| invalid("잘못된 파일 길이입니다."))?;
    }
    source.unchanged()?;
    let directory = GuardedDirectory::new_session(output, "Recovery")?;
    let marker = OsStr::new("INCOMPLETE.txt");
    directory.write_new(
        marker,
        b"This recovery did not finish. Files may be partial or unverified.\n",
    )?;
    let mut buffer = vec![0; 1024 * 1024];
    let mut saved = Vec::new();
    let total: u64 = ranges.iter().map(|c| c.length).sum();
    let mut completed = 0;
    for candidate in ranges {
        let extension = match candidate.format {
            Format::Jpeg => "jpg",
            Format::Raf => "raf",
            Format::Png => "png",
            Format::Wav => "wav",
        };
        let name = format!("{:012x}.{extension}", candidate.offset);
        let target = directory.path().join(&name);
        let partial = format!("{name}.partial");
        let mut file = directory.create_new(OsStr::new(&partial), candidate.length)?;
        let mut reader = source.region(candidate.offset, candidate.length);
        let mut remaining = candidate.length;
        let mut hash = Sha256::new();
        while remaining > 0 {
            let size = remaining.min(buffer.len() as u64) as usize;
            reader.read_exact(&mut buffer[..size])?;
            file.write_all(&buffer[..size])?;
            hash.update(&buffer[..size]);
            remaining -= size as u64;
        }
        file.sync_all()?;
        source.unchanged()?;
        directory.verify_file(OsStr::new(&partial), &file)?;
        drop(file);
        directory.publish(OsStr::new(&partial), OsStr::new(&name))?;
        let sha256 = format!("{:x}", hash.finalize());
        saved.push(Saved {
            candidate: candidate.clone(),
            file: name,
            sha256: sha256.clone(),
            validation: "structure_only",
        });
        emit(Event::Saved {
            candidate: candidate.clone(),
            path: target.to_string_lossy().into_owned(),
            sha256,
        })?;
        completed += candidate.length;
        emit(Event::Progress {
            scanned: completed,
            total,
        })?;
    }
    source.unchanged()?;
    let manifest = serde_json::to_vec_pretty(
        &serde_json::json!({"schema":1,"source":request.source,"files":saved,"status":"extracted_not_decoded"}),
    )?;
    directory.write_new(OsStr::new("manifest.json"), &manifest)?;
    source.unchanged()?;
    directory.remove_file(marker)?;
    emit(Event::Complete {
        count: saved.len(),
        directory: Some(directory.path().to_string_lossy().into_owned()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn publishing_never_replaces_existing_files_or_links() {
        let folder = tempfile::tempdir_in(std::env::temp_dir().canonicalize().unwrap()).unwrap();
        let directory = GuardedDirectory::open(folder.path()).unwrap();
        let partial = OsStr::new("photo.partial");
        let target = OsStr::new("photo.jpg");
        directory.write_new(partial, b"recovered").unwrap();
        directory.write_new(target, b"existing").unwrap();
        assert_eq!(
            directory.publish(partial, target).unwrap_err().kind(),
            io::ErrorKind::AlreadyExists
        );
        assert_eq!(fs::read(folder.path().join(target)).unwrap(), b"existing");
        assert_eq!(fs::read(folder.path().join(partial)).unwrap(), b"recovered");
        fs::rename(
            folder.path().join(target),
            folder.path().join("existing.jpg"),
        )
        .unwrap();
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(
                folder.path().join("existing.jpg"),
                folder.path().join(target),
            )
            .unwrap();
            assert_eq!(
                directory.publish(partial, target).unwrap_err().kind(),
                io::ErrorKind::AlreadyExists
            );
            assert_eq!(
                fs::read(folder.path().join("existing.jpg")).unwrap(),
                b"existing"
            );
            fs::remove_file(folder.path().join(target)).unwrap();
        }
        directory.publish(partial, target).unwrap();
        assert!(!folder.path().join(partial).exists());
        assert_eq!(fs::read(folder.path().join(target)).unwrap(), b"recovered");
    }
}
