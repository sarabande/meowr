use super::*;
use meowr_core::edit::KillRing;
use meowr_core::thing::Things;
use meowr_core::undo::History;
use meowr_core::{Selection, SelectionState};
use meowr_domain::Document;

fn noop(_: &mut Ctx<'_>) -> Outcome {
    Outcome::Handled
}

fn insert_bang(ctx: &mut Ctx<'_>) -> Outcome {
    let pos = ctx.selection.current().pos();
    let added = ctx.doc.insert(pos, "!").unwrap();
    ctx.selection
        .select_without_history(Selection::at(pos + added));
    Outcome::Handled
}

fn registry() -> Registry {
    let mut reg = Registry::new();
    reg.register(
        Command::new("insert-bang", Handler::Native(insert_bang))
            .described("Insert an exclamation mark")
            .in_states(&[State::Insert]),
    );
    reg.register(
        Command::new("do-nothing", Handler::Native(noop))
            .described("Do nothing")
            .taking_count(),
    );
    reg
}

#[test]
fn a_command_is_found_by_name() {
    let reg = registry();

    assert!(reg.get("insert-bang").is_some());
    assert!(reg.get("no-such-command").is_none());
}

#[test]
fn a_command_carries_the_metadata_the_leader_menu_needs() {
    let reg = registry();
    let cmd = reg.get("insert-bang").unwrap();

    assert_eq!(cmd.description(), "Insert an exclamation mark");
    assert!(!cmd.takes_count());
    assert!(reg.get("do-nothing").unwrap().takes_count());
}

#[test]
fn a_command_declares_which_states_it_is_valid_in() {
    let reg = registry();

    assert!(reg.get("insert-bang").unwrap().valid_in(State::Insert));
    assert!(!reg.get("insert-bang").unwrap().valid_in(State::Normal));
    // A command that names no states is valid everywhere.
    assert!(reg.get("do-nothing").unwrap().valid_in(State::Normal));
    assert!(reg.get("do-nothing").unwrap().valid_in(State::Beacon));
}

#[test]
fn a_command_can_be_invoked_by_name_without_going_through_a_key() {
    let reg = registry();
    let mut doc = Document::from_text("ab");
    let mut selection = SelectionState::new(Selection::at(1));
    let mut state = State::Insert;

    let outcome = reg.invoke(
        "insert-bang",
        &mut Ctx {
            doc: &mut doc,
            selection: &mut selection,
            state: &mut state,
            argument: Argument::None,
            next_argument: &mut Argument::None,
            options: &Options::default(),
            kill_ring: &mut KillRing::new(),
            history: &mut History::new(),
            things: &Things::new(),
            text_objects: None,
            window: None,
            last_command: None,
            key: None,
        },
    );

    assert_eq!(outcome, Some(Outcome::Handled));
    assert_eq!(doc.text().to_string(), "a!b");
    assert_eq!(selection.current().pos(), 2);
}

#[test]
fn invoking_an_unknown_command_reports_that_rather_than_panicking() {
    let reg = registry();
    let mut doc = Document::default();
    let mut selection = SelectionState::new(Selection::at(0));
    let mut state = State::Normal;

    let outcome = reg.invoke(
        "no-such-command",
        &mut Ctx {
            doc: &mut doc,
            selection: &mut selection,
            state: &mut state,
            argument: Argument::None,
            next_argument: &mut Argument::None,
            options: &Options::default(),
            kill_ring: &mut KillRing::new(),
            history: &mut History::new(),
            things: &Things::new(),
            text_objects: None,
            window: None,
            last_command: None,
            key: None,
        },
    );

    assert_eq!(outcome, None);
}

#[test]
fn the_registry_can_be_enumerated_by_name_alone() {
    // This is what a plugin binding will do in M2: discover what exists
    // without compile-time knowledge of which commands are present.
    let reg = registry();
    let mut names: Vec<_> = reg.names().collect();
    names.sort_unstable();

    assert_eq!(names, ["do-nothing", "insert-bang"]);
}

fn keymap() -> Keymap {
    let mut map = Keymap::new();
    map.bind(State::Insert, &[Key::Esc], "leave-insert")
        .unwrap();
    map.bind(State::Normal, &[Key::Char('s')], "enter-insert")
        .unwrap();
    map.bind(State::Normal, &[Key::Char('g'), Key::Char('g')], "go-top")
        .unwrap();
    map.bind(State::Normal, &[Key::Char('g'), Key::Char('e')], "go-end")
        .unwrap();
    map
}

#[test]
fn a_single_key_resolves_to_its_command() {
    let map = keymap();

    assert_eq!(
        map.resolve(State::Normal, &[Key::Char('s')]),
        Resolution::Command("enter-insert")
    );
}

#[test]
fn an_unbound_key_resolves_to_unbound() {
    assert_eq!(
        keymap().resolve(State::Normal, &[Key::Char('q')]),
        Resolution::Unbound
    );
}

#[test]
fn a_prefix_is_pending_until_the_sequence_completes() {
    let map = keymap();

    assert_eq!(
        map.resolve(State::Normal, &[Key::Char('g')]),
        Resolution::Pending
    );
    assert_eq!(
        map.resolve(State::Normal, &[Key::Char('g'), Key::Char('e')]),
        Resolution::Command("go-end")
    );
}

#[test]
fn a_sequence_that_leaves_the_bound_paths_is_unbound_not_pending() {
    assert_eq!(
        keymap().resolve(State::Normal, &[Key::Char('g'), Key::Char('z')]),
        Resolution::Unbound
    );
}

#[test]
fn each_state_has_its_own_bindings() {
    let map = keymap();

    assert_eq!(
        map.resolve(State::Insert, &[Key::Esc]),
        Resolution::Command("leave-insert")
    );
    // The same key means nothing in Normal State, where nothing bound it.
    assert_eq!(map.resolve(State::Normal, &[Key::Esc]), Resolution::Unbound);
}

#[test]
fn a_binding_that_would_make_a_sequence_ambiguous_is_refused() {
    let mut map = keymap();

    // `g` is already a prefix of `g g`; binding it as a command too would make
    // resolving `g` ambiguous, so it is refused rather than silently shadowing.
    assert!(map.bind(State::Normal, &[Key::Char('g')], "go").is_err());
    // And the reverse: extending an existing leaf binding.
    assert!(
        map.bind(State::Normal, &[Key::Char('s'), Key::Char('x')], "nope")
            .is_err()
    );
    // The existing bindings are untouched by a refused bind.
    assert_eq!(
        map.resolve(State::Normal, &[Key::Char('g')]),
        Resolution::Pending
    );
}

#[test]
fn rebinding_the_same_sequence_replaces_it() {
    let mut map = keymap();

    map.bind(State::Normal, &[Key::Char('s')], "something-else")
        .unwrap();

    assert_eq!(
        map.resolve(State::Normal, &[Key::Char('s')]),
        Resolution::Command("something-else")
    );
}

#[test]
fn a_command_declares_whether_it_changes_the_buffer() {
    let mut reg = Registry::new();
    reg.register(Command::new("wreck", Handler::Native(noop)).mutating());
    reg.register(Command::new("look", Handler::Native(noop)));

    assert!(reg.get("wreck").unwrap().is_mutating());
    assert!(!reg.get("look").unwrap().is_mutating());
}

#[test]
fn the_fallback_table_is_data() {
    let mut fallbacks = Fallbacks::new();
    fallbacks.set("meow-kill", "meow-C-k");

    assert_eq!(fallbacks.get("meow-kill"), Some("meow-C-k"));
    assert_eq!(fallbacks.get("meow-save"), None);
}

// --- numeric arguments ----------------------------------------------------
//
// These cannot be covered by the differential harness: neither way of
// replaying keys in batch Emacs can carry a prefix argument, which is recorded
// at length in the fidelity case generator. So they are written from the
// behaviour of `digit-argument` and `negative-argument` directly.

#[test]
fn digits_accumulate_left_to_right() {
    let arg = Argument::None.digit(1).digit(2).digit(3);
    assert_eq!(arg.value(), Some(123));
}

#[test]
fn a_digit_after_a_minus_is_the_negative_of_that_digit() {
    // Not -1 then 2 making -12: Emacs's `negative-argument` leaves a marker
    // rather than the number -1, and `digit-argument` reads the marker.
    assert_eq!(Argument::None.negate().digit(2).value(), Some(-2));
    assert_eq!(Argument::None.negate().digit(2).digit(5).value(), Some(-25));
}

#[test]
fn a_bare_minus_means_minus_one() {
    assert_eq!(Argument::None.negate().value(), Some(-1));
}

#[test]
fn minus_twice_cancels() {
    assert_eq!(Argument::None.negate().negate().value(), None);
}

#[test]
fn minus_after_a_count_negates_the_count() {
    assert_eq!(Argument::Count(12).negate().value(), Some(-12));
}

#[test]
fn no_argument_reads_as_none_rather_than_one() {
    // The distinction matters: a command that takes a count needs to know
    // whether one was *given*, because Meow's `meow-expand` uses the key's
    // digit when there is none.
    assert_eq!(Argument::None.value(), None);
}

#[test]
fn a_held_down_digit_key_saturates_rather_than_overflowing() {
    // The digit keys repeat. Twelve of them overflow an i32, and a panic in
    // the editor's own key handler is not an acceptable answer to leaning on
    // the keyboard.
    let mut arg = Argument::None;
    for _ in 0..12 {
        arg = arg.digit(9);
    }
    assert_eq!(arg.value(), Some(i32::MAX));
}

// --- the universal argument -----------------------------------------------

#[test]
fn a_universal_argument_carries_both_readings() {
    // Emacs's raw prefix argument for `C-u` is the list `(4)`:
    // `prefix-numeric-value` reads it as four, and
    // `meow--with-universal-argument-p` compares it against `'(4)` exactly.
    let once = Argument::None.universal();
    assert_eq!(once.value(), Some(4), "prefix-numeric-value");
    assert!(once.is_universal(), "(equal '(4) arg)");

    let twice = once.universal();
    assert_eq!(twice.value(), Some(16), "C-u C-u is (16)");
    assert!(
        !twice.is_universal(),
        "two is not more universal, it is a different argument"
    );
}

#[test]
fn a_digit_after_a_universal_argument_replaces_it() {
    // `C-u 5` is five, not fifty-four.
    assert_eq!(Argument::None.universal().digit(5).value(), Some(5));
    assert_eq!(
        Argument::None.universal().digit(5).digit(3),
        Argument::Count(53)
    );
}

#[test]
fn a_minus_after_a_universal_argument_is_a_bare_minus() {
    assert_eq!(Argument::None.universal().negate(), Argument::Negative);
}

#[test]
fn a_universal_argument_after_a_count_starts_again() {
    // `universal-argument` sets `prefix-arg` outright rather than folding
    // into what was there.
    assert_eq!(Argument::Count(7).universal(), Argument::Universal(1));
}

#[test]
fn holding_c_u_down_saturates_rather_than_overflowing() {
    // The same reason `digit` saturates: the key repeats, and four to the
    // sixteenth does not fit.
    let mut argument = Argument::None;
    for _ in 0..40 {
        argument = argument.universal();
    }
    assert_eq!(argument.value(), Some(i32::MAX));
}

// --- reading a key sequence back (#54) -------------------------------------

#[test]
fn a_key_sequence_reads_back_the_way_it_is_written() {
    assert_eq!(
        parse_keys("SPC f f").expect("a leader sequence"),
        vec![Key::Char(' '), Key::Char('f'), Key::Char('f')]
    );
    assert_eq!(
        parse_keys("C-x C-f").expect("a chord sequence"),
        vec![Key::Ctrl('x'), Key::Ctrl('f')]
    );
    assert_eq!(parse_keys("M-'").expect("a meta key"), vec![Key::Alt('\'')]);
    assert_eq!(
        parse_keys("ESC RET DEL").expect("the named keys"),
        vec![Key::Esc, Key::Enter, Key::Backspace]
    );
}

#[test]
fn every_key_survives_being_written_and_read_again() {
    // The reader is the writer's inverse, and this is what stops the two
    // drifting apart. Not a handful of examples: every variant, and for the
    // ones that carry a character, every character that could embarrass them —
    // the modifier letters themselves, the separator, and two that are not
    // ASCII at all.
    let awkward = [
        'a', 'Z', '0', '-', '\'', '"', '\\', 'C', 'M', 'x', ' ', 'é', '中',
    ];
    // Built through a `match` on one of each variant, so that adding a `Key`
    // will not compile until it is covered here. Without that, "every variant"
    // is a claim the list makes and nothing checks.
    let mut every: Vec<Key> = Vec::new();
    for variant in [
        Key::Char('a'),
        Key::Ctrl('a'),
        Key::Alt('a'),
        Key::Esc,
        Key::Enter,
        Key::Backspace,
        Key::Tab,
        Key::BackTab,
        Key::Up,
        Key::Down,
        Key::Left,
        Key::Right,
        Key::PageUp,
        Key::PageDown,
    ] {
        match variant {
            Key::Char(_) => every.extend(awkward.iter().copied().map(Key::Char)),
            Key::Ctrl(_) => every.extend(awkward.iter().copied().map(Key::Ctrl)),
            Key::Alt(_) => every.extend(awkward.iter().copied().map(Key::Alt)),
            Key::Esc => every.push(Key::Esc),
            Key::Enter => every.push(Key::Enter),
            Key::Backspace => every.push(Key::Backspace),
            // Carry no character, so one each.
            Key::Tab => every.push(Key::Tab),
            Key::BackTab => every.push(Key::BackTab),
            Key::Up => every.push(Key::Up),
            Key::Down => every.push(Key::Down),
            Key::Left => every.push(Key::Left),
            Key::Right => every.push(Key::Right),
            Key::PageUp => every.push(Key::PageUp),
            Key::PageDown => every.push(Key::PageDown),
        }
    }

    for key in &every {
        let written = format!("{key}");
        let read = parse_keys(&written)
            .unwrap_or_else(|e| panic!("{key:?} wrote {written:?}, which did not read back: {e}"));
        assert_eq!(read, vec![*key], "{key:?} wrote {written:?}");
    }

    // And the whole lot as one sequence, which is what a config writes.
    let written = format!("{}", Keys(&every));
    assert_eq!(parse_keys(&written).expect("the sequence"), every);
}

#[test]
fn an_unreadable_key_says_which_part_failed() {
    let e = parse_keys("SPC nope f").expect_err("`nope` is not a key");
    assert!(format!("{e}").contains("nope"), "{e}");
}

#[test]
fn an_empty_sequence_is_not_a_sequence() {
    assert!(parse_keys("   ").is_err(), "nothing to bind");
}

// --- what a config will ask of the registries (#55) ------------------------

#[test]
fn a_key_can_be_unbound() {
    let mut keymap = Keymap::new();
    keymap
        .bind(State::Normal, &parse_keys("g g").unwrap(), "top")
        .unwrap();
    assert!(matches!(
        keymap.resolve(State::Normal, &parse_keys("g g").unwrap()),
        Resolution::Command(name) if name == "top"
    ));

    assert!(keymap.unbind(State::Normal, &parse_keys("g g").unwrap()));

    assert!(matches!(
        keymap.resolve(State::Normal, &parse_keys("g g").unwrap()),
        Resolution::Unbound
    ));
}

#[test]
fn unbinding_retracts_a_prefix_that_leads_nowhere() {
    // The half that is easy to forget: `g` stays a prefix after `g g` is gone,
    // so the key reads as dead — every press pending, nothing ever resolving —
    // rather than as unbound.
    let mut keymap = Keymap::new();
    keymap
        .bind(State::Normal, &parse_keys("g g").unwrap(), "top")
        .unwrap();

    keymap.unbind(State::Normal, &parse_keys("g g").unwrap());

    assert!(
        matches!(
            keymap.resolve(State::Normal, &parse_keys("g").unwrap()),
            Resolution::Unbound
        ),
        "`g` no longer leads anywhere"
    );
}

#[test]
fn unbinding_keeps_a_prefix_that_still_leads_somewhere() {
    let mut keymap = Keymap::new();
    keymap
        .bind(State::Normal, &parse_keys("g g").unwrap(), "top")
        .unwrap();
    keymap
        .bind(State::Normal, &parse_keys("g e").unwrap(), "bottom")
        .unwrap();

    keymap.unbind(State::Normal, &parse_keys("g g").unwrap());

    assert!(
        matches!(
            keymap.resolve(State::Normal, &parse_keys("g").unwrap()),
            Resolution::Pending
        ),
        "`g e` is still below it"
    );
}

#[test]
fn unbinding_a_key_nobody_bound_says_so_rather_than_pretending() {
    let mut keymap = Keymap::new();
    assert!(!keymap.unbind(State::Normal, &parse_keys("g g").unwrap()));
}

/// Hooks at two points under one name, each recording that it ran.
fn counting_hooks(ran: &std::rc::Rc<std::cell::RefCell<Vec<HookPoint>>>) -> Hooks {
    let mut hooks = Hooks::new();
    for point in [HookPoint::BeforeSave, HookPoint::AfterSave] {
        let ran = std::rc::Rc::clone(ran);
        hooks.add("format", point, move |_| {
            ran.borrow_mut().push(point);
            Proceed::Yes
        });
    }
    hooks
}

#[test]
fn one_command_can_be_hooked_at_two_points_and_both_fire() {
    // `Hooks::add` removed by name alone, so the second add ate the first —
    // silently, which is the worst way for a config to be wrong. Counting the
    // names is not enough: they have to run.
    let ran = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let hooks = counting_hooks(&ran);
    let mut fixture = Fixture::new("");

    hooks.run(HookPoint::BeforeSave, &mut fixture.ctx());
    hooks.run(HookPoint::AfterSave, &mut fixture.ctx());

    assert_eq!(
        *ran.borrow(),
        vec![HookPoint::BeforeSave, HookPoint::AfterSave]
    );
}

#[test]
fn removing_a_hook_removes_it_from_the_point_named_and_no_other() {
    // Counting survivors is not enough either: removing the *wrong* point
    // leaves exactly the same count behind.
    let ran = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let mut hooks = counting_hooks(&ran);

    assert!(hooks.remove(HookPoint::BeforeSave, "format"));

    let mut fixture = Fixture::new("");
    hooks.run(HookPoint::BeforeSave, &mut fixture.ctx());
    hooks.run(HookPoint::AfterSave, &mut fixture.ctx());
    assert_eq!(
        *ran.borrow(),
        vec![HookPoint::AfterSave],
        "the one that was named is gone, and only that one"
    );
}

#[test]
fn adding_the_same_hook_twice_at_one_point_replaces_it() {
    let mut hooks = Hooks::new();
    hooks.add("format", HookPoint::AfterSave, |_| Proceed::Yes);
    hooks.add("format", HookPoint::AfterSave, |_| Proceed::No);

    assert_eq!(hooks.names().filter(|n| *n == "format").count(), 1);
}

#[test]
fn registering_a_name_that_already_exists_is_refused() {
    // A config defining `kill-line` would otherwise replace the native one
    // with no indication at all — worse than the re-registration #48 declined,
    // because it cannot even call through.
    let mut reg = registry();
    let taken = Command::new("insert-bang", Handler::Native(noop));

    let refused = reg.register_new(taken);

    let e = refused.expect_err("the name is taken");
    assert!(format!("{e}").contains("insert-bang"), "{e}");
}

// --- a command that is a list of commands (#56) ----------------------------

/// The owned state a `Ctx` borrows, so a test can make one in a line.
///
/// A struct rather than eight parameters: they always travel together, and
/// they are the same eight every time.
struct Fixture {
    doc: Document,
    selection: SelectionState,
    state: State,
    next: Argument,
    kill_ring: KillRing,
    history: History,
    things: Things,
    options: Options,
    /// The key that triggered the command under test, when a key did.
    key: Option<Key>,
}

impl Fixture {
    fn new(text: &str) -> Self {
        Self {
            doc: Document::from_text(text),
            selection: SelectionState::new(Selection::at(0)),
            state: State::Normal,
            next: Argument::None,
            kill_ring: KillRing::new(),
            history: History::new(),
            things: Things::new(),
            options: Options::default(),
            key: None,
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
            key: self.key,
        }
    }
}

// --- options --------------------------------------------------------------

#[test]
fn project_scope_is_on_by_default_and_can_be_turned_off() {
    // ADR-0010 declines to make *what* a Project is configurable and leaves
    // exactly this: a boolean, because two states are both implementable and
    // both testable, where a marker list would promise roots meowr has no
    // directory to exercise.
    assert!(Options::default().project_scope);

    let mut options = Options::default();
    options
        .set("project-scope", OptionValue::Bool(false))
        .expect("`project-scope` takes a boolean");

    assert!(!options.project_scope);
}

#[test]
fn every_listed_option_name_actually_sets_something() {
    // The table and the list are two halves of one thing, and nothing in the
    // compiler makes them agree. This is the check that a field added without
    // a name, or a name left behind by a field, is caught here rather than by
    // someone whose `init.scm` silently did nothing.
    for name in Options::NAMES {
        // Each candidate against a *fresh* `Options`, and both booleans among
        // them. Reusing one and stopping at the first `Ok` said "every bool
        // Option defaults to on" without meaning to: `hover-on-rest` defaults
        // to off, so setting it to `#f` is accepted and changes nothing, which
        // is the name reaching its field rather than missing it.
        let written: Vec<Options> = [
            OptionValue::Bool(false),
            OptionValue::Bool(true),
            OptionValue::Int(7),
            OptionValue::Symbol("expand".into()),
        ]
        .into_iter()
        .filter_map(|value| {
            let mut options = Options::default();
            options.set(name, value).ok().map(|()| options)
        })
        .collect();

        assert!(
            !written.is_empty(),
            "`{name}` is listed but takes no value meowr writes"
        );
        assert!(
            written.iter().any(|written| *written != Options::default()),
            "`{name}` was accepted and changed nothing"
        );
    }
}

#[test]
fn the_listed_names_are_the_ones_an_unknown_name_is_told_about() {
    let mut options = Options::default();

    let said = options
        .set("preview-dealy", OptionValue::Int(400))
        .expect_err("no such option");

    for name in Options::NAMES {
        assert!(said.contains(name), "`{name}` is not in `{said}`");
    }
}

#[test]
fn an_option_refused_leaves_the_rest_of_the_options_alone() {
    // `set` takes `&mut self`, so a partial write is the shape of mistake to
    // rule out: the field is assigned from an already-checked value or not at
    // all.
    let mut options = Options::default();

    assert!(
        options
            .set("preview-delay", OptionValue::Bool(true))
            .is_err()
    );

    assert_eq!(options, Options::default());
}

#[test]
fn documentation_on_rest_is_off_until_a_config_asks_for_it() {
    // `lsp-ui-doc-show-with-cursor` is nil in lsp-ui's own defaults and Doom
    // leaves it there; the reference configuration sets it explicitly
    // (`config-ui.el:17`). That is the whole reason this Option is off rather
    // than on -- `auto-save`'s docstring states the same rule reaching the
    // opposite answer, because Doom turns *that* one on for everybody.
    let options = Options::default();
    assert!(!options.hover_on_rest);
    // `lsp-ui-doc-delay`, to the millisecond.
    assert_eq!(options.hover_delay, Duration::from_millis(200));
}

// --- command-hooks (#64) --------------------------------------------------

#[test]
fn a_point_can_hold_a_command_name_beside_its_closures() {
    // A hook that names a command cannot run it: a hook is
    // `Fn(&mut Ctx) -> Proceed` and the registry is not reachable from inside
    // one. So the name is *held* here and dispatched by the session once the
    // closures have run and the borrow they needed is released.
    let mut hooks = Hooks::new();

    hooks.add_command(HookPoint::AfterSave, "my-command");

    assert_eq!(hooks.commands(HookPoint::AfterSave), vec!["my-command"]);
    assert!(hooks.commands(HookPoint::StateChange).is_empty());
}

#[test]
fn the_same_command_hooked_twice_at_one_point_is_held_once() {
    // A config re-read, or one that says the same thing twice, should not
    // make the command run twice — the same reason `add` replaces by point
    // and name rather than appending.
    let mut hooks = Hooks::new();

    hooks.add_command(HookPoint::AfterSave, "my-command");
    hooks.add_command(HookPoint::AfterSave, "my-command");

    assert_eq!(hooks.commands(HookPoint::AfterSave).len(), 1);
}

#[test]
fn one_command_may_be_hooked_at_two_points() {
    // The bug `remove` already guards for closures: keyed by point *and*
    // name, so the second add does not silently delete the first.
    let mut hooks = Hooks::new();

    hooks.add_command(HookPoint::AfterSave, "my-command");
    hooks.add_command(HookPoint::StateChange, "my-command");

    assert_eq!(hooks.commands(HookPoint::AfterSave), vec!["my-command"]);
    assert_eq!(hooks.commands(HookPoint::StateChange), vec!["my-command"]);
}

#[test]
#[should_panic(expected = "cannot dispatch a command")]
fn a_command_at_a_point_that_cannot_dispatch_is_caught_in_debug() {
    // A config is told at load, by the resolve pass. This is the other caller:
    // native Rust, which has no resolve pass in front of it. Held-and-never-
    // fired is exactly the silent no-op the Declaration pass exists to
    // prevent, so it fails in every test run rather than waiting to be noticed.
    let mut hooks = Hooks::new();

    hooks.add_command(HookPoint::BeforeCommand, "write-file");
}

#[test]
fn the_navigation_keys_are_spelled_the_way_emacs_spells_them() {
    // `SPC`, `ESC`, `RET` and `DEL` are Emacs's own `kbd` names, so these
    // follow: `TAB` bare like the other four, and the rest in the angle
    // brackets `kbd` uses for keys that are not characters. `<prior>` and
    // `<next>` read oddly and are what Emacs calls PageUp and PageDown —
    // spelling them `<pageup>` would be inventing a second vocabulary for a
    // reader who already has one.
    for (written, key) in [
        ("TAB", Key::Tab),
        ("<backtab>", Key::BackTab),
        ("<up>", Key::Up),
        ("<down>", Key::Down),
        ("<prior>", Key::PageUp),
        ("<next>", Key::PageDown),
    ] {
        assert_eq!(parse_keys(written).unwrap(), vec![key], "reading {written}");
        assert_eq!(Keys(&[key]).to_string(), written, "writing {written}");
    }
}

#[test]
fn a_navigation_key_takes_no_modifier() {
    // The same rule `C-RET` already follows: a modifier may only wrap a
    // character, because there is no `Key` for a modified one of these.
    for written in ["C-TAB", "M-<up>", "C-<next>"] {
        assert!(parse_keys(written).is_err(), "{written} should be refused");
    }
}

#[test]
fn the_leader_has_two_delays_like_which_key() {
    // `which-key-idle-delay` and `which-key-idle-secondary-delay`, both of
    // which Doom sets — 1.0 and 0.1. The second is what makes descending a
    // prefix feel instant once the menu is already up, and meowr charged the
    // full first delay for every level until #74's follow-up.
    let options = Options::default();

    assert_eq!(options.leader_delay, Duration::from_secs(1));
    assert_eq!(options.leader_secondary_delay, Duration::from_millis(100));
}

// --- language servers (#119) ----------------------------------------------

fn server(program: &str, args: &[&str]) -> LanguageServer {
    LanguageServer {
        program: program.into(),
        args: args.iter().map(|a| (*a).to_string()).collect(),
    }
}

#[test]
fn a_bare_program_name_is_left_for_path_to_resolve() {
    assert_eq!(
        server("rust-analyzer", &[]).resolved(Some("/home/user")),
        "rust-analyzer"
    );
}

#[test]
fn a_leading_tilde_slash_expands_to_the_home_directory() {
    // The one common exception to a bare name: a hand-built server under a home
    // directory. `Command::new` does no expansion of its own, so nothing would.
    // The home is passed in, not read: this crate does no I/O.
    assert_eq!(
        server("~/bin/rust-analyzer", &[]).resolved(Some("/home/user")),
        "/home/user/bin/rust-analyzer"
    );
    // A trailing slash on the home must not double up.
    assert_eq!(
        server("~/bin/ra", &[]).resolved(Some("/home/user/")),
        "/home/user/bin/ra"
    );
}

#[test]
fn with_no_home_known_a_tilde_is_left_alone() {
    // Better a program name that obviously does not exist than one silently
    // rewritten to the wrong place.
    assert_eq!(server("~/bin/ra", &[]).resolved(None), "~/bin/ra");
}

#[test]
fn nothing_else_expands() {
    // No `$VAR`, no globbing, no word splitting — which is exactly what makes
    // "there is no quoting" true rather than merely unimplemented.
    for program in [
        "$HOME/bin/ra",
        "/usr/bin/rust-analyzer",
        "./ra",
        "bin/*/ra",
        "~",
        "a~/b",
    ] {
        assert_eq!(
            server(program, &[]).resolved(Some("/home/user")),
            program,
            "{program} must be left alone"
        );
    }
}

#[test]
fn arguments_stay_separate_and_are_never_parsed() {
    // A value with a space in it is one argument, because it arrived as one.
    // This is the whole of the no-quoting answer.
    let s = server("typescript-language-server", &["--stdio", "--log dir here"]);
    assert_eq!(s.args.len(), 2);
    assert_eq!(s.args[1], "--log dir here");
}

#[test]
fn an_argument_is_not_expanded_even_when_it_looks_like_a_path() {
    // Deliberate asymmetry with the program: meowr must *find* a program in
    // order to run it, and hands an argument over without reading it.
    let s = server("ra", &["--log", "~/ra.log"]);
    assert_eq!(s.args[1], "~/ra.log");
}
