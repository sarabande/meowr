//! The command registry and keymap for meowr. The seam plugins extend.
//!
//! This crate exists because the extension system, not the package ecosystem,
//! is what makes meowr an Emacs-class environment rather than another Helix.
//! Steel arrives in M2, but the shape it plugs into is fixed here: commands are
//! addressed **by name**, carry metadata a menu can read, and receive a single
//! context. Adding Steel should add a [`Handler`] variant, not change every
//! command.
//!
//! # What is deliberately not extensible
//!
//! An extension system that extends everything has not made a decision. These
//! are fixed, and each is fixed for a reason:
//!
//! - **The set of [`State`]s.** Meow in Emacs lets a user define a sixth state,
//!   and the author's own Doom configuration used that to add an "Emacs state".
//!   Here the five are closed, because ADR-0005 settled that a Guest receives
//!   the keyboard through Motion State rather than through a bespoke state, and
//!   an open set would let a plugin reintroduce exactly the arbitration that
//!   decision removed.
//! - **The fields of [`Ctx`].** A command sees the document, the point, the
//!   state, the count and the triggering key: no windows, no filesystem, no
//!   process control. Widening this is a decision about what a plugin may
//!   touch, and it should be made deliberately rather than by a plugin
//!   reaching further than the last one did.
//! - **[`Outcome`].** A command reports that it handled the key or that the
//!   editor should quit. A plugin cannot invent a third thing for the event
//!   loop to do.
//! - **[`Key`].** The vocabulary of keys is the terminal's, not a plugin's.
//! - **The ambiguity rule in [`Keymap::bind`].** No key may be both a command
//!   and a prefix, for plugins as much as for the defaults. The alternative is
//!   deciding, on every keystroke, whether to act or to wait.
//!
//! What *is* open: the set of commands, their names, metadata and handlers, and
//! every key sequence bound to them.

use meowr_core::{SelectionState, edit::KillRing, thing::Things, undo::History};

/// Re-exported because [`Options::expand_axis`] is one, and a public field
/// whose type cannot be named from outside is a field nobody outside can set.
pub use meowr_core::Axis;
use meowr_domain::Document;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::time::Duration;

/// A named mode determining how a keypress is interpreted.
///
/// The five Meow states. `Insert` is the only one this slice populates.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum State {
    Normal,
    Insert,
    Motion,
    Keypad,
    Beacon,
}

impl State {
    /// Every State, in the order the editor moves through them.
    pub const ALL: &'static [State] = &[
        State::Normal,
        State::Insert,
        State::Motion,
        State::Keypad,
        State::Beacon,
    ];

    /// The State's name, in one place.
    ///
    /// The modeline writes it, the bindings buffer heads a section with it and
    /// a config's `define-key` is parsed against it. Three copies of one match
    /// is three chances to disagree, and the config's copy is the one a reader
    /// would never think to check.
    pub fn name(self) -> &'static str {
        match self {
            State::Normal => "normal",
            State::Insert => "insert",
            State::Motion => "motion",
            State::Keypad => "keypad",
            State::Beacon => "beacon",
        }
    }
}

/// A key as meowr understands it, independent of any terminal library.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Key {
    Char(char),
    Ctrl(char),
    /// Emacs's `M-`. Crossterm delivers Alt as a modifier rather than as a key
    /// of its own, so without a variant here it flattens onto `Char` and any
    /// `M-` binding becomes unreachable.
    Alt(char),
    Esc,
    /// Return. Not `Char('\n')`: a key and the character it usually inserts
    /// are different things, and conflating them means Return cannot be bound
    /// to anything but self-insertion.
    Enter,
    Backspace,
    /// The navigation keys, which meowr's own bindings do not use — Meow is
    /// modal and reaches everything from the home row — and which a **Guest**
    /// does. `wagit` binds all six, and until #71 they were dropped in the
    /// binary's `to_key` and never reached a session at all.
    ///
    /// Separate variants rather than `Char('\t')` for Tab, for the reason
    /// [`Key::Enter`] already gives: a key and the character it usually
    /// inserts are different things.
    Tab,
    BackTab,
    Up,
    Down,
    Left,
    Right,
    PageUp,
    PageDown,
}

/// A key sequence, written the way a keymap reference writes it.
pub struct Keys<'a>(pub &'a [Key]);

/// Why a key sequence could not be read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyParseError(String);

impl std::fmt::Display for KeyParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for KeyParseError {}

/// Read a key sequence written the way [`Keys`] writes one.
///
/// The exact inverse of that `Display`, and a test holds the two together over
/// every variant. One spelling is the point: what the leader menu shows, what
/// the Echo Area shows and what a config file says are the same string, so a
/// key can be copied off the screen into `init.scm`.
///
/// Keys are separated by spaces, which is unambiguous because a space *key* is
/// written `SPC` and never appears bare.
pub fn parse_keys(written: &str) -> Result<Vec<Key>, KeyParseError> {
    let keys = written
        .split_whitespace()
        .map(parse_key)
        .collect::<Result<Vec<Key>, KeyParseError>>()?;
    if keys.is_empty() {
        return Err(KeyParseError(
            "a key sequence needs at least one key".into(),
        ));
    }
    Ok(keys)
}

fn parse_key(token: &str) -> Result<Key, KeyParseError> {
    // The named keys first. A bare `C` or `M` is a character, not a modifier
    // with nothing after it, and the prefixes below are checked with their
    // hyphen for exactly that reason.
    match token {
        "SPC" => return Ok(Key::Char(' ')),
        "ESC" => return Ok(Key::Esc),
        "RET" => return Ok(Key::Enter),
        "DEL" => return Ok(Key::Backspace),
        "TAB" => return Ok(Key::Tab),
        "<backtab>" => return Ok(Key::BackTab),
        "<up>" => return Ok(Key::Up),
        "<down>" => return Ok(Key::Down),
        "<left>" => return Ok(Key::Left),
        "<right>" => return Ok(Key::Right),
        "<prior>" => return Ok(Key::PageUp),
        "<next>" => return Ok(Key::PageDown),
        _ => {}
    }
    // Recursive rather than character-slicing, so `C-SPC` works and `C--` —
    // control-hyphen — does too. A modifier may only wrap a character: there
    // is no `Key` for a modified Return, so `C-RET` is refused rather than
    // silently becoming something else.
    for (prefix, wrap) in [
        ("C-", Key::Ctrl as fn(char) -> Key),
        ("M-", Key::Alt as fn(char) -> Key),
    ] {
        if let Some(rest) = token.strip_prefix(prefix) {
            return match parse_key(rest)? {
                Key::Char(c) => Ok(wrap(c)),
                _ => Err(KeyParseError(format!(
                    "`{token}` modifies a key that cannot be modified"
                ))),
            };
        }
    }
    let mut chars = token.chars();
    match (chars.next(), chars.next()) {
        (Some(c), None) => Ok(Key::Char(c)),
        _ => Err(KeyParseError(format!("`{token}` is not a key"))),
    }
}

impl std::fmt::Display for Keys<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for (at, key) in self.0.iter().enumerate() {
            if at > 0 {
                f.write_str(" ")?;
            }
            write!(f, "{key}")?;
        }
        Ok(())
    }
}

impl std::fmt::Display for Key {
    /// As Emacs writes a key: `C-x`, `SPC`, `RET`, `ESC`, or the character.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Key::Char(' ') => f.write_str("SPC"),
            Key::Char(c) => write!(f, "{c}"),
            // `C-SPC` rather than `C- `, which is how Emacs writes
            // `set-mark-command` and, less prettily, the only spelling that
            // survives being split back apart on whitespace.
            Key::Ctrl(' ') => f.write_str("C-SPC"),
            Key::Alt(' ') => f.write_str("M-SPC"),
            Key::Ctrl(c) => write!(f, "C-{c}"),
            Key::Alt(c) => write!(f, "M-{c}"),
            Key::Esc => f.write_str("ESC"),
            Key::Enter => f.write_str("RET"),
            Key::Backspace => f.write_str("DEL"),
            // Emacs's `kbd` names. `TAB` is bare like the four above it;
            // the rest take the angle brackets `kbd` uses for keys that are
            // not characters. `<prior>` and `<next>` read oddly and are what
            // Emacs calls PageUp and PageDown — inventing `<pageup>` would
            // give a reader who already has one vocabulary a second.
            Key::Tab => f.write_str("TAB"),
            Key::BackTab => f.write_str("<backtab>"),
            Key::Up => f.write_str("<up>"),
            Key::Down => f.write_str("<down>"),
            Key::Left => f.write_str("<left>"),
            Key::Right => f.write_str("<right>"),
            Key::PageUp => f.write_str("<prior>"),
            Key::PageDown => f.write_str("<next>"),
        }
    }
}

/// What a command did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Handled,
    /// The command could not do its job.
    ///
    /// Emacs signals here and its command loop beeps: `backward-char` at the
    /// beginning of the buffer raises `beginning-of-buffer`, and a selection
    /// command with no selection and no fallback raises "No selection". The
    /// buffer state after a signal is often identical to the state after a
    /// command that ran and chose to do nothing, so this is not cosmetic —
    /// the differential harness compares it, because a golden that could not
    /// tell those apart is how a wrong expectation gets believed.
    Failed,
    /// The command could not do its job, and can say why.
    ///
    /// [`Outcome::Failed`] with a sentence. Emacs's `user-error` takes a
    /// message and its command loop prints it; `Failed` models the bell and
    /// dropped the words, which left every registry command able to beep and
    /// unable to explain — the gap #80 found from the outside.
    ///
    /// A separate variant rather than a field on `Failed` so that the failures
    /// with nothing useful to say stay as cheap as they were, and so the
    /// differential harness keeps comparing the same signal count for both.
    Refused(&'static str),
    /// The command needs the host to write the buffer out.
    ///
    /// Commands cannot do I/O — that is the whole point of the layering — so
    /// writing a file is a *request*, and the binary that owns the filesystem
    /// carries it out. One variant for one command today; if this grows past
    /// two or three it wants to become a request type rather than more
    /// variants.
    Save,
}

/// A numeric argument in the making.
///
/// Emacs distinguishes "no argument", "`-` was typed but no digits yet", and
/// "a number", because a digit after `-` gives `-d` rather than `-1d`. Three
/// states, so three variants — collapsing `Negative` into `Count(-1)` makes
/// `- 2` mean −12.
///
/// The fourth is the universal argument, which is not a number even though it
/// has one. Emacs's raw prefix argument for `C-u` is the *list* `(4)`, and
/// `C-u C-u` is `(16)`; commands that want a count call
/// `prefix-numeric-value` and get 4 and 16, while commands that want to know
/// whether `C-u` was pressed compare against `'(4)` exactly. Both readings are
/// live in Meow — `meow-block` takes the count, `meow-next` takes the
/// comparison — so this keeps the raw form and offers both.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Argument {
    #[default]
    None,
    Negative,
    Count(i32),
    /// `C-u`, and `C-u C-u`, and so on. The field is how many were pressed,
    /// because `meow--with-universal-argument-p` is `(equal '(4) arg)` —
    /// exactly one. Two `C-u` is not "more universal", it is a different
    /// argument, and the commands that branch on it fall through to the count.
    Universal(u32),
}

impl Argument {
    /// What a command receives, as `(interactive "p")` would see it.
    pub fn value(self) -> Option<i32> {
        match self {
            Argument::None => None,
            Argument::Negative => Some(-1),
            Argument::Count(n) => Some(n),
            // `prefix-numeric-value` of `(4)`, `(16)`, `(64)`: four to the
            // power of however many times `C-u` was pressed. Saturating for
            // the same reason `digit` is — the key repeats, and sixteen of
            // them would overflow.
            Argument::Universal(times) => Some(4i32.checked_pow(times).unwrap_or(i32::MAX)),
        }
    }

    /// `meow--with-universal-argument-p`: `(equal '(4) arg)`, one `C-u` and no
    /// digits after it.
    pub fn is_universal(self) -> bool {
        self == Argument::Universal(1)
    }

    /// The `(interactive "p")` reading: no argument is 1, a bare `-` is -1.
    pub fn count(self) -> i32 {
        self.value().unwrap_or(1)
    }

    /// The `(< n 0)` reading, which is all `meow-mark-thing` takes: a
    /// direction, with the magnitude discarded.
    pub fn is_negative(self) -> bool {
        self.value().is_some_and(|n| n < 0)
    }

    /// Fold in a digit, as `digit-argument` does.
    ///
    /// Saturating, because the digit keys repeat: holding one down would
    /// otherwise overflow and panic a debug build after ten characters.
    pub fn digit(self, d: i32) -> Self {
        Argument::Count(match self {
            Argument::None => d,
            Argument::Negative => -d,
            Argument::Count(n) if n < 0 => n.saturating_mul(10).saturating_sub(d),
            Argument::Count(n) => n.saturating_mul(10).saturating_add(d),
            // `C-u 5` is five, not fifty-four: the digit replaces the
            // universal argument rather than extending it.
            Argument::Universal(_) => d,
        })
    }

    /// Flip the sign, as `negative-argument` does.
    pub fn negate(self) -> Self {
        match self {
            Argument::None => Argument::Negative,
            Argument::Negative => Argument::None,
            Argument::Count(n) => Argument::Count(n.saturating_neg()),
            // `C-u -` is a bare minus, the same as `-` alone.
            Argument::Universal(_) => Argument::Negative,
        }
    }

    /// Press `C-u`, as `universal-argument` and `universal-argument-more` do.
    ///
    /// A second `C-u` multiplies by four, which is why the count is kept
    /// rather than a bare flag. Anything already typed is replaced: `C-u`
    /// after a digit starts the argument again, as `universal-argument` does
    /// by setting `prefix-arg` outright.
    pub fn universal(self) -> Self {
        match self {
            Argument::Universal(times) => Argument::Universal(times.saturating_add(1)),
            _ => Argument::Universal(1),
        }
    }
}

/// The Options a command may consult.
///
/// One field on [`Ctx`] rather than one field per Option. `Ctx`'s shape is
/// part of the extension contract, so growing it every time something becomes
/// configurable would make it the dumping ground it exists to avoid — and
/// every added field is a change a Plugin written against the old shape has to
/// survive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Options {
    /// `meow-expand-selection-type`. Meow defaults to `select`, so expanding
    /// with a digit yields a transient selection that a further motion
    /// replaces rather than grows.
    pub expand_axis: Axis,
    /// `meow-expand-hint-remove-delay`. Meow's default is one second; the
    /// reference configuration sets four, on the grounds that the hint is
    /// something you read rather than something that flickers.
    pub expand_hint_delay: Duration,
    /// How long the selection must sit still before the window behind scrolls
    /// to it. `consult-preview-delay`; the reference configuration uses 0.4s
    /// for the expensive sources, and previewing on every keystroke of a fuzzy
    /// search would be unreadable.
    pub preview_delay: Duration,
    /// How long `SPC` sits before the leader menu appears. Which-key's
    /// default, confirmed on disk: someone typing `SPC w s` fluently should
    /// never see it.
    pub leader_delay: Duration,
    /// `which-key-idle-secondary-delay`: how long a *subsequent* leader key
    /// waits, once the menu has already been seen this walk.
    ///
    /// Doom sets 0.1 against the first delay's 1.0, and the difference is the
    /// whole feel of the leader: the first menu waits, so that typing `SPC w s`
    /// fluently never flashes one, and every level after it is near-instant,
    /// because by then you are reading rather than typing.
    ///
    /// meowr charged the full first delay at every level until this existed,
    /// which made going one prefix deep feel ten times slower than Emacs.
    pub leader_secondary_delay: Duration,
    /// `auto-save-default`, which the author's Doom config sets. On, because
    /// defaulting it off would silently withdraw the net from everyone who
    /// never writes a config.
    ///
    /// Read where the work is taken rather than latched at startup, so
    /// turning it back on resumes rather than waiting for the next process.
    pub auto_save: bool,
    /// `hl-line`: whether the line the point is on is highlighted.
    ///
    /// On, which is Doom's default — `global-hl-line-mode` goes on
    /// `doom-first-input-hook`. An Option rather than Doom's
    /// `global-hl-line-modes` whitelist, because that list is a set of major
    /// modes and meowr has none: a Buffer here holds text or it belongs to a
    /// Guest, and a Guest draws itself.
    pub highlight_line: bool,
    /// `auto-save-interval`: how many edits may pass before a buffer is
    /// auto-saved. Emacs counts 300 *characters*; this counts *commands* that
    /// changed the buffer, and a command can change more than one — so the
    /// same number would be a much longer leash, and 64 is the shorter one it
    /// should be.
    pub auto_save_interval: usize,
    /// `auto-save-timeout`: how long a buffer sits untouched before it is
    /// written out regardless of how few edits it has had.
    ///
    /// Emacs's default is thirty seconds. The moment you stop typing is the
    /// moment you walk away from the machine, which is the moment the net has
    /// to be up. The host arms the timer; the Option says how long.
    pub auto_save_timeout: Duration,
    /// `lsp-ui-doc-show-with-cursor`: whether documentation appears when the
    /// point rests on a symbol, with no key pressed.
    ///
    /// **Off**, which is `lsp-ui`'s own default and one Doom does not change.
    /// The reference configuration sets it explicitly (`config-ui.el:17`), so
    /// it ports as a line of `init.scm` rather than as a default here --
    /// exactly the rule `auto-save` follows to the opposite answer, because
    /// Doom turns that one on for everybody and nobody has to ask.
    ///
    /// It is also a different editor either way: one that asks a language
    /// server a question every time the cursor pauses is not the one that
    /// waits to be asked, and that is a choice rather than a detail.
    pub hover_on_rest: bool,
    /// `lsp-ui-doc-delay`: how long the point must rest before the question is
    /// asked. `lsp-ui`'s default, 0.2s, which is short because the answer is
    /// the thing being waited for rather than a menu appearing over the text.
    ///
    /// Debounced the way `preview_delay` is -- the host re-arms the timer on
    /// every event, so this is "no key for this long" rather than a stopwatch
    /// on the selection.
    pub hover_delay: Duration,
    /// `display-line-numbers-type`. On, which is Doom's default: its core
    /// turns `display-line-numbers-mode` on for `prog-mode`, `text-mode` and
    /// `conf-mode` unconditionally — not a module, and not something the
    /// reference configuration's own `(setq display-line-numbers-type t)`
    /// (`config.el:44`) switches on. Emacs's own default for the *type*, once
    /// the mode is on, is already `t`; the reference restates rather than
    /// changes it. `highlight_line`'s situation exactly, and the same reason
    /// this is `true` rather than `false`.
    ///
    /// One bool rather than Emacs's four-way `t` / `relative` / `visual` /
    /// `nil`: the reference never sets anything but `t`, and `relative` is a
    /// motion aid meowr's Things already are.
    pub line_numbers: bool,
    /// `consult-async-input-debounce`: how long the input sits still before a
    /// re-asking picker puts its question again.
    ///
    /// consult's own 0.2s. A picker whose source is a *question* rather than a
    /// list asks once per keystroke without this, and a language server is not
    /// a thing to ask five times a second.
    ///
    /// Debounced the way `hover_delay` and `preview_delay` are: the host
    /// re-arms on every change, so it is "no typing for this long" rather than
    /// a stopwatch on the picker.
    pub ask_debounce: Duration,
    /// `consult-async-min-input`: how many characters before asking at all.
    ///
    /// consult's own 3. The first two characters of a symbol name match most
    /// of a workspace, so the answer would be truncated where it is longest
    /// and least useful -- which is the failure `workspace/symbol` with an
    /// empty query already demonstrated, one keystroke further along.
    pub ask_min_input: usize,
    /// Whether a picker is scoped to the **Project** the buffer is in.
    ///
    /// **On.** ADR-0010 makes *what* a Project is a constant with a reason
    /// rather than an Option, and leaves this: `projectile-mode` is a global
    /// minor mode and consult documents `consult-project-function` set to
    /// `nil` as "no project support", so Doom has a variable for the switch
    /// even though it has none the author configures for the rule.
    ///
    /// Off, a picker looks in the buffer's own directory instead of refusing
    /// outside a repository -- which is what consult's `nil` does, and which
    /// makes this the pre-Project editor rather than a crippled one. The
    /// difference from the fallback #120 argued against is that a fallback
    /// chosen in an `init.scm` is the editor being told, not guessing.
    pub project_scope: bool,
    /// Whether a save formats the buffer, apheleia's way: write, format, write
    /// again (ADR-0014).
    ///
    /// **Off.** Doom formats on save only under `(format +onsave)`, which is a
    /// flag the author chose rather than Doom's default -- the rule that made
    /// `hover-on-rest` default off -- so the author's `init.scm` turns it on.
    pub format_on_save: bool,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            expand_axis: Axis::Select,
            highlight_line: true,
            expand_hint_delay: Duration::from_secs(1),
            preview_delay: Duration::from_millis(400),
            leader_delay: Duration::from_secs(1),
            leader_secondary_delay: Duration::from_millis(100),
            auto_save: true,
            auto_save_interval: 64,
            auto_save_timeout: Duration::from_secs(30),
            line_numbers: true,
            hover_on_rest: false,
            hover_delay: Duration::from_millis(200),
            project_scope: true,
            format_on_save: false,
            ask_debounce: Duration::from_millis(200),
            ask_min_input: 3,
        }
    }
}

/// A scalar an Option can be set to.
///
/// Three shapes rather than seven, because a `SteelVal` can be checked for
/// what it *is* without knowing which Option it was aimed at — and which
/// Option it was aimed at is not knowable until the name has been checked.
/// So the bridge narrows a value to one of these, and [`Options::set`] is the
/// one place that knows a duration from a count.
///
/// No string. #50 kept them out deliberately: the day a path or a program
/// name needs configuring is also the day someone has to decide about `~`
/// expansion and shell quoting.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OptionValue {
    Bool(bool),
    Int(i64),
    /// An enum member, as #48 chose — symbols for anything meowr looks up.
    Symbol(String),
}

impl std::fmt::Display for OptionValue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            OptionValue::Bool(true) => f.write_str("#t"),
            OptionValue::Bool(false) => f.write_str("#f"),
            OptionValue::Int(n) => write!(f, "{n}"),
            OptionValue::Symbol(name) => write!(f, "'{name}"),
        }
    }
}

impl Options {
    /// Every Option's written name, in the order they are listed back.
    ///
    /// Kebab-case of the Rust field, which is one source rather than two. The
    /// docstrings say which Emacs variable each came from, so the lineage is
    /// discoverable without being load-bearing — `'meow-expand-selection-type`
    /// in an `init.scm` would promise Emacs semantics meowr does not have.
    /// Every Option's written name, for the error a misspelling raises.
    ///
    /// Gains an entry whenever [`Options::set`] gains an arm, and the two are
    /// not checked against each other: the test over this list walks the list,
    /// so a name that was never added is a name it cannot look for.
    /// `highlight-line` was settable and unlisted for exactly that reason.
    pub const NAMES: &'static [&'static str] = &[
        "ask-debounce",
        "ask-min-input",
        "auto-save",
        "auto-save-interval",
        "auto-save-timeout",
        "expand-axis",
        "expand-hint-delay",
        "format-on-save",
        "highlight-line",
        "hover-delay",
        "hover-on-rest",
        "leader-delay",
        "leader-secondary-delay",
        "line-numbers",
        "preview-delay",
        "project-scope",
    ];

    /// Set one Option by its written name, or say why not.
    ///
    /// A closed, typed set rather than a `HashMap<String, Value>`, which is
    /// what makes `(set-option 'preview-dealy 400)` an error at load rather
    /// than a key ignored for ever. A Plugin cannot invent an Option of its
    /// own, and ADR-0003 does not scope it to: it configures meowr, it does
    /// not define what configuration means.
    pub fn set(&mut self, name: &str, value: OptionValue) -> Result<(), String> {
        match name {
            "auto-save" => self.auto_save = boolean(name, value)?,
            "highlight-line" => self.highlight_line = boolean(name, value)?,
            "hover-on-rest" => self.hover_on_rest = boolean(name, value)?,
            "line-numbers" => self.line_numbers = boolean(name, value)?,
            "project-scope" => self.project_scope = boolean(name, value)?,
            "format-on-save" => self.format_on_save = boolean(name, value)?,
            "auto-save-interval" => self.auto_save_interval = count(name, value)?,
            "auto-save-timeout" => self.auto_save_timeout = millis(name, value)?,
            "expand-axis" => self.expand_axis = axis(name, value)?,
            "expand-hint-delay" => self.expand_hint_delay = millis(name, value)?,
            "hover-delay" => self.hover_delay = millis(name, value)?,
            "leader-delay" => self.leader_delay = millis(name, value)?,
            "leader-secondary-delay" => self.leader_secondary_delay = millis(name, value)?,
            "preview-delay" => self.preview_delay = millis(name, value)?,
            "ask-debounce" => self.ask_debounce = millis(name, value)?,
            "ask-min-input" => self.ask_min_input = count(name, value)?,
            unknown => {
                return Err(format!(
                    "`{unknown}` is not an option; meowr has {}",
                    listed(Options::NAMES)
                ));
            }
        }
        Ok(())
    }
}

/// `a, b and c` — the way the State list already reads, rather than a bare
/// comma-separated join that stops looking like a sentence at about four.
///
/// Public because three crates now list names back at a config that got one
/// wrong, and a message that reads like a sentence in one place and like a
/// CSV row in the next is the drift this exists to stop.
pub fn listed(names: &[&str]) -> String {
    match names.split_last() {
        Some((last, [])) => (*last).to_string(),
        Some((last, rest)) => format!("{} and {last}", rest.join(", ")),
        None => String::new(),
    }
}

fn boolean(name: &str, value: OptionValue) -> Result<bool, String> {
    match value {
        OptionValue::Bool(on) => Ok(on),
        other => Err(format!(
            "`{name}` is on or off, so it takes #t or #f, got {other}"
        )),
    }
}

fn whole(name: &str, value: OptionValue, unit: &str) -> Result<u64, String> {
    match value {
        OptionValue::Int(n) if n < 0 => Err(format!("`{name}` cannot be negative, and {n} is")),
        OptionValue::Int(n) => Ok(n as u64),
        other => Err(format!("`{name}` is {unit}, got {other}")),
    }
}

fn count(name: &str, value: OptionValue) -> Result<usize, String> {
    whole(name, value, "a count of edits").map(|n| n as usize)
}

fn millis(name: &str, value: OptionValue) -> Result<Duration, String> {
    whole(name, value, "a duration in milliseconds").map(Duration::from_millis)
}

fn axis(name: &str, value: OptionValue) -> Result<Axis, String> {
    let OptionValue::Symbol(member) = &value else {
        return Err(format!(
            "`{name}` is `select` or `expand`, written as a symbol, got {value}"
        ));
    };
    match member.as_str() {
        "select" => Ok(Axis::Select),
        "expand" => Ok(Axis::Expand),
        other => Err(format!(
            "`{other}` is not an expand-axis; meowr has select and expand"
        )),
    }
}

/// Everything a command may touch.
///
/// One context rather than per-capability contexts, so that a scripting
/// binding crosses this boundary once rather than once per command shape.
pub struct Ctx<'a> {
    pub doc: &'a mut Document,
    /// The typed selection, whose `pos` is the point.
    pub selection: &'a mut SelectionState,
    pub state: &'a mut State,
    /// The numeric argument this command was given — Emacs's
    /// `current-prefix-arg`. Read-only: it is already decided.
    ///
    /// The **raw** argument, not an integer. Emacs's `current-prefix-arg` can
    /// be the symbol `-`, and `digit-argument` reads it to know that a digit
    /// after a bare minus means `-d` rather than `-1d`. Collapsing it to
    /// `Option<i32>` here made `- 3` come out as −13; call
    /// [`Argument::value`] for the `(interactive "p")` reading.
    pub argument: Argument,
    /// The numeric argument the *next* command will be given — Emacs's
    /// `prefix-arg`. Two fields rather than one because Emacs has two
    /// variables, for a real reason: `digit-argument` writes the one the next
    /// command reads, and must be able to see its own accumulated value
    /// without disturbing what it was itself called with.
    pub next_argument: &'a mut Argument,
    /// What the user has configured. See [`Options`].
    pub options: &'a Options,
    /// Emacs's kill ring, which is not the system clipboard. See [`KillRing`].
    pub kill_ring: &'a mut KillRing,
    /// The undo log. Every buffer mutation goes through it, so that "undoable"
    /// is a property of the only way to edit rather than a discipline each
    /// command has to remember.
    ///
    /// Lives beside `doc` and `selection` here, and moves onto the buffer with
    /// them when there is more than one — decision #5 already says per-buffer
    /// state belongs on the buffer.
    pub history: &'a mut History,
    /// The Thing registry — what `,` `.` `[` `]` can name. Open by
    /// construction, like the command registry and [`meowr_core::Kind`].
    pub things: &'a Things,
    /// The tree-sitter Things, when the buffer has a grammar. `None` is the
    /// ordinary case for plain text, and the commands treat a Thing they
    /// cannot resolve as simply not found.
    pub text_objects: Option<meowr_syntax::TextObjects<'a>>,
    /// The visible region, for the `window` Thing.
    ///
    /// A plain range rather than the viewport it came from: `window` is the
    /// one Thing that is a property of what is on screen instead of what is in
    /// the buffer, and this is the whole of what a command needs to know about
    /// that. Passing the viewport would put the view layer in this crate's
    /// dependencies to answer one question.
    pub window: Option<std::ops::Range<usize>>,
    /// The name of the command that ran before this one — Emacs's
    /// `last-command`, which is a real input to behaviour rather than
    /// bookkeeping: `yank-pop` is only meaningful straight after a yank, and
    /// `line-move` consults it for the goal column.
    pub last_command: Option<&'a str>,
    /// The key that triggered this command, when a key did.
    ///
    /// Present so that self-insertion is an ordinary command rather than a
    /// special case the session handles outside the registry — which would be
    /// a hole a plugin could not reach into.
    pub key: Option<Key>,
}

/// A point at which a hook may run.
///
/// A short, named set rather than a general interception mechanism, per
/// decision #7. Wrapping arbitrary commands is what re-registration already
/// gives; what this adds is the ability to observe *when* something happened
/// from outside the command that did it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum HookPoint {
    /// Before a command runs. A hook may refuse it.
    BeforeCommand,
    AfterCommand,
    /// After the State changed.
    StateChange,
    BeforeSave,
    AfterSave,
}

impl HookPoint {
    /// Every hook point, in the order they are listed back.
    pub const ALL: &'static [HookPoint] = &[
        HookPoint::BeforeCommand,
        HookPoint::AfterCommand,
        HookPoint::StateChange,
        HookPoint::BeforeSave,
        HookPoint::AfterSave,
    ];

    /// The hook points a *config* may attach a command to (#64).
    ///
    /// Two of the five. These reach the hooks through `Session::run_hooks`,
    /// which builds its own `Ctx` and drops it — so the name can be dispatched
    /// the moment the closures return, and "after" means what it says.
    ///
    /// The other three fire inside the `&mut` borrow `dispatch_command` holds
    /// over the buffer. Dispatching from there needs that borrow torn down and
    /// rebuilt, and for `before-command` the result would not be "before" in
    /// any sense the name claims. That one is worse than awkward: the hook point
    /// exists so a hook can *refuse*, by answering [`Proceed::No`], and a
    /// composed command answers [`Outcome`] and cannot refuse anything — so
    /// accepting it would offer the one thing the hook point is for and never
    /// deliver it.
    ///
    /// Native Rust hooks still take all five. This is about what a config may
    /// name.
    pub const FOR_COMMANDS: &'static [HookPoint] = &[HookPoint::AfterSave, HookPoint::StateChange];

    /// How a hook point is written in an `init.scm`, and read back in a message.
    ///
    /// Kebab-case of the variant, the rule [`Options::NAMES`] already follows.
    pub fn name(self) -> &'static str {
        match self {
            HookPoint::BeforeCommand => "before-command",
            HookPoint::AfterCommand => "after-command",
            HookPoint::StateChange => "state-change",
            HookPoint::BeforeSave => "before-save",
            HookPoint::AfterSave => "after-save",
        }
    }

    /// The hook points in a list, for a message that has to name them.
    pub fn listed(points: &[HookPoint]) -> String {
        let names: Vec<&str> = points.iter().map(|p| p.name()).collect();
        listed(&names)
    }
}

/// What a hook decided.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Proceed {
    Yes,
    /// Only meaningful at [`HookPoint::BeforeCommand`]: the command does not run.
    No,
}

/// A named function that runs at a [`HookPoint`].
///
/// Named, because a hook that cannot be removed is a leak — and M2's Steel
/// bindings will add and remove them at runtime, from a config file that is
/// re-evaluated.
pub struct Hook {
    pub name: String,
    pub point: HookPoint,
    pub run: Rc<dyn Fn(&mut Ctx<'_>) -> Proceed>,
}

/// The registered hooks: closures, and the command names a config attached.
///
/// Two lists rather than one, because a command-hook is not a closure and
/// cannot be made into one. A hook is `Fn(&mut Ctx) -> Proceed` and the
/// registry is not reachable from inside a `Ctx` — so a hook that *names* a
/// command cannot run it, and the session dispatches the name once the
/// closures have run and the borrow they needed is released.
///
/// Closures first, then names. Forced by that borrow rather than chosen, and
/// worth saying out loud because it is observable: a native hook at a point
/// always sees the buffer before any command a config hooked there.
#[derive(Default)]
pub struct Hooks {
    hooks: Vec<Hook>,
    commands: Vec<(HookPoint, String)>,
}

impl Hooks {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add(
        &mut self,
        name: impl Into<String>,
        point: HookPoint,
        run: impl Fn(&mut Ctx<'_>) -> Proceed + 'static,
    ) {
        let name = name.into();
        self.remove(point, &name);
        self.hooks.push(Hook {
            name,
            point,
            run: Rc::new(run),
        });
    }

    /// `true` if there was one to remove.
    ///
    /// Identified by point *and* name. By name alone, hooking one command at
    /// two points meant the second add deleted the first — silently, which is
    /// the worst way for a config to be wrong.
    pub fn remove(&mut self, point: HookPoint, name: &str) -> bool {
        let before = self.hooks.len();
        self.hooks.retain(|h| !(h.point == point && h.name == name));
        self.hooks.len() != before
    }

    /// Attach a command, by name, to `point`.
    ///
    /// Keyed by point *and* name like [`Hooks::remove`], so the same command
    /// at two points is two hooks and the same command twice at one point is
    /// one. Whether the point *accepts* a command is not asked here: this
    /// holds what it is given, and [`Session::apply`] is where a config is
    /// told it named a point that cannot dispatch.
    #[track_caller]
    pub fn add_command(&mut self, point: HookPoint, name: impl Into<String>) {
        // A command at any other HookPoint would be held and never dispatched —
        // the silent no-op the Declaration pass exists to prevent. A config is
        // told at load; this catches the native caller, in every test run.
        debug_assert!(
            HookPoint::FOR_COMMANDS.contains(&point),
            "`{}` cannot dispatch a command; meowr has {}",
            point.name(),
            HookPoint::listed(HookPoint::FOR_COMMANDS)
        );
        let name = name.into();
        if !self.commands.iter().any(|(p, n)| *p == point && *n == name) {
            self.commands.push((point, name));
        }
    }

    /// The commands attached to `point`, in the order they were added.
    pub fn commands(&self, point: HookPoint) -> Vec<&str> {
        self.commands
            .iter()
            .filter(|(p, _)| *p == point)
            .map(|(_, name)| name.as_str())
            .collect()
    }

    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.hooks.iter().map(|h| h.name.as_str())
    }

    /// The closures registered at `point`, by name.
    ///
    /// The counterpart of [`Hooks::commands`], and the other half of what a
    /// config may take away: `auto-exit-insert-on-save` is a closure meowr
    /// ships, not a command anyone can name.
    pub fn names_at(&self, point: HookPoint) -> impl Iterator<Item = &str> {
        self.hooks
            .iter()
            .filter(move |h| h.point == point)
            .map(|h| h.name.as_str())
    }

    /// Take a command back off `point`. `true` if there was one.
    pub fn remove_command(&mut self, point: HookPoint, name: &str) -> bool {
        let before = self.commands.len();
        self.commands.retain(|(p, n)| !(*p == point && n == name));
        self.commands.len() != before
    }

    /// Run every hook at `point`, in registration order.
    ///
    /// Returns [`Proceed::No`] if any of them refused. Every hook still runs:
    /// stopping at the first refusal would make the behaviour depend on
    /// registration order in a way nobody could reason about.
    pub fn run(&self, point: HookPoint, ctx: &mut Ctx<'_>) -> Proceed {
        let mut proceed = Proceed::Yes;
        for hook in self.hooks.iter().filter(|h| h.point == point) {
            if (hook.run)(ctx) == Proceed::No {
                proceed = Proceed::No;
            }
        }
        proceed
    }
}

/// How a command is carried out.
///
/// `Clone` but **not `Copy`**, and that is the whole point. An earlier version
/// was `Copy` with only the `Native` variant, whose doc comment promised a
/// `Scripted` variant in M2 — which it could not have delivered. A bare `fn`
/// pointer cannot capture, so it cannot hold a Steel closure, and adding the
/// variant would have forced `Handler` to stop being `Copy` after 161 commands
/// were already written against it. The registration macros in the session crate
/// exist only because `fn` pointers cannot capture; that was the first symptom
/// and it was not read as one at the time.
///
/// `Rc` rather than `Arc`: the buffer has a single owner and commands run on
/// its thread, so there is nothing to share across threads.
#[derive(Clone)]
pub enum Handler {
    Native(fn(&mut Ctx<'_>) -> Outcome),
    /// A command supplied by a Plugin: the names of other commands, run in
    /// order against one `Ctx`.
    ///
    /// Data rather than a closure, which is the whole of #48's answer. It can
    /// be checked before it runs, shown in the leader menu, and walked for
    /// cycles — none of which a closure allows.
    Composed(Vec<String>),
}

/// A registered command: a name, the metadata a menu needs, and a handler.
pub struct Command {
    name: String,
    description: String,
    takes_count: bool,
    /// Whether the command changes the buffer, so a read-only buffer can
    /// refuse it centrally rather than each command remembering to check.
    mutating: bool,
    /// Whether the command is meaningless without a selection, so the fallback
    /// table can be consulted when there is none.
    requires_selection: bool,
    /// Empty means valid in every state, which is the common case and reads
    /// better than listing all five.
    states: HashSet<State>,
    handler: Handler,
}

impl Command {
    pub fn new(name: impl Into<String>, handler: Handler) -> Self {
        Self {
            name: name.into(),
            description: String::new(),
            takes_count: false,
            mutating: false,
            requires_selection: false,
            states: HashSet::new(),
            handler,
        }
    }

    pub fn described(mut self, description: impl Into<String>) -> Self {
        self.description = description.into();
        self
    }

    pub fn taking_count(mut self) -> Self {
        self.takes_count = true;
        self
    }

    pub fn mutating(mut self) -> Self {
        self.mutating = true;
        self
    }

    pub fn requiring_selection(mut self) -> Self {
        self.requires_selection = true;
        self
    }

    pub fn in_states(mut self, states: &[State]) -> Self {
        self.states = states.iter().copied().collect();
        self
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn description(&self) -> &str {
        &self.description
    }

    pub fn takes_count(&self) -> bool {
        self.takes_count
    }

    pub fn is_mutating(&self) -> bool {
        self.mutating
    }

    pub fn requires_selection(&self) -> bool {
        self.requires_selection
    }

    pub fn valid_in(&self, state: State) -> bool {
        self.states.is_empty() || self.states.contains(&state)
    }
}

/// Every command meowr knows, addressed by name.
#[derive(Default)]
pub struct Registry {
    commands: HashMap<String, Command>,
}

impl Registry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a command, replacing any of the same name.
    ///
    /// For the names a human wrote out, which are the native table's own and
    /// can be checked against each other by eye. A name that arrives at
    /// runtime, or that the table builds rather than spells, goes through
    /// [`Registry::register_new`], which refuses to overwrite — see there.
    pub fn register(&mut self, command: Command) {
        self.commands.insert(command.name.clone(), command);
    }

    /// Register a command whose name nobody has taken.
    ///
    /// The path a Plugin's Declarations go through, and the path any name that
    /// was *built* rather than written goes through — the native table's
    /// per-Thing commands `concat!` theirs from a table, so no one reads one
    /// before it is registered. [`Registry::register`] stays infallible for
    /// names a human wrote out and can check by eye. Everywhere else,
    /// overwriting would replace a command with no indication at all — worse
    /// than the re-registration #48 declined to offer, because it cannot even
    /// call through to what it replaced.
    pub fn register_new(&mut self, command: Command) -> Result<(), RegisterError> {
        if self.commands.contains_key(&command.name) {
            return Err(RegisterError(format!(
                "`{}` is already a command",
                command.name
            )));
        }
        self.register(command);
        Ok(())
    }

    pub fn get(&self, name: &str) -> Option<&Command> {
        self.commands.get(name)
    }

    /// Every registered name, so a binding can discover what exists without
    /// compile-time knowledge of it.
    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.commands.keys().map(String::as_str)
    }

    /// A composition's steps, if `name` is one.
    ///
    /// The registry stores a composition and does not run it: running it means
    /// applying the session's guards to every step, and the guards need a
    /// Buffer and a fallback table that this crate cannot see. So the shape a
    /// composition has lives here and the running of it lives there (#89).
    pub fn composition(&self, name: &str) -> Option<&[String]> {
        match &self.commands.get(name)?.handler {
            Handler::Composed(names) => Some(names),
            _ => None,
        }
    }

    /// Run a command by name. `None` when no such command is registered — or
    /// when it is a composition, which this cannot run; see
    /// [`Registry::composition`].
    pub fn invoke(&self, name: &str, ctx: &mut Ctx<'_>) -> Option<Outcome> {
        let command = self.commands.get(name)?;
        match &command.handler {
            Handler::Native(f) => Some(f(ctx)),
            // Not the registry's to run. A composition's steps go through
            // the session one at a time so that each is behind the guards
            // `Session::dispatch_command` holds — read-only, State, and the
            // selection fallback, none of which this crate can see (#89). The
            // session asks [`Registry::composition`] before it gets here.
            Handler::Composed(_) => None,
        }
    }
}

/// What a key sequence means so far.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Resolution<'a> {
    /// Nothing is bound here, and nothing longer will be either.
    Unbound,
    /// A longer sequence starts this way; wait for more keys.
    Pending,
    /// This sequence names a command.
    Command(&'a str),
}

/// Why a command could not be registered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegisterError(String);

impl std::fmt::Display for RegisterError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for RegisterError {}

/// Why a binding was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BindError {
    /// The sequence is a prefix of an existing binding, or an existing binding
    /// is a prefix of it. Either way resolving the shorter one would be
    /// ambiguous.
    Ambiguous,
    /// Ring 2 only: the sequence sits under a prefix nobody declared, so the
    /// menu would have a row it cannot name. See `Leader::set`.
    NoPrefix,
}

impl core::fmt::Display for BindError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            BindError::Ambiguous => {
                write!(f, "the sequence conflicts with an existing binding")
            }
            BindError::NoPrefix => {
                write!(f, "the sequence needs a leader prefix that has a name")
            }
        }
    }
}

impl core::error::Error for BindError {}

/// Key sequences to command names, per state.
///
/// This is data, deliberately: a plugin will read and write it in M2, so
/// nothing here may depend on knowing which commands exist. Names are stored
/// as strings and are never checked against the registry, so a keymap can be
/// loaded before the commands it names are registered.
#[derive(Debug, Clone, Default)]
pub struct Keymap {
    bindings: HashMap<(State, Vec<Key>), String>,
    /// Every proper prefix of every bound sequence, so `resolve` can answer
    /// `Pending` without walking the whole table.
    prefixes: HashSet<(State, Vec<Key>)>,
}

/// How far out of the way a key is, for choosing which of several to name.
///
/// A character is what a modal editor is made of; a chord is one hand-shape
/// further off; a key with a name is off the letters entirely. Nothing depends
/// on the numbers beyond their order.
fn reachability(key: &Key) -> u8 {
    match key {
        Key::Char(_) => 0,
        Key::Ctrl(_) | Key::Alt(_) => 1,
        _ => 2,
    }
}

impl Keymap {
    pub fn new() -> Self {
        Self::default()
    }

    /// Bind `keys` to a command name in `state`.
    ///
    /// Refused when it would make an existing sequence ambiguous. Meow has no
    /// key that is both a command and a prefix, and allowing one would mean
    /// choosing between waiting and acting on every keystroke.
    pub fn bind(
        &mut self,
        state: State,
        keys: &[Key],
        command: impl Into<String>,
    ) -> Result<(), BindError> {
        let seq = keys.to_vec();
        if self.prefixes.contains(&(state, seq.clone())) {
            return Err(BindError::Ambiguous);
        }
        for len in 1..seq.len() {
            if self.bindings.contains_key(&(state, seq[..len].to_vec())) {
                return Err(BindError::Ambiguous);
            }
        }
        for len in 1..seq.len() {
            self.prefixes.insert((state, seq[..len].to_vec()));
        }
        self.bindings.insert((state, seq), command.into());
        Ok(())
    }

    /// The shortest key sequence that reaches `command` in `state`.
    ///
    /// The keymap is the one place that knows this, so anything wanting to
    /// *tell* a user which key to press asks here rather than repeating the
    /// binding in prose — `leader.rs` says why: "two sources for one fact
    /// drift, and the one nobody looks at drifts first."
    pub fn keys_for(&self, state: State, command: &str) -> Option<Vec<Key>> {
        self.bindings
            .iter()
            .filter(|((bound, _), name)| *bound == state && name.as_str() == command)
            .map(|((_, keys), _)| keys.clone())
            .min_by_key(|keys| {
                (
                    keys.len(),
                    // Then by what kind of key it is, not by how it is spelled.
                    // Sorting on the spelling made `--help` advertise `<left>`
                    // over `h`, because `<` sorts before a letter; ranking only
                    // `Key::Char` above the rest fixed that one pair and left
                    // `<next>` beating `C-v` for the same reason. What a reader
                    // wants told is the plainest key that reaches the command.
                    keys.iter().map(reachability).max().unwrap_or(0),
                    format!("{}", Keys(keys)),
                )
            })
    }

    /// Every binding in `state`, in the order a reader would scan them.
    ///
    /// Shortest sequences first, then by how the keys are written — so a
    /// single key is never buried under a chord that happens to hash before
    /// it. The order is decided here rather than at the call site because
    /// "what order do bindings come in" is a fact about the keymap, and the
    /// `HashMap` has one that nobody would want to read.
    pub fn bindings_in(&self, state: State) -> Vec<(&[Key], &str)> {
        let mut found: Vec<(&[Key], &str)> = self
            .bindings
            .iter()
            .filter(|((bound, _), _)| *bound == state)
            .map(|((_, keys), name)| (keys.as_slice(), name.as_str()))
            .collect();
        found.sort_by_key(|(keys, _)| (keys.len(), format!("{}", Keys(keys))));
        found
    }

    /// Remove a binding. `true` if there was one.
    ///
    /// Prefixes are recomputed rather than reference-counted. A prefix earns
    /// its place by having something below it, and rebuilding the set from the
    /// bindings that remain cannot drift out of step with them — where a count
    /// per prefix is a second source for the same fact. Unbinding happens when
    /// a config loads and not on any hot path, so the walk is free.
    pub fn unbind(&mut self, state: State, keys: &[Key]) -> bool {
        if self.bindings.remove(&(state, keys.to_vec())).is_none() {
            return false;
        }
        self.prefixes.clear();
        for (bound_state, seq) in self.bindings.keys() {
            for len in 1..seq.len() {
                self.prefixes.insert((*bound_state, seq[..len].to_vec()));
            }
        }
        true
    }

    pub fn resolve(&self, state: State, keys: &[Key]) -> Resolution<'_> {
        let seq = keys.to_vec();
        if let Some(name) = self.bindings.get(&(state, seq.clone())) {
            return Resolution::Command(name);
        }
        if self.prefixes.contains(&(state, seq)) {
            return Resolution::Pending;
        }
        Resolution::Unbound
    }
}

/// Which command stands in for another when there is no selection.
///
/// Data, not a `match`: Meow exposes this as `meow-selection-command-fallback`
/// for the user to change, and with no selection the digit keys are numeric
/// arguments while with one they expand — a difference that lives here.
#[derive(Debug, Clone, Default)]
pub struct Fallbacks {
    table: HashMap<String, String>,
}

impl Fallbacks {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set(&mut self, command: impl Into<String>, fallback: impl Into<String>) {
        self.table.insert(command.into(), fallback.into());
    }

    pub fn get(&self, command: &str) -> Option<&str> {
        self.table.get(command).map(String::as_str)
    }
}

#[cfg(test)]
mod tests;

/// The command that serves one Language, as a config declared it.
///
/// **An Option by the glossary's definition — "a named value the editor reads
/// to decide how to behave, held for the whole session and set by a Plugin" —
/// but deliberately not an [`OptionValue`], which is what keeps #50 intact.**
///
/// #50 kept strings out of `OptionValue` for a stated reason rather than a
/// blanket one: "the day a path or a program name needs configuring is also the
/// day someone has to decide about `~` expansion and shell quoting". Both are
/// answered below. What it does *not* need is a wider `OptionValue`, because
/// this does not fit `set-option`'s shape at all: an Option is one name and one
/// scalar, and this is keyed **per Language** and carries a program *and* an
/// argument list. Widening the scalar would not make a flat table of named
/// scalars hold a three-dimensional thing.
///
/// So it is its own kind of Declaration beside `Declaration::Option` rather
/// than a value inside one. Both are Declarations — the glossary is clear that
/// an option is one kind of Declaration and not a separate category.
///
/// **There is no shell, so there is no quoting.** The program and its arguments
/// are separate values from the moment a config writes them, and they go
/// straight to `Command::new(program).args(args)`. Nothing is ever split on
/// whitespace, so nothing ever needs escaping. This is a server *command*, not
/// a command *line*.
///
/// **A leading `~/` expands in the program, and nothing else does anywhere.** A
/// program name is usually bare and found on `PATH`; the one common exception is
/// a hand-built binary under a home directory. `$VAR`, globbing and word
/// splitting are all absent, which is what makes the paragraph above true.
/// Arguments are **not** expanded either — `--log ~/ra.log` stays literal — and
/// that asymmetry is deliberate: the program is a thing meowr must find in order
/// to run it, and an argument is a thing meowr hands over without reading.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LanguageServer {
    /// The program. Bare names are left for `PATH` to resolve.
    pub program: String,
    /// Its arguments, already separate. `--stdio` is the common one.
    pub args: Vec<String>,
}

impl LanguageServer {
    /// The program as it should be handed to the operating system.
    ///
    /// Only a leading `~/` is expanded, and only when a `home` is given. A bare
    /// `~` is left alone: as a program name it means nothing, and turning it
    /// into the home directory would name a directory rather than an executable
    /// — a confusing failure in place of an obvious one.
    ///
    /// **`home` is a parameter and not read from the environment here**, which
    /// the house rule requires: this is a domain crate and domain crates do no
    /// I/O, and `Ctx`'s own doc in this file promises "no filesystem, no process
    /// control". Reading `$HOME` would also make this impure and its test
    /// dependent on the environment it runs in. The binary knows the home
    /// directory because knowing such things is what a binary is for.
    pub fn resolved(&self, home: Option<&str>) -> String {
        with_home(&self.program, home)
    }
}

/// What formats a Language: a program and its arguments (ADR-0014).
///
/// **Declaration** data of [`LanguageServer`]'s shape, and resolved the same
/// way: a program found on `PATH` or under a leading `~/`, and arguments
/// handed over without being read -- except `{file}`, which is the buffer's
/// path. That is apheleia's `filepath`: the text still goes on stdin, and the
/// path is how `rustfmt.toml` or `.editorconfig` is found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Formatter {
    /// The program. Bare names are left for `PATH` to resolve.
    pub program: String,
    /// Its arguments, already separate. `{file}` in one is the buffer's path.
    pub args: Vec<String>,
}

/// A program and the arguments to run it with, ready for the host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Invocation {
    pub program: String,
    pub args: Vec<String>,
}

impl Invocation {
    /// The same, with a leading `~/` in the program expanded against `home`:
    /// the session builds an invocation without knowing the home directory,
    /// and the host, which does, finishes it.
    pub fn at_home(self, home: Option<&str>) -> Self {
        Self {
            program: with_home(&self.program, home),
            args: self.args,
        }
    }
}

impl Formatter {
    /// How the host runs this formatter for `file`. `home` is passed in, for
    /// the reason [`LanguageServer::resolved`] gives.
    pub fn invocation(&self, file: &str, home: Option<&str>) -> Invocation {
        Invocation {
            program: with_home(&self.program, home),
            args: self.args.iter().map(|arg| arg.replace("{file}", file)).collect(),
        }
    }
}

/// A program name with a leading `~/` expanded, when a home is given.
fn with_home(program: &str, home: Option<&str>) -> String {
    match (program.strip_prefix("~/"), home) {
        (Some(rest), Some(home)) => format!("{}/{rest}", home.trim_end_matches('/')),
        _ => program.to_string(),
    }
}
