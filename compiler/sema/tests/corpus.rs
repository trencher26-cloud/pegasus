//! The sema conformance corpus (tests/sema). Each `.peg` file under
//! `accept/` and `reject/` is a one-file package; each directory under
//! `pkg/` is a package rooted at its `main.peg`. A comment
//! `// error: CODE[, CODE]` marks a line that must produce exactly those
//! error codes; every other line must produce none. Syntax errors are never
//! expected in this corpus.

use pegasus_sema::source::FsLoader;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

fn corpus_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/sema")
}

fn expected(path: &str, src: &str) -> BTreeSet<(String, u32, String)> {
    let mut out = BTreeSet::new();
    for (i, line) in src.lines().enumerate() {
        if let Some(pos) = line.find("// error:") {
            for code in line[pos + 9..].split(',') {
                out.insert((path.to_string(), i as u32 + 1, code.trim().to_string()));
            }
        }
    }
    out
}

fn run_package(dir: &Path, root: &str) -> Result<(), String> {
    let loader = FsLoader {
        root: dir.to_path_buf(),
    };
    let a = pegasus_sema::check_package(&loader, root);
    let mut want = BTreeSet::new();
    for f in a.files.iter().skip(1) {
        want.extend(expected(&f.path, &String::from_utf8_lossy(&f.src)));
    }
    if !a.syntax.is_empty() {
        return Err(format!(
            "unexpected syntax errors:\n{}",
            a.render().join("\n")
        ));
    }
    let mut got = BTreeSet::new();
    for d in &a.diags {
        if d.severity != pegasus_sema::diag::Severity::Error {
            continue;
        }
        let f = &a.files[d.file as usize];
        got.insert((f.path.clone(), f.line_col(d.offset).0, d.code.to_string()));
    }
    if want == got {
        return Ok(());
    }
    let missing: Vec<_> = want.difference(&got).collect();
    let extra: Vec<_> = got.difference(&want).collect();
    Err(format!(
        "missing: {missing:?}\nunexpected: {extra:?}\nall diagnostics:\n{}",
        a.render().join("\n")
    ))
}

#[test]
fn sema_corpus() {
    let root = corpus_root();
    let mut failures = Vec::new();
    let mut n = 0;
    for group in ["accept", "reject"] {
        let mut files: Vec<_> = std::fs::read_dir(root.join(group))
            .unwrap()
            .map(|e| e.unwrap().path())
            .collect();
        files.sort();
        for f in files {
            if f.extension().is_some_and(|e| e == "peg") {
                n += 1;
                let name = f.file_name().unwrap().to_str().unwrap().to_string();
                if let Err(e) = run_package(f.parent().unwrap(), &name) {
                    failures.push(format!("== {group}/{name}\n{e}"));
                }
                if group == "accept" {
                    // Accept cases must not carry error annotations.
                    let src = std::fs::read_to_string(&f).unwrap();
                    assert!(
                        !src.contains("// error:"),
                        "{name}: accept case with expected errors"
                    );
                }
            }
        }
    }
    let mut pkgs: Vec<_> = std::fs::read_dir(root.join("pkg"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    pkgs.sort();
    for p in pkgs {
        n += 1;
        if let Err(e) = run_package(&p, "main.peg") {
            failures.push(format!(
                "== pkg/{}\n{e}",
                p.file_name().unwrap().to_str().unwrap()
            ));
        }
    }
    assert!(n >= 10, "corpus too small: {n}");
    assert!(
        failures.is_empty(),
        "{} of {n} corpus case(s) failed:\n\n{}",
        failures.len(),
        failures.join("\n\n")
    );
}

/// Every diagnostic code the compiler can emit is catalogued in the spec.
#[test]
fn every_code_is_catalogued() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let spec = std::fs::read_to_string(root.join("../../spec/semantics.md")).unwrap()
        + &std::fs::read_to_string(root.join("../../spec/purm.md")).unwrap();
    let mut missing = Vec::new();
    for e in std::fs::read_dir(root.join("src")).unwrap() {
        let src = std::fs::read_to_string(e.unwrap().path()).unwrap();
        let b = src.as_bytes();
        for i in 0..b.len().saturating_sub(5) {
            if b[i] == b'"'
                && matches!(b[i + 1], b'R' | b'T' | b'U' | b'W' | b'O')
                && b[i + 2..i + 5].iter().all(u8::is_ascii_digit)
                && b[i + 5] == b'"'
            {
                let code = &src[i + 1..i + 5];
                if !spec.contains(&format!("| {code} |")) && !missing.contains(&code.to_string()) {
                    missing.push(code.to_string());
                }
            }
        }
    }
    assert!(
        missing.is_empty(),
        "codes missing from spec/semantics.md and spec/purm.md: {missing:?}"
    );
}
