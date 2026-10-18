//! Pinned, exclusive filesystem access shared by recovery and desktop jobs.
use crate::{SourceFingerprint, source::invalid};
use std::{
    ffi::{OsStr, OsString},
    fs::File,
    io::{self, Read, Write},
    path::{Component, Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

#[cfg(unix)]
#[path = "platform/unix.rs"]
mod os;
#[cfg(windows)]
#[path = "platform/windows.rs"]
mod os;
pub(crate) use os::{fingerprint, read_at};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Identity(u64, u64);

pub(crate) struct GuardedDirectory {
    path: PathBuf,
    // Retain every ancestor. Windows denies delete-sharing on these handles,
    // preventing replacement by a junction between validation and creation.
    directories: Vec<File>,
    identities: Vec<Identity>,
}

impl GuardedDirectory {
    pub(crate) fn open(path: &Path) -> io::Result<Self> {
        let path = if path.is_absolute() {
            path.to_owned()
        } else {
            std::env::current_dir()?.join(path)
        };
        let mut current = PathBuf::new();
        let mut directories = Vec::new();
        let mut identities = Vec::new();
        for component in path.components() {
            match component {
                Component::Prefix(prefix) => current.push(prefix.as_os_str()),
                Component::RootDir => {
                    current.push(component.as_os_str());
                    let directory = os::open_root(&current)?;
                    identities.push(os::identity(&directory)?);
                    directories.push(directory);
                }
                Component::CurDir => {}
                Component::Normal(name) => {
                    let parent = directories
                        .last()
                        .ok_or_else(|| invalid("Missing path root"))?;
                    current.push(name);
                    let directory = os::open_directory(parent, name, &current)?;
                    identities.push(os::identity(&directory)?);
                    directories.push(directory);
                }
                Component::ParentDir => return Err(invalid("Parent traversal is not allowed")),
            }
        }
        if directories.is_empty() {
            return Err(invalid("Missing output directory"));
        }
        Ok(Self {
            path: current,
            directories,
            identities,
        })
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    pub(crate) fn verify(&self) -> io::Result<()> {
        let current = Self::open(&self.path)?;
        if current.identities != self.identities {
            return Err(invalid("Output/source directory was replaced"));
        }
        Ok(())
    }

    fn handle(&self) -> &File {
        // Construction guarantees at least the root directory is retained.
        &self.directories[self.directories.len() - 1]
    }

    pub(crate) fn new_session(parent: &Path, prefix: &str) -> io::Result<Self> {
        Self::open(parent)?.create_session(prefix)
    }

    pub(crate) fn create_session(&self, prefix: &str) -> io::Result<Self> {
        checked_name(OsStr::new(prefix))?;
        let parent = self;
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(io::Error::other)?
            .as_nanos();
        for attempt in 0..100 {
            let name = OsString::from(format!("{prefix}-{stamp}-{attempt}"));
            parent.verify()?;
            match os::mkdir(parent.handle(), &name, &parent.path.join(&name)) {
                Ok(()) => {
                    parent.verify()?;
                    let path = parent.path.join(&name);
                    let child = os::open_directory(parent.handle(), &name, &path)?;
                    let child_identity = os::identity(&child)?;
                    let session = Self::open(&path)?;
                    if session.identities.len() != parent.identities.len() + 1
                        || !session.identities.starts_with(&parent.identities)
                        || session.identities.last() != Some(&child_identity)
                    {
                        return Err(invalid("Output session directory was replaced"));
                    }
                    return Ok(session);
                }
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error),
            }
        }
        Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "Cannot create a unique recovery session",
        ))
    }

    pub(crate) fn open_existing(&self, name: &OsStr) -> io::Result<File> {
        checked_name(name)?;
        self.verify()?;
        let file = os::open_file(self.handle(), name, &self.path.join(name), false)?;
        regular(&file)?;
        self.verify()?;
        Ok(file)
    }

    pub(crate) fn create_new(&self, name: &OsStr, required_bytes: u64) -> io::Result<File> {
        checked_name(name)?;
        self.check_space(required_bytes)?;
        let file = os::open_file(self.handle(), name, &self.path.join(name), true)?;
        regular(&file)?;
        self.verify_file(name, &file)?;
        Ok(file)
    }

    pub(crate) fn check_space(&self, required_bytes: u64) -> io::Result<()> {
        self.verify()?;
        if os::free_space(self.handle(), &self.path)? < u128::from(required_bytes) {
            return Err(io::Error::other(
                "Insufficient free space for recovered file",
            ));
        }
        self.verify()
    }

    pub(crate) fn verify_file(&self, name: &OsStr, file: &File) -> io::Result<()> {
        let current = self.open_existing(name)?;
        if os::identity(&current)? != os::identity(file)? {
            return Err(invalid("Output file was replaced"));
        }
        Ok(())
    }

    pub(crate) fn write_new(&self, name: &OsStr, bytes: &[u8]) -> io::Result<()> {
        let mut file = self.create_new(name, bytes.len() as u64)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        self.verify_file(name, &file)?;
        verify_written_bytes(&file, bytes)?;
        self.verify_file(name, &file)
    }

    pub(crate) fn publish(&self, partial: &OsStr, target: &OsStr) -> io::Result<()> {
        let mut source = self.open_existing(partial)?;
        let expected = fingerprint(&source)?;
        let mut output = self.create_new(target, expected.size)?;
        // A fixed-size buffer bounds memory on exFAT and other filesystems where
        // exclusive atomic rename is unavailable. Never open a target for truncation.
        let mut buffer = vec![0; 1024 * 1024];
        let mut remaining = expected.size;
        while remaining != 0 {
            let count = remaining.min(buffer.len() as u64) as usize;
            source.read_exact(&mut buffer[..count])?;
            output.write_all(&buffer[..count])?;
            remaining -= count as u64;
        }
        output.sync_all()?;
        self.verify_file(target, &output)?;
        let published = self.open_existing(target)?;
        if os::identity(&published)? != os::identity(&output)? {
            return Err(invalid("Published output was replaced"));
        }
        // Close the writable handle before the fingerprint baseline: Windows
        // may finish updating last-write metadata when that handle closes.
        drop(output);
        let published_fingerprint = verify_published_copy(&source, &expected, &published)?;
        self.verify_file(target, &published)?;
        self.verify_file(partial, &source)?;
        if fingerprint(&source)? != expected || fingerprint(&published)? != published_fingerprint {
            return Err(invalid("Output changed before publication completed"));
        }
        self.remove_file(partial)
    }

    pub(crate) fn remove_file(&self, name: &OsStr) -> io::Result<()> {
        checked_name(name)?;
        self.verify()?;
        os::remove_file(self.handle(), name, &self.path.join(name))?;
        self.verify()
    }
}

fn verify_published_copy(
    source: &File,
    expected: &SourceFingerprint,
    output: &File,
) -> io::Result<SourceFingerprint> {
    if fingerprint(source)? != *expected {
        return Err(invalid("Partial output changed during publication"));
    }
    let published = fingerprint(output)?;
    if published.size != expected.size {
        return Err(invalid("Published output length changed"));
    }
    let mut source_buffer = vec![0; 64 * 1024];
    let mut output_buffer = vec![0; source_buffer.len()];
    let mut offset = 0;
    while offset < expected.size {
        let count = (expected.size - offset).min(source_buffer.len() as u64) as usize;
        read_exact_at(source, &mut source_buffer[..count], offset)?;
        read_exact_at(output, &mut output_buffer[..count], offset)?;
        if source_buffer[..count] != output_buffer[..count] {
            return Err(invalid("Published output content changed"));
        }
        offset += count as u64;
    }
    if fingerprint(source)? != *expected || fingerprint(output)? != published {
        return Err(invalid("Output changed during publication verification"));
    }
    Ok(published)
}

fn verify_written_bytes(file: &File, bytes: &[u8]) -> io::Result<()> {
    let expected = fingerprint(file)?;
    if expected.size != bytes.len() as u64 {
        return Err(invalid("Written report length changed"));
    }
    let mut buffer = vec![0; 64 * 1024];
    let mut offset = 0;
    for intended in bytes.chunks(buffer.len()) {
        let actual = &mut buffer[..intended.len()];
        read_exact_at(file, actual, offset)?;
        if actual != intended {
            return Err(invalid("Written report content changed"));
        }
        offset += intended.len() as u64;
    }
    if fingerprint(file)? != expected {
        return Err(invalid("Written report changed during verification"));
    }
    Ok(())
}

fn checked_name(name: &OsStr) -> io::Result<()> {
    let mut components = Path::new(name).components();
    if !matches!(components.next(), Some(Component::Normal(_))) || components.next().is_some() {
        return Err(invalid("Expected one plain filename"));
    }
    os::check_name(name)
}

fn regular(file: &File) -> io::Result<()> {
    if !file.metadata()?.is_file() {
        return Err(invalid(
            "Only regular disk image/output files are supported",
        ));
    }
    Ok(())
}

pub(crate) fn read_exact_at(file: &File, mut bytes: &mut [u8], mut offset: u64) -> io::Result<()> {
    while !bytes.is_empty() {
        match read_at(file, bytes, offset) {
            Ok(0) => {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "Short image read",
                ));
            }
            Ok(count) => {
                offset += count as u64;
                bytes = &mut bytes[count..];
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

/// Retains the parent pins until extraction has synced and verified its file.
pub(crate) struct GuardedOutput {
    directory: GuardedDirectory,
    name: OsString,
    file: File,
}

impl GuardedOutput {
    pub(crate) fn create(path: &Path, length: u64) -> io::Result<Self> {
        let name = path
            .file_name()
            .ok_or_else(|| invalid("Output filename missing"))?
            .to_owned();
        let parent = path
            .parent()
            .filter(|path| !path.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        let directory = GuardedDirectory::open(parent)?;
        let file = directory.create_new(&name, length)?;
        Ok(Self {
            directory,
            name,
            file,
        })
    }

    pub(crate) fn sync_all(&self) -> io::Result<()> {
        self.file.sync_all()?;
        self.directory.verify_file(&self.name, &self.file)
    }
}

impl Write for GuardedOutput {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.file.write(bytes)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.file.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn tempdir() -> tempfile::TempDir {
        tempfile::tempdir_in(std::env::temp_dir().canonicalize().unwrap()).unwrap()
    }

    #[test]
    fn exclusive_output_supports_unicode_names_and_bounded_publication() {
        let dir = tempdir();
        let session = GuardedDirectory::new_session(dir.path(), "Recovery").unwrap();
        let partial = OsStr::new("복구-画像.partial");
        let target = OsStr::new("복구-画像.bin");
        let bytes = vec![0x4a; 2 * 1024 * 1024 + 17];
        session.write_new(partial, &bytes).unwrap();
        session.publish(partial, target).unwrap();
        assert_eq!(fs::read(session.path().join(target)).unwrap(), bytes);
        assert!(!session.path().join(partial).exists());
    }

    #[test]
    fn existing_destinations_and_parent_traversal_are_refused() {
        let dir = tempdir();
        let guard = GuardedDirectory::open(dir.path()).unwrap();
        guard.write_new(OsStr::new("keep"), b"keep").unwrap();
        assert_eq!(
            guard.create_new(OsStr::new("keep"), 0).unwrap_err().kind(),
            io::ErrorKind::AlreadyExists
        );
        for name in ["../escape", "a/b", ".", "..", ""] {
            assert!(guard.create_new(OsStr::new(name), 0).is_err());
        }
        assert_eq!(fs::read(dir.path().join("keep")).unwrap(), b"keep");
    }

    #[test]
    fn space_check_fails_before_creating_a_file() {
        let dir = tempdir();
        let guard = GuardedDirectory::open(dir.path()).unwrap();
        assert!(guard.create_new(OsStr::new("huge"), u64::MAX).is_err());
        assert!(!dir.path().join("huge").exists());
    }

    #[test]
    fn report_verification_checks_content_and_length_through_the_pinned_file() {
        let dir = tempdir();
        let guard = GuardedDirectory::open(dir.path()).unwrap();
        let bytes = vec![0x6a; 128 * 1024 + 17];
        let mut file = guard.create_new(OsStr::new("report"), 0).unwrap();
        file.write_all(&bytes).unwrap();
        file.sync_all().unwrap();
        verify_written_bytes(&file, &bytes).unwrap();

        let mut wrong = bytes.clone();
        wrong[64 * 1024 + 3] ^= 1;
        assert!(verify_written_bytes(&file, &wrong).is_err());
        assert!(verify_written_bytes(&file, &bytes[..bytes.len() - 1]).is_err());
        file.set_len(bytes.len() as u64 - 1).unwrap();
        assert!(verify_written_bytes(&file, &bytes).is_err());
    }

    #[test]
    fn publication_verification_rejects_changed_content_lengths_and_source() {
        let dir = tempdir();
        let guard = GuardedDirectory::open(dir.path()).unwrap();
        let bytes = vec![0x6a; 128 * 1024 + 17];
        guard.write_new(OsStr::new("partial"), &bytes).unwrap();
        guard.write_new(OsStr::new("published"), &bytes).unwrap();
        let source = guard.open_existing(OsStr::new("partial")).unwrap();
        let output = guard.open_existing(OsStr::new("published")).unwrap();
        let expected = fingerprint(&source).unwrap();
        verify_published_copy(&source, &expected, &output).unwrap();

        let mut wrong = bytes.clone();
        wrong[64 * 1024 + 3] ^= 1;
        fs::write(dir.path().join("published"), &wrong).unwrap();
        assert!(verify_published_copy(&source, &expected, &output).is_err());
        fs::write(dir.path().join("published"), &bytes[..bytes.len() - 1]).unwrap();
        assert!(verify_published_copy(&source, &expected, &output).is_err());

        fs::write(dir.path().join("partial"), &bytes[..bytes.len() - 1]).unwrap();
        assert!(verify_published_copy(&source, &expected, &output).is_err());
        assert!(dir.path().join("partial").exists());
    }

    #[test]
    fn replaced_output_file_is_detected() {
        let dir = tempdir();
        let guard = GuardedDirectory::open(dir.path()).unwrap();
        let file = guard.create_new(OsStr::new("output"), 0).unwrap();
        fs::rename(dir.path().join("output"), dir.path().join("moved")).unwrap();
        fs::write(dir.path().join("output"), b"replacement").unwrap();
        assert!(guard.verify_file(OsStr::new("output"), &file).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn directory_replacement_cannot_redirect_output_creation() {
        let dir = tempdir();
        let parent = dir.path().join("parent");
        fs::create_dir(&parent).unwrap();
        let guard = GuardedDirectory::open(&parent).unwrap();
        fs::rename(&parent, dir.path().join("old")).unwrap();
        fs::create_dir(&parent).unwrap();
        assert!(guard.create_new(OsStr::new("escape"), 0).is_err());
        assert!(guard.create_session("Recovery").is_err());
        assert_eq!(fs::read_dir(&parent).unwrap().count(), 0);
    }

    #[cfg(unix)]
    #[test]
    fn linked_parent_and_output_are_rejected() {
        let dir = tempdir();
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(dir.path(), &link).unwrap();
        assert!(GuardedDirectory::open(&link).is_err());
        let file = dir.path().join("file");
        fs::write(&file, b"keep").unwrap();
        let alias = dir.path().join("alias");
        std::os::unix::fs::symlink(&file, &alias).unwrap();
        let guard = GuardedDirectory::open(dir.path()).unwrap();
        assert!(guard.open_existing(OsStr::new("alias")).is_err());
        assert!(guard.create_new(OsStr::new("alias"), 0).is_err());
        assert_eq!(fs::read(file).unwrap(), b"keep");
    }

    #[cfg(windows)]
    fn junction(target: &Path, link: &Path) {
        let result = std::process::Command::new("cmd")
            .arg("/C")
            .arg("mklink")
            .arg("/J")
            .arg(link)
            .arg(target)
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
    }

    #[cfg(windows)]
    #[test]
    fn windows_rejects_junction_ancestors_without_redirected_writes() {
        let dir = tempdir();
        let real = dir.path().join("real");
        fs::create_dir(&real).unwrap();
        let link = dir.path().join("junction");
        junction(&real, &link);
        assert!(GuardedDirectory::open(&link).is_err());
        assert!(GuardedOutput::create(&link.join("escape"), 0).is_err());
        assert_eq!(fs::read_dir(&real).unwrap().count(), 0);
        fs::remove_dir(link).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn windows_pins_parent_against_junction_replacement() {
        let dir = tempdir();
        let parent = dir.path().join("parent");
        fs::create_dir(&parent).unwrap();
        let guard = GuardedDirectory::open(&parent).unwrap();
        assert!(fs::rename(&parent, dir.path().join("old")).is_err());
        assert!(fs::remove_dir(&parent).is_err());
        guard.write_new(OsStr::new("safe"), b"safe").unwrap();
        assert_eq!(fs::read(parent.join("safe")).unwrap(), b"safe");
    }

    #[cfg(windows)]
    #[test]
    fn windows_rejects_alternate_streams_and_ambiguous_names() {
        let dir = tempdir();
        let guard = GuardedDirectory::open(dir.path()).unwrap();
        for name in ["file:secret", "NUL", "COM1.txt", "trailing.", "trailing "] {
            assert!(guard.create_new(OsStr::new(name), 0).is_err(), "{name}");
        }
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 0);
    }
}
