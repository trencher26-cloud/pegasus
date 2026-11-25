//! Semantic types.

use crate::defs::DefId;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum IntTy {
    I8,
    I16,
    I32,
    I64,
    I128,
    Isize,
    U8,
    U16,
    U32,
    U64,
    U128,
    Usize,
}

impl IntTy {
    pub const ALL: [IntTy; 12] = [
        IntTy::I8,
        IntTy::I16,
        IntTy::I32,
        IntTy::I64,
        IntTy::I128,
        IntTy::Isize,
        IntTy::U8,
        IntTy::U16,
        IntTy::U32,
        IntTy::U64,
        IntTy::U128,
        IntTy::Usize,
    ];
    pub fn name(self) -> &'static str {
        match self {
            IntTy::I8 => "i8",
            IntTy::I16 => "i16",
            IntTy::I32 => "i32",
            IntTy::I64 => "i64",
            IntTy::I128 => "i128",
            IntTy::Isize => "isize",
            IntTy::U8 => "u8",
            IntTy::U16 => "u16",
            IntTy::U32 => "u32",
            IntTy::U64 => "u64",
            IntTy::U128 => "u128",
            IntTy::Usize => "usize",
        }
    }
    pub fn from_name(s: &str) -> Option<IntTy> {
        IntTy::ALL.into_iter().find(|t| t.name() == s)
    }
    pub fn signed(self) -> bool {
        matches!(
            self,
            IntTy::I8 | IntTy::I16 | IntTy::I32 | IntTy::I64 | IntTy::I128 | IntTy::Isize
        )
    }
    /// Bit width. Pointer-sized integers are 64 bits on every v1 target
    /// (x86-64, ARM64); see spec/semantics.md, section 4.
    pub fn bits(self) -> u32 {
        match self {
            IntTy::I8 | IntTy::U8 => 8,
            IntTy::I16 | IntTy::U16 => 16,
            IntTy::I32 | IntTy::U32 => 32,
            IntTy::I64 | IntTy::U64 | IntTy::Isize | IntTy::Usize => 64,
            IntTy::I128 | IntTy::U128 => 128,
        }
    }
    pub fn min(self) -> i128 {
        if self.signed() {
            if self.bits() == 128 {
                i128::MIN
            } else {
                -(1i128 << (self.bits() - 1))
            }
        } else {
            0
        }
    }
    /// Maximum value; u128::MAX does not fit i128, so the result is u128.
    pub fn max(self) -> u128 {
        if self.signed() {
            (1u128 << (self.bits() - 1)) - 1
        } else if self.bits() == 128 {
            u128::MAX
        } else {
            (1u128 << self.bits()) - 1
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FloatTy {
    F16,
    Bf16,
    F32,
    F64,
}

impl FloatTy {
    pub fn name(self) -> &'static str {
        match self {
            FloatTy::F16 => "f16",
            FloatTy::Bf16 => "bf16",
            FloatTy::F32 => "f32",
            FloatTy::F64 => "f64",
        }
    }
    pub fn from_name(s: &str) -> Option<FloatTy> {
        [FloatTy::F16, FloatTy::Bf16, FloatTy::F32, FloatTy::F64]
            .into_iter()
            .find(|t| t.name() == s)
    }
}

/// An inference variable.
pub type Var = u32;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Ty {
    Int(IntTy),
    Float(FloatTy),
    Bool,
    Char,
    /// The UTF-8 string view `str`.
    Str,
    /// Tuples; the empty tuple is the unit type `()`.
    Tuple(Vec<Ty>),
    /// `[T; N]`; the length is a `Const`, a const `Param` or a variable.
    Array(Box<Ty>, Box<Ty>),
    /// A struct or enum with its generic arguments (types or constants).
    Adt(DefId, Vec<Ty>),
    /// `fn(A, B) -> R`, also the type of closures in v0.1.
    Fn(Vec<Ty>, Box<Ty>),
    /// Raw pointer `*const T` / `*var T`.
    Ptr(bool, Box<Ty>),
    /// A generic type or const parameter.
    Param(DefId),
    /// A constant generic argument or array length.
    Const(ConstVal),
    /// The handle bound by `region r { ... }` (opaque until PURM, Phase 7).
    Region,
    Infer(Var),
    /// The type of `return`, `break` and `continue`.
    Never,
    /// Produced after an error has been reported; unifies with everything.
    Error,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ConstVal {
    Int(CInt),
    Bool(bool),
}

/// An integer constant in sign-magnitude form: every value of every
/// integer type (`i128::MIN` through `u128::MAX`) is representable.
/// Zero is never negative, so equality is value equality.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct CInt {
    pub neg: bool,
    pub mag: u128,
}

impl CInt {
    pub fn new(neg: bool, mag: u128) -> CInt {
        CInt {
            neg: neg && mag != 0,
            mag,
        }
    }
    pub fn from_u128(v: u128) -> CInt {
        CInt::new(false, v)
    }
    pub fn from_i128(v: i128) -> CInt {
        CInt::new(v < 0, v.unsigned_abs())
    }
    pub fn from_usize(v: usize) -> CInt {
        CInt::from_u128(v as u128)
    }
    /// As an i128, if it fits.
    pub fn to_i128(self) -> Option<i128> {
        if self.neg {
            if self.mag == 1u128 << 127 {
                Some(i128::MIN)
            } else {
                i128::try_from(self.mag).ok().map(|m| -m)
            }
        } else {
            i128::try_from(self.mag).ok()
        }
    }
    /// As an array length or count: non-negative and within `usize`.
    pub fn to_usize(self) -> Option<usize> {
        if self.neg {
            None
        } else {
            usize::try_from(self.mag).ok()
        }
    }
    /// Within the value range of `t`.
    pub fn fits(self, t: IntTy) -> bool {
        if self.neg {
            t.signed() && self.mag <= t.min().unsigned_abs()
        } else {
            self.mag <= t.max()
        }
    }
    pub fn cmp_value(self, o: CInt) -> std::cmp::Ordering {
        use std::cmp::Ordering::*;
        match (self.neg, o.neg) {
            (false, true) => Greater,
            (true, false) => Less,
            (false, false) => self.mag.cmp(&o.mag),
            (true, true) => o.mag.cmp(&self.mag),
        }
    }
}

impl std::fmt::Display for CInt {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.neg {
            write!(f, "-{}", self.mag)
        } else {
            write!(f, "{}", self.mag)
        }
    }
}

impl Ty {
    pub fn unit() -> Ty {
        Ty::Tuple(Vec::new())
    }
    pub fn is_unit(&self) -> bool {
        matches!(self, Ty::Tuple(v) if v.is_empty())
    }
    pub fn usize() -> Ty {
        Ty::Int(IntTy::Usize)
    }
    /// Apply `f` to every directly contained type, rebuilding the type.
    pub fn map(&self, f: &mut impl FnMut(&Ty) -> Ty) -> Ty {
        match self {
            Ty::Tuple(v) => Ty::Tuple(v.iter().map(&mut *f).collect()),
            Ty::Array(t, n) => Ty::Array(Box::new(f(t)), Box::new(f(n))),
            Ty::Adt(d, a) => Ty::Adt(*d, a.iter().map(&mut *f).collect()),
            Ty::Fn(p, r) => Ty::Fn(p.iter().map(&mut *f).collect(), Box::new(f(r))),
            Ty::Ptr(m, t) => Ty::Ptr(*m, Box::new(f(t))),
            other => other.clone(),
        }
    }
    pub fn any(&self, p: &mut impl FnMut(&Ty) -> bool) -> bool {
        if p(self) {
            return true;
        }
        match self {
            Ty::Tuple(v) | Ty::Adt(_, v) => v.iter().any(|t| t.any(p)),
            Ty::Array(t, n) => t.any(p) || n.any(p),
            Ty::Fn(ps, r) => ps.iter().any(|t| t.any(p)) || r.any(p),
            Ty::Ptr(_, t) => t.any(p),
            _ => false,
        }
    }
    /// Replace generic parameters by the given arguments.
    pub fn subst(&self, params: &[DefId], args: &[Ty]) -> Ty {
        if params.is_empty() {
            return self.clone();
        }
        match self {
            Ty::Param(p) => match params.iter().position(|q| q == p) {
                Some(i) => args.get(i).cloned().unwrap_or(Ty::Error),
                None => self.clone(),
            },
            _ => self.map(&mut |t| t.subst(params, args)),
        }
    }
    pub fn has_error(&self) -> bool {
        self.any(&mut |t| matches!(t, Ty::Error))
    }
}
