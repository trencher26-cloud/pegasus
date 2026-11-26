// PIRB writer; byte layout identical to compiler/pir/src/bridge.rs::encode.
#include "pirb_write.hpp"

#include <string>
#include <utility>

namespace pegasus::pir {
namespace {

struct W {
  std::vector<std::uint8_t> b;
  void byte(unsigned x) { b.push_back(static_cast<std::uint8_t>(x)); }
  void u(u64 x) {
    for (;;) {
      std::uint8_t c = x & 0x7f;
      x >>= 7;
      if (!x) {
        b.push_back(c);
        return;
      }
      b.push_back(c | 0x80);
    }
  }
  void str(const std::string &s) {
    u(s.size());
    b.insert(b.end(), s.begin(), s.end());
  }
  void u128(U128 v) {
    for (int i = 0; i < 8; ++i) byte((v.lo >> (8 * i)) & 0xff);
    for (int i = 0; i < 8; ++i) byte((v.hi >> (8 * i)) & 0xff);
  }
  void ids(const std::vector<u32> &v) {
    u(v.size());
    for (auto x : v) u(x);
  }
  void ty(const TypeP &t) {
    byte(t->kind);
    switch (t->kind) {
    case Type::Int: byte(static_cast<unsigned>(t->ity)); break;
    case Type::Float: byte(static_cast<unsigned>(t->fty)); break;
    case Type::Tuple:
      u(t->elems.size());
      for (auto &e : t->elems) ty(e);
      break;
    case Type::Array: ty(t->inner); u(t->len); break;
    case Type::Struct: case Type::Enum: u(t->id); break;
    case Type::Fn:
      u(t->elems.size());
      for (auto &e : t->elems) ty(e);
      ty(t->inner);
      break;
    case Type::Ptr: case Type::Ref: byte(t->mut_); ty(t->inner); break;
    default: break;
    }
  }
  void path(const std::vector<Proj> &p) {
    u(p.size());
    for (auto &x : p) {
      byte(x.index ? 1 : 0);
      u(x.n);
    }
  }
  void edge(const Edge &e) {
    u(e.block);
    ids(e.args);
  }
  void inst(const Inst &i) {
    byte(i.has_result);
    if (i.has_result) u(i.result);
    byte(static_cast<unsigned>(i.op));
    switch (i.op) {
    case Op::Const:
      byte(i.c.kind);
      switch (i.c.kind) {
      case Const::Int: u128(i.c.bits); byte(static_cast<unsigned>(i.c.ity)); break;
      case Const::Float:
        for (int k = 0; k < 8; ++k) byte((i.c.fbits >> (8 * k)) & 0xff);
        byte(static_cast<unsigned>(i.c.fty));
        break;
      case Const::Bool: byte(i.c.b); break;
      case Const::Char: u(i.c.ch); break;
      case Const::Str: str(i.c.s); break;
      }
      break;
    case Op::Bin: byte(static_cast<unsigned>(i.bop)); byte(static_cast<unsigned>(i.ov)); u(i.a); u(i.b); break;
    case Op::Un: byte(static_cast<unsigned>(i.uop)); u(i.a); break;
    case Op::Cmp: byte(static_cast<unsigned>(i.cop)); u(i.a); u(i.b); break;
    case Op::Cast: u(i.a); ty(i.to); break;
    case Op::Load: u(i.a); path(i.path); break;
    case Op::Store: u(i.a); path(i.path); u(i.b); break;
    case Op::Tuple: case Op::Array: ids(i.args); break;
    case Op::Struct: u(i.a); ids(i.args); break;
    case Op::Variant: u(i.a); u(i.b); ids(i.args); break;
    case Op::Repeat: u(i.a); u(i.n); break;
    case Op::Extract: u(i.a); u(i.idx); break;
    case Op::ExtractIndex: u(i.a); u(i.b); break;
    case Op::Tag: u(i.a); break;
    case Op::VariantField: u(i.a); u(i.b); u(i.idx); break;
    case Op::Call: case Op::CallIndirect: case Op::Closure: u(i.a); ids(i.args); break;
    case Op::Move: u(i.a); ids(i.args); break;
    case Op::Drop: u(i.a); break;
    case Op::Borrow: case Op::BorrowRef: u(i.a); path(i.path); byte(i.mut_); break;
    case Op::LoadRef: u(i.a); path(i.path); break;
    case Op::StoreRef: u(i.a); path(i.path); u(i.b); break;
    }
  }
  void term(const Term &t) {
    byte(t.kind);
    switch (t.kind) {
    case Term::Br: edge(t.t); break;
    case Term::CondBr: u(t.v); edge(t.t); edge(t.f); break;
    case Term::Switch:
      u(t.v);
      u(t.cases.size());
      for (auto &c : t.cases) {
        u128(c.first);
        u(c.second);
      }
      u(t.dflt);
      break;
    case Term::Ret: u(t.v); break;
    case Term::Trap: str(t.msg); break;
    case Term::Unreachable: break;
    }
  }
};

}  // namespace

bool encodeUnverified(const Module &m, std::vector<std::uint8_t> &out, std::string &error) {
  out.clear();
  if (m.flags != kFlagUnverified) {
    error = "the C++ PIRB writer only writes unverified files (flags 0, got " + std::to_string(m.flags) +
            "); only the Rust PIR verifier (`pegasus pirb-verify`) may mark PIRB verified";
    return false;
  }
  if (m.minor > kPirbMinor) {
    error = "PIRB minor version " + std::to_string(m.minor) + " is not supported (this writer writes " +
            std::to_string(kPirbMajor) + "." + std::to_string(kPirbMinor) + ")";
    return false;
  }
  W w;
  for (char c : std::string("PIRB")) w.byte(static_cast<unsigned char>(c));
  w.byte(kPirbMajor & 0xff); w.byte(kPirbMajor >> 8);
  w.byte(m.minor & 0xff); w.byte(m.minor >> 8);
  w.u(kFlagUnverified);
  w.u(m.has_entry ? u64(m.entry) + 1 : 0);
  w.u(m.structs.size());
  for (auto &s : m.structs) {
    w.str(s.name);
    w.u(s.fields.size());
    for (auto &f : s.fields) {
      w.str(f.first);
      w.ty(f.second);
    }
  }
  w.u(m.enums.size());
  for (auto &e : m.enums) {
    w.str(e.name);
    w.u(e.variants.size());
    for (auto &v : e.variants) {
      w.str(v.first);
      w.u(v.second.size());
      for (auto &t : v.second) w.ty(t);
    }
  }
  w.u(m.funcs.size());
  for (auto &f : m.funcs) {
    w.str(f.name);
    w.byte(f.defined ? 0 : 1);
    if (!f.defined) w.str(f.abi);
    w.u(f.params.size());
    for (auto &t : f.params) w.ty(t);
    w.ty(f.ret);
    w.u(f.slots.size());
    for (auto &t : f.slots) w.ty(t);
    w.u(f.slot_names.size());
    for (auto &s : f.slot_names) w.str(s);
    w.u(f.values.size());
    for (auto &t : f.values) w.ty(t);
    w.u(f.blocks.size());
    for (auto &b : f.blocks) {
      w.ids(b.params);
      w.u(b.insts.size());
      for (auto &i : b.insts) w.inst(i);
      w.term(b.term);
    }
  }
  u32 c = crc32(w.b.data(), w.b.size());
  for (int k = 0; k < 4; ++k) w.byte((c >> (8 * k)) & 0xff);
  out = std::move(w.b);
  return true;
}

}  // namespace pegasus::pir
