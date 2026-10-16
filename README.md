# Framend · 프레임엔드

**Frame + Mend.** macOS에서 사진·영상의 남은 데이터를 찾고 복구 근거를 기록하는 로컬 앱입니다. Rust 자체 엔진으로 JPEG·RAF 추출, exFAT 영상 조각 복원, H.264·HEVC 프레임 부분 복구를 제공합니다. 현재는 실험용 프리뷰입니다.

## 실행

이미 빌드된 앱: `dist/Framend.app`

```sh
open 'dist/Framend.app'
```

1. SSD에 보관한 디스크 이미지(`.img`, `.dd`, `.raw`)를 선택합니다.
2. **파일 찾기**를 누릅니다. 한 파일을 선택하면 JPEG 미리보기를 볼 수 있습니다.
3. 복구할 파일을 선택하고 **선택 파일 저장**을 누릅니다. 원본 SD카드가 아닌 별도 위치를 사용하세요.
4. 새 `Recovery-*` 폴더에 사진과 `manifest.json`, `validation.json`이 저장됩니다.

`manifest.json`에는 이미지의 파일 식별정보, 바이트 위치·길이, 추출한 파일의 SHA-256이 기록됩니다.
`validation.json`에는 저장 후 JPEG 디코딩 결과가 기록됩니다.
중단·오류가 난 세션은 `INCOMPLETE.txt`와 `.partial` 파일이 남을 수 있습니다.
exFAT 저장장치에서는 새 파일 생성 후 복사하는 방식으로 덮어쓰기를 방지합니다. 중간에 종료되면 확장자가 `.jpg`/`.raf`인 미완성 파일도 남을 수 있으므로 `INCOMPLETE.txt`가 있는 폴더는 완료된 결과로 간주하지 마세요.

## 현재 지원과 한계

- 입력은 일반 파일만 허용. 장치·FIFO·심볼릭 링크는 엔진에서 거부합니다.
- 원본을 읽기 전용으로 열고 원본 변경을 검사합니다. 출력은 매번 새 디렉터리와 새 파일로 작성합니다.
- 최대 512 MiB의 연속 JPEG/RAF 후보, 최대 100,000개. JPEG 미리보기는 64 MiB까지, 디코딩은 80 MP까지.
- RAF는 유효한 내장 JPEG와 겹치지 않는 헤더 범위를 확인한 뒤 추출합니다. 내장 JPEG가 손상된 RAF는 아직 지원하지 않습니다. **RAW 전체 디코딩 미검증**이며 내장 JPEG 미리보기와 구별합니다.
- JPEG 디코딩 성공은 ImageIO가 픽셀을 읽었다는 뜻입니다. 원본과 동일함이나 손상 없음의 보증이 아닙니다.
- 삭제 여부·원래 이름·폴더는 알 수 없습니다. 존재하는 사진도 후보로 발견됩니다.
- `.dmg` 압축 컨테이너, 앱 안에서의 물리 SD카드 복제, FAT32/GPT, 메타데이터가 사라진 임의 조각 재배열, HEIF·일반 문서 복구는 아직 지원하지 않습니다. exFAT·영상 지원 범위는 아래와 같습니다.
- 실제 X-T5 카드에서 사진 후보 검출과 별도 RAW 검증을 수행했습니다. 이 앱의 RAF 검증은 여전히 구조 검사이며, 전체 카드에 대한 복구율이나 모든 압축 RAW 모드 호환성은 확정하지 않았습니다.
- macOS 14+를 대상으로 Apple Silicon Mac에서 빌드·검증합니다. 로컬 ad hoc 서명이며 공증한 배포본이 아닙니다.

## 영상 복구 (0.2 실험 기능)

영상 기능에는 `ffmpeg`와 `ffprobe`가 필요합니다 (`brew install ffmpeg`). 영상 수리 알고리즘은 자체 Rust 코드이고, FFmpeg는 컨테이너 저장과 독립 디코딩 검사에 사용합니다.

- **카드 이미지에서 영상 찾기:** exFAT 파일 목록·FAT 체인·할당 비트맵을 검사해 MOV/MP4 후보를 표시합니다. 선택한 후보의 원래 바이트 순서를 복원하고 모든 영상·음성 트랙을 끝까지 디코딩합니다. 저장 폴더는 외장 SSD를 선택하세요.
- **손상 영상 수리:** 손상 MOV/MP4와 같은 카메라·코덱·해상도·프레임 속도의 정상 참고 영상을 선택합니다. H.264/HEVC NAL 구조와 IDR 시작점을 검사해 남은 영상 프레임을 추출합니다. **영상 트랙만 부분 복구**하며 음성과 원래 타임스탬프는 복원하지 않습니다. 재생 속도는 참고 영상의 평균값입니다.
- 결과는 매번 새 `Framend-*` 폴더에 저장됩니다. `results.json`, 원본 위치/해시 보고서, 도구 정보, 트랙별 프레임 해시와 오류 로그를 보존합니다. 중단·실패·디코딩 오류가 있으면 `INCOMPLETE.txt`를 유지합니다.
- `전체 디코딩 통과`는 디코더가 트랙을 읽었다는 의미입니다. 원본의 완전성·장면 순서·조각의 동일 출처까지 증명하지 않습니다. NAL 수리는 디코딩을 통과해도 항상 부분 복구로 표시합니다.

exFAT는 단일 FAT의 1.00 볼륨/MBR만 지원합니다. 삭제된 폴더, 사라진 FAT 체인, 미초기화된 파일 끝은 추정하지 않습니다. NAL 수리는 지원되는 progressive H.264 / single-layer HEVC에 한정되며 IDR에서만 시작·재시작합니다. 손상·알 수 없는 바이트 뒤의 종속 프레임은 건너뜁니다. 메타데이터와 프레임 경계가 모두 사라진 일반적인 단편화 복구는 후속 연구 범위입니다.

[재현 가능한 검증 결과](docs/native-video-validation.md) · [논문 근거와 AI 도입 실험안](docs/research/native-video-and-ai.md). AI 모델은 아직 탑재하지 않았습니다.

## 개발·검증

Rust stable(1.85+), Swift 6 이상과 macOS Command Line Tools가 필요합니다.
설치된 Swift 6.3.1과 Rust 1.99.1을 사용합니다. 셸 PATH에 cargo가 없다면 `$HOME/.cargo/bin/cargo`를 사용하세요.

```sh
cargo test --manifest-path engine/Cargo.toml
cargo clippy --manifest-path engine/Cargo.toml --all-targets -- -D warnings
swift scripts/make-demo.swift
bash scripts/build-app.sh
swift run RecoveryChecks --integration --broken-pipe --bad-json --video --video-process
python3 scripts/smoke-test.py
```

Xcode 전체가 없는 환경에서도 실행할 수 있도록 Swift 검사는 의존성이 없는 실행형 테스트 하네스를 사용합니다.
`fixtures/generated/demo-card.img`를 앱에서 열면 실제 JPEG와 테스트용 합성 RAF를 복구해 볼 수 있습니다.

## 구조

- `engine/`: Rust 파일 파서, 스트리밍 검색, 변경 감지, 안전한 추출, JSON Lines CLI.
- `Sources/RecoveryCore/`: 엔진 통신과 프로토콜 검증, ImageIO 검사.
- `Sources/RecoveryCard/`: SwiftUI 앱과 작업 상태.
- `docs/design.md`: 이번 범위와 후속 개발 단계.

서드파티 복구 엔진이나 LibRaw를 포함하지 않습니다. Rust 의존성과 라이선스는 `engine/Cargo.lock` 및 각 프로젝트를 참고하세요.
