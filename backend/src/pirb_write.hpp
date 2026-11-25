// PIRB writer (spec/pir-bridge.md §2): the C++ mirror of the Rust encoder in
// compiler/pir/src/bridge.rs. Used by the MLIR exporter; a module that was
// read with pir::decode re-encodes byte-identically.
#pragma once

#include "pirb.hpp"

#include <cstdint>
#include <vector>

namespace pegasus::pir {

std::vector<std::uint8_t> encode(const Module &m);

}  // namespace pegasus::pir
