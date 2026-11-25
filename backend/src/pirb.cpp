// PIRB decoder (spec/pir-bridge.md §3) with structural validation (§4).
#include "pirb.hpp"

#include <cstdio>
#include <cstring>

namespace pegasus::pir {

const char *name(IntTy t) {
  static const char *n[] = {"i8", "i16", "i32", "i64", "i128", "isize",
                            "u8", "u16", "u32", "u64", "u128", "usize"};
  return n[static_cast<int>(t)];
}

namespace {


struct Reader {
  const std::uint8_t *b;
  std::size_t n, p = 0;

  [[noreturn]] void fail(const std::string &m) const {
    throw FormatError(m + " (at byte " + std::to_string(p) + ")");
  }
  const std::uint8_t *take(std::size_t k) {
    if (n - p < k) fail("truncated");
    p += k;
    return b + p - k;
  }
  std::uint8_t byte() { return *take(1); }
  bool boolean() {
    auto x = byte();
    if (x > 1) fail("bad boolean byte");
    return x == 1;
  }
  u64 uleb() {
    u64 x = 0;
    for (int shift = 0; shift < 64; shift += 7) {
      auto c = byte();
      u64 bits = c & 0x7f;
      if (shift == 63 && bits > 1) fail("LEB128 value overflows 64 bits");
      x |= bits << shift;
      if (!(c & 0x80)) return x;
    }
    fail("LEB128 value overflows 64 bits");
  }
  u32 id() {
    u64 x = uleb();
    if (x > 0xffffffffu) fail("identifier exceeds 32 bits");
    return static_cast<u32>(x);
  }
  std::size_t len() {
    u64 k = uleb();
    if (k > n - p) fail("length exceeds the remaining input");
    return static_cast<std::size_t>(k);
  }
  std::string str() {
    auto k = len();
    auto d = take(k);
    return std::string(reinterpret_cast<const char *>(d), k);
  }
  U128 u128() {
    auto d = take(16);
    U128 v;
    for (int i = 7; i >= 0; --i) v.lo = (v.lo << 8) | d[i];
    for (int i = 15; i >= 8; --i) v.hi = (v.hi << 8) | d[i];
    return v;
  }
  template <class F> auto vec(F f) {
    auto k = len();
    std::vector<decltype(f())> v;
    v.reserve(k);
    for (std::size_t i = 0; i < k; ++i) v.push_back(f());
    return v;
  }
  std::vector<u32> ids() { return vec([&] { return id(); }); }
  template <class E> E tagged(int count, const char *what) {
    auto k = byte();
    if (k >= count) fail(std::string("bad ") + what);
    return static_cast<E>(k);
  }
  IntTy int_ty() { return tagged<IntTy>(12, "integer type"); }
  FloatTy float_ty() { return tagged<FloatTy>(4, "float type"); }

  TypeP ty(int depth = 0) {
    if (depth > 256) fail("type nesting too deep");
    auto t = std::make_shared<Type>();
    auto k = byte();
    if (k > 12) fail("bad type tag");
    t->kind = static_cast<Type::Kind>(k);
    switch (t->kind) {
    case Type::Int: t->ity = int_ty(); break;
    case Type::Float: t->fty = float_ty(); break;
    case Type::Tuple: t->elems = vec([&] { return ty(depth + 1); }); break;
    case Type::Array: t->inner = ty(depth + 1); t->len = uleb(); break;
    case Type::Struct: case Type::Enum: t->id = id(); break;
    case Type::Fn:
      t->elems = vec([&] { return ty(depth + 1); });
      t->inner = ty(depth + 1);
      break;
    case Type::Ptr: case Type::Ref: t->mut_ = boolean(); t->inner = ty(depth + 1); break;
    default: break;
    }
    return t;
  }
  std::vector<Proj> path() {
    return vec([&] {
      auto k = byte();
      if (k > 1) fail("bad projection tag");
      return Proj{k == 1, id()};
    });
  }
  Inst inst() {
    Inst i;
    i.has_result = boolean();
    if (i.has_result) i.result = id();
    auto k = byte();
    if (k > 24) fail("bad instruction tag");
    i.op = static_cast<Op>(k);
    switch (i.op) {
    case Op::Const: {
      auto c = byte();
      if (c > 4) fail("bad constant tag");
      i.c.kind = static_cast<Const::Kind>(c);
      switch (i.c.kind) {
      case Const::Int: i.c.bits = u128(); i.c.ity = int_ty(); break;
      case Const::Float: {
        auto d = take(8);
        for (int j = 7; j >= 0; --j) i.c.fbits = (i.c.fbits << 8) | d[j];
        i.c.fty = float_ty();
        break;
      }
      case Const::Bool: i.c.b = boolean(); break;
      case Const::Char: {
        u64 ch = uleb();
        if (ch > 0x10ffff || (ch >= 0xd800 && ch <= 0xdfff)) fail("bad char constant");
        i.c.ch = static_cast<u32>(ch);
        break;
      }
      case Const::Str: i.c.s = str(); break;
      }
      break;
    }
    case Op::Bin:
      i.bop = tagged<BinOp>(11, "binary operator");
      i.ov = tagged<Ov>(3, "overflow mode");
      i.a = id(); i.b = id();
      break;
    case Op::Un: i.uop = tagged<UnOp>(3, "unary operator"); i.a = id(); break;
    case Op::Cmp: i.cop = tagged<CmpOp>(6, "comparison"); i.a = id(); i.b = id(); break;
    case Op::Cast: i.a = id(); i.to = ty(); break;
    case Op::Load: i.a = id(); i.path = path(); break;
    case Op::Store: i.a = id(); i.path = path(); i.b = id(); break;
    case Op::Tuple: case Op::Array: i.args = ids(); break;
    case Op::Struct: i.a = id(); i.args = ids(); break;
    case Op::Variant: i.a = id(); i.b = id(); i.args = ids(); break;
    case Op::Repeat: i.a = id(); i.n = uleb(); break;
    case Op::Extract: i.a = id(); i.idx = id(); break;
    case Op::ExtractIndex: i.a = id(); i.b = id(); break;
    case Op::Tag: i.a = id(); break;
    case Op::VariantField: i.a = id(); i.b = id(); i.idx = id(); break;
    case Op::Call: case Op::CallIndirect: case Op::Closure: i.a = id(); i.args = ids(); break;
    case Op::Move: i.a = id(); i.args = ids(); break;
    case Op::Drop: i.a = id(); break;
    case Op::Borrow: case Op::BorrowRef: i.a = id(); i.path = path(); i.mut_ = boolean(); break;
    case Op::LoadRef: i.a = id(); i.path = path(); break;
    case Op::StoreRef: i.a = id(); i.path = path(); i.b = id(); break;
    }
    return i;
  }
  Edge edge() {
    Edge e;
    e.block = id();
    e.args = ids();
    return e;
  }
  Term term() {
    Term t;
    auto k = byte();
    if (k > 5) fail("bad terminator tag");
    t.kind = static_cast<Term::Kind>(k);
    switch (t.kind) {
    case Term::Br: t.t = edge(); break;
    case Term::CondBr: t.v = id(); t.t = edge(); t.f = edge(); break;
    case Term::Switch:
      t.v = id();
      t.cases = vec([&] {
        auto key = u128();
        return std::make_pair(key, id());
      });
      t.dflt = id();
      break;
    case Term::Ret: t.v = id(); break;
    case Term::Trap: t.msg = str(); break;
    case Term::Unreachable: break;
    }
    return t;
  }
};

// ---- structural validation (spec/pir-bridge.md §4)

struct Validator {
  const Module &m;
  std::string where;
  [[noreturn]] void fail(const std::string &msg) const { throw FormatError(where + ": " + msg); }

  void ty(const TypeP &t) const {
    switch (t->kind) {
    case Type::Struct: if (t->id >= m.structs.size()) fail("struct id out of range"); break;
    case Type::Enum: if (t->id >= m.enums.size()) fail("enum id out of range"); break;
    default: break;
    }
    for (auto &e : t->elems) ty(e);
    if (t->inner) ty(t->inner);
  }
  void func(const Func &f) {
    for (auto &t : f.params) ty(t);
    ty(f.ret);
    for (auto &t : f.slots) ty(t);
    for (auto &t : f.values) ty(t);
    if (!f.defined) {
      if (!f.blocks.empty() || !f.slots.empty() || !f.values.empty()) fail("extern function with a body");
      return;
    }
    if (f.blocks.empty()) fail("defined function without blocks");
    if (!f.slot_names.empty() && f.slot_names.size() != f.slots.size()) fail("slot names do not match slots");
    if (f.blocks[0].params.size() != f.params.size()) fail("entry block parameters differ from the parameters");
    std::vector<int> defs(f.values.size(), 0);
    auto val = [&](u32 v) { if (v >= f.values.size()) fail("value %" + std::to_string(v) + " out of range"); };
    auto slot = [&](u32 s) { if (s >= f.slots.size()) fail("slot $" + std::to_string(s) + " out of range"); };
    auto blk = [&](u32 b) { if (b >= f.blocks.size()) fail("block bb" + std::to_string(b) + " out of range"); };
    auto fn = [&](u32 g) { if (g >= m.funcs.size()) fail("function #" + std::to_string(g) + " out of range"); };
    auto path = [&](const std::vector<Proj> &p) { for (auto &x : p) if (x.index) val(x.n); };
    auto def = [&](u32 v) { val(v); if (++defs[v] > 1) fail("value %" + std::to_string(v) + " defined twice"); };
    for (auto &b : f.blocks) {
      for (auto p : b.params) def(p);
      for (auto &i : b.insts) {
        if (i.has_result) def(i.result);
        switch (i.op) {
        case Op::Const: break;
        case Op::Bin: case Op::Cmp: val(i.a); val(i.b); break;
        case Op::Un: case Op::Cast: case Op::Extract: case Op::Tag: case Op::Repeat: case Op::VariantField: val(i.a); break;
        case Op::ExtractIndex: val(i.a); val(i.b); break;
        case Op::Load: case Op::Borrow: slot(i.a); path(i.path); break;
        case Op::Store: slot(i.a); path(i.path); val(i.b); break;
        case Op::Move: case Op::Drop: slot(i.a); break;
        case Op::BorrowRef: case Op::LoadRef: val(i.a); path(i.path); break;
        case Op::StoreRef: val(i.a); path(i.path); val(i.b); break;
        case Op::Tuple: case Op::Array: for (auto v : i.args) val(v); break;
        case Op::Struct: if (i.a >= m.structs.size()) fail("struct id out of range"); for (auto v : i.args) val(v); break;
        case Op::Variant: if (i.a >= m.enums.size()) fail("enum id out of range"); for (auto v : i.args) val(v); break;
        case Op::Call: case Op::Closure: fn(i.a); for (auto v : i.args) val(v); break;
        case Op::CallIndirect: val(i.a); for (auto v : i.args) val(v); break;
        }
      }
      auto edge = [&](const Edge &e) { blk(e.block); for (auto v : e.args) val(v); };
      switch (b.term.kind) {
      case Term::Br: edge(b.term.t); break;
      case Term::CondBr: val(b.term.v); edge(b.term.t); edge(b.term.f); break;
      case Term::Switch: val(b.term.v); for (auto &c : b.term.cases) blk(c.second); blk(b.term.dflt); break;
      case Term::Ret: val(b.term.v); break;
      default: break;
      }
    }
    for (std::size_t v = 0; v < defs.size(); ++v)
      if (!defs[v]) fail("value %" + std::to_string(v) + " never defined");
  }
};

}  // namespace

u32 crc32(const std::uint8_t *d, std::size_t n) {
  u32 c = 0xffffffffu;
  for (std::size_t i = 0; i < n; ++i) {
    c ^= d[i];
    for (int k = 0; k < 8; ++k) c = (c & 1) ? (c >> 1) ^ 0xedb88320u : c >> 1;
  }
  return ~c;
}

Module decode(const std::uint8_t *data, std::size_t len) {
  if (len < 12 || std::memcmp(data, "PIRB", 4) != 0) throw FormatError("not a PIRB file (bad magic)");
  len -= 4;  // trailer: CRC-32 of everything before it (spec/pir-bridge.md §2)
  u32 want = u32(data[len]) | u32(data[len + 1]) << 8 | u32(data[len + 2]) << 16 | u32(data[len + 3]) << 24;
  Reader r{data, len};
  r.take(4);
  // Each byte read is its own statement: the evaluation order of the
  // operands of `|` is unspecified in C++.
  int major = r.byte();
  major |= r.byte() << 8;
  int minor = r.byte();
  minor |= r.byte() << 8;
  if (major != kPirbMajor || minor > kPirbMinor)
    throw FormatError("unsupported PIRB version " + std::to_string(major) + "." + std::to_string(minor));
  if (crc32(data, len) != want) throw FormatError("checksum mismatch (corrupted PIRB file)");
  Module m;
  m.minor = static_cast<std::uint16_t>(minor);
  m.flags = r.uleb();
  u64 e = r.uleb();
  if (e) {
    if (e - 1 > 0xffffffffu) r.fail("entry exceeds 32 bits");
    m.has_entry = true;
    m.entry = static_cast<u32>(e - 1);
  }
  m.structs = r.vec([&] {
    StructDef s;
    s.name = r.str();
    s.fields = r.vec([&] {
      auto n = r.str();
      return std::make_pair(n, r.ty());
    });
    return s;
  });
  m.enums = r.vec([&] {
    EnumDef d;
    d.name = r.str();
    d.variants = r.vec([&] {
      auto n = r.str();
      return std::make_pair(n, r.vec([&] { return r.ty(); }));
    });
    return d;
  });
  m.funcs = r.vec([&] {
    Func f;
    f.name = r.str();
    auto k = r.byte();
    if (k > 1) r.fail("bad function kind");
    f.defined = k == 0;
    if (!f.defined) f.abi = r.str();
    f.params = r.vec([&] { return r.ty(); });
    f.ret = r.ty();
    f.slots = r.vec([&] { return r.ty(); });
    f.slot_names = r.vec([&] { return r.str(); });
    f.values = r.vec([&] { return r.ty(); });
    f.blocks = r.vec([&] {
      Block b;
      b.params = r.ids();
      b.insts = r.vec([&] { return r.inst(); });
      b.term = r.term();
      return b;
    });
    return f;
  });
  if (r.p != len) r.fail("trailing bytes");
  if (!(m.flags & 1)) throw FormatError("module is not marked verified");
  if (m.flags != 1) {
    // spec/pir-bridge.md §2: every bit but bit 0 (verified) is reserved and zero.
    char hex[32];
    std::snprintf(hex, sizeof hex, "%#llx", static_cast<unsigned long long>(m.flags));
    throw FormatError(std::string("reserved PIRB flag bits set (") + hex + "; only bit 0, verified, is defined)");
  }
  for (std::size_t i = 0; i < m.structs.size(); ++i) {
    Validator v{m, "struct #" + std::to_string(i)};
    for (auto &f : m.structs[i].fields) v.ty(f.second);
  }
  for (std::size_t i = 0; i < m.enums.size(); ++i) {
    Validator v{m, "enum #" + std::to_string(i)};
    for (auto &var : m.enums[i].variants)
      for (auto &t : var.second) v.ty(t);
  }
  for (std::size_t i = 0; i < m.funcs.size(); ++i) {
    Validator v{m, "@" + m.funcs[i].name + " #" + std::to_string(i)};
    v.func(m.funcs[i]);
  }
  if (m.has_entry && (m.entry >= m.funcs.size() || !m.funcs[m.entry].defined || !m.funcs[m.entry].params.empty()))
    throw FormatError("entry is not a defined function without parameters");
  return m;
}

}  // namespace pegasus::pir
