# Pegasus Phase 2 — Parser P1.3

P1.3 is a robustness correction release based on the pristine P1.2 archive. P1.2, P1.1 and the R6.2 Phase 1 archive are retained unchanged.

## Fixed

1. **D1 — Recovery must always make progress.** Added an explicit recovery-progress invariant to module, block, item-body, brace-list and parenthesized-list recovery. A closer belonging to an ancestor is propagated rather than retried by the inner loop. A closer that leaves recovery at the same cursor but is not a valid boundary is propagated as an error. Unexpected non-delimiter stops are consumed as a last-resort progress guarantee. This prevents repeated `Error` node insertion and unbounded memory growth.
2. **D2 — Formatting verification remains open.** The P1.2 `cargo fmt --check` failure cannot be certified fixed here because neither `rustfmt` nor the Rust toolchain is installed, and the package host is unreachable. New source changes were kept within the repository's visible Rust style, but this is not a substitute for `cargo fmt --check`. The independent reviewer must run it, apply `cargo fmt` if needed, inspect the resulting diff, and rerun tests.
3. **D3 — Test expectations.** Updated the generic-argument regression to accept the grammar's permitted trailing comma while continuing to reject empty lists and empty slots. Relaxed the trait-item recovery assertion to require survival of the valid sibling method without prescribing incidental recovery-tree details.
4. **D4 — Dormant newline after skipped tokens.** `skip_one` wakes a dormant newline after a skipped `>`, `>>` or `*`, because recovery has no operator/generic/glob interpretation to preserve. Added direct cursor tests for all three markers and an integration regression for a skipped generic closer preserving the following field.
5. **D5 — Public diagnostic API notice.** `Diagnostic.related: Option<(u32, u32)>` was introduced in P1.2 to identify a secondary source span, including the opener associated with P010. It is retained in P1.3. Downstream code constructing `Diagnostic` with a struct literal must now provide `related: None` when no secondary span applies. This is an additive field in the public struct, but may require source updates for struct-literal construction.
6. **D6 — Per-input parser watchdog.** Token-soup fuzz inputs now each run behind an individual timeout across eight deterministic seeds. On timeout the test process aborts intentionally: Rust cannot cancel a stuck parser thread safely, and detaching a runaway parser could continue unbounded memory growth while later tests run. Added the four minimal hang reproducers and additional malformed inputs across function, module, impl, struct, trait, mod and enum contexts.

## Deliberately deferred

- **D7 — P010 nested-opener wording and EOF cascade.** The existing related-span selection remains: use the lexer-paired same-kind opener when one exists, otherwise the innermost active opener. The exact diagnostic wording for nested mismatches and the small EOF cascade need a separate specification-level review; P1.3 does not silently redefine them.
- Parser AST golden oracle and production-by-production parse-path audit remain follow-on parser-conformance work.
- Name resolution, type checking, PURM, semantic PIR, runtime and code generation are out of scope.

## Preserved baseline

- The P1.2 archive is not modified.
- The 163 original Phase 1 source cases and 326 original token/filter candidate golden files remain byte-identical to R6.2.
- Grammar Markdown and machine-readable EBNF are unchanged.
- No golden group is frozen by this release.

## Verification status

The authoring environment has no Cargo, rustc or rustfmt executable. Python repository checks can be reproduced locally, but Rust tests, `cargo fmt --check`, Clippy, CTest, Rust 1.97.1 and Windows CI must be run by the independent reviewer. This archive is a candidate for review, not a claim that the Rust/Windows gates have passed.
