# Native backend v0.1

Status: implemented by `backend/` (checkpoint PIR0.4.1). Toolchain: LLVM
`llvmorg-18.1.3` (commit `c13b7485b87909fcf739f62cfa382b55407433c0`),
pinned in `toolchain.lock` and enforced (§7).

## 1. Pipeline

```
file.peg --pegasus (Rust: sema, PURM, lowering, verifier)--> verified PIR
         --PIRB (spec/pir-bridge.md)--> pegasus-native (C++, C ABI)
         --> LLVM IR text --clang -O2 + runtime--> executable
```

`pegasus build file.peg out` runs all of it, finding `pegasus-native`
through `PEGASUS_NATIVE`, the runtime through `PEGASUS_RT` (the
`pegasus_rt` library or `backend/runtime/pegasus_rt.c`) and clang through
`PEGASUS_CLANG` (default `clang`). The CMake build produces
`pegasus-native`, `libpegasus_rt` and the tests.

`pegasus-native` emits textual LLVM IR directly. `pegasus build
--backend=mlir` instead gets the LLVM IR from `pegasus-mlir lower`
(found through `PEGASUS_MLIR`), which imports the same PIRB into the MLIR
dialect and lowers it through MLIR with exactly these semantics
(spec/mlir.md §5); the direct emitter stays the default.

## 2. The native subset

Only functions reachable from the entry are translated; anything outside
the subset is refused with `PEGASUS_ERR_UNSUPPORTED` ("not supported by the
native backend yet: …") — never translated approximately.

| Supported | Not yet (refused) |
|---|---|
| all integer types (`isize`/`usize` are 64-bit), `bool`, tuples (unit included), structs, arrays, references `&T` / `&var T` | floats, `char`, `str`, enums, function values and closures, raw pointers, regions |
| constants, all binary and unary integer operations with trap / wrap / sat modes, `**`, shifts, comparisons (structural equality on tuples, structs and arrays up to 1 024 elements), lossless integer casts and `bool` → integer | `extern` calls, `call_indirect`, `closure`, `variant`, `tag`, `variant_field` |
| slots, `load`/`store`/`move`/`drop` with field and (bounds-checked) index paths; `borrow`, reborrow, `load`/`store` through references | |
| aggregates, `repeat`, `extract`, `extract_index`; direct calls; `br`, `cond_br`, `switch`, `ret`, `trap`, `unreachable` | |

## 3. Semantics

The native code reproduces the reference interpreter (spec/pir.md §5):

- **Integers** — Phase 0 §7 exactly: trapping `+ - *` via LLVM's
  overflow intrinsics; wrapping forms as plain arithmetic; saturating
  `+ -` via the saturation intrinsics and saturating `*` by the sign of the
  true product; division and remainder by zero trap; signed `MIN / -1`
  traps and `MIN % -1` is 0; a shift by at least the bit width traps; `**`
  traps unless the exact power fits (an exponent beyond `u32` traps unless
  the base is an unsigned 0 or 1); unsigned negation traps. 128-bit
  division and remainder are computed by an IR helper (restoring
  shift-subtract division) instead of LLVM's `i128` `div`/`rem`, which
  lower to compiler-rt calls (`__divti3`, …) that MSVC's runtime lacks; the
  same code runs on every platform.
- **Places and references** — slots are stack allocations; a reference is
  the address of a place; `inout` writes reach the caller's storage. The
  verifier's provenance rule guarantees no reference outlives its frame, so
  the interpreter's dangling-reference trap is unreachable for verified
  PIR.
- **Moves and drops** — `move` reads the place; `drop` has no observable
  effect, because no type in the subset has a destructor (the interpreter's
  drop trace is a test aid, not program output). PURM guarantees no use of a
  moved value. Destructors will make `drop` a call.
- **Traps** — the interpreter's messages, byte for byte, on standard error
  as `trap: <message>`, exit status 101. Native code prints no backtrace.
- **Result** — `main`'s value is printed in the interpreter's format
  (`interp::show`), followed by a newline; `()` prints nothing.

## 4. Runtime (`backend/runtime/pegasus_rt.c`)

`pg_rt_trap`, `pg_rt_trap_shift`, `pg_rt_trap_index` (messages with
run-time values), `pg_rt_print_int` (128-bit decimal), `pg_rt_print_str`,
the call-depth counter `pg_rt_depth`, and `pg_rt_run`, which runs the
program on a thread with a 4 GiB reserved stack. Every function checks the
depth counter against **100 000 frames**, the interpreter's limit, and traps
with the same `stack overflow (call depth limit reached)` message one frame
past it.

## 5. Differences from the interpreter

- The interpreter's **step limit** guards tests against runaway loops; it
  is not language semantics. Native code has none: a program the
  interpreter stops at the step limit runs on natively.
- A program whose frames are so large that 100 000 of them exceed the
  4 GiB stack would fault before reaching the depth limit (not observed;
  frames of a few kilobytes fit).
- No backtrace on a trap.

## 6. Verification (differential)

`tools/native_diff.py` (ctest `native_differential`) compares, for every
program, the interpreter and the native executable: stdout, exit status
and the trap message must be identical. Sources: `tests/native/*.peg`
(must all build), `tests/run/*.peg` (build or be refused as unsupported),
random single integer operations over all types and operators, generated
programs (functions, `inout` parameters, borrowed returns, structs, arrays,
loops, branches, mixed overflow modes) and, with `--mutated`, fuzzing
corpora.

## 7. Toolchain pin

`toolchain.lock` pins one llvm-project commit for LLVM, clang and MLIR.

- **Provisioning** (`tools/provision_llvm.py`, both CI jobs): fetch the
  commit by its full id, require `HEAD` to equal it and the release tag to
  peel to it, build clang + MLIR from that checkout with
  `LLVM_APPEND_VC_REV=ON`, require the installed clang to report the
  version and commit, and write `pegasus-toolchain.json` with the SHA-256 of
  the clang binary. CI caches the install by the lock's hash.
- **Enforcement** (`tools/check_toolchain.py`, run at configure time and by
  the `toolchain_pinned` test): the clang must report the pinned version
  and commit; a different commit always fails. With `PEGASUS_LLVM_ROOT`
  (CI) the stamp must match the commit and the clang binary's hash. A
  distribution clang reports no commit and is refused unless
  `PEGASUS_ALLOW_UNPINNED_LLVM=ON`, which checks the version only and says
  that the commit is not enforced.

### 7.1 Prebuilt toolchain packages

Ordinary CI does not build LLVM. The toolchain builder
(`.github/workflows/toolchain.yml`) has two jobs. `build` (read-only
token; on a pull request with the `build-toolchain` label, or on a manual
run) builds the source pin with `tools/provision_llvm.py` on pinned images
(Linux: `ubuntu-24.04`, GCC 13.3, since `llvmorg-18.1.3` does not compile
with GCC 15; Windows: `windows-2025`, Server 2025 with Visual Studio 2026,
MSVC 19.52, which a consumer linking the package's C++ libraries must
match or exceed), runs the package-consumer smoke test on the install
prefix, packages it
(`tools/package_llvm.py pack`: a deterministic `tar.xz` of the whole
prefix, split into parts under GitHub's 2 GiB asset limit, plus a manifest
recording the source pin, the hashes, the stamp's clang hash and the build
host), and hands it over as a run artifact. `publish`, the only job with
write, OIDC and attestation permissions, runs only for a manual run on
`main`: it attests the build provenance of every part and publishes the
parts to the release `toolchain-llvm-<version>-<commit12>`. Published parts are
immutable: an existing asset with a different digest fails the builder.

`toolchain-packages.lock.json` pins each platform's package: release,
archive, SHA-256 and size of the archive and of every part, the source pin
it was built from (which must equal `toolchain.lock`), and the builder
workflow and ref whose attestation must verify. It changes only by a reviewed
commit (`tools/package_llvm.py record`).

CI runs `tools/fetch_llvm_package.py`: download the parts, check every
part's size and SHA-256 and the archive's SHA-256 against the lock, verify
every part's provenance attestation (`gh attestation verify
--signer-workflow --source-ref`), unpack with Python's `data` filter (nothing outside the
destination), and run `check_toolchain.py --require-stamp`. Then
`tools/package_llvm.py smoke` checks that the unpacked package is complete:
its clang compiles and runs a C program; `mlir-opt`, `mlir-translate` and
`mlir-tblgen` run; and `tools/llvm_smoke` configures through the installed
MLIR CMake package, builds against the installed headers and libraries, and
lowers a func/arith module to LLVM IR. A missing package, a hash mismatch or
a failed attestation fails CI; there is no fallback to an unverified
toolchain.
