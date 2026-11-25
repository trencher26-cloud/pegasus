// pegasus-native: PIRB file -> LLVM IR file, through the C ABI only.
// Usage: pegasus-native <in.pirb> <out.ll>
// Exit status: 0 ok, 1 unsupported, 2 usage, 3 malformed input.
#include "pegasus/backend.h"

#include <cstdio>
#include <string>
#include <fstream>
#include <iterator>
#include <vector>

int main(int argc, char **argv) {
  if (argc == 2 && std::string(argv[1]) == "--version") {
    std::printf("pegasus-native %s\n", pegasus_backend_version());
    return 0;
  }
  if (argc != 3) {
    std::fprintf(stderr, "usage: pegasus-native <in.pirb> <out.ll>\n");
    return 2;
  }
  std::ifstream in(argv[1], std::ios::binary);
  if (!in) {
    std::fprintf(stderr, "error: cannot read %s\n", argv[1]);
    return 2;
  }
  std::vector<uint8_t> bytes((std::istreambuf_iterator<char>(in)), std::istreambuf_iterator<char>());
  char *ir = nullptr, *err = nullptr;
  auto st = pegasus_backend_emit_llvm(bytes.data(), bytes.size(), &ir, &err);
  if (st != PEGASUS_OK) {
    std::fprintf(stderr, "error: %s\n", err ? err : "unknown failure");
    pegasus_backend_free(err);
    return st == PEGASUS_ERR_UNSUPPORTED ? 1 : st == PEGASUS_ERR_FORMAT ? 3 : 2;
  }
  std::ofstream out(argv[2], std::ios::binary);
  out << ir;
  pegasus_backend_free(ir);
  if (!out) {
    std::fprintf(stderr, "error: cannot write %s\n", argv[2]);
    return 2;
  }
  return 0;
}
