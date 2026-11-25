//! PURM v0.1 checker (spec/purm.md): moves and use-after-move (flow
//! sensitive, through branches, loops, early exits, closures and `defer`),
//! no moves out of borrows, the law of exclusivity at calls, borrowed
//! returns, `Copy` validity. Consuming uses of non-Copy locals are rewritten
//! to `MoveLocal` so lowering can transfer ownership explicitly.

use crate::defs::{DefId, DefKind};
use crate::hir::{self, Block, Expr, ExprKind, LocalId, Pat, PatKind, Stmt};
use crate::items::{AdtKind, Bound, Mode, Sema, VariantShape};
use crate::source::FileId;
use crate::ty::Ty;
use pegasus_parser::ast::Span;
use std::collections::{HashMap, HashSet};

/// The core marker trait `Copy`.
pub fn copy_trait(s: &Sema) -> DefId {
    s.t.mods[&s.t.core_root].types["Copy"].def
}

/// Is `t` Copy, given the generic bounds in scope?
pub fn is_copy(s: &mut Sema, bounds: &[Bound], t: &Ty) -> bool {
    match t {
        Ty::Int(_) | Ty::Float(_) | Ty::Bool | Ty::Char | Ty::Str | Ty::Ptr(..) | Ty::Region => {
            true
        }
        Ty::Never | Ty::Error | Ty::Infer(_) | Ty::Const(_) => true,
        Ty::Tuple(v) => v.iter().all(|x| is_copy(s, bounds, x)),
        Ty::Array(e, _) => is_copy(s, bounds, e),
        Ty::Fn(..) => false,
        Ty::Param(p) => {
            let c = copy_trait(s);
            bounds
                .iter()
                .any(|b| b.ty == Ty::Param(*p) && b.trait_ == c)
        }
        Ty::Adt(..) => {
            let c = copy_trait(s);
            s.implements(bounds, t, c, &[], 0)
        }
    }
}

/// Copy-ness of a fully concrete type (after monomorphization).
pub fn is_copy_concrete(s: &mut Sema, t: &Ty) -> bool {
    is_copy(s, &[], t)
}

/// Run the checker on every body, then validate `impl Copy`.
pub fn check(s: &mut Sema) {
    check_copy_impls(s);
    let mut ids: Vec<DefId> = s.bodies.keys().copied().collect();
    ids.sort();
    for d in ids {
        let mut body = s.bodies.remove(&d).unwrap();
        let file = s.def(d).file;
        let mut pc = Pc::new(s, d, file, &body);
        pc.run(&mut body);
        s.bodies.insert(d, body);
    }
}

fn check_copy_impls(s: &mut Sema) {
    let c = copy_trait(s);
    for imp in s.trait_impls.get(&c).cloned().unwrap_or_default() {
        let g = s.generics_of(imp);
        let bounds = crate::typeck::close_bounds(s, g.bounds.clone());
        let Some(Ty::Adt(d, args)) = s.impls.get(&imp).map(|i| i.self_ty.clone()) else {
            continue;
        };
        let a = s.adt(d);
        let mut tys: Vec<(String, Ty)> = Vec::new();
        match &a.kind {
            AdtKind::Struct(fs) => tys.extend(
                fs.iter()
                    .map(|f| (format!("field `{}`", f.name), f.ty.clone())),
            ),
            AdtKind::Enum(vs) => {
                for (v, sh) in vs {
                    let n = s.def(*v).name.clone();
                    match sh {
                        VariantShape::Unit => {}
                        VariantShape::Tuple(ts) => {
                            tys.extend(ts.iter().map(|t| (format!("variant `{n}`"), t.clone())))
                        }
                        VariantShape::Struct(fs) => {
                            tys.extend(fs.iter().map(|f| (format!("variant `{n}`"), f.ty.clone())))
                        }
                    }
                }
            }
        }
        for (what, t) in tys {
            let t = t.subst(&a.params, &args);
            if !is_copy(s, &bounds, &t) {
                let (f, sp) = (s.def(imp).file, s.def(imp).span);
                let ts = s.ty_str(&t);
                s.err(
                    "O010",
                    f,
                    sp,
                    format!("`Copy` needs every field to be Copy, but {what} has type `{ts}`"),
                );
            }
        }
    }
}

/// A place: a local and a path of field indices into it.
type PlacePath = (LocalId, Vec<u32>);

#[derive(Clone, Debug, Default)]
struct St {
    /// Moved places (whole locals have an empty path) and where they moved.
    moved: HashMap<PlacePath, Span>,
}

fn overlaps(a: &[u32], b: &[u32]) -> bool {
    a.starts_with(b) || b.starts_with(a)
}

impl St {
    /// A moved place overlapping `(l, path)`.
    fn conflict(&self, l: LocalId, path: &[u32]) -> Option<&Vec<u32>> {
        self.moved
            .keys()
            .find(|(m, p)| *m == l && overlaps(p, path))
            .map(|(_, p)| p)
    }
}

type State = Option<St>;

fn join(a: State, b: State) -> State {
    match (a, b) {
        (None, x) | (x, None) => x,
        (Some(mut x), Some(y)) => {
            for (k, v) in y.moved {
                x.moved.entry(k).or_insert(v);
            }
            Some(x)
        }
    }
}

struct LoopSt {
    breaks: State,
    conts: State,
    defer_depth: usize,
}

struct Pc<'x, 'a> {
    s: &'x mut Sema<'a>,
    file: FileId,
    bounds: Vec<Bound>,
    locals: Vec<hir::Local>,
    borrowed: HashSet<LocalId>,
    captured: Vec<HashSet<LocalId>>,
    loops: Vec<LoopSt>,
    defers: Vec<Vec<Expr>>,
    ret_borrow: bool,
    in_defer: bool,
    copy_cache: HashMap<Ty, bool>,
}

impl<'x, 'a> Pc<'x, 'a> {
    fn new(s: &'x mut Sema<'a>, d: DefId, file: FileId, body: &hir::Body) -> Self {
        let g = s.generics_of(d);
        let mut bounds = g.bounds.clone();
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
        let bounds = crate::typeck::close_bounds(s, bounds);
        let mut borrowed = HashSet::new();
        let mut ret_borrow = false;
        if s.def(d).kind == DefKind::Fn {
            let sig = s.sig(d);
            ret_borrow = sig.ret_borrow;
            for (p, l) in sig.inputs.iter().zip(&body.params) {
                if p.mode != Mode::Own {
                    borrowed.insert(*l);
                }
            }
        }
        Pc {
            s,
            file,
            bounds,
            locals: body.locals.clone(),
            borrowed,
            captured: Vec::new(),
            loops: Vec::new(),
            defers: Vec::new(),
            ret_borrow,
            in_defer: false,
            copy_cache: HashMap::new(),
        }
    }

    fn err(&mut self, code: &'static str, span: Span, msg: String) {
        let f = self.file;
        self.s.diags.error(code, f, span, msg);
    }

    fn copy(&mut self, t: &Ty) -> bool {
        if let Some(&c) = self.copy_cache.get(t) {
            return c;
        }
        let bounds = self.bounds.clone();
        let c = is_copy(self.s, &bounds, t);
        self.copy_cache.insert(t.clone(), c);
        c
    }

    fn name(&self, l: LocalId) -> String {
        self.locals[l as usize].name.clone()
    }

    fn run(&mut self, body: &mut hir::Body) {
        let mut st: State = Some(St::default());
        for r in body.requires.iter_mut() {
            self.expr(r, false, &mut st);
        }
        if let Some(bad) = self
            .ret_borrow
            .then(|| self.borrow_bad(&body.value))
            .flatten()
        {
            self.err(
                "O005",
                bad,
                "a borrowed return (`-> in`) must be derived from a borrowed parameter".into(),
            );
        }
        // A borrowed return is a borrow of the caller's data, not a move.
        let consume = !self.ret_borrow;
        self.expr(&mut body.value, consume, &mut st);
        // `ensures` reads the result and the parameters after the body.
        let mut st2: State = Some(St::default());
        for e in body.ensures.iter_mut() {
            self.expr(e, false, &mut st2);
        }
    }

    // ------------------------------------------------------------ uses

    fn path_str(&self, l: LocalId, path: &[u32]) -> String {
        let mut n = self.name(l);
        for i in path {
            n.push_str(&format!(".{i}"));
        }
        n
    }

    /// A place expression made only of field projections on a local.
    fn place_path(e: &Expr) -> Option<PlacePath> {
        match &e.kind {
            ExprKind::Local(l) | ExprKind::MoveLocal(l) => Some((*l, Vec::new())),
            ExprKind::MovePath { local, path } => Some((*local, path.clone())),
            ExprKind::Field { recv, index } | ExprKind::TupleField { recv, index } => {
                let (l, mut p) = Self::place_path(recv)?;
                p.push(*index as u32);
                Some((l, p))
            }
            _ => None,
        }
    }

    /// Read or consume the place `(l, path)` that `e` denotes.
    fn use_place(
        &mut self,
        e: &mut Expr,
        l: LocalId,
        path: Vec<u32>,
        consume: bool,
        st: &mut State,
    ) {
        let Some(s) = st.as_mut() else { return };
        let span = e.span;
        if let Some(q) = s.conflict(l, &path).cloned() {
            let what = self.path_str(l, &path);
            let moved = self.path_str(l, &q);
            let detail = if q == path {
                String::new()
            } else {
                format!(" (`{moved}` was moved)")
            };
            self.err("O001", span, format!("use of moved value `{what}`{detail} (it was moved earlier on some path to here)"));
            return;
        }
        if !consume || self.copy(&e.ty.clone()) {
            return;
        }
        let n = self.path_str(l, &path);
        if self.in_defer {
            self.err("O013", span, format!("`defer` may not move `{n}`"));
            return;
        }
        if self.captured.last().is_some_and(|c| c.contains(&l)) {
            self.err("O008", span, format!("cannot move captured variable `{n}` out of a closure (the closure may run more than once)"));
            return;
        }
        if self.borrowed.contains(&l) {
            self.err(
                "O002",
                span,
                format!("cannot move out of `{n}`, which is borrowed (take the parameter as `own`, or use a Copy type)"),
            );
            return;
        }
        st.as_mut().unwrap().moved.insert((l, path.clone()), span);
        e.kind = if path.is_empty() {
            ExprKind::MoveLocal(l)
        } else {
            ExprKind::MovePath { local: l, path }
        };
    }

    fn use_local(&mut self, e: &mut Expr, l: LocalId, consume: bool, st: &mut State) {
        self.use_place(e, l, Vec::new(), consume, st);
    }

    /// The local at the root of a place expression.
    fn place_root(e: &Expr) -> Option<LocalId> {
        match &e.kind {
            ExprKind::Local(l) | ExprKind::MoveLocal(l) => Some(*l),
            ExprKind::MovePath { local, .. } => Some(*local),
            ExprKind::Field { recv, .. }
            | ExprKind::TupleField { recv, .. }
            | ExprKind::Index { recv, .. } => Self::place_root(recv),
            _ => None,
        }
    }

    fn mentions(e: &Expr, l: LocalId) -> bool {
        let mut found = false;
        visit(e, &mut |x| {
            if matches!(x.kind, ExprKind::Local(y) | ExprKind::MoveLocal(y) if y == l)
                || matches!(&x.kind, ExprKind::MovePath { local, .. } if *local == l)
            {
                found = true;
            }
        });
        found
    }

    fn pat_binds_noncopy(&mut self, p: &Pat) -> bool {
        match &p.kind {
            PatKind::Binding(l) => {
                let t = self.locals[*l as usize].ty.clone();
                !self.copy(&t)
            }
            PatKind::Tuple(v) | PatKind::Or(v) => v.iter().any(|x| self.pat_binds_noncopy(x)),
            PatKind::Variant { fields, .. } | PatKind::Struct { fields, .. } => {
                fields.iter().any(|x| self.pat_binds_noncopy(x))
            }
            PatKind::Array { before, after, .. } => before
                .iter()
                .chain(after)
                .any(|x| self.pat_binds_noncopy(x)),
            _ => false,
        }
    }

    /// The first sub-expression of a borrowed return that is not derived
    /// from a borrowed parameter, if any (for a precise O005).
    fn borrow_bad(&self, e: &Expr) -> Option<Span> {
        if e.ty == Ty::Never {
            return None;
        }
        match &e.kind {
            ExprKind::Field { recv, .. }
            | ExprKind::TupleField { recv, .. }
            | ExprKind::Index { recv, .. }
                if !matches!(recv.kind, ExprKind::Local(_)) =>
            {
                self.borrow_bad(recv)
            }
            ExprKind::Block(b) | ExprKind::Unsafe { body: b, .. } => {
                b.tail.as_ref().and_then(|t| self.borrow_bad(t))
            }
            ExprKind::If {
                then, otherwise, ..
            }
            | ExprKind::IfLet {
                then, otherwise, ..
            } => then
                .tail
                .as_ref()
                .and_then(|t| self.borrow_bad(t))
                .or_else(|| otherwise.as_ref().and_then(|o| self.borrow_bad(o))),
            ExprKind::Match { arms, .. } => arms.iter().find_map(|a| self.borrow_bad(&a.body)),
            _ => (!self.borrow_ok(e)).then_some(e.span),
        }
    }

    /// Is `e` derived from a borrowed parameter (for `-> in` returns)?
    fn borrow_ok(&self, e: &Expr) -> bool {
        if e.ty == Ty::Never {
            return true;
        }
        match &e.kind {
            ExprKind::Local(l) | ExprKind::MoveLocal(l) => self.borrowed.contains(l),
            ExprKind::MovePath { local, .. } => self.borrowed.contains(local),
            ExprKind::Field { recv, .. }
            | ExprKind::TupleField { recv, .. }
            | ExprKind::Index { recv, .. } => self.borrow_ok(recv),
            ExprKind::Block(b) | ExprKind::Unsafe { body: b, .. } => match &b.tail {
                Some(t) => self.borrow_ok(t),
                None => true,
            },
            ExprKind::If {
                then, otherwise, ..
            }
            | ExprKind::IfLet {
                then, otherwise, ..
            } => {
                then.tail.as_ref().is_none_or(|t| self.borrow_ok(t))
                    && otherwise.as_ref().is_none_or(|o| self.borrow_ok(o))
            }
            ExprKind::Match { arms, .. } => arms.iter().all(|a| self.borrow_ok(&a.body)),
            // A borrowed result is tied to every argument the callee borrows
            // (Phase 0 §6: tie to all borrowed inputs when ambiguous), so it
            // is derived from our parameters only if all of those are.
            ExprKind::Call { callee, args } => match &callee.kind {
                ExprKind::FnRef(d, _) => match self.s.sigs.get(d) {
                    Some(sig) if sig.ret_borrow => sig
                        .inputs
                        .iter()
                        .zip(args)
                        .filter(|(p, _)| p.mode != Mode::Own)
                        .all(|(_, a)| self.borrow_ok(a)),
                    _ => false,
                },
                _ => false,
            },
            ExprKind::Return(_) | ExprKind::Break(_) | ExprKind::Continue => true,
            _ => false,
        }
    }

    // ------------------------------------------------------------ expressions

    fn expr(&mut self, e: &mut Expr, consume: bool, st: &mut State) {
        if st.is_none() {
            return;
        }
        let ety = e.ty.clone();
        let span = e.span;
        let field_place = match e.kind {
            ExprKind::Field { .. } | ExprKind::TupleField { .. } => Self::place_path(e),
            _ => None,
        };
        if let Some((l, p)) = field_place {
            // A field path of a local is a place: reading checks it,
            // consuming a non-Copy part moves only that part.
            self.use_place(e, l, p, consume, st);
            return;
        }
        match &mut e.kind {
            ExprKind::Local(l) | ExprKind::MoveLocal(l) => {
                let l = *l;
                self.use_local(e, l, consume, st);
            }
            ExprKind::MovePath { local, path } => {
                let (l, p) = (*local, path.clone());
                self.use_place(e, l, p, consume, st);
            }
            ExprKind::Lit(_) | ExprKind::FnRef(..) | ExprKind::Const(_) | ExprKind::Error => {}
            ExprKind::Call { callee, args } => {
                let (modes, ret_borrow): (Vec<Mode>, bool) = match &callee.kind {
                    ExprKind::FnRef(d, _)
                        if matches!(self.s.def(*d).kind, DefKind::Fn | DefKind::ExternFn) =>
                    {
                        let sig = self.s.sig(*d);
                        (sig.inputs.iter().map(|p| p.mode).collect(), sig.ret_borrow)
                    }
                    _ => (vec![Mode::In; args.len()], false),
                };
                self.expr(callee, false, st);
                // Law of exclusivity: an exclusively borrowed variable may not
                // appear anywhere else in the same call.
                for (i, a) in args.iter().enumerate() {
                    if modes.get(i) != Some(&Mode::Inout) {
                        continue;
                    }
                    if let Some(root) = Self::place_root(a) {
                        if args
                            .iter()
                            .enumerate()
                            .any(|(j, b)| j != i && Self::mentions(b, root))
                        {
                            let n = self.name(root);
                            self.err(
                                "O003",
                                a.span,
                                format!("`{n}` is borrowed exclusively (`inout`) here, so it cannot also be used by another argument of this call"),
                            );
                        }
                    }
                }
                for (i, a) in args.iter_mut().enumerate() {
                    let own = modes.get(i) == Some(&Mode::Own);
                    self.expr(a, own, st);
                }
                if ret_borrow && consume && !self.copy(&ety) {
                    self.err("O002", span, "cannot move out of a borrowed result (`-> in`); only Copy values can be taken out".into());
                }
            }
            ExprKind::Variant { fields, .. }
            | ExprKind::Tuple(fields)
            | ExprKind::Array(fields) => {
                for f in fields {
                    self.expr(f, true, st);
                }
            }
            ExprKind::Struct { def, fields, base } => {
                for (_, f) in fields.iter_mut() {
                    self.expr(f, true, st);
                }
                if let Some(b) = base {
                    // `..base` moves base only if a field it supplies is non-Copy.
                    let given: HashSet<usize> = fields.iter().map(|(i, _)| *i).collect();
                    let a = self.s.adt(*def);
                    let args = match &ety {
                        Ty::Adt(_, a) => a.clone(),
                        _ => vec![],
                    };
                    let mut needs_move = false;
                    if let AdtKind::Struct(fs) = &a.kind {
                        for (i, f) in fs.iter().enumerate() {
                            if !given.contains(&i) && !self.copy(&f.ty.subst(&a.params, &args)) {
                                needs_move = true;
                            }
                        }
                    }
                    self.expr(b, needs_move, st);
                }
            }
            ExprKind::Field { recv, .. } | ExprKind::TupleField { recv, .. } => {
                // A field path of a local is a place: reading checks it,
                // consuming a non-Copy part moves only that part.
                let c = consume && !self.copy(&ety);
                self.expr(recv, c, st);
            }
            ExprKind::Index { recv, index } => {
                self.expr(index, false, st);
                if consume && !self.copy(&ety) {
                    self.err(
                        "O004",
                        span,
                        "cannot move a non-Copy element out of an array".into(),
                    );
                }
                self.expr(recv, false, st);
            }
            ExprKind::Unary { expr, .. } | ExprKind::Cast(expr) => self.expr(expr, false, st),
            ExprKind::Binary { op, lhs, rhs } => {
                self.expr(lhs, false, st);
                if op == "&&" || op == "||" {
                    let mut s2 = st.clone();
                    self.expr(rhs, false, &mut s2);
                    *st = join(st.take(), s2);
                } else {
                    self.expr(rhs, false, st);
                }
            }
            ExprKind::Repeat { value, count } => {
                if *count != 1 && !self.copy(&value.ty.clone()) {
                    self.err(
                        "O012",
                        span,
                        "`[v; n]` needs a Copy element: it would duplicate a value with one owner"
                            .into(),
                    );
                }
                self.expr(value, true, st);
            }
            ExprKind::Block(b) | ExprKind::Unsafe { body: b, .. } => self.block(b, consume, st),
            ExprKind::If {
                cond,
                then,
                otherwise,
            } => {
                self.expr(cond, false, st);
                let mut a = st.clone();
                self.block(then, consume, &mut a);
                let mut b = st.clone();
                if let Some(o) = otherwise {
                    self.expr(o, consume, &mut b);
                }
                *st = join(a, b);
            }
            ExprKind::IfLet {
                pat,
                value,
                then,
                otherwise,
            } => {
                let c = self.pat_binds_noncopy(pat);
                self.expr(value, c, st);
                let mut a = st.clone();
                self.block(then, consume, &mut a);
                let mut b = st.clone();
                if let Some(o) = otherwise {
                    self.expr(o, consume, &mut b);
                }
                *st = join(a, b);
            }
            ExprKind::Match { scrutinee, arms } => {
                let c = arms.iter().any(|a| {
                    // (computed below with &mut self)
                    let _ = a;
                    false
                });
                let binds = {
                    let mut any = c;
                    for a in arms.iter() {
                        if self.pat_binds_noncopy(&a.pat) {
                            any = true;
                        }
                    }
                    any
                };
                self.expr(scrutinee, binds, st);
                let mut acc: State = None;
                for a in arms.iter_mut() {
                    let mut s2 = st.clone();
                    if let Some(g) = &mut a.guard {
                        self.expr(g, false, &mut s2);
                    }
                    self.expr(&mut a.body, consume, &mut s2);
                    acc = join(acc, s2);
                }
                *st = acc;
            }
            ExprKind::Loop(b) => {
                self.run_loop(st, false, |pc, s| {
                    pc.block(b, false, s);
                    None
                });
            }
            ExprKind::While { cond, body } => {
                self.run_loop(st, true, |pc, s| {
                    pc.expr(cond, false, s);
                    let exit = s.clone();
                    pc.block(body, false, s);
                    exit
                });
            }
            ExprKind::WhileLet { pat, value, body } => {
                let c = self.pat_binds_noncopy(pat);
                self.run_loop(st, true, |pc, s| {
                    pc.expr(value, c, s);
                    let exit = s.clone();
                    pc.block(body, false, s);
                    exit
                });
            }
            ExprKind::ForRange { lo, hi, body, .. } => {
                self.expr(lo, false, st);
                self.expr(hi, false, st);
                self.run_loop(st, true, |pc, s| {
                    let exit = s.clone();
                    pc.block(body, false, s);
                    exit
                });
            }
            ExprKind::ForArray { pat, array, body } => {
                // Taking non-Copy elements out of the array consumes it.
                let c = self.pat_binds_noncopy(pat);
                self.expr(array, c, st);
                self.run_loop(st, true, |pc, s| {
                    let exit = s.clone();
                    pc.block(body, false, s);
                    exit
                });
            }
            ExprKind::Break(v) => {
                if let Some(v) = v {
                    self.expr(v, true, st);
                }
                let depth = self.loops.last().map(|l| l.defer_depth).unwrap_or(0);
                self.check_defers(depth, st);
                if let Some(lp) = self.loops.last_mut() {
                    lp.breaks = join(lp.breaks.take(), st.clone());
                }
                *st = None;
            }
            ExprKind::Continue => {
                let depth = self.loops.last().map(|l| l.defer_depth).unwrap_or(0);
                self.check_defers(depth, st);
                if let Some(lp) = self.loops.last_mut() {
                    lp.conts = join(lp.conts.take(), st.clone());
                }
                *st = None;
            }
            ExprKind::Return(v) => {
                if let Some(v) = v {
                    if let Some(bad) = self.ret_borrow.then(|| self.borrow_bad(v)).flatten() {
                        self.err(
                            "O005",
                            bad,
                            "a borrowed return (`-> in`) must be derived from a borrowed parameter"
                                .into(),
                        );
                    }
                    let consume = !self.ret_borrow;
                    self.expr(v, consume, st);
                }
                self.check_defers(0, st);
                *st = None;
            }
            ExprKind::Try { expr, .. } => self.expr(expr, true, st),
            ExprKind::Closure {
                params,
                body,
                captures,
            } => {
                let caps = captures.clone();
                for c in &caps {
                    let mut fake = Expr {
                        span,
                        ty: self.locals[*c as usize].ty.clone(),
                        kind: ExprKind::Local(*c),
                    };
                    self.use_local(&mut fake, *c, true, st);
                }
                let _ = params;
                let saved_loops = std::mem::take(&mut self.loops);
                let saved_defers = std::mem::take(&mut self.defers);
                let saved_rb = std::mem::replace(&mut self.ret_borrow, false);
                self.captured.push(caps.into_iter().collect());
                let mut inner: State = Some(St::default());
                self.expr(body, true, &mut inner);
                self.captured.pop();
                self.loops = saved_loops;
                self.defers = saved_defers;
                self.ret_borrow = saved_rb;
            }
        }
    }

    /// Analyse a loop twice so a move in one iteration is seen at the next.
    /// `body` returns the state on the loop's natural exit (None for `loop`).
    fn run_loop(
        &mut self,
        st: &mut State,
        natural_exit: bool,
        mut body: impl FnMut(&mut Self, &mut State) -> State,
    ) {
        let entry = st.clone();
        let mut back: State = None;
        let mut exit: State = None;
        for _pass in 0..2 {
            let mut s = join(entry.clone(), back.clone());
            self.loops.push(LoopSt {
                breaks: None,
                conts: None,
                defer_depth: self.defers.len(),
            });
            let head_exit = body(self, &mut s);
            let lp = self.loops.pop().unwrap();
            back = join(s, lp.conts);
            exit = join(if natural_exit { head_exit } else { None }, lp.breaks);
        }
        *st = exit;
    }

    fn block(&mut self, b: &mut Block, consume: bool, st: &mut State) {
        self.defers.push(Vec::new());
        for s in b.stmts.iter_mut() {
            self.stmt(s, st);
        }
        if let Some(t) = &mut b.tail {
            self.expr(t, consume, st);
        }
        let depth = self.defers.len() - 1;
        self.check_defers(depth, st);
        self.defers.pop();
    }

    fn stmt(&mut self, s: &mut Stmt, st: &mut State) {
        match s {
            Stmt::Let { init, .. } => self.expr(init, true, st),
            Stmt::Assign { place, op, value } => {
                self.expr(value, true, st);
                match (Self::place_path(place), op.is_none()) {
                    (Some((l, path)), true) => {
                        // `p = v` re-initializes p and everything below it,
                        // but a moved parent cannot be partly assigned.
                        if let Some(s) = st.as_mut() {
                            if let Some(q) = s
                                .moved
                                .keys()
                                .find(|(m, q)| {
                                    *m == l && q.len() < path.len() && path.starts_with(q)
                                })
                                .map(|(_, q)| q.clone())
                            {
                                let n = self.path_str(l, &q);
                                self.err(
                                    "O001",
                                    place.span,
                                    format!("assignment to part of moved value `{n}`"),
                                );
                            } else {
                                s.moved
                                    .retain(|(m, q), _| !(*m == l && q.starts_with(&path)));
                            }
                        }
                    }
                    _ => {
                        if let Some(root) = Self::place_root(place) {
                            if st.as_ref().is_some_and(|s| s.conflict(root, &[]).is_some()) {
                                let n = self.name(root);
                                self.err(
                                    "O001",
                                    place.span,
                                    format!("assignment to part of moved value `{n}`"),
                                );
                            }
                        }
                        // Indices inside the place, and the old value for `op=`.
                        self.place_reads(place, op.is_some(), st);
                    }
                }
            }
            Stmt::Expr(e) => self.expr(e, false, st),
            Stmt::Defer(e) => {
                // Moves are checked now; uses against each exit's state later.
                let mut probe = st.clone();
                let saved = std::mem::replace(&mut self.in_defer, true);
                let mut copy = e.clone();
                self.expr(&mut copy, false, &mut probe);
                self.in_defer = saved;
                if let Some(d) = self.defers.last_mut() {
                    d.push(e.clone());
                }
            }
            Stmt::Region { body, .. } => self.block(body, false, st),
        }
    }

    fn place_reads(&mut self, place: &mut Expr, read_old: bool, st: &mut State) {
        match &mut place.kind {
            ExprKind::Field { recv, .. } | ExprKind::TupleField { recv, .. } => {
                self.place_reads(recv, read_old, st)
            }
            ExprKind::Index { recv, index } => {
                self.expr(index, false, st);
                self.place_reads(recv, read_old, st);
            }
            ExprKind::Local(_) | ExprKind::MoveLocal(_) | ExprKind::MovePath { .. } if read_old => {
                let mut c = place.clone();
                self.expr(&mut c, false, st);
            }
            _ => {}
        }
    }

    /// Deferred expressions of scopes `from..` run at this exit: everything
    /// they read must still be initialized.
    fn check_defers(&mut self, from: usize, st: &State) {
        if st.is_none() {
            return;
        }
        let ds: Vec<Expr> = self.defers[from.min(self.defers.len())..]
            .iter()
            .flatten()
            .cloned()
            .collect();
        let saved = std::mem::replace(&mut self.in_defer, true);
        for mut d in ds {
            let mut s = st.clone();
            self.expr(&mut d, false, &mut s);
        }
        self.in_defer = saved;
    }
}

/// Pre-order visit of every expression.
pub fn visit(e: &Expr, f: &mut dyn FnMut(&Expr)) {
    f(e);
    let vb = visit_block;
    match &e.kind {
        ExprKind::Call { callee, args } => {
            visit(callee, f);
            args.iter().for_each(|a| visit(a, f));
        }
        ExprKind::Variant { fields, .. } | ExprKind::Tuple(fields) | ExprKind::Array(fields) => {
            fields.iter().for_each(|a| visit(a, f))
        }
        ExprKind::Struct { fields, base, .. } => {
            fields.iter().for_each(|(_, a)| visit(a, f));
            if let Some(b) = base {
                visit(b, f);
            }
        }
        ExprKind::Field { recv, .. } | ExprKind::TupleField { recv, .. } => visit(recv, f),
        ExprKind::Index { recv, index } => {
            visit(recv, f);
            visit(index, f);
        }
        ExprKind::Unary { expr, .. } | ExprKind::Cast(expr) | ExprKind::Try { expr, .. } => {
            visit(expr, f)
        }
        ExprKind::Binary { lhs, rhs, .. } => {
            visit(lhs, f);
            visit(rhs, f);
        }
        ExprKind::Repeat { value, .. } => visit(value, f),
        ExprKind::Block(b) | ExprKind::Loop(b) | ExprKind::Unsafe { body: b, .. } => vb(b, f),
        ExprKind::If {
            cond,
            then,
            otherwise,
        } => {
            visit(cond, f);
            vb(then, f);
            if let Some(o) = otherwise {
                visit(o, f);
            }
        }
        ExprKind::IfLet {
            value,
            then,
            otherwise,
            ..
        } => {
            visit(value, f);
            vb(then, f);
            if let Some(o) = otherwise {
                visit(o, f);
            }
        }
        ExprKind::Match { scrutinee, arms } => {
            visit(scrutinee, f);
            for a in arms {
                if let Some(g) = &a.guard {
                    visit(g, f);
                }
                visit(&a.body, f);
            }
        }
        ExprKind::While { cond, body } => {
            visit(cond, f);
            vb(body, f);
        }
        ExprKind::WhileLet { value, body, .. } => {
            visit(value, f);
            vb(body, f);
        }
        ExprKind::ForRange { lo, hi, body, .. } => {
            visit(lo, f);
            visit(hi, f);
            vb(body, f);
        }
        ExprKind::ForArray { array, body, .. } => {
            visit(array, f);
            vb(body, f);
        }
        ExprKind::Break(Some(v)) | ExprKind::Return(Some(v)) => visit(v, f),
        ExprKind::Closure { body, .. } => visit(body, f),
        _ => {}
    }
}

fn visit_block(b: &Block, f: &mut dyn FnMut(&Expr)) {
    for s in &b.stmts {
        match s {
            Stmt::Let { init, .. } => visit(init, f),
            Stmt::Assign { place, value, .. } => {
                visit(place, f);
                visit(value, f);
            }
            Stmt::Expr(x) | Stmt::Defer(x) => visit(x, f),
            Stmt::Region { body, .. } => visit_block(body, f),
        }
    }
    if let Some(t) = &b.tail {
        visit(t, f);
    }
}
