# Pegasus — checkpoint PIR0.4: verifier hardening and native backend foundation

Based on PIR0.3 (SHA-256 `f11abd2ba4ba13a22281f95c8d245ac9c67c7e8135f547ce1f158d303a80e262`),
which is unchanged. Lexer, parser (P1.4), grammar and corpus are unchanged.

## 1. Verifier: the reference-provenance hole (review of PIR0.3)

The review showed a malformed function that stores a borrow of its own
slot in a reference-typed slot, `move`s it back out and returns it: the
provenance analysis had no `move` case, so the returned reference had an
empty origin and was accepted. Fixed on every axis the review named:

- **Reference slots hold reference parameters only.** A slot of reference
  type must be written exactly once, as a whole, in the entry block, with
  an entry-block parameter of its type, and not read before that write.
- **No `move` or `drop` of a reference slot** (and, as before, no borrow).
- **Origins flow through every slot operation**: whole and partial stores,
  `load` and `move`.
- **Fail closed**: a returned reference needs a *proven* parameter origin
  (`param && !local`); an unknown origin is rejected, not accepted.
- **Fixed point, not a cap**: the 1 000-round cap that stopped silently is
  replaced by iteration to the least fixed point with a proven bound
  (`2 × (values + slots) + 1` rounds); exceeding it is an error.
- Extern functions cannot take or return references.

Tests: the review's exact sequence (with `move` and with `load`), an
overwritten parameter slot, `drop` / read-before-init / missing write of a
reference slot, a reference with no provenance, extern signatures. New
mutation kinds: `drop` of a reference slot, a second write to it, a
reference-slot `load` turned into `move`, and returning a borrow of an own
slot. The mutation test now also covers tests/native: 6 412 mutants
(PIR0.3: 4 587), 395 of them reference-discipline, all rejected.

## 2. The PIR bridge (frozen): PIRB 1.0 and the backend C ABI 1.0

spec/pir-bridge.md. A versioned binary serialization of the whole module —
types, signatures, reference parameters and returns, slots, values, every
instruction including borrows, moves and drops, and the CFG — with a
verified flag and a CRC-32 trailer. `bridge::encode` refuses unverified
modules; the Rust reader also re-verifies what it decodes. The C++ reader
validates structure only and never re-implements typing or PURM; the
emitter checks each typing fact it relies on and fails closed. `pegasus
pirb file.peg out.pirb` writes a file.

## 3. Toolchain pin

`toolchain.lock`: LLVM/MLIR `llvmorg-18.1.3`, commit
`c13b7485b87909fcf739f62cfa382b55407433c0` (verified against the
llvm-project repository), Rust 1.97.1. `tools/check_toolchain.py` (ctest
`toolchain_pinned`) checks the clang a build uses.

## 4. Native backend v0.1 (spec/native.md)

The first C++ code in the project: `backend/` — PIRB reader, LLVM IR
emitter, C ABI, `pegasus-native`, and a C runtime. `pegasus build` compiles
a program to an executable (PIRB → pegasus-native → LLVM IR → clang -O2).

Subset: all integer types, `bool`, tuples, structs, arrays, references;
every integer operation with trap / wrap / sat modes, `**`, shifts,
comparisons including structural equality, casts; slots and places with
bounds-checked indices; borrows, reborrows, loads and stores through
references; direct calls; all terminators. Anything else is refused as
"not supported by the native backend yet", never approximated. The trap
messages are the interpreter's, exit status 101; the 100 000-frame call
depth limit is enforced natively with the same message.

## 5. Differential testing (native vs interpreter)

`tools/native_diff.py` (ctest `native_differential`: corpus + 300 integer
operations + 60 generated programs). Stdout, exit status and trap message
must be identical. Recorded for this checkpoint:

| Source | Result |
|---|---|
| tests/native (9 programs, all must build) | 9/9 match |
| tests/run (15) | 11 match (incl. `borrows.peg` and every trap test), 4 refused as unsupported (chars/floats, enums, closures) |
| integer operations, all types and operators | 1 500 (seed 3) + 3 000 (seed 21): all match |
| generated programs (inout, borrowed returns, structs, arrays, loops) | 150 (seed 2) + 500 (seed 21): all match |
| fuzz-mutated programs (`--mutated`) | 643 accepted and in the subset: all match |

## 6. Found and fixed while building it

- C++ reader: the version bytes were combined in one expression whose
  operand evaluation order is unspecified; one compiler read them reversed
  (found by the sanitizer build of the ABI test). Each read is now its own
  statement.
- The ABI test's corruption sweep crashed the emitter on a corrupted file
  that still claimed to be verified (a non-array operand of
  `extract_index`). Hence the CRC-32 trailer and the emitter's fail-closed
  checks; the sweep (truncations + 180 000 resealed corruptions over the
  native corpus) is clean under AddressSanitizer and
  UndefinedBehaviorSanitizer.

## 7. Recorded, not changed

- **DEFERRED S4 (parser/spec):** rule B counts a balanced `{…}` group as a
  generic-argument atom, so `while i < 5 {` with a body longer than 256
  tokens is a syntax error. The spec text permits this reading; a fix needs
  your decision. Found by the program generator, which parenthesizes
  conditions.
- DEFERRED NATIVE-1…6: MLIR (not available in this environment), subset
  gaps, no native backtrace, no native step limit (an interpreter guard),
  Windows backend untested, `drop` emits nothing (no destructors yet).

## Verification

| Check | Result |
|---|---|
| `cargo fmt --check`, `cargo clippy --all-targets --locked -D warnings` | clean |
| `cargo test --workspace --locked` | 104 passed, 0 failed |
| Verifier mutation test | 6 412 mutants, all rejected |
| CMake + ctest (fresh Release build, GCC 13.3, clang 18.1.3) | 14/14 passed |
| ABI test under ASan + UBSan, all 9 native fixtures | 0 failures, no leaks |
| Integer oracle | 3 000 operations: 0 mismatches |
| `sema_fuzz` / `pir_fuzz` (42 000 programs, 7 corpora) | 0 panics, 0 verifier failures, every module round-trips through PIRB |
| Windows CRLF emulation (honoring `.gitattributes`) | checkout intact; 104 passed; C++ builds |

Gate 1 (real Linux/Windows CI on Rust 1.97.1, and now the C++ backend on
Windows) remains a user action.
