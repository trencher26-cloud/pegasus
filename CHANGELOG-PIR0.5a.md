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

## Third review of PR #3

The dialect accepted types broader than PIR's verifier; now it matches
`compiler/pir/src/verify.rs` exactly: array index projections in paths and
the `pg.extract_index` index must be `usize`, and `pg.tag` yields `u32`.
Found while checking against `verify.rs`: `pg.extract` accepted arrays,
which PIR does not (`Extract` is a field projection of a tuple or struct;
array elements use `extract_index`); it is now refused. 5 new invalid
modules (20 in all), each accepted by the previous verifier. No false
positives: 30 corpus and 858 fuzz-mutated modules still verify and
round-trip.

## Fourth review of PR #3

`pg.const` accepted integer values outside their type (`256 : i128` as
`!pg.int<i8>`). It now applies PIR's rule `v & !mask(t) == 0`: the constant
is the type's bit pattern, so `-1` for a `u32` is `0xffffffff`, never 128
one bits. A `char` constant must also be a Unicode scalar value (a Rust
`char` always is). 3 new invalid modules (23 in all), each accepted by the
previous verifier. No false positives: 30 corpus and 858 fuzz-mutated
modules still verify and round-trip; ctest 18/18.

## Fifth review of PR #3 (consolidated audit, six findings, all fixed)

- **P1, export claimed verification it never did.** `pegasus-mlir export`
  set the PIRB verified flag after only MLIR's structural verification, so
  an MLIR-valid but PIR-invalid module became a "verified" file the native
  backend would consume. Export now writes flags 0 (unverified), which every
  consumer refuses; the new `pegasus pirb-verify` (`bridge::certify`) runs
  the Rust verifier and writes the verified file. The verifier is not
  reimplemented in C++. Four new `backend/mlir/tests/semantic_invalid`
  modules (bool addition, lossy cast, duplicate switch key, a returned
  reference to a local slot) verify as MLIR, export with flag byte 0, are
  rejected by `pirb-verify` with the Rust message (no file written), and are
  refused by `pegasus-native`. Corpus and fuzz round trips now go through
  the whole chain and must be byte-identical to the original.
- **P2, `pg.pirb_minor`.** Export accepted any minor and wrote 1.0 anyway;
  it now refuses anything above 0 (unit tests: 1 and 70000). The C++
  reader, writer and exporter share `kPirbMajor`/`kPirbMinor`.
- **P2, spec.** spec/mlir.md §1 states the contract (MLIR verification is
  structural; semantic validity is the Rust verifier's) and
  spec/pir-bridge.md §2.3 documents unverified files and `pirb-verify`.
- **P2, type kinds.** `!pg.int` and `!pg.float` had no verifier, and the
  accessors indexed tables with unchecked kinds. Both types now verify
  (0–11, 0–3), accessors are safe on any value, `pg.const` and export check
  the kind.
- **P2, round-trip script.** Compile failures were silently counted as
  skips; a simulated regression in `tests/run/borrows.peg` passed (exit 0)
  with the old script and fails with the new one. Only the explicit no-main
  outcome in `tests/sema/accept` is skipped, listed by name.
- **P2, char constants.** No Rust verifier change: a Rust `char` is a
  Unicode scalar by construction and the decoder builds it with
  `char::from_u32`, so a verifier check would be dead code. Documented at
  the verifier, and new tests show the decoder (and so `certify`) refuses
  surrogates and values above U+10FFFF.

Verified locally: cargo fmt, clippy `-D warnings`, workspace tests, ctest
18/18, round trips of 30 corpus and 858 fuzz-mutated programs through the
certification chain, byte-identical.
