//! Pegasus IR (PIR v0.1): the authoritative intermediate representation
//! (Phase 0 §10), its verifier, lowering from typed HIR, and the reference
//! interpreter. See spec/pir.md.

pub mod bridge;
pub mod interp;
pub mod ir;
pub mod lower;
pub mod verify;

use pegasus_sema::items::Sema;

/// The outcome of compiling a checked package to PIR.
pub struct Compiled {
    pub module: ir::Module,
    /// The root module's `main`, if it exists and was lowered.
    pub main: Option<ir::FuncId>,
    pub errors: Vec<lower::LowerErr>,
    /// Verifier failures (compiler bugs).
    pub verify_errors: Vec<String>,
}

/// Lower a package that sema accepted (no syntax or semantic errors) and
/// verify the result. Returns None if the package has errors.
pub fn compile(s: &mut Sema) -> Option<Compiled> {
    let syntax_errors = s.t.files.iter().any(|f| {
        f.syntax
            .iter()
            .any(|d| d.severity == pegasus_lexer_severity::Error)
    });
    if syntax_errors || s.diags.has_errors() {
        return None;
    }
    let (module, roots, errors) = lower::lower_package(s);
    let main_def = s.t.mods[&s.t.root].values.get("main").map(|b| b.def);
    let main = main_def.and_then(|d| roots.get(&d).copied());
    let verify_errors = verify::verify(&module).err().unwrap_or_default();
    Some(Compiled {
        module,
        main,
        errors,
        verify_errors,
    })
}

use pegasus_sema::diag::Severity as pegasus_lexer_severity;
