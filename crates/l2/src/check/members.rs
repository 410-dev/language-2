//! Typing of built-in members: String, numbers, arrays, Dictionary, stdio and `castTo`.

use super::*;
use crate::ast::ExprKind as A;
use crate::hir::{Expr as HExpr, ExprKind as H};
use l2_runtime::ops::ArithOp;
use l2_runtime::{Builtin, IntTy};

#[derive(Clone)]
pub enum P {
    /// An index (Int64, signed integer types only).
    Idx,
    /// An owned value (consumed).
    Val(Type),
    /// A borrowed value.
    Borrow(Type),
}

impl<'a> Checker<'a> {
    fn none(&self, span: Span) -> HExpr {
        HExpr::new(H::Lit(Lit::Null), Type::Error, span)
    }

    pub fn bargs(&mut self, args: &'a [ast::Arg], params: &[P], min: usize, what: &str, span: Span) -> Option<Vec<HExpr>> {
        if let Some(a) = args.iter().find(|a| a.name.is_some()) {
            self.err(a.value.span, format!("{} does not take named arguments", what));
            return None;
        }
        if args.len() < min || args.len() > params.len() {
            let n = if min == params.len() { format!("{}", min) } else { format!("{} to {}", min, params.len()) };
            self.err(span, format!("{} takes {} argument(s) but {} were given", what, n, args.len()));
            return None;
        }
        let mut out = Vec::new();
        for (a, p) in args.iter().zip(params.iter()) {
            let x = match p {
                P::Idx => self.index_arg(&a.value),
                P::Val(t) => {
                    let x = self.expr(&a.value, Some(t));
                    let x = self.coerce(x, t, a.value.span);
                    self.consume(x)
                }
                P::Borrow(t) => {
                    let x = self.expr(&a.value, Some(t));
                    if *t == Type::Dyn {
                        x
                    } else {
                        self.coerce(x, t, a.value.span)
                    }
                }
            };
            out.push(x);
        }
        Some(out)
    }

    fn any_arg(&mut self, a: &'a ast::Arg) -> HExpr {
        let x = self.expr(&a.value, None);
        if x.ty == Type::Void {
            self.err(a.value.span, "a void expression has no value");
        }
        x
    }

    pub fn mut_place_pub(&mut self, obj: &'a ast::Expr, name: &str, span: Span) -> Option<Place> {
        self.mut_place(obj, name, span)
    }

    /// Resolves the receiver of a mutating method to a place.
    fn mut_place(&mut self, obj: &'a ast::Expr, name: &str, span: Span) -> Option<Place> {
        match self.place_of(obj) {
            Some((p, _, true, _)) => Some(p),
            Some((_, _, false, why)) => {
                self.err(span, format!("cannot call mutating method '{}': {}", name, why));
                None
            }
            None => {
                self.err(span, format!("mutating method '{}' needs a variable or field as receiver", name));
                None
            }
        }
    }

    pub fn cast_expr(&mut self, recv: HExpr, args: &'a [ast::Arg], span: Span) -> HExpr {
        if args.len() != 1 {
            self.err(span, "castTo takes exactly one type argument");
            return self.none(span);
        }
        let Some(te) = crate::parser::expr_to_type(&args[0].value) else {
            self.err(args[0].value.span, "castTo expects a type, e.g. x.castTo(Int8)");
            return self.none(span);
        };
        let module = self.current_module_pub();
        let subst = self.cur_ref().subst.clone();
        let target = self.resolve_type(&te, module, &subst);
        let from = recv.ty.deref().clone();
        let ok = from == target
            || target.is_error()
            || from.is_error()
            || (from.is_numeric() && target.is_numeric())
            || matches!(from, Type::Dyn | Type::Union(_) | Type::Nullable(_))
            || self.assignable(&from, &target)
            || self.assignable(&target, &from)
            || matches!((&from, &target), (Type::Iface(_), Type::Class(_)) | (Type::Class(_), Type::Iface(_)) | (Type::Iface(_), Type::Iface(_)));
        if !ok {
            let (a, b) = (self.tname(&from), self.tname(&target));
            self.err(span, format!("cannot cast {} to {}", a, b));
        }
        let wrap = self.cur_ref().wrap;
        let recv = if target.is_copy() { recv } else { self.consume(recv) };
        HExpr::new(H::Cast(Box::new(recv), wrap), target, span)
    }

    pub fn current_module_pub(&self) -> usize {
        self.cur_ref().module
    }

    /// Methods available on every value.
    fn generic_method(&mut self, recv: HExpr, name: &str, args: &'a [ast::Arg], span: Span) -> Option<HExpr> {
        let rty = recv.ty.deref().clone();
        Some(match name {
            "equals" => {
                let a = self.bargs(args, &[P::Borrow(rty.clone())], 1, "equals", span)?;
                HExpr::new(H::Builtin(Builtin::Equals, vec![recv, a.into_iter().next().unwrap()]), Type::Bool, span)
            }
            "isSameReferenceWith" => {
                let a = self.bargs(args, &[P::Borrow(Type::Dyn)], 1, "isSameReferenceWith", span)?;
                HExpr::new(H::Builtin(Builtin::SameRef, vec![recv, a.into_iter().next().unwrap()]), Type::Bool, span)
            }
            "toString" | "string" => {
                self.bargs(args, &[], 0, name, span)?;
                HExpr::new(H::Builtin(Builtin::ToString, vec![recv]), Type::Str, span)
            }
            "clone" => {
                self.bargs(args, &[], 0, "clone", span)?;
                HExpr::new(H::Builtin(Builtin::Clone, vec![recv]), rty, span)
            }
            "castTo" => self.cast_expr(recv, args, span),
            // JSON text of strings, numbers, Booleans, arrays and dictionaries (spec 14.5)
            "toJson" => {
                let a = self.bargs(args, &[P::Val(Type::int64())], 0, "toJson", span)?;
                let indent = a.into_iter().next().unwrap_or_else(|| HExpr::new(H::Lit(Lit::Int(0)), Type::int64(), span));
                let code = HExpr::new(H::Lit(Lit::Int(l2_runtime::sys::SysOp::jsonStringify.code() as i128)), Type::int32(), span);
                HExpr::new(H::Builtin(Builtin::Sys, vec![code, recv, indent]), Type::Str, span)
            }
            _ => return None,
        })
    }

    /// Calls a (non-generic) prelude helper function.
    pub fn prelude_call(&mut self, name: &str, args: Vec<HExpr>, span: Span) -> HExpr {
        let Some(fid) = self.prelude_func(name) else {
            self.err(span, format!("internal: prelude function '{}' is missing", name));
            return self.none(span);
        };
        let sig = self.sigs[fid as usize].clone();
        let args: Vec<HExpr> = args.into_iter().zip(sig.params.iter()).map(|(x, p)| self.pass_arg(x, p, span)).collect();
        HExpr::new(H::Call(fid, args), sig.ret, span)
    }

    /// The prelude class `Bytes`.
    pub fn bytes_type(&mut self, span: Span) -> Type {
        let pm = self.prelude_module;
        match self.lookup_type(pm, "Bytes", span) {
            Some(TypeRef::Class(f)) => match self.instantiate_class(&f, Vec::new(), span) {
                Some(c) => Type::Class(c),
                None => Type::Error,
            },
            _ => Type::Error,
        }
    }

    pub fn builtin_method(&mut self, obj: &'a ast::Expr, recv: HExpr, name: &str, args: &'a [ast::Arg], _expected: Option<&Type>, span: Span) -> Option<HExpr> {
        let rty = recv.ty.deref().clone();
        if matches!(rty, Type::Nullable(_) | Type::Null) && !matches!(name, "equals" | "isSameReferenceWith" | "toString" | "castTo") {
            let n = self.tname(&rty);
            self.err(span, format!("cannot call '{}' on nullable type {}; check for null first or use '!'", name, n));
            return Some(self.none(span));
        }
        let what = format!("{}.{}", self.tname(&rty), name);
        let b = |b: Builtin, args: Vec<HExpr>, ty: Type| HExpr::new(H::Builtin(b, args), ty, span);
        let with_recv = |recv: HExpr, mut a: Vec<HExpr>| {
            a.insert(0, recv);
            a
        };
        match &rty {
            Type::Str => {
                let r = match name {
                    "length" => {
                        self.bargs(args, &[], 0, &what, span)?;
                        b(Builtin::StrLength, vec![recv], Type::int64())
                    }
                    "isEmpty" => {
                        self.bargs(args, &[], 0, &what, span)?;
                        b(Builtin::StrIsEmpty, vec![recv], Type::Bool)
                    }
                    "substring" => {
                        let a = self.bargs(args, &[P::Idx, P::Idx], 1, &what, span)?;
                        b(Builtin::StrSubstring, with_recv(recv, a), Type::Str)
                    }
                    "startsWith" | "endsWith" | "contains" => {
                        let a = self.bargs(args, &[P::Borrow(Type::Str)], 1, &what, span)?;
                        let bi = match name {
                            "startsWith" => Builtin::StrStartsWith,
                            "endsWith" => Builtin::StrEndsWith,
                            _ => Builtin::StrContains,
                        };
                        b(bi, with_recv(recv, a), Type::Bool)
                    }
                    "indexOf" => {
                        let a = self.bargs(args, &[P::Borrow(Type::Str)], 1, &what, span)?;
                        b(Builtin::StrIndexOf, with_recv(recv, a), Type::int64())
                    }
                    "toUpperCase" | "toLowerCase" | "trim" => {
                        self.bargs(args, &[], 0, &what, span)?;
                        let bi = match name {
                            "toUpperCase" => Builtin::StrToUpper,
                            "toLowerCase" => Builtin::StrToLower,
                            _ => Builtin::StrTrim,
                        };
                        b(bi, vec![recv], Type::Str)
                    }
                    "replace" => {
                        let a = self.bargs(args, &[P::Borrow(Type::Str), P::Borrow(Type::Str), P::Val(Type::int64()), P::Val(Type::Bool)], 2, &what, span)?;
                        b(Builtin::StrReplace, with_recv(recv, a), Type::Str)
                    }
                    "split" => {
                        let a = self.bargs(args, &[P::Borrow(Type::Str), P::Val(Type::int64())], 1, &what, span)?;
                        b(Builtin::StrSplit, with_recv(recv, a), Type::Array(Box::new(Type::Str)))
                    }
                    "characters" => {
                        self.bargs(args, &[], 0, &what, span)?;
                        b(Builtin::StrCharacters, vec![recv], Type::Array(Box::new(Type::Str)))
                    }
                    "format" | "formatWithInjection" => {
                        let mut a = vec![recv];
                        for x in args {
                            if x.name.is_some() {
                                self.err(x.value.span, "format does not take named arguments");
                            }
                            a.push(self.any_arg(x));
                        }
                        b(if name == "format" { Builtin::StrFormat } else { Builtin::StrFormatInj }, a, Type::Str)
                    }
                    "parse" => {
                        if args.len() != 1 {
                            self.err(span, "parse takes exactly one type argument, e.g. s.parse(Int64)");
                            return Some(self.none(span));
                        }
                        let Some(te) = crate::parser::expr_to_type(&args[0].value) else {
                            self.err(span, "parse expects a type, e.g. s.parse(Int64)");
                            return Some(self.none(span));
                        };
                        let module = self.current_module_pub();
                        let subst = self.cur_ref().subst.clone();
                        let t = self.resolve_type(&te, module, &subst);
                        if !matches!(t, Type::Int(_) | Type::Big | Type::Float(_) | Type::Bool | Type::Str) {
                            let n = self.tname(&t);
                            self.err(span, format!("cannot parse a String as {}", n));
                        }
                        let code = HExpr::new(H::Lit(Lit::Str(rt_type(&t).encode())), Type::Str, span);
                        b(Builtin::StrParse, vec![recv, code], t)
                    }
                    // regular expressions (spec 11.3, 14.15)
                    "matches" | "containsMatch" | "replaceRegex" | "splitRegex" | "findAll" => {
                        use l2_runtime::sys::SysOp;
                        if let Some(a) = args.first() {
                            self.check_regex_literal(&a.value, false);
                        }
                        let (op, n, ret) = match name {
                            "matches" => (SysOp::strMatches, 1, Type::Bool),
                            "containsMatch" => (SysOp::strContainsMatch, 1, Type::Bool),
                            "replaceRegex" => (SysOp::strReplaceRegex, 2, Type::Str),
                            "splitRegex" => (SysOp::strSplitRegex, 1, Type::Array(Box::new(Type::Str))),
                            _ => (SysOp::strFindAllRegex, 1, Type::Array(Box::new(Type::Str))),
                        };
                        let ps: Vec<P> = (0..n).map(|_| P::Borrow(Type::Str)).collect();
                        let a = self.bargs(args, &ps, n, &what, span)?;
                        let mut all = vec![HExpr::new(H::Lit(Lit::Int(op.code() as i128)), Type::int32(), span), recv];
                        all.extend(a);
                        b(Builtin::Sys, all, ret)
                    }
                    // text to Bytes (spec 14.5)
                    "encode" => {
                        let a = self.bargs(args, &[P::Borrow(Type::Str)], 0, &what, span)?;
                        let enc = a.into_iter().next().unwrap_or_else(|| HExpr::new(H::Lit(Lit::Str("utf-8".into())), Type::Str, span));
                        self.prelude_call("__stringEncode", vec![recv, enc], span)
                    }
                    "append" | "prepend" => {
                        let a = self.bargs(args, &[P::Borrow(Type::Dyn)], 1, &what, span)?;
                        let place = self.mut_place(obj, name, span)?;
                        let bi = if name == "append" { Builtin::StrAppend } else { Builtin::StrPrepend };
                        HExpr::new(H::BuiltinMut(bi, Box::new(place), a), Type::Void, span)
                    }
                    // replaced by a random string matching the pattern, of the same length
                    "randomize" => {
                        if let Some(a) = args.first() {
                            self.check_generator_pattern(&a.value);
                        }
                        let a = self.bargs(args, &[P::Borrow(Type::Str)], 1, &what, span)?;
                        let place = self.mut_place(obj, name, span)?;
                        HExpr::new(H::BuiltinMut(Builtin::StrRandomize, Box::new(place), a), Type::Void, span)
                    }
                    _ => return self.generic_method(recv, name, args, span),
                };
                Some(r)
            }
            Type::Int(_) | Type::Float(_) | Type::Big => {
                let r = match name {
                    "format" => {
                        let a = self.bargs(args, &[P::Val(Type::int64()), P::Val(Type::int64())], 2, &what, span)?;
                        b(Builtin::NumFormat, with_recv(recv, a), Type::Str)
                    }
                    // round() / round(decimals) / round(wholeDigits, decimals) (spec 12.2)
                    "round" | "ceil" | "floor" => {
                        let a = self.bargs(args, &[P::Val(Type::int64()), P::Val(Type::int64())], 0, &what, span)?;
                        let bi = match name {
                            "round" => Builtin::NumRound,
                            "ceil" => Builtin::NumCeil,
                            _ => Builtin::NumFloor,
                        };
                        let wrap = HExpr::new(H::Lit(Lit::Bool(self.cur_ref().wrap)), Type::Bool, span);
                        let mut all = vec![recv, wrap];
                        all.extend(a);
                        b(bi, all, rty.clone())
                    }
                    // the number's bytes in its own width: "big" (default) or "little" endian
                    "toBytes" => {
                        let a = self.bargs(args, &[P::Borrow(Type::Str)], 0, &what, span)?;
                        let order = a.into_iter().next().unwrap_or_else(|| HExpr::new(H::Lit(Lit::Str("big".into())), Type::Str, span));
                        self.prelude_call("__numberToBytes", vec![recv, order], span)
                    }
                    "abs" => {
                        self.bargs(args, &[], 0, &what, span)?;
                        let wrap = HExpr::new(H::Lit(Lit::Bool(self.cur_ref().wrap)), Type::Bool, span);
                        b(Builtin::NumAbs, vec![recv, wrap], rty.clone())
                    }
                    "addWrap" | "subWrap" | "mulWrap" => {
                        let a = self.bargs(args, &[P::Val(rty.clone())], 1, &what, span)?;
                        let op = match name {
                            "addWrap" => ArithOp::Add,
                            "subWrap" => ArithOp::Sub,
                            _ => ArithOp::Mul,
                        };
                        HExpr::new(H::Arith(op, Box::new(recv), Box::new(a.into_iter().next().unwrap()), true), rty.clone(), span)
                    }
                    "randomize" => {
                        let a = self.bargs(args, &[P::Val(rty.clone()), P::Val(rty.clone())], 2, &what, span)?;
                        let place = self.mut_place(obj, name, span)?;
                        HExpr::new(H::BuiltinMut(Builtin::NumRandomize, Box::new(place), a), Type::Void, span)
                    }
                    // mathematical functions (spec 12.3)
                    n if l2_runtime::math::MathOp::ALL.iter().any(|o| o.name().trim_end_matches('_') == n) => {
                        use l2_runtime::math::{Kind, MathOp};
                        let Some(op) = MathOp::by_name(n, args.len()) else {
                            let arities: Vec<String> = MathOp::ALL.iter().filter(|o| o.name().trim_end_matches('_') == n).map(|o| o.arity().to_string()).collect();
                            self.err(span, format!("{} takes {} argument(s) but {} were given", what, arities.join(" or "), args.len()));
                            return Some(self.none(span));
                        };
                        let (pty, ret) = match op.kind() {
                            Kind::Float => {
                                let r = if matches!(rty, Type::Float(_)) { rty.clone() } else { Type::Float(FloatTy::F64) };
                                (Type::Float(FloatTy::F64), r)
                            }
                            Kind::Same => (rty.clone(), rty.clone()),
                            Kind::Test => (Type::Float(FloatTy::F64), Type::Bool),
                            Kind::Integer => {
                                if !matches!(rty, Type::Int(_) | Type::Big) {
                                    let tn = self.tname(&rty);
                                    self.err(span, format!("{} is defined for integer types only, not {}", n, tn));
                                    return Some(self.none(span));
                                }
                                (rty.clone(), rty.clone())
                            }
                        };
                        let ps: Vec<P> = (0..op.arity()).map(|_| P::Val(pty.clone())).collect();
                        let a = self.bargs(args, &ps, op.arity(), &what, span)?;
                        let wrap = HExpr::new(H::Lit(Lit::Bool(self.cur_ref().wrap)), Type::Bool, span);
                        let code = HExpr::new(H::Lit(Lit::Int(op.code() as i128)), Type::int32(), span);
                        let mut all = vec![recv, wrap, code];
                        all.extend(a);
                        b(Builtin::NumMath, all, ret)
                    }
                    "compareTo" => {
                        let a = self.bargs(args, &[P::Val(rty.clone())], 1, &what, span)?;
                        let x = a.into_iter().next().unwrap();
                        // (a > b) - (a < b)
                        let gt = HExpr::new(H::Cmp(l2_runtime::ops::CmpOp::Gt, Box::new(recv.clone()), Box::new(x.clone())), Type::Bool, span);
                        let lt = HExpr::new(H::Cmp(l2_runtime::ops::CmpOp::Lt, Box::new(recv), Box::new(x)), Type::Bool, span);
                        let one = HExpr::new(H::Lit(Lit::Int(1)), Type::int32(), span);
                        let m1 = HExpr::new(H::Lit(Lit::Int(-1)), Type::int32(), span);
                        let zero = HExpr::new(H::Lit(Lit::Int(0)), Type::int32(), span);
                        let inner = HExpr::new(H::Ternary(Box::new(lt), Box::new(m1), Box::new(zero)), Type::int32(), span);
                        HExpr::new(H::Ternary(Box::new(gt), Box::new(one), Box::new(inner)), Type::int32(), span)
                    }
                    _ => return self.generic_method(recv, name, args, span),
                };
                Some(r)
            }
            Type::Array(et) => {
                let et = (**et).clone();
                let r = match name {
                    "length" => {
                        self.bargs(args, &[], 0, &what, span)?;
                        b(Builtin::ArrLength, vec![recv], Type::int64())
                    }
                    "isEmpty" => {
                        self.bargs(args, &[], 0, &what, span)?;
                        b(Builtin::ArrIsEmpty, vec![recv], Type::Bool)
                    }
                    "at" => {
                        let a = self.bargs(args, &[P::Idx], 1, &what, span)?;
                        b(Builtin::ArrAt, with_recv(recv, a), et)
                    }
                    "first" | "last" => {
                        let a = self.bargs(args, &[P::Idx], 0, &what, span)?;
                        b(if name == "first" { Builtin::ArrFirst } else { Builtin::ArrLast }, with_recv(recv, a), et)
                    }
                    "kv" => {
                        self.bargs(args, &[], 0, &what, span)?;
                        b(Builtin::ArrKv, vec![recv], Type::Array(Box::new(Type::Tuple(vec![Type::int64(), et]))))
                    }
                    "contains" => {
                        let a = self.bargs(args, &[P::Borrow(et.clone())], 1, &what, span)?;
                        b(Builtin::ArrContains, with_recv(recv, a), Type::Bool)
                    }
                    "set" | "insert" => {
                        let a = self.bargs(args, &[P::Idx, P::Val(et.clone())], 2, &what, span)?;
                        let place = self.mut_place(obj, name, span)?;
                        let bi = if name == "set" { Builtin::ArrSet } else { Builtin::ArrInsert };
                        HExpr::new(H::BuiltinMut(bi, Box::new(place), a), Type::Void, span)
                    }
                    "remove" => {
                        let a = self.bargs(args, &[P::Idx], 1, &what, span)?;
                        let place = self.mut_place(obj, name, span)?;
                        HExpr::new(H::BuiltinMut(Builtin::ArrRemove, Box::new(place), a), et, span)
                    }
                    "fill" => {
                        let mut a = Vec::new();
                        for x in args {
                            let v = self.expr(&x.value, Some(&et));
                            let v = self.coerce(v, &et, x.value.span);
                            a.push(self.consume(v));
                        }
                        if a.is_empty() {
                            self.err(span, "fill needs at least one value");
                        }
                        let place = self.mut_place(obj, name, span)?;
                        HExpr::new(H::BuiltinMut(Builtin::ArrFill, Box::new(place), a), Type::Void, span)
                    }
                    "reverse" | "sort" | "shuffle" => {
                        self.bargs(args, &[], 0, &what, span)?;
                        if name == "sort" {
                            let ok = match &et {
                                Type::Int(_) | Type::Float(_) | Type::Big | Type::Str | Type::Bool => true,
                                Type::Class(c) => self.classes[*c as usize].compare_fn.is_some(),
                                _ => false,
                            };
                            if !ok {
                                let n = self.tname(&et);
                                self.err(span, format!("sort() needs Comparable elements, found {}", n));
                            }
                        }
                        let place = self.mut_place(obj, name, span)?;
                        let bi = match name {
                            "reverse" => Builtin::ArrReverse,
                            "sort" => Builtin::ArrSort,
                            _ => Builtin::ArrShuffle,
                        };
                        HExpr::new(H::BuiltinMut(bi, Box::new(place), vec![]), Type::Void, span)
                    }
                    n if super::arrays::ARRAY_METHODS.contains(&n) => return self.array_method(obj, recv, et, name, args, span),
                    _ => return self.generic_method(recv, name, args, span),
                };
                Some(r)
            }
            Type::Dict(kt, vt) => {
                let (kt, vt) = ((**kt).clone(), (**vt).clone());
                let r = match name {
                    "kv" => {
                        self.bargs(args, &[], 0, &what, span)?;
                        b(Builtin::DictKv, vec![recv], Type::Array(Box::new(Type::Tuple(vec![kt, vt]))))
                    }
                    "length" => {
                        self.bargs(args, &[], 0, &what, span)?;
                        b(Builtin::DictLength, vec![recv], Type::int64())
                    }
                    "isEmpty" => {
                        self.bargs(args, &[], 0, &what, span)?;
                        b(Builtin::DictIsEmpty, vec![recv], Type::Bool)
                    }
                    "containsKey" => {
                        let a = self.bargs(args, &[P::Borrow(kt.clone())], 1, &what, span)?;
                        b(Builtin::DictContainsKey, with_recv(recv, a), Type::Bool)
                    }
                    "keys" => {
                        self.bargs(args, &[], 0, &what, span)?;
                        b(Builtin::DictKeys, vec![recv], Type::Array(Box::new(kt)))
                    }
                    "values" => {
                        self.bargs(args, &[], 0, &what, span)?;
                        b(Builtin::DictValues, vec![recv], Type::Array(Box::new(vt)))
                    }
                    "get" => {
                        let a = self.bargs(args, &[P::Borrow(kt.clone())], 1, &what, span)?;
                        b(Builtin::DictGet, with_recv(recv, a), vt)
                    }
                    "set" => {
                        let a = self.bargs(args, &[P::Val(kt.clone()), P::Val(vt.clone())], 2, &what, span)?;
                        let place = self.mut_place(obj, name, span)?;
                        HExpr::new(H::BuiltinMut(Builtin::DictSet, Box::new(place), a), Type::Void, span)
                    }
                    "merge" => {
                        let a = self.bargs(args, &[P::Borrow(rty.clone())], 1, &what, span)?;
                        let place = self.mut_place(obj, name, span)?;
                        HExpr::new(H::BuiltinMut(Builtin::DictMerge, Box::new(place), a), Type::Void, span)
                    }
                    "remove" => {
                        let a = self.bargs(args, &[P::Borrow(kt.clone())], 1, &what, span)?;
                        let place = self.mut_place(obj, name, span)?;
                        HExpr::new(H::BuiltinMut(Builtin::DictRemove, Box::new(place), a), vt, span)
                    }
                    _ => return self.generic_method(recv, name, args, span),
                };
                Some(r)
            }
            Type::Class(c) => {
                if name == "drop" {
                    self.err(span, "drop() is called automatically and cannot be called directly");
                    return Some(self.none(span));
                }
                let _ = c;
                self.generic_method(recv, name, args, span)
            }
            _ => self.generic_method(recv, name, args, span),
        }
    }

    pub fn builtin_static_const(&mut self, tn: &str, name: &str, span: Span) -> Option<HExpr> {
        if let Some(t) = int_type_by_name(tn) {
            let v = match name {
                "MAX" => t.max(),
                "MIN" => t.min(),
                _ => return None,
            };
            return Some(HExpr::new(H::Lit(Lit::Int(v)), Type::Int(t), span));
        }
        if let Some(f) = float_type_by_name(tn) {
            let max = match f {
                FloatTy::F16 => 65504.0,
                FloatTy::F32 => f32::MAX as f64,
                FloatTy::F64 => f64::MAX,
            };
            let v = match name {
                "MAX" => max,
                "MIN" => -max,
                _ => return None,
            };
            return Some(HExpr::new(H::Lit(Lit::Float(v)), Type::Float(f), span));
        }
        None
    }

    pub fn array_new(&mut self, et: Type, args: &'a [ast::Arg], span: Span) -> HExpr {
        let arr_ty = Type::Array(Box::new(et.clone()));
        let named = args.iter().any(|a| a.name.is_some());
        if named {
            let ok = args.iter().zip(["length", "length_immutable"].iter()).all(|(a, n)| a.name.as_deref() == Some(*n));
            if !ok || args.iter().any(|a| a.name.is_none()) {
                self.err(span, "array() parameters are (length, length_immutable); named arguments must match exactly and in order");
                return HExpr::new(H::Lit(Lit::Null), arr_ty, span);
            }
        }
        if args.len() > 2 {
            self.err(span, "array() takes at most 2 arguments (length, length_immutable)");
            return HExpr::new(H::Lit(Lit::Null), arr_ty, span);
        }
        let mut out = Vec::new();
        let def = if self.has_default(&et) {
            self.default_value_expr(&et, span)
        } else {
            if !args.is_empty() {
                let n = self.tname(&et);
                self.err(span, format!("elements of type {} have no default value; create an empty growable array with array() and insert elements", n));
            }
            HExpr::new(H::Lit(Lit::Null), Type::Null, span)
        };
        out.push(def);
        if let Some(a) = args.first() {
            let l = self.expr(&a.value, Some(&Type::int64()));
            let l = self.coerce(l, &Type::int64(), a.value.span);
            out.push(l);
        }
        if let Some(a) = args.get(1) {
            let f = self.expr(&a.value, Some(&Type::Bool));
            let f = self.coerce(f, &Type::Bool, a.value.span);
            out.push(f);
        }
        HExpr::new(H::Builtin(Builtin::ArrNew, out), arr_ty, span)
    }

    pub fn builtin_static_call(&mut self, tn: &str, name: &str, args: &'a [ast::Arg], _expected: Option<&Type>, span: Span) -> HExpr {
        let module = self.current_module_pub();
        let subst = self.cur_ref().subst.clone();
        let ty = self.resolve_type(&TypeExpr::named(tn, span), module, &subst);
        let what = format!("{}.{}", tn, name);
        match name {
            "array" => return self.array_new(ty, args, span),
            "max" | "min" if matches!(tn, "Int" | "Int32") => {
                if args.is_empty() {
                    self.err(span, format!("{} needs at least one argument", what));
                    return self.none(span);
                }
                let mut xs = Vec::new();
                for a in args {
                    xs.push(self.expr(&a.value, None));
                }
                let mut ct = xs[0].ty.clone();
                for x in &xs[1..] {
                    match common_numeric(&ct, &x.ty) {
                        Some(t) => ct = t,
                        None => {
                            let n = self.tname(&x.ty);
                            self.err(x.span, format!("{} accepts numbers only, found {}", what, n));
                        }
                    }
                }
                if !ct.is_numeric() && !ct.is_error() {
                    let n = self.tname(&ct);
                    self.err(span, format!("{} accepts numbers only, found {}", what, n));
                }
                let xs: Vec<HExpr> = xs.into_iter().map(|x| self.coerce(x, &ct, span)).collect();
                return HExpr::new(H::Builtin(if name == "max" { Builtin::NumMax } else { Builtin::NumMin }, xs), ct, span);
            }
            "random" if ty.is_numeric() => {
                let Some(a) = self.bargs(args, &[P::Val(ty.clone()), P::Val(ty.clone())], 2, &what, span) else {
                    return self.none(span);
                };
                return HExpr::new(H::Builtin(Builtin::NumRandom, a), ty, span);
            }
            "range" if ty.is_numeric() => {
                let Some(a) = self.bargs(args, &[P::Val(ty.clone()), P::Val(ty.clone()), P::Val(ty.clone())], 2, &what, span) else {
                    return self.none(span);
                };
                return HExpr::new(H::Builtin(Builtin::NumRange, a), Type::Array(Box::new(ty)), span);
            }
            "fromBytes" if ty.is_numeric() => {
                let bt = self.bytes_type(span);
                let Some(a) = self.bargs(args, &[P::Borrow(Type::Ref(false, Box::new(bt))), P::Borrow(Type::Str)], 1, &what, span) else {
                    return self.none(span);
                };
                let mut it = a.into_iter();
                let b = it.next().unwrap();
                let order = it.next().unwrap_or_else(|| HExpr::new(H::Lit(Lit::Str("big".into())), Type::Str, span));
                let code = HExpr::new(H::Lit(Lit::Str(rt_type(&ty).encode())), Type::Str, span);
                let v = self.prelude_call("__numberFromBytes", vec![b, code, order], span);
                let wrap = self.cur_ref().wrap;
                return HExpr::new(H::Cast(Box::new(v), wrap), ty, span);
            }
            "random" if ty == Type::Str => {
                if args.len() == 2 {
                    self.err(span, "String.random takes (pattern) or (pattern, minLength, maxLength)");
                    return self.none(span);
                }
                if let Some(a) = args.first() {
                    self.check_generator_pattern(&a.value);
                }
                let Some(a) = self.bargs(args, &[P::Borrow(Type::Str), P::Val(Type::int64()), P::Val(Type::int64())], 1, &what, span) else {
                    return self.none(span);
                };
                return HExpr::new(H::Builtin(Builtin::StrRandom, a), Type::Str, span);
            }
            _ => {}
        }
        self.err(span, format!("'{}' has no static method '{}'", tn, name));
        self.none(span)
    }

    /// Compile-time check of a literal pattern for String.random / randomize.
    fn check_generator_pattern(&mut self, e: &ast::Expr) {
        if let A::Str(p) = &e.kind {
            if let Err(msg) = l2_runtime::regex_gen::check(p) {
                self.err(e.span, msg);
            }
        }
    }

    /// Compile-time check of a literal regular expression.
    pub fn check_regex_literal(&mut self, e: &ast::Expr, fancy: bool) {
        if let A::Str(p) = &e.kind {
            if let Err(msg) = l2_runtime::sys::regex::check_pattern(p, "", fancy) {
                self.err(e.span, msg);
            }
        }
    }

    pub fn stdio_call(&mut self, name: &str, args: &'a [ast::Arg], span: Span) -> HExpr {
        let void = Type::Void;
        match name {
            "println" | "print" => {
                if args.len() != 1 {
                    if name == "println" && args.is_empty() {
                        let e = HExpr::new(H::Lit(Lit::Str(String::new())), Type::Str, span);
                        return HExpr::new(H::Builtin(Builtin::Println, vec![e]), void, span);
                    }
                    self.err(span, format!("stdio.{} takes one argument", name));
                    return self.none(span);
                }
                let x = self.any_arg(&args[0]);
                HExpr::new(H::Builtin(if name == "println" { Builtin::Println } else { Builtin::Print }, vec![x]), void, span)
            }
            "read" => {
                let Some(a) = self.bargs(args, &[P::Borrow(Type::Str)], 0, "stdio.read", span) else {
                    return self.none(span);
                };
                HExpr::new(H::Builtin(Builtin::Read, a), Type::Str, span)
            }
            "replaceLine" => {
                let named_ok = args.len() == 2 && args[1].name.as_deref() == Some("lines_from_last") && args[0].name.as_deref().map_or(true, |n| n == "s");
                if args.len() == 2 && args[1].name.is_some() && !named_ok {
                    self.err(span, "stdio.replaceLine parameters are (s, lines_from_last)");
                    return self.none(span);
                }
                if args.is_empty() || args.len() > 2 {
                    self.err(span, "stdio.replaceLine takes 1 or 2 arguments");
                    return self.none(span);
                }
                let s = self.any_arg(&args[0]);
                let mut out = vec![s];
                if let Some(a) = args.get(1) {
                    let n = self.expr(&a.value, Some(&Type::int64()));
                    out.push(self.coerce(n, &Type::int64(), a.value.span));
                }
                HExpr::new(H::Builtin(Builtin::ReplaceLine, out), void, span)
            }
            _ => {
                self.err(span, format!("stdio has no function '{}' (available: println, print, read, replaceLine)", name));
                self.none(span)
            }
        }
    }

    /// `"...%a%...".format(x)` / `f"...".format(x)`: only placeholders that appear in the literal
    /// text are substituted, so interpolated values can never inject placeholders (spec 11.1).
    pub fn literal_format(&mut self, obj: &'a ast::Expr, args: &'a [ast::Arg], span: Span) -> HExpr {
        let mut segs: Vec<Result<String, &'a ast::FStrPart>> = Vec::new();
        match &obj.kind {
            A::Str(s) => segs.push(Ok(s.clone())),
            A::FStr(parts) => {
                for p in parts {
                    match p {
                        ast::FStrPart::Lit(s) => segs.push(Ok(s.clone())),
                        other => segs.push(Err(other)),
                    }
                }
            }
            _ => unreachable!(),
        }
        let total: usize = segs.iter().map(|s| if let Ok(t) = s { l2_runtime::builtins::find_placeholders_spec(t).len() } else { 0 }).sum();
        if total != args.len() {
            self.err(span, format!("format string has {} placeholder(s) but {} argument(s) were given", total, args.len()));
        }
        let mut argv: Vec<HExpr> = args.iter().map(|a| self.any_arg(a)).collect::<Vec<_>>();
        argv.reverse();
        let mut out = Vec::new();
        for s in segs {
            match s {
                Ok(text) => {
                    let ph = l2_runtime::builtins::find_placeholders_spec(&text);
                    let mut last = 0;
                    for (a, b, spec) in ph {
                        out.push(HExpr::new(H::Lit(Lit::Str(text[last..a].to_string())), Type::Str, span));
                        match (argv.pop(), spec) {
                            (Some(x), None) => out.push(x),
                            (Some(x), Some(spec)) => {
                                // `%name:spec%`: Python's format specification (spec 11.1)
                                self.check_format_spec(&spec, &x.ty, false, span);
                                let sp = HExpr::new(H::Lit(Lit::Str(spec)), Type::Str, span);
                                let conv = HExpr::new(H::Lit(Lit::Str(String::new())), Type::Str, span);
                                out.push(HExpr::new(H::Builtin(Builtin::FormatValue, vec![x, sp, conv]), Type::Str, span));
                            }
                            (None, _) => out.push(HExpr::new(H::Lit(Lit::Str(text[a..b].to_string())), Type::Str, span)),
                        }
                        last = b;
                    }
                    out.push(HExpr::new(H::Lit(Lit::Str(text[last..].to_string())), Type::Str, span));
                }
                Err(p) => {
                    let xs = self.fstr_part(p, span);
                    out.extend(xs);
                }
            }
        }
        let _ = IntTy::I32;
        HExpr::new(H::Concat(out), Type::Str, span)
    }
}
