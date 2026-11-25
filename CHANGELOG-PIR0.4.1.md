# Pegasus — checkpoint PIR0.4.1: hardening (review of PIR0.4)

Based on PIR0.4 (SHA-256 `e65d07a38a753807e8606604c230f01d0240519871218af4c01d5fc850e07842`),
which is unchanged. No architectural change; the PIRB format and C ABI keep
their versions (1.0): the two reader rules below were already in the
specification and are now enforced.

## Finding 1 — boolean ordering accepted by the PIR verifier (high)

`verify.rs` listed `bool` among the ordered types; spec/semantics.md §6
defines `==`/`!=` on `bool` but no ordering. Fixed: ordering is defined on
integers, floats, `char` and `str` only. References are now also excluded
from `cmp` (they are compared through loads). The C++ emitter no longer has
a `bool` ordering path: it reports "PIR inconsistent with its verification"
(fail closed) instead of emitting `icmp ult i1`.

Tests: PIR verifier tests for `lt`/`le`/`gt`/`ge` on `bool` (rejected) and
`eq`/`ne` (accepted); reference comparison rejected; a semantic reject case
for all four source operators (T003); a mutation kind turning boolean
`eq`/`ne` into each ordering (20 mutants, all rejected — this kind would
have caught the finding); and the ABI test rewrites a real module's boolean
`cmp.eq` into each ordering with the checksum recomputed. The PIR0.4
backend accepts that rewritten file and emits `icmp ult i1` (reproduced);
PIR0.4.1 refuses it.

## Finding 2 — reserved PIRB flags accepted (medium)

Both readers now require `flags == 1` exactly: a missing verified bit and
any reserved bit are rejected, with distinct messages. Tests: every bit
1–63, alone and together with bit 0, plus 0 and all-ones, in the Rust
reader test and in the C ABI test (126 cases), each file resealed.

## Finding 3 — LLVM commit pin not enforced (medium)

- `tools/check_toolchain.py` now requires the clang to report the pinned
  version **and** commit; a different commit always fails (the review's
  fake clang). `--require-stamp` additionally checks the provisioning stamp
  against the commit and the SHA-256 of the clang binary. A distribution
  clang reporting no commit is refused unless explicitly allowed
  (`-DPEGASUS_ALLOW_UNPINNED_LLVM=ON` / `--allow-unpinned`), and then the
  result says the commit is NOT enforced.
- The check runs at CMake configure time (fatal on failure) and as ctest
  `toolchain_pinned`; `tools/test_check_toolchain.py` (ctest
  `toolchain_check_selftest`, and the corpus CI job) tests it with 12 fake
  clang scenarios.
- `tools/provision_llvm.py` provisions the pin: fetches the commit by full
  id, verifies `HEAD` and that tag `llvmorg-18.1.3` peels to it, builds
  clang + MLIR with the commit embedded, verifies, stamps. Its
  fetch-and-verify step was run against the real llvm-project repository,
  and refuses a lock whose tag does not peel to the commit (tested with
  `llvmorg-18.1.2`). `toolchain.lock` gains `source` and `projects`.
- Both CI jobs (Linux and Windows, with the MSVC environment) restore or
  provision the pinned LLVM, enforce the pin with `--require-stamp`, and
  configure with `PEGASUS_LLVM_ROOT`.

**Not yet run:** the CI provisioning itself — building LLVM needs CI-class
machines (DEFERRED TOOLCHAIN-1); the first CI run will take hours until
its cache is populated. Local verification here used the Ubuntu clang
18.1.3 with `PEGASUS_ALLOW_UNPINNED_LLVM=ON`, so for this checkpoint's own
numbers the version was checked and the commit was not (TOOLCHAIN-2).

## Windows preparation (untested, NATIVE-5)

The runtime switches stdout/stderr to binary mode on Windows (byte-exact
output like the interpreter's); the differential tool names executables
`.exe` on Windows.

## Verification

| Check | Result |
|---|---|
| `cargo fmt --check`, `cargo clippy --all-targets --locked -D warnings` | clean |
| `cargo test --workspace --locked` | 107 passed, 0 failed |
| Verifier mutation test | 6 432 mutants (395 reference-discipline, 20 boolean-ordering), all rejected |
| CMake + ctest (Release, `PEGASUS_ALLOW_UNPINNED_LLVM=ON`) | 16/16 passed |
| Strict configure with the Ubuntu clang | refused, as designed ("no llvm-project commit") |
| ABI test under ASan + UBSan, 9 native fixtures | 0 failures |
| Native differential | corpus 9/9 + 11 (4 unsupported); 1 000 integer ops + 200 generated programs (seed 41): all match |
| `pir_fuzz` (24 000 programs) | identical to PIR0.4: 0 verifier failures, 0 panics |
| Windows CRLF emulation | checkout intact; 107 passed; toolchain self-test passes |
