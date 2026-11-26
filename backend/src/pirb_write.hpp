// PIRB writer (spec/pir-bridge.md §2): the C++ mirror of the Rust encoder in
// compiler/pir/src/bridge.rs. Used by the MLIR exporter.
//
// Trust boundary (spec/pir-bridge.md §2.3): C++ never runs the PIR verifier,
// so it can never write a VERIFIED file. encodeUnverified refuses a module
// whose flags are not kFlagUnverified, or whose minor version this writer
// does not write; every file it produces has flags 0 and must be certified
// with `pegasus pirb-verify` before any consumer accepts it. Apart from the
// flag, a module read with pir::decode re-encodes byte-identically.
#pragma once

#include "pirb.hpp"

#include <cstdint>
#include <string>
#include <vector>

namespace pegasus::pir {

// On success fills `out` and returns true; otherwise sets `error`, leaves
// `out` empty and returns false.
bool encodeUnverified(const Module &m, std::vector<std::uint8_t> &out, std::string &error);

}  // namespace pegasus::pir
