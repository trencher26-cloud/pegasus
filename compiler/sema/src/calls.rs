//! Paths in expressions, calls, method resolution and argument checking.

use crate::defs::{DefId, DefKind};
use crate::hir::{Expr, ExprKind};
use crate::items::{AdtKind, FnSigTy, Mode, VariantShape};
use crate::ty::Ty;
use crate::typeck::{Cx, PathRes};
use pegasus_parser::ast::{self, ExprKind as AE, Span};

impl<'s, 'a> Cx<'s, 'a> {
    /// Resolve `e` as a path if it is one. None: `e` denotes a value.
    pub(crate) fn path_res(&mut self, e: &ast::Expr) -> Option<PathRes> {
        let span = e.span;
        match &e.kind {
            AE::Name(n) => {
                if self.lookup_local(n).is_some() {
                    return None;
                }
                if let Some(&p) = self
                    .scope
                    .generics
                    .iter()
                    .rev()
                    .find(|&&p| self.s.def(p).name == *n)
                {
                    return Some(PathRes::Type(Ty::Param(p)));
                }
                let m = self.scope.module;
                if let Ok(Some(_)) = self.s.t.lookup(m, n, false) {
                    return None;
                }
                match self.s.t.lookup(m, n, true) {
                    Ok(Some(b)) => Some(self.type_ns_res(b.def, &[], span)),
                    Err(_) => {
                        self.err(
                            "R005",
                            span,
                            format!("`{n}` is ambiguous (several glob imports provide it)"),
                        );
                        Some(PathRes::Err)
                    }
                    Ok(None) if n == "core" => Some(PathRes::Module(self.s.t.core_root)),
                    Ok(None) => None,
                }
            }
            AE::SelfValue(s) if s == "Self" => {
                match self.scope.self_ty.clone() {
                    Some(t) => Some(PathRes::Type(t)),
                    None => {
                        self.err("R007", span, "`Self` is only available inside impl, trait, struct and enum declarations".into());
                        Some(PathRes::Err)
                    }
                }
            }
            AE::GenericName { name, args } => {
                let m = self.scope.module;
                if let Ok(Some(_)) = self.s.t.lookup(m, name, false) {
                    return None;
                }
                match self.s.t.lookup(m, name, true) {
                    Ok(Some(b)) => Some(self.type_ns_res(b.def, args, span)),
                    _ => None,
                }
            }
            AE::Field { recv, name, args } => {
                let base = self.path_res(recv)?;
                Some(match base {
                    PathRes::Err => PathRes::Err,
                    PathRes::Module(m) => {
                        self.module_member(m, name, args.as_deref().unwrap_or(&[]), span)
                    }
                    PathRes::Type(t) => self.type_member(&t, name, span),
                    PathRes::Item(_) | PathRes::Variant(..) | PathRes::Assoc(..) => return None,
                })
            }
            _ => None,
        }
    }

    fn type_ns_res(&mut self, d: DefId, args: &[ast::GenericArg], span: Span) -> PathRes {
        match self.s.def(d).kind {
            DefKind::Mod => PathRes::Module(d),
            DefKind::Struct | DefKind::Enum => {
                let g = self.s.generics_of(d);
                let targs: Vec<Ty> = if args.is_empty() {
                    g.params.iter().map(|_| self.fresh()).collect()
                } else {
                    let sc = self.scope.clone();
                    let v: Vec<Ty> = args
                        .iter()
                        .map(|a| self.s.lower_garg(a, &sc, span))
                        .collect();
                    if v.len() != g.params.len() {
                        let n = self.s.def(d).name.clone();
                        self.err(
                            "T011",
                            span,
                            format!(
                                "`{n}` takes {} generic argument(s), found {}",
                                g.params.len(),
                                v.len()
                            ),
                        );
                        return PathRes::Err;
                    }
                    v
                };
                PathRes::Type(Ty::Adt(d, targs))
            }
            DefKind::Prim => {
                PathRes::Type(crate::items::Sema::prim_ty(&self.s.def(d).name).unwrap())
            }
            DefKind::TypeAlias => {
                let ty = ast::Type {
                    span,
                    text: self.s.def(d).name.clone(),
                    kind: ast::TypeKind::Path {
                        path: vec![self.s.def(d).name.clone()],
                        args: args.to_vec(),
                    },
                };
                let sc = self.scope.clone();
                PathRes::Type(self.s.lower_ty(&ty, &sc))
            }
            k => {
                let n = self.s.def(d).name.clone();
                self.err(
                    "T054",
                    span,
                    format!("expected a value or type, found {} `{n}`", k.describe()),
                );
                PathRes::Err
            }
        }
    }

    fn module_member(
        &mut self,
        m: DefId,
        name: &str,
        args: &[ast::GenericArg],
        span: Span,
    ) -> PathRes {
        let from = self.scope.module;
        let (tr, vr) = self.s.t.member(m, name);
        if let Ok(Some(b)) = vr {
            if !self.s.t.binding_visible(&b, m, from) {
                self.err("R004", span, format!("`{name}` is private"));
                return PathRes::Err;
            }
            return match self.s.def(b.def).kind {
                DefKind::Variant => {
                    let e = self.s.def(b.def).parent.unwrap();
                    let n = self.s.generics_of(e).params.len();
                    let a = (0..n).map(|_| self.fresh()).collect();
                    PathRes::Variant(b.def, a)
                }
                _ => PathRes::Item(b.def),
            };
        }
        if let Ok(Some(b)) = tr {
            if !self.s.t.binding_visible(&b, m, from) {
                self.err("R004", span, format!("`{name}` is private"));
                return PathRes::Err;
            }
            return self.type_ns_res(b.def, args, span);
        }
        if vr.is_err() || tr.is_err() {
            self.err(
                "R005",
                span,
                format!("`{name}` is ambiguous (several glob imports provide it)"),
            );
            return PathRes::Err;
        }
        let mn = self.s.def(m).name.clone();
        self.err(
            "R010",
            span,
            format!("cannot find `{name}` in module `{mn}`"),
        );
        PathRes::Err
    }

    fn type_member(&mut self, t: &Ty, name: &str, span: Span) -> PathRes {
        if let Ty::Adt(d, args) = t {
            if self.s.def(*d).kind == DefKind::Enum {
                if let Some(&v) = self.s.t.variants[d]
                    .iter()
                    .find(|&&v| self.s.def(v).name == name)
                {
                    return PathRes::Variant(v, args.clone());
                }
            }
        }
        match self.lookup_assoc(t, name, span) {
            Some((m, pargs)) => PathRes::Assoc(m, pargs),
            None => {
                let ts = self.tys(t);
                self.err(
                    "T053",
                    span,
                    format!("no associated item named `{name}` for type `{ts}`"),
                );
                PathRes::Err
            }
        }
    }

    // ------------------------------------------------------------ method lookup

    /// Candidates for `name` on `recv` (already shallow-resolved): returns the
    /// member and the arguments for its parent's generics (impl params, or
    /// [Self, trait args...] for trait methods).
    pub(crate) fn lookup_assoc(
        &mut self,
        recv: &Ty,
        name: &str,
        span: Span,
    ) -> Option<(DefId, Vec<Ty>)> {
        let recv = self.tab.shallow(recv);
        if let Ty::Param(p) = recv {
            let mut found: Vec<(DefId, Vec<Ty>)> = Vec::new();
            for b in self.bounds.clone() {
                if b.ty != Ty::Param(p) {
                    continue;
                }
                if let Some(m) = self.trait_member(b.trait_, name) {
                    let mut a = vec![recv.clone()];
                    a.extend(b.args.iter().cloned());
                    if !found.iter().any(|(x, _)| *x == m) {
                        found.push((m, a));
                    }
                }
            }
            return self.pick(found, name, span);
        }
        if matches!(recv, Ty::Error) {
            return None;
        }
        // Inherent impls first.
        let mut found = Vec::new();
        if let Ty::Adt(d, _) = &recv {
            for imp in self.s.inherent.get(d).cloned().unwrap_or_default() {
                if let Some(m) = self.impl_member(imp, name) {
                    if let Some(a) = self.impl_applies(imp, &recv) {
                        found.push((m, a));
                    }
                }
            }
        }
        if !found.is_empty() {
            return self.pick(found, name, span);
        }
        // Then trait impls (and the trait's default methods).
        let traits: Vec<DefId> = self.s.trait_impls.keys().copied().collect();
        for tr in traits {
            let Some(tm) = self.trait_member(tr, name) else {
                continue;
            };
            for imp in self.s.trait_impls[&tr].clone() {
                if let Some(a) = self.impl_applies(imp, &recv) {
                    match self.impl_member(imp, name) {
                        Some(m) => found.push((m, a)),
                        None => {
                            let (_, targs) = self.s.impls[&imp].trait_ref.clone().unwrap();
                            let g = self.s.generics_of(imp);
                            let mut ta = vec![recv.clone()];
                            ta.extend(targs.iter().map(|t| t.subst(&g.params, &a)));
                            found.push((tm, ta));
                        }
                    }
                }
            }
        }
        self.pick(found, name, span)
    }

    fn pick(
        &mut self,
        found: Vec<(DefId, Vec<Ty>)>,
        name: &str,
        span: Span,
    ) -> Option<(DefId, Vec<Ty>)> {
        match found.len() {
            0 => None,
            1 => found.into_iter().next(),
            _ => {
                self.err(
                    "T052",
                    span,
                    format!("`{name}` is ambiguous: several traits provide it for this type"),
                );
                None
            }
        }
    }

    fn impl_member(&self, imp: DefId, name: &str) -> Option<DefId> {
        self.s
            .t
            .members
            .get(&imp)?
            .iter()
            .copied()
            .find(|&m| self.s.def(m).name == name && self.s.def(m).kind == DefKind::Fn)
    }

    fn trait_member(&mut self, tr: DefId, name: &str) -> Option<DefId> {
        if let Some(m) = self
            .s
            .t
            .members
            .get(&tr)?
            .iter()
            .copied()
            .find(|&m| self.s.def(m).name == name && self.s.def(m).kind == DefKind::Fn)
        {
            return Some(m);
        }
        None
    }

    /// If impl `imp` applies to `recv`, instantiate its parameters (binding
    /// them in the inference table) and return them.
    fn impl_applies(&mut self, imp: DefId, recv: &Ty) -> Option<Vec<Ty>> {
        let g = self.s.generics_of(imp);
        let vars: Vec<Ty> = g.params.iter().map(|_| self.fresh()).collect();
        let st = self.s.impls.get(&imp)?.self_ty.subst(&g.params, &vars);
        if st.has_error() {
            return None;
        }
        if self.tab.probe(&st, recv) {
            Some(vars)
        } else {
            None
        }
    }

    // ------------------------------------------------------------ instantiation

    /// Instantiate a function's generics: parent arguments given (or fresh),
    /// own arguments explicit (or fresh). Records the bounds as obligations.
    pub(crate) fn instantiate(
        &mut self,
        d: DefId,
        parent_args: Option<Vec<Ty>>,
        explicit: Option<Vec<Ty>>,
        span: Span,
    ) -> (FnSigTy, Vec<Ty>) {
        let sig = self.s.sig(d);
        let n_parent = sig.generics.len() - sig.own_generics.len();
        let mut args: Vec<Ty> = match parent_args {
            Some(a) if a.len() == n_parent => a,
            _ => (0..n_parent).map(|_| self.fresh()).collect(),
        };
        match explicit {
            Some(e) => {
                if e.len() != sig.own_generics.len() {
                    let n = self.s.def(d).name.clone();
                    self.err(
                        "T011",
                        span,
                        format!(
                            "`{n}` takes {} generic argument(s), found {}",
                            sig.own_generics.len(),
                            e.len()
                        ),
                    );
                    args.extend(sig.own_generics.iter().map(|_| Ty::Error));
                } else {
                    args.extend(e);
                }
            }
            None => args.extend(
                sig.own_generics
                    .iter()
                    .map(|_| self.tab.fresh(crate::infer::VarKind::General)),
            ),
        }
        let g = self.s.generics_of(d);
        let mut bounds = g.bounds.clone();
        // A trait method called through the trait needs `Self: Trait`.
        if let Some(p) = self.s.def(d).parent {
            if self.s.def(p).kind == DefKind::Trait {
                let tg = self.s.generics_of(p);
                bounds.push(crate::items::Bound {
                    ty: Ty::Param(tg.params[0]),
                    trait_: p,
                    args: tg.params[1..].iter().map(|q| Ty::Param(*q)).collect(),
                });
            }
        }
        for b in bounds {
            self.obligations.push((
                b.ty.subst(&sig.generics, &args),
                b.trait_,
                b.args
                    .iter()
                    .map(|t| t.subst(&sig.generics, &args))
                    .collect(),
                span,
            ));
        }
        let mut s = sig.clone();
        for p in s.inputs.iter_mut() {
            p.ty = p.ty.subst(&sig.generics, &args);
        }
        s.ret = s.ret.subst(&sig.generics, &args);
        (s, args)
    }

    fn fn_value(
        &mut self,
        d: DefId,
        parent: Option<Vec<Ty>>,
        explicit: Option<Vec<Ty>>,
        span: Span,
    ) -> (Expr, FnSigTy) {
        let (sig, args) = self.instantiate(d, parent, explicit, span);
        let ty = Ty::Fn(
            sig.inputs.iter().map(|p| p.ty.clone()).collect(),
            Box::new(sig.ret.clone()),
        );
        (
            Expr {
                span,
                ty,
                kind: ExprKind::FnRef(d, args),
            },
            sig,
        )
    }

    fn explicit_args(
        &mut self,
        args: &Option<Vec<ast::GenericArg>>,
        span: Span,
    ) -> Option<Vec<Ty>> {
        let sc = self.scope.clone();
        args.as_ref()
            .map(|v| v.iter().map(|a| self.s.lower_garg(a, &sc, span)).collect())
    }

    fn variant_ctor_value(&mut self, v: DefId, args: Vec<Ty>, span: Span) -> Expr {
        let e = self.s.def(v).parent.unwrap();
        let a = self.s.adt(e);
        let AdtKind::Enum(vs) = &a.kind else {
            unreachable!()
        };
        let shape = vs.iter().find(|(d, _)| *d == v).unwrap().1.clone();
        let n = self.s.def(v).name.clone();
        match shape {
            VariantShape::Unit => Expr {
                span,
                ty: Ty::Adt(e, args),
                kind: ExprKind::Variant {
                    variant: v,
                    fields: Vec::new(),
                },
            },
            VariantShape::Tuple(_) => {
                self.err("U008", span, format!("variant constructors as function values are not implemented yet; call it: `{n}(...)`"));
                Expr::error(span)
            }
            VariantShape::Struct(_) => {
                self.err(
                    "T043",
                    span,
                    format!("variant `{n}` has named fields; construct it with `{n} {{ ... }}`"),
                );
                Expr::error(span)
            }
        }
    }

    /// A function used as a value (not called): function types carry no
    /// parameter modes and no borrowed return, so only functions whose
    /// parameters are all `in` and whose result is owned qualify.
    fn as_fn_value(&mut self, v: (Expr, FnSigTy), span: Span) -> Expr {
        let (e, sig) = v;
        let bad = sig.inputs.iter().find(|p| p.mode != Mode::In);
        if bad.is_some() || sig.ret_borrow {
            let what = match bad {
                Some(p) if p.mode == Mode::Inout => {
                    format!("its parameter `{}` is `inout`", p.name)
                }
                Some(p) => format!("its parameter `{}` is `own`", p.name),
                None => "it returns a borrow (`-> in`)".to_string(),
            };
            self.err(
                "T065",
                span,
                format!("this function cannot be used as a function value: {what}, and function types carry no parameter or return modes"),
            );
            return Expr::error(span);
        }
        e
    }

    fn item_value(&mut self, d: DefId, explicit: Option<Vec<Ty>>, span: Span) -> Expr {
        match self.s.def(d).kind {
            DefKind::Fn | DefKind::ExternFn => {
                let v = self.fn_value(d, None, explicit, span);
                self.as_fn_value(v, span)
            }
            DefKind::Const => {
                let ty = self.s.const_ty(d);
                Expr {
                    span,
                    ty,
                    kind: ExprKind::Const(d),
                }
            }
            DefKind::Variant => {
                let e = self.s.def(d).parent.unwrap();
                let n = self.s.generics_of(e).params.len();
                let a = (0..n).map(|_| self.fresh()).collect();
                self.variant_ctor_value(d, a, span)
            }
            k => {
                let n = self.s.def(d).name.clone();
                self.err(
                    "T054",
                    span,
                    format!("expected a value, found {} `{n}`", k.describe()),
                );
                Expr::error(span)
            }
        }
    }

    /// Names, dotted paths and field accesses.
    pub(crate) fn path_or_field(&mut self, e: &ast::Expr, _expected: Option<&Ty>) -> Expr {
        let span = e.span;
        match &e.kind {
            AE::Name(n) | AE::SelfValue(n) if n != "Self" => {
                if let Some(l) = self.lookup_local(n) {
                    let ty = self.locals[l as usize].ty.clone();
                    return Expr {
                        span,
                        ty,
                        kind: ExprKind::Local(l),
                    };
                }
                if n == "self" {
                    self.err(
                        "R008",
                        span,
                        "`self` is only available in methods that take `self`".into(),
                    );
                    return Expr::error(span);
                }
                let m = self.scope.module;
                match self.s.t.lookup(m, n, false) {
                    Ok(Some(b)) => return self.item_value(b.def, None, span),
                    Err(_) => {
                        self.err(
                            "R005",
                            span,
                            format!("`{n}` is ambiguous (several glob imports provide it)"),
                        );
                        return Expr::error(span);
                    }
                    Ok(None) => {}
                }
                if let Some(pr) = self.path_res(e) {
                    return self.res_value(pr, span);
                }
                self.err(
                    "R009",
                    span,
                    format!("cannot find value `{n}` in this scope"),
                );
                Expr::error(span)
            }
            AE::GenericName { name, args } => {
                let m = self.scope.module;
                match self.s.t.lookup(m, name, false) {
                    Ok(Some(b))
                        if matches!(self.s.def(b.def).kind, DefKind::Fn | DefKind::ExternFn) =>
                    {
                        let ex = self.explicit_args(&Some(args.clone()), span);
                        self.item_value(b.def, ex, span)
                    }
                    _ => match self.path_res(e) {
                        Some(pr) => self.res_value(pr, span),
                        None => {
                            self.err(
                                "R009",
                                span,
                                format!("cannot find function `{name}` in this scope"),
                            );
                            Expr::error(span)
                        }
                    },
                }
            }
            AE::Field { recv, name, args } => {
                if let Some(pr) = self.path_res(e) {
                    if let (PathRes::Item(d), Some(_)) = (&pr, args) {
                        let ex = self.explicit_args(args, span);
                        return self.item_value(*d, ex, span);
                    }
                    return self.res_value(pr, span);
                }
                let r = self.expr(recv, None);
                self.field_access(r, name, span)
            }
            _ => {
                // `Self` alone as a value.
                if let Some(pr) = self.path_res(e) {
                    return self.res_value(pr, span);
                }
                Expr::error(span)
            }
        }
    }

    fn res_value(&mut self, pr: PathRes, span: Span) -> Expr {
        match pr {
            PathRes::Err => Expr::error(span),
            PathRes::Item(d) => self.item_value(d, None, span),
            PathRes::Variant(v, a) => self.variant_ctor_value(v, a, span),
            PathRes::Assoc(m, pargs) => {
                let v = self.fn_value(m, Some(pargs), None, span);
                self.as_fn_value(v, span)
            }
            PathRes::Module(m) => {
                let n = self.s.def(m).name.clone();
                self.err(
                    "T054",
                    span,
                    format!("expected a value, found module `{n}`"),
                );
                Expr::error(span)
            }
            PathRes::Type(t) => {
                let n = self.tys(&t);
                self.err("T054", span, format!("expected a value, found type `{n}`"));
                Expr::error(span)
            }
        }
    }

    fn field_access(&mut self, r: Expr, name: &str, span: Span) -> Expr {
        let rt = self.tab.shallow(&r.ty);
        match &rt {
            Ty::Adt(d, args) if self.s.def(*d).kind == DefKind::Struct => {
                let a = self.s.adt(*d);
                let AdtKind::Struct(fs) = &a.kind else {
                    unreachable!()
                };
                if let Some(i) = fs.iter().position(|f| f.name == name) {
                    let m = self.s.def(*d).module;
                    if !fs[i].is_pub && !self.s.t.is_within(self.scope.module, m) {
                        self.err(
                            "R011",
                            span,
                            format!("field `{name}` of `{}` is private", self.s.def(*d).name),
                        );
                    }
                    let ty = fs[i].ty.subst(&a.params, args);
                    return Expr {
                        span,
                        ty,
                        kind: ExprKind::Field {
                            recv: Box::new(r),
                            index: i,
                        },
                    };
                }
                let t = self.tys(&rt);
                self.err("T041", span, format!("type `{t}` has no field `{name}`"));
                Expr::error(span)
            }
            Ty::Error => Expr::error(span),
            Ty::Infer(_) => {
                self.err(
                    "T050",
                    span,
                    "type annotations needed: the type of this value is not known here".into(),
                );
                Expr::error(span)
            }
            _ => {
                let t = self.tys(&rt);
                self.err("T041", span, format!("type `{t}` has no field `{name}`"));
                Expr::error(span)
            }
        }
    }

    // ------------------------------------------------------------ calls

    pub(crate) fn call(
        &mut self,
        callee: &ast::Expr,
        args: &[ast::Arg],
        span: Span,
        _expected: Option<&Ty>,
    ) -> Expr {
        // Resolve the callee: a path to a function, variant or associated
        // function; a method call `recv.m(...)`; or a value of function type.
        let mut via_path = None;
        let named_fn = match &callee.kind {
            AE::Name(n) if self.lookup_local(n).is_none() => {
                match self.s.t.lookup(self.scope.module, n, false) {
                    Ok(Some(b)) => Some((b.def, None)),
                    _ => None,
                }
            }
            AE::GenericName { name, args: ga } => {
                match self.s.t.lookup(self.scope.module, name, false) {
                    Ok(Some(b)) => {
                        Some((b.def, self.explicit_args(&Some(ga.clone()), callee.span)))
                    }
                    _ => None,
                }
            }
            _ => None,
        };
        if named_fn.is_none() {
            via_path = self.path_res(callee);
        }
        if let Some((d, ex)) = named_fn {
            via_path = Some(match self.s.def(d).kind {
                DefKind::Variant => {
                    let e = self.s.def(d).parent.unwrap();
                    let n = self.s.generics_of(e).params.len();
                    let a = (0..n).map(|_| self.fresh()).collect();
                    PathRes::Variant(d, a)
                }
                DefKind::Fn | DefKind::ExternFn => {
                    let (fv, sig) = self.fn_value(d, None, ex, callee.span);
                    return self.finish_call(fv, Some(sig), None, args, span);
                }
                _ => PathRes::Item(d),
            });
        }
        match via_path {
            Some(PathRes::Err) => {
                for a in args {
                    self.expr(&a.value, None);
                }
                Expr::error(span)
            }
            Some(PathRes::Variant(v, eargs)) => self.variant_call(v, eargs, args, span),
            Some(PathRes::Item(d))
                if matches!(self.s.def(d).kind, DefKind::Fn | DefKind::ExternFn) =>
            {
                let ex = match &callee.kind {
                    AE::Field { args: ga, .. } => self.explicit_args(ga, callee.span),
                    _ => None,
                };
                let (fv, sig) = self.fn_value(d, None, ex, callee.span);
                self.finish_call(fv, Some(sig), None, args, span)
            }
            Some(PathRes::Assoc(m, pargs)) => {
                let ex = match &callee.kind {
                    AE::Field { args: ga, .. } => self.explicit_args(ga, callee.span),
                    _ => None,
                };
                let (fv, sig) = self.fn_value(m, Some(pargs), ex, callee.span);
                self.finish_call(fv, Some(sig), None, args, span)
            }
            Some(other) => {
                let v = self.res_value(other, callee.span);
                self.finish_call(v, None, None, args, span)
            }
            None => {
                if let AE::Field {
                    recv,
                    name,
                    args: ga,
                } = &callee.kind
                {
                    let r = self.expr(recv, None);
                    let rt = self.tab.shallow(&r.ty);
                    if let Ty::Infer(_) = rt {
                        self.err(
                            "T050",
                            callee.span,
                            "type annotations needed: the receiver's type is not known here".into(),
                        );
                        return Expr::error(span);
                    }
                    // A field of function type is called like a function.
                    if let Ty::Adt(d, _) = &rt {
                        if self.s.def(*d).kind == DefKind::Struct {
                            let a = self.s.adt(*d);
                            if let AdtKind::Struct(fs) = &a.kind {
                                if fs.iter().any(|f| f.name == *name)
                                    && self.lookup_assoc(&rt, name, callee.span).is_none()
                                {
                                    let fv = self.field_access(r, name, callee.span);
                                    return self.finish_call(fv, None, None, args, span);
                                }
                            }
                        }
                    }
                    let Some((m, pargs)) = self.lookup_assoc(&rt, name, callee.span) else {
                        if !matches!(rt, Ty::Error) {
                            let t = self.tys(&rt);
                            self.err(
                                "T051",
                                callee.span,
                                format!("no method named `{name}` for type `{t}`"),
                            );
                        }
                        for a in args {
                            self.expr(&a.value, None);
                        }
                        return Expr::error(span);
                    };
                    let ex = self.explicit_args(ga, callee.span);
                    let (fv, sig) = self.fn_value(m, Some(pargs), ex, callee.span);
                    if !sig.has_self {
                        self.err(
                            "T055",
                            callee.span,
                            format!("`{name}` is an associated function, not a method: call it as `Type.{name}(...)`"),
                        );
                        return Expr::error(span);
                    }
                    return self.finish_call(fv, Some(sig), Some(r), args, span);
                }
                let v = self.expr(callee, None);
                self.finish_call(v, None, None, args, span)
            }
        }
    }

    fn variant_call(&mut self, v: DefId, eargs: Vec<Ty>, args: &[ast::Arg], span: Span) -> Expr {
        let e = self.s.def(v).parent.unwrap();
        let a = self.s.adt(e);
        let AdtKind::Enum(vs) = &a.kind else {
            unreachable!()
        };
        let shape = vs.iter().find(|(d, _)| *d == v).unwrap().1.clone();
        let n = self.s.def(v).name.clone();
        let VariantShape::Tuple(tys) = shape else {
            self.err(
                "T047",
                span,
                format!("variant `{n}` is not called with arguments"),
            );
            for x in args {
                self.expr(&x.value, None);
            }
            return Expr::error(span);
        };
        if tys.len() != args.len() {
            self.err(
                "T058",
                span,
                format!(
                    "variant `{n}` takes {} value(s), found {}",
                    tys.len(),
                    args.len()
                ),
            );
        }
        let mut fields = Vec::new();
        for (i, x) in args.iter().enumerate() {
            if x.name.is_some() || x.marker.is_some() {
                self.err(
                    "T056",
                    x.value.span,
                    "variant payloads take positional values without names or markers".into(),
                );
            }
            match tys.get(i) {
                Some(t) => fields.push(self.check(&x.value, &t.subst(&a.params, &eargs))),
                None => {
                    self.expr(&x.value, None);
                }
            }
        }
        while fields.len() < tys.len() {
            fields.push(Expr::error(span));
        }
        Expr {
            span,
            ty: Ty::Adt(e, eargs),
            kind: ExprKind::Variant { variant: v, fields },
        }
    }

    /// Check arguments and build the call. With a signature, names and
    /// passing modes are checked; `recv` is a method receiver.
    fn finish_call(
        &mut self,
        callee: Expr,
        sig: Option<FnSigTy>,
        recv: Option<Expr>,
        args: &[ast::Arg],
        span: Span,
    ) -> Expr {
        let ct = self.tab.shallow(&callee.ty);
        if let ExprKind::FnRef(d, _) = &callee.kind {
            if self.s.def(*d).kind == DefKind::ExternFn && !self.has_cap("FFI") {
                let n = self.s.def(*d).name.clone();
                self.err(
                    "T100",
                    span,
                    format!("calling extern function `{n}` requires `unsafe(cap: FFI)`"),
                );
            }
        }
        let (ptys, ret) = match &ct {
            Ty::Fn(p, r) => (p.clone(), (**r).clone()),
            Ty::Error => {
                for a in args {
                    self.expr(&a.value, None);
                }
                return Expr::error(span);
            }
            Ty::Infer(v) if self.tab.kind(*v) == Some(crate::infer::VarKind::General) => {
                self.err(
                    "T050",
                    span,
                    "type annotations needed: the callee's type is not known here".into(),
                );
                return Expr::error(span);
            }
            other => {
                let t = self.tys(other);
                self.err(
                    "T059",
                    span,
                    format!("`{t}` is not a function and cannot be called"),
                );
                for a in args {
                    self.expr(&a.value, None);
                }
                return Expr::error(span);
            }
        };
        let mut out: Vec<Option<Expr>> = vec![None; ptys.len()];
        let mut start = 0;
        if let Some(r) = recv {
            let rt = r.ty.clone();
            self.coerce(&rt, &ptys[0], r.span);
            if let Some(s) = &sig {
                if s.inputs[0].mode == Mode::Inout {
                    self.check_place(&r, r.span, "call an `inout self` method on");
                }
            }
            out[0] = Some(r);
            start = 1;
        }
        let names: Vec<Option<(String, Mode)>> = match &sig {
            Some(s) => s
                .inputs
                .iter()
                .map(|p| Some((p.name.clone(), p.mode)))
                .collect(),
            None => vec![None; ptys.len()],
        };
        let mut next = start;
        let mut seen_named = false;
        for a in args {
            let idx = match &a.name {
                Some(n) => {
                    seen_named = true;
                    match names
                        .iter()
                        .position(|x| x.as_ref().is_some_and(|(pn, _)| pn == n))
                    {
                        Some(i) if i >= start => Some(i),
                        _ => {
                            self.err("T057", a.value.span, format!("no parameter named `{n}`"));
                            None
                        }
                    }
                }
                None => {
                    if seen_named {
                        self.err(
                            "T056",
                            a.value.span,
                            "positional arguments must come before named arguments".into(),
                        );
                    }
                    while next < out.len() && out[next].is_some() {
                        next += 1;
                    }
                    if next < out.len() {
                        next += 1;
                        Some(next - 1)
                    } else {
                        None
                    }
                }
            };
            let Some(i) = idx else {
                if a.name.is_none() {
                    self.err(
                        "T058",
                        a.value.span,
                        format!("too many arguments: expected {}", ptys.len() - start),
                    );
                }
                self.expr(&a.value, None);
                continue;
            };
            let h = self.check(&a.value, &ptys[i].clone());
            let mode = names[i].as_ref().map(|(_, m)| *m);
            match (a.marker, mode) {
                (Some("&"), Some(Mode::Inout)) => {
                    self.check_place(&h, a.value.span, "pass `&` (exclusive) access to")
                }
                (None, Some(Mode::Inout)) => {
                    let n = names[i].as_ref().unwrap().0.clone();
                    self.err(
                        "T062",
                        a.value.span,
                        format!("argument for `inout` parameter `{n}` must be marked with `&`"),
                    );
                }
                (Some("&"), _) => self.err(
                    "T063",
                    a.value.span,
                    "`&` marks an argument for an `inout` parameter only".into(),
                ),
                (Some("take"), Some(Mode::Own)) => {}
                (Some("take"), _) => self.err(
                    "T063",
                    a.value.span,
                    "`take` marks an argument for an `own` parameter only".into(),
                ),
                _ => {}
            }
            if out[i].is_some() {
                self.err(
                    "T057",
                    a.value.span,
                    "this parameter is given more than once".into(),
                );
            }
            out[i] = Some(h);
        }
        let mut final_args = Vec::new();
        for (i, a) in out.into_iter().enumerate() {
            match a {
                Some(a) => final_args.push(a),
                None => {
                    let what = names[i]
                        .as_ref()
                        .map(|(n, _)| format!("`{n}`"))
                        .unwrap_or_else(|| format!("{}", i + 1));
                    self.err("T058", span, format!("missing argument {what}"));
                    final_args.push(Expr::error(span));
                }
            }
        }
        Expr {
            span,
            ty: ret,
            kind: ExprKind::Call {
                callee: Box::new(callee),
                args: final_args,
            },
        }
    }
}
