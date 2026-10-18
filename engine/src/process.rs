//! Bounded external-tool execution. Desktop children inherit the desktop worker's group.
use process_wrap::std::*;
use sha2::{Digest, Sha256};
use std::{
    ffi::OsString,
    fs::File,
    io::{self, Read, Write},
    path::Path,
    process::{ExitStatus, Stdio},
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

pub(crate) struct Output {
    pub status: ExitStatus,
    pub stdout: Vec<u8>,
    pub stdout_sha256: String,
    pub stderr: Vec<u8>,
}

struct Captured {
    bytes: Vec<u8>,
    sha256: String,
}

pub(crate) fn run(
    executable: &Path,
    arguments: &[OsString],
    destination: Option<File>,
    stdout_limit: u64,
    timeout: Duration,
) -> io::Result<Output> {
    let mut command = CommandWrap::with_new(executable, |command| {
        command
            .args(arguments)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
    });
    #[cfg(unix)]
    if std::env::var_os("FRAMEND_DESKTOP_PROCESS_GROUP").as_deref()
        != Some(std::ffi::OsStr::new("1"))
    {
        command.wrap(ProcessGroup::leader());
    }
    #[cfg(windows)]
    command.wrap(JobObject);
    let mut child = command.spawn()?;
    let stdout = child
        .stdout()
        .take()
        .ok_or_else(|| io::Error::other("Missing tool stdout"))?;
    let stderr = child
        .stderr()
        .take()
        .ok_or_else(|| io::Error::other("Missing tool stderr"))?;
    let (sender, receiver) = mpsc::channel();
    let out_sender = sender.clone();
    thread::spawn(move || {
        let _ = out_sender.send((true, drain(stdout, destination, stdout_limit)));
    });
    thread::spawn(move || {
        let _ = sender.send((false, drain(stderr, None, 512 * 1024)));
    });
    let start = Instant::now();
    let mut ended = None;
    let mut status = None;
    let mut out = None;
    let mut err = None;
    let result = 'poll: loop {
        while let Ok((is_stdout, result)) = receiver.try_recv() {
            match result {
                Ok(bytes) if is_stdout => out = Some(bytes),
                Ok(bytes) => err = Some(bytes),
                Err(error) => break 'poll Err(error),
            }
        }
        if status.is_none() {
            match child.try_wait() {
                Ok(Some(value)) => {
                    status = Some(value);
                    ended = Some(Instant::now());
                }
                Ok(None) => {}
                Err(error) => break Err(error),
            }
        }
        if let (Some(status), Some(stdout), Some(stderr)) = (status, out.as_mut(), err.as_mut()) {
            break Ok(Output {
                status,
                stdout: std::mem::take(&mut stdout.bytes),
                stdout_sha256: std::mem::take(&mut stdout.sha256),
                stderr: std::mem::take(&mut stderr.bytes),
            });
        }
        if start.elapsed() >= timeout {
            break Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "External tool timed out",
            ));
        }
        if ended.is_some_and(|time: Instant| time.elapsed() > Duration::from_secs(2)) {
            break Err(io::Error::other("External tool left a pipe open"));
        }
        thread::sleep(Duration::from_millis(20));
    };
    // Kill lingering descendants as well as the leader. In desktop mode the
    // bridge also tears down the enclosing group when this worker terminates.
    let _ = child.start_kill();
    let reap_start = Instant::now();
    while matches!(child.try_wait(), Ok(None)) && reap_start.elapsed() < Duration::from_secs(2) {
        thread::sleep(Duration::from_millis(20));
    }
    result
}

fn drain(mut reader: impl Read, mut destination: Option<File>, limit: u64) -> io::Result<Captured> {
    let mut bytes = Vec::new();
    let mut hash = Sha256::new();
    let mut count = 0u64;
    let mut buffer = [0; 32 * 1024];
    loop {
        let read = reader.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        count = count
            .checked_add(read as u64)
            .ok_or_else(|| io::Error::other("Tool output size overflow"))?;
        if count > limit {
            return Err(io::Error::other("External tool output exceeded its limit"));
        }
        hash.update(&buffer[..read]);
        if let Some(file) = destination.as_mut() {
            file.write_all(&buffer[..read])?;
        } else {
            bytes.extend_from_slice(&buffer[..read]);
        }
    }
    if let Some(file) = destination {
        file.sync_all()?;
    }
    Ok(Captured {
        bytes,
        sha256: format!("{:x}", hash.finalize()),
    })
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    fn shell(script: &str, limit: u64, timeout: Duration) -> io::Result<Output> {
        run(
            Path::new("/bin/sh"),
            &["-c".into(), script.into()],
            None,
            limit,
            timeout,
        )
    }
    #[test]
    fn captures_both_streams_and_nonzero_exit() {
        let result = shell(
            "printf result; printf diagnostic >&2; exit 7",
            1024,
            Duration::from_secs(3),
        )
        .unwrap();
        assert_eq!(result.status.code(), Some(7));
        assert_eq!(result.stdout, b"result");
        assert_eq!(result.stderr, b"diagnostic");
    }
    #[test]
    fn stops_timeout_and_output_overflow() {
        assert_eq!(
            shell("sleep 10", 1024, Duration::from_millis(50))
                .err()
                .unwrap()
                .kind(),
            io::ErrorKind::TimedOut
        );
        assert!(shell("head -c 4096 /dev/zero", 64, Duration::from_secs(3)).is_err());
    }
    #[test]
    fn inherited_pipe_has_bounded_shutdown() {
        let start = Instant::now();
        assert!(shell("sleep 10 & exit 0", 1024, Duration::from_secs(5)).is_err());
        assert!(start.elapsed() < Duration::from_secs(4));
    }
}
