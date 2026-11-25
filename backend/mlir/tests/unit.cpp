// Unit tests for the PIRB <-> MLIR bridge on hand-built modules: shapes the
// corpus does not produce (spec/mlir.md §3).
#include "pegasus/mlir/Bridge.h"
#include "pirb_write.hpp"

#include "mlir/IR/Verifier.h"
#include "mlir/Parser/Parser.h"
#include "llvm/Support/raw_ostream.h"

#include <cstdio>

using namespace pegasus;

static int failures = 0;
#define CHECK(c, what)                                    \
  do {                                                    \
    if (!(c)) {                                           \
      std::fprintf(stderr, "FAILED: %s\n", what);         \
      ++failures;                                         \
    }                                                     \
  } while (0)

static pir::TypeP intTy(pir::IntTy k) {
  auto t = std::make_shared<pir::Type>();
  t->kind = pir::Type::Int;
  t->ity = k;
  return t;
}
static pir::TypeP boolTy() {
  auto t = std::make_shared<pir::Type>();
  t->kind = pir::Type::Bool;
  return t;
}

// fn count(n: u32) -> u32: bb0(n) loops to itself until n == 0. bb0 has a
// predecessor, so the importer must add a separate entry block.
static pir::Module loopToEntry() {
  pir::Module m;
  m.flags = 1;
  pir::Func f;
  f.name = "count";
  f.params = {intTy(pir::IntTy::U32)};
  f.ret = intTy(pir::IntTy::U32);
  f.values = {intTy(pir::IntTy::U32), intTy(pir::IntTy::U32), boolTy(), intTy(pir::IntTy::U32)};
  pir::Block b0;
  b0.params = {0};
  pir::Inst zero;
  zero.has_result = true;
  zero.result = 1;
  zero.op = pir::Op::Const;
  zero.c.kind = pir::Const::Int;
  zero.c.ity = pir::IntTy::U32;
  pir::Inst cmp;
  cmp.has_result = true;
  cmp.result = 2;
  cmp.op = pir::Op::Cmp;
  cmp.cop = pir::CmpOp::Eq;
  cmp.a = 0;
  cmp.b = 1;
  pir::Inst one = zero;
  one.result = 3;
  one.c.bits = {1, 0};
  b0.insts = {zero, cmp, one};
  b0.term.kind = pir::Term::CondBr;
  b0.term.v = 2;
  b0.term.t = {1, {}};
  b0.term.f = {0, {1}};  // back to the entry block
  pir::Block b1;
  b1.term.kind = pir::Term::Ret;
  b1.term.v = 1;
  f.blocks = {b0, b1};
  m.funcs = {f};
  m.has_entry = false;
  return m;
}

int main() {
  mlir::MLIRContext ctx;
  pg::registerDialects(ctx);
  pir::Module m = loopToEntry();
  auto bytes = pir::encode(m);
  auto decoded = pir::decode(bytes.data(), bytes.size());
  auto mod = pg::importModule(ctx, decoded);
  CHECK(mlir::succeeded(mlir::verify(*mod)), "loop to the entry block imports and verifies");
  std::string text;
  llvm::raw_string_ostream os(text);
  mod->print(os);
  os.flush();
  CHECK(text.find("pg.entry_trampoline") != std::string::npos, "a trampoline entry block was added");
  pir::Module back;
  std::string err;
  CHECK(pg::exportModule(*mod, back, err), "export succeeds");
  CHECK(pir::encode(back) == bytes, "export drops the trampoline: PIRB identical");
  auto reparsed = mlir::parseSourceString<mlir::ModuleOp>(text, &ctx);
  CHECK(reparsed && mlir::succeeded(mlir::verify(*reparsed)), "printed module reparses and verifies");
  pir::Module back2;
  CHECK(reparsed && pg::exportModule(*reparsed, back2, err) && pir::encode(back2) == bytes,
        "reparsed module exports identically");

  // Export refuses modules that are not Pegasus modules.
  auto foreign = mlir::parseSourceString<mlir::ModuleOp>("module { func.func private @g() -> i32 }", &ctx);
  CHECK(foreign && mlir::succeeded(mlir::verify(*foreign)), "the foreign module parses");
  pir::Module out;
  CHECK(foreign && !pg::exportModule(*foreign, out, err), "a non-Pegasus module is refused");
  CHECK(err.find("missing attribute") != std::string::npos, "with a reason");

  std::printf("mlir unit tests: %d failures\n", failures);
  return failures ? 1 : 0;
}
