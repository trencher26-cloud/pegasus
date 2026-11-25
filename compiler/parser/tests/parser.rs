//! Parser tests beyond the conformance corpus: rule 2a (dormant newlines),
//! nesting limits, robustness, linear running time, P1.3 regressions and P1.4 sibling-survival checks.

use pegasus_parser::{
    ast::{FnBody, ItemKind, Stmt},
    fn_shape, fn_stmts, parse_source,
};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::{Duration, Instant};

fn errors(src: &str) -> Vec<(&'static str, u32)> {
    let (_, d) = parse_source(src.as_bytes());
    d.iter()
        .filter(|d| d.severity == pegasus_lexer::Severity::Error)
        .map(|d| (d.code, d.offset))
        .collect()
}

fn ok(src: &str) -> pegasus_parser::ast::Module {
    let (m, d) = parse_source(src.as_bytes());
    assert!(d.is_empty(), "unexpected diagnostics {d:?} for:\n{src}");
    m
}

// ---------------------------------------------------------------- rule 2a

#[test]
fn generic_closer_at_line_end_separates_items() {
    let m = ok("type A = Vec<u8>\ntype B = u8\n");
    assert_eq!(m.items.len(), 2);
}

#[test]
fn split_double_closer_at_line_end_separates_items() {
    let m = ok("type A = Vec<Vec<u8>>\ntype B = Map<K, Vec<V>>\nconst C: u8 = 1\n");
    assert_eq!(m.items.len(), 3);
}

#[test]
fn generic_closer_at_line_end_separates_fields() {
    let m = ok("struct S {\n    a: Vec<u8>\n    b: Map<K, V>\n    c: i32\n}\n");
    match &m.items[0].kind {
        ItemKind::Struct { fields, .. } => assert_eq!(fields.len(), 3),
        k => panic!("{k:?}"),
    }
}

#[test]
fn generic_closer_at_line_end_separates_statements() {
    let m = ok("fn t() {\n    let v: Vec<i32> = make()\n    let w: Vec<i32> = make()\n    x as Vec<u8>\n    y\n}\n");
    assert_eq!(fn_stmts(&m, "t"), Some(4));
}

#[test]
fn line_may_not_start_with_assignment_after_generic_type() {
    // Woken NEWLINE ends `let w: Vec<i32>`, which then lacks `=`.
    assert_eq!(
        errors("fn t() {\n    let w: Vec<i32>\n        = make()\n}\n")[0].0,
        "P001"
    );
}

#[test]
fn glob_at_line_end_separates_items() {
    let m = ok("use std.math.*\nuse std.io\n");
    assert_eq!(m.items.len(), 2);
}

#[test]
fn comparison_at_line_end_still_continues() {
    let m = ok("fn t() {\n    a >\n        b\n}\n");
    assert_eq!(fn_stmts(&m, "t"), Some(1));
}

#[test]
fn multiplication_at_line_end_still_continues() {
    let m = ok("fn t() = a *\n    b\n");
    assert_eq!(fn_shape(&m, "t").as_deref(), Some("(* a b)"));
}

#[test]
fn shift_at_line_end_still_continues() {
    let m = ok("fn t() = a >>\n    b\n");
    assert_eq!(fn_shape(&m, "t").as_deref(), Some("(>> a b)"));
}

#[test]
fn generic_closer_before_brace_line_keeps_header_rule() {
    ok("struct P<T>\n{\n    x: T\n}\nfn f() -> Vec<u8>\n{\n    v\n}\n");
}

#[test]
fn generic_closer_before_leading_dot_continues() {
    let m = ok("fn t() = Vec<i32>\n    .new()\n");
    assert_eq!(
        fn_shape(&m, "t").as_deref(),
        Some("(mcall (gpath Vec (i32)) new)")
    );
}

#[test]
fn generic_closer_inside_parens_stays_dropped() {
    ok("fn t() = f(x as Vec<u8>\n    , y)\n");
}

#[test]
fn braces_inside_parens_follow_the_filter_stack() {
    // A closure body inside a call: the top of the stack is `{`, so rule 1 does
    // not apply and rule 2a separates the statements, as in the filter.
    ok("fn t() = f(x => {\n    let v: Vec<u8> = a\n    v\n})\n");
    ok("fn t() = f(x => {\n    let v: Map<K, Vec<V>> = a\n    v\n}, y)\n");
}

#[test]
fn generic_call_paren_must_share_the_closer_line() {
    // `f<T>` + NEWLINE + `(x)`: the scan reads `<`/`>` as comparisons, which
    // then chain: P002 at the second operator.
    assert_eq!(errors("fn t() = f < T >\n(x)\n")[0].0, "P002");
}

// ---------------------------------------------------------------- shapes

#[test]
fn shapes_cover_the_notation() {
    for (src, want) in [
        ("a.b<T>(c)", "(gmcall a b (T) c)"),
        ("f<m/s**2>(x)", "(gcall f (m/s**2) x)"),
        ("x[i]?", "(try (index x i))"),
        ("..=b", "(range= _ b)"),
        ("a..", "(range a _)"),
        ("[1; n]", "(repeat 1 n)"),
        ("(a, b) => a", "(closure (a b) a)"),
        ("async x => await x", "(closure (x) (await x))"),
        ("P { x, y: 2 }", "(struct P (x) (y 2))"),
        ("~-!+a", "(bnot (neg (not (pos a))))"),
        ("t.0.1", "(field (field t 0) 1)"),
    ] {
        let m = ok(&format!("fn t() = {src}\n"));
        assert_eq!(fn_shape(&m, "t").as_deref(), Some(want), "{src}");
    }
}

#[test]
fn struct_literal_needs_a_bare_path_and_s_context() {
    // In expr_ns (an if condition) `P {` starts the body, not a literal.
    let m = ok("fn t() {\n    if x == P { y }\n}\n");
    assert_eq!(fn_stmts(&m, "t"), Some(1));
    // A call result is not a bare path: `f() { }` is not a struct literal.
    assert!(!errors("fn t() = f() { x: 1 }\n").is_empty());
}

#[test]
fn interpolation_holes_are_parsed() {
    ok("fn t() = \"sum {a + b:>8} and {f(x, y)}\"\n");
    let e = errors("fn t() = \"bad {a +}\"\n");
    assert_eq!(e.first().map(|e| e.0), Some("P001"));
}

// ---------------------------------------------------------------- nesting

/// Callers on a tiny stack are safe too: the parse runs on its own thread.
fn on_small_stack(f: impl FnOnce() + Send + 'static) {
    std::thread::Builder::new()
        .stack_size(256 << 10)
        .spawn(f)
        .unwrap()
        .join()
        .unwrap();
}

#[test]
fn nesting_limit_is_reported_not_overflowed() {
    on_small_stack(|| {
        let n = 100_000;
        let cases = [
            format!("fn t() = {}x{}\n", "(".repeat(n), ")".repeat(n)),
            format!("fn t() = {}x{}\n", "[".repeat(n), "]".repeat(n)),
            format!("fn t() = {}x\n", "-".repeat(n)),
            format!("fn t() = x{}\n", " ** x".repeat(n)),
            format!("fn t() {}{}\n", "{".repeat(n), "}".repeat(n)),
            format!("fn t() = {}x\n", "x => ".repeat(n)),
            format!("type T = {}u8{}\n", "Vec<".repeat(n), ">".repeat(n)),
            format!("fn t() {{ let {}x{} = y }}\n", "(".repeat(n), ")".repeat(n)),
            format!("fn t() = {}0{}\n", "if a { ".repeat(n), " }".repeat(n)),
            format!(
                "fn t() = {}\"{{{}0{}}}\"{}\n",
                "(".repeat(n),
                "(".repeat(n),
                ")".repeat(n),
                ")".repeat(n)
            ),
        ];
        for src in cases {
            let e = errors(&src);
            assert!(
                e.iter().any(|e| e.0 == "P018"),
                "no P018 for {}",
                &src[..40]
            );
        }
    });
}

#[test]
fn nesting_below_the_limit_is_accepted() {
    on_small_stack(|| {
        let n = pegasus_parser::MAX_NESTING - 8;
        ok(&format!("fn t() = {}x{}\n", "(".repeat(n), ")".repeat(n)));
        ok(&format!(
            "type T = {}u8{}\n",
            "Vec<".repeat(n),
            ">".repeat(n)
        ));
    });
}

#[test]
fn holes_continue_the_enclosing_depth() {
    let half = 100; // each side alone is fine; together they exceed MAX_NESTING
    ok(&format!(
        "fn t() = \"{{{}0{}}}\"\n",
        "(".repeat(half),
        ")".repeat(half)
    ));
    let src = format!(
        "fn t() = {}\"{{{}0{}}}\"{}\n",
        "(".repeat(half),
        "(".repeat(half),
        ")".repeat(half),
        ")".repeat(half)
    );
    assert!(errors(&src).iter().any(|e| e.0 == "P018"));
}

// ---------------------------------------------------------------- robustness

fn corpus() -> Vec<PathBuf> {
    fn walk(d: &Path, out: &mut Vec<PathBuf>) {
        for e in std::fs::read_dir(d).unwrap() {
            let p = e.unwrap().path();
            if p.is_dir() {
                walk(&p, out)
            } else if p.extension().is_some_and(|x| x == "peg") {
                out.push(p)
            }
        }
    }
    let mut v = Vec::new();
    walk(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/grammar/cases"),
        &mut v,
    );
    v.sort();
    v
}

#[test]
fn every_prefix_of_every_case_parses_without_panic() {
    let mut n = 0;
    for p in corpus() {
        let src = std::fs::read(&p).unwrap();
        if src.len() > 4096 {
            continue;
        }
        for cut in 0..=src.len() {
            let _ = parse_source(&src[..cut]);
            n += 1;
        }
    }
    assert!(n > 10_000);
}

/// Small deterministic generator (xorshift), so failures reproduce.
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
}

fn parse_with_watchdog(
    src: String,
    timeout: Duration,
) -> (pegasus_parser::ast::Module, Vec<pegasus_lexer::Diagnostic>) {
    let label = src.clone();
    let (tx, rx) = mpsc::sync_channel(1);
    let worker = std::thread::spawn(move || {
        let result = parse_source(src.as_bytes());
        let _ = tx.send(result);
    });
    let result = match rx.recv_timeout(timeout) {
        Ok(result) => result,
        Err(mpsc::RecvTimeoutError::Timeout) => {
            eprintln!("parser exceeded {timeout:?} on input: {label:?}");
            // Rust cannot cancel a stuck thread. Abort the test process so a
            // parser that grows memory indefinitely cannot poison later tests.
            std::process::abort();
        }
        Err(mpsc::RecvTimeoutError::Disconnected) => {
            worker.join().expect("parser watchdog worker panicked");
            panic!("parser watchdog worker exited without returning a result");
        }
    };
    worker.join().expect("parser watchdog worker panicked");
    result
}

fn parse_bytes_finishes_within(src: Vec<u8>, timeout: Duration) {
    let label = format!("{src:?}");
    let (tx, rx) = mpsc::sync_channel(1);
    let worker = std::thread::spawn(move || {
        let _ = parse_source(&src);
        let _ = tx.send(());
    });
    match rx.recv_timeout(timeout) {
        Ok(()) => worker.join().expect("parser watchdog worker panicked"),
        Err(mpsc::RecvTimeoutError::Timeout) => {
            eprintln!("parser hung or exceeded {timeout:?} on input bytes: {label}");
            // Do not leave a detached runaway parser alive after the watchdog fires.
            std::process::abort();
        }
        Err(mpsc::RecvTimeoutError::Disconnected) => {
            worker.join().expect("parser watchdog worker panicked");
            panic!("parser watchdog worker exited without signalling completion");
        }
    }
}

fn parse_finishes_within(src: String, timeout: Duration) {
    parse_bytes_finishes_within(src.into_bytes(), timeout);
}

#[test]
fn token_soup_parses_without_panic() {
    // Fragments drawn from every syntactic area, including the tokens with
    // special handling: generic closers, globs, holes, ERROR-producing text.
    const FRAGS: &[&str] = &[
        "fn",
        "t",
        "(",
        ")",
        "{",
        "}",
        "[",
        "]",
        "<",
        ">",
        ">>",
        ">=",
        ">>=",
        "=",
        "==",
        "=>",
        ",",
        ";",
        "\n",
        ".",
        "..",
        "..=",
        "*",
        "**",
        "-",
        "!",
        "~",
        "?",
        ":",
        "->",
        "@",
        "/// d\n",
        "if",
        "else",
        "match",
        "for",
        "in",
        "while",
        "let",
        "var",
        "const",
        "struct",
        "enum",
        "trait",
        "impl",
        "type",
        "use",
        "mod",
        "pub",
        "async",
        "await",
        "return",
        "x",
        "Vec",
        "1",
        "2.5",
        "'c'",
        "\"s {x} t\"",
        "\"{\"",
        "r\"raw\"",
        "_",
        "as",
        "where",
        "requires",
        "self",
        "Self",
        "|",
        "&",
        "&&",
        "take",
        "extern \"C\"",
        "unsafe",
        "cap",
        "region",
        "with",
        "effects",
        "mut",
        "0x",
        "\"unterminated",
    ];
    // Eight deterministic seeds, each input isolated behind its own watchdog.
    // A parser loop cannot stall the rest of the test suite indefinitely.
    for seed in [
        0x9E37_79B9_7F4A_7C15,
        0xD1B5_4A32_D192_ED03,
        0x94D0_49BB_1331_11EB,
        0x8538_EB7D_4F21_9A63,
        0xDA94_2042_E4DD_58B5,
        0xA24B_AED4_963E_E407,
        0x9FB2_1C65_1E98_DF25,
        0xC13F_A9A9_02A6_328F,
    ] {
        let mut rng = Rng(seed);
        for _ in 0..64 {
            let len = 1 + (rng.next() % 60) as usize;
            let src: Vec<&str> = (0..len)
                .map(|_| FRAGS[(rng.next() % FRAGS.len() as u64) as usize])
                .collect();
            parse_finishes_within(src.join(" "), Duration::from_millis(500));
        }
    }
}

#[test]
fn malformed_recovery_inputs_have_individual_watchdogs() {
    let cases = [
        "({)\nfn good() = 1\n",
        "fn t() = ({)\nfn good() = 1\n",
        "fn t() {\n( { -> )\n}\nfn g() = 1\n",
        "fn t() {\n[ { ] x struct fn impl\n}\nfn g() = 1\n",
        "fn t() {\n( if { ) ]\n}\nfn g() = 1\n",
        "impl T {\nfn t() = ({)\nfn good() = 1\n}\n",
        "struct S {\na: Vec<{N +}>\nb: i32\n}\n",
        "trait T {\nfn t(x: Vec<{N +}>)\nfn good()\n}\n",
        "mod m {\nfn t() = ({)\nfn good() = 1\n}\n",
        "enum E {\nA(Vec<{N +}>),\nB\n}\n",
    ];
    for src in cases {
        let (_, diagnostics) = parse_with_watchdog(src.to_string(), Duration::from_millis(750));
        assert!(
            diagnostics
                .iter()
                .any(|d| d.severity == pegasus_lexer::Severity::Error),
            "malformed seed unexpectedly produced no error: {src:?}"
        );
    }
}

#[test]
fn recovery_wakes_dormant_newline_after_skipped_generic_closers() {
    for src in [
        "struct S {\n    a: Vec<T,, >\n    b: i32\n}\n",
        "struct S {\n    a: Vec<Vec<T,, >>\n    b: i32\n}\n",
    ] {
        let (m, d) = parse_source(src.as_bytes());
        assert!(d
            .iter()
            .any(|d| d.severity == pegasus_lexer::Severity::Error));
        match &m.items[0].kind {
            ItemKind::Struct { fields, .. } => assert!(
                fields.iter().any(|field| field.name == "b"),
                "field following the skipped closer must survive recovery: {fields:#?}"
            ),
            kind => panic!("expected struct, found {kind:?}"),
        }
    }
}

#[test]
fn random_bytes_parse_without_panic() {
    let mut rng = Rng(42);
    for _ in 0..1_024 {
        let len = (rng.next() % 200) as usize;
        let bytes: Vec<u8> = (0..len).map(|_| (rng.next() % 128) as u8).collect();
        parse_bytes_finishes_within(bytes, Duration::from_millis(500));
    }
}

// ---------------------------------------------------------------- complexity

fn time_for(lines: usize) -> Duration {
    let unit =
        "struct S {\n    a: Vec<u8>\n}\nfn f(x: Map<K, V>) -> i32 = g<i32>(x) + a < b\nuse a.b.*\n";
    let src = unit.repeat(lines);
    let t = Instant::now();
    let (m, d) = parse_source(src.as_bytes());
    assert!(d.is_empty());
    assert_eq!(m.items.len(), 3 * lines);
    t.elapsed()
}

#[test]
fn parse_time_is_linear() {
    let _ = time_for(500); // warm-up
    let small = time_for(2_000);
    let big = time_for(16_000);
    // 8x the input: allow 24x the time before calling it superlinear.
    assert!(
        big < small * 24 + Duration::from_millis(50),
        "small {small:?}, big {big:?}"
    );
}

#[test]
fn many_errors_do_not_cascade_without_bound() {
    let src = "fn f( {\n".repeat(10_000);
    let (_, d) = parse_source(src.as_bytes());
    assert!(d.len() <= 100 + pegasus_lexer::lex(src.as_bytes()).diagnostics.len());
}

// ---------------------------------------------------------------- P1.1 findings

use pegasus_parser::ast::{GenericParam, VariantKind};

#[test]
fn rule_b_skips_matched_groups() {
    // `>` inside a brace argument is not the closer.
    let m = ok("fn t() = f<{N > 1}>(x)\n");
    assert_eq!(fn_shape(&m, "t").as_deref(), Some("(gcall f ({N > 1}) x)"));
    ok("fn t() = f<[u8; 4], (i32, i32), fn(A) -> B>(x)\n");
    ok("fn t() = f<{ g(a, b) }, T>(x)\n");
}

#[test]
fn rule_b_rejects_malformed_argument_lists() {
    // An empty argument is not well-formed generic_args, so `<` is a comparison
    // and the error is where the comparison's operand is missing: at the first
    // `,`. (Read as a generic call, the error would be at the second one.)
    for src in ["fn t() = f < , > (x)\n", "fn t() = f < a , , b > (x)\n"] {
        let first_comma = src.find(',').unwrap() as u32;
        let e = errors(src);
        assert_eq!(e[0], ("P001", first_comma), "{src}");
    }
    // A trailing comma is explicitly permitted by list(x, ",").
    ok("type A = Vec<T,>\n");
    ok("fn t() = parse<T,>(x)\n");
    ok("fn t() = parse<Vec<T,>,>(x)\n");
}

#[test]
fn where_clauses_and_supertraits_are_kept() {
    let m = ok("struct S<T> where T: A + B {\n    x: T\n}\nenum E<T> where T: C {\n    V\n}\ntrait Tr: Base + Other where Self: Sized {\n    fn f(self)\n}\nimpl<T> Tr for S<T> where T: A, (T, T): D {\n    fn f(self) = 1\n}\n");
    let n = |k: &ItemKind| match k {
        ItemKind::Struct { where_, .. }
        | ItemKind::Enum { where_, .. }
        | ItemKind::Impl { where_, .. } => where_.len(),
        ItemKind::Trait {
            where_,
            supertraits,
            ..
        } => where_.len() * 10 + supertraits.len(),
        _ => 99,
    };
    let got: Vec<usize> = m.items.iter().map(|i| n(&i.kind)).collect();
    assert_eq!(got, vec![1, 1, 12, 2]);
}

#[test]
fn where_predicates_may_start_with_any_type() {
    ok("fn f<T>(x: T) where T: A, (T, T): B, [T; 2]: C, *const T: D, fn(T): E {\n    x\n}\n");
}

#[test]
fn field_and_variant_docs_and_attributes_are_kept() {
    let m = ok("struct S {\n    /// the x\n    @deprecated\n    pub x: i32\n}\nenum E {\n    /// first\n    @a\n    A\n    B { /// y\n    y: i32 }\n}\n");
    match &m.items[0].kind {
        ItemKind::Struct { fields, .. } => {
            assert_eq!(fields[0].docs.len(), 1);
            assert_eq!(fields[0].attrs.len(), 1);
        }
        k => panic!("{k:?}"),
    }
    match &m.items[1].kind {
        ItemKind::Enum { variants, .. } => {
            assert_eq!((variants[0].docs.len(), variants[0].attrs.len()), (1, 1));
            match &variants[1].kind {
                VariantKind::Struct(f) => assert_eq!(f[0].docs.len(), 1),
                k => panic!("{k:?}"),
            }
        }
        k => panic!("{k:?}"),
    }
}

#[test]
fn dangling_doc_comments_are_p008() {
    assert_eq!(
        errors("struct S {\n    x: i32\n    /// dangling\n}\n")[0].0,
        "P008"
    );
    assert_eq!(
        errors("enum E {\n    A\n    /// dangling\n}\n")[0].0,
        "P008"
    );
}

#[test]
fn context_grammar_is_exact() {
    // trait_item has no `pub`; impl_item has; fn prefixes are part of fn_sig.
    assert_eq!(errors("trait T {\n    pub fn f(self)\n}\n")[0].0, "P001");
    ok("impl X {\n    pub fn f(self) = 1\n}\n");
    ok("trait T {\n    async fn f(self)\n    kernel fn g(self)\n}\n");
    // Rule E: the trait in `impl X for Y` must be a path_type.
    assert_eq!(errors("impl (A, B) for Y {\n}\n")[0].0, "P001");
    assert_eq!(errors("impl [A; 2] for Y {\n}\n")[0].0, "P001");
    ok("impl A.B<T> for Y {\n}\n");
}

#[test]
fn recovery_keeps_the_rest_of_the_function_and_file() {
    // Statement-level: a bad statement does not discard its function.
    let (m, d) =
        parse_source(b"fn f() {\n    let a = 1\n    let b = = 2\n    let c = 3\n}\nfn g() = 1\n");
    assert_eq!(
        d.iter()
            .filter(|d| d.severity == pegasus_lexer::Severity::Error)
            .count(),
        1
    );
    assert_eq!(m.items.len(), 2);
    assert_eq!(fn_stmts(&m, "f"), Some(3));
    // Expression-level inside a list: the rest of the call survives.
    let (m, d) = parse_source(b"fn f() = g(1, +, 3)\nfn h() = 2\n");
    assert_eq!(d.len(), 1);
    assert_eq!(m.items.len(), 2);
    // Item-level inside a body: a bad impl item does not discard the impl.
    let (m, d) = parse_source(
        b"impl X {\n    fn a(self) = 1\n    fn b(self) =\n    fn c(self) = 3\n}\nfn z() = 0\n",
    );
    assert!(!d.is_empty());
    assert_eq!(m.items.len(), 2);
    match &m.items[0].kind {
        ItemKind::Impl { items, .. } => assert!(items.len() >= 2),
        k => panic!("{k:?}"),
    }
    let _ = GenericParam::Const {
        name: String::new(),
        ty: pegasus_parser::ast::Type {
            span: (0, 0),
            text: String::new(),
            kind: pegasus_parser::ast::TypeKind::Tuple(vec![]),
        },
    };
}

#[test]
fn argument_recovery_keeps_the_other_arguments() {
    use pegasus_parser::ast::{ExprKind, FnBody};
    let (m, d) = parse_source(b"fn f() = g(1, +, 3)\n");
    assert_eq!(d.len(), 1);
    let ItemKind::Fn(f) = &m.items[0].kind else {
        panic!()
    };
    let Some(FnBody::Expr(e)) = &f.body else {
        panic!("{f:?}")
    };
    let ExprKind::Call { args, .. } = &e.kind else {
        panic!("{e:?}")
    };
    assert_eq!(args.len(), 3);
    assert!(matches!(args[1].value.kind, ExprKind::Error));
    assert!(matches!(args[2].value.kind, ExprKind::Literal(_)));
    // A failing element with trailing garbage: recovery skips to the next comma.
    let (m, d) = parse_source(b"fn f() = [1, 2 +* 3, 4]\n");
    assert_eq!(d.len(), 1);
    let ItemKind::Fn(f) = &m.items[0].kind else {
        panic!()
    };
    let Some(FnBody::Expr(e)) = &f.body else {
        panic!()
    };
    let ExprKind::Array(xs) = &e.kind else {
        panic!("{e:?}")
    };
    assert_eq!(xs.len(), 3);
    assert!(matches!(xs[1].kind, ExprKind::Error));
    let (m, _) = parse_source(b"fn f() = (1, 2 +* 3, 4)\n");
    let ItemKind::Fn(f) = &m.items[0].kind else {
        panic!()
    };
    let Some(FnBody::Expr(e)) = &f.body else {
        panic!()
    };
    let ExprKind::Tuple(xs) = &e.kind else {
        panic!("{e:?}")
    };
    assert_eq!(xs.len(), 3);
    // Same for tuple and array elements.
    let (m, d) = parse_source(b"fn f() = [1, +, 3]\n");
    assert_eq!(d.len(), 1);
    let ItemKind::Fn(f) = &m.items[0].kind else {
        panic!()
    };
    let Some(FnBody::Expr(e)) = &f.body else {
        panic!()
    };
    let ExprKind::Array(xs) = &e.kind else {
        panic!("{e:?}")
    };
    assert_eq!(xs.len(), 3);
    assert!(matches!(xs[1].kind, ExprKind::Error));
}

#[test]
fn recovery_does_not_cascade() {
    // The parameter fails at `]`; recovery stops at that same token, and the
    // closer check there must not report a second error for it (8, rule 4).
    let e = errors("fn f(] {\n}\nfn g() = 1\n");
    assert_eq!(e.len(), 1, "{e:?}");
    // Several independent errors are all still reported.
    let e = errors("fn f() {\n    a = ]\n    b = ]\n}\n");
    assert_eq!(e.len(), 2, "{e:?}");
}

// ---------------------------------------------------------------- P1.2 regressions

#[test]
fn generic_arguments_allow_the_grammar_trailing_comma() {
    let m = ok(
        "type A = Vec<i32,>\ntype B = Map<K, Vec<V,>,>\nfn t() = parse<T,>(x)\nfn u() = parse<Vec<T,>,>(x)\n",
    );
    assert_eq!(m.items.len(), 4);
}

#[test]
fn generic_arguments_still_reject_empty_slots() {
    for src in [
        "type A = Vec<>\n",
        "type A = Vec<, T>\n",
        "type A = Vec<T,, U>\n",
        "fn t() = parse<>(x)\n",
        "fn t() = parse<T,, U>(x)\n",
    ] {
        assert!(
            !errors(src).is_empty(),
            "malformed generic argument list was accepted:\n{src}"
        );
    }
}

#[test]
fn statement_recovery_skips_a_nested_const_argument_brace() {
    let src = "fn broken() {\n    let x: Vec<{N +}> = 1\n    let y = 2\n}\nfn good() = 3\n";
    let (m, d) = parse_source(src.as_bytes());
    assert!(d
        .iter()
        .any(|d| d.severity == pegasus_lexer::Severity::Error));
    assert_eq!(fn_stmts(&m, "broken"), Some(2));
    assert_eq!(fn_shape(&m, "good").as_deref(), Some("3"));
    match &m.items[0].kind {
        ItemKind::Fn(f) => match f.body.as_ref().unwrap() {
            FnBody::Block(b) => assert!(matches!(&b.stmts[0], Stmt::Error(_))),
            body => panic!("expected block body, found {body:?}"),
        },
        kind => panic!("expected broken function to be retained, found {kind:?}"),
    }
}

#[test]
fn item_recovery_skips_nested_const_argument_brace_in_trait_signature() {
    let src = "trait T {\n    fn broken(x: Vec<{N +}>)\n    fn good()\n}\n";
    let (m, d) = parse_source(src.as_bytes());
    assert!(d
        .iter()
        .any(|d| d.severity == pegasus_lexer::Severity::Error));
    match &m.items[0].kind {
        ItemKind::Trait { items, .. } => assert!(
            items.iter().any(|item| matches!(
                &item.kind,
                ItemKind::Fn(f) if f.sig.name == "good"
            )),
            "the valid sibling trait method must survive recovery: {items:#?}"
        ),
        kind => panic!("expected trait, found {kind:?}"),
    }
}

#[test]
fn p018_recovers_at_the_item_boundary() {
    let deep = format!("{}0{}", "(".repeat(140), ")".repeat(140));
    let src = format!("fn too_deep() = {deep}\nfn good() = 3\n");
    let (m, d) = parse_source(src.as_bytes());
    assert!(d.iter().any(|d| d.code == "P018"));
    assert_eq!(m.items.len(), 2);
    assert!(matches!(&m.items[0].kind, ItemKind::Error));
    assert!(matches!(&m.items[1].kind, ItemKind::Fn(_)));
    assert_eq!(fn_shape(&m, "good").as_deref(), Some("3"));
}

#[test]
fn p010_reports_both_the_opener_and_unexpected_closer() {
    let src = "fn t() = (1, 2]\n";
    let (_, d) = parse_source(src.as_bytes());
    let diag = d
        .iter()
        .find(|d| d.code == "P010")
        .expect("P010 diagnostic");
    let opener = src.find("(1").unwrap() as u32;
    let closer = src.find(']').unwrap() as u32;
    assert_eq!(diag.offset, closer);
    assert_eq!(diag.related, Some((opener, 1)));
}

#[test]
fn p018_inside_a_block_abandons_the_item_and_keeps_the_next_one() {
    let deep = format!("{}0{}", "(".repeat(140), ")".repeat(140));
    let src = format!("fn too_deep() {{\n    let x = {deep}\n}}\nfn good() = 3\n");
    let (m, d) = parse_source(src.as_bytes());
    assert!(d.iter().any(|d| d.code == "P018"));
    assert_eq!(m.items.len(), 2);
    assert!(matches!(&m.items[0].kind, ItemKind::Error));
    assert_eq!(fn_shape(&m, "good").as_deref(), Some("3"));
}

#[test]
fn p010_prefers_the_opener_matched_to_a_mistyped_closer() {
    let src = "fn t() = [(1, 2]\n";
    let (_, d) = parse_source(src.as_bytes());
    let diag = d
        .iter()
        .find(|d| d.code == "P010")
        .expect("P010 diagnostic");
    let opener = src.find('[').unwrap() as u32;
    let closer = src.find(']').unwrap() as u32;
    assert_eq!(diag.offset, closer);
    assert_eq!(diag.related, Some((opener, 1)));
}

// ---------------------------------------------------------------- P1.4 regressions

/// Names of the top-level functions that parsed as real `fn` items.
fn top_level_fn_names(src: &str) -> Vec<String> {
    let (m, _) = parse_source(src.as_bytes());
    m.items
        .iter()
        .filter_map(|item| match &item.kind {
            ItemKind::Fn(f) => Some(f.sig.name.clone()),
            _ => None,
        })
        .collect()
}

#[test]
fn stray_closer_in_a_block_keeps_the_function() {
    // A stray `]` or `)` makes the bracket stack pop the block's own `{`.
    // The block's `}` must still close the block (P1.3 lost the function).
    for stray in ["]", ")"] {
        let src = format!("fn f() {{\n    a = {stray}\n}}\nfn g() = 1\n");
        let e = errors(&src);
        assert_eq!(e.len(), 1, "{src:?}: {e:?}");
        assert_eq!(top_level_fn_names(&src), ["f", "g"], "{src:?}");
    }
    let src = "fn f() {\n    a = ]\n    b = ]\n    c = 1\n}\nfn g() = 1\n";
    assert_eq!(errors(src).len(), 2);
    assert_eq!(top_level_fn_names(src), ["f", "g"]);
}

#[test]
fn malformed_inputs_preserve_the_valid_sibling() {
    // Inputs whose braces are all closed: the following item must survive.
    for src in [
        "fn t() {\n( { -> )\n}\nfn g() = 1\n",
        "fn t() {\n[ { ] x struct fn impl\n}\nfn g() = 1\n",
        "fn t() {\n( if { ) ]\n}\nfn g() = 1\n",
        "fn t() {\n    a = ]\n}\nfn g() = 1\n",
        "fn t() {\n    a = )\n    b = ]\n}\nfn g() = 1\n",
    ] {
        assert!(
            top_level_fn_names(src).iter().any(|n| n == "g"),
            "valid sibling `g` was swallowed: {src:?}"
        );
    }
}

#[test]
fn unclosed_brace_consumes_the_rest_of_the_file_by_design() {
    // Documents P1.3's N2 observation rather than prescribing it: with a `{`
    // that is never closed, the lexer's bracket table gives no place to stop,
    // so later items are part of the unclosed group. Whether line-start item
    // starters should end an unclosed brace is a specification question.
    let src = "fn t() = ({)\nfn good() = 1\n";
    assert!(!errors(src).is_empty());
    assert!(!top_level_fn_names(src).iter().any(|n| n == "good"));
}
