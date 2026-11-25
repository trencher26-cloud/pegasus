//! The reference interpreter: executes PIR and is the semantic oracle that
//! native code is tested against (Phase 0 §10, §12). Every materialized
//! check traps with a reason; nothing is undefined.

use crate::ir::*;
use pegasus_sema::ty::{FloatTy, IntTy};
use std::rc::Rc;

#[derive(Clone, Debug, PartialEq)]
pub enum Val {
    /// Integer bit pattern in the type's width.
    Int(u128, IntTy),
    F32(f32),
    F64(f64),
    Bool(bool),
    Char(char),
    Str(Rc<str>),
    /// Tuple, struct or array.
    Agg(Rc<Vec<Val>>),
    Enum(u32, Rc<Vec<Val>>),
    Closure(FuncId, Rc<Vec<Val>>),
    Region,
    /// A reference to a place in a live frame (see `RefVal`).
    Ref(Rc<RefVal>),
    /// A slot or value that has not been written.
    Undef,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Trap {
    pub message: String,
    /// Function names from the innermost frame outward.
    pub backtrace: Vec<String>,
}

pub struct Limits {
    /// Maximum instructions executed (guards tests against runaway loops).
    pub max_steps: u64,
    /// Maximum call depth; exceeding it is a stack-overflow trap.
    pub max_depth: usize,
}

impl Default for Limits {
    fn default() -> Limits {
        Limits {
            max_steps: 200_000_000,
            max_depth: 100_000,
        }
    }
}

fn mask(t: IntTy) -> u128 {
    if t.bits() == 128 {
        u128::MAX
    } else {
        (1u128 << t.bits()) - 1
    }
}

/// Signed value of a bit pattern.
fn sval(b: u128, t: IntTy) -> i128 {
    let bits = t.bits();
    if bits == 128 {
        b as i128
    } else if (b >> (bits - 1)) & 1 == 1 {
        (b | !mask(t)) as i128
    } else {
        b as i128
    }
}

fn from_i(v: i128, t: IntTy) -> u128 {
    (v as u128) & mask(t)
}

fn fits_i(v: i128, t: IntTy) -> bool {
    v >= t.min() && (v < 0 || (v as u128) <= t.max())
}

/// A place: frame (index in the stack and the frame's unique id, so a
/// reference into a frame that has returned is detected), slot, and a path of
/// field/element indices.
#[derive(Clone, Debug, PartialEq)]
pub struct RefVal {
    frame: usize,
    id: u64,
    slot: SlotId,
    path: Vec<usize>,
}

struct Frame {
    id: u64,
    f: FuncId,
    block: BlockId,
    ip: usize,
    slots: Vec<Val>,
    vals: Vec<Val>,
    /// The caller's value receiving the result.
    ret_to: Option<ValueId>,
}

pub struct Interp<'m> {
    /// Values destroyed by `drop`, in order, as "function:local".
    pub drops: Vec<String>,
    m: &'m Module,
    stack: Vec<Frame>,
    steps: u64,
    limits: Limits,
    next_frame: u64,
}

type R<T> = Result<T, String>;

impl<'m> Interp<'m> {
    pub fn new(m: &'m Module, limits: Limits) -> Interp<'m> {
        Interp {
            drops: Vec::new(),
            m,
            stack: Vec::new(),
            steps: 0,
            limits,
            next_frame: 0,
        }
    }

    fn trap<T>(&self, msg: impl Into<String>) -> Result<T, Trap> {
        Err(Trap {
            message: msg.into(),
            backtrace: {
                // Innermost 16 frames, then a count (a stack overflow has 100,000).
                let mut b: Vec<String> = self
                    .stack
                    .iter()
                    .rev()
                    .take(16)
                    .map(|fr| self.m.funcs[fr.f as usize].name.clone())
                    .collect();
                if self.stack.len() > 16 {
                    b.push(format!("... {} more frames", self.stack.len() - 16));
                }
                b
            },
        })
    }

    fn push(&mut self, f: FuncId, args: Vec<Val>, ret_to: Option<ValueId>) -> Result<(), Trap> {
        let func = &self.m.funcs[f as usize];
        if let FuncKind::Extern(abi) = &func.kind {
            return self.trap(format!(
                "call to extern \"{abi}\" function `{}`: foreign calls need native code (the interpreter cannot call them)",
                func.name
            ));
        }
        if self.stack.len() >= self.limits.max_depth {
            return self.trap("stack overflow (call depth limit reached)");
        }
        let mut vals = vec![Val::Undef; func.values.len()];
        for (p, a) in func.blocks[0].params.iter().zip(args) {
            vals[*p as usize] = a;
        }
        self.next_frame += 1;
        self.stack.push(Frame {
            id: self.next_frame,
            f,
            block: 0,
            ip: 0,
            slots: vec![Val::Undef; func.slots.len()],
            vals,
            ret_to,
        });
        Ok(())
    }

    /// Run function `f` with `args` to completion.
    pub fn call(&mut self, f: FuncId, args: Vec<Val>) -> Result<Val, Trap> {
        self.push(f, args, None)?;
        let base = self.stack.len() - 1;
        loop {
            self.steps += 1;
            if self.steps > self.limits.max_steps {
                return self.trap("step limit reached (possible infinite loop)");
            }
            let fr = self.stack.last().unwrap();
            let func = &self.m.funcs[fr.f as usize];
            let blk = &func.blocks[fr.block as usize];
            if fr.ip < blk.insts.len() {
                let inst = blk.insts[fr.ip].clone();
                self.stack.last_mut().unwrap().ip += 1;
                match self.exec(&inst) {
                    Ok(()) => {}
                    Err(m) => return self.trap(m),
                }
                continue;
            }
            let term = blk.term.clone();
            match term {
                Term::Br(b, args) => self.jump(b, &args),
                Term::CondBr { c, t, f } => {
                    let go = matches!(self.get(c), Val::Bool(true));
                    let (b, a) = if go { t } else { f };
                    self.jump(b, &a);
                }
                Term::Switch { v, cases, default } => {
                    let k = match self.get(v) {
                        Val::Int(b, _) => b,
                        _ => u128::MAX,
                    };
                    let b = cases
                        .iter()
                        .find(|c| c.0 == k)
                        .map(|c| c.1)
                        .unwrap_or(default);
                    self.jump(b, &[]);
                }
                Term::Ret(v) => {
                    let val = self.get(v);
                    let fr = self.stack.pop().unwrap();
                    if self.stack.len() == base {
                        return Ok(val);
                    }
                    if let Some(r) = fr.ret_to {
                        self.stack.last_mut().unwrap().vals[r as usize] = val;
                    }
                }
                Term::Trap(m) => return self.trap(m),
                Term::Unreachable => return self.trap("reached unreachable code (compiler bug)"),
            }
        }
    }

    fn as_ref(&self, r: ValueId) -> R<Rc<RefVal>> {
        match self.get(r) {
            Val::Ref(x) => Ok(x),
            _ => Err("dereference of a non-reference value".into()),
        }
    }

    fn live(&self, r: &RefVal) -> R<()> {
        match self.stack.get(r.frame) {
            Some(fr) if fr.id == r.id => Ok(()),
            _ => Err("dangling reference: the frame it points into has returned".into()),
        }
    }

    /// The value a reference denotes.
    fn deref(&self, r: &RefVal) -> R<Val> {
        self.live(r)?;
        let mut cur = &self.stack[r.frame].slots[r.slot as usize];
        for &k in &r.path {
            match cur {
                Val::Agg(v) if k < v.len() => cur = &v[k],
                _ => return Err("invalid reference path".into()),
            }
        }
        Ok(cur.clone())
    }

    /// Mutable access to the place `r.slot` + `keys` in r's frame.
    fn place_mut(&mut self, r: &RefVal, keys: &[usize]) -> R<&mut Val> {
        self.live(r)?;
        let mut cur = &mut self.stack[r.frame].slots[r.slot as usize];
        for &k in keys {
            match cur {
                Val::Agg(v) if k < v.len() => cur = &mut Rc::make_mut(v)[k],
                _ => return Err("invalid reference path".into()),
            }
        }
        Ok(cur)
    }

    /// Turn a projection path into indices into `base`, bounds-checking
    /// array indices.
    fn resolve(&self, base: &Val, path: &[Proj]) -> R<Vec<usize>> {
        let mut keys = Vec::new();
        let mut cur = base.clone();
        for p in path {
            let Val::Agg(v) = &cur else {
                return Err("invalid projection path".into());
            };
            let k = match p {
                Proj::Field(i) => *i as usize,
                Proj::Index(iv) => self.index(*iv, v.len())?,
            };
            let next = v.get(k).cloned().ok_or("invalid projection path")?;
            keys.push(k);
            cur = next;
        }
        Ok(keys)
    }

    fn moved_msg(&self, slot: SlotId) -> String {
        let fr = self.stack.last().unwrap();
        let n = self.m.funcs[fr.f as usize]
            .slot_names
            .get(slot as usize)
            .cloned()
            .unwrap_or_default();
        format!("use of a moved or uninitialized value `{n}` (a PURM violation the checker should have rejected)")
    }

    fn jump(&mut self, b: BlockId, args: &[ValueId]) {
        let vals: Vec<Val> = args.iter().map(|a| self.get(*a)).collect();
        let fr = self.stack.last_mut().unwrap();
        let params = self.m.funcs[fr.f as usize].blocks[b as usize]
            .params
            .clone();
        for (p, v) in params.iter().zip(vals) {
            fr.vals[*p as usize] = v;
        }
        fr.block = b;
        fr.ip = 0;
    }

    fn get(&self, v: ValueId) -> Val {
        self.stack.last().unwrap().vals[v as usize].clone()
    }

    fn set(&mut self, r: Option<ValueId>, v: Val) {
        if let Some(r) = r {
            self.stack.last_mut().unwrap().vals[r as usize] = v;
        }
    }

    fn index(&self, i: ValueId, len: usize) -> R<usize> {
        match self.get(i) {
            Val::Int(b, _) if (b as usize as u128) == b && (b as usize) < len => Ok(b as usize),
            Val::Int(b, _) => Err(format!(
                "index out of bounds: the length is {len} but the index is {b}"
            )),
            _ => Err("invalid index value".into()),
        }
    }

    fn exec(&mut self, inst: &Inst) -> R<()> {
        let r = inst.result;
        let v = match &inst.kind {
            InstKind::Const(c) => match c {
                Const::Int(b, t) => Val::Int(*b, *t),
                Const::Float(x, FloatTy::F64) => Val::F64(*x),
                Const::Float(x, FloatTy::F32) => Val::F32(*x as f32),
                Const::Float(_, t) => {
                    return Err(format!(
                        "`{}` arithmetic is not implemented in the interpreter yet",
                        t.name()
                    ))
                }
                Const::Bool(b) => Val::Bool(*b),
                Const::Char(c) => Val::Char(*c),
                Const::Str(s) => Val::Str(s.as_str().into()),
            },
            InstKind::Bin { op, ov, a, b } => bin(*op, *ov, self.get(*a), self.get(*b))?,
            InstKind::Un { op, a } => match (op, self.get(*a)) {
                (UnOp::Neg, Val::Int(b, t)) => {
                    let x = sval(b, t);
                    if !t.signed() || x == t.min() {
                        return Err(format!("attempt to negate with overflow (`{}`)", t.name()));
                    }
                    Val::Int(from_i(-x, t), t)
                }
                (UnOp::Neg, Val::F64(x)) => Val::F64(-x),
                (UnOp::Neg, Val::F32(x)) => Val::F32(-x),
                (UnOp::Not, Val::Bool(x)) => Val::Bool(!x),
                (UnOp::BitNot, Val::Int(b, t)) => Val::Int(!b & mask(t), t),
                (o, x) => return Err(format!("invalid operand for {o:?}: {x:?}")),
            },
            InstKind::Cmp { op, a, b } => Val::Bool(cmp(*op, &self.get(*a), &self.get(*b))?),
            InstKind::Cast { a, to } => cast(self.get(*a), to)?,
            InstKind::Borrow { slot, path, .. } => {
                let base = self.stack.last().unwrap().slots[*slot as usize].clone();
                let keys = self.resolve(&base, path)?;
                let top = self.stack.len() - 1;
                Val::Ref(Rc::new(RefVal {
                    frame: top,
                    id: self.stack[top].id,
                    slot: *slot,
                    path: keys,
                }))
            }
            InstKind::BorrowRef { r, path, .. } => {
                let rv = self.as_ref(*r)?;
                let base = self.deref(&rv)?;
                let mut keys = rv.path.clone();
                keys.extend(self.resolve(&base, path)?);
                Val::Ref(Rc::new(RefVal {
                    path: keys,
                    ..(*rv).clone()
                }))
            }
            InstKind::LoadRef { r, path } => {
                let rv = self.as_ref(*r)?;
                let base = self.deref(&rv)?;
                let keys = self.resolve(&base, path)?;
                let mut cur = base;
                for k in keys {
                    cur = match cur {
                        Val::Agg(v) => v[k].clone(),
                        _ => return Err("invalid load path through a reference".into()),
                    };
                }
                if contains_undef(&cur) {
                    return Err("use of a moved value through a reference (a PURM violation the checker should have rejected)".into());
                }
                cur
            }
            InstKind::StoreRef { r, path, v } => {
                let rv = self.as_ref(*r)?;
                let base = self.deref(&rv)?;
                let mut keys = rv.path.clone();
                keys.extend(self.resolve(&base, path)?);
                let val = self.get(*v);
                let target = self.place_mut(&rv, &keys)?;
                *target = val;
                return Ok(());
            }
            InstKind::Move { slot, path } => {
                let msg = self.moved_msg(*slot);
                let fr = self.stack.last_mut().unwrap();
                let mut cur = &mut fr.slots[*slot as usize];
                for i in path {
                    match cur {
                        Val::Agg(v) => cur = &mut Rc::make_mut(v)[*i as usize],
                        _ => return Err(msg),
                    }
                }
                let v = std::mem::replace(cur, Val::Undef);
                if contains_undef(&v) {
                    return Err(msg);
                }
                v
            }
            InstKind::Drop { slot } => {
                let fr = self.stack.last_mut().unwrap();
                let v = std::mem::replace(&mut fr.slots[*slot as usize], Val::Undef);
                if v != Val::Undef {
                    let func = &self.m.funcs[fr.f as usize];
                    let n = func
                        .slot_names
                        .get(*slot as usize)
                        .cloned()
                        .unwrap_or_default();
                    let n = if n.is_empty() { format!("${slot}") } else { n };
                    // A partly moved value: only its remaining parts are destroyed.
                    let partial = if contains_undef(&v) { " (partial)" } else { "" };
                    self.drops.push(format!("{}:{n}{partial}", func.name));
                }
                return Ok(());
            }
            InstKind::Load { slot, path } => {
                let mut cur = self.stack.last().unwrap().slots[*slot as usize].clone();
                if cur == Val::Undef {
                    return Err(self.moved_msg(*slot));
                }
                let whole = path.is_empty();
                for p in path {
                    cur = match (p, &cur) {
                        (Proj::Field(i), Val::Agg(v)) => v[*i as usize].clone(),
                        (Proj::Index(iv), Val::Agg(v)) => {
                            let k = self.index(*iv, v.len())?;
                            v[k].clone()
                        }
                        _ => return Err("invalid load path".into()),
                    };
                }
                if contains_undef(&cur) {
                    let _ = whole;
                    return Err(self.moved_msg(*slot));
                }
                cur
            }
            InstKind::Store { slot, path, v } => {
                let val = self.get(*v);
                let mut idx = Vec::new();
                for p in path {
                    idx.push(match p {
                        Proj::Field(i) => (*i as usize, None),
                        Proj::Index(iv) => (0, Some(*iv)),
                    });
                }
                let resolved: Vec<(usize, Option<ValueId>)> = idx;
                // Resolve dynamic indices (bounds-checked) before mutating.
                let mut keys = Vec::new();
                {
                    let mut cur = &self.stack.last().unwrap().slots[*slot as usize];
                    for (i, dyn_i) in &resolved {
                        let Val::Agg(v) = cur else {
                            return Err("invalid store path".into());
                        };
                        let k = match dyn_i {
                            Some(iv) => self.index(*iv, v.len())?,
                            None => *i,
                        };
                        keys.push(k);
                        cur = &v[k];
                    }
                }
                let fr = self.stack.last_mut().unwrap();
                let mut cur = &mut fr.slots[*slot as usize];
                for k in keys {
                    let Val::Agg(v) = cur else {
                        return Err("invalid store path".into());
                    };
                    cur = &mut Rc::make_mut(v)[k];
                }
                *cur = val;
                return Ok(());
            }
            InstKind::Tuple(vs) | InstKind::Array(vs) | InstKind::Struct { fields: vs, .. } => {
                Val::Agg(Rc::new(vs.iter().map(|x| self.get(*x)).collect()))
            }
            InstKind::Variant {
                variant, fields, ..
            } => Val::Enum(
                *variant,
                Rc::new(fields.iter().map(|x| self.get(*x)).collect()),
            ),
            InstKind::Repeat { v, n } => {
                let x = self.get(*v);
                let n = usize::try_from(*n).map_err(|_| "array too large".to_string())?;
                if n > 1 << 28 {
                    return Err(format!(
                        "array of {n} elements is too large for the interpreter"
                    ));
                }
                Val::Agg(Rc::new(vec![x; n]))
            }
            InstKind::Extract { v, idx } => match self.get(*v) {
                Val::Agg(a) => a[*idx as usize].clone(),
                _ => return Err("extract from a non-aggregate".into()),
            },
            InstKind::ExtractIndex { v, i } => match self.get(*v) {
                Val::Agg(a) => {
                    let k = self.index(*i, a.len())?;
                    a[k].clone()
                }
                _ => return Err("index into a non-array".into()),
            },
            InstKind::Tag { v } => match self.get(*v) {
                Val::Enum(t, _) => Val::Int(t as u128, IntTy::U32),
                _ => return Err("tag of a non-enum".into()),
            },
            InstKind::VariantField { v, variant, idx } => match self.get(*v) {
                Val::Enum(t, f) if t == *variant => f[*idx as usize].clone(),
                _ => return Err("payload of the wrong variant (compiler bug)".into()),
            },
            InstKind::Call { f, args } => {
                let a: Vec<Val> = args.iter().map(|x| self.get(*x)).collect();
                return self.push(*f, a, r).map_err(|t| t.message);
            }
            InstKind::CallIndirect { f, args } => {
                let Val::Closure(fid, env) = self.get(*f) else {
                    return Err("call of a non-function".into());
                };
                let mut a: Vec<Val> = env.as_ref().clone();
                a.extend(args.iter().map(|x| self.get(*x)));
                return self.push(fid, a, r).map_err(|t| t.message);
            }
            InstKind::Closure { f, captures } => {
                Val::Closure(*f, Rc::new(captures.iter().map(|x| self.get(*x)).collect()))
            }
        };
        self.set(r, v);
        Ok(())
    }
}

/// A value with a moved-out part (PURM forbids using it).
fn contains_undef(v: &Val) -> bool {
    match v {
        Val::Undef => true,
        Val::Agg(xs) | Val::Enum(_, xs) => xs.iter().any(contains_undef),
        _ => false,
    }
}

fn int_result(v: i128, t: IntTy, what: &str) -> R<Val> {
    if fits_i(v, t) {
        Ok(Val::Int(from_i(v, t), t))
    } else {
        Err(format!("attempt to {what} with overflow (`{}`)", t.name()))
    }
}

fn uint_result(v: Option<u128>, t: IntTy, what: &str) -> R<Val> {
    match v {
        Some(x) if x <= t.max() => Ok(Val::Int(x, t)),
        _ => Err(format!("attempt to {what} with overflow (`{}`)", t.name())),
    }
}

fn bin(op: BinOp, ov: Ov, a: Val, b: Val) -> R<Val> {
    use BinOp::*;
    // Only + - * have wrapping and saturating forms; floats and bools have
    // no overflow mode. The verifier rejects anything else; refuse here too.
    let mode_ok = match &a {
        Val::Int(..) => ov == Ov::Trap || matches!(op, Add | Sub | Mul),
        _ => ov == Ov::Trap,
    };
    if !mode_ok {
        return Err(format!("invalid PIR: {op:?} has no {ov:?} mode"));
    }
    match (a, b) {
        (Val::Int(x, t), Val::Int(y, ty)) => {
            if matches!(op, Shl | Shr) {
                if y >= t.bits() as u128 {
                    return Err(format!(
                        "attempt to shift by {y}, not less than the bit width of `{}` ({})",
                        t.name(),
                        t.bits()
                    ));
                }
                let s = y as u32;
                return Ok(Val::Int(
                    match op {
                        Shl => (x << s) & mask(t),
                        _ if t.signed() => from_i(sval(x, t) >> s, t),
                        _ => x >> s,
                    },
                    t,
                ));
            }
            if op == Pow {
                let Ok(e) = u32::try_from(y) else {
                    return if x <= 1 && !t.signed() {
                        Ok(Val::Int(x, t))
                    } else {
                        Err(format!(
                            "attempt to exponentiate with overflow (`{}`)",
                            t.name()
                        ))
                    };
                };
                let _ = ty;
                return if t.signed() {
                    match sval(x, t).checked_pow(e) {
                        Some(v) => int_result(v, t, "exponentiate"),
                        None => Err(format!(
                            "attempt to exponentiate with overflow (`{}`)",
                            t.name()
                        )),
                    }
                } else {
                    uint_result(x.checked_pow(e), t, "exponentiate")
                };
            }
            match op {
                And => return Ok(Val::Int(x & y, t)),
                Or => return Ok(Val::Int(x | y, t)),
                Xor => return Ok(Val::Int(x ^ y, t)),
                _ => {}
            }
            if matches!(op, Div | Rem) && y == 0 {
                return Err(if op == Div {
                    "attempt to divide by zero".into()
                } else {
                    "attempt to calculate the remainder with a divisor of zero".into()
                });
            }
            if t.signed() {
                let (p, q) = (sval(x, t), sval(y, t));
                let exact = match op {
                    Add => p.checked_add(q),
                    Sub => p.checked_sub(q),
                    Mul => p.checked_mul(q),
                    Div => p.checked_div(q),
                    // MIN % -1 = 0 (Phase 0 §7).
                    _ => Some(if q == -1 { 0 } else { p % q }),
                };
                let what = verb(op);
                match ov {
                    Ov::Trap => match exact {
                        Some(v) => int_result(v, t, what),
                        None => Err(format!("attempt to {what} with overflow (`{}`)", t.name())),
                    },
                    Ov::Wrap => {
                        let w = match op {
                            Add => p.wrapping_add(q),
                            Sub => p.wrapping_sub(q),
                            _ => p.wrapping_mul(q),
                        };
                        Ok(Val::Int(from_i(w, t), t))
                    }
                    Ov::Sat => {
                        let v = exact
                            .map(|v| v.clamp(t.min(), t.max() as i128))
                            .unwrap_or_else(|| {
                                // i128 overflow: the sign of the true result decides.
                                let neg = match op {
                                    Add => p < 0,
                                    Sub => p < 0,
                                    _ => (p < 0) != (q < 0),
                                };
                                if neg {
                                    t.min()
                                } else {
                                    t.max() as i128
                                }
                            });
                        Ok(Val::Int(from_i(v, t), t))
                    }
                }
            } else {
                let what = verb(op);
                let exact = match op {
                    Add => x.checked_add(y),
                    Sub => x.checked_sub(y),
                    Mul => x.checked_mul(y),
                    Div => Some(x / y),
                    _ => Some(x % y),
                };
                match ov {
                    Ov::Trap => uint_result(exact, t, what),
                    Ov::Wrap => {
                        let w = match op {
                            Add => x.wrapping_add(y),
                            Sub => x.wrapping_sub(y),
                            _ => x.wrapping_mul(y),
                        };
                        Ok(Val::Int(w & mask(t), t))
                    }
                    Ov::Sat => {
                        let v = match (op, exact) {
                            (Sub, None) => 0,
                            (_, Some(v)) => v.min(t.max()),
                            (_, None) => t.max(),
                        };
                        Ok(Val::Int(v, t))
                    }
                }
            }
        }
        (Val::F64(x), Val::F64(y)) => Ok(Val::F64(fbin(op, x, y)?)),
        (Val::F32(x), Val::F32(y)) => Ok(Val::F32(fbin(op, x as f64, y as f64)? as f32)),
        (Val::F64(x), Val::Int(y, t)) if op == Pow => Ok(Val::F64(x.powi(int_exp(y, t)?))),
        (Val::F32(x), Val::Int(y, t)) if op == Pow => Ok(Val::F32(x.powi(int_exp(y, t)?))),
        (Val::Bool(x), Val::Bool(y)) => Ok(Val::Bool(match op {
            And => x & y,
            Or => x | y,
            _ => x ^ y,
        })),
        (a, b) => Err(format!("invalid operands for {op:?}: {a:?}, {b:?}")),
    }
}

fn verb(op: BinOp) -> &'static str {
    match op {
        BinOp::Add => "add",
        BinOp::Sub => "subtract",
        BinOp::Mul => "multiply",
        BinOp::Div => "divide",
        _ => "calculate the remainder",
    }
}

fn int_exp(y: u128, t: IntTy) -> R<i32> {
    let v = if t.signed() { sval(y, t) } else { y as i128 };
    i32::try_from(v).map_err(|_| "float exponent out of range for powi".to_string())
}

fn fbin(op: BinOp, x: f64, y: f64) -> R<f64> {
    Ok(match op {
        BinOp::Add => x + y,
        BinOp::Sub => x - y,
        BinOp::Mul => x * y,
        BinOp::Div => x / y,
        BinOp::Rem => x % y,
        BinOp::Pow => x.powf(y),
        _ => return Err(format!("invalid float operation {op:?}")),
    })
}

fn cmp(op: CmpOp, a: &Val, b: &Val) -> R<bool> {
    use std::cmp::Ordering;
    let ord: Option<Ordering> = match (a, b) {
        (Val::Int(x, t), Val::Int(y, _)) => Some(if t.signed() {
            sval(*x, *t).cmp(&sval(*y, *t))
        } else {
            x.cmp(y)
        }),
        (Val::F64(x), Val::F64(y)) => x.partial_cmp(y),
        (Val::F32(x), Val::F32(y)) => x.partial_cmp(y),
        (Val::Char(x), Val::Char(y)) => Some(x.cmp(y)),
        (Val::Str(x), Val::Str(y)) => Some(x.as_bytes().cmp(y.as_bytes())),
        (Val::Bool(x), Val::Bool(y)) => Some(x.cmp(y)),
        _ => {
            return match op {
                CmpOp::Eq => Ok(a == b),
                CmpOp::Ne => Ok(a != b),
                _ => Err("ordering comparison on aggregates".into()),
            };
        }
    };
    Ok(match (op, ord) {
        (CmpOp::Eq, o) => o == Some(Ordering::Equal),
        (CmpOp::Ne, o) => o != Some(Ordering::Equal),
        (CmpOp::Lt, o) => o == Some(Ordering::Less),
        (CmpOp::Le, o) => matches!(o, Some(Ordering::Less | Ordering::Equal)),
        (CmpOp::Gt, o) => o == Some(Ordering::Greater),
        (CmpOp::Ge, o) => matches!(o, Some(Ordering::Greater | Ordering::Equal)),
    })
}

fn cast(a: Val, to: &Type) -> R<Val> {
    Ok(match (a, to) {
        (Val::Int(b, t), Type::Int(u)) => {
            let v = if t.signed() {
                from_i(sval(b, t), *u)
            } else {
                b & mask(*u)
            };
            Val::Int(v, *u)
        }
        (Val::Int(b, t), Type::Float(f)) => {
            let x = if t.signed() {
                sval(b, t) as f64
            } else {
                b as f64
            };
            match f {
                FloatTy::F64 => Val::F64(x),
                FloatTy::F32 => Val::F32(x as f32),
                _ => {
                    return Err(format!(
                        "`{}` arithmetic is not implemented in the interpreter yet",
                        f.name()
                    ))
                }
            }
        }
        (Val::F32(x), Type::Float(FloatTy::F64)) => Val::F64(x as f64),
        (Val::Bool(b), Type::Int(u)) => Val::Int(b as u128, *u),
        (Val::Char(c), Type::Int(u)) => Val::Int(c as u128, *u),
        (Val::Int(b, _), Type::Char) => Val::Char(char::from_u32(b as u32).ok_or("invalid char")?),
        (v, t) => return Err(format!("unsupported cast of {v:?} to {t:?}")),
    })
}

/// Render a value for display, using the module's type names.
pub fn show(m: &Module, t: &Type, v: &Val) -> String {
    match (t, v) {
        (Type::Int(it), Val::Int(b, _)) => {
            if it.signed() {
                sval(*b, *it).to_string()
            } else {
                b.to_string()
            }
        }
        (_, Val::F64(x)) => format!("{x:?}"),
        (_, Val::F32(x)) => format!("{x:?}"),
        (_, Val::Bool(b)) => b.to_string(),
        (_, Val::Char(c)) => format!("{c:?}"),
        (_, Val::Str(s)) => format!("{s:?}"),
        (Type::Tuple(ts), Val::Agg(vs)) => format!(
            "({})",
            ts.iter()
                .zip(vs.iter())
                .map(|(t, v)| show(m, t, v))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        (Type::Array(et, _), Val::Agg(vs)) => format!(
            "[{}]",
            vs.iter()
                .map(|v| show(m, et, v))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        (Type::Struct(id), Val::Agg(vs)) => {
            let sd = &m.structs[*id as usize];
            let fs: Vec<String> = sd
                .fields
                .iter()
                .zip(vs.iter())
                .map(|((n, t), v)| format!("{n}: {}", show(m, t, v)))
                .collect();
            format!("{} {{ {} }}", sd.name, fs.join(", "))
        }
        (Type::Enum(id), Val::Enum(k, fs)) => {
            let (n, ts) = &m.enums[*id as usize].variants[*k as usize];
            if fs.is_empty() {
                n.clone()
            } else {
                format!(
                    "{n}({})",
                    ts.iter()
                        .zip(fs.iter())
                        .map(|(t, v)| show(m, t, v))
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            }
        }
        (_, Val::Closure(f, _)) => format!("<fn {}>", m.funcs[*f as usize].name),
        (_, v) => format!("{v:?}"),
    }
}
