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

## Review of PR #3 (four findings, all fixed)

1. **Malformed module metadata crashed the exporter.** Export now runs the
   MLIR verifier first, then checks every custom attribute it reads
   (`pg.pirb_minor` present and within 16 bits, `pg.entry` a symbol of a
   defined parameterless function, `pg.structs`/`pg.enums` entries and their
   arrays, `pg.block_params` entries, struct/enum ids in types, `char`
   scalars, nothing but `func.func` in the module) and reports an error
   instead of repairing or crashing. Pre-fix: segmentation fault.
2. **Value ids silently narrowed.** Every id stored in an `i64` attribute
   (value, block-parameter, struct, enum, variant, field, path) must fit in
   32 bits. Pre-fix: `pg.v = 4294967296` exported as value 0.
3. **The entry trampoline was trusted.** It is dropped only if it holds
   `pg.slot` declarations and one `pg.br` to the next block forwarding the
   function's arguments in order. Pre-fix: a `pg.const` in it was silently
   dropped, producing PIRB the reader rejects.
4. **MSVC multi-config Debug.** The guard is replaced by enforcement: with
   MSVC, every target linking MLIR or `pegasus_backend` uses the release
   runtime `/MD` in every configuration. Windows CI now builds and runs
   `pegasus-mlir-unit`, `abi_test` and `pegasus-native` in the Debug
   configuration of a Ninja Multi-Config build.

Tests: 16 new negative cases in `mlir_unit` (each module parses and
verifies; export must fail with a named reason), checked against the pre-fix
exporter, which crashes or silently mis-exports them.

## Second review of PR #3 (two findings, both fixed)

1. **`pg.v` width.** `pg.v` is a discardable attribute that ODS does not
   constrain; export now requires an `i64` before reading it (tests: `i128`
   values in and beyond 64 bits).
2. **Aggregate and enum consistency.** New dialect verifiers (so every MLIR
   consumer, and export, which verifies first, gets them): `pg.struct` and
   `pg.variant` ids must equal the result type's, with operands matching the
   definition's fields; `pg.variant_field` checks variant and field bounds
   and the result type; `pg.tuple`, `pg.array`, `pg.repeat`, `pg.extract`,
   `pg.extract_index` and `pg.tag` check their result types, lengths and
   indices. 8 new invalid modules; the previous exporter exported each of
   the checked ones (exit 0), producing inconsistent PIRB.

No false positives: 30 corpus and 858 fuzz-mutated modules still verify and
round-trip byte-identically.
