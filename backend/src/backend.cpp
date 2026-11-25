// The C ABI (include/pegasus/backend.h) over the PIRB reader and emitter.
#include "pegasus/backend.h"

#include "emit_llvm.hpp"
#include "pirb.hpp"

#include <cstdlib>
#include <cstring>
#include <new>
#include <string>

namespace {
char *dup(const std::string &s) {
  char *p = static_cast<char *>(std::malloc(s.size() + 1));
  if (p) std::memcpy(p, s.c_str(), s.size() + 1);
  return p;
}
}  // namespace

extern "C" const char *pegasus_backend_version(void) { return "1.0 (PIRB 1.0; LLVM IR for llvmorg-18.1.3)"; }

extern "C" pegasus_status pegasus_backend_emit_llvm(const uint8_t *pirb, size_t len, char **out_ir,
                                                    char **out_error) {
  if (!out_ir || !out_error) return PEGASUS_ERR_USAGE;
  *out_ir = nullptr;
  *out_error = nullptr;
  if (!pirb && len) {
    *out_error = dup("null input");
    return PEGASUS_ERR_USAGE;
  }
  try {
    auto m = pegasus::pir::decode(pirb, len);
    if (!m.has_entry) {
      *out_error = dup("the module has no entry function");
      return PEGASUS_ERR_USAGE;
    }
    *out_ir = dup(pegasus::native::emit_llvm(m));
    return PEGASUS_OK;
  } catch (const pegasus::pir::FormatError &e) {
    *out_error = dup(std::string("malformed PIRB: ") + e.what());
    return PEGASUS_ERR_FORMAT;
  } catch (const pegasus::native::Unsupported &e) {
    *out_error = dup(std::string("not supported by the native backend yet: ") + e.what());
    return PEGASUS_ERR_UNSUPPORTED;
  } catch (const std::exception &e) {
    *out_error = dup(std::string("malformed PIRB: ") + e.what());
    return PEGASUS_ERR_FORMAT;
  }
}

extern "C" void pegasus_backend_free(char *s) { std::free(s); }
