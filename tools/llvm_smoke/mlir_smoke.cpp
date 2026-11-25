// Parse a func/arith module, verify it, lower it to the LLVM dialect and
// translate it to LLVM IR: the path the Pegasus MLIR backend will use.
// Exit status 0 and "ret i32 42" in the printed IR mean the package works.
#include "mlir/Pass/Pass.h"  // complete mlir::Pass before the pass factories
#include "mlir/Conversion/ArithToLLVM/ArithToLLVM.h"
#include "mlir/Conversion/FuncToLLVM/ConvertFuncToLLVMPass.h"
#include "mlir/Conversion/ReconcileUnrealizedCasts/ReconcileUnrealizedCasts.h"
#include "mlir/Dialect/Arith/IR/Arith.h"
#include "mlir/Dialect/Func/IR/FuncOps.h"
#include "mlir/Dialect/LLVMIR/LLVMDialect.h"
#include "mlir/IR/BuiltinOps.h"
#include "mlir/IR/MLIRContext.h"
#include "mlir/IR/Verifier.h"
#include "mlir/Parser/Parser.h"
#include "mlir/Pass/PassManager.h"
#include "mlir/Target/LLVMIR/Dialect/Builtin/BuiltinToLLVMIRTranslation.h"
#include "mlir/Target/LLVMIR/Dialect/LLVMIR/LLVMToLLVMIRTranslation.h"
#include "mlir/Target/LLVMIR/Export.h"
#include "llvm/IR/LLVMContext.h"
#include "llvm/IR/Module.h"
#include "llvm/Support/raw_ostream.h"

#include <string>

int main() {
  mlir::DialectRegistry registry;
  registry.insert<mlir::func::FuncDialect, mlir::arith::ArithDialect, mlir::LLVM::LLVMDialect>();
  mlir::registerBuiltinDialectTranslation(registry);
  mlir::registerLLVMDialectTranslation(registry);
  mlir::MLIRContext ctx(registry);
  ctx.loadAllAvailableDialects();

  const char *src =
      "func.func @answer(%a: i32) -> i32 {\n"
      "  %c = arith.constant 40 : i32\n"
      "  %s = arith.addi %a, %c : i32\n"
      "  return %s : i32\n"
      "}\n"
      "func.func @main() -> i32 {\n"
      "  %two = arith.constant 2 : i32\n"
      "  %r = func.call @answer(%two) : (i32) -> i32\n"
      "  return %r : i32\n"
      "}\n";
  mlir::OwningOpRef<mlir::ModuleOp> mod = mlir::parseSourceString<mlir::ModuleOp>(src, &ctx);
  if (!mod || mlir::failed(mlir::verify(*mod))) {
    llvm::errs() << "smoke: parse or verify failed\n";
    return 1;
  }
  mlir::PassManager pm(&ctx);
  pm.addPass(mlir::createArithToLLVMConversionPass());
  pm.addPass(mlir::createConvertFuncToLLVMPass());
  pm.addPass(mlir::createReconcileUnrealizedCastsPass());
  if (mlir::failed(pm.run(*mod))) {
    llvm::errs() << "smoke: lowering to the LLVM dialect failed\n";
    return 2;
  }
  llvm::LLVMContext lctx;
  std::unique_ptr<llvm::Module> lmod = mlir::translateModuleToLLVMIR(*mod, lctx);
  if (!lmod) {
    llvm::errs() << "smoke: translation to LLVM IR failed\n";
    return 3;
  }
  std::string ir;
  llvm::raw_string_ostream os(ir);
  lmod->print(os, nullptr);
  os.flush();
  if (ir.find("add i32") == std::string::npos || ir.find("define i32 @main()") == std::string::npos) {
    llvm::errs() << "smoke: unexpected LLVM IR:\n" << ir;
    return 4;
  }
  llvm::outs() << ir << "smoke: MLIR parse, verify, lower and translate OK\n";
  return 0;
}
