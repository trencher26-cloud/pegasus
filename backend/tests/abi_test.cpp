// C ABI contract test: a valid PIRB file translates; every truncation and a
// sweep of single-byte corruptions are refused or translated without
// crashing; null outputs are usage errors; foreign versions are refused.
// Usage: abi_test <valid.pirb> <unsupported.pirb> <bool_eq.pirb>
#include "pegasus/backend.h"

#include <cstdio>
#include <cstring>
#include <fstream>
#include <iterator>
#include <string>
#include <vector>

static int failures = 0;
#define EXPECT(c, what)                                   \
  do {                                                    \
    if (!(c)) {                                           \
      std::fprintf(stderr, "FAILED: %s (%s)\n", what, #c); \
      ++failures;                                         \
    }                                                     \
  } while (0)

// Recompute the CRC-32 trailer after an edit (spec/pir-bridge.md §2).
static void reseal(std::vector<uint8_t> &b) {
  uint32_t c = 0xffffffffu;
  for (std::size_t i = 0; i + 4 < b.size(); ++i) {
    c ^= b[i];
    for (int k = 0; k < 8; ++k) c = (c & 1) ? (c >> 1) ^ 0xedb88320u : c >> 1;
  }
  c = ~c;
  for (int k = 0; k < 4; ++k) b[b.size() - 4 + k] = uint8_t(c >> (8 * k));
}

static std::vector<uint8_t> slurp(const char *p) {
  std::ifstream in(p, std::ios::binary);
  return std::vector<uint8_t>((std::istreambuf_iterator<char>(in)), std::istreambuf_iterator<char>());
}

static pegasus_status emit(const std::vector<uint8_t> &b, std::string *ir = nullptr, std::string *err = nullptr) {
  char *o = nullptr, *e = nullptr;
  auto st = pegasus_backend_emit_llvm(b.data(), b.size(), &o, &e);
  if (st == PEGASUS_OK) {
    EXPECT(o && !e, "ok returns IR only");
    if (ir) *ir = o;
  } else {
    EXPECT(!o && e, "failure returns a message only");
    if (err && e) *err = e;
  }
  pegasus_backend_free(o);
  pegasus_backend_free(e);
  return st;
}

int main(int argc, char **argv) {
  if (argc != 4) return 2;
  auto good = slurp(argv[1]);
  auto unsup = slurp(argv[2]);
  auto beq = slurp(argv[3]);
  EXPECT(good.size() > 8, "fixture present");
  EXPECT(std::strncmp(pegasus_backend_version(), "1.", 2) == 0, "ABI major version 1");

  std::string ir, err;
  EXPECT(emit(good, &ir) == PEGASUS_OK, "valid module translates");
  EXPECT(ir.find("define i32 @main()") != std::string::npos, "IR defines main");
  EXPECT(emit(unsup, nullptr, &err) == PEGASUS_ERR_UNSUPPORTED, "unsupported PIR is reported as such");
  EXPECT(err.find("not supported by the native backend yet") != std::string::npos, "unsupported message");

  // Review finding 1 (PIR0.4): `bool` has no ordering. Rewrite the
  // fixture's only `cmp.eq` on booleans into each ordering and reseal: the
  // file still claims to be verified, and the backend must fail closed
  // instead of emitting `icmp ult i1`.
  {
    std::vector<std::size_t> at;
    for (std::size_t i = 0; i + 5 < beq.size(); ++i)
      if (beq[i] == 1 && beq[i + 1] < 16 && beq[i + 2] == 3 && beq[i + 3] == 0 && beq[i + 4] < 16 && beq[i + 5] < 16)
        at.push_back(i + 3);
    EXPECT(at.size() == 1, "fixture has exactly one cmp.eq");
    EXPECT(emit(beq) == PEGASUS_OK, "bool equality translates");
    for (uint8_t op = 2; op <= 5 && at.size() == 1; ++op) {  // lt, le, gt, ge
      auto c = beq;
      c[at[0]] = op;
      reseal(c);
      EXPECT(emit(c, nullptr, &err) == PEGASUS_ERR_FORMAT, "bool ordering refused");
      EXPECT(err.find("ordering comparison on bool") != std::string::npos, "bool ordering message");
    }
  }

  char *o = nullptr;
  EXPECT(pegasus_backend_emit_llvm(good.data(), good.size(), nullptr, &o) == PEGASUS_ERR_USAGE, "null out_ir");
  EXPECT(pegasus_backend_emit_llvm(good.data(), good.size(), &o, nullptr) == PEGASUS_ERR_USAGE, "null out_error");

  auto v = good;
  v[10] ^= 1;
  EXPECT(emit(v, nullptr, &err) == PEGASUS_ERR_FORMAT && err.find("checksum") != std::string::npos, "checksum checked");
  v = good;
  v[4] = 2;
  reseal(v);
  EXPECT(emit(v, nullptr, &err) == PEGASUS_ERR_FORMAT && err.find("version") != std::string::npos, "major 2 refused");
  v = good;
  v[6] = 1;
  reseal(v);
  EXPECT(emit(v) == PEGASUS_ERR_FORMAT, "newer minor refused");
  v = good;
  v[8] = 0;  // flags: not verified
  reseal(v);
  EXPECT(emit(v, nullptr, &err) == PEGASUS_ERR_FORMAT && err.find("verified") != std::string::npos, "unverified module refused");
  // Every reserved flag bit (spec/pir-bridge.md §2), alone and with bit 0.
  int flag_cases = 0;
  for (int bit = 1; bit < 64; ++bit) {
    for (uint64_t flags : {1ull | (1ull << bit), 1ull << bit}) {
      std::vector<uint8_t> f(good.begin(), good.begin() + 8);
      for (uint64_t x = flags;;) {
        uint8_t byte = x & 0x7f;
        x >>= 7;
        f.push_back(x ? byte | 0x80 : byte);
        if (!x) break;
      }
      f.insert(f.end(), good.begin() + 9, good.end());
      reseal(f);
      EXPECT(emit(f, nullptr, &err) == PEGASUS_ERR_FORMAT, "reserved or missing flag refused");
      EXPECT(err.find(flags & 1 ? "reserved PIRB flag bits" : "not marked verified") != std::string::npos,
             "flag message");
      ++flag_cases;
    }
  }
  EXPECT(good[8] == 1 && flag_cases == 126, "flag sweep ran");
  v = good;
  v.insert(v.end() - 4, 0);
  reseal(v);
  EXPECT(emit(v) == PEGASUS_ERR_FORMAT, "trailing byte refused");

  std::size_t prefixes = 0, corrupt = 0;
  for (std::size_t k = 0; k < good.size(); ++k) {
    std::vector<uint8_t> p(good.begin(), good.begin() + k);
    EXPECT(emit(p) == PEGASUS_ERR_FORMAT, "every truncation is malformed");
    ++prefixes;
  }
  uint32_t x = 0x9e3779b9u;
  for (std::size_t k = 0; k < 20000; ++k) {
    x ^= x << 13;
    x ^= x >> 17;
    x ^= x << 5;
    auto c = good;
    c[x % (c.size() - 4)] ^= uint8_t(1 + (x >> 24) % 255);
    EXPECT(emit(c) == PEGASUS_ERR_FORMAT, "corruption detected by the checksum");
    reseal(c);
    auto st = emit(c);  // resealed: reaches the decoder and emitter; any status, never a crash
    EXPECT(st == PEGASUS_OK || st == PEGASUS_ERR_FORMAT || st == PEGASUS_ERR_UNSUPPORTED, "status in range");
    ++corrupt;
  }
  std::printf("abi_test: %zu truncations, %zu corruptions, %d failures\n", prefixes, corrupt, failures);
  return failures ? 1 : 0;
}
