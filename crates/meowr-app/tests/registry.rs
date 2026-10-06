//! Every command in the table, dispatched with no key.
//!
//! Not in `session.rs`, and the reason is that file's own opening paragraph:
//! it is session behaviour end to end, and reaching past the session into
//! internals is what it exists not to do. This reaches past nothing — it is
//! `default_registry`, `Ctx` and `Registry::invoke`, all public — but it is a
//! seam below the session, and `wagit`'s `DEVELOPING.md` separates the two on
//! purpose. The witnesses that this behaviour survives the trip to the Echo
//! Area stay in `session.rs`, where they belong.

use meowr_command::{Argument, Ctx, Options, Outcome, State};
use meowr_core::edit::KillRing;
use meowr_core::thing::Things;
use meowr_core::undo::History;
use meowr_core::{Axis, Kind, Selection, SelectionState};
use meowr_domain::Document;

/// Everything a command needs, with no key in it.
///
/// A near-copy of the fixture in `meowr-command`'s own tests, which is
/// `#[cfg(test)]` in another crate and so cannot be shared without exporting
/// test scaffolding from a domain crate. The copy is the cheaper of the two.
struct Keyless {
    doc: Document,
    selection: SelectionState,
    state: State,
    next: Argument,
    kill_ring: KillRing,
    history: History,
    things: Things,
    options: Options,
}

impl Keyless {
    fn new(text: &str, selection: Selection) -> Self {
        Self {
            doc: Document::from_text(text),
            selection: SelectionState::new(selection),
            state: State::Normal,
            next: Argument::None,
            kill_ring: KillRing::new(),
            history: History::new(),
            things: Things::new(),
            options: Options::default(),
        }
    }

    fn ctx(&mut self) -> Ctx<'_> {
        Ctx {
            doc: &mut self.doc,
            selection: &mut self.selection,
            state: &mut self.state,
            argument: Argument::None,
            next_argument: &mut self.next,
            options: &self.options,
            kill_ring: &mut self.kill_ring,
            history: &mut self.history,
            things: &self.things,
            text_objects: None,
            window: None,
            last_command: None,
            // The whole point.
            key: None,
        }
    }
}

/// The buffer states a command can be asked to work in.
///
/// More than one, because the first version of this swept a single healthy
/// buffer and called itself "no command beeps wordlessly" on that evidence.
/// Three commands were beeping at the ends of the buffer the whole time and
/// the sweep could not see them, which is the same shape as #80 pressing keys
/// in a buffer where every key had something to do.
///
/// Descriptions rather than fixtures, because a fixture has to be **fresh for
/// every command**: they edit, and `Registry::names` walks a hash map, so a
/// shared one made the answer depend on the order the names came out in. The
/// first draft of this test reported a different set of offenders on
/// consecutive runs, which is how that was found.
struct Where {
    /// How a failure reads when this state is the one that produced it.
    what: &'static str,
    text: &'static str,
    /// `None` for no selection, with the point at the end of the text.
    span: Option<(usize, usize)>,
}

const fn state(what: &'static str, text: &'static str, span: Option<(usize, usize)>) -> Where {
    Where { what, text, span }
}

const STATES: [Where; 5] = [
    state(
        "a selection",
        "alpha (beta) gamma\nsecond line\n",
        Some((0, 5)),
    ),
    state("no selection", "alpha beta\n", None),
    state("an empty buffer", "", None),
    state("the end of the buffer", "alpha\n", None),
    state("a selection at the end", "alpha\n", Some((6, 6))),
];

fn fixture(text: &str, span: Option<(usize, usize)>) -> Keyless {
    let selection = match span {
        Some((mark, pos)) => Selection::typed(Axis::Select, Kind::TRANSIENT, mark, pos),
        None => Selection::at(text.len()),
    };
    Keyless::new(text, selection)
}

#[test]
fn no_command_beeps_wordlessly_when_it_is_dispatched_without_a_key() {
    // `Outcome::Failed` *is* the wordless beep: the session signals and puts
    // nothing in the Echo Area, so the user is told that something did not
    // happen and never told what. #80 removed nine of them by pressing every
    // key the Normal keymap binds. These are the ones behind a config, which
    // no keypress reaches — a composition and a hook both arrive with no key
    // at all, so there is no key to press.
    //
    // A sweep rather than a case per command, so a command added later is
    // covered without anyone remembering to add one.
    //
    // **What it cannot see.** `Failed` is the only shape it catches. A command
    // that answers `Handled` having done nothing is invisible from out here,
    // because it looks exactly like one that correctly had nothing to do —
    // `digit-argument` was that, and was found by reading every `ctx.key` in
    // the table rather than by this. #88 asked for the enumeration *and* the
    // sweep for that reason; neither subsumes the other. The readers, and what
    // each does with no key:
    //
    //   `self-insert`      refuses          (#78)
    //   `expand`           counts 1         a deliberate default
    //   `digit-argument`   refuses          (#88, invisible to this sweep)
    //   `*-of-thing` × 4   refuse           (#78)
    //   `surround-*` × 4   no longer read the key at all (#88)
    let reg = meowr_app::default_registry();
    let names: Vec<String> = reg.names().map(str::to_owned).collect();
    assert!(
        names.len() > 100,
        "the sweep must actually reach the table, and reached {}",
        names.len()
    );

    let mut wordless: Vec<String> = Vec::new();
    for state in STATES {
        for name in &names {
            // Fresh every time: the command before this one may have edited.
            let mut fixture = fixture(state.text, state.span);
            if let Some(Outcome::Failed) = reg.invoke(name, &mut fixture.ctx()) {
                wordless.push(format!("`{name}` with {}", state.what));
            }
        }
    }
    wordless.sort();

    assert!(
        wordless.is_empty(),
        "these beep and say nothing with no key: {wordless:#?}"
    );
}
