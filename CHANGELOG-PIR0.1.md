# Pegasus — PIR v0.1, HIR → PIR lowering, reference interpreter (checkpoint PIR0.1)

Based on S0.2 (SHA-256 `f6538578e84e12fc2a2ab687fb3746e6f8810b4b70d69b93f5d5d4f556099b4f`),
which is unchanged. First checkpoint at which Pegasus programs execute.

## Added

- `compiler/pir` (`pegasus-pir`):
  - PIR v0.1 data structures and text printer (`spec/pir.md`): SSA CFG with
    block arguments, stack slots, overflow mode on every integer operation,
    bounds-checked indexing, explicit traps.
  - Verifier: single definition, dominance, instruction typing, call and
    block-argument arity/types, terminators, returns.
  - Lowering from typed HIR: monomorphization, trait method resolution,
    pattern compilation, loops with break values, `defer` on every scope
    exit, `?`, `requires`/`ensures` as runtime checks, closure lifting,
    constants.
  - Reference interpreter: explicit frame stack, step and depth limits,
    Phase 0 integer semantics, traps with backtraces.
- Driver: `pegasus pir file.peg` (print verified PIR), `pegasus run file.peg`
  (run `main`; trap → exit 101; ICE → exit 70).
- `tests/run`: execution corpus (13 programs: arithmetic, control flow,
  traits/generics/closures, data and `?`, 9 trap cases).
- `tools/int_oracle.py`: differential oracle — an independent Python model
  of the Phase 0 integer rules compared with the compiler + interpreter
  (ctest runs 300 random operations).
- `compiler/pir/tests`: execution corpus runner, semantic accept corpus
  runs, verifier negative tests.

## Changed

- `pegasus_sema::check_package_with` passes `&mut Sema` (lowering fills caches).

## Verification

See the checkpoint report. Highlights: 20,017 mutated programs → 8,174
accepted by sema, 6,746 executed, 0 verifier failures, 0 panics; 3,200
random integer operations agree with the Python model (a planted
saturation bug produces mismatches).
