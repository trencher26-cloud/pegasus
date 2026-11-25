//! Pattern checking (spec/semantics.md, section 5).

use crate::defs::{DefId, DefKind};
use crate::hir::{Pat, PatKind};
use crate::items::{AdtKind, VariantShape};
use crate::lit::Lit;
use crate::ty::{ConstVal, IntTy, Ty};
use crate::typeck::{Cx, PathRes};
use pegasus_parser::ast::{self, ExprKind as AE, PatternKind, Span};
use std::collections::HashMap;

impl<'s, 'a> Cx<'s, 'a> {
    pub(crate) fn pat(&mut self, p: &ast::Pattern, expected: &Ty) -> Pat {
        let span = p.span;
        let mk = |ty: Ty, kind| Pat { span, ty, kind };
        match &p.kind {
            PatternKind::Wildcard => mk(expected.clone(), PatKind::Wild),
            PatternKind::Rest => {
                self.err(
                    "T073",
                    span,
                    "`..` is only allowed inside tuple, array, variant and struct patterns".into(),
                );
                mk(expected.clone(), PatKind::Wild)
            }
            PatternKind::Ident { var, name } => {
                if !*var {
                    if let Some(v) = self.contextual_variant(name, expected) {
                        let unit = matches!(
                            self.s.def(v).variant,
                            Some((
                                ast::Variant {
                                    kind: ast::VariantKind::Unit,
                                    ..
                                },
                                _
                            ))
                        );
                        if unit {
                            return self.variant_pat(v, None, &[], span, expected);
                        }
                    }
                    // Rule F: a name that resolves to a unit variant or a
                    // constant is a value pattern, not a new binding.
                    if let Ok(Some(b)) = self.s.t.lookup(self.scope.module, name, false) {
                        match self.s.def(b.def).kind {
                            DefKind::Variant => {
                                return self.variant_pat(b.def, None, &[], span, expected)
                            }
                            DefKind::Const => {
                                let ct = self.s.const_ty(b.def);
                                self.coerce(&ct, expected, span);
                                return mk(expected.clone(), PatKind::Const(b.def));
                            }
                            _ => {}
                        }
                    }
                }
                let l = self.bind_name(name, expected, *var, span);
                mk(expected.clone(), PatKind::Binding(l))
            }
            PatternKind::Literal(text) => {
                let (neg, body) = match text.strip_prefix('-') {
                    Some(rest) => (true, rest.trim()),
                    None => (false, text.as_str()),
                };
                let Some(lit) = crate::lit::parse(body) else {
                    return mk(Ty::Error, PatKind::Wild);
                };
                let t = self.lit_pat_ty(&lit, neg, span);
                self.coerce(&t, expected, span);
                mk(expected.clone(), PatKind::Lit { lit, neg })
            }
            PatternKind::Range { lo, hi, inclusive } => {
                let lo_v = lo.as_ref().map(|s| self.range_bound(s, expected, span));
                let hi_v = hi.as_ref().map(|s| self.range_bound(s, expected, span));
                let et = self.tab.shallow(expected);
                if !matches!(et, Ty::Int(_) | Ty::Char | Ty::Infer(_) | Ty::Error) {
                    let t = self.tys(&et);
                    self.err(
                        "T074",
                        span,
                        format!("range patterns need integer or char values, not `{t}`"),
                    );
                }
                if let (Some(Some(a)), Some(Some(b))) = (&lo_v, &hi_v) {
                    let (x, y) = (lit_num(a), lit_num(b));
                    if let (Some(x), Some(y)) = (x, y) {
                        if x > y || (!inclusive && x == y) {
                            self.err("T075", span, "this range pattern is empty".into());
                        }
                    }
                }
                mk(
                    expected.clone(),
                    PatKind::Range {
                        lo: lo_v.flatten(),
                        hi: hi_v.flatten(),
                        inclusive: *inclusive,
                    },
                )
            }
            PatternKind::Tuple(ps) => {
                let has_rest = ps.iter().any(|x| matches!(x.kind, PatternKind::Rest));
                let et = self.tab.shallow(expected);
                let elems: Vec<Ty> = match &et {
                    Ty::Tuple(v) => v.clone(),
                    Ty::Infer(_) if !has_rest => {
                        let v: Vec<Ty> = ps.iter().map(|_| self.fresh()).collect();
                        let _ = self.tab.unify(&et, &Ty::Tuple(v.clone()));
                        v
                    }
                    Ty::Error => return mk(Ty::Error, PatKind::Wild),
                    other => {
                        let t = self.tys(other);
                        self.err(
                            "T076",
                            span,
                            format!("tuple pattern cannot match a value of type `{t}`"),
                        );
                        return mk(Ty::Error, PatKind::Wild);
                    }
                };
                let subs = self.seq_pats(ps, &elems, span, "tuple");
                mk(expected.clone(), PatKind::Tuple(subs))
            }
            PatternKind::Array(ps) => {
                let et = self.tab.shallow(expected);
                let (elem, len) = match &et {
                    Ty::Array(e, n) => (
                        (**e).clone(),
                        match self.tab.shallow(n) {
                            Ty::Const(ConstVal::Int(n)) => n.to_usize(),
                            _ => None,
                        },
                    ),
                    Ty::Error => return mk(Ty::Error, PatKind::Wild),
                    Ty::Infer(_) => {
                        self.err(
                            "T050",
                            span,
                            "type annotations needed for this array pattern".into(),
                        );
                        return mk(Ty::Error, PatKind::Wild);
                    }
                    other => {
                        let t = self.tys(other);
                        self.err(
                            "T076",
                            span,
                            format!("array pattern cannot match a value of type `{t}`"),
                        );
                        return mk(Ty::Error, PatKind::Wild);
                    }
                };
                let rest_at = ps.iter().position(|x| matches!(x.kind, PatternKind::Rest));
                if ps
                    .iter()
                    .filter(|x| matches!(x.kind, PatternKind::Rest))
                    .count()
                    > 1
                {
                    self.err("T073", span, "at most one `..` per pattern".into());
                }
                let fixed = ps.len() - rest_at.map_or(0, |_| 1);
                if let Some(n) = len {
                    if (rest_at.is_none() && fixed != n) || (rest_at.is_some() && fixed > n) {
                        self.err(
                            "T077",
                            span,
                            format!("array pattern has {fixed} element(s), but the array has {n}"),
                        );
                    }
                }
                let (bp, ap): (&[ast::Pattern], &[ast::Pattern]) = match rest_at {
                    Some(i) => (&ps[..i], &ps[i + 1..]),
                    None => (ps, &[]),
                };
                let before = bp.iter().map(|x| self.pat(x, &elem)).collect();
                let after = ap.iter().map(|x| self.pat(x, &elem)).collect();
                mk(
                    expected.clone(),
                    PatKind::Array {
                        before,
                        rest: rest_at.is_some(),
                        after,
                    },
                )
            }
            PatternKind::Variant { path, elems }
                if path.len() == 1 && self.contextual_variant(&path[0], expected).is_some() =>
            {
                let v = self.contextual_variant(&path[0], expected).unwrap();
                self.variant_pat(v, Some(elems), &[], span, expected)
            }
            PatternKind::Variant { path, elems } => match self.pat_path(path, span) {
                Some(PathRes::Variant(v, _)) => {
                    self.variant_pat(v, Some(elems), &[], span, expected)
                }
                Some(PathRes::Err) => mk(Ty::Error, PatKind::Wild),
                _ => {
                    self.err(
                        "T078",
                        span,
                        format!("`{}` is not an enum variant", path.join(".")),
                    );
                    mk(Ty::Error, PatKind::Wild)
                }
            },
            PatternKind::Struct { path, fields, rest } => {
                self.struct_pat(path, fields, *rest, span, expected)
            }
            PatternKind::Or(alts) => {
                let saved = std::mem::take(self.or_state().0);
                let saved_seen = std::mem::take(self.or_state().1);
                let first_local = self.locals.len();
                let mut pats = vec![self.pat(&alts[0], expected)];
                let map: HashMap<String, u32> = (first_local..self.locals.len())
                    .map(|i| (self.locals[i].name.clone(), i as u32))
                    .collect();
                for alt in &alts[1..] {
                    *self.or_state().0 = Some(map.clone());
                    self.or_state().1.clear();
                    pats.push(self.pat(alt, expected));
                    let seen = self.or_state().1.clone();
                    for n in map.keys() {
                        if !seen.contains(n) {
                            self.err(
                                "T072",
                                alt.span,
                                format!("variable `{n}` is not bound in every alternative"),
                            );
                        }
                    }
                }
                *self.or_state().0 = saved;
                *self.or_state().1 = saved_seen;
                mk(expected.clone(), PatKind::Or(pats))
            }
        }
    }

    /// Rule F and the Phase 0 examples: an unqualified name in a pattern
    /// first resolves among the variants of the expected enum type.
    fn contextual_variant(&mut self, name: &str, expected: &Ty) -> Option<DefId> {
        match self.tab.shallow(expected) {
            Ty::Adt(d, _) if self.s.def(d).kind == DefKind::Enum => self.s.t.variants[&d]
                .iter()
                .copied()
                .find(|&v| self.s.def(v).name == name),
            _ => None,
        }
    }

    fn bind_name(&mut self, name: &str, ty: &Ty, var: bool, span: Span) -> u32 {
        let (map, seen, mutable) = self.or_state();
        let mutable = mutable || var;
        if let Some(m) = map {
            if let Some(&l) = m.get(name) {
                seen.push(name.to_string());
                let lt = self.locals[l as usize].ty.clone();
                if self.tab.unify(&lt, ty).is_err() {
                    let (a, b) = (self.tys(&lt), self.tys(ty));
                    self.err("T072", span, format!("variable `{name}` has type `{a}` in one alternative and `{b}` in another"));
                }
                return l;
            }
            self.err(
                "T072",
                span,
                format!("variable `{name}` is not bound in every alternative"),
            );
        }
        self.new_local(name, ty.clone(), mutable, span)
    }

    fn lit_pat_ty(&mut self, lit: &Lit, neg: bool, span: Span) -> Ty {
        match lit {
            Lit::Int(v, suf) => {
                let t = match suf {
                    Some(s) => Ty::Int(*s),
                    None => self.tab.fresh(crate::infer::VarKind::Int),
                };
                self.int_lits.push((t.clone(), *v, neg, span));
                if neg {
                    self.deferred_op(crate::typeck::OpCheck::Neg, "-", &t, &t, span);
                }
                t
            }
            Lit::Float(..) => {
                self.err(
                    "T079",
                    span,
                    "floating-point literals cannot be used in patterns".into(),
                );
                Ty::Error
            }
            Lit::Bool(_) => Ty::Bool,
            Lit::Char(_) => Ty::Char,
            Lit::Str(_) => Ty::Str,
            Lit::Bytes(b) => Ty::Array(
                Box::new(Ty::Int(IntTy::U8)),
                Box::new(Ty::Const(ConstVal::Int(crate::ty::CInt::from_usize(
                    b.len(),
                )))),
            ),
        }
    }

    fn range_bound(&mut self, s: &str, expected: &Ty, span: Span) -> Option<(Lit, bool)> {
        let (neg, body) = match s.strip_prefix('-') {
            Some(r) => (true, r.trim()),
            None => (false, s),
        };
        if let Some(lit) = crate::lit::parse(body) {
            let t = self.lit_pat_ty(&lit, neg, span);
            self.coerce(&t, expected, span);
            return Some((lit, neg));
        }
        // A named constant.
        let path: Vec<String> = body.split('.').map(String::from).collect();
        let d = if path.len() == 1 {
            match self.s.t.lookup(self.scope.module, &path[0], false) {
                Ok(Some(b)) => Some(b.def),
                _ => None,
            }
        } else {
            match self.pat_path(&path, span) {
                Some(PathRes::Item(d)) => Some(d),
                _ => None,
            }
        };
        match d {
            Some(d) if self.s.def(d).kind == DefKind::Const => {
                let ct = self.s.const_ty(d);
                self.coerce(&ct, expected, span);
                match self.s.const_value(d) {
                    Some(ConstVal::Int(v)) => Some((Lit::Int(v.mag, None), v.neg)),
                    _ => {
                        self.err(
                            "U007",
                            span,
                            format!("the value of `{body}` is not known at compile time yet"),
                        );
                        None
                    }
                }
            }
            _ => {
                self.err(
                    "R009",
                    span,
                    format!("cannot find constant `{body}` for this range pattern"),
                );
                None
            }
        }
    }

    fn pat_path(&mut self, path: &[String], span: Span) -> Option<PathRes> {
        if path.len() == 1 {
            return match self.s.t.lookup(self.scope.module, &path[0], false) {
                Ok(Some(b)) if self.s.def(b.def).kind == DefKind::Variant => {
                    let e = self.s.def(b.def).parent.unwrap();
                    let n = self.s.generics_of(e).params.len();
                    let a = (0..n).map(|_| self.fresh()).collect();
                    Some(PathRes::Variant(b.def, a))
                }
                Ok(Some(b)) => Some(PathRes::Item(b.def)),
                _ => match self.s.t.lookup(self.scope.module, &path[0], true) {
                    Ok(Some(b)) => {
                        let e = ast::Expr {
                            span,
                            kind: AE::Name(path[0].clone()),
                        };
                        let _ = b;
                        self.path_res(&e)
                    }
                    _ => {
                        self.err(
                            "R009",
                            span,
                            format!("cannot find `{}` in this scope", path[0]),
                        );
                        Some(PathRes::Err)
                    }
                },
            };
        }
        let mut e = ast::Expr {
            span,
            kind: AE::Name(path[0].clone()),
        };
        if path[0] == "Self" {
            e.kind = AE::SelfValue("Self".into());
        }
        for s in &path[1..] {
            e = ast::Expr {
                span,
                kind: AE::Field {
                    recv: Box::new(e),
                    name: s.clone(),
                    args: None,
                },
            };
        }
        let r = self.path_res(&e);
        if r.is_none() {
            self.err(
                "R009",
                span,
                format!("cannot find `{}` in this scope", path.join(".")),
            );
            return Some(PathRes::Err);
        }
        r
    }

    fn seq_pats(&mut self, ps: &[ast::Pattern], tys: &[Ty], span: Span, what: &str) -> Vec<Pat> {
        let rest_at = ps.iter().position(|x| matches!(x.kind, PatternKind::Rest));
        let fixed = ps.len() - rest_at.map_or(0, |_| 1);
        if (rest_at.is_none() && fixed != tys.len()) || fixed > tys.len() {
            self.err(
                "T077",
                span,
                format!(
                    "{what} pattern has {fixed} element(s), but the value has {}",
                    tys.len()
                ),
            );
            return tys
                .iter()
                .map(|t| Pat {
                    span,
                    ty: t.clone(),
                    kind: PatKind::Wild,
                })
                .collect();
        }
        let mut out = Vec::new();
        let mut ti = 0;
        for (i, p) in ps.iter().enumerate() {
            if Some(i) == rest_at {
                let skip = tys.len() - fixed;
                for _ in 0..skip {
                    out.push(Pat {
                        span: p.span,
                        ty: tys[ti].clone(),
                        kind: PatKind::Wild,
                    });
                    ti += 1;
                }
                continue;
            }
            out.push(self.pat(p, &tys[ti]));
            ti += 1;
        }
        out
    }

    fn variant_pat(
        &mut self,
        v: DefId,
        elems: Option<&Vec<ast::Pattern>>,
        _f: &[()],
        span: Span,
        expected: &Ty,
    ) -> Pat {
        let e = self.s.def(v).parent.unwrap();
        let g = self.s.generics_of(e);
        let args: Vec<Ty> = g.params.iter().map(|_| self.fresh()).collect();
        let et = Ty::Adt(e, args.clone());
        self.coerce(&et, expected, span);
        let a = self.s.adt(e);
        let AdtKind::Enum(vs) = &a.kind else {
            unreachable!()
        };
        let shape = vs.iter().find(|(d, _)| *d == v).unwrap().1.clone();
        let n = self.s.def(v).name.clone();
        let fields = match (shape, elems) {
            (VariantShape::Unit, None) => Vec::new(),
            (VariantShape::Unit, Some(_)) => {
                self.err("T078", span, format!("variant `{n}` has no payload"));
                Vec::new()
            }
            (VariantShape::Tuple(tys), Some(ps)) => {
                let tys: Vec<Ty> = tys.iter().map(|t| t.subst(&a.params, &args)).collect();
                self.seq_pats(ps, &tys, span, "variant")
            }
            (VariantShape::Tuple(tys), None) => {
                self.err(
                    "T078",
                    span,
                    format!("variant `{n}` has a payload: write `{n}(..)`"),
                );
                tys.iter()
                    .map(|t| Pat {
                        span,
                        ty: t.subst(&a.params, &args),
                        kind: PatKind::Wild,
                    })
                    .collect()
            }
            (VariantShape::Struct(fs), _) => {
                self.err(
                    "T078",
                    span,
                    format!("variant `{n}` has named fields: write `{n} {{ .. }}`"),
                );
                fs.iter()
                    .map(|f| Pat {
                        span,
                        ty: f.ty.subst(&a.params, &args),
                        kind: PatKind::Wild,
                    })
                    .collect()
            }
        };
        Pat {
            span,
            ty: expected.clone(),
            kind: PatKind::Variant { variant: v, fields },
        }
    }

    fn struct_pat(
        &mut self,
        path: &[String],
        fields: &[(String, Option<ast::Pattern>)],
        rest: bool,
        span: Span,
        expected: &Ty,
    ) -> Pat {
        let contextual = if path.len() == 1 {
            self.contextual_variant(&path[0], expected)
        } else {
            None
        };
        let res = if let Some(v) = contextual {
            let e = self.s.def(v).parent.unwrap();
            let k = self.s.generics_of(e).params.len();
            let a = (0..k).map(|_| self.fresh()).collect();
            Some(PathRes::Variant(v, a))
        } else if path.len() == 1 {
            match self.s.t.lookup(self.scope.module, &path[0], false) {
                Ok(Some(b)) if self.s.def(b.def).kind == DefKind::Variant => {
                    self.pat_path(path, span)
                }
                _ => {
                    let e = if path[0] == "Self" {
                        ast::Expr {
                            span,
                            kind: AE::SelfValue("Self".into()),
                        }
                    } else {
                        ast::Expr {
                            span,
                            kind: AE::Name(path[0].clone()),
                        }
                    };
                    self.path_res(&e)
                }
            }
        } else {
            self.pat_path(path, span)
        };
        let (fdefs, ty, kind_ctor): (Vec<crate::items::FieldDef>, Ty, Result<DefId, DefId>) =
            match res {
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
                    match vs.into_iter().find(|(d, _)| *d == v).unwrap().1 {
                        VariantShape::Struct(fs) => {
                            let fs = fs.into_iter().map(|mut f| {
                                f.ty = f.ty.subst(&a.params, &args);
                                f
                            });
                            (fs.collect(), Ty::Adt(e, args), Err(v))
                        }
                        _ => {
                            self.err(
                                "T078",
                                span,
                                format!("variant `{}` has no named fields", self.s.def(v).name),
                            );
                            return Pat {
                                span,
                                ty: Ty::Error,
                                kind: PatKind::Wild,
                            };
                        }
                    }
                }
                Some(PathRes::Err) => {
                    return Pat {
                        span,
                        ty: Ty::Error,
                        kind: PatKind::Wild,
                    }
                }
                _ => {
                    self.err(
                        "T078",
                        span,
                        format!(
                            "`{}` is not a struct or struct-like variant",
                            path.join(".")
                        ),
                    );
                    return Pat {
                        span,
                        ty: Ty::Error,
                        kind: PatKind::Wild,
                    };
                }
            };
        self.coerce(&ty, expected, span);
        let mut subs: Vec<Option<Pat>> = vec![None; fdefs.len()];
        let owner_mod = match kind_ctor {
            Ok(d) => self.s.def(d).module,
            Err(v) => self.s.def(v).module,
        };
        for (name, p) in fields {
            if name == ".." {
                continue;
            }
            let Some(i) = fdefs.iter().position(|f| f.name == *name) else {
                let t = self.tys(&ty);
                self.err("T041", span, format!("`{t}` has no field `{name}`"));
                continue;
            };
            if kind_ctor.is_ok()
                && !fdefs[i].is_pub
                && !self.s.t.is_within(self.scope.module, owner_mod)
            {
                self.err("R011", span, format!("field `{name}` is private"));
            }
            let ft = fdefs[i].ty.clone();
            let sp = match p {
                Some(p) => self.pat(p, &ft),
                None => {
                    let l = self.bind_name(name, &ft, false, span);
                    Pat {
                        span,
                        ty: ft.clone(),
                        kind: PatKind::Binding(l),
                    }
                }
            };
            subs[i] = Some(sp);
        }
        if !rest {
            let missing: Vec<String> = fdefs
                .iter()
                .zip(&subs)
                .filter(|(_, s)| s.is_none())
                .map(|(f, _)| format!("`{}`", f.name))
                .collect();
            if !missing.is_empty() {
                self.err(
                    "T048",
                    span,
                    format!(
                        "pattern does not mention field(s) {} (add `..` to ignore them)",
                        missing.join(", ")
                    ),
                );
            }
        }
        let subs: Vec<Pat> = subs
            .into_iter()
            .zip(&fdefs)
            .map(|(s, f)| {
                s.unwrap_or(Pat {
                    span,
                    ty: f.ty.clone(),
                    kind: PatKind::Wild,
                })
            })
            .collect();
        match kind_ctor {
            Ok(d) => Pat {
                span,
                ty: expected.clone(),
                kind: PatKind::Struct {
                    def: d,
                    fields: subs,
                },
            },
            Err(v) => Pat {
                span,
                ty: expected.clone(),
                kind: PatKind::Variant {
                    variant: v,
                    fields: subs,
                },
            },
        }
    }
}

fn lit_num(l: &(Lit, bool)) -> Option<i128> {
    match l {
        (Lit::Int(v, _), neg) => {
            let v = i128::try_from(*v).ok()?;
            Some(if *neg { -v } else { v })
        }
        (Lit::Char(c), _) => Some(*c as i128),
        _ => None,
    }
}
