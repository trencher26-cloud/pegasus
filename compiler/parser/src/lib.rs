//! Pegasus parser: token stream (after the newline filter) to syntax tree.
//!
//! ```
//! let src = b"fn t() = 1 + 2";
//! let lexed = pegasus_lexer::lex(src);
//! let kept = pegasus_lexer::filter(src, &lexed.tokens);
//! let (module, diags) = pegasus_parser::parse(src, &lexed, &kept);
//! assert!(diags.is_empty());
//! assert_eq!(module.items.len(), 1);
//! ```

pub mod ast;
mod parser;
pub mod shape;

pub use parser::{parse, MAX_NESTING, PARSER_STACK};

use ast::{ExprKind, FnBody, ItemKind, Module};
use pegasus_lexer::Diagnostic;

/// Lex, filter and parse in one call. Returns the module and all lexer and
/// parser diagnostics, ordered by offset (lexer first on ties).
pub fn parse_source(src: &[u8]) -> (Module, Vec<Diagnostic>) {
    let lexed = pegasus_lexer::lex(src);
    let kept = pegasus_lexer::filter(src, &lexed.tokens);
    let (module, pdiags) = parse(src, &lexed, &kept);
    let mut all = lexed.diagnostics.clone();
    all.extend(pdiags);
    all.sort_by_key(|d| d.offset); // stable: lexer diagnostics stay first on ties
    (module, all)
}

/// The top-level function named `name`; failing that, the module's only
/// top-level function (corpus convention, tests/grammar/README.md).
pub fn find_fn<'m>(module: &'m Module, name: &str) -> Option<&'m ast::FnDecl> {
    let fns: Vec<&ast::FnDecl> = module
        .items
        .iter()
        .filter_map(|it| match &it.kind {
            ItemKind::Fn(f) => Some(f),
            _ => None,
        })
        .collect();
    fns.iter()
        .find(|f| f.sig.name == name)
        .copied()
        .or(if fns.len() == 1 { Some(fns[0]) } else { None })
}

/// Shape of `fn name() = expr` (corpus `shape` header).
pub fn fn_shape(module: &Module, name: &str) -> Option<String> {
    match find_fn(module, name)?.body.as_ref()? {
        FnBody::Expr(e) => Some(shape::render(e)),
        FnBody::Block(b) => match b.stmts.as_slice() {
            [ast::Stmt::Expr(e)] if !matches!(e.kind, ExprKind::Error) => Some(shape::render(e)),
            _ => None,
        },
    }
}

/// Statement count of `fn name() { ... }` (corpus `stmts` header).
pub fn fn_stmts(module: &Module, name: &str) -> Option<usize> {
    match find_fn(module, name)?.body.as_ref()? {
        FnBody::Block(b) => Some(b.stmts.len()),
        FnBody::Expr(_) => None,
    }
}
