//! Buffers and positions for meowr. No I/O.
//!
//! Text lives in a [`ropey::Rope`]. That is forced rather than preferred:
//! `tree-house`, the syntax highlighter, takes a `ropey::RopeSlice` concretely
//! and depends on ropey 1.x, so ropey is re-exported here as part of the domain
//! rather than hidden as an implementation detail.

pub use ropey::{Rope, RopeSlice};

/// A unit of text being edited, addressed in bytes.
///
/// Byte offsets are the native position type because tree-sitter edits are
/// byte-based; the conversion to `u32` that tree-house's query API wants is
/// deliberately not made here, so it stays isolated at that boundary.
#[derive(Debug, Clone, Default)]
pub struct Document {
    text: Rope,
}

impl Document {
    pub fn from_text(text: &str) -> Self {
        Self {
            text: Rope::from_str(text),
        }
    }

    pub fn text(&self) -> &Rope {
        &self.text
    }

    pub fn byte_len(&self) -> usize {
        self.text.len_bytes()
    }

    /// The number of lines a reader would count.
    ///
    /// This is not `Rope::len_lines`, which counts the empty line after a
    /// trailing newline: a file ending in `\n` is not thereby one line longer.
    pub fn line_count(&self) -> usize {
        let lines = self.text.len_lines();
        if self.byte_len() > 0 && self.text.byte(self.byte_len() - 1) == b'\n' {
            lines - 1
        } else {
            lines
        }
    }

    /// The line at `idx`, including its trailing newline if it has one.
    pub fn line(&self, idx: usize) -> Option<RopeSlice<'_>> {
        (idx < self.line_count()).then(|| self.text.line(idx))
    }

    /// The byte offset at which line `idx` starts.
    pub fn line_to_byte(&self, idx: usize) -> Option<usize> {
        (idx < self.line_count()).then(|| self.text.line_to_byte(idx))
    }

    /// The line containing byte `offset`.
    pub fn byte_to_line(&self, offset: usize) -> Option<usize> {
        (offset < self.byte_len()).then(|| self.text.byte_to_line(offset))
    }

    /// Whether `offset` falls on a character boundary.
    ///
    /// Every byte offset crossing into tree-sitter, or used to slice text, must
    /// satisfy this; an offset inside a multi-byte character is a bug, not an
    /// edge case to round away.
    ///
    /// Note that ropey's own conversions will not tell you: `byte_to_char`
    /// silently rounds a mid-character offset down to the enclosing character,
    /// and `try_byte_to_char` errors only when out of bounds. Round-tripping is
    /// the only way to detect it.
    pub fn is_char_boundary(&self, offset: usize) -> bool {
        if offset > self.byte_len() {
            return false;
        }
        let ch = self.text.byte_to_char(offset);
        self.text.char_to_byte(ch) == offset
    }
}

/// One replacement in a **Foreign edit**: the bytes in `range` become `text`.
///
/// The range addresses the text as it stood *before* any replacement in the same
/// set, which is how language servers send a `TextEdit[]` (#157) and how a diff
/// describes hunks. Plain data, so the domain can hold one without knowing which
/// of the two produced it (#161, ADR-0013).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Replacement {
    pub range: core::ops::Range<usize>,
    pub text: String,
}

/// A line and a column, the column in **UTF-16 code units**: a [`Place`]
/// without the file, as a language server addresses text.
///
/// Not `Position`, which is LSP's wire type for it and a word the glossary's
/// **point** entry avoids.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LineColumn {
    pub line: usize,
    pub column: usize,
}

/// A line a [`LineEdit`] names that the document does not reach.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PastEnd {
    /// Counted from zero, as the edit counted it.
    pub line: usize,
}

/// A [`Replacement`] addressed by lines and columns rather than bytes -- a language
/// server's `TextEdit` (#167).
///
/// A server names text in files meowr has not read, so the edit cannot be
/// bytes until there is a Document to count them in: [`Document::replacement_for`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LineEdit {
    pub start: LineColumn,
    pub end: LineColumn,
    pub text: String,
}

/// How large a changed block may be before it is replaced whole rather than
/// refined into the words that changed (#162).
///
/// A bound so a formatter rewriting a large generated file cannot stall a save
/// on word-level diffing; past it the point may move within that block, which
/// is the trade. apheleia bounds a different step for the same reason -- its
/// point alignment, at 400 characters (`apheleia-max-alignment-size`) -- and
/// this is set higher because refining is cheap: a 16 KB block with no word in
/// common refines in under 2 ms.
const REFINE_LIMIT: usize = 16 * 1024;

/// The replacements that turn `old` into `new`, as small as a formatter's output
/// allows (#162, ADR-0013).
///
/// A formatter hands back the whole new text, and replacing the buffer with it
/// wholesale would move the point every time a line above it shifted -- on
/// format-on-save, constantly -- and give every fold one replacement spanning
/// it. So the text is diffed by line, and each changed block refined by word:
/// a reindented line becomes a replacement of its indentation, the code after
/// it is outside every replacement, and the point on that code stays on it.
///
/// Every range addresses `old`, so the result is a **Foreign edit**'s
/// replacements as they are: in order, and never overlapping.
pub fn replacements_between(old: &str, new: &str) -> Vec<Replacement> {
    runs(&lines(old), &lines(new))
        .into_iter()
        .flat_map(|(old_bytes, new_bytes)| refined(old_bytes.clone(), &old[old_bytes], &new[new_bytes]))
        .collect()
}

/// A changed block by word, or whole past [`REFINE_LIMIT`].
fn refined(at: core::ops::Range<usize>, old: &str, new: &str) -> Vec<Replacement> {
    if old.is_empty() || new.is_empty() || old.len().max(new.len()) > REFINE_LIMIT {
        return vec![Replacement {
            range: at,
            text: new.to_string(),
        }];
    }
    runs(&words(old), &words(new))
        .into_iter()
        .map(|(old_bytes, new_bytes)| Replacement {
            range: at.start + old_bytes.start..at.start + old_bytes.end,
            text: new[new_bytes].to_string(),
        })
        .collect()
}

/// A text's lines, each with its newline.
fn lines(text: &str) -> Vec<&str> {
    text.split_inclusive('\n').collect()
}

/// A text as words, runs of spaces and tabs, and newlines each on their own.
///
/// Not `similar`'s own word tokens, which put a newline and the indentation
/// after it in one token: reindenting two lines then replaced from the first
/// line's end into the second -- across the head of a fold starting there,
/// which opened. A newline alone keeps every replacement on its own line.
fn words(text: &str) -> Vec<&str> {
    #[derive(PartialEq)]
    enum Class {
        Newline,
        Blank,
        Word,
    }
    let class = |c: char| match c {
        '\n' => Class::Newline,
        c if c.is_whitespace() => Class::Blank,
        _ => Class::Word,
    };
    let mut tokens = Vec::new();
    let mut start = 0;
    let mut previous: Option<Class> = None;
    for (at, c) in text.char_indices() {
        let this = class(c);
        let joins = previous.as_ref().is_some_and(|p| *p == this && this != Class::Newline);
        if !joins && at > start {
            tokens.push(&text[start..at]);
            start = at;
        }
        previous = Some(this);
    }
    if start < text.len() {
        tokens.push(&text[start..]);
    }
    tokens
}

/// Each run of changes between two stretches of equal tokens, as the old and
/// new byte ranges it covers.
///
/// Runs rather than single operations: a deletion beside an insertion is one
/// replacement, not two that a reader has to know apply together.
fn runs(old: &[&str], new: &[&str]) -> Vec<(core::ops::Range<usize>, core::ops::Range<usize>)> {
    let old_at = boundaries(old);
    let new_at = boundaries(new);
    let ops = similar::capture_diff_slices(similar::Algorithm::Myers, old, new);
    let equal = |op: &similar::DiffOp| op.tag() == similar::DiffTag::Equal;
    ops.chunk_by(|a, b| equal(a) == equal(b))
        .filter(|run| !equal(&run[0]))
        .map(|run| {
            let (first, last) = (&run[0], &run[run.len() - 1]);
            (
                old_at[first.old_range().start]..old_at[last.old_range().end],
                new_at[first.new_range().start]..new_at[last.new_range().end],
            )
        })
        .collect()
}

/// The byte offset each token starts at, and the end of the last.
fn boundaries(tokens: &[&str]) -> Vec<usize> {
    core::iter::once(0)
        .chain(tokens.iter().scan(0, |at, token| {
            *at += token.len();
            Some(*at)
        }))
        .collect()
}

/// An edit that could not be applied.
///
/// Both variants exist because ropey would otherwise accept the offset and
/// round it, and a silently relocated edit is far worse than a refused one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditError {
    /// The offset lies past the end of the document.
    OutOfBounds,
    /// The offset falls inside a multi-byte character.
    NotACharBoundary,
}

impl core::fmt::Display for EditError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::OutOfBounds => write!(f, "offset is past the end of the document"),
            Self::NotACharBoundary => write!(f, "offset is inside a multi-byte character"),
        }
    }
}

impl core::error::Error for EditError {}

impl Document {
    /// Insert `text` at `at`, returning how many **bytes** were added.
    ///
    /// The byte count is the return value because callers advance a point by
    /// it, and the number of bytes is not the number of characters.
    pub fn insert(&mut self, at: usize, text: &str) -> Result<usize, EditError> {
        let char_at = self.checked_char_index(at)?;
        self.text.insert(char_at, text);
        Ok(text.len())
    }

    /// Delete the bytes in `range`.
    pub fn delete(&mut self, range: core::ops::Range<usize>) -> Result<(), EditError> {
        let start = self.checked_char_index(range.start)?;
        let end = self.checked_char_index(range.end)?;
        self.text.remove(start..end);
        Ok(())
    }

    /// Convert a byte offset to a char index, refusing anything ropey would
    /// otherwise round into place.
    fn checked_char_index(&self, at: usize) -> Result<usize, EditError> {
        if at > self.byte_len() {
            return Err(EditError::OutOfBounds);
        }
        if !self.is_char_boundary(at) {
            return Err(EditError::NotACharBoundary);
        }
        Ok(self.text.byte_to_char(at))
    }
}

impl From<&str> for Document {
    fn from(text: &str) -> Self {
        Self::from_text(text)
    }
}

#[cfg(test)]
mod tests;

/// How bad a diagnostic is, in the four grades LSP defines.
///
/// Kept apart rather than collapsed into one "there is a problem here"
/// because an error and a hint must not look the same: the colour of a
/// squiggle is the only thing that tells a reader whether to stop what they
/// are doing, and a render layer handed one name for all four could not
/// decide. The name is `&'static str` so it reaches [`crate`]'s consumers the
/// way a highlight name already does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Error,
    Warning,
    Information,
    Hint,
}

impl Severity {
    /// What a list of diagnostics calls this one.
    ///
    /// The four apart in a column of text, where [`Severity::highlight`] tells
    /// them apart in colour. A reader scanning `*diagnostics*` is asking the
    /// same question the squiggle's colour answers, and a list that made them
    /// all look alike would be worse than the squiggles already on screen.
    /// LSP's own words, shortened only where the protocol's is longer than the
    /// column needs: `Information` is `info` everywhere a human writes it.
    pub fn name(self) -> &'static str {
        match self {
            Severity::Error => "error",
            Severity::Warning => "warning",
            Severity::Information => "info",
            Severity::Hint => "hint",
        }
    }

    /// The highlight name the render layer keys on.
    pub fn highlight(self) -> &'static str {
        match self {
            Severity::Error => "diagnostic.error",
            Severity::Warning => "diagnostic.warning",
            Severity::Information => "diagnostic.info",
            Severity::Hint => "diagnostic.hint",
        }
    }
}

/// Something a language server said about a span of a buffer.
///
/// A **Buffer property**, not a fact about the syntax tree — which is the
/// distinction #111 settled and the reason this is not a `meowr_syntax::Span`.
/// A `Styled` run is recomputed from the current tree every frame; a
/// diagnostic is something the session was *told* and has to go on believing
/// until told otherwise, which is the shape the Grab already has.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostic {
    pub range: core::ops::Range<usize>,
    pub message: String,
    pub severity: Severity,
}

impl Document {
    /// A `{line, character}` position as a byte offset.
    ///
    /// `character` is counted in **UTF-16 code units**, which is LSP's default
    /// and not a detail that can be skipped: one emoji is two of them, and one
    /// byte short is a squiggle under the wrong column for the rest of the line.
    ///
    /// Here rather than in the binary's LSP module, where it was written. It
    /// takes a `Document` and two numbers and returns a third; there is no
    /// protocol in it, and [`Place`] needs the same walk for a reason that has
    /// nothing to do with a language server.
    ///
    /// A line *after* the last one is clamped to the end rather than refused,
    /// because the end of a range covering the final line is spelled that way.
    pub fn offset_at(&self, line: usize, character: usize) -> Option<usize> {
        let Some(start) = self.line_to_byte(line) else {
            return Some(self.byte_len());
        };
        let mut utf16 = 0;
        let mut bytes = 0;
        for ch in self.line(line)?.chars() {
            if utf16 >= character || ch == '\n' {
                break;
            }
            utf16 += ch.len_utf16();
            bytes += ch.len_utf8();
        }
        Some(start + bytes)
    }

    /// `edit` in bytes, or the line it names that is past the end.
    ///
    /// Stricter than [`Document::offset_at`] about lines, because a
    /// replacement is written where a diagnostic is only drawn: an edit
    /// computed against a longer file is not this file's, and clamping it
    /// would put its text at the end of this one (#167). The line just after
    /// the last is still the end, since that is how a range to the end of the
    /// text is spelled. A column past a line's end is clamped, which is what
    /// the protocol says a client does.
    pub fn replacement_for(&self, edit: &LineEdit) -> Result<Replacement, PastEnd> {
        Ok(Replacement {
            range: self.range_for(edit.start, edit.end)?,
            text: edit.text.clone(),
        })
    }

    /// The bytes from `start` to `end`, on [`Document::replacement_for`]'s
    /// terms: a line past the end is refused.
    pub fn range_for(
        &self,
        start: LineColumn,
        end: LineColumn,
    ) -> Result<core::ops::Range<usize>, PastEnd> {
        let offset = |at: LineColumn| {
            (at.line <= self.line_count())
                .then(|| self.offset_at(at.line, at.column))
                .flatten()
                .ok_or(PastEnd { line: at.line })
        };
        Ok(offset(start)?..offset(end)?)
    }

    /// A byte offset as a `{line, character}` position, which is the other
    /// direction.
    ///
    /// The walk [`Place`] is made of. A `Diagnostic` carries a byte range and
    /// a Place carries a line and a UTF-16 column, so a list of diagnostics
    /// cannot be built without converting between the two -- and the session
    /// was already doing this arithmetic inline to say where the point is.
    /// Two copies of it is how the two come to disagree, which is the same
    /// argument that brought `offset_at` here from the binary.
    ///
    /// `None` past the end, where [`Document::offset_at`] clamps instead. The
    /// asymmetry is deliberate: a column past the end of a line is a real
    /// position spelled loosely, and an offset past the end of the text is not
    /// a position at all.
    pub fn position_at(&self, offset: usize) -> Option<(usize, usize)> {
        let line = self.byte_to_line(offset)?;
        let start = self.line_to_byte(line)?;
        let character = self
            .text()
            .byte_slice(start..offset.max(start))
            .chars()
            .map(char::len_utf16)
            .sum();
        Some((line, character))
    }
}

/// The regions of a buffer collapsed to their first lines.
///
/// A **Buffer property** (ADR-0012): it belongs to one buffer and is changed by
/// a command, so a buffer shown in two Windows folds the same in both. That is
/// `hs-minor-mode`'s behaviour and `treesit-fold`'s, and the ADR takes it
/// against the more obviously useful alternative on the grounds that the
/// author's daily editor is the better evidence about what is useful.
///
/// **Byte ranges rather than line numbers**, which is what the Grab and a
/// `Diagnostic` already are and for the same reason: a fold kept as "lines 2 to
/// 5" hides the wrong region the moment a line is typed above it, and says
/// nothing about having done so. The lines come back at the moment there is a
/// Document to ask, which is every draw.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Folds {
    /// Each fold's span, from the first byte of its head line to the end of
    /// the last line it hides.
    regions: Vec<core::ops::Range<usize>>,
}

impl Folds {
    /// Open every fold whose edge one of `ranges` crosses.
    ///
    /// A **Foreign edit** rewrites text at positions nobody chose, and a fold
    /// whose start or end falls strictly inside a replaced span no longer covers
    /// the code that was folded -- so it opens, rather than being guessed at
    /// (ADR-0013). A replacement wholly inside a fold, wholly outside it, or
    /// starting or ending exactly on its edge leaves it folded, to shift with
    /// the text like any other edit: a formatter reindenting a fold's head line
    /// must not open every fold in the file.
    pub fn open_crossed(&mut self, ranges: &[core::ops::Range<usize>]) {
        let crosses = |edge: usize| ranges.iter().any(|range| range.start < edge && edge < range.end);
        self.regions.retain(|region| !crosses(region.start) && !crosses(region.end));
    }

    /// Collapse a span of text.
    ///
    /// Nested folds are kept as they are rather than merged: folding a `match`
    /// inside a function and then the function is two folds, and unfolding the
    /// function should leave the `match` folded — which is what `treesit-fold`
    /// does, and what merging would lose.
    pub fn fold(&mut self, region: core::ops::Range<usize>) {
        if region.is_empty() || self.regions.contains(&region) {
            return;
        }
        self.regions.push(region);
    }

    /// Take away the fold whose head is `line`, if there is one.
    ///
    /// `false` when there was nothing to unfold, which is what lets a toggle
    /// tell the two cases apart without asking twice.
    pub fn unfold(&mut self, doc: &Document, line: usize) -> bool {
        let before = self.regions.len();
        self.regions
            .retain(|region| doc.byte_to_line(region.start) != Some(line));
        self.regions.len() != before
    }

    /// Open every fold hiding `line`, and no others.
    ///
    /// **Enough to show it, and no more.** Nested folds both hide a line, so
    /// "the one in the way" is not one fold — but a fold elsewhere in the
    /// buffer was never in the way, and opening the lot would make a jump undo
    /// work the reader had done.
    ///
    /// `false` when the line was already visible, so a caller can tell a jump
    /// that had to open something from one that did not.
    pub fn reveal(&mut self, doc: &Document, line: usize) -> bool {
        if !self.hides(doc, line) {
            return false;
        }
        self.regions.retain(|region| {
            // The same test `hides` makes, asked of one region: a line inside
            // it and past its head. Read off `span_of` so the two cannot come
            // to disagree about where a fold ends.
            !span_of(doc, region).is_some_and(|(head, last)| line > head && line <= last)
        });
        true
    }

    pub fn clear(&mut self) {
        self.regions.clear();
    }

    pub fn is_empty(&self) -> bool {
        self.regions.is_empty()
    }

    /// Whether `line` is hidden by any fold.
    ///
    /// Inside one and not its head: the head stays, because it is what you
    /// unfold from and hiding it would leave nothing to press.
    pub fn hides(&self, doc: &Document, line: usize) -> bool {
        self.spans(doc)
            .any(|(head, last)| line > head && line <= last)
    }

    /// Whether `line` is the head of a fold, and so carries the ellipsis.
    pub fn heads(&self, doc: &Document, line: usize) -> bool {
        self.spans(doc).any(|(head, _)| head == line)
    }

    /// The first line at or after `line` that nothing hides.
    ///
    /// Every fold's head is visible, so this terminates: the worst case walks
    /// to the end of the longest fold containing `line`.
    pub fn next_visible(&self, doc: &Document, line: usize) -> usize {
        let mut at = line;
        while self.hides(doc, at) {
            at += 1;
        }
        at
    }

    /// The last line at or before `line` that nothing hides.
    pub fn previous_visible(&self, doc: &Document, line: usize) -> usize {
        let mut at = line;
        while at > 0 && self.hides(doc, at) {
            at -= 1;
        }
        at
    }

    /// `n` visible lines on from `line`, stopping at the last line.
    ///
    /// What "down one" has to mean once something is hidden, and the reason
    /// ADR-0012 calls the scroll calculation the real work in folding.
    pub fn forward(&self, doc: &Document, line: usize, n: usize) -> usize {
        let last = doc.line_count().saturating_sub(1);
        let mut at = self.next_visible(doc, line.min(last));
        for _ in 0..n {
            let next = self.next_visible(doc, (at + 1).min(last));
            if next == at || next > last {
                break;
            }
            at = next;
        }
        at.min(last)
    }

    /// `n` visible lines back from `line`, stopping at the top.
    pub fn backward(&self, doc: &Document, line: usize, n: usize) -> usize {
        let mut at = self.previous_visible(doc, line);
        for _ in 0..n {
            if at == 0 {
                break;
            }
            at = self.previous_visible(doc, at - 1);
        }
        at
    }

    /// Where each fold begins and ends, in lines.
    fn spans<'a>(&'a self, doc: &'a Document) -> impl Iterator<Item = (usize, usize)> + 'a {
        self.regions.iter().filter_map(|region| span_of(doc, region))
    }

    /// Move every fold past an edit, with the caller's own arithmetic.
    ///
    /// **The shift is handed in rather than written here**, which is the rule
    /// the Grab and a diagnostic already follow in as many words: two copies of
    /// it is how the two come to disagree. A fold that did not move would hide
    /// the wrong region after a line was typed above it, and say nothing.
    ///
    /// A region an edit has emptied is dropped: the text it collapsed is gone,
    /// so there is nothing left to hide.
    pub fn shift_each(&mut self, shift: impl Fn(core::ops::Range<usize>) -> core::ops::Range<usize>) {
        self.regions = self
            .regions
            .drain(..)
            .map(shift)
            .filter(|region| !region.is_empty())
            .collect();
    }
}

/// Where one fold begins and ends, in lines.
///
/// `None` for a region that spans no whole line, which is a fold with nothing
/// to hide. The end is exclusive, so the last hidden line is the one holding
/// the byte before it — and a region ending exactly at a line's first byte
/// hides up to the line above, not that line.
fn span_of(doc: &Document, region: &core::ops::Range<usize>) -> Option<(usize, usize)> {
    let head = doc.byte_to_line(region.start)?;
    let last = doc.byte_to_line(region.end.saturating_sub(1))?;
    (last > head).then_some((head, last))
}

/// What a search is looking for.
///
/// Two kinds rather than a string and a flag beside it, because the two are
/// different *questions* and the type is what says which. The distinction is
/// not a nicety: meowr's `symbol` Thing is Emacs's syntax classes `w` and `_`
/// together, and the `_` class in the standard table is exactly `&*+-/<=>_|`,
/// so the symbol at the point really can be `*` — which as a regex is a parse
/// error rather than a search. `SPC s .` asking for a regex would report a row
/// that could not run, for a symbol that is perfectly searchable.
///
/// Here rather than in the binary or the session because both ends speak it:
/// the session says which kind a row means, and the searcher is what knows the
/// flag. [`Place`] below is the same vocabulary in the other direction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Pattern {
    /// What the user typed, read as a regular expression — which is what
    /// `consult-ripgrep` does with the same input, and most of what project
    /// search is for.
    Regex(String),
    /// Text to be found as it stands: the symbol at the point, which is a name
    /// rather than a pattern.
    Literal(String),
}

impl Pattern {
    /// The characters of it, whichever kind it is.
    pub fn text(&self) -> &str {
        match self {
            Pattern::Regex(text) | Pattern::Literal(text) => text,
        }
    }
}

/// Somewhere to go: a path, a line, a column, and sometimes that line's text.
///
/// What a list of many results is a list *of* — references, symbols, search
/// hits, a diagnostic — and what choosing one of them jumps to (ADR-0011).
///
/// **Not a byte offset**, which is what [`Diagnostic`] carries and what the
/// rest of this crate speaks. An offset cannot be computed for a file that has
/// not been read, and a Place names somewhere in a file that is usually not
/// open; requiring one would mean reading four hundred files before a picker of
/// four hundred references could be shown. It becomes an offset through
/// [`Place::offset_in`], at the one moment there is a `Document`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Place {
    pub path: String,
    pub line: usize,
    /// The column, in **UTF-16 code units**.
    ///
    /// The protocol's unit rather than this crate's, because a language server
    /// is the one source that *cannot* convert for itself: it names a position
    /// in a file it has not read, so it cannot count bytes in one. Every other
    /// source holds the line's text and can convert to this; none of them can
    /// ask a server to.
    pub column: usize,
    /// The line, where the source had it to give.
    ///
    /// Grep hands back the matched line for free; a language server hands back
    /// positions and nothing else. A `None` is a fact about LSP rather than a
    /// gap in this type, and it is what makes the case for previewing the
    /// selected row instead of pretending every list reads at a glance.
    pub text: Option<String>,
}

impl Place {
    pub fn new(path: impl Into<String>, line: usize, column: usize) -> Self {
        Self {
            path: path.into(),
            line,
            column,
            text: None,
        }
    }

    /// The same Place, carrying the line a source was able to give.
    pub fn showing(mut self, text: impl Into<String>) -> Self {
        self.text = Some(text.into());
        self
    }

    /// Where this lands in `doc`, which is the only thing that can say.
    pub fn offset_in(&self, doc: &Document) -> Option<usize> {
        doc.offset_at(self.line, self.column)
    }
}
