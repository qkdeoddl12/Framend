# Recovery Card implementation ledger

## 2026-10-18 — General files and shared desktop

- User approved A/B implementation: portable guarded Rust I/O, general exFAT
  files, PNG/WAVE carving and a Tauri desktop shared by macOS and Windows.
- Shared worker owns selected-file recovery, per-segment video repair, media
  decoding, provenance, hashes, incomplete markers and bounded tool processes.
  Original Swift application and CLI APIs remain regression targets.
- Independent review findings were reproduced and fixed: evidence changes,
  audio-only M4A validation, bounded child termination, Windows close-time file
  metadata and final completion callback checks. Real external exFAT testing
  found AppleDouble sidecars in evidence enumeration; explicit registration fixes
  this without relaxing actual result/report checks.
- Final engine 115 tests, host/Windows target Clippy, desktop state/bridge tests,
  Swift checks, six-file byte-exact recovery and six generated H.264/HEVC worker
  cases pass. Eight successful video outputs contain 240 exact-pixel frames.
- The macOS native picker/scan/preview/recover flow works with the external SSD.
  Native Windows execution remains pending; CI includes native engine tests,
  portable hash checks, app builds and NSIS packaging. Nothing is published.
- See [cross-platform validation](cross-platform-validation.md) for commands,
  measured scope and remaining acceptance. FAT32/GPT/NTFS/APFS, additional RAW
  carving, arbitrary fragment reconstruction and AI inference are subsequent work.

## 2026-10-17 independent video segments (continuation)

- User authorized continued implementation. Ruling: address unsafe cross-gap
  concatenation and failure isolation before arbitrary fragment ranking/reordering.
- Rust report now maps independent runs to aggregate ES byte ranges, source extent
  indices, structural frame counts, SHA-256 and end reasons; 256-run resource cap.
- Swift validates the complete map, copies with bounded reads/exclusive writes,
  muxes/decodes each run separately, and records per-run evidence. Original files
  remain read-only and all bulk tests are on the external destination.
- RED: five new Rust tests failed on missing segment evidence/limit. GREEN: all 63
  Rust tests and strict Clippy passed. Swift previously returned one merged result;
  new integration test caught it, then passed for H.264/HEVC separate outputs.
- Generated mixed clips yield separate 30+30 exact-pixel frames for each codec.
  Corrupt-first/healthy-second keeps the healthy 30-frame result and records failure.
- Ruling: a mid-NAL cut consumes junk within the declared length and can decode
  changed pixels (observed H.264 final picture). Record exact_pixel_prefix=false;
  do not call decoder success exact recovery. Later intact run is still pixel-exact.
- Independent review reproduced ES and MP4 replacement during progress callbacks;
  pinned snapshots now span mux/validation and final hashing. Both-codec regressions
  pass. An early decoder exit before framehash creation now remains a failed result,
  while app-owned session I/O and child resource/launch failures still abort.
- Real X-T5 sample splits into 32 runs. Leading timestamp adjustment fixes a measured
  27/30 decoded-picture loss per run; all 960 pixels now match the source across the
  32 outputs. Same-original reference only; no original-timing or reassembly claim.
- Two additional synthetic B-frame layouts still fail strict timestamp validation.
  The 12-case generated suite records these failures explicitly and preserves raw
  evidence; universal B-frame timing reconstruction remains unfinished.
- Focused independent re-review found no remaining blocker. Version 0.3.0 builds
  and passes strict local code-sign verification. Final Swift protocol/photo/video,
  process, replacement, failure-isolation and CLI smoke checks passed. The rebuilt
  app's real X-T5 GUI run saved 32 independently decoded clips (960 total frames)
  on the external drive, with results and per-run evidence and no incomplete marker.
- Local helper's UI/version edit exited 3 (invalid edit blocks); completed that
  operation directly without retry. Worklog records results and remaining limits.

## Earlier implementation history

Plan: `docs/superpowers/plans/2026-10-16-recovery-preview.md`

- User approved proceeding with the preceding design. Implementing inline without another approval round.
- Fresh empty repository initialized on `feat/recovery-preview`; no existing branch or work to isolate.
- Swift 6.3.1 available, Xcode app absent, Command Line Tools available. Use SwiftPM and a local app bundle.
- `lq` returned exit 3 (reserved for trading bot); fallback to direct work, no retry for that operation.
- Scope: first useful image-file recovery increment. Physical-card acquisition and advanced media recovery remain later milestones.
- Pre-flight: Rust events ↔ Swift Codable; Rust source fingerprint ↔ Swift request; exact protocol definitions will be shared in fixtures/tests.
- Task 1: initial RED 8/9 failed on placeholder implementation; implemented parser/export, GREEN 9/9.
- Ruling: Command Line Tools lacks the Swift Testing module. Use a portable Swift executable test harness (`swift run RecoveryChecks`) instead of installing Xcode or a framework dependency. Same assertions, nonzero exit on failure.

- Independent final review found three important issues. All reproduced before fixing: early child exit killed the Swift host via SIGPIPE (exit 141), malformed protocol waited for a sleeping child, and an invalid RAF preview hid an intact JPEG. Regression checks now pass.
- Use per-pipe F_SETNOSIGPIPE and terminate the engine before unwinding stderr collection. RAF acceptance now requires a valid preview of exactly the declared length. Reset preview loading when selection clears.
- Actual external exFAT test exposed ENOTSUP for both hard links and RENAME_EXCL. Added create_new + copy fallback with incomplete marker retained; existing-file and symlink replacement checks pass for both publication paths. Byte-exact JPEG/RAF export to the connected exFAT SSD passed.
- Verification: 14 Rust tests; clippy with -D warnings; 8 Swift protocol/media checks and 5 process/integration checks passed. App built and ad hoc signature verified. GUI tested image selection, scan, JPEG preview, two-file selection, export, and JPEG validation; output hashes verified.
- User subsequently requested recovery of the connected 128 GB Untitled SD card onto the external SSD. Identified distinct physical devices and sufficient space. Bulk output goes to the SSD; raw-card acquisition uses GNU ddrescue 1.30 and a mapfile, with source identity checks and normal unmount before read. System administrator authentication succeeded in visible Terminal and card acquisition is completed: exact 128,035,323,904 bytes and zero read errors. This external workflow is not yet a feature in the app.
- A synthetic 5-format PhotoRec check recovered JPEG, PNG, MP4, PDF and ZIP with exact SHA-256 matches, while no real-card recovery-rate claim is available yet.
- Local commit draft: `lq commit` returned exit 3 (reserved window); use a manually checked Conventional Commit for the initial app implementation. No remote publishing.

## Final Test Results

1,477 preferred RAW photos consist of 1,475 ordinary RAF sensor-decode passes plus 2 complete M-RAW containers preserving 3 exposures each; only the first exposure in each container was decoded. Four standalone undecodable RAF outputs are secondary embedded M-RAW frames, not four damaged independent photos. MOV 2 files: 32.032 and 20.020 seconds at 1920x1080, all 960 and 480 video frames plus available audio decoded without errors. Three extra 160x120 JPEG thumbnails passed image decoding. Seven other files remain unverified; one partial RAF and one partial TIFF retained. Source metadata unchanged; no recovery-rate claim.

Native scan found 1477 RAF and 3 JPEG candidates; redundant RAW export stopped after confirming source-offset overlap, existing incomplete comparison exports preserved and excluded. exFAT double copy/full-sync publication is slow; no application code fix was made. Current app still only JPEG/RAF preview; broad recovery, LibRaw validation and M-RAW reconstruction were temporary external operator workflows. GitHub remains unpublished and new app feature design awaits confirmation. No private filenames, media, disk identifiers or session paths in these docs.


## 2026-10-17 — Framend native video implementation

- User approved implementation, requested an independently written engine and research-informed development; project named Framend (Frame + Mend).
- Added native exFAT MOV/MP4 extent recovery, AVC/HEVC reference-assisted video-only NAL salvage, typed CLI, macOS video tab, progress/cancellation and per-track full decode/evidence reports.
- Current app includes photo and video workflows. Prior statements above that video integration is pending describe the previous milestone. Generic document/HEIF recovery, missing-metadata arbitrary fragment reordering and AI inference remain unimplemented.
- All large experiments and real outputs stay on the external drive. Native exFAT recovered both known videos byte-identical to earlier outputs, with 960/480 video frames and 960,960 audio samples decoded. Synthetic distinct-reference AVC/HEVC recovered 30/30 exact pixel hashes; truncated tails recovered24/19. Real same-original-reference tests recovered960/480 exact pixel hashes, explicitly not independent-reference evidence.
- Independent review reproduced and fixed lost ownership from corrupt active FAT chains, valid emulation-prevention bytes at sampled-header boundaries and parent symlink traversal; reviewer reran44 focused tests and approved all3 fixes. Swift session directory replacement is now detected and own report writes use a pinned directory descriptor.
- Final Rust58 tests and all-target Clippy pass. Swift protocol, existing photo integration, process-group cancellation/timeout/output-bound checks and native video integration pass. Native generated-media and GUI real-image export checks pass. Ad hoc app bundle built; no remote publishing.
- AI direction: local fragment classification and candidate ranking, followed by structural/decode validation; generation of missing pixels cannot establish original content. Sources, benchmark plan and limits saved in docs/research/native-video-and-ai.md.
- Branding lq edit returned3 (invalid output blocks); completed directly without retrying that operation.
