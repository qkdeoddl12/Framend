use crate::SourceFingerprint;
use std::{
    fs::{File, OpenOptions},
    io::{self, Read},
    os::unix::fs::{FileExt, MetadataExt, OpenOptionsExt},
    path::{Path, PathBuf},
};

pub(crate) fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

pub(crate) struct Source {
    pub file: File,
    pub fingerprint: SourceFingerprint,
    path: PathBuf,
}

impl Source {
    pub fn open(path: &Path) -> io::Result<Self> {
        // O_NONBLOCK prevents an unexpected FIFO from hanging before fstat.
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(path)?;
        let metadata = file.metadata()?;
        if !metadata.is_file() {
            return Err(invalid(
                "일반 디스크 이미지 파일만 열 수 있습니다. 장치와 폴더는 지원하지 않습니다.",
            ));
        }
        let fingerprint = fingerprint(&metadata);
        Ok(Self {
            file,
            fingerprint,
            path: path.to_owned(),
        })
    }

    pub fn unchanged(&self) -> io::Result<()> {
        if fingerprint(&self.file.metadata()?) != self.fingerprint
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
        self.file.read_exact_at(bytes, offset)
    }

    pub fn region(&self, offset: u64, length: u64) -> Region<'_> {
        Region {
            source: self,
            position: offset,
            remaining: length,
        }
    }
}

fn fingerprint(m: &std::fs::Metadata) -> SourceFingerprint {
    SourceFingerprint {
        device: m.dev(),
        inode: m.ino(),
        size: m.len(),
        modified_seconds: m.mtime(),
        modified_nanos: m.mtime_nsec(),
        changed_seconds: m.ctime(),
        changed_nanos: m.ctime_nsec(),
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
        let read = self
            .source
            .file
            .read_at(&mut buffer[..count], self.position)?;
        self.position += read as u64;
        self.remaining -= read as u64;
        Ok(read)
    }
}
