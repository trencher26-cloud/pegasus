# Pegasus — CI toolchain packages (after PIR0.4.1)

CI infrastructure only. The compiler, the backend, PIRB and the source pin
(`toolchain.lock`) are unchanged.

## Why

The first real CI run (run 45593202765) failed in the Linux LLVM source
build: `ubuntu-latest` is now ubuntu-26.04 with GCC 15.2, and
llvmorg-18.1.3 does not compile with it (`llvm/ADT/SmallVector.h` uses
`uint64_t` without `<cstdint>`). Also, every CI run would have rebuilt about
5 600 LLVM/MLIR targets, and `actions/cache` saves only when the whole job
passes.

## What

- **Builder** (`.github/workflows/toolchain.yml`, on demand): builds the
  source pin on pinned images (Linux `ubuntu-24.04`, GCC 13.3), runs the
  package-consumer smoke test, packages the install prefix, attests the
  build provenance and publishes immutable parts to the release
  `toolchain-llvm-18.1.3-c13b7485b879`.
- **Package pins** (`toolchain-packages.lock.json`, mandatory): per platform,
  the SHA-256 and size of the archive and of each part, the source pin, and
  the builder workflow whose attestation must verify.
- **CI** fetches and verifies (hashes, provenance, safe unpack, pin check),
  then runs the smoke test on the unpacked package. No LLVM build, no
  fallback.
- `tools/package_llvm.py` (smoke, pack, record), `tools/fetch_llvm_package.py`,
  `tools/toolchain_packages.py`, `tools/llvm_smoke/` (MLIR CMake consumer:
  parse, verify, lower to the LLVM dialect, translate to LLVM IR; clang
  compiles and runs a C program).
- `tools/test_toolchain_packages.py` (corpus CI job): multi-part pack,
  record, fetch, a corrupted part, a package for another commit, an
  unpinned platform, a non-empty destination and an archive member
  escaping the destination.
- CI triggers: push to `main` and pull requests (no duplicate runs). The
  one-time import workflow is removed.

## Verification so far

- On the pinned commit, built locally with GCC 13.3: the 1 240 build steps
  of LLVMSupport, LLVMCore, MLIRSupport and clangBasic, without errors. This
  shows those components compile; it does not show the install prefix is
  complete — the builder's smoke test and the CI consumer test establish
  that.
- `tools/test_toolchain_packages.py`: 13/13. `tools/test_check_toolchain.py`:
  12/12.
