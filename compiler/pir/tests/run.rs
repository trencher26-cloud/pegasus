//! The execution corpus (tests/run): each file is a one-file package whose
//! `main` must return the value in its `//@ result:` header (as rendered by
//! `interp::show`) or trap with a message containing its `//@ trap:` text.
//! Every accept case of the semantic corpus must also lower to verifier-clean
//! PIR and run without trapping.

use pegasus_pir::interp::{show, Interp, Limits};
use pegasus_sema::source::FsLoader;
use std::path::{Path, PathBuf};

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests")
}

/// Ok(rendered result) or Err(trap message); panics on compile errors.
fn run(dir: &Path, file: &str) -> Result<String, String> {
    run_traced(dir, file).0
}

/// Also returns the interpreter's drop trace.
fn run_traced(dir: &Path, file: &str) -> (Result<String, String>, Vec<String>) {
    let loader = FsLoader {
        root: dir.to_path_buf(),
    };
    let f2 = file.to_string();
    let (a, r) = pegasus_sema::check_package_with(&loader, file, move |s| {
        let c = pegasus_pir::compile(s).expect("package has errors");
        assert!(
            c.verify_errors.is_empty(),
            "{f2}: PIR verification failed: {:#?}",
            c.verify_errors
        );
        assert!(c.errors.is_empty(), "{f2}: lowering errors: {:?}", c.errors);
        let main = c.main.expect("no main");
        let mut it = Interp::new(&c.module, Limits::default());
        let r = match it.call(main, vec![]) {
            Ok(v) => Ok(show(&c.module, &c.module.funcs[main as usize].ret, &v)),
            Err(t) => Err(t.message),
        };
        (r, it.drops)
    });
    assert!(!a.has_errors(), "{file}: {}", a.render().join("\n"));
    r
}

#[test]
fn execution_corpus() {
    let dir = root().join("run");
    let n = check_dir(&dir);
    assert!(n >= 14);
}

/// tests/native: the native backend's corpus (tools/native_diff.py compares
/// native executables with the interpreter; this checks the interpreter
/// against each file's header).
#[test]
fn native_corpus_headers() {
    let n = check_dir(&root().join("native"));
    assert!(n >= 9);
}

fn check_dir(dir: &Path) -> usize {
    let dir = dir.to_path_buf();
    let mut files: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == "peg"))
        .collect();
    files.sort();
    let mut failures = Vec::new();
    for f in &files {
        let name = f.file_name().unwrap().to_str().unwrap();
        let src = std::fs::read_to_string(f).unwrap();
        let header = src.lines().next().unwrap_or("");
        let (got, drops) = run_traced(&dir, name);
        if let Some(want) = src
            .lines()
            .nth(1)
            .and_then(|l| l.strip_prefix("//@ drops: "))
        {
            let want: Vec<&str> = want.split(", ").collect();
            if drops != want {
                failures.push(format!("{name}: drops {drops:?}, expected {want:?}"));
            }
        }
        let ok = if let Some(want) = header.strip_prefix("//@ result: ") {
            got.as_deref() == Ok(want.trim())
        } else if let Some(want) = header.strip_prefix("//@ trap: ") {
            matches!(&got, Err(m) if m.contains(want.trim()))
        } else {
            false
        };
        if !ok {
            failures.push(format!("{name}: header `{header}`, got {got:?}"));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
    files.len()
}

#[test]
fn semantic_accept_corpus_runs() {
    let dir = root().join("sema/accept");
    for e in std::fs::read_dir(&dir).unwrap() {
        let p = e.unwrap().path();
        let name = p.file_name().unwrap().to_str().unwrap().to_string();
        if let Err(t) = run(&dir, &name) {
            panic!("{name}: trapped: {t}");
        }
    }
}
