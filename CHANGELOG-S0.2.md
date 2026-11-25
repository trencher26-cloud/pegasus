# Pegasus Phase 3 — Semantic frontend, checkpoint S0.2

Correction release based on the pristine S0.1 archive
(SHA-256 `8b946eccf4062ab2505babb7d0d9c6019fa8c7f7f3f8cd97a1ed5705bc46f082`), which is
unchanged. Parser P1.4, lexer, grammar, grammar corpus and goldens remain
byte-identical.

## Fixed (review of S0.1)

1. **Exhaustiveness could accept a non-exhaustive match.** Reaching the old
   depth limit (64) returned "nothing missing". The analysis now has four
   outcomes — exhaustive, missing (with witness), inconclusive, skipped
   (patterns with an already-reported type error) — and an inconclusive
   result is the new error T083. Limits are depth 1024 and one million
   steps; a row of wildcards short-circuits to exhaustive. The semantic
   phases now run on a dedicated 64 MiB-stack thread (`SEMA_STACK`), so the
   depth limit is safe in every caller (found by the new regression test,
   which overflowed a 2 MiB test thread).
2. **Large arrays caused enormous allocations.** Array patterns are no longer
   expanded per element: a column becomes at most (longest prefix + longest
   suffix) columns. `[u8; 2**60]` matches are analysed in microseconds.
   Array lengths are converted with checked conversions (`CInt::to_usize`);
   lengths outside `usize` or negative are T017.
3. **Trait conformance checked only presence.** Now also: associated types
   against the trait's bounds (T037), associated constant types (T036),
   `-> in T` borrow returns, and impl methods that require stricter generic
   bounds than the trait (T035). `implements` moved to `Sema` so impl
   checking can use it.
4. **Constant evaluation was 128-bit signed and untyped.** New sign-magnitude
   `CInt` covers `i128::MIN..=u128::MAX`; evaluation is typed and
   width-aware (overflow per type, shifts up to width − 1, `<<` discards
   bits, arithmetic `>>`, `MIN % -1 = 0`, unsigned shift amounts and
   exponents), and `-literal` is a single negative value. Every constant item
   is evaluated when possible, so overflow is reported even if unused (T017).
   Const generic arguments are evaluated at the parameter's declared type.
5. **Expression bodies differed from block bodies in HIR.** `fn f() = e` is
   now checked as the block `{ e }`, producing identical typed HIR.
6. **README was stale.** Rewritten for P1.4 and the semantic frontend.

## Tests

- Corpus: `accept/consts.peg`, `reject/consts.peg`, trait-member contract
  cases in `reject/traits.peg` (12 corpus packages in all).
- `compiler/sema/tests/regressions.rs`: deep missing pattern (beyond 64),
  inconclusive → T083 and catch-all → accepted, huge arrays under 2 s,
  array prefix/suffix exhaustiveness, expression-body HIR equivalence.
- Unit tests for `CInt` bit patterns.
- Robustness: 24,188 programs through sema, 0 panics.

## Verification (Rust 1.97.0 fallback; pinned 1.97.1 not downloadable here)

`cargo fmt --check`, `cargo clippy --all-targets --locked -- -D warnings`,
`cargo test --workspace --locked`, CMake + `ctest`, Python checks — see the
review message for exit codes. Not run: Rust 1.97.1, Windows CI.
