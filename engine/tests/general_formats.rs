use recovery_engine::{Candidate, Event, RecoveryRequest, recover, scan};
use sha2::{Digest, Sha256};
use std::fs;

fn crc(bytes: &[u8]) -> u32 {
    bytes.iter().fold(u32::MAX, |mut crc, &byte| {
        crc ^= u32::from(byte);
        for _ in 0..8 {
            crc = (crc >> 1) ^ (0xedb88320 & 0u32.wrapping_sub(crc & 1));
        }
        crc
    }) ^ u32::MAX
}
fn chunk(kind: &[u8; 4], bytes: &[u8]) -> Vec<u8> {
    let mut result = (bytes.len() as u32).to_be_bytes().to_vec();
    result.extend(kind);
    result.extend(bytes);
    result.extend(crc(&result[4..]).to_be_bytes());
    result
}
fn png_with(chunks: Vec<Vec<u8>>) -> Vec<u8> {
    [b"\x89PNG\r\n\x1a\n".to_vec(), chunks.concat()].concat()
}
fn ihdr() -> Vec<u8> {
    chunk(b"IHDR", &[0, 0, 0, 3, 0, 0, 0, 2, 8, 2, 0, 0, 0])
}
fn png() -> Vec<u8> {
    // Deliberately not a decodable zlib stream: the scanner promises structure/CRC only.
    png_with(vec![
        ihdr(),
        chunk(b"IDAT", &[1, 2, 3]),
        chunk(b"IEND", &[]),
    ])
}
fn riff_chunk(kind: &[u8; 4], payload: &[u8]) -> Vec<u8> {
    let mut result = kind.to_vec();
    result.extend((payload.len() as u32).to_le_bytes());
    result.extend(payload);
    if payload.len() % 2 == 1 {
        result.push(0);
    }
    result
}
fn wav_with(chunks: Vec<Vec<u8>>) -> Vec<u8> {
    let payload = [b"WAVE".to_vec(), chunks.concat()].concat();
    [
        b"RIFF".to_vec(),
        (payload.len() as u32).to_le_bytes().to_vec(),
        payload,
    ]
    .concat()
}
fn fmt() -> Vec<u8> {
    riff_chunk(
        b"fmt ",
        &[1, 0, 1, 0, 0x40, 0x1f, 0, 0, 0x40, 0x1f, 0, 0, 1, 0, 8, 0],
    )
}
fn wav() -> Vec<u8> {
    wav_with(vec![
        riff_chunk(b"JUNK", &[9]),
        fmt(),
        riff_chunk(b"data", &[1, 2, 3]),
    ])
}
fn found(bytes: &[u8]) -> Vec<Candidate> {
    let temp = tempfile::tempdir().unwrap();
    let image = temp.path().join("image");
    fs::write(&image, bytes).unwrap();
    let mut candidates = Vec::new();
    scan(&image, |event| {
        if let Event::Candidate { candidate } = event {
            candidates.push(candidate);
        }
        Ok(())
    })
    .unwrap();
    candidates
}
#[test]
fn png_and_wave_scan_across_chunk_boundary_and_export_exact_hashes() {
    let temp = tempfile::tempdir_in(std::env::temp_dir().canonicalize().unwrap()).unwrap();
    let image = temp.path().join("image");
    let bytes = [vec![0; 4 * 1024 * 1024 - 4], png(), wav()].concat();
    fs::write(&image, &bytes).unwrap();
    let mut source = None;
    let mut candidates = Vec::new();
    scan(&image, |e| {
        match e {
            Event::Started { source: s } => source = Some(s),
            Event::Candidate { candidate } => candidates.push(candidate),
            _ => {}
        }
        Ok(())
    })
    .unwrap();
    assert_eq!(candidates.len(), 2);
    assert_eq!(serde_json::to_value(candidates[0].format).unwrap(), "png");
    assert_eq!(
        (candidates[0].width, candidates[0].height),
        (Some(3), Some(2))
    );
    assert_eq!(serde_json::to_value(candidates[1].format).unwrap(), "wav");
    let request = RecoveryRequest {
        source: source.unwrap(),
        candidates,
    };
    let mut saved = 0;
    recover(&image, temp.path(), &request, |event| {
        if let Event::Saved {
            candidate,
            path,
            sha256,
        } = event
        {
            let expected =
                &bytes[candidate.offset as usize..(candidate.offset + candidate.length) as usize];
            assert_eq!(fs::read(&path).unwrap(), expected);
            assert_eq!(sha256, format!("{:x}", Sha256::digest(expected)));
            assert!(path.ends_with(if saved == 0 { ".png" } else { ".wav" }));
            saved += 1;
        }
        Ok(())
    })
    .unwrap();
    assert_eq!(saved, 2);
}
#[test]
fn png_rejects_crc_damage_truncation_and_unbounded_chunks() {
    let valid = png();
    for end in 8..valid.len() {
        assert!(found(&valid[..end]).is_empty(), "truncation {end}");
    }
    let mut bad = valid.clone();
    bad[45] ^= 1;
    assert!(found(&bad).is_empty());
    let huge = png_with(vec![
        ihdr(),
        [u32::MAX.to_be_bytes().to_vec(), b"IDAT".to_vec()].concat(),
    ]);
    assert!(found(&huge).is_empty());
}
#[test]
fn png_rejects_bad_critical_order_and_dimensions() {
    let data = chunk(b"IDAT", &[1]);
    let end = chunk(b"IEND", &[]);
    for chunks in [
        vec![data.clone(), ihdr(), end.clone()],
        vec![ihdr(), ihdr(), data.clone(), end.clone()],
        vec![
            ihdr(),
            data.clone(),
            chunk(b"tEXt", b"x"),
            data.clone(),
            end.clone(),
        ],
        vec![ihdr(), chunk(b"ABCD", &[]), data.clone(), end.clone()],
        vec![ihdr(), end.clone()],
        vec![chunk(b"IHDR", &[0; 13]), data.clone(), end],
    ] {
        assert!(found(&png_with(chunks)).is_empty());
    }
}
#[test]
fn wave_rejects_truncation_bad_riff_bounds_missing_padding_and_bad_chunk_order() {
    let valid = wav();
    for end in 12..valid.len() {
        assert!(found(&valid[..end]).is_empty(), "truncation {end}");
    }
    for declared in [0, 3, u32::MAX] {
        let mut bad = valid.clone();
        bad[4..8].copy_from_slice(&declared.to_le_bytes());
        assert!(found(&bad).is_empty());
    }
    let mut unpadded = riff_chunk(b"JUNK", &[9]);
    unpadded.pop();
    for chunks in [
        vec![unpadded, fmt(), riff_chunk(b"data", &[1])],
        vec![riff_chunk(b"data", &[1]), fmt()],
        vec![fmt()],
        vec![fmt(), fmt(), riff_chunk(b"data", &[1])],
        vec![riff_chunk(b"fmt ", &[0; 16]), riff_chunk(b"data", &[1])],
    ] {
        assert!(found(&wav_with(chunks)).is_empty());
    }
}

#[test]
fn png_checks_crc_across_stream_buffers_and_palette_ordering() {
    let payload = vec![42; 128 * 1024 + 3];
    let bytes = png_with(vec![ihdr(), chunk(b"IDAT", &payload), chunk(b"IEND", &[])]);
    assert_eq!(found(&bytes).len(), 1);
    let mut damaged = bytes;
    damaged[70_000] ^= 1;
    assert!(found(&damaged).is_empty());
    let indexed = chunk(b"IHDR", &[0, 0, 0, 1, 0, 0, 0, 1, 1, 3, 0, 0, 0]);
    assert!(
        found(&png_with(vec![
            indexed.clone(),
            chunk(b"IDAT", &[1]),
            chunk(b"IEND", &[])
        ]))
        .is_empty()
    );
    assert_eq!(
        found(&png_with(vec![
            indexed,
            chunk(b"PLTE", &[0, 0, 0]),
            chunk(b"IDAT", &[1]),
            chunk(b"IEND", &[])
        ]))
        .len(),
        1
    );
    for parts in [
        vec![
            ihdr(),
            chunk(b"IDAT", &[1]),
            chunk(b"tRNS", &[0; 6]),
            chunk(b"IEND", &[]),
        ],
        vec![
            ihdr(),
            chunk(b"gAMA", &[0, 0, 1, 0]),
            chunk(b"gAMA", &[0, 0, 1, 0]),
            chunk(b"IDAT", &[1]),
            chunk(b"IEND", &[]),
        ],
        vec![
            ihdr(),
            chunk(b"PLTE", &[0, 0, 0]),
            chunk(b"gAMA", &[0, 0, 1, 0]),
            chunk(b"IDAT", &[1]),
            chunk(b"IEND", &[]),
        ],
    ] {
        assert!(found(&png_with(parts)).is_empty());
    }
}
#[test]
fn wave_rejects_misaligned_pcm_data_and_bad_extension_size() {
    let stereo = riff_chunk(
        b"fmt ",
        &[1, 0, 2, 0, 0x40, 0x1f, 0, 0, 0, 0x7d, 0, 0, 4, 0, 16, 0],
    );
    assert!(
        found(&wav_with(vec![
            stereo.clone(),
            riff_chunk(b"data", &[0; 3])
        ]))
        .is_empty()
    );
    assert_eq!(
        found(&wav_with(vec![stereo, riff_chunk(b"data", &[0; 8])])).len(),
        1
    );
    let wrong_extension = riff_chunk(
        b"fmt ",
        &[
            1, 0, 1, 0, 0x40, 0x1f, 0, 0, 0x40, 0x1f, 0, 0, 1, 0, 8, 0, 1, 0,
        ],
    );
    assert!(found(&wav_with(vec![wrong_extension, riff_chunk(b"data", &[0])])).is_empty());
}

#[test]
fn png_accepts_independently_encoded_pixel_fixture() {
    // One grey pixel, encoded by Python's zlib/CRC implementation, independently of fixture helpers.
    let bytes = [
        137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 13, 73, 72, 68, 82, 0, 0, 0, 1, 0, 0, 0, 1, 8, 0,
        0, 0, 0, 58, 126, 155, 85, 0, 0, 0, 10, 73, 68, 65, 84, 120, 156, 99, 168, 7, 0, 0, 129, 0,
        128, 211, 148, 83, 74, 0, 0, 0, 0, 73, 69, 78, 68, 174, 66, 96, 130,
    ];
    let candidates = found(&bytes);
    assert_eq!(candidates.len(), 1);
    assert_eq!(
        (
            candidates[0].length,
            candidates[0].width,
            candidates[0].height
        ),
        (67, Some(1), Some(1))
    );
}
#[test]
fn chunk_count_limits_reject_unbounded_png_and_wave_metadata() {
    let mut png_chunks = vec![ihdr()];
    png_chunks.extend((0..65_536).map(|_| chunk(b"tEXt", b"k\0v")));
    png_chunks.extend([chunk(b"IDAT", &[1]), chunk(b"IEND", &[])]);
    assert!(found(&png_with(png_chunks)).is_empty());
    let mut wave_chunks: Vec<_> = (0..65_536).map(|_| riff_chunk(b"JUNK", &[])).collect();
    wave_chunks.extend([fmt(), riff_chunk(b"data", &[1])]);
    assert!(found(&wav_with(wave_chunks)).is_empty());
}

#[test]
fn png_rejects_palette_after_dependent_metadata_and_unsupported_animation() {
    for parts in [
        vec![
            ihdr(),
            chunk(b"tRNS", &[0; 6]),
            chunk(b"PLTE", &[0, 0, 0]),
            chunk(b"IDAT", &[1]),
            chunk(b"IEND", &[]),
        ],
        vec![
            ihdr(),
            chunk(b"IDAT", &[1]),
            chunk(b"sPLT", b"x"),
            chunk(b"IEND", &[]),
        ],
        vec![
            ihdr(),
            chunk(b"acTL", &[0, 0, 0, 1, 0, 0, 0, 0]),
            chunk(b"IDAT", &[1]),
            chunk(b"IEND", &[]),
        ],
    ] {
        assert!(found(&png_with(parts)).is_empty());
    }
}
