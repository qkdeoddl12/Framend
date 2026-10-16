use crate::{
    Candidate, Event, Format, RecoveryRequest,
    formats::parse,
    source::{Source, invalid},
};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::{
    fs::{self, OpenOptions},
    io::{self, Read, Write},
    os::unix::{ffi::OsStrExt, fs::DirBuilderExt},
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
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
    let directory = new_directory(output)?;
    let marker = directory.join("INCOMPLETE.txt");
    write_new(
        &marker,
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
        };
        let name = format!("{:012x}.{extension}", candidate.offset);
        let target = directory.join(&name);
        let partial = directory.join(format!("{name}.partial"));
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&partial)?;
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
        publish(&partial, &target)?;
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
    write_new(&directory.join("manifest.json"), &manifest)?;
    fs::remove_file(marker)?;
    emit(Event::Complete {
        count: saved.len(),
        directory: Some(directory.to_string_lossy().into_owned()),
    })
}

fn publish(partial: &Path, target: &Path) -> io::Result<()> {
    let from = std::ffi::CString::new(partial.as_os_str().as_bytes())?;
    let to = std::ffi::CString::new(target.as_os_str().as_bytes())?;
    // Prefer atomic publication on file systems that support exclusive rename.
    // SAFETY: both C strings remain alive during the call; the OS owns no pointers.
    let result = unsafe { libc::renamex_np(from.as_ptr(), to.as_ptr(), libc::RENAME_EXCL) };
    if result == 0 {
        Ok(())
    } else {
        let error = io::Error::last_os_error();
        if error.raw_os_error() == Some(libc::ENOTSUP) {
            publish_copy(partial, target)
        } else {
            Err(error)
        }
    }
}

fn publish_copy(partial: &Path, target: &Path) -> io::Result<()> {
    // macOS exFAT lacks exclusive rename. create_new still prevents replacement.
    // INCOMPLETE.txt remains until every file and the manifest have been saved.
    let mut source = fs::File::open(partial)?;
    let mut output = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(target)?;
    io::copy(&mut source, &mut output)?;
    output.sync_all()?;
    fs::remove_file(partial)
}

fn write_new(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut file = OpenOptions::new().write(true).create_new(true).open(path)?;
    file.write_all(bytes)?;
    file.sync_all()
}

fn new_directory(parent: &Path) -> io::Result<PathBuf> {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(io::Error::other)?
        .as_nanos();
    for attempt in 0..100 {
        let path = parent.join(format!("Recovery-{stamp}-{attempt}"));
        match fs::DirBuilder::new().mode(0o700).create(&path) {
            Ok(()) => return Ok(path),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "고유한 저장 폴더를 만들 수 없습니다.",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn publishing_never_replaces_existing_files_or_links() {
        check_publication(publish);
    }

    #[test]
    fn copy_fallback_never_replaces_existing_files_or_links() {
        check_publication(publish_copy);
    }

    fn check_publication(publish: fn(&Path, &Path) -> io::Result<()>) {
        let folder = tempfile::tempdir().unwrap();
        let partial = folder.path().join("photo.partial");
        let target = folder.path().join("photo.jpg");
        let existing = folder.path().join("existing.jpg");
        fs::write(&partial, b"recovered").unwrap();
        fs::write(&target, b"existing").unwrap();
        assert_eq!(
            publish(&partial, &target).unwrap_err().kind(),
            io::ErrorKind::AlreadyExists
        );
        assert_eq!(fs::read(&target).unwrap(), b"existing");
        assert_eq!(fs::read(&partial).unwrap(), b"recovered");
        fs::rename(&target, &existing).unwrap();
        std::os::unix::fs::symlink(&existing, &target).unwrap();
        assert_eq!(
            publish(&partial, &target).unwrap_err().kind(),
            io::ErrorKind::AlreadyExists
        );
        assert_eq!(fs::read(&existing).unwrap(), b"existing");
        fs::remove_file(&target).unwrap();
        publish(&partial, &target).unwrap();
        assert!(!partial.exists());
        assert_eq!(fs::read(&target).unwrap(), b"recovered");
    }
}
