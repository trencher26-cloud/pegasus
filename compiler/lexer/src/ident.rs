//! Identifier keys: grammar 2.1 says identifiers are NFC-normalized before
//! comparison, and 2.3 that `r#with` and `with` are the same identifier.
//!
//! Ownership: the lexer never rewrites source. Tokens keep the exact source
//! spelling and byte span; `key` produces the comparison key, and the name
//! interner (resolver, Phase 2) must intern identifiers by `key`, never by raw
//! text. Keyword classification needs no normalization: NFC maps no non-ASCII
//! sequence onto ASCII letters, so a keyword can only be spelled one way.

use std::borrow::Cow;
use unicode_normalization::{is_nfc_quick, IsNormalized, UnicodeNormalization};

/// Comparison key for an IDENT or RAW_IDENT token's text.
pub fn key(text: &str) -> Cow<'_, str> {
    let name = text.strip_prefix("r#").unwrap_or(text);
    if name.is_ascii() || is_nfc_quick(name.chars()) == IsNormalized::Yes {
        Cow::Borrowed(name)
    } else {
        Cow::Owned(name.nfc().collect())
    }
}
