# PIR0.5b — lowering through MLIR (PIR0.5, second part)

A second native path: PIRB → `pg` dialect → LLVM dialect → LLVM IR, with
exactly the direct emitter's semantics, checked three ways against the
reference interpreter. The direct LLVM IR emitter stays the default and the
fallback. Built on PR #3 (PIR0.5a: dialect, importer/exporter, the Rust
certification gate).

## Added

- `backend/mlir/lib/Lowering.cpp`, `include/pegasus/mlir/Lowering.h`: the
  `pg` → `llvm` lowering (spec/mlir.md §5). Reachability and support check
  first (the direct emitter's native subset and refusal messages), then a
  full dialect conversion with one pattern per operation and a type
  converter, then the C entry point. Run-time checks call internal
  `alwaysinline` helpers (`pg.check`, `pg.check_index`, `pg.check_shift`)
  so that no pattern edits the CFG; at `-O2` none remain. The 128-bit
  division, `**` and array-fill helpers are the direct emitter's, block for
  block, built in the LLVM dialect.
- `pegasus-mlir lower <in.pirb> <out.ll>`: a drop-in for `pegasus-native`
  (same exit statuses; 4 for an internal lowering failure). It reads only
  verified PIRB.
- `pegasus build --backend=direct|mlir` (default `direct`); the MLIR path
  finds `pegasus-mlir` through `PEGASUS_MLIR`.
- `tools/native_diff.py --mlir`: the three-way differential test. Both
  backends build every program; both executables must match the interpreter
  (stdout, exit status, trap message) and the backends must agree on the
  native subset. ctest `native_differential` runs three-way whenever MLIR is
  built (CI: Linux and Windows).
- Unit tests (`mlir_unit`): a callee looping back to its entry block
  (trampoline), a module without an entry, an unsupported operation in an
  unreachable block (still refused, as by the direct emitter), an
  unreachable extern (ignored). `mlir_roundtrip`: `lower` refuses
  unverified PIRB like every other consumer.

## Found while building it

- MLIR's conversion driver visits only reachable blocks, and the frontend
  leaves unreachable trap blocks (the failure arm of an irrefutable pattern).
  The lowering checks every block first (so the native subset matches the
  direct emitter, which emits dead blocks) and then erases unreachable
  blocks, which cannot run.

## Docs

spec/mlir.md §5 (new; Build is now §6), spec/native.md §1, DEFERRED
(MLIR-2 implemented, NATIVE-1 resolved; new MLIR-4 subset growth in both
backends, MLIR-5 MLIR-level optimization).
