//! The plain-data row model meowr renders. No decisions, no ratatui.
//!
//! This crate turns a [`Document`] and a [`Viewport`] into the rows that should
//! appear on screen. It makes no rendering decisions and knows nothing about
//! terminals, so it can be tested without one.

use meowr_core::Selection;
use meowr_domain::{Document, Folds};
use std::ops::Range;

/// The slice of a document a window is showing.
///
/// Width is deliberately absent: there is no horizontal scrolling yet, and
/// clipping a line to the window's width is the renderer's job, not a fact
/// about the view.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Viewport {
    /// Index of the first visible line.
    pub top: usize,
    /// How many lines fit.
    pub height: usize,
}

/// One line as it should appear.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    /// The line's index in the document, so the renderer can label it.
    pub line: usize,
    /// The line's text, without its trailing newline.
    pub text: String,
    /// The selected span, as byte offsets **within `text`**.
    ///
    /// Clipped to this row, so a selection crossing lines appears as a span on
    /// each of them and the renderer never has to know about the document.
    pub selected: Option<Range<usize>>,
    /// Where the point sits within `text`, when it is on this row.
    ///
    /// Separate from `selected` because the span alone cannot tell you which
    /// end you are at, and a backward selection is a different thing to a
    /// forward one.
    pub cursor: Option<usize>,
    /// Whether this is the line to highlight — `hl-line`.
    ///
    /// A row rather than a column range, because the highlight runs to the
    /// window's edge and past the end of the text, which a `Styled` run cannot
    /// say: those only cover columns that have a character in them.
    pub current: bool,
}

/// The rows visible through `viewport`.
///
/// Yields fewer rows than the viewport's height when the document runs out;
/// padding the remainder is a rendering decision and is not made here.
pub fn rows(doc: &Document, viewport: Viewport) -> Vec<Row> {
    // No selection to place and no window to be focused: this is the plain
    // "what does the text look like" call, and the highlight belongs to a
    // Window rather than to a Document.
    rows_with(doc, viewport, None, false, &Folds::default())
}

/// The rows visible through `viewport`, marked up with `selection`.
/// Rows for one window, with the current line marked if `hl_line` asks.
///
/// `hl_line` is the caller's decision and not this function's: it is false for
/// an unfocused window and false when the Option is off, and neither of those
/// is a fact about a Document. Doom's own rule is the same shape —
/// `global-hl-line-sticky-flag 'window`, with the reason in its comment:
/// "Don't display line highlights in non-focused windows, for performance
/// sake and to reduce UI clutter."
///
/// **It is not a range over lines any more** (ADR-0012). With folds it yields
/// *the next `height` unfolded lines from `top`*, so a `top` that has landed
/// inside a fold begins at the next line something can be drawn for rather than
/// coming back empty. `Row::line` goes on naming the document line, which is
/// what keeps the gutter truthful: the numbers jump — 17, 18, 32, 33 — and that
/// jump is the second fold indicator, free.
pub fn rows_with(
    doc: &Document,
    viewport: Viewport,
    selection: Option<Selection>,
    hl_line: bool,
    folds: &Folds,
) -> Vec<Row> {
    let mut at = folds.next_visible(doc, viewport.top);
    std::iter::from_fn(|| {
        let line = at;
        // The next line to try, worked out before this one is yielded: `line +
        // 1` may be inside the fold this one heads.
        at = folds.next_visible(doc, line + 1);
        (at > line).then_some(line)
    })
    .take(viewport.height)
        .map_while(|line| {
            let slice = doc.line(line)?;
            let mut text = slice.to_string().trim_end_matches('\n').to_string();
            let start = doc.line_to_byte(line)?;
            // Before the ellipsis is added, so the selection and the point are
            // placed against the line's own text rather than against a marker
            // that is not in the buffer.
            let end = start + text.len();
            // `hl-line` goes away while a region is live, which is Doom's
            // rule and Doom's reason — its `activate-mark-hook` says it in as
            // many words: "it obscures the bounds of the selection, depending
            // on the active theme". A band behind a selection makes the
            // selection's own edges harder to find, which is the opposite of
            // what a selection is for. The condition is the same one the match
            // below turns on, which is why it reads off it.
            let current = hl_line
                && selection.is_some_and(|sel| {
                    sel.selection_type().is_none() && (start..=end).contains(&sel.pos())
                });
            let (selected, cursor) = match selection {
                Some(sel) if sel.selection_type().is_some() => {
                    let range = sel.range();
                    let overlap = range.start.max(start)..range.end.min(end);
                    let selected = (overlap.start < overlap.end)
                        .then(|| overlap.start - start..overlap.end - start);
                    let cursor = (start..=end)
                        .contains(&sel.pos())
                        .then(|| sel.pos() - start);
                    (selected, cursor)
                }
                // `meow--selection-type` answering nil: an ordinary motion
                // with nothing selected, which is most of editing. Nothing is
                // selected, but the point still has to land somewhere — a
                // `None` here is what let the render layer skip drawing it.
                Some(sel) => {
                    let cursor = (start..=end)
                        .contains(&sel.pos())
                        .then(|| sel.pos() - start);
                    (None, cursor)
                }
                None => (None, None),
            };
            // `+fold-ellipsis`, verbatim. Appended last so nothing above has
            // to know it is there.
            if folds.heads(doc, line) {
                text.push_str(ELLIPSIS);
            }
            Some(Row {
                line,
                current,
                text,
                selected,
                cursor,
            })
        })
        .collect()
}

/// Doom's `+fold-ellipsis`, verbatim.
///
/// An ellipsis somebody has looked at for years is worth more than one chosen
/// here (ADR-0012). The spaces are part of it.
pub const ELLIPSIS: &str = " [...] ";

/// A numbered position the user could expand to, placed on the grid.
///
/// Plain data, like [`Row`]: the view layer says *where* and *what*, and the
/// render layer decides how it looks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Hint {
    /// Row within the viewport, not the line number in the document.
    pub row: usize,
    /// Character offset into that row's text.
    pub column: usize,
    /// The digit to press. `10` is reached by `0`, as Meow has it.
    pub label: u32,
}

/// Place expand hints on the visible grid, dropping any that fall outside it.
pub fn hints(doc: &Document, viewport: Viewport, positions: &[usize]) -> Vec<Hint> {
    positions
        .iter()
        .enumerate()
        .filter_map(|(index, &offset)| {
            let line = doc.byte_to_line(offset)?;
            let row = line.checked_sub(viewport.top)?;
            if row >= viewport.height {
                return None;
            }
            let start = doc.line_to_byte(line)?;
            let column = doc.text().byte_to_char(offset) - doc.text().byte_to_char(start);
            Some(Hint {
                row,
                column,
                label: index as u32 + 1,
            })
        })
        .collect()
}

/// How wide the line-number gutter must be to hold every line the document
/// has, `display-line-numbers-width`'s job.
///
/// Sized from the document rather than from what a `Viewport` happens to show
/// (a scrolled-to page could be all three-digit lines in a thousand-line
/// file), so scrolling never shifts the column the text sits in. One column
/// is added for separation from the text, which Emacs's own gutter always
/// keeps.
///
/// A digit count in a crate whose `Viewport` doc says width is "the
/// renderer's job, not a fact about the view" — and this is a narrower
/// question than that one settles. What that doc rules out is clipping a
/// *line* to a *window's* width, which depends on where the window happens to
/// be sized; how many digits the document's own line count needs depends on
/// nothing the render layer knows that this crate does not already hold.
/// `modeline::short_path` sits in the same place for the same reason: a
/// formatting fact about the data, not a decision about where to draw it.
pub fn gutter_width(total_lines: usize) -> usize {
    total_lines.max(1).to_string().len() + 1
}

pub mod modeline;

#[cfg(test)]
mod tests;

/// A highlighted run, placed on the visible grid.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Styled {
    pub row: usize,
    pub columns: core::ops::Range<usize>,
    /// The highlight name, e.g. `keyword`. The render layer decides the colour.
    pub name: &'static str,
}

/// Place syntax spans on the grid, splitting any that cross a line.
///
/// The view layer converts byte offsets into rows and columns; the render
/// layer never sees a byte offset, and the syntax layer never sees a row.
pub fn styled(doc: &Document, viewport: Viewport, spans: &[meowr_syntax::Span]) -> Vec<Styled> {
    let mut placed = Vec::new();
    for span in spans {
        let mut at = span.range.start;
        while at < span.range.end {
            let Some(line) = doc.byte_to_line(at) else {
                break;
            };
            let line_start = doc.line_to_byte(line).unwrap_or(0);
            let line_end = doc
                .line_to_byte(line + 1)
                .unwrap_or_else(|| doc.byte_len())
                .min(span.range.end.max(line_start));
            let stop = span.range.end.min(line_end);
            if let Some(row) = line.checked_sub(viewport.top)
                && row < viewport.height
                && at < stop
            {
                let column = |byte: usize| {
                    doc.text().byte_to_char(byte) - doc.text().byte_to_char(line_start)
                };
                placed.push(Styled {
                    row,
                    columns: column(at)..column(stop),
                    name: span.name(),
                });
            }
            if stop <= at {
                break;
            }
            at = stop;
        }
    }
    placed
}

/// One row of the narrowing widget, as plain data.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NarrowRow {
    pub text: String,
    pub annotation: Option<String>,
    /// Character indices matched by the input, for marking.
    pub matched: Vec<u32>,
    pub selected: bool,
}

/// What the Echo Area shows while narrowing.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct NarrowView {
    /// Nearest the input line first. The widget grows **upward**, so the
    /// render layer reverses this to draw it — the selection ends up next to
    /// what the user is typing rather than a dozen rows away from it.
    pub rows: Vec<NarrowRow>,
    /// The prompt line: source, if narrowed, then the input.
    pub prompt: String,
    pub input: String,
    /// Why there are no rows, where there is a reason worth saying.
    ///
    /// The widget's own line rather than the Echo Area, because ADR-0004 gives
    /// the Bottom to exactly one owner and an open picker *is* that owner -- a
    /// refusal sent to the Echo Area while this is on screen is a sentence
    /// nobody ever sees. Drawn nearest the prompt, where the eye already is.
    pub note: Option<String>,
}

/// The Grab, placed on the visible grid.
///
/// Drawn distinctly from the selection because they are different things: the
/// point is that a Grab persists while the selection moves, and a user who
/// cannot see the difference cannot use it.
pub fn grabbed(
    doc: &Document,
    viewport: Viewport,
    grab: Option<core::ops::Range<usize>>,
) -> Vec<Styled> {
    let Some(range) = grab else {
        return Vec::new();
    };
    styled(
        doc,
        viewport,
        &[meowr_syntax::Span {
            range,
            // Reusing the span type rather than inventing a parallel one; the
            // name is what the render layer keys on.
            highlight: 0,
        }],
    )
    .into_iter()
    .map(|s| Styled { name: "grab", ..s })
    .collect()
}

/// Diagnostics, placed on the visible grid.
///
/// The same shape [`grabbed`] is, and for the same reason: a stored range the
/// session was told about, turned into rows and columns so the render layer
/// never sees a byte offset. The severity picks the highlight name, which is
/// all this layer says about how a squiggle should look.
pub fn diagnostics(
    doc: &Document,
    viewport: Viewport,
    diagnostics: &[meowr_domain::Diagnostic],
) -> Vec<Styled> {
    diagnostics
        .iter()
        .flat_map(|diagnostic| {
            let name = diagnostic.severity.highlight();
            styled(
                doc,
                viewport,
                &[meowr_syntax::Span {
                    range: diagnostic.range.clone(),
                    // As `grabbed` does: the span type is reused rather than a
                    // parallel one invented, and the name is what the render
                    // layer keys on.
                    highlight: 0,
                }],
            )
            .into_iter()
            .map(move |s| Styled { name, ..s })
        })
        .collect()
}

/// What the Echo Area shows for a hover.
///
/// Plain data like [`NarrowView`]: the lines are already wrapped and capped,
/// so the render layer places rows and decides nothing. #111 settled that a
/// hover is the Echo Area grown to fit rather than a floating box (ADR-0004
/// leaves nowhere to float) or a Window — `eldoc` and `lsp-ui-doc` are both
/// transient, and a Window in the tree is a more permanent thing than that.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct HoverView {
    /// Top line first, as it reads.
    pub lines: Vec<String>,
    /// Whether the cap left something out, so the render layer can say so.
    /// Silently showing the first three lines of a twelve-line doc comment
    /// reads as "that is all there is".
    pub truncated: bool,
}

/// How many rows a hover may take before it is truncated.
///
/// `max-mini-window-height` is 0.25 of the frame in Emacs and
/// `lsp-ui-doc-max-height` is 13; ten is between them and is a whole number of
/// rows rather than a fraction the caller has to compute. A caller with a
/// short terminal caps it again against the height it actually has.
pub const HOVER_MAX_ROWS: usize = 10;

/// How many columns a hover may take before it wraps.
///
/// `lsp-ui-doc-max-width` is 150, and the reference configuration sets 150 --
/// restating `lsp-ui`'s own default rather than changing it, which is why this
/// is a constant here and not an Option. It bites only on a frame wider than a
/// doc comment wants to be; a prose line run across 300 columns loses the eye
/// on the way back to the start of the next one.
pub const HOVER_MAX_COLS: usize = 150;

/// Wrap a hover to `width` and cap it at `cap` rows.
///
/// Hard line breaks are kept — a server's hover is markdown, and a signature
/// on one line with its doc comment below is the commonest shape there is.
/// Long lines wrap at spaces where they can and mid-word where they cannot,
/// because a single unbreakable token is often exactly the type name the user
/// asked about.
pub fn hover(text: &str, width: usize, cap: usize) -> HoverView {
    if width == 0 || cap == 0 || text.trim().is_empty() {
        return HoverView::default();
    }
    let mut lines: Vec<String> = Vec::new();
    for hard in text.trim_end().split('\n') {
        if hard.trim().is_empty() {
            lines.push(String::new());
            continue;
        }
        lines.extend(wrapped(hard.trim_end(), width));
    }
    let truncated = lines.len() > cap;
    lines.truncate(cap);
    HoverView { lines, truncated }
}

/// One hard line, broken to `width` columns.
///
/// Counted in characters rather than bytes: a hover is prose and routinely
/// carries text no narrower than one byte per column.
fn wrapped(line: &str, width: usize) -> Vec<String> {
    // A line that already fits is left exactly as it is, indentation included.
    // Rebuilding it from its words would strip the leading whitespace, and a
    // hover is mostly code: a struct whose fields all sit in column one reads
    // as a different struct.
    if line.chars().count() <= width {
        return vec![line.to_string()];
    }
    let mut out: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut used = 0;
    for word in line.split(' ').filter(|w| !w.is_empty()) {
        let len = word.chars().count();
        // A word that cannot fit on a line of its own is broken across as many
        // as it needs; dropping it would lose the name that was asked about.
        if len > width {
            if used > 0 {
                out.push(std::mem::take(&mut current));
                used = 0;
            }
            let mut chars = word.chars().peekable();
            while chars.peek().is_some() {
                let chunk: String = chars.by_ref().take(width).collect();
                used = chunk.chars().count();
                current = chunk;
                if used == width {
                    out.push(std::mem::take(&mut current));
                    used = 0;
                }
            }
            continue;
        }
        let needed = if used == 0 { len } else { len + 1 };
        if used + needed > width {
            out.push(std::mem::take(&mut current));
            used = 0;
        }
        if used > 0 {
            current.push(' ');
            used += 1;
        }
        current.push_str(word);
        used += len;
    }
    if !current.is_empty() {
        out.push(current);
    }
    out
}

/// A rectangle of the frame, in cells.
///
/// Here rather than in `meowr-app` because it is plain data about the screen,
/// which is this crate's subject, and because [`Screen`] needs it: a model of
/// the frame cannot live below the type that says where a Window sits.
/// `meowr_app::window` re-exports it, so the tree that computes areas still
/// names it where it is used.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Area {
    pub x: u16,
    pub y: u16,
    pub width: u16,
    pub height: u16,
}

impl Area {
    pub fn viewport(&self, top: usize) -> Viewport {
        Viewport {
            top,
            height: self.height as usize,
        }
    }
}

/// What occupies the Bottom of the frame, of the four things that can.
///
/// Exactly one does, which is #116's own criterion, and saying so as a type is
/// what makes that true by construction rather than by two `if` cascades
/// agreeing. Nothing floats -- ADR-0004 -- so whatever this names takes rows
/// off the bottom and the Windows above are laid out shorter.
///
/// The type is here, where the rest of the plain-data model is. *Which* one it
/// is, is [`Session::bottom`]'s to say: the glossary is explicit that it is a
/// fact about the session and not about the layer drawing it, and this crate
/// makes no decisions. The value travels down; the choosing does not come up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Bottom {
    /// The narrowing widget, which is a modal read and outranks everything.
    Narrow,
    /// The leader menu.
    Menu,
    /// A hover: the Echo Area grown to fit.
    Hover,
    /// One row of ordinary message.
    Echo,
    /// Nothing; every row belongs to a Window.
    None,
}

/// One Window's worth of the screen.
///
/// A struct rather than the eight-tuple this was: the fields travel together
/// everywhere they go, and `Option<String>` in the fourth position of eight is
/// exactly the element a reader has to stop and work out.
///
/// Named for the Window it shows. The glossary spends a paragraph on why this
/// is not a *pane* -- that word belongs to editors whose splits are windows in
/// their own right -- and [`NarrowView`] and [`HoverView`] had already settled
/// the spelling for a plain-data picture of one thing on screen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WindowView {
    pub area: Area,
    pub rows: Vec<Row>,
    pub styled: Vec<Styled>,
    /// What to show *instead of* the rows: a load that failed, or one that has
    /// not finished.
    pub message: Option<String>,
    pub hints: Vec<Hint>,
    pub modeline: modeline::Fitted,
    pub focused: bool,
    pub gutter: Option<usize>,
}

/// What the frame would show, as plain data.
///
/// Compared against the frame on screen to decide whether to paint -- #127. It
/// is every read the draw used to make inline, named and kept, so that "has
/// anything changed" is a question about the screen rather than about whether
/// somebody remembered to set a flag.
///
/// `PartialEq` is the whole point and every member already had it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Screen {
    pub narrow: Option<NarrowView>,
    pub menu: Vec<(String, String, bool)>,
    pub prefix: String,
    pub echo: Option<String>,
    pub hover: Option<HoverView>,
    pub bottom: Bottom,
    pub windows: Vec<WindowView>,
}
