// PIR -> textual LLVM IR (spec/native.md). The emitted code reproduces the
// reference interpreter's observable behaviour for the native subset:
// integer semantics with trap / wrap / saturate modes, materialized traps
// with the interpreter's messages, places and references, and the result
// printed in the interpreter's format.
#include "emit_llvm.hpp"

#include <map>
#include <set>
#include <sstream>
#include <vector>

namespace pegasus::native {
using namespace pir;

namespace {

// ---- 128-bit helpers (portable: two 64-bit halves)

U128 sext(U128 v, unsigned w) {
  if (w >= 128) return v;
  if (w == 64) return {v.lo, (v.lo >> 63) ? ~0ull : 0};
  u64 m = (1ull << w) - 1;
  u64 lo = v.lo & m;
  bool neg = (lo >> (w - 1)) & 1;
  return {neg ? (lo | ~m) : lo, neg ? ~0ull : 0};
}

// Decimal text of an unsigned 128-bit value.
std::string udec(U128 v) {
  if (v.lo == 0 && v.hi == 0) return "0";
  std::string s;
  while (v.lo || v.hi) {
    // Divide by 10 using 32-bit limbs.
    std::uint32_t limb[4] = {std::uint32_t(v.hi >> 32), std::uint32_t(v.hi), std::uint32_t(v.lo >> 32),
                             std::uint32_t(v.lo)};
    u64 rem = 0;
    for (auto &l : limb) {
      u64 cur = (rem << 32) | l;
      l = std::uint32_t(cur / 10);
      rem = cur % 10;
    }
    v.hi = (u64(limb[0]) << 32) | limb[1];
    v.lo = (u64(limb[2]) << 32) | limb[3];
    s.push_back(char('0' + rem));
  }
  return std::string(s.rbegin(), s.rend());
}

// LLVM literal for a bit pattern in an integer of width w: the signed value.
std::string lit(U128 v, unsigned w) {
  U128 s = sext(v, w);
  if (s.hi >> 63) {
    U128 neg{~s.lo + 1, ~s.hi + (s.lo == 0 ? 1 : 0)};
    return "-" + udec(neg);
  }
  return udec(s);
}

std::string escape_bytes(const std::string &s) {
  static const char *hex = "0123456789ABCDEF";
  std::string o;
  for (unsigned char c : s) {
    if (c >= 0x20 && c < 0x7f && c != '"' && c != '\\') {
      o.push_back(char(c));
    } else {
      o.push_back('\\');
      o.push_back(hex[c >> 4]);
      o.push_back(hex[c & 15]);
    }
  }
  return o;
}

const char *verb(BinOp op) {
  switch (op) {
  case BinOp::Add: return "add";
  case BinOp::Sub: return "subtract";
  case BinOp::Mul: return "multiply";
  case BinOp::Div: return "divide";
  default: return "calculate the remainder";
  }
}

// The call-depth limit (the reference interpreter's `Limits::max_depth`).
constexpr unsigned long kMaxDepth = 100000;

class Emitter {
public:
  explicit Emitter(const Module &m) : m_(m) {}

  std::string run() {
    if (!m_.has_entry) throw Unsupported("the module has no entry function");
    reach(m_.entry);
    std::string funcs;
    for (u32 f : order_) funcs += func(f);
    std::string wrapper = main_wrapper();
    std::ostringstream o;
    o << "; Pegasus native backend: PIRB " << 1 << "." << m_.minor << " -> LLVM IR (llvmorg-18.1.3)\n";
    o << "; ModuleID = 'pegasus'\n\n";
    o << "declare void @pg_rt_trap(ptr) noreturn nounwind\n";
    o << "declare void @pg_rt_trap_shift(i64, i64, ptr, i32) noreturn nounwind\n";
    o << "declare void @pg_rt_trap_index(i64, i64, i64) noreturn nounwind\n";
    o << "declare void @pg_rt_print_int(i64, i64, i32) nounwind\n";
    o << "declare void @pg_rt_print_str(ptr) nounwind\n";
    o << "declare void @pg_rt_run(ptr) nounwind\n";
    o << "@pg_rt_depth = external global i64\n";
    for (auto &d : decls_) o << d << "\n";
    o << "\n" << globals_ << "\n" << helpers_ << funcs << wrapper;
    return o.str();
  }

private:
  const Module &m_;
  std::vector<u32> order_;
  std::set<u32> seen_;
  std::set<std::string> decls_;
  std::map<std::string, std::string> strs_;
  std::string globals_, helpers_;
  std::set<std::string> helper_names_;

  // ---- reachability (only what the entry needs is translated)
  void reach(u32 entry) {
    std::vector<u32> work{entry};
    while (!work.empty()) {
      u32 f = work.back();
      work.pop_back();
      if (!seen_.insert(f).second) continue;
      order_.push_back(f);
      const Func &fn = m_.funcs.at(f);
      if (!fn.defined) throw Unsupported("call of extern function `" + fn.name + "`");
      for (auto &b : fn.blocks)
        for (auto &i : b.insts)
          if (i.op == Op::Call || i.op == Op::Closure) work.push_back(i.a);
    }
  }

  // The emitter relies on the verifier's typing; a file that claims to be
  // verified but is not (corruption the checksum missed, or a forged
  // file) must fail closed rather than crash.
  [[noreturn]] static void inconsistent(const std::string &what) {
    throw FormatError("PIR inconsistent with its verification: " + what);
  }
  static const TypeP &array(const TypeP &t) {
    if (t->kind != Type::Array) inconsistent("expected an array");
    return t;
  }

  // ---- names, types, strings
  std::string fname(u32 f) const {
    return "@\"pg." + std::to_string(f) + "." + escape_bytes(m_.funcs[f].name) + "\"";
  }

  std::string ty(const TypeP &t, int depth = 0) const {
    if (depth > 64) throw Unsupported("type nesting too deep");
    switch (t->kind) {
    case Type::Int: return "i" + std::to_string(bits(t->ity));
    case Type::Bool: return "i1";
    case Type::Tuple: {
      std::string s = "{";
      for (std::size_t i = 0; i < t->elems.size(); ++i) s += (i ? ", " : " ") + ty(t->elems[i], depth + 1);
      return s + (t->elems.empty() ? "}" : " }");
    }
    case Type::Struct: {
      auto &sd = m_.structs[t->id];
      std::string s = "{";
      for (std::size_t i = 0; i < sd.fields.size(); ++i)
        s += (i ? ", " : " ") + ty(sd.fields[i].second, depth + 1);
      return s + (sd.fields.empty() ? "}" : " }");
    }
    case Type::Array: return "[" + std::to_string(t->len) + " x " + ty(t->inner, depth + 1) + "]";
    case Type::Ref: ty(t->inner, depth + 1); return "ptr";
    case Type::Float: throw Unsupported("floating-point types");
    case Type::Char: throw Unsupported("`char`");
    case Type::Str: throw Unsupported("`str`");
    case Type::Enum: throw Unsupported("enum `" + m_.enums[t->id].name + "`");
    case Type::Fn: throw Unsupported("function values");
    case Type::Ptr: throw Unsupported("raw pointers");
    case Type::Region: throw Unsupported("regions");
    }
    throw Unsupported("type");
  }

  // Field types of a tuple or struct.
  std::vector<TypeP> fields(const TypeP &t) const {
    if (t->kind == Type::Tuple) return t->elems;
    std::vector<TypeP> v;
    for (auto &f : m_.structs[t->id].fields) v.push_back(f.second);
    return v;
  }

  std::string str(const std::string &s) {
    auto it = strs_.find(s);
    if (it != strs_.end()) return it->second;
    std::string g = "@.s." + std::to_string(strs_.size());
    globals_ += g + " = private unnamed_addr constant [" + std::to_string(s.size() + 1) + " x i8] c\"" +
                escape_bytes(s) + "\\00\"\n";
    strs_[s] = g;
    return g;
  }

  // ---- per-function state
  struct Fx {
    const Func *f = nullptr;
    std::vector<std::string> op;  // operand text of every value
    std::string allocas;
    std::string cur;  // current block body
    int label = 0, tmp = 0;
  };
  Fx *x_ = nullptr;

  std::string fresh() { return "%t" + std::to_string(x_->tmp++); }
  std::string label() { return "L" + std::to_string(x_->label++); }
  void emit(const std::string &s) { x_->cur += "  " + s + "\n"; }
  void start(const std::string &l) { x_->cur += l + ":\n"; }
  const TypeP &vty(u32 v) const { return x_->f->values.at(v); }
  const std::string &opnd(u32 v) const { return x_->op.at(v); }
  std::string typed(u32 v) const { return ty(vty(v)) + " " + opnd(v); }

  // Branch to a trap unless `ok` holds; the trap block runs `trap_call`.
  void check(const std::string &ok, const std::string &trap_call) {
    auto t = label(), k = label();
    emit("br i1 " + ok + ", label %" + k + ", label %" + t);
    start(t);
    emit(trap_call);
    emit("unreachable");
    start(k);
  }
  std::string trap_call(const std::string &msg) { return "call void @pg_rt_trap(ptr " + str(msg) + ")"; }

  // Split an integer (zero- or sign-extended to i128) into i64 halves.
  std::pair<std::string, std::string> halves(const std::string &v, unsigned w, bool sign) {
    std::string wide = v;
    if (w < 128) {
      wide = fresh();
      emit(wide + " = " + (sign ? "sext" : "zext") + " i" + std::to_string(w) + " " + v + " to i128");
    }
    auto lo = fresh(), sh = fresh(), hi = fresh();
    emit(lo + " = trunc i128 " + wide + " to i64");
    emit(sh + " = lshr i128 " + wide + ", 64");
    emit(hi + " = trunc i128 " + sh + " to i64");
    return {lo, hi};
  }

  // An integer value as i64 for indexing, after a bounds check against len.
  std::string index(u32 iv, u64 len) {
    const TypeP &it = vty(iv);
    if (it->kind != Type::Int) throw Unsupported("non-integer index");
    unsigned w = bits(it->ity);
    std::string i128 = opnd(iv);
    if (w < 128) {
      i128 = fresh();
      emit(i128 + " = zext i" + std::to_string(w) + " " + opnd(iv) + " to i128");
    }
    auto ok = fresh();
    emit(ok + " = icmp ult i128 " + i128 + ", " + std::to_string(len));
    auto t = label(), k = label();
    emit("br i1 " + ok + ", label %" + k + ", label %" + t);
    start(t);
    auto [lo, hi] = halves(i128, 128, false);
    emit("call void @pg_rt_trap_index(i64 " + std::to_string(len) + ", i64 " + lo + ", i64 " + hi + ")");
    emit("unreachable");
    start(k);
    auto r = fresh();
    emit(r + " = trunc i128 " + i128 + " to i64");
    return r;
  }

  // Pointer to the place `base.path` (base points to a value of type t).
  std::pair<std::string, TypeP> place(std::string base, TypeP t, const std::vector<Proj> &path) {
    for (auto &p : path) {
      auto r = fresh();
      if (!p.index) {
        if (t->kind != Type::Tuple && t->kind != Type::Struct) throw Unsupported("field of a non-aggregate");
        auto fs = fields(t);
        emit(r + " = getelementptr inbounds " + ty(t) + ", ptr " + base + ", i32 0, i32 " + std::to_string(p.n));
        t = fs.at(p.n);
      } else {
        if (t->kind != Type::Array) throw Unsupported("index of a non-array");
        auto i = index(p.n, t->len);
        emit(r + " = getelementptr inbounds " + ty(t) + ", ptr " + base + ", i64 0, i64 " + i);
        t = t->inner;
      }
      base = r;
    }
    return {base, t};
  }
  std::string slot(u32 s) const { return "%s" + std::to_string(s); }

  TypeP deref(u32 r) const {
    const TypeP &t = vty(r);
    if (t->kind != Type::Ref) throw Unsupported("dereference of a non-reference");
    return t->inner;
  }

  // ---- integer arithmetic

  void bin(const Inst &i) {
    const TypeP &t = vty(i.a);
    std::string r = opnd(i.result);
    if (t->kind == Type::Bool) {
      const char *o = i.bop == BinOp::And ? "and" : i.bop == BinOp::Or ? "or" : "xor";
      emit(r + " = " + o + " i1 " + opnd(i.a) + ", " + opnd(i.b));
      return;
    }
    if (t->kind != Type::Int) throw Unsupported("non-integer arithmetic");
    unsigned w = bits(t->ity);
    bool sg = is_signed(t->ity);
    std::string T = "i" + std::to_string(w), a = opnd(i.a), b = opnd(i.b);
    std::string tyname = name(t->ity);
    std::string ovmsg = std::string("attempt to ") + verb(i.bop) + " with overflow (`" + tyname + "`)";
    switch (i.bop) {
    case BinOp::And: case BinOp::Or: case BinOp::Xor: {
      const char *o = i.bop == BinOp::And ? "and" : i.bop == BinOp::Or ? "or" : "xor";
      emit(r + " = " + o + " " + T + " " + a + ", " + b);
      return;
    }
    case BinOp::Add: case BinOp::Sub: case BinOp::Mul: {
      const char *base = i.bop == BinOp::Add ? "add" : i.bop == BinOp::Sub ? "sub" : "mul";
      if (i.ov == Ov::Wrap) {
        emit(r + " = " + base + " " + T + " " + a + ", " + b);
        return;
      }
      if (i.ov == Ov::Sat && i.bop != BinOp::Mul) {
        std::string in = std::string("@llvm.") + (sg ? "s" : "u") + base + ".sat." + T;
        decls_.insert("declare " + T + " " + in + "(" + T + ", " + T + ")");
        emit(r + " = call " + T + " " + in + "(" + T + " " + a + ", " + T + " " + b + ")");
        return;
      }
      std::string in = std::string("@llvm.") + (sg ? "s" : "u") + base + ".with.overflow." + T;
      std::string pt = "{ " + T + ", i1 }";
      decls_.insert("declare " + pt + " " + in + "(" + T + ", " + T + ")");
      auto p = fresh(), o = fresh(), v = fresh();
      emit(p + " = call " + pt + " " + in + "(" + T + " " + a + ", " + T + " " + b + ")");
      emit(o + " = extractvalue " + pt + " " + p + ", 1");
      if (i.ov == Ov::Trap) {
        auto nok = fresh();
        emit(nok + " = xor i1 " + o + ", true");
        check(nok, trap_call(ovmsg));
        emit(r + " = extractvalue " + pt + " " + p + ", 0");
        return;
      }
      // Saturating multiply: on overflow the sign of the true product decides.
      emit(v + " = extractvalue " + pt + " " + p + ", 0");
      std::string bound;
      if (sg) {
        auto na = fresh(), nb = fresh(), neg = fresh();
        emit(na + " = icmp slt " + T + " " + a + ", 0");
        emit(nb + " = icmp slt " + T + " " + b + ", 0");
        emit(neg + " = xor i1 " + na + ", " + nb);
        bound = fresh();
        U128 mn{0, 0}, mx{~0ull, ~0ull};
        if (w == 128) { mn = {0, 1ull << 63}; mx = {~0ull, ~0ull >> 1}; }
        else if (w == 64) { mn = {1ull << 63, 0}; mx = {~0ull >> 1, 0}; }
        else { mn = {1ull << (w - 1), 0}; mx = {(1ull << (w - 1)) - 1, 0}; }
        emit(bound + " = select i1 " + neg + ", " + T + " " + lit(mn, w) + ", " + T + " " + lit(mx, w));
      } else {
        bound = "-1";
      }
      emit(r + " = select i1 " + o + ", " + T + " " + bound + ", " + T + " " + v);
      return;
    }
    case BinOp::Div: case BinOp::Rem: {
      auto nz = fresh();
      emit(nz + " = icmp ne " + T + " " + b + ", 0");
      check(nz, trap_call(i.bop == BinOp::Div ? "attempt to divide by zero"
                                               : "attempt to calculate the remainder with a divisor of zero"));
      // 128-bit division goes through our own IR helper: LLVM lowers
      // `udiv/sdiv/urem/srem i128` to compiler-rt calls (__divti3 ...),
      // which MSVC's runtime does not provide (spec/native.md §3).
      auto div = [&](const char *op, const std::string &x, const std::string &y) {
        if (w == 128)
          emit(r + " = call i128 " + div128_helper(op) + "(i128 " + x + ", i128 " + y + ")");
        else
          emit(r + " = " + op + " " + T + " " + x + ", " + y);
      };
      if (!sg) {
        div(i.bop == BinOp::Div ? "udiv" : "urem", a, b);
        return;
      }
      auto m1 = fresh();
      emit(m1 + " = icmp eq " + T + " " + b + ", -1");
      if (i.bop == BinOp::Div) {
        U128 mn = w == 128 ? U128{0, 1ull << 63} : U128{w == 64 ? 1ull << 63 : 1ull << (w - 1), 0};
        auto isMin = fresh(), bad = fresh(), ok = fresh();
        emit(isMin + " = icmp eq " + T + " " + a + ", " + lit(mn, w));
        emit(bad + " = and i1 " + isMin + ", " + m1);
        emit(ok + " = xor i1 " + bad + ", true");
        check(ok, trap_call(ovmsg));
        div("sdiv", a, b);
      } else {
        // MIN % -1 = 0 (Phase 0 §7); x % 1 = 0 as well, and srem by -1 is UB.
        auto sb = fresh();
        emit(sb + " = select i1 " + m1 + ", " + T + " 1, " + T + " " + b);
        div("srem", a, sb);
      }
      return;
    }
    case BinOp::Shl: case BinOp::Shr: {
      const TypeP &bt = vty(i.b);
      if (bt->kind != Type::Int) throw Unsupported("non-integer shift amount");
      unsigned bw = bits(bt->ity);
      std::string B = "i" + std::to_string(bw);
      auto ok = fresh();
      emit(ok + " = icmp ult " + B + " " + b + ", " + lit(U128{w, 0}, bw));
      auto t = label(), k = label();
      emit("br i1 " + ok + ", label %" + k + ", label %" + t);
      start(t);
      auto [lo, hi] = halves(b, bw, false);
      emit("call void @pg_rt_trap_shift(i64 " + lo + ", i64 " + hi + ", ptr " + str(tyname) + ", i32 " +
           std::to_string(w) + ")");
      emit("unreachable");
      start(k);
      std::string amt = b;
      if (bw != w) {
        amt = fresh();
        emit(amt + " = " + (bw > w ? "trunc " : "zext ") + B + " " + b + " to " + T);
      }
      const char *o = i.bop == BinOp::Shl ? "shl" : sg ? "ashr" : "lshr";
      emit(r + " = " + o + " " + T + " " + a + ", " + amt);
      return;
    }
    case BinOp::Pow: {
      const TypeP &bt = vty(i.b);
      if (bt->kind != Type::Int) throw Unsupported("non-integer exponent");
      std::string h = pow_helper(t->ity, bt->ity);
      emit(r + " = call " + T + " " + h + "(" + T + " " + a + ", " + ty(bt) + " " + b + ")");
      return;
    }
    }
  }

  // `x ** y` with the interpreter's semantics: an exponent beyond u32 traps
  // unless the base is an unsigned 0 or 1; otherwise the exact power must
  // fit the type. |x| >= 2 overflows within the type's bit width, so the
  // multiplication loop is bounded; 0, 1 and -1 are answered directly.
  // 128-bit `udiv`, `urem`, `sdiv`, `srem` (truncating, as LLVM's), in IR
  // that LLVM expands inline: restoring shift-subtract division over 128
  // bits. Callers have already excluded a zero divisor and, for `sdiv`,
  // MIN / -1; `srem` by -1 never reaches here (the caller divides by 1).
  std::string div128_helper(const char *op) {
    std::string nm = std::string("@pg.") + op + "128";
    if (!helper_names_.insert(nm).second) return nm;
    if (helper_names_.insert("@pg.udivmod128").second) {
      helpers_ +=
          "define internal { i128, i128 } @pg.udivmod128(i128 %n, i128 %d) {\n"
          "entry:\n"
          "  br label %loop\n"
          "loop:\n"
          "  %i = phi i32 [ 127, %entry ], [ %i.next, %loop ]\n"
          "  %q = phi i128 [ 0, %entry ], [ %q.next, %loop ]\n"
          "  %r = phi i128 [ 0, %entry ], [ %r.next, %loop ]\n"
          "  %sh = zext i32 %i to i128\n"
          "  %bit = lshr i128 %n, %sh\n"
          "  %b1 = and i128 %bit, 1\n"
          // 2r + bit can exceed 128 bits when d > 2^127: the carry out of the
          // shift then means the true value is >= 2^128 > d.
          "  %carry = icmp slt i128 %r, 0\n"
          "  %r2 = shl i128 %r, 1\n"
          "  %r3 = or i128 %r2, %b1\n"
          "  %ge0 = icmp uge i128 %r3, %d\n"
          "  %ge = or i1 %carry, %ge0\n"
          "  %r4 = sub i128 %r3, %d\n"
          "  %r.next = select i1 %ge, i128 %r4, i128 %r3\n"
          "  %qbit = shl i128 1, %sh\n"
          "  %q2 = or i128 %q, %qbit\n"
          "  %q.next = select i1 %ge, i128 %q2, i128 %q\n"
          "  %i.next = sub i32 %i, 1\n"
          "  %done = icmp eq i32 %i, 0\n"
          "  br i1 %done, label %out, label %loop\n"
          "out:\n"
          "  %res0 = insertvalue { i128, i128 } poison, i128 %q.next, 0\n"
          "  %res = insertvalue { i128, i128 } %res0, i128 %r.next, 1\n"
          "  ret { i128, i128 } %res\n"
          "}\n\n";
    }
    std::string o = "define internal i128 " + nm + "(i128 %a, i128 %b) {\nentry:\n";
    bool sgn = op[0] == 's', quot = std::string(op).find("div") != std::string::npos;
    if (sgn) {
      // |x| as unsigned (|MIN| = 2^127 is exact); quotient negative when the
      // signs differ, remainder takes the dividend's sign.
      o += "  %an = icmp slt i128 %a, 0\n  %bn = icmp slt i128 %b, 0\n"
           "  %na = sub i128 0, %a\n  %nb = sub i128 0, %b\n"
           "  %ua = select i1 %an, i128 %na, i128 %a\n  %ub = select i1 %bn, i128 %nb, i128 %b\n"
           "  %qr = call { i128, i128 } @pg.udivmod128(i128 %ua, i128 %ub)\n";
      if (quot)
        o += "  %uq = extractvalue { i128, i128 } %qr, 0\n  %neg = xor i1 %an, %bn\n"
             "  %nq = sub i128 0, %uq\n  %v = select i1 %neg, i128 %nq, i128 %uq\n";
      else
        o += "  %ur = extractvalue { i128, i128 } %qr, 1\n"
             "  %nr = sub i128 0, %ur\n  %v = select i1 %an, i128 %nr, i128 %ur\n";
    } else {
      o += "  %qr = call { i128, i128 } @pg.udivmod128(i128 %a, i128 %b)\n";
      o += std::string("  %v = extractvalue { i128, i128 } %qr, ") + (quot ? "0" : "1") + "\n";
    }
    o += "  ret i128 %v\n}\n\n";
    helpers_ += o;
    return nm;
  }

  std::string pow_helper(IntTy t, IntTy e) {
    std::string nm = std::string("@\"pg.pow.") + name(t) + "." + name(e) + "\"";
    if (!helper_names_.insert(nm).second) return nm;
    unsigned w = bits(t), ew = bits(e);
    bool sg = is_signed(t);
    std::string T = "i" + std::to_string(w), E = "i" + std::to_string(ew);
    std::string msg = str(std::string("attempt to exponentiate with overflow (`") + name(t) + "`)");
    std::string mul = std::string("@llvm.") + (sg ? "s" : "u") + "mul.with.overflow." + T;
    decls_.insert("declare { " + T + ", i1 } " + mul + "(" + T + ", " + T + ")");
    std::ostringstream o;
    o << "define internal " << T << " " << nm << "(" << T << " %x, " << E << " %y) {\n";
    o << "entry:\n";
    if (ew < 128) o << "  %y128 = zext " << E << " %y to i128\n";
    else o << "  %y128 = add i128 %y, 0\n";
    o << "  %huge = icmp ugt i128 %y128, 4294967295\n";
    o << "  br i1 %huge, label %huge_e, label %norm\n";
    o << "huge_e:\n";
    if (sg) {
      o << "  br label %trap\n";
    } else {
      o << "  %small = icmp ule " << T << " %x, 1\n";
      o << "  br i1 %small, label %ret_x, label %trap\n";
      o << "ret_x:\n  ret " << T << " %x\n";
    }
    o << "trap:\n  call void @pg_rt_trap(ptr " << msg << ")\n  unreachable\n";
    o << "norm:\n";
    o << "  %e = trunc i128 %y128 to i32\n";
    o << "  %ez = icmp eq i32 %e, 0\n";
    o << "  br i1 %ez, label %ret1, label %c0\n";
    o << "ret1:\n  ret " << T << " 1\n";
    o << "c0:\n  %x0 = icmp eq " << T << " %x, 0\n  br i1 %x0, label %ret0, label %c1\n";
    o << "ret0:\n  ret " << T << " 0\n";
    o << "c1:\n  %x1 = icmp eq " << T << " %x, 1\n  br i1 %x1, label %ret1, label %cm1\n";
    o << "cm1:\n";
    if (sg) {
      o << "  %xm1 = icmp eq " << T << " %x, -1\n  br i1 %xm1, label %parity, label %loop\n";
      o << "parity:\n  %odd = trunc i32 %e to i1\n  %pr = select i1 %odd, " << T << " -1, " << T
        << " 1\n  ret " << T << " %pr\n";
    } else {
      o << "  br label %loop\n";
    }
    o << "loop:\n";
    o << "  %acc = phi " << T << " [ 1, %" << (sg ? "cm1" : "cm1") << " ], [ %acc2, %body ]\n";
    o << "  %k = phi i32 [ %e, %cm1 ], [ %k2, %body ]\n";
    o << "  %p = call { " << T << ", i1 } " << mul << "(" << T << " %acc, " << T << " %x)\n";
    o << "  %ov = extractvalue { " << T << ", i1 } %p, 1\n";
    o << "  br i1 %ov, label %trap, label %body\n";
    o << "body:\n";
    o << "  %acc2 = extractvalue { " << T << ", i1 } %p, 0\n";
    o << "  %k2 = sub i32 %k, 1\n";
    o << "  %done = icmp eq i32 %k2, 0\n";
    o << "  br i1 %done, label %out, label %loop\n";
    o << "out:\n  ret " << T << " %acc2\n}\n\n";
    helpers_ += o.str();
    return nm;
  }

  // Structural equality of two values of type t (an i1 operand).
  std::string equal(const std::string &a, const std::string &b, const TypeP &t) {
    switch (t->kind) {
    case Type::Int: case Type::Bool: {
      auto r = fresh();
      emit(r + " = icmp eq " + ty(t) + " " + a + ", " + b);
      return r;
    }
    case Type::Tuple: case Type::Struct: case Type::Array: {
      std::vector<TypeP> fs;
      if (t->kind == Type::Array) {
        if (t->len > 1024) throw Unsupported("comparison of arrays longer than 1024 elements");
        fs.assign(t->len, t->inner);
      } else {
        fs = fields(t);
      }
      std::string acc = "true";
      for (std::size_t k = 0; k < fs.size(); ++k) {
        auto x = fresh(), y = fresh();
        emit(x + " = extractvalue " + ty(t) + " " + a + ", " + std::to_string(k));
        emit(y + " = extractvalue " + ty(t) + " " + b + ", " + std::to_string(k));
        auto e = equal(x, y, fs[k]);
        auto n = fresh();
        emit(n + " = and i1 " + acc + ", " + e);
        acc = n;
      }
      return acc;
    }
    case Type::Ref: inconsistent("equality on references");
    default: throw Unsupported("equality on this type");
    }
  }

  void cmp(const Inst &i) {
    const TypeP &t = vty(i.a);
    std::string r = opnd(i.result);
    if (i.cop == CmpOp::Eq || i.cop == CmpOp::Ne) {
      if (t->kind == Type::Int || t->kind == Type::Bool) {
        emit(r + " = icmp " + (i.cop == CmpOp::Eq ? "eq " : "ne ") + ty(t) + " " + opnd(i.a) + ", " + opnd(i.b));
        return;
      }
      auto e = equal(opnd(i.a), opnd(i.b), t);
      emit(r + " = xor i1 " + e + ", " + (i.cop == CmpOp::Eq ? "false" : "true"));
      return;
    }
    // `bool` has no ordering (spec/semantics.md §6); the verifier rejects it.
    if (t->kind == Type::Bool) inconsistent("ordering comparison on bool");
    if (t->kind != Type::Int) throw Unsupported("ordering on this type");
    bool sg = is_signed(t->ity);
    const char *p = "";
    switch (i.cop) {
    case CmpOp::Lt: p = sg ? "slt" : "ult"; break;
    case CmpOp::Le: p = sg ? "sle" : "ule"; break;
    case CmpOp::Gt: p = sg ? "sgt" : "ugt"; break;
    default: p = sg ? "sge" : "uge"; break;
    }
    emit(r + " = icmp " + p + " " + ty(t) + " " + opnd(i.a) + ", " + opnd(i.b));
  }

  void un(const Inst &i) {
    const TypeP &t = vty(i.a);
    std::string r = opnd(i.result), a = opnd(i.a);
    switch (i.uop) {
    case UnOp::Not: emit(r + " = xor i1 " + a + ", true"); return;
    case UnOp::BitNot: emit(r + " = xor " + ty(t) + " " + a + ", -1"); return;
    case UnOp::Neg: {
      if (t->kind != Type::Int) throw Unsupported("non-integer negation");
      unsigned w = bits(t->ity);
      std::string T = ty(t);
      std::string msg = std::string("attempt to negate with overflow (`") + name(t->ity) + "`)";
      if (!is_signed(t->ity)) {
        check("false", trap_call(msg));
      } else {
        U128 mn = w == 128 ? U128{0, 1ull << 63} : U128{w == 64 ? 1ull << 63 : 1ull << (w - 1), 0};
        auto ok = fresh();
        emit(ok + " = icmp ne " + T + " " + a + ", " + lit(mn, w));
        check(ok, trap_call(msg));
      }
      emit(r + " = sub " + T + " 0, " + a);
      return;
    }
    }
  }

  void cast(const Inst &i) {
    const TypeP &from = vty(i.a);
    const TypeP &to = i.to;
    std::string r = opnd(i.result);
    if (to->kind != Type::Int) throw Unsupported("cast to a non-integer type");
    unsigned tw = bits(to->ity);
    std::string T = ty(to);
    if (from->kind == Type::Bool) {
      emit(r + " = zext i1 " + opnd(i.a) + " to " + T);
      return;
    }
    if (from->kind != Type::Int) throw Unsupported("cast from this type");
    unsigned fw = bits(from->ity);
    if (fw == tw) emit(r + " = add " + T + " " + opnd(i.a) + ", 0");
    else if (fw > tw) emit(r + " = trunc " + ty(from) + " " + opnd(i.a) + " to " + T);
    else emit(r + " = " + (is_signed(from->ity) ? "sext " : "zext ") + ty(from) + " " + opnd(i.a) + " to " + T);
  }

  // Build an aggregate of type t from element operands.
  void build(const std::string &r, const TypeP &t, const std::vector<std::string> &elems,
             const std::vector<TypeP> &etys) {
    std::string T = ty(t), acc = "poison";
    for (std::size_t k = 0; k < elems.size(); ++k) {
      std::string n = k + 1 == elems.size() ? r : fresh();
      emit(n + " = insertvalue " + T + " " + acc + ", " + ty(etys[k]) + " " + elems[k] + ", " + std::to_string(k));
      acc = n;
    }
  }

  std::string temp_alloca(const TypeP &t) {
    std::string a = "%a" + std::to_string(x_->tmp++);
    x_->allocas += "  " + a + " = alloca " + ty(t) + "\n";
    return a;
  }

  void inst(const Inst &i) {
    const Func &f = *x_->f;
    bool produces = i.op != Op::Store && i.op != Op::StoreRef && i.op != Op::Drop && i.op != Op::Call;
    if (produces && !i.has_result) inconsistent("instruction without a result");
    auto rty = [&] { return vty(i.result); };
    switch (i.op) {
    case Op::Const:
      if (i.c.kind == Const::Int || i.c.kind == Const::Bool) return;  // literal operand
      throw Unsupported(i.c.kind == Const::Float ? "floating-point constants"
                        : i.c.kind == Const::Char ? "`char` constants" : "string constants");
    case Op::Bin: bin(i); return;
    case Op::Un: un(i); return;
    case Op::Cmp: cmp(i); return;
    case Op::Cast: cast(i); return;
    case Op::Load: case Op::Move: {
      std::vector<Proj> path = i.path;
      if (i.op == Op::Move)
        for (auto n : i.args) path.push_back(Proj{false, n});
      auto [p, t] = place(slot(i.a), f.slots[i.a], path);
      emit(opnd(i.result) + " = load " + ty(t) + ", ptr " + p);
      return;
    }
    case Op::Store: {
      auto [p, t] = place(slot(i.a), f.slots[i.a], i.path);
      emit("store " + typed(i.b) + ", ptr " + p);
      return;
    }
    case Op::Drop:
      // No type in the native subset has a destructor: ending the owner's
      // scope has no observable effect (spec/native.md §3).
      return;
    case Op::Borrow: case Op::BorrowRef: {
      auto [p, t] = i.op == Op::Borrow ? place(slot(i.a), f.slots[i.a], i.path) : place(opnd(i.a), deref(i.a), i.path);
      emit(opnd(i.result) + " = getelementptr inbounds i8, ptr " + p + ", i64 0");
      return;
    }
    case Op::LoadRef: {
      auto [p, t] = place(opnd(i.a), deref(i.a), i.path);
      emit(opnd(i.result) + " = load " + ty(t) + ", ptr " + p);
      return;
    }
    case Op::StoreRef: {
      auto [p, t] = place(opnd(i.a), deref(i.a), i.path);
      emit("store " + typed(i.b) + ", ptr " + p);
      return;
    }
    case Op::Tuple: case Op::Struct: case Op::Array: {
      if (i.args.empty()) return;  // zeroinitializer operand
      std::vector<std::string> el;
      std::vector<TypeP> et;
      for (auto v : i.args) {
        el.push_back(opnd(v));
        et.push_back(vty(v));
      }
      build(opnd(i.result), rty(), el, et);
      return;
    }
    case Op::Repeat: {
      if (i.n == 0) return;
      const TypeP &t = rty();
      if (i.n <= 64) {
        build(opnd(i.result), t, std::vector<std::string>(i.n, opnd(i.a)), std::vector<TypeP>(i.n, vty(i.a)));
        return;
      }
      auto a = temp_alloca(t);
      auto head = label(), body = label(), done = label();
      std::string pre = label();
      emit("br label %" + pre);
      start(pre);
      emit("br label %" + head);
      start(head);
      auto k = fresh(), k2 = fresh(), more = fresh(), p = fresh();
      emit(k + " = phi i64 [ 0, %" + pre + " ], [ " + k2 + ", %" + body + " ]");
      emit(more + " = icmp ult i64 " + k + ", " + std::to_string(i.n));
      emit("br i1 " + more + ", label %" + body + ", label %" + done);
      start(body);
      emit(p + " = getelementptr inbounds " + ty(t) + ", ptr " + a + ", i64 0, i64 " + k);
      emit("store " + typed(i.a) + ", ptr " + p);
      emit(k2 + " = add i64 " + k + ", 1");
      emit("br label %" + head);
      start(done);
      emit(opnd(i.result) + " = load " + ty(t) + ", ptr " + a);
      return;
    }
    case Op::Extract:
      emit(opnd(i.result) + " = extractvalue " + typed(i.a) + ", " + std::to_string(i.idx));
      return;
    case Op::ExtractIndex: {
      const TypeP &t = array(vty(i.a));
      auto a = temp_alloca(t);
      emit("store " + typed(i.a) + ", ptr " + a);
      auto idx = index(i.b, t->len);
      auto p = fresh();
      emit(p + " = getelementptr inbounds " + ty(t) + ", ptr " + a + ", i64 0, i64 " + idx);
      emit(opnd(i.result) + " = load " + ty(t->inner) + ", ptr " + p);
      return;
    }
    case Op::Call: {
      const Func &g = m_.funcs[i.a];
      std::string args;
      for (std::size_t k = 0; k < i.args.size(); ++k) args += (k ? ", " : "") + typed(i.args[k]);
      std::string call = "call " + ty(g.ret) + " " + fname(i.a) + "(" + args + ")";
      emit(i.has_result ? opnd(i.result) + " = " + call : call);
      return;
    }
    case Op::CallIndirect: case Op::Closure: throw Unsupported("function values and closures");
    case Op::Variant: case Op::Tag: case Op::VariantField: throw Unsupported("enums");
    }
  }

  std::string func(u32 fid) {
    const Func &f = m_.funcs[fid];
    Fx fx;
    fx.f = &f;
    x_ = &fx;
    fx.op.assign(f.values.size(), "");
    // Operand text: block parameters and instruction results are named
    // %vN; integer and boolean constants and empty aggregates are literals.
    for (auto &b : f.blocks) {
      for (auto p : b.params) fx.op.at(p) = "%v" + std::to_string(p);
      for (auto &i : b.insts) {
        if (!i.has_result) continue;
        std::string &o = fx.op.at(i.result);
        o = "%v" + std::to_string(i.result);
        if (i.op == Op::Const && i.c.kind == Const::Int) o = lit(i.c.bits, bits(i.c.ity));
        if (i.op == Op::Const && i.c.kind == Const::Bool) o = i.c.b ? "true" : "false";
        bool empty = ((i.op == Op::Tuple || i.op == Op::Struct || i.op == Op::Array) && i.args.empty()) ||
                     (i.op == Op::Repeat && i.n == 0);
        if (empty) o = "zeroinitializer";
      }
    }
    // Every Br/CondBr edge goes through its own block, so that phi nodes
    // name a unique predecessor even when a PIR block is split by checks.
    std::vector<std::vector<std::pair<std::string, std::string>>> incoming(f.blocks.size());  // (label, args)
    std::vector<std::vector<std::vector<u32>>> in_args(f.blocks.size());
    std::vector<std::string> bodies(f.blocks.size());
    std::string tramps;
    for (std::size_t b = 0; b < f.blocks.size(); ++b) {
      fx.cur.clear();
      for (auto &i : f.blocks[b].insts) inst(i);
      const Term &t = f.blocks[b].term;
      auto edge = [&](const Edge &e, int k) {
        std::string l = "bb" + std::to_string(b) + ".e" + std::to_string(k);
        tramps += l + ":\n  br label %bb" + std::to_string(e.block) + "\n";
        incoming[e.block].push_back({l, ""});
        in_args[e.block].push_back(e.args);
        return l;
      };
      switch (t.kind) {
      case Term::Br: emit("br label %" + edge(t.t, 0)); break;
      case Term::CondBr: {
        auto lt = edge(t.t, 0), lf = edge(t.f, 1);
        emit("br i1 " + opnd(t.v) + ", label %" + lt + ", label %" + lf);
        break;
      }
      case Term::Switch: {
        const TypeP &st = vty(t.v);
        if (st->kind != Type::Int && st->kind != Type::Bool) throw Unsupported("switch on this type");
        unsigned w = st->kind == Type::Bool ? 1 : bits(st->ity);
        std::string s = "switch " + typed(t.v) + ", label %bb" + std::to_string(t.dflt) + " [";
        for (auto &c : t.cases) {
          std::string k = st->kind == Type::Bool ? ((c.first.lo & 1) ? "true" : "false") : lit(c.first, w);
          s += " " + ty(st) + " " + k + ", label %bb" + std::to_string(c.second);
        }
        emit(s + " ]");
        for (auto &c : t.cases)
          if (!f.blocks[c.second].params.empty()) throw Unsupported("switch to a block with parameters");
        if (!f.blocks[t.dflt].params.empty()) throw Unsupported("switch to a block with parameters");
        break;
      }
      case Term::Ret: {
        auto d = fresh(), d1 = fresh();
        emit(d + " = load i64, ptr @pg_rt_depth");
        emit(d1 + " = sub i64 " + d + ", 1");
        emit("store i64 " + d1 + ", ptr @pg_rt_depth");
        emit("ret " + typed(t.v));
        break;
      }
      case Term::Trap: emit(trap_call(t.msg)); emit("unreachable"); break;
      case Term::Unreachable:
        emit(trap_call("reached unreachable code (compiler bug)"));
        emit("unreachable");
        break;
      }
      bodies[b] = fx.cur;
    }
    std::ostringstream o;
    o << "define internal " << ty(f.ret) << " " << fname(fid) << "(";
    for (std::size_t k = 0; k < f.params.size(); ++k) o << (k ? ", " : "") << ty(f.params[k]) << " %p" << k;
    o << ") {\nentry:\n";
    for (std::size_t s = 0; s < f.slots.size(); ++s) o << "  %s" << s << " = alloca " << ty(f.slots[s]) << "\n";
    o << fx.allocas;
    // Call depth (spec/native.md §4): the interpreter's limit, same message.
    o << "  %depth = load i64, ptr @pg_rt_depth\n";
    o << "  %depth1 = add i64 %depth, 1\n";
    o << "  store i64 %depth1, ptr @pg_rt_depth\n";
    o << "  %deep = icmp ugt i64 %depth1, " << kMaxDepth << "\n";
    o << "  br i1 %deep, label %overflow, label %bb0\n";
    o << "overflow:\n  call void @pg_rt_trap(ptr " << str("stack overflow (call depth limit reached)")
      << ")\n  unreachable\n";
    for (std::size_t b = 0; b < f.blocks.size(); ++b) {
      o << "bb" << b << ":\n";
      const auto &params = f.blocks[b].params;
      for (std::size_t k = 0; k < params.size(); ++k) {
        u32 p = params[k];
        o << "  %v" << p << " = phi " << ty(f.values[p]);
        bool first = true;
        if (b == 0) {
          o << " [ %p" << k << ", %entry ]";
          first = false;
        }
        for (std::size_t e = 0; e < incoming[b].size(); ++e) {
          o << (first ? " " : ", ") << "[ " << fx.op.at(in_args[b][e].at(k)) << ", %" << incoming[b][e].first << " ]";
          first = false;
        }
        o << "\n";
      }
      o << bodies[b];
    }
    o << tramps << "}\n\n";
    x_ = nullptr;
    return o.str();
  }

  // ---- result printing (interp::show) in the generated `main`

  void print(const std::string &v, const TypeP &t) {
    auto s = [&](const std::string &text) { emit("call void @pg_rt_print_str(ptr " + str(text) + ")"); };
    switch (t->kind) {
    case Type::Int: {
      auto [lo, hi] = halves(v, bits(t->ity), is_signed(t->ity));
      emit("call void @pg_rt_print_int(i64 " + lo + ", i64 " + hi + ", i32 " + (is_signed(t->ity) ? "1" : "0") + ")");
      return;
    }
    case Type::Bool: {
      auto p = fresh();
      emit(p + " = select i1 " + v + ", ptr " + str("true") + ", ptr " + str("false"));
      emit("call void @pg_rt_print_str(ptr " + p + ")");
      return;
    }
    case Type::Tuple: case Type::Array: case Type::Struct: {
      std::vector<TypeP> fs;
      if (t->kind == Type::Array) {
        if (t->len > 4096) throw Unsupported("printing arrays longer than 4096 elements");
        fs.assign(t->len, t->inner);
      } else {
        fs = fields(t);
      }
      if (t->kind == Type::Struct) s(m_.structs[t->id].name + " { ");
      else s(t->kind == Type::Tuple ? "(" : "[");
      for (std::size_t k = 0; k < fs.size(); ++k) {
        if (k) s(", ");
        if (t->kind == Type::Struct) s(m_.structs[t->id].fields[k].first + ": ");
        auto e = fresh();
        emit(e + " = extractvalue " + ty(t) + " " + v + ", " + std::to_string(k));
        print(e, fs[k]);
      }
      s(t->kind == Type::Struct ? " }" : t->kind == Type::Tuple ? ")" : "]");
      return;
    }
    default: throw Unsupported("printing a result of this type");
    }
  }

  // `main` runs the program on a thread with a large stack (pg_rt_run), so
  // the depth limit, not the platform's default stack, decides overflow.
  std::string main_wrapper() {
    const Func &f = m_.funcs[m_.entry];
    Fx fx;
    fx.f = &f;
    x_ = &fx;
    std::string call = "call " + ty(f.ret) + " " + fname(m_.entry) + "()";
    if (f.ret->is_unit()) {
      emit(call);
    } else {
      emit("%r = " + call);
      print("%r", f.ret);
      emit("call void @pg_rt_print_str(ptr " + str("\n") + ")");
    }
    emit("ret void");
    std::string out = "define internal void @pg.program() {\nentry:\n" + fx.allocas + fx.cur + "}\n\n";
    out += "define i32 @main() {\nentry:\n  call void @pg_rt_run(ptr @pg.program)\n  ret i32 0\n}\n";
    x_ = nullptr;
    return out;
  }
};

}  // namespace

std::string emit_llvm(const Module &m) { return Emitter(m).run(); }

}  // namespace pegasus::native
