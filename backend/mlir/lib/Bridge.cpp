// PIRB <-> Pegasus MLIR dialect (spec/mlir.md §3).
//
// Import: one func.func per PIR function (symbol "f<id>.<name>"), one pg
// operation per instruction and terminator, PIR blocks as MLIR blocks with
// arguments. What MLIR does not number itself is kept as attributes so that
// export is exact: the value id of every result ("pg.v") and of every block
// parameter (func attribute "pg.block_params"), and whether slot names were
// present ("pg.named_slots"). Struct and enum definitions are module
// attributes ("pg.structs", "pg.enums"); the entry is "pg.entry".
//
// MLIR forbids branches to a region's entry block; when PIR's bb0 has
// predecessors, a separate entry block (attribute "pg.entry_trampoline")
// branches to it with the function's arguments, and export drops it.
#include "pegasus/mlir/Bridge.h"

#include "pegasus/mlir/PegasusDialect.h"

#include "mlir/Dialect/Func/IR/FuncOps.h"
#include "mlir/IR/Builders.h"
#include "mlir/IR/Verifier.h"

#include <map>

namespace pegasus::pg {
using namespace mlir;

void registerDialects(MLIRContext &ctx) {
  ctx.getOrLoadDialect<PgDialect>();
  ctx.getOrLoadDialect<func::FuncDialect>();
}

namespace {

const char *const kBin[] = {"add", "sub", "mul", "div", "rem", "pow", "shl", "shr", "and", "or", "xor"};
const char *const kOv[] = {"trap", "wrap", "sat"};
const char *const kUn[] = {"neg", "not", "bitnot"};
const char *const kCmp[] = {"eq", "ne", "lt", "le", "gt", "ge"};

std::string symName(const pir::Module &m, pir::u32 f) { return "f" + std::to_string(f) + "." + m.funcs[f].name; }

APInt apint(pir::U128 v) {
  uint64_t words[2] = {v.lo, v.hi};
  return APInt(128, words);
}
pir::U128 u128(const APInt &a) {
  APInt w = a.zextOrTrunc(128);
  return {w.extractBitsAsZExtValue(64, 0), w.extractBitsAsZExtValue(64, 64)};
}

//===----------------------------------------------------------------------===//
// Import
//===----------------------------------------------------------------------===//

class Importer {
public:
  Importer(MLIRContext &ctx, const pir::Module &m) : ctx_(ctx), m_(m), b_(&ctx) {}

  OwningOpRef<ModuleOp> run() {
    auto loc = UnknownLoc::get(&ctx_);
    OwningOpRef<ModuleOp> mod = ModuleOp::create(loc);
    mod->getOperation()->setAttr("pg.pirb_minor", b_.getI64IntegerAttr(m_.minor));
    SmallVector<Attribute> structs, enums;
    for (auto &s : m_.structs) {
      SmallVector<Attribute> names, tys;
      for (auto &f : s.fields) {
        names.push_back(b_.getStringAttr(f.first));
        tys.push_back(TypeAttr::get(type(f.second)));
      }
      structs.push_back(b_.getDictionaryAttr({b_.getNamedAttr("name", b_.getStringAttr(s.name)),
                                              b_.getNamedAttr("field_names", b_.getArrayAttr(names)),
                                              b_.getNamedAttr("field_types", b_.getArrayAttr(tys))}));
    }
    for (auto &e : m_.enums) {
      SmallVector<Attribute> names, tys;
      for (auto &v : e.variants) {
        names.push_back(b_.getStringAttr(v.first));
        SmallVector<Attribute> vt;
        for (auto &t : v.second) vt.push_back(TypeAttr::get(type(t)));
        tys.push_back(b_.getArrayAttr(vt));
      }
      enums.push_back(b_.getDictionaryAttr({b_.getNamedAttr("name", b_.getStringAttr(e.name)),
                                            b_.getNamedAttr("variant_names", b_.getArrayAttr(names)),
                                            b_.getNamedAttr("variant_types", b_.getArrayAttr(tys))}));
    }
    mod->getOperation()->setAttr("pg.structs", b_.getArrayAttr(structs));
    mod->getOperation()->setAttr("pg.enums", b_.getArrayAttr(enums));
    if (m_.has_entry)
      mod->getOperation()->setAttr("pg.entry", FlatSymbolRefAttr::get(&ctx_, symName(m_, m_.entry)));
    b_.setInsertionPointToEnd(mod->getBody());
    for (pir::u32 f = 0; f < m_.funcs.size(); ++f) func(f);
    return mod;
  }

private:
  MLIRContext &ctx_;
  const pir::Module &m_;
  OpBuilder b_;
  // Per function.
  const pir::Func *f_ = nullptr;
  std::map<pir::u32, Value> vals_;
  std::map<pir::u32, Operation *> placeholders_;
  std::vector<Value> slots_;
  std::vector<Block *> blocks_;
  Block *placeholderBlock_ = nullptr;

  Type type(const pir::TypeP &t) {
    using K = pir::Type;
    switch (t->kind) {
    case K::Int: return IntType::get(&ctx_, static_cast<unsigned>(t->ity));
    case K::Float: return FloatType::get(&ctx_, static_cast<unsigned>(t->fty));
    case K::Bool: return BoolType::get(&ctx_);
    case K::Char: return CharType::get(&ctx_);
    case K::Str: return StrType::get(&ctx_);
    case K::Region: return RegionType::get(&ctx_);
    case K::Tuple: {
      SmallVector<Type> es;
      for (auto &e : t->elems) es.push_back(type(e));
      return TupleType::get(&ctx_, es);
    }
    case K::Array: return ArrayType::get(&ctx_, type(t->inner), t->len);
    case K::Struct: return StructType::get(&ctx_, t->id);
    case K::Enum: return EnumType::get(&ctx_, t->id);
    case K::Fn: {
      SmallVector<Type> ps;
      for (auto &e : t->elems) ps.push_back(type(e));
      return FnType::get(&ctx_, ps, type(t->inner));
    }
    case K::Ptr: return PtrType::get(&ctx_, t->mut_, type(t->inner));
    case K::Ref: return RefType::get(&ctx_, t->mut_, type(t->inner));
    }
    return {};
  }
  Type vty(pir::u32 v) { return type(f_->values.at(v)); }

  // The value with PIR id v; a placeholder until its definition is reached
  // (PIR orders blocks by index, not dominance).
  Value val(pir::u32 v) {
    auto it = vals_.find(v);
    if (it != vals_.end()) return it->second;
    OpBuilder pb(&ctx_);
    pb.setInsertionPointToEnd(placeholderBlock_);
    auto ph = pb.create<UnrealizedConversionCastOp>(UnknownLoc::get(&ctx_), TypeRange{vty(v)}, ValueRange{});
    placeholders_[v] = ph;
    vals_[v] = ph.getResult(0);
    return ph.getResult(0);
  }
  void define(pir::u32 v, Value x) {
    auto ph = placeholders_.find(v);
    if (ph != placeholders_.end()) {
      ph->second->getResult(0).replaceAllUsesWith(x);
      ph->second->erase();
      placeholders_.erase(ph);
    }
    vals_[v] = x;
  }
  SmallVector<Value> vals(const std::vector<pir::u32> &vs) {
    SmallVector<Value> out;
    for (auto v : vs) out.push_back(val(v));
    return out;
  }
  // A path as (DenseI64ArrayAttr path, index operands).
  std::pair<DenseI64ArrayAttr, SmallVector<Value>> path(const std::vector<pir::Proj> &p) {
    SmallVector<int64_t> steps;
    SmallVector<Value> idx;
    for (auto &x : p) {
      if (x.index) {
        steps.push_back(-1);
        idx.push_back(val(x.n));
      } else {
        steps.push_back(x.n);
      }
    }
    return {b_.getDenseI64ArrayAttr(steps), idx};
  }

  void func(pir::u32 id) {
    const pir::Func &f = m_.funcs[id];
    f_ = &f;
    auto loc = UnknownLoc::get(&ctx_);
    SmallVector<Type> params;
    for (auto &t : f.params) params.push_back(type(t));
    auto fty = b_.getFunctionType(params, {type(f.ret)});
    auto fn = b_.create<func::FuncOp>(loc, symName(m_, id), fty);
    fn->setAttr("pg.name", b_.getStringAttr(f.name));
    if (!f.defined) {
      fn.setPrivate();
      fn->setAttr("pg.abi", b_.getStringAttr(f.abi));
      return;
    }
    vals_.clear();
    placeholders_.clear();
    slots_.clear();
    blocks_.clear();
    Region &body = fn.getBody();
    // Does bb0 have predecessors?
    bool bb0Target = false;
    for (auto &blk : f.blocks) {
      const pir::Term &t = blk.term;
      if ((t.kind == pir::Term::Br && t.t.block == 0) ||
          (t.kind == pir::Term::CondBr && (t.t.block == 0 || t.f.block == 0)) ||
          (t.kind == pir::Term::Switch && t.dflt == 0))
        bb0Target = true;
      if (t.kind == pir::Term::Switch)
        for (auto &c : t.cases) bb0Target |= c.second == 0;
    }
    Block *entry = new Block();
    body.push_back(entry);
    for (auto &t : params) entry->addArgument(t, loc);
    placeholderBlock_ = new Block();
    for (size_t k = 0; k < f.blocks.size(); ++k) {
      Block *blk = (k == 0 && !bb0Target) ? entry : new Block();
      if (blk != entry) body.push_back(blk);
      blocks_.push_back(blk);
      if (blk != entry)
        for (auto p : f.blocks[k].params) blk->addArgument(vty(p), loc);
    }
    if (bb0Target) fn->setAttr("pg.entry_trampoline", b_.getUnitAttr());
    // Block parameter ids, in block order.
    SmallVector<Attribute> bparams;
    for (size_t k = 0; k < f.blocks.size(); ++k) {
      SmallVector<int64_t> ids(f.blocks[k].params.begin(), f.blocks[k].params.end());
      bparams.push_back(b_.getDenseI64ArrayAttr(ids));
      for (size_t a = 0; a < f.blocks[k].params.size(); ++a)
        define(f.blocks[k].params[a], blocks_[k]->getArgument(a));
    }
    fn->setAttr("pg.block_params", b_.getArrayAttr(bparams));
    if (!f.slot_names.empty()) fn->setAttr("pg.named_slots", b_.getUnitAttr());
    // Slots at the top of the entry block, in slot order.
    b_.setInsertionPointToStart(entry);
    for (size_t s = 0; s < f.slots.size(); ++s) {
      auto st = SlotType::get(&ctx_, type(f.slots[s]));
      StringAttr name = f.slot_names.empty() ? StringAttr() : b_.getStringAttr(f.slot_names[s]);
      slots_.push_back(b_.create<SlotOp>(loc, st, name).getSlot());
    }
    if (bb0Target) {
      b_.setInsertionPointToEnd(entry);
      b_.create<BrOp>(loc, entry->getArguments(), blocks_[0]);
    }
    for (size_t k = 0; k < f.blocks.size(); ++k) {
      b_.setInsertionPointToEnd(blocks_[k]);
      for (auto &i : f.blocks[k].insts) inst(i);
      term(f.blocks[k].term);
    }
    delete placeholderBlock_;  // empty: every placeholder was defined
    b_.setInsertionPointAfter(fn);
  }

  Value slot(pir::u32 s) { return slots_.at(s); }

  void inst(const pir::Inst &i) {
    auto loc = UnknownLoc::get(&ctx_);
    using pir::Op;
    Type rt = i.has_result ? vty(i.result) : Type();
    Operation *op = nullptr;
    switch (i.op) {
    case Op::Const: {
      Attribute a;
      switch (i.c.kind) {
      case pir::Const::Int: a = b_.getIntegerAttr(b_.getIntegerType(128), apint(i.c.bits)); break;
      case pir::Const::Float: a = b_.getIntegerAttr(b_.getIntegerType(64), APInt(64, i.c.fbits)); break;
      case pir::Const::Bool: a = b_.getBoolAttr(i.c.b); break;
      case pir::Const::Char: a = b_.getIntegerAttr(b_.getIntegerType(32), APInt(32, i.c.ch)); break;
      case pir::Const::Str: a = b_.getStringAttr(i.c.s); break;
      }
      op = b_.create<ConstOp>(loc, rt, a);
      break;
    }
    case Op::Bin:
      op = b_.create<BinOp>(loc, rt, val(i.a), val(i.b), kBin[static_cast<int>(i.bop)], kOv[static_cast<int>(i.ov)]);
      break;
    case Op::Un: op = b_.create<UnOp>(loc, rt, val(i.a), kUn[static_cast<int>(i.uop)]); break;
    case Op::Cmp: op = b_.create<CmpOp>(loc, rt, val(i.a), val(i.b), kCmp[static_cast<int>(i.cop)]); break;
    case Op::Cast: op = b_.create<CastOp>(loc, rt, val(i.a)); break;
    case Op::Load: {
      auto [p, idx] = path(i.path);
      op = b_.create<LoadOp>(loc, rt, slot(i.a), idx, p);
      break;
    }
    case Op::Store: {
      auto [p, idx] = path(i.path);
      op = b_.create<StoreOp>(loc, slot(i.a), val(i.b), idx, p);
      break;
    }
    case Op::Move: {
      SmallVector<int64_t> steps(i.args.begin(), i.args.end());
      op = b_.create<MoveOp>(loc, rt, slot(i.a), b_.getDenseI64ArrayAttr(steps));
      break;
    }
    case Op::Drop: op = b_.create<DropOp>(loc, slot(i.a)); break;
    case Op::Borrow: {
      auto [p, idx] = path(i.path);
      op = b_.create<BorrowOp>(loc, rt, slot(i.a), idx, p, i.mut_);
      break;
    }
    case Op::BorrowRef: {
      auto [p, idx] = path(i.path);
      op = b_.create<BorrowRefOp>(loc, rt, val(i.a), idx, p, i.mut_);
      break;
    }
    case Op::LoadRef: {
      auto [p, idx] = path(i.path);
      op = b_.create<LoadRefOp>(loc, rt, val(i.a), idx, p);
      break;
    }
    case Op::StoreRef: {
      auto [p, idx] = path(i.path);
      op = b_.create<StoreRefOp>(loc, val(i.a), val(i.b), idx, p);
      break;
    }
    case Op::Tuple: op = b_.create<TupleOp>(loc, rt, vals(i.args)); break;
    case Op::Array: op = b_.create<ArrayOp>(loc, rt, vals(i.args)); break;
    case Op::Struct: op = b_.create<StructOp>(loc, rt, vals(i.args), i.a); break;
    case Op::Variant: op = b_.create<VariantOp>(loc, rt, vals(i.args), i.a, i.b); break;
    case Op::Repeat: op = b_.create<RepeatOp>(loc, rt, val(i.a), static_cast<int64_t>(i.n)); break;
    case Op::Extract: op = b_.create<ExtractOp>(loc, rt, val(i.a), i.idx); break;
    case Op::ExtractIndex: op = b_.create<ExtractIndexOp>(loc, rt, val(i.a), val(i.b)); break;
    case Op::Tag: op = b_.create<TagOp>(loc, rt, val(i.a)); break;
    case Op::VariantField: op = b_.create<VariantFieldOp>(loc, rt, val(i.a), i.b, i.idx); break;
    case Op::Call:
      op = b_.create<CallOp>(loc, i.has_result ? TypeRange{rt} : TypeRange{},
                             FlatSymbolRefAttr::get(&ctx_, symName(m_, i.a)), vals(i.args));
      break;
    case Op::CallIndirect:
      op = b_.create<CallIndirectOp>(loc, i.has_result ? TypeRange{rt} : TypeRange{}, val(i.a), vals(i.args));
      break;
    case Op::Closure:
      op = b_.create<ClosureOp>(loc, rt, FlatSymbolRefAttr::get(&ctx_, symName(m_, i.a)), vals(i.args));
      break;
    }
    if (i.has_result) {
      op->setAttr("pg.v", b_.getI64IntegerAttr(i.result));
      define(i.result, op->getResult(0));
    }
  }

  void term(const pir::Term &t) {
    auto loc = UnknownLoc::get(&ctx_);
    switch (t.kind) {
    case pir::Term::Br: b_.create<BrOp>(loc, vals(t.t.args), blocks_.at(t.t.block)); break;
    case pir::Term::CondBr:
      b_.create<CondBrOp>(loc, val(t.v), vals(t.t.args), vals(t.f.args), blocks_.at(t.t.block),
                          blocks_.at(t.f.block));
      break;
    case pir::Term::Switch: {
      SmallVector<Attribute> keys;
      SmallVector<Block *> dests;
      for (auto &c : t.cases) {
        keys.push_back(b_.getIntegerAttr(b_.getIntegerType(128), apint(c.first)));
        dests.push_back(blocks_.at(c.second));
      }
      b_.create<SwitchOp>(loc, val(t.v), b_.getArrayAttr(keys), blocks_.at(t.dflt), dests);
      break;
    }
    case pir::Term::Ret: b_.create<RetOp>(loc, val(t.v)); break;
    case pir::Term::Trap: b_.create<TrapOp>(loc, t.msg); break;
    case pir::Term::Unreachable: b_.create<UnreachableOp>(loc); break;
    }
  }
};

//===----------------------------------------------------------------------===//
// Export
//===----------------------------------------------------------------------===//

struct ExportError {
  std::string msg;
};

class Exporter {
public:
  explicit Exporter(ModuleOp mod) : mod_(mod) {}

  pir::Module run() {
    pir::Module m;
    // Module metadata is not covered by mlir::verify: every attribute is
    // checked here and malformed metadata is an export error, never repaired.
    auto minor = need<IntegerAttr>(mod_->getAttr("pg.pirb_minor"), "module attribute pg.pirb_minor");
    if (minor.getValue().isNegative() || minor.getValue().getActiveBits() > 16)
      fail("pg.pirb_minor is out of range for a PIRB minor version");
    m.minor = static_cast<std::uint16_t>(minor.getValue().getZExtValue());
    m.flags = 1;
    auto structs = need<ArrayAttr>(mod_->getAttr("pg.structs"), "module attribute pg.structs");
    auto enums = need<ArrayAttr>(mod_->getAttr("pg.enums"), "module attribute pg.enums");
    structCount_ = structs.size();
    enumCount_ = enums.size();
    for (size_t k = 0; k < structs.size(); ++k) {
      std::string where = "pg.structs[" + std::to_string(k) + "]";
      auto d = need<DictionaryAttr>(structs[k], where);
      pir::StructDef sd;
      sd.name = need<StringAttr>(d.get("name"), where + ".name").str();
      auto names = need<ArrayAttr>(d.get("field_names"), where + ".field_names");
      auto tys = need<ArrayAttr>(d.get("field_types"), where + ".field_types");
      if (names.size() != tys.size()) fail(where + ": field_names and field_types differ in length");
      for (size_t f = 0; f < names.size(); ++f)
        sd.fields.emplace_back(need<StringAttr>(names[f], where + ".field_names").str(),
                               type(need<TypeAttr>(tys[f], where + ".field_types").getValue()));
      m.structs.push_back(sd);
    }
    for (size_t k = 0; k < enums.size(); ++k) {
      std::string where = "pg.enums[" + std::to_string(k) + "]";
      auto d = need<DictionaryAttr>(enums[k], where);
      pir::EnumDef ed;
      ed.name = need<StringAttr>(d.get("name"), where + ".name").str();
      auto names = need<ArrayAttr>(d.get("variant_names"), where + ".variant_names");
      auto tys = need<ArrayAttr>(d.get("variant_types"), where + ".variant_types");
      if (names.size() != tys.size()) fail(where + ": variant_names and variant_types differ in length");
      for (size_t v = 0; v < names.size(); ++v) {
        std::vector<pir::TypeP> vt;
        for (Attribute t : need<ArrayAttr>(tys[v], where + ".variant_types"))
          vt.push_back(type(need<TypeAttr>(t, where + ".variant_types").getValue()));
        ed.variants.emplace_back(need<StringAttr>(names[v], where + ".variant_names").str(), vt);
      }
      m.enums.push_back(ed);
    }
    std::map<std::string, pir::u32> ids;
    for (Operation &op : *mod_.getBody()) {
      auto fn = dyn_cast<func::FuncOp>(op);
      if (!fn) fail(("the module contains a " + op.getName().getStringRef() + "; only func.func is exported").str());
      ids[fn.getSymName().str()] = static_cast<pir::u32>(ids.size());
    }
    funcIds_ = &ids;
    if (Attribute ea = mod_->getAttr("pg.entry")) {
      m.has_entry = true;
      m.entry = funcId(need<FlatSymbolRefAttr>(ea, "module attribute pg.entry"));
    }
    for (auto fn : mod_.getOps<func::FuncOp>()) m.funcs.push_back(func(fn));
    if (m.has_entry && (!m.funcs[m.entry].defined || !m.funcs[m.entry].params.empty()))
      fail("pg.entry is not a defined function without parameters");
    return m;
  }

private:
  ModuleOp mod_;
  size_t structCount_ = 0, enumCount_ = 0;
  std::map<std::string, pir::u32> *funcIds_ = nullptr;
  std::map<Block *, pir::u32> blockIds_;
  std::map<void *, pir::u32> vids_;
  std::map<void *, pir::u32> slotIds_;

  [[noreturn]] static void fail(const std::string &m) { throw ExportError{m}; }

  template <class A>
  static A attr(Operation *op, StringRef name) {
    Attribute a = op->getAttr(name);
    if (!a) fail(("missing attribute " + name).str());
    return need<A>(a, ("attribute " + name).str());
  }
  // `a` as an A, or an export error naming `what`.
  template <class A>
  static A need(Attribute a, const std::string &what) {
    if (!a) fail("missing " + what);
    auto x = dyn_cast<A>(a);
    if (!x) fail("malformed " + what);
    return x;
  }
  // A PIR identifier (u32) stored in an i64 attribute.
  static pir::u32 u32of(int64_t v, const std::string &what) {
    if (v < 0 || v > static_cast<int64_t>(UINT32_MAX)) fail(what + " " + std::to_string(v) + " is not a 32-bit id");
    return static_cast<pir::u32>(v);
  }

  pir::u32 funcId(FlatSymbolRefAttr s) {
    auto it = funcIds_->find(s.getValue().str());
    if (it == funcIds_->end()) fail("unknown function @" + s.getValue().str());
    return it->second;
  }

  pir::TypeP type(Type t) {
    auto out = std::make_shared<pir::Type>();
    if (auto x = dyn_cast<IntType>(t)) {
      out->kind = pir::Type::Int;
      out->ity = static_cast<pir::IntTy>(x.getKind());
    } else if (auto x = dyn_cast<FloatType>(t)) {
      out->kind = pir::Type::Float;
      out->fty = static_cast<pir::FloatTy>(x.getKind());
    } else if (isa<BoolType>(t)) {
      out->kind = pir::Type::Bool;
    } else if (isa<CharType>(t)) {
      out->kind = pir::Type::Char;
    } else if (isa<StrType>(t)) {
      out->kind = pir::Type::Str;
    } else if (isa<RegionType>(t)) {
      out->kind = pir::Type::Region;
    } else if (auto x = dyn_cast<TupleType>(t)) {
      out->kind = pir::Type::Tuple;
      for (Type e : x.getElems()) out->elems.push_back(type(e));
    } else if (auto x = dyn_cast<ArrayType>(t)) {
      out->kind = pir::Type::Array;
      out->inner = type(x.getElem());
      out->len = x.getLen();
    } else if (auto x = dyn_cast<StructType>(t)) {
      out->kind = pir::Type::Struct;
      out->id = x.getId();
      if (out->id >= structCount_) fail("!pg.struct<" + std::to_string(out->id) + "> has no definition in pg.structs");
    } else if (auto x = dyn_cast<EnumType>(t)) {
      out->kind = pir::Type::Enum;
      out->id = x.getId();
      if (out->id >= enumCount_) fail("!pg.enum<" + std::to_string(out->id) + "> has no definition in pg.enums");
    } else if (auto x = dyn_cast<FnType>(t)) {
      out->kind = pir::Type::Fn;
      for (Type e : x.getParams()) out->elems.push_back(type(e));
      out->inner = type(x.getResult());
    } else if (auto x = dyn_cast<PtrType>(t)) {
      out->kind = pir::Type::Ptr;
      out->mut_ = x.getExclusive();
      out->inner = type(x.getPointee());
    } else if (auto x = dyn_cast<RefType>(t)) {
      out->kind = pir::Type::Ref;
      out->mut_ = x.getExclusive();
      out->inner = type(x.getPointee());
    } else {
      std::string s;
      llvm::raw_string_ostream os(s);
      os << t;
      fail("not a Pegasus type: " + os.str());
    }
    return out;
  }

  pir::u32 vid(Value v) {
    auto it = vids_.find(v.getAsOpaquePointer());
    if (it == vids_.end()) fail("operand without a PIR value id");
    return it->second;
  }
  std::vector<pir::u32> vids(ValueRange vs) {
    std::vector<pir::u32> out;
    for (Value v : vs) out.push_back(vid(v));
    return out;
  }
  pir::u32 slot(Value v) {
    auto it = slotIds_.find(v.getAsOpaquePointer());
    if (it == slotIds_.end()) fail("slot operand is not a pg.slot of this function");
    return it->second;
  }
  std::vector<pir::Proj> path(ArrayRef<int64_t> steps, ValueRange idx) {
    std::vector<pir::Proj> out;
    size_t next = 0;
    for (int64_t s : steps) {
      if (s == -1) {
        if (next >= idx.size()) fail("path has more index steps than operands");
        out.push_back({true, vid(idx[next++])});
      } else {
        out.push_back({false, u32of(s, "path field")});
      }
    }
    return out;
  }
  pir::Edge edge(Block *b, ValueRange args) {
    auto it = blockIds_.find(b);
    if (it == blockIds_.end()) fail("branch to an unknown block");
    return {it->second, vids(args)};
  }

  pir::Func func(func::FuncOp fn) {
    pir::Func f;
    f.name = attr<StringAttr>(fn, "pg.name").str();
    for (Type t : fn.getFunctionType().getInputs()) f.params.push_back(type(t));
    if (fn.getFunctionType().getNumResults() != 1) fail("@" + f.name + ": a function has exactly one result");
    f.ret = type(fn.getFunctionType().getResult(0));
    if (fn.isDeclaration()) {
      f.defined = false;
      f.abi = attr<StringAttr>(fn, "pg.abi").str();
      return f;
    }
    blockIds_.clear();
    vids_.clear();
    slotIds_.clear();
    bool trampoline = fn->hasAttr("pg.entry_trampoline");
    if (trampoline) checkTrampoline(fn, f.name);
    std::vector<Block *> blocks;
    for (Block &b : fn.getBody())
      if (!(trampoline && &b == &fn.getBody().front())) blocks.push_back(&b);
    for (size_t k = 0; k < blocks.size(); ++k) blockIds_[blocks[k]] = static_cast<pir::u32>(k);
    auto bparams = attr<ArrayAttr>(fn, "pg.block_params");
    if (bparams.size() != blocks.size()) fail("@" + f.name + ": pg.block_params does not match the blocks");
    // Value ids and types.
    std::map<pir::u32, Type> types;
    auto def = [&](Value v, int64_t id) {
      pir::u32 vid = u32of(id, "@" + f.name + ": value id");
      if (!types.emplace(vid, v.getType()).second) fail("@" + f.name + ": value id " + std::to_string(vid) + " defined twice");
      vids_[v.getAsOpaquePointer()] = vid;
    };
    Block *entry = &fn.getBody().front();
    for (size_t k = 0; k < blocks.size(); ++k) {
      auto ids = need<DenseI64ArrayAttr>(bparams[k], "@" + f.name + ": pg.block_params entry").asArrayRef();
      if (ids.size() != blocks[k]->getNumArguments()) fail("@" + f.name + ": block parameter ids do not match");
      for (size_t a = 0; a < ids.size(); ++a) def(blocks[k]->getArgument(a), ids[a]);
    }
    for (Block &b : fn.getBody())
      for (Operation &op : b) {
        if (auto s = dyn_cast<SlotOp>(op)) {
          if (&b != entry) fail("@" + f.name + ": pg.slot outside the entry block");
          slotIds_[s.getSlot().getAsOpaquePointer()] = static_cast<pir::u32>(f.slots.size());
          f.slots.push_back(type(cast<SlotType>(s.getType()).getElem()));
          f.slot_names.push_back(s.getName() ? s.getName()->str() : std::string());
          continue;
        }
        if (op.getNumResults() > 1) fail("@" + f.name + ": an operation with several results");
        if (op.getNumResults() == 1) def(op.getResult(0), attr<IntegerAttr>(&op, "pg.v").getInt());
      }
    if (!fn->hasAttr("pg.named_slots")) f.slot_names.clear();
    for (pir::u32 k = 0; k < types.size(); ++k) {
      auto it = types.find(k);
      if (it == types.end()) fail("@" + f.name + ": value ids are not contiguous");
      f.values.push_back(type(it->second));
    }
    for (Block *b : blocks) {
      pir::Block pb;
      auto ids = cast<DenseI64ArrayAttr>(bparams[blockIds_[b]]).asArrayRef();  // checked above
      for (int64_t id : ids) pb.params.push_back(u32of(id, "@" + f.name + ": block parameter id"));
      for (Operation &op : *b) {
        if (isa<SlotOp>(op)) continue;
        if (op.hasTrait<OpTrait::IsTerminator>()) {
          pb.term = term(&op);
          break;
        }
        pb.insts.push_back(inst(&op));
      }
      f.blocks.push_back(pb);
    }
    return f;
  }

  // The entry block may be dropped only if it is exactly what the importer
  // makes: pg.slot declarations, then one pg.br to the next block that
  // forwards the function's arguments unchanged, in order.
  static void checkTrampoline(func::FuncOp fn, const std::string &name) {
    Region &body = fn.getBody();
    if (body.getBlocks().size() < 2) fail("@" + name + ": pg.entry_trampoline without a block to branch to");
    Block &entry = body.front();
    Block *target = entry.getNextNode();
    auto bad = [&](const std::string &why) { fail("@" + name + ": not an importer entry trampoline: " + why); };
    for (Operation &op : entry) {
      if (isa<SlotOp>(op)) continue;
      auto br = dyn_cast<BrOp>(op);
      if (!br) bad("contains " + op.getName().getStringRef().str());
      if (br.getDest() != target) bad("does not branch to the next block");
      if (br.getArgs().size() != entry.getNumArguments()) bad("does not forward exactly the function's arguments");
      for (unsigned k = 0; k < entry.getNumArguments(); ++k)
        if (br.getArgs()[k] != entry.getArgument(k)) bad("does not forward the function's arguments in order");
      return;
    }
    bad("has no branch");
  }

  template <size_t N>
  static unsigned index(const char *const (&names)[N], StringRef s) {
    for (size_t k = 0; k < N; ++k)
      if (s == names[k]) return static_cast<unsigned>(k);
    fail(("unknown operation kind '" + s + "'").str());
  }

  pir::Inst inst(Operation *op) {
    using pir::Op;
    pir::Inst i;
    if (op->getNumResults() == 1) {
      i.has_result = true;
      i.result = vid(op->getResult(0));
    }
    if (auto o = dyn_cast<ConstOp>(op)) {
      i.op = Op::Const;
      Type t = o.getType();
      Attribute a = o.getValue();
      if (auto it = dyn_cast<IntType>(t)) {
        i.c.kind = pir::Const::Int;
        i.c.bits = u128(cast<IntegerAttr>(a).getValue());
        i.c.ity = static_cast<pir::IntTy>(it.getKind());
      } else if (auto ft = dyn_cast<FloatType>(t)) {
        i.c.kind = pir::Const::Float;
        i.c.fbits = cast<IntegerAttr>(a).getValue().getZExtValue();
        i.c.fty = static_cast<pir::FloatTy>(ft.getKind());
      } else if (isa<BoolType>(t)) {
        i.c.kind = pir::Const::Bool;
        i.c.b = cast<BoolAttr>(a).getValue();
      } else if (isa<CharType>(t)) {
        i.c.kind = pir::Const::Char;
        uint64_t ch = cast<IntegerAttr>(a).getValue().getZExtValue();
        if (ch > 0x10ffff || (ch >= 0xd800 && ch <= 0xdfff)) fail("char constant is not a Unicode scalar value");
        i.c.ch = static_cast<pir::u32>(ch);
      } else {
        i.c.kind = pir::Const::Str;
        i.c.s = cast<StringAttr>(a).str();
      }
    } else if (auto o = dyn_cast<BinOp>(op)) {
      i.op = Op::Bin;
      i.bop = static_cast<pir::BinOp>(index(kBin, o.getKind()));
      i.ov = static_cast<pir::Ov>(index(kOv, o.getOv()));
      i.a = vid(o.getLhs());
      i.b = vid(o.getRhs());
    } else if (auto o = dyn_cast<UnOp>(op)) {
      i.op = Op::Un;
      i.uop = static_cast<pir::UnOp>(index(kUn, o.getKind()));
      i.a = vid(o.getOperand());
    } else if (auto o = dyn_cast<CmpOp>(op)) {
      i.op = Op::Cmp;
      i.cop = static_cast<pir::CmpOp>(index(kCmp, o.getPred()));
      i.a = vid(o.getLhs());
      i.b = vid(o.getRhs());
    } else if (auto o = dyn_cast<CastOp>(op)) {
      i.op = Op::Cast;
      i.a = vid(o.getOperand());
      i.to = type(o.getType());
    } else if (auto o = dyn_cast<LoadOp>(op)) {
      i.op = Op::Load;
      i.a = slot(o.getSlot());
      i.path = path(o.getPath(), o.getIndices());
    } else if (auto o = dyn_cast<StoreOp>(op)) {
      i.op = Op::Store;
      i.a = slot(o.getSlot());
      i.path = path(o.getPath(), o.getIndices());
      i.b = vid(o.getValue());
    } else if (auto o = dyn_cast<MoveOp>(op)) {
      i.op = Op::Move;
      i.a = slot(o.getSlot());
      for (int64_t s : o.getPath()) i.args.push_back(u32of(s, "move path field"));
    } else if (auto o = dyn_cast<DropOp>(op)) {
      i.op = Op::Drop;
      i.a = slot(o.getSlot());
    } else if (auto o = dyn_cast<BorrowOp>(op)) {
      i.op = Op::Borrow;
      i.a = slot(o.getSlot());
      i.path = path(o.getPath(), o.getIndices());
      i.mut_ = o.getExclusive();
    } else if (auto o = dyn_cast<BorrowRefOp>(op)) {
      i.op = Op::BorrowRef;
      i.a = vid(o.getRef());
      i.path = path(o.getPath(), o.getIndices());
      i.mut_ = o.getExclusive();
    } else if (auto o = dyn_cast<LoadRefOp>(op)) {
      i.op = Op::LoadRef;
      i.a = vid(o.getRef());
      i.path = path(o.getPath(), o.getIndices());
    } else if (auto o = dyn_cast<StoreRefOp>(op)) {
      i.op = Op::StoreRef;
      i.a = vid(o.getRef());
      i.path = path(o.getPath(), o.getIndices());
      i.b = vid(o.getValue());
    } else if (auto o = dyn_cast<TupleOp>(op)) {
      i.op = Op::Tuple;
      i.args = vids(o.getElems());
    } else if (auto o = dyn_cast<ArrayOp>(op)) {
      i.op = Op::Array;
      i.args = vids(o.getElems());
    } else if (auto o = dyn_cast<StructOp>(op)) {
      i.op = Op::Struct;
      i.a = u32of(o.getId(), "struct id");
      i.args = vids(o.getFields());
    } else if (auto o = dyn_cast<VariantOp>(op)) {
      i.op = Op::Variant;
      i.a = u32of(o.getId(), "enum id");
      i.b = u32of(o.getVariant(), "variant index");
      i.args = vids(o.getFields());
    } else if (auto o = dyn_cast<RepeatOp>(op)) {
      i.op = Op::Repeat;
      i.a = vid(o.getValue());
      i.n = static_cast<pir::u64>(o.getCount());
    } else if (auto o = dyn_cast<ExtractOp>(op)) {
      i.op = Op::Extract;
      i.a = vid(o.getAggregate());
      i.idx = u32of(o.getIndex(), "field index");
    } else if (auto o = dyn_cast<ExtractIndexOp>(op)) {
      i.op = Op::ExtractIndex;
      i.a = vid(o.getArray());
      i.b = vid(o.getIndex());
    } else if (auto o = dyn_cast<TagOp>(op)) {
      i.op = Op::Tag;
      i.a = vid(o.getValue());
    } else if (auto o = dyn_cast<VariantFieldOp>(op)) {
      i.op = Op::VariantField;
      i.a = vid(o.getValue());
      i.b = u32of(o.getVariant(), "variant index");
      i.idx = u32of(o.getIndex(), "field index");
    } else if (auto o = dyn_cast<CallOp>(op)) {
      i.op = Op::Call;
      i.a = funcId(o.getCalleeAttr());
      i.args = vids(o.getArgs());
    } else if (auto o = dyn_cast<CallIndirectOp>(op)) {
      i.op = Op::CallIndirect;
      i.a = vid(o.getCallee());
      i.args = vids(o.getArgs());
    } else if (auto o = dyn_cast<ClosureOp>(op)) {
      i.op = Op::Closure;
      i.a = funcId(o.getCalleeAttr());
      i.args = vids(o.getCaptures());
    } else {
      fail(("not a Pegasus instruction: " + op->getName().getStringRef()).str());
    }
    return i;
  }

  pir::Term term(Operation *op) {
    pir::Term t;
    if (auto o = dyn_cast<BrOp>(op)) {
      t.kind = pir::Term::Br;
      t.t = edge(o.getDest(), o.getArgs());
    } else if (auto o = dyn_cast<CondBrOp>(op)) {
      t.kind = pir::Term::CondBr;
      t.v = vid(o.getCondition());
      t.t = edge(o.getTrueDest(), o.getTrueArgs());
      t.f = edge(o.getFalseDest(), o.getFalseArgs());
    } else if (auto o = dyn_cast<SwitchOp>(op)) {
      t.kind = pir::Term::Switch;
      t.v = vid(o.getValue());
      auto dests = o.getCaseDests();
      for (size_t k = 0; k < dests.size(); ++k)
        t.cases.emplace_back(u128(cast<IntegerAttr>(o.getCases()[k]).getValue()), edge(dests[k], {}).block);
      t.dflt = edge(o.getDefaultDest(), {}).block;
    } else if (auto o = dyn_cast<RetOp>(op)) {
      t.kind = pir::Term::Ret;
      t.v = vid(o.getValue());
    } else if (auto o = dyn_cast<TrapOp>(op)) {
      t.kind = pir::Term::Trap;
      t.msg = o.getMessage().str();
    } else if (isa<UnreachableOp>(op)) {
      t.kind = pir::Term::Unreachable;
    } else {
      fail(("not a Pegasus terminator: " + op->getName().getStringRef()).str());
    }
    return t;
  }
};

}  // namespace

OwningOpRef<ModuleOp> importModule(MLIRContext &ctx, const pir::Module &m) { return Importer(ctx, m).run(); }

bool exportModule(ModuleOp mod, pir::Module &out, std::string &error) {
  // Operation-level structure (types of operands, attribute kinds, block
  // arguments) is the verifier's; the exporter relies on it.
  if (failed(verify(mod))) {
    error = "the module does not verify";
    return false;
  }
  try {
    out = Exporter(mod).run();
    return true;
  } catch (const ExportError &e) {
    error = e.msg;
    return false;
  }
}

}  // namespace pegasus::pg
