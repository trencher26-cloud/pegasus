//! The newline filter: grammar v0.1.1-draft, section 3.
//!
//! A deterministic state machine between lexer and parser. It sees tokens only,
//! tracks bracket nesting, attribute boundaries and one item-header state, and
//! decides for every NEWLINE whether it reaches the parser.

use crate::lexer::BracketStack;
use crate::token::{Kind, Token};

#[derive(Clone, Copy, Debug)]
struct Header {
    depth: usize,
    /// `fn` or `extern` header: only these have a parameter list.
    is_fn: bool,
    angles: usize,
    params_seen: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Attr {
    Idle,
    ExpectName,
    AfterName,
    /// Inside the attribute's `( ... )`; holds the stack size before the `(`.
    Args(usize),
}

const CONTINUATION: [&str; 47] = [
    "**", "*", "/", "%", "*%", "*|", "+", "-", "+%", "-%", "+|", "-|", "<<", ">>", "&", "^", "|",
    "==", "!=", "<", ">", "<=", ">=", "&&", "||", "..", "..=", "=", "+=", "-=", "*=", "/=", "%=",
    "&=", "|=", "^=", "<<=", ">>=", "**=", ",", ".", ":", "->", "=>", "(", "[", "{",
];

/// Returns the indices (into `tokens`) of the tokens that reach the parser.
/// `tokens` must be the lexer's top-level token list, ending with EOF.
pub fn filter(src: &[u8], tokens: &[Token]) -> Vec<usize> {
    let text = |t: &Token| t.text(src);
    let is_punct = |t: &Token, s: &str| t.kind == Kind::Punct && text(t) == s.as_bytes();
    let is_kw = |t: &Token, s: &str| t.kind == Kind::Keyword && text(t) == s.as_bytes();
    let is_ident = |t: &Token, s: &str| t.kind == Kind::Ident && text(t) == s.as_bytes();

    let mut out: Vec<usize> = Vec::with_capacity(tokens.len());
    let mut stack: BracketStack<()> = BracketStack::new();
    let mut header: Option<Header> = None;
    let mut attr = Attr::Idle;
    let mut attr_closed = false; // previous passed token closed an attribute's ( ... )
                                 // prev / prev2: the last two tokens passed to the parser (None = start of file).
    let mut prev: Option<usize> = None;
    let mut prev2: Option<usize> = None;

    // next_real[i]: index of the first non-NEWLINE token after i, computed in one
    // reverse pass so the filter stays O(n) however many blank lines there are.
    let mut next_real = vec![tokens.len(); tokens.len()];
    let mut nearest = tokens.len();
    for i in (0..tokens.len()).rev() {
        next_real[i] = nearest;
        if tokens[i].kind != Kind::Newline {
            nearest = i;
        }
    }

    for (i, t) in tokens.iter().enumerate() {
        if t.kind == Kind::Newline {
            let next = tokens.get(next_real[i]);
            let prev_tok = prev.map(|p| &tokens[p]);
            let drop = {
                // Rule 1: inside ( ) or [ ].
                matches!(stack.top(), Some(b'(') | Some(b'['))
                // Rule 2: continuation token at line end.
                || prev_tok.is_some_and(|p| {
                    (p.kind == Kind::Punct && CONTINUATION.iter().any(|c| text(p) == c.as_bytes()))
                        || is_kw(p, "as") || is_kw(p, "else") || is_kw(p, "in")
                })
                // Rule 3: leading `.` or `else` on the next line.
                || next.is_some_and(|n| is_punct(n, ".") || is_kw(n, "else"))
                // Rule 4: header clause.
                || (header.is_some() && next.is_some_and(|n| {
                    is_kw(n, "requires") || is_kw(n, "ensures") || is_kw(n, "where")
                        || is_ident(n, "effects") || is_punct(n, "->") || is_punct(n, "{")
                }))
                // Rule 5: duplicate.
                || prev_tok.is_some_and(|p| p.kind == Kind::Newline)
            };
            if !drop {
                header = None;
                if attr == Attr::AfterName {
                    attr = Attr::Idle;
                }
                attr_closed = false;
                out.push(i);
                prev2 = prev;
                prev = Some(i);
            }
            continue;
        }

        // Attribute boundary tracking: `@` dotted.name ( ... )?
        let mut attr_ended_before = false;
        let mut closes_attr = false;
        match attr {
            Attr::ExpectName => {
                attr = if matches!(t.kind, Kind::Ident | Kind::Keyword | Kind::RawIdent) {
                    Attr::AfterName
                } else {
                    Attr::Idle
                };
            }
            Attr::AfterName => {
                if is_punct(t, ".") {
                    attr = Attr::ExpectName;
                } else if is_punct(t, "(") {
                    attr = Attr::Args(stack.len());
                } else {
                    attr = Attr::Idle;
                    attr_ended_before = true;
                }
            }
            Attr::Args(d) => {
                if is_punct(t, ")") && stack.len() == d + 1 {
                    attr = Attr::Idle;
                    closes_attr = true;
                }
            }
            Attr::Idle => {}
        }
        if is_punct(t, "@") {
            attr = Attr::ExpectName;
        }

        // Item start and header entry.
        let at_item_start = attr_ended_before
            || attr_closed
            || match prev.map(|p| &tokens[p]) {
                None => true,
                Some(p) => {
                    p.kind == Kind::Newline
                        || p.kind == Kind::DocComment
                        || is_punct(p, ";")
                        || is_punct(p, "{")
                        || is_punct(p, "}")
                        || is_kw(p, "pub")
                        || is_kw(p, "async")
                        || is_kw(p, "comptime")
                        || is_kw(p, "kernel")
                        || (p.kind == Kind::Str
                            && prev2.is_some_and(|q| is_ident(&tokens[q], "extern")))
                }
            };
        let header_kw = ["fn", "struct", "enum", "trait", "impl", "type"]
            .iter()
            .any(|k| is_kw(t, k))
            || is_ident(t, "extern");
        if header.is_none() && header_kw && at_item_start {
            header = Some(Header {
                depth: stack.len(),
                is_fn: is_kw(t, "fn") || is_ident(t, "extern"),
                angles: 0,
                params_seen: false,
            });
        }

        // Header bookkeeping, for tokens at the header's depth.
        let mut end_header = false;
        if let Some(h) = header.as_mut() {
            if stack.len() == h.depth {
                if !h.params_seen && t.kind == Kind::Punct {
                    match text(t) {
                        b"<" => h.angles += 1,
                        b">" => h.angles = h.angles.saturating_sub(1),
                        b">>" => h.angles = h.angles.saturating_sub(2),
                        // `>=` / `>>=` here are closing angle(s) followed by `=`
                        // (grammar 2.6 splitting); the `=` ends the header once the
                        // angles are closed, exactly as a separate `=` would.
                        b">=" | b">>=" => {
                            let closes = if text(t) == b">=" { 1 } else { 2 };
                            h.angles = h.angles.saturating_sub(closes);
                            if h.angles == 0 {
                                end_header = true;
                            }
                        }
                        // Only a `(` outside the generic angles of a fn header
                        // starts the parameter list (grammar 3, changelog 29).
                        b"(" if h.is_fn && h.angles == 0 => h.params_seen = true,
                        _ => {}
                    }
                }
                if h.angles == 0 && (is_punct(t, "{") || is_punct(t, "=")) {
                    header = None;
                }
            }
        }
        if end_header {
            header = None;
        }

        // Bracket stack.
        if t.kind == Kind::Punct && t.end - t.start == 1 {
            match text(t)[0] {
                c @ (b'(' | b'[' | b'{') => stack.push(c, ()),
                // Recovery rule (grammar 3 and 8), amortized O(1): pop down to
                // the nearest opener of the same kind, or, if none is open, close
                // the innermost opener as a mistyped closer.
                c @ (b')' | b']' | b'}') => {
                    stack.close(c);
                }
                _ => {}
            }
        }
        if let Some(h) = header {
            if stack.len() < h.depth {
                header = None;
            }
        }

        attr_closed = closes_attr;
        out.push(i);
        prev2 = prev;
        prev = Some(i);
    }
    out
}
