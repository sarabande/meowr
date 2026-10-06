//! A buffer: text, and everything that belongs to *that* text.
//!
//! Decision #5 settled where this state lives, and it corrects Meow while
//! doing so. In Emacs `meow--selection` and `meow--current-state` are
//! `defvar-local` — per buffer — but `meow--selection-history` and
//! `meow--expand-nav-function` are plain `defvar`s, so the selection history
//! survives a buffer switch and `z` can pop a selection belonging to a file you
//! are no longer looking at. That is an oversight rather than a design, and
//! everything here is per buffer.
//!
//! The kill ring is the deliberate exception: it is global in Emacs *on
//! purpose*, because copying in one buffer and yanking in another is the point
//! of it. It stays on the session.

use meowr_core::{Selection, SelectionState, undo::History};
use meowr_domain::{Diagnostic, Document, Folds, Place};
use meowr_syntax::{InputEdit, Language, Reparse, Syntax};

/// What a buffer is showing, which is not always text: the file is read off
/// the event loop, so "not yet" is a state the UI must be able to draw.
///
/// **Not `Clone`.** It was, and nothing used it; the derive went out with the
/// Guest variant that could not satisfy it, and is not worth bringing back
/// while nothing asks. `Buffer` is not `Clone` either, so nothing is lost.
#[derive(Debug)]
pub enum Content {
    Loading,
    Ready(Document),
    Failed(String),
}

/// Which buffer, in the session's list.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct BufferId(pub usize);

/// Whether a Buffer may be edited, and whether that is a preference.
///
/// Three states rather than a `bool` and a second `bool`, which between them
/// can spell a fourth that means nothing — editable, but structurally
/// read-only. The same argument [`Recovery`](crate::Recovery) already makes.
///
/// The distinction exists because `toggle-read-only` needs it. Emacs has no
/// equivalent — it has `buffer-read-only` and `inhibit-read-only`, and leans
/// on major modes to keep the two apart — so this is meowr's own, and it is
/// the line #50 drew when it separated a **Buffer property** from an
/// **Option**.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadOnly {
    /// It may be edited.
    No,
    /// Read-only because someone asked for it. A preference, so
    /// `toggle-read-only` takes it back.
    Asked,
    /// Read-only because of what this Buffer *is*: the narrowing preview
    /// holds a file nobody chose to open, and `*config-error*` holds a
    /// report. Not a preference, so not a thing to toggle.
    Always,
}

impl ReadOnly {
    /// Whether editing is refused, which is all most callers want to know.
    pub fn refuses_edits(self) -> bool {
        !matches!(self, ReadOnly::No)
    }
}

/// A list of [`Place`]s living in a Buffer rather than in the picker.
///
/// ADR-0011's second home for many results, as the generated buffer holding one
/// knows it: which of its lines names where, and where the list was made from.
/// Today that is `*diagnostics*`, and tomorrow whatever else the ADR sends to a
/// buffer rather than to the picker.
///
/// **One value rather than two fields**, for the reason the diagnostics
/// themselves are an `Option`: a buffer either is a listing or is not, and two
/// fields can disagree about which.
///
/// It lives on the Buffer rather than in a keymap because meowr has no
/// buffer-local keymaps (ADR-0011). `visit-place` is one global command that
/// means something where there are Places and is inert everywhere else, which
/// is exactly what `quit-window` already is.
pub struct Listing {
    /// What each line names, indexed by line, `None` where it names nowhere --
    /// the heading and the blank line under it.
    ///
    /// Indexed rather than a list of Places and an offset to add, because the
    /// offset is arithmetic in two places that has to agree, and the day the
    /// heading grows a line it would not.
    places: Vec<Option<Place>>,
    /// Where the point was when the list was made.
    ///
    /// The generated buffer's half of what the picker keeps in
    /// `Narrowing::from`. Why either exists is `Session::leaving_from`, which
    /// is the one reader of both.
    from: Option<Place>,
}

impl Listing {
    pub fn new(places: Vec<Option<Place>>, from: Option<Place>) -> Self {
        Self { places, from }
    }

    /// Where the given line points, if it points anywhere.
    pub fn place_on(&self, line: usize) -> Option<&Place> {
        self.places.get(line)?.as_ref()
    }

    /// Where this list was made from. See the field.
    pub fn from(&self) -> Option<&Place> {
        self.from.as_ref()
    }

    /// Whether any line of it names anywhere at all.
    pub fn names_anywhere(&self) -> bool {
        self.places.iter().any(Option::is_some)
    }
}

pub struct Buffer {
    pub(crate) content: Content,
    pub(crate) path: Option<String>,
    pub(crate) selection: SelectionState,
    pub(crate) history: History,
    pub(crate) read_only: ReadOnly,
    pub(crate) language: Option<Language>,
    pub(crate) syntax: Option<Syntax>,
    pub(crate) pending_edits: Vec<InputEdit>,
    pub(crate) revision: u64,
    pub(crate) parsing: bool,
    /// Where the digits would expand to, painted until they time out.
    pub(crate) hints: Vec<usize>,
    /// The Grab: a genuinely independent second selection, not a saved copy of
    /// the first. It stays where it is while the primary selection moves, and
    /// it moves with the text when an edit happens before it — Emacs keeps it
    /// as an overlay, which is markers, which is what `after_edit` is.
    pub(crate) grab: Option<std::ops::Range<usize>>,
    /// What a language server said about this buffer's text.
    ///
    /// Beside `grab` rather than folded into the syntax spans, which is #111's
    /// decision and its reason: a `Styled` run is recomputed from the current
    /// tree every frame, and a diagnostic is not a fact about the tree. It is
    /// something the session was *told* and goes on believing until told
    /// otherwise — which is what a Grab already is, and why it shifts with
    /// edits and goes stale by revision exactly as one does.
    ///
    /// `None` until a server has said something, which is not the same as one
    /// saying there is nothing: a server that has not answered has not produced
    /// an empty list, and an empty `*diagnostics*` claiming "none" where the
    /// truth is "nobody asked" is ADR-0010's silent fallback in a new place.
    /// The two cases are different *answers*, so they are different values
    /// rather than one value and a flag beside it to disagree with.
    pub(crate) diagnostics: Option<Vec<Diagnostic>>,
    /// The regions of this buffer collapsed to their first lines.
    ///
    /// A **Buffer property** and not a Window one (ADR-0012), which is the
    /// decision that ADR reversed itself on: `hs-minor-mode` and `treesit-fold`
    /// are both buffer-local, so a buffer in two Windows folds the same in
    /// both. The cost is real and accepted there — a split exists to see two
    /// parts of one file at once, and a fold in one collapses the other.
    pub(crate) folds: Folds,
    /// The list of [`Place`]s this buffer *is*, where it is one.
    ///
    /// `None` in every buffer holding a file. See [`Listing`].
    pub(crate) listing: Option<Listing>,
    /// How many edits have been made since this buffer was last auto-saved.
    ///
    /// Emacs counts them too — `auto-save-interval` is a number of changes,
    /// not of seconds — because time alone is the wrong measure: a buffer
    /// nobody is typing into does not need writing out every thirty seconds,
    /// and one being typed into hard does.
    pub(crate) since_autosave: usize,
    /// Opened only to be looked at, by the narrowing widget's preview.
    ///
    /// It is not offered as a buffer to switch to, because it is not one yet:
    /// the choice that would make it real has not been made. Accepting the
    /// candidate clears this and the buffer becomes the file you opened;
    /// cancelling leaves it for the next preview to reuse.
    pub(crate) preview: bool,
    /// Killed, and kept only so the slot stays where it is.
    ///
    /// [`BufferId`] is an index into the session's list and windows hold them,
    /// so taking a buffer *out* of that list would renumber every buffer after
    /// it and leave every window pointing at the wrong file. The slot stays and
    /// the buffer in it is replaced by an empty one wearing this flag: the rope
    /// is dropped, the index is not.
    ///
    /// Slots are therefore never reused and a long session accumulates them —
    /// deliberately. An empty `Buffer` is a few hundred bytes with the text
    /// gone, and handing a killed slot to the next `open` would make one
    /// `BufferId` mean two different files in one session, which is the bug
    /// this flag exists to prevent.
    pub(crate) killed: bool,
    /// This buffer was given its name and has not been written under it, so a
    /// save should look before it writes over somebody else's file.
    ///
    /// On the Buffer and not the Session, which held it first: one slot for a
    /// per-buffer fact meant naming a second buffer cleared the first's, and
    /// the first then wrote over an existing file without a word.
    pub(crate) fresh_name: bool,
    /// What to call a buffer that visits no file.
    ///
    /// `*scratch*` is the default and needs no label; `*config-error*` is a
    /// buffer with a job and a name for it. Not a path, because it is not one:
    /// nothing on disk corresponds, and `save_target` must go on refusing it.
    pub(crate) label: Option<String>,
    /// The Project whose scratch buffer this is (#180).
    ///
    /// The root rather than the name, because the name is what it is *called*
    /// and the root is what it *is*: two repositories can end in the same
    /// directory name, and a buffer found by name would then be the wrong
    /// Project's. It is also the only way a Project's scratch can name its own
    /// Project once you are sitting in it -- it has no path, so nothing else
    /// about it says where it belongs.
    ///
    /// The one global `*scratch*` has none. It is still recognised by having no
    /// path and no label at all, which is what a session started with no file
    /// argument is already sitting in.
    pub(crate) scratch_of: Option<String>,
}

impl Default for Buffer {
    fn default() -> Self {
        Self::new()
    }
}

impl Buffer {
    pub fn new() -> Self {
        Self {
            content: Content::Loading,
            path: None,
            selection: SelectionState::new(Selection::at(0)),
            history: History::new(),
            read_only: ReadOnly::No,
            language: None,
            syntax: None,
            pending_edits: Vec::new(),
            revision: 0,
            parsing: false,
            hints: Vec::new(),
            grab: None,
            diagnostics: None,
            folds: Folds::default(),
            listing: None,
            since_autosave: 0,
            preview: false,
            killed: false,
            fresh_name: false,
            label: None,
            scratch_of: None,
        }
    }

    /// What is left where a killed buffer was. See the `killed` field.
    pub(crate) fn tombstone() -> Self {
        Self {
            content: Content::Ready(Document::default()),
            killed: true,
            ..Self::new()
        }
    }

    /// Whether this buffer has been killed. A killed slot is not a buffer: it
    /// is not offered, not cycled to, and holds no text.
    pub fn is_killed(&self) -> bool {
        self.killed
    }

    /// Whether this is a buffer someone opened.
    ///
    /// Two kinds of slot are not: the one previewing reads into, which nobody
    /// has chosen yet, and a killed one, which nobody has any more. Everywhere
    /// that offers a buffer or cycles to one asks this rather than testing the
    /// two flags, so a third kind of slot is one edit instead of a sweep.
    /// Whether meowr wrote this buffer rather than read it.
    ///
    /// `ReadOnly::Always` is the mark and a good one: it is set by
    /// construction and `toggle-read-only` refuses to clear it, so a buffer
    /// somebody is editing cannot wander into the category. Named rather than
    /// asked as a flag comparison at each call site, because what those sites
    /// mean is "meowr wrote this" and what they were saying was "this happens
    /// to be read-only for good" — so a fourth `Always` buffer would have
    /// joined them without anybody deciding it should.
    pub fn meowr_wrote(&self) -> bool {
        self.read_only == ReadOnly::Always
    }

    pub fn is_real(&self) -> bool {
        !self.preview && !self.killed
    }

    /// Whether this buffer exists only to be previewed. See the field.
    pub fn is_preview(&self) -> bool {
        self.preview
    }

    /// A scratch buffer: ready, empty, and belonging to no file.
    pub fn scratch() -> Self {
        Self {
            content: Content::Ready(Document::default()),
            ..Self::new()
        }
    }

    /// Whether this buffer is a scratch buffer: ready, visiting no file, and
    /// wearing no label of its own.
    ///
    /// Not the same question [`Buffer::name`] answers, and the gap is the
    /// point. `name` reads two of these — no path and no label — and a
    /// tombstone has both, so `name` calls a killed slot `*scratch*` and this
    /// does not. Nothing shows that name (`is_real` hides tombstones from the
    /// picker, the modeline and the cycle), but a predicate that agreed with
    /// `name` here would hand `SPC b x` a killed slot to switch to.
    ///
    /// `Loading` is deliberately not scratch either. A buffer still reading a
    /// file has no path *yet* rather than none at all, and `SPC b x` arriving
    /// during the read would otherwise hand it over as the scratch buffer and
    /// then watch somebody else's file land in it.
    ///
    /// Whether it is *the* scratch buffer is not a question a Buffer can
    /// answer — uniqueness is a fact about the session's list, which is why
    /// `Session::scratch_buffer` takes the first match rather than asserting
    /// there is only one.
    ///
    /// A Project's scratch is deliberately **not** one of these (#180). It has
    /// a name, so it fails the label test, and that is the answer rather than
    /// an oversight: this is the question `SPC f X` asks, and a row that means
    /// "the scratch buffer" must not be handed some repository's.
    pub fn is_the_scratch_buffer(&self) -> bool {
        self.is_real() && self.document().is_some() && self.path.is_none() && self.label.is_none()
    }

    /// The root of the Project whose scratch this is, if it is one (#180).
    ///
    /// With [`Buffer::is_the_scratch_buffer`] this is the whole of what #174
    /// asked the type to say: which kind of scratch a buffer is, or neither.
    /// There was a third predicate over the union of the two and nothing in
    /// the editor ever wanted it -- every row means one kind or the other, and
    /// a short name for "either" is one a row reaches for by mistake.
    pub fn scratch_of(&self) -> Option<&str> {
        self.scratch_of.as_deref()
    }

    pub fn document(&self) -> Option<&Document> {
        match &self.content {
            Content::Ready(doc) => Some(doc),
            _ => None,
        }
    }

    pub fn path(&self) -> Option<&str> {
        self.path.as_deref()
    }

    /// The name a modeline or a picker shows.
    ///
    /// The last path segment, because that is what identifies a file at a
    /// glance; disambiguating two buffers with the same basename needs the
    /// picker, and arrives with it.
    pub fn name(&self) -> &str {
        match (&self.path, &self.label) {
            (Some(path), _) => crate::basename(path),
            (None, Some(label)) => label,
            (None, None) => "*scratch*",
        }
    }

    /// Whether the buffer differs from what was last written.
    ///
    /// Asked of the history rather than kept as a flag. It was a flag, and a
    /// one-way one: every mutating command set it and only saving cleared it —
    /// so undoing an edit marked the buffer modified exactly as making it had,
    /// and `SPC q q` then sent you to `SPC q Q` with nothing at risk.
    ///
    /// The history is where the answer lives because the question is about a
    /// *place in the timeline*, which is also why redoing past the save point
    /// sets it again. A flag cannot do that at all.
    pub fn is_modified(&self) -> bool {
        self.history.is_modified()
    }

    pub fn is_loading(&self) -> bool {
        matches!(self.content, Content::Loading)
    }

    pub fn error(&self) -> Option<&str> {
        match &self.content {
            Content::Failed(message) => Some(message),
            _ => None,
        }
    }

    pub fn selection(&self) -> Selection {
        self.selection.current()
    }

    pub fn set_path(&mut self, path: impl Into<String>, languages: &meowr_syntax::Languages) {
        let path = path.into();
        self.language = languages.for_path(&path);
        self.path = Some(path);
        // A buffer with a file is named by its file, and is no longer a
        // scratch of either kind (#180). A Project's scratch saved somewhere
        // would otherwise still be what `SPC p X` found for that Project --
        // and a scratch, by its own definition, visits no file. Doom renames
        // the buffer out of `*doom:scratch (%s)*` on `write-file` for the same
        // reason. The global one needs no help: it is recognised by having no
        // path at all.
        self.label = None;
        self.scratch_of = None;
    }

    pub fn loaded(&mut self, doc: Document) {
        self.content = Content::Ready(doc);
        // The text and the Listing are one fact, so replacing half of it leaves
        // the other half naming lines that have gone. The caller that has both
        // sets it back immediately; nobody else has one at all. A fold is the
        // same kind of claim about the same text -- it rides an *edit*, which
        // it is told about, and cannot ride a whole new document, which it is
        // not.
        self.listing = None;
        self.folds.clear();
    }

    /// Say what this buffer's lines name, or that they name nothing.
    pub(crate) fn set_listing(&mut self, listing: Option<Listing>) {
        self.listing = listing;
    }

    /// What this buffer has folded. See the field.
    pub fn folds(&self) -> &Folds {
        &self.folds
    }

    /// The list this buffer is, if it is one. See [`Listing`].
    pub fn listing(&self) -> Option<&Listing> {
        self.listing.as_ref()
    }

    pub fn failed(&mut self, message: String) {
        self.content = Content::Failed(message);
    }

    /// Take the next parse to run on a worker, if one is due.
    pub fn take_reparse(&mut self) -> Option<Reparse> {
        let language = self.language?;
        if self.parsing || (self.syntax.is_some() && self.pending_edits.is_empty()) {
            return None;
        }
        let text = self.document()?.text().clone();
        self.parsing = true;
        Some(Reparse {
            text,
            language,
            syntax: self.syntax.take(),
            edits: std::mem::take(&mut self.pending_edits),
            revision: self.revision,
        })
    }

    /// Why this buffer would not take a Foreign edit computed against
    /// `revision`, before any replacement is looked at -- asked of every buffer
    /// a workspace edit reaches before it is applied to any (#167).
    pub(crate) fn foreign_refusal(&self, revision: u64) -> Option<ForeignRefusal> {
        if self.read_only.refuses_edits() {
            Some(ForeignRefusal::ReadOnly)
        } else if revision != self.revision {
            Some(ForeignRefusal::Moved)
        } else if !matches!(self.content, Content::Ready(_)) {
            Some(ForeignRefusal::Unread)
        } else {
            None
        }
    }

    /// Apply a **Foreign edit** computed against `revision` (#161, ADR-0013).
    ///
    /// The same shape as `apply_reparse` below, and for the same reason: an
    /// answer computed against one revision means nothing in another. Refused
    /// whole by a buffer that refuses edits -- which every generated buffer
    /// does -- and dropped whole if the revision has moved. `Ok(false)` for a
    /// set with nothing in it, which is not an edit and so moves nothing.
    ///
    /// Otherwise one undo step, then what any edit gets. A fold whose edge a
    /// replacement crosses opens first, while the replacements and the folds
    /// still address the same text; `after_command` then shifts the rest.
    pub(crate) fn apply_foreign(
        &mut self,
        revision: u64,
        replacements: &[meowr_domain::Replacement],
    ) -> Result<bool, ForeignRefusal> {
        if let Some(why) = self.foreign_refusal(revision) {
            return Err(why);
        }
        let Content::Ready(doc) = &mut self.content else {
            return Err(ForeignRefusal::Unread);
        };
        if replacements.is_empty() {
            return Ok(false);
        }
        self.history
            .replace_all(doc, &mut self.selection, replacements)
            .map_err(ForeignRefusal::Unapplicable)?;
        let ranges: Vec<_> = replacements.iter().map(|r| r.range.clone()).collect();
        self.folds.open_crossed(&ranges);
        self.after_changes(10, Inserted::Before);
        self.since_autosave += 1;
        Ok(true)
    }

    /// Take a finished parse back. A stale one is dropped: the buffer moved
    /// on, and the tree no longer describes the text.
    pub fn apply_reparse(&mut self, revision: u64, syntax: Option<Syntax>) -> bool {
        self.parsing = false;
        if revision != self.revision {
            return false;
        }
        self.syntax = syntax;
        true
    }

    /// Hand every recorded change to the syntax tree as an edit it can reuse
    /// its work across, and recompute the expand hints.
    pub fn grab(&self) -> Option<std::ops::Range<usize>> {
        self.grab.clone()
    }

    /// The grammar this buffer's text is in, if meowr knows one for it.
    ///
    /// `None` is an ordinary answer, not an error: a buffer with no grammar is
    /// plain text. Exposed because a language server is chosen per Language and
    /// the host is the side that starts one.
    pub fn language(&self) -> Option<Language> {
        self.language
    }

    /// Which edit of this buffer's text the current state describes.
    ///
    /// What tags an answer computed off the event loop, so one that arrives
    /// after the buffer has moved on can be refused rather than misplaced —
    /// `take_reparse`/`apply_reparse` run on it, and diagnostics do too.
    pub fn revision(&self) -> u64 {
        self.revision
    }

    /// What a language server said about this buffer. See the field.
    ///
    /// Lenient, because every reader but one wants the list and has nothing to
    /// do with the distinction: a squiggle is drawn for each of them and there
    /// are none either way.
    pub fn diagnostics(&self) -> &[Diagnostic] {
        self.diagnostics.as_deref().unwrap_or_default()
    }

    /// What a language server said, or `None` if none has said anything.
    ///
    /// The one reader that needs the distinction is `SPC k x`, which opens a
    /// buffer saying "No diagnostics." for the first and refuses into the Echo
    /// Area for the second (ADR-0011).
    pub fn heard_diagnostics(&self) -> Option<&[Diagnostic]> {
        self.diagnostics.as_deref()
    }

    /// Take a set of diagnostics, or refuse a stale one.
    ///
    /// `apply_reparse`'s own guard, for the same reason: an answer computed
    /// against text the buffer has since moved past would put a squiggle under
    /// something it says nothing about. Real LSP hands us the tag for free —
    /// `publishDiagnostics` carries a `version` field for exactly this.
    pub fn set_diagnostics(&mut self, revision: u64, diagnostics: Vec<Diagnostic>) -> bool {
        if revision != self.revision {
            return false;
        }
        self.diagnostics = Some(diagnostics);
        true
    }

    pub(crate) fn after_command(&mut self, hint_count: usize) {
        self.after_changes(hint_count, Inserted::Inside);
    }

    /// What a command's edits, or a foreign edit's, mean for everything else
    /// this buffer holds by position. See [`Inserted`].
    fn after_changes(&mut self, hint_count: usize, inserted: Inserted) {
        let changes = self.history.take_changes();
        if !changes.is_empty() {
            self.revision += 1;
            if let Content::Ready(doc) = &self.content {
                self.pending_edits
                    .extend(tree_sitter_edits(doc.text(), &changes));
            }
            // The Grab moves with the text, exactly as the selection does. An
            // overlay in Emacs survives an edit elsewhere in the buffer, and a
            // Grab that did not would be a saved copy rather than a second
            // selection. A diagnostic's range wants the identical treatment
            // and for the identical reason, so both read off one definition:
            // two copies of this arithmetic is how the two come to disagree.
            if let Some(grab) = self.grab.take() {
                self.grab = Some(shifted(grab, &changes, inserted));
            }
            for diagnostic in self.diagnostics.iter_mut().flatten() {
                diagnostic.range = shifted(diagnostic.range.clone(), &changes, inserted);
            }
            // And a fold, which is the third span of text this buffer was told
            // about and goes on believing in. One definition, handed to each.
            self.folds.shift_each(|region| shifted(region, &changes, inserted));
        }
        if let Content::Ready(doc) = &self.content {
            self.hints = meowr_core::hint_positions(doc, &self.selection, hint_count);
        }
    }
}

/// Why a **Foreign edit** was not applied to a file (ADR-0013). The Session
/// says it, naming the file and the source, because only the Session knows
/// either.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ForeignRefusal {
    ReadOnly,
    /// The revision moved since the edit was computed.
    Moved,
    Unread,
    Unapplicable(meowr_core::undo::ReplaceError),
    /// A line the edit names is not in the text (#167).
    PastEnd(meowr_domain::PastEnd),
    /// A workspace edit names the file more than once (#167).
    NamedTwice,
}

/// Where text inserted exactly at a range's start belongs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Inserted {
    /// Inside the range, as with typing: an Emacs overlay's start does not
    /// advance, so text typed at the start of the Grab is grabbed text.
    Inside,
    /// Before it, as with a **Foreign edit**: a formatter adding a line above
    /// an unindented one inserts at that line's first byte, and a range starting
    /// there is about the code, not the gap (#162). The end never advances, so a
    /// line added just after a fold does not join it.
    Before,
}

/// One range, moved through every change in a batch.
///
/// A position at or before an edit stays where it is; one at or after its
/// removed span moves by the difference; one *inside* it collapses to the
/// edit's own offset, because the text it described is no longer there. A
/// start exactly at an insertion moves past it when `inserted` says so.
fn shifted(
    range: std::ops::Range<usize>,
    changes: &[meowr_core::undo::Change],
    inserted: Inserted,
) -> std::ops::Range<usize> {
    changes.iter().fold(range, |range, change| {
        let removed = change.removed.len();
        let added = change.inserted.len();
        let shift = |offset: usize| {
            if offset <= change.at {
                offset
            } else if offset >= change.at + removed {
                offset - removed + added
            } else {
                change.at
            }
        };
        let start = match inserted {
            Inserted::Before if removed == 0 && range.start == change.at => range.start + added,
            _ => shift(range.start),
        };
        start..shift(range.end).max(start)
    })
}

/// The batch of edits from one command, expressed as tree-sitter needs them:
/// each one a diff against the text as it stood *before the whole batch*,
/// sorted so the earliest position comes first.
///
/// A command that edits once — ordinary typing — needs none of this: the
/// buffer's current text already *is* the only "before" there is. A Beacon
/// command edits several places in one command, back-to-front by position
/// so each edit's own offset stays valid without adjustment (`beacon.rs`'s
/// own comment says so, twice). That is exactly the wrong order to hand
/// [`meowr_syntax::reparse`]: `tree_house::Syntax::update`'s own comment says
/// the reverse — it wants edits ascending by their position in the
/// *pre-batch* text, applied highest first so none of them disturbs a
/// position still to come. Fed the batch's own recording order instead, a
/// multi-edit command — Beacon chief among them — hands the parser
/// internally inconsistent byte and point offsets, which is what actually
/// segfaulted a background reparse thread on real Markdown content rather
/// than merely mis-colouring it: tree-sitter's incremental edit trusts
/// what it is given.
///
/// Recovering each edit's pre-batch position without replaying the whole
/// rope leans on one fact about Beacon: its edits never overlap — they are
/// distinct cursors — and two edits that never overlap keep the same
/// left-to-right order in *every* coordinate system either of them is ever
/// recorded in. That is what makes "is this earlier change before or after
/// the one I'm resolving now" answerable by comparing live offsets instead
/// of needing the original ones already.
fn tree_sitter_edits(
    text: &meowr_domain::Rope,
    changes: &[meowr_core::undo::Change],
) -> Vec<InputEdit> {
    // The text before this batch, found by undoing each change in the
    // reverse of the order it was made — last one first — starting from the
    // buffer's current, fully-edited state. By the time a given change's
    // turn comes to be undone, everything recorded after it already has
    // been, so its own `at` is exactly as valid here as it was when the
    // change first happened.
    let mut original = text.clone();
    for change in changes.iter().rev() {
        let start = original.byte_to_char(change.at);
        let end = original.byte_to_char(change.at + change.inserted.len());
        original.remove(start..end);
        original.insert(start, &change.removed);
    }

    // Each change's `at` is a byte offset valid only in the buffer as it
    // stood at the moment of that one edit — after every earlier change in
    // this batch, before every later one. An already-resolved change sits
    // before this one exactly when its own *current* position does; that
    // comparison is what non-overlap keeps meaningful, and it is what lets
    // each change's pre-batch position be recovered one at a time rather
    // than by replaying the rope a second time forward.
    struct Resolved {
        current_at: usize,
        delta: isize,
    }
    let mut resolved: Vec<Resolved> = Vec::with_capacity(changes.len());
    let mut placed: Vec<(usize, &meowr_core::undo::Change)> = Vec::with_capacity(changes.len());
    for change in changes {
        let shift: isize = resolved
            .iter()
            .filter(|r| r.current_at <= change.at)
            .map(|r| r.delta)
            .sum();
        let original_at = (change.at as isize - shift) as usize;
        let delta = change.inserted.len() as isize - change.removed.len() as isize;
        for r in &mut resolved {
            if r.current_at >= change.at + change.removed.len() {
                r.current_at = (r.current_at as isize + delta) as usize;
            }
        }
        resolved.push(Resolved {
            current_at: change.at,
            delta,
        });
        placed.push((original_at, change));
    }
    placed.sort_by_key(|(at, _)| *at);

    placed
        .into_iter()
        .map(|(original_at, change)| {
            meowr_syntax::edit(
                original.slice(..),
                original_at,
                &change.removed,
                &change.inserted,
            )
        })
        .collect()
}
