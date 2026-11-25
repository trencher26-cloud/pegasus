//! Compile-time evaluation of constant expressions (spec/semantics.md §3.1).
//!
//! Evaluation is typed and width-aware: every integer value carries its
//! type, every intermediate result must fit that type, and the Phase 0
//! integer rules apply (overflow, division by zero, `MIN / -1` and shifts by
//! at least the bit width are errors; `<<` discards shifted-out bits;
//! `MIN % -1` is 0). Supported: integer and boolean literals, constants,
//! const generic parameters, unary and binary operators. Anything else needs
//! the comptime interpreter and is `Unsupported`.

use crate::defs::{DefId, DefKind};
use crate::items::{Sema, TyScope};
use crate::lit::Lit;
use crate::ty::{CInt, ConstVal, IntTy, Ty};
use pegasus_parser::ast::{self, ExprKind};

#[derive(Debug, Clone, PartialEq)]
pub enum ConstErr {
    /// Valid, but beyond this evaluator (reported as U007 where required).
    Unsupported(String),
    /// The expression is wrong (reported as T017).
    Invalid(String),
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum CValue {
    Int(CInt, IntTy),
    Bool(bool),
    /// A const generic parameter: its value is not known until instantiation.
    Param(DefId),
}

impl CValue {
    pub fn to_ty(self) -> Ty {
        match self {
            CValue::Int(v, _) => Ty::Const(ConstVal::Int(v)),
            CValue::Bool(b) => Ty::Const(ConstVal::Bool(b)),
            CValue::Param(p) => Ty::Param(p),
        }
    }
}

fn unsupported<T>() -> Result<T, ConstErr> {
    Err(ConstErr::Unsupported(
        "this constant expression needs comptime evaluation, which is not implemented yet".into(),
    ))
}

fn invalid<T>(m: impl Into<String>) -> Result<T, ConstErr> {
    Err(ConstErr::Invalid(m.into()))
}

fn mask(t: IntTy) -> u128 {
    if t.bits() == 128 {
        u128::MAX
    } else {
        (1u128 << t.bits()) - 1
    }
}

/// Two's-complement bit pattern of `v` in `t` (v must fit `t`).
fn to_bits(v: CInt, t: IntTy) -> u128 {
    if v.neg {
        (!v.mag).wrapping_add(1) & mask(t)
    } else {
        v.mag & mask(t)
    }
}

fn from_bits(b: u128, t: IntTy) -> CInt {
    let b = b & mask(t);
    if t.signed() && (b >> (t.bits() - 1)) & 1 == 1 {
        CInt::new(true, (!b).wrapping_add(1) & mask(t))
    } else {
        CInt::from_u128(b)
    }
}

fn want_int(want: Option<&Ty>) -> Option<IntTy> {
    match want {
        Some(Ty::Int(t)) => Some(*t),
        _ => None,
    }
}

fn check_fit(v: CInt, t: IntTy, what: &str) -> Result<CValue, ConstErr> {
    if v.fits(t) {
        Ok(CValue::Int(v, t))
    } else {
        invalid(format!("{what} overflows `{}` in a constant", t.name()))
    }
}

/// Evaluate `e`; `want` is the expected type (for unsuffixed literals and
/// for checking).
pub fn eval(
    s: &mut Sema,
    e: &ast::Expr,
    scope: &TyScope,
    want: Option<&Ty>,
) -> Result<CValue, ConstErr> {
    let v = eval_inner(s, e, scope, want)?;
    match (v, want) {
        (CValue::Int(_, t), Some(Ty::Int(w))) if t != *w => invalid(format!(
            "expected a `{}` constant, found `{}`",
            w.name(),
            t.name()
        )),
        (CValue::Int(_, t), Some(Ty::Bool)) => {
            invalid(format!("expected a `bool` constant, found `{}`", t.name()))
        }
        (CValue::Bool(_), Some(Ty::Int(w))) => {
            invalid(format!("expected a `{}` constant, found `bool`", w.name()))
        }
        _ => Ok(v),
    }
}

/// Evaluate to a type-level constant (`Ty::Const`) or a const parameter.
pub fn eval_ty(
    s: &mut Sema,
    e: &ast::Expr,
    scope: &TyScope,
    want: Option<&Ty>,
) -> Result<Ty, ConstErr> {
    eval(s, e, scope, want).map(CValue::to_ty)
}

fn eval_inner(
    s: &mut Sema,
    e: &ast::Expr,
    scope: &TyScope,
    want: Option<&Ty>,
) -> Result<CValue, ConstErr> {
    match &e.kind {
        ExprKind::Literal(t) => match crate::lit::parse(t) {
            Some(Lit::Int(v, suf)) => {
                let ty = suf.or(want_int(want)).unwrap_or(IntTy::I32);
                let c = CInt::from_u128(v);
                if !c.fits(ty) {
                    return invalid(format!("literal `{v}` is out of range for `{}`", ty.name()));
                }
                Ok(CValue::Int(c, ty))
            }
            Some(Lit::Bool(b)) => Ok(CValue::Bool(b)),
            _ => unsupported(),
        },
        ExprKind::Name(n) => {
            if let Some(&p) = scope.generics.iter().rev().find(|&&p| s.def(p).name == *n) {
                if s.is_const_param(p) {
                    return Ok(CValue::Param(p));
                }
                return invalid(format!("`{n}` is a type parameter, not a constant"));
            }
            match s.t.lookup(scope.module, n, false) {
                Ok(Some(b)) if s.def(b.def).kind == DefKind::Const => {
                    let ty = s.const_ty(b.def);
                    match (s.const_value(b.def), ty) {
                        (Some(ConstVal::Int(v)), Ty::Int(t)) => Ok(CValue::Int(v, t)),
                        (Some(ConstVal::Bool(v)), _) => Ok(CValue::Bool(v)),
                        _ => unsupported(),
                    }
                }
                Ok(Some(_)) => invalid(format!("`{n}` is not a constant")),
                _ => invalid(format!("cannot find constant `{n}` in this scope")),
            }
        }
        ExprKind::Unary { op: "neg", expr } if matches!(&expr.kind, ExprKind::Literal(_)) => {
            // `-literal` is one negative value: `-128` is a valid `i8`.
            let ExprKind::Literal(t) = &expr.kind else {
                unreachable!()
            };
            match crate::lit::parse(t) {
                Some(Lit::Int(v, suf)) => {
                    let ty = suf.or(want_int(want)).unwrap_or(IntTy::I32);
                    if !ty.signed() {
                        return invalid(format!(
                            "cannot negate a value of unsigned type `{}`",
                            ty.name()
                        ));
                    }
                    let c = CInt::new(true, v);
                    if !c.fits(ty) {
                        return invalid(format!(
                            "literal `-{v}` is out of range for `{}`",
                            ty.name()
                        ));
                    }
                    Ok(CValue::Int(c, ty))
                }
                _ => unsupported(),
            }
        }
        ExprKind::Unary { op, expr } => {
            let v = eval_inner(s, expr, scope, want)?;
            match (*op, v) {
                (_, CValue::Param(_)) => unsupported(),
                ("neg", CValue::Int(x, t)) => {
                    if !t.signed() {
                        return invalid(format!(
                            "cannot negate a value of unsigned type `{}`",
                            t.name()
                        ));
                    }
                    check_fit(CInt::new(!x.neg, x.mag), t, "negation")
                }
                ("pos", CValue::Int(x, t)) => Ok(CValue::Int(x, t)),
                ("bnot", CValue::Int(x, t)) => Ok(CValue::Int(from_bits(!to_bits(x, t), t), t)),
                ("not", CValue::Bool(b)) => Ok(CValue::Bool(!b)),
                _ => invalid(format!(
                    "operator `{op}` cannot be applied in this constant"
                )),
            }
        }
        ExprKind::Binary { op, lhs, rhs } => binary(s, op, lhs, rhs, scope, want),
        ExprKind::Block(b) if b.stmts.len() == 1 => match &b.stmts[0] {
            ast::Stmt::Expr(inner) => eval_inner(s, inner, scope, want),
            _ => unsupported(),
        },
        _ => unsupported(),
    }
}

fn binary(
    s: &mut Sema,
    op: &str,
    lhs: &ast::Expr,
    rhs: &ast::Expr,
    scope: &TyScope,
    want: Option<&Ty>,
) -> Result<CValue, ConstErr> {
    let cmp = matches!(op, "==" | "!=" | "<" | ">" | "<=" | ">=");
    let separate = matches!(op, "<<" | ">>" | "**");
    if op == "&&" || op == "||" {
        let a = eval(s, lhs, scope, Some(&Ty::Bool))?;
        let b = eval(s, rhs, scope, Some(&Ty::Bool))?;
        return match (a, b) {
            (CValue::Bool(x), CValue::Bool(y)) => {
                Ok(CValue::Bool(if op == "&&" { x && y } else { x || y }))
            }
            (CValue::Param(_), _) | (_, CValue::Param(_)) => unsupported(),
            _ => invalid(format!("operator `{op}` needs bool constants")),
        };
    }
    let a = eval_inner(s, lhs, scope, if cmp { None } else { want })?;
    let rhs_want = match (separate, a) {
        (false, CValue::Int(_, t)) => Some(Ty::Int(t)),
        (false, CValue::Bool(_)) => Some(Ty::Bool),
        _ => None,
    };
    let b = eval(s, rhs, scope, rhs_want.as_ref())?;
    let (x, tx, y, ty_b) = match (a, b) {
        (CValue::Param(_), _) | (_, CValue::Param(_)) => return unsupported(),
        (CValue::Bool(x), CValue::Bool(y)) => {
            return match op {
                "==" => Ok(CValue::Bool(x == y)),
                "!=" => Ok(CValue::Bool(x != y)),
                "&" => Ok(CValue::Bool(x & y)),
                "|" => Ok(CValue::Bool(x | y)),
                "^" => Ok(CValue::Bool(x ^ y)),
                _ => invalid(format!(
                    "operator `{op}` cannot be applied to bool constants"
                )),
            }
        }
        (CValue::Int(x, tx), CValue::Int(y, ty)) => (x, tx, y, ty),
        _ => return invalid(format!("operator `{op}` needs operands of the same type")),
    };
    if cmp {
        let o = x.cmp_value(y);
        use std::cmp::Ordering::*;
        return Ok(CValue::Bool(match op {
            "==" => o == Equal,
            "!=" => o != Equal,
            "<" => o == Less,
            ">" => o == Greater,
            "<=" => o != Greater,
            _ => o != Less,
        }));
    }
    let t = tx;
    if separate {
        if ty_b.signed() {
            let what = if op == "**" {
                "exponent"
            } else {
                "shift amount"
            };
            return invalid(format!(
                "the {what} must have an unsigned integer type, found `{}`",
                ty_b.name()
            ));
        }
        if op == "**" {
            let Ok(e) = u32::try_from(y.mag) else {
                return invalid(format!("`**` overflows `{}` in a constant", t.name()));
            };
            return if t.signed() {
                match x.to_i128().and_then(|v| v.checked_pow(e)) {
                    Some(r) => check_fit(CInt::from_i128(r), t, "`**`"),
                    None => invalid(format!("`**` overflows `{}` in a constant", t.name())),
                }
            } else {
                match x.mag.checked_pow(e) {
                    Some(r) => check_fit(CInt::from_u128(r), t, "`**`"),
                    None => invalid(format!("`**` overflows `{}` in a constant", t.name())),
                }
            };
        }
        if y.mag >= t.bits() as u128 {
            return invalid(format!(
                "shift by {} is not less than the bit width of `{}` ({})",
                y.mag,
                t.name(),
                t.bits()
            ));
        }
        let sh = y.mag as u32;
        let bits = to_bits(x, t);
        let r = if op == "<<" {
            from_bits(bits << sh, t)
        } else if t.signed() {
            // Arithmetic shift: value fits i128 for every signed type.
            CInt::from_i128(x.to_i128().unwrap() >> sh)
        } else {
            CInt::from_u128(x.mag >> sh)
        };
        return Ok(CValue::Int(r, t));
    }
    if t != ty_b {
        return invalid(format!(
            "operator `{op}` needs operands of the same type, found `{}` and `{}`",
            t.name(),
            ty_b.name()
        ));
    }
    match op {
        "&" => return Ok(CValue::Int(from_bits(to_bits(x, t) & to_bits(y, t), t), t)),
        "|" => return Ok(CValue::Int(from_bits(to_bits(x, t) | to_bits(y, t), t), t)),
        "^" => return Ok(CValue::Int(from_bits(to_bits(x, t) ^ to_bits(y, t), t), t)),
        _ => {}
    }
    if matches!(op, "/" | "%") && y.mag == 0 {
        return invalid("division by zero in a constant");
    }
    let what = format!("`{op}`");
    if t.signed() {
        let (a, b) = (x.to_i128().unwrap(), y.to_i128().unwrap());
        let r = match op {
            "+" => a.checked_add(b),
            "-" => a.checked_sub(b),
            "*" => a.checked_mul(b),
            "/" => a.checked_div(b),
            // MIN % -1 is 0 (Phase 0 spec, section 7).
            "%" => Some(if b == -1 { 0 } else { a % b }),
            _ => {
                return invalid(format!(
                    "operator `{op}` cannot be applied in this constant"
                ))
            }
        };
        match r {
            Some(r) => check_fit(CInt::from_i128(r), t, &what),
            None => invalid(format!("{what} overflows `{}` in a constant", t.name())),
        }
    } else {
        let (a, b) = (x.mag, y.mag);
        let r = match op {
            "+" => a.checked_add(b),
            "-" => a.checked_sub(b),
            "*" => a.checked_mul(b),
            "/" => Some(a / b),
            "%" => Some(a % b),
            _ => {
                return invalid(format!(
                    "operator `{op}` cannot be applied in this constant"
                ))
            }
        };
        match r {
            Some(r) => check_fit(CInt::from_u128(r), t, &what),
            None => invalid(format!("{what} overflows `{}` in a constant", t.name())),
        }
    }
}

/// Evaluate constant-argument text such as `3`, `-3`, `true` or `N + 1`.
pub fn eval_text(
    s: &mut Sema,
    text: &str,
    scope: &TyScope,
    want: Option<&Ty>,
) -> Result<Ty, ConstErr> {
    let src = format!("const X: i32 = {text}\n");
    let (m, d) = pegasus_parser::parse_source(src.as_bytes());
    if !d.is_empty() {
        return invalid(format!("cannot evaluate constant argument `{text}`"));
    }
    match m.items.first().map(|i| &i.kind) {
        Some(ast::ItemKind::Const { value: Some(v), .. }) => eval_ty(s, v, scope, want),
        _ => invalid(format!("cannot evaluate constant argument `{text}`")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn bits_roundtrip() {
        for t in IntTy::ALL {
            for v in [
                CInt::from_i128(-1),
                CInt::from_u128(0),
                CInt::from_u128(5),
                CInt::from_i128(t.min()),
                CInt::from_u128(t.max()),
            ] {
                if v.fits(t) {
                    assert_eq!(from_bits(to_bits(v, t), t), v, "{t:?} {v}");
                }
            }
        }
        assert_eq!(CInt::from_i128(i128::MIN).to_i128(), Some(i128::MIN));
        assert!(CInt::from_u128(u128::MAX).fits(IntTy::U128));
        assert!(!CInt::from_u128(u128::MAX).fits(IntTy::I128));
    }
}
