use recovery_engine::exfat::{analyze, extract};
use sha2::{Digest, Sha256};
use std::{
    fs, io,
    os::unix::fs::{FileExt, symlink},
    path::Path,
};
use tempfile::TempDir;

fn tempdir() -> TempDir {
    // macOS /var is a symlink; safe output tests use the physical temp path.
    tempfile::tempdir_in(std::env::temp_dir().canonicalize().unwrap()).unwrap()
}

const SECTOR: usize = 512;
const FAT: usize = 24 * SECTOR;
const HEAP: usize = 40 * SECTOR;
const CLUSTERS: u32 = 2048 - 40;

fn put16(bytes: &mut [u8], at: usize, value: u16) {
    bytes[at..at + 2].copy_from_slice(&value.to_le_bytes());
}
fn put32(bytes: &mut [u8], at: usize, value: u32) {
    bytes[at..at + 4].copy_from_slice(&value.to_le_bytes());
}
fn put64(bytes: &mut [u8], at: usize, value: u64) {
    bytes[at..at + 8].copy_from_slice(&value.to_le_bytes());
}
fn cluster(index: u32) -> usize {
    HEAP + (index as usize - 2) * SECTOR
}
fn fat(image: &mut [u8], index: u32, next: u32) {
    put32(image, FAT + index as usize * 4, next);
}
fn allocated(image: &mut [u8], index: u32, value: bool) {
    let byte = &mut image[cluster(3) + (index as usize - 2) / 8];
    let mask = 1 << ((index - 2) % 8);
    if value {
        *byte |= mask;
    } else {
        *byte &= !mask;
    }
}
fn boot_checksum(image: &mut [u8], offset: usize) {
    let mut sum = 0u32;
    for (index, &byte) in image[offset..offset + 11 * SECTOR].iter().enumerate() {
        if ![106, 107, 112].contains(&index) {
            sum = sum.rotate_right(1).wrapping_add(u32::from(byte));
        }
    }
    for index in (offset + 11 * SECTOR..offset + 12 * SECTOR).step_by(4) {
        put32(image, index, sum);
    }
}
fn both_boots(image: &mut [u8]) {
    boot_checksum(image, 0);
    image.copy_within(0..12 * SECTOR, 12 * SECTOR);
}
fn volume() -> Vec<u8> {
    let mut image = vec![0; 1024 * 1024];
    image[..3].copy_from_slice(&[0xeb, 0x76, 0x90]);
    image[3..11].copy_from_slice(b"EXFAT   ");
    put64(&mut image, 72, 2048);
    put32(&mut image, 80, 24);
    put32(&mut image, 84, 16);
    put32(&mut image, 88, 40);
    put32(&mut image, 92, CLUSTERS);
    put32(&mut image, 96, 2);
    put16(&mut image, 104, 0x100);
    image[108] = 9;
    image[110] = 1;
    image[112] = 0xff;
    put16(&mut image, 510, 0xaa55);
    for index in 1..=8 {
        put32(&mut image, (index + 1) * SECTOR - 4, 0xaa550000);
    }
    both_boots(&mut image);
    fat(&mut image, 0, 0xfffffff8);
    fat(&mut image, 1, u32::MAX);
    fat(&mut image, 2, u32::MAX);
    fat(&mut image, 3, u32::MAX);
    image[cluster(2)] = 0x81;
    put32(&mut image, cluster(2) + 20, 3);
    put64(&mut image, cluster(2) + 24, u64::from(CLUSTERS).div_ceil(8));
    allocated(&mut image, 2, true);
    allocated(&mut image, 3, true);
    image
}
fn set_checksum(entries: &mut [u8]) {
    let sum = entries
        .iter()
        .enumerate()
        .fold(0u16, |sum, (index, &byte)| {
            if index == 2 || index == 3 {
                sum
            } else {
                sum.rotate_right(1).wrapping_add(u16::from(byte))
            }
        });
    put16(entries, 2, sum);
}
fn entry(name: &str, start: u32, length: u64, contiguous: bool, deleted: bool) -> Vec<u8> {
    let name: Vec<_> = name.encode_utf16().collect();
    let mut entries = vec![0; (2 + name.len().div_ceil(15)) * 32];
    entries[0] = 0x85;
    entries[1] = (entries.len() / 32 - 1) as u8;
    put16(&mut entries, 4, 0x20);
    entries[32] = 0xc0;
    entries[33] = if contiguous { 3 } else { 1 };
    entries[35] = name.len() as u8;
    put64(&mut entries, 40, length);
    put32(&mut entries, 52, start);
    put64(&mut entries, 56, length);
    for (index, chars) in name.chunks(15).enumerate() {
        let at = (index + 2) * 32;
        entries[at] = 0xc1;
        for (index, &character) in chars.iter().enumerate() {
            put16(&mut entries, at + 2 + index * 2, character);
        }
    }
    set_checksum(&mut entries);
    if deleted {
        for entry in entries.chunks_exact_mut(32) {
            entry[0] &= 0x7f;
        }
    }
    entries
}
fn write_entry(image: &mut [u8], at: usize, entries: &[u8]) {
    image[at..at + entries.len()].copy_from_slice(entries);
}
fn media(length: usize) -> Vec<u8> {
    let mut bytes: Vec<_> = (0..length).map(|index| (index % 251) as u8).collect();
    bytes[..24].copy_from_slice(b"\0\0\0\x18ftypqt  \0\0\0\0qt  isom");
    bytes[24..28].copy_from_slice(&((length - 24) as u32).to_be_bytes());
    bytes[28..32].copy_from_slice(b"mdat");
    bytes
}
fn with_video(contiguous: bool, deleted: bool) -> (Vec<u8>, Vec<u8>) {
    let mut image = volume();
    let video = media(1200);
    let clusters = if contiguous { [8, 9, 10] } else { [8, 15, 6] };
    for (index, bytes) in video.chunks(SECTOR).enumerate() {
        let at = cluster(clusters[index]);
        image[at..at + bytes.len()].copy_from_slice(bytes);
        allocated(&mut image, clusters[index], !deleted);
        fat(
            &mut image,
            clusters[index],
            clusters.get(index + 1).copied().unwrap_or(u32::MAX),
        );
    }
    write_entry(
        &mut image,
        cluster(2) + 32,
        &entry("촬영 clip.MOV", 8, video.len() as u64, contiguous, deleted),
    );
    (image, video)
}
fn save(directory: &Path, image: &[u8]) -> std::path::PathBuf {
    let path = directory.join("card.img");
    fs::write(&path, image).unwrap();
    path
}

#[test]
fn fragmented_active_video_extracts_in_logical_order_with_exact_hash() {
    let temp = tempdir();
    let (image, video) = with_video(false, false);
    let path = save(temp.path(), &image);
    let scan = analyze(&path).unwrap();
    let candidate = &scan.candidates[0];
    assert_eq!(candidate.name, "촬영 clip.MOV");
    assert!(candidate.recoverable, "{:?}", candidate.reason);
    assert_eq!(candidate.extents.len(), 3);
    assert_eq!(candidate.allocated_clusters, 3);
    let output = temp.path().join("recovered.mov");
    let mut progress = Vec::new();
    let hash = extract(&path, &output, &scan.source, candidate, |done, total| {
        progress.push((done, total));
        Ok(())
    })
    .unwrap();
    assert_eq!(fs::read(output).unwrap(), video);
    assert_eq!(hash, format!("{:x}", Sha256::digest(&video)));
    assert_eq!(progress.last(), Some(&(1200, 1200)));
    assert_eq!(fs::read(path).unwrap(), image);
}

#[test]
fn deleted_contiguous_video_restores_checksum_and_ignores_stale_fat() {
    let temp = tempdir();
    let (mut image, video) = with_video(true, true);
    fat(&mut image, 8, 0);
    let path = save(temp.path(), &image);
    let scan = analyze(&path).unwrap();
    let candidate = &scan.candidates[0];
    assert!(
        candidate.deleted && candidate.recoverable,
        "{:?}",
        candidate
    );
    assert_eq!(candidate.extents.len(), 1);
    assert_eq!(candidate.allocated_clusters, 0);
    let output = temp.path().join("deleted.mov");
    extract(&path, &output, &scan.source, candidate, |_, _| Ok(())).unwrap();
    assert_eq!(fs::read(output).unwrap(), video);
}

#[test]
fn reallocated_deleted_cluster_is_reported_and_refused() {
    let temp = tempdir();
    let (mut image, _) = with_video(true, true);
    allocated(&mut image, 9, true);
    let path = save(temp.path(), &image);
    let scan = analyze(&path).unwrap();
    let candidate = &scan.candidates[0];
    assert!(!candidate.recoverable);
    assert_eq!(candidate.allocated_clusters, 1);
    assert!(candidate.reason.as_deref().unwrap().contains("allocated"));
    assert!(
        extract(
            &path,
            &temp.path().join("out.mov"),
            &scan.source,
            candidate,
            |_, _| Ok(())
        )
        .is_err()
    );
}

#[test]
fn active_cluster_missing_from_bitmap_is_not_recoverable() {
    let temp = tempdir();
    let (mut image, _) = with_video(false, false);
    allocated(&mut image, 15, false);
    let scan = analyze(&save(temp.path(), &image)).unwrap();
    assert!(!scan.candidates[0].recoverable);
}

#[test]
fn checksum_or_mixed_in_use_entries_do_not_become_candidates() {
    for mixed in [false, true] {
        let temp = tempdir();
        let (mut image, _) = with_video(true, true);
        if mixed {
            image[cluster(2) + 64] |= 0x80;
        } else {
            image[cluster(2) + 34] ^= 0x42;
        }
        let scan = analyze(&save(temp.path(), &image)).unwrap();
        assert!(scan.candidates.is_empty());
        assert!(!scan.warnings.is_empty());
    }
}

#[test]
fn backup_boot_recovers_even_when_primary_sector_shift_is_corrupt() {
    let temp = tempdir();
    let (mut image, _) = with_video(true, false);
    image[108] = 63;
    let scan = analyze(&save(temp.path(), &image)).unwrap();
    assert!(scan.candidates[0].recoverable);
    assert!(
        scan.warnings
            .iter()
            .any(|warning| warning.contains("backup"))
    );
}

#[test]
fn corrupt_boot_checksums_and_invalid_geometry_are_rejected() {
    for case in 0..5 {
        let temp = tempdir();
        let mut image = volume();
        match case {
            0 => {
                image[11 * SECTOR] ^= 1;
                image[23 * SECTOR] ^= 1;
            }
            1 => {
                put32(&mut image, 84, 1);
                both_boots(&mut image);
            }
            2 => {
                put32(&mut image, 88, u32::MAX);
                both_boots(&mut image);
            }
            3 => {
                put64(&mut image, 72, u64::MAX);
                both_boots(&mut image);
            }
            _ => {
                image[109] = 32;
                both_boots(&mut image);
            }
        }
        assert!(analyze(&save(temp.path(), &image)).is_err(), "case {case}");
    }
}

#[test]
fn cyclic_short_out_of_range_and_bad_terminator_chains_are_refused() {
    for next in [8, u32::MAX, CLUSTERS + 2, 0xfffffff7, 0xfffffff8, 0] {
        let temp = tempdir();
        let (mut image, _) = with_video(false, false);
        fat(&mut image, 15, next);
        let scan = analyze(&save(temp.path(), &image)).unwrap();
        assert!(!scan.candidates[0].recoverable, "next {next:x}");
        assert!(scan.candidates[0].reason.is_some());
    }
}

#[test]
fn valid_length_and_container_header_are_checked() {
    for bad_header in [false, true] {
        let temp = tempdir();
        let (mut image, _) = with_video(true, false);
        if bad_header {
            image[cluster(8)..cluster(8) + 32].fill(0x55);
        } else {
            let at = cluster(2) + 32;
            put64(&mut image, at + 40, 700);
            set_checksum(&mut image[at..at + 96]);
        }
        let scan = analyze(&save(temp.path(), &image)).unwrap();
        assert!(!scan.candidates[0].recoverable);
    }
}

#[test]
fn nested_directory_is_followed_and_directory_cycle_is_bounded() {
    let temp = tempdir();
    let (mut image, _) = with_video(true, false);
    let file = image[cluster(2) + 32..cluster(2) + 128].to_vec();
    let mut directory = entry("DCIM", 4, 512, false, false);
    put16(&mut directory, 4, 0x10);
    set_checksum(&mut directory);
    write_entry(&mut image, cluster(2) + 32, &directory);
    write_entry(&mut image, cluster(4), &file);
    write_entry(&mut image, cluster(4) + 96, &directory);
    fat(&mut image, 4, u32::MAX);
    allocated(&mut image, 4, true);
    let scan = analyze(&save(temp.path(), &image)).unwrap();
    assert_eq!(scan.candidates.len(), 1);
    assert_eq!(scan.candidates[0].name, "DCIM/촬영 clip.MOV");
    assert!(
        scan.warnings
            .iter()
            .any(|warning| warning.contains("directory"))
    );
}

#[test]
fn mbr_partition_offsets_are_applied_and_gpt_is_explicitly_unsupported() {
    let temp = tempdir();
    let (mut volume, _) = with_video(true, false);
    put64(&mut volume, 64, 2048);
    both_boots(&mut volume);
    let mut disk = vec![0; 1024 * 1024];
    disk.extend_from_slice(&volume);
    put16(&mut disk, 510, 0xaa55);
    disk[450] = 7;
    put32(&mut disk, 454, 2048);
    put32(&mut disk, 458, 2048);
    let path = save(temp.path(), &disk);
    let scan = analyze(&path).unwrap();
    assert_eq!(
        scan.candidates[0].extents[0].offset,
        (1024 * 1024 + cluster(8)) as u64
    );
    disk[450] = 0xee;
    fs::write(&path, disk).unwrap();
    assert!(analyze(&path).unwrap_err().to_string().contains("GPT"));
}

#[test]
fn texfat_and_missing_bitmap_are_explicitly_rejected() {
    for texfat in [true, false] {
        let temp = tempdir();
        let mut image = volume();
        if texfat {
            image[110] = 2;
            both_boots(&mut image);
        } else {
            image[cluster(2)] = 0;
        }
        assert!(analyze(&save(temp.path(), &image)).is_err());
    }
}

#[test]
fn extraction_revalidates_fingerprint_and_entire_candidate_before_output_creation() {
    let temp = tempdir();
    let (image, _) = with_video(false, false);
    let path = save(temp.path(), &image);
    let scan = analyze(&path).unwrap();
    for case in 0..4 {
        let mut candidate = scan.candidates[0].clone();
        match case {
            0 => candidate.extents[1] = candidate.extents[0].clone(),
            1 => candidate.length = u64::MAX,
            2 => candidate.id += 32,
            _ => candidate.extents[0].offset = u64::MAX,
        }
        let output = temp.path().join(format!("forged-{case}.mov"));
        assert!(extract(&path, &output, &scan.source, &candidate, |_, _| Ok(())).is_err());
        assert!(!output.exists());
    }
    let mut changed = scan.source.clone();
    changed.modified_nanos += 1;
    assert!(
        extract(
            &path,
            &temp.path().join("changed.mov"),
            &changed,
            &scan.candidates[0],
            |_, _| Ok(())
        )
        .is_err()
    );
}

#[test]
fn symlink_input_existing_output_and_symlink_parent_are_refused() {
    let temp = tempdir();
    let (image, _) = with_video(true, false);
    let path = save(temp.path(), &image);
    let input_link = temp.path().join("input-link");
    symlink(&path, &input_link).unwrap();
    assert!(analyze(&input_link).is_err());
    let scan = analyze(&path).unwrap();
    let existing = temp.path().join("existing.mov");
    fs::write(&existing, b"keep").unwrap();
    assert!(
        extract(
            &path,
            &existing,
            &scan.source,
            &scan.candidates[0],
            |_, _| Ok(())
        )
        .is_err()
    );
    assert_eq!(fs::read(&existing).unwrap(), b"keep");
    let output_link = temp.path().join("output-link");
    symlink(&existing, &output_link).unwrap();
    assert!(
        extract(
            &path,
            &output_link,
            &scan.source,
            &scan.candidates[0],
            |_, _| Ok(())
        )
        .is_err()
    );
    let parent_link = temp.path().join("parent-link");
    symlink(temp.path(), &parent_link).unwrap();
    assert!(
        extract(
            &path,
            &parent_link.join("escaped.mov"),
            &scan.source,
            &scan.candidates[0],
            |_, _| Ok(())
        )
        .is_err()
    );
}

#[test]
fn cancellation_retains_partial_output_without_claiming_success() {
    let temp = tempdir();
    let (image, _) = with_video(false, false);
    let path = save(temp.path(), &image);
    let scan = analyze(&path).unwrap();
    let output = temp.path().join("cancelled.mov");
    let result = extract(
        &path,
        &output,
        &scan.source,
        &scan.candidates[0],
        |done, _| {
            if done > 0 {
                Err(io::Error::new(io::ErrorKind::Interrupted, "cancel"))
            } else {
                Ok(())
            }
        },
    );
    assert_eq!(result.unwrap_err().kind(), io::ErrorKind::Interrupted);
    assert!(output.exists());
    assert!(fs::metadata(output).unwrap().len() < 1200);
}

#[test]
fn checksum_valid_unsupported_main_layout_must_not_fall_back_to_stale_backup() {
    let temp = tempdir();
    let mut image = volume();
    image[110] = 2;
    boot_checksum(&mut image, 0);
    assert_eq!(
        analyze(&save(temp.path(), &image)).unwrap_err().kind(),
        io::ErrorKind::Unsupported
    );
}

#[test]
fn unknown_critical_primary_directory_entry_is_explicitly_unsupported() {
    let temp = tempdir();
    let mut image = volume();
    image[cluster(2) + 32] = 0x84;
    assert_eq!(
        analyze(&save(temp.path(), &image)).unwrap_err().kind(),
        io::ErrorKind::Unsupported
    );
}

#[test]
fn active_file_crosslink_including_nonvideo_owner_blocks_extraction() {
    let temp = tempdir();
    let (mut image, _) = with_video(true, false);
    write_entry(
        &mut image,
        cluster(2) + 128,
        &entry("other.bin", 9, 512, true, false),
    );
    let scan = analyze(&save(temp.path(), &image)).unwrap();
    assert!(!scan.candidates[0].recoverable);
    assert!(
        scan.candidates[0]
            .reason
            .as_deref()
            .unwrap()
            .contains("overlaps")
    );
}

#[test]
fn damaged_active_nonvideo_chains_keep_proven_cluster_ownership() {
    for case in 0..7 {
        let temp = tempdir();
        let (mut image, _) = with_video(true, false);
        let (first, length, contiguous) = match case {
            0 => (9, 512, false),  // The existing FAT chain continues past this length.
            1 => (9, 2048, false), // A valid prefix ends before the declared length.
            2 => {
                fat(&mut image, 15, 9);
                fat(&mut image, 9, 15);
                (15, 2048, false) // The overlapping cluster is later in a cyclic chain.
            }
            3 => {
                fat(&mut image, 15, 9);
                fat(&mut image, 9, 0xfffffff7);
                (15, 2048, false) // A bad FAT value follows the overlapping prefix.
            }
            4 => (9, u64::MAX, false), // Invalid length must still reserve FirstCluster.
            5 => (9, u64::MAX, true),
            _ => (9, 0, false), // Contradictory empty length cannot erase FirstCluster.
        };
        write_entry(
            &mut image,
            cluster(2) + 128,
            &entry("other.bin", first, length, contiguous, false),
        );
        let path = save(temp.path(), &image);
        let scan = analyze(&path).unwrap();
        let candidate = &scan.candidates[0];
        assert!(!candidate.recoverable, "case {case}: {candidate:?}");
        assert!(
            candidate.reason.as_deref().unwrap().contains("overlaps"),
            "case {case}"
        );
        assert!(
            scan.warnings
                .iter()
                .any(|warning| warning.contains("Invalid active file other.bin")),
            "case {case}: {:?}",
            scan.warnings
        );
        let output = temp.path().join("must-not-extract.mov");
        assert!(
            extract(&path, &output, &scan.source, candidate, |_, _| Ok(())).is_err(),
            "case {case}"
        );
        assert!(!output.exists(), "case {case}");
    }
}

#[test]
fn video_overlapping_upcase_table_metadata_is_not_recoverable() {
    let temp = tempdir();
    let (mut image, _) = with_video(true, false);
    let mut entry = [0u8; 32];
    entry[0] = 0x82;
    let checksum = image[cluster(8)..cluster(8) + 512]
        .iter()
        .fold(0u32, |sum, &byte| {
            sum.rotate_right(1).wrapping_add(u32::from(byte))
        });
    put32(&mut entry, 4, checksum);
    put32(&mut entry, 20, 8);
    put64(&mut entry, 24, 512);
    fat(&mut image, 8, u32::MAX);
    write_entry(&mut image, cluster(2) + 128, &entry);
    let scan = analyze(&save(temp.path(), &image)).unwrap();
    assert!(!scan.candidates[0].recoverable);
}

#[test]
fn damaged_filesystem_name_can_use_verified_backup_despite_boot_code_bytes() {
    let temp = tempdir();
    let (mut image, _) = with_video(true, false);
    image[3] = 0;
    image[120..510].fill(0xf4);
    let scan = analyze(&save(temp.path(), &image)).unwrap();
    assert!(scan.candidates[0].recoverable);
    assert!(
        scan.warnings
            .iter()
            .any(|warning| warning.contains("backup"))
    );
}

#[test]
fn bitmap_stream_missing_bits_and_root_chain_cycles_are_rejected() {
    for bitmap in [true, false] {
        let temp = tempdir();
        let mut image = volume();
        if bitmap {
            put64(&mut image, cluster(2) + 24, 1);
        } else {
            fat(&mut image, 2, 2);
        }
        assert!(analyze(&save(temp.path(), &image)).is_err());
    }
}

#[test]
fn deleted_fragmented_video_requires_preserved_complete_fat_chain() {
    let temp = tempdir();
    let (mut image, video) = with_video(false, true);
    let path = save(temp.path(), &image);
    let scan = analyze(&path).unwrap();
    assert!(scan.candidates[0].recoverable);
    let output = temp.path().join("deleted-fragmented.mov");
    extract(&path, &output, &scan.source, &scan.candidates[0], |_, _| {
        Ok(())
    })
    .unwrap();
    assert_eq!(fs::read(output).unwrap(), video);
    fat(&mut image, 8, 0);
    fs::write(&path, image).unwrap();
    let scan = analyze(&path).unwrap();
    assert!(!scan.candidates[0].recoverable);
}

#[test]
fn source_modified_during_extraction_is_detected() {
    let temp = tempdir();
    let (image, _) = with_video(false, false);
    let path = save(temp.path(), &image);
    let scan = analyze(&path).unwrap();
    let output = temp.path().join("changed-during-copy.mov");
    let source_writer = fs::OpenOptions::new().write(true).open(&path).unwrap();
    let result = extract(
        &path,
        &output,
        &scan.source,
        &scan.candidates[0],
        |done, _| {
            if done > 0 {
                source_writer.write_all_at(&[0x42], image.len() as u64 - 1)?;
            }
            Ok(())
        },
    );
    assert!(result.unwrap_err().to_string().contains("변경"));
}

#[test]
fn video_larger_than_512_mib_is_scanned_and_streamed_without_loading_payload() {
    let temp = tempdir();
    let path = temp.path().join("large-sparse.img");
    let file = fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&path)
        .unwrap();
    let mut metadata = volume();
    let big_cluster = 1024 * 1024u64;
    let count = 600u32;
    let length = 513 * 1024 * 1024u64;
    put64(&mut metadata, 72, 40 + u64::from(count) * 2048);
    put32(&mut metadata, 92, count);
    metadata[109] = 11;
    both_boots(&mut metadata);
    put64(&mut metadata, cluster(2) + 24, u64::from(count).div_ceil(8));
    write_entry(
        &mut metadata,
        cluster(2) + 32,
        &entry("large.MOV", 8, length, true, false),
    );
    // Erase the old fixture bitmap location inside the enlarged root directory.
    metadata[cluster(3)..cluster(3) + SECTOR].fill(0);
    file.set_len(HEAP as u64 + u64::from(count) * big_cluster)
        .unwrap();
    file.write_all_at(&metadata[..HEAP + SECTOR], 0).unwrap();
    let mut bitmap = vec![0u8; u64::from(count).div_ceil(8) as usize];
    for index in [2, 3].into_iter().chain(8..8 + 513) {
        bitmap[(index - 2) / 8] |= 1 << ((index - 2) % 8);
    }
    file.write_all_at(&bitmap, HEAP as u64 + big_cluster)
        .unwrap();
    file.write_all_at(&media(64), HEAP as u64 + 6 * big_cluster)
        .unwrap();
    drop(file);
    let scan = analyze(&path).unwrap();
    assert_eq!(scan.candidates[0].length, length);
    assert!(
        scan.candidates[0].recoverable,
        "{:?}",
        scan.candidates[0].reason
    );
    let output = temp.path().join("large.partial.mov");
    let result = extract(
        &path,
        &output,
        &scan.source,
        &scan.candidates[0],
        |done, _| {
            if done > 0 {
                Err(io::Error::new(
                    io::ErrorKind::Interrupted,
                    "stop after first bounded chunk",
                ))
            } else {
                Ok(())
            }
        },
    );
    assert_eq!(result.unwrap_err().kind(), io::ErrorKind::Interrupted);
    assert_eq!(fs::metadata(output).unwrap().len(), 1024 * 1024);
}
