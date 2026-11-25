//! Pattern exhaustiveness (match arms) and irrefutability (let, for and
//! closure parameters), by the usefulness algorithm over constructors.
//!
//! The analysis has four outcomes: exhaustive; a missing pattern (with a
//! witness); inconclusive (the work budget ran out: reported as T083, never
//! treated as exhaustive); or skipped (the patterns contain a type error that
//! was already reported).
//!
//! Fixed-size arrays are never expanded element by element: an array column
//! becomes at most `max prefix + max suffix` columns, the positions some
//! pattern actually constrains. Integers, chars, strings and floats have no
//! finite constructor set in v0.1: matching them needs a catch-all arm.

use crate::defs::DefId;
use crate::hir::{self, Block, Expr, ExprKind, Pat, PatKind, Stmt};
use crate::items::{AdtKind, Sema, VariantShape};
use crate::source::FileId;
use crate::ty::{ConstVal, Ty};

/// Recursion and work limits; exceeding either is inconclusive (T083).
const MAX_DEPTH: u32 = 1024;
const MAX_STEPS: u64 = 1_000_000;

#[derive(Clone, Debug, PartialEq)]
enum Ctor {
    Bool(bool),
    Variant(DefId),
    Single,
}

#[derive(Clone, Debug)]
enum P {
    Wild,
    /// Matches some values of an infinite type (literal, range, constant).
    Opaque,
    C(Ctor, Vec<P>),
    /// A fixed-size array pattern, kept unexpanded.
    Arr {
        before: Vec<P>,
        rest: bool,
        after: Vec<P>,
    },
}

#[derive(Debug)]
enum Outcome {
    Exhaustive,
    Missing(Vec<String>),
    Inconclusive,
    /// Ill-typed input whose error is already reported.
    Skip,
}

fn lower(p: &Pat, out: &mut Vec<P>) {
    match &p.kind {
        PatKind::Or(alts) => alts.iter().for_each(|a| lower(a, out)),
        _ => out.push(lower1(p)),
    }
}

fn lower1(p: &Pat) -> P {
    match &p.kind {
        PatKind::Wild | PatKind::Binding(_) => P::Wild,
        PatKind::Lit {
            lit: crate::lit::Lit::Bool(b),
            ..
        } => P::C(Ctor::Bool(*b), Vec::new()),
        PatKind::Lit { .. } | PatKind::Range { .. } | PatKind::Const(_) => P::Opaque,
        PatKind::Tuple(v) => P::C(Ctor::Single, v.iter().map(lower1).collect()),
        PatKind::Struct { fields, .. } => P::C(Ctor::Single, fields.iter().map(lower1).collect()),
        PatKind::Variant { variant, fields } => {
            P::C(Ctor::Variant(*variant), fields.iter().map(lower1).collect())
        }
        PatKind::Array {
            before,
            rest,
            after,
        } => {
            let b: Vec<P> = before.iter().map(lower1).collect();
            let a: Vec<P> = after.iter().map(lower1).collect();
            if *rest && b.iter().chain(&a).all(|x| matches!(x, P::Wild)) {
                P::Wild
            } else {
                P::Arr {
                    before: b,
                    rest: *rest,
                    after: a,
                }
            }
        }
        PatKind::Or(alts) => {
            // Inside a constructor: irrefutable if some alternative is;
            // otherwise opaque (conservative: may report a missing pattern
            // that is in fact covered; never the reverse).
            if alts.iter().map(lower1).any(|x| matches!(x, P::Wild)) {
                P::Wild
            } else {
                P::Opaque
            }
        }
    }
}

fn array_len(t: &Ty) -> Option<usize> {
    match t {
        Ty::Array(_, n) => match **n {
            Ty::Const(ConstVal::Int(n)) => n.to_usize(),
            _ => None,
        },
        _ => None,
    }
}

struct Ex<'x, 'a> {
    s: &'x mut Sema<'a>,
    steps: u64,
}

/// What a column's type offers to the algorithm.
enum Cols {
    /// A finite set of constructors with their field types.
    Finite(Vec<(Ctor, Vec<Ty>)>),
    /// A fixed-size array, compressed to `m` columns: `prefix` from the
    /// front and `m - prefix` from the back (all `n` if `m == n`).
    Array {
        elem: Ty,
        n: usize,
        m: usize,
        prefix: usize,
    },
    Infinite,
}

impl Ex<'_, '_> {
    fn columns(&mut self, t: &Ty, rows: &[Vec<P>]) -> Cols {
        match t {
            Ty::Bool => Cols::Finite(vec![
                (Ctor::Bool(false), vec![]),
                (Ctor::Bool(true), vec![]),
            ]),
            Ty::Never => Cols::Finite(vec![]),
            Ty::Tuple(v) => Cols::Finite(vec![(Ctor::Single, v.clone())]),
            Ty::Array(e, _) => {
                let Some(n) = array_len(t) else {
                    return Cols::Infinite;
                };
                let (mut maxp, mut maxs) = (0usize, 0usize);
                for r in rows {
                    if let P::Arr {
                        before,
                        rest,
                        after,
                    } = &r[0]
                    {
                        if *rest {
                            maxp = maxp.max(before.len());
                            maxs = maxs.max(after.len());
                        } else {
                            maxp = maxp.max(before.len());
                        }
                    }
                }
                let m = if maxp.saturating_add(maxs) >= n {
                    n
                } else {
                    maxp + maxs
                };
                Cols::Array {
                    elem: (**e).clone(),
                    n,
                    m,
                    prefix: if m == n { n } else { maxp },
                }
            }
            Ty::Adt(d, args) => {
                let a = self.s.adt(*d);
                match &a.kind {
                    AdtKind::Struct(fs) => Cols::Finite(vec![(
                        Ctor::Single,
                        fs.iter().map(|f| f.ty.subst(&a.params, args)).collect(),
                    )]),
                    AdtKind::Enum(vs) => Cols::Finite(
                        vs.iter()
                            .map(|(v, s)| {
                                let tys = match s {
                                    VariantShape::Unit => vec![],
                                    VariantShape::Tuple(ts) => {
                                        ts.iter().map(|t| t.subst(&a.params, args)).collect()
                                    }
                                    VariantShape::Struct(fs) => {
                                        fs.iter().map(|f| f.ty.subst(&a.params, args)).collect()
                                    }
                                };
                                (Ctor::Variant(*v), tys)
                            })
                            .collect(),
                    ),
                }
            }
            _ => Cols::Infinite,
        }
    }

    fn useful(&mut self, rows: &[Vec<P>], tys: &[Ty], depth: u32) -> Outcome {
        if tys.iter().any(|t| t.has_error()) || rows.iter().any(|r| r.len() != tys.len()) {
            return Outcome::Skip;
        }
        // A row of wildcards matches every value: nothing is missing.
        if rows.iter().any(|r| r.iter().all(|p| matches!(p, P::Wild))) {
            return Outcome::Exhaustive;
        }
        if tys.is_empty() {
            return Outcome::Missing(vec![]);
        }
        self.steps += 1 + rows.len() as u64;
        if depth > MAX_DEPTH || self.steps > MAX_STEPS {
            return Outcome::Inconclusive;
        }
        let head = tys[0].clone();
        let rest = &tys[1..];
        let prefix_with = |w: Vec<String>, first: String, k: usize| {
            let mut out = vec![first];
            out.extend_from_slice(&w[k..]);
            out
        };
        match self.columns(&head, rows) {
            Cols::Finite(cs) => {
                let used: Vec<Ctor> = rows
                    .iter()
                    .filter_map(|r| match &r[0] {
                        P::C(c, _) => Some(c.clone()),
                        _ => None,
                    })
                    .collect();
                let complete = cs.iter().all(|(c, _)| used.contains(c));
                if complete {
                    let mut inconclusive = false;
                    for (c, ftys) in &cs {
                        let spec = specialize(rows, c, ftys.len());
                        let mut nt = ftys.clone();
                        nt.extend_from_slice(rest);
                        match self.useful(&spec, &nt, depth + 1) {
                            Outcome::Missing(w) => {
                                let first = self.render(c, &head, &w[..ftys.len()]);
                                return Outcome::Missing(prefix_with(w, first, ftys.len()));
                            }
                            Outcome::Inconclusive => inconclusive = true,
                            Outcome::Skip => return Outcome::Skip,
                            Outcome::Exhaustive => {}
                        }
                    }
                    if inconclusive {
                        Outcome::Inconclusive
                    } else {
                        Outcome::Exhaustive
                    }
                } else {
                    match self.useful(&default_rows(rows), rest, depth + 1) {
                        Outcome::Missing(w) => {
                            let (c, ftys) = cs.iter().find(|(c, _)| !used.contains(c)).unwrap();
                            let wild: Vec<String> = ftys.iter().map(|_| "_".to_string()).collect();
                            let first = self.render(c, &head, &wild);
                            let mut out = vec![first];
                            out.extend(w);
                            Outcome::Missing(out)
                        }
                        other => other,
                    }
                }
            }
            Cols::Array { elem, n, m, prefix } => {
                let spec = specialize_array(rows, n, m, prefix);
                let mut nt = vec![elem; m];
                nt.extend_from_slice(rest);
                match self.useful(&spec, &nt, depth + 1) {
                    Outcome::Missing(w) => {
                        let first = if m == n {
                            format!("[{}]", w[..m].join(", "))
                        } else {
                            let mut parts: Vec<String> = w[..prefix].to_vec();
                            parts.push("..".into());
                            parts.extend_from_slice(&w[prefix..m]);
                            format!("[{}]", parts.join(", "))
                        };
                        Outcome::Missing(prefix_with(w, first, m))
                    }
                    other => other,
                }
            }
            Cols::Infinite => match self.useful(&default_rows(rows), rest, depth + 1) {
                Outcome::Missing(w) => {
                    let mut out = vec!["_".to_string()];
                    out.extend(w);
                    Outcome::Missing(out)
                }
                other => other,
            },
        }
    }

    fn render(&self, c: &Ctor, t: &Ty, fields: &[String]) -> String {
        match c {
            Ctor::Bool(b) => b.to_string(),
            Ctor::Variant(v) => {
                let n = self.s.def(*v).name.clone();
                let e = self.s.def(*v).parent.unwrap();
                let is_struct = matches!(self.s.adts.get(&e).map(|a| &a.kind),
                    Some(AdtKind::Enum(vs)) if vs.iter().any(|(d, s)| d == v && matches!(s, VariantShape::Struct(_))));
                if fields.is_empty() {
                    n
                } else if is_struct {
                    format!("{n} {{ .. }}")
                } else {
                    format!("{n}({})", fields.join(", "))
                }
            }
            Ctor::Single => match t {
                Ty::Tuple(_) => format!("({})", fields.join(", ")),
                Ty::Adt(d, _) => format!("{} {{ .. }}", self.s.def(*d).name),
                _ => "_".into(),
            },
        }
    }
}

fn specialize(rows: &[Vec<P>], c: &Ctor, arity: usize) -> Vec<Vec<P>> {
    let mut out = Vec::new();
    for r in rows {
        match &r[0] {
            P::C(c2, subs) if c2 == c => {
                let mut nr = subs.clone();
                nr.resize(arity, P::Wild);
                nr.extend_from_slice(&r[1..]);
                out.push(nr);
            }
            P::Wild => {
                let mut nr = vec![P::Wild; arity];
                nr.extend_from_slice(&r[1..]);
                out.push(nr);
            }
            _ => {}
        }
    }
    out
}

/// Expand array patterns into the `m` relevant positions.
fn specialize_array(rows: &[Vec<P>], n: usize, m: usize, prefix: usize) -> Vec<Vec<P>> {
    let mut out = Vec::new();
    for r in rows {
        let cols: Vec<P> = match &r[0] {
            P::Wild => vec![P::Wild; m],
            P::Arr {
                before,
                rest,
                after,
            } => {
                if !rest && before.len() != n {
                    continue; // wrong arity: already reported (T077)
                }
                let suffix = m - prefix;
                let mut v = Vec::with_capacity(m);
                for i in 0..prefix {
                    // Element i from the front.
                    let from_back = n - 1 - i;
                    v.push(if i < before.len() {
                        before[i].clone()
                    } else if from_back < after.len() {
                        after[after.len() - 1 - from_back].clone()
                    } else {
                        P::Wild
                    });
                }
                for j in 0..suffix {
                    // Element n - suffix + j, i.e. (suffix - j) from the back.
                    let idx = n - suffix + j;
                    let from_back = suffix - 1 - j;
                    v.push(if from_back < after.len() {
                        after[after.len() - 1 - from_back].clone()
                    } else if idx < before.len() {
                        before[idx].clone()
                    } else {
                        P::Wild
                    });
                }
                v
            }
            _ => continue,
        };
        let mut nr = cols;
        nr.extend_from_slice(&r[1..]);
        out.push(nr);
    }
    out
}

fn default_rows(rows: &[Vec<P>]) -> Vec<Vec<P>> {
    rows.iter()
        .filter(|r| matches!(r[0], P::Wild))
        .map(|r| r[1..].to_vec())
        .collect()
}

pub fn check_body(s: &mut Sema, file: FileId, body: &hir::Body) {
    let mut ex = Ex { s, steps: 0 };
    walk_expr(&mut ex, file, &body.value);
}

fn pat_has_error(p: &Pat) -> bool {
    p.ty.has_error()
        || match &p.kind {
            PatKind::Tuple(v) | PatKind::Or(v) => v.iter().any(pat_has_error),
            PatKind::Variant { fields, .. } | PatKind::Struct { fields, .. } => {
                fields.iter().any(pat_has_error)
            }
            PatKind::Array { before, after, .. } => before.iter().chain(after).any(pat_has_error),
            _ => false,
        }
}

fn analyse(
    ex: &mut Ex,
    file: FileId,
    rows: Vec<Vec<P>>,
    ty: &Ty,
    span: (u32, u32),
    missing: impl FnOnce(&str) -> (&'static str, String),
) {
    ex.steps = 0;
    match ex.useful(&rows, std::slice::from_ref(ty), 0) {
        Outcome::Exhaustive | Outcome::Skip => {}
        Outcome::Missing(w) => {
            let (code, msg) = missing(&w[0]);
            ex.s.diags.error(code, file, span, msg);
        }
        Outcome::Inconclusive => ex.s.diags.error(
            "T083",
            file,
            span,
            "these patterns are too large to check for exhaustiveness; add a catch-all arm (`_`)"
                .into(),
        ),
    }
}

fn irrefutable(ex: &mut Ex, file: FileId, p: &Pat, what: &str) {
    if pat_has_error(p) {
        return;
    }
    let mut rows = Vec::new();
    lower(p, &mut rows);
    let rows = rows.into_iter().map(|x| vec![x]).collect();
    analyse(ex, file, rows, &p.ty, p.span, |w| {
        (
            "T081",
            format!("refutable pattern in {what}: `{w}` not covered (use `if let` or `match`)"),
        )
    });
}

fn walk_block(ex: &mut Ex, file: FileId, b: &Block) {
    for s in &b.stmts {
        match s {
            Stmt::Let { pat, init } => {
                walk_expr(ex, file, init);
                irrefutable(ex, file, pat, "`let`");
            }
            Stmt::Assign { place, value, .. } => {
                walk_expr(ex, file, place);
                walk_expr(ex, file, value);
            }
            Stmt::Expr(e) | Stmt::Defer(e) => walk_expr(ex, file, e),
            Stmt::Region { body, .. } => walk_block(ex, file, body),
        }
    }
    if let Some(t) = &b.tail {
        walk_expr(ex, file, t);
    }
}

fn walk_expr(ex: &mut Ex, file: FileId, e: &Expr) {
    match &e.kind {
        ExprKind::Match { scrutinee, arms } => {
            walk_expr(ex, file, scrutinee);
            let mut rows = Vec::new();
            for a in arms {
                if let Some(g) = &a.guard {
                    walk_expr(ex, file, g);
                }
                walk_expr(ex, file, &a.body);
                if a.guard.is_none() {
                    let mut ps = Vec::new();
                    lower(&a.pat, &mut ps);
                    rows.extend(ps.into_iter().map(|x| vec![x]));
                }
            }
            if !arms.iter().any(|a| pat_has_error(&a.pat)) {
                analyse(ex, file, rows, &scrutinee.ty, e.span, |w| {
                    ("T082", format!("non-exhaustive match: `{w}` not covered"))
                });
            }
        }
        ExprKind::Call { callee, args } => {
            walk_expr(ex, file, callee);
            args.iter().for_each(|a| walk_expr(ex, file, a));
        }
        ExprKind::Variant { fields, .. } | ExprKind::Tuple(fields) | ExprKind::Array(fields) => {
            fields.iter().for_each(|a| walk_expr(ex, file, a))
        }
        ExprKind::Struct { fields, base, .. } => {
            fields.iter().for_each(|(_, a)| walk_expr(ex, file, a));
            if let Some(b) = base {
                walk_expr(ex, file, b);
            }
        }
        ExprKind::Field { recv, .. } | ExprKind::TupleField { recv, .. } => {
            walk_expr(ex, file, recv)
        }
        ExprKind::Index { recv, index } => {
            walk_expr(ex, file, recv);
            walk_expr(ex, file, index);
        }
        ExprKind::Unary { expr, .. } | ExprKind::Cast(expr) | ExprKind::Try { expr, .. } => {
            walk_expr(ex, file, expr)
        }
        ExprKind::Binary { lhs, rhs, .. } => {
            walk_expr(ex, file, lhs);
            walk_expr(ex, file, rhs);
        }
        ExprKind::Repeat { value, .. } => walk_expr(ex, file, value),
        ExprKind::Block(b) | ExprKind::Loop(b) | ExprKind::Unsafe { body: b, .. } => {
            walk_block(ex, file, b)
        }
        ExprKind::If {
            cond,
            then,
            otherwise,
        } => {
            walk_expr(ex, file, cond);
            walk_block(ex, file, then);
            if let Some(o) = otherwise {
                walk_expr(ex, file, o);
            }
        }
        ExprKind::IfLet {
            value,
            then,
            otherwise,
            ..
        } => {
            walk_expr(ex, file, value);
            walk_block(ex, file, then);
            if let Some(o) = otherwise {
                walk_expr(ex, file, o);
            }
        }
        ExprKind::While { cond, body } => {
            walk_expr(ex, file, cond);
            walk_block(ex, file, body);
        }
        ExprKind::WhileLet { value, body, .. } => {
            walk_expr(ex, file, value);
            walk_block(ex, file, body);
        }
        ExprKind::ForRange {
            pat, lo, hi, body, ..
        } => {
            walk_expr(ex, file, lo);
            walk_expr(ex, file, hi);
            irrefutable(ex, file, pat, "`for`");
            walk_block(ex, file, body);
        }
        ExprKind::ForArray { pat, array, body } => {
            walk_expr(ex, file, array);
            irrefutable(ex, file, pat, "`for`");
            walk_block(ex, file, body);
        }
        ExprKind::Break(v) | ExprKind::Return(v) => {
            if let Some(v) = v {
                walk_expr(ex, file, v);
            }
        }
        ExprKind::Closure { params, body, .. } => {
            for p in params {
                irrefutable(ex, file, p, "closure parameter");
            }
            walk_expr(ex, file, body);
        }
        ExprKind::Lit(_)
        | ExprKind::MoveLocal(_)
        | ExprKind::MovePath { .. }
        | ExprKind::Local(_)
        | ExprKind::FnRef(..)
        | ExprKind::Const(_)
        | ExprKind::Continue
        | ExprKind::Error => {}
    }
}
