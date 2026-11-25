//! End of a body: literal defaults, final types, operator checks, literal
//! ranges, trait obligations and pattern exhaustiveness.

use crate::defs::DefId;
use crate::hir::{self, Block, Expr, ExprKind, Pat, PatKind, Stmt};
use crate::infer::{InferTable, VarKind};
use crate::ty::{IntTy, Ty};
use crate::typeck::{Cx, OpCheck};
use pegasus_parser::ast::Span;

impl<'s, 'a> Cx<'s, 'a> {
    pub(crate) fn finish(&mut self, body: &mut hir::Body) {
        // Shift amounts and integer exponents default to u32, not i32: they
        // must be unsigned (Phase 0 spec, section 7).
        for d in &self.deferred {
            if matches!(d.kind, OpCheck::Shift | OpCheck::Pow) {
                if let Ty::Infer(v) = self.tab.shallow(&d.b) {
                    if self.tab.kind(v) == Some(VarKind::Int) {
                        let _ = self.tab.unify(&d.b, &Ty::Int(IntTy::U32));
                    }
                }
            }
        }
        let mut un: Option<Span> = None;
        let tab = std::mem::take(&mut self.tab);
        zonk_expr(&tab, &mut body.value, &mut un);
        for e in body.requires.iter_mut().chain(body.ensures.iter_mut()) {
            zonk_expr(&tab, e, &mut un);
        }
        let mut locals = std::mem::take(&mut self.locals);
        for l in locals.iter_mut() {
            let mut u = false;
            l.ty = tab.finish(&l.ty, &mut u);
            if u && un.is_none() {
                un = Some(l.span);
            }
        }
        body.locals = locals;
        if let Some(sp) = un {
            self.err(
                "T050",
                sp,
                "type annotations needed: cannot infer a type here".into(),
            );
        }
        let fin = |t: &Ty| {
            let mut u = false;
            tab.finish(t, &mut u)
        };
        let deferred = std::mem::take(&mut self.deferred);
        for d in deferred {
            let a = fin(&d.a);
            let b = fin(&d.b);
            if a.has_error() || b.has_error() {
                continue;
            }
            if let Some(msg) = op_problem(d.kind, &d.op, &a, &b) {
                let code = if d.kind == OpCheck::Not {
                    "T092"
                } else {
                    "T003"
                };
                let full = msg
                    .replace("{a}", &self.s.ty_str(&a))
                    .replace("{b}", &self.s.ty_str(&b));
                self.err(code, d.span, full);
            }
        }
        for (t, v, neg, span) in std::mem::take(&mut self.int_lits) {
            if let Ty::Int(it) = fin(&t) {
                let ok = if neg {
                    it.signed() && v <= it.min().unsigned_abs()
                } else {
                    v <= it.max()
                };
                if !ok && !(neg && !it.signed()) {
                    let sign = if neg { "-" } else { "" };
                    self.err(
                        "T091",
                        span,
                        format!("literal `{sign}{v}` is out of range for `{}`", it.name()),
                    );
                }
            }
        }
        for (ty, tr, args, span) in std::mem::take(&mut self.obligations) {
            let ty = fin(&ty);
            let args: Vec<Ty> = args.iter().map(&fin).collect();
            if ty.has_error() || args.iter().any(|a| a.has_error()) {
                continue;
            }
            if !self.implements(&ty, tr, &args, 0) {
                let t = self.s.ty_str(&ty);
                let n = self.s.def(tr).name.clone();
                self.err(
                    "T064",
                    span,
                    format!("type `{t}` does not implement trait `{n}`"),
                );
            }
        }
        self.tab = tab;
        crate::exhaust::check_body(self.s, self.file, body);
    }

    /// Does `ty` implement `tr<args>`? Types are fully resolved.
    pub(crate) fn implements(&mut self, ty: &Ty, tr: DefId, args: &[Ty], depth: u32) -> bool {
        let bounds = self.bounds.clone();
        self.s.implements(&bounds, ty, tr, args, depth)
    }
}

fn op_problem(kind: OpCheck, op: &str, a: &Ty, b: &Ty) -> Option<String> {
    let int = |t: &Ty| matches!(t, Ty::Int(_));
    let uint = |t: &Ty| matches!(t, Ty::Int(i) if !i.signed());
    let float = |t: &Ty| matches!(t, Ty::Float(_));
    let ok = match kind {
        OpCheck::Arith => int(a) || float(a),
        OpCheck::WrapSat => int(a),
        OpCheck::Bitwise => int(a) || *a == Ty::Bool,
        OpCheck::Shift => {
            if !int(a) {
                false
            } else if !uint(b) {
                return Some(format!("shift amounts must have an unsigned integer type, found `{{b}}` (operator `{op}`)"));
            } else {
                true
            }
        }
        OpCheck::Pow => {
            if int(a) {
                if !uint(b) {
                    return Some(
                        "integer `**` needs an unsigned integer exponent, found `{b}`".into(),
                    );
                }
                true
            } else if float(a) {
                int(b) || b == a
            } else {
                false
            }
        }
        OpCheck::Eq => equatable(a),
        OpCheck::Ord => int(a) || float(a) || matches!(a, Ty::Char | Ty::Str),
        OpCheck::Neg => matches!(a, Ty::Int(i) if i.signed()) || float(a),
        OpCheck::Plus => int(a) || float(a),
        OpCheck::BitNot => int(a),
        OpCheck::Not => *a == Ty::Bool,
    };
    if ok {
        return None;
    }
    Some(match kind {
        OpCheck::Neg if uint(a) => "cannot negate a value of unsigned type `{a}`".into(),
        OpCheck::Not if int(a) => "`!` is logical NOT and needs `bool`, found `{a}` (use `~` for bitwise NOT)".into(),
        OpCheck::Eq | OpCheck::Ord if matches!(a, Ty::Adt(..)) => format!(
            "operator `{op}` is not defined for `{{a}}` (comparison traits come with the standard library)"
        ),
        _ => format!("operator `{op}` cannot be applied to `{{a}}`"),
    })
}

fn equatable(t: &Ty) -> bool {
    match t {
        Ty::Int(_) | Ty::Float(_) | Ty::Bool | Ty::Char | Ty::Str => true,
        Ty::Tuple(v) => v.iter().all(equatable),
        Ty::Array(e, _) => equatable(e),
        _ => false,
    }
}

fn zonk_ty(tab: &InferTable, t: &mut Ty, un: &mut Option<Span>, span: Span) {
    let mut u = false;
    *t = tab.finish(t, &mut u);
    if u && un.is_none() {
        *un = Some(span);
    }
}

fn zonk_pat(tab: &InferTable, p: &mut Pat, un: &mut Option<Span>) {
    zonk_ty(tab, &mut p.ty, un, p.span);
    match &mut p.kind {
        PatKind::Tuple(v) | PatKind::Or(v) => v.iter_mut().for_each(|x| zonk_pat(tab, x, un)),
        PatKind::Variant { fields, .. } | PatKind::Struct { fields, .. } => {
            fields.iter_mut().for_each(|x| zonk_pat(tab, x, un))
        }
        PatKind::Array { before, after, .. } => before
            .iter_mut()
            .chain(after.iter_mut())
            .for_each(|x| zonk_pat(tab, x, un)),
        _ => {}
    }
}

fn zonk_block(tab: &InferTable, b: &mut Block, un: &mut Option<Span>) {
    for s in b.stmts.iter_mut() {
        match s {
            Stmt::Let { pat, init } => {
                zonk_pat(tab, pat, un);
                zonk_expr(tab, init, un);
            }
            Stmt::Assign { place, value, .. } => {
                zonk_expr(tab, place, un);
                zonk_expr(tab, value, un);
            }
            Stmt::Expr(e) | Stmt::Defer(e) => zonk_expr(tab, e, un),
            Stmt::Region { body, .. } => zonk_block(tab, body, un),
        }
    }
    if let Some(t) = &mut b.tail {
        zonk_expr(tab, t, un);
    }
}

pub(crate) fn zonk_expr(tab: &InferTable, e: &mut Expr, un: &mut Option<Span>) {
    zonk_ty(tab, &mut e.ty, un, e.span);
    let sp = e.span;
    match &mut e.kind {
        ExprKind::FnRef(_, args) => args.iter_mut().for_each(|t| zonk_ty(tab, t, un, sp)),
        ExprKind::Call { callee, args } => {
            zonk_expr(tab, callee, un);
            args.iter_mut().for_each(|x| zonk_expr(tab, x, un));
        }
        ExprKind::Variant { fields, .. } | ExprKind::Tuple(fields) | ExprKind::Array(fields) => {
            fields.iter_mut().for_each(|x| zonk_expr(tab, x, un))
        }
        ExprKind::Struct { fields, base, .. } => {
            fields.iter_mut().for_each(|(_, x)| zonk_expr(tab, x, un));
            if let Some(b) = base {
                zonk_expr(tab, b, un);
            }
        }
        ExprKind::Field { recv, .. } | ExprKind::TupleField { recv, .. } => {
            zonk_expr(tab, recv, un)
        }
        ExprKind::Index { recv, index } => {
            zonk_expr(tab, recv, un);
            zonk_expr(tab, index, un);
        }
        ExprKind::Unary { expr, .. } | ExprKind::Cast(expr) | ExprKind::Try { expr, .. } => {
            zonk_expr(tab, expr, un)
        }
        ExprKind::Binary { lhs, rhs, .. } => {
            zonk_expr(tab, lhs, un);
            zonk_expr(tab, rhs, un);
        }
        ExprKind::Repeat { value, .. } => zonk_expr(tab, value, un),
        ExprKind::Block(b) | ExprKind::Loop(b) | ExprKind::Unsafe { body: b, .. } => {
            zonk_block(tab, b, un)
        }
        ExprKind::If {
            cond,
            then,
            otherwise,
        } => {
            zonk_expr(tab, cond, un);
            zonk_block(tab, then, un);
            if let Some(o) = otherwise {
                zonk_expr(tab, o, un);
            }
        }
        ExprKind::IfLet {
            pat,
            value,
            then,
            otherwise,
        } => {
            zonk_pat(tab, pat, un);
            zonk_expr(tab, value, un);
            zonk_block(tab, then, un);
            if let Some(o) = otherwise {
                zonk_expr(tab, o, un);
            }
        }
        ExprKind::Match { scrutinee, arms } => {
            zonk_expr(tab, scrutinee, un);
            for a in arms {
                zonk_pat(tab, &mut a.pat, un);
                if let Some(g) = &mut a.guard {
                    zonk_expr(tab, g, un);
                }
                zonk_expr(tab, &mut a.body, un);
            }
        }
        ExprKind::While { cond, body } => {
            zonk_expr(tab, cond, un);
            zonk_block(tab, body, un);
        }
        ExprKind::WhileLet { pat, value, body } => {
            zonk_pat(tab, pat, un);
            zonk_expr(tab, value, un);
            zonk_block(tab, body, un);
        }
        ExprKind::ForRange {
            pat, lo, hi, body, ..
        } => {
            zonk_pat(tab, pat, un);
            zonk_expr(tab, lo, un);
            zonk_expr(tab, hi, un);
            zonk_block(tab, body, un);
        }
        ExprKind::ForArray { pat, array, body } => {
            zonk_pat(tab, pat, un);
            zonk_expr(tab, array, un);
            zonk_block(tab, body, un);
        }
        ExprKind::Break(v) | ExprKind::Return(v) => {
            if let Some(v) = v {
                zonk_expr(tab, v, un);
            }
        }
        ExprKind::Closure { params, body, .. } => {
            params.iter_mut().for_each(|p| zonk_pat(tab, p, un));
            zonk_expr(tab, body, un);
        }
        ExprKind::Lit(_)
        | ExprKind::MoveLocal(_)
        | ExprKind::MovePath { .. }
        | ExprKind::Local(_)
        | ExprKind::Const(_)
        | ExprKind::Continue
        | ExprKind::Error => {}
    }
}
