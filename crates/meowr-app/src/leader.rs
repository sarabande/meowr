//! Ring 2: the leader tree behind `SPC`.
//!
//! Decision #12 settled the shape: **reserve, do not reassign**. The extracted
//! Doom tree has 229 commands under 16 prefixes and about 42% of it is magit,
//! org, LSP or DAP — out of M1 by construction. A key whose package will never
//! exist here is dropped; a key whose command arrives in a later milestone is
//! *held*, with a note naming that milestone rather than a fixed pair of them —
//! the milestones move, and this list has been renumbered three times.
//! Muscle memory is the entire point of Ring 2
//! and teaching a finger that `SPC g s` means something else for a year and
//! then taking it back is the worst available outcome.
//!
//! Descriptions come from the command registry, not from a table beside it.
//! Two sources for one fact drift, and the one nobody looks at drifts first.
//!
//! **Dropped**, and listed because "dropped" and "forgotten" look identical
//! from inside the table. Each is a Doom prefix whose package will not exist
//! here, so there is nothing to hold the key for:
//!
//! - `d` — dape, 19 bindings. No debug adapter is planned.
//! - `n` — notes, 18. org-mode.
//! - `&` — snippets, 7. yasnippet.
//! - `u`, `m`, `l`, `h`, `e`, `a` — one binding each, all into packages above.
//! - `v` — versioning, magit. It was *reserved* for the `wagit` Guest until
//!   Guests were dropped and ADR-0003 lost its second seam; with nothing to
//!   host magit, the key is dropped rather than held.
//! - `o` — open, 10 bindings: dired, vterm, a debugger, treemacs, a browser, a
//!   REPL, a new frame. The terminal Guest was one of the ten and went with the
//!   seam; none of the other nine is planned either, so the whole prefix drops
//!   rather than one row of it.
//!
//! A leader key is a [`Key`], not a `char`, which is why Doom's `C-f` fold
//! prefix can be held here rather than left out of the table for want of a way
//! to spell it. It is the only Doom prefix in that position, and it was
//! recorded as an omission for exactly as long as the table could not say it.

use meowr_command::{BindError, Key, Keys};

/// How a [`Entry::Reserved`] note may begin.
///
/// The note is the whole value of a reservation: it holds a key for something
/// that does not exist yet, so a note that does not say *when* is
/// indistinguishable from a key nobody got round to. Checked by prefix rather
/// than by `contains`, which would pass on a note that merely mentions a
/// milestone somewhere in its prose.
///
/// `unscheduled` is a real answer and not a dodge. Some of Doom's rows are
/// things meowr would want and nobody has planned — dropping them would say
/// they are not coming, and inventing a milestone for them would say more
/// than is known.
///
/// Here rather than in a test, because two tests ask this question — the
/// leader's own and `rings.rs`, which classifies every row of the author's
/// tree — and two copies of a list is how one of them goes stale.
pub const RESERVATION_OPENINGS: &[&str] = &["unscheduled — "];

/// What a leader key leads to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Entry {
    /// A command in the registry. Its description comes from there.
    Command(String),
    /// More keys below this one.
    Prefix(String),
    /// Held for a milestone that has not arrived. Not dead, not reassigned.
    Reserved(String),
}

/// One entry of the table below, before the tree is built from it.
///
/// A separate type so the table can stay a `const` of `&'static str` — it is
/// 33 entries thick with the reasoning behind each key, and rewriting it into
/// constructor calls would put every one of those comments at risk to gain
/// nothing the conversion in [`Leader::new`] does not.
#[derive(Debug, Clone, Copy)]
enum SeedEntry {
    Command(&'static str),
    Prefix(&'static str),
    Reserved(&'static str),
}

#[derive(Debug, Clone, Copy)]
struct Seed {
    keys: &'static [Key],
    entry: SeedEntry,
}

/// Top-level keys Doom uses that meowr will not take, and why.
///
/// Decision #12 has two outcomes and the tree only ever recorded one. A key
/// held for later is a `Reserved` seed with a milestone; a key that is never
/// coming has, until now, been *absent* — indistinguishable from one nobody
/// got round to. #46 found exactly that failure with `SPC b k`, which read as
/// a stale label and was hiding a real gap.
///
/// Listing them is what lets a test say "every Doom key is bound, reserved, or
/// dropped on purpose", which is decision #12 fully enforced rather than half.
pub const DROPPED: &[(Key, &str)] = &[
    (
        Key::Char('n'),
        "notes — org, and ADR-0001 rules out the runtime it needs",
    ),
    (Key::Char('d'), "dape — no debugger is charted"),
    (
        Key::Char('&'),
        "snippets — yasnippet; `SPC i` holds the insert prefix instead",
    ),
    (Key::Char('a'), "embark-act — no equivalent concept"),
    (Key::Char('e'), "eval — no Lisp to evaluate (ADR-0001)"),
    (
        Key::Char('h'),
        "consult-history — meowr keeps no command history",
    ),
    (
        Key::Char('l'),
        "+localleader — never implemented; #48 said so plainly",
    ),
    (Key::Char('m'), "consult-man — no man pages"),
    (Key::Char(','), "dired-jump — no dired"),
    (
        Key::Esc,
        "unreachable by construction: `keypad_key` takes ESC to leave Keypad State before the tree is consulted, so no binding under it could ever fire. The TSV puts magit and consult under it, which would drop it too — that reason is true and hides this one",
    ),
];

/// The one key that opens Ring 2.
///
/// Hard-wired, and single. The Doom table has a `+localleader` at `SPC l`
/// which meowr has never implemented and this is not; nor can the leader key
/// be changed. A second leader is a real feature with its own questions —
/// which tree a sequence lands in, what the menu shows for each — and it does
/// not arrive as a side effect of letting a config bind the first.
pub const KEY: Key = Key::Char(' ');

/// One binding in the tree.
#[derive(Debug, Clone)]
pub struct Binding {
    pub keys: Vec<Key>,
    pub entry: Entry,
}

/// The default leader tree.
///
/// Everything here either runs today or says which milestone it waits for. Not
/// "the M1 tree" any more: M2 added `SPC t` and took `SPC b k`, and a Plugin
/// may rebind any of it (#58).
/// There is deliberately no fourth category: a key that is neither bound nor
/// explained is how a keymap rots.
const SEED: &[Seed] = &[
    // The cheatsheet, on the key Doom gives `meow-cheatsheet`. It was dropped
    // rather than reserved, on the ground that "the leader menu is the
    // cheatsheet" — and #79 is the record of that being false: the menu is a
    // view of the walk you are in the middle of, and Ring 1 is not under `SPC`
    // at all. The premise went, so the key came back.
    Seed {
        keys: &[Key::Char('?')],
        entry: SeedEntry::Command("describe-bindings"),
    },
    // `SPC SPC` is `projectile-find-file`: the picker, narrowed to the Project,
    // the way `SPC .` and `SPC RET` narrow it to their sources (#147).
    Seed {
        keys: &[Key::Char(' ')],
        entry: SeedEntry::Command("project-find-file"),
    },
    // Files.
    Seed {
        keys: &[Key::Char('f')],
        entry: SeedEntry::Prefix("file"),
    },
    Seed {
        keys: &[Key::Char('f'), Key::Char('f')],
        entry: SeedEntry::Command("find-file"),
    },
    Seed {
        keys: &[Key::Char('f'), Key::Char('s')],
        entry: SeedEntry::Command("write-file"),
    },
    // `S` past `s`, which is how `SPC q Q` and `SPC b K` are past their own
    // lower-case forms: the shifted key is the one that goes through.
    Seed {
        keys: &[Key::Char('f'), Key::Char('S')],
        entry: SeedEntry::Command("overwrite-file"),
    },
    Seed {
        keys: &[Key::Char('f'), Key::Char('r')],
        entry: SeedEntry::Command("find-file"),
    },
    // `recover-file`. Emacs binds it to nothing at all — it is an `M-x`
    // command — so, like the Grab, the key is a decision. Doom's `f` subtree
    // leaves `a` free, and auto-save is what there is to recover from.
    Seed {
        keys: &[Key::Char('f'), Key::Char('a')],
        entry: SeedEntry::Command("recover-file"),
    },
    // `+default/yank-buffer-path`. The one row of Doom's file prefix meowr can
    // answer without a project, a `~/.emacs.d`, sudo, or a file operation
    // nobody has scheduled (#98).
    Seed {
        keys: &[Key::Char('f'), Key::Char('y')],
        entry: SeedEntry::Command("yank-buffer-path"),
    },
    // `f F` reads as though it were project-shaped and is not:
    // `+default/find-file-under-here` is rooted at the current directory and
    // needs no root, so its `project:` prefix was wrong rather than stale
    // (#128), and it is bound over the directory rather than the Project (#149).
    Seed {
        keys: &[Key::Char('f'), Key::Char('F')],
        entry: SeedEntry::Command("find-file-under-here"),
    },
    // Doom's `f R` and `f Y` are project-shaped, which #69 evicted out of M3
    // and #128 claimed for M5; bound by #148 over the Project's root.
    Seed {
        keys: &[Key::Char('f'), Key::Char('R')],
        entry: SeedEntry::Command("project-recent-files"),
    },
    Seed {
        keys: &[Key::Char('f'), Key::Char('Y')],
        entry: SeedEntry::Command("yank-buffer-path-relative-to-project"),
    },
    // Held, each naming what it waits for: file operations and config-opening
    // that meowr would want and nobody has scheduled — `unscheduled` is the
    // honest note, not a milestone invented to make the row look decided.
    Seed {
        keys: &[Key::Char('f'), Key::Char('C')],
        entry: SeedEntry::Command("copy-this-file"),
    },
    Seed {
        keys: &[Key::Char('f'), Key::Char('D')],
        entry: SeedEntry::Command("delete-this-file"),
    },
    Seed {
        keys: &[Key::Char('f'), Key::Char('m')],
        entry: SeedEntry::Command("move-this-file"),
    },
    Seed {
        keys: &[Key::Char('f'), Key::Char('P')],
        entry: SeedEntry::Command("open-config"),
    },
    Seed {
        keys: &[Key::Char('f'), Key::Char('p')],
        entry: SeedEntry::Command("find-file-in-config"),
    },
    Seed {
        keys: &[Key::Char('f'), Key::Char('X')],
        entry: SeedEntry::Command("switch-to-scratch-buffer"),
    },
    Seed {
        keys: &[Key::Char('f'), Key::Char('x')],
        entry: SeedEntry::Command("toggle-scratch-buffer"),
    },
    // Buffers.
    Seed {
        keys: &[Key::Char('b')],
        entry: SeedEntry::Prefix("buffer"),
    },
    Seed {
        keys: &[Key::Char('b'), Key::Char('b')],
        entry: SeedEntry::Command("next-buffer"),
    },
    Seed {
        keys: &[Key::Char('b'), Key::Char('s')],
        entry: SeedEntry::Command("write-file"),
    },
    // `b k → kill-buffer` in the Doom table, which is Emacs's name-taking one:
    // it reads a buffer name and defaults to this one. meowr's kills this one
    // and cannot yet be given another, because naming a buffer means the
    // picker — so this is `kill-buffer` with its argument still missing, not a
    // different command wearing its name. Doom's explicit current-buffer key,
    // `b d → kill-current-buffer`, stays free rather than being claimed for the
    // reduced version. Reserved as "unscheduled" between #46 and #52.
    Seed {
        keys: &[Key::Char('b'), Key::Char('k')],
        entry: SeedEntry::Command("kill-buffer"),
    },
    // Coined, and the Doom table is why it can be: `b` has `. D X d k o x` and
    // no `K`, so decision #12 is not broken by taking it. The shape is `SPC q q`
    // and `SPC q Q` — one key refuses on unsaved work, its capital means it —
    // but those two were both inherited and this one is a decision, the way
    // `SPC f a` for `recover-file` was.
    Seed {
        keys: &[Key::Char('b'), Key::Char('K')],
        entry: SeedEntry::Command("kill-buffer-without-saving"),
    },
    // Doom's own two, answered with commands meowr already had (#101). `SPC .`
    // opens the same picker: Doom binds both and so does the author's config.
    Seed {
        keys: &[Key::Char('b'), Key::Char('.')],
        entry: SeedEntry::Command("switch-to-buffer"),
    },
    Seed {
        keys: &[Key::Char('b'), Key::Char('d')],
        entry: SeedEntry::Command("kill-buffer"),
    },
    // The four rows the author's own `config-buffer.el` names and meowr held
    // as `unscheduled` (#68). A reservation is meant to end by being bound,
    // and nothing was ever in the way of these: the window tree, `kill-buffer`
    // and `Buffer::scratch` were each built for something else, so all four
    // are routes to what already exists.
    Seed {
        keys: &[Key::Char('b'), Key::Char('D')],
        entry: SeedEntry::Command("kill-buffer-and-window"),
    },
    Seed {
        keys: &[Key::Char('b'), Key::Char('o')],
        entry: SeedEntry::Command("kill-other-buffers"),
    },
    // `O` past `o`, the shape `SPC q Q` and `SPC b K` already have. Not one of
    // Doom's rows: Doom asks a yes-or-no question per modified buffer and
    // meowr has no yes/no read, so the decision is a key.
    Seed {
        keys: &[Key::Char('b'), Key::Char('O')],
        entry: SeedEntry::Command("kill-other-buffers-without-saving"),
    },
    Seed {
        keys: &[Key::Char('b'), Key::Char('X')],
        entry: SeedEntry::Command("open-scratch-buffer"),
    },
    Seed {
        keys: &[Key::Char('b'), Key::Char('x')],
        entry: SeedEntry::Command("switch-to-scratch-buffer"),
    },
    // Doom's quit prefix. `q q` and `q Q` are bound above and `q F` below;
    // the sessions are four keys for one feature meowr does not have, and
    // nobody has planned it (#99).
    Seed {
        keys: &[Key::Char('q'), Key::Char('F')],
        entry: SeedEntry::Command("kill-all-buffers"),
    },
    Seed {
        keys: &[Key::Char('q'), Key::Char('r')],
        entry: SeedEntry::Command("restart"),
    },
    Seed {
        keys: &[Key::Char('q'), Key::Char('S')],
        entry: SeedEntry::Reserved("unscheduled — sessions: save one"),
    },
    Seed {
        keys: &[Key::Char('q'), Key::Char('s')],
        entry: SeedEntry::Reserved("unscheduled — sessions: quick-save"),
    },
    Seed {
        keys: &[Key::Char('q'), Key::Char('L')],
        entry: SeedEntry::Reserved("unscheduled — sessions: load one"),
    },
    Seed {
        keys: &[Key::Char('q'), Key::Char('l')],
        entry: SeedEntry::Reserved("unscheduled — sessions: quick-load"),
    },
    // Doom's toggle prefix. `t r` is bound above, which #67 discharged.
    Seed {
        keys: &[Key::Char('t'), Key::Char('f')],
        entry: SeedEntry::Reserved("unscheduled — LSP: diagnostics"),
    },
    Seed {
        keys: &[Key::Char('t'), Key::Char('l')],
        entry: SeedEntry::Reserved("unscheduled — line numbers"),
    },
    Seed {
        keys: &[Key::Char('t'), Key::Char('c')],
        entry: SeedEntry::Reserved("unscheduled — the fill-column indicator"),
    },
    Seed {
        keys: &[Key::Char('t'), Key::Char('I')],
        entry: SeedEntry::Reserved("unscheduled — the indent style"),
    },
    // Doom's `SPC w` is workspaces *and* windows, and in the author's config
    // every row of it is a session or `winner`. meowr's window commands live
    // here anyway; see the note in `rings.rs` on `w s`, the one key this
    // effort has knowingly reassigned (#102).
    Seed {
        keys: &[Key::Char('w'), Key::Char('u')],
        entry: SeedEntry::Reserved("unscheduled — undo the window layout"),
    },
    Seed {
        keys: &[Key::Char('w'), Key::Char('U')],
        entry: SeedEntry::Reserved("unscheduled — redo the window layout"),
    },
    Seed {
        keys: &[Key::Char('w'), Key::Char('l')],
        entry: SeedEntry::Reserved("unscheduled — sessions: load one"),
    },
    Seed {
        keys: &[Key::Char('w'), Key::Char('L')],
        entry: SeedEntry::Reserved("unscheduled — sessions: quick-load"),
    },
    Seed {
        keys: &[Key::Char('w'), Key::Char('a')],
        entry: SeedEntry::Reserved("unscheduled — sessions: autosave one"),
    },
    // Windows. Doom calls this `workspaces/windows`; workspaces are out by
    // ADR-0004, so the prefix keeps only its second half.
    Seed {
        keys: &[Key::Char('w')],
        entry: SeedEntry::Prefix("window"),
    },
    Seed {
        keys: &[Key::Char('w'), Key::Char('s')],
        entry: SeedEntry::Command("split-below"),
    },
    Seed {
        keys: &[Key::Char('w'), Key::Char('v')],
        entry: SeedEntry::Command("split-right"),
    },
    Seed {
        keys: &[Key::Char('w'), Key::Char('w')],
        entry: SeedEntry::Command("other-window"),
    },
    Seed {
        keys: &[Key::Char('w'), Key::Char('d')],
        entry: SeedEntry::Command("delete-window"),
    },
    Seed {
        keys: &[Key::Char('w'), Key::Char('m')],
        entry: SeedEntry::Command("delete-other-windows"),
    },
    // Search.
    Seed {
        keys: &[Key::Char('s')],
        entry: SeedEntry::Prefix("search"),
    },
    Seed {
        keys: &[Key::Char('s'), Key::Char('s')],
        entry: SeedEntry::Command("search"),
    },
    Seed {
        keys: &[Key::Char('s'), Key::Char('i')],
        entry: SeedEntry::Command("visit"),
    },
    // `consult-line`, which Doom calls "Search buffer". The picker's `line`
    // source, which until #97 no leader key reached.
    Seed {
        keys: &[Key::Char('s'), Key::Char('b')],
        entry: SeedEntry::Command("search-lines"),
    },
    Seed {
        keys: &[Key::Char('s'), Key::Char('p')],
        entry: SeedEntry::Command("search-project"),
    },
    // The rest of Doom's search prefix that meowr answers. Held rather than
    // reassigned by #97 while project search had no milestone, and bound by
    // #139 and #140; `SPC p s` and `p .` reach two of them again (#147).
    Seed {
        keys: &[Key::Char('s'), Key::Char('d')],
        entry: SeedEntry::Command("search-here"),
    },
    Seed {
        keys: &[Key::Char('s'), Key::Char('D')],
        entry: SeedEntry::Command("search-elsewhere"),
    },
    Seed {
        keys: &[Key::Char('s'), Key::Char('P')],
        entry: SeedEntry::Reserved("unscheduled — project search: another project"),
    },
    Seed {
        keys: &[Key::Char('s'), Key::Char('.')],
        entry: SeedEntry::Command("search-symbol"),
    },
    Seed {
        keys: &[Key::Char('s'), Key::Char('S')],
        entry: SeedEntry::Command("search-buffer"),
    },
    Seed {
        keys: &[Key::Char('s'), Key::Char('B')],
        entry: SeedEntry::Command("search-buffers"),
    },
    Seed {
        keys: &[Key::Char('s'), Key::Char('I')],
        entry: SeedEntry::Command("search-definitions"),
    },
    // Held for later, on the keys Doom actually uses. `v` is versioning and
    // `k` is code — *not* `g` and `c`, neither of which is a Doom leader
    // *prefix*. Reserving the wrong key is reassigning by another name, and
    // the finger it teaches is wrong for exactly as long as the reservation
    // holds.
    // Discharged the way `SPC t` was by #50: a reservation ends by being
    // honoured, and Doom's own name for this prefix is `code`. `k k` is
    // `+lookup/documentation`, which is the one row M4's LSP slice answers —
    // the other twenty in Doom's table are not bound, because nothing here
    // does them yet, and a key bound to nothing is the promise #12 is against.
    Seed {
        keys: &[Key::Char('k')],
        entry: SeedEntry::Prefix("code"),
    },
    Seed {
        keys: &[Key::Char('k'), Key::Char('k')],
        entry: SeedEntry::Command("lookup-documentation"),
    },
    // The other twenty rows of Doom's `code` prefix, held one at a time now
    // that the blanket reservation on `k` is gone. Spelled out rather than left
    // to the prefix, because that is what discharging only `k k` *means*: a
    // prefix accounts for nothing on its own, and a row under a bound prefix
    // with no entry of its own is a key that silently does nothing.
    //
    // Split by what each is waiting on, which is not the same ticket. The LSP
    // rows that *read* are bound, the last three by #146; the three that write,
    // and format, are M6's (#154), which is about text meowr did not type;
    // eval and compile wait on meowr being able to run a subprocess and show
    // its output, which nothing charts.
    Seed {
        keys: &[Key::Char('k'), Key::Char('d')],
        entry: SeedEntry::Command("lookup-definition"),
    },
    Seed {
        keys: &[Key::Char('k'), Key::Char('D')],
        entry: SeedEntry::Command("lookup-references"),
    },
    Seed {
        keys: &[Key::Char('k'), Key::Char('t')],
        entry: SeedEntry::Command("lookup-type-definition"),
    },
    Seed {
        keys: &[Key::Char('k'), Key::Char('i')],
        entry: SeedEntry::Command("lookup-implementations"),
    },
    Seed {
        keys: &[Key::Char('k'), Key::Char('a')],
        entry: SeedEntry::Command("lsp-execute-code-action"),
    },
    Seed {
        keys: &[Key::Char('k'), Key::Char('r')],
        entry: SeedEntry::Command("lsp-rename"),
    },
    Seed {
        keys: &[Key::Char('k'), Key::Char('o')],
        entry: SeedEntry::Command("lsp-organize-imports"),
    },
    Seed {
        keys: &[Key::Char('k'), Key::Char('x')],
        entry: SeedEntry::Command("list-diagnostics"),
    },
    Seed {
        keys: &[Key::Char('k'), Key::Char('j')],
        entry: SeedEntry::Command("lookup-symbols"),
    },
    Seed {
        keys: &[Key::Char('k'), Key::Char('J')],
        entry: SeedEntry::Command("lookup-symbols-everywhere"),
    },
    // `+default/lsp-command-map` is lsp-mode's whole keymap -- restarts,
    // toggles, peeks, `lsp-describe-session` -- and M5 never named it (#146).
    // `unscheduled` rather than carried, which is #132's rule for a row a map
    // does not intend to answer.
    Seed {
        keys: &[Key::Char('k'), Key::Char('l')],
        entry: SeedEntry::Reserved("unscheduled — LSP: the rest of the command map"),
    },
    // Doom's `+format/region-or-buffer`, and the whole buffer always rather
    // than a region: under Meow a selection is nearly always active, and
    // Doom's region path formats a fragment as a standalone file (ADR-0014).
    Seed {
        keys: &[Key::Char('k'), Key::Char('f')],
        entry: SeedEntry::Command("format-buffer"),
    },
    Seed {
        keys: &[Key::Char('k'), Key::Char('c')],
        entry: SeedEntry::Reserved("unscheduled — compile, and somewhere to show its output"),
    },
    Seed {
        keys: &[Key::Char('k'), Key::Char('C')],
        entry: SeedEntry::Reserved("unscheduled — recompile"),
    },
    Seed {
        keys: &[Key::Char('k'), Key::Char('e')],
        entry: SeedEntry::Reserved("unscheduled — evaluate the buffer or the region"),
    },
    Seed {
        keys: &[Key::Char('k'), Key::Char('E')],
        entry: SeedEntry::Reserved("unscheduled — evaluate the region and replace it"),
    },
    Seed {
        keys: &[Key::Char('k'), Key::Char('s')],
        entry: SeedEntry::Reserved("unscheduled — send the region to a repl"),
    },
    Seed {
        keys: &[Key::Char('k'), Key::Char('w')],
        entry: SeedEntry::Reserved("unscheduled — delete trailing whitespace"),
    },
    Seed {
        keys: &[Key::Char('k'), Key::Char('W')],
        entry: SeedEntry::Reserved("unscheduled — delete trailing newlines"),
    },
    Seed {
        keys: &[Key::Char('i')],
        entry: SeedEntry::Reserved("unscheduled — insert"),
    },
    // Doom's `n` is notes and `d` is dape: 33 bindings between them, and both
    // are **dropped** rather than held. ADR-0001 rules out an Emacs Lisp
    // runtime, so org is not coming; a debugger is not charted anywhere. The
    // map's rule allows both outcomes — "a key whose package will never exist
    // here is dropped; a key whose command arrives later is *held*" — and the
    // defect this fixes is that a drop nobody wrote down is indistinguishable
    // from an oversight. `DROPPED` below is that record, and a test reads it.
    //
    // #50 discharged this reservation. Doom's toggles are `F I b c f l r s v
    // w`; meowr has no line numbers, no fill column and no minor modes, so
    // `r` is the one it can honour. Auto-save is not on Doom's list and is an
    // Option besides — session-wide and configured once, where a toggle is a
    // Buffer property changed as you work.
    Seed {
        keys: &[Key::Char('t')],
        entry: SeedEntry::Prefix("toggle"),
    },
    Seed {
        keys: &[Key::Char('t'), Key::Char('r')],
        entry: SeedEntry::Command("toggle-read-only"),
    },
    // Doom's project prefix, five rows in the reference (#147). Two are the
    // searches `SPC s p` and `SPC s .` already reach, under the prefix named
    // for what they search. `p F` needs a list of *other* projects, which #130
    // put out of reach; the two scratch rows wait with `f x` and `f X`.
    Seed {
        keys: &[Key::Char('p')],
        entry: SeedEntry::Prefix("project"),
    },
    Seed {
        keys: &[Key::Char('p'), Key::Char('s')],
        entry: SeedEntry::Command("search-project"),
    },
    Seed {
        keys: &[Key::Char('p'), Key::Char('.')],
        entry: SeedEntry::Command("search-symbol"),
    },
    Seed {
        keys: &[Key::Char('p'), Key::Char('F')],
        entry: SeedEntry::Reserved("unscheduled — project: a file in another project"),
    },
    Seed {
        keys: &[Key::Char('p'), Key::Char('x')],
        entry: SeedEntry::Command("toggle-project-scratch-buffer"),
    },
    Seed {
        keys: &[Key::Char('p'), Key::Char('X')],
        entry: SeedEntry::Command("switch-to-project-scratch-buffer"),
    },
    // Stock Doom binds `SPC u` to `meow-universal-argument`, and meowr already
    // had the command on `C-u`. The binding was the only thing missing.
    Seed {
        keys: &[Key::Char('u')],
        entry: SeedEntry::Command("universal-argument"),
    },
    // The author's own two, from `config-buffer.el`. Doom's are
    // `switch-to-buffer` and `recentf-open-files` — two *sources* — so they
    // get two commands rather than a second name for `find-file`. Pointing
    // both at `find-file` was tried first and made `keys_for` advertise
    // `SPC .` as the way to reach `*config-error*`.
    Seed {
        keys: &[Key::Char('.')],
        entry: SeedEntry::Command("switch-to-buffer"),
    },
    Seed {
        keys: &[Key::Enter],
        entry: SeedEntry::Command("recent-files"),
    },
    // The Grab. Meow's, not Doom's, so there is no upstream row to copy and
    // the key is a decision — made under decision #12 like any other: Doom has
    // no top-level `g`, so `SPC g` collides with nothing and teaches no finger
    // something that has to be taken back.
    //
    // Not that `g` is unused in the tree — `SPC v g` is magit-status, and
    // `SPC v f g` and `ESC g` exist too. Those are different *paths*, reached
    // through the `v` subtree this file reserves for M4, so nothing here
    // claims them. The rule is about a key at a given point in the tree, not
    // about a letter.
    //
    // `s` is swap and `y` is sync, each the letter that tells the two apart.
    Seed {
        keys: &[Key::Char('g')],
        entry: SeedEntry::Prefix("grab"),
    },
    Seed {
        keys: &[Key::Char('g'), Key::Char('s')],
        entry: SeedEntry::Command("swap-grab"),
    },
    Seed {
        keys: &[Key::Char('g'), Key::Char('y')],
        entry: SeedEntry::Command("sync-grab"),
    },
    // Doom's fold prefix, on the key Doom uses. Four of its eight rows are
    // bound and two are dropped: `C-f C-d` and `C-f C-a C-d` delete a *vimish*
    // fold, and nothing creates one here — nor does Doom in this
    // configuration, where `vimish-fold-global-mode` sits inside
    // `(modulep! :editor evil)` and the only binding that creates one is
    // evil's `zf`.
    //
    // Doom has **eight** fold commands and ADR-0012 counts four of them
    // unreachable here. Two of those four, `+fold/close` and `+fold/open-rec`,
    // are about fold state and simply have no leader row to reserve; the other
    // two, `+fold/next` and `+fold/previous`, are navigation and never had one
    // either. Spelled out because "four reachable of eight" and "four of the
    // six state commands" are the same fact seen from two sides.
    Seed {
        keys: &[Key::Ctrl('f')],
        entry: SeedEntry::Prefix("fold"),
    },
    Seed {
        keys: &[Key::Ctrl('f'), Key::Ctrl('f')],
        entry: SeedEntry::Command("fold-toggle"),
    },
    Seed {
        keys: &[Key::Ctrl('f'), Key::Ctrl('u')],
        entry: SeedEntry::Command("fold-open"),
    },
    Seed {
        keys: &[Key::Ctrl('f'), Key::Ctrl('a')],
        entry: SeedEntry::Prefix("all"),
    },
    Seed {
        keys: &[Key::Ctrl('f'), Key::Ctrl('a'), Key::Ctrl('f')],
        entry: SeedEntry::Command("fold-close-all"),
    },
    Seed {
        keys: &[Key::Ctrl('f'), Key::Ctrl('a'), Key::Ctrl('u')],
        entry: SeedEntry::Command("fold-open-all"),
    },
    // Quit.
    Seed {
        keys: &[Key::Char('q')],
        entry: SeedEntry::Prefix("quit"),
    },
    Seed {
        keys: &[Key::Char('q'), Key::Char('q')],
        entry: SeedEntry::Command("quit"),
    },
    // `kill-emacs` in Doom's tree, and the way past a refused `SPC q q`.
    Seed {
        keys: &[Key::Char('q'), Key::Char('Q')],
        entry: SeedEntry::Command("quit-without-saving"),
    },
];

/// The leader keys that reach `command`, written as `SPC q Q`.
///
/// The counterpart of `Keymap::keys_for` for Ring 2, and for the same reason:
/// a message that tells someone which key to press must ask the table rather
/// than repeat it.
/// Ring 2, as the session holds it.
///
/// Seeded from the table above and owned from then on, because a Plugin adds
/// leader keys and names prefixes — and until #57 this was a `const`, so Ring 2
/// was the one part of the keymap a config could not reach at all. That matters
/// more than it sounds: `~/docs/meowr-doom-leader.tsv` holds 252 leader
/// bindings, and the leader is where a Doom user's customisation lives.
#[derive(Debug, Clone)]
pub struct Leader {
    bindings: Vec<Binding>,
}

impl Default for Leader {
    fn default() -> Self {
        Self::new()
    }
}

impl Leader {
    pub fn new() -> Self {
        Self {
            bindings: SEED
                .iter()
                .map(|seed| Binding {
                    keys: seed.keys.to_vec(),
                    entry: match seed.entry {
                        SeedEntry::Command(name) => Entry::Command(name.into()),
                        SeedEntry::Prefix(name) => Entry::Prefix(name.into()),
                        SeedEntry::Reserved(note) => Entry::Reserved(note.into()),
                    },
                })
                .collect(),
        }
    }

    /// Every binding, in the order declared.
    pub fn bindings(&self) -> &[Binding] {
        &self.bindings
    }

    /// The shortest keys *under* the leader that reach `command`.
    ///
    /// A suffix rather than a sequence, which is the whole difference from
    /// [`Leader::keys_for`] beside it: that one writes `SPC` into its answer,
    /// and `SPC` is a binding like any other. What precedes the suffix is
    /// whatever the keymap has on `leader`, and the tree cannot see the
    /// keymap — so the caller that can holds them together.
    pub fn suffix_for(&self, command: &str) -> Option<Vec<Key>> {
        self.bindings
            .iter()
            .filter(|binding| matches!(&binding.entry, Entry::Command(name) if name == command))
            .map(|binding| binding.keys.clone())
            .min_by_key(|keys| (keys.len(), format!("{}", Keys(keys))))
    }

    /// What `keys` resolves to, if anything.
    pub fn lookup(&self, keys: &[Key]) -> Option<&Entry> {
        self.bindings
            .iter()
            .find(|b| b.keys == keys)
            .map(|b| &b.entry)
    }

    /// The keys available directly below `prefix`, in the order declared.
    pub fn below(&self, prefix: &[Key]) -> Vec<(&Key, &Entry)> {
        self.bindings
            .iter()
            .filter(|b| b.keys.len() == prefix.len() + 1 && b.keys.starts_with(prefix))
            .filter_map(|b| b.keys.last().map(|k| (k, &b.entry)))
            .collect()
    }

    /// Whether anything at all lies below `prefix`.
    pub fn is_prefix(&self, keys: &[Key]) -> bool {
        matches!(self.lookup(keys), Some(Entry::Prefix(_))) || !self.below(keys).is_empty()
    }

    /// Put `entry` at `keys`, replacing whatever was there.
    ///
    /// Refused where the tree could not show or reach the result, which is the
    /// same rule `Keymap::bind` applies and for the same reason — except that
    /// Ring 2 has a menu, so a third case joins the two:
    ///
    /// - Nothing may live under a command: `SPC b b` is one, so `SPC b b x`
    ///   can never be typed.
    /// - A command may not sit over a subtree, which would hide every key
    ///   below it.
    /// - A binding may not appear under a prefix nobody declared. Which-key
    ///   exists to say what `SPC d` leads to before you commit, and an
    ///   auto-created prefix with no name is a blank row in the one interface
    ///   whose whole job is to have none.
    ///
    /// Anything the default table holds may be replaced, reservations
    /// included: decision #12 is a promise meowr makes about the keys *it*
    /// ships, not a lock on the user's own config.
    pub fn set(&mut self, keys: Vec<Key>, entry: Entry) -> Result<(), BindError> {
        for len in 1..keys.len() {
            match self.lookup(&keys[..len]) {
                Some(Entry::Command(_)) => return Err(BindError::Ambiguous),
                Some(_) => {}
                None => return Err(BindError::NoPrefix),
            }
        }
        if matches!(entry, Entry::Command(_)) && !self.below(&keys).is_empty() {
            return Err(BindError::Ambiguous);
        }
        self.insert(keys, entry);
        Ok(())
    }

    fn insert(&mut self, keys: Vec<Key>, entry: Entry) {
        match self.bindings.iter_mut().find(|b| b.keys == keys) {
            Some(existing) => existing.entry = entry,
            None => self.bindings.push(Binding { keys, entry }),
        }
    }

    /// Take `keys` out of the tree. `true` if there was something there.
    pub fn unset(&mut self, keys: &[Key]) -> bool {
        let before = self.bindings.len();
        self.bindings.retain(|b| b.keys != keys);
        self.bindings.len() != before
    }
}

#[cfg(test)]
mod tests;
