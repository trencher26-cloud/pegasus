//! Token and diagnostic types. Every position is a UTF-8 byte offset into the
//! source, counted from 0 at the first byte of the file (BOM included).

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Newline,
    DocComment,
    Ident,
    RawIdent,
    Keyword,
    Underscore,
    Int,
    Float,
    Char,
    Str,
    RawStr,
    ByteStr,
    Punct,
    Error,
    Eof,
}

impl Kind {
    /// Label used in golden files; matches the grammar's token names.
    pub fn label(self) -> &'static str {
        match self {
            Kind::Newline => "NEWLINE",
            Kind::DocComment => "DOC_COMMENT",
            Kind::Ident => "IDENT",
            Kind::RawIdent => "RAW_IDENT",
            Kind::Keyword => "KEYWORD",
            Kind::Underscore => "UNDERSCORE",
            Kind::Int => "INT_LIT",
            Kind::Float => "FLOAT_LIT",
            Kind::Char => "CHAR_LIT",
            Kind::Str => "STRING_LIT",
            Kind::RawStr => "RAW_STRING",
            Kind::ByteStr => "BYTE_STRING",
            Kind::Punct => "PUNCT",
            Kind::Error => "ERROR",
            Kind::Eof => "EOF",
        }
    }
}

/// An interpolation hole inside a STRING_LIT: the expression's tokens and the
/// optional format specification span.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hole {
    pub tokens: Vec<Token>,
    pub format_spec: Option<(u32, u32)>,
    /// Bracket-match table for `tokens` (grammar section 7, rule C), so a hole's
    /// expression is parsed with the same O(1) closure decision as top-level code.
    pub matches: Vec<Option<u32>>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Token {
    pub kind: Kind,
    pub start: u32,
    pub end: u32,
    pub holes: Vec<Hole>,
}

impl Token {
    pub fn new(kind: Kind, start: usize, end: usize) -> Self {
        Token {
            kind,
            start: start as u32,
            end: end as u32,
            holes: Vec::new(),
        }
    }

    pub fn text<'a>(&self, src: &'a [u8]) -> &'a [u8] {
        &src[self.start as usize..self.end as usize]
    }

    /// True for a PUNCT, KEYWORD or IDENT token whose text is exactly `s`.
    pub fn is(&self, src: &[u8], kind: Kind, s: &str) -> bool {
        self.kind == kind && self.text(src) == s.as_bytes()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Severity {
    /// Makes the program invalid (an `L` code).
    Error,
    /// Reported, but the program stays valid (a `W` code).
    Warning,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Diagnostic {
    /// Stable message ID from tests/grammar/diagnostics.toml.
    pub code: &'static str,
    pub severity: Severity,
    pub offset: u32,
    pub len: u32,
    /// Optional secondary span for diagnostics that relate two source locations,
    /// such as P010 (the opener and the unexpected closer).
    pub related: Option<(u32, u32)>,
}
