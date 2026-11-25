# PURM — the Pegasus Unified Resource Model (v0.1)

Status: the rules in sections 1–6 are implemented by `compiler/sema/src/purm.rs`
(checker on typed HIR) and by PIR references, `move` and `drop` (checkpoint PIR0.3). This
refines Phase 0 §6. Interpretations are marked **[interp]** and listed in
`DEFERRED.md`.

## 1. Ownership and `Copy`

Every value has exactly one owner: a local variable, a parameter taken
`own`, a field or element of an owned aggregate, or a temporary.

A type is **Copy** when using its value duplicates it bitwise instead of
moving it:

- integers, floats, `bool`, `char`, `str` (a view), raw pointers, `region`;
- tuples and arrays whose element types are Copy;
- a struct or enum for which `impl Copy for T {}` exists (the core marker
  trait `Copy`; every field and payload must itself be Copy: O010);
  `Option<T>` and `Result<T, E>` are Copy when their parameters are;
- a generic parameter bounded by `Copy`.

Function values (closures) are never Copy **[interp]** — they may own
captured values.

## 2. Moves

An expression is evaluated either to **read** a value or to **consume** it.
Consuming a non-Copy place moves it: the place is uninitialized afterwards.
Consuming positions: `let`/`var` initializers, the right side of `=`,
arguments to `own` parameters, returned and `break` values, the value of a
function body, elements of tuple/array/struct/variant constructions,
closure captures (by value in v0.1), the operand of `?`, and a `match`/`if let`
scrutinee whose patterns bind a non-Copy part. Everything else reads:
operator operands, `in` and `inout` arguments, receivers, conditions,
indices.

- Using a moved place — on every path or on some path (a conditional move),
  including from a previous loop iteration — is **O001**.
- Assigning `x = e` re-initializes `x`.
- **Partial moves.** A place is a local followed by a path of fields
  (`s.a.b`). Consuming a non-Copy field moves only that path: `s.a` and
  `s.b` may be moved independently. Afterwards, any use of a place that
  overlaps a moved path — the path itself, a part of it, or a whole that
  contains it (`s` after `s.a` moved) — is O001. Assigning a path
  re-initializes it and everything below it; assigning into a moved path's
  part (`s.a.x = v` after `s.a` moved) is O001. Struct update
  `S { a: e, ..base }` moves `base` only if a field taken from it is
  non-Copy. A partly moved local is destroyed partly (§5).
- Moving out of an array element by index is **O004** (the array would have
  a hole).
- `[v; n]` with a non-Copy `v` and `n ≠ 1` is **O012** (it would duplicate
  ownership).

## 3. Borrows (second-class)

Parameters are shared borrows by default (`in`); `inout` is an exclusive
borrow; `own` transfers ownership. Borrows are second-class: they exist only
as parameters and borrowed returns, never as stored values, so no lifetime
names exist in source (Phase 0 §6, decision 1).

- Moving out of a borrowed parameter (`in`, `inout`, or `self` taken that
  way), out of a borrowed result, or out of a variable captured by a closure
  inside the closure body is **O002** (O008 for captures). Copy values may
  always be read out.
- **Law of exclusivity** at a call: a variable passed with `&` to an `inout`
  parameter, or used as an `inout self` receiver, may not appear in any other
  argument of the same call (**O003**).
- **Borrow semantics.** An `in` argument is a shared reference to the
  caller's place and an `inout` argument an exclusive one: writes through an
  `inout` parameter change the caller's variable (PIR references, spec/pir.md
  §2.1). An argument that is not a place (a temporary) is borrowed from a
  hidden temporary.
- **Borrowed returns** (`-> in T`): the body's value and every `return`
  value are *borrowed*, not moved, so `T` may be non-Copy. Each must be
  derived from a borrowed parameter (**O005**, reported at the offending
  sub-expression). Derived forms: a borrowed parameter; a field or element
  of a derived place; a call to a borrowed-return function **all** of whose
  borrowed arguments are derived — the result is tied to every borrowed
  input (Phase 0 rule 3) — and a block, `if` or `match` whose results are
  all derived. The caller may read a borrowed result, pass it on as an `in`
  argument, or copy a Copy one, but not move a non-Copy one (O002).
- **Function values.** Function types carry no parameter modes and no
  borrowed return, so a function used as a value (not called) must take only
  `in` parameters and return an owned value (**T065**).
- **Regions:** a `region r { ... }` handle cannot leave its block: it has no
  nameable type and nothing can be allocated in a region until the standard
  library exists, so escape is impossible by construction in v0.1.

## 4. `defer`

A deferred expression runs at scope exit, after the scope's statements. It
may read and assign variables but not move them (**O013**); a variable it
reads must still be initialized at every exit of the scope (O001).

## 5. Destruction

At every exit from a scope (normal end, `return`, `break`, `continue`), after
the scope's `defer`s, the owned non-Copy locals declared in that scope that
still hold a value are destroyed in reverse declaration order; `own`
parameters are destroyed at function exit. Moved-from locals are not
destroyed (drop flags, PIR `drop` on an uninitialized slot is a no-op).
Borrowed parameters are never destroyed by the callee.

v0.1 has no user-defined destructors, so destruction is observable only
through the interpreter's drop trace (used by the tests). Temporaries are not
yet traced **[interp]**.

## 6. Diagnostics

| Code | Meaning |
|---|---|
| O001 | use of a moved (or possibly moved) value |
| O002 | cannot move out of a borrowed value |
| O003 | exclusive (`inout`) access overlaps another use in the same call |
| O004 | cannot move out of an array element |
| O005 | a borrowed return must be derived from a borrowed parameter |
| O008 | cannot move a captured variable out of a closure body |
| O010 | `Copy` implemented for a type with non-Copy fields |
| O012 | repeat expression with a non-Copy element |
| O013 | `defer` may not move a value |

## 7. Not yet implemented (see DEFERRED.md)

Partial moves through array elements (O004); borrow-carrying structs inside named regions and region
allocation; inferred borrow contracts exported in interface metadata;
Share/Transfer and task scopes; async borrows across suspension; lock-guard
rules; user destructors and traced temporaries; disjoint slices
(`split_at`).
