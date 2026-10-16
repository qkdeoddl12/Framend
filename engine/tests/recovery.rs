use recovery_engine::{Candidate, Event, Format, RecoveryRequest, recover, scan};
use std::{fs, io, path::Path};
use tempfile::tempdir;

// Structural fixture. Actual JPEG pixel decoding is exercised by the end-to-end fixture.
fn jpeg() -> Vec<u8> {
    vec![
        0xff, 0xd8, 0xff, 0xe1, 0, 8, 0xff, 0xd8, 0xff, 0xd9, 1, 2, 0xff, 0xc0, 0, 11, 8, 0, 2, 0,
        3, 1, 1, 0x11, 0, 0xff, 0xda, 0, 8, 1, 1, 0, 0, 63, 0, 42, 0xff, 0, 0xd9, 42, 0xff, 0xd0,
        43, 0xff, 0xd9,
    ]
}

fn raf() -> Vec<u8> {
    let preview = jpeg();
    let mut data = vec![0; 148];
    data[..16].copy_from_slice(b"FUJIFILMCCD-RAW ");
    data[28..32].copy_from_slice(b"X-T5");
    let meta_offset = 148 + preview.len();
    let raw_offset = meta_offset + 16;
    for (at, value) in [
        (84, 148),
        (88, preview.len()),
        (92, meta_offset),
        (96, 16),
        (100, raw_offset),
        (104, 32),
    ] {
        data[at..at + 4].copy_from_slice(&(value as u32).to_be_bytes());
    }
    data.extend(preview);
    data.extend([0; 16]);
    data.extend(b"II*\0");
    data.extend([42; 28]);
    data
}

fn events(path: &Path) -> io::Result<Vec<Event>> {
    let mut result = Vec::new();
    scan(path, |e| {
        result.push(e);
        Ok(())
    })?;
    Ok(result)
}

fn candidates(events: &[Event]) -> Vec<Candidate> {
    events
        .iter()
        .filter_map(|e| match e {
            Event::Candidate { candidate } => Some(candidate.clone()),
            _ => None,
        })
        .collect()
}

fn request(events: &[Event]) -> RecoveryRequest {
    let source = events
        .iter()
        .find_map(|e| match e {
            Event::Started { source } => Some(source.clone()),
            _ => None,
        })
        .unwrap();
    RecoveryRequest {
        source,
        candidates: candidates(events),
    }
}

#[test]
fn jpeg_skips_embedded_thumbnail_and_stuffed_entropy_bytes() {
    let temp = tempdir().unwrap();
    let image = temp.path().join("card.img");
    let photo = jpeg();
    let bytes = [vec![0; 29], photo.clone(), vec![0; 41]].concat();
    fs::write(&image, &bytes).unwrap();
    let found = candidates(&events(&image).unwrap());
    assert_eq!(found.len(), 1);
    assert_eq!(
        (
            found[0].offset,
            found[0].length,
            found[0].width,
            found[0].height
        ),
        (29, photo.len() as u64, Some(3), Some(2))
    );
    assert_eq!(fs::read(image).unwrap(), bytes);
}

#[test]
fn raf_covers_all_sections_without_reporting_its_preview_as_a_photo() {
    let temp = tempdir().unwrap();
    let image = temp.path().join("card.img");
    let raw = raf();
    fs::write(&image, [vec![0; 19], raw.clone(), jpeg()].concat()).unwrap();
    let found = candidates(&events(&image).unwrap());
    assert_eq!(found.len(), 2);
    assert_eq!(
        (found[0].format, found[0].length, found[0].camera.as_deref()),
        (Format::Raf, raw.len() as u64, Some("X-T5"))
    );
}

#[test]
fn signatures_crossing_scan_buffer_boundaries_are_found_once() {
    let temp = tempdir().unwrap();
    let image = temp.path().join("card.img");
    fs::write(
        &image,
        [vec![0; 4 * 1024 * 1024 - 1], raf(), jpeg()].concat(),
    )
    .unwrap();
    assert_eq!(candidates(&events(&image).unwrap()).len(), 2);
}

#[test]
fn truncated_jpeg_and_overflowing_raf_extent_are_not_complete_candidates() {
    let temp = tempdir().unwrap();
    let image = temp.path().join("bad.img");
    let mut raw = raf();
    raw[100..104].copy_from_slice(&u32::MAX.to_be_bytes());
    // Remove the preview too; do not mistake a salvageable preview for a bad RAF.
    raw[148..].fill(0);
    let mut photo = jpeg();
    photo.truncate(photo.len() - 2);
    fs::write(&image, [raw, photo].concat()).unwrap();
    assert!(candidates(&events(&image).unwrap()).is_empty());
}

#[test]
fn scan_rejects_directories_and_special_devices() {
    assert!(events(Path::new("/dev/null")).is_err());
    assert!(events(tempdir().unwrap().path()).is_err());
}

#[test]
fn export_is_exact_preserves_source_and_never_overwrites_prior_runs() {
    let temp = tempdir().unwrap();
    let image = temp.path().join("card.img");
    let bytes = [vec![0; 17], jpeg(), raf()].concat();
    fs::write(&image, &bytes).unwrap();
    let req = request(&events(&image).unwrap());
    let mut paths = Vec::new();
    for _ in 0..2 {
        recover(&image, temp.path(), &req, |e| {
            if let Event::Saved {
                candidate,
                path,
                sha256,
            } = e
            {
                assert_eq!(
                    fs::read(&path).unwrap(),
                    bytes
                        [candidate.offset as usize..(candidate.offset + candidate.length) as usize]
                );
                assert_eq!(sha256.len(), 64);
                paths.push(path);
            }
            Ok(())
        })
        .unwrap();
    }
    paths.sort();
    paths.dedup();
    assert_eq!(paths.len(), 4);
    assert_eq!(fs::read(image).unwrap(), bytes);
}

#[test]
fn changed_source_is_rejected_before_creating_output() {
    let temp = tempdir().unwrap();
    let image = temp.path().join("card.img");
    fs::write(&image, jpeg()).unwrap();
    let req = request(&events(&image).unwrap());
    fs::write(&image, vec![0; 5]).unwrap();
    assert!(recover(&image, temp.path(), &req, |_| Ok(())).is_err());
    assert_eq!(fs::read_dir(temp.path()).unwrap().count(), 1);
}

#[test]
fn forged_ranges_and_output_file_are_rejected() {
    let temp = tempdir().unwrap();
    let image = temp.path().join("card.img");
    fs::write(&image, jpeg()).unwrap();
    let mut req = request(&events(&image).unwrap());
    assert!(recover(&image, &image, &req, |_| Ok(())).is_err());
    req.candidates[0].length = u64::MAX;
    assert!(recover(&image, temp.path(), &req, |_| Ok(())).is_err());
    assert_eq!(fs::read(&image).unwrap(), jpeg());
}

#[test]
fn callback_error_stops_scanning_without_a_success_event() {
    let temp = tempdir().unwrap();
    let image = temp.path().join("card.img");
    fs::write(&image, jpeg()).unwrap();
    assert!(
        scan(&image, |_| Err(io::Error::new(
            io::ErrorKind::Interrupted,
            "cancelled"
        )))
        .is_err()
    );
}

#[test]
fn invalid_raf_preview_must_not_hide_an_intact_jpeg() {
    let temp = tempdir().unwrap();
    let image = temp.path().join("bad-raf.img");
    let mut bytes = vec![0; 2048];
    bytes[..16].copy_from_slice(b"FUJIFILMCCD-RAW ");
    for (at, value) in [
        (84, 148u32),
        (88, 100),
        (92, 248),
        (96, 16),
        (100, 264),
        (104, 1500),
    ] {
        bytes[at..at + 4].copy_from_slice(&value.to_be_bytes());
    }
    let photo = jpeg();
    bytes[512..512 + photo.len()].copy_from_slice(&photo);
    fs::write(&image, bytes).unwrap();
    let found = candidates(&events(&image).unwrap());
    assert_eq!(found.len(), 1);
    assert_eq!((found[0].format, found[0].offset), (Format::Jpeg, 512));
}

#[test]
fn interrupted_export_keeps_incomplete_marker() {
    let temp = tempdir().unwrap();
    let image = temp.path().join("card.img");
    fs::write(&image, jpeg()).unwrap();
    let req = request(&events(&image).unwrap());
    assert!(
        recover(&image, temp.path(), &req, |_| Err(io::Error::new(
            io::ErrorKind::Interrupted,
            "cancelled"
        )))
        .is_err()
    );
    let directory = fs::read_dir(temp.path())
        .unwrap()
        .filter_map(Result::ok)
        .find(|e| e.file_type().unwrap().is_dir())
        .unwrap()
        .path();
    assert!(directory.join("INCOMPLETE.txt").exists());
    assert!(!directory.join("manifest.json").exists());
}

#[test]
fn symbolic_source_link_is_rejected() {
    let temp = tempdir().unwrap();
    let target = temp.path().join("card.img");
    fs::write(&target, jpeg()).unwrap();
    let alias = temp.path().join("alias.img");
    std::os::unix::fs::symlink(&target, &alias).unwrap();
    assert!(events(&alias).is_err());
}
