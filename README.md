# Recovery Card

macOS용 로컬 사진 복구 앱의 첫 프리뷰. **평면 SD카드 이미지 파일에서 연속 저장된 JPEG·RAF를 찾아 추출합니다.**

## 실행

이미 빌드된 앱: `dist/Recovery Card.app`

```sh
open 'dist/Recovery Card.app'
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
- `.dmg` 압축 컨테이너, 물리 SD카드 복제, exFAT/FAT32 메타데이터 복구, 단편화 재조립, HEIF·영상 복구는 아직 지원하지 않습니다.
- 합성 RAF로 구조 검사를 검증했습니다. **실제 X-T5 카드 복구율·압축 RAW 호환성은 아직 검증하지 않았습니다.**
- macOS 14+를 대상으로 Apple Silicon Mac에서 빌드·검증합니다. 로컬 ad hoc 서명이며 공증한 배포본이 아닙니다.

## 개발·검증

Rust stable(1.85+), Swift 6 이상과 macOS Command Line Tools가 필요합니다.
설치된 Swift 6.3.1과 Rust 1.99.1을 사용합니다. 셸 PATH에 cargo가 없다면 `$HOME/.cargo/bin/cargo`를 사용하세요.

```sh
cargo test --manifest-path engine/Cargo.toml
cargo clippy --manifest-path engine/Cargo.toml --all-targets -- -D warnings
swift scripts/make-demo.swift
bash scripts/build-app.sh
swift run RecoveryChecks --integration --broken-pipe --bad-json
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
