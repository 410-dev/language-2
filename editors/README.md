# 에디터 지원

`.l2` 소스 파일용 문법 정의와 언어 서버 클라이언트입니다. 두 에디터 모두 같은 스코프 이름(`source.l2`, `keyword.control.*`, `storage.type.*` …)을 쓰므로 어떤 컬러 테마에서도 색이 입혀집니다.

| 경로 | 에디터 |
| --- | --- |
| [`vscode/`](vscode) | Visual Studio Code 확장: TextMate 문법, 언어 설정, 언어 서버 클라이언트 |
| [`sublime/`](sublime) | Sublime Text 4 (`.sublime-syntax` + 주석 단축키 설정) |

## 언어 서버

`language-2 lsp`가 언어 서버입니다 (사양 15.4). VS Code 확장이 자동으로 실행하며 다음을 제공합니다.

- **진단**: 문법, 타입, 소유권 오류와 경고. 입력을 잠시 멈추면 갱신하며, 문법 오류가 있어도 파일의 나머지를 계속 검사합니다.
- **호버**: 변수와 식의 타입, 함수와 메서드의 시그니처, 선언 위의 `//` 문서 주석.
- **정의로 이동** (F12): 지역 변수, 필드, 메서드, 함수, 생성자, 클래스, 모듈. 표준 라이브러리 선언은 읽기 전용 사본으로 열립니다.
- **자동완성**: `.` 뒤의 필드, 메서드, 내장 메서드(String, 숫자, 배열, Dictionary), 정적 멤버, 모듈의 함수와 타입. 범위 안의 지역 변수, 멤버, 함수, 타입, 키워드. `new` 뒤의 클래스, `using` 경로, `@` 지시자. f-string의 `{...}` 안에서도 동작합니다.
- **시그니처 도움말**: `(`나 `,`를 입력하면 오버로드 목록과 현재 인자를 보여 줍니다.
- **문서 기호**: 개요(Outline) 창과 `Ctrl+Shift+O`에 클래스, 멤버, 함수를 보여 줍니다.
- **참조 찾기** (`Shift+F12`, `Shift+Alt+F12`): 지역 변수는 그 파일에서, 필드·메서드·함수·클래스는 워크스페이스의 모든 `.l2` 파일에서 찾습니다. 커서를 두면 같은 파일 안의 같은 기호가 강조됩니다.
- **이름 바꾸기** (`F2`): 찾은 모든 곳을 한 번에 바꿉니다. 내장 멤버, 표준 라이브러리 선언, 파일 이름과 같은 클래스(모듈의 주 클래스)는 바꾸지 않습니다. 오버라이드한 메서드는 따로 바뀌지 않습니다.

## 설치

### VS Code

1. 툴체인을 빌드하고 SDK로 설치합니다. 실행 파일은 `%LOCALAPPDATA%\language-2\sdk\1\bin`(Windows) 또는 `~/.language-2/sdk/1/bin`에 복사되고, 확장은 이 위치를 자동으로 찾습니다.

    ```bash
    cargo build --release
    ```

    ```bash
    ./target/release/language-2 sdk install
    ```

2. 확장을 `.vsix` 패키지로 만듭니다. 언어 서버 클라이언트 라이브러리(`vscode-languageclient`)를 받기 위해 `npm install`이 필요합니다. 확장 폴더에 직접 복사하는 방식은 쓰지 마세요. 최신 VS Code는 `extensions.json`에 등록되지 않은 확장을 무시합니다.

    ```bash
    cd editors/vscode && npm install && npx @vscode/vsce package --allow-missing-repository --skip-license -o language-2-syntax.vsix
    ```

3. VS Code CLI로 설치하고, 열려 있는 창에서 *Developer: Reload Window*를 실행합니다.

    ```bash
    code --install-extension editors/vscode/language-2-syntax.vsix --force
    ```

`.l2` 파일을 열면 언어 서버가 시작됩니다. 서버 로그는 명령 팔레트의 *language-2: Show Language Server Output*에서 볼 수 있습니다.

#### 서버 실행 파일

확장은 다음 순서로 `language-2` 실행 파일을 찾습니다.

1. 설정 `language-2.server.path`: 실행 파일, 실행 파일이 든 폴더, SDK 버전 폴더(`.../sdk/1`), SDK 홈 폴더 중 하나. `%LOCALAPPDATA%`, `${env:NAME}`, `$NAME`, `~`를 펼칩니다. 찾지 못하면 경고를 띄우고 아래 순서로 찾습니다.
2. `PATH`의 `language-2`
3. SDK 홈(`$L2_HOME`, 기본값 `%LOCALAPPDATA%\language-2` 또는 `~/.language-2`)에 설치된 가장 높은 SDK 버전

Windows에서는 실행 중인 파일을 덮어쓸 수 없으므로, 확장은 서버를 확장 저장소에 복사한 사본으로 실행합니다. 그래서 에디터를 연 채로 `cargo build`나 `sdk install`을 다시 할 수 있습니다. 새 빌드는 *language-2: Restart Language Server*를 실행하면 적용됩니다. 컴파일러를 개발하는 중이라면 `language-2.server.path`를 `target/release/language-2.exe`로 지정해 두고, 빌드할 때마다 서버를 다시 시작하면 됩니다.

#### 설정

| 설정 | 기본값 | 설명 |
| --- | --- | --- |
| `language-2.server.path` | `""` | 언어 서버로 실행할 `language-2` 실행 파일 |
| `language-2.server.enabled` | `true` | 끄면 문법 강조만 남습니다 |
| `language-2.trace.server` | `"off"` | `messages` / `verbose`: 서버와 주고받는 메시지를 출력 창에 기록 |

### Sublime Text

`sublime` 폴더의 파일을 `Packages/User`(메뉴: *Preferences → Browse Packages…*)에 복사합니다. 별도의 재시작은 필요 없습니다.

```bash
cp editors/sublime/* "$APPDATA/Sublime Text/Packages/User/"
```

언어 서버 기능을 쓰려면 [LSP](https://packagecontrol.io/packages/LSP) 패키지를 설치하고, *Preferences → Package Settings → LSP → Settings*에 클라이언트를 추가합니다 (`language-2`가 `PATH`에 없으면 `command`에 전체 경로를 씁니다). 이 설정은 LSP 패키지의 일반 형식이며, 이 저장소에서는 VS Code로만 확인했습니다.

```json
{
  "clients": {
    "language-2": {
      "enabled": true,
      "command": ["language-2", "lsp"],
      "selector": "source.l2"
    }
  }
}
```

## 하이라이팅 대상

- 지시자: `@using`, `@runtime`, `@compiler(...)`, `@runtimecfg(...)`와 그 옵션/값, 일반 어노테이션(`@Override`)
- 키워드: 제어 흐름, 예외, `and`/`or`/`not`, 수정자(`Immutable`, `copied`, `getter`, `setter.chain` …)
- 타입: 내장 숫자 타입, `String`/`Dictionary`/`DTVariable` 등, 표준 예외 클래스, 대문자로 시작하는 사용자 타입
- 리터럴: 정수(16/2/8진수, `_` 구분자), 실수(지수 포함), 문자열 이스케이프(`\u{...}` 포함), `%name%` 자리표시자
- f-string: `{...}` 안의 식을 코드로 하이라이팅, `{{` `}}` 이스케이프
- 연산자: `**`, `??`, `->`, 참조 `&T`, 복합 대입 등

언어에 키워드나 타입을 추가하면 두 파일([`language-2.tmLanguage.json`](vscode/syntaxes/language-2.tmLanguage.json), [`language-2.sublime-syntax`](sublime/language-2.sublime-syntax))의 목록을 함께 갱신해야 합니다. 키워드의 기준은 [`crates/l2/src/lexer.rs`](../crates/l2/src/lexer.rs)의 `keyword()`입니다. 내장 멤버를 추가하면 자동완성용 표 [`crates/l2/src/ide/builtins.rs`](../crates/l2/src/ide/builtins.rs)도 고칩니다.
