# Recovery Card Implementation Plan

> **For agentic workers:** Use superpowers:executing-plans to implement task-by-task. Checkboxes track completion.

**Goal:** 실행 가능한 macOS JPEG/RAF 이미지 복구 프리뷰를 만든다.

**Architecture:** SwiftUI가 Rust CLI를 JSON Lines로 호출. Rust는 읽기 전용 스캔과 안전한 추출, Swift는 선택·상태·이미지 검증을 담당.

**Tech Stack:** Rust stable, serde/serde_json/sha2, Swift 6, SwiftUI, ImageIO, Swift Package Manager.

**Spec:** `docs/design.md`

## Global Constraints

- macOS 14+; 설치된 Swift 6.3.1로 빌드한다.
- 원본 일반 파일만 읽기 전용 접근. 실제 SD카드 장치 접근 제외.
- RAW 디코딩 검증 전에는 RAF를 정상 복구라고 표시하지 않는다.
- 원본과 사용자 파일을 덮어쓰지 않는다. 전체 이미지 메모리 로드 금지.

## Review Focus

- JPEG 내부 썸네일과 가짜 EOI가 원본 경계를 잘못 결정하지 않아야 한다.
- 경계에 걸친 시그니처, 잘린 헤더, 거짓 길이는 누락·패닉을 일으키지 않아야 한다.
- 내보내기 직전 이미지 교체·수정은 거부해야 한다.
- 중단·디스크 부족·프로세스 실패는 성공으로 표시하면 안 된다.
- 위조된 요청, 경로와 심볼릭 링크는 기존 파일 손상으로 이어지면 안 된다.

### Task 1: Rust scanner and safe exporter

**Files:** `engine/Cargo.toml`, `engine/src/{lib,source,formats,scan,export,main}.rs`, `engine/tests/recovery.rs`.

**Interfaces:** `scan(path, callback)` emits started/progress/candidate/complete JSON events.
`recover(path, output, request)` verifies a source fingerprint and revalidates selected candidates; emits saved/complete events and writes a manifest in a new directory.

- [x] Write and run failing integration tests for JPEG boundaries, RAF extents, malformed files, unchanged input, safe exports and changed-source rejection.
- [x] Implement bounded readers, parsers, streaming scan, export hashing and CLI.
- [x] Run `cargo test --manifest-path engine/Cargo.toml`; expect all tests pass.
- [x] Run `cargo clippy --manifest-path engine/Cargo.toml --all-targets -- -D warnings`; expect no warnings.

### Task 2: Native app

**Files:** `Package.swift`, `Sources/RecoveryCore/*`, `Sources/RecoveryCard/*`, `Tests/RecoveryCoreTests/*`.

**Interfaces:** Codable protocol types mirror the Rust JSON. Process bridge streams events asynchronously, terminates on cancellation. MainActor observable model owns one operation at a time.

- [x] Write failing protocol/state tests: decode events, reject incomplete streams, no false RAW validation.
- [x] Implement async process bridge, bounded JPEG validation, source picker, progress, result selection, export summary and errors.
- [x] Run `swift run RecoveryChecks --integration --broken-pipe --bad-json` (approved test-harness substitution in ledger); expect all checks pass. Run `swift build`; expect executable.

### Task 3: Deliverable and review

**Files:** `scripts/build-app.sh`, `scripts/make-demo.swift`, `scripts/smoke-test.py`, `README.md`, `docs/progress.md`.

- [x] Generate a real JPEG plus synthetic RAF disk fixture; scan/export and compare hashes against originals.
- [x] Package `dist/Recovery Card.app` with the Rust binary and ad hoc signature; launch on this Mac.
- [x] Request an independent final code review under executing-plans, fix important findings with regression tests.
- [x] Document actual test evidence and limitations. Commit locally using `lq commit` draft, or self on exit 3. No push/publish.
