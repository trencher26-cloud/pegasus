// pegasus-mlir: PIRB <-> Pegasus MLIR dialect (spec/mlir.md).
//
//   pegasus-mlir import <in.pirb> [out.mlir]   import, verify, print
//   pegasus-mlir export <in.mlir> <out.pirb>   parse, verify, export
//   pegasus-mlir verify <in.mlir>              parse and verify
//   pegasus-mlir roundtrip <in.pirb>           import, verify, print, parse,
//       verify, print again (identical text), export both modules (each
//       byte-identical to the input PIRB)
//
// Exit status: 0 ok, 1 verification or round-trip failure, 2 usage or I/O,
// 3 malformed PIRB.
#include "pegasus/mlir/Bridge.h"
#include "pegasus/mlir/PegasusDialect.h"
#include "pirb.hpp"
#include "pirb_write.hpp"

#include "mlir/IR/Verifier.h"
#include "mlir/Parser/Parser.h"
#include "llvm/Support/raw_ostream.h"

#include <cstdio>
#include <fstream>
#include <iterator>
#include <string>
#include <vector>

namespace {

bool slurp(const char *path, std::string &out) {
  std::ifstream in(path, std::ios::binary);
  if (!in) return false;
  out.assign(std::istreambuf_iterator<char>(in), std::istreambuf_iterator<char>());
  return true;
}

std::string print(mlir::ModuleOp m) {
  std::string s;
  llvm::raw_string_ostream os(s);
  m.print(os);
  os.flush();
  return s;
}

int fail(const std::string &msg, int code = 1) {
  std::fprintf(stderr, "error: %s\n", msg.c_str());
  return code;
}

}  // namespace

int main(int argc, char **argv) {
  if (argc < 3) {
    std::fprintf(stderr, "usage: pegasus-mlir (import|export|verify|roundtrip) <in> [out]\n");
    return 2;
  }
  std::string cmd = argv[1], data;
  if (!slurp(argv[2], data)) return fail(std::string("cannot read ") + argv[2], 2);
  mlir::MLIRContext ctx;
  pegasus::pg::registerDialects(ctx);

  if (cmd == "verify" || cmd == "export") {
    auto mod = mlir::parseSourceString<mlir::ModuleOp>(data, &ctx);
    if (!mod || mlir::failed(mlir::verify(*mod))) return fail("the module does not parse or verify");
    if (cmd == "verify") return 0;
    if (argc != 4) return fail("export needs an output path", 2);
    pegasus::pir::Module out;
    std::string err;
    if (!pegasus::pg::exportModule(*mod, out, err)) return fail("export: " + err);
    auto bytes = pegasus::pir::encode(out);
    std::ofstream o(argv[3], std::ios::binary);
    o.write(reinterpret_cast<const char *>(bytes.data()), static_cast<std::streamsize>(bytes.size()));
    return o ? 0 : fail(std::string("cannot write ") + argv[3], 2);
  }

  std::vector<std::uint8_t> bytes(data.begin(), data.end());
  pegasus::pir::Module pm;
  try {
    pm = pegasus::pir::decode(bytes.data(), bytes.size());
  } catch (const std::exception &e) {
    return fail(std::string("malformed PIRB: ") + e.what(), 3);
  }
  auto mod = pegasus::pg::importModule(ctx, pm);
  if (mlir::failed(mlir::verify(*mod))) return fail("the imported module does not verify");
  std::string text = print(*mod);

  if (cmd == "import") {
    if (argc == 4) {
      std::ofstream o(argv[3], std::ios::binary);
      o << text;
      return o ? 0 : fail(std::string("cannot write ") + argv[3], 2);
    }
    std::fwrite(text.data(), 1, text.size(), stdout);
    return 0;
  }
  if (cmd != "roundtrip") return fail("unknown command " + cmd, 2);

  std::string err;
  pegasus::pir::Module back;
  if (!pegasus::pg::exportModule(*mod, back, err)) return fail("export of the imported module: " + err);
  if (pegasus::pir::encode(back) != bytes) return fail("export of the imported module differs from the input PIRB");
  auto reparsed = mlir::parseSourceString<mlir::ModuleOp>(text, &ctx);
  if (!reparsed || mlir::failed(mlir::verify(*reparsed))) return fail("the printed module does not parse or verify");
  if (print(*reparsed) != text) return fail("printing the reparsed module gives different text");
  pegasus::pir::Module back2;
  if (!pegasus::pg::exportModule(*reparsed, back2, err)) return fail("export of the reparsed module: " + err);
  if (pegasus::pir::encode(back2) != bytes) return fail("export of the reparsed module differs from the input PIRB");
  std::printf("roundtrip ok: %zu functions, %zu bytes of MLIR, PIRB identical\n", pm.funcs.size(), text.size());
  return 0;
}
