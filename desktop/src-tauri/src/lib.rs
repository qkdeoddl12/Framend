mod worker;

use recovery_engine::jobs::{self, Request};
use serde::Serialize;
use serde_json::{Value, json};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
use tauri::{Emitter, Manager, State};

struct ActiveJob {
    id: String,
    cancel: Arc<AtomicBool>,
}

#[derive(Default)]
struct Jobs {
    active: Mutex<Option<ActiveJob>>,
    results: Mutex<HashSet<PathBuf>>,
    closing: AtomicBool,
    previewing: AtomicBool,
}

#[derive(Clone, Serialize)]
struct Envelope {
    job_id: String,
    payload: Value,
}

fn worker_path() -> Result<PathBuf, String> {
    let executable = std::env::current_exe().map_err(|e| e.to_string())?;
    let name = if cfg!(windows) {
        "recovery-engine.exe"
    } else {
        "recovery-engine"
    };
    let path = executable
        .parent()
        .ok_or("Missing application directory")?
        .join(name);
    if path.is_file() {
        return Ok(path);
    }
    #[cfg(debug_assertions)]
    {
        let directory = Path::new(env!("CARGO_MANIFEST_DIR")).join("binaries");
        let target = option_env!("TAURI_ENV_TARGET_TRIPLE").unwrap_or("");
        if !target.is_empty() {
            let suffix = if cfg!(windows) { ".exe" } else { "" };
            let staged = directory.join(format!("recovery-engine-{target}{suffix}"));
            if staged.is_file() {
                return Ok(staged);
            }
        }
        let engine = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../engine/target/release")
            .join(name);
        if engine.is_file() {
            return Ok(engine);
        }
    }
    Err("Bundled recovery engine was not found. Build using npm run desktop:build.".into())
}

#[tauri::command]
fn start_job(
    app: tauri::AppHandle,
    state: State<'_, Jobs>,
    job_id: String,
    request: Request,
) -> Result<(), String> {
    if job_id.is_empty()
        || job_id.len() > 128
        || !job_id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        return Err("Invalid job identifier".into());
    }
    if state.closing.load(Ordering::SeqCst) {
        return Err("Application is closing".into());
    }
    let bytes = serde_json::to_vec(&request).map_err(|e| e.to_string())?;
    if bytes.len() > worker::REQUEST_LIMIT {
        return Err("Recovery request exceeds 32 MiB".into());
    }
    let executable = worker_path()?;
    let mut active = state.active.lock().map_err(|_| "Job lock failed")?;
    if state.closing.load(Ordering::SeqCst) {
        return Err("Application is closing".into());
    }
    if active.is_some() {
        return Err("A recovery job is already running".into());
    }
    if let Ok(value) = serde_json::from_slice::<Value>(&bytes) {
        if let Some(directory) = value["output"]
            .as_str()
            .and_then(|path| Path::new(path).canonicalize().ok())
        {
            if directory.is_dir() {
                state
                    .results
                    .lock()
                    .map_err(|_| "Result lock failed")?
                    .insert(directory);
            }
        }
    }
    let cancel = Arc::new(AtomicBool::new(false));
    *active = Some(ActiveJob {
        id: job_id.clone(),
        cancel: cancel.clone(),
    });
    let spawn = std::thread::Builder::new().name("framend-worker".into()).spawn(move || {
        let terminal = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let mut command = Command::new(executable);
                command.arg("desktop-job");
                worker::run(command, bytes, cancel, |payload| {
                let state = app.state::<Jobs>();
                let directory = if payload["event"] == "complete" { payload["directory"].as_str().map(PathBuf::from) }
                    else if payload["event"] == "saved" { payload["result"]["path"].as_str().and_then(|p| Path::new(p).parent().map(Path::to_path_buf)) }
                    else { None };
                if let Some(directory) = directory.and_then(|p| p.canonicalize().ok()) {
                    if let Ok(mut results) = state.results.lock() { results.insert(directory); }
                }
                let _ = app.emit("framend-event", Envelope { job_id: job_id.clone(), payload });
            })
        })).unwrap_or_else(|_| worker::Terminal::failure("Recovery worker supervisor panicked"));
        let state = app.state::<Jobs>();
        if let Ok(mut active) = state.active.lock() {
            if active.as_ref().is_some_and(|a| a.id == job_id) { *active = None; }
        }
        let _ = app.emit("framend-event", Envelope { job_id, payload: serde_json::to_value(terminal).unwrap_or_else(|_| json!({"event":"terminal","success":false,"cancelled":false,"error":"Invalid terminal event"})) });
        if state.closing.load(Ordering::SeqCst) { app.exit(0); }
    });
    if let Err(error) = spawn {
        *active = None;
        return Err(error.to_string());
    }
    Ok(())
}

#[tauri::command]
fn cancel_job(state: State<'_, Jobs>, job_id: String) -> Result<(), String> {
    let active = state.active.lock().map_err(|_| "Job lock failed")?;
    if let Some(job) = active.as_ref().filter(|job| job.id == job_id) {
        job.cancel.store(true, Ordering::SeqCst);
    }
    Ok(())
}

#[tauri::command]
async fn preview_image(
    state: State<'_, Jobs>,
    source: String,
    selection: String,
) -> Result<jobs::Preview, String> {
    if source.len() > 32768 || selection.len() > 1024 * 1024 {
        return Err("Preview request exceeds size limit".into());
    }
    if state.previewing.swap(true, Ordering::SeqCst) {
        return Err("Another image preview is still loading".into());
    }
    let result = tauri::async_runtime::spawn_blocking(move || {
        jobs::preview(Path::new(&source), &selection).map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string());
    state.previewing.store(false, Ordering::SeqCst);
    result?
}

#[tauri::command]
fn open_result_directory(state: State<'_, Jobs>, path: String) -> Result<(), String> {
    let path = PathBuf::from(path)
        .canonicalize()
        .map_err(|e| e.to_string())?;
    if !path.is_dir()
        || !state
            .results
            .lock()
            .map_err(|_| "Result lock failed")?
            .contains(&path)
    {
        return Err("Only directories produced by this application session can be opened".into());
    }
    #[cfg(target_os = "macos")]
    let mut command = {
        let mut command = Command::new("/usr/bin/open");
        command.arg("--").arg(&path);
        command
    };
    #[cfg(target_os = "windows")]
    let mut command = {
        let root = std::env::var_os("WINDIR").ok_or("Windows directory unavailable")?;
        let mut command = Command::new(PathBuf::from(root).join("explorer.exe"));
        command.arg(&path);
        command
    };
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    let mut command = {
        let mut command = Command::new("xdg-open");
        command.arg(&path);
        command
    };
    command
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map(|_| ())
        .map_err(|e| e.to_string())
}

pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .manage(Jobs::default())
        .invoke_handler(tauri::generate_handler![
            start_job,
            cancel_job,
            preview_image,
            open_result_directory
        ])
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                let state = window.state::<Jobs>();
                state.closing.store(true, Ordering::SeqCst);
                if let Ok(active) = state.active.lock() {
                    if let Some(job) = active.as_ref() {
                        api.prevent_close();
                        job.cancel.store(true, Ordering::SeqCst);
                    }
                }
            }
        })
        .build(tauri::generate_context!())
        .expect("Cannot build Framend desktop application")
        .run(|app, event| {
            if let tauri::RunEvent::ExitRequested { api, .. } = event {
                let state = app.state::<Jobs>();
                state.closing.store(true, Ordering::SeqCst);
                if let Ok(active) = state.active.lock() {
                    if let Some(job) = active.as_ref() {
                        api.prevent_exit();
                        job.cancel.store(true, Ordering::SeqCst);
                    }
                }
            }
        });
}
