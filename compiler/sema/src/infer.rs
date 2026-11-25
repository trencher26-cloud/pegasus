//! Unification-based local type inference (one table per body).

/// Two types cannot be made equal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Mismatch;

use crate::ty::{Ty, Var};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VarKind {
    /// Any type.
    General,
    /// An unsuffixed integer literal: some integer type, default i32.
    Int,
    /// An unsuffixed float literal: some float type, default f64.
    Float,
}

#[derive(Clone, Debug)]
enum Slot {
    Unbound(VarKind),
    Bound(Ty),
}

#[derive(Clone, Debug, Default)]
pub struct InferTable {
    slots: Vec<Slot>,
}

impl InferTable {
    pub fn fresh(&mut self, kind: VarKind) -> Ty {
        self.slots.push(Slot::Unbound(kind));
        Ty::Infer((self.slots.len() - 1) as Var)
    }

    pub fn snapshot(&self) -> InferTable {
        self.clone()
    }

    pub fn rollback(&mut self, s: InferTable) {
        *self = s;
    }

    /// Follow bindings at the top level only.
    pub fn shallow(&self, t: &Ty) -> Ty {
        let mut t = t.clone();
        while let Ty::Infer(v) = t {
            match &self.slots[v as usize] {
                Slot::Bound(b) => t = b.clone(),
                Slot::Unbound(_) => break,
            }
        }
        t
    }

    pub fn kind(&self, v: Var) -> Option<VarKind> {
        match &self.slots[v as usize] {
            Slot::Unbound(k) => Some(*k),
            Slot::Bound(_) => None,
        }
    }

    /// Resolve every variable that is bound; unbound ones stay.
    pub fn resolve(&self, t: &Ty) -> Ty {
        match self.shallow(t) {
            Ty::Infer(v) => Ty::Infer(v),
            other => other.map(&mut |c| self.resolve(c)),
        }
    }

    /// Resolve fully, applying literal defaults (i32, f64). Unconstrained
    /// general variables become `Error`, and `None` is returned in their place
    /// through `unresolved` so the caller can report them.
    pub fn finish(&self, t: &Ty, unresolved: &mut bool) -> Ty {
        match self.shallow(t) {
            Ty::Infer(v) => match self.kind(v) {
                Some(VarKind::Int) => Ty::Int(crate::ty::IntTy::I32),
                Some(VarKind::Float) => Ty::Float(crate::ty::FloatTy::F64),
                _ => {
                    *unresolved = true;
                    Ty::Error
                }
            },
            other => other.map(&mut |c| self.finish(c, unresolved)),
        }
    }

    fn occurs(&self, v: Var, t: &Ty) -> bool {
        let t = self.resolve(t);
        t.any(&mut |x| matches!(x, Ty::Infer(w) if *w == v))
    }

    fn bind(&mut self, v: Var, t: Ty) -> Result<(), Mismatch> {
        if let Ty::Infer(w) = t {
            if w == v {
                return Ok(());
            }
            // Merge kinds: the more specific literal kind survives.
            let kv = self.kind(v).unwrap();
            let kw = self.kind(w).unwrap();
            let merged = match (kv, kw) {
                (VarKind::General, k) | (k, VarKind::General) => k,
                (a, b) if a == b => a,
                _ => return Err(Mismatch),
            };
            self.slots[w as usize] = Slot::Unbound(merged);
            self.slots[v as usize] = Slot::Bound(Ty::Infer(w));
            return Ok(());
        }
        match (self.kind(v).unwrap(), &t) {
            (VarKind::General, _) | (_, Ty::Error) => {}
            (VarKind::Int, Ty::Int(_)) | (VarKind::Float, Ty::Float(_)) => {}
            _ => return Err(Mismatch),
        }
        if self.occurs(v, &t) {
            return Err(Mismatch);
        }
        self.slots[v as usize] = Slot::Bound(t);
        Ok(())
    }

    /// Make two types equal. On failure the table may be partly updated;
    /// callers that probe use `snapshot`/`rollback`.
    pub fn unify(&mut self, a: &Ty, b: &Ty) -> Result<(), Mismatch> {
        let a = self.shallow(a);
        let b = self.shallow(b);
        match (&a, &b) {
            (Ty::Error, _) | (_, Ty::Error) => Ok(()),
            (Ty::Infer(v), _) => self.bind(*v, b.clone()),
            (_, Ty::Infer(v)) => self.bind(*v, a.clone()),
            (Ty::Int(x), Ty::Int(y)) if x == y => Ok(()),
            (Ty::Float(x), Ty::Float(y)) if x == y => Ok(()),
            (Ty::Bool, Ty::Bool)
            | (Ty::Char, Ty::Char)
            | (Ty::Str, Ty::Str)
            | (Ty::Never, Ty::Never)
            | (Ty::Region, Ty::Region) => Ok(()),
            (Ty::Param(x), Ty::Param(y)) if x == y => Ok(()),
            (Ty::Const(x), Ty::Const(y)) if x == y => Ok(()),
            (Ty::Tuple(x), Ty::Tuple(y)) if x.len() == y.len() => {
                for (p, q) in x.iter().zip(y) {
                    self.unify(p, q)?;
                }
                Ok(())
            }
            (Ty::Array(t1, n1), Ty::Array(t2, n2)) => {
                self.unify(t1, t2)?;
                self.unify(n1, n2)
            }
            (Ty::Adt(d1, a1), Ty::Adt(d2, a2)) if d1 == d2 && a1.len() == a2.len() => {
                for (p, q) in a1.iter().zip(a2) {
                    self.unify(p, q)?;
                }
                Ok(())
            }
            (Ty::Fn(p1, r1), Ty::Fn(p2, r2)) if p1.len() == p2.len() => {
                for (p, q) in p1.iter().zip(p2) {
                    self.unify(p, q)?;
                }
                self.unify(r1, r2)
            }
            (Ty::Ptr(m1, t1), Ty::Ptr(m2, t2)) if m1 == m2 => self.unify(t1, t2),
            _ => Err(Mismatch),
        }
    }

    /// Unify without leaving any trace on failure.
    pub fn probe(&mut self, a: &Ty, b: &Ty) -> bool {
        let snap = self.snapshot();
        if self.unify(a, b).is_ok() {
            true
        } else {
            self.rollback(snap);
            false
        }
    }
}
