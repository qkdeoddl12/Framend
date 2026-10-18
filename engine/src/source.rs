use crate::{
    SourceFingerprint,
    platform::{self, GuardedDirectory},
};
use std::{
    fs::File,
    io::{self, Read},
    path::{Path, PathBuf},
};

pub(crate) fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

pub(crate) struct Source {
    pub file: File,
    pub fingerprint: SourceFingerprint,
    path: PathBuf,
    directory: GuardedDirectory,
}

impl Source {
    pub fn open(path: &Path) -> io::Result<Self> {
        let name = path
            .file_name()
            .ok_or_else(|| invalid("Source filename missing"))?;
        let parent = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        // Resolve conventional aliases such as macOS /var once, then pin the
        // physical directories. The original path is checked again on completion.
        let directory = GuardedDirectory::open(&parent.canonicalize()?)?;
        let file = directory.open_existing(name)?;
        let metadata = file.metadata()?;
        if !metadata.is_file() {
            return Err(invalid(
                "일반 디스크 이미지 파일만 열 수 있습니다. 장치와 폴더는 지원하지 않습니다.",
            ));
        }
        let fingerprint = platform::fingerprint(&file)?;
        Ok(Self {
            file,
            fingerprint,
            path: path.to_owned(),
            directory,
        })
    }

    pub fn unchanged(&self) -> io::Result<()> {
        self.directory.verify()?;
        if platform::fingerprint(&self.file)? != self.fingerprint
            || Self::open(&self.path)?.fingerprint != self.fingerprint
        {
            return Err(invalid(
                "원본 이미지가 변경되었습니다. 이미지를 다시 스캔하세요.",
            ));
        }
        Ok(())
    }

    pub fn read_at(&self, offset: u64, bytes: &mut [u8]) -> io::Result<()> {
        if offset
            .checked_add(bytes.len() as u64)
            .is_none_or(|end| end > self.fingerprint.size)
        {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "이미지 범위를 벗어난 읽기입니다.",
            ));
        }
        platform::read_exact_at(&self.file, bytes, offset)
    }

    pub fn region(&self, offset: u64, length: u64) -> Region<'_> {
        Region {
            source: self,
            position: offset,
            remaining: length,
        }
    }
}

pub(crate) struct Region<'a> {
    source: &'a Source,
    position: u64,
    remaining: u64,
}
impl Read for Region<'_> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        let count = buffer.len().min(self.remaining as usize);
        if count == 0 {
            return Ok(0);
        }
        self.source.read_at(self.position, &mut buffer[..count])?;
        self.position += count as u64;
        self.remaining -= count as u64;
        Ok(count)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tempdir() -> tempfile::TempDir {
        tempfile::tempdir_in(std::env::temp_dir().canonicalize().unwrap()).unwrap()
    }

    #[test]
    fn source_reads_are_bounded_and_regular_files_only() {
        let dir = tempdir();
        let path = dir.path().join("source.img");
        std::fs::write(&path, b"abc").unwrap();
        let source = Source::open(&path).unwrap();
        let mut bytes = [0; 2];
        source.read_at(1, &mut bytes).unwrap();
        assert_eq!(&bytes, b"bc");
        assert_eq!(
            source.read_at(2, &mut bytes).unwrap_err().kind(),
            io::ErrorKind::UnexpectedEof
        );
        assert!(Source::open(dir.path()).is_err());
    }

    #[test]
    fn source_detects_content_changes() {
        let dir = tempdir();
        let path = dir.path().join("source.img");
        std::fs::write(&path, b"abc").unwrap();
        let source = Source::open(&path).unwrap();
        std::fs::write(&path, b"changed").unwrap();
        assert!(source.unchanged().is_err());
    }

    #[test]
    fn source_region_cannot_read_past_the_scanned_length() {
        let dir = tempdir();
        let path = dir.path().join("source.img");
        std::fs::write(&path, b"abc").unwrap();
        let source = Source::open(&path).unwrap();
        let mut bytes = [0; 3];
        assert_eq!(
            source.region(2, 3).read(&mut bytes).unwrap_err().kind(),
            io::ErrorKind::UnexpectedEof
        );
    }

    #[cfg(unix)]
    #[test]
    fn source_refuses_fifo_without_blocking() {
        use std::os::unix::ffi::OsStrExt;
        let dir = tempdir();
        let path = dir.path().join("fifo");
        let name = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
        // SAFETY: a live NUL-terminated pathname in an isolated test directory.
        assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
        assert!(Source::open(&path).is_err());
    }

    #[test]
    fn source_detects_same_length_replacement() {
        let dir = tempdir();
        let path = dir.path().join("source.img");
        std::fs::write(&path, b"abc").unwrap();
        let source = Source::open(&path).unwrap();
        std::fs::rename(&path, dir.path().join("old.img")).unwrap();
        std::fs::write(&path, b"xyz").unwrap();
        assert!(source.unchanged().is_err());
    }

    #[cfg(unix)]
    #[test]
    fn source_detects_replaced_linked_ancestor() {
        let dir = tempdir();
        let real = dir.path().join("real");
        std::fs::create_dir(&real).unwrap();
        std::fs::write(real.join("source.img"), b"abc").unwrap();
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        let source = Source::open(&link.join("source.img")).unwrap();
        let replacement = dir.path().join("replacement");
        std::fs::create_dir(&replacement).unwrap();
        std::fs::write(replacement.join("source.img"), b"xyz").unwrap();
        std::fs::remove_file(&link).unwrap();
        std::os::unix::fs::symlink(&replacement, &link).unwrap();
        assert!(source.unchanged().is_err());
    }
}
