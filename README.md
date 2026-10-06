# language-2

> 가칭입니다. 언어 이름은 추후 변경될 예정이며, 이름은 한 곳(`crates/l2/src/lib.rs`의 `LANG_NAME`, 바이너리 이름은 `crates/l2/Cargo.toml`, 확장자는 `driver.rs`의 `SOURCE_EXT`)에서 바꿀 수 있습니다.

[SPEC.md](SPEC.md)(Draft v0.2)를 구현한 언어 처리기입니다. 문법은 Java/Python, 안전성 설계는 Rust를 따릅니다.
**하나의 프론트엔드**를 공유하는 **세 가지 실행 방식**을 모두 제공합니다 (사양 1.1, 15.1).

| 백엔드 | 명령 | 설명 |
| --- | --- | --- |
| 트리 워킹 인터프리터 | `run --backend interpreter` (기본) | 기준 구현 (사양 15.3) |
| 바이트코드 컴파일러 + VM | `run --backend bytecode` | 스택 기반 바이트코드, 예외 핸들러 테이블 |
| LLVM 네이티브 컴파일러 | `run --backend compiler`, `build` | LLVM IR → `opt`/`llc` → 링크, `i386`/`amd64`/`arm64` 크로스 컴파일 |

모든 테스트 프로그램은 세 백엔드에서 **출력·종료 코드가 동일**한지 차분 테스트로 검증됩니다.

## 빠른 시작

```bash
cargo build --release
```

```bash
./target/release/language-2 run examples/hello.l2 a b c
```

```bash
./target/release/language-2 run --backend bytecode examples/tour.l2
```

```bash
./target/release/language-2 run --backend compiler examples/tour.l2
```

네이티브 백엔드는 LLVM의 `llc`(선택적으로 `opt`)를 사용합니다. rustup의 `llvm-tools` 컴포넌트를 자동으로 찾습니다.

```bash
rustup component add llvm-tools
```

```bash
./target/release/language-2 doctor
```

### 명령어

```
language-2 run [--backend interpreter|bytecode|compiler] [--target i386|amd64|arm64] <file> [args...]
language-2 build <file> [-o <output>] [--target ...]... [-O0|-O1|-O2]
language-2 check <file>                 # 타입/소유권 검사만
language-2 emit-llvm <file> [--target amd64] [-o out.ll]
language-2 disasm <file>                # 바이트코드 덤프
language-2 doctor                       # 네이티브 툴체인/런타임 점검
```

`build`는 `@compiler(Target=[...])`에 나열된 **모든 타깃**에 대해 바이너리를 생성합니다 (사양 2.5). 해당 타깃용 런타임 라이브러리가 있으면 실행 파일까지 링크하고, 없으면 오브젝트 파일만 만듭니다. 예를 들어 32비트 x86용 런타임은 다음과 같이 준비합니다.

```bash
rustup target add i686-pc-windows-msvc
```

```bash
cargo build -p l2-native-rt --release --target i686-pc-windows-msvc
```

## 예제

```
@using sdk 1
@runtime compiler interpreter bytecode

using stdio as stdio

public class Resource implements Droppable {
    private String name
    public Resource(String name) { this.name = name }
    @Override
    public function void drop() { stdio.println("drop " + this.name) }
}

function (Int64, String) divide(Int64 a, Int64 b) {
    return a / b, f"{a} / {b}"
}

function void main(String[] args) {
    Resource r = Resource("file")
    q, text = divide(7, 2)
    stdio.println(f"{text} = {q}")          // 7 / 2 = 3
    String s = "abc"
    String t = s                            // 소유권 이동
    // stdio.println(s)                     // 컴파일 에러: use of moved value 's'
    try {
        Int8 x = 127
        x += 1                              // IntegerOverflow=error → ArithmeticException
    } catch (ArithmeticException e) {
        stdio.println(e.getMessage())
    }
}                                           // "drop file"
```

더 많은 예제는 [`examples/`](examples)와 [`tests/programs/`](tests/programs)에 있습니다.

## 구조

```
crates/
  l2-runtime/    공유 런타임: 값 모델(Value), BigInt, 연산자, 내장 함수(문자열/배열/Dictionary/stdio)
  l2-native-rt/  네이티브 실행 파일에 정적 링크되는 C ABI 런타임 (l2-runtime 재사용)
  l2/            컴파일러 라이브러리 + CLI
    lexer.rs     줄바꿈 기반 문장 종결 (사양 3.2)
    parser.rs    재귀 하향 파서 → AST
    check/       선언 수집, 이름 해석, 타입 검사, 제네릭 단형화 → HIR
    flow.rs      이동 후 사용, 확정 할당, 생성자 필드 초기화, 누락 return, 빌림 검사
    hir.rs       세 백엔드가 공유하는 타입 지정·단형화된 중간 표현
    interp.rs    트리 워킹 인터프리터
    bytecode.rs  바이트코드 컴파일러,  vm.rs  가상 머신
    llvm.rs      LLVM IR 생성,  native.rs  opt/llc/링커 구동
    prelude.l2   예외 계층·Droppable·Comparable (언어 자체로 작성)
tests/
  programs/      차분 테스트 프로그램 (*.l2, 기대 출력 *.out, 선택: *.err, *.in)
  errors/        컴파일 에러 테스트 (첫 줄 `// error: <메시지>`)
```

파이프라인 (사양 15.1):

```
소스 → 렉서 → 파서 → AST → 선언 수집/이름 해석/타입 검사/단형화(HIR) → 흐름·소유권 검사
                                                                      ├→ 트리 워킹 인터프리터
                                                                      ├→ 바이트코드 컴파일러 + VM
                                                                      └→ LLVM IR → opt/llc → 링크
```

### 백엔드 간 동일성 보장

- 세 백엔드는 같은 HIR을 입력으로 받고, 값 표현·연산자·내장 함수·문자열 변환을 **같은 Rust 코드(`l2-runtime`)** 로 수행합니다. 네이티브 코드도 숫자/Boolean만 레지스터에 두고 나머지 값은 `l2-runtime`의 `Value`를 박싱해 사용합니다.
- 정수 오버플로(`error`/`wrap`), 0 나누기, 부동소수점 반올림(Float32/Float16은 매 연산 후 해당 정밀도로 반올림)을 세 백엔드가 같은 규칙으로 처리합니다.
- 호출 깊이 제한(3000)도 동일하여 `StackOverflowError`가 같은 지점에서 발생합니다.
- 잡히지 않은 예외는 `Exception in thread "main" <클래스>: <메시지>`를 stderr에 출력하고 종료 코드 1을 반환합니다.

### 네이티브 백엔드

- LLVM IR을 **텍스트로 생성**하고 rustup `llvm-tools`의 `opt`/`llc`로 오브젝트 파일을 만든 뒤, Windows에서는 MSVC `link.exe`(없으면 `rust-lld`), 그 외에서는 `cc`로 런타임과 정적 링크합니다.
- 예외는 런타임의 대기(pending) 플래그로 전파하며, 호출 후 플래그를 검사해 catch/finally/스코프 해제 블록으로 분기합니다.
- 가상 호출은 메서드 셀렉터마다 생성되는 디스패치 함수(클래스 ID `switch`)로 처리합니다.

## 테스트

```bash
cargo test
```

- `interpreter`, `bytecode_vm`, `native_compiler`: `tests/programs`의 모든 프로그램을 각 백엔드로 실행해 stdout/stderr/종료 코드를 비교 (네이티브 툴체인이 없으면 건너뜀)
- `compile_errors`: `tests/errors`의 각 파일이 기대한 메시지로 컴파일에 실패하는지 확인
- manual 모드의 해제 후 접근은 네이티브에서 정의되지 않은 동작이므로 차분 테스트에서 제외합니다 (사양 15.3).
- `tools/fuzz_mutate.py <seed> <count> [native]`: 테스트 프로그램을 무작위로 변형해 컴파일러 패닉과 백엔드 간 출력 차이를 찾는 퍼저
- `tools/fuzz_arith.py <seed> <rounds>`: 모든 정수 폭·두 오버플로 정책에 대한 무작위 산술 프로그램을 세 백엔드로 비교하는 퍼저

## 사양 구현 현황

| 사양 | 내용 | 상태 |
| --- | --- | --- |
| 2 | `@using`, `@runtime`(백엔드 제한), `@compiler`(엔트리 전용, 기본값), `@runtimecfg`(파일 단위) | ✅ |
| 3 | 주석, 줄바꿈/세미콜론, 줄 이어짐 규칙, 리터럴, f-string, Dictionary 리터럴 | ✅ |
| 4.1–4.3 | 정수/부호 없는 정수/IntLarge, Boolean, Float16/32/64 (+소프트웨어 에뮬레이션 규칙) | ✅ |
| 4.4–4.6 | String(COW), 배열(`length_immutable`, `Immutable`), Dictionary(타입 인자 생략 시 DTVariable) | ✅ |
| 4.7–4.11 | 유니온, Nullable, DTVariable/STVariable, 함수 타입, 확대 변환/`castTo` | ✅ |
| 4.12 | 제네릭 함수·클래스·인터페이스, `extends` 제약, `[?]` 와일드카드, 단형화, 대괄호 구분 | ✅ |
| 5 | 재선언/섀도잉 금지, `Immutable`, `copied` | ✅ |
| 6 | 연산자 전부 (`&&`/`||`는 비트 연산, `**` 우선순위, `>>` 산술/논리, `?:`, `??`, 후위 `!`, `&`/`*`) | ✅ |
| 7 | Boolean 조건, if, switch(`as`, `fallthrough`), for 6가지 형태, 예외/checked exception | ✅ |
| 8 | 반환 타입 필수, 다중 반환, 이름 있는 인자, 오버로딩(DTVariable 규칙), 람다/`move` | ✅ |
| 9 | 복사/이동 타입, 참조 `&`/`*`, `copied`, 참조 반환 규칙, `Droppable`/해제 순서, manual 모드/`free` | ✅ (빌림 검사는 단순화) |
| 10 | 클래스/상속/인터페이스/`default`/`= origin`/`super(X)`, 생성자 규칙, getter/setter | ✅ |
| 11–13 | String/숫자/배열/Dictionary 메서드, 음수 인덱스, 부호 없는 인덱스 금지 | ✅ |
| 14 | `using`(파일/함수 스코프), 순환 import, 정적 초기화 순환 검출, stdio | ✅ |
| 15 | 세 백엔드, 크로스 컴파일, 차분 테스트 | ✅ |

## 사양 해석 및 결정 사항

사양에 명시되지 않았거나 모호한 부분은 다음과 같이 정했습니다. 사양 확정 시 조정이 필요할 수 있습니다.

- **배열 리터럴 `[a, b, c]`**: Dictionary 리터럴이 JSON 호환(사양 3.3, 4.6)이므로 JSON 배열을 표현하기 위해 지원합니다. 일반 식에서도 사용할 수 있으며 결과는 길이 가변 배열입니다. 원소 타입은 기대 타입에서, 없으면 원소들로부터 추론합니다.
- **`copied` 필드**: 필드가 이동의 원천이 되면(예: `return this.name`) 자동으로 `.clone()`됩니다 (사양 9.4, 10.5).
- **정수 리터럴 타입**: 기대 타입이 있으면 그 타입(범위 검사), 없으면 `Int32`(넘치면 `Int64`). 실수 리터럴 기본은 `Float64`.
- **작은 정수 연산**: Java의 int 승격을 하지 않습니다. `Int8 + Int8`은 `Int8`이며 오버플로 정책이 적용됩니다. 서로 다른 정수 타입은 손실 없는 공통 타입으로 승격합니다(공통 타입이 없으면 `castTo` 필요). 정수와 실수의 혼합 연산은 Java처럼 실수로 승격합니다.
- **시프트**: Java처럼 시프트 양을 비트 수로 마스킹하며 오버플로 검사 대상이 아닙니다.
- **`==`의 비교 가능성**: 서로 관련 없는 타입(`Int`와 `String` 등)의 비교는 컴파일 에러입니다.
- **문자열 결합(`+`)과 f-string**: 피연산자를 빌려 읽으므로 이동이 일어나지 않습니다.
- **필드/배열 원소에서의 이동**: Rust처럼 금지합니다(`.clone()` 또는 `remove()` 사용). getter는 이동 타입 필드를 `&T`로 돌려줍니다(사양 10.6).
- **`&T` 매개변수**: 임시값과 변수 모두 자동으로 빌려 전달합니다. `*T`는 반드시 `*x`로 명시합니다.
- **`switch`의 `break`**: Java처럼 switch를 빠져나갑니다. `default`는 위치와 무관하게 다른 case가 모두 거짓일 때 실행됩니다.
- **for-each 변수**: 이동 타입 원소는 읽기 전용 참조(`&T`)로 바인딩됩니다.
- **Nullable 스마트 캐스트**: `if (x != null) {...}` 안, `if (x == null) { return }` 이후에는 `x`를 non-null로 취급합니다 (해당 블록에서 `x`에 대입하지 않는 경우).
- **다중 대입 `a, b = f()`**: 선언되지 않은 이름은 새 지역 변수로 선언됩니다.
- **Dictionary**: 삽입 순서를 유지합니다. 없는 키 접근은 `IllegalArgumentException`. Java 기본 동작에 맞춰 `length()`, `isEmpty()`, `containsKey()`, `keys()`, `values()`, `get()`, `set()`, `remove()`도 제공합니다.
- **문자열 표현**: 객체 기본 `toString`은 `Name(f=v, ...)`, 예외는 `Name: message`, 컨테이너 안의 문자열은 따옴표로 감쌉니다. 실수는 Java의 `Double.toString` 형식(`1.0`, `1.0E10`)을 따릅니다.
- **`.format(whole, decimal)`**: `whole`은 최소 정수부 자릿수(0으로 채움), `decimal`은 소수부 자릿수, `-1`은 제한 없음.
- **`.format()` 주입 방지**: 수신자가 문자열/f-string 리터럴이면 컴파일 시점에 리터럴 부분의 `%이름%`만 치환합니다. 변수에 담긴 문자열은 출처 정보가 없으므로 전체를 대상으로 치환합니다.
- **`String.random(regex, ...)`**: 정규식의 첫 문자 집합(`[a-z0-9]`, `\d`, `\w`, `.` 또는 리터럴 문자)에서 길이 `[min, max]`의 문자열을 생성합니다. 난수 시드는 `L2_SEED` 환경 변수로 고정할 수 있습니다.
- **`Float16` 하드웨어 지원**: 지원 타깃 중 `arm64`만 하드웨어 지원으로 간주합니다. 즉 `Target`이 `arm64`만이 아니면 `EnableSoftwareEmulation=true`가 필요합니다.
- **`Comparable`**: 표식 인터페이스이며 구현 클래스는 `Int32 compareTo(other)`를 정의해야 합니다. 숫자·String은 기본적으로 Comparable입니다.
- **추가 예외 클래스**: `UnsupportedOperationException`(길이 고정 배열의 insert/remove), `UseAfterFreeError`(manual 모드 해제 후 접근, 인터프리터/VM에서 감지).
- **`main`**: `function void main(String[] args)`, `function void main()`, 또는 `Int32`를 반환하면 종료 코드로 사용합니다.
- **모듈**: 파일이 곧 모듈이며 `using a.b as x`는 엔트리 파일 기준 `a/b.l2`를 읽습니다. 함수는 모듈 별칭으로 접근하고, 클래스·인터페이스 이름은 프로젝트 전역입니다. `protected`는 Java의 패키지처럼 같은 모듈 안에서도 접근할 수 있습니다.
- **람다**: 지역 변수를 빌려 캡처한 람다는 반환하거나 필드에 저장할 수 없습니다(`move` 필요).

## 현재 제한 사항

- **LLVM 바인딩**: 사양은 Rust 바인딩 사용을 명시하지만, 이 환경에는 LLVM 개발 라이브러리가 없어 IR을 텍스트로 생성합니다. 코드 생성기는 IR 문자열만 만들기 때문에 `inkwell` 등으로 교체하기 쉽게 분리되어 있습니다. lld와 런타임을 컴파일러와 함께 배포하는 패키징(사양 2.5 구현 노트)은 아직 하지 않았습니다.
- **크로스 링크**: 오브젝트 파일은 모든 타깃용으로 생성됩니다. 실행 파일 링크에는 해당 타깃용 `l2-native-rt` 정적 라이브러리와 링커(Windows: 해당 아키텍처의 MSVC 라이브러리)가 필요합니다.
- **제네릭 본문 검사**: 단형화 방식이라 제네릭 본문은 인스턴스화될 때(구체 타입으로) 검사됩니다. 사용되지 않은 제네릭은 검사되지 않습니다. 클래스의 제네릭 메서드(`function T f[T](...)`)는 지원하지만 가상 호출 대상이 아니며(오버라이드 불가), 인터페이스의 제네릭 메서드는 아직 지원하지 않습니다.
- **빌림 검사**: 이동 후 사용·미할당 사용은 흐름 분석으로 정확히 검사하지만, 빌림 충돌은 (1) 같은 호출 안의 `*x`와 다른 사용, (2) 참조 변수가 살아 있는 동안 원본의 이동/대입/변경만 검사하는 단순화된 형태입니다.
- **IntegerOverflow 범위 순환**: `T.range(a, b, step)`의 마지막 증가가 타입 범위를 넘으면 정책(`error`)에 따라 예외가 날 수 있습니다.
- **패키지/외부 라이브러리**(사양 16.1, 9.8), `Shared[T]`, 표준 라이브러리 확장은 추후 작업입니다. `IncludeDependencies` 옵션은 파싱만 합니다.
- **성능**: 네이티브 코드는 숫자 연산·제어 흐름을 직접 컴파일하지만, 문자열·배열·객체 필드 접근은 런타임 호출을 거칩니다.
