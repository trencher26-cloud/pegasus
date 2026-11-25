//! Type checking of function bodies, producing typed HIR
//! (spec/semantics.md, sections 4-6).

use crate::defs::{DefId, DefKind};
use crate::hir::{self, Block, Expr, ExprKind, LocalId, Pat, PatKind, Stmt};
use crate::infer::{InferTable, VarKind};
use crate::items::{Bound, FnSigTy, Mode, Sema, TyScope};
use crate::source::FileId;
use crate::ty::Ty;
use pegasus_parser::ast::{self, ExprKind as AE, ItemKind, Span};
use std::collections::HashMap;

pub(crate) struct LoopCx {
    /// `loop`: the type of its break values. `while`/`for`: None.
    pub(crate) break_ty: Option<Ty>,
    pub(crate) has_break: bool,
}

struct ClosureFrame {
    first_local: LocalId,
    captures: Vec<LocalId>,
    ret: Ty,
    loops: Vec<LoopCx>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum OpCheck {
    Arith,
    WrapSat,
    Bitwise,
    Shift,
    Pow,
    Eq,
    Ord,
    Neg,
    Plus,
    Not,
    BitNot,
}

pub(crate) struct Deferred {
    pub(crate) kind: OpCheck,
    pub(crate) op: String,
    pub(crate) a: Ty,
    pub(crate) b: Ty,
    pub(crate) span: Span,
}

/// A resolved path (a prefix of a dotted expression).
pub(crate) enum PathRes {
    Module(DefId),
    Type(Ty),
    Item(DefId),
    Variant(DefId, Vec<Ty>),
    /// An associated function of a type, with the impl or trait arguments.
    Assoc(DefId, Vec<Ty>),
    Err,
}

pub struct Cx<'s, 'a> {
    pub(crate) s: &'s mut Sema<'a>,
    pub(crate) file: FileId,
    pub(crate) scope: TyScope,
    pub(crate) bounds: Vec<Bound>,
    pub(crate) tab: InferTable,
    pub(crate) locals: Vec<hir::Local>,
    scopes: Vec<Vec<(String, LocalId)>>,
    ret: Ty,
    loops: Vec<LoopCx>,
    closures: Vec<ClosureFrame>,
    pub(crate) deferred: Vec<Deferred>,
    pub(crate) obligations: Vec<(Ty, DefId, Vec<Ty>, Span)>,
    pub(crate) int_lits: Vec<(Ty, u128, bool, Span)>,
    caps: Vec<String>,
    /// While checking the alternatives of an or-pattern: names bound by the
    /// first alternative.
    or_bindings: Option<HashMap<String, LocalId>>,
    or_seen: Vec<String>,
    binding_mutable: bool,
}

impl<'s, 'a> Cx<'s, 'a> {
    pub(crate) fn err(&mut self, code: &'static str, span: Span, msg: String) {
        let f = self.file;
        self.s.diags.error(code, f, span, msg);
    }

    pub(crate) fn tys(&self, t: &Ty) -> String {
        let r = self.tab.resolve(t);
        self.s.ty_str(&r)
    }

    pub(crate) fn fresh(&mut self) -> Ty {
        self.tab.fresh(VarKind::General)
    }

    // ------------------------------------------------------------ locals

    pub(crate) fn new_local(&mut self, name: &str, ty: Ty, mutable: bool, span: Span) -> LocalId {
        self.locals.push(hir::Local {
            name: name.to_string(),
            ty,
            mutable,
            span,
        });
        let id = (self.locals.len() - 1) as LocalId;
        self.scopes.last_mut().unwrap().push((name.to_string(), id));
        id
    }

    pub(crate) fn lookup_local(&mut self, name: &str) -> Option<LocalId> {
        let id = self
            .scopes
            .iter()
            .rev()
            .flat_map(|s| s.iter().rev())
            .find(|(n, _)| n == name)
            .map(|(_, id)| *id)?;
        for f in self.closures.iter_mut() {
            if id < f.first_local && !f.captures.contains(&id) {
                f.captures.push(id);
            }
        }
        Some(id)
    }

    fn push_scope(&mut self) {
        self.scopes.push(Vec::new());
    }

    fn pop_scope(&mut self) {
        self.scopes.pop();
    }

    // ------------------------------------------------------------ coercion

    /// `actual` flows into a place of type `expected`.
    pub(crate) fn coerce(&mut self, actual: &Ty, expected: &Ty, span: Span) {
        let a = self.tab.shallow(actual);
        if a == Ty::Never {
            return;
        }
        if self.tab.unify(&a, expected).is_err() {
            let (x, y) = (self.tys(expected), self.tys(&a));
            self.err(
                "T001",
                span,
                format!("mismatched types: expected `{x}`, found `{y}`"),
            );
        }
    }

    /// Join the types of alternative branches.
    pub(crate) fn join(&mut self, acc: &mut Option<Ty>, t: &Ty, span: Span) {
        let t = self.tab.shallow(t);
        if t == Ty::Never {
            return;
        }
        match acc {
            None => *acc = Some(t),
            Some(a) => {
                let a = a.clone();
                if self.tab.unify(&a, &t).is_err() {
                    let (x, y) = (self.tys(&a), self.tys(&t));
                    self.err(
                        "T001",
                        span,
                        format!("branches have incompatible types: `{x}` and `{y}`"),
                    );
                }
            }
        }
    }

    pub(crate) fn is_unit_expected(&self, t: Option<&Ty>) -> bool {
        t.is_some_and(|t| self.tab.shallow(t).is_unit())
    }

    // ------------------------------------------------------------ blocks

    pub(crate) fn block(&mut self, b: &ast::Block, expected: Option<&Ty>) -> Block {
        self.push_scope();
        let mut stmts = Vec::new();
        let mut tail = None;
        let n = b.stmts.len();
        for (i, st) in b.stmts.iter().enumerate() {
            if i + 1 == n {
                if let ast::Stmt::Expr(e) = st {
                    if self.is_unit_expected(expected) {
                        // The block's value is not wanted: the last
                        // expression is evaluated for its effect.
                        let h = self.expr(e, None);
                        if self.tab.shallow(&h.ty).is_unit() || self.tab.shallow(&h.ty) == Ty::Never
                        {
                            tail = Some(Box::new(h));
                        } else {
                            stmts.push(Stmt::Expr(h));
                        }
                    } else {
                        tail = Some(Box::new(self.expr(e, expected)));
                    }
                    continue;
                }
            }
            if let Some(s) = self.stmt(st) {
                stmts.push(s);
            }
        }
        self.pop_scope();
        Block { stmts, tail }
    }

    pub(crate) fn block_ty(&self, b: &Block) -> Ty {
        match &b.tail {
            Some(t) => t.ty.clone(),
            None => {
                // A block whose last statement diverges has type never.
                if b.stmts
                    .iter()
                    .any(|s| matches!(s, Stmt::Expr(e) if self.tab.shallow(&e.ty) == Ty::Never))
                {
                    Ty::Never
                } else {
                    Ty::unit()
                }
            }
        }
    }

    fn stmt(&mut self, st: &ast::Stmt) -> Option<Stmt> {
        match st {
            ast::Stmt::Let {
                var,
                pat,
                ty,
                value,
                ..
            } => {
                let declared = ty.as_ref().map(|t| {
                    let sc = self.scope.clone();
                    self.s.lower_ty(t, &sc)
                });
                let init = match &declared {
                    Some(d) => self.check(value, d),
                    None => self.expr(value, None),
                };
                let pty = declared.unwrap_or_else(|| init.ty.clone());
                let saved = self.binding_mutable;
                self.binding_mutable = *var;
                let p = self.pat(pat, &pty);
                self.binding_mutable = saved;
                Some(Stmt::Let { pat: p, init })
            }
            ast::Stmt::Const { name, ty, value } => {
                let sc = self.scope.clone();
                let t = self.s.lower_ty(ty, &sc);
                let init = self.check(value, &t);
                let span = value.span;
                let l = self.new_local(name, t.clone(), false, span);
                Some(Stmt::Let {
                    pat: Pat {
                        span,
                        ty: t,
                        kind: PatKind::Binding(l),
                    },
                    init,
                })
            }
            ast::Stmt::Assign { target, op, value } => {
                let place = self.expr(target, None);
                self.check_place(&place, target.span, "assign to");
                if op == "=" {
                    let v = self.check(value, &place.ty.clone());
                    Some(Stmt::Assign {
                        place,
                        op: None,
                        value: v,
                    })
                } else {
                    let bop = op.trim_end_matches('=').to_string();
                    let v = self.expr(value, Some(&place.ty.clone()));
                    let span = (target.span.0, value.span.1);
                    let rt = self.binary_ty(&bop, &place.ty.clone(), &v.ty.clone(), span);
                    self.coerce(&rt, &place.ty.clone(), span);
                    Some(Stmt::Assign {
                        place,
                        op: Some(bop),
                        value: v,
                    })
                }
            }
            ast::Stmt::Defer(e) => {
                let h = match &e.kind {
                    AE::Block(b) => {
                        let blk = self.block(b, Some(&Ty::unit()));
                        let ty = self.block_ty(&blk);
                        Expr {
                            span: e.span,
                            ty,
                            kind: ExprKind::Block(blk),
                        }
                    }
                    _ => self.expr(e, None),
                };
                Some(Stmt::Defer(h))
            }
            ast::Stmt::Region { name, body } => {
                self.push_scope();
                let l = self.new_local(name, Ty::Region, false, (0, 0));
                let b = self.block(body, Some(&Ty::unit()));
                self.pop_scope();
                Some(Stmt::Region { local: l, body: b })
            }
            ast::Stmt::Expr(e) => {
                let h = self.expr(e, None);
                Some(Stmt::Expr(h))
            }
            ast::Stmt::Error(_) => None,
        }
    }

    /// Assignment targets and `&` arguments must be mutable places.
    pub(crate) fn check_place(&mut self, e: &Expr, span: Span, what: &str) {
        match &e.kind {
            ExprKind::Local(l) => {
                if !self.locals[*l as usize].mutable {
                    let n = self.locals[*l as usize].name.clone();
                    self.err(
                        "T060",
                        span,
                        format!("cannot {what} `{n}`: it is not mutable (declare it with `var`, or take the parameter as `inout`)"),
                    );
                }
            }
            ExprKind::Field { recv, .. }
            | ExprKind::TupleField { recv, .. }
            | ExprKind::Index { recv, .. } => self.check_place(recv, span, what),
            ExprKind::Error => {}
            _ => self.err(
                "T061",
                span,
                format!("cannot {what} this expression: it is not a place"),
            ),
        }
    }
}

/// Type-check every function body and constant in the package.
pub fn check_bodies(s: &mut Sema) {
    let fns: Vec<DefId> =
        s.t.defs
            .iter()
            .enumerate()
            .filter(|(_, d)| d.kind == DefKind::Fn)
            .map(|(i, _)| i as DefId)
            .collect();
    for d in fns {
        let has_body =
            matches!(s.def(d).item.map(|i| &i.kind), Some(ItemKind::Fn(f)) if f.body.is_some());
        let sig = s.sig(d);
        if !has_body {
            continue;
        }
        check_fn(s, d, &sig);
    }
    let externs: Vec<DefId> = (0..s.t.defs.len() as DefId)
        .filter(|&d| s.def(d).kind == DefKind::ExternFn)
        .collect();
    for d in externs {
        s.sig(d);
    }
    let consts: Vec<DefId> = (0..s.t.defs.len() as DefId)
        .filter(|&d| s.def(d).kind == DefKind::Const)
        .collect();
    for d in consts {
        check_const(s, d);
    }
}

fn new_cx<'s, 'a>(s: &'s mut Sema<'a>, d: DefId) -> Cx<'s, 'a> {
    let g = s.generics_of(d);
    let scope = s.scope_for(d, &g);
    let file = scope.file;
    let mut bounds = g.bounds.clone();
    // Inside a trait, `Self` implements the trait.
    if let Some(p) = s.def(d).parent {
        if s.def(p).kind == DefKind::Trait {
            let tg = s.generics_of(p);
            bounds.push(Bound {
                ty: Ty::Param(tg.params[0]),
                trait_: p,
                args: tg.params[1..].iter().map(|q| Ty::Param(*q)).collect(),
            });
        }
    }
    let bounds = close_bounds(s, bounds);
    Cx {
        s,
        file,
        scope,
        bounds,
        tab: InferTable::default(),
        locals: Vec::new(),
        scopes: vec![Vec::new()],
        ret: Ty::unit(),
        loops: Vec::new(),
        closures: Vec::new(),
        deferred: Vec::new(),
        obligations: Vec::new(),
        int_lits: Vec::new(),
        caps: Vec::new(),
        or_bindings: None,
        or_seen: Vec::new(),
        binding_mutable: false,
    }
}

/// Add the supertrait bounds implied by each bound.
pub(crate) fn close_bounds(s: &mut Sema, mut bounds: Vec<Bound>) -> Vec<Bound> {
    let mut i = 0;
    while i < bounds.len() && bounds.len() < 256 {
        let b = bounds[i].clone();
        let tg = s.generics_of(b.trait_);
        let selfp = tg.params[0];
        let mut p = vec![selfp];
        p.extend(tg.params[1..].iter().copied());
        let mut a = vec![b.ty.clone()];
        a.extend(b.args.iter().cloned());
        for sb in &tg.bounds {
            if sb.ty == Ty::Param(selfp) {
                let nb = Bound {
                    ty: b.ty.clone(),
                    trait_: sb.trait_,
                    args: sb.args.iter().map(|t| t.subst(&p, &a)).collect(),
                };
                if !bounds
                    .iter()
                    .any(|x| x.ty == nb.ty && x.trait_ == nb.trait_ && x.args == nb.args)
                {
                    bounds.push(nb);
                }
            }
        }
        i += 1;
    }
    bounds
}

fn check_fn(s: &mut Sema, d: DefId, sig: &FnSigTy) {
    let Some(ItemKind::Fn(f)) = s.def(d).item.map(|i| &i.kind) else {
        return;
    };
    let body_ast = f.body.as_ref().unwrap();
    let clauses = &f.sig.clauses;
    let span = s.def(d).span;
    let mut cx = new_cx(s, d);
    cx.ret = sig.ret.clone();
    let mut params = Vec::new();
    for p in &sig.inputs {
        let mutable = p.mode == Mode::Inout || p.var;
        params.push(cx.new_local(&p.name, p.ty.clone(), mutable, span));
    }
    let mut requires = Vec::new();
    let mut ensures = Vec::new();
    for c in clauses {
        if let ast::Clause::Requires(e) = c {
            requires.push(cx.check(e, &Ty::Bool));
        }
    }
    let ret = sig.ret.clone();
    // `fn f() = e` is sugar for `fn f() { e }` (grammar 4.2, Phase 0 §4):
    // both forms are checked as the same block.
    let synth;
    let b: &ast::Block = match body_ast {
        ast::FnBody::Block(b) => b,
        ast::FnBody::Expr(e) => {
            synth = ast::Block {
                span: e.span,
                stmts: vec![ast::Stmt::Expr(e.clone())],
            };
            &synth
        }
    };
    let blk = cx.block(b, Some(&ret));
    let ty = cx.block_ty(&blk);
    let value = Expr {
        span: b.span,
        ty: ty.clone(),
        kind: ExprKind::Block(blk),
    };
    if !cx.tab.shallow(&ret).is_unit() {
        cx.coerce(&ty, &ret, b.span);
    }
    let mut result_local = None;
    if clauses.iter().any(|c| matches!(c, ast::Clause::Ensures(_))) {
        cx.push_scope();
        result_local = Some(cx.new_local("result", ret.clone(), false, span));
        for c in clauses {
            if let ast::Clause::Ensures(e) = c {
                ensures.push(cx.check(e, &Ty::Bool));
            }
        }
        cx.pop_scope();
    }
    let mut body = hir::Body {
        def: d,
        locals: Vec::new(),
        params,
        requires,
        ensures,
        result_local,
        value,
    };
    cx.finish(&mut body);
    s.bodies.insert(d, body);
}

fn check_const(s: &mut Sema, d: DefId) {
    let Some(ItemKind::Const { value, .. }) = s.def(d).item.map(|i| &i.kind) else {
        return;
    };
    let ty = s.const_ty(d);
    let Some(value) = value else {
        // A trait constant without a default: checked in impls.
        return;
    };
    let before = s.diags.list.len();
    let mut cx = new_cx(s, d);
    let v = cx.check(value, &ty);
    let mut body = hir::Body {
        def: d,
        locals: Vec::new(),
        params: Vec::new(),
        requires: Vec::new(),
        ensures: Vec::new(),
        result_local: None,
        value: v,
    };
    cx.finish(&mut body);
    s.bodies.insert(d, body);
    // Evaluate the constant when the evaluator can (reports overflow and
    // other T017 errors); skipped if its initializer already has errors.
    if !s.diags.list[before..]
        .iter()
        .any(|x| x.severity == crate::diag::Severity::Error)
    {
        s.const_value(d);
    }
}

impl<'s, 'a> Cx<'s, 'a> {
    pub(crate) fn cur_ret(&self) -> Ty {
        self.closures
            .last()
            .map(|f| f.ret.clone())
            .unwrap_or_else(|| self.ret.clone())
    }
    pub(crate) fn loops_mut(&mut self) -> &mut Vec<LoopCx> {
        &mut self.loops
    }
    pub(crate) fn enter_closure(&mut self, ret: Ty) {
        let loops = std::mem::take(&mut self.loops);
        self.closures.push(ClosureFrame {
            first_local: self.locals.len() as LocalId,
            captures: Vec::new(),
            ret,
            loops,
        });
    }
    pub(crate) fn leave_closure(&mut self) -> Vec<LocalId> {
        let f = self.closures.pop().unwrap();
        self.loops = f.loops;
        f.captures
    }
    pub(crate) fn caps_len(&self) -> usize {
        self.caps.len()
    }
    pub(crate) fn push_cap(&mut self, c: String) {
        self.caps.push(c);
    }
    pub(crate) fn truncate_caps(&mut self, n: usize) {
        self.caps.truncate(n);
    }
    pub(crate) fn has_cap(&self, c: &str) -> bool {
        self.caps.iter().any(|x| x == c)
    }
    pub(crate) fn push_scope_pub(&mut self) {
        self.push_scope();
    }
    pub(crate) fn pop_scope_pub(&mut self) {
        self.pop_scope();
    }
    pub(crate) fn or_state(
        &mut self,
    ) -> (
        &mut Option<HashMap<String, LocalId>>,
        &mut Vec<String>,
        bool,
    ) {
        (
            &mut self.or_bindings,
            &mut self.or_seen,
            self.binding_mutable,
        )
    }
}
