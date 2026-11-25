# Pegasus Phase 3 — Semantic frontend, checkpoint S0.1

Based on parser P1.4 (SHA-256 `d73cb48a3ba8646cbe9557544373b4c423bed47207fc89108c41bc4878402a56`),
which is unchanged: lexer, parser, grammar, corpus and goldens are byte-identical.

## Added

- `compiler/sema` (`pegasus-sema`): the semantic frontend.
  - Package loading (`mod x` → `x.peg` / `x/mod.peg`), the built-in `core`
    prelude (`Option`, `Result`, their variants).
  - Definition table, two namespaces, `use` resolution to a fixed point
    (paths, aliases, groups, globs, enum containers, `pub use`), visibility
    for items and fields.
  - Generics with bounds, where clauses and supertraits; type lowering;
    aliases; a constant evaluator for array lengths and const arguments.
  - Struct/enum layouts, infinite-size detection, impl tables, trait
    conformance, coherence (overlap) checks.
  - Body type checking with local inference: literals, all operators with
    the Phase 0 numeric rules, lossless `as`, calls with named arguments and
    `inout`/`own` markers, methods (inherent, trait, bounds), associated
    functions, struct literals, closures with captures, `if`/`if let`,
    `match`, `while`/`while let`, `loop` with break values, `for` over
    ranges and arrays, `return`, `?` on Option/Result, `unsafe` capabilities,
    extern calls, `defer`, `region`.
  - Patterns, exhaustiveness and irrefutability (usefulness algorithm).
  - Typed HIR output for PIR lowering.
  - 88 catalogued diagnostics (R/T/U); every unimplemented feature is a `U`
    diagnostic, never accepted silently.
- `compiler/driver`: the `pegasus` CLI (`pegasus check`, `pegasus hir`).
- `spec/semantics.md`: the semantic specification and diagnostic catalogue.
- `tests/sema`: annotation-driven corpus (`// error: CODE`), 3 accept,
  5 reject and 2 multi-file packages; tests that every emitted code is catalogued.
- `DEFERRED.md`: ledger of deferred issues (S1, S2, S3, N2, D7, parser
  conformance work, sema interpretations).

## Verification (Rust 1.97.0 fallback; pinned 1.97.1 not downloadable here)

`cargo fmt --check` 0; `cargo clippy --all-targets --locked -- -D warnings` 0;
`cargo test --workspace --locked` 0; CMake + `ctest` 8/8; Python checks 0.
Robustness: 24,188 programs (parser corpus, grammar-generated random
programs, line-mutated corpus files) through sema with 0 panics.
Not run: Rust 1.97.1, Windows CI.
