//! The lexer: grammar v0.1.1-draft, section 2.
//!
//! Byte-oriented. Produces every token the grammar emits (including NEWLINE and
//! DOC_COMMENT), records lexical diagnostics, and never stops at an error:
//! malformed input becomes ERROR tokens and lexing continues (section 8).

use crate::token::{Diagnostic, Hole, Kind, Severity, Token};
use unicode_ident::{is_xid_continue, is_xid_start};

pub const KEYWORDS: [&str; 36] = [
    "as", "async", "await", "break", "comptime", "const", "continue", "defer", "else", "enum",
    "ensures", "false", "fn", "for", "if", "impl", "in", "kernel", "let", "loop", "match", "mod",
    "pub", "requires", "return", "self", "Self", "struct", "trait", "true", "type", "unsafe",
    "use", "var", "where", "while",
];
const PUNCT3: [&str; 4] = ["**=", "<<=", ">>=", "..="];
const PUNCT2: [&str; 26] = [
    "**", "+%", "-%", "*%", "+|", "-|", "*|", "<<", ">>", "<=", ">=", "==", "!=", "&&", "||", "..",
    "=>", "->", "+=", "-=", "*=", "/=", "%=", "&=", "|=", "^=",
];
const PUNCT1: &[u8] = b"+-*/%&|^~!<>=?.,:;@()[]{}";
const INT_SUFFIX: [&str; 12] = [
    "i8", "i16", "i32", "i64", "i128", "isize", "u8", "u16", "u32", "u64", "u128", "usize",
];
const FLOAT_SUFFIX: [&str; 4] = ["f16", "bf16", "f32", "f64"];

/// Result of lexing one file.
#[derive(Debug)]
pub struct Lexed {
    /// Top-level tokens in source order, ending with EOF.
    pub tokens: Vec<Token>,
    /// Lexical errors and warnings, ordered by offset. Use `errors()` and
    /// `warnings()` to separate them.
    pub diagnostics: Vec<Diagnostic>,
    /// Bracket-match table (grammar section 7, rule C): for each top-level token
    /// that is `(`, `[` or `{` (or its closer), the index of its partner.
    pub matches: Vec<Option<u32>>,
}

impl Lexed {
    pub fn errors(&self) -> impl Iterator<Item = &Diagnostic> {
        self.diagnostics
            .iter()
            .filter(|d| d.severity == Severity::Error)
    }
    pub fn warnings(&self) -> impl Iterator<Item = &Diagnostic> {
        self.diagnostics
            .iter()
            .filter(|d| d.severity == Severity::Warning)
    }
}

/// Decoded character at a position.
enum Ch {
    Ok(char, usize),
    Bad,
}

fn ch_at(src: &[u8], at: usize) -> Option<Ch> {
    let b = *src.get(at)?;
    if b < 0x80 {
        return Some(Ch::Ok(b as char, 1));
    }
    let w = match b {
        0xC2..=0xDF => 2,
        0xE0..=0xEF => 3,
        0xF0..=0xF4 => 4,
        _ => return Some(Ch::Bad),
    };
    if at + w > src.len() {
        return Some(Ch::Bad);
    }
    match std::str::from_utf8(&src[at..at + w]) {
        Ok(s) => Some(Ch::Ok(s.chars().next().unwrap(), w)),
        Err(_) => Some(Ch::Bad),
    }
}

fn is_bidi(c: char) -> bool {
    matches!(c as u32, 0x202A..=0x202E | 0x2066..=0x2069)
}

fn is_id_start(c: char) -> bool {
    c == '_' || is_xid_start(c)
}

/// How an interpolation hole ended.
enum HoleEnd {
    Colon,
    Brace,
    LineEnd,
    Eof,
}

struct Lexer<'a> {
    src: &'a [u8],
    pos: usize,
    diags: Vec<Diagnostic>,
    /// Errors reported so far (warnings excluded). A literal or doc comment
    /// during whose scan this grows is malformed and becomes an ERROR token.
    errors: usize,
}

pub fn lex(src: &[u8]) -> Lexed {
    let mut lx = Lexer {
        src,
        pos: 0,
        diags: Vec::new(),
        errors: 0,
    };
    if src.starts_with(b"\xEF\xBB\xBF") {
        lx.pos = 3;
    }
    let (mut tokens, _) = lx.run(false);
    tokens.push(Token::new(Kind::Eof, src.len(), src.len()));
    let mut diagnostics = lx.diags;
    diagnostics.sort_by_key(|d| d.offset);
    let matches = bracket_matches(src, &tokens);
    Lexed {
        tokens,
        diagnostics,
        matches,
    }
}

/// The opener that pairs with a closing bracket.
pub fn opener_for(closer: u8) -> u8 {
    match closer {
        b')' => b'(',
        b']' => b'[',
        _ => b'{',
    }
}

/// A bracket stack with the recovery rule of grammar sections 3 and 8, in
/// amortized O(1) per operation: alongside the stack it keeps, for each bracket
/// kind, the stack positions of open brackets of that kind, so the nearest
/// same-kind opener is found without scanning. Each opener is pushed once and
/// popped once, so a whole file costs O(n) however it is mismatched.
pub struct BracketStack<T> {
    items: Vec<(u8, T)>,
    by_kind: [Vec<usize>; 3],
}

fn kind_index(opener: u8) -> usize {
    match opener {
        b'(' => 0,
        b'[' => 1,
        _ => 2,
    }
}

impl<T: Copy> BracketStack<T> {
    pub fn new() -> Self {
        BracketStack {
            items: Vec::new(),
            by_kind: [Vec::new(), Vec::new(), Vec::new()],
        }
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// The innermost open bracket.
    pub fn top(&self) -> Option<u8> {
        self.items.last().map(|&(k, _)| k)
    }

    pub fn push(&mut self, opener: u8, value: T) {
        self.by_kind[kind_index(opener)].push(self.items.len());
        self.items.push((opener, value));
    }

    fn pop_one(&mut self) -> Option<(u8, T)> {
        let item = self.items.pop()?;
        self.by_kind[kind_index(item.0)].pop();
        Some(item)
    }

    /// Apply a closer: pop down to the nearest opener of its own kind and return
    /// that opener's value; if none is open, pop the innermost opener (a
    /// mistyped closer) and return None.
    pub fn close(&mut self, closer: u8) -> Option<T> {
        let kind = kind_index(opener_for(closer));
        match self.by_kind[kind].last().copied() {
            Some(pos) => {
                while self.items.len() > pos + 1 {
                    self.pop_one();
                }
                self.pop_one().map(|(_, v)| v)
            }
            None => {
                self.pop_one();
                None
            }
        }
    }
}

impl<T: Copy> Default for BracketStack<T> {
    fn default() -> Self {
        Self::new()
    }
}

/// Bracket-match table, built in one O(n) pass with a `BracketStack`. Only
/// same-kind pairs are recorded; openers left unclosed by recovery stay None.
pub fn bracket_matches(src: &[u8], tokens: &[Token]) -> Vec<Option<u32>> {
    let mut m = vec![None; tokens.len()];
    let mut stack: BracketStack<usize> = BracketStack::new();
    for (i, t) in tokens.iter().enumerate() {
        if t.kind != Kind::Punct || t.end - t.start != 1 {
            continue;
        }
        match t.text(src)[0] {
            c @ (b'(' | b'[' | b'{') => stack.push(c, i),
            c @ (b')' | b']' | b'}') => {
                if let Some(j) = stack.close(c) {
                    m[i] = Some(j as u32);
                    m[j] = Some(i as u32);
                }
            }
            _ => {}
        }
    }
    m
}

impl<'a> Lexer<'a> {
    fn peek(&self, off: usize) -> Option<u8> {
        self.src.get(self.pos + off).copied()
    }

    fn diag(&mut self, code: &'static str, offset: usize, len: usize) {
        self.errors += 1;
        self.diags.push(Diagnostic {
            code,
            severity: Severity::Error,
            offset: offset as u32,
            len: len as u32,
            related: None,
        });
    }

    fn warn(&mut self, code: &'static str, offset: usize, len: usize) {
        self.diags.push(Diagnostic {
            code,
            severity: Severity::Warning,
            offset: offset as u32,
            len: len as u32,
            related: None,
        });
    }

    /// Lex until end of input, or (in a hole) until the hole's `:` or `}` at
    /// bracket depth 0, or a line end.
    fn run(&mut self, hole: bool) -> (Vec<Token>, HoleEnd) {
        let src = self.src;
        let mut out: Vec<Token> = Vec::new();
        let mut depth = 0usize;
        loop {
            let Some(b) = self.peek(0) else {
                return (out, HoleEnd::Eof);
            };
            if hole {
                if b == b'\n' || b == b'\r' {
                    return (out, HoleEnd::LineEnd);
                }
                if depth == 0 && b == b':' {
                    return (out, HoleEnd::Colon);
                }
                if depth == 0 && b == b'}' {
                    return (out, HoleEnd::Brace);
                }
            }
            let start = self.pos;
            match b {
                b' ' | b'\t' => self.pos += 1,
                b'\n' => {
                    out.push(Token::new(Kind::Newline, start, start + 1));
                    self.pos += 1;
                }
                b'\r' => {
                    if self.peek(1) == Some(b'\n') {
                        out.push(Token::new(Kind::Newline, start, start + 2));
                        self.pos += 2;
                    } else {
                        self.diag("L002", start, 1);
                        out.push(Token::new(Kind::Error, start, start + 1));
                        self.pos += 1;
                    }
                }
                b'/' if self.peek(1) == Some(b'/') => self.line_comment(&mut out),
                b'/' if self.peek(1) == Some(b'*') => self.block_comment(&mut out),
                b'0'..=b'9' => {
                    let after_dot = matches!(out.last(), Some(t) if t.is(src, Kind::Punct, "."));
                    self.number(&mut out, after_dot);
                }
                b'"' => self.string(&mut out, hole),
                b'\'' => self.char_lit(&mut out),
                b'b' if self.peek(1) == Some(b'"') => self.byte_string(&mut out),
                b'r' if self.raw_string_hashes().is_some() => self.raw_string(&mut out),
                b'r' if self.peek(1) == Some(b'#')
                    && matches!(ch_at(src, start + 2), Some(Ch::Ok(c, _)) if is_id_start(c)) =>
                {
                    self.raw_ident(&mut out)
                }
                _ => match ch_at(src, start).unwrap() {
                    Ch::Bad => {
                        self.diag("L001", start, 1);
                        out.push(Token::new(Kind::Error, start, start + 1));
                        self.pos += 1;
                    }
                    Ch::Ok(c, w) => {
                        if is_id_start(c) {
                            self.word(&mut out);
                        } else if is_bidi(c) {
                            self.diag("L003", start, w);
                            out.push(Token::new(Kind::Error, start, start + w));
                            self.pos += w;
                        } else if let Some(n) = self.punct_len() {
                            out.push(Token::new(Kind::Punct, start, start + n));
                            self.pos += n;
                            if hole && n == 1 {
                                match b {
                                    b'(' | b'[' | b'{' => depth += 1,
                                    b')' | b']' | b'}' => depth = depth.saturating_sub(1),
                                    _ => {}
                                }
                            }
                        } else {
                            self.diag("L009", start, w);
                            out.push(Token::new(Kind::Error, start, start + w));
                            self.pos += w;
                        }
                    }
                },
            }
        }
    }

    fn punct_len(&self) -> Option<usize> {
        let rest = &self.src[self.pos..];
        for p in PUNCT3 {
            if rest.starts_with(p.as_bytes()) {
                return Some(3);
            }
        }
        for p in PUNCT2 {
            if rest.starts_with(p.as_bytes()) {
                return Some(2);
            }
        }
        PUNCT1.contains(&rest[0]).then_some(1)
    }

    /// Advance over one source character inside a comment or literal body,
    /// reporting invalid UTF-8. Returns false at end of input.
    fn skip_char(&mut self) -> bool {
        match ch_at(self.src, self.pos) {
            None => false,
            Some(Ch::Ok(c, w)) => {
                // Bidi controls are errors in code but warnings inside comments
                // and literals (grammar 2.1, Trojan Source mitigation).
                if is_bidi(c) {
                    self.warn("W001", self.pos, w);
                }
                self.pos += w;
                true
            }
            Some(Ch::Bad) => {
                self.diag("L001", self.pos, 1);
                self.pos += 1;
                true
            }
        }
    }

    fn word(&mut self, out: &mut Vec<Token>) {
        let start = self.pos;
        self.read_word();
        let text = &self.src[start..self.pos];
        let kind = if text == b"_" {
            Kind::Underscore
        } else if KEYWORDS.iter().any(|k| k.as_bytes() == text) {
            Kind::Keyword
        } else {
            Kind::Ident
        };
        out.push(Token::new(kind, start, self.pos));
    }

    /// Reads ID_START ID_CONTINUE* at the current position (caller checked the start).
    fn read_word(&mut self) {
        if let Some(Ch::Ok(_, w)) = ch_at(self.src, self.pos) {
            self.pos += w;
        }
        while let Some(Ch::Ok(c, w)) = ch_at(self.src, self.pos) {
            if !is_xid_continue(c) {
                break;
            }
            self.pos += w;
        }
    }

    /// Grammar 2.3: `r#` + longest WORD; self, Self and _ are one ERROR token (L008).
    fn raw_ident(&mut self, out: &mut Vec<Token>) {
        let start = self.pos;
        self.pos += 2;
        let wstart = self.pos;
        self.read_word();
        let word = &self.src[wstart..self.pos];
        if word == b"self" || word == b"Self" || word == b"_" {
            self.diag("L008", start, self.pos - start);
            out.push(Token::new(Kind::Error, start, self.pos));
        } else {
            out.push(Token::new(Kind::RawIdent, start, self.pos));
        }
    }

    fn line_comment(&mut self, out: &mut Vec<Token>) {
        let start = self.pos;
        let errors_before = self.errors;
        let doc = self.peek(2) == Some(b'/') && self.peek(3) != Some(b'/');
        self.pos += 2;
        loop {
            match self.peek(0) {
                None | Some(b'\n') => break,
                Some(b'\r') if self.peek(1) == Some(b'\n') => break,
                Some(b'\r') => {
                    self.diag("L002", self.pos, 1);
                    self.pos += 1;
                }
                Some(_) => {
                    self.skip_char();
                }
            }
        }
        if doc {
            // DOC_COMMENT owns its line ending (grammar 2.2).
            match self.peek(0) {
                Some(b'\n') => self.pos += 1,
                Some(b'\r') => self.pos += 2,
                _ => {}
            }
            let kind = if self.errors > errors_before {
                Kind::Error
            } else {
                Kind::DocComment
            };
            out.push(Token::new(kind, start, self.pos));
        }
    }

    fn block_comment(&mut self, out: &mut Vec<Token>) {
        let start = self.pos;
        self.pos += 2;
        let mut depth = 1usize;
        let mut saw_line_end = false;
        while depth > 0 {
            match (self.peek(0), self.peek(1)) {
                (None, _) => {
                    self.diag("L004", start, 2);
                    break;
                }
                (Some(b'/'), Some(b'*')) => {
                    depth += 1;
                    self.pos += 2;
                }
                (Some(b'*'), Some(b'/')) => {
                    depth -= 1;
                    self.pos += 2;
                }
                (Some(b'\n'), _) => {
                    saw_line_end = true;
                    self.pos += 1;
                }
                (Some(b'\r'), Some(b'\n')) => {
                    saw_line_end = true;
                    self.pos += 2;
                }
                (Some(b'\r'), _) => {
                    self.diag("L002", self.pos, 1);
                    self.pos += 1;
                }
                _ => {
                    self.skip_char();
                }
            }
        }
        if saw_line_end {
            // A block comment containing a line end yields one NEWLINE in its place.
            out.push(Token::new(Kind::Newline, start, self.pos));
        }
    }

    fn eat_digits(&mut self, ok: fn(u8) -> bool) {
        while let Some(b) = self.peek(0) {
            if ok(b) || b == b'_' {
                self.pos += 1;
            } else {
                break;
            }
        }
    }

    fn number(&mut self, out: &mut Vec<Token>, after_dot: bool) {
        let start = self.pos;
        let mut kind = Kind::Int;
        let mut based = false;
        let prefix = (self.peek(0), self.peek(1), self.peek(2));
        let base_digit: Option<fn(u8) -> bool> = match prefix {
            (Some(b'0'), Some(b'x'), Some(c)) if c.is_ascii_hexdigit() => {
                Some(|b: u8| b.is_ascii_hexdigit())
            }
            (Some(b'0'), Some(b'o'), Some(b'0'..=b'7')) => Some(|b: u8| (b'0'..=b'7').contains(&b)),
            (Some(b'0'), Some(b'b'), Some(b'0' | b'1')) => Some(|b: u8| b == b'0' || b == b'1'),
            _ => None,
        };
        if let (false, Some(ok)) = (after_dot, base_digit) {
            based = true;
            self.pos += 2;
            self.eat_digits(ok);
        } else {
            self.eat_digits(|b| b.is_ascii_digit());
            if !after_dot {
                if self.peek(0) == Some(b'.') && matches!(self.peek(1), Some(b'0'..=b'9')) {
                    self.pos += 1;
                    self.eat_digits(|b| b.is_ascii_digit());
                    kind = Kind::Float;
                }
                let exp = match (self.peek(0), self.peek(1), self.peek(2)) {
                    (Some(b'e' | b'E'), Some(b'0'..=b'9'), _) => Some(1),
                    (Some(b'e' | b'E'), Some(b'+' | b'-'), Some(b'0'..=b'9')) => Some(2),
                    _ => None,
                };
                if let Some(n) = exp {
                    self.pos += n;
                    self.eat_digits(|b| b.is_ascii_digit());
                    kind = Kind::Float;
                }
            }
        }
        // Suffix boundary (grammar 2.5): an ID_CONTINUE run right after the literal
        // must be a valid suffix, or the whole run is one ERROR token (L006).
        let sstart = self.pos;
        while let Some(Ch::Ok(c, w)) = ch_at(self.src, self.pos) {
            if !is_xid_continue(c) {
                break;
            }
            self.pos += w;
        }
        if self.pos > sstart {
            let suffix = std::str::from_utf8(&self.src[sstart..self.pos]).unwrap_or("");
            let int_ok = kind == Kind::Int && INT_SUFFIX.contains(&suffix);
            let float_ok = !based && !after_dot && FLOAT_SUFFIX.contains(&suffix);
            if float_ok {
                kind = Kind::Float;
            } else if !int_ok {
                self.diag("L006", start, self.pos - start);
                out.push(Token::new(Kind::Error, start, self.pos));
                return;
            }
        }
        out.push(Token::new(kind, start, self.pos));
    }

    /// Escape sequence after a backslash at self.pos. Returns false if invalid
    /// (diagnostic already reported, position advanced past the bad escape).
    fn escape(&mut self, allowed: &[u8], unicode: bool, hex_byte: bool) -> bool {
        let at = self.pos;
        match self.peek(1) {
            Some(c) if allowed.contains(&c) => {
                self.pos += 2;
                true
            }
            Some(b'u') if unicode => {
                let rest = &self.src[at + 2..];
                let close = rest.iter().take(9).position(|&b| b == b'}');
                if rest.first() == Some(&b'{') {
                    if let Some(close) = close {
                        let hex = &rest[1..close];
                        let valid = (1..=6).contains(&hex.len())
                            && hex.iter().all(u8::is_ascii_hexdigit)
                            && u32::from_str_radix(std::str::from_utf8(hex).unwrap(), 16)
                                .ok()
                                .and_then(char::from_u32)
                                .is_some();
                        if valid {
                            self.pos = at + 2 + close + 1;
                            return true;
                        }
                        self.diag("L007", at, 2 + close + 1);
                        self.pos = at + 2 + close + 1;
                        return false;
                    }
                }
                self.diag("L007", at, 2);
                self.pos += 2;
                false
            }
            Some(b'x') if hex_byte => {
                let ok = matches!((self.peek(2), self.peek(3)), (Some(a), Some(b)) if a.is_ascii_hexdigit() && b.is_ascii_hexdigit());
                if ok {
                    self.pos += 4;
                    true
                } else {
                    self.diag("L007", at, 2);
                    self.pos += 2;
                    false
                }
            }
            Some(b'\n' | b'\r') | None => {
                self.diag("L007", at, 1);
                self.pos += 1;
                false
            }
            Some(_) => {
                self.diag("L007", at, 2);
                self.pos += 1;
                self.skip_char();
                false
            }
        }
    }

    fn string(&mut self, out: &mut Vec<Token>, in_hole: bool) {
        let start = self.pos;
        self.pos += 1;
        let mut holes = Vec::new();
        // Errors inside holes belong to the hole's own tokens, not to the string.
        let errors_before = self.errors;
        let mut hole_errors = 0;
        loop {
            match self.peek(0) {
                None | Some(b'\n') | Some(b'\r') => {
                    // Ordinary strings are single-line: ends at its line (grammar 8).
                    self.diag("L005", start, 1);
                    out.push(Token::new(Kind::Error, start, self.pos));
                    return;
                }
                Some(b'"') => {
                    self.pos += 1;
                    let malformed = self.errors - errors_before > hole_errors;
                    let kind = if malformed { Kind::Error } else { Kind::Str };
                    let mut t = Token::new(kind, start, self.pos);
                    t.holes = holes;
                    out.push(t);
                    return;
                }
                Some(b'\\') => {
                    self.escape(b"nrt0\\'\"{}", true, false);
                }
                Some(b'{') if self.peek(1) == Some(b'{') => self.pos += 2,
                Some(b'}') if self.peek(1) == Some(b'}') => self.pos += 2,
                Some(b'}') => {
                    self.diag("L013", self.pos, 1);
                    self.pos += 1;
                }
                Some(b'{') if in_hole => {
                    self.diag("L012", self.pos, 1);
                    self.pos += 1;
                }
                Some(b'{') => {
                    let open = self.pos;
                    self.pos += 1;
                    let hole_start_errors = self.errors;
                    let (tokens, end) = self.run(true);
                    hole_errors += self.errors - hole_start_errors;
                    let mut format_spec = None;
                    match end {
                        HoleEnd::Brace => self.pos += 1,
                        HoleEnd::Colon => {
                            self.pos += 1;
                            let fs = self.pos;
                            while let Some(b) = self.peek(0) {
                                if matches!(b, b'{' | b'}' | b'"' | b'\n' | b'\r') {
                                    break;
                                }
                                self.skip_char();
                            }
                            format_spec = Some((fs as u32, self.pos as u32));
                            if self.peek(0) == Some(b'}') {
                                self.pos += 1;
                            } else {
                                self.diag("L014", open, 1);
                            }
                        }
                        HoleEnd::LineEnd | HoleEnd::Eof => {
                            self.diag("L005", start, 1);
                            out.push(Token::new(Kind::Error, start, self.pos));
                            return;
                        }
                    }
                    let matches = bracket_matches(self.src, &tokens);
                    holes.push(Hole {
                        tokens,
                        format_spec,
                        matches,
                    });
                }
                Some(_) => {
                    self.skip_char();
                }
            }
        }
    }

    fn char_lit(&mut self, out: &mut Vec<Token>) {
        let start = self.pos;
        self.pos += 1;
        let mut ok = true;
        match self.peek(0) {
            None | Some(b'\n') | Some(b'\r') => {
                self.diag("L005", start, 1);
                out.push(Token::new(Kind::Error, start, self.pos));
                return;
            }
            Some(b'\'') => ok = false, // empty literal ''
            Some(b'\\') => ok &= self.escape(b"nrt0\\'\"{}", true, false),
            Some(_) => {
                self.skip_char();
            }
        }
        if ok && self.peek(0) == Some(b'\'') {
            self.pos += 1;
            out.push(Token::new(Kind::Char, start, self.pos));
            return;
        }
        // Not exactly one character: find a closing quote on this line (L011),
        // otherwise the literal is unterminated (L005).
        let line_rest = &self.src[self.pos..];
        let eol = line_rest
            .iter()
            .position(|&b| b == b'\n' || b == b'\r')
            .unwrap_or(line_rest.len());
        let close = line_rest[..eol].iter().position(|&b| b == b'\'');
        match close {
            Some(i) => {
                self.pos += i + 1;
                if ok {
                    self.diag("L011", start, self.pos - start);
                } else if self.pos - start == 2 {
                    self.diag("L011", start, 2);
                }
                out.push(Token::new(Kind::Error, start, self.pos));
            }
            None => {
                self.pos += eol;
                self.diag("L005", start, 1);
                out.push(Token::new(Kind::Error, start, self.pos));
            }
        }
    }

    /// If a raw string starts here (`r`, then 0-255 `#`, then `"`), its hash count.
    fn raw_string_hashes(&self) -> Option<usize> {
        let rest = &self.src[self.pos + 1..];
        let n = rest.iter().take_while(|&&b| b == b'#').count();
        (n <= 255 && rest.get(n) == Some(&b'"')).then_some(n)
    }

    fn raw_string(&mut self, out: &mut Vec<Token>) {
        let start = self.pos;
        let errors_before = self.errors;
        let hashes = self.raw_string_hashes().unwrap();
        self.pos += 1 + hashes + 1;
        loop {
            match self.peek(0) {
                None => {
                    self.diag("L010", start, 1 + hashes + 1);
                    out.push(Token::new(Kind::Error, start, self.pos));
                    return;
                }
                Some(b'"') => {
                    let close = &self.src[self.pos + 1..];
                    if close.len() >= hashes && close[..hashes].iter().all(|&b| b == b'#') {
                        self.pos += 1 + hashes;
                        let kind = if self.errors > errors_before {
                            Kind::Error
                        } else {
                            Kind::RawStr
                        };
                        out.push(Token::new(kind, start, self.pos));
                        return;
                    }
                    self.pos += 1;
                }
                Some(b'\r') if self.peek(1) == Some(b'\n') => self.pos += 2,
                Some(b'\r') => {
                    self.diag("L002", self.pos, 1);
                    self.pos += 1;
                }
                Some(_) => {
                    self.skip_char();
                }
            }
        }
    }

    fn byte_string(&mut self, out: &mut Vec<Token>) {
        let start = self.pos;
        let errors_before = self.errors;
        self.pos += 2;
        loop {
            match self.peek(0) {
                None | Some(b'\n') | Some(b'\r') => {
                    self.diag("L005", start, 2);
                    out.push(Token::new(Kind::Error, start, self.pos));
                    return;
                }
                Some(b'"') => {
                    self.pos += 1;
                    let kind = if self.errors > errors_before {
                        Kind::Error
                    } else {
                        Kind::ByteStr
                    };
                    out.push(Token::new(kind, start, self.pos));
                    return;
                }
                Some(b'\\') => {
                    self.escape(b"nrt0\\\"", false, true);
                }
                Some(0x20..=0x7E) => self.pos += 1,
                Some(_) => {
                    let at = self.pos;
                    self.skip_char();
                    self.diag("L015", at, self.pos - at);
                }
            }
        }
    }
}
