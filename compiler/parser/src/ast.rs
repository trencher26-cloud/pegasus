//! Syntax tree for grammar v0.1.2-draft. Every node carries a byte span
//! `(start, end)` into the source; names and literals keep their source text.
//!
//! The tree records syntax only. Whether an identifier pattern binds or names a
//! unit variant (rule F), or whether a generic argument names a unit or a type
//! (rule G), is decided later by the resolver.

pub type Span = (u32, u32);

#[derive(Debug, Clone, PartialEq)]
pub struct Module {
    pub items: Vec<Item>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Item {
    pub span: Span,
    pub docs: Vec<Span>,
    pub attrs: Vec<Attribute>,
    pub is_pub: bool,
    pub kind: ItemKind,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Attribute {
    pub span: Span,
    pub path: Vec<String>,
    pub args: Vec<Arg>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ItemKind {
    Use(UseTree),
    Mod {
        name: String,
        items: Option<Vec<Item>>,
    },
    Fn(FnDecl),
    ExternFn {
        abi: String,
        sig: FnSig,
    },
    Struct {
        name: String,
        generics: Vec<GenericParam>,
        where_: Vec<WherePred>,
        fields: Vec<Field>,
    },
    Enum {
        name: String,
        generics: Vec<GenericParam>,
        where_: Vec<WherePred>,
        variants: Vec<Variant>,
    },
    Trait {
        name: String,
        generics: Vec<GenericParam>,
        /// `trait T: A + B`
        supertraits: Vec<Type>,
        where_: Vec<WherePred>,
        items: Vec<Item>,
    },
    Impl {
        generics: Vec<GenericParam>,
        trait_: Option<Type>,
        ty: Type,
        where_: Vec<WherePred>,
        items: Vec<Item>,
    },
    TypeAlias {
        name: String,
        generics: Vec<GenericParam>,
        ty: Type,
        refinement: Option<Expr>,
    },
    Const {
        name: String,
        ty: Type,
        value: Option<Expr>,
    },
    /// An item that failed to parse and was skipped by recovery (8).
    Error,
    /// Associated type declaration in a trait: `type Item: Bound`.
    AssocType {
        name: String,
        bounds: Vec<Type>,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub enum UseTree {
    Path {
        path: Vec<String>,
        alias: Option<String>,
    },
    Group {
        prefix: Vec<String>,
        trees: Vec<UseTree>,
    },
    Glob {
        prefix: Vec<String>,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct FnDecl {
    pub sig: FnSig,
    /// None only for trait methods without a default body.
    pub body: Option<FnBody>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct FnSig {
    pub prefix: Option<String>,
    pub name: String,
    pub generics: Vec<GenericParam>,
    pub params: Vec<Param>,
    pub ret: Option<RetType>,
    pub effects: Option<Vec<String>>,
    pub clauses: Vec<Clause>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum FnBody {
    Block(Block),
    /// `= expression` sugar for a one-expression block.
    Expr(Expr),
}

#[derive(Debug, Clone, PartialEq)]
pub struct RetType {
    pub borrow: bool,
    pub ty: Type,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Param {
    SelfParam {
        mode: Option<String>,
    },
    Named {
        var: bool,
        name: String,
        mode: Option<String>,
        ty: Type,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub enum Clause {
    Requires(Expr),
    Ensures(Expr),
    Where(Vec<WherePred>),
}

/// `type: bound + bound`
#[derive(Debug, Clone, PartialEq)]
pub struct WherePred {
    pub ty: Type,
    pub bounds: Vec<Type>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum GenericParam {
    Type {
        name: String,
        bounds: Vec<Type>,
        default: Option<Type>,
    },
    Const {
        name: String,
        ty: Type,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct Field {
    pub span: Span,
    pub docs: Vec<Span>,
    pub attrs: Vec<Attribute>,
    pub is_pub: bool,
    pub name: String,
    pub ty: Type,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Variant {
    pub span: Span,
    pub docs: Vec<Span>,
    pub attrs: Vec<Attribute>,
    pub name: String,
    pub kind: VariantKind,
}

#[derive(Debug, Clone, PartialEq)]
pub enum VariantKind {
    Unit,
    Tuple(Vec<(Option<String>, Type)>),
    Struct(Vec<Field>),
}

/// A type. `text` is the exact source spelling, used by shape rendering.
#[derive(Debug, Clone, PartialEq)]
pub struct Type {
    pub span: Span,
    pub text: String,
    pub kind: TypeKind,
}

#[derive(Debug, Clone, PartialEq)]
pub enum TypeKind {
    Path {
        path: Vec<String>,
        args: Vec<GenericArg>,
    },
    Tuple(Vec<Type>),
    Paren(Box<Type>),
    Array(Box<Type>, Box<Expr>),
    Fn {
        params: Vec<Type>,
        ret: Option<Box<Type>>,
        effects: Option<Vec<String>>,
    },
    Pointer {
        var: bool,
        ty: Box<Type>,
    },
    Dyn(Vec<Type>),
    Optional(Box<Type>),
}

#[derive(Debug, Clone, PartialEq)]
pub enum GenericArg {
    Type(Type),
    /// A unit expression such as `m/s**2`, kept as source text.
    Unit(String),
    Const(String),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Block {
    pub span: Span,
    pub stmts: Vec<Stmt>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Stmt {
    Let {
        attrs: Vec<Attribute>,
        var: bool,
        pat: Pattern,
        ty: Option<Type>,
        value: Expr,
    },
    Const {
        name: String,
        ty: Type,
        value: Expr,
    },
    Assign {
        target: Expr,
        op: String,
        value: Expr,
    },
    Defer(Expr),
    Region {
        name: String,
        body: Block,
    },
    Expr(Expr),
    /// A statement that failed to parse and was skipped by recovery (8).
    Error(Span),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Pattern {
    pub span: Span,
    pub kind: PatternKind,
}

#[derive(Debug, Clone, PartialEq)]
pub enum PatternKind {
    Wildcard,
    Rest,
    /// A lone name: binding or unit variant/constant, decided by the resolver.
    Ident {
        var: bool,
        name: String,
    },
    Literal(String),
    Range {
        lo: Option<String>,
        hi: Option<String>,
        inclusive: bool,
    },
    Tuple(Vec<Pattern>),
    Array(Vec<Pattern>),
    Variant {
        path: Vec<String>,
        elems: Vec<Pattern>,
    },
    Struct {
        path: Vec<String>,
        fields: Vec<(String, Option<Pattern>)>,
        rest: bool,
    },
    Or(Vec<Pattern>),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Arg {
    pub name: Option<String>,
    /// `&` (exclusive borrow) or `take` (move).
    pub marker: Option<&'static str>,
    pub value: Expr,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Expr {
    pub span: Span,
    pub kind: ExprKind,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ExprKind {
    Name(String),
    SelfValue(String),
    Literal(String),
    /// STRING_LIT with interpolation holes: (expression, format spec).
    Interp {
        text: String,
        holes: Vec<(Expr, Option<String>)>,
    },
    /// A path segment with generic arguments, e.g. `Vec<i32>` in `Vec<i32>.new()`.
    GenericName {
        name: String,
        args: Vec<GenericArg>,
    },
    Field {
        recv: Box<Expr>,
        name: String,
        args: Option<Vec<GenericArg>>,
    },
    TupleField {
        recv: Box<Expr>,
        index: String,
    },
    Call {
        callee: Box<Expr>,
        args: Vec<Arg>,
    },
    Index {
        recv: Box<Expr>,
        index: Box<Expr>,
    },
    Try(Box<Expr>),
    Await(Box<Expr>),
    Unary {
        op: &'static str,
        expr: Box<Expr>,
    },
    Binary {
        op: String,
        lhs: Box<Expr>,
        rhs: Box<Expr>,
    },
    Cast {
        expr: Box<Expr>,
        ty: Type,
    },
    Range {
        lo: Option<Box<Expr>>,
        hi: Option<Box<Expr>>,
        inclusive: bool,
    },
    Closure {
        is_async: bool,
        params: Vec<(Pattern, Option<Type>)>,
        body: Box<Expr>,
    },
    Struct {
        path: Box<Expr>,
        fields: Vec<(String, Option<Expr>)>,
        base: Option<Box<Expr>>,
    },
    Tuple(Vec<Expr>),
    Unit,
    Array(Vec<Expr>),
    Repeat {
        value: Box<Expr>,
        count: Box<Expr>,
    },
    Block(Block),
    Comptime(Block),
    Unsafe {
        caps: Vec<Vec<String>>,
        body: Block,
    },
    If {
        cond: Box<Cond>,
        then: Block,
        otherwise: Option<Box<Expr>>,
    },
    Match {
        scrutinee: Box<Expr>,
        arms: Vec<MatchArm>,
    },
    While {
        cond: Box<Cond>,
        body: Block,
    },
    Loop(Block),
    For {
        pat: Pattern,
        iter: Box<Expr>,
        with: Vec<(String, Vec<Arg>)>,
        body: Block,
    },
    Return(Option<Box<Expr>>),
    Break(Option<Box<Expr>>),
    Continue,
    /// Placeholder for an ERROR token or a recovered parse error.
    Error,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Cond {
    Expr(Expr),
    Let { pat: Pattern, value: Expr },
}

#[derive(Debug, Clone, PartialEq)]
pub struct MatchArm {
    pub pat: Pattern,
    pub guard: Option<Expr>,
    pub body: Expr,
}
