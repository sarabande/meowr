//! The editing operations, over a [`Document`] and a [`SelectionState`].
//!
//! Meow implements none of these itself: it computes a selection and presses
//! an Emacs key, through 44 `meow--kbd-*` delegations. Fidelity therefore binds
//! the observable result and not the implementation (ADR-0006), so these are
//! written directly rather than by replaying keystrokes — which is both
//! simpler and the only option without an Emacs underneath.

use crate::undo::History;
use crate::{Axis, Kind, Selection, SelectionState};
use meowr_domain::Document;
use meowr_domain::Place;
use std::collections::VecDeque;

/// Emacs's kill ring, kept **separate from the system clipboard**.
///
/// The split is Meow's, not an oversight: `meow-use-clipboard` defaults to nil
/// and there are distinct `meow-clipboard-*` commands, so a kill and a copy to
/// the desktop are deliberately different acts. Copying that is cheap;
/// reversing it later is not.
#[derive(Debug, Clone)]
pub struct KillRing {
    entries: VecDeque<String>,
    /// Emacs's `kill-ring-yank-pointer`: where `yank-pop` has walked to.
    index: usize,
}

impl KillRing {
    /// `kill-ring-max`.
    pub const MAX: usize = 60;

    pub fn new() -> Self {
        Self {
            entries: VecDeque::new(),
            index: 0,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// `kill-new`: a fresh entry at the front, and the yank pointer rewound.
    pub fn push(&mut self, text: String) {
        self.entries.push_front(text);
        self.entries.truncate(Self::MAX);
        self.index = 0;
    }

    /// `current-kill 0`: what a yank would insert.
    pub fn head(&self) -> Option<&str> {
        self.entries.get(self.index).map(String::as_str)
    }

    /// `yank-pop`: step the pointer on, wrapping, and return the new head.
    pub fn rotate(&mut self) -> Option<&str> {
        if self.entries.is_empty() {
            return None;
        }
        self.index = (self.index + 1) % self.entries.len();
        self.head()
    }
}

impl Default for KillRing {
    fn default() -> Self {
        Self::new()
    }
}

/// Meow's `meow--prepare-region-for-kill`.
///
/// A forward *line* selection stops before the newline, because `meow-line`
/// excludes it — but killing a line is supposed to take the line away, not
/// leave a blank one. So the region grows by one character first. This mutates
/// the selection, exactly as the elisp moves point before killing.
fn prepare_region_for_kill(doc: &Document, state: &mut SelectionState) {
    let current = state.current();
    if current.kind() != Some(Kind::LINE) || current.is_backward() {
        return;
    }
    if let Some(next) = crate::next_char_boundary(doc, current.pos()) {
        state.select_without_history(Selection::typed(
            current.axis().unwrap_or(Axis::Expand),
            Kind::LINE,
            current.mark(),
            next,
        ));
    }
}

fn region_text(doc: &Document, range: std::ops::Range<usize>) -> String {
    let text = doc.text();
    text.slice(text.byte_to_char(range.start)..text.byte_to_char(range.end))
        .to_string()
}

/// Remove `range` and leave the point where the text used to start.
///
/// Goes through [`History`] rather than touching the document, so that every
/// edit is undoable by construction: there is no second way to change the
/// buffer that could forget to record itself.
///
/// The selection **history is kept**, shifted to account for the edit.
/// `meow--delete-region` is a bare `delete-region`: it deactivates the mark,
/// but nothing in it touches `meow--selection-history`, so `z` after a kill
/// still walks back through what was selected. An earlier version cleared it
/// and the harness caught every kill case at once.
fn delete_region(
    doc: &mut Document,
    state: &mut SelectionState,
    history: &mut History,
    range: std::ops::Range<usize>,
) {
    let _ = history.delete(doc, state, range);
}

/// `meow-change`'s delete: the raw region, **not** killed and **not** prepared.
///
/// `meow-change` calls `meow--delete-region` directly, where `meow-kill` calls
/// `meow--prepare-region-for-kill` first and then `kill-region`. So `c` on a
/// line leaves the newline behind and puts nothing on the ring, where `k`
/// takes the newline and does. Two lines apart in the elisp, and entirely
/// different behaviour.
pub fn change(doc: &mut Document, state: &mut SelectionState, history: &mut History) -> bool {
    if state.current().selection_type().is_none() {
        return false;
    }
    delete_region(doc, state, history, state.current().range());
    true
}

/// Insert `text` at the point and leave the point after it.
fn insert_at_point(
    doc: &mut Document,
    state: &mut SelectionState,
    history: &mut History,
    text: &str,
) -> bool {
    let at = state.current().pos();
    history.insert(doc, state, at, text).is_ok()
}

/// `meow-kill`: the selection goes to the ring and out of the buffer.
pub fn kill(
    doc: &mut Document,
    state: &mut SelectionState,
    ring: &mut KillRing,
    history: &mut History,
) -> bool {
    if state.current().selection_type().is_none() {
        return false;
    }
    prepare_region_for_kill(doc, state);
    let range = state.current().range();
    ring.push(region_text(doc, range.clone()));
    delete_region(doc, state, history, range);
    true
}

/// Kill an explicit range, for the commands that compute their own rather
/// than taking the selection — `kill-line` is the one that matters.
pub fn kill_range(
    doc: &mut Document,
    state: &mut SelectionState,
    ring: &mut KillRing,
    history: &mut History,
    range: std::ops::Range<usize>,
) -> bool {
    if range.is_empty() {
        return false;
    }
    ring.push(region_text(doc, range.clone()));
    delete_region(doc, state, history, range);
    true
}

/// `meow-save`: copy, and deactivate the selection as `kill-ring-save` does.
pub fn save(doc: &Document, state: &mut SelectionState, ring: &mut KillRing) -> bool {
    if state.current().selection_type().is_none() {
        return false;
    }
    prepare_region_for_kill(doc, state);
    let current = state.current();
    ring.push(region_text(doc, current.range()));
    state.select_without_history(Selection::at(current.pos()));
    true
}

/// Emacs's `kill-ring-save`, which `meow-selection-command-fallback` hands
/// `meow-save` when there is no selection to save.
///
/// It needs a **mark**, not a region. `meow-grab` deactivates the selection
/// and leaves the mark where it was, so `y` after `G` copies from there to the
/// point; before the mark outlived the selection this fallback was wired and
/// still found nothing to copy.
///
/// `false` where there is no mark at all, which is `kill-ring-save` saying
/// "The mark is not set now", and where the mark has landed on the point,
/// which is copying an empty region.
pub fn copy_region(doc: &Document, state: &SelectionState, ring: &mut KillRing) -> bool {
    let Some(mark) = state.mark() else {
        return false;
    };
    let point = state.current().pos();
    let range = mark.min(point)..mark.max(point);
    if range.is_empty() {
        return false;
    }
    ring.push(region_text(doc, range));
    true
}

/// `meow-yank`: insert the head of the ring. Fails on an empty ring, which is
/// what `yank` signalling "Kill ring is empty" amounts to.
pub fn yank(
    doc: &mut Document,
    state: &mut SelectionState,
    ring: &KillRing,
    history: &mut History,
) -> bool {
    match ring.head() {
        Some(text) => {
            let text = text.to_string();
            insert_at_point(doc, state, history, &text)
        }
        None => false,
    }
}

/// `meow-yank-pop`: replace what the last yank inserted with the next entry.
///
/// The extent to remove is recomputed from the ring rather than remembered:
/// the previous head's length ends at the point, which is where the last yank
/// left it. That is why this is only valid straight after a yank.
pub fn yank_pop(
    doc: &mut Document,
    state: &mut SelectionState,
    ring: &mut KillRing,
    history: &mut History,
) -> bool {
    let Some(previous) = ring.head().map(str::to_string) else {
        return false;
    };
    let pos = state.current().pos();
    let Some(start) = pos.checked_sub(previous.len()) else {
        return false;
    };
    if region_text(doc, start..pos) != previous {
        return false;
    }
    let Some(next) = ring.rotate().map(str::to_string) else {
        return false;
    };
    delete_region(doc, state, history, start..pos);
    insert_at_point(doc, state, history, &next)
}

/// `meow-replace`: the selection becomes the head of the ring, with trailing
/// newlines trimmed so replacing with a copied line does not split the target.
pub fn replace(
    doc: &mut Document,
    state: &mut SelectionState,
    ring: &KillRing,
    history: &mut History,
) -> bool {
    if state.current().selection_type().is_none() {
        return false;
    }
    let Some(text) = ring.head().map(|s| s.trim_end_matches('\n').to_string()) else {
        return false;
    };
    delete_region(doc, state, history, state.current().range());
    insert_at_point(doc, state, history, &text)
}

/// `meow-delete`, which is `meow-C-d`, which is `delete-char`.
///
/// **Not a selection command.** It deletes the character after the point and
/// leaves any selection alone, because `C-d` does — that is what it means for
/// Meow to delegate rather than implement.
pub fn delete_char(doc: &mut Document, state: &mut SelectionState, history: &mut History) -> bool {
    let pos = state.current().pos();
    match crate::next_char_boundary(doc, pos) {
        Some(end) => history.delete(doc, state, pos..end).is_ok(),
        None => false,
    }
}

/// `meow-backward-delete`, which is `backward-delete-char` — and that is
/// `delete-backward-char`, which **honours `delete-active-region`**. So with a
/// selection it deletes the selection, not a character.
///
/// `meow-delete` does not, because `delete-char` does not consult that
/// variable. The asymmetry is Emacs's, not a slip: `x` on a selection removes
/// one character and `X` removes the lot.
pub fn backward_delete(
    doc: &mut Document,
    state: &mut SelectionState,
    history: &mut History,
) -> bool {
    if state.current().selection_type().is_some() {
        delete_region(doc, state, history, state.current().range());
        return true;
    }
    let pos = state.current().pos();
    match crate::previous_char_boundary(doc, pos) {
        Some(start) => history.delete(doc, state, start..pos).is_ok(),
        None => false,
    }
}

/// `meow-open-below`: a newline after this line, point on it.
pub fn open_below(doc: &mut Document, state: &mut SelectionState, history: &mut History) -> bool {
    let (_, end) = crate::line_bounds_of(doc, state.current().pos());
    state.select_without_history(Selection::at(end));
    insert_at_point(doc, state, history, "\n")
}

/// `meow-open-above`: a newline before this line, point still at its start.
///
/// The newline goes in *at* the point and the point does not move, because the
/// elisp wraps `newline` in `save-mark-and-excursion`.
pub fn open_above(doc: &mut Document, state: &mut SelectionState, history: &mut History) -> bool {
    let (start, _) = crate::line_bounds_of(doc, state.current().pos());
    let restore = Selection::at(start);
    let ok = history.insert(doc, state, start, "\n").is_ok();
    // `meow-open-above` wraps `newline` in `save-mark-and-excursion`, so the
    // point stays at the start of the line it opened above.
    state.select_without_history(restore);
    ok
}

/// `meow-insert`: to the start of the selection, then drop it.
pub fn to_selection_start(state: &mut SelectionState) {
    let at = state.current().range().start;
    crate::cancel(state);
    state.select_without_history(Selection::at(at));
}

/// `meow-append`: to the end of the selection, then drop it. With no selection
/// the point does not move — `meow-use-cursor-position-hack` is nil.
pub fn to_selection_end(state: &mut SelectionState) {
    if state.current().selection_type().is_none() {
        return;
    }
    let at = state.current().range().end;
    crate::cancel(state);
    state.select_without_history(Selection::at(at));
}

#[cfg(test)]
mod tests;

/// Where you have been: the [`Place`]s jumps were made *from*.
///
/// A stack rather than a ring, and the name is the point — **Ring** is the
/// three tiers of keybinding fidelity, [`KillRing`] already borrows the word
/// once, and a third sense would make it useless (ADR-0011). Emacs's mark ring
/// is cyclable because the mark does double duty as the selection anchor; the
/// Grab already holds that job here, so the extra affordance buys nothing.
///
/// Beside `KillRing` because it is the same kind of thing: a bounded stack of
/// values with no I/O in it, which is what this crate is for.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Trail {
    entries: VecDeque<Place>,
}

impl Trail {
    /// Emacs's `global-mark-ring-max`, rather than a number invented here.
    pub const MAX: usize = 16;

    pub fn new() -> Self {
        Self {
            entries: VecDeque::new(),
        }
    }

    /// Remember somewhere a jump is leaving.
    ///
    /// Every jump to a Place pushes, including one inside the current file:
    /// "only across files" is a rule you would have to explain, and a jump to a
    /// symbol moves far enough inside one file to want the same undo.
    pub fn push(&mut self, place: Place) {
        self.entries.push_front(place);
        self.entries.truncate(Self::MAX);
    }

    /// Where going back would take you, taking it off.
    pub fn pop(&mut self) -> Option<Place> {
        self.entries.pop_front()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// The files most recently visited, switched to or written, newest first.
///
/// Emacs's `recentf-list`, with Doom's settings: capped at
/// `recentf-max-saved-items` 200, and a file already listed moves to the top
/// when touched again rather than appearing twice, which is what Doom's
/// `doom--recentf-touch-buffer-h` calls frecency (#152). Paths are as the
/// session spells them; making them absolute to keep them across sessions is
/// the host's, because that needs the working directory and this crate does no
/// I/O.
///
/// Beside the [`Trail`] and [`KillRing`] for the reason the Trail gives: a
/// bounded list with no I/O in it. Unlike the Trail it outlives the session.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct RecentFiles {
    paths: VecDeque<String>,
}

impl RecentFiles {
    /// Doom's `recentf-max-saved-items`. Emacs's own default is 20.
    pub const MAX: usize = 200;

    /// A list read back, most recent first. A path named twice keeps its
    /// first, newer, position.
    pub fn from_paths(paths: impl IntoIterator<Item = String>) -> Self {
        Self {
            paths: paths
                .into_iter()
                .fold(VecDeque::new(), |mut kept, path| {
                    if !kept.contains(&path) {
                        kept.push_back(path);
                    }
                    kept
                })
                .into_iter()
                .take(Self::MAX)
                .collect(),
        }
    }

    /// Put `path` at the top, from wherever it was.
    pub fn touch(&mut self, path: &str) {
        self.paths.retain(|kept| kept != path);
        self.paths.push_front(path.to_string());
        self.paths.truncate(Self::MAX);
    }

    /// A file that moved keeps its place in the list (#178).
    ///
    /// Rewritten rather than touched: moving a file is not visiting it, and
    /// pushing it to the front would say you had just been there. A file moved
    /// onto a name the list already held is one file now, and one entry.
    pub fn moved(&mut self, from: &str, to: &str) {
        let mut seen = false;
        self.paths.retain_mut(|path| {
            if path == from {
                *path = to.to_string();
            }
            let first = path != to || !seen;
            seen |= path == to;
            first
        });
    }

    /// A file that was deleted leaves the list (#179).
    ///
    /// `recentf-remove-if-non-kept`, which Doom's `doom-files--update-refs`
    /// calls for any path it can no longer read. The list is an offer to go
    /// back to a file, and there is nothing to go back to.
    ///
    /// Unlike [`RecentFiles::moved`], which keeps the entry where it was: a
    /// file that moved is still a file.
    pub fn gone(&mut self, path: &str) {
        self.paths.retain(|held| held != path);
    }

    /// Newest first.
    pub fn paths(&self) -> impl Iterator<Item = &str> {
        self.paths.iter().map(String::as_str)
    }

    pub fn is_empty(&self) -> bool {
        self.paths.is_empty()
    }
}
