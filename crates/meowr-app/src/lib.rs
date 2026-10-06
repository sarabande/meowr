//! Session state and event handling for meowr.
//!
//! The session owns what is being edited and dispatches keys through the
//! command registry. It knows nothing about terminals: keys arrive as
//! [`meowr_command::Key`] values, so the whole state machine is testable
//! without a tty.

use meowr_command::{
    Argument, Command, Ctx, Fallbacks, Handler, HookPoint, Hooks, Key, Keymap, Keys, OptionValue,
    Options, Outcome, Proceed, Registry, Resolution, State,
};
pub mod autosave;
pub mod beacon;
pub mod buffer;
pub mod declare;
pub mod grab;
pub mod keypad;
pub mod leader;
pub mod narrow;
pub mod window;

pub use autosave::{Autosave, Recovery, Snapshot, autosave_path};
use beacon::BEACON_NOOPS;
use buffer::{Buffer, BufferId, Content, ForeignRefusal, Listing, ReadOnly};
use meowr_core::{
    self as core, Axis, Kind, Selection,
    edit::{KillRing, Trail},
    thing::{Extent, Things},
};
use meowr_domain::{Document, Pattern, Place};
use meowr_narrow::{Candidate, Driver, Narrow, Source, SourceId};
use meowr_syntax::{Languages, Reparse, Span, Syntax};
use meowr_view::modeline::{Fitted, Status, fit, modeline};
use meowr_view::{
    Hint, NarrowRow, NarrowView, Row, Styled, Viewport, gutter_width, hints as view_hints,
    rows_with, styled,
};
use narrow::Preview;
use std::ops::Range;
use std::sync::Arc;
use window::{Area, Direction, Tree, WindowId, text_area};

/// What the event loop should do next.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Nothing,
    Redraw,
    Quit,
    /// Start again: `exec` on this binary with the arguments it was given,
    /// once the terminal has been put back (#182).
    ///
    /// A quit that comes back, and dealt with as one: the auto-saves go and
    /// the recent files are written, because the process that wrote them is
    /// the process that is ending. Nothing else is restored -- not the window
    /// layout, not the file on screen -- which is `doom/restart`'s own answer;
    /// restoring a session is `SPC q l`'s question and is not on this map.
    ///
    /// The terminal has to be put back *before* the `exec`, or the new process
    /// inherits a screen it did not draw. That is why this is an `Action` the
    /// loop leaves on rather than something the session could do: only the
    /// binary owns the terminal, and only it can hand it back first.
    Restart,
    /// Write the buffer out. The session does no I/O of its own — the binary
    /// owns the filesystem, as it owns the terminal.
    ///
    /// Which file, and what goes in it, is [`Session::save_target`]: a signal
    /// carries no payload, so the host asks.
    Save,
    /// Read a file the session has decided to open. [`Session::pending_open`]
    /// names it and [`Session::opened`] takes the text back, the same shape
    /// [`Session::preview_target`] and [`Session::previewed`] already use.
    Open,
    /// Ask a language server what is at the point.
    ///
    /// [`Session::pending_hover`] names where, and [`Session::show_hover`]
    /// takes the answer back — the same shape `Open`/`pending_open`/`opened`
    /// already has, and for the same reason: talking to a subprocess is I/O
    /// and the session does none.
    Hover,
    /// Ask a language server for a **list** of places: references, or symbols.
    ///
    /// [`Session::pending_lookup`] names what was asked and [`Session::found`]
    /// takes the answer back -- the fourth instance of the shape `Open`,
    /// `Hover` and `Project` already have, so ADR-0009 binds it: the answer is
    /// re-checked when it lands, and never cancelled in flight.
    Lookup,
    /// Search a directory for a pattern.
    ///
    /// [`Session::pending_search`] names what and where, and
    /// [`Session::searched`] takes the answer back -- the fifth instance of the
    /// shape `Open`, `Hover`, `Lookup` and `Project` already have. **Which**
    /// directory is the host's to work out, because for two of the four rows it
    /// is the Project, and ADR-0010 puts that question and its refusal on the
    /// side that knows where it looked.
    Search,
    /// Ask git which repository the buffer's file is in, and what is tracked in
    /// it.
    ///
    /// [`Session::pending_project`] names which buffer and
    /// [`Session::set_project`] takes the answer back -- the shape `Open` and
    /// `Hover` already have, and for the same reason: a subprocess is I/O and
    /// the session does none. Asked lazily -- when a picker opens, rather than
    /// for every buffer opened (#130).
    Project,
    /// List every file under the buffer's directory.
    ///
    /// [`Session::pending_files`] names which buffer and
    /// [`Session::files_listed`] takes the answer back. A question for the host
    /// because listing a directory is I/O and the session does none. Not
    /// `Project` with a different directory: that answer *is* the picker's
    /// project source, and a listing of somewhere else arriving there is #120's
    /// bug (#149).
    Files,
    /// Ask where the buffer's Project is, for a row that needs paths relative
    /// to it.
    ///
    /// [`Session::pending_from_root`] names the question and
    /// [`Session::path_from_root`] or [`Session::recent_from_root`] takes the answer
    /// back. The session spells paths against a working directory it never
    /// sees, so "relative to the root" is arithmetic only the host can do
    /// (#148).
    FromRoot,
    /// Run a formatter over a buffer's text (ADR-0014).
    ///
    /// [`Session::pending_format`] names what to run and on what, and
    /// [`Session::formatted`] takes the outcome back. A subprocess is I/O and
    /// the session does none.
    Format,
    /// Write several buffers (#166).
    ///
    /// [`Session::pending_saves`] names them and [`Session::saved_some`] takes
    /// back how each write went, in one call, so one sentence can say it.
    SaveSome,
    /// Ask a language server to rename the symbol at the point (#168).
    ///
    /// Twice for one rename: first whether it can be renamed and what it is
    /// called, then, with the new name read, the rename itself.
    /// [`Session::pending_rename`] says which, [`Session::rename_prepared`] and
    /// [`Session::renamed`] take the answers back.
    Rename,
    /// Ask a language server for the code actions at the point, or apply the
    /// one chosen (#169). [`Session::pending_code_actions`] says which.
    CodeActions,
    /// Copy or move this file (#178). Not `Files` above, which lists a
    /// directory: this one writes.
    ///
    /// [`Session::pending_file_work`] says which file and where, and
    /// [`Session::file_done`] takes back what happened. The filesystem is the
    /// host's: the session names the work and says what it came to.
    FileWorked,
    /// Delete this buffer's file (#179). Its own action rather than a third
    /// [`FileWork`]: there is no target to resolve and nothing to read, so the
    /// only thing it shares with copying and moving is which layer does it.
    ///
    /// [`Session::pending_file_deletion`] says which file, and
    /// [`Session::file_deleted`] takes back whether it went.
    FileDeleted,
}

/// Who asked for a format, which decides what is said about it (ADR-0014).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FormatOrigin {
    /// `SPC k f`: a request, so every outcome is answered -- "already
    /// formatted" included, and "not installed" every time.
    Key,
    /// A save with `format-on-save` on: nobody asked in words, so what changed
    /// nothing says nothing, and a missing program is said once per Language.
    Save,
}

/// A formatter to run over one buffer's text, and what it was asked about
/// (#164, ADR-0014).
///
/// The text goes on stdin; the revision is ADR-0013's stamp, so an answer for
/// text that has since changed is dropped rather than applied.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Format {
    pub path: String,
    pub revision: u64,
    /// The Language's name, for what is said when nothing could be done.
    pub language: String,
    pub invocation: meowr_command::Invocation,
    pub text: String,
    pub origin: FormatOrigin,
}

/// What running a formatter came to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Formatted {
    /// It succeeded, and this is the whole new text.
    Output(String),
    /// It exited unsuccessfully; the first line it wrote to stderr, if any.
    Failed(String),
    /// The program could not be found.
    Missing,
}

/// What reading a file the session asked for came to (#181).
///
/// Three-way rather than a `Result`, because *absent* is neither of the other
/// two and the difference decides what the user is handed. `config.rs` has
/// drawn this same line since #44 -- [`crate::Session::opened`]'s caller reads
/// files the way `config::evaluate` does, and `Failed::Unreadable` is
/// documented there as "distinct from absent". A config that is not there yet
/// is the ordinary state of a fresh checkout; one that exists and will not open
/// is a failure, and offering an empty buffer for it is a lie you could then
/// write over the top of.
///
/// `Missing` arrives only for a read the session said could be new
/// ([`crate::Session::pending_open_may_be_new`]). Every other row's gone file
/// is `Failed`, carrying the host's own sentence about it, exactly as before.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Opened {
    /// It was read, and this is what was in it.
    Read(String),
    /// It would not open, and this says why.
    Failed(String),
    /// There is nothing there, and the row that asked for it knew that could be
    /// so: the buffer visits the path anyway and writing it makes the file.
    Missing,
}

/// A **Foreign edit** for the buffer visiting `path`: replacements that came
/// from somewhere other than the keyboard, and the revision they were computed
/// against (#161, ADR-0013).
///
/// `from` names the source for what is said when it cannot be applied --
/// `rustfmt`, `rust-analyzer` -- because "an edit was not applied" with no
/// subject is a sentence nobody can act on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForeignEdit {
    pub path: String,
    /// The buffer's revision when the question that produced this went out.
    pub revision: u64,
    pub from: String,
    pub replacements: Vec<meowr_domain::Replacement>,
}

/// The revision every open buffer had when a question went out, for the
/// workspace edit that answers it (#167, ADR-0013).
///
/// A snapshot of all of them, because the question cannot know which files the
/// answer will reach -- a rename names them only when it comes back.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
///
/// Each stamp says which buffer as well as which revision: a buffer killed and
/// reopened counts its revisions from the start again, so a number alone could
/// match text the server never saw.
pub struct Revisions(std::collections::HashMap<String, (BufferId, u64)>);

/// A **Foreign edit** across files, from a language server (#167).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceEdit {
    /// Who computed it, for what is said.
    pub from: String,
    /// The open buffers as they were when the question went out.
    pub asked: Revisions,
    /// What the server sent, in its order.
    pub changes: Vec<FileChange>,
}

/// One step of a workspace edit: edits to a file, or a resource operation --
/// which meowr declares it does not do, and so refuses (ADR-0013 amended).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileChange {
    Edit(FileEdit),
    Create { path: String },
    Rename { from: String, to: String },
    Delete { path: String },
}

/// The edits a workspace edit makes to one file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileEdit {
    pub path: String,
    /// The file's text as the host read it, for a file no buffer visited --
    /// reading is I/O and the session does none. `None` where a buffer does.
    pub read: Option<Result<String, String>>,
    pub edits: Vec<meowr_domain::LineEdit>,
}

/// A file's part of a workspace edit, checked and ready to apply.
struct Planned {
    target: Target,
    replacements: Vec<meowr_domain::Replacement>,
}

/// Where a planned file's replacements go.
enum Target {
    Open { id: BufferId, revision: u64 },
    /// A buffer for a file nobody had open, not yet among the session's.
    Unopened(Box<Buffer>),
}

/// A directory listing the host has been asked for, and what would make the
/// answer stale (#181).
///
/// The two rows ask the same question -- list what is beside this path -- and
/// differ only in what the answer belongs to. `SPC f F` is about the buffer's
/// own file, so an answer landing after you have moved on is about nothing;
/// `SPC f p` is about the config, which is nobody's buffer and cannot have
/// changed under the question. The rule travels with the question rather than
/// being worked out again where the answer lands, which is where two copies of
/// it would drift apart.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Files {
    /// `SPC f F`: everything under this buffer's file.
    UnderThisBuffer(String),
    /// `SPC f p`: everything beside `init.scm`.
    BesideTheConfig(String),
}

impl Files {
    /// The file whose directory is to be listed, which is all the host needs:
    /// `+default/find-file-under-here` lists the directory a *file* is in, and
    /// the config is a file in the directory `SPC f p` means.
    pub fn path(&self) -> &str {
        match self {
            Files::UnderThisBuffer(path) | Files::BesideTheConfig(path) => path,
        }
    }
}

/// A question that needs the Project's root, and what it is for (#148).
///
/// One type for two rows, which differ in what they hand over and what comes
/// back and in nothing the host has to find out: both are the buffer's
/// Project, resolved once.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FromRoot {
    /// `SPC f Y`: this buffer's path, from the Project's root.
    Yank { about: String },
    /// `SPC f R`: which of these recent files are in the Project.
    Recent { about: String, paths: Vec<String> },
    /// `SPC p x` and `SPC p X`: the Project whose scratch buffer to reach
    /// (#180). The row travels with the question because the session cannot
    /// hold it -- `pending_from_root` is drained by the host, so by the time
    /// the answer lands there is nothing left saying which key asked.
    Scratch { about: String, row: ScratchRow },
}

/// Which of the two ways a scratch row reaches its buffer (#180).
///
/// Doom has one command per row rather than a flag, and here they are one
/// command each too -- this says which, for the round trip a Project's scratch
/// takes through the host.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScratchRow {
    /// `SPC f x` and `SPC p x`: close the window showing it, or split below.
    Toggle,
    /// `SPC f X` and `SPC p X`: show it in this window.
    Switch,
}

/// A write the host should carry out: which file, and what goes in it.
///
/// The same division [`autosave::Autosave`] draws — this crate decides *what*
/// and *where*, and the binary owns the filesystem. `Action::Save` used to say
/// only "save something", and the host answered it from the path on its own
/// command line. That was right while a session had one path-bearing buffer,
/// and previewing made a second.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Write {
    pub path: String,
    pub contents: String,
    /// The buffer was given this name and has not been written under it yet,
    /// so the host should look before it writes.
    ///
    /// Checking whether a path is taken is I/O and `meowr-app` does none, so
    /// the one write that can land on somebody else's file has to say so and
    /// let the host look. False for every ordinary save: a file meowr opened
    /// is a file it may write back.
    pub fresh: bool,
}

/// Why a buffer cannot be written.
///
/// Ordered as [`Session::unwritable_reason`] tests them: read-only first,
/// because a read-only buffer with no path would otherwise be told the less
/// useful half.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Unwritable {
    ReadOnly,
    Preview,
    /// Nobody has said where. The one that is a question rather than a
    /// refusal — see [`Session::nameable`].
    Unnamed,
    Unread,
}

impl Unwritable {
    /// The reason as the end of a sentence about a named buffer, for
    /// `save-some-buffers`, which says several at once (#166).
    fn briefly(self) -> &'static str {
        match self {
            Unwritable::ReadOnly => "is read-only",
            Unwritable::Preview => "is a preview",
            Unwritable::Unnamed => "has no file",
            Unwritable::Unread => "has not been read yet",
        }
    }
}

/// One `save-some-buffers` in flight (#166): the writes the host is asked
/// for, and the buffers that could not be asked about, which wait for the
/// writes so that one sentence says both.
#[derive(Debug, Default)]
struct SavingSome {
    writes: Vec<Write>,
    unsaveable: Vec<String>,
}

/// Why a Foreign edit was not applied, naming the file and who computed it
/// (ADR-0013).
fn not_applied(path: &str, from: &str, why: ForeignRefusal) -> String {
    match why {
        ForeignRefusal::ReadOnly => format!("{path} is read-only, so {from}'s edit was not applied."),
        ForeignRefusal::Moved => {
            format!("{path} changed while {from} was working, so its edit was not applied.")
        }
        ForeignRefusal::Unread => {
            format!("{path} has not been read yet, so {from}'s edit was not applied.")
        }
        ForeignRefusal::Unapplicable(why) => {
            format!("{from}'s edit could not be applied to {path}: {why}.")
        }
        ForeignRefusal::PastEnd(past) => format!(
            "{from}'s edit could not be applied to {path}: line {} is past its end.",
            past.line + 1
        ),
        ForeignRefusal::NamedTwice => {
            format!("{from}'s edit could not be applied to {path}: it names the file twice.")
        }
    }
}

/// The last segment of a path, which is what a Project is called (#180).
///
/// Doom names a project scratch with `doom-project-name`, which is the root
/// directory's own name. A trailing slash is trimmed first, so `/w/meowr/` is
/// still `meowr` rather than nothing -- the root arrives from the host and
/// nothing promises which spelling it takes.
fn basename(path: &str) -> &str {
    let trimmed = path.trim_end_matches('/');
    match trimmed.rsplit('/').next() {
        Some(name) if !name.is_empty() => name,
        // `/` itself, which is a repository somebody could have made.
        _ => path,
    }
}

/// "buffer" or "buffers", for a sentence that counts them.
fn buffers(count: usize) -> &'static str {
    match count {
        1 => "buffer",
        _ => "buffers",
    }
}

/// Buffers standing in the way of something that would lose them (#182).
///
/// Three rows say this and have to say it the same way -- quitting, restarting
/// and killing the other buffers are one shape of refusal with one sentence --
/// and a comment claiming they match is not the same as their matching.
struct Unsaved {
    /// The buffers, named: `Unsaved: a.rs, b.rs.`
    named: String,
    /// Whether `write-file` would help, which it does not for a buffer with no
    /// path: a refusal should not advise a save that cannot happen.
    savable: bool,
}

/// The sentence those three rows share.
///
/// Named rather than counted, because a count is a refusal you cannot act on
/// without hunting for what it meant; labelled rather than joined with an `and`,
/// because the list can be long.
fn unsaved_sentence<S: AsRef<str>>(names: &[S]) -> String {
    let names: Vec<&str> = names.iter().map(AsRef::as_ref).collect();
    format!("Unsaved: {}.", names.join(", "))
}

/// A **Go-ahead**: what a refusal names as meaning "do it anyway" (#182).
///
/// The glossary has the concept and why meowr has it -- it is what stands in
/// for the yes-or-no read meowr does not have. This is the pair of forms, in
/// the type that has to choose between them: a second *command* where Doom has
/// one, and `SPC u` in front of the same row where Doom would have prompted,
/// which is what #172 and #173 settled for the rows that cannot be undone.
/// A restart is one of those.
#[derive(Debug, Clone, Copy)]
enum WayPast<'a> {
    /// A command of its own, named by the keys that reach it.
    Command(&'a str),
    /// The same row, with the universal argument in front of it.
    GoAhead(&'a str),
}

/// The directory part of a path, for the sentence that says it was created.
fn directory_of(path: &str) -> Option<String> {
    std::path::Path::new(path)
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .map(|parent| format!("{}/", parent.display()))
}

/// The kind `SPC k o` asks for.
const ORGANIZE_IMPORTS: &str = "source.organizeImports";

/// The bytes of the line `pos` is on, without its newline.
fn line_bounds(doc: &Document, pos: usize) -> Range<usize> {
    let line = doc
        .byte_to_line(pos)
        .unwrap_or_else(|| doc.line_count().saturating_sub(1));
    let start = doc.line_to_byte(line).unwrap_or(0);
    let text = doc.line(line).map(|l| l.to_string()).unwrap_or_default();
    start..start + text.trim_end_matches('\n').len()
}

/// Names in a sentence: `A`, `A and B`, `A, B and C`.
fn listed(names: &[&str]) -> String {
    match names {
        [] => String::new(),
        [only] => only.to_string(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}

/// A server's words, as a sentence: who said it, then what, ending once.
fn in_words(from: &str, words: &str) -> String {
    let words = words.trim_end();
    let stop = if words.ends_with(['.', '!', '?']) { "" } else { "." };
    format!("{from}: {words}{stop}")
}

/// One row of the leader menu.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LeaderEntry {
    /// A [`Key`], not a `char`: the tree holds chords, so the menu shows them.
    pub key: Key,
    pub description: String,
    pub kind: LeaderKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LeaderKind {
    Command,
    Prefix,
    /// Held for M2 or M3. Shown differently, because "not yet" and "not a
    /// thing" are different answers and the menu should not conflate them.
    Reserved,
}

/// What the next key will be an argument to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Awaiting {
    Find { forward: bool },
    Till { forward: bool },
}

/// The command a printable key runs in Insert State.
///
/// A named command rather than a special case in `on_key`, so that a Plugin
/// can advise it like any other — a hole a plugin could not reach into is not
/// an extension seam.
const SELF_INSERT: &str = "self-insert";

pub struct Session {
    buffers: Vec<Buffer>,
    windows: Tree,
    /// The whole frame, which the window tree divides.
    area: Area,
    state: State,
    registry: Registry,
    keymap: Keymap,
    fallbacks: Fallbacks,
    /// Keys of a sequence that has not resolved yet.
    pending: Vec<Key>,
    /// The numeric argument the next command will receive — `prefix-arg`.
    argument: Argument,
    options: Options,
    /// Which command serves each Language, by the Language's own name.
    ///
    /// #119. Keyed by name rather than by `Language` because that is what a
    /// config writes and what a problem message has to quote back; the
    /// `Language` is a workspace-local index and means nothing to a reader.
    servers: std::collections::HashMap<String, meowr_command::LanguageServer>,
    /// Which program formats each Language, by Language name (ADR-0014).
    formatters: std::collections::HashMap<String, meowr_command::Formatter>,
    /// Whether a command run by a hook is on the stack.
    ///
    /// #64: **hooks do not fire for commands run by hooks.** One flat rule, and
    /// it terminates by construction — an `after-save` hook that saves, or a
    /// `state-change` hook that changes State, cannot loop. Emacs does not
    /// guard this and its users hit it.
    in_hook: bool,
    /// Global on purpose, as it is in Emacs: copying in one buffer and yanking
    /// in another is the point of a kill ring. Everything else that used to
    /// live here has moved onto the buffer it belongs to.
    kill_ring: KillRing,
    /// Where you have been, for going back. ADR-0011.
    trail: Trail,
    /// The files most recently visited, switched to or written, which the host
    /// reads at launch and keeps on the way out (#152).
    recent: meowr_core::edit::RecentFiles,
    /// Doom's one `recentf-exclude`, as the host found it: `$XDG_RUNTIME_DIR`
    /// or `/run`. Nothing under it is ever a recent file.
    recent_exclude: Option<String>,
    /// Ring 2, seeded from the default table and owned from then on so a
    /// Plugin can add to it. See [`leader::Leader`].
    leader: leader::Leader,
    /// The narrowing widget and what belongs with it.
    narrow: narrow::Narrowing,
    /// Auto-save files the host should delete, because the buffer beside them
    /// was killed. Writing is the host's, and so is unwriting.
    autosaves_to_forget: Vec<String>,
    /// Keypad State's own state.
    keypad: keypad::Keypad,
    /// Beacon State's own state.
    beacon: beacon::Beacons,
    last_command: Option<String>,
    /// The argument the last command was given — Emacs's `last-prefix-arg`,
    /// which is what `repeat` falls back to.
    last_argument: Argument,
    signals: usize,
    /// `meow-numeric-argument-keymap` is transient and binds the ten digits
    /// and nothing else, so a digit after `SPC 3` folds into the count instead
    /// of meaning `meow-expand`. True only between the two.
    counting: bool,
    things: Things,
    hooks: Hooks,
    languages: Arc<Languages>,
    /// The VCS branch, handed in by the host.
    branch: Option<String>,
    /// The narrowing widget, when one is open. There is at most one, because
    /// it lives in the Echo Area and there is one of those.
    /// `<` was pressed; the next key names a source.
    /// A command is waiting for a character — `f` and `t` are
    /// `(interactive "cFind:")`, so the next key is an argument rather than a
    /// binding. Kept as state rather than as a keymap of 256 sequences.
    awaiting_char: Option<Awaiting>,
    /// The search ring — `regexp-search-ring`. Deliberately not the selection
    /// history: repeating a search and walking back through selections are
    /// different things, and Meow keeps them as separate variables.
    search_ring: Vec<String>,
    /// The open widget is a `meow-visit`, so accepting searches.
    /// Where an edit will be replayed. Positions, not cursors: Meow records a
    /// keyboard macro and replays it **sequentially**, which is why a search
    /// inside the macro can find something different on each line. That
    /// divergence is the feature, and simultaneous multi-cursor under this
    /// name would be the first crack in Fidelity.
    /// Where the point was when narrowing opened, so cancelling can put it
    /// back. Preview moves it into the window behind — that is the whole idea
    /// of previewing there rather than into a panel — and a preview you backed
    /// A line for the Echo Area: what the last command wanted to say.
    ///
    /// Cleared by the next key, which is what Emacs does with its own — a
    /// message that outlives the command that wrote it starts describing
    /// something that already happened.
    echo: Option<String>,
    /// Where a hover was asked for, until the host has asked the server.
    pending_hover: Option<(String, usize)>,
    pending_lookup: Option<Lookup>,
    /// A Project question waiting to be put to git.
    ///
    /// Two `Option`s deliberately: the outer is whether a question is waiting,
    /// the inner which file it is about. A buffer visiting no file still has a
    /// Project -- the working directory's -- so `Some(None)` is a real question
    /// and not an absent one.
    pending_project: Option<Option<String>>,
    pending_search: Option<Search>,
    /// A directory `SPC f F` or `SPC f p` wants listed. See [`Files`].
    pending_files: Option<Files>,
    /// Where `init.scm` is, as the host worked it out at startup (#181).
    ///
    /// The host's fact and not the session's: finding it means `XDG_CONFIG_HOME`
    /// and `HOME`, which is the environment, which is I/O. `None` when there is
    /// no home to look in at all -- the one case `config::find` cannot answer --
    /// and both config rows refuse by name for it.
    ///
    /// Held rather than asked for each time because it cannot change while meowr
    /// runs: a config is read once at startup and there is no reload (#175).
    config: Option<String>,
    /// A question for the Project's root, for `SPC f Y` or `SPC f R`.
    pending_from_root: Option<FromRoot>,
    /// Formatters the host should run, for `SPC k f` or after a save -- a queue,
    /// because `save-some-buffers` saves several buffers at once.
    pending_format: std::collections::VecDeque<Format>,
    /// What `save-some-buffers` asked to be written, and what it could not.
    saving_some: Option<SavingSome>,
    /// A rename's question for a language server, for `SPC k r`.
    pending_rename: Option<RenameQuestion>,
    /// A code action question for a language server, for `SPC k a` and
    /// `SPC k o`.
    pending_code_actions: Option<CodeActionQuestion>,
    /// A file to copy or move, for `SPC f C` and `SPC f m`.
    pending_file_work: Option<FileRequest>,
    /// A file to delete, for `SPC f D`. Set only once the go-ahead was given.
    pending_file_deletion: Option<FileDeletion>,
    /// A `SPC q F` waiting for the saves it asked for to land (#176).
    killing_all: Option<KillAll>,
    /// The next save skips formatting: it was asked for with `SPC u`, Doom's
    /// prefix argument on `+format/save-buffer`.
    skip_format_once: bool,
    /// A buffer whose save-time format landed, waiting to be written again.
    pending_quiet_write: Option<String>,
    /// Languages whose formatter was found missing on a save, said once each.
    missing_formatters: std::collections::HashSet<String>,
    /// Where the last idle hover was asked about, so one resting point is
    /// asked about once.
    ///
    /// Without it a point resting on whitespace -- where the answer is "there
    /// is nothing here" and so nothing is shown -- would be asked about again
    /// every `hover-delay` for as long as nobody touched the keyboard.
    asked_at_rest: Option<Resting>,
    /// A hover, waiting for the next key to dismiss it.
    ///
    /// Beside `echo` rather than inside it because a hover is many lines and a
    /// message is one, and #111 settled that a hover is the Echo Area *grown
    /// to fit* rather than a floating box — ADR-0004 leaves nowhere to float,
    /// and `lsp-ui-doc`'s popup is the casualty that decision named.
    hover: Option<Hover>,
    /// Unsaved work from a previous run, and whether it has been asked for.
    /// See [`Recovery`].
    recovery: Recovery,
    /// Whether keys are being replayed rather than typed — Emacs's
    /// `executing-kbd-macro`. Meow reads it in exactly one place, and it is
    /// load-bearing there: see [`Session::replay`].
    executing: bool,
    /// Whether a run of commands is being made one undoable change — Meow's
    /// `meow--wrap-collapse-undo`. Only the beacon commands that act at every
    /// beacon at once set it; see [`Session::beacon_at_each`].
    collapsing: bool,
}

impl Session {
    pub fn new(viewport: Viewport) -> Self {
        let mut session = Self {
            buffers: vec![Buffer::new()],
            windows: Tree::new(BufferId(0)),
            area: Area {
                x: 0,
                y: 0,
                width: 80,
                height: viewport.height as u16,
            },
            state: State::Normal,
            registry: default_registry(),
            keymap: default_keymap(),
            fallbacks: default_fallbacks(),
            pending: Vec::new(),
            argument: Argument::None,
            options: Options::default(),
            servers: default_servers(),
            formatters: default_formatters(),
            in_hook: false,
            kill_ring: KillRing::new(),
            trail: Trail::new(),
            recent: meowr_core::edit::RecentFiles::default(),
            recent_exclude: None,
            last_command: None,
            last_argument: Argument::None,
            signals: 0,
            things: Things::new(),
            hooks: default_hooks(),
            languages: meowr_syntax::languages(),
            branch: None,
            leader: leader::Leader::new(),
            narrow: narrow::Narrowing::default(),
            counting: false,
            autosaves_to_forget: Vec::new(),
            keypad: keypad::Keypad::default(),
            beacon: beacon::Beacons::default(),
            awaiting_char: None,
            search_ring: Vec::new(),
            executing: false,
            collapsing: false,
            echo: None,
            pending_hover: None,
            pending_lookup: None,
            pending_project: None,
            pending_search: None,
            pending_files: None,
            config: None,
            pending_from_root: None,
            pending_format: std::collections::VecDeque::new(),
            saving_some: None,
            pending_rename: None,
            pending_code_actions: None,
            pending_file_work: None,
            pending_file_deletion: None,
            killing_all: None,
            skip_format_once: false,
            pending_quiet_write: None,
            missing_formatters: std::collections::HashSet::new(),
            asked_at_rest: None,
            hover: None,
            recovery: Recovery::Nothing,
        };
        // Lay the tree out immediately: a window with no area has no rows, and
        // a session that has never been resized would show nothing at all.
        session.windows.layout(session.area);
        session
    }

    // --- buffers and windows ----------------------------------------------

    pub fn buffers(&self) -> &[Buffer] {
        &self.buffers
    }

    pub fn buffer(&self, id: BufferId) -> &Buffer {
        &self.buffers[id.0]
    }

    pub fn current_buffer_id(&self) -> BufferId {
        self.windows.current().buffer
    }

    pub fn current(&self) -> &Buffer {
        &self.buffers[self.current_buffer_id().0]
    }

    pub fn current_mut(&mut self) -> &mut Buffer {
        let id = self.current_buffer_id();
        &mut self.buffers[id.0]
    }

    /// Open a buffer and show it in the focused window.
    pub fn open(&mut self, buffer: Buffer) -> BufferId {
        let id = BufferId(self.buffers.len());
        self.buffers.push(buffer);
        self.windows.current_mut().buffer = id;
        self.windows.current_mut().top = 0;
        id
    }

    /// Show an already-open buffer in the focused window.
    ///
    /// A killed slot is refused. Keeping the slot is what makes every other
    /// `BufferId` stay valid, but the tombstone in it is not a buffer — showing
    /// one would put a writable, unnamed buffer on screen that the modeline
    /// draws as `*scratch*`.
    pub fn show(&mut self, id: BufferId) {
        if id.0 < self.buffers.len() && !self.buffers[id.0].killed {
            self.windows.current_mut().buffer = id;
            self.windows.current_mut().top = 0;
        }
    }

    pub fn windows(&self) -> &Tree {
        &self.windows
    }

    pub fn window_count(&self) -> usize {
        self.windows.order().len()
    }

    pub fn focus(&self) -> WindowId {
        self.windows.focus()
    }

    /// The area of every visible window, in draw order.
    pub fn layout(&mut self) -> Vec<(WindowId, Area)> {
        self.windows.layout(self.area);
        self.windows
            .order()
            .into_iter()
            .map(|id| (id, self.windows.window(id).area))
            .collect()
    }

    // --- what the focused buffer is doing ---------------------------------

    pub fn signals(&self) -> usize {
        self.signals
    }

    pub fn argument(&self) -> Argument {
        self.argument
    }

    pub fn options(&self) -> Options {
        self.options
    }

    /// The server for a buffer's Language, if one is declared.
    ///
    /// The question `lsp::serves` used to answer by looking at a file
    /// extension, asked properly: a buffer gets a language server exactly when
    /// its Language has one declared, which is the same question for every
    /// Language rather than one hardcoded answer for Rust.
    pub fn language_server(&self, id: BufferId) -> Option<&meowr_command::LanguageServer> {
        let language = self.buffers.get(id.0)?.language?;
        self.servers.get(self.languages.name_of(language)?)
    }

    /// How to run the formatter for a buffer, if its Language has one and the
    /// buffer has a file (ADR-0014): the program and its arguments, `{file}`
    /// already the buffer's path. A leading `~/` in the program is left for the
    /// host, which knows the home directory, to expand with
    /// [`meowr_command::Invocation::at_home`].
    pub fn formatter(&self, id: BufferId) -> Option<meowr_command::Invocation> {
        let buffer = self.buffers.get(id.0)?;
        let formatter = self.formatters.get(self.languages.name_of(buffer.language?)?)?;
        Some(formatter.invocation(buffer.path()?, None))
    }

    /// Every buffer a language server has something to say about.
    ///
    /// Asked as one question rather than left to the host to assemble, and the
    /// reason is a bug that shape had: iterating `buffers()`, filtering to the
    /// real ones and *then* enumerating gives an index into the filtered
    /// sequence, while a [`BufferId`] indexes the whole list. `entomb` replaces
    /// a killed buffer with a tombstone **in place** and a preview is `open`ed
    /// into the list, so one non-real slot before a served buffer shifted every
    /// later id — and the host asked about the wrong buffer, got `None`, and
    /// concluded nothing needed serving. That dropped the connection out from
    /// under a Rust file still on screen.
    ///
    /// Here the id comes from the same list the lookup uses, so the two cannot
    /// disagree.
    pub fn served(&self) -> Vec<Served<'_>> {
        self.buffers
            .iter()
            .enumerate()
            .filter(|(_, buffer)| buffer.is_real())
            .filter_map(|(index, buffer)| {
                let language = buffer.language?;
                let name = self.languages.name_of(language)?;
                Some(Served {
                    id: BufferId(index),
                    path: buffer.path()?,
                    language: name,
                    server: self.servers.get(name)?,
                    revision: buffer.revision(),
                })
            })
            .collect()
    }

    pub fn set_options(&mut self, options: Options) {
        self.options = options;
    }

    pub fn loaded(&mut self, doc: Document) {
        self.current_mut().loaded(doc);
        self.touch_recent();
    }

    pub fn failed(&mut self, message: String) {
        self.current_mut().failed(message);
    }

    pub fn is_loading(&self) -> bool {
        self.current().is_loading()
    }

    pub fn error(&self) -> Option<&str> {
        self.current().error()
    }

    pub fn document(&self) -> Option<&Document> {
        self.current().document()
    }

    /// The State the editor is in.
    ///
    /// A plain field read since Guests were dropped. It used to derive Motion
    /// State from a Guest owning the buffer, which is what ADR-0005 asked for;
    /// with no Guests there is nothing to derive it from, and Motion State is
    /// now a State meowr has a name for and never enters. See
    /// `meowr-adr-0005`, superseded.
    pub fn state(&self) -> State {
        self.state
    }

    /// The shortest keys that reach `command`: the keymap first, then the
    /// leader.
    ///
    /// [`Keymap::keys_for`] answers only for Ring 1, and a command may live
    /// only in Ring 2 — `write-file` does the moment a config drops `C-x C-s`,
    /// and answering `None` there would say "unbound" about a command two keys
    /// away. Both halves have to be asked, and the session is the only thing
    /// holding both, so the question is answered here rather than in either.
    ///
    /// The leader's own key is looked up rather than written as `SPC`, for the
    /// reason this file gives everywhere else: Ring 3 lets it move.
    pub fn keys_for(&self, state: State, command: &str) -> Option<Vec<Key>> {
        if let Some(keys) = self.keymap.keys_for(state, command) {
            return Some(keys);
        }
        // The leader hangs off Normal State alone, so there is nothing to add
        // for any other State.
        if state != State::Normal {
            return None;
        }
        self.under_leader(&self.leader.suffix_for(command)?)
    }

    /// The leader route to `command`, spelled — when that route is the point.
    ///
    /// Not the same question [`Session::keys_for`] answers. That one gives the
    /// *shortest* way, which for `find-file` is `C-x C-f`; a message about a
    /// broken config is addressed to somebody who writes leader bindings, and
    /// wants the leader binding. Both are right answers to different questions,
    /// which is why there are two of these and not one with a flag.
    fn leader_route(&self, command: &str) -> Option<String> {
        let keys = self.under_leader(&self.leader.suffix_for(command)?)?;
        Some(Keys(&keys).to_string())
    }

    /// A leader suffix, with the key that opens the leader in front of it.
    ///
    /// The tree stores suffixes and cannot see the keymap, so this join has to
    /// happen somewhere that can — and it happens *here*, once. Three copies
    /// of it is what deleting `Leader::keys_for`, which wrote `SPC` into its
    /// own answer, was supposed to prevent rather than triple.
    fn under_leader(&self, suffix: &[Key]) -> Option<Vec<Key>> {
        let opens = self.keymap.keys_for(State::Normal, "leader")?;
        Some([opens.as_slice(), suffix].concat())
    }

    /// Run a `find` or a `till` for a character that is already in hand.
    ///
    /// Reached twice: once when the character has just been typed, and once
    /// when `repeat` supplies the one from last time. The second is why this
    /// is a method rather than the tail of the key handler — Emacs's `repeat`
    /// replays a command with the arguments it had, and for these two the
    /// character *is* an argument.
    fn search_for(&mut self, ch: char, forward: bool, till: bool) -> Action {
        // `meow-till` steps off the point before searching — `(if (> n 0)
        // (forward-char 1) (forward-char -1))` — and at the edge of the buffer
        // that raises rather than failing to find. So a backward till at
        // position 0 is an error where a fruitless search is only a message.
        // `meow-find` has no such step and no such edge.
        let Some(end) = self.current().document().map(|doc| doc.byte_len()) else {
            // Asked before the edges are, because a file still being read has
            // no edges: `byte_len` of nothing is zero, and a forward till was
            // told it had reached the end of a buffer it had not seen.
            return self.refuse("Still reading this file.");
        };
        let at = self.current().selection.current().pos();
        let stuck = till && if forward { at >= end } else { at == 0 };
        if stuck {
            return self.refuse_aloud(if forward {
                "End of buffer."
            } else {
                "Beginning of buffer."
            });
        }
        let buffer = self.current_mut();
        let Content::Ready(doc) = &buffer.content else {
            return self.refuse("Still reading this file.");
        };
        let found = core::find(doc, &mut buffer.selection, ch, forward, till);
        buffer.after_command(10);
        self.scroll_into_view();
        if !found {
            // `(message "char %s not found" ch-str)` — Meow's own words, and
            // its own quietness. `meow-find` does not raise here, so a bell is
            // a divergence: the harness reads Emacs signalling nothing for
            // every failed find and till.
            return self.refuse(format!("char {ch} not found"));
        }
        Action::Redraw
    }

    /// Which way a `find` or a `till` runs, consuming the argument that said.
    ///
    /// Both places that start a search ask this, and neither states the rule:
    /// a negative argument searches backward, as `meow-find` does. Remembering
    /// happens here too, *with* the argument, because Emacs's `repeat` gives a
    /// command the prefix it was given before — so `- f x \'` searches backward
    /// the second time as well.
    fn take_search_direction(&mut self, name: &str) -> bool {
        let argument = std::mem::take(&mut self.argument);
        let forward = argument.value().is_none_or(|n| n >= 0);
        self.remember(name, argument);
        forward
    }

    /// The character a repeated `find` or `till` should look for again.
    ///
    /// `None` when the command being repeated is not one of them, which is the
    /// ordinary case, or when nothing has been searched for yet.
    ///
    /// It exists because re-dispatching would re-enter the character read and
    /// wait for one the user has already given: `f o \'` sat still where Emacs
    /// finds the next `o`. The character is kept for the digit-expand path
    /// already — `meow--last-find` — so this asks the same store.
    fn search_to_repeat(&self, name: &str) -> Option<(char, bool)> {
        let till = match name {
            "find" => false,
            "till" => true,
            _ => return None,
        };
        let selection = &self.current().selection;
        let ch = if till {
            selection.last_till()
        } else {
            selection.last_find()
        }?;
        Some((ch, till))
    }

    /// Decline, and say why.
    ///
    /// A command that cannot do the thing has three ways to say so and only
    /// one of them reaches the user: returning `Action::Nothing` says it to
    /// the caller, `signals` says it to the terminal bell, and this says it to
    /// the person. An audit of the ordinary flow found six commands using the
    /// first two alone — and in a modal editor a silent key is the *expected*
    /// outcome of a mistype, so silence on a real refusal is unreadable.
    ///
    /// The bell is **not** rung here, and that is the whole reason there are
    /// two of these. Emacs chooses between `message` and `user-error` per
    /// command and Meow inherits the choice, so a bell where Emacs has none is
    /// a divergence — `meow-pop-grab` with no Grab is tested to be quiet, and
    /// was quiet before this existed. [`Session::refuse_aloud`] is the other
    /// one, for refusals that already rang.
    fn refuse(&mut self, why: impl Into<String>) -> Action {
        self.echo = Some(why.into());
        Action::Redraw
    }

    /// The refusal three Grab commands share, with the way out named.
    ///
    /// One sentence rather than three, and the key is looked up rather than
    /// written: `G` is where `grab` lives today and Ring 3 lets it move, so a
    /// message that spelled it would be the second source for the keymap's
    /// fact that this codebase keeps refusing to create. It lives on the
    /// session because that is what can ask.
    fn no_grab(&mut self) -> Action {
        let make = match self.written("grab") {
            Some(keys) => format!(" {keys} over a selection makes one."),
            None => String::new(),
        };
        self.refuse(format!("No Grab held.{make}"))
    }

    /// Decline audibly: the bell as well as the sentence.
    ///
    /// For a refusal Emacs raises as an error rather than reporting as a
    /// message. The rarer of the two, and deliberately the one you have to
    /// ask for: adding a bell is a change to what meowr sounds like, where
    /// adding a sentence is only a change to what it says.
    fn refuse_aloud(&mut self, why: impl Into<String>) -> Action {
        self.signals += 1;
        self.refuse(why)
    }

    /// [`Session::keys_for`], as a message would write it.
    ///
    /// Every echo line that names a key wants the same two things — both rings
    /// asked, and the answer spelled the way the keymap spells it — and each
    /// one that assembled those itself assembled them differently. The one
    /// that asked only the keymap lost its advice the day a config moved
    /// `write-file` under the leader; the one that asked only the leader would
    /// lose it the day a config moved something the other way.
    /// How to reach `command`, for a sentence to name: its leader route, else
    /// its shortest key, else the name `M-x` takes.
    fn reached_by(&self, command: &str) -> String {
        self.leader_route(command)
            .or_else(|| self.written(command))
            .unwrap_or_else(|| command.to_string())
    }

    fn written(&self, command: &str) -> Option<String> {
        self.keys_for(State::Normal, command)
            .map(|keys| Keys(&keys).to_string())
    }

    pub fn point(&self) -> usize {
        self.current().selection.current().pos()
    }

    pub fn selection(&self) -> Selection {
        self.current().selection.current()
    }

    pub fn history_depth(&self) -> usize {
        self.current().selection.history_depth()
    }

    /// Start from an arbitrary selection, discarding any history. The fidelity
    /// harness needs it: a case that tests what `w` does from an existing
    /// selection cannot key its way there without also testing the keys that
    /// built it.
    pub fn set_selection(&mut self, selection: Selection) {
        self.current_mut().selection = meowr_core::SelectionState::new(selection);
    }

    pub fn is_modified(&self) -> bool {
        self.current().is_modified()
    }

    pub fn contents(&self) -> Option<String> {
        self.document().map(|doc| doc.text().to_string())
    }

    /// Auto-save files the host should delete, cleared as they are handed out.
    ///
    /// Drained rather than read, for the reason `take_autosaves` is: handing
    /// the same removal out twice would have the host delete a file that a
    /// later buffer had since written.
    pub fn take_autosaves_to_forget(&mut self) -> Vec<String> {
        std::mem::take(&mut self.autosaves_to_forget)
    }

    /// What `write-file` should write, if anything.
    ///
    /// The buffer on screen names its own file. The host used to answer
    /// `Action::Save` from the path on its own command line, which was right
    /// while a session had one path-bearing buffer and became a write to the
    /// wrong file the moment previewing could make a second.
    ///
    /// A buffer nobody chose to open answers `None`: a preview is read-only
    /// because the choice that would make it real has not been made, and
    /// writing one out would put a file you were only looking at over a file
    /// you never named.
    pub fn save_target(&self) -> Option<Write> {
        if self.unwritable_reason().is_some() {
            return None;
        }
        let buffer = self.current();
        Some(Write {
            path: buffer.path()?.to_string(),
            contents: buffer.document()?.text().to_string(),
            fresh: buffer.fresh_name,
        })
    }

    /// Why there is nothing for the host to write.
    ///
    /// The inverse of [`Session::save_target`]'s `None`, walking the same four
    /// conditions in the same order — read-only first, because a read-only
    /// buffer with no path would otherwise be told the less useful half.
    ///
    /// Four, not three. The first version of this walked only the three the
    /// `if` in `save_target` makes obvious and missed the `?` on `document()`
    /// hiding in the struct literal below it, so a buffer that had a path and
    /// had not finished loading was told it had no file — which was false.
    /// Adjacency is not enough to keep two lists in step; the last arm is
    /// therefore the *unloaded* case rather than a catch-all, so a fifth
    /// condition added to `save_target` shows up here as a wrong sentence
    /// about loading rather than as silence.
    fn unwritable(&self) -> &'static str {
        match self.unwritable_reason() {
            Some(Unwritable::ReadOnly) => "This buffer is read-only.",
            Some(Unwritable::Preview) => "This buffer is a preview; nothing here is yours to save.",
            // Reached only where the offer cannot be made — see `nameable`.
            Some(Unwritable::Unnamed) => "No file behind this buffer to write to.",
            Some(Unwritable::Unread) => "Still reading this file; nothing to write yet.",
            None => "Nothing to write.",
        }
    }

    /// Why there is nothing to write, if there is nothing.
    ///
    /// One walk of the four conditions, because there were three: `save_target`
    /// answered `None` by them, `unwritable` named them again to say which, and
    /// `nameable` asked a third time whether only the third had fired.
    /// `unwritable`'s own comment already warned that adjacency is not enough
    /// to keep two lists in step, having been caught out by exactly that.
    fn unwritable_reason(&self) -> Option<Unwritable> {
        let buffer = self.current();
        if buffer.read_only.refuses_edits() {
            Some(Unwritable::ReadOnly)
        } else if !buffer.is_real() {
            Some(Unwritable::Preview)
        } else if buffer.path().is_none() {
            Some(Unwritable::Unnamed)
        } else if buffer.document().is_none() {
            Some(Unwritable::Unread)
        } else {
            None
        }
    }

    /// The host found a file already at the name just given.
    ///
    /// Refused rather than confirmed: Emacs asks a yes-or-no question here and
    /// meowr has no yes-or-no read, which is the same wall `refuse_unsaved`
    /// met — so the answer is the same shape, a refusal that names the key
    /// which means "yes".
    pub fn refuse_overwrite(&mut self) {
        let path = self
            .current()
            .path()
            .map(str::to_string)
            .unwrap_or_default();
        let force = self.reached_by("overwrite-file");
        self.refuse_aloud(format!("{path} exists. {force} to write over it."));
    }

    /// Say the name is no longer new, then save the ordinary way.
    ///
    /// Through `write-file` rather than answering `Action::Save` here: that
    /// arm is where a save with nothing to write is refused and where
    /// `before-save` runs, and returning `Save` directly walked past both — so
    /// this key over `*scratch*` asked the host to write a file with no name
    /// and the host, finding none, said nothing. #80's silence under a new key.
    fn overwrite_file(&mut self) -> Action {
        self.current_mut().fresh_name = false;
        self.dispatch_command("write-file", None)
    }

    /// Whether the only thing stopping a write is that nobody has said where.
    ///
    /// The other three reasons [`Session::save_target`] answers `None` are not
    /// questions to ask: a read-only buffer stays read-only whatever it is
    /// called, a preview is not yours, and a file still being read has nothing
    /// settled to write.
    fn nameable(&self) -> bool {
        matches!(self.unwritable_reason(), Some(Unwritable::Unnamed))
    }

    /// The host wrote the file. `after-save` fires here and not in the
    /// command, because until this is called nothing has actually been saved.
    pub fn mark_saved(&mut self) {
        // Taken before the hooks run, so an `after-save` hook that runs
        // `write-file` itself cannot reset what this save was asked with.
        let skip_format = std::mem::take(&mut self.skip_format_once);
        self.marked_saved(skip_format);
    }

    /// The current buffer was written: the rest of `mark_saved`, shared with
    /// `saved_some`, which runs it with each written buffer current.
    fn marked_saved(&mut self, skip_format: bool) {
        let buffer = self.current_mut();
        // Written under its new name, so it is ours now and the host need not
        // look again.
        buffer.fresh_name = false;
        buffer.history.mark_saved();
        // `delete-auto-save-file-if-necessary`: the file on disk now says
        // what the auto-save said, and an auto-save left beside it would
        // offer to recover work that is no longer missing.
        buffer.since_autosave = 0;
        self.touch_recent();
        // The host sets `dirty` after every write, so a Redraw from here has
        // nowhere it needs to go.
        let _ = self.run_hooks(HookPoint::AfterSave);
        // Then the format, apheleia's order: the save above is already true.
        if self.options.format_on_save
            && !skip_format
            && let Ok(format) = self.format_of(self.current_buffer_id(), FormatOrigin::Save)
        {
            self.pending_format.push_back(format);
        }
    }

    /// Run `act` with buffer `id` current, and put the focused window back as
    /// it was: Emacs's `with-current-buffer`, for a save's hooks run in the
    /// buffer saved rather than the one on screen (#166).
    ///
    /// The whole window is put back -- its buffer, its first visible line and
    /// the point parked on it -- because a command a hook runs scrolls the
    /// window it thinks it is in. It is put back by its id, and the focus with
    /// it, because a hook can move the focus: restoring whichever window is
    /// focused afterwards would show one window's buffer in another. A window
    /// the hook closed is not focused again, and a buffer the hook killed is
    /// not put back, since `entomb` already pointed the window elsewhere.
    fn in_buffer<R>(&mut self, id: BufferId, act: impl FnOnce(&mut Self) -> R) -> R {
        let focus = self.windows.focus();
        let window = self.windows.current();
        let (buffer, top, point) = (window.buffer, window.top, window.point.clone());
        self.windows.current_mut().buffer = id;
        let result = act(self);
        if self.windows.order().contains(&focus) {
            self.windows.set_focus(focus);
        }
        if self.buffers[buffer.0].is_real() {
            let window = self.windows.window_mut(focus);
            window.buffer = buffer;
            window.top = top;
            window.point = point;
        }
        result
    }

    /// Every real buffer with work in it that is not on disk.
    ///
    /// Asked by `save-some-buffers` and by `SPC q F`, which has to know
    /// whether there is anything to save before it asks for the saves (#176).
    fn modified_buffers(&self) -> Vec<BufferId> {
        self.real_buffers()
            .filter(|id| self.buffer(*id).is_modified())
            .collect()
    }

    /// "SPC u SPC f D": the two presses that mean "yes, really" for `command`.
    ///
    /// The go-ahead idiom #173 decided, in the one place that spells it. Both
    /// halves are looked up rather than written, because a binding named in
    /// prose is a second source for the keymap's fact and Ring 3 exists to
    /// move bindings.
    fn go_ahead_for(&self, command: &str) -> String {
        format!(
            "{} {}",
            self.reached_by("universal-argument"),
            self.reached_by(command)
        )
    }

    /// `save-some-buffers`: write every modified buffer (#166).
    ///
    /// Emacs's `C-x s` without its question for each buffer -- meowr has no
    /// yes-or-no read, and what this exists for is the forty buffers a rename
    /// leaves (ADR-0013 amended). Each buffer's `before-save` runs with it
    /// current; one that cannot be written is named in the sentence said once
    /// the writes are back, rather than stopping the rest. That includes a
    /// `*scratch*`, which Emacs passes over in silence: `SPC q q` names it among
    /// the unsaved, and a save that did not would leave the quit refusing for a
    /// reason nobody was told.
    fn save_some_buffers(&mut self) -> Action {
        let modified = self.modified_buffers();
        if modified.is_empty() {
            self.say("No buffers need saving.");
            return Action::Redraw;
        }
        let (writes, unsaveable): (Vec<_>, Vec<_>) = modified
            .into_iter()
            .map(|id| {
                self.in_buffer(id, |session| {
                    let name = session.current().name().to_string();
                    let unwritable = |why: Unwritable| format!("{name} {}", why.briefly());
                    match session.unwritable_reason() {
                        Some(why) => Err(unwritable(why)),
                        None => {
                            let _ = session.run_hooks(HookPoint::BeforeSave);
                            session
                                .save_target()
                                .ok_or_else(|| unwritable(Unwritable::Unread))
                        }
                    }
                })
            })
            .partition(Result::is_ok);
        let writes: Vec<Write> = writes.into_iter().filter_map(Result::ok).collect();
        let nothing_to_write = writes.is_empty();
        self.saving_some = Some(SavingSome {
            writes,
            unsaveable: unsaveable.into_iter().filter_map(Result::err).collect(),
        });
        if nothing_to_write {
            return self.saved_some(Vec::new());
        }
        Action::SaveSome
    }

    /// The writes `save-some-buffers` asked for. Drained.
    pub fn pending_saves(&mut self) -> Vec<Write> {
        self.saving_some
            .as_mut()
            .map(|saving| std::mem::take(&mut saving.writes))
            .unwrap_or_default()
    }

    /// How each write `save-some-buffers` asked for went, by path.
    ///
    /// Each one that landed is marked saved with its buffer current, so its
    /// `after-save` hooks act on it and its format-on-save is queued; then one
    /// sentence says what was saved and what was not, out loud if anything
    /// was not.
    pub fn saved_some(&mut self, results: Vec<(String, Result<(), String>)>) -> Action {
        let failed = results.iter().filter_map(|(path, result)| {
            result
                .as_ref()
                .err()
                .map(|why| format!("could not write {path}: {why}"))
        });
        let unsaved: Vec<String> = self
            .saving_some
            .take()
            .map(|saving| saving.unsaveable)
            .unwrap_or_default()
            .into_iter()
            .chain(failed)
            .collect();
        let saved: Vec<BufferId> = results
            .iter()
            .filter(|(_, result)| result.is_ok())
            .filter_map(|(path, _)| self.buffer_visiting(path))
            .collect();
        for id in &saved {
            self.in_buffer(*id, |session| session.marked_saved(false));
        }
        let count = saved.len();
        let noun = buffers(count);
        // A `SPC q F` asked for these saves on its way to killing everything,
        // and has the rest of the job and its own sentence to finish (#176).
        if let Some(killing) = self.killing_all.take() {
            return self.kill_all_after_saving(killing, count, unsaved);
        }
        if unsaved.is_empty() {
            self.say(&format!("Saved {count} {noun}."));
            Action::Redraw
        } else {
            self.refuse_aloud(format!("Saved {count} {noun}; {}.", unsaved.join("; ")))
        }
    }

    /// A save-time format that landed, for the host to write again.
    pub fn quiet_save_target(&mut self) -> Option<Write> {
        let path = self.pending_quiet_write.take()?;
        let buffer = self.buffer(self.buffer_visiting(&path)?);
        Some(Write {
            contents: buffer.document()?.text().to_string(),
            path,
            fresh: false,
        })
    }

    /// The host wrote a save-time format back. **No hooks**, and no format:
    /// it is not a second save anybody asked for, and `after-save` has already
    /// run for the one they did (ADR-0014).
    /// A write that had to make the directory it was going into (#181).
    ///
    /// #172's rule for a copy and a move -- "missing parent directories are
    /// created and said" -- for the one other act that can meet a directory
    /// that is not there. Said in that rule's own words, `, creating {it}`, so
    /// the three rows sound like one editor.
    ///
    /// Only the row you pressed says it. `save-some-buffers` has a sentence of
    /// its own that counts buffers, and the write that follows a format is
    /// deliberately quiet; a directory made under either is made under a file
    /// that was already going there.
    pub fn saved_making_directories(&mut self, path: &str) {
        let Some(directory) = directory_of(path) else {
            return;
        };
        self.say(&format!("Saved {path}, creating {directory}."));
    }

    pub fn mark_saved_quietly(&mut self, path: &str) {
        if let Some(id) = self.buffer_visiting(path) {
            let buffer = &mut self.buffers[id.0];
            buffer.history.mark_saved();
            buffer.since_autosave = 0;
        }
    }

    pub fn path(&self) -> Option<&str> {
        self.current().path()
    }

    /// A name typed at save-as, given the host's one spelling once the write
    /// under it has landed (#150): `./new.rs` and `new.rs` would otherwise be
    /// two buffers the next time anything named the file. Acts on whichever
    /// buffer has the name, which need not be the one on screen -- a
    /// `save-some-buffers` writes buffers that are not shown.
    pub fn respell_path(&mut self, from: &str, to: &str) {
        if from != to
            && let Some(id) = self.buffer_visiting(from)
        {
            let languages = Arc::clone(&self.languages);
            self.buffers[id.0].set_path(to, &languages);
        }
    }

    pub fn set_path(&mut self, path: &str) {
        let languages = Arc::clone(&self.languages);
        self.current_mut().set_path(path, &languages);
    }

    pub fn languages(&self) -> Arc<Languages> {
        Arc::clone(&self.languages)
    }

    /// `read-only-mode`, Doom's `SPC t r`.
    ///
    /// **Not a mutating command.** A mutating command is refused on a
    /// read-only buffer before it is dispatched, so marking this one mutating
    /// would let it turn read-only on and never off — and off is the
    /// direction that matters.
    ///
    /// A buffer that is read-only by construction is refused rather than
    /// toggled, and told so: the narrowing preview holds a file nobody chose
    /// to open and `*config-error*` holds a report, and a key that silently
    /// did nothing on either would read as broken.
    pub(crate) fn toggle_read_only(&mut self) -> Action {
        let buffer = self.current_mut();
        buffer.read_only = match buffer.read_only {
            ReadOnly::No => ReadOnly::Asked,
            ReadOnly::Asked => ReadOnly::No,
            ReadOnly::Always => {
                let name = buffer.name().to_string();
                self.signals += 1;
                self.echo = Some(format!("{name} is read-only because of what it is."));
                return Action::Redraw;
            }
        };
        let name = buffer.name().to_string();
        self.echo = Some(match buffer.read_only {
            ReadOnly::No => format!("{name} is writable."),
            _ => format!("{name} is read-only."),
        });
        Action::Redraw
    }

    pub fn set_read_only(&mut self, read_only: bool) {
        self.current_mut().read_only = if read_only {
            ReadOnly::Asked
        } else {
            ReadOnly::No
        };
    }

    /// Take the next parse due in **any** buffer, not only the visible one: a
    /// buffer in a background window still needs its colour.
    pub fn take_reparse(&mut self) -> Option<(BufferId, Reparse)> {
        for (index, buffer) in self.buffers.iter_mut().enumerate() {
            if let Some(job) = buffer.take_reparse() {
                return Some((BufferId(index), job));
            }
        }
        None
    }

    /// The current buffer's revision, so a caller can tag what it asks for.
    ///
    /// An answer computed against text the buffer has since moved past is
    /// refused on arrival, and the tag is how that is decided — the same
    /// contract `take_reparse`/`apply_reparse` already run on.
    pub fn revision(&self) -> u64 {
        self.current().revision
    }

    /// What a language server said about a buffer, or a refusal if the buffer
    /// has moved on since it was asked.
    pub fn set_diagnostics(
        &mut self,
        buffer: BufferId,
        revision: u64,
        diagnostics: Vec<meowr_domain::Diagnostic>,
    ) -> Action {
        match self.buffers.get_mut(buffer.0) {
            Some(b) => {
                if b.set_diagnostics(revision, diagnostics) {
                    Action::Redraw
                } else {
                    Action::Nothing
                }
            }
            None => Action::Nothing,
        }
    }

    /// The current buffer's diagnostics.
    pub fn diagnostics(&self) -> &[meowr_domain::Diagnostic] {
        self.current().diagnostics()
    }

    pub fn apply_reparse(
        &mut self,
        buffer: BufferId,
        revision: u64,
        syntax: Option<Syntax>,
    ) -> Action {
        match self.buffers.get_mut(buffer.0) {
            Some(b) => {
                if b.apply_reparse(revision, syntax) {
                    Action::Redraw
                } else {
                    Action::Nothing
                }
            }
            None => Action::Nothing,
        }
    }

    pub fn thing_range(&self, pos: usize, name: &str, extent: Extent) -> Option<Range<usize>> {
        let buffer = self.current();
        let doc = buffer.document()?;
        resolve_thing(
            &self.things,
            buffer
                .syntax
                .as_ref()
                .map(|syntax| meowr_syntax::TextObjects {
                    syntax,
                    languages: &self.languages,
                })
                .as_ref(),
            self.window_range(),
            doc,
            pos,
            name,
            extent,
        )
    }

    /// The visible region of the focused window, as byte offsets.
    fn window_range(&self) -> Option<Range<usize>> {
        let doc = self.current().document()?;
        let viewport = self.viewport();
        let start = doc.line_to_byte(viewport.top)?;
        let end = doc
            .line_to_byte(viewport.top + viewport.height)
            .unwrap_or_else(|| doc.byte_len());
        Some(start..end)
    }

    pub fn viewport(&self) -> Viewport {
        let window = self.windows.current();
        text_area(window.area).viewport(window.top)
    }

    // --- what a window shows ----------------------------------------------

    /// The rows of one window.
    pub fn rows_of(&self, id: WindowId) -> Vec<Row> {
        let window = self.windows.window(id);
        let buffer = &self.buffers[window.buffer.0];
        match buffer.document() {
            Some(doc) => rows_with(
                doc,
                text_area(window.area).viewport(window.top),
                Some(buffer.selection.current()),
                // Focused only, on a buffer that is yours, and only if the
                // Option is on. All three are facts about this session rather
                // than about the text, which is why the view is told rather
                // than asked to work them out.
                //
                // `is_real` excludes a narrowing preview, which is meowr's
                // answer to the `(doom-temp-buffer-p b)` in Doom's own
                // predicate: a file nobody chose to open, shown behind the
                // widget, where a band would point at a line in a buffer you
                // are not editing.
                self.options.highlight_line && id == self.windows.focus() && buffer.is_real(),
                // The buffer's, not the window's: a fold is a Buffer property
                // (ADR-0012), so the same buffer in two Windows is folded the
                // same in both.
                buffer.folds(),
            ),
            None => Vec::new(),
        }
    }

    pub fn rows(&self) -> Vec<Row> {
        self.rows_of(self.windows.focus())
    }

    /// How wide one window's line-number gutter should be, or `None` when
    /// there is nothing to draw one over.
    ///
    /// Off with the buffer rather than with the window: a Guest has no lines
    /// to number and neither does one still loading, and both answer `None`
    /// here the same way `rows_of` answers `Vec::new()` for them — the two
    /// draw together and disagree about a buffer's shape at their own risk.
    /// Unlike `hl-line`, not gated on focus: Emacs numbers every window's
    /// lines, and the option that turns numbering off is the only gate.
    pub fn gutter_of(&self, id: WindowId) -> Option<usize> {
        if !self.options.line_numbers {
            return None;
        }
        let window = self.windows.window(id);
        let buffer = &self.buffers[window.buffer.0];
        buffer.document().map(|doc| gutter_width(doc.line_count()))
    }

    /// The highlighted runs of one window, placed on its grid.
    pub fn styled_of(&self, id: WindowId) -> Vec<Styled> {
        let window = self.windows.window(id);
        let buffer = &self.buffers[window.buffer.0];
        let (Some(doc), Some(syntax)) = (buffer.document(), buffer.syntax.as_ref()) else {
            return Vec::new();
        };
        let viewport = text_area(window.area).viewport(window.top);
        let first = doc.line_to_byte(viewport.top).unwrap_or(0);
        let last = doc
            .line_to_byte(viewport.top + viewport.height)
            .unwrap_or_else(|| doc.byte_len());
        let spans: Vec<Span> =
            meowr_syntax::spans(syntax, doc.text().slice(..), &self.languages, first..last);
        styled(doc, viewport, &spans)
    }

    pub fn styled(&self) -> Vec<Styled> {
        self.styled_of(self.windows.focus())
    }

    /// The Grab, placed on a window's grid, so it can be seen persisting.
    pub fn grabbed_of(&self, id: WindowId) -> Vec<Styled> {
        let window = self.windows.window(id);
        let buffer = &self.buffers[window.buffer.0];
        match buffer.document() {
            Some(doc) => meowr_view::grabbed(
                doc,
                text_area(window.area).viewport(window.top),
                buffer.grab(),
            ),
            None => Vec::new(),
        }
    }

    /// Diagnostics, placed on a window's grid.
    ///
    /// The same shape `grabbed_of` is: the session holds the ranges, the view
    /// layer turns them into rows and columns, and the render layer decides
    /// what a squiggle looks like.
    pub fn diagnostics_of(&self, id: WindowId) -> Vec<Styled> {
        let window = self.windows.window(id);
        let buffer = &self.buffers[window.buffer.0];
        match buffer.document() {
            Some(doc) => meowr_view::diagnostics(
                doc,
                text_area(window.area).viewport(window.top),
                buffer.diagnostics(),
            ),
            None => Vec::new(),
        }
    }

    /// The State, as the modeline writes it.
    ///
    /// One place the State is spelled, rather than each modeline spelling it
    /// again — which is how two spellings come to disagree.
    fn state_name(&self) -> &'static str {
        self.state().name()
    }

    /// The modeline for one window, fitted to its width.
    ///
    /// Per window, because a modeline describes *a window's* buffer: with two
    /// windows on one file the point and the selection differ between them,
    /// and one shared line could only be right about one of them.
    pub fn modeline_of(&self, id: WindowId) -> Fitted {
        let window = self.windows.window(id);
        let buffer = &self.buffers[window.buffer.0];
        let selection = buffer.selection.current();
        let typed = selection.selection_type();
        let doc = buffer.document();
        let (line, column, lines) = match doc {
            Some(doc) => {
                let line = doc.byte_to_line(selection.pos()).unwrap_or(0);
                let start = doc.line_to_byte(line).unwrap_or(0);
                let column =
                    doc.text().byte_to_char(selection.pos()) - doc.text().byte_to_char(start);
                (line, column, doc.line_count())
            }
            None => (0, 0, 0),
        };
        let range = selection.range();
        let (selection_lines, selection_chars) = match doc {
            Some(doc) if typed.is_some() => {
                let first = doc.byte_to_line(range.start).unwrap_or(0);
                // `end - 1` names the last byte *inside* the range, so an
                // empty range has no such byte — and asking anyway answers
                // with the line before, which is one fewer than `first` and
                // underflows the subtraction below. `L` onto an empty line
                // makes exactly that selection, and so does Emacs — checked,
                // not recalled: on the blank line of "one\n\nthree\n" both
                // `line-beginning-position` and `line-end-position` answer 5.
                let last = if range.is_empty() {
                    first
                } else {
                    doc.byte_to_line(range.end - 1).unwrap_or(first)
                };
                (
                    last - first + 1,
                    doc.text().byte_to_char(range.end) - doc.text().byte_to_char(range.start),
                )
            }
            _ => (0, 0),
        };
        let status = Status {
            state: self.state_name(),
            expand_hints: if id == self.windows.focus() {
                buffer.hints.len()
            } else {
                0
            },
            path: buffer.path(),
            buffer_name: buffer.name(),
            modified: buffer.is_modified(),
            read_only: buffer.read_only.refuses_edits(),
            line,
            lines,
            column,
            selection: typed.map(|(axis, kind)| {
                (
                    match axis {
                        Axis::Expand => "expand",
                        Axis::Select => "select",
                    },
                    kind.name(),
                )
            }),
            selection_lines,
            selection_chars,
            branch: self.branch.as_deref(),
            encoding: "UTF-8",
            language: buffer.language.and_then(|l| self.languages.name_of(l)),
            diagnostics: buffer.diagnostics().len(),
            focused: id == self.windows.focus(),
        };
        fit(&modeline(&status), window.area.width as usize)
    }

    /// `meow-search`: the head of the search ring, in the direction given.
    ///
    /// A selection that is not already the search target is pushed onto the
    /// ring first, which is what makes `w v` mean "find the next one of
    /// these" without typing it.
    pub fn search(&mut self, forward: bool) -> Action {
        let selected = {
            let buffer = self.current();
            let current = buffer.selection.current();
            match (buffer.document(), current.selection_type()) {
                (Some(doc), Some(_)) => {
                    let text = doc.text();
                    let range = current.range();
                    Some(
                        text.slice(text.byte_to_char(range.start)..text.byte_to_char(range.end))
                            .to_string(),
                    )
                }
                _ => None,
            }
        };
        if let Some(text) = selected
            && !text.is_empty()
            && self.search_ring.first() != Some(&text)
        {
            self.push_search(text);
        }
        let Some(needle) = self.search_ring.first().cloned() else {
            // Emacs's `isearch-repeat` with an empty ring: "No previous
            // search string". Nothing has been searched for and nothing is
            // selected to search for, so there is no needle to look for.
            return self.refuse_aloud("No previous search string.");
        };
        let buffer = self.current_mut();
        let Content::Ready(doc) = &buffer.content else {
            return Action::Nothing;
        };
        let found = core::search(doc, &mut buffer.selection, &needle, forward);
        buffer.after_command(10);
        self.scroll_into_view();
        if !found {
            self.signals += 1;
        }
        Action::Redraw
    }

    /// The search ring, newest first.
    pub fn search_ring(&self) -> &[String] {
        &self.search_ring
    }

    pub fn push_search(&mut self, text: String) {
        self.search_ring.retain(|s| *s != text);
        self.search_ring.insert(0, text);
        self.search_ring.truncate(32);
    }

    /// Doom's `save-buffers-kill-terminal`, which is what `SPC q q` and `C-c`
    /// reach.
    ///
    /// Emacs prompts for each modified buffer. meowr has no yes/no read, so it
    /// refuses instead and says in the Echo Area what is holding it up — which
    /// the glossary has always allowed: the Echo Area is "used for messages,
    /// prompts, and the narrowing interface". Doom binds the way past in the
    /// same prefix: `SPC q Q` is `kill-emacs`, "quit without saving".
    ///
    /// The keys in that message are looked up, not written out. A binding
    /// named in prose is a second source for the keymap's fact, and Ring 3
    /// exists to let those bindings move.
    fn quit(&mut self) -> Action {
        let Some(unsaved) = self.unsaved() else {
            return Action::Quit;
        };
        self.refuse_unsaved(
            &unsaved.named,
            unsaved.savable,
            WayPast::Command("quit-without-saving"),
            "quit anyway",
        )
    }

    /// The work a quit would lose, or `None` when there is nothing in the way.
    ///
    /// Shared by the two rows that end the session -- quitting and restarting --
    /// because they are stopped by the same thing and say so in the same
    /// sentence (#182).
    fn unsaved(&self) -> Option<Unsaved> {
        let modified: Vec<&Buffer> = self
            .buffers
            .iter()
            .filter(|buffer| buffer.is_modified())
            .collect();
        if modified.is_empty() {
            return None;
        }
        let names: Vec<&str> = modified.iter().map(|buffer| buffer.name()).collect();
        Some(Unsaved {
            named: unsaved_sentence(&names),
            savable: modified.iter().all(|buffer| buffer.path().is_some()),
        })
    }

    /// Doom's `SPC q r`, which is `doom/restart`.
    ///
    /// Stopped by exactly what a quit is stopped by, and past it by `SPC u`
    /// rather than by a second command: #172 and #173 settled that idiom for
    /// the rows that cannot be undone, and this is one -- what is on screen
    /// does not come back (#175).
    ///
    /// The `exec` itself is the host's, because the terminal has to be handed
    /// back before it. See [`Action::Restart`].
    fn restart(&mut self) -> Action {
        // Taken before the refusal below, for the reason `ask_file_work` takes
        // it first: a go-ahead left behind by a refused press is one the *next*
        // press would find, and restart on a keystroke meant for this one.
        let go_ahead = self.take_prefix();
        match self.unsaved() {
            None => Action::Restart,
            Some(_) if go_ahead => Action::Restart,
            Some(unsaved) => self.refuse_unsaved(
                &unsaved.named,
                unsaved.savable,
                WayPast::GoAhead("restart"),
                "restart anyway",
            ),
        }
    }

    /// Refuse something that would lose unsaved work, and say what means it.
    ///
    /// Emacs prompts; meowr has no yes/no read, so both refusals it makes —
    /// quitting and killing a buffer — take this shape instead: say what is in
    /// the way, then name the key that goes ahead regardless. The keys are
    /// looked up rather than written out, because a binding named in prose is a
    /// second source for the keymap's fact and Ring 3 exists to move bindings.
    ///
    /// `savable` is false where writing would do nothing — `*scratch*` has no
    /// path — because advising a save that cannot happen is a dead end.
    fn refuse_unsaved(
        &mut self,
        what: &str,
        savable: bool,
        way_past: WayPast<'_>,
        force_reads: &str,
    ) -> Action {
        self.signals += 1;
        // The leader route by preference, which is what this named before
        // `written` existed. `write-file` below wants the other question —
        // the shortest way — because `C-x C-s` is the answer on the defaults
        // and a message is not the place to campaign against a chord.
        let force = match way_past {
            WayPast::Command(command) => self.reached_by(command),
            WayPast::GoAhead(command) => self.go_ahead_for(command),
        };
        let save = savable
            .then(|| {
                self.written("write-file")
                    .map(|keys| format!("Save with {keys}, or "))
            })
            .flatten()
            .unwrap_or_default();
        self.echo = Some(format!("{what} {save}{force} to {force_reads}."));
        Action::Redraw
    }

    /// Doom's `kill-emacs`, on `SPC q Q`: the way past [`Session::quit`].
    fn quit_without_saving(&mut self) -> Action {
        Action::Quit
    }

    fn kill_buffer(&mut self) -> Action {
        self.kill_current(false)
    }

    /// The way past [`Session::kill_buffer`], as `SPC q Q` is past `SPC q q`.
    fn kill_buffer_without_saving(&mut self) -> Action {
        self.kill_current(true)
    }

    fn kill_current(&mut self, force: bool) -> Action {
        let id = self.current_buffer_id();
        let buffer = &self.buffers[id.0];
        let discarding = buffer.is_modified();
        if discarding && !force {
            // The shape `Session::quit` already uses, and for the same reason:
            // Emacs asks a yes-or-no question here and meowr has no yes/no read.
            let name = buffer.name().to_string();
            let savable = buffer.path().is_some();
            return self.refuse_unsaved(
                &format!("{name} is modified."),
                savable,
                WayPast::Command("kill-buffer-without-saving"),
                "kill it anyway",
            );
        }
        self.discard(id);
        Action::Redraw
    }

    /// Replace a buffer with a tombstone and point every window that showed it
    /// somewhere else.
    ///
    /// Split out of `kill_current` so that every way a buffer can end closes
    /// it the same way `SPC b k` does. A second copy of this would be a second
    /// chance to forget that the buffer can be on screen in more than one
    /// window, and that a parked point into it is an offset into text that no
    /// longer exists.
    fn entomb(&mut self, id: BufferId) {
        self.buffers[id.0] = Buffer::tombstone();
        // The preview slot is deliberately not considered here. #52 cleared it,
        // guessing that a killed preview buffer could be resurrected by the next
        // `previewed`; #53 showed the branch was reachable only through its own
        // defect. Every way out of narrowing now leaves the preview buffer —
        // cancelling restores, a `buffers` choice shows, a file choice promotes
        // or opens, a `line` choice restores — so the preview slot is never the
        // buffer this acts on, and a branch for it would be dead code that no
        // test could reach. It comes back with a test if killing ever takes a
        // name instead of meaning "this one".
        // Emacs never has zero buffers and neither can this: the renderer asks
        // the session what the focused window shows on every frame.
        let replacement = self.live_buffer_after(id).unwrap_or_else(|| {
            let fresh = BufferId(self.buffers.len());
            self.buffers.push(Buffer::scratch());
            fresh
        });
        // Every window, not just the focused one: the buffer being killed can
        // be on screen in several.
        for window_id in self.windows.order() {
            let window = self.windows.window_mut(window_id);
            if window.buffer == id {
                window.buffer = replacement;
                window.top = 0;
            }
            // A parked point belonging to the killed buffer is an offset into
            // text that no longer exists.
            if matches!(window.point, Some((held, _)) if held == id) {
                window.point = None;
            }
        }
    }

    /// The next buffer after `from` that someone actually opened.
    ///
    /// Wraps, and skips both kinds of slot that are not buffers: a killed one,
    /// and the one previewing reads into.
    fn live_buffer_after(&self, from: BufferId) -> Option<BufferId> {
        let count = self.buffers.len();
        (1..=count)
            .map(|step| (from.0 + step) % count)
            .find(|&index| self.buffers[index].is_real())
            .map(BufferId)
    }

    /// Kill `id`, taking its auto-save with it if work is being thrown away.
    ///
    /// The bookkeeping around [`Session::entomb`], split out for the reason
    /// `entomb` itself was: the question is about the *work* being lost rather
    /// than about which buffer is on screen, so it belongs to every way of
    /// losing one — and `SPC b O` loses several at once, none of them the
    /// buffer in the window.
    ///
    /// The auto-save goes **only** where work is actually being thrown away.
    /// One beside an *unmodified* buffer is not that buffer's leavings — it is
    /// #37 doing its job, unrecovered work from a crash — and the keys that
    /// refuse when work is at risk must not be the ones that silently delete
    /// the last copy of some. `SPC b K` and `SPC b O` are the decisions to
    /// lose work, and they take the auto-save with them.
    fn discard(&mut self, id: BufferId) {
        let buffer = &self.buffers[id.0];
        if let Some(path) = buffer.path().filter(|_| buffer.is_modified()) {
            self.autosaves_to_forget.push(autosave_path(path));
        }
        self.entomb(id);
    }

    /// `kill-buffer-and-window`, Doom's `SPC b D`.
    ///
    /// Emacs kills the buffer first and deletes the window after, then re-runs
    /// `delete-window` when it failed "to regenerate the error so it can be
    /// seen in the echo area". Both halves are kept.
    ///
    /// The refusal in front is [`Session::kill_buffer`]'s own and the window is
    /// not touched when it fires: a `D` that declined the kill and closed the
    /// window anyway would take away the place the work is shown while keeping
    /// the work, which is the worst of the two answers rather than either.
    ///
    /// The note behind it rings, where most of meowr's do not. That is Emacs's
    /// choice at this exact condition — `delete-window` raises rather than
    /// reports — and it is the one `SPC w d` already makes on the same
    /// sentence. Without it `b D` is indistinguishable from `b d` in the only
    /// layout where the two differ.
    fn kill_buffer_and_window(&mut self) -> Action {
        let id = self.current_buffer_id();
        let refused = self.kill_current(false);
        if !self.buffers[id.0].is_killed() {
            return refused;
        }
        // Asked before parking, not after failing to close. `park_point` and
        // `take_up_point` are a pair — `window_command` runs both around every
        // focus change — and an early return between them would leave a point
        // parked that nothing takes up.
        if self.window_count() == 1 {
            return self.refuse_aloud("This is the only window.");
        }
        self.park_point();
        self.windows.close();
        self.windows.layout(self.area);
        self.take_up_point();
        Action::Redraw
    }

    /// `doom/kill-other-buffers`, on `SPC b o`.
    ///
    /// Doom maps each buffer through `doom-kill-buffer-and-windows` and so
    /// closes the windows the killed buffers were in. This does not, and the
    /// divergence is inherited rather than chosen here: every kill in meowr
    /// goes through [`Session::entomb`], which *retargets* a window at another
    /// buffer instead of closing it (#52). Diverging at this one command would
    /// buy Doom's layout at the price of a second way to kill a buffer, which
    /// is the thing `entomb` exists to prevent. Two windows left showing one
    /// buffer is what `SPC w m` is for.
    fn kill_other_buffers(&mut self) -> Action {
        self.kill_others(false)
    }

    /// The way past [`Session::kill_other_buffers`], as `SPC b K` is past
    /// `SPC b k`.
    ///
    /// Not one of Doom's rows. Doom asks a yes-or-no question per modified
    /// buffer and meowr has no yes/no read — the shape `Session::quit` met
    /// first — so the decision is a key instead, and it is the capital of the
    /// one that refuses.
    fn kill_other_buffers_without_saving(&mut self) -> Action {
        self.kill_others(true)
    }

    fn kill_others(&mut self, force: bool) -> Action {
        let keep = self.current_buffer_id();
        let others: Vec<BufferId> = (0..self.buffers.len())
            .map(BufferId)
            .filter(|&id| id != keep && self.buffers[id.0].is_real())
            .collect();
        if !force {
            let modified: Vec<&str> = others
                .iter()
                .filter(|&&id| self.buffers[id.0].is_modified())
                .map(|&id| self.buffers[id.0].name())
                .collect();
            if !modified.is_empty() {
                // Named, all of them, because none is on screen: the buffer
                // `SPC b k` refuses over is the one you are looking at, and
                // these are the ones you are not. A count would be a refusal
                // you cannot act on without hunting for what it meant.
                //
                // `Session::quit`'s sentence, and now literally it: the set
                // differs -- these are the buffers you are *not* looking at --
                // so the list is this row's and the wording is everyone's.
                // Never savable: `write-file` writes the buffer in the window,
                // which is the one buffer this command is not about.
                return self.refuse_unsaved(
                    &unsaved_sentence(&modified),
                    false,
                    WayPast::Command("kill-other-buffers-without-saving"),
                    "kill them anyway",
                );
            }
        }
        if others.is_empty() {
            // Doom's own command reports a count and would say "Killed 0" here.
            // Silence is what #80 and #88 spent two tickets removing, and this
            // is the shape neither could see: nothing failed, so there is no
            // beep and no `Outcome::Failed` for the sweep to catch — just a key
            // that did nothing and said nothing.
            return self.refuse("No other buffers.");
        }
        let killed = others.len();
        for id in others {
            self.discard(id);
        }
        // `doom/kill-other-buffers` messages "Killed %d other buffers", and the
        // count is most of the feedback: the buffers that went were the ones
        // this window was not showing. Not *all* of it — a second window on a
        // killed buffer is repointed rather than closed (see this command's
        // doc), so that window's contents change under you and the count is
        // what says why.
        self.echo = Some(format!("Killed {killed} other {}.", buffers(killed)));
        Action::Redraw
    }

    /// `doom/kill-all-buffers`, on `SPC q F` (#176).
    ///
    /// Doom's order: `save-some-buffers`, `delete-other-windows`, then the
    /// buffers, then a count. The first of those is the host's half -- meowr
    /// writes every modified buffer rather than asking about each, and the
    /// writes go out as `Action::SaveSome` -- so the rest of the job waits in
    /// [`Session::killing_all`] until [`Session::saved_some`] brings them
    /// back. Killing a buffer whose write was still in flight would be killing
    /// it before anyone could be told the write had failed.
    fn kill_all_buffers(&mut self) -> Action {
        // First, and before the guard in `kill_all_after_saving`: a go-ahead
        // left behind by a refused press is one the next press would spend
        // without asking (#179).
        let forced = self.take_prefix();
        // Not from a hook. The saves are a round trip through the host, and
        // the rest of the job waits across it -- so a hook doing this would
        // be asking the editor to finish a job after the hook had ended.
        // Refused in front rather than left to the dispatch guard, because
        // that is what makes `killing_all` something only a key can set.
        if self.in_hook {
            return self.refuse_aloud("`kill-all-buffers` cannot run from a hook.");
        }
        let killing = KillAll { forced };
        // This check is load-bearing rather than an optimisation, and it is
        // `save_some_buffers`'s own: that command says "No buffers need
        // saving" and returns *without* reaching `saved_some`, so a
        // `killing_all` set in front of it would be left behind to be found
        // and spent by the next unrelated `save-some-buffers` -- which would
        // then kill every buffer. `killing_all` is set only where the funnel
        // is certain to be reached.
        if self.modified_buffers().is_empty() {
            return self.kill_all_after_saving(killing, 0, Vec::new());
        }
        self.killing_all = Some(killing);
        self.save_some_buffers()
    }

    /// The buffers, once the saves `SPC q F` asked for are back (#176).
    ///
    /// Whatever is still modified here could not be written at all -- it has
    /// no file, or is read-only, or had not finished loading -- so killing it
    /// throws away the only copy of that work. That is the refusal shape #173
    /// decided and the one `SPC q q` and `SPC b k` already take: say what is
    /// in the way, name the key that means yes, and touch nothing. The saves
    /// have happened by then, which is Doom's order too and costs nothing: a
    /// write is not what anybody needs protecting from.
    fn kill_all_after_saving(
        &mut self,
        killing: KillAll,
        saved: usize,
        unsaved: Vec<String>,
    ) -> Action {
        // One fragment, built once, so that a zero is suppressed everywhere
        // rather than on whichever path remembered to. "Saved 0 buffers" is
        // not news on any of them.
        let saves = match saved {
            0 => String::new(),
            n => format!("Saved {n} {}, ", buffers(n)),
        };
        if !unsaved.is_empty() && !killing.forced {
            // Named rather than counted, as `kill-others` names them: none of
            // these is necessarily on screen, and a count is a refusal you
            // cannot act on without hunting for what it meant.
            return self.refuse_aloud(format!(
                "{saves}{}. {} to kill them anyway.",
                unsaved.join("; "),
                self.go_ahead_for("kill-all-buffers"),
            ));
        }
        // Doom's `delete-other-windows`, and before the buffers: a window on a
        // buffer that is about to go would be repointed by `entomb` only to be
        // closed a moment later.
        self.windows.close_others();
        // `close_others` rewrites the tree and `layout` is what hands the
        // survivor the space, which is why every window command does both.
        // Without it the last window keeps the half of the frame it had while
        // it was split, and the other half is drawn from whatever was there.
        self.windows.layout(self.area);
        let all: Vec<BufferId> = self.real_buffers().collect();
        let killed = all.len();
        for id in all {
            self.discard(id);
        }
        // The fallback `*scratch*` `entomb` makes when nothing live is left is
        // not counted: it is what you are looking at now, not something that
        // went.
        let kills = match saved {
            0 => format!("Killed {killed} {}", buffers(killed)),
            _ => format!("{saves}killed {killed}"),
        };
        // What `SPC u` went ahead over, named in the same breath and by the
        // same joining as the refusal names it. Said even when nothing
        // refused first: a go-ahead pressed straight off is one nobody was
        // warned about, and work thrown away is the part of what happened
        // that matters most.
        let said = match unsaved.is_empty() {
            true => format!("{kills}."),
            false => format!("{kills}; {}.", unsaved.join("; ")),
        };
        self.say(&said);
        Action::Redraw
    }

    /// The scratch buffer, made if there is not one yet.
    ///
    /// Found rather than created, because Emacs has one `*scratch*` and not
    /// one per press: a second buffer wearing the name is one the modeline
    /// cannot tell from the first and one `switch-to-buffer` offers twice
    /// under a single label.
    ///
    /// **Doom's two rows already reach one buffer.** `doom/switch-to-scratch-
    /// buffer` *is* `doom/open-scratch-buffer` with `same-window`, and both go
    /// through `doom-scratch-buffer` — so the only difference between them
    /// upstream is `switch-to-buffer` against `pop-to-buffer`, which is
    /// exactly the difference kept here. The per-project one is a third
    /// command (`doom/open-project-scratch-buffer`) and neither row is bound
    /// to it.
    ///
    /// What does not port is the buffer itself. Doom's is `*doom:scratch*` and
    /// is written to `doom-scratch-dir` on exit, so it survives the session;
    /// meowr's is `*scratch*` and does not, because nothing here persists a
    /// buffer that visits no file. The name follows the behaviour: calling it
    /// `*doom:scratch*` would promise the persistence.
    ///
    /// Usually it is already there. `Session::entomb` makes one when the last
    /// live buffer is killed, and meowr started with no file argument is
    /// sitting in one from the first frame — `main` loads an empty `Document`
    /// into the buffer the session starts with, which is what makes that
    /// buffer scratch rather than merely empty.
    fn scratch_buffer(&mut self) -> BufferId {
        if let Some(found) = self.buffers.iter().position(Buffer::is_the_scratch_buffer) {
            return BufferId(found);
        }
        let fresh = BufferId(self.buffers.len());
        self.buffers.push(Buffer::scratch());
        fresh
    }

    /// `doom/switch-to-scratch-buffer`, on `SPC b x` and `SPC f X` (#180).
    fn switch_to_scratch_buffer(&mut self) -> Action {
        let id = self.scratch_buffer();
        self.show_scratch(id, ScratchRow::Switch)
    }

    /// `doom/open-scratch-buffer`, on `SPC b X`: the same buffer, in a window
    /// of its own.
    ///
    /// Doom pops it up, and `doom/switch-to-scratch-buffer` is that same
    /// command told to use the current window instead — which is the whole
    /// difference between the two rows. Nothing floats here (ADR-0004), so the
    /// popup is a split, and it splits *below* because that is where Doom puts
    /// a popup and where the author's own `+magit-open-windows-in-direction`
    /// says they like one.
    fn open_scratch_buffer(&mut self) -> Action {
        let id = self.scratch_buffer();
        self.open_scratch_in_a_split(id)
    }

    /// `SPC b X`'s half of the work, for whichever scratch it is about (#180).
    fn open_scratch_in_a_split(&mut self, id: BufferId) -> Action {
        self.park_point();
        // `pop-to-buffer` reuses a window already displaying the buffer before
        // it makes one, and Doom's popup rules share a slot on top of that. A
        // split per press would answer the second `SPC b X` with a third
        // window showing what the second one already had.
        //
        // The focused window counts, which is why this is not `w != focus`:
        // the second press usually arrives *from* the scratch window, and
        // excluding it is exactly how the stacking gets back in.
        match self.window_showing(id) {
            Some(showing) => self.windows.set_focus(showing),
            None => {
                self.windows.split(Direction::Vertical);
            }
        }
        self.show(id);
        self.windows.layout(self.area);
        self.take_up_point();
        Action::Redraw
    }

    /// `doom/toggle-scratch-buffer`, on `SPC f x` (#180).
    ///
    /// Doom deletes the scratch buffer's window when it has one and pops it up
    /// otherwise. ADR-0004 leaves nothing floating, so the popup is the split
    /// below that [`Session::open_scratch_buffer`] already makes, and the two
    /// rows differ only in what they do when the window is already there:
    /// `SPC b X` focuses it, and this closes it.
    ///
    /// The window is closed rather than the buffer killed. A toggle that killed
    /// would throw away whatever had been typed into it, which is the one thing
    /// a scratch buffer is for.
    fn toggle_scratch_buffer(&mut self) -> Action {
        let id = self.scratch_buffer();
        self.show_scratch(id, ScratchRow::Toggle)
    }

    /// `doom/toggle-project-scratch-buffer` and
    /// `doom/open-project-scratch-buffer`, on `SPC p x` and `SPC p X` (#180).
    ///
    /// The Project is ADR-0010's repository and the session never holds one, so
    /// this is the round trip every project row takes. The exception is being
    /// *in* a Project's scratch already: it carries its own root, so the row
    /// can act without asking -- which is what makes `SPC p x` able to toggle
    /// at all, since a scratch has no path for a Project to be found from.
    fn ask_project_scratch(&mut self, row: ScratchRow) -> Action {
        if let Some(root) = self.current().scratch_of().map(str::to_owned) {
            return self.show_project_scratch(&root, row);
        }
        let Some(about) = self.current().path().map(str::to_owned) else {
            // ADR-0010 names what is missing rather than guessing a directory,
            // and a buffer with no file has no Project to name.
            return self.refuse_aloud("This buffer has no file, so it is in no project.");
        };
        self.pending_from_root = Some(FromRoot::Scratch { about, row });
        Action::FromRoot
    }

    fn toggle_project_scratch_buffer(&mut self) -> Action {
        self.ask_project_scratch(ScratchRow::Toggle)
    }

    fn switch_to_project_scratch_buffer(&mut self) -> Action {
        self.ask_project_scratch(ScratchRow::Switch)
    }

    /// `SPC p x` and `SPC p X`'s answer: the Project's root, or why there is
    /// none (#180).
    ///
    /// Dropped if the buffer has moved on, as `path_from_root` drops one: a
    /// scratch for a repository you have since left is not what the key meant.
    pub fn scratch_from_root(
        &mut self,
        about: &str,
        row: ScratchRow,
        answer: Result<String, String>,
    ) -> Action {
        // Both halves of `file_answer`'s guard, not just the first: this
        // answer shows a buffer and may split a window, so a picker opened
        // while the host was looking would have a scratch appear underneath
        // it. `path_from_root` needs only the first half because a yank
        // touches nothing but the ring.
        if self.current().path() != Some(about) || self.narrow.widget.is_some() {
            return Action::Nothing;
        }
        match answer {
            Ok(root) => self.show_project_scratch(&root, row),
            Err(why) => self.refuse_aloud(why),
        }
    }

    /// The Project's scratch, found by root and made if it is not there.
    ///
    /// Found by root and not by name for the reason [`Buffer::scratch_of`]
    /// gives: two repositories can end in the same directory name.
    fn show_project_scratch(&mut self, root: &str, row: ScratchRow) -> Action {
        let id = match self
            .buffers
            .iter()
            .position(|buffer| buffer.scratch_of() == Some(root))
        {
            Some(found) => BufferId(found),
            None => {
                let mut fresh = Buffer::scratch();
                // Named for the Project, which is what makes it visibly not
                // the one `SPC f X` reaches.
                fresh.label = Some(format!("*scratch: {}*", basename(root)));
                fresh.scratch_of = Some(root.to_string());
                // Pushed rather than `open`ed, which would put it in this
                // window: how it is shown is the row's to say, and a toggle
                // that found it already on screen would close the window it
                // had just been put in.
                let id = BufferId(self.buffers.len());
                self.buffers.push(fresh);
                id
            }
        };
        self.show_scratch(id, row)
    }

    /// Show a scratch buffer the way `row` says, whichever scratch it is.
    ///
    /// The half of both pairs of rows that does not care which scratch it has:
    /// `SPC f x` and `SPC p x` toggle, `SPC f X` and `SPC p X` switch.
    fn show_scratch(&mut self, id: BufferId, row: ScratchRow) -> Action {
        let showing = match row {
            ScratchRow::Switch => {
                self.show(id);
                return Action::Redraw;
            }
            // Asked only here: which window has it is the toggle's question,
            // and `Switch` has this window whatever the answer.
            ScratchRow::Toggle => self.window_showing(id),
        };
        match showing {
            // Focused first, because `close` closes the focused window: a
            // toggle pressed from another window would otherwise close that
            // one and leave the scratch on screen.
            Some(showing) => {
                // The last window cannot be closed, and a scratch filling the
                // frame alone is not a popup to dismiss -- there would be
                // nothing to put back. Asked before the point is parked,
                // because a refusal touches nothing: `close` would answer the
                // same question, and answering it after parking leaves a
                // parked point behind on the way out.
                if self.windows.order().len() <= 1 {
                    return self.refuse_aloud("This is the only window.");
                }
                self.park_point();
                self.windows.set_focus(showing);
                self.windows.close();
                self.windows.layout(self.area);
                self.take_up_point();
                Action::Redraw
            }
            None => self.open_scratch_in_a_split(id),
        }
    }

    /// The window showing `id`, if one does.
    ///
    /// The focused window counts. Both scratch rows ask this and mean it: the
    /// second press usually arrives *from* the scratch window.
    fn window_showing(&self, id: BufferId) -> Option<window::WindowId> {
        self.windows
            .order()
            .into_iter()
            .find(|&w| self.windows.window(w).buffer == id)
    }

    /// `leader`: `SPC` opens Ring 2.
    fn open_leader(&mut self) -> Action {
        self.state = State::Keypad;
        self.keypad.keys.clear();
        self.keypad.menu_visible = false;
        Action::Redraw
    }

    fn visit(&mut self) -> Action {
        self.open_visit();
        Action::Redraw
    }

    fn find_file(&mut self) -> Action {
        self.open_picker()
    }

    /// Doom's `SPC .`. The picker, opened on the buffers source.
    fn switch_to_buffer(&mut self) -> Action {
        self.open_picker_on('b')
    }

    /// Doom's `SPC f y`, which is `+default/yank-buffer-path`.
    ///
    /// The path as the buffer holds it, not an absolute one: the session does
    /// no I/O and has nothing to resolve against — the binary owns the
    /// filesystem, as it owns the terminal. Doom's own relative-to-project
    /// variant is `SPC f Y` and waits on there being a project (#98).
    fn yank_buffer_path(&mut self) -> Action {
        let Some(path) = self.current().path().map(str::to_owned) else {
            return self.refuse_aloud("This buffer has no file.");
        };
        self.kill_ring.push(path);
        Action::Redraw
    }

    /// Doom's `SPC s b`, which is `consult-line`.
    ///
    /// The `line` source has been in the picker since M1 and no leader key
    /// reached it: `SPC .` opens on buffers and `SPC RET` on recent files, and
    /// lines wanted `SPC f f` then `< l`. One widget over four sources is
    /// meowr's answer to four of Doom's commands, and this is the fourth key
    /// that names one (#97).
    fn search_lines(&mut self) -> Action {
        self.open_picker_on('l')
    }

    /// Doom's `SPC RET`, which is `recentf-open-files`.
    fn recent_files(&mut self) -> Action {
        self.open_picker_on('r')
    }

    /// Doom's `SPC f F`, which is `+default/find-file-under-here`: every file
    /// under the buffer's directory, recursively.
    ///
    /// `doom-project-find-file` on a directory that is not a project root, which
    /// binds `project-current-directory-override` to it -- a project rooted
    /// there for one call. So no Project is asked for and none can be missing:
    /// the host lists the directory whether or not a repository sits above it.
    fn find_file_under_here(&mut self) -> Action {
        let Some(path) = self.current().path().map(str::to_owned) else {
            return self.refuse_aloud("This buffer has no file.");
        };
        self.pending_files = Some(Files::UnderThisBuffer(path));
        Action::Files
    }

    /// Which directory is to be listed, if one was asked for.
    ///
    /// Drained rather than read, as every other pending question is.
    pub fn pending_files(&mut self) -> Option<Files> {
        self.pending_files.take()
    }

    /// Where the host found `init.scm`, told once at startup (#181).
    ///
    /// `None` when there is no home to look in, which is the only thing
    /// `config::find` cannot answer and what both config rows refuse for.
    pub fn set_config_path(&mut self, path: Option<String>) {
        self.config = path;
    }

    /// The config, or the sentence for there being nowhere to keep one.
    ///
    /// Both rows need the same path and refuse for the same reason, and a
    /// refusal written twice is one that drifts.
    ///
    /// The sentence says what is missing without saying which environment
    /// variables were consulted for it: which ones those are is `config::find`'s
    /// and the host's, and a domain crate that names them is a second source
    /// for a fact it cannot check.
    fn config_path(&mut self) -> Result<String, Action> {
        match self.config.clone() {
            Some(path) => Ok(path),
            None => Err(self
                .refuse_aloud("There is nowhere for a config to live: meowr has no home to look in.")),
        }
    }

    /// Doom's `SPC f P`, which is `doom/open-private-config`.
    ///
    /// Doom opens the *directory* in dired and meowr has no dired, so this
    /// opens the one file that directory is for -- and #175 settled that it
    /// opens even when the file is not there, so that writing the buffer makes
    /// it. That is the first buffer meowr has ever had visiting a path with
    /// nothing behind it: see [`Opened`] for why absence had to stop being an
    /// error to get there.
    ///
    /// There is no reload, so a config edited here takes effect on the next
    /// `SPC q r` (#175, #182).
    fn open_config(&mut self) -> Action {
        let path = match self.config_path() {
            Ok(path) => path,
            Err(refused) => return refused,
        };
        // The picker's own way in, which shows the buffer and answers `Redraw`
        // when one is already visiting the file: pressing `SPC f P` twice
        // should be pressing it once, not a second buffer over the first.
        let action = self.open_chosen(path);
        // Only once it is really a read, and safe to set after the fact only
        // because `ask_to_open` has just cleared it: a file already open was
        // never asked about, and a leave left standing meets the next read.
        if action == Action::Open {
            self.narrow.pending_new = true;
        }
        action
    }

    /// Doom's `SPC f p`, which is `doom/find-file-in-private-config`.
    ///
    /// `SPC f F`'s listing (#149) rooted at the config instead of at this
    /// buffer. Not the project picker: the config directory is not a Project,
    /// and a listing of one place shown as another is #120's bug.
    fn find_file_in_config(&mut self) -> Action {
        let path = match self.config_path() {
            Ok(path) => path,
            Err(refused) => return refused,
        };
        self.pending_files = Some(Files::BesideTheConfig(path));
        Action::Files
    }

    /// A listing on its way into a picker.
    ///
    /// Dropped if a widget has opened since, which owns the Bottom (ADR-0004)
    /// and is something the user is in the middle of -- and, for `SPC f F`, if
    /// the buffer is no longer the one asked about, because that listing is of
    /// *its* directory. The config's is nobody's buffer and has only the first
    /// guard to answer to (#181). A refusal is the host's sentence and is said.
    pub fn files_listed(&mut self, about: &Files, answer: Result<Vec<String>, String>) -> Action {
        match about {
            Files::UnderThisBuffer(path) => self.file_answer(Some(path), answer, "Nothing found."),
            Files::BesideTheConfig(_) => {
                self.file_answer(None, answer, "Nothing beside the config.")
            }
        }
    }

    /// A list of files a question was answered with, into a picker: dropped if
    /// a widget has opened since, a refusal said, and an empty list said in the
    /// row's own words.
    ///
    /// `about` is the buffer the answer has to still be about, for the rows
    /// whose question was about one. `None` is a question that was about
    /// nobody's buffer -- the config's directory (#181) -- which has nothing to
    /// go stale against.
    fn file_answer(
        &mut self,
        about: Option<&str>,
        answer: Result<Vec<String>, String>,
        empty: &str,
    ) -> Action {
        // A widget opened meanwhile owns the Bottom (ADR-0004) and is something
        // the user is in the middle of.
        if self.narrow.widget.is_some() {
            return Action::Nothing;
        }
        if about.is_some_and(|about| self.current().path() != Some(about)) {
            return Action::Nothing;
        }
        match answer {
            Err(why) => self.refuse_aloud(why),
            Ok(files) if files.is_empty() => {
                self.echo = Some(empty.to_string());
                Action::Redraw
            }
            Ok(files) => self.open_files(files),
        }
    }

    /// Doom's `SPC f Y`, `+default/yank-buffer-path-relative-to-project`.
    ///
    /// Which is `(+default/yank-buffer-path (doom-project-root))`, and that
    /// takes the path relative to the root **or the whole path when there is no
    /// root** -- Doom's fallback, followed here. ADR-0010 refuses a silent one
    /// for a listing, because an empty list reads as success; a yank says what
    /// it copied, so the long path is its own explanation.
    fn yank_buffer_path_relative_to_project(&mut self) -> Action {
        let Some(about) = self.current().path().map(str::to_owned) else {
            return self.refuse_aloud("This buffer has no file.");
        };
        self.pending_from_root = Some(FromRoot::Yank { about });
        Action::FromRoot
    }

    /// Doom's `SPC f R`, `projectile-recentf`: the recent files in this
    /// buffer's Project.
    ///
    /// `projectile-recentf-files` filters `recentf-list` by the root as a
    /// prefix, so a recent file the Project ignores is still one of them --
    /// which is why the host filters by the root rather than the session
    /// intersecting with the Project's listing.
    fn project_recent_files(&mut self) -> Action {
        let Some(about) = self.current().path().map(str::to_owned) else {
            return self.refuse_aloud("This buffer has no file.");
        };
        let paths = self.recent.paths().map(str::to_owned).collect();
        self.pending_from_root = Some(FromRoot::Recent { about, paths });
        Action::FromRoot
    }

    /// Doom's `SPC k f`, `+format/region-or-buffer` -- **the whole buffer**,
    /// always (ADR-0014). Doom formats the active region as though it were a
    /// standalone file and warns about it, and under Meow a selection is almost
    /// always active, so region-or-buffer would nearly always mean region.
    fn format_buffer(&mut self) -> Action {
        match self.format_of(self.current_buffer_id(), FormatOrigin::Key) {
            Ok(format) => {
                self.pending_format.push_back(format);
                Action::Format
            }
            Err(why) => self.refuse_aloud(why),
        }
    }

    /// A format of one buffer, or the sentence saying why there cannot be one.
    ///
    /// One walk that answers both, in the order they are checked, because
    /// `unwritable_reason`'s doc says what happens otherwise: a caller that
    /// re-derives why `None` came back walks one condition fewer.
    fn format_of(&self, id: BufferId, origin: FormatOrigin) -> Result<Format, String> {
        let buffer = self.buffer(id);
        let path = buffer.path().ok_or("This buffer has no file.")?;
        let language = buffer
            .language
            .and_then(|language| self.languages.name_of(language))
            .ok_or("Nothing formats this buffer.")?;
        let invocation = self
            .formatter(id)
            .ok_or_else(|| format!("Nothing formats {language}."))?;
        let text = buffer
            .document()
            .ok_or("This buffer has not been read yet.")?
            .text()
            .to_string();
        Ok(Format {
            path: path.to_string(),
            revision: buffer.revision(),
            language: language.to_string(),
            invocation,
            text,
            origin,
        })
    }

    /// The formatter to run, if one was asked for. Drained.
    pub fn pending_format(&mut self) -> Option<Format> {
        self.pending_format.pop_front()
    }

    /// What a formatter came to, for the text `asked` sent it (ADR-0014).
    ///
    /// Output is diffed into hunks and applied as a **Foreign edit** from the
    /// program -- so typing since the ask drops it, and says so, by ADR-0013's
    /// revision check. Output identical to the text is said, because a key that
    /// changed nothing and said nothing reads as a key that did not run.
    /// Failure and absence are said and change nothing; the save, if there was
    /// one, stands.
    ///
    /// **From a save** what changed nothing says nothing, a missing program is
    /// said once per Language, and a format that lands answers `Action::Save`
    /// so the host writes it again through [`Session::quiet_save_target`].
    pub fn formatted(&mut self, asked: &Format, outcome: Formatted) -> Action {
        let program = asked.invocation.program.clone();
        let from_save = asked.origin == FormatOrigin::Save;
        match outcome {
            Formatted::Missing
                if from_save && !self.missing_formatters.insert(asked.language.clone()) =>
            {
                Action::Nothing
            }
            Formatted::Missing => self.refuse(format!(
                "{program} is not installed, so {} is not formatted.",
                asked.language
            )),
            Formatted::Failed(why) => self.refuse(format!("{program} failed: {why}")),
            Formatted::Output(new) => {
                // Diffed against the buffer's own text, and only while its
                // revision is the one asked about -- which makes that text the
                // text the formatter was given, without trusting what came
                // back. A moved revision is refused before replacements are
                // looked at, so none are made for it.
                let current = self
                    .buffer_visiting(&asked.path)
                    .filter(|id| self.buffer(*id).revision() == asked.revision)
                    .and_then(|id| self.buffer(id).document())
                    .map(|doc| doc.text().to_string());
                if current.as_deref() == Some(new.as_str()) {
                    return if from_save {
                        Action::Nothing
                    } else {
                        self.refuse(format!("{} is already formatted.", asked.path))
                    };
                }
                let (action, applied) = self.apply_foreign(ForeignEdit {
                    path: asked.path.clone(),
                    revision: asked.revision,
                    from: program,
                    replacements: current
                        .map(|current| meowr_domain::replacements_between(&current, &new))
                        .unwrap_or_default(),
                });
                if from_save && applied {
                    self.pending_quiet_write = Some(asked.path.clone());
                    return Action::Save;
                }
                action
            }
        }
    }

    /// A question for the Project's root, if one was asked. Drained.
    pub fn pending_from_root(&mut self) -> Option<FromRoot> {
        self.pending_from_root.take()
    }

    /// `SPC f Y`'s answer: the path to yank, from the root or whole.
    ///
    /// Dropped if the buffer is no longer the one asked about, as `found` drops
    /// an answer -- a path copied for a file nobody is looking at would be a
    /// surprise the next `p` delivered.
    pub fn path_from_root(&mut self, about: &str, path: String) -> Action {
        if self.current().path() != Some(about) {
            return Action::Nothing;
        }
        self.echo = Some(format!("Copied path: {path}"));
        self.kill_ring.push(path);
        Action::Redraw
    }

    /// `SPC f R`'s answer: the recent files in the Project, or why there is none.
    ///
    /// A buffer in no repository is refused, with the host's sentence, rather
    /// than shown every recent file: `projectile-recentf` asks for a project
    /// before it lists, and ADR-0010 names what is missing rather than guessing
    /// a directory. With `project-scope` off the buffer's own directory stands in
    /// for the Project, as it does for every project row, so an empty answer
    /// does not claim there is a project to be empty.
    pub fn recent_from_root(&mut self, about: &str, answer: Result<Vec<String>, String>) -> Action {
        let empty = if self.options.project_scope {
            "No recent files in this project."
        } else {
            "No recent files under this directory."
        };
        self.file_answer(Some(about), answer, empty)
    }

    /// Doom's `SPC SPC`, which is `projectile-find-file`.
    ///
    /// The picker narrowed to the Project, as `SPC .`, `SPC RET` and `SPC s b`
    /// narrow it to theirs (#147). The Project is asked for by opening, as it
    /// is for every opening of the picker, so a row that exists to show the
    /// Project cannot be the one that forgot to.
    fn project_find_file(&mut self) -> Action {
        self.open_picker_on('p')
    }

    fn complete_at_point(&mut self) -> Action {
        self.open_completion();
        Action::Redraw
    }

    /// `meow-visit`: choose a symbol from the buffer and select it.
    ///
    /// Through the narrowing widget, which is why this waited on it — Meow's
    /// is a completing-read over the words and symbols in the buffer.
    pub fn open_visit(&mut self) {
        let Some(doc) = self.document() else { return };
        let whole = doc.text().to_string();
        let mut words: Vec<String> = Vec::new();
        for word in whole.split(|c: char| !(c.is_alphanumeric() || c == '_')) {
            if word.len() > 1 && !words.iter().any(|w| w == word) {
                words.push(word.to_string());
            }
        }
        let candidates = words
            .into_iter()
            .map(|w| Candidate::new(w, narrow::VISIT))
            .collect();
        self.narrow.origin = Some(self.current().selection.current());
        self.narrow.widget = Some(Narrow::new(Driver::Picker, Vec::new(), candidates));
        self.narrow.accepts = narrow::Accepts::Search;
    }

    /// Point the window back at the buffer narrowing was opened over.
    ///
    /// A no-op unless a file preview has moved it, which is why it takes the
    /// `Option` rather than making each caller unwrap one: "there was nothing to
    /// go back to" and "we are already there" are the same answer here.
    pub(crate) fn return_to(&mut self, opened_over: Option<BufferId>) {
        if let Some(id) = opened_over
            && id != self.current_buffer_id()
        {
            self.show(id);
        }
    }

    /// Put the point at the start of a one-based line, and scroll to it.
    ///
    /// Previewing a `line` candidate and accepting one are the same movement —
    /// the preview is what accepting would do, done early — so they are one
    /// function rather than two that have to be kept saying the same thing.
    /// Go to a [`Place`] in the buffer now current, honouring its column.
    ///
    /// Not `go_to_line`, which takes a **1-based** line because that is what a
    /// `line` candidate's annotation holds. A Place is 0-based, because LSP is,
    /// and it has a column that `go_to_line` has nowhere to put.
    pub(crate) fn go_to_place(&mut self, place: &Place) -> Action {
        // **Before the Place resolves**, which is ADR-0012's ordering and the
        // half of it that matters: a jump onto a line nobody can see leaves the
        // screen unchanged and reads as a broken jump rather than as a fold,
        // and refusing instead would let a fold in a buffer veto a jump into
        // it. Here rather than in `jump_to`, because `trail_back` comes through
        // the same door and a Trail entry outlives the fold state it was made
        // in -- the line it names can have been folded since it was pushed.
        self.reveal(place.line);
        let Some(target) = self.document().and_then(|doc| place.offset_in(doc)) else {
            return Action::Nothing;
        };
        self.current_mut()
            .selection
            .select_without_history(Selection::at(target));
        self.scroll_into_view();
        Action::Redraw
    }

    /// Where the point is, as somewhere to come back to.
    ///
    /// `None` in a buffer with no file: there is nothing to name it by, and a
    /// Trail entry that cannot be reopened is worse than one that was never
    /// pushed.
    pub(crate) fn here(&self) -> Option<Place> {
        let path = self.buffer(self.current_buffer_id()).path()?.to_owned();
        let doc = self.document()?;
        let pos = self.current().selection.current().pos();
        let (line, column) = doc.position_at(pos)?;
        Some(Place::new(path, line, column))
    }

    /// Go to a Place, remembering where it left.
    ///
    /// **The one way a Place is jumped to**, which is what makes the Trail
    /// impossible for a caller to forget -- #127's lesson, and the shape
    /// `open_picker` took after `find-file` opened a picker and asked nothing.
    /// Every row ADR-0011 sends to the picker comes through here.
    pub fn jump_to(&mut self, place: Place) -> Action {
        if let Some(from) = self.leaving_from() {
            self.trail.push(from);
        }
        self.go_to_or_open(place, false)
    }

    /// Where a jump is leaving, which is **not always where the point is**.
    ///
    /// A list of Places is opened *over* somewhere, and neither of the two
    /// homes ADR-0011 gives one can answer with the point. The picker has
    /// previewed the selected row before it is accepted, so the point already
    /// names where the jump is *going* -- pushing it makes going back a no-op,
    /// which is a key that appears to work. A generated listing visits no file
    /// at all, so there is nothing for [`Session::here`] to name.
    ///
    /// So each home remembers where it was opened over, and this is the one
    /// place that knows to ask them: the reasoning lives here rather than
    /// twice over on two fields that would then have to go on agreeing. The
    /// point is the last resort rather than the first, which is still the only
    /// answer for an ordinary jump with no list behind it.
    ///
    /// The picker's is **taken**, because a widget is opened once and accepted
    /// once; the buffer's is read, because `*diagnostics*` stays open and every
    /// row in it leaves from the same place.
    fn leaving_from(&mut self) -> Option<Place> {
        self.narrow
            .from
            .take()
            .or_else(|| self.current().listing().and_then(Listing::from).cloned())
            .or_else(|| self.here())
    }

    /// The half both jumping and going back share: be in that file, be at that
    /// place. `back` says whose failure it is if the file cannot be read.
    fn go_to_or_open(&mut self, place: Place, back: bool) -> Action {
        match self.buffer_visiting(&place.path) {
            Some(id) => {
                self.show(id);
                self.go_to_place(&place)
            }
            None => {
                self.ask_to_open(place.path.clone(), false);
                self.narrow.pending_place = Some(place);
                self.narrow.pending_back = back;
                Action::Open
            }
        }
    }

    /// The list a previous session kept, read back by the host.
    pub fn set_recent_files(&mut self, files: Vec<String>) {
        self.recent = meowr_core::edit::RecentFiles::from_paths(files);
    }

    /// The recent files, newest first, for the host to keep on the way out.
    pub fn recent_file_paths(&self) -> impl Iterator<Item = &str> {
        self.recent.paths()
    }

    /// Never keep a file under `directory` as a recent one.
    ///
    /// The host's to say, because it is an environment variable: Doom excludes
    /// `$XDG_RUNTIME_DIR`, or `/run`, and checks it as a file is added
    /// (`recentf-add-file`), so one is not in `SPC RET` for the rest of the
    /// session either.
    pub fn exclude_from_recent(&mut self, directory: &str) {
        self.recent_exclude = Some(directory.to_string());
    }

    /// Put the current buffer's file at the top of the recent files.
    ///
    /// Where Doom's recentf adds or bumps one: a visit (`find-file-hook`) and a
    /// write, called from those three places, and a change of focused window,
    /// which `dispatch_command` notices for every command at once rather than
    /// each window command remembering to (`doom--recentf-touch-buffer-h`).
    /// Only a real buffer with a file -- a preview is nobody's choice yet, and a
    /// generated listing visits nothing.
    pub(crate) fn touch_recent(&mut self) {
        let buffer = self.current();
        let excluded = |path: &str| {
            self.recent_exclude
                .as_deref()
                .is_some_and(|dir| std::path::Path::new(path).starts_with(dir))
        };
        if buffer.is_real()
            && let Some(path) = buffer.path()
            && !excluded(path)
        {
            let path = path.to_string();
            self.recent.touch(&path);
        }
    }

    /// The revision of the buffer visiting `path`, for a question whose answer
    /// will be a **Foreign edit** to stamp itself with (ADR-0013).
    pub fn revision_of(&self, path: &str) -> Option<u64> {
        self.buffer_visiting(path).map(|id| self.buffer(id).revision())
    }

    /// Apply a **Foreign edit** to the buffer visiting its path (#161).
    ///
    /// The buffer decides whether it takes it (`Buffer::apply_foreign`); this
    /// says why when it does not, naming the buffer and the source, because
    /// somebody asked for this and silence reads as done (ADR-0013). No command
    /// hooks run: it is not a command.
    ///
    /// A path no buffer is visiting is #156's question, so it does nothing here.
    /// A preview is never visited by path, so it cannot be the target.
    pub fn apply_foreign_edit(&mut self, edit: ForeignEdit) -> Action {
        self.apply_foreign(edit).0
    }

    /// [`Session::apply_foreign_edit`], and whether the edit applied -- asked
    /// of the buffer rather than inferred afterwards, since a refusal and an
    /// edit both leave the frame to redraw.
    fn apply_foreign(&mut self, edit: ForeignEdit) -> (Action, bool) {
        let Some(id) = self.buffer_visiting(&edit.path) else {
            return (Action::Nothing, false);
        };
        let ForeignEdit {
            path,
            revision,
            from,
            replacements,
        } = edit;
        match self.buffers[id.0].apply_foreign(revision, &replacements) {
            Ok(false) => (Action::Nothing, false),
            Ok(true) => {
                if id == self.current_buffer_id() {
                    self.scroll_into_view();
                }
                (Action::Redraw, true)
            }
            Err(why) => (self.refuse(not_applied(&path, &from, why)), false),
        }
    }

    /// The revision of every open buffer, for a question whose answer may be a
    /// workspace edit (#167).
    pub fn revisions(&self) -> Revisions {
        Revisions(
            (0..self.buffers.len())
                .map(BufferId)
                .filter(|id| self.buffer(*id).is_real())
                .filter_map(|id| {
                    let buffer = self.buffer(id);
                    Some((buffer.path()?.to_string(), (id, buffer.revision())))
                })
                .collect(),
        )
    }

    /// Apply a **workspace edit**: a Foreign edit across files, all or nothing
    /// (#167, ADR-0013 amended).
    ///
    /// Every file is planned before any is changed -- its buffer writable and
    /// unmoved, every line it names there, no two replacements overlapping, no
    /// file named twice -- and the first that fails is named and nothing is
    /// applied, because half a rename is a build that does not compile. A file
    /// nobody had open becomes a buffer that is not shown, edited and left
    /// modified, lsp-mode's `find-file-noselect`; the host tells its server
    /// about it as it does any buffer, with the edited text. A resource
    /// operation refuses the whole edit, since meowr declares none. One sentence
    /// says what happened, and a refusal is said as a formatter's is.
    pub fn apply_workspace_edit(&mut self, edit: WorkspaceEdit) -> Action {
        let WorkspaceEdit { from, asked, changes } = edit;
        let planned = match self.plan_workspace_edit(&from, &asked, changes) {
            Ok(planned) => planned,
            Err(why) => return self.refuse(why),
        };
        let mut touched = Vec::new();
        for plan in planned.into_iter().filter(|plan| !plan.replacements.is_empty()) {
            touched.push(self.apply_planned(plan));
        }
        if touched.is_empty() {
            self.say(&format!("{from}'s edit changes nothing."));
            return Action::Redraw;
        }
        if touched.contains(&self.current_buffer_id()) {
            self.scroll_into_view();
        }
        let unsaved = touched.iter().filter(|id| self.buffer(**id).is_modified()).count();
        let files = match touched.len() {
            1 => "1 file".to_string(),
            n => format!("{n} files"),
        };
        let save = self.reached_by("save-some-buffers");
        self.say(&match unsaved {
            0 => format!("{from} changed {files}."),
            1 => format!("{from} changed {files}; 1 is unsaved, and {save} saves it."),
            n => format!("{from} changed {files}; {n} are unsaved, and {save} saves them."),
        });
        Action::Redraw
    }

    /// Every file of a workspace edit checked, or the sentence that refuses it.
    fn plan_workspace_edit(
        &self,
        from: &str,
        asked: &Revisions,
        changes: Vec<FileChange>,
    ) -> Result<Vec<Planned>, String> {
        let files = changes
            .into_iter()
            .map(|change| match change {
                FileChange::Edit(file) => Ok(file),
                FileChange::Create { path } => Err(format!("creates {path}")),
                FileChange::Rename { from: path, .. } => Err(format!("renames {path}")),
                FileChange::Delete { path } => Err(format!("deletes {path}")),
            })
            .collect::<Result<Vec<FileEdit>, String>>()
            .map_err(|does| {
                format!(
                    "{from}'s edit {does}, and meowr does not move files for an edit, so nothing was applied."
                )
            })?;
        // The protocol lets `documentChanges` name a document twice, each
        // against the text the one before left; planned as one set, the second
        // would be refused after the first had applied.
        let mut named = std::collections::HashSet::new();
        if let Some(twice) = files.iter().find(|file| !named.insert(file.path.as_str())) {
            return Err(not_applied(&twice.path, from, ForeignRefusal::NamedTwice));
        }
        files
            .into_iter()
            .map(|file| self.plan_file(from, asked, file))
            .collect()
    }

    /// One file of a workspace edit, checked against the buffer visiting it or
    /// the text the host read.
    fn plan_file(&self, from: &str, asked: &Revisions, file: FileEdit) -> Result<Planned, String> {
        let FileEdit { path, read, edits } = file;
        let refused = |why| not_applied(&path, from, why);
        let stamped = asked.0.get(&path).copied();
        let (target, doc) = match (self.buffer_visiting(&path), stamped) {
            // Stamped under another buffer -- one killed since, whose unsaved
            // text the server answered against, and perhaps reopened, counting
            // its revisions from the start again.
            (None, Some(_)) => return Err(refused(ForeignRefusal::Moved)),
            (Some(id), Some((was, _))) if was != id => return Err(refused(ForeignRefusal::Moved)),
            (Some(id), stamped) => {
                let buffer = self.buffer(id);
                // A buffer opened after the question was not stamped, so the
                // server answered from the file -- which a modified buffer no
                // longer says.
                let revision = match stamped {
                    Some((_, revision)) => revision,
                    None if !buffer.is_modified() => buffer.revision(),
                    None => return Err(refused(ForeignRefusal::Moved)),
                };
                if let Some(why) = buffer.foreign_refusal(revision) {
                    return Err(refused(why));
                }
                let doc = buffer.document().ok_or_else(|| refused(ForeignRefusal::Unread))?;
                (Target::Open { id, revision }, doc.clone())
            }
            (None, None) => {
                let text = match read {
                    Some(Ok(text)) => text,
                    Some(Err(why)) => {
                        return Err(format!(
                            "{path} could not be read ({why}), so {from}'s edit was not applied."
                        ));
                    }
                    None => return Err(refused(ForeignRefusal::Unread)),
                };
                let doc = Document::from_text(&text);
                let mut buffer = Buffer::new();
                buffer.set_path(path.as_str(), &self.languages);
                buffer.loaded(doc.clone());
                (Target::Unopened(Box::new(buffer)), doc)
            }
        };
        let replacements = edits
            .iter()
            .map(|edit| doc.replacement_for(edit))
            .collect::<Result<Vec<_>, _>>()
            .map_err(|past| refused(ForeignRefusal::PastEnd(past)))?;
        meowr_core::undo::application_order(&replacements)
            .map_err(|why| refused(ForeignRefusal::Unapplicable(why)))?;
        Ok(Planned {
            target,
            replacements,
        })
    }

    /// Apply one planned file, opening its buffer unshown if it had none.
    fn apply_planned(&mut self, Planned { target, replacements }: Planned) -> BufferId {
        let (id, revision) = match target {
            Target::Open { id, revision } => (id, revision),
            Target::Unopened(buffer) => {
                let revision = buffer.revision();
                self.buffers.push(*buffer);
                (BufferId(self.buffers.len() - 1), revision)
            }
        };
        // Planned against the same checks `apply_foreign` makes, each file
        // once, and nothing runs between the plan and this.
        let applied = self.buffers[id.0].apply_foreign(revision, &replacements);
        debug_assert!(applied.is_ok(), "a planned edit was refused: {applied:?}");
        id
    }

    /// The real buffer visiting `path`, if one is open.
    ///
    /// The one question every way of reaching a file asks before reading it --
    /// a jump to a Place and a file chosen in the picker -- so neither opens a
    /// file a second time. Paths compare as strings, which holds because the
    /// host gives every path one spelling before the session sees it (#150).
    pub(crate) fn buffer_visiting(&self, path: &str) -> Option<BufferId> {
        (0..self.buffers.len())
            .map(BufferId)
            .find(|id| self.buffer(*id).is_real() && self.buffer(*id).path() == Some(path))
    }

    /// `trail-back`: the Place the last jump left.
    ///
    /// Pops rather than cycles. An entry whose file has gone is **consumed**
    /// and said out loud rather than skipped past -- ADR-0011, where skipping
    /// is the worse failure because "back did nothing" and "back went somewhere
    /// unexpected" are indistinguishable from outside.
    #[doc(hidden)]
    pub fn trail_back_for_test(&mut self) -> Action {
        self.trail_back()
    }

    /// Kill the buffer with this path, for a test that needs a Trail entry
    /// whose buffer has gone. Not a command: `kill-buffer` kills the current
    /// one, and a test that had to make it current would be testing that.
    #[doc(hidden)]
    pub fn kill_named_for_test(&mut self, path: &str) {
        if let Some(id) = (0..self.buffers.len())
            .map(BufferId)
            .find(|id| self.buffer(*id).path() == Some(path))
        {
            self.show(id);
            self.kill_current(true);
        }
    }

    fn trail_back(&mut self) -> Action {
        let Some(place) = self.trail.pop() else {
            self.say("The Trail is empty.");
            return Action::Redraw;
        };
        self.go_to_or_open(place, true)
    }

    pub(crate) fn go_to_line(&mut self, line: usize) -> Action {
        let target = self
            .document()
            .and_then(|doc| doc.line_to_byte(line.saturating_sub(1)));
        let Some(target) = target else {
            return Action::Nothing;
        };
        self.current_mut()
            .selection
            .select_without_history(Selection::at(target));
        self.scroll_into_view();
        Action::Redraw
    }

    /// The file the host should read for the preview, if it needs reading.
    ///
    /// `None` for a file that is already open: the buffer is what a preview of
    /// it shows, because the disk holds the file as it was last saved and the
    /// line a row names is counted in the text being edited (#153).
    pub fn preview_read(&self) -> Option<String> {
        self.preview_target()
            .as_ref()
            .and_then(Preview::path)
            .filter(|path| self.buffer_visiting(path).is_none())
            .map(str::to_owned)
    }

    /// Put back the point the last preview moved in an open buffer, if it did.
    pub(crate) fn put_back_preview_move(&mut self) {
        if let Some((id, selection)) = self.narrow.moved.take()
            && let Some(buffer) = self.buffers.get_mut(id.0)
        {
            buffer.selection.select_without_history(selection);
        }
    }

    /// Show the previewed candidate, where showing it needs no I/O.
    ///
    /// A file nobody has open is the one this cannot do: reading a file is the
    /// host's, so the host asks [`Session::preview_read`], reads, and hands the
    /// text to [`Session::previewed`].
    pub fn preview(&mut self) -> Action {
        // Whatever the last preview moved goes back first, so each preview
        // starts from the buffers as they were -- `consult--jump-preview` runs
        // its restore list before every step.
        self.put_back_preview_move();
        match self.preview_target() {
            // A `line` names a line in the buffer narrowing was opened over, so
            // a file preview that pointed the window elsewhere has to be undone
            // before scrolling — otherwise the jump lands in the file you were
            // only looking at.
            Some(Preview::Line(line)) => {
                self.return_to(self.narrow.shown);
                self.go_to_line(line)
            }
            // Already loaded, so previewing it is pointing the window at it
            // and, on a cancel, back again.
            Some(Preview::Buffer(id)) => {
                if id == self.current_buffer_id() {
                    return Action::Nothing;
                }
                self.show(id);
                Action::Redraw
            }
            // An open file is its buffer, and a Place in it moves that buffer's
            // point -- remembered first, so the move can be put back.
            Some(Preview::At(place)) => match self.buffer_visiting(&place.path) {
                Some(id) => {
                    self.narrow.moved = Some((id, self.buffers[id.0].selection.current()));
                    self.show(id);
                    self.go_to_place(&place)
                }
                None => Action::Nothing,
            },
            // A file nobody has open needs reading, which is the host's. The
            // difference between a head and a line is decided in `previewed`,
            // after there is text to scroll.
            Some(Preview::File(_)) | None => Action::Nothing,
        }
    }

    /// The branch every modeline shows. Supplied by the host, which is the
    /// only layer allowed to run `git`.
    pub fn set_branch(&mut self, branch: Option<String>) {
        self.branch = branch;
    }

    /// Expand hints, which only ever belong to the focused window: they are
    /// about what the *next key* would select.
    pub fn hints(&self) -> Vec<Hint> {
        let window = self.windows.current();
        let buffer = &self.buffers[window.buffer.0];
        match buffer.document() {
            Some(doc) => view_hints(
                doc,
                text_area(window.area).viewport(window.top),
                &buffer.hints,
            ),
            None => Vec::new(),
        }
    }

    pub fn hints_pending(&self) -> bool {
        !self.current().hints.is_empty()
    }

    pub fn expire_hints(&mut self) -> Action {
        if self.current().hints.is_empty() {
            return Action::Nothing;
        }
        self.current_mut().hints.clear();
        Action::Redraw
    }

    // --- dispatch ---------------------------------------------------------

    /// Register a hook, replacing any with the same name.
    ///
    /// By name, because a hook that cannot be removed is a leak — and M2's
    /// Steel bindings will add and remove them from a config file that gets
    /// re-evaluated.
    pub fn add_hook(
        &mut self,
        name: impl Into<String>,
        point: HookPoint,
        run: impl Fn(&mut Ctx<'_>) -> Proceed + 'static,
    ) {
        self.hooks.add(name, point, run);
    }

    pub fn remove_hook(&mut self, point: HookPoint, name: &str) -> bool {
        self.hooks.remove(point, name)
    }

    pub fn hook_names(&self) -> Vec<String> {
        self.hooks.names().map(str::to_owned).collect()
    }

    /// Bind a key sequence, for tests and, later, for configuration.
    /// Bind `keys` to a command, in the keymap or in Ring 2.
    ///
    /// A sequence beginning with [`leader::KEY`] in Normal State goes into the
    /// leader tree, and everything else into the keymap. Routing rather than a
    /// second entry point so that the one string on screen is the one string
    /// that works: the menu shows `SPC d s`, the Echo Area shows `SPC d s`, and
    /// so does a config. A separate `bind_leader` would mean the key you can
    /// read off the screen is the key you cannot paste into `init.scm`.
    pub fn bind(
        &mut self,
        state: State,
        keys: &[Key],
        command: &str,
    ) -> Result<(), meowr_command::BindError> {
        match leader_suffix(state, keys) {
            Some(suffix) => self
                .leader
                .set(suffix.to_vec(), leader::Entry::Command(command.into())),
            None => self.keymap.bind(state, keys, command),
        }
    }

    /// Name a leader prefix, so the menu can say what lies under it.
    pub fn bind_prefix(
        &mut self,
        keys: &[Key],
        name: &str,
    ) -> Result<(), meowr_command::BindError> {
        match leader_suffix(State::Normal, keys) {
            Some(suffix) => self
                .leader
                .set(suffix.to_vec(), leader::Entry::Prefix(name.into())),
            None => Err(meowr_command::BindError::NoPrefix),
        }
    }

    /// Take a binding out, from wherever it lives. `true` if there was one.
    pub fn unbind(&mut self, state: State, keys: &[Key]) -> bool {
        match leader_suffix(state, keys) {
            Some(suffix) => self.leader.unset(suffix),
            None => self.keymap.unbind(state, keys),
        }
    }

    /// Run a command by name, with no key involved.
    /// Run `keys` as `execute-kbd-macro` would, rather than as typing.
    ///
    /// The difference is not cosmetic. `meow--maybe-toggle-beacon-state` opens
    /// with `(unless (or defining-kbd-macro executing-kbd-macro) ...)`, so a
    /// replay never switches into Beacon State — which is what stops a beacon
    /// macro from changing State underneath itself while it is being applied
    /// at the second beacon.
    pub fn replay(&mut self, keys: &[Key]) -> Action {
        let was = std::mem::replace(&mut self.executing, true);
        let mut action = Action::Nothing;
        for &key in keys {
            action = self.on_key(key);
        }
        self.executing = was;
        action
    }

    pub fn run(&mut self, name: &str) -> Action {
        self.dispatch(name, None)
    }

    /// Record what just ran, for `repeat` and for `last_command` tests.
    ///
    /// One place rather than the six the early returns used to each carry:
    /// every one of them had to remember, and a path that forgot would have
    /// left `repeat` pointing at the command before it.
    fn remember(&mut self, name: &str, argument: Argument) {
        self.last_command = Some(name.to_owned());
        self.last_argument = argument;
    }

    /// Run `name`, and tell the hooks if the State moved.
    ///
    /// A wrapper rather than a line at the end of `dispatch_command`, because
    /// half the State changes leave through an early `return` — `leader` and
    /// `beacon-start` never reach the bottom of that function. The recursive
    /// calls inside it go to `dispatch_command` directly so that a fallback or
    /// a `repeat` reports one change and not two.
    fn dispatch(&mut self, name: &str, key: Option<Key>) -> Action {
        let before = self.state;
        let action = self.dispatch_command(name, key);
        if self.state != before && self.run_hooks(HookPoint::StateChange) == Action::Redraw {
            // A hook that changed the buffer has to be drawn even when the
            // command that moved the State asked for nothing.
            return match action {
                Action::Nothing => Action::Redraw,
                other => other,
            };
        }
        action
    }

    /// Run the hooks at `point` against the current buffer.
    ///
    /// The context `dispatch_command` builds, less the parts that only mean
    /// something inside a command: no argument is being consumed and no
    /// command is being stood before or after.
    fn run_hooks(&mut self, point: HookPoint) -> Action {
        if self.in_hook {
            return Action::Nothing;
        }
        let buffer_id = self.current_buffer_id();
        if !matches!(self.buffers[buffer_id.0].content, Content::Ready(_)) {
            return Action::Nothing;
        }
        let last = self.last_command.clone();
        let window = self.window_range();
        let mut next = Argument::None;
        let buffer = &mut self.buffers[buffer_id.0];
        let Content::Ready(doc) = &mut buffer.content else {
            unreachable!("checked above")
        };
        let mut ctx = Ctx {
            doc,
            selection: &mut buffer.selection,
            state: &mut self.state,
            argument: Argument::None,
            next_argument: &mut next,
            options: &self.options,
            kill_ring: &mut self.kill_ring,
            history: &mut buffer.history,
            things: &self.things,
            text_objects: buffer
                .syntax
                .as_ref()
                .map(|syntax| meowr_syntax::TextObjects {
                    syntax,
                    languages: &self.languages,
                }),
            last_command: last.as_deref(),
            key: None,
            window,
        };
        self.hooks.run(point, &mut ctx);
        self.dispatch_hook_commands(point)
    }

    /// Run the commands a config attached to `point`, after its closures.
    ///
    /// After, and not interleaved, because a hook cannot run a command itself:
    /// it is `Fn(&mut Ctx) -> Proceed` and the registry is not reachable from
    /// inside a `Ctx`. The borrow that `Ctx` needs has to be released first,
    /// which is exactly why only the two hook points that reach here through
    /// `run_hooks` accept a command at all — see [`HookPoint::FOR_COMMANDS`].
    ///
    /// A command that asks the *host* to act cannot be honoured from here, and
    /// the reason is structural rather than about any one Action: neither
    /// caller plumbs this return value out. `mark_saved` is called *by* the
    /// host and discards it, and `dispatch` merges only a `Redraw`. So the
    /// host is not listening at either hook point, and every escaping Action is
    /// refused for the same reason.
    ///
    /// `Save` is the one that would also *loop*, since `mark_saved` runs after
    /// a write and would ask for another — a round trip `in_hook` cannot see,
    /// because it guards one stack and not a return through the host. `Open`
    /// is unreachable today, arising only from `Key::Enter` inside the
    /// narrowing widget rather than from any dispatch, and is matched for
    /// exhaustiveness.
    ///
    /// Refused out loud rather than dropped: a key that silently does nothing
    /// is the failure this codebase keeps not shipping.
    fn dispatch_hook_commands(&mut self, point: HookPoint) -> Action {
        let names: Vec<String> = self
            .hooks
            .commands(point)
            .into_iter()
            .map(str::to_owned)
            .collect();
        if names.is_empty() {
            return Action::Nothing;
        }
        // #64's rule is about what a hook-run command *sees*; these three are
        // what it would otherwise *write*. `repeat` reads `last_command`, so a
        // hook left in it makes `'` redo the hook rather than the keystroke —
        // a key silently doing the wrong thing, which is worse than a key
        // doing nothing. And `dispatch_command` ends with `self.argument =
        // next`, which would eat the prefix argument the user was mid-way
        // through building.
        let last_command = self.last_command.clone();
        let last_argument = self.last_argument;
        let argument = self.argument;
        self.in_hook = true;
        let mut action = Action::Nothing;
        for name in names {
            // Formats queued before this command are other saves' -- the
            // buffers `save-some-buffers` wrote before this one -- and are
            // not the refused command's to take with it.
            let queued = self.pending_format.len();
            let asked = self.dispatch(&name, None);
            let refused_verb = match asked {
                Action::Nothing => None,
                Action::Redraw => {
                    action = Action::Redraw;
                    None
                }
                Action::Save => Some("save"),
                Action::Open => Some("open a file"),
                Action::Hover | Action::Lookup => Some("ask a language server"),
                Action::Project => Some("look up a project"),
                Action::Search => Some("search a directory"),
                Action::Files => Some("list a directory"),
                Action::FromRoot => Some("look up a project"),
                Action::Format => Some("run a formatter"),
                Action::SaveSome => Some("save"),
                Action::Rename | Action::CodeActions => Some("ask a language server"),
                Action::FileWorked => Some("copy or move a file"),
                Action::FileDeleted => Some("delete a file"),
                Action::Quit => Some("quit"),
                Action::Restart => Some("restart"),
            };
            if let Some(verb) = refused_verb {
                self.signals += 1;
                self.echo = Some(format!("`{name}` cannot {verb} from a hook."));
                // The refused command may have left a signal's payload behind.
                // Every question writes one before returning, and a payload
                // nobody collects would be answered by whichever question
                // asked next — for a point three edits ago, or for a buffer in
                // another repository. `pending_search` was missing from this
                // list from #139 to #149.
                self.pending_hover = None;
                self.pending_lookup = None;
                self.pending_project = None;
                self.pending_search = None;
                self.pending_files = None;
                self.pending_from_root = None;
                self.pending_format.truncate(queued);
                self.saving_some = None;
                self.pending_rename = None;
                self.pending_code_actions = None;
                self.pending_file_work = None;
                self.pending_file_deletion = None;
                // `killing_all` is deliberately **not** cleared here. It is
                // an outer command's unfinished business rather than an
                // unanswered question of the refused one's, and a hook that
                // runs a refused command during `save-some-buffers`'s
                // `before-save` would otherwise take a `SPC q F` with it --
                // leaving a press that saved everything, killed nothing, and
                // said only that it had saved. `kill-all-buffers` refuses in
                // a hook instead, so nothing here can have set it.
                action = Action::Redraw;
            }
        }
        self.in_hook = false;
        self.argument = argument;
        self.last_argument = last_argument;
        self.last_command = last_command;
        action
    }

    /// Run a command, and notice if it moved the focus to another window.
    ///
    /// Doom's `doom-switch-window-hook` fires on any change of the selected
    /// window, which is how a window switch bumps the recent files: splitting,
    /// closing and `other-window` all move it, and so can a hook's command. So
    /// this asks once, around every command, rather than each of them
    /// remembering to (#152).
    fn dispatch_command(&mut self, name: &str, key: Option<Key>) -> Action {
        let focused = self.windows.focus();
        let action = self.dispatch_command_unwatched(name, key);
        if self.windows.focus() != focused {
            self.touch_recent();
        }
        action
    }

    fn dispatch_command_unwatched(&mut self, name: &str, key: Option<Key>) -> Action {
        // The Beacon keymap's remaps of the commands that act, before anything
        // else looks at the name: `beacon_remap` runs the ordinary command
        // itself, from Normal State, so the re-entry finds nothing to remap.
        // The noop remaps are further down, in the match; the two lists are
        // disjoint and a name in neither is its ordinary self.
        if let Some(action) = self.beacon_remap(name, key) {
            self.remember(name, Argument::None);
            return action;
        }
        let Some(command) = self.registry.get(name) else {
            return Action::Nothing;
        };
        if !self.available_here(command) {
            return Action::Nothing;
        }
        let mutating = command.is_mutating();
        // Asked before the `Ctx` borrows the buffer, and asked once: both the
        // `before-save` hook and the refusal below turn on it, and they have
        // to agree or the hook fires for a save that is then declined.
        let writable = self.save_target().is_some();
        let buffer_id = self.current_buffer_id();
        let requires_selection = command.requires_selection();
        // The guards live here rather than in each command, so a command
        // cannot forget them.
        if mutating && self.buffers[buffer_id.0].read_only.refuses_edits() {
            // Found by a test for something else: pressing an editing key in a
            // buffer meowr wrote did nothing and said nothing. The #80 sweep
            // could not see it because it drives an ordinary buffer, and this
            // guard only fires in a read-only one.
            return self.refuse_aloud("This buffer is read-only.");
        }
        if requires_selection
            && self.buffers[buffer_id.0]
                .selection
                .current()
                .selection_type()
                .is_none()
        {
            return match self.fallbacks.get(name) {
                Some(fallback) => {
                    let fallback = fallback.to_owned();
                    self.dispatch_command(&fallback, key)
                }
                // `meow--selection-fallback` raises "No selection" when the
                // table has no entry — a signal *and*, in Emacs, a sentence.
                // meowr had only the signal, which made one origin account for
                // most of the wordless beeps #80 found: every command that
                // requires a selection and has no fallback arrived here.
                None => self.refuse_aloud("No selection."),
            };
        }

        // A composition's steps, dispatched one at a time so that each is
        // behind the guards above. The registry used to run them itself, which
        // meant a config could edit a read-only buffer and be told nothing
        // (#89) — the guards were in one place and the steps went round it.
        //
        // Cloned because dispatching borrows the session again, and a
        // composition is a handful of short strings written once when a config
        // loads. No depth counter: a cycle is refused when the declaration set
        // is applied, so a composition that reaches itself cannot be
        // registered at all.
        if let Some(steps) = self.registry.composition(name).map(<[String]>::to_vec) {
            return self.run_composed(name, &steps);
        }

        match name {
            // `repeat` names no behaviour of its own: it stands for whatever
            // ran last. It belongs here rather than above the guards — every
            // other command has had its State and read-only checks by now, and
            // `last_command` is not written until after this match, so it
            // still cannot record *itself* and repeat a repeat forever.
            "repeat" => {
                return match self.last_command.clone() {
                    // A fresh argument on the repeat is refused. This said
                    // Emacs "takes a fresh prefix if there is one", and the
                    // oracle disagrees: `w - \'` leaves the point exactly where
                    // `w` left it and raises, in every situation in the suite.
                    // So the recorded argument is the only one `repeat` uses,
                    // and asking for another is an error rather than an
                    // override.
                    Some(_) if !matches!(self.argument, Argument::None) => {
                        self.argument = Argument::None;
                        self.refuse_aloud("`repeat` takes no argument of its own.")
                    }
                    Some(last) if last != "repeat" => {
                        // The argument the repeated command had, so `- f x \'`
                        // searches backward the second time too.
                        self.argument = self.last_argument;
                        if let Some((ch, till)) = self.search_to_repeat(&last) {
                            let forward = self.take_search_direction(&last);
                            return self.search_for(ch, forward, till);
                        }
                        self.dispatch_command(&last, key)
                    }
                    // Nothing has run yet, or the only thing that has is
                    // `repeat` itself — the guard above that stops it
                    // repeating forever, and from outside the same nothing.
                    _ => self.refuse("Nothing to repeat."),
                };
            }
            // The session's own commands, from `SESSION_COMMANDS`. Every one
            // of them records itself, which the hand-written arms did not:
            // `G` left `last_command` alone, so `'` after it repeated whatever
            // came before the grab.
            _ if SESSION_COMMANDS.iter().any(|(n, _, _, _)| *n == name)
                && !(self.state == State::Beacon && BEACON_NOOPS.contains(&name)) =>
            {
                let run = SESSION_COMMANDS
                    .iter()
                    .find(|(n, _, _, _)| *n == name)
                    .map(|(_, _, _, f)| *f)
                    .expect("just matched");
                let action = run(self);
                self.remember(name, Argument::None);
                return action;
            }
            // The commands Meow remaps to `meow-beacon-noop`, and only those.
            // Motions stay live — you have to be able to move out of the Grab,
            // which is how Beacon State is left.
            _ if self.state == State::Beacon && BEACON_NOOPS.contains(&name) => {
                return Action::Nothing;
            }
            "find" | "till" => {
                let forward = self.take_search_direction(name);
                self.awaiting_char = Some(if name == "find" {
                    Awaiting::Find { forward }
                } else {
                    Awaiting::Till { forward }
                });
                return Action::Nothing;
            }
            "search" => {
                let backward = self.argument.value().is_some_and(|n| n < 0);
                self.argument = Argument::None;
                return self.search(!backward);
            }
            _ => {}
        }

        let window_commands = self.window_command(name);
        if let Some(action) = window_commands {
            self.remember(name, Argument::None);
            return action;
        }

        let Content::Ready(_) = self.buffers[buffer_id.0].content else {
            return Action::Nothing;
        };

        let window = self.window_range();
        // Read before the buffer is borrowed: the boundary below is inside
        // that borrow and cannot ask `self` again.
        let collapsing = self.collapsing;
        let argument = std::mem::take(&mut self.argument);
        let mut next = Argument::None;
        let previous = self.last_command.take();
        let buffer = &mut self.buffers[buffer_id.0];
        let Content::Ready(doc) = &mut buffer.content else {
            unreachable!("checked above")
        };
        // #64's rule is flat: *nothing* fires for a command run by a hook,
        // closures included. Read before `ctx` borrows the fields it needs.
        let in_hook = self.in_hook;
        let outcome = {
            let mut ctx = Ctx {
                doc,
                selection: &mut buffer.selection,
                state: &mut self.state,
                argument,
                next_argument: &mut next,
                options: &self.options,
                kill_ring: &mut self.kill_ring,
                history: &mut buffer.history,
                things: &self.things,
                text_objects: buffer
                    .syntax
                    .as_ref()
                    .map(|syntax| meowr_syntax::TextObjects {
                        syntax,
                        languages: &self.languages,
                    }),
                last_command: previous.as_deref(),
                key,
                window,
            };
            // `before-command` may refuse. Every hook runs even after a
            // refusal, so the outcome does not depend on registration order.
            if !in_hook && self.hooks.run(HookPoint::BeforeCommand, &mut ctx) == Proceed::No {
                None
            } else {
                let outcome = self.registry.invoke(name, &mut ctx);
                if !in_hook {
                    self.hooks.run(HookPoint::AfterCommand, &mut ctx);
                    // Only for a save that will actually be asked for.
                    // `before-save` ran on every `Outcome::Save` and the
                    // refusal below then dropped some of them, so a buffer
                    // with no file left Insert State for a write that never
                    // happened — the hook is named for the save, not for the
                    // asking.
                    if matches!(outcome, Some(Outcome::Save)) && writable {
                        self.hooks.run(HookPoint::BeforeSave, &mut ctx);
                    }
                }
                outcome
            }
        };
        // Emacs's `undo-boundary`, called between commands. Self-insertion is
        // the exception: consecutive typing amalgamates.
        if name != SELF_INSERT && !collapsing {
            buffer.history.boundary();
        }
        buffer.after_command(10);
        if matches!(outcome, Some(Outcome::Handled)) && mutating {
            buffer.since_autosave += 1;
        }
        self.remember(name, argument);
        self.argument = next;
        self.scroll_into_view();
        self.maybe_toggle_beacon();
        match outcome {
            // A save the host cannot carry out is refused here rather than
            // dropped there. `save_target` answers `None` for four different
            // reasons and the host knows none of them — it asked "what do I
            // write", not "why is there nothing" — so the sentence belongs on
            // this side of the seam, where the reasons are.
            // A buffer with no file is *asked* about rather than refused;
            // every other reason there is nothing to write still is. #80 gave
            // this refusal a voice and #84 gave it the feature the voice was
            // pointing at.
            Some(Outcome::Save) if !writable && self.nameable() => {
                // Carried across the name being read: see `accept_read`.
                self.skip_format_once = matches!(argument, Argument::Universal(_));
                self.read_line(narrow::Reading::SaveAs, "Write file: ", "");
                Action::Redraw
            }
            Some(Outcome::Save) if !writable => {
                let why = self.unwritable();
                self.refuse_aloud(why)
            }
            Some(Outcome::Save) => {
                self.skip_format_once = matches!(argument, Argument::Universal(_));
                Action::Save
            }
            Some(Outcome::Handled) => Action::Redraw,
            Some(Outcome::Refused(why)) => self.refuse_aloud(why),
            Some(Outcome::Failed) => {
                self.signals += 1;
                Action::Redraw
            }
            None => Action::Nothing,
        }
    }

    /// The commands that act on windows rather than on text.
    ///
    /// Handled here rather than in the registry because they need the window
    /// tree, which is session state and deliberately not in `Ctx` — a command
    /// that could rearrange the frame is a different kind of thing from one
    /// that edits a buffer, and the seam should say so.
    /// Park this window's point before the focus leaves it.
    fn park_point(&mut self) {
        let buffer = self.windows.current().buffer;
        let selection = self.buffers[buffer.0].selection.clone();
        self.windows.current_mut().point = Some((buffer, selection));
    }

    /// Take up the point the window now in focus was left with.
    ///
    /// Only when it is looking at the same buffer: a window that has been
    /// pointed at something else since has a point into a different text, and
    /// restoring it would be an offset with no meaning.
    fn take_up_point(&mut self) {
        let window = self.windows.current();
        let Some((parked, point)) = window.point.clone() else {
            return;
        };
        if parked == window.buffer {
            self.buffers[parked.0].selection = point;
        }
    }

    fn window_command(&mut self, name: &str) -> Option<Action> {
        // Every focus change goes through here, so the point can be parked and
        // taken up in one place rather than at each command that moves it.
        self.park_point();
        let action = match name {
            "split-below" => {
                self.windows.split(Direction::Vertical);
                Action::Redraw
            }
            "split-right" => {
                self.windows.split(Direction::Horizontal);
                Action::Redraw
            }
            "other-window" => {
                self.windows.next_window();
                Action::Redraw
            }
            "delete-window" => {
                if self.windows.close() {
                    Action::Redraw
                } else {
                    // The last window cannot be closed, which Emacs also
                    // refuses rather than quitting — and says so, where this
                    // only rang the bell.
                    self.refuse_aloud("This is the only window.")
                }
            }
            "delete-other-windows" => {
                self.windows.close_others();
                Action::Redraw
            }
            "next-buffer" => {
                // Past the leavings of a cancelled preview and past killed
                // slots, which `picker_candidates` also hides: neither is a
                // buffer anyone opened.
                if let Some(next) = self.live_buffer_after(self.current_buffer_id()) {
                    self.show(next);
                }
                Action::Redraw
            }
            _ => return None,
        };
        self.windows.layout(self.area);
        self.take_up_point();
        Some(action)
    }

    /// `recenter-top-bottom`, less the top and bottom.
    ///
    /// Emacs cycles middle, top, bottom on repeated presses. This is the
    /// middle only: the cycle needs `last-command` to decide where it is in
    /// the sequence, and meowr has no such command yet — one that reads its
    /// own repetition would be the first. Recorded rather than half-built.
    fn recenter(&mut self) -> Action {
        let Some((_, height, at)) = self.view() else {
            return self.refuse("Still reading this file.");
        };
        self.windows.current_mut().top = at.saturating_sub(height / 2);
        Action::Redraw
    }

    /// `scroll-up-command`: a screenful further on.
    ///
    /// The other direction from [`Session::scroll_into_view`], and the one
    /// meowr had no command for at all: the view moves and the selection is
    /// brought along, where every other command moves the selection and lets
    /// the view follow. Without it the only way to reach line 400 was to
    /// select your way there.
    fn scroll_up(&mut self) -> Action {
        self.page(1)
    }

    /// `scroll-down-command`: a screenful back.
    fn scroll_down(&mut self) -> Action {
        self.page(-1)
    }

    /// Move the view `screens` screenfuls, then bring the selection into it.
    ///
    /// A screenful is the window less two lines, which is Emacs's
    /// `next-screen-context-lines`: the overlap is what lets you read a
    /// paragraph that straddles the break without scrolling back.
    ///
    /// Stops at the edges rather than signalling. Emacs raises
    /// `beginning-of-buffer` at the top, but it does so having *already*
    /// scrolled as far as it can, and a bell at the end of a file you are
    /// paging through is noise rather than news.
    fn page(&mut self, screens: i32) -> Action {
        let Some((count, height, at)) = self.view() else {
            return self.refuse("Still reading this file.");
        };
        let screenful = height.saturating_sub(2).max(1);
        let Some(doc) = self.text_of_current() else {
            return Action::Nothing;
        };
        let folds = self.current().folds().clone();
        // Clamped so the *last screenful is full*, not so the last line can be
        // dragged to the top: bounding by `count - 1` let paging to the end
        // leave one row of text above ten blank ones, which no editor does.
        // In **visible** lines, like everything else here: the last full
        // screenful begins a screenful back from the end, and with a fold in
        // the way that is a different line from `count - height`.
        let ceiling = folds.backward(&doc, count.saturating_sub(1), height.saturating_sub(1));
        let was = self.windows.current().top;
        let top = if screens > 0 {
            folds.forward(&doc, was, screenful).min(ceiling)
        } else {
            folds.backward(&doc, was, screenful)
        };
        if top == was {
            // Emacs raises `End of buffer` here and only here: it scrolls as
            // far as it can first, and signals when it cannot move at all.
            // Measured — paging a 60-line buffer runs quiet until the view
            // stops, then raises on every press after. This ticket's own
            // acceptance criterion said "stops rather than signalling", which
            // was written before anybody asked Emacs.
            return self.refuse_aloud(if screens > 0 {
                "End of buffer."
            } else {
                "Beginning of buffer."
            });
        }
        self.windows.current_mut().top = top;
        // The selection follows the view rather than the other way round, and
        // only as far as it has to: a point already on screen stays where it
        // is, so paging back to where you were leaves it undisturbed.
        let bottom = folds.forward(&doc, top, height.saturating_sub(1));
        let wanted = at.clamp(top, bottom.min(count.saturating_sub(1)));
        // Never onto a line nothing draws: a point inside a fold is a cursor
        // that has gone missing, which is the thing folding moves it out of.
        let wanted = folds.next_visible(&doc, wanted);
        if wanted != at {
            let byte = self
                .current()
                .document()
                .and_then(|doc| doc.line_to_byte(wanted))
                .unwrap_or(0);
            let buffer = self.current_mut();
            buffer.selection.select_without_history(Selection::at(byte));
        }
        Action::Redraw
    }

    /// What both view commands need: how many lines the buffer has, how many
    /// rows the Window shows, and which line the selection is on.
    ///
    /// `None` while the buffer is still being read, which is the one case both
    /// of them decline for the same reason.
    fn view(&self) -> Option<(usize, usize, usize)> {
        let window = self.windows.current();
        let doc = self.buffers[window.buffer.0].document()?;
        let at = doc
            .byte_to_line(self.current().selection.current().pos())
            .unwrap_or(0);
        Some((
            doc.line_count(),
            text_area(window.area).height.max(1) as usize,
            at,
        ))
    }

    /// Keep the point on screen after a motion.
    ///
    /// Per window, because each has its own scroll position — the same buffer
    /// in two windows scrolls in each independently, which is the reason to
    /// split at all.
    fn scroll_into_view(&mut self) {
        let window = self.windows.current();
        let buffer = &self.buffers[window.buffer.0];
        let Some(doc) = buffer.document() else {
            return;
        };
        let line = doc
            .byte_to_line(buffer.selection.current().pos())
            .unwrap_or(0);
        let height = text_area(window.area).height.max(1) as usize;
        let top = window.top;
        let folds = &buffer.folds;
        // **Reckoned in visible lines, not document ones** (ADR-0012). The last
        // row is `height - 1` *visible* lines down from the top, and the top
        // that puts `line` on the last row is the same walk backwards. Counting
        // documents lines instead asks for a top inside a fold, which draws
        // nothing -- so the view starts at the far side of it and throws the
        // fold's head off the screen for a step of one row.
        let last_row = folds.forward(doc, top, height.saturating_sub(1));
        let new_top = if line < top {
            folds.previous_visible(doc, line)
        } else if line > last_row {
            folds.backward(doc, line, height.saturating_sub(1))
        } else {
            top
        };
        self.windows.current_mut().top = new_top;
    }

    /// What the Echo Area should show, if anything.
    /// Which of the four things that can own the Bottom does.
    ///
    /// The glossary's sentence, made into code: *which one it is, is a fact
    /// about the session and not about the layer drawing it*. It was a free
    /// function in the binary and then, briefly, one in `meowr-view`, whose own
    /// module doc says it makes no decisions -- both of them a layer reading
    /// four accessors back and re-deriving something this type already knows.
    ///
    /// It costs almost nothing to ask, because the session had already made
    /// the decision three times over and this is where those live:
    /// [`Session::hover_view`] hands back `None` behind a widget, behind a menu
    /// and behind an unread message (#122, #123), so the cascade below is
    /// mostly reading answers rather than computing them. The widgets come
    /// first because both take the keyboard, and a hover or a message drawn
    /// under one would be unreachable as well as unread; a hover outranks a
    /// message because it is the answer to a key just pressed.
    pub fn bottom(&self) -> meowr_view::Bottom {
        use meowr_view::Bottom;
        if self.narrow.widget.is_some() {
            Bottom::Narrow
        } else if self.menu_visible() && !self.menu().is_empty() {
            Bottom::Menu
        } else if self.hover_view().is_some() {
            Bottom::Hover
        } else if self.echo.is_some() {
            Bottom::Echo
        } else {
            Bottom::None
        }
    }

    pub fn echo(&self) -> Option<&str> {
        self.echo.as_deref()
    }

    /// Where a hover was asked for: the file, and the byte offset in it.
    ///
    /// Drained rather than read, as `take_autosaves_to_forget` is: a signal
    /// answered twice would ask the server the same question again on the next
    /// key, and the second answer would arrive with nothing waiting for it.
    pub fn pending_hover(&mut self) -> Option<(String, usize)> {
        self.pending_hover.take()
    }

    /// What a language server was asked for a list of.
    ///
    /// The three rows differ only in the question, so they differ only here --
    /// three commands that build one of these and hand it to the host, rather
    /// than three paths through the session.
    pub fn pending_lookup(&mut self) -> Option<Lookup> {
        self.pending_lookup.take()
    }

    /// A rename's question for a language server. Drained.
    pub fn pending_rename(&mut self) -> Option<RenameQuestion> {
        self.pending_rename.take()
    }

    /// `lsp-rename`, Doom's `SPC c r`: rename the symbol at the point.
    ///
    /// Asks the server to prepare first; the name is read when it answers.
    /// A buffer with no server is refused by the host, as every row that asks
    /// a server is.
    fn lsp_rename(&mut self) -> Action {
        let buffer = self.current();
        let Some(path) = buffer.path().map(str::to_owned) else {
            return self.refuse_aloud("No file to rename in.");
        };
        self.pending_rename = Some(RenameQuestion::Prepare(RenameSite {
            path,
            buffer: self.current_buffer_id(),
            at: buffer.selection.current().pos(),
            revision: buffer.revision(),
        }));
        Action::Rename
    }

    /// What a server said to a rename's first question.
    ///
    /// Dropped quietly, as `found` drops an answer, if another buffer is on
    /// screen or a widget has opened: a prompt is not something to open over
    /// the user's picker, nor about a file they have left. Dropped and said if
    /// the text has been typed over, because the rename has to be asked again
    /// and the message says why (ADR-0013). Otherwise the new name is read
    /// seeded with the old one, lsp-mode's `read-string`.
    pub fn rename_prepared(&mut self, site: &RenameSite, prepared: Prepared) -> Action {
        if self.current_buffer_id() != site.buffer || self.narrow.widget.is_some() {
            return Action::Nothing;
        }
        let from = self.server_name(site.buffer);
        if self.current().revision() != site.revision {
            return self.refuse(format!(
                "{} changed while {from} was working, so nothing was renamed.",
                site.path
            ));
        }
        let doc = self.current().document();
        let placeholder = match prepared {
            Prepared::Placeholder(name) => Some(name),
            Prepared::Range { start, end } => doc.and_then(|doc| {
                let range = doc.range_for(start, end).ok()?;
                Some(doc.text().byte_slice(range).to_string())
            }),
            Prepared::Default => self.symbol_at(site.at),
            Prepared::Nothing => None,
            Prepared::Refused(why) => return self.refuse(in_words(&from, &why)),
            Prepared::Unsupported => return self.refuse(format!("{from} cannot rename.")),
            Prepared::Failed(why) => return self.refuse(format!("Could not rename: {why}.")),
        };
        let Some(placeholder) = placeholder else {
            return self.refuse("Nothing to rename at the point.");
        };
        self.read_line(
            narrow::Reading::Rename(site.clone()),
            &format!("Rename {placeholder} to: "),
            &placeholder,
        );
        Action::Redraw
    }

    /// The new name, read: ask for the rename itself, stamped now, as the
    /// question goes out (ADR-0013). A name unchanged is still asked, as
    /// lsp-mode asks it; what the server makes of that is the server's to say.
    pub(crate) fn rename_to(&mut self, site: RenameSite, name: String) -> Action {
        if name.is_empty() {
            return self.refuse("No name given, so nothing was renamed.");
        }
        if self.current_buffer_id() != site.buffer || self.current().revision() != site.revision {
            return self.refuse(format!("{} changed, so nothing was renamed.", site.path));
        }
        self.pending_rename = Some(RenameQuestion::Apply(RenameApply {
            site,
            name,
            asked: self.revisions(),
        }));
        Action::Rename
    }

    /// What a server said to a rename: its workspace edit, applied all or
    /// nothing, or why there is none (#168).
    pub fn renamed(&mut self, apply: &RenameApply, answer: EditReply) -> Action {
        let from = self.server_name(apply.site.buffer);
        match answer {
            EditReply::Edit(changes) => self.apply_workspace_edit(WorkspaceEdit {
                from,
                asked: apply.asked.clone(),
                changes,
            }),
            EditReply::Nothing => {
                self.say(&format!("{from} renamed nothing."));
                Action::Redraw
            }
            EditReply::Refused(why) => self.refuse(in_words(&from, &why)),
            EditReply::Failed(why) => self.refuse(format!("Could not rename: {why}.")),
        }
    }

    /// The file to copy or move, and where to. Drained.
    pub fn pending_file_work(&mut self) -> Option<FileRequest> {
        self.pending_file_work.take()
    }

    /// `doom/move-this-file` and `doom/copy-this-file`, on `SPC f m` and
    /// `SPC f C` (#178).
    ///
    /// The target is read seeded with the path, so editing the basename is one
    /// edit and moving to another directory is another. A prefix argument is
    /// carried across the read, as a save-as carries `SPC u`, and means "write
    /// over what is there" -- `doom/copy-this-file` takes any prefix (its
    /// `(interactive "P")`), so `SPC u 4` forces as `SPC u` does.
    ///
    /// A preview is not checked for: a command cannot run while one is on
    /// screen, because the narrowing widget owns the keyboard until it closes
    /// and closing it puts the window back.
    /// Take the prefix argument, and say whether there was one.
    ///
    /// `SPC u` before a key is the go-ahead on all three of M7's file rows:
    /// write over what is there for a copy or a move (#178), and delete at all
    /// for `SPC f D` (#179).
    ///
    /// Taken, not read. A command the session carries out itself does not come
    /// through the dispatch that consumes the argument, and one left behind
    /// would arm the *next* press of any of them -- a move forcing its way past
    /// a name that is taken, or a delete that never stopped to ask.
    fn take_prefix(&mut self) -> bool {
        !matches!(
            std::mem::take(&mut self.argument),
            meowr_command::Argument::None
        )
    }

    fn ask_file_work(&mut self, work: FileWork) -> Action {
        // Taken before the refusal, not after. A command that returns early
        // never reaches the dispatch that consumes the argument, so one left
        // here would arm the *next* press: a move that wrote over a name it
        // should have refused, having been told to go ahead by a keystroke
        // meant for a buffer that had no file at all.
        let force = self.take_prefix();
        let Some(path) = self.current().path().map(str::to_owned) else {
            return self.refuse_aloud("This buffer has no file.");
        };
        // The read carries it onward, as a save-as carries `SPC u` (ADR-0014).
        self.read_line(
            narrow::Reading::File(FileAsk { work, from: path.clone(), force }),
            work.asking(),
            &path,
        );
        Action::Redraw
    }

    fn move_this_file(&mut self) -> Action {
        self.ask_file_work(FileWork::Move)
    }

    fn copy_this_file(&mut self) -> Action {
        self.ask_file_work(FileWork::Copy)
    }

    /// The file to delete. Drained.
    pub fn pending_file_deletion(&mut self) -> Option<FileDeletion> {
        self.pending_file_deletion.take()
    }

    /// `doom/delete-this-file`, on `SPC f D` (#179).
    ///
    /// Doom asks `y-or-n-p "Really delete %S?"`; meowr has no yes/no read and
    /// has met that wall four times already, so this is the fifth answer of the
    /// same shape -- refuse, and name the key that means yes (#173). The key is
    /// `SPC u`, which is the go-ahead across copy, move and delete rather than
    /// a second command name to remember.
    ///
    /// The sentence says it is an unlink. Every other refusal meowr makes is
    /// about work it is still holding, and can be gone back on by saving; this
    /// one is about a file, and nothing in the editor can undo a filesystem.
    ///
    /// No path argument and no picker, so there is no way to delete a file you
    /// are not looking at.
    ///
    /// The preview arm cannot be reached today, and is here anyway. `SPC f m`
    /// leaves the same case out and gives the reason: a command cannot run
    /// while a preview is on screen, because the narrowing widget owns the
    /// keyboard until it closes and closing it puts the window back. The
    /// difference is what the two commands do if that ever stops being true --
    /// a move can be moved back, and this cannot. #173 asked for it by name,
    /// and it stays until a preview can be current with a key to press.
    fn delete_this_file(&mut self) -> Action {
        // Before the refusals below, for the reason `ask_file_work` takes it
        // first: a go-ahead left behind by a refused press is one this command
        // would find on the *next* press and delete a file without asking.
        let go_ahead = self.take_prefix();
        if !self.current().is_real() {
            return self.refuse_aloud("This buffer is a preview; nothing here is yours to delete.");
        }
        let Some(path) = self.current().path().map(str::to_owned) else {
            return self.refuse_aloud("This buffer has no file.");
        };
        if !go_ahead {
            return self.refuse_aloud(format!(
                "{path} would be deleted, not trashed, and nothing here can undo it. \
                 {} to delete it.",
                self.go_ahead_for("delete-this-file"),
            ));
        }
        self.pending_file_deletion = Some(FileDeletion { path });
        Action::FileDeleted
    }

    /// Whether the file went, and what that leaves behind (#179).
    ///
    /// An **unmodified** buffer is killed with its file, in every window, which
    /// is Doom's `doom/kill-this-buffer-in-all-windows` and the honest answer
    /// for a copy of something that is no longer there. A **modified** one is
    /// kept, because its text is then the only copy of that work left anywhere,
    /// and the sentence says exactly that rather than leaving it to be noticed.
    ///
    /// The auto-save goes either way. One beside a file nobody can open is an
    /// offer to recover work into a dead end -- unlike [`Session::discard`],
    /// which keeps an auto-save next to an *unmodified* buffer because there
    /// the file is still there to recover into. If the buffer stays, the timer
    /// writes a new one, which is the truth: that work is still unsaved.
    ///
    /// The buffer is found by path rather than taken as the current one. They
    /// are the same buffer here today, but the one that held the file that went
    /// is the one this is about, and #150's spelling is what makes the question
    /// answerable at all.
    pub fn file_deleted(&mut self, asked: &FileDeletion, outcome: Result<(), String>) -> Action {
        let FileDeletion { path } = asked;
        if let Err(why) = outcome {
            return self.refuse(format!("{path} could not be deleted: {why}."));
        }
        self.autosaves_to_forget.push(autosave_path(path));
        // The Trail is left alone, because ADR-0011 already consumes a Place
        // whose file has gone and says so. The recent files have no such
        // answer -- a row that opens nothing is an offer that can only
        // disappoint -- so the path leaves the list, as it leaves `recentf`.
        self.recent.gone(path);
        let kept = match self.buffer_visiting(path) {
            Some(id) if !self.buffer(id).is_modified() => {
                self.entomb(id);
                String::new()
            }
            // Not named again: it is the buffer the key was pressed in, and
            // the sentence has just said which file it was.
            Some(_) => {
                " This buffer is still open and modified, so its text is the only copy of that \
                 work left."
                    .to_string()
            }
            None => String::new(),
        };
        self.say(&format!("Deleted {path}.{kept}"));
        Action::Redraw
    }

    /// The target, read: ask the host to do it.
    pub(crate) fn file_work_to(&mut self, asked: FileAsk, to: String) -> Action {
        let FileAsk { work, from, force } = asked;
        if to.is_empty() {
            return self.refuse(format!("No name given, so nothing was {}.", work.past()));
        }
        self.pending_file_work = Some(FileRequest { from, to, work, force });
        Action::FileWorked
    }

    /// What the filesystem did (#178).
    ///
    /// A move is followed: the buffer takes the new path, the auto-save left
    /// under the old one is forgotten, and the recent files are rewritten in
    /// place. A copy is opened, as choosing a file in the picker opens one --
    /// the host reads it, because reading is its half. Nothing in the session
    /// moves unless the file did.
    pub fn file_done(&mut self, asked: &FileRequest, outcome: Result<FileDone, FileRefusal>) -> Action {
        let FileRequest { from, work, .. } = asked;
        let done = match outcome {
            Ok(done) => done,
            Err(FileRefusal::Taken(path)) => {
                // The path in the way rather than the name typed: `src` and
                // `src/main.rs` are the same answer to type and different
                // answers to read.
                return self.refuse(format!(
                    "{path} exists. {} to {} it there anyway.",
                    self.go_ahead_for(work.command()),
                    work.verb(),
                ));
            }
            Err(FileRefusal::Itself) => {
                return self.refuse(format!("{from} is already where it is."));
            }
            Err(FileRefusal::Failed(why)) => {
                return self.refuse(format!("{from} could not be {}: {why}.", work.past()));
            }
        };
        let made = done
            .made_directories
            .then(|| directory_of(&done.path))
            .flatten()
            .map_or_else(String::new, |directory| format!(", creating {directory}"));
        let said = format!("{} {from} to {}{made}.", work.past_capital(), done.path);
        // A buffer already visiting the target held the file that was just
        // written over. Two buffers with one path is what #150's spelling
        // exists to prevent -- `buffer_visiting` answers with the first — so
        // the one whose file is gone is killed where nothing is lost by it, and
        // kept and said where something would be. #173's rule for a deleted
        // file, and for its reason.
        let displaced = self
            .buffer_visiting(&done.path)
            .filter(|id| Some(*id) != self.buffer_visiting(from));
        let overwritten = match displaced {
            Some(id) if !self.buffer(id).is_modified() => {
                self.entomb(id);
                String::new()
            }
            Some(id) => format!(
                " The buffer that had {} still holds unsaved text under that name.",
                self.buffer(id).name()
            ),
            None => String::new(),
        };
        match work {
            FileWork::Move => {
                // The auto-save beside the old name is work under a name
                // nothing visits now: an offer to recover it would name a file
                // that is not there.
                self.autosaves_to_forget.push(autosave_path(from));
                self.recent.moved(from, &done.path);
                self.respell_path(from, &done.path);
                self.say(&format!("{said}{overwritten}"));
                Action::Redraw
            }
            FileWork::Copy => {
                self.say(&format!("{said}{overwritten}"));
                self.open_chosen(done.path)
            }
        }
    }

    /// A code action question for a language server. Drained.
    pub fn pending_code_actions(&mut self) -> Option<CodeActionQuestion> {
        self.pending_code_actions.take()
    }

    /// `lsp-execute-code-action`, Doom's `SPC c a`: offer the code actions at
    /// the point.
    fn lsp_execute_code_action(&mut self) -> Action {
        self.ask_code_actions(None)
    }

    /// `lsp-organize-imports`, Doom's `SPC c o`: apply the server's
    /// `source.organizeImports` action.
    fn lsp_organize_imports(&mut self) -> Action {
        self.ask_code_actions(Some(ORGANIZE_IMPORTS))
    }

    /// Ask for the code actions over the selection, or with none the line, with
    /// the diagnostics on the point's line: lsp-mode's question.
    fn ask_code_actions(&mut self, only: Option<&str>) -> Action {
        let buffer = self.current();
        let (Some(path), Some(doc)) = (buffer.path(), buffer.document()) else {
            return self.refuse_aloud("No file to act on.");
        };
        let selection = buffer.selection.current();
        let range = if selection.is_empty() {
            line_bounds(doc, selection.pos())
        } else {
            selection.range()
        };
        // By line number, as `lsp-cur-line-diagnostics` compares them: a range
        // ending exactly where this line starts ends on the line before.
        let line_of = |offset: usize| doc.byte_to_line(offset).unwrap_or_else(|| doc.line_count());
        let line = line_of(selection.pos());
        let diagnostics = buffer
            .diagnostics()
            .iter()
            .filter(|d| {
                let last = d.range.end.saturating_sub(1).max(d.range.start);
                line_of(d.range.start) <= line && line <= line_of(last)
            })
            .cloned()
            .collect();
        self.pending_code_actions = Some(CodeActionQuestion::List(CodeActionsAsked {
            path: path.to_string(),
            buffer: self.current_buffer_id(),
            range,
            revision: buffer.revision(),
            diagnostics,
            only: only.map(str::to_string),
            asked: self.revisions(),
        }));
        Action::CodeActions
    }

    /// What a server offered, on its way into the picker or straight to being
    /// applied.
    ///
    /// Dropped as a rename's prepared answer is: quietly for a buffer left or a
    /// widget opened since, and said for text typed over. One action is applied
    /// without a picker -- `lsp-auto-execute-action`, on by default -- and for
    /// `SPC k o` only the kind asked for counts, since a server may ignore
    /// `only`; rust-analyzer does (#157).
    pub fn code_actions_found(&mut self, asked: &CodeActionsAsked, found: CodeActionsFound) -> Action {
        if self.current_buffer_id() != asked.buffer || self.narrow.widget.is_some() {
            return Action::Nothing;
        }
        let from = self.server_name(asked.buffer);
        if self.current().revision() != asked.revision {
            return self.refuse(format!(
                "{} changed while {from} was working, so no action was offered.",
                asked.path
            ));
        }
        let actions = match found {
            CodeActionsFound::Actions(actions) => actions,
            CodeActionsFound::Unsupported => return self.refuse(format!("{from} offers no code actions.")),
            CodeActionsFound::Refused(why) => return self.refuse(in_words(&from, &why)),
            CodeActionsFound::Failed(why) => return self.refuse(format!("Could not ask for code actions: {why}.")),
        };
        let (wanted, others): (Vec<CodeAction>, Vec<CodeAction>) = match &asked.only {
            None => (actions, Vec::new()),
            Some(kind) => actions.into_iter().partition(|action: &CodeAction| {
                action
                    .kind
                    .as_deref()
                    .is_some_and(|k| k == kind || k.starts_with(&format!("{kind}.")))
            }),
        };
        match (asked.only.as_deref(), <[CodeAction; 1]>::try_from(wanted)) {
            (_, Ok([only])) => self.choose_action(asked.clone(), only),
            (None, Err(none)) if none.is_empty() => self.refuse("No code actions here."),
            (Some(kind), Err(none)) if none.is_empty() => {
                let wanted = match kind {
                    ORGANIZE_IMPORTS => "to organize imports".to_string(),
                    kind => format!("of kind {kind}"),
                };
                let titles: Vec<&str> = others.iter().map(|a| a.title.as_str()).collect();
                self.refuse(match titles.as_slice() {
                    [] => format!("{from} has no action {wanted}."),
                    titles => format!("{from} has no action {wanted}; here it offers {}.", listed(titles)),
                })
            }
            (_, Err(several)) => self.offer_code_actions(asked.clone(), several),
        }
    }

    /// A code action chosen: applied if it came with its edit, resolved first
    /// if not, and said if all it has is a command meowr does not run.
    pub(crate) fn choose_action(&mut self, asked: CodeActionsAsked, action: CodeAction) -> Action {
        let from = self.server_name(asked.buffer);
        match action.effect {
            CodeActionEffect::Command => {
                return self.refuse(format!(
                    "{} is a command for {from} to run, and meowr does not run server commands.",
                    action.title
                ));
            }
            CodeActionEffect::Nothing => {
                return self.refuse(format!(
                    "{} has no edit {from} gave meowr to apply.",
                    action.title
                ));
            }
            CodeActionEffect::Edit(_) | CodeActionEffect::Resolve(_) => {}
        }
        self.pending_code_actions = Some(CodeActionQuestion::Apply(ChosenCodeAction { asked, action }));
        Action::CodeActions
    }

    /// What applying a code action came to: its workspace edit applied all or
    /// nothing, stamped when the actions were asked for, or why there is none.
    pub fn code_action_applied(&mut self, chosen: &ChosenCodeAction, reply: EditReply) -> Action {
        let from = self.server_name(chosen.asked.buffer);
        let title = &chosen.action.title;
        match reply {
            EditReply::Edit(changes) => {
                let applied = self.apply_workspace_edit(WorkspaceEdit {
                    from,
                    asked: chosen.asked.asked.clone(),
                    changes,
                });
                // Said after what the edit came to, rather than not at all:
                // lsp-mode runs the command next, and silence would read as
                // having done it.
                if let (Some(command), Some(said)) = (&chosen.action.command, self.echo.as_mut()) {
                    said.push_str(&format!(" Its command, {command}, was not run."));
                }
                applied
            }
            EditReply::Nothing => {
                self.say(&format!("{title} changed nothing."));
                Action::Redraw
            }
            EditReply::Refused(why) => self.refuse(in_words(&from, &why)),
            EditReply::Failed(why) => self.refuse(format!("Could not apply {title}: {why}.")),
        }
    }

    /// The language server's name for a buffer, as a sentence names it: the
    /// program without its directory. A buffer killed since has no Language
    /// left to ask, and is given the generic name.
    fn server_name(&self, id: BufferId) -> String {
        self.language_server(id)
            .map(|server| {
                std::path::Path::new(&server.program)
                    .file_name()
                    .map_or_else(|| server.program.clone(), |name| name.to_string_lossy().into_owned())
            })
            .unwrap_or_else(|| "The language server".to_string())
    }

    /// What is to be searched for, and where.
    ///
    /// Drained rather than read, as every other pending question is: one
    /// answered twice would run `rg` again with nothing waiting for the second
    /// answer.
    pub fn pending_search(&mut self) -> Option<Search> {
        self.pending_search.take()
    }

    fn ask_lookup(&mut self, kind: LookupKind, nothing: &str) -> Action {
        let buffer = self.current();
        let Some(path) = buffer.path().map(str::to_owned) else {
            return self.refuse_aloud(nothing);
        };
        let at = buffer.selection.current().pos();
        self.pending_lookup = Some(Lookup { path, at, kind });
        Action::Lookup
    }

    /// `+lookup/references`: everywhere the symbol at the point is used.
    fn lookup_references(&mut self) -> Action {
        self.ask_lookup(LookupKind::References, "No file to look for references in.")
    }

    /// `+lookup/definition`: where the symbol at the point is defined.
    fn lookup_definition(&mut self) -> Action {
        self.ask_lookup(
            LookupKind::Definition,
            "No file to look for a definition in.",
        )
    }

    /// `+lookup/type-definition`: where the type of the symbol at the point is
    /// defined.
    fn lookup_type_definition(&mut self) -> Action {
        self.ask_lookup(
            LookupKind::TypeDefinition,
            "No file to look for a type definition in.",
        )
    }

    /// `+lookup/implementations`: what implements the symbol at the point.
    fn lookup_implementations(&mut self) -> Action {
        self.ask_lookup(
            LookupKind::Implementations,
            "No file to look for implementations in.",
        )
    }

    /// What a server found, on its way into the picker.
    ///
    /// ADR-0009's arrival check, and the same one a key-asked hover makes: the
    /// answer is dropped if the *file* changed, and not merely if the point
    /// moved. Three keys were pressed for this, so a point that has drifted a
    /// word does not make the answer unwanted -- where a buffer that is no
    /// longer the one it was asked about does.
    ///
    /// **One Place is a jump**, which is `consult-xref` and so Doom's for every
    /// row that arrives here: `(if (cdr candidates) (consult--read ...) (car
    /// candidates))`. Here rather than in `open_results`, because the search
    /// rows open a picker over a single hit -- `consult-ripgrep` has no such
    /// branch -- and this is the door only the lookups come through (#146).
    ///
    /// **And dropped if a widget has opened since**, which is `searched`'s rule
    /// for `searched`'s reason: it owns the Bottom, and neither a picker opened
    /// over it nor a jump behind it is something the user asked for mid-way
    /// through another. The jump would also spend that picker's `from`.
    pub fn found(&mut self, about: &str, places: Vec<Place>) -> Action {
        if self.current().path() != Some(about) || self.narrow.widget.is_some() {
            return Action::Nothing;
        }
        match <[Place; 1]>::try_from(places) {
            Ok([only]) => self.jump_to(only),
            Err(places) => self.open_results(places),
        }
    }

    /// `SPC k x`: what a language server has said about this buffer.
    ///
    /// ADR-0011's one exception. Ten rows that produce many results go to the
    /// picker; this one goes to a generated buffer, because a diagnostic list
    /// is read top to bottom and acted on rather than fuzzy-matched -- the line
    /// Emacs draws itself, where `flycheck-list-errors` gets a buffer and
    /// `xref` gets the minibuffer.
    ///
    /// **Empty is not one thing.** A clean buffer is a true answer and gets a
    /// real buffer saying so; a server that is absent or still starting has not
    /// produced an empty list, and an empty `*diagnostics*` would be claiming
    /// "none" where the truth is "nobody asked" -- ADR-0010's distinction,
    /// borrowed whole.
    fn list_diagnostics(&mut self) -> Action {
        if self.language_server(self.current_buffer_id()).is_none() {
            return self.refuse_aloud("No language server for this buffer.");
        }
        if self.current().heard_diagnostics().is_none() {
            return self.refuse_aloud("The language server has not answered yet.");
        }
        let Some((text, places)) = self.diagnostics_listing() else {
            return self.refuse_aloud("Nothing to list diagnostics against.");
        };
        let listing = Listing::new(places, self.here());
        let listed = listing.names_anywhere();
        self.show_generated(DIAGNOSTICS, &text, Some(listing));
        // Looked up rather than written down, which is what `describe_bindings`
        // goes to the same trouble for: the report behind `quit-window` was as
        // much about nothing saying which key as about the key not existing.
        // Nothing is said about visiting a list with nothing in it.
        let advice: Vec<String> = [("visit-place", "to visit"), ("quit-window", "to close")]
            .iter()
            .filter(|(name, _)| listed || *name != "visit-place")
            .filter_map(|(name, what)| Some(format!("{} {what}", self.written(name)?)))
            .collect();
        if !advice.is_empty() {
            self.echo = Some(format!("{}.", advice.join(", ")));
        }
        Action::Redraw
    }

    /// This buffer's diagnostics as lines, and what each line names.
    ///
    /// The two are built as one list and split at the end, deliberately. A
    /// text assembled here and Places assembled beside it is the same
    /// arithmetic written twice, and the day the heading grows a line the jump
    /// key lands one row off with nothing to say it has.
    fn diagnostics_listing(&self) -> Option<(String, Vec<Option<Place>>)> {
        let buffer = self.current();
        let path = buffer.path()?;
        let doc = buffer.document()?;
        // Read top to bottom, which is the whole reason this is a buffer. A
        // server answers in the order it found them, which is its own.
        let mut found: Vec<&meowr_domain::Diagnostic> = buffer.diagnostics().iter().collect();
        found.sort_by_key(|diagnostic| diagnostic.range.start);
        let rows: Vec<(String, String, Place)> = found
            .iter()
            .filter_map(|diagnostic| {
                // Dropped rather than clamped, and the two cases it can be are
                // both already answered elsewhere: a range past the end of the
                // text is one computed against text that has gone, which the
                // revision guard refuses on arrival, and putting it at the end
                // instead would be naming a line it says nothing about.
                let (line, column) = doc.position_at(diagnostic.range.start)?;
                Some((
                    format!("{}:{}", line + 1, column + 1),
                    // One line, because a row here *is* one: a message broken
                    // over three would give two of them no Place and put the
                    // third's under a line it says nothing about. Kept whole
                    // rather than cut at the first line -- what the server said
                    // is the reason to open this buffer at all.
                    format!(
                        "{:7}  {}",
                        diagnostic.severity.name(),
                        diagnostic.message.lines().collect::<Vec<_>>().join(" ")
                    ),
                    Place::new(path, line, column),
                ))
            })
            .collect();
        let width = rows.iter().map(|(at, ..)| at.len()).max().unwrap_or(0);
        let body: Vec<(String, Option<Place>)> = if rows.is_empty() {
            // A true answer, and the one case that earns a buffer rather than
            // a refusal: this buffer has been looked at and is clean.
            vec![("  No diagnostics.".to_string(), None)]
        } else {
            rows.into_iter()
                .map(|(at, said, place)| (format!("  {at:width$}  {said}"), Some(place)))
                .collect()
        };
        let lines: Vec<(String, Option<Place>)> = [
            // Named the way the modeline names it rather than by the path the
            // buffer holds. Found in a real terminal: meowr opened on an
            // absolute path puts a heading wider than the frame, and the answer
            // cannot be a second rule for shortening a path when there is
            // already one on screen two rows below.
            (
                format!(
                    "DIAGNOSTICS  {}",
                    meowr_view::modeline::short_path(path, meowr_view::modeline::PATH_SEGMENTS)
                ),
                None,
            ),
            (String::new(), None),
        ]
        .into_iter()
        .chain(body)
        .collect();
        let text = lines
            .iter()
            .map(|(line, _)| line.as_str())
            .collect::<Vec<_>>()
            .join("\n")
            + "\n";
        Some((text, lines.into_iter().map(|(_, place)| place).collect()))
    }

    /// Go to the Place the line under the point names.
    ///
    /// One **global** command, because meowr has no buffer-local keymaps
    /// (ADR-0011): it means something in a buffer with Places in it and is
    /// inert everywhere else, which is exactly what `quit-window` already is.
    /// A keymap architecture may well be right one day; it should be argued by
    /// a ticket with two customers rather than smuggled in behind a list.
    fn visit_place(&mut self) -> Action {
        let buffer = self.current();
        let Some(place) = buffer
            .document()
            .and_then(|doc| doc.byte_to_line(buffer.selection.current().pos()))
            .and_then(|line| buffer.listing()?.place_on(line))
            .cloned()
        else {
            return Action::Nothing;
        };
        self.jump_to(place)
    }

    /// `+default/search-cwd`: search the directory this buffer's file is in.
    ///
    /// The one of the four rows that needs no Project: a path has a parent
    /// whether or not anything has ever been committed there.
    fn search_here(&mut self) -> Action {
        self.ask_pattern("Search this directory: ", Subject::Here)
    }

    /// What a search found, on its way into the picker.
    ///
    /// **Nothing to re-check, and that is a fact about searching rather than a
    /// gap in ADR-0009.** An answer is dropped when the question has stopped
    /// meaning what it meant -- a hover is about a point, a references list
    /// about a buffer -- and this one is about a pattern and a directory, both
    /// of which were fixed when the pattern was typed. Neither moves while
    /// `rg` runs.
    ///
    /// What *can* have changed is where the answer would be drawn. A widget
    /// that has opened since owns the Bottom (ADR-0004), so a picker opened
    /// over it would take the keyboard from something the user is in the
    /// middle of -- and the sentence explaining why could not be shown either,
    /// which is #130's finding. Dropped, as `found` drops an answer about
    /// another buffer, and for the same reason: there is nowhere to say it.
    pub fn searched(&mut self, answer: Result<Vec<Place>, String>) -> Action {
        if self.narrow.widget.is_some() {
            return Action::Nothing;
        }
        match answer {
            // The host's sentence, said rather than swallowed: ADR-0010 puts it
            // there because that is the layer that knows where it looked.
            Err(why) => self.refuse_aloud(why),
            Ok(places) => self.open_results(places),
        }
    }

    /// Hand the host a search to run.
    pub(crate) fn ask_search(&mut self, pattern: Pattern, subject: Subject) -> Action {
        self.pending_search = Some(Search { pattern, subject });
        Action::Search
    }

    /// `+vertico/search-symbol-at-point`: this buffer, for the symbol at the
    /// point.
    ///
    /// No filesystem and no Project. The buffer is already here, and searching
    /// the file on disk would search text this buffer has moved past -- which
    /// is what makes this row a different question from `SPC s d` rather than
    /// a narrower one.
    fn search_buffer(&mut self) -> Action {
        let Some(symbol) = self.symbol_at_point() else {
            return self.refuse_aloud("No symbol at the point.");
        };
        let places = self.places_matching(self.current_buffer_id(), &symbol);
        self.open_results(places)
    }

    /// Every buffer that is really there, in the order they were opened.
    ///
    /// A killed slot is a tombstone and a preview is a buffer nobody chose, so
    /// neither is one of "the open buffers" any row means.
    fn real_buffers(&self) -> impl Iterator<Item = BufferId> + '_ {
        (0..self.buffers.len())
            .map(BufferId)
            .filter(|id| self.buffer(*id).is_real())
    }

    /// Where an offset in a buffer is, as a Place carrying its line.
    ///
    /// The tail both rows that build Places share. The **line as it stands**,
    /// minus only its ending: what to strip for *display* is `open_results`'s,
    /// and a second opinion here is how the two come to disagree -- which a
    /// review caught, one of these trimming a line ending and the other
    /// trimming both ends.
    fn place_at(&self, path: &str, doc: &Document, at: usize) -> Option<Place> {
        let (line, column) = doc.position_at(at)?;
        let text = doc.line(line)?.to_string();
        Some(Place::new(path, line, column).showing(text.trim_end_matches(['\n', '\r'])))
    }

    /// Every occurrence of `needle` in one buffer, as Places.
    ///
    /// Empty for a buffer visiting no file: a Place carries a path because the
    /// whole reason it exists is that it may be somewhere else, and one naming
    /// a buffer with no file is somewhere nothing can reopen. The same answer
    /// [`Session::here`] gives, for the same reason.
    fn places_matching(&self, id: BufferId, needle: &str) -> Vec<Place> {
        let buffer = self.buffer(id);
        let (Some(path), Some(doc)) = (buffer.path(), buffer.document()) else {
            return Vec::new();
        };
        // The matched line comes with it, which is what makes a row readable
        // without previewing -- the half grep gives for free (ADR-0011), and
        // in-memory search has for freer still.
        meowr_core::occurrences(doc, needle)
            .into_iter()
            .filter_map(|at| self.place_at(path, doc, at))
            .collect()
    }

    /// `consult-line-multi` with `all-buffers`: every open buffer, for what you
    /// type.
    ///
    /// **All of them, the current one included**, which is what the extracted
    /// row says in as many words -- Doom describes `SPC s B` as *"Search all
    /// open buffers"* and sets `current-prefix-arg` to `all-buffers` to get it.
    /// Naming it "other buffers" would be a second source for that fact and
    /// the wrong one; a review caught this comment claiming it.
    ///
    /// The buffers, not their files. One of them may have unsaved edits in it,
    /// and searching what is on disk would answer about text that is no longer
    /// there -- which is the whole reason these three rows are their own slice.
    fn search_buffers(&mut self) -> Action {
        self.read_line(narrow::Reading::SearchOpen, "Search open buffers: ", "");
        Action::Redraw
    }

    /// Every open buffer, searched for `needle`.
    ///
    /// In buffer order, which is the order they were opened in. A row carries
    /// the file it is in, so the list reads as a list of places rather than of
    /// lines that could be anywhere.
    pub(crate) fn search_open(&mut self, needle: &str) -> Action {
        let places: Vec<Place> = self
            .real_buffers()
            .flat_map(|id| self.places_matching(id, needle))
            .collect();
        self.open_results(places)
    }

    /// `+fold/toggle`: close what is open here, open what is closed.
    ///
    /// The one fold row a hand reaches for, and in this configuration one of
    /// only four that can be reached at all — `+fold/close`, `+fold/open-rec`,
    /// `+fold/next` and `+fold/previous` are bound to evil `z` keys, and the
    /// author runs `(meow +colemak)`.
    ///
    /// Opening is tried first, so pressing it twice on one place gets you back
    /// where you started: after folding, the point is on the head, and a toggle
    /// that folded again from there would close the *next* thing out.
    fn fold_toggle(&mut self) -> Action {
        if !self.unfold_at_point() {
            self.fold_at_point();
        }
        Action::Redraw
    }

    /// `+fold/open`: open the fold at the point, and only open.
    ///
    /// Not a toggle, which is its whole difference from `C-f C-f`: on an open
    /// fold it leaves it open rather than closing it.
    fn fold_open(&mut self) -> Action {
        self.unfold_at_point();
        Action::Redraw
    }

    /// `+fold/close-all`: fold everything this buffer's parse says it can.
    fn fold_close_all(&mut self) -> Action {
        self.fold_all();
        self.scroll_into_view();
        Action::Redraw
    }

    /// `+fold/open-all`: open every fold in this buffer.
    fn fold_open_all(&mut self) -> Action {
        self.unfold_all();
        Action::Redraw
    }

    /// Fold the innermost foldable region around the point.
    ///
    /// `false` when nothing around the point is foldable — a Language with no
    /// rule set, a buffer with no parse, or a point inside nothing that spans
    /// more than a line. The caller says what that means; a toggle reads it to
    /// decide whether there was anything to do.
    ///
    /// **The point moves and nothing else does** (ADR-0012). A point left on a
    /// line nobody can see is a cursor that has gone missing, so it goes to the
    /// head — and only when it was inside the region being hidden, so folding
    /// from the head line leaves the selection exactly as it was rather than
    /// replacing it with a bare point in the same place. The Grab and the
    /// beacons are not consulted at all: `hs-minor-mode` and `treesit-fold`
    /// move point out and touch nothing else.
    pub fn fold_at_point(&mut self) -> bool {
        let Some(region) = self.foldable_at_point() else {
            return false;
        };
        self.current_mut().folds.fold(region);
        self.keep_point_visible();
        self.scroll_into_view();
        true
    }

    /// Move the point out of anything that now hides it.
    ///
    /// **Asked after folding rather than worked out while folding**, which is
    /// one rule instead of two: "did this region contain the point" has to be
    /// re-derived per fold, where "can the point still be seen" is the thing
    /// actually wanted and is true or false whatever was folded. Closing every
    /// fold left the point on a hidden line for exactly that reason — the
    /// question had only been asked on the single-fold path. Found in a real
    /// terminal, where the modeline named a line that was not drawn.
    ///
    /// Onto the nearest visible line above (ADR-0012). That is the head of the
    /// **outermost** fold hiding it, not the innermost: an inner fold's head
    /// can itself be hidden by an outer one, and a point put there would be no
    /// more visible than where it started. `previous_visible` walks until the
    /// line is really drawn, which is the only condition that matters.
    ///
    /// A point on a head has nowhere it needs to go, so folding from one leaves
    /// the selection exactly as it was rather than replacing it with a bare
    /// point in the same place.
    fn keep_point_visible(&mut self) {
        let Some(doc) = self.text_of_current() else {
            return;
        };
        let buffer = self.current();
        let line = match doc.byte_to_line(buffer.selection.current().pos()) {
            Some(line) if buffer.folds.hides(&doc, line) => line,
            _ => return,
        };
        let Some(head) = doc.line_to_byte(buffer.folds.previous_visible(&doc, line)) else {
            return;
        };
        // Without history: nothing was selected and nothing popped, which is
        // what "the selection is untouched" has to mean once the point is one
        // end of it.
        self.current_mut()
            .selection
            .select_without_history(Selection::at(head));
    }

    /// Open the fold the point's line heads, if it heads one.
    pub fn unfold_at_point(&mut self) -> bool {
        let line = self.line_of(self.current().selection.current().pos());
        let Some(doc) = self.text_of_current() else {
            return false;
        };
        self.current_mut().folds.unfold(&doc, line)
    }

    /// Open every fold in this buffer.
    pub fn unfold_all(&mut self) {
        self.current_mut().folds.clear();
    }

    /// Fold every foldable region in this buffer.
    ///
    /// Walked line by line rather than by querying the tree for every node at
    /// once, because "the innermost foldable thing here" is the question the
    /// rule set answers and asking it per line gets the outer ones too --
    /// a `block` inside a `declaration_list` is reached from its own lines.
    pub fn fold_all(&mut self) {
        let Some(doc) = self.text_of_current() else {
            return;
        };
        let regions: Vec<Range<usize>> = (0..doc.line_count())
            .filter_map(|line| doc.line_to_byte(line))
            .filter_map(|at| self.foldable_at(at))
            .collect();
        let buffer = self.current_mut();
        for region in regions {
            buffer.folds.fold(region);
        }
        self.keep_point_visible();
    }

    /// This buffer's text, taken so the session can be borrowed afterwards.
    ///
    /// Three callers wanted the same `let … else` and wrote it three times,
    /// which is one more copy than `line_of` below was extracted for.
    fn text_of_current(&self) -> Option<Document> {
        self.current().document().cloned()
    }

    /// Open whatever hides this line in the current buffer.
    fn reveal(&mut self, line: usize) {
        let Some(doc) = self.text_of_current() else {
            return;
        };
        self.current_mut().folds.reveal(&doc, line);
    }

    /// The line an offset is on, or the first.
    fn line_of(&self, at: usize) -> usize {
        self.current()
            .document()
            .and_then(|doc| doc.byte_to_line(at))
            .unwrap_or(0)
    }

    /// The innermost foldable region around the point.
    fn foldable_at_point(&self) -> Option<Range<usize>> {
        self.foldable_at(self.current().selection.current().pos())
    }

    /// The innermost foldable region around an offset.
    fn foldable_at(&self, at: usize) -> Option<Range<usize>> {
        let buffer = self.current();
        let doc = buffer.document()?;
        meowr_syntax::foldable_at(
            buffer.syntax.as_ref()?,
            &self.languages,
            |byte| doc.byte_to_line(byte),
            at,
        )
    }

    /// `consult-imenu-multi`: what every open buffer's parse says is defined.
    ///
    /// **imenu, not a language server**, which is why this row is here rather
    /// than beside `SPC k D`: the tree meowr already parsed for colour knows
    /// what a function and a class are, so nothing needs asking.
    fn search_definitions(&mut self) -> Action {
        let places: Vec<Place> = self
            .real_buffers()
            .flat_map(|id| self.definitions_in(id))
            .collect();
        self.open_results(places)
    }

    /// One buffer's definitions, as Places naming the line each begins on.
    ///
    /// The **first line** of the definition is what a row shows, which is its
    /// signature -- `fn search_open(&mut self, needle: &str) -> Action {`. Not
    /// a bare name, and that is a choice rather than a shortfall: meowr
    /// vendors no `tags.scm`, the signature is what tells two `new`s apart,
    /// and the picker filters on the row text either way.
    fn definitions_in(&self, id: BufferId) -> Vec<Place> {
        let buffer = self.buffer(id);
        let (Some(path), Some(doc), Some(syntax)) =
            (buffer.path(), buffer.document(), buffer.syntax.as_ref())
        else {
            return Vec::new();
        };
        meowr_syntax::definitions_in(syntax, doc.text().slice(..), &self.languages)
            .into_iter()
            .filter_map(|range| self.place_at(path, doc, range.start))
            .collect()
    }

    /// `+default/search-project`: search the Project this buffer is in.
    fn search_project(&mut self) -> Action {
        self.ask_pattern("Search the project: ", Subject::Project)
    }

    /// `+default/search-other-cwd`: search a directory the user names.
    ///
    /// The one row that asks twice. The directory travels inside the second
    /// read rather than being remembered beside it, so there is no pair to
    /// disagree about which directory the pattern is for.
    fn search_elsewhere(&mut self) -> Action {
        self.read_line(
            narrow::Reading::SearchDirectory,
            "Search which directory: ",
            "",
        );
        Action::Redraw
    }

    /// `+default/search-project-for-symbol-at-point`: no prompt, because where
    /// the point is has already answered the question.
    ///
    /// The symbol is searched as **text**. meowr's `symbol` Thing is Emacs's
    /// syntax classes `w` and `_` together and the `_` class is exactly
    /// `&*+-/<=>_|`, so the symbol under the point really can be `*` -- which
    /// as a regex is a parse error rather than a search, and the row would
    /// report itself unable to run for a symbol that is perfectly searchable.
    fn search_symbol(&mut self) -> Action {
        let Some(path) = self.current().path().map(str::to_owned) else {
            return self.refuse_aloud("No file, so nowhere to search.");
        };
        let Some(symbol) = self.symbol_at_point() else {
            // Not an empty pattern, which `rg` answers with every line of the
            // Project: a question with no subject is not a search.
            return self.refuse_aloud("No symbol at the point.");
        };
        self.ask_search(Pattern::Literal(symbol), Subject::Project(path))
    }

    /// The symbol the point is in, as text.
    fn symbol_at_point(&self) -> Option<String> {
        self.symbol_at(self.current().selection.current().pos())
    }

    /// The symbol at `pos` in the current buffer, as text.
    fn symbol_at(&self, pos: usize) -> Option<String> {
        let doc = self.current().document()?;
        let range = self.things.inner(doc, pos, "symbol")?;
        let text = doc.text();
        let symbol = text
            .slice(text.byte_to_char(range.start)..text.byte_to_char(range.end))
            .to_string();
        (!symbol.is_empty()).then_some(symbol)
    }

    /// Ask for a pattern, and remember what the answer will search.
    ///
    /// A buffer visiting no file is refused before the question rather than
    /// after it: there is no directory to search and no Project to be in, and
    /// finding that out is not worth a pattern typed first.
    fn ask_pattern(&mut self, prompt: &str, subject: fn(String) -> Subject) -> Action {
        let Some(path) = self.current().path().map(str::to_owned) else {
            return self.refuse_aloud("No file, so nowhere to search.");
        };
        self.read_line(narrow::Reading::Search(subject(path)), prompt, "");
        Action::Redraw
    }

    /// `consult-lsp-symbols`: a symbol anywhere the server knows about.
    ///
    /// A picker over a **question** rather than a list, because the server is
    /// the matcher and it truncates -- which is what #137 found and handed
    /// these two rows back for.
    fn lookup_symbols(&mut self) -> Action {
        self.open_asking(false)
    }

    /// The same with `all-workspaces`: every server rather than this buffer's.
    fn lookup_symbols_everywhere(&mut self) -> Action {
        self.open_asking(true)
    }

    /// `+lookup/documentation`: ask what is at the point.
    ///
    /// The session decides *where*, the host asks the server — which is the
    /// division `Action::Save` and `Action::Open` already draw. A buffer
    /// visiting no file has nothing to ask about: a server is told about
    /// documents by URI, and there is no URI for text that is only in memory.
    fn lookup_documentation(&mut self) -> Action {
        let buffer = self.current();
        let Some(path) = buffer.path().map(str::to_owned) else {
            return self.refuse_aloud("No file to look anything up in.");
        };
        let at = buffer.selection.current().pos();
        self.pending_hover = Some((path, at));
        Action::Hover
    }

    /// Where documentation should be asked about, because the point is resting.
    ///
    /// `lsp-ui-doc-show-with-cursor`: the answer arrives without a key being
    /// pressed for it. Read rather than drained, because the host arms a timer
    /// on it -- `hints_pending` and `autosave_pending` have the same shape and
    /// exist for the same reason.
    pub fn resting_hover(&self) -> Option<Resting> {
        let asked = self.resting_at()?;
        // Asked already, and answered with nothing to show. There is no hover
        // on screen to stand for "this one has been done", so the question
        // itself is remembered.
        (self.asked_at_rest.as_ref() != Some(&asked)).then_some(asked)
    }

    /// The question the point is standing on, latch aside: every condition
    /// under which documentation may appear unasked, and where it would be
    /// about.
    ///
    /// One place rather than two, because the answer coming back has to ask
    /// exactly the same question again -- "is this still a resting point, and
    /// still *this* one". A second copy of the conditions is how a message
    /// that stopped the question being asked comes to fail to stop the answer
    /// being shown.
    fn resting_at(&self) -> Option<Resting> {
        if !self.options.hover_on_rest {
            return None;
        }
        // The two States in which the point moves and the text does not.
        // Insert State would ask a question per character; Keypad State has a
        // key sequence in flight and the menu about to take the bottom of the
        // frame; Beacon State has a point per line and no single one to ask
        // about.
        if !matches!(self.state, State::Normal | State::Motion) {
            return None;
        }
        // Exactly one thing owns the bottom of the frame. A message nobody has
        // read yet keeps it: `Bottom` ranks a hover above one because a hover
        // answers a key just pressed, and an idle hover answers no key at all.
        // A widget keeps it for the reason `hover_view` already gives -- it is
        // a modal read taking every key.
        if self.echo.is_some() || self.narrow.widget.is_some() || self.menu_visible() {
            return None;
        }
        let id = self.current_buffer_id();
        let served = self.served().into_iter().find(|served| served.id == id)?;
        Some(Resting {
            path: served.path.to_string(),
            at: self.selection().pos(),
            revision: served.revision,
        })
    }

    /// The same question, taken: the host is about to put it to a server.
    ///
    /// Split from [`resting_hover`](Self::resting_hover) the way `preview` is
    /// split from `preview_target` -- one arms the timer and the other is what
    /// the timer firing means.
    pub fn ask_at_rest(&mut self) -> Option<Resting> {
        let asked = self.resting_hover()?;
        self.asked_at_rest = Some(asked.clone());
        Some(asked)
    }

    /// What a server said about a point that was resting.
    ///
    /// The answer is dropped unless the point is still where the question was
    /// about. #110 settled that a stale answer is dropped by revision, and a
    /// hover carries one -- but a revision is not enough on its own here: the
    /// point moves without the buffer changing, so the *position* is the half
    /// of the check that does the work. `$/cancelRequest` would be the other
    /// way to spend this, and #121 left it unspent: dropping an answer costs a
    /// comparison, and cancelling costs a connection-level capability.
    ///
    /// Nothing to say is said by saying nothing, which is where this parts
    /// company with [`show_hover`](Self::show_hover): that one answers a key,
    /// and silence after a key reads as a key that did not work. Nobody
    /// pressed anything here, so a message would be the editor interrupting.
    pub fn show_hover_at(&mut self, asked: &Resting, text: Option<&str>) -> Action {
        let Some(text) = text else {
            return Action::Nothing;
        };
        if self.resting_at().as_ref() != Some(asked) {
            return Action::Nothing;
        }
        self.hover = Some(self.hovering(text));
        Action::Redraw
    }

    /// Put a message in the Echo Area, from the host.
    ///
    /// The host has things to say the session cannot know it should — a server
    /// that would not start, a hover that came back empty. `refuse_aloud` is
    /// the session's own way of saying one and is private for that reason;
    /// this is the same row of the frame reached from outside, and it does not
    /// count a signal, because nothing was refused.
    pub fn say(&mut self, message: &str) {
        self.echo = Some(message.to_string());
    }

    /// What a server said about the point a key asked about.
    ///
    /// `path` is the file the question was about, and the answer is dropped
    /// unless that file is still the one on screen -- documentation for a
    /// buffer the user has left is about text that is not there. #123's guard
    /// does not reach this on its own: it records which buffer a hover is
    /// about when the hover is *shown*, and for a key-asked one that is when
    /// the answer comes back, so a stale answer would be stamped with the new
    /// buffer and pass.
    ///
    /// The point and the revision are deliberately **not** checked, which is
    /// where this parts company with [`show_hover_at`](Self::show_hover_at).
    /// That one drops an answer whose point has moved because nobody asked for
    /// it, so one about somewhere the point has left is an interruption about
    /// nothing. Three keys were pressed for this one: a point that has drifted
    /// a word, or text edited underneath, does not make the answer unwanted,
    /// and Emacs does not cancel the request either.
    ///
    /// Nothing to say is said out loud -- silence after a key reads as a key
    /// that did not work -- but not about a file the user has left, where the
    /// message is noise for the same reason the answer is. Both live here
    /// rather than in the host's receiving arm, because which of the three it
    /// is, is one decision.
    pub fn show_hover(&mut self, path: &str, text: Option<&str>) -> Action {
        if self.current().path() != Some(path) {
            return Action::Nothing;
        }
        match text {
            Some(text) => self.hover = Some(self.hovering(text)),
            None => self.echo = Some("No documentation for this.".to_string()),
        }
        Action::Redraw
    }

    /// A hover about the buffer that is current now.
    ///
    /// Both ways of showing one go through this, so neither can forget which
    /// buffer it was about -- which is the whole of #123.
    fn hovering(&self, text: &str) -> Hover {
        Hover {
            buffer: self.current_buffer_id(),
            text: text.to_string(),
        }
    }

    /// The hover to draw, wrapped to the frame and capped to fit in it.
    ///
    /// `None` when one of the other two things that live at the bottom of the
    /// frame is open. Decided here rather than by the caller because "exactly
    /// one of the three" is a fact about the session, and a layer that drew
    /// two of them would be laying windows out against a height nothing
    /// reserved.
    pub fn hover_view(&self) -> Option<meowr_view::HoverView> {
        let hover = self.hover.as_ref()?;
        // #123: a hover describes a symbol in a buffer, and it must not outlive
        // the buffer being the one on screen. `on_key` clearing it covers every
        // way the user changes buffer *by hand* and looks like enough -- but
        // choosing a file does not open it. The host reads it on a task and
        // `opened` makes the new buffer current whenever the read lands, with
        // no key pressed at that moment.
        //
        // Checked here rather than cleared in `opened`, for the reason the Echo
        // Area check below is here: this covers every way the current buffer can
        // change, including the ones nobody has written yet.
        if hover.buffer != self.current_buffer_id() {
            return None;
        }
        let text = hover.text.as_str();
        if self.narrow.widget.is_some() || self.menu_visible() {
            return None;
        }
        // #122: the third thing that lives at the bottom of the frame, and the
        // one this did not check. A message here arrived *after* the hover:
        // every other writer of `echo` is inside command dispatch, and `on_key`
        // cleared the hover two lines before any of them ran, so the only way
        // both are set at once is `say` -- the host's door, for the things only
        // it knows. A server that would not start says so once, by design, so a
        // message lost behind a hover is lost for the session.
        //
        // Decided here rather than by re-ranking `Bottom`, because the ranking
        // is right for the case it was written for: a hover outranks the
        // message the key *before* it left, which is `SPC k k`'s own case.
        if self.echo.is_some() {
            return None;
        }
        // One row for the ordinary Echo Area's own line is not reserved — a
        // hover *is* that line, grown — but the frame's last row still has to
        // be left for the modeline of the window above, which is why the cap
        // is the height less one rather than the height.
        let cap = meowr_view::HOVER_MAX_ROWS.min(usize::from(self.area.height).saturating_sub(1));
        let width = meowr_view::HOVER_MAX_COLS.min(usize::from(self.area.width));
        let view = meowr_view::hover(text, width, cap);
        (!view.lines.is_empty()).then_some(view)
    }

    pub fn on_key(&mut self, key: Key) -> Action {
        // Emacs clears the echo area on the next command; so does this. A
        // hover goes with it: it is the Echo Area grown to fit, and `eldoc`
        // and `lsp-ui-doc` are both transient in exactly this way.
        self.echo = None;
        self.hover = None;
        // The latch is against an *idle* editor asking one question over and
        // over; a key is a user doing something, and `lsp-ui-doc` comes back
        // after one. The debounce keeps that to one question per
        // `hover-delay` however hard the key is leaned on.
        self.asked_at_rest = None;
        // While the widget is open it takes every key. It is a modal read, the
        // same way the minibuffer is in Emacs, and routing keys through the
        // ordinary keymap first would make every binding a possible collision
        // with typing a filename.
        if self.narrow.widget.is_some() {
            return self.narrow_key(key);
        }
        if self.state == State::Keypad {
            return self.keypad_key(key);
        }
        // `meow-digit-argument`'s transient map, which binds the ten digits
        // and nothing else. Ahead of the keymap because Normal State's digits
        // are `meow-expand`, and the whole reason `SPC 3` exists is that they
        // are.
        if std::mem::take(&mut self.counting)
            && let Key::Char(c) = key
            && let Some(digit) = c.to_digit(10)
        {
            self.argument = self.argument.digit(digit as i32);
            self.counting = true;
            return Action::Redraw;
        }
        // Everything typed while recording is part of the macro, including the
        // key that ends it — which is how `meow-beacon-insert-exit` works.
        if let Some(recording) = self.beacon.recording.as_mut() {
            recording.keys.push(key);
            if key == Key::Esc && self.state == State::Insert {
                let action = self.dispatch("leave-insert", Some(key));
                self.beacon_apply();
                return action;
            }
        }
        // A command asked for a character. It is an argument, not a binding,
        // so the keymap does not get a say — and that has to hold for the keys
        // that are *not* characters too. Taking the read and then falling
        // through on `ESC` abandoned it and ran the binding as well, which
        // once ESC could close buffers meant `f` then `ESC` closed the one you
        // were reading. Emacs aborts the read there and stops.
        if let Some(awaiting) = self.awaiting_char.take() {
            let Key::Char(ch) = key else {
                return self.refuse_aloud("Quit.");
            };
            let (forward, till) = match awaiting {
                Awaiting::Find { forward } => (forward, false),
                Awaiting::Till { forward } => (forward, true),
            };
            return self.search_for(ch, forward, till);
        }
        self.pending.push(key);
        // Beacon State's own bindings first, then Normal's — which is what
        // "the Beacon keymap inherits Normal" means, and the order matters:
        // `s` is `beacon-start` there and `insert` here.
        let resolution = match self.keymap.resolve(self.state, &self.pending) {
            Resolution::Unbound if self.state == State::Beacon => {
                self.keymap.resolve(State::Normal, &self.pending)
            }
            other => other,
        };
        match resolution {
            Resolution::Pending => Action::Nothing,
            Resolution::Command(name) => {
                let name = name.to_owned();
                self.pending.clear();
                self.dispatch(&name, Some(key))
            }
            Resolution::Unbound => {
                self.pending.clear();
                if self.state == State::Insert && matches!(key, Key::Char(_)) {
                    self.dispatch(SELF_INSERT, Some(key))
                } else {
                    Action::Nothing
                }
            }
        }
    }

    /// The State a command's validity is judged in.
    ///
    /// Beacon counts as Normal, because Meow's Beacon keymap *inherits* the
    /// Normal one and remaps a handful of commands to no-ops. Without this,
    /// almost nothing is valid in Beacon State — including the motions, which
    /// is how you leave it.
    /// Dispatch a composition's steps, each behind the guards.
    ///
    /// **A step sees no key.** #78 settled that, and the reason survives the
    /// move: a Thing step reading `ctx.key` took whichever key the composition
    /// was bound under, so `(bounds-of-thing kill)` meant "round" under
    /// `SPC k r` and beeped under `SPC k q`. `None` is what a keyless dispatch
    /// already means, and #88 made every command answer it properly.
    ///
    /// **`last_command` is the composition, not its last step.** Saved and
    /// restored around the run, the way `dispatch_hook_commands` does — `'`
    /// repeats the thing you invoked, which is the composition.
    ///
    /// **A refusal stops the run**, and carries its sentence out: the step
    /// that could not happen is the one worth naming, not the composition
    /// around it. A signal is how a refusal is recognised from here, since
    /// every one of the guards raises one and `Action` does not distinguish.
    ///
    /// **Hooks fire per step, and not for the composition around them.** A
    /// step is an ordinary dispatch now, so it reaches the hook machinery that
    /// the registry's own runner never did — the same answer a keyboard macro
    /// gives, and the alternative is a composition whose steps skip
    /// `before-save` and quietly stop being saves. The wrapper itself has no
    /// behaviour of its own to hook: it *is* its steps, and firing for both
    /// would run every hook N+1 times for N steps.
    fn run_composed(&mut self, name: &str, steps: &[String]) -> Action {
        let outer = self.last_command.clone();
        let mut action = Action::Nothing;
        // A `Save` is a *request to the host*, not a step's return value, and
        // the step after it would otherwise overwrite the request with its own
        // `Redraw` — the file never written and nothing said. `Open` is the
        // same shape and is kept for the same reason.
        let mut asked = None;
        for step in steps {
            // #59's resolve pass catches an unknown name when the declaration
            // set is applied. If one ever gets past it, refusing is better
            // than skipping it in silence — which is what a bare dispatch
            // would do, since an unregistered name is `Action::Nothing`.
            if self.registry.get(step).is_none() {
                self.last_command = outer;
                return self
                    .refuse_aloud(format!("`{name}` names `{step}`, which does not exist."));
            }
            let signals = self.signals;
            action = self.dispatch_command(step, None);
            if matches!(action, Action::Save | Action::Open) {
                asked = Some(action);
            }
            // A refusal stops the run. Every guard raises a signal and
            // `Action` does not distinguish one from an ordinary redraw, so
            // the count is what says a step declined.
            if self.signals != signals {
                break;
            }
        }
        self.last_command = outer;
        self.remember(name, Argument::None);
        asked.unwrap_or(action)
    }

    /// Whether `command` may run where the session is.
    ///
    /// Valid in the actual State, or — in Beacon — in Normal, since the Beacon
    /// keymap inherits it. Checking only the inherited State would reject
    /// `beacon-start`, which is Beacon's alone.
    ///
    /// Asked in two places that must agree: `dispatch_command` answers a key
    /// with silence, and `M-x` answers a chosen name with a sentence. Two
    /// answers to one question is the point; two copies of the question is how
    /// the inheritance rule gets changed in one of them.
    fn available_here(&self, command: &Command) -> bool {
        command.valid_in(self.state) || command.valid_in(self.keymap_state())
    }

    fn keymap_state(&self) -> State {
        match self.state {
            State::Beacon => State::Normal,
            other => other,
        }
    }

    /// The frame changed size, so the windows are redivided.
    ///
    /// Width matters now: with a horizontal split, a narrower frame means
    /// narrower columns, not clipped ones.
    pub fn on_resize(&mut self, width: u16, height: u16) -> Action {
        self.area = Area {
            x: 0,
            y: 0,
            width,
            height,
        };
        self.windows.layout(self.area);
        Action::Redraw
    }
}

/// A buffer with a language server behind it, and what the host needs to reach
/// it.
///
/// Borrowed rather than cloned: every field is already held by the session, and
/// the host reads them to decide what to start and what to send.
#[derive(Debug, Clone, Copy)]
pub struct Served<'a> {
    pub id: BufferId,
    pub path: &'a str,
    /// meowr's own Language name, which is also LSP's `languageId`: they are the
    /// same string for every language either side has a name for, so nothing
    /// maps between them. A genuine disagreement would be a row to add, not a
    /// reason to keep two names for one thing now.
    pub language: &'a str,
    pub server: &'a meowr_command::LanguageServer,
    /// Which edit of the text this describes, for tagging what is sent.
    pub revision: u64,
}

/// What the Echo Area is saying about the point, and which buffer it is about.
///
/// The buffer is not bookkeeping: a hover describes a symbol in a buffer, and
/// `opened` makes another one current without a key ever being pressed, so the
/// text alone cannot say whether it is still about anything on screen.
#[derive(Debug)]
struct Hover {
    buffer: BufferId,
    text: String,
}

/// Where an idle hover was asked about.
///
/// A position rather than a revision, which is the whole difficulty #121
/// names: the point moves without the buffer changing, so an answer cannot be
/// checked against a revision alone the way `apply_reparse` checks a parse.
/// It carries both, and the answer is dropped unless both still hold.
///
/// Owned rather than borrowed, unlike [`Served`]: it outlives the borrow by
/// design -- the host holds one across an `await` on a language server.
///
/// `Resting` rather than `RestingPoint` because the glossary is explicit that
/// "point" is deliberately not a type here -- it is a Selection's `pos`, and a
/// `Point` in this codebase would be tree-sitter's `(row, column)`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resting {
    pub path: String,
    /// The byte offset in the buffer that the question is about.
    pub at: usize,
    /// Which edit of the text it was asked against.
    pub revision: u64,
}

/// The servers meowr ships knowing about.
///
/// Rust only, which is #117's hardcoding turned into a default rather than
/// removed: an editor with no config behaves exactly as it did, and the
/// difference is that a config can now add to this or replace what is in it.
/// A default rather than an empty table because the alternative is an editor
/// that silently does less than the previous version for everyone who has not
/// written a config yet.
fn default_servers() -> std::collections::HashMap<String, meowr_command::LanguageServer> {
    std::collections::HashMap::from([(
        "rust".to_string(),
        meowr_command::LanguageServer {
            program: "rust-analyzer".to_string(),
            args: Vec::new(),
        },
    )])
}

/// The formatters meowr ships knowing about: apheleia's table, for the
/// Languages meowr links (ADR-0014).
///
/// Read from `apheleia-formatters.el` rather than recalled: the argv is
/// apheleia's, `{file}` standing for its `filepath`, and `prettier` named
/// directly where apheleia goes through its `apheleia-npx` wrapper. Markdown is
/// absent because apheleia leaves it out on purpose, and Scheme because
/// apheleia has nothing for it. Python is `black`, which is what Doom runs.
fn default_formatters() -> std::collections::HashMap<String, meowr_command::Formatter> {
    let formatter = |program: &str, args: &[&str]| meowr_command::Formatter {
        program: program.to_string(),
        args: args.iter().map(|arg| arg.to_string()).collect(),
    };
    std::collections::HashMap::from([
        ("rust".to_string(), formatter("rustfmt", &["--quiet", "--emit", "stdout"])),
        ("python".to_string(), formatter("black", &["--stdin-filename", "{file}", "-"])),
        ("bash".to_string(), formatter("shfmt", &["-filename", "{file}", "-"])),
        ("nix".to_string(), formatter("nixfmt", &[])),
        ("toml".to_string(), formatter("taplo", &["format", "--colors", "never", "-"])),
        (
            "json".to_string(),
            formatter("prettier", &["--stdin-filepath", "{file}", "--parser=json"]),
        ),
        (
            "yaml".to_string(),
            formatter("prettier", &["--stdin-filepath", "{file}", "--parser=yaml"]),
        ),
    ])
}

pub fn default_registry() -> Registry {
    let mut reg = Registry::new();

    reg.register(
        Command::new(
            SELF_INSERT,
            Handler::Native(|ctx| {
                // A composition and a hook both arrive with no key (#78), and
                // this returned `Handled` having done nothing — the silence
                // #80 swept the keymap for and could not reach, because it
                // takes a config to get here. Before #78 it was worse: the
                // step inserted whichever key the composition was bound under.
                let Some(Key::Char(c)) = ctx.key else {
                    return Outcome::Refused("Nothing to insert.");
                };
                let mut buf = [0u8; 4];
                let text = c.encode_utf8(&mut buf);
                let pos = ctx.selection.current().pos();
                // `type_text`, not `insert`: a run of typing amalgamates into
                // one transaction, so undo takes back the word rather than the
                // letter.
                let _ = ctx.history.type_text(ctx.doc, ctx.selection, pos, text);
                Outcome::Handled
            }),
        )
        .described("Insert the typed character")
        .mutating()
        .in_states(&[State::Insert]),
    );

    reg.register(
        Command::new(
            "newline",
            Handler::Native(|ctx| {
                let pos = ctx.selection.current().pos();
                let _ = ctx.history.type_text(ctx.doc, ctx.selection, pos, "\n");
                Outcome::Handled
            }),
        )
        .described("Insert a newline")
        .mutating()
        .in_states(&[State::Insert]),
    );

    reg.register(
        Command::new(
            "delete-backward",
            Handler::Native(|ctx| {
                let pos = ctx.selection.current().pos();
                if let Some(from) = core::previous_char_boundary(ctx.doc, pos) {
                    let _ = ctx.history.delete(ctx.doc, ctx.selection, from..pos);
                }
                Outcome::Handled
            }),
        )
        .described("Delete the character before the point")
        .mutating()
        .in_states(&[State::Insert]),
    );

    reg.register(
        Command::new(
            "enter-insert",
            Handler::Native(|ctx| {
                *ctx.state = State::Insert;
                Outcome::Handled
            }),
        )
        .described("Enter Insert State")
        .in_states(&[State::Normal]),
    );

    reg.register(
        Command::new(
            "leave-insert",
            Handler::Native(|ctx| {
                *ctx.state = State::Normal;
                Outcome::Handled
            }),
        )
        .described("Leave Insert State")
        .in_states(&[State::Insert]),
    );

    // Normal State motions. A `Handler` is a plain fn pointer, so each of
    // these is a non-capturing closure over the corresponding core function
    // rather than a captured one.
    moving!(reg, "left", "Move left", "Beginning of buffer.", |d, s| {
        core::left(d, s, Axis::Select)
    });
    moving!(
        reg,
        "left-expand",
        "Expand left",
        "Beginning of buffer.",
        |d, s| core::left(d, s, Axis::Expand)
    );
    moving!(reg, "right", "Move right", "End of buffer.", |d, s| {
        core::right(d, s, Axis::Select)
    });
    moving!(
        reg,
        "right-expand",
        "Expand right",
        "End of buffer.",
        |d, s| core::right(d, s, Axis::Expand)
    );
    // The line motions take a count where `h` and `i` take nothing: Meow's
    // `meow-left` and `meow-right` are `(interactive)` with no argument at
    // all, while `meow-next` and `meow-prev` are `(interactive "P")`. They are
    // also the only motions with a *universal* argument branch, which is why
    // they have a macro of their own.
    line_motion!(reg, "prev", "Move up", Axis::Select, false);
    line_motion!(reg, "prev-expand", "Expand up", Axis::Expand, false);
    line_motion!(reg, "next", "Move down", Axis::Select, true);
    line_motion!(reg, "next-expand", "Expand down", Axis::Expand, true);
    counted!(
        reg,
        "next-word",
        "Select to the end of the next word",
        Argument::count,
        core::next_word
    );
    counted!(
        reg,
        "back-word",
        "Select back over a word",
        Argument::count,
        core::back_word
    );
    counted!(
        reg,
        "next-symbol",
        "Select to the end of the next symbol",
        Argument::count,
        core::next_symbol
    );
    counted!(
        reg,
        "back-symbol",
        "Select back over a symbol",
        Argument::count,
        core::back_symbol
    );
    counted!(
        reg,
        "mark-word",
        "Mark the word under the point",
        Argument::is_negative,
        core::mark_word
    );
    counted!(
        reg,
        "mark-symbol",
        "Mark the symbol under the point",
        Argument::is_negative,
        core::mark_symbol
    );
    // Counted, not a bare motion: `meow-line` is `(interactive "p")` and reads
    // the sign for its direction, so `-l` selects the line backwards.
    counted!(
        reg,
        "line",
        "Select the current line",
        Argument::count,
        core::select_line
    );
    // `repeat`. Ring 1 binds `'` to Emacs's own `repeat`, not to anything of
    // Meow's, so it repeats whatever ran last rather than a Meow motion. The
    // handler is unreachable — `dispatch` answers the name in its own match
    // arm — but the entry has to exist for the keymap to resolve it
    // and for the leader menu to have a description to show.
    reg.register(
        Command::new("repeat", Handler::Native(|_| Outcome::Handled))
            .described("Repeat the last command")
            // `2 '` repeats with a fresh prefix, so it takes one too.
            .taking_count()
            .in_states(&[State::Normal]),
    );

    // `meow-goto-line`. Emacs reads the line number from the minibuffer; the
    // numeric argument is meowr's way of saying the same thing, and with no
    // argument there is nothing to prompt with, so it refuses. Recorded as a
    // divergence rather than papered over — see `fidelity/README.md`.
    reg.register(
        Command::new(
            "goto-line",
            Handler::Native(|ctx| match ctx.argument.value() {
                Some(n) if n > 0 => {
                    core::goto_line(ctx.doc, ctx.selection, n as usize);
                    Outcome::Handled
                }
                _ => Outcome::Refused("No such line."),
            }),
        )
        .described("Go to a line and select it")
        .taking_count()
        .in_states(&[State::Normal]),
    );

    // `meow-block`. Not a bare motion: the scan takes a direction, and it has
    // to know which brackets are delimiters and which are characters inside a
    // string or a comment. The direction is Meow's own
    // `(xor (meow--direction-backward-p) (< arg 0))` — a backward selection
    // and a negative argument each reverse the scan, and together they cancel.
    // The delimiters come from the syntax tree; see [`Syntactic`].
    reg.register(
        Command::new(
            "block",
            Handler::Native(|ctx| {
                let current = ctx.selection.current();
                let backward = current.selection_type().is_some() && current.is_backward();
                let back = backward ^ ctx.argument.value().is_some_and(|n| n < 0);
                let syntactic = Syntactic::of(ctx.text_objects.as_ref());
                core::block(ctx.doc, ctx.selection, back, syntactic.as_port());
                Outcome::Handled
            }),
        )
        .described("Select the block, or expand to the parent block")
        .taking_count()
        .in_states(&[State::Normal]),
    );
    // `meow-to-block`. Not a bare motion: it takes a direction, from the block
    // selection it starts from — reversed, it reaches backward — or from a
    // negative argument. A bare registration discards both, which is why this
    // is spelled out.
    reg.register(
        Command::new(
            "to-block",
            Handler::Native(|ctx| {
                let current = ctx.selection.current();
                let back = (current.kind() == Some(Kind::BLOCK) && current.is_backward())
                    || ctx.argument.value().is_some_and(|n| n < 0);
                let syntactic = Syntactic::of(ctx.text_objects.as_ref());
                core::to_block(ctx.doc, ctx.selection, back, syntactic.as_port());
                Outcome::Handled
            }),
        )
        .described("Expand to the end of the next block")
        .taking_count()
        .in_states(&[State::Normal]),
    );

    // `meow-join`. The name says join and the docstring says select: it marks
    // the indentation, and deleting the mark is what joins. Both ways to
    // `meow--join-both` are reachable: a universal argument, and repeating `j`
    // on a join selection.
    reg.register(
        Command::new(
            "join",
            Handler::Native(|ctx| {
                // `(or (equal '(expand . join) (meow--selection-type))
                //      (meow--with-universal-argument-p arg))`, in that order.
                let side = if ctx.selection.current().kind() == Some(Kind::JOIN)
                    || ctx.argument.is_universal()
                {
                    core::JoinSide::Both
                } else if ctx.argument.value().is_some_and(|n| n < 0) {
                    core::JoinSide::Forward
                } else {
                    core::JoinSide::Backward
                };
                core::join(ctx.doc, ctx.selection, side);
                Outcome::Handled
            }),
        )
        .described("Select the indentation joining this line to the last")
        .taking_count()
        .in_states(&[State::Normal]),
    );

    // `meow-expand`. One command rather than Meow's ten `meow-expand-N`,
    // because the digit is already in `ctx.key` and ten near-identical names
    // would buy nothing a rebinding cannot do by binding a different key here.
    reg.register(
        Command::new(
            "expand",
            Handler::Native(|ctx| {
                // Only the magnitude. Direction is the selection's, exactly
                // as `meow-expand` takes it from `meow--direction-backward-p`
                // rather than from the argument — so `- 3` before an expand
                // grows it by three the way it already points, and does not
                // turn it round.
                let n = ctx
                    .argument
                    .value()
                    .map(|c| c.unsigned_abs())
                    .or_else(|| match ctx.key {
                        Some(Key::Char(c)) => c.to_digit(10),
                        _ => None,
                    })
                    // The one `ctx.key` reader that wants nothing said when
                    // there is no key (#88): a composition or a hook gets the
                    // count of one that `meow-expand` takes when the argument
                    // and the digit are both absent, which is the same answer
                    // pressing the key alone gives.
                    .unwrap_or(1);
                core::expand(ctx.doc, ctx.selection, n, ctx.options.expand_axis);
                Outcome::Handled
            }),
        )
        .described("Expand the selection by a count of whatever made it")
        .taking_count()
        .requiring_selection()
        .in_states(&[State::Normal]),
    );

    // `beginning-of-buffer` and `end-of-buffer`, on Emacs's `M-<` and `M->`.
    // The same function the line motions' universal-argument branch uses —
    // `C-u n` already goes to the end of the buffer — so these are a second
    // door to a behaviour Meow defines rather than a new one. `Axis::Select`
    // because they are plain motions: Emacs's own do not extend a region.
    //
    // Two registrations rather than a loop: a `Handler` is a bare `fn` pointer
    // and cannot capture which end it is, which is the constraint that shaped
    // every motion macro in this file.
    reg.register(
        Command::new(
            "beginning-of-buffer",
            Handler::Native(|ctx| {
                core::to_buffer_edge(ctx.doc, ctx.selection, Axis::Select, false);
                Outcome::Handled
            }),
        )
        .described("Go to the beginning of the buffer")
        .in_states(&[State::Normal]),
    );
    reg.register(
        Command::new(
            "end-of-buffer",
            Handler::Native(|ctx| {
                core::to_buffer_edge(ctx.doc, ctx.selection, Axis::Select, true);
                Outcome::Handled
            }),
        )
        .described("Go to the end of the buffer")
        .in_states(&[State::Normal]),
    );

    // `digit-argument` and `negative-argument`. Ordinary commands writing the
    // argument the *next* command will see, so a Plugin can build an argument
    // the same way rather than the session doing it behind the registry's back.
    reg.register(
        Command::new(
            "digit-argument",
            Handler::Native(|ctx| {
                // The digit *is* the key that ran this, so with no key there
                // is no digit. It returned `Handled` having built nothing —
                // the silence #88 swept for, and one the sweep cannot see,
                // because a command that claims success is indistinguishable
                // from one that had nothing to do. Found by reading.
                let Some(d) = ctx.key.and_then(|key| match key {
                    Key::Char(c) => c.to_digit(10),
                    _ => None,
                }) else {
                    return Outcome::Refused(
                        "`digit-argument` takes its digit from the key that ran it.",
                    );
                };
                *ctx.next_argument = ctx.argument.digit(d as i32);
                Outcome::Handled
            }),
        )
        .described("Begin or extend a numeric argument"),
    );

    // `universal-argument`. One command for the whole `C-u` family, because
    // Emacs's `universal-argument-more` is what turns the second press into
    // `(16)` and this reads the argument it was given to do the same.
    reg.register(
        Command::new(
            "universal-argument",
            Handler::Native(|ctx| {
                *ctx.next_argument = ctx.argument.universal();
                Outcome::Handled
            }),
        )
        .described("Begin a universal argument"),
    );

    reg.register(
        Command::new(
            "negative-argument",
            Handler::Native(|ctx| {
                *ctx.next_argument = ctx.argument.negate();
                Outcome::Handled
            }),
        )
        .described("Make the numeric argument negative"),
    );

    // Emacs's `keyboard-quit`, which signals. It exists here only as the
    // fallback for cancelling with nothing to cancel, which is what Meow's
    // `meow-selection-command-fallback` points `meow-cancel-selection` at.
    reg.register(
        // Emacs's `keyboard-quit` is `(signal 'quit nil)`, and its command
        // loop puts "Quit" in the echo area. meowr had the bell without the
        // word, which the sweep in the session tests found and the hand-written
        // table could not have.
        Command::new(
            "keyboard-quit",
            Handler::Native(|ctx| {
                // Emacs's `keyboard-quit` clears `prefix-arg`. Without that,
                // `SPC 3` followed by a change of mind left a 3 armed for
                // whatever was pressed next — and a count you did not type is
                // worse than one that did nothing.
                *ctx.next_argument = Argument::None;
                Outcome::Refused("Quit.")
            }),
        )
        .described("Abort"),
    );

    // --- editing ----------------------------------------------------------
    //
    // Meow delegates every one of these to an Emacs key. What is reproduced is
    // the result, per ADR-0006, so they are written directly.

    macro_rules! editing {
        ($name:literal, $desc:literal, $f:expr) => {
            reg.register(
                Command::new($name, Handler::Native($f))
                    .described($desc)
                    .mutating()
                    .in_states(&[State::Normal]),
            );
        };
    }

    // Entering Insert State, in its four positions.
    reg.register(
        Command::new(
            "insert",
            Handler::Native(|ctx| {
                core::edit::to_selection_start(ctx.selection);
                *ctx.state = State::Insert;
                Outcome::Handled
            }),
        )
        .described("Insert before the selection")
        .in_states(&[State::Normal]),
    );
    reg.register(
        Command::new(
            "append",
            Handler::Native(|ctx| {
                core::edit::to_selection_end(ctx.selection);
                *ctx.state = State::Insert;
                Outcome::Handled
            }),
        )
        .described("Insert after the selection")
        .in_states(&[State::Normal]),
    );
    editing!("open-below", "Open a line below", |ctx| {
        core::edit::open_below(ctx.doc, ctx.selection, ctx.history);
        *ctx.state = State::Insert;
        Outcome::Handled
    });
    editing!("open-above", "Open a line above", |ctx| {
        core::edit::open_above(ctx.doc, ctx.selection, ctx.history);
        *ctx.state = State::Insert;
        Outcome::Handled
    });

    reg.register(
        Command::new(
            "kill",
            Handler::Native(|ctx| {
                core::edit::kill(ctx.doc, ctx.selection, ctx.kill_ring, ctx.history);
                Outcome::Handled
            }),
        )
        .described("Kill the selection into the ring")
        .mutating()
        .requiring_selection()
        .in_states(&[State::Normal]),
    );

    reg.register(
        Command::new(
            "save",
            Handler::Native(|ctx| {
                core::edit::save(ctx.doc, ctx.selection, ctx.kill_ring);
                Outcome::Handled
            }),
        )
        .described("Copy the selection into the ring")
        .requiring_selection()
        .in_states(&[State::Normal]),
    );

    // `(meow-save . kill-ring-save)` in `meow-selection-command-fallback`.
    // Emacs's `kill-ring-save` does not need an *active* region, only a mark —
    // so after `G`, which deactivates the selection but leaves the mark where
    // it was, `y` copies rather than signalling.
    reg.register(
        Command::new(
            "copy-region",
            Handler::Native(|ctx| {
                if core::edit::copy_region(ctx.doc, ctx.selection, ctx.kill_ring) {
                    Outcome::Handled
                } else {
                    Outcome::Refused("No region to copy.")
                }
            }),
        )
        .described("Copy between the mark and the point")
        .in_states(&[State::Normal]),
    );

    editing!("yank", "Insert the head of the kill ring", |ctx| {
        // Emacs raises `user-error "Kill ring is empty"` here — a bell *and*
        // a sentence. meowr had only the bell until `Outcome::Refused`, and
        // the words are Emacs's own.
        if core::edit::yank(ctx.doc, ctx.selection, ctx.kill_ring, ctx.history) {
            Outcome::Handled
        } else {
            Outcome::Refused("Kill ring is empty.")
        }
    });

    editing!(
        "yank-pop",
        "Replace the last yank with the next kill",
        |ctx| {
            // `yank-pop` signals unless the previous command was a yank. Checking
            // `last_command` rather than trusting the buffer is what makes that
            // faithful rather than approximately right.
            let after_yank = matches!(ctx.last_command, Some("yank") | Some("yank-pop"));
            if after_yank
                && core::edit::yank_pop(ctx.doc, ctx.selection, ctx.kill_ring, ctx.history)
            {
                Outcome::Handled
            } else {
                Outcome::Refused("Previous command was not a yank.")
            }
        }
    );

    reg.register(
        Command::new(
            "replace",
            Handler::Native(|ctx| {
                if core::edit::replace(ctx.doc, ctx.selection, ctx.kill_ring, ctx.history) {
                    Outcome::Handled
                } else {
                    // The same sentence `yank` gives, because it is the same
                    // empty ring — `replace` is a yank over the selection.
                    Outcome::Refused("Kill ring is empty.")
                }
            }),
        )
        .described("Replace the selection with the head of the ring")
        .mutating()
        .requiring_selection()
        .in_states(&[State::Normal]),
    );

    reg.register(
        Command::new(
            "change",
            Handler::Native(|ctx| {
                core::edit::change(ctx.doc, ctx.selection, ctx.history);
                *ctx.state = State::Insert;
                Outcome::Handled
            }),
        )
        .described("Kill the selection and insert")
        .mutating()
        .requiring_selection()
        .in_states(&[State::Normal]),
    );

    // "End of buffer." is what `backward-delete` says in the mirror direction
    // just below, and what Emacs puts in the echo area for the `end-of-buffer`
    // signal these three raise. They beeped instead until #88's sweep asked
    // them at the end of a buffer — #80 pressed their keys in the middle of
    // one, where there was always a character to take.
    editing!("change-char", "Delete the character and insert", |ctx| {
        if core::edit::delete_char(ctx.doc, ctx.selection, ctx.history) {
            *ctx.state = State::Insert;
            Outcome::Handled
        } else {
            Outcome::Refused("End of buffer.")
        }
    });

    editing!("delete", "Delete the character after the point", |ctx| {
        if core::edit::delete_char(ctx.doc, ctx.selection, ctx.history) {
            Outcome::Handled
        } else {
            Outcome::Refused("End of buffer.")
        }
    });

    editing!(
        "backward-delete",
        "Delete the character before the point",
        |ctx| {
            if core::edit::backward_delete(ctx.doc, ctx.selection, ctx.history) {
                Outcome::Handled
            } else {
                Outcome::Refused("Beginning of buffer.")
            }
        }
    );

    // `meow-C-k`: Emacs's `kill-line`, which is what `k` falls back to with no
    // selection. To the end of the line, or the newline itself when already
    // there.
    editing!("kill-line", "Kill to the end of the line", |ctx| {
        let pos = ctx.selection.current().pos();
        let end = core::line_end_after(ctx.doc, pos);
        let range = if end > pos {
            pos..end
        } else {
            match core::next_char_boundary(ctx.doc, pos) {
                Some(next) => pos..next,
                None => return Outcome::Refused("End of buffer."),
            }
        };
        core::edit::kill_range(ctx.doc, ctx.selection, ctx.kill_ring, ctx.history, range);
        Outcome::Handled
    });

    reg.register(
        Command::new("write-file", Handler::Native(|_| Outcome::Save))
            // `C-x C-s`, its real home, since Keypad landed and took it — plus
            // `SPC f s`, which is Doom's. Bare `C-s` was the stand-in before
            // that and is bound to nothing now. Nothing in the Colemak Normal
            // State table writes a file: in Emacs that is a `C-x` chord.
            .described("Write the buffer to disk"),
    );

    // --- Things -----------------------------------------------------------
    //
    // Four commands rather than four-times-twelve: the Thing is named by the
    // second key of the sequence, which arrives in `ctx.key`, so `, r` and
    // `, p` are the same command told different things. Binding each pair
    // separately keeps every one of them individually rebindable.

    /// The Thing a key names, from `meow-char-thing-table`.
    fn thing_named(key: Option<Key>) -> Option<&'static str> {
        match key {
            Some(Key::Char(c)) => THING_CHARS
                .iter()
                .find(|(ch, _)| *ch == c)
                .map(|(_, name)| *name),
            _ => None,
        }
    }

    // Four commands rather than four-times-nineteen, because the Thing is a
    // *keypress*: `, r` and `, p` are one command told different things. The
    // seventy-six that name a Thing outright are a different question, and
    // `thing_table!` answers it — they exist for the chords a config wants and
    // for compositions, neither of which has a second key to be told with.
    macro_rules! of_thing {
        ($name:literal, $desc:literal, $select:expr) => {
            reg.register(
                Command::new(
                    $name,
                    Handler::Native(|ctx| {
                        // No Thing, by any of three routes: a composition
                        // and a hook arrive with no key at all, and `repeat`
                        // forwards its own. The keymap binds these four to the
                        // nineteen Thing keys and to nothing else, so a
                        // keypress never lands here. It beeped wordlessly by
                        // every route until #78. Whatever wants a particular
                        // Thing names the command for it instead.
                        //
                        // Signalling is what Emacs does — `fidelity`'s
                        // `repeat-*` cases have `[ l '` raise and leave the
                        // selection where the two keys put it — so the
                        // sentence is the only thing meowr adds here.
                        let Some(thing) = thing_named(ctx.key) else {
                            return Outcome::Refused(concat!(
                                "`",
                                $name,
                                "` takes its Thing from the next key."
                            ));
                        };
                        select_thing(ctx, thing, $select)
                    }),
                )
                .described($desc)
                .in_states(&[State::Normal]),
            );
        };
    }

    // `meow-thing-selection-directions`: inner and end run forward, bounds and
    // beginning run backward. The direction decides which end the point lands
    // on, which decides what the next motion extends.
    of_thing!("inner-of-thing", "Select inside a thing", inner_of_thing);
    of_thing!(
        "bounds-of-thing",
        "Select a thing and its delimiters",
        bounds_of_thing
    );
    // Both of these read the *inner* bounds, which is what the elisp does —
    // `meow-beginning-of-thing` and `meow-end-of-thing` each call
    // `meow--parse-inner-of-thing-char`, not the bounds one.
    of_thing!(
        "beginning-of-thing",
        "Select back to the start of a thing",
        beginning_of_thing
    );
    of_thing!(
        "end-of-thing",
        "Select on to the end of a thing",
        end_of_thing
    );

    // `meow-undo` cancels the selection and then undoes; `meow-undo-in-selection`
    // undoes with the region live and lets Emacs narrow the work to it.
    editing!("undo", "Undo the last change", |ctx| {
        // Cancel first, as `meow-undo` does. It is not redundant with the
        // selection the transaction restores: it is what makes `u` mean "undo"
        // rather than "undo in the region", which is `U`. Emacs's single
        // `C-/` decides between the two by whether a region is live, and Meow
        // disambiguates by cancelling.
        core::cancel(ctx.selection);
        if ctx.history.undo(ctx.doc, ctx.selection) {
            Outcome::Handled
        } else {
            Outcome::Refused("Nothing to undo.")
        }
    });

    reg.register(
        Command::new(
            "undo-in-selection",
            Handler::Native(|ctx| {
                let range = ctx.selection.current().range();
                if ctx.history.undo_in(ctx.doc, ctx.selection, range) {
                    Outcome::Handled
                } else {
                    // `undo` says "Nothing to undo." and `redo` says "No
                    // further redo information."; this one beeped. Narrower
                    // than `undo`'s, because the history may be far from empty
                    // and still have nothing inside these bounds.
                    Outcome::Refused("Nothing to undo in the selection.")
                }
            }),
        )
        .described("Undo only what happened inside the selection")
        .mutating()
        .requiring_selection()
        .in_states(&[State::Normal]),
    );

    // Redo has no key in Meow's table, because Emacs has no redo: you undo an
    // undo. meowr keeps an explicit stack (see `meowr_core::undo`), so it needs
    // a key, and `C-r` is where the muscle memory of every other editor is.
    editing!("redo", "Redo the last undone change", |ctx| {
        if ctx.history.redo(ctx.doc, ctx.selection) {
            Outcome::Handled
        } else {
            Outcome::Refused("No further redo information.")
        }
    });

    // --- windows and buffers ----------------------------------------------
    //
    // Registered so the keymap and, later, a Plugin can name them; carried out
    // by the session, which owns the window tree. A command that rearranges
    // the frame is a different kind of thing from one that edits a buffer, and
    // `Ctx` says so by not offering it.
    for (name, description) in [
        ("split-below", "Split the window, the new one below"),
        ("split-right", "Split the window, the new one to the right"),
        ("other-window", "Move to the next window"),
        ("delete-window", "Close this window"),
        ("delete-other-windows", "Close every window but this one"),
        ("next-buffer", "Show the next buffer in this window"),
    ] {
        reg.register(
            Command::new(name, Handler::Native(|_| Outcome::Handled))
                .described(description)
                .in_states(&[State::Normal]),
        );
    }

    // The narrowing widget. Opened by a command so it is nameable and
    // rebindable; carried out by the session, which owns the Echo Area.
    // `#17` asks each command to carry "whether they take a numeric argument".
    // `find`, `till` and `search` all read one — a negative argument sends
    // them backward — so they say so, even though `dispatch` is what acts on
    // it. Metadata that only some of the commands it describes bother to set
    // is worse than none: the leader menu and, in M2, Steel read this.
    for (name, description, counted) in [
        ("find", "Select to the next occurrence of a character", true),
        (
            "till",
            "Select up to the next occurrence of a character",
            true,
        ),
        ("search", "Select the next match of the search ring", true),
        (
            "visit",
            "Choose a symbol in the buffer and select it",
            false,
        ),
        ("find-file", "Narrow over buffers, files and lines", false),
        ("switch-to-buffer", "Switch to an open buffer", false),
        ("recent-files", "Open a recently visited file", false),
        (
            "complete-at-point",
            "Complete the symbol under the point",
            false,
        ),
    ] {
        let command =
            Command::new(name, Handler::Native(|_| Outcome::Handled)).described(description);
        reg.register(if counted {
            command.taking_count()
        } else {
            command
        });
    }

    // The surround keymap, on `M-'`. Wrapping and unwrapping a selection is
    // paredit's territory in Emacs, which is unbound on stock — so there is
    // nothing to be faithful to and it can simply be right (ADR-0006).
    //
    // **The pair comes from the name, not from the key.** It used to be read
    // back out of `ctx.key` through `surround_pair`, which is the mistake #78
    // found in the Thing commands wearing different clothes — and a worse case
    // of it, because `surround-round` already says which pair it means. Bound
    // under `c` it inserted curly braces while calling itself round, and in a
    // composition, which has no key at all (#78), it beeped and said nothing
    // (#88). A `Handler::Native` is a bare `fn` pointer and cannot capture, so
    // the pair has to be a literal in each body — hence a macro rather than
    // the loop that was here.
    macro_rules! surround {
        ($name:literal, $open:literal, $close:literal) => {
            reg.register(
                Command::new(
                    $name,
                    Handler::Native(|ctx| {
                        // No selection check here. `requiring_selection`
                        // below says it and the session enforces it, for every
                        // way in — #88 had to repeat it in this body because a
                        // composition's steps went round the guards entirely,
                        // and #89 put them back behind. A guard inside a
                        // command is one a command can forget, which is what
                        // the comment on `dispatch_command`'s guards means.
                        let range = ctx.selection.current().range();
                        // The closing delimiter first: inserting the opening
                        // one would shift the closing offset out from under
                        // itself.
                        let _ = ctx
                            .history
                            .insert(ctx.doc, ctx.selection, range.end, $close);
                        let _ = ctx
                            .history
                            .insert(ctx.doc, ctx.selection, range.start, $open);
                        Outcome::Handled
                    }),
                )
                .described(concat!("Wrap the selection in ", $open, " ", $close))
                .mutating()
                .requiring_selection()
                .in_states(&[State::Normal]),
            );
        };
    }
    surround!("surround-round", "(", ")");
    surround!("surround-square", "[", "]");
    surround!("surround-curly", "{", "}");
    surround!("surround-quote", "\"", "\"");

    reg.register(
        Command::new(
            "unsurround",
            Handler::Native(|ctx| {
                // The delimiters just outside the selection, if they are a
                // pair. Unwrapping something that is not wrapped is a mistake
                // worth refusing rather than guessing at.
                let range = ctx.selection.current().range();
                let before = meowr_core::previous_char_boundary(ctx.doc, range.start);
                let text = ctx.doc.text();
                let at = |offset: usize| {
                    (offset < text.len_bytes()).then(|| text.char(text.byte_to_char(offset)))
                };
                let (Some(start), Some(open), Some(close)) =
                    (before, before.and_then(at), at(range.end))
                else {
                    return Outcome::Refused("The selection is not wrapped.");
                };
                if closing_delimiter(open) != Some(close) {
                    return Outcome::Refused("The selection is not wrapped.");
                }
                let _ = ctx.history.delete(
                    ctx.doc,
                    ctx.selection,
                    range.end..range.end + close.len_utf8(),
                );
                let _ = ctx
                    .history
                    .delete(ctx.doc, ctx.selection, start..range.start);
                Outcome::Handled
            }),
        )
        .described("Remove the delimiters around the selection")
        .mutating()
        .requiring_selection()
        .in_states(&[State::Normal]),
    );

    // The session's own, from the one table that also says what they do. The
    // handler is never reached — `dispatch_command` answers these names before
    // the registry is consulted — but the entry has to exist for the keymap to
    // resolve them and for the leader menu to have a description to show.
    for (name, description, states, _) in SESSION_COMMANDS {
        reg.register(
            Command::new(*name, Handler::Native(|_| Outcome::Handled))
                .described(*description)
                .in_states(states),
        );
    }

    reg.register(
        Command::new(
            "reverse",
            Handler::Native(|ctx| {
                core::reverse(ctx.selection);
                Outcome::Handled
            }),
        )
        .described("Reverse the direction of the selection")
        // `meow-reverse` goes through `meow--with-selection-fallback`, and
        // `meow-selection-command-fallback` has no entry for it — so with no
        // selection it raises "No selection" rather than doing nothing.
        .requiring_selection()
        .in_states(&[State::Normal]),
    );

    reg.register(
        Command::new(
            "cancel-selection",
            Handler::Native(|ctx| {
                core::cancel(ctx.selection);
                Outcome::Handled
            }),
        )
        .described("Cancel the selection")
        // Falls back to `keyboard-quit`, which signals.
        .requiring_selection()
        .in_states(&[State::Normal]),
    );

    reg.register(
        Command::new(
            "pop-selection",
            Handler::Native(|ctx| {
                ctx.selection.pop();
                Outcome::Handled
            }),
        )
        .described("Restore the previous selection")
        .requiring_selection()
        .in_states(&[State::Normal]),
    );

    register_thing_commands(&mut reg);

    reg
}

/// Register a Normal State motion over a core function.
/// A motion that can fail the way Emacs's does: `h` at the beginning of the
/// buffer runs `backward-char`, which raises `beginning-of-buffer`.
macro_rules! moving {
    ($reg:expr, $name:literal, $desc:literal, $edge:literal, $f:expr) => {
        $reg.register(
            Command::new(
                $name,
                Handler::Native(|ctx| {
                    if ($f)(ctx.doc, ctx.selection) {
                        Outcome::Handled
                    } else {
                        // The edge it ran into, which is Emacs's own message:
                        // `backward-char` at position zero raises
                        // `beginning-of-buffer`, and the command loop prints
                        // its name. Passed in per command because the macro
                        // cannot know which way it was going.
                        Outcome::Refused($edge)
                    }
                }),
            )
            .described($desc)
            .in_states(&[State::Normal]),
        );
    };
}
use moving;

/// A Normal State motion that reads the numeric argument.
///
/// `$adapt` is *how* the elisp reads it, and it stays at the registration
/// because the readings genuinely differ: `(interactive "p")` is a signed
/// count (`Argument::count`), `meow-mark-thing` takes only `(< n 0)`
/// (`Argument::is_negative`), and `meow-left` takes no argument at all —
/// which is what `moving!` is still for.
macro_rules! counted {
    ($reg:expr, $name:literal, $desc:literal, $adapt:expr, $f:expr) => {
        $reg.register(
            Command::new(
                $name,
                Handler::Native(|ctx| {
                    ($f)(ctx.doc, ctx.selection, ($adapt)(ctx.argument));
                    Outcome::Handled
                }),
            )
            .described($desc)
            .taking_count()
            .in_states(&[State::Normal]),
        );
    };
}
use counted;

/// `meow-next` and `meow-prev`, and their expanding twins.
///
/// These four have their own macro because they are the commands whose
/// docstring says "use with universal argument to move to the last line of
/// buffer". That is a jump rather than a count, so the argument is read twice:
/// once for the branch and once for the number.
///
/// `false` from the motion is where Emacs signals, so it has to reach
/// `Outcome::Failed` and not be swallowed: the harness counts signals.
macro_rules! line_motion {
    ($reg:expr, $name:literal, $desc:literal, $axis:expr, $down:literal) => {
        $reg.register(
            Command::new(
                $name,
                Handler::Native(|ctx| {
                    // `(meow--with-universal-argument-p arg)` — exactly one
                    // `C-u`. `C-u C-u` is a different argument and `C-u 4` is
                    // a count, and both fall through to the branch below.
                    if ctx.argument.is_universal() {
                        core::to_buffer_edge(ctx.doc, ctx.selection, $axis, $down);
                        return Outcome::Handled;
                    }
                    let n = ctx.argument.count();
                    let moved = if $down {
                        core::next_lines(ctx.doc, ctx.selection, $axis, n)
                    } else {
                        core::prev_lines(ctx.doc, ctx.selection, $axis, n)
                    };
                    if moved {
                        Outcome::Handled
                    } else {
                        // Which edge, from which way it was going: the macro
                        // knows `$down` where `moving!` had to be told.
                        Outcome::Refused(if $down {
                            "End of buffer."
                        } else {
                            "Beginning of buffer."
                        })
                    }
                }),
            )
            .described($desc)
            .taking_count()
            .in_states(&[State::Normal]),
        );
    };
}
use line_motion;

/// [`meowr_core::Delimiters`] answered from the syntax tree.
///
/// The adapter for the port `meow-block` and `meow-to-block` scan through:
/// meowr-core parses nothing, and this crate already holds the tree that was
/// parsed to colour the buffer, so the question is asked here and nothing is
/// parsed twice.
///
/// `None` where the buffer has no grammar. That is not a failure — it is the
/// honest answer that there is nothing better to ask than the characters
/// themselves, and the scans then count every bracket exactly as they did
/// before the tree was consulted at all.
///
/// It is also the answer while a *parse is still running*, since the tree
/// arrives with `apply_reparse` and not before. So `o` pressed in the first
/// frames after opening a file counts a `)` in a string, and stops once the
/// parse lands. Blocking the keystroke on the parse would be the worse trade —
/// the whole reason parsing is off the keystroke path — so this is recorded
/// rather than fixed.
enum Syntactic<'a> {
    /// The buffer has a grammar, and the tree can say what is text.
    Tree(&'a meowr_syntax::Syntax),
    /// It has none, and every bracket counts.
    Blind,
}

impl<'a> Syntactic<'a> {
    fn of(objects: Option<&meowr_syntax::TextObjects<'a>>) -> Self {
        match objects {
            Some(objects) => Self::Tree(objects.syntax),
            None => Self::Blind,
        }
    }

    /// As the port, or `None` when there is nothing to answer with.
    fn as_port(&self) -> Option<&dyn core::Delimiters> {
        match self {
            Self::Tree(_) => Some(self),
            Self::Blind => None,
        }
    }
}

impl core::Delimiters for Syntactic<'_> {
    fn counts(&self, byte: usize) -> bool {
        match self {
            Self::Tree(syntax) => meowr_syntax::literal_at(syntax, byte).is_none(),
            Self::Blind => true,
        }
    }
}

/// [`resolve_thing`] with the pieces a command already has to hand.
///
/// The Thing commands reach every registry through here. They used to ask
/// `ctx.things` alone, which is why `, w` and `, a` were bound keys that did
/// nothing: `window` is not in that table and neither is anything tree-sitter
/// knows.
fn thing_at(ctx: &Ctx<'_>, pos: usize, name: &str, extent: Extent) -> Option<Range<usize>> {
    resolve_thing(
        ctx.things,
        ctx.text_objects.as_ref(),
        ctx.window.clone(),
        ctx.doc,
        pos,
        name,
        extent,
    )
}

/// Take the Thing named `thing`, however the command was told which.
///
/// The one body behind both ways of naming a Thing: `[ d`, where the `d` is a
/// keypress, and `beginning-of-thing/defun`, where it is the command's own
/// name. Two routes to one behaviour rather than two behaviours that have to
/// be kept agreeing.
fn select_thing(
    ctx: &mut Ctx<'_>,
    thing: &str,
    select: fn(&Ctx<'_>, usize, &str) -> Option<Selection>,
) -> Outcome {
    let pos = ctx.selection.current().pos();
    // A Thing that is not found leaves the selection alone and does **not**
    // signal: `meow--select-range` is a bare `(when bounds ...)`, so `, s`
    // where there is no bracket is quiet. Returning `Failed` here made the
    // harness fail forty cases at once.
    if let Some(selection) = select(ctx, pos, thing) {
        ctx.selection.select(selection);
    }
    Outcome::Handled
}

/// `meow-inner-of-thing`.
fn inner_of_thing(ctx: &Ctx<'_>, pos: usize, thing: &str) -> Option<Selection> {
    let r = thing_at(ctx, pos, thing, Extent::Inner)?;
    Some(Selection::typed(
        Axis::Select,
        Kind::TRANSIENT,
        r.start,
        r.end,
    ))
}

/// `meow-bounds-of-thing`.
fn bounds_of_thing(ctx: &Ctx<'_>, pos: usize, thing: &str) -> Option<Selection> {
    let r = thing_at(ctx, pos, thing, Extent::Bounds)?;
    Some(Selection::typed(
        Axis::Select,
        Kind::TRANSIENT,
        r.end,
        r.start,
    ))
}

/// `meow-beginning-of-thing`, which reads the *inner* bounds.
fn beginning_of_thing(ctx: &Ctx<'_>, pos: usize, thing: &str) -> Option<Selection> {
    let r = thing_at(ctx, pos, thing, Extent::Inner)?;
    Some(Selection::typed(
        Axis::Select,
        Kind::TRANSIENT,
        pos,
        r.start,
    ))
}

/// `meow-end-of-thing`, likewise.
fn end_of_thing(ctx: &Ctx<'_>, pos: usize, thing: &str) -> Option<Selection> {
    let r = thing_at(ctx, pos, thing, Extent::Inner)?;
    Some(Selection::typed(Axis::Select, Kind::TRANSIENT, pos, r.end))
}

/// A command the session carries out itself.
type SessionCommand = fn(&mut Session) -> Action;

/// The commands the session carries out itself, with what they are called and
/// what they do, in one place.
///
/// A `Handler::Native` is handed a `Ctx` — a document and its selection — and
/// these need the session: a window tree, the narrowing widget, the Grab. That
/// is why `dispatch_command` answers them by name instead of the registry
/// running them. What it is *not* a reason for is saying so twice: this was a
/// `match` arm here and a registration over there with a handler that could
/// never run, and the two had to agree by hand. Now the registry is built from
/// this table and `dispatch_command` looks names up in it.
const SESSION_COMMANDS: &[(&str, &str, &[State], SessionCommand)] = &[
    (
        "leader",
        "Open the leader",
        &[State::Normal],
        Session::open_leader,
    ),
    (
        "describe-bindings",
        "Show every binding",
        &[State::Normal],
        Session::describe_bindings,
    ),
    (
        "quit-window",
        "Close a buffer meowr wrote",
        &[State::Normal],
        Session::quit_window,
    ),
    (
        "trail-back",
        "Go back to where the last jump left",
        &[State::Normal],
        Session::trail_back,
    ),
    (
        "lookup-references",
        "Find where the symbol at the point is used",
        &[State::Normal],
        Session::lookup_references,
    ),
    (
        "lookup-definition",
        "Jump to where the symbol at the point is defined",
        &[State::Normal],
        Session::lookup_definition,
    ),
    (
        "lookup-type-definition",
        "Jump to where the type of the symbol at the point is defined",
        &[State::Normal],
        Session::lookup_type_definition,
    ),
    (
        "lookup-implementations",
        "Find what implements the symbol at the point",
        &[State::Normal],
        Session::lookup_implementations,
    ),
    (
        "move-this-file",
        "Move or rename this file",
        &[State::Normal],
        Session::move_this_file,
    ),
    (
        "copy-this-file",
        "Copy this file, and open the copy",
        &[State::Normal],
        Session::copy_this_file,
    ),
    (
        "delete-this-file",
        "Delete this file",
        &[State::Normal],
        Session::delete_this_file,
    ),
    (
        "kill-all-buffers",
        "Kill every buffer",
        &[State::Normal],
        Session::kill_all_buffers,
    ),
    (
        "toggle-scratch-buffer",
        "Show the scratch buffer below, or close its window",
        &[State::Normal],
        Session::toggle_scratch_buffer,
    ),
    (
        "toggle-project-scratch-buffer",
        "Show this project's scratch buffer below, or close its window",
        &[State::Normal],
        Session::toggle_project_scratch_buffer,
    ),
    (
        "switch-to-project-scratch-buffer",
        "Switch to this project's scratch buffer",
        &[State::Normal],
        Session::switch_to_project_scratch_buffer,
    ),
    (
        "lsp-rename",
        "Rename the symbol at the point",
        &[State::Normal],
        Session::lsp_rename,
    ),
    (
        "lsp-execute-code-action",
        "Offer the code actions at the point",
        &[State::Normal],
        Session::lsp_execute_code_action,
    ),
    (
        "lsp-organize-imports",
        "Organize imports",
        &[State::Normal],
        Session::lsp_organize_imports,
    ),
    (
        "lookup-symbols",
        "Find a symbol in this workspace",
        &[State::Normal],
        Session::lookup_symbols,
    ),
    (
        "lookup-symbols-everywhere",
        "Find a symbol in any workspace",
        &[State::Normal],
        Session::lookup_symbols_everywhere,
    ),
    (
        "fold-toggle",
        "Fold what is open at the point, or open what is folded",
        &[State::Normal],
        Session::fold_toggle,
    ),
    (
        "fold-open",
        "Open the fold at the point",
        &[State::Normal],
        Session::fold_open,
    ),
    (
        "fold-close-all",
        "Fold everything in this buffer",
        &[State::Normal],
        Session::fold_close_all,
    ),
    (
        "fold-open-all",
        "Open every fold in this buffer",
        &[State::Normal],
        Session::fold_open_all,
    ),
    (
        "search-definitions",
        "List what every open buffer defines",
        &[State::Normal],
        Session::search_definitions,
    ),
    (
        "search-buffers",
        "Search every open buffer",
        &[State::Normal],
        Session::search_buffers,
    ),
    (
        "search-buffer",
        "Search this buffer for the symbol at the point",
        &[State::Normal],
        Session::search_buffer,
    ),
    (
        "search-project",
        "Search the project this file is in",
        &[State::Normal],
        Session::search_project,
    ),
    (
        "search-elsewhere",
        "Search a directory you name",
        &[State::Normal],
        Session::search_elsewhere,
    ),
    (
        "search-symbol",
        "Search the project for the symbol at the point",
        &[State::Normal],
        Session::search_symbol,
    ),
    (
        "search-here",
        "Search the directory this file is in",
        &[State::Normal],
        Session::search_here,
    ),
    (
        "list-diagnostics",
        "List what a language server said about this buffer",
        &[State::Normal],
        Session::list_diagnostics,
    ),
    (
        "visit-place",
        "Go to the place the line under the point names",
        &[State::Normal],
        Session::visit_place,
    ),
    (
        "escape",
        "Cancel the selection, close what meowr wrote, or abort",
        &[State::Normal],
        Session::escape,
    ),
    (
        "overwrite-file",
        "Write over the file already at this name",
        &[State::Normal],
        Session::overwrite_file,
    ),
    (
        "recenter",
        "Put the selection in the middle of the window",
        &[State::Normal],
        Session::recenter,
    ),
    (
        "scroll-up",
        "Show the next screenful",
        &[State::Normal],
        Session::scroll_up,
    ),
    (
        "scroll-down",
        "Show the previous screenful",
        &[State::Normal],
        Session::scroll_down,
    ),
    (
        "beacon-start",
        "Record an edit to replay at every beacon",
        &[State::Beacon],
        Session::beacon_start,
    ),
    (
        "grab",
        "Grab the selection as a second one",
        &[State::Normal],
        Session::grab_command,
    ),
    (
        "pop-grab",
        "Make the Grab the selection",
        &[State::Normal],
        Session::pop_grab,
    ),
    (
        "swap-grab",
        "Exchange the selection and the Grab",
        &[State::Normal],
        Session::swap_grab,
    ),
    (
        "sync-grab",
        "Give the Grab the selection's text",
        &[State::Normal],
        Session::sync_grab,
    ),
    // The three that open the narrowing widget carry no States at all, which
    // `valid_in` reads as "any": `complete-at-point` is on `C-n` in Insert
    // State, and pinning these to Normal is what broke it.
    ("quit", "Quit meowr", &[], Session::quit),
    (
        "quit-without-saving",
        "Quit, discarding unsaved changes",
        &[],
        Session::quit_without_saving,
    ),
    (
        "lookup-documentation",
        "Jump to documentation",
        &[],
        Session::lookup_documentation,
    ),
    ("kill-buffer", "Kill this buffer", &[], Session::kill_buffer),
    (
        "kill-buffer-without-saving",
        "Kill this buffer, discarding unsaved changes",
        &[],
        Session::kill_buffer_without_saving,
    ),
    (
        "kill-buffer-and-window",
        "Kill this buffer and close its window",
        &[],
        Session::kill_buffer_and_window,
    ),
    (
        "kill-other-buffers",
        "Kill every buffer but this one",
        &[],
        Session::kill_other_buffers,
    ),
    (
        "kill-other-buffers-without-saving",
        "Kill every buffer but this one, discarding unsaved changes",
        &[],
        Session::kill_other_buffers_without_saving,
    ),
    (
        "switch-to-scratch-buffer",
        "Show the scratch buffer in this window",
        &[],
        Session::switch_to_scratch_buffer,
    ),
    (
        "open-scratch-buffer",
        "Show the scratch buffer in a window of its own",
        &[],
        Session::open_scratch_buffer,
    ),
    (
        "toggle-read-only",
        "Toggle whether this buffer is read-only",
        &[],
        Session::toggle_read_only,
    ),
    (
        "recover-file",
        "Recover unsaved work from the auto-save file",
        &[],
        Session::recover_file,
    ),
    (
        "visit",
        "Choose a symbol in the buffer and select it",
        &[],
        Session::visit,
    ),
    (
        "find-file",
        "Narrow over buffers, files and lines",
        &[],
        Session::find_file,
    ),
    (
        "execute-extended-command",
        "Run a command by name",
        &[State::Normal],
        Session::execute_extended_command,
    ),
    (
        "switch-to-buffer",
        "Switch to an open buffer",
        &[],
        Session::switch_to_buffer,
    ),
    (
        "recent-files",
        "Open a recently visited file",
        &[],
        Session::recent_files,
    ),
    (
        "save-some-buffers",
        "Save every modified buffer",
        &[],
        Session::save_some_buffers,
    ),
    (
        "format-buffer",
        "Format this buffer with its Language's formatter",
        &[],
        Session::format_buffer,
    ),
    (
        "yank-buffer-path-relative-to-project",
        "Copy this file's path from its project's root",
        &[],
        Session::yank_buffer_path_relative_to_project,
    ),
    (
        "project-recent-files",
        "Open a recent file in this file's project",
        &[],
        Session::project_recent_files,
    ),
    (
        "find-file-under-here",
        "Open a file under this file's directory",
        &[],
        Session::find_file_under_here,
    ),
    (
        "project-find-file",
        "Open a file in the project this file is in",
        &[],
        Session::project_find_file,
    ),
    (
        "restart",
        "Start meowr again, losing what is on screen",
        &[],
        Session::restart,
    ),
    (
        "open-config",
        "Open init.scm, whether or not it is there yet",
        &[],
        Session::open_config,
    ),
    (
        "find-file-in-config",
        "Open a file beside init.scm",
        &[],
        Session::find_file_in_config,
    ),
    (
        "search-lines",
        "Narrow over the lines of this buffer",
        &[State::Normal],
        Session::search_lines,
    ),
    (
        "describe-commands",
        "List every command name a config may use",
        &[State::Normal],
        Session::describe_commands,
    ),
    (
        "yank-buffer-path",
        "Copy this buffer's path into the ring",
        &[State::Normal],
        Session::yank_buffer_path,
    ),
    (
        "complete-at-point",
        "Complete the symbol under the point",
        &[],
        Session::complete_at_point,
    ),
];
/// Where a Thing is, from whichever registry knows.
///
/// Three of them, and a caller that tries only one leaves a bound key doing
/// nothing: the lexical table in `meowr-core`, the viewport (which only
/// `window` asks about, being the one Thing that is a property of what is on
/// screen rather than of the text), and the tree-sitter queries. The Thing
/// commands and [`Session::thing_range`] both come here so that the two cannot
/// answer differently.
fn resolve_thing(
    things: &core::thing::Things,
    text_objects: Option<&meowr_syntax::TextObjects<'_>>,
    window: Option<Range<usize>>,
    doc: &Document,
    pos: usize,
    name: &str,
    extent: Extent,
) -> Option<Range<usize>> {
    // Inner and bounds are the same for a window: it has no delimiters for a
    // selection to sit inside of.
    if name == "window" {
        return window;
    }
    // Beside `window`, and outside the registry for the same shape of reason:
    // `Things` is a pure function of the document and this one has a second
    // question to ask. `window` needs the viewport; `match` needs to know
    // which brackets are real, which is the syntax tree's answer and not the
    // text's. See [`core::thing::enclosing_pair`] for why it asks at all when
    // `, r` does not.
    if name == "match" {
        let syntactic = Syntactic::of(text_objects);
        return core::thing::enclosing_pair(doc, pos, syntactic.as_port(), extent);
    }
    if things.get(name).is_some() {
        return if extent.is_inner() {
            things.inner(doc, pos, name)
        } else {
            things.bounds(doc, pos, name)
        };
    }
    // ADR-0006 puts `defun` with tree-sitter "where it can be right, rather
    // than with a heuristic that would have to be wrong in the same places
    // Emacs's is". The vendored Helix queries spell it `function`, so the name
    // Meow's char table uses is translated here rather than duplicated into
    // every grammar's query file.
    let name = if name == "defun" { "function" } else { name };
    let objects = text_objects?;
    meowr_syntax::text_object(
        objects.syntax,
        doc.text().slice(..),
        objects.languages,
        pos,
        name,
        // `meowr-syntax` does not depend on `meowr-core` and cannot name
        // `Extent`, so the flag stops being a type at this boundary and not
        // before it.
        extent.is_inner(),
    )
}

/// What closes `open`, if `open` opens anything.
///
/// A question about a character **in the buffer**, which since #88 is the only
/// question anyone asks. It used to answer a second one — which pair a surround
/// *key* names, where `r` meant round brackets because `r` is the key you press
/// for them — and `unsurround` has been reading buffer characters through it the
/// whole time. So a plain `r` before the selection and a `)` after it looked
/// wrapped, and `barX)` unwrapped to `baX`. The key letters went out with the
/// key lookup they belonged to, and the closing delimiters with them: `)` does
/// not open anything either.
fn closing_delimiter(open: char) -> Option<char> {
    match open {
        '(' => Some(')'),
        '[' => Some(']'),
        '{' => Some('}'),
        '"' => Some('"'),
        _ => None,
    }
}

/// Ring 3: the personal layer, expressed as hooks rather than as special
/// cases inside the commands they affect.
///
/// Auto-exit-on-save is the one that proves the mechanism. It is a *user
/// preference*, and it lives outside `write-file` entirely — which is what the
/// Steel seam will need in M2, and what a flag inside the save command would
/// have quietly made impossible.
pub fn default_hooks() -> Hooks {
    let mut hooks = Hooks::new();
    hooks.add("auto-exit-insert-on-save", HookPoint::BeforeSave, |ctx| {
        if *ctx.state == State::Insert {
            *ctx.state = State::Normal;
        }
        Proceed::Yes
    });
    hooks
}

/// The part of a sequence that lands in Ring 2, if it does.
///
/// Normal State only, and longer than the leader key itself: binding
/// [`leader::KEY`] alone is rebinding what that key *does*, which is the
/// keymap's business and not the tree's.
fn leader_suffix(state: State, keys: &[Key]) -> Option<&[Key]> {
    (state == State::Normal && keys.len() > 1 && keys.first() == Some(&leader::KEY))
        .then(|| &keys[1..])
}

/// What stands in for a command when there is no selection.
pub fn default_fallbacks() -> Fallbacks {
    let mut fallbacks = Fallbacks::new();
    fallbacks.set("pop-selection", "pop-grab");
    fallbacks.set("cancel-selection", "keyboard-quit");
    // `meow-selection-command-fallback` names this pairing, and in Emacs it
    // never fires: the fallback is looked up by `this-command`, which is
    // `meow-expand-2` rather than `meow-expand`, so a digit with no selection
    // raises "No selection" instead. meowr does what the table says, and the
    // divergence is recorded in the fidelity case generator.
    fallbacks.set("expand", "digit-argument");
    fallbacks.set("change", "change-char");
    fallbacks.set("kill", "kill-line");
    fallbacks.set("save", "copy-region");
    // `meow-save` falls back to `kill-ring-save`, which signals without a
    // region; `meow-replace` has no entry at all. Both therefore signal, and
    // saying so by omission is the same statement the elisp makes.
    fallbacks
}

/// One Thing command whose Thing is its own name rather than the next key.
///
/// The name and the description are both `concat!`ed from the Thing, so the
/// nineteen rows of the table carry no prose of their own and cannot drift
/// from it. Most descriptions end at the Thing, so the suffix is an arm rather
/// than three empty strings at the call sites.
///
/// `register_new` rather than `register`, and the reason is that these names
/// are *built* rather than written: nobody reads one before it is registered,
/// so a collision cannot be caught by eye. `register` would replace what it
/// collided with in silence, which is how `beginning-of-buffer` would have
/// quietly stopped being a motion.
macro_rules! of_one_thing {
    ($reg:expr, $prefix:literal, $thing:literal, $before:literal, $select:expr) => {
        of_one_thing!($reg, $prefix, $thing, $before, "", $select)
    };
    ($reg:expr, $prefix:literal, $thing:literal, $before:literal, $after:literal, $select:expr) => {
        $reg.register_new(
            Command::new(
                concat!($prefix, $thing),
                Handler::Native(|ctx| select_thing(ctx, $thing, $select)),
            )
            .described(concat!($before, $thing, $after))
            .in_states(&[State::Normal]),
        )
        .expect("a Thing command must not take a name already registered");
    };
}

/// The Thing keys, and the nineteen-times-four commands that name a Thing
/// outright.
///
/// One list with two products, because they are two halves of one fact and
/// nothing in the compiler would otherwise make them agree — the failure this
/// codebase keeps writing tests against. The count is written once and checked
/// by the array's own type.
///
/// **Why the second product exists.** A Thing normally arrives as a keypress,
/// which is what makes `,` `.` `[` `]` four commands rather than seventy-six.
/// But a keypress is not always there to arrive: the author's config binds a
/// *chord* to `beginning-of-defun` (#78), and a composed command (#48) has no
/// keys at all. Both need "beginning of defun" to be a name. These are that
/// name, and they are bound to nothing — Ring 3 material a config reaches for,
/// leaving `[ d` exactly as Meow has it.
///
/// Two decisions in the name, and only one of them is the `/`. Keeping
/// `-thing` in the middle is what avoids a collision: `beginning-of-buffer`
/// and `end-of-buffer` are already the `M-<` and `M->` *motions*, which move
/// the point without selecting, so the short names the ticket reached for are
/// taken for exactly the `buffer` Thing. The `/` then separates the Thing from
/// the operation, so a reader can see where the operation ends without
/// knowing the table — a hyphen would read as part of one long name.
macro_rules! thing_table {
    ($count:literal; $($ch:literal => $thing:literal),+ $(,)?) => {
        /// `meow-char-thing-table`.
        ///
        /// `window` and `defun` have no entry in the Thing registry and are
        /// still reachable: `window` is a property of the viewport and is
        /// answered from the window tree, and `defun` belongs to tree-sitter
        /// by ADR-0006, where the vendored queries spell it `function`. Both
        /// translations live in `thing_at`, so the names below behave the way
        /// their two keys do without knowing any of that.
        const THING_CHARS: [(char, &str); $count] = [$(($ch, $thing)),+];

        /// One command per operation and Thing, for the callers with no key.
        ///
        /// `register_new` rather than `register`: a name that collided would
        /// otherwise *replace* the command it collided with, in silence, which
        /// is how `beginning-of-buffer` would have stopped being a motion.
        fn register_thing_commands(reg: &mut Registry) {
            $(
                // Written out rather than looped, and this is the reason:
                // `Handler::Native` is a bare `fn` pointer and cannot capture,
                // so the Thing has to be a *literal* in each body. A runtime
                // loop over the table would need one closure holding one
                // `thing`, which is the capture the pointer cannot do.
                of_one_thing!(reg, "inner-of-thing/", $thing, "Select inside the ", inner_of_thing);
                of_one_thing!(
                    reg, "bounds-of-thing/", $thing, "Select the ", " and its delimiters", bounds_of_thing
                );
                of_one_thing!(
                    reg, "beginning-of-thing/", $thing, "Select back to the start of the ",
                    beginning_of_thing
                );
                of_one_thing!(
                    reg, "end-of-thing/", $thing, "Select on to the end of the ", end_of_thing
                );
            )+
        }
    };
}

thing_table!(
    19;
    // The six `meow-tree-sitter` adds, taking `,` and `/` from the lexical
    // table exactly as `meow-tree-sitter-register-defaults` does.
    'a' => "class",
    'f' => "function",
    't' => "test",
    'y' => "entry",
    ',' => "parameter",
    '/' => "comment",
    'r' => "round",
    's' => "square",
    'c' => "curly",
    'g' => "string",
    'e' => "symbol",
    'w' => "window",
    'b' => "buffer",
    'p' => "paragraph",
    'l' => "line",
    'v' => "visual-line",
    'd' => "defun",
    '.' => "sentence",
    // meowr's own, with no row in `meow-char-thing-table`: the innermost
    // bracket pair around the point, whichever of the three kinds it is.
    // `m` is free in Meow's table, so this claims a key rather than taking
    // one. Recorded as a divergence in the fidelity harness, since `, m` in
    // real Meow is a silent no-op.
    'm' => "match",
);

/// The keys meowr ships with.
///
/// Quit is bound to `C-c` only until the Normal State slice lands: Meow's
/// Colemak table leaves `q` unbound, and claiming it now would build a habit
/// the real keymap would have to break.
pub fn default_keymap() -> Keymap {
    let mut map = Keymap::new();
    // `C-u` is `universal-argument` in the global map, and Meow's Normal state
    // does not shadow it — so it is reachable there in Emacs, and is here.
    map.bind(State::Normal, &[Key::Ctrl('u')], "universal-argument")
        .expect("default keymap must not conflict");
    // `isearch-backward` upstream, and redo here — a deliberate divergence
    // (#103), not an unmeasured one. meowr has no incremental search to put
    // there: searching is `v` and `/`, which is where Meow puts it, and a key
    // held for a feature that is not coming is the promise decision #12 is
    // against. Redo has nowhere else obvious to be, `C-r` is where every
    // editor that is not Emacs puts it, and `u` undoes.
    map.bind(State::Normal, &[Key::Ctrl('r')], "redo")
        .expect("default keymap must not conflict");
    // `M-x`, where Emacs has it. Reachable from Meow's Normal State in the
    // author's Doom because Meow does not shadow the global map — the surface
    // #92 measured, of which this was the first row bound because it was asked
    // for rather than because it was measured.
    //
    // #103 then checked the eighteen keys meowr holds on that surface against
    // it. Nine agree, five differ only in the shorter name meowr gives the
    // same behaviour, one was wrong (`C-x b`, above), and three are deliberate
    // and are recorded where they are bound.
    map.bind(State::Normal, &[Key::Alt('x')], "execute-extended-command")
        .expect("default keymap must not conflict");
    // The `C-x` sequences, spelled as sequences. Keypad State (#30) will make
    // these reachable from Normal State without the modifier, but the chords
    // themselves are what Emacs has and cost nothing to bind now.
    for (key, command) in [
        (Key::Char('2'), "split-below"),
        (Key::Char('3'), "split-right"),
        (Key::Char('o'), "other-window"),
        (Key::Char('0'), "delete-window"),
        (Key::Char('1'), "delete-other-windows"),
        // `switch-to-buffer`, not `next-buffer`: Emacs puts choosing here and
        // stepping on `C-x <right>`, and the author's Doom rebinds it to
        // `consult-buffer`, which is the same act through a picker. meowr had
        // the stepping command on the choosing key until #103 measured this
        // surface — the first time anything had.
        (Key::Char('b'), "switch-to-buffer"),
        (Key::Ctrl('s'), "write-file"),
        (Key::Ctrl('f'), "find-file"),
        // Where `next-buffer` actually lives, straight off the table #92
        // produced. It was on `C-x b` and had to move when that key went back
        // to choosing; leaving the command keyless would have been a
        // regression dressed up as a fix. `C-x <left>` is `previous-buffer`
        // upstream and stays free until meowr has one.
        (Key::Right, "next-buffer"),
    ] {
        map.bind(State::Normal, &[Key::Ctrl('x'), key], command)
            .expect("default keymap must not conflict");
    }
    // `C-x C-c` is Emacs's own quit; `C-c` is meowr's, and shorter.
    //
    // Both are deliberate, and #103 is where they stopped being provisional.
    // Upstream `C-x C-c` is `save-buffers-kill-terminal`, whose prompt exists
    // to stop you losing work — and `quit` refuses with unsaved buffers and
    // names them, which is the same guarantee arrived at differently.
    //
    // `C-c` is the one worth writing down. Upstream it holds the leader tree a
    // second time: Doom sets `meow-keypad-leader-dispatch` to `C-c`, so
    // `C-c h` and `SPC h` are one binding reached two ways. meowr's leader is
    // a real tree rather than a dispatch fallback, so there is nothing here
    // for `C-c` to alias, and quit keeps it. The comment above used to say
    // this was temporary "until the Normal State slice lands". That slice has
    // landed — Ring 1 Normal is complete — so it is not temporary any more.
    map.bind(State::Normal, &[Key::Ctrl('x'), Key::Ctrl('c')], "quit")
        .expect("default keymap must not conflict");
    map.bind(State::Normal, &[Key::Ctrl('c')], "quit")
        .expect("default keymap must not conflict");
    // `+meow-escape`, and the fallback table already spells its second half:
    // `cancel-selection` stands aside for `keyboard-quit` where there is no
    // selection, which is "call `meow-cancel-selection', else fall back to
    // `doom/escape'" in the machinery meowr already had. Doom binds this
    // because ESC bound to `ignore` breaks popup handling, and meowr's popups
    // — the narrowing widget, the leader menu — want the same way out.
    map.bind(State::Normal, &[Key::Esc], "escape")
        .expect("default keymap must not conflict");
    // Emacs's own abort chord, on the same command Doom reaches through ESC.
    map.bind(State::Normal, &[Key::Ctrl('g')], "keyboard-quit")
        .expect("default keymap must not conflict");
    for (key, command) in [
        (Key::Alt('<'), "beginning-of-buffer"),
        (Key::Alt('>'), "end-of-buffer"),
    ] {
        map.bind(State::Normal, &[key], command)
            .expect("default keymap must not conflict");
    }
    // The arrows. `meow-normal` binds none, so this is Ring 3 and takes
    // nothing back — and they were reaching the session and dying there, which
    // is why an arrow navigated the narrowing widget and did nothing in a
    // buffer. The plain motions rather than the expanding ones: an arrow key
    // that silently started selecting would be the surprise.
    for (key, command) in [
        (Key::Up, "prev"),
        (Key::Down, "next"),
        (Key::Left, "left"),
        (Key::Right, "right"),
    ] {
        map.bind(State::Normal, &[key], command)
            .expect("default keymap must not conflict");
    }
    map.bind(State::Normal, &[Key::Ctrl('l')], "recenter")
        .expect("default keymap must not conflict");
    // Ring 3, on Emacs's own globals. `meow-normal` binds no scrolling at all
    // and neither does the Doom leader, so there is nothing upstream to be
    // faithful to and nothing to take back later — and these are the plain
    // chords rather than the `C-x` prefixes the author's config drops.
    //
    // `PgUp`/`PgDn` reach the same commands. `to_key` has translated them
    // since the beginning and nothing has ever bound them, which is how four
    // keys came to arrive at the session and die there.
    for (key, command) in [
        (Key::Ctrl('v'), "scroll-up"),
        (Key::PageDown, "scroll-up"),
        (Key::Alt('v'), "scroll-down"),
        (Key::PageUp, "scroll-down"),
    ] {
        map.bind(State::Normal, &[key], command)
            .expect("default keymap must not conflict");
    }
    map.bind(State::Insert, &[Key::Esc], "leave-insert")
        .expect("default keymap must not conflict");
    map.bind(State::Insert, &[Key::Ctrl('c')], "quit")
        .expect("default keymap must not conflict");
    // Not `beacon-start`, which this bound here on a comment claiming Meow
    // does. It does not: the Beacon keymap has `meow-insert` on `s` and remaps
    // it to `meow-beacon-insert`, while `meow-beacon-start` is reached only
    // through `<remap> <kmacro-start-macro>` and has no letter key at all
    // (#93). It keeps its name, which `M-x` (#91) makes enough.
    map.bind(State::Beacon, &[Key::Char('s')], "insert")
        .expect("default keymap must not conflict");
    map.bind(State::Beacon, &[Key::Ctrl('c')], "quit")
        .expect("default keymap must not conflict");
    // `C-x C-s` saves from Insert State too — it is a global chord in Emacs,
    // and needing to leave Insert State first to save would be exactly the
    // friction the auto-exit hook exists to remove.
    map.bind(
        State::Insert,
        &[Key::Ctrl('x'), Key::Ctrl('s')],
        "write-file",
    )
    .expect("default keymap must not conflict");
    // Ring 3: the personal layer. `C-q` leaves Insert State from anywhere,
    // which is the one binding worth having in both.
    map.bind(State::Insert, &[Key::Ctrl('q')], "leave-insert")
        .expect("default keymap must not conflict");
    map.bind(State::Insert, &[Key::Ctrl('n')], "complete-at-point")
        .expect("default keymap must not conflict");
    map.bind(State::Insert, &[Key::Enter], "newline")
        .expect("default keymap must not conflict");
    map.bind(State::Insert, &[Key::Backspace], "delete-backward")
        .expect("default keymap must not conflict");

    // Normal State, Colemak. Taken from the extracted table in
    // `~/docs/meowr-meow-normal.tsv`; only the motions this slice owns.
    for (key, command) in [
        ('h', "left"),
        ('H', "left-expand"),
        ('i', "right"),
        ('I', "right-expand"),
        ('e', "prev"),
        ('E', "prev-expand"),
        ('n', "next"),
        ('N', "next-expand"),
        ('w', "next-word"),
        ('W', "next-symbol"),
        ('b', "back-word"),
        ('B', "back-symbol"),
        ('m', "mark-word"),
        ('M', "mark-symbol"),
        ('l', "line"),
        ('o', "block"),
        ('L', "goto-line"),
        ('\'', "repeat"),
        ('O', "to-block"),
        ('j', "join"),
        (';', "reverse"),
        ('g', "cancel-selection"),
        ('z', "pop-selection"),
        ('0', "expand"),
        ('1', "expand"),
        ('2', "expand"),
        ('3', "expand"),
        ('4', "expand"),
        ('5', "expand"),
        ('6', "expand"),
        ('7', "expand"),
        ('8', "expand"),
        ('9', "expand"),
        ('-', "negative-argument"),
        ('s', "insert"),
        ('a', "append"),
        ('A', "open-below"),
        ('S', "open-above"),
        ('c', "change"),
        ('k', "kill"),
        ('y', "save"),
        ('p', "yank"),
        ('P', "yank-pop"),
        ('r', "replace"),
        ('x', "delete"),
        ('d', "delete"),
        ('X', "backward-delete"),
        ('u', "undo"),
        ('U', "undo-in-selection"),
        ('f', "find"),
        ('t', "till"),
        ('v', "search"),
        ('/', "visit"),
        ('G', "grab"),
        // Ring 3. `meow-normal` binds no `q`, so this takes nothing back —
        // and `q` is what an Emacs hand reaches for in a buffer it did not
        // open, because `quit-window` is on it in every one of them.
        ('q', "quit-window"),
        // Ring 3, and the same reasoning `q` above carries: `meow-normal` binds
        // no backtick, so this takes nothing back. Vim puts "jump to where you
        // were" on it, which is the nearest thing to a hand already trained for
        // this -- Doom binds nothing at all for going back outside evil's `C-o`,
        // so there is no sequence to preserve and this is coined (ADR-0011).
        ('`', "trail-back"),
        (' ', "leader"),
    ] {
        map.bind(State::Normal, &[Key::Char(key)], command)
            .expect("default keymap must not conflict");
    }
    // Ring 3, and the third key to take this reasoning: `meow-normal` binds no
    // Return, so this takes nothing back. It is what an Emacs hand presses on a
    // row in a list -- `flycheck-error-list` and `compile-goto-error` are both
    // on RET -- and in a buffer with no Places under it, it does nothing at all
    // (ADR-0011, which declines buffer-local keymaps for one list's sake).
    map.bind(State::Normal, &[Key::Enter], "visit-place")
        .expect("default keymap must not conflict");
    // `M-'` opens the surround keymap. Crossterm gives Alt as a modifier
    // rather than a key, which is why `Key::Alt` exists: without it the
    // modifier is dropped and the keymap cannot be reached from a terminal
    // at all. `C-'` is not a fallback — it has no encoding a terminal sends.
    for (key, command) in [
        ('r', "surround-round"),
        ('s', "surround-square"),
        ('c', "surround-curly"),
        ('g', "surround-quote"),
        ('\'', "unsurround"),
    ] {
        map.bind(State::Normal, &[Key::Alt('\''), Key::Char(key)], command)
            .expect("default keymap must not conflict");
    }

    for (prefix, command) in [
        (',', "inner-of-thing"),
        ('.', "bounds-of-thing"),
        ('[', "beginning-of-thing"),
        (']', "end-of-thing"),
    ] {
        for (ch, _) in THING_CHARS {
            map.bind(State::Normal, &[Key::Char(prefix), Key::Char(ch)], command)
                .expect("default keymap must not conflict");
        }
    }
    map
}

/// What the buffer holding a config's error is called.
pub const CONFIG_ERROR: &str = "*config-error*";

/// What the buffer holding the keymap is called.
pub const BINDINGS: &str = "*bindings*";

/// What the buffer holding a buffer's diagnostics is called.
///
/// **One buffer, regenerated per invocation**, matching the two above. One per
/// subject would accumulate buffers nobody closes, which the picker would then
/// list -- and the list of diagnostics you want is almost always the one for
/// the buffer you are in.
pub const DIAGNOSTICS: &str = "*diagnostics*";

/// The buffer that answers "what can I put in `init.scm`".
///
/// A second buffer rather than more of `*bindings*`, because they answer
/// different questions: that one is "what do my keys do", and a command with
/// no key at all is invisible to it and is the whole point of this one (#87).
pub const COMMANDS: &str = "*commands*";

/// Rows, with a run of keys on one command written as a range.
///
/// `bindings_in` orders by how the keys are written, so the nineteen Things
/// behind `,` are adjacent and so are the ten digits. Eighty-two of the Normal
/// rows are those two shapes, every one of them repeating a description that
/// does not say which Thing or which count — so listed in full they bury the
/// hundred-odd rows that do say something.
///
fn collapsed<'a>(bindings: &[(&'a [Key], &'a str)]) -> Vec<(String, &'a str)> {
    let mut rows = Vec::new();
    let mut at = 0;
    while at < bindings.len() {
        let (_, command) = bindings[at];
        let end = bindings[at..]
            .iter()
            .position(|(_, other)| *other != command)
            .map_or(bindings.len(), |offset| at + offset);
        let group = &bindings[at..end];
        match folded(group) {
            Some(keys) => rows.push((keys, command)),
            None => rows.extend(
                group
                    .iter()
                    .map(|(keys, command)| (Keys(keys).to_string(), *command)),
            ),
        }
        at = end;
    }
    rows
}

/// How a group of bindings on one command is written as a single row, when it
/// can be written as one without claiming something untrue.
///
/// Three, not two: a pair sharing a command is as likely to be two keys that
/// happen to do the same thing — `x` and `d` are both `delete` — as a run
/// worth folding.
///
/// And three of a kind is still not a range. The group has to differ in its
/// last key alone, and then either those last keys run consecutively (`0 … 9`)
/// or the shared prefix names them well enough on its own (`, …` for the
/// nineteen Things). Neither holds for three scattered keys a config put on
/// one command, and `! … %` would be saying that `"` and `$` do it too.
fn folded(group: &[(&[Key], &str)]) -> Option<String> {
    let (first, _) = *group.first().filter(|_| group.len() >= 3)?;
    let (last, _) = *group.last()?;
    let prefix = &first[..first.len() - 1];
    let same_shape = group
        .iter()
        .all(|(keys, _)| keys.len() == first.len() && &keys[..keys.len() - 1] == prefix);
    if !same_shape {
        return None;
    }
    let ends: Option<Vec<char>> = group
        .iter()
        .map(|(keys, _)| match keys[keys.len() - 1] {
            Key::Char(c) => Some(c),
            _ => None,
        })
        .collect();
    let consecutive = ends.is_some_and(|ends| {
        ends.windows(2)
            .all(|pair| pair[0] as u32 + 1 == pair[1] as u32)
    });
    if consecutive {
        return Some(format!("{} … {}", Keys(first), Keys(last)));
    }
    // A prefix of nothing names nothing: single keys that are not a run are
    // listed, however many of them share a command.
    (!prefix.is_empty()).then(|| format!("{} …", Keys(prefix)))
}

/// How wide the key column of the bindings buffer is.
///
/// Not shared with `--help`, which pads to the same number: that is a different
/// document with a handful of rows, and tying its layout to this one would make
/// every future column change in either a change to both.
const KEY_COLUMN: usize = 16;

/// A buffer meowr wrote rather than read.
///
/// Read-only by construction: nothing should be typed into one, and
/// `save_target` refuses read-only buffers, so it can never be written
/// anywhere either. The starred name is Emacs's mark for a buffer that is not
/// a file, and both of these are that.
fn generated(name: &str, text: &str) -> Buffer {
    let mut buffer = Buffer::scratch();
    buffer.label = Some(name.to_string());
    buffer.read_only = ReadOnly::Always;
    buffer.loaded(Document::from_text(text));
    buffer
}

/// What a State's section says besides its own bindings.
///
/// Three States have something to add and it is the same kind of thing each
/// time: the keymap is not the whole story there. Motion routes ahead of the
/// keymap in `on_key`, Keypad walks the leader tree, and Beacon resolves
/// against Normal when its own map has nothing — so a section showing only
/// what `bindings_in` returns would be describing a gap that is not there.
///
/// Appended whether or not the list is empty. Conditioning it on emptiness put
/// the answer to "why did that key do nothing" one `define-key` away from
/// vanishing, and a Motion binding is exactly what provokes the question.
fn footnote(state: State) -> Option<&'static str> {
    match state {
        // The glossary's words. Motion State is for "buffers that are not
        // for text entry, where single keys belong to the buffer's own
        // application". Nothing in meowr enters it since Guests were dropped:
        // it is Meow's State, kept because the State set is Meow's, and it
        // has no case of its own until something needs one.
        State::Motion => Some(
            "Single keys would belong to the buffer's own application. \
             Nothing enters this State today.",
        ),
        // A run of keys on one command folds into `, …`, which is what keeps
        // seventy-six Thing rows from burying the hundred that say something
        // — but the description it keeps cannot say *which* Thing, so the row
        // was already misleading before #87 asked it to say more. It names
        // where the answer is instead of trying to be the answer.
        State::Normal => Some(concat!(
            "A folded row like `, …` stands for one key per Thing. \
             `describe-commands` lists them by name, in ",
            "*commands*",
            "."
        )),
        State::Keypad => Some("Entered from the leader; its keys are the LEADER list below."),
        State::Beacon => Some("Normal's bindings apply too; these override them."),
        _ => None,
    }
}

impl Session {
    /// Say that the config did not run, and keep the detail where it can be read.
    ///
    /// One line in the Echo Area and the rest in a buffer, because the Echo
    /// Area is a single line by definition and ADR-0004 forbids anything that
    /// floats — so there is nowhere else for a backtrace to be but a Window,
    /// which means a Buffer.
    ///
    /// The buffer is **not** shown. A config error that seizes the window from
    /// the file you asked to open is a second insult after the first: you are
    /// told, and you choose. The echo line is the notification and the buffer
    /// is the record, which is why the line clearing on the next keypress
    /// costs nothing.
    pub fn config_failed(&mut self, summary: &str, detail: &str) {
        let showing = self.current_buffer_id();
        self.open(generated(CONFIG_ERROR, detail));
        self.show(showing);

        // Looked up, never written down. A binding named in prose is a second
        // source for the keymap's fact — and a config that rebound the picker
        // would otherwise be told to press a key it had just removed.
        let reach = self.reached_by("find-file");
        self.echo = Some(format!("{summary} — {reach} for {CONFIG_ERROR}"));
    }

    /// Show the keymap, in a Buffer.
    ///
    /// A Buffer because ADR-0004 forbids anything that floats, so generated
    /// text has nowhere else to be — the same reasoning, and the same shape,
    /// as `config_failed` above.
    fn describe_bindings(&mut self) -> Action {
        let text = self.bindings_text();
        self.show_generated(BINDINGS, &text, None);
        // Named rather than assumed, and looked up rather than written: the
        // report that produced `quit-window` was as much about nothing saying
        // which key as about the key not existing.
        if let Some(keys) = self.written("quit-window") {
            self.echo = Some(format!("{keys} to close."));
        }
        Action::Redraw
    }

    /// `describe-commands`: every name a config may use, and what it does.
    ///
    /// No key of its own. `M-x` (#91) is how you reach it, which is the same
    /// shape Emacs gives every `describe-` command, and a config may bind it.
    /// `SPC ?` is the sibling and keeps its key, because "what do my keys do"
    /// is the question you ask while your hands are on the keyboard.
    fn describe_commands(&mut self) -> Action {
        let text = self.commands_text();
        self.show_generated(COMMANDS, &text, None);
        Action::Redraw
    }

    /// A generated buffer, refreshed if it is already open.
    ///
    /// Refreshed rather than re-made. A name is what tells two buffers apart,
    /// so a second `*bindings*` is a different buffer wearing the same one —
    /// and the picker would then offer both with nothing to choose between
    /// them but which is stale.
    ///
    /// The [`Listing`] comes with the text rather than after it, so a buffer
    /// cannot be refreshed into rows that name where the last set of rows
    /// pointed. Two of the three callers have none, which is the ordinary
    /// case: a generated buffer is text unless something says otherwise.
    fn show_generated(&mut self, name: &str, text: &str, listing: Option<Listing>) {
        let existing = self
            .buffers
            .iter()
            .position(|buffer| !buffer.killed && buffer.name() == name)
            .map(BufferId);
        let id = match existing {
            Some(id) => {
                self.buffers[id.0].loaded(Document::from_text(text));
                self.show(id);
                id
            }
            None => self.open(generated(name, text)),
        };
        self.buffers[id.0].set_listing(listing);
    }

    /// Every registered name, what it does, and the keys that reach it.
    ///
    /// Sorted by name, because the name is what you are looking for here —
    /// `*bindings*` is ordered by key for the same reason in reverse. The keys
    /// come from [`Session::written`], so a command a config moved is written
    /// where the config put it rather than where meowr ships it.
    fn commands_text(&self) -> String {
        let mut names: Vec<&str> = self.registry.names().collect();
        names.sort_unstable();
        let width = names.iter().map(|name| name.len()).max().unwrap_or(0);
        let mut out = String::from("COMMANDS\n\n");
        for name in names {
            let description = self
                .registry
                .get(name)
                .map(|command| command.description())
                .unwrap_or_default();
            // A dash, not a blank: a command with no key is the case this
            // buffer exists for, and an empty column reads as a missing value
            // rather than an answer.
            let keys = self.written(name).unwrap_or_else(|| "—".into());
            out.push_str(&format!("  {name:width$}  {keys:12}  {description}\n"));
        }
        out
    }

    /// `+meow-escape`: cancel the selection, else close what meowr wrote,
    /// else abort.
    ///
    /// Doom's, with one step Doom does not need to spell. `+meow-escape` is
    /// "call `meow-cancel-selection', else fall back to `doom/escape'" — and
    /// `doom/escape` is itself incremental, running `doom-escape-hook`, where
    /// a popup closes, before it reaches `keyboard-quit`. meowr's popup is a
    /// Buffer, because ADR-0004 says nothing floats, so the hook's one job
    /// here is a job this can do directly.
    ///
    /// The middle step is why the key is worth reserving. Doom binds
    /// Motion-state `<escape>` for the reason it gives in as many words:
    /// "Popups will be in Motion state ... So we can't have <escape> bound to
    /// `ignore'." A key that says "Quit." and leaves you exactly where you
    /// were is not a way out.
    ///
    /// A generated buffer is the case that remains here and is
    /// treated alike: both are `ReadOnly::Always`, both hold text that is not
    /// yours, and both are things you are *in* rather than working on.
    fn escape(&mut self) -> Action {
        let buffer = self.current();
        let selected = buffer.selection.current().selection_type().is_some();
        if selected || !buffer.meowr_wrote() {
            // Through the name, so the fallback table answers the second half
            // rather than this repeating its answer: `cancel-selection` stands
            // aside for `keyboard-quit` where there is no selection, which is
            // `+meow-escape`'s "else" in machinery meowr already had. Spelling
            // `keyboard-quit` here would make `g` and `ESC` disagree the
            // moment a config retargets that entry.
            return self.dispatch_command("cancel-selection", Some(Key::Esc));
        }
        self.dispatch_command("quit-window", Some(Key::Esc))
    }

    /// Dismiss a buffer meowr wrote, the way `q` dismisses one in Emacs.
    ///
    /// Reported from use: `SPC ?` opened a buffer that `q` and `ESC` both left
    /// standing. `SPC b k` worked and nothing said so, which made the way out
    /// something to be told rather than something to reach for.
    ///
    /// Only a buffer meowr wrote — see [`Buffer::meowr_wrote`], which is what
    /// keeps a buffer somebody is editing from being killed by a help key.
    ///
    /// `ESC` reaches it too, deliberately, and #82 made that the way out.
    fn quit_window(&mut self) -> Action {
        if !self.current().meowr_wrote() {
            return Action::Nothing;
        }
        self.kill_current(false)
    }

    /// The keymap, written out.
    ///
    /// A section per State, then the leader tree.
    ///
    /// A State the keymap does not drive gets a line saying so rather than
    /// being left out. Omission and emptiness look identical from the reading
    /// side, and the difference is the whole answer for a reader asking why a
    /// key did nothing.
    fn bindings_text(&self) -> String {
        let mut out = String::new();
        for state in State::ALL {
            let bindings = self.keymap.bindings_in(*state);
            let note = footnote(*state);
            out.push_str(&format!("{}\n\n", state.name().to_uppercase()));
            if bindings.is_empty() && note.is_none() {
                out.push_str("  Nothing is bound here.\n");
            }
            for (keys, command) in collapsed(&bindings) {
                out.push_str(&self.described(&keys, command));
            }
            if let Some(note) = note {
                out.push_str(&format!("  {note}\n"));
            }
            out.push('\n');
        }
        out.push_str(&self.leader_text());
        out
    }

    /// Ring 2, written out under the key that opens it.
    ///
    /// Commands only. A prefix row would repeat what the full path beside it
    /// already says, and a Reserved key names a milestone rather than
    /// something to press — the leader menu is where both belong, because it
    /// is answering "what is under my finger right now" and this is not.
    ///
    /// The opening key is looked up rather than written as `SPC`: Ring 3 lets
    /// it move, and a buffer that exists to tell you what your keys do is the
    /// last place that should assume one.
    fn leader_text(&self) -> String {
        let mut out = String::from("LEADER\n\n");
        for binding in self.leader.bindings() {
            let leader::Entry::Command(command) = &binding.entry else {
                continue;
            };
            let Some(keys) = self.under_leader(&binding.keys) else {
                return String::new();
            };
            out.push_str(&self.described(&Keys(&keys).to_string(), command));
        }
        out
    }

    /// One row: the keys, then what the registry calls the command.
    ///
    /// The name itself when nothing is registered under it, which is not a
    /// hypothetical — a config binds a key and a later edit can take the
    /// command away. A row saying the name is more use than no row at all.
    fn described(&self, keys: &str, command: &str) -> String {
        let description = self
            .registry
            .get(command)
            .map(|found| found.description())
            .unwrap_or(command);
        format!("  {keys:<KEY_COLUMN$} {description}\n")
    }

    /// Say that the config was deliberately not run.
    ///
    /// Cheap, and it closes a real trap: start with `--no-config` to find out
    /// whether a bug is yours, get distracted, and spend ten minutes wondering
    /// why your keys are wrong. Unlike a config error this describes a
    /// completed action rather than a condition needing repair, so the Echo
    /// Area's ordinary clear-on-next-key is exactly right for it.
    pub fn config_skipped(&mut self) {
        self.echo = Some("Config skipped: started with --no-config.".into());
    }
}

/// What a language server is being asked for a list of.
///
/// A type rather than three `Action` variants: the three rows differ in the
/// question and in nothing else, and the host's half is one request either way.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Lookup {
    pub path: String,
    /// The byte offset the question is about.
    pub at: usize,
    pub kind: LookupKind,
}

/// Copying or moving a file, as the host is asked to do it (#178).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileRequest {
    /// The buffer's file, as the session spells it.
    pub from: String,
    /// Where it should go, as it was typed: it may name a directory, and its
    /// parents may not exist. Which of those it is takes the filesystem to
    /// answer, so the host answers it.
    pub to: String,
    pub work: FileWork,
    /// `SPC u` was pressed: write over what is there.
    pub force: bool,
}

/// A `SPC q F` part-way through: the saves are out with the host, and the
/// buffers are still to kill (#176).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct KillAll {
    /// `SPC u` was pressed: kill even what could not be saved.
    forced: bool,
}

/// Deleting a file, as the host is asked to do it (#179).
///
/// Only the path, because there is nothing else to decide: which file is this
/// buffer's, the go-ahead has already been given, and an unlink either happens
/// or says why it did not.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileDeletion {
    /// The buffer's file, as the session spells it.
    pub path: String,
}

/// Which of the two file rows this is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileWork {
    /// `SPC f C`: the copy is opened, and this buffer is left where it was.
    Copy,
    /// `SPC f m`: this buffer follows the file.
    Move,
}

impl FileWork {
    /// "copy" or "move": the verb the way past a refusal is named with.
    fn verb(self) -> &'static str {
        match self {
            FileWork::Copy => "copy",
            FileWork::Move => "move",
        }
    }

    /// "copied" or "moved", for a sentence about what did or did not happen.
    fn past(self) -> &'static str {
        match self {
            FileWork::Copy => "copied",
            FileWork::Move => "moved",
        }
    }

    /// The same, to begin a sentence with.
    fn past_capital(self) -> &'static str {
        match self {
            FileWork::Copy => "Copied",
            FileWork::Move => "Moved",
        }
    }

    /// What the read asks for.
    fn asking(self) -> &'static str {
        match self {
            FileWork::Copy => "Copy file to: ",
            FileWork::Move => "Move file to: ",
        }
    }

    /// The command, for the sentence that names the key it is on.
    fn command(self) -> &'static str {
        match self {
            FileWork::Copy => "copy-this-file",
            FileWork::Move => "move-this-file",
        }
    }

    /// What was abandoned, when the read is cancelled.
    fn abandoned(self) -> &'static str {
        match self {
            FileWork::Copy => "Nothing was copied.",
            FileWork::Move => "Nothing was moved.",
        }
    }
}

/// A file operation as it stands while its target is being read (#178).
///
/// The three travel together from the key that started the read to the answer
/// that ends it, which is what makes them a type rather than three parameters.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileAsk {
    pub work: FileWork,
    /// The path the buffer has now, which is also the read's seed.
    pub from: String,
    /// A prefix argument was given before the key: write over what is there.
    pub force: bool,
}

/// Where a file ended up, once the host had resolved the name that was typed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileDone {
    /// The path it now has, in the one spelling (#150) -- the basename added
    /// if a directory was named.
    pub path: String,
    /// Directories were created to put it there, which is worth saying.
    pub made_directories: bool,
}

/// Why a file was not copied or moved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileRefusal {
    /// Something is already there, and `SPC u` was not pressed. Carries the
    /// path in the way, which is not always the one that was typed: a
    /// directory was named and the file's own name added to it.
    Taken(String),
    /// The target resolved to the file itself.
    Itself,
    Failed(String),
}

/// Where a rename was asked about: the symbol at `at` in a buffer, as the
/// buffer stood (#168).
///
/// Stamped by buffer as well as revision, as [`Revisions`] is: a buffer killed
/// and reopened counts from the start again.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenameSite {
    pub path: String,
    pub buffer: BufferId,
    /// The byte offset the question is about.
    pub at: usize,
    pub revision: u64,
}

/// The rename itself, to `name`, stamped for the workspace edit that answers
/// it (ADR-0013).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenameApply {
    pub site: RenameSite,
    pub name: String,
    pub asked: Revisions,
}

/// A rename's question for a language server: one of its two.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RenameQuestion {
    /// `textDocument/prepareRename`: can the symbol be renamed, and what is it
    /// called. lsp-mode asks first too, so a name is not typed for a rename the
    /// server would refuse.
    Prepare(RenameSite),
    /// `textDocument/rename`.
    Apply(RenameApply),
}

impl RenameQuestion {
    pub fn site(&self) -> &RenameSite {
        match self {
            RenameQuestion::Prepare(site) => site,
            RenameQuestion::Apply(apply) => &apply.site,
        }
    }
}

/// What a server said to `textDocument/prepareRename`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Prepared {
    /// The symbol's name as the server would have it edited.
    Placeholder(String),
    /// Where the symbol is, its text the name.
    Range {
        start: meowr_domain::LineColumn,
        end: meowr_domain::LineColumn,
    },
    /// The server leaves it to the client: the symbol at the point. Also what
    /// a server that does not know the request is taken to mean.
    Default,
    /// `null`: nothing here can be renamed.
    Nothing,
    /// The server refused, in its words.
    Refused(String),
    /// The server does not rename at all, which it said when it started.
    Unsupported,
    /// No answer to be had, in meowr's words: the connection closed, or a
    /// reply that could not be read.
    Failed(String),
}

/// What a server answered a question whose answer is a workspace edit: a
/// rename (#168), or a code action resolved (#169).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EditReply {
    /// Its workspace edit, paths respelled and unopened files read.
    Edit(Vec<FileChange>),
    /// `null`.
    Nothing,
    /// The server refused, in its words.
    Refused(String),
    /// No answer to be had, in meowr's words.
    Failed(String),
}

/// A code action as a server offered it (#169). lsp-mode shows its title, and
/// so does the picker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodeAction {
    pub title: String,
    /// `quickfix`, `source.organizeImports` and the like, where the server
    /// said.
    pub kind: Option<String>,
    pub effect: CodeActionEffect,
    /// The command it also carries, which meowr does not run and so says it
    /// did not: lsp-mode runs one after the edit.
    pub command: Option<String>,
}

/// What choosing a code action does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CodeActionEffect {
    /// Its edit, which came with it.
    Edit(Vec<FileChange>),
    /// Nothing yet: `codeAction/resolve` returns the edit, asked with the
    /// action as the server sent it -- JSON text, which the session carries
    /// without reading (#157 measured rust-analyzer's actions arrive so).
    Resolve(String),
    /// Only a command for the server to run, which meowr does not do: no
    /// `workspace/applyEdit` is declared (#157).
    Command,
    /// No edit, and none to be had: the server cannot resolve it.
    Nothing,
}

/// Where code actions were asked for, and as what everything stood (#169).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodeActionsAsked {
    pub path: String,
    pub buffer: BufferId,
    /// The selection, or with none the line -- lsp-mode's `lsp--region-or-line`.
    pub range: Range<usize>,
    pub revision: u64,
    /// The diagnostics on the point's line, lsp-mode's
    /// `lsp-cur-line-diagnostics`, which a quick fix answers.
    pub diagnostics: Vec<meowr_domain::Diagnostic>,
    /// The one kind wanted: `source.organizeImports` for `SPC k o`.
    pub only: Option<String>,
    /// Stamped as the actions are asked for, because an action that comes with
    /// its edit computed it then (ADR-0013).
    pub asked: Revisions,
}

/// The code action chosen, and the question it answered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChosenCodeAction {
    pub asked: CodeActionsAsked,
    pub action: CodeAction,
}

/// A code action question for a language server: the list, then the one
/// chosen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CodeActionQuestion {
    /// `textDocument/codeAction`.
    List(CodeActionsAsked),
    /// Apply the chosen one: its edit read, or `codeAction/resolve` first.
    Apply(ChosenCodeAction),
}

impl CodeActionQuestion {
    /// Where the actions were asked for, whichever question this is.
    pub fn asked(&self) -> &CodeActionsAsked {
        match self {
            CodeActionQuestion::List(asked) => asked,
            CodeActionQuestion::Apply(chosen) => &chosen.asked,
        }
    }
}

/// What a server said to `textDocument/codeAction`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CodeActionsFound {
    Actions(Vec<CodeAction>),
    /// No `codeActionProvider`, said when it started.
    Unsupported,
    /// The server refused, in its words.
    Refused(String),
    /// No answer to be had, in meowr's words.
    Failed(String),
}

/// What is to be searched for, and where.
///
/// A type rather than four `Action` variants: the rows differ in the subject
/// and in nothing else, and the host's half is one `rg` either way -- which is
/// exactly the argument [`Lookup`] above makes about its three.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Search {
    /// What is being looked for, and how it is to be read. `SPC s .` means the
    /// symbol as text; every other row means what the user typed, as a regex.
    pub pattern: Pattern,
    pub subject: Subject,
}

/// Where a search looks.
///
/// Named rather than resolved, because resolving is the host's: `Project` is a
/// `git rev-parse` away and ADR-0010 puts both that question and its refusal on
/// the side that knows where it looked. The session's half is which of the
/// three a row means, which is a fact about the row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Subject {
    /// The Project the named file is in, or a refusal if it is in none.
    Project(String),
    /// The directory the named file is in. No Project, so no refusal: a path
    /// has a parent whether or not anything has ever been committed.
    Here(String),
    /// A directory the user named.
    Directory(String),
}

/// Which list a language server is being asked for.
///
/// `References` is about a *place* -- the symbol under the point -- and
/// `Symbols` is about a *string*, which is why the query lives in the variant
/// rather than beside it: a `Lookup` with a query that means nothing for one of
/// the two is a field every reader has to learn the exception for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LookupKind {
    /// `textDocument/references`: where the symbol at the point is used.
    References,
    /// `textDocument/definition`: where the symbol at the point is defined.
    Definition,
    /// `textDocument/typeDefinition`: where its type is defined.
    TypeDefinition,
    /// `textDocument/implementation`: what implements it.
    Implementations,
    /// `workspace/symbol`, asked **with** a query and asked again as it
    /// changes.
    ///
    /// #137 built these two rows on an empty query and handed them back:
    /// rust-analyzer answers one with a truncated, alphabetically-first list,
    /// so a symbol picker was missing its own workspace's symbols. The server
    /// is the matcher here, which is the whole reason the picker has to re-ask
    /// rather than filter (#144).
    Symbols {
        query: String,
        /// `SPC k J` rather than `k j`: Doom passes `all-workspaces`, and meowr
        /// runs one server per Language, so a workspace is a server and "any"
        /// is all of them.
        everywhere: bool,
    },
}
