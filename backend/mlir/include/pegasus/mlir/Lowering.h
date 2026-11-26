// Lowering of the Pegasus dialect to LLVM (spec/mlir.md §5).
//
// The input is a module imported from VERIFIED PIRB (pir::decode refuses
// anything else; spec/pir-bridge.md §2.3). The lowering is a dialect
// conversion `pg` -> `llvm` that reproduces the direct LLVM IR emitter
// (backend/src/emit_llvm.cpp, spec/native.md) exactly: the same native
// subset, integer semantics, trap messages, runtime calls, call-depth limit
// and result printing. The two paths are compared against the reference
// interpreter by tools/native_diff.py.
#pragma once

#include "mlir/IR/BuiltinOps.h"
#include "mlir/IR/MLIRContext.h"

#include <string>

namespace pegasus::pg {

// Load the dialects the lowering produces (call before parsing or importing).
void registerLoweringDialects(mlir::MLIRContext &ctx);

enum class LowerStatus {
  Ok,
  NoEntry,        // the module has no entry function
  Unsupported,    // outside the native subset (spec/native.md §2)
  Inconsistent,   // the module contradicts its verification
  Failed,         // the conversion itself failed (a lowering bug)
};

struct LowerResult {
  LowerStatus status = LowerStatus::Ok;
  std::string message;
};

// Lower `mod` in place to the LLVM dialect, with the C entry point `main`.
LowerResult lowerToLLVM(mlir::ModuleOp mod);

// Translate a module in the LLVM dialect to textual LLVM IR.
bool translateToLLVMIR(mlir::ModuleOp mod, std::string &ir, std::string &error);

}  // namespace pegasus::pg
