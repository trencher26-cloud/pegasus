// The Pegasus MLIR dialect (spec/mlir.md).
#pragma once

#include "mlir/Bytecode/BytecodeOpInterface.h"
#include "mlir/IR/BuiltinAttributes.h"
#include "mlir/IR/BuiltinTypes.h"
#include "mlir/IR/Dialect.h"
#include "mlir/IR/OpDefinition.h"
#include "mlir/IR/OpImplementation.h"
#include "mlir/IR/SymbolTable.h"
#include "mlir/Interfaces/ControlFlowInterfaces.h"

#include "pegasus/mlir/PegasusDialect.h.inc"

#define GET_TYPEDEF_CLASSES
#include "pegasus/mlir/PegasusTypes.h.inc"

#define GET_OP_CLASSES
#include "pegasus/mlir/PegasusOps.h.inc"
