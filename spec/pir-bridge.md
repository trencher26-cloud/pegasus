# The PIR bridge: PIRB 1.0 and the backend C ABI 1.0

Status: **frozen** at checkpoint PIR0.4. Implemented by
`compiler/pir/src/bridge.rs` (writer and reference reader) and
`backend/src/pirb.cpp` (C++ reader); the C ABI is
`backend/include/pegasus/backend.h`.

## 1. Contract

The Rust frontend owns every semantic check: name resolution, typing,
PURM ownership and borrowing, lowering, and the PIR verifier
(spec/pir.md §4). A PIRB file is written only for a module that passed the
verifier (`bridge::encode` runs it and refuses otherwise), and carries
everything a backend needs: types, struct and enum definitions, function
signatures (reference parameters and returns included), slots, SSA values,
every instruction (references, `borrow`, `move`, `drop` included) and the
control-flow graph.

A backend consumes PIRB and must not re-implement type checking or PURM.
It must validate the file's *structure* — the encoding, the checksum, and
that every identifier is in range — and fail closed on anything else it
cannot rely on, never crash. Both future lowering paths (the direct LLVM IR
emitter and the MLIR importer) consume the same PIRB semantics.

## 2. File layout

All multi-byte fixed-width integers are little-endian. `uleb` is unsigned
LEB128 (at most 64 bits). `str` is `uleb` byte length + UTF-8 bytes.
`vec<X>` is `uleb` count + elements. `id` is a `uleb` that fits in 32 bits.

```
file    := "PIRB" major:u16 minor:u16 flags:uleb entry:uleb module crc:u32
module  := structs:vec<struct> enums:vec<enum> funcs:vec<func>
struct  := name:str fields:vec<(name:str, type)>
enum    := name:str variants:vec<(name:str, vec<type>)>
func    := name:str kind:u8 [abi:str if kind = 1] params:vec<type> ret:type
           slots:vec<type> slot_names:vec<str> values:vec<type> blocks:vec<block>
block   := params:vec<id> insts:vec<inst> term
inst    := has_result:u8 [result:id] tag:u8 operands
```

- `major` = 1, `minor` = 0. A reader rejects a different major version and
  a higher minor version (a minor version may only add tags).
- `flags` must equal 1 in any file a backend consumes: bit 0 = *verified*
  (set only by the Rust frontend; see §2.3);
  bits 1–63 are reserved and zero. A reader rejects a file without bit 0 and
  a file with any reserved bit set (a future minor version that defines a
  flag also raises `minor`, which older readers reject).
- `entry` = function id + 1 of the program's entry (`main`), or 0 for none.
- `crc` = CRC-32 (IEEE 802.3, reflected, polynomial `0xEDB88320`, initial
  value and final XOR `0xFFFFFFFF`) of every preceding byte. It detects
  accidental corruption; it is not a security measure.
- After `module`, only `crc` may follow.

### 2.1 Tags

| Kind | Tags |
|---|---|
| function kind | 0 defined, 1 extern (+ ABI string) |
| integer type | 0 i8, 1 i16, 2 i32, 3 i64, 4 i128, 5 isize, 6 u8, 7 u16, 8 u32, 9 u64, 10 u128, 11 usize |
| float type | 0 f16, 1 bf16, 2 f32, 3 f64 |
| type | 0 Int(int type), 1 Float(float type), 2 Bool, 3 Char, 4 Str, 5 Tuple(vec<type>), 6 Array(type, len:uleb), 7 Struct(id), 8 Enum(id), 9 Fn(vec<type>, type), 10 Ptr(mut:u8, type), 11 Region, 12 Ref(mut:u8, type) |
| constant | 0 Int(bits:u128, int type), 1 Float(bits:u64, float type), 2 Bool(u8), 3 Char(scalar:uleb), 4 Str(str) |
| projection | 0 Field(id), 1 Index(value id) |
| binary op | 0 add, 1 sub, 2 mul, 3 div, 4 rem, 5 pow, 6 shl, 7 shr, 8 and, 9 or, 10 xor |
| overflow mode | 0 trap, 1 wrap, 2 sat |
| unary op | 0 neg, 1 not, 2 bitnot |
| comparison | 0 eq, 1 ne, 2 lt, 3 le, 4 gt, 5 ge |
| terminator | 0 br(edge), 1 cond_br(cond, edge, edge), 2 switch(v, vec<(key:u128, block)>, default), 3 ret(v), 4 trap(str), 5 unreachable |

`edge := block:id args:vec<id>`; booleans are one byte, 0 or 1. `u128` is
16 bytes little-endian (an integer constant's two's-complement bit pattern
in its type's width).

### 2.2 Instructions

| Tag | Instruction | Operands |
|---|---|---|
| 0 | `const` | constant |
| 1 | binary | op:u8 mode:u8 a b |
| 2 | unary | op:u8 a |
| 3 | `cmp` | op:u8 a b |
| 4 | `cast` | a type |
| 5 | `load` | slot path |
| 6 | `store` | slot path v |
| 7 | `tuple` | vec<id> |
| 8 | `struct` | struct-id vec<id> |
| 9 | `variant` | enum-id variant vec<id> |
| 10 | `array` | vec<id> |
| 11 | `repeat` | v n:uleb |
| 12 | `extract` | v index |
| 13 | `extract_index` | v i |
| 14 | `tag` | v |
| 15 | `variant_field` | v variant index |
| 16 | `call` | func-id vec<id> |
| 17 | `call_indirect` | v vec<id> |
| 18 | `closure` | func-id vec<id> |
| 19 | `move` | slot vec<field-id> |
| 20 | `drop` | slot |
| 21 | `borrow` | slot path mut:u8 |
| 22 | `borrow` through a reference | ref path mut:u8 |
| 23 | `load` through a reference | ref path |
| 24 | `store` through a reference | ref path v |

`path := vec<projection>`. Operand meanings are those of spec/pir.md §2.

### 2.3 Unverified files and `pegasus pirb-verify`

A file with `flags = 0` is *unverified*: well formed, but no claim that the
module passed the PIR verifier. Tools that produce PIRB from something
other than the Rust frontend (`pegasus-mlir export`, spec/mlir.md) write
flags 0. An unverified file is valid only as input to
`pegasus pirb-verify in.pirb out.pirb`; every consumer (the C++ reader, the
Rust `bridge::decode`) refuses it as "not marked verified".

`pegasus pirb-verify` (`bridge::certify`) parses the file structurally,
refuses reserved flag bits, runs the PIR verifier and the entry check, and
writes the module with `bridge::encode` — so its output is the verified
file the frontend would have written. It accepts verified input too
(certifying is idempotent). Exit status: 0 written; 1 the verifier
rejected the module (`error: PIR verifier: …`), and no file is written;
2 usage or I/O error; 3 malformed file (`error: malformed PIRB: …`).

This adds no format change: readers already refused flags 0.

## 3. Structural validation (every reader)

A reader rejects, without crashing: a bad magic; an unsupported version; a
checksum mismatch; a truncated file or trailing bytes; an unknown tag; a
LEB128 value over 64 bits or an `id` over 32 bits; a length longer than the
remaining input; a struct, enum, function, block, slot or value identifier
out of range; a value defined twice or never; an entry block whose
parameter count differs from the function's; an extern function with a
body; a missing *verified* flag or any reserved flag bit; an entry that is not a defined function
without parameters. Strings are length-checked. The Rust reference reader
also requires UTF-8 and runs the full PIR verifier on the decoded module.

A backend that relies on typing facts the verifier established (an operand
is an array, an instruction has a result) checks each before use and
reports "PIR inconsistent with its verification" instead of crashing.

## 4. C ABI 1.0 (`backend/include/pegasus/backend.h`)

```c
const char *pegasus_backend_version(void);
pegasus_status pegasus_backend_emit_llvm(const uint8_t *pirb, size_t len,
                                         char **out_ir, char **out_error);
void pegasus_backend_free(char *s);
```

Statuses: `PEGASUS_OK` (0), `PEGASUS_ERR_FORMAT` (1, malformed or
unsupported-version PIRB), `PEGASUS_ERR_UNSUPPORTED` (2, valid PIR outside
the native subset, spec/native.md), `PEGASUS_ERR_USAGE` (3). On success
`*out_ir` holds the LLVM IR text; otherwise `*out_error` holds a message;
both are released with `pegasus_backend_free`. Additions bump
`PEGASUS_BACKEND_ABI_MINOR`; any change to an existing declaration bumps
`PEGASUS_BACKEND_ABI_MAJOR`.

## 5. Tests

- `compiler/pir/tests/bridge.rs`: lossless round trip of every corpus
  module (byte-identical re-encoding), header and version rules, the CRC-32
  check value, refusal of unverified modules, every truncation refused, and
  single-byte corruptions caught by the checksum and — with the checksum
  recomputed — never panicking the reader.
- `compiler/pir/examples/pir_fuzz.rs`: every module from the fuzzing
  corpus round-trips.
- `backend/tests/abi_test.cpp` (ctest `backend_abi`): the C ABI on real
  frontend output: translation, unsupported reporting, usage errors,
  version and flag checks, every truncation, and 20 000 corruptions with
  the checksum recomputed (also run under AddressSanitizer and
  UndefinedBehaviorSanitizer for the PIR0.4 checkpoint).
