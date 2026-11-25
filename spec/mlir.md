# The Pegasus MLIR dialect (`pg`)

Status: dialect and PIRB importer/exporter implemented by `backend/mlir`
(PIR0.5, first part). Lowering to LLVM through MLIR is the next part; the
direct LLVM IR emitter (spec/native.md) remains the default and the
fallback.

## 1. Role

The dialect is PIR in MLIR form: one operation per PIR instruction and
terminator, one type per PIR type. It is built from PIRB — the frozen
interchange of verified PIR (spec/pir-bridge.md) — and adds no semantics.
The Rust frontend stays the authority on typing, ownership and borrowing;
the dialect's verifiers check structure only, so that MLIR transformations
cannot silently produce malformed IR. MLIR is for optimization and lowering.

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

Verifiers (never broader than PIR's, `compiler/pir/src/verify.rs`):
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

`pegasus-mlir import file.pirb` builds the module; `export` writes PIRB
back. Export is the exact inverse of import: the attributes `pg.v` (value
id of each result), `pg.block_params` (value ids of block parameters),
`pg.named_slots` and `pg.pirb_minor` keep what MLIR does not number itself.
MLIR forbids branches to the entry block, so when PIR's `bb0` has
predecessors the importer adds an entry block (`pg.entry_trampoline`) that
branches to it, and export drops it.

## 4. Tests

- `tools/mlir_roundtrip.py` (ctest `mlir_roundtrip`): every corpus program
  with a `main` goes PIRB → MLIR → verify → print → parse → verify → print
  (identical text), and both the imported and the reparsed module export
  byte-identical PIRB. `--mutated` does the same for a fuzzing corpus.
  Every module in `backend/mlir/tests/invalid` must be rejected with the
  message on its first line.
- `backend/mlir/tests/unit.cpp` (ctest `mlir_unit`): a loop back to the
  entry block (trampoline added, and dropped on export), refusal of a
  non-Pegasus module, and modules that verify but must not export: malformed
  `pg.structs`, `pg.pirb_minor`, `pg.entry` or `pg.block_params`; a struct
  type without a definition; value ids outside 32 bits or `pg.v` not an
  `i64`; and entry blocks
  marked as trampolines that are not exactly the importer's.

Export runs the MLIR verifier first and checks every module attribute it
reads; malformed metadata is an error, never repaired.
- The C++ PIRB writer re-encodes every module the C++ reader decodes
  byte-identically with the Rust encoder.

## 5. Build

`backend/mlir` builds against the pinned package (`PEGASUS_LLVM_ROOT`,
spec/native.md §7) with the host compiler the package was built with or
newer, without RTTI like LLVM. The package is a Release build: with MSVC,
every target that links MLIR or the backend library uses the release
runtime (`/MD`) in every configuration, so a Debug configuration (single-
or multi-config) still links; CI builds and runs one on Windows. CMake option `PEGASUS_BUILD_MLIR`, on by
default when `PEGASUS_LLVM_ROOT` is set (CI).
