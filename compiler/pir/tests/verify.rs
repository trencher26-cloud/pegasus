//! The verifier rejects malformed PIR: one targeted test per rule.

use pegasus_pir::interp::{Interp, Limits};
use pegasus_pir::ir::*;
use pegasus_pir::verify::verify;
use pegasus_sema::ty::IntTy;

const I32: Type = Type::Int(IntTy::I32);
const U32: Type = Type::Int(IntTy::U32);

fn module(blocks: Vec<Block>, values: Vec<Type>, params: Vec<Type>, ret: Type) -> Module {
    Module {
        funcs: vec![Func {
            name: "f".into(),
            kind: FuncKind::Defined,
            params,
            ret,
            slots: vec![],
            slot_names: vec![],
            values,
            blocks,
        }],
        ..Default::default()
    }
}

fn inst(r: u32, kind: InstKind) -> Inst {
    Inst {
        result: Some(r),
        kind,
    }
}

/// One block, no parameters, returning `ret_v`.
fn single(insts: Vec<Inst>, values: Vec<Type>, ret: Type, ret_v: u32) -> Module {
    module(
        vec![Block {
            params: vec![],
            insts,
            term: Term::Ret(ret_v),
        }],
        values,
        vec![],
        ret,
    )
}

fn rejects(m: &Module, needle: &str) {
    let errs = verify(m)
        .expect_err("verifier accepted malformed PIR")
        .join("\n");
    assert!(errs.contains(needle), "expected `{needle}` in:\n{errs}");
}

#[test]
fn accepts_a_well_formed_function() {
    let m = module(
        vec![Block {
            params: vec![0],
            insts: vec![inst(
                1,
                InstKind::Bin {
                    op: BinOp::Add,
                    ov: Ov::Wrap,
                    a: 0,
                    b: 0,
                },
            )],
            term: Term::Ret(1),
        }],
        vec![I32, I32],
        vec![I32],
        I32,
    );
    assert_eq!(verify(&m), Ok(()));
}

#[test]
fn constant_of_the_wrong_type() {
    // The reviewer's example: an i32 value defined by a bool constant.
    rejects(
        &single(
            vec![inst(0, InstKind::Const(Const::Bool(true)))],
            vec![I32],
            I32,
            0,
        ),
        "result type does not match",
    );
    // Integer bits outside the type.
    rejects(
        &single(
            vec![inst(0, InstKind::Const(Const::Int(300, IntTy::U8)))],
            vec![Type::Int(IntTy::U8)],
            Type::Int(IntTy::U8),
            0,
        ),
        "bits outside",
    );
}

#[test]
fn unused_undefined_and_duplicate_values() {
    // %1 is declared but never defined (and never used).
    rejects(
        &single(
            vec![inst(0, InstKind::Const(Const::Int(1, IntTy::I32)))],
            vec![I32, I32],
            I32,
            0,
        ),
        "defined 0 times",
    );
    // %0 defined twice.
    rejects(
        &single(
            vec![
                inst(0, InstKind::Const(Const::Int(1, IntTy::I32))),
                inst(0, InstKind::Const(Const::Int(2, IntTy::I32))),
            ],
            vec![I32],
            I32,
            0,
        ),
        "defined 2 times",
    );
}

#[test]
fn invalid_identifiers_are_errors_not_panics() {
    rejects(
        &single(
            vec![inst(
                0,
                InstKind::Struct {
                    id: 7,
                    fields: vec![],
                },
            )],
            vec![Type::Struct(7)],
            Type::Struct(7),
            0,
        ),
        "invalid",
    );
    rejects(
        &single(
            vec![inst(
                0,
                InstKind::Variant {
                    id: 3,
                    variant: 0,
                    fields: vec![],
                },
            )],
            vec![I32],
            I32,
            0,
        ),
        "out of range",
    );
    rejects(
        &single(
            vec![inst(
                0,
                InstKind::Call {
                    f: 99,
                    args: vec![],
                },
            )],
            vec![I32],
            I32,
            0,
        ),
        "unknown function",
    );
    rejects(
        &single(
            vec![inst(
                0,
                InstKind::Load {
                    slot: 4,
                    path: vec![],
                },
            )],
            vec![I32],
            I32,
            0,
        ),
        "slot",
    );
    rejects(
        &single(
            vec![inst(
                0,
                InstKind::Move {
                    slot: 4,
                    path: vec![],
                },
            )],
            vec![I32],
            I32,
            0,
        ),
        "slot",
    );
    rejects(
        &single(
            vec![inst(
                0,
                InstKind::Closure {
                    f: 42,
                    captures: vec![],
                },
            )],
            vec![I32],
            I32,
            0,
        ),
        "unknown function",
    );
    let m = module(
        vec![Block {
            params: vec![],
            insts: vec![],
            term: Term::Br(9, vec![]),
        }],
        vec![],
        vec![],
        I32,
    );
    rejects(&m, "missing bb9");
}

#[test]
fn operation_and_overflow_mode_legality() {
    let k = |v| inst(v, InstKind::Const(Const::Int(5, IntTy::I32)));
    for (op, ov) in [
        (BinOp::Div, Ov::Wrap),
        (BinOp::Rem, Ov::Sat),
        (BinOp::Shl, Ov::Wrap),
        (BinOp::Pow, Ov::Sat),
        (BinOp::And, Ov::Wrap),
    ] {
        let m = single(
            vec![k(0), k(1), inst(2, InstKind::Bin { op, ov, a: 0, b: 1 })],
            vec![I32, I32, I32],
            I32,
            2,
        );
        rejects(&m, "mode");
        // The interpreter refuses the same combination instead of computing it.
        let mut it = Interp::new(&m, Limits::default());
        let err = it
            .call(0, vec![])
            .expect_err("interpreter computed an invalid operation");
        assert!(err.message.contains("invalid PIR"), "{}", err.message);
    }
    // Float operations have no overflow mode; shifts need unsigned amounts.
    let f = |v| {
        inst(
            v,
            InstKind::Const(Const::Float(1.0, pegasus_sema::ty::FloatTy::F64)),
        )
    };
    let f64t = Type::Float(pegasus_sema::ty::FloatTy::F64);
    rejects(
        &single(
            vec![
                f(0),
                f(1),
                inst(
                    2,
                    InstKind::Bin {
                        op: BinOp::Add,
                        ov: Ov::Wrap,
                        a: 0,
                        b: 1,
                    },
                ),
            ],
            vec![f64t.clone(), f64t.clone(), f64t.clone()],
            f64t,
            2,
        ),
        "no overflow mode",
    );
    rejects(
        &single(
            vec![
                k(0),
                k(1),
                inst(
                    2,
                    InstKind::Bin {
                        op: BinOp::Shl,
                        ov: Ov::Trap,
                        a: 0,
                        b: 1,
                    },
                ),
            ],
            vec![I32, I32, I32],
            I32,
            2,
        ),
        "unsigned",
    );
}

#[test]
fn every_instruction_kind_is_type_checked() {
    let k = |v, t: IntTy| inst(v, InstKind::Const(Const::Int(1, t)));
    // Un: negation of an unsigned value, logical not of an integer.
    rejects(
        &single(
            vec![
                k(0, IntTy::U32),
                inst(
                    1,
                    InstKind::Un {
                        op: UnOp::Neg,
                        a: 0,
                    },
                ),
            ],
            vec![U32, U32],
            U32,
            1,
        ),
        "Neg",
    );
    rejects(
        &single(
            vec![
                k(0, IntTy::I32),
                inst(
                    1,
                    InstKind::Un {
                        op: UnOp::Not,
                        a: 0,
                    },
                ),
            ],
            vec![I32, I32],
            I32,
            1,
        ),
        "Not",
    );
    // Array: mixed element types; wrong declared length.
    rejects(
        &single(
            vec![
                k(0, IntTy::I32),
                k(1, IntTy::U32),
                inst(2, InstKind::Array(vec![0, 1])),
            ],
            vec![I32, U32, Type::Array(Box::new(I32), 2)],
            I32,
            0,
        ),
        "different types",
    );
    rejects(
        &single(
            vec![k(0, IntTy::I32), inst(1, InstKind::Array(vec![0]))],
            vec![I32, Type::Array(Box::new(I32), 3)],
            I32,
            0,
        ),
        "result type",
    );
    // Repeat: declared length differs.
    rejects(
        &single(
            vec![k(0, IntTy::I32), inst(1, InstKind::Repeat { v: 0, n: 4 })],
            vec![I32, Type::Array(Box::new(I32), 5)],
            I32,
            0,
        ),
        "result type",
    );
    // ExtractIndex: index must be usize; base must be an array.
    let arr = Type::Array(Box::new(I32), 2);
    rejects(
        &single(
            vec![
                k(0, IntTy::I32),
                inst(1, InstKind::Repeat { v: 0, n: 2 }),
                inst(2, InstKind::ExtractIndex { v: 1, i: 0 }),
            ],
            vec![I32, arr.clone(), I32],
            I32,
            2,
        ),
        "usize",
    );
    // Tag and payload of a non-enum.
    rejects(
        &single(
            vec![k(0, IntTy::I32), inst(1, InstKind::Tag { v: 0 })],
            vec![I32, U32],
            I32,
            0,
        ),
        "non-enum",
    );
    rejects(
        &single(
            vec![
                k(0, IntTy::I32),
                inst(
                    1,
                    InstKind::VariantField {
                        v: 0,
                        variant: 0,
                        idx: 0,
                    },
                ),
            ],
            vec![I32, I32],
            I32,
            1,
        ),
        "non-enum",
    );
    // Cast that is not lossless.
    rejects(
        &single(
            vec![
                k(0, IntTy::U32),
                inst(
                    1,
                    InstKind::Cast {
                        a: 0,
                        to: Type::Int(IntTy::U8),
                    },
                ),
            ],
            vec![U32, Type::Int(IntTy::U8)],
            U32,
            0,
        ),
        "lossless",
    );
}

#[test]
fn control_flow_rules() {
    // Non-dominating use across blocks.
    let m = module(
        vec![
            Block {
                params: vec![0],
                insts: vec![],
                term: Term::CondBr {
                    c: 0,
                    t: (1, vec![]),
                    f: (2, vec![]),
                },
            },
            Block {
                params: vec![],
                insts: vec![inst(1, InstKind::Const(Const::Int(1, IntTy::I32)))],
                term: Term::Br(2, vec![]),
            },
            Block {
                params: vec![],
                insts: vec![],
                term: Term::Ret(1),
            },
        ],
        vec![Type::Bool, I32],
        vec![Type::Bool],
        I32,
    );
    rejects(&m, "not dominated");
    // Block arguments; duplicate switch cases.
    let m = module(
        vec![
            Block {
                params: vec![0],
                insts: vec![],
                term: Term::Br(1, vec![]),
            },
            Block {
                params: vec![1],
                insts: vec![],
                term: Term::Ret(1),
            },
        ],
        vec![I32, I32],
        vec![I32],
        I32,
    );
    rejects(&m, "do not match its parameters");
    let m = module(
        vec![
            Block {
                params: vec![0],
                insts: vec![],
                term: Term::Switch {
                    v: 0,
                    cases: vec![(1, 1), (1, 1)],
                    default: 1,
                },
            },
            Block {
                params: vec![],
                insts: vec![],
                term: Term::Ret(0),
            },
        ],
        vec![I32],
        vec![I32],
        I32,
    );
    rejects(&m, "duplicate switch case");
}

fn rref(mutable: bool, t: Type) -> Type {
    Type::Ref {
        mutable,
        ty: Box::new(t),
    }
}

#[test]
fn reference_rules() {
    // Store through a shared reference.
    let m = module(
        vec![Block {
            params: vec![0],
            insts: vec![
                inst(1, InstKind::Const(Const::Int(1, IntTy::I32))),
                Inst {
                    result: None,
                    kind: InstKind::StoreRef {
                        r: 0,
                        path: vec![],
                        v: 1,
                    },
                },
                inst(2, InstKind::Tuple(vec![])),
            ],
            term: Term::Ret(2),
        }],
        vec![rref(false, I32), I32, Type::unit()],
        vec![rref(false, I32)],
        Type::unit(),
    );
    rejects(&m, "shared reference");
    // Exclusive reborrow through a shared reference.
    let m = module(
        vec![Block {
            params: vec![0],
            insts: vec![inst(
                1,
                InstKind::BorrowRef {
                    r: 0,
                    path: vec![],
                    mutable: true,
                },
            )],
            term: Term::Ret(0),
        }],
        vec![rref(false, I32), rref(true, I32)],
        vec![rref(false, I32)],
        rref(false, I32),
    );
    rejects(&m, "exclusive borrow through a shared reference");
    // A reference nested inside an aggregate type.
    let m = single(
        vec![inst(0, InstKind::Tuple(vec![]))],
        vec![Type::Tuple(vec![rref(false, I32)])],
        Type::unit(),
        0,
    );
    rejects(&m, "invalid type");
}

#[test]
fn returned_reference_must_not_point_into_own_frame() {
    let mut m = single(
        vec![
            inst(0, InstKind::Const(Const::Int(1, IntTy::I32))),
            Inst {
                result: None,
                kind: InstKind::Store {
                    slot: 0,
                    path: vec![],
                    v: 0,
                },
            },
            inst(
                1,
                InstKind::Borrow {
                    slot: 0,
                    path: vec![],
                    mutable: false,
                },
            ),
        ],
        vec![I32, rref(false, I32)],
        rref(false, I32),
        1,
    );
    m.funcs[0].slots = vec![I32];
    m.funcs[0].slot_names = vec!["local".into()];
    rejects(&m, "own storage");
}

#[test]
fn interpreter_detects_dangling_references() {
    // Bypass the verifier: @f returns a borrow of its own slot; @main reads it.
    let mut m = single(
        vec![
            inst(0, InstKind::Const(Const::Int(1, IntTy::I32))),
            Inst {
                result: None,
                kind: InstKind::Store {
                    slot: 0,
                    path: vec![],
                    v: 0,
                },
            },
            inst(
                1,
                InstKind::Borrow {
                    slot: 0,
                    path: vec![],
                    mutable: false,
                },
            ),
        ],
        vec![I32, rref(false, I32)],
        rref(false, I32),
        1,
    );
    m.funcs[0].slots = vec![I32];
    m.funcs[0].slot_names = vec!["local".into()];
    m.funcs.push(Func {
        name: "main".into(),
        kind: FuncKind::Defined,
        params: vec![],
        ret: I32,
        slots: vec![],
        slot_names: vec![],
        values: vec![rref(false, I32), I32],
        blocks: vec![Block {
            params: vec![],
            insts: vec![
                inst(0, InstKind::Call { f: 0, args: vec![] }),
                inst(1, InstKind::LoadRef { r: 0, path: vec![] }),
            ],
            term: Term::Ret(1),
        }],
    });
    let mut it = Interp::new(&m, Limits::default());
    let err = it.call(1, vec![]).expect_err("dangling read must trap");
    assert!(err.message.contains("dangling"), "{}", err.message);
}

fn st(slot: u32, v: u32) -> Inst {
    Inst {
        result: None,
        kind: InstKind::Store {
            slot,
            path: vec![],
            v,
        },
    }
}

/// The PIR0.3 review's exact sequence: a borrow of a local stashed in a
/// reference-typed slot and moved back out to hide its provenance.
#[test]
fn reference_hidden_in_slot_and_moved_out_is_rejected() {
    let mut m = single(
        vec![
            inst(0, InstKind::Const(Const::Int(42, IntTy::I32))),
            st(0, 0),
            inst(
                1,
                InstKind::Borrow {
                    slot: 0,
                    path: vec![],
                    mutable: false,
                },
            ),
            st(1, 1),
            inst(
                2,
                InstKind::Move {
                    slot: 1,
                    path: vec![],
                },
            ),
        ],
        vec![I32, rref(false, I32), rref(false, I32)],
        rref(false, I32),
        2,
    );
    m.funcs[0].slots = vec![I32, rref(false, I32)];
    m.funcs[0].slot_names = vec!["x".into(), "r".into()];
    rejects(&m, "must hold an entry-block parameter");
    rejects(&m, "move out of a reference parameter slot");
    rejects(&m, "own storage");
    // The same with `load` instead of `move`: the slot rule and provenance
    // both still reject it.
    m.funcs[0].blocks[0].insts[4] = inst(
        2,
        InstKind::Load {
            slot: 1,
            path: vec![],
        },
    );
    rejects(&m, "must hold an entry-block parameter");
    rejects(&m, "own storage");
}

/// A genuine reference parameter slot overwritten with a local borrow.
#[test]
fn reference_parameter_slot_overwritten_is_rejected() {
    let mut m = module(
        vec![Block {
            params: vec![0],
            insts: vec![
                st(0, 0),
                inst(1, InstKind::Const(Const::Int(1, IntTy::I32))),
                st(1, 1),
                inst(
                    2,
                    InstKind::Borrow {
                        slot: 1,
                        path: vec![],
                        mutable: false,
                    },
                ),
                st(0, 2),
                inst(
                    3,
                    InstKind::Load {
                        slot: 0,
                        path: vec![],
                    },
                ),
            ],
            term: Term::Ret(3),
        }],
        vec![rref(false, I32), I32, rref(false, I32), rref(false, I32)],
        vec![rref(false, I32)],
        rref(false, I32),
    );
    m.funcs[0].slots = vec![rref(false, I32), I32];
    m.funcs[0].slot_names = vec!["p".into(), "x".into()];
    rejects(&m, "written exactly once");
    rejects(&m, "own storage");
}

#[test]
fn reference_slot_rules() {
    // A proper parameter slot, then dropped / read before initialization.
    let base = |insts: Vec<Inst>| {
        let mut m = module(
            vec![Block {
                params: vec![0],
                insts,
                term: Term::Ret(0),
            }],
            vec![rref(false, I32), rref(false, I32)],
            vec![rref(false, I32)],
            rref(false, I32),
        );
        m.funcs[0].slots = vec![rref(false, I32)];
        m.funcs[0].slot_names = vec!["p".into()];
        m
    };
    assert_eq!(
        verify(&base(vec![
            st(0, 0),
            inst(
                1,
                InstKind::Load {
                    slot: 0,
                    path: vec![]
                }
            )
        ])),
        Ok(())
    );
    rejects(
        &base(vec![
            st(0, 0),
            Inst {
                result: None,
                kind: InstKind::Drop { slot: 0 },
            },
        ]),
        "drop of a reference parameter slot",
    );
    rejects(
        &base(vec![
            inst(
                1,
                InstKind::Load {
                    slot: 0,
                    path: vec![],
                },
            ),
            st(0, 0),
        ]),
        "read before it is initialized",
    );
    rejects(&base(vec![]), "written exactly once");
}

#[test]
fn returned_reference_needs_a_proven_parameter_origin() {
    // A reference produced by a call with no reference arguments has no
    // origin at all: rejected (fail closed), not accepted for lack of a
    // local origin.
    let mut m = module(
        vec![Block {
            params: vec![],
            insts: vec![inst(0, InstKind::Call { f: 0, args: vec![] })],
            term: Term::Ret(0),
        }],
        vec![rref(false, I32)],
        vec![],
        rref(false, I32),
    );
    m.funcs[0].name = "f".into();
    rejects(&m, "no proven parameter origin");
}

#[test]
fn extern_functions_cannot_carry_references() {
    let mut m = single(
        vec![inst(0, InstKind::Tuple(vec![]))],
        vec![Type::unit()],
        Type::unit(),
        0,
    );
    m.funcs.push(Func {
        name: "ext".into(),
        kind: FuncKind::Extern("C".into()),
        params: vec![rref(false, I32)],
        ret: Type::unit(),
        slots: vec![],
        slot_names: vec![],
        values: vec![],
        blocks: vec![],
    });
    rejects(&m, "extern function cannot take or return a reference");
}

/// Finding 1 of the PIR0.4 review: `bool` has equality but no ordering.
#[test]
fn boolean_ordering_is_rejected() {
    let prog = |op: CmpOp| {
        single(
            vec![
                inst(0, InstKind::Const(Const::Bool(false))),
                inst(1, InstKind::Const(Const::Bool(true))),
                inst(2, InstKind::Cmp { op, a: 0, b: 1 }),
            ],
            vec![Type::Bool, Type::Bool, Type::Bool],
            Type::Bool,
            2,
        )
    };
    for op in [CmpOp::Lt, CmpOp::Le, CmpOp::Gt, CmpOp::Ge] {
        rejects(&prog(op), "is not defined on this operand type");
    }
    for op in [CmpOp::Eq, CmpOp::Ne] {
        assert_eq!(verify(&prog(op)), Ok(()), "{op:?} on bool must stay valid");
    }
}

#[test]
fn references_are_not_compared_as_addresses() {
    let m = module(
        vec![Block {
            params: vec![0, 1],
            insts: vec![inst(
                2,
                InstKind::Cmp {
                    op: CmpOp::Eq,
                    a: 0,
                    b: 1,
                },
            )],
            term: Term::Ret(2),
        }],
        vec![rref(false, I32), rref(false, I32), Type::Bool],
        vec![rref(false, I32), rref(false, I32)],
        Type::Bool,
    );
    rejects(&m, "is not defined on this operand type");
}
