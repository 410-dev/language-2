//! Semantic analysis: declaration collection, name resolution, type checking, generic
//! instantiation (monomorphisation) and lowering to HIR.

mod expr;
mod members;
mod stmt;

use crate::ast::{self, Access, DirValue, Item, TypeExpr};
use crate::diag::{Diag, Span};
use crate::hir::{self, *};
use crate::types::*;
use l2_runtime::{ExcKind, FloatTy};
use std::collections::{HashMap, HashSet, VecDeque};
use std::rc::Rc;

pub type Subst = HashMap<String, Type>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Import {
    Stdio,
    Module(usize),
}

#[derive(Clone)]
pub struct FnRef<'a> {
    pub decl: &'a ast::FuncDecl,
    pub module: usize,
    /// `None` for generic functions (instantiated on use).
    pub func: Option<FuncId>,
}

pub struct ModuleInfo<'a> {
    pub file: u32,
    pub name: String,
    pub funcs: HashMap<String, Vec<FnRef<'a>>>,
    pub imports: HashMap<String, Import>,
    pub wrap: bool,
    pub is_prelude: bool,
}

#[derive(Clone, Debug)]
pub struct FuncSig {
    pub name: String,
    pub params: Vec<Type>,
    pub param_names: Vec<String>,
    pub ret: Type,
    pub throws: Vec<ClassId>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Owner {
    Class(ClassId),
    Iface(IfaceId),
}

#[derive(Clone, Debug)]
pub struct MethodInfo {
    pub name: String,
    pub func: Option<FuncId>,
    pub params: Vec<Type>,
    pub param_names: Vec<String>,
    pub ret: Type,
    pub throws: Vec<ClassId>,
    pub is_static: bool,
    pub access: Access,
    pub selector: Option<SelectorId>,
    pub owner: Owner,
    pub is_default: bool,
    pub overrides: bool,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub struct FieldMeta {
    pub access: Access,
    pub immutable: bool,
    pub owner: ClassId,
    pub has_setter: bool,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub struct StaticMeta {
    pub global: GlobalId,
    pub access: Access,
    pub immutable: bool,
}

pub struct ClassMeta<'a> {
    pub decl: Option<&'a ast::ClassDecl>,
    pub module: usize,
    pub subst: Rc<Subst>,
    pub template: String,
    pub targs: Vec<Type>,
    pub state: u8,
    pub methods: Vec<MethodInfo>,
    pub ctors: Vec<(FuncId, Vec<Type>, Vec<String>, Access, Vec<ClassId>)>,
    pub fields: Vec<FieldMeta>,
    pub statics: HashMap<String, StaticMeta>,
    pub own_field_start: usize,
}

pub struct IfaceMeta<'a> {
    pub decl: &'a ast::InterfaceDecl,
    pub module: usize,
    pub subst: Rc<Subst>,
    pub template: String,
    pub targs: Vec<Type>,
    pub state: u8,
    pub methods: Vec<MethodInfo>,
}

pub enum JobKind<'a> {
    Func(&'a ast::FuncDecl),
    Ctor(Option<&'a ast::CtorDecl>),
    Getter(u32),
    Setter(u32, bool),
    Delegate(FuncId),
}

pub struct Job<'a> {
    pub func: FuncId,
    pub kind: JobKind<'a>,
    pub module: usize,
    pub class: Option<ClassId>,
    pub iface: Option<IfaceId>,
    pub subst: Rc<Subst>,
    pub is_static: bool,
}

pub struct Scope {
    pub names: Vec<(String, LocalId)>,
    pub owned: Vec<LocalId>,
}

pub struct FnCtx {
    pub locals: Vec<LocalInfo>,
    pub immutable: Vec<bool>,
    pub copied: Vec<bool>,
    pub scopes: Vec<Scope>,
    pub ret: Option<Type>,
    pub class: Option<ClassId>,
    pub iface: Option<IfaceId>,
    pub is_static: bool,
    pub kind: FuncKind,
    pub module: usize,
    pub wrap: bool,
    pub subst: Rc<Subst>,
    pub declared_throws: Vec<ClassId>,
    pub catch_stack: Vec<Vec<ClassId>>,
    pub loops: u32,
    pub switches: u32,
    pub narrow: Vec<HashMap<LocalId, Type>>,
    /// (outer local, inner local, by_ref)
    pub captures: Vec<(LocalId, LocalId, bool)>,
    pub is_move_lambda: bool,
    pub this_local: Option<LocalId>,
}

impl FnCtx {
    fn new(kind: FuncKind, module: usize, wrap: bool, subst: Rc<Subst>) -> FnCtx {
        FnCtx {
            locals: Vec::new(),
            immutable: Vec::new(),
            copied: Vec::new(),
            scopes: vec![Scope { names: Vec::new(), owned: Vec::new() }],
            ret: Some(Type::Void),
            class: None,
            iface: None,
            is_static: true,
            kind,
            module,
            wrap,
            subst,
            declared_throws: Vec::new(),
            catch_stack: Vec::new(),
            loops: 0,
            switches: 0,
            narrow: Vec::new(),
            captures: Vec::new(),
            is_move_lambda: false,
            this_local: None,
        }
    }
}

pub struct Checker<'a> {
    pub files: &'a [ast::FileAst],
    pub diags: Vec<Diag>,
    pub warnings: Vec<Diag>,
    pub modules: Vec<ModuleInfo<'a>>,
    pub class_decls: HashMap<String, (usize, &'a ast::ClassDecl)>,
    pub iface_decls: HashMap<String, (usize, &'a ast::InterfaceDecl)>,
    pub classes: Vec<ClassInfo>,
    pub cmeta: Vec<ClassMeta<'a>>,
    pub ifaces: Vec<IfaceInfo>,
    pub imeta: Vec<IfaceMeta<'a>>,
    pub class_inst: HashMap<(String, Vec<Type>), ClassId>,
    pub iface_inst: HashMap<(String, Vec<Type>), IfaceId>,
    pub funcs: Vec<Func>,
    pub sigs: Vec<FuncSig>,
    pub func_inst: HashMap<(usize, Vec<Type>), FuncId>,
    pub selectors: Vec<Selector>,
    pub selector_map: HashMap<(String, Vec<Type>, Type), SelectorId>,
    pub globals: Vec<Global>,
    /// static initialisers: (global, module, class, initializer expression)
    pub global_inits: Vec<(GlobalId, usize, ClassId, &'a ast::Expr, bool)>,
    pub queue: VecDeque<Job<'a>>,
    pub fstack: Vec<FnCtx>,
    pub config: Config,
    pub entry_module: usize,
    pub lambda_counter: u32,
    pub prelude_module: usize,
}

pub struct CheckOutput {
    pub program: Program,
    pub warnings: Vec<Diag>,
}

/// Type-checks a whole program. `files[0]` must be the prelude, `files[entry]` the entry file.
pub fn check_program(files: &[ast::FileAst], module_names: &[String], entry: usize) -> Result<CheckOutput, Vec<Diag>> {
    let mut c = Checker {
        files,
        diags: Vec::new(),
        warnings: Vec::new(),
        modules: Vec::new(),
        class_decls: HashMap::new(),
        iface_decls: HashMap::new(),
        classes: Vec::new(),
        cmeta: Vec::new(),
        ifaces: Vec::new(),
        imeta: Vec::new(),
        class_inst: HashMap::new(),
        iface_inst: HashMap::new(),
        funcs: Vec::new(),
        sigs: Vec::new(),
        func_inst: HashMap::new(),
        selectors: Vec::new(),
        selector_map: HashMap::new(),
        globals: Vec::new(),
        global_inits: Vec::new(),
        queue: VecDeque::new(),
        fstack: Vec::new(),
        config: Config::default(),
        entry_module: entry,
        lambda_counter: 0,
        prelude_module: 0,
    };
    c.run(module_names);
    let errors: Vec<Diag> = c.diags.drain(..).collect();
    if !errors.is_empty() {
        return Err(errors);
    }
    let program = c.finish();
    match program {
        Ok(p) => {
            let mut p = p;
            let errs = crate::flow::check_program(&mut p);
            if !errs.is_empty() {
                return Err(errs);
            }
            Ok(CheckOutput { program: p, warnings: c.warnings })
        }
        Err(e) => Err(e),
    }
}

impl<'a> Checker<'a> {
    pub fn err(&mut self, span: Span, msg: impl Into<String>) {
        self.diags.push(Diag::error(span, msg));
    }
    pub fn warn(&mut self, span: Span, msg: impl Into<String>) {
        self.warnings.push(Diag::warning(span, msg));
    }

    pub fn tname(&self, t: &Type) -> String {
        match t {
            Type::Class(c) => self.classes[*c as usize].name.clone(),
            Type::Iface(i) => self.ifaces[*i as usize].name.clone(),
            Type::Array(e) => format!("{}[]", self.tname(e)),
            Type::Dict(k, v) => format!("Dictionary[{}, {}]", self.tname(k), self.tname(v)),
            Type::Nullable(t) => format!("{}?", self.tname(t)),
            Type::Union(ts) => ts.iter().map(|t| self.tname(t)).collect::<Vec<_>>().join("|"),
            Type::Tuple(ts) => format!("({})", ts.iter().map(|t| self.tname(t)).collect::<Vec<_>>().join(", ")),
            Type::Func(ps, r) => {
                format!("Function[({}), {}]", ps.iter().map(|t| self.tname(t)).collect::<Vec<_>>().join(", "), self.tname(r))
            }
            Type::Ref(m, t) => format!("{}{}", if *m { "*" } else { "&" }, self.tname(t)),
            other => {
                let p = Program {
                    classes: vec![],
                    ifaces: vec![],
                    funcs: vec![],
                    globals: vec![],
                    selectors: vec![],
                    init: 0,
                    main: 0,
                    main_takes_args: false,
                    config: Config::default(),
                    exc_classes: vec![],
                    throwable: 0,
                    any_droppable: false,
                    drop_selector: None,
                };
                p.type_name(other)
            }
        }
    }

    // ------------------------------------------------------------------ driver
    fn run(&mut self, module_names: &[String]) {
        // modules and directives
        for (i, f) in self.files.iter().enumerate() {
            let wrap = self.file_policy(f);
            self.modules.push(ModuleInfo {
                file: f.file,
                name: module_names.get(i).cloned().unwrap_or_default(),
                funcs: HashMap::new(),
                imports: HashMap::new(),
                wrap,
                is_prelude: i == 0,
            });
        }
        self.read_config();
        // collect declarations
        for (mi, f) in self.files.iter().enumerate() {
            for item in &f.items {
                match item {
                    Item::Class(c) => {
                        if self.class_decls.contains_key(&c.name) || self.iface_decls.contains_key(&c.name) {
                            self.err(c.span, format!("duplicate type name '{}'", c.name));
                            continue;
                        }
                        if is_builtin_type_name(&c.name) {
                            self.err(c.span, format!("'{}' is a built-in type name", c.name));
                            continue;
                        }
                        self.class_decls.insert(c.name.clone(), (mi, c));
                    }
                    Item::Interface(d) => {
                        if self.class_decls.contains_key(&d.name) || self.iface_decls.contains_key(&d.name) {
                            self.err(d.span, format!("duplicate type name '{}'", d.name));
                            continue;
                        }
                        self.iface_decls.insert(d.name.clone(), (mi, d));
                    }
                    Item::Using { module, alias, span } => {
                        let imp = self.resolve_import(module, *span);
                        if let Some(imp) = imp {
                            self.modules[mi].imports.insert(alias.clone(), imp);
                        }
                    }
                    Item::Function(_) => {}
                }
            }
        }
        // instantiate non-generic interfaces and classes eagerly
        let inames: Vec<String> = self.iface_decls.iter().filter(|(_, (_, d))| d.type_params.is_empty()).map(|(n, _)| n.clone()).collect();
        let mut inames = inames;
        inames.sort();
        for n in inames {
            self.instantiate_iface(&n, Vec::new(), Span::default());
        }
        let mut cnames: Vec<(usize, u32, u32, String)> = self
            .class_decls
            .iter()
            .filter(|(_, (_, d))| d.type_params.is_empty())
            .map(|(n, (m, d))| (*m, d.span.line, d.span.col, n.clone()))
            .collect();
        cnames.sort();
        for (_, _, _, n) in cnames {
            self.instantiate_class(&n, Vec::new(), Span::default());
        }
        // functions
        for (mi, f) in self.files.iter().enumerate() {
            for item in &f.items {
                if let Item::Function(d) = item {
                    self.declare_function(mi, d);
                }
            }
        }
        self.process_queue();
    }

    pub fn process_queue(&mut self) {
        while let Some(job) = self.queue.pop_front() {
            self.check_job(job);
        }
    }

    fn file_policy(&mut self, f: &ast::FileAst) -> bool {
        let mut wrap = false;
        for d in &f.directives {
            if d.name == "runtimecfg" {
                for (k, v, span) in &d.options {
                    match (k.as_str(), v) {
                        ("IntegerOverflow", DirValue::Ident(s)) if s == "wrap" => wrap = true,
                        ("IntegerOverflow", DirValue::Ident(s)) if s == "error" => wrap = false,
                        ("IntegerOverflow", _) => self.err(*span, "IntegerOverflow must be 'error' or 'wrap'"),
                        _ => self.err(*span, format!("unknown @runtimecfg option '{}'", k)),
                    }
                }
            }
        }
        wrap
    }

    fn read_config(&mut self) {
        let entry = self.entry_module;
        for (mi, f) in self.files.iter().enumerate() {
            for d in &f.directives {
                match d.name.as_str() {
                    "using" => {
                        if d.words.first().map(|s| s.as_str()) != Some("sdk") {
                            self.err(d.span, "expected '@using sdk <version>'");
                        } else if let Some(v) = d.words.get(1) {
                            match v.parse::<u32>() {
                                Ok(1) => self.config.sdk = 1,
                                _ => self.err(d.span, format!("unsupported SDK version '{}' (supported: 1)", v)),
                            }
                        }
                    }
                    "runtime" => {
                        if mi != entry {
                            continue;
                        }
                        let mut rts = Vec::new();
                        for w in &d.words {
                            match w.as_str() {
                                "compiler" => rts.push(Backend::Compiler),
                                "interpreter" => rts.push(Backend::Interpreter),
                                "bytecode" => rts.push(Backend::Bytecode),
                                other => self.err(d.span, format!("unknown runtime '{}'", other)),
                            }
                        }
                        self.config.runtimes = rts;
                    }
                    "compiler" => {
                        if mi != entry {
                            self.err(d.span, "@compiler may only be declared in the entry file (the file containing main)");
                            continue;
                        }
                        for (k, v, span) in &d.options {
                            match (k.as_str(), v) {
                                ("IncludeDependencies", DirValue::Bool(b)) => self.config.include_dependencies = *b,
                                ("EnableSoftwareEmulation", DirValue::Bool(b)) => self.config.software_emulation = *b,
                                ("MemoryManagement", DirValue::Ident(s)) if s == "ownership" => self.config.memory = MemoryMode::Ownership,
                                ("MemoryManagement", DirValue::Ident(s)) if s == "manual" => self.config.memory = MemoryMode::Manual,
                                ("Target", DirValue::List(ts)) => {
                                    let mut out = Vec::new();
                                    for t in ts {
                                        match t.as_str() {
                                            "i386" => out.push(Target::I386),
                                            "amd64" => out.push(Target::Amd64),
                                            "arm64" => out.push(Target::Arm64),
                                            other => self.err(*span, format!("unknown target '{}' (expected i386, amd64, arm64)", other)),
                                        }
                                    }
                                    self.config.targets = out;
                                }
                                ("Target", DirValue::Ident(t)) => match t.as_str() {
                                    "i386" => self.config.targets = vec![Target::I386],
                                    "amd64" => self.config.targets = vec![Target::Amd64],
                                    "arm64" => self.config.targets = vec![Target::Arm64],
                                    other => self.err(*span, format!("unknown target '{}'", other)),
                                },
                                _ => self.err(*span, format!("invalid @compiler option '{}'", k)),
                            }
                        }
                    }
                    "runtimecfg" => {}
                    _ => {}
                }
            }
        }
    }

    fn resolve_import(&mut self, module: &str, span: Span) -> Option<Import> {
        if module == "stdio" {
            return Some(Import::Stdio);
        }
        for (i, m) in self.modules.iter().enumerate() {
            if m.name == module && !m.is_prelude {
                return Some(Import::Module(i));
            }
        }
        self.err(span, format!("unknown module '{}'", module));
        None
    }

    // ------------------------------------------------------------------ selectors and functions
    pub fn selector(&mut self, name: &str, params: &[Type], ret: &Type) -> SelectorId {
        let key = (name.to_string(), params.to_vec(), ret.clone());
        if let Some(&s) = self.selector_map.get(&key) {
            return s;
        }
        let id = self.selectors.len() as SelectorId;
        self.selectors.push(Selector { name: name.to_string(), params: params.to_vec(), ret: ret.clone() });
        self.selector_map.insert(key, id);
        id
    }

    pub fn new_func(&mut self, name: String, kind: FuncKind, sig: FuncSig, wrap: bool, this_class: Option<ClassId>, span: Span) -> FuncId {
        let id = self.funcs.len() as FuncId;
        self.funcs.push(Func {
            name,
            kind,
            params: Vec::new(),
            ret: sig.ret.clone(),
            locals: Vec::new(),
            body: Vec::new(),
            wrap,
            this_class,
            captures: Vec::new(),
            span,
        });
        self.sigs.push(sig);
        id
    }

    fn resolve_throws(&mut self, names: &[String], module: usize, span: Span) -> Vec<ClassId> {
        let mut out = Vec::new();
        for n in names {
            match self.resolve_type(&TypeExpr::named(n, span), module, &Subst::new()) {
                Type::Class(c) if self.classes[c as usize].is_throwable => out.push(c),
                Type::Error => {}
                _ => self.err(span, format!("'{}' is not an exception class", n)),
            }
        }
        out
    }

    fn param_types(&mut self, params: &[ast::Param], module: usize, subst: &Subst) -> (Vec<Type>, Vec<String>) {
        let mut tys = Vec::new();
        let mut names = Vec::new();
        for p in params {
            let t = self.resolve_type(&p.ty, module, subst);
            if t == Type::Void {
                self.err(p.span, "parameters cannot have type void");
            }
            if self.config.memory == MemoryMode::Manual && matches!(t, Type::Ref(..)) {
                self.err(p.span, "'&' and '*' references cannot be used with MemoryManagement=manual");
            }
            tys.push(t);
            names.push(p.name.clone());
        }
        (tys, names)
    }

    fn declare_function(&mut self, mi: usize, d: &'a ast::FuncDecl) {
        if d.body.is_none() {
            self.err(d.span, format!("function '{}' has no body", d.name));
            return;
        }
        if d.mods.is_static || d.mods.getter || d.mods.setter.is_some() {
            self.err(d.span, "invalid modifier on a top-level function");
        }
        let generic = !d.type_params.is_empty() || has_wildcard(&d.params);
        let func = if generic {
            None
        } else {
            let subst = Subst::new();
            let (params, names) = self.param_types(&d.params, mi, &subst);
            let ret = self.resolve_type(&d.ret, mi, &subst);
            self.check_ret_type(&ret, d.span);
            let throws = self.resolve_throws(&d.throws, mi, d.span);
            // duplicate signature check
            if let Some(existing) = self.modules[mi].funcs.get(&d.name).cloned() {
                for e in &existing {
                    if let Some(f) = e.func {
                        if self.sigs[f as usize].params == params {
                            self.err(d.span, format!("function '{}' is already defined with the same parameter types", d.name));
                        }
                    }
                }
            }
            let wrap = self.modules[mi].wrap;
            let sig = FuncSig { name: d.name.clone(), params, param_names: names, ret, throws };
            let fid = self.new_func(d.name.clone(), FuncKind::Free, sig, wrap, None, d.span);
            self.queue.push_back(Job { func: fid, kind: JobKind::Func(d), module: mi, class: None, iface: None, subst: Rc::new(Subst::new()), is_static: true });
            Some(fid)
        };
        self.modules[mi].funcs.entry(d.name.clone()).or_default().push(FnRef { decl: d, module: mi, func });
    }

    fn check_ret_type(&mut self, ret: &Type, span: Span) {
        if let Type::Ref(_, _) = ret {
            if self.config.memory == MemoryMode::Manual {
                self.err(span, "references cannot be used with MemoryManagement=manual");
            }
        }
    }

    /// Instantiates a generic function with concrete type arguments.
    pub fn instantiate_generic(&mut self, fr: &FnRef<'a>, targs: Vec<Type>, span: Span) -> Option<FuncId> {
        let d = fr.decl;
        let key = (d as *const ast::FuncDecl as usize, targs.clone());
        if let Some(&f) = self.func_inst.get(&key) {
            return Some(f);
        }
        let tparams = generic_params(d);
        if tparams.len() != targs.len() {
            self.err(span, format!("'{}' expects {} type argument(s), got {}", d.name, tparams.len(), targs.len()));
            return None;
        }
        let mut subst = Subst::new();
        for ((name, bound), t) in tparams.iter().zip(targs.iter()) {
            if let Some(b) = bound {
                if !self.satisfies_bound(t, b, fr.module, span) {
                    let tn = self.tname(t);
                    self.err(span, format!("type argument {} does not satisfy the bound of {}", tn, name));
                }
            }
            subst.insert(name.clone(), t.clone());
        }
        let decl_params = wildcard_to_params(&d.params);
        let (params, names) = self.param_types(&decl_params, fr.module, &subst);
        let ret = self.resolve_type(&d.ret, fr.module, &subst);
        let throws = self.resolve_throws(&d.throws, fr.module, d.span);
        let wrap = self.modules[fr.module].wrap;
        let tn: Vec<String> = targs.iter().map(|t| self.tname(t)).collect();
        let name = format!("{}[{}]", d.name, tn.join(", "));
        let sig = FuncSig { name: d.name.clone(), params, param_names: names, ret, throws };
        let fid = self.new_func(name, FuncKind::Free, sig, wrap, None, d.span);
        self.func_inst.insert(key, fid);
        self.queue.push_back(Job { func: fid, kind: JobKind::Func(d), module: fr.module, class: None, iface: None, subst: Rc::new(subst), is_static: true });
        Some(fid)
    }

    pub fn satisfies_bound(&mut self, t: &Type, bound: &TypeExpr, module: usize, _span: Span) -> bool {
        if let TypeExpr::Named { name, args, .. } = bound {
            if name == "Comparable" && args.is_empty() {
                return match t {
                    Type::Int(_) | Type::Float(_) | Type::Big | Type::Str | Type::Bool => true,
                    Type::Class(c) => self.classes[*c as usize].compare_fn.is_some() || self.class_implements_name(*c, "Comparable"),
                    _ => false,
                };
            }
        }
        let b = self.resolve_type(bound, module, &Subst::new());
        self.assignable(t, &b)
    }

    fn class_implements_name(&self, c: ClassId, name: &str) -> bool {
        self.classes[c as usize].ifaces.iter().any(|i| self.ifaces[*i as usize].name == name)
    }

    // ------------------------------------------------------------------ type resolution
    pub fn resolve_type(&mut self, t: &TypeExpr, module: usize, subst: &Subst) -> Type {
        match t {
            TypeExpr::Void => Type::Void,
            TypeExpr::Wildcard(span) => {
                self.err(*span, "wildcard '?' is only allowed in parameter types");
                Type::Error
            }
            TypeExpr::Array(e) => {
                let et = self.resolve_type(e, module, subst);
                if matches!(et, Type::Ref(..)) {
                    self.err(Span::default(), "arrays cannot hold references");
                }
                Type::Array(Box::new(et))
            }
            TypeExpr::Nullable(e) => {
                let et = self.resolve_type(e, module, subst);
                et.nullable()
            }
            TypeExpr::Union(ts) => {
                let mut out: Vec<Type> = Vec::new();
                for t in ts {
                    let r = self.resolve_type(t, module, subst);
                    let parts = match r {
                        Type::Union(inner) => inner,
                        other => vec![other],
                    };
                    for p in parts {
                        if !out.contains(&p) {
                            out.push(p);
                        }
                    }
                }
                // a union containing Null-able members becomes nullable
                let nullable = out.iter().any(|t| matches!(t, Type::Nullable(_)));
                let out: Vec<Type> = out.into_iter().map(|t| t.non_null()).collect();
                let mut dedup: Vec<Type> = Vec::new();
                for t in out {
                    if !dedup.contains(&t) {
                        dedup.push(t);
                    }
                }
                let u = if dedup.len() == 1 { dedup.pop().unwrap() } else { Type::Union(dedup) };
                if nullable {
                    u.nullable()
                } else {
                    u
                }
            }
            TypeExpr::Tuple(ts) => {
                let v: Vec<Type> = ts.iter().map(|t| self.resolve_type(t, module, subst)).collect();
                if v.len() == 1 {
                    return v.into_iter().next().unwrap();
                }
                Type::Tuple(v)
            }
            TypeExpr::Func(ps, r) => {
                let p: Vec<Type> = ps.iter().map(|t| self.resolve_type(t, module, subst)).collect();
                let r = self.resolve_type(r, module, subst);
                Type::Func(p, Box::new(r))
            }
            TypeExpr::Ref { mutable, inner } => {
                let it = self.resolve_type(inner, module, subst);
                Type::Ref(*mutable, Box::new(it))
            }
            TypeExpr::Named { name, args, span } => {
                if let Some(t) = subst.get(name) {
                    if !args.is_empty() {
                        self.err(*span, format!("type parameter '{}' cannot take type arguments", name));
                    }
                    return t.clone();
                }
                if let Some(i) = int_type_by_name(name) {
                    return Type::Int(i);
                }
                if let Some(f) = float_type_by_name(name) {
                    if f == FloatTy::F16 {
                        self.check_float16(*span);
                    }
                    return Type::Float(f);
                }
                match name.as_str() {
                    "Boolean" => return Type::Bool,
                    "String" => return Type::Str,
                    "IntLarge" => return Type::Big,
                    "DTVariable" => return Type::Dyn,
                    "STVariable" => {
                        self.err(*span, "STVariable can only be used as the type of an initialized local variable");
                        return Type::Error;
                    }
                    "Dictionary" => {
                        return match args.len() {
                            0 => Type::Dict(Box::new(Type::Dyn), Box::new(Type::Dyn)),
                            2 => {
                                let k = self.resolve_type(&args[0], module, subst);
                                let v = self.resolve_type(&args[1], module, subst);
                                Type::Dict(Box::new(k), Box::new(v))
                            }
                            _ => {
                                self.err(*span, "Dictionary takes exactly two type arguments (key, value) or none");
                                Type::Error
                            }
                        };
                    }
                    "void" => return Type::Void,
                    _ => {}
                }
                let targs: Vec<Type> = args.iter().map(|a| self.resolve_type(a, module, subst)).collect();
                if self.class_decls.contains_key(name) {
                    return match self.instantiate_class(name, targs, *span) {
                        Some(c) => Type::Class(c),
                        None => Type::Error,
                    };
                }
                if self.iface_decls.contains_key(name) {
                    return match self.instantiate_iface(name, targs, *span) {
                        Some(i) => Type::Iface(i),
                        None => Type::Error,
                    };
                }
                self.err(*span, format!("unknown type '{}'", name));
                Type::Error
            }
        }
    }

    fn check_float16(&mut self, span: Span) {
        // Float16 has hardware support only on arm64 among the supported targets.
        let all_arm = self.config.targets.iter().all(|t| *t == Target::Arm64);
        if !all_arm && !self.config.software_emulation {
            self.err(span, "Float16 is not supported in hardware by every target in @compiler Target; set EnableSoftwareEmulation=true");
        }
    }

    // ------------------------------------------------------------------ interfaces
    pub fn instantiate_iface(&mut self, name: &str, targs: Vec<Type>, span: Span) -> Option<IfaceId> {
        let key = (name.to_string(), targs.clone());
        if let Some(&i) = self.iface_inst.get(&key) {
            return Some(i);
        }
        let (module, decl) = *self.iface_decls.get(name)?;
        if decl.type_params.len() != targs.len() {
            self.err(span, format!("interface '{}' expects {} type argument(s), got {}", name, decl.type_params.len(), targs.len()));
            return None;
        }
        let mut subst = Subst::new();
        for (tp, t) in decl.type_params.iter().zip(targs.iter()) {
            subst.insert(tp.name.clone(), t.clone());
        }
        let id = self.ifaces.len() as IfaceId;
        let display = if targs.is_empty() {
            name.to_string()
        } else {
            format!("{}[{}]", name, targs.iter().map(|t| self.tname(t)).collect::<Vec<_>>().join(", "))
        };
        self.ifaces.push(IfaceInfo { name: display, parents: Vec::new() });
        let subst = Rc::new(subst);
        self.imeta.push(IfaceMeta { decl, module, subst: subst.clone(), template: name.to_string(), targs, state: 1, methods: Vec::new() });
        self.iface_inst.insert(key, id);
        // parents
        let mut parents = Vec::new();
        for p in &decl.extends {
            match self.resolve_type(p, module, &subst) {
                Type::Iface(pi) => parents.push(pi),
                Type::Error => {}
                _ => self.err(decl.span, "an interface can only extend interfaces"),
            }
        }
        self.ifaces[id as usize].parents = parents;
        // methods
        let wrap = self.modules[module].wrap;
        let mut methods = Vec::new();
        for m in &decl.methods {
            if !m.type_params.is_empty() {
                self.err(m.span, "generic methods are not supported yet; use a generic function or class");
                continue;
            }
            let (params, names) = self.param_types(&m.params, module, &subst);
            let ret = self.resolve_type(&m.ret, module, &subst);
            let throws = self.resolve_throws(&m.throws, module, m.span);
            let is_static = m.mods.is_static;
            let selector = if is_static { None } else { Some(self.selector(&m.name, &params, &ret)) };
            let func = if m.body.is_some() {
                if !m.mods.is_default && !is_static {
                    self.err(m.span, "interface methods with a body must be declared 'default'");
                }
                let sig = FuncSig { name: m.name.clone(), params: params.clone(), param_names: names.clone(), ret: ret.clone(), throws: throws.clone() };
                let fname = format!("{}.{}", self.ifaces[id as usize].name, m.name);
                let kind = if is_static { FuncKind::Free } else { FuncKind::Method };
                let fid = self.new_func(fname, kind, sig, wrap, None, m.span);
                self.queue.push_back(Job {
                    func: fid,
                    kind: JobKind::Func(m),
                    module,
                    class: None,
                    iface: Some(id),
                    subst: subst.clone(),
                    is_static,
                });
                Some(fid)
            } else {
                if m.mods.is_default {
                    self.err(m.span, "a 'default' method needs a body");
                }
                None
            };
            methods.push(MethodInfo {
                name: m.name.clone(),
                func,
                params,
                param_names: names,
                ret,
                throws,
                is_static,
                access: Access::Public,
                selector,
                owner: Owner::Iface(id),
                is_default: m.body.is_some(),
                overrides: false,
                span: m.span,
            });
        }
        self.imeta[id as usize].methods = methods;
        self.imeta[id as usize].state = 2;
        Some(id)
    }

    pub fn iface_ancestors(&self, i: IfaceId, out: &mut Vec<IfaceId>) {
        if out.contains(&i) {
            return;
        }
        out.push(i);
        for &p in &self.ifaces[i as usize].parents.clone() {
            self.iface_ancestors(p, out);
        }
    }

    // ------------------------------------------------------------------ classes
    pub fn instantiate_class(&mut self, name: &str, targs: Vec<Type>, span: Span) -> Option<ClassId> {
        let key = (name.to_string(), targs.clone());
        if let Some(&c) = self.class_inst.get(&key) {
            return Some(c);
        }
        let (module, decl) = *self.class_decls.get(name)?;
        if decl.type_params.len() != targs.len() {
            self.err(span, format!("class '{}' expects {} type argument(s), got {}", name, decl.type_params.len(), targs.len()));
            return None;
        }
        let mut subst = Subst::new();
        for (tp, t) in decl.type_params.iter().zip(targs.iter()) {
            if let Some(b) = &tp.bound {
                if !self.satisfies_bound(t, b, module, span) {
                    let tn = self.tname(t);
                    self.err(span, format!("type argument {} does not satisfy the bound of {}", tn, tp.name));
                }
            }
            subst.insert(tp.name.clone(), t.clone());
        }
        let id = self.classes.len() as ClassId;
        let display = if targs.is_empty() {
            name.to_string()
        } else {
            format!("{}[{}]", name, targs.iter().map(|t| self.tname(t)).collect::<Vec<_>>().join(", "))
        };
        self.classes.push(ClassInfo {
            name: display,
            parent: None,
            ifaces: Vec::new(),
            fields: Vec::new(),
            vtable: HashMap::new(),
            drop_fn: None,
            equals_fn: None,
            to_string_fn: None,
            compare_fn: None,
            needs_drop: false,
            is_throwable: name == "Throwable",
        });
        let subst = Rc::new(subst);
        self.cmeta.push(ClassMeta {
            decl: Some(decl),
            module,
            subst: subst.clone(),
            template: name.to_string(),
            targs,
            state: 1,
            methods: Vec::new(),
            ctors: Vec::new(),
            fields: Vec::new(),
            statics: HashMap::new(),
            own_field_start: 0,
        });
        self.class_inst.insert(key, id);
        self.build_class(id, decl, module, subst);
        Some(id)
    }

    fn build_class(&mut self, id: ClassId, decl: &'a ast::ClassDecl, module: usize, subst: Rc<Subst>) {
        let wrap = self.modules[module].wrap;
        let cname = self.classes[id as usize].name.clone();
        // parent
        let mut parent = None;
        if let Some(p) = &decl.extends {
            match self.resolve_type(p, module, &subst) {
                Type::Class(pc) => {
                    if self.cmeta[pc as usize].state == 1 {
                        self.err(decl.span, format!("cyclic inheritance involving '{}'", cname));
                    } else {
                        parent = Some(pc);
                    }
                }
                Type::Iface(_) => self.err(decl.span, "use 'implements' for interfaces"),
                Type::Error => {}
                _ => self.err(decl.span, "a class can only extend a class"),
            }
        }
        let mut fields = Vec::new();
        let mut fmeta = Vec::new();
        let mut vtable = HashMap::new();
        let mut ifaces = Vec::new();
        if let Some(pc) = parent {
            fields = self.classes[pc as usize].fields.clone();
            fmeta = self.cmeta[pc as usize].fields.clone();
            vtable = self.classes[pc as usize].vtable.clone();
            ifaces = self.classes[pc as usize].ifaces.clone();
            self.classes[id as usize].is_throwable = self.classes[pc as usize].is_throwable;
        }
        self.classes[id as usize].parent = parent;
        for it in &decl.implements {
            match self.resolve_type(it, module, &subst) {
                Type::Iface(i) => self.iface_ancestors(i, &mut ifaces),
                Type::Class(_) => self.err(decl.span, "use 'extends' for classes"),
                Type::Error => {}
                _ => self.err(decl.span, "can only implement interfaces"),
            }
        }
        self.classes[id as usize].ifaces = ifaces.clone();
        // fields
        let own_start = fields.len();
        self.cmeta[id as usize].own_field_start = own_start;
        for f in &decl.fields {
            let ty = self.resolve_type(&f.ty, module, &subst);
            if matches!(ty, Type::Ref(..)) {
                self.err(f.span, "references cannot be stored in fields (spec 9.5)");
            }
            if ty == Type::Void {
                self.err(f.span, "fields cannot have type void");
            }
            if f.mods.setter.is_some() && f.mods.immutable {
                self.err(f.span, "'setter' cannot be combined with 'Immutable'");
            }
            if f.mods.copied && ty.is_copy() {
                self.warn(f.span, "'copied' has no effect on a copy type and is ignored");
            }
            if f.mods.is_static {
                if f.mods.getter || f.mods.setter.is_some() {
                    self.err(f.span, "getter/setter are not supported on static fields");
                }
                let gid = self.globals.len() as GlobalId;
                self.globals.push(Global { name: format!("{}.{}", cname, f.name), ty: ty.clone() });
                if self.cmeta[id as usize].statics.contains_key(&f.name) {
                    self.err(f.span, format!("duplicate field '{}'", f.name));
                }
                self.cmeta[id as usize].statics.insert(f.name.clone(), StaticMeta { global: gid, access: f.mods.access, immutable: f.mods.immutable });
                match &f.init {
                    Some(e) => self.global_inits.push((gid, module, id, e, f.mods.immutable)),
                    None => {
                        if !ty.is_nullable() && l2_runtime::ops::default_value(&rt_type(&ty)).is_none() {
                            self.err(f.span, format!("static field '{}' needs an initializer", f.name));
                        }
                        self.global_inits.push((gid, module, id, &NULL_EXPR, f.mods.immutable));
                    }
                }
                continue;
            }
            if fields.iter().any(|x: &FieldInfo| x.name == f.name) {
                self.err(f.span, format!("duplicate field '{}' (fields cannot be redeclared in subclasses)", f.name));
            }
            fields.push(FieldInfo { name: f.name.clone(), ty, immutable: f.mods.immutable });
            fmeta.push(FieldMeta { access: f.mods.access, immutable: f.mods.immutable, owner: id, has_setter: f.mods.setter.is_some(), span: f.span });
        }
        self.classes[id as usize].fields = fields.clone();
        self.cmeta[id as usize].fields = fmeta;

        // methods
        let mut methods: Vec<MethodInfo> = Vec::new();
        for m in &decl.methods {
            if !m.type_params.is_empty() {
                self.err(m.span, "generic methods are not supported yet; use a generic function or class");
                continue;
            }
            let (params, names) = self.param_types(&m.params, module, &subst);
            let ret = self.resolve_type(&m.ret, module, &subst);
            self.check_ret_type(&ret, m.span);
            let throws = self.resolve_throws(&m.throws, module, m.span);
            let is_static = m.mods.is_static;
            if m.name == "drop" && params.is_empty() && !is_static {
                // fine: Droppable implementation
            }
            if methods.iter().any(|x| x.name == m.name && x.params == params) {
                self.err(m.span, format!("method '{}' is already defined with the same parameter types", m.name));
            }
            let sig = FuncSig { name: m.name.clone(), params: params.clone(), param_names: names.clone(), ret: ret.clone(), throws: throws.clone() };
            let kind = if is_static { FuncKind::Free } else { FuncKind::Method };
            let fid = self.new_func(format!("{}.{}", cname, m.name), kind, sig, wrap, if is_static { None } else { Some(id) }, m.span);
            if m.body.is_none() && m.delegate.is_none() {
                self.err(m.span, format!("method '{}' needs a body", m.name));
            }
            let selector = if is_static || m.mods.access == Access::Private { None } else { Some(self.selector(&m.name, &params, &ret)) };
            let overrides = m.mods.annotations.iter().any(|a| a == "Override");
            methods.push(MethodInfo {
                name: m.name.clone(),
                func: Some(fid),
                params,
                param_names: names,
                ret,
                throws,
                is_static,
                access: m.mods.access,
                selector,
                owner: Owner::Class(id),
                is_default: false,
                overrides,
                span: m.span,
            });
            if let Some(origin) = &m.delegate {
                // `= origin Iface`: forward to that interface's default implementation
                let target = self.resolve_type(&TypeExpr::named(origin, m.span), module, &subst);
                let mut found = None;
                if let Type::Iface(i) = target {
                    if !ifaces.contains(&i) {
                        self.err(m.span, format!("'{}' is not implemented by this class", origin));
                    }
                    let ps = methods.last().unwrap().params.clone();
                    for im in &self.imeta[i as usize].methods {
                        if im.name == m.name && im.params == ps {
                            found = im.func;
                        }
                    }
                } else {
                    self.err(m.span, format!("'{}' is not an interface", origin));
                }
                match found {
                    Some(df) => self.queue.push_back(Job { func: fid, kind: JobKind::Delegate(df), module, class: Some(id), iface: None, subst: subst.clone(), is_static: false }),
                    None => self.err(m.span, format!("interface '{}' has no default method '{}' with these parameters", origin, m.name)),
                }
            } else {
                self.queue.push_back(Job { func: fid, kind: JobKind::Func(m), module, class: Some(id), iface: None, subst: subst.clone(), is_static });
            }
        }
        // generated accessors
        for (fi, f) in decl.fields.iter().enumerate() {
            let _ = fi;
            if f.mods.is_static {
                continue;
            }
            let idx = match fields.iter().position(|x| x.name == f.name) {
                Some(i) => i as u32,
                None => continue,
            };
            let fty = fields[idx as usize].ty.clone();
            if f.mods.getter {
                let ret = if fty.is_copy() { fty.clone() } else { Type::Ref(false, Box::new(fty.clone())) };
                if methods.iter().any(|m| m.name == f.name && m.params.is_empty()) {
                    self.err(f.span, format!("getter '{}' conflicts with an existing method", f.name));
                }
                let sig = FuncSig { name: f.name.clone(), params: vec![], param_names: vec![], ret: ret.clone(), throws: vec![] };
                let fid = self.new_func(format!("{}.{}", cname, f.name), FuncKind::Method, sig, wrap, Some(id), f.span);
                let selector = Some(self.selector(&f.name, &[], &ret));
                methods.push(MethodInfo {
                    name: f.name.clone(),
                    func: Some(fid),
                    params: vec![],
                    param_names: vec![],
                    ret,
                    throws: vec![],
                    is_static: false,
                    access: Access::Public,
                    selector,
                    owner: Owner::Class(id),
                    is_default: false,
                    overrides: false,
                    span: f.span,
                });
                self.queue.push_back(Job { func: fid, kind: JobKind::Getter(idx), module, class: Some(id), iface: None, subst: subst.clone(), is_static: false });
            }
            if let Some(chain) = f.mods.setter {
                let ret = if chain { Type::Ref(true, Box::new(Type::Class(id))) } else { Type::Void };
                let params = vec![fty.clone()];
                let sig = FuncSig { name: f.name.clone(), params: params.clone(), param_names: vec!["value".into()], ret: ret.clone(), throws: vec![] };
                let fid = self.new_func(format!("{}.{}", cname, f.name), FuncKind::Method, sig, wrap, Some(id), f.span);
                let selector = Some(self.selector(&f.name, &params, &ret));
                methods.push(MethodInfo {
                    name: f.name.clone(),
                    func: Some(fid),
                    params,
                    param_names: vec!["value".into()],
                    ret,
                    throws: vec![],
                    is_static: false,
                    access: Access::Public,
                    selector,
                    owner: Owner::Class(id),
                    is_default: false,
                    overrides: false,
                    span: f.span,
                });
                self.queue.push_back(Job { func: fid, kind: JobKind::Setter(idx, chain), module, class: Some(id), iface: None, subst: subst.clone(), is_static: false });
            }
        }
        // an inherited method with the same parameters must keep its return type
        for m in &methods {
            if m.is_static || m.selector.is_none() {
                continue;
            }
            let clash = vtable.keys().chain(ifaces.iter().flat_map(|i| self.imeta[*i as usize].methods.iter().filter_map(|im| im.selector.as_ref()))).any(|s| {
                let sel = &self.selectors[*s as usize];
                sel.name == m.name && sel.params == m.params && sel.ret != m.ret
            });
            if clash {
                self.err(m.span, format!("method '{}' overrides a method with a different return type", m.name));
            }
        }
        // vtable: own instance methods override inherited entries
        for m in &methods {
            if let (Some(sel), Some(f)) = (m.selector, m.func) {
                let inherited = vtable.contains_key(&sel);
                let from_iface = ifaces.iter().any(|i| self.imeta[*i as usize].methods.iter().any(|im| im.selector == Some(sel)));
                let implicit_root = (m.name == "equals" && m.params.len() == 1) || (m.name == "toString" && m.params.is_empty());
                if m.overrides && !inherited && !from_iface && !implicit_root {
                    self.err(m.span, format!("method '{}' is marked @Override but does not override anything", m.name));
                }
                if inherited {
                    // return types must agree
                    let old = vtable[&sel];
                    if self.funcs[old as usize].ret != m.ret {
                        self.err(m.span, format!("method '{}' overrides a method with a different return type", m.name));
                    }
                }
                vtable.insert(sel, f);
            } else if m.overrides && m.is_static {
                self.err(m.span, "static methods cannot override");
            }
        }
        // interface methods: defaults and abstract checks
        let mut by_sel: HashMap<SelectorId, Vec<(IfaceId, Option<FuncId>, String, Span)>> = HashMap::new();
        for &i in &ifaces {
            for im in &self.imeta[i as usize].methods {
                if let Some(sel) = im.selector {
                    by_sel.entry(sel).or_default().push((i, im.func, im.name.clone(), im.span));
                }
            }
        }
        let mut sels: Vec<SelectorId> = by_sel.keys().copied().collect();
        sels.sort();
        for sel in sels {
            let entries = &by_sel[&sel];
            if vtable.contains_key(&sel) {
                continue;
            }
            let defaults: Vec<(IfaceId, FuncId)> = entries.iter().filter_map(|(i, f, _, _)| f.map(|f| (*i, f))).collect();
            let mut uniq: Vec<FuncId> = defaults.iter().map(|(_, f)| *f).collect();
            uniq.sort();
            uniq.dedup();
            if uniq.len() > 1 {
                // a default from a sub-interface overrides one from its parent
                let mut most_specific: Vec<(IfaceId, FuncId)> = Vec::new();
                for (i, f) in &defaults {
                    let shadowed = defaults.iter().any(|(j, g)| g != f && {
                        let mut anc = Vec::new();
                        self.iface_ancestors(*j, &mut anc);
                        anc.contains(i)
                    });
                    if !shadowed && !most_specific.iter().any(|(_, g)| g == f) {
                        most_specific.push((*i, *f));
                    }
                }
                if most_specific.len() > 1 {
                    let names: Vec<String> = most_specific.iter().map(|(i, _)| self.ifaces[*i as usize].name.clone()).collect();
                    self.err(
                        decl.span,
                        format!(
                            "class '{}' inherits conflicting default methods '{}' from {}; override it (e.g. `= origin {}`)",
                            cname,
                            entries[0].2,
                            names.join(" and "),
                            names[0]
                        ),
                    );
                } else if let Some((_, f)) = most_specific.first() {
                    vtable.insert(sel, *f);
                }
            } else if let Some(f) = uniq.first() {
                vtable.insert(sel, *f);
            } else {
                let iname = self.ifaces[entries[0].0 as usize].name.clone();
                self.err(decl.span, format!("class '{}' must implement method '{}' of interface '{}'", cname, entries[0].2, iname));
            }
        }
        self.classes[id as usize].vtable = vtable.clone();
        self.cmeta[id as usize].methods = methods.clone();

        // special methods
        let drop_sel = self.selector("drop", &[], &Type::Void);
        let droppable = ifaces.iter().any(|i| self.ifaces[*i as usize].name == "Droppable");
        if droppable {
            self.classes[id as usize].drop_fn = vtable.get(&drop_sel).copied();
        } else if let Some(pc) = parent {
            self.classes[id as usize].drop_fn = self.classes[pc as usize].drop_fn;
        }
        for m in &methods {
            if m.is_static {
                continue;
            }
            match (m.name.as_str(), m.params.len()) {
                ("equals", 1) if m.ret == Type::Bool => self.classes[id as usize].equals_fn = m.func,
                ("toString", 0) if m.ret == Type::Str => self.classes[id as usize].to_string_fn = m.func,
                ("compareTo", 1) if m.ret == Type::int32() => self.classes[id as usize].compare_fn = m.func,
                _ => {}
            }
        }
        if let Some(pc) = parent {
            let pi = self.classes[pc as usize].clone();
            let ci = &mut self.classes[id as usize];
            if ci.equals_fn.is_none() {
                ci.equals_fn = pi.equals_fn;
            }
            if ci.to_string_fn.is_none() {
                ci.to_string_fn = pi.to_string_fn;
            }
            if ci.compare_fn.is_none() {
                ci.compare_fn = pi.compare_fn;
            }
        }
        if self.class_implements_name(id, "Comparable") && self.classes[id as usize].compare_fn.is_none() {
            self.err(decl.span, format!("class '{}' implements Comparable but has no 'Int32 compareTo(other)' method", cname));
        }

        // constructors
        let mut ctors = Vec::new();
        for cd in &decl.ctors {
            let (params, names) = self.param_types(&cd.params, module, &subst);
            let throws = self.resolve_throws(&cd.throws, module, cd.span);
            if ctors.iter().any(|(_, p, _, _, _): &(FuncId, Vec<Type>, Vec<String>, Access, Vec<ClassId>)| *p == params) {
                self.err(cd.span, "duplicate constructor signature");
            }
            let sig = FuncSig { name: cname.clone(), params: params.clone(), param_names: names.clone(), ret: Type::Void, throws: throws.clone() };
            let fid = self.new_func(format!("{}.<init>", cname), FuncKind::Ctor, sig, wrap, Some(id), cd.span);
            ctors.push((fid, params, names, cd.mods.access, throws));
            self.queue.push_back(Job { func: fid, kind: JobKind::Ctor(Some(cd)), module, class: Some(id), iface: None, subst: subst.clone(), is_static: false });
        }
        if decl.ctors.is_empty() {
            let sig = FuncSig { name: cname.clone(), params: vec![], param_names: vec![], ret: Type::Void, throws: vec![] };
            let fid = self.new_func(format!("{}.<init>", cname), FuncKind::Ctor, sig, wrap, Some(id), decl.span);
            ctors.push((fid, vec![], vec![], Access::Public, vec![]));
            self.queue.push_back(Job { func: fid, kind: JobKind::Ctor(None), module, class: Some(id), iface: None, subst: subst.clone(), is_static: false });
        }
        self.cmeta[id as usize].ctors = ctors;
        self.cmeta[id as usize].state = 2;
    }

    /// All methods named `name` visible on a class or interface type (most derived first).
    pub fn lookup_methods(&self, owner: Owner, name: &str) -> Vec<MethodInfo> {
        let mut out: Vec<MethodInfo> = Vec::new();
        let mut seen: HashSet<Vec<Type>> = HashSet::new();
        match owner {
            Owner::Class(c) => {
                let mut cur = Some(c);
                while let Some(cc) = cur {
                    for m in &self.cmeta[cc as usize].methods {
                        if m.name == name && !seen.contains(&m.params) {
                            seen.insert(m.params.clone());
                            out.push(m.clone());
                        }
                    }
                    cur = self.classes[cc as usize].parent;
                }
                for &i in &self.classes[c as usize].ifaces {
                    for m in &self.imeta[i as usize].methods {
                        if m.name == name && !seen.contains(&m.params) {
                            seen.insert(m.params.clone());
                            out.push(m.clone());
                        }
                    }
                }
            }
            Owner::Iface(i) => {
                let mut anc = Vec::new();
                self.iface_ancestors(i, &mut anc);
                for a in anc {
                    for m in &self.imeta[a as usize].methods {
                        if m.name == name && !seen.contains(&m.params) {
                            seen.insert(m.params.clone());
                            out.push(m.clone());
                        }
                    }
                }
            }
        }
        out
    }

    pub fn field_index(&self, c: ClassId, name: &str) -> Option<u32> {
        self.classes[c as usize].fields.iter().position(|f| f.name == name).map(|i| i as u32)
    }

    pub fn find_static(&self, c: ClassId, name: &str) -> Option<StaticMeta> {
        let mut cur = Some(c);
        while let Some(cc) = cur {
            if let Some(s) = self.cmeta[cc as usize].statics.get(name) {
                return Some(s.clone());
            }
            cur = self.classes[cc as usize].parent;
        }
        None
    }

    pub fn is_subclass(&self, mut c: ClassId, of: ClassId) -> bool {
        loop {
            if c == of {
                return true;
            }
            match self.classes[c as usize].parent {
                Some(p) => c = p,
                None => return false,
            }
        }
    }

    pub fn class_by_name(&self, n: &str) -> Option<ClassId> {
        self.class_inst.get(&(n.to_string(), Vec::new())).copied()
    }

    pub fn is_checked_exception(&self, c: ClassId) -> bool {
        let exc = self.class_by_name("Exception");
        let rt = self.class_by_name("RuntimeException");
        match (exc, rt) {
            (Some(e), Some(r)) => self.is_subclass(c, e) && !self.is_subclass(c, r),
            _ => false,
        }
    }

    // ------------------------------------------------------------------ assignability
    pub fn assignable(&self, from: &Type, to: &Type) -> bool {
        if from == to {
            return true;
        }
        match (from, to) {
            (Type::Error, _) | (_, Type::Error) | (Type::Never, _) => true,
            (Type::Void, _) | (_, Type::Void) => false,
            (Type::Int(a), Type::Int(b)) => int_widens(*a, *b),
            (Type::Int(a), Type::Float(b)) => int_widens_to_float(*a, *b),
            (Type::Int(_), Type::Big) => true,
            (Type::Float(a), Type::Float(b)) => float_widens(*a, *b),
            (Type::Null, Type::Nullable(_)) | (Type::Null, Type::Dyn) => true,
            (Type::Dyn, _) => false,
            (_, Type::Dyn) => !matches!(from, Type::Ref(..)),
            (Type::Nullable(a), Type::Nullable(b)) => self.assignable(a, b),
            (Type::Nullable(_), _) => false,
            (_, Type::Nullable(b)) => self.assignable(from, b),
            (Type::Union(fs), Type::Union(_)) => fs.iter().all(|f| self.assignable(f, to)),
            (_, Type::Union(us)) => us.iter().any(|u| self.assignable(from, u)),
            (Type::Class(a), Type::Class(b)) => self.is_subclass(*a, *b),
            (Type::Class(a), Type::Iface(i)) => self.classes[*a as usize].ifaces.contains(i),
            (Type::Iface(a), Type::Iface(b)) => {
                let mut anc = Vec::new();
                self.iface_ancestors(*a, &mut anc);
                anc.contains(b)
            }
            (Type::Ref(_, a), Type::Ref(false, b)) => self.assignable(a, b) && self.ref_compatible(a, b),
            (Type::Ref(true, a), Type::Ref(true, b)) => a == b,
            (_, Type::Ref(false, b)) => self.assignable(from, b) && self.ref_compatible(from, b),
            (Type::Ref(_, a), _) => a.is_copy() && self.assignable(a, to),
            (Type::Tuple(a), Type::Tuple(b)) => a.len() == b.len() && a.iter().zip(b.iter()).all(|(x, y)| self.assignable(x, y)),
            (Type::Func(pa, ra), Type::Func(pb, rb)) => pa == pb && (ra == rb || self.assignable(ra, rb)),
            _ => false,
        }
    }

    /// A borrowed value can be viewed at a different type only if no conversion is needed.
    fn ref_compatible(&self, a: &Type, b: &Type) -> bool {
        a == b || matches!((a, b), (Type::Class(_), Type::Class(_)) | (Type::Class(_), Type::Iface(_)) | (Type::Iface(_), Type::Iface(_)))
    }

    // ------------------------------------------------------------------ finishing
    fn finish(&mut self) -> Result<Program, Vec<Diag>> {
        // global initialisers in dependency order
        let init = self.build_init();
        self.process_queue();
        // main
        let entry = self.entry_module;
        let mut main = None;
        let mut main_takes_args = false;
        if let Some(cands) = self.modules[entry].funcs.get("main").cloned() {
            for c in &cands {
                if let Some(f) = c.func {
                    let sig = &self.sigs[f as usize];
                    let ok_ret = matches!(sig.ret, Type::Void | Type::Int(l2_runtime::IntTy::I32));
                    if sig.params.is_empty() && ok_ret {
                        main = Some(f);
                    } else if sig.params == vec![Type::Array(Box::new(Type::Str))] && ok_ret {
                        main = Some(f);
                        main_takes_args = true;
                    } else {
                        let span = self.funcs[f as usize].span;
                        self.err(span, "main must be 'function void main(String[] args)' or 'function void main()'");
                    }
                }
            }
        }
        let main = match main {
            Some(m) => m,
            None => {
                let file = self.modules[entry].file;
                self.err(Span::new(file, 1, 1), "entry file has no 'main' function");
                return Err(std::mem::take(&mut self.diags));
            }
        };
        if !self.diags.is_empty() {
            return Err(std::mem::take(&mut self.diags));
        }
        let mut exc_classes = Vec::new();
        for k in ExcKind::ALL {
            match self.class_by_name(k.class_name()) {
                Some(c) => exc_classes.push((k, c)),
                None => panic!("prelude class {} missing", k.class_name()),
            }
        }
        let throwable = self.class_by_name("Throwable").unwrap();
        // needs_drop for classes: Droppable, or owns a field that needs drop (fixpoint)
        for i in 0..self.classes.len() {
            self.classes[i].needs_drop = self.classes[i].drop_fn.is_some();
        }
        let any_droppable = self.classes.iter().any(|c| c.needs_drop);
        let drop_selector = self.selector_map.get(&("drop".to_string(), vec![], Type::Void)).copied();
        let mut p = Program {
            classes: std::mem::take(&mut self.classes),
            ifaces: std::mem::take(&mut self.ifaces),
            funcs: std::mem::take(&mut self.funcs),
            globals: std::mem::take(&mut self.globals),
            selectors: std::mem::take(&mut self.selectors),
            init,
            main,
            main_takes_args,
            config: self.config.clone(),
            exc_classes,
            throwable,
            any_droppable,
            drop_selector,
        };
        if any_droppable {
            loop {
                let mut changed = false;
                for i in 0..p.classes.len() {
                    if p.classes[i].needs_drop {
                        continue;
                    }
                    let fields = p.classes[i].fields.clone();
                    if fields.iter().any(|f| p.needs_drop(&f.ty)) {
                        p.classes[i].needs_drop = true;
                        changed = true;
                    }
                }
                if !changed {
                    break;
                }
            }
        }
        // filter scope drop lists
        if p.config.memory == MemoryMode::Manual || !any_droppable {
            for f in &mut p.funcs {
                clear_drops(&mut f.body);
            }
        } else {
            let pc = p.clone();
            for f in &mut p.funcs {
                let locals = f.locals.clone();
                filter_drops(&mut f.body, &|l: LocalId| pc.needs_drop(&locals[l as usize].ty));
            }
        }
        Ok(p)
    }

    fn build_init(&mut self) -> FuncId {
        let sig = FuncSig { name: "<init>".into(), params: vec![], param_names: vec![], ret: Type::Void, throws: vec![] };
        let fid = self.new_func("<static-init>".into(), FuncKind::Init, sig, false, None, Span::default());
        // check each initializer in its own context, collect dependencies
        let inits = self.global_inits.clone();
        let mut checked: Vec<(GlobalId, hir::Expr, Vec<GlobalId>, Span)> = Vec::new();
        let mut locals = Vec::new();
        for (gid, module, class, e, _imm) in inits {
            let wrap = self.modules[module].wrap;
            let mut ctx = FnCtx::new(FuncKind::Init, module, wrap, self.cmeta[class as usize].subst.clone());
            ctx.class = Some(class);
            ctx.is_static = true;
            ctx.locals = std::mem::take(&mut locals);
            ctx.immutable = vec![false; ctx.locals.len()];
            ctx.copied = vec![false; ctx.locals.len()];
            self.fstack.push(ctx);
            let ty = self.globals[gid as usize].ty.clone();
            let ex = if std::ptr::eq(e, &NULL_EXPR) {
                let def = l2_runtime::ops::default_value(&rt_type(&ty));
                match def {
                    _ if ty.is_nullable() => hir::Expr::new(ExprKind::Lit(Lit::Null), ty.clone(), e.span),
                    Some(_) => self.default_value_expr(&ty, e.span),
                    None => hir::Expr::new(ExprKind::Lit(Lit::Null), ty.clone(), e.span),
                }
            } else {
                let x = self.expr(e, Some(&ty));
                let x = self.coerce(x, &ty, e.span);
                self.consume(x)
            };
            let ctx = self.fstack.pop().unwrap();
            locals = ctx.locals;
            let mut deps = Vec::new();
            collect_globals(&ex, &mut deps);
            checked.push((gid, ex, deps, e.span));
        }
        // topological order
        let mut order = Vec::new();
        let mut state: HashMap<GlobalId, u8> = HashMap::new();
        let map: HashMap<GlobalId, usize> = checked.iter().enumerate().map(|(i, c)| (c.0, i)).collect();
        fn visit(
            g: GlobalId,
            map: &HashMap<GlobalId, usize>,
            checked: &[(GlobalId, hir::Expr, Vec<GlobalId>, Span)],
            state: &mut HashMap<GlobalId, u8>,
            order: &mut Vec<usize>,
            errs: &mut Vec<(Span, GlobalId)>,
        ) {
            match state.get(&g) {
                Some(2) => return,
                Some(1) => {
                    if let Some(&i) = map.get(&g) {
                        errs.push((checked[i].3, g));
                    }
                    return;
                }
                _ => {}
            }
            state.insert(g, 1);
            if let Some(&i) = map.get(&g) {
                for d in &checked[i].2 {
                    visit(*d, map, checked, state, order, errs);
                }
                order.push(i);
            }
            state.insert(g, 2);
        }
        let mut errs = Vec::new();
        for c in &checked {
            visit(c.0, &map, &checked, &mut state, &mut order, &mut errs);
        }
        for (span, g) in errs {
            let n = self.globals[g as usize].name.clone();
            self.err(span, format!("cyclic static initialization involving '{}'", n));
        }
        let mut body = Vec::new();
        for i in order {
            let (gid, ex, _, span) = checked[i].clone();
            body.push(hir::Stmt { kind: StmtKind::Assign(Place::Global(gid), ex), span });
        }
        let f = &mut self.funcs[fid as usize];
        f.locals = locals;
        f.body = body;
        fid
    }
}

pub static NULL_EXPR: ast::Expr = ast::Expr { kind: ast::ExprKind::Null, span: Span { file: 0, line: 0, col: 0 } };

pub fn is_builtin_type_name(n: &str) -> bool {
    int_type_by_name(n).is_some()
        || float_type_by_name(n).is_some()
        || matches!(n, "Boolean" | "String" | "IntLarge" | "DTVariable" | "STVariable" | "Dictionary" | "Function")
}

fn has_wildcard(params: &[ast::Param]) -> bool {
    fn w(t: &TypeExpr) -> bool {
        match t {
            TypeExpr::Wildcard(_) => true,
            TypeExpr::Named { args, .. } => args.iter().any(w),
            TypeExpr::Array(e) | TypeExpr::Nullable(e) => w(e),
            TypeExpr::Union(ts) | TypeExpr::Tuple(ts) => ts.iter().any(w),
            TypeExpr::Func(ps, r) => ps.iter().any(w) || w(r),
            TypeExpr::Ref { inner, .. } => w(inner),
            TypeExpr::Void => false,
        }
    }
    params.iter().any(|p| w(&p.ty))
}

/// Type parameters of a generic function, including anonymous ones introduced by `[?]`.
pub fn generic_params(d: &ast::FuncDecl) -> Vec<(String, Option<TypeExpr>)> {
    let mut out: Vec<(String, Option<TypeExpr>)> = d.type_params.iter().map(|t| (t.name.clone(), t.bound.clone())).collect();
    let mut n = 0;
    fn w(t: &TypeExpr, n: &mut usize, out: &mut Vec<(String, Option<TypeExpr>)>) {
        match t {
            TypeExpr::Wildcard(_) => {
                out.push((format!("?{}", n), None));
                *n += 1;
            }
            TypeExpr::Named { args, .. } => args.iter().for_each(|a| w(a, n, out)),
            TypeExpr::Array(e) | TypeExpr::Nullable(e) => w(e, n, out),
            TypeExpr::Union(ts) | TypeExpr::Tuple(ts) => ts.iter().for_each(|a| w(a, n, out)),
            TypeExpr::Func(ps, r) => {
                ps.iter().for_each(|a| w(a, n, out));
                w(r, n, out)
            }
            TypeExpr::Ref { inner, .. } => w(inner, n, out),
            TypeExpr::Void => {}
        }
    }
    for p in &d.params {
        w(&p.ty, &mut n, &mut out);
    }
    out
}

/// Replaces `?` wildcards by their anonymous type parameter names (`?0`, `?1`, ...).
pub fn wildcard_to_params(params: &[ast::Param]) -> Vec<ast::Param> {
    let mut n = 0;
    fn w(t: &TypeExpr, n: &mut usize) -> TypeExpr {
        match t {
            TypeExpr::Wildcard(span) => {
                let r = TypeExpr::named(&format!("?{}", n), *span);
                *n += 1;
                r
            }
            TypeExpr::Named { name, args, span } => TypeExpr::Named { name: name.clone(), args: args.iter().map(|a| w(a, n)).collect(), span: *span },
            TypeExpr::Array(e) => TypeExpr::Array(Box::new(w(e, n))),
            TypeExpr::Nullable(e) => TypeExpr::Nullable(Box::new(w(e, n))),
            TypeExpr::Union(ts) => TypeExpr::Union(ts.iter().map(|a| w(a, n)).collect()),
            TypeExpr::Tuple(ts) => TypeExpr::Tuple(ts.iter().map(|a| w(a, n)).collect()),
            TypeExpr::Func(ps, r) => TypeExpr::Func(ps.iter().map(|a| w(a, n)).collect(), Box::new(w(r, n))),
            TypeExpr::Ref { mutable, inner } => TypeExpr::Ref { mutable: *mutable, inner: Box::new(w(inner, n)) },
            TypeExpr::Void => TypeExpr::Void,
        }
    }
    params.iter().map(|p| ast::Param { ty: w(&p.ty, &mut n), name: p.name.clone(), mods: p.mods.clone(), span: p.span }).collect()
}

fn collect_globals(e: &hir::Expr, out: &mut Vec<GlobalId>) {
    crate::hir_visit::walk_expr(e, &mut |x| {
        if let ExprKind::Global(g) = &x.kind {
            out.push(*g);
        }
    });
}

fn clear_drops(stmts: &mut [hir::Stmt]) {
    crate::hir_visit::walk_stmts_mut(stmts, &mut |s| {
        if let StmtKind::Block { drops, .. } = &mut s.kind {
            drops.clear();
        }
    });
}

fn filter_drops(stmts: &mut [hir::Stmt], keep: &dyn Fn(LocalId) -> bool) {
    crate::hir_visit::walk_stmts_mut(stmts, &mut |s| {
        if let StmtKind::Block { drops, .. } = &mut s.kind {
            drops.retain(|l| keep(*l));
        }
    });
}
