//! Item-level semantics: generics and bounds, type lowering, function
//! signatures, struct/enum layouts, impls and trait conformance.

use crate::defs::{Binding, Def, DefId, DefKind, DefTable};
use crate::diag::Diags;
use crate::hir;
use crate::source::FileId;
use crate::ty::{ConstVal, FloatTy, IntTy, Ty};
use pegasus_parser::ast::{self, ItemKind, Span, TypeKind};
use std::collections::{HashMap, HashSet};

pub const EFFECTS: [&str; 6] = ["alloc", "io", "block", "panic", "unsafe", "nondet"];
pub const CAPABILITIES: [&str; 6] = [
    "RawMemory",
    "FFI",
    "Mmio",
    "Asm",
    "AssumeContract",
    "UncheckedMath",
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// Shared borrow (the default).
    In,
    /// Exclusive borrow; the call site writes `&`.
    Inout,
    /// Ownership moves in; the call site may write `take`.
    Own,
}

impl Mode {
    fn parse(s: &Option<String>) -> Mode {
        match s.as_deref() {
            Some("inout") => Mode::Inout,
            Some("own") => Mode::Own,
            _ => Mode::In,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Bound {
    pub ty: Ty,
    pub trait_: DefId,
    pub args: Vec<Ty>,
}

#[derive(Clone, Debug, Default)]
pub struct Generics {
    /// Every parameter in scope, outer (impl/trait) first.
    pub params: Vec<DefId>,
    pub bounds: Vec<Bound>,
}

#[derive(Clone, Debug)]
pub struct FieldDef {
    pub name: String,
    pub ty: Ty,
    pub is_pub: bool,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub enum VariantShape {
    Unit,
    Tuple(Vec<Ty>),
    Struct(Vec<FieldDef>),
}

#[derive(Clone, Debug)]
pub enum AdtKind {
    Struct(Vec<FieldDef>),
    Enum(Vec<(DefId, VariantShape)>),
}

#[derive(Clone, Debug)]
pub struct AdtDef {
    pub params: Vec<DefId>,
    pub kind: AdtKind,
}

#[derive(Clone, Debug)]
pub struct ParamTy {
    pub name: String,
    pub mode: Mode,
    pub ty: Ty,
    pub var: bool,
    pub is_self: bool,
}

#[derive(Clone, Debug)]
pub struct FnSigTy {
    pub generics: Vec<DefId>,
    /// Only the function's own parameters (excluding impl/trait ones), in order;
    /// explicit generic arguments at a call site bind these.
    pub own_generics: Vec<DefId>,
    pub inputs: Vec<ParamTy>,
    pub ret: Ty,
    pub ret_borrow: bool,
    pub has_self: bool,
}

#[derive(Clone, Debug)]
pub struct ImplData {
    pub self_ty: Ty,
    pub trait_ref: Option<(DefId, Vec<Ty>)>,
}

/// Scope for lowering types written in an item.
#[derive(Clone, Debug)]
pub struct TyScope {
    pub module: DefId,
    pub file: FileId,
    pub generics: Vec<DefId>,
    pub self_ty: Option<Ty>,
    /// The impl whose associated types `Self.X` may name.
    pub impl_: Option<DefId>,
}

pub struct Sema<'a> {
    pub t: DefTable<'a>,
    pub diags: Diags,
    pub generics: HashMap<DefId, Generics>,
    pub adts: HashMap<DefId, AdtDef>,
    pub sigs: HashMap<DefId, FnSigTy>,
    pub impls: HashMap<DefId, ImplData>,
    pub inherent: HashMap<DefId, Vec<DefId>>,
    pub trait_impls: HashMap<DefId, Vec<DefId>>,
    pub trait_self: HashMap<DefId, DefId>,
    pub alias_busy: HashSet<DefId>,
    pub const_tys: HashMap<DefId, Ty>,
    pub const_vals: HashMap<DefId, Option<ConstVal>>,
    pub const_busy: HashSet<DefId>,
    pub bodies: HashMap<DefId, hir::Body>,
    pub option: DefId,
    pub result: DefId,
}

impl<'a> Sema<'a> {
    pub fn new(t: DefTable<'a>, diags: Diags) -> Sema<'a> {
        let find = |t: &DefTable, n: &str| t.mods[&t.core_root].types[n].def;
        let option = find(&t, "Option");
        let result = find(&t, "Result");
        Sema {
            t,
            diags,
            generics: HashMap::new(),
            adts: HashMap::new(),
            sigs: HashMap::new(),
            impls: HashMap::new(),
            inherent: HashMap::new(),
            trait_impls: HashMap::new(),
            trait_self: HashMap::new(),
            alias_busy: HashSet::new(),
            const_tys: HashMap::new(),
            const_vals: HashMap::new(),
            const_busy: HashSet::new(),
            bodies: HashMap::new(),
            option,
            result,
        }
    }

    pub fn def(&self, d: DefId) -> &Def<'a> {
        self.t.def(d)
    }

    pub fn err(&mut self, code: &'static str, file: FileId, span: Span, msg: String) {
        self.diags.error(code, file, span, msg);
    }

    // ------------------------------------------------------------ display

    pub fn ty_str(&self, t: &Ty) -> String {
        match t {
            Ty::Int(i) => i.name().to_string(),
            Ty::Float(f) => f.name().to_string(),
            Ty::Bool => "bool".into(),
            Ty::Char => "char".into(),
            Ty::Str => "str".into(),
            Ty::Tuple(v) if v.len() == 1 => format!("({},)", self.ty_str(&v[0])),
            Ty::Tuple(v) => format!(
                "({})",
                v.iter()
                    .map(|t| self.ty_str(t))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            Ty::Array(t, n) => format!("[{}; {}]", self.ty_str(t), self.ty_str(n)),
            Ty::Adt(d, a) if a.is_empty() => self.def(*d).name.clone(),
            Ty::Adt(d, a) => format!(
                "{}<{}>",
                self.def(*d).name,
                a.iter()
                    .map(|t| self.ty_str(t))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            Ty::Fn(p, r) => format!(
                "fn({}) -> {}",
                p.iter()
                    .map(|t| self.ty_str(t))
                    .collect::<Vec<_>>()
                    .join(", "),
                self.ty_str(r)
            ),
            Ty::Ptr(m, t) => format!("*{} {}", if *m { "var" } else { "const" }, self.ty_str(t)),
            Ty::Param(p) => self.def(*p).name.clone(),
            Ty::Const(ConstVal::Int(i)) => i.to_string(),
            Ty::Const(ConstVal::Bool(b)) => b.to_string(),
            Ty::Region => "region".into(),
            Ty::Infer(_) => "_".into(),
            Ty::Never => "never".into(),
            Ty::Error => "{error}".into(),
        }
    }

    // ------------------------------------------------------------ generics

    fn new_param(&mut self, owner: DefId, name: &str, gp: Option<&'a ast::GenericParam>) -> DefId {
        let o = self.def(owner);
        let (module, file, span) = (o.module, o.file, o.span);
        self.t.add(Def {
            kind: DefKind::GenericParam,
            name: name.to_string(),
            parent: Some(owner),
            module,
            file,
            span,
            is_pub: true,
            item: None,
            variant: None,
            gparam: gp,
        })
    }

    /// The parent whose generics a member inherits (impl or trait).
    fn member_parent(&self, d: DefId) -> Option<DefId> {
        let p = self.def(d).parent?;
        matches!(self.def(p).kind, DefKind::Impl | DefKind::Trait).then_some(p)
    }

    /// The declared type of a const generic parameter.
    pub fn const_param_ty(&mut self, p: DefId) -> Option<Ty> {
        let Some(ast::GenericParam::Const { ty, .. }) = self.def(p).gparam else {
            return None;
        };
        let owner = self.def(p).parent?;
        let g = self.generics_of(owner);
        let scope = self.scope_for(owner, &g);
        Some(self.lower_ty(ty, &scope))
    }

    pub fn is_const_param(&self, p: DefId) -> bool {
        matches!(self.def(p).gparam, Some(ast::GenericParam::Const { .. }))
    }

    pub fn generics_of(&mut self, d: DefId) -> Generics {
        if let Some(g) = self.generics.get(&d) {
            return g.clone();
        }
        // Placeholder to stop recursion through bounds that mention the item.
        self.generics.insert(d, Generics::default());
        let mut g = match self.member_parent(d) {
            Some(p) => self.generics_of(p),
            None => Generics::default(),
        };
        let def = self.def(d);
        let item = def.item;
        let kind = def.kind;
        if kind == DefKind::Trait {
            let sp = self.new_param(d, "Self", None);
            self.trait_self.insert(d, sp);
            g.params.push(sp);
        }
        let (gps, where_): (&'a [ast::GenericParam], Vec<&'a ast::WherePred>) =
            match item.map(|i| &i.kind) {
                Some(ItemKind::Struct {
                    generics, where_, ..
                })
                | Some(ItemKind::Enum {
                    generics, where_, ..
                }) => (generics, where_.iter().collect()),
                Some(ItemKind::Trait {
                    generics, where_, ..
                })
                | Some(ItemKind::Impl {
                    generics, where_, ..
                }) => (generics, where_.iter().collect()),
                Some(ItemKind::TypeAlias { generics, .. }) => (generics, Vec::new()),
                Some(ItemKind::Fn(f)) if kind == DefKind::Fn => (
                    &f.sig.generics,
                    f.sig
                        .clauses
                        .iter()
                        .filter_map(|c| match c {
                            ast::Clause::Where(w) => Some(w.iter()),
                            _ => None,
                        })
                        .flatten()
                        .collect(),
                ),
                _ => (&[], Vec::new()),
            };
        let start = g.params.len();
        let mut seen = HashSet::new();
        for gp in gps {
            let name = match gp {
                ast::GenericParam::Type { name, .. } | ast::GenericParam::Const { name, .. } => {
                    name
                }
            };
            if !seen.insert(name.clone()) {
                let (f, s) = (self.def(d).file, self.def(d).span);
                self.err(
                    "R001",
                    f,
                    s,
                    format!("generic parameter `{name}` is declared twice"),
                );
            }
            let p = self.new_param(d, name, Some(gp));
            g.params.push(p);
        }
        self.generics.insert(d, g.clone());
        let scope = self.scope_for(d, &g);
        // Bounds: inline bounds, where clauses, and trait supertraits.
        for (i, gp) in gps.iter().enumerate() {
            if let ast::GenericParam::Type { bounds, .. } = gp {
                let p = g.params[start + i];
                for b in bounds {
                    if let Some(bd) = self.lower_bound(&Ty::Param(p), b, &scope) {
                        g.bounds.push(bd);
                    }
                }
            }
            if let ast::GenericParam::Const { ty, .. } = gp {
                let t = self.lower_ty(ty, &scope);
                if !matches!(t, Ty::Int(_) | Ty::Bool | Ty::Error) {
                    self.err(
                        "T040",
                        scope.file,
                        ty.span,
                        format!(
                            "const generic parameters must be integers or bool, not `{}`",
                            self.ty_str(&t)
                        ),
                    );
                }
            }
        }
        for w in where_ {
            let lhs = self.lower_ty(&w.ty, &scope);
            if !matches!(lhs, Ty::Param(_) | Ty::Error) {
                self.err(
                    "U005",
                    scope.file,
                    w.ty.span,
                    "where clauses on types other than generic parameters are not implemented yet"
                        .into(),
                );
                continue;
            }
            for b in &w.bounds {
                if let Some(bd) = self.lower_bound(&lhs, b, &scope) {
                    g.bounds.push(bd);
                }
            }
        }
        if let Some(ItemKind::Trait { supertraits, .. }) = item.map(|i| &i.kind) {
            let sp = self.trait_self[&d];
            for st in supertraits {
                if let Some(bd) = self.lower_bound(&Ty::Param(sp), st, &scope) {
                    g.bounds.push(bd);
                }
            }
        }
        self.generics.insert(d, g.clone());
        g
    }

    fn lower_bound(&mut self, ty: &Ty, b: &ast::Type, scope: &TyScope) -> Option<Bound> {
        let TypeKind::Path { path, args } = &b.kind else {
            self.err(
                "T010",
                scope.file,
                b.span,
                format!("`{}` is not a trait", b.text),
            );
            return None;
        };
        let d = self.resolve_type_path(path, scope, b.span)?;
        if self.def(d).kind != DefKind::Trait {
            self.err(
                "T010",
                scope.file,
                b.span,
                format!("`{}` is not a trait", b.text),
            );
            return None;
        }
        let tg = self.generics_of(d);
        // A trait's own parameters exclude its implicit `Self`.
        let want = tg.params.len() - 1;
        let mut targs = Vec::new();
        for a in args {
            targs.push(self.lower_garg(a, scope, b.span));
        }
        if targs.len() != want {
            self.err(
                "T011",
                scope.file,
                b.span,
                format!(
                    "trait `{}` takes {want} generic argument(s), found {}",
                    self.def(d).name,
                    targs.len()
                ),
            );
            return None;
        }
        Some(Bound {
            ty: ty.clone(),
            trait_: d,
            args: targs,
        })
    }

    /// The type scope for an item's signature and body.
    pub fn scope_for(&mut self, d: DefId, g: &Generics) -> TyScope {
        let def = self.def(d);
        let (module, file) = (def.module, def.file);
        let mut self_ty = None;
        let mut impl_ = None;
        let owner = if matches!(def.kind, DefKind::Impl | DefKind::Trait) {
            Some(d)
        } else {
            self.member_parent(d)
        };
        if let Some(o) = owner {
            match self.def(o).kind {
                DefKind::Trait => self_ty = self.trait_self.get(&o).map(|p| Ty::Param(*p)),
                DefKind::Impl => {
                    impl_ = Some(o);
                    if let Some(id) = self.impls.get(&o) {
                        self_ty = Some(id.self_ty.clone());
                    }
                }
                _ => {}
            }
        }
        if matches!(def.kind, DefKind::Struct | DefKind::Enum) {
            let args = g.params.iter().map(|p| Ty::Param(*p)).collect();
            self_ty = Some(Ty::Adt(d, args));
        }
        TyScope {
            module,
            file,
            generics: g.params.clone(),
            self_ty,
            impl_,
        }
    }

    // ------------------------------------------------------------ paths

    fn binding_or_report(
        &mut self,
        r: Result<Option<Binding>, Vec<Binding>>,
        name: &str,
        file: FileId,
        span: Span,
    ) -> Option<Binding> {
        match r {
            Ok(b) => b,
            Err(_) => {
                self.err(
                    "R005",
                    file,
                    span,
                    format!("`{name}` is ambiguous (several glob imports provide it)"),
                );
                None
            }
        }
    }

    /// Resolve a dotted path in the type namespace. Leading segments must be
    /// modules; privacy is checked at every step.
    pub fn resolve_type_path(
        &mut self,
        path: &[String],
        scope: &TyScope,
        span: Span,
    ) -> Option<DefId> {
        let file = scope.file;
        let first = &path[0];
        let r = self.t.lookup(scope.module, first, true);
        let mut cur = match self.binding_or_report(r, first, file, span) {
            Some(b) => b.def,
            None if first == "core" && path.len() > 1 => self.t.core_root,
            None if first == "self" && path.len() > 1 => scope.module,
            None => {
                let hint = if first == "std" {
                    ": the standard library does not exist yet"
                } else {
                    ""
                };
                self.err(
                    "R006",
                    file,
                    span,
                    format!("cannot find type `{first}` in this scope{hint}"),
                );
                return None;
            }
        };
        for s in &path[1..] {
            if self.def(cur).kind != DefKind::Mod {
                self.err(
                    "R003",
                    file,
                    span,
                    format!(
                        "`{}` is a {}, not a module",
                        self.def(cur).name,
                        self.def(cur).kind.describe()
                    ),
                );
                return None;
            }
            let (tr, _) = self.t.member(cur, s);
            match self.binding_or_report(tr, s, file, span) {
                Some(b) => {
                    if !self.t.binding_visible(&b, cur, scope.module) {
                        self.err("R004", file, span, format!("`{s}` is private"));
                        return None;
                    }
                    cur = b.def;
                }
                None => {
                    self.err(
                        "R006",
                        file,
                        span,
                        format!("cannot find type `{s}` in module `{}`", self.def(cur).name),
                    );
                    return None;
                }
            }
        }
        Some(cur)
    }

    // ------------------------------------------------------------ types

    pub fn prim_ty(name: &str) -> Option<Ty> {
        Some(match name {
            "bool" => Ty::Bool,
            "char" => Ty::Char,
            "str" => Ty::Str,
            _ => {
                if let Some(i) = IntTy::from_name(name) {
                    Ty::Int(i)
                } else {
                    Ty::Float(FloatTy::from_name(name)?)
                }
            }
        })
    }

    pub fn lower_ty(&mut self, t: &ast::Type, scope: &TyScope) -> Ty {
        let file = scope.file;
        match &t.kind {
            TypeKind::Paren(inner) => self.lower_ty(inner, scope),
            TypeKind::Tuple(v) => Ty::Tuple(v.iter().map(|x| self.lower_ty(x, scope)).collect()),
            TypeKind::Optional(inner) => {
                let i = self.lower_ty(inner, scope);
                Ty::Adt(self.option, vec![i])
            }
            TypeKind::Pointer { var, ty } => Ty::Ptr(*var, Box::new(self.lower_ty(ty, scope))),
            TypeKind::Fn { params, ret, .. } => {
                let p = params.iter().map(|x| self.lower_ty(x, scope)).collect();
                let r = match ret {
                    Some(r) => self.lower_ty(r, scope),
                    None => Ty::unit(),
                };
                Ty::Fn(p, Box::new(r))
            }
            TypeKind::Dyn(_) => {
                self.err(
                    "U001",
                    file,
                    t.span,
                    "`dyn` trait objects are not implemented yet".into(),
                );
                Ty::Error
            }
            TypeKind::Array(elem, len) => {
                let e = self.lower_ty(elem, scope);
                let n = self.const_arg_expr(len, scope, Ty::usize());
                Ty::Array(Box::new(e), Box::new(n))
            }
            TypeKind::Path { path, args } => self.lower_path_ty(path, args, t.span, scope),
        }
    }

    fn lower_path_ty(
        &mut self,
        path: &[String],
        args: &[ast::GenericArg],
        span: Span,
        scope: &TyScope,
    ) -> Ty {
        let file = scope.file;
        if path.len() == 1 {
            let n = &path[0];
            if let Some(&p) = scope
                .generics
                .iter()
                .rev()
                .find(|&&p| self.def(p).name == *n)
            {
                if !args.is_empty() {
                    self.err(
                        "T011",
                        file,
                        span,
                        format!("generic parameter `{n}` takes no arguments"),
                    );
                }
                return Ty::Param(p);
            }
            if n == "Self" {
                return match &scope.self_ty {
                    Some(t) => {
                        if !args.is_empty() {
                            self.err(
                                "T011",
                                file,
                                span,
                                "`Self` takes no generic arguments".into(),
                            );
                        }
                        t.clone()
                    }
                    None => {
                        self.err("R007", file, span, "`Self` is only available inside impl, trait, struct and enum declarations".into());
                        Ty::Error
                    }
                };
            }
        }
        if path.len() == 2 && path[0] == "Self" {
            if let Some(imp) = scope.impl_ {
                let found = self.t.members.get(&imp).and_then(|ms| {
                    ms.iter().copied().find(|&m| {
                        self.def(m).kind == DefKind::AssocType && self.def(m).name == path[1]
                    })
                });
                if let Some(m) = found {
                    if let Some(ast::Item {
                        kind: ItemKind::TypeAlias { ty, .. },
                        ..
                    }) = self.def(m).item
                    {
                        return self.lower_ty(ty, scope);
                    }
                }
            }
            self.err(
                "U003",
                file,
                span,
                "associated type projections are not implemented yet".into(),
            );
            return Ty::Error;
        }
        let Some(d) = self.resolve_type_path(path, scope, span) else {
            return Ty::Error;
        };
        let name = path.join(".");
        match self.def(d).kind {
            DefKind::Prim => {
                if !args.is_empty() {
                    let unit = args.iter().any(|a| matches!(a, ast::GenericArg::Unit(_)))
                        || (matches!(self.def(d).name.as_str(), "f16" | "bf16" | "f32" | "f64")
                            && args.len() == 1
                            && matches!(&args[0], ast::GenericArg::Type(_)));
                    if unit {
                        self.err(
                            "U002",
                            file,
                            span,
                            "units of measure (`f64<m/s>`) are not implemented yet".into(),
                        );
                    } else {
                        self.err(
                            "T011",
                            file,
                            span,
                            format!("`{name}` takes no generic arguments"),
                        );
                    }
                    return Ty::Error;
                }
                Sema::prim_ty(&self.def(d).name).unwrap()
            }
            DefKind::Struct | DefKind::Enum | DefKind::TypeAlias => {
                let g = self.generics_of(d);
                let params = g.params.clone();
                let mut targs: Vec<Ty> = Vec::new();
                for (i, a) in args.iter().enumerate() {
                    let want = params.get(i).and_then(|&p| self.const_param_ty(p));
                    targs.push(self.lower_garg_want(a, scope, span, want.as_ref()));
                }
                if targs.len() < params.len() {
                    // Trailing defaults.
                    for &p in &params[targs.len()..] {
                        match self.def(p).gparam {
                            Some(ast::GenericParam::Type {
                                default: Some(dt), ..
                            }) => {
                                let ds = self.scope_for(d, &g);
                                let raw = self.lower_ty(dt, &ds);
                                targs.push(raw.subst(&params[..targs.len()], &targs.clone()));
                            }
                            _ => break,
                        }
                    }
                }
                if targs.len() != params.len() {
                    self.err(
                        "T011",
                        file,
                        span,
                        format!(
                            "`{name}` takes {} generic argument(s), found {}",
                            params.len(),
                            args.len()
                        ),
                    );
                    return Ty::Error;
                }
                for (i, &p) in params.iter().enumerate() {
                    let is_const = self.is_const_param(p);
                    let arg_const = match &targs[i] {
                        Ty::Const(_) => Some(true),
                        Ty::Param(q) => Some(self.is_const_param(*q)),
                        Ty::Error => None,
                        _ => Some(false),
                    };
                    if let Some(ac) = arg_const {
                        if ac != is_const {
                            let want = if is_const { "a constant" } else { "a type" };
                            self.err(
                                "T012",
                                file,
                                span,
                                format!("generic argument {} of `{name}` must be {want}", i + 1),
                            );
                            return Ty::Error;
                        }
                    }
                }
                if self.def(d).kind == DefKind::TypeAlias {
                    return self.expand_alias(d, &params, &targs, file, span);
                }
                Ty::Adt(d, targs)
            }
            DefKind::Trait => {
                self.err(
                    "T010",
                    file,
                    span,
                    format!("trait `{name}` is not a type (trait objects need `dyn`, which is not implemented yet)"),
                );
                Ty::Error
            }
            k => {
                self.err(
                    "T010",
                    file,
                    span,
                    format!("`{name}` is a {}, not a type", k.describe()),
                );
                Ty::Error
            }
        }
    }

    fn expand_alias(
        &mut self,
        d: DefId,
        params: &[DefId],
        args: &[Ty],
        file: FileId,
        span: Span,
    ) -> Ty {
        let Some(ast::Item {
            kind: ItemKind::TypeAlias { ty, refinement, .. },
            ..
        }) = self.def(d).item
        else {
            return Ty::Error;
        };
        if refinement.is_some() {
            self.err(
                "U006",
                file,
                span,
                format!(
                    "refinement type `{}` needs the contract prover, which is not implemented yet",
                    self.def(d).name
                ),
            );
            return Ty::Error;
        }
        if !self.alias_busy.insert(d) {
            self.err(
                "T013",
                file,
                span,
                format!("type alias `{}` refers to itself", self.def(d).name),
            );
            return Ty::Error;
        }
        let g = self.generics_of(d);
        let scope = self.scope_for(d, &g);
        let raw = self.lower_ty(ty, &scope);
        self.alias_busy.remove(&d);
        raw.subst(params, args)
    }

    pub fn lower_garg(&mut self, a: &ast::GenericArg, scope: &TyScope, span: Span) -> Ty {
        self.lower_garg_want(a, scope, span, None)
    }

    /// `want`: the declared type of the const parameter this argument binds.
    pub fn lower_garg_want(
        &mut self,
        a: &ast::GenericArg,
        scope: &TyScope,
        span: Span,
        want: Option<&Ty>,
    ) -> Ty {
        match a {
            ast::GenericArg::Type(t) => {
                // A lone name may denote a const parameter or a constant item.
                if let TypeKind::Path { path, args } = &t.kind {
                    if path.len() == 1 && args.is_empty() {
                        let n = &path[0];
                        if let Some(&p) = scope
                            .generics
                            .iter()
                            .rev()
                            .find(|&&p| self.def(p).name == *n)
                        {
                            return Ty::Param(p);
                        }
                        if let Ok(Some(b)) = self.t.lookup(scope.module, n, false) {
                            if self.def(b.def).kind == DefKind::Const {
                                let e = ast::Expr {
                                    span,
                                    kind: ast::ExprKind::Name(n.clone()),
                                };
                                return self.const_or_report(&e, scope, want);
                            }
                        }
                    }
                }
                self.lower_ty(t, scope)
            }
            ast::GenericArg::Unit(_) => {
                self.err(
                    "U002",
                    scope.file,
                    span,
                    "units of measure (`f64<m/s>`) are not implemented yet".into(),
                );
                Ty::Error
            }
            ast::GenericArg::Const(text) => {
                let s = text.trim();
                let inner = s
                    .strip_prefix('{')
                    .and_then(|x| x.strip_suffix('}'))
                    .map(str::trim);
                let src = inner.unwrap_or(s);
                let r = crate::consteval::eval_text(self, src, scope, want);
                self.report_const(r, scope.file, span)
            }
        }
    }

    /// A constant expression in a type (array length): evaluated now as
    /// `usize`, so it is non-negative and fits the target's address width.
    fn const_arg_expr(&mut self, e: &ast::Expr, scope: &TyScope, want: Ty) -> Ty {
        self.const_or_report(e, scope, Some(&want))
    }

    pub fn const_or_report(&mut self, e: &ast::Expr, scope: &TyScope, want: Option<&Ty>) -> Ty {
        let r = crate::consteval::eval_ty(self, e, scope, want);
        self.report_const(r, scope.file, e.span)
    }

    pub fn report_const(
        &mut self,
        r: Result<Ty, crate::consteval::ConstErr>,
        file: FileId,
        span: Span,
    ) -> Ty {
        match r {
            Ok(t) => t,
            Err(crate::consteval::ConstErr::Unsupported(m)) => {
                self.err("U007", file, span, m);
                Ty::Error
            }
            Err(crate::consteval::ConstErr::Invalid(m)) => {
                self.err("T017", file, span, m);
                Ty::Error
            }
        }
    }

    // ------------------------------------------------------------ constants

    pub fn const_value(&mut self, d: DefId) -> Option<ConstVal> {
        if let Some(v) = self.const_vals.get(&d) {
            return *v;
        }
        let def = self.def(d);
        let (file, span) = (def.file, def.span);
        let value = match def.item.map(|i| &i.kind) {
            Some(ItemKind::Const { value: Some(v), .. }) => v,
            _ => return None,
        };
        if !self.const_busy.insert(d) {
            self.err(
                "T015",
                file,
                span,
                format!("constant `{}` depends on itself", self.def(d).name),
            );
            return None;
        }
        let g = self.generics_of(d);
        let scope = self.scope_for(d, &g);
        let ty = self.const_ty(d);
        let want = matches!(ty, Ty::Int(_) | Ty::Bool).then_some(ty);
        let r = match crate::consteval::eval(self, value, &scope, want.as_ref()) {
            Ok(crate::consteval::CValue::Int(v, _)) => Some(ConstVal::Int(v)),
            Ok(crate::consteval::CValue::Bool(b)) => Some(ConstVal::Bool(b)),
            Ok(crate::consteval::CValue::Param(_)) => None,
            // Not evaluable here: the initializer is still type-checked, and
            // its value is computed by the comptime interpreter later.
            Err(crate::consteval::ConstErr::Unsupported(_)) => None,
            Err(crate::consteval::ConstErr::Invalid(m)) => {
                self.err("T017", file, value.span, m);
                None
            }
        };
        self.const_busy.remove(&d);
        self.const_vals.insert(d, r);
        r
    }

    pub fn const_ty(&mut self, d: DefId) -> Ty {
        if let Some(t) = self.const_tys.get(&d) {
            return t.clone();
        }
        let ty_ast = match self.def(d).item.map(|i| &i.kind) {
            Some(ItemKind::Const { ty, .. }) => ty,
            _ => return Ty::Error,
        };
        let g = self.generics_of(d);
        let scope = self.scope_for(d, &g);
        let t = self.lower_ty(ty_ast, &scope);
        self.const_tys.insert(d, t.clone());
        t
    }

    // ------------------------------------------------------------ signatures

    pub fn fn_sig_ast(&self, d: DefId) -> Option<&'a ast::FnSig> {
        match self.def(d).item.map(|i| &i.kind) {
            Some(ItemKind::Fn(f)) => Some(&f.sig),
            Some(ItemKind::ExternFn { sig, .. }) => Some(sig),
            _ => None,
        }
    }

    pub fn sig(&mut self, d: DefId) -> FnSigTy {
        if let Some(s) = self.sigs.get(&d) {
            return s.clone();
        }
        let sig = self.fn_sig_ast(d).expect("sig of a function");
        let g = self.generics_of(d);
        let scope = self.scope_for(d, &g);
        let parent_n = self
            .member_parent(d)
            .map(|p| self.generics_of(p).params.len())
            .unwrap_or(0);
        let mut inputs = Vec::new();
        let mut has_self = false;
        let mut names = HashSet::new();
        for (i, p) in sig.params.iter().enumerate() {
            match p {
                ast::Param::SelfParam { mode } => {
                    let ty = match (&scope.self_ty, self.member_parent(d)) {
                        (Some(t), Some(_)) if i == 0 => t.clone(),
                        _ => {
                            let (f, s) = (scope.file, self.def(d).span);
                            self.err("R008", f, s, "`self` parameter is only allowed first, in an impl or trait method".into());
                            Ty::Error
                        }
                    };
                    has_self = true;
                    inputs.push(ParamTy {
                        name: "self".into(),
                        mode: Mode::parse(mode),
                        ty,
                        var: false,
                        is_self: true,
                    });
                }
                ast::Param::Named {
                    var,
                    name,
                    mode,
                    ty,
                } => {
                    if !names.insert(name.clone()) {
                        let (f, s) = (scope.file, self.def(d).span);
                        self.err(
                            "R001",
                            f,
                            s,
                            format!("parameter `{name}` is declared twice"),
                        );
                    }
                    let t = self.lower_ty(ty, &scope);
                    inputs.push(ParamTy {
                        name: name.clone(),
                        mode: Mode::parse(mode),
                        ty: t,
                        var: *var,
                        is_self: false,
                    });
                }
            }
        }
        let (ret, ret_borrow) = match &sig.ret {
            Some(r) => (self.lower_ty(&r.ty, &scope), r.borrow),
            None => (Ty::unit(), false),
        };
        let file = scope.file;
        let span = self.def(d).span;
        if let Some(effs) = &sig.effects {
            for e in effs {
                if !EFFECTS.contains(&e.as_str()) {
                    self.err(
                        "T016",
                        file,
                        span,
                        format!("unknown effect `{e}` (v1 effects: {})", EFFECTS.join(", ")),
                    );
                }
            }
        }
        if let Some(p) = &sig.prefix {
            let what = match p.as_str() {
                "async" => "async functions are not implemented yet (runtime and state-machine lowering, Phase 9)",
                "comptime" => "comptime functions need the reference interpreter, which is not implemented yet",
                _ => "kernel functions need a GPU backend, which is not implemented yet",
            };
            self.err("U004", file, span, what.into());
        }
        let s = FnSigTy {
            own_generics: g.params[parent_n..].to_vec(),
            generics: g.params,
            inputs,
            ret,
            ret_borrow,
            has_self,
        };
        self.sigs.insert(d, s.clone());
        s
    }

    // ------------------------------------------------------------ ADTs

    pub fn adt(&mut self, d: DefId) -> AdtDef {
        if let Some(a) = self.adts.get(&d) {
            return a.clone();
        }
        let g = self.generics_of(d);
        let scope = self.scope_for(d, &g);
        let item = self.def(d).item.unwrap();
        let kind = match &item.kind {
            ItemKind::Struct { fields, .. } => AdtKind::Struct(self.lower_fields(fields, &scope)),
            ItemKind::Enum { variants, .. } => {
                let vds = self.t.variants[&d].clone();
                let mut out = Vec::new();
                for (v, vd) in variants.iter().zip(vds) {
                    let shape = match &v.kind {
                        ast::VariantKind::Unit => VariantShape::Unit,
                        ast::VariantKind::Tuple(elems) => VariantShape::Tuple(
                            elems
                                .iter()
                                .map(|(_, t)| self.lower_ty(t, &scope))
                                .collect(),
                        ),
                        ast::VariantKind::Struct(fs) => {
                            VariantShape::Struct(self.lower_fields(fs, &scope))
                        }
                    };
                    out.push((vd, shape));
                }
                AdtKind::Enum(out)
            }
            _ => unreachable!(),
        };
        let a = AdtDef {
            params: g.params,
            kind,
        };
        self.adts.insert(d, a.clone());
        a
    }

    fn lower_fields(&mut self, fields: &[ast::Field], scope: &TyScope) -> Vec<FieldDef> {
        let mut seen = HashSet::new();
        let mut out = Vec::new();
        for f in fields {
            if !seen.insert(f.name.clone()) {
                self.err(
                    "R001",
                    scope.file,
                    f.span,
                    format!("field `{}` is declared twice", f.name),
                );
            }
            out.push(FieldDef {
                name: f.name.clone(),
                ty: self.lower_ty(&f.ty, scope),
                is_pub: f.is_pub,
                span: f.span,
            });
        }
        out
    }

    /// Report structs and enums that contain themselves by value.
    pub fn check_recursive_adts(&mut self) {
        let adts: Vec<DefId> = self
            .t
            .defs
            .iter()
            .enumerate()
            .filter(|(_, d)| matches!(d.kind, DefKind::Struct | DefKind::Enum))
            .map(|(i, _)| i as DefId)
            .collect();
        for d in adts {
            let mut stack = vec![d];
            if self.contains_by_value(&Ty::Adt(d, Vec::new()), d, &mut stack, true) {
                let (f, s) = (self.def(d).file, self.def(d).span);
                self.err(
                    "T020",
                    f,
                    s,
                    format!(
                        "`{}` contains itself by value and would have infinite size (use an index, arena or region; Box is not available yet)",
                        self.def(d).name
                    ),
                );
            }
        }
    }

    fn contains_by_value(
        &mut self,
        t: &Ty,
        target: DefId,
        stack: &mut Vec<DefId>,
        top: bool,
    ) -> bool {
        match t {
            Ty::Tuple(v) => v
                .iter()
                .any(|x| self.contains_by_value(x, target, stack, false)),
            Ty::Array(e, _) => self.contains_by_value(e, target, stack, false),
            Ty::Adt(d, args) => {
                if !top && *d == target {
                    return true;
                }
                if !top && stack.contains(d) {
                    return false;
                }
                if !matches!(self.def(*d).kind, DefKind::Struct | DefKind::Enum) {
                    return false;
                }
                stack.push(*d);
                let a = self.adt(*d);
                let mut tys = Vec::new();
                match &a.kind {
                    AdtKind::Struct(fs) => tys.extend(fs.iter().map(|f| f.ty.clone())),
                    AdtKind::Enum(vs) => {
                        for (_, s) in vs {
                            match s {
                                VariantShape::Unit => {}
                                VariantShape::Tuple(ts) => tys.extend(ts.iter().cloned()),
                                VariantShape::Struct(fs) => {
                                    tys.extend(fs.iter().map(|f| f.ty.clone()))
                                }
                            }
                        }
                    }
                }
                let args = if top {
                    a.params.iter().map(|p| Ty::Param(*p)).collect()
                } else {
                    args.clone()
                };
                let r = tys.iter().any(|ft| {
                    self.contains_by_value(&ft.subst(&a.params, &args), target, stack, false)
                });
                stack.pop();
                r
            }
            _ => false,
        }
    }

    // ------------------------------------------------------------ impls

    pub fn collect_impls(&mut self) {
        for imp in self.t.impls.clone() {
            let g = self.generics_of(imp);
            let item = self.def(imp).item.unwrap();
            let ItemKind::Impl { trait_, ty, .. } = &item.kind else {
                continue;
            };
            let mut scope = self.scope_for(imp, &g);
            let self_ty = self.lower_ty(ty, &scope);
            scope.self_ty = Some(self_ty.clone());
            let file = scope.file;
            let trait_ref = match trait_ {
                None => None,
                Some(tt) => {
                    match &tt.kind {
                        TypeKind::Path { path, args } => {
                            match self.resolve_type_path(path, &scope, tt.span) {
                                Some(td) if self.def(td).kind == DefKind::Trait => {
                                    let targs: Vec<Ty> = args
                                        .iter()
                                        .map(|a| self.lower_garg(a, &scope, tt.span))
                                        .collect();
                                    let want = self.generics_of(td).params.len() - 1;
                                    if targs.len() != want {
                                        self.err(
                                    "T011",
                                    file,
                                    tt.span,
                                    format!("trait `{}` takes {want} generic argument(s), found {}", self.def(td).name, targs.len()),
                                );
                                    }
                                    Some((td, targs))
                                }
                                Some(_) => {
                                    self.err(
                                        "T010",
                                        file,
                                        tt.span,
                                        format!("`{}` is not a trait", tt.text),
                                    );
                                    None
                                }
                                None => None,
                            }
                        }
                        _ => None,
                    }
                }
            };
            // Unused-parameter check: every impl parameter must appear in the
            // self type or trait arguments, or method calls cannot infer it.
            for &p in &g.params {
                let used = self_ty.any(&mut |t| *t == Ty::Param(p))
                    || trait_ref
                        .as_ref()
                        .is_some_and(|(_, a)| a.iter().any(|x| x.any(&mut |t| *t == Ty::Param(p))));
                if !used {
                    let span = item.span;
                    self.err(
                        "T032",
                        file,
                        span,
                        format!(
                            "generic parameter `{}` of this impl is not used by its type or trait",
                            self.def(p).name
                        ),
                    );
                }
            }
            match &trait_ref {
                None => match &self_ty {
                    Ty::Adt(d, _) => {
                        if self.t.mods[&self.def(*d).module].package_root
                            != self.t.mods[&self.def(imp).module].package_root
                        {
                            self.err("T030", file, item.span, "inherent impls are only allowed in the package that defines the type".into());
                        } else {
                            self.inherent.entry(*d).or_default().push(imp);
                        }
                    }
                    Ty::Error => {}
                    other => {
                        let s = self.ty_str(other);
                        self.err(
                            "T030",
                            file,
                            item.span,
                            format!("inherent impls are only allowed for structs and enums of this package, not `{s}` (use a trait)"),
                        );
                    }
                },
                Some((td, _)) => self.trait_impls.entry(*td).or_default().push(imp),
            }
            self.impls.insert(imp, ImplData { self_ty, trait_ref });
        }
    }

    /// Duplicate inherent methods, overlapping trait impls, and trait
    /// conformance of each impl.
    pub fn check_impls(&mut self) {
        for (adt, imps) in self.inherent.clone() {
            let mut seen: HashMap<String, DefId> = HashMap::new();
            for imp in imps {
                for m in self.t.members.get(&imp).cloned().unwrap_or_default() {
                    let n = self.def(m).name.clone();
                    if let Some(prev) = seen.insert(n.clone(), m) {
                        if prev != m && self.overlap(self.def(prev).parent.unwrap(), imp) {
                            let (f, s) = (self.def(m).file, self.def(m).span);
                            self.err(
                                "R001",
                                f,
                                s,
                                format!(
                                    "`{}` already has an associated item named `{n}`",
                                    self.def(adt).name
                                ),
                            );
                        }
                    }
                }
            }
        }
        for (td, imps) in self.trait_impls.clone() {
            for i in 0..imps.len() {
                for j in 0..i {
                    if self.overlap(imps[i], imps[j]) {
                        let (f, s) = (self.def(imps[i]).file, self.def(imps[i]).span);
                        self.err(
                            "T031",
                            f,
                            s,
                            format!(
                                "conflicting implementations of trait `{}`",
                                self.def(td).name
                            ),
                        );
                    }
                }
            }
            for imp in imps {
                self.check_trait_impl(td, imp);
            }
        }
    }

    /// Two impls can apply to one type (their self types and trait arguments unify).
    fn overlap(&mut self, a: DefId, b: DefId) -> bool {
        let mut tab = crate::infer::InferTable::default();
        let inst = |s: &mut Sema, imp: DefId, tab: &mut crate::infer::InferTable| {
            let g = s.generics_of(imp);
            let vars: Vec<Ty> = g
                .params
                .iter()
                .map(|_| tab.fresh(crate::infer::VarKind::General))
                .collect();
            let id = &s.impls[&imp];
            let st = id.self_ty.subst(&g.params, &vars);
            let ta: Vec<Ty> = id
                .trait_ref
                .as_ref()
                .map(|(_, a)| a.iter().map(|t| t.subst(&g.params, &vars)).collect())
                .unwrap_or_default();
            Ty::Tuple(std::iter::once(st).chain(ta).collect())
        };
        let ta = inst(self, a, &mut tab);
        let tb = inst(self, b, &mut tab);
        if ta.has_error() || tb.has_error() {
            return false;
        }
        tab.unify(&ta, &tb).is_ok()
    }

    fn check_trait_impl(&mut self, td: DefId, imp: DefId) {
        let file = self.def(imp).file;
        let span = self.def(imp).span;
        let id = self.impls[&imp].clone();
        let (_, targs) = id.trait_ref.clone().unwrap();
        let tg = self.generics_of(td);
        // Trait params: [Self, trait generics...]
        let mut map_params = tg.params.clone();
        let mut map_args = vec![id.self_ty.clone()];
        map_args.extend(targs.iter().cloned());
        let tmembers = self.t.members.get(&td).cloned().unwrap_or_default();
        let imembers = self.t.members.get(&imp).cloned().unwrap_or_default();
        let tname = self.def(td).name.clone();
        for &im in &imembers {
            let n = &self.def(im).name;
            if !tmembers
                .iter()
                .any(|&tm| self.def(tm).name == *n && self.def(tm).kind == self.def(im).kind)
            {
                let (f, s) = (self.def(im).file, self.def(im).span);
                let n = n.clone();
                self.err(
                    "T033",
                    f,
                    s,
                    format!("`{n}` is not a member of trait `{tname}`"),
                );
            }
        }
        // Associated types defined by the impl are substituted into the
        // trait's method signatures as they are compared.
        for &tm in &tmembers {
            let tn = self.def(tm).name.clone();
            let found = imembers
                .iter()
                .copied()
                .find(|&im| self.def(im).name == tn && self.def(im).kind == self.def(tm).kind);
            let has_default = match self.def(tm).item.map(|i| &i.kind) {
                Some(ItemKind::Fn(f)) => f.body.is_some(),
                Some(ItemKind::Const { value, .. }) => value.is_some(),
                _ => false,
            };
            let Some(im) = found else {
                if !has_default {
                    self.err(
                        "T034",
                        file,
                        span,
                        format!(
                            "missing {} `{tn}` required by trait `{tname}`",
                            self.def(tm).kind.describe()
                        ),
                    );
                }
                continue;
            };
            match self.def(tm).kind {
                DefKind::AssocType => {
                    self.check_assoc_type(td, tm, im, imp, &map_params, &map_args);
                    continue;
                }
                DefKind::Const => {
                    let tt = self.const_ty(tm).subst(&map_params, &map_args);
                    let it = self.const_ty(im);
                    if !tt.has_error() && !it.has_error() && tt != it {
                        let (f, s) = (self.def(im).file, self.def(im).span);
                        let (a, b) = (self.ty_str(&it), self.ty_str(&tt));
                        self.err(
                            "T036",
                            f,
                            s,
                            format!("constant `{tn}` has type `{a}`, but trait `{tname}` declares `{b}`"),
                        );
                    }
                    continue;
                }
                DefKind::Fn => {}
                _ => continue,
            }
            let ts = self.sig(tm);
            let is = self.sig(im);
            let own_t = &ts.own_generics;
            let own_i = &is.own_generics;
            if own_t.len() != own_i.len() || ts.inputs.len() != is.inputs.len() {
                let (f, s) = (self.def(im).file, self.def(im).span);
                self.err(
                    "T035",
                    f,
                    s,
                    format!("method `{tn}` does not match its declaration in trait `{tname}`"),
                );
                continue;
            }
            let mut p = map_params.clone();
            p.extend(own_t.iter().copied());
            let mut a = map_args.clone();
            a.extend(own_i.iter().map(|q| Ty::Param(*q)));
            let mut ok = true;
            for (x, y) in ts.inputs.iter().zip(&is.inputs) {
                if x.mode != y.mode || x.is_self != y.is_self || x.ty.subst(&p, &a) != y.ty {
                    ok = false;
                }
            }
            if ts.ret.subst(&p, &a) != is.ret || ts.ret_borrow != is.ret_borrow {
                ok = false;
            }
            // The impl may not require more of its method's own generic
            // parameters than the trait does.
            if ok && !own_i.is_empty() {
                let tbounds: Vec<Bound> = self
                    .generics_of(tm)
                    .bounds
                    .iter()
                    .map(|b| Bound {
                        ty: b.ty.subst(&p, &a),
                        trait_: b.trait_,
                        args: b.args.iter().map(|x| x.subst(&p, &a)).collect(),
                    })
                    .collect();
                let tbounds = crate::typeck::close_bounds(self, tbounds);
                let ibounds = self.generics_of(im).bounds;
                for b in ibounds {
                    let own =
                        b.ty.any(&mut |t| matches!(t, Ty::Param(q) if own_i.contains(q)));
                    if own
                        && !tbounds
                            .iter()
                            .any(|x| x.ty == b.ty && x.trait_ == b.trait_ && x.args == b.args)
                    {
                        ok = false;
                    }
                }
            }
            if !ok {
                let (f, s) = (self.def(im).file, self.def(im).span);
                let want = self.describe_sig(&ts, &p, &a);
                self.err(
                    "T035",
                    f,
                    s,
                    format!("method `{tn}` does not match its declaration in trait `{tname}` (expected `{want}`)"),
                );
            }
        }
        map_params.clear();
        let _ = &mut map_params;
    }

    /// An impl's `type X = T` against the trait's `type X: Bounds`.
    fn check_assoc_type(
        &mut self,
        td: DefId,
        tm: DefId,
        im: DefId,
        imp: DefId,
        map_params: &[DefId],
        map_args: &[Ty],
    ) {
        let Some(ast::Item {
            kind: ItemKind::TypeAlias { ty, .. },
            ..
        }) = self.def(im).item
        else {
            return;
        };
        let ig = self.generics_of(imp);
        let mut iscope = self.scope_for(imp, &ig);
        iscope.self_ty = Some(self.impls[&imp].self_ty.clone());
        let concrete = self.lower_ty(ty, &iscope);
        if concrete.has_error() {
            return;
        }
        let Some(ast::Item {
            kind: ItemKind::AssocType { bounds, .. },
            ..
        }) = self.def(tm).item
        else {
            return;
        };
        let tg = self.generics_of(td);
        let tscope = self.scope_for(td, &tg);
        let ibounds = crate::typeck::close_bounds(self, ig.bounds.clone());
        for b in bounds {
            let Some(bd) = self.lower_bound(&concrete, b, &tscope) else {
                continue;
            };
            let args: Vec<Ty> = bd
                .args
                .iter()
                .map(|x| x.subst(map_params, map_args))
                .collect();
            if !self.implements(&ibounds, &concrete, bd.trait_, &args, 0) {
                let (f, s) = (self.def(im).file, self.def(im).span);
                let (c, tn, xn) = (
                    self.ty_str(&concrete),
                    self.def(bd.trait_).name.clone(),
                    self.def(tm).name.clone(),
                );
                self.err(
                    "T037",
                    f,
                    s,
                    format!("associated type `{xn}` = `{c}` does not implement `{tn}`, which the trait requires"),
                );
            }
        }
    }

    /// Does `ty` implement `tr<args>`? `bounds` are the (closed) bounds in
    /// scope for generic parameters; other types are matched against impls.
    pub fn implements(
        &mut self,
        bounds: &[Bound],
        ty: &Ty,
        tr: DefId,
        args: &[Ty],
        depth: u32,
    ) -> bool {
        if depth > 32 {
            return false;
        }
        if let Ty::Param(_) = ty {
            return bounds
                .iter()
                .any(|b| b.ty == *ty && b.trait_ == tr && b.args == args);
        }
        // `Copy` is built in for primitives, pointers, tuples and arrays
        // (spec/purm.md §1); user types need an `impl Copy`.
        if tr == crate::purm::copy_trait(self) {
            match ty {
                Ty::Int(_)
                | Ty::Float(_)
                | Ty::Bool
                | Ty::Char
                | Ty::Str
                | Ty::Ptr(..)
                | Ty::Region
                | Ty::Never
                | Ty::Error => return true,
                Ty::Fn(..) => return false,
                Ty::Tuple(v) => {
                    return v
                        .iter()
                        .all(|x| self.implements(bounds, x, tr, args, depth + 1))
                }
                Ty::Array(e, _) => return self.implements(bounds, e, tr, args, depth + 1),
                _ => {}
            }
        }
        for imp in self.trait_impls.get(&tr).cloned().unwrap_or_default() {
            let g = self.generics_of(imp);
            let mut t = crate::infer::InferTable::default();
            let vars: Vec<Ty> = g
                .params
                .iter()
                .map(|_| t.fresh(crate::infer::VarKind::General))
                .collect();
            let Some(id) = self.impls.get(&imp).cloned() else {
                continue;
            };
            let st = id.self_ty.subst(&g.params, &vars);
            let ta: Vec<Ty> = id
                .trait_ref
                .as_ref()
                .unwrap()
                .1
                .iter()
                .map(|x| x.subst(&g.params, &vars))
                .collect();
            if t.unify(&st, ty).is_err() || ta.len() != args.len() {
                continue;
            }
            if ta.iter().zip(args).any(|(x, y)| t.unify(x, y).is_err()) {
                continue;
            }
            let ok = g.bounds.iter().all(|b| {
                let bt = t.resolve(&b.ty.subst(&g.params, &vars));
                let ba: Vec<Ty> = b
                    .args
                    .iter()
                    .map(|x| t.resolve(&x.subst(&g.params, &vars)))
                    .collect();
                self.implements(bounds, &bt, b.trait_, &ba, depth + 1)
            });
            if ok {
                return true;
            }
        }
        false
    }

    fn describe_sig(&self, s: &FnSigTy, p: &[DefId], a: &[Ty]) -> String {
        let ins: Vec<String> = s
            .inputs
            .iter()
            .map(|i| {
                let m = match i.mode {
                    Mode::In => "",
                    Mode::Inout => "inout ",
                    Mode::Own => "own ",
                };
                if i.is_self {
                    format!("{m}self")
                } else {
                    format!("{}: {m}{}", i.name, self.ty_str(&i.ty.subst(p, a)))
                }
            })
            .collect();
        let b = if s.ret_borrow { "in " } else { "" };
        format!(
            "fn({}) -> {b}{}",
            ins.join(", "),
            self.ty_str(&s.ret.subst(p, a))
        )
    }
}
