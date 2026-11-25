//! Recursive-descent parser for grammar v0.1.2-draft, sections 4-8 (and rule 2a of section 3).
//!
//! Input is the token stream that survives the newline filter (section 3),
//! read through the lexer's token indices; no token is copied except where a
//! generic closer is split (section 2.6). Parsing never backtracks. The two
//! lookahead points are rule B (a bounded scan, at most 256 tokens) and rule C
//! (an O(1) lookup in the lexer's bracket-match table).

use crate::ast::*;
use pegasus_lexer::lexer::BracketStack;
use pegasus_lexer::{Diagnostic, Hole, Kind, Lexed, Severity, Token};

#[derive(Clone, Copy, Debug)]
struct PTok {
    kind: Kind,
    start: u32,
    end: u32,
    /// Partner position in this parser's token list for either side of a
    /// same-kind bracket pair (openers point forward, closers point backward).
    close: Option<usize>,
    /// Index into the lexer's token list (for interpolation holes).
    lex: Option<usize>,
    /// A dormant NEWLINE (grammar 3, rule 2a): invisible to the cursor until
    /// the token before it is consumed as a generic closer or a use glob.
    soft: bool,
}

type PResult<T> = Result<T, ()>;

const MAX_DIAGNOSTICS: usize = 100;
const RULE_B_LIMIT: usize = 256;
/// Nesting limit (P018), so hostile input cannot exhaust the stack.
pub const MAX_NESTING: usize = 128;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Ctx {
    Module,
    Impl,
    Trait,
}

pub struct Parser<'a> {
    src: &'a [u8],
    lexed_tokens: &'a [Token],
    toks: Vec<PTok>,
    pos: usize,
    last_end: u32,
    depth: usize,
    /// No-cascade rule (8): after a recovery, report nothing until a token has
    /// been consumed normally.
    suppress: bool,
    diags: Vec<Diagnostic>,
    /// Consumed opening delimiters, represented by parser-token indices. This
    /// remains accurate while parsing and while recovery skips malformed text.
    delimiters: Vec<usize>,
    /// Curly openers only, kept separately so item recovery's brace-depth test
    /// remains O(1) even when parentheses/brackets nest deeply.
    braces: Vec<usize>,
    /// P018 is a fatal-to-the-current-item recovery signal, not a local list
    /// error. It is cleared only after the enclosing item recovery completes.
    fatal_recovery: bool,
}

/// Tokens after which a NEWLINE dropped by filter rule 2 is only dormant.
const SOFT_PREV: [&str; 3] = [">", ">>", "*"];
/// Next tokens for which the NEWLINE stays dropped (filter rules 3 and 4).
const SOFT_EXEMPT_NEXT: [&str; 8] = [
    ".", "else", "{", "where", "requires", "ensures", "effects", "->",
];

/// Parse a whole file. `kept` is the newline filter's output for `lexed`.
///
/// Rule 2a (grammar 3): filter rule 2 cannot tell a comparison `>` or a
/// multiplication `*` at a line end from a generic closer or a use glob. The
/// NEWLINE after such a token, outside `()` and `[]` and not exempted by rules 3
/// and 4, is kept here as a dormant token. The parser wakes it when it consumes
/// the token as a generic closer or a glob, so the line break separates items,
/// statements and fields; otherwise it stays invisible and the line continues.
///
/// The parse runs on its own thread with a PARSER_STACK stack. Nesting is capped
/// at MAX_NESTING (P018), and the deepest permitted input needs at most ~5 MiB in
/// an unoptimized build (~1 MiB optimized), so no input can overflow the stack,
/// whatever thread the caller is on.
pub fn parse(src: &[u8], lexed: &Lexed, kept: &[usize]) -> (Module, Vec<Diagnostic>) {
    std::thread::scope(|scope| {
        std::thread::Builder::new()
            .name("pegasus-parser".into())
            .stack_size(PARSER_STACK)
            .spawn_scoped(scope, || parse_on_this_thread(src, lexed, kept))
            .expect("cannot spawn the parser thread")
            .join()
            .unwrap_or_else(|panic| std::panic::resume_unwind(panic))
    })
}

/// Stack for the parser thread (reserved, not committed, by the OS).
pub const PARSER_STACK: usize = 32 << 20;

fn parse_on_this_thread(src: &[u8], lexed: &Lexed, kept: &[usize]) -> (Module, Vec<Diagnostic>) {
    let text = |i: usize| -> &[u8] { lexed.tokens[i].text(src) };
    let mut toks: Vec<PTok> = Vec::with_capacity(kept.len());
    // The filter's own bracket stack and closer rule (grammar 3), so "inside
    // `()` or `[]`" means exactly what it means for rule 1.
    let mut stack: BracketStack<()> = BracketStack::new();
    for (k, &i) in kept.iter().enumerate() {
        let t = &lexed.tokens[i];
        toks.push(PTok {
            kind: t.kind,
            start: t.start,
            end: t.end,
            close: None,
            lex: Some(i),
            soft: false,
        });
        if t.kind == Kind::Punct {
            match text(i) {
                [b @ (b'(' | b'[' | b'{')] => stack.push(*b, ()),
                [b @ (b')' | b']' | b'}')] => {
                    stack.close(*b);
                }
                _ => {}
            }
        }
        let Some(&next) = kept.get(k + 1) else {
            continue;
        };
        if matches!(stack.top(), Some(b'(' | b'['))
            || t.kind != Kind::Punct
            || !SOFT_PREV.iter().any(|p| text(i) == p.as_bytes())
        {
            continue;
        }
        if SOFT_EXEMPT_NEXT.iter().any(|x| text(next) == x.as_bytes()) {
            continue;
        }
        if let Some(nl) = lexed.tokens[i + 1..next]
            .iter()
            .find(|t| t.kind == Kind::Newline)
        {
            toks.push(PTok {
                kind: Kind::Newline,
                start: nl.start,
                end: nl.end,
                close: None,
                lex: None,
                soft: true,
            });
        }
    }
    let mut pos_of = vec![usize::MAX; lexed.tokens.len()];
    for (p, t) in toks.iter().enumerate() {
        if let Some(i) = t.lex {
            pos_of[i] = p;
        }
    }
    for t in toks.iter_mut() {
        if let Some(i) = t.lex {
            t.close = lexed.matches[i]
                .map(|c| pos_of[c as usize])
                .filter(|&c| c != usize::MAX);
        }
    }
    let mut p = Parser {
        src,
        lexed_tokens: &lexed.tokens,
        toks,
        pos: 0,
        last_end: 0,
        depth: 0,
        suppress: false,
        diags: Vec::new(),
        delimiters: Vec::new(),
        braces: Vec::new(),
        fatal_recovery: false,
    };
    let module = p.module();
    (module, p.diags)
}

impl<'a> Parser<'a> {
    fn for_hole(src: &'a [u8], lexed_tokens: &'a [Token], hole: &Hole, depth: usize) -> Self {
        let mut toks: Vec<PTok> = hole
            .tokens
            .iter()
            .enumerate()
            .map(|(i, t)| PTok {
                kind: t.kind,
                start: t.start,
                end: t.end,
                close: hole.matches[i].map(|c| c as usize),
                lex: None,
                soft: false,
            })
            .collect();
        let end = hole.tokens.last().map(|t| t.end).unwrap_or(0);
        toks.push(PTok {
            kind: Kind::Eof,
            start: end,
            end,
            close: None,
            lex: None,
            soft: false,
        });
        Parser {
            src,
            lexed_tokens,
            toks,
            pos: 0,
            last_end: 0,
            depth,
            suppress: false,
            diags: Vec::new(),
            delimiters: Vec::new(),
            braces: Vec::new(),
            fatal_recovery: false,
        }
    }

    // ------------------------------------------------------------ cursor

    /// First position at or after `i` that is not a dormant NEWLINE.
    fn live(&self, mut i: usize) -> usize {
        while i < self.toks.len() - 1 && self.toks[i].soft {
            i += 1;
        }
        i.min(self.toks.len() - 1)
    }

    fn tok(&self) -> PTok {
        self.toks[self.live(self.pos)]
    }

    fn nth(&self, n: usize) -> PTok {
        let mut i = self.live(self.pos);
        for _ in 0..n {
            i = self.live(i + 1);
        }
        self.toks[i]
    }

    /// Wake a dormant NEWLINE directly after the token just consumed (rule 2a).
    fn wake_newline(&mut self) {
        if let Some(t) = self.toks.get_mut(self.pos) {
            if t.soft {
                t.soft = false;
            }
        }
    }

    fn text(&self, t: PTok) -> &'a str {
        std::str::from_utf8(&self.src[t.start as usize..t.end as usize]).unwrap_or("")
    }

    fn is_p(&self, t: PTok, s: &str) -> bool {
        t.kind == Kind::Punct && self.text(t) == s
    }

    fn is_kw(&self, t: PTok, s: &str) -> bool {
        t.kind == Kind::Keyword && self.text(t) == s
    }

    fn is_id(&self, t: PTok, s: &str) -> bool {
        t.kind == Kind::Ident && self.text(t) == s
    }

    fn at_p(&self, s: &str) -> bool {
        self.is_p(self.tok(), s)
    }

    fn at_kw(&self, s: &str) -> bool {
        self.is_kw(self.tok(), s)
    }

    fn at_id(&self, s: &str) -> bool {
        self.is_id(self.tok(), s)
    }

    fn is_name(t: PTok) -> bool {
        matches!(t.kind, Kind::Ident | Kind::RawIdent)
    }

    fn at_eof(&self) -> bool {
        self.tok().kind == Kind::Eof
    }

    fn track_delimiter(&mut self, index: usize, t: PTok) {
        if t.kind != Kind::Punct || t.end - t.start != 1 {
            return;
        }
        match self.text(t) {
            "(" | "[" => self.delimiters.push(index),
            "{" => {
                self.delimiters.push(index);
                self.braces.push(index);
            }
            ")" | "]" | "}" => {
                if let Some(open) = t.close {
                    if let Some(pos) = self.delimiters.iter().rposition(|&i| i == open) {
                        self.delimiters.truncate(pos);
                        // All curly openers at or above the matched opener were
                        // popped by the same bracket-stack operation.
                        let brace_pos = self.braces.partition_point(|&i| i < open);
                        self.braces.truncate(brace_pos);
                    }
                } else if let Some(open) = self.delimiters.pop() {
                    // No same-kind opener: the lexer pops the innermost opener.
                    if self.is_p(self.toks[open], "{") {
                        self.braces.pop();
                    }
                }
            }
            _ => {}
        }
    }

    fn bump(&mut self) -> PTok {
        self.pos = self.live(self.pos);
        let index = self.pos;
        let t = self.tok();
        if t.kind != Kind::Eof {
            self.track_delimiter(index, t);
            self.pos += 1;
            self.last_end = t.end;
            self.suppress = false;
        }
        t
    }

    fn bump_text(&mut self) -> &'a str {
        let t = self.bump();
        self.text(t)
    }

    fn span_from(&self, start: u32) -> Span {
        (start, self.last_end.max(start))
    }

    fn slice(&self, start: u32) -> String {
        String::from_utf8_lossy(&self.src[start as usize..self.last_end.max(start) as usize])
            .into_owned()
    }

    // ------------------------------------------------------------ errors

    fn err_at<T>(&mut self, code: &'static str, offset: u32, len: u32) -> PResult<T> {
        self.err_related_at(code, offset, len, None)
    }

    fn err_related_at<T>(
        &mut self,
        code: &'static str,
        offset: u32,
        len: u32,
        related: Option<(u32, u32)>,
    ) -> PResult<T> {
        if code == "P018" {
            self.fatal_recovery = true;
        }
        if !self.suppress && self.diags.len() < MAX_DIAGNOSTICS {
            self.diags.push(Diagnostic {
                code,
                severity: Severity::Error,
                offset,
                len,
                related,
            });
        }
        Err(())
    }

    fn err<T>(&mut self, code: &'static str) -> PResult<T> {
        let t = self.tok();
        self.err_at(code, t.start, t.end - t.start)
    }

    /// Run `f` one nesting level deeper; P018 past MAX_NESTING.
    fn nested<T>(&mut self, f: impl FnOnce(&mut Self) -> PResult<T>) -> PResult<T> {
        if self.depth >= MAX_NESTING {
            return self.err("P018");
        }
        self.depth += 1;
        let r = f(self);
        self.depth -= 1;
        r
    }

    fn is_closer(&self, t: PTok) -> bool {
        self.is_p(t, ")") || self.is_p(t, "]") || self.is_p(t, "}")
    }

    fn expect_p(&mut self, s: &str) -> PResult<PTok> {
        if self.at_p(s) {
            Ok(self.bump())
        } else {
            self.err("P001")
        }
    }

    fn expect_kw(&mut self, s: &str) -> PResult<PTok> {
        if self.at_kw(s) {
            Ok(self.bump())
        } else {
            self.err("P001")
        }
    }

    /// Report an unexpected closer, including the opener selected by the
    /// grammar's same-kind-first / otherwise-innermost recovery rule.
    fn err_unexpected_closer<T>(&mut self) -> PResult<T> {
        let t = self.tok();
        let related_open = t
            .close
            .filter(|open| self.delimiters.contains(open))
            .or_else(|| self.delimiters.last().copied());
        let related = related_open.map(|open| {
            let opener = self.toks[open];
            (opener.start, opener.end - opener.start)
        });
        self.err_related_at("P010", t.start, t.end - t.start, related)
    }

    /// Expect a specific closer; a different closer is P010 (grammar 8).
    fn expect_close(&mut self, c: &str) -> PResult<PTok> {
        if self.at_p(c) {
            Ok(self.bump())
        } else if self.is_closer(self.tok()) {
            self.err_unexpected_closer()
        } else {
            self.err("P001")
        }
    }

    /// Consume one `>` closing a generic list, splitting `>>`, `>=`, `>>=` (2.6).
    fn expect_gt(&mut self) -> PResult<()> {
        let t = self.tok();
        if t.kind == Kind::Punct && self.text(t).starts_with('>') {
            if t.end - t.start == 1 {
                self.bump();
                self.wake_newline();
            } else {
                let p = self.live(self.pos);
                self.toks[p].start += 1;
                self.last_end = t.start + 1;
            }
            Ok(())
        } else {
            self.err("P001")
        }
    }

    fn name(&mut self) -> PResult<String> {
        if Self::is_name(self.tok()) {
            let t = self.bump();
            Ok(self.text(t).to_string())
        } else {
            self.err("P001")
        }
    }

    fn skip_newlines(&mut self) {
        while self.tok().kind == Kind::Newline {
            self.bump();
        }
    }

    fn at_sep(&self) -> bool {
        self.tok().kind == Kind::Newline || self.at_p(";")
    }

    fn skip_seps(&mut self) {
        while self.at_sep() {
            self.bump();
        }
    }

    // ------------------------------------------------------------ recovery (8)
    //
    // A function that fails has already recorded its diagnostic and returns
    // Err. The nearest enclosing recovery point catches it, skips to its
    // synchronisation token (jumping over matched bracket groups in O(1), so
    // recovery is linear), and carries on; `suppress` then silences cascades.

    /// Advance past one token without clearing `suppress`.
    fn skip_one(&mut self) {
        let p = self.live(self.pos);
        let t = self.toks[p];
        if t.kind != Kind::Eof {
            let wakes_newline =
                t.kind == Kind::Punct && SOFT_PREV.iter().any(|marker| self.text(t) == *marker);
            self.track_delimiter(p, t);
            self.last_end = t.end;
            self.pos = p + 1;
            // Recovery has no syntactic role for the skipped token. Therefore
            // a dormant newline after `>`, `>>` or `*` must become a real
            // separator, rather than silently joining the next construct.
            if wakes_newline {
                self.wake_newline();
            }
        }
    }

    /// Skip one token, or a whole matched bracket group if one opens here.
    fn skip_group_or_one(&mut self) {
        let p = self.live(self.pos);
        let t = self.toks[p];
        let opener = self.is_p(t, "(") || self.is_p(t, "[") || self.is_p(t, "{");
        match t.close {
            Some(c) if opener && c > p => {
                self.last_end = self.toks[c].end;
                self.pos = c + 1;
            }
            _ => self.skip_one(),
        }
    }

    fn at_recovery_depth(&self, base_depth: usize) -> bool {
        // A mismatched closer may already have popped the recovery boundary.
        // In that case the parser is back at or outside the boundary's depth.
        self.delimiters.len() <= base_depth
    }

    /// A closer belongs to an ancestor of this recovery boundary.
    /// The boundary's own closer is handled by the enclosing loop; treating it
    /// as an outer closer would make that loop lose its closing token.
    fn closes_boundary_or_outer(&self, t: PTok, boundary: Option<usize>) -> bool {
        if !self.is_closer(t) {
            return false;
        }
        match (boundary, t.close) {
            (Some(boundary), Some(open)) => open < boundary,
            _ => false,
        }
    }

    /// Whether the current closer is paired with this exact opener. For a
    /// mistyped closer with no same-kind partner, the lexer leaves `close`
    /// empty; the innermost active opener is then the one it will pop.
    fn is_boundary_close(&self, close: &str, boundary: usize) -> bool {
        let t = self.tok();
        if !self.is_p(t, close) {
            return false;
        }
        match t.close {
            Some(open) => open == boundary,
            // An unpaired closer. A stray closer earlier in this block may
            // already have made the bracket stack pop this block's own
            // opener, so the stack top is at or below the boundary, or empty.
            None => self.delimiters.last().is_none_or(|&top| top <= boundary),
        }
    }

    /// A recovery routine may intentionally stop without consuming a
    /// separator or this list's own closer. An ancestor's closer must instead
    /// propagate to the enclosing parser; it must never be retried in a loop.
    fn recovery_stopped_at_outer(&self, boundary: usize) -> bool {
        self.closes_boundary_or_outer(self.tok(), Some(boundary))
    }

    fn ensure_module_recovery_progress(&mut self, before: usize) {
        if self.live(self.pos) != before
            || self.at_eof()
            || (self.at_line_start() && self.at_item_starter())
        {
            return;
        }
        self.skip_one();
    }

    /// Enforce the parser's fundamental recovery invariant: every recovery
    /// iteration either advances, stops at a token the caller can consume, or
    /// propagates an enclosing closer. This prevents malformed input from
    /// repeatedly appending Error nodes while leaving the cursor unchanged.
    fn ensure_recovery_progress(
        &mut self,
        before: usize,
        boundary: usize,
        close: &str,
        allow_comma: bool,
        allow_item_start: bool,
    ) -> PResult<()> {
        if self.live(self.pos) != before || self.at_eof() {
            return Ok(());
        }
        let t = self.tok();
        if t.kind == Kind::Newline
            || self.is_p(t, ";")
            || (allow_comma && self.is_p(t, ","))
            || self.is_boundary_close(close, boundary)
            || (allow_item_start && self.at_line_start() && self.at_item_starter())
        {
            return Ok(());
        }
        if self.is_closer(t) {
            return Err(());
        }
        self.skip_one();
        Ok(())
    }

    /// Expression/list recovery: stop at a comma or the boundary closer, but
    /// consume closers belonging to already-open nested groups.
    fn sync_list(&mut self, boundary: usize, base_depth: usize) {
        self.suppress = true;
        loop {
            let t = self.tok();
            if t.kind == Kind::Eof
                || (self.at_recovery_depth(base_depth)
                    && (self.is_p(t, ",")
                        || (self.is_closer(t) && t.close.is_none_or(|open| open <= boundary))))
                || self.closes_boundary_or_outer(t, Some(boundary))
            {
                return;
            }
            self.skip_group_or_one();
        }
    }

    /// Recovery for `{ ... }` lists whose separators may be newlines.
    fn sync_brace_list(&mut self, boundary: usize, base_depth: usize) {
        self.suppress = true;
        loop {
            let t = self.tok();
            if t.kind == Kind::Eof
                || (self.at_recovery_depth(base_depth)
                    && (t.kind == Kind::Newline || self.is_p(t, ",") || self.is_p(t, "}")))
                || self.closes_boundary_or_outer(t, Some(boundary))
            {
                return;
            }
            self.skip_group_or_one();
        }
    }

    /// Statement recovery is scoped to the block that began at `boundary`.
    /// A `}` for a nested brace group already consumed by the failed parse is
    /// skipped; only this block's closer (or an ancestor's) can stop recovery.
    fn sync_stmt(&mut self, boundary: usize, base_depth: usize) {
        self.suppress = true;
        loop {
            let t = self.tok();
            if t.kind == Kind::Eof
                || (self.at_recovery_depth(base_depth)
                    && (t.kind == Kind::Newline || self.is_p(t, ";") || self.is_p(t, "}")))
                || self.closes_boundary_or_outer(t, Some(boundary))
            {
                return;
            }
            self.skip_group_or_one();
        }
    }

    /// True if the token at the cursor is the first on its line.
    fn at_line_start(&self) -> bool {
        let i = self.live(self.pos);
        if i == 0 {
            return true;
        }
        let prev = self.toks[i - 1];
        prev.kind == Kind::Newline
            || self.src[prev.end as usize..self.toks[i].start as usize].contains(&b'\n')
    }

    fn at_item_brace_depth(&self, boundary: Option<usize>) -> bool {
        match self.braces.last() {
            Some(&open) => boundary.is_some_and(|body| open <= body),
            None => true,
        }
    }

    /// Item recovery: synchronize at brace depth zero of the enclosing module,
    /// trait, impl or mod body. Parentheses/brackets do not change the grammar's
    /// specified brace-depth test; nested curly groups do.
    fn sync_item(&mut self, start: usize, boundary: Option<(usize, usize)>) {
        self.suppress = true;
        let open = boundary.map(|(open, _)| open);
        loop {
            let t = self.tok();
            if t.kind == Kind::Eof
                || self.closes_boundary_or_outer(t, open)
                || (boundary.is_some() && self.at_item_brace_depth(open) && self.is_p(t, "}"))
            {
                return;
            }
            if self.at_item_brace_depth(open)
                && self.live(self.pos) > start
                && self.at_line_start()
                && self.at_item_starter()
            {
                return;
            }
            self.skip_group_or_one();
        }
    }

    /// A parse of `T` that yields `placeholder` after recovering from an error.
    fn error_expr(&self, start: u32) -> Expr {
        Expr {
            span: self.span_from(start),
            kind: ExprKind::Error,
        }
    }

    fn at_item_starter(&self) -> bool {
        let t = self.tok();
        t.kind == Kind::DocComment
            || self.is_p(t, "@")
            || [
                "fn", "async", "comptime", "kernel", "struct", "enum", "trait", "impl", "type",
                "const", "use", "mod", "pub",
            ]
            .iter()
            .any(|k| self.is_kw(t, k))
            || (self.is_id(t, "extern") && self.nth(1).kind == Kind::Str)
    }

    // ------------------------------------------------------------ module and items

    fn module(&mut self) -> Module {
        let mut items = Vec::new();
        loop {
            self.skip_seps();
            if self.at_eof() {
                break;
            }
            let start = self.live(self.pos);
            let first = self.tok().start;
            match self.item(Ctx::Module) {
                Ok(item) => {
                    items.push(item);
                    if !(self.at_sep() || self.at_eof()) {
                        let _ = self.err::<()>("P001");
                        let before_sync = self.live(self.pos);
                        self.sync_item(start, None);
                        self.ensure_module_recovery_progress(before_sync);
                    }
                }
                Err(()) => {
                    let before_sync = self.live(self.pos);
                    self.sync_item(start, None);
                    self.ensure_module_recovery_progress(before_sync);
                    self.fatal_recovery = false;
                    items.push(self.error_item(first));
                }
            }
        }
        Module { items }
    }

    fn error_item(&self, start: u32) -> Item {
        Item {
            span: self.span_from(start),
            docs: Vec::new(),
            attrs: Vec::new(),
            is_pub: false,
            kind: ItemKind::Error,
        }
    }

    fn item(&mut self, ctx: Ctx) -> PResult<Item> {
        let start = self.tok().start;
        let mut docs = Vec::new();
        while self.tok().kind == Kind::DocComment {
            let t = self.bump();
            docs.push((t.start, t.end));
        }
        let mut attrs = Vec::new();
        while self.at_p("@") {
            attrs.push(self.attribute()?);
        }
        if !docs.is_empty() && (self.at_eof() || self.at_p("}")) {
            let (a, b) = docs[0];
            return self.err_at("P008", a, b - a);
        }
        let is_pub = self.at_kw("pub");
        if is_pub {
            if ctx == Ctx::Trait {
                // trait_item has no `pub` (4.3).
                return self.err("P001");
            }
            self.bump();
        }
        let t = self.tok();
        let module_only = ["use", "mod", "struct", "enum", "trait", "impl"]
            .iter()
            .any(|k| self.is_kw(t, k))
            || (self.is_id(t, "extern") && self.nth(1).kind == Kind::Str);
        if module_only && ctx != Ctx::Module {
            return self.err("P012");
        }
        let kind = if self.at_kw("use") {
            self.bump();
            ItemKind::Use(self.use_tree()?)
        } else if self.at_kw("mod") {
            self.bump();
            let name = self.name()?;
            let items = if self.at_p("{") {
                Some(self.item_body(Ctx::Module)?)
            } else {
                None
            };
            ItemKind::Mod { name, items }
        } else if ["fn", "async", "comptime", "kernel"]
            .iter()
            .any(|k| self.at_kw(k))
        {
            ItemKind::Fn(self.fn_decl(ctx)?)
        } else if self.at_id("extern") && self.nth(1).kind == Kind::Str {
            self.bump();
            let abi = self.bump_text().to_string();
            let sig = self.fn_sig_after_prefix(None)?;
            ItemKind::ExternFn { abi, sig }
        } else if self.at_kw("struct") {
            self.bump();
            let name = self.name()?;
            let generics = self.opt_generic_params()?;
            let where_ = self.opt_where()?;
            let fields = self.brace_list(|p| p.field())?;
            ItemKind::Struct {
                name,
                generics,
                where_,
                fields,
            }
        } else if self.at_kw("enum") {
            self.bump();
            let name = self.name()?;
            let generics = self.opt_generic_params()?;
            let where_ = self.opt_where()?;
            let variants = self.brace_list(|p| p.variant())?;
            ItemKind::Enum {
                name,
                generics,
                where_,
                variants,
            }
        } else if self.at_kw("trait") {
            self.bump();
            let name = self.name()?;
            let generics = self.opt_generic_params()?;
            let mut supertraits = Vec::new();
            if self.at_p(":") {
                self.bump();
                supertraits = self.bounds()?;
            }
            let where_ = self.opt_where()?;
            let items = self.item_body(Ctx::Trait)?;
            ItemKind::Trait {
                name,
                generics,
                supertraits,
                where_,
                items,
            }
        } else if self.at_kw("impl") {
            self.bump();
            let generics = self.opt_generic_params()?;
            let first = self.ty()?;
            let (trait_, ty) = if self.at_kw("for") {
                // Rule E: the type before `for` must be a path_type.
                if !matches!(first.kind, TypeKind::Path { .. }) {
                    return self.err_at("P001", first.span.0, first.span.1 - first.span.0);
                }
                self.bump();
                (Some(first), self.ty()?)
            } else {
                (None, first)
            };
            let where_ = self.opt_where()?;
            let items = self.item_body(Ctx::Impl)?;
            ItemKind::Impl {
                generics,
                trait_,
                ty,
                where_,
                items,
            }
        } else if self.at_kw("type") {
            self.bump();
            let name = self.name()?;
            if ctx == Ctx::Trait {
                let mut bounds = Vec::new();
                if self.at_p(":") {
                    self.bump();
                    bounds = self.bounds()?;
                }
                ItemKind::AssocType { name, bounds }
            } else {
                let generics = self.opt_generic_params()?;
                self.expect_p("=")?;
                let ty = self.ty()?;
                let refinement = if self.at_kw("where") {
                    self.bump();
                    Some(self.expr(false)?)
                } else {
                    None
                };
                ItemKind::TypeAlias {
                    name,
                    generics,
                    ty,
                    refinement,
                }
            }
        } else if self.at_kw("const") {
            self.bump();
            let name = self.name()?;
            self.expect_p(":")?;
            let ty = self.ty()?;
            let value = if self.at_p("=") {
                self.bump();
                Some(self.expr(true)?)
            } else if ctx == Ctx::Trait {
                None
            } else {
                return self.err("P001");
            };
            ItemKind::Const { name, ty, value }
        } else {
            return self.err("P001");
        };
        Ok(Item {
            span: self.span_from(start),
            docs,
            attrs,
            is_pub,
            kind,
        })
    }

    /// `{ stmt_sep? (item (stmt_sep item)* stmt_sep?)? }` for mod, impl and trait bodies.
    fn item_body(&mut self, ctx: Ctx) -> PResult<Vec<Item>> {
        let open = self.live(self.pos);
        self.expect_p("{")?;
        let base_depth = self.delimiters.len();
        let mut items = Vec::new();
        loop {
            self.skip_seps();
            if self.at_eof() || self.is_boundary_close("}", open) {
                break;
            }
            if self.recovery_stopped_at_outer(open) {
                return self.err_unexpected_closer();
            }
            let start = self.live(self.pos);
            let first = self.tok().start;
            match self.item(ctx) {
                Ok(item) => {
                    items.push(item);
                    if !(self.at_sep() || self.is_boundary_close("}", open)) {
                        if self.recovery_stopped_at_outer(open) {
                            return self.err_unexpected_closer();
                        }
                        let _ = self.expect_close("}");
                        let before_sync = self.live(self.pos);
                        self.sync_item(start, Some((open, base_depth)));
                        self.ensure_recovery_progress(before_sync, open, "}", false, true)?;
                    }
                }
                Err(()) => {
                    let before_sync = self.live(self.pos);
                    self.sync_item(start, Some((open, base_depth)));
                    self.ensure_recovery_progress(before_sync, open, "}", false, true)?;
                    if self.recovery_stopped_at_outer(open) {
                        return self.err_unexpected_closer();
                    }
                    self.fatal_recovery = false;
                    items.push(self.error_item(first));
                }
            }
            if self.recovery_stopped_at_outer(open) {
                return self.err_unexpected_closer();
            }
        }
        self.expect_close("}")?;
        Ok(items)
    }

    fn attribute(&mut self) -> PResult<Attribute> {
        let start = self.bump().start; // @
        let path = self.path()?;
        let mut args = Vec::new();
        if self.at_p("(") {
            self.bump();
            while !self.at_p(")") {
                let name = if Self::is_name(self.tok()) && self.is_p(self.nth(1), ":") {
                    let n = self.name()?;
                    self.bump();
                    Some(n)
                } else {
                    None
                };
                let value = self.expr(true)?;
                args.push(Arg {
                    name,
                    marker: None,
                    value,
                });
                if self.at_p(",") {
                    self.bump();
                } else {
                    break;
                }
            }
            self.expect_close(")")?;
        }
        if self.tok().kind == Kind::Newline {
            self.bump();
        }
        Ok(Attribute {
            span: self.span_from(start),
            path,
            args,
        })
    }

    /// path ::= path_start ( "." name )*
    fn path(&mut self) -> PResult<Vec<String>> {
        let mut segs = vec![self.path_start()?];
        while self.at_p(".") && Self::is_name(self.nth(1)) {
            self.bump();
            segs.push(self.name()?);
        }
        Ok(segs)
    }

    fn path_start(&mut self) -> PResult<String> {
        if self.at_kw("self") || self.at_kw("Self") {
            let t = self.bump();
            Ok(self.text(t).to_string())
        } else {
            self.name()
        }
    }

    fn use_tree(&mut self) -> PResult<UseTree> {
        let mut segs = vec![self.path_start()?];
        while self.at_p(".") && Self::is_name(self.nth(1)) {
            self.bump();
            segs.push(self.name()?);
        }
        if self.at_kw("as") {
            self.bump();
            return Ok(UseTree::Path {
                path: segs,
                alias: Some(self.name()?),
            });
        }
        if self.at_p(".") && self.is_p(self.nth(1), "{") {
            self.bump();
            let trees = self.brace_list(|p| p.use_tree())?;
            return Ok(UseTree::Group {
                prefix: segs,
                trees,
            });
        }
        if self.at_p(".") && self.is_p(self.nth(1), "*") {
            self.bump();
            self.bump();
            self.wake_newline();
            return Ok(UseTree::Glob { prefix: segs });
        }
        Ok(UseTree::Path {
            path: segs,
            alias: None,
        })
    }

    /// `{ NEWLINE? list(x, brace_sep)? }` (grammar 3, newlines as list separators).
    fn brace_list<T>(&mut self, mut f: impl FnMut(&mut Self) -> PResult<T>) -> PResult<Vec<T>> {
        let open = self.live(self.pos);
        self.expect_p("{")?;
        let base_depth = self.delimiters.len();
        self.skip_newlines();
        let mut out = Vec::new();
        while !self.at_eof() && !self.is_boundary_close("}", open) {
            if self.recovery_stopped_at_outer(open) {
                return self.err_unexpected_closer();
            }
            match f(self) {
                Ok(v) => out.push(v),
                Err(()) if self.fatal_recovery => return Err(()),
                Err(()) => {
                    let before_sync = self.live(self.pos);
                    self.sync_brace_list(open, base_depth);
                    self.ensure_recovery_progress(before_sync, open, "}", true, false)?;
                    if self.recovery_stopped_at_outer(open) {
                        return self.err_unexpected_closer();
                    }
                }
            }
            if self.at_p(",") {
                self.bump();
                self.skip_newlines();
            } else if self.tok().kind == Kind::Newline {
                self.skip_newlines();
            } else if !self.is_boundary_close("}", open) && !self.at_eof() {
                break;
            }
        }
        if self.recovery_stopped_at_outer(open) {
            return self.err_unexpected_closer();
        }
        self.expect_close("}")?;
        Ok(out)
    }

    /// `( list(x, ",")? )` with a trailing comma allowed. A failed element is
    /// dropped after recovery to the next `,` or closer.
    fn paren_list<T>(
        &mut self,
        open: &str,
        close: &str,
        f: impl FnMut(&mut Self) -> PResult<T>,
    ) -> PResult<Vec<T>> {
        self.paren_list_r(open, close, f, |_, _| None)
    }

    /// As `paren_list`, but a failed element is replaced by `placeholder`
    /// (an Error expression, for argument lists).
    fn paren_list_r<T>(
        &mut self,
        open: &str,
        close: &str,
        mut f: impl FnMut(&mut Self) -> PResult<T>,
        placeholder: impl Fn(&Self, u32) -> Option<T>,
    ) -> PResult<Vec<T>> {
        let boundary = self.live(self.pos);
        self.expect_p(open)?;
        let base_depth = self.delimiters.len();
        let mut out = Vec::new();
        while !self.at_eof() && !self.is_boundary_close(close, boundary) {
            if self.recovery_stopped_at_outer(boundary) {
                return self.err_unexpected_closer();
            }
            let first = self.tok().start;
            match f(self) {
                Ok(v) => out.push(v),
                Err(()) if self.fatal_recovery => return Err(()),
                Err(()) => {
                    let before_sync = self.live(self.pos);
                    self.sync_list(boundary, base_depth);
                    self.ensure_recovery_progress(before_sync, boundary, close, true, false)?;
                    if self.recovery_stopped_at_outer(boundary) {
                        return self.err_unexpected_closer();
                    }
                    out.extend(placeholder(self, first));
                }
            }
            if self.at_p(",") {
                self.bump();
            } else {
                break;
            }
        }
        if self.recovery_stopped_at_outer(boundary) {
            return self.err_unexpected_closer();
        }
        self.expect_close(close)?;
        Ok(out)
    }

    /// `DOC_COMMENT* attribute*`, for fields and variants. A doc comment with
    /// nothing after it is P008.
    fn docs_and_attrs(&mut self) -> PResult<(Vec<Span>, Vec<Attribute>)> {
        let mut docs = Vec::new();
        while self.tok().kind == Kind::DocComment {
            let t = self.bump();
            docs.push((t.start, t.end));
        }
        let mut attrs = Vec::new();
        while self.at_p("@") {
            attrs.push(self.attribute()?);
        }
        if let Some(&(a, b)) = docs.first() {
            if self.at_p("}") || self.at_eof() {
                return self.err_at("P008", a, b - a);
            }
        }
        Ok((docs, attrs))
    }

    fn field(&mut self) -> PResult<Field> {
        let start = self.tok().start;
        let (docs, attrs) = self.docs_and_attrs()?;
        let is_pub = self.at_kw("pub");
        if is_pub {
            self.bump();
        }
        let name = self.name()?;
        self.expect_p(":")?;
        let ty = self.ty()?;
        Ok(Field {
            span: self.span_from(start),
            docs,
            attrs,
            is_pub,
            name,
            ty,
        })
    }

    fn variant(&mut self) -> PResult<Variant> {
        let start = self.tok().start;
        let (docs, attrs) = self.docs_and_attrs()?;
        let name = self.name()?;
        let kind = if self.at_p("(") {
            VariantKind::Tuple(self.paren_list("(", ")", |p| {
                if Self::is_name(p.tok()) && p.is_p(p.nth(1), ":") {
                    let n = p.name()?;
                    p.bump();
                    Ok((Some(n), p.ty()?))
                } else {
                    Ok((None, p.ty()?))
                }
            })?)
        } else if self.at_p("{") {
            VariantKind::Struct(self.brace_list(|p| p.field())?)
        } else {
            VariantKind::Unit
        };
        Ok(Variant {
            span: self.span_from(start),
            docs,
            attrs,
            name,
            kind,
        })
    }

    fn fn_decl(&mut self, ctx: Ctx) -> PResult<FnDecl> {
        let mut prefix = None;
        if ["async", "comptime", "kernel"]
            .iter()
            .any(|k| self.at_kw(k))
        {
            let t = self.bump();
            prefix = Some(self.text(t).to_string());
            if ["async", "comptime", "kernel"]
                .iter()
                .any(|k| self.at_kw(k))
            {
                return self.err("P007");
            }
        }
        let sig = self.fn_sig_after_prefix(prefix)?;
        let body = if self.at_p("{") {
            Some(FnBody::Block(self.block()?))
        } else if self.at_p("=") {
            self.bump();
            Some(FnBody::Expr(self.expr(true)?))
        } else if ctx == Ctx::Trait {
            None
        } else {
            return self.err("P006");
        };
        Ok(FnDecl { sig, body })
    }

    fn fn_sig_after_prefix(&mut self, prefix: Option<String>) -> PResult<FnSig> {
        self.expect_kw("fn")?;
        let name = self.name()?;
        let generics = self.opt_generic_params()?;
        let params = self.paren_list("(", ")", |p| p.param())?;
        let ret = if self.at_p("->") {
            self.bump();
            let borrow = self.at_kw("in");
            if borrow {
                self.bump();
            }
            Some(RetType {
                borrow,
                ty: self.ty()?,
            })
        } else {
            None
        };
        let effects = self.opt_effects()?;
        let mut clauses = Vec::new();
        loop {
            if self.at_kw("requires") {
                self.bump();
                clauses.push(Clause::Requires(self.expr(false)?));
            } else if self.at_kw("ensures") {
                self.bump();
                clauses.push(Clause::Ensures(self.expr(false)?));
            } else if self.at_kw("where") {
                self.bump();
                clauses.push(Clause::Where(self.where_preds()?));
            } else {
                break;
            }
        }
        Ok(FnSig {
            prefix,
            name,
            generics,
            params,
            ret,
            effects,
            clauses,
        })
    }

    fn opt_effects(&mut self) -> PResult<Option<Vec<String>>> {
        if self.at_id("effects") && self.is_p(self.nth(1), "(") {
            self.bump();
            Ok(Some(self.paren_list("(", ")", |p| p.name())?))
        } else {
            Ok(None)
        }
    }

    fn param(&mut self) -> PResult<Param> {
        let t = self.tok();
        if self.is_kw(t, "self") {
            self.bump();
            return Ok(Param::SelfParam { mode: None });
        }
        let is_mode = self.is_kw(t, "in") || self.is_id(t, "inout") || self.is_id(t, "own");
        if is_mode && self.is_kw(self.nth(1), "self") {
            let m = self.bump_text().to_string();
            self.bump();
            return Ok(Param::SelfParam { mode: Some(m) });
        }
        let var = self.at_kw("var");
        if var {
            self.bump();
        }
        let name = self.name()?;
        self.expect_p(":")?;
        let mut mode = None;
        let t = self.tok();
        let next = self.nth(1);
        let next_ends = self.is_p(next, ",") || self.is_p(next, ")");
        if self.is_kw(t, "in") || ((self.is_id(t, "inout") || self.is_id(t, "own")) && !next_ends) {
            mode = Some(self.bump_text().to_string());
        }
        let ty = self.ty()?;
        Ok(Param::Named {
            var,
            name,
            mode,
            ty,
        })
    }

    fn opt_generic_params(&mut self) -> PResult<Vec<GenericParam>> {
        if !self.at_p("<") {
            return Ok(Vec::new());
        }
        self.bump();
        let mut out = Vec::new();
        loop {
            if self.at_kw("const") {
                self.bump();
                let name = self.name()?;
                self.expect_p(":")?;
                out.push(GenericParam::Const {
                    name,
                    ty: self.ty()?,
                });
            } else {
                let name = self.name()?;
                let bounds = if self.at_p(":") {
                    self.bump();
                    self.bounds()?
                } else {
                    Vec::new()
                };
                let default = if self.at_p("=") {
                    self.bump();
                    Some(self.ty()?)
                } else {
                    None
                };
                out.push(GenericParam::Type {
                    name,
                    bounds,
                    default,
                });
            }
            if self.at_p(",") {
                self.bump();
                if self.text(self.tok()).starts_with('>') {
                    break;
                }
            } else {
                break;
            }
        }
        self.expect_gt()?;
        Ok(out)
    }

    fn bounds(&mut self) -> PResult<Vec<Type>> {
        let mut out = vec![self.path_type()?];
        while self.at_p("+") {
            self.bump();
            out.push(self.path_type()?);
        }
        Ok(out)
    }

    fn opt_where(&mut self) -> PResult<Vec<WherePred>> {
        if self.at_kw("where") {
            self.bump();
            self.where_preds()
        } else {
            Ok(Vec::new())
        }
    }

    /// Can the token start a `type` (4.4)? Decides whether a `,` after a
    /// where-predicate continues the list.
    fn starts_type(&self, t: PTok) -> bool {
        Self::is_name(t)
            || self.is_kw(t, "self")
            || self.is_kw(t, "Self")
            || self.is_kw(t, "fn")
            || self.is_p(t, "(")
            || self.is_p(t, "[")
            || self.is_p(t, "*")
    }

    fn where_preds(&mut self) -> PResult<Vec<WherePred>> {
        let mut out = Vec::new();
        loop {
            let ty = self.ty()?;
            self.expect_p(":")?;
            out.push(WherePred {
                ty,
                bounds: self.bounds()?,
            });
            if self.at_p(",") && self.starts_type(self.nth(1)) {
                self.bump();
            } else {
                break;
            }
        }
        Ok(out)
    }

    // ------------------------------------------------------------ types

    fn ty(&mut self) -> PResult<Type> {
        self.nested(|p| p.ty_inner())
    }

    fn ty_inner(&mut self) -> PResult<Type> {
        let start = self.tok().start;
        let mut kind = if self.at_id("dyn") {
            self.bump();
            TypeKind::Dyn(self.bounds()?)
        } else if self.at_kw("fn") {
            self.bump();
            let params = self.paren_list("(", ")", |p| p.ty())?;
            let ret = if self.at_p("->") {
                self.bump();
                Some(Box::new(self.ty()?))
            } else {
                None
            };
            let effects = self.opt_effects()?;
            TypeKind::Fn {
                params,
                ret,
                effects,
            }
        } else if self.at_p("*") {
            self.bump();
            let var = if self.at_kw("const") {
                false
            } else if self.at_kw("var") {
                true
            } else {
                return self.err("P001");
            };
            self.bump();
            TypeKind::Pointer {
                var,
                ty: Box::new(self.ty()?),
            }
        } else if self.at_p("(") {
            self.bump();
            if self.at_p(")") {
                self.bump();
                TypeKind::Tuple(Vec::new())
            } else {
                let first = self.ty()?;
                if self.at_p(",") {
                    let mut elems = vec![first];
                    while self.at_p(",") {
                        self.bump();
                        if self.at_p(")") {
                            break;
                        }
                        elems.push(self.ty()?);
                    }
                    self.expect_close(")")?;
                    TypeKind::Tuple(elems)
                } else {
                    self.expect_close(")")?;
                    TypeKind::Paren(Box::new(first))
                }
            }
        } else if self.at_p("[") {
            self.bump();
            let elem = self.ty()?;
            self.expect_p(";")?;
            let len = self.expr(true)?;
            self.expect_close("]")?;
            TypeKind::Array(Box::new(elem), Box::new(len))
        } else {
            return self.path_type();
        };
        while self.at_p("?") {
            self.bump();
            let inner = Type {
                span: self.span_from(start),
                text: self.slice(start),
                kind,
            };
            kind = TypeKind::Optional(Box::new(inner));
        }
        Ok(Type {
            span: self.span_from(start),
            text: self.slice(start),
            kind,
        })
    }

    fn path_type(&mut self) -> PResult<Type> {
        let start = self.tok().start;
        let path = self.path()?;
        let args = if self.at_p("<") {
            self.generic_args()?
        } else {
            Vec::new()
        };
        let mut kind = TypeKind::Path { path, args };
        while self.at_p("?") {
            self.bump();
            let inner = Type {
                span: self.span_from(start),
                text: self.slice(start),
                kind,
            };
            kind = TypeKind::Optional(Box::new(inner));
        }
        Ok(Type {
            span: self.span_from(start),
            text: self.slice(start),
            kind,
        })
    }

    fn generic_args(&mut self) -> PResult<Vec<GenericArg>> {
        self.bump(); // <
        if self.text(self.tok()).starts_with('>') {
            return self.err("P001"); // the list requires at least one argument
        }
        let mut out = Vec::new();
        loop {
            out.push(self.generic_arg()?);
            if self.at_p(",") {
                self.bump();
                // list(x, ",") permits one trailing separator.
                if self.text(self.tok()).starts_with('>') {
                    break;
                }
            } else {
                break;
            }
        }
        self.expect_gt()?;
        Ok(out)
    }

    fn generic_arg(&mut self) -> PResult<GenericArg> {
        let t = self.tok();
        let n = self.nth(1);
        let start = t.start;
        if Self::is_name(t) && (self.is_p(n, "*") || self.is_p(n, "/") || self.is_p(n, "**")) {
            self.unit_expr()?;
            return Ok(GenericArg::Unit(self.slice(start)));
        }
        if t.kind == Kind::Int
            || (self.is_p(t, "-") && n.kind == Kind::Int)
            || self.is_kw(t, "true")
            || self.is_kw(t, "false")
        {
            if self.is_p(t, "-") {
                self.bump();
            }
            self.bump();
            return Ok(GenericArg::Const(self.slice(start)));
        }
        if self.is_p(t, "{") {
            self.bump();
            self.expr(true)?;
            self.expect_close("}")?;
            return Ok(GenericArg::Const(self.slice(start)));
        }
        Ok(GenericArg::Type(self.ty()?))
    }

    fn unit_expr(&mut self) -> PResult<()> {
        self.name()?;
        self.opt_unit_power()?;
        while self.at_p("*") || self.at_p("/") {
            self.bump();
            if self.at_p("(") {
                self.bump();
                self.unit_expr()?;
                self.expect_close(")")?;
            } else {
                self.name()?;
                self.opt_unit_power()?;
            }
        }
        Ok(())
    }

    fn opt_unit_power(&mut self) -> PResult<()> {
        if self.at_p("**") {
            self.bump();
            if self.at_p("-") {
                self.bump();
            }
            if self.tok().kind != Kind::Int {
                return self.err("P001");
            }
            self.bump();
        }
        Ok(())
    }

    // ------------------------------------------------------------ statements

    fn block(&mut self) -> PResult<Block> {
        let boundary = self.live(self.pos);
        let start = self.expect_p("{")?.start;
        let base_depth = self.delimiters.len();
        let mut stmts = Vec::new();
        loop {
            self.skip_seps();
            if self.at_eof() || self.is_boundary_close("}", boundary) {
                break;
            }
            if self.recovery_stopped_at_outer(boundary) {
                return self.err_unexpected_closer();
            }
            let first = self.tok().start;
            match self.stmt() {
                Ok(stmt) => stmts.push(stmt),
                Err(()) if self.fatal_recovery => return Err(()),
                Err(()) => {
                    let before_sync = self.live(self.pos);
                    self.sync_stmt(boundary, base_depth);
                    self.ensure_recovery_progress(before_sync, boundary, "}", false, false)?;
                    if self.recovery_stopped_at_outer(boundary) {
                        return self.err_unexpected_closer();
                    }
                    stmts.push(Stmt::Error(self.span_from(first)));
                }
            }
            if !(self.at_sep() || self.is_boundary_close("}", boundary) || self.at_eof()) {
                // Something other than a separator follows a complete statement.
                // Do not let an ancestor's `}` masquerade as this block's close.
                if self.recovery_stopped_at_outer(boundary) {
                    return self.err_unexpected_closer();
                }
                let _ = self.expect_close("}");
                if self.fatal_recovery {
                    return Err(());
                }
                if self.recovery_stopped_at_outer(boundary) {
                    return self.err_unexpected_closer();
                }
                let before_sync = self.live(self.pos);
                self.sync_stmt(boundary, base_depth);
                self.ensure_recovery_progress(before_sync, boundary, "}", false, false)?;
                if self.recovery_stopped_at_outer(boundary) {
                    return self.err_unexpected_closer();
                }
            }
        }
        self.expect_close("}")?;
        Ok(Block {
            span: self.span_from(start),
            stmts,
        })
    }

    fn is_assign_op(&self, t: PTok) -> bool {
        t.kind == Kind::Punct
            && [
                "=", "+=", "-=", "*=", "/=", "%=", "&=", "|=", "^=", "<<=", ">>=", "**=",
            ]
            .contains(&self.text(t))
    }

    fn stmt(&mut self) -> PResult<Stmt> {
        let mut attrs = Vec::new();
        while self.at_p("@") {
            attrs.push(self.attribute()?);
        }
        let t = self.tok();
        if t.kind == Kind::DocComment {
            return self.err("P008");
        }
        if self.at_kw("let") || self.at_kw("var") {
            let var = self.at_kw("var");
            self.bump();
            let pat = self.pattern()?;
            let ty = if self.at_p(":") {
                self.bump();
                Some(self.ty()?)
            } else {
                None
            };
            self.expect_p("=")?;
            let value = self.expr(true)?;
            return Ok(Stmt::Let {
                attrs,
                var,
                pat,
                ty,
                value,
            });
        }
        if self.at_kw("const") {
            self.bump();
            let name = self.name()?;
            self.expect_p(":")?;
            let ty = self.ty()?;
            self.expect_p("=")?;
            return Ok(Stmt::Const {
                name,
                ty,
                value: self.expr(true)?,
            });
        }
        if self.at_kw("defer") {
            self.bump();
            return Ok(Stmt::Defer(self.expr(true)?));
        }
        if self.at_id("region") && Self::is_name(self.nth(1)) && self.is_p(self.nth(2), "{") {
            self.bump();
            let name = self.name()?;
            return Ok(Stmt::Region {
                name,
                body: self.block()?,
            });
        }
        let item_start = [
            "fn", "struct", "enum", "trait", "impl", "type", "mod", "use",
        ]
        .iter()
        .any(|k| self.is_kw(t, k))
            || (["async", "comptime", "kernel"]
                .iter()
                .any(|k| self.is_kw(t, k))
                && self.is_kw(self.nth(1), "fn"));
        if item_start {
            return self.err("P013");
        }
        let target = self.expr(true)?;
        if self.is_assign_op(self.tok()) {
            let op = self.bump_text().to_string();
            let value = self.expr(true)?;
            if self.is_assign_op(self.tok()) {
                return self.err("P015");
            }
            return Ok(Stmt::Assign { target, op, value });
        }
        Ok(Stmt::Expr(target))
    }

    // ------------------------------------------------------------ patterns

    fn pattern(&mut self) -> PResult<Pattern> {
        self.nested(|p| p.pattern_inner())
    }

    fn pattern_inner(&mut self) -> PResult<Pattern> {
        let start = self.tok().start;
        if self.at_p("|") {
            self.bump();
        }
        let first = self.pattern_alt()?;
        if !self.at_p("|") {
            return Ok(first);
        }
        let mut alts = vec![first];
        while self.at_p("|") {
            self.bump();
            alts.push(self.pattern_alt()?);
        }
        Ok(Pattern {
            span: self.span_from(start),
            kind: PatternKind::Or(alts),
        })
    }

    fn range_bound(&mut self) -> PResult<String> {
        let start = self.tok().start;
        if self.at_p("-") {
            self.bump();
            if self.tok().kind != Kind::Int {
                return self.err("P001");
            }
            self.bump();
        } else if matches!(self.tok().kind, Kind::Int | Kind::Char) {
            self.bump();
        } else {
            self.path()?;
        }
        Ok(self.slice(start))
    }

    fn at_range_bound(&self) -> bool {
        let t = self.tok();
        matches!(t.kind, Kind::Int | Kind::Char)
            || Self::is_name(t)
            || (self.is_p(t, "-") && self.nth(1).kind == Kind::Int)
    }

    fn range_tail(&mut self, lo: Option<String>, start: u32) -> PResult<Pattern> {
        let inclusive = self.at_p("..=");
        self.bump();
        let hi = if inclusive || self.at_range_bound() {
            Some(self.range_bound()?)
        } else {
            None
        };
        Ok(Pattern {
            span: self.span_from(start),
            kind: PatternKind::Range { lo, hi, inclusive },
        })
    }

    fn pattern_alt(&mut self) -> PResult<Pattern> {
        let t = self.tok();
        let start = t.start;
        let pat = |p: &Self, kind| Pattern {
            span: p.span_from(start),
            kind,
        };
        if t.kind == Kind::Underscore {
            self.bump();
            return Ok(pat(self, PatternKind::Wildcard));
        }
        if self.is_p(t, "..") {
            self.bump();
            return Ok(pat(self, PatternKind::Rest));
        }
        if self.is_p(t, "..=") {
            return self.range_tail(None, start);
        }
        if self.is_kw(t, "var") {
            self.bump();
            let name = self.name()?;
            return Ok(pat(self, PatternKind::Ident { var: true, name }));
        }
        if self.is_id(t, "mut") && Self::is_name(self.nth(1)) {
            return self.err("P011");
        }
        let neg_int = self.is_p(t, "-") && self.nth(1).kind == Kind::Int;
        if matches!(t.kind, Kind::Int | Kind::Char) || neg_int {
            let lo = self.range_bound()?;
            if self.at_p("..") || self.at_p("..=") {
                return self.range_tail(Some(lo), start);
            }
            return Ok(pat(self, PatternKind::Literal(lo)));
        }
        if matches!(t.kind, Kind::Str | Kind::ByteStr)
            || self.is_kw(t, "true")
            || self.is_kw(t, "false")
        {
            if t.kind == Kind::Str && !self.lexed_hole_free(t) {
                return self.err("P001");
            }
            self.bump();
            return Ok(pat(self, PatternKind::Literal(self.slice(start))));
        }
        if self.is_p(t, "(") {
            self.bump();
            if self.at_p(")") {
                self.bump();
                return Ok(pat(self, PatternKind::Tuple(Vec::new())));
            }
            let first = self.pattern()?;
            if !self.at_p(",") {
                self.expect_close(")")?;
                return Ok(first);
            }
            let mut elems = vec![first];
            while self.at_p(",") {
                self.bump();
                if self.at_p(")") {
                    break;
                }
                elems.push(self.pattern()?);
            }
            self.expect_close(")")?;
            return Ok(pat(self, PatternKind::Tuple(elems)));
        }
        if self.is_p(t, "[") {
            let elems = self.paren_list("[", "]", |p| p.pattern())?;
            return Ok(pat(self, PatternKind::Array(elems)));
        }
        if Self::is_name(t) || self.is_kw(t, "self") || self.is_kw(t, "Self") {
            let path = self.path()?;
            if self.at_p("(") {
                let elems = self.paren_list("(", ")", |p| p.pattern())?;
                return Ok(pat(self, PatternKind::Variant { path, elems }));
            }
            if self.at_p("{") {
                let mut rest = false;
                let fields = self.brace_list(|p| {
                    if p.at_p("..") {
                        p.bump();
                        rest = true;
                        return Ok(None);
                    }
                    let n = p.name()?;
                    let sub = if p.at_p(":") {
                        p.bump();
                        Some(p.pattern()?)
                    } else {
                        None
                    };
                    Ok(Some((n, sub)))
                })?;
                let fields = fields.into_iter().flatten().collect();
                return Ok(pat(self, PatternKind::Struct { path, fields, rest }));
            }
            if self.at_p("..") || self.at_p("..=") {
                return self.range_tail(Some(path.join(".")), start);
            }
            if path.len() == 1 && Self::is_name(t) {
                return Ok(pat(
                    self,
                    PatternKind::Ident {
                        var: false,
                        name: path[0].clone(),
                    },
                ));
            }
            return self.err("P001");
        }
        self.err("P001")
    }

    /// A STRING_LIT in a pattern may not contain interpolation holes (5.2).
    fn lexed_hole_free(&self, t: PTok) -> bool {
        t.lex
            .map(|i| self.lexed_tokens[i].holes.is_empty())
            .unwrap_or(true)
    }

    // ------------------------------------------------------------ expressions

    fn starts_expr(&self, t: PTok) -> bool {
        match t.kind {
            Kind::Eof | Kind::Newline | Kind::DocComment | Kind::Underscore => false,
            Kind::Punct => ["(", "[", "{", "-", "!", "~", "+", "..", "..="].contains(&self.text(t)),
            Kind::Keyword => [
                "if", "match", "while", "loop", "for", "return", "break", "continue", "unsafe",
                "comptime", "await", "async", "self", "Self", "true", "false",
            ]
            .contains(&self.text(t)),
            _ => true,
        }
    }

    /// Rule C: is the `(` at the cursor the start of closure parameters?
    fn paren_is_closure(&self, at: usize) -> bool {
        let t = self.toks[at.min(self.toks.len() - 1)];
        match t.close {
            Some(c) if self.is_p(t, "(") => {
                self.toks.get(c + 1).is_some_and(|n| self.is_p(*n, "=>"))
            }
            _ => false,
        }
    }

    pub(crate) fn expr(&mut self, s: bool) -> PResult<Expr> {
        self.nested(|p| p.expr_inner(s))
    }

    fn expr_inner(&mut self, s: bool) -> PResult<Expr> {
        let t = self.tok();
        let is_async = self.is_kw(t, "async");
        let here = self.live(self.pos);
        let at = if is_async { self.live(here + 1) } else { here };
        let first = self.toks[at];
        let second = self.toks[self.live(at + 1)];
        if (Self::is_name(first) && self.is_p(second, "=>")) || self.paren_is_closure(at) {
            return self.closure(s);
        }
        self.range_expr(s)
    }

    fn closure(&mut self, s: bool) -> PResult<Expr> {
        let start = self.tok().start;
        let is_async = self.at_kw("async");
        if is_async {
            self.bump();
        }
        let params = if self.at_p("(") {
            self.paren_list("(", ")", |p| {
                let pat = p.pattern()?;
                let ty = if p.at_p(":") {
                    p.bump();
                    Some(p.ty()?)
                } else {
                    None
                };
                Ok((pat, ty))
            })?
        } else {
            let pstart = self.tok().start;
            let name = self.name()?;
            vec![(
                Pattern {
                    span: self.span_from(pstart),
                    kind: PatternKind::Ident { var: false, name },
                },
                None,
            )]
        };
        self.expect_p("=>")?;
        let body = self.expr(s)?;
        Ok(Expr {
            span: self.span_from(start),
            kind: ExprKind::Closure {
                is_async,
                params,
                body: Box::new(body),
            },
        })
    }

    fn range_expr(&mut self, s: bool) -> PResult<Expr> {
        let start = self.tok().start;
        let rhs_starts = |p: &Self| p.starts_expr(p.tok()) && !(p.at_p("{") && !s);
        if self.at_p("..") || self.at_p("..=") {
            let inclusive = self.at_p("..=");
            self.bump();
            let hi = if rhs_starts(self) {
                Some(Box::new(self.binary(s, 0)?))
            } else {
                None
            };
            return Ok(Expr {
                span: self.span_from(start),
                kind: ExprKind::Range {
                    lo: None,
                    hi,
                    inclusive,
                },
            });
        }
        let lo = self.binary(s, 0)?;
        if !(self.at_p("..") || self.at_p("..=")) {
            return Ok(lo);
        }
        let inclusive = self.at_p("..=");
        self.bump();
        let hi = if rhs_starts(self) {
            Some(Box::new(self.binary(s, 0)?))
        } else {
            None
        };
        if self.at_p("..") || self.at_p("..=") {
            return self.err("P003");
        }
        Ok(Expr {
            span: self.span_from(start),
            kind: ExprKind::Range {
                lo: Some(Box::new(lo)),
                hi,
                inclusive,
            },
        })
    }

    /// Binary levels 14 down to 6 of the precedence table (6.1), by precedence
    /// climbing: one frame per operator, not one per level, which keeps stack use
    /// per nesting level small. Level index: 0 `||`, 1 `&&`, 2 comparisons
    /// (non-associative, P002), 3 `|`, 4 `^`, 5 `&`, 6 shifts, 7 additive,
    /// 8 multiplicative. All other levels are left-associative.
    fn binary(&mut self, s: bool, min: usize) -> PResult<Expr> {
        let start = self.tok().start;
        let mut lhs = self.cast(s)?;
        let mut lhs_is_cmp = false;
        while let Some(level) = self.binary_level() {
            if level < min {
                break;
            }
            if level == 2 && lhs_is_cmp {
                return self.err("P002");
            }
            let op = self.bump_text().to_string();
            let rhs = self.binary(s, level + 1)?;
            lhs = Expr {
                span: self.span_from(start),
                kind: ExprKind::Binary {
                    op,
                    lhs: Box::new(lhs),
                    rhs: Box::new(rhs),
                },
            };
            lhs_is_cmp = level == 2;
        }
        Ok(lhs)
    }

    fn binary_level(&self) -> Option<usize> {
        let t = self.tok();
        if t.kind != Kind::Punct {
            return None;
        }
        Some(match self.text(t) {
            "||" => 0,
            "&&" => 1,
            "==" | "!=" | "<" | ">" | "<=" | ">=" => 2,
            "|" => 3,
            "^" => 4,
            "&" => 5,
            "<<" | ">>" => 6,
            "+" | "-" | "+%" | "-%" | "+|" | "-|" => 7,
            "*" | "/" | "%" | "*%" | "*|" => 8,
            _ => return None,
        })
    }

    fn cast(&mut self, s: bool) -> PResult<Expr> {
        let start = self.tok().start;
        let mut e = self.unary(s)?;
        while self.at_kw("as") {
            self.bump();
            let ty = self.ty()?;
            e = Expr {
                span: self.span_from(start),
                kind: ExprKind::Cast {
                    expr: Box::new(e),
                    ty,
                },
            };
        }
        Ok(e)
    }

    fn unary(&mut self, s: bool) -> PResult<Expr> {
        let start = self.tok().start;
        for (tok, op) in [("+", "pos"), ("-", "neg"), ("!", "not"), ("~", "bnot")] {
            if self.at_p(tok) {
                self.bump();
                let inner = self.nested(|p| p.unary(s))?;
                return Ok(Expr {
                    span: self.span_from(start),
                    kind: ExprKind::Unary {
                        op,
                        expr: Box::new(inner),
                    },
                });
            }
        }
        let base = self.await_expr(s)?;
        if self.at_p("**") {
            self.bump();
            let rhs = self.nested(|p| p.unary(s))?;
            return Ok(Expr {
                span: self.span_from(start),
                kind: ExprKind::Binary {
                    op: "**".into(),
                    lhs: Box::new(base),
                    rhs: Box::new(rhs),
                },
            });
        }
        Ok(base)
    }

    fn await_expr(&mut self, s: bool) -> PResult<Expr> {
        let start = self.tok().start;
        if self.at_kw("await") {
            self.bump();
            let (head, bare) = self.head(s)?;
            let inner = self.postfix(head, bare, s, false)?;
            let mut e = Expr {
                span: self.span_from(start),
                kind: ExprKind::Await(Box::new(inner)),
            };
            while self.at_p("?") {
                self.bump();
                e = Expr {
                    span: self.span_from(start),
                    kind: ExprKind::Try(Box::new(e)),
                };
            }
            return Ok(e);
        }
        let (head, bare) = self.head(s)?;
        self.postfix(head, bare, s, true)
    }

    /// head ::= path_head | primary. Returns whether the head starts a bare path chain.
    fn head(&mut self, s: bool) -> PResult<(Expr, bool)> {
        let t = self.tok();
        let start = t.start;
        if Self::is_name(t) || self.is_kw(t, "self") || self.is_kw(t, "Self") {
            self.bump();
            let name = self.text(t).to_string();
            if self.at_p("<") && self.rule_b(s)? {
                let args = self.generic_args()?;
                return Ok((
                    Expr {
                        span: self.span_from(start),
                        kind: ExprKind::GenericName { name, args },
                    },
                    true,
                ));
            }
            let kind = if Self::is_name(t) {
                ExprKind::Name(name)
            } else {
                ExprKind::SelfValue(name)
            };
            return Ok((
                Expr {
                    span: self.span_from(start),
                    kind,
                },
                true,
            ));
        }
        Ok((self.primary(s)?, false))
    }

    fn postfix(&mut self, mut e: Expr, mut bare: bool, s: bool, allow_try: bool) -> PResult<Expr> {
        let start = e.span.0;
        loop {
            if self.at_p(".") {
                let n = self.nth(1);
                self.bump();
                if Self::is_name(n) {
                    let name = self.name()?;
                    let args = if self.at_p("<") && self.rule_b(s)? {
                        Some(self.generic_args()?)
                    } else {
                        None
                    };
                    e = Expr {
                        span: self.span_from(start),
                        kind: ExprKind::Field {
                            recv: Box::new(e),
                            name,
                            args,
                        },
                    };
                } else if n.kind == Kind::Int {
                    let index = self.bump_text().to_string();
                    e = Expr {
                        span: self.span_from(start),
                        kind: ExprKind::TupleField {
                            recv: Box::new(e),
                            index,
                        },
                    };
                    bare = false;
                } else {
                    return self.err("P001");
                }
            } else if self.at_p("(") {
                let args = self.call_args()?;
                e = Expr {
                    span: self.span_from(start),
                    kind: ExprKind::Call {
                        callee: Box::new(e),
                        args,
                    },
                };
                bare = false;
            } else if self.at_p("[") {
                self.bump();
                let index = self.expr(true)?;
                self.expect_close("]")?;
                e = Expr {
                    span: self.span_from(start),
                    kind: ExprKind::Index {
                        recv: Box::new(e),
                        index: Box::new(index),
                    },
                };
                bare = false;
            } else if allow_try && self.at_p("?") {
                self.bump();
                e = Expr {
                    span: self.span_from(start),
                    kind: ExprKind::Try(Box::new(e)),
                };
                bare = false;
            } else if bare && s && self.at_p("{") {
                e = self.struct_body(e)?;
                bare = false;
            } else {
                return Ok(e);
            }
        }
    }

    /// Rule B (grammar 7): does the `<` at the cursor open a generic argument list?
    ///
    /// A bounded scan without consuming. It accepts only a well-formed
    /// `generic_args`: bracket groups are matched (and skipped in O(1) through
    /// the bracket-match table, so `{N > 1}` and `[u8; 4]` cannot be mistaken
    /// for a closer), only tokens that occur in types appear at the top level,
    /// and no argument is empty. The closing `>` must be followed by `(`, `.`,
    /// or, outside expr_ns, `{`.
    fn rule_b(&mut self, s: bool) -> PResult<bool> {
        #[derive(Clone, Copy)]
        struct ArgsState {
            has_arg: bool,
            at_start: bool,
            after_comma: bool,
        }

        let lt = self.tok();
        let mut states = vec![ArgsState {
            has_arg: false,
            at_start: true,
            after_comma: false,
        }];
        let mut i = self.live(self.pos) + 1;
        let mut scanned = 0usize;
        loop {
            scanned += 1;
            if scanned > RULE_B_LIMIT {
                return self.err_at("P009", lt.start, 1);
            }
            let Some(&t) = self.toks.get(i) else {
                return Ok(false);
            };
            if t.soft {
                i += 1;
                continue;
            }
            let text = self.text(t);
            match t.kind {
                Kind::Ident | Kind::RawIdent | Kind::Int => {
                    let state = states.last_mut().expect("generic argument state");
                    state.has_arg = true;
                    state.at_start = false;
                    state.after_comma = false;
                }
                Kind::Keyword
                    if ["fn", "const", "var", "self", "Self", "true", "false"].contains(&text) =>
                {
                    let state = states.last_mut().expect("generic argument state");
                    state.has_arg = true;
                    state.at_start = false;
                    state.after_comma = false;
                }
                Kind::Punct => match text {
                    "(" | "[" | "{" => match t.close {
                        Some(c) if c > i => {
                            // The balanced group is one generic argument atom
                            // here; the real parser validates its contents.
                            scanned = scanned.saturating_add(c - i);
                            if scanned > RULE_B_LIMIT {
                                return self.err_at("P009", lt.start, 1);
                            }
                            let state = states.last_mut().expect("generic argument state");
                            state.has_arg = true;
                            state.at_start = false;
                            state.after_comma = false;
                            i = c;
                        }
                        _ => return Ok(false),
                    },
                    ")" | "]" | "}" => return Ok(false),
                    "<" => {
                        // A nested generic list must follow the type/path atom
                        // that owns it; a bare `<...>` cannot start an argument.
                        if states.last().expect("generic argument state").at_start {
                            return Ok(false);
                        }
                        states.push(ArgsState {
                            has_arg: false,
                            at_start: true,
                            after_comma: false,
                        });
                    }
                    "," => {
                        let state = states.last_mut().expect("generic argument state");
                        if state.at_start {
                            return Ok(false);
                        }
                        state.at_start = true;
                        state.after_comma = true;
                    }
                    "." | "*" | "/" | "**" | "-" | "?" | "->" | "+" => {
                        let state = states.last_mut().expect("generic argument state");
                        state.has_arg = true;
                        state.at_start = false;
                        state.after_comma = false;
                    }
                    _ if text.starts_with('>') => {
                        let closes = text.bytes().take_while(|&b| b == b'>').count();
                        for k in 0..closes {
                            let current = *states.last().expect("generic argument state");
                            // Empty lists are invalid, but a separator after at
                            // least one argument is the grammar's valid trailing comma.
                            if current.at_start && !(current.has_arg && current.after_comma) {
                                return Ok(false);
                            }
                            states.pop();
                            if states.is_empty() {
                                if k + 1 < closes || text.len() != closes {
                                    // A remaining `>` or `=` means this token
                                    // did not end exactly at the generic closer.
                                    return Ok(false);
                                }
                                let next = self.toks.get(i + 1).copied();
                                return Ok(next.is_some_and(|n| {
                                    self.is_p(n, "(")
                                        || self.is_p(n, ".")
                                        || (s && self.is_p(n, "{"))
                                }));
                            }
                            // The nested list was itself the current type atom
                            // in its parent argument.
                            if let Some(parent) = states.last_mut() {
                                parent.has_arg = true;
                                parent.at_start = false;
                                parent.after_comma = false;
                            }
                        }
                    }
                    _ => return Ok(false),
                },
                _ => return Ok(false),
            }
            i += 1;
        }
    }

    fn call_args(&mut self) -> PResult<Vec<Arg>> {
        self.paren_list_r(
            "(",
            ")",
            |p| {
                let name = if Self::is_name(p.tok()) && p.is_p(p.nth(1), ":") {
                    let n = p.name()?;
                    p.bump();
                    Some(n)
                } else {
                    None
                };
                let marker = if p.at_p("&") {
                    p.bump();
                    Some("&")
                } else if p.at_id("take") && p.starts_expr(p.nth(1)) {
                    p.bump();
                    Some("take")
                } else {
                    None
                };
                Ok(Arg {
                    name,
                    marker,
                    value: p.expr(true)?,
                })
            },
            |p, start| {
                Some(Arg {
                    name: None,
                    marker: None,
                    value: p.error_expr(start),
                })
            },
        )
    }

    fn struct_body(&mut self, path: Expr) -> PResult<Expr> {
        let start = path.span.0;
        let mut base = None;
        let fields = self.brace_list(|p| {
            if p.at_p("..") {
                p.bump();
                base = Some(Box::new(p.expr(true)?));
                return Ok(None);
            }
            let name = p.name()?;
            let value = if p.at_p(":") {
                p.bump();
                Some(p.expr(true)?)
            } else {
                None
            };
            Ok(Some((name, value)))
        })?;
        let fields = fields.into_iter().flatten().collect();
        Ok(Expr {
            span: self.span_from(start),
            kind: ExprKind::Struct {
                path: Box::new(path),
                fields,
                base,
            },
        })
    }

    /// Dispatcher only: each construct has its own small function, so the frame
    /// on the recursion path (and debug-build stack use per nesting level) stays
    /// small.
    fn primary(&mut self, s: bool) -> PResult<Expr> {
        let t = self.tok();
        let start = t.start;
        let kind = match t.kind {
            Kind::Int | Kind::Float | Kind::Char | Kind::RawStr | Kind::ByteStr => {
                self.bump();
                ExprKind::Literal(self.slice(start))
            }
            Kind::Str => self.string_lit(t)?,
            Kind::Error => {
                // The lexer already reported this token; do not cascade.
                self.bump();
                ExprKind::Error
            }
            Kind::Punct if self.at_p("(") => return self.paren_expr(),
            Kind::Punct if self.at_p("[") => self.array_expr()?,
            Kind::Punct if self.at_p("{") => ExprKind::Block(self.block()?),
            Kind::Keyword => match self.text(t) {
                "true" | "false" => {
                    self.bump();
                    ExprKind::Literal(self.slice(start))
                }
                "comptime" => {
                    self.bump();
                    ExprKind::Comptime(self.block()?)
                }
                "unsafe" => self.unsafe_expr()?,
                "if" => return self.if_expr(),
                "match" => self.match_expr()?,
                "while" => self.while_expr()?,
                "loop" => {
                    self.bump();
                    ExprKind::Loop(self.block()?)
                }
                "for" => self.for_expr()?,
                "return" | "break" => self.jump_expr(s)?,
                "continue" => {
                    self.bump();
                    ExprKind::Continue
                }
                _ => return self.err("P001"),
            },
            _ => return self.err("P001"),
        };
        Ok(Expr {
            span: self.span_from(start),
            kind,
        })
    }

    #[inline(never)]
    fn string_lit(&mut self, t: PTok) -> PResult<ExprKind> {
        self.bump();
        let text = self.slice(t.start);
        let lexed_tokens = self.lexed_tokens;
        match t.lex.map(|i| &lexed_tokens[i].holes) {
            Some(h) if !h.is_empty() => {
                let mut parsed = Vec::new();
                for hole in h {
                    parsed.push(self.hole(hole)?);
                }
                Ok(ExprKind::Interp {
                    text,
                    holes: parsed,
                })
            }
            _ => Ok(ExprKind::Literal(text)),
        }
    }

    /// An element of a tuple or array: on error, recover to the next `,` or
    /// closer and stand an Error expression in its place (8).
    fn expr_or_recover(&mut self, boundary: usize, base_depth: usize) -> PResult<Expr> {
        let start = self.tok().start;
        match self.expr(true) {
            Ok(e) => Ok(e),
            Err(()) if self.fatal_recovery => Err(()),
            Err(()) => {
                self.sync_list(boundary, base_depth);
                Ok(self.error_expr(start))
            }
        }
    }

    #[inline(never)]
    fn paren_expr(&mut self) -> PResult<Expr> {
        let boundary = self.live(self.pos);
        let start = self.bump().start;
        let base_depth = self.delimiters.len();
        if self.at_p(")") {
            self.bump();
            return Ok(Expr {
                span: self.span_from(start),
                kind: ExprKind::Unit,
            });
        }
        let first = self.expr_or_recover(boundary, base_depth)?;
        if !self.at_p(",") {
            self.expect_close(")")?;
            return Ok(first); // parentheses group only
        }
        let mut elems = vec![first];
        while self.at_p(",") {
            self.bump();
            if self.at_p(")") {
                break;
            }
            elems.push(self.expr_or_recover(boundary, base_depth)?);
        }
        self.expect_close(")")?;
        Ok(Expr {
            span: self.span_from(start),
            kind: ExprKind::Tuple(elems),
        })
    }

    #[inline(never)]
    fn array_expr(&mut self) -> PResult<ExprKind> {
        let boundary = self.live(self.pos);
        self.bump();
        let base_depth = self.delimiters.len();
        if self.at_p("]") {
            self.bump();
            return Ok(ExprKind::Array(Vec::new()));
        }
        let first = self.expr_or_recover(boundary, base_depth)?;
        if self.at_p(";") {
            self.bump();
            let count = self.expr(true)?;
            self.expect_close("]")?;
            return Ok(ExprKind::Repeat {
                value: Box::new(first),
                count: Box::new(count),
            });
        }
        let mut elems = vec![first];
        while self.at_p(",") {
            self.bump();
            if self.at_p("]") {
                break;
            }
            elems.push(self.expr_or_recover(boundary, base_depth)?);
        }
        self.expect_close("]")?;
        Ok(ExprKind::Array(elems))
    }

    #[inline(never)]
    fn unsafe_expr(&mut self) -> PResult<ExprKind> {
        self.bump();
        self.expect_p("(")?;
        if !self.at_id("cap") {
            return self.err("P001");
        }
        self.bump();
        self.expect_p(":")?;
        let mut caps = vec![self.path()?];
        while self.at_p("+") {
            self.bump();
            caps.push(self.path()?);
        }
        self.expect_close(")")?;
        let body = self.block()?;
        Ok(ExprKind::Unsafe { caps, body })
    }

    #[inline(never)]
    fn match_expr(&mut self) -> PResult<ExprKind> {
        self.bump();
        let scrutinee = self.expr(false)?;
        let arms = self.brace_list(|p| {
            let pat = p.pattern()?;
            let guard = if p.at_kw("if") {
                p.bump();
                Some(p.expr(false)?)
            } else {
                None
            };
            p.expect_p("=>")?;
            let body = p.expr(true)?;
            Ok(MatchArm { pat, guard, body })
        })?;
        Ok(ExprKind::Match {
            scrutinee: Box::new(scrutinee),
            arms,
        })
    }

    #[inline(never)]
    fn while_expr(&mut self) -> PResult<ExprKind> {
        self.bump();
        let cond = self.cond()?;
        let body = self.block()?;
        Ok(ExprKind::While {
            cond: Box::new(cond),
            body,
        })
    }

    #[inline(never)]
    fn for_expr(&mut self) -> PResult<ExprKind> {
        self.bump();
        let pat = self.pattern()?;
        self.expect_kw("in")?;
        let iter = self.expr(false)?;
        let mut with = Vec::new();
        while self.at_id("with") && Self::is_name(self.nth(1)) {
            self.bump();
            let name = self.name()?;
            with.push((name, self.call_args()?));
        }
        let body = self.block()?;
        Ok(ExprKind::For {
            pat,
            iter: Box::new(iter),
            with,
            body,
        })
    }

    #[inline(never)]
    fn jump_expr(&mut self, s: bool) -> PResult<ExprKind> {
        let is_return = self.at_kw("return");
        self.bump();
        let value = if self.starts_expr(self.tok()) && !(self.at_p("{") && !s) {
            Some(Box::new(self.expr(true)?))
        } else {
            None
        };
        Ok(if is_return {
            ExprKind::Return(value)
        } else {
            ExprKind::Break(value)
        })
    }

    fn hole(&mut self, hole: &Hole) -> PResult<(Expr, Option<String>)> {
        // The hole continues the enclosing nesting depth (P018).
        let mut sub = Parser::for_hole(self.src, self.lexed_tokens, hole, self.depth + 1);
        let result = sub.expr(true);
        if result.is_ok() && !sub.at_eof() {
            let _ = sub.err::<()>("P001");
        }
        let failed = result.is_err() || !sub.diags.is_empty();
        self.fatal_recovery |= sub.diags.iter().any(|d| d.code == "P018");
        self.diags.append(&mut sub.diags);
        if failed {
            return Err(());
        }
        let spec = hole
            .format_spec
            .map(|(a, b)| String::from_utf8_lossy(&self.src[a as usize..b as usize]).into_owned());
        Ok((result.unwrap(), spec))
    }

    fn cond(&mut self) -> PResult<Cond> {
        if self.at_kw("let") {
            self.bump();
            let pat = self.pattern()?;
            self.expect_p("=")?;
            return Ok(Cond::Let {
                pat,
                value: self.expr(false)?,
            });
        }
        Ok(Cond::Expr(self.expr(false)?))
    }

    fn if_expr(&mut self) -> PResult<Expr> {
        let start = self.bump().start;
        let cond = self.cond()?;
        if self.at_p("=") {
            return self.err("P004");
        }
        if !self.at_p("{") {
            return self.err("P017");
        }
        let then = self.block()?;
        let otherwise = if self.at_kw("else") {
            self.bump();
            if self.at_kw("if") {
                Some(Box::new(self.if_expr()?))
            } else {
                let bstart = self.tok().start;
                let b = self.block()?;
                Some(Box::new(Expr {
                    span: self.span_from(bstart),
                    kind: ExprKind::Block(b),
                }))
            }
        } else {
            None
        };
        Ok(Expr {
            span: self.span_from(start),
            kind: ExprKind::If {
                cond: Box::new(cond),
                then,
                otherwise,
            },
        })
    }
}

#[cfg(test)]
mod recovery_cursor_tests {
    use super::{PTok, Parser};
    use pegasus_lexer::Kind;

    #[test]
    fn skipping_a_rule_2a_token_wakes_its_dormant_newline() {
        for marker in [">", ">>", "*"] {
            let mut src = marker.as_bytes().to_vec();
            src.extend_from_slice(b"\nx");
            let split = marker.len() as u32;
            let end = src.len() as u32;
            let toks = vec![
                PTok {
                    kind: Kind::Punct,
                    start: 0,
                    end: split,
                    close: None,
                    lex: None,
                    soft: false,
                },
                PTok {
                    kind: Kind::Newline,
                    start: split,
                    end: split + 1,
                    close: None,
                    lex: None,
                    soft: true,
                },
                PTok {
                    kind: Kind::Ident,
                    start: split + 1,
                    end,
                    close: None,
                    lex: None,
                    soft: false,
                },
                PTok {
                    kind: Kind::Eof,
                    start: end,
                    end,
                    close: None,
                    lex: None,
                    soft: false,
                },
            ];
            let mut parser = Parser {
                src: &src,
                lexed_tokens: &[],
                toks,
                pos: 0,
                last_end: 0,
                depth: 0,
                suppress: false,
                diags: Vec::new(),
                delimiters: Vec::new(),
                braces: Vec::new(),
                fatal_recovery: false,
            };
            parser.skip_one();
            assert_eq!(
                parser.tok().kind,
                Kind::Newline,
                "skipping {marker:?} must wake the following dormant newline"
            );
        }
    }
}
