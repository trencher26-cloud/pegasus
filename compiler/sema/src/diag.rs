//! Semantic diagnostics. Codes are catalogued in spec/semantics.md:
//! R = resolution, T = types, U = valid syntax the compiler does not yet
//! implement (never silently accepted), W = warning.

use crate::source::FileId;
pub use pegasus_lexer::Severity;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Diag {
    pub code: &'static str,
    pub severity: Severity,
    pub file: FileId,
    pub offset: u32,
    pub len: u32,
    pub message: String,
}

#[derive(Default, Debug)]
pub struct Diags {
    pub list: Vec<Diag>,
}

impl Diags {
    pub fn error(&mut self, code: &'static str, file: FileId, span: (u32, u32), message: String) {
        self.push(code, Severity::Error, file, span, message);
    }
    pub fn warn(&mut self, code: &'static str, file: FileId, span: (u32, u32), message: String) {
        self.push(code, Severity::Warning, file, span, message);
    }
    fn push(
        &mut self,
        code: &'static str,
        severity: Severity,
        file: FileId,
        span: (u32, u32),
        message: String,
    ) {
        let d = Diag {
            code,
            severity,
            file,
            offset: span.0,
            len: span.1.saturating_sub(span.0),
            message,
        };
        // The same check may run twice on shared AST (e.g. a type written once
        // and lowered for two purposes); report each problem once.
        if !self.list.contains(&d) {
            self.list.push(d);
        }
    }
    pub fn has_errors(&self) -> bool {
        self.list.iter().any(|d| d.severity == Severity::Error)
    }
}
