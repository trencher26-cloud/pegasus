//! Lowering typed HIR to PIR (spec/pir.md §3): monomorphization, trait
//! method resolution, pattern compilation, control flow, `defer`, `?`,
//! contracts as runtime checks, and closure lifting.

use crate::ir::*;
use pegasus_parser::ast::Span;
use pegasus_sema::defs::{DefId, DefKind};
use pegasus_sema::hir::{self, ExprKind as HE, LocalId, PatKind, Stmt};
use pegasus_sema::infer::{InferTable, VarKind};
use pegasus_sema::items::{AdtKind, Sema, VariantShape};
use pegasus_sema::lit::Lit;
use pegasus_sema::source::FileId;
use pegasus_sema::ty::{ConstVal, IntTy, Ty};
use std::collections::HashMap;

#[derive(Clone, Debug)]
pub struct LowerErr {
    pub code: &'static str,
    pub file: FileId,
    pub span: Span,
    pub message: String,
}

pub struct Lowerer<'s, 'a> {
    pub s: &'s mut Sema<'a>,
    pub m: Module,
    funcs: HashMap<(DefId, Vec<Ty>), FuncId>,
    consts: HashMap<DefId, FuncId>,
    structs: HashMap<(DefId, Vec<Ty>), StructId>,
    enums: HashMap<(DefId, Vec<Ty>), EnumId>,
    queue: Vec<Job>,
    pub errs: Vec<LowerErr>,
    /// How each function receives its parameters.
    modes: HashMap<FuncId, Vec<PMode>>,
    /// By-value wrappers used when a function is taken as a value.
    wrappers: HashMap<FuncId, FuncId>,
}

/// Parameter passing in PIR (spec/pir.md §2.1).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PMode {
    /// `own` parameters, extern functions and closures: the value itself.
    Value,
    /// `in`: a shared reference to the caller's place.
    Shared,
    /// `inout`: an exclusive reference to the caller's place.
    Excl,
}

enum Job {
    Fn(FuncId, DefId, Vec<Ty>),
    Const(FuncId, DefId),
}

/// Lower every non-generic function of the user package (and, on demand,
/// every generic instance and constant they use).
/// Returns the module, the function for every root definition, and errors.
pub fn lower_package(s: &mut Sema) -> (Module, HashMap<DefId, FuncId>, Vec<LowerErr>) {
    let mut l = Lowerer {
        s,
        m: Module::default(),
        funcs: HashMap::new(),
        consts: HashMap::new(),
        structs: HashMap::new(),
        enums: HashMap::new(),
        queue: Vec::new(),
        errs: Vec::new(),
        modes: HashMap::new(),
        wrappers: HashMap::new(),
    };
    let candidates: Vec<DefId> =
        l.s.bodies
            .keys()
            .copied()
            .filter(|&d| l.s.def(d).kind == DefKind::Fn && l.s.def(d).file != 0)
            .collect();
    let mut roots: Vec<DefId> = candidates
        .into_iter()
        .filter(|&d| l.s.generics_of(d).params.is_empty())
        .collect();
    roots.sort();
    let mut root_ids = HashMap::new();
    for d in roots {
        let f = l.func(d, Vec::new());
        root_ids.insert(d, f);
    }
    while let Some(job) = l.queue.pop() {
        match job {
            Job::Fn(fid, d, args) => l.lower_fn(fid, d, &args),
            Job::Const(fid, d) => l.lower_const(fid, d),
        }
    }
    (l.m, root_ids, l.errs)
}

impl<'s, 'a> Lowerer<'s, 'a> {
    fn err(&mut self, code: &'static str, file: FileId, span: Span, message: String) {
        self.errs.push(LowerErr {
            code,
            file,
            span,
            message,
        });
    }

    // ------------------------------------------------------------ types

    pub fn ty(&mut self, t: &Ty) -> Type {
        match t {
            Ty::Int(i) => Type::Int(*i),
            Ty::Float(f) => Type::Float(*f),
            Ty::Bool => Type::Bool,
            Ty::Char => Type::Char,
            Ty::Str => Type::Str,
            Ty::Tuple(v) => Type::Tuple(v.iter().map(|x| self.ty(x)).collect()),
            Ty::Array(e, n) => {
                let len = match **n {
                    Ty::Const(ConstVal::Int(c)) => c.to_usize().map(|x| x as u64).unwrap_or(0),
                    _ => 0,
                };
                Type::Array(Box::new(self.ty(e)), len)
            }
            Ty::Adt(d, args) => match self.s.def(*d).kind {
                DefKind::Struct => Type::Struct(self.struct_id(*d, args)),
                _ => Type::Enum(self.enum_id(*d, args)),
            },
            Ty::Fn(p, r) => Type::Fn(p.iter().map(|x| self.ty(x)).collect(), Box::new(self.ty(r))),
            Ty::Ptr(m, t) => Type::Ptr(*m, Box::new(self.ty(t))),
            Ty::Region => Type::Region,
            // Never-typed values are never materialized; Param/Infer/Error do
            // not survive monomorphization of an error-free package.
            Ty::Never | Ty::Param(_) | Ty::Const(_) | Ty::Infer(_) | Ty::Error => Type::unit(),
        }
    }

    fn adt_name(&self, d: DefId, args: &[Ty]) -> String {
        self.s.ty_str(&Ty::Adt(d, args.to_vec()))
    }

    fn struct_id(&mut self, d: DefId, args: &[Ty]) -> StructId {
        let key = (d, args.to_vec());
        if let Some(&id) = self.structs.get(&key) {
            return id;
        }
        let id = self.m.structs.len() as StructId;
        let name = self.adt_name(d, args);
        self.m.structs.push(StructDef {
            name,
            fields: Vec::new(),
        });
        self.structs.insert(key, id);
        let a = self.s.adt(d);
        let AdtKind::Struct(fs) = &a.kind else {
            unreachable!()
        };
        let fields: Vec<(String, Type)> = fs
            .iter()
            .map(|f| (f.name.clone(), self.ty(&f.ty.subst(&a.params, args))))
            .collect();
        self.m.structs[id as usize].fields = fields;
        id
    }

    fn enum_id(&mut self, d: DefId, args: &[Ty]) -> EnumId {
        let key = (d, args.to_vec());
        if let Some(&id) = self.enums.get(&key) {
            return id;
        }
        let id = self.m.enums.len() as EnumId;
        let name = self.adt_name(d, args);
        self.m.enums.push(EnumDef {
            name,
            variants: Vec::new(),
        });
        self.enums.insert(key, id);
        let a = self.s.adt(d);
        let AdtKind::Enum(vs) = &a.kind else {
            unreachable!()
        };
        let mut variants = Vec::new();
        for (v, shape) in vs.clone() {
            let tys: Vec<Ty> = match shape {
                VariantShape::Unit => vec![],
                VariantShape::Tuple(ts) => ts,
                VariantShape::Struct(fs) => fs.into_iter().map(|f| f.ty).collect(),
            };
            let pts = tys
                .iter()
                .map(|t| self.ty(&t.subst(&a.params, args)))
                .collect();
            variants.push((self.s.def(v).name.clone(), pts));
        }
        self.m.enums[id as usize].variants = variants;
        id
    }

    fn variant_index(&self, v: DefId) -> u32 {
        let e = self.s.def(v).parent.unwrap();
        self.s.t.variants[&e].iter().position(|&x| x == v).unwrap() as u32
    }

    fn named_variant(&self, enum_def: DefId, name: &str) -> u32 {
        self.s.t.variants[&enum_def]
            .iter()
            .position(|&x| self.s.def(x).name == name)
            .unwrap() as u32
    }

    // ------------------------------------------------------------ functions

    /// Resolve a trait method to the implementation for its concrete `Self`.
    fn resolve_method(&mut self, d: DefId, args: &[Ty]) -> (DefId, Vec<Ty>) {
        let Some(tr) = self
            .s
            .def(d)
            .parent
            .filter(|&p| self.s.def(p).kind == DefKind::Trait)
        else {
            return (d, args.to_vec());
        };
        let n_trait = self.s.generics_of(tr).params.len();
        let self_ty = args[0].clone();
        let name = self.s.def(d).name.clone();
        for imp in self.s.trait_impls.get(&tr).cloned().unwrap_or_default() {
            let g = self.s.generics_of(imp);
            let mut tab = InferTable::default();
            let vars: Vec<Ty> = g
                .params
                .iter()
                .map(|_| tab.fresh(VarKind::General))
                .collect();
            let id = self.s.impls[&imp].clone();
            if tab
                .unify(&id.self_ty.subst(&g.params, &vars), &self_ty)
                .is_err()
            {
                continue;
            }
            let targs = &id.trait_ref.as_ref().unwrap().1;
            if targs
                .iter()
                .zip(&args[1..n_trait])
                .any(|(a, b)| tab.unify(&a.subst(&g.params, &vars), b).is_err())
            {
                continue;
            }
            let member = self.s.t.members.get(&imp).and_then(|ms| {
                ms.iter()
                    .copied()
                    .find(|&m| self.s.def(m).name == name && self.s.def(m).kind == DefKind::Fn)
            });
            return match member {
                Some(m) => {
                    let mut na: Vec<Ty> = vars.iter().map(|v| tab.resolve(v)).collect();
                    na.extend_from_slice(&args[n_trait..]);
                    (m, na)
                }
                None => (d, args.to_vec()),
            };
        }
        (d, args.to_vec())
    }

    pub fn func(&mut self, d: DefId, args: Vec<Ty>) -> FuncId {
        let (d, args) = self.resolve_method(d, &args);
        let key = (d, args.clone());
        if let Some(&f) = self.funcs.get(&key) {
            return f;
        }
        let sig = self.s.sig(d);
        let is_extern = self.s.def(d).kind == DefKind::ExternFn;
        let pmodes: Vec<PMode> = sig
            .inputs
            .iter()
            .map(|p| match p.mode {
                _ if is_extern => PMode::Value,
                pegasus_sema::items::Mode::Own => PMode::Value,
                pegasus_sema::items::Mode::In => PMode::Shared,
                pegasus_sema::items::Mode::Inout => PMode::Excl,
            })
            .collect();
        let mut params: Vec<Type> = Vec::new();
        for (p, m) in sig.inputs.iter().zip(&pmodes) {
            let t = p.ty.subst(&sig.generics, &args);
            let t = self.ty(&t);
            params.push(match m {
                PMode::Value => t,
                PMode::Shared => Type::Ref {
                    mutable: false,
                    ty: Box::new(t),
                },
                PMode::Excl => Type::Ref {
                    mutable: true,
                    ty: Box::new(t),
                },
            });
        }
        let ret_ty = sig.ret.subst(&sig.generics, &args);
        let ret = self.ty(&ret_ty);
        let ret = if sig.ret_borrow && !is_extern {
            Type::Ref {
                mutable: false,
                ty: Box::new(ret),
            }
        } else {
            ret
        };
        let mut name = self.s.def(d).name.clone();
        if let Some(p) = self.s.def(d).parent {
            if self.s.def(p).kind == DefKind::Impl {
                let gp = self.s.generics_of(p).params;
                let st = self.s.impls[&p].self_ty.subst(&gp, &args);
                name = format!("{}.{name}", self.s.ty_str(&st));
            } else if self.s.def(p).kind == DefKind::Trait {
                name = format!("{}.{name}", self.s.ty_str(&args[0]));
            }
        }
        if !sig.own_generics.is_empty() {
            let n = sig.generics.len() - sig.own_generics.len();
            let own: Vec<String> = args[n..].iter().map(|t| self.s.ty_str(t)).collect();
            name = format!("{name}<{}>", own.join(", "));
        }
        let kind = match self.s.def(d).kind {
            DefKind::ExternFn => {
                let abi = match self.s.def(d).item.map(|i| &i.kind) {
                    Some(pegasus_parser::ast::ItemKind::ExternFn { abi, .. }) => abi.clone(),
                    _ => "C".into(),
                };
                FuncKind::Extern(abi)
            }
            _ => FuncKind::Defined,
        };
        let fid = self.m.funcs.len() as FuncId;
        self.modes.insert(fid, pmodes);
        self.m.funcs.push(Func {
            name,
            kind: kind.clone(),
            params,
            ret,
            slots: vec![],
            slot_names: vec![],
            values: vec![],
            blocks: vec![],
        });
        self.funcs.insert(key, fid);
        if kind == FuncKind::Defined {
            self.queue.push(Job::Fn(fid, d, args));
        }
        fid
    }

    /// A by-value entry point for `f`, so it can be a function value
    /// (function types carry no parameter modes; sema allows only functions
    /// whose parameters are all `in` and whose result is owned: T065).
    fn value_wrapper(&mut self, f: FuncId) -> FuncId {
        let modes = self.modes.get(&f).cloned().unwrap_or_default();
        if modes.iter().all(|m| *m == PMode::Value) {
            return f;
        }
        if let Some(&w) = self.wrappers.get(&f) {
            return w;
        }
        let callee = self.m.funcs[f as usize].clone();
        let vparams: Vec<Type> = callee
            .params
            .iter()
            .map(|t| match t {
                Type::Ref { ty, .. } => (**ty).clone(),
                t => t.clone(),
            })
            .collect();
        let mut values = vparams.clone();
        let mut slots = Vec::new();
        let mut insts = Vec::new();
        let mut args = Vec::new();
        for (i, (pt, m)) in callee.params.iter().zip(&modes).enumerate() {
            if *m == PMode::Value {
                args.push(i as ValueId);
                continue;
            }
            slots.push(vparams[i].clone());
            let s = (slots.len() - 1) as SlotId;
            insts.push(Inst {
                result: None,
                kind: InstKind::Store {
                    slot: s,
                    path: vec![],
                    v: i as ValueId,
                },
            });
            values.push(pt.clone());
            let r = (values.len() - 1) as ValueId;
            insts.push(Inst {
                result: Some(r),
                kind: InstKind::Borrow {
                    slot: s,
                    path: vec![],
                    mutable: *m == PMode::Excl,
                },
            });
            args.push(r);
        }
        values.push(callee.ret.clone());
        let res = (values.len() - 1) as ValueId;
        insts.push(Inst {
            result: Some(res),
            kind: InstKind::Call { f, args },
        });
        let w = self.m.funcs.len() as FuncId;
        let n = slots.len();
        self.m.funcs.push(Func {
            name: format!("{}.value", callee.name),
            kind: FuncKind::Defined,
            params: vparams.clone(),
            ret: callee.ret.clone(),
            slots,
            slot_names: vec![String::new(); n],
            values,
            blocks: vec![Block {
                params: (0..vparams.len() as ValueId).collect(),
                insts,
                term: Term::Ret(res),
            }],
        });
        self.modes.insert(w, vec![PMode::Value; vparams.len()]);
        self.wrappers.insert(f, w);
        w
    }

    fn const_fn(&mut self, d: DefId) -> FuncId {
        if let Some(&f) = self.consts.get(&d) {
            return f;
        }
        let t = self.s.const_ty(d);
        let ret = self.ty(&t);
        let fid = self.m.funcs.len() as FuncId;
        let name = format!("const.{}", self.s.def(d).name);
        self.m.funcs.push(Func {
            name,
            kind: FuncKind::Defined,
            params: vec![],
            ret,
            slots: vec![],
            slot_names: vec![],
            values: vec![],
            blocks: vec![],
        });
        self.consts.insert(d, fid);
        self.queue.push(Job::Const(fid, d));
        fid
    }

    fn lower_fn(&mut self, fid: FuncId, d: DefId, args: &[Ty]) {
        let Some(body) = self.s.bodies.get(&d).cloned() else {
            return;
        };
        let sig = self.s.sig(d);
        let file = self.s.def(d).file;
        let subst = (sig.generics.clone(), args.to_vec());
        let params = self.m.funcs[fid as usize].params.clone();
        let ret = self.m.funcs[fid as usize].ret.clone();
        let pmodes = self.modes.get(&fid).cloned().unwrap_or_default();
        let ret_ref = matches!(ret, Type::Ref { .. });
        let mut fx = Fx::new(self, file, subst, &body, params.clone(), ret);
        fx.ret_ref = ret_ref;
        // Parameters go to their slots; a reference parameter's slot holds
        // the reference.
        let entry_params = fx.entry_params.clone();
        for (i, &l) in body.params.iter().enumerate() {
            match pmodes.get(i) {
                Some(PMode::Shared) => {
                    fx.ref_locals.insert(l, false);
                }
                Some(PMode::Excl) => {
                    fx.ref_locals.insert(l, true);
                }
                _ => {}
            }
            if sig.inputs[i].mode == pegasus_sema::items::Mode::Own {
                fx.own_local(l);
            }
            let slot = fx.slot_of(l);
            fx.emit(
                InstKind::Store {
                    slot,
                    path: vec![],
                    v: entry_params[i],
                },
                None,
            );
        }
        fx.ensures = body.ensures.clone();
        fx.result_local = body.result_local;
        for r in &body.requires {
            if let Some(c) = fx.expr(r) {
                fx.check(c, "precondition (`requires`) violated");
            }
        }
        let v = if ret_ref {
            fx.expr_ref(&body.value)
        } else {
            fx.expr(&body.value)
        };
        if let Some(v) = v {
            fx.emit_return(v);
        }
        let f = fx.finish();
        let slot = &mut self.m.funcs[fid as usize];
        slot.slots = f.slots;
        slot.slot_names = f.slot_names;
        slot.values = f.values;
        slot.blocks = f.blocks;
    }

    fn lower_const(&mut self, fid: FuncId, d: DefId) {
        let Some(body) = self.s.bodies.get(&d).cloned() else {
            let (f, sp) = (self.s.def(d).file, self.s.def(d).span);
            self.err("L001", f, sp, "constant without a value".into());
            return;
        };
        let file = self.s.def(d).file;
        let ret = self.m.funcs[fid as usize].ret.clone();
        let mut fx = Fx::new(self, file, (vec![], vec![]), &body, vec![], ret);
        if let Some(v) = fx.expr(&body.value) {
            fx.emit_return(v);
        }
        let f = fx.finish();
        let slot = &mut self.m.funcs[fid as usize];
        slot.slots = f.slots;
        slot.slot_names = f.slot_names;
        slot.values = f.values;
        slot.blocks = f.blocks;
    }
}

// ------------------------------------------------------------ function builder

struct LoopFrame {
    brk: BlockId,
    cont: BlockId,
    /// Value type passed to `brk` (a `loop` with break values), if any.
    brk_val: bool,
    defer_depth: usize,
}

struct Fx<'l, 's, 'a> {
    l: &'l mut Lowerer<'s, 'a>,
    file: FileId,
    subst: (Vec<DefId>, Vec<Ty>),
    hlocals: Vec<hir::Local>,
    slots: Vec<Type>,
    slot_names: Vec<String>,
    local_slot: HashMap<LocalId, SlotId>,
    values: Vec<Type>,
    blocks: Vec<(Vec<ValueId>, Vec<Inst>, Option<Term>)>,
    cur: BlockId,
    entry_params: Vec<ValueId>,
    ret: Type,
    loops: Vec<LoopFrame>,
    defers: Vec<Vec<hir::Expr>>,
    /// Owned non-Copy locals bound in each scope (parallel to `defers`),
    /// destroyed in reverse order at every exit from the scope.
    owned: Vec<Vec<LocalId>>,
    needs_drop_cache: HashMap<Ty, bool>,
    ensures: Vec<hir::Expr>,
    result_local: Option<LocalId>,
    /// Locals captured by the closure being lowered (assignment is not
    /// implemented for them: PURM decides capture modes).
    captured: Vec<LocalId>,
    /// Reference parameters: their slot holds a `&T` (false) or `&var T` (true).
    ref_locals: HashMap<LocalId, bool>,
    /// This function returns a borrow (`-> in T`): its result is a reference.
    ret_ref: bool,
}

/// Where a place lives.
#[derive(Clone, Copy)]
enum Base {
    Slot(SlotId),
    /// Behind a reference value.
    Ref(ValueId),
}

impl<'l, 's, 'a> Fx<'l, 's, 'a> {
    fn new(
        l: &'l mut Lowerer<'s, 'a>,
        file: FileId,
        subst: (Vec<DefId>, Vec<Ty>),
        body: &hir::Body,
        params: Vec<Type>,
        ret: Type,
    ) -> Self {
        let mut fx = Fx {
            l,
            file,
            subst,
            hlocals: body.locals.clone(),
            slots: vec![],
            slot_names: vec![],
            local_slot: HashMap::new(),
            values: vec![],
            blocks: vec![],
            cur: 0,
            entry_params: vec![],
            ret,
            loops: vec![],
            defers: vec![vec![]],
            owned: vec![vec![]],
            needs_drop_cache: HashMap::new(),
            ensures: vec![],
            result_local: None,
            captured: vec![],
            ref_locals: HashMap::new(),
            ret_ref: false,
        };
        let (b, ps) = fx.new_block(&params);
        fx.cur = b;
        fx.entry_params = ps;
        fx
    }

    fn finish(mut self) -> Func {
        let blocks = std::mem::take(&mut self.blocks)
            .into_iter()
            .map(|(params, insts, term)| Block {
                params,
                insts,
                term: term.unwrap_or(Term::Unreachable),
            })
            .collect();
        Func {
            name: String::new(),
            kind: FuncKind::Defined,
            params: vec![],
            ret: self.ret.clone(),
            slots: std::mem::take(&mut self.slots),
            slot_names: std::mem::take(&mut self.slot_names),
            values: std::mem::take(&mut self.values),
            blocks,
        }
    }

    fn sty(&mut self, t: &Ty) -> Type {
        let t = t.subst(&self.subst.0, &self.subst.1);
        self.l.ty(&t)
    }

    fn subst_ty(&self, t: &Ty) -> Ty {
        t.subst(&self.subst.0, &self.subst.1)
    }

    fn new_value(&mut self, t: Type) -> ValueId {
        self.values.push(t);
        (self.values.len() - 1) as ValueId
    }

    fn new_block(&mut self, params: &[Type]) -> (BlockId, Vec<ValueId>) {
        let ps: Vec<ValueId> = params.iter().map(|t| self.new_value(t.clone())).collect();
        self.blocks.push((ps.clone(), vec![], None));
        ((self.blocks.len() - 1) as BlockId, ps)
    }

    fn emit(&mut self, kind: InstKind, ty: Option<Type>) -> Option<ValueId> {
        let result = ty.map(|t| self.new_value(t));
        self.blocks[self.cur as usize].1.push(Inst { result, kind });
        result
    }

    fn val(&mut self, kind: InstKind, ty: Type) -> ValueId {
        self.emit(kind, Some(ty)).unwrap()
    }

    fn term(&mut self, t: Term) {
        let b = &mut self.blocks[self.cur as usize].2;
        if b.is_none() {
            *b = Some(t);
        }
    }

    fn switch_to(&mut self, b: BlockId) {
        self.cur = b;
    }

    fn unit(&mut self) -> ValueId {
        self.val(InstKind::Tuple(vec![]), Type::unit())
    }

    fn konst(&mut self, c: Const) -> ValueId {
        let t = match &c {
            Const::Int(_, t) => Type::Int(*t),
            Const::Float(_, t) => Type::Float(*t),
            Const::Bool(_) => Type::Bool,
            Const::Char(_) => Type::Char,
            Const::Str(_) => Type::Str,
        };
        self.val(InstKind::Const(c), t)
    }

    fn slot_of(&mut self, l: LocalId) -> SlotId {
        if let Some(&s) = self.local_slot.get(&l) {
            return s;
        }
        let t = self.hlocals[l as usize].ty.clone();
        let pt = self.sty(&t);
        let pt = match self.ref_locals.get(&l) {
            Some(m) => Type::Ref {
                mutable: *m,
                ty: Box::new(pt),
            },
            None => pt,
        };
        self.slots.push(pt);
        self.slot_names.push(self.hlocals[l as usize].name.clone());
        let s = (self.slots.len() - 1) as SlotId;
        self.local_slot.insert(l, s);
        s
    }

    /// Branch to a trap unless `c` holds.
    fn check(&mut self, c: ValueId, msg: &str) {
        let (ok, _) = self.new_block(&[]);
        let (bad, _) = self.new_block(&[]);
        self.term(Term::CondBr {
            c,
            t: (ok, vec![]),
            f: (bad, vec![]),
        });
        self.switch_to(bad);
        self.term(Term::Trap(msg.into()));
        self.switch_to(ok);
    }

    /// Leave scopes `down_to..` (innermost first): each scope's deferred
    /// expressions in reverse order, then its owned locals in reverse
    /// declaration order (spec/purm.md §4–5).
    fn emit_defers(&mut self, down_to: usize) {
        for depth in (down_to..self.defers.len()).rev() {
            let ds: Vec<hir::Expr> = self.defers[depth].clone();
            for e in ds.iter().rev() {
                if self.expr(e).is_none() {
                    return;
                }
            }
            self.emit_drops(depth);
        }
    }

    fn emit_drops(&mut self, depth: usize) {
        let owned: Vec<LocalId> = self.owned[depth].clone();
        for l in owned.iter().rev() {
            let slot = self.slot_of(*l);
            self.emit(InstKind::Drop { slot }, None);
        }
    }

    fn push_scope(&mut self) {
        self.defers.push(Vec::new());
        self.owned.push(Vec::new());
    }

    /// Normal end of the innermost scope: its defers and drops, then pop.
    /// Returns false if a deferred expression diverged.
    fn end_scope(&mut self) -> bool {
        let depth = self.defers.len() - 1;
        let ds: Vec<hir::Expr> = self.defers[depth].clone();
        let mut ok = true;
        for e in ds.iter().rev() {
            if self.expr(e).is_none() {
                ok = false;
                break;
            }
        }
        if ok {
            self.emit_drops(depth);
        }
        self.defers.pop();
        self.owned.pop();
        ok
    }

    /// Pop the innermost scope without emitting its exit code: every path
    /// out of it has already emitted it (or diverged).
    fn drop_scope(&mut self) {
        self.defers.pop();
        self.owned.pop();
    }

    fn needs_drop(&mut self, t: &Ty) -> bool {
        let t = self.subst_ty(t);
        if let Some(&b) = self.needs_drop_cache.get(&t) {
            return b;
        }
        let b = !pegasus_sema::purm::is_copy_concrete(self.l.s, &t);
        self.needs_drop_cache.insert(t, b);
        b
    }

    /// Record that the current scope owns local `l`.
    fn own_local(&mut self, l: LocalId) {
        let t = self.hlocals[l as usize].ty.clone();
        if self.needs_drop(&t) && !self.owned.iter().flatten().any(|x| *x == l) {
            self.owned.last_mut().unwrap().push(l);
        }
    }

    fn emit_return(&mut self, v: ValueId) {
        self.emit_defers(0);
        if !self.ensures.is_empty() {
            if let Some(rl) = self.result_local {
                let s = self.slot_of(rl);
                let rv = if self.ret_ref {
                    let t = self.slots[s as usize].clone();
                    self.val(InstKind::LoadRef { r: v, path: vec![] }, t)
                } else {
                    v
                };
                self.emit(
                    InstKind::Store {
                        slot: s,
                        path: vec![],
                        v: rv,
                    },
                    None,
                );
            }
            for e in self.ensures.clone() {
                if let Some(c) = self.expr(&e) {
                    self.check(c, "postcondition (`ensures`) violated");
                }
            }
        }
        self.term(Term::Ret(v));
    }

    // ------------------------------------------------------------ blocks

    fn block(&mut self, b: &hir::Block) -> Option<ValueId> {
        self.block_mode(b, false)
    }

    fn block_mode(&mut self, b: &hir::Block, want_ref: bool) -> Option<ValueId> {
        self.push_scope();
        for s in &b.stmts {
            if self.stmt(s).is_none() {
                self.drop_scope();
                return None;
            }
        }
        let v = match &b.tail {
            Some(t) => self.expr_mode(t, want_ref),
            None => Some(self.unit()),
        };
        let Some(v) = v else {
            self.drop_scope();
            return None;
        };
        if !self.end_scope() {
            return None;
        }
        Some(v)
    }

    fn stmt(&mut self, s: &Stmt) -> Option<()> {
        match s {
            Stmt::Let { pat, init } => {
                let v = self.expr(init)?;
                let (fail, _) = self.new_block(&[]);
                self.bind_pat(pat, v, fail);
                let cur = self.cur;
                self.switch_to(fail);
                self.term(Term::Trap("irrefutable pattern did not match".into()));
                self.switch_to(cur);
            }
            Stmt::Assign { place, op, value } => {
                let (base, path) = self.place(place, true)?;
                let v = self.expr(value)?;
                let ty = self.sty(&place.ty);
                match base {
                    Base::Slot(slot) => {
                        if op.is_none() && path.is_empty() && self.needs_drop(&place.ty) {
                            // The old value (if the local holds one) is destroyed first.
                            self.emit(InstKind::Drop { slot }, None);
                        }
                        let nv = match op {
                            None => v,
                            Some(op) => {
                                let old = self.val(
                                    InstKind::Load {
                                        slot,
                                        path: path.clone(),
                                    },
                                    ty.clone(),
                                );
                                self.binop(op, old, v, ty)
                            }
                        };
                        self.emit(InstKind::Store { slot, path, v: nv }, None);
                    }
                    Base::Ref(r) => {
                        // Writing through an `inout` parameter: the caller's place.
                        let nv = match op {
                            None => v,
                            Some(op) => {
                                let old = self.val(
                                    InstKind::LoadRef {
                                        r,
                                        path: path.clone(),
                                    },
                                    ty.clone(),
                                );
                                self.binop(op, old, v, ty)
                            }
                        };
                        self.emit(InstKind::StoreRef { r, path, v: nv }, None);
                    }
                }
            }
            Stmt::Expr(e) => {
                self.expr(e)?;
            }
            Stmt::Defer(e) => {
                self.defers.last_mut().unwrap().push(e.clone());
            }
            Stmt::Region { body, .. } => {
                self.block(body)?;
            }
        }
        Some(())
    }

    /// The storage location of a place expression.
    /// `for_write`: the place is assigned or borrowed exclusively.
    fn place(&mut self, e: &hir::Expr, for_write: bool) -> Option<(Base, Vec<Proj>)> {
        match &e.kind {
            HE::Local(l) => {
                if for_write && self.captured.contains(l) {
                    let n = self.hlocals[*l as usize].name.clone();
                    self.l.err(
                        "L002",
                        self.file,
                        e.span,
                        format!("assigning to `{n}` from inside a closure is not implemented yet (capture modes belong to PURM)"),
                    );
                }
                let s = self.slot_of(*l);
                if self.ref_locals.contains_key(l) {
                    let t = self.slots[s as usize].clone();
                    let r = self.val(
                        InstKind::Load {
                            slot: s,
                            path: vec![],
                        },
                        t,
                    );
                    return Some((Base::Ref(r), vec![]));
                }
                Some((Base::Slot(s), vec![]))
            }
            HE::Field { recv, index } | HE::TupleField { recv, index } => {
                let (s, mut p) = self.place(recv, for_write)?;
                p.push(Proj::Field(*index as u32));
                Some((s, p))
            }
            HE::Index { recv, index } => {
                let (s, mut p) = self.place(recv, for_write)?;
                let i = self.expr(index)?;
                p.push(Proj::Index(i));
                Some((s, p))
            }
            _ => {
                self.l.err(
                    "L001",
                    self.file,
                    e.span,
                    "unsupported assignment target".into(),
                );
                None
            }
        }
    }

    fn is_place(e: &hir::Expr) -> bool {
        match &e.kind {
            HE::Local(_) => true,
            HE::Field { recv, .. } | HE::TupleField { recv, .. } | HE::Index { recv, .. } => {
                Self::is_place(recv)
            }
            _ => false,
        }
    }

    /// A reference to the argument for an `in` (`mutable` false) or `inout`
    /// parameter: places are borrowed where they are; borrowed call results
    /// are passed on; other values are spilled to a temporary slot.
    fn borrow_arg(&mut self, e: &hir::Expr, mutable: bool) -> Option<ValueId> {
        let ty = self.sty(&e.ty);
        let rty = Type::Ref {
            mutable,
            ty: Box::new(ty.clone()),
        };
        if Self::is_place(e) {
            let (base, path) = self.place(e, mutable)?;
            return Some(match base {
                Base::Slot(slot) => self.val(
                    InstKind::Borrow {
                        slot,
                        path,
                        mutable,
                    },
                    rty,
                ),
                Base::Ref(r) => self.val(InstKind::BorrowRef { r, path, mutable }, rty),
            });
        }
        if !mutable && self.is_ref_call(e) {
            return self.expr_ref(e);
        }
        let v = self.expr(e)?;
        self.slots.push(ty);
        self.slot_names.push(String::new());
        let slot = (self.slots.len() - 1) as SlotId;
        self.emit(
            InstKind::Store {
                slot,
                path: vec![],
                v,
            },
            None,
        );
        Some(self.val(
            InstKind::Borrow {
                slot,
                path: vec![],
                mutable,
            },
            rty,
        ))
    }

    fn is_ref_call(&mut self, e: &hir::Expr) -> bool {
        match &e.kind {
            HE::Call { callee, .. } => match &callee.kind {
                HE::FnRef(d, _) => self.l.s.sigs.get(d).is_some_and(|s| s.ret_borrow),
                _ => false,
            },
            _ => false,
        }
    }

    /// A direct call: arguments passed by mode; the result is a reference
    /// for a borrowed-return callee (returned as is when `want_ref`, else read).
    fn call_direct(
        &mut self,
        d: DefId,
        gargs: &[Ty],
        args: &[hir::Expr],
        ty: &Type,
        want_ref: bool,
    ) -> Option<ValueId> {
        let gargs: Vec<Ty> = gargs.iter().map(|t| self.subst_ty(t)).collect();
        let f = self.l.func(d, gargs);
        let modes = self.l.modes.get(&f).cloned().unwrap_or_default();
        let mut vs = Vec::new();
        for (i, a) in args.iter().enumerate() {
            vs.push(match modes.get(i).copied().unwrap_or(PMode::Value) {
                PMode::Value => self.expr(a)?,
                PMode::Shared => self.borrow_arg(a, false)?,
                PMode::Excl => self.borrow_arg(a, true)?,
            });
        }
        let rt = self.l.m.funcs[f as usize].ret.clone();
        let r = self.val(InstKind::Call { f, args: vs }, rt.clone());
        if matches!(rt, Type::Ref { .. }) && !want_ref {
            return Some(self.val(InstKind::LoadRef { r, path: vec![] }, ty.clone()));
        }
        Some(r)
    }

    /// Lower `e` to a shared reference to the place it denotes (the value of
    /// a borrowed-return function). PURM's borrow_ok admits exactly these forms.
    fn expr_ref(&mut self, e: &hir::Expr) -> Option<ValueId> {
        let vty = self.sty(&e.ty);
        let rty = Type::Ref {
            mutable: false,
            ty: Box::new(vty.clone()),
        };
        if matches!(e.ty, Ty::Never) {
            return self.expr(e);
        }
        if Self::is_place(e) {
            return self.borrow_arg(e, false);
        }
        match &e.kind {
            HE::Call { callee, args } => match &callee.kind {
                HE::FnRef(d, gargs) if self.is_ref_call(e) => {
                    self.call_direct(*d, gargs, args, &vty, true)
                }
                _ => self.spill_ref(e),
            },
            HE::Field { recv, index } | HE::TupleField { recv, index } => {
                let r = self.expr_ref(recv)?;
                Some(self.val(
                    InstKind::BorrowRef {
                        r,
                        path: vec![Proj::Field(*index as u32)],
                        mutable: false,
                    },
                    rty,
                ))
            }
            HE::Index { recv, index } => {
                let r = self.expr_ref(recv)?;
                let i = self.expr(index)?;
                Some(self.val(
                    InstKind::BorrowRef {
                        r,
                        path: vec![Proj::Index(i)],
                        mutable: false,
                    },
                    rty,
                ))
            }
            HE::Block(b) | HE::Unsafe { body: b, .. } => self.block_mode(b, true),
            HE::If {
                cond,
                then,
                otherwise,
            } => self.if_mode(cond, then, otherwise.as_deref(), &rty, true),
            HE::IfLet {
                pat,
                value,
                then,
                otherwise,
            } => self.if_let_mode(pat, value, then, otherwise.as_deref(), &rty, true),
            HE::Match { scrutinee, arms } => self.match_mode(scrutinee, arms, &rty, true),
            _ => self.spill_ref(e),
        }
    }

    /// Not a borrowable form: PURM (O005) rejects returning it, so reaching
    /// here is a lowering error rather than a silent dangling reference.
    fn spill_ref(&mut self, e: &hir::Expr) -> Option<ValueId> {
        self.l.err(
            "L003",
            self.file,
            e.span,
            "this expression cannot be returned as a borrow".into(),
        );
        let v = self.expr(e)?;
        let ty = self.values[v as usize].clone();
        self.slots.push(ty.clone());
        self.slot_names.push(String::new());
        let slot = (self.slots.len() - 1) as SlotId;
        self.emit(
            InstKind::Store {
                slot,
                path: vec![],
                v,
            },
            None,
        );
        Some(self.val(
            InstKind::Borrow {
                slot,
                path: vec![],
                mutable: false,
            },
            Type::Ref {
                mutable: false,
                ty: Box::new(ty),
            },
        ))
    }

    fn expr_mode(&mut self, e: &hir::Expr, want_ref: bool) -> Option<ValueId> {
        if want_ref {
            self.expr_ref(e)
        } else {
            self.expr(e)
        }
    }

    fn if_mode(
        &mut self,
        cond: &hir::Expr,
        then: &hir::Block,
        otherwise: Option<&hir::Expr>,
        ty: &Type,
        want_ref: bool,
    ) -> Option<ValueId> {
        let c = self.expr(cond)?;
        let (tb, _) = self.new_block(&[]);
        let (eb, _) = self.new_block(&[]);
        self.term(Term::CondBr {
            c,
            t: (tb, vec![]),
            f: (eb, vec![]),
        });
        self.branches(
            ty,
            tb,
            eb,
            |fx| fx.block_mode(then, want_ref),
            |fx| match otherwise {
                Some(o) => fx.expr_mode(o, want_ref),
                None => Some(fx.unit()),
            },
        )
    }

    fn if_let_mode(
        &mut self,
        pat: &hir::Pat,
        value: &hir::Expr,
        then: &hir::Block,
        otherwise: Option<&hir::Expr>,
        ty: &Type,
        want_ref: bool,
    ) -> Option<ValueId> {
        let v = self.expr(value)?;
        let (tb, _) = self.new_block(&[]);
        let (eb, _) = self.new_block(&[]);
        // The bindings belong to the `then` branch only.
        self.push_scope();
        self.bind_pat(pat, v, eb);
        self.term(Term::Br(tb, vec![]));
        self.branches(
            ty,
            tb,
            eb,
            |fx| match fx.block_mode(then, want_ref) {
                Some(r) => fx.end_scope().then_some(r),
                None => {
                    fx.drop_scope();
                    None
                }
            },
            |fx| match otherwise {
                Some(o) => fx.expr_mode(o, want_ref),
                None => Some(fx.unit()),
            },
        )
    }

    fn match_mode(
        &mut self,
        scrutinee: &hir::Expr,
        arms: &[hir::Arm],
        ty: &Type,
        want_ref: bool,
    ) -> Option<ValueId> {
        let v = self.expr(scrutinee)?;
        let (join, jp) = self.new_block(std::slice::from_ref(ty));
        let mut reached = false;
        for arm in arms {
            let (next, _) = self.new_block(&[]);
            // Each arm's bindings are owned by the arm.
            self.push_scope();
            self.bind_pat(&arm.pat, v, next);
            if let Some(g) = &arm.guard {
                if let Some(c) = self.expr(g) {
                    let (body, _) = self.new_block(&[]);
                    self.term(Term::CondBr {
                        c,
                        t: (body, vec![]),
                        f: (next, vec![]),
                    });
                    self.switch_to(body);
                }
            }
            match self.expr_mode(&arm.body, want_ref) {
                Some(r) => {
                    if self.end_scope() {
                        self.term(Term::Br(join, vec![r]));
                        reached = true;
                    }
                }
                None => self.drop_scope(),
            }
            self.switch_to(next);
        }
        self.term(Term::Trap("no match arm matched".into()));
        self.switch_to(join);
        reached.then_some(jp[0])
    }

    // ------------------------------------------------------------ operators

    fn binop(&mut self, op: &str, a: ValueId, b: ValueId, ty: Type) -> ValueId {
        let (bop, ov) = match op {
            "+" => (BinOp::Add, Ov::Trap),
            "-" => (BinOp::Sub, Ov::Trap),
            "*" => (BinOp::Mul, Ov::Trap),
            "/" => (BinOp::Div, Ov::Trap),
            "%" => (BinOp::Rem, Ov::Trap),
            "**" => (BinOp::Pow, Ov::Trap),
            "<<" => (BinOp::Shl, Ov::Trap),
            ">>" => (BinOp::Shr, Ov::Trap),
            "&" => (BinOp::And, Ov::Trap),
            "|" => (BinOp::Or, Ov::Trap),
            "^" => (BinOp::Xor, Ov::Trap),
            "+%" => (BinOp::Add, Ov::Wrap),
            "-%" => (BinOp::Sub, Ov::Wrap),
            "*%" => (BinOp::Mul, Ov::Wrap),
            "+|" => (BinOp::Add, Ov::Sat),
            "-|" => (BinOp::Sub, Ov::Sat),
            _ => (BinOp::Mul, Ov::Sat),
        };
        self.val(InstKind::Bin { op: bop, ov, a, b }, ty)
    }

    fn lit(&mut self, l: &Lit, ty: &Type, neg: bool) -> ValueId {
        match (l, ty) {
            (Lit::Int(v, _), Type::Int(t)) => {
                let bits = if neg { (!*v).wrapping_add(1) } else { *v };
                let mask = if t.bits() == 128 {
                    u128::MAX
                } else {
                    (1u128 << t.bits()) - 1
                };
                self.konst(Const::Int(bits & mask, *t))
            }
            (Lit::Float(v, _), Type::Float(f)) => {
                self.konst(Const::Float(if neg { -*v } else { *v }, *f))
            }
            (Lit::Bool(b), _) => self.konst(Const::Bool(*b)),
            (Lit::Char(c), _) => self.konst(Const::Char(*c)),
            (Lit::Str(s), _) => self.konst(Const::Str(s.clone())),
            (Lit::Bytes(bs), _) => {
                let vs: Vec<ValueId> = bs
                    .iter()
                    .map(|b| self.konst(Const::Int(*b as u128, IntTy::U8)))
                    .collect();
                self.val(InstKind::Array(vs), ty.clone())
            }
            _ => self.konst(Const::Int(0, IntTy::I32)),
        }
    }

    // ------------------------------------------------------------ expressions

    /// Lower an expression; None if control does not continue past it.
    fn expr(&mut self, e: &hir::Expr) -> Option<ValueId> {
        let ty = self.sty(&e.ty);
        match &e.kind {
            HE::Lit(l) => Some(self.lit(l, &ty, false)),
            HE::Unary { op: "-", expr }
                if matches!(expr.kind, HE::Lit(Lit::Int(..)) | HE::Lit(Lit::Float(..))) =>
            {
                let HE::Lit(l) = &expr.kind else {
                    unreachable!()
                };
                Some(self.lit(l, &ty, true))
            }
            HE::Local(l) => {
                let s = self.slot_of(*l);
                if self.ref_locals.contains_key(l) {
                    let t = self.slots[s as usize].clone();
                    let r = self.val(
                        InstKind::Load {
                            slot: s,
                            path: vec![],
                        },
                        t,
                    );
                    return Some(self.val(InstKind::LoadRef { r, path: vec![] }, ty));
                }
                Some(self.val(
                    InstKind::Load {
                        slot: s,
                        path: vec![],
                    },
                    ty,
                ))
            }
            HE::MoveLocal(l) => {
                let s = self.slot_of(*l);
                Some(self.val(
                    InstKind::Move {
                        slot: s,
                        path: vec![],
                    },
                    ty,
                ))
            }
            HE::MovePath { local, path } => {
                let s = self.slot_of(*local);
                Some(self.val(
                    InstKind::Move {
                        slot: s,
                        path: path.clone(),
                    },
                    ty,
                ))
            }
            HE::FnRef(d, args) => {
                let args: Vec<Ty> = args.iter().map(|t| self.subst_ty(t)).collect();
                let f = self.l.func(*d, args);
                let f = self.l.value_wrapper(f);
                Some(self.val(
                    InstKind::Closure {
                        f,
                        captures: vec![],
                    },
                    ty,
                ))
            }
            HE::Const(d) => {
                let f = self.l.const_fn(*d);
                Some(self.val(InstKind::Call { f, args: vec![] }, ty))
            }
            HE::Call { callee, args } => {
                if let HE::FnRef(d, gargs) = &callee.kind {
                    return self.call_direct(*d, gargs, args, &ty, false);
                }
                let fv = self.expr(callee)?;
                let mut vs = Vec::new();
                for a in args {
                    vs.push(self.expr(a)?);
                }
                Some(self.val(InstKind::CallIndirect { f: fv, args: vs }, ty))
            }
            HE::Variant { variant, fields } => {
                let mut vs = Vec::new();
                for f in fields {
                    vs.push(self.expr(f)?);
                }
                let Type::Enum(id) = ty else {
                    return Some(self.unit());
                };
                let idx = self.l.variant_index(*variant);
                Some(self.val(
                    InstKind::Variant {
                        id,
                        variant: idx,
                        fields: vs,
                    },
                    ty,
                ))
            }
            HE::Struct { fields, base, .. } => {
                let Type::Struct(id) = ty.clone() else {
                    return Some(self.unit());
                };
                let n = self.l.m.structs[id as usize].fields.len();
                let mut given: Vec<Option<ValueId>> = vec![None; n];
                for (i, fe) in fields {
                    given[*i] = Some(self.expr(fe)?);
                }
                let bv = match base {
                    Some(b) => Some(self.expr(b)?),
                    None => None,
                };
                let mut vs = Vec::new();
                for (i, g) in given.into_iter().enumerate() {
                    vs.push(match g {
                        Some(v) => v,
                        None => {
                            let ft = self.l.m.structs[id as usize].fields[i].1.clone();
                            self.val(
                                InstKind::Extract {
                                    v: bv.unwrap(),
                                    idx: i as u32,
                                },
                                ft,
                            )
                        }
                    });
                }
                Some(self.val(InstKind::Struct { id, fields: vs }, ty))
            }
            HE::Field { recv, index } | HE::TupleField { recv, index } => {
                let r = self.expr(recv)?;
                Some(self.val(
                    InstKind::Extract {
                        v: r,
                        idx: *index as u32,
                    },
                    ty,
                ))
            }
            HE::Index { recv, index } => {
                let r = self.expr(recv)?;
                let i = self.expr(index)?;
                Some(self.val(InstKind::ExtractIndex { v: r, i }, ty))
            }
            HE::Unary { op, expr } => {
                let a = self.expr(expr)?;
                Some(match *op {
                    "-" => self.val(InstKind::Un { op: UnOp::Neg, a }, ty),
                    "!" => self.val(InstKind::Un { op: UnOp::Not, a }, ty),
                    "~" => self.val(
                        InstKind::Un {
                            op: UnOp::BitNot,
                            a,
                        },
                        ty,
                    ),
                    _ => a,
                })
            }
            HE::Binary { op, lhs, rhs } if op == "&&" || op == "||" => {
                let a = self.expr(lhs)?;
                let (rhs_b, _) = self.new_block(&[]);
                let (join, ps) = self.new_block(&[Type::Bool]);
                let short = self.konst(Const::Bool(op == "||"));
                if op == "&&" {
                    self.term(Term::CondBr {
                        c: a,
                        t: (rhs_b, vec![]),
                        f: (join, vec![short]),
                    });
                } else {
                    self.term(Term::CondBr {
                        c: a,
                        t: (join, vec![short]),
                        f: (rhs_b, vec![]),
                    });
                }
                self.switch_to(rhs_b);
                if let Some(b) = self.expr(rhs) {
                    self.term(Term::Br(join, vec![b]));
                }
                self.switch_to(join);
                Some(ps[0])
            }
            HE::Binary { op, lhs, rhs } => {
                let a = self.expr(lhs)?;
                let b = self.expr(rhs)?;
                let cmp = match op.as_str() {
                    "==" => Some(CmpOp::Eq),
                    "!=" => Some(CmpOp::Ne),
                    "<" => Some(CmpOp::Lt),
                    "<=" => Some(CmpOp::Le),
                    ">" => Some(CmpOp::Gt),
                    ">=" => Some(CmpOp::Ge),
                    _ => None,
                };
                Some(match cmp {
                    Some(c) => self.val(InstKind::Cmp { op: c, a, b }, Type::Bool),
                    None => self.binop(op, a, b, ty),
                })
            }
            HE::Cast(inner) => {
                let a = self.expr(inner)?;
                if self.values[a as usize] == ty {
                    return Some(a);
                }
                Some(self.val(InstKind::Cast { a, to: ty.clone() }, ty))
            }
            HE::Tuple(es) => {
                let mut vs = Vec::new();
                for x in es {
                    vs.push(self.expr(x)?);
                }
                Some(self.val(InstKind::Tuple(vs), ty))
            }
            HE::Array(es) => {
                let mut vs = Vec::new();
                for x in es {
                    vs.push(self.expr(x)?);
                }
                Some(self.val(InstKind::Array(vs), ty))
            }
            HE::Repeat { value, count } => {
                let v = self.expr(value)?;
                Some(self.val(InstKind::Repeat { v, n: *count }, ty))
            }
            HE::Block(b) | HE::Unsafe { body: b, .. } => self.block(b),
            HE::If {
                cond,
                then,
                otherwise,
            } => self.if_mode(cond, then, otherwise.as_deref(), &ty, false),
            HE::IfLet {
                pat,
                value,
                then,
                otherwise,
            } => self.if_let_mode(pat, value, then, otherwise.as_deref(), &ty, false),
            HE::Match { scrutinee, arms } => self.match_mode(scrutinee, arms, &ty, false),
            HE::Loop(body) => {
                let (head, _) = self.new_block(&[]);
                let brk_val = !matches!(e.ty, Ty::Never);
                let (exit, ep) = self.new_block(if brk_val {
                    std::slice::from_ref(&ty)
                } else {
                    &[]
                });
                self.term(Term::Br(head, vec![]));
                self.switch_to(head);
                self.loops.push(LoopFrame {
                    brk: exit,
                    cont: head,
                    brk_val,
                    defer_depth: self.defers.len(),
                });
                if self.block(body).is_some() {
                    self.term(Term::Br(head, vec![]));
                }
                self.loops.pop();
                self.switch_to(exit);
                if brk_val {
                    Some(ep[0])
                } else {
                    None
                }
            }
            HE::While { cond, body } => {
                let (head, _) = self.new_block(&[]);
                let (bodyb, _) = self.new_block(&[]);
                let (exit, _) = self.new_block(&[]);
                self.term(Term::Br(head, vec![]));
                self.switch_to(head);
                if let Some(c) = self.expr(cond) {
                    self.term(Term::CondBr {
                        c,
                        t: (bodyb, vec![]),
                        f: (exit, vec![]),
                    });
                }
                self.switch_to(bodyb);
                self.loops.push(LoopFrame {
                    brk: exit,
                    cont: head,
                    brk_val: false,
                    defer_depth: self.defers.len(),
                });
                if self.block(body).is_some() {
                    self.term(Term::Br(head, vec![]));
                }
                self.loops.pop();
                self.switch_to(exit);
                Some(self.unit())
            }
            HE::WhileLet { pat, value, body } => {
                let (head, _) = self.new_block(&[]);
                let (bodyb, _) = self.new_block(&[]);
                let (exit, _) = self.new_block(&[]);
                self.term(Term::Br(head, vec![]));
                self.switch_to(head);
                let depth = self.defers.len();
                self.push_scope();
                if let Some(v) = self.expr(value) {
                    self.bind_pat(pat, v, exit);
                    self.term(Term::Br(bodyb, vec![]));
                }
                self.switch_to(bodyb);
                self.loops.push(LoopFrame {
                    brk: exit,
                    cont: head,
                    brk_val: false,
                    defer_depth: depth,
                });
                if self.block(body).is_some() {
                    if self.end_scope() {
                        self.term(Term::Br(head, vec![]));
                    }
                } else {
                    self.drop_scope();
                }
                self.loops.pop();
                self.switch_to(exit);
                Some(self.unit())
            }
            HE::ForRange {
                pat,
                lo,
                hi,
                inclusive,
                body,
            } => {
                let it = self.sty(&lo.ty);
                let Type::Int(ity) = it.clone() else {
                    return Some(self.unit());
                };
                let lo_v = self.expr(lo)?;
                let hi_v = self.expr(hi)?;
                self.slots.push(it.clone());
                self.slot_names.push(String::new());
                let ctr = (self.slots.len() - 1) as SlotId;
                self.emit(
                    InstKind::Store {
                        slot: ctr,
                        path: vec![],
                        v: lo_v,
                    },
                    None,
                );
                let (head, _) = self.new_block(&[]);
                let (bodyb, _) = self.new_block(&[]);
                let (step, _) = self.new_block(&[]);
                let (exit, _) = self.new_block(&[]);
                self.term(Term::Br(head, vec![]));
                self.switch_to(head);
                let i = self.val(
                    InstKind::Load {
                        slot: ctr,
                        path: vec![],
                    },
                    it.clone(),
                );
                let op = if *inclusive { CmpOp::Le } else { CmpOp::Lt };
                let c = self.val(InstKind::Cmp { op, a: i, b: hi_v }, Type::Bool);
                self.term(Term::CondBr {
                    c,
                    t: (bodyb, vec![]),
                    f: (exit, vec![]),
                });
                self.switch_to(bodyb);
                let (fail, _) = self.new_block(&[]);
                // One scope per iteration owns the loop variable's bindings.
                let depth = self.defers.len();
                self.push_scope();
                self.bind_pat(pat, i, fail);
                self.loops.push(LoopFrame {
                    brk: exit,
                    cont: step,
                    brk_val: false,
                    defer_depth: depth,
                });
                if self.block(body).is_some() {
                    if self.end_scope() {
                        self.term(Term::Br(step, vec![]));
                    }
                } else {
                    self.drop_scope();
                }
                self.loops.pop();
                self.switch_to(fail);
                self.term(Term::Trap("irrefutable pattern did not match".into()));
                // Step: an inclusive range ends at `hi` without computing
                // `hi + 1` (which could overflow at the type's maximum).
                self.switch_to(step);
                let i2 = self.val(
                    InstKind::Load {
                        slot: ctr,
                        path: vec![],
                    },
                    it.clone(),
                );
                if *inclusive {
                    let (inc, _) = self.new_block(&[]);
                    let at_end = self.val(
                        InstKind::Cmp {
                            op: CmpOp::Eq,
                            a: i2,
                            b: hi_v,
                        },
                        Type::Bool,
                    );
                    self.term(Term::CondBr {
                        c: at_end,
                        t: (exit, vec![]),
                        f: (inc, vec![]),
                    });
                    self.switch_to(inc);
                }
                let one = self.konst(Const::Int(1, ity));
                let n = self.val(
                    InstKind::Bin {
                        op: BinOp::Add,
                        ov: Ov::Wrap,
                        a: i2,
                        b: one,
                    },
                    it,
                );
                self.emit(
                    InstKind::Store {
                        slot: ctr,
                        path: vec![],
                        v: n,
                    },
                    None,
                );
                self.term(Term::Br(head, vec![]));
                self.switch_to(exit);
                Some(self.unit())
            }
            HE::ForArray { pat, array, body } => {
                let at = self.sty(&array.ty);
                let Type::Array(elem, n) = at.clone() else {
                    return Some(self.unit());
                };
                let a = self.expr(array)?;
                let us = Type::Int(IntTy::Usize);
                self.slots.push(us.clone());
                self.slot_names.push(String::new());
                let ctr = (self.slots.len() - 1) as SlotId;
                let zero = self.konst(Const::Int(0, IntTy::Usize));
                self.emit(
                    InstKind::Store {
                        slot: ctr,
                        path: vec![],
                        v: zero,
                    },
                    None,
                );
                let (head, _) = self.new_block(&[]);
                let (bodyb, _) = self.new_block(&[]);
                let (step, _) = self.new_block(&[]);
                let (exit, _) = self.new_block(&[]);
                self.term(Term::Br(head, vec![]));
                self.switch_to(head);
                let i = self.val(
                    InstKind::Load {
                        slot: ctr,
                        path: vec![],
                    },
                    us.clone(),
                );
                let len = self.konst(Const::Int(n as u128, IntTy::Usize));
                let c = self.val(
                    InstKind::Cmp {
                        op: CmpOp::Lt,
                        a: i,
                        b: len,
                    },
                    Type::Bool,
                );
                self.term(Term::CondBr {
                    c,
                    t: (bodyb, vec![]),
                    f: (exit, vec![]),
                });
                self.switch_to(bodyb);
                let x = self.val(InstKind::ExtractIndex { v: a, i }, *elem);
                let (fail, _) = self.new_block(&[]);
                // One scope per iteration owns the loop variable's bindings.
                let depth = self.defers.len();
                self.push_scope();
                self.bind_pat(pat, x, fail);
                self.loops.push(LoopFrame {
                    brk: exit,
                    cont: step,
                    brk_val: false,
                    defer_depth: depth,
                });
                if self.block(body).is_some() {
                    if self.end_scope() {
                        self.term(Term::Br(step, vec![]));
                    }
                } else {
                    self.drop_scope();
                }
                self.loops.pop();
                self.switch_to(fail);
                self.term(Term::Trap("irrefutable pattern did not match".into()));
                self.switch_to(step);
                let i2 = self.val(
                    InstKind::Load {
                        slot: ctr,
                        path: vec![],
                    },
                    us.clone(),
                );
                let one = self.konst(Const::Int(1, IntTy::Usize));
                let nx = self.val(
                    InstKind::Bin {
                        op: BinOp::Add,
                        ov: Ov::Trap,
                        a: i2,
                        b: one,
                    },
                    us,
                );
                self.emit(
                    InstKind::Store {
                        slot: ctr,
                        path: vec![],
                        v: nx,
                    },
                    None,
                );
                self.term(Term::Br(head, vec![]));
                self.switch_to(exit);
                Some(self.unit())
            }
            HE::Break(v) => {
                let val = match v {
                    Some(v) => Some(self.expr(v)?),
                    None => None,
                };
                let lp = self
                    .loops
                    .last()
                    .map(|l| (l.brk, l.brk_val, l.defer_depth))?;
                self.emit_defers(lp.2);
                let args = if lp.1 {
                    vec![val.unwrap_or_else(|| self.unit())]
                } else {
                    vec![]
                };
                self.term(Term::Br(lp.0, args));
                None
            }
            HE::Continue => {
                let lp = self.loops.last().map(|l| (l.cont, l.defer_depth))?;
                self.emit_defers(lp.1);
                self.term(Term::Br(lp.0, vec![]));
                None
            }
            HE::Return(v) => {
                let val = match v {
                    Some(v) if self.ret_ref => self.expr_ref(v)?,
                    Some(v) => self.expr(v)?,
                    None => self.unit(),
                };
                self.emit_return(val);
                None
            }
            HE::Try { expr, is_result } => {
                let v = self.expr(expr)?;
                let et = self.subst_ty(&expr.ty);
                let Ty::Adt(ed, _) = &et else {
                    return Some(self.unit());
                };
                let ok_idx = self
                    .l
                    .named_variant(*ed, if *is_result { "Ok" } else { "Some" });
                let tag = self.val(InstKind::Tag { v }, Type::Int(IntTy::U32));
                let want = self.konst(Const::Int(ok_idx as u128, IntTy::U32));
                let is_ok = self.val(
                    InstKind::Cmp {
                        op: CmpOp::Eq,
                        a: tag,
                        b: want,
                    },
                    Type::Bool,
                );
                let (okb, _) = self.new_block(&[]);
                let (errb, _) = self.new_block(&[]);
                self.term(Term::CondBr {
                    c: is_ok,
                    t: (okb, vec![]),
                    f: (errb, vec![]),
                });
                // Early return of None / Err(e) in the function's return type.
                self.switch_to(errb);
                let ret = self.ret.clone();
                if let Type::Enum(rid) = ret.clone() {
                    let rv = if *is_result {
                        let err_idx = self.l.named_variant(*ed, "Err");
                        let et2 = match &self.l.m.enums[rid as usize].variants[err_idx as usize].1[..]
                        {
                            [t] => t.clone(),
                            _ => Type::unit(),
                        };
                        let payload = self.val(
                            InstKind::VariantField {
                                v,
                                variant: err_idx,
                                idx: 0,
                            },
                            et2,
                        );
                        self.val(
                            InstKind::Variant {
                                id: rid,
                                variant: err_idx,
                                fields: vec![payload],
                            },
                            ret,
                        )
                    } else {
                        let none_idx = self.l.named_variant(*ed, "None");
                        self.val(
                            InstKind::Variant {
                                id: rid,
                                variant: none_idx,
                                fields: vec![],
                            },
                            ret,
                        )
                    };
                    self.emit_return(rv);
                } else {
                    self.term(Term::Trap(
                        "`?` in a function that does not return Option/Result".into(),
                    ));
                }
                self.switch_to(okb);
                Some(self.val(
                    InstKind::VariantField {
                        v,
                        variant: ok_idx,
                        idx: 0,
                    },
                    ty,
                ))
            }
            HE::Closure {
                params,
                body,
                captures,
            } => self.closure(params, body, captures, ty, e.span),
            HE::Error => {
                self.l.err(
                    "L001",
                    self.file,
                    e.span,
                    "erroneous expression reached lowering".into(),
                );
                Some(self.unit())
            }
        }
    }

    /// Lower two branches that rejoin; the result has type `ty`.
    fn branches(
        &mut self,
        ty: &Type,
        tb: BlockId,
        eb: BlockId,
        then: impl FnOnce(&mut Self) -> Option<ValueId>,
        other: impl FnOnce(&mut Self) -> Option<ValueId>,
    ) -> Option<ValueId> {
        let (join, jp) = self.new_block(std::slice::from_ref(ty));
        let mut reached = false;
        self.switch_to(tb);
        if let Some(v) = then(self) {
            let v = self.fit(v, ty);
            self.term(Term::Br(join, vec![v]));
            reached = true;
        }
        self.switch_to(eb);
        if let Some(v) = other(self) {
            let v = self.fit(v, ty);
            self.term(Term::Br(join, vec![v]));
            reached = true;
        }
        self.switch_to(join);
        reached.then_some(jp[0])
    }

    /// A branch whose value is discarded (`if` without else) yields unit.
    fn fit(&mut self, v: ValueId, ty: &Type) -> ValueId {
        if self.values[v as usize] == *ty {
            v
        } else {
            self.unit()
        }
    }

    fn closure(
        &mut self,
        params: &[hir::Pat],
        body: &hir::Expr,
        captures: &[LocalId],
        ty: Type,
        span: Span,
    ) -> Option<ValueId> {
        let Type::Fn(ptys, ret) = ty.clone() else {
            return Some(self.unit());
        };
        let cap_vals: Vec<ValueId> = captures
            .iter()
            .map(|&l| {
                let s = self.slot_of(l);
                if self.ref_locals.contains_key(&l) {
                    // A borrowed parameter is captured as a copy of its value:
                    // a borrow cannot be stored in a closure (second-class).
                    let rt = self.slots[s as usize].clone();
                    let r = self.val(
                        InstKind::Load {
                            slot: s,
                            path: vec![],
                        },
                        rt,
                    );
                    let ht = self.hlocals[l as usize].ty.clone();
                    let vt = self.sty(&ht);
                    return self.val(InstKind::LoadRef { r, path: vec![] }, vt);
                }
                let t = self.slots[s as usize].clone();
                // A non-Copy capture moves into the closure (PURM checked it).
                let ht = self.hlocals[l as usize].ty.clone();
                if self.needs_drop(&ht) {
                    self.val(
                        InstKind::Move {
                            slot: s,
                            path: vec![],
                        },
                        t,
                    )
                } else {
                    self.val(
                        InstKind::Load {
                            slot: s,
                            path: vec![],
                        },
                        t,
                    )
                }
            })
            .collect();
        let cap_tys: Vec<Type> = cap_vals
            .iter()
            .map(|v| self.values[*v as usize].clone())
            .collect();
        let mut all = cap_tys.clone();
        all.extend(ptys.iter().cloned());
        let fid = self.l.m.funcs.len() as FuncId;
        let name = format!("closure@{}", span.0);
        self.l.m.funcs.push(Func {
            name,
            kind: FuncKind::Defined,
            params: all.clone(),
            ret: (*ret).clone(),
            slots: vec![],
            slot_names: vec![],
            values: vec![],
            blocks: vec![],
        });
        let fake = hir::Body {
            def: 0,
            locals: self.hlocals.clone(),
            params: vec![],
            requires: vec![],
            ensures: vec![],
            result_local: None,
            value: body.clone(),
        };
        let subst = self.subst.clone();
        let file = self.file;
        let lifted = {
            let mut fx = Fx::new(self.l, file, subst, &fake, all, (*ret).clone());
            fx.captured = captures.to_vec();
            let eps = fx.entry_params.clone();
            for (k, &l) in captures.iter().enumerate() {
                let s = fx.slot_of(l);
                fx.emit(
                    InstKind::Store {
                        slot: s,
                        path: vec![],
                        v: eps[k],
                    },
                    None,
                );
            }
            let (fail, _) = fx.new_block(&[]);
            for (k, p) in params.iter().enumerate() {
                fx.bind_pat(p, eps[captures.len() + k], fail);
            }
            let cur = fx.cur;
            fx.switch_to(fail);
            fx.term(Term::Trap("irrefutable pattern did not match".into()));
            fx.switch_to(cur);
            if let Some(v) = fx.expr(body) {
                fx.term(Term::Ret(v));
            }
            fx.finish()
        };
        let f = &mut self.l.m.funcs[fid as usize];
        f.slots = lifted.slots;
        f.slot_names = lifted.slot_names;
        f.values = lifted.values;
        f.blocks = lifted.blocks;
        Some(self.val(
            InstKind::Closure {
                f: fid,
                captures: cap_vals,
            },
            ty,
        ))
    }

    // ------------------------------------------------------------ patterns

    /// Test `v` against `p`, binding variables; on mismatch branch to
    /// `fail`. Control continues in the current block on success.
    fn bind_pat(&mut self, p: &hir::Pat, v: ValueId, fail: BlockId) {
        let ty = self.sty(&p.ty);
        match &p.kind {
            PatKind::Wild => {}
            PatKind::Binding(l) => {
                let s = self.slot_of(*l);
                self.emit(
                    InstKind::Store {
                        slot: s,
                        path: vec![],
                        v,
                    },
                    None,
                );
                self.own_local(*l);
            }
            PatKind::Lit { lit, neg } => {
                let k = self.lit(lit, &ty, *neg);
                let c = self.val(
                    InstKind::Cmp {
                        op: CmpOp::Eq,
                        a: v,
                        b: k,
                    },
                    Type::Bool,
                );
                self.guard(c, fail);
            }
            PatKind::Range { lo, hi, inclusive } => {
                if let Some((l, n)) = lo {
                    let k = self.lit(l, &ty, *n);
                    let c = self.val(
                        InstKind::Cmp {
                            op: CmpOp::Ge,
                            a: v,
                            b: k,
                        },
                        Type::Bool,
                    );
                    self.guard(c, fail);
                }
                if let Some((h, n)) = hi {
                    let k = self.lit(h, &ty, *n);
                    let op = if *inclusive { CmpOp::Le } else { CmpOp::Lt };
                    let c = self.val(InstKind::Cmp { op, a: v, b: k }, Type::Bool);
                    self.guard(c, fail);
                }
            }
            PatKind::Const(d) => {
                let f = self.l.const_fn(*d);
                let k = self.val(InstKind::Call { f, args: vec![] }, ty);
                let c = self.val(
                    InstKind::Cmp {
                        op: CmpOp::Eq,
                        a: v,
                        b: k,
                    },
                    Type::Bool,
                );
                self.guard(c, fail);
            }
            PatKind::Tuple(ps) | PatKind::Struct { fields: ps, .. } => {
                for (i, sp) in ps.iter().enumerate() {
                    if matches!(sp.kind, PatKind::Wild) {
                        continue;
                    }
                    let ft = self.sty(&sp.ty);
                    let x = self.val(InstKind::Extract { v, idx: i as u32 }, ft);
                    self.bind_pat(sp, x, fail);
                }
            }
            PatKind::Variant { variant, fields } => {
                let idx = self.l.variant_index(*variant);
                let tag = self.val(InstKind::Tag { v }, Type::Int(IntTy::U32));
                let k = self.konst(Const::Int(idx as u128, IntTy::U32));
                let c = self.val(
                    InstKind::Cmp {
                        op: CmpOp::Eq,
                        a: tag,
                        b: k,
                    },
                    Type::Bool,
                );
                self.guard(c, fail);
                for (i, sp) in fields.iter().enumerate() {
                    if matches!(sp.kind, PatKind::Wild) {
                        continue;
                    }
                    let ft = self.sty(&sp.ty);
                    let x = self.val(
                        InstKind::VariantField {
                            v,
                            variant: idx,
                            idx: i as u32,
                        },
                        ft,
                    );
                    self.bind_pat(sp, x, fail);
                }
            }
            PatKind::Array { before, after, .. } => {
                let Type::Array(elem, n) = ty else { return };
                let at = |fx: &mut Self, i: u64| {
                    let k = fx.konst(Const::Int(i as u128, IntTy::Usize));
                    fx.val(InstKind::ExtractIndex { v, i: k }, (*elem).clone())
                };
                for (i, sp) in before.iter().enumerate() {
                    if !matches!(sp.kind, PatKind::Wild) {
                        let x = at(self, i as u64);
                        self.bind_pat(sp, x, fail);
                    }
                }
                for (j, sp) in after.iter().enumerate() {
                    if !matches!(sp.kind, PatKind::Wild) {
                        let x = at(self, n - after.len() as u64 + j as u64);
                        self.bind_pat(sp, x, fail);
                    }
                }
            }
            PatKind::Or(alts) => {
                let (ok, _) = self.new_block(&[]);
                for (i, alt) in alts.iter().enumerate() {
                    let last = i + 1 == alts.len();
                    let (next, _) = if last {
                        (fail, vec![])
                    } else {
                        self.new_block(&[])
                    };
                    self.bind_pat(alt, v, next);
                    self.term(Term::Br(ok, vec![]));
                    if !last {
                        self.switch_to(next);
                    }
                }
                self.switch_to(ok);
            }
        }
    }

    fn guard(&mut self, c: ValueId, fail: BlockId) {
        let (ok, _) = self.new_block(&[]);
        self.term(Term::CondBr {
            c,
            t: (ok, vec![]),
            f: (fail, vec![]),
        });
        self.switch_to(ok);
    }
}
