//! Edge cases from grammar sections 2-3 that the corpus does not pin to a single token.

use pegasus_lexer::{filter, lex, Kind};

fn kinds(src: &str) -> Vec<(Kind, String)> {
    let l = lex(src.as_bytes());
    l.tokens
        .iter()
        .filter(|t| t.kind != Kind::Eof)
        .map(|t| {
            (
                t.kind,
                String::from_utf8_lossy(t.text(src.as_bytes())).into_owned(),
            )
        })
        .collect()
}

fn filtered(src: &str) -> String {
    let l = lex(src.as_bytes());
    filter(src.as_bytes(), &l.tokens)
        .into_iter()
        .map(|i| &l.tokens[i])
        .filter(|t| t.kind != Kind::Eof)
        .map(|t| {
            if t.kind == Kind::Newline {
                "/".to_string()
            } else {
                String::from_utf8_lossy(t.text(src.as_bytes())).into_owned()
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

#[test]
fn raw_identifier_takes_the_whole_word() {
    assert_eq!(
        kinds("r#selfish"),
        vec![(Kind::RawIdent, "r#selfish".into())]
    );
    for bad in ["r#self", "r#Self", "r#_"] {
        let l = lex(bad.as_bytes());
        assert_eq!(l.tokens[0].kind, Kind::Error, "{bad}");
        assert_eq!(
            l.tokens[0].end as usize,
            bad.len(),
            "{bad} must be one token"
        );
        assert_eq!(
            l.diagnostics.len(),
            1,
            "{bad} must give exactly one diagnostic"
        );
        assert_eq!(l.diagnostics[0].code, "L008");
    }
}

#[test]
fn r_hash_without_word_or_quote() {
    let l = lex(b"r#1");
    assert_eq!(l.tokens[0].kind, Kind::Ident); // r
    assert_eq!(l.diagnostics[0].code, "L009"); // # starts no token
}

#[test]
fn longest_match_punctuation_is_not_split_by_the_lexer() {
    let k = kinds("a>>=b");
    assert_eq!(k[1], (Kind::Punct, ">>=".into()));
    let k = kinds("x..=y");
    assert_eq!(k[1], (Kind::Punct, "..=".into()));
}

#[test]
fn numbers_after_dot_and_ranges() {
    assert_eq!(
        kinds("t.0.1").iter().filter(|k| k.0 == Kind::Int).count(),
        2
    );
    assert_eq!(kinds("1..2")[1], (Kind::Punct, "..".into()));
    assert_eq!(kinds("7f64")[0].0, Kind::Float);
    assert_eq!(kinds("0xFFu8")[0].0, Kind::Int);
    assert_eq!(lex(b"0x1f32").tokens[0].kind, Kind::Int); // hex digits, not a float suffix
}

#[test]
fn raw_string_line_endings_normalize() {
    let a = lex(b"r\"one\ntwo\"");
    let b = lex(b"r\"one\r\ntwo\"");
    let va = pegasus_lexer::golden::string_value(b"r\"one\ntwo\"", &a.tokens[0]);
    let vb = pegasus_lexer::golden::string_value(b"r\"one\r\ntwo\"", &b.tokens[0]);
    assert_eq!(va.as_deref(), Some("one\ntwo"));
    assert_eq!(va, vb);
}

#[test]
fn block_comment_with_line_end_is_one_newline() {
    let k = kinds("a /* x\n y */ b");
    assert_eq!(k.iter().filter(|t| t.0 == Kind::Newline).count(), 1);
    assert_eq!(
        kinds("a /* x */ b")
            .iter()
            .filter(|t| t.0 == Kind::Newline)
            .count(),
        0
    );
}

#[test]
fn doc_comment_owns_its_line_end() {
    let k = kinds("/// doc\nfn f() {}");
    assert_eq!(k[0].0, Kind::DocComment);
    assert_eq!(k[1], (Kind::Keyword, "fn".into()));
    assert_eq!(kinds("//// not doc\nx")[0].0, Kind::Newline);
}

#[test]
fn blank_line_does_not_end_a_header() {
    // Grammar section 3: `next` is the next non-NEWLINE token (changelog item 28).
    assert_eq!(
        filtered("fn f(x: i32) -> i32\n\n    requires x > 0\n{\n    x\n}"),
        "fn f ( x : i32 ) -> i32 requires x > 0 { x / }"
    );
}

#[test]
fn header_ends_at_equals_but_not_inside_angles() {
    // The header ended at `=`, so the newline before `{` survives. (The newline
    // after `{` is dropped by rule 2: `{` is a continuation token.)
    assert_eq!(filtered("fn f() = x\n{\n}"), "fn f ( ) = x / { }");
    // `=` inside `< >` does not end the header, so `where` on the next line still
    // attaches (rule 4). The line ends in `T`, so rule 2 cannot be what drops it.
    assert_eq!(
        filtered("fn f<T = i32>(x: T) -> T\n    where T: Copy\n{\n}"),
        "fn f < T = i32 > ( x : T ) -> T where T : Copy { }"
    );
    // Control: an `=` outside the angles ends the header, so the same `where` line
    // becomes a new statement.
    assert_eq!(
        filtered("type X = Y\n    where T: Copy"),
        "type X = Y / where T : Copy"
    );
}

#[test]
fn bracket_match_table_pairs_nested_groups() {
    let src = b"((a, [b]), {c})";
    let l = lex(src);
    let open = 0usize;
    let close = l.matches[open].unwrap() as usize;
    assert_eq!(l.tokens[close].text(src), b")");
    assert_eq!(close, l.tokens.len() - 2);
}

#[test]
fn nested_interpolation_is_rejected() {
    let l = lex(br#""a {f("b {c}")} d""#);
    assert!(l.diagnostics.iter().any(|d| d.code == "L012"));
}

#[test]
fn parenthesized_generic_default_does_not_start_params() {
    // Review finding: `(` inside `< >` must not set params_seen. With the bug, the
    // header never ended at `{`, so the NEWLINE before `{ y }` in the body was
    // dropped by rule 4 and `x { y }` became a struct literal.
    let src = "fn f<T = (i32, i32)>(x: T) -> T\n{\n    x\n    { y }\n}";
    assert_eq!(
        filtered(src),
        "fn f < T = ( i32 , i32 ) > ( x : T ) -> T { x / { y } / }"
    );
}

#[test]
fn parentheses_in_non_fn_headers_are_not_params() {
    // An `impl` header whose type is a fn type: its `(` is not a parameter list,
    // so angle tracking continues and the header still ends at `{`.
    let src = "impl<T> Show for fn(T) -> Vec<T>\n{\n    a\n    { b }\n}";
    assert_eq!(
        filtered(src),
        "impl < T > Show for fn ( T ) -> Vec < T > { a / { b } / }"
    );
}

#[test]
fn bidi_is_an_error_in_code_and_a_warning_in_comments_and_literals() {
    use pegasus_lexer::Severity;
    let code = lex("let a\u{202E} = 1".as_bytes());
    assert_eq!(code.errors().next().map(|d| d.code), Some("L003"));
    for src in [
        "\"a\u{202E}b\"",
        "r\"a\u{202E}b\"",
        "r#\"a\u{2066}b\"#",
        "// a \u{202D}",
        "/* a \u{202A} */",
        "/// a \u{2069}\nfn f() {}",
        "'\u{202E}'",
    ] {
        let l = lex(src.as_bytes());
        assert_eq!(l.errors().count(), 0, "{src:?} must stay valid");
        let w: Vec<_> = l.warnings().collect();
        assert_eq!(w.len(), 1, "{src:?}");
        assert_eq!((w[0].code, w[0].severity), ("W001", Severity::Warning));
    }
}

#[test]
fn identifier_keys_are_nfc_and_ignore_raw_prefix() {
    use pegasus_lexer::ident::key;
    let composed = "caf\u{e9}";
    let decomposed = "cafe\u{301}";
    assert_ne!(composed.as_bytes(), decomposed.as_bytes());
    assert_eq!(key(composed), key(decomposed));
    assert_eq!(key("r#with"), key("with"));
    // Spans and source spelling are untouched: both lex as one IDENT each.
    let src = format!("{composed} {decomposed}");
    let l = lex(src.as_bytes());
    assert_eq!(l.tokens[0].kind, Kind::Ident);
    assert_eq!(l.tokens[1].kind, Kind::Ident);
    assert_eq!(l.tokens[0].text(src.as_bytes()), composed.as_bytes());
    assert_eq!(l.tokens[1].text(src.as_bytes()), decomposed.as_bytes());
    assert_ne!(
        l.tokens[0].end - l.tokens[0].start,
        l.tokens[1].end - l.tokens[1].start
    );
}

#[test]
fn interpolation_holes_carry_their_own_bracket_matches() {
    // A closure inside a hole needs rule C's O(1) lookup, like top-level code.
    let src = br#""{xs.map((a, b) => a + b)}""#;
    let l = lex(src);
    let hole = &l.tokens[0].holes[0];
    assert_eq!(hole.matches.len(), hole.tokens.len());
    let open = hole
        .tokens
        .iter()
        .position(|t| t.text(src) == b"(")
        .unwrap(); // map(
    let params = open + 1; // (a, b)
    let close = hole.matches[params].unwrap() as usize;
    assert_eq!(hole.tokens[close].text(src), b")");
    assert_eq!(hole.tokens[close + 1].text(src), b"=>");
}

#[test]
fn filter_is_linear_in_blank_lines() {
    // Review finding: the NEWLINE lookahead rescanned every following blank line,
    // O(n^2) for n blank lines. 200,000 blank lines must take milliseconds.
    let mut src = String::from("fn f() {\n");
    src.push_str(&"\n".repeat(200_000));
    src.push_str("    x\n}\n");
    let l = lex(src.as_bytes());
    let t0 = std::time::Instant::now();
    let kept = filter(src.as_bytes(), &l.tokens);
    let elapsed = t0.elapsed();
    assert!(
        elapsed.as_secs_f64() < 1.0,
        "filter took {elapsed:?} for 200k blank lines"
    );
    assert_eq!(kept.len(), 10); // fn f ( ) { x / } / EOF
}

#[test]
fn compact_and_spaced_generic_close_assign_agree() {
    // `>=` is a closing angle plus `=`; the `=` must end the header either way.
    // The lines end in a plain name, so rule 2 cannot be what keeps or drops the
    // NEWLINE: only the header state decides.
    let compact = filtered("type A<T>=Box\nwhere T: Copy");
    let spaced = filtered("type A<T> = Box\nwhere T: Copy");
    assert_eq!(spaced, "type A < T > = Box / where T : Copy");
    assert_eq!(compact, "type A < T >= Box / where T : Copy");
    let compact2 = filtered("type A<B<T>>=X\nwhere T: Copy");
    assert_eq!(compact2, "type A < B < T >>= X / where T : Copy");
}

#[test]
fn malformed_literals_are_error_tokens() {
    for (src, code) in [
        (r#""bad \q escape""#, "L007"),
        (r#""lone } brace""#, "L013"),
        (r#"b"bad \q""#, "L007"),
        ("b\"caf\u{e9}\"", "L015"),
        ("r\"one\rtwo\"", "L002"),
        ("/// doc\rtext\nfn f() {}", "L002"),
    ] {
        let l = lex(src.as_bytes());
        assert_eq!(l.errors().next().map(|d| d.code), Some(code), "{src:?}");
        assert_eq!(
            l.tokens[0].kind,
            Kind::Error,
            "{src:?} must be one ERROR token"
        );
    }
    // The ERROR token still spans the whole literal, so recovery continues after it.
    let l = lex(br#""bad \q" x"#);
    assert_eq!(l.tokens[0].end, 8);
    assert_eq!(l.tokens[1].kind, Kind::Ident);
}

#[test]
fn mismatched_closer_recovery() {
    // Grammar 3 and 8: a closer pops down to the nearest opener of its own kind,
    // or, if none is open, pops the innermost opener (a mistyped closer).
    assert_eq!(
        filtered("fn f() {\n    let p = (1, 2]\n    next()\n}"),
        "fn f ( ) { let p = ( 1 , 2 ] / next ( ) / }"
    );
    // `}` closes the `{` and the unclosed `(` inside it.
    assert_eq!(
        filtered("fn f() {\n    g(1\n}\nx"),
        "fn f ( ) { g ( 1 } / x"
    );
    let src = b"{ ( }";
    let l = lex(src);
    assert_eq!(l.matches[0], Some(2)); // { pairs with }
    assert_eq!(l.matches[1], None); // ( stays unmatched
}

#[test]
fn error_inside_a_hole_stays_in_the_hole() {
    // `$` starts no token: the hole gets an ERROR token, the string stays valid.
    let src = br#""a {x $ y} b""#;
    let l = lex(src);
    assert_eq!(l.errors().next().map(|d| d.code), Some("L009"));
    assert_eq!(l.tokens[0].kind, Kind::Str);
    assert!(l.tokens[0].holes[0]
        .tokens
        .iter()
        .any(|t| t.kind == Kind::Error));
}

#[test]
fn mismatched_closers_stay_linear() {
    // 100k `(` then 100k `]`: no `[` is ever open, so every `]` takes the
    // "pop the innermost opener" path. Finding the nearest same-kind opener must
    // not scan the stack, or this is O(n^2).
    let src = format!("{}{}", "(".repeat(100_000), "]".repeat(100_000));
    let t0 = std::time::Instant::now();
    let l = lex(src.as_bytes());
    let kept = filter(src.as_bytes(), &l.tokens);
    let elapsed = t0.elapsed();
    assert!(elapsed.as_secs_f64() < 1.0, "lex + filter took {elapsed:?}");
    assert_eq!(kept.len(), 200_001);
    assert!(l.matches.iter().all(|m| m.is_none()));
}
