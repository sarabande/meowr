//! The Meow state machine: typed selections, motions and history. No I/O.
//!
//! Meow's observable behaviour is not expressible as a keymap. A selection is
//! typed on two axes, commands branch on the *previous* selection's type, and a
//! history stack sits underneath. This crate is that calculus, and Fidelity
//! (see `~/docs/meowr-context.md`) binds it exactly.

use meowr_domain::Document;

pub mod edit;
pub mod thing;
pub mod undo;
use std::ops::Range;

/// Whether repeating the motion that made a selection grows it or replaces it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Axis {
    Expand,
    Select,
}

/// What produced a selection.
///
/// **Deliberately not an enum.** Meow builds a selection type as
/// `(cons 'expand type)` where `type` is a symbol handed in by the caller:
/// `meow-mark-thing` and `meow-next-thing` pass the *thing* being selected, so
/// every registered thing — 18 in the reference configuration, six of them
/// added by `meow-tree-sitter` — is a possible kind, alongside the literals
/// below. A closed enum would be wrong, and reading the elisp is what caught
/// it: an earlier draft of the spec claimed nine fixed combinations.
///
/// Kept separate from [`Axis`] because Meow branches on the axis alone
/// (`meow-expand-selection-type` sets it independently) and on the kind alone.
///
/// A `&'static str` keeps [`Selection`] `Copy` and is sufficient while every
/// thing is registered in Rust. If Steel is ever able to register a thing at
/// runtime, this needs interning rather than leaking. Not M2: a Plugin there
/// composes named commands and does not reach the Thing registry, so nothing
/// yet asks for a `Kind` that outlives the binary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Kind(&'static str);

impl Kind {
    pub const CHAR: Self = Self("char");
    pub const LINE: Self = Self("line");
    pub const JOIN: Self = Self("join");
    pub const BLOCK: Self = Self("block");
    pub const TRANSIENT: Self = Self("transient");
    pub const VISIT: Self = Self("visit");
    pub const FIND: Self = Self("find");
    pub const TILL: Self = Self("till");
    pub const WORD: Self = Self("word");
    pub const SYMBOL: Self = Self("symbol");

    /// The kind named by a thing, for selections the Thing system produces.
    pub const fn thing(name: &'static str) -> Self {
        Self(name)
    }

    /// Every kind this crate names, in the order they are declared above.
    pub const KNOWN: &'static [Self] = &[
        Self::CHAR,
        Self::LINE,
        Self::JOIN,
        Self::BLOCK,
        Self::TRANSIENT,
        Self::VISIT,
        Self::FIND,
        Self::TILL,
        Self::WORD,
        Self::SYMBOL,
    ];

    /// Look a kind up by name.
    ///
    /// `None` for a name this crate does not know, which is not the same as an
    /// invalid kind: a thing registered elsewhere is a legitimate kind whose
    /// name simply is not here. Callers that accept those need the Thing
    /// registry, not this.
    pub fn from_name(name: &str) -> Option<Self> {
        Self::KNOWN.iter().copied().find(|k| k.name() == name)
    }

    pub const fn name(self) -> &'static str {
        self.0
    }
}

/// The pair of navigation functions a selection stashes for `meow-expand`.
///
/// Meow keeps `(backward . forward)` closures in `meow--expand-nav-function`,
/// set by whichever command made the selection. A pair of function pointers
/// cannot live in a `Copy` struct without the same trouble `Handler` has, and
/// it does not need to: for every kind M1 can expand, the pair is determined
/// by the kind. So the *kind of navigation* is stashed and the functions are
/// looked up.
///
/// The variants are exactly `meow--select-expandable-p`'s list — `word`,
/// `symbol`, `line`, `block`, `find`, `till`. `char` is deliberately absent:
/// a char selection cannot be expanded by a digit, which is why `i 2` does
/// nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Nav {
    Word,
    Symbol,
    Line,
    Block,
    Find,
    Till,
}

/// A span of the buffer with a direction, and the type that made it.
///
/// Positions are byte offsets. Direction runs mark → pos, so a backward
/// selection has `pos < mark`; this is not the same as `range()`, which is
/// always ordered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Selection {
    selection_type: Option<(Axis, Kind)>,
    mark: usize,
    pos: usize,
}

impl Selection {
    /// A bare point: no selection, no type.
    pub fn at(pos: usize) -> Self {
        Self {
            selection_type: None,
            mark: pos,
            pos,
        }
    }

    /// An active region with no Meow selection type.
    ///
    /// Emacs distinguishes "the region is active" from "Meow made a
    /// selection", and one command depends on it: `meow-pop-grab` goes through
    /// `secondary-selection-to-region`, which activates the region without
    /// touching `meow--selection`. A typed selection there would answer
    /// `meow--selection-type` where Emacs answers nil.
    pub fn spanning(mark: usize, pos: usize) -> Self {
        Self {
            selection_type: None,
            mark,
            pos,
        }
    }

    pub fn typed(axis: Axis, kind: Kind, mark: usize, pos: usize) -> Self {
        Self {
            selection_type: Some((axis, kind)),
            mark,
            pos,
        }
    }

    pub fn selection_type(&self) -> Option<(Axis, Kind)> {
        self.selection_type
    }

    pub fn axis(&self) -> Option<Axis> {
        self.selection_type.map(|(a, _)| a)
    }

    pub fn kind(&self) -> Option<Kind> {
        self.selection_type.map(|(_, k)| k)
    }

    pub fn mark(&self) -> usize {
        self.mark
    }

    pub fn pos(&self) -> usize {
        self.pos
    }

    pub fn is_backward(&self) -> bool {
        self.pos < self.mark
    }

    pub fn is_empty(&self) -> bool {
        self.mark == self.pos
    }

    /// The bytes covered, always ordered.
    pub fn range(&self) -> Range<usize> {
        if self.is_backward() {
            self.pos..self.mark
        } else {
            self.mark..self.pos
        }
    }

    /// Where this selection sits after one replacement in a **Foreign edit**.
    ///
    /// [`Selection::after_edit`]'s rule except inside the replaced span, where
    /// a position keeps its distance from the span's start, clamped to the new
    /// text, rather than collapsing to it: a point in the middle of a renamed
    /// identifier stays in the middle (#161, ADR-0013). Typing keeps the
    /// collapse, because a typed edit is *at* the point.
    pub fn after_replacement(self, at: usize, removed: usize, inserted: usize) -> Self {
        let moved = |offset: usize| {
            if removed == 0 && offset == at {
                // An insertion exactly here belongs before this position: a
                // formatter adding a line above an unindented one inserts at its
                // first byte, and the point is on the code, not on the gap (#162).
                offset + inserted
            } else if offset > at && offset < at + removed {
                at + (offset - at).min(inserted)
            } else {
                shift(offset, at, removed, inserted)
            }
        };
        Self {
            selection_type: self.selection_type,
            mark: moved(self.mark),
            pos: moved(self.pos),
        }
    }

    /// This selection with both ends moved back onto the start of the
    /// character they fall in, if either falls inside one.
    pub fn on_char_boundaries(self, doc: &Document) -> Self {
        let text = doc.text();
        let edge = |offset: usize| {
            let offset = offset.min(text.len_bytes());
            text.char_to_byte(text.byte_to_char(offset))
        };
        Self {
            selection_type: self.selection_type,
            mark: edge(self.mark),
            pos: edge(self.pos),
        }
    }

    /// Where this selection sits after an edit elsewhere in the buffer.
    ///
    /// Emacs uses markers, so a selection follows the text it covers rather
    /// than staying at the offsets it was made at. `at` is where the edit
    /// happened, `removed` and `inserted` its byte counts.
    pub fn after_edit(self, at: usize, removed: usize, inserted: usize) -> Self {
        Self {
            selection_type: self.selection_type,
            mark: shift(self.mark, at, removed, inserted),
            pos: shift(self.pos, at, removed, inserted),
        }
    }

    /// Swap mark and pos, covering the same bytes in the other direction.
    pub fn reversed(self) -> Self {
        Self {
            selection_type: self.selection_type,
            mark: self.pos,
            pos: self.mark,
        }
    }
}

/// Move one offset to account for an edit.
///
/// An offset inside the removed span collapses to the start of the edit: the
/// text it pointed at is gone, and the edit site is the only honest answer.
fn shift(offset: usize, at: usize, removed: usize, inserted: usize) -> usize {
    if offset <= at {
        offset
    } else if offset >= at + removed {
        offset - removed + inserted
    } else {
        at
    }
}

/// The current selection and the stack of ones it replaced.
///
/// The history is why `z` can walk backwards through what you had selected. It
/// is the concept a port most easily drops by accident, and it is observable:
/// it is bound to a key.
#[derive(Debug, Clone)]
pub struct SelectionState {
    current: Selection,
    history: Vec<Selection>,
    /// What a digit would expand by, stashed by the command that made the
    /// selection. `None` means the digits have nothing to work with.
    ///
    /// Emacs keeps this in a global `defvar`. It lives with the selection
    /// here, for the reason recorded against the selection history: state that
    /// belongs to a buffer should not survive a buffer switch.
    nav: Option<Nav>,
    /// The character `f` last found and `t` last stopped before.
    ///
    /// Kept apart from the selection history on purpose: `;`-style repetition
    /// of a find is a different thing from walking back through what was
    /// selected, and Meow keeps `meow--last-find` and `meow--last-till` as
    /// their own variables for the same reason.
    last_find: Option<char>,
    last_till: Option<char>,
    /// The column a run of vertical motions is aiming for.
    ///
    /// Emacs keeps this across a short line so that moving down through `xy`
    /// returns you to the column you started from rather than to column 2.
    /// Any non-vertical motion abandons it.
    goal_column: Option<usize>,
    /// Emacs's mark, which outlives the selection.
    ///
    /// In Emacs the mark is a per-buffer marker and "is the region active" is
    /// a separate question; `kill-ring-save` needs only the former. Keeping
    /// the mark *inside* the selection made cancelling one throw the other
    /// away — which is why `y` after `G` signalled instead of copying, and why
    /// the `meow-save` fallback found an empty range even though it was wired.
    ///
    /// `None` is Emacs's unset mark in a fresh buffer, where `kill-ring-save`
    /// says "The mark is not set now" rather than copying nothing.
    mark: Option<usize>,
}

impl SelectionState {
    pub fn new(current: Selection) -> Self {
        Self {
            current,
            history: Vec::new(),
            nav: None,
            last_find: None,
            last_till: None,
            goal_column: None,
            mark: None,
        }
    }

    /// Where the mark is, active region or not. See the field.
    pub fn mark(&self) -> Option<usize> {
        self.mark
    }

    pub fn nav(&self) -> Option<Nav> {
        self.nav
    }

    /// Stash what a digit should expand by. Called by the commands that make
    /// an expandable selection, mirroring `meow--maybe-highlight-num-positions`.
    pub fn set_nav(&mut self, nav: Nav) {
        self.nav = Some(nav);
    }

    /// Forget it, so the digits do nothing.
    ///
    /// `meow-mark-thing` does this without looking like it does: it calls
    /// `meow--highlight-regexp-in-buffer`, whose first act is to clear
    /// `meow--expand-nav-function`. That is why `m 2` leaves the selection
    /// exactly as `m` left it — verified against Emacs, not inferred.
    pub fn clear_nav(&mut self) {
        self.nav = None;
    }

    pub fn last_find(&self) -> Option<char> {
        self.last_find
    }

    pub fn last_till(&self) -> Option<char> {
        self.last_till
    }

    pub fn current(&self) -> Selection {
        self.current
    }

    pub fn history_depth(&self) -> usize {
        self.history.len()
    }

    /// Install a selection, recording the one it replaced.
    ///
    /// A selection identical to the top of the stack is not pushed again, so a
    /// run of motions does not fill the history with duplicates.
    pub fn select(&mut self, selection: Selection) {
        if self.history.last() != Some(&self.current) {
            self.history.push(self.current);
        }
        self.current = selection;
        // `meow--select` calls `meow--set-mark`, and `meow--select-without-
        // history` calls `push-mark`. Both put the mark where the selection
        // says, so both are the same line here.
        self.mark = Some(selection.mark());
    }

    /// Install a selection without recording it.
    ///
    /// Meow has a `meow--select-without-history` for the cases that must not
    /// record; without it, motions that merely adjust the current selection
    /// would fill the stack with near-duplicates.
    pub fn select_without_history(&mut self, selection: Selection) {
        self.current = selection;
        self.mark = Some(selection.mark());
    }

    /// Restore the previous selection. `false` when there was none.
    pub fn pop(&mut self) -> bool {
        match self.history.pop() {
            Some(previous) => {
                self.current = previous;
                true
            }
            None => false,
        }
    }

    pub fn clear_history(&mut self) {
        self.history.clear();
    }

    /// `meow--cancel-selection`: deactivate the region and clear the history,
    /// leaving the mark behind.
    ///
    /// The mark is not part of what a cancellation destroys — Emacs only
    /// *deactivates* it, which is the whole reason `kill-ring-save` still has
    /// something to copy after `meow-grab`. Where there is a history the mark
    /// first moves back to where the run of selections began:
    /// `(cadar (last meow--selection-history))` is the mark of the oldest
    /// entry, and the list is pushed onto, so the oldest is the front here.
    ///
    /// The history goes because `meow--cancel-selection` clears it explicitly.
    /// Leaving it would make `z` after `g` walk back into selections the user
    /// has already thrown away.
    pub fn cancel(&mut self) {
        if let Some(oldest) = self.history.first() {
            self.mark = Some(oldest.mark());
        }
        self.history.clear();
        self.current = Selection::at(self.current.pos());
    }

    pub fn goal_column(&self) -> Option<usize> {
        self.goal_column
    }

    pub fn set_goal_column(&mut self, column: usize) {
        self.goal_column = Some(column);
    }

    pub fn forget_goal_column(&mut self) {
        self.goal_column = None;
    }

    /// Move the current selection, the whole history and the mark the same way.
    ///
    /// One walk over the three, handed the movement: an ordinary edit, a
    /// replacement in a **Foreign edit**, or settling onto character edges.
    /// Written once so a fourth movement cannot forget the mark.
    pub fn map_each(&mut self, movement: impl Fn(Selection) -> Selection) {
        self.current = movement(self.current);
        for selection in &mut self.history {
            *selection = movement(*selection);
        }
        // A mark is a marker, and a marker moves with the text it sits in.
        self.mark = self.mark.map(|mark| movement(Selection::at(mark)).pos());
    }

    /// Move the current selection and the whole history to account for an edit.
    pub fn after_edit(&mut self, at: usize, removed: usize, inserted: usize) {
        self.map_each(|selection| selection.after_edit(at, removed, inserted));
    }
}

// --- motions --------------------------------------------------------------
//
// Every motion takes the document and the selection state, and a flag saying
// whether it expands. The pair (plain, expanding) is one function rather than
// two because the only difference is where the mark ends up, and Meow's
// behaviour there depends on the *previous* selection's type.

/// Move the point, for the *plain* motions `h i e n`.
///
/// Meow's rule, from the docstring "Will cancel all other selection, except
/// char selection": a selection survives the motion only if it is exactly
/// `(expand . char)`, in which case the mark stays put and the span changes.
/// Anything else is cancelled outright. **No plain motion ever creates a
/// selection** — an earlier draft of this file had them all creating
/// `(expand . char)`, which made `h` on a bare point select a character. The
/// differential harness is what caught it.
///
/// Neither branch records history: Emacs moves point and mark directly here,
/// without going through `meow--select`.
fn move_point(state: &mut SelectionState, pos: usize, guarded: bool) {
    let current = state.current();
    let typed = current.selection_type();
    if typed == Some((Axis::Expand, Kind::CHAR)) {
        state.select_without_history(Selection::typed(
            Axis::Expand,
            Kind::CHAR,
            current.mark(),
            pos,
        ));
        return;
    }
    // `meow-left` and `meow-right` guard the cancel with `(region-active-p)`;
    // `meow-prev` and `meow-next` do not. The difference is invisible until
    // something has left the point untyped but the history full — a kill —
    // and then `i` keeps the history where `n` throws it away. Two spellings
    // of the same idea in the elisp, three lines apart, and not the same.
    if !guarded || typed.is_some() {
        cancel(state);
    }
    state.select_without_history(Selection::at(pos));
}

/// Move the point, for the *expanding* motions `H I E N`.
///
/// Meow makes a `(expand . char)` selection over the span it already had —
/// whatever kind that span used to be — records it, and only then moves. So
/// `H` after `l` keeps the line's extent but reports `char`, and the history
/// gains exactly one entry however far the point then travels.
fn expand_point(state: &mut SelectionState, pos: usize) {
    let current = state.current();
    let mark = match current.selection_type() {
        Some(_) => current.mark(),
        None => current.pos(),
    };
    state.select(Selection::typed(
        Axis::Expand,
        Kind::CHAR,
        mark,
        current.pos(),
    ));
    state.select_without_history(Selection::typed(Axis::Expand, Kind::CHAR, mark, pos));
}

/// Take the point to `pos`, either dragging the selection with it or leaving
/// it behind.
///
/// The axis is the glossary's **Expand**, and it is the same one the resulting
/// selection carries — `H` expands where `h` moves — so it is that type and
/// not a `bool` that the reader has to remember the polarity of.
fn step(state: &mut SelectionState, pos: usize, axis: Axis, guarded: bool) {
    if axis == Axis::Expand {
        expand_point(state, pos)
    } else {
        move_point(state, pos, guarded)
    }
}

/// Move one character left. `false` when there was nowhere to go.
///
/// The return value is whether Emacs would have *signalled*: `meow-left`
/// cancels the selection and then presses `C-b`, and `backward-char` at the
/// beginning of the buffer raises `beginning-of-buffer`. The cancel still
/// happened, so the failure is reported rather than short-circuited.
pub fn left(doc: &Document, state: &mut SelectionState, axis: Axis) -> bool {
    state.forget_goal_column();
    let moved = previous_boundary(doc, state.current().pos());
    let pos = moved.unwrap_or(0);
    step(state, pos, axis, true);
    moved.is_some()
}

/// Move one character right. `false` when there was nowhere to go.
pub fn right(doc: &Document, state: &mut SelectionState, axis: Axis) -> bool {
    state.forget_goal_column();
    let moved = next_boundary(doc, state.current().pos());
    let pos = moved.unwrap_or_else(|| doc.byte_len());
    step(state, pos, axis, true);
    moved.is_some()
}

/// Move one line up. `false` when there was no line to move to.
pub fn prev(doc: &Document, state: &mut SelectionState, axis: Axis) -> bool {
    vertical(doc, state, axis, -1)
}

/// Move one line down. `false` when there was no line to move to.
pub fn next(doc: &Document, state: &mut SelectionState, axis: Axis) -> bool {
    vertical(doc, state, axis, 1)
}

/// Move `n` lines, as `meow-next` does with a numeric prefix: "use with
/// numeric argument to move multiple lines at once".
///
/// The universal argument takes the other branch — see [`to_buffer_edge`].
///
/// A negative `n` runs the other way, which is what `next-line` does with a
/// negative prefix. `false` when the buffer ran out — Emacs moves as far as it
/// can and *then* signals `end-of-buffer`, and `vertical` below already does
/// the moving half, so stopping at the first refusal reports the signal
/// without losing the movement that preceded it.
pub fn next_lines(doc: &Document, state: &mut SelectionState, axis: Axis, n: i32) -> bool {
    let delta = if n < 0 { -1 } else { 1 };
    for _ in 0..n.unsigned_abs() {
        if !vertical(doc, state, axis, delta) {
            return false;
        }
    }
    true
}

/// `meow-prev` with a count: `next_lines` the other way about.
pub fn prev_lines(doc: &Document, state: &mut SelectionState, axis: Axis, n: i32) -> bool {
    next_lines(doc, state, axis, n.saturating_neg())
}

/// `meow-next` and `meow-prev` under a universal argument: "use with universal
/// argument to move to the last line of buffer", and the first line going up.
///
/// `(goto-char (point-max))` and `(goto-char (point-min))` are the whole of
/// the two branches. They go through the same [`step`] the counted motions do,
/// so the selection is cancelled or carried exactly as it would be by a move
/// of one line — the jump is where the point ends up, not a different kind of
/// motion.
///
/// Unguarded, because `meow-next` and `meow-prev` cancel unconditionally where
/// `meow-left` and `meow-right` guard it with `(region-active-p)`.
pub fn to_buffer_edge(doc: &Document, state: &mut SelectionState, axis: Axis, down: bool) {
    state.forget_goal_column();
    step(state, if down { doc.byte_len() } else { 0 }, axis, false);
}

fn vertical(doc: &Document, state: &mut SelectionState, axis: Axis, delta: isize) -> bool {
    let pos = state.current().pos();
    // The rope's line for the fallback, not `line_count() - 1`. At `byte_len`
    // `Document::byte_to_line` answers `None` — the offset is not *in* a line
    // by its counting — and taking the last counted line there put the point
    // one line early, so a move to the line below it "succeeded" without
    // moving and the end of the buffer never signalled.
    let line = doc
        .byte_to_line(pos)
        .unwrap_or_else(|| doc.text().byte_to_line(pos));
    let column = state.goal_column().unwrap_or_else(|| column_of(doc, pos));
    // No line to move to. Emacs's `line-move` does not leave the point where
    // it was: it goes to the end of the buffer it was heading for and *then*
    // signals, so `e` on the first line lands on offset 0. The selection half
    // of an expanding motion runs before the signal, so `E` at the top of the
    // buffer still leaves a selection behind.
    // The rope's line count, not `Document::line_count`, for the reason given
    // on `line_bounds`: Emacs counts the empty line a trailing newline leaves
    // behind, so `n` on the second-to-last line of a file ending in `\n`
    // succeeds rather than signalling.
    let Some(target) = line
        .checked_add_signed(delta)
        .filter(|l| *l < doc.text().len_lines())
    else {
        state.set_goal_column(column);
        let edge = if delta < 0 { 0 } else { doc.byte_len() };
        step(state, edge, axis, false);
        return false;
    };
    let pos = byte_at_column(doc, target, column);
    state.set_goal_column(column);
    // `char`, not `line`: the vertical motions are char selections that happen
    // to move by a line. `meow-line` is the one that makes a line selection.
    step(state, pos, axis, false);
    true
}

/// How many characters into its line `pos` is.
fn column_of(doc: &Document, pos: usize) -> usize {
    let Some(line) = doc.byte_to_line(pos) else {
        return 0;
    };
    let start = doc.line_to_byte(line).unwrap_or(0);
    doc.text().byte_to_char(pos) - doc.text().byte_to_char(start)
}

/// The byte offset `column` characters into `line`, clamped to its end.
fn byte_at_column(doc: &Document, line: usize, column: usize) -> usize {
    if line >= doc.text().len_lines() {
        return doc.byte_len();
    }
    let start = doc.text().line_to_byte(line);
    let text = doc.text().line(line).to_string();
    let usable = text.trim_end_matches('\n').chars().count().min(column);
    let start_char = doc.text().byte_to_char(start);
    doc.text().char_to_byte(start_char + usable)
}

/// The next character boundary after `pos`.
pub fn next_char_boundary(doc: &Document, pos: usize) -> Option<usize> {
    next_boundary(doc, pos)
}

/// The character boundary before `pos`.
pub fn previous_char_boundary(doc: &Document, pos: usize) -> Option<usize> {
    previous_boundary(doc, pos)
}

fn next_boundary(doc: &Document, pos: usize) -> Option<usize> {
    (pos + 1..=doc.byte_len()).find(|&at| doc.is_char_boundary(at))
}

fn previous_boundary(doc: &Document, pos: usize) -> Option<usize> {
    (0..pos).rev().find(|&at| doc.is_char_boundary(at))
}

#[cfg(test)]
mod tests;

// --- things ---------------------------------------------------------------
//
// Word and symbol membership is the **standard syntax table**, transcribed
// from a batch Emacs. Meow resolves these through `bounds-of-thing-at-point`,
// which reads whatever table the mode installed; meowr has no modes, so the
// standard table — `fundamental-mode`'s, and a mode-less buffer's — is the
// one to be faithful to, and it is the one the differential harness compares
// against.
//
// What remains is that Emacs's tables are per-mode: `=` is a symbol
// constituent here and punctuation in a C-like mode. That is a real gap and a
// much narrower one than the guess it replaces, and it is the sort of thing
// the tree-sitter Things answer rather than a table would.

/// Emacs's word constituents, syntax class `w`.
///
/// `$` and `%` are in the standard table alongside the letters and digits.
/// Enumerated from a batch Emacs rather than remembered — the surprising
/// entries are the ones a guess leaves out.
fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '$' || c == '%'
}

pub fn is_symbol_char(c: char) -> bool {
    is_symbol(c)
}

/// Emacs's symbol constituents, syntax classes `w` and `_` together — which is
/// what `bounds-of-thing-at-point 'symbol` walks over, and so what
/// `meow-symbol-thing` means.
///
/// The `_` class in the standard table is exactly `&*+-/<=>_|`, so `+=` is one
/// symbol and a bare `=` is a symbol by itself. This was `alphanumeric | _ | -`
/// — a guess, recorded as one — until the differential harness caught it: on
/// `let s = "("; f(x)`, `B` from the `(` lands on the `=` in Emacs and landed
/// on the `s` here.
///
/// The **standard** table, which is what `fundamental-mode` uses and so what a
/// buffer with no mode should use. Emacs's tables are per-mode and meowr has
/// no modes; that is the debt that remains, and it is a narrower one than the
/// guess this replaces.
fn is_symbol(c: char) -> bool {
    is_word(c) || matches!(c, '&' | '*' | '+' | '-' | '/' | '<' | '=' | '>' | '_' | '|')
}

fn char_at(doc: &Document, at: usize) -> Option<char> {
    (at < doc.byte_len()).then(|| doc.text().char(doc.text().byte_to_char(at)))
}

/// The bounds of the thing containing `pos`, if any.
fn bounds_of_thing(doc: &Document, pos: usize, member: fn(char) -> bool) -> Option<Range<usize>> {
    // Emacs's `bounds-of-thing-at-point` answers for the thing *ending* at the
    // point as well as the one containing it, which is why `m` with the point
    // on the space after `hello` still marks `hello`. Probed against Emacs
    // rather than assumed: without it, `w m` leaves `w`'s selection in place
    // and the axis comes out `select` where Meow reports `expand`.
    let before = previous_boundary(doc, pos);
    let anchor = if char_at(doc, pos).is_some_and(member) {
        pos
    } else if before.and_then(|p| char_at(doc, p)).is_some_and(member) {
        before?
    } else {
        return None;
    };

    Some(scan(doc, anchor, Toward::Backward, member)..scan(doc, anchor, Toward::Forward, member))
}

/// Walk to the far end of the next thing, forwards or backwards.
fn end_of_next_thing(
    doc: &Document,
    from: usize,
    member: fn(char) -> bool,
    forward: bool,
) -> usize {
    // Over whatever is *not* the thing, and then over the thing. Which
    // character a direction looks at is `steps`'s business, not this one's.
    let toward = Toward::forward_if(forward);
    let gap = scan(doc, from, toward, |c| !member(c));
    scan(doc, gap, toward, member)
}

/// Meow's `meow-mark-thing`: select the thing under the point, expandably.
fn mark_thing(
    doc: &Document,
    state: &mut SelectionState,
    kind: Kind,
    member: fn(char) -> bool,
    backward: bool,
) {
    state.forget_goal_column();
    // `meow-mark-thing` passes a regexp-format, which sends it through
    // `meow--highlight-regexp-in-buffer`, whose first act is to clear
    // `meow--expand-nav-function`. So `m` leaves the digits with nothing to
    // expand — surprising, and confirmed against Emacs.
    state.clear_nav();
    if let Some(bounds) = bounds_of_thing(doc, state.current().pos(), member) {
        // `meow--select` is handed BACKWARD, which only decides which end the
        // point lands on — the bounds are the same either way. So a negative
        // argument reverses the mark, and a count does nothing at all.
        let (mark, pos) = if backward {
            (bounds.end, bounds.start)
        } else {
            (bounds.start, bounds.end)
        };
        state.select(Selection::typed(Axis::Expand, kind, mark, pos));
    }
}

/// Meow's `meow-next-thing`.
///
/// Two branches on the *previous* selection, both load-bearing: a selection of
/// a different kind is cancelled rather than extended, and the new selection
/// expands only if the old one was already expanding this same kind.
/// Otherwise it is `select`, which is what "non-expandable" means in the
/// docstrings.
fn next_thing(
    doc: &Document,
    state: &mut SelectionState,
    kind: Kind,
    member: fn(char) -> bool,
    n: i32,
) {
    state.forget_goal_column();
    // `(interactive "p")` down to `(forward-thing thing n)`: the sign is the
    // direction and the magnitude is how many things to cross. Zero moves
    // nothing, and `meow--direction-forward` runs for it, as `(< n 0)` says.
    let forward = n >= 0;
    // Ported verbatim from `meow-next-thing`, which cancels *before* it looks
    // at whether the motion can move at all. So `b` at the beginning of the
    // buffer still throws away whatever was selected.
    if state.current().kind() != Some(kind) {
        cancel(state);
    }
    let current = state.current();
    let expand = current.selection_type() == Some((Axis::Expand, kind));
    let axis = if expand { Axis::Expand } else { Axis::Select };

    // `meow--direction-forward`/`-backward` runs before the motion is even
    // attempted, so it turns the selection round whether or not the motion
    // then succeeds: `m b` at the beginning of the buffer cannot move, but it
    // still leaves the point on the far end. That is a state change, not a
    // local variable.
    let span = current.range();
    if expand {
        let (mark, pos) = if forward {
            (span.start, span.end)
        } else {
            (span.end, span.start)
        };
        state.select_without_history(Selection::typed(Axis::Expand, kind, mark, pos));
    }

    let from = state.current().pos();
    let mut pos = from;
    for _ in 0..n.unsigned_abs() {
        let next = end_of_next_thing(doc, pos, member, forward);
        if next == pos {
            break;
        }
        pos = next;
    }
    // `(unless (= (point) m) (point))`: a motion that cannot move leaves no
    // selection behind — not an empty one, and not the old one restored.
    if pos == from {
        return;
    }

    let mark = if expand {
        // `meow--make-selection` with EXPAND merges against the live region,
        // keeping whichever end is further from where the point is going.
        if pos > from { span.start } else { span.end }
    } else {
        fix_thing_selection_mark(doc, member, pos, from)
    };
    state.select(Selection::typed(axis, kind, mark, pos));
    // `meow--maybe-highlight-num-positions` stashes the thing's pair here.
    state.set_nav(if kind == Kind::SYMBOL {
        Nav::Symbol
    } else {
        Nav::Word
    });
}

/// Emacs's `" _w"` syntax classes: whitespace, symbol constituent, word.
///
/// Hardcoded, where `meow-next-thing` picks the `car` or the `cadr` of
/// `meow-next-thing-include-syntax` according to `(> n 0)`. That is harmless
/// today because the default has the same string on both sides —
/// `'((word " _w" " _w") (symbol " _w" " _w"))` — but it is a `defcustom`, so
/// a user who sets it asymmetrically would find the sign ignored. Recorded
/// rather than implemented: there is nowhere to configure it from yet.
///
/// Approximated from [`is_symbol`], which is close enough for the buffers M1
/// is judged against but is not a syntax table: `-` is a symbol constituent
/// here and punctuation in Emacs's standard table. The Thing registry is where
/// that gets a real answer.
fn is_word_syntax(c: char) -> bool {
    c.is_whitespace() || is_symbol(c)
}

/// Meow's `meow--fix-thing-selection-mark`.
///
/// The mark of a thing motion is not simply where the point started: it is
/// pulled in to the bounds of the thing actually landed on, then let back out
/// over `" _w"` syntax as far as the original mark. Without it, `b` from the
/// `a` of `qux(arg)` marks at the `a` where Meow marks at the `(`.
fn fix_thing_selection_mark(
    doc: &Document,
    member: fn(char) -> bool,
    pos: usize,
    mark: usize,
) -> usize {
    let backward = mark > pos;
    // "Point must be before the end of the word to get the bounds correctly."
    let anchor = if backward {
        pos
    } else {
        previous_boundary(doc, pos).unwrap_or(pos)
    };
    let bounds = bounds_of_thing(doc, anchor, member).unwrap_or(mark..mark);
    let from = if backward {
        mark.min(bounds.end)
    } else {
        mark.max(bounds.start)
    };
    // `skip-syntax-forward`/`-backward`, bounded by the original mark. The
    // direction is the *opposite* of the selection's — a backward selection
    // skips forward towards its mark — which is why `forward_if` is handed
    // `backward` and is not a typo. That, and which side `mark` bounds, were
    // the whole of what differed between the two branches this used to have.
    walk(
        doc,
        from,
        Toward::forward_if(backward),
        |looking_at, landing| {
            let within = if backward {
                landing <= mark
            } else {
                landing >= mark
            };
            within && char_at(doc, looking_at).is_some_and(is_word_syntax)
        },
    )
}

pub fn mark_word(doc: &Document, state: &mut SelectionState, backward: bool) {
    mark_thing(doc, state, Kind::WORD, is_word, backward);
}

pub fn mark_symbol(doc: &Document, state: &mut SelectionState, backward: bool) {
    mark_thing(doc, state, Kind::SYMBOL, is_symbol, backward);
}

pub fn next_word(doc: &Document, state: &mut SelectionState, n: i32) {
    next_thing(doc, state, Kind::WORD, is_word, n);
}

pub fn back_word(doc: &Document, state: &mut SelectionState, n: i32) {
    next_thing(doc, state, Kind::WORD, is_word, n.saturating_neg());
}

pub fn next_symbol(doc: &Document, state: &mut SelectionState, n: i32) {
    next_thing(doc, state, Kind::SYMBOL, is_symbol, n);
}

pub fn back_symbol(doc: &Document, state: &mut SelectionState, n: i32) {
    next_thing(doc, state, Kind::SYMBOL, is_symbol, n.saturating_neg());
}

/// Select the line the point is on. **The newline is not included** —
/// `meow-line`'s docstring says so in its first sentence, and the goldens
/// agree: `l` on the first line of `hello world\n` ends at offset 11, not 12.
///
/// Repeating extends by a line in the direction the selection already runs,
/// keeping the far end where it was; any other kind of selection is replaced.
pub fn select_line(doc: &Document, state: &mut SelectionState, count: i32) {
    state.forget_goal_column();
    let current = state.current();
    let extending = current.selection_type() == Some((Axis::Expand, Kind::LINE));
    let backward = extending && current.is_backward();
    // `(n (if backward (- n) n))` then `(forward (> n 0))`. The count's sign
    // is a direction and an existing backward selection flips it, so `-` on a
    // selection already running backwards extends it *forwards*.
    let forward = if backward { -count } else { count } > 0;

    // `(if backward (- n) n)` again, this time for the distance rather than
    // the direction: the magnitude is how many lines `forward-line` moves.
    let n = if backward { -count } else { count };
    let (start, end) = line_bounds(doc, current.pos());
    let (mark, pos) = match (extending, forward) {
        // A fresh selection runs the way the count points: `(m (if forward
        // (line-beginning-position) (line-end-position)))`, and the point at
        // the other end, `(unless (= n 1) (forward-line (1- n)))` lines away.
        // Without the sign this was always forward, which is what `-l` did
        // wrong; without the magnitude `C-u 3 l` took one line.
        (false, true) => (start, line_bounds(doc, forward_line(doc, end, n - 1)).1),
        // `(forward-line (1+ n))` then `(when (meow--empty-line-p)
        // (backward-char 1))` then `(line-beginning-position)`: on an empty
        // line going backwards there is nothing between the two ends, so Meow
        // steps off it and takes the *previous* line's beginning.
        (false, false) => {
            let at = forward_line(doc, start, n + 1);
            let (from, _) = line_bounds(doc, at);
            let empty = line_bounds(doc, at).0 == line_bounds(doc, at).1;
            let from = if empty && from > 0 {
                line_bounds(doc, from - 1).0
            } else {
                from
            };
            (end, from)
        }
        // Extending moves the *point* `forward-line n` and keeps the mark:
        // `(orig (if cancel-sel (point) (mark t)))`.
        (true, false) => (
            current.mark(),
            line_bounds(doc, forward_line(doc, start, n)).0,
        ),
        (true, true) => (
            current.mark(),
            line_bounds(doc, forward_line(doc, end, n)).1,
        ),
    };
    state.select(Selection::typed(Axis::Expand, Kind::LINE, mark, pos));
    state.set_nav(Nav::Line);
}

/// Where each digit would take the point, for the numbered hints Meow paints
/// after a selection that can be expanded.
///
/// `max` positions, in order, starting with what `1` would do. Empty when
/// there is nothing to expand — which is the same guard `meow-expand` uses,
/// so the hints appear exactly when the digits would work.
///
/// Meow caps these at `meow-expand-hint-counts`, 30 per type. Ten is enough
/// here because ten is what a single digit can reach, and a hint you cannot
/// press is decoration.
pub fn hint_positions(doc: &Document, state: &SelectionState, max: usize) -> Vec<usize> {
    let current = state.current();
    let (Some(nav), Some(_)) = (state.nav(), current.kind()) else {
        return Vec::new();
    };
    let backward = current.is_backward();
    let mut positions = Vec::with_capacity(max);
    let mut pos = current.pos();
    for _ in 0..max {
        let next = match nav {
            Nav::Word => end_of_next_thing(doc, pos, is_word, !backward),
            Nav::Symbol => end_of_next_thing(doc, pos, is_symbol, !backward),
            Nav::Line => line_step(doc, pos, backward),
            Nav::Block => pos,
            // `meow--find-continue-forward` and its till counterpart: the
            // character is remembered, so a digit repeats the find rather
            // than doing nothing.
            Nav::Find => match state.last_find() {
                Some(ch) => find_char(doc, pos, ch, !backward).unwrap_or(pos),
                None => pos,
            },
            Nav::Till => match state.last_till() {
                Some(ch) => till_char(doc, pos, ch, !backward).unwrap_or(pos),
                None => pos,
            },
        };
        // Stop rather than repeat: a column of hints all pointing at the end
        // of the buffer is worse than a shorter column.
        if next == pos {
            break;
        }
        pos = next;
        positions.push(pos);
    }
    positions
}

/// One line further out in `backward`'s direction, as `meow--forward-line-1`
/// and `meow--backward-line-1` do it with `meow--expanding-p` bound.
fn line_step(doc: &Document, pos: usize, backward: bool) -> usize {
    let (start, end) = line_bounds(doc, pos);
    if backward {
        if start > 0 {
            line_bounds(doc, start - 1).0
        } else {
            start
        }
    } else if end < doc.byte_len() {
        line_bounds(doc, end + 1).1
    } else {
        end
    }
}

/// The offset just past the next `ch`, searching from `from`.
fn find_char(doc: &Document, from: usize, ch: char, forward: bool) -> Option<usize> {
    steps(doc, from, Toward::forward_if(forward))
        .find(|&(looking_at, _)| char_at(doc, looking_at) == Some(ch))
        .map(|(_, landing)| landing)
}

/// As [`find_char`], but stopping *before* the character rather than after it.
///
/// Skips one character first, so `t` twice in a row does not sit still on the
/// character it already stopped at — `meow--till-continue-forward` does the
/// same `forward-char 1`.
fn till_char(doc: &Document, from: usize, ch: char, forward: bool) -> Option<usize> {
    if forward {
        let start = next_boundary(doc, from)?;
        let found = find_char(doc, start, ch, true)?;
        previous_boundary(doc, found)
    } else {
        let start = previous_boundary(doc, from)?;
        let found = find_char(doc, start, ch, false)?;
        next_boundary(doc, found)
    }
}

/// `meow-find`: select from the point to just past the next `ch`.
///
/// `till` stops before it instead. Both make a `select` selection — they are
/// not expandable by repetition, only by a digit, which is what the stashed
/// navigation pair is for.
pub fn find(
    doc: &Document,
    state: &mut SelectionState,
    ch: char,
    forward: bool,
    till: bool,
) -> bool {
    let from = state.current().pos();
    let found = if till {
        till_char(doc, from, ch, forward)
    } else {
        find_char(doc, from, ch, forward)
    };
    let Some(pos) = found else {
        return false;
    };
    let kind = if till { Kind::TILL } else { Kind::FIND };
    state.select(Selection::typed(Axis::Select, kind, from, pos));
    if till {
        state.last_till = Some(ch);
        state.set_nav(Nav::Till);
    } else {
        state.last_find = Some(ch);
        state.set_nav(Nav::Find);
    }
    true
}

/// Every occurrence of `needle`, as byte offsets.
///
/// What [`search`] below finds one at a time, found all at once — the rows that
/// search what is already open want the list rather than the next one, and two
/// notions of "matches" in one crate is how they come to disagree about
/// overlaps or about the empty needle.
///
/// **Literal, like `search` itself**, which is `str::find`: the text is here in
/// memory and meowr links no regular-expression engine. The rows that search
/// *files* get one for nothing because `rg` is one; these do not, and taking a
/// crate for it is a decision with its own measurement rather than a detail of
/// this function.
///
/// Non-overlapping, and the earliest match wins each time: `aa` in `aaa` is one
/// occurrence at 0, not two. Overlapping matches would put two rows on one line
/// that a reader cannot tell apart.
pub fn occurrences(doc: &Document, needle: &str) -> Vec<usize> {
    if needle.is_empty() {
        return Vec::new();
    }
    let text = doc.text().to_string();
    text.match_indices(needle).map(|(at, _)| at).collect()
}

/// `meow-search`: select the next occurrence of `needle`, wrapping.
///
/// Wrapping rather than failing at the end is Meow's behaviour and the useful
/// one: a search that stops at the bottom of the file makes you press a
/// different key to carry on.
pub fn search(doc: &Document, state: &mut SelectionState, needle: &str, forward: bool) -> bool {
    if needle.is_empty() {
        return false;
    }
    let text = doc.text().to_string();
    let from = state.current().pos();
    let found = if forward {
        text.get(from..)
            .and_then(|rest| rest.find(needle).map(|at| from + at))
            .or_else(|| text.find(needle))
    } else {
        text.get(..from)
            .and_then(|head| head.rfind(needle))
            .or_else(|| text.rfind(needle))
    };
    let Some(start) = found else {
        return false;
    };
    let end = start + needle.len();
    // The point lands on the far end in the direction of travel, so the next
    // search carries on rather than finding the same match again.
    let (mark, pos) = if forward { (start, end) } else { (end, start) };
    state.select(Selection::typed(Axis::Select, Kind::VISIT, mark, pos));
    true
}

/// Meow's `meow-expand`: grow the selection by `n` of whatever made it.
///
/// Does nothing at all unless there is a selection *and* a stashed navigation
/// pair — `meow-expand`'s own `(and meow--expand-nav-function (region-active-p)
/// (meow--selection-type))`. That is why `i 2` and `m 2` are no-ops.
///
/// `n = 0` means ten. The kind is preserved and only the axis changes, to
/// `axis` — `meow-expand-selection-type`, which defaults to `select`, so
/// expanding normally produces a transient selection rather than an expanding
/// one.
pub fn expand(doc: &Document, state: &mut SelectionState, n: u32, axis: Axis) -> bool {
    let current = state.current();
    let (Some(nav), Some(kind)) = (state.nav(), current.kind()) else {
        return false;
    };
    let n = if n == 0 { 10 } else { n };
    let backward = current.is_backward();
    let mut pos = current.pos();
    for _ in 0..n {
        pos = match nav {
            Nav::Word => end_of_next_thing(doc, pos, is_word, !backward),
            Nav::Symbol => end_of_next_thing(doc, pos, is_symbol, !backward),
            Nav::Line => line_step(doc, pos, backward),
            Nav::Block => pos,
            // `meow--find-continue-forward` and its till counterpart: the
            // character is remembered, so a digit repeats the find rather
            // than doing nothing.
            Nav::Find => match state.last_find() {
                Some(ch) => find_char(doc, pos, ch, !backward).unwrap_or(pos),
                None => pos,
            },
            Nav::Till => match state.last_till() {
                Some(ch) => till_char(doc, pos, ch, !backward).unwrap_or(pos),
                None => pos,
            },
        };
    }
    state.select(Selection::typed(axis, kind, current.mark(), pos));
    true
}

/// The start of the line containing `pos`, and the offset of its newline —
/// the end of the line *without* it.
///
/// Deliberately goes to the rope rather than to [`Document::line_count`],
/// which stops before the empty line a trailing newline leaves behind. That is
/// the right answer for drawing rows and the wrong one here: Emacs counts that
/// line, and `l l` on the second-to-last line of a file ending in a newline
/// extends onto it.
/// The offset of the newline ending the line `pos` is on, or the end of the
/// buffer. What `kill-line` kills up to.
pub fn line_end_after(doc: &Document, pos: usize) -> usize {
    line_bounds(doc, pos).1
}

pub(crate) fn line_bounds_of(doc: &Document, pos: usize) -> (usize, usize) {
    line_bounds(doc, pos)
}

/// Emacs's `forward-line`: `n` lines from `pos`, clamped to the buffer.
///
/// Clamped rather than refusing, because `forward-line` stops at the edge and
/// reports how far it fell short; nothing here reads the shortfall, and
/// `meow-line` at the edge re-makes the selection unchanged.
fn forward_line(doc: &Document, pos: usize, n: i32) -> usize {
    let rope = doc.text();
    let line = rope.byte_to_line(pos.min(doc.byte_len()));
    let last = rope.len_lines().saturating_sub(1);
    let moved = (line as i64 + n as i64).clamp(0, last as i64) as usize;
    rope.line_to_byte(moved)
}

fn line_bounds(doc: &Document, pos: usize) -> (usize, usize) {
    let len = doc.byte_len();
    let rope = doc.text();
    let pos = pos.min(len);
    let line = rope.byte_to_line(pos);
    let start = rope.line_to_byte(line);
    let end = if line + 1 < rope.len_lines() {
        rope.line_to_byte(line + 1).saturating_sub(1)
    } else {
        len
    };
    (start, end)
}

/// Swap mark and pos. Not recorded: the selection has not changed, only which
/// end the point is at.
pub fn reverse(state: &mut SelectionState) {
    let reversed = state.current().reversed();
    state.select_without_history(reversed);
}

/// Drop the selection, leaving a bare point where it ended — and the mark
/// where [`SelectionState::cancel`] leaves it, which is the interesting half.
pub fn cancel(state: &mut SelectionState) {
    state.cancel();
}

/// The bracket pairs [`block`] and [`crate::thing::enclosing_pair`] understand.
pub(crate) const PAIRS: [(char, char); 3] = [('(', ')'), ('[', ']'), ('{', '}')];

/// Which delimiters a bracket scan should count.
///
/// A `)` inside a string or a comment is a character being shown, not a
/// delimiter that closes anything. Counted, it ends the scan early: on a line
/// like `let s = ")";` the block commands then select the wrong thing, or
/// nothing at all. Emacs asks `syntax-ppss` this question.
///
/// This crate parses nothing, so the question is a port rather than an
/// answer. The session answers it from the syntax tree it already keeps in
/// order to colour the buffer, so nothing is parsed twice to make the block
/// commands right. Where the buffer has no grammar there is nothing better to
/// ask and the caller passes `None`, which counts every bracket — exactly
/// what these scans did before the question could be asked at all.
pub trait Delimiters {
    /// Whether the character at `byte` is a delimiter, rather than a bracket
    /// that happens to be sitting inside a string or a comment.
    fn counts(&self, byte: usize) -> bool;
}

/// Meow's `meow-block`: select the block, or expand to the parent block.
///
/// `meow-block` does not look for a pair around the point, which is what this
/// did for as long as every case it was tried on had one. It scans for the
/// **next delimiter that returns to the depth the point began at** — forward
/// for a close, backward for an open — and then jumps to that delimiter's
/// partner. The two readings agree whenever the point is already inside a
/// pair, and part company everywhere the point sits before a block rather
/// than within it: `o` on `foo bar (baz)` selects `(baz)` in Emacs and did
/// nothing here.
///
/// With a region already active the depth test tightens from "at or below" to
/// "strictly below". That one character is the whole of why repeating widens
/// outward rather than finding the same pair again — and the reason the two
/// directions cannot simply be swapped.
///
/// `back` is `(xor (meow--direction-backward-p) (< arg 0))`: a backward
/// selection or a negative argument each reverse the scan, and both together
/// cancel out.
pub fn block(
    doc: &Document,
    state: &mut SelectionState,
    back: bool,
    delimiters: Option<&dyn Delimiters>,
) {
    state.forget_goal_column();
    let current = state.current();
    let active = current.selection_type().is_some();
    let Some(p) = returning_delimiter(doc, current.pos(), back, active, delimiters) else {
        return;
    };
    // `forward-list`, wrapped in `ignore-errors`: an unbalanced delimiter has
    // no partner to select back to, and Meow then makes no selection at all
    // rather than selecting to the edge of the buffer.
    let Some(m) = partner_of(doc, p, back, delimiters) else {
        return;
    };
    state.select(Selection::typed(Axis::Expand, Kind::BLOCK, m, p));
    state.set_nav(Nav::Block);
}

/// The first delimiter whose depth returns to the one `from` began at, as the
/// offset Emacs's point lands on: just past a closing delimiter going forward,
/// on the opening one going back.
///
/// Emacs asks `syntax-ppss` for an absolute depth and compares it against the
/// depth at the point. The comparison is all that is ever used, so this keeps
/// the difference instead and never needs the absolute number — which is also
/// why a bracket the caller says does not count can simply be stepped over,
/// exactly as `parse-partial-sexp` steps over one inside a string.
///
/// `active` is `region-active-p`. It tightens the accepting test by one, and
/// that is what makes a second `o` widen: the delimiter that satisfied the
/// first is no longer deep enough to satisfy the second.
fn returning_delimiter(
    doc: &Document,
    from: usize,
    back: bool,
    active: bool,
    delimiters: Option<&dyn Delimiters>,
) -> Option<usize> {
    let mut depth = 0i32;
    for (looking_at, landing) in steps(doc, from, Toward::forward_if(!back)) {
        let Some(kind) = delimiter_at(doc, looking_at, back, delimiters) else {
            continue;
        };
        match kind {
            Delimiter::Widening => depth += 1,
            Delimiter::Narrowing => {
                depth -= 1;
                if if active { depth < 0 } else { depth <= 0 } {
                    return Some(landing);
                }
            }
        }
    }
    None
}

/// Emacs's `forward-list` over one balanced list, from the delimiter `p`
/// names: back over the list going forward, forward over it going back.
///
/// `None` where the list does not balance, which is `ignore-errors` returning
/// nil.
fn partner_of(
    doc: &Document,
    p: usize,
    back: bool,
    delimiters: Option<&dyn Delimiters>,
) -> Option<usize> {
    let mut depth = 0i32;
    // The mirror of the scan that found `p`: it ran forward over a close, so
    // this runs back over the same close and out to its open.
    for (looking_at, landing) in steps(doc, p, Toward::forward_if(back)) {
        let Some(kind) = delimiter_at(doc, looking_at, !back, delimiters) else {
            continue;
        };
        match kind {
            Delimiter::Widening => depth += 1,
            Delimiter::Narrowing => {
                depth -= 1;
                if depth == 0 {
                    return Some(landing);
                }
            }
        }
    }
    None
}

/// Which way the delimiter at `byte` moves a depth counter walking `back`,
/// or `None` when it is not one that counts.
///
/// Going forward an opening bracket deepens; going back a closing one does.
/// Everything else — a bracket inside a string or a comment, or a character
/// that is not a bracket at all — is not a delimiter here. See [`Delimiters`].
fn delimiter_at(
    doc: &Document,
    byte: usize,
    back: bool,
    delimiters: Option<&dyn Delimiters>,
) -> Option<Delimiter> {
    let c = char_at(doc, byte)?;
    let opening = PAIRS.iter().any(|(open, _)| *open == c);
    if !opening && !PAIRS.iter().any(|(_, close)| *close == c) {
        return None;
    }
    // The port is asked only about brackets, and only here. The scans walk
    // every character between the point and the answer, and the answer is a
    // tree descent rather than a lookup — a question worth asking about `)` is
    // not worth asking about `x`.
    if !delimiters.is_none_or(|d| d.counts(byte)) {
        return None;
    }
    Some(if opening ^ back {
        Delimiter::Widening
    } else {
        Delimiter::Narrowing
    })
}

/// What a delimiter does to the depth of the scan that meets it.
///
/// Named for the effect rather than for the character, because which of `(`
/// and `)` widens depends on which way the scan is running.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Delimiter {
    /// Deeper: away from the depth the scan is trying to return to.
    Widening,
    /// Shallower: the kind that can end the scan.
    Narrowing,
}

/// Meow's `meow-goto-line`: select the whole of line `n`, counting from one.
///
/// "This command will expand line selection" — so with a line selection
/// already made it takes the union rather than replacing it, which is what
/// makes `L` twice cover everything between the two lines.
pub fn goto_line(doc: &Document, state: &mut SelectionState, n: usize) {
    state.forget_goal_column();
    let last = doc.line_count().saturating_sub(1);
    let line = n.saturating_sub(1).min(last);
    let Some(at) = doc.line_to_byte(line) else {
        return;
    };
    let (mut start, mut end) = line_bounds(doc, at);
    // Emacs tests the *raw* line beginning, before the union below widens it.
    let backward = state.current().pos() > start;
    let current = state.current();
    if current.selection_type() == Some((Axis::Expand, Kind::LINE)) {
        let range = current.range();
        start = start.min(range.start);
        end = end.max(range.end);
    }
    // `meow--select` sends the point to `beg` and the mark to `end` when it is
    // told the selection runs backward, so jumping up puts the point at the
    // start of the line it landed on.
    let (mark, pos) = if backward { (end, start) } else { (start, end) };
    state.select(Selection::typed(Axis::Expand, Kind::LINE, mark, pos));
}

/// Meow's `meow-to-block`: expand from the point to the end of the next block.
///
/// Distinct from `block`, which marks a whole bracket pair. This one keeps the
/// point where it is and reaches *to* a delimiter, so the selection starts
/// wherever you were rather than at an opening bracket.
///
/// Emacs does this with `syntax-ppss`, scanning for a close delimiter that
/// leaves it at the depth it began at. All three bracket kinds share one depth
/// there, so they share one here: `(]` is unbalanced to Emacs too.
pub fn to_block(
    doc: &Document,
    state: &mut SelectionState,
    back: bool,
    delimiters: Option<&dyn Delimiters>,
) {
    state.forget_goal_column();
    let current = state.current();
    let point = current.pos();
    let Some(target) = block_edge_at_starting_depth(doc, point, back, delimiters) else {
        return;
    };
    // `meow-to-block` passes EXPAND to `meow--make-selection`, and that is not
    // decoration: with a region already active the new mark becomes the far
    // end of it rather than the point. Which end depends on the direction —
    // `(if (< mark pos) (min orig-mark orig-pos) (max orig-mark orig-pos))` —
    // so reaching backward keeps the *end* and reaching forward the start.
    let mark = match (current.selection_type(), back) {
        (Some(_), false) => current.range().start.min(point),
        (Some(_), true) => current.range().end.max(point),
        (None, _) => point,
    };
    state.select(Selection::typed(Axis::Expand, Kind::BLOCK, mark, target));
    state.set_nav(Nav::Block);
}

/// Scan for the delimiter that returns to the depth `from` began at, and
/// answer the offset Meow's point lands on: just past a closing delimiter
/// going forward, on the opening one going back.
///
/// `None` when the opposite delimiter is met while already at the starting
/// depth: that one belongs to an enclosing block, so the scan has left the
/// depth it was called at and Meow does nothing rather than reaching across
/// the boundary.
///
/// Brackets inside strings and comments are not delimiters, and `delimiters`
/// is where that is decided — see [`Delimiters`] for why the question is asked
/// rather than answered here.
fn block_edge_at_starting_depth(
    doc: &Document,
    from: usize,
    back: bool,
    delimiters: Option<&dyn Delimiters>,
) -> Option<usize> {
    // A plain counter over `steps` rather than `Iterator::scan`, which does
    // express the stateful fold: its closure would return `Option<Option<_>>`
    // here, the outer meaning "keep going" and the inner "this is the answer".
    // Two levels of `Option` meaning two unrelated things is worse to read.
    let mut depth = 0u32;
    for (looking_at, landing) in steps(doc, from, Toward::forward_if(!back)) {
        match delimiter_at(doc, looking_at, back, delimiters) {
            Some(Delimiter::Widening) => depth += 1,
            Some(Delimiter::Narrowing) => {
                // Below the depth this began at: the delimiter belongs to an
                // enclosing block, so `meow-to-block` refuses rather than
                // reaching across the boundary. `meow-block` is the one that
                // may cross it, which is why the two do not share this.
                if depth == 0 {
                    return None;
                }
                depth -= 1;
                if depth == 0 {
                    return Some(landing);
                }
            }
            None => {}
        }
    }
    None
}

/// Which side of the point `meow-join` reaches over.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JoinSide {
    /// `meow--join-backward`, the no-argument default.
    Backward,
    /// `meow--join-forward`, reached with a negative argument.
    Forward,
    /// `meow--join-both`, which `(or (equal '(expand . join) …) (universal-p
    /// arg))` reaches: a universal argument, or repeating `j` on a join
    /// selection.
    Both,
}

/// Meow's `meow-join`: select the whitespace joining this line to its
/// neighbour.
///
/// The name is misleading and the docstring is the thing to trust — it
/// *selects* the indentation rather than joining anything. Deleting what it
/// selects is what performs the join, which is why this lives with the
/// selection calculus and not with the editing commands.
pub fn join(doc: &Document, state: &mut SelectionState, side: JoinSide) {
    state.forget_goal_column();
    let point = state.current().pos();
    let (from, to) = match side {
        JoinSide::Backward => {
            let end = back_to_indentation(doc, point);
            (blanks_before(doc, line_bounds(doc, point).0), end)
        }
        JoinSide::Forward => {
            let start = line_bounds(doc, point).1;
            (start, blanks_after(doc, start))
        }
        // Both reaches from the point itself rather than from the line
        // boundaries, which is why it can land inside a run of spaces.
        JoinSide::Both => (blanks_before(doc, point), blanks_after(doc, point)),
    };
    state.select(Selection::typed(Axis::Expand, Kind::JOIN, from, to));
}

/// Emacs's `back-to-indentation`: the first non-blank on the line, or the line
/// end when the whole line is blank.
fn back_to_indentation(doc: &Document, pos: usize) -> usize {
    // Indentation only, so the newline stops it: on a wholly blank line Emacs
    // lands at the line end rather than running on into the next line.
    scan(doc, line_bounds(doc, pos).0, Toward::Forward, |c| {
        c.is_whitespace() && c != '\n'
    })
}

/// Walk forward from `pos` over every blank, the way `looking-at` does.
fn blanks_after(doc: &Document, pos: usize) -> usize {
    scan(doc, pos, Toward::Forward, char::is_whitespace)
}

/// Walk back from `pos` over every blank, the way `looking-back` does.
fn blanks_before(doc: &Document, pos: usize) -> usize {
    scan(doc, pos, Toward::Backward, char::is_whitespace)
}

/// Which way a scan runs.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Toward {
    Forward,
    Backward,
}

impl Toward {
    /// For the callers that carry a direction as the `bool` their Meow command
    /// was given. Named for what `true` means, because two of the three
    /// callers hold that bool the other way up.
    fn forward_if(forward: bool) -> Self {
        if forward {
            Toward::Forward
        } else {
            Toward::Backward
        }
    }
}

/// The character steps from `pos`, in the direction given.
///
/// Each item is the pair a walk needs: where the character *is*, and where a
/// step onto it lands. Going forward those differ — `looking-at` asks about
/// the character starting at the point, so the character is at `at` and the
/// landing is the next boundary. Going back they are the same offset, because
/// `looking-back` asks about the character *ending* at the point and stepping
/// onto it puts you there.
///
/// That asymmetry is Emacs's, and getting it wrong has a specific symptom
/// worth naming: a backward walk that samples the character at `at` reads the
/// one it is about to leave, and stops one short.
///
/// Having it here means the walks over it — [`walk`], [`scan`], `find_char`,
/// and the two depth counters — do not each carry their own copy of it.
fn steps(doc: &Document, pos: usize, toward: Toward) -> impl Iterator<Item = (usize, usize)> {
    let mut at = pos;
    std::iter::from_fn(move || {
        let landing = match toward {
            Toward::Forward => next_boundary(doc, at)?,
            Toward::Backward => previous_boundary(doc, at)?,
        };
        let looking_at = match toward {
            Toward::Forward => at,
            Toward::Backward => landing,
        };
        at = landing;
        Some((looking_at, landing))
    })
}

/// Walk from `pos` for as long as `keep` accepts the step, and answer where it
/// stopped. `pos` itself when nothing was accepted: no step, no movement.
fn walk(doc: &Document, pos: usize, toward: Toward, keep: impl Fn(usize, usize) -> bool) -> usize {
    steps(doc, pos, toward)
        .take_while(|&(looking_at, landing)| keep(looking_at, landing))
        .map(|(_, landing)| landing)
        .last()
        .unwrap_or(pos)
}

/// [`walk`] over the characters `member` accepts.
fn scan(doc: &Document, pos: usize, toward: Toward, member: impl Fn(char) -> bool) -> usize {
    walk(doc, pos, toward, |looking_at, _| {
        char_at(doc, looking_at).is_some_and(&member)
    })
}
