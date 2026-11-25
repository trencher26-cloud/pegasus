// The Pegasus MLIR dialect (spec/mlir.md): one operation per PIR instruction
// and terminator, and one type per PIR type. It mirrors verified PIR as
// carried by PIRB; it adds no semantics. The verifiers check structure (an
// operand that must be a reference is one, a path fits its type); typing and
// ownership were established by the Rust frontend.
#ifndef PEGASUS_DIALECT_TD
#define PEGASUS_DIALECT_TD

include "mlir/IR/OpBase.td"
include "mlir/IR/AttrTypeBase.td"
include "mlir/IR/SymbolInterfaces.td"
include "mlir/Interfaces/ControlFlowInterfaces.td"

def Pg_Dialect : Dialect {
  let name = "pg";
  let cppNamespace = "::pegasus::pg";
  let summary = "Pegasus PIR as an MLIR dialect";
  let useDefaultTypePrinterParser = 1;
}

//===----------------------------------------------------------------------===//
// Types
//===----------------------------------------------------------------------===//

class Pg_Type<string name, string typeMnemonic> : TypeDef<Pg_Dialect, name> {
  let mnemonic = typeMnemonic;
}

// Integer kinds, numbered as PIRB: i8 i16 i32 i64 i128 isize u8 u16 u32 u64
// u128 usize. `isize` stays distinct from `i64`: trap messages name it.
def Pg_IntType : Pg_Type<"Int", "int"> {
  let parameters = (ins "unsigned":$kind);
  let hasCustomAssemblyFormat = 1;
  let extraClassDeclaration = [{
    unsigned getBits() const;
    bool isSigned() const;
    ::llvm::StringRef getKindName() const;
  }];
}
// Float kinds, numbered as PIRB: f16 bf16 f32 f64.
def Pg_FloatType : Pg_Type<"Float", "float"> {
  let parameters = (ins "unsigned":$kind);
  let hasCustomAssemblyFormat = 1;
}
def Pg_BoolType : Pg_Type<"Bool", "bool">;
def Pg_CharType : Pg_Type<"Char", "char">;
def Pg_StrType : Pg_Type<"Str", "str">;
def Pg_RegionType : Pg_Type<"Region", "region">;
def Pg_TupleType : Pg_Type<"Tuple", "tuple"> {
  let parameters = (ins ArrayRefParameter<"::mlir::Type">:$elems);
  let hasCustomAssemblyFormat = 1;
}
def Pg_ArrayType : Pg_Type<"Array", "array"> {
  let parameters = (ins "::mlir::Type":$elem, "uint64_t":$len);
  let hasCustomAssemblyFormat = 1;
}
// Struct and enum types refer to the module's definitions by id
// (module attributes pg.structs / pg.enums).
def Pg_StructType : Pg_Type<"Struct", "struct"> {
  let parameters = (ins "unsigned":$id);
  let assemblyFormat = "`<` $id `>`";
}
def Pg_EnumType : Pg_Type<"Enum", "enum"> {
  let parameters = (ins "unsigned":$id);
  let assemblyFormat = "`<` $id `>`";
}
def Pg_FnType : Pg_Type<"Fn", "fn"> {
  let parameters = (ins ArrayRefParameter<"::mlir::Type">:$params, "::mlir::Type":$result);
  let hasCustomAssemblyFormat = 1;
}
def Pg_PtrType : Pg_Type<"Ptr", "ptr"> {
  let parameters = (ins "bool":$exclusive, "::mlir::Type":$pointee);
  let hasCustomAssemblyFormat = 1;
}
// A second-class reference (spec/pir.md §2.1): `!pg.ref<T>` shared,
// `!pg.ref<mut, T>` exclusive.
def Pg_RefType : Pg_Type<"Ref", "ref"> {
  let parameters = (ins "bool":$exclusive, "::mlir::Type":$pointee);
  let hasCustomAssemblyFormat = 1;
}
// A function-local storage slot holding a value of `elem`.
def Pg_SlotType : Pg_Type<"Slot", "slot"> {
  let parameters = (ins "::mlir::Type":$elem);
  let assemblyFormat = "`<` $elem `>`";
}

def Pg_AnyRef : Type<CPred<"::llvm::isa<::pegasus::pg::RefType>($_self)">, "Pegasus reference">;
def Pg_AnySlot : Type<CPred<"::llvm::isa<::pegasus::pg::SlotType>($_self)">, "Pegasus slot">;

//===----------------------------------------------------------------------===//
// Operations
//===----------------------------------------------------------------------===//

class Pg_Op<string mnemonic, list<Trait> traits = []> : Op<Pg_Dialect, mnemonic, traits>;

// Paths (place projections): `path` lists field indices; -1 marks an index
// projection whose value is the next operand of `indices`.

def Pg_ConstOp : Pg_Op<"const"> {
  let summary = "constant (integer bit pattern as i128, float bits as i64, bool, char as i32, string)";
  let arguments = (ins AnyAttr:$value);
  let results = (outs AnyType:$result);
  let hasVerifier = 1;
}
def Pg_BinOp : Pg_Op<"bin"> {
  let summary = "binary operation; kind add|sub|mul|div|rem|pow|shl|shr|and|or|xor, ov trap|wrap|sat";
  let arguments = (ins AnyType:$lhs, AnyType:$rhs, StrAttr:$kind, StrAttr:$ov);
  let results = (outs AnyType:$result);
  let hasVerifier = 1;
}
def Pg_UnOp : Pg_Op<"un"> {
  let summary = "unary operation; kind neg|not|bitnot";
  let arguments = (ins AnyType:$operand, StrAttr:$kind);
  let results = (outs AnyType:$result);
  let hasVerifier = 1;
}
def Pg_CmpOp : Pg_Op<"cmp"> {
  let summary = "comparison; pred eq|ne|lt|le|gt|ge";
  let arguments = (ins AnyType:$lhs, AnyType:$rhs, StrAttr:$pred);
  let results = (outs Pg_BoolType:$result);
  let hasVerifier = 1;
}
def Pg_CastOp : Pg_Op<"cast"> {
  let arguments = (ins AnyType:$operand);
  let results = (outs AnyType:$result);
}
def Pg_SlotOp : Pg_Op<"slot"> {
  let summary = "a function-local storage slot (entry block, in slot order)";
  let arguments = (ins OptionalAttr<StrAttr>:$name);
  let results = (outs Pg_AnySlot:$slot);
}
def Pg_LoadOp : Pg_Op<"load"> {
  let arguments = (ins Pg_AnySlot:$slot, Variadic<AnyType>:$indices, DenseI64ArrayAttr:$path);
  let results = (outs AnyType:$result);
  let hasVerifier = 1;
}
def Pg_StoreOp : Pg_Op<"store"> {
  let arguments = (ins Pg_AnySlot:$slot, AnyType:$value, Variadic<AnyType>:$indices, DenseI64ArrayAttr:$path);
  let hasVerifier = 1;
}
def Pg_MoveOp : Pg_Op<"move"> {
  let summary = "move out of a slot place (field path only)";
  let arguments = (ins Pg_AnySlot:$slot, DenseI64ArrayAttr:$path);
  let results = (outs AnyType:$result);
  let hasVerifier = 1;
}
def Pg_DropOp : Pg_Op<"drop"> {
  let arguments = (ins Pg_AnySlot:$slot);
}
def Pg_BorrowOp : Pg_Op<"borrow"> {
  let arguments = (ins Pg_AnySlot:$slot, Variadic<AnyType>:$indices, DenseI64ArrayAttr:$path, BoolAttr:$exclusive);
  let results = (outs Pg_AnyRef:$result);
  let hasVerifier = 1;
}
def Pg_BorrowRefOp : Pg_Op<"borrow_ref"> {
  let summary = "reborrow a place reached through a reference";
  let arguments = (ins Pg_AnyRef:$ref, Variadic<AnyType>:$indices, DenseI64ArrayAttr:$path, BoolAttr:$exclusive);
  let results = (outs Pg_AnyRef:$result);
  let hasVerifier = 1;
}
def Pg_LoadRefOp : Pg_Op<"load_ref"> {
  let arguments = (ins Pg_AnyRef:$ref, Variadic<AnyType>:$indices, DenseI64ArrayAttr:$path);
  let results = (outs AnyType:$result);
  let hasVerifier = 1;
}
def Pg_StoreRefOp : Pg_Op<"store_ref"> {
  let arguments = (ins Pg_AnyRef:$ref, AnyType:$value, Variadic<AnyType>:$indices, DenseI64ArrayAttr:$path);
  let hasVerifier = 1;
}
def Pg_TupleOp : Pg_Op<"tuple"> {
  let arguments = (ins Variadic<AnyType>:$elems);
  let results = (outs Pg_TupleType:$result);
}
def Pg_StructOp : Pg_Op<"struct"> {
  let arguments = (ins Variadic<AnyType>:$fields, I64Attr:$id);
  let results = (outs Pg_StructType:$result);
}
def Pg_VariantOp : Pg_Op<"variant"> {
  let arguments = (ins Variadic<AnyType>:$fields, I64Attr:$id, I64Attr:$variant);
  let results = (outs Pg_EnumType:$result);
}
def Pg_ArrayOp : Pg_Op<"array"> {
  let arguments = (ins Variadic<AnyType>:$elems);
  let results = (outs Pg_ArrayType:$result);
}
def Pg_RepeatOp : Pg_Op<"repeat"> {
  let arguments = (ins AnyType:$value, I64Attr:$count);
  let results = (outs Pg_ArrayType:$result);
}
def Pg_ExtractOp : Pg_Op<"extract"> {
  let arguments = (ins AnyType:$aggregate, I64Attr:$index);
  let results = (outs AnyType:$result);
}
def Pg_ExtractIndexOp : Pg_Op<"extract_index"> {
  let arguments = (ins Pg_ArrayType:$array, AnyType:$index);
  let results = (outs AnyType:$result);
}
def Pg_TagOp : Pg_Op<"tag"> {
  let arguments = (ins Pg_EnumType:$value);
  let results = (outs AnyType:$result);
}
def Pg_VariantFieldOp : Pg_Op<"variant_field"> {
  let arguments = (ins Pg_EnumType:$value, I64Attr:$variant, I64Attr:$index);
  let results = (outs AnyType:$result);
}
def Pg_CallOp : Pg_Op<"call"> {
  let arguments = (ins FlatSymbolRefAttr:$callee, Variadic<AnyType>:$args);
  let results = (outs Variadic<AnyType>:$results);
}
def Pg_CallIndirectOp : Pg_Op<"call_indirect"> {
  let arguments = (ins Pg_FnType:$callee, Variadic<AnyType>:$args);
  let results = (outs Variadic<AnyType>:$results);
}
def Pg_ClosureOp : Pg_Op<"closure"> {
  let arguments = (ins FlatSymbolRefAttr:$callee, Variadic<AnyType>:$captures);
  let results = (outs Pg_FnType:$result);
}

// Terminators.
def Pg_BrOp : Pg_Op<"br", [Terminator, DeclareOpInterfaceMethods<BranchOpInterface>]> {
  let arguments = (ins Variadic<AnyType>:$args);
  let successors = (successor AnySuccessor:$dest);
}
def Pg_CondBrOp : Pg_Op<"cond_br", [Terminator, AttrSizedOperandSegments,
                                    DeclareOpInterfaceMethods<BranchOpInterface>]> {
  let arguments = (ins Pg_BoolType:$condition, Variadic<AnyType>:$trueArgs, Variadic<AnyType>:$falseArgs);
  let successors = (successor AnySuccessor:$trueDest, AnySuccessor:$falseDest);
}
def Pg_SwitchOp : Pg_Op<"switch", [Terminator, DeclareOpInterfaceMethods<BranchOpInterface>]> {
  let summary = "multi-way branch: cases (i128 bit patterns) to caseDests; default otherwise";
  let arguments = (ins AnyType:$value, ArrayAttr:$cases);
  let successors = (successor AnySuccessor:$defaultDest, VariadicSuccessor<AnySuccessor>:$caseDests);
  let hasVerifier = 1;
}
def Pg_RetOp : Pg_Op<"ret", [Terminator]> {
  let arguments = (ins AnyType:$value);
}
def Pg_TrapOp : Pg_Op<"trap", [Terminator]> {
  let arguments = (ins StrAttr:$message);
}
def Pg_UnreachableOp : Pg_Op<"unreachable", [Terminator]>;

#endif // PEGASUS_DIALECT_TD
