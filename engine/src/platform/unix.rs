use super::{Identity, SourceFingerprint};
use std::{
    ffi::{CString, OsStr},
    fs::{File, Metadata},
    io,
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::{
            ffi::OsStrExt,
            fs::{FileExt, MetadataExt},
        },
    },
    path::Path,
};

fn name(value: &OsStr) -> io::Result<CString> {
    CString::new(value.as_bytes()).map_err(|_| super::invalid("NUL in filesystem path"))
}

pub(super) fn check_name(value: &OsStr) -> io::Result<()> {
    name(value).map(|_| ())
}

fn owned(fd: i32) -> io::Result<File> {
    if fd < 0 {
        Err(io::Error::last_os_error())
    } else {
        // SAFETY: successful open/openat returns an exclusively owned descriptor.
        Ok(unsafe { File::from_raw_fd(fd) })
    }
}

pub(super) fn open_root(path: &Path) -> io::Result<File> {
    let path = name(path.as_os_str())?;
    // SAFETY: path is a live, NUL-terminated string and no ownership is transferred.
    owned(unsafe {
        libc::open(
            path.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    })
}

pub(super) fn open_directory(parent: &File, value: &OsStr, _: &Path) -> io::Result<File> {
    let name = name(value)?;
    // SAFETY: live directory descriptor and NUL-terminated single component.
    owned(unsafe {
        libc::openat(
            parent.as_raw_fd(),
            name.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    })
}

pub(super) fn open_file(parent: &File, value: &OsStr, _: &Path, create: bool) -> io::Result<File> {
    let name = name(value)?;
    let flags = libc::O_NOFOLLOW
        | libc::O_NONBLOCK
        | libc::O_CLOEXEC
        | if create {
            libc::O_RDWR | libc::O_CREAT | libc::O_EXCL
        } else {
            libc::O_RDONLY
        };
    // SAFETY: live directory descriptor and NUL-terminated single component.
    owned(unsafe { libc::openat(parent.as_raw_fd(), name.as_ptr(), flags, 0o600) })
}

pub(super) fn identity(file: &File) -> io::Result<Identity> {
    let metadata = file.metadata()?;
    Ok(Identity(metadata.dev(), metadata.ino()))
}

pub(crate) fn fingerprint(file: &File) -> io::Result<SourceFingerprint> {
    let m: Metadata = file.metadata()?;
    Ok(SourceFingerprint {
        device: m.dev(),
        inode: m.ino(),
        size: m.len(),
        modified_seconds: m.mtime(),
        modified_nanos: m.mtime_nsec(),
        changed_seconds: m.ctime(),
        changed_nanos: m.ctime_nsec(),
    })
}

pub(crate) fn read_at(file: &File, bytes: &mut [u8], offset: u64) -> io::Result<usize> {
    file.read_at(bytes, offset)
}

pub(super) fn free_space(directory: &File, _: &Path) -> io::Result<u128> {
    let mut stats = std::mem::MaybeUninit::<libc::statvfs>::uninit();
    // SAFETY: writable storage initialized by fstatvfs on success.
    if unsafe { libc::fstatvfs(directory.as_raw_fd(), stats.as_mut_ptr()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: the successful call above initialized the structure.
    let stats = unsafe { stats.assume_init() };
    Ok(u128::from(stats.f_bavail) * u128::from(stats.f_frsize))
}

pub(super) fn mkdir(directory: &File, value: &OsStr, _: &Path) -> io::Result<()> {
    let name = name(value)?;
    // SAFETY: live directory descriptor and single-component name.
    if unsafe { libc::mkdirat(directory.as_raw_fd(), name.as_ptr(), 0o700) } == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

pub(super) fn remove_file(directory: &File, value: &OsStr, _: &Path) -> io::Result<()> {
    let name = name(value)?;
    // SAFETY: live directory descriptor and single-component name; no traversal.
    if unsafe { libc::unlinkat(directory.as_raw_fd(), name.as_ptr(), 0) } == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}
