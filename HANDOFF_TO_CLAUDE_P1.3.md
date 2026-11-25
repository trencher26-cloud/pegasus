# Independent engineering handoff — Pegasus Parser P1.3

## Your role

Act as an independent senior compiler engineer. Do not trust the archive's changelog, previous review, or author's test claims without reproducing them. Inspect the actual source, reproduce edge cases, and report evidence. Do not begin semantic compiler stages in this task.

## Baseline and immutability

- Review the supplied `pegasus-phase2-parser-p1.3.zip`.
- Treat P1.2, P1.1 and R6.2 as immutable historical baselines.
- Do not rewrite or overwrite those archives.
- P1.3 is a candidate, not a declared verified release.

## Required audit

### D1 — No-progress recovery and hang regression

Inspect the recovery-progress invariant in `compiler/parser/src/parser.rs`. Reproduce these exact malformed inputs under a watchdog:

```peg
fn t() = ({)
fn good() = 1
```

```peg
fn t() {
( { -> )
}
fn g() = 1
```

```peg
fn t() {
[ { ] x struct fn impl
}
fn g() = 1
```

```peg
fn t() {
( if { ) ]
}
fn g() = 1
```

Also fuzz all eight syntactic contexts represented in `malformed_recovery_inputs_have_individual_watchdogs`. Check that malformed inputs terminate, memory remains bounded, valid sibling constructs are not swallowed, and diagnostics do not cascade without bound. Inspect `module`, `item_body`, `block`, `brace_list`, and `paren_list_r` for any remaining loop that can retry an unchanged token.

### D2 — Formatting and toolchain

Run the exact pinned toolchain (Rust 1.97.1) and these commands:

```powershell
cargo fmt --check
cargo test --workspace --locked
cargo clippy --all-targets --locked -- -D warnings
cmake -S . -B build -G Ninja
cmake --build build
ctest --test-dir build --output-on-failure
```

Also run the Python checks listed below. If the sandbox cannot download/use 1.97.1, state the exact fallback version and leave the pinned-toolchain gate open.

### D3 — Grammar-correct tests

Confirm `list(generic_arg, ",")` permits one trailing comma:

```peg
type A = Vec<T,>
fn t() = parse<T,>(x)
fn t() = parse<Vec<T,>,>(x)
```

Confirm empty lists and empty slots remain invalid: `<>`, `<,T>`, `<T,,U>`. Check that the trait recovery test asserts survival of the valid sibling method without depending on incidental Error-node layout.

### D4 — Dormant newline after skipped tokens

Inspect `skip_one`. Confirm it wakes a dormant `NEWLINE` after recovery skips `>`, `>>` and `*`, while ordinary successfully parsed operators retain rule 2a semantics. Run the direct cursor unit test and integration test preserving the field after a skipped generic closer.

### D5 — Public API notice

Confirm the P1.2 addition `Diagnostic.related: Option<(u32, u32)>` is documented in `CHANGELOG-P1.3.md`. Check all `Diagnostic` construction sites initialize it. Do not remove the field just to make lexer source hashes match R6.2; the original corpus and token/filter golden bytes must remain unchanged.

### D6 — Fuzz watchdog

Verify the token-soup fuzz test has a per-input timeout, uses deterministic seeds and cannot leave an unbounded runaway parser thread behind. The implementation intentionally aborts the test process on timeout because Rust cannot safely cancel an arbitrary stuck thread. Judge whether this is appropriate for CI; suggest a subprocess-based alternative only if it can preserve a hard per-input deadline portably.

## Python checks

Run:

```sh
python3 tools/corpus.py check
python3 tools/extract_ebnf.py --check spec/grammar.md
python3 tools/goldens.py check
python3 tools/test_goldens.py
python3 tools/corpus.py coverage
python3 tools/check_checkout.py
```

Verify that the 163 original R6.2 Phase 1 source cases and 326 original token/filter candidate golden files are unchanged. No group should be frozen as part of this audit.

## D7 — explicitly deferred

P010 nested-mismatch wording and the small EOF cascade were not part of P1.3's D1–D6 scope. Report whether either issue blocks parser closure, but do not silently change the grammar or diagnostics catalogue. The AST golden oracle and production-by-production parser coverage audit are also still pending.

## Return format

Return:

1. Exact commands, toolchain version and exit codes.
2. A table of all D1–D6 checks with reproduced evidence.
3. Any new minimal reproducer, with source line and function responsible.
4. Whether P1.3 is safe to close, or requires P1.4.
5. Remaining work in the parser stage, prioritized and separated from future semantic work.

Do not begin name resolution, type checking, PURM, semantic PIR, runtime or code generation. The objective is to establish a trustworthy parser and AST before advancing the compiler.
