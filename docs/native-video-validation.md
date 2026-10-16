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

63 Rust tests passed (2 export, 12 original pipeline, 25 exFAT, 24 native video). Strict Clippy and Swift integration checks passed. The earlier GUI flow selected the real image, found two videos, exported to the external drive and displayed per-track full-decode results.

Rust tests exercise exFAT boot/backup checksums, cycles, deleted and active streams, bitmap reallocation, crosslinks, forged requests, aliases, cancellation and a sparse 513MiB candidate. Native-video tests exercise codec headers, access units, unsafe starts, malformed/truncated data, random data and source/output protection. Swift checks cover subprocess groups, timeout/cancellation, inherited pipes, byte limits, source changes, session replacement and real decode orchestration.

Not yet established: recovery-rate comparison with commercial tools, camera/firmware coverage beyond the named samples, all X-T5 recording modes, arbitrary fragment rearrangement, physically failing SD cards, or learned-model benefit. [Research and AI proposal](research/native-video-and-ai.md).

## Independent segments (0.3)

Additional generated-media cases use two visually different clips with matching
codec settings, separated by 4096 unknown bytes. Both H.264 and HEVC:

- Intact runs: two independent outputs, 30 + 30 frames, each pixel-identical to its
  respective source. Neither output contains pictures from the other run.
- Corrupted first run: its full decode fails; the second still recovers all 30
  frames with exact pixel hashes. Swift returns both results and keeps INCOMPLETE.
- Mid-NAL cut followed by junk: H.264 first run decodes 19 frames, HEVC 16; **these
  are not exact original prefixes**. A NAL's stored length can consume following
  junk and yield a changed final picture that the decoder accepts. The second
  independent run still matches all 30 original frames in both cases.

The third case deliberately records `exact_pixel_prefix: false`. It is a measured
limitation, not a successful exact-recovery claim; structural checks and strict
decoding cannot establish pixel fidelity without original ground truth. No model
or generated pixels are used. The six new cases run through the native CLI and
through Swift's actual split/copy/hash/mux/decode pipeline on the external drive.

Native tests cover unknown bytes, distinct mdat ranges, picture mismatch, exact
hash/range partitioning and explicit 257th-run rejection. Swift tests reject bad
maps (overlap, gaps, overflow, inconsistent counts and source extents), verify
exclusive bounded copy including nonzero offsets across the 1MiB buffer boundary,
and exercise cancellation/source replacement. Per-segment logs and evidence are
kept even when another segment fails. Source/read errors, cancellation and invalid
protocol still terminate the overall operation. Child-process resource/launch
failures and app-owned output I/O errors also stop the session; a nonzero muxer or
decoder exit is retained with diagnostics without assuming media corruption.

Review regressions replace either the copied ES or the muxed MP4 at an awaited
progress callback. Both codecs reject the replacement before recording successful
provenance. A decoder that exits before creating framehash evidence is an individual
failed result; a replaced output directory remains a fatal session error.

The generated suite now contains 12 cases. Two added B-frame layouts expose an
unresolved timing limitation: the saved MP4 has non-monotonic decoded timestamps.
Strict validation fails and Swift keeps the raw stream and INCOMPLETE marker.
These are failure-detection cases, not successful B-frame recovery claims.

A real X-T5 metadata-erased copy produces 32 independent runs at unknown padding
boundaries. Initially one 30-picture run muxed with only 27 decoded pictures.
Shifting leading timestamps with `-avoid_negative_ts make_zero` restored 30 per
run: all 32 outputs decode, and their combined 960 pixel hashes match the healthy
source. This option shifts timestamps; it does not reconstruct original timing
or solve the generated B-frame failures. See the [FFmpeg format option](https://ffmpeg.org/ffmpeg-formats.html#Format-Options).
The healthy reference here is the same original recording, so this remains a
structural self-check rather than independent-real-reference compatibility proof.
No output movie joins those 32 runs; physical boundaries can split one recording
into many short files. Both input hashes remained unchanged.

The rebuilt 0.3.0 app was exercised through the native macOS file pickers and repair
button with that X-T5 copy, healthy reference and external destination. Its 32
results all passed full decode (960 frames total), with individual reports and no
INCOMPLETE marker. The app was left displaying those partial-recovery results.
