//! Keypad State: the door to Ring 2.
//!
//! `SPC` leaves Normal State for a state whose entire job is to walk the leader
//! tree in `leader.rs` and dispatch what it lands on. The menu is a *view* of
//! that walk and appears on a delay, because a leader that paints a menu the
//! instant it opens trains the hand to wait for it.

use super::*;

/// The Keypad State's own state: how far into the leader tree we are, and
/// whether the menu has appeared yet.
#[derive(Default)]
pub(crate) struct Keypad {
    /// The keys walked so far.
    pub(crate) keys: Vec<Key>,
    /// The menu appears on a delay — a leader that paints one the instant it
    /// opens trains the hand to wait for it.
    pub(crate) menu_visible: bool,
    /// Whether the menu has been seen *at all* during this walk.
    ///
    /// Distinct from `menu_visible`, which goes false at every level so the
    /// next one repaints. Without the distinction every level charged the
    /// first delay, and going one prefix deep took a full second — ten times
    /// what Doom's `which-key-idle-secondary-delay` asks.
    pub(crate) menu_seen: bool,
}

impl Session {
    pub fn keypad(&self) -> &[Key] {
        &self.keypad.keys
    }

    /// How long before the leader menu is painted.
    ///
    /// Two delays, because which-key has two and Doom sets both: 1.0 before
    /// the *first* menu, so that typing `SPC w s` fluently never flashes one,
    /// and 0.1 for every level after it, because by then you are reading the
    /// menu rather than typing past it.
    ///
    /// One difference from Emacs, deliberate: which-key leaves the previous
    /// level on screen during that second delay, where meowr blanks. meowr's
    /// menu is derived from the path walked so far, so there is no previous
    /// level to hold without storing one — and at 100ms there is nothing to
    /// see. ADR-0006 puts this kind of difference outside Fidelity.
    pub fn leader_delay(&self) -> std::time::Duration {
        if self.keypad.menu_seen {
            self.options.leader_secondary_delay
        } else {
            self.options.leader_delay
        }
    }

    /// Whether the leader menu should be drawn.
    ///
    /// Which-key's model: not immediately, because someone typing `SPC w s`
    /// fluently should never see it, and a menu that flashes on every leader
    /// key is noise. The host arms the timer.
    pub fn menu_visible(&self) -> bool {
        self.keypad.menu_visible && self.state == State::Keypad
    }

    pub fn show_menu(&mut self) -> Action {
        if self.state != State::Keypad || self.keypad.menu_visible {
            return Action::Nothing;
        }
        self.keypad.menu_visible = true;
        self.keypad.menu_seen = true;
        Action::Redraw
    }

    /// What the leader menu lists: the keys below the current prefix, with
    /// their descriptions.
    ///
    /// Descriptions come from the command registry rather than a table beside
    /// it. Two sources for one fact drift, and the one nobody looks at drifts
    /// first.
    pub fn menu(&self) -> Vec<LeaderEntry> {
        self.leader
            .below(&self.keypad.keys)
            .into_iter()
            .map(|(key, entry)| {
                let (description, kind) = match entry {
                    leader::Entry::Command(name) => (
                        self.registry
                            .get(name)
                            .map(|c| c.description().to_owned())
                            .unwrap_or_else(|| name.to_string()),
                        LeaderKind::Command,
                    ),
                    leader::Entry::Prefix(name) => (format!("+{name}"), LeaderKind::Prefix),
                    leader::Entry::Reserved(note) => (note.to_string(), LeaderKind::Reserved),
                };
                LeaderEntry {
                    key: *key,
                    description,
                    kind,
                }
            })
            .collect()
    }

    pub(crate) fn keypad_key(&mut self, key: Key) -> Action {
        match key {
            Key::Esc | Key::Ctrl('g') => {
                self.leave_keypad();
                return Action::Redraw;
            }
            Key::Backspace => {
                // `meow-keypad-undo`: pop one key, and then leave if none are
                // left. Not "leave if there was nothing to pop", which is what
                // this said and which cost a second backspace at every depth
                // (#94) — `SPC w` holds one key, so popping it empties the
                // list and Meow is out. The list excludes the `SPC` itself:
                // `meow-keypad` is `meow-keypad-start-with nil`, and Doom's
                // leader takes the base-keymap branch rather than seeding a
                // dispatch prefix, so `SPC w` is one entry there too.
                //
                // The stay-and-step-back arm is unreachable through the tree
                // meowr ships, whose eight prefixes are all one key deep. It
                // is the general rule rather than "always leave" because a
                // config may nest, and Doom's own tree does.
                self.keypad.keys.pop();
                if self.keypad.keys.is_empty() {
                    self.leave_keypad();
                }
                self.keypad.menu_visible = false;
                return Action::Redraw;
            }
            _ => {}
        }
        // `SPC 0`-`9` are `meow-digit-argument`, and the walk ends on the
        // digit rather than carrying it: keypad looks the key up, finds a
        // command, and `meow--keypad-try-execute` calls `meow--keypad-quit`
        // *before* `meow--keypad-execute`. What the digit then does is
        // `(set-transient-map meow-numeric-argument-keymap)` and
        // `digit-argument`, so the count is armed in Normal State for a Normal
        // State command — which is the point of the key existing, because
        // Normal State's own digits are `meow-expand` and `SPC 3` is the only
        // way left to say "three" to `n`.
        //
        // Looked up wherever the walk is, as Meow does, rather than only at
        // the root: nothing in the tree binds a digit at any depth, so this
        // asks the same question Meow asks and gets the same answer.
        if self.leader.lookup(&self.keypad.keys).is_none()
            && let Key::Char(c) = key
            && let Some(digit) = c.to_digit(10)
        {
            self.leave_keypad();
            self.argument = self.argument.digit(digit as i32);
            self.counting = true;
            return Action::Redraw;
        }
        // Any key at all can be a leader key, chords included — which is how
        // Doom's `C-f` fold prefix is held rather than left out. `Esc`,
        // `C-g` and `Backspace` are taken above, so they never arrive here.
        self.keypad.keys.push(key);
        self.keypad.menu_visible = false;
        match self.leader.lookup(&self.keypad.keys) {
            Some(leader::Entry::Command(name)) => {
                let name = name.clone();
                self.leave_keypad();
                self.dispatch(&name, Some(key))
            }
            Some(leader::Entry::Prefix(_)) => Action::Redraw,
            Some(leader::Entry::Reserved(_)) => {
                // Held for a later milestone. It does nothing and says so
                // rather than falling through to something else.
                self.leave_keypad();
                Action::Redraw
            }
            None if self.leader.is_prefix(&self.keypad.keys) => Action::Redraw,
            None => {
                // `meow-keypad-self-insert-undefined` is nil in the reference
                // configuration: an undefined key does nothing at all, and
                // certainly does not type itself into the buffer.
                self.keypad.keys.pop();
                self.signals += 1;
                Action::Redraw
            }
        }
    }

    fn leave_keypad(&mut self) {
        self.keypad.keys.clear();
        self.keypad.menu_visible = false;
        // Forgotten on the way out, or the *next* `SPC` would inherit the fast
        // delay and flash a menu at someone typing fluently — which is the one
        // thing the first delay exists to prevent.
        self.keypad.menu_seen = false;
        self.state = State::Normal;
    }
}
