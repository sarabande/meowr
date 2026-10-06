//! Beacon State: record once, replay everywhere.
//!
//! A beacon is a position, not a cursor: the recording happens once, against
//! the real point, and is applied at each beacon afterwards. That ordering is
//! what keeps the edits independent of each other — an edit that shifted a
//! later beacon would silently change where the next replay lands.

use super::*;

/// Beacon State's own state: where the beacons are, and the keys recorded to
/// replay at them.
#[derive(Default)]
pub(crate) struct Beacons {
    /// Where the beacons are, and **over what**.
    ///
    /// A region rather than a position, because Meow's overlays are regions:
    /// `meow--add-beacons-for-line` adds one at `prev .. (line-end-position)`
    /// with the kind `(select . line)`. Positions were enough while the only
    /// way in was `beacon-start`, which types at each one — but `a` appends at
    /// the *end* of a selection and `c` changes the whole of it, and neither
    /// question can be asked of a point.
    pub(crate) at: Vec<Range<usize>>,
    /// Keys recorded since the recording began, if it has — Emacs's
    /// `defining-kbd-macro`.
    pub(crate) recording: Option<Recording>,
}

/// Keys recorded once, to be replayed at every other beacon.
pub(crate) struct Recording {
    pub(crate) keys: Vec<Key>,
    /// Whether the replay selects each beacon's region before replaying.
    ///
    /// The two ways in disagree, and both are right. `beacon-start` records
    /// from a bare point at the first beacon and the keys typed after it say
    /// what to select, so restoring a selection would be answering a question
    /// the macro asks itself. The remaps record the key that *made* the
    /// selection, so the replay has to put one back or `c` falls through to
    /// `meow-change-char` and eats one letter per line.
    pub(crate) region: bool,
}

/// The commands `meow-keymap` remaps to `meow-beacon-noop`.
///
/// A short, specific list rather than "everything that edits": Meow chose
/// these, and no-opping motions as well would make Beacon State inescapable —
/// moving out of the Grab is how it is left.
///
/// Seven against Meow's ten, and the arithmetic is in `tests.rs` rather than
/// here, because it is the sort of claim that goes quietly wrong: this said
/// *two* were absent and named two, and there are three.
///
/// Two of the three would be a claim about a remap that has nothing to remap.
/// `meow-last-buffer` has no command here at all. `meow-insert-exit` is
/// `leave-insert`, which is bound only in Insert State — and Beacon State
/// inherits the *Normal* keymap, so no key in Beacon State reaches it.
///
/// The third is not an absence at all. `meow-C-d` and `meow-delete` are one
/// function under two names — `(defalias 'meow-delete 'meow-C-d)`,
/// `meow-command.el:1884` — and a remap is keyed on the symbol, so Meow has to
/// spell both. `delete` is meowr's one command for that one operation, and
/// listing it twice would be listing it twice.
pub(crate) const BEACON_NOOPS: &[&str] = &[
    "delete",
    "kill-line",
    "save",
    "open-below",
    "open-above",
    "swap-grab",
    "sync-grab",
];

impl Session {
    /// Where the beacons are, and over what.
    pub fn beacons(&self) -> &[Range<usize>] {
        &self.beacon.at
    }

    pub fn is_recording(&self) -> bool {
        self.beacon.recording.is_some()
    }

    /// Meow's `meow--maybe-toggle-beacon-state`, run after every command.
    ///
    /// Beacon State is not entered by a key: it is entered by the point being
    /// inside the Grab. That is why `G` on a multi-line selection and then a
    /// motion into it lands you in Beacon — there is no Beacon key to press,
    /// and looking for one is the first thing that confuses people.
    pub(crate) fn maybe_toggle_beacon(&mut self) {
        // `(unless (or defining-kbd-macro executing-kbd-macro) ...)`: both
        // halves. Without the second, applying a beacon macro would switch
        // State part-way through applying it.
        if self.beacon.recording.is_some() || self.executing {
            return;
        }
        let inside = match (self.current().grab(), self.state) {
            (Some(grab), State::Normal | State::Beacon) => {
                let pos = self.current().selection.current().pos();
                grab.start <= pos && pos <= grab.end
            }
            _ => false,
        };
        match (self.state, inside) {
            (State::Normal, true) => {
                self.state = State::Beacon;
                self.place_beacons();
            }
            (State::Beacon, true) => self.place_beacons(),
            (State::Beacon, false) => {
                self.beacon.at.clear();
                self.state = State::Normal;
            }
            _ => {}
        }
    }

    /// One beacon per line of the Grab, which is `meow--add-beacons-for-line`.
    ///
    /// Meow places them by *selection kind* — per word, per match, per line —
    /// and line is the one that makes the feature what it is: an edit applied
    /// down forty lines.
    fn place_beacons(&mut self) {
        let Some(grab) = self.current().grab() else {
            self.beacon.at.clear();
            return;
        };
        let Some(doc) = self.document() else { return };
        let first = doc.byte_to_line(grab.start).unwrap_or(0);
        let last = doc
            .byte_to_line(grab.end.saturating_sub(1))
            .unwrap_or(first);
        self.beacon.at = (first..=last)
            .filter_map(|line| {
                let start = doc.line_to_byte(line)?;
                // `line-end-position`: up to the newline, never over it. A
                // beacon that swallowed its own `\n` would join the lines when
                // `c` changed it.
                let end = doc
                    .line_to_byte(line + 1)
                    .map(|next| next.saturating_sub(1))
                    .unwrap_or_else(|| doc.byte_len());
                Some(start..end.max(start))
            })
            .collect();
    }

    /// `meow-beacon-start`: record from here, and apply on leaving Insert.
    pub(crate) fn beacon_start(&mut self) -> Action {
        if self.beacon.at.is_empty() {
            self.signals += 1;
            return Action::Nothing;
        }
        // Record *at* the first beacon, so what is typed is applied there and
        // replayed at the rest. Recording wherever the point happened to be
        // put the edit in one place and replayed it in the others.
        let first = self.beacon.at[0].start;
        self.current_mut()
            .selection
            .select_without_history(Selection::at(first));
        self.beacon.recording = Some(Recording {
            keys: Vec::new(),
            region: false,
        });
        self.state = State::Normal;
        Action::Redraw
    }

    /// The Beacon keymap's remaps of the commands that *act*.
    ///
    /// `meow-keymap.el` remaps five of them — `meow-insert`, `meow-append`,
    /// `meow-change`, `meow-kill` and `meow-replace` — and a sixth,
    /// `meow-change-save`, which meowr has no command for, so there is nothing
    /// here to remap it to. That is the same reason `BEACON_NOOPS` is missing
    /// two of Meow's ten, and it is written down for the same reason.
    ///
    /// Two shapes behind one list. `insert`, `append` and `change` **record**:
    /// they run once at the first beacon and the keys typed after them are
    /// replayed at the rest when Insert is left, which is Meow's `'quick`
    /// kmacro. `kill` and `replace` **act**: Meow loops the overlays there and
    /// then, inside one `meow--wrap-collapse-undo`, and nothing is recorded
    /// because nothing is typed.
    pub(crate) fn beacon_remap(&mut self, name: &str, key: Option<Key>) -> Option<Action> {
        if self.state != State::Beacon || self.beacon.at.is_empty() {
            return None;
        }
        match name {
            "insert" | "append" | "change" => Some(self.beacon_record(name, key)),
            "kill" | "replace" => Some(self.beacon_at_each(name)),
            _ => None,
        }
    }

    /// Run `name` at the first beacon and record from there.
    ///
    /// The key that got here is seeded into the recording, because it is part
    /// of what has to happen at every other beacon: the replay is a *macro*,
    /// and a macro that omitted the `c` would change nothing at the beacons
    /// after the first.
    fn beacon_record(&mut self, name: &str, key: Option<Key>) -> Action {
        let Some(first) = self.beacon.at.first().cloned() else {
            return Action::Nothing;
        };
        self.select_beacon(&first);
        self.beacon.recording = Some(Recording {
            keys: key.into_iter().collect(),
            region: true,
        });
        self.state = State::Normal;
        self.dispatch_command(name, key)
    }

    /// Run `name` at every beacon now, as one undoable change.
    ///
    /// Backwards, for the reason the replay goes backwards: an edit at one
    /// beacon shifts every position after it, and the ones before it stay put.
    /// `meow--wrap-collapse-undo` is the `collapsing` flag -- Meow makes the
    /// whole loop one undo step, and a `k` that took eleven presses of `u` to
    /// take back would be a worse feature than no `k` at all.
    fn beacon_at_each(&mut self, name: &str) -> Action {
        let beacons = std::mem::take(&mut self.beacon.at);
        self.state = State::Normal;
        self.collapsing = true;
        match name {
            // Not the same operation, and Meow says so: the overlays get
            // `meow--beacon-delete-region` and not `meow-kill`.
            "kill" => self.delete_beacons(&beacons),
            "replace" => {
                for range in beacons.iter().rev() {
                    self.select_beacon(range);
                    self.dispatch_command(name, None);
                }
            }
            // Spelled out rather than left to `_`, so a third remap added to
            // `beacon_remap` and forgotten here fails in a test run rather
            // than quietly behaving like `replace`.
            other => debug_assert!(false, "`{other}` acts at every beacon?"),
        }
        self.collapsing = false;
        let buffer = self.current_mut();
        buffer.history.boundary();
        // `"replace"` already drained this through each beacon's own
        // recursive `dispatch_command` — harmless to call again on an empty
        // batch. `"kill"` never went through `dispatch_command` at all:
        // `delete_beacons` mutates every beacon directly, and without this
        // call its edits would sit in `history.unread` until whatever
        // command happens to run next drains them — batched in with edits
        // that have nothing to do with them, against a buffer that has
        // since moved on. The same gap `recovered` had, and for the same
        // reason: the reparser trusts a batch to describe a transformation
        // ending at the buffer's current text, and a stale one breaks that
        // outright.
        buffer.after_command(10);
        buffer.grab = None;
        Action::Redraw
    }

    /// Empty every beacon, which is not the same as killing it.
    ///
    /// `meow-beacon-kill-delete` runs `meow-kill` on the real selection and
    /// `meow--beacon-delete-region` — a plain region delete — on the overlays,
    /// and says why in its own docstring: "Only the content in real selection
    /// will be saved to `kill-ring`". The difference is not only the ring.
    /// `meow-kill` on a `line` selection takes the newline with it
    /// (`prepare_region_for_kill`), and a plain delete does not, so killing
    /// each beacon would close the lines up while deleting each one empties
    /// them — and emptying is what a column of beacons is for.
    ///
    /// **A reasoned divergence.** Meow has one real cursor among its overlays
    /// and meowr has none: every beacon here is a beacon, and the point is at
    /// one of them. So all of them are emptied and the first one's text is
    /// what reaches the ring, rather than one line behaving unlike its
    /// neighbours. There is no beacon suite in the fidelity harness to settle
    /// this against, which is #93's third question and is answered on the
    /// ticket rather than guessed at here.
    fn delete_beacons(&mut self, beacons: &[Range<usize>]) {
        let Some(first) = beacons.first() else { return };
        if let Some(doc) = self.document() {
            let text = doc.text();
            self.kill_ring.push(
                text.slice(text.byte_to_char(first.start)..text.byte_to_char(first.end))
                    .to_string(),
            );
        }
        // Backwards: an edit at one beacon shifts every position after it.
        for range in beacons.iter().rev() {
            let buffer = self.current_mut();
            let Content::Ready(doc) = &mut buffer.content else {
                return;
            };
            let _ = buffer
                .history
                .delete(doc, &mut buffer.selection, range.clone());
        }
    }

    /// Select a beacon, as Meow's overlay has it: `(select . line)`.
    fn select_beacon(&mut self, range: &Range<usize>) {
        self.current_mut()
            .selection
            .select_without_history(Selection::typed(
                Axis::Select,
                Kind::LINE,
                range.start,
                range.end,
            ));
    }

    /// Replay the recorded keys at every beacon, in order.
    ///
    /// Sequentially, and from the last beacon backwards: replaying forwards
    /// would have each edit shift the positions of the ones after it. Going
    /// backwards leaves the untouched positions where they were.
    pub(crate) fn beacon_apply(&mut self) -> Action {
        let Some(Recording { keys, region }) = self.beacon.recording.take() else {
            return Action::Nothing;
        };
        // Recomputed from the Grab rather than reused: the edit made while
        // recording shifted everything after it, and the Grab is the thing
        // that already knows how to move with the text.
        self.place_beacons();
        let beacons = std::mem::take(&mut self.beacon.at);
        // The first beacon already had the keys applied as they were typed.
        // The rest are replayed from the end backwards, because replaying
        // forwards would have each edit shift the positions after it.
        for at in beacons.iter().skip(1).rev() {
            if region {
                self.select_beacon(at);
            } else {
                self.current_mut()
                    .selection
                    .select_without_history(Selection::at(at.start));
            }
            self.state = State::Normal;
            let _ = self.replay(&keys);
        }
        self.state = State::Normal;
        self.current_mut().grab = None;
        Action::Redraw
    }
}

#[cfg(test)]
mod tests;
