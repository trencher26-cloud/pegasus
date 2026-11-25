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

## First packages (toolchain run 45633865068, PR #1)

| Platform | Archive SHA-256 | Size | Built on |
|---|---|---|---|
| linux-x86_64 | `c1cb839296b22f515b677c95f0616b0fb45397c278505151c97ef2ccc9346f88` | 751 233 592 | ubuntu24 20261116.376.1, GCC 13.3.0, glibc 2.39 |
| windows-x86_64 | `4d0d4e7577eef48a4de94aafd84c876abe67641689a7822d3e9ba1f81f666536` | 613 881 816 | win25-vs2026 20261116.315.1, MSVC 19.52.37029 |

Both are one part each, built from `llvmorg-18.1.3` = `c13b7485b879…`
(source fetched and verified by the CI source builds of run 45597782356:
Linux 1 h 48 min, Windows 2 h 52 min, reused through the cache). The
builder's smoke test passed on both install prefixes (on Windows, the first
MLIR and clang run of the project). Each part has one SLSA v1 provenance
attestation from `.github/workflows/toolchain.yml` (ref `refs/pull/1/merge`).

Checked independently before recording: the release digests equal the
manifests; the Linux package, fetched with `tools/fetch_llvm_package.py`,
matches the lock's hashes, unpacks safely (3.0 GB), passes
`check_toolchain.py --require-stamp` and the full smoke test. Attestation
*verification* could not run in that environment (Sigstore's trust root is
not reachable there; the fetch failed closed); CI verifies it.

## Windows CI fix found on the way

The first Windows CI run on the pinned toolchain built LLVM, saved the
cache and enforced the pin, then failed linking Rust: in Git Bash,
`/usr/bin/link` (coreutils) precedes MSVC's `link.exe`. Windows steps now
default to PowerShell. The builder checks Windows out with `autocrlf=true`
like CI, because the cache key hashes checked-out files (Windows key
`5f00a1a6…`, Linux `3b1185fd…`).
