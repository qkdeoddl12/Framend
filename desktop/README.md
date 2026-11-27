# Framend desktop

Shared Tauri 2 application for macOS 14+ and Windows 11 x64. Inputs are ordinary disk image/video files opened read-only. The app does not open physical drives. Windows application support is separate from filesystem support: metadata recovery currently handles exFAT, while signature scanning handles JPEG, PNG, RAF and RIFF/WAVE.

## Local development

Use Rust 1.99.1 and Node.js 24 to match the release toolchain. On macOS install Xcode Command Line Tools. On Windows install Microsoft C++ Build Tools with the Desktop development with C++ workload and WebView2. Use the native operating system for installers and runtime checks.

From this directory:

```sh
npm ci
npm test
npm run build
npm run desktop:dev
```

The Tauri development hook builds the engine in release mode, stages the native sidecar, then starts Vite on `127.0.0.1:1420`. A browser-only Vite preview can display the interface but cannot select files or start recovery. The desktop application listens for worker events before enabling jobs.

The language control switches between Korean and English. Select a source image, select an output folder, choose signature or exFAT scanning, and scan. Filter candidates, select the files to recover, and choose **Recover selected**. Only selected recoverable candidates are submitted. JPEG/PNG and carved RAF embedded JPEG candidates support a bounded preview; this does not verify complete RAW decoding.

Video repair has separate source/reference selectors. Use a healthy reference with compatible codec and recording settings. FFmpeg and ffprobe are discovered on PATH or can be chosen individually in the optional tools section. They are not bundled. Without an applicable decoder, extracted files remain explicitly unverified. Repair requires the tools.

## Native builds

```sh
npm run desktop:build
```

The staging script builds the engine for `TAURI_ENV_TARGET_TRIPLE` (or the native Rust host target), copies it into `src-tauri/binaries/recovery-engine-<target>[.exe]`, and Tauri packages it beside the application executable. No arbitrary command execution is exposed to frontend JavaScript. Cross-target staging requires the corresponding Rust target, linker and native toolchain; a cross-build does not establish runtime support. Universal macOS staging is not implemented: build ARM64 and x64 separately.

macOS produces `.app` and `.dmg` bundles in `src-tauri/target/release/bundle/`. Windows produces an NSIS installer under the corresponding native target directory. These local builds do not publish or notarize a release. For a locally signed macOS application and DMG without Finder layout automation:

```sh
CI=true APPLE_SIGNING_IDENTITY=- npm run desktop:build
```

`-` selects ad hoc signing, not a distribution identity. `CI=true` skips Finder's cosmetic DMG setup, which can otherwise wait for a `.DS_Store` file. This behavior is defined in the [Tauri DMG bundler](https://github.com/tauri-apps/tauri/blob/dev/crates/tauri-bundler/src/bundle/macos/dmg/mod.rs). Add `-- --bundles app` when only the application bundle is needed. An unsigned bundle is only a packaging artifact; validate the ad hoc bundle with `codesign --verify --deep --strict` before local delivery.

## Checks and CI

Run on each native runner after installing Rust and Node.js:

```sh
npm ci
npm test
npm run build
npm run stage-engine
cargo fmt --manifest-path src-tauri/Cargo.toml --check
cargo test --manifest-path src-tauri/Cargo.toml
cargo clippy --manifest-path src-tauri/Cargo.toml --all-targets -- -D warnings
npm run tauri -- build --no-bundle --ci
```

Installer jobs can run `npm run desktop:build -- --ci --no-sign` separately. Keep the lockfiles; dependency resolution is reproducible with `npm ci` and Cargo's `--locked` option. The stage hook needs `cargo` and `rustc` on PATH; `CARGO` and `RUSTC` environment variables can override their executable paths.

State tests cover stale jobs, opaque selections containing 64-bit identifiers, cancelled/failed terminal states and selected-only recovery. Rust bridge tests cover invalid and oversized protocol records, missing completion, stderr retention, process-tree cancellation and inherited pipes. Native picker → scan → recover → result opening remains a runtime check; CI build success alone does not establish that flow.

## Results and cancellation

The worker writes a new session directory, output hashes, inspection evidence and logs. Structure checks, checksums, successful decoding, partial recovery and byte-for-byte original fidelity are separate claims. A PNG CRC check is not a successful pixel decode; a RAF JPEG preview is not complete RAW validation. Unknown filesystem file types can be extracted without an internal format decoder and are marked unverified.

The bridge permits one worker at a time, caps each JSON record at 1 MiB, request JSON at 32 MiB, aggregate worker stdout at 256 MiB, and retained stderr at 64 KiB. It rejects malformed records, requires a completion event plus a successful exit, and always sends a terminal event. A completion record is provisional until the worker exits. Cancellation and application close terminate the process group on macOS or Job Object on Windows, including decoder descendants. Inherited pipes cannot prevent the interface from reaching a terminal state.

Failures and cancellation preserve incomplete evidence. Use **Open result folder** after a failed/cancelled write job to inspect the selected output folder. The directory-opening command accepts only directories selected for a write job or returned by this session's worker. Candidates and progress come from the actual engine; there is no demo data or network upload.
