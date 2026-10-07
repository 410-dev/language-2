//! Editor tooling (spec 15.4): diagnostics, hover, go to definition, completion, signature help
//! and document symbols on small programs.

use l2::check::ide::Want;
use l2::ide::{self, SymKind, Workspace};
use std::path::{Path, PathBuf};

const SRC: &str = r#"@using sdk 1
using stdio as stdio
using io.File as File

// A point in the plane.
class Point {
    // horizontal coordinate
    public Float64 x = 0.0
    public Float64 y = 0.0
    private Int64 secret = 0
    public Point(Float64 x, Float64 y) {
        this.x = x
        this.y = y
    }
    // distance from the origin
    public function Float64 length() {
        return x.hypotenuse(y)
    }
    public static function Point origin() {
        return new Point(0.0, 0.0)
    }
}

// doubles n
function Int64 twice(Int64 n) {
    return n * 2
}

function void main() {
    Point p = new Point(3.0, 4.0)
    Int64 count = twice(21)
    String name = "hello"
    Int64[] xs = [1, 2, 3]
    stdio.println(f"{p.length()} {count} {name.length()} {xs.length()}")
    //MARK
}
"#;

fn dir() -> PathBuf {
    let d = std::env::temp_dir().join(format!("l2-ide-test-{}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn main_path() -> PathBuf {
    dir().join("main.l2")
}

/// 1-based (line, column) of the `nth` occurrence of `needle`, plus `offset` columns.
fn pos(src: &str, needle: &str, nth: usize, offset: u32) -> (u32, u32) {
    let mut from = 0;
    let mut seen = 0;
    while let Some(k) = src[from..].find(needle) {
        let at = from + k;
        if seen == nth {
            let before = &src[..at];
            let line = before.matches('\n').count() as u32 + 1;
            let col = before.rsplit('\n').next().unwrap().chars().count() as u32 + 1;
            return (line, col + offset);
        }
        seen += 1;
        from = at + needle.len();
    }
    panic!("'{}' #{} not found", needle, nth)
}

fn with_line(src: &str, code: &str) -> String {
    src.replace("    //MARK", code)
}

fn analyze(src: &str) -> ide::Analysis {
    ide::analyze(&main_path(), src, &Workspace::default(), Want::Refs, None)
}

fn labels(items: &[ide::CompItem]) -> Vec<&str> {
    items.iter().map(|i| i.label.as_str()).collect()
}

#[test]
fn clean_program_has_no_diagnostics() {
    let a = analyze(SRC);
    assert!(a.diags.is_empty(), "{:?}", a.diags);
}

#[test]
fn syntax_errors_are_recovered() {
    let src = with_line(SRC, "    Int64 broken = 1 2\n    String s = 5\n    p.nothing()");
    let a = analyze(&src);
    let msgs: Vec<String> = a.diags.iter().map(|d| format!("{}:{} {}", d.span.line, d.span.col, d.msg)).collect();
    // the syntax error does not hide the type errors after it
    assert!(msgs.iter().any(|m| m.contains("expected")), "{:?}", msgs);
    assert!(msgs.iter().any(|m| m.contains("String")), "{:?}", msgs);
    assert!(msgs.iter().any(|m| m.contains("nothing")), "{:?}", msgs);
    // lexical errors are reported too
    let a = analyze(&with_line(SRC, "    String bad = \"unterminated\n    Int64 z = \"x\""));
    assert!(a.diags.len() >= 2, "{:?}", a.diags);
}

#[test]
fn hover_describes_names() {
    let a = analyze(SRC);
    let h = |needle: &str, nth: usize, off: u32| {
        let (l, c) = pos(SRC, needle, nth, off);
        ide::hover(&a, l, c).map(|h| h.markdown).unwrap_or_default()
    };
    let p = h("p.length", 0, 0);
    assert!(p.contains("Point p"), "{}", p);
    let m = h("p.length", 0, 3);
    assert!(m.contains("function Float64 Point.length()") && m.contains("distance from the origin"), "{}", m);
    let t = h("twice(21)", 0, 1);
    assert!(t.contains("function Int64 twice(Int64 n)") && t.contains("doubles n"), "{}", t);
    let b = h("name.length", 0, 6);
    assert!(b.contains("Int64 String.length()"), "{}", b);
    let x = h("this.x", 0, 5);
    assert!(x.contains("public Float64 x") && x.contains("horizontal coordinate"), "{}", x);
    let cls = h("Point p", 0, 1);
    assert!(cls.contains("public class Point") || cls.contains("class Point"), "{}", cls);
    assert!(cls.contains("A point in the plane."), "{}", cls);
    let ctor = h("new Point(3.0", 0, 5);
    assert!(ctor.contains("Point(Float64 x, Float64 y)"), "{}", ctor);
    let println = h("println", 0, 1);
    assert!(println.contains("stdio.println"), "{}", println);
    let param = h("n * 2", 0, 0);
    assert!(param.contains("Int64 n") && param.contains("parameter"), "{}", param);
    let decl = h("function Int64 twice", 0, 16);
    assert!(decl.contains("twice(Int64 n)"), "{}", decl);
}

#[test]
fn definitions_lead_to_declarations() {
    let a = analyze(SRC);
    let (l, c) = pos(SRC, "twice(21)", 0, 0);
    let (path, dl, dc, _) = ide::definition(&a, l, c).unwrap();
    assert_eq!(path, main_path());
    assert_eq!((dl, dc), pos(SRC, "twice(Int64", 0, 0));
    let (l, c) = pos(SRC, "p.length", 0, 0);
    let (_, dl, dc, _) = ide::definition(&a, l, c).unwrap();
    assert_eq!((dl, dc), pos(SRC, "p = new", 0, 0));
    // standard library declarations are opened from a read-only copy
    let src = with_line(SRC, "    String t = File.readText(\"x.txt\")");
    let a = analyze(&src);
    let (l, c) = pos(&src, "readText", 0, 0);
    let (path, dl, _, _) = ide::definition(&a, l, c).unwrap();
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(path.ends_with(Path::new("stdlib/io/File.l2")), "{}", path.display());
    assert!(text.lines().nth(dl as usize - 1).unwrap().contains("readText"));
}

fn complete_at(src: &str, needle: &str, off: u32) -> Vec<String> {
    let (l, c) = pos(src, needle, 0, off);
    let c = ide::complete(&main_path(), src, &Workspace::default(), l, c).expect("completion");
    labels(&c.items).iter().map(|s| s.to_string()).collect()
}

#[test]
fn member_completion() {
    let src = with_line(SRC, "    p.");
    let items = complete_at(&src, "    p.", 6);
    for want in ["x", "y", "length", "toString", "equals"] {
        assert!(items.iter().any(|i| i == want), "{} missing from {:?}", want, items);
    }
    assert!(!items.iter().any(|i| i == "secret" || i == "origin" || i.starts_with("__")), "{:?}", items);
    // a partly written name, in the middle of an argument list
    let src = with_line(SRC, "    stdio.println(name.su)");
    let items = complete_at(&src, "name.su", 7);
    assert!(items.iter().any(|i| i == "substring") && items.iter().any(|i| i == "toUpperCase"), "{:?}", items);
    let src = with_line(SRC, "    xs.");
    let items = complete_at(&src, "    xs.", 7);
    assert!(items.iter().any(|i| i == "map") && items.iter().any(|i| i == "add"), "{:?}", items);
    let src = with_line(SRC, "    stdio.");
    assert!(complete_at(&src, "    stdio.", 10).iter().any(|i| i == "println"));
    let src = with_line(SRC, "    Int64 m = Int64.");
    let items = complete_at(&src, "Int64.", 6);
    assert!(items.iter().any(|i| i == "MAX") && items.iter().any(|i| i == "random"), "{:?}", items);
    let src = with_line(SRC, "    Point q = Point.");
    let items = complete_at(&src, "Point.", 6);
    assert!(items.iter().any(|i| i == "origin") && !items.iter().any(|i| i == "length"), "{:?}", items);
    let src = with_line(SRC, "    String t = File.");
    let items = complete_at(&src, "File.", 5);
    assert!(items.iter().any(|i| i == "readText"), "{:?}", items);
    // inside the class: private members and `this.`
    let src = SRC.replace("        return x.hypotenuse(y)", "        Int64 s = this.\n        return x.hypotenuse(y)");
    let items = complete_at(&src, "this.\n", 5);
    assert!(items.iter().any(|i| i == "secret"), "{:?}", items);
}

#[test]
fn scope_completion() {
    let src = with_line(SRC, "    co");
    let items = complete_at(&src, "    co\n", 6);
    for want in ["count", "p", "name", "twice", "Point", "stdio", "Int64", "for"] {
        assert!(items.iter().any(|i| i == want), "{} missing from {:?}", want, items);
    }
    // a type in a declaration
    let src = with_line(SRC, "    Poi");
    let items = complete_at(&src, "    Poi\n", 7);
    assert!(items.iter().any(|i| i == "Point"), "{:?}", items);
    // `new` offers classes
    let src = with_line(SRC, "    Point q = new Po");
    let items = complete_at(&src, "new Po", 6);
    assert!(items.iter().any(|i| i == "Point"), "{:?}", items);
    // modules after `using`
    let src = SRC.replace("using io.File as File", "using io.");
    let items = complete_at(&src, "using io.", 9);
    assert!(items.iter().any(|i| i == "File") && items.iter().any(|i| i == "Directory"), "{:?}", items);
    let src = SRC.replace("using io.File as File", "using da");
    let items = complete_at(&src, "using da", 8);
    assert!(items.iter().any(|i| i == "data"), "{:?}", items);
}

fn sig_at(src: &str, needle: &str, off: u32) -> ide::SignatureHelp {
    let (l, c) = pos(src, needle, 0, off);
    ide::signature_help(&main_path(), src, &Workspace::default(), l, c).expect("signature help")
}

#[test]
fn signature_help() {
    let src = with_line(SRC, "    Int64 k = twice()");
    let h = sig_at(&src, "twice()", 6);
    assert_eq!(h.sigs[0].label, "function Int64 twice(Int64 n)");
    assert_eq!(h.active_param, 0);
    assert!(h.sigs[0].doc.as_deref().unwrap_or("").contains("doubles n") || h.sigs[0].def.is_some());
    // unfinished call, second argument
    let src = with_line(SRC, "    Point q = new Point(1.0, ");
    let h = sig_at(&src, "Point(1.0, ", 11);
    assert!(h.sigs[0].label.contains("Point(Float64 x, Float64 y)"), "{}", h.sigs[0].label);
    assert_eq!(h.active_param, 1);
    let src = with_line(SRC, "    String s = name.substring(1, )");
    let h = sig_at(&src, "substring(1, ", 13);
    assert_eq!(h.sigs.len(), 2);
    assert_eq!(h.active_param, 1);
    assert!(h.sigs[h.active_sig].params.len() == 2, "{:?}", h.sigs[h.active_sig].label);
    let src = with_line(SRC, "    stdio.println(p.length())");
    let h = sig_at(&src, "println(p", 8);
    assert!(h.sigs[0].label.contains("println"), "{}", h.sigs[0].label);
    let h = sig_at(&src, "println(p.length()", 17);
    assert!(h.sigs[0].label.contains("Point.length()"), "{}", h.sigs[0].label);
}

#[test]
fn document_symbols() {
    let syms = ide::document_symbols(SRC);
    let names: Vec<&str> = syms.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(names, ["Point", "twice", "main"]);
    let point = &syms[0];
    let members: Vec<&str> = point.children.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(members, ["x", "y", "secret", "Point", "length", "origin"]);
    assert_eq!(point.kind, SymKind::Class);
    assert_eq!(point.end.0, pos(SRC, "\n}\n\n// doubles", 0, 0).0);
}

#[test]
fn modules_of_a_package() {
    let root = dir().join("pkgtest");
    let lib = root.join("geo");
    std::fs::create_dir_all(&lib).unwrap();
    std::fs::write(root.join("app.l2"), "@using sdk 1\nusing geo.Shape as Shape\nfunction void main() {\n    Shape s = new Shape()\n}\n").unwrap();
    std::fs::write(lib.join("Shape.l2"), "@using sdk 1\npublic class Shape {\n    public Shape() {}\n    public function Int64 corners() { return Helper.count() }\n}\n").unwrap();
    let helper = "@using sdk 1\npublic class Helper {\n    public static function Int64 count() { return 3 }\n}\n";
    std::fs::write(lib.join("Helper.l2"), helper).unwrap();
    let ws = Workspace { roots: vec![root.clone()], ..Default::default() };
    let shape = std::fs::read_to_string(lib.join("Shape.l2")).unwrap();
    let m = ide::module_context(&lib.join("Shape.l2"), &shape, &ws);
    assert_eq!((m.base.as_deref(), m.module.as_str(), m.package.as_str()), (Some(root.as_path()), "geo.Shape", "geo"));
    // a module of a package sees the other types of its package without `using`
    let a = ide::analyze(&lib.join("Shape.l2"), &shape, &ws, Want::Refs, None);
    assert!(a.diags.is_empty(), "{:?}", a.diags);
    // unsaved changes of other files are used
    let mut ws = ws;
    ws.overlays.insert(lib.join("Helper.l2"), helper.replace("count()", "total()"));
    let a = ide::analyze(&lib.join("Shape.l2"), &shape, &ws, Want::Refs, None);
    assert!(a.diags.iter().any(|d| d.msg.contains("count")), "{:?}", a.diags);
}



fn l2_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for e in std::fs::read_dir(dir).unwrap().filter_map(|e| e.ok()) {
        let p = e.path();
        if p.is_dir() {
            l2_files(&p, out);
        } else if p.extension().map(|e| e == "l2").unwrap_or(false) {
            out.push(p);
        }
    }
}

/// Every source of the repository is analysed without internal errors; programs that compile
/// have no diagnostics, and hover / definition / completion work everywhere.
#[test]
fn repository_sources() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..").canonicalize().unwrap();
    let ws = Workspace { roots: vec![root.clone()], ..Default::default() };
    let mut files = Vec::new();
    for d in ["tests/programs", "tests/errors", "examples", "crates/l2/stdlib"] {
        l2_files(&root.join(d), &mut files);
    }
    files.push(root.join("crates/l2/src/prelude.l2"));
    files.sort();
    // panics inside the analysis are caught (and reported as an internal error); count them
    static PANICS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        PANICS.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        default_hook(info);
    }));
    let mut failures = Vec::new();
    for f in &files {
        let text = std::fs::read_to_string(f).unwrap();
        let a = ide::analyze(f, &text, &ws, Want::Refs, None);
        let errors: Vec<String> = a.diags.iter().filter(|d| d.severity == l2::diag::Severity::Error).map(|d| format!("{}:{} {}", d.span.line, d.span.col, d.msg)).collect();
        if errors.iter().any(|e| e.contains("internal error")) {
            failures.push(format!("{}: internal error", f.display()));
        }
        let must_be_clean = !f.starts_with(root.join("tests/errors"));
        if must_be_clean && !errors.is_empty() {
            failures.push(format!("{}: {:?}", f.display(), errors));
        }
        for (i, line) in text.lines().enumerate() {
            let chars: Vec<char> = line.chars().collect();
            for c in 0..chars.len() {
                if chars[c].is_alphanumeric() && (c == 0 || !(chars[c - 1].is_alphanumeric() || chars[c - 1] == '_')) {
                    let _ = ide::hover(&a, i as u32 + 1, c as u32 + 1);
                    let _ = ide::definition(&a, i as u32 + 1, c as u32 + 1);
                }
            }
            if i % 15 == 1 && !line.trim().is_empty() {
                // at the end of the line, and inside its second word
                let col = chars.len() as u32 + 1;
                let _ = ide::complete(f, &text, &ws, i as u32 + 1, col);
                let _ = ide::signature_help(f, &text, &ws, i as u32 + 1, col.saturating_sub(1).max(1));
                let words: Vec<usize> = (0..chars.len()).filter(|&c| chars[c].is_alphabetic() && (c == 0 || !chars[c - 1].is_alphanumeric())).collect();
                if let Some(&w) = words.get(1) {
                    let _ = ide::complete(f, &text, &ws, i as u32 + 1, w as u32 + 2);
                }
            }
        }
        let _ = ide::document_symbols(&text);
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
    assert_eq!(PANICS.load(std::sync::atomic::Ordering::SeqCst), 0, "the analysis panicked");
}

#[test]
fn inside_format_strings() {
    let src = with_line(SRC, "    stdio.println(f\"total: {p.}\")");
    let items = complete_at(&src, "{p.}", 3);
    assert!(items.iter().any(|i| i == "length"), "{:?}", items);
    let src = with_line(SRC, "    stdio.println(f\"total: {twice()}\")");
    let h = sig_at(&src, "{twice(", 7);
    assert_eq!(h.sigs[0].label, "function Int64 twice(Int64 n)");
    // plain text of a string offers nothing
    let (l, c) = pos(&src, "total", 0, 2);
    assert!(ide::complete(&main_path(), &src, &Workspace::default(), l, c).is_none());
}

#[test]
fn unused_generic_code() {
    let src = SRC.replace(
        "// doubles n",
        "class Box[T] {\n    private T value\n    public Box(T v) {\n        this.value = v\n    }\n    public function T get() {\n        T copy = value\n        return copy\n    }\n}\n\nfunction T firstOf[T](T[] xs) {\n    return xs[0]\n}\n\n// doubles n",
    );
    let a = analyze(&src);
    assert!(a.diags.is_empty(), "{:?}", a.diags);
    let (l, c) = pos(&src, "copy = value", 0, 7);
    let h = ide::hover(&a, l, c).expect("hover in a generic class").markdown;
    assert!(h.contains("value") && h.contains("field of"), "{}", h);
    let (l, c) = pos(&src, "xs[0]", 0, 0);
    assert!(ide::hover(&a, l, c).is_some());
    let src2 = src.replace("        T copy = value\n", "        T copy = value\n        this.\n");
    let items = complete_at(&src2, "this.\n", 5);
    assert!(items.iter().any(|i| i == "value") && items.iter().any(|i| i == "get"), "{:?}", items);
}

#[test]
fn type_positions() {
    // type argument of a declaration
    let src = with_line(SRC, "    Dictionary[String, Poi");
    let items = complete_at(&src, "String, Poi", 11);
    assert!(items.iter().any(|i| i == "Point"), "{:?}", items);
    // parameter type
    let src = SRC.replace("// doubles n", "function void take(Poi\n\n// doubles n");
    let items = complete_at(&src, "take(Poi", 8);
    assert!(items.iter().any(|i| i == "Point") && items.iter().any(|i| i == "Int64"), "{:?}", items);
    // field type in a class body
    let src = SRC.replace("    private Int64 secret = 0\n", "    private Int64 secret = 0\n    public Str\n");
    let items = complete_at(&src, "public Str\n", 10);
    assert!(items.iter().any(|i| i == "String"), "{:?}", items);
    // return type
    let src = SRC.replace("// doubles n", "function Poi\n\n// doubles n");
    let items = complete_at(&src, "function Poi\n", 12);
    assert!(items.iter().any(|i| i == "Point"), "{:?}", items);
}

