# The Pegasus MLIR dialect (`pg`)

Status: dialect and PIRB importer/exporter (PIR0.5, first part) and the
lowering to LLVM through MLIR (`pegasus build --backend=mlir`, PIR0.5,
second part) implemented by `backend/mlir`. The direct LLVM IR emitter
(spec/native.md) remains the default and the fallback.

## 1. Role

The dialect is PIR in MLIR form: one operation per PIR instruction and
terminator, one type per PIR type. It is built from PIRB — the frozen
interchange of verified PIR (spec/pir-bridge.md) — and adds no semantics.
The Rust frontend stays the authority on typing, ownership and borrowing;
the dialect's verifiers check structure only, so that MLIR transformations
cannot silently produce malformed IR. MLIR is for optimization and lowering.

**Trust contract.** MLIR verification guarantees the dialect's structural
invariants (§2), not that the module is valid PIR: a module can verify and
still add two `bool`s, cast lossily, repeat a switch key or return a
reference into its own slots. Only the Rust PIR verifier decides semantic
validity. Hence `pegasus-mlir export` never marks its output verified: it
writes PIRB with `flags = 0`, which every consumer refuses, and
`pegasus pirb-verify in.pirb out.pirb` runs the Rust verifier and writes the
verified file (spec/pir-bridge.md §2.3). The verifier is not reimplemented
in C++.

## 2. Types and operations

| PIR | `pg` |
|---|---|
| integers | `!pg.int<i8>` … `!pg.int<usize>` (`isize`/`usize` stay distinct: trap messages name them) |
| floats | `!pg.float<f16 \| bf16 \| f32 \| f64>` |
| `bool`, `char`, `str`, region | `!pg.bool`, `!pg.char`, `!pg.str`, `!pg.region` |
| tuple, array | `!pg.tuple<(T, U)>`, `!pg.array<N x T>` |
| struct, enum | `!pg.struct<id>`, `!pg.enum<id>` (definitions: module attributes `pg.structs`, `pg.enums`) |
| function, raw pointer | `!pg.fn<(T) -> R>`, `!pg.ptr<T>` / `!pg.ptr<mut, T>` |
| reference | `!pg.ref<T>` (shared), `!pg.ref<mut, T>` (exclusive) |
| slot | `pg.slot` → `!pg.slot<T>`, in the entry block, in slot order |

Functions are `func.func` (symbol `f<id>.<name>`; externs are private
declarations with `pg.abi`). Blocks and block arguments are PIR's.
Operations: `pg.const`, `pg.bin` (`kind`, `ov`), `pg.un`, `pg.cmp`
(`pred`), `pg.cast`, `pg.load`, `pg.store`, `pg.move`, `pg.drop`,
`pg.borrow`, `pg.borrow_ref`, `pg.load_ref`, `pg.store_ref` (with
`exclusive` for borrows), `pg.tuple`, `pg.struct`, `pg.variant`,
`pg.array`, `pg.repeat`, `pg.extract`, `pg.extract_index`, `pg.tag`,
`pg.variant_field`, `pg.call`, `pg.call_indirect`, `pg.closure`, and the
terminators `pg.br`, `pg.cond_br`, `pg.switch`, `pg.ret`, `pg.trap`,
`pg.unreachable`. A place's `path` lists field indices; `-1` marks an index
projection whose value is the next of the op's `indices` operands.

**Structural verifier checks.** These keep MLIR transformations from
producing malformed IR; they are not PIR's semantic rules and make no claim
of parity with `compiler/pir/src/verify.rs` (§1: which operations are
defined on which types, cast losslessness, switch-key uniqueness, ownership
and borrowing are checked only by the Rust verifier). The dialect checks:
integer, float, bool, char and string constants carry the
attribute their type needs, an integer constant has no bits above its
type's width, and a char constant is a Unicode scalar value; operation kinds, overflow modes and predicates
are known; places type-check against their slot or reference, including
struct fields from `pg.structs`; `pg.store_ref` and exclusive reborrows need
an exclusive reference; borrow results have the borrow's exclusivity; switch
keys are `i128` and match the destinations; branch arguments match the
successor's block arguments (MLIR's `BranchOpInterface` check); aggregates
agree with their result types and the module's definitions (`pg.struct` and
`pg.variant` ids equal the result type's and their operands match the
fields; `pg.variant_field` variant and field indices are in range and the
result is that field's type; `pg.tuple`, `pg.array` and `pg.repeat` match
their element types and lengths; `pg.extract` takes an in-range field of a
tuple or struct; `pg.tag` yields a `u32`); array indices, in place paths and
in `pg.extract_index`, are `usize`.

## 3. Import and export

`pegasus-mlir import file.pirb` builds the module; `export` writes
*unverified* PIRB back (flags 0; §1), which `pegasus pirb-verify` certifies.
Apart from the flag, export is the exact inverse of import: the attributes `pg.v` (value
id of each result), `pg.block_params` (value ids of block parameters),
`pg.named_slots` and `pg.pirb_minor` keep what MLIR does not number itself.
MLIR forbids branches to the entry block, so when PIR's `bb0` has
predecessors the importer adds an entry block (`pg.entry_trampoline`) that
branches to it, and export drops it.

Export runs the MLIR verifier first and checks every module attribute it
reads; malformed metadata is an error, never repaired. It writes PIRB 1.0
only: a `pg.pirb_minor` other than 0 is refused, as is any integer kind
outside 0–11 or float kind outside 0–3 (the type verifiers refuse those
too, and the kind accessors are safe on any value).

## 4. Tests

- `tools/mlir_roundtrip.py` (ctest `mlir_roundtrip`):
  - Every program in `tests/run` and `tests/native` must compile to PIRB;
    a failure fails the test. In `tests/sema/accept` the only allowed
    failure is the explicit "has no `fn main()`" outcome, and the skipped
    files are listed by name.
  - Each PIRB goes PIRB → MLIR → verify → print → parse → verify → print
    (identical text), and both the imported and the reparsed module export
    byte-identical PIRB (apart from the verified flag). Each also goes
    through the trust chain `import` → `export` (flag byte 0) →
    `pegasus pirb-verify`, whose output must be byte-identical to the
    original.
  - Every module in `backend/mlir/tests/invalid` must be rejected by
    `pegasus-mlir verify` with the message on its first line.
  - Every module in `backend/mlir/tests/semantic_invalid` verifies as MLIR
    but is invalid PIR: it must export unverified, `pegasus pirb-verify`
    must reject it with the message on its first line and write no file,
    and `pegasus-native` must refuse the export ("not marked verified").
  - `--mutated FILE` round-trips a NUL-separated fuzzing corpus; programs
    the frontend rejects or that lack `main` are counted as skips, any
    other compiler failure fails the test.
- `backend/mlir/tests/unit.cpp` (ctest `mlir_unit`): a loop back to the
  entry block (trampoline added, and dropped on export), refusal of a
  non-Pegasus module, export writing flags 0, and modules that verify but
  must not export: malformed `pg.structs`, `pg.pirb_minor` (including the
  unsupported minors 1 and 70000), `pg.entry` or `pg.block_params`; a
  struct type without a definition; value ids outside 32 bits or `pg.v` not
  an `i64`; entry blocks marked as trampolines that are not exactly the
  importer's; the C++ writer refusing to write a verified file (any
  nonzero flags) or an unsupported minor, and the C++ reader refusing what
  it does write; and integer/float types with invalid kinds (refused by
  `getChecked`, safe in the accessors, refused by export).
- `compiler/pir/tests/bridge.rs`: `certify` turns an unverified file into
  the verified original, is the identity on a verified one, refuses
  malformed files and reserved flags, and runs the verifier.
- The C++ PIRB writer re-encodes every module the C++ reader decodes
  byte-identically with the Rust encoder.

## 5. Lowering to LLVM (`pegasus-mlir lower`)

`pegasus-mlir lower in.pirb out.ll` is a drop-in for `pegasus-native`
(same arguments, exit statuses 0 ok / 1 outside the native subset, "not
supported by the native backend yet: …" / 2 usage / 3 malformed, plus 4 for
an internal lowering failure); `pegasus build --backend=mlir` runs it, found
through `PEGASUS_MLIR`. It reads only verified PIRB (an unverified export is
refused like by every consumer), imports it, verifies the module, lowers it
to the LLVM dialect and translates that to LLVM IR, which clang compiles
with the runtime exactly as on the direct path.

**Contract: the direct emitter's semantics, exactly.** The lowering
(`backend/mlir/lib/Lowering.cpp`) reproduces `backend/src/emit_llvm.cpp`:
the same native subset (spec/native.md §2; refused with the same
messages), integer semantics, trap messages, runtime calls, call-depth limit
and result printing (spec/native.md §3–§4). Steps:

1. Reachability from `pg.entry`, as the direct emitter: only functions the
   entry calls are lowered; a reachable extern is refused; the rest are
   erased.
2. Support check of every block of every reachable function (including
   blocks no path reaches, which the direct emitter also emits), so that
   both backends refuse the same programs; nothing outside the subset
   reaches the conversion. Unreachable blocks are then erased: they cannot
   run, and MLIR's conversion driver only visits reachable blocks.
3. Module-level declarations: the runtime functions and `pg_rt_depth`; the
   128-bit division, `**` and array-fill helpers (the direct emitter's,
   block for block); and the check helpers `pg.check`,
   `pg.check_index` and `pg.check_shift`.
4. A full dialect conversion `pg` + `func` → `llvm`, one pattern per
   operation and a type converter (integers → `iN`, `bool` → `i1`, tuples
   and structs → literal LLVM structs, arrays → LLVM arrays, references and
   slots → `ptr`). Slots become `alloca`s, places GEPs, block arguments stay
   block arguments. Each pattern is a straight-line rewrite: a run-time
   check is a call to an internal `alwaysinline` `pg.check*` helper that
   returns when the condition holds and otherwise calls the runtime's
   `noreturn` trap, so no pattern edits the control-flow graph. At `-O2`
   LLVM inlines the helpers into the direct emitter's check-and-branch, and
   none remain.
5. The C entry point: `pg.program` calls the entry and prints its result in
   the interpreter's format; `main` runs it on the runtime's large-stack
   thread (`pg_rt_run`).

Tests: `tools/native_diff.py --mlir` (ctest `native_differential` when MLIR
is built) is three-way. Every corpus, integer-operator, generated and
(optionally) fuzz-mutated program is built by both backends; both
executables must match the interpreter (stdout, exit status, trap message),
and the backends must agree on which programs are outside the subset.
`mlir_unit` covers a callee looping to its entry block (trampoline), a
module without an entry, an unsupported operation in an unreachable block
(still refused) and an unreachable extern (ignored); `mlir_roundtrip`
checks that `lower` refuses unverified PIRB.

## 6. Build

`backend/mlir` builds against the pinned package (`PEGASUS_LLVM_ROOT`,
spec/native.md §7) with the host compiler the package was built with or
newer, without RTTI like LLVM. The package is a Release build: with MSVC,
every target that links MLIR or the backend library uses the release
runtime (`/MD`) in every configuration, so a Debug configuration (single-
or multi-config) still links; CI builds and runs one on Windows. CMake option `PEGASUS_BUILD_MLIR`, on by
default when `PEGASUS_LLVM_ROOT` is set (CI).
