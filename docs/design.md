# Recovery Card — 첫 구현

macOS에서 SD카드 이미지의 JPEG와 Fujifilm RAF를 읽기 전용으로 탐색하고
선택한 결과를 새 폴더에 추출하는 로컬 앱. 사용자가 승인한 전체 기획의 첫 단계다.

SwiftUI(macOS 14+, Swift 6) 앱은 Rust CLI를 자식 프로세스로 실행한다.
스캔 진행률과 결과는 JSON Lines, 내보내기 요청은 표준입력 JSON으로 전달한다.
파서는 별도 프로세스에서 실행하며 관리자 권한과 네트워크를 사용하지 않는다.

## 이번 버전

- 일반 파일인 `.img`/`.dd`/`.raw` 등 평면 디스크 이미지만 입력. 장치·디렉터리·FIFO 거부.
- 원본은 쓰기 권한 없이 열고, 수 MB 버퍼로 순차 검색. 전체 카드를 메모리에 올리지 않는다.
- JPEG 마커 길이와 entropy stuffing을 해석해 내장 썸네일의 EOI에서 잘못 자르지 않는다.
- RAF 헤더의 JPEG/metadata/CFA 범위와 내장 JPEG의 정확한 길이를 검사하고 연속 저장된 후보를 추출한다. 손상된 내장 JPEG의 RAF 구제는 후속 과제다.
- 완전한 RAF 안의 JPEG는 별도 사진으로 중복 표시하지 않는다.
- JPEG는 저장 후 제한된 크기의 전체 픽셀 디코딩 검사. RAF는 구조 확인만 하며 RAW 디코딩 미검증을 명시.
- 구조 검사와 실제 이미지 검증을 구분하고 손상 후보를 정상으로 표시하지 않는다.
- 실행 중 진행률, 중단, 결과 선택, 별도 세션 폴더 저장, SHA-256/원본 위치 보고서.
- 원본 메타데이터 변경 감지, 출력 덮어쓰기 금지, 잘못된 오프셋/길이 요청 거부.
- exFAT처럼 배타적 rename이 없는 저장장치는 create_new 복사로 저장. 완료 마커 제거 전에는 결과가 불완전할 수 있다.
- CPU/메모리/탐색 범위 상한, 한국어 UI, 키보드와 VoiceOver용 레이블.

## 후속 단계

실제 SD카드 복제와 ddrescue mapfile, exFAT/FAT32 메타데이터 복구,
LibRaw 전체 디코딩, HEIF, MOV/MP4 검증·복구, 단편화 재조립.
이 버전은 파일 시스템을 해석하지 않으므로 삭제 여부·원래 이름·원래 폴더를 알 수 없다.
진짜 X-T5 자료가 없는 상태의 합성 RAF 테스트를 카메라 호환성 검증으로 간주하지 않는다.

## 검증 기준

합성 이미지에서 원본 JPEG/RAF 바이트 일치, 경계에 걸친 시그니처 탐지,
JPEG 메타데이터 안의 썸네일·가짜 EOI 처리, 잘린 파일·악성 길이 거부,
소스 변경 거부, 중단 뒤 불완전 결과의 표시, 재실행 시 덮어쓰기 방지,
Swift 모델 테스트와 실제 앱 빌드·실행.

## 포맷 근거

- https://libopenraw.freedesktop.org/formats/raf/
- https://github.com/exiftool/exiftool/blob/master/lib/Image/ExifTool/FujiFilm.pm
- https://www.cgsecurity.org/wiki/PhotoRec
- https://www.gnu.org/software/ddrescue/manual/ddrescue_manual.html
