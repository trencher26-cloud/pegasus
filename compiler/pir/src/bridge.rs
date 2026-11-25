//! The PIR bridge (spec/pir-bridge.md): the versioned binary serialization
//! of a verified PIR module that crosses the Rust/C++ boundary.
//!
//! `encode` refuses a module that does not verify, so every PIRB file the
//! frontend writes carries verified semantics; the C++ backend consumes it
//! without re-implementing type checking or PURM. `decode` is the reference
//! reader, used to test that the encoding is lossless.

use crate::ir::*;
use crate::verify::verify;
use pegasus_sema::ty::{FloatTy, IntTy};

pub const MAGIC: &[u8; 4] = b"PIRB";
pub const VERSION_MAJOR: u16 = 1;
pub const VERSION_MINOR: u16 = 0;
/// Header flag: the module passed the PIR verifier when it was encoded.
pub const FLAG_VERIFIED: u64 = 1;

const INT_TYS: [IntTy; 12] = [
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
const FLOAT_TYS: [FloatTy; 4] = [FloatTy::F16, FloatTy::Bf16, FloatTy::F32, FloatTy::F64];

/// Encode a verified module. `entry` is the function a native executable
/// starts in (the program's `main`), if any.
pub fn encode(m: &Module, entry: Option<FuncId>) -> Result<Vec<u8>, Vec<String>> {
    verify(m)?;
    if let Some(e) = entry {
        let ok = m
            .funcs
            .get(e as usize)
            .is_some_and(|f| f.kind == FuncKind::Defined && f.params.is_empty());
        if !ok {
            return Err(vec![format!(
                "entry #{e} is not a defined function without parameters"
            )]);
        }
    }
    let mut w = W(Vec::new());
    w.0.extend_from_slice(MAGIC);
    w.0.extend_from_slice(&VERSION_MAJOR.to_le_bytes());
    w.0.extend_from_slice(&VERSION_MINOR.to_le_bytes());
    w.u(FLAG_VERIFIED);
    w.u(entry.map_or(0, |e| e as u64 + 1));
    w.vec(&m.structs, |w, s| {
        w.str(&s.name);
        w.vec(&s.fields, |w, (n, t)| {
            w.str(n);
            w.ty(t);
        });
    });
    w.vec(&m.enums, |w, e| {
        w.str(&e.name);
        w.vec(&e.variants, |w, (n, ts)| {
            w.str(n);
            w.vec(ts, W::ty);
        });
    });
    w.vec(&m.funcs, |w, f| {
        w.str(&f.name);
        match &f.kind {
            FuncKind::Defined => w.b(0),
            FuncKind::Extern(abi) => {
                w.b(1);
                w.str(abi);
            }
        }
        w.vec(&f.params, W::ty);
        w.ty(&f.ret);
        w.vec(&f.slots, W::ty);
        w.vec(&f.slot_names, |w, s| w.str(s));
        w.vec(&f.values, W::ty);
        w.vec(&f.blocks, |w, b| {
            w.ids(&b.params);
            w.vec(&b.insts, W::inst);
            w.term(&b.term);
        });
    });
    let crc = crc32(&w.0);
    w.0.extend_from_slice(&crc.to_le_bytes());
    Ok(w.0)
}

/// CRC-32 (IEEE 802.3, reflected, polynomial 0xEDB88320): the PIRB trailer
/// over every preceding byte. It detects accidental corruption in transit
/// or storage; it is not a security measure.
pub fn crc32(data: &[u8]) -> u32 {
    let mut c = 0xffff_ffffu32;
    for &b in data {
        c ^= b as u32;
        for _ in 0..8 {
            c = if c & 1 != 0 {
                (c >> 1) ^ 0xedb8_8320
            } else {
                c >> 1
            };
        }
    }
    !c
}

struct W(Vec<u8>);

impl W {
    fn b(&mut self, x: u8) {
        self.0.push(x);
    }
    /// Unsigned LEB128.
    fn u(&mut self, mut x: u64) {
        loop {
            let byte = (x & 0x7f) as u8;
            x >>= 7;
            if x == 0 {
                self.0.push(byte);
                return;
            }
            self.0.push(byte | 0x80);
        }
    }
    fn str(&mut self, s: &str) {
        self.u(s.len() as u64);
        self.0.extend_from_slice(s.as_bytes());
    }
    fn vec<T>(&mut self, xs: &[T], mut f: impl FnMut(&mut W, &T)) {
        self.u(xs.len() as u64);
        for x in xs {
            f(self, x);
        }
    }
    fn ids(&mut self, xs: &[u32]) {
        self.vec(xs, |w, x| w.u(*x as u64));
    }
    fn int_ty(&mut self, t: IntTy) {
        self.b(INT_TYS.iter().position(|x| *x == t).unwrap() as u8);
    }
    fn ty(&mut self, t: &Type) {
        match t {
            Type::Int(i) => {
                self.b(0);
                self.int_ty(*i);
            }
            Type::Float(f) => {
                self.b(1);
                self.b(FLOAT_TYS.iter().position(|x| x == f).unwrap() as u8);
            }
            Type::Bool => self.b(2),
            Type::Char => self.b(3),
            Type::Str => self.b(4),
            Type::Tuple(ts) => {
                self.b(5);
                self.vec(ts, W::ty);
            }
            Type::Array(e, n) => {
                self.b(6);
                self.ty(e);
                self.u(*n);
            }
            Type::Struct(s) => {
                self.b(7);
                self.u(*s as u64);
            }
            Type::Enum(e) => {
                self.b(8);
                self.u(*e as u64);
            }
            Type::Fn(p, r) => {
                self.b(9);
                self.vec(p, W::ty);
                self.ty(r);
            }
            Type::Ptr(m, e) => {
                self.b(10);
                self.b(*m as u8);
                self.ty(e);
            }
            Type::Region => self.b(11),
            Type::Ref { mutable, ty } => {
                self.b(12);
                self.b(*mutable as u8);
                self.ty(ty);
            }
        }
    }
    fn path(&mut self, p: &[Proj]) {
        self.vec(p, |w, x| match x {
            Proj::Field(i) => {
                w.b(0);
                w.u(*i as u64);
            }
            Proj::Index(v) => {
                w.b(1);
                w.u(*v as u64);
            }
        });
    }
    fn inst(&mut self, i: &Inst) {
        match i.result {
            Some(r) => {
                self.b(1);
                self.u(r as u64);
            }
            None => self.b(0),
        }
        use InstKind::*;
        match &i.kind {
            Const(c) => {
                self.b(0);
                match c {
                    crate::ir::Const::Int(v, t) => {
                        self.b(0);
                        self.0.extend_from_slice(&v.to_le_bytes());
                        self.int_ty(*t);
                    }
                    crate::ir::Const::Float(x, t) => {
                        self.b(1);
                        self.0.extend_from_slice(&x.to_bits().to_le_bytes());
                        self.b(FLOAT_TYS.iter().position(|y| y == t).unwrap() as u8);
                    }
                    crate::ir::Const::Bool(x) => {
                        self.b(2);
                        self.b(*x as u8);
                    }
                    crate::ir::Const::Char(x) => {
                        self.b(3);
                        self.u(*x as u64);
                    }
                    crate::ir::Const::Str(s) => {
                        self.b(4);
                        self.str(s);
                    }
                }
            }
            Bin { op, ov, a, b } => {
                self.b(1);
                self.b(*op as u8);
                self.b(*ov as u8);
                self.u(*a as u64);
                self.u(*b as u64);
            }
            Un { op, a } => {
                self.b(2);
                self.b(*op as u8);
                self.u(*a as u64);
            }
            Cmp { op, a, b } => {
                self.b(3);
                self.b(*op as u8);
                self.u(*a as u64);
                self.u(*b as u64);
            }
            Cast { a, to } => {
                self.b(4);
                self.u(*a as u64);
                self.ty(to);
            }
            Load { slot, path } => {
                self.b(5);
                self.u(*slot as u64);
                self.path(path);
            }
            Store { slot, path, v } => {
                self.b(6);
                self.u(*slot as u64);
                self.path(path);
                self.u(*v as u64);
            }
            Tuple(vs) => {
                self.b(7);
                self.ids(vs);
            }
            Struct { id, fields } => {
                self.b(8);
                self.u(*id as u64);
                self.ids(fields);
            }
            Variant {
                id,
                variant,
                fields,
            } => {
                self.b(9);
                self.u(*id as u64);
                self.u(*variant as u64);
                self.ids(fields);
            }
            Array(vs) => {
                self.b(10);
                self.ids(vs);
            }
            Repeat { v, n } => {
                self.b(11);
                self.u(*v as u64);
                self.u(*n);
            }
            Extract { v, idx } => {
                self.b(12);
                self.u(*v as u64);
                self.u(*idx as u64);
            }
            ExtractIndex { v, i } => {
                self.b(13);
                self.u(*v as u64);
                self.u(*i as u64);
            }
            Tag { v } => {
                self.b(14);
                self.u(*v as u64);
            }
            VariantField { v, variant, idx } => {
                self.b(15);
                self.u(*v as u64);
                self.u(*variant as u64);
                self.u(*idx as u64);
            }
            Call { f, args } => {
                self.b(16);
                self.u(*f as u64);
                self.ids(args);
            }
            CallIndirect { f, args } => {
                self.b(17);
                self.u(*f as u64);
                self.ids(args);
            }
            Closure { f, captures } => {
                self.b(18);
                self.u(*f as u64);
                self.ids(captures);
            }
            Move { slot, path } => {
                self.b(19);
                self.u(*slot as u64);
                self.ids(path);
            }
            Drop { slot } => {
                self.b(20);
                self.u(*slot as u64);
            }
            Borrow {
                slot,
                path,
                mutable,
            } => {
                self.b(21);
                self.u(*slot as u64);
                self.path(path);
                self.b(*mutable as u8);
            }
            BorrowRef { r, path, mutable } => {
                self.b(22);
                self.u(*r as u64);
                self.path(path);
                self.b(*mutable as u8);
            }
            LoadRef { r, path } => {
                self.b(23);
                self.u(*r as u64);
                self.path(path);
            }
            StoreRef { r, path, v } => {
                self.b(24);
                self.u(*r as u64);
                self.path(path);
                self.u(*v as u64);
            }
        }
    }
    fn edge(&mut self, (b, args): &(BlockId, Vec<ValueId>)) {
        self.u(*b as u64);
        self.ids(args);
    }
    fn term(&mut self, t: &Term) {
        match t {
            Term::Br(b, args) => {
                self.b(0);
                self.edge(&(*b, args.clone()));
            }
            Term::CondBr { c, t, f } => {
                self.b(1);
                self.u(*c as u64);
                self.edge(t);
                self.edge(f);
            }
            Term::Switch { v, cases, default } => {
                self.b(2);
                self.u(*v as u64);
                self.vec(cases, |w, (k, b)| {
                    w.0.extend_from_slice(&k.to_le_bytes());
                    w.u(*b as u64);
                });
                self.u(*default as u64);
            }
            Term::Ret(v) => {
                self.b(3);
                self.u(*v as u64);
            }
            Term::Trap(m) => {
                self.b(4);
                self.str(m);
            }
            Term::Unreachable => self.b(5),
        }
    }
}

// ------------------------------------------------------------ decoding

/// A decoded PIRB file.
#[derive(Debug, PartialEq)]
pub struct Decoded {
    pub minor: u16,
    pub flags: u64,
    pub entry: Option<FuncId>,
    pub module: Module,
}

/// Decode a PIRB file. Fails on a bad magic, an unsupported major version,
/// a checksum mismatch, malformed or truncated data, or trailing bytes. Unknown higher minor
/// versions are rejected too: a minor version may add tags.
pub fn decode(bytes: &[u8]) -> Result<Decoded, String> {
    if bytes.len() < 12 {
        return Err(format!("truncated: {} bytes", bytes.len()));
    }
    let (bytes, trailer) = bytes.split_at(bytes.len() - 4);
    let want = u32::from_le_bytes(trailer.try_into().unwrap());
    let mut r = R { b: bytes, p: 0 };
    if r.take(4)? != MAGIC {
        return Err("not a PIRB file (bad magic)".into());
    }
    let major = u16::from_le_bytes(r.take(2)?.try_into().unwrap());
    let minor = u16::from_le_bytes(r.take(2)?.try_into().unwrap());
    if major != VERSION_MAJOR || minor > VERSION_MINOR {
        return Err(format!(
            "unsupported PIRB version {major}.{minor} (this reader: {VERSION_MAJOR}.{VERSION_MINOR})"
        ));
    }
    if crc32(bytes) != want {
        return Err("checksum mismatch (corrupted PIRB file)".into());
    }
    let flags = r.u()?;
    let entry = match r.u()? {
        0 => None,
        e => Some(r.id_of(e - 1)?),
    };
    let structs = r.vec(|r| {
        Ok(StructDef {
            name: r.str()?,
            fields: r.vec(|r| Ok((r.str()?, r.ty()?)))?,
        })
    })?;
    let enums = r.vec(|r| {
        Ok(EnumDef {
            name: r.str()?,
            variants: r.vec(|r| Ok((r.str()?, r.vec(R::ty)?)))?,
        })
    })?;
    let funcs = r.vec(|r| {
        let name = r.str()?;
        let kind = match r.byte()? {
            0 => FuncKind::Defined,
            1 => FuncKind::Extern(r.str()?),
            k => return Err(format!("bad function kind {k}")),
        };
        Ok(Func {
            name,
            kind,
            params: r.vec(R::ty)?,
            ret: r.ty()?,
            slots: r.vec(R::ty)?,
            slot_names: r.vec(R::str)?,
            values: r.vec(R::ty)?,
            blocks: r.vec(|r| {
                Ok(Block {
                    params: r.ids()?,
                    insts: r.vec(R::inst)?,
                    term: r.term()?,
                })
            })?,
        })
    })?;
    if r.p != bytes.len() {
        return Err(format!("{} trailing bytes", bytes.len() - r.p));
    }
    if flags & FLAG_VERIFIED == 0 {
        return Err("module is not marked verified".into());
    }
    if flags != FLAG_VERIFIED {
        return Err(format!(
            "reserved PIRB flag bits set ({flags:#x}; only bit 0, verified, is defined)"
        ));
    }
    let module = Module {
        structs,
        enums,
        funcs,
    };
    // The reference reader checks more than the structural rules every
    // reader must apply (spec/pir-bridge.md §3): the decoded module must
    // pass the full verifier.
    verify(&module).map_err(|e| format!("decoded module does not verify: {}", e.join("; ")))?;
    if let Some(e) = entry {
        let ok = module
            .funcs
            .get(e as usize)
            .is_some_and(|f| f.kind == FuncKind::Defined && f.params.is_empty());
        if !ok {
            return Err("entry is not a defined function without parameters".into());
        }
    }
    Ok(Decoded {
        minor,
        flags,
        entry,
        module,
    })
}

struct R<'a> {
    b: &'a [u8],
    p: usize,
}

impl R<'_> {
    fn take(&mut self, n: usize) -> Result<&[u8], String> {
        if self.b.len() - self.p < n {
            return Err(format!("truncated at byte {}", self.p));
        }
        self.p += n;
        Ok(&self.b[self.p - n..self.p])
    }
    fn byte(&mut self) -> Result<u8, String> {
        Ok(self.take(1)?[0])
    }
    fn bool(&mut self) -> Result<bool, String> {
        match self.byte()? {
            0 => Ok(false),
            1 => Ok(true),
            x => Err(format!("bad boolean byte {x}")),
        }
    }
    fn u(&mut self) -> Result<u64, String> {
        let mut x = 0u64;
        for shift in (0..64).step_by(7) {
            let byte = self.byte()?;
            let bits = (byte & 0x7f) as u64;
            if shift == 63 && bits > 1 {
                return Err("LEB128 value overflows 64 bits".into());
            }
            x |= bits << shift;
            if byte & 0x80 == 0 {
                return Ok(x);
            }
        }
        Err("LEB128 value overflows 64 bits".into())
    }
    fn id_of(&mut self, x: u64) -> Result<u32, String> {
        u32::try_from(x).map_err(|_| format!("identifier {x} exceeds 32 bits"))
    }
    fn id(&mut self) -> Result<u32, String> {
        let x = self.u()?;
        self.id_of(x)
    }
    fn len(&mut self) -> Result<usize, String> {
        let n = self.u()?;
        // Every element takes at least one byte: a length beyond the
        // remaining input is malformed (and must not drive an allocation).
        if n > (self.b.len() - self.p) as u64 {
            return Err(format!("length {n} exceeds the remaining input"));
        }
        Ok(n as usize)
    }
    fn str(&mut self) -> Result<String, String> {
        let n = self.len()?;
        String::from_utf8(self.take(n)?.to_vec()).map_err(|_| "string is not UTF-8".into())
    }
    fn vec<T>(
        &mut self,
        mut f: impl FnMut(&mut Self) -> Result<T, String>,
    ) -> Result<Vec<T>, String> {
        let n = self.len()?;
        let mut v = Vec::with_capacity(n);
        for _ in 0..n {
            v.push(f(self)?);
        }
        Ok(v)
    }
    fn ids(&mut self) -> Result<Vec<u32>, String> {
        self.vec(R::id)
    }
    fn int_ty(&mut self) -> Result<IntTy, String> {
        let k = self.byte()?;
        INT_TYS
            .get(k as usize)
            .copied()
            .ok_or(format!("bad integer type {k}"))
    }
    fn float_ty(&mut self) -> Result<FloatTy, String> {
        let k = self.byte()?;
        FLOAT_TYS
            .get(k as usize)
            .copied()
            .ok_or(format!("bad float type {k}"))
    }
    fn ty(&mut self) -> Result<Type, String> {
        Ok(match self.byte()? {
            0 => Type::Int(self.int_ty()?),
            1 => Type::Float(self.float_ty()?),
            2 => Type::Bool,
            3 => Type::Char,
            4 => Type::Str,
            5 => Type::Tuple(self.vec(R::ty)?),
            6 => {
                let e = self.ty()?;
                Type::Array(Box::new(e), self.u()?)
            }
            7 => Type::Struct(self.id()?),
            8 => Type::Enum(self.id()?),
            9 => {
                let p = self.vec(R::ty)?;
                Type::Fn(p, Box::new(self.ty()?))
            }
            10 => {
                let m = self.bool()?;
                Type::Ptr(m, Box::new(self.ty()?))
            }
            11 => Type::Region,
            12 => {
                let mutable = self.bool()?;
                Type::Ref {
                    mutable,
                    ty: Box::new(self.ty()?),
                }
            }
            k => return Err(format!("bad type tag {k}")),
        })
    }
    fn u128(&mut self) -> Result<u128, String> {
        Ok(u128::from_le_bytes(self.take(16)?.try_into().unwrap()))
    }
    fn path(&mut self) -> Result<Vec<Proj>, String> {
        self.vec(|r| match r.byte()? {
            0 => Ok(Proj::Field(r.id()?)),
            1 => Ok(Proj::Index(r.id()?)),
            k => Err(format!("bad projection tag {k}")),
        })
    }
    fn tagged<T: Copy>(&mut self, all: &[T], what: &str) -> Result<T, String> {
        let k = self.byte()?;
        all.get(k as usize)
            .copied()
            .ok_or(format!("bad {what} {k}"))
    }
    fn inst(&mut self) -> Result<Inst, String> {
        use BinOp::*;
        let result = if self.bool()? { Some(self.id()?) } else { None };
        let kind = match self.byte()? {
            0 => InstKind::Const(match self.byte()? {
                0 => {
                    let v = self.u128()?;
                    crate::ir::Const::Int(v, self.int_ty()?)
                }
                1 => {
                    let x = f64::from_bits(u64::from_le_bytes(self.take(8)?.try_into().unwrap()));
                    crate::ir::Const::Float(x, self.float_ty()?)
                }
                2 => crate::ir::Const::Bool(self.bool()?),
                3 => {
                    let c = self.u()?;
                    crate::ir::Const::Char(
                        u32::try_from(c)
                            .ok()
                            .and_then(char::from_u32)
                            .ok_or("bad char constant")?,
                    )
                }
                4 => crate::ir::Const::Str(self.str()?),
                k => return Err(format!("bad constant tag {k}")),
            }),
            1 => InstKind::Bin {
                op: self.tagged(
                    &[Add, Sub, Mul, Div, Rem, Pow, Shl, Shr, And, Or, Xor],
                    "binary operator",
                )?,
                ov: self.tagged(&[Ov::Trap, Ov::Wrap, Ov::Sat], "overflow mode")?,
                a: self.id()?,
                b: self.id()?,
            },
            2 => InstKind::Un {
                op: self.tagged(&[UnOp::Neg, UnOp::Not, UnOp::BitNot], "unary operator")?,
                a: self.id()?,
            },
            3 => InstKind::Cmp {
                op: self.tagged(
                    &[
                        CmpOp::Eq,
                        CmpOp::Ne,
                        CmpOp::Lt,
                        CmpOp::Le,
                        CmpOp::Gt,
                        CmpOp::Ge,
                    ],
                    "comparison",
                )?,
                a: self.id()?,
                b: self.id()?,
            },
            4 => InstKind::Cast {
                a: self.id()?,
                to: self.ty()?,
            },
            5 => InstKind::Load {
                slot: self.id()?,
                path: self.path()?,
            },
            6 => InstKind::Store {
                slot: self.id()?,
                path: self.path()?,
                v: self.id()?,
            },
            7 => InstKind::Tuple(self.ids()?),
            8 => InstKind::Struct {
                id: self.id()?,
                fields: self.ids()?,
            },
            9 => InstKind::Variant {
                id: self.id()?,
                variant: self.id()?,
                fields: self.ids()?,
            },
            10 => InstKind::Array(self.ids()?),
            11 => InstKind::Repeat {
                v: self.id()?,
                n: self.u()?,
            },
            12 => InstKind::Extract {
                v: self.id()?,
                idx: self.id()?,
            },
            13 => InstKind::ExtractIndex {
                v: self.id()?,
                i: self.id()?,
            },
            14 => InstKind::Tag { v: self.id()? },
            15 => InstKind::VariantField {
                v: self.id()?,
                variant: self.id()?,
                idx: self.id()?,
            },
            16 => InstKind::Call {
                f: self.id()?,
                args: self.ids()?,
            },
            17 => InstKind::CallIndirect {
                f: self.id()?,
                args: self.ids()?,
            },
            18 => InstKind::Closure {
                f: self.id()?,
                captures: self.ids()?,
            },
            19 => InstKind::Move {
                slot: self.id()?,
                path: self.ids()?,
            },
            20 => InstKind::Drop { slot: self.id()? },
            21 => InstKind::Borrow {
                slot: self.id()?,
                path: self.path()?,
                mutable: self.bool()?,
            },
            22 => InstKind::BorrowRef {
                r: self.id()?,
                path: self.path()?,
                mutable: self.bool()?,
            },
            23 => InstKind::LoadRef {
                r: self.id()?,
                path: self.path()?,
            },
            24 => InstKind::StoreRef {
                r: self.id()?,
                path: self.path()?,
                v: self.id()?,
            },
            k => return Err(format!("bad instruction tag {k}")),
        };
        Ok(Inst { result, kind })
    }
    fn edge(&mut self) -> Result<(BlockId, Vec<ValueId>), String> {
        Ok((self.id()?, self.ids()?))
    }
    fn term(&mut self) -> Result<Term, String> {
        Ok(match self.byte()? {
            0 => {
                let (b, a) = self.edge()?;
                Term::Br(b, a)
            }
            1 => Term::CondBr {
                c: self.id()?,
                t: self.edge()?,
                f: self.edge()?,
            },
            2 => Term::Switch {
                v: self.id()?,
                cases: self.vec(|r| Ok((r.u128()?, r.id()?)))?,
                default: self.id()?,
            },
            3 => Term::Ret(self.id()?),
            4 => Term::Trap(self.str()?),
            5 => Term::Unreachable,
            k => return Err(format!("bad terminator tag {k}")),
        })
    }
}
