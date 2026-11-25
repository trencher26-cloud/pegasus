//! Golden-file rendering and literal decoding.
//!
//! Golden line format (one token per line, spans as UTF-8 byte offsets):
//!
//! ```text
//! <offset> <length> <KIND> <escaped source text>
//! ```
//!
//! Tokens inside a string's interpolation hole follow the string on lines
//! indented by two spaces; a format specification appears as `FORMAT_SPEC`.

use crate::token::{Kind, Token};

/// Escape source bytes for a golden line: printable ASCII and other printable
/// characters pass through; `\`, CR, LF, tab, controls and bidi characters are
/// escaped; invalid UTF-8 bytes become `\xHH`.
pub fn escape(bytes: &[u8]) -> String {
    let mut out = String::new();
    let mut i = 0;
    while i < bytes.len() {
        match std::str::from_utf8(&bytes[i..]) {
            Ok(s) => {
                push_escaped(&mut out, s);
                break;
            }
            Err(e) => {
                let good = e.valid_up_to();
                push_escaped(&mut out, std::str::from_utf8(&bytes[i..i + good]).unwrap());
                i += good;
                let bad = e.error_len().unwrap_or(bytes.len() - i);
                for b in &bytes[i..i + bad] {
                    out.push_str(&format!("\\x{b:02x}"));
                }
                i += bad;
            }
        }
    }
    out
}

fn push_escaped(out: &mut String, s: &str) {
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c.is_control()
                || matches!(c as u32, 0x202A..=0x202E | 0x2066..=0x2069 | 0xFEFF) =>
            {
                out.push_str(&format!("\\u{{{:04X}}}", c as u32))
            }
            c => out.push(c),
        }
    }
}

fn render_one(out: &mut String, src: &[u8], t: &Token, indent: &str) {
    out.push_str(&format!(
        "{indent}{} {} {} {}\n",
        t.start,
        t.end - t.start,
        t.kind.label(),
        escape(t.text(src))
    ));
    for hole in &t.holes {
        for ht in &hole.tokens {
            render_one(out, src, ht, &format!("{indent}  "));
        }
        if let Some((a, b)) = hole.format_spec {
            out.push_str(&format!(
                "{indent}  {a} {} FORMAT_SPEC {}\n",
                b - a,
                escape(&src[a as usize..b as usize])
            ));
        }
    }
}

/// Render tokens (all, or the selected indices) as golden text.
pub fn render(src: &[u8], tokens: &[Token], select: Option<&[usize]>) -> String {
    let mut out = String::new();
    match select {
        None => tokens.iter().for_each(|t| render_one(&mut out, src, t, "")),
        Some(idx) => idx
            .iter()
            .for_each(|&i| render_one(&mut out, src, &tokens[i], "")),
    }
    out
}

/// The value of a string literal token: STRING_LIT without holes (escapes
/// decoded) or RAW_STRING (line endings normalized to LF, grammar 2.7).
pub fn string_value(src: &[u8], t: &Token) -> Option<String> {
    let text = std::str::from_utf8(t.text(src)).ok()?;
    match t.kind {
        Kind::RawStr => {
            let hashes = text[1..].bytes().take_while(|&b| b == b'#').count();
            let body = &text[1 + hashes + 1..text.len() - 1 - hashes];
            Some(body.replace("\r\n", "\n"))
        }
        Kind::Str if t.holes.is_empty() => {
            let body = &text[1..text.len() - 1];
            let mut out = String::new();
            let mut chars = body.chars().peekable();
            while let Some(c) = chars.next() {
                match c {
                    '\\' => match chars.next()? {
                        'n' => out.push('\n'),
                        'r' => out.push('\r'),
                        't' => out.push('\t'),
                        '0' => out.push('\0'),
                        'u' => {
                            chars.next(); // {
                            let hex: String = chars.by_ref().take_while(|&c| c != '}').collect();
                            out.push(char::from_u32(u32::from_str_radix(&hex, 16).ok()?)?);
                        }
                        other => out.push(other),
                    },
                    '{' | '}' => {
                        chars.next(); // doubled brace
                        out.push(c);
                    }
                    c => out.push(c),
                }
            }
            Some(out)
        }
        _ => None,
    }
}
