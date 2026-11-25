# Pegasus — Semantic Frontend Specification (sema v0.1)

Status: implemented by `compiler/sema` (Phase 3, checkpoint S0.2). This
document states what the semantic frontend does, normatively for this
implementation. It refines the Phase 0 specification (sections 5–7) and
the grammar (v0.1.2-draft); where it interprets an open point, the
interpretation is marked **[interp]** and listed in `DEFERRED.md`.

Every construct the grammar accepts falls into one of three groups:

- **checked**: resolved and type-checked as described here;
- **rejected as not implemented**: a `U` diagnostic, never silent acceptance;
- **out of scope for sema**: ownership and borrow rules (PURM), effects
  inference, contract proving — recorded in typed HIR for later phases.

## 1. Packages and modules

A package is a root file plus the files that its `mod name` items load.
Module `m` with directory `D` loads `mod x` from `D/x.peg` or
`D/x/mod.peg`; both existing is R014, neither is R013. The root file's
directory is the package root; `mod x { ... }` (inline) has directory `D/x`.
A file is loaded at most once.

A built-in package `core` (file `<core>`, `compiler/sema/prelude/core.peg`)
defines `Option<T>` and `Result<T, E>` and re-exports their variants. Its
public names form the **prelude**.

## 2. Names, scopes and visibility

Two namespaces: **types** (modules, structs, enums, traits, aliases,
primitive types) and **values** (functions, extern functions, constants,
imported variants). One module may not define a name twice in one namespace
(R001).

`use` paths are package-absolute (grammar §9, open questions): the first
segment names a top-level item of the current package, `self` (the current
module) or `core`. Forms: `use a.b`, `use a.b as c`, `use a.{b, c}`,
`use a.*`; an enum may be the container (`use Shape.*`, `use Shape.Circle`).
Imports are resolved to a fixed point, so their order does not matter.

Unqualified lookup in a module: its own items and explicit imports, then
glob imports (several distinct definitions under one name: R005 when used),
then the prelude, then primitive type names. Inline modules do **not** see
their parent's names **[interp]**.

An item is visible outside its module only if `pub`; a private item is
visible in its module and that module's descendants. The same rule applies
to struct fields (R011). `pub use` re-exports. Enum variants are as visible
as their enum; trait members as their trait.

Dotted expressions (`a.b.c`) are resolved left to right: a local variable
starts a value; otherwise a value-namespace name starts a value, and a
type-namespace name starts a path (module → member, enum → variant,
type → associated function).

**Patterns [interp]:** an unqualified name in a pattern first resolves among
the variants of the expected (scrutinee) enum type, as in the Phase 0
example `match self { Circle(r) => ... }` and rule F. Expressions are not
contextual: write `Shape.Circle(1.0)` or `use Shape.*`.

## 3. Items

- **Generics:** type and const parameters; bounds inline and in `where`
  clauses on parameters (U005 otherwise); defaults for trailing type
  parameters. Const parameters are integers or `bool` (T040).
- **Functions:** parameter modes `in` (default, shared), `inout`
  (exclusive), `own` (moves in). Missing return type means `()`.
  `async`, `comptime` and `kernel` functions: U004. Effect names must be
  among `alloc io block panic unsafe nondet` (T016); effect inference and
  checking are deferred. `requires`/`ensures` are type-checked as `bool`
  (`ensures` sees `result`); proving and runtime checks are later phases.
- **Structs/enums:** fields and variants are lowered with their generics.
  A type containing itself by value is T020.
- **Aliases** expand transparently; a cycle is T013; refinement aliases
  (`where self < N`) are U006.
- **Traits:** methods (with optional default bodies), associated types,
  constants, supertraits. Inside a trait, `Self` is a parameter bounded by
  the trait.
- **Impls:** inherent impls only for structs and enums of the same package
  (T030); every impl parameter must appear in the self type or trait
  arguments (T032). A trait impl must provide every member without a
  default (T034), only trait members (T033), with signatures equal to the
  trait's after substitution (T035). Two impls of a trait whose types unify
  conflict (T031); duplicate inherent names conflict (R001).
  `Self.X` resolves to the impl's associated type; projections elsewhere are U003.

### 3.1 Constant evaluation

Constants (`const` items), array lengths and const generic arguments are
evaluated at compile time. Evaluation is **typed**: an unsuffixed literal
takes the expected type (the constant's declared type; `usize` for array
lengths; the parameter's declared type for const arguments), every
intermediate result must fit its type (the full range of every integer type,
`i128::MIN` to `u128::MAX`, is representable), and the Phase 0 integer rules
apply: overflow, division by zero, `MIN / -1`, and a shift by at least the
operand's bit width are errors (T017); `<<` discards shifted-out bits; `>>`
is arithmetic for signed and logical for unsigned types; `MIN % -1` is 0;
shift amounts and exponents are unsigned. Supported: integer and boolean
literals (including `-literal`), constants, const generic parameters, and
the unary and binary operators. Other initializers (calls, floats, strings,
aggregates) are type-checked but evaluated only by the comptime interpreter
(not implemented yet); where a value is needed now (array length, const
argument), they are U007.

## 4. Types

Primitive: `bool char str i8..i128 isize u8..u128 usize f16 bf16 f32 f64`.
Compound: tuples (`()` is unit), arrays `[T; N]` (N a constant), structs,
enums, `fn(A) -> R`, raw pointers `*const T` / `*var T`, `T?` = `Option<T>`.
`dyn` (U001) and units of measure (U002) are not implemented.
`isize`/`usize` are 64-bit: every v1 target is 64-bit **[interp]**.

### 4.1 Inference

Local, unification-based, one table per body; signatures are always
explicit. An unsuffixed integer literal has some integer type (default
`i32`), a float literal some float type (default `f64`); a shift amount or
integer exponent literal defaults to `u32`. A value whose type cannot be
determined is T050. There are **no implicit conversions** (T001/T002);
`never` (`return`, `break`, `continue`, a diverging block) flows into any type.

### 4.2 Operators (primitives only; operator traits come with the stdlib)

| Operators | Operands | Result |
|---|---|---|
| `+ - * / %` | same integer or float type | that type |
| `+% -% *%` `+\| -\| *\|` | same integer type | that type |
| `& \| ^` | same integer type, or `bool` | that type |
| `<< >>` | integer, unsigned integer amount | left type |
| `**` | integer with unsigned exponent; float with integer or same float | base type |
| `== !=` | same type: numbers, bool, char, str, tuples/arrays of these | bool |
| `< > <= >=` | same type: numbers, char, str | bool |
| `&& \|\| !` | bool (`!` on an integer: T092, "use `~`") | bool |
| unary `-` / `+` / `~` | signed int or float / number / integer | operand type |

Violations are T003. Integer literals out of range for their final type
are T091.

### 4.3 Places and mutability

`let` bindings are immutable, `var` bindings mutable; parameters are
immutable unless `inout` or declared `var`. Assignment (`=`, `op=`) needs a
mutable place: a mutable local, or a field/element of one (T060, T061).

### 4.4 Casts

`as` performs **lossless** conversions only (Phase 0 §5): integer widening
(same signedness, or unsigned to strictly wider signed); integer to float
when exact (≤ 8 value bits → f16/bf16, ≤ 24 → f32, ≤ 53 → f64); float
widening; `bool` to any integer; `char` to an integer with ≥ 21 value bits;
`u8` to `char`. Anything else is T090.

### 4.5 Calls

Arguments are positional, then named (`f(1, b: 2)`; T056–T058). An `inout`
parameter requires `&` on the argument, which must be a mutable place
(T062, T060); `&` or `take` elsewhere is T063. A method's `inout self`
receiver must be a mutable place. Calling an `extern` function requires
`unsafe(cap: FFI)` (T100); capability names are fixed (T101).

Method lookup: inherent impls of the receiver's type, then trait impls
(including trait default methods); on a generic parameter, the traits of
its bounds and their supertraits. Ambiguity is T052. Generic calls record
their bounds as obligations, checked after inference (T064).

### 4.6 Control flow

`if` without `else` and loop bodies have type `()`; a block whose value is
not wanted evaluates its last expression for effect **[interp]**. `loop`
has the type of its `break` values (or `never`); `break` with a value
outside `loop` is T070. `for` iterates over integer ranges `a..b`, `a..=b`
and over arrays; other iterables need the stdlib Iterator (U015). `e?` on
`Option` needs an `Option` return type; on `Result<T, E>`, a
`Result<_, E>` return type (T080; no error conversion).

## 5. Patterns

Wildcard, bindings (`var x` mutable), literals, ranges (integers and
chars; empty ranges T075), tuples, arrays with one `..`, variants, structs
(missing fields need `..`, T048), or-patterns (each alternative binds the
same names with the same types, T072). `match` must be exhaustive (T082);
`let`, `for` and closure parameters must be irrefutable (T081). Integers,
chars, strings and floats have no finite constructor set: matching them
needs a catch-all arm **[interp]**. Arms with guards do not count toward
exhaustiveness. Fixed-size arrays are analysed by the positions their
patterns constrain (longest prefix plus longest suffix), never element by
element. The analysis runs within fixed limits (depth 1024, one million
steps); if it cannot finish it reports T083 and asks for a catch-all arm —
an undecided match is never accepted.

## 6. Typed HIR

The output (`compiler/sema/src/hir.rs`): every name resolved, every
expression typed, methods as calls with the receiver first, arguments in
parameter order, struct fields in declaration order. It is the input to
PIR lowering.

## 7. Diagnostic catalogue

R — resolution

| Code | Meaning |
|---|---|
| R001 | name, field, variant or parameter defined twice |
| R002 | unresolved import |
| R003 | path segment is not a module |
| R004 | private item |
| R005 | ambiguous glob-imported name |
| R006 | cannot find type |
| R007 | `Self` outside impl/trait/struct/enum |
| R008 | `self` outside a method / not first |
| R009 | cannot find value |
| R010 | no such item in module |
| R011 | private field |
| R013 | module file or root file not found |
| R014 | module file ambiguous or loaded twice |

T — types

| Code | Meaning |
|---|---|
| T001 | mismatched types |
| T002 | operands of different types |
| T003 | operator not applicable to type |
| T010 | not a type / not a trait |
| T011 | wrong number of generic arguments |
| T012 | generic argument of the wrong kind |
| T013 | recursive type alias |
| T014 | invalid array length |
| T015 | recursive constant |
| T016 | unknown effect |
| T017 | constant evaluation error (overflow, division by zero, shift ≥ width, type mismatch) |
| T020 | type contains itself by value |
| T030 | inherent impl not allowed here |
| T031 | conflicting trait impls |
| T032 | impl parameter unused by its type |
| T033 | impl member not in trait |
| T034 | missing trait member |
| T035 | method signature differs from trait (types, modes, `in` return, or stricter generic bounds) |
| T036 | associated constant type differs from trait |
| T037 | associated type does not satisfy the trait's bounds |
| T040 | invalid const parameter type |
| T041 | no such field |
| T042 | type cannot be indexed |
| T043 | invalid struct literal / variant construction form |
| T044 | field given twice |
| T045 | `..base` in a variant literal |
| T046 | missing fields in struct literal |
| T047 | unit or struct variant called with arguments |
| T048 | struct pattern omits fields without `..` |
| T050 | type annotations needed |
| T051 | no such method |
| T052 | ambiguous method |
| T053 | no such associated item |
| T054 | expected a value |
| T055 | associated function called as a method |
| T056 | argument order or form |
| T057 | unknown or repeated named argument |
| T058 | wrong number of arguments |
| T059 | value is not callable |
| T060 | not mutable |
| T061 | not a place |
| T062 | `inout` argument without `&` |
| T063 | misplaced `&` / `take` marker |
| T064 | trait bound not satisfied |
| T065 | function used as a value has `inout`/`own` parameters or a borrowed return (function types carry no modes) |
| T070 | `break`/`continue` misuse |
| T071 | invalid `for` range |
| T072 | or-pattern binding mismatch |
| T073 | misplaced `..` in pattern |
| T074 | range pattern on a non-integer, non-char type |
| T075 | empty range pattern |
| T076 | tuple/array pattern on a wrong type |
| T077 | pattern arity mismatch |
| T078 | variant/struct pattern form error |
| T079 | float literal in pattern |
| T080 | invalid `?` |
| T081 | refutable pattern where irrefutable required |
| T082 | non-exhaustive match |
| T083 | exhaustiveness could not be decided within limits; add a catch-all arm |
| T090 | `as` is not lossless |
| T091 | integer literal out of range |
| T092 | `!` on an integer |
| T100 | extern call outside `unsafe(cap: FFI)` |
| T101 | unknown capability |

U — valid syntax, not implemented yet

| Code | Construct | Needs |
|---|---|---|
| U001 | `dyn Trait` | vtables |
| U002 | units of measure | dimension checker |
| U003 | associated type projections | projection normalization |
| U004 | `async` / `comptime` / `kernel` fn | runtime / interpreter / GPU backend |
| U005 | `where` on non-parameter types | general predicate solving |
| U006 | refinement aliases | contract prover |
| U007 | non-trivial constant expressions in types | comptime interpreter |
| U008 | variant constructors as values | closure synthesis |
| U009 | slicing | stdlib `Span<T>` |
| U010 | string interpolation | stdlib `String` |
| U011 | `await`, async closures | runtime |
| U012 | `comptime` blocks | interpreter |
| U013 | range values outside `for` | stdlib `Range` |
| U014 | `for ... with` | parallel runtime |
| U015 | iteration beyond ranges and arrays | stdlib `Iterator` |
