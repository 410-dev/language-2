//! Generic traversal helpers over HIR.

use crate::hir::*;

pub fn walk_place(p: &Place, f: &mut dyn FnMut(&Expr)) {
    match p {
        Place::Field(e, _) => walk_expr(e, f),
        Place::Elem(b, k) => {
            walk_place(b, f);
            walk_expr(k, f);
        }
        _ => {}
    }
}

/// Visits every expression (pre-order), including expressions nested in statements.
pub fn walk_expr(e: &Expr, f: &mut dyn FnMut(&Expr)) {
    f(e);
    match &e.kind {
        ExprKind::Lit(_) | ExprKind::Local(_) | ExprKind::Move(_) | ExprKind::Global(_) | ExprKind::FuncRef(_) => {}
        ExprKind::Deref(a)
        | ExprKind::Field(a, _)
        | ExprKind::TupleGet(a, _)
        | ExprKind::Unary(_, a)
        | ExprKind::NonNull(a)
        | ExprKind::Unwrap(a)
        | ExprKind::Convert(a)
        | ExprKind::Cast(a, _) => walk_expr(a, f),
        ExprKind::Arith(_, a, b, _)
        | ExprKind::Cmp(_, a, b)
        | ExprKind::And(a, b)
        | ExprKind::Or(a, b)
        | ExprKind::Coalesce(a, b) => {
            walk_expr(a, f);
            walk_expr(b, f);
        }
        ExprKind::Ternary(a, b, c) => {
            walk_expr(a, f);
            walk_expr(b, f);
            walk_expr(c, f);
        }
        ExprKind::Concat(xs) | ExprKind::Call(_, xs) | ExprKind::CallVirtual(_, xs) | ExprKind::New(_, _, xs) | ExprKind::Builtin(_, xs) | ExprKind::Tuple(xs) | ExprKind::ArrayLit(xs) => {
            xs.iter().for_each(|x| walk_expr(x, f))
        }
        ExprKind::CallClosure(c, xs) => {
            walk_expr(c, f);
            xs.iter().for_each(|x| walk_expr(x, f));
        }
        ExprKind::BuiltinMut(_, p, xs) => {
            walk_place(p, f);
            xs.iter().for_each(|x| walk_expr(x, f));
        }
        ExprKind::Lambda(_, caps) => {
            for c in caps {
                match c {
                    CaptureSrc::Ref(p) => walk_place(p, f),
                    CaptureSrc::Value(e) => walk_expr(e, f),
                }
            }
        }
        ExprKind::Dict(kv) => {
            for (k, v) in kv {
                walk_expr(k, f);
                walk_expr(v, f);
            }
        }
        ExprKind::RefMut(p) => walk_place(p, f),
        ExprKind::Seq(stmts, e) => {
            walk_stmts(stmts, &mut |s| walk_stmt_exprs(s, f));
            walk_expr(e, f);
        }
    }
}

/// Visits the expressions directly owned by a statement (not nested statements).
pub fn walk_stmt_exprs(s: &Stmt, f: &mut dyn FnMut(&Expr)) {
    match &s.kind {
        StmtKind::Let(_, Some(e)) | StmtKind::Expr(e) | StmtKind::Throw(e) | StmtKind::Return(Some(e)) | StmtKind::Free(e) => walk_expr(e, f),
        StmtKind::Assign(p, e) => {
            walk_place(p, f);
            walk_expr(e, f);
        }
        StmtKind::If(c, _, _) => walk_expr(c, f),
        StmtKind::Loop { cond: Some(c), .. } => walk_expr(c, f),
        StmtKind::Switch { cases } => {
            for c in cases {
                if let Some(e) = &c.cond {
                    walk_expr(e, f);
                }
            }
        }
        _ => {}
    }
}

/// Visits every statement recursively (pre-order).
pub fn walk_stmts(stmts: &[Stmt], f: &mut dyn FnMut(&Stmt)) {
    for s in stmts {
        f(s);
        match &s.kind {
            StmtKind::If(_, a, b) => {
                walk_stmts(a, f);
                walk_stmts(b, f);
            }
            StmtKind::Loop { body, step, .. } => {
                walk_stmts(body, f);
                walk_stmts(step, f);
            }
            StmtKind::Try { body, catches, finally } => {
                walk_stmts(body, f);
                for c in catches {
                    walk_stmts(&c.body, f);
                }
                if let Some(fb) = finally {
                    walk_stmts(fb, f);
                }
            }
            StmtKind::Block { body, .. } => walk_stmts(body, f),
            StmtKind::Switch { cases } => {
                for c in cases {
                    walk_stmts(&c.body, f);
                }
            }
            _ => {}
        }
    }
}

pub fn walk_stmts_mut(stmts: &mut [Stmt], f: &mut dyn FnMut(&mut Stmt)) {
    for s in stmts {
        f(s);
        match &mut s.kind {
            StmtKind::If(_, a, b) => {
                walk_stmts_mut(a, f);
                walk_stmts_mut(b, f);
            }
            StmtKind::Loop { body, step, .. } => {
                walk_stmts_mut(body, f);
                walk_stmts_mut(step, f);
            }
            StmtKind::Try { body, catches, finally } => {
                walk_stmts_mut(body, f);
                for c in catches {
                    walk_stmts_mut(&mut c.body, f);
                }
                if let Some(fb) = finally {
                    walk_stmts_mut(fb, f);
                }
            }
            StmtKind::Block { body, .. } => walk_stmts_mut(body, f),
            StmtKind::Switch { cases } => {
                for c in cases {
                    walk_stmts_mut(&mut c.body, f);
                }
            }
            _ => {}
        }
    }
}
