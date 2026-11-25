//! pgcorpus: runs the grammar conformance corpus against the Pegasus lexer.
//!
//!   pgcorpus verify   check every case (default); compare with candidate goldens if present
//!   pgcorpus bless    write unfrozen candidates after strict review-manifest validation
//!
//! Checks performed (lexer, newline filter and parser):
//!   1. Rejected cases with an L-code: the first lexical error has exactly that
//!      code at exactly that byte offset.
//!   2. Accepted cases and P-code cases: the lexer reports no errors.
//!   3. Warnings: the lexer's warnings equal the case's `warning:` lines exactly
//!      (code and offset); a case without `warning:` lines must produce none.
//!   4. `value:` cases: the decoded literal equals the expected value.
//!   5. P-code cases whose error is at a line end: that NEWLINE survives the filter.
//!   6. `nl-kept:` / `nl-dropped:` lines: that NEWLINE does / does not survive the
//!      newline filter. These are hand-written filter assertions, independent of
//!      the candidate goldens.
//!   7. Candidate goldens, if present, match the current output byte for byte.
//!   8. Accepted cases: the parser reports no errors.
//!   9. Rejected cases with a P-code: the first error (lexer and parser merged by
//!      offset, lexer first on ties) has exactly that code at exactly that offset.
//!  10. `shape:` cases: the body of `fn t` renders to exactly that S-expression.
//!  11. `stmts:` cases: the block body of `fn t` has exactly that many statements.

use pegasus_lexer::{filter, golden, lex};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

struct Case {
    path: PathBuf,
    id: String,
    expect: String,
    error: Option<(String, u32, String)>,
    value: Option<(String, String)>,
    warnings: Vec<(String, u32)>,
    nl_kept: Vec<u32>,
    nl_dropped: Vec<u32>,
    shape: Option<String>,
    stmts: Option<usize>,
    src: Vec<u8>,
}

fn unescape(s: &str) -> Vec<u8> {
    let mut out = Vec::new();
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\\' {
            let mut buf = [0u8; 4];
            out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
            continue;
        }
        match chars.next() {
            Some('r') => out.push(b'\r'),
            Some('n') => out.push(b'\n'),
            Some('t') => out.push(b'\t'),
            Some('x') => {
                let h: String = chars.by_ref().take(2).collect();
                out.push(u8::from_str_radix(&h, 16).unwrap());
            }
            Some('u') => {
                chars.next();
                let h: String = chars.by_ref().take_while(|&c| c != '}').collect();
                let ch = char::from_u32(u32::from_str_radix(&h, 16).unwrap()).unwrap();
                let mut buf = [0u8; 4];
                out.extend_from_slice(ch.encode_utf8(&mut buf).as_bytes());
            }
            Some(c) => out.push(c as u8),
            None => {}
        }
    }
    out
}

fn parse_case(path: &Path) -> Case {
    let src = fs::read(path).unwrap();
    let body = if src.starts_with(b"\xEF\xBB\xBF") {
        &src[3..]
    } else {
        &src[..]
    };
    let mut case = Case {
        path: path.to_path_buf(),
        id: String::new(),
        expect: String::new(),
        error: None,
        value: None,
        warnings: Vec::new(),
        nl_kept: Vec::new(),
        nl_dropped: Vec::new(),
        shape: None,
        stmts: None,
        src: src.clone(),
    };
    for line in body.split(|&b| b == b'\n') {
        let Some(rest) = line.strip_prefix(b"//@ ") else {
            break;
        };
        let rest = String::from_utf8_lossy(rest)
            .trim_end_matches('\r')
            .to_string();
        let Some((key, val)) = rest.split_once(": ") else {
            continue;
        };
        match key {
            "id" => case.id = val.to_string(),
            "expect" => case.expect = val.to_string(),
            "error" => {
                // CODE offset=N near="..."
                let code = val[..4].to_string();
                let off = val
                    .split("offset=")
                    .nth(1)
                    .and_then(|r| r.split(' ').next())
                    .unwrap();
                let near = val.split_once("near=\"").map(|(_, r)| r).unwrap_or("");
                let near = near.rsplit_once('"').map(|(n, _)| n).unwrap_or(near);
                case.error = Some((code, off.parse().unwrap(), near.to_string()));
            }
            "nl-kept" | "nl-dropped" => {
                let off: u32 = val
                    .strip_prefix("offset=")
                    .and_then(|r| r.split(' ').next())
                    .unwrap()
                    .parse()
                    .unwrap();
                if key == "nl-kept" {
                    case.nl_kept.push(off);
                } else {
                    case.nl_dropped.push(off);
                }
            }
            "warning" => {
                let code = val[..4].to_string();
                let off = val
                    .split("offset=")
                    .nth(1)
                    .and_then(|r| r.split(' ').next())
                    .unwrap();
                case.warnings.push((code, off.parse().unwrap()));
            }
            "shape" => case.shape = Some(val.trim().to_string()),
            "stmts" => case.stmts = Some(val.trim().parse().unwrap()),
            "value" => {
                let (name, v) = val.split_once(" = ").unwrap();
                let v = v.trim().trim_start_matches('"').trim_end_matches('"');
                case.value = Some((name.to_string(), v.to_string()));
            }
            _ => {}
        }
    }
    case
}

fn collect(dir: &Path, out: &mut Vec<PathBuf>) {
    let mut entries: Vec<_> = fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    entries.sort();
    for p in entries {
        if p.is_dir() {
            collect(&p, out);
        } else if p.extension().is_some_and(|e| e == "peg") {
            out.push(p);
        }
    }
}

/// Ask the strict Python/TOML review tool for the frozen file inventory.
///
/// `tools/goldens.py frozen-files` validates the complete manifest, the exact
/// source-case inventory, and every frozen source/golden hash before emitting
/// any names. This deliberately fails closed: a missing interpreter, malformed
/// TOML, invalid hash, modified source, or changed golden prevents `bless` from
/// writing anything.
fn frozen_goldens(root: &Path) -> Result<std::collections::HashSet<String>, String> {
    let script = root.join("tools/goldens.py");
    let configured = std::env::var_os("PEGASUS_PYTHON");
    let explicit_interpreter = configured.is_some();
    let interpreters: Vec<std::ffi::OsString> = configured
        .map(|value| vec![value])
        .unwrap_or_else(|| vec!["python3".into(), "python".into()]);

    let mut last_not_found = None;
    for interpreter in interpreters {
        let version = match Command::new(&interpreter)
            .arg("-c")
            .arg("import sys; sys.exit(0 if sys.version_info >= (3, 11) else 1)")
            .current_dir(root)
            .output()
        {
            Ok(output) => output,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound && !explicit_interpreter => {
                last_not_found = Some(format!("{}: {error}", interpreter.to_string_lossy()));
                continue;
            }
            Err(error) => {
                return Err(format!(
                    "could not execute Python interpreter {}: {error}",
                    interpreter.to_string_lossy()
                ));
            }
        };
        if !version.status.success() {
            if explicit_interpreter {
                return Err("PEGASUS_PYTHON must point to Python 3.11 or newer".into());
            }
            last_not_found = Some(format!(
                "{} is not Python 3.11+",
                interpreter.to_string_lossy()
            ));
            continue;
        }

        let output = match Command::new(&interpreter)
            .arg(&script)
            .arg("frozen-files")
            .current_dir(root)
            .output()
        {
            Ok(output) => output,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                last_not_found = Some(format!("{}: {error}", interpreter.to_string_lossy()));
                continue;
            }
            Err(error) => {
                return Err(format!(
                    "could not execute review-manifest validator {}: {error}",
                    interpreter.to_string_lossy()
                ));
            }
        };

        if !output.status.success() {
            return Err(format!(
                "review-manifest validation failed ({}): {}",
                output.status,
                String::from_utf8_lossy(&output.stderr).trim()
            ));
        }

        let text = std::str::from_utf8(&output.stdout).map_err(|error| {
            format!("review-manifest validator emitted non-UTF-8 output: {error}")
        })?;
        let mut names = std::collections::HashSet::new();
        for line in text.lines() {
            if line.is_empty()
                || line.contains('/')
                || line.contains('\\')
                || !(line.ends_with(".tokens") || line.ends_with(".filtered"))
                || std::path::Path::new(line)
                    .file_name()
                    .and_then(|name| name.to_str())
                    != Some(line)
            {
                return Err(format!(
                    "review-manifest validator emitted an unsafe filename: {line:?}"
                ));
            }
            if !names.insert(line.to_string()) {
                return Err(format!(
                    "review-manifest validator emitted duplicate filename: {line}"
                ));
            }
        }
        return Ok(names);
    }

    Err(format!(
        "cannot validate frozen goldens: Python 3.11+ was not found (set PEGASUS_PYTHON); {}",
        last_not_found.unwrap_or_else(|| "no interpreter candidates tried".into())
    ))
}

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap()
}

fn main() -> ExitCode {
    let cmd = std::env::args().nth(1).unwrap_or_else(|| "verify".into());
    let root = root();
    let cand_dir = root.join("tests/grammar/goldens/candidates");
    let mut paths = Vec::new();
    collect(&root.join("tests/grammar/cases"), &mut paths);
    let cases: Vec<Case> = paths.iter().map(|p| parse_case(p)).collect();

    if cmd == "bless" {
        let frozen = match frozen_goldens(&root) {
            Ok(names) => names,
            Err(error) => {
                eprintln!("FAIL: pgcorpus bless refused to write candidate goldens: {error}");
                return ExitCode::FAILURE;
            }
        };
        if let Err(error) = fs::create_dir_all(&cand_dir) {
            eprintln!("FAIL: cannot create candidate golden directory: {error}");
            return ExitCode::FAILURE;
        }
        let mut skipped = 0;
        let mut frozen_differs = Vec::new();
        for c in &cases {
            let lexed = lex(&c.src);
            let kept = filter(&c.src, &lexed.tokens);
            let outputs = [
                (
                    format!("{}.tokens", c.id),
                    golden::render(&c.src, &lexed.tokens, None),
                ),
                (
                    format!("{}.filtered", c.id),
                    golden::render(&c.src, &lexed.tokens, Some(&kept)),
                ),
            ];
            for (name, text) in outputs {
                let path = cand_dir.join(&name);
                if frozen.contains(&name) {
                    // Frozen goldens are never overwritten; a change needs
                    // `tools/goldens.py unfreeze` and a new review.
                    skipped += 1;
                    if fs::read_to_string(&path).ok().as_deref() != Some(text.as_str()) {
                        frozen_differs.push(name);
                    }
                } else {
                    fs::write(&path, text).unwrap();
                }
            }
        }
        println!("skipped {skipped} frozen golden file(s)");
        if !frozen_differs.is_empty() {
            println!(
                "current output differs from {} frozen golden(s), left unchanged:",
                frozen_differs.len()
            );
            for n in &frozen_differs {
                println!("  {n}");
            }
        }
        fs::write(
            cand_dir.join("PROVENANCE.toml"),
            format!(
                "# CANDIDATE goldens: produced by the hand-written lexer, NOT by the independent\n\
                 # reference implementation, and NOT yet reviewed. They detect regressions in\n\
                 # this lexer; they are not authoritative. See tests/grammar/README.md.\n\
                 status          = \"candidate-unreviewed\"\n\
                 generator       = \"pegasus-lexer {} (hand-written Rust)\"\n\
                 grammar_version = \"0.1.1-draft\"\n\
                 cases           = {}\n",
                env!("CARGO_PKG_VERSION"),
                cases.len()
            ),
        )
        .unwrap();
        println!(
            "wrote candidate goldens for {} cases to {}",
            cases.len(),
            cand_dir.display()
        );
        if !frozen_differs.is_empty() {
            println!(
                "FAIL: frozen goldens no longer match the lexer; fix the lexer, or unfreeze \
                 the group (tools/goldens.py) and re-review"
            );
            return ExitCode::FAILURE;
        }
        return ExitCode::SUCCESS;
    }

    let mut failures: Vec<String> = Vec::new();
    let mut checked = [0usize; 11];
    for c in &cases {
        let lexed = lex(&c.src);
        let kept = filter(&c.src, &lexed.tokens);
        let first = lexed.errors().next();
        let rel = c.path.strip_prefix(&root).unwrap().display().to_string();
        match (&c.expect[..], &c.error) {
            ("reject", Some((code, off, _))) if code.starts_with('L') => {
                checked[0] += 1;
                match first {
                    Some(d) if d.code == code && d.offset == *off => {}
                    Some(d) => failures.push(format!(
                        "{rel}: expected {code} at {off}, lexer reported {} at {}",
                        d.code, d.offset
                    )),
                    None => failures.push(format!(
                        "{rel}: expected {code} at {off}, lexer reported nothing"
                    )),
                }
            }
            _ => {
                checked[1] += 1;
                if let Some(d) = first {
                    failures.push(format!(
                        "{rel}: expected no lexical error, got {} at {} ({} total)",
                        d.code,
                        d.offset,
                        lexed.errors().count()
                    ));
                }
            }
        }
        // Hand-written newline-filter assertions.
        let is_newline_at = |i: usize, off: u32| {
            let t = &lexed.tokens[i];
            t.kind == pegasus_lexer::Kind::Newline && t.start == off
        };
        for (&off, want_kept) in c
            .nl_kept
            .iter()
            .map(|o| (o, true))
            .chain(c.nl_dropped.iter().map(|o| (o, false)))
        {
            checked[6] += 1;
            let lexed_has = (0..lexed.tokens.len()).any(|i| is_newline_at(i, off));
            let kept_has = kept.iter().any(|&i| is_newline_at(i, off));
            if !lexed_has {
                failures.push(format!("{rel}: no NEWLINE token starts at {off}"));
            } else if kept_has != want_kept {
                let what = if want_kept {
                    "survive"
                } else {
                    "be dropped by"
                };
                failures.push(format!(
                    "{rel}: the NEWLINE at {off} must {what} the filter"
                ));
            }
        }
        // Warnings must match exactly, in both directions.
        let got: Vec<(String, u32)> = lexed
            .warnings()
            .map(|w| (w.code.to_string(), w.offset))
            .collect();
        if !got.is_empty() || !c.warnings.is_empty() {
            checked[5] += 1;
        }
        if got != c.warnings {
            failures.push(format!(
                "{rel}: warnings: expected {:?}, lexer reported {:?}",
                c.warnings, got
            ));
        }
        if let Some((name, want)) = &c.value {
            checked[2] += 1;
            let toks = &lexed.tokens;
            let pos = toks.iter().position(|t| {
                t.kind == pegasus_lexer::Kind::Ident && t.text(&c.src) == name.as_bytes()
            });
            let lit = pos.and_then(|p| {
                toks[p..].iter().find(|t| {
                    matches!(
                        t.kind,
                        pegasus_lexer::Kind::Str | pegasus_lexer::Kind::RawStr
                    )
                })
            });
            let got = lit.and_then(|t| golden::string_value(&c.src, t));
            let want = String::from_utf8(unescape(want)).unwrap();
            if got.as_deref() != Some(want.as_str()) {
                failures.push(format!(
                    "{rel}: value of {name}: expected {want:?}, got {got:?}"
                ));
            }
        }
        if let Some((code, off, near)) = &c.error {
            if code.starts_with('P') && near.starts_with("\\n") {
                checked[3] += 1;
                let survives = kept.iter().any(|&i| {
                    let t = &lexed.tokens[i];
                    t.kind == pegasus_lexer::Kind::Newline && t.start == *off
                });
                if !survives {
                    failures.push(format!(
                        "{rel}: the NEWLINE at {off} must survive the filter (error {code} is reported there)"
                    ));
                }
            }
        }
        // Parser stage.
        let (module, pdiags) = pegasus_parser::parse(&c.src, &lexed, &kept);
        let first_any = lexed.errors().chain(pdiags.iter()).min_by_key(|d| d.offset); // min_by_key keeps the first (lexer) on ties
        match (&c.expect[..], &c.error) {
            ("accept", _) => {
                checked[7] += 1;
                if let Some(d) = pdiags.first() {
                    failures.push(format!(
                        "{rel}: expected no parse error, got {} at {} ({} total)",
                        d.code,
                        d.offset,
                        pdiags.len()
                    ));
                }
            }
            ("reject", Some((code, off, _))) if code.starts_with('P') => {
                checked[8] += 1;
                match first_any {
                    Some(d) if d.code == code && d.offset == *off => {}
                    Some(d) => failures.push(format!(
                        "{rel}: expected {code} at {off}, first error is {} at {}",
                        d.code, d.offset
                    )),
                    None => failures.push(format!(
                        "{rel}: expected {code} at {off}, no error reported"
                    )),
                }
            }
            _ => {}
        }
        if let Some(want) = &c.shape {
            checked[9] += 1;
            let got = pegasus_parser::fn_shape(&module, "t");
            if got.as_deref() != Some(want.as_str()) {
                failures.push(format!("{rel}: shape: expected {want}, got {got:?}"));
            }
        }
        if let Some(want) = c.stmts {
            checked[10] += 1;
            let got = pegasus_parser::fn_stmts(&module, "t");
            if got != Some(want) {
                failures.push(format!("{rel}: stmts: expected {want}, got {got:?}"));
            }
        }
        for (ext, text) in [
            ("tokens", golden::render(&c.src, &lexed.tokens, None)),
            (
                "filtered",
                golden::render(&c.src, &lexed.tokens, Some(&kept)),
            ),
        ] {
            let p = cand_dir.join(format!("{}.{ext}", c.id));
            if let Ok(old) = fs::read_to_string(&p) {
                checked[4] += 1;
                if old != text {
                    failures.push(format!(
                        "{rel}: output differs from candidate golden {} (run `pgcorpus bless` only after review)",
                        p.strip_prefix(&root).unwrap().display()
                    ));
                }
            }
        }
    }
    println!(
        concat!(
            "pgcorpus: {} cases | lexical-error checks {} | no-error checks {} | ",
            "warning checks {} | value checks {} | filter checks {} | golden comparisons {}\n",
            "  parser: no-error checks {} | P-error checks {} | shape checks {} | stmts checks {}"
        ),
        cases.len(),
        checked[0],
        checked[1],
        checked[5],
        checked[2],
        checked[3] + checked[6],
        checked[4],
        checked[7],
        checked[8],
        checked[9],
        checked[10]
    );
    if failures.is_empty() {
        println!("all checks passed");
        ExitCode::SUCCESS
    } else {
        for f in &failures {
            println!("FAIL {f}");
        }
        println!("{} failure(s)", failures.len());
        ExitCode::FAILURE
    }
}
