# Deferred issues ledger

Open items that are known, recorded and intentionally not resolved yet.
Nothing here may be silently dropped; an entry leaves this file only with a
changelog line saying how it was resolved. Grammar changes need an explicit
decision by the project owner.

## Grammar and parser (from the P1.x reviews)

| ID | Issue | Status / owner decision needed |
|---|---|---|
| S1 | `a as i32 < b` is a P001: after `as T`, `<` always opens generic arguments. Section 7 lists no rule for it. Proposed rule J: "`<` after a cast type opens generic arguments; parenthesize the cast", plus a hint diagnostic. | Spec decision pending. Parser behaviour unchanged. |
| S2 | Qualified unit variants/constants (`Opt.None`) cannot be patterns: `pattern_ident` is a bare name. Rule F covers unqualified names only. Sema resolves unqualified variant names against the scrutinee type (spec/semantics.md §2), which covers the common case. | Spec decision pending. |
| S3 | `spec/pegasus.ebnf` header says v0.1.1; the grammar is v0.1.2-draft. | Documentation fix, no grammar change. |
| N2 | An unclosed `{` swallows later items; whether a line-start item starter should end an unclosed brace is unspecified. Test `unclosed_brace_consumes_the_rest_of_the_file_by_design` records current behaviour. | Spec decision pending. |
| D7 | P010 related-opener wording for nested mismatches; small EOF cascade. | Spec-level review pending. |
| S4 | Rule B counts a balanced `{…}` group as one generic-argument atom, so `while i < 5 {` followed by a body longer than 256 tokens is P009 (`syntax`, no hint): the scan reads `5 { body }` as candidate generic arguments and hits the limit. Repro: a `while i < 5 {` loop with 70 statements `a = a +% k`. The spec text ("tokens that can occur in types", 256-token limit) permits this reading. Proposal: a `const_arg` block may only start an argument (after `<` or `,`), so the scan stops at `INT {` — a parser change that needs a spec decision. Workaround: parenthesize the condition (`while (i < 5) {`). Found by the native differential generator (PIR0.4). | Spec decision pending. Parser unchanged. |
| PC-1 | Grammar §9 conformance targets unmet: ≥1,000 positive / ≥500 negative programs (have 128/52); ≥10 ambiguity cases per rule A–I (have 25 total); a second parser generated from the EBNF; round-trip, incremental-reparse and 24 h fuzzing suites. | Open. |
| PC-2 | Independent AST oracle with hand-reviewed expected trees; production-by-production audit (59 coverage entries below target). | Open. |
| PC-3 | Gate 1: green Linux and Windows CI on Rust 1.97.1 (needs a GitHub push). Gate 2: independent review and `goldens.py freeze` of the 8 golden groups. | Owner actions. |
| PC-4 | Incremental parsing for the language server (grammar §8). | Open; needed before tooling milestone. |

## Semantic frontend (sema v0.1) interpretations and gaps

| ID | Issue | Status |
|---|---|---|
| SEM-1 | Inline modules do not inherit their parent's names (Rust rule). The module system spec is unwritten. | [interp], confirm when the module spec is written. |
| SEM-2 | Unqualified variant names in patterns resolve against the scrutinee's enum (rule F + Phase 0 example). Expressions require qualification or `use`. | [interp]. |
| SEM-3 | A block whose value is not wanted (`()` expected) evaluates its last expression for effect; there is no unused-value warning yet (rule I mentions one). | [interp]; warning W-code pending. |
| SEM-4 | `isize`/`usize` are 64-bit for `as` losslessness: all v1 targets are 64-bit. | [interp]; revisit for 32-bit targets. |
| SEM-5 | Exhaustiveness treats integers/chars as infinite: full coverage by ranges (e.g. `0..=255` on u8) still needs `_`. Or-patterns nested inside constructors are approximated conservatively (may report a covered case as missing, never the reverse). No unreachable-arm warnings. Matches beyond the analysis limits get T083. | Gap. |
| SEM-6 | Trait methods are callable without the trait being imported (unspecified in Phase 0). | [interp]. |
| SEM-7 | `take` is optional on arguments to `own` parameters; moves and use-after-move are PURM (Phase 7). | Deferred to PURM. |
| SEM-8 | Effects: names validated, inference/checking not implemented. Contracts: type-checked, not proven or runtime-checked yet. | Deferred (effects checker; contract prover). |
| SEM-9 | U-coded features (spec/semantics.md §7): dyn, units, projections, async/comptime/kernel, refinements, comptime constants, variant constructors as values, slices, interpolation, ranges as values, `for ... with`, Iterator. | Each needs its listed dependency. |
| SEM-10 | No orphan rule across packages yet (single package plus core). | Needed with the package manager. |
| SEM-11 | Integer-literal range errors report the literal's magnitude (`129` for `-129`). | Cosmetic. |
| SEM-13 | Constant initializers beyond integer/bool arithmetic (calls, floats, strings, aggregates) are type-checked but not evaluated until the comptime interpreter exists. | Deferred (Phase 4 interpreter). |
| SEM-12 | Params in the AST carry no spans; parameter diagnostics point at the function. | Parser AST enhancement (no grammar change). |

## PIR v0.1 and the reference interpreter

| ID | Issue | Status |
|---|---|---|
| PIR-1 | PIR uses an SSA CFG with block arguments, not structured control flow as Phase 0 describes for PIR-high. | [deviation]; add structured regions with the optimizer (Phase 8). |
| PIR-2 | No PIR *text* parser. The binary format exists (PIRB 1.0, spec/pir-bridge.md) with a lossless round-trip test over every corpus and fuzzed module (PIR0.4). | Text parser open. |
| PIR-3 | References and borrow operations exist (PIR0.3); explicit `end_borrow` markers and per-parameter borrow contracts in PIR signatures and interface metadata do not (a borrowed result is tied to all borrowed inputs). | Open (PURM phase 2). |
| PIR-4 | Interpreter: `f16`/`bf16` arithmetic traps as not implemented. | Open. |
| PIR-5 | Interpreter: `extern` calls trap (no FFI in the interpreter). | By design until native code. |
| PIR-6 | Closures capture by value (non-Copy captures move in); assigning to a captured variable is L002. | Capture modes: PURM phase 2. |
| PIR-7 | No I/O: programs report results only through `main`'s return value (no standard library). | Needs runtime + stdlib (Phase 9). |
| PIR-8 | Constants are re-evaluated at every use (as parameterless functions), not folded. | Optimization (Phase 8) / comptime interpreter. |
| PIR-9 | Phase 0 Phase-4 gate items: UB detection is by construction (no UB ops exist); "seeded safety violations" corpus is the trap tests in tests/run. | Expand with PURM. |

## PURM v0.1 (spec/purm.md)

| ID | Issue | Status |
|---|---|---|
| PURM-1 | Partial moves track field paths of locals; moving out through an array index is rejected (O004), not tracked per element. | By design for v0.1. |
| PURM-2 | Closures are never Copy; capture is by value; a closure body cannot move a captured value (O008). Capture modes (borrowing captures) are not designed yet. | Open. |
| PURM-3 | Destruction of temporaries and of an overwritten field (`s.f = v`) is not traced; no user destructors exist, so this is unobservable in v0.1. | Open (with destructors / stdlib resources). |
| PURM-4 | Region blocks: no allocation APIs, no borrow-carrying structs in named regions; escape impossible by construction (no nameable region type). | Needs stdlib allocators. |
| PURM-5 | Inferred borrow contracts are not exported in interface metadata (Phase 0 §6); single-package compilation only. | Needs the package/interface format. |
| PURM-6 | Share/Transfer capabilities, task scopes, async borrows across suspension, lock-guard rules, disjoint slices. | Phase 7–9. |
| PURM-7 | Function types carry no parameter modes: closures take parameters by value, and functions with `inout`/`own` parameters or a borrowed return cannot be function values (T065). | Spec question: modes in `fn(...)` types. |
| PURM-8 | `match`/`if let` on a non-Copy scrutinee whose patterns bind a non-Copy part consumes the whole scrutinee (even on the non-matching path). | [interp], conservative. |
| PURM-9 | A borrowed result is tied to every borrowed argument (conservative): `fn f(a: R, b: R) -> in R = a` called as `f(x, temp)` cannot be forwarded from another borrowed-return function. Per-parameter inference needs borrow contracts. | Open (PURM phase 2). |
| PURM-10 | Overlap of a borrowed result with a later write of its source is impossible in v0.1 because non-Copy borrowed results cannot be stored (O002); storing borrows needs region-scoped borrows. | By design for v0.1. |

## Native backend v0.1 (spec/native.md)

| ID | Issue | Status |
|---|---|---|
| NATIVE-1 | Direct LLVM IR text emission only; the MLIR dialect and importer (same PIRB contract, same llvm-project commit) are not started. MLIR is not available in this build environment; CI provisions clang + MLIR from llvm-project `c13b7485b879` (PIR0.4.1). | Next backend stage (PIR0.5). |
| NATIVE-2 | Subset: floats, `char`, `str`, enums, closures and function values, raw pointers, regions and `extern` calls are refused as unsupported. | Expand behind the differential tests. |
| NATIVE-3 | No backtrace on a trap (the interpreter prints one). | Needs debug info / unwinding. |
| NATIVE-4 | No step limit natively (an interpreter test guard, not semantics); the 100 000-frame depth limit is enforced, on a 4 GiB reserved stack. | By design. |
| NATIVE-5 | The C++ backend is built and tested on Linux (GCC 13, clang 18.1.3) only; the Windows paths (runtime thread creation, binary-mode output, `.exe` naming in the differential tool, MSVC environment in CI) are written but untested. | Gate 1 CI. |
| NATIVE-6 | `drop` emits nothing: no type in the subset has a destructor. | With destructors. |
| TOOLCHAIN-1 | Ordinary CI consumes prebuilt, hash-pinned, attested LLVM/MLIR packages (spec/native.md §7.1); the packages are built by `.github/workflows/toolchain.yml`. Until the first packages are published and recorded in `toolchain-packages.lock.json`, CI's fetch step fails by design. The first CI run showed `ubuntu-latest` = ubuntu-26.04 with GCC 15.2, which cannot compile llvmorg-18.1.3 (missing `<cstdint>`); builder and Linux CI are pinned to ubuntu-24.04. | Open until both packages pass the CI gate. |
| TOOLCHAIN-2 | Local builds with a distribution clang (which reports no commit) need `-DPEGASUS_ALLOW_UNPINNED_LLVM=ON`; the configure step and the `toolchain_pinned` test then say the commit is NOT enforced. The PIR0.4.1 local verification ran this way (Ubuntu clang 18.1.3). | By design. |
