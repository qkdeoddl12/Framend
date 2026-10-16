use recovery_engine::{Event, RecoveryRequest, recover, scan};
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

fn run() -> io::Result<()> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    let stdout = io::stdout();
    let mut output = stdout.lock();
    let emit = |event: Event| -> io::Result<()> {
        serde_json::to_writer(&mut output, &event)?;
        output.write_all(b"\n")?;
        output.flush()
    };
    match args.first().and_then(|a| a.to_str()) {
        Some("scan") if args.len() == 2 => scan(Path::new(&args[1]), emit),
        Some("recover") if args.len() == 3 => {
            let mut bytes = Vec::new();
            io::stdin()
                .take(32 * 1024 * 1024 + 1)
                .read_to_end(&mut bytes)?;
            if bytes.len() > 32 * 1024 * 1024 {
                return Err(io::Error::other("복구 요청이 너무 큽니다."));
            }
            let request: RecoveryRequest = serde_json::from_slice(&bytes)?;
            recover(Path::new(&args[1]), Path::new(&args[2]), &request, emit)
        }
        _ => Err(io::Error::other(
            "Usage: recovery-engine scan IMAGE | recovery-engine recover IMAGE OUTPUT_DIR < request.json",
        )),
    }
}
