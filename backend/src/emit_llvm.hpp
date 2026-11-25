// PIR -> textual LLVM IR for the native subset (spec/native.md).
#pragma once

#include "pirb.hpp"

#include <stdexcept>
#include <string>

namespace pegasus::native {

// The module uses PIR outside the native subset.
struct Unsupported : std::runtime_error {
  using std::runtime_error::runtime_error;
};

// Translate the entry function and everything it calls. Throws Unsupported.
std::string emit_llvm(const pir::Module &m);

}  // namespace pegasus::native
