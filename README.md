# iko PDF 변환

탐색기에서 이미지나 PDF를 **우클릭**해서 바로 변환·나누기·합치기를 하는 Windows용 프로그램입니다.
설치 프로그램 없이 실행 파일 하나(`ikoPDF.exe`)로 동작합니다.

만든 곳: [event_code 블로그](https://blog.naver.com/event_code) — 일은 줄이고, 돈은 벌자!

## 받기

**[최신 버전 내려받기 (ikoPDF.exe)](https://github.com/PortPapa/ikoPDF/releases/latest/download/ikoPDF.exe)**

1. `ikoPDF.exe` 를 실행합니다. 처음에는 Windows가 "PC 보호" 화면을 띄울 수 있습니다 → **추가 정보 → 실행**.
2. 설정 창에서 **우클릭 메뉴 등록**을 누릅니다.
3. 이미지나 PDF 파일을 우클릭 → **iko PDF 변환**.

등록하면 프로그램이 `%LOCALAPPDATA%\Programs\ikoPDF` 로 복사되므로, 내려받은 파일은 지워도 됩니다.
관리자 권한은 필요 없습니다.

## 기능

| 선택한 파일 | 메뉴 | 결과 |
|---|---|---|
| 이미지 | PDF로 변환 | 이미지마다 `이름.pdf` |
| PDF | JPG로 변환 / PNG로 변환 | 1쪽이면 `이름.jpg`, 여러 쪽이면 `이름\이름_001.jpg …` (200 DPI) |
| PDF (2쪽 이상) | 페이지별로 나누기 | `이름\이름_001.pdf …` |
| 이미지·PDF 2개 이상 | 하나의 PDF로 합치기… | 순서 조정 창 → `첫파일이름_합본.pdf` |
| 아무 파일 | 설정… | 메뉴 해제, 업데이트 |

- 결과물은 원본과 같은 폴더에 생기고, 같은 이름이 있으면 ` (2)` 를 붙여 덮어쓰지 않습니다.
- 지원 이미지: jpg, jpeg, jpe, jfif, png, bmp, gif, tif, tiff, webp
- JPG는 다시 압축하지 않고 PDF에 그대로 넣습니다. 휴대폰 사진의 회전 정보를 반영합니다.
- 암호가 걸린 PDF는 처리하지 않습니다. 합치기/나누기는 책갈피(목차)를 옮기지 않습니다.

## 메뉴가 뜨는 위치

- **Windows 11 새 우클릭 메뉴(첫 화면)** 에 등록을 먼저 시도합니다. 이를 위해 프로그램의 인증서가
  *현재 사용자 계정*의 "신뢰할 수 있는 사용자" 저장소에 추가됩니다.
- 회사 보안 정책 등으로 막히면 자동으로 **기존 메뉴**(Windows 11에서는 우클릭 → "추가 옵션 표시")에 등록합니다.

설정 창의 **우클릭 메뉴 해제**를 누르면 메뉴, 인증서, 자동 실행, 설치 폴더가 모두 지워집니다.

## 업데이트

메뉴를 등록하면 "Windows 시작 시 자동으로 업데이트 확인"이 켜집니다(설정 창에서 끌 수 있음).
로그인할 때 창 없이 잠깐 실행되어 이 저장소의 최신 릴리스를 확인하고, 새 버전이 있으면 받아서 교체합니다.
설정 창의 **지금 업데이트 확인**으로 바로 확인할 수도 있습니다.

## 직접 빌드하기

Rust(MSVC)와 Windows SDK가 필요합니다. 아이콘은 `scripts\make-assets.ps1` 이 만듭니다(메뉴 아이콘, 이코 캐릭터 프로그램 아이콘).

```powershell
cargo test -p ikopdf                                                   # 변환 테스트
powershell -ExecutionPolicy Bypass -File scripts\release.ps1 -SkipPublish   # target\release\ikoPDF.exe 만들기
```

- `crates/converter` — `ikoPDF.exe`. 변환(`img2pdf`, `pdf2img`, `pdfops`), 자체 설치·메뉴 등록(`setup`), 업데이트(`update`).
  - 화면은 `ui/*.html` (event_code 블로그 스타일)이고 Windows에 들어 있는 WebView2로 보여 줍니다(`webui`, `settings_web`, `order_web`).
    `ui` 폴더를 `python -m http.server` 로 열면 브라우저에서 바로 미리 볼 수 있습니다.
  - WebView2를 쓸 수 없는 PC에서는 기본 Windows 창(`settings_dialog`, `order_dialog`)이 대신 뜹니다.
- `crates/shell-ext` — `ikopdf_shell.dll`. 우클릭 메뉴(`IExplorerCommand`). 고른 파일 목록을 exe에 넘기기만 합니다.
  exe 안에 함께 들어가므로 **DLL을 먼저 빌드**해야 합니다(`release.ps1` 이 순서대로 빌드).
- `package/AppxManifest.xml` — 새 메뉴용 패키지 선언. 바꾸면 Version을 올리고 `scripts\make-package.ps1` 로
  `package/dist/ikoPDF.msix` 를 다시 만듭니다(서명 인증서는 만든 사람의 Windows 계정에만 있습니다).

### 새 버전 내기

1. `Cargo.toml` 의 `version` 을 올립니다.
2. `powershell -ExecutionPolicy Bypass -File scripts\release.ps1` — 빌드해서 GitHub 릴리스에 `ikoPDF.exe` 와 `version.txt` 를 올립니다.
