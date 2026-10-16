# Recovery Card implementation ledger

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
- User subsequently requested recovery of the connected 128 GB Untitled SD card onto the external SSD. Identified distinct physical devices and sufficient space. Bulk output goes to the SSD; raw-card acquisition uses GNU ddrescue 1.30 and a mapfile, with source identity checks and normal unmount before read. System administrator authentication succeeded in visible Terminal and card acquisition is running, monitored by a separate Terminal progress window. This external workflow is not yet a feature in the app.
- A synthetic 5-format PhotoRec check recovered JPEG, PNG, MP4, PDF and ZIP with exact SHA-256 matches, while no real-card recovery-rate claim is available yet.
- Local commit draft: `lq commit` returned exit 3 (reserved window); use a manually checked Conventional Commit for the initial app implementation. No remote publishing.

