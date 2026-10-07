# 언어 사양서 (Draft v0.2)

> 문법은 Java/Python, 안전성 및 백엔드 설계는 Rust를 기준으로 한다.
> 본 문서에서 명시하지 않은 문법과 동작은 원칙적으로 Java를 따른다.

---

## 0. v0.1 대비 변경 사항

- 함수 반환 타입 명시 필수 (`void` 포함)
- 같은 스코프 재선언 및 바깥 지역 변수 섀도잉 금지
- `==`는 `.equals()`의 축약 (값 비교), 참조 비교는 `.isSameReferenceWith()`
- `protected` 접근 제어자 추가 (Java와 동일)
- 인터페이스 선언 문법 확정, `default` 메서드 지원
- 소멸자: `Droppable` 인터페이스의 `drop()`
- 부모 생성자 호출: Java와 동일 (`super(...)`)
- 예외 클래스 계층 및 checked exception 확정 (Java와 동일)
- 타입 인자 없는 `Dictionary` = `Dictionary[DTVariable, DTVariable]`
- `DataClass` 인터페이스 폐기 (`getter`/`setter` 수정자로 대체)
- 배열 기본 메서드 확정, 음수 인덱스, 대괄호 인덱싱, 부호 없는 인덱스 금지
- `>>>` 없음. `>>`는 피연산자 타입에 따라 산술/논리 시프트
- 참조 반환은 `this`에서 빌린 경우만 허용 (확장하지 않음, 확정)
- 사용자 정의 제네릭: 대괄호 문법 `[T]`, 와일드카드 `[?]`
- `@compiler` 기본값 확정, 엔트리 파일 전용, 크로스 컴파일 지원
- 연산자 우선순위 확정 (비트 연산이 비교보다 우선, `**`는 왼쪽 단항 `-`보다 우선)

---

## 1. 개요

### 1.1 설계 원칙
- 문법: Java와 Python을 기본으로 한다.
- 메모리 안전성: Rust의 소유권 모델을 단순화하여 채택한다.
- Null 안전성: 기본적으로 Null을 허용하지 않으며, nullable은 타입에 명시한다.
- 조건식은 반드시 Boolean이어야 한다 (truthiness 없음).
- 하나의 프론트엔드를 공유하는 세 가지 실행 방식(인터프리터, 바이트코드 VM, 네이티브 컴파일러)을 지원한다.

### 1.2 구현
- 구현 언어: Rust
- 네이티브 백엔드: LLVM IR 생성 (Rust 바인딩 사용)
- 지원 타깃: `i386`, `amd64`, `arm64`

---

## 2. 파일 구조와 지시자

### 2.1 일반 소스 파일
```
@using sdk 1
@runtimecfg(
    IntegerOverflow=error
)
```

### 2.2 엔트리 파일 (`main`이 있는 파일)
```
@using sdk 1
@runtime compiler interpreter bytecode
@compiler(
    IncludeDependencies=false
    Target=[i386, amd64, arm64]
    EnableSoftwareEmulation=false
    MemoryManagement=ownership
)
@runtimecfg(
    IntegerOverflow=error
)

function void main(String[] args) {
}
```

### 2.3 `@using`
- 사용할 SDK와 버전을 지정한다.

### 2.4 `@runtime`
- 이 프로그램이 지원하는 실행 방식을 나열한다: `compiler`, `interpreter`, `bytecode`.

### 2.5 `@compiler`
- **엔트리 파일에만 선언할 수 있다.** 다른 파일에 선언하면 컴파일 에러.
- 모든 옵션은 프로젝트 전체에 적용된다.
- 옵션 구분자는 줄바꿈 또는 쉼표.

| 옵션                      | 값                              | 기본값                 | 설명                                              |
| ------------------------- | ------------------------------- | ---------------------- | ------------------------------------------------- |
| `IncludeDependencies`     | `true` / `false`                | `false`                | 의존성 포함 여부                                  |
| `Target`                  | `i386`, `amd64`, `arm64`의 목록 | `[i386, amd64, arm64]` | 네이티브 컴파일 타깃                              |
| `EnableSoftwareEmulation` | `true` / `false`                | `false`                | 하드웨어 미지원 기능의 소프트웨어 에뮬레이션 허용 |
| `MemoryManagement`        | `ownership` / `manual`          | `ownership`            | 메모리 관리 정책                                  |

**Target 동작**
- 컴파일러는 `Target`에 나열된 모든 플랫폼용 바이너리를 생성한다 (크로스 컴파일).
- 컴파일러가 실행 중인 플랫폼이 목록에 없어도 컴파일은 가능하다.
- `EnableSoftwareEmulation`은 네이티브 컴파일에만 의미가 있다. 인터프리터와 바이트코드 VM은 항상 소프트웨어로 처리한다.
- 구현 노트: 타깃별 링크를 위해 LLVM 링커(lld)를 컴파일러와 함께 배포하고 런타임 라이브러리는 정적 링크한다.

### 2.6 `@runtimecfg`
| 옵션              | 값               | 기본값  | 설명                    |
| ----------------- | ---------------- | ------- | ----------------------- |
| `IntegerOverflow` | `error` / `wrap` | `error` | 정수 오버플로 처리 정책 |

- 파일 단위로 적용된다. 각 파일의 코드는 해당 파일의 정책을 따른다.
- 세 백엔드는 동일한 정책에서 동일한 결과를 보장해야 한다.

---

## 3. 어휘 구조

### 3.1 주석
```
// 한 줄 주석
/* 여러 줄 주석 */
/**
 문서화 주석 (Javadoc 형식)
**/
```

### 3.2 문장 종결
- 줄바꿈이 문장을 종결한다.
- 세미콜론 `;`은 선택 사항이다. 한 줄에 여러 문장을 쓸 때 사용할 수 있다.
- 다음 경우 줄바꿈은 문장을 종결하지 않고 다음 줄로 이어진다:
  - 괄호 `(`, `[`, `{`가 닫히지 않은 경우
  - 줄이 이항 연산자로 끝나는 경우
  - 줄이 쉼표로 끝나는 경우
- 단, `using pkg.*`의 `*`는 줄을 잇지 않는다.

### 3.3 리터럴
| 종류        | 예시                                    |
| ----------- | --------------------------------------- |
| 정수        | `0`, `42`, `-7`                         |
| 실수        | `1.0`, `3.14`                           |
| Boolean     | `true`, `false`                         |
| Null        | `Null`, `null` (동일)                   |
| 문자열      | `"text"`                                |
| 포맷 문자열 | `f"name={name}"`                        |
| Dictionary  | `{"a": "b", "c": {"d": 1}}` (JSON 호환) |

---

## 4. 타입 시스템

### 4.1 정수 타입
| 타입       | 크기       | 비고               |
| ---------- | ---------- | ------------------ |
| `Int8`     | 8bit       |                    |
| `Int16`    | 16bit      |                    |
| `Int32`    | 32bit      |                    |
| `Int64`    | 64bit      | 모든 타깃에서 지원 |
| `IntLarge` | 가변       | 임의 정밀도 정수   |
| `Int`      | = `Int32`  | 별칭               |
| `Byte`     | = `Int8`   | 별칭               |
| `UInt8`    | 8bit       |                    |
| `UInt16`   | 16bit      |                    |
| `UInt32`   | 32bit      |                    |
| `UInt64`   | 64bit      | 모든 타깃에서 지원 |
| `UInt`     | = `UInt32` | 별칭               |

- 1/2/4비트 정수 타입은 존재하지 않는다.

### 4.2 Boolean
- `Boolean`: `true` / `false`.
- 숫자 타입과 상호 변환되지 않는다.

### 4.3 부동소수점 타입
| 타입      | 크기        | 비고                                                         |
| --------- | ----------- | ------------------------------------------------------------ |
| `Float16` | 16bit       | 하드웨어 미지원 타깃에서는 `EnableSoftwareEmulation=true` 필요 |
| `Float32` | 32bit       |                                                              |
| `Float64` | 64bit       |                                                              |
| `Float`   | = `Float32` | 별칭                                                         |

- 부호 없는 부동소수점 타입은 존재하지 않는다.
- `Target`의 어느 플랫폼이라도 하드웨어로 지원하지 않는 타입을 사용하면, `EnableSoftwareEmulation=true`가 없는 한 컴파일 에러.

### 4.4 String
- 유니코드 문자열. 이동 타입 (9장 참조).
- 내부 구현은 copy-on-write.

### 4.5 배열
```
Int64[] arr = Int64.array(length = 10, length_immutable = true)  // 길이 고정, 내용 변경 가능
Int64[] arr = Int64.array()                                      // 길이 가변
Immutable Int64[] arr = ...                                      // 길이와 내용 모두 불변
```
- `length_immutable = true`: 길이만 고정 (Java 배열과 동일).
- `Immutable T[]`: 내용과 길이 모두 불변 (Python 튜플과 동일).
- 배열 메서드는 13장 참조.

### 4.6 Dictionary
```
Dictionary[String, String|Int64|Dictionary[String, DTVariable]] d = {
    "a": "b",
    "n": 1,
    "d": {"x": "y"}
}

Dictionary loose = {"a": 1, "b": "text"}   // Dictionary[DTVariable, DTVariable]
```
- 타입 인자를 쓸 때는 반드시 두 개 (키, 값).
- 타입 인자를 생략하면 `Dictionary[DTVariable, DTVariable]`과 동일하다.
- JSON과 호환되는 리터럴을 사용하며, 리터럴은 선언된 타입에 대해 타입 검사된다.

### 4.7 유니온 타입
- `A|B`: A 또는 B.

### 4.8 Nullable 타입
- `T?`: T 또는 Null.
- Null이 아닌 타입에는 Null을 대입할 수 없다.
- nullable 값은 조건식에 직접 사용할 수 없다. `x != null` 비교를 사용한다.

### 4.9 동적 타입과 타입 추론
| 키워드       | 의미                                                         |
| ------------ | ------------------------------------------------------------ |
| `DTVariable` | 동적 타입. 어떤 타입의 값이든 담을 수 있으며 런타임에 타입 태그를 가진다. |
| `STVariable` | 정적 타입 추론. 초기값의 타입으로 고정된다.                  |

```
STVariable x = 1       // Int32로 고정
DTVariable z = 1
z = "text"             // 허용
```

### 4.10 함수 타입
```
Function[(Int32, Float32), void] f
```
- 형식: `Function[(매개변수 타입들), 반환 타입]`

### 4.11 타입 변환
- 손실 없는 확대 변환은 암시적으로 허용한다 (Java 규칙): 예) `Int32` → `Int64`, `Int32` → `Float64`.
- 손실 가능한 축소 변환은 `.castTo(T)`로 명시한다.
```
  Int64 big = 300
  Int8 small = big.castTo(Int8)
```
- 정수 축소 변환에서 범위를 넘으면 파일의 `IntegerOverflow` 정책을 따른다.
- `DTVariable`에서 구체 타입으로의 변환은 `.castTo(T)`로 하며, 실제 값의 타입이 맞지 않으면 `ClassCastException`.

### 4.12 제네릭
```
function T identity[T](T x) {
    return x
}

public class Box[T extends Comparable] {
    private getter T value
}

function void printAll(List[?] items) { ... }

identity[Int64](5)     // 명시적 타입 인자
identity(5)            // 타입 추론

public class Tensor[T extends Numeric = Float64] { ... }   // 기본 타입 인자
Tensor t = ...         // Tensor[Float64]
t.migrate[Int32]()     // 제네릭 메서드의 명시적 타입 인자
```
- 타입 매개변수는 대괄호로 선언한다. 함수는 이름 뒤, 클래스와 인터페이스는 이름 뒤에 둔다.
- 제약은 Java처럼 `extends`로 표기한다.
- `Numeric` 제약: 모든 내장 숫자 타입(`Int8`~`Int64`, `UInt8`~`UInt64`, `IntLarge`, `Float16`~`Float64`)이 만족한다. 클래스는 `Numeric`을 구현할 수 없다. `T extends Numeric`인 코드에서는 `T`에 산술·비교 연산자와 숫자 메서드를 쓸 수 있고, 정수 리터럴을 `T`에 대입할 수 있다 (`T zero = 0`).
- `Comparable` 제약: 숫자, `String`, `Boolean`, 그리고 `compareTo`를 가진 클래스가 만족한다.
- 기본 타입 인자 `T = Float64`: 타입 인자를 생략하면 기본값을 쓴다 (`Tensor` = `Tensor[Float64]`). 생성 시에는 명시된 인자 → 기대 타입(`Matrix[Int32] m = new Matrix(...)`) → 리터럴이 아닌 생성자 인자로부터의 추론 → 기본값 순서로 정한다. 리터럴(`[[1, 2]]`, `5`)은 기본값이 있는 타입 매개변수를 정하지 않고 그 타입에 맞춰진다.
- 제약을 만족하지 않는 타입 인자로는 인스턴스를 만들지 않는다 (컴파일 에러).
- 제네릭 메서드는 인자에서 타입을 추론하거나, `x.method[T](...)`로 명시한다. 같은 이름의 제네릭 메서드는 매개변수 개수로 구분한다.
- `[?]` 와일드카드는 이름 없는 타입 매개변수로 처리된다. `printAll(List[?] items)`는 `printAll[T](List[T] items)`와 동일하다.
- 구현은 단형화(monomorphization) 방식이다. 사용된 타입 인자마다 별도의 코드를 생성한다 (Java의 type erasure와 다름).
- 대괄호 구분 규칙:
  - 타입 위치에서 빈 대괄호 `T[]`는 배열, 내용이 있는 대괄호 `T[A, B]`는 타입 인자.
  - 식 위치에서 `x[...]`는 `x`가 제네릭 함수/타입이면 타입 인자 적용, 그 외에는 인덱싱. 이름 해석 단계에서 결정한다.

---

## 5. 변수와 불변성

### 5.1 선언
```
Int64 a = 1
Immutable String id = "x"
copied String s = "abcd"
```
- 수정자의 순서는 자유다.
- 같은 스코프에서 같은 이름을 다시 선언할 수 없다.
- 안쪽 블록에서 바깥 지역 변수와 같은 이름을 선언할 수 없다 (Java와 동일).

### 5.2 `Immutable`
- 재대입 불가, 내용 변경 불가.
- 선언 시점의 결정이 최우선이다. `Immutable String`에 `.append()` 같은 변경 메서드를 호출하면 컴파일 에러.
- `Immutable` 필드는 생성자에서 정확히 한 번 대입해야 한다 (Java `final`과 동일).
- `Immutable T?` 필드도 생성자에서 명시적으로 정확히 한 번 대입해야 한다.

### 5.3 `copied`
- 9.4절 참조.

---

## 6. 연산자

연산자 동작은 별도 명시가 없는 한 Java를 따른다.

### 6.1 산술
| 연산자 | 의미             | 비고                                                         |
| ------ | ---------------- | ------------------------------------------------------------ |
| `+`    | 덧셈             | 문자열 연결 포함                                             |
| `-`    | 뺄셈 / 단항 부호 |                                                              |
| `*`    | 곱셈             |                                                              |
| `/`    | 나눗셈           | 정수끼리는 0 방향 절삭 (`5 / 2 == 2`), 실수 포함 시 실수 나눗셈 |
| `%`    | 나머지           | 부호는 피제수를 따름 (`-7 % 3 == -1`)                        |
| `**`   | 거듭제곱         | 우결합. 정수 밑에 음수 지수는 `ArithmeticException`          |

- 정수 연산 오버플로는 파일의 `IntegerOverflow` 정책을 따른다 (`error`일 때 `ArithmeticException`).
- 정수 0 나누기는 `ArithmeticException`.
- 정책과 무관하게 순환 연산이 필요하면 명시적 메서드를 사용한다: `a.addWrap(b)`, `a.subWrap(b)`, `a.mulWrap(b)`.

### 6.2 비교와 동등성
| 연산자               | 의미                                     |
| -------------------- | ---------------------------------------- |
| `==`, `!=`           | 값 비교. `a == b`는 `a.equals(b)`의 축약 |
| `<`, `>`, `<=`, `>=` | 크기 비교                                |

- 클래스의 기본 `equals()`는 모든 필드를 `==`로 비교한다.
- `equals()`를 오버라이드하면 `==`도 오버라이드된 구현을 사용한다.
```
  @Override
  public function Boolean equals(&Human other) {
      return this.socialSecurity == other.socialSecurity()
  }
```
- 참조 비교: `a.isSameReferenceWith(b)`. 두 값이 같은 객체를 가리키는지 검사한다.

### 6.3 논리 (Boolean 전용, 단락 평가)
| 연산자            | 의미     |
| ----------------- | -------- |
| `and`             | 논리 AND |
| `or`              | 논리 OR  |
| `not`, `!` (전위) | 논리 NOT |

### 6.4 비트 (정수 전용)
| 연산자 | 의미          |
| ------ | ------------- |
| `&&`   | 비트 AND      |
| `\|\|` | 비트 OR       |
| `^`    | 비트 XOR      |
| `~`    | 비트 NOT      |
| `<<`   | 왼쪽 시프트   |
| `>>`   | 오른쪽 시프트 |

- **Java와 다른 점**: `&&`, `||`는 논리 연산자가 아니라 비트 연산자다. `&`, `|`는 비트 연산자로 쓰이지 않는다.
- `&&`, `||`에 Boolean 피연산자를 쓰면 컴파일 에러.
- `>>`는 부호 있는 타입에서 산술 시프트(부호 유지), 부호 없는 타입에서 논리 시프트(0 채움)로 동작한다.
- `>>>` 연산자는 존재하지 않는다. 0 채움이 필요하면 부호 없는 타입으로 변환 후 시프트한다.

### 6.5 조건 및 Null 관련
| 연산자           | 의미           | 예시                          |
| ---------------- | -------------- | ----------------------------- |
| `? :`            | 삼항           | `v = x == 10 ? a : b`         |
| `??`             | Null 기본값    | `name = input ?? "anonymous"` |
| `!` (후위, 우변) | Null 아님 단언 | `v1, v2, v3 = myFunc(...)!`   |

- 삼항의 조건은 반드시 Boolean.
- 후위 `!`는 대입의 우변 식 전체에 적용되며, 결과에 포함된 모든 nullable 값을 non-null로 단언한다. Null이면 `NullPointerException`.

### 6.6 참조 (9장 참조)
| 연산자      | 의미           |
| ----------- | -------------- |
| `&x` (전위) | 읽기 전용 참조 |
| `*x` (전위) | 변경 가능 참조 |

- 전위 위치(값이 와야 할 자리)의 `*`는 참조, 이항 위치의 `*`는 곱셈.

### 6.7 대입
`=`, `+=`, `-=`, `*=`, `/=`, `%=`, `**=`

### 6.8 우선순위 (높은 순)
| 순위 | 연산자                                 | 결합   |
| ---- | -------------------------------------- | ------ |
| 1    | 후위: 호출 `()`, 멤버 `.`, 인덱스 `[]` | 왼쪽   |
| 2    | `**`                                   | 오른쪽 |
| 3    | 전위: `!`, `~`, 단항 `-`, `&`, `*`     | 오른쪽 |
| 4    | `*`, `/`, `%`                          | 왼쪽   |
| 5    | `+`, `-`                               | 왼쪽   |
| 6    | `<<`, `>>`                             | 왼쪽   |
| 7    | `&&` (비트 AND)                        | 왼쪽   |
| 8    | `^`                                    | 왼쪽   |
| 9    | `\|\|` (비트 OR)                       | 왼쪽   |
| 10   | `<`, `>`, `<=`, `>=`, `==`, `!=`       | 왼쪽   |
| 11   | `not`                                  | 오른쪽 |
| 12   | `and`                                  | 왼쪽   |
| 13   | `or`                                   | 왼쪽   |
| 14   | `??`                                   | 오른쪽 |
| 15   | `? :`                                  | 오른쪽 |
| 16   | 대입 연산자                            | 오른쪽 |

- 비트 연산은 비교보다 먼저 계산된다 (Python, Rust 방식). `x && 1 == 0`은 `(x && 1) == 0`.
- `**`는 왼쪽의 단항 `-`보다 강하게 결합한다. `-2 ** 2 == -4`. 오른쪽 피연산자에는 단항 연산자가 올 수 있다 (`2 ** -1`).

### 6.9 연산자 오버로딩
```
public class Money {
    public function Money operator+(&Money o) { ... }                 // a + b
    public function Money operator*(Int64 k) { ... }                  // a * 3
    public static function Money operator*(Int64 k, &Money m) { ... } // 3 * a
    public function Money operator-() { ... }                         // -a
    public function Int64 operator[](Int64 x, Int64 y) { ... }        // a[x, y]
    public function void operator[]=(Int64 x, Int64 y, Int64 v) { ... } // a[x, y] = v
}
```
- 오버로딩할 수 있는 연산자: `+ - * / % **`, `&& || ^ << >>`, 단항 `-`, `~`, 인덱싱 `[]`, 인덱스 대입 `[]=`.
- `== != < > <= >=`는 오버로딩하지 않는다. `==`/`!=`는 `equals()`, 크기 비교는 `compareTo()`를 쓴다 (6.2).
- 연산자 메서드는 `public`이어야 하고 제네릭일 수 없다. 형태:
  - 이항: 인스턴스 메서드(매개변수 1개 = 오른쪽 피연산자) 또는 정적 메서드(매개변수 2개).
  - 단항 `-`, `~`: 매개변수 없는 인스턴스 메서드 또는 매개변수 1개인 정적 메서드.
  - `operator[]`: 인덱스 매개변수 1개 이상, 결과 타입 있음. `operator[]=`: 인덱스 매개변수들과 마지막 값 매개변수, `void`.
- `a op b`의 해석: 왼쪽 피연산자의 인스턴스 메서드를 먼저 찾고, 맞는 것이 없으면 두 피연산자 클래스의 정적 메서드를 찾는다 (`2.0 * m`). 오버로드 선택 규칙은 일반 메서드와 같다 (8.4). 리터럴 피연산자는 선택된 매개변수 타입에 맞춰진다.
- `+`의 한쪽이 `String`이면 항상 문자열 연결이다.
- 인스턴스 연산자 메서드는 가상 호출이다. 인터페이스도 연산자 메서드를 선언할 수 있다.
- 피연산자 전달은 매개변수 타입을 따른다: `&T`는 빌림, `T`는 소유권 이동 (9장).
- 복합 대입 `a += b`는 `a = a + b`이다. `a[i, j] += v`는 `operator[]`로 읽고 `operator[]=`로 쓴다 (인덱스는 한 번만 계산).

---

## 7. 제어 흐름

### 7.1 조건식 규칙
- `if`, `for`의 조건, 삼항의 조건은 반드시 `Boolean` 타입이어야 한다.
- `if (0)`, `if (str)`, `if (nullableValue)`는 타입 에러.
- 빈 값 검사는 명시적으로 한다: `s.length() == 0`, `s.isEmpty()`.

### 7.2 if
```
if (a == b and not c) {
} else if (d) {
} else {
}
```

### 7.3 switch
```
switch (value as v) {
    case v == 1:
        ...
    case v.startsWith(x):
        ...
        fallthrough
    default:
        ...
}
```
- `case`는 Boolean 조건식을 받는다.
- 각 `case`는 기본적으로 종료된다 (암시적 break).
- 다음 case로 이어서 실행하려면 `fallthrough`를 명시한다.

### 7.4 for (while 없음)
```
for (Int counter = 0; counter < 10; counter += 1) {}   // C 스타일
for (element in arr) {}                                // for-each
for (element in Int.range(start, end, step)) {}        // 범위
for (index, element in arr.kv()) {}                    // 키-값 언패킹
for (; a == b;) {}                                     // while과 동일
for (;;) {}                                            // 무한 루프
```
- `break`, `continue` 지원.

### 7.5 예외 처리
```
public function String readFile(String path) throws IOException { ... }

try {
    String s = readFile("a.txt")
} catch (IOException e) {
    ...
} finally {
    ...
}

throw IllegalArgumentException("message")
```
- Java와 동일한 문법과 checked exception 규칙을 따른다.
- `Exception` 계열(단, `RuntimeException` 제외)은 checked exception이다. 던질 수 있는 함수는 `throws`로 선언해야 하고, 호출자는 catch하거나 자신도 `throws`로 선언해야 한다.
- `RuntimeException` 계열과 `Error` 계열은 unchecked exception이다. 선언 없이 던질 수 있다.
- 예외는 해당 스코프의 소유 객체를 해제(`drop()` 호출 포함)하며 전파된다.

### 7.6 예외 클래스 계층 (Java 계승)
```
Throwable
├── Error
│   └── OutOfMemoryError, StackOverflowError ...
└── Exception                         (checked)
    ├── IOException
    └── RuntimeException              (unchecked)
        ├── NullPointerException       // 후위 ! 단언 실패
        ├── ArithmeticException        // 오버플로, 0 나누기, 정수 음수 지수
        ├── ClassCastException         // castTo 실패
        ├── IndexOutOfBoundsException  // 배열 범위 초과
        └── IllegalArgumentException
```

---

## 8. 함수

### 8.1 선언
```
function Int64 add(Int64 a, Int64 b) {
    return a + b
}

function void log(String msg) {
    stdio.println(msg)
}
```
- 형식: `function <반환 타입> 이름(매개변수) { 본문 }`
- 반환 타입은 생략할 수 없다. 반환값이 없으면 `void`를 명시한다.

### 8.2 다중 반환
```
function (Int|Float, Int|String, Boolean?) myFunc(String[] args) { ... }

v1, v2, v3 = myFunc(args)!      // nullable 단언
v1, _, v3 = myFunc(args)!       // _로 값 버리기
```

### 8.3 인자 전달
```
Int64.array(length = 10, length_immutable = false)   // 이름 있는 인자
Int64.array(10, false)                               // 위치 인자
```
- 이름 있는 인자와 위치 인자를 한 호출에서 섞을 수 없다.
- 이름 있는 인자의 레이블은 실제 매개변수 이름 및 순서와 정확히 일치해야 한다. 다르면 컴파일 에러.
- 매개변수 기본값은 지원하지 않는다.

### 8.4 오버로딩과 오버라이딩
- Java 규칙을 따른다.
- 오버로딩 선택은 항상 컴파일 타임에 이루어진다.
- `DTVariable` 인자는 `DTVariable` 타입 매개변수에만 매칭된다. 다른 오버로드를 호출하려면 `.castTo(T)`로 명시 변환한다.

### 8.5 람다
```
Function[(Int32, Float32), void] f = (Int32 a, Float32 b) -> {
    stdio.println(f"a={a}, b={b}")
}
f.run(1, 3.0)
```
- 바깥 변수는 기본적으로 빌려서 캡처한다.
- 람다가 생성된 스코프보다 오래 살아야 하면 `move` 키워드로 소유권을 가져간다.

---

## 9. 메모리 모델

### 9.1 정책
- 기본값은 `ownership`.
- `manual`은 엔트리 파일의 `@compiler(MemoryManagement=manual)`로만 지정하며, 프로젝트 내 모든 소스 파일에 적용된다.
- 외부 라이브러리는 항상 `ownership` 모드다.

### 9.2 복사 타입과 이동 타입 (ownership 모드)
| 분류      | 타입                                                        | 대입/전달 시 |
| --------- | ----------------------------------------------------------- | ------------ |
| 복사 타입 | 모든 숫자 타입, `Boolean`                                   | 값 복사      |
| 이동 타입 | `String`, 배열, `Dictionary`, 클래스 인스턴스, `DTVariable` | 소유권 이동  |

```
String a = "hello"
String b = a            // 소유권 이동
stdio.println(a)        // 컴파일 에러: 이동된 값 사용
String c = b.clone()    // 명시적 복제
```
- `String`, 배열, `Dictionary`의 `.clone()`은 copy-on-write로 구현한다. 실제 복사는 한쪽이 수정할 때 일어난다.
- `DTVariable`은 내용물과 무관하게 이동 타입으로 취급한다.

### 9.3 참조
| 표기        | 종류           | 규칙                                      |
| ----------- | -------------- | ----------------------------------------- |
| `&T` / `&x` | 읽기 전용 참조 | 동시에 여러 개 가능                       |
| `*T` / `*x` | 변경 가능 참조 | 동시에 하나만, 읽기 전용 참조와 공존 불가 |

```
function void greet(&String name) { stdio.println(name) }
function void shout(*String name) { name.append("!") }
function void addOne(*Int64 count) { count += 1 }

String n = "Alice"
greet(&n)
shout(*n)
```
- 역참조는 자동이다. 참조는 원래 값처럼 사용한다.
- 빌림은 참조가 마지막으로 사용된 지점에서 끝난다.
- 빌려준 동안 원본은 수정, 이동, 해제할 수 없다.

### 9.4 `copied` 키워드
```
copied String s = "abcd"
String t = s              // 자동으로 s.clone()
lib.archive(s)            // 소유권 매개변수에도 복제본 전달
stdio.println(s)          // OK
```
- 해당 변수가 이동의 원천이 될 때 자동으로 `.clone()`이 삽입된다.
- 효과는 변수가 "보내는 쪽"일 때만 적용된다. `copied String s = t`에서 `t`는 평소대로 이동한다.
- 복사 타입(숫자, Boolean)에 붙이면 컴파일 에러 없이 경고를 출력하고, 컴파일 단계에서 키워드를 제거한다.
- `Immutable copied` 변수는 수정이 불가능하므로 복사 없이 데이터를 공유한다.
- `copied DTVariable`은 허용되며, 내용물이 복제 불가능한 타입이면 복사 시점에 런타임 예외.

### 9.5 참조 반환 규칙 (라이프타임 표기 없음)
- 참조는 함수 밖으로 반환하거나 필드에 저장할 수 없다.
- 유일한 예외: 메서드가 `this`에서 빌린 참조를 반환하는 것은 허용한다. 반환된 참조는 해당 객체에서 빌린 것으로 간주된다.

```
// 허용
public function &String name() { return &this.name }

// 에러: 지역 변수 참조 반환 (댕글링)
function &String make() { String s = "hi"; return &s }

// 에러: this가 아닌 곳에서 빌린 참조 반환
function &String longer(&String a, &String b) { ... }

// 대안: 소유권 있는 값 반환
function String longer(&String a, &String b) {
    return a.length() > b.length() ? a.clone() : b.clone()
}

// 에러: 참조를 필드에 저장
class Holder { private &String ref }
```

### 9.6 자동 해제와 소멸자
- 소유자 변수가 스코프를 벗어나면 자동 해제된다.
- 해제 시 정리 작업(파일 닫기, 연결 종료 등)이 필요한 클래스는 `Droppable` 인터페이스를 구현한다.
```
  public class FileWriter implements Droppable {
      @Override
      public function void drop() {
          // 파일 닫기
      }
  }
```
- `drop()`은 객체 해제 직전에 자동 호출되며, 직접 호출할 수 없다.
- 객체가 소유한 필드들은 `drop()` 호출 이후 선언 역순으로 해제된다.
- 예외로 스코프를 벗어날 때도 동일하게 해제된다.

### 9.7 manual 모드
- 이동 규칙과 빌림 검사를 하지 않는다.
- `&`, `*` 참조 표기를 사용할 수 없다.
- 객체 대입은 같은 객체를 가리키는 포인터 복사다.
- `free(x)`로 직접 해제하며, `free`는 `drop()`을 호출하고 객체가 소유한 내부 데이터까지 재귀적으로 해제한다.
- `free`는 manual 모드에서만, `&`/`*`는 ownership 모드에서만 사용할 수 있다.
- 해제된 객체 접근 시: 인터프리터와 바이트코드 VM은 감지하여 에러를 낸다. 네이티브 코드는 정의되지 않은 동작이다.

### 9.8 manual 프로젝트에서 ownership 라이브러리 호출
| 라이브러리 매개변수 | manual 코드의 동작                                           |
| ------------------- | ------------------------------------------------------------ |
| `&T`                | 컴파일러가 포인터를 자동 전달. 객체는 여전히 호출자 책임     |
| `*T`                | 동일                                                         |
| `T` (소유권)        | 라이브러리가 소유권을 가져가 해제한다. 이후 사용/`free`는 금지 (탐지 가능한 경우 경고) |
| 반환값 `T`          | 호출자가 소유하며 `free` 책임을 진다                         |

---

## 10. 클래스와 인터페이스

### 10.1 클래스 선언
```
public class Human extends Animal implements Entity, MovingEntity {

    public  static              Immutable String id = "animal.mammal.Human"
    private setter.chain getter           String name
    private setter.chain getter           Int64 age
    private              getter Immutable Int64 socialSecurity
    private                     Immutable String race
    private setter.nochain                String? eyeColor

    public Human(String name, Int64 age, Int64 socialSecurity) {
        super(name)
        this.name = name
        this.age = age
        this.socialSecurity = socialSecurity
        this.race = "Someone"
    }

    @Override
    public function Int64 hello() { ... }
}
```

### 10.2 상속
- 클래스는 단일 상속만 지원한다 (`extends` 하나).
- 인터페이스는 여러 개 구현할 수 있다 (`implements A, B`).
- 부모 생성자 호출은 Java와 동일하게 생성자 첫 문장에서 `super(...)`로 한다.
- 두 인터페이스가 같은 시그니처의 `default` 메서드를 가지면, 클래스에서 명시적으로 해결하지 않는 한 컴파일 에러.
- 오버라이드하는 메서드는 반환 타입을 하위 클래스(또는 하위 인터페이스) 타입으로 좁힐 수 있다 (공변 반환 타입, Java와 동일). 예) `Tensor.round()`는 `Tensor[T]`, `Matrix.round()`는 `Matrix[T]`.
```
  // 특정 인터페이스의 구현에 위임
  @Override
  public function Int64 hello() = origin Entity

  // 특정 상위 구현 호출
  super(Entity).hello()
```

### 10.3 인터페이스 선언
```
public interface MovingEntity extends Entity {
    public function void move(Int64 dx, Int64 dy)

    public default function Boolean canMove() {
        return true
    }
}
```
- Java와 동일하다. 인터페이스는 여러 인터페이스를 `extends`할 수 있다.
- `default` 메서드로 기본 구현을 제공할 수 있다.

### 10.4 생성자
- Java 방식: 클래스 이름으로 선언한다 (`public Human(...)`). 반환 타입을 쓰지 않는다.
- 객체 생성은 `new Human(...)` 또는 `Human(...)`이다 (`new`는 선택). `new` 뒤에는 클래스 이름만 올 수 있고, 패키지 경로와 타입 인자를 쓸 수 있다: `new linear.Matrix(2, 2)`, `new Box[Int64](5)`. 인터페이스와 내장 타입은 `new`로 만들 수 없다.
- 생성자 종료 시점에 모든 non-null 필드가 초기화되어 있어야 한다. 아니면 컴파일 에러.
- nullable 필드는 대입하지 않으면 Null로 초기화된다.
- `Immutable` 필드는 정확히 한 번 대입해야 한다 (nullable 포함).

### 10.5 수정자
- 접근 제어: `public`, `protected`, `private` (Java와 동일)
- 기타: `static`, `Immutable`, `copied`
- 접근자 자동 생성: `getter`, `setter.chain`, `setter.nochain`
- 수정자의 순서는 자유다.

### 10.6 자동 생성 접근자
| 수정자           | 생성 메서드   | 동작                                                         |
| ---------------- | ------------- | ------------------------------------------------------------ |
| `getter`         | `name()`      | 복사 타입 필드는 값 복사 반환, 이동 타입 필드는 `&T` 참조 반환 |
| `setter.chain`   | `name(value)` | 값의 소유권을 가져가 대입하고, 체이닝을 위해 자기 자신을 반환 |
| `setter.nochain` | `name(value)` | 값의 소유권을 가져가 대입하고, `void` 반환                   |

```
alice.name("Alice").age(31)
Int64 a = alice.age()                 // 복사
&String n = alice.name()              // alice에서 빌림
String owned = alice.name().clone()   // 소유권 있는 복제본
```
- `setter`와 `Immutable`을 함께 쓰면 컴파일 에러.

### 10.7 `toString` 프로토콜
```
public class Point3 extends Point {
    @Override
    public function String toString() { return "Point3 of " + super.toString() }
}
```
- 모든 값은 `toString()`(별칭 `string()`)을 가진다. 클래스의 기본 형식은 `Name(field=value, ...)`, 예외는 `Name: message`이다.
- 클래스는 `public function String toString()`을 선언해 형식을 바꾼다. 상위 클래스에 없더라도 `@Override`를 붙일 수 있다 (Java의 `Object.toString`과 동일). 인자 없는 `toString`이 `String`이 아닌 타입을 반환하면 컴파일 에러.
- 오버라이드는 동적으로 선택되며, `stdio.println`, 문자열 연결, f-string, 포맷 지정자(11.1), 컨테이너 안의 값 출력, 잡히지 않은 예외 메시지에 모두 쓰인다.
- `super.toString()`: 상위 클래스의 구현을 호출한다. 상위 클래스들에 구현이 없으면 기본 형식 `Name(field=value, ...)`을 돌려준다.

---

## 11. 문자열

### 11.1 포맷팅
```
String s = f"my name is {name}, x={xLocation.format(-1, 3)}"
String t = "Hello %str%, you have %number% messages".format(name, count)
```
- 처리 순서: f-string 보간 → `.format()` 치환.
- `.format()`은 원본 리터럴에 있던 `%...%` 자리만 치환한다. f-string으로 보간된 값 안의 `%str%`는 치환되지 않는다 (주입 방지).
- `.formatWithInjection()`: 보간된 결과 전체에서 자리표시자를 치환한다 (주입을 의도적으로 허용).

**포맷 지정자** (Python의 format specification mini-language)
```
f"{pi:.2f}"           // 3.14
f"{pi:>10.3f}"        // "     3.142"
f"{1234567.891:,.2f}" // 1,234,567.89
f"{255:#x} {255:08b}" // 0xff 11111111
f"{0.256:.1%}"        // 25.6%
f"{name:^10}"         // 가운데 정렬
f"{x:{width}.{prec}f}"// 지정자 안의 중첩 필드
f"{x=}"  f"{x = :.3f}" // 디버그 형식: "x=123.456"
f"{s!r}"              // 변환: !r (따옴표 붙은 형식), !s (표시 형식)
"Total: %amount:.2f% for %name%".format(19.5, "Kim")
```
- 형식: `[[fill]align][sign][z][#][0][width][grouping][.precision][type]`
  - align `<` `>` `^` `=`, sign `+` `-` 공백, `z`(음수 0을 양수로), `#`(대체 형식: `0x` 접두사 등), `0`(0 채움), grouping `,` `_`.
  - type: 정수 `d b o x X c n`, 실수 `f F e E g G n %`, 문자열 `s`. 정수에 실수 type을 쓰면 실수로 변환한다.
- f-string 필드: `{식}`, `{식:지정자}`, `{식!r}`, `{식!s:지정자}`, `{식=}`. 지정자에는 `{식}` 필드를 중첩할 수 있다. 최상위의 `:`가 지정자를 시작하므로, 삼항 연산자는 `? :`의 짝이 맞으면 그대로 쓸 수 있고 그 외에는 괄호로 감싼다.
- `.format()` 자리표시자: `%이름%` 또는 `%이름:지정자%` (지정자에 `%`는 쓸 수 없으므로 `%` type은 f-string에서만 쓴다).
- Python과 다른 점: type 없이 정밀도도 없으면 이 언어의 기본 표시 형식을 쓴다 (`1.0E10`, `NaN`, `true`). `Boolean`은 숫자 type(`d` 등)을 줄 때만 숫자(1/0)로 형식화된다.
- 리터럴 지정자는 컴파일 시 문법과 값 타입을 검사한다 (`f"{1.5:d}"`는 컴파일 에러). 실행 중 만들어진 지정자가 잘못되면 `IllegalArgumentException`.

### 11.2 정적 멤버
| 멤버                                                 | 설명                        |
| ---------------------------------------------------- | --------------------------- |
| `String.random(regex, min_length, max_length)`       | 정규식에 맞는 무작위 문자열 |
| `String.array(length = x, length_immutable = false)` | 배열 생성                   |
| `String.array()`                                     | 가변 길이 배열 생성         |

### 11.3 인스턴스 메서드
| 메서드                                             | 설명                              | 변경 |
| -------------------------------------------------- | --------------------------------- | ---- |
| `.format(a, b, ...)`                               | `%str%`, `%number%` 순서대로 치환 |      |
| `.formatWithInjection(a, b, ...)`                  | 주입 허용 치환                    |      |
| `.randomize(regex, min_length, max_length)`        | 무작위 값으로 변경                | 변경 |
| `.replace(a, b, limit, reverse)`                   | 치환                              |      |
| `.substring(a, b)`                                 | 부분 문자열                       |      |
| `.startsWith(a)` / `.endsWith(a)` / `.contains(a)` | 검사                              |      |
| `.length()`                                        | 길이                              |      |
| `.isEmpty()`                                       | 빈 문자열 여부                    |      |
| `.parse(type)`                                     | 타입 변환                         |      |
| `.characters()`                                    | `String[]` 반환                   |      |
| `.split(splitter, max)`                            | 분할                              |      |
| `.append(a)` / `.prepend(a)`                       | 앞/뒤에 추가                      | 변경 |
| `.equals(other)`                                   | 값 비교 (`==`와 동일)             |      |
| `.clone()`                                         | 복제 (copy-on-write)              |      |

- 변경 메서드는 `Immutable String`에 호출할 수 없다.

---

## 12. 숫자 타입 멤버

### 12.1 정적 멤버
| 멤버                                                        | 설명                                               |
| ----------------------------------------------------------- | -------------------------------------------------- |
| `MAX` / `MIN`                                               | 각 타입의 최댓값/최솟값 (부호 없는 타입의 MIN은 0) |
| `.array(length = x, length_immutable = false)` / `.array()` | 배열 생성                                          |
| `Int.max(a, b, ...)` / `Int.min(a, b, ...)`                 | `Int` 클래스 전용, 모든 숫자 타입 인자 허용        |
| `.random(start, end)`                                       | 시작 포함, 끝 미포함 무작위 값                     |
| `.range(start, end, step)`                                  | 범위                                               |

### 12.2 인스턴스 메서드
| 메서드                                        | 설명                                     |
| --------------------------------------------- | ---------------------------------------- |
| `.randomize(start, end)`                      | 무작위 값으로 변경                       |
| `.format(whole, decimal)`                     | 정수부/소수부 자릿수 포맷, `-1`은 무제한 |
| `.round()` / `.ceil()` / `.floor()`           | 정수 단위로 반올림 / 올림 / 내림          |
| `.round(d)` / `.ceil(d)` / `.floor(d)`        | 소수점 `d`자리 단위 (`0`: 정수, `1`: 0.1, `2`: 0.01, 음수 `-2`: 100) |
| `.round(w, d)` / `.ceil(w, d)` / `.floor(w, d)` | 정수부는 `10^w` 단위, 소수부는 `10^-d` 단위로 각각 처리 |
| `.abs()`                                      | 절댓값 (오버플로는 파일 정책을 따름)      |
| `.string()`                                   | 문자열 변환                              |
| `.castTo(T)`                                  | 명시적 타입 변환                         |
| `.addWrap(b)` / `.subWrap(b)` / `.mulWrap(b)` | 정책 무관 순환 연산                      |

**반올림 규칙**
```
Float64 x = 123.456
x.round()       // 123.0
x.round(1)      // 123.5
x.round(2, 1)   // 100.5   (123 -> 100, 0.456 -> 0.5)
x.ceil(2, 1)    // 200.5
x.floor(2, 1)   // 100.4
2.675.round(2)  // 2.68    (값의 가장 짧은 십진 표현 기준)
```
- 결과 타입은 수신 값의 타입과 같다.
- `round`는 0.5에서 0에서 먼 쪽으로 반올림한다 (`2.5 -> 3`, `-2.5 -> -3`). `ceil`은 +∞ 쪽, `floor`는 -∞ 쪽.
- 실수는 값을 다시 읽었을 때 같은 값이 되는 가장 짧은 십진 표현을 기준으로 처리하므로 `2.675.round(2)`는 `2.68`이다.
- `(w, d)` 형태는 정수부와 소수부를 독립적으로 처리한 뒤 더한다. 소수부가 올림되어 1이 되면 정수부에 더해진다 (`6.67.round(1, 0)` = `10 + 1` = `11.0`).
- `(w, d)`에서 `w`, `d`가 음수이면 `IllegalArgumentException`. 인자는 최대 2개.
- 정수 타입에서는 정수부 자릿수(`10^w`, 또는 음수 `d`)만 의미가 있다. 결과가 타입 범위를 넘으면 파일의 `IntegerOverflow` 정책을 따른다 (`error`일 때 `ArithmeticException`).

---

## 13. 배열과 Dictionary

### 13.1 인덱스 규칙
- 인덱스 매개변수 타입은 `Int64`다. 부호 있는 정수 타입은 확대 변환으로 전달된다.
- **부호 없는 정수 타입(`UInt8`~`UInt64`)은 인덱스로 사용할 수 없다.** 확대 변환이 가능하더라도 컴파일 에러.
- 음수 인덱스는 끝에서부터 센다. `-1`은 마지막 원소, `-2`는 마지막에서 두 번째 원소.
- 범위를 벗어나면 `IndexOutOfBoundsException`.

### 13.2 대괄호 인덱싱
```
Int64 v = arr[0]       // arr.at(0)
arr[-1] = 5            // arr.set(-1, 5)
```
- `arr[i]`는 `arr.at(i)`, `arr[i] = v`는 `arr.set(i, v)`의 축약이다.
- 다차원 인덱싱: `grid[i, j]`는 `grid[i][j]`와 같다. 배열과 Dictionary를 한 단계씩 내려가며, 대입·복합 대입(`grid[i, j] += 1`)도 같다.
- 클래스 값의 인덱싱은 남은 인덱스를 모두 `operator[]` / `operator[]=`에 넘긴다 (6.9). 예) `m[i, j]`는 `m.operator[](i, j)`.

### 13.3 배열 메서드
| 메서드                  | 설명                                         | `Immutable` | `length_immutable` |
| ----------------------- | -------------------------------------------- | ----------- | ------------------ |
| `.length()`             | 길이                                         | 허용        | 허용               |
| `.isEmpty()`            | 비어 있는지 여부                             | 허용        | 허용               |
| `.at(index)`            | 원소 접근                                    | 허용        | 허용               |
| `.first(offset)`        | 앞에서 `offset`번째 원소. `offset` 생략 시 0 | 허용        | 허용               |
| `.last(offset)`         | 뒤에서 `offset`번째 원소. `offset` 생략 시 0 | 허용        | 허용               |
| `.kv()`                 | 인덱스-값 쌍                                 | 허용        | 허용               |
| `.set(index, value)`    | 원소 대입                                    | 금지        | 허용               |
| `.fill(a, b, ...)`      | 값 채우기                                    | 금지        | 허용               |
| `.reverse()`            | 순서 뒤집기                                  | 금지        | 허용               |
| `.sort()`               | 정렬 (원소 타입이 `Comparable`이어야 함)     | 금지        | 허용               |
| `.shuffle()`            | 무작위 섞기                                  | 금지        | 허용               |
| `.insert(index, value)` | 삽입                                         | 금지        | 금지               |
| `.remove(index)`        | 제거하고 그 값을 소유권과 함께 반환          | 금지        | 금지               |
| `.equals(other)`        | 값 비교 (`==`와 동일)                        | 허용        | 허용               |
| `.clone()`              | 복제 (copy-on-write)                         | 허용        | 허용               |

- `.at()`, `.first()`, `.last()`의 반환은 getter와 같은 규칙을 따른다: 복사 타입 원소는 값 복사, 이동 타입 원소는 `&T` 참조.
- `first(k)`는 `at(k)`와, `last(k)`는 `at(-1 - k)`와 같다. 예) `last()` == `at(-1)`, `last(1)` == `at(-2)`.
- `first`, `last`에 음수 오프셋을 넘기면 `IndexOutOfBoundsException`.
- `index`, `offset` 매개변수는 13.1절의 인덱스 규칙을 따른다.

### 13.4 Dictionary 메서드
| 메서드      | 설명                                   |
| ----------- | -------------------------------------- |
| `.kv()`     | 키-값 쌍                               |
| `.merge(d)` | 다른 Dictionary 병합 (Python `update`) |

---

## 14. 모듈과 표준 입출력

### 14.1 import
```
using stdio as stdio
using lib.ping as p                 // 모듈: p.ping(...), 모듈의 클래스
using math.linear.Matrix as Matrix  // 모듈 math/linear/Matrix.l2 — 별칭은 그 클래스도 가리킨다
using math.linear.* as linear       // 패키지 별칭: linear.Matrix
using math.linear as linear         // 위와 같음
using math.linear.*                 // 패키지의 타입을 단순 이름으로: Matrix, Vector
using math.linear                   // 위와 같음 (별칭 없는 패키지)
```
- 파일 최상단과 함수 스코프 모두 허용한다. 파일 최상단을 권장한다.
- 경로 해석 순서: 모듈 파일(`a/b/C.l2`) → 패키지 디렉터리(`a/b/`) → 패키지 안의 타입(`using a.b.Type`). 프로젝트 파일이 표준 라이브러리보다 우선한다.
- 모듈을 가져오면 그 모듈의 함수(`별칭.f()`)와 그 모듈 패키지의 타입을 쓸 수 있다. 모듈이 자기 이름과 같은 타입을 선언하면 별칭은 그 타입의 이름이기도 하다.
- `as`가 없는 모듈 import의 별칭은 경로의 마지막 부분이다. `as`가 없는 패키지 import는 패키지의 모든 타입을 단순 이름으로 가져온다.
- 순환 import는 허용한다. 컴파일러는 모든 모듈의 선언을 먼저 수집한 뒤 본문을 분석한다.
- 정적 변수 초기화가 순환 참조하는 경우만 컴파일 에러.

### 14.2 stdio
| 함수                                        | 설명                        |
| ------------------------------------------- | --------------------------- |
| `stdio.println(s)`                          | 출력 후 줄바꿈              |
| `stdio.read(prompt)`                        | 프롬프트 출력 후 한 줄 입력 |
| `stdio.replaceLine(s, lines_from_last = 0)` | 마지막에서 N번째 줄 교체    |

### 14.3 패키지와 이름 해석
- 패키지는 디렉터리다. `math/linear/Matrix.l2`의 모듈 이름은 `math.linear.Matrix`이고, 그 안의 클래스 `Matrix`의 정규 이름은 `math.linear.Matrix`이다. 엔트리 파일의 디렉터리와 프렐류드는 기본 패키지(이름 없음)다.
- 이름 있는 패키지의 모듈을 불러오면 같은 패키지의 모듈도 함께 불러오므로, 같은 패키지의 타입끼리는 import 없이 단순 이름으로 참조한다.
- 단순 타입 이름은 다음 순서로 찾는다: 명시적 import(별칭 포함) → 같은 패키지 → 가져온 패키지/모듈의 패키지 → 기본 패키지와 프렐류드.
- 가져온 패키지 중 둘 이상에 같은 이름이 있으면 그 단순 이름은 모호하다 (컴파일 에러). 정규 이름(`math.linear.Matrix`) 또는 별칭(`linear.Matrix`)으로 구분한다. 정규 이름은 import 없이도 쓸 수 있다 (해당 모듈이 로드된 경우).
- 타입 위치(`math.linear.Matrix m`), 생성(`new math.linear.Matrix(2, 2)`, `linear.Matrix(2, 2)`), 정적 멤버(`linear.Matrix.identity(3)`, `Matrix[Int32].identity(3)`) 모두에 정규 이름과 별칭을 쓸 수 있다.
- 타입 인자 없이 정적 멤버에 접근하면 기대 타입의 타입 인자, 없으면 기본 타입 인자를 쓴다 (`Matrix.identity(3)`은 `Matrix[Float64]`).

### 14.4 표준 라이브러리: `math.linear`
`Tensor[T extends Numeric = Float64]`, `Matrix[T]`(`Tensor`의 하위 클래스, 2차원), `Vector[T]`(`Tensor`의 하위 클래스, 1차원).
```
using math.linear.Matrix as Matrix
using math.linear.Vector as Vector

Matrix a = new Matrix([[1, 2], [3, 4]])
Matrix b = a * a.transpose() + 2.0 * Matrix.identity(2)
Vector x = b.solve(&new Vector([1.0, 2.0]))
Matrix[Int32] counts = a.migrate[Int32]()          // 기본: 반올림
Matrix[Float16] half = a.migrate[Float16]("ceil")  // "round" | "ceil" | "floor"
a[0, 1] = 9.5
a.forEachRow((Vector row) -> row * 2.0)
```
- 데이터는 행 우선(row-major) 1차원 배열로 저장한다. 요소 타입은 모든 내장 숫자 타입이다.
- **Tensor**: `shape()`, `rank()`, `size()`, `get(idx)`/`set(idx, v)`, `t[i, j, k]`(인덱스 1~4개 또는 `Int64[]`, 음수 인덱스 허용), `reshape`, `flatten`, `toArray`, 정적 `zeros`/`ones`/`full`; 같은 모양끼리 `+ -`, 스칼라와 `+ - * / %`, 단항 `-`, `hadamard`(요소곱), `divide`(요소 나눗셈), `pow`, `abs`, `sum`, `product`, `min`, `max`, `mean`(Float64), `map`.
- **Matrix**: `*`는 행렬 곱(행렬×행렬, 행렬×벡터), `**`는 정수 거듭제곱, `transpose`, `determinant`(Float64), `inverse`/`solve`(Float64 결과, 특이 행렬은 `ArithmeticException`), `trace`, `row`/`column`/`setRow`/`setColumn`, `rows`/`cols`/`isSquare`, 정적 `identity`/`zeros`/`ones`, `new Matrix(rows, cols)`, `new Matrix(rows, cols, values)`, `new Matrix([[...], ...])`.
- **Vector**: `dot`, `cross`(3차원), `norm`(Float64), `normalized`(Vector[Float64]), `outer`(Matrix), 행벡터×행렬 `v * m`, `new Vector(n)`, `new Vector([...])`.
- 연산 결과는 하위 클래스 타입을 유지한다 (`Matrix + Matrix`는 `Matrix`, `Matrix.round()`는 `Matrix`). 모양이 맞지 않으면 `IllegalArgumentException`. 정수 요소의 오버플로는 `ArithmeticException`.
- **요소별 반올림**: `round()`, `round(d)`, `round(w, d)`와 `ceil`, `floor`는 숫자 메서드(12.2)와 같은 규칙을 모든 요소에 적용한 새 텐서를 돌려준다.
- **요소 타입 변환** `migrate[U]()` / `migrate[U](method)`: 모든 요소를 `U`로 옮긴 새 텐서를 만든다. 넓히는 변환(`Float32 -> Float64`)은 정확하다. 좁히는 변환은 `method`에 따라 가장 가까운 값(`"round"`, 기본), 위쪽(`"ceil"`), 아래쪽(`"floor"`)의 표현 가능한 값으로 옮긴다 (실수 → 정수, `Float64 -> Float16` 등, 대소문자 무관). 범위를 넘는 정수 변환은 `ArithmeticException`, 알 수 없는 method는 `IllegalArgumentException`.
- **람다 적용** (제자리 변경): `forEachElement((T) -> T)`, `forEachElement((Int64[] index, T) -> T)`, Matrix의 `forEachElement((Int64 i, Int64 j, T) -> T)`, `forEachRow((Vector[T]) -> Vector[T])` / `forEachRow((Int64, Vector[T]) -> Vector[T])`, `forEachColumn(...)`. 행은 마지막 축을 따르는 1차원 조각, 열은 그 앞 축을 따르는 조각이다 (1차원 텐서는 한 행). 돌려준 벡터의 길이가 다르면 `IllegalArgumentException`.
- **멀티스레드**: `allowMultithreading`(기본 `false`)과 `maxWorkerThreads`(기본 `0` = 코어 수) 속성으로 제어한다 (`t.allowMultithreading(true).maxWorkerThreads(4)`). 켜면 큰 텐서(약 3만 2천 회 이상의 요소 연산)의 행렬 곱, 요소별 산술, 반올림, 타입 변환을 런타임이 여러 스레드로 나누어 계산한다. 각 결과 요소의 계산 순서가 같으므로 결과와 예외는 스레드 사용 여부와 무관하게 동일하다. 연산 결과는 왼쪽 피연산자의 설정을 물려받는다. 람다(`forEach*`, `map`)는 항상 호출한 스레드에서 순서대로 실행된다.
- `toString()`은 numpy처럼 열을 맞춘 중첩 대괄호로 출력한다. `==`는 모양과 모든 요소가 같을 때 참이다.

---

## 15. 컴파일러 아키텍처

### 15.1 파이프라인
```
소스 → 렉서 → 파서 → AST → 이름 해석 → 타입 검사 → 소유권/빌림 검사
                                                         │
                     ┌───────────────────────────────────┼──────────────────────┐
                     ▼                                   ▼                      ▼
           트리 워킹 인터프리터                바이트코드 컴파일러 + VM     LLVM IR 생성 → 네이티브
```
- 프론트엔드(렉서~소유권 검사)는 세 백엔드가 공유한다.
- 선언 수집 단계를 먼저 수행하여 순환 import와 대괄호 구분(타입 인자/인덱싱)을 처리한다.
- 제네릭은 프론트엔드 이후 단형화하여 각 백엔드에 전달한다.
- 네이티브 백엔드는 `Target`의 모든 플랫폼에 대해 LLVM 크로스 컴파일과 lld 링크를 수행한다.

### 15.2 구현 순서
1. 핵심 기능만으로 프론트엔드 + 트리 워킹 인터프리터 (기준 구현)
2. 바이트코드 컴파일러 + VM
3. LLVM 기반 네이티브 컴파일러 (단일 타깃 → 크로스 컴파일)
4. 기능 확장 (클래스, 제네릭, Dictionary 타입 검사, 메모리 모델 고급 기능 등)

### 15.3 테스트
- 차분 테스트: 동일한 테스트 프로그램을 세 백엔드로 실행하여 출력이 일치하는지 검증한다.
- 트리 워킹 인터프리터를 기준 정답으로 사용한다.
- manual 모드의 잘못된 메모리 접근은 백엔드별 결과가 다를 수 있으므로 차분 테스트 대상에서 제외한다.

---

## 16. 추후 작업

| #    | 항목                                | 비고                                      |
| ---- | ----------------------------------- | ----------------------------------------- |
| 1    | 라이브러리 및 패키지 시스템         | 패키지·import·이름 해석(14.1, 14.3), `math.linear`(14.4) 완료. 외부 라이브러리 배포는 미정 |
| 2    | 공유 소유권 타입 `Shared[T]`        | 참조 카운팅                               |
| 3    | Dictionary 키로 사용할 수 있는 타입 | 클래스를 키로 쓸 때의 해시 규칙           |
| 4    | 표준 라이브러리 전반                | 파일 입출력, 컬렉션(`List` 등), 수학 함수 |
| 5    | 사용자 정의 `Numeric` 타입           | 연산자 오버로딩한 클래스(복소수 등)를 `Tensor` 요소로 |