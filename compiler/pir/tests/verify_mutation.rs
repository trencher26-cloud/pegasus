//! Mutation testing of the verifier: compile every corpus program, apply
//! thousands of single mutations that each make the PIR invalid, and require
//! that every mutant is rejected — without the verifier panicking.

use pegasus_pir::ir::*;
use pegasus_pir::verify::verify;
use pegasus_sema::source::FsLoader;
use pegasus_sema::ty::IntTy;
use std::path::Path;

fn modules() -> Vec<(String, Module)> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests");
    let mut out = Vec::new();
    for dir in ["run", "sema/accept", "native"] {
        let d = root.join(dir);
        let mut files: Vec<_> = std::fs::read_dir(&d)
            .unwrap()
            .map(|e| e.unwrap().path())
            .filter(|p| p.extension().is_some_and(|e| e == "peg"))
            .collect();
        files.sort();
        for f in files {
            let name = f.file_name().unwrap().to_str().unwrap().to_string();
            let loader = FsLoader { root: d.clone() };
            let (_, m) = pegasus_sema::check_package_with(&loader, &name, |s| {
                pegasus_pir::compile(s).map(|c| c.module)
            });
            let m = m.expect("corpus program must compile");
            assert_eq!(verify(&m), Ok(()), "{name}: corpus PIR must verify");
            out.push((name, m));
        }
    }
    out
}

/// A type certainly different from `t`.
fn other(t: &Type) -> Type {
    if *t == Type::Char {
        Type::Bool
    } else {
        Type::Char
    }
}

fn has_preds(f: &Func, b: usize) -> bool {
    b == 0
        || f.blocks.iter().any(|blk| match &blk.term {
            Term::Br(t, _) => *t as usize == b,
            Term::CondBr { t, f, .. } => t.0 as usize == b || f.0 as usize == b,
            Term::Switch { cases, default, .. } => {
                *default as usize == b || cases.iter().any(|c| c.1 as usize == b)
            }
            _ => false,
        })
}

/// Every used value id in a function.
fn used(f: &Func) -> Vec<ValueId> {
    let mut v = Vec::new();
    for b in &f.blocks {
        if let Term::Ret(x) = &b.term {
            v.push(*x);
        }
    }
    v
}

static REF_MUTANTS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
static BOOL_ORDER_MUTANTS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

#[test]
fn every_mutant_is_rejected() {
    let mut total = 0usize;
    let mut escaped = Vec::new();
    let check =
        |name: &str, what: String, m: &Module, total: &mut usize, escaped: &mut Vec<String>| {
            *total += 1;
            if what.contains("ref slot") || what.contains("own slot") {
                REF_MUTANTS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            }
            let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| verify(m)));
            match r {
                Err(_) => escaped.push(format!("{name}: verifier PANICKED on mutant: {what}")),
                Ok(Ok(())) => escaped.push(format!("{name}: mutant accepted: {what}")),
                Ok(Err(_)) => {}
            }
        };
    for (name, m) in modules() {
        for (fi, f) in m.funcs.iter().enumerate() {
            if f.kind != FuncKind::Defined {
                continue;
            }
            // (R) Reference-discipline mutants (PIR0.4).
            for (k, t) in f.slots.iter().enumerate() {
                let k = k as SlotId;
                if matches!(t, Type::Ref { .. }) {
                    // (R1) Drop a reference parameter slot.
                    let mut mm = m.clone();
                    let n = mm.funcs[fi].blocks[0].insts.len();
                    mm.funcs[fi].blocks[0].insts.insert(
                        n,
                        Inst {
                            result: None,
                            kind: InstKind::Drop { slot: k },
                        },
                    );
                    check(
                        &name,
                        format!("#{fi} drop ref slot ${k}"),
                        &mm,
                        &mut total,
                        &mut escaped,
                    );
                    // (R2) Write the reference slot a second time.
                    if let Some(p) = f.blocks[0].params.get(k as usize) {
                        let mut mm = m.clone();
                        let n = mm.funcs[fi].blocks[0].insts.len();
                        mm.funcs[fi].blocks[0].insts.insert(
                            n,
                            Inst {
                                result: None,
                                kind: InstKind::Store {
                                    slot: k,
                                    path: vec![],
                                    v: *p,
                                },
                            },
                        );
                        check(
                            &name,
                            format!("#{fi} second store to ref slot ${k}"),
                            &mm,
                            &mut total,
                            &mut escaped,
                        );
                    }
                } else if matches!(f.ret, Type::Ref { .. }) {
                    // (R3) Return a borrow of one of this function's own
                    // slots instead of the derived reference.
                    for (bi, b) in f.blocks.iter().enumerate() {
                        if let Term::Ret(v) = b.term {
                            if f.values[v as usize]
                                != (Type::Ref {
                                    mutable: false,
                                    ty: Box::new(t.clone()),
                                })
                            {
                                continue;
                            }
                            let mut mm = m.clone();
                            let fresh = mm.funcs[fi].values.len() as ValueId;
                            mm.funcs[fi].values.push(Type::Ref {
                                mutable: false,
                                ty: Box::new(t.clone()),
                            });
                            mm.funcs[fi].blocks[bi].insts.push(Inst {
                                result: Some(fresh),
                                kind: InstKind::Borrow {
                                    slot: k,
                                    path: vec![],
                                    mutable: false,
                                },
                            });
                            mm.funcs[fi].blocks[bi].term = Term::Ret(fresh);
                            check(
                                &name,
                                format!("#{fi} bb{bi} return borrow of own slot ${k}"),
                                &mm,
                                &mut total,
                                &mut escaped,
                            );
                        }
                    }
                }
            }
            for (bi, b) in f.blocks.iter().enumerate() {
                // (R4) Loads of reference slots turned into moves.
                for (ii, ins) in b.insts.iter().enumerate() {
                    if let InstKind::Load { slot, path } = &ins.kind {
                        if matches!(f.slots.get(*slot as usize), Some(Type::Ref { .. })) {
                            let mut mm = m.clone();
                            mm.funcs[fi].blocks[bi].insts[ii].kind = InstKind::Move {
                                slot: *slot,
                                path: path
                                    .iter()
                                    .filter_map(|p| {
                                        if let Proj::Field(i) = p {
                                            Some(*i)
                                        } else {
                                            None
                                        }
                                    })
                                    .collect(),
                            };
                            check(
                                &name,
                                format!("#{fi} bb{bi}[{ii}] move of ref slot"),
                                &mm,
                                &mut total,
                                &mut escaped,
                            );
                        }
                    }
                }
            }
            for (bi, b) in f.blocks.iter().enumerate() {
                // (1) Result type changed.
                for (ii, ins) in b.insts.iter().enumerate() {
                    if let Some(r) = ins.result {
                        let mut mm = m.clone();
                        let t = mm.funcs[fi].values[r as usize].clone();
                        mm.funcs[fi].values[r as usize] = other(&t);
                        check(
                            &name,
                            format!("#{fi} bb{bi}[{ii}] result type"),
                            &mm,
                            &mut total,
                            &mut escaped,
                        );
                    }
                    // (2) Constant replaced by one of another type.
                    if let InstKind::Const(c) = &ins.kind {
                        let mut mm = m.clone();
                        let nc = if matches!(c, Const::Char(_)) {
                            Const::Bool(false)
                        } else {
                            Const::Char('x')
                        };
                        mm.funcs[fi].blocks[bi].insts[ii].kind = InstKind::Const(nc);
                        check(
                            &name,
                            format!("#{fi} bb{bi}[{ii}] const kind"),
                            &mm,
                            &mut total,
                            &mut escaped,
                        );
                    }
                    // (3) Illegal overflow mode.
                    if let InstKind::Bin {
                        op,
                        ov: Ov::Trap,
                        a,
                        b: bv,
                    } = &ins.kind
                    {
                        if !matches!(op, BinOp::Add | BinOp::Sub | BinOp::Mul)
                            || !matches!(f.values[*a as usize], Type::Int(_))
                        {
                            let mut mm = m.clone();
                            mm.funcs[fi].blocks[bi].insts[ii].kind = InstKind::Bin {
                                op: *op,
                                ov: Ov::Wrap,
                                a: *a,
                                b: *bv,
                            };
                            check(
                                &name,
                                format!("#{fi} bb{bi}[{ii}] {op:?}.wrap"),
                                &mm,
                                &mut total,
                                &mut escaped,
                            );
                        }
                    }
                    // (4) Out-of-range identifiers.
                    let bad: Option<InstKind> = match &ins.kind {
                        InstKind::Struct { fields, .. } => Some(InstKind::Struct {
                            id: 9999,
                            fields: fields.clone(),
                        }),
                        InstKind::Variant {
                            variant, fields, ..
                        } => Some(InstKind::Variant {
                            id: 9999,
                            variant: *variant,
                            fields: fields.clone(),
                        }),
                        InstKind::Call { args, .. } => Some(InstKind::Call {
                            f: 9999,
                            args: args.clone(),
                        }),
                        InstKind::Load { path, .. } => Some(InstKind::Load {
                            slot: 9999,
                            path: path.clone(),
                        }),
                        InstKind::Store { path, v, .. } => Some(InstKind::Store {
                            slot: 9999,
                            path: path.clone(),
                            v: *v,
                        }),
                        InstKind::Closure { captures, .. } => Some(InstKind::Closure {
                            f: 9999,
                            captures: captures.clone(),
                        }),
                        InstKind::Extract { v, .. } => Some(InstKind::Extract { v: *v, idx: 9999 }),
                        InstKind::VariantField { v, variant, .. } => Some(InstKind::VariantField {
                            v: *v,
                            variant: *variant,
                            idx: 9999,
                        }),
                        _ => None,
                    };
                    if let Some(k) = bad {
                        let mut mm = m.clone();
                        mm.funcs[fi].blocks[bi].insts[ii].kind = k;
                        check(
                            &name,
                            format!("#{fi} bb{bi}[{ii}] bad id"),
                            &mm,
                            &mut total,
                            &mut escaped,
                        );
                    }
                    // (4b) Flip a borrow's mutability (its declared type no
                    // longer matches).
                    let flipped = match &ins.kind {
                        InstKind::Borrow {
                            slot,
                            path,
                            mutable,
                        } => Some(InstKind::Borrow {
                            slot: *slot,
                            path: path.clone(),
                            mutable: !mutable,
                        }),
                        InstKind::BorrowRef { r, path, mutable } => Some(InstKind::BorrowRef {
                            r: *r,
                            path: path.clone(),
                            mutable: !mutable,
                        }),
                        _ => None,
                    };
                    if let Some(k) = flipped {
                        let mut mm = m.clone();
                        mm.funcs[fi].blocks[bi].insts[ii].kind = k;
                        check(
                            &name,
                            format!("#{fi} bb{bi}[{ii}] borrow mutability"),
                            &mm,
                            &mut total,
                            &mut escaped,
                        );
                    }
                    // (4c) A store through a reference turned into a load
                    // result-less store to an out-of-range slot.
                    if let InstKind::StoreRef { path, v, .. } = &ins.kind {
                        let mut mm = m.clone();
                        mm.funcs[fi].blocks[bi].insts[ii].kind = InstKind::StoreRef {
                            r: 99_999,
                            path: path.clone(),
                            v: *v,
                        };
                        check(
                            &name,
                            format!("#{fi} bb{bi}[{ii}] store_ref bad ref"),
                            &mm,
                            &mut total,
                            &mut escaped,
                        );
                    }
                    // (4d) Equality on booleans turned into an ordering
                    // (`bool` has none, spec/semantics.md §6).
                    if let InstKind::Cmp {
                        op: CmpOp::Eq | CmpOp::Ne,
                        a,
                        b,
                    } = &ins.kind
                    {
                        if f.values[*a as usize] == Type::Bool {
                            for op in [CmpOp::Lt, CmpOp::Le, CmpOp::Gt, CmpOp::Ge] {
                                let mut mm = m.clone();
                                mm.funcs[fi].blocks[bi].insts[ii].kind =
                                    InstKind::Cmp { op, a: *a, b: *b };
                                check(
                                    &name,
                                    format!("#{fi} bb{bi}[{ii}] bool ordering {op:?}"),
                                    &mm,
                                    &mut total,
                                    &mut escaped,
                                );
                                BOOL_ORDER_MUTANTS
                                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                            }
                        }
                    }
                    // (5) Duplicate definition: the result id of another value.
                    if let Some(r) = ins.result {
                        if r > 0 {
                            let mut mm = m.clone();
                            mm.funcs[fi].blocks[bi].insts[ii].result = Some(r - 1);
                            check(
                                &name,
                                format!("#{fi} bb{bi}[{ii}] duplicate def"),
                                &mm,
                                &mut total,
                                &mut escaped,
                            );
                        }
                    }
                }
                // (6) Block parameter type changed (reachable blocks only:
                // an unreachable block's parameters have no incoming edges).
                if bi > 0 && has_preds(f, bi) {
                    for &p in &b.params {
                        let mut mm = m.clone();
                        let t = mm.funcs[fi].values[p as usize].clone();
                        mm.funcs[fi].values[p as usize] = other(&t);
                        check(
                            &name,
                            format!("#{fi} bb{bi} param %{p} type"),
                            &mm,
                            &mut total,
                            &mut escaped,
                        );
                    }
                }
                // (7) Branch to a missing block; use of an undeclared value.
                if let Term::Br(_, a) = &b.term {
                    let mut mm = m.clone();
                    mm.funcs[fi].blocks[bi].term = Term::Br(9999, a.clone());
                    check(
                        &name,
                        format!("#{fi} bb{bi} br target"),
                        &mm,
                        &mut total,
                        &mut escaped,
                    );
                }
                if let Term::Ret(_) = &b.term {
                    let mut mm = m.clone();
                    mm.funcs[fi].blocks[bi].term = Term::Ret(99_999);
                    check(
                        &name,
                        format!("#{fi} bb{bi} ret undeclared"),
                        &mm,
                        &mut total,
                        &mut escaped,
                    );
                }
            }
            // (8) Delete the definition of a returned value.
            for v in used(f) {
                for (bi, b) in f.blocks.iter().enumerate() {
                    if let Some(ii) = b.insts.iter().position(|i| i.result == Some(v)) {
                        let mut mm = m.clone();
                        mm.funcs[fi].blocks[bi].insts.remove(ii);
                        check(
                            &name,
                            format!("#{fi} delete def of %{v}"),
                            &mm,
                            &mut total,
                            &mut escaped,
                        );
                    }
                }
            }
            // (9) An extra declared value that is never defined.
            let mut mm = m.clone();
            mm.funcs[fi].values.push(Type::Int(IntTy::I32));
            check(
                &name,
                format!("#{fi} undefined unused value"),
                &mm,
                &mut total,
                &mut escaped,
            );
        }
    }
    assert!(total > 2000, "too few mutants: {total}");
    assert!(
        escaped.is_empty(),
        "{} of {total} mutants escaped:\n{}",
        escaped.len(),
        escaped[..escaped.len().min(30)].join("\n")
    );
    let refs = REF_MUTANTS.load(std::sync::atomic::Ordering::Relaxed);
    assert!(refs > 0, "no reference-discipline mutants were generated");
    let bools = BOOL_ORDER_MUTANTS.load(std::sync::atomic::Ordering::Relaxed);
    assert!(bools > 0, "no boolean-ordering mutants were generated");
    eprintln!(
        "{total} mutants ({refs} reference-discipline, {bools} boolean-ordering), all rejected"
    );
}
