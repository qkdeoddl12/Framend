use process_wrap::std::CommandWrap;
use serde::Serialize;
use serde_json::Value;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::process::{Command, Stdio};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
    mpsc,
};
use std::time::{Duration, Instant};

pub const REQUEST_LIMIT: usize = 32 * 1024 * 1024;
const LINE_LIMIT: usize = 1024 * 1024;
const OUTPUT_LIMIT: usize = 256 * 1024 * 1024;
const STDERR_LIMIT: usize = 64 * 1024;

#[derive(Clone, Debug, Serialize)]
pub struct Terminal {
    pub event: &'static str,
    pub success: bool,
    pub cancelled: bool,
    pub error: Option<String>,
}

impl Terminal {
    pub fn failure(error: impl ToString) -> Self {
        Self {
            event: "terminal",
            success: false,
            cancelled: false,
            error: Some(error.to_string()),
        }
    }
}

pub fn parse_event(bytes: &[u8]) -> io::Result<Value> {
    let value: Value = serde_json::from_slice(bytes)?;
    let valid = match value["event"].as_str() {
        Some("started") => true,
        Some("progress") => {
            value["phase"].is_string()
                && value["completed"].is_u64()
                && (value["total"].is_null() || value["total"].is_u64())
        }
        Some("candidate") => {
            let e = &value["entry"];
            ["id", "name", "format", "selection"]
                .iter()
                .all(|key| e[key].is_string())
                && e["bytes"].is_u64()
                && e["recoverable"].is_boolean()
                && e["warnings"]
                    .as_array()
                    .is_some_and(|warnings| warnings.iter().all(Value::is_string))
                && e.get("inspection").is_some()
        }
        Some("saved") => {
            let r = &value["result"];
            r["name"].is_string()
                && r["path"].is_string()
                && r["bytes"].is_u64()
                && r["partial"].is_boolean()
                && r.get("inspection").is_some()
                && (r["sha256"].is_null() || r["sha256"].is_string())
                && (r["error"].is_null() || r["error"].is_string())
        }
        Some("complete") => {
            (value["directory"].is_null() || value["directory"].is_string())
                && value["count"].is_u64()
                && value["failed"].is_u64()
        }
        _ => false,
    };
    if !valid {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Invalid recovery worker event",
        ));
    }
    Ok(value)
}

pub fn read_line(reader: &mut impl BufRead, limit: usize) -> io::Result<Option<Vec<u8>>> {
    let mut line = Vec::new();
    loop {
        let available = reader.fill_buf()?;
        if available.is_empty() {
            return Ok(if line.is_empty() { None } else { Some(line) });
        }
        let end = available.iter().position(|byte| *byte == b'\n');
        let count = end.map_or(available.len(), |at| at + 1);
        if line.len().saturating_add(count) > limit {
            return Err(io::Error::other("Recovery worker event exceeds size limit"));
        }
        line.extend_from_slice(&available[..count]);
        reader.consume(count);
        if end.is_some() {
            return Ok(Some(line));
        }
    }
}

enum Message {
    Event(Value),
    Error(String),
    Eof,
}

struct Tree(Box<dyn process_wrap::std::ChildWrapper>);
impl Drop for Tree {
    fn drop(&mut self) {
        let _ = self.0.start_kill();
        let _ = self.0.inner_mut().try_wait();
    }
}

pub fn run(
    mut command: Command,
    request: Vec<u8>,
    cancel: Arc<AtomicBool>,
    mut emit: impl FnMut(Value),
) -> Terminal {
    if request.len() > REQUEST_LIMIT {
        return Terminal::failure("Recovery request exceeds 32 MiB");
    }
    if cancel.load(Ordering::SeqCst) {
        return Terminal {
            event: "terminal",
            success: false,
            cancelled: true,
            error: None,
        };
    }
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    command.env("FRAMEND_DESKTOP_PROCESS_GROUP", "1");
    let mut wrapped = CommandWrap::from(command);
    #[cfg(unix)]
    wrapped.wrap(process_wrap::std::ProcessGroup::leader());
    #[cfg(windows)]
    {
        wrapped.wrap(process_wrap::std::CreationFlags(
            windows::Win32::System::Threading::CREATE_NO_WINDOW,
        ));
        wrapped.wrap(process_wrap::std::JobObject);
    }
    let mut child = match wrapped.spawn() {
        Ok(child) => Tree(child),
        Err(error) => return Terminal::failure(error),
    };
    let mut stdin = child.0.stdin().take().expect("piped stdin");
    let stdout = child.0.stdout().take().expect("piped stdout");
    let mut stderr = child.0.stderr().take().expect("piped stderr");
    let (sender, receiver) = mpsc::sync_channel(32);
    let writer_sender = sender.clone();
    std::thread::spawn(move || {
        if let Err(error) = stdin.write_all(&request) {
            let _ = writer_sender.send(Message::Error(format!("Worker request: {error}")));
        }
    });
    std::thread::spawn(move || {
        let mut reader = BufReader::new(stdout);
        let mut total = 0usize;
        loop {
            match read_line(&mut reader, LINE_LIMIT) {
                Ok(Some(line)) => {
                    total = total.saturating_add(line.len());
                    if total > OUTPUT_LIMIT {
                        let _ = sender.send(Message::Error(
                            "Recovery worker output exceeds size limit".into(),
                        ));
                        break;
                    }
                    match parse_event(&line) {
                        Ok(event) => {
                            if sender.send(Message::Event(event)).is_err() {
                                return;
                            }
                        }
                        Err(error) => {
                            let _ = sender.send(Message::Error(error.to_string()));
                            break;
                        }
                    }
                }
                Ok(None) => break,
                Err(error) => {
                    let _ = sender.send(Message::Error(error.to_string()));
                    break;
                }
            }
        }
        let _ = sender.send(Message::Eof);
    });
    let errors = Arc::new(Mutex::new(Vec::new()));
    let collected = errors.clone();
    let stderr_done = Arc::new(AtomicBool::new(false));
    let done = stderr_done.clone();
    std::thread::spawn(move || {
        let mut chunk = [0u8; 4096];
        while let Ok(count) = stderr.read(&mut chunk) {
            if count == 0 {
                break;
            }
            let mut bytes = collected.lock().unwrap_or_else(|e| e.into_inner());
            bytes.extend_from_slice(&chunk[..count]);
            if bytes.len() > STDERR_LIMIT {
                let excess = bytes.len() - STDERR_LIMIT;
                bytes.drain(..excess);
            }
        }
        done.store(true, Ordering::SeqCst);
    });
    let mut status = None;
    let mut stopped_at = None;
    let mut stdout_done = false;
    let mut complete = false;
    let mut protocol_error = None;
    let mut cancelled = false;
    loop {
        if cancel.load(Ordering::SeqCst) && !cancelled {
            cancelled = true;
            let _ = child.0.start_kill();
            stopped_at.get_or_insert_with(Instant::now);
        }
        if status.is_none() {
            // Inspect the parent separately: group wait can include descendants that inherited pipes.
            match child.0.inner_mut().try_wait() {
                Ok(Some(exit)) => {
                    status = Some(exit);
                    let _ = child.0.start_kill();
                    stopped_at.get_or_insert_with(Instant::now);
                }
                Err(error) => {
                    protocol_error.get_or_insert_with(|| error.to_string());
                    let _ = child.0.start_kill();
                    stopped_at.get_or_insert_with(Instant::now);
                }
                Ok(None) => {}
            }
        }
        match receiver.recv_timeout(Duration::from_millis(20)) {
            Ok(Message::Event(event)) => {
                if complete {
                    protocol_error
                        .get_or_insert("Recovery worker emitted data after complete".into());
                } else if !cancelled && protocol_error.is_none() {
                    if event["event"] == "complete" {
                        complete = true;
                    }
                    emit(event);
                }
            }
            Ok(Message::Error(error)) => {
                protocol_error.get_or_insert(error);
            }
            Ok(Message::Eof) | Err(mpsc::RecvTimeoutError::Disconnected) => stdout_done = true,
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
        if protocol_error.is_some() {
            let _ = child.0.start_kill();
            stopped_at.get_or_insert_with(Instant::now);
        }
        if status.is_some() && stdout_done && stderr_done.load(Ordering::SeqCst) {
            break;
        }
        if stopped_at.is_some_and(|at| at.elapsed() >= Duration::from_secs(2)) {
            if !cancelled {
                protocol_error
                    .get_or_insert("Recovery worker pipes did not close after exit".into());
            }
            break;
        }
    }
    if cancelled {
        return Terminal {
            event: "terminal",
            success: false,
            cancelled: true,
            error: None,
        };
    }
    let success = status.is_some_and(|exit| exit.success()) && complete && protocol_error.is_none();
    let error = if success {
        None
    } else {
        let bytes = errors.lock().unwrap_or_else(|e| e.into_inner());
        let stderr = String::from_utf8_lossy(&bytes);
        let reason = protocol_error.unwrap_or_else(|| {
            if !status.is_some_and(|exit| exit.success()) {
                format!("Recovery worker failed ({status:?})")
            } else {
                "Recovery worker exited without a complete event".into()
            }
        });
        Some(if stderr.trim().is_empty() {
            reason
        } else {
            format!("{reason}\n{}", stderr.trim())
        })
    };
    Terminal {
        event: "terminal",
        success,
        cancelled: false,
        error,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };
    use std::time::{Duration, Instant};

    #[test]
    fn protocol_rejects_invalid_and_oversized_lines() {
        assert!(
            parse_event(b"{\"event\":\"complete\",\"directory\":null,\"count\":0,\"failed\":0}")
                .is_ok()
        );
        assert!(parse_event(b"{\"event\":\"surprise\"}").is_err());
        assert!(parse_event(b"{\"event\":\"candidate\",\"entry\":{}}").is_err());
        assert!(read_line(&mut std::io::BufReader::new(&b"abcdefghi\n"[..]), 5).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn worker_success_requires_complete_event() {
        let mut command = std::process::Command::new("/bin/sh");
        command.args([
            "-c",
            "cat >/dev/null; printf '%s\\n' '{\"event\":\"started\"}'",
        ]);
        let result = run(
            command,
            b"{}".to_vec(),
            Arc::new(AtomicBool::new(false)),
            |_| {},
        );
        assert!(!result.success);
        assert!(result.error.unwrap().contains("complete"));
    }

    #[cfg(unix)]
    #[test]
    fn nonzero_worker_retains_bounded_stderr() {
        let mut command = std::process::Command::new("/bin/sh");
        command.args(["-c", "cat >/dev/null; echo 'decoder failed' >&2; exit 9"]);
        let result = run(
            command,
            b"{}".to_vec(),
            Arc::new(AtomicBool::new(false)),
            |_| {},
        );
        assert!(!result.success);
        assert!(result.error.unwrap().contains("decoder failed"));
    }

    #[cfg(unix)]
    #[test]
    fn cancelled_worker_kills_descendants_holding_pipes_and_terminates() {
        let directory = tempfile::tempdir().unwrap();
        let marker = directory.path().join("escaped-child");
        let mut command = std::process::Command::new("/bin/sh");
        command
            .arg("-c")
            .arg("(sleep 1; touch \"$1\") & wait")
            .arg("test")
            .arg(&marker);
        let cancel = Arc::new(AtomicBool::new(false));
        let trigger = cancel.clone();
        let handle = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(100));
            trigger.store(true, Ordering::SeqCst);
        });
        let start = Instant::now();
        let result = run(command, vec![], cancel, |_| {});
        handle.join().unwrap();
        assert!(result.cancelled);
        assert!(!result.success);
        assert!(start.elapsed() < Duration::from_secs(3));
        std::thread::sleep(Duration::from_millis(1100));
        assert!(!marker.exists(), "descendant survived cancellation");
    }

    #[cfg(unix)]
    #[test]
    fn parent_exit_does_not_wait_forever_for_inherited_stdout() {
        let mut command = std::process::Command::new("/bin/sh");
        command.args(["-c", "sleep 30 & printf '%s\\n' '{\"event\":\"complete\",\"directory\":null,\"count\":0,\"failed\":0}'"]);
        let start = Instant::now();
        let result = run(command, vec![], Arc::new(AtomicBool::new(false)), |_| {});
        assert!(start.elapsed() < Duration::from_secs(3));
        assert!(result.success, "{:?}", result.error);
    }

    #[cfg(windows)]
    fn powershell(script: &str) -> Command {
        let mut command = Command::new("powershell.exe");
        command.args(["-NoProfile", "-NonInteractive", "-Command", script]);
        command
    }

    #[cfg(windows)]
    #[test]
    fn windows_worker_requires_completion_and_zero_exit() {
        let command = powershell(
            "[Console]::In.ReadToEnd() | Out-Null; [Console]::Out.WriteLine('{\"event\":\"started\"}')",
        );
        let result = run(
            command,
            b"{}".to_vec(),
            Arc::new(AtomicBool::new(false)),
            |_| {},
        );
        assert!(!result.success);
        assert!(result.error.unwrap().contains("complete"));
        let command = powershell(
            "[Console]::In.ReadToEnd() | Out-Null; [Console]::Out.WriteLine('{\"event\":\"complete\",\"directory\":null,\"count\":0,\"failed\":0}')",
        );
        let result = run(
            command,
            b"{}".to_vec(),
            Arc::new(AtomicBool::new(false)),
            |_| {},
        );
        assert!(result.success, "{:?}", result.error);
    }

    #[cfg(windows)]
    #[test]
    fn windows_cancel_terminates_job_object_descendants() {
        let directory = tempfile::tempdir().unwrap();
        let marker = directory.path().join("escaped-child");
        let ready = directory.path().join("ready");
        let mut command = powershell(
            "$script = 'Start-Sleep -Seconds 2; [IO.File]::WriteAllText($env:FRAMEND_MARKER, ''escaped''); Start-Sleep -Seconds 30'; Start-Process -FilePath ($PSHOME + '\\powershell.exe') -NoNewWindow -ArgumentList @('-NoProfile','-NonInteractive','-Command',$script); [IO.File]::WriteAllText($env:FRAMEND_READY, 'ready'); Start-Sleep -Seconds 30",
        );
        command
            .env("FRAMEND_MARKER", &marker)
            .env("FRAMEND_READY", &ready);
        let cancel = Arc::new(AtomicBool::new(false));
        let trigger = cancel.clone();
        let handle = std::thread::spawn(move || {
            let start = Instant::now();
            while !ready.exists() && start.elapsed() < Duration::from_secs(8) {
                std::thread::sleep(Duration::from_millis(25));
            }
            let started = ready.exists();
            trigger.store(true, Ordering::SeqCst);
            started
        });
        let start = Instant::now();
        let result = run(command, vec![], cancel, |_| {});
        assert!(handle.join().unwrap(), "test worker did not start");
        assert!(result.cancelled);
        assert!(!result.success);
        assert!(start.elapsed() < Duration::from_secs(11));
        std::thread::sleep(Duration::from_millis(2500));
        assert!(
            !marker.exists(),
            "descendant survived Job Object cancellation"
        );
    }
}
