use recovery_engine::{
    Event, RecoveryRequest, SourceFingerprint, exfat, jobs, native_video, recover, scan,
};
use serde::{Deserialize, Serialize};
use std::{
    io::{self, Read, Write},
    path::Path,
};

fn main() {
    if let Err(error) = run() {
        eprintln!("{error}");
        std::process::exit(1);
    }
}
fn emit(output: &mut impl Write, value: &impl Serialize) -> io::Result<()> {
    serde_json::to_writer(&mut *output, value)?;
    output.write_all(b"\n")?;
    output.flush()
}
fn request<T: serde::de::DeserializeOwned>() -> io::Result<T> {
    let mut bytes = Vec::new();
    io::stdin()
        .take(32 * 1024 * 1024 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > 32 * 1024 * 1024 {
        return Err(io::Error::other("복구 요청이 너무 큽니다."));
    }
    Ok(serde_json::from_slice(&bytes)?)
}
#[derive(Deserialize)]
struct ExtractRequest {
    source: SourceFingerprint,
    candidate: exfat::ExfatCandidate,
}
fn run() -> io::Result<()> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    let stdout = io::stdout();
    let mut output = stdout.lock();
    match args.first().and_then(|a| a.to_str()) {
        Some("desktop-job") if args.len() == 1 => {
            jobs::run(request()?, |event| emit(&mut output, &event))
        }
        Some("scan") if args.len() == 2 => scan(Path::new(&args[1]), |event: Event| {
            emit(&mut output, &event)
        }),
        Some("recover") if args.len() == 3 => {
            let request: RecoveryRequest = request()?;
            recover(
                Path::new(&args[1]),
                Path::new(&args[2]),
                &request,
                |event| emit(&mut output, &event),
            )
        }
        Some(command @ ("video-scan" | "file-scan")) if args.len() == 2 => {
            let scan = if command == "file-scan" {
                exfat::analyze_files(Path::new(&args[1]))?
            } else {
                exfat::analyze(Path::new(&args[1]))?
            };
            emit(&mut output, &scan)
        }
        Some(command @ ("video-extract" | "file-extract")) if args.len() == 3 => {
            let request: ExtractRequest = request()?;
            let extract = if command == "file-extract" {
                exfat::extract_file
            } else {
                exfat::extract
            };
            let sha256 = extract(
                Path::new(&args[1]),
                Path::new(&args[2]),
                &request.source,
                &request.candidate,
                |scanned, total| {
                    emit(
                        &mut output,
                        &serde_json::json!({"event":"progress","scanned":scanned,"total":total}),
                    )
                },
            )?;
            emit(
                &mut output,
                &serde_json::json!({"event":"complete","sha256":sha256}),
            )
        }
        Some("video-salvage") if args.len() == 4 => {
            let report = native_video::salvage(
                Path::new(&args[1]),
                Path::new(&args[2]),
                Path::new(&args[3]),
                |scanned, total| {
                    emit(
                        &mut output,
                        &serde_json::json!({"event":"progress","scanned":scanned,"total":total}),
                    )
                },
            )?;
            emit(
                &mut output,
                &serde_json::json!({"event":"complete","report":report}),
            )
        }
        _ => Err(io::Error::other(
            "Usage: recovery-engine scan IMAGE | recover IMAGE OUTPUT_DIR < request.json | file-scan IMAGE | file-extract IMAGE OUTPUT_FILE < request.json | video-scan IMAGE | video-extract IMAGE OUTPUT_FILE < request.json | video-salvage INPUT REFERENCE OUTPUT_ES | desktop-job < request.json",
        )),
    }
}
