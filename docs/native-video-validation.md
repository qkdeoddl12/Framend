# Framend native video validation

Development snapshot: 2026-10-17. macOS, Swift 6.3.1, Rust 1.99.1, FFmpeg 8.1. This is a small correctness suite, not a general recovery-rate benchmark.

| Case | Outcome | What it establishes |
| --- | --- | --- |
| Synthetic H.264, moov removed, distinct healthy reference | 30/30 decoded frame hashes match original | Retained pictures are pixel-identical for this fixture |
| Synthetic HEVC, moov removed, distinct healthy reference | 30/30 decoded frame hashes match original | Same, for this HEVC fixture |
| Synthetic H.264, tail truncated | 24/30 frames, exact original prefix | Partial recovery only |
| Synthetic HEVC, tail truncated | 19/30 frames, exact original prefix | Partial recovery only |
| X-T5 SD image, native exFAT metadata extraction | Two MOV files, 960 + 480 decoded video frames | Exact extraction and full decode for these files |
| X-T5 second MOV audio | 960,960 decoded PCM samples, 20.020 seconds | Audio retained by byte-preserving exFAT extraction |
| X-T5 copies, moov removed, same-original healthy references | 960/960 + 480/480 pixel hashes match | Structural self-check only; separate-real-reference compatibility remains unproven; audio omitted |
| Native exFAT vs earlier PhotoRec outputs | Both SHA-256 identical | Two independent extraction paths produced identical files; not proof of pre-deletion original identity |

NAL salvage remains **partial video-only recovery** even when every test-frame hash matches. Real X-T5 images/media and absolute personal storage paths are excluded from git; detailed reports live on the selected external drive.

## Reproduce

```sh
cargo test --manifest-path engine/Cargo.toml --locked
cargo clippy --manifest-path engine/Cargo.toml --all-targets --locked -- -D warnings
bash scripts/build-app.sh
swift run RecoveryChecks --integration --broken-pipe --bad-json --video --video-process
python3 scripts/smoke-test.py
python3 scripts/test-native-video.py /path/to/external/test-folder
# Use the FIXTURE_DIRECTORY printed by the preceding command:
swift run RecoveryChecks --video-fixture /path/to/generated/native-video-directory
```

The generated-media script uses FFmpeg test sources, separate input/reference clips, missing metadata and truncated tails. It compares decoded pixel hashes, verifies input/reference SHA-256, and requires partial status. It writes only into a new timestamped subdirectory; it does not modify existing recordings.

The optional `--video-exfat-fixture IMAGE DESTINATION` Swift check is a private, fixture-specific check expecting these two known X-T5 videos (960 and 480 frames). It is not a general assertion for arbitrary cards.

## Coverage and limits

58 Rust tests passed (2 format, 12 original pipeline, 25 exFAT, 19 native video). Strict Clippy and Swift integration checks passed. The GUI flow selected the real image, found two videos, exported to the external drive and displayed per-track full-decode results.

Rust tests exercise exFAT boot/backup checksums, cycles, deleted and active streams, bitmap reallocation, crosslinks, forged requests, aliases, cancellation and a sparse 513MiB candidate. Native-video tests exercise codec headers, access units, unsafe starts, malformed/truncated data, random data and source/output protection. Swift checks cover subprocess groups, timeout/cancellation, inherited pipes, byte limits, source changes, session replacement and real decode orchestration.

Not yet established: recovery-rate comparison with commercial tools, camera/firmware coverage beyond the named samples, all X-T5 recording modes, arbitrary fragment rearrangement, physically failing SD cards, or learned-model benefit. [Research and AI proposal](research/native-video-and-ai.md).
