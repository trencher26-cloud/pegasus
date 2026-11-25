//! Pegasus semantic frontend (Phase 3, sema v0.1).
//!
//! Input: the parser's AST for every file of a package. Output: resolved
//! definitions, typed HIR for every function body, and diagnostics. What is
//! implemented, and what is rejected with a `U` diagnostic because it is not
//! implemented yet, is specified in spec/semantics.md.

mod calls;
pub mod consteval;
pub mod defs;
pub mod diag;
mod exhaust;
mod expr;
mod finish;
pub mod hir;
pub mod infer;
pub mod items;
pub mod lit;
mod pat;
pub mod purm;
pub mod source;
pub mod ty;
pub mod typeck;

use diag::{Diag, Diags};
use pegasus_lexer::Severity;
use source::{FileId, Loader, MemLoader, SourceFile};

/// The result of checking a package.
pub struct Analysis {
    pub files: Vec<SourceFile>,
    /// Syntax diagnostics (lexer, parser) as (file, diagnostic).
    pub syntax: Vec<(FileId, pegasus_lexer::Diagnostic)>,
    /// Semantic diagnostics, in report order.
    pub diags: Vec<Diag>,
}

impl Analysis {
    pub fn has_errors(&self) -> bool {
        self.syntax
            .iter()
            .any(|(_, d)| d.severity == Severity::Error)
            || self.diags.iter().any(|d| d.severity == Severity::Error)
    }

    /// Diagnostics rendered as `path:line:col: error[CODE]: message`.
    pub fn render(&self) -> Vec<String> {
        let mut out = Vec::new();
        for (f, d) in &self.syntax {
            let file = &self.files[*f as usize];
            let (l, c) = file.line_col(d.offset);
            let sev = if d.severity == Severity::Error {
                "error"
            } else {
                "warning"
            };
            out.push(format!("{}:{l}:{c}: {sev}[{}]: syntax", file.path, d.code));
        }
        for d in &self.diags {
            let file = &self.files[d.file as usize];
            let (l, c) = file.line_col(d.offset);
            let sev = if d.severity == Severity::Error {
                "error"
            } else {
                "warning"
            };
            out.push(format!(
                "{}:{l}:{c}: {sev}[{}]: {}",
                file.path, d.code, d.message
            ));
        }
        out
    }
}

/// Stack for the semantic phases. Recursion in the checker is bounded
/// (parser nesting limit 128; exhaustiveness depth limit 1024), and this
/// stack holds the deepest case even in unoptimized builds, whatever thread
/// the caller uses.
pub const SEMA_STACK: usize = 64 << 20;

/// Check a package whose root file is `root` (relative to the loader).
/// `f` receives the checked semantic state (typed HIR in `Sema::bodies`)
/// before it is dropped.
pub fn check_package_with<R: Send>(
    loader: &(dyn Loader + Sync),
    root: &str,
    f: impl FnOnce(&mut items::Sema) -> R + Send,
) -> (Analysis, R) {
    std::thread::scope(|scope| {
        std::thread::Builder::new()
            .name("pegasus-sema".into())
            .stack_size(SEMA_STACK)
            .spawn_scoped(scope, || check_package_inner(loader, root, f))
            .expect("spawn the semantic analysis thread")
            .join()
            .unwrap_or_else(|e| std::panic::resume_unwind(e))
    })
}

fn check_package_inner<R>(
    loader: &dyn Loader,
    root: &str,
    f: impl FnOnce(&mut items::Sema) -> R,
) -> (Analysis, R) {
    let mut diags = Diags::default();
    let (files, mod_files) = defs::load_package(loader, root, &mut diags);
    let mut syntax = Vec::new();
    for (i, file) in files.iter().enumerate() {
        for d in &file.syntax {
            syntax.push((i as FileId, d.clone()));
        }
    }
    let r = {
        let mut t = defs::DefTable::collect(&files, &mod_files, &mut diags);
        t.resolve_imports(&mut diags);
        let mut s = items::Sema::new(t, diags);
        run(&mut s);
        let r = f(&mut s);
        diags = std::mem::take(&mut s.diags);
        r
    };
    (
        Analysis {
            files,
            syntax,
            diags: diags.list,
        },
        r,
    )
}

pub fn check_package(loader: &(dyn Loader + Sync), root: &str) -> Analysis {
    check_package_with(loader, root, |_| ()).0
}

/// Check a single-file package held in memory.
pub fn check_source(src: &str) -> Analysis {
    check_package(&MemLoader::new(&[("main.peg", src)]), "main.peg")
}

fn run(s: &mut items::Sema) {
    let n = s.t.defs.len() as defs::DefId;
    // Item signatures first, then layouts, impls, and bodies.
    for d in 0..n {
        match s.def(d).kind {
            defs::DefKind::Struct | defs::DefKind::Enum => {
                s.adt(d);
            }
            defs::DefKind::Trait | defs::DefKind::TypeAlias => {
                s.generics_of(d);
            }
            _ => {}
        }
    }
    s.collect_impls();
    for d in 0..n {
        match s.def(d).kind {
            defs::DefKind::Fn | defs::DefKind::ExternFn => {
                s.sig(d);
            }
            defs::DefKind::TypeAlias => check_alias(s, d),
            _ => {}
        }
    }
    s.check_recursive_adts();
    s.check_impls();
    typeck::check_bodies(s);
    // Ownership checking runs only on well-typed code.
    if !s.diags.has_errors() {
        purm::check(s);
    }
}

/// Lower an alias's target once so errors in it are reported even when the
/// alias is never used.
fn check_alias(s: &mut items::Sema, d: defs::DefId) {
    let Some(item) = s.def(d).item else { return };
    let pegasus_parser::ast::ItemKind::TypeAlias { ty, refinement, .. } = &item.kind else {
        return;
    };
    // Associated types inside impls are lowered at their use sites.
    if s.def(d)
        .parent
        .is_some_and(|p| s.def(p).kind != defs::DefKind::Mod)
    {
        return;
    }
    if refinement.is_some() {
        let (f, sp) = (s.def(d).file, s.def(d).span);
        s.err(
            "U006",
            f,
            sp,
            format!(
                "refinement type `{}` needs the contract prover, which is not implemented yet",
                s.def(d).name
            ),
        );
        return;
    }
    let g = s.generics_of(d);
    let scope = s.scope_for(d, &g);
    s.lower_ty(ty, &scope);
}
