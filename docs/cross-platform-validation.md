# Desktop recovery validation

## Public preview 0.1.0 · 2026-11-27

| Release download | Checks completed |
| --- | --- |
| Windows x64 installer | A Windows Server 2025 x64 runner installed the NSIS package, launched the installed app and observed its Framend window. The installed worker recovered six generated files. |
| Apple silicon and Intel Mac disk images | Native macOS 15 runners verified and mounted each DMG read-only, checked the bundled app signature and ran the same six-file recovery check with the worker inside the DMG. |

All three release builds reproduced the expected SHA-256 hashes for two files found by signature and four files extracted from exFAT. The input images stayed unchanged. Windows 11 interactive file-picker and cancellation testing on a physical PC remains open.

[GitHub Actions run](https://github.com/qkdeoddl12/Framend/actions/runs/45960395573) · [Release files and checksums](https://github.com/qkdeoddl12/Framend/releases/tag/v0.1.0)

## Local validation · 2026-10-18

The A/B implementation adds a portable Rust worker and a shared Tauri desktop.
The October tests below covered macOS execution and Windows engine target checks. This is an experimental
preview, not a measured comparison with commercial recovery products.

## Implemented scope

| Path | Supported behavior | Boundary |
| --- | --- | --- |
| General exFAT | Extract regular files whose metadata and allocation pass validation, including documents, archives, audio and RAW | Single-FAT exFAT 1.00 volume/MBR; missing chains and conflicting allocations are rejected |
| Signature scanning | JPEG, RAF, PNG and RIFF/WAVE | Contiguous data; PNG CRC/order and WAVE structure checks do not imply decoding |
| Video repair | Reference-assisted H.264/HEVC salvage, independent segments and full track validation | Always partial and video-only; timing is estimated from the reference |
| Desktop | Korean/English, file selectors, filters, selected recovery, bounded image previews, progress, cancellation and result folders | Regular image/video inputs; no physical-drive acquisition in the shared app |
| Evidence | Input identity, byte ranges, SHA-256, tool versions, reports, per-track hashes and failure markers | Decoding does not prove original fidelity without ground truth |

Windows file handles, file identities, reparse-point rejection, exclusive output,
free-space checks and process Job Objects have separate implementations. Unix uses
pinned directories and process groups. Neither path permits source writes or
overwriting an existing output. Changed media and changed evidence are checked
again before a worker can exit successfully.

## Local verification

- Rust: 115 tests passed; formatting and strict all-target/all-feature Clippy passed.
- Windows: strict Clippy/type checking for `x86_64-pc-windows-gnu`, all targets and
  features, passed. This does not link or execute a Windows desktop application.
- Desktop: 5 state tests and 5 native bridge tests passed; TypeScript, production
  frontend build, native bridge formatting and strict Clippy passed.
- Existing Swift application: release build and code-sign verification passed;
  11 protocol/media checks plus scan/export, process cancellation, timeout,
  inherited pipes, video failure isolation and malformed-protocol checks passed.
- Portable worker: generated PNG/WAVE signature scan (2 files) and an exFAT image
  (4 files: PNG with a misleading MOV extension, WAVE, text and ZIP) recovered exact
  SHA-256 matches on the external exFAT SSD. Inputs were unchanged. Text/ZIP remained
  explicitly unverified. The same script is included in both native CI jobs.
- Actual macOS interface: native file pickers, PNG/WAVE scan, PNG preview,
  selected recovery and two decoded results were exercised using an external SSD.
- Apple Silicon `.app` and `.dmg` were built from the final source. Ad hoc bundle
  signature verification and DMG checksum verification passed. The packaged
  worker also passed the six-file portable recovery script. These artifacts are
  local previews, without distribution signing or notarization.
- Shared video worker on the external exFAT SSD: six H.264/HEVC cases covering
  missing `moov`, two intact separated runs and corrupt-first/intact-second runs.
  Eight successful outputs total 240 frames whose pixel hashes match their known
  originals. Both deliberately damaged first runs remain failed; their later
  30-frame runs succeed. Source/reference hashes remain unchanged, and incomplete
  markers match the failure state. These are generated fixtures, not a recovery
  rate measured across real cameras.

The regression suite covers forged/stale selections, large identifiers kept as
opaque strings, damaged structures, overlapping allocations, source/output
replacement, report tampering, malformed tool output, completion delivery failure
and process-tree cancellation. External-drive testing exposed automatic macOS
AppleDouble sidecars; only explicitly created evidence files are now registered,
so removal of `._INCOMPLETE.txt` cannot invalidate an otherwise completed repair.

## Reproduction

```sh
cargo fmt --manifest-path engine/Cargo.toml --check
cargo test --manifest-path engine/Cargo.toml --all-features --locked
cargo clippy --manifest-path engine/Cargo.toml --all-targets --all-features --locked -- -D warnings
cargo build --manifest-path engine/Cargo.toml --locked
python3 scripts/test-desktop-jobs.py --engine engine/target/debug/recovery-engine --output-root /path/to/existing/output
```

Use `recovery-engine.exe` on Windows. The portable script creates a new directory
and retains generated fixtures/results there. It needs only Python's standard
library; installed FFmpeg/ffprobe additionally enable actual media decoding.
Real media tests in the Rust suite require those tools and return early when they
are unavailable. See [desktop build instructions](../desktop/README.md) for native
toolchains and installation artifacts.

## Outstanding acceptance

An interactive Windows 11 picker → scan → recover → cancel session on a physical PC remains to be checked. The first public preview uses ad hoc signing on macOS and an unsigned Windows installer.

FAT32/GPT/NTFS/APFS, additional RAW/HEIF/PDF/ZIP signature parsers, arbitrary
fragment reassembly and AI inference remain subsequent work. General exFAT
extraction of a file type does not establish a dedicated parser or decoder for it.
