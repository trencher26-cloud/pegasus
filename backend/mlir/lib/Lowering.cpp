// pg -> LLVM dialect (spec/mlir.md §5). A dialect conversion that reproduces
// backend/src/emit_llvm.cpp: same native subset (refused with the same
// messages), same integer semantics, traps, runtime calls, call-depth limit
// and result printing (spec/native.md §2-§4).
//
// Structure:
//  1. Reachability from the entry (only what the entry needs is lowered,
//     exactly as the direct emitter); unreachable functions are erased.
//  2. Support check: everything outside the native subset is refused before
//     any rewriting, so the conversion patterns never meet it.
//  3. Module-level declarations and helpers: the runtime, `pg_rt_depth`, the
//     check helpers, 128-bit division, `**`, and array fill loops.
//  4. applyFullConversion with one pattern per pg operation plus func.func.
//     Every pattern is a straight-line rewrite: run-time checks call small
//     internal `alwaysinline` helpers (`pg.check*`) that branch to the
//     runtime's noreturn trap, so no pattern edits the CFG; LLVM inlines them
//     back into exactly the direct emitter's check-and-branch.
//  5. The C entry point: `pg.program` calls the entry and prints its result;
//     `main` runs it on the runtime's large-stack thread.
//
// No exception is thrown through MLIR code: the analysis walks blocks with
// header-only iterators, and the patterns report failure instead.
#include "pegasus/mlir/Lowering.h"

#include "pegasus/mlir/PegasusDialect.h"

#include "mlir/Dialect/Func/IR/FuncOps.h"
#include "mlir/Dialect/LLVMIR/LLVMDialect.h"
#include "mlir/Dialect/LLVMIR/LLVMTypes.h"
#include "mlir/Dialect/LLVMIR/Transforms/LegalizeForExport.h"
#include "mlir/IR/Builders.h"
#include "mlir/IR/PatternMatch.h"
#include "mlir/IR/SymbolTable.h"
#include "mlir/Transforms/RegionUtils.h"
#include "mlir/Target/LLVMIR/Dialect/Builtin/BuiltinToLLVMIRTranslation.h"
#include "mlir/Target/LLVMIR/Dialect/LLVMIR/LLVMToLLVMIRTranslation.h"
#include "mlir/Target/LLVMIR/Export.h"
#include "mlir/Transforms/DialectConversion.h"
#include "llvm/IR/LLVMContext.h"
#include "llvm/IR/Module.h"
#include "llvm/Support/raw_ostream.h"

#include <map>
#include <set>
#include <stdexcept>
#include <vector>

namespace pegasus::pg {
using namespace mlir;

void registerLoweringDialects(MLIRContext &ctx) { ctx.getOrLoadDialect<LLVM::LLVMDialect>(); }

namespace {

struct Unsupported : std::runtime_error {
  using std::runtime_error::runtime_error;
};
struct Inconsistent : std::runtime_error {
  using std::runtime_error::runtime_error;
};

// The call-depth limit (the reference interpreter's `Limits::max_depth`).
constexpr uint64_t kMaxDepth = 100000;

struct StructDef {
  std::string name;
  std::vector<std::string> fieldNames;
  std::vector<Type> fieldTypes;
};

//===----------------------------------------------------------------------===//
// Module information
//===----------------------------------------------------------------------===//

struct Info {
  std::vector<StructDef> structs;
  std::vector<std::string> enumNames;

  explicit Info(ModuleOp mod) {
    if (auto arr = mod->getAttrOfType<ArrayAttr>("pg.structs"))
      for (Attribute a : arr) {
        auto d = dyn_cast<DictionaryAttr>(a);
        auto name = d ? d.getAs<StringAttr>("name") : StringAttr();
        auto names = d ? d.getAs<ArrayAttr>("field_names") : ArrayAttr();
        auto tys = d ? d.getAs<ArrayAttr>("field_types") : ArrayAttr();
        if (!name || !names || !tys || names.size() != tys.size()) throw Inconsistent("malformed pg.structs");
        StructDef s;
        s.name = name.str();
        for (Attribute n : names) {
          auto sn = dyn_cast<StringAttr>(n);
          if (!sn) throw Inconsistent("malformed pg.structs");
          s.fieldNames.push_back(sn.str());
        }
        for (Attribute t : tys) {
          auto ta = dyn_cast<TypeAttr>(t);
          if (!ta) throw Inconsistent("malformed pg.structs");
          s.fieldTypes.push_back(ta.getValue());
        }
        structs.push_back(std::move(s));
      }
    if (auto arr = mod->getAttrOfType<ArrayAttr>("pg.enums"))
      for (Attribute a : arr) {
        auto d = dyn_cast<DictionaryAttr>(a);
        auto name = d ? d.getAs<StringAttr>("name") : StringAttr();
        enumNames.push_back(name ? name.str() : std::string("?"));
      }
  }

  const StructDef &structDef(unsigned id) const {
    if (id >= structs.size()) throw Inconsistent("struct id out of range");
    return structs[id];
  }
  // Field types of a tuple or struct.
  std::vector<Type> fields(Type t) const {
    if (auto tt = dyn_cast<TupleType>(t)) return {tt.getElems().begin(), tt.getElems().end()};
    if (auto st = dyn_cast<StructType>(t)) return structDef(st.getId()).fieldTypes;
    throw Inconsistent("field of a non-aggregate");
  }
};

bool isUnit(Type t) {
  auto tt = dyn_cast<TupleType>(t);
  return tt && tt.getElems().empty();
}

//===----------------------------------------------------------------------===//
// 1-2. Reachability and the native subset (spec/native.md §2)
//===----------------------------------------------------------------------===//

class Analysis {
public:
  Analysis(ModuleOp mod, const Info &info) : mod_(mod), info_(info) {}

  // Functions reachable from the entry, in the direct emitter's order.
  std::vector<func::FuncOp> reach(func::FuncOp entry) {
    std::vector<func::FuncOp> order;
    std::set<Operation *> seen;
    std::vector<func::FuncOp> work{entry};
    while (!work.empty()) {
      func::FuncOp f = work.back();
      work.pop_back();
      if (!seen.insert(f).second) continue;
      order.push_back(f);
      if (f.isDeclaration()) {
        auto name = f->getAttrOfType<StringAttr>("pg.name");
        throw Unsupported("call of extern function `" + (name ? name.str() : f.getSymName().str()) + "`");
      }
      for (Block &b : f.getBody())
        for (Operation &op : b) {
          StringRef callee;
          if (auto c = dyn_cast<CallOp>(op)) callee = c.getCallee();
          else if (auto c = dyn_cast<ClosureOp>(op)) callee = c.getCallee();
          else continue;
          auto g = mod_.lookupSymbol<func::FuncOp>(callee);
          if (!g) throw Inconsistent("call of an unknown function");
          work.push_back(g);
        }
    }
    return order;
  }

  void type(Type t, int depth = 0) const {
    if (depth > 64) throw Unsupported("type nesting too deep");
    if (isa<IntType, BoolType>(t)) return;
    if (auto tt = dyn_cast<TupleType>(t)) {
      for (Type e : tt.getElems()) type(e, depth + 1);
      return;
    }
    if (auto st = dyn_cast<StructType>(t)) {
      for (Type e : info_.structDef(st.getId()).fieldTypes) type(e, depth + 1);
      return;
    }
    if (auto at = dyn_cast<ArrayType>(t)) return type(at.getElem(), depth + 1);
    if (auto rt = dyn_cast<RefType>(t)) return type(rt.getPointee(), depth + 1);
    if (auto sl = dyn_cast<SlotType>(t)) return type(sl.getElem(), depth);
    if (isa<FloatType>(t)) throw Unsupported("floating-point types");
    if (isa<CharType>(t)) throw Unsupported("`char`");
    if (isa<StrType>(t)) throw Unsupported("`str`");
    if (auto et = dyn_cast<EnumType>(t)) {
      unsigned id = et.getId();
      throw Unsupported("enum `" + (id < info_.enumNames.size() ? info_.enumNames[id] : std::string("?")) + "`");
    }
    if (isa<FnType>(t)) throw Unsupported("function values");
    if (isa<PtrType>(t)) throw Unsupported("raw pointers");
    if (isa<RegionType>(t)) throw Unsupported("regions");
    throw Unsupported("type");
  }

  // Structural equality is unrolled; the direct emitter bounds arrays.
  void equality(Type t) const {
    if (auto at = dyn_cast<ArrayType>(t)) {
      if (at.getLen() > 1024) throw Unsupported("comparison of arrays longer than 1024 elements");
      return equality(at.getElem());
    }
    if (isa<TupleType, StructType>(t))
      for (Type e : info_.fields(t)) equality(e);
    if (isa<RefType>(t)) throw Inconsistent("equality on references");
  }

  // The entry's result is printed in the interpreter's format.
  void printable(Type t) const {
    if (isa<IntType, BoolType>(t)) return;
    if (auto at = dyn_cast<ArrayType>(t)) {
      if (at.getLen() > 4096) throw Unsupported("printing arrays longer than 4096 elements");
      return printable(at.getElem());
    }
    if (isa<TupleType, StructType>(t)) {
      for (Type e : info_.fields(t)) printable(e);
      return;
    }
    throw Unsupported("printing a result of this type");
  }

  void func(func::FuncOp f) const {
    for (Type t : f.getFunctionType().getInputs()) type(t);
    for (Type t : f.getFunctionType().getResults()) type(t);
    for (Block &b : f.getBody()) {
      for (BlockArgument a : b.getArguments()) type(a.getType());
      for (Operation &op : b) {
        if (auto c = dyn_cast<ConstOp>(op)) {
          Attribute v = c.getValue();
          if (isa<StringAttr>(v)) throw Unsupported("string constants");
          if (isa<CharType>(c.getType())) throw Unsupported("`char` constants");
          if (isa<FloatType>(c.getType())) throw Unsupported("floating-point constants");
        }
        if (isa<CallIndirectOp, ClosureOp>(op)) throw Unsupported("function values and closures");
        if (isa<VariantOp, TagOp, VariantFieldOp>(op)) throw Unsupported("enums");
        if (auto c = dyn_cast<pg::CmpOp>(op)) {
          StringRef p = c.getPred();
          if ((p == "eq" || p == "ne") && !isa<IntType, BoolType>(c.getLhs().getType()))
            equality(c.getLhs().getType());
          if (p != "eq" && p != "ne" && isa<BoolType>(c.getLhs().getType()))
            throw Inconsistent("ordering comparison on bool");
        }
        if (auto s = dyn_cast<SwitchOp>(op))
          if (!isa<IntType, BoolType>(s.getValue().getType())) throw Unsupported("switch on this type");
        for (Value v : op.getOperands()) type(v.getType());
        for (Value v : op.getResults()) type(v.getType());
      }
    }
  }

private:
  ModuleOp mod_;
  const Info &info_;
};

//===----------------------------------------------------------------------===//
// Types
//===----------------------------------------------------------------------===//

class PgTypeConverter : public TypeConverter {
public:
  PgTypeConverter(MLIRContext *ctx, const Info &info) {
    addConversion([](Type t) -> std::optional<Type> {
      if (LLVM::isCompatibleType(t)) return t;
      return std::nullopt;
    });
    addConversion([ctx](IntType t) -> Type { return IntegerType::get(ctx, t.getBits()); });
    addConversion([ctx](BoolType) -> Type { return IntegerType::get(ctx, 1); });
    addConversion([ctx](RefType) -> Type { return LLVM::LLVMPointerType::get(ctx); });
    addConversion([ctx](SlotType) -> Type { return LLVM::LLVMPointerType::get(ctx); });
    addConversion([this](ArrayType t) -> std::optional<Type> {
      Type e = convertType(t.getElem());
      if (!e) return std::nullopt;
      return LLVM::LLVMArrayType::get(e, t.getLen());
    });
    addConversion([this, ctx](TupleType t) -> std::optional<Type> { return literal(ctx, t.getElems()); });
    addConversion([this, ctx, &info](StructType t) -> std::optional<Type> {
      if (t.getId() >= info.structs.size()) return std::nullopt;
      return literal(ctx, info.structs[t.getId()].fieldTypes);
    });
  }

private:
  std::optional<Type> literal(MLIRContext *ctx, ArrayRef<Type> elems) {
    SmallVector<Type> out;
    for (Type e : elems) {
      Type c = convertType(e);
      if (!c) return std::nullopt;
      out.push_back(c);
    }
    return LLVM::LLVMStructType::getLiteral(ctx, out);
  }
};

//===----------------------------------------------------------------------===//
// 3. The module library: runtime, strings, helpers
//===----------------------------------------------------------------------===//

const char *verb(StringRef k) {
  if (k == "add") return "add";
  if (k == "sub") return "subtract";
  if (k == "mul") return "multiply";
  if (k == "div") return "divide";
  return "calculate the remainder";
}

APInt minSigned(unsigned w) { return APInt::getSignedMinValue(w); }
APInt maxSigned(unsigned w) { return APInt::getSignedMaxValue(w); }

class Lib {
public:
  Lib(ModuleOp mod, const Info &info, const TypeConverter &tc)
      : mod_(mod), ctx_(mod.getContext()), info_(info), tc_(tc) {}

  const Info &info() const { return info_; }
  MLIRContext *ctx() const { return ctx_; }
  Type conv(Type t) const { return tc_.convertType(t); }
  Type ptr() const { return LLVM::LLVMPointerType::get(ctx_); }
  Type i(unsigned w) const { return IntegerType::get(ctx_, w); }
  Type voidTy() const { return LLVM::LLVMVoidType::get(ctx_); }

  Value cint(OpBuilder &b, Location l, unsigned w, const APInt &v) const {
    return b.create<LLVM::ConstantOp>(l, i(w), b.getIntegerAttr(i(w), v.zextOrTrunc(w)));
  }
  Value cint(OpBuilder &b, Location l, unsigned w, uint64_t v) const { return cint(b, l, w, APInt(w, v)); }
  Value ones(OpBuilder &b, Location l, unsigned w) const { return cint(b, l, w, APInt::getAllOnes(w)); }
  Value ctrue(OpBuilder &b, Location l) const { return cint(b, l, 1, 1); }

  // A NUL-terminated private constant, one per distinct text.
  Value str(OpBuilder &b, Location l, StringRef s) {
    auto it = strs_.find(s.str());
    std::string name;
    if (it != strs_.end()) {
      name = it->second;
    } else {
      name = ".s." + std::to_string(strs_.size());
      OpBuilder::InsertionGuard g(b);
      b.setInsertionPointToStart(mod_.getBody());
      std::string text = s.str();
      text.push_back('\0');
      auto ty = LLVM::LLVMArrayType::get(i(8), text.size());
      auto glob = b.create<LLVM::GlobalOp>(l, ty, /*isConstant=*/true, LLVM::Linkage::Private, name,
                                           b.getStringAttr(text), /*alignment=*/0);
      glob.setUnnamedAddr(LLVM::UnnamedAddr::Global);
      strs_[s.str()] = name;
    }
    return b.create<LLVM::AddressOfOp>(l, ptr(), name);
  }

  void call(OpBuilder &b, Location l, StringRef fn, ValueRange args) const {
    b.create<LLVM::CallOp>(l, TypeRange{}, fn, args);
  }
  Value call1(OpBuilder &b, Location l, Type ret, StringRef fn, ValueRange args) const {
    return b.create<LLVM::CallOp>(l, TypeRange{ret}, fn, args).getResult();
  }

  // Trap with `msg` unless `ok` holds.
  void check(OpBuilder &b, Location l, Value ok, StringRef msg) { call(b, l, "pg.check", {ok, str(b, l, msg)}); }

  // Split an integer (zero- or sign-extended to i128) into i64 halves.
  std::pair<Value, Value> halves(OpBuilder &b, Location l, Value v, unsigned w, bool sign) const {
    Value wide = v;
    if (w < 128)
      wide = sign ? b.create<LLVM::SExtOp>(l, i(128), v).getResult() : b.create<LLVM::ZExtOp>(l, i(128), v).getResult();
    Value lo = b.create<LLVM::TruncOp>(l, i(64), wide);
    Value sh = b.create<LLVM::LShrOp>(l, wide, cint(b, l, 128, 64));
    Value hi = b.create<LLVM::TruncOp>(l, i(64), sh);
    return {lo, hi};
  }

  // An integer as i64 for indexing, after a bounds check against len.
  Value index(OpBuilder &b, Location l, Value v, unsigned w, uint64_t len) const {
    Value wide = w < 128 ? b.create<LLVM::ZExtOp>(l, i(128), v).getResult() : v;
    Value ok = b.create<LLVM::ICmpOp>(l, LLVM::ICmpPredicate::ult, wide, cint(b, l, 128, len));
    auto [lo, hi] = halves(b, l, wide, 128, false);
    call(b, l, "pg.check_index", {ok, cint(b, l, 64, len), lo, hi});
    return b.create<LLVM::TruncOp>(l, i(64), wide);
  }

  // Pointer to the place base.path, where base points to a value of pg type
  // t; returns null on a path that does not fit (the verifier excludes it).
  std::pair<Value, Type> place(OpBuilder &b, Location l, Value base, Type t, ArrayRef<int64_t> path,
                               ValueRange idx, TypeRange idxTys) const {
    size_t k = 0;
    for (int64_t step : path) {
      Type lt = conv(t);
      if (!lt) return {};
      if (step >= 0) {
        std::vector<Type> fs;
        if (auto tt = dyn_cast<TupleType>(t)) fs.assign(tt.getElems().begin(), tt.getElems().end());
        else if (auto st = dyn_cast<StructType>(t); st && st.getId() < info_.structs.size())
          fs = info_.structs[st.getId()].fieldTypes;
        if (static_cast<uint64_t>(step) >= fs.size()) return {};
        base = b.create<LLVM::GEPOp>(l, ptr(), lt, base, ArrayRef<LLVM::GEPArg>{0, static_cast<int32_t>(step)},
                                     /*inbounds=*/true);
        t = fs[step];
      } else {
        auto at = dyn_cast<ArrayType>(t);
        auto it = k < idxTys.size() ? dyn_cast<IntType>(idxTys[k]) : IntType();
        if (!at || !it) return {};
        Value n = index(b, l, idx[k], it.getBits(), at.getLen());
        ++k;
        base = b.create<LLVM::GEPOp>(l, ptr(), lt, base, ArrayRef<LLVM::GEPArg>{0, n}, /*inbounds=*/true);
        t = at.getElem();
      }
    }
    return {base, t};
  }

  // A temporary in the entry block of op's (already converted) function.
  Value entryAlloca(OpBuilder &b, Operation *op, Type lt) const {
    auto fn = op->getParentOfType<LLVM::LLVMFuncOp>();
    if (!fn) return {};
    OpBuilder::InsertionGuard g(b);
    b.setInsertionPointToStart(&fn.getBody().front());
    Value one = cint(b, op->getLoc(), 64, 1);
    return b.create<LLVM::AllocaOp>(op->getLoc(), ptr(), lt, one, 0);
  }

  // Structural equality of two values of pg type t (an i1).
  Value equal(OpBuilder &b, Location l, Value x, Value y, Type t) const {
    if (isa<IntType, BoolType>(t)) return b.create<LLVM::ICmpOp>(l, LLVM::ICmpPredicate::eq, x, y);
    std::vector<Type> fs;
    if (auto at = dyn_cast<ArrayType>(t)) fs.assign(at.getLen(), at.getElem());
    else fs = info_.fields(t);
    Value acc = ctrue(b, l);
    for (size_t k = 0; k < fs.size(); ++k) {
      Value a = b.create<LLVM::ExtractValueOp>(l, x, ArrayRef<int64_t>{static_cast<int64_t>(k)});
      Value c = b.create<LLVM::ExtractValueOp>(l, y, ArrayRef<int64_t>{static_cast<int64_t>(k)});
      acc = b.create<LLVM::AndOp>(l, acc, equal(b, l, a, c, fs[k]));
    }
    return acc;
  }

  // Build an aggregate of LLVM type lt from elements.
  Value build(OpBuilder &b, Location l, Type lt, ValueRange elems) const {
    if (elems.empty()) return b.create<LLVM::ZeroOp>(l, lt);
    Value acc = b.create<LLVM::PoisonOp>(l, lt);
    for (size_t k = 0; k < elems.size(); ++k)
      acc = b.create<LLVM::InsertValueOp>(l, acc, elems[k], ArrayRef<int64_t>{static_cast<int64_t>(k)});
    return acc;
  }

  // ---- module-level declarations and helpers (before the conversion)

  void declareRuntime(OpBuilder &b, Location l) {
    auto decl = [&](StringRef name, Type ret, ArrayRef<Type> args, bool noreturn) {
      auto f = b.create<LLVM::LLVMFuncOp>(l, name, LLVM::LLVMFunctionType::get(ret, args), LLVM::Linkage::External);
      SmallVector<Attribute> pass;
      if (noreturn) pass.push_back(b.getStringAttr("noreturn"));
      pass.push_back(b.getStringAttr("nounwind"));
      f.setPassthroughAttr(b.getArrayAttr(pass));
    };
    decl("pg_rt_trap", voidTy(), {ptr()}, true);
    decl("pg_rt_trap_shift", voidTy(), {i(64), i(64), ptr(), i(32)}, true);
    decl("pg_rt_trap_index", voidTy(), {i(64), i(64), i(64)}, true);
    decl("pg_rt_print_int", voidTy(), {i(64), i(64), i(32)}, false);
    decl("pg_rt_print_str", voidTy(), {ptr()}, false);
    decl("pg_rt_run", voidTy(), {ptr()}, false);
    b.create<LLVM::GlobalOp>(l, i(64), /*isConstant=*/false, LLVM::Linkage::External, "pg_rt_depth", Attribute());
    // pg.check*(ok, ...): return when ok, otherwise call the runtime trap.
    auto checker = [&](StringRef name, StringRef rt, ArrayRef<Type> rest) {
      SmallVector<Type> args{i(1)};
      args.append(rest.begin(), rest.end());
      auto f = internal(b, l, name, voidTy(), args);
      f.setPassthroughAttr(b.getArrayAttr({b.getStringAttr("alwaysinline")}));
      OpBuilder::InsertionGuard g(b);
      Block *entry = b.createBlock(&f.getBody(), {}, args, SmallVector<Location>(args.size(), l));
      Block *bad = b.createBlock(&f.getBody());
      Block *ok = b.createBlock(&f.getBody());
      b.setInsertionPointToEnd(entry);
      b.create<LLVM::CondBrOp>(l, entry->getArgument(0), ok, bad);
      b.setInsertionPointToEnd(bad);
      call(b, l, rt, entry->getArguments().drop_front());
      b.create<LLVM::UnreachableOp>(l);
      b.setInsertionPointToEnd(ok);
      b.create<LLVM::ReturnOp>(l, ValueRange{});
    };
    checker("pg.check", "pg_rt_trap", {ptr()});
    checker("pg.check_index", "pg_rt_trap_index", {i(64), i(64), i(64)});
    checker("pg.check_shift", "pg_rt_trap_shift", {i(64), i(64), ptr(), i(32)});
  }

  // An internal function, always created at module scope (callers may be
  // positioned inside a function body).
  LLVM::LLVMFuncOp internal(OpBuilder &b, Location l, StringRef name, Type ret, ArrayRef<Type> args) {
    OpBuilder::InsertionGuard g(b);
    b.setInsertionPointToEnd(mod_.getBody());
    return b.create<LLVM::LLVMFuncOp>(l, name, LLVM::LLVMFunctionType::get(ret, args), LLVM::Linkage::Internal);
  }

  // 128-bit udiv/urem/sdiv/srem in IR (spec/native.md §3): restoring
  // shift-subtract division, the direct emitter's helper block for block.
  std::string div128(OpBuilder &b, Location l, StringRef op) {
    std::string nm = "pg." + op.str() + "128";
    if (!helpers_.insert(nm).second) return nm;
    Type T = i(128), pair = LLVM::LLVMStructType::getLiteral(ctx_, {T, T});
    if (helpers_.insert("pg.udivmod128").second) {
      auto f = internal(b, l, "pg.udivmod128", pair, {T, T});
      OpBuilder::InsertionGuard g(b);
      Block *entry = b.createBlock(&f.getBody(), {}, {T, T}, {l, l});
      Block *loop = b.createBlock(&f.getBody(), {}, {i(32), T, T}, {l, l, l});
      Block *out = b.createBlock(&f.getBody());
      Value n = entry->getArgument(0), d = entry->getArgument(1);
      b.setInsertionPointToEnd(entry);
      b.create<LLVM::BrOp>(l, ValueRange{cint(b, l, 32, 127), cint(b, l, 128, 0), cint(b, l, 128, 0)}, loop);
      b.setInsertionPointToEnd(loop);
      Value iv = loop->getArgument(0), q = loop->getArgument(1), r = loop->getArgument(2);
      Value sh = b.create<LLVM::ZExtOp>(l, T, iv);
      Value bit = b.create<LLVM::LShrOp>(l, n, sh);
      Value b1 = b.create<LLVM::AndOp>(l, bit, cint(b, l, 128, 1));
      // 2r + bit can exceed 128 bits when d > 2^127: the carry out of the
      // shift then means the true value is >= 2^128 > d.
      Value carry = b.create<LLVM::ICmpOp>(l, LLVM::ICmpPredicate::slt, r, cint(b, l, 128, 0));
      Value r2 = b.create<LLVM::ShlOp>(l, r, cint(b, l, 128, 1));
      Value r3 = b.create<LLVM::OrOp>(l, r2, b1);
      Value ge0 = b.create<LLVM::ICmpOp>(l, LLVM::ICmpPredicate::uge, r3, d);
      Value ge = b.create<LLVM::OrOp>(l, carry, ge0);
      Value r4 = b.create<LLVM::SubOp>(l, r3, d);
      Value rn = b.create<LLVM::SelectOp>(l, ge, r4, r3);
      Value qbit = b.create<LLVM::ShlOp>(l, cint(b, l, 128, 1), sh);
      Value q2 = b.create<LLVM::OrOp>(l, q, qbit);
      Value qn = b.create<LLVM::SelectOp>(l, ge, q2, q);
      Value in = b.create<LLVM::SubOp>(l, iv, cint(b, l, 32, 1));
      Value done = b.create<LLVM::ICmpOp>(l, LLVM::ICmpPredicate::eq, iv, cint(b, l, 32, 0));
      b.create<LLVM::CondBrOp>(l, done, out, ValueRange{}, loop, ValueRange{in, qn, rn});
      b.setInsertionPointToEnd(out);
      Value res = b.create<LLVM::InsertValueOp>(l, b.create<LLVM::PoisonOp>(l, pair), qn, ArrayRef<int64_t>{0});
      res = b.create<LLVM::InsertValueOp>(l, res, rn, ArrayRef<int64_t>{1});
      b.create<LLVM::ReturnOp>(l, res);
    }
    auto f = internal(b, l, nm, T, {T, T});
    OpBuilder::InsertionGuard g(b);
    Block *entry = b.createBlock(&f.getBody(), {}, {T, T}, {l, l});
    Value a = entry->getArgument(0), d = entry->getArgument(1), v;
    bool sgn = op[0] == 's', quot = op.contains("div");
    if (sgn) {
      // |x| as unsigned (|MIN| = 2^127 is exact); the quotient is negative
      // when the signs differ, the remainder takes the dividend's sign.
      Value zero = cint(b, l, 128, 0);
      Value an = b.create<LLVM::ICmpOp>(l, LLVM::ICmpPredicate::slt, a, zero);
      Value bn = b.create<LLVM::ICmpOp>(l, LLVM::ICmpPredicate::slt, d, zero);
      Value ua = b.create<LLVM::SelectOp>(l, an, b.create<LLVM::SubOp>(l, zero, a), a);
      Value ub = b.create<LLVM::SelectOp>(l, bn, b.create<LLVM::SubOp>(l, zero, d), d);
      Value qr = call1(b, l, pair, "pg.udivmod128", {ua, ub});
      if (quot) {
        Value uq = b.create<LLVM::ExtractValueOp>(l, qr, ArrayRef<int64_t>{0});
        Value neg = b.create<LLVM::XOrOp>(l, an, bn);
        v = b.create<LLVM::SelectOp>(l, neg, b.create<LLVM::SubOp>(l, zero, uq), uq);
      } else {
        Value ur = b.create<LLVM::ExtractValueOp>(l, qr, ArrayRef<int64_t>{1});
        v = b.create<LLVM::SelectOp>(l, an, b.create<LLVM::SubOp>(l, zero, ur), ur);
      }
    } else {
      Value qr = call1(b, l, pair, "pg.udivmod128", {a, d});
      v = b.create<LLVM::ExtractValueOp>(l, qr, ArrayRef<int64_t>{quot ? 0 : 1});
    }
    b.create<LLVM::ReturnOp>(l, v);
    return nm;
  }

  // `x ** y` with the interpreter's semantics (the direct emitter's helper):
  // an exponent beyond u32 traps unless the base is an unsigned 0 or 1;
  // otherwise the exact power must fit. 0, 1 and -1 are answered directly,
  // and |x| >= 2 overflows within the type's width, bounding the loop.
  std::string pow(OpBuilder &b, Location l, IntType t, IntType e) {
    std::string nm = "pg.pow." + t.getKindName().str() + "." + e.getKindName().str();
    if (!helpers_.insert(nm).second) return nm;
    unsigned w = t.getBits(), ew = e.getBits();
    bool sg = t.isSigned();
    Type T = i(w), E = i(ew), pair = LLVM::LLVMStructType::getLiteral(ctx_, {T, i(1)});
    std::string msg = "attempt to exponentiate with overflow (`" + t.getKindName().str() + "`)";
    auto f = internal(b, l, nm, T, {T, E});
    OpBuilder::InsertionGuard g(b);
    Region &body = f.getBody();
    Block *entry = b.createBlock(&body, {}, {T, E}, {l, l});
    Block *hugeE = b.createBlock(&body), *retX = sg ? nullptr : b.createBlock(&body);
    Block *trap = b.createBlock(&body), *norm = b.createBlock(&body), *ret1 = b.createBlock(&body);
    Block *c0 = b.createBlock(&body), *ret0 = b.createBlock(&body), *c1 = b.createBlock(&body);
    Block *cm1 = b.createBlock(&body), *parity = sg ? b.createBlock(&body) : nullptr;
    Block *loop = b.createBlock(&body, {}, {T, i(32)}, {l, l});
    Block *bodyB = b.createBlock(&body), *out = b.createBlock(&body);
    Value x = entry->getArgument(0), y = entry->getArgument(1);
    b.setInsertionPointToEnd(entry);
    Value y128 = ew < 128 ? b.create<LLVM::ZExtOp>(l, i(128), y).getResult() : y;
    Value huge = b.create<LLVM::ICmpOp>(l, LLVM::ICmpPredicate::ugt, y128, cint(b, l, 128, 4294967295ull));
    b.create<LLVM::CondBrOp>(l, huge, hugeE, norm);
    b.setInsertionPointToEnd(hugeE);
    if (sg) {
      b.create<LLVM::BrOp>(l, ValueRange{}, trap);
    } else {
      Value small = b.create<LLVM::ICmpOp>(l, LLVM::ICmpPredicate::ule, x, cint(b, l, w, 1));
      b.create<LLVM::CondBrOp>(l, small, retX, trap);
      b.setInsertionPointToEnd(retX);
      b.create<LLVM::ReturnOp>(l, x);
    }
    b.setInsertionPointToEnd(trap);
    call(b, l, "pg_rt_trap", {str(b, l, msg)});
    b.create<LLVM::UnreachableOp>(l);
    b.setInsertionPointToEnd(norm);
    Value ex = b.create<LLVM::TruncOp>(l, i(32), y128);
    Value ez = b.create<LLVM::ICmpOp>(l, LLVM::ICmpPredicate::eq, ex, cint(b, l, 32, 0));
    b.create<LLVM::CondBrOp>(l, ez, ret1, c0);
    b.setInsertionPointToEnd(ret1);
    b.create<LLVM::ReturnOp>(l, cint(b, l, w, 1));
    b.setInsertionPointToEnd(c0);
    Value x0 = b.create<LLVM::ICmpOp>(l, LLVM::ICmpPredicate::eq, x, cint(b, l, w, 0));
    b.create<LLVM::CondBrOp>(l, x0, ret0, c1);
    b.setInsertionPointToEnd(ret0);
    b.create<LLVM::ReturnOp>(l, cint(b, l, w, 0));
    b.setInsertionPointToEnd(c1);
    Value x1 = b.create<LLVM::ICmpOp>(l, LLVM::ICmpPredicate::eq, x, cint(b, l, w, 1));
    b.create<LLVM::CondBrOp>(l, x1, ret1, cm1);
    b.setInsertionPointToEnd(cm1);
    Value one = cint(b, l, w, 1);
    if (sg) {
      Value xm1 = b.create<LLVM::ICmpOp>(l, LLVM::ICmpPredicate::eq, x, ones(b, l, w));
      b.create<LLVM::CondBrOp>(l, xm1, parity, ValueRange{}, loop, ValueRange{one, ex});
      b.setInsertionPointToEnd(parity);
      Value odd = b.create<LLVM::TruncOp>(l, i(1), ex);
      b.create<LLVM::ReturnOp>(l, b.create<LLVM::SelectOp>(l, odd, ones(b, l, w), cint(b, l, w, 1)).getResult());
    } else {
      b.create<LLVM::BrOp>(l, ValueRange{one, ex}, loop);
    }
    b.setInsertionPointToEnd(loop);
    Value acc = loop->getArgument(0), k = loop->getArgument(1);
    Value p = sg ? b.create<LLVM::SMulWithOverflowOp>(l, pair, acc, x).getResult()
                 : b.create<LLVM::UMulWithOverflowOp>(l, pair, acc, x).getResult();
    Value ov = b.create<LLVM::ExtractValueOp>(l, p, ArrayRef<int64_t>{1});
    b.create<LLVM::CondBrOp>(l, ov, trap, bodyB);
    b.setInsertionPointToEnd(bodyB);
    Value acc2 = b.create<LLVM::ExtractValueOp>(l, p, ArrayRef<int64_t>{0});
    Value k2 = b.create<LLVM::SubOp>(l, k, cint(b, l, 32, 1));
    Value done = b.create<LLVM::ICmpOp>(l, LLVM::ICmpPredicate::eq, k2, cint(b, l, 32, 0));
    b.create<LLVM::CondBrOp>(l, done, out, ValueRange{}, loop, ValueRange{acc2, k2});
    b.setInsertionPointToEnd(out);
    b.create<LLVM::ReturnOp>(l, acc2);
    return nm;
  }

  // Fill an array of more than 64 elements with one value (a loop, as the
  // direct emitter's `repeat`): pg.fill.<n>(ptr dst, elem).
  std::string fill(OpBuilder &b, Location l, ArrayType t) {
    Type at = conv(t), et = conv(t.getElem());
    auto key = fills_.find(at.getAsOpaquePointer());
    if (key != fills_.end()) return key->second;
    std::string nm = "pg.fill." + std::to_string(fills_.size());
    fills_[at.getAsOpaquePointer()] = nm;
    auto f = internal(b, l, nm, voidTy(), {ptr(), et});
    OpBuilder::InsertionGuard g(b);
    Block *entry = b.createBlock(&f.getBody(), {}, {ptr(), et}, {l, l});
    Block *head = b.createBlock(&f.getBody(), {}, {i(64)}, {l});
    Block *body = b.createBlock(&f.getBody()), *done = b.createBlock(&f.getBody());
    b.setInsertionPointToEnd(entry);
    b.create<LLVM::BrOp>(l, ValueRange{cint(b, l, 64, 0)}, head);
    b.setInsertionPointToEnd(head);
    Value k = head->getArgument(0);
    Value more = b.create<LLVM::ICmpOp>(l, LLVM::ICmpPredicate::ult, k, cint(b, l, 64, t.getLen()));
    b.create<LLVM::CondBrOp>(l, more, body, done);
    b.setInsertionPointToEnd(body);
    Value p = b.create<LLVM::GEPOp>(l, ptr(), at, entry->getArgument(0), ArrayRef<LLVM::GEPArg>{0, k}, true);
    b.create<LLVM::StoreOp>(l, entry->getArgument(1), p);
    b.create<LLVM::BrOp>(l, ValueRange{b.create<LLVM::AddOp>(l, k, cint(b, l, 64, 1)).getResult()}, head);
    b.setInsertionPointToEnd(done);
    b.create<LLVM::ReturnOp>(l, ValueRange{});
    return nm;
  }

  // Function results (converted), for calls.
  std::map<std::string, Type> rets;

private:
  ModuleOp mod_;
  MLIRContext *ctx_;
  const Info &info_;
  const TypeConverter &tc_;
  std::map<std::string, std::string> strs_;
  std::set<std::string> helpers_;
  std::map<const void *, std::string> fills_;
};

//===----------------------------------------------------------------------===//
// 4. Conversion patterns
//===----------------------------------------------------------------------===//

template <typename Op>
struct Pat : OpConversionPattern<Op> {
  Pat(const TypeConverter &tc, MLIRContext *ctx, Lib &lib) : OpConversionPattern<Op>(tc, ctx), lib(lib) {}
  Lib &lib;
  Type conv(Type t) const { return this->getTypeConverter()->convertType(t); }
};

// func.func -> llvm.func (internal), with the call-depth prologue.
struct FuncLowering : Pat<func::FuncOp> {
  using Pat::Pat;
  LogicalResult matchAndRewrite(func::FuncOp f, OpAdaptor, ConversionPatternRewriter &rw) const override {
    FunctionType ft = f.getFunctionType();
    if (ft.getNumResults() != 1 || f.isDeclaration()) return rw.notifyMatchFailure(f, "not a Pegasus function");
    TypeConverter::SignatureConversion sig(ft.getNumInputs());
    for (auto [k, t] : llvm::enumerate(ft.getInputs())) {
      Type c = conv(t);
      if (!c) return failure();
      sig.addInputs(k, c);
    }
    Type ret = conv(ft.getResult(0));
    if (!ret) return failure();
    auto lf = rw.create<LLVM::LLVMFuncOp>(f.getLoc(), f.getSymName(),
                                          LLVM::LLVMFunctionType::get(ret, sig.getConvertedTypes()),
                                          LLVM::Linkage::Internal);
    rw.inlineRegionBefore(f.getBody(), lf.getBody(), lf.end());
    FailureOr<Block *> entry = rw.convertRegionTypes(&lf.getBody(), *getTypeConverter(), &sig);
    if (failed(entry)) return failure();
    // Call depth (spec/native.md §4): the interpreter's limit, same message.
    Location l = f.getLoc();
    rw.setInsertionPointToStart(*entry);
    Value g = rw.create<LLVM::AddressOfOp>(l, lib.ptr(), "pg_rt_depth");
    Value d = rw.create<LLVM::LoadOp>(l, lib.i(64), g);
    Value d1 = rw.create<LLVM::AddOp>(l, d, lib.cint(rw, l, 64, 1));
    rw.create<LLVM::StoreOp>(l, d1, g);
    Value ok = rw.create<LLVM::ICmpOp>(l, LLVM::ICmpPredicate::ule, d1, lib.cint(rw, l, 64, kMaxDepth));
    lib.check(rw, l, ok, "stack overflow (call depth limit reached)");
    rw.eraseOp(f);
    return success();
  }
};

struct ConstLowering : Pat<ConstOp> {
  using Pat::Pat;
  LogicalResult matchAndRewrite(ConstOp op, OpAdaptor, ConversionPatternRewriter &rw) const override {
    Location l = op.getLoc();
    if (auto it = dyn_cast<IntType>(op.getType())) {
      auto v = dyn_cast<IntegerAttr>(op.getValue());
      if (!v) return failure();
      rw.replaceOp(op, lib.cint(rw, l, it.getBits(), v.getValue()));
      return success();
    }
    if (isa<BoolType>(op.getType())) {
      auto v = dyn_cast<BoolAttr>(op.getValue());
      if (!v) return failure();
      rw.replaceOp(op, lib.cint(rw, l, 1, v.getValue() ? 1 : 0));
      return success();
    }
    return failure();
  }
};

struct BinLowering : Pat<BinOp> {
  using Pat::Pat;
  LogicalResult matchAndRewrite(BinOp op, OpAdaptor a, ConversionPatternRewriter &rw) const override {
    Location l = op.getLoc();
    Value x = a.getLhs(), y = a.getRhs();
    StringRef k = op.getKind(), ov = op.getOv();
    Type pt = op.getLhs().getType();
    if (isa<BoolType>(pt)) {
      if (k == "and") rw.replaceOpWithNewOp<LLVM::AndOp>(op, x, y);
      else if (k == "or") rw.replaceOpWithNewOp<LLVM::OrOp>(op, x, y);
      else if (k == "xor") rw.replaceOpWithNewOp<LLVM::XOrOp>(op, x, y);
      else return failure();
      return success();
    }
    auto it = dyn_cast<IntType>(pt);
    if (!it) return failure();
    unsigned w = it.getBits();
    bool sg = it.isSigned();
    Type T = lib.i(w);
    std::string tyname = it.getKindName().str();
    std::string ovmsg = std::string("attempt to ") + verb(k) + " with overflow (`" + tyname + "`)";
    Value r;
    if (k == "and") r = rw.create<LLVM::AndOp>(l, x, y);
    else if (k == "or") r = rw.create<LLVM::OrOp>(l, x, y);
    else if (k == "xor") r = rw.create<LLVM::XOrOp>(l, x, y);
    else if (k == "add" || k == "sub" || k == "mul") {
      if (ov == "wrap") {
        if (k == "add") r = rw.create<LLVM::AddOp>(l, x, y);
        else if (k == "sub") r = rw.create<LLVM::SubOp>(l, x, y);
        else r = rw.create<LLVM::MulOp>(l, x, y);
      } else if (ov == "sat" && k != "mul") {
        if (k == "add") r = sg ? rw.create<LLVM::SAddSat>(l, T, x, y).getResult() : rw.create<LLVM::UAddSat>(l, T, x, y).getResult();
        else r = sg ? rw.create<LLVM::SSubSat>(l, T, x, y).getResult() : rw.create<LLVM::USubSat>(l, T, x, y).getResult();
      } else {
        Type pair = LLVM::LLVMStructType::getLiteral(rw.getContext(), {T, lib.i(1)});
        Value p;
        if (k == "add") p = sg ? rw.create<LLVM::SAddWithOverflowOp>(l, pair, x, y).getResult() : rw.create<LLVM::UAddWithOverflowOp>(l, pair, x, y).getResult();
        else if (k == "sub") p = sg ? rw.create<LLVM::SSubWithOverflowOp>(l, pair, x, y).getResult() : rw.create<LLVM::USubWithOverflowOp>(l, pair, x, y).getResult();
        else p = sg ? rw.create<LLVM::SMulWithOverflowOp>(l, pair, x, y).getResult() : rw.create<LLVM::UMulWithOverflowOp>(l, pair, x, y).getResult();
        Value o = rw.create<LLVM::ExtractValueOp>(l, p, ArrayRef<int64_t>{1});
        Value v = rw.create<LLVM::ExtractValueOp>(l, p, ArrayRef<int64_t>{0});
        if (ov == "trap") {
          lib.check(rw, l, rw.create<LLVM::XOrOp>(l, o, lib.ctrue(rw, l)), ovmsg);
          r = v;
        } else {
          // Saturating multiply: on overflow the sign of the true product decides.
          Value bound;
          if (sg) {
            Value zero = lib.cint(rw, l, w, 0);
            Value na = rw.create<LLVM::ICmpOp>(l, LLVM::ICmpPredicate::slt, x, zero);
            Value nb = rw.create<LLVM::ICmpOp>(l, LLVM::ICmpPredicate::slt, y, zero);
            Value neg = rw.create<LLVM::XOrOp>(l, na, nb);
            bound = rw.create<LLVM::SelectOp>(l, neg, lib.cint(rw, l, w, minSigned(w)), lib.cint(rw, l, w, maxSigned(w)));
          } else {
            bound = lib.ones(rw, l, w);
          }
          r = rw.create<LLVM::SelectOp>(l, o, bound, v);
        }
      }
    } else if (k == "div" || k == "rem") {
      bool div = k == "div";
      Value nz = rw.create<LLVM::ICmpOp>(l, LLVM::ICmpPredicate::ne, y, lib.cint(rw, l, w, 0));
      lib.check(rw, l, nz, div ? "attempt to divide by zero" : "attempt to calculate the remainder with a divisor of zero");
      // 128-bit division goes through the IR helper (spec/native.md §3).
      auto divide = [&](StringRef name, Value p, Value q) -> Value {
        if (w == 128) return lib.call1(rw, l, T, lib.div128(rw, l, name), {p, q});
        if (name == "udiv") return rw.create<LLVM::UDivOp>(l, p, q);
        if (name == "urem") return rw.create<LLVM::URemOp>(l, p, q);
        if (name == "sdiv") return rw.create<LLVM::SDivOp>(l, p, q);
        return rw.create<LLVM::SRemOp>(l, p, q);
      };
      if (!sg) {
        r = divide(div ? "udiv" : "urem", x, y);
      } else {
        Value m1 = rw.create<LLVM::ICmpOp>(l, LLVM::ICmpPredicate::eq, y, lib.ones(rw, l, w));
        if (div) {
          Value isMin = rw.create<LLVM::ICmpOp>(l, LLVM::ICmpPredicate::eq, x, lib.cint(rw, l, w, minSigned(w)));
          Value bad = rw.create<LLVM::AndOp>(l, isMin, m1);
          lib.check(rw, l, rw.create<LLVM::XOrOp>(l, bad, lib.ctrue(rw, l)), ovmsg);
          r = divide("sdiv", x, y);
        } else {
          // MIN % -1 = 0 (Phase 0 §7); x % 1 = 0 as well, and srem by -1 is UB.
          Value sb = rw.create<LLVM::SelectOp>(l, m1, lib.cint(rw, l, w, 1), y);
          r = divide("srem", x, sb);
        }
      }
    } else if (k == "shl" || k == "shr") {
      auto bt = dyn_cast<IntType>(op.getRhs().getType());
      if (!bt) return failure();
      unsigned bw = bt.getBits();
      Value ok = rw.create<LLVM::ICmpOp>(l, LLVM::ICmpPredicate::ult, y, lib.cint(rw, l, bw, w));
      auto [lo, hi] = lib.halves(rw, l, y, bw, false);
      lib.call(rw, l, "pg.check_shift", {ok, lo, hi, lib.str(rw, l, tyname), lib.cint(rw, l, 32, w)});
      Value amt = y;
      if (bw > w) amt = rw.create<LLVM::TruncOp>(l, T, y);
      else if (bw < w) amt = rw.create<LLVM::ZExtOp>(l, T, y);
      if (k == "shl") r = rw.create<LLVM::ShlOp>(l, x, amt);
      else if (sg) r = rw.create<LLVM::AShrOp>(l, x, amt);
      else r = rw.create<LLVM::LShrOp>(l, x, amt);
    } else if (k == "pow") {
      auto et = dyn_cast<IntType>(op.getRhs().getType());
      if (!et) return failure();
      r = lib.call1(rw, l, T, lib.pow(rw, l, it, et), {x, y});
    } else {
      return failure();
    }
    rw.replaceOp(op, r);
    return success();
  }
};

struct UnLowering : Pat<UnOp> {
  using Pat::Pat;
  LogicalResult matchAndRewrite(UnOp op, OpAdaptor a, ConversionPatternRewriter &rw) const override {
    Location l = op.getLoc();
    Value x = a.getOperand();
    StringRef k = op.getKind();
    if (k == "not") {
      rw.replaceOpWithNewOp<LLVM::XOrOp>(op, x, lib.ctrue(rw, l));
      return success();
    }
    unsigned w = isa<BoolType>(op.getOperand().getType()) ? 1 : 0;
    auto it = dyn_cast<IntType>(op.getOperand().getType());
    if (it) w = it.getBits();
    if (!w) return failure();
    if (k == "bitnot") {
      rw.replaceOpWithNewOp<LLVM::XOrOp>(op, x, lib.ones(rw, l, w));
      return success();
    }
    if (k != "neg" || !it) return failure();
    std::string msg = "attempt to negate with overflow (`" + it.getKindName().str() + "`)";
    Value ok = it.isSigned()
                   ? rw.create<LLVM::ICmpOp>(l, LLVM::ICmpPredicate::ne, x, lib.cint(rw, l, w, minSigned(w))).getResult()
                   : lib.cint(rw, l, 1, 0);
    lib.check(rw, l, ok, msg);
    rw.replaceOpWithNewOp<LLVM::SubOp>(op, lib.cint(rw, l, w, 0), x);
    return success();
  }
};

struct CmpLowering : Pat<pg::CmpOp> {
  using Pat::Pat;
  LogicalResult matchAndRewrite(pg::CmpOp op, OpAdaptor a, ConversionPatternRewriter &rw) const override {
    Location l = op.getLoc();
    Type t = op.getLhs().getType();
    StringRef p = op.getPred();
    Value x = a.getLhs(), y = a.getRhs();
    if (p == "eq" || p == "ne") {
      if (isa<IntType, BoolType>(t)) {
        rw.replaceOpWithNewOp<LLVM::ICmpOp>(op, p == "eq" ? LLVM::ICmpPredicate::eq : LLVM::ICmpPredicate::ne, x, y);
        return success();
      }
      Value e = lib.equal(rw, l, x, y, t);
      if (p == "ne") e = rw.create<LLVM::XOrOp>(l, e, lib.ctrue(rw, l));
      rw.replaceOp(op, e);
      return success();
    }
    auto it = dyn_cast<IntType>(t);
    if (!it) return failure();
    bool sg = it.isSigned();
    LLVM::ICmpPredicate pr = p == "lt" ? (sg ? LLVM::ICmpPredicate::slt : LLVM::ICmpPredicate::ult)
                             : p == "le" ? (sg ? LLVM::ICmpPredicate::sle : LLVM::ICmpPredicate::ule)
                             : p == "gt" ? (sg ? LLVM::ICmpPredicate::sgt : LLVM::ICmpPredicate::ugt)
                                         : (sg ? LLVM::ICmpPredicate::sge : LLVM::ICmpPredicate::uge);
    rw.replaceOpWithNewOp<LLVM::ICmpOp>(op, pr, x, y);
    return success();
  }
};

struct CastLowering : Pat<CastOp> {
  using Pat::Pat;
  LogicalResult matchAndRewrite(CastOp op, OpAdaptor a, ConversionPatternRewriter &rw) const override {
    auto to = dyn_cast<IntType>(op.getType());
    if (!to) return failure();
    Type T = lib.i(to.getBits());
    Value x = a.getOperand();
    Type from = op.getOperand().getType();
    if (isa<BoolType>(from)) {
      rw.replaceOpWithNewOp<LLVM::ZExtOp>(op, T, x);
      return success();
    }
    auto fi = dyn_cast<IntType>(from);
    if (!fi) return failure();
    unsigned fw = fi.getBits(), tw = to.getBits();
    if (fw == tw) rw.replaceOp(op, x);
    else if (fw > tw) rw.replaceOpWithNewOp<LLVM::TruncOp>(op, T, x);
    else if (fi.isSigned()) rw.replaceOpWithNewOp<LLVM::SExtOp>(op, T, x);
    else rw.replaceOpWithNewOp<LLVM::ZExtOp>(op, T, x);
    return success();
  }
};

struct SlotLowering : Pat<SlotOp> {
  using Pat::Pat;
  LogicalResult matchAndRewrite(SlotOp op, OpAdaptor, ConversionPatternRewriter &rw) const override {
    Type et = conv(cast<SlotType>(op.getType()).getElem());
    if (!et) return failure();
    Value one = lib.cint(rw, op.getLoc(), 64, 1);
    rw.replaceOpWithNewOp<LLVM::AllocaOp>(op, lib.ptr(), et, one, 0);
    return success();
  }
};

Type slotElem(Value v) { return cast<SlotType>(v.getType()).getElem(); }
Type pointee(Value v) { return cast<RefType>(v.getType()).getPointee(); }

struct LoadLowering : Pat<pg::LoadOp> {
  using Pat::Pat;
  LogicalResult matchAndRewrite(pg::LoadOp op, OpAdaptor a, ConversionPatternRewriter &rw) const override {
    auto [p, t] = lib.place(rw, op.getLoc(), a.getSlot(), slotElem(op.getSlot()), op.getPath(), a.getIndices(),
                            op.getIndices().getTypes());
    if (!p) return failure();
    rw.replaceOpWithNewOp<LLVM::LoadOp>(op, conv(t), p);
    return success();
  }
};

struct MoveLowering : Pat<MoveOp> {
  using Pat::Pat;
  LogicalResult matchAndRewrite(MoveOp op, OpAdaptor a, ConversionPatternRewriter &rw) const override {
    auto [p, t] = lib.place(rw, op.getLoc(), a.getSlot(), slotElem(op.getSlot()), op.getPath(), {}, {});
    if (!p) return failure();
    rw.replaceOpWithNewOp<LLVM::LoadOp>(op, conv(t), p);
    return success();
  }
};

struct StoreLowering : Pat<pg::StoreOp> {
  using Pat::Pat;
  LogicalResult matchAndRewrite(pg::StoreOp op, OpAdaptor a, ConversionPatternRewriter &rw) const override {
    auto [p, t] = lib.place(rw, op.getLoc(), a.getSlot(), slotElem(op.getSlot()), op.getPath(), a.getIndices(),
                            op.getIndices().getTypes());
    if (!p) return failure();
    rw.create<LLVM::StoreOp>(op.getLoc(), a.getValue(), p);
    rw.eraseOp(op);
    return success();
  }
};

// No type in the native subset has a destructor: ending the owner's scope
// has no observable effect (spec/native.md §3).
struct DropLowering : Pat<DropOp> {
  using Pat::Pat;
  LogicalResult matchAndRewrite(DropOp op, OpAdaptor, ConversionPatternRewriter &rw) const override {
    rw.eraseOp(op);
    return success();
  }
};

struct BorrowLowering : Pat<BorrowOp> {
  using Pat::Pat;
  LogicalResult matchAndRewrite(BorrowOp op, OpAdaptor a, ConversionPatternRewriter &rw) const override {
    auto [p, t] = lib.place(rw, op.getLoc(), a.getSlot(), slotElem(op.getSlot()), op.getPath(), a.getIndices(),
                            op.getIndices().getTypes());
    if (!p) return failure();
    rw.replaceOp(op, p);
    return success();
  }
};

struct BorrowRefLowering : Pat<BorrowRefOp> {
  using Pat::Pat;
  LogicalResult matchAndRewrite(BorrowRefOp op, OpAdaptor a, ConversionPatternRewriter &rw) const override {
    auto [p, t] = lib.place(rw, op.getLoc(), a.getRef(), pointee(op.getRef()), op.getPath(), a.getIndices(),
                            op.getIndices().getTypes());
    if (!p) return failure();
    rw.replaceOp(op, p);
    return success();
  }
};

struct LoadRefLowering : Pat<LoadRefOp> {
  using Pat::Pat;
  LogicalResult matchAndRewrite(LoadRefOp op, OpAdaptor a, ConversionPatternRewriter &rw) const override {
    auto [p, t] = lib.place(rw, op.getLoc(), a.getRef(), pointee(op.getRef()), op.getPath(), a.getIndices(),
                            op.getIndices().getTypes());
    if (!p) return failure();
    rw.replaceOpWithNewOp<LLVM::LoadOp>(op, conv(t), p);
    return success();
  }
};

struct StoreRefLowering : Pat<StoreRefOp> {
  using Pat::Pat;
  LogicalResult matchAndRewrite(StoreRefOp op, OpAdaptor a, ConversionPatternRewriter &rw) const override {
    auto [p, t] = lib.place(rw, op.getLoc(), a.getRef(), pointee(op.getRef()), op.getPath(), a.getIndices(),
                            op.getIndices().getTypes());
    if (!p) return failure();
    rw.create<LLVM::StoreOp>(op.getLoc(), a.getValue(), p);
    rw.eraseOp(op);
    return success();
  }
};

template <typename Op>
struct AggregateLowering : Pat<Op> {
  using Pat<Op>::Pat;
  LogicalResult matchAndRewrite(Op op, typename Op::Adaptor a, ConversionPatternRewriter &rw) const override {
    Type lt = this->conv(op.getType());
    if (!lt) return failure();
    rw.replaceOp(op, this->lib.build(rw, op.getLoc(), lt, a.getOperands()));
    return success();
  }
};

struct RepeatLowering : Pat<RepeatOp> {
  using Pat::Pat;
  LogicalResult matchAndRewrite(RepeatOp op, OpAdaptor a, ConversionPatternRewriter &rw) const override {
    Location l = op.getLoc();
    auto at = cast<ArrayType>(op.getType());
    Type lt = conv(at);
    uint64_t n = op.getCount();
    if (!lt) return failure();
    if (n <= 64) {
      SmallVector<Value> el(n, a.getValue());
      rw.replaceOp(op, lib.build(rw, l, lt, el));
      return success();
    }
    Value tmp = lib.entryAlloca(rw, op, lt);
    if (!tmp) return failure();
    lib.call(rw, l, lib.fill(rw, l, at), {tmp, a.getValue()});
    rw.replaceOpWithNewOp<LLVM::LoadOp>(op, lt, tmp);
    return success();
  }
};

struct ExtractLowering : Pat<ExtractOp> {
  using Pat::Pat;
  LogicalResult matchAndRewrite(ExtractOp op, OpAdaptor a, ConversionPatternRewriter &rw) const override {
    rw.replaceOpWithNewOp<LLVM::ExtractValueOp>(op, a.getAggregate(),
                                                ArrayRef<int64_t>{static_cast<int64_t>(op.getIndex())});
    return success();
  }
};

struct ExtractIndexLowering : Pat<ExtractIndexOp> {
  using Pat::Pat;
  LogicalResult matchAndRewrite(ExtractIndexOp op, OpAdaptor a, ConversionPatternRewriter &rw) const override {
    Location l = op.getLoc();
    auto at = cast<ArrayType>(op.getArray().getType());
    auto it = dyn_cast<IntType>(op.getIndex().getType());
    Type lt = conv(at), et = conv(at.getElem());
    if (!it || !lt || !et) return failure();
    Value tmp = lib.entryAlloca(rw, op, lt);
    if (!tmp) return failure();
    rw.create<LLVM::StoreOp>(l, a.getArray(), tmp);
    Value k = lib.index(rw, l, a.getIndex(), it.getBits(), at.getLen());
    Value p = rw.create<LLVM::GEPOp>(l, lib.ptr(), lt, tmp, ArrayRef<LLVM::GEPArg>{0, k}, true);
    rw.replaceOpWithNewOp<LLVM::LoadOp>(op, et, p);
    return success();
  }
};

struct CallLowering : Pat<CallOp> {
  using Pat::Pat;
  LogicalResult matchAndRewrite(CallOp op, OpAdaptor a, ConversionPatternRewriter &rw) const override {
    auto it = lib.rets.find(op.getCallee().str());
    if (it == lib.rets.end()) return failure();
    Value r = lib.call1(rw, op.getLoc(), it->second, op.getCallee(), a.getArgs());
    if (op.getNumResults()) rw.replaceOp(op, r);
    else rw.eraseOp(op);
    return success();
  }
};

struct BrLowering : Pat<BrOp> {
  using Pat::Pat;
  LogicalResult matchAndRewrite(BrOp op, OpAdaptor a, ConversionPatternRewriter &rw) const override {
    rw.replaceOpWithNewOp<LLVM::BrOp>(op, a.getArgs(), op.getDest());
    return success();
  }
};

struct CondBrLowering : Pat<CondBrOp> {
  using Pat::Pat;
  LogicalResult matchAndRewrite(CondBrOp op, OpAdaptor a, ConversionPatternRewriter &rw) const override {
    rw.replaceOpWithNewOp<LLVM::CondBrOp>(op, a.getCondition(), op.getTrueDest(), a.getTrueArgs(), op.getFalseDest(),
                                          a.getFalseArgs());
    return success();
  }
};

struct SwitchLowering : Pat<SwitchOp> {
  using Pat::Pat;
  LogicalResult matchAndRewrite(SwitchOp op, OpAdaptor a, ConversionPatternRewriter &rw) const override {
    Type vt = op.getValue().getType();
    unsigned w = isa<BoolType>(vt) ? 1 : isa<IntType>(vt) ? cast<IntType>(vt).getBits() : 0;
    if (!w) return failure();
    SmallVector<APInt> cases;
    for (Attribute c : op.getCases()) {
      auto ia = dyn_cast<IntegerAttr>(c);
      if (!ia) return failure();
      cases.push_back(ia.getValue().trunc(w));
    }
    SmallVector<Block *> dests(op.getCaseDests().begin(), op.getCaseDests().end());
    SmallVector<ValueRange> operands(dests.size(), ValueRange{});
    rw.replaceOpWithNewOp<LLVM::SwitchOp>(op, a.getValue(), op.getDefaultDest(), ValueRange{}, cases, dests,
                                          operands);
    return success();
  }
};

struct RetLowering : Pat<RetOp> {
  using Pat::Pat;
  LogicalResult matchAndRewrite(RetOp op, OpAdaptor a, ConversionPatternRewriter &rw) const override {
    Location l = op.getLoc();
    Value g = rw.create<LLVM::AddressOfOp>(l, lib.ptr(), "pg_rt_depth");
    Value d = rw.create<LLVM::LoadOp>(l, lib.i(64), g);
    rw.create<LLVM::StoreOp>(l, rw.create<LLVM::SubOp>(l, d, lib.cint(rw, l, 64, 1)), g);
    rw.replaceOpWithNewOp<LLVM::ReturnOp>(op, a.getValue());
    return success();
  }
};

struct TrapLowering : Pat<TrapOp> {
  using Pat::Pat;
  LogicalResult matchAndRewrite(TrapOp op, OpAdaptor, ConversionPatternRewriter &rw) const override {
    lib.call(rw, op.getLoc(), "pg_rt_trap", {lib.str(rw, op.getLoc(), op.getMessage())});
    rw.replaceOpWithNewOp<LLVM::UnreachableOp>(op);
    return success();
  }
};

struct UnreachableLowering : Pat<UnreachableOp> {
  using Pat::Pat;
  LogicalResult matchAndRewrite(UnreachableOp op, OpAdaptor, ConversionPatternRewriter &rw) const override {
    lib.call(rw, op.getLoc(), "pg_rt_trap", {lib.str(rw, op.getLoc(), "reached unreachable code (compiler bug)")});
    rw.replaceOpWithNewOp<LLVM::UnreachableOp>(op);
    return success();
  }
};

//===----------------------------------------------------------------------===//
// 5. The C entry point and result printing (interp::show)
//===----------------------------------------------------------------------===//

void print(Lib &lib, OpBuilder &b, Location l, Value v, Type t) {
  auto s = [&](StringRef text) { lib.call(b, l, "pg_rt_print_str", {lib.str(b, l, text)}); };
  if (auto it = dyn_cast<IntType>(t)) {
    auto [lo, hi] = lib.halves(b, l, v, it.getBits(), it.isSigned());
    lib.call(b, l, "pg_rt_print_int", {lo, hi, lib.cint(b, l, 32, it.isSigned() ? 1 : 0)});
    return;
  }
  if (isa<BoolType>(t)) {
    Value p = b.create<LLVM::SelectOp>(l, v, lib.str(b, l, "true"), lib.str(b, l, "false"));
    lib.call(b, l, "pg_rt_print_str", {p});
    return;
  }
  std::vector<Type> fs;
  auto st = dyn_cast<StructType>(t);
  if (auto at = dyn_cast<ArrayType>(t)) fs.assign(at.getLen(), at.getElem());
  else fs = lib.info().fields(t);
  const StructDef *sd = st ? &lib.info().structDef(st.getId()) : nullptr;
  if (sd) s(sd->name + " { ");
  else s(isa<TupleType>(t) ? "(" : "[");
  for (size_t k = 0; k < fs.size(); ++k) {
    if (k) s(", ");
    if (sd) s(sd->fieldNames[k] + ": ");
    Value e = b.create<LLVM::ExtractValueOp>(l, v, ArrayRef<int64_t>{static_cast<int64_t>(k)});
    print(lib, b, l, e, fs[k]);
  }
  s(sd ? " }" : isa<TupleType>(t) ? ")" : "]");
}

}  // namespace

LowerResult lowerToLLVM(ModuleOp mod) {
  MLIRContext *ctx = mod.getContext();
  registerLoweringDialects(*ctx);
  Location l = mod.getLoc();
  auto entryAttr = mod->getAttrOfType<FlatSymbolRefAttr>("pg.entry");
  if (!entryAttr) return {LowerStatus::NoEntry, "the module has no entry function"};
  auto entry = mod.lookupSymbol<func::FuncOp>(entryAttr.getValue());
  if (!entry) return {LowerStatus::Inconsistent, "pg.entry names no function"};
  if (entry.getFunctionType().getNumResults() != 1)
    return {LowerStatus::Inconsistent, "the entry function has no result type"};
  Type entryResult = entry.getFunctionType().getResult(0);
  std::string entryName = entry.getSymName().str();

  std::optional<Info> info;
  try {
    info.emplace(mod);
    Analysis an(mod, *info);
    std::vector<func::FuncOp> order = an.reach(entry);
    for (func::FuncOp f : order) an.func(f);
    if (!isUnit(entryResult)) an.printable(entryResult);
    // Only what the entry needs is lowered (spec/native.md §2).
    std::set<Operation *> keep(order.begin(), order.end());
    SmallVector<Operation *> drop;
    for (Operation &op : *mod.getBody())
      if (isa<func::FuncOp>(op) && !keep.count(&op)) drop.push_back(&op);
    for (Operation *op : drop) op->erase();
  } catch (const Unsupported &e) {
    return {LowerStatus::Unsupported, e.what()};
  } catch (const Inconsistent &e) {
    return {LowerStatus::Inconsistent, std::string("PIR inconsistent with its verification: ") + e.what()};
  }

  PgTypeConverter tc(ctx, *info);
  Lib lib(mod, *info, tc);
  OpBuilder b(ctx);
  b.setInsertionPointToEnd(mod.getBody());
  lib.declareRuntime(b, l);
  // Results of every function (calls name their callee's result type), and
  // the helpers the bodies need.
  for (auto f : mod.getOps<func::FuncOp>()) {
    Type r = tc.convertType(f.getFunctionType().getResult(0));
    if (!r) return {LowerStatus::Failed, "cannot convert the result type of " + f.getSymName().str()};
    lib.rets[f.getSymName().str()] = r;
    for (Block &blk : f.getBody())
      for (Operation &op : blk) {
        if (auto bo = dyn_cast<BinOp>(op)) {
          auto it = dyn_cast<IntType>(bo.getLhs().getType());
          if (it && bo.getKind() == "pow") lib.pow(b, l, it, cast<IntType>(bo.getRhs().getType()));
          if (it && it.getBits() == 128 && (bo.getKind() == "div" || bo.getKind() == "rem"))
            lib.div128(b, l, it.isSigned() ? (bo.getKind() == "div" ? "sdiv" : "srem")
                                           : (bo.getKind() == "div" ? "udiv" : "urem"));
        }
        if (auto ro = dyn_cast<RepeatOp>(op); ro && ro.getCount() > 64) lib.fill(b, l, cast<ArrayType>(ro.getType()));
      }
  }

  // Blocks no path reaches (the frontend leaves e.g. the failure arm of an
  // irrefutable pattern) can never run; the conversion driver visits only
  // reachable blocks, so they are erased first. They were still checked
  // above, so the native subset is decided exactly as by the direct emitter.
  {
    IRRewriter rw(ctx);
    for (auto f : mod.getOps<func::FuncOp>()) (void)eraseUnreachableBlocks(rw, f.getBody());
  }

  ConversionTarget target(*ctx);
  target.addLegalDialect<LLVM::LLVMDialect>();
  target.addLegalOp<ModuleOp>();
  target.addIllegalDialect<PgDialect, func::FuncDialect>();
  RewritePatternSet patterns(ctx);
  patterns.add<FuncLowering, ConstLowering, BinLowering, UnLowering, CmpLowering, CastLowering, SlotLowering,
               LoadLowering, MoveLowering, StoreLowering, DropLowering, BorrowLowering, BorrowRefLowering,
               LoadRefLowering, StoreRefLowering, AggregateLowering<TupleOp>, AggregateLowering<StructOp>,
               AggregateLowering<ArrayOp>, RepeatLowering, ExtractLowering, ExtractIndexLowering, CallLowering,
               BrLowering, CondBrLowering, SwitchLowering, RetLowering, TrapLowering, UnreachableLowering>(tc, ctx,
                                                                                                          lib);
  if (failed(applyFullConversion(mod, target, std::move(patterns))))
    return {LowerStatus::Failed, "the pg -> LLVM conversion failed"};

  // pg.program: call the entry, print its result; main: run it on the
  // runtime's large-stack thread, so the depth limit decides overflow.
  b.setInsertionPointToEnd(mod.getBody());
  auto prog = lib.internal(b, l, "pg.program", lib.voidTy(), {});
  b.createBlock(&prog.getBody());
  Value r = lib.call1(b, l, lib.rets[entryName], entryName, {});
  if (!isUnit(entryResult)) {
    print(lib, b, l, r, entryResult);
    lib.call(b, l, "pg_rt_print_str", {lib.str(b, l, "\n")});
  }
  b.create<LLVM::ReturnOp>(l, ValueRange{});
  b.setInsertionPointToEnd(mod.getBody());
  auto mainFn = b.create<LLVM::LLVMFuncOp>(l, "main", LLVM::LLVMFunctionType::get(lib.i(32), {}),
                                           LLVM::Linkage::External);
  b.createBlock(&mainFn.getBody());
  lib.call(b, l, "pg_rt_run", {b.create<LLVM::AddressOfOp>(l, prog).getResult()});
  b.create<LLVM::ReturnOp>(l, lib.cint(b, l, 32, 0));

  // The pg module attributes have been consumed.
  for (StringRef a : {"pg.pirb_minor", "pg.structs", "pg.enums", "pg.entry"}) mod->removeAttr(a);
  return {};
}

bool translateToLLVMIR(ModuleOp mod, std::string &ir, std::string &error) {
  MLIRContext &ctx = *mod.getContext();
  registerBuiltinDialectTranslation(ctx);
  registerLLVMDialectTranslation(ctx);
  // LLVM IR cannot branch twice to one block with different arguments.
  LLVM::ensureDistinctSuccessors(mod);
  llvm::LLVMContext llctx;
  std::unique_ptr<llvm::Module> lm = translateModuleToLLVMIR(mod, llctx, "pegasus");
  if (!lm) {
    error = "translation of the LLVM dialect to LLVM IR failed";
    return false;
  }
  llvm::raw_string_ostream os(ir);
  lm->print(os, nullptr);
  os.flush();
  return true;
}

}  // namespace pegasus::pg
