//! Regression tests for review findings on S0.1 (fixed in S0.2).

use pegasus_sema::hir::{ExprKind, Stmt};
use pegasus_sema::source::MemLoader;
use std::time::{Duration, Instant};

fn codes(src: &str) -> Vec<&'static str> {
    pegasus_sema::check_source(src)
        .diags
        .iter()
        .filter(|d| d.severity == pegasus_sema::diag::Severity::Error)
        .map(|d| d.code)
        .collect()
}

/// Finding 1: a missing pattern deeper than the old depth limit (64) must be
/// reported, not accepted.
#[test]
fn deep_missing_pattern_is_reported() {
    let n = 80;
    let ty = vec!["bool"; n].join(", ");
    let all_true = vec!["true"; n].join(", ");
    let src = format!("fn f(t: ({ty})) -> i32 = match t {{\n    ({all_true}) => 1,\n}}\n");
    assert_eq!(codes(&src), ["T082"]);
}

/// Finding 1: when the analysis cannot finish, it reports T083; it never
/// treats the match as exhaustive.
#[test]
fn inconclusive_analysis_is_an_error() {
    let n = 1100;
    let ty = vec!["bool"; n].join(", ");
    let all_true = vec!["true"; n].join(", ");
    let src = format!("fn f(t: ({ty})) -> i32 = match t {{\n    ({all_true}) => 1,\n}}\n");
    assert_eq!(codes(&src), ["T083"]);
    // With a catch-all arm the match is fine.
    let src =
        format!("fn f(t: ({ty})) -> i32 = match t {{\n    ({all_true}) => 1,\n    _ => 0,\n}}\n");
    assert!(codes(&src).is_empty());
}

/// Finding 2: huge fixed-size arrays are analysed without expanding them.
#[test]
fn huge_arrays_are_cheap() {
    let start = Instant::now();
    let src = "fn f(x: [u8; 1152921504606846976]) -> u8 = match x {\n    [a, .., b] => a,\n}\n\
               fn g(x: [u8; 1152921504606846976]) -> u8 = match x {\n    [0, ..] => 1,\n}\n\
               fn h(x: [bool; 1000000000]) -> u8 = match x {\n    [true, .., false] => 1,\n    [false, ..] => 2,\n    [_, .., true] => 3,\n}\n";
    let a = pegasus_sema::check_source(src);
    let errs: Vec<_> = a
        .diags
        .iter()
        .map(|d| (d.code, d.message.clone()))
        .collect();
    assert_eq!(errs.len(), 1, "{errs:?}");
    assert_eq!(errs[0].0, "T082");
    assert!(errs[0].1.contains("[_, ..]"), "{errs:?}");
    assert!(
        start.elapsed() < Duration::from_secs(2),
        "took {:?}",
        start.elapsed()
    );
}

/// Array exhaustiveness: exact-length patterns, prefix/suffix coverage.
#[test]
fn array_exhaustiveness() {
    let ok = "fn f(x: [bool; 2]) -> u8 = match x {\n    [true, true] => 0,\n    [true, false] => 1,\n    [false, ..] => 2,\n}\n";
    assert!(codes(ok).is_empty());
    let missing =
        "fn f(x: [bool; 3]) -> u8 = match x {\n    [true, ..] => 0,\n    [.., true] => 1,\n}\n";
    let a = pegasus_sema::check_source(missing);
    assert_eq!(a.diags.len(), 1);
    assert!(
        a.diags[0].message.contains("[false, .., false]"),
        "{}",
        a.diags[0].message
    );
}

/// Finding 5: `fn f() = e` and `fn f() { e }` give the same typed HIR.
#[test]
fn expression_body_is_block_sugar() {
    let src = "fn g() -> i32 = 1\nfn a() = g()\nfn b() { g() }\n";
    let loader = MemLoader::new(&[("main.peg", src)]);
    let (an, shapes) = pegasus_sema::check_package_with(&loader, "main.peg", |s| {
        let mut out = Vec::new();
        for (d, body) in &s.bodies {
            let name = s.def(*d).name.clone();
            if name == "a" || name == "b" {
                let ExprKind::Block(b) = &body.value.kind else {
                    panic!("{name}: root is not a block")
                };
                let stmt_is_call = matches!(&b.stmts[..], [Stmt::Expr(e)] if matches!(e.kind, ExprKind::Call { .. }));
                out.push((
                    name,
                    body.value.ty.is_unit(),
                    stmt_is_call,
                    b.tail.is_none(),
                ));
            }
        }
        out.sort();
        out
    });
    assert!(!an.has_errors());
    assert_eq!(
        shapes,
        vec![
            ("a".to_string(), true, true, true),
            ("b".to_string(), true, true, true)
        ]
    );
}
