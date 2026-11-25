//! Typed HIR: the output of type checking and the input to PIR lowering.
//!
//! Every name is resolved (locals by id, items by DefId), every expression
//! carries its final type, method calls are plain calls with the receiver as
//! the first argument, named arguments are reordered to parameter order,
//! struct-literal fields are in declaration order, and `T?` is `Option<T>`.

use crate::defs::DefId;
use crate::lit::Lit;
use crate::ty::Ty;
use pegasus_parser::ast::Span;

pub type LocalId = u32;

#[derive(Clone, Debug)]
pub struct Local {
    pub name: String,
    pub ty: Ty,
    pub mutable: bool,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub struct Body {
    pub def: DefId,
    pub locals: Vec<Local>,
    /// One local per parameter, in order (including `self`).
    pub params: Vec<LocalId>,
    pub requires: Vec<Expr>,
    /// `ensures` predicates; the local named `result` holds the return value.
    pub ensures: Vec<Expr>,
    pub result_local: Option<LocalId>,
    pub value: Expr,
}

#[derive(Clone, Debug)]
pub struct Expr {
    pub span: Span,
    pub ty: Ty,
    pub kind: ExprKind,
}

#[derive(Clone, Debug)]
pub enum ExprKind {
    Lit(Lit),
    Local(LocalId),
    /// A consuming use of a non-Copy local: ownership moves out and the local
    /// is uninitialized afterwards (inserted by the PURM checker).
    MoveLocal(LocalId),
    /// A consuming use of a non-Copy field path of a local (`s.a.b`): only
    /// that part moves (a partial move).
    MovePath {
        local: LocalId,
        path: Vec<u32>,
    },
    /// A function, method or extern function, with its generic arguments.
    FnRef(DefId, Vec<Ty>),
    Const(DefId),
    Call {
        callee: Box<Expr>,
        args: Vec<Expr>,
    },
    /// An enum value: the variant and its payload in declaration order.
    Variant {
        variant: DefId,
        fields: Vec<Expr>,
    },
    /// A struct value; `fields` covers every field not taken from `base`,
    /// as (field index, value).
    Struct {
        def: DefId,
        fields: Vec<(usize, Expr)>,
        base: Option<Box<Expr>>,
    },
    Field {
        recv: Box<Expr>,
        index: usize,
    },
    TupleField {
        recv: Box<Expr>,
        index: usize,
    },
    Index {
        recv: Box<Expr>,
        index: Box<Expr>,
    },
    Unary {
        op: &'static str,
        expr: Box<Expr>,
    },
    Binary {
        op: String,
        lhs: Box<Expr>,
        rhs: Box<Expr>,
    },
    Cast(Box<Expr>),
    Tuple(Vec<Expr>),
    Array(Vec<Expr>),
    Repeat {
        value: Box<Expr>,
        count: u64,
    },
    Block(Block),
    If {
        cond: Box<Expr>,
        then: Block,
        otherwise: Option<Box<Expr>>,
    },
    IfLet {
        pat: Pat,
        value: Box<Expr>,
        then: Block,
        otherwise: Option<Box<Expr>>,
    },
    Match {
        scrutinee: Box<Expr>,
        arms: Vec<Arm>,
    },
    Loop(Block),
    While {
        cond: Box<Expr>,
        body: Block,
    },
    WhileLet {
        pat: Pat,
        value: Box<Expr>,
        body: Block,
    },
    /// `for p in lo..hi` / `lo..=hi` over integers.
    ForRange {
        pat: Pat,
        lo: Box<Expr>,
        hi: Box<Expr>,
        inclusive: bool,
        body: Block,
    },
    /// `for p in array` over a fixed-size array, element by element.
    ForArray {
        pat: Pat,
        array: Box<Expr>,
        body: Block,
    },
    Break(Option<Box<Expr>>),
    Continue,
    Return(Option<Box<Expr>>),
    /// `e?`: `is_result` selects Result (true) or Option (false).
    Try {
        expr: Box<Expr>,
        is_result: bool,
    },
    Closure {
        params: Vec<Pat>,
        body: Box<Expr>,
        captures: Vec<LocalId>,
    },
    Unsafe {
        caps: Vec<String>,
        body: Block,
    },
    Error,
}

#[derive(Clone, Debug)]
pub struct Block {
    pub stmts: Vec<Stmt>,
    pub tail: Option<Box<Expr>>,
}

#[derive(Clone, Debug)]
pub enum Stmt {
    Let {
        pat: Pat,
        init: Expr,
    },
    /// `op` is None for `=`, else the binary operator of `op=`.
    Assign {
        place: Expr,
        op: Option<String>,
        value: Expr,
    },
    Expr(Expr),
    Defer(Expr),
    Region {
        local: LocalId,
        body: Block,
    },
}

#[derive(Clone, Debug)]
pub struct Arm {
    pub pat: Pat,
    pub guard: Option<Expr>,
    pub body: Expr,
}

#[derive(Clone, Debug)]
pub struct Pat {
    pub span: Span,
    pub ty: Ty,
    pub kind: PatKind,
}

#[derive(Clone, Debug)]
pub enum PatKind {
    Wild,
    Binding(LocalId),
    /// A literal; `neg` for a leading minus.
    Lit {
        lit: Lit,
        neg: bool,
    },
    Range {
        lo: Option<(Lit, bool)>,
        hi: Option<(Lit, bool)>,
        inclusive: bool,
    },
    /// A named constant used as a pattern (rule F).
    Const(DefId),
    Tuple(Vec<Pat>),
    /// Fixed-size array: `before`, then (if `rest`) any number, then `after`.
    Array {
        before: Vec<Pat>,
        rest: bool,
        after: Vec<Pat>,
    },
    /// Enum variant with every payload field in declaration order.
    Variant {
        variant: DefId,
        fields: Vec<Pat>,
    },
    /// Struct with every field in declaration order.
    Struct {
        def: DefId,
        fields: Vec<Pat>,
    },
    Or(Vec<Pat>),
}

impl Expr {
    pub fn error(span: Span) -> Expr {
        Expr {
            span,
            ty: Ty::Error,
            kind: ExprKind::Error,
        }
    }
}
