# Pegasus — PIR0.5 (first part): the Pegasus MLIR dialect and PIRB importer

Based on `main` at 8f05cc2 (PR #1, with PR #2). No change to the
frontend, PIR, PIRB, the direct LLVM emitter or the runtime.

## Added

- `backend/mlir`: the `pg` dialect (TableGen: 14 types, 32 operations),
  with structural verifiers and `BranchOpInterface` on branches; the PIRB
  importer and exporter (`importModule`, `exportModule`); the
  `pegasus-mlir` tool (`import`, `export`, `verify`, `roundtrip`).
- `backend/src/pirb_write.cpp`: a C++ PIRB writer mirroring the Rust
  encoder.
- `spec/mlir.md`.
- Tests: `mlir_roundtrip` (every corpus program round-trips: identical MLIR
  text after print/parse, byte-identical PIRB after export of both the
  imported and the reparsed module; 7 invalid modules rejected with their
  messages), `mlir_unit` (loop to the entry block, non-Pegasus module).
- CMake option `PEGASUS_BUILD_MLIR`, on when `PEGASUS_LLVM_ROOT` is set.

## Verified locally (Linux, the pinned package, GCC 13.3, strict pin check)

| Check | Result |
|---|---|
| ctest | 18/18 (16 before + `mlir_roundtrip`, `mlir_unit`) |
| C++ PIRB writer: decode → encode of every corpus module | 29/29 byte-identical with the Rust encoder |
| Corpus round trips | 30/30 |
| Fuzz-mutated programs accepted by the frontend | 414/414 round-trip |
| Invalid modules | 7/7 rejected with the expected message |

Windows: first MLIR build with MSVC in this pull request's CI.
