# Independent video segment recovery implementation plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Recover and validate disconnected video runs independently.

**Architecture:** Add a byte-exact segment map to the native ES report. Swift checks
and extracts each range through pinned session writes, muxes/decodes separately,
and preserves failed fragments alongside successful ones.

**Tech Stack:** Rust, Swift 6 / SwiftUI macOS 14, existing FFmpeg and SHA-256.

**Spec:** docs/superpowers/specs/2026-10-17-segment-recovery-design.md

## Global constraints

- Originals read-only; media stays on the chosen external destination.
- Maximum 256 segments, explicit error on exhaustion; no guessed reordering.
- Partial video-only results with estimated timing; no AI or completeness claim.
- No new dependency or publication. Use the existing feature branch.

## Review focus

- Adjacent retained extents at a segment boundary must not be coalesced.
- Malformed maps must fail before a subrange is read or muxed.
- Cancellation and I/O failure must not be swallowed as a fragment decode failure.
- One corrupt run must not prevent a later intact run from being exported.
- Output replacement and existing paths must never redirect/overwrite outputs.

### Task 1: Native segment evidence

Files: engine/src/native_video.rs, engine/tests/native_video.rs.
Interface: SalvageReport.segment_map: Vec<SalvageSegment> with id, output_offset,
output_length, frames, extent_start, extent_count, sha256, end_reason.

- [x] Add failing tests for the above boundaries, exact hashes and 256-run cap.
- [x] Implement a bounded segment recorder around existing output writes.
- [x] Run native tests, formatting and clippy; preserve legacy ES bytes.

### Task 2: Independent app export

Files: Sources/RecoveryCore/{VideoProtocol,VideoSession,VideoRecovery}.swift,
Sources/RecoveryCard/VideoRecoveryView.swift, Tests/RecoveryCoreTests/VideoChecks.swift.
Interface: NativeSalvageReport validates its segment map against ES size;
VideoSession.copySegment(from:segment:named:) streams and verifies a selected range.

- [x] Add failing map/copy tests and multi-run integration assertions.
- [x] Implement bounded exclusive range copy and per-run processing with evidence.
- [x] Update UI explanation and partial labels; run Swift checks/build.

### Task 3: Real decoder verification and handoff

Files: scripts/test-native-video.py, README.md, docs/native-video-validation.md,
docs/progress.md.

- [x] Extend generated-media tests with two distinct clips, junk gaps and corruption.
- [x] Run H.264/HEVC pixel equality, isolated failure and source hash checks on SSD.
- [x] Obtain independent code review, fix material findings, run required checks.
- [x] Build/sign local app and document measured results/limits for local commit.

Verification: 63 Rust checks; complete Swift/photo/process/video suite and CLI smoke
passed. The generated suite covers 12 cases, including two deliberately recorded
B-frame timestamp failures. Real X-T5 32-run exports retain 960 exact pixel hashes;
the rebuilt app GUI independently decoded the same 960 frames. ES/MP4 replacement
regressions and early decoder-exit handling pass. Commit uses the required lq draft
workflow; publishing remains outside this milestone.

Ruling: user already requested implementation and continuation; no repeat design
approval. This milestone addresses false joins and failure isolation; arbitrary
fragment reordering and learned ranking remain subsequent work.
