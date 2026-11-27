use recovery_engine::jobs::{Request, run};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    fs, io,
    path::{Path, PathBuf},
};
fn tempdir() -> io::Result<tempfile::TempDir> {
    tempfile::tempdir_in(std::env::temp_dir().canonicalize()?)
}

fn jpeg() -> Vec<u8> {
    vec![
        0xff, 0xd8, 0xff, 0xc0, 0, 11, 8, 0, 2, 0, 3, 1, 1, 0x11, 0, 0xff, 0xda, 0, 8, 1, 1, 0, 0,
        63, 0, 42, 0xff, 0xd9,
    ]
}

fn wave() -> Vec<u8> {
    let mut bytes = b"RIFF".to_vec();
    bytes.extend(100u32.to_le_bytes());
    bytes.extend(b"WAVEfmt ");
    bytes.extend(16u32.to_le_bytes());
    bytes.extend(1u16.to_le_bytes());
    bytes.extend(1u16.to_le_bytes());
    bytes.extend(8_000u32.to_le_bytes());
    bytes.extend(16_000u32.to_le_bytes());
    bytes.extend(2u16.to_le_bytes());
    bytes.extend(16u16.to_le_bytes());
    bytes.extend(b"data");
    bytes.extend(64u32.to_le_bytes());
    bytes.extend([0; 64]);
    bytes
}

fn selection(path: &Path) -> String {
    let events = job(json!({"operation":"scan","source":path,"method":"carve"})).unwrap();
    events
        .iter()
        .find(|event| event["event"] == "candidate")
        .unwrap()["entry"]["selection"]
        .as_str()
        .unwrap()
        .to_owned()
}

fn session(parent: &Path) -> PathBuf {
    fs::read_dir(parent)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|path| path.is_dir())
        .unwrap()
}

fn job(value: Value) -> io::Result<Vec<Value>> {
    let request: Request = serde_json::from_value(value)?;
    let mut events = Vec::new();
    run(request, |event| {
        events.push(event);
        Ok(())
    })?;
    Ok(events)
}

#[test]
fn desktop_scan_selection_is_opaque_and_recovers_exact_bytes() {
    let temp = tempdir().unwrap();
    let source = temp.path().join("한글 source.img");
    fs::write(&source, [vec![0; 9], jpeg()].concat()).unwrap();
    let events = job(json!({"operation":"scan","source":source,"method":"carve"})).unwrap();
    let entry = &events.iter().find(|e| e["event"] == "candidate").unwrap()["entry"];
    let selection = entry["selection"].as_str().unwrap();
    assert!(serde_json::from_str::<Value>(selection).unwrap()["source"]["inode"].is_u64());
    let result = job(json!({"operation":"recover","source":source,"output":temp.path(),"selections":[selection]})).unwrap();
    let saved = &result.iter().find(|e| e["event"] == "saved").unwrap()["result"];
    assert_eq!(fs::read(saved["path"].as_str().unwrap()).unwrap(), jpeg());
    assert_eq!(saved["sha256"], format!("{:x}", Sha256::digest(jpeg())));
    assert_eq!(result.last().unwrap()["event"], "complete");
    assert!(source.exists());
}

#[test]
fn altered_source_and_forged_or_duplicate_selections_are_rejected() {
    let temp = tempdir().unwrap();
    let source = temp.path().join("card.img");
    fs::write(&source, jpeg()).unwrap();
    let events = job(json!({"operation":"scan","source":source,"method":"carve"})).unwrap();
    let token = events.iter().find(|e| e["event"] == "candidate").unwrap()["entry"]["selection"]
        .as_str()
        .unwrap();
    assert!(job(json!({"operation":"recover","source":source,"output":temp.path(),"selections":[token,token]})).is_err());
    let mut forged: Value = serde_json::from_str(token).unwrap();
    forged["candidate"]["offset"] = json!(1);
    assert!(job(json!({"operation":"recover","source":source,"output":temp.path(),"selections":[forged.to_string()]})).is_err());
    fs::write(&source, [jpeg(), vec![1]].concat()).unwrap();
    assert!(
        job(
            json!({"operation":"recover","source":source,"output":temp.path(),"selections":[token]})
        )
        .is_err()
    );
}

#[test]
fn invalid_method_and_empty_recovery_have_no_output() {
    assert!(
        serde_json::from_value::<Request>(
            json!({"operation":"scan","source":"x","method":"guess"})
        )
        .is_err()
    );
    let temp = tempdir().unwrap();
    let source = temp.path().join("input.img");
    fs::write(&source, jpeg()).unwrap();
    assert!(
        job(json!({"operation":"recover","source":source,"output":temp.path(),"selections":[]}))
            .is_err()
    );
    assert_eq!(fs::read_dir(temp.path()).unwrap().count(), 1);
}

#[test]
fn callback_failure_keeps_incomplete_session() {
    let temp = tempdir().unwrap();
    let source = temp.path().join("input.img");
    fs::write(&source, jpeg()).unwrap();
    let events = job(json!({"operation":"scan","source":source,"method":"carve"})).unwrap();
    let token = &events.iter().find(|e| e["event"] == "candidate").unwrap()["entry"]["selection"];
    let request = serde_json::from_value(
        json!({"operation":"recover","source":source,"output":temp.path(),"selections":[token]}),
    )
    .unwrap();
    let result = run(request, |event| {
        if event["event"] == "saved" {
            Err(io::Error::other("cancel"))
        } else {
            Ok(())
        }
    });
    assert!(result.is_err());
    let dirs: Vec<_> = fs::read_dir(temp.path())
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.is_dir())
        .collect();
    assert_eq!(dirs.len(), 1);
    assert!(dirs[0].join("INCOMPLETE.txt").exists());
}

#[test]
fn tampered_evidence_never_finishes_as_success() {
    for target in ["tools.json", "file_000001.evidence.json", "report.json"] {
        let temp = tempdir().unwrap();
        let source = temp.path().join("input.img");
        fs::write(&source, jpeg()).unwrap();
        let events = job(json!({"operation":"scan","source":source,"method":"carve"})).unwrap();
        let token = &events
            .iter()
            .find(|event| event["event"] == "candidate")
            .unwrap()["entry"]["selection"];
        let request = serde_json::from_value(json!({"operation":"recover","source":source,"output":temp.path(),"selections":[token]})).unwrap();
        let mut directory = None;
        let result = run(request, |event| {
            if event["event"] == "saved" {
                let path = std::path::Path::new(event["result"]["path"].as_str().unwrap())
                    .parent()
                    .unwrap()
                    .to_owned();
                if target != "report.json" {
                    fs::write(path.join(target), b"forged evidence")?;
                }
                directory = Some(path);
            }
            if event["event"] == "complete" && target == "report.json" {
                fs::write(directory.as_ref().unwrap().join(target), b"forged report")?;
            }
            Ok(())
        });
        assert!(result.is_err(), "tampering {target} was accepted");
        assert!(directory.unwrap().join("INCOMPLETE.txt").exists());
    }
}

#[test]
fn source_replacement_during_copy_keeps_incomplete_evidence() {
    let temp = tempdir().unwrap();
    let retained = tempdir().unwrap();
    let source = temp.path().join("source.img");
    fs::write(&source, wave()).unwrap();
    let request = serde_json::from_value(json!({"operation":"recover","source":source,"output":temp.path(),"selections":[selection(&source)]})).unwrap();
    let mut replaced = false;
    let mut completed = false;
    let result = run(request, |event| {
        if !replaced && event["event"] == "progress" && event["phase"] == "copy" {
            fs::rename(&source, retained.path().join("original-source.img"))?;
            fs::write(&source, wave())?;
            replaced = true;
        }
        completed |= event["event"] == "complete";
        Ok(())
    });
    assert!(replaced);
    assert!(result.is_err());
    assert!(!completed);
    assert!(session(temp.path()).join("INCOMPLETE.txt").exists());
}

#[test]
fn output_replacement_during_copy_is_detected_without_touching_replacement() {
    let temp = tempdir().unwrap();
    let retained = tempdir().unwrap();
    let source = temp.path().join("source.img");
    fs::write(&source, wave()).unwrap();
    let request = serde_json::from_value(json!({"operation":"recover","source":source,"output":temp.path(),"selections":[selection(&source)]})).unwrap();
    let mut replaced = None;
    let mut completed = false;
    let result = run(request, |event| {
        if replaced.is_none() && event["event"] == "progress" && event["phase"] == "copy" {
            let directory = session(temp.path());
            let path = directory.join("file_000001.wav");
            fs::rename(&path, retained.path().join("retained.wav"))?;
            fs::write(&path, b"replacement")?;
            replaced = Some(path);
        }
        completed |= event["event"] == "complete";
        Ok(())
    });
    assert!(result.is_err());
    assert!(!completed);
    assert_eq!(fs::read(replaced.unwrap()).unwrap(), b"replacement");
    assert!(session(temp.path()).join("INCOMPLETE.txt").exists());
}

#[test]
fn failed_complete_callback_restores_incomplete_marker() {
    let temp = tempdir().unwrap();
    let source = temp.path().join("source.img");
    fs::write(&source, wave()).unwrap();
    let request = serde_json::from_value(json!({"operation":"recover","source":source,"output":temp.path(),"selections":[selection(&source)]})).unwrap();
    let mut reached = false;
    let result = run(request, |event| {
        if event["event"] == "complete" {
            reached = true;
            return Err(io::Error::other("receiver stopped"));
        }
        Ok(())
    });
    assert!(reached);
    assert!(result.is_err());
    let directory = session(temp.path());
    assert!(directory.join("INCOMPLETE.txt").exists());
    assert_eq!(fs::read(directory.join("file_000001.wav")).unwrap(), wave());
}

#[cfg(unix)]
#[test]
fn replaced_job_session_receives_no_redirected_evidence() {
    let temp = tempdir().unwrap();
    let source = temp.path().join("source.img");
    fs::write(&source, wave()).unwrap();
    let request = serde_json::from_value(json!({"operation":"recover","source":source,"output":temp.path(),"selections":[selection(&source)]})).unwrap();
    let mut replacement = None;
    let result = run(request, |event| {
        if event["event"] == "saved" {
            let directory = Path::new(event["result"]["path"].as_str().unwrap())
                .parent()
                .unwrap();
            fs::rename(directory, temp.path().join("retained-session"))?;
            fs::create_dir(directory)?;
            replacement = Some(directory.to_owned());
        }
        Ok(())
    });
    assert!(result.is_err());
    assert_eq!(fs::read_dir(replacement.unwrap()).unwrap().count(), 0);
    assert!(temp.path().join("retained-session/INCOMPLETE.txt").exists());
}
