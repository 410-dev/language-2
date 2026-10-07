//! Built-in members (spec 11-13, 14.2) described for editor tooling: completion, hover and
//! signature help. The checker implements them in `check/members.rs` and `check/arrays.rs`;
//! keep this table in step when a built-in member is added or changed.
//!
//! Signatures use placeholders that are replaced by the receiver's types when shown:
//! `Self` (the receiver type), `T` (array element), `K` / `V` (dictionary key / value) and
//! `Float` (the receiver's float type, `Float64` for integers).

/// What a built-in member is available on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Recv {
    /// Every value (`equals`, `toString`, `clone`, ...).
    Any,
    Str,
    /// Integer and float numbers.
    Num,
    /// Integers only.
    Int,
    Array,
    Dict,
    /// Function values.
    Func,
    /// `String.xxx`
    StrStatic,
    /// `Int64.xxx`, `Float64.xxx`, ...
    NumStatic,
    /// `Int.max(...)` / `Int.min(...)`
    IntClassStatic,
    /// `stdio.xxx`
    Stdio,
}

pub struct Member {
    pub recv: Recv,
    pub name: &'static str,
    /// Parameter list as written in a declaration, without parentheses; `None` for a constant.
    pub params: Option<&'static str>,
    pub ret: &'static str,
    pub doc: &'static str,
}

const fn m(recv: Recv, name: &'static str, params: &'static str, ret: &'static str, doc: &'static str) -> Member {
    Member { recv, name, params: Some(params), ret, doc }
}

const fn c(recv: Recv, name: &'static str, ret: &'static str, doc: &'static str) -> Member {
    Member { recv, name, params: None, ret, doc }
}

use Recv::*;

pub const MEMBERS: &[Member] = &[
    // ---------------------------------------------------------------- every value (10.7, 4.11)
    m(Any, "equals", "&Self other", "Boolean", "값 비교 (`==`와 같다)."),
    m(Any, "isSameReferenceWith", "&DTVariable other", "Boolean", "같은 객체를 가리키는지 (참조 비교)."),
    m(Any, "toString", "", "String", "문자열 표현. 클래스는 `toString()`을 선언해 바꿀 수 있다 (10.7)."),
    m(Any, "string", "", "String", "`toString()`의 별칭."),
    m(Any, "clone", "", "Self", "복제 (copy-on-write)."),
    m(Any, "castTo", "Type type", "type", "명시적 타입 변환 (4.11)."),
    m(Any, "toJson", "", "String", "JSON 텍스트 (14.5)."),
    m(Any, "toJson", "Int64 indent", "String", "들여쓰기 `indent`칸의 JSON 텍스트 (14.5)."),
    // ---------------------------------------------------------------- String (11.3)
    m(Str, "length", "", "Int64", "문자 수."),
    m(Str, "isEmpty", "", "Boolean", "빈 문자열인지."),
    m(Str, "substring", "Int64 start", "String", "`start`부터 끝까지의 부분 문자열."),
    m(Str, "substring", "Int64 start, Int64 end", "String", "`[start, end)` 구간의 부분 문자열. 음수는 끝에서부터 센다."),
    m(Str, "startsWith", "&String prefix", "Boolean", "`prefix`로 시작하는지."),
    m(Str, "endsWith", "&String suffix", "Boolean", "`suffix`로 끝나는지."),
    m(Str, "contains", "&String text", "Boolean", "`text`를 포함하는지."),
    m(Str, "indexOf", "&String text", "Int64", "`text`가 처음 나오는 위치. 없으면 `-1`."),
    m(Str, "toUpperCase", "", "String", "대문자로 바꾼 새 문자열."),
    m(Str, "toLowerCase", "", "String", "소문자로 바꾼 새 문자열."),
    m(Str, "trim", "", "String", "앞뒤 공백을 지운 새 문자열."),
    m(Str, "replace", "&String from, &String to", "String", "`from`을 모두 `to`로 치환한 새 문자열."),
    m(Str, "replace", "&String from, &String to, Int64 limit", "String", "앞에서부터 최대 `limit`번 치환."),
    m(Str, "replace", "&String from, &String to, Int64 limit, Boolean reverse", "String", "최대 `limit`번 치환. `reverse`가 `true`면 뒤에서부터."),
    m(Str, "split", "&String splitter", "String[]", "`splitter`로 분할."),
    m(Str, "split", "&String splitter, Int64 max", "String[]", "최대 `max`개로 분할."),
    m(Str, "characters", "", "String[]", "한 문자씩 나눈 배열."),
    m(Str, "format", "DTVariable values...", "String", "`%str%`, `%number%` 자리표시자를 순서대로 치환 (11.1)."),
    m(Str, "formatWithInjection", "DTVariable values...", "String", "치환된 값 안의 자리표시자도 다시 치환하는 `format`."),
    m(Str, "parse", "Type type", "type", "문자열을 `type`의 값으로 변환. 실패하면 `NumberFormatException` 등."),
    m(Str, "matches", "&String pattern", "Boolean", "문자열 전체가 정규식 `pattern`에 맞는지 (14.15)."),
    m(Str, "containsMatch", "&String pattern", "Boolean", "정규식에 맞는 부분이 있는지."),
    m(Str, "findAll", "&String pattern", "String[]", "정규식에 맞는 부분들."),
    m(Str, "replaceRegex", "&String pattern, &String replacement", "String", "맞는 부분을 모두 치환. `replacement`에서 `$1`, `$name`으로 그룹을 쓴다."),
    m(Str, "splitRegex", "&String pattern", "String[]", "정규식에 맞는 부분을 구분자로 분할."),
    m(Str, "encode", "", "Bytes", "UTF-8 바이트 (14.5)."),
    m(Str, "encode", "&String encoding", "Bytes", "`encoding`(`\"utf-8\"`, `\"utf-16le\"`, `\"latin1\"` ...)으로 인코딩한 바이트."),
    m(Str, "append", "&DTVariable value", "void", "끝에 덧붙인다 (변경)."),
    m(Str, "prepend", "&DTVariable value", "void", "앞에 덧붙인다 (변경)."),
    m(Str, "randomize", "&String pattern", "void", "같은 길이의, 정규식 `pattern`에 맞는 무작위 문자열로 바꾼다 (변경)."),
    // ---------------------------------------------------------------- numbers (12.2, 12.3)
    m(Num, "format", "Int64 whole, Int64 decimal", "String", "정수부 / 소수부 자릿수로 포맷. `-1`은 제한 없음."),
    m(Num, "round", "", "Self", "정수 단위로 반올림 (0.5는 0에서 먼 쪽)."),
    m(Num, "round", "Int64 decimals", "Self", "소수점 `decimals`자리 단위로 반올림 (음수: 10의 거듭제곱 단위)."),
    m(Num, "round", "Int64 wholeDigits, Int64 decimals", "Self", "정수부는 `10^wholeDigits`, 소수부는 `10^-decimals` 단위로 각각 반올림."),
    m(Num, "ceil", "", "Self", "정수 단위로 올림 (+∞ 쪽)."),
    m(Num, "ceil", "Int64 decimals", "Self", "소수점 `decimals`자리 단위로 올림."),
    m(Num, "ceil", "Int64 wholeDigits, Int64 decimals", "Self", "정수부 / 소수부를 각각 올림."),
    m(Num, "floor", "", "Self", "정수 단위로 내림 (-∞ 쪽)."),
    m(Num, "floor", "Int64 decimals", "Self", "소수점 `decimals`자리 단위로 내림."),
    m(Num, "floor", "Int64 wholeDigits, Int64 decimals", "Self", "정수부 / 소수부를 각각 내림."),
    m(Num, "abs", "", "Self", "절댓값 (오버플로는 파일의 `IntegerOverflow` 정책을 따른다)."),
    m(Num, "addWrap", "Self b", "Self", "정책과 무관하게 순환하는 덧셈."),
    m(Num, "subWrap", "Self b", "Self", "정책과 무관하게 순환하는 뺄셈."),
    m(Num, "mulWrap", "Self b", "Self", "정책과 무관하게 순환하는 곱셈."),
    m(Num, "randomize", "Self start, Self end", "void", "`[start, end)`의 무작위 값으로 바꾼다 (변경)."),
    m(Num, "compareTo", "Self other", "Int32", "작으면 -1, 같으면 0, 크면 1."),
    m(Num, "toBytes", "", "Bytes", "타입 크기의 빅 엔디언 바이트 (14.5)."),
    m(Num, "toBytes", "&String order", "Bytes", "타입 크기의 바이트. `order`: `\"big\"` / `\"little\"`."),
    m(Num, "squareRoot", "", "Float", "제곱근 √x."),
    m(Num, "cubeRoot", "", "Float", "세제곱근 ∛x."),
    m(Num, "powerOf", "Float64 y", "Float", "x의 y제곱."),
    m(Num, "exponential", "", "Float", "e의 x제곱."),
    m(Num, "logarithm", "", "Float", "자연로그 ln x."),
    m(Num, "logarithm", "Float64 base", "Float", "밑이 `base`인 로그."),
    m(Num, "logarithmBase2", "", "Float", "밑이 2인 로그."),
    m(Num, "logarithmBase10", "", "Float", "밑이 10인 로그."),
    m(Num, "sine", "", "Float", "사인 (라디안)."),
    m(Num, "cosine", "", "Float", "코사인 (라디안)."),
    m(Num, "tangent", "", "Float", "탄젠트 (라디안)."),
    m(Num, "arcSine", "", "Float", "역사인 (라디안)."),
    m(Num, "arcCosine", "", "Float", "역코사인 (라디안)."),
    m(Num, "arcTangent", "", "Float", "역탄젠트 (라디안)."),
    m(Num, "arcTangent2", "Float64 x", "Float", "점 (x, y)의 각도 (수신자가 y), -π..π."),
    m(Num, "hyperbolicSine", "", "Float", "쌍곡사인."),
    m(Num, "hyperbolicCosine", "", "Float", "쌍곡코사인."),
    m(Num, "hyperbolicTangent", "", "Float", "쌍곡탄젠트."),
    m(Num, "hypotenuse", "Float64 y", "Float", "√(x² + y²) (중간 오버플로 없음)."),
    m(Num, "toRadians", "", "Float", "도 → 라디안."),
    m(Num, "toDegrees", "", "Float", "라디안 → 도."),
    m(Num, "sign", "", "Self", "부호: -1, 0, 1."),
    m(Num, "clamp", "Self min, Self max", "Self", "`min..max` 범위로 자른다."),
    m(Num, "truncate", "", "Self", "0 쪽으로 버림."),
    m(Num, "isNaN", "", "Boolean", "NaN인지."),
    m(Num, "isInfinite", "", "Boolean", "무한대인지."),
    m(Num, "isFinite", "", "Boolean", "유한한 값인지 (정수는 항상 `true`)."),
    m(Int, "greatestCommonDivisor", "Self b", "Self", "최대공약수."),
    m(Int, "leastCommonMultiple", "Self b", "Self", "최소공배수."),
    m(Int, "integerSquareRoot", "", "Self", "⌊√x⌋."),
    m(Int, "modularPower", "Self exponent, Self modulus", "Self", "x^exponent mod modulus."),
    // ---------------------------------------------------------------- arrays (13.3)
    m(Array, "length", "", "Int64", "원소 수."),
    m(Array, "isEmpty", "", "Boolean", "비어 있는지."),
    m(Array, "at", "Int64 index", "T", "원소 (`arr[index]`와 같다). 음수는 끝에서부터."),
    m(Array, "first", "", "T", "첫 원소."),
    m(Array, "first", "Int64 offset", "T", "앞에서 `offset`번째 원소."),
    m(Array, "last", "", "T", "마지막 원소."),
    m(Array, "last", "Int64 offset", "T", "뒤에서 `offset`번째 원소."),
    m(Array, "kv", "", "(Int64, T)[]", "(인덱스, 값) 쌍."),
    m(Array, "contains", "&T value", "Boolean", "`value`를 포함하는지."),
    m(Array, "set", "Int64 index, T value", "void", "원소를 바꾼다 (`arr[index] = value`)."),
    m(Array, "insert", "Int64 index, T value", "void", "`index` 위치에 삽입."),
    m(Array, "remove", "Int64 index", "T", "원소를 제거하고 소유권과 함께 돌려준다."),
    m(Array, "fill", "T values...", "void", "값들을 반복해 채운다."),
    m(Array, "reverse", "", "void", "순서를 뒤집는다."),
    m(Array, "sort", "", "void", "정렬 (원소가 `Comparable`이어야 한다)."),
    m(Array, "shuffle", "", "void", "무작위로 섞는다."),
    m(Array, "add", "T value", "void", "끝에 추가 (`append`, `push`와 같다)."),
    m(Array, "append", "T value", "void", "끝에 추가."),
    m(Array, "push", "T value", "void", "끝에 추가."),
    m(Array, "addAll", "&T[] other", "void", "`other`의 원소(복제)를 끝에 추가."),
    m(Array, "pop", "", "T", "마지막 원소를 제거하고 소유권과 함께 돌려준다."),
    m(Array, "clear", "", "void", "모두 제거."),
    m(Array, "swap", "Int64 i, Int64 j", "void", "두 원소를 바꾼다."),
    m(Array, "slice", "Int64 from", "T[]", "`from`부터 끝까지의 새 배열."),
    m(Array, "slice", "Int64 from, Int64 to", "T[]", "`[from, to)` 구간의 새 배열 (음수는 끝에서부터)."),
    m(Array, "indexOf", "&T value", "Int64", "처음 위치, 없으면 `-1`."),
    m(Array, "lastIndexOf", "&T value", "Int64", "마지막 위치, 없으면 `-1`."),
    m(Array, "concat", "&T[] other", "T[]", "이어 붙인 새 배열 (`a + b`)."),
    m(Array, "join", "", "String", "원소의 문자열 표현을 `\", \"`로 연결."),
    m(Array, "join", "&String separator", "String", "원소의 문자열 표현을 `separator`로 연결."),
    m(Array, "map", "(T value) -> R f", "R[]", "각 원소에 `f`를 적용한 새 배열. `f`는 `(value)` 또는 `(index, value)`를 받는다."),
    m(Array, "filter", "(T value) -> Boolean f", "T[]", "`f`가 `true`인 원소만 담은 새 배열."),
    m(Array, "reduce", "A init, (A acc, T value) -> A f", "A", "`acc = f(acc, value)`를 차례로 적용한 결과."),
    m(Array, "forEach", "(T value) -> void f", "void", "각 원소에 `f`를 실행."),
    m(Array, "find", "(T value) -> Boolean f", "T?", "`f`가 `true`인 첫 원소, 없으면 `null`."),
    m(Array, "findIndex", "(T value) -> Boolean f", "Int64", "`f`가 `true`인 첫 위치, 없으면 `-1`."),
    m(Array, "any", "(T value) -> Boolean f", "Boolean", "하나라도 `f`가 `true`인지."),
    m(Array, "all", "(T value) -> Boolean f", "Boolean", "모두 `f`가 `true`인지."),
    m(Array, "count", "(T value) -> Boolean f", "Int64", "`f`가 `true`인 원소 수."),
    m(Array, "sortBy", "(T a, T b) -> Int64 compare", "void", "비교 함수로 안정 정렬. 음수: `a`가 앞, 0: 같음, 양수: `b`가 앞."),
    // ---------------------------------------------------------------- Dictionary (13.4)
    m(Dict, "length", "", "Int64", "항목 수."),
    m(Dict, "isEmpty", "", "Boolean", "비어 있는지."),
    m(Dict, "kv", "", "(K, V)[]", "(키, 값) 쌍."),
    m(Dict, "containsKey", "&K key", "Boolean", "`key`가 있는지."),
    m(Dict, "keys", "", "K[]", "키 배열."),
    m(Dict, "values", "", "V[]", "값 배열."),
    m(Dict, "get", "&K key", "V", "`key`의 값 (`d[key]`와 같다)."),
    m(Dict, "set", "K key, V value", "void", "`key`의 값을 설정 (`d[key] = value`)."),
    m(Dict, "merge", "&Self other", "void", "`other`의 항목을 덮어써 합친다 (Python `update`)."),
    m(Dict, "remove", "&K key", "V", "`key`의 항목을 제거하고 값을 돌려준다."),
    // ---------------------------------------------------------------- function values
    m(Func, "run", "...", "R", "함수 값을 호출한다 (`f(...)`와 같다)."),
    // ---------------------------------------------------------------- static members (11.2, 12.1)
    m(StrStatic, "random", "&String pattern", "String", "정규식 `pattern` 전체에 맞는 무작위 문자열."),
    m(StrStatic, "random", "&String pattern, Int64 minLength, Int64 maxLength", "String", "길이가 `[minLength, maxLength]`인 무작위 문자열."),
    m(StrStatic, "array", "", "String[]", "빈 가변 길이 배열."),
    m(StrStatic, "array", "Int64 length, Boolean length_immutable", "String[]", "기본값으로 채운 길이 `length`의 배열."),
    c(NumStatic, "MAX", "Self", "이 타입의 최댓값."),
    c(NumStatic, "MIN", "Self", "이 타입의 최솟값 (부호 없는 타입은 0)."),
    m(NumStatic, "random", "Self start, Self end", "Self", "`[start, end)`의 무작위 값."),
    m(NumStatic, "range", "Self start, Self end", "Self[]", "`[start, end)` 범위 (`for`에서 쓴다)."),
    m(NumStatic, "range", "Self start, Self end, Self step", "Self[]", "`step` 간격의 범위."),
    m(NumStatic, "fromBytes", "&Bytes bytes", "Self", "빅 엔디언 바이트에서 읽은 값 (14.5)."),
    m(NumStatic, "fromBytes", "&Bytes bytes, &String order", "Self", "바이트에서 읽은 값. `order`: `\"big\"` / `\"little\"`."),
    m(NumStatic, "array", "", "Self[]", "빈 가변 길이 배열."),
    m(NumStatic, "array", "Int64 length, Boolean length_immutable", "Self[]", "0으로 채운 길이 `length`의 배열."),
    m(IntClassStatic, "max", "DTVariable values...", "Self", "가장 큰 값 (모든 숫자 타입 인자 허용)."),
    m(IntClassStatic, "min", "DTVariable values...", "Self", "가장 작은 값 (모든 숫자 타입 인자 허용)."),
    // ---------------------------------------------------------------- stdio (14.2)
    m(Stdio, "println", "&DTVariable value", "void", "출력한 뒤 줄을 바꾼다."),
    m(Stdio, "print", "&DTVariable value", "void", "줄바꿈 없이 출력."),
    m(Stdio, "read", "&String prompt", "String", "`prompt`를 출력하고 한 줄을 읽는다."),
    m(Stdio, "replaceLine", "&String text, Int64 lines_from_last", "void", "마지막에서 `lines_from_last`번째 줄을 `text`로 바꾼다."),
];

/// Members available on `recv`, in table order.
pub fn members(recv: Recv) -> impl Iterator<Item = &'static Member> {
    MEMBERS.iter().filter(move |x| x.recv == recv)
}

/// Replaces the placeholders of a built-in signature with the receiver's types.
pub fn substitute(text: &str, subst: &[(&str, &str)]) -> String {
    let mut out = String::new();
    let chars: Vec<char> = text.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        if chars[i].is_alphanumeric() || chars[i] == '_' {
            let start = i;
            while i < chars.len() && (chars[i].is_alphanumeric() || chars[i] == '_') {
                i += 1;
            }
            let word: String = chars[start..i].iter().collect();
            match subst.iter().find(|(k, _)| *k == word) {
                Some((_, v)) => out.push_str(v),
                None => out.push_str(&word),
            }
        } else {
            out.push(chars[i]);
            i += 1;
        }
    }
    out
}

/// Language keywords offered by completion.
pub const KEYWORDS: &[&str] = &[
    "function", "class", "interface", "extends", "implements", "public", "private", "protected", "static", "Immutable", "copied", "return", "if", "else",
    "for", "in", "break", "continue", "switch", "case", "default", "fallthrough", "try", "catch", "finally", "throw", "throws", "true", "false", "null", "this",
    "super", "and", "or", "not", "using", "as", "void", "new", "getter", "setter",
];

/// Built-in type names (spec 4).
pub const TYPES: &[(&str, &str)] = &[
    ("Int8", "8비트 부호 있는 정수"),
    ("Int16", "16비트 부호 있는 정수"),
    ("Int32", "32비트 부호 있는 정수"),
    ("Int64", "64비트 부호 있는 정수 (정수 리터럴의 기본 타입)"),
    ("Int", "`Int32`의 별칭"),
    ("UInt8", "8비트 부호 없는 정수"),
    ("UInt16", "16비트 부호 없는 정수"),
    ("UInt32", "32비트 부호 없는 정수"),
    ("UInt64", "64비트 부호 없는 정수"),
    ("IntLarge", "임의 정밀도 정수"),
    ("Float16", "16비트 부동소수점"),
    ("Float32", "32비트 부동소수점"),
    ("Float64", "64비트 부동소수점 (실수 리터럴의 기본 타입)"),
    ("Boolean", "`true` / `false`"),
    ("String", "UTF-8 문자열 (copy-on-write)"),
    ("Dictionary", "`Dictionary[K, V]`: 삽입 순서를 지키는 해시 맵"),
    ("DTVariable", "동적 타입 값 (4.9)"),
    ("STVariable", "선언 시 초기값으로 타입이 정해지는 변수 (4.9)"),
    ("Function", "`Function[(A, B), R]`: 함수 타입 (4.10)"),
];

/// The receiver class of a built-in type used in static position (`Int64.MAX`, `String.random`).
pub fn static_recv(type_name: &str) -> Option<Recv> {
    match type_name {
        "String" => Some(StrStatic),
        "Int8" | "Int16" | "Int32" | "Int64" | "Int" | "UInt8" | "UInt16" | "UInt32" | "UInt64" | "IntLarge" | "Float16" | "Float32" | "Float64" => Some(NumStatic),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn substitution_is_word_based() {
        assert_eq!(substitute("(T value) -> Boolean f", &[("T", "Int64")]), "(Int64 value) -> Boolean f");
        assert_eq!(substitute("&Self other", &[("Self", "String"), ("T", "x")]), "&String other");
        assert_eq!(substitute("Tensor T", &[("T", "Int64")]), "Tensor Int64");
    }
}
