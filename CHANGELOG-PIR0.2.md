# Pegasus — checkpoint PIR0.2: complete PIR verifier, PURM v0.1

Based on PIR0.1 (SHA-256 `326c92ace9382e02ff1f076570c121a9e98f0f14ae4e6ba34bec373a0bcb4316`),
which is unchanged. S0.2 and P1.4 remain immutable; the parser, lexer,
grammar and grammar corpus are byte-identical.

## 1. PIR verifier (review of PIR0.1)

- Every instruction kind is type-checked, including the previously
  unchecked `Const`, `Un`, `Array`, `Repeat`, `ExtractIndex`,
  `VariantField` (and `Tag`, `Cast`, `move`, `drop`).
- Every declared value must be defined exactly once, used or not.
- Struct, enum, function, slot, block and value identifiers are
  range-checked everywhere: malformed PIR produces errors, never panics.
- Legal operation/overflow-mode combinations: `wrap`/`sat` only for integer
  `add`/`sub`/`mul`; no mode on floats and `bool`; unsigned shift amounts and
  integer exponents; integer constants within their width; distinct switch
  cases within the value's type; lossless casts only.
- The interpreter refuses illegal operation/mode combinations instead of
  computing them.
- Tests: one negative unit test per rule (`tests/verify.rs`, 7 tests) and
  a mutation test (`tests/verify_mutation.rs`): every corpus program is
  compiled and 3,000+ single invalid mutations (wrong result/parameter
  types, wrong constants, illegal modes, out-of-range ids, deleted or
  duplicated definitions, missing blocks, undeclared values) must all be
  rejected without a panic. Weakening one verifier rule lets hundreds of
  mutants escape, so the test guards the verifier itself.

## 2. PURM v0.1 (`spec/purm.md`)

- Formal rules: ownership, `Copy` (core marker trait; built in for
  primitives, tuples, arrays; `impl Copy` for structs and enums, validated),
  moves and use-after-move (flow-sensitive through branches, loops with
  loop-carried moves, early exits, closures, `defer`), partial moves of
  field paths, second-class `in`/`inout` borrows, the law of exclusivity at
  calls, borrowed returns tied to parameters, deterministic destruction.
- Checker on typed HIR (`compiler/sema/src/purm.rs`), diagnostics O001–O013;
  consuming uses become `MoveLocal`/`MovePath` in HIR.
- Corpus: `reject/ownership.peg` (every O code), `accept/ownership.peg`.
  `accept/generics.peg` and `run/traits.peg` now take generic values `own`
  (an unbounded `T` is not Copy); `run/data.peg` declares `impl Copy for Point`.

## 3. Ownership in PIR and the interpreter

- New PIR instructions `move $slot.path` and `drop $slot`.
- Lowering: scopes for blocks, match arms, `if let` branches, loop
  iterations and the function root (`own` parameters); drops in reverse
  declaration order after `defer`s at every exit; old values dropped on
  reassignment; non-Copy captures move into closures.
- Interpreter: `move` leaves the slot uninitialized; reading it traps as a
  PURM violation (second line of defence); `drop` is a no-op on moved slots
  and is recorded in `Interp::drops` (partial drops marked).
- `tests/run/drops.peg` checks the exact destruction trace.

## Verification (Rust 1.97.0; pinned 1.97.1 not downloadable here)

See the checkpoint report.
