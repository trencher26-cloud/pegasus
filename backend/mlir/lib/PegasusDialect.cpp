// The Pegasus MLIR dialect: types (custom assembly), structural verifiers
// and branch successor operands (spec/mlir.md).
#include "pegasus/mlir/PegasusDialect.h"

#include "mlir/IR/Builders.h"
#include "mlir/IR/BuiltinOps.h"
#include "mlir/IR/DialectImplementation.h"
#include "llvm/ADT/TypeSwitch.h"

#include "pegasus/mlir/PegasusDialect.cpp.inc"

#define GET_TYPEDEF_CLASSES
#include "pegasus/mlir/PegasusTypes.cpp.inc"

#define GET_OP_CLASSES
#include "pegasus/mlir/PegasusOps.cpp.inc"

// Definitions live inside the namespace: pg::FloatType and pg::TupleType
// share names with MLIR builtins, and the inner scope must win.
namespace pegasus::pg {
using namespace mlir;

void PgDialect::initialize() {
  addTypes<
#define GET_TYPEDEF_LIST
#include "pegasus/mlir/PegasusTypes.cpp.inc"
      >();
  addOperations<
#define GET_OP_LIST
#include "pegasus/mlir/PegasusOps.cpp.inc"
      >();
}

//===----------------------------------------------------------------------===//
// Types
//===----------------------------------------------------------------------===//

namespace {
const char *const kIntNames[] = {"i8", "i16", "i32", "i64", "i128", "isize",
                                 "u8", "u16", "u32", "u64", "u128", "usize"};
const unsigned kIntBits[] = {8, 16, 32, 64, 128, 64, 8, 16, 32, 64, 128, 64};
const char *const kFloatNames[] = {"f16", "bf16", "f32", "f64"};

template <unsigned N>
ParseResult parseKind(AsmParser &p, const char *const (&names)[N], unsigned &kind) {
  StringRef word;
  SMLoc loc = p.getCurrentLocation();
  if (p.parseLess() || p.parseKeyword(&word) || p.parseGreater())
    return failure();
  for (unsigned i = 0; i < N; ++i)
    if (word == names[i]) {
      kind = i;
      return success();
    }
  return p.emitError(loc, "unknown kind '") << word << "'";
}

ParseResult parseTypeList(AsmParser &p, SmallVectorImpl<Type> &out) {
  if (succeeded(p.parseOptionalRParen()))
    return success();
  do {
    Type t;
    if (p.parseType(t))
      return failure();
    out.push_back(t);
  } while (succeeded(p.parseOptionalComma()));
  return p.parseRParen();
}

void printTypeList(AsmPrinter &p, ArrayRef<Type> ts) {
  p << '(';
  llvm::interleaveComma(ts, p, [&](Type t) { p.printType(t); });
  p << ')';
}
}  // namespace

unsigned IntType::getBits() const { return kIntBits[getKind()]; }
bool IntType::isSigned() const { return getKind() <= 5; }
StringRef IntType::getKindName() const { return kIntNames[getKind()]; }

Type IntType::parse(AsmParser &p) {
  unsigned k;
  if (parseKind(p, kIntNames, k))
    return {};
  return IntType::get(p.getContext(), k);
}
void IntType::print(AsmPrinter &p) const { p << '<' << kIntNames[getKind()] << '>'; }

Type FloatType::parse(AsmParser &p) {
  unsigned k;
  if (parseKind(p, kFloatNames, k))
    return {};
  return FloatType::get(p.getContext(), k);
}
void FloatType::print(AsmPrinter &p) const { p << '<' << kFloatNames[getKind()] << '>'; }

// !pg.tuple<(T, U)>
Type TupleType::parse(AsmParser &p) {
  SmallVector<Type> ts;
  if (p.parseLess() || p.parseLParen() || parseTypeList(p, ts) || p.parseGreater())
    return {};
  return TupleType::get(p.getContext(), ts);
}
void TupleType::print(AsmPrinter &p) const {
  p << '<';
  printTypeList(p, getElems());
  p << '>';
}

// !pg.array<N x T>
Type ArrayType::parse(AsmParser &p) {
  uint64_t n;
  Type t;
  if (p.parseLess() || p.parseInteger(n) || p.parseXInDimensionList() || p.parseType(t) || p.parseGreater())
    return {};
  return ArrayType::get(p.getContext(), t, n);
}
void ArrayType::print(AsmPrinter &p) const {
  p << '<' << getLen() << " x ";
  p.printType(getElem());
  p << '>';
}

// !pg.fn<(T, U) -> R>
Type FnType::parse(AsmParser &p) {
  SmallVector<Type> ps;
  Type r;
  if (p.parseLess() || p.parseLParen() || parseTypeList(p, ps) || p.parseArrow() || p.parseType(r) ||
      p.parseGreater())
    return {};
  return FnType::get(p.getContext(), ps, r);
}
void FnType::print(AsmPrinter &p) const {
  p << '<';
  printTypeList(p, getParams());
  p << " -> ";
  p.printType(getResult());
  p << '>';
}

// !pg.ref<T>, !pg.ref<mut, T> (and the same for !pg.ptr)
template <class T>
static Type parseMutType(AsmParser &p) {
  bool mut = false;
  Type t;
  if (p.parseLess())
    return {};
  if (succeeded(p.parseOptionalKeyword("mut"))) {
    mut = true;
    if (p.parseComma())
      return {};
  }
  if (p.parseType(t) || p.parseGreater())
    return {};
  return T::get(p.getContext(), mut, t);
}
template <class T>
static void printMutType(AsmPrinter &p, T t) {
  p << '<' << (t.getExclusive() ? "mut, " : "");
  p.printType(t.getPointee());
  p << '>';
}
Type PtrType::parse(AsmParser &p) { return parseMutType<PtrType>(p); }
void PtrType::print(AsmPrinter &p) const { printMutType(p, *this); }
Type RefType::parse(AsmParser &p) { return parseMutType<RefType>(p); }
void RefType::print(AsmPrinter &p) const { printMutType(p, *this); }

//===----------------------------------------------------------------------===//
// Verifiers (structure only; spec/mlir.md §2)
//===----------------------------------------------------------------------===//

namespace {

// The field types of struct `id`, from the module attribute pg.structs.
FailureOr<SmallVector<Type>> structFields(Operation *op, unsigned id) {
  auto mod = op->getParentOfType<ModuleOp>();
  auto defs = mod ? mod->getAttrOfType<ArrayAttr>("pg.structs") : ArrayAttr();
  if (!defs || id >= defs.size())
    return op->emitOpError("struct id ") << id << " has no definition in pg.structs", failure();
  auto d = dyn_cast<DictionaryAttr>(defs[id]);
  auto tys = d ? d.getAs<ArrayAttr>("field_types") : ArrayAttr();
  if (!tys)
    return op->emitOpError("malformed pg.structs entry ") << id, failure();
  SmallVector<Type> out;
  for (Attribute a : tys) {
    auto ta = dyn_cast<TypeAttr>(a);
    if (!ta)
      return op->emitOpError("malformed pg.structs entry ") << id, failure();
    out.push_back(ta.getValue());
  }
  return out;
}

// The type of the place `base.path`, consuming one index operand per -1.
FailureOr<Type> placeType(Operation *op, Type base, ArrayRef<int64_t> path, ValueRange indices) {
  unsigned next = 0;
  Type t = base;
  for (int64_t step : path) {
    if (step == -1) {
      auto a = dyn_cast<ArrayType>(t);
      if (!a)
        return op->emitOpError("index projection into a non-array"), failure();
      if (next >= indices.size())
        return op->emitOpError("path needs more index operands"), failure();
      if (!isa<IntType>(indices[next++].getType()))
        return op->emitOpError("index operand is not an integer"), failure();
      t = a.getElem();
    } else if (step < 0) {
      return op->emitOpError("malformed path step ") << step, failure();
    } else if (auto tu = dyn_cast<TupleType>(t)) {
      if (static_cast<uint64_t>(step) >= tu.getElems().size())
        return op->emitOpError("tuple field ") << step << " out of range", failure();
      t = tu.getElems()[step];
    } else if (auto st = dyn_cast<StructType>(t)) {
      auto fs = structFields(op, st.getId());
      if (failed(fs))
        return failure();
      if (static_cast<uint64_t>(step) >= fs->size())
        return op->emitOpError("struct field ") << step << " out of range", failure();
      t = (*fs)[step];
    } else {
      return op->emitOpError("field projection into a non-aggregate"), failure();
    }
  }
  if (next != indices.size())
    return op->emitOpError("more index operands than index projections"), failure();
  return t;
}

LogicalResult expectType(Operation *op, Type got, Type want, StringRef what) {
  if (got != want)
    return op->emitOpError() << what << " has type " << got << ", the place has type " << want;
  return success();
}

bool oneOf(StringRef s, std::initializer_list<StringRef> xs) {
  for (StringRef x : xs)
    if (s == x)
      return true;
  return false;
}

}  // namespace

LogicalResult ConstOp::verify() {
  Type t = getType();
  Attribute v = getValue();
  auto intOfWidth = [&](unsigned w) {
    auto ia = dyn_cast<IntegerAttr>(v);
    return ia && ia.getType().isInteger(w);
  };
  if (isa<IntType>(t) ? intOfWidth(128)
      : isa<FloatType>(t) ? intOfWidth(64)
      : isa<BoolType>(t)  ? isa<BoolAttr>(v)
      : isa<CharType>(t)  ? intOfWidth(32)
      : isa<StrType>(t)   ? isa<StringAttr>(v)
                          : false)
    return success();
  return emitOpError("constant attribute does not match the result type ") << t;
}

LogicalResult BinOp::verify() {
  if (!oneOf(getKind(), {"add", "sub", "mul", "div", "rem", "pow", "shl", "shr", "and", "or", "xor"}))
    return emitOpError("unknown kind '") << getKind() << "'";
  if (!oneOf(getOv(), {"trap", "wrap", "sat"}))
    return emitOpError("unknown overflow mode '") << getOv() << "'";
  if (getResult().getType() != getLhs().getType())
    return emitOpError("result type differs from the left operand's");
  return success();
}

LogicalResult UnOp::verify() {
  if (!oneOf(getKind(), {"neg", "not", "bitnot"}))
    return emitOpError("unknown kind '") << getKind() << "'";
  return success();
}

LogicalResult CmpOp::verify() {
  if (!oneOf(getPred(), {"eq", "ne", "lt", "le", "gt", "ge"}))
    return emitOpError("unknown predicate '") << getPred() << "'";
  if (getLhs().getType() != getRhs().getType())
    return emitOpError("operands of different types");
  return success();
}

LogicalResult LoadOp::verify() {
  auto t = placeType(*this, cast<SlotType>(getSlot().getType()).getElem(), getPath(), getIndices());
  return failed(t) ? failure() : expectType(*this, getType(), *t, "result");
}
LogicalResult StoreOp::verify() {
  auto t = placeType(*this, cast<SlotType>(getSlot().getType()).getElem(), getPath(), getIndices());
  return failed(t) ? failure() : expectType(*this, getValue().getType(), *t, "stored value");
}
LogicalResult MoveOp::verify() {
  for (int64_t s : getPath())
    if (s < 0)
      return emitOpError("a move path has field projections only");
  auto t = placeType(*this, cast<SlotType>(getSlot().getType()).getElem(), getPath(), {});
  return failed(t) ? failure() : expectType(*this, getType(), *t, "result");
}
LogicalResult BorrowOp::verify() {
  auto r = cast<RefType>(getType());
  if (r.getExclusive() != getExclusive())
    return emitOpError("result reference mutability differs from the borrow's");
  auto t = placeType(*this, cast<SlotType>(getSlot().getType()).getElem(), getPath(), getIndices());
  return failed(t) ? failure() : expectType(*this, r.getPointee(), *t, "borrowed reference");
}
LogicalResult BorrowRefOp::verify() {
  auto src = cast<RefType>(getRef().getType());
  auto r = cast<RefType>(getType());
  if (getExclusive() && !src.getExclusive())
    return emitOpError("exclusive reborrow through a shared reference");
  if (r.getExclusive() != getExclusive())
    return emitOpError("result reference mutability differs from the borrow's");
  auto t = placeType(*this, src.getPointee(), getPath(), getIndices());
  return failed(t) ? failure() : expectType(*this, r.getPointee(), *t, "borrowed reference");
}
LogicalResult LoadRefOp::verify() {
  auto t = placeType(*this, cast<RefType>(getRef().getType()).getPointee(), getPath(), getIndices());
  return failed(t) ? failure() : expectType(*this, getType(), *t, "result");
}
LogicalResult StoreRefOp::verify() {
  auto src = cast<RefType>(getRef().getType());
  if (!src.getExclusive())
    return emitOpError("store through a shared reference");
  auto t = placeType(*this, src.getPointee(), getPath(), getIndices());
  return failed(t) ? failure() : expectType(*this, getValue().getType(), *t, "stored value");
}

LogicalResult SwitchOp::verify() {
  if (getCases().size() != getCaseDests().size())
    return emitOpError("has ") << getCases().size() << " cases but " << getCaseDests().size() << " destinations";
  for (Attribute a : getCases())
    if (auto ia = dyn_cast<IntegerAttr>(a); !ia || !ia.getType().isInteger(128))
      return emitOpError("case keys must be i128 integer attributes");
  return success();
}

//===----------------------------------------------------------------------===//
// Successor operands (block arguments are checked against them by MLIR)
//===----------------------------------------------------------------------===//

SuccessorOperands BrOp::getSuccessorOperands(unsigned) { return SuccessorOperands(getArgsMutable()); }
SuccessorOperands CondBrOp::getSuccessorOperands(unsigned i) {
  return SuccessorOperands(i == 0 ? getTrueArgsMutable() : getFalseArgsMutable());
}
SuccessorOperands SwitchOp::getSuccessorOperands(unsigned) {
  // No successor takes operands; an empty range after the scrutinee.
  return SuccessorOperands(MutableOperandRange(getOperation(), 1, 0));
}

}  // namespace pegasus::pg
