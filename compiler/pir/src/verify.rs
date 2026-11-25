//! The PIR verifier (spec/pir.md §4). Run after lowering (and after every
//! future pass); a failure is a compiler bug, never a user error. The
//! verifier never panics: malformed identifiers are reported, not indexed.

use crate::ir::*;
use pegasus_sema::ty::{FloatTy, IntTy};

pub fn verify(m: &Module) -> Result<(), Vec<String>> {
    let mut errs = Vec::new();
    for (i, s) in m.structs.iter().enumerate() {
        for (n, t) in &s.fields {
            if !inner_ok(m, t) {
                errs.push(format!("struct #{i} field `{n}`: invalid type"));
            }
        }
    }
    for (i, e) in m.enums.iter().enumerate() {
        for (n, ts) in &e.variants {
            if ts.iter().any(|t| !inner_ok(m, t)) {
                errs.push(format!("enum #{i} variant `{n}`: invalid type"));
            }
        }
    }
    for (i, f) in m.funcs.iter().enumerate() {
        if f.params
            .iter()
            .chain(std::iter::once(&f.ret))
            .any(|t| !type_ok(m, t))
        {
            errs.push(format!(
                "@{} #{i}: invalid parameter or return type",
                f.name
            ));
            continue;
        }
        if f.kind != FuncKind::Defined
            && f.params
                .iter()
                .chain(std::iter::once(&f.ret))
                .any(|t| matches!(t, Type::Ref { .. }))
        {
            errs.push(format!(
                "@{} #{i}: an extern function cannot take or return a reference",
                f.name
            ));
            continue;
        }
        if f.kind == FuncKind::Defined {
            verify_func(m, i as FuncId, f, &mut errs);
        }
    }
    if errs.is_empty() {
        Ok(())
    } else {
        Err(errs)
    }
}

/// A type that may appear at the top level of a value, slot, parameter or
/// return: a reference is allowed only there (second-class borrows).
fn type_ok(m: &Module, t: &Type) -> bool {
    match t {
        Type::Ref { ty, .. } => inner_ok(m, ty),
        _ => inner_ok(m, t),
    }
}

/// Every struct and enum identifier inside `t` exists and no reference is
/// nested inside it.
fn inner_ok(m: &Module, t: &Type) -> bool {
    match t {
        Type::Ref { .. } => false,
        Type::Struct(s) => (*s as usize) < m.structs.len(),
        Type::Enum(e) => (*e as usize) < m.enums.len(),
        Type::Tuple(v) => v.iter().all(|x| inner_ok(m, x)),
        Type::Array(e, _) | Type::Ptr(_, e) => inner_ok(m, e),
        Type::Fn(p, r) => p.iter().all(|x| inner_ok(m, x)) && inner_ok(m, r),
        _ => true,
    }
}

fn mask(t: IntTy) -> u128 {
    if t.bits() == 128 {
        u128::MAX
    } else {
        (1u128 << t.bits()) - 1
    }
}

/// Lossless conversions (spec/semantics.md §4.4), the only casts PIR has.
pub fn lossless(from: &Type, to: &Type) -> bool {
    use Type::*;
    match (from, to) {
        (Int(a), Int(b)) => {
            a != b
                && if a.signed() == b.signed() {
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
            (if a.signed() { a.bits() - 1 } else { a.bits() }) <= mant
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

/// Legal operand types and overflow modes of a binary operation; returns the
/// result type or an explanation.
pub fn bin_type(op: BinOp, ov: Ov, a: &Type, b: &Type) -> Result<Type, String> {
    use BinOp::*;
    let int_mode_ok = match op {
        Add | Sub | Mul => true,
        _ => ov == Ov::Trap,
    };
    match a {
        Type::Int(_) => {
            if !int_mode_ok {
                return Err(format!("{op:?} has no {ov:?} mode"));
            }
            match op {
                Shl | Shr | Pow => match b {
                    Type::Int(bt) if !bt.signed() => Ok(a.clone()),
                    _ => Err(format!("{op:?} needs an unsigned integer right operand")),
                },
                _ if a == b => Ok(a.clone()),
                _ => Err("operands of different types".into()),
            }
        }
        Type::Float(_) => {
            if ov != Ov::Trap {
                return Err("floating-point operations have no overflow mode".into());
            }
            match op {
                Add | Sub | Mul | Div | Rem if a == b => Ok(a.clone()),
                Pow if a == b || matches!(b, Type::Int(_)) => Ok(a.clone()),
                Add | Sub | Mul | Div | Rem | Pow => Err("operands of different types".into()),
                _ => Err(format!("{op:?} is not defined on floats")),
            }
        }
        Type::Bool => match op {
            And | Or | Xor if b == a && ov == Ov::Trap => Ok(Type::Bool),
            _ => Err(format!("{op:?} is not defined on bool")),
        },
        _ => Err(format!("{op:?} is not defined on this type")),
    }
}

fn succs(t: &Term) -> Vec<BlockId> {
    match t {
        Term::Br(b, _) => vec![*b],
        Term::CondBr { t, f, .. } => vec![t.0, f.0],
        Term::Switch { cases, default, .. } => cases
            .iter()
            .map(|c| c.1)
            .chain(std::iter::once(*default))
            .collect(),
        _ => vec![],
    }
}

/// Immediate dominators (Cooper, Harvey, Kennedy); None = unreachable.
/// Out-of-range successors are ignored here (reported elsewhere).
fn dominators(f: &Func) -> Vec<Option<BlockId>> {
    let n = f.blocks.len();
    let valid = |s: BlockId| (s as usize) < n;
    let mut order = Vec::new();
    let mut seen = vec![false; n];
    let mut stack = vec![(0u32, 0usize)];
    seen[0] = true;
    while let Some((b, i)) = stack.pop() {
        let ss: Vec<BlockId> = succs(&f.blocks[b as usize].term)
            .into_iter()
            .filter(|s| valid(*s))
            .collect();
        if i < ss.len() {
            stack.push((b, i + 1));
            let s = ss[i];
            if !seen[s as usize] {
                seen[s as usize] = true;
                stack.push((s, 0));
            }
        } else {
            order.push(b);
        }
    }
    let mut rpo_num = vec![usize::MAX; n];
    for (k, &b) in order.iter().rev().enumerate() {
        rpo_num[b as usize] = k;
    }
    let mut preds = vec![Vec::new(); n];
    for (b, blk) in f.blocks.iter().enumerate() {
        if seen[b] {
            for s in succs(&blk.term).into_iter().filter(|s| valid(*s)) {
                preds[s as usize].push(b as BlockId);
            }
        }
    }
    let mut idom: Vec<Option<BlockId>> = vec![None; n];
    idom[0] = Some(0);
    let rpo: Vec<BlockId> = order.iter().rev().copied().collect();
    let mut changed = true;
    while changed {
        changed = false;
        for &b in rpo.iter().skip(1) {
            let mut new: Option<BlockId> = None;
            for &p in &preds[b as usize] {
                if idom[p as usize].is_none() {
                    continue;
                }
                new = Some(match new {
                    None => p,
                    Some(mut a) => {
                        let mut c = p;
                        while a != c {
                            while rpo_num[a as usize] > rpo_num[c as usize] {
                                a = idom[a as usize].unwrap();
                            }
                            while rpo_num[c as usize] > rpo_num[a as usize] {
                                c = idom[c as usize].unwrap();
                            }
                        }
                        a
                    }
                });
            }
            if new.is_some() && idom[b as usize] != new {
                idom[b as usize] = new;
                changed = true;
            }
        }
    }
    idom
}

fn dominates(idom: &[Option<BlockId>], a: BlockId, mut b: BlockId) -> bool {
    loop {
        if a == b {
            return true;
        }
        match idom[b as usize] {
            Some(p) if p != b => b = p,
            _ => return false,
        }
    }
}

fn path_ty(m: &Module, values: &[Type], base: &Type, path: &[Proj]) -> Option<Type> {
    let mut t = base.clone();
    for p in path {
        t = match (p, &t) {
            (Proj::Field(i), Type::Tuple(v)) => v.get(*i as usize)?.clone(),
            (Proj::Field(i), Type::Struct(s)) => m
                .structs
                .get(*s as usize)?
                .fields
                .get(*i as usize)?
                .1
                .clone(),
            (Proj::Index(v), Type::Array(e, _)) => {
                if values.get(*v as usize)? != &Type::Int(IntTy::Usize) {
                    return None;
                }
                (**e).clone()
            }
            _ => return None,
        };
    }
    Some(t)
}

fn idx_uses(path: &[Proj]) -> Vec<ValueId> {
    path.iter()
        .filter_map(|p| {
            if let Proj::Index(v) = p {
                Some(*v)
            } else {
                None
            }
        })
        .collect()
}

fn uses(k: &InstKind) -> Vec<ValueId> {
    match k {
        InstKind::Const(_) | InstKind::Move { .. } | InstKind::Drop { .. } => vec![],
        InstKind::Bin { a, b, .. } | InstKind::Cmp { a, b, .. } => vec![*a, *b],
        InstKind::Un { a, .. } | InstKind::Cast { a, .. } => vec![*a],
        InstKind::Load { path, .. } => path
            .iter()
            .filter_map(|p| {
                if let Proj::Index(v) = p {
                    Some(*v)
                } else {
                    None
                }
            })
            .collect(),
        InstKind::Store { path, v, .. } => path
            .iter()
            .filter_map(|p| {
                if let Proj::Index(v) = p {
                    Some(*v)
                } else {
                    None
                }
            })
            .chain(std::iter::once(*v))
            .collect(),
        InstKind::Tuple(v) | InstKind::Array(v) => v.clone(),
        InstKind::Struct { fields, .. } | InstKind::Variant { fields, .. } => fields.clone(),
        InstKind::Repeat { v, .. }
        | InstKind::Extract { v, .. }
        | InstKind::Tag { v }
        | InstKind::VariantField { v, .. } => vec![*v],
        InstKind::ExtractIndex { v, i } => vec![*v, *i],
        InstKind::Borrow { path, .. } => idx_uses(path),
        InstKind::BorrowRef { r, path, .. } | InstKind::LoadRef { r, path } => {
            std::iter::once(*r).chain(idx_uses(path)).collect()
        }
        InstKind::StoreRef { r, path, v } => std::iter::once(*r)
            .chain(idx_uses(path))
            .chain(std::iter::once(*v))
            .collect(),
        InstKind::Call { args, .. } => args.clone(),
        InstKind::CallIndirect { f, args } => {
            std::iter::once(*f).chain(args.iter().copied()).collect()
        }
        InstKind::Closure { captures, .. } => captures.clone(),
    }
}

/// The result type an instruction must have (None: it has no result).
fn inst_type(m: &Module, f: &Func, k: &InstKind) -> Result<Option<Type>, String> {
    let ty = |v: ValueId| {
        f.values
            .get(v as usize)
            .cloned()
            .ok_or_else(|| format!("value %{v} out of range"))
    };
    let slot = |s: SlotId| {
        f.slots
            .get(s as usize)
            .cloned()
            .ok_or_else(|| format!("slot ${s} out of range"))
    };
    Ok(Some(match k {
        InstKind::Const(c) => match c {
            Const::Int(v, t) => {
                if v & !mask(*t) != 0 {
                    return Err(format!(
                        "integer constant {v} has bits outside `{}`",
                        t.name()
                    ));
                }
                Type::Int(*t)
            }
            Const::Float(_, t) => Type::Float(*t),
            Const::Bool(_) => Type::Bool,
            Const::Char(_) => Type::Char,
            Const::Str(_) => Type::Str,
        },
        InstKind::Bin { op, ov, a, b } => bin_type(*op, *ov, &ty(*a)?, &ty(*b)?)?,
        InstKind::Un { op, a } => {
            let t = ty(*a)?;
            let ok = match op {
                UnOp::Neg => matches!(t, Type::Int(i) if i.signed()) || matches!(t, Type::Float(_)),
                UnOp::Not => t == Type::Bool,
                UnOp::BitNot => matches!(t, Type::Int(_)),
            };
            if !ok {
                return Err(format!("{op:?} is not defined on this operand type"));
            }
            t
        }
        InstKind::Cmp { op, a, b } => {
            let (ta, tb) = (ty(*a)?, ty(*b)?);
            if ta != tb {
                return Err("comparison operands of different types".into());
            }
            // spec/semantics.md §6: ordering is defined on numbers, `char`
            // and `str` only; `bool` has `==` and `!=` but no ordering.
            // References are compared through loads, never as addresses.
            let ordered = matches!(ta, Type::Int(_) | Type::Float(_) | Type::Char | Type::Str);
            let eq = !matches!(ta, Type::Fn(..) | Type::Region | Type::Ref { .. });
            if !(eq && (matches!(op, CmpOp::Eq | CmpOp::Ne) || ordered)) {
                return Err(format!("{op:?} is not defined on this operand type"));
            }
            Type::Bool
        }
        InstKind::Cast { a, to } => {
            let from = ty(*a)?;
            if !lossless(&from, to) {
                return Err("cast is not a lossless conversion".into());
            }
            to.clone()
        }
        InstKind::Load { slot: s, path } => {
            path_ty(m, &f.values, &slot(*s)?, path).ok_or("invalid load path")?
        }
        InstKind::Store { slot: s, path, v } => {
            let pt = path_ty(m, &f.values, &slot(*s)?, path).ok_or("invalid store path")?;
            if pt != ty(*v)? {
                return Err(format!("store of %{v} does not match the place type"));
            }
            return Ok(None);
        }
        InstKind::Move { slot: s, path } => {
            if !path_ok_root(f, *s) {
                return Err(format!("slot ${s} out of range"));
            }
            if matches!(slot(*s)?, Type::Ref { .. }) {
                return Err("move out of a reference parameter slot".into());
            }
            let proj: Vec<Proj> = path.iter().map(|i| Proj::Field(*i)).collect();
            path_ty(m, &f.values, &slot(*s)?, &proj).ok_or("invalid move path")?
        }
        InstKind::Drop { slot: s } => {
            if matches!(slot(*s)?, Type::Ref { .. }) {
                return Err("drop of a reference parameter slot".into());
            }
            return Ok(None);
        }
        InstKind::Borrow {
            slot: s,
            path,
            mutable,
        } => {
            let st = slot(*s)?;
            if matches!(st, Type::Ref { .. }) {
                return Err("borrow of a slot that holds a reference (reborrow through the reference instead)".into());
            }
            let t = path_ty(m, &f.values, &st, path).ok_or("invalid borrow path")?;
            Type::Ref {
                mutable: *mutable,
                ty: Box::new(t),
            }
        }
        InstKind::BorrowRef { r, path, mutable } => match ty(*r)? {
            Type::Ref {
                mutable: rm,
                ty: inner,
            } => {
                if *mutable && !rm {
                    return Err("exclusive borrow through a shared reference".into());
                }
                let t = path_ty(m, &f.values, &inner, path).ok_or("invalid borrow path")?;
                Type::Ref {
                    mutable: *mutable,
                    ty: Box::new(t),
                }
            }
            _ => return Err("reborrow of a non-reference value".into()),
        },
        InstKind::LoadRef { r, path } => match ty(*r)? {
            Type::Ref { ty: inner, .. } => {
                path_ty(m, &f.values, &inner, path).ok_or("invalid load path")?
            }
            _ => return Err("load through a non-reference value".into()),
        },
        InstKind::StoreRef { r, path, v } => match ty(*r)? {
            Type::Ref {
                mutable: true,
                ty: inner,
            } => {
                let pt = path_ty(m, &f.values, &inner, path).ok_or("invalid store path")?;
                if pt != ty(*v)? {
                    return Err(format!("store of %{v} does not match the place type"));
                }
                return Ok(None);
            }
            Type::Ref { mutable: false, .. } => {
                return Err("store through a shared reference".into())
            }
            _ => return Err("store through a non-reference value".into()),
        },
        InstKind::Tuple(v) => Type::Tuple(v.iter().map(|x| ty(*x)).collect::<Result<_, _>>()?),
        InstKind::Array(v) => {
            let ts: Vec<Type> = v.iter().map(|x| ty(*x)).collect::<Result<_, _>>()?;
            let Some(first) = ts.first().cloned() else {
                // An empty array literal: its element type comes from the result.
                return Err("empty-array".into());
            };
            if ts.iter().any(|t| *t != first) {
                return Err("array elements of different types".into());
            }
            Type::Array(Box::new(first), ts.len() as u64)
        }
        InstKind::Repeat { v, n } => Type::Array(Box::new(ty(*v)?), *n),
        InstKind::Struct { id, fields } => {
            let sd = m
                .structs
                .get(*id as usize)
                .ok_or_else(|| format!("struct #{id} out of range"))?;
            if sd.fields.len() != fields.len() {
                return Err(format!(
                    "struct %{} takes {} fields, given {}",
                    sd.name,
                    sd.fields.len(),
                    fields.len()
                ));
            }
            for ((_, t), v) in sd.fields.iter().zip(fields) {
                if *t != ty(*v)? {
                    return Err(format!("struct %{} field of the wrong type", sd.name));
                }
            }
            Type::Struct(*id)
        }
        InstKind::Variant {
            id,
            variant,
            fields,
        } => {
            let ed = m
                .enums
                .get(*id as usize)
                .ok_or_else(|| format!("enum #{id} out of range"))?;
            let (_, ts) = ed
                .variants
                .get(*variant as usize)
                .ok_or_else(|| format!("enum %{} has no variant {variant}", ed.name))?;
            if ts.len() != fields.len() {
                return Err(format!(
                    "variant payload of %{} has the wrong arity",
                    ed.name
                ));
            }
            for (t, v) in ts.iter().zip(fields) {
                if *t != ty(*v)? {
                    return Err(format!(
                        "variant payload of %{} has the wrong type",
                        ed.name
                    ));
                }
            }
            Type::Enum(*id)
        }
        InstKind::Extract { v, idx } => {
            path_ty(m, &f.values, &ty(*v)?, &[Proj::Field(*idx)]).ok_or("invalid extract")?
        }
        InstKind::ExtractIndex { v, i } => {
            if ty(*i)? != Type::Int(IntTy::Usize) {
                return Err("array index must be usize".into());
            }
            match ty(*v)? {
                Type::Array(e, _) => *e,
                _ => return Err("index into a non-array".into()),
            }
        }
        InstKind::Tag { v } => match ty(*v)? {
            Type::Enum(_) => Type::Int(IntTy::U32),
            _ => return Err("tag of a non-enum".into()),
        },
        InstKind::VariantField { v, variant, idx } => match ty(*v)? {
            Type::Enum(id) => {
                let ed = m.enums.get(id as usize).ok_or("enum out of range")?;
                let (_, ts) = ed
                    .variants
                    .get(*variant as usize)
                    .ok_or("variant out of range")?;
                ts.get(*idx as usize)
                    .cloned()
                    .ok_or("payload field out of range")?
            }
            _ => return Err("payload of a non-enum".into()),
        },
        InstKind::Call { f: callee, args } => {
            let cf = m
                .funcs
                .get(*callee as usize)
                .ok_or_else(|| format!("call to unknown function #{callee}"))?;
            let ats: Vec<Type> = args.iter().map(|a| ty(*a)).collect::<Result<_, _>>()?;
            if ats != cf.params {
                return Err(format!(
                    "call to @{}: argument types differ from its parameters",
                    cf.name
                ));
            }
            cf.ret.clone()
        }
        InstKind::CallIndirect { f: fv, args } => match ty(*fv)? {
            Type::Fn(ps, r) => {
                let ats: Vec<Type> = args.iter().map(|a| ty(*a)).collect::<Result<_, _>>()?;
                if ats != ps {
                    return Err("indirect call argument types differ".into());
                }
                *r
            }
            _ => return Err("indirect call of a non-function value".into()),
        },
        InstKind::Closure {
            f: callee,
            captures,
        } => {
            let cf = m
                .funcs
                .get(*callee as usize)
                .ok_or_else(|| format!("closure of unknown function #{callee}"))?;
            let cts: Vec<Type> = captures.iter().map(|c| ty(*c)).collect::<Result<_, _>>()?;
            if cf.params.len() < cts.len() || cf.params[..cts.len()] != cts[..] {
                return Err(format!(
                    "closure captures do not match @{}'s leading parameters",
                    cf.name
                ));
            }
            Type::Fn(cf.params[cts.len()..].to_vec(), Box::new(cf.ret.clone()))
        }
    }))
}

fn path_ok_root(f: &Func, s: SlotId) -> bool {
    (s as usize) < f.slots.len()
}

fn verify_func(m: &Module, fid: FuncId, f: &Func, errs: &mut Vec<String>) {
    let name = format!("@{} #{fid}", f.name);
    let mut e = |s: String| errs.push(format!("{name}: {s}"));
    if f.blocks.is_empty() {
        e("defined function has no blocks".into());
        return;
    }
    if !f.slot_names.is_empty() && f.slot_names.len() != f.slots.len() {
        e("slot names do not match the slots".into());
    }
    for (k, t) in f.slots.iter().enumerate() {
        if !type_ok(m, t) {
            e(format!("slot ${k}: invalid type"));
        }
    }
    for (k, t) in f.values.iter().enumerate() {
        if !type_ok(m, t) {
            e(format!("value %{k}: invalid type"));
        }
    }
    ref_slots(f, &mut e);
    let nb = f.blocks.len();
    let entry_tys: Vec<Option<&Type>> = f.blocks[0]
        .params
        .iter()
        .map(|p| f.values.get(*p as usize))
        .collect();
    if entry_tys != f.params.iter().map(Some).collect::<Vec<_>>() {
        e("entry block parameters differ from the function parameters".into());
    }
    // Every declared value is defined exactly once.
    let nv = f.values.len();
    let mut defs = vec![0u32; nv];
    let mut def_at: Vec<Option<(BlockId, usize)>> = vec![None; nv];
    for (b, blk) in f.blocks.iter().enumerate() {
        let results = blk
            .insts
            .iter()
            .enumerate()
            .filter_map(|(k, i)| i.result.map(|r| (r, k + 1)));
        for (v, pos) in blk.params.iter().map(|p| (*p, 0)).chain(results) {
            if (v as usize) >= nv {
                e(format!("bb{b}: value %{v} is not declared"));
                continue;
            }
            defs[v as usize] += 1;
            def_at[v as usize] = Some((b as BlockId, pos));
        }
    }
    for (v, &n) in defs.iter().enumerate() {
        if n != 1 {
            e(format!(
                "value %{v} is defined {n} times (must be exactly once)"
            ));
        }
    }
    let idom = dominators(f);
    for (b, blk) in f.blocks.iter().enumerate() {
        let b = b as BlockId;
        let reachable = idom[b as usize].is_some();
        let check_use = |v: ValueId, pos: usize, e: &mut dyn FnMut(String)| match def_at
            .get(v as usize)
            .copied()
            .flatten()
        {
            None => e(format!("bb{b}: use of undefined value %{v}")),
            Some((db, dpos)) => {
                if reachable && !((db == b && dpos <= pos) || (db != b && dominates(&idom, db, b)))
                {
                    e(format!(
                        "bb{b}: use of %{v} is not dominated by its definition"
                    ));
                }
            }
        };
        for (k, inst) in blk.insts.iter().enumerate() {
            for u in uses(&inst.kind) {
                check_use(u, k, &mut e);
            }
            match inst_type(m, f, &inst.kind) {
                Err(msg) if msg == "empty-array" => {
                    // `[]`: the declared result must be an array of length 0.
                    let rt = inst.result.and_then(|r| f.values.get(r as usize));
                    if !matches!(rt, Some(Type::Array(_, 0))) {
                        e(format!(
                            "bb{b}: empty array literal must have a zero-length array type"
                        ));
                    }
                }
                Err(msg) => e(format!("bb{b}: {msg}")),
                Ok(want) => {
                    let got = inst.result.map(|r| f.values.get(r as usize).cloned());
                    match (want, got) {
                        (None, None) => {}
                        (None, Some(_)) => e(format!("bb{b}: this instruction has no result")),
                        (Some(_), None) => e(format!("bb{b}: instruction result is missing")),
                        (Some(w), Some(Some(g))) if w == g => {}
                        (Some(_), Some(_)) => {
                            e(format!("bb{b}: result type does not match the instruction"))
                        }
                    }
                }
            }
        }
        let tpos = blk.insts.len();
        let check_edge = |target: BlockId, args: &[ValueId], e: &mut dyn FnMut(String)| match f
            .blocks
            .get(target as usize)
        {
            None => e(format!("bb{b}: branch to missing bb{target}")),
            Some(tb) => {
                let ats: Vec<Option<&Type>> =
                    args.iter().map(|a| f.values.get(*a as usize)).collect();
                let pts: Vec<Option<&Type>> = tb
                    .params
                    .iter()
                    .map(|p| f.values.get(*p as usize))
                    .collect();
                if ats != pts {
                    e(format!(
                        "bb{b}: arguments to bb{target} do not match its parameters"
                    ));
                }
            }
        };
        let ty = |v: ValueId| f.values.get(v as usize).cloned();
        match &blk.term {
            Term::Br(t, a) => {
                a.iter().for_each(|v| check_use(*v, tpos, &mut e));
                check_edge(*t, a, &mut e);
            }
            Term::CondBr { c, t, f: fb } => {
                check_use(*c, tpos, &mut e);
                if ty(*c) != Some(Type::Bool) {
                    e(format!("bb{b}: cond_br on a non-bool value"));
                }
                t.1.iter()
                    .chain(&fb.1)
                    .for_each(|v| check_use(*v, tpos, &mut e));
                check_edge(t.0, &t.1, &mut e);
                check_edge(fb.0, &fb.1, &mut e);
            }
            Term::Switch { v, cases, default } => {
                check_use(*v, tpos, &mut e);
                match ty(*v) {
                    Some(Type::Int(it)) => {
                        let mut keys: Vec<u128> = cases.iter().map(|c| c.0).collect();
                        if keys.iter().any(|k| k & !mask(it) != 0) {
                            e(format!("bb{b}: switch case outside the value's type"));
                        }
                        keys.sort();
                        keys.dedup();
                        if keys.len() != cases.len() {
                            e(format!("bb{b}: duplicate switch case"));
                        }
                    }
                    _ => e(format!("bb{b}: switch on a non-integer value")),
                }
                for (_, t) in cases {
                    check_edge(*t, &[], &mut e);
                }
                check_edge(*default, &[], &mut e);
            }
            Term::Ret(v) => {
                check_use(*v, tpos, &mut e);
                if ty(*v) != Some(f.ret.clone()) {
                    e(format!("bb{b}: returned value has the wrong type"));
                }
            }
            Term::Trap(_) | Term::Unreachable => {}
        }
        let _ = nb;
    }
    check_provenance(f, &def_at, errs, &name);
}

/// Where a reference may point: into the caller's storage through a
/// reference parameter, or into this function's own slots.
#[derive(Clone, Copy, Default, PartialEq)]
struct Origin {
    param: bool,
    local: bool,
}

/// Reference-typed slots (spec/pir.md §2.1) exist only to hold a reference
/// parameter: each is written exactly once, as a whole, in the entry block,
/// with an entry-block parameter of the same type, and is not read in the
/// entry block before that write. Every other slot operation on it except
/// `load` is rejected (`borrow`, `move`, `drop` in the instruction checks;
/// partial loads and stores by their path types).
fn ref_slots(f: &Func, e: &mut impl FnMut(String)) {
    let entry = &f.blocks[0];
    for (k, t) in f.slots.iter().enumerate() {
        if !matches!(t, Type::Ref { .. }) {
            continue;
        }
        let k = k as SlotId;
        let mut stores = Vec::new();
        for (b, blk) in f.blocks.iter().enumerate() {
            for (i, inst) in blk.insts.iter().enumerate() {
                if let InstKind::Store { slot, path, v } = &inst.kind {
                    if *slot == k {
                        stores.push((b, i, path.is_empty(), *v));
                    }
                }
            }
        }
        match stores.as_slice() {
            [(0, i, true, v)] => {
                if !entry.params.contains(v) || f.values.get(*v as usize) != Some(t) {
                    e(format!("slot ${k}: a reference slot must hold an entry-block parameter of its type"));
                }
                let read_early = entry.insts[..*i].iter().any(|inst| {
                    matches!(&inst.kind, InstKind::Load { slot, .. } | InstKind::Move { slot, .. } | InstKind::Borrow { slot, .. } if *slot == k)
                });
                if read_early {
                    e(format!("slot ${k}: reference slot read before it is initialized"));
                }
            }
            _ => e(format!(
                "slot ${k}: a reference slot must be written exactly once, whole, in the entry block"
            )),
        }
    }
}

/// A returned reference must be *proven* to point only into storage reached
/// through the function's reference parameters: its origin must include a
/// parameter and must not include the function's own slots. An unknown
/// origin (no rule derives one) is rejected — the analysis fails closed.
/// Origins flow through reborrows, whole-slot stores, loads and moves, call
/// arguments (a returned reference is tied to every argument) and block
/// arguments. The lattice is finite (two bits per value and slot) and every
/// update only sets bits, so iteration reaches the least fixed point within
/// `2 * (values + slots) + 1` rounds; exceeding that bound is reported.
fn check_provenance(
    f: &Func,
    def_at: &[Option<(BlockId, usize)>],
    errs: &mut Vec<String>,
    name: &str,
) {
    if !matches!(f.ret, Type::Ref { .. }) {
        return;
    }
    let _ = def_at;
    let nv = f.values.len();
    let mut o = vec![Origin::default(); nv];
    let mut slot_o = vec![Origin::default(); f.slots.len()];
    for &p in &f.blocks[0].params {
        if let Some(Type::Ref { .. }) = f.values.get(p as usize) {
            o[p as usize].param = true;
        }
    }
    let join = |a: Origin, b: Origin| Origin {
        param: a.param || b.param,
        local: a.local || b.local,
    };
    let get = |o: &Vec<Origin>, v: ValueId| o.get(v as usize).copied().unwrap_or_default();
    let upd = |cell: &mut Origin, x: Origin, changed: &mut bool| {
        let n = join(*cell, x);
        if n != *cell {
            *cell = n;
            *changed = true;
        }
    };
    let bound = 2 * (nv + f.slots.len()) + 1;
    let mut rounds = 0;
    loop {
        let mut changed = false;
        rounds += 1;
        if rounds > bound {
            errs.push(format!(
                "{name}: reference provenance analysis did not converge"
            ));
            return;
        }
        for blk in &f.blocks {
            for inst in &blk.insts {
                let Some(r) = inst.result else {
                    if let InstKind::Store { slot, path, v } = &inst.kind {
                        let x = get(&o, *v);
                        if let Some(so) = slot_o.get_mut(*slot as usize) {
                            // A partial store taints the whole slot too.
                            let _ = path;
                            upd(so, x, &mut changed);
                        }
                    }
                    continue;
                };
                let x = match &inst.kind {
                    InstKind::Borrow { .. } => Origin {
                        param: false,
                        local: true,
                    },
                    InstKind::BorrowRef { r: src, .. } => get(&o, *src),
                    InstKind::Load { slot, .. } | InstKind::Move { slot, .. } => {
                        slot_o.get(*slot as usize).copied().unwrap_or_default()
                    }
                    InstKind::Call { args, .. } | InstKind::CallIndirect { args, .. } => args
                        .iter()
                        .fold(Origin::default(), |acc, a| join(acc, get(&o, *a))),
                    _ => Origin::default(),
                };
                if let Some(cell) = o.get_mut(r as usize) {
                    upd(cell, x, &mut changed);
                }
            }
            let edges: Vec<(BlockId, Vec<ValueId>)> = match &blk.term {
                Term::Br(t, a) => vec![(*t, a.clone())],
                Term::CondBr { t, f: e, .. } => vec![t.clone(), e.clone()],
                _ => vec![],
            };
            for (t, args) in edges {
                if let Some(tb) = f.blocks.get(t as usize) {
                    for (p, a) in tb.params.iter().zip(&args) {
                        let x = get(&o, *a);
                        if let Some(cell) = o.get_mut(*p as usize) {
                            upd(cell, x, &mut changed);
                        }
                    }
                }
            }
        }
        if !changed {
            break;
        }
    }
    for (b, blk) in f.blocks.iter().enumerate() {
        if let Term::Ret(v) = &blk.term {
            let x = get(&o, *v);
            if x.local {
                errs.push(format!(
                    "{name}: bb{b}: returned reference may point into this function's own storage"
                ));
            } else if !x.param {
                errs.push(format!(
                    "{name}: bb{b}: returned reference has no proven parameter origin"
                ));
            }
        }
    }
}
