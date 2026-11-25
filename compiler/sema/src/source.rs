//! Source files and package loading.
//!
//! A package is a root file plus the files its `mod name` declarations load.
//! Module `m` whose directory is `D` loads `mod x` from `D/x.peg` or
//! `D/x/mod.peg`; both existing is an error (R014). The new module's
//! directory is `D/x`. The root file's directory is the package root; an
//! inline `mod x { ... }` has directory `D/x`.

use pegasus_lexer::Diagnostic;
use pegasus_parser::ast;
use std::collections::HashMap;
use std::path::PathBuf;

pub type FileId = u32;

pub struct SourceFile {
    /// Path relative to the package root ("main.peg", "a/b.peg"), or
    /// "<core>" for the built-in prelude.
    pub path: String,
    pub src: Vec<u8>,
    pub module: ast::Module,
    /// Lexer and parser diagnostics for this file.
    pub syntax: Vec<Diagnostic>,
}

impl SourceFile {
    pub fn parse(path: String, src: Vec<u8>) -> SourceFile {
        let (module, syntax) = pegasus_parser::parse_source(&src);
        SourceFile {
            path,
            src,
            module,
            syntax,
        }
    }
    /// 1-based line and column (in bytes) of an offset.
    pub fn line_col(&self, offset: u32) -> (u32, u32) {
        let off = (offset as usize).min(self.src.len());
        let before = &self.src[..off];
        let line = before.iter().filter(|&&b| b == b'\n').count() as u32 + 1;
        let col = (off
            - before
                .iter()
                .rposition(|&b| b == b'\n')
                .map_or(0, |p| p + 1)) as u32
            + 1;
        (line, col)
    }
}

/// Reads package files by path relative to the package root.
pub trait Loader {
    fn read(&self, rel: &str) -> Option<Vec<u8>>;
}

/// Files held in memory (tests, the language server).
#[derive(Default)]
pub struct MemLoader {
    pub files: HashMap<String, Vec<u8>>,
}

impl MemLoader {
    pub fn new(files: &[(&str, &str)]) -> MemLoader {
        MemLoader {
            files: files
                .iter()
                .map(|(p, s)| (p.to_string(), s.as_bytes().to_vec()))
                .collect(),
        }
    }
}

impl Loader for MemLoader {
    fn read(&self, rel: &str) -> Option<Vec<u8>> {
        self.files.get(rel).cloned()
    }
}

/// Files on disk below a root directory.
pub struct FsLoader {
    pub root: PathBuf,
}

impl Loader for FsLoader {
    fn read(&self, rel: &str) -> Option<Vec<u8>> {
        std::fs::read(self.root.join(rel)).ok()
    }
}

pub const PRELUDE_SRC: &str = include_str!("../prelude/core.peg");
