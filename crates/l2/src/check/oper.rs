//! Operator overloading (spec 6.9), `new` and qualified type names in expressions (spec 10.4,
//! 14.3), formatted f-string fields (spec 11.1) and the standard library's numeric intrinsics.

use super::expr::OArg;
use super::*;
use crate::ast::{BinOp, ExprKind as A, UnOp};
use crate::hir::{Expr as HExpr, ExprKind as H};
use l2_runtime::format::{check_spec, parse_spec, Kind};
use l2_runtime::ops::ArithOp;
use l2_runtime::{Builtin, IntTy};

impl<'a> Checker<'a> {
    fn bad(&self, span: Span) -> HExpr {
        HExpr::new(H::Lit(Lit::Null), Type::Error, span)
    }

    pub fn is_object(&self, t: &Type) -> bool {
        matches!(t.deref(), Type::Class(_) | Type::Iface(_))
    }

    fn owner_of(&self, t: &Type) -> Option<Owner> {
        match t.deref() {
            Type::Class(c) => Some(Owner::Class(*c)),
            Type::Iface(i) => Some(Owner::Iface(*i)),
            _ => None,
        }
    }

    // ------------------------------------------------------------------ overload selection
    /// Checks the non-literal operands; their types select the overload.
    fn settle(&mut self, args: Vec<OArg<'a>>) -> Vec<OArg<'a>> {
        args.into_iter()
            .map(|a| match a {
                OArg::Ast(e) if !expr::is_literalish(e) => OArg::Done(self.arg_expr_pub(e)),
                a => a,
            })
            .collect()
    }

    fn arg_fits(&self, a: &OArg<'a>, p: &Type) -> bool {
        match a {
            OArg::Done(x) => self.assignable(&x.ty, p) || (matches!(p, Type::Ref(true, _)) && x.ty == *p),
            OArg::Ast(e) => self.literal_fits_pub(e, p),
        }
    }

    fn arg_desc(&self, a: &OArg<'a>) -> String {
        match a {
            OArg::Done(x) => self.tname(&x.ty),
            OArg::Ast(_) => "literal".into(),
        }
    }

    /// The most specific candidate whose parameters accept the arguments: `Err(true)` when
    /// ambiguous, `Err(false)` when none applies.
    fn pick(&self, cands: &[Vec<Type>], args: &[OArg<'a>]) -> Result<usize, bool> {
        let applicable: Vec<usize> = (0..cands.len()).filter(|&i| cands[i].len() == args.len() && cands[i].iter().zip(args).all(|(p, a)| self.arg_fits(a, p))).collect();
        if applicable.is_empty() {
            return Err(false);
        }
        let more_specific = |a: usize, b: usize| cands[a].iter().zip(cands[b].iter()).all(|(x, y)| self.assignable(x, y));
        let winners: Vec<usize> = applicable.iter().copied().filter(|&a| applicable.iter().all(|&b| a == b || more_specific(a, b))).collect();
        if winners.len() == 1 {
            return Ok(winners[0]);
        }
        // literal operands prefer their own type family (`2` -> Int32, `2.0` -> Float64)
        let rank = |p: &Type, e: &ast::Expr| -> u32 {
            let is_int = matches!(e.kind, A::Int(_)) || matches!(&e.kind, A::Unary(UnOp::Neg, i) if matches!(i.kind, A::Int(_)));
            match (p.deref().non_null(), is_int) {
                (Type::Int(IntTy::I32), true) => 0,
                (Type::Int(_), true) => 1,
                (Type::Float(FloatTy::F64), false) => 0,
                (Type::Float(_), _) => 2,
                (Type::Big, _) => 3,
                _ => 4,
            }
        };
        let score = |ci: usize| -> u32 {
            args.iter()
                .enumerate()
                .map(|(i, a)| match a {
                    OArg::Ast(e) => rank(&cands[ci][i], e),
                    OArg::Done(_) => 0,
                })
                .sum()
        };
        let best = applicable.iter().map(|&c| score(c)).min().unwrap_or(0);
        let lit: Vec<usize> = applicable.iter().copied().filter(|&c| score(c) == best).collect();
        if lit.len() == 1 {
            return Ok(lit[0]);
        }
        Err(true)
    }

    fn finish_args(&mut self, args: Vec<OArg<'a>>, params: &[Type]) -> Vec<HExpr> {
        args.into_iter()
            .zip(params.iter())
            .map(|(a, p)| {
                let x = match a {
                    OArg::Done(x) => x,
                    OArg::Ast(e) => self.expr(e, Some(p)),
                };
                let sp = x.span;
                self.pass_arg(x, p, sp)
            })
            .collect()
    }

    // ------------------------------------------------------------------ operators
    /// Calls the instance method `name` (an operator) on `recv`.
    pub fn operator_call(&mut self, recv: HExpr, name: &str, args: Vec<OArg<'a>>, span: Span, what: &str) -> HExpr {
        let args = self.settle(args);
        let Some(owner) = self.owner_of(&recv.ty) else {
            let n = self.tname(&recv.ty);
            self.err(span, format!("type {} does not support {}", n, what));
            return self.bad(span);
        };
        let ms: Vec<MethodInfo> = self.lookup_methods(owner, name).into_iter().filter(|m| !m.is_static && m.generic.is_none()).collect();
        let tn = self.tname(&recv.ty);
        if ms.is_empty() {
            self.err(span, format!("type {} does not support {} (it has no '{}' method)", tn, what, name));
            return self.bad(span);
        }
        let cands: Vec<Vec<Type>> = ms.iter().map(|m| m.params.clone()).collect();
        match self.pick(&cands, &args) {
            Ok(i) => {
                let m = ms[i].clone();
                let out = self.finish_args(args, &m.params);
                self.emit_method(&m, Some(recv), out, span, false)
            }
            Err(ambiguous) => {
                let ds: Vec<String> = args.iter().map(|a| self.arg_desc(a)).collect();
                if ambiguous {
                    self.err(span, format!("call to '{}' of {} is ambiguous for ({})", name, tn, ds.join(", ")));
                } else {
                    self.err(span, format!("no '{}' of {} accepts ({})", name, tn, ds.join(", ")));
                }
                self.bad(span)
            }
        }
    }

    /// `a op b` with an object operand: `a.operator op(b)`, or a static `operator op(a, b)` of
    /// either operand's class (e.g. `2.0 * m`).
    pub fn operator_binary(&mut self, op: BinOp, l: OArg<'a>, r: OArg<'a>, span: Span) -> HExpr {
        let name = format!("operator{}", op.symbol());
        let mut args = self.settle(vec![l, r]);
        let r = args.pop().unwrap();
        let l = args.pop().unwrap();
        let ty_of = |a: &OArg<'a>| match a {
            OArg::Done(x) => Some(x.ty.clone()),
            OArg::Ast(_) => None,
        };
        let (lt, rt) = (ty_of(&l), ty_of(&r));
        if lt.as_ref().map(|t| t.is_error()).unwrap_or(false) || rt.as_ref().map(|t| t.is_error()).unwrap_or(false) {
            return self.bad(span);
        }
        // `+` with a String operand is concatenation (spec 6.1)
        if op == BinOp::Add && (lt.as_ref().map(|t| *t.deref() == Type::Str).unwrap_or(false) || rt.as_ref().map(|t| *t.deref() == Type::Str).unwrap_or(false)) {
            let mut vals = Vec::new();
            for a in [l, r] {
                vals.push(match a {
                    OArg::Done(x) => x,
                    OArg::Ast(e) => self.expr(e, None),
                });
            }
            let y = vals.pop().unwrap();
            let x = vals.pop().unwrap();
            return self.binary_typed(op, x, y, span);
        }
        // instance methods of the left operand
        let mut inst: Vec<MethodInfo> = Vec::new();
        if let Some(owner) = lt.as_ref().and_then(|t| self.owner_of(t)) {
            inst = self.lookup_methods(owner, &name).into_iter().filter(|m| !m.is_static && m.params.len() == 1 && m.generic.is_none()).collect();
        }
        // static methods of either operand's class
        let mut stat: Vec<MethodInfo> = Vec::new();
        for t in [&lt, &rt].into_iter().flatten() {
            if let Some(owner) = self.owner_of(t) {
                for m in self.lookup_methods(owner, &name) {
                    if m.is_static && m.params.len() == 2 && m.generic.is_none() && !stat.iter().any(|x| x.func == m.func) {
                        stat.push(m);
                    }
                }
            }
        }
        let cands: Vec<Vec<Type>> = inst.iter().map(|m| m.params.clone()).collect();
        let rargs = vec![r.clone()];
        let inst_pick = self.pick(&cands, &rargs);
        if let Ok(i) = inst_pick {
            let m = inst[i].clone();
            let OArg::Done(recv) = l else { unreachable!() };
            let out = self.finish_args(rargs, &m.params);
            return self.emit_method(&m, Some(recv), out, span, false);
        }
        let scands: Vec<Vec<Type>> = stat.iter().map(|m| m.params.clone()).collect();
        let both = vec![l.clone(), r.clone()];
        let stat_pick = self.pick(&scands, &both);
        if let Ok(i) = stat_pick {
            let m = stat[i].clone();
            let out = self.finish_args(both, &m.params);
            return self.emit_method(&m, None, out, span, true);
        }
        let (a, b) = (self.arg_desc(&l), self.arg_desc(&r));
        if inst_pick == Err(true) || stat_pick == Err(true) {
            self.err(span, format!("operator '{}' is ambiguous for {} and {}", op.symbol(), a, b));
        } else if inst.is_empty() && stat.is_empty() {
            self.err(span, format!("operator '{}' is not defined for {} and {} (declare 'operator{}' to overload it)", op.symbol(), a, b, op.symbol()));
        } else {
            self.err(span, format!("no 'operator{}' accepts {} and {}", op.symbol(), a, b));
        }
        self.bad(span)
    }

    /// `-x` / `~x` on an object.
    pub fn operator_unary(&mut self, sym: &str, x: HExpr, span: Span) -> HExpr {
        let name = format!("operator{}", sym);
        let Some(owner) = self.owner_of(&x.ty) else { return self.bad(span) };
        let ms = self.lookup_methods(owner, &name);
        if let Some(m) = ms.iter().find(|m| !m.is_static && m.params.is_empty() && m.generic.is_none()).cloned() {
            return self.emit_method(&m, Some(x), Vec::new(), span, false);
        }
        let stat: Vec<MethodInfo> = ms.into_iter().filter(|m| m.is_static && m.params.len() == 1 && m.generic.is_none()).collect();
        let cands: Vec<Vec<Type>> = stat.iter().map(|m| m.params.clone()).collect();
        let args = vec![OArg::Done(x)];
        if let Ok(i) = self.pick(&cands, &args) {
            let m = stat[i].clone();
            let out = self.finish_args(args, &m.params);
            return self.emit_method(&m, None, out, span, true);
        }
        let n = self.arg_desc(&args[0]);
        self.err(span, format!("unary '{}' is not defined for {} (declare 'operator{}' to overload it)", sym, n, sym));
        self.bad(span)
    }

    /// `obj[i, j, ...]` through `operator[]`.
    pub fn operator_index(&mut self, recv: HExpr, idx: &'a [ast::Expr], span: Span) -> HExpr {
        let args = idx.iter().map(OArg::Ast).collect();
        self.operator_call(recv, "operator[]", args, span, "indexing")
    }

    // ------------------------------------------------------------------ formatted fields
    /// `{value!conv:spec}` of an f-string.
    pub fn format_field(&mut self, h: HExpr, conv: &str, spec: &'a [ast::FStrPart], span: Span) -> HExpr {
        if spec.is_empty() && conv.is_empty() {
            return h;
        }
        let mut parts = Vec::new();
        let mut literal = Some(String::new());
        for p in spec {
            match p {
                ast::FStrPart::Lit(s) => {
                    if let Some(l) = literal.as_mut() {
                        l.push_str(s);
                    }
                    parts.push(HExpr::new(H::Lit(Lit::Str(s.clone())), Type::Str, span));
                }
                ast::FStrPart::Expr(x) => {
                    literal = None;
                    let e = self.expr(x, None);
                    parts.push(e);
                }
                ast::FStrPart::Fmt { .. } => {
                    literal = None;
                    self.err(span, "format specifiers cannot nest formatted fields");
                }
            }
        }
        let spec_expr = match &literal {
            Some(l) => {
                self.check_format_spec(l, &h.ty, !conv.is_empty(), span);
                HExpr::new(H::Lit(Lit::Str(l.clone())), Type::Str, span)
            }
            None => HExpr::new(H::Concat(parts), Type::Str, span),
        };
        let conv = HExpr::new(H::Lit(Lit::Str(conv.to_string())), Type::Str, span);
        HExpr::new(H::Builtin(Builtin::FormatValue, vec![h, spec_expr, conv]), Type::Str, span)
    }

    /// Compile-time check of a literal format specifier against the value's type.
    pub fn check_format_spec(&mut self, spec: &str, ty: &Type, as_text: bool, span: Span) {
        if spec.is_empty() {
            return;
        }
        let sp = match parse_spec(spec) {
            Ok(sp) => sp,
            Err(e) => {
                self.err(span, e);
                return;
            }
        };
        let kind = if as_text {
            Some(Kind::Text)
        } else {
            match ty.deref() {
                Type::Int(_) | Type::Big => Some(Kind::Int),
                Type::Float(_) => Some(Kind::Float),
                Type::Bool => Some(Kind::Bool),
                Type::Str | Type::Class(_) | Type::Iface(_) | Type::Array(_) | Type::Dict(_, _) | Type::Tuple(_) | Type::Func(_, _) => Some(Kind::Text),
                _ => None,
            }
        };
        if let Some(k) = kind {
            let n = self.tname(ty.deref());
            if let Err(e) = check_spec(&sp, k, &n) {
                self.err(span, e);
            }
        }
    }

    // ------------------------------------------------------------------ classes by name
    /// The leading identifier of `a.b.c` / `a.b[T]`.
    fn root_ident(e: &ast::Expr) -> Option<&str> {
        match &e.kind {
            A::Ident(n) => Some(n),
            A::Member(o, _) => Self::root_ident(o),
            A::Index(b, _) => Self::root_ident(b),
            _ => None,
        }
    }

    /// A dotted name in expression position that is not rooted at a local variable or field.
    pub fn type_path_of(&mut self, e: &'a ast::Expr) -> Option<String> {
        let root = Self::root_ident(e)?;
        if self.lookup_local(root).is_some() || self.current_class_pub().and_then(|c| self.field_index(c, root)).is_some() {
            return None;
        }
        crate::parser::dotted_name(e)
    }

    /// The class named by a type written in expression position (`Shared`, `lib.Shared`,
    /// `Box[Int64]`), used for static members. Generic classes without type arguments take
    /// the expected type's arguments or their defaults.
    pub fn static_class_of(&mut self, e: &'a ast::Expr, expected: Option<&Type>) -> Option<ClassId> {
        let (path_expr, targs) = match &e.kind {
            A::Index(b, t) => (&**b, Some(t)),
            _ => (e, None),
        };
        let path = self.type_path_of(path_expr)?;
        let module = self.current_module_pub();
        let fqn = match self.find_type(module, &path) {
            Ok(Some(f)) if self.class_decls.contains_key(&f) => f,
            _ => return None,
        };
        if self.ide.is_some() {
            let pos = match &path_expr.kind {
                A::Member(..) => self.ide_member_pos(path_expr.span),
                _ => Some(path_expr.span),
            };
            if let Some(p) = pos {
                self.ide_type_use(&fqn, p);
            }
        }
        let targs = match targs {
            Some(ts) => {
                let subst = self.cur_ref().subst.clone();
                let mut out = Vec::new();
                for t in ts.iter() {
                    let te = crate::parser::expr_to_type(t)?;
                    out.push(self.resolve_type(&te, module, &subst));
                }
                Some(out)
            }
            None => None,
        };
        self.class_for(&fqn, targs, expected, e.span)
    }

    /// Instantiates a class for static access or construction.
    fn class_for(&mut self, fqn: &str, targs: Option<Vec<Type>>, expected: Option<&Type>, span: Span) -> Option<ClassId> {
        let nparams = self.class_decls[fqn].1.type_params.len();
        if nparams == 0 {
            return self.instantiate_class(fqn, Vec::new(), span);
        }
        if let Some(t) = targs {
            let t = self.complete_targs(fqn, t);
            return self.instantiate_class(fqn, t, span);
        }
        if let Some(Type::Class(c)) = expected.map(|t| t.deref().non_null()) {
            if self.cmeta[c as usize].template == fqn {
                return Some(c);
            }
        }
        if self.all_defaults(fqn) {
            let t = self.complete_targs(fqn, Vec::new());
            return self.instantiate_class(fqn, t, span);
        }
        self.instantiate_class(fqn, vec![Type::Dyn; nparams], span)
    }

    /// `C.name(args)`: a static method of class `c` (or `C.array(...)`).
    pub fn static_call(&mut self, c: ClassId, name: &str, args: &'a [ast::Arg], span: Span) -> HExpr {
        if name == "fancy" && args.len() == 1 && self.cmeta[c as usize].template == "text.Regex" {
            self.check_regex_literal(&args[0].value, true);
        }
        let ms: Vec<MethodInfo> = self.lookup_methods(Owner::Class(c), name).into_iter().filter(|m| m.is_static).collect();
        if ms.is_empty() {
            if name == "array" {
                return self.array_new(Type::Class(c), args, span);
            }
            let n = self.classes[c as usize].name.clone();
            self.err(span, format!("class '{}' has no static method '{}'", n, name));
            return self.bad(span);
        }
        self.invoke_methods(ms, None, args, span, true)
    }

    /// Constructs class `fqn`: explicit type arguments, the expected type's, inferred from the
    /// constructor arguments, or the defaults.
    pub fn construct_named(&mut self, fqn: &str, targs: Option<Vec<Type>>, args: &'a [ast::Arg], expected: Option<&Type>, span: Span) -> HExpr {
        // literal patterns are checked now rather than when the program runs
        if fqn == "text.Regex" && args.len() == 1 {
            self.check_regex_literal(&args[0].value, false);
        }
        let nparams = self.class_decls[fqn].1.type_params.len();
        if nparams == 0 || targs.is_some() {
            return match self.class_for(fqn, targs, None, span) {
                Some(c) => self.construct(c, args, span),
                None => self.bad(span),
            };
        }
        if let Some(Type::Class(c)) = expected.map(|t| t.deref().non_null()) {
            if self.cmeta[c as usize].template == fqn {
                return self.construct(c, args, span);
            }
        }
        self.construct_generic_inferred(fqn, args, span)
    }

    /// `new T(args)` (spec 10.4).
    pub fn new_expr(&mut self, callee: &'a ast::Expr, args: &'a [ast::Arg], expected: Option<&Type>, span: Span) -> HExpr {
        if self.ide.is_none() {
            return self.new_expr_inner(callee, args, expected, span);
        }
        let path_expr = match &callee.kind {
            A::Index(b, _) => &**b,
            _ => callee,
        };
        let pos = match &path_expr.kind {
            A::Member(..) => self.ide_member_pos(path_expr.span),
            _ => Some(path_expr.span),
        };
        let Some(path) = crate::parser::dotted_name(path_expr) else {
            return self.new_expr_inner(callee, args, expected, span);
        };
        if let Some(p) = pos {
            self.ide_target_type(p, Some(&path));
        }
        // the constructor is recorded as the callee; the class when no constructor was chosen
        let simple = path.rsplit('.').next().unwrap_or(&path).to_string();
        self.ide_call_push(pos, &simple);
        let r = self.new_expr_inner(callee, args, expected, span);
        self.ide_call_pop();
        if let (Some(p), Type::Class(c)) = (pos, &r.ty) {
            let fqn = self.cmeta[*c as usize].template.clone();
            self.ide_type_use(&fqn, p);
        }
        r
    }

    fn new_expr_inner(&mut self, callee: &'a ast::Expr, args: &'a [ast::Arg], expected: Option<&Type>, span: Span) -> HExpr {
        let (path_expr, targs) = match &callee.kind {
            A::Index(b, t) => (&**b, Some(t)),
            _ => (callee, None),
        };
        let Some(path) = crate::parser::dotted_name(path_expr) else {
            self.err(span, "'new' needs a class name");
            return self.bad(span);
        };
        let module = self.current_module_pub();
        let fqn = match self.lookup_type(module, &path, span) {
            Some(TypeRef::Class(f)) => f,
            Some(TypeRef::Iface(_)) => {
                self.err(span, format!("interface '{}' cannot be instantiated", path));
                return self.bad(span);
            }
            None => {
                if self.find_type(module, &path).is_ok() {
                    if is_builtin_type_name(&path) {
                        self.err(span, format!("'new' creates class instances; '{}' is a built-in type", path));
                    } else {
                        self.err(span, format!("unknown class '{}'", path));
                    }
                }
                return self.bad(span);
            }
        };
        let targs = match targs {
            Some(ts) => {
                let subst = self.cur_ref().subst.clone();
                let mut out = Vec::new();
                for t in ts.iter() {
                    match crate::parser::expr_to_type(t) {
                        Some(te) => out.push(self.resolve_type(&te, module, &subst)),
                        None => {
                            self.err(t.span, "expected a type argument");
                            return self.bad(span);
                        }
                    }
                }
                Some(out)
            }
            None => None,
        };
        self.construct_named(&fqn, targs, args, expected, span)
    }

    /// `f[T](...)`, `Box[T](...)`, `pkg.Box[T](...)`, `obj.method[T](...)`, `intr.f[T](...)`.
    pub fn call_with_type_args(&mut self, callee: &'a ast::Expr, base: &'a ast::Expr, targs: &'a [ast::Expr], args: &'a [ast::Arg], span: Span) -> Option<HExpr> {
        let module = self.current_module_pub();
        let path = self.type_path_of(base);
        // a type: construct with explicit type arguments
        if let Some(p) = &path {
            if let Ok(Some(f)) = self.find_type(module, p) {
                if self.class_decls.contains_key(&f) {
                    let tys = self.type_args(targs)?;
                    return Some(self.construct_named(&f, Some(tys), args, None, span));
                }
            }
        }
        match &base.kind {
            A::Ident(name) if self.lookup_local(name).is_none() => {
                let fs = self.lookup_funcs(module, name)?;
                let generic: Vec<FnRef<'a>> = fs.into_iter().filter(|f| f.func.is_none()).collect();
                if generic.len() != 1 {
                    return None;
                }
                let tys = self.type_args(targs)?;
                let fr = generic[0].clone();
                Some(match self.instantiate_generic(&fr, tys, span) {
                    Some(fid) => self.call_func(fid, args, span),
                    None => self.bad(span),
                })
            }
            A::Member(obj, name) => {
                if let A::Ident(n) = &obj.kind {
                    if self.lookup_local(n).is_none() && self.modules[module].imports.get(n) == Some(&Import::Intrinsics) {
                        return Some(self.intrinsic_call(name, Some(targs), args, span));
                    }
                }
                let _ = callee;
                // `Class.method[T](...)`: a generic static method
                if let Some(c) = self.static_class_of(obj, None) {
                    return Some(self.static_call_targs(c, name, targs, args, span));
                }
                Some(self.call_method_targs(obj, name, targs, args, span))
            }
            _ => None,
        }
    }

    fn type_args(&mut self, targs: &'a [ast::Expr]) -> Option<Vec<Type>> {
        let module = self.current_module_pub();
        let subst = self.cur_ref().subst.clone();
        let mut tys = Vec::new();
        for t in targs {
            match crate::parser::expr_to_type(t) {
                Some(te) => tys.push(self.resolve_type(&te, module, &subst)),
                None => {
                    self.err(t.span, "expected a type argument");
                    return None;
                }
            }
        }
        Some(tys)
    }

    /// `Class.method[T](args)`: a generic static method with explicit type arguments.
    fn static_call_targs(&mut self, c: ClassId, name: &str, targs: &'a [ast::Expr], args: &'a [ast::Arg], span: Span) -> HExpr {
        let gs: Vec<MethodInfo> = self.lookup_methods(Owner::Class(c), name).into_iter().filter(|m| m.generic.is_some() && m.is_static && m.param_names.len() == args.len()).collect();
        let Some(m) = gs.first().cloned() else {
            let n = self.classes[c as usize].name.clone();
            self.err(span, format!("class '{}' has no generic static method '{}' taking {} argument(s)", n, name, args.len()));
            return self.bad(span);
        };
        let Some(tys) = self.type_args(targs) else { return self.bad(span) };
        let Owner::Class(oc) = m.owner else { unreachable!() };
        self.check_access(m.access, oc, span, &m.name);
        let Some(fid) = self.instantiate_method(oc, m.generic.unwrap(), tys, span) else {
            return self.bad(span);
        };
        let sig = self.sigs[fid as usize].clone();
        let out = self.check_args_against(&sig.params, args);
        self.note_throws(&sig.throws, span);
        HExpr::new(H::Call(fid, out), sig.ret, span)
    }

    /// `obj.method[T](args)`: a generic method with explicit type arguments.
    fn call_method_targs(&mut self, obj: &'a ast::Expr, name: &str, targs: &'a [ast::Expr], args: &'a [ast::Arg], span: Span) -> HExpr {
        let recv = self.expr(obj, None);
        if recv.ty.is_error() {
            return recv;
        }
        let owner = match self.owner_of(&recv.ty) {
            Some(o @ Owner::Class(_)) => o,
            _ => {
                let n = self.tname(&recv.ty);
                self.err(span, format!("type {} has no generic method '{}'", n, name));
                return self.bad(span);
            }
        };
        let gs: Vec<MethodInfo> = self.lookup_methods(owner, name).into_iter().filter(|m| m.generic.is_some() && m.param_names.len() == args.len()).collect();
        let Some(m) = gs.first().cloned() else {
            let n = self.tname(&recv.ty);
            self.err(span, format!("type {} has no generic method '{}' taking {} argument(s)", n, name, args.len()));
            return self.bad(span);
        };
        let Some(tys) = self.type_args(targs) else { return self.bad(span) };
        let Owner::Class(c) = m.owner else { unreachable!() };
        self.check_access(m.access, c, span, &m.name);
        let Some(fid) = self.instantiate_method(c, m.generic.unwrap(), tys, span) else {
            return self.bad(span);
        };
        let sig = self.sigs[fid as usize].clone();
        let out = self.check_args_against(&sig.params, args);
        self.note_throws(&sig.throws, span);
        let mut all = Vec::new();
        if !m.is_static {
            all.push(recv);
        }
        all.extend(out);
        HExpr::new(H::Call(fid, all), sig.ret, span)
    }

    // ------------------------------------------------------------------ intrinsics
    /// Whether values of type `t` can be built from JSON; `Err` names the reason.
    fn json_decodable(&self, t: &Type) -> Result<(), String> {
        match t {
            Type::Class(c) => {
                if self.classes[*c as usize].from_json_fn.is_some() {
                    Ok(())
                } else {
                    Err(format!("class {} has no json[decode] fields", self.classes[*c as usize].name))
                }
            }
            Type::Array(e) | Type::Nullable(e) => self.json_decodable(e),
            Type::Dict(k, v) => {
                if !matches!(**k, Type::Str | Type::Int(_) | Type::Dyn) {
                    return Err("Dictionary keys must be String or an integer type".into());
                }
                self.json_decodable(v)
            }
            Type::Tuple(ts) | Type::Union(ts) => ts.iter().try_for_each(|t| self.json_decodable(t)),
            Type::Iface(_) => Err("interfaces cannot be decoded (use a class)".into()),
            Type::Func(_, _) => Err("functions have no JSON form".into()),
            _ => Ok(()),
        }
    }

    /// The language type of a system-operation signature letter (see `l2_runtime::sys`).
    fn sig_type(c: char) -> Type {
        match c {
            'S' => Type::Str,
            's' => Type::Nullable(Box::new(Type::Str)),
            'I' => Type::int64(),
            'F' => Type::Float(FloatTy::F64),
            'B' => Type::Bool,
            'Y' => Type::Array(Box::new(Type::Int(IntTy::U8))),
            'A' => Type::Array(Box::new(Type::Str)),
            'L' => Type::Array(Box::new(Type::int64())),
            'M' => Type::Dict(Box::new(Type::Str), Box::new(Type::Str)),
            'V' => Type::Void,
            _ => Type::Dyn,
        }
    }

    /// `intr.fsReadBytes(path)`: a system operation, checked against its signature.
    fn sys_intrinsic(&mut self, op: l2_runtime::sys::SysOp, args: &'a [ast::Arg], span: Span) -> HExpr {
        let (ps, rs) = op.sig().split_once('>').unwrap_or((op.sig(), "V"));
        let params: Vec<Type> = ps.chars().map(Self::sig_type).collect();
        let ret = if let Some(inner) = rs.strip_prefix('(') {
            Type::Tuple(inner.trim_end_matches(')').chars().map(Self::sig_type).collect())
        } else {
            Self::sig_type(rs.chars().next().unwrap_or('V'))
        };
        if args.len() != params.len() || args.iter().any(|a| a.name.is_some()) {
            self.err(span, format!("intrinsic '{}' takes {} positional argument(s)", op.name(), params.len()));
            return self.bad(span);
        }
        let mut out = vec![HExpr::new(H::Lit(Lit::Int(op.code() as i128)), Type::int32(), span)];
        for (a, p) in args.iter().zip(params.iter()) {
            let x = self.expr(&a.value, Some(p));
            out.push(if *p == Type::Dyn { x } else { self.coerce(x, p, a.value.span) });
        }
        HExpr::new(H::Builtin(Builtin::Sys, out), ret, span)
    }

    /// Numeric kernels for `math.linear` (standard library only).
    pub fn intrinsic_call(&mut self, name: &str, targs: Option<&'a [ast::Expr]>, args: &'a [ast::Arg], span: Span) -> HExpr {
        if let Some(op) = l2_runtime::sys::SysOp::by_name(name) {
            return self.sys_intrinsic(op, args, span);
        }
        // jsonDecode[T](text) / jsonConvert[T](value): JSON into a typed value (spec 10.8)
        if name == "jsonDecode" || name == "jsonConvert" {
            let Some(ts) = targs else {
                self.err(span, format!("intrinsic '{}' needs the result type: {}[T](...)", name, name));
                return self.bad(span);
            };
            let Some(tys) = self.type_args(ts) else { return self.bad(span) };
            let Some(t) = tys.first().cloned() else { return self.bad(span) };
            if args.len() != 1 {
                self.err(span, format!("intrinsic '{}' takes 1 argument", name));
                return self.bad(span);
            }
            if let Err(what) = self.json_decodable(&t) {
                self.err(span, format!("{} cannot be read from JSON: {}", self.tname(&t), what));
            }
            let pty = if name == "jsonDecode" { Type::Str } else { Type::Dyn };
            let x = self.expr(&args[0].value, Some(&pty));
            let x = if pty == Type::Dyn { x } else { self.coerce(x, &pty, args[0].value.span) };
            let code = HExpr::new(H::Lit(Lit::Str(rt_type(&t).encode())), Type::Str, span);
            if name == "jsonDecode" {
                return HExpr::new(H::Builtin(Builtin::JsonDecode, vec![x, code]), t, span);
            }
            let path = HExpr::new(H::Lit(Lit::Str("$".into())), Type::Str, span);
            return HExpr::new(H::Builtin(Builtin::JsonConvert, vec![x, code, path]), t, span);
        }
        let wrap = self.cur_ref().wrap;
        let arity = match name {
            "zip" => 4,
            "scalar" => 5,
            "matmul" => 6,
            "round" => 6,
            "migrate" => 3,
            "transpose" => 3,
            _ => {
                self.err(span, format!("unknown intrinsic '{}'", name));
                return self.bad(span);
            }
        };
        if args.len() != arity || args.iter().any(|a| a.name.is_some()) {
            self.err(span, format!("intrinsic '{}' takes {} positional arguments", name, arity));
            return self.bad(span);
        }
        let lit_bool = |b: bool| HExpr::new(H::Lit(Lit::Bool(b)), Type::Bool, span);
        let lit_i64 = |v: i128| HExpr::new(H::Lit(Lit::Int(v)), Type::int64(), span);
        let int_arg = |s: &mut Self, a: &'a ast::Arg| {
            let x = s.expr(&a.value, Some(&Type::int64()));
            s.coerce(x, &Type::int64(), a.value.span)
        };
        let op_code = |s: &mut Self, a: &'a ast::Arg| -> Option<i128> {
            let code = match &a.value.kind {
                A::Str(op) => match op.as_str() {
                    "+" => Some(ArithOp::Add),
                    "-" => Some(ArithOp::Sub),
                    "*" => Some(ArithOp::Mul),
                    "/" => Some(ArithOp::Div),
                    "%" => Some(ArithOp::Rem),
                    "**" => Some(ArithOp::Pow),
                    _ => None,
                },
                _ => None,
            };
            if code.is_none() {
                s.err(a.value.span, "expected an arithmetic operator literal such as \"+\"");
            }
            code.map(|c| c.code() as i128)
        };
        // the numeric array operand
        let arr = self.expr(&args[if matches!(name, "zip" | "scalar") { 1 } else { 0 }].value, None);
        let aty = arr.ty.deref().clone();
        let et = match &aty {
            Type::Array(e) if e.is_numeric() => (**e).clone(),
            Type::Error => return self.bad(span),
            other => {
                let n = self.tname(other);
                self.err(span, format!("intrinsic '{}' needs a numeric array, found {}", name, n));
                return self.bad(span);
            }
        };
        let (bi, out, ty) = match name {
            "zip" => {
                let Some(op) = op_code(self, &args[0]) else { return self.bad(span) };
                let b = self.expr(&args[2].value, Some(&aty));
                let b = self.coerce(b, &aty, args[2].value.span);
                let t = int_arg(self, &args[3]);
                (Builtin::TensorZip, vec![lit_i64(op), arr, b, lit_bool(wrap), t], aty)
            }
            "scalar" => {
                let Some(op) = op_code(self, &args[0]) else { return self.bad(span) };
                let sv = self.expr(&args[2].value, Some(&et));
                let sv = self.coerce(sv, &et, args[2].value.span);
                let left = self.expr(&args[3].value, Some(&Type::Bool));
                let left = self.coerce(left, &Type::Bool, args[3].value.span);
                let t = int_arg(self, &args[4]);
                (Builtin::TensorScalar, vec![lit_i64(op), arr, sv, left, lit_bool(wrap), t], aty)
            }
            "matmul" => {
                let b = self.expr(&args[1].value, Some(&aty));
                let b = self.coerce(b, &aty, args[1].value.span);
                let mut v = vec![arr, b];
                for a in &args[2..6] {
                    v.push(int_arg(self, a));
                }
                v.insert(5, lit_bool(wrap));
                (Builtin::TensorMatMul, v, aty)
            }
            "round" => {
                let mut v = vec![arr];
                for a in &args[1..5] {
                    v.push(int_arg(self, a));
                }
                v.push(lit_bool(wrap));
                v.push(int_arg(self, &args[5]));
                (Builtin::TensorRound, v, aty)
            }
            "migrate" => {
                let Some(ts) = targs else {
                    self.err(span, "intrinsic 'migrate' needs the target element type: migrate[U](...)");
                    return self.bad(span);
                };
                let Some(tys) = self.type_args(ts) else { return self.bad(span) };
                let Some(u) = tys.first().cloned() else { return self.bad(span) };
                if !u.is_numeric() {
                    let n = self.tname(&u);
                    self.err(span, format!("cannot migrate to non-numeric type {}", n));
                }
                let code = HExpr::new(H::Lit(Lit::Str(rt_type(&u).encode())), Type::Str, span);
                let mode = self.expr(&args[1].value, Some(&Type::Str));
                let mode = self.coerce(mode, &Type::Str, args[1].value.span);
                let t = int_arg(self, &args[2]);
                (Builtin::TensorMigrate, vec![arr, code, mode, t], Type::Array(Box::new(u)))
            }
            _ => {
                let r = int_arg(self, &args[1]);
                let c = int_arg(self, &args[2]);
                (Builtin::TensorTranspose, vec![arr, r, c], aty)
            }
        };
        HExpr::new(H::Builtin(bi, out), ty, span)
    }
}
