//! The narrowing widget: one mechanism behind the picker and completion.
//!
//! Prototype #8 settled that these are the same thing. A picker filters a list
//! of candidates supplied up front; completion filters candidates drawn from
//! the buffer. Everything between — the query, the match, the ordering, the
//! selected row, the preview — is identical, so there is one widget and two
//! sources rather than two widgets that drift apart.

use super::*;

/// One Place as a row the picker can show.
///
/// A row shows the line where the source had one to give and `path:line` where
/// it did not -- which is not a formatting choice but the shape of the two
/// upstreams. Grep hands back the matched line; a language server hands back
/// positions. A references list of bare paths is unreadable, and pretending
/// otherwise is what the optional `text` on a [`Place`] exists to avoid
/// (ADR-0011).
///
/// The path is shortened the way the modeline names a buffer. A Place carries
/// an absolute one so it can be opened from anywhere, and an absolute path in a
/// row runs off the frame and takes the line number with it — found in a real
/// terminal, and the same finding `*diagnostics*` made one ticket earlier.
fn result_row(place: &Place) -> Candidate {
    let at = format!(
        "{}:{}",
        meowr_view::modeline::short_path(&place.path, meowr_view::modeline::PATH_SEGMENTS),
        place.line + 1
    );
    match &place.text {
        Some(text) => Candidate::new(text.trim().to_string(), RESULT).annotated(at),
        None => Candidate::new(at, RESULT),
    }
}

/// A picker source that is a **question rather than a list**.
///
/// The widget filters a list fixed when the picker opened, and some sources
/// cannot be fixed then because the *server* is the matcher and it truncates
/// (#144). So the input changes, the source is asked again, and the rows are
/// replaced.
pub(crate) struct Asking {
    /// Which workspaces to ask. See [`crate::LookupKind::Symbols`].
    pub(crate) everywhere: bool,
    /// The input a request is out for, or `None` when nothing is in flight.
    ///
    /// **ADR-0009's stamp, and it is the input rather than the buffer.** The
    /// answers arrive late and out of order, and a reply to "Tra" landing after
    /// a reply to "Trail" would replace the better list with the worse one. An
    /// answer is taken only when it names the question still outstanding.
    pub(crate) asked: Option<String>,
    /// Whether the input has changed since a request last went out, so one is
    /// due once the debounce is up.
    ///
    /// **Independent of `asked` rather than another state of it**, which is why
    /// these are not one enum: typing while a request is out sets this with
    /// that still `Some`, and both facts are live at once — one request in
    /// flight for the query before last, and another owed for what is on screen
    /// now. A single state could only name one of them.
    pub(crate) due: bool,
    /// The query the rows on screen are for, when any are.
    ///
    /// **Not the same as `asked`, and separating them is what makes "any
    /// workspace" work.** The host puts one question to *each* server, so
    /// several answers arrive for one query — and clearing the stamp on the
    /// first of them dropped the rest as though the input had moved on, which
    /// showed whichever server was quickest. An answer for the query already on
    /// screen adds to it; an answer for a newer one replaces it. Invisible with
    /// a single server running, which is why a test now runs two.
    pub(crate) showing: Option<String>,
}

/// Everything the narrowing widget needs, and nothing else.
///
/// Six fields that changed together and sat among two dozen that did not. The
/// widget itself is an `Option` because narrowing is a mode the session is
/// either in or not; the rest is what has to survive alongside it.
#[derive(Default)]
pub(crate) struct Narrowing {
    /// The live widget, when one is open.
    pub(crate) widget: Option<Narrow>,
    /// Where the point was when it opened, so cancelling can put it back.
    pub(crate) origin: Option<Selection>,
    /// Waiting for `<` to name a source.
    pub(crate) awaiting_source: bool,
    /// What a `Driver::Reading` widget is asking for, and what to do with the
    /// answer. `None` whenever the open widget is not one.
    pub(crate) reading: Option<Reading>,
    /// The prompt such a read shows, which is the caller's words rather than
    /// the widget's.
    pub(crate) prompt: String,
    /// What accepting a chosen candidate means.
    pub(crate) accepts: Accepts,
    /// The question this picker's rows come from, when they come from one.
    ///
    /// `None` for every picker whose source is a standing fact about the
    /// session — the buffers that are open, the files in the Project. See
    /// [`Asking`].
    pub(crate) asking: Option<Asking>,
    /// Where the point was when this picker was opened over it.
    ///
    /// The picker's half of what a generated listing keeps in `Listing::from`.
    /// Why either exists is [`Session::leaving_from`], which is the one reader
    /// of both. Found in a real terminal: no test previewed before accepting
    /// until one did.
    pub(crate) from: Option<Place>,
    /// The Project's files, or why there is no Project.
    ///
    /// One value rather than a list beside a flag, for the reason #120 taught
    /// about a directory beside a prefix: two things that must agree and are
    /// not made to will eventually disagree.
    ///
    /// **One slot, not one per buffer**, which is a deliberate shortfall against
    /// #130's "cached per buffer": alternating between two buffers asks git
    /// again each time. The trade is freshness. A listing includes untracked
    /// files (`-o`), so it goes stale the moment you create one -- and the
    /// picker is exactly where you look for a file you just made. A map would
    /// need an invalidation story; projectile has one and binds
    /// `projectile-invalidate-cache` to a key because of it. One subprocess per
    /// picker-open, off the event loop, buys never being wrong. Revisit if a
    /// repository is ever slow enough that the ask is felt.
    pub(crate) project: ProjectAnswer,
    /// Which buffer the window behind was showing when narrowing opened.
    ///
    /// The counterpart of `origin` for the buffer rather than the point: a
    /// preview that points the window somewhere else has to be able to point
    /// it back, or cancelling leaves you looking at a file you did not choose.
    pub(crate) shown: Option<BufferId>,
    /// The buffer a file preview reads into, created lazily and reused.
    ///
    /// One buffer rather than one per candidate: previewing walks a list, and
    /// a buffer per row seen would be a buffer per row for the rest of the
    /// session. Accepting takes this one away — it stops being a preview and
    /// becomes the file you opened — and the next preview makes another.
    pub(crate) preview_buffer: Option<BufferId>,
    /// An open buffer a preview moved the point in, and where the point was.
    ///
    /// `consult--jump-preview`'s restore list, one entry long because one
    /// preview is on screen at a time: put back before the next preview moves
    /// anything and on every way out, accepting included -- the accepted row
    /// then makes its own move (#153). Beside `origin`, which puts back the
    /// buffer the picker was opened over and serves pickers with no Places.
    pub(crate) moved: Option<(BufferId, Selection)>,
    /// A file that was chosen and has not been read yet.
    ///
    /// Accepting a file candidate is the one picker choice this crate cannot
    /// carry out alone, so it records the path and answers [`Action::Open`].
    /// The host reads it and hands the text back to [`Session::opened`].
    pub(crate) pending_open: Option<String>,
    /// The [`Place`] a jump is going to, kept until its file has been read.
    ///
    /// Going to a Place is opening a file *and* going somewhere in it, and the
    /// two are separated by the host's read. Beside `pending_open` rather than
    /// inside it, because a plain file candidate has no Place and an `Option`
    /// in the same slot would make every reader ask which kind this is.
    pub(crate) pending_place: Option<Place>,
    /// Whether that read was asked for by going *back* along the Trail.
    ///
    /// Whose failure it is. A file that will not open is an ordinary failed
    /// buffer when you chose it, and a sentence when the Trail led you there --
    /// ADR-0011 consumes the entry and says so rather than opening a buffer
    /// full of an error nobody asked for.
    pub(crate) pending_back: bool,
    /// Whether that read is of a file the row means to visit even if there is
    /// nothing there.
    ///
    /// Beside `pending_back` and for the same reason: absence is not one thing.
    /// A file chosen from a picker and found gone is a failure, because the
    /// picker had listed it; the config is the ordinary case of a file that has
    /// not been written yet, and `SPC f P` opens it so that it can be (#181).
    /// The row says which it means, so the host never has to guess and no other
    /// row's answer changes shape.
    pub(crate) pending_new: bool,
    /// The [`Place`] each row of a result list names, by candidate index.
    ///
    /// Beside the candidates rather than inside them for the reason
    /// `buffers_shown` already sits beside them: a `Candidate` is what is
    /// matched and shown, and a Place is neither.
    pub(crate) places: Vec<Place>,
    /// The code actions an [`Accepts::CodeAction`] picker offers, by candidate
    /// index, and the question they answer (#169). Beside the candidates for
    /// `places`'s reason.
    pub(crate) offered: Option<(crate::CodeActionsAsked, Vec<crate::CodeAction>)>,
    /// Which buffer each `buffers` candidate names, by candidate index.
    ///
    /// The widget matches text and answers with a row. Getting from that row
    /// back to the buffer it stands for has to come from somewhere, and the
    /// alternative is matching on the name — which two buffers can share, and
    /// which `Buffer::name`'s own docstring says the picker is supposed to
    /// disambiguate.
    pub(crate) buffers_shown: Vec<BufferId>,
}

/// Why meowr is reading a line.
///
/// One variant today. It is an enum rather than a `bool` because `find-file`
/// wants the same read and will be the second — the ticket that built this
/// (#84) says so, and a second caller arriving to find a field called
/// `reading_a_path` would have to widen it anyway.
// Not `Copy` since #139: a `Search` read carries where it will look, and a
// Subject holds a path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reading {
    /// A path for a buffer that has none, after which it is written.
    SaveAs,
    /// A pattern, after which the named subject is searched for it.
    ///
    /// The subject travels with the read rather than being worked out again
    /// when the answer lands: `SPC s d` means the directory of the buffer that
    /// was current *when the key was pressed*, and a preview can have pointed
    /// the window elsewhere by the time the pattern is typed.
    Search(Subject),
    /// A directory, after which a pattern is asked for and that directory
    /// searched. `SPC s D` is the one row that asks twice.
    SearchDirectory,
    /// Text, after which every open buffer is searched for it. `SPC s B`,
    /// which never leaves the session: the buffers are already here, and the
    /// files under them may have moved on.
    SearchOpen,
    /// A new name for the symbol a server prepared a rename of (#168), after
    /// which the rename is asked for.
    Rename(crate::RenameSite),
    /// Where this file should be copied or moved to (#178).
    File(crate::FileAsk),
}

impl Reading {
    /// What to say when this read is cancelled.
    ///
    /// The read knows, and the caller of `cancel_narrowing` does not. It was
    /// one sentence about writing, which was true of the only read there was.
    fn abandoned(&self) -> &'static str {
        match self {
            Reading::SaveAs => "Nothing was written.",
            Reading::Search(_) | Reading::SearchDirectory | Reading::SearchOpen => {
                "Nothing was searched."
            }
            Reading::Rename(_) => "Nothing was renamed.",
            Reading::File(asked) => asked.work.abandoned(),
        }
    }
}

/// What previewing the selected candidate would show.
///
/// Three answers because the four sources need three different things done,
/// and only one of them is I/O — which is the whole reason this is a
/// description rather than an action. The session says what previewing means;
/// the host reads the file, because reading files is the host's.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Preview {
    /// A line of the buffer already on screen. Scroll to it.
    Line(usize),
    /// A buffer that is already open. Point the window at it.
    Buffer(BufferId),
    /// A file that is not open. Its text has to be read first; see
    /// [`Session::previewed`]. A file row naming an open buffer is
    /// [`Preview::Buffer`] instead (#153).
    File(String),
    /// A [`Place`]: a line in a file, open or not (ADR-0011).
    ///
    /// The fourth, and the one the other three could not express. `File` shows
    /// a file's head, which is the wrong thing to show for a reference — you
    /// picked a row because of the line it named, and the preview has to be
    /// that line. In a file nobody has open it is read like a `File` and then
    /// scrolled; in an open one the buffer's own point is moved and put back
    /// (#153). Which of the two is [`Session::preview_read`]'s to say.
    At(Place),
}

impl Preview {
    /// The file this preview is about, if it is about one.
    ///
    /// Written once because [`Session::preview_read`] asks it and
    /// [`Session::previewed`] checks the answer against what it was handed --
    /// and two spellings of "which file is this about" is how a preview comes to
    /// be loaded into a slot that was waiting for a different one.
    pub fn path(&self) -> Option<&str> {
        match self {
            Preview::File(path) => Some(path),
            Preview::At(place) => Some(&place.path),
            Preview::Line(_) | Preview::Buffer(_) => None,
        }
    }
}

/// What accepting a picker candidate means.
///
/// The picker is one widget asking three different questions, and the
/// difference is entirely in what accepting *does*: `meow-visit` chose a word
/// to search for, `M-x` chose a command to run, and everything else chose
/// somewhere to go. A `bool` said which of the first two it was, and a second
/// `bool` beside it would have been two flags that must never both be true --
/// a state machine spelled as booleans, which is how the third question would
/// have arrived as a bug rather than a variant.
///
/// It also answers previewing in one place: only a [`Accepts::Goto`] has
/// anywhere to preview, so the two arms that do not are refused by this type
/// rather than by a growing condition.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum Accepts {
    /// A buffer, a file or a line: go there. The picker's usual business.
    ///
    /// `Goto` rather than `Place`, which it was called until [`Place`] became a
    /// type: two things named Place in one file, one a mode and one a value, is
    /// a name that has stopped revealing anything. It also puts all three
    /// variants in the same grammar -- go there, search for it, run it.
    #[default]
    Goto,
    /// `meow-visit`: the candidate is a word, and accepting searches for it.
    Search,
    /// `M-x`: the candidate is a command name, and accepting runs it.
    Command,
    /// `SPC k a`: the candidate is a code action, and accepting applies it.
    CodeAction,
}

/// What the host said about a buffer's **Project**, or that nobody has asked.
///
/// Named for the *answer* and not for the Project: the glossary's Project is a
/// directory, and this holds the files under one or the sentence saying there is
/// none. Calling it `Project` made the type claim to be the thing it describes.
///
/// Three states rather than two, because "not asked yet" is a real one: the
/// host is asked lazily, when a picker opens rather than for every buffer
/// opened (#130). Collapsing it into an empty list would make a repository
/// with nothing tracked in it indistinguishable from a question nobody has put
/// yet -- which is not a hypothetical, because for a while `find-file` put no
/// question and the empty list was what you saw.
#[derive(Default, Debug, Clone, PartialEq, Eq)]
pub(crate) enum ProjectAnswer {
    #[default]
    Unasked,
    /// What the host said, and which buffer it said it about.
    ///
    /// The stamp is ADR-0009's, for the reason a hover needs one: a session on
    /// this machine accumulates buffers from many repositories, and an answer
    /// about one of them is wrong for the next. Kept *with* the answer rather
    /// than beside it, so the two cannot disagree.
    Answered {
        about: Option<String>,
        /// The files, or why there is no Project. ADR-0010 refuses rather than
        /// guessing a directory, and the host writes the sentence because it is
        /// what knows where it looked.
        files: Result<Vec<String>, String>,
    },
}

impl ProjectAnswer {
    /// The answer, if there is one and it is about `subject`.
    ///
    /// ADR-0009's second check, written once. Three readers ask it -- the
    /// candidate list, the widget's note, and whether `ensure_project_answer`
    /// has a question to put -- and three spellings of "is this answer about
    /// the buffer I am looking at" is how one of them comes to disagree with
    /// the others. Which has happened: a stamped answer drawn without its
    /// stamp is the laundering ADR-0009 records getting wrong once already.
    ///
    /// `Unasked` and *answered about something else* collapse to the same
    /// `None` on purpose. Every caller here wants the same thing of both --
    /// there is nothing to show and a question to put -- and the two are only
    /// distinguishable to a reader who has to remember that they are not.
    fn for_subject(&self, subject: &Option<String>) -> Option<&Result<Vec<String>, String>> {
        match self {
            ProjectAnswer::Answered { about, files } if about == subject => Some(files),
            _ => None,
        }
    }
}

/// The picker's four sources, named.
///
/// `SourceId` is a positional integer, and the place that assigns it and the
/// place that reads it back are forty lines apart in this file. Naming them
/// once is what stops the two drifting.
pub(crate) const BUFFERS: SourceId = SourceId(0);
pub(crate) const RECENT: SourceId = SourceId(1);
pub(crate) const PROJECT: SourceId = SourceId(2);
pub(crate) const LINE: SourceId = SourceId(3);
/// `meow-visit`'s candidates, which are words rather than places and so have
/// no preview.
pub(crate) const VISIT: SourceId = SourceId(4);
/// `M-x`'s candidates: command names, which are neither places nor words.
pub(crate) const COMMANDS: SourceId = SourceId(5);
/// One question's answer: references, symbols, search hits (ADR-0011).
///
/// Deliberately **not** reachable by `<`. The four sources above are standing
/// facts about the session -- the buffers that are open, the files recently
/// visited, the files in the Project, the lines of this buffer -- and `<` means
/// "narrow to one of those". This is the answer to a question just asked, and a
/// source that could be narrowed back to would be stale results wearing a live
/// key.
pub(crate) const RESULT: SourceId = SourceId(6);
/// One question's answer that is a list of files: `SPC f F`'s directory (#149)
/// and `SPC f R`'s recent files in the Project (#148).
///
/// Not reachable by `<`, for `RESULT`'s reason, and not `PROJECT` or `RECENT`,
/// because a row claiming to be one of those standing sources while holding a
/// filtered or re-rooted list is #120's bug.
pub(crate) const FILES: SourceId = SourceId(7);
/// One question's answer that is a list of code actions (#169): titles, and
/// accepting applies one. Not reachable by `<`, for `RESULT`'s reason.
pub(crate) const CODE_ACTIONS: SourceId = SourceId(8);

impl Session {
    pub fn narrowing(&self) -> Option<&Narrow> {
        self.narrow.widget.as_ref()
    }

    pub fn narrowing_mut(&mut self) -> Option<&mut Narrow> {
        self.narrow.widget.as_mut()
    }

    /// Candidate files, supplied by the host. Walking a directory is I/O and
    /// belongs to the binary, as reading a file does.
    ///
    /// `Err` is a file in no repository with `project-scope` on: ADR-0010
    /// refuses rather than guessing a directory, and the host supplies the
    /// reason because it is what knows where it looked.
    pub fn set_project(&mut self, about: Option<String>, files: Result<Vec<String>, String>) {
        // Re-checked on arrival, because the picker may be about a different
        // buffer than the one this was asked about -- ADR-0009, and the reason
        // the answer carries a stamp at all.
        let mine = about == self.project_subject();
        let fresh: Vec<Candidate> = match (&files, mine) {
            (Ok(files), true) => files
                .iter()
                .map(|path| Candidate::new(path.clone(), PROJECT))
                .collect(),
            _ => Vec::new(),
        };
        if !mine {
            // Dropped, not stored. ADR-0009 discards the *stale* answer, and
            // overwriting here would evict a good answer about the buffer the
            // picker is actually on -- turning a late reply about somewhere else
            // into an empty list here.
            return;
        }
        self.narrow.project = ProjectAnswer::Answered { about, files };
        // Only into a widget that has a project source to put them in. `M-x`
        // and `meow-visit` build a picker with *no* sources, so `narrowed` is
        // `None` and nothing filters: extending one of those turns file paths
        // into command candidates, and accepting one answers "`README.md` is
        // not a command". The arrival check has to ask which *widget* asked,
        // not only which buffer.
        if let Some(widget) = self.narrow.widget.as_mut()
            && !fresh.is_empty()
            && widget.sources().iter().any(|source| source.id == PROJECT)
        {
            widget.extend(fresh);
        }
        // A refusal is not said here. `narrow_view` reads it off the answer
        // every frame, which is the second of ADR-0009's two checks and also
        // what makes one mechanism rather than two that can disagree: a
        // sentence pushed at the Echo Area now would be covered by the very
        // picker that asked for it.
    }

    /// Which buffer's Project the host should look up, if one was asked for.
    ///
    /// Drained rather than read, as `pending_hover` is: a question answered
    /// twice would run git twice and leave the second answer with nothing
    /// waiting for it.
    pub fn pending_project(&mut self) -> Option<Option<String>> {
        self.pending_project.take()
    }

    /// The buffer a Project question is about.
    ///
    /// The one the window *behind* the picker is showing, not whatever a
    /// preview has put on screen -- ADR-0009's warning about a buffer that
    /// changes with no key pressed reaches this directly, because previewing a
    /// candidate makes a different buffer current while the picker is open.
    fn project_subject(&self) -> Option<String> {
        let id = self
            .narrow
            .shown
            .unwrap_or_else(|| self.current_buffer_id());
        self.buffer(id).path().map(str::to_owned)
    }

    /// The project source was asked for, so see that there is an answer about
    /// the buffer it ought to be about.
    fn ensure_project_answer(&mut self) -> Action {
        let subject = self.project_subject();
        if self.narrow.project.for_subject(&subject).is_none() {
            self.pending_project = Some(subject);
            return Action::Project;
        }
        // An `Err` needs nothing done here: `narrow_view` puts it on the
        // widget's own line, where an open picker can actually show it.
        Action::Redraw
    }



    /// The four M1 sources: open buffers, recent files, project files, and the
    /// lines of the current buffer.
    fn picker_candidates(&self) -> (Vec<Source>, Vec<Candidate>, Vec<BufferId>) {
        let sources = vec![
            Source {
                id: BUFFERS,
                name: "buffers".into(),
                key: 'b',
            },
            Source {
                id: RECENT,
                name: "recent".into(),
                key: 'r',
            },
            Source {
                id: PROJECT,
                name: "project".into(),
                key: 'p',
            },
            Source {
                id: LINE,
                name: "line".into(),
                key: 'l',
            },
        ];
        let mut candidates = Vec::new();
        // The buffers come first, and `buffers_shown` is built in the same
        // pass so the two cannot fall out of step. Slots that are not buffers
        // anyone opened are skipped — the preview slot, which nobody has chosen
        // yet, and killed ones, which nobody has any more.
        let mut buffers_shown = Vec::new();
        for (index, buffer) in self.buffers.iter().enumerate() {
            if !buffer.is_real() {
                continue;
            }
            candidates.push(
                Candidate::new(buffer.name(), BUFFERS)
                    .annotated(buffer.path().unwrap_or("").to_string()),
            );
            buffers_shown.push(BufferId(index));
        }
        for path in self.recent.paths() {
            candidates.push(Candidate::new(path.to_string(), RECENT));
        }
        // Only the answer about *this* buffer. A stale one belongs to a file
        // the picker is not about, and ADR-0009 is why it is dropped here as
        // well as on arrival.
        let listed: &[String] = match self.narrow.project.for_subject(&self.project_subject()) {
            Some(Ok(files)) => files,
            _ => &[],
        };
        for path in listed {
            candidates.push(Candidate::new(path.clone(), PROJECT));
        }
        if let Some(doc) = self.document() {
            for line in 0..doc.line_count() {
                if let Some(text) = doc.line(line) {
                    candidates.push(
                        Candidate::new(text.to_string().trim_end().to_string(), LINE)
                            .annotated((line + 1).to_string()),
                    );
                }
            }
        }
        (sources, candidates, buffers_shown)
    }

    /// Open the picker, and ask for the Project it advertises a source for.
    ///
    /// The ask is *here* rather than in the callers because the callers are
    /// where it went wrong. It lived in `open_picker_on` and in the `<p`
    /// branch, and `find-file` -- the ordinary way the picker is opened, and
    /// the busiest of the three -- opened one without asking, so its project
    /// source was empty with nothing said. A question nobody put is
    /// indistinguishable from a repository with nothing in it.
    ///
    /// Which is #127's lesson one layer up: a step every caller has to
    /// remember is a step a caller will forget, and the compiler cannot say
    /// which. Opening a picker *is* advertising the source, so opening one is
    /// what asks, and a fourth entry point gets it for nothing.
    pub fn open_picker(&mut self) -> Action {
        let (sources, candidates, buffers_shown) = self.picker_candidates();
        self.narrow.origin = Some(self.current().selection.current());
        self.narrow.shown = Some(self.current_buffer_id());
        self.narrow.buffers_shown = buffers_shown;
        self.narrow.widget = Some(Narrow::new(Driver::Picker, sources, candidates));
        self.ensure_project_answer()
    }

    /// The picker, already narrowed to one source.
    ///
    /// Doom reaches two of these sources by their own keys — `SPC .` is
    /// `switch-to-buffer` and `SPC RET` is `recentf-open-files` — where meowr
    /// has one widget over four. Opening it pre-narrowed is how both readings
    /// hold at once: the same picker, entered where you meant.
    ///
    /// `<` still re-narrows from here, so this is a shortcut rather than a
    /// mode. Consult behaves the same way.
    #[track_caller]
    pub fn open_picker_on(&mut self, source: char) -> Action {
        let asked = self.open_picker();
        if let Some(widget) = self.narrow.widget.as_mut() {
            // `narrow_to` answers `false` for a key no source has, and the
            // picker would then open showing *everything* — `SPC .` quietly
            // becoming `SPC f f`. The keys are `picker_candidates`' business
            // and nothing ties them to the callers here, so this is the tie:
            // wrong in a test run rather than surprising in use.
            //
            // **Narrowed outside the assertion**, which the tie itself once
            // was not: `debug_assert!` is compiled out of release builds along
            // with its argument, so for every row through here the binary
            // opened showing everything, the very failure above. The lint that
            // now forbids it says the rest, in the workspace `Cargo.toml` (#151).
            let narrowed = widget.narrow_to(source);
            debug_assert!(narrowed, "no picker source is on `{source}`");
        }
        // `open_picker` already asked. `SPC SPC` comes in through here (#147)
        // and needs the answer as much as `<p` does, and the ask is one
        // layer down now precisely so neither this nor a fifth caller has to
        // remember it.
        asked
    }

    /// Open the picker over the answer to one question.
    ///
    /// Sourceless, which is the shape `M-x` and `meow-visit` already take: `<`
    /// offers nothing, because there is nothing standing to narrow *to*.
    ///
    /// A row shows the line where the source had one to give and `path:line`
    /// where it did not -- which is not a formatting choice but the shape of
    /// the two upstreams. Grep hands back the matched line; a language server
    /// hands back positions. A references list of bare paths is unreadable, and
    /// pretending otherwise is what the optional `text` on a [`Place`] exists
    /// to avoid (ADR-0011).
    pub fn open_results(&mut self, places: Vec<Place>) -> Action {
        if places.is_empty() {
            // **Here rather than in each caller**, which is what a review found
            // three copies of: `found`, `searched` and the rows that search
            // what is already open each drew this line for themselves. It is
            // ADR-0010's distinction and it belongs to *opening a picker over
            // results* rather than to any one question -- nothing found is an
            // answer and deserves saying, where a picker with no rows in it
            // reads as a feature that did not run. Now there is no way to open
            // an empty one.
            self.echo = Some("Nothing found.".to_string());
            return Action::Redraw;
        }
        let candidates = places.iter().map(result_row).collect();
        self.narrow.origin = Some(self.current().selection.current());
        self.narrow.shown = Some(self.current_buffer_id());
        // Taken now, before previewing can move the point off it.
        self.narrow.from = self.here();
        self.narrow.places = places;
        self.narrow.accepts = Accepts::Goto;
        self.narrow.widget = Some(Narrow::new(Driver::Picker, Vec::new(), candidates));
        Action::Redraw
    }

    /// Open the picker over code actions' titles (#169).
    ///
    /// Sourceless, as `open_results` is, and with no preview: a code action is
    /// not somewhere to go, which [`Accepts::CodeAction`] says.
    pub(crate) fn offer_code_actions(&mut self, asked: CodeActionsAsked, actions: Vec<CodeAction>) -> Action {
        let candidates = actions
            .iter()
            .map(|action| Candidate::new(action.title.clone(), CODE_ACTIONS))
            .collect();
        self.narrow.origin = Some(self.current().selection.current());
        self.narrow.offered = Some((asked, actions));
        self.narrow.accepts = Accepts::CodeAction;
        self.narrow.widget = Some(Narrow::new(Driver::Picker, Vec::new(), candidates));
        Action::Redraw
    }

    /// Open the picker over a one-shot list of files.
    ///
    /// Sourceless, as `open_results` is: the rows answer one question and are
    /// not a standing fact to narrow back to. Its callers come through
    /// `file_answer`, which says an empty list in each row's own words before
    /// there is a picker to be empty.
    pub(crate) fn open_files(&mut self, files: Vec<String>) -> Action {
        let candidates = files
            .into_iter()
            .map(|path| Candidate::new(path, FILES))
            .collect();
        self.narrow.origin = Some(self.current().selection.current());
        self.narrow.shown = Some(self.current_buffer_id());
        self.narrow.accepts = Accepts::Goto;
        self.narrow.widget = Some(Narrow::new(Driver::Picker, Vec::new(), candidates));
        Action::Redraw
    }

    /// Open the picker over a question, re-asked as the input changes.
    ///
    /// Sourceless like `open_results`, and empty: there is nothing to show
    /// until something has been asked, and `<` has nothing to offer because the
    /// rows are not a standing fact about the session.
    pub fn open_asking(&mut self, everywhere: bool) -> Action {
        self.narrow.origin = Some(self.current().selection.current());
        self.narrow.shown = Some(self.current_buffer_id());
        self.narrow.from = self.here();
        self.narrow.places = Vec::new();
        self.narrow.accepts = Accepts::Goto;
        self.narrow.asking = Some(Asking {
            everywhere,
            asked: None,
            due: false,
            showing: None,
        });
        self.narrow.widget = Some(Narrow::new(Driver::Picker, Vec::new(), Vec::new()));
        Action::Redraw
    }

    /// Whether a re-asking picker wants its question put again.
    ///
    /// Read rather than drained, because the host arms a timer on it --
    /// `hints_pending` and `resting_hover` have the same shape and exist for
    /// the same reason. The debounce is `ask-debounce`, so this is "the input
    /// stopped changing" rather than a stopwatch on the picker.
    pub fn asking_pending(&self) -> bool {
        self.narrow
            .asking
            .as_ref()
            .is_some_and(|asking| asking.due && self.asking_input().is_some())
    }

    /// The input, when there is enough of it to ask about.
    ///
    /// `ask-min-input` is consult's 3: the first two characters of a symbol
    /// name match most of a workspace, so the answer would be truncated where
    /// it is longest and least useful -- which is the failure an empty query
    /// already demonstrated, one keystroke further along.
    fn asking_input(&self) -> Option<String> {
        let input = self.narrow.widget.as_ref()?.input();
        (input.chars().count() >= self.options.ask_min_input).then(|| input.to_string())
    }

    /// The question to put, once the input has sat still long enough.
    pub fn take_ask(&mut self) -> Option<Lookup> {
        let query = self.asking_input()?;
        let everywhere = {
            let asking = self.narrow.asking.as_mut()?;
            if !asking.due {
                return None;
            }
            asking.due = false;
            asking.asked = Some(query.clone());
            asking.everywhere
        };
        let buffer = self.current();
        Some(Lookup {
            path: buffer.path().unwrap_or_default().to_owned(),
            at: buffer.selection.current().pos(),
            kind: LookupKind::Symbols { query, everywhere },
        })
    }

    /// What a server found for one query, on its way into the open picker.
    ///
    /// **Stamped by the input** (ADR-0009): an answer is drawn only when it
    /// names the question still outstanding, so a reply to "Tra" landing after
    /// a reply to "Trail" is dropped rather than replacing the better list with
    /// the worse one.
    pub fn found_symbols(&mut self, query: &str, places: Vec<Place>) -> Action {
        let Some(asking) = self.narrow.asking.as_mut() else {
            return Action::Nothing;
        };
        // The stamp is *not* cleared here. Several servers may answer one
        // question, and taking only the first was `SPC k J` showing whichever
        // was quickest. It is replaced when a newer question goes out, which is
        // the only thing that makes an answer stale.
        if asking.asked.as_deref() != Some(query) {
            return Action::Nothing;
        }
        let adding = asking.showing.as_deref() == Some(query);
        asking.showing = Some(query.to_string());
        if adding {
            self.narrow.places.extend(places);
        } else {
            self.narrow.places = places;
        }
        // Rebuilt from the whole list rather than appended to, so the rows and
        // the Places an accepted row indexes into cannot come to disagree about
        // how many there are.
        let candidates = self.narrow.places.iter().map(result_row).collect();
        if let Some(widget) = self.narrow.widget.as_mut() {
            widget.replace_candidates(candidates);
        }
        Action::Redraw
    }

    /// Complete the symbol at the point, from the symbols already in the
    /// buffer. The same widget: only the driver differs.
    pub fn open_completion(&mut self) {
        let Some(doc) = self.document() else { return };
        let pos = self.current().selection.current().pos();
        let Some(range) = self.things.inner(doc, pos, "symbol") else {
            return;
        };
        let range = range.start..pos.max(range.start);
        let text = doc.text();
        let prefix = text
            .slice(text.byte_to_char(range.start)..text.byte_to_char(range.end))
            .to_string();
        let mut words: Vec<String> = Vec::new();
        let whole = text.to_string();
        for word in whole.split(|c: char| !(c.is_alphanumeric() || c == '_')) {
            if word.len() > 2 && !words.iter().any(|w| w == word) {
                words.push(word.to_string());
            }
        }
        let candidates = words
            .into_iter()
            .map(|w| Candidate::new(w, SourceId(0)))
            .collect();
        self.narrow.origin = Some(self.current().selection.current());
        self.narrow.widget = Some(Narrow::completing(candidates, &prefix, range));
    }

    /// Accept the selection. For completion this replaces the prefix; for the
    /// picker it opens what was chosen.
    pub fn accept_narrowing(&mut self) -> Action {
        // Accepting keeps where it took you, so the point is not put back.
        // The *window* is a different question: a `line` candidate means a line
        // in the buffer narrowing was opened over, and a file preview may have
        // pointed the window somewhere else since. Kept rather than dropped, so
        // the `Line` arm below can put it back.
        self.narrow.origin = None;
        let opened_over = self.narrow.shown.take();
        // What the selected row names, worked out before the widget is taken.
        // This is the same question previewing asks, and asking it once is
        // what keeps the two answers from drifting: accepting used to walk
        // `buffers_shown` and parse the annotation itself, and so grew its own
        // opinion of what a `recent` row meant, which was nothing.
        let chosen_target = self.preview_target();
        // Put back before going anywhere, as consult does on every way out: the
        // row accepted need not be the row last previewed, because Enter is not
        // on the preview's timer. The jump below repeats the move when it was.
        self.put_back_preview_move();
        self.narrow.asking = None;
        self.narrow.buffers_shown.clear();
        self.narrow.places.clear();
        let Some(widget) = self.narrow.widget.take() else {
            return Action::Nothing;
        };
        // A line read has no candidates, so it has to answer before the
        // `selected()` below decides there is nothing to accept.
        if let Some(reading) = self.narrow.reading.take() {
            // Not trimmed. A trailing space is a legal character in a filename
            // and Emacs does not trim one either — and trimming would sit
            // oddly beside taking the trouble to let a space *into* the name.
            let line = widget.input().to_string();
            self.narrow.prompt.clear();
            return self.accept_read(reading, line);
        }
        let offered = self.narrow.offered.take();
        let Some(chosen) = widget.selected().cloned() else {
            self.narrow.accepts = Accepts::default();
            return Action::Redraw;
        };
        match std::mem::take(&mut self.narrow.accepts) {
            Accepts::Goto => {}
            Accepts::Search => {
                self.push_search(chosen.text.clone());
                return self.search(true);
            }
            Accepts::Command => return self.run_named(&chosen.text),
            Accepts::CodeAction => {
                let row = widget.matches().get(widget.selected_index()).map(|row| row.index);
                return match offered.zip(row) {
                    Some(((asked, actions), row)) => match actions.into_iter().nth(row) {
                        Some(action) => self.choose_action(asked, action),
                        None => Action::Redraw,
                    },
                    None => Action::Redraw,
                };
            }
        }
        match widget.driver() {
            // Handled above, before `selected()` had a chance to find nothing.
            Driver::Reading => Action::Redraw,
            Driver::Completion => {
                let Some(range) = widget.replacing.clone() else {
                    return Action::Redraw;
                };
                let buffer = self.current_mut();
                let Content::Ready(doc) = &mut buffer.content else {
                    return Action::Redraw;
                };
                let _ = buffer
                    .history
                    .delete(doc, &mut buffer.selection, range.clone());
                let _ =
                    buffer
                        .history
                        .insert(doc, &mut buffer.selection, range.start, &chosen.text);
                Action::Redraw
            }
            // Accepting must not depend on the preview having fired.
            // `consult-preview-delay` is 0.4s and typing a name and pressing
            // Enter beats it easily, and then "keeps where it took you" would
            // have kept you where you started.
            //
            // Opening a file is I/O; the host does it. Choosing a *line* moves
            // the point, and choosing a buffer points the window, and neither
            // is.
            Driver::Picker => match chosen_target {
                Some(Preview::Buffer(id)) => {
                    self.show(id);
                    Action::Redraw
                }
                Some(Preview::File(path)) => self.open_chosen(path),
                // The file, and then the line in it. The preview may already
                // have read and scrolled it, in which case `open_chosen`
                // promotes that buffer and the line is already right -- so the
                // scroll is repeated rather than skipped, which costs nothing
                // and means the two paths do not have to agree about which
                // happened.
                Some(Preview::At(place)) => self.jump_to(place),
                Some(Preview::Line(line)) => {
                    self.return_to(opened_over);
                    self.go_to_line(line);
                    Action::Redraw
                }
                None => Action::Redraw,
            },
        }
    }

    /// `M-x`: choose a command by name and run it.
    ///
    /// Every name the registry has, including the seventy-six per-Thing ones
    /// that no key reaches. Those exist *so that they can be named* (#78), and
    /// a prompt whose whole job is spelling names would be a strange place to
    /// start hiding them. nucleo is orderless already, so a long list costs
    /// the typist nothing: `iotr` finds `inner-of-thing/round`.
    ///
    /// No sources: `<` narrows the picker to buffers or files, and there is
    /// only one kind of thing here. And no preview -- a command is not
    /// somewhere to go, which [`Accepts`] is what says.
    pub(crate) fn execute_extended_command(&mut self) -> Action {
        let mut names: Vec<&str> = self.registry.names().collect();
        names.sort_unstable();
        let candidates = names
            .into_iter()
            .map(|name| Candidate::new(name.to_string(), COMMANDS))
            .collect();
        self.narrow.origin = Some(self.current().selection.current());
        self.narrow.widget = Some(Narrow::new(Driver::Picker, Vec::new(), candidates));
        self.narrow.accepts = Accepts::Command;
        Action::Redraw
    }

    /// Run the command the picker chose.
    ///
    /// The State check is made here rather than left to `dispatch_command`,
    /// which answers `Action::Nothing` for a command that is not valid where
    /// you are. That silence is right for a *key* -- an unbound key says
    /// nothing in Emacs either -- and wrong for a name you were offered and
    /// chose, which is #80's bar: no wordless beep. The registry is one list
    /// and `M-x` offers all of it, so the refusal is the price of not having
    /// to filter the list by State on every keystroke.
    fn run_named(&mut self, name: &str) -> Action {
        let Some(command) = self.registry.get(name) else {
            return self.refuse_aloud(format!("`{name}` is not a command."));
        };
        if !self.available_here(command) {
            let state = self.state.name();
            return self.refuse_aloud(format!("`{name}` is not available in {state} State."));
        }
        self.dispatch_command(name, None)
    }

    /// Open the line read, with the words to ask it in.
    pub(crate) fn read_line(&mut self, reading: Reading, prompt: &str, seed: &str) {
        let mut widget = Narrow::reading();
        widget.set_input(seed.to_string());
        self.narrow.reading = Some(reading);
        self.narrow.prompt = prompt.to_string();
        self.narrow.widget = Some(widget);
    }

    /// What a finished line read does with its line.
    fn accept_read(&mut self, reading: Reading, line: String) -> Action {
        match reading {
            Reading::SaveAs => {
                if line.is_empty() {
                    return self.refuse("No name given, so nothing was written.");
                }
                self.set_path(&line);
                // Remembered on the Buffer so the request can say the name is
                // new and the host knows to look before writing over anything.
                self.current_mut().fresh_name = true;
                // Through `write-file`, not by answering `Action::Save`: that
                // is where `before-save` runs and where a save that cannot
                // happen is refused, and a save-as is a save. It is dispatched
                // with no argument of its own, so the `SPC u` the save-as was
                // asked with is carried across rather than lost (ADR-0014).
                let skip_format = self.skip_format_once;
                let action = self.dispatch_command("write-file", None);
                self.skip_format_once = skip_format;
                action
            }
            Reading::Search(subject) => {
                if line.is_empty() {
                    // Not an empty pattern handed to `rg`, which matches every
                    // line of every file: a list of the whole Project is not an
                    // answer to a question nobody finished asking.
                    return self.refuse("No pattern given, so nothing was searched.");
                }
                self.ask_search(Pattern::Regex(line), subject)
            }
            Reading::Rename(site) => self.rename_to(site, line),
            Reading::File(asked) => self.file_work_to(asked, line),
            Reading::SearchOpen => {
                if line.is_empty() {
                    return self.refuse("No pattern given, so nothing was searched.");
                }
                self.search_open(&line)
            }
            Reading::SearchDirectory => {
                if line.is_empty() {
                    return self.refuse("No directory given, so nothing was searched.");
                }
                self.read_line(
                    Reading::Search(Subject::Directory(line)),
                    "Search that directory: ",
                    "",
                );
                Action::Redraw
            }
        }
    }

    pub fn cancel_narrowing(&mut self) -> Action {
        // Whatever the read was for, it is not happening.
        if let Some(reading) = self.narrow.reading.take() {
            self.narrow.widget = None;
            self.narrow.prompt.clear();
            return self.refuse(reading.abandoned());
        }
        self.put_back_preview_move();
        let origin = self.narrow.origin.take();
        let shown = self.narrow.shown.take();
        self.narrow.from = None;
        self.narrow.asking = None;
        self.narrow.buffers_shown.clear();
        self.narrow.places.clear();
        self.narrow.offered = None;
        // What accepting would have meant goes with the widget: left behind, it
        // made the next picker accept a buffer as a code action, or a command.
        self.narrow.accepts = Accepts::default();
        match self.narrow.widget.take() {
            Some(_) => {
                // Put the window back where previewing found it, and then the
                // point. Both, and in that order: a preview you cancelled is a
                // move you did not ask for, and pointing the window at another
                // buffer is a bigger one than scrolling this one. The preview
                // buffer is left where it is for the next preview to reuse —
                // it is no longer on screen and no longer offered.
                if let Some(shown) = shown {
                    self.show(shown);
                }
                if let Some(origin) = origin {
                    self.current_mut().selection.select_without_history(origin);
                    self.scroll_into_view();
                }
                Action::Redraw
            }
            None => Action::Nothing,
        }
    }

    /// Install the text of a previewed file, which the host has read.
    ///
    /// `path` says which file it is, because the selection may have moved on
    /// while the read was in flight — the same staleness check
    /// [`Session::apply_reparse`] makes, and for the same reason.
    pub fn previewed(&mut self, path: &str, contents: Result<String, String>) -> Action {
        let target = self.preview_target();
        if target.as_ref().and_then(Preview::path) != Some(path) {
            return Action::Nothing;
        }
        self.put_back_preview_move();
        let languages = self.languages();
        // Built whole rather than edited in place. The slot is reused across
        // candidates, and a buffer carries a parse, a revision, a selection
        // and a queue of pending edits that all belong to the *last* file it
        // held — replacing it is how none of them survive into the next one.
        let mut buffer = Buffer::new();
        buffer.preview = true;
        // Nothing here has been chosen yet, so nothing here can be typed into.
        buffer.read_only = ReadOnly::Always;
        buffer.set_path(path, &languages);
        match contents {
            Ok(text) => buffer.loaded(Document::from_text(&text)),
            Err(message) => buffer.failed(message),
        }
        match self.narrow.preview_buffer {
            Some(id) => {
                self.buffers[id.0] = buffer;
                self.show(id);
            }
            None => {
                let id = self.open(buffer);
                self.narrow.preview_buffer = Some(id);
            }
        }
        // A Place was picked for the line it named, so the head of the file is
        // the one thing worth less than not previewing at all.
        if let Some(Preview::At(place)) = target {
            self.go_to_place(&place);
        }
        Action::Redraw
    }

    /// Make a chosen file real, reading it only if the preview has not.
    ///
    /// A previewed file stops being a preview when it is what was chosen: the
    /// choice that would make it real has now been made, and it is the buffer
    /// you opened. Matched on the path rather than on "the preview buffer is
    /// the one on screen", because arrowing past a previewed file to one the
    /// preview has not reached yet leaves the first still showing — and
    /// promoting that would open the file you moved off.
    ///
    /// **And not at all if it is already open**, which is asked first: before
    /// #150 a preview of the open file was promoted beside it, and without a
    /// preview it was read again, so `SPC SPC` on the file being edited made a
    /// second buffer for it.
    pub(crate) fn open_chosen(&mut self, path: String) -> Action {
        if let Some(id) = self.buffer_visiting(&path) {
            self.show(id);
            return Action::Redraw;
        }
        if let Some(id) = self.narrow.preview_buffer
            && self.buffers[id.0].path() == Some(path.as_str())
        {
            self.narrow.preview_buffer = None;
            self.buffers[id.0].preview = false;
            self.buffers[id.0].read_only = ReadOnly::No;
            self.show(id);
            self.touch_recent();
            return Action::Redraw;
        }
        // Enter beat the delay, so there is no text yet and reading it is the
        // host's.
        self.ask_to_open(path, false);
        Action::Open
    }

    /// Ask the host to read `path`, saying whether nothing there is an answer
    /// rather than a failure (#181).
    ///
    /// The only place either field is set, because they are one question and a
    /// row that leaves them apart lends its leave to the next row. A read can
    /// still be in flight when the next one is asked for: `SPC f P` followed by
    /// a picker choice made an empty buffer that called a file the picker had
    /// lost new, which is the failure a chosen file's disappearance is.
    pub(crate) fn ask_to_open(&mut self, path: String, new: bool) {
        self.narrow.pending_open = Some(path);
        self.narrow.pending_new = new;
    }

    /// The file [`Action::Open`] is asking the host to read.
    pub fn pending_open(&self) -> Option<&str> {
        self.narrow.pending_open.as_deref()
    }

    /// Whether nothing there is an answer rather than a failure (#181).
    ///
    /// Asked with [`Session::pending_open`] and not folded into it: the path is
    /// what the host reads, and this is what it does when the read says there is
    /// no such file. Only `SPC f P` sets it, so `Opened::Missing` reaches
    /// [`Session::opened`] for the config and for nothing else.
    pub fn pending_open_may_be_new(&self) -> bool {
        self.narrow.pending_new
    }

    /// Install a file the host has read because it was *chosen*, not previewed.
    ///
    /// The sibling of [`Session::previewed`], and different from it in the two
    /// ways the choice makes: the buffer is a new one rather than the reused
    /// preview slot, and it is neither a preview nor read-only, because the
    /// choice that would make it real has been made.
    pub fn opened(&mut self, path: &str, contents: Opened) -> Action {
        // The same staleness check `previewed` makes: a second choice can be
        // made while the first read is still in flight.
        if self.narrow.pending_open.as_deref() != Some(path) {
            return Action::Nothing;
        }
        self.narrow.pending_open = None;
        let back = std::mem::take(&mut self.narrow.pending_back);
        self.narrow.pending_new = false;
        let place = self.narrow.pending_place.take();
        // Going back to a file that will not open is a Trail entry being spent,
        // not a buffer to open. ADR-0011 consumes it and says so, because a
        // buffer full of "No such file" is not what going back should leave on
        // screen -- and it is consumed either way, so a second press tries the
        // next entry rather than this one again.
        if back && !matches!(contents, Opened::Read(_)) {
            if let Some(place) = place {
                self.say(&format!("{} is no longer there.", place.path));
            }
            return Action::Redraw;
        }
        // A buffer may have come to visit the file while it was being read --
        // a workspace edit opens the files it reaches, unshown (#167) -- and
        // that buffer holds the edit the text read from disk does not.
        if let Some(id) = self.buffer_visiting(path) {
            self.show(id);
            self.touch_recent();
            if let Some(place) = place {
                self.go_to_place(&place);
            }
            return Action::Redraw;
        }
        let languages = self.languages();
        let mut buffer = Buffer::new();
        buffer.set_path(path, &languages);
        // A file that is not there yet is not a file you have visited: it would
        // be listed by `SPC RET` and fail to open when chosen. Emacs draws the
        // line in the same place -- `recentf-keep` defaults to keeping what is
        // readable, so `find-file` on a new name records nothing until it is
        // written (#181).
        let visited = matches!(contents, Opened::Read(_));
        let new = matches!(contents, Opened::Missing);
        match contents {
            Opened::Read(text) => buffer.loaded(Document::from_text(&text)),
            // An ordinary empty buffer, because the file it visits is one write
            // away from existing. A `Failed` buffer cannot be typed in, which is
            // the one thing a config that has not been written yet is for.
            Opened::Missing => buffer.loaded(Document::from_text("")),
            Opened::Failed(message) => buffer.failed(message),
        }
        self.open(buffer);
        if visited {
            self.touch_recent();
        }
        // Said after the buffer is on screen, and only for the row that asked
        // to be brought here: an empty buffer otherwise reads as an empty
        // config, and the two are different claims about the filesystem.
        if new {
            self.say(&format!("{path} is new."));
        }
        // The read a jump was waiting on has landed.
        if let Some(place) = place {
            self.go_to_place(&place);
        }
        Action::Redraw
    }

    /// What the Echo Area shows while narrowing.
    pub fn narrow_view(&mut self) -> Option<NarrowView> {
        let widget = self.narrow.widget.as_mut()?;
        let selected = widget.selected_index();
        let visible: Vec<(usize, bool)> = widget
            .visible()
            .iter()
            .enumerate()
            .map(|(offset, m)| {
                let start = selected.saturating_sub(meowr_narrow::MAX_ROWS - 1);
                (m.index, start + offset == selected)
            })
            .collect();
        let prompt = match widget.narrowed() {
            Some(source) => format!("{}: ", source.name),
            None => match widget.driver() {
                Driver::Completion => "complete: ".to_string(),
                Driver::Picker => "find: ".to_string(),
                Driver::Reading => self.narrow.prompt.clone(),
            },
        };
        let input = widget.input().to_string();
        let rows = visible
            .into_iter()
            .map(|(index, is_selected)| {
                let candidate = widget.candidate(index).clone();
                NarrowRow {
                    text: candidate.text,
                    annotation: candidate.annotation,
                    matched: widget.positions(index),
                    selected: is_selected,
                }
            })
            .collect();
        // Said by the widget, not into the Echo Area the widget is covering.
        // Stamped like the rows are. Without the `about` check a refusal about
        // one buffer is drawn under another buffer's picker while the fresh
        // question is still in flight -- which is the second of ADR-0009's two
        // checks, and the laundering it records having got wrong once already.
        let narrowed_to = widget.narrowed().map(|source| source.id);
        let subject = self.project_subject();
        let note = match (narrowed_to, self.narrow.project.for_subject(&subject)) {
            (Some(PROJECT), Some(Err(why))) => Some(why.clone()),
            // `consult-recent-file` refuses an empty list with "No recent
            // files" and opens nothing; a picker with three other sources
            // opens anyway, so it says the same on its own line.
            (Some(RECENT), _) if self.recent.is_empty() => {
                Some("No recent files.".to_string())
            }
            _ => self.asking_note(),
        };
        Some(NarrowView {
            rows,
            prompt,
            input,
            note,
        })
    }

    /// What a re-asking picker has to say about itself.
    ///
    /// **Four states, and three of them need saying.** Too little typed to ask,
    /// a question in flight, an answer that found nothing, and an answer with
    /// rows in it — only the last is self-evident. The one that must never be
    /// silent is the second: rows from the *previous* answer stay up while the
    /// next question is out, and with nothing said they read as the answer to
    /// what you have just typed.
    fn asking_note(&self) -> Option<String> {
        let asking = self.narrow.asking.as_ref()?;
        let widget = self.narrow.widget.as_ref()?;
        if self.asking_input().is_none() {
            let least = self.options.ask_min_input;
            return Some(format!("Type {least} characters to search."));
        }
        // In flight: the input has moved since a question went out, or the
        // question that did go out has not been answered yet.
        if asking.due || asking.showing != asking.asked {
            return Some("Asking…".to_string());
        }
        (asking.showing.is_some() && widget.matches().is_empty())
            .then(|| "No matches.".to_string())
    }

    /// What previewing the selected candidate would show.
    ///
    /// Previewing *into the window behind* rather than into a panel is
    /// ADR-0004 — there is nowhere else for a preview to go — and it turns out
    /// to be better: you see the candidate in its real surroundings at its
    /// real width.
    ///
    /// All four picker sources answer. `line` scrolls, `buffers` points the
    /// window at something already loaded, and `recent` and `project` name a
    /// file that has to be read — which is why this describes the preview
    /// rather than performing it. Completion and `meow-visit` do not preview:
    /// one is choosing a word to type and the other a word to search for, and
    /// neither is somewhere to go.
    pub fn preview_target(&self) -> Option<Preview> {
        let widget = self.narrow.widget.as_ref()?;
        if widget.driver() != Driver::Picker || self.narrow.accepts != Accepts::Goto {
            return None;
        }
        let row = widget.matches().get(widget.selected_index())?;
        let chosen = widget.candidate(row.index);
        match chosen.source {
            BUFFERS => self
                .narrow
                .buffers_shown
                .get(row.index)
                .copied()
                .map(Preview::Buffer),
            // An open file is its buffer: what previewing it shows is the text
            // being edited, and accepting it goes to that buffer rather than
            // reading it again (#150, #153).
            RECENT | PROJECT | FILES => Some(
                self.buffer_visiting(&chosen.text)
                    .map_or_else(|| Preview::File(chosen.text.clone()), Preview::Buffer),
            ),
            LINE => chosen
                .annotation
                .as_ref()?
                .parse::<usize>()
                .ok()
                .map(Preview::Line),
            // The row was picked for the line it named, so the head of the file
            // is the one thing worth less than not previewing at all.
            RESULT => self.narrow.places.get(row.index).cloned().map(Preview::At),
            _ => None,
        }
    }

    pub(crate) fn narrow_key(&mut self, key: Key) -> Action {
        let Some(widget) = self.narrow.widget.as_mut() else {
            return Action::Nothing;
        };
        match key {
            Key::Esc | Key::Ctrl('g') => return self.cancel_narrowing(),
            Key::Enter => return self.accept_narrowing(),
            // The arrows alias `C-n` and `C-p` rather than replacing them:
            // consult and vertico take both, and `C-n` is what the author's
            // fingers already know. Until #71 there was no `Key` for an arrow,
            // so the widget offered the chord alone — a workaround that had
            // started to look like a decision.
            Key::Ctrl('n') | Key::Down => widget.select_next(),
            Key::Ctrl('p') | Key::Up => widget.select_previous(),
            Key::Backspace => widget.backspace(),
            // consult's `<`: the next key names a source. Not while reading a
            // line — there are no sources to narrow to, and a path may contain
            // a `<` as readily as it contains a space.
            Key::Char('<') if widget.driver() != Driver::Reading => {
                self.narrow.awaiting_source = true
            }
            Key::Char(c) if self.narrow.awaiting_source => {
                self.narrow.awaiting_source = false;
                if !widget.narrow_to(c) {
                    widget.push('<');
                    widget.push(c);
                } else if widget.narrowed().map(|source| source.id) == Some(PROJECT) {
                    // Asked again, because the subject may have moved since
                    // the picker opened: previewing a candidate makes a
                    // different buffer current, and ADR-0009's whole point is
                    // that the buffer an answer is about can change with no key
                    // pressed. `ensure_project_answer` is a no-op when the
                    // answer already fits. The key is not spelled here either;
                    // the source is asked which it is, because
                    // `picker_candidates` owns the keys and a second place that
                    // wrote `p` would be exactly the drift `open_picker_on`
                    // warns about.
                    return self.ensure_project_answer();
                }
            }
            Key::Char(c) => widget.push(c),
            _ => return Action::Nothing,
        }
        // The input moved, so a question over it is one query behind. Marked
        // rather than asked: `ask-debounce` is what decides when, and a server
        // is not a thing to ask five times a second.
        if let Some(asking) = self.narrow.asking.as_mut() {
            asking.due = true;
        }
        Action::Redraw
    }
}
