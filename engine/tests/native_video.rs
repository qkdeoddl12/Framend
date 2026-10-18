use recovery_engine::native_video::salvage;
use sha2::{Digest, Sha256};
#[cfg(unix)]
use std::os::unix::fs::symlink;
use std::{fs, io};
use tempfile::TempDir;

fn tempdir() -> TempDir {
    // Safe output creation rejects symlink ancestors, including macOS /var.
    tempfile::tempdir_in(std::env::temp_dir().canonicalize().unwrap()).unwrap()
}

fn atom(kind: &[u8; 4], payload: &[u8]) -> Vec<u8> {
    let mut out = ((payload.len() + 8) as u32).to_be_bytes().to_vec();
    out.extend_from_slice(kind);
    out.extend_from_slice(payload);
    out
}

#[derive(Default)]
struct Bits(Vec<bool>);
impl Bits {
    fn n(&mut self, value: u32, count: u32) {
        self.0
            .extend((0..count).rev().map(|bit| (value >> bit) & 1 != 0));
    }
    fn ue(&mut self, value: u32) {
        let value = value + 1;
        let count = 32 - value.leading_zeros();
        self.n(0, count - 1);
        self.n(value, count);
    }
    fn end(mut self) -> Vec<u8> {
        self.0.push(true);
        while !self.0.len().is_multiple_of(8) {
            self.0.push(false);
        }
        self.0
            .chunks(8)
            .map(|b| b.iter().fold(0, |n, bit| (n << 1) | u8::from(*bit)))
            .collect()
    }
}

fn parameter_sets() -> (Vec<u8>, Vec<u8>) {
    let mut bits = Bits::default();
    bits.ue(0); // SPS id
    bits.ue(0); // log2_max_frame_num_minus4
    bits.ue(0); // pic_order_cnt_type
    bits.ue(0); // log2_max_pic_order_cnt_lsb_minus4
    bits.ue(1); // max_num_ref_frames
    bits.n(0, 1); // gaps_in_frame_num_value_allowed_flag
    bits.ue(3);
    bits.ue(3); // 64 x 64
    bits.n(1, 1);
    bits.n(1, 1);
    bits.n(0, 1);
    bits.n(0, 1);
    let mut sps = vec![0x67, 66, 0, 30];
    sps.extend(bits.end());
    let mut bits = Bits::default();
    bits.ue(0);
    bits.ue(0);
    bits.n(0, 1);
    bits.n(0, 1);
    bits.ue(0);
    bits.ue(0);
    bits.ue(0);
    bits.n(0, 1);
    bits.n(0, 2);
    bits.ue(0);
    bits.ue(0);
    bits.ue(0);
    bits.n(1, 1);
    bits.n(0, 1);
    bits.n(0, 1);
    let mut pps = vec![0x68];
    pps.extend(bits.end());
    (sps, pps)
}

fn reference(external: bool) -> Vec<u8> {
    let (sps, pps) = parameter_sets();
    let mut config = vec![1, 66, 0, 30, 0xff, 0xe1];
    config.extend((sps.len() as u16).to_be_bytes());
    config.extend(sps);
    config.push(1);
    config.extend((pps.len() as u16).to_be_bytes());
    config.extend(pps);
    reference_with_config(external, b"avc1", b"avcC", &config)
}

fn reference_with_config(
    external: bool,
    codec: &[u8; 4],
    kind: &[u8; 4],
    config: &[u8],
) -> Vec<u8> {
    let mut entry = vec![0; 78];
    entry[6..8].copy_from_slice(&1u16.to_be_bytes());
    entry.extend(atom(kind, config));
    let mut stsd = vec![0, 0, 0, 0, 0, 0, 0, 1];
    stsd.extend(atom(codec, &entry));
    let mut stts = vec![0, 0, 0, 0, 0, 0, 0, 1];
    stts.extend(3u32.to_be_bytes());
    stts.extend(1001u32.to_be_bytes());
    let mut stbl = atom(b"stsd", &stsd);
    stbl.extend(atom(b"stts", &stts));
    let mut dref = vec![0, 0, 0, 0, 0, 0, 0, 1];
    dref.extend(atom(b"url ", &[0, 0, 0, u8::from(!external)]));
    let mut minf = atom(b"dinf", &atom(b"dref", &dref));
    minf.extend(atom(b"stbl", &stbl));
    let mut mdhd = vec![0; 24];
    mdhd[12..16].copy_from_slice(&30_000u32.to_be_bytes());
    let mut hdlr = vec![0; 24];
    hdlr[8..12].copy_from_slice(b"vide");
    let mut mdia = atom(b"mdhd", &mdhd);
    mdia.extend(atom(b"hdlr", &hdlr));
    mdia.extend(atom(b"minf", &minf));
    let mut result = atom(b"ftyp", b"isom\0\0\0\0isom");
    result.extend(atom(b"moov", &atom(b"trak", &atom(b"mdia", &mdia))));
    result
}

fn slice(idr: bool, number: u32, first_mb: u32) -> Vec<u8> {
    let mut bits = Bits::default();
    bits.ue(first_mb);
    bits.ue(if idr { 2 } else { 0 });
    bits.ue(0);
    bits.n(number & 15, 4);
    if idr {
        bits.ue(0);
    }
    bits.n((number * 2) & 15, 4);
    let mut nal = vec![if idr { 0x65 } else { 0x41 }];
    nal.extend(bits.end());
    nal.resize(24, 0x55);
    nal
}

fn pack(nals: &[Vec<u8>]) -> Vec<u8> {
    let mut out = Vec::new();
    for nal in nals {
        out.extend((nal.len() as u32).to_be_bytes());
        out.extend(nal);
    }
    out
}

fn run(data: &[u8]) -> (TempDir, recovery_engine::native_video::SalvageReport) {
    let dir = tempdir();
    let input = dir.path().join("input.mov");
    let reference_path = dir.path().join("reference.mov");
    fs::write(&input, data).unwrap();
    fs::write(&reference_path, reference(false)).unwrap();
    let report = salvage(
        &input,
        &reference_path,
        &dir.path().join("video.h264"),
        |_, _| Ok(()),
    )
    .unwrap();
    (dir, report)
}

#[test]
fn segment_map_partitions_output_and_source_extents_at_unknown_gaps() {
    let chain = pack(&[slice(true, 0, 0), slice(false, 1, 0), slice(false, 2, 0)]);
    let mut data = chain.clone();
    data.extend([0xff; 17]);
    data.extend(&chain);
    let (dir, report) = run(&data);
    let json = serde_json::to_value(&report).unwrap();
    let segments = json["segment_map"]
        .as_array()
        .expect("missing segment evidence");
    assert_eq!(segments.len(), 2);
    let output = fs::read(dir.path().join("video.h264")).unwrap();
    let mut cursor = 0;
    for (i, segment) in segments.iter().enumerate() {
        assert_eq!(segment["id"], i + 1);
        assert_eq!(segment["output_offset"], cursor);
        assert_eq!(segment["frames"], 3);
        assert_eq!(segment["extent_start"], i);
        assert_eq!(segment["extent_count"], 1);
        let length = segment["output_length"].as_u64().unwrap() as usize;
        assert!(length > chain.len()); // includes reference parameter sets
        assert_eq!(
            segment["sha256"],
            format!("{:x}", Sha256::digest(&output[cursor..cursor + length]))
        );
        cursor += length;
    }
    assert_eq!(cursor, output.len());
    assert_eq!(segments[0]["end_reason"], "unknown_bytes");
    assert_eq!(segments[1]["end_reason"], "range_end");
    assert_eq!(report.extents[1].offset, (chain.len() + 17) as u64);
}

#[test]
fn segment_map_preserves_one_contiguous_run_across_idr_pictures() {
    let chain = pack(&[slice(true, 0, 0), slice(false, 1, 0), slice(false, 2, 0)]);
    let (_, report) = run(&[chain.clone(), chain].concat());
    let json = serde_json::to_value(report).unwrap();
    assert_eq!(json["segment_map"].as_array().unwrap().len(), 1);
    assert_eq!(json["segment_map"][0]["frames"], 6);
}

#[test]
fn segment_map_splits_distinct_mdat_ranges() {
    let chain = pack(&[slice(true, 0, 0), slice(false, 1, 0), slice(false, 2, 0)]);
    let (_, report) = run(&[atom(b"mdat", &chain), atom(b"mdat", &chain)].concat());
    let json = serde_json::to_value(report).unwrap();
    assert_eq!(json["segment_map"].as_array().unwrap().len(), 2);
    assert_eq!(json["segment_map"][0]["end_reason"], "range_end");
}

#[test]
fn segment_map_closes_run_on_picture_mismatch() {
    let mut nals = vec![slice(true, 0, 0), slice(false, 1, 0), slice(false, 2, 0)];
    nals.push(slice(false, 3, 1)); // continuation belongs to a different picture
    nals.extend([slice(true, 0, 0), slice(false, 1, 0), slice(false, 2, 0)]);
    let (_, report) = run(&pack(&nals));
    let json = serde_json::to_value(report).unwrap();
    assert_eq!(json["segment_map"][0]["end_reason"], "picture_mismatch");
    assert_eq!(json["segment_map"].as_array().unwrap().len(), 2);
}

#[test]
fn segment_limit_fails_explicitly_instead_of_silently_truncating() {
    let dir = tempdir();
    let chain = pack(&[slice(true, 0, 0), slice(false, 1, 0), slice(false, 2, 0)]);
    let mut data = Vec::new();
    for _ in 0..257 {
        data.extend(&chain);
        data.extend([0xff; 9]);
    }
    let input = dir.path().join("input");
    let reference_path = dir.path().join("reference");
    fs::write(&input, data).unwrap();
    fs::write(&reference_path, reference(false)).unwrap();
    let error = salvage(
        &input,
        &reference_path,
        &dir.path().join("output"),
        |_, _| Ok(()),
    )
    .unwrap_err();
    assert!(error.to_string().contains("segment limit"), "{error}");
}

#[test]
fn native_video_salvages_missing_moov_without_changing_nal_payloads() {
    let nals = vec![slice(true, 0, 0), slice(false, 1, 0), slice(false, 2, 0)];
    let data = atom(b"mdat", &pack(&nals));
    let (dir, report) = run(&data);
    assert_eq!(
        (
            report.codec.as_str(),
            report.fps_num,
            report.fps_den,
            report.frames
        ),
        ("h264", 30_000, 1001, 3)
    );
    let (sps, pps) = parameter_sets();
    let mut expected = Vec::new();
    for nal in [sps, pps].iter().chain(nals.iter()) {
        expected.extend([0, 0, 0, 1]);
        expected.extend(nal);
    }
    assert_eq!(fs::read(dir.path().join("video.h264")).unwrap(), expected);
    assert_eq!(fs::read(dir.path().join("input.mov")).unwrap(), data);
    assert_eq!(
        report.extents.iter().map(|e| e.length).sum::<u64>() + report.skipped_bytes,
        data.len() as u64
    );
    assert_ne!(report.input_sha256, report.reference_sha256);
    assert!(report.warnings.iter().any(|w| w.contains("timing")));
}

#[test]
fn native_video_counts_multislice_access_units_once() {
    let mut nals = Vec::new();
    for n in 0..3 {
        nals.push(slice(n == 0, n, 0));
        nals.push(slice(n == 0, n, 8));
    }
    assert_eq!(run(&atom(b"mdat", &pack(&nals))).1.frames, 3);
}

#[test]
fn native_video_drops_non_keyframe_prefix() {
    let nals = vec![
        slice(false, 7, 0),
        slice(true, 0, 0),
        slice(false, 1, 0),
        slice(false, 2, 0),
    ];
    let (_, report) = run(&atom(b"mdat", &pack(&nals)));
    assert_eq!(report.frames, 3);
    assert_eq!(report.extents[0].offset, 36);
}

#[test]
fn native_video_truncated_tail_is_not_reported_as_a_frame() {
    let nals = vec![
        slice(true, 0, 0),
        slice(false, 1, 0),
        slice(false, 2, 0),
        slice(false, 3, 0),
    ];
    let mut payload = pack(&nals);
    payload.truncate(payload.len() - 10);
    assert_eq!(run(&atom(b"mdat", &payload)).1.frames, 3);
}

#[test]
fn native_video_unknown_gap_requires_new_idr() {
    let mut payload = pack(&[slice(true, 0, 0), slice(false, 1, 0), slice(false, 2, 0)]);
    payload.extend([0xa5; 13]);
    payload.extend(pack(&[
        slice(false, 3, 0),
        slice(true, 0, 0),
        slice(false, 1, 0),
        slice(false, 2, 0),
    ]));
    let (_, report) = run(&atom(b"mdat", &payload));
    assert_eq!(report.frames, 6);
}

#[test]
fn native_video_rejects_random_bytes_and_isolated_idr() {
    for payload in [vec![0x55; 100_000], pack(&[slice(true, 0, 0)])] {
        let dir = tempdir();
        let input = dir.path().join("input");
        let refer = dir.path().join("reference");
        fs::write(&input, atom(b"mdat", &payload)).unwrap();
        fs::write(&refer, reference(false)).unwrap();
        assert!(salvage(&input, &refer, &dir.path().join("out"), |_, _| Ok(())).is_err());
    }
}

#[test]
fn native_video_rejects_external_data_references() {
    let dir = tempdir();
    let input = dir.path().join("input");
    let refer = dir.path().join("reference");
    fs::write(
        &input,
        pack(&[slice(true, 0, 0), slice(false, 1, 0), slice(false, 2, 0)]),
    )
    .unwrap();
    fs::write(&refer, reference(true)).unwrap();
    assert!(salvage(&input, &refer, &dir.path().join("out"), |_, _| Ok(())).is_err());
}

#[test]
fn native_video_rejects_identical_inode_and_output_collision() {
    let dir = tempdir();
    let input = dir.path().join("input");
    let refer = dir.path().join("reference");
    fs::write(&input, reference(false)).unwrap();
    fs::hard_link(&input, &refer).unwrap();
    assert!(salvage(&input, &refer, &dir.path().join("out"), |_, _| Ok(())).is_err());
    fs::remove_file(&refer).unwrap();
    fs::write(&refer, reference(false)).unwrap();
    fs::write(
        &input,
        pack(&[slice(true, 0, 0), slice(false, 1, 0), slice(false, 2, 0)]),
    )
    .unwrap();
    let out = dir.path().join("out");
    fs::write(&out, b"preserve").unwrap();
    assert!(salvage(&input, &refer, &out, |_, _| Ok(())).is_err());
    assert_eq!(fs::read(&out).unwrap(), b"preserve");
}

#[test]
fn native_video_rejects_symlinks_and_honors_cancellation() {
    let dir = tempdir();
    let input = dir.path().join("input");
    let refer = dir.path().join("reference");
    fs::write(
        &input,
        pack(&[slice(true, 0, 0), slice(false, 1, 0), slice(false, 2, 0)]),
    )
    .unwrap();
    fs::write(&refer, reference(false)).unwrap();
    #[cfg(unix)]
    {
        let link = dir.path().join("link");
        symlink(&input, &link).unwrap();
        assert!(salvage(&link, &refer, &dir.path().join("out"), |_, _| Ok(())).is_err());
    }
    let result = salvage(&input, &refer, &dir.path().join("cancelled"), |_, _| {
        Err(io::Error::new(io::ErrorKind::Interrupted, "cancelled"))
    });
    assert_eq!(result.unwrap_err().kind(), io::ErrorKind::Interrupted);
}

fn escaped(data: Vec<u8>) -> Vec<u8> {
    let mut out = Vec::new();
    let mut zeroes = 0;
    for b in data {
        if zeroes >= 2 && b <= 3 {
            out.push(3);
            zeroes = 0;
        }
        out.push(b);
        zeroes = if b == 0 { zeroes + 1 } else { 0 };
    }
    out
}
fn hevc_config() -> (Vec<u8>, Vec<Vec<u8>>) {
    let vps = vec![0x40, 1, 0x0c, 0x01, 0xff, 0xff];
    let mut bits = Bits::default();
    bits.n(0, 4);
    bits.n(0, 3);
    bits.n(1, 1);
    bits.n(1, 8);
    bits.n(0, 32);
    bits.n(0, 32);
    bits.n(0, 16);
    bits.n(30, 8);
    bits.ue(0);
    bits.ue(1);
    bits.ue(64);
    bits.ue(64);
    bits.n(0, 1);
    bits.ue(0);
    bits.ue(0);
    bits.ue(0);
    bits.n(0, 1);
    bits.ue(4);
    bits.ue(0);
    bits.ue(0);
    bits.ue(0);
    bits.ue(2);
    let mut sps = vec![0x42, 1];
    sps.extend(escaped(bits.end()));
    let mut bits = Bits::default();
    bits.ue(0);
    bits.ue(0);
    bits.n(0, 1);
    bits.n(0, 1);
    bits.n(0, 3);
    let mut pps = vec![0x44, 1];
    pps.extend(bits.end());
    let parameters = vec![vps, sps, pps];
    let mut config = vec![0; 23];
    config[0] = 1;
    config[21] = 3;
    config[22] = 3;
    for (kind, nal) in [32, 33, 34].iter().zip(&parameters) {
        config.push(*kind);
        config.extend(1u16.to_be_bytes());
        config.extend((nal.len() as u16).to_be_bytes());
        config.extend(nal);
    }
    (config, parameters)
}
fn hevc_slice(kind: u8, number: u32, first: bool) -> Vec<u8> {
    let mut bits = Bits::default();
    bits.n(u32::from(first), 1);
    if (16..=21).contains(&kind) {
        bits.n(0, 1);
    }
    bits.ue(0);
    if !first {
        bits.n(1, 2);
    }
    bits.ue(if (16..=21).contains(&kind) { 2 } else { 1 });
    if ![19, 20].contains(&kind) {
        bits.n(number, 4);
    }
    let mut nal = vec![kind << 1, 1];
    nal.extend(bits.end());
    nal.resize(24, 0x55);
    nal
}

#[test]
fn native_video_hevc_retains_idr_chain_and_counts_multislice_pictures() {
    let dir = tempdir();
    let input = dir.path().join("in");
    let refer = dir.path().join("ref");
    let out = dir.path().join("out");
    let (config, params) = hevc_config();
    fs::write(
        &refer,
        reference_with_config(false, b"hvc1", b"hvcC", &config),
    )
    .unwrap();
    let mut nals = Vec::new();
    for i in 0..3 {
        for first in [true, false] {
            nals.push(hevc_slice(if i == 0 { 20 } else { 1 }, i, first));
        }
    }
    fs::write(&input, atom(b"mdat", &pack(&nals))).unwrap();
    let report = salvage(&input, &refer, &out, |_, _| Ok(())).unwrap();
    assert_eq!((report.codec.as_str(), report.frames), ("hevc", 3));
    let mut expected = Vec::new();
    for nal in params.iter().chain(nals.iter()) {
        expected.extend([0, 0, 0, 1]);
        expected.extend(nal);
    }
    assert_eq!(fs::read(out).unwrap(), expected);
}

#[test]
fn native_video_hevc_cra_is_not_a_safe_restart() {
    let dir = tempdir();
    let input = dir.path().join("in");
    let refer = dir.path().join("ref");
    let (config, _) = hevc_config();
    fs::write(
        &refer,
        reference_with_config(false, b"hvc1", b"hvcC", &config),
    )
    .unwrap();
    fs::write(
        &input,
        pack(&[
            hevc_slice(21, 0, true),
            hevc_slice(1, 1, true),
            hevc_slice(1, 2, true),
        ]),
    )
    .unwrap();
    assert!(salvage(&input, &refer, &dir.path().join("out"), |_, _| Ok(())).is_err());
}

#[test]
fn native_video_rejects_conflicting_inband_parameter_sets() {
    let dir = tempdir();
    let input = dir.path().join("in");
    let refer = dir.path().join("ref");
    let (mut sps, _) = parameter_sets();
    sps[3] = 31;
    fs::write(
        &input,
        atom(
            b"mdat",
            &pack(&[
                sps,
                slice(true, 0, 0),
                slice(false, 1, 0),
                slice(false, 2, 0),
            ]),
        ),
    )
    .unwrap();
    fs::write(&refer, reference(false)).unwrap();
    let error = salvage(&input, &refer, &dir.path().join("out"), |_, _| Ok(())).unwrap_err();
    assert!(error.to_string().contains("parameter"));
}

#[test]
fn native_video_ignores_unconfirmed_parameter_like_bytes_in_unknown_gap() {
    let (mut sps, _) = parameter_sets();
    sps[3] = 31;
    let mut bytes = vec![0xa5; 13];
    bytes.extend(pack(&[sps]));
    bytes.extend([0xa5; 13]);
    bytes.extend(pack(&[
        slice(true, 0, 0),
        slice(false, 1, 0),
        slice(false, 2, 0),
    ]));
    let (_, report) = run(&atom(b"mdat", &bytes));
    assert_eq!(report.frames, 3);
}

#[test]
fn native_video_preserves_avc_emulation_prevention_crossing_header_window() {
    let mut idr = slice(true, 0, 0);
    idr.resize(140, 0x55);
    idr[125..129].copy_from_slice(&[0, 0, 3, 0]);
    let (_, report) = run(&atom(
        b"mdat",
        &pack(&[idr, slice(false, 1, 0), slice(false, 2, 0)]),
    ));
    assert_eq!(report.frames, 3);
}

#[test]
fn native_video_preserves_hevc_emulation_prevention_crossing_header_window() {
    let dir = tempdir();
    let input = dir.path().join("in");
    let refer = dir.path().join("ref");
    let (config, _) = hevc_config();
    fs::write(
        &refer,
        reference_with_config(false, b"hvc1", b"hvcC", &config),
    )
    .unwrap();
    let mut idr = hevc_slice(20, 0, true);
    idr.resize(140, 0x55);
    idr[125..129].copy_from_slice(&[0, 0, 3, 0]);
    fs::write(
        &input,
        pack(&[idr, hevc_slice(1, 1, true), hevc_slice(1, 2, true)]),
    )
    .unwrap();
    let report = salvage(&input, &refer, &dir.path().join("out"), |_, _| Ok(())).unwrap();
    assert_eq!(report.frames, 3);
}

#[test]
#[cfg(unix)]
fn native_video_rejects_output_parent_symlinks() {
    let dir = tempdir();
    let outside = tempdir();
    let input = dir.path().join("in");
    let refer = dir.path().join("ref");
    fs::write(
        &input,
        pack(&[slice(true, 0, 0), slice(false, 1, 0), slice(false, 2, 0)]),
    )
    .unwrap();
    fs::write(&refer, reference(false)).unwrap();
    let link = dir.path().join("redirect");
    symlink(outside.path(), &link).unwrap();
    assert!(salvage(&input, &refer, &link.join("out"), |_, _| Ok(())).is_err());
    assert!(!outside.path().join("out").exists());
}

#[test]
fn native_video_detects_source_change_during_final_progress() {
    let dir = tempdir();
    let input = dir.path().join("in");
    let refer = dir.path().join("ref");
    let original = pack(&[slice(true, 0, 0), slice(false, 1, 0), slice(false, 2, 0)]);
    fs::write(&input, &original).unwrap();
    fs::write(&refer, reference(false)).unwrap();
    let result = salvage(&input, &refer, &dir.path().join("out"), |done, total| {
        if done == total {
            fs::write(&input, b"changed during final callback")?;
        }
        Ok(())
    });
    assert!(result.is_err());
}

#[test]
fn native_video_detects_output_replacement_during_final_progress() {
    let dir = tempdir();
    let input = dir.path().join("in");
    let refer = dir.path().join("ref");
    let output = dir.path().join("out");
    fs::write(
        &input,
        pack(&[slice(true, 0, 0), slice(false, 1, 0), slice(false, 2, 0)]),
    )
    .unwrap();
    fs::write(&refer, reference(false)).unwrap();
    let mut replaced = false;
    let result = salvage(&input, &refer, &output, |done, total| {
        if done == total {
            fs::rename(&output, dir.path().join("retained-evidence"))?;
            fs::write(&output, b"replacement")?;
            replaced = true;
        }
        Ok(())
    });
    assert!(replaced);
    assert!(result.is_err());
    assert_eq!(fs::read(&output).unwrap(), b"replacement");
    assert!(dir.path().join("retained-evidence").exists());
}

#[test]
fn native_video_rejects_invalid_and_overflowing_reference_boxes() {
    for bytes in [
        vec![],
        vec![0xff; 16],
        [
            1u32.to_be_bytes().as_slice(),
            b"moov",
            u64::MAX.to_be_bytes().as_slice(),
        ]
        .concat(),
    ] {
        let dir = tempdir();
        let input = dir.path().join("in");
        let refer = dir.path().join("ref");
        fs::write(
            &input,
            pack(&[slice(true, 0, 0), slice(false, 1, 0), slice(false, 2, 0)]),
        )
        .unwrap();
        fs::write(&refer, bytes).unwrap();
        assert!(salvage(&input, &refer, &dir.path().join("out"), |_, _| Ok(())).is_err());
    }
}

#[test]
fn native_video_rejects_random_payloads_with_plausible_headers() {
    let mut state = 7u32;
    let mut bytes = Vec::new();
    for _ in 0..1000 {
        let mut nal = vec![0x65];
        for _ in 0..31 {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            nal.push(state as u8);
        }
        bytes.extend(pack(&[nal]));
    }
    let dir = tempdir();
    let input = dir.path().join("in");
    let refer = dir.path().join("ref");
    fs::write(&input, atom(b"mdat", &bytes)).unwrap();
    fs::write(&refer, reference(false)).unwrap();
    assert!(salvage(&input, &refer, &dir.path().join("out"), |_, _| Ok(())).is_err());
}
