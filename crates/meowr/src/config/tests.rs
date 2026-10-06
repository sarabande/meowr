use super::*;

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("meowr-config-{}-{name}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a scratch directory");
    dir.join("init.scm")
}

#[test]
fn no_config_at_all_is_not_a_failure() {
    // The normal state of a fresh checkout. Nothing is wrong, so nothing is
    // said — #49 settled that absent is silent.
    assert!(
        evaluate(None)
            .expect("absent is fine")
            .declarations
            .is_empty()
    );
    let missing = scratch("missing");
    let _ = std::fs::remove_file(&missing);
    assert!(
        evaluate(Some(&missing))
            .expect("absent is fine")
            .declarations
            .is_empty()
    );
}

#[test]
fn a_config_that_runs_says_it_ran() {
    let path = scratch("runs");
    std::fs::write(&path, "(define (unused) 1)\n").unwrap();

    assert!(evaluate(Some(&path)).is_ok());
}

#[test]
fn a_config_steel_refuses_comes_back_as_a_failure_not_a_panic() {
    // #44 established these arrive as values rather than unwinding, and this
    // is the check that stays true if that ever changes.
    let path = scratch("rejected");
    std::fs::write(&path, "(this-does-not-exist)\n").unwrap();

    let e = evaluate(Some(&path)).expect_err("an unbound identifier");

    assert!(
        format!("{e}").contains("this-does-not-exist"),
        "the message names it: {e}"
    );
}

#[test]
fn a_config_that_never_finishes_is_abandoned() {
    // The failure #49 found that nothing else covers: a tail call never
    // overflows the stack, so Steel never returns — it spins, before the
    // terminal is taken, with no UI and nothing to press.
    let path = scratch("spins");
    std::fs::write(&path, "(let loop () (loop))\n").unwrap();

    let started = std::time::Instant::now();
    let e = evaluate(Some(&path)).expect_err("it never returns");

    assert!(matches!(e, Failed::Overran), "{e}");
    // Generously, because  also builds an engine and this is a
    // debug build, where that costs far more than the 252ms #44 measured
    // optimised. The point is that it gave up at all.
    assert!(
        started.elapsed() < BUDGET + Duration::from_secs(60),
        "it gave up rather than ran on"
    );
}

#[test]
fn a_config_can_require_a_sibling() {
    // Steel's own module system, so `include` is not a thing meowr invents.
    let path = scratch("requires");
    let dir = path.parent().unwrap();
    std::fs::write(
        dir.join("helper.scm"),
        "(provide helped)\n(define helped 7)\n",
    )
    .unwrap();
    std::fs::write(&path, "(require \"helper.scm\")\n(define got helped)\n").unwrap();

    let outcome = evaluate(Some(&path));
    assert!(outcome.is_ok(), "the sibling resolved: {outcome:?}");
}

#[test]
fn the_config_lives_under_xdg_config_home() {
    let found = find().expect("a home to look in");
    assert!(found.ends_with("meowr/init.scm"), "{found:?}");
}

// --- what a config declares (#61) ------------------------------------------

/// Evaluate `source` and hand back what it asked for.
fn declared(name: &str, source: &str) -> api::Collected {
    let path = scratch(name);
    std::fs::write(&path, source).unwrap();
    evaluate(Some(&path)).expect("it ran")
}

#[test]
fn a_config_declares_a_binding_a_prefix_and_a_command() {
    let collected = declared(
        "declares",
        r#"
        (define-prefix "SPC d" "dape")
        (define-command 'save-and-exit "Save, then go" '(write-file quit))
        (define-key 'normal "SPC d s" 'save-and-exit)
        (define-key 'normal "M-a" #f)
        "#,
    );

    assert!(collected.problems.is_empty(), "{:?}", collected.problems);
    assert_eq!(collected.declarations.len(), 4);
}

#[test]
fn a_config_can_bind_a_chord_to_one_thing() {
    // #78's answer, at the only seam that decides whether it is usable: the
    // author writes `M-a` in `init.scm`, and the name it points at carries a
    // `/`. Steel reads `/` as an ordinary constituent of a symbol, so this is
    // a check that the reader agrees rather than that the name exists —
    // nothing else in the config vocabulary has one, and a name meowr can
    // register but nobody can spell would be no answer at all.
    let collected = declared(
        "chord-to-a-thing",
        r#"
        (define-key 'normal "M-a" 'beginning-of-thing/defun)
        (define-key 'normal "M-e" 'end-of-thing/defun)
        "#,
    );

    assert!(collected.problems.is_empty(), "{:?}", collected.problems);
    assert_eq!(collected.declarations.len(), 2);
}

#[test]
fn nothing_in_a_config_can_run_a_command() {
    // Not a rule that is enforced — a thing that cannot be written. #48 dropped
    // `call` once a command body became a list of names, so there is no
    // function in the engine that runs anything.
    let path = scratch("no-call");
    std::fs::write(&path, "(call \"kill-line\")\n").unwrap();

    let e = evaluate(Some(&path)).expect_err("there is no `call`");

    assert!(format!("{e}").contains("call"), "{e}");
}

#[test]
fn a_string_where_a_symbol_belongs_says_which_to_write() {
    // #48 chose symbols for anything meowr looks up and strings only for key
    // sequences. Getting it wrong is the commonest thing a config will do.
    let collected = declared(
        "string-not-symbol",
        "(define-key 'normal \"SPC z\" \"find-file\")\n",
    );

    let said = collected.problems.join("; ");
    assert!(said.contains("symbol"), "{said}");
    assert!(said.contains("'find-file"), "and shows the fix: {said}");
}

#[test]
fn an_unknown_state_is_reported_with_the_ones_that_exist() {
    let collected = declared("bad-state", "(define-key 'norml \"SPC z\" 'find-file)\n");

    let said = collected.problems.join("; ");
    assert!(said.contains("norml"), "{said}");
    assert!(said.contains("normal"), "and lists the real ones: {said}");
}

#[test]
fn an_unparseable_key_is_reported() {
    let collected = declared(
        "bad-key",
        "(define-key 'normal \"SPC nope z\" 'find-file)\n",
    );

    assert!(
        collected.problems.iter().any(|p| p.contains("nope")),
        "{:?}",
        collected.problems
    );
}

#[test]
fn a_config_split_across_files_declares_from_both() {
    let path = scratch("split");
    let dir = path.parent().unwrap();
    std::fs::write(
        dir.join("keys.scm"),
        "(define-key 'normal \"SPC z\" 'find-file)\n",
    )
    .unwrap();
    std::fs::write(
        &path,
        "(require \"keys.scm\")\n(define-prefix \"SPC d\" \"dape\")\n",
    )
    .unwrap();

    let collected = evaluate(Some(&path)).expect("it ran");

    assert!(collected.problems.is_empty(), "{:?}", collected.problems);
    assert_eq!(collected.declarations.len(), 2, "one from each file");
}

// --- a config file driving a real session, end to end ----------------------

use meowr_app::Session;
use meowr_command::Key;
use meowr_view::Viewport;

/// Evaluate a config and apply it to a fresh session, as `main` does.
fn driven(name: &str, source: &str) -> Session {
    let path = scratch(name);
    std::fs::write(&path, source).unwrap();
    let collected = evaluate(Some(&path)).expect("it ran");
    assert!(collected.problems.is_empty(), "{:?}", collected.problems);

    let mut session = Session::new(Viewport { top: 0, height: 10 });
    session.loaded(meowr_domain::Document::from_text("alpha beta\n"));
    // A path, because these tests prove a config *drives the editor* and the
    // proof they reach for is `Action::Save`. Since #80 a save with no file
    // behind it is refused with a reason rather than asked for, so a pathless
    // fixture would be testing the refusal instead of the binding.
    session.set_path("notes.rs");
    session
        .apply(collected.declarations)
        .expect("it resolved as a whole");
    session
}

#[test]
fn a_config_file_binds_a_leader_key_to_a_command_it_defined() {
    // M2's criterion, minus options: `init.scm` rebinds keys and defines a
    // composed command, and the editor does what it says.
    let mut session = driven(
        "drives",
        r#"
        (define-prefix "SPC d" "dape")
        (define-command 'just-save "Save the buffer" '(write-file))
        (define-key 'normal "SPC d s" 'just-save)
        "#,
    );

    session.on_key(meowr_app::leader::KEY);
    session.on_key(Key::Char('d'));
    assert_eq!(
        session.on_key(Key::Char('s')),
        meowr_app::Action::Save,
        "the command the config composed actually ran"
    );
}

#[test]
fn a_config_file_names_a_prefix_the_menu_shows() {
    let mut session = driven("names-prefix", "(define-prefix \"SPC d\" \"dape\")\n");

    session.on_key(meowr_app::leader::KEY);
    session.show_menu();
    let entry = session
        .menu()
        .into_iter()
        .find(|e| e.key == Key::Char('d'))
        .expect("listed");
    assert_eq!(entry.description, "+dape");
}

#[test]
fn a_config_file_unbinds_a_key_with_false() {
    let mut session = driven("unbinds", "(define-key 'normal \"SPC b b\" #f)\n");

    session.on_key(meowr_app::leader::KEY);
    session.show_menu();
    session.on_key(Key::Char('b'));
    session.show_menu();
    let keys: Vec<Key> = session.menu().into_iter().map(|e| e.key).collect();
    assert!(!keys.contains(&Key::Char('b')), "gone: {keys:?}");
}

#[test]
fn a_config_split_across_files_drives_the_session_from_both() {
    let path = scratch("split-drives");
    let dir = path.parent().unwrap();
    std::fs::write(dir.join("more.scm"), "(define-prefix \"SPC d\" \"dape\")\n").unwrap();
    std::fs::write(
        &path,
        "(require \"more.scm\")\n(define-command 'just-save \"Save\" '(write-file))\n(define-key 'normal \"SPC d s\" 'just-save)\n",
    )
    .unwrap();
    let collected = evaluate(Some(&path)).expect("it ran");
    let mut session = Session::new(Viewport { top: 0, height: 10 });
    session.loaded(meowr_domain::Document::from_text("alpha\n"));
    // See `driven` above: `Action::Save` needs a file to be asked for.
    session.set_path("notes.rs");
    session.apply(collected.declarations).expect("resolved");

    session.on_key(meowr_app::leader::KEY);
    session.on_key(Key::Char('d'));
    assert_eq!(session.on_key(Key::Char('s')), meowr_app::Action::Save);
}

#[test]
fn a_config_naming_a_command_nobody_registered_changes_nothing() {
    let path = scratch("dangling");
    std::fs::write(&path, "(define-key 'normal \"SPC z\" 'fnid-file)\n").unwrap();
    let collected = evaluate(Some(&path)).expect("Steel accepted it");
    let mut session = Session::new(Viewport { top: 0, height: 10 });
    session.loaded(meowr_domain::Document::from_text("alpha\n"));

    let problems = session
        .apply(collected.declarations)
        .expect_err("no such command");

    assert!(
        problems.iter().any(|p| p.to_string().contains("fnid-file")),
        "{problems:?}"
    );
}

// --- options --------------------------------------------------------------

/// Evaluate a config, apply it, and hand back the problems it could not get
/// past. The counterpart of `driven`, for the configs that should not resolve.
fn refused(name: &str, source: &str) -> Vec<String> {
    let path = scratch(name);
    std::fs::write(&path, source).unwrap();
    let collected = evaluate(Some(&path)).expect("Steel accepted it");
    let mut session = Session::new(Viewport { top: 0, height: 10 });
    session.loaded(meowr_domain::Document::from_text("alpha\n"));
    let mut said: Vec<String> = collected.problems.clone();
    if let Err(problems) = session.apply(collected.declarations) {
        said.extend(problems.iter().map(|p| p.to_string()));
    }
    assert!(!said.is_empty(), "it should have been refused");
    said
}

#[test]
fn a_config_sets_an_option_of_every_kind() {
    // The four kinds #50 admitted: a boolean, a count, a duration in integer
    // milliseconds, and an enum as a symbol. Nothing else is an option value,
    // and in particular there are no strings.
    let session = driven(
        "options",
        r#"
        (set-option 'auto-save #f)
        (set-option 'auto-save-interval 8)
        (set-option 'preview-delay 250)
        (set-option 'expand-axis 'expand)
        "#,
    );

    let options = session.options();
    assert!(!options.auto_save);
    assert_eq!(options.auto_save_interval, 8);
    assert_eq!(options.preview_delay, std::time::Duration::from_millis(250));
    assert_eq!(options.expand_axis, meowr_command::Axis::Expand);
}

#[test]
fn an_option_a_config_set_changes_what_the_editor_does() {
    // Not just the field: the behaviour behind it. `expand-axis` is the one
    // that shows without a clock, because #37's own test for it is a keypress.
    let mut session = driven("option-behaves", "(set-option 'expand-axis 'expand)\n");

    session.on_key(Key::Char('w'));
    session.on_key(Key::Char('2'));

    assert_eq!(
        session.selection().axis(),
        Some(meowr_command::Axis::Expand)
    );
}

#[test]
fn a_config_asking_for_documentation_on_rest_gets_an_editor_that_offers_it() {
    // The whole path #121 opens, end to end: `lsp-ui-doc-show-with-cursor t`
    // in the reference configuration is a line of `init.scm` here rather than
    // a default, so the line has to arrive at a session that then treats a
    // resting point as a question. It passes the moment `Options::set` gains
    // the arm, which is the table being one table and not two.
    let mut session = driven(
        "hover-on-rest",
        r#"
        (set-option 'hover-on-rest #t)
        (set-option 'hover-delay 150)
        "#,
    );
    assert_eq!(
        session.options().hover_delay,
        std::time::Duration::from_millis(150)
    );

    session.on_key(Key::Char('w'));

    let asked = session.resting_hover().expect("a question to ask");
    assert_eq!(asked.path, "notes.rs");
}

#[test]
fn an_unknown_option_is_refused_and_the_real_ones_named() {
    // #50: "an unknown name lists the real ones, which is the cheapest
    // possible place to pay for" meowr naming its own options.
    let said = refused("misspelt", "(set-option 'preview-dealy 400)\n").join("\n");

    assert!(said.contains("preview-dealy"), "{said}");
    assert!(said.contains("preview-delay"), "and the one meant: {said}");
}

#[test]
fn an_option_given_the_wrong_type_says_what_was_expected() {
    let said = refused("wrong-type", "(set-option 'preview-delay 'fast)\n").join("\n");

    assert!(said.contains("preview-delay"), "{said}");
    assert!(
        said.contains("millisecond"),
        "and what it wanted instead: {said}"
    );
}

#[test]
fn a_duration_written_as_a_fraction_of_a_second_says_to_write_milliseconds() {
    // The whole reason #48 chose integer milliseconds: `0.4` is the natural
    // thing to write and there is no float formatting question if it is
    // refused at the door.
    let said = refused("float", "(set-option 'preview-delay 0.4)\n").join("\n");

    assert!(said.contains("millisecond"), "{said}");
    assert!(
        said.contains("400"),
        "spelling out the number to write: {said}"
    );
}

#[test]
fn an_enum_option_given_a_symbol_it_does_not_have_names_the_ones_it_does() {
    let said = refused("bad-axis", "(set-option 'expand-axis 'sideways)\n").join("\n");

    assert!(said.contains("sideways"), "{said}");
    assert!(said.contains("select"), "{said}");
    assert!(said.contains("expand"), "{said}");
}

#[test]
fn an_impossible_value_is_refused_rather_than_wrapped() {
    // A negative duration has no meaning and `Duration` cannot hold one.
    let said = refused("negative", "(set-option 'preview-delay -1)\n").join("\n");

    assert!(said.contains("preview-delay"), "{said}");
    assert!(said.contains("negative"), "{said}");
}

#[test]
fn a_string_where_an_option_symbol_belongs_says_which_to_write() {
    // The same split #48 drew for command names: symbols for anything meowr
    // looks up, strings only for key sequences.
    let said = refused("option-string", "(set-option 'expand-axis \"expand\")\n").join("\n");

    assert!(said.contains("'expand"), "{said}");
}

#[test]
fn a_misspelt_option_stops_the_bindings_landing_too() {
    // The atomicity #59 settled, extended to options: a set that does not
    // resolve applies *nothing*, so a typo in one line cannot leave the other
    // lines half-applied.
    let path = scratch("atomic-option");
    std::fs::write(
        &path,
        r#"
        (define-key 'normal "SPC z" 'write-file)
        (set-option 'auto-sav #f)
        "#,
    )
    .unwrap();
    let collected = evaluate(Some(&path)).expect("Steel accepted it");
    let mut session = Session::new(Viewport { top: 0, height: 10 });
    session.loaded(meowr_domain::Document::from_text("alpha\n"));

    session.apply(collected.declarations).expect_err("auto-sav");

    assert!(session.options().auto_save, "the option is untouched");
    session.on_key(meowr_app::leader::KEY);
    assert_ne!(
        session.on_key(Key::Char('z')),
        meowr_app::Action::Save,
        "and the binding beside it never landed"
    );
}

// --- hooks (#64) -----------------------------------------------------------

#[test]
fn a_config_hooks_a_command_it_defined_to_after_save() {
    // The map's criterion for M2, complete: an `init.scm` that rebinds a key,
    // sets an option, defines a composed command *and* adds a hook.
    let mut session = driven(
        "hooks",
        r#"
        (define-command 'tidy "Open a line below" '(open-below))
        (add-hook 'after-save 'tidy)
        "#,
    );

    session.mark_saved();

    assert_eq!(session.contents().unwrap(), "alpha beta\n\n");
}

#[test]
fn a_hook_at_a_point_that_does_not_take_a_command_is_refused_at_load() {
    // `before-command` exists so a hook can *refuse*, and a composed command
    // returns `Outcome` and cannot. Accepting it would offer the one thing
    // the hook point is for and never deliver it.
    let said = refused("bad-point", "(add-hook 'before-command 'write-file)\n").join("\n");

    assert!(said.contains("before-command"), "{said}");
    assert!(said.contains("after-save"), "and the ones that do: {said}");
    assert!(said.contains("state-change"), "{said}");
}

#[test]
fn a_hook_naming_a_command_nobody_registered_is_refused() {
    let said = refused("dangling-hook", "(add-hook 'after-save 'tidy)\n").join("\n");

    assert!(said.contains("tidy"), "{said}");
}

#[test]
fn a_point_that_does_not_exist_is_reported_with_the_ones_that_do() {
    let said = refused("no-point", "(add-hook 'after-lunch 'write-file)\n").join("\n");

    assert!(said.contains("after-lunch"), "{said}");
    assert!(said.contains("after-save"), "{said}");
}

#[test]
fn a_point_written_as_a_string_says_which_to_write() {
    let said = refused("point-string", "(add-hook \"after-save\" 'write-file)\n").join("\n");

    assert!(said.contains("'after-save"), "{said}");
}

#[test]
fn a_config_removes_a_hook_meowr_shipped() {
    // #48's own example line, run for real.
    let session = driven(
        "unhook",
        "(remove-hook 'before-save 'auto-exit-insert-on-save)\n",
    );

    assert!(session.echo().is_none(), "it resolved quietly");
}

#[test]
fn removing_a_hook_that_is_not_there_says_so() {
    let said = refused(
        "unhook-typo",
        "(remove-hook 'before-save 'auto-exit-insert-on-sav)\n",
    )
    .join("\n");

    assert!(said.contains("auto-exit-insert-on-sav"), "{said}");
}

#[test]
fn a_config_can_take_a_hook_off_and_put_another_on() {
    // Removal is ordered before addition for the same reason unbinding is:
    // so one file can free something and reuse it.
    let mut session = driven(
        "rehook",
        r#"
        (define-command 'tidy "Open a line below" '(open-below))
        (remove-hook 'before-save 'auto-exit-insert-on-save)
        (add-hook 'after-save 'tidy)
        "#,
    );

    session.mark_saved();

    assert_eq!(session.contents().unwrap(), "alpha beta\n\n");
}

// --- language servers (#119) ----------------------------------------------

#[test]
fn a_language_written_as_a_string_is_corrected() {
    // The seam's own rule (#48): a symbol is checked against a registry and a
    // string is not, so writing one for the other is worth naming.
    let collected = declared(
        "server-string-lang",
        r#"(set-language-server "rust" "ra" '())"#,
    );
    assert_eq!(collected.problems.len(), 1);
    assert!(
        collected.problems[0].contains("symbol"),
        "{:?}",
        collected.problems
    );
}

#[test]
fn a_program_written_as_a_symbol_is_corrected() {
    // The other direction, which matters just as much: a program name is not
    // looked up in anything of meowr's, so it is a string.
    let collected = declared(
        "server-symbol-prog",
        r#"(set-language-server 'rust 'ra '())"#,
    );
    assert_eq!(collected.problems.len(), 1);
    assert!(
        collected.problems[0].contains("string"),
        "{:?}",
        collected.problems
    );
}

#[test]
fn arguments_that_are_not_a_list_are_refused() {
    let collected = declared(
        "server-args",
        r#"(set-language-server 'rust "ra" "--stdio")"#,
    );
    assert_eq!(collected.problems.len(), 1);
    assert!(
        collected.problems[0].contains("list"),
        "{:?}",
        collected.problems
    );
}

#[test]
fn a_config_declares_the_server_for_a_language() {
    // `driven` opens `notes.rs`, so the buffer's Language is Rust.
    let session = driven(
        "server-rust",
        r#"(set-language-server 'rust "~/bin/rust-analyzer" '("--stdio"))"#,
    );
    let server = session
        .language_server(session.current_buffer_id())
        .expect("a Rust buffer has a Rust server");
    assert_eq!(server.args, vec!["--stdio".to_string()]);
    // The home is passed in, not read by the domain crate — which is the point
    // of `resolved` taking it.
    assert_eq!(
        server.resolved(Some("/home/user")),
        "/home/user/bin/rust-analyzer"
    );
}

#[test]
fn an_editor_with_no_config_still_serves_rust() {
    // #117's hardcoding became a default rather than going away: an editor
    // nobody has configured must not silently do less than it did.
    let mut session = Session::new(Viewport { top: 0, height: 10 });
    session.loaded(meowr_domain::Document::from_text("fn main() {}\n"));
    session.set_path("notes.rs");
    let server = session
        .language_server(session.current_buffer_id())
        .expect("the default table serves Rust");
    assert_eq!(server.program, "rust-analyzer");
    assert!(server.args.is_empty());
}

#[test]
fn a_language_with_no_server_declared_gets_none() {
    let mut session = Session::new(Viewport { top: 0, height: 10 });
    session.loaded(meowr_domain::Document::from_text("# notes\n"));
    session.set_path("notes.md");
    assert!(
        session
            .language_server(session.current_buffer_id())
            .is_none(),
        "nothing serves Markdown, and that is an ordinary answer"
    );
}

#[test]
fn a_server_for_a_language_meowr_does_not_know_is_said_out_loud() {
    // The `set-option` precedent: a name meowr cannot resolve stops the whole
    // set rather than being dropped on its own while the bindings beside it
    // land. Silently ignoring it would leave the config looking applied.
    let path = scratch("server-unknown");
    std::fs::write(&path, r#"(set-language-server 'cobol "cobol-ls" '())"#).unwrap();
    let collected = evaluate(Some(&path)).expect("it ran");
    assert!(collected.problems.is_empty(), "the syntax was fine");

    let mut session = Session::new(Viewport { top: 0, height: 10 });
    let problems = session
        .apply(collected.declarations)
        .expect_err("an unknown Language does not resolve");
    assert_eq!(problems.len(), 1);
    assert!(problems[0].to_string().contains("cobol"), "{}", problems[0]);
}

// --- formatters (#163, ADR-0014) ---------------------------------------------

#[test]
fn a_config_declares_the_formatter_for_a_language() {
    // `driven` opens `notes.rs`. `{file}` is the buffer's path, which is how
    // apheleia's `filepath` lets a formatter find its own config.
    let session = driven(
        "formatter-rust",
        r#"(set-formatter 'rust "~/bin/rustfmt" '("--edition" "2024" "--stdin-filepath" "{file}"))"#,
    );
    let invocation = session
        .formatter(session.current_buffer_id())
        .expect("a Rust buffer has a Rust formatter")
        .at_home(Some("/home/user"));
    assert_eq!(invocation.program, "/home/user/bin/rustfmt");
    assert_eq!(
        invocation.args,
        ["--edition", "2024", "--stdin-filepath", "notes.rs"].map(String::from)
    );
}

#[test]
fn a_formatter_set_to_false_is_taken_away() {
    // Doom's `+format-on-save-disabled-modes`, for one Language.
    let session = driven("formatter-off", r#"(set-formatter 'rust #f)"#);
    assert!(session.formatter(session.current_buffer_id()).is_none());
}

#[test]
fn an_editor_with_no_config_formats_with_apheleias_table() {
    // ADR-0014: the defaults are apheleia's, for the Languages meowr links.
    let formatted = |path: &str| {
        let mut session = Session::new(Viewport { top: 0, height: 10 });
        session.loaded(meowr_domain::Document::from_text("x\n"));
        session.set_path(path);
        session
            .formatter(session.current_buffer_id())
            .map(|invocation| (invocation.program, invocation.args))
    };
    let argv = |program: &str, args: &[&str]| {
        Some((program.to_string(), args.iter().map(|a| a.to_string()).collect::<Vec<_>>()))
    };
    assert_eq!(formatted("a.rs"), argv("rustfmt", &["--quiet", "--emit", "stdout"]));
    assert_eq!(formatted("a.py"), argv("black", &["--stdin-filename", "a.py", "-"]));
    assert_eq!(formatted("a.sh"), argv("shfmt", &["-filename", "a.sh", "-"]));
    assert_eq!(formatted("a.nix"), argv("nixfmt", &[]));
    assert_eq!(formatted("a.toml"), argv("taplo", &["format", "--colors", "never", "-"]));
    assert_eq!(
        formatted("a.json"),
        argv("prettier", &["--stdin-filepath", "a.json", "--parser=json"])
    );
    assert_eq!(
        formatted("a.yaml"),
        argv("prettier", &["--stdin-filepath", "a.yaml", "--parser=yaml"])
    );
    assert_eq!(formatted("a.md"), None, "apheleia leaves markdown out on purpose");
    assert_eq!(formatted("a.scm"), None, "apheleia has none for scheme");
}

#[test]
fn a_formatter_for_a_language_meowr_does_not_know_is_said_out_loud() {
    let path = scratch("formatter-unknown");
    std::fs::write(&path, r#"(set-formatter 'cobol "cobolfmt" '())"#).unwrap();
    let collected = evaluate(Some(&path)).expect("it ran");
    assert!(collected.problems.is_empty(), "the syntax was fine");
    let mut session = Session::new(Viewport { top: 0, height: 10 });
    let problems = session
        .apply(collected.declarations)
        .expect_err("an unknown Language does not resolve");
    assert_eq!(problems.len(), 1);
    assert!(problems[0].to_string().contains("cobol"), "{}", problems[0]);
}

#[test]
fn a_formatter_declared_wrongly_is_corrected() {
    // The three ways `set-language-server` is refused, and the one the rest
    // arguments add: a list given twice is not two lists of arguments.
    for (name, source, says) in [
        ("formatter-string-lang", r#"(set-formatter "rust" "rustfmt" '())"#, "symbol"),
        ("formatter-symbol-prog", r#"(set-formatter 'rust 'rustfmt '())"#, "string"),
        ("formatter-args", r#"(set-formatter 'rust "rustfmt" "--emit")"#, "list"),
        ("formatter-extra", r#"(set-formatter 'rust "rustfmt" '() '("x"))"#, "takes a Language"),
    ] {
        let collected = declared(name, source);
        assert_eq!(collected.problems.len(), 1, "{source}");
        assert!(collected.problems[0].contains(says), "{source}: {:?}", collected.problems);
    }
}

#[test]
fn format_on_save_is_off_until_a_config_turns_it_on() {
    // `+onsave` is the author's flag, not Doom's default -- `hover-on-rest`'s
    // rule reaching the same answer.
    let session = driven("format-on-save", "(set-option 'format-on-save #t)");
    assert!(session.options().format_on_save);
    assert!(!meowr_command::Options::default().format_on_save);
}
