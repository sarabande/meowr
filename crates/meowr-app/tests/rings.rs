//! Ring 2, kept honest: every leader row is accounted for.
//!
//! `leader.rs`'s own header says the thing this file exists to stop: "'dropped'
//! and 'forgotten' look identical from inside the table". A key that nobody
//! decided about looks exactly like a key somebody decided to drop, and the
//! difference is invisible until the day a finger reaches for it.
//!
//! So the author's leader is read from `rings/doom-leader.tsv` and every
//! command row in it must land in exactly one of four buckets. The fourth is
//! the fidelity harness's `DIVERGENCES` trick: an *open* row is parked where it
//! can be seen rather than hidden, the suite is green while it waits, and
//! closing its ticket breaks the build until the entry goes.

use meowr_app::leader::{Entry, Leader, RESERVATION_OPENINGS};
use meowr_command::Key;

/// The reference, as checked in. Not `~/docs`: that is not a git repository and
/// CI cannot reach it.
const REFERENCE: &str = include_str!("../../../rings/doom-leader.tsv");

/// Prefixes whose package will never exist here, and why.
///
/// Prose in a module header cannot be checked. This is the same list
/// `leader.rs` describes, as data, so that a row under one of these keys is
/// *dropped* rather than merely undiscussed.
/// A whole prefix, or one row, that will never be answered — and why.
///
/// Matched against the full key first and the prefix second, because dropping
/// is not always a whole subtree: Doom's search prefix is half project search,
/// which is bound, and half lookups into packages that will not exist here.
const DROPPED: &[(&str, &str)] = &[
    ("d", "dape; no debug adapter is planned"),
    ("n", "org-mode"),
    ("&", "yasnippet"),
    ("u", "one binding, into a package above"),
    ("m", "one binding, into a package above"),
    ("l", "one binding, into a package above"),
    ("h", "one binding, into a package above"),
    ("e", "one binding, into a package above"),
    ("a", "one binding, into a package above"),
    // Both were *reserved* until Guests were dropped: `v` for the `wagit`
    // Guest and `o` for the terminal one. With no native seam there is nothing
    // to host either, and a reservation nobody is coming back for is the thing
    // decision #12 calls worse than a drop.
    (
        "v",
        "magit; the Guest that would have hosted it went with ADR-0003's second seam",
    ),
    (
        "o",
        "Doom's open prefix: dired, vterm, treemacs and friends, none of them planned",
    ),
    // Doom's `:tools lookup` and friends, row by row (#97).
    ("s k", "Dash docsets"),
    ("s K", "Dash docsets"),
    ("s o", "online lookup"),
    ("s O", "online lookup"),
    ("s t", "dictionary lookup"),
    ("s T", "thesaurus lookup"),
    ("s f", "ffap: the file named at point"),
    ("s L", "ffap"),
    ("s l", "link-hint"),
    ("s m", "bookmarks; none are planned"),
    ("s e", "searching ~/.emacs.d, which meowr does not have"),
    // Doom's file prefix, row by row (#98). The rest of it is bound or held:
    // seven `unscheduled`, which is a note rather than a drop because meowr
    // would want them. The three M5 held are bound -- `f R` and `f Y` over the
    // Project's root (#148), and `f F` (#149), which #128 had found was never
    // a project row -- Doom's
    // `+default/find-file-under-here` is rooted at the current directory and
    // needs no root -- so its `project:` prefix was wrong rather than stale.
    ("f c", "editorconfig"),
    ("f d", "dired"),
    ("f l", "locate"),
    ("f u", "sudo; meowr does not re-open a file as another user"),
    ("f U", "sudo; meowr does not re-open a file as another user"),
    ("f e", "~/.emacs.d, which meowr does not have"),
    ("f E", "~/.emacs.d, which meowr does not have"),
    // Doom's fold prefix (#142). Six of its eight rows are meowr's; these two
    // delete a *vimish* fold, which only means something if folds are created
    // explicitly and persist. Nothing here creates one -- and nor does Doom in
    // this configuration, where `vimish-fold-global-mode` sits inside
    // `(modulep! :editor evil)` and never runs, and the only binding that
    // *creates* a vimish fold is evil's `zf`. Held until `C-f` became a prefix,
    // because the bucket below reads the head's reservation first and stopped
    // there; now each row is asked for itself, which is what ADR-0012 said
    // would happen and when.
    ("C-f C-d", "vimish folds; nothing here creates one to delete"),
    (
        "C-f C-a C-d",
        "vimish folds; nothing here creates one to delete",
    ),
    // Doom's quit prefix (#99). `q q` and `q Q` are bound; these three are
    // Emacs's own shape rather than meowr's.
    ("q K", "an Emacs daemon; `q q` is the quit here"),
    ("q d", "an Emacs server to restart"),
    ("q f", "frames; meowr is one terminal"),
    // Doom's toggle prefix (#100). Five of these are the terminal's business
    // or a package meowr does not have.
    (
        "t w",
        "soft wrapping; meowr clips a long line rather than wrapping it",
    ),
    ("t s", "flyspell"),
    ("t b", "font size; the terminal owns the font"),
    ("t F", "fullscreen; the terminal owns the frame"),
    (
        "t v",
        "visible-mode, which shows invisible text meowr has none of",
    ),
    // The loose rows (#104).
    (",", "dired"),
    // `ESC` is how the leader is left (#82), so no row beginning with it can
    // be reached from here at all — a stronger reason than either command's
    // own. `magit-file-dispatch` would have gone with the Guest seam anyway,
    // and `consult-mode-command` wants major modes meowr does not have.
    (
        "ESC g",
        "unreachable: `ESC` leaves the leader, and magit went with the native seam",
    ),
    (
        "ESC x",
        "unreachable: `ESC` leaves the leader; `M-x` is how a command is named",
    ),
];

/// Rows answered somewhere other than the leader tree.
///
/// The entry that exists because of the mistake this whole effort kept making.
/// `SPC 0`-`9` are `meow-digit-argument`, and `leader.rs` does not seed them
/// because they are not leader *entries*: `keypad.rs` answers a digit wherever
/// the walk is, exactly as `meow--keypad-try-execute` does. A diff of the seed
/// table reports them missing, which is the same shape of wrong as a diff of
/// command names reporting `BEACON_NOOPS` missing.
///
/// These are **bound** — the bucket is not a fifth one. This table is only the
/// evidence of where, and the behaviour is proved rather than asserted, by
/// `session::a_digit_under_the_leader_leaves_it_and_arms_a_count`.
const ELSEWHERE: &[(&str, &str)] = &[
    ("0", "keypad.rs"),
    ("1", "keypad.rs"),
    ("2", "keypad.rs"),
    ("3", "keypad.rs"),
    ("4", "keypad.rs"),
    ("5", "keypad.rs"),
    ("6", "keypad.rs"),
    ("7", "keypad.rs"),
    ("8", "keypad.rs"),
    ("9", "keypad.rs"),
];

/// Rows nobody has decided about yet, each with the ticket that will.
///
/// Graduated from `~/docs/meowr-ring-coverage.md` by #95. Deleting a ticket's
/// rows without binding, holding or dropping them fails this suite, which is
/// the point: the list may only shrink downward into the other three buckets.
const OPEN: &[(&str, &str)] = &[
    // Empty since #104. Kept, not deleted: the mechanism is how the next batch
    // of rows gets parked where it can be seen, and `ring_two_is_fully_decided`
    // is what makes parking one a deliberate act rather than a quiet one.
];

/// Where a row is accounted for.
///
/// A type rather than a string: the second test asks whether a prefix has any
/// `Open` rows left, and a mistyped `"open"` there would be a comparison that
/// can never be true rather than one the compiler refuses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Bucket {
    /// meowr answers it -- in the leader tree, or in `ELSEWHERE`.
    Bound,
    /// A milestone owns the key. Reserved, never reassigned.
    Held,
    /// The package will never exist here, and `DROPPED` says why.
    Dropped,
    /// Nobody has decided, and `OPEN` names the ticket that will.
    Open,
}

/// One reference row.
struct Row {
    key: String,
    command: String,
}

fn rows() -> Vec<Row> {
    REFERENCE
        .lines()
        .filter(|line| !line.starts_with('#') && !line.is_empty())
        .filter_map(|line| {
            let mut fields = line.split('\t');
            let key = fields.next()?.to_string();
            let command = fields.next()?.to_string();
            // `PREFIX` is the tree's shape, not a binding to answer for.
            (command != "PREFIX").then_some(Row { key, command })
        })
        .collect()
}

/// The reference's key notation, as `Key`s.
///
/// `SPC` and `RET` are spelled out, `C-` and `M-` are modifiers, and everything
/// else is the character itself. `ESC g` is the reference's way of writing
/// `M-g`, which is why it is a two-key row rather than a modifier.
fn keys_of(spelling: &str) -> Option<Vec<Key>> {
    spelling
        .split(' ')
        .map(|part| match part {
            "SPC" => Some(Key::Char(' ')),
            "RET" => Some(Key::Enter),
            "TAB" => Some(Key::Tab),
            "DEL" => Some(Key::Backspace),
            "ESC" => Some(Key::Esc),
            _ => {
                let mut chars = part.chars();
                match (chars.next(), chars.as_str()) {
                    (Some('C'), rest) if rest.starts_with('-') && rest.len() == 2 => {
                        rest.chars().nth(1).map(Key::Ctrl)
                    }
                    (Some('M'), rest) if rest.starts_with('-') && rest.len() == 2 => {
                        rest.chars().nth(1).map(Key::Alt)
                    }
                    (Some(c), "") => Some(Key::Char(c)),
                    _ => None,
                }
            }
        })
        .collect()
}

/// Which bucket a row is in, or `Err` with why it is in none.
fn bucket(
    leader: &Leader,
    registry: &meowr_command::Registry,
    row: &Row,
) -> Result<Bucket, String> {
    let head = row.key.split(' ').next().unwrap_or_default();
    if let Some(keys) = keys_of(&row.key) {
        match leader.lookup(&keys) {
            Some(Entry::Command(name)) => {
                return if registry.get(name).is_some() {
                    Ok(Bucket::Bound)
                } else {
                    Err(format!(
                        "`{}` is bound to `{name}`, which the registry does not have",
                        row.key
                    ))
                };
            }
            // A prefix meowr fills differently is not this test's business: the
            // rows under it are each asked for themselves.
            Some(Entry::Prefix(_)) => return Ok(Bucket::Bound),
            Some(Entry::Reserved(note)) => {
                // The same rule the leader's own tests apply, read from the
                // one list: a note must say *when*, and `unscheduled` is one
                // of the ways to say it.
                return if RESERVATION_OPENINGS.iter().any(|o| note.starts_with(o)) {
                    Ok(Bucket::Held)
                } else {
                    Err(format!(
                        "`{}` is reserved by `{note}`, which does not say when",
                        row.key
                    ))
                };
            }
            None => {}
        }
    }
    // Not in the tree. It must be deliberately absent -- or answered somewhere
    // the tree cannot see.
    if ELSEWHERE.iter().any(|(key, _)| *key == row.key) {
        return Ok(Bucket::Bound);
    }
    if let Some(head_keys) = keys_of(head)
        && leader
            .lookup(&head_keys)
            .is_some_and(|e| matches!(e, Entry::Reserved(_)))
    {
        return Ok(Bucket::Held);
    }
    if DROPPED
        .iter()
        .any(|(key, _)| *key == row.key || *key == head)
    {
        return Ok(Bucket::Dropped);
    }
    if OPEN.iter().any(|(prefix, _)| *prefix == head) {
        return Ok(Bucket::Open);
    }
    Err(format!(
        "`{}` ({}) is in no bucket: not bound, not held, not dropped, not open",
        row.key, row.command
    ))
}

#[test]
fn every_leader_row_is_bound_held_dropped_or_open() {
    let leader = Leader::new();
    let registry = meowr_app::default_registry();
    let rows = rows();
    assert!(rows.len() > 200, "the reference has {} rows", rows.len());

    let mut counts = std::collections::BTreeMap::new();
    let mut unaccounted = Vec::new();
    for row in &rows {
        match bucket(&leader, &registry, row) {
            Ok(found) => *counts.entry(found).or_insert(0) += 1,
            Err(why) => unaccounted.push(why),
        }
    }

    assert!(
        unaccounted.is_empty(),
        "{} of {} leader rows are unaccounted for:\n  {}",
        unaccounted.len(),
        rows.len(),
        unaccounted.join("\n  ")
    );
    println!("{counts:?}");
}

#[test]
fn ring_two_is_fully_decided() {
    // What #97 through #104 add up to: every row of the author's leader is
    // bound, held with a note that says when, or dropped with a reason. None
    // is merely undiscussed.
    //
    // This fails if a row is ever parked in `OPEN` again. That is allowed — a
    // regenerated table could bring a batch too large to decide in one sitting
    // — but it should be a deliberate act with the reason written down, the way
    // the reservation count is. Amend this test, do not route around it.
    assert!(
        OPEN.is_empty(),
        "these rows are parked rather than decided: {OPEN:?}"
    );
}

#[test]
fn every_open_ticket_still_has_rows_to_answer_for() {
    // The half that makes the list shrink rather than linger. A ticket whose
    // rows have all been bound, held or dropped has nothing left to do, and its
    // entry here is then a lie that would quietly swallow the next new row
    // under the same prefix.
    let leader = Leader::new();
    let registry = meowr_app::default_registry();
    let rows = rows();
    for (prefix, ticket) in OPEN {
        let still_open = rows.iter().any(|row| {
            row.key.split(' ').next() == Some(prefix)
                && matches!(bucket(&leader, &registry, row), Ok(Bucket::Open))
        });
        assert!(
            still_open,
            "`{prefix}` ({ticket}) has no open rows left; delete its entry"
        );
    }
}
