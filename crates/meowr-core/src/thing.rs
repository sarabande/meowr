//! Things: the named units `,` `.` `[` `]` select.
//!
//! Meow's registry is a small DSL rather than a table of functions — a
//! depth-counted pair matcher over literal tokens, a paired-regexp variant, a
//! regexp pair, and a syntax-class descriptor. Three of those four port
//! directly. The fourth, `(syntax . STRING)`, is a direct dependency on Emacs
//! syntax tables and is replaced rather than ported: a character class here,
//! and per-language rules when there are languages (#25).
//!
//! Every Thing is looked up **by name**, and the set is open — the same answer
//! the command registry and [`crate::Kind`] give, and the reason `Kind` is a
//! newtype over a string rather than an enum.

use crate::{next_char_boundary, previous_char_boundary};
use meowr_domain::Document;
use std::collections::HashMap;
use std::ops::Range;

/// How to find a Thing around a position.
///
/// Not `PartialEq`: one variant holds a function pointer, and comparing those
/// answers a question about addresses rather than about behaviour.
/// Which part of a Thing a selection takes.
///
/// `meow-inner-of-thing` and `meow-bounds-of-thing` are separate commands on
/// separate keys, and the difference between them is the delimiters: `, r`
/// takes what is inside the parentheses and `. r` takes the parentheses too.
/// A `bool` at the call site says `true` and leaves the reader to work out
/// which of the two that was.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Extent {
    /// Inside the delimiters — `meow-inner-of-thing`.
    Inner,
    /// The Thing with its delimiters — `meow-bounds-of-thing`.
    Bounds,
}

impl Extent {
    /// For the one boundary that still takes a `bool`: `meowr-syntax` does not
    /// depend on this crate and cannot name this type.
    pub fn is_inner(self) -> bool {
        self == Extent::Inner
    }
}

#[derive(Debug, Clone)]
pub enum Thing {
    /// A depth-counted pair of delimiters, as `(pair ("(") (")"))`.
    ///
    /// Nesting is the whole point: from inside `((a))`, the inner pair is the
    /// answer, and counting rather than matching the first delimiter seen is
    /// what makes that true.
    Pair { open: char, close: char },
    /// A run of characters sharing a class — Meow's `(syntax . STRING)` with
    /// the syntax table taken out.
    Chars(fn(char) -> bool),
    /// A symbol. Its own variant because its *bounds* are not its inner:
    /// `meow--bounds-of-symbol` also swallows an adjacent space or comma, on
    /// whichever side is not against a closing bracket — so `.e` on an
    /// argument takes the separator with it and the selection can be killed
    /// without leaving `(a, , c)` behind.
    Symbol,
    /// Text between two occurrences of the same delimiter, which cannot nest.
    Quoted(char),
    /// The lines between blank lines.
    Paragraph,
    /// Up to a sentence-ending character and the space after it.
    Sentence,
    /// One line: inner from the indentation, bounds including the newline.
    Line,
    /// The whole document. An editor concept rather than a text one, which is
    /// why it needs no rule.
    Buffer,
}

/// Things by name, extensible.
#[derive(Debug, Clone)]
pub struct Things {
    map: HashMap<String, Thing>,
}

impl Default for Things {
    fn default() -> Self {
        Self::new()
    }
}

impl Things {
    /// The lexical Things, under the names `meow-char-thing-table` gives them.
    ///
    /// `defun` is deliberately absent: ADR-0006 puts it with tree-sitter,
    /// where it can be right, rather than with a heuristic that would have to
    /// be wrong in the same places Emacs's is. `window` is absent for a
    /// different reason — it is a property of the viewport, which this crate
    /// cannot see, so the session supplies it.
    pub fn new() -> Self {
        let mut map = HashMap::new();
        map.insert(
            "round".into(),
            Thing::Pair {
                open: '(',
                close: ')',
            },
        );
        map.insert(
            "square".into(),
            Thing::Pair {
                open: '[',
                close: ']',
            },
        );
        map.insert(
            "curly".into(),
            Thing::Pair {
                open: '{',
                close: '}',
            },
        );
        map.insert("string".into(), Thing::Quoted('"'));
        map.insert("symbol".into(), Thing::Symbol);
        map.insert("paragraph".into(), Thing::Paragraph);
        map.insert("sentence".into(), Thing::Sentence);
        map.insert("line".into(), Thing::Line);
        // The same Thing as `line`, and deliberately so: a visual line is a
        // *screen* line, and meowr clips rather than wrapping — "a wrapped
        // line would make the row on screen and the line in the buffer
        // different things" (`meowr-render`). Where nothing wraps, the two
        // are one. This alias is a claim about the renderer, so it stops
        // being right the day soft wrap arrives.
        map.insert("visual-line".into(), Thing::Line);
        map.insert("buffer".into(), Thing::Buffer);
        Self { map }
    }

    pub fn register(&mut self, name: impl Into<String>, thing: Thing) {
        self.map.insert(name.into(), thing);
    }

    pub fn get(&self, name: &str) -> Option<&Thing> {
        self.map.get(name)
    }

    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.map.keys().map(String::as_str)
    }

    /// Inside the delimiters.
    pub fn inner(&self, doc: &Document, pos: usize, name: &str) -> Option<Range<usize>> {
        range_of(doc, pos, self.get(name)?, Extent::Inner)
    }

    /// Including the delimiters.
    pub fn bounds(&self, doc: &Document, pos: usize, name: &str) -> Option<Range<usize>> {
        range_of(doc, pos, self.get(name)?, Extent::Bounds)
    }
}

fn range_of(doc: &Document, pos: usize, thing: &Thing, extent: Extent) -> Option<Range<usize>> {
    match thing {
        Thing::Pair { open, close } => pair(doc, pos, *open, *close, extent),
        Thing::Chars(member) => chars(doc, pos, *member),
        Thing::Symbol => symbol(doc, pos, extent),
        Thing::Quoted(delimiter) => quoted(doc, pos, *delimiter, extent),
        Thing::Paragraph => paragraph(doc, pos),
        Thing::Sentence => sentence(doc, pos),
        Thing::Line => Some(line(doc, pos, extent)),
        Thing::Buffer => Some(0..doc.byte_len()),
    }
}

/// The enclosing pair, counted by depth.
///
/// Backwards from the point, a closing delimiter deepens and an opening one
/// shallows; the opening that takes the depth positive is the one that
/// encloses. Forwards, the mirror. Matching the first delimiter seen instead
/// would make `(a (b) c)` answer `(b)` from anywhere after it.
///
/// **Every bracket counts here, including one inside a string or a comment**,
/// and that is deliberate — it is not the debt [`crate::Delimiters`] paid off
/// for `meow-block`. Emacs has two bracket matchers with two different rules:
/// `meow-block` asks `syntax-ppss` and skips strings, while
/// `meow--thing-parse-pair-search`, which is what `,` and `.` reach for
/// `round`, `square` and `curly`, is a bare `re-search-forward` over the
/// literal token with no `syntax-ppss` and no `meow--in-string-p` anywhere
/// near it. Nothing in `meow-thing.el` consults either except
/// `meow--bounds-of-string-1`, finding a string's own bounds.
///
/// So `o` and `, r` disagree in Emacs, and therefore disagree here. Making
/// them agree would be a divergence dressed up as a fix. The `stringpair`
/// cases in the `things` suite pin it: on `call(a, ")", b) end`, `. r` from
/// the comma answers 4..10 — reaching to the `)` *inside* the string.
fn pair(
    doc: &Document,
    pos: usize,
    open: char,
    close: char,
    extent: Extent,
) -> Option<Range<usize>> {
    let mut depth = 0i32;
    let mut at = pos;
    let start = loop {
        let prev = previous_char_boundary(doc, at)?;
        match char_at(doc, prev) {
            Some(c) if c == close => depth -= 1,
            Some(c) if c == open => {
                depth += 1;
                if depth > 0 {
                    break prev;
                }
            }
            _ => {}
        }
        at = prev;
    };

    let mut depth = 0i32;
    let mut at = pos;
    let end = loop {
        match char_at(doc, at) {
            Some(c) if c == open => depth -= 1,
            Some(c) if c == close => {
                depth += 1;
                if depth > 0 {
                    break at;
                }
            }
            None => return None,
            _ => {}
        }
        at = next_char_boundary(doc, at)?;
    };

    Some(if extent.is_inner() {
        next_char_boundary(doc, start).unwrap_or(start)..end
    } else {
        start..next_char_boundary(doc, end).unwrap_or(end)
    })
}

/// The `match` Thing: the innermost bracket pair **around** the point,
/// whichever of the three kinds it is.
///
/// meowr's own, with no counterpart in `meow-char-thing-table` — `, m` in real
/// Meow is a silent no-op, because `meow--parse-inner-of-thing-char` is a
/// `when-let*` over the char table and its callers are a bare `(when bounds
/// ...)`. So this is measured as a divergence rather than against a golden.
///
/// **It asks [`crate::Delimiters`], and [`pair`] does not.** That is the one
/// interesting decision here, and it is chosen rather than inherited: a `)`
/// inside a string is a character being shown, so `, m` agrees with `o` about
/// which brackets are real and disagrees with `, r`, which is a bare literal
/// search by way of `meow--thing-parse-pair-search`. ADR-0006 binds Fidelity
/// to the keys and the selection calculus and not to the heuristic underneath,
/// and a Thing Meow does not have has no faithful heuristic to be faithful to.
///
/// **It does not reach ahead, and [`crate::block`] does.** `o` scans for the
/// delimiter that returns to the depth the point began at, which finds a block
/// the point merely sits *before*; that is what `o` is for. Every entry in the
/// char table means the Thing *around* the point, and this one keeps that.
/// The two are complements, not duplicates.
///
/// Unbalanced text — no partner, or a closer of the wrong kind — is no match
/// at all, the same refusal `meow-block` makes by wrapping `forward-list` in
/// `ignore-errors`.
pub fn enclosing_pair(
    doc: &Document,
    pos: usize,
    delimiters: Option<&dyn crate::Delimiters>,
    extent: Extent,
) -> Option<Range<usize>> {
    let counts = |at: usize| delimiters.is_none_or(|d| d.counts(at));

    // Back to the first opener this position is not already inside the
    // matching close of. `pending` holds the closers passed on the way, so a
    // pair opened and closed behind the point is stepped over rather than
    // mistaken for the enclosing one.
    let mut pending: Vec<char> = Vec::new();
    let mut at = pos;
    let (start, close) = loop {
        let prev = previous_char_boundary(doc, at)?;
        if counts(prev) {
            match char_at(doc, prev) {
                Some(c) if is_closer(c) => pending.push(c),
                Some(c) => {
                    if let Some(closer) = closer_of(c) {
                        match pending.pop() {
                            None => break (prev, closer),
                            Some(seen) if seen == closer => {}
                            Some(_) => return None,
                        }
                    }
                }
                None => {}
            }
        }
        at = prev;
    };

    // Forward to that opener's partner. Everything between the opener and the
    // point balanced out above, so this starts from the point with an empty
    // stack rather than re-walking it.
    let mut depth: Vec<char> = Vec::new();
    let mut at = pos;
    let end = loop {
        let c = char_at(doc, at)?;
        if counts(at) {
            if let Some(closer) = closer_of(c) {
                depth.push(closer);
            } else if is_closer(c) {
                match depth.pop() {
                    None if c == close => break at,
                    None => return None,
                    Some(expected) if expected == c => {}
                    Some(_) => return None,
                }
            }
        }
        at = next_char_boundary(doc, at)?;
    };

    Some(if extent.is_inner() {
        next_char_boundary(doc, start).unwrap_or(start)..end
    } else {
        start..next_char_boundary(doc, end).unwrap_or(end)
    })
}

/// What closes `c`, if `c` opens one of [`crate::PAIRS`].
fn closer_of(c: char) -> Option<char> {
    crate::PAIRS
        .iter()
        .find(|(open, _)| *open == c)
        .map(|(_, close)| *close)
}

/// Whether `c` closes one of [`crate::PAIRS`].
fn is_closer(c: char) -> bool {
    crate::PAIRS.iter().any(|(_, close)| *close == c)
}

/// A run of characters in a class, around the point.
///
/// `inner` and `bounds` are the same for these: there are no delimiters to be
/// inside of. Meow says so too — `symbol` registers the same function twice,
/// give or take the trailing comma it also swallows.
fn chars(doc: &Document, pos: usize, member: fn(char) -> bool) -> Option<Range<usize>> {
    let anchor = if char_at(doc, pos).is_some_and(member) {
        pos
    } else {
        let before = previous_char_boundary(doc, pos)?;
        if !char_at(doc, before).is_some_and(member) {
            return None;
        }
        before
    };
    let mut start = anchor;
    while let Some(prev) = previous_char_boundary(doc, start) {
        if !char_at(doc, prev).is_some_and(member) {
            break;
        }
        start = prev;
    }
    let mut end = anchor;
    while char_at(doc, end).is_some_and(member) {
        match next_char_boundary(doc, end) {
            Some(next) => end = next,
            None => break,
        }
    }
    Some(start..end)
}

/// A symbol, and for `bounds` the separator beside it.
fn symbol(doc: &Document, pos: usize, extent: Extent) -> Option<Range<usize>> {
    let run = chars(doc, pos, crate::is_symbol_char)?;
    if extent.is_inner() {
        return Some(run);
    }
    let separator = |c: char| c == ' ' || c == ',';
    // Against a closing bracket, take the separator on the left instead —
    // otherwise `(a, b)` would give `b)` rather than `, b`.
    if char_at(doc, run.end).is_some_and(|c| matches!(c, ')' | ']' | '}')) {
        let mut start = run.start;
        while let Some(prev) = previous_char_boundary(doc, start) {
            if !char_at(doc, prev).is_some_and(separator) {
                break;
            }
            start = prev;
        }
        Some(start..run.end)
    } else {
        let mut end = run.end;
        while char_at(doc, end).is_some_and(separator) {
            end = match next_char_boundary(doc, end) {
                Some(next) => next,
                None => break,
            };
        }
        Some(run.start..end)
    }
}

/// Text between two of the same delimiter.
///
/// Counted from the start of the buffer rather than searched outward: a quote
/// character is both an opening and a closing one, so "the nearest quote
/// backwards" cannot tell you which side of a string you are on. Parity can.
fn quoted(doc: &Document, pos: usize, delimiter: char, extent: Extent) -> Option<Range<usize>> {
    let mut open: Option<usize> = None;
    let mut at = 0;
    while at < doc.byte_len() {
        if char_at(doc, at) == Some(delimiter) {
            match open {
                Some(start) => {
                    let end = at;
                    if start <= pos && pos <= end {
                        return Some(if extent.is_inner() {
                            next_char_boundary(doc, start).unwrap_or(start)..end
                        } else {
                            start..next_char_boundary(doc, end).unwrap_or(end)
                        });
                    }
                    open = None;
                }
                None => open = Some(at),
            }
        }
        at = next_char_boundary(doc, at)?;
    }
    None
}

fn line_of(doc: &Document, pos: usize) -> (usize, usize) {
    crate::line_bounds_of(doc, pos)
}

fn line(doc: &Document, pos: usize, extent: Extent) -> Range<usize> {
    let (start, end) = line_of(doc, pos);
    if extent.is_inner() {
        // From the indentation, as `meow--inner-of-line` does.
        let mut at = start;
        while at < end && char_at(doc, at).is_some_and(|c| c == ' ' || c == '\t') {
            at = next_char_boundary(doc, at).unwrap_or(end);
        }
        at..end
    } else {
        start..next_char_boundary(doc, end).unwrap_or(end)
    }
}

fn blank(doc: &Document, pos: usize) -> bool {
    let (start, end) = line_of(doc, pos);
    (start..end).all(|_| false) || {
        let mut at = start;
        while at < end {
            if !char_at(doc, at).is_some_and(char::is_whitespace) {
                return false;
            }
            at = match next_char_boundary(doc, at) {
                Some(next) => next,
                None => break,
            };
        }
        true
    }
}

/// The run of non-blank lines around the point, **including the newline that
/// ends the last of them** — which is what `bounds-of-thing-at-point` gives,
/// and what makes killing a paragraph leave no blank line behind.
fn paragraph(doc: &Document, pos: usize) -> Option<Range<usize>> {
    if doc.byte_len() == 0 {
        return None;
    }
    let (mut start, mut end) = line_of(doc, pos.min(doc.byte_len()));
    while start > 0 {
        let (above, _) = line_of(doc, start - 1);
        if blank(doc, above) {
            break;
        }
        start = above;
    }
    while end < doc.byte_len() {
        let (below, below_end) = line_of(doc, end + 1);
        if blank(doc, below) {
            break;
        }
        end = below_end;
    }
    Some(start..next_char_boundary(doc, end).unwrap_or(end))
}

/// Up to and including a sentence-ending character.
///
/// Emacs ends a sentence at `.`, `!` or `?` **followed by two spaces, a
/// newline, or the end of the buffer** — `sentence-end-double-space`, which
/// defaults to t. So "One thing. Then another." is *one* sentence to Emacs,
/// not two, which is surprising until you know the rule and then explains
/// every complaint about `M-e`. Matched rather than improved on: unlike a
/// `defun` boundary, this one is cheap to reproduce exactly, and a Thing that
/// selects a different span than the user's Emacs did is a worse surprise
/// than a rule they have already met.
fn sentence(doc: &Document, pos: usize) -> Option<Range<usize>> {
    const ENDINGS: [char; 3] = ['.', '!', '?'];

    /// Whether an ending character at `at` really ends a sentence.
    fn terminal(doc: &Document, at: usize) -> bool {
        let Some(after) = next_char_boundary(doc, at) else {
            return true;
        };
        match char_at(doc, after) {
            None => true,
            Some('\n') => true,
            Some(' ') => next_char_boundary(doc, after)
                .and_then(|two| char_at(doc, two))
                .is_none_or(|c| c == ' ' || c == '\n'),
            Some(_) => false,
        }
    }
    if doc.byte_len() == 0 {
        return None;
    }
    let mut start = pos.min(doc.byte_len());
    while let Some(prev) = previous_char_boundary(doc, start) {
        if char_at(doc, prev).is_some_and(|c| ENDINGS.contains(&c)) && terminal(doc, prev) {
            break;
        }
        start = prev;
    }
    while start < doc.byte_len() && char_at(doc, start).is_some_and(char::is_whitespace) {
        start = next_char_boundary(doc, start)?;
    }
    let mut end = pos.min(doc.byte_len());
    loop {
        match char_at(doc, end) {
            Some(c) if ENDINGS.contains(&c) && terminal(doc, end) => {
                end = next_char_boundary(doc, end).unwrap_or(end);
                break;
            }
            None => break,
            _ => match next_char_boundary(doc, end) {
                Some(next) => end = next,
                None => break,
            },
        }
    }
    // Emacs's sentence stops at the punctuation, and where there is none it
    // stops at the end of the text rather than after the newline that follows
    // it. Trailing whitespace is not part of a sentence.
    while end > start
        && previous_char_boundary(doc, end)
            .and_then(|p| char_at(doc, p))
            .is_some_and(char::is_whitespace)
    {
        end = previous_char_boundary(doc, end).unwrap_or(end);
    }
    (start < end).then_some(start..end)
}

fn char_at(doc: &Document, at: usize) -> Option<char> {
    (at < doc.byte_len()).then(|| doc.text().char(doc.text().byte_to_char(at)))
}

#[cfg(test)]
mod tests;
