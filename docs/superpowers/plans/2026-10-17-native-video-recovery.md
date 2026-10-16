# Native Video Recovery Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development or superpowers:executing-plans. Independent engine modules may use dispatching-parallel-agents with disjoint file ownership; integration stays with the coordinator.

**Goal:** Add an original implementation of exFAT video reconstruction and reference-assisted AVC/HEVC video-track salvage, exposed in the macOS app.

**Architecture:** Two independent Rust parsers produce evidence and byte-preserving outputs. Swift owns sessions, subprocess cancellation, FFmpeg mux/validation, reports and UI. No PhotoRec/untrunc repair dependency.

**Tech Stack:** Existing Rust/serde/sha2/libc and Swift 6/macOS 14; installed FFmpeg 8.1 for standard mux/decode only.

**Spec:** `docs/superpowers/specs/2026-10-17-video-repair-design.md`, especially its approved implementation amendment.

## Global Constraints

- Inputs remain read-only regular files; outputs are exclusive new files in new directories on the selected drive.
- Retain partial outputs and INCOMPLETE markers; never confuse successful decode with complete recovery.
- Native salvage is video-only, estimated frame rate, uncertain timing; exFAT extraction preserves container bytes including audio.
- Bound untrusted lengths, allocations, traversal, external-tool output and time; no network or external data references in media decoding.
- Large fixtures and real media stay on the external SSD and outside git; source metadata and hashes belong in local reports.
- Existing photo workflow and Swift 6.3.1 toolchain remain compatible. Work in the existing feature checkout; no extra approval round.

## Review Focus

1. Random bytes resembling a NAL must not become a validated recovery; negative fixtures and independent decode cover this.
2. Cross-stream/reference mismatch must remain a partial/failed recovery, never an assertion of original identity.
3. Corrupt exFAT lengths, cycles, reallocated deleted clusters must terminate and report why.
4. Cancelled external tools and descendants must be reaped, and incomplete sessions stay incomplete.
5. Existing paths, symlinks, source changes and disappearing destinations must fail without overwriting input or prior outputs.

### Task 1: exFAT metadata recovery

**Files:** `engine/src/exfat.rs`, `engine/tests/exfat.rs`; coordinator registers module in `lib.rs`.
**Interfaces:** `analyze(&Path) -> io::Result<ExfatScan>`; public serializable scan includes `source`, `candidates`, `warnings`. Candidate: `id: u64`, `name: String`, `length: u64`, `extents: Vec<Extent{offset,length}>`, `deleted: bool`, `recoverable: bool`, `reason: Option<String>`, `allocated_clusters: u64`. `extract(&Path, &Path, &SourceFingerprint, &ExfatCandidate, impl FnMut(u64,u64)->io::Result<()>) -> io::Result<String>` writes exclusive output, returns SHA256.
- [x] Write fixtures/tests asserting exact logical bytes for fragmented and contiguous/deleted videos, checksums, backup boot, invalid geometry, cycles, shortened FAT, bitmap reassignment, symlink/output collisions.
- [x] Observe failures, implement bounded parser and extraction with revalidation, run `cargo test --test exfat` and clippy.

### Task 2: native AVC/HEVC salvage

**Files:** `engine/src/native_video.rs`, `engine/tests/native_video.rs`; coordinator registers module.
**Interfaces:** `salvage(input: &Path, reference: &Path, output: &Path, progress: impl FnMut(u64,u64)->io::Result<()>) -> io::Result<SalvageReport>`. Output is Annex B AVC or HEVC based on report `codec` (`h264`/`hevc`). Report serializes codec, frame rate numerator/denominator, frames, retained byte extents, skipped bytes, source/reference hashes, warnings. No other repair executable invoked.
- [x] Test missing moov with distinct reference, malformed lengths, non-video data, non-keyframe prefix, multi-slice access units, truncated NAL, unsupported reference and identical input rejection.
- [x] Observe failures, implement bounded reference box/config parsing and NAL chain/access-unit validation, stream output with parameter sets and start at safe random access; never present unknown audio/timing as restored.
- [x] Run unit tests and synthetic AVC/HEVC full-frame decode comparisons using distinct clips.

### Task 3: command protocol, sessions and validation

**Files:** `engine/src/main.rs`, `engine/src/lib.rs`, `Sources/RecoveryCore/VideoRecovery.swift`, `Sources/RecoveryCore/ToolProcess.swift`, `Sources/RecoveryCore/VideoProtocol.swift`, `Tests/RecoveryCoreTests/VideoChecks.swift`.
**Interfaces:** CLI `video-scan IMAGE` emits one JSON scan; `video-extract IMAGE OUTPUT` reads `{source,candidate}` stdin and emits JSONL progress/complete; `video-salvage INPUT REFERENCE OUTPUT` emits JSONL progress and complete with report. Swift `VideoRecovery.scan`, `extract`, `repair` async methods report stages and return local report URL/results.
- [x] Write negative process tests for cancellation, descendant termination, excessive output, timeout, nonzero exit, spaces/Korean paths, overwrite/link and missing tool failures.
- [x] Implement subprocess groups with bounded capture, session marker and reports, source fingerprint checks, free space checks, versions/hashes, safe output paths.
- [x] For salvage, mux native elementary stream without reencoding at explicitly estimated reference rate. Decode all video/audio tracks of container output separately; retain diagnostics and per-track frame/sample counts. Keep partial label even when salvage decode passes.
- [x] Run Swift checks and native CLI end-to-end regression fixtures.

### Task 4: app integration and reproducible evidence

**Files:** `Sources/RecoveryCard/VideoRecoveryView.swift`, `Sources/RecoveryCard/VideoRecoveryModel.swift`, `Sources/RecoveryCard/RecoveryCardApp.swift`, `scripts/test-native-video.py`, `README.md`, `docs/research/native-video-and-ai.md`, `docs/progress.md`.
- [x] Connect existing app with photo/video tabs, repair/exFAT modes, file/reference/output selection, candidate selection, elapsed stage, cancel, results and Finder.
- [x] Generate distinct synthetic AVC/HEVC clips, remove moov/truncate/reorder extents, test native output frames/hashes/audio and unsupported input behavior. Store expected and measured results.
- [x] Test external SSD copies of X-T5 videos, report same-source-reference self-check separately from distinct-reference validation.
- [x] Run all Rust/Swift checks, clippy, release build and GUI smoke flow; request independent whole-change review and fix substantial findings.
- [x] Document papers, implemented principles, limits, reproduction commands and benchmark evidence; use lq commit draft after staging, review against known changes and commit. No GitHub publication in this task.
