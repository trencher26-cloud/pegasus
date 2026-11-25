# Claude handoff — Pegasus Parser P1.2

You are reviewing the next movement of the Pegasus compiler. Start from this exact archive state. Do not assume that the implementation is correct merely because this handoff describes the intended fixes; inspect the source, reproduce the tests, and report what you actually verified.

## Current project state

Pegasus is a new language and compiler project. The repository currently contains:

- Phase 1: Rust lexer, newline filter and bracket-match table.
- Phase 2 in progress: Rust recursive-descent parser and syntax-only AST.
- A separate machine-readable EBNF and conformance corpus.
- CMake/Ninja orchestration for Cargo, Python corpus checks and CTest.

This is not yet a semantic compiler. Name resolution, type checking, PURM ownership/borrow analysis, semantic PIR, runtime and native code generation are not implemented in this repository. Do not imply otherwise.

## P1.2 changes to verify

P1.2 is a targeted correction over P1.1. The intended changes are:

1. **Generic argument trailing commas:** `generic_args()` accepts one trailing comma as required by `list(generic_arg, ",")`; Rule B lookahead also accepts trailing commas, including nested generic lists and `>>` closers. Empty lists and empty slots remain invalid.
2. **Delimiter-aware recovery:** parser-consumed open delimiters are tracked by token identity. Statement, expression-list, brace-list and item recovery must not mistake a nested `}` for the active function/trait/impl/mod body's closer. Curly depth for item resynchronization is tracked separately to keep the check O(1).
3. **P018 propagation:** a nesting-limit violation must bypass local expression/list recovery and bubble to the enclosing item recovery point. The parser should then resume at the next valid sibling item rather than continue constructing a partial item.
4. **P010 secondary source span:** `Diagnostic.related` optionally identifies the opener. The primary span remains the unexpected closer; the related span points to the matching opener selected by the bracket table, or the innermost active opener when the closer has no same-kind partner.
5. **Regression tests:** new tests cover type and expression generic trailing commas, empty generic slots, nested const-generic brace recovery in a function statement and trait signature, P018 item recovery (including a block body), and P010 related spans.

The grammar and `spec/pegasus.ebnf` have not changed. Trait method `async`/`comptime`/`kernel` prefixes remain accepted because the current grammar routes trait methods through `fn_sig`, which includes `fn_prefix`.

## First actions — reproduce before changing code

The repository pins Rust 1.97.1 in `rust-toolchain.toml`. Use that toolchain and Python 3.11+.

```powershell
rustup show active-toolchain
cargo fmt --check
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
python tools/corpus.py check
python tools/extract_ebnf.py --check spec/grammar.md
python tools/goldens.py check
python tools/test_goldens.py
python tools/corpus.py coverage
cmake -S . -B build -G Ninja
cmake --build build
ctest --test-dir build --output-on-failure
```

On Linux, use the same commands (with `python3` if necessary). CMake/CTest has eight normal suites in this configuration. Do not count a check as passed unless you actually ran it and retained its output.

Also run the GitHub Actions workflow on both Linux and Windows. The Windows job deliberately uses `core.autocrlf=true`; verify that the Phase 1 source and golden hashes remain stable.

## Specific adversarial tests to add or strengthen

Do not stop at the included happy-path regressions. Independently test:

- `Foo<T,>`, `Foo<Bar<T,>,>`, `f<T,>(x)`, `f<Vec<T,>,>(x)`, and combined `>>` closers.
- Invalid `Foo<>`, `Foo<,T>`, `Foo<T,,U>` and malformed const generic expressions.
- Nested `{...}` const arguments inside a failed statement, call argument, tuple/array element, struct/enum/trait/impl/mod item, and nested blocks.
- Mismatched `)`, `]`, `}` where the closer either has no same-kind opener or matches an outer opener beneath a nested opener. Check both primary and related spans.
- P018 reached through expressions, types, patterns, prefix chains, `**` chains and interpolation holes. Confirm local recovery does not swallow it and that the following sibling item survives.
- No-cascade suppression followed by a valid sibling statement/item.
- Recovery and scanner complexity on deeply nested and adversarial input. Ensure delimiter bookkeeping and Rule B remain linear/bounded as specified.

If any test exposes a mismatch between the grammar and implementation, state which side is normative and propose the smallest correction. Do not silently change the grammar to make a test pass.

## Baseline and review gates

- The seven original Phase 1 golden groups remain unreviewed until an independent reviewer checks and freezes them.
- The Phase 2 `parser` golden group also remains unreviewed. Candidate lexer-stage `.tokens`/`.filtered` snapshots are not independent AST goldens; do not claim that parser tree conformance has been established by them.
- The Phase 1 gate must remain independent of the parser group: `--require-all-frozen` should require exactly the seven Phase 1 groups. The Phase 2 parser group has its own `--require-phase2-frozen` gate.
- Do not run `goldens.py freeze` on behalf of the user. A reviewer must inspect the outputs against the grammar first.
- Keep P1.1 and R6.2 immutable. Make any further corrections in a new revision/archive with a new SHA-256.

## Ask: determine the next movement

After verification, give a concise engineering review with:

1. Exact commands run and their actual results.
2. Defects found, severity, minimal reproducer, and source locations.
3. Whether P1.2 is ready to close Phase 2 parser implementation or needs a P1.3 correction.
4. A prioritized next-milestone plan based on the actual code, not an aspirational feature list. Focus next on parser/AST completeness, independent AST conformance oracles, diagnostics/recovery behavior and grammar-production coverage before starting semantic analysis.
5. Explicit statements of what remains unimplemented. Do not start PURM, type checking or PIR implementation until parser acceptance criteria are genuinely satisfied and the user approves the next stage.

Do not rewrite working components or broaden scope without evidence. Preserve the existing architecture and revision history. Prefer small, independently testable changes with regression cases.
