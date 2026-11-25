//! PIR v0.1 data structures and text printer (spec/pir.md).

use pegasus_sema::ty::{FloatTy, IntTy};
use std::fmt::{self, Write};

pub type ValueId = u32;
pub type BlockId = u32;
pub type FuncId = u32;
pub type StructId = u32;
pub type EnumId = u32;
pub type SlotId = u32;

/// Monomorphic PIR types.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Type {
    Int(IntTy),
    Float(FloatTy),
    Bool,
    Char,
    Str,
    Tuple(Vec<Type>),
    Array(Box<Type>, u64),
    Struct(StructId),
    Enum(EnumId),
    /// A function value (function plus captured environment).
    Fn(Vec<Type>, Box<Type>),
    Ptr(bool, Box<Type>),
    Region,
    /// A second-class borrow (spec/pir.md §2.1): `&T` (shared, `in`) or
    /// `&var T` (exclusive, `inout`). Only parameters, returns, SSA values and
    /// parameter slots may have a reference type, never aggregate members.
    Ref {
        mutable: bool,
        ty: Box<Type>,
    },
}

impl Type {
    pub fn unit() -> Type {
        Type::Tuple(Vec::new())
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Const {
    /// Integer bit pattern (two's complement in the type's width).
    Int(u128, IntTy),
    Float(f64, FloatTy),
    Bool(bool),
    Char(char),
    Str(String),
}

/// What an integer operation does when the mathematical result does not fit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ov {
    /// Trap (the default for `+ - * / % **` and shifts; Phase 0 §7).
    Trap,
    /// Two's-complement wrap (`+% -% *%`).
    Wrap,
    /// Clamp to the type's range (`+| -| *|`).
    Sat,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    Rem,
    Pow,
    Shl,
    Shr,
    And,
    Or,
    Xor,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CmpOp {
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnOp {
    /// Arithmetic negation (traps on the signed minimum).
    Neg,
    /// Logical not on bool.
    Not,
    /// Bitwise not on integers.
    BitNot,
}

/// A place projection.
#[derive(Clone, Debug, PartialEq)]
pub enum Proj {
    /// Field of a tuple or struct.
    Field(u32),
    /// Element of an array; bounds-checked (traps when out of range).
    Index(ValueId),
}

#[derive(Clone, Debug, PartialEq)]
pub enum InstKind {
    Const(Const),
    Bin {
        op: BinOp,
        ov: Ov,
        a: ValueId,
        b: ValueId,
    },
    Un {
        op: UnOp,
        a: ValueId,
    },
    Cmp {
        op: CmpOp,
        a: ValueId,
        b: ValueId,
    },
    /// A lossless conversion to `to` (sema guarantees losslessness).
    Cast {
        a: ValueId,
        to: Type,
    },
    Load {
        slot: SlotId,
        path: Vec<Proj>,
    },
    Store {
        slot: SlotId,
        path: Vec<Proj>,
        v: ValueId,
    },
    Tuple(Vec<ValueId>),
    Struct {
        id: StructId,
        fields: Vec<ValueId>,
    },
    Variant {
        id: EnumId,
        variant: u32,
        fields: Vec<ValueId>,
    },
    Array(Vec<ValueId>),
    Repeat {
        v: ValueId,
        n: u64,
    },
    /// Field `idx` of a tuple or struct value.
    Extract {
        v: ValueId,
        idx: u32,
    },
    /// Element of an array value; bounds-checked.
    ExtractIndex {
        v: ValueId,
        i: ValueId,
    },
    /// The variant index of an enum value, as `u32`.
    Tag {
        v: ValueId,
    },
    /// Payload field of an enum value known to be `variant`.
    VariantField {
        v: ValueId,
        variant: u32,
        idx: u32,
    },
    Call {
        f: FuncId,
        args: Vec<ValueId>,
    },
    CallIndirect {
        f: ValueId,
        args: Vec<ValueId>,
    },
    /// A function value: `f` with captured values prepended to its arguments.
    Closure {
        f: FuncId,
        captures: Vec<ValueId>,
    },
    /// Read a slot's value and end the slot's ownership of it (PURM move).
    /// The slot is uninitialized afterwards until it is stored to again.
    /// `path` selects a field (partial move); empty moves the whole slot.
    Move {
        slot: SlotId,
        path: Vec<u32>,
    },
    /// End of the owner's scope: destroy the slot's value if it still holds
    /// one (a moved-from slot holds none), then mark it uninitialized.
    Drop {
        slot: SlotId,
    },
    /// Borrow a place in a slot: `&` (shared) or `&var` (exclusive).
    Borrow {
        slot: SlotId,
        path: Vec<Proj>,
        mutable: bool,
    },
    /// Borrow a place reached through a reference (reborrow / projection).
    BorrowRef {
        r: ValueId,
        path: Vec<Proj>,
        mutable: bool,
    },
    /// Read the place a reference (plus `path`) denotes.
    LoadRef {
        r: ValueId,
        path: Vec<Proj>,
    },
    /// Write through an exclusive reference.
    StoreRef {
        r: ValueId,
        path: Vec<Proj>,
        v: ValueId,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub struct Inst {
    pub result: Option<ValueId>,
    pub kind: InstKind,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Term {
    Br(BlockId, Vec<ValueId>),
    CondBr {
        c: ValueId,
        t: (BlockId, Vec<ValueId>),
        f: (BlockId, Vec<ValueId>),
    },
    /// Multi-way branch on an integer (or tag) value.
    Switch {
        v: ValueId,
        cases: Vec<(u128, BlockId)>,
        default: BlockId,
    },
    Ret(ValueId),
    /// A materialized runtime check failed or the program panicked.
    Trap(String),
    Unreachable,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Block {
    pub params: Vec<ValueId>,
    pub insts: Vec<Inst>,
    pub term: Term,
}

#[derive(Clone, Debug, PartialEq)]
pub enum FuncKind {
    /// Has a body.
    Defined,
    /// `extern "ABI" fn`: no body; calls go to foreign code.
    Extern(String),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Func {
    pub name: String,
    pub kind: FuncKind,
    pub params: Vec<Type>,
    pub ret: Type,
    /// Stack slots (mutable storage for locals).
    pub slots: Vec<Type>,
    /// Source name of each slot's local ("" for compiler temporaries); used
    /// in diagnostics and drop traces.
    pub slot_names: Vec<String>,
    /// Type of every SSA value, indexed by ValueId.
    pub values: Vec<Type>,
    /// Block 0 is the entry; its parameters are the function parameters.
    pub blocks: Vec<Block>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct StructDef {
    pub name: String,
    pub fields: Vec<(String, Type)>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct EnumDef {
    pub name: String,
    pub variants: Vec<(String, Vec<Type>)>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Module {
    pub structs: Vec<StructDef>,
    pub enums: Vec<EnumDef>,
    pub funcs: Vec<Func>,
}

// ------------------------------------------------------------ printing

impl Module {
    pub fn ty(&self, t: &Type) -> String {
        match t {
            Type::Int(i) => i.name().into(),
            Type::Float(f) => f.name().into(),
            Type::Bool => "bool".into(),
            Type::Char => "char".into(),
            Type::Str => "str".into(),
            Type::Tuple(v) => format!(
                "({})",
                v.iter().map(|x| self.ty(x)).collect::<Vec<_>>().join(", ")
            ),
            Type::Array(e, n) => format!("[{}; {n}]", self.ty(e)),
            Type::Struct(s) => format!("%{}", self.structs[*s as usize].name),
            Type::Enum(e) => format!("%{}", self.enums[*e as usize].name),
            Type::Fn(p, r) => format!(
                "fn({}) -> {}",
                p.iter().map(|x| self.ty(x)).collect::<Vec<_>>().join(", "),
                self.ty(r)
            ),
            Type::Ptr(m, t) => format!("*{} {}", if *m { "var" } else { "const" }, self.ty(t)),
            Type::Region => "region".into(),
            Type::Ref { mutable, ty } => {
                format!("&{}{}", if *mutable { "var " } else { "" }, self.ty(ty))
            }
        }
    }
}

fn vs(v: &[ValueId]) -> String {
    v.iter()
        .map(|x| format!("%{x}"))
        .collect::<Vec<_>>()
        .join(", ")
}

fn path(p: &[Proj]) -> String {
    p.iter()
        .map(|x| match x {
            Proj::Field(i) => format!(".{i}"),
            Proj::Index(v) => format!("[%{v}]"),
        })
        .collect()
}

impl fmt::Display for Module {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for s in &self.structs {
            let fs: Vec<String> = s
                .fields
                .iter()
                .map(|(n, t)| format!("{n}: {}", self.ty(t)))
                .collect();
            writeln!(f, "struct %{} {{ {} }}", s.name, fs.join(", "))?;
        }
        for e in &self.enums {
            let vs: Vec<String> = e
                .variants
                .iter()
                .map(|(n, ts)| {
                    format!(
                        "{n}({})",
                        ts.iter().map(|t| self.ty(t)).collect::<Vec<_>>().join(", ")
                    )
                })
                .collect();
            writeln!(f, "enum %{} {{ {} }}", e.name, vs.join(", "))?;
        }
        for (i, func) in self.funcs.iter().enumerate() {
            writeln!(f)?;
            let ps: Vec<String> = func.params.iter().map(|t| self.ty(t)).collect();
            match &func.kind {
                FuncKind::Extern(abi) => {
                    writeln!(
                        f,
                        "extern \"{abi}\" fn @{} #{i}({}) -> {}",
                        func.name,
                        ps.join(", "),
                        self.ty(&func.ret)
                    )?;
                    continue;
                }
                FuncKind::Defined => writeln!(
                    f,
                    "fn @{} #{i}({}) -> {} {{",
                    func.name,
                    ps.join(", "),
                    self.ty(&func.ret)
                )?,
            }
            for (k, t) in func.slots.iter().enumerate() {
                writeln!(f, "  slot ${k}: {}", self.ty(t))?;
            }
            for (b, blk) in func.blocks.iter().enumerate() {
                let params: Vec<String> = blk
                    .params
                    .iter()
                    .map(|p| format!("%{p}: {}", self.ty(&func.values[*p as usize])))
                    .collect();
                writeln!(f, "bb{b}({}):", params.join(", "))?;
                for inst in &blk.insts {
                    let mut line = String::from("  ");
                    if let Some(r) = inst.result {
                        let _ = write!(line, "%{r} = ");
                    }
                    let _ = write!(
                        line,
                        "{}",
                        self.inst(&inst.kind, inst.result.map(|r| &func.values[r as usize]))
                    );
                    writeln!(f, "{line}")?;
                }
                let t = match &blk.term {
                    Term::Br(b, a) => format!("br bb{b}({})", vs(a)),
                    Term::CondBr { c, t, f: e } => format!(
                        "cond_br %{c}, bb{}({}), bb{}({})",
                        t.0,
                        vs(&t.1),
                        e.0,
                        vs(&e.1)
                    ),
                    Term::Switch { v, cases, default } => {
                        let cs: Vec<String> =
                            cases.iter().map(|(k, b)| format!("{k} => bb{b}")).collect();
                        format!("switch %{v} [{}], default bb{default}", cs.join(", "))
                    }
                    Term::Ret(v) => format!("ret %{v}"),
                    Term::Trap(m) => format!("trap {m:?}"),
                    Term::Unreachable => "unreachable".into(),
                };
                writeln!(f, "  {t}")?;
            }
            writeln!(f, "}}")?;
        }
        Ok(())
    }
}

impl Module {
    fn fname(&self, f: FuncId) -> String {
        self.funcs
            .get(f as usize)
            .map(|x| x.name.clone())
            .unwrap_or_else(|| format!("<invalid #{f}>"))
    }

    fn inst(&self, k: &InstKind, rty: Option<&Type>) -> String {
        let rt = rty.map(|t| self.ty(t)).unwrap_or_default();
        match k {
            InstKind::Const(c) => match c {
                Const::Int(v, t) => format!("const {} {v}", t.name()),
                Const::Float(v, t) => format!("const {} {v:?}", t.name()),
                Const::Bool(b) => format!("const bool {b}"),
                Const::Char(c) => format!("const char {c:?}"),
                Const::Str(s) => format!("const str {s:?}"),
            },
            InstKind::Bin { op, ov, a, b } => {
                let o = format!("{op:?}").to_lowercase();
                let m = match ov {
                    Ov::Trap => "trap",
                    Ov::Wrap => "wrap",
                    Ov::Sat => "sat",
                };
                format!("{o}.{m} {rt} %{a}, %{b}")
            }
            InstKind::Un { op, a } => format!("{} %{a}", format!("{op:?}").to_lowercase()),
            InstKind::Cmp { op, a, b } => {
                format!("cmp.{} %{a}, %{b}", format!("{op:?}").to_lowercase())
            }
            InstKind::Cast { a, to } => format!("cast %{a} to {}", self.ty(to)),
            InstKind::Load { slot, path: p } => format!("load ${slot}{}", path(p)),
            InstKind::Store { slot, path: p, v } => format!("store ${slot}{}, %{v}", path(p)),
            InstKind::Tuple(v) => format!("tuple ({})", vs(v)),
            InstKind::Struct { id, fields } => format!(
                "struct %{} {{{}}}",
                self.structs[*id as usize].name,
                vs(fields)
            ),
            InstKind::Variant {
                id,
                variant,
                fields,
            } => {
                let e = &self.enums[*id as usize];
                format!(
                    "variant %{}.{}({})",
                    e.name,
                    e.variants[*variant as usize].0,
                    vs(fields)
                )
            }
            InstKind::Array(v) => format!("array [{}]", vs(v)),
            InstKind::Repeat { v, n } => format!("repeat [%{v}; {n}]"),
            InstKind::Extract { v, idx } => format!("extract %{v}.{idx}"),
            InstKind::ExtractIndex { v, i } => format!("index %{v}[%{i}]"),
            InstKind::Tag { v } => format!("tag %{v}"),
            InstKind::VariantField { v, variant, idx } => {
                format!("payload %{v} as {variant}.{idx}")
            }
            InstKind::Call { f, args } => {
                format!("call @{}({})", self.funcs[*f as usize].name, vs(args))
            }
            InstKind::CallIndirect { f, args } => format!("call_indirect %{f}({})", vs(args)),
            InstKind::Closure { f, captures } => {
                format!("closure @{} [{}]", self.fname(*f), vs(captures))
            }
            InstKind::Move { slot, path } => format!(
                "move ${slot}{}",
                path.iter().map(|i| format!(".{i}")).collect::<String>()
            ),
            InstKind::Drop { slot } => format!("drop ${slot}"),
            InstKind::Borrow {
                slot,
                path: p,
                mutable,
            } => format!(
                "borrow{} ${slot}{}",
                if *mutable { "_mut" } else { "" },
                path(p)
            ),
            InstKind::BorrowRef {
                r,
                path: p,
                mutable,
            } => format!(
                "borrow{} *%{r}{}",
                if *mutable { "_mut" } else { "" },
                path(p)
            ),
            InstKind::LoadRef { r, path: p } => format!("load *%{r}{}", path(p)),
            InstKind::StoreRef { r, path: p, v } => format!("store *%{r}{}, %{v}", path(p)),
        }
    }
}
