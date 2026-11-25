//! `pegasus` — the Pegasus compiler driver.
//!
//!     pegasus check <file.peg>   lex, parse, resolve and type-check a package
//!     pegasus hir <file.peg>     also print the typed HIR of every function
//!     pegasus pir <file.peg>     lower to PIR, verify it, and print it
//!     pegasus run <file.peg>     lower to PIR and run `main` in the reference interpreter
//!     pegasus pirb <file.peg> <out.pirb>
//!                                write the verified PIR as a PIRB file (spec/pir-bridge.md)
//!     pegasus pirb-verify <in.pirb> <out.pirb>
//!                                certify PIRB written outside the frontend (unverified, e.g.
//!                                by `pegasus-mlir export`): run the PIR verifier, write it
//!                                marked verified
//!     pegasus build [--backend=direct|mlir] <file.peg> <out>
//!                                compile to a native executable (spec/native.md): PIRB ->
//!                                LLVM IR -> clang, linked with the runtime. The LLVM IR comes
//!                                from `pegasus-native` (the direct emitter, the default) or,
//!                                with `--backend=mlir`, from `pegasus-mlir lower` (the pg
//!                                dialect lowered through MLIR, spec/mlir.md §5)
//!
//! `build` finds its tools through PEGASUS_NATIVE (the pegasus-native
//! executable) or PEGASUS_MLIR (the pegasus-mlir executable), PEGASUS_RT (the
//! runtime: pegasus_rt.c or a built library) and PEGASUS_CLANG (default
//! `clang`).
//!
//! The package root is the directory of the given file; `mod x` loads
//! `x.peg` or `x/mod.peg` relative to it. Exit status: 0 on success; 1 if
//! the package has errors; 2 on usage errors; 101 if the program trapped;
//! 70 on an internal compiler error (PIR verification failure).

use pegasus_pir::interp::{self, Interp, Limits};
use pegasus_sema::source::FsLoader;
use std::path::Path;
use std::process::ExitCode;

fn usage() -> ExitCode {
    eprintln!("usage: pegasus (check | hir | pir | run) <file.peg>");
    eprintln!("       pegasus pirb <file.peg> <output>");
    eprintln!("       pegasus build [--backend=direct|mlir] <file.peg> <output>");
    eprintln!("       pegasus pirb-verify <in.pirb> <out.pirb>");
    ExitCode::from(2)
}

/// Where `pegasus build` gets its LLVM IR.
#[derive(Clone, Copy, PartialEq)]
enum Backend {
    /// `pegasus-native`: the direct LLVM IR emitter (spec/native.md).
    Direct,
    /// `pegasus-mlir lower`: pg lowered through MLIR (spec/mlir.md §5).
    Mlir,
}

/// `pegasus build`: PIRB -> LLVM IR (either backend) -> clang.
fn build(pirb: &[u8], out: &str, backend: Backend) -> ExitCode {
    let need = |var: &str| match std::env::var(var) {
        Ok(v) if !v.is_empty() => Ok(v),
        _ => Err(format!("error: set {var} for `pegasus build`")),
    };
    let tool_var = match backend {
        Backend::Direct => "PEGASUS_NATIVE",
        Backend::Mlir => "PEGASUS_MLIR",
    };
    let (native, rt) = match (need(tool_var), need("PEGASUS_RT")) {
        (Ok(n), Ok(r)) => (n, r),
        (Err(e), _) | (_, Err(e)) => {
            eprintln!("{e}");
            return ExitCode::from(2);
        }
    };
    let clang = std::env::var("PEGASUS_CLANG").unwrap_or_else(|_| "clang".into());
    let pirb_path = format!("{out}.pirb");
    let ll_path = format!("{out}.ll");
    if let Err(e) = std::fs::write(&pirb_path, pirb) {
        eprintln!("error: cannot write {pirb_path}: {e}");
        return ExitCode::from(2);
    }
    let run = |cmd: &mut std::process::Command| -> Result<(), ExitCode> {
        match cmd.status() {
            Ok(s) if s.success() => Ok(()),
            Ok(s) => Err(ExitCode::from(if s.code() == Some(1) { 1 } else { 70 })),
            Err(e) => {
                eprintln!("error: cannot run {:?}: {e}", cmd.get_program());
                Err(ExitCode::from(2))
            }
        }
    };
    let mut emit = std::process::Command::new(&native);
    if backend == Backend::Mlir {
        emit.arg("lower");
    }
    let r = run(emit.arg(&pirb_path).arg(&ll_path)).and_then(|()| {
        run(std::process::Command::new(&clang).args([
            "-O2",
            "-Wno-override-module",
            "-o",
            out,
            &ll_path,
            &rt,
        ]))
    });
    let _ = std::fs::remove_file(&pirb_path);
    let _ = std::fs::remove_file(&ll_path);
    match r {
        Ok(()) => ExitCode::SUCCESS,
        Err(c) => c,
    }
}

enum Outcome {
    Nothing,
    Text(String),
    Run(Result<String, String>),
    Ice(Vec<String>),
    Bytes(Vec<u8>),
    LowerErrors(Vec<String>),
    NoMain,
}

/// `pegasus pirb-verify <in.pirb> <out.pirb>`: certify PIRB written outside
/// the frontend (e.g. by `pegasus-mlir export`, which writes it unverified):
/// run the PIR verifier and write it marked verified (spec/pir-bridge.md
/// §2.3). Exit status: 0 ok, 1 the verifier rejects it, 2 usage or I/O,
/// 3 malformed PIRB.
fn pirb_verify(input: &str, output: &str) -> ExitCode {
    let bytes = match std::fs::read(input) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("error: cannot read {input}: {e}");
            return ExitCode::from(2);
        }
    };
    match pegasus_pir::bridge::certify(&bytes) {
        Ok(out) => match std::fs::write(output, out) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("error: cannot write {output}: {e}");
                ExitCode::from(2)
            }
        },
        Err(pegasus_pir::bridge::CertifyError::Malformed(m)) => {
            eprintln!("error: malformed PIRB: {m}");
            ExitCode::from(3)
        }
        Err(pegasus_pir::bridge::CertifyError::Invalid(errs)) => {
            for e in errs {
                eprintln!("error: PIR verifier: {e}");
            }
            ExitCode::from(1)
        }
    }
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if let [c, i, o] = args.as_slice() {
        if c == "pirb-verify" {
            return pirb_verify(i, o);
        }
    }
    // `build --backend=...`: the only option; it selects the IR emitter.
    let mut args = args;
    let mut backend = Backend::Direct;
    if args.len() == 4 && args[0] == "build" {
        if let Some(b) = args[1].strip_prefix("--backend=") {
            backend = match b {
                "direct" => Backend::Direct,
                "mlir" => Backend::Mlir,
                _ => {
                    eprintln!("error: unknown backend `{b}` (expected `direct` or `mlir`)");
                    return usage();
                }
            };
            args.remove(1);
        }
    }
    let (cmd, path, output) = match args.as_slice() {
        [c, p] if matches!(c.as_str(), "check" | "hir" | "pir" | "run") => {
            (c.clone(), p.clone(), None)
        }
        [c, p, o] if matches!(c.as_str(), "pirb" | "build") => {
            (c.clone(), p.clone(), Some(o.clone()))
        }
        _ => return usage(),
    };
    let p = Path::new(&path);
    let dir = p
        .parent()
        .filter(|d| !d.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let Some(name) = p.file_name().and_then(|n| n.to_str()) else {
        return usage();
    };
    let loader = FsLoader {
        root: dir.to_path_buf(),
    };
    let c = cmd.clone();
    let (analysis, outcome) =
        pegasus_sema::check_package_with(&loader, name, move |s| match c.as_str() {
            "hir" => {
                let mut ids: Vec<_> = s.bodies.keys().copied().collect();
                ids.sort();
                let mut out = String::new();
                for d in ids {
                    if s.def(d).file != 0 {
                        out.push_str(&format!(
                            "== {} {}\n{:#?}\n",
                            s.def(d).kind.describe(),
                            s.def(d).name,
                            s.bodies[&d]
                        ));
                    }
                }
                Outcome::Text(out)
            }
            "pir" | "run" | "pirb" | "build" => {
                let Some(compiled) = pegasus_pir::compile(s) else {
                    return Outcome::Nothing;
                };
                if !compiled.verify_errors.is_empty() {
                    return Outcome::Ice(compiled.verify_errors);
                }
                if !compiled.errors.is_empty() {
                    let lines = compiled
                        .errors
                        .iter()
                        .map(|e| {
                            let f = &s.t.files[e.file as usize];
                            let (l, col) = f.line_col(e.span.0);
                            format!("{}:{l}:{col}: error[{}]: {}", f.path, e.code, e.message)
                        })
                        .collect();
                    return Outcome::LowerErrors(lines);
                }
                if c == "pir" {
                    return Outcome::Text(compiled.module.to_string());
                }
                let Some(main) = compiled.main else {
                    return Outcome::NoMain;
                };
                let m = &compiled.module;
                if !m.funcs[main as usize].params.is_empty() {
                    return Outcome::NoMain;
                }
                if c == "pirb" || c == "build" {
                    return match pegasus_pir::bridge::encode(m, Some(main)) {
                        Ok(b) => Outcome::Bytes(b),
                        Err(e) => Outcome::Ice(e),
                    };
                }
                let mut it = Interp::new(m, Limits::default());
                Outcome::Run(match it.call(main, vec![]) {
                    Ok(v) => {
                        let rt = &m.funcs[main as usize].ret;
                        Ok(if *rt == pegasus_pir::ir::Type::unit() {
                            String::new()
                        } else {
                            interp::show(m, rt, &v)
                        })
                    }
                    Err(t) => Err(format!(
                        "trap: {}\n  in {}",
                        t.message,
                        t.backtrace.join("\n  in ")
                    )),
                })
            }
            _ => Outcome::Nothing,
        });
    for line in analysis.render() {
        eprintln!("{line}");
    }
    if analysis.has_errors() {
        return ExitCode::from(1);
    }
    match outcome {
        Outcome::Nothing => ExitCode::SUCCESS,
        Outcome::Text(t) => {
            print!("{t}");
            ExitCode::SUCCESS
        }
        Outcome::LowerErrors(ls) => {
            for l in ls {
                eprintln!("{l}");
            }
            ExitCode::from(1)
        }
        Outcome::Ice(errs) => {
            eprintln!("internal compiler error: PIR verification failed:");
            for e in errs {
                eprintln!("  {e}");
            }
            ExitCode::from(70)
        }
        Outcome::NoMain => {
            eprintln!("error: the package has no `fn main()` without parameters to run");
            ExitCode::from(1)
        }
        Outcome::Bytes(b) => {
            let out = output.expect("pirb and build take an output path");
            if cmd == "build" {
                return build(&b, &out, backend);
            }
            if let Err(e) = std::fs::write(&out, b) {
                eprintln!("error: cannot write {out}: {e}");
                return ExitCode::from(2);
            }
            ExitCode::SUCCESS
        }
        Outcome::Run(Ok(v)) => {
            if !v.is_empty() {
                println!("{v}");
            }
            ExitCode::SUCCESS
        }
        Outcome::Run(Err(t)) => {
            eprintln!("{t}");
            ExitCode::from(101)
        }
    }
}
