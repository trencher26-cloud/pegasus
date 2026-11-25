# Pegasus Phase 2 — Parser P1.4

P1.4 is a correction release based on the pristine P1.3 archive
(SHA-256 `4b3f079f509fff2e139d0e4d46bda41e5685fc426b1f107d784fae7a019fc2da`).
P1.3, P1.2, P1.1 and the R6.2 Phase 1 archive are retained unchanged.

## Fixed

1. **N1 — a stray closer inside a block destroyed the whole function (P1.3 regression).**
   `fn f() {\n    a = ]\n}\n` produced two P001 errors and an `Error` item in
   place of `fn f`; P1.2 reported one error and kept the function.
   Cause: `is_boundary_close` required the bracket-stack top to equal the
   block's opener for an unpaired `}`. An earlier stray `]` or `)` makes the
   stack pop that opener, so the block's own `}` was no longer recognised and
   the error propagated to the item. For an unpaired closer the stack top is
   now accepted when it is at or below the boundary, or the stack is empty.
   One expression changed in `compiler/parser/src/parser.rs`.
2. **D2 — formatting.** `cargo fmt` applied (16 hunks, formatting only). With
   `cargo fmt --check` exiting 0 on Rust 1.97.0.
3. **N3 — sibling survival is now tested.** New tests:
   `stray_closer_in_a_block_keeps_the_function`,
   `malformed_inputs_preserve_the_valid_sibling`.
   Reverting the N1 fix makes two tests fail.

## Documented, not changed

- **N2 — an unclosed `{` consumes later items.** `fn t() = ({)\nfn good() = 1\n`
  reports P010 and does not produce `fn good`; the `{` is never closed. Whether
  a line-start item starter should end an unclosed brace is a specification
  question. The grammar is not changed. The new test
  `unclosed_brace_consumes_the_rest_of_the_file_by_design` records the current
  behaviour and states that it does not prescribe it. If the spec decides
  otherwise, that test is the one to change.
- **D7** (P010 nested-opener wording, small EOF cascade): still deferred.

## Unchanged

- Grammar Markdown and EBNF; corpus (180 cases); candidate goldens; the lexer.
- No golden group is frozen. Nothing was reviewed or frozen by this release.

## Verification (Rust 1.97.0 fallback; the pinned 1.97.1 could not be downloaded)

`cargo fmt --check` 0; `cargo test --workspace --locked` 0 (parser 48/48);
`cargo clippy --all-targets --locked -- -D warnings` 0; `cmake` configure,
build and `ctest` (8/8) 0; the six Python checks 0; fuzz matrix recorded in
the review. **Not run:** Rust 1.97.1, Windows CI. Gates 1 and 2 remain open.
