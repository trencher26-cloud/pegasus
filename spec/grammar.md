# Pegasus — Grammar Specification (EBNF, v0.1)

Nov 22, 2026 · @siii

## 1. Scope, notation and conformance

This document defines exactly which character sequences are Pegasus programs and how they group into syntax trees; semantics are out of scope and live in the type-system, PURM and PIR-high specifications. A parser conforms if, for every input, it accepts exactly the sentences this grammar generates after applying the newline rules (section 3) and the ambiguity rules (section 7), and produces the same tree shape.

### Notation

| Form | Meaning |
| --- | --- |
| `rule ::= ...` | Production |
| `"fn"` | Literal terminal (keyword or punctuation) |
| `UPPER_CASE` | Lexical token, defined in section 2 |
| `lower_case` | Syntactic rule |
| `a b` | Sequence |
| `a \| b` | Alternative |
| `a?` `a*` `a+` | Optional, zero or more, one or more |
| `( ... )` | Grouping |
| `list(x, sep)` | Shorthand for `x (sep x)* sep?`: one or more `x`, trailing separator allowed |
| `[^ ... ]` | Any character except those listed (lexical rules only) |

### Design properties

- **Three stages.** A lexer turns UTF-8 text into tokens, including `NEWLINE`; a newline filter (section 3), a deterministic state machine defined there, removes the `NEWLINE` tokens that do not end statements, except that a few it only marks dormant (rule 2a); the parser consumes what remains and decides the dormant ones. The lexer is context-free except for string-interpolation holes (2.7), which it tracks with a mode stack.
- **Deterministic, with explicit disambiguation.** This is a deterministic grammar whose remaining choice points are resolved by the rules in section 7. Most decisions need one token of lookahead; rule B uses a bounded token scan; rule C uses an O(1) lookup in a bracket-match table that the lexer builds during its single O(n) pass; rules F and G record syntax and leave the meaning to the resolver. Parsing never backtracks and is linear in input size. No stronger formal property (such as LL(1)) is claimed until the reference parser generator demonstrates it.
- **No semantic feedback.** The parser never consults name resolution or types. Where meaning depends on names (for example whether `m` in `f64<m>` is a unit or a type), the tree records the syntax and the resolver decides.
- **Machine-readable copy.** The normative grammar is also kept as `spec/pegasus.ebnf`; CI checks it against this document and generates a reference parser for differential testing (section 9).

**Revision.** This text is v0.1.2 (draft): v0.1.1, the consistency pass after the first review, plus the corrections found while implementing the parser. The changes are listed in section 9. It is frozen once the conformance corpus passes on both parsers.

## 2. Lexical grammar

The lexer reads UTF-8 source and produces tokens by **maximal munch**: at each position it takes the longest character sequence that forms a valid token, with the two exceptions in 2.5 (numbers after `.`) and 2.6 (`>` inside generic argument lists).

### 2.1 Source text

- Source files are UTF-8. A byte order mark at the start is ignored. Invalid UTF-8 is a lexical error.
- **Unicode version:** identifier properties and NFC normalization use one pinned Unicode version, chosen at the Phase 1 toolchain freeze and recorded in `toolchain.lock`. It changes only in a language edition, so all conforming compilers agree on which identifiers are valid.
- Identifiers are NFC-normalized before comparison; other text (strings, comments) is kept byte-exact, with one exception: each line ending inside a raw string becomes a single LF in the string's value (2.7).
- Line terminators are LF or CR LF. A lone CR is a lexical error.
- Bidirectional control characters (U+202A–U+202E, U+2066–U+2069) are errors outside comments and strings, and warnings inside them (Trojan Source mitigation).

### 2.2 Whitespace and comments

```ebnf
LINE_END      ::= "\n" | "\r\n"
WHITESPACE    ::= ( " " | "\t" )+                          (* not emitted *)
NEWLINE       ::= LINE_END                                 (* emitted; filtered in section 3 *)
COMMENT_CHAR  ::= [^\r\n]                                   (* never consumes any part of a LINE_END *)
LINE_COMMENT  ::= "//" ( [^/\r\n] COMMENT_CHAR* )?          (* "//", "// text" *)
                | "////" COMMENT_CHAR*                      (* four or more slashes: ordinary comment *)
                                                           (* not emitted; the following LINE_END still yields NEWLINE *)
DOC_COMMENT   ::= "///" ( [^/\r\n] COMMENT_CHAR* )? DOC_END (* exactly three slashes *)
DOC_END       ::= LINE_END | (* end of input, not consumed *)
BLOCK_COMMENT ::= "/*" ( BLOCK_COMMENT | BLOCK_CHAR )* "*/"  (* nests; not emitted *)
BLOCK_CHAR    ::= COMMENT_CHAR | LINE_END                   (* a lone \r is never a BLOCK_CHAR *)
EOF           ::= (* end of input; emitted once, last *)
ERROR         ::= (* a character that starts no token, or a malformed token; emitted; see section 8 *)
```

**Scanning a block comment:** the lexer reads left to right; at each position it recognizes `/*` (open a nested comment) and `*/` (close the innermost) before single characters, so `**/` ends a comment and `/*/` opens one. No comment body, line or block, can contain a lone `\r`: one is a lexical error, as everywhere (2.1), and token boundaries are identical for LF and CR LF files.

A lone `\r` inside a comment is a lexical error, as everywhere else (2.1), so comment bodies never contain `\r` and token boundaries are identical for LF and CR LF files.

The alternatives are disjoint: after `//`, a third `/` followed by a non-`/` character (or by a line end or end of input) makes a doc comment; two or more further `/` make a line comment.

- A block comment containing a line terminator yields one `NEWLINE` in its place.
- **Doc comments own their line ending**, so no `NEWLINE` separates a doc comment from the item, field or variant it documents. A doc comment must be followed (after any further doc comments and attributes) by an item, field or variant; otherwise it is a recoverable “dangling doc comment” error.
- `////` and longer runs are ordinary line comments, not doc comments.

An unterminated block comment runs to end of file and is reported at its opening `/*`.

### 2.3 Identifiers and keywords

```ebnf
ID_START    ::= "_" | (* any character with Unicode property XID_Start *)
ID_CONTINUE ::= (* any character with Unicode property XID_Continue *)
WORD        ::= ID_START ID_CONTINUE*                     (* always read to its longest extent *)

KEYWORD     ::= "as" | "async" | "await" | "break" | "comptime" | "const" | "continue" | "defer"
              | "else" | "enum" | "ensures" | "false" | "fn" | "for" | "if" | "impl" | "in"
              | "kernel" | "let" | "loop" | "match" | "mod" | "pub" | "requires" | "return"
              | "self" | "Self" | "struct" | "trait" | "true" | "type" | "unsafe" | "use"
              | "var" | "where" | "while"             (* 36 reserved words; compared against the whole WORD *)
CONTEXTUAL_KEYWORD ::= "own" | "inout" | "take" | "effects" | "extern" | "with" | "region" | "dyn" | "cap"
                                                          (* lexed as IDENT; a keyword only where a production names it *)

IDENT       ::= WORD - KEYWORD - "_"
RAW_IDENT   ::= "r#" ( WORD - "self" - "Self" - "_" )
RAW_IDENT_INVALID ::= "r#" ( "self" | "Self" | "_" )     (* emitted as ONE ERROR token, diagnostic L008 *)
UNDERSCORE  ::= "_"

name        ::= IDENT | RAW_IDENT                         (* every declared or referenced ordinary name *)
```

**Scanning rule for `r` and `r#`.** When the lexer sees `r#`: if the next character is `"` or `#`, it scans a `RAW_STRING` (2.7); if it is an `ID_START` character, it reads the longest `WORD` after `r#` and then classifies the whole run — `self`, `Self` or `_` gives one `ERROR` token spanning `r#` and the word (L008), anything else gives one `RAW_IDENT`. So `r#selfish` is a valid raw identifier and `r#self` is a single error, never `r`, `#`, `self`. If `r#` is followed by anything else, `r` is an `IDENT` and `#` an `ERROR` token (L009). Contextual keywords need no lexer rule at all: they are always `IDENT` tokens, and the productions that use them (`param_mode`, `extern_fn`, `region_stmt`, ...) name them as terminals.

- `name` is used everywhere the grammar declares or refers to an ordinary identifier: items, parameters, fields, variants, paths, patterns and named arguments. `self` and `Self` are separate terminals and cannot be raw.
- `r#` followed by `"` or `#` starts a raw string (2.7); followed by an `ID_START` character it starts a raw identifier. The two never overlap.
- A raw identifier denotes the same name as its text without `r#`: `r#with` and `with` are the same identifier.

**Reserved keywords (36)** — never identifiers:

`as async await break comptime const continue defer else enum ensures false fn for if impl in kernel let loop match mod pub requires return self Self struct trait true type unsafe use var where while`

**Contextual keywords (9)** — identifiers everywhere except the stated position:

| Word | Keyword only when |
| --- | --- |
| `own`, `inout` | First token of a parameter's type |
| `take` | First token of a call argument |
| `effects` | After a function's return type or parameter list |
| `extern` | At item start, followed by a string literal (the ABI name, as in `extern "C" fn`) |
| `with` | Inside a `for` header, after the iterable |
| `region` | At statement start, followed by a `name` (an `IDENT` or `RAW_IDENT`) and `{` |
| `dyn` | First token of a type |
| `cap` | First token inside `unsafe(` |

This revises the Phase 0 list: `task` and `spawn` are library names (`task.scope`, `s.spawn`), so they cannot be reserved; `as`, `self`, `Self`, `true` and `false` must be. `dyn` and `cap` join the contextual set.

### 2.4 Punctuation and operators

Longest match applies, so `**=` is one token, not `**` then `=`.

```ebnf
PUNCT ::= "**=" | "<<=" | ">>=" | "..="                             (* three characters *)
        | "**" | "+%" | "-%" | "*%" | "+|" | "-|" | "*|" | "<<" | ">>" | "<=" | ">="
        | "==" | "!=" | "&&" | "||" | ".." | "=>" | "->" | "+=" | "-=" | "*=" | "/="
        | "%=" | "&=" | "|=" | "^="                                      (* two characters *)
        | "+" | "-" | "*" | "/" | "%" | "&" | "|" | "^" | "~" | "!" | "<" | ">" | "="
        | "?" | "." | "," | ":" | ";" | "@" | "(" | ")" | "[" | "]" | "{" | "}"  (* one character *)
```

**Token priority at each position**, applied in this order: whitespace and line ends; comments (`//`, `/*`) before `/`; `r#` and `b"` prefixes (2.3, 2.7) before identifiers; numeric literals (with the `.`-after-dot exception in 2.5); string and character literals; `WORD`s, classified as `KEYWORD`, `UNDERSCORE` or `IDENT`; then `PUNCT` by longest match. `#`, `$`, `` ` `` and `\` outside literals start no token and become `ERROR` (L009).

### 2.5 Numeric literals

```ebnf
DIGIT        ::= "0" .. "9"
HEX_DIGIT    ::= DIGIT | "a" .. "f" | "A" .. "F"
HEX_DIGITS   ::= HEX_DIGIT ( HEX_DIGIT | "_" )*
OCT_DIGITS   ::= ( "0" .. "7" ) ( "0" .. "7" | "_" )*
BIN_DIGITS   ::= ( "0" | "1" ) ( "0" | "1" | "_" )*
DEC          ::= DIGIT ( DIGIT | "_" )*
INT_LIT      ::= ( DEC | "0x" HEX_DIGITS | "0o" OCT_DIGITS | "0b" BIN_DIGITS ) INT_SUFFIX?
FLOAT_LIT    ::= DEC "." DEC EXPONENT? FLOAT_SUFFIX?
               | DEC EXPONENT FLOAT_SUFFIX?
               | DEC FLOAT_SUFFIX
EXPONENT     ::= ( "e" | "E" ) ( "+" | "-" )? DEC
INT_SUFFIX   ::= "i8" | "i16" | "i32" | "i64" | "i128" | "isize" | "u8" | "u16" | "u32" | "u64" | "u128" | "usize"
FLOAT_SUFFIX ::= "f16" | "bf16" | "f32" | "f64"
```

- A float needs digits on both sides of the point: `1.` and `.5` are not literals, so `1..2` lexes as `1`, `..`, `2`.
- **Exception to maximal munch:** directly after a `.` token, the lexer reads digits as `INT_LIT` only, so tuple access `t.0.1` lexes as `t . 0 . 1`, not `t . 0.1`.
- **Suffix boundary:** if an `ID_CONTINUE` character directly follows a numeric literal and the characters do not form a valid suffix, the whole run is one `ERROR` token (`2ms` → error, hint: write `ms(2)`). `0xFFu8` is valid; in hex literals, `e` and `f` digits are digits, never an exponent or suffix start.
- A trailing underscore is allowed (`1_000_`).
- Units are not part of literals. A quantity gets its unit from its type: `let g: f64<m/s**2> = 9.81`, or `9.81 as f64<m/s**2>`.

### 2.6 `>` inside generic argument lists

The lexer always produces the longest token. When the parser expects the `>` that closes a generic argument list and the current token begins with `>` but is longer (`>>`, `>=`, `>>=`), it **splits** the token: it consumes one `>` and replaces the current token with the remainder (`>>` → `>`, `>=` → `=`, `>>=` → `>=`), with the remainder's position one column later. The split repeats for each closing level, so `Vec<Vec<Vec<i32>>>` closes three lists from one `>>` and one `>`. A split never happens outside a generic argument list or a generic parameter list.

### 2.7 Character and string literals

```ebnf
CHAR_LIT     ::= "'" ( [^'\\\n\r] | ESCAPE ) "'"
STRING_LIT   ::= '"' ( STR_CHAR | ESCAPE | "{{" | "}}" | INTERP )* '"'
STR_CHAR     ::= [^"\\{}\n\r]                              (* single-line: no raw line breaks *)
INTERP       ::= "{" expression ( ":" FORMAT_SPEC )? "}"   (* lexer switches to expression mode *)
FORMAT_SPEC  ::= [^{}"\n\r]*                                (* no escapes; ends at "}" *)
RAW_STRING   ::= "r" HASHES '"' RAW_CHAR* '"' HASHES        (* both HASHES equal in length *)
HASHES       ::= "#"*                                       (* 0 to 255 *)
RAW_CHAR     ::= [^\r] | "\r\n"                             (* LF and CR LF allowed; a lone \r is an error *)
                                                           (* the string ends at the first '"' followed by the same number of "#" *)
BYTE_STRING  ::= "b" '"' ( ASCII_CHAR | BYTE_ESCAPE )* '"'
ASCII_CHAR   ::= (* U+0020 to U+007E except '"' and "\\" *)
BYTE_ESCAPE  ::= "\\" ( "n" | "r" | "t" | "0" | "\\" | '"' ) | "\\x" HEX_DIGIT HEX_DIGIT
ESCAPE       ::= "\\" ( "n" | "r" | "t" | "0" | "\\" | "'" | '"' | "{" | "}" ) | "\\u{" HEX_DIGIT{1,6} "}"
```

**Line endings in raw strings are normalized:** each `LINE_END` inside a raw string contributes a single LF (U+000A) to the string's value, whether the source used LF or CR LF. A program's string values therefore never depend on how a checkout stores line endings. A string that needs a CR uses an ordinary string with `\r`.

- **Line breaks:** ordinary and byte strings are single-line. Text spanning lines uses a raw string (`r"..."`, `r#"..."#`), which may contain line breaks; those become part of the value and produce no `NEWLINE` tokens. This keeps recovery simple: an unterminated ordinary string always ends at its line (section 8).
- **Interpolation holes:** inside a hole the lexer tokenizes an expression, tracking bracket depth. The hole's expression ends at the first `:` or `}` at bracket depth 0; a `:` starts the `FORMAT_SPEC`, which runs to the next `}`. (A top-level `:` cannot occur inside an expression, because named arguments and struct-literal fields are always inside brackets.) Holes may not contain string literals with holes of their own (depth limit 1 in v1).
- **Format specifications** are passed to the formatting library unparsed; their meaning is defined there, not by the grammar.
- `\u{...}` must name a Unicode scalar value (no surrogates).

## 3. Newlines and statement termination

A `NEWLINE` ends a statement unless the newline filter drops it. The filter is a deterministic state machine that sits between lexer and parser; it sees tokens only, tracks bracket nesting and one *item-header* state, and never consults the parser. Its full definition follows; nothing else decides which newlines reach the parser, except for the dormant newlines of rule 2a, which the filter marks and the parser resolves by one fixed test.

### Suppression rules

**Filter state.**

| Component | Content |
| --- | --- |
| `stack` | Open brackets `(` `[` `{`. An opener is pushed. A closer pops down to the nearest opener of its own kind; if none is open, it pops the innermost opener, as a mistyped closer (section 8) |
| `prev` | The last token passed to the parser (initially start-of-file) |
| `header` | Either off, or `{ depth, is_fn, angles, params_seen }`: the bracket depth where an item header began, whether it is a function header (`fn` or `extern`), the open `<` count, and whether its parameter list has started |

**Item start.** A token is *at item start* when `prev` is start-of-file, `NEWLINE`, `;`, `{`, `}`, `DOC_COMMENT`, `pub`, `async`, `comptime`, `kernel`, `extern`'s string literal, or the last token of an attribute (`@` followed by a dotted name and an optional bracket-balanced `( ... )`).

**Header entry.** When `fn`, `struct`, `enum`, `trait`, `impl`, `type` or `extern` arrives at item start and `header` is off, set `header = { depth: stack size, is_fn: (token is fn or extern), angles: 0, params_seen: false }`.

**Header bookkeeping**, for tokens at the header's depth: before `params_seen`, `<` adds one to `angles`, `>` subtracts one and `>>` two (never below 0). A `>=` or `>>=` there is closing angle(s) followed by `=` (2.6): it subtracts one or two, and its `=` then ends the header if `angles` reached 0, so `type A<T>=X` behaves exactly like `type A<T> = X`. In a function header (`is_fn`), the first `(` **with `angles = 0`** sets `params_seen`; a `(` inside the generic angles, as in `fn f<T = (i32, i32)>(x: T)`, belongs to a type and does not. In other headers `params_seen` stays false. The header **ends** at a `{` or `=` with `angles = 0`, at a `NEWLINE` that survives, or when `stack` drops below `depth`.

**Decision for each `NEWLINE`.** Let `next` be the next token that is not a NEWLINE (so blank lines never change a decision; rule 5 removes the extra NEWLINEs). The `NEWLINE` is dropped when the first matching rule applies; otherwise it is kept:

1. **Inside `( )` or `[ ]`:** the top of `stack` is `(` or `[`. (Newlines inside `{ }` are kept, because braces delimit blocks.)
2. **Continuation token at line end:** `prev` is a binary operator (`** * / % *% *| + - +% -% +| -| << >> & ^ | == != < > <= >= && || .. ..=`), an assignment operator (`= += -= *= /= %= &= |= ^= <<= >>= **=`), one of `, . : -> => ( [ {`, or the keyword `as`, `else` or `in`.
3. **Leading dot or `else`:** `next` is `.` (not `..`) or `else`.
4. **Header clause:** `header` is on and `next` is `requires`, `ensures`, `where`, `effects`, `->` or `{`. This lets clauses, and a brace on its own line, follow a signature, while `fn f() = x` followed by a line starting with `{` stays two statements, because the header ended at `=`.
5. **Duplicate:** `prev` is already `NEWLINE`; runs collapse to one.

A kept `NEWLINE` also ends any active header.

**Rule 2a: dormant newlines.** Rule 2 cannot tell a comparison `>`, a shift `>>` or a product `*` at a line end from a generic closer (2.6) or a `use` glob, which end a type or a path and should end the line. So when the first matching rule is rule 2, `prev` is `>`, `>>` or `*`, and `next` is none of `.` `else` `{` `where` `requires` `ensures` `effects` `->`, the `NEWLINE` is *dormant* instead of dropped. The filter's state is updated exactly as for a dropped `NEWLINE` (a dormant newline does not end a header). The parser ignores a dormant `NEWLINE` unless it consumes the token before it as the `>` that closes a generic list (for `>>`, its second `>`) or as the `*` of a `use` glob; then the `NEWLINE` is kept. Rule B (section 7) treats a dormant `NEWLINE` after its closing `>` as a non-follower. So:

| Code | Parses as |
| --- | --- |
| `type A = Vec<u8>` / `type B = u8` | Two items |
| `a: Vec<u8>` / `b: i32` in a struct body | Two fields |
| `use std.math.*` / `use std.io` | Two items |
| `ready = a >` / `b` | One statement, `ready = a > b` |
| `x = a >>` / `b` | One statement, `x = a >> b` |
| `f < T >` / `(x)` | `f < T > (x)`: comparisons, so P002 (the call would need `(` on the closer's line) |

The decision is still made once per token without backtracking: the parser knows, when it consumes `>` or `*`, which role the token plays. A line that ends with a generic closer therefore ends there, unless the next line starts with `.`, `else`, `{` or a header clause.

**Semicolons are not newlines.** The filter passes `;` through unchanged. The grammar uses it in two distinct roles:

```ebnf
stmt_sep ::= ( NEWLINE | ";" )+        (* between statements and items, sections 4 and 5 *)
```

and as the length delimiter inside `[T; N]` and `[value; count]`. Because rule 1 drops all newlines inside brackets and `stmt_sep` never occurs inside `[ ]`, the two roles cannot collide.

### Newlines as list separators

Inside `{ }` lists that are comma-separated — struct fields, enum variants, struct-literal fields, and `match` arms — a surviving `NEWLINE` may stand in for the comma:

```ebnf
brace_sep ::= "," NEWLINE? | NEWLINE
```

### Deliberate consequences

| Code | Parses as | Why |
| --- | --- | --- |
| `let x = a` / `+ b` | Two statements: `let x = a`, then the expression statement `+b` (unary plus) | Leading binary operators do not continue a line; put the operator at the end of the first line. |
| `foo` / `.bar()` | `foo.bar()` | Rule 3 |
| `}` / `else {` | One `if` expression | Rule 3 |
| `return` / `x` | `return` with no value, then `x` | `return` is not a continuation token; the compiler then warns that `x` is unreachable |
| `f(a,` / `b)` | `f(a, b)` | Rule 1 |

Two further cases: a signature followed by `requires x > 0` and then `{` on separate lines is one function (rule 4). `Vec<` at a line end continues onto the next line (rule 2), but a line may not *start* with the closing `>` of a generic list, so break after `<` or `,`.

The formatter always puts binary operators at the end of a broken line and leading dots at the start, so formatted code never depends on the subtle cases.

## 4. Modules, items and types

A source file is one module: a sequence of items separated by newlines. Items never nest except through inline `mod` blocks, `impl` blocks and trait bodies.

### 4.1 Modules and items

```ebnf
module      ::= stmt_sep? ( item ( stmt_sep item )* stmt_sep? )? EOF
item        ::= DOC_COMMENT* attribute* "pub"? item_kind
item_kind   ::= use_decl | mod_decl | fn_decl | extern_fn | struct_decl | enum_decl
              | trait_decl | impl_decl | type_alias | const_decl

use_decl    ::= "use" use_tree
use_tree    ::= path ( "as" name | "." "{" list(use_tree, ",") "}" | "." "*" )?
mod_decl    ::= "mod" name ( "{" stmt_sep? ( item ( stmt_sep item )* stmt_sep? )? "}" )?
path        ::= path_start ( "." name )*
path_start  ::= name | "self" | "Self"

attribute   ::= "@" path ( "(" list(attr_arg, ",")? ")" )? NEWLINE?
attr_arg    ::= ( name ":" )? expression
```

- `mod name` without a body loads `name.peg` or `name/mod.peg`; the module system spec defines lookup.
- Attribute arguments use `key: value`, the same form as named call arguments. Values must be constant expressions (checked after parsing).
- Attributes always come **before** what they annotate. Phase 0 examples that placed `@layout(soa)` after a type must be rewritten as `@layout(soa) let particles: Vec<Particle> = load()`.

### 4.2 Functions

```ebnf
fn_decl     ::= fn_sig fn_body
fn_sig      ::= fn_prefix? "fn" name generic_params? "(" list(param, ",")? ")"
                return_type? effects? clause*
fn_prefix   ::= "async" | "comptime" | "kernel"
fn_body     ::= block | "=" expression                 (* "=" form: syntactic sugar for a one-expression block *)
extern_fn   ::= "extern" STRING_LIT "fn" name "(" list(param, ",")? ")" return_type?

param       ::= self_param | pattern_ident ":" param_mode? type
self_param  ::= param_mode? "self"
param_mode  ::= "in" | "inout" | "own"
return_type ::= "->" "in"? type                        (* "in" marks a returned borrow *)
effects     ::= "effects" "(" list(name, ",")? ")"
clause      ::= "requires" expr_ns | "ensures" expr_ns | "where" list(where_pred, ",")
where_pred  ::= type ":" bounds

generic_params ::= "<" list(generic_param, ",") ">"
generic_param  ::= name ( ":" bounds )? ( "=" type )? | "const" name ":" type
bounds         ::= path_type ( "+" path_type )*
```

- `expr_ns` disables struct literals at the current expression position; nested bracketed expressions follow the reset rules in 6.2 (section 7, rule A), so `requires x > 0 {` never reads `0 {` as a struct literal.
- The `=` body form is accepted anywhere `fn_body` is; in an `extern_fn` there is no body at all.
- At most one `fn_prefix`. `async kernel fn` and similar combinations are syntax errors.

### 4.3 Structs, enums, traits, impls, aliases, constants

```ebnf
struct_decl  ::= "struct" name generic_params? where_clause? "{" NEWLINE? list(field, brace_sep)? "}"
field        ::= DOC_COMMENT* attribute* "pub"? name ":" type
enum_decl    ::= "enum" name generic_params? where_clause? "{" NEWLINE? list(variant, brace_sep)? "}"
variant      ::= DOC_COMMENT* attribute* name ( "(" list(variant_elem, ",") ")"
                                          | "{" NEWLINE? list(field, brace_sep) "}" )?
variant_elem ::= ( name ":" )? type                     (* Circle(r: f64) or Pair(i32, i32) *)

trait_decl   ::= "trait" name generic_params? ( ":" bounds )? where_clause?
                 "{" stmt_sep? ( trait_item ( stmt_sep trait_item )* stmt_sep? )? "}"
trait_item   ::= DOC_COMMENT* attribute* ( fn_sig fn_body?
                                         | "type" name ( ":" bounds )?
                                         | "const" name ":" type ( "=" expression )? )
impl_decl    ::= "impl" generic_params? ( path_type "for" )? type where_clause?
                 "{" stmt_sep? ( impl_item ( stmt_sep impl_item )* stmt_sep? )? "}"
impl_item    ::= DOC_COMMENT* attribute* "pub"? ( fn_decl | type_alias | const_decl )
type_alias   ::= "type" name generic_params? "=" type ( "where" expr_ns )?   (* refinement alias *)
const_decl   ::= "const" name ":" type "=" expression
where_clause ::= "where" list(where_pred, ",")
```

Trait and impl bodies have their own item productions, so a `struct` inside an `impl` is a syntax error where it appears rather than a later check. A trait's last item needs no separator before `}`, so `trait Area { fn area(self) -> f64 }` is valid.

### 4.4 Types

```ebnf
type         ::= "dyn" bounds | fn_type | pointer_type | base_type "?"*
base_type    ::= path_type | tuple_type | array_type
path_type    ::= path generic_args?
generic_args ::= "<" list(generic_arg, ",") ">"
generic_arg  ::= unit_expr | type | const_arg
const_arg    ::= INT_LIT | "-" INT_LIT | "true" | "false" | "{" expression "}"
unit_expr    ::= name unit_power ( ( "*" | "/" ) unit_factor )*
               | name ( ( "*" | "/" ) unit_factor )+
unit_factor  ::= name unit_power? | "(" unit_expr ")"
unit_power   ::= "**" "-"? INT_LIT
tuple_type   ::= "(" ")" | "(" type ")" | "(" type "," list(type, ",")? ")"
array_type   ::= "[" type ";" expression "]"
fn_type      ::= "fn" "(" list(type, ",")? ")" ( "->" type )? effects?
pointer_type ::= "*" ( "const" | "var" ) type
```

The three `generic_arg` alternatives are disjoint by their first two tokens: a `unit_expr` starts with a name followed by `*`, `/` or `**`; a `const_arg` starts with a literal, `-` or `{`; anything else is a `type`. So `Foo<1>` is a constant argument, `f64<m/s>` a unit, and `Vec<T>` a type. A unit always starts with a named unit; a parenthesized group may appear only after `*` or `/` (`kg/(m*s**2)`), because a leading `(` starts a tuple type.

- `T?` is sugar for `Option<T>`. `(T)` is a parenthesized type; `(T,)` is a one-element tuple.
- **Units:** inside generic arguments, an identifier followed by `*`, `/` or `**` starts a `unit_expr` (`f64<m/s**2>`). A lone identifier is parsed as a type and the resolver decides whether it names a unit. Exponents use `**`, consistent with the `^` = XOR decision.
- **Raw pointers** are `*const T` and `*var T`, reusing the `var` keyword for mutability instead of adding `mut`.

**Newlines inside `< >`.** The filter does not track angle brackets, so generic argument and parameter lists follow the ordinary rules: a line break is allowed after `<` and after `,` (rule 2), and nowhere else. A line may not begin with the closing `>`. Inside parentheses or brackets, as in `f(Vec<\n i32>)`, rule 1 already drops every newline.

## 5. Statements, blocks and patterns

A block is a newline-separated sequence of statements whose value is its final expression statement. Assignment is a statement, never an expression, so `if x = y` cannot be written.

### 5.1 Blocks and statements

```ebnf
block       ::= "{" stmt_sep? ( stmt ( stmt_sep stmt )* stmt_sep? )? "}"
stmt        ::= let_stmt | var_stmt | const_decl | assign_stmt | defer_stmt | region_stmt | expr_stmt

let_stmt    ::= attribute* "let" pattern ( ":" type )? "=" expression
var_stmt    ::= attribute* "var" pattern ( ":" type )? "=" expression
assign_stmt ::= expression assign_op expression          (* left side must be a place; checked after parsing *)
assign_op   ::= "=" | "+=" | "-=" | "*=" | "/=" | "%=" | "&=" | "|=" | "^=" | "<<=" | ">>=" | "**="
defer_stmt  ::= "defer" ( block | expression )
region_stmt ::= "region" name block
expr_stmt   ::= expression
```

- `let` and `var` always have an initializer in v1.
- The parser reads an expression, then treats the statement as an assignment if an `assign_op` follows. No lookahead is needed.
- A block's value is its last statement when that statement is an `expr_stmt`; otherwise it is `()`. Whether a non-unit final value may be discarded (for example in a function returning `()`) is a type-system rule, not a grammar rule.
- Assignment is not chainable: `a = b = c` is a syntax error.
- Local `fn`, `struct` and `enum` items inside blocks are not allowed in v1; local `const` is.

### 5.2 Patterns

```ebnf
pattern       ::= "|"? pattern_alt ( "|" pattern_alt )*
pattern_alt   ::= literal_pat | range_pat | "_" | ".." | pattern_ident
                | tuple_pat | array_pat | variant_pat | struct_pat
pattern_ident ::= "var"? name                            (* var: binding is mutable *)
variant_pat   ::= path "(" list(pattern, ",")? ")"
struct_pat    ::= path "{" NEWLINE? list(field_pat, brace_sep)? "}"
field_pat     ::= name ( ":" pattern )? | ".."
tuple_pat     ::= "(" ")" | "(" pattern ")" | "(" pattern "," list(pattern, ",")? ")"
array_pat     ::= "[" list(pattern, ",")? "]"
literal_pat   ::= "-"? INT_LIT | CHAR_LIT | STRING_LIT | BYTE_STRING | "true" | "false"
range_pat     ::= range_bound ( "..=" range_bound | ".." range_bound? ) | "..=" range_bound
range_bound   ::= "-"? INT_LIT | CHAR_LIT | path
```

- Float literals are not allowed in patterns. A `STRING_LIT` in a pattern may not contain interpolation holes.
- `..` (rest) is valid only directly inside tuple, array and struct patterns, at most once per pattern (checked after parsing).
- A `path` with more than one segment in `variant_pat` or `struct_pat` names an enum or struct (`Shape.Circle(r)`); a single segment is resolved by type: arms matching a `Shape` may write `Circle(r)`.
- A lone identifier is recorded as an *identifier pattern*. Whether it binds a new name or matches a unit variant or constant is decided by the resolver (section 7, rule F).

## 6. Expressions and operator precedence

Pegasus has 15 precedence levels. Unlike C, bitwise operators bind tighter than comparisons, so `x & MASK == 0` means `(x & MASK) == 0`, and comparisons do not chain.

### 6.1 Precedence and associativity (highest first)

| Level | Operators | Associativity | Example |
| --- | --- | --- | --- |
| 1 | Postfix: `.field` `.method()` `.0` `()` `[]` `?` | Left | `a.b(c)[i]?` |
| 2 | `await` (prefix, applies to the postfix chain after it) | — | `await http.get(u)?` = `(await http.get(u))?` |
| 3 | `**` | Right | `2**3**2` = `2**(3**2)` |
| 4 | Prefix `+` `-` `!` `~` | Right (prefix) | `-2**2` = `-(2**2)` |
| 5 | `as` | Left | `-x as i64` = `(-x) as i64` |
| 6 | `*` `/` `%` `*%` `*\|` | Left |  |
| 7 | `+` `-` `+%` `-%` `+\|` `-\|` | Left |  |
| 8 | `<<` `>>` | Left |  |
| 9 | `&` | Left |  |
| 10 | `^` | Left |  |
| 11 | `\|` | Left |  |
| 12 | `==` `!=` `<` `>` `<=` `>=` | **None** | `a < b < c` is a syntax error |
| 13 | `&&` | Left |  |
| 14 | `\|\|` | Left |  |
| 15 | `..` `..=` | None | `a..b..c` is a syntax error |
| — | Closure `=>` | Body extends as far right as possible | `x => x + 1` |
| Statement | `=` and compound assignments | Not an expression |  |

Prefix `+` is the identity on numeric types (accepted for symmetry and for the newline case in section 3). `!` is logical NOT on `bool`; `~` is bitwise NOT on integers. Using `!` on an integer is a type error with a hint to use `~`.

### 6.2 Grammar

Expression rules carry one parameter, **\[S\]**, which says whether a struct literal may follow a path at this position: `[+S]` allows it, `[~S]` forbids it, and `[?S]` passes the caller's value through. `expression` means `expression[+S]`; `expr_ns` means `expression[~S]`. Every bracketed context — parentheses, brackets, braces, call arguments, interpolation holes — resets to `[+S]`.

```ebnf
expression[S]   ::= closure[?S] | range_expr[?S]
expr_ns         ::= expression[~S]                                (* no struct literal at this position *)
closure[S]      ::= "async"? closure_params "=>" expression[?S]
closure_params  ::= name | "(" list(closure_param, ",")? ")"
closure_param   ::= pattern ( ":" type )?

range_expr[S]   ::= or_expr[?S] ( ( ".." | "..=" ) or_expr[?S]? )? | ( ".." | "..=" ) or_expr[?S]?
or_expr[S]      ::= and_expr[?S] ( "||" and_expr[?S] )*
and_expr[S]     ::= cmp_expr[?S] ( "&&" cmp_expr[?S] )*
cmp_expr[S]     ::= bor_expr[?S] ( cmp_op bor_expr[?S] )?        (* at most one: non-associative *)
cmp_op          ::= "==" | "!=" | "<" | ">" | "<=" | ">="
bor_expr[S]     ::= bxor_expr[?S] ( "|" bxor_expr[?S] )*
bxor_expr[S]    ::= band_expr[?S] ( "^" band_expr[?S] )*
band_expr[S]    ::= shift_expr[?S] ( "&" shift_expr[?S] )*
shift_expr[S]   ::= add_expr[?S] ( ( "<<" | ">>" ) add_expr[?S] )*
add_expr[S]     ::= mul_expr[?S] ( ( "+" | "-" | "+%" | "-%" | "+|" | "-|" ) mul_expr[?S] )*
mul_expr[S]     ::= cast_expr[?S] ( ( "*" | "/" | "%" | "*%" | "*|" ) cast_expr[?S] )*
cast_expr[S]    ::= unary_expr[?S] ( "as" type )*
unary_expr[S]   ::= ( "+" | "-" | "!" | "~" ) unary_expr[?S] | power_expr[?S]
power_expr[S]   ::= await_expr[?S] ( "**" unary_expr[?S] )?     (* right-assoc *)
await_expr[S]   ::= "await" head[?S] postfix_nq* "?"* | head[?S] postfix*

head[S]         ::= path_head[?S] | primary
path_head[S]    ::= path_chain ( [+S] struct_body )?             (* struct literal: only here *)
path_chain      ::= ( name | "self" | "Self" ) gen_tail? ( "." name gen_tail? )*
gen_tail        ::= generic_args                                  (* only if followed by ( . or {; rule B *)

postfix         ::= postfix_nq | "?"
postfix_nq      ::= "." name gen_tail? | "." INT_LIT | call_args | "[" expression[+S] "]"
call_args       ::= "(" list(call_arg, ",")? ")"
call_arg        ::= ( name ":" )? ( "&" | "take" )? expression[+S]   (* & = exclusive borrow, take = move *)
struct_body     ::= "{" NEWLINE? list(field_init, brace_sep)? "}"
field_init      ::= name ( ":" expression[+S] )? | ".." expression[+S]

primary         ::= literal
                  | "(" ")" | "(" expression[+S] ")" | "(" expression[+S] "," list(expression[+S], ",")? ")"
                  | "[" ( list(expression[+S], ",") | expression[+S] ";" expression[+S] )? "]"
                  | block | "comptime" block | unsafe_block
                  | if_expr | match_expr | while_expr | loop_expr | for_expr
                  | "return" expression[+S]? | "break" expression[+S]? | "continue"
literal         ::= INT_LIT | FLOAT_LIT | CHAR_LIT | STRING_LIT | RAW_STRING | BYTE_STRING | "true" | "false"

if_expr         ::= "if" cond block ( "else" ( if_expr | block ) )?
cond            ::= expr_ns | "let" pattern "=" expr_ns
match_expr      ::= "match" expr_ns "{" NEWLINE? list(match_arm, brace_sep)? "}"
match_arm       ::= pattern ( "if" expr_ns )? "=>" expression[+S]
while_expr      ::= "while" cond block
loop_expr       ::= "loop" block
for_expr        ::= "for" pattern "in" expr_ns ( "with" name call_args )* block
unsafe_block    ::= "unsafe" "(" "cap" ":" path ( "+" path )* ")" block
```

`path_chain` is greedy: it takes every `.name` it can before any call or index, so a struct literal can follow only a bare dotted name with optional generic arguments (`geo.Point { ... }`, `Pair<i32, i32> { ... }`), never a call result or an indexed value. `return` and `break` values are always parsed as `expression[+S]`, as `primary` shows; neither form is useful inside a condition, and the compiler warns when one appears there.

**Where generic arguments may appear in expressions.** Exactly three forms: a generic call `parse<i32>(s)`, a generic method call or path step `obj.get<T>(i)` / `Vec<i32>.new()`, and generic struct construction `Pair<i32, i32> { a: 1, b: 2 }` (outside `expr_ns`). A bare `Foo<T>` followed by anything else is not a generic application; it reads as a comparison and, since comparisons do not chain, usually fails with a hint.

`expr_ns` is defined formally above as `expression[~S]`; it is used by `cond`, `match` scrutinees, `for` iterables, `match` guards, `requires`, `ensures` and refinement `where` clauses.

### 6.3 Notes

- **`await` and `?`:** `await` takes a primary and its postfix chain, minus any trailing `?`s, which then apply to the awaited result. To call a method on an awaited value, parenthesize: `(await f()).g()`.
- **`&` and `take`** are argument markers, not general operators: they are valid only as the first token of a call argument. A binary `&` elsewhere is bitwise AND.
- **Dotted names** (`std.io.print`, `self.src`) are postfix field accesses in the tree; the resolver decides which prefixes are modules.
- **`return`, `break`, `continue`** are expressions of type never, so `let x = y?` and `match ... { None => return }` compose.
- Labels on loops (`'outer:`) are not in v1.

## 7. Ambiguity resolution

Every place where the grammar alone admits two readings is listed here with a single deterministic rule. A conforming parser applies exactly these rules and no others; any input not covered is unambiguous by construction.

| Rule | Situation | Resolution | Cost |
| --- | --- | --- | --- |
| A | `{` after a path in a condition: `if p == Point {` | In `expr_ns` positions (`if`, `while`, `match` scrutinee, `for` iterable, `requires`, `ensures`, refinement `where`), `{` after a path starts the block, never a struct literal. Write `if p == (Point { x: 0 })`. | 1 token |
| B | `<` after an identifier in an expression: `f<T>(x)` vs `a < b` | Scan ahead without consuming. If the tokens form a well-formed `generic_args` (bracket-matched, splitting `>>` as in 2.6, using only tokens that can occur in types) **and** the token after the closing `>` is `(`, `.`, or `{` (the last only outside `expr_ns`, then a struct literal follows), it is a generic argument list. Otherwise `<` is a comparison. The scan stops at the first token that cannot occur in a type and never exceeds 256 tokens (beyond that: syntax error with a hint). The same rule applies after `.name`. | Bounded scan (at most 256 tokens) |
| C | `(` at expression start: closure parameters vs parenthesized or tuple expression | The lexer builds a **bracket-match table** in the same single pass that produces tokens: a stack pairs every `(`, `[` and `{` with its closer, recording each opener's matching index. At a `(` in expression position the parser looks at the token after `match[i]`: if it is `=>`, the group is `closure_params`; otherwise it is an expression. The decision is O(1), so the total cost stays linear however deeply groups nest. An unmatched `(` has no entry, is treated as an expression, and is reported by recovery. A single `name` followed by `=>` is a closure (1 token). | O(1) lookup after O(n) preprocessing |
| D | `await e?` | `?` applies to the awaited result (6.3). | Grammar |
| E | `impl X for Y` vs `impl Y` | Parse a type; if `for` follows, the type must be a `path_type` and becomes the trait. | 1 token |
| F | Identifier pattern: binding vs unit variant or constant (`None =>`) | The parser records an identifier pattern. The resolver treats it as a value pattern if the name resolves to a unit variant of the scrutinee's type or to a `const` in scope; otherwise it binds. A lint warns when a new binding has the same name as a variant of the scrutinee's type. | Resolver |
| G | Identifier vs unit in generic arguments (`f64<m>`) | A lone identifier is a type node; an identifier followed by `*`, `/` or `**` starts a unit expression (4.4). The resolver decides what a lone name denotes. | 1 token |
| H | `region` as a keyword vs a name | Keyword only at statement start when followed by a `name` and `{`; otherwise an identifier. The same pattern applies to all contextual keywords (2.3). | 2 tokens |
| I | Leading `-` or `+` on a new line | A new statement (section 3): `+b` and `-b` are valid unary expressions. The compiler warns that the statement's value is unused. | Newline filter |

### The residual case for rule B

`f(a < b, c > (d))` parses as one argument `a<b, c>(d)` (a generic call), not two comparisons. This is the one place where a valid-looking comparison pair changes meaning, and it is resolved in favour of generics because `a < b > c` can never be a valid comparison chain. The compiler warns when a generic call's arguments name values, and the formatter writes comparisons inside call arguments with parentheses.

### Not ambiguous, by design

- **Dangling `else`:** impossible, since `if` bodies are always blocks.
- **Struct literal after a call:** `{` after `)` always starts a block, never a struct literal.
- **`T?` in types vs `?` in expressions:** types and expressions are separate grammars and never share a parse position.
- **`|` in patterns vs bitwise OR:** patterns and expressions are separate grammars.

## 8. Error-recovery expectations

A conforming parser reports the first error at its exact position, recovers, and keeps going, so that one mistake yields one diagnostic and the rest of the file is still checked. The first error's position is normative and is tested; later diagnostics are best effort but must meet the rules below.

### Required behavior

1. **Never stop at the first error.** Parse to end of file, producing a tree with `Error` nodes where input was skipped or missing. Later phases (name resolution, the language server) must accept trees containing `Error` nodes.
2. **Exact first error.** The first diagnostic points at the first token that cannot continue any valid parse, and names what was expected (`expected expression, found }`).
3. **Synchronize at fixed points**, innermost first:

| Level | Recovery point | Action |
| --- | --- | --- |
| Expression | `,` or the closing bracket of the enclosing list | Insert an `Error` expression, continue the list |
| Statement | Surviving `NEWLINE` or `;` at the current brace depth | Skip to it, insert an `Error` statement |
| Block | The matching `}` (brackets are tracked even inside skipped text) | Close the block |
| Item | A line starting, at brace depth 0 of the enclosing module, `impl` or `trait` body, with any item starter: `fn`, `async`, `comptime`, `kernel`, `extern`, `struct`, `enum`, `trait`, `impl`, `type`, `const`, `use`, `mod`, `pub`, `@` or a doc comment | Skip to it, start a new item |

4. **No cascades.** After a recovery, suppress further diagnostics until at least one token has been consumed successfully at the recovery level. Mismatched brackets produce one diagnostic naming both the opener's and the unexpected closer's positions. The closer then pops down to the nearest opener of its own kind, leaving the openers above it unclosed; if none is open, it closes the innermost opener as a mistyped closer. The newline filter and the bracket-match table apply the same rule, so \`(1, 2\]\` followed by a new line recovers at that line.
5. **Lexical errors are always recoverable:** an invalid character becomes an `ERROR` token; an unterminated ordinary, byte or character literal ends at the end of its line (these literals are single-line, 2.7); an unterminated raw string or block comment runs to end of file and is reported at its opener; a dangling doc comment is reported and dropped. A malformed literal or doc comment (an invalid escape, a lone CR, an unmatched brace, a non-ASCII byte in a byte string) becomes one ERROR token spanning the whole literal, with the diagnostic at the exact offset of the fault; errors inside an interpolation hole stay with the hole's own tokens.
6. **Common-mistake hints** (required, with fixed wording in the diagnostics catalogue): a C-style brace-less `if (x) y`; `=` used in a condition; `a < b < c`; `!` on an integer; a value statement that looks like a broken line (`+b` after a complete statement); `fn` missing its body; `mut` written where `var` is meant; a unit suffix on a literal (`2ms`).
7. **Bounds:** recovery must keep parsing linear in input size. The default diagnostic limit is 100 per file (configurable); beyond it the parser stops reporting but still finishes the tree.
8. **Nesting limit:** expressions, types and patterns nest at most 128 levels (P018), so no input can exhaust a parser's stack. Each expression, type and pattern counts one level, and so does each operand of a prefix operator or the right operand of `**`; a block counts through the expression that contains it. P018 is reported at the first token of the construct that would be level 129, and that construct is skipped by item-level recovery. Interpolation holes continue the depth of the string that contains them.

### Requirements for the language server

The parser must also run incrementally on edited text, with results identical to a full parse of the same text. Because one edit can change lexer state far downstream (opening a raw string or block comment), the incremental algorithm is:

1. The lexer records a **checkpoint** at the start of every line: its mode stack (normal, interpolation hole, raw string with hash count, block comment with depth) and the newline filter's state.
2. After an edit, re-lex from the checkpoint of the first edited line, and continue past the edit until the lexer reaches a line start whose recomputed checkpoint equals the stored one *and* the remaining tokens are unchanged. That is the convergence point.
3. Re-parse the smallest enclosing item (or items) that contain changed tokens; if the change crosses an item boundary or alters bracket balance, re-parse from the previous item boundary to the convergence point.
4. CI checks equivalence by applying random edits and comparing incremental and full parses (section 9).

## 9. Conformance, changes to the Phase 0 spec, and open questions

The grammar is accepted when two independent parsers — the hand-written Rust parser and a parser generated from `spec/pegasus.ebnf` — produce identical trees on the whole conformance suite.

### Conformance suite

| Suite | Content | Pass criterion |
| --- | --- | --- |
| Phase 0 examples | Every code example in the Phase 0 specification, with `...` placeholders replaced by real bodies | All parse; trees reviewed once and then fixed as golden files |
| Positive | ≥ 1,000 valid programs covering every production | Identical trees from both parsers |
| Negative | ≥ 500 invalid programs, each with the expected first-error position and message ID | Exact first error |
| Precedence | Every ordered pair of the 15 levels, plus associativity of each | Tree matches the table in 6.1 |
| Newline filter | Every rule and every header-state transition, both sides of each, and every row of the “deliberate consequences” table | Exact token stream after filtering |
| Lexical | Every token class, suffix boundaries, `>>` splitting, raw identifiers vs raw strings, interpolation and format specs | Exact token stream |
| Ambiguity | At least 10 cases per rule A–I, including the rule B residual case | Specified reading |
| Round-trip | Parse → format → parse yields the same tree | Identical |
| Fuzzing | Grammar-aware and byte-level fuzzing, 24 h per release | No crash, no hang, linear time on all inputs |
| Incremental | Random edits re-parsed incrementally vs. from scratch | Identical |

### Changes this grammar makes to the Phase 0 spec

Writing the grammar exposed nine places where the Phase 0 text was not parseable as written. Each is a syntax-only correction; no decision in the ledger changes.

1. **Keywords:** 36 reserved and 9 contextual (2.3). `task` and `spawn` are no longer reserved; `as`, `self`, `Self`, `true`, `false` are; `dyn` and `cap` are contextual.
2. **Units:** no unit suffix on literals. Write `let g: f64<m/s**2> = 9.81`. Unit exponents use `**`.
3. **Attribute placement:** attributes always precede what they annotate (`@layout(soa) let particles = ...`).
4. **Attribute values** are constant expressions, so `@budget(latency: 2ms)` becomes `@budget(latency: ms(2))`.
5. **Raw pointers:** `*const T` and `*var T` (no `mut`).
6. **Bitwise NOT** is `~`; `!` is logical NOT only.
7. **Lock levels:** generic arguments are positional, so `Mutex<T, level: 3>` becomes `Mutex<T, 3>`.
8. **Cancellation shields:** `task.shield { ... }` would parse as a struct literal; it becomes `task.shield(() => { ... })`.
9. **Operators as values:** `reduce(0, +)` is not an expression; it becomes `reduce(0, (a, b) => a + b)`.

### Open questions (do not block Phase 2)

- Loop labels: needed for breaking out of nested loops; candidate syntax `outer: for ...` with `break outer`.
- Relative `use` paths (`super`, sibling modules): v1 uses package-absolute paths only.
- Nested interpolation (a string with holes inside a hole) is excluded in v1; revisit if the formatter or logging libraries need it.

### Changes in v0.1.1

| # | Change | Section |
| --- | --- | --- |
| 1 | Defined every lexical symbol (`ID_START`, `ID_CONTINUE`, digit classes, `STR_CHAR`, `FORMAT_SPEC`, `HASHES`, `ASCII_CHAR`, `BYTE_ESCAPE`) plus `EOF` and `ERROR`; pinned the Unicode version | 2 |
| 2 | Added `name ::= IDENT \| RAW_IDENT` and used it for every ordinary name | 2.3, 4–6 |
| 3 | `;` is no longer a newline: it is a statement separator (`stmt_sep`) or the array-length delimiter | 3 |
| 4 | Doc comments own their line ending, so they attach to the next item, field or variant | 2.2 |
| 5 | The newline filter is specified as a state machine, including the item-header state | 3 |
| 6 | Unary `+` added, making `+b` on a new line a valid (warned) statement | 3, 6 |
| 7 | A trait's last item needs no separator before `}`; explicit `trait_item` and `impl_item` productions | 4.3 |
| 8 | Expression-level generic arguments limited to calls, path steps and struct construction; `{` added as a rule B follower | 6, 7 |
| 9 | Recovery recognizes `async`, `comptime`, `kernel` and `extern` as item starters | 8 |
| 10 | Ordinary strings are single-line; raw strings carry multi-line text; recovery made consistent | 2.7, 8 |
| 11 | Newlines inside `< >` specified; exact `>>` splitting algorithm | 2.6, 4.4 |
| 12 | Incremental parsing uses lexer checkpoints and runs to a convergence point | 8 |
| 13 | Parser property restated: deterministic with explicit disambiguation rules, not LL(1) | 1 |

**Second pass (still v0.1.1):**

14. `RAW_IDENT` now structurally excludes `r#self`, `r#Self` and `r#_` (2.3).
15. `LINE_COMMENT` and `DOC_COMMENT` made disjoint: exactly three slashes is a doc comment, four or more an ordinary comment; a doc comment may end at end of input, and is then reported as dangling (2.2).
16. `expr_ns` is formal: expression rules carry an `[S]` parameter, and `struct_body` is structurally limited to a bare path chain (6.2).
17. `generic_arg` alternatives are disjoint: units must start with a named unit, so `Foo<1>` is only a constant argument (4.4).
18. Rule C uses a bracket-match table built by the lexer, making the closure decision O(1) and parsing linear (7).

**Third pass (still v0.1.1):**

19. Removed the stale `expr_ns` description in 4.2; 6.2 is the single definition.
20. Complexity labels corrected: rule B is a bounded scan, rule C an O(1) lookup after O(n) preprocessing (1, 7).
21. Contextual-keyword conditions aligned with the productions: `extern` requires an ABI string; `region` is followed by any `name` (2.3, 7).
22. Comment bodies exclude both `\r` and `\n`, so CR LF line endings never leak into comment tokens (2.2).

**Fourth pass (still v0.1.1):**

23. Block comments are built from `BLOCK_CHAR` (no lone `\r`), with the left-to-right scanning rule for `/*` and `*/` stated explicitly (2.2).
24. Raw strings accept LF and CR LF but not a lone `\r`; their line endings normalize to LF in the string value (2.7).

**Fifth pass (still v0.1.1), for the machine-readable grammar:**

25. `KEYWORD` (36 reserved words) and `CONTEXTUAL_KEYWORD` are now productions, so `spec/pegasus.ebnf` has no undefined symbols (2.3).
26. `RAW_IDENT_INVALID` plus an explicit scanning rule: the lexer reads the whole word after `r#`, and `r#self`, `r#Self`, `r#_` become one `ERROR` token each (2.3).
27. Punctuation is a `PUNCT` production, with the token-priority order stated (2.4).

28) Newline filter: `next` is the next token that is not a `NEWLINE`, so a blank line between a signature and its `requires` clause does not end the header (3). Found while implementing the lexer.

29. Header bookkeeping: only a `(` outside the generic angles of a function header starts the parameter list, so a parenthesized default type (`fn f<T = (i32, i32)>`) no longer stops angle tracking. Headers record `is_fn` (3). Found in lexer review.

30) A `>=` or `>>=` closing a generic list in a header also ends the header with its `=` (3).
31) Mismatched closers: pop to the nearest opener of the same kind, else close the innermost opener; same rule in the filter and the bracket-match table (3, 8).
32) Malformed literals and doc comments are one ERROR token spanning the literal (8). All three found in lexer review.

### Changes in v0.1.2

Found while implementing the parser against the corpus.

33. Rule 2a, dormant newlines: a `NEWLINE` after a line-ending `>`, `>>` or `*` is decided by the parser, kept after a generic closer or `use` glob and dropped after an operator. Before this, `type A = Vec<u8>` followed by another item, or two struct fields whose first type ended in `>`, ran together (3, 7 rule B). The filter's state machine is unchanged.
34. Nesting limit of 128 levels, diagnostic P018 (8).
