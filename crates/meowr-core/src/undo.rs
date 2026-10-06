//! Undo, as a log of transactions rather than a stack of buffer snapshots.
//!
//! Meow has nothing to be faithful to here: `meow-undo` cancels the selection
//! and presses `C-/`, and `meow-undo-in-selection` presses the same key with
//! the region live and lets Emacs's `undo-in-region` do the work. So the model
//! is meowr's, and two choices in it are deliberate departures from Emacs.
//!
//! **Redo is an explicit stack**, not "undo the undo". Emacs makes redo
//! reachable by undoing an undo, which is famously hard to reason about — the
//! only way to know what `C-/` will do next is to remember what you have
//! already pressed. Here, undone transactions go on a second stack, and any
//! fresh edit discards them.
//!
//! **A transaction is a command**, not a keystroke. A burst of typing
//! amalgamates into one transaction so that undo does not walk back one
//! character at a time, and any other command closes it — which is exactly
//! what `undo-boundary` does in Emacs, arrived at from the other direction.

use crate::{Selection, SelectionState};
use meowr_domain::{Document, EditError};
use std::ops::Range;

/// One replacement: what was at `at`, and what is there now.
///
/// Storing both halves is what makes this a transaction log rather than a
/// snapshot: reverting is deleting `inserted` and putting `removed` back, and
/// the same record replays forward for redo.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Change {
    pub at: usize,
    pub removed: String,
    pub inserted: String,
}

impl Change {
    fn span(&self) -> Range<usize> {
        self.at..self.at + self.inserted.len()
    }
}

/// The edits of one command, with the selection either side of it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Transaction {
    /// Which committed transaction this is, so a save can name a place in the
    /// timeline rather than a depth in it.
    ///
    /// Carried on the transaction and not in a stack beside `done`, because
    /// every push and pop then moves it without anybody remembering to — and
    /// there are five such sites, counting `undo_in`'s partial revert.
    ///
    /// Private, and the only field that is: nothing outside this module has
    /// any use for it, and a serial that a caller could set is one this
    /// module's invariant no longer holds over. An open transaction's value is
    /// meaningless rather than reserved — `boundary` overwrites it, and the
    /// first committed transaction takes 0 like any other.
    stamp: u64,
    pub changes: Vec<Change>,
    /// The selection before the command ran. Undo restores it, because putting
    /// the text back without putting the point back leaves you somewhere you
    /// never were.
    pub before: Selection,
    pub after: Selection,
}

/// How many characters of typing amalgamate into one transaction.
///
/// Emacs's `amalgamating-undo-limit`, and the same value. Long enough that a
/// word is one undo, short enough that a paragraph is not.
const AMALGAMATE_LIMIT: usize = 20;

#[derive(Debug, Clone, Default)]
pub struct History {
    done: Vec<Transaction>,
    undone: Vec<Transaction>,
    /// The transaction being built, if a command is still adding to it.
    open: Option<Transaction>,
    /// Every change since a consumer last drained them.
    ///
    /// The undo stacks are the wrong shape for this: they are grouped into
    /// transactions and walked backwards. Anything that needs to know *what
    /// changed* — the syntax tree now, an LSP client later — needs the changes
    /// in order and exactly once, which is what this is for.
    unread: Vec<Change>,
    /// The serial the next committed transaction gets. Only ever increases.
    next_stamp: u64,
    /// Where `done` stood when the buffer was last written, if it ever was.
    ///
    /// `None` means never saved, which is not the same as "at the save point"
    /// — an empty history that was never written is a `*scratch*` with typing
    /// in it.
    saved: Option<Option<u64>>,
}

impl History {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn can_undo(&self) -> bool {
        self.open.is_some() || !self.done.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.undone.is_empty()
    }

    /// Close the open transaction — Emacs's `undo-boundary`.
    ///
    /// Called between commands, so that "undo" means "the last thing I did"
    /// rather than "the last byte that changed".
    pub fn boundary(&mut self) {
        if let Some(mut open) = self.open.take() {
            open.stamp = self.next_stamp;
            self.next_stamp += 1;
            self.done.push(open);
        }
    }

    /// Say the buffer on disk now says what this history has arrived at.
    ///
    /// Emacs records the undo entry current at save time and clears the
    /// modified flag when undo returns to it; this records the same place.
    pub fn mark_saved(&mut self) {
        // *Not* `boundary()` first. Committing an in-flight edit here and then
        // naming it the save point would call text the host never wrote
        // unmodified — and both this and `Session::mark_saved` are public, so
        // the ordering cannot be argued from the one call site that exists.
        // An open transaction is unsaved work by definition, and `is_modified`
        // says so.
        self.saved = Some(self.done.last().map(|t| t.stamp));
    }

    /// Whether the buffer differs from what was last written.
    ///
    /// A question about *where in the timeline* rather than a flag that an
    /// edit happened, which is the difference that makes undoing back to the
    /// saved state clear it — and makes redoing past it set it again, which a
    /// flag cannot do at all.
    pub fn is_modified(&self) -> bool {
        match self.saved {
            // Written, so it is modified exactly when the timeline has moved.
            // An open transaction counts: the edit is real even before the
            // boundary that will close it.
            Some(saved) => self.open.is_some() || self.done.last().map(|t| t.stamp) != saved,
            // Never written. Anything at all in the history is a modification,
            // and the `*scratch*` you have typed into is the case this serves.
            None => self.open.is_some() || !self.done.is_empty(),
        }
    }

    /// Take the changes nothing has consumed yet.
    pub fn take_changes(&mut self) -> Vec<Change> {
        std::mem::take(&mut self.unread)
    }

    fn record(&mut self, change: Change, before: Selection, after: Selection) {
        self.unread.push(change.clone());
        // Any new edit makes the redo stack unreachable: the future it
        // described no longer follows from the present.
        self.undone.clear();
        match &mut self.open {
            Some(open) => {
                open.changes.push(change);
                open.after = after;
            }
            None => {
                self.open = Some(Transaction {
                    // Placed by `boundary`, which is what committing means.
                    stamp: 0,
                    changes: vec![change],
                    before,
                    after,
                });
            }
        }
    }

    /// Apply a **Foreign edit**: replacements that each address the text as it
    /// stood before any of them, as one undo step (#161, ADR-0013).
    ///
    /// Highest position first, so no replacement moves a range still to come --
    /// lsp-mode's order, stable among replacements at one position so the order
    /// given still decides between two inserts there, which is what the protocol
    /// says it should. Checked whole before anything changes: an overlap or a bad
    /// range applies nothing, because half a rename is worse than none. Closed
    /// off from typing on both sides, so undoing a format mid-word does not take
    /// the word.
    pub fn replace_all(
        &mut self,
        doc: &mut Document,
        state: &mut SelectionState,
        replacements: &[meowr_domain::Replacement],
    ) -> Result<(), ReplaceError> {
        let ordered = application_order(replacements)?;
        // Applied to a copy first, so a range that turns out to be bad partway
        // through leaves the document as it was. A rope clone shares its nodes.
        let mut edited = doc.clone();
        let mut changes = Vec::with_capacity(ordered.len());
        for replacement in &ordered {
            let range = replacement.range.clone();
            // Deleted first, because `delete` is what says *why* a range is bad;
            // what it removed is read from the original, which holds the same
            // text there -- nothing above this position has changed it.
            edited.delete(range.clone()).map_err(ReplaceError::Edit)?;
            let removed = slice(doc, range.clone());
            edited
                .insert(range.start, &replacement.text)
                .map_err(ReplaceError::Edit)?;
            changes.push(Change {
                at: range.start,
                removed,
                inserted: replacement.text.clone(),
            });
        }
        *doc = edited;
        // One transaction: the first `record` opens it with the selection from
        // before the whole edit, and the rest join it.
        self.boundary();
        for change in changes {
            let before = state.current();
            let (at, removed, inserted) = (change.at, change.removed.len(), change.inserted.len());
            state.map_each(|selection| selection.after_replacement(at, removed, inserted));
            self.record(change, before, state.current());
        }
        // A distance kept inside a replaced span is counted in bytes, and a
        // replacement of wider characters would leave it inside one.
        state.map_each(|selection| selection.on_char_boundaries(doc));
        if let Some(open) = self.open.as_mut() {
            open.after = state.current();
        }
        self.boundary();
        Ok(())
    }

    /// Insert, recording it. Starts a fresh transaction.
    pub fn insert(
        &mut self,
        doc: &mut Document,
        state: &mut SelectionState,
        at: usize,
        text: &str,
    ) -> Result<usize, EditError> {
        let before = state.current();
        let added = doc.insert(at, text)?;
        state.after_edit(at, 0, added);
        state.select_without_history(Selection::at(at + added));
        self.record(
            Change {
                at,
                removed: String::new(),
                inserted: text.to_string(),
            },
            before,
            state.current(),
        );
        Ok(added)
    }

    /// Insert one typed character, amalgamating with the run before it.
    ///
    /// Only when it continues that run exactly: same transaction, immediately
    /// after the last insertion, and not yet at the limit. Typing, moving, and
    /// typing again is two undos, because the move closed the transaction.
    pub fn type_text(
        &mut self,
        doc: &mut Document,
        state: &mut SelectionState,
        at: usize,
        text: &str,
    ) -> Result<usize, EditError> {
        let continues = self.open.as_ref().is_some_and(|open| {
            let typed: usize = open.changes.iter().map(|c| c.inserted.len()).sum();
            open.changes.iter().all(|c| c.removed.is_empty())
                && typed < AMALGAMATE_LIMIT
                && open.changes.last().is_some_and(|c| c.span().end == at)
        });
        if !continues {
            self.boundary();
        }
        self.insert(doc, state, at, text)
    }

    /// Delete, recording what was there.
    pub fn delete(
        &mut self,
        doc: &mut Document,
        state: &mut SelectionState,
        range: Range<usize>,
    ) -> Result<(), EditError> {
        let before = state.current();
        let removed = slice(doc, range.clone());
        let (at, len) = (range.start, range.end - range.start);
        doc.delete(range)?;
        state.after_edit(at, len, 0);
        state.select_without_history(Selection::at(at));
        self.record(
            Change {
                at,
                removed,
                inserted: String::new(),
            },
            before,
            state.current(),
        );
        Ok(())
    }

    /// Undo the most recent transaction. `false` when there is nothing to undo.
    pub fn undo(&mut self, doc: &mut Document, state: &mut SelectionState) -> bool {
        self.boundary();
        let Some(transaction) = self.done.pop() else {
            return false;
        };
        // Told as the changes they are, so an undo is what any edit is to the
        // revision, tree-sitter, the Grab and a language server (#171).
        let made = revert(doc, &transaction.changes);
        self.unread.extend(made);
        state.select_without_history(transaction.before);
        self.undone.push(transaction);
        true
    }

    /// Redo the most recently undone transaction.
    pub fn redo(&mut self, doc: &mut Document, state: &mut SelectionState) -> bool {
        let Some(transaction) = self.undone.pop() else {
            return false;
        };
        let made = apply(doc, &transaction.changes);
        self.unread.extend(made);
        state.select_without_history(transaction.after);
        self.done.push(transaction);
        true
    }

    /// Undo only what happened inside `range` — `meow-undo-in-selection`.
    ///
    /// Reverts the changes of the **most recent** transaction that fall inside
    /// the span, and leaves the rest of it on the stack for a later undo.
    ///
    /// Only the most recent, and that is a real limit rather than laziness.
    /// A change records the offset it happened at, in the coordinates of the
    /// buffer at that moment — so once a later edit has shifted the text, an
    /// older change's offset no longer means what it says, and comparing it
    /// against a live selection would revert the wrong span. Emacs pays for
    /// the general case by rewriting its whole undo list on every
    /// `undo-in-region`; meowr does not, and says so here instead of
    /// pretending the deeper history is reachable.
    pub fn undo_in(
        &mut self,
        doc: &mut Document,
        state: &mut SelectionState,
        range: Range<usize>,
    ) -> bool {
        self.boundary();
        // Taken before the borrow below, for the partial-revert branch: that
        // one leaves the transaction on `done` with its text changed, so it
        // has to become a *different* place in the timeline or a save made at
        // it would still look current.
        let next = self.next_stamp;
        let Some(transaction) = self.done.last_mut() else {
            return false;
        };
        let (inside, outside): (Vec<Change>, Vec<Change>) = transaction
            .changes
            .iter()
            .cloned()
            .partition(|c| touches(c, &range));
        if inside.is_empty() {
            return false;
        }
        let made = revert(doc, &inside);
        self.unread.extend(made);
        let before = transaction.before;
        if outside.is_empty() {
            let mut done = self.done.pop().expect("just borrowed it");
            done.changes = inside;
            self.undone.push(done);
            state.select_without_history(before);
        } else {
            transaction.changes = outside;
            transaction.stamp = next;
            self.next_stamp += 1;
        }
        true
    }
}

/// Whether a change overlaps `range`.
///
/// A pure deletion has an empty span, so it would never overlap anything under
/// a strict test; it counts when the range covers the point it happened at,
/// which is where the deleted text would come back.
fn touches(change: &Change, range: &Range<usize>) -> bool {
    let span = change.span();
    if span.is_empty() {
        range.start <= span.start && span.start <= range.end
    } else {
        span.start < range.end && range.start < span.end
    }
}

/// `replacements` in the order [`History::replace_all`] applies them, or why
/// no order applies them all.
///
/// Its own function because a **workspace edit** checks every file before it
/// applies anything (#167, ADR-0013 amended), and a check that was a second
/// copy of this one could pass an edit this then refuses halfway through.
///
/// Reversed before a stable sort, so that among replacements the sort cannot
/// tell apart -- two inserts at one position -- the one given first is applied
/// last and so ends up first in the text, which is the order the protocol says
/// the array defines. Ties on the start are broken by the end, so a
/// replacement at `p..q` goes before an insert at `p` and the insert lands in
/// front of it, in whichever order the two were given.
pub fn application_order(
    replacements: &[meowr_domain::Replacement],
) -> Result<Vec<&meowr_domain::Replacement>, ReplaceError> {
    let mut ordered: Vec<&meowr_domain::Replacement> = replacements.iter().rev().collect();
    ordered.sort_by_key(|r| (std::cmp::Reverse(r.range.start), std::cmp::Reverse(r.range.end)));
    if ordered.iter().any(|r| r.range.start > r.range.end) {
        return Err(ReplaceError::Edit(EditError::OutOfBounds));
    }
    // Highest first, so each next one must end at or before where the last
    // began. Two inserts at one position satisfy that, as they should.
    if ordered
        .windows(2)
        .any(|pair| pair[1].range.end > pair[0].range.start)
    {
        return Err(ReplaceError::Overlapping);
    }
    Ok(ordered)
}

/// Why a foreign edit's replacements were not applied. Nothing was, either way.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReplaceError {
    /// Two replacements claim some of the same bytes, so no order applies both.
    Overlapping,
    /// A range falls outside the text or inside a character.
    Edit(EditError),
}

impl core::fmt::Display for ReplaceError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Overlapping => write!(f, "two of its replacements overlap"),
            Self::Edit(error) => write!(f, "{error}"),
        }
    }
}

fn slice(doc: &Document, range: Range<usize>) -> String {
    let text = doc.text();
    text.slice(text.byte_to_char(range.start)..text.byte_to_char(range.end))
        .to_string()
}

/// Undo `changes`, newest first — later changes are described in terms of the
/// buffer the earlier ones produced, so reverting in order would misplace them.
///
/// Returns the changes it made, as they happened: see [`made`].
fn revert(doc: &mut Document, changes: &[Change]) -> Vec<Change> {
    changes
        .iter()
        .rev()
        .filter_map(|change| made(doc, change.at, change.inserted.len(), &change.removed))
        .collect()
}

/// Redo `changes`, oldest first. Returns the changes it made, as [`revert`]
/// does.
fn apply(doc: &mut Document, changes: &[Change]) -> Vec<Change> {
    changes
        .iter()
        .filter_map(|change| made(doc, change.at, change.removed.len(), &change.inserted))
        .collect()
}

/// Replace `len` bytes at `at` with `text`, and say what that did.
///
/// What it did rather than what was asked: an undo's changes are recorded for
/// the revision, tree-sitter, the Grab and a language server (#171), and a
/// change that does not fit the text any more -- `undo_in`'s partial revert
/// leaves the rest of its transaction at offsets that have moved, its
/// documented limit -- must not be recorded as though it did. tree-sitter's
/// arithmetic panics on a change past the end, and the rest would believe in
/// text that is not there. A deletion that fails is skipped whole, with no
/// insertion at a place it no longer describes.
fn made(doc: &mut Document, at: usize, len: usize, text: &str) -> Option<Change> {
    let range = at..at + len;
    let removed = if len == 0 {
        String::new()
    } else {
        let removed = (range.end <= doc.byte_len()).then(|| slice(doc, range.clone()))?;
        doc.delete(range).ok()?;
        removed
    };
    let inserted = if text.is_empty() || doc.insert(at, text).is_ok() {
        text.to_string()
    } else {
        String::new()
    };
    (!removed.is_empty() || !inserted.is_empty()).then_some(Change {
        at,
        removed,
        inserted,
    })
}

#[cfg(test)]
mod tests;
