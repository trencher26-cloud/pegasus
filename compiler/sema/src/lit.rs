//! Literal classification and parsing.

use crate::ty::{FloatTy, IntTy};

#[derive(Clone, Debug, PartialEq)]
pub enum Lit {
    Int(u128, Option<IntTy>),
    Float(f64, Option<FloatTy>),
    Bool(bool),
    Char(char),
    Str(String),
    /// Byte string contents.
    Bytes(Vec<u8>),
}

const INT_SUFFIXES: [&str; 12] = [
    "i128", "u128", "isize", "usize", "i16", "i32", "i64", "u16", "u32", "u64", "i8", "u8",
];

/// Classify a literal token's source text. Returns None for text the lexer
/// would not have produced as a valid literal (it already reported that).
pub fn parse(text: &str) -> Option<Lit> {
    match text {
        "true" => return Some(Lit::Bool(true)),
        "false" => return Some(Lit::Bool(false)),
        _ => {}
    }
    let b = text.as_bytes();
    match b.first()? {
        b'\'' => unescape(&text[1..text.len().saturating_sub(1)]).and_then(|s| {
            let mut it = s.chars();
            let c = it.next()?;
            it.next().is_none().then_some(Lit::Char(c))
        }),
        b'"' => unescape(&text[1..text.len().saturating_sub(1)]).map(Lit::Str),
        b'r' if text.len() > 1 && (b[1] == b'"' || b[1] == b'#') => {
            let hashes = text[1..].bytes().take_while(|&c| c == b'#').count();
            let inner = &text[1 + hashes + 1..text.len() - 1 - hashes];
            Some(Lit::Str(inner.replace("\r\n", "\n")))
        }
        b'b' if text.len() > 1 && b[1] == b'"' => {
            unescape_bytes(&text[2..text.len() - 1]).map(Lit::Bytes)
        }
        b'0'..=b'9' => parse_number(text),
        _ => None,
    }
}

fn parse_number(text: &str) -> Option<Lit> {
    let (radix, body) = match text.get(..2) {
        Some("0x") => (16, &text[2..]),
        Some("0o") => (8, &text[2..]),
        Some("0b") => (2, &text[2..]),
        _ => (10, text),
    };
    if radix != 10 {
        let (digits, suf) = split_int_suffix(body);
        let v = u128::from_str_radix(&digits.replace('_', ""), radix).ok()?;
        return Some(Lit::Int(v, suf));
    }
    for fs in ["bf16", "f16", "f32", "f64"] {
        if let Some(d) = body.strip_suffix(fs) {
            if d.ends_with('b') {
                continue;
            }
            let v: f64 = d.replace('_', "").parse().ok()?;
            return Some(Lit::Float(v, FloatTy::from_name(fs)));
        }
    }
    let (digits, suf) = split_int_suffix(body);
    if suf.is_some() {
        let v: u128 = digits.replace('_', "").parse().ok()?;
        return Some(Lit::Int(v, suf));
    }
    if body.contains(['.', 'e', 'E']) {
        let v: f64 = body.replace('_', "").parse().ok()?;
        return Some(Lit::Float(v, None));
    }
    // Too large for u128 still classifies as an integer; range is checked
    // against the final type.
    let v: u128 = body.replace('_', "").parse().unwrap_or(u128::MAX);
    Some(Lit::Int(v, None))
}

fn split_int_suffix(s: &str) -> (&str, Option<IntTy>) {
    for suf in INT_SUFFIXES {
        if let Some(d) = s.strip_suffix(suf) {
            return (d, IntTy::from_name(suf));
        }
    }
    (s, None)
}

fn unescape(s: &str) -> Option<String> {
    let mut out = String::new();
    let mut it = s.chars().peekable();
    while let Some(c) = it.next() {
        if c == '{' || c == '}' {
            // `{{` / `}}` in strings without holes.
            if it.peek() == Some(&c) {
                it.next();
            }
            out.push(c);
            continue;
        }
        if c != '\\' {
            out.push(c);
            continue;
        }
        match it.next()? {
            'n' => out.push('\n'),
            'r' => out.push('\r'),
            't' => out.push('\t'),
            '0' => out.push('\0'),
            '\\' => out.push('\\'),
            '\'' => out.push('\''),
            '"' => out.push('"'),
            '{' => out.push('{'),
            '}' => out.push('}'),
            'u' => {
                if it.next()? != '{' {
                    return None;
                }
                let mut h = String::new();
                for c in it.by_ref() {
                    if c == '}' {
                        break;
                    }
                    h.push(c);
                }
                out.push(char::from_u32(u32::from_str_radix(&h, 16).ok()?)?);
            }
            _ => return None,
        }
    }
    Some(out)
}

fn unescape_bytes(s: &str) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i] != b'\\' {
            out.push(b[i]);
            i += 1;
            continue;
        }
        i += 1;
        match *b.get(i)? {
            b'n' => out.push(b'\n'),
            b'r' => out.push(b'\r'),
            b't' => out.push(b'\t'),
            b'0' => out.push(0),
            b'\\' => out.push(b'\\'),
            b'"' => out.push(b'"'),
            b'x' => {
                out.push(u8::from_str_radix(s.get(i + 1..i + 3)?, 16).ok()?);
                i += 2;
            }
            _ => return None,
        }
        i += 1;
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn numbers() {
        assert_eq!(parse("1_000u32"), Some(Lit::Int(1000, Some(IntTy::U32))));
        assert_eq!(
            parse("0xFF_FFu16"),
            Some(Lit::Int(0xFFFF, Some(IntTy::U16)))
        );
        assert_eq!(parse("0x1f16"), Some(Lit::Int(0x1f16, None)));
        assert_eq!(parse("3.0f32"), Some(Lit::Float(3.0, Some(FloatTy::F32))));
        assert_eq!(parse("1e9f64"), Some(Lit::Float(1e9, Some(FloatTy::F64))));
        assert_eq!(parse("2bf16"), Some(Lit::Float(2.0, Some(FloatTy::Bf16))));
        assert_eq!(parse("1e3"), Some(Lit::Float(1000.0, None)));
        assert_eq!(parse("42"), Some(Lit::Int(42, None)));
        assert_eq!(parse("'\\u{41}'"), Some(Lit::Char('A')));
        assert_eq!(parse("b\"a\\x01\""), Some(Lit::Bytes(vec![b'a', 1])));
        assert_eq!(parse("r#\"x\"#"), Some(Lit::Str("x".into())));
    }
}
