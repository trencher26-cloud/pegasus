# Pegasus — checkpoint PIR0.3: borrow semantics and PIR ownership completion

Based on PIR0.2 (SHA-256 `bfb72ac83e3ee1b15e2725c5edf661dcc333b56ac811141c044ae66050130cf5`),
which is unchanged. P1.4, S0.2 and PIR0.1 remain immutable.

## Fixed (review of PIR0.2)

**A. `inout` was not preserved through PIR calls.** `bump(&c)` changed the
callee's copy; `c.value` stayed 1. PIR now has second-class references:
`in` parameters are `&T`, `inout` parameters `&var T`; call sites borrow the
caller's place (or a temporary); writes through an `inout` parameter use
`store` through the reference. The program now returns 2 (tests/run/borrows.peg).

**B. Non-Copy borrowed returns were rejected.** The body of a `-> in`
function was analysed as a move. Borrowed returns are now borrows: the
result is a reference into the caller's storage. Provenance is tracked: a
call's borrowed result is derived only if every argument it borrows is
derived (tied to all borrowed inputs, Phase 0 rule 3). O005 points at the
offending sub-expression.

**Specification.** `spec/purm.md` now describes the implemented partial
moves, borrow semantics, provenance and function values.

## Added

- PIR: `Type::Ref`, instructions `borrow`/`borrow_mut` (slot places),
  reborrow through a reference, `load`/`store` through a reference;
  by-value wrappers for functions used as values; reference-mode lowering of
  borrowed returns through places, projections, blocks, `if`, `match` and
  forwarded borrowed calls.
- Verifier: references only at the top level; `&var` required for stores and
  exclusive reborrows; no borrow of a reference slot; provenance — a
  returned reference must not point into the function's own storage.
- Interpreter: references are frame-stamped pointers; writes reach the
  caller's slot; a reference into a returned frame traps as dangling.
- Sema: T065 — functions with `inout`/`own` parameters or a borrowed return
  cannot be function values (function types carry no modes).
- Tests: `run/borrows.peg` (12 checked results: inout through calls,
  reborrows, fields, array elements, `inout self`; borrowed returns: direct,
  projection, nested projection, forwarding, branches, indexing, passed on);
  `reject/borrows.peg`, `reject/fn_values.peg`; verifier unit tests for
  references and provenance; interpreter dangling-reference test; new
  mutation kinds (borrow mutability, references).

## Verification (run for this checkpoint, Rust 1.97.0 `stable`)

| Check | Result |
|---|---|
| `cargo fmt --check`, `cargo clippy --all-targets --locked -D warnings` | clean |
| `cargo test --workspace --locked` | 94 passed, 0 failed |
| Verifier mutation test | 4587 mutants (PIR0.2: 3715), all rejected |
| CMake + ctest (Release) | 9/9 passed |
| Integer oracle | 300 ops in ctest; 3000 ops seed 11: 0 mismatches |
| `sema_fuzz` / `pir_fuzz` (mutated corpus, seeds 1–5, 26 000 programs) | 0 panics; 1 219 accepted, 1 192 run (others hit limits), 0 verifier failures or soundness traps |
| Windows CRLF emulation (honoring `.gitattributes`) | checkout check intact; 94 passed |

Gate 1 (real Linux/Windows CI with the pinned Rust 1.97.1) remains a user
action: that toolchain cannot be downloaded in this environment.
