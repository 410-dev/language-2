//! Recording for editor tooling (spec 15.4). While `Checker::ide` is set, the checker notes what
//! the names of one file refer to (hover, go to definition) and, at a requested position, what
//! could be written there (completion) or which signatures a call may take (signature help).
//! Nothing is recorded, and nothing changes, when `ide` is `None` (normal compilation).

use super::*;
use crate::ast::ExprKind as A;
use crate::ide::builtins::{self, Recv};
use crate::ide::{text, CompItem, IdeRef, SigInfo, SymKind};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Want {
    /// Only references (hover, definition).
    Refs,
    /// Candidates for the name at the target position.
    Completion,
    /// Signatures of the call whose callee name is at the target position.
    Signature,
}

pub struct IdeRec {
    /// The analysed file.
    pub file: u32,
    /// Its text, by line.
    pub lines: Vec<Vec<char>>,
    pub refs: Vec<IdeRef>,
    /// (line, column) of the name that completion or signature help is about.
    pub target: Option<(u32, u32)>,
    pub want: Want,
    pub completion: Option<Vec<CompItem>>,
    pub signatures: Option<Vec<SigInfo>>,
    /// Callee names of the calls being checked: (position, name, recorded).
    calls: Vec<(Span, String, bool)>,
    seen: HashSet<(u32, u32)>,
}

impl IdeRec {
    pub fn new(file: u32, src: &str, want: Want, target: Option<(u32, u32)>) -> IdeRec {
        IdeRec {
            file,
            lines: src.split('\n').map(|l| l.trim_end_matches('\r').chars().collect()).collect(),
            refs: Vec::new(),
            target,
            want,
            completion: None,
            signatures: None,
            calls: Vec::new(),
            seen: HashSet::new(),
        }
    }
}

/// Removes package qualifiers from a type name: `data.collections.Set[io.File]` -> `Set[File]`.
pub fn short_name(s: &str) -> String {
    let mut out = String::new();
    let mut word = String::new();
    let flush = |word: &mut String, out: &mut String| {
        match word.rsplit_once('.') {
            Some((_, last)) => out.push_str(last),
            None => out.push_str(word),
        }
        word.clear();
    };
    for ch in s.chars() {
        if ch.is_alphanumeric() || ch == '_' || ch == '.' {
            word.push(ch);
        } else {
            flush(&mut word, &mut out);
            out.push(ch);
        }
    }
    flush(&mut word, &mut out);
    out
}

fn access_word(a: Access) -> &'static str {
    match a {
        Access::Public => "public ",
        Access::Private => "private ",
        Access::Protected => "protected ",
        Access::Default => "",
    }
}

/// Source text of a type expression.
pub fn type_text(t: &TypeExpr) -> String {
    match t {
        TypeExpr::Named { name, args, .. } => {
            if args.is_empty() {
                name.clone()
            } else {
                format!("{}[{}]", name, args.iter().map(type_text).collect::<Vec<_>>().join(", "))
            }
        }
        TypeExpr::Array(e) => format!("{}[]", type_text(e)),
        TypeExpr::Nullable(e) => format!("{}?", type_text(e)),
        TypeExpr::Union(ts) => ts.iter().map(type_text).collect::<Vec<_>>().join("|"),
        TypeExpr::Tuple(ts) => format!("({})", ts.iter().map(type_text).collect::<Vec<_>>().join(", ")),
        TypeExpr::Func(ps, r) => format!("Function[({}), {}]", ps.iter().map(type_text).collect::<Vec<_>>().join(", "), type_text(r)),
        TypeExpr::Ref { mutable, inner } => format!("{}{}", if *mutable { "*" } else { "&" }, type_text(inner)),
        TypeExpr::Wildcard(_) => "?".into(),
        TypeExpr::Void => "void".into(),
    }
}

fn type_params_text(tps: &[ast::TypeParam]) -> String {
    if tps.is_empty() {
        return String::new();
    }
    let parts: Vec<String> = tps
        .iter()
        .map(|tp| {
            let mut s = tp.name.clone();
            if let Some(b) = &tp.bound {
                s.push_str(&format!(" extends {}", type_text(b)));
            }
            if let Some(d) = &tp.default {
                s.push_str(&format!(" = {}", type_text(d)));
            }
            s
        })
        .collect();
    format!("[{}]", parts.join(", "))
}

/// `function Ret name[T](A a, B b)` of a declaration, as written.
pub fn decl_signature(d: &ast::FuncDecl, owner: Option<&str>) -> (String, Vec<String>) {
    let params: Vec<String> = d.params.iter().map(|p| format!("{} {}", type_text(&p.ty), p.name)).collect();
    let mut s = String::new();
    s.push_str(access_word(d.mods.access));
    if d.mods.is_static {
        s.push_str("static ");
    }
    let owner = owner.map(|o| format!("{}.", o)).unwrap_or_default();
    s.push_str(&format!("function {} {}{}{}({})", type_text(&d.ret), owner, d.name, type_params_text(&d.type_params), params.join(", ")));
    if !d.throws.is_empty() {
        s.push_str(&format!(" throws {}", d.throws.join(", ")));
    }
    (s, params)
}

impl<'a> Checker<'a> {
    // ------------------------------------------------------------------ basics
    fn ide_in_file(&self, span: Span) -> bool {
        self.ide.as_ref().map(|r| r.file == span.file && span.line > 0).unwrap_or(false)
    }

    fn ide_line(&self, line: u32) -> Option<&[char]> {
        let r = self.ide.as_ref()?;
        r.lines.get(line.checked_sub(1)? as usize).map(|l| l.as_slice())
    }

    /// Position of the name `name` written at or after `from` on the same line.
    pub fn ide_name_pos(&self, from: Span, name: &str) -> Option<Span> {
        if !self.ide_in_file(from) {
            return None;
        }
        let line = self.ide_line(from.line)?;
        let col = text::find_word(line, (from.col as usize).saturating_sub(1), name)?;
        Some(Span::new(from.file, from.line, col as u32 + 1))
    }

    /// Position of the member name after the `.` at `dot`.
    pub fn ide_member_pos(&self, dot: Span) -> Option<Span> {
        if !self.ide_in_file(dot) {
            return None;
        }
        let line = self.ide_line(dot.line)?;
        let mut i = dot.col as usize; // just past the dot (columns are 1-based)
        while i < line.len() && line[i].is_whitespace() {
            i += 1;
        }
        Some(Span::new(dot.file, dot.line, i as u32 + 1))
    }

    fn ide_is_target(&self, pos: Span) -> bool {
        match &self.ide {
            Some(r) if r.file == pos.file && r.want != Want::Refs && r.completion.is_none() && r.signatures.is_none() => r.target == Some((pos.line, pos.col)),
            _ => false,
        }
    }

    /// The module being checked: the current function's, else (declarations) the analysed file's.
    fn ide_module(&self) -> usize {
        if let Some(c) = self.fstack.last() {
            return c.module;
        }
        let file = self.ide.as_ref().map(|r| r.file);
        self.modules.iter().position(|m| Some(m.file) == file).unwrap_or(self.entry_module)
    }

    fn ide_want(&self) -> Want {
        self.ide.as_ref().map(|r| r.want).unwrap_or(Want::Refs)
    }

    #[allow(clippy::too_many_arguments)]
    fn ide_add(&mut self, pos: Span, len: usize, kind: SymKind, code: String, about: String, doc: Option<String>, def: Option<(Span, String)>) {
        let Some(r) = self.ide.as_mut() else { return };
        if pos.file != r.file || !r.seen.insert((pos.line, pos.col)) {
            return;
        }
        r.refs.push(IdeRef { line: pos.line, col: pos.col, len: len as u32, kind, code, about, doc, def });
    }

    /// Short name of a type for display.
    pub fn ide_tname(&self, t: &Type) -> String {
        short_name(&self.tname(t))
    }

    fn class_short(&self, c: ClassId) -> String {
        short_name(&self.classes[c as usize].name)
    }

    fn owner_name(&self, o: Owner) -> String {
        match o {
            Owner::Class(c) => self.class_short(c),
            Owner::Iface(i) => short_name(&self.ifaces[i as usize].name),
        }
    }

    // ------------------------------------------------------------------ symbol descriptions
    fn local_code(&self, name: &str, ty: &Type) -> String {
        format!("{} {}", self.ide_tname(ty), name)
    }

    fn method_text(&self, m: &MethodInfo) -> (String, Vec<String>) {
        if let (Some(gi), Owner::Class(c)) = (m.generic, m.owner) {
            if let Some(d) = self.cmeta[c as usize].generic_decls.get(gi) {
                return decl_signature(d, Some(&self.owner_name(m.owner)));
            }
        }
        let params: Vec<String> = m.params.iter().zip(m.param_names.iter()).map(|(t, n)| format!("{} {}", self.ide_tname(t), n)).collect();
        let mut s = String::new();
        s.push_str(access_word(m.access));
        if m.is_static {
            s.push_str("static ");
        }
        s.push_str(&format!("function {} {}.{}({})", self.ide_tname(&m.ret), self.owner_name(m.owner), m.name, params.join(", ")));
        (s, params)
    }

    fn fn_text(&self, fr: &FnRef<'a>) -> (String, Vec<String>) {
        if let Some(f) = fr.func {
            let sig = &self.sigs[f as usize];
            let params: Vec<String> = sig.params.iter().zip(sig.param_names.iter()).map(|(t, n)| format!("{} {}", self.ide_tname(t), n)).collect();
            return (format!("function {} {}({})", self.ide_tname(&sig.ret), fr.decl.name, params.join(", ")), params);
        }
        decl_signature(fr.decl, None)
    }

    fn ctor_text(&self, c: ClassId, k: usize) -> (String, Vec<String>, Span) {
        let (fid, ps, names, access, _) = &self.cmeta[c as usize].ctors[k];
        let params: Vec<String> = ps.iter().zip(names.iter()).map(|(t, n)| format!("{} {}", self.ide_tname(t), n)).collect();
        let cn = self.class_short(c);
        let simple = cn.split('[').next().unwrap_or(&cn).to_string();
        (format!("{}{}({})", access_word(*access), simple, params.join(", ")), params, self.funcs[*fid as usize].span)
    }

    fn class_code(&self, fqn: &str) -> (String, SymKind, Option<(Span, String)>) {
        if let Some((_, d)) = self.class_decls.get(fqn) {
            let mut s = format!("{}class {}{}", access_word(d.mods.access), d.name, type_params_text(&d.type_params));
            if let Some(e) = &d.extends {
                s.push_str(&format!(" extends {}", type_text(e)));
            }
            if !d.implements.is_empty() {
                s.push_str(&format!(" implements {}", d.implements.iter().map(type_text).collect::<Vec<_>>().join(", ")));
            }
            return (s, SymKind::Class, Some((d.span, d.name.clone())));
        }
        if let Some((_, d)) = self.iface_decls.get(fqn) {
            let mut s = format!("{}interface {}{}", access_word(d.mods.access), d.name, type_params_text(&d.type_params));
            if !d.extends.is_empty() {
                s.push_str(&format!(" extends {}", d.extends.iter().map(type_text).collect::<Vec<_>>().join(", ")));
            }
            return (s, SymKind::Interface, Some((d.span, d.name.clone())));
        }
        (fqn.to_string(), SymKind::Class, None)
    }

    fn package_about(fqn: &str) -> String {
        match fqn.rsplit_once('.') {
            Some((p, _)) => format!("package `{}`", p),
            None => String::new(),
        }
    }

    fn field_def(&self, owner: ClassId, name: &str) -> Option<(Span, String)> {
        let decl = self.cmeta[owner as usize].decl?;
        decl.fields.iter().find(|f| f.name == name).map(|f| (f.span, f.name.clone()))
    }

    fn field_code(&self, c: ClassId, idx: u32) -> (String, String, Option<(Span, String)>) {
        let f = &self.classes[c as usize].fields[idx as usize];
        let fm = &self.cmeta[c as usize].fields[idx as usize];
        let imm = if f.immutable { "Immutable " } else { "" };
        let code = format!("{}{}{} {}", access_word(fm.access), imm, self.ide_tname(&f.ty), f.name);
        (code, format!("field of `{}`", self.class_short(fm.owner)), self.field_def(fm.owner, &f.name))
    }

    fn static_owner(&self, c: ClassId, name: &str) -> ClassId {
        let mut cur = Some(c);
        while let Some(cc) = cur {
            if self.cmeta[cc as usize].statics.contains_key(name) {
                return cc;
            }
            cur = self.classes[cc as usize].parent;
        }
        c
    }

    fn static_code(&self, c: ClassId, name: &str, s: &StaticMeta) -> (String, String, Option<(Span, String)>) {
        let owner = self.static_owner(c, name);
        let ty = &self.globals[s.global as usize].ty;
        let imm = if s.immutable { "Immutable " } else { "" };
        (format!("{}static {}{} {}", access_word(s.access), imm, self.ide_tname(ty), name), format!("static field of `{}`", self.class_short(owner)), self.field_def(owner, name))
    }

    fn builtin_subst(&self, t: &Type) -> Vec<(&'static str, String)> {
        let t = t.deref().non_null();
        let mut v = vec![("Self", self.ide_tname(&t))];
        match &t {
            Type::Array(e) => v.push(("T", self.ide_tname(e))),
            Type::Dict(k, val) => {
                v.push(("K", self.ide_tname(k)));
                v.push(("V", self.ide_tname(val)));
            }
            Type::Float(_) => v.push(("Float", self.ide_tname(&t))),
            Type::Int(_) | Type::Big => v.push(("Float", "Float64".into())),
            _ => {}
        }
        v
    }

    /// Built-in member kinds available on a value of type `t`.
    fn builtin_recvs(t: &Type) -> Vec<Recv> {
        let t = t.deref().non_null();
        let mut v = match t {
            Type::Str => vec![Recv::Str],
            Type::Int(_) | Type::Big => vec![Recv::Num, Recv::Int],
            Type::Float(_) => vec![Recv::Num],
            Type::Array(_) => vec![Recv::Array],
            Type::Dict(..) => vec![Recv::Dict],
            Type::Func(..) => vec![Recv::Func],
            _ => vec![],
        };
        v.push(Recv::Any);
        v
    }

    fn builtin_desc(&self, m: &builtins::Member, subst: &[(&str, String)], owner: &str) -> (String, Vec<String>) {
        let sub: Vec<(&str, &str)> = subst.iter().map(|(k, v)| (*k, v.as_str())).collect();
        let ret = builtins::substitute(m.ret, &sub);
        match m.params {
            None => (format!("static {} {}.{}", ret, owner, m.name), Vec::new()),
            Some(p) => {
                let p = builtins::substitute(p, &sub);
                let params: Vec<String> = if p.is_empty() { Vec::new() } else { split_params(&p) };
                (format!("{} {}.{}({})", ret, owner, m.name, params.join(", ")), params)
            }
        }
    }

    // ------------------------------------------------------------------ references
    pub fn ide_local_decl(&mut self, id: LocalId, span: Span) {
        if !self.ide_in_file(span) {
            return;
        }
        let info = self.cur_ref().locals[id as usize].clone();
        if info.name.starts_with('$') || info.name == "this" || info.name == "_" {
            return;
        }
        let Some(pos) = self.ide_name_pos(span, &info.name) else { return };
        let code = self.local_code(&info.name, &info.ty);
        self.ide_add(pos, info.name.chars().count(), SymKind::Variable, code, "local variable".into(), None, Some((pos, info.name.clone())));
    }

    /// Marks the local declared last as a parameter.
    pub fn ide_mark_param(&mut self, id: LocalId) {
        let name = self.cur_ref().locals[id as usize].name.clone();
        if let Some(r) = self.ide.as_mut() {
            if let Some(last) = r.refs.last_mut() {
                if last.kind == SymKind::Variable && last.def.as_ref().map(|d| d.1 == name).unwrap_or(false) {
                    last.kind = SymKind::Parameter;
                    last.about = "parameter".into();
                }
            }
        }
    }

    pub fn ide_local_use(&mut self, id: LocalId, pos: Span) {
        if !self.ide_in_file(pos) {
            return;
        }
        let info = self.cur_ref().locals[id as usize].clone();
        let def = self.ide_name_pos(info.span, &info.name).unwrap_or(info.span);
        let kind = self.ide.as_ref().and_then(|r| r.refs.iter().find(|x| x.line == def.line && x.col == def.col).map(|x| (x.kind, x.about.clone())));
        let (kind, about) = kind.unwrap_or((SymKind::Variable, "local variable".into()));
        let code = self.local_code(&info.name, &info.ty);
        self.ide_add(pos, info.name.chars().count(), kind, code, about, None, Some((def, info.name.clone())));
    }

    pub fn ide_field_use(&mut self, c: ClassId, idx: u32, pos: Span) {
        if !self.ide_in_file(pos) {
            return;
        }
        let (code, about, def) = self.field_code(c, idx);
        let len = self.classes[c as usize].fields[idx as usize].name.chars().count();
        self.ide_add(pos, len, SymKind::Field, code, about, None, def);
    }

    pub fn ide_static_use(&mut self, c: ClassId, name: &str, s: &StaticMeta, pos: Span) {
        if !self.ide_in_file(pos) {
            return;
        }
        let (code, about, def) = self.static_code(c, name, s);
        self.ide_add(pos, name.chars().count(), SymKind::Field, code, about, None, def);
    }

    /// A type name written at `pos` that resolved to `fqn`.
    pub fn ide_type_use(&mut self, fqn: &str, pos: Span) {
        if !self.ide_in_file(pos) {
            return;
        }
        let (code, kind, def) = self.class_code(fqn);
        let len = fqn.rsplit('.').next().unwrap_or(fqn).chars().count();
        let about = Self::package_about(fqn);
        self.ide_add(pos, len, kind, code, about, None, def);
    }

    /// A type written as `name` at `span` that resolved to `ty`.
    pub fn ide_type_named(&mut self, name: &str, span: Span, ty: &Type, subst: &Subst) {
        if !self.ide_in_file(span) {
            return;
        }
        let last = name.rsplit('.').next().unwrap_or(name);
        let pos = if name.contains('.') { self.ide_name_pos(span, last).unwrap_or(span) } else { span };
        self.ide_target_type(pos, None);
        if subst.contains_key(name) {
            let code = format!("type parameter {} = {}", name, self.ide_tname(ty));
            self.ide_add(pos, name.chars().count(), SymKind::TypeParam, code, String::new(), None, None);
            return;
        }
        if builtins::TYPES.iter().any(|(n, _)| *n == name) {
            self.ide_builtin_type_use(name, pos);
            return;
        }
        let fqn = match ty {
            Type::Class(c) => self.cmeta[*c as usize].template.clone(),
            Type::Iface(i) => self.imeta[*i as usize].template.clone(),
            _ => return,
        };
        self.ide_type_use(&fqn, pos);
    }

    pub fn ide_builtin_type_use(&mut self, name: &str, pos: Span) {
        if !self.ide_in_file(pos) {
            return;
        }
        let doc = builtins::TYPES.iter().find(|(n, _)| *n == name).map(|(_, d)| d.to_string());
        self.ide_add(pos, name.chars().count(), SymKind::BuiltinType, format!("built-in type {}", name), String::new(), doc, None);
    }

    pub fn ide_module_use(&mut self, alias: &str, imp: &Import, pos: Span) {
        if !self.ide_in_file(pos) {
            return;
        }
        let code = match imp {
            Import::Stdio => "using stdio".to_string(),
            Import::Intrinsics => "using intrinsics".to_string(),
            Import::Module(mi) => format!("using {} as {}", self.modules[*mi].name, alias),
            Import::Package(p) => format!("using {} as {}", p, alias),
        };
        let def = match imp {
            Import::Module(mi) => Some((Span::new(self.modules[*mi].file, 1, 1), String::new())),
            _ => None,
        };
        self.ide_add(pos, alias.chars().count(), SymKind::Module, code, String::new(), None, def);
    }

    // ------------------------------------------------------------------ calls
    pub fn ide_call_push(&mut self, pos: Option<Span>, name: &str) {
        if let (Some(r), Some(p)) = (self.ide.as_mut(), pos) {
            r.calls.push((p, name.to_string(), false));
        }
    }

    pub fn ide_call_pop(&mut self) {
        if let Some(r) = self.ide.as_mut() {
            r.calls.pop();
        }
    }

    /// The position of the innermost call being checked, if its callee is named `name` and
    /// was not recorded yet.
    fn ide_call_take(&mut self, name: &str) -> Option<Span> {
        let r = self.ide.as_mut()?;
        let top = r.calls.last_mut()?;
        if top.1 != name || top.2 {
            return None;
        }
        top.2 = true;
        Some(top.0)
    }

    pub fn ide_method_called(&mut self, m: &MethodInfo) {
        if self.ide.is_none() {
            return;
        }
        let Some(pos) = self.ide_call_take(&m.name) else { return };
        let (code, _) = self.method_text(m);
        let about = match m.owner {
            Owner::Class(_) => format!("method of `{}`", self.owner_name(m.owner)),
            Owner::Iface(_) => format!("method of interface `{}`", self.owner_name(m.owner)),
        };
        self.ide_add(pos, m.name.chars().count(), SymKind::Method, code, about, None, Some((m.span, m.name.clone())));
    }

    pub fn ide_function_called(&mut self, fr: &FnRef<'a>, fid: Option<FuncId>) {
        if self.ide.is_none() {
            return;
        }
        let Some(pos) = self.ide_call_take(&fr.decl.name) else { return };
        let fr = FnRef { decl: fr.decl, module: fr.module, func: fid.or(fr.func) };
        let (code, _) = self.fn_text(&fr);
        let about = if self.modules[fr.module].name.is_empty() || fr.module == self.entry_module { String::new() } else { format!("module `{}`", self.modules[fr.module].name) };
        self.ide_add(pos, fr.decl.name.chars().count(), SymKind::Function, code, about, None, Some((fr.decl.span, fr.decl.name.clone())));
    }

    /// `Point(...)` / `new Point(...)`: the constructor `fid` of `c`.
    pub fn ide_ctor_called(&mut self, c: ClassId, k: usize) {
        if self.ide.is_none() {
            return;
        }
        let fqn = self.cmeta[c as usize].template.clone();
        let simple = fqn.rsplit('.').next().unwrap_or(&fqn).to_string();
        let Some(pos) = self.ide_call_take(&simple) else { return };
        let (code, _, span) = self.ctor_text(c, k);
        self.ide_add(pos, simple.chars().count(), SymKind::Constructor, code, format!("constructor of `{}`", simple), None, Some((span, simple.clone())));
    }

    pub fn ide_builtin_called(&mut self, recv: &Type, name: &str, nargs: usize) {
        if self.ide.is_none() {
            return;
        }
        let Some(pos) = self.ide_call_take(name) else { return };
        let recvs = Self::builtin_recvs(recv);
        let cands: Vec<&builtins::Member> = builtins::MEMBERS.iter().filter(|m| recvs.contains(&m.recv) && m.name == name).collect();
        let Some(m) = cands.iter().find(|m| m.params.map(|p| param_count(p) == nargs).unwrap_or(false)).or(cands.first()) else { return };
        let subst = self.builtin_subst(recv);
        let (code, _) = self.builtin_desc(m, &subst, &self.ide_tname(&recv.deref().non_null()));
        self.ide_add(pos, name.chars().count(), SymKind::BuiltinMethod, code, String::new(), Some(m.doc.to_string()), None);
    }

    /// `Int64.random(...)`, `stdio.println(...)`: a built-in static member called by name.
    pub fn ide_static_builtin_called(&mut self, recvs: &[Recv], owner: &str, name: &str, nargs: usize) {
        if self.ide.is_none() {
            return;
        }
        let Some(pos) = self.ide_call_take(name) else { return };
        self.ide_static_builtin_at(recvs, owner, name, nargs, pos);
    }

    /// `Int64.MAX`: a built-in constant.
    pub fn ide_static_builtin_at(&mut self, recvs: &[Recv], owner: &str, name: &str, nargs: usize, pos: Span) {
        if !self.ide_in_file(pos) {
            return;
        }
        let cands: Vec<&builtins::Member> = builtins::MEMBERS.iter().filter(|m| recvs.contains(&m.recv) && m.name == name).collect();
        let Some(m) = cands.iter().find(|m| m.params.map(|p| param_count(p) == nargs).unwrap_or(true)).or(cands.first()) else { return };
        let subst = vec![("Self", owner.to_string())];
        let (code, _) = self.builtin_desc(m, &subst, owner);
        self.ide_add(pos, name.chars().count(), SymKind::BuiltinMethod, code, String::new(), Some(m.doc.to_string()), None);
    }

    // ------------------------------------------------------------------ declarations
    /// References for the declarations of the analysed file (classes, members, functions).
    pub fn ide_record_decls(&mut self) {
        let Some(file) = self.ide.as_ref().map(|r| r.file) else { return };
        let Some(mi) = self.modules.iter().position(|m| m.file == file) else { return };
        // types
        let mut types: Vec<String> = self.class_decls.iter().filter(|(_, (m, _))| *m == mi).map(|(n, _)| n.clone()).collect();
        types.extend(self.iface_decls.iter().filter(|(_, (m, _))| *m == mi).map(|(n, _)| n.clone()));
        for fqn in types {
            let (code, kind, def) = self.class_code(&fqn);
            if let Some((span, name)) = def.clone() {
                if let Some(pos) = self.ide_name_pos(span, &name) {
                    self.ide_add(pos, name.chars().count(), kind, code, Self::package_about(&fqn), None, def);
                }
            }
        }
        // members of the non-generic classes (generic ones are described where they are used)
        for c in 0..self.cmeta.len() {
            let Some(d) = self.cmeta[c].decl else { continue };
            if self.cmeta[c].module != mi || !self.cmeta[c].targs.is_empty() {
                continue;
            }
            let c = c as ClassId;
            for f in &d.fields {
                let Some(pos) = self.ide_name_pos(f.span, &f.name) else { continue };
                if let Some(idx) = self.field_index(c, &f.name) {
                    let (code, about, def) = self.field_code(c, idx);
                    self.ide_add(pos, f.name.chars().count(), SymKind::Field, code, about, None, def);
                } else if let Some(s) = self.find_static(c, &f.name) {
                    let (code, about, def) = self.static_code(c, &f.name, &s);
                    self.ide_add(pos, f.name.chars().count(), SymKind::Field, code, about, None, def);
                }
            }
            let methods = self.cmeta[c as usize].methods.clone();
            for m in methods.iter().filter(|m| m.owner == Owner::Class(c)) {
                let Some(pos) = self.ide_name_pos(m.span, &m.name) else { continue };
                let (code, _) = self.method_text(m);
                let about = format!("method of `{}`", self.class_short(c));
                self.ide_add(pos, m.name.chars().count(), SymKind::Method, code, about, None, Some((m.span, m.name.clone())));
            }
            for k in 0..self.cmeta[c as usize].ctors.len() {
                let (code, _, span) = self.ctor_text(c, k);
                let simple = d.name.clone();
                if let Some(pos) = self.ide_name_pos(span, &simple) {
                    self.ide_add(pos, simple.chars().count(), SymKind::Constructor, code, format!("constructor of `{}`", simple), None, Some((span, simple.clone())));
                }
            }
        }
        for i in 0..self.imeta.len() {
            if self.imeta[i].module != mi || !self.imeta[i].targs.is_empty() {
                continue;
            }
            let methods = self.imeta[i].methods.clone();
            for m in methods.iter().filter(|m| m.owner == Owner::Iface(i as IfaceId)) {
                let Some(pos) = self.ide_name_pos(m.span, &m.name) else { continue };
                let (code, _) = self.method_text(m);
                let about = format!("method of interface `{}`", self.owner_name(m.owner));
                self.ide_add(pos, m.name.chars().count(), SymKind::Method, code, about, None, Some((m.span, m.name.clone())));
            }
        }
        // `using a.b.C [as D]`: the imported type or module
        let usings: Vec<&'a ast::UsingDecl> = self.files[mi].items.iter().filter_map(|it| if let Item::Using(u) = it { Some(u) } else { None }).collect();
        for u in usings {
            let last = u.path.rsplit('.').next().unwrap_or(&u.path).to_string();
            let mut spots: Vec<(Span, usize)> = Vec::new();
            if let Some(p) = self.ide_name_pos(u.span, &last) {
                spots.push((p, last.chars().count()));
                if let Some(alias) = &u.alias {
                    if let Some(a) = self.ide_name_pos(Span::new(p.file, p.line, p.col + last.chars().count() as u32), alias) {
                        spots.push((a, alias.chars().count()));
                    }
                }
            }
            if self.class_decls.contains_key(&u.path) || self.iface_decls.contains_key(&u.path) {
                let (code, kind, def) = self.class_code(&u.path);
                for (p, len) in spots {
                    self.ide_add(p, len, kind, code.clone(), Self::package_about(&u.path), None, def.clone());
                }
            } else if let Some(m) = self.modules.iter().find(|m| m.name == u.path) {
                let def = Some((Span::new(m.file, 1, 1), String::new()));
                for (p, len) in spots {
                    self.ide_add(p, len, SymKind::Module, format!("module {}", u.path), String::new(), None, def.clone());
                }
            }
        }
        // functions
        let funcs: Vec<FnRef<'a>> = self.modules[mi].funcs.values().flatten().cloned().collect();
        for fr in funcs {
            let Some(pos) = self.ide_name_pos(fr.decl.span, &fr.decl.name) else { continue };
            let (code, _) = self.fn_text(&fr);
            self.ide_add(pos, fr.decl.name.chars().count(), SymKind::Function, code, String::new(), None, Some((fr.decl.span, fr.decl.name.clone())));
        }
    }

    // ------------------------------------------------------------------ unused generics
    /// A type argument for checking a generic declaration that is never instantiated: the
    /// default, else the first of a few types that satisfies the bound.
    fn ide_sample_type(&mut self, default: Option<&TypeExpr>, bound: Option<&TypeExpr>, mi: usize) -> Type {
        if let Some(d) = default {
            let t = self.resolve_type(d, mi, &Subst::new());
            if !t.is_error() {
                return t;
            }
        }
        let Some(b) = bound else { return Type::Dyn };
        let mut cands = vec![self.resolve_type(b, mi, &Subst::new())];
        cands.extend([Type::Float(l2_runtime::FloatTy::F64), Type::int64(), Type::Str]);
        for t in cands {
            if !t.is_error() && self.satisfies_bound(&t, b, mi, Span::default()) {
                return t;
            }
        }
        Type::Dyn
    }

    /// Checks the generic classes and functions of the analysed file that the program does not
    /// instantiate, with sample type arguments, so that their bodies have hover and completion.
    /// Their diagnostics are dropped (they depend on the type arguments); the lines they span are
    /// returned so that later checks can ignore them too.
    pub fn ide_check_generics(&mut self) -> Vec<(u32, u32)> {
        let Some(file) = self.ide.as_ref().map(|r| r.file) else { return Vec::new() };
        let Some(mi) = self.modules.iter().position(|m| m.file == file) else { return Vec::new() };
        let (nd, nw) = (self.diags.len(), self.warnings.len());
        let mut spans: Vec<Span> = Vec::new();
        let mut classes: Vec<String> = self
            .class_decls
            .iter()
            .filter(|(n, (m, d))| *m == mi && !d.type_params.is_empty() && !self.cmeta.iter().any(|c| &c.template == *n))
            .map(|(n, _)| n.clone())
            .collect();
        classes.sort();
        for n in classes {
            let d = self.class_decls[&n].1;
            spans.push(d.span);
            let mut targs = Vec::new();
            for tp in &d.type_params {
                targs.push(self.ide_sample_type(tp.default.as_ref(), tp.bound.as_ref(), mi));
            }
            self.instantiate_class(&n, targs, d.span);
        }
        let fns: Vec<FnRef<'a>> = self.modules[mi].funcs.values().flatten().filter(|f| f.func.is_none()).cloned().collect();
        for fr in fns {
            let ptr = fr.decl as *const ast::FuncDecl as usize;
            if self.func_inst.keys().any(|(p, _)| *p == ptr) {
                continue;
            }
            spans.push(fr.decl.span);
            let mut targs = Vec::new();
            for (_, bound) in generic_params(fr.decl) {
                targs.push(self.ide_sample_type(None, bound.as_ref(), fr.module));
            }
            self.instantiate_generic(&fr, targs, fr.decl.span);
        }
        self.process_queue();
        self.diags.truncate(nd);
        self.warnings.truncate(nw);
        let text: String = self.ide.as_ref().map(|r| r.lines.iter().map(|l| l.iter().collect::<String>()).collect::<Vec<_>>().join("\n")).unwrap_or_default();
        spans.iter().map(|s| (s.line, text::block_end(&text, s.line, s.col).map(|e| e.0 + 1).unwrap_or(s.line))).collect()
    }

    // ------------------------------------------------------------------ completion targets
    /// An identifier at `pos` (expression or assignment target). `call`: it is a callee.
    pub fn ide_target_ident(&mut self, name: &str, pos: Span, call: bool) {
        if !self.ide_is_target(pos) {
            return;
        }
        match self.ide_want() {
            Want::Completion => {
                let items = self.ide_scope_items();
                self.ide.as_mut().unwrap().completion = Some(items);
            }
            Want::Signature if call => {
                let sigs = self.ide_ident_sigs(name);
                self.ide.as_mut().unwrap().signatures = Some(sigs);
            }
            _ => {}
        }
    }

    /// `obj.name` with the name at `pos`. `call`: it is a method call.
    pub fn ide_target_member(&mut self, obj: &'a ast::Expr, name: &str, pos: Option<Span>, call: bool) {
        let Some(pos) = pos else { return };
        if !self.ide_is_target(pos) {
            return;
        }
        let want = self.ide_want();
        if want == Want::Signature && !call {
            return;
        }
        // nothing else may claim the target while the receiver is analysed
        let saved = self.ide.as_mut().unwrap().target.take();
        let src = self.ide_member_source(obj);
        self.ide.as_mut().unwrap().target = saved;
        match want {
            Want::Completion => {
                let items = self.ide_member_items(&src);
                self.ide.as_mut().unwrap().completion = Some(items);
            }
            Want::Signature => {
                let sigs = self.ide_member_sigs(&src, name);
                self.ide.as_mut().unwrap().signatures = Some(sigs);
            }
            Want::Refs => {}
        }
    }

    /// A type name at `pos` (declarations, parameters, `new`).
    pub fn ide_target_type(&mut self, pos: Span, ctor: Option<&str>) {
        if !self.ide_is_target(pos) {
            return;
        }
        match self.ide_want() {
            Want::Completion => {
                let m = self.ide_module();
                let items = self.ide_type_items(m, ctor.is_some());
                self.ide.as_mut().unwrap().completion = Some(items);
            }
            Want::Signature => {
                if let Some(path) = ctor {
                    let sigs = self.ide_ctor_sigs_named(path);
                    self.ide.as_mut().unwrap().signatures = Some(sigs);
                }
            }
            Want::Refs => {}
        }
    }

    // ------------------------------------------------------------------ candidates
    fn ide_visible(&self, access: Access, owner: ClassId) -> bool {
        match access {
            Access::Public | Access::Default => true,
            Access::Private => self.current_class_pub() == Some(owner),
            Access::Protected => self.current_class_pub().map(|c| self.is_subclass(c, owner)).unwrap_or(false),
        }
    }

    fn method_items(&self, ms: &[MethodInfo], statics: Option<bool>, out: &mut Vec<CompItem>) {
        let mut seen: HashMap<String, usize> = HashMap::new();
        for m in ms {
            if m.name.starts_with("__") || m.name.starts_with("operator") || m.name == "drop" {
                continue;
            }
            if let Some(s) = statics {
                if m.is_static != s {
                    continue;
                }
            }
            if let Owner::Class(oc) = m.owner {
                if !self.ide_visible(m.access, oc) {
                    continue;
                }
            }
            if let Some(&i) = seen.get(&m.name) {
                let n = out[i].detail.matches(" (+").count();
                if n == 0 {
                    out[i].detail.push_str(" (+1 overload)");
                } else if let Some(p) = out[i].detail.rfind(" (+") {
                    let k: usize = out[i].detail[p + 3..].split(' ').next().and_then(|x| x.parse().ok()).unwrap_or(1);
                    out[i].detail.truncate(p);
                    out[i].detail.push_str(&format!(" (+{} overloads)", k + 1));
                }
                continue;
            }
            let (code, params) = self.method_text(m);
            seen.insert(m.name.clone(), out.len());
            out.push(CompItem {
                label: m.name.clone(),
                kind: SymKind::Method,
                detail: code,
                doc: None,
                def: Some((m.span, m.name.clone())),
                takes_args: Some(!params.is_empty()),
            });
        }
    }

    /// Methods of a class or interface, inherited ones included.
    fn all_methods(&self, owner: Owner) -> Vec<MethodInfo> {
        let mut out: Vec<MethodInfo> = Vec::new();
        let mut push = |ms: &[MethodInfo]| {
            for m in ms {
                if !out.iter().any(|x| x.name == m.name && x.params == m.params && x.param_names.len() == m.param_names.len()) {
                    out.push(m.clone());
                }
            }
        };
        match owner {
            Owner::Class(c) => {
                let mut cur = Some(c);
                while let Some(cc) = cur {
                    push(&self.cmeta[cc as usize].methods);
                    cur = self.classes[cc as usize].parent;
                }
                for &i in &self.classes[c as usize].ifaces {
                    push(&self.imeta[i as usize].methods);
                }
            }
            Owner::Iface(i) => {
                let mut anc = Vec::new();
                self.iface_ancestors(i, &mut anc);
                for a in anc {
                    push(&self.imeta[a as usize].methods);
                }
            }
        }
        out
    }

    fn builtin_items(&self, recvs: &[Recv], subst: &[(&str, String)], owner: &str, out: &mut Vec<CompItem>) {
        for m in builtins::MEMBERS.iter().filter(|m| recvs.contains(&m.recv)) {
            if out.iter().any(|x| x.label == m.name) {
                continue;
            }
            let (code, params) = self.builtin_desc(m, subst, owner);
            out.push(CompItem {
                label: m.name.to_string(),
                kind: if m.params.is_none() { SymKind::Constant } else { SymKind::BuiltinMethod },
                detail: code,
                doc: Some(m.doc.to_string()),
                def: None,
                takes_args: m.params.map(|_| !params.is_empty() || builtins::MEMBERS.iter().any(|x| x.recv == m.recv && x.name == m.name && x.params.map(|p| !p.is_empty()).unwrap_or(false))),
            });
        }
    }

    fn instance_items(&self, t: &Type, out: &mut Vec<CompItem>) {
        let t = t.deref().non_null();
        match &t {
            Type::Class(c) => {
                for (i, f) in self.classes[*c as usize].fields.iter().enumerate() {
                    let fm = &self.cmeta[*c as usize].fields[i];
                    if !self.ide_visible(fm.access, fm.owner) || f.name.starts_with('$') || f.name.starts_with("__") {
                        continue;
                    }
                    let (code, _, def) = self.field_code(*c, i as u32);
                    out.push(CompItem { label: f.name.clone(), kind: SymKind::Field, detail: code, doc: None, def, takes_args: None });
                }
                let ms = self.all_methods(Owner::Class(*c));
                self.method_items(&ms, Some(false), out);
            }
            Type::Iface(i) => {
                let ms = self.all_methods(Owner::Iface(*i));
                self.method_items(&ms, Some(false), out);
            }
            _ => {}
        }
        let subst = self.builtin_subst(&t);
        self.builtin_items(&Self::builtin_recvs(&t), &subst, &self.ide_tname(&t), out);
    }

    fn static_items(&self, c: ClassId, out: &mut Vec<CompItem>) {
        let mut cur = Some(c);
        while let Some(cc) = cur {
            let mut names: Vec<(&String, &StaticMeta)> = self.cmeta[cc as usize].statics.iter().collect();
            names.sort_by(|a, b| a.0.cmp(b.0));
            for (n, s) in names {
                if n.starts_with("__") || !self.ide_visible(s.access, cc) || out.iter().any(|x| &x.label == n) {
                    continue;
                }
                let (code, _, def) = self.static_code(c, n, s);
                out.push(CompItem { label: n.clone(), kind: SymKind::Field, detail: code, doc: None, def, takes_args: None });
            }
            cur = self.classes[cc as usize].parent;
        }
        let ms = self.all_methods(Owner::Class(c));
        self.method_items(&ms, Some(true), out);
        let cn = self.class_short(c);
        out.push(CompItem {
            label: "array".into(),
            kind: SymKind::BuiltinMethod,
            detail: format!("{}[] {}.array()", cn, cn),
            doc: Some("배열 생성: `array()`는 빈 가변 길이 배열, `array(length = n)`은 길이 n의 배열.".into()),
            def: None,
            takes_args: Some(false),
        });
    }

    fn type_item(&self, fqn: &str, label: String) -> CompItem {
        let (code, kind, def) = self.class_code(fqn);
        CompItem { label, kind, detail: code, doc: None, def, takes_args: None }
    }

    /// Types visible by simple name in module `m`.
    fn ide_type_items(&self, m: usize, classes_only: bool) -> Vec<CompItem> {
        let mut out: Vec<CompItem> = Vec::new();
        let mut names: Vec<&String> = self.class_decls.keys().collect();
        if !classes_only {
            names.extend(self.iface_decls.keys());
        }
        names.sort();
        for fqn in names {
            let simple = fqn.rsplit('.').next().unwrap_or(fqn);
            if simple.starts_with("__") || out.iter().any(|x| x.label == simple) {
                continue;
            }
            if self.find_type(m, simple).ok().flatten().as_deref() == Some(fqn.as_str()) {
                out.push(self.type_item(fqn, simple.to_string()));
            }
        }
        if !classes_only {
            for (n, d) in builtins::TYPES {
                out.push(CompItem { label: n.to_string(), kind: SymKind::BuiltinType, detail: format!("built-in type {}", n), doc: Some(d.to_string()), def: None, takes_args: None });
            }
            if let Some(ctx) = self.fstack.last() {
                let mut tps: Vec<&String> = ctx.subst.keys().collect();
                tps.sort();
                for n in tps {
                    out.push(CompItem { label: n.clone(), kind: SymKind::TypeParam, detail: format!("type parameter {}", n), doc: None, def: None, takes_args: None });
                }
            }
        }
        out
    }

    /// Everything a plain name can refer to at the current point of the current function.
    fn ide_scope_items(&mut self) -> Vec<CompItem> {
        let mut out: Vec<CompItem> = Vec::new();
        // locals, innermost first, through enclosing functions of lambdas
        for ctx in self.fstack.iter().rev() {
            for s in ctx.scopes.iter().rev() {
                for (n, id) in s.names.iter().rev() {
                    if n == "this" || n == "_" || n.starts_with('$') || out.iter().any(|x| &x.label == n) {
                        continue;
                    }
                    let info = &ctx.locals[*id as usize];
                    out.push(CompItem {
                        label: n.clone(),
                        kind: SymKind::Variable,
                        detail: self.local_code(n, &info.ty),
                        doc: None,
                        def: Some((info.span, n.clone())),
                        takes_args: match info.ty.deref() {
                            Type::Func(ps, _) => Some(!ps.is_empty()),
                            _ => None,
                        },
                    });
                }
            }
            if ctx.kind != FuncKind::Lambda {
                break;
            }
        }
        // members of the current class
        if let Some(c) = self.current_class_pub() {
            let statics_only = self.cur_ref().is_static;
            let mut members = Vec::new();
            if !statics_only {
                for (i, f) in self.classes[c as usize].fields.iter().enumerate() {
                    if f.name.starts_with('$') || f.name.starts_with("__") {
                        continue;
                    }
                    let fm = &self.cmeta[c as usize].fields[i];
                    if !self.ide_visible(fm.access, fm.owner) {
                        continue;
                    }
                    let (code, _, def) = self.field_code(c, i as u32);
                    members.push(CompItem { label: f.name.clone(), kind: SymKind::Field, detail: code, doc: None, def, takes_args: None });
                }
                let ms = self.all_methods(Owner::Class(c));
                self.method_items(&ms, None, &mut members);
            } else {
                let ms = self.all_methods(Owner::Class(c));
                self.method_items(&ms, Some(true), &mut members);
            }
            let mut cur = Some(c);
            while let Some(cc) = cur {
                for (n, s) in self.cmeta[cc as usize].statics.iter() {
                    if !n.starts_with("__") && !members.iter().any(|x| &x.label == n) {
                        let (code, _, def) = self.static_code(c, n, s);
                        members.push(CompItem { label: n.clone(), kind: SymKind::Field, detail: code, doc: None, def, takes_args: None });
                    }
                }
                cur = self.classes[cc as usize].parent;
            }
            for it in members {
                if !out.iter().any(|x| x.label == it.label) {
                    out.push(it);
                }
            }
        }
        // functions of the module
        let m = self.ide_module();
        let mut fnames: Vec<&String> = self.modules[m].funcs.keys().collect();
        fnames.sort();
        for n in fnames {
            if n.starts_with("__") || out.iter().any(|x| &x.label == n) {
                continue;
            }
            let fs = &self.modules[m].funcs[n];
            let (code, params) = self.fn_text(&fs[0]);
            let detail = if fs.len() > 1 { format!("{} (+{} overload{})", code, fs.len() - 1, if fs.len() > 2 { "s" } else { "" }) } else { code };
            out.push(CompItem {
                label: n.clone(),
                kind: SymKind::Function,
                detail,
                doc: None,
                def: Some((fs[0].decl.span, n.clone())),
                takes_args: Some(!params.is_empty()),
            });
        }
        // imported modules and packages
        let mut imps: Vec<(&String, &Import)> = self.modules[m].imports.iter().collect();
        imps.sort_by(|a, b| a.0.cmp(b.0));
        for (alias, imp) in imps {
            if out.iter().any(|x| &x.label == alias) {
                continue;
            }
            let detail = match imp {
                Import::Stdio => "using stdio".to_string(),
                Import::Intrinsics => "using intrinsics".to_string(),
                Import::Module(mi) => format!("using {}", self.modules[*mi].name),
                Import::Package(p) => format!("using {}", p),
            };
            out.push(CompItem { label: alias.clone(), kind: SymKind::Module, detail, doc: None, def: None, takes_args: None });
        }
        // types and keywords
        for it in self.ide_type_items(m, false) {
            if !out.iter().any(|x| x.label == it.label) {
                out.push(it);
            }
        }
        for k in builtins::KEYWORDS {
            if !out.iter().any(|x| x.label == *k) {
                out.push(CompItem { label: k.to_string(), kind: SymKind::Keyword, detail: String::new(), doc: None, def: None, takes_args: None });
            }
        }
        out
    }

    /// What the receiver of `obj.name` is.
    fn ide_member_source(&mut self, obj: &'a ast::Expr) -> MemberSrc {
        if let A::Super(which) = &obj.kind {
            return match (which, self.current_class_pub()) {
                (None, Some(c)) => match self.classes[c as usize].parent {
                    Some(p) => MemberSrc::Value(Type::Class(p)),
                    None => MemberSrc::Nothing,
                },
                _ => MemberSrc::Nothing,
            };
        }
        if let A::Ident(n) = &obj.kind {
            if self.lookup_local(n).is_none() && self.current_class_pub().and_then(|c| self.field_index(c, n)).is_none() {
                let m = self.ide_module();
                if let Some(imp) = self.modules[m].imports.get(n).cloned() {
                    return match imp {
                        Import::Stdio => MemberSrc::Stdio,
                        Import::Intrinsics => MemberSrc::Nothing,
                        Import::Module(mi) => {
                            // the module's own class (`using math.Math as Math`): its statics
                            let ty = self.modules[mi].name.clone();
                            match self.class_decls.get(&ty).map(|(_, d)| d.type_params.is_empty()) {
                                Some(true) => match self.instantiate_class(&ty, Vec::new(), obj.span) {
                                    Some(c) => MemberSrc::Static(c),
                                    None => MemberSrc::Module(mi),
                                },
                                _ => MemberSrc::Module(mi),
                            }
                        }
                        Import::Package(p) => MemberSrc::Package(p),
                    };
                }
                if let Some(rcv) = builtins::static_recv(n) {
                    let rcv = if n == "Int" { MemberSrc::BuiltinStatic(vec![rcv, Recv::IntClassStatic], n.clone()) } else { MemberSrc::BuiltinStatic(vec![rcv], n.clone()) };
                    return rcv;
                }
                if let Ok(Some(f)) = self.find_type(m, n) {
                    if let Some(i) = self.iface_decls.get(&f).map(|(_, d)| d.type_params.len()) {
                        return match self.instantiate_iface(&f, vec![Type::Dyn; i], obj.span) {
                            Some(i) => MemberSrc::IfaceStatic(i),
                            None => MemberSrc::Nothing,
                        };
                    }
                }
            }
        }
        if let Some(c) = self.static_class_of(obj, None) {
            return MemberSrc::Static(c);
        }
        if let Some(path) = self.type_path_of(obj) {
            // `math.linear.` : the types of a package written out in full
            if self.class_decls.keys().chain(self.iface_decls.keys()).any(|k| k.starts_with(&format!("{}.", path))) {
                return MemberSrc::Package(path);
            }
        }
        let nd = self.diags.len();
        let x = self.expr(obj, None);
        self.diags.truncate(nd);
        MemberSrc::Value(x.ty)
    }

    fn ide_member_items(&mut self, src: &MemberSrc) -> Vec<CompItem> {
        let mut out = Vec::new();
        match src {
            MemberSrc::Value(t) => self.instance_items(t, &mut out),
            MemberSrc::Static(c) => self.static_items(*c, &mut out),
            MemberSrc::IfaceStatic(i) => {
                let ms = self.all_methods(Owner::Iface(*i));
                self.method_items(&ms, Some(true), &mut out);
            }
            MemberSrc::BuiltinStatic(rs, owner) => {
                let subst = vec![("Self", owner.clone())];
                self.builtin_items(rs, &subst, owner, &mut out);
            }
            MemberSrc::Stdio => self.builtin_items(&[Recv::Stdio], &[], "stdio", &mut out),
            MemberSrc::Module(mi) => {
                let mi = *mi;
                let mut fnames: Vec<&String> = self.modules[mi].funcs.keys().collect();
                fnames.sort();
                for n in fnames {
                    if n.starts_with("__") {
                        continue;
                    }
                    let fs = &self.modules[mi].funcs[n];
                    let (code, params) = self.fn_text(&fs[0]);
                    out.push(CompItem { label: n.clone(), kind: SymKind::Function, detail: code, doc: None, def: Some((fs[0].decl.span, n.clone())), takes_args: Some(!params.is_empty()) });
                }
                let pkg = self.modules[mi].package.clone();
                for d in self.modules[mi].declared.iter() {
                    let fqn = qualify(&pkg, d);
                    out.push(self.type_item(&fqn, d.clone()));
                }
            }
            MemberSrc::Package(p) => {
                let prefix = format!("{}.", p);
                let mut names: Vec<&String> = self.class_decls.keys().chain(self.iface_decls.keys()).filter(|k| k.starts_with(&prefix)).collect();
                names.sort();
                let mut subs: Vec<String> = Vec::new();
                for fqn in names {
                    let rest = &fqn[prefix.len()..];
                    match rest.split_once('.') {
                        None => out.push(self.type_item(fqn, rest.to_string())),
                        Some((sub, _)) => {
                            if !subs.iter().any(|s| s == sub) {
                                subs.push(sub.to_string());
                            }
                        }
                    }
                }
                for s in subs {
                    out.push(CompItem { label: s.clone(), kind: SymKind::Module, detail: format!("package {}{}", prefix, s), doc: None, def: None, takes_args: None });
                }
            }
            MemberSrc::Nothing => {}
        }
        out.retain(|x| !x.label.starts_with("__"));
        out
    }

    // ------------------------------------------------------------------ signatures
    fn method_sigs(&self, ms: &[MethodInfo]) -> Vec<SigInfo> {
        ms.iter()
            .map(|m| {
                let (label, params) = self.method_text(m);
                SigInfo { label, params, doc: None, def: Some((m.span, m.name.clone())) }
            })
            .collect()
    }

    fn fn_sigs(&self, fs: &[FnRef<'a>]) -> Vec<SigInfo> {
        fs.iter()
            .map(|f| {
                let (label, params) = self.fn_text(f);
                SigInfo { label, params, doc: None, def: Some((f.decl.span, f.decl.name.clone())) }
            })
            .collect()
    }

    fn ctor_sigs(&self, c: ClassId) -> Vec<SigInfo> {
        let simple = self.cmeta[c as usize].decl.map(|d| d.name.clone()).unwrap_or_default();
        (0..self.cmeta[c as usize].ctors.len())
            .filter(|&k| self.ide_visible(self.cmeta[c as usize].ctors[k].3, c))
            .map(|k| {
                let (label, params, span) = self.ctor_text(c, k);
                SigInfo { label, params, doc: None, def: Some((span, simple.clone())) }
            })
            .collect()
    }

    fn ide_ctor_sigs_named(&mut self, path: &str) -> Vec<SigInfo> {
        let m = self.ide_module();
        let Ok(Some(fqn)) = self.find_type(m, path) else { return Vec::new() };
        if !self.class_decls.contains_key(&fqn) {
            return Vec::new();
        }
        let n = self.class_decls[&fqn].1.type_params.len();
        let targs = if n == 0 { Vec::new() } else { self.complete_targs(&fqn, Vec::new()) };
        let targs = if targs.len() == n { targs } else { vec![Type::Dyn; n] };
        match self.instantiate_class(&fqn, targs, Span::default()) {
            Some(c) => self.ctor_sigs(c),
            None => Vec::new(),
        }
    }

    fn builtin_sigs(&self, recvs: &[Recv], subst: &[(&str, String)], owner: &str, name: &str) -> Vec<SigInfo> {
        builtins::MEMBERS
            .iter()
            .filter(|m| recvs.contains(&m.recv) && m.name == name && m.params.is_some())
            .map(|m| {
                let (label, params) = self.builtin_desc(m, subst, owner);
                SigInfo { label, params, doc: Some(m.doc.to_string()), def: None }
            })
            .collect()
    }

    fn ide_ident_sigs(&mut self, name: &str) -> Vec<SigInfo> {
        if let Some(id) = self.lookup_local(name) {
            if let Type::Func(ps, r) = self.local_ty(id).deref() {
                let params: Vec<String> = ps.iter().map(|t| self.ide_tname(t)).collect();
                return vec![SigInfo { label: format!("{}({}) -> {}", name, params.join(", "), self.ide_tname(r)), params, doc: None, def: None }];
            }
            return Vec::new();
        }
        if let Some(c) = self.current_class_pub() {
            let ms = self.lookup_methods(Owner::Class(c), name);
            if !ms.is_empty() {
                return self.method_sigs(&ms);
            }
        }
        let m = self.ide_module();
        if let Some(fs) = self.lookup_funcs(m, name) {
            return self.fn_sigs(&fs);
        }
        self.ide_ctor_sigs_named(name)
    }

    fn ide_member_sigs(&mut self, src: &MemberSrc, name: &str) -> Vec<SigInfo> {
        match src {
            MemberSrc::Value(t) => {
                let t = t.deref().non_null();
                let ms = match &t {
                    Type::Class(c) => self.lookup_methods(Owner::Class(*c), name),
                    Type::Iface(i) => self.lookup_methods(Owner::Iface(*i), name),
                    _ => Vec::new(),
                };
                if !ms.is_empty() {
                    return self.method_sigs(&ms);
                }
                let subst = self.builtin_subst(&t);
                self.builtin_sigs(&Self::builtin_recvs(&t), &subst, &self.ide_tname(&t), name)
            }
            MemberSrc::Static(c) => {
                let ms: Vec<MethodInfo> = self.lookup_methods(Owner::Class(*c), name).into_iter().filter(|m| m.is_static).collect();
                self.method_sigs(&ms)
            }
            MemberSrc::IfaceStatic(i) => {
                let ms: Vec<MethodInfo> = self.lookup_methods(Owner::Iface(*i), name).into_iter().filter(|m| m.is_static).collect();
                self.method_sigs(&ms)
            }
            MemberSrc::BuiltinStatic(rs, owner) => self.builtin_sigs(rs, &[("Self", owner.clone())], owner, name),
            MemberSrc::Stdio => self.builtin_sigs(&[Recv::Stdio], &[], "stdio", name),
            MemberSrc::Module(mi) => {
                if let Some(fs) = self.lookup_funcs(*mi, name) {
                    return self.fn_sigs(&fs);
                }
                let fqn = qualify(&self.modules[*mi].package, name);
                self.ide_ctor_sigs_named(&fqn)
            }
            MemberSrc::Package(p) => {
                let fqn = qualify(p, name);
                self.ide_ctor_sigs_named(&fqn)
            }
            MemberSrc::Nothing => Vec::new(),
        }
    }
}

/// What the left side of `obj.` names.
pub enum MemberSrc {
    Value(Type),
    Static(ClassId),
    IfaceStatic(IfaceId),
    BuiltinStatic(Vec<Recv>, String),
    Stdio,
    Module(usize),
    Package(String),
    Nothing,
}

/// Splits a parameter list at top-level commas.
pub fn split_params(p: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut depth = 0i32;
    let mut cur = String::new();
    for ch in p.chars() {
        match ch {
            '(' | '[' => depth += 1,
            ')' | ']' => depth -= 1,
            ',' if depth == 0 => {
                out.push(cur.trim().to_string());
                cur.clear();
                continue;
            }
            _ => {}
        }
        cur.push(ch);
    }
    if !cur.trim().is_empty() {
        out.push(cur.trim().to_string());
    }
    out
}

fn param_count(p: &str) -> usize {
    if p.trim().is_empty() {
        0
    } else {
        split_params(p).len()
    }
}
