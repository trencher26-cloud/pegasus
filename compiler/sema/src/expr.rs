//! Expression checking.

use crate::defs::DefKind;
use crate::hir::{self, Block, Expr, ExprKind};
use crate::infer::VarKind;
use crate::items::{AdtKind, VariantShape, CAPABILITIES};
use crate::lit::Lit;
use crate::ty::{CInt, ConstVal, FloatTy, IntTy, Ty};
use crate::typeck::{Cx, LoopCx, OpCheck, PathRes};
use pegasus_parser::ast::{self, ExprKind as AE, Span};

impl<'s, 'a> Cx<'s, 'a> {
    pub(crate) fn check(&mut self, e: &ast::Expr, ty: &Ty) -> Expr {
        let h = self.expr(e, Some(ty));
        let t = h.ty.clone();
        self.coerce(&t, ty, e.span);
        h
    }

    fn mk(&self, span: Span, ty: Ty, kind: ExprKind) -> Expr {
        Expr { span, ty, kind }
    }

    pub(crate) fn expr(&mut self, e: &ast::Expr, expected: Option<&Ty>) -> Expr {
        let span = e.span;
        match &e.kind {
            AE::Error => Expr::error(span),
            AE::Literal(text) => self.literal(text, span, expected, false),
            AE::Unary { op: "neg", expr } if matches!(&expr.kind, AE::Literal(_)) => {
                let AE::Literal(text) = &expr.kind else {
                    unreachable!()
                };
                let inner = self.literal(text, expr.span, expected, true);
                let ty = inner.ty.clone();
                self.deferred_op(OpCheck::Neg, "-", &ty, &ty, span);
                self.mk(
                    span,
                    ty,
                    ExprKind::Unary {
                        op: "-",
                        expr: Box::new(inner),
                    },
                )
            }
            AE::Interp { .. } => {
                self.err(
                    "U010",
                    span,
                    "string interpolation produces a String, which needs the standard library (not implemented yet)".into(),
                );
                Expr::error(span)
            }
            AE::Name(_) | AE::SelfValue(_) | AE::GenericName { .. } | AE::Field { .. } => {
                self.path_or_field(e, expected)
            }
            AE::TupleField { recv, index } => {
                let r = self.expr(recv, None);
                let rt = self.tab.shallow(&r.ty);
                let idx: usize = index.parse().unwrap_or(usize::MAX);
                match &rt {
                    Ty::Tuple(ts) if idx < ts.len() => {
                        let t = ts[idx].clone();
                        self.mk(
                            span,
                            t,
                            ExprKind::TupleField {
                                recv: Box::new(r),
                                index: idx,
                            },
                        )
                    }
                    Ty::Error => Expr::error(span),
                    Ty::Infer(_) => {
                        self.err(
                            "T050",
                            span,
                            "type annotations needed: the type of this value is not known here"
                                .into(),
                        );
                        Expr::error(span)
                    }
                    _ => {
                        let t = self.tys(&rt);
                        self.err("T041", span, format!("type `{t}` has no field `{index}`"));
                        Expr::error(span)
                    }
                }
            }
            AE::Call { callee, args } => self.call(callee, args, span, expected),
            AE::Index { recv, index } => {
                let r = self.expr(recv, None);
                if let AE::Range { .. } = index.kind {
                    self.err(
                        "U009",
                        span,
                        "slicing needs Span<T> from the standard library (not implemented yet)"
                            .into(),
                    );
                    return Expr::error(span);
                }
                let rt = self.tab.shallow(&r.ty);
                match rt {
                    Ty::Array(elem, _) => {
                        let i = self.check(index, &Ty::usize());
                        self.mk(
                            span,
                            *elem,
                            ExprKind::Index {
                                recv: Box::new(r),
                                index: Box::new(i),
                            },
                        )
                    }
                    Ty::Error => Expr::error(span),
                    Ty::Infer(_) => {
                        self.err(
                            "T050",
                            span,
                            "type annotations needed: the type of this value is not known here"
                                .into(),
                        );
                        Expr::error(span)
                    }
                    other => {
                        let t = self.tys(&other);
                        self.err(
                            "T042",
                            span,
                            format!(
                                "type `{t}` cannot be indexed (only fixed-size arrays in v0.1)"
                            ),
                        );
                        Expr::error(span)
                    }
                }
            }
            AE::Try(inner) => {
                let h = self.expr(inner, None);
                let t = self.tab.shallow(&h.ty);
                let ret = self.cur_ret();
                let rret = self.tab.shallow(&ret);
                match &t {
                    Ty::Adt(d, args) if *d == self.s.option => {
                        let want = Ty::Adt(self.s.option, vec![self.fresh()]);
                        if self.tab.unify(&rret, &want).is_err() {
                            let r = self.tys(&rret);
                            self.err("T080", span, format!("`?` on an Option needs the function to return an Option, not `{r}`"));
                        }
                        let ty = args[0].clone();
                        self.mk(
                            span,
                            ty,
                            ExprKind::Try {
                                expr: Box::new(h),
                                is_result: false,
                            },
                        )
                    }
                    Ty::Adt(d, args) if *d == self.s.result => {
                        let want = Ty::Adt(self.s.result, vec![self.fresh(), args[1].clone()]);
                        if self.tab.unify(&rret, &want).is_err() {
                            let r = self.tys(&rret);
                            let e = self.tys(&args[1]);
                            self.err(
                                "T080",
                                span,
                                format!("`?` on a Result with error type `{e}` needs the function to return `Result<_, {e}>`, not `{r}`"),
                            );
                        }
                        let ty = args[0].clone();
                        self.mk(
                            span,
                            ty,
                            ExprKind::Try {
                                expr: Box::new(h),
                                is_result: true,
                            },
                        )
                    }
                    Ty::Error => Expr::error(span),
                    Ty::Infer(_) => {
                        self.err(
                            "T050",
                            span,
                            "type annotations needed: the type of this value is not known here"
                                .into(),
                        );
                        Expr::error(span)
                    }
                    other => {
                        let s = self.tys(other);
                        self.err(
                            "T080",
                            span,
                            format!("`?` applies to Option and Result values, not `{s}`"),
                        );
                        Expr::error(span)
                    }
                }
            }
            AE::Await(_) => {
                self.err(
                    "U011",
                    span,
                    "`await` needs async functions, which are not implemented yet".into(),
                );
                Expr::error(span)
            }
            AE::Unary { op, expr } => {
                let h = self.expr(expr, expected);
                let t = h.ty.clone();
                // The parser names prefix operators; HIR uses their symbols.
                let (kind, op) = match *op {
                    "neg" => (OpCheck::Neg, "-"),
                    "pos" => (OpCheck::Plus, "+"),
                    "not" => (OpCheck::Not, "!"),
                    _ => (OpCheck::BitNot, "~"),
                };
                if kind == OpCheck::Not {
                    let st = self.tab.shallow(&t);
                    let intlike = matches!(st, Ty::Int(_))
                        || matches!(st, Ty::Infer(v) if self.tab.kind(v) == Some(VarKind::Int));
                    if intlike {
                        self.deferred_op(OpCheck::Not, "!", &t, &t, span);
                    } else {
                        self.coerce(&t, &Ty::Bool, expr.span);
                    }
                } else {
                    self.deferred_op(kind, op, &t, &t, span);
                }
                self.mk(
                    span,
                    t,
                    ExprKind::Unary {
                        op,
                        expr: Box::new(h),
                    },
                )
            }
            AE::Binary { op, lhs, rhs } => {
                if op == "&&" || op == "||" {
                    let l = self.check(lhs, &Ty::Bool);
                    let r = self.check(rhs, &Ty::Bool);
                    return self.mk(
                        span,
                        Ty::Bool,
                        ExprKind::Binary {
                            op: op.clone(),
                            lhs: Box::new(l),
                            rhs: Box::new(r),
                        },
                    );
                }
                let cmp = matches!(op.as_str(), "==" | "!=" | "<" | ">" | "<=" | ">=");
                let separate = matches!(op.as_str(), "<<" | ">>" | "**");
                let l = self.expr(lhs, if cmp || separate { None } else { expected });
                let lt = l.ty.clone();
                let r = if separate {
                    self.expr(rhs, None)
                } else {
                    self.expr(rhs, Some(&lt))
                };
                let rt = r.ty.clone();
                let ty = self.binary_ty(op, &lt, &rt, span);
                self.mk(
                    span,
                    ty,
                    ExprKind::Binary {
                        op: op.clone(),
                        lhs: Box::new(l),
                        rhs: Box::new(r),
                    },
                )
            }
            AE::Cast { expr, ty } => {
                let sc = self.scope.clone();
                let target = self.s.lower_ty(ty, &sc);
                let h = self.expr(expr, None);
                self.cast(&h.ty.clone(), &target, span);
                self.mk(span, target, ExprKind::Cast(Box::new(h)))
            }
            AE::Range { .. } => {
                self.err(
                    "U013",
                    span,
                    "range values need the standard library's Range type (ranges work in `for` loops)".into(),
                );
                Expr::error(span)
            }
            AE::Closure {
                is_async,
                params,
                body,
            } => self.closure(*is_async, params, body, span, expected),
            AE::Struct { path, fields, base } => {
                self.struct_lit(path, fields, base.as_deref(), span)
            }
            AE::Tuple(es) => {
                let exp: Option<Vec<Ty>> = expected.and_then(|t| match self.tab.shallow(t) {
                    Ty::Tuple(v) if v.len() == es.len() => Some(v),
                    _ => None,
                });
                let hs: Vec<Expr> = es
                    .iter()
                    .enumerate()
                    .map(|(i, x)| self.expr(x, exp.as_ref().map(|v| &v[i])))
                    .collect();
                let ty = Ty::Tuple(hs.iter().map(|h| h.ty.clone()).collect());
                self.mk(span, ty, ExprKind::Tuple(hs))
            }
            AE::Unit => self.mk(span, Ty::unit(), ExprKind::Tuple(Vec::new())),
            AE::Array(es) => {
                let elem = match expected.map(|t| self.tab.shallow(t)) {
                    Some(Ty::Array(e, _)) => *e,
                    _ => self.fresh(),
                };
                let mut hs = Vec::new();
                for x in es {
                    let h = self.expr(x, Some(&elem));
                    let t = h.ty.clone();
                    self.coerce(&t, &elem, x.span);
                    hs.push(h);
                }
                let ty = Ty::Array(
                    Box::new(elem),
                    Box::new(Ty::Const(ConstVal::Int(CInt::from_usize(hs.len())))),
                );
                self.mk(span, ty, ExprKind::Array(hs))
            }
            AE::Repeat { value, count } => {
                let elem = match expected.map(|t| self.tab.shallow(t)) {
                    Some(Ty::Array(e, _)) => *e,
                    _ => self.fresh(),
                };
                let v = self.check(value, &elem);
                let sc = self.scope.clone();
                let n = match crate::consteval::eval(self.s, count, &sc, Some(&Ty::usize())) {
                    // A `usize` value always fits u64 on the 64-bit v1 targets.
                    Ok(crate::consteval::CValue::Int(v, _)) => {
                        v.to_usize().map(|n| n as u64).unwrap_or(0)
                    }
                    Ok(_) => {
                        self.err("U007", count.span, "array lengths that depend on const parameters in expressions are not implemented yet".into());
                        0
                    }
                    Err(crate::consteval::ConstErr::Unsupported(m)) => {
                        self.err("U007", count.span, m);
                        0
                    }
                    Err(crate::consteval::ConstErr::Invalid(m)) => {
                        self.err("T017", count.span, m);
                        0
                    }
                };
                let ty = Ty::Array(
                    Box::new(elem),
                    Box::new(Ty::Const(ConstVal::Int(CInt::from_u128(n as u128)))),
                );
                self.mk(
                    span,
                    ty,
                    ExprKind::Repeat {
                        value: Box::new(v),
                        count: n,
                    },
                )
            }
            AE::Block(b) => {
                let blk = self.block(b, expected);
                let ty = self.block_ty(&blk);
                self.mk(span, ty, ExprKind::Block(blk))
            }
            AE::Comptime(_) => {
                self.err(
                    "U012",
                    span,
                    "comptime blocks need the reference interpreter, which is not implemented yet"
                        .into(),
                );
                Expr::error(span)
            }
            AE::Unsafe { caps, body } => {
                let mut names = Vec::new();
                for c in caps {
                    let n = c.join(".");
                    if !CAPABILITIES.contains(&n.as_str()) {
                        self.err(
                            "T101",
                            span,
                            format!("unknown capability `{n}` (v1: {})", CAPABILITIES.join(", ")),
                        );
                    }
                    names.push(n);
                }
                let before = self.caps_len();
                for n in &names {
                    self.push_cap(n.clone());
                }
                let blk = self.block(body, expected);
                self.truncate_caps(before);
                let ty = self.block_ty(&blk);
                self.mk(
                    span,
                    ty,
                    ExprKind::Unsafe {
                        caps: names,
                        body: blk,
                    },
                )
            }
            AE::If {
                cond,
                then,
                otherwise,
            } => self.if_expr(cond, then, otherwise.as_deref(), span, expected),
            AE::Match { scrutinee, arms } => self.match_expr(scrutinee, arms, span, expected),
            AE::While { cond, body } => match &**cond {
                ast::Cond::Expr(c) => {
                    let c = self.check(c, &Ty::Bool);
                    let b = self.loop_body(body, None);
                    self.mk(
                        span,
                        Ty::unit(),
                        ExprKind::While {
                            cond: Box::new(c),
                            body: b.0,
                        },
                    )
                }
                ast::Cond::Let { pat, value } => {
                    let v = self.expr(value, None);
                    self.scope_push();
                    let p = self.pat(pat, &v.ty.clone());
                    let b = self.loop_body(body, None);
                    self.scope_pop();
                    self.mk(
                        span,
                        Ty::unit(),
                        ExprKind::WhileLet {
                            pat: p,
                            value: Box::new(v),
                            body: b.0,
                        },
                    )
                }
            },
            AE::Loop(body) => {
                let bt = self.fresh();
                let (b, broke) = self.loop_body(body, Some(bt.clone()));
                let ty = if broke { bt } else { Ty::Never };
                self.mk(span, ty, ExprKind::Loop(b))
            }
            AE::For {
                pat,
                iter,
                with,
                body,
            } => {
                if !with.is_empty() {
                    self.err(
                        "U014",
                        span,
                        "`with` clauses on for loops are not implemented yet".into(),
                    );
                }
                self.for_expr(pat, iter, body, span)
            }
            AE::Return(v) => {
                let ret = self.cur_ret();
                let h = match v {
                    Some(v) => Some(Box::new(self.check(v, &ret))),
                    None => {
                        if !self.tab.shallow(&ret).is_unit()
                            && self.tab.unify(&ret, &Ty::unit()).is_err()
                        {
                            let r = self.tys(&ret);
                            self.err(
                                "T001",
                                span,
                                format!("`return` without a value in a function returning `{r}`"),
                            );
                        }
                        None
                    }
                };
                self.mk(span, Ty::Never, ExprKind::Return(h))
            }
            AE::Break(v) => {
                let Some(lp) = self.loops_mut().last_mut() else {
                    self.err("T070", span, "`break` outside of a loop".into());
                    return Expr::error(span);
                };
                lp.has_break = true;
                let bt = lp.break_ty.clone();
                let h = match (v, bt) {
                    (Some(v), Some(bt)) => Some(Box::new(self.check(v, &bt))),
                    (Some(v), None) => {
                        self.err(
                            "T070",
                            span,
                            "`break` with a value is only allowed in `loop`".into(),
                        );
                        Some(Box::new(self.expr(v, None)))
                    }
                    (None, Some(bt)) => {
                        self.coerce(&Ty::unit(), &bt, span);
                        None
                    }
                    (None, None) => None,
                };
                self.mk(span, Ty::Never, ExprKind::Break(h))
            }
            AE::Continue => {
                if self.loops_mut().is_empty() {
                    self.err("T070", span, "`continue` outside of a loop".into());
                }
                self.mk(span, Ty::Never, ExprKind::Continue)
            }
        }
    }

    // ------------------------------------------------------------ literals

    fn literal(&mut self, text: &str, span: Span, expected: Option<&Ty>, negated: bool) -> Expr {
        let Some(lit) = crate::lit::parse(text) else {
            return Expr::error(span);
        };
        let ty = match &lit {
            Lit::Int(v, suf) => {
                let t = match suf {
                    Some(s) => Ty::Int(*s),
                    None => {
                        let v = self.tab.fresh(VarKind::Int);
                        if let Some(e) = expected {
                            let e = self.tab.shallow(e);
                            if matches!(e, Ty::Int(_) | Ty::Infer(_)) {
                                let _ = self.tab.probe(&v, &e);
                            }
                        }
                        v
                    }
                };
                self.int_lits.push((t.clone(), *v, negated, span));
                t
            }
            Lit::Float(_, suf) => match suf {
                Some(s) => Ty::Float(*s),
                None => {
                    let v = self.tab.fresh(VarKind::Float);
                    if let Some(e) = expected {
                        let e = self.tab.shallow(e);
                        if matches!(e, Ty::Float(_) | Ty::Infer(_)) {
                            let _ = self.tab.probe(&v, &e);
                        }
                    }
                    v
                }
            },
            Lit::Bool(_) => Ty::Bool,
            Lit::Char(_) => Ty::Char,
            Lit::Str(_) => Ty::Str,
            Lit::Bytes(b) => Ty::Array(
                Box::new(Ty::Int(IntTy::U8)),
                Box::new(Ty::Const(ConstVal::Int(CInt::from_usize(b.len())))),
            ),
        };
        self.mk(span, ty, ExprKind::Lit(lit))
    }

    // ------------------------------------------------------------ operators

    pub(crate) fn deferred_op(&mut self, kind: OpCheck, op: &str, a: &Ty, b: &Ty, span: Span) {
        self.deferred.push(crate::typeck::Deferred {
            kind,
            op: op.to_string(),
            a: a.clone(),
            b: b.clone(),
            span,
        });
    }

    pub(crate) fn binary_ty(&mut self, op: &str, lt: &Ty, rt: &Ty, span: Span) -> Ty {
        let same = |cx: &mut Self| {
            if cx.tab.unify(lt, rt).is_err() {
                let (a, b) = (cx.tys(lt), cx.tys(rt));
                cx.err(
                    "T002",
                    span,
                    format!("operator `{op}` needs operands of the same type, found `{a}` and `{b}` (no implicit conversions)"),
                );
            }
        };
        match op {
            "+" | "-" | "*" | "/" | "%" => {
                same(self);
                self.deferred_op(OpCheck::Arith, op, lt, lt, span);
                lt.clone()
            }
            "+%" | "-%" | "*%" | "+|" | "-|" | "*|" => {
                same(self);
                self.deferred_op(OpCheck::WrapSat, op, lt, lt, span);
                lt.clone()
            }
            "&" | "|" | "^" => {
                same(self);
                self.deferred_op(OpCheck::Bitwise, op, lt, lt, span);
                lt.clone()
            }
            "<<" | ">>" => {
                self.deferred_op(OpCheck::Shift, op, lt, rt, span);
                lt.clone()
            }
            "**" => {
                self.deferred_op(OpCheck::Pow, op, lt, rt, span);
                lt.clone()
            }
            "==" | "!=" => {
                same(self);
                self.deferred_op(OpCheck::Eq, op, lt, lt, span);
                Ty::Bool
            }
            "<" | ">" | "<=" | ">=" => {
                same(self);
                self.deferred_op(OpCheck::Ord, op, lt, lt, span);
                Ty::Bool
            }
            _ => Ty::Error,
        }
    }

    /// `as` performs lossless conversions only (Phase 0 spec, section 5).
    fn cast(&mut self, from: &Ty, to: &Ty, span: Span) {
        let f = self.tab.shallow(from);
        if let Ty::Infer(v) = f {
            match (self.tab.kind(v), to) {
                (Some(VarKind::Int), Ty::Int(_)) | (Some(VarKind::Float), Ty::Float(_)) => {
                    let _ = self.tab.unify(&f, to);
                    return;
                }
                _ => {}
            }
        }
        // Literal defaults apply before judging other casts.
        let mut unresolved = false;
        let f = self.tab.finish(&f, &mut unresolved);
        if unresolved {
            self.err(
                "T050",
                span,
                "type annotations needed: the type being cast is not known here".into(),
            );
            return;
        }
        if let Ty::Infer(_) = self.tab.shallow(from) {
            let _ = self.tab.unify(from, &f);
        }
        if !lossless(&f, to) {
            let (a, b) = (self.s.ty_str(&f), self.s.ty_str(to));
            self.err(
                "T090",
                span,
                format!("`{a} as {b}` is not a lossless conversion; `as` only widens (checked conversions need the standard library)"),
            );
        }
    }

    // ------------------------------------------------------------ control flow

    fn if_expr(
        &mut self,
        cond: &ast::Cond,
        then: &ast::Block,
        otherwise: Option<&ast::Expr>,
        span: Span,
        expected: Option<&Ty>,
    ) -> Expr {
        let want = if otherwise.is_none() {
            Some(Ty::unit())
        } else {
            expected.cloned()
        };
        let (c, pat) = match cond {
            ast::Cond::Expr(c) => (self.check(c, &Ty::Bool), None),
            ast::Cond::Let { pat, value } => {
                let v = self.expr(value, None);
                self.scope_push();
                let p = self.pat(pat, &v.ty.clone());
                (v, Some(p))
            }
        };
        let tb = self.block(then, want.as_ref());
        if pat.is_some() {
            self.scope_pop();
        }
        let tt = self.block_ty(&tb);
        let ob = otherwise.map(|o| Box::new(self.expr(o, want.as_ref())));
        let ty = match &ob {
            None => {
                if !self.tab.shallow(&tt).is_unit() && self.tab.shallow(&tt) != Ty::Never {
                    // Unreachable in practice: the block discards its value.
                    Ty::unit()
                } else {
                    Ty::unit()
                }
            }
            Some(o) => {
                let mut acc = None;
                self.join(&mut acc, &tt, then.span);
                self.join(&mut acc, &o.ty.clone(), o.span);
                acc.unwrap_or(Ty::Never)
            }
        };
        match pat {
            None => self.mk(
                span,
                ty,
                ExprKind::If {
                    cond: Box::new(c),
                    then: tb,
                    otherwise: ob,
                },
            ),
            Some(p) => self.mk(
                span,
                ty,
                ExprKind::IfLet {
                    pat: p,
                    value: Box::new(c),
                    then: tb,
                    otherwise: ob,
                },
            ),
        }
    }

    fn match_expr(
        &mut self,
        scrutinee: &ast::Expr,
        arms: &[ast::MatchArm],
        span: Span,
        expected: Option<&Ty>,
    ) -> Expr {
        let s = self.expr(scrutinee, None);
        let st = s.ty.clone();
        let mut acc: Option<Ty> = None;
        let mut out = Vec::new();
        for arm in arms {
            self.scope_push();
            let p = self.pat(&arm.pat, &st);
            let g = arm.guard.as_ref().map(|g| self.check(g, &Ty::Bool));
            let want = acc.clone().or_else(|| expected.cloned());
            let b = self.expr(&arm.body, want.as_ref());
            let bt = b.ty.clone();
            self.join(&mut acc, &bt, arm.body.span);
            self.scope_pop();
            out.push(hir::Arm {
                pat: p,
                guard: g,
                body: b,
            });
        }
        let ty = acc.unwrap_or(Ty::Never);
        self.mk(
            span,
            ty,
            ExprKind::Match {
                scrutinee: Box::new(s),
                arms: out,
            },
        )
    }

    fn loop_body(&mut self, body: &ast::Block, break_ty: Option<Ty>) -> (Block, bool) {
        self.loops_mut().push(LoopCx {
            break_ty,
            has_break: false,
        });
        let b = self.block(body, Some(&Ty::unit()));
        let lp = self.loops_mut().pop().unwrap();
        (b, lp.has_break)
    }

    fn for_expr(
        &mut self,
        pat: &ast::Pattern,
        iter: &ast::Expr,
        body: &ast::Block,
        span: Span,
    ) -> Expr {
        if let AE::Range {
            lo: Some(lo),
            hi: Some(hi),
            inclusive,
        } = &iter.kind
        {
            let l = self.expr(lo, None);
            let lt = l.ty.clone();
            let h = self.check(hi, &lt);
            self.deferred_op(OpCheck::Ord, "..", &lt, &lt, iter.span);
            if !matches!(self.tab.shallow(&lt), Ty::Int(_) | Ty::Infer(_) | Ty::Error) {
                let t = self.tys(&lt);
                self.err(
                    "T071",
                    iter.span,
                    format!("`for` over a range needs integer bounds, not `{t}`"),
                );
            } else if let Ty::Infer(v) = self.tab.shallow(&lt) {
                if self.tab.kind(v) != Some(VarKind::Int) {
                    let iv = self.tab.fresh(VarKind::Int);
                    let _ = self.tab.unify(&lt, &iv);
                }
            }
            self.scope_push();
            let p = self.pat(pat, &lt);
            let (b, _) = self.loop_body(body, None);
            self.scope_pop();
            return self.mk(
                span,
                Ty::unit(),
                ExprKind::ForRange {
                    pat: p,
                    lo: Box::new(l),
                    hi: Box::new(h),
                    inclusive: *inclusive,
                    body: b,
                },
            );
        }
        if let AE::Range { .. } = &iter.kind {
            self.err(
                "T071",
                iter.span,
                "`for` over a range needs both bounds in v0.1".into(),
            );
            return Expr::error(span);
        }
        let it = self.expr(iter, None);
        match self.tab.shallow(&it.ty) {
            Ty::Array(elem, _) => {
                self.scope_push();
                let p = self.pat(pat, &elem);
                let (b, _) = self.loop_body(body, None);
                self.scope_pop();
                self.mk(
                    span,
                    Ty::unit(),
                    ExprKind::ForArray {
                        pat: p,
                        array: Box::new(it),
                        body: b,
                    },
                )
            }
            Ty::Error => Expr::error(span),
            other => {
                let t = self.tys(&other);
                self.err(
                    "U015",
                    iter.span,
                    format!("iterating over `{t}` needs the standard library's Iterator trait (v0.1 iterates over integer ranges and arrays)"),
                );
                Expr::error(span)
            }
        }
    }

    // ------------------------------------------------------------ closures

    fn closure(
        &mut self,
        is_async: bool,
        params: &[(ast::Pattern, Option<ast::Type>)],
        body: &ast::Expr,
        span: Span,
        expected: Option<&Ty>,
    ) -> Expr {
        if is_async {
            self.err(
                "U011",
                span,
                "async closures are not implemented yet".into(),
            );
        }
        let exp = expected.and_then(|t| match self.tab.shallow(t) {
            Ty::Fn(p, r) if p.len() == params.len() => Some((p, *r)),
            _ => None,
        });
        let ret = exp
            .as_ref()
            .map(|(_, r)| r.clone())
            .unwrap_or_else(|| self.fresh());
        self.enter_closure(ret.clone());
        self.scope_push();
        let mut pats = Vec::new();
        let mut ptys = Vec::new();
        for (i, (p, t)) in params.iter().enumerate() {
            let pt = match t {
                Some(t) => {
                    let sc = self.scope.clone();
                    let lt = self.s.lower_ty(t, &sc);
                    if let Some((ep, _)) = &exp {
                        self.coerce(&ep[i].clone(), &lt, p.span);
                    }
                    lt
                }
                None => exp
                    .as_ref()
                    .map(|(ep, _)| ep[i].clone())
                    .unwrap_or_else(|| self.fresh()),
            };
            pats.push(self.pat(p, &pt));
            ptys.push(pt);
        }
        let b = self.check(body, &ret);
        self.scope_pop();
        let captures = self.leave_closure();
        let ty = Ty::Fn(ptys, Box::new(ret));
        self.mk(
            span,
            ty,
            ExprKind::Closure {
                params: pats,
                body: Box::new(b),
                captures,
            },
        )
    }

    // ------------------------------------------------------------ struct literals

    fn struct_lit(
        &mut self,
        path: &ast::Expr,
        fields: &[(String, Option<ast::Expr>)],
        base: Option<&ast::Expr>,
        span: Span,
    ) -> Expr {
        let res = match &path.kind {
            AE::Name(n) => match self.s.t.lookup(self.scope.module, n, false) {
                Ok(Some(b)) if self.s.def(b.def).kind == DefKind::Variant => {
                    let e = self.s.def(b.def).parent.unwrap();
                    let k = self.s.generics_of(e).params.len();
                    let a = (0..k).map(|_| self.fresh()).collect();
                    Some(PathRes::Variant(b.def, a))
                }
                _ => self.path_res(path),
            },
            _ => self.path_res(path),
        };
        let (fdefs, adt_ty, ctor): (
            Vec<crate::items::FieldDef>,
            Ty,
            Result<crate::defs::DefId, crate::defs::DefId>,
        ) = match res {
            Some(PathRes::Type(Ty::Adt(d, args))) if self.s.def(d).kind == DefKind::Struct => {
                let a = self.s.adt(d);
                let AdtKind::Struct(fs) = a.kind else {
                    unreachable!()
                };
                let fs = fs.into_iter().map(|mut f| {
                    f.ty = f.ty.subst(&a.params, &args);
                    f
                });
                (fs.collect(), Ty::Adt(d, args), Ok(d))
            }
            Some(PathRes::Variant(v, args)) => {
                let e = self.s.def(v).parent.unwrap();
                let a = self.s.adt(e);
                let AdtKind::Enum(vs) = a.kind else {
                    unreachable!()
                };
                let shape = vs.into_iter().find(|(d, _)| *d == v).unwrap().1;
                match shape {
                    VariantShape::Struct(fs) => {
                        let fs = fs.into_iter().map(|mut f| {
                            f.ty = f.ty.subst(&a.params, &args);
                            f
                        });
                        (fs.collect(), Ty::Adt(e, args), Err(v))
                    }
                    _ => {
                        let n = self.s.def(v).name.clone();
                        self.err(
                            "T043",
                            span,
                            format!(
                                "variant `{n}` has no named fields; construct it with `{n}(...)`"
                            ),
                        );
                        return Expr::error(span);
                    }
                }
            }
            Some(PathRes::Err) => return Expr::error(span),
            _ => {
                self.err(
                    "T043",
                    path.span,
                    "expected a struct or struct-like variant before `{`".into(),
                );
                return Expr::error(span);
            }
        };
        let owner_mod = match ctor {
            Ok(d) => self.s.def(d).module,
            Err(v) => self.s.def(v).module,
        };
        let mut vals: Vec<Option<Expr>> = vec![None; fdefs.len()];
        for (name, v) in fields {
            let Some(i) = fdefs.iter().position(|f| f.name == *name) else {
                let t = self.tys(&adt_ty);
                self.err("T041", span, format!("`{t}` has no field `{name}`"));
                if let Some(v) = v {
                    self.expr(v, None);
                }
                continue;
            };
            if ctor.is_ok() && !fdefs[i].is_pub && !self.s.t.is_within(self.scope.module, owner_mod)
            {
                self.err("R011", span, format!("field `{name}` is private"));
            }
            let fty = fdefs[i].ty.clone();
            let h = match v {
                Some(v) => self.check(v, &fty),
                None => {
                    let fake = ast::Expr {
                        span,
                        kind: AE::Name(name.clone()),
                    };
                    self.check(&fake, &fty)
                }
            };
            if vals[i].is_some() {
                self.err(
                    "T044",
                    span,
                    format!("field `{name}` is given more than once"),
                );
            }
            vals[i] = Some(h);
        }
        let b = base.map(|b| Box::new(self.check(b, &adt_ty)));
        if let (Err(_), Some(_)) = (ctor, &b) {
            self.err(
                "T045",
                span,
                "`..base` is only allowed in struct literals, not enum variants".into(),
            );
        }
        if b.is_none() {
            let missing: Vec<String> = fdefs
                .iter()
                .zip(&vals)
                .filter(|(_, v)| v.is_none())
                .map(|(f, _)| format!("`{}`", f.name))
                .collect();
            if !missing.is_empty() {
                self.err(
                    "T046",
                    span,
                    format!("missing field(s) {}", missing.join(", ")),
                );
            }
        }
        match ctor {
            Ok(d) => {
                let fs = vals
                    .into_iter()
                    .enumerate()
                    .filter_map(|(i, v)| v.map(|v| (i, v)))
                    .collect();
                self.mk(
                    span,
                    adt_ty,
                    ExprKind::Struct {
                        def: d,
                        fields: fs,
                        base: b,
                    },
                )
            }
            Err(v) => {
                let fs = vals
                    .into_iter()
                    .map(|x| x.unwrap_or_else(|| Expr::error(span)))
                    .collect();
                self.mk(
                    span,
                    adt_ty,
                    ExprKind::Variant {
                        variant: v,
                        fields: fs,
                    },
                )
            }
        }
    }

    // ------------------------------------------------------------ small helpers

    pub(crate) fn scope_push(&mut self) {
        self.push_scope_pub();
    }
    pub(crate) fn scope_pop(&mut self) {
        self.pop_scope_pub();
    }
}

/// Lossless `as` conversions (spec/semantics.md, section 4.4).
pub fn lossless(from: &Ty, to: &Ty) -> bool {
    use Ty::*;
    if from == to || matches!(from, Error) || matches!(to, Error) {
        return true;
    }
    match (from, to) {
        (Int(a), Int(b)) => {
            if a.signed() == b.signed() {
                b.bits() >= a.bits()
            } else {
                !a.signed() && b.bits() > a.bits()
            }
        }
        (Int(a), Float(f)) => {
            let mant = match f {
                FloatTy::F64 => 53,
                FloatTy::F32 => 24,
                FloatTy::F16 => 11,
                FloatTy::Bf16 => 8,
            };
            let value_bits = if a.signed() { a.bits() - 1 } else { a.bits() };
            value_bits <= mant
        }
        (Float(a), Float(b)) => matches!(
            (a, b),
            (FloatTy::F16, FloatTy::F32 | FloatTy::F64)
                | (FloatTy::Bf16, FloatTy::F32 | FloatTy::F64)
                | (FloatTy::F32, FloatTy::F64)
        ),
        (Bool, Int(_)) => true,
        (Char, Int(b)) => (if b.signed() { b.bits() - 1 } else { b.bits() }) >= 21,
        (Int(IntTy::U8), Char) => true,
        _ => false,
    }
}
