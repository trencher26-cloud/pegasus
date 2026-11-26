// PIRB <-> Pegasus MLIR dialect (spec/mlir.md §3).
#pragma once

#include "mlir/IR/BuiltinOps.h"
#include "mlir/IR/MLIRContext.h"
#include "mlir/IR/OwningOpRef.h"

#include "pirb.hpp"

#include <string>

namespace pegasus::pg {

// Register the dialects a Pegasus module uses (pg, func, builtin).
void registerDialects(mlir::MLIRContext &ctx);

// Import a decoded PIRB module. Every PIR function becomes a func.func, every
// instruction and terminator one pg operation. The result is not verified
// here; callers run mlir::verify.
mlir::OwningOpRef<mlir::ModuleOp> importModule(mlir::MLIRContext &ctx, const pir::Module &m);

// Export a module back to PIR (inverse of importModule; the import's
// numbering attributes make the round trip exact). On failure, returns false
// and sets `error`.
bool exportModule(mlir::ModuleOp mod, pir::Module &out, std::string &error);

}  // namespace pegasus::pg
