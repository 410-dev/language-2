# 에디터 신택스 하이라이팅

`.l2` 소스 파일용 문법 정의입니다. 두 에디터 모두 같은 스코프 이름(`source.l2`, `keyword.control.*`, `storage.type.*` …)을 쓰므로 어떤 컬러 테마에서도 색이 입혀집니다.

| 경로 | 에디터 |
| --- | --- |
| [`vscode/`](vscode) | Visual Studio Code 확장 (TextMate 문법 + 언어 설정) |
| [`sublime/`](sublime) | Sublime Text 4 (`.sublime-syntax` + 주석 단축키 설정) |

## 설치

### VS Code

`.vsix` 패키지로 만든 뒤 VS Code CLI로 설치합니다. 확장 폴더에 직접 복사하면 최신 VS Code는 `extensions.json`에 등록되지 않은 확장을 무시하므로 하이라이팅이 적용되지 않습니다.

```bash
cd editors/vscode && npx @vscode/vsce package --allow-missing-repository --skip-license -o language-2-syntax.vsix
```

```bash
code --install-extension editors/vscode/language-2-syntax.vsix --force
```

설치 후 열려 있는 VS Code 창에서 *Developer: Reload Window*를 실행합니다.

### Sublime Text

`sublime` 폴더의 파일을 `Packages/User`(메뉴: *Preferences → Browse Packages…*)에 복사합니다. 별도의 재시작은 필요 없습니다.

```bash
cp editors/sublime/* "$APPDATA/Sublime Text/Packages/User/"
```

## 하이라이팅 대상

- 지시자: `@using`, `@runtime`, `@compiler(...)`, `@runtimecfg(...)`와 그 옵션/값, 일반 어노테이션(`@Override`)
- 키워드: 제어 흐름, 예외, `and`/`or`/`not`, 수정자(`Immutable`, `copied`, `getter`, `setter.chain` …)
- 타입: 내장 숫자 타입, `String`/`Dictionary`/`DTVariable` 등, 표준 예외 클래스, 대문자로 시작하는 사용자 타입
- 리터럴: 정수(16/2/8진수, `_` 구분자), 실수(지수 포함), 문자열 이스케이프(`\u{...}` 포함), `%name%` 자리표시자
- f-string: `{...}` 안의 식을 코드로 하이라이팅, `{{` `}}` 이스케이프
- 연산자: `**`, `??`, `->`, 참조 `&T`, 복합 대입 등

언어에 키워드나 타입을 추가하면 두 파일([`language-2.tmLanguage.json`](vscode/syntaxes/language-2.tmLanguage.json), [`language-2.sublime-syntax`](sublime/language-2.sublime-syntax))의 목록을 함께 갱신해야 합니다. 키워드의 기준은 [`crates/l2/src/lexer.rs`](../crates/l2/src/lexer.rs)의 `keyword()`입니다.
