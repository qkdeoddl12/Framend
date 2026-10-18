# Cross-platform recovery implementation plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development or superpowers:executing-plans. Track steps with checkboxes.

**Goal:** General exFAT file recovery and PNG/WAVE carving, a portable Rust engine, and a working macOS/Windows desktop application.

**Architecture:** Preserve existing engine APIs and Swift app behavior. Add platform I/O, general-format APIs, and a Rust desktop-job worker. A Tauri/TypeScript app launches this worker, receives bounded JSON events and cancels the process tree.

**Tech Stack:** Rust, Tauri 2, TypeScript/Vite, existing Swift regression harness, FFmpeg/ffprobe as externally installed tools.

**Spec:** `docs/superpowers/specs/2026-10-18-cross-platform-recovery-design.md` (user approved implementation on 2026-10-18).

**Implementation status:** Implementation of A/B is complete on macOS; native Windows app execution and recovery checks remain pending, so cross-platform acceptance is not yet complete.

## Global constraints

- macOS 14+, Windows 11 x64 first; Windows runtime checks require a Windows runner. Never describe cross-compilation as runtime validation.
- Read-only regular-file inputs, new output sessions, no overwrite, bounded memory, source/destination replacement detection, evidence preserved on cancellation/failure.
- Work in the existing `feat/native-video-recovery` feature checkout; no main-branch edits, push or publication.
- Full A/B scope; RAW family carving, FAT32/GPT and NTFS/APFS are subsequent C milestones, not silently claimed here.
- Use synthetic tests and existing external-SSD fixtures. Do not scan or modify the physical SD card.
- Local `lq` rules apply; main agent coordinates commits. Agents do not stage or commit shared work.

## Review focus

- Windows output parent replaced by a junction: no redirected writes (Task 1).
- False extension or damaged PNG CRC: extraction evidence does not imply successful decoding (Task 2).
- Output ES/MP4 replaced while FFmpeg is running: the job must stop with incomplete evidence (Task 3).
- Cancelled worker with descendants holding pipes: UI must reach a terminal state and preserve incomplete files (Tasks 3/4).
- 64-bit file identifiers passing through JavaScript: use opaque selection strings, never round-trip fingerprints as JS numbers (Tasks 3/4).

## Task 1 — Portable guarded I/O

Files: `engine/src/platform.rs` (or platform submodules), `source.rs`, `export.rs`, output helper sections only in `exfat.rs`/`native_video.rs`, platform tests, `engine/Cargo.toml`.

Interfaces: preserve public engine APIs and SourceFingerprint. Supply internal `Source` read/fingerprint behavior on Windows and Unix; shared guarded directory/output helpers can be used by desktop jobs. Coordinate exact helper signatures before Task 3 integration.

- [x] Add targeted tests for regular files, changed/replaced source, no overwrite, linked parents and destination replacement, plus platform-specific Windows tests.
- [x] Implement OS-specific source identity/read-at and output creation/publication/free-space checks without removing existing protections.
- [x] Make existing integration tests compile conditionally on both OSes without disabling shared functional tests.
- [x] Run engine tests, strict clippy and Windows target check; record unsupported runtime checks separately.

## Task 2 — General files and format-aware carving

Files: `formats.rs`, `scan.rs`, `lib.rs` format declarations, `export.rs` extension match only, exFAT analyzer/extract logic (not output helpers), `main.rs` file commands, `engine/tests/*`; affected Swift format/preview/protocol code.

Interfaces: `exfat::analyze_files(path: &Path) -> io::Result<ExfatScan>` and `exfat::extract_file(...)` with the same arguments as existing `extract`; preserve `analyze`/`extract` video-only semantics. New CLI `file-scan`/`file-extract`. Add `Format::Png`/`Format::Wav`, serialized `png`/`wav`.

- [x] Write failing tests for PNG CRC/truncation/chunk order/bounds, WAVE lengths/chunks/padding, mixed scans and scan-chunk boundaries.
- [x] Implement streaming bounded parsers; use PNG CRC as structural evidence, not a pixel-fidelity claim. WAVE metadata alone is not an audio decode.
- [x] Test and implement general exFAT extraction of intact/deleted supported metadata with arbitrary contents, overlap rejection and source revalidation. Keep video-only compatibility.
- [x] Update all Swift enum consumers, filtering and validation so PNG/WAV cannot crash decoding or receive RAF/JPEG-only labels.
- [x] Run targeted Rust tests and Swift checks.

## Task 3 — Portable jobs, validation and evidence

Files: new `engine/src/jobs.rs`, `process.rs` and supporting session modules, `engine/tests/jobs.rs`; `main.rs` adds `desktop-job`, `lib.rs` exports `jobs`.

Interfaces: stdin JSON `jobs::Request` tagged by `operation`: `scan {source, method: "carve"|"filesystem"}`, `recover {source, output, selections: string[], ffmpeg?: string, ffprobe?: string}`, `repair {source, reference, output, ffmpeg?: string, ffprobe?: string}`. Limits: 32 MiB request, 100k selections, existing parser limits.

Output JSON events use `event`: `started`, `progress {phase, completed, total}`, `candidate {entry}`, `saved {result}`, `complete {directory, count, failed}`. Errors are nonzero exits with stderr; no fake complete event. `entry` fields: `id`, `name`, `format`, `bytes`, `recoverable`, `inspection`, `warnings`, `selection` (opaque serialized source fingerprint plus candidate). `result`: `name`, `path`, `bytes`, `sha256`, `inspection`, `partial`, `error` (nullable).

- [x] Test selection fingerprint preservation, forged selection rejection, mixed-format session hash output, failure marker retention, recovery without decoders -> explicitly unverified results.
- [x] Implement job dispatch over existing carver/general exFAT APIs with schema-versioned results, exclusive files and source/session checks.
- [x] Port video reference-assisted salvage into per-segment ES/MP4 outputs with hash/range validation, reference-derived FPS, `-avoid_negative_ts make_zero`, full track decode/frame hashes and logs.
- [x] Implement bounded process execution, timeout and process-tree cancellation; test bad exits, too much output, inherited pipes and cancelled children.
- [x] Test segment decode failure isolation and file/source/session replacement; compare synthetic and existing X-T5 cases where available.
- [x] Implement bounded JPEG/PNG/RAF-preview reading for the desktop bridge; preserve RAW-unverified labels.

## Task 4 — Shared desktop UI and bridge

Files: `desktop/package.json`, `desktop/src/*`, `desktop/src-tauri/*`, desktop build scripts and UI tests.

Interfaces: Tauri commands `start_job(job_id: String, request: jobs::Request)`, `cancel_job(job_id: String)`, `preview_image(source: String, selection: String) -> {mime, base64}`, `open_result_directory(path: String)`. Listen to `framend-event` envelopes `{job_id, payload}`; payload is worker event or `terminal {success, cancelled, error}`. Dialog plugin handles input/output file pickers. Bridge owns worker lookup and selected output directory opening; frontend cannot execute arbitrary shell commands.

- [x] Build a clear local app with scan/repair modes, file/image and output selection, category filtering, candidate selection, bounded preview, real progress, cancellation, result evidence and errors.
- [x] Provide Korean/English labels, no fixed camera name, truthful unsupported/partial/unverified messaging.
- [x] Test stale-job events, large identifiers stored as opaque strings, failed/cancelled terminal state and selected candidates only.
- [x] Integrate worker launch and bounded IPC, cancellation tree, shutdown cancellation and native directory opening.
- [x] Build production frontend and macOS app; exercise actual picker→scan→recover→result flow.

## Task 5 — CI, documentation, review and delivery

Files: `.github/workflows/checks.yml`, `README.md`, `docs/progress.md`, `docs/cross-platform-validation.md`, scripts as needed.

- [x] Add macOS/Windows native CI for Rust and shared desktop build; no automatic publishing. Preserve macOS Swift regression job.
- [x] Document supported OS versus supported filesystems, exact file recovery versus structure/decode checks, build and external-tool setup, current limitations.
- [x] Run local Rust fmt/test/clippy, Swift regression checks, TypeScript/UI checks, desktop build and actual local flow.
- [x] Request independent task and whole-change reviews; address concrete findings, rerun affected checks.
- [x] Commit via `git add`, `lq commit`, check draft against known work, then `git commit -m`; record worklog and final limitations. Windows native execution remains pending if no runner is available.
Local delivery is recorded in docs/cross-platform-validation.md; native Windows acceptance remains pending.
