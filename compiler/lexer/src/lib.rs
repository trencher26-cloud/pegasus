//! Pegasus lexer and newline filter, implementing grammar v0.1.1-draft
//! sections 2 and 3 (spec/grammar.md, spec/pegasus.ebnf).
//!
//! This is the hand-written bootstrap implementation. Its output is checked
//! against the conformance corpus in tests/grammar; it is not the golden oracle.

pub mod filter;
pub mod golden;
pub mod ident;
pub mod lexer;
pub mod token;

pub use filter::filter;
pub use lexer::{lex, Lexed};
pub use token::{Diagnostic, Hole, Kind, Severity, Token};
