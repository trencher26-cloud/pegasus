# Pegasus

Bootstrap repository for the Pegasus language: specifications, conformance
corpora, and the compiler stages built so far.

**Status (PIR0.4.1):** the lexer and newline filter, the parser (baseline P1.4),
the semantic frontend (`compiler/sema`: modules, names, visibility, types,
traits, exhaustiveness, typed HIR), PURM v0.1 ownership checking (moves,
partial moves, borrows with provenance, exclusivity, deterministic
destruction), PIR v0.1 with references and a complete verifier, HIR → PIR lowering, and the reference interpreter are implemented:
Pegasus programs run (`pegasus run`) in the interpreter. The native backend
(C++, `backend/`) compiles a subset — integers, `bool`, tuples, structs,
arrays, references, calls and control flow — to executables through the
frozen PIRB bridge and LLVM 18.1.3 (`pegasus build`), with results and traps
identical to the interpreter. MLIR, the rest of the language natively, the
runtime and the standard library are next; there is no I/O yet, so programs
report results through `main`'s return value. Open questions and known gaps are listed in `DEFERRED.md`.

```sh
cargo run -p pegasus-driver -- check tests/sema/accept/basics.peg   # resolve and type-check
cargo run -p pegasus-driver -- hir tests/sema/accept/basics.peg     # also print typed HIR
cargo run -p pegasus-driver -- pir tests/run/traits.peg             # print verified PIR
cargo run -p pegasus-driver -- run tests/run/data.peg               # run main in the interpreter
python3 tools/fetch_llvm_package.py --platform linux-x86_64 --dest ../llvm   # pinned, verified package (GH_TOKEN for provenance)
# or build it from source: python3 tools/provision_llvm.py --prefix ../llvm  (hours)
cmake -S . -B build -DPEGASUS_LLVM_ROOT=../llvm && cmake --build build && (cd build && ctest)
# with a distribution clang 18.1.3 instead: -DPEGASUS_ALLOW_UNPINNED_LLVM=ON (commit not enforced)
PEGASUS_NATIVE=build/backend/pegasus-native PEGASUS_RT=build/backend/libpegasus_rt.a \
  cargo run -p pegasus-driver -- build tests/native/control.peg out && ./out   # native executable
```

| Path | Contents | Status |
| --- | --- | --- |
| `spec/grammar.md` | Grammar specification v0.1.2-draft (snapshot of the specification document) | Draft |
| `spec/pegasus.ebnf` | Machine-readable grammar, 157 productions, extracted by `tools/extract_ebnf.py` | Coverage-checked |
| `spec/semantics.md` | Semantic frontend specification and R/T/U diagnostic catalogue | Implemented by `compiler/sema` |
| `compiler/lexer` | `pegasus-lexer`: lexer (grammar section 2), newline filter (section 3), bracket-match table | Passes the corpus's lexer-stage checks |
| `compiler/parser` | `pegasus-parser`: syntax tree (`ast`), recursive-descent parser with error recovery (sections 4–8) | P1.4; Gates 1–2 open (see below) |
| `compiler/sema` | `pegasus-sema`: package loading, name resolution, type checking, exhaustiveness, typed HIR | S0.2 |
| `compiler/pir` | `pegasus-pir`: PIR v0.1, verifier, HIR → PIR lowering, reference interpreter, PIRB bridge | PIR0.4 |
| `compiler/driver` | `pegasus` command-line driver (`check`, `hir`, `pir`, `run`, `pirb`, `build`) | |
| `backend/` | Native backend (C++17): PIRB reader, LLVM IR emitter, C ABI (`include/pegasus/backend.h`), `pegasus-native`, runtime | v0.1 subset |
| `spec/pir-bridge.md` | PIRB 1.0 format and backend C ABI 1.0 | Frozen |
| `spec/native.md` | Native backend: subset, semantics, runtime, differences | Implemented by `backend/` |
| `toolchain.lock` | Pinned Rust and LLVM/MLIR (llvmorg-18.1.3, `c13b7485b879`); provisioned by `tools/provision_llvm.py`, enforced by `tools/check_toolchain.py` | |
| `tests/native` | Native corpus: programs compared against the interpreter | |
| `compiler/corpus` | `pgcorpus`: runs the grammar corpus against the lexer and parser; writes candidate goldens | |
| `tests/grammar` | Grammar conformance corpus v0.1.6: 180 cases, diagnostics catalogue, coverage matrix | Seed (see its README) |
| `tests/sema` | Semantic corpus: accept/reject cases with `// error: CODE` annotations, multi-file packages | |
| `tests/run` | Execution corpus: programs with expected results or traps | |
| `spec/pir.md` | PIR v0.1 specification | Implemented by `compiler/pir` |
| `spec/purm.md` | PURM v0.1: ownership, moves, borrows, destruction | Implemented by `compiler/sema/src/purm.rs` and PIR `move`/`drop` |
| `tools/` | `corpus.py` (format, offsets, coverage), `extract_ebnf.py`, `goldens.py` (golden review and freeze), `check_checkout.py` (byte-exact checkout), `int_oracle.py`, `native_diff.py` (native vs interpreter), `check_toolchain.py` | |

## Revisions

Each revision is an archive with a SHA-256 fingerprint and a changelog;
earlier archives are never modified. Parser: P1.1 → P1.4 (`CHANGELOG-P1.2.md`
… `CHANGELOG-P1.4.md`). Semantic frontend: S0.1, S0.2 (`CHANGELOG-S0.1.md`,
`CHANGELOG-S0.2.md`). PIR, interpreter, PURM and the native backend:
PIR0.1–PIR0.4.1 (`CHANGELOG-PIR0.1.md` … `CHANGELOG-PIR0.4.1.md`).

The parser milestone still has two gates that need the project owner: Gate 1,
green Linux and Windows CI on the pinned Rust 1.97.1 (local verification used
Rust 1.97.0); Gate 2, independent review and freezing of the golden groups
(`tools/goldens.py freeze`). Parser conformance targets from grammar §9 are
tracked in `DEFERRED.md` (PC-1 to PC-4).

## Build and test

```sh
cmake -S . -B build -G Ninja
cmake --build build
ctest --test-dir build --output-on-failure
```

Requires CMake 3.25+, Ninja, Python 3.11+ and the Rust toolchain pinned in `rust-toolchain.toml` (1.97.1). `ctest` runs the Rust unit tests (including the semantic corpus), the parser tests (rule 2a, nesting limit, truncation and token-soup fuzzing, linear time), the grammar sync check (`tools/extract_ebnf.py --check`), `tools/corpus.py check` and `coverage`, adversarial tests for the golden-review manager, the frozen source/golden integrity check (`tools/goldens.py check`), and `pgcorpus verify` (lexer and parser conformance). Set `PEGASUS_REQUIRE_ALL_GOLDEN_GROUPS_FROZEN=ON` at CMake configure time to add Gate 2 (the seven Phase 1 review groups frozen), and `PEGASUS_REQUIRE_PARSER_GOLDENS_FROZEN=ON` to add the parser-milestone gate (the `parser` group). CI runs the suite on Linux and on Windows, the latter checked out with `core.autocrlf=true`.

Direct commands: `cargo test`, `cargo run -p pegasus-corpus -- verify`, `python3 tools/corpus.py coverage`, and `python3 tools/goldens.py status`. `pgcorpus bless` also invokes the strict Python 3.11+ review-manifest validator; set `PEGASUS_PYTHON` to the interpreter path if it is not discoverable as `python3` or `python`. If a frozen group gains, loses or renames a source case, `check` and `bless` report the inventory drift, while `unfreeze GROUP --reason "..."` remains usable so the group can be deliberately revised and reviewed again.

## Changing the grammar

1. Edit the specification document; export it to Markdown as `spec/grammar.md`.
2. `python3 tools/extract_ebnf.py spec/grammar.md` regenerates `spec/pegasus.ebnf`.
3. `python3 tools/extract_ebnf.py --check spec/grammar.md` and `python3 tools/corpus.py coverage` must pass (CI runs both): map new productions in `tests/grammar/coverage.toml`.
