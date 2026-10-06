//! Auto-save: work that outlives the process.
//!
//! `Session::quit` refuses while a buffer is modified, but that guard only
//! covers the keyboard. A closed terminal, a broken event stream, a kill
//! signal or a panic all end the process with the work in memory and nothing
//! on disk, and there is nothing to refuse *to* on any of those paths.
//!
//! Emacs's answer is a file beside the original — `#notes.txt#` — written
//! periodically and on the way down, and offered back on the next visit. The
//! point is not the periodic write: it is that the work exists somewhere the
//! editor did not have to survive in order to write.
//!
//! Writing it is I/O, so this crate decides *what* and *where* and the host
//! does it, the same division `Action::Save` and `take_reparse` already draw.
//! A panic is the case that division has to be right for: nothing runs on the
//! way out of one except the hook that restores the terminal, so what saves
//! the work is the auto-save that already happened.

use super::*;

/// What there is to recover, and how far the asking has got.
///
/// Three states rather than an `Option` and a `bool`, which between them can
/// spell a fourth that means nothing — asked-for with nothing to read.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Recovery {
    /// No auto-save file worth offering.
    #[default]
    Nothing,
    /// One exists and the Echo Area has named it. Nothing has been read.
    Offered(String),
    /// `recover-file` has asked for it; the host reads it next.
    Wanted(String),
}

/// A write the host should carry out to keep work off the heap.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Autosave {
    /// Where to write it: the auto-save file, never the visited one. An
    /// auto-save that wrote over the original would turn a crash into data
    /// loss of a different kind.
    pub path: String,
    pub contents: String,
}

/// An [`Autosave`] that has not been rendered to text yet.
///
/// The same pair, held one step earlier: a `Rope` clone is O(1) and shares its
/// nodes, and `to_string` is the part that costs. Keeping the rope means the
/// snapshot can be refreshed after every keystroke and only the write pays.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Snapshot {
    pub path: String,
    pub text: meowr_domain::Rope,
}

/// Emacs's `make-auto-save-file-name`: `#name#` beside the original.
///
/// Beside it rather than in a state directory, because the whole point is
/// that someone who has lost work can find it — and the place they will look
/// is next to the file.
pub fn autosave_path(path: &str) -> String {
    match path.rsplit_once('/') {
        Some((directory, name)) => format!("{directory}/#{name}#"),
        None => format!("#{path}#"),
    }
}

impl Session {
    /// The auto-saves that are due, clearing what it hands out.
    ///
    /// Every buffer, not just the focused one: a kill signal takes all of
    /// them, and the one you were not looking at is the one you will not
    /// think to check.
    ///
    /// A buffer with no file has nowhere to write — Emacs puts those under
    /// `~/`, which is a decision about the user's home directory that this
    /// layer has no business making. `*scratch*` is unsaved by construction
    /// and stays that way.
    pub fn take_autosaves(&mut self, force: bool) -> Vec<Autosave> {
        let mut due = Vec::new();
        if !self.options.auto_save {
            return due;
        }
        for buffer in &mut self.buffers {
            if !buffer.is_modified() || !buffer.is_real() || buffer.since_autosave == 0 {
                continue;
            }
            if !force && buffer.since_autosave < self.options.auto_save_interval {
                continue;
            }
            let (Some(path), Some(doc)) = (buffer.path(), buffer.document()) else {
                continue;
            };
            due.push(Autosave {
                path: autosave_path(path),
                contents: doc.text().to_string(),
            });
            buffer.since_autosave = 0;
        }
        due
    }

    /// Every buffer's unsaved text, paired with where an auto-save would go.
    ///
    /// Cheap enough to take after every keystroke: a `Rope` clone is O(1) and
    /// shares its nodes, which is the same property `take_reparse` already
    /// relies on to hand a snapshot to a worker thread.
    ///
    /// That cheapness is what makes a panic survivable. Nothing runs on the
    /// way out of a panic except the hooks, and a hook cannot reach into the
    /// session — so the work has to be *already* in the hook's hands, and
    /// this is how it gets there.
    pub fn autosave_snapshot(&self) -> Vec<Snapshot> {
        if !self.options.auto_save {
            return Vec::new();
        }
        self.buffers
            .iter()
            .filter(|b| b.is_modified() && b.is_real())
            .filter_map(|b| {
                Some(Snapshot {
                    path: autosave_path(b.path()?),
                    text: b.document()?.text().clone(),
                })
            })
            .collect()
    }

    /// Whether anything is waiting to be auto-saved, so the host can arm an
    /// idle timer rather than waking to find nothing to do.
    pub fn autosave_pending(&self) -> bool {
        self.options.auto_save
            && self.buffers.iter().any(|b| {
                b.is_modified() && b.is_real() && b.since_autosave > 0 && b.path().is_some()
            })
    }

    /// Say that unsaved work from a previous run is sitting at `path`.
    ///
    /// Named rather than silently applied. The auto-save may be older work
    /// than the file, or nonsense from a crash mid-edit, and substituting it
    /// without asking would make recovery something that happens *to* you.
    pub fn offer_recovery(&mut self, path: String) {
        let keys = self.reached_by("recover-file");
        self.echo = Some(format!("Unsaved work in {path}. {keys} to recover it."));
        self.recovery = Recovery::Offered(path);
    }

    /// The auto-save file `recover-file` wants read, if it has been asked for.
    ///
    /// A request the host services, exactly as `take_reparse` is: reading is
    /// the host's, and this crate does no I/O.
    pub fn take_recovery(&mut self) -> Option<String> {
        match &self.recovery {
            Recovery::Wanted(path) => Some(path.clone()),
            _ => None,
        }
    }

    /// Install recovered text, which the host has read.
    ///
    /// The buffer is left **modified**: the file on disk still says what it
    /// said, and recovering is putting you back where you were, not saving
    /// for you. Undo reaches back past it, because the recovery goes through
    /// the history like any other change.
    pub fn recovered(&mut self, text: Result<String, String>) -> Action {
        let path = match std::mem::take(&mut self.recovery) {
            Recovery::Wanted(path) | Recovery::Offered(path) => Some(path),
            Recovery::Nothing => None,
        };
        match text {
            Ok(text) => {
                let buffer = self.current_mut();
                let Content::Ready(doc) = &mut buffer.content else {
                    return Action::Nothing;
                };
                let whole = 0..doc.byte_len();
                let _ = buffer.history.delete(doc, &mut buffer.selection, whole);
                let _ = buffer.history.insert(doc, &mut buffer.selection, 0, &text);
                // Outside `dispatch_command`, so nothing else calls this:
                // `find`/`search` mutate the same way and call it themselves
                // for the same reason. Left undrained, this command's two
                // changes would sit in `history.unread` until whatever
                // command happens to run next — batched in with edits that
                // have nothing to do with them, against a buffer that has
                // since moved on. Found via a 2026-09-11 segfault: the
                // reparser trusts `changes` to describe a transformation
                // ending at the buffer's current text, and a stale batch
                // breaks that outright rather than merely mis-colouring.
                buffer.after_command(10);
                self.echo = Some(match path {
                    Some(path) => format!("Recovered from {path}."),
                    None => "Recovered.".into(),
                });
                Action::Redraw
            }
            Err(message) => {
                self.echo = Some(message);
                Action::Redraw
            }
        }
    }

    /// `recover-file`: ask for the auto-save to be read back in.
    pub(crate) fn recover_file(&mut self) -> Action {
        match std::mem::take(&mut self.recovery) {
            Recovery::Offered(path) | Recovery::Wanted(path) => {
                self.recovery = Recovery::Wanted(path);
                Action::Redraw
            }
            Recovery::Nothing => {
                self.signals += 1;
                self.echo = Some("No auto-saved work to recover.".into());
                Action::Redraw
            }
        }
    }
}
