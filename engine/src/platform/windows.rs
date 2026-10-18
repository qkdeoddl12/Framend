use super::{Identity, SourceFingerprint};
use std::{
    ffi::OsStr,
    fs::{File, OpenOptions},
    io,
    os::windows::{
        ffi::OsStrExt,
        fs::{FileExt, MetadataExt, OpenOptionsExt},
        io::AsRawHandle,
    },
    path::Path,
};
use windows_sys::Win32::Storage::FileSystem::{
    BY_HANDLE_FILE_INFORMATION, FILE_ATTRIBUTE_REPARSE_POINT, FILE_BASIC_INFO,
    FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_DELETE, FILE_SHARE_READ,
    FILE_SHARE_WRITE, FileBasicInfo, GetDiskFreeSpaceExW, GetFileInformationByHandle,
    GetFileInformationByHandleEx,
};

fn wide(value: &OsStr) -> io::Result<Vec<u16>> {
    let mut result: Vec<_> = value.encode_wide().collect();
    if result.contains(&0) {
        return Err(super::invalid("NUL in filesystem path"));
    }
    result.push(0);
    Ok(result)
}

pub(super) fn check_name(value: &OsStr) -> io::Result<()> {
    let text = value.to_string_lossy();
    // Colon permits NTFS alternate data streams; trailing dots/spaces and device
    // names have Win32 normalization rules that are unsafe for exclusive outputs.
    let stem = text
        .split('.')
        .next()
        .unwrap_or_default()
        .to_ascii_uppercase();
    let reserved = matches!(
        stem.as_str(),
        "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$"
    ) || (stem.len() == 4
        && (stem.starts_with("COM") || stem.starts_with("LPT"))
        && stem.as_bytes()[3].is_ascii_digit());
    if reserved
        || text.ends_with(['.', ' '])
        || text
            .chars()
            .any(|ch| ch < ' ' || "<>:\"/\\|?*".contains(ch))
    {
        return Err(super::invalid("Unsafe Windows filename"));
    }
    wide(value).map(|_| ())
}

fn no_reparse(file: File) -> io::Result<File> {
    if file.metadata()?.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return Err(super::invalid("Reparse points are not allowed"));
    }
    Ok(file)
}

pub(super) fn open_root(path: &Path) -> io::Result<File> {
    // Directory handles deny DELETE sharing for their entire lifetime. This also
    // prevents a previously checked ancestor from becoming a redirected junction.
    let file = OpenOptions::new()
        .read(true)
        .share_mode(FILE_SHARE_READ)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)?;
    if !file.metadata()?.is_dir() {
        return Err(super::invalid("Expected a directory"));
    }
    no_reparse(file)
}

pub(super) fn open_directory(_: &File, name: &OsStr, path: &Path) -> io::Result<File> {
    check_name(name)?;
    open_root(path)
}

pub(super) fn open_file(_: &File, name: &OsStr, path: &Path, create: bool) -> io::Result<File> {
    check_name(name)?;
    no_reparse(
        OpenOptions::new()
            .read(true)
            .write(create)
            .create_new(create)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_BACKUP_SEMANTICS)
            .open(path)?,
    )
}

fn information(file: &File) -> io::Result<BY_HANDLE_FILE_INFORMATION> {
    let mut info = std::mem::MaybeUninit::uninit();
    // SAFETY: a live handle and writable correctly sized output storage.
    if unsafe { GetFileInformationByHandle(file.as_raw_handle(), info.as_mut_ptr()) } == 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: the successful API call initialized the complete structure.
    Ok(unsafe { info.assume_init() })
}

pub(super) fn identity(file: &File) -> io::Result<Identity> {
    let info = information(file)?;
    Ok(Identity(
        u64::from(info.dwVolumeSerialNumber),
        (u64::from(info.nFileIndexHigh) << 32) | u64::from(info.nFileIndexLow),
    ))
}

pub(crate) fn fingerprint(file: &File) -> io::Result<SourceFingerprint> {
    let Identity(device, inode) = identity(file)?;
    let mut basic = std::mem::MaybeUninit::<FILE_BASIC_INFO>::uninit();
    // SAFETY: live handle and writable storage whose size is supplied to Win32.
    if unsafe {
        GetFileInformationByHandleEx(
            file.as_raw_handle(),
            FileBasicInfo,
            basic.as_mut_ptr().cast(),
            std::mem::size_of::<FILE_BASIC_INFO>() as u32,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: successful call initialized basic.
    let basic = unsafe { basic.assume_init() };
    let timestamp = |ticks: i64| {
        let unix_ticks = ticks - 116_444_736_000_000_000;
        (
            unix_ticks.div_euclid(10_000_000),
            unix_ticks.rem_euclid(10_000_000) * 100,
        )
    };
    let (modified_seconds, modified_nanos) = timestamp(basic.LastWriteTime);
    let (changed_seconds, changed_nanos) = timestamp(basic.ChangeTime);
    Ok(SourceFingerprint {
        device,
        inode,
        size: file.metadata()?.len(),
        modified_seconds,
        modified_nanos,
        changed_seconds,
        changed_nanos,
    })
}

pub(crate) fn read_at(file: &File, bytes: &mut [u8], offset: u64) -> io::Result<usize> {
    file.seek_read(bytes, offset)
}

pub(super) fn free_space(_: &File, path: &Path) -> io::Result<u128> {
    let path = wide(path.as_os_str())?;
    let mut available = 0;
    // SAFETY: NUL-terminated path, valid output pointer; other outputs are optional.
    if unsafe {
        GetDiskFreeSpaceExW(
            path.as_ptr(),
            &mut available,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok(u128::from(available))
}

pub(super) fn mkdir(_: &File, _: &OsStr, path: &Path) -> io::Result<()> {
    std::fs::create_dir(path)
}

pub(super) fn remove_file(_: &File, _: &OsStr, path: &Path) -> io::Result<()> {
    std::fs::remove_file(path)
}
