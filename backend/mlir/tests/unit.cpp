// Unit tests for the PIRB <-> MLIR bridge on hand-built modules: shapes the
// corpus does not produce (spec/mlir.md §3).
#include "pegasus/mlir/Bridge.h"
#include "pirb_write.hpp"

#include "mlir/IR/Verifier.h"
#include "mlir/Parser/Parser.h"
#include "llvm/Support/raw_ostream.h"

#include <cstdio>
#include <map>

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
  CHECK(err.find("missing module attribute pg.pirb_minor") != std::string::npos, "with a reason");

  // Malformed or edited modules (review of PR #3): each parses and passes
  // mlir::verify, and export must fail closed with the named reason.
  const std::string base =
      "module attributes {pg.pirb_minor = 0 : i64, pg.structs = STRUCTS, pg.enums = [], pg.entry = @\"f0.main\"} {\n"
      "  func.func @\"f0.main\"() -> !pg.int<i32> attributes {pg.name = \"main\", pg.block_params = BPARAMS} {\n"
      "    ENTRY"
      "    %c = \"pg.const\"() <{value = 7 : i128}> {pg.v = VID : i64} : () -> !pg.int<i32>\n"
      "    \"pg.ret\"(%c) : (!pg.int<i32>) -> ()\n"
      "  }\n"
      "}\n";
  auto variant = [&](std::initializer_list<std::pair<std::string, std::string>> subs) {
    std::string t = base;
    std::map<std::string, std::string> v = {{"STRUCTS", "[]"}, {"BPARAMS", "[array<i64>]"}, {"ENTRY", ""}, {"VID", "0"}};
    for (auto &kv : subs) v[kv.first] = kv.second;
    for (auto &kv : v)
      for (size_t at; (at = t.find(kv.first)) != std::string::npos;) t.replace(at, kv.first.size(), kv.second);
    return t;
  };
  auto expectExport = [&](const std::string &text, const char *needle, const char *what) {
    auto m2 = mlir::parseSourceString<mlir::ModuleOp>(text, &ctx);
    if (!m2 || mlir::failed(mlir::verify(*m2))) {
      std::fprintf(stderr, "FAILED: %s (test module does not parse/verify)\n", what);
      ++failures;
      return;
    }
    pir::Module out2;
    std::string e2;
    bool ok = pg::exportModule(*m2, out2, e2);
    if (!needle) {
      CHECK(ok, what);
      return;
    }
    if (ok || e2.find(needle) == std::string::npos) {
      std::fprintf(stderr, "FAILED: %s (export %s: '%s')\n", what, ok ? "succeeded" : "failed", e2.c_str());
      ++failures;
    }
  };
  expectExport(variant({}), nullptr, "the baseline module exports");
  // Finding 1: module metadata.
  expectExport(variant({{"STRUCTS", "[5 : i64]"}}), "malformed pg.structs[0]", "a struct entry that is not a dictionary");
  expectExport(variant({{"STRUCTS", "[{name = \"S\", field_names = [\"a\", \"b\"], field_types = [!pg.int<i32>]}]"}}),
               "differ in length", "field names and types of different lengths");
  expectExport(variant({{"STRUCTS", "[{name = \"S\", field_names = [\"a\"]}]"}}), "missing pg.structs[0].field_types",
               "a struct without field types");
  expectExport(variant({{"STRUCTS", "[{name = \"S\", field_names = [\"a\"], field_types = [3 : i64]}]"}}),
               "malformed pg.structs[0].field_types", "a field type that is not a type");
  {
    std::string t = variant({});
    t.replace(t.find("pg.pirb_minor = 0 : i64"), 23, "pg.pirb_minor = \"one\"");
    expectExport(t, "malformed module attribute pg.pirb_minor", "a non-integer pg.pirb_minor");
    t = variant({});
    t.replace(t.find("pg.pirb_minor = 0 : i64"), 23, "pg.pirb_minor = 70000 : i64");
    expectExport(t, "out of range", "pg.pirb_minor beyond 16 bits");
    t = variant({});
    const std::string entryAttr = "pg.entry = @\"f0.main\"";
    t.replace(t.find(entryAttr), entryAttr.size(), "pg.entry = 5 : i64");
    expectExport(t, "malformed module attribute pg.entry", "a pg.entry that is not a symbol");
  }
  expectExport(variant({{"BPARAMS", "[[1, 2]]"}}), "pg.block_params entry", "a malformed pg.block_params entry");
  expectExport(
      "module attributes {pg.pirb_minor = 0 : i64, pg.structs = [], pg.enums = []} {\n"
      "  func.func @\"f0.f\"(%z: !pg.struct<4>) -> !pg.int<i32> attributes {pg.name = \"f\","
      " pg.block_params = [array<i64: 0>]} {\n"
      "    %c = \"pg.const\"() <{value = 7 : i128}> {pg.v = 1 : i64} : () -> !pg.int<i32>\n"
      "    \"pg.ret\"(%c) : (!pg.int<i32>) -> ()\n"
      "  }\n"
      "}\n",
      "has no definition in pg.structs", "a struct type id beyond pg.structs");
  // Finding 2: identifiers that do not fit in 32 bits.
  expectExport(variant({{"VID", "4294967296"}}), "is not a 32-bit id", "pg.v = 2^32 (would narrow to 0)");
  expectExport(variant({{"VID", "-1"}}), "is not a 32-bit id", "a negative pg.v");
  // Finding 3: the entry trampoline must be exactly the importer's.
  const std::string tramp =
      "module attributes {pg.pirb_minor = 0 : i64, pg.structs = [], pg.enums = []} {\n"
      "  func.func @\"f0.f\"(%a: !pg.int<i32>) -> !pg.int<i32> attributes {pg.name = \"f\", pg.entry_trampoline,"
      " pg.block_params = [array<i64: 0>]} {\n"
      "    BODY"
      "  ^bb1(%x: !pg.int<i32>):\n"
      "    \"pg.ret\"(%x) : (!pg.int<i32>) -> ()\n"
      "  }\n"
      "}\n";
  auto trampVariant = [&](const std::string &body) {
    std::string t = tramp;
    t.replace(t.find("BODY"), 4, body);
    return t;
  };
  expectExport(trampVariant("    %s = \"pg.slot\"() : () -> !pg.slot<!pg.int<i32>>\n    \"pg.br\"(%a)[^bb1] : (!pg.int<i32>) -> ()\n"),
               nullptr, "a genuine trampoline exports");
  expectExport(trampVariant("    %k = \"pg.const\"() <{value = 1 : i128}> {pg.v = 9 : i64} : () -> !pg.int<i32>\n"
                            "    \"pg.br\"(%a)[^bb1] : (!pg.int<i32>) -> ()\n"),
               "contains pg.const", "a trampoline with an operation that export would drop");
  expectExport(trampVariant("    %k = \"pg.const\"() <{value = 1 : i128}> {pg.v = 9 : i64} : () -> !pg.int<i32>\n"
                            "    \"pg.br\"(%k)[^bb1] : (!pg.int<i32>) -> ()\n"),
               "not an importer entry trampoline", "a trampoline forwarding something else");

  std::printf("mlir unit tests: %d failures\n", failures);
  return failures ? 1 : 0;
}
