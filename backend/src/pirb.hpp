// PIRB reader: the C++ mirror of compiler/pir/src/ir.rs and the decoder of
// spec/pir-bridge.md. Structural validation only (identifiers in range,
// well-formed encoding); semantic validity was established by the Rust
// verifier before encoding.
#pragma once

#include <cstdint>
#include <memory>
#include <stdexcept>
#include <string>
#include <vector>

namespace pegasus::pir {

using u32 = std::uint32_t;
using u64 = std::uint64_t;

// 128-bit integers are carried as two halves (portable across compilers).
struct U128 {
  u64 lo = 0, hi = 0;
  bool operator==(const U128 &o) const { return lo == o.lo && hi == o.hi; }
};

enum class IntTy : std::uint8_t { I8, I16, I32, I64, I128, Isize, U8, U16, U32, U64, U128, Usize };
enum class FloatTy : std::uint8_t { F16, Bf16, F32, F64 };

inline unsigned bits(IntTy t) {
  switch (t) {
  case IntTy::I8: case IntTy::U8: return 8;
  case IntTy::I16: case IntTy::U16: return 16;
  case IntTy::I32: case IntTy::U32: return 32;
  case IntTy::I64: case IntTy::U64: case IntTy::Isize: case IntTy::Usize: return 64;
  case IntTy::I128: case IntTy::U128: return 128;
  }
  return 0;
}
inline bool is_signed(IntTy t) { return static_cast<int>(t) <= static_cast<int>(IntTy::Isize); }
const char *name(IntTy t);

struct Type;
using TypeP = std::shared_ptr<const Type>;

struct Type {
  enum Kind : std::uint8_t { Int, Float, Bool, Char, Str, Tuple, Array, Struct, Enum, Fn, Ptr, Region, Ref } kind;
  IntTy ity = IntTy::I8;
  FloatTy fty = FloatTy::F64;
  std::vector<TypeP> elems;  // Tuple elements; Fn parameters
  TypeP inner;               // Array element; Fn return; Ptr/Ref target
  u64 len = 0;               // Array length
  u32 id = 0;                // Struct / Enum id
  bool mut_ = false;         // Ptr / Ref mutability
  bool is_unit() const { return kind == Tuple && elems.empty(); }
};

struct Proj {
  bool index;  // false: Field(n); true: Index(value)
  u32 n;
};

enum class BinOp : std::uint8_t { Add, Sub, Mul, Div, Rem, Pow, Shl, Shr, And, Or, Xor };
enum class Ov : std::uint8_t { Trap, Wrap, Sat };
enum class UnOp : std::uint8_t { Neg, Not, BitNot };
enum class CmpOp : std::uint8_t { Eq, Ne, Lt, Le, Gt, Ge };

struct Const {
  enum Kind : std::uint8_t { Int, Float, Bool, Char, Str } kind;
  U128 bits;  // Int: two's complement bit pattern
  IntTy ity = IntTy::I8;
  u64 fbits = 0;
  FloatTy fty = FloatTy::F64;
  bool b = false;
  u32 ch = 0;
  std::string s;
};

// Instruction kinds, numbered as in the PIRB encoding.
enum class Op : std::uint8_t {
  Const, Bin, Un, Cmp, Cast, Load, Store, Tuple, Struct, Variant, Array, Repeat, Extract,
  ExtractIndex, Tag, VariantField, Call, CallIndirect, Closure, Move, Drop, Borrow, BorrowRef,
  LoadRef, StoreRef
};

struct Inst {
  bool has_result = false;
  u32 result = 0;
  Op op;
  Const c;                 // Const
  BinOp bop{};             // Bin
  Ov ov{};                 // Bin
  UnOp uop{};              // Un
  CmpOp cop{};             // Cmp
  u32 a = 0, b = 0;        // operands: Bin/Cmp (a, b); Un/Cast/Extract*/Tag/Repeat (a = v)
                           // ExtractIndex (a = v, b = i); Call/Closure (a = func);
                           // CallIndirect (a = callee); Load/Store/Move/Drop/Borrow (a = slot);
                           // BorrowRef/LoadRef/StoreRef (a = ref); Store/StoreRef (b = value)
                           // Struct/Variant (a = id; b = variant); VariantField (b = variant)
  u32 idx = 0;             // Extract / VariantField field index
  u64 n = 0;               // Repeat count
  TypeP to;                // Cast target
  std::vector<u32> args;   // aggregates, call arguments, captures, move path
  std::vector<Proj> path;  // place projections
  bool mut_ = false;       // Borrow / BorrowRef
};

struct Edge {
  u32 block = 0;
  std::vector<u32> args;
};

struct Term {
  enum Kind : std::uint8_t { Br, CondBr, Switch, Ret, Trap, Unreachable } kind;
  u32 v = 0;  // CondBr condition, Switch scrutinee, Ret value
  Edge t, f;  // Br uses t
  std::vector<std::pair<U128, u32>> cases;
  u32 dflt = 0;
  std::string msg;
};

struct Block {
  std::vector<u32> params;
  std::vector<Inst> insts;
  Term term;
};

struct Func {
  std::string name;
  bool defined = true;
  std::string abi;
  std::vector<TypeP> params;
  TypeP ret;
  std::vector<TypeP> slots;
  std::vector<std::string> slot_names;
  std::vector<TypeP> values;
  std::vector<Block> blocks;
};

struct StructDef {
  std::string name;
  std::vector<std::pair<std::string, TypeP>> fields;
};

struct EnumDef {
  std::string name;
  std::vector<std::pair<std::string, std::vector<TypeP>>> variants;
};

struct Module {
  std::uint16_t minor = 0;
  u64 flags = 0;
  bool has_entry = false;
  u32 entry = 0;
  std::vector<StructDef> structs;
  std::vector<EnumDef> enums;
  std::vector<Func> funcs;
};

struct FormatError : std::runtime_error {
  using std::runtime_error::runtime_error;
};

// CRC-32 (IEEE), the PIRB trailer.
u32 crc32(const std::uint8_t *data, std::size_t len);

// Decode and structurally validate a PIRB file; throws FormatError.
Module decode(const std::uint8_t *data, std::size_t len);

}  // namespace pegasus::pir
