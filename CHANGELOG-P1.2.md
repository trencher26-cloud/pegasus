# Pegasus Phase 2 — Parser P1.2

P1.2 is a focused correction release based on `pegasus-phase2-parser-p1.1.zip`.

## Fixed

1. **Generic argument trailing commas.** The parser and Rule B lookahead now follow `list(generic_arg, ",")`: one trailing comma is accepted, while empty lists and empty slots remain invalid. Nested generic lists are handled, including combined `>>` closers.
2. **Delimiter-aware recovery.** Parser consumption and skipped tokens maintain a stack of consumed delimiter identities. Statement, list and item recovery distinguish a nested closer from the closer of the active recovery boundary. Item resynchronization uses an O(1) curly-brace-depth check rather than rescanning all active delimiters.
3. **P018 propagation.** Nesting overflow is fatal to local recovery handlers and bubbles to the enclosing item recovery point. The parser resumes at the next item boundary instead of inserting a local expression placeholder and continuing the invalid construct.
4. **P010 related location.** `Diagnostic.related` optionally carries a secondary `(offset, length)` span. For P010, the primary span is the unexpected closer and the related span identifies the opener selected by the lexer bracket-match table, or the innermost active opener when no same-kind match exists.

## Regression tests added

- Trailing commas in type-level, expression-level and nested generic argument lists.
- Rejection of empty generic lists and empty argument slots.
- Recovery from a malformed brace-delimited const generic inside a function statement, preserving the following statement and item.
- Recovery from the same nested-brace failure in a trait method signature, preserving the next trait item.
- P018 recovery at the item boundary, preserving a following function.
- P010 opener and closer source spans.

## Preserved baseline

- P1.1 source archive was not modified.
- The Phase 1 lexer implementation's tokenization behavior is unchanged.
- All 163 original Phase 1 source cases and 326 token/filter candidate goldens remain byte-identical to R6.2.
- The grammar and machine-readable EBNF were not changed.

## Verification status

Python-side repository checks were run successfully in the authoring environment: corpus format, EBNF synchronization, frozen-golden integrity, golden-review tool tests, and coverage reporting. The 59 below-target coverage entries remain corpus quantity gaps, not parser failures.

Rust compilation, parser tests, `cargo fmt --check`, Clippy, CTest and Windows CI were not executable in the authoring environment because the Rust toolchain is unavailable. The parser golden review group remains unreviewed; no candidate goldens have been frozen by this release.
