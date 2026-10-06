//! Session behaviour, end to end: keys in through `on_key`, state out through
//! the accessors the renderer uses.
//!
//! An integration test rather than an in-file one because that is the whole
//! contract — `wagit`'s `DEVELOPING.md` puts "session behaviour end to end" in
//! `<crate>/tests/` and keeps in-file `mod tests` for pure functions. Reaching
//! past this boundary into internals is what the view layer exists to prevent,
//! and from out here it is not reachable to begin with.

use meowr_app::buffer::{Buffer, BufferId};
use meowr_app::narrow::Preview;
use meowr_app::{Action, Files, LeaderKind, Opened, Session, Subject};
use meowr_command::{HookPoint, Key, Options, Proceed, State};
use meowr_core as core;
use meowr_core::thing::Extent;
use meowr_core::{Axis, Kind, Selection};
use meowr_domain::{Diagnostic, Document, Severity};
use meowr_view::Viewport;
use std::time::Duration;

fn editing(text: &str) -> Session {
    let mut session = Session::new(Viewport { top: 0, height: 10 });
    session.loaded(Document::from_text(text));
    session
}

// --- carried over from slice 01 -------------------------------------------

#[test]
fn a_session_starts_empty_and_showing_nothing() {
    let session = Session::new(Viewport { top: 0, height: 10 });
    assert!(session.is_loading());
    assert!(session.rows().is_empty());
}

#[test]
fn a_loaded_document_becomes_visible() {
    let session = editing("alpha\nbeta\ngamma\n");
    assert!(!session.is_loading());
    assert_eq!(session.rows()[0].text, "alpha");
}

#[test]
fn a_failed_load_is_reported_rather_than_left_loading() {
    let mut session = Session::new(Viewport::default());
    session.failed("no such file".to_string());
    assert_eq!(session.error(), Some("no such file"));
}

#[test]
fn resizing_changes_how_many_lines_are_visible() {
    let mut session = Session::new(Viewport { top: 0, height: 2 });
    session.loaded(Document::from_text("a\nb\nc\nd\ne\n"));
    assert_eq!(session.on_resize(80, 4), Action::Redraw);
    // Three, not four: the modeline takes the window's last row. It is not an
    // overlay — ADR-0004 — so it costs a line of text, and every viewport
    // calculation has to know that or the last line hides underneath it.
    assert_eq!(session.rows().len(), 3);
}

// --- this slice -----------------------------------------------------------

#[test]
fn a_session_starts_in_normal_state() {
    assert_eq!(editing("hi\n").state(), State::Normal);
}

#[test]
fn typing_in_normal_state_does_not_insert() {
    let mut session = editing("hi\n");

    session.on_key(Key::Char('q')); // unbound in Normal State

    assert_eq!(session.document().unwrap().text().to_string(), "hi\n");
}

#[test]
fn typing_in_insert_state_inserts_at_the_point_and_advances_it() {
    let mut session = editing("hi\n");
    session.on_key(Key::Char('s')); // enter Insert
    assert_eq!(session.state(), State::Insert);

    session.on_key(Key::Char('a'));
    session.on_key(Key::Char('b'));

    assert_eq!(session.document().unwrap().text().to_string(), "abhi\n");
    assert_eq!(session.point(), 2);
}

#[test]
fn typing_a_multi_byte_character_advances_the_point_by_its_bytes() {
    let mut session = editing("");
    session.on_key(Key::Char('s'));

    session.on_key(Key::Char('é'));

    assert_eq!(session.point(), 2);
}

#[test]
fn escape_leaves_insert_state() {
    let mut session = editing("hi\n");
    session.on_key(Key::Char('s'));

    assert_eq!(session.on_key(Key::Esc), Action::Redraw);

    assert_eq!(session.state(), State::Normal);
}

#[test]
fn backspace_in_insert_state_removes_the_whole_previous_character() {
    let mut session = editing("");
    session.on_key(Key::Char('s'));
    session.on_key(Key::Char('é'));
    assert_eq!(session.point(), 2);

    session.on_key(Key::Backspace);

    assert_eq!(session.document().unwrap().text().to_string(), "");
    assert_eq!(session.point(), 0);
}

#[test]
fn a_prefix_key_is_pending_and_does_nothing_until_the_sequence_completes() {
    let mut session = editing("hi\n");
    session
        .bind(
            State::Normal,
            // A key nothing binds: binding a sequence *through* a bound key
            // is the ambiguity the keymap rejects on purpose, so this test
            // needs one that is free. It was `x` until `x` became
            // `meow-delete`, then `q` until `q` became `quit-window`.
            &[Key::Char('Z'), Key::Char('s')],
            "enter-insert",
        )
        .unwrap();

    assert_eq!(session.on_key(Key::Char('Z')), Action::Nothing);
    assert_eq!(session.state(), State::Normal);

    session.on_key(Key::Char('s'));

    assert_eq!(session.state(), State::Insert);
}

#[test]
fn a_sequence_that_leaves_the_bound_paths_is_abandoned_not_remembered() {
    let mut session = editing("hi\n");
    session
        .bind(
            State::Normal,
            // Free, for the reason the test above gives.
            &[Key::Char('Z'), Key::Char('s')],
            "enter-insert",
        )
        .unwrap();

    session.on_key(Key::Char('Z'));
    session.on_key(Key::Char('Z')); // no such sequence
    session.on_key(Key::Char('s')); // must be read fresh, not as `g s`

    assert_eq!(session.state(), State::Insert);
}

#[test]
fn a_command_can_be_run_by_name_without_a_key() {
    let mut session = editing("hi\n");

    session.run("enter-insert");

    assert_eq!(session.state(), State::Insert);
}

#[test]
fn the_quit_command_asks_the_event_loop_to_quit() {
    let mut session = editing("hi\n");

    assert_eq!(session.on_key(Key::Ctrl('c')), Action::Quit);
}

#[test]
fn a_command_bound_in_another_state_does_not_fire_in_this_one() {
    let mut session = editing("hi\n");
    session.on_key(Key::Char('s')); // Insert

    // `s` is bound in Normal only; in Insert it must self-insert instead.
    session.on_key(Key::Char('s'));

    assert_eq!(session.document().unwrap().text().to_string(), "shi\n");
}

#[test]
fn a_motion_key_moves_the_point_in_normal_state() {
    let mut session = editing("alpha beta\n");

    session.on_key(Key::Char('i')); // right, in Colemak
    session.on_key(Key::Char('i'));

    assert_eq!(session.point(), 2);
}

#[test]
fn a_word_key_selects_a_word() {
    let mut session = editing("alpha beta\n");

    session.on_key(Key::Char('m')); // mark-word

    assert_eq!(session.selection().range(), 0..5);
    assert_eq!(session.selection().kind(), Some(Kind::WORD));
}

#[test]
fn an_expanding_motion_key_grows_the_selection() {
    let mut session = editing("alpha beta\n");
    session.on_key(Key::Char('m')); // (expand . word) over "alpha"

    session.on_key(Key::Char('w')); // next-word continues expanding

    assert_eq!(session.selection().range(), 0..10);
    assert_eq!(session.selection().axis(), Some(Axis::Expand));
}

#[test]
fn the_reverse_key_flips_the_selection() {
    let mut session = editing("alpha beta\n");
    session.on_key(Key::Char('m'));

    session.on_key(Key::Char(';'));

    assert!(session.selection().is_backward());
}

#[test]
fn the_cancel_key_drops_the_selection() {
    let mut session = editing("alpha beta\n");
    session.on_key(Key::Char('m'));

    session.on_key(Key::Char('g'));

    assert_eq!(session.selection().selection_type(), None);
}

#[test]
fn the_pop_key_walks_back_through_the_selection_history() {
    let mut session = editing("alpha beta gamma\n");
    session.on_key(Key::Char('m')); // "alpha"
    session.on_key(Key::Char('w')); // "alpha beta"

    session.on_key(Key::Char('z'));

    assert_eq!(session.selection().range(), 0..5);
}

#[test]
fn pop_with_no_selection_falls_back_through_the_table() {
    // pop-selection requires a selection; with none it stands aside for its
    // fallback, `meow-pop-grab` — which with no Grab either does nothing and
    // does *not* signal, because Emacs only messages there.
    let mut session = editing("alpha\n");
    let signals = session.signals();

    // `Redraw` rather than `Nothing` since #80: it now says "No Grab held."
    // in the Echo Area, which is a frame. Still no signal, which is the fact
    // this test is about — Emacs messages here rather than raising.
    assert_eq!(session.on_key(Key::Char('z')), Action::Redraw);
    assert_eq!(session.selection().selection_type(), None);
    assert_eq!(session.signals(), signals, "a missing Grab is quiet");
    assert!(session.echo().is_some(), "but it is not silent");
}

#[test]
fn a_read_only_buffer_refuses_a_mutating_command() {
    let mut session = editing("hi\n");
    session.set_read_only(true);
    session.on_key(Key::Char('s')); // entering Insert is not a mutation

    session.on_key(Key::Char('a'));

    assert_eq!(session.state(), State::Insert);
    assert_eq!(session.document().unwrap().text().to_string(), "hi\n");
}

#[test]
fn the_selection_follows_the_text_when_an_edit_happens_before_it() {
    let mut session = editing("alpha beta\n");
    session.on_key(Key::Char('w')); // select "alpha", point at 5
    session.on_key(Key::Char('w')); // select " beta"
    let before = session.selection().range();

    // `s` is `meow-insert`, which goes to the *start* of the selection and
    // drops it; typing there shifts everything after the edit.
    session.on_key(Key::Char('s'));
    session.on_key(Key::Char('X'));

    assert_ne!(session.selection().range(), before);
    assert_eq!(
        session.document().unwrap().text().to_string(),
        "alphaX beta\n"
    );
}

#[test]
fn a_block_key_selects_the_enclosing_brackets() {
    let mut session = editing("call(a, b)\n");
    for _ in 0..6 {
        session.on_key(Key::Char('i')); // move inside the parens
    }

    session.on_key(Key::Char('o'));

    assert_eq!(session.selection().range(), 4..10);
}

/// A session over a Rust buffer with its tree already parsed and applied.
///
/// `o` cannot tell a delimiter from a bracket inside a string without one, so
/// every test about that has to get this far rather than stopping at
/// `set_path`.
fn parsed_rust(text: &str) -> Session {
    let mut session = editing(text);
    session.set_path("t.rs");
    let (buffer, job) = session.take_reparse().expect("rust is recognised");
    let revision = job.revision;
    let languages = session.languages();
    let syntax = job.run(&languages);
    assert!(syntax.is_some(), "the parse succeeded");
    session.apply_reparse(buffer, revision, syntax);
    session
}

#[test]
fn a_block_key_does_not_count_a_bracket_inside_a_string() {
    // The whole of #34, end to end: `o` on a line whose string contains a `)`
    // used to close the pair there and select `(a, "` — five characters short
    // and unbalanced. The tree that colours the buffer is what says otherwise.
    let text = "fn f() { g(a, \")\", b); }\n";
    let mut session = parsed_rust(text);
    let inside = text.find("a,").unwrap();
    session.set_selection(Selection::at(inside));

    session.on_key(Key::Char('o'));

    let open = text.find("(a,").unwrap();
    let close = text.find(", b)").unwrap() + 3;
    assert_eq!(session.selection().range(), open..close + 1);
}

#[test]
fn a_block_key_does_not_count_a_bracket_inside_a_comment() {
    let text = "fn f() {\n    g(a, // )\n        b);\n}\n";
    let mut session = parsed_rust(text);
    let inside = text.find("a,").unwrap();
    session.set_selection(Selection::at(inside));

    session.on_key(Key::Char('o'));

    let open = text.find("(a,").unwrap();
    let close = text.find("b);").unwrap() + 2;
    assert_eq!(session.selection().range(), open..close);
}

#[test]
fn the_match_thing_takes_whichever_bracket_encloses_the_point() {
    // `, r` `, s` `, c` each make you already know which bracket you are
    // inside. `, m` does not ask, and answers the innermost one.
    let text = "a {b [c] d}\n";
    let mut session = editing(text);
    session.set_selection(Selection::at(text.find('c').unwrap()));

    session.on_key(Key::Char(','));
    session.on_key(Key::Char('m'));

    let open = text.find('[').unwrap();
    assert_eq!(session.selection().range(), open + 1..open + 2);
}

#[test]
fn the_match_thing_asks_the_tree_which_brackets_are_real_and_the_round_thing_does_not() {
    // The end of the wire: `. m` reaches `Delimiters` through the same
    // `Syntactic` adapter `o` uses, so it agrees with `o` on this buffer. `. r`
    // is Meow's bare literal search and stops at the `)` inside the string.
    // Both are right; they are different matchers, and this pins that the
    // Thing key chooses between them rather than blurring them.
    let text = "fn f() { g(a, \")\", b); }\n";
    let open = text.find("(a,").unwrap();
    let real_close = text.find(", b)").unwrap() + 3;
    let quoted_close = text.find(r#"")""#).unwrap() + 1;

    let mut session = parsed_rust(text);
    session.set_selection(Selection::at(text.find("a,").unwrap()));
    session.on_key(Key::Char('.'));
    session.on_key(Key::Char('m'));
    assert_eq!(session.selection().range(), open..real_close + 1);

    let mut session = parsed_rust(text);
    session.set_selection(Selection::at(text.find("a,").unwrap()));
    session.on_key(Key::Char('.'));
    session.on_key(Key::Char('r'));
    assert_eq!(session.selection().range(), open..quoted_close + 1);
}

#[test]
fn the_match_thing_is_quiet_where_no_bracket_encloses_the_point() {
    // Unlike `o`, which reaches the block ahead of it. A Thing is the one
    // around the point, and `meow--select-range` is a bare `(when bounds ...)`
    // — so nothing happens, and nothing signals either.
    let text = "foo bar (baz) qux\n";
    let mut session = editing(text);
    session.set_selection(Selection::at(0));

    session.on_key(Key::Char(','));
    session.on_key(Key::Char('m'));

    assert_eq!(session.selection().range(), 0..0);
}

#[test]
fn a_buffer_with_no_grammar_counts_every_bracket_as_it_always_did() {
    // The unchanged half of the acceptance criteria. Plain text has no tree to
    // ask, so `o` behaves exactly as it did before there was a question.
    let text = "g(a, \")\", b) end\n";
    let mut session = editing(text);
    session.set_selection(Selection::at(text.find("a,").unwrap()));

    session.on_key(Key::Char('o'));

    let open = text.find("(a,").unwrap();
    let quoted_close = text.find(r#"")""#).unwrap() + 1;
    assert_eq!(session.selection().range(), open..quoted_close + 1);
}

// --- expand and numeric arguments -----------------------------------------

/// Give a fixture a file to be saved to.
///
/// `editing` builds a buffer with no path, and since #80 a save with nothing
/// behind it is refused with a reason rather than asked for — so a test whose
/// proof is `Action::Save` needs a target or it is testing the refusal.
///
/// `.txt` on purpose: these fixtures are about bindings, and a `.rs` name
/// would attach a Language and a syntax tree to buffers that have no use for
/// either.
fn savable(session: &mut Session) {
    session.set_path("notes.txt");
}

fn press(session: &mut Session, keys: &str) {
    for key in keys.chars() {
        session.on_key(Key::Char(key));
    }
}

#[test]
fn a_digit_expands_a_selection_made_by_a_motion() {
    let mut session = editing("alpha beta gamma delta\n");
    press(&mut session, "w");
    let one_word = session.selection().pos();

    press(&mut session, "2");

    assert!(session.selection().pos() > one_word);
    assert_eq!(session.selection().mark(), 0, "the far end stays put");
}

#[test]
fn a_digit_with_no_selection_builds_a_numeric_argument() {
    // Meow *errors* here, because its fallback table is keyed by a command
    // name the digit keys never carry. meowr does what the table says it
    // meant; the divergence is recorded in the fidelity case generator.
    let mut session = editing("alpha beta gamma delta\n");
    press(&mut session, "2");
    assert_eq!(session.argument().value(), Some(2));

    press(&mut session, "3");
    assert_eq!(session.argument().value(), Some(23), "digits accumulate");
}

#[test]
fn a_numeric_argument_is_consumed_by_the_command_it_precedes() {
    let mut session = editing("alpha beta gamma delta\n");
    press(&mut session, "2");
    assert_eq!(session.argument().value(), Some(2));

    press(&mut session, "w");

    assert_eq!(
        session.argument().value(),
        None,
        "the argument does not linger for the next command"
    );
}

#[test]
fn minus_starts_a_negative_argument() {
    let mut session = editing("alpha beta gamma\n");
    press(&mut session, "-");
    assert_eq!(session.argument().value(), Some(-1));

    press(&mut session, "3");
    assert_eq!(session.argument().value(), Some(-3));
}

#[test]
fn a_motion_that_cannot_move_signals() {
    // `h` at the beginning of the buffer runs `backward-char`, which raises
    // `beginning-of-buffer` in Emacs. The command loop beeps and carries on,
    // so the only trace is the signal itself.
    let mut session = editing("alpha\n");
    assert_eq!(session.signals(), 0);

    press(&mut session, "h");

    assert_eq!(session.signals(), 1);
    assert_eq!(session.point(), 0);
}

#[test]
fn reversing_with_no_selection_signals_but_cancelling_falls_back() {
    let mut session = editing("alpha beta\n");

    // `meow-reverse` has no entry in `meow-selection-command-fallback`.
    press(&mut session, ";");
    assert_eq!(session.signals(), 1);

    // `meow-cancel-selection` does: `keyboard-quit`, which also signals.
    press(&mut session, "g");
    assert_eq!(session.signals(), 2);

    // ...but `meow-pop-selection` falls back to `meow-pop-grab`, which is
    // quiet.
    press(&mut session, "z");
    assert_eq!(session.signals(), 2);
}

#[test]
fn the_expand_axis_is_configurable() {
    let mut session = editing("alpha beta gamma\n");
    press(&mut session, "w2");
    assert_eq!(session.selection().axis(), Some(core::Axis::Select));

    let mut session = editing("alpha beta gamma\n");
    session.set_options(Options {
        expand_axis: core::Axis::Expand,
        ..Options::default()
    });
    press(&mut session, "w2");
    assert_eq!(session.selection().axis(), Some(core::Axis::Expand));
}

// --- editing and save -----------------------------------------------------

#[test]
fn a_mutating_command_marks_the_buffer_modified_and_saving_clears_it() {
    let mut session = editing("alpha beta\n");
    assert!(!session.is_modified());

    press(&mut session, "wk");

    assert!(session.is_modified());
    assert_eq!(session.contents().unwrap(), " beta\n");

    session.mark_saved();
    assert!(!session.is_modified());
}

#[test]
fn a_motion_does_not_mark_the_buffer_modified() {
    let mut session = editing("alpha beta\n");
    press(&mut session, "wwlm");
    assert!(!session.is_modified());
}

#[test]
fn write_file_asks_the_host_rather_than_touching_the_disk() {
    // The session owns no I/O — it asks. `C-x C-s`, as in Emacs.
    let mut session = editing("alpha\n");
    savable(&mut session);
    assert_eq!(session.on_key(Key::Ctrl('x')), Action::Nothing);
    assert_eq!(session.on_key(Key::Ctrl('s')), Action::Save);
}

#[test]
fn a_read_only_buffer_refuses_edits_but_still_moves() {
    let mut session = editing("alpha beta\n");
    session.set_read_only(true);

    press(&mut session, "wk");

    assert_eq!(session.contents().unwrap(), "alpha beta\n");
    assert!(!session.is_modified());
    assert_eq!(session.selection().range(), 0..5, "the motion still ran");
}

#[test]
fn yank_pop_only_works_directly_after_a_yank() {
    let mut session = editing("alpha beta\n");
    // Two things on the ring.
    press(&mut session, "wy");
    press(&mut session, "wwy");
    press(&mut session, "p");
    let after_yank = session.contents().unwrap();

    // A motion in between, and `P` must decline rather than eat text.
    press(&mut session, "h");
    press(&mut session, "P");
    assert_eq!(
        session.contents().unwrap(),
        after_yank,
        "yank-pop after anything but a yank is a signal, not an edit"
    );
}

#[test]
fn the_four_ways_into_insert_state_differ_only_in_where_they_leave_the_point() {
    for (key, expected) in [('s', 0), ('a', 5)] {
        let mut session = editing("alpha beta\n");
        press(&mut session, "w");
        press(&mut session, &key.to_string());
        assert_eq!(session.state(), State::Insert, "after {key}");
        assert_eq!(session.point(), expected, "after {key}");
    }
}

#[test]
fn editing_with_no_selection_goes_through_the_fallback_table() {
    // `k` falls back to kill-line, `c` to change-char. `y` and `r` have no
    // entry and signal, which is the elisp saying so by omission.
    let mut session = editing("alpha beta\ngamma\n");
    press(&mut session, "k");
    assert_eq!(session.contents().unwrap(), "\ngamma\n");

    let mut session = editing("alpha beta\n");
    press(&mut session, "c");
    assert_eq!(session.contents().unwrap(), "lpha beta\n");
    assert_eq!(session.state(), State::Insert);

    let mut session = editing("alpha beta\n");
    let before = session.signals();
    press(&mut session, "y");
    assert_eq!(session.signals(), before + 1, "no fallback for save");
}

// --- undo -----------------------------------------------------------------

#[test]
fn undo_takes_back_the_last_command_and_redo_puts_it_back() {
    let mut session = editing("alpha beta\n");
    press(&mut session, "wk");
    assert_eq!(session.contents().unwrap(), " beta\n");

    press(&mut session, "u");
    assert_eq!(session.contents().unwrap(), "alpha beta\n");

    session.on_key(Key::Ctrl('r'));
    assert_eq!(session.contents().unwrap(), " beta\n");
}

#[test]
fn a_run_of_typing_is_one_undo_but_a_motion_between_them_is_two() {
    let mut session = editing("");
    press(&mut session, "s"); // Insert State
    press(&mut session, "hello");
    session.on_key(Key::Esc);

    press(&mut session, "u");
    assert_eq!(
        session.contents().unwrap(),
        "",
        "the whole word, not \"hell\""
    );

    // Now the same typing split by leaving and re-entering Insert State.
    let mut session = editing("");
    press(&mut session, "s");
    press(&mut session, "hel");
    session.on_key(Key::Esc);
    press(&mut session, "s");
    press(&mut session, "lo");
    session.on_key(Key::Esc);

    press(&mut session, "u");
    assert_eq!(
        session.contents().unwrap(),
        "hel",
        "the second run is its own transaction"
    );
}

#[test]
fn undo_with_nothing_to_undo_signals_rather_than_doing_nothing_quietly() {
    let mut session = editing("alpha\n");
    let before = session.signals();
    press(&mut session, "u");
    assert_eq!(session.signals(), before + 1);
}

#[test]
fn undo_in_selection_needs_a_selection_and_leaves_the_rest_alone() {
    let mut session = editing("alpha beta\n");
    // No selection: `U` requires one, and there is no fallback for it.
    let before = session.signals();
    press(&mut session, "U");
    assert_eq!(session.signals(), before + 1);
}

#[test]
fn undo_restores_the_selection_the_edit_was_made_from() {
    let mut session = editing("alpha beta\n");
    press(&mut session, "w");
    let selected = session.selection();

    press(&mut session, "k");
    press(&mut session, "u");

    assert_eq!(session.contents().unwrap(), "alpha beta\n");
    assert_eq!(
        session.selection().range(),
        selected.range(),
        "the point comes back with the text"
    );
}

// --- syntax highlighting --------------------------------------------------

#[test]
fn a_known_extension_gets_a_grammar_and_an_unknown_one_stays_plain_text() {
    let mut session = editing("fn main() {}\n");
    assert!(session.take_reparse().is_none(), "no language yet");

    session.set_path("src/main.rs");
    assert!(session.take_reparse().is_some(), "rust is recognised");

    let mut session = editing("fn main() {}\n");
    session.set_path("notes.xyzzy");
    assert!(
        session.take_reparse().is_none(),
        "a buffer with no grammar is plain text, not an error"
    );
}

#[test]
fn a_finished_parse_produces_styled_runs() {
    let mut session = editing("fn main() {\n    let s = \"hi\";\n}\n");
    session.set_path("t.rs");

    let (buffer, job) = session.take_reparse().expect("a parse is due");
    let revision = job.revision;
    let syntax = job.run(&session.languages());
    assert!(syntax.is_some(), "the parse succeeded");
    session.apply_reparse(buffer, revision, syntax);

    let styled = session.styled();
    assert!(!styled.is_empty(), "nothing was highlighted");
    assert!(
        styled.iter().any(|s| s.name == "keyword"),
        "no keyword in {styled:?}"
    );
}

#[test]
fn a_stale_parse_is_dropped_rather_than_applied() {
    // The buffer moved on while the worker was running, so the tree it
    // produced does not describe the text any more.
    let mut session = editing("fn main() {}\n");
    session.set_path("t.rs");
    let (buffer, job) = session.take_reparse().unwrap();
    let syntax = job.run(&session.languages());

    press(&mut session, "s"); // Insert State
    press(&mut session, "x"); // an edit, bumping the revision

    assert_eq!(session.apply_reparse(buffer, 0, syntax), Action::Nothing);
    assert!(session.styled().is_empty(), "the stale tree was not kept");
}

#[test]
fn an_edit_schedules_an_incremental_reparse_carrying_the_change() {
    let mut session = editing("fn main() {}\n");
    session.set_path("t.rs");
    let (buffer, job) = session.take_reparse().unwrap();
    let revision = job.revision;
    let languages = session.languages();
    session.apply_reparse(buffer, revision, job.run(&languages));

    press(&mut session, "s");
    press(&mut session, "x");

    let (_, next) = session.take_reparse().expect("the edit scheduled a parse");
    assert!(
        next.syntax.is_some(),
        "the existing tree is handed back for reuse, not thrown away"
    );
    assert_eq!(next.edits.len(), 1, "with the edit that caused it");
    assert!(next.revision > revision);
}

// --- windows and buffers --------------------------------------------------

#[test]
fn splitting_gives_each_window_its_own_point_and_scroll() {
    // The reason to split on one buffer at all: two views of the same file
    // that move independently.
    let mut session = editing("a\nb\nc\nd\ne\nf\ng\nh\n");
    session.on_resize(80, 4);

    session.on_key(Key::Ctrl('x'));
    session.on_key(Key::Char('2'));
    assert_eq!(session.window_count(), 2);

    // Both windows show the same buffer.
    let ids: Vec<_> = session.windows().order();
    assert_eq!(
        session.windows().window(ids[0]).buffer,
        session.windows().window(ids[1]).buffer
    );

    // Scroll the focused one down; the other stays put.
    let other = ids
        .iter()
        .find(|id| **id != session.focus())
        .copied()
        .unwrap();
    let before = session.windows().window(other).top;
    for _ in 0..6 {
        session.on_key(Key::Char('n'));
    }
    assert!(
        session.windows().window(session.focus()).top > 0,
        "it scrolled"
    );
    assert_eq!(
        session.windows().window(other).top,
        before,
        "the other did not"
    );
}

#[test]
fn moving_between_and_closing_windows_works_including_closing_all_but_one() {
    let mut session = editing("alpha\n");
    session.on_key(Key::Ctrl('x'));
    session.on_key(Key::Char('2'));
    session.on_key(Key::Ctrl('x'));
    session.on_key(Key::Char('3'));
    assert_eq!(session.window_count(), 3);

    let before = session.focus();
    session.on_key(Key::Ctrl('x'));
    session.on_key(Key::Char('o'));
    assert_ne!(session.focus(), before, "other-window moved");

    session.on_key(Key::Ctrl('x'));
    session.on_key(Key::Char('0'));
    assert_eq!(session.window_count(), 2, "delete-window");

    session.on_key(Key::Ctrl('x'));
    session.on_key(Key::Char('1'));
    assert_eq!(session.window_count(), 1, "delete-other-windows");
}

#[test]
fn closing_the_last_window_is_refused_rather_than_quitting() {
    let mut session = editing("alpha\n");
    let before = session.signals();
    session.on_key(Key::Ctrl('x'));
    session.on_key(Key::Char('0'));
    assert_eq!(session.window_count(), 1);
    assert_eq!(session.signals(), before + 1, "it signals, as Emacs does");
}

#[test]
fn several_buffers_can_be_open_and_a_window_switches_between_them() {
    let mut session = editing("first buffer\n");
    let mut second = Buffer::scratch();
    second.loaded(Document::from_text("second buffer\n"));
    let id = session.open(second);

    assert_eq!(session.buffers().len(), 2);
    assert_eq!(session.contents().unwrap(), "second buffer\n");

    // `C-x <right>`, which is where Emacs puts `next-buffer`. It was `C-x b`
    // here until #103 gave that key back to choosing a buffer.
    session.on_key(Key::Ctrl('x'));
    session.on_key(Key::Right);
    assert_ne!(session.current_buffer_id(), id, "it cycled");
    assert_eq!(session.contents().unwrap(), "first buffer\n");
}

#[test]
fn each_buffer_keeps_its_own_selection_history_and_undo() {
    // Decision #5: Meow's selection history is global and survives a buffer
    // switch, which lets `z` pop a selection belonging to a file you are no
    // longer looking at. meowr diverges on purpose.
    let mut session = editing("alpha beta\n");
    press(&mut session, "wk"); // kill in the first buffer
    assert_eq!(session.contents().unwrap(), " beta\n");
    let depth = session.history_depth();
    assert!(depth > 0);

    let mut second = Buffer::scratch();
    second.loaded(Document::from_text("untouched\n"));
    session.open(second);

    assert_eq!(
        session.history_depth(),
        0,
        "a fresh buffer, a fresh history"
    );
    press(&mut session, "u");
    assert_eq!(
        session.contents().unwrap(),
        "untouched\n",
        "undo did not reach into the other buffer"
    );

    session.on_key(Key::Ctrl('x'));
    session.on_key(Key::Right);
    assert_eq!(session.history_depth(), depth, "and the first kept its own");
}

#[test]
fn the_kill_ring_is_shared_between_buffers_on_purpose() {
    // The deliberate exception to per-buffer state: copying in one buffer and
    // yanking in another is what a kill ring is for.
    let mut session = editing("copied text\n");
    press(&mut session, "wy");

    let mut second = Buffer::scratch();
    second.loaded(Document::from_text(""));
    session.open(second);
    press(&mut session, "p");

    assert_eq!(session.contents().unwrap(), "copied");
}

#[test]
fn resizing_redistributes_the_windows() {
    let mut session = editing("alpha\n");
    session.on_resize(80, 24);
    session.on_key(Key::Ctrl('x'));
    session.on_key(Key::Char('3'));

    session.on_resize(40, 24);
    let areas = session.layout();

    assert_eq!(areas.len(), 2);
    let total: u16 = areas.iter().map(|(_, a)| a.width).sum();
    assert_eq!(total, 40, "the columns narrow rather than clip");
}

#[test]
fn each_window_has_its_own_modeline_describing_its_own_buffer() {
    let mut session = editing("alpha beta\n");
    session.set_path("/home/user/tools/meow/src/first.rs");
    session.on_resize(80, 12);

    let mut second = Buffer::scratch();
    second.loaded(Document::from_text("other\n"));
    second.set_path("/home/user/tools/meow/src/second.rs", &session.languages());

    session.on_key(Key::Ctrl('x'));
    session.on_key(Key::Char('2'));
    session.open(second);

    let ids = session.windows().order();
    let first = session.modeline_of(ids[0]).text();
    let other = session.modeline_of(ids[1]).text();

    assert!(first.contains("first.rs"), "{first}");
    assert!(other.contains("second.rs"), "{other}");
    assert!(
        first.contains("meow/src/first.rs"),
        "three path segments: {first}"
    );
}

#[test]
fn the_modeline_shows_the_state_and_the_selection_type() {
    let mut session = editing("alpha beta gamma\n");
    session.on_resize(100, 12);
    assert!(
        session
            .modeline_of(session.focus())
            .text()
            .contains("NORMAL")
    );

    press(&mut session, "l");
    let text = session.modeline_of(session.focus()).text();
    assert!(text.contains("expand·line"), "{text}");

    press(&mut session, "s");
    assert!(
        session
            .modeline_of(session.focus())
            .text()
            .contains("INSERT")
    );
}

#[test]
fn only_the_focused_window_shows_the_expand_hint() {
    // The hint is about what *your* next key would do.
    let mut session = editing("alpha beta gamma\n");
    session.on_resize(100, 12);
    session.on_key(Key::Ctrl('x'));
    session.on_key(Key::Char('2'));
    press(&mut session, "w");

    let focused = session.modeline_of(session.focus()).text();
    let other = session
        .windows()
        .order()
        .into_iter()
        .find(|id| *id != session.focus())
        .map(|id| session.modeline_of(id).text())
        .unwrap();

    assert!(focused.starts_with("NORMAL "), "{focused}");
    assert!(other.starts_with("NORMAL "), "{other}");
    assert!(
        focused
            .split_whitespace()
            .nth(1)
            .unwrap()
            .parse::<usize>()
            .is_ok(),
        "the focused one carries a count: {focused}"
    );
}

// --- the narrowing widget -------------------------------------------------

#[test]
fn a_late_answer_about_another_buffer_does_not_evict_a_good_one() {
    // ADR-0009 drops the *stale* answer. Storing it instead threw away the
    // answer about the buffer the picker is actually on, so a reply about
    // somewhere else turned this picker into an empty list.
    let mut session = editing_a_file("struct Widget {}\n");
    session.set_project(Some("main.rs".into()), Ok(vec!["kept.rs".into()]));

    session.set_project(
        Some("/elsewhere/other.rs".into()),
        Ok(vec!["intruder.rs".into()]),
    );

    session.on_key(Key::Ctrl('x'));
    session.on_key(Key::Ctrl('f'));
    session.on_key(Key::Char('<'));
    assert_eq!(
        session.on_key(Key::Char('p')),
        Action::Redraw,
        "the good answer is still here, so nothing is re-asked"
    );
    let widget = session.narrowing().expect("open");
    let shown: Vec<&str> = widget
        .matches()
        .iter()
        .map(|found| widget.candidate(found.index).text.as_str())
        .collect();
    assert_eq!(shown, ["kept.rs"], "and it is the one that was kept");
}

#[test]
fn a_refusal_about_another_buffer_is_not_drawn_under_this_picker() {
    // The drawn half of ADR-0009's two checks. The rows carried the stamp from
    // the start and the note did not, so a refusal about a file the user had
    // left was shown under the picker for one they had not.
    let mut session = editing_a_file("struct Widget {}\n");
    session.set_project(
        Some("/elsewhere/notes.md".into()),
        Err("/elsewhere is in no git repository".into()),
    );

    session.on_key(Key::Ctrl('x'));
    session.on_key(Key::Ctrl('f'));
    session.on_key(Key::Char('<'));
    session.on_key(Key::Char('p'));

    assert_eq!(
        session.narrow_view().expect("open").note,
        None,
        "that refusal is about a buffer this picker is not showing"
    );
}

#[test]
fn a_project_answer_does_not_leak_into_the_m_x_picker() {
    // `M-x` and `meow-visit` build a picker with *no* sources, so nothing
    // filters by source: extending one turned file paths into command
    // candidates, and accepting one answered "`kept.rs` is not a command".
    let mut session = editing_a_file("struct Widget {}\n");

    session.on_key(Key::Ctrl('x'));
    session.on_key(Key::Ctrl('f'));
    session.on_key(Key::Char('<'));
    assert_eq!(session.on_key(Key::Char('p')), Action::Project);
    let asked = session.pending_project().expect("a question to put");

    // The picker goes away and `M-x` opens before the answer lands.
    session.on_key(Key::Esc);
    session.on_key(Key::Alt('x'));
    let before = session.narrowing().expect("M-x is open").matches().len();

    session.set_project(asked, Ok(vec!["zzleaked.rs".into()]));

    let widget = session.narrowing().expect("M-x is still open");
    assert_eq!(
        widget.matches().len(),
        before,
        "no file paths joined the command list"
    );
    assert!(
        !widget
            .matches()
            .iter()
            .any(|found| widget.candidate(found.index).text == "zzleaked.rs"),
    );
}

#[test]
fn the_ordinary_picker_asks_for_a_project_too() {
    // The third path, and the one that was missed. `open_picker_on` and the
    // `<p` branch both ask; `find-file` -- `C-x C-f`, `SPC f f`, and the way
    // the picker is actually opened -- called `open_picker` and returned
    // `Redraw`. So the picker came up advertising four sources with the
    // project one empty, and nothing on screen to say why: a question nobody
    // put reads exactly like a repository with nothing in it.
    //
    // A regression rather than a gap. Before laziness the host seeded the
    // answer at startup, so this path had one without asking.
    let mut session = editing_a_file("struct Widget {}\n");

    session.on_key(Key::Ctrl('x'));
    assert_eq!(session.on_key(Key::Ctrl('f')), Action::Project);
    assert_eq!(session.pending_project(), Some(Some("main.rs".to_string())));
    assert!(session.narrowing().is_some(), "and the picker is open");
}

#[test]
fn opening_the_picker_already_on_the_project_source_asks_too() {
    // The other way to land on a source. `SPC SPC` comes in through here
    // (#147), and the ask used to live only in the `<p` branch -- so this path
    // would have opened an empty project picker and never asked anything.
    let mut session = editing_a_file("struct Widget {}\n");

    assert_eq!(session.open_picker_on('p'), Action::Project);
    assert_eq!(session.pending_project(), Some(Some("main.rs".to_string())));
}

#[test]
fn a_project_that_lands_while_the_picker_is_open_reaches_it() {
    // The half laziness adds: a picker's candidates were fixed when it opened,
    // and now the answer arrives after. Without this the rows are computed,
    // sent, stored -- and never shown, which looks exactly like a repository
    // with nothing in it.
    let mut session = editing_a_file("struct Widget {}\n");

    session.on_key(Key::Ctrl('x'));
    session.on_key(Key::Ctrl('f'));
    session.on_key(Key::Char('<'));
    assert_eq!(session.on_key(Key::Char('p')), Action::Project);
    let asked = session.pending_project().expect("a question to put");

    assert_eq!(
        session.narrowing().expect("open").matches().len(),
        0,
        "nothing to show until the answer lands"
    );

    session.set_project(asked, Ok(vec!["README.md".into(), "Cargo.toml".into()]));

    let widget = session.narrowing().expect("still open");
    let shown: Vec<&str> = widget
        .matches()
        .iter()
        .map(|found| widget.candidate(found.index).text.as_str())
        .collect();
    assert_eq!(
        shown.len(),
        2,
        "the answer reached the open picker: {shown:?}"
    );
    assert!(shown.contains(&"README.md"));
}

#[test]
fn a_refusal_that_lands_while_the_picker_is_open_is_spoken() {
    // The same half for the other answer. `project_asked_for` cannot refuse at
    // the moment `<p` is pressed, because at that moment nothing has been
    // asked yet -- so the sentence has to be said when it arrives.
    let mut session = editing_a_file("struct Widget {}\n");

    session.on_key(Key::Ctrl('x'));
    session.on_key(Key::Ctrl('f'));
    session.on_key(Key::Char('<'));
    assert_eq!(session.on_key(Key::Char('p')), Action::Project);
    let asked = session.pending_project().expect("a question to put");
    assert_eq!(
        session.narrow_view().expect("open").note,
        None,
        "nothing claimed before the answer arrives"
    );

    session.set_project(
        asked,
        Err("/home/user/docs is in no git repository".into()),
    );

    assert_eq!(
        session.narrow_view().expect("open").note.as_deref(),
        Some("/home/user/docs is in no git repository")
    );
}

#[test]
fn the_project_is_asked_for_when_a_picker_opens_and_not_before() {
    // #130 asks lazily, and this is where "lazily" stops. It used to mean "not
    // until `<p`", and that was a rule this file's *other* half contradicted:
    // `picker_candidates` puts project rows into the un-narrowed list, so a
    // picker opened and never narrowed is already a consumer of the answer.
    // Under the old rule it consumed one nobody had asked for, and came up
    // missing every project file with nothing said.
    //
    // The laziness that was actually worth having survives intact: nothing at
    // startup, nothing per buffer, and nothing at all in a session that never
    // opens a picker -- which is most of them. What it cost was one rule that
    // two pieces of code disagreed about.
    let mut session = editing_a_file("struct Widget {}\n");

    assert_eq!(
        session.pending_project(),
        None,
        "a session that opened no picker asked nothing"
    );

    session.on_key(Key::Ctrl('x'));
    assert_eq!(session.on_key(Key::Ctrl('f')), Action::Project);
    assert_eq!(
        session.pending_project(),
        Some(Some("main.rs".to_string())),
        "and it names the buffer it is about"
    );
}

#[test]
fn a_project_already_answered_about_this_buffer_is_not_asked_again() {
    // The cache, keyed on the buffer the answer is about rather than on
    // nothing: asking git per keystroke would be the subprocess-per-frame
    // mistake `branch_of` already refuses to make.
    let mut session = editing_a_file("struct Widget {}\n");
    session.set_project(Some("main.rs".into()), Ok(vec!["src/lib.rs".into()]));

    session.on_key(Key::Ctrl('x'));
    session.on_key(Key::Ctrl('f'));
    session.on_key(Key::Char('<'));

    assert_eq!(session.on_key(Key::Char('p')), Action::Redraw);
    assert_eq!(
        session.pending_project(),
        None,
        "the answer was already here"
    );
}

#[test]
fn an_answer_about_another_buffer_is_asked_again_rather_than_offered() {
    // ADR-0009's stamp doing its job: two buffers from two repositories is the
    // ordinary case, so an answer about one of them is not an answer about the
    // other -- and showing its files would offer paths that do not resolve.
    let mut session = editing_a_file("struct Widget {}\n");
    session.set_project(
        Some("/elsewhere/other.rs".into()),
        Ok(vec!["/elsewhere/src/lib.rs".into()]),
    );

    session.on_key(Key::Ctrl('x'));
    session.on_key(Key::Ctrl('f'));
    let widget = session.narrowing().expect("the picker opened");
    assert!(
        !widget
            .matches()
            .iter()
            .any(|m| widget.candidate(m.index).text.contains("/elsewhere/")),
        "a stale Project offers nothing"
    );

    session.on_key(Key::Char('<'));
    assert_eq!(session.on_key(Key::Char('p')), Action::Project);
    assert_eq!(session.pending_project(), Some(Some("main.rs".to_string())));
}

#[test]
fn narrowing_to_the_project_source_outside_a_repository_says_why() {
    // ADR-0010 takes the refusal rather than a fallback, and the sentence names
    // what was missing. Said when the source is *asked for*: ADR-0009's rule
    // about an answer nobody wanted reaches a refusal too, so opening a file in
    // a directory that is no repository announces nothing by itself.
    let mut session = editing("alpha\n");
    session.set_project(
        None,
        Err("/home/user/docs is in no git repository".into()),
    );

    session.on_key(Key::Ctrl('x'));
    session.on_key(Key::Ctrl('f'));
    assert_eq!(session.echo(), None, "nobody has asked for the project yet");

    session.on_key(Key::Char('<'));
    session.on_key(Key::Char('p'));

    assert_eq!(
        session
            .narrow_view()
            .expect("the picker is open")
            .note
            .as_deref(),
        Some("/home/user/docs is in no git repository"),
        "on the widget's own line: an open picker owns the Bottom (ADR-0004), so a \
         refusal sent to the Echo Area is one nobody ever sees"
    );
}

#[test]
fn the_picker_offers_all_four_sources_and_narrows_to_one() {
    let mut session = editing("alpha\nbeta\ngamma\n");
    session.set_recent_files(vec!["/tmp/recent.rs".into()]);
    session.set_project(None, Ok(vec!["/src/project.rs".into()]));

    session.on_key(Key::Ctrl('x'));
    session.on_key(Key::Ctrl('f'));
    let widget = session.narrowing().expect("the picker opened");
    assert_eq!(widget.sources().len(), 4, "buffers, recent, project, line");
    assert!(widget.matches().len() >= 5);

    // `<` then a source key.
    session.on_key(Key::Char('<'));
    session.on_key(Key::Char('l'));
    let widget = session.narrowing().unwrap();
    assert_eq!(widget.narrowed().map(|s| s.name.as_str()), Some("line"));
    assert_eq!(widget.matches().len(), 3, "the three lines of the buffer");
}

#[test]
fn typing_narrows_and_escape_closes() {
    let mut session = editing("alpha\nbeta\ngamma\n");
    session.on_key(Key::Ctrl('x'));
    session.on_key(Key::Ctrl('f'));

    for c in "gam".chars() {
        session.on_key(Key::Char(c));
    }
    let widget = session.narrowing().unwrap();
    assert_eq!(widget.input(), "gam");
    assert!(
        widget.selected().is_some_and(|c| c.text.contains("gamma")),
        "{:?}",
        widget.selected()
    );

    session.on_key(Key::Esc);
    assert!(session.narrowing().is_none());
}

#[test]
fn the_widget_takes_every_key_while_it_is_open() {
    // It is a modal read, like the minibuffer. Routing keys through the
    // ordinary keymap first would make every binding a collision with typing
    // a filename.
    let mut session = editing("alpha beta\n");
    let before = session.contents().unwrap();
    session.on_key(Key::Ctrl('x'));
    session.on_key(Key::Ctrl('f'));

    for c in "kill".chars() {
        session.on_key(Key::Char(c));
    }

    assert_eq!(session.contents().unwrap(), before, "`k` did not kill");
    session.on_key(Key::Esc);
}

#[test]
fn accepting_a_line_moves_the_point_to_it() {
    let mut session = editing("alpha\nbeta\ngamma\n");
    session.on_key(Key::Ctrl('x'));
    session.on_key(Key::Ctrl('f'));
    session.on_key(Key::Char('<'));
    session.on_key(Key::Char('l'));
    for c in "gam".chars() {
        session.on_key(Key::Char(c));
    }
    // The preview target is the line, so the window behind can scroll to it.
    assert_eq!(session.preview_target(), Some(Preview::Line(3)));

    session.on_key(Key::Enter);

    assert!(session.narrowing().is_none());
    assert_eq!(session.point(), 11, "the start of the third line");
}

#[test]
fn completion_replaces_the_prefix_under_the_cursor() {
    // The same widget: only the driver differs.
    let mut session = editing("selection_type and sel\n");
    press(&mut session, "s"); // Insert State
    for _ in 0..3 {
        session.on_key(Key::Char('x'));
    }
    // Undo the noise; put the point after the trailing `sel`.
    let mut session = editing("selection_type and sel");
    session.set_selection(Selection::at(22));
    press(&mut session, "s");

    session.on_key(Key::Ctrl('n'));
    let widget = session.narrowing().expect("completion opened");
    assert_eq!(widget.driver(), meowr_narrow::Driver::Completion);
    assert_eq!(widget.input(), "sel");

    session.on_key(Key::Enter);

    assert_eq!(
        session.contents().unwrap(),
        "selection_type and selection_type"
    );
}

#[test]
fn return_inserts_a_newline_in_insert_state() {
    // `Key::Enter` is its own variant rather than `Char('\n')`: a key and the
    // character it usually inserts are different things.
    let mut session = editing("");
    press(&mut session, "s");
    session.on_key(Key::Char('a'));
    session.on_key(Key::Enter);
    session.on_key(Key::Char('b'));
    assert_eq!(session.contents().unwrap(), "a\nb");
}

#[test]
fn the_preview_scrolls_the_window_behind_rather_than_opening_a_panel() {
    // ADR-0004 leaves nowhere else for a preview to go — and it turns out to
    // be better, because the candidate is seen in its real surroundings.
    let mut session = editing("alpha\nbeta\ngamma\ndelta\nepsilon\n");
    session.on_resize(80, 6);
    session.on_key(Key::Ctrl('x'));
    session.on_key(Key::Ctrl('f'));
    session.on_key(Key::Char('<'));
    session.on_key(Key::Char('l'));
    for c in "eps".chars() {
        session.on_key(Key::Char(c));
    }

    assert_eq!(session.preview_target(), Some(Preview::Line(5)));
    assert_eq!(session.preview(), Action::Redraw);
    assert_eq!(session.point(), 23, "the window behind moved to the line");
}

// --- a preview of an open file is the buffer, not the disk (#153) ----------

/// `main.rs` on screen and `other.rs` open behind it with its point at 0, and a
/// picker over one Place in each -- `other.rs` first.
fn two_open_files_and_a_picker_over_both() -> (Session, BufferId, BufferId) {
    let mut session = editing_a_file("fn main() {}\nlet a = 1;\n");
    let main = session.current_buffer_id();
    let mut other = Buffer::scratch();
    other.set_path("other.rs", &session.languages());
    // Edited and unsaved, as far as the disk knows: whatever is on disk, this
    // is the text a preview of `other.rs` has to show.
    other.loaded(Document::from_text("zero\none\nEDITED two\nthree\n"));
    let other = session.open(other);
    session.show(main);
    press(&mut session, " kD");
    let asked = session.pending_lookup().expect("a question");
    session.found(
        &asked.path,
        vec![
            meowr_domain::Place::new("other.rs", 2, 0),
            meowr_domain::Place::new("main.rs", 1, 4),
        ],
    );
    (session, main, other)
}

#[test]
fn previewing_a_row_in_an_open_file_shows_the_buffer_and_reads_nothing() {
    let (mut session, _main, other) = two_open_files_and_a_picker_over_both();

    assert_eq!(session.preview_read(), None, "nothing for the host to read");
    assert_eq!(session.preview(), Action::Redraw);
    assert_eq!(session.current_buffer_id(), other, "the open buffer, shown");
    assert_eq!(session.point(), "zero\none\n".len(), "on the line named");
    assert_eq!(session.buffers().len(), 2, "and no preview slot was filled");
}

#[test]
fn a_row_in_a_file_nobody_has_open_is_still_read() {
    let mut session = editing_a_file("fn main() {}\n");
    press(&mut session, " kD");
    let asked = session.pending_lookup().expect("a question");
    session.found(
        &asked.path,
        vec![
            meowr_domain::Place::new("closed.rs", 3, 0),
            meowr_domain::Place::new("main.rs", 0, 0),
        ],
    );
    assert_eq!(session.preview_read(), Some("closed.rs".to_string()));
}

#[test]
fn cancelling_puts_back_the_point_a_preview_moved_in_another_buffer() {
    // `consult--jump-preview` saves the point of every buffer it moves and puts
    // it back when the preview moves on or the picker closes.
    let (mut session, main, other) = two_open_files_and_a_picker_over_both();
    session.preview();
    assert_eq!(session.current_buffer_id(), other);

    session.cancel_narrowing();
    assert_eq!(session.current_buffer_id(), main, "the window behind, back");
    session.show(other);
    assert_eq!(session.point(), 0, "and other.rs's point where it was");
}

#[test]
fn moving_to_the_next_row_puts_back_what_the_last_preview_moved() {
    let (mut session, main, other) = two_open_files_and_a_picker_over_both();
    session.preview();
    session.narrowing_mut().expect("open").select_next();
    session.preview();
    assert_eq!(session.current_buffer_id(), main, "the second row's file");

    session.show(other);
    assert_eq!(session.point(), 0, "the first row's move undone");
}

#[test]
fn accepting_a_row_the_preview_had_not_reached_puts_back_what_it_moved() {
    // Enter is not on the preview's timer. Preview a row in `other.rs`, move
    // to the next row and accept before it is previewed, and `other.rs` would
    // keep a point nobody chose -- consult runs its restore list on every way
    // out, accepting included, and jumps after.
    let (mut session, main, other) = two_open_files_and_a_picker_over_both();
    session.preview();
    session.narrowing_mut().expect("open").select_next();
    session.accept_narrowing();
    assert_eq!(session.current_buffer_id(), main, "the accepted row's file");

    session.show(other);
    assert_eq!(session.point(), 0, "other.rs's point where it was");
}

#[test]
fn a_file_row_naming_the_buffer_on_screen_leaves_the_window_alone() {
    // A project row for the file being edited is that buffer, and showing it
    // again would put its window back at the head of the file with the point
    // somewhere below -- once per preview tick.
    let mut session = editing_a_file("one\ntwo\nthree\n");
    session.set_project(Some("main.rs".into()), Ok(vec!["main.rs".into()]));
    press(&mut session, "  ");
    let here = session.current_buffer_id();
    assert_eq!(session.preview_target(), Some(Preview::Buffer(here)));
    assert_eq!(session.preview(), Action::Nothing);
    assert_eq!(session.preview_read(), None);
}

#[test]
fn accepting_keeps_the_move_the_preview_made() {
    let (mut session, _main, other) = two_open_files_and_a_picker_over_both();
    session.preview();
    session.accept_narrowing();
    assert_eq!(session.current_buffer_id(), other);
    assert_eq!(session.point(), "zero\none\n".len());
}

#[test]
fn previewing_a_buffer_candidate_shows_that_buffer_behind() {
    let mut session = editing("alpha\n");
    session.set_path("first.rs");
    let mut second = Buffer::scratch();
    second.set_path("second.rs", &session.languages());
    second.loaded(Document::from_text("the other file\n"));
    let other = session.open(second);
    session.show(BufferId(0));
    let before = session.current_buffer_id();

    session.on_key(Key::Ctrl('x'));
    session.on_key(Key::Ctrl('f'));
    session.on_key(Key::Char('<'));
    session.on_key(Key::Char('b'));
    for c in "second".chars() {
        session.on_key(Key::Char(c));
    }

    assert_eq!(session.preview_target(), Some(Preview::Buffer(other)));
    assert_eq!(session.preview(), Action::Redraw);
    assert_eq!(
        session.current_buffer_id(),
        other,
        "the window behind shows it"
    );
    assert_eq!(session.rows()[0].text, "the other file");

    session.on_key(Key::Esc);
    assert_eq!(
        session.current_buffer_id(),
        before,
        "cancelling put the window back"
    );
}

#[test]
fn accepting_a_buffer_works_before_the_preview_has_fired() {
    // `consult-preview-delay` is 0.4s, and typing a name and pressing Enter
    // beats it easily. Accepting used to act only on a line candidate, so a
    // buffer chosen faster than the debounce switched to nothing at all.
    let mut session = editing("alpha\n");
    session.set_path("first.rs");
    let mut second = Buffer::scratch();
    second.set_path("second.rs", &session.languages());
    second.loaded(Document::from_text("the other file\n"));
    let other = session.open(second);
    session.show(BufferId(0));

    session.on_key(Key::Ctrl('x'));
    session.on_key(Key::Ctrl('f'));
    session.on_key(Key::Char('<'));
    session.on_key(Key::Char('b'));
    for c in "second".chars() {
        session.on_key(Key::Char(c));
    }
    // No `session.preview()` here: that is the whole point.
    session.on_key(Key::Enter);

    assert_eq!(session.current_buffer_id(), other);
    assert_eq!(session.rows()[0].text, "the other file");
}

#[test]
fn accepting_a_previewed_buffer_keeps_where_it_took_you() {
    let mut session = editing("alpha\n");
    session.set_path("first.rs");
    let mut second = Buffer::scratch();
    second.set_path("second.rs", &session.languages());
    second.loaded(Document::from_text("the other file\n"));
    let other = session.open(second);
    session.show(BufferId(0));

    session.on_key(Key::Ctrl('x'));
    session.on_key(Key::Ctrl('f'));
    session.on_key(Key::Char('<'));
    session.on_key(Key::Char('b'));
    for c in "second".chars() {
        session.on_key(Key::Char(c));
    }
    session.preview();
    session.on_key(Key::Enter);

    assert!(session.narrowing().is_none());
    assert_eq!(session.current_buffer_id(), other);
}

#[test]
fn previewing_a_file_candidate_shows_the_text_the_host_read() {
    // Reading is the host's, so the session says which file and takes the
    // text back. The two halves are separate calls for exactly that reason.
    let mut session = editing("alpha\n");
    session.set_path("first.rs");
    session.set_recent_files(vec!["/tmp/notes.rs".into()]);
    let before = session.current_buffer_id();

    session.on_key(Key::Ctrl('x'));
    session.on_key(Key::Ctrl('f'));
    session.on_key(Key::Char('<'));
    session.on_key(Key::Char('r'));

    assert_eq!(
        session.preview_target(),
        Some(Preview::File("/tmp/notes.rs".into()))
    );
    assert_eq!(
        session.previewed("/tmp/notes.rs", Ok("fn read() {}\n".into())),
        Action::Redraw
    );
    assert_eq!(session.rows()[0].text, "fn read() {}");
    assert_ne!(session.current_buffer_id(), before);
    assert!(
        session.current().is_preview(),
        "nothing has been chosen yet"
    );

    session.on_key(Key::Esc);
    assert_eq!(
        session.current_buffer_id(),
        before,
        "cancelling put the window back"
    );
}

#[test]
fn a_file_that_will_not_open_previews_as_the_reason() {
    let mut session = editing("alpha\n");
    session.set_recent_files(vec!["/tmp/gone.rs".into()]);
    session.on_key(Key::Ctrl('x'));
    session.on_key(Key::Ctrl('f'));
    session.on_key(Key::Char('<'));
    session.on_key(Key::Char('r'));

    session.previewed("/tmp/gone.rs", Err("no such file: /tmp/gone.rs".into()));

    assert_eq!(session.error(), Some("no such file: /tmp/gone.rs"));
}

#[test]
fn a_preview_that_arrives_after_the_selection_moved_on_is_dropped() {
    // The read is in flight while the keys keep coming, so the text can be
    // for a candidate nobody is looking at any more — the same staleness the
    // reparse worker has, and the same answer.
    let mut session = editing("alpha\n");
    session.set_recent_files(vec!["/tmp/one.rs".into(), "/tmp/two.rs".into()]);
    session.on_key(Key::Ctrl('x'));
    session.on_key(Key::Ctrl('f'));
    session.on_key(Key::Char('<'));
    session.on_key(Key::Char('r'));
    for c in "two".chars() {
        session.on_key(Key::Char(c));
    }

    assert_eq!(
        session.previewed("/tmp/one.rs", Ok("stale\n".into())),
        Action::Nothing
    );
}

#[test]
fn accepting_a_previewed_file_keeps_it_and_it_stops_being_a_preview() {
    let mut session = editing("alpha\n");
    session.set_recent_files(vec!["/tmp/notes.rs".into()]);
    session.on_key(Key::Ctrl('x'));
    session.on_key(Key::Ctrl('f'));
    session.on_key(Key::Char('<'));
    session.on_key(Key::Char('r'));
    session.previewed("/tmp/notes.rs", Ok("fn read() {}\n".into()));

    session.on_key(Key::Enter);

    assert!(session.narrowing().is_none());
    assert_eq!(session.rows()[0].text, "fn read() {}");
    assert_eq!(session.path(), Some("/tmp/notes.rs"));
    assert!(
        !session.current().is_preview(),
        "the choice that would make it real has been made"
    );
}

#[test]
fn a_preview_buffer_is_not_offered_as_a_buffer_to_switch_to() {
    // It is not a buffer anyone opened. Listing it would make the picker
    // offer its own leavings.
    let mut session = editing("alpha\n");
    session.set_path("first.rs");
    session.set_recent_files(vec!["/tmp/notes.rs".into()]);
    session.on_key(Key::Ctrl('x'));
    session.on_key(Key::Ctrl('f'));
    session.on_key(Key::Char('<'));
    session.on_key(Key::Char('r'));
    session.previewed("/tmp/notes.rs", Ok("fn read() {}\n".into()));
    session.on_key(Key::Esc);

    session.on_key(Key::Ctrl('x'));
    session.on_key(Key::Ctrl('f'));
    session.on_key(Key::Char('<'));
    session.on_key(Key::Char('b'));
    let widget = session.narrowing().expect("the picker opened");
    assert_eq!(widget.matches().len(), 1, "only the file that was opened");
}

#[test]
fn accepting_a_file_works_before_the_preview_has_fired() {
    // The sibling of `accepting_a_buffer_works_before_the_preview_has_fired`.
    // A buffer is already in memory, so accepting one is a pointer move the
    // session can make alone; a file has to be read, so accepting one is a
    // request to the host. Both have to survive Enter beating the 0.4s
    // `consult-preview-delay`, and only the buffer did.
    let mut session = editing("alpha\n");
    session.set_path("first.rs");
    session.set_recent_files(vec!["/tmp/notes.rs".into()]);
    session.on_key(Key::Ctrl('x'));
    session.on_key(Key::Ctrl('f'));
    session.on_key(Key::Char('<'));
    session.on_key(Key::Char('r'));
    // No `session.previewed(...)` here: that is the whole point.

    assert_eq!(session.on_key(Key::Enter), Action::Open);
    assert_eq!(
        session.pending_open(),
        Some("/tmp/notes.rs"),
        "the session says which file; reading it is the host's"
    );

    session.opened("/tmp/notes.rs", Opened::Read("fn read() {}\n".into()));
    assert_eq!(session.path(), Some("/tmp/notes.rs"));
    assert_eq!(session.rows()[0].text, "fn read() {}");
    assert!(
        !session.current().is_preview(),
        "it was chosen, so it is a buffer that was opened"
    );
    assert!(session.pending_open().is_none(), "the request is spent");
}

#[test]
fn accepting_a_file_the_preview_already_read_asks_the_host_for_nothing() {
    // The preview fired and the text is already here. Reading it again would
    // be a second read of a file that has not changed, and would throw away
    // the parse the preview has already paid for.
    let mut session = editing("alpha\n");
    session.set_path("first.rs");
    session.set_recent_files(vec!["/tmp/notes.rs".into()]);
    session.on_key(Key::Ctrl('x'));
    session.on_key(Key::Ctrl('f'));
    session.on_key(Key::Char('<'));
    session.on_key(Key::Char('r'));
    session.previewed("/tmp/notes.rs", Ok("fn read() {}\n".into()));

    assert_eq!(session.on_key(Key::Enter), Action::Redraw);
    assert!(session.pending_open().is_none());
    assert_eq!(session.path(), Some("/tmp/notes.rs"));
    assert!(!session.current().is_preview());
}

#[test]
fn next_buffer_does_not_cycle_into_a_preview_buffer() {
    // `picker_candidates` hides the leavings of a cancelled preview because
    // nobody opened them. `next-buffer` walked the list itself and so did not,
    // which put you in a read-only buffer that #52 says nothing can close.
    let mut session = editing("alpha\n");
    session.set_path("first.rs");
    session.set_recent_files(vec!["/tmp/notes.rs".into()]);
    session.on_key(Key::Ctrl('x'));
    session.on_key(Key::Ctrl('f'));
    session.on_key(Key::Char('<'));
    session.on_key(Key::Char('r'));
    session.previewed("/tmp/notes.rs", Ok("fn read() {}\n".into()));
    session.on_key(Key::Esc);
    assert_eq!(session.path(), Some("first.rs"), "cancelling put it back");

    press(&mut session, " bb");

    assert_eq!(
        session.path(),
        Some("first.rs"),
        "the only buffer anyone opened is the only one to cycle to"
    );
}

#[test]
fn saving_writes_the_file_the_current_buffer_visits() {
    // The host used to write `session.contents()` to the path its own command
    // line named, which was right while a session had one path-bearing buffer
    // and became data loss the moment previewing could make a second.
    let mut session = editing("alpha\n");
    session.set_path("first.rs");
    let mut second = Buffer::scratch();
    second.set_path("second.rs", &session.languages());
    second.loaded(Document::from_text("the other file\n"));
    let other = session.open(second);
    session.show(other);

    assert_eq!(
        session.save_target(),
        Some(meowr_app::Write {
            path: "second.rs".into(),
            contents: "the other file\n".into(),
            // A file meowr opened is a file it may write back: only a name
            // just typed makes the host look first.
            fresh: false,
        }),
        "the buffer on screen, not the one the command line named"
    );
}

#[test]
fn a_buffer_nobody_chose_to_open_has_nothing_to_save() {
    // A preview is read-only because the choice that would make it real has
    // not been made. Writing one out would put a file you were only looking at
    // over a file you never named.
    let mut session = editing("alpha\n");
    session.set_path("first.rs");
    session.set_recent_files(vec!["/tmp/notes.rs".into()]);
    session.on_key(Key::Ctrl('x'));
    session.on_key(Key::Ctrl('f'));
    session.on_key(Key::Char('<'));
    session.on_key(Key::Char('r'));
    session.previewed("/tmp/notes.rs", Ok("fn read() {}\n".into()));

    assert_eq!(session.save_target(), None);
}

// --- `M-x`: running a command by name (#91) --------------------------------

/// Type a whole command name into the picker.
fn type_name(session: &mut Session, name: &str) {
    for c in name.chars() {
        session.on_key(Key::Char(c));
    }
}

#[test]
fn m_x_runs_a_command_by_name_exactly_as_its_key_does() {
    // The point of naming commands at all: the name and the key are two ways
    // to the same behaviour, not two behaviours. This is also the keyless
    // dispatch path #88 hardened -- a command that reads a parameter out of
    // `ctx.key` would answer differently here, which is the bug that ticket
    // was about.
    let by_key = {
        let mut session = editing("alpha beta\n");
        press(&mut session, "w");
        session.selection().range()
    };

    let mut session = editing("alpha beta\n");
    session.on_key(Key::Alt('x'));
    type_name(&mut session, "next-word");
    session.on_key(Key::Enter);

    assert_eq!(session.selection().range(), by_key);
    assert!(session.narrowing().is_none(), "the picker closes behind it");
}

#[test]
fn m_x_offers_every_name_the_registry_has_including_the_ones_no_key_reaches() {
    // `Registry::names()` has existed as long as the registry and had no
    // caller until this one. The seventy-six per-Thing names are the reason
    // it matters: #78 registered them *so that they could be named*, and a
    // prompt that spells names while hiding them would undo that.
    let mut session = editing("alpha\n");

    session.on_key(Key::Alt('x'));

    let widget = session.narrowing().expect("`M-x` opens the picker");
    let offered: Vec<&str> = widget
        .matches()
        .iter()
        .map(|m| widget.candidate(m.index).text.as_str())
        .collect();
    assert_eq!(
        offered.len(),
        meowr_app::default_registry().names().count(),
        "every name, not a chosen subset"
    );
    assert!(offered.contains(&"inner-of-thing/round"), "a Thing name");
    assert!(offered.contains(&"undo"), "and an ordinary one");
}

#[test]
fn cancelling_m_x_leaves_the_buffer_and_the_selection_where_they_were() {
    let mut session = editing("alpha beta\n");
    press(&mut session, "w");
    let before = session.selection().range();

    session.on_key(Key::Alt('x'));
    type_name(&mut session, "undo");
    session.on_key(Key::Esc);

    assert!(session.narrowing().is_none());
    assert_eq!(session.selection().range(), before);
    assert_eq!(session.contents().unwrap(), "alpha beta\n");
    assert_eq!(session.state(), State::Normal);
}

#[test]
fn m_x_says_why_a_command_it_offered_will_not_run_here() {
    // #80's bar: no wordless beep. The registry is one list and every name in
    // it is offerable, but `beacon-start` is Beacon's alone -- so the prompt
    // that let you choose it owes you the reason it did nothing.
    let mut session = editing("alpha\n");

    session.on_key(Key::Alt('x'));
    type_name(&mut session, "beacon-start");
    session.on_key(Key::Enter);

    let echo = session.echo().expect("a refusal is spoken");
    assert!(
        echo.contains("beacon-start"),
        "it names the command: {echo}"
    );
    assert_eq!(session.state(), State::Normal, "and changes nothing");
    assert!(session.narrowing().is_none());
}

#[test]
fn spc_s_b_narrows_over_the_lines_of_this_buffer() {
    // Doom has `consult-line` there and calls it "Search buffer". meowr's
    // picker has had a `line` source since M1 and nothing reached it by a
    // leader key: `SPC .` opens it on buffers and `SPC RET` on recent files,
    // and lines were reachable only by opening the picker on everything and
    // pressing `< l`. #97.
    let mut session = editing("alpha\nbeta\ngamma\n");

    session.on_key(Key::Char(' '));
    session.on_key(Key::Char('s'));
    session.on_key(Key::Char('b'));

    let widget = session.narrowing().expect("the picker opened");
    assert_eq!(session.state(), State::Normal, "and left the leader");
    assert_eq!(
        widget.matches().len(),
        3,
        "the three lines, not the buffers beside them"
    );
}

// --- previewing after a file preview (#53) ---------------------------------

/// The picker, unnarrowed, over a buffer with four lines and one recent file.
///
/// Row 0 is the buffer, row 1 the recent file, rows 2..6 the lines.
fn picker_over_four_lines() -> (Session, BufferId) {
    let mut session = editing("alpha\nbeta\ngamma\ndelta\n");
    session.set_path("first.rs");
    let origin = session.current_buffer_id();
    session.set_recent_files(vec!["/tmp/notes.rs".into()]);
    session.on_key(Key::Ctrl('x'));
    session.on_key(Key::Ctrl('f'));
    (session, origin)
}

#[test]
fn previewing_a_line_after_a_file_preview_scrolls_the_buffer_you_started_in() {
    // A `line` candidate always means a line in the buffer the picker was
    // opened over. A file preview points the window at the previewed file, and
    // nothing put it back — so scrolling landed in the file you were only
    // looking at.
    let (mut session, origin) = picker_over_four_lines();
    session.on_key(Key::Ctrl('n'));
    assert_eq!(
        session.preview_target(),
        Some(Preview::File("/tmp/notes.rs".into())),
        "row 1 is the recent file"
    );
    session.previewed("/tmp/notes.rs", Ok("a different file\n".into()));
    assert_ne!(session.current_buffer_id(), origin, "the preview took over");

    // On to `gamma`, the third line.
    for _ in 0..3 {
        session.on_key(Key::Ctrl('n'));
    }
    assert_eq!(session.preview_target(), Some(Preview::Line(3)));
    session.preview();

    assert_eq!(
        session.current_buffer_id(),
        origin,
        "the window went back before scrolling"
    );
    assert_eq!(session.point(), 11, "the start of `gamma`");
}

#[test]
fn accepting_a_line_after_a_file_preview_keeps_you_in_the_buffer_you_started_in() {
    let (mut session, origin) = picker_over_four_lines();
    session.on_key(Key::Ctrl('n'));
    session.previewed("/tmp/notes.rs", Ok("a different file\n".into()));
    for _ in 0..3 {
        session.on_key(Key::Ctrl('n'));
    }

    session.on_key(Key::Enter);

    assert!(session.narrowing().is_none(), "the picker closed");
    assert_eq!(session.current_buffer_id(), origin);
    assert_eq!(session.path(), Some("first.rs"));
    assert_eq!(session.point(), 11, "the start of `gamma`");
}

// --- killing buffers (#52) -------------------------------------------------

/// Three buffers visiting three files, focused on the first.
fn three_buffers() -> (Session, BufferId, BufferId, BufferId) {
    let mut session = editing("first\n");
    session.set_path("a.rs");
    let a = session.current_buffer_id();
    let mut second = Buffer::scratch();
    second.set_path("b.rs", &session.languages());
    second.loaded(Document::from_text("second\n"));
    let b = session.open(second);
    let mut third = Buffer::scratch();
    third.set_path("c.rs", &session.languages());
    third.loaded(Document::from_text("third\n"));
    let c = session.open(third);
    session.show(a);
    (session, a, b, c)
}

#[test]
fn killing_a_buffer_takes_it_out_of_the_cycle() {
    let (mut session, _a, b, _c) = three_buffers();
    session.show(b);

    press(&mut session, " bk");

    assert_ne!(session.path(), Some("b.rs"), "not still showing it");
    // Round the whole cycle: it must not come back.
    let mut seen = Vec::new();
    for _ in 0..3 {
        seen.push(session.path().unwrap_or("*scratch*").to_string());
        press(&mut session, " bb");
    }
    assert!(!seen.iter().any(|p| p == "b.rs"), "b.rs is gone: {seen:?}");
}

#[test]
fn killing_a_buffer_leaves_every_other_buffer_id_valid() {
    // `BufferId` is an index into the session's list and windows hold them, so
    // taking a buffer *out* of the list would silently renumber every buffer
    // after it — every window would be looking at the wrong file. This is the
    // invariant that decides how killing is implemented.
    let (mut session, a, b, c) = three_buffers();
    session.show(b);

    press(&mut session, " bk");

    assert!(session.buffer(b).is_killed(), "b's slot stayed, b did not");
    assert_eq!(session.buffer(a).path(), Some("a.rs"), "a is still a");
    assert_eq!(session.buffer(c).path(), Some("c.rs"), "c is still c");
}

#[test]
fn killing_the_last_buffer_leaves_a_scratch_rather_than_nothing() {
    // Emacs never has zero buffers. Neither can this: the renderer asks the
    // session what the focused window shows on every frame.
    let mut session = editing("only\n");
    session.set_path("only.rs");

    press(&mut session, " bk");

    assert_eq!(session.path(), None, "a buffer visiting no file");
    assert_eq!(session.current().name(), "*scratch*");
    assert!(!session.is_loading(), "and it is ready to type into");
}

#[test]
fn killing_a_modified_buffer_refuses_and_names_the_way_past() {
    // The same shape as `Session::quit`: refuse, and say which key means it.
    let (mut session, _a, b, _c) = three_buffers();
    session.show(b);
    press(&mut session, "sZ");
    session.on_key(Key::Esc);
    assert!(session.is_modified(), "the buffer really is modified");

    press(&mut session, " bk");

    assert_eq!(session.path(), Some("b.rs"), "still here");
    let echo = session.echo().expect("it says why");
    assert!(echo.contains("SPC b K"), "and how to mean it: {echo}");
}

#[test]
fn the_forced_kill_goes_anyway_and_takes_the_auto_save_with_it() {
    // `SPC b K` is to `SPC b k` what `SPC q Q` is to `SPC q q`: a decision to
    // lose the work, which has to take the auto-save too or the next visit
    // offers to recover work that was thrown away on purpose.
    let (mut session, _a, b, _c) = three_buffers();
    session.show(b);
    press(&mut session, "sZ");
    session.on_key(Key::Esc);

    press(&mut session, " bK");

    assert_ne!(session.path(), Some("b.rs"), "gone");
    assert_eq!(
        session.take_autosaves_to_forget(),
        vec![meowr_app::autosave_path("b.rs")],
        "the host is told to take the auto-save away"
    );
}

#[test]
fn a_window_showing_a_killed_buffer_shows_another() {
    // The buffer being killed can be on screen in more than one window.
    let (mut session, a, b, _c) = three_buffers();
    session.show(b);
    press(&mut session, " ws");
    assert_eq!(session.window_count(), 2, "two windows on b.rs");

    press(&mut session, " bk");

    assert_ne!(session.path(), Some("b.rs"));
    press(&mut session, " ww");
    assert_ne!(session.path(), Some("b.rs"), "the other window too");
    assert!(session.buffer(a).path().is_some(), "a survived");
}

#[test]
fn killing_an_unmodified_buffer_leaves_its_auto_save_alone() {
    // #37's auto-save outlives the process, so one sitting beside an unmodified
    // file is unrecovered work from a crash — the whole point of the feature.
    // `SPC b k` is the key that *refuses* when work is at risk; it must not be
    // the key that silently deletes the last copy of some. Only `SPC b K`, which
    // is a decision to lose work, takes the auto-save with it.
    let (mut session, _a, b, _c) = three_buffers();
    session.show(b);
    assert!(!session.is_modified(), "nothing typed into it");

    press(&mut session, " bk");

    assert!(
        session.take_autosaves_to_forget().is_empty(),
        "an auto-save beside an unmodified buffer is a crash's leavings, not this buffer's"
    );
}

#[test]
fn a_killed_buffer_cannot_be_shown_again() {
    // A tombstone is addressable — that is the point of keeping the slot — but
    // it is not a buffer. Showing one would put a writable, unnamed buffer on
    // screen that the modeline draws as `*scratch*`, which is a buffer nobody
    // opened wearing the name of one that exists.
    let (mut session, a, b, _c) = three_buffers();
    session.show(b);
    press(&mut session, " bk");
    let landed = session.current_buffer_id();

    session.show(b);

    assert_eq!(
        session.current_buffer_id(),
        landed,
        "the tombstone is refused"
    );
    assert!(session.buffer(a).path().is_some());
}

#[test]
fn a_killed_buffer_is_not_offered_in_the_picker() {
    // The same rule the preview slot follows: it is not a buffer anyone can
    // switch to, so it is not one the picker offers.
    let (mut session, _a, b, _c) = three_buffers();
    session.show(b);
    press(&mut session, " bk");

    session.on_key(Key::Ctrl('x'));
    session.on_key(Key::Ctrl('f'));
    session.on_key(Key::Char('<'));
    session.on_key(Key::Char('b'));

    let widget = session.narrowing().expect("the picker opened");
    let names: Vec<String> = (0..widget.matches().len())
        .map(|row| widget.candidate(widget.matches()[row].index).text.clone())
        .collect();
    assert_eq!(
        names.len(),
        2,
        "a.rs and c.rs, not the tombstone: {names:?}"
    );
    assert!(!names.iter().any(|n| n == "b.rs"), "{names:?}");
}

// --- the rest of Doom's buffer prefix (#68) --------------------------------
//
// Four rows of the author's own `config-buffer.el` that meowr held as
// `unscheduled` reservations. #69's first clause is that the leader tree
// matches the author's, and these are where it did not.
//
// None of them was waiting on anything. The window tree, `kill-buffer` and
// `Buffer::scratch` were each built for something else, and every one of these
// rows is a route to what already exists rather than a new capability — which
// is what `unscheduled` means and why the four sat there: nobody had a reason
// to reach for them until the config that names them was the config being run.

#[test]
fn spc_b_shift_d_kills_the_buffer_and_closes_its_window() {
    let (mut session, _a, b, _c) = three_buffers();
    session.show(b);
    press(&mut session, " ws");
    assert_eq!(session.window_count(), 2, "a window for it to close");

    press(&mut session, " bD");

    assert!(session.buffer(b).is_killed(), "the buffer went");
    assert_eq!(session.window_count(), 1, "and its window with it");
}

#[test]
fn spc_b_shift_d_refuses_a_modified_buffer_and_leaves_the_window_standing() {
    // `kill-buffer`'s refusal, unchanged. `D` closes a window as well as
    // killing a buffer, so a `D` that refused the kill and closed the window
    // anyway would take away the place the work was shown while keeping the
    // work — the worst of both answers.
    let (mut session, _a, b, _c) = three_buffers();
    session.show(b);
    press(&mut session, " ws");
    press(&mut session, "wk");
    assert!(session.is_modified(), "the buffer really is modified");

    press(&mut session, " bD");

    assert!(!session.buffer(b).is_killed(), "still here");
    assert_eq!(session.window_count(), 2, "and still on screen");
    let echo = session.echo().expect("it says why");
    assert!(echo.contains("modified"), "{echo}");
}

#[test]
fn spc_b_shift_d_in_the_only_window_kills_and_says_the_window_stayed() {
    // Emacs kills the buffer and then reports that the window could not go,
    // which is the honest order: the kill succeeded and the other half did
    // not. Saying nothing would leave `b D` indistinguishable from `b d` in
    // the one layout where they differ, which is the silence #80 went hunting.
    let (mut session, _a, b, _c) = three_buffers();
    session.show(b);
    assert_eq!(session.window_count(), 1);

    press(&mut session, " bD");

    assert!(session.buffer(b).is_killed(), "the buffer still goes");
    assert_eq!(session.window_count(), 1);
    let echo = session.echo().expect("it says why the window stayed");
    assert!(echo.contains("only window"), "{echo}");
}

#[test]
fn spc_b_o_kills_every_other_buffer_and_keeps_this_one() {
    let (mut session, a, b, c) = three_buffers();

    press(&mut session, " bo");

    assert!(
        !session.buffer(a).is_killed(),
        "the one in the window stays"
    );
    assert!(session.buffer(b).is_killed());
    assert!(session.buffer(c).is_killed());
    assert_eq!(session.path(), Some("a.rs"), "and is still on screen");
    // The count is the whole feedback: what went was off screen, so a working
    // `b o` changes nothing you can see.
    let echo = session.echo().expect("it says what it did");
    assert!(echo.contains("Killed 2 other buffers"), "{echo}");
}

#[test]
fn spc_b_o_with_nothing_else_open_says_so_rather_than_nothing() {
    // The silence neither #80 nor #88 could reach. Nothing fails here, so
    // there is no beep to hear and no `Outcome::Failed` for the registry sweep
    // to catch — only a key that did nothing and said nothing about it.
    let mut session = editing("alpha\n");
    session.set_path("a.rs");

    press(&mut session, " bo");

    let echo = session.echo().expect("it says why nothing happened");
    assert!(echo.contains("No other buffers"), "{echo}");
}

#[test]
fn spc_b_o_refuses_when_another_buffer_is_modified_and_names_the_way_past() {
    // `kill-buffer`'s discipline, applied to a command that kills buffers it
    // is *not* showing. The work at risk belongs to a buffer nobody is looking
    // at, which makes the refusal more necessary rather than less: `b k` puts
    // the buffer it is about to lose on screen first, and this one cannot.
    let (mut session, a, b, _c) = three_buffers();
    session.show(b);
    press(&mut session, "wk");
    session.show(a);

    press(&mut session, " bo");

    assert!(!session.buffer(b).is_killed(), "nothing was killed");
    let echo = session.echo().expect("it says why");
    // `Session::quit`'s sentence, because it is the same sentence: buffers
    // standing in the way of one bulk action, named rather than counted.
    assert!(
        echo.contains("Unsaved: b.rs."),
        "it names the buffer: {echo}"
    );
    assert!(echo.contains("SPC b O"), "and the way past: {echo}");
}

#[test]
fn spc_b_shift_o_kills_the_others_and_their_unsaved_work_with_them() {
    // `b O` past `b o`, which is how `SPC q Q` and `SPC b K` are past their
    // own lower-case forms. Not one of Doom's rows — Doom asks a yes-or-no
    // question here and meowr has no yes/no read, so the decision is a key.
    let (mut session, a, b, _c) = three_buffers();
    session.show(b);
    press(&mut session, "wk");
    session.show(a);

    press(&mut session, " bO");

    assert!(session.buffer(b).is_killed(), "modified, and gone anyway");
    assert!(!session.buffer(a).is_killed(), "this one still stays");
}

#[test]
fn spc_b_x_shows_the_scratch_buffer_in_this_window() {
    let (mut session, a, _b, _c) = three_buffers();

    press(&mut session, " bx");

    assert_ne!(session.current_buffer_id(), a, "not the file it was on");
    assert_eq!(session.current().name(), "*scratch*");
    assert!(session.current().path().is_none(), "it visits no file");
    assert_eq!(session.window_count(), 1, "in this window, not a new one");
}

#[test]
fn spc_b_x_twice_reaches_the_one_scratch_buffer() {
    // Emacs has one `*scratch*`, not one per press. A second buffer wearing
    // the same name is one the modeline could not tell from the first and one
    // `switch-to-buffer` would offer twice under a single name.
    let (mut session, a, _b, _c) = three_buffers();
    press(&mut session, " bx");
    let first = session.current_buffer_id();
    session.show(a);

    press(&mut session, " bx");

    assert_eq!(session.current_buffer_id(), first, "the same one");
}

#[test]
fn spc_b_x_keeps_what_was_typed_into_the_scratch_buffer() {
    // The half of "one scratch buffer" that matters in use: going away and
    // coming back is the ordinary way it is used, and a `b x` that found the
    // buffer but not its text would be a fresh one under an old name.
    let (mut session, a, _b, _c) = three_buffers();
    press(&mut session, " bx");
    press(&mut session, "shello"); // `s` is `meow-insert` on this layout
    session.on_key(Key::Esc);
    session.show(a);

    press(&mut session, " bx");

    assert!(
        session.contents().unwrap().contains("hello"),
        "{:?}",
        session.contents()
    );
}

#[test]
fn spc_b_shift_x_opens_the_scratch_buffer_in_a_second_window() {
    // Doom pops it up in a window of its own, and `b x` is the same command
    // told to use this one instead. Nothing floats here — ADR-0004 — so the
    // popup is a split, which is what every other meowr window already is.
    let (mut session, a, _b, _c) = three_buffers();

    press(&mut session, " bX");

    assert_eq!(session.window_count(), 2, "a window of its own");
    assert_eq!(session.current().name(), "*scratch*");
    assert_eq!(
        session.buffer(a).path(),
        Some("a.rs"),
        "the file is still open"
    );
}

#[test]
fn spc_b_shift_x_and_spc_b_x_land_on_the_same_scratch_buffer() {
    let (mut session, _a, _b, _c) = three_buffers();
    press(&mut session, " bx");
    let from_switching = session.current_buffer_id();

    press(&mut session, " bX");

    assert_eq!(
        session.current_buffer_id(),
        from_switching,
        "one scratch buffer, two ways in"
    );
}

#[test]
fn spc_b_o_repoints_a_window_on_a_killed_buffer_rather_than_closing_it() {
    // The one place this diverges from Doom, pinned so it is a decision on the
    // record rather than something noticed later. `doom/kill-other-buffers`
    // maps `doom-kill-buffer-and-windows` and the windows go with the buffers;
    // every kill here goes through `entomb`, which repoints instead (#52).
    let (mut session, a, b, _c) = three_buffers();
    press(&mut session, " ws"); // two windows, both on a.rs
    session.show(b); // the new one shows b.rs
    press(&mut session, " ww"); // back to the one on a.rs
    assert_eq!(session.path(), Some("a.rs"));

    press(&mut session, " bo");

    assert!(session.buffer(b).is_killed());
    assert_eq!(session.window_count(), 2, "the window stayed");
    let showing: Vec<BufferId> = session
        .windows()
        .order()
        .into_iter()
        .map(|w| session.windows().window(w).buffer)
        .collect();
    assert!(
        showing.iter().all(|&id| id == a),
        "both windows were pointed at what is left: {showing:?}"
    );
}

#[test]
fn spc_b_shift_x_twice_does_not_stack_up_windows() {
    // `pop-to-buffer` reuses a window already displaying the buffer. Two
    // presses that made three windows would be two views of one scratch buffer
    // and a layout nobody asked for — and the second press arrives *from* the
    // scratch window, which is the case a `w != focus` search would miss.
    let (mut session, _a, _b, _c) = three_buffers();
    press(&mut session, " bX");
    assert_eq!(session.window_count(), 2);
    assert_eq!(session.current().name(), "*scratch*");

    press(&mut session, " bX");

    assert_eq!(session.window_count(), 2, "the second press reused it");
    assert_eq!(session.current().name(), "*scratch*");
}

#[test]
fn spc_b_shift_x_from_the_file_returns_to_the_scratch_window_already_open() {
    // The other way in to the same reuse: the scratch window exists and the
    // focus is somewhere else, which is `pop-to-buffer`'s ordinary case.
    let (mut session, _a, _b, _c) = three_buffers();
    press(&mut session, " bX");
    press(&mut session, " ww"); // away to the other window
    assert_ne!(session.current().name(), "*scratch*");

    press(&mut session, " bX");

    assert_eq!(session.window_count(), 2, "no third window");
    assert_eq!(session.current().name(), "*scratch*", "focus went to it");
}

// --- the line-number gutter (#68) -------------------------------------------

#[test]
fn the_gutter_is_on_by_default_and_sized_to_the_buffer() {
    let session = editing("alpha\nbeta\ngamma\n");
    assert_eq!(
        session.gutter_of(session.focus()),
        Some(2),
        "one digit, plus one"
    );
}

#[test]
fn turning_off_line_numbers_removes_the_gutter() {
    let mut session = editing("alpha\n");
    let mut options = session.options();
    options.line_numbers = false;
    session.set_options(options);

    assert_eq!(session.gutter_of(session.focus()), None);
}

#[test]
fn a_buffer_still_loading_has_no_gutter_yet() {
    let mut session = Session::new(Viewport { top: 0, height: 10 });
    session.on_resize(80, 10);
    assert_eq!(session.gutter_of(session.focus()), None);
}

#[test]
fn the_gutter_widens_as_the_buffer_grows_past_a_power_of_ten() {
    let mut session = editing("one\n");
    assert_eq!(session.gutter_of(session.focus()), Some(2));

    for _ in 0..10 {
        press(&mut session, "A");
        session.on_key(Key::Enter);
        session.on_key(Key::Esc);
    }

    assert_eq!(
        session.gutter_of(session.focus()),
        Some(3),
        "eleven lines needs two digits"
    );
}

// --- search, visit, find and till -----------------------------------------

#[test]
fn find_selects_to_the_character_and_till_stops_before_it() {
    let mut session = editing("alpha beta gamma\n");
    press(&mut session, "f");
    session.on_key(Key::Char('b'));
    assert_eq!(session.selection().range(), 0..7, "through the b");
    assert_eq!(session.selection().kind().unwrap().name(), "find");

    let mut session = editing("alpha beta gamma\n");
    press(&mut session, "t");
    session.on_key(Key::Char('b'));
    assert_eq!(session.selection().range(), 0..6, "up to the b");
    assert_eq!(session.selection().kind().unwrap().name(), "till");
}

#[test]
fn a_character_that_is_not_there_says_so_and_leaves_the_selection() {
    // This asserted a signal until the fidelity harness could oracle a find
    // (#85). Meow does not raise here — `(message "char %s not found" ch-str)`
    // — and Emacs signals zero for every failed find and till in the suite.
    // The assertion had been written from the reasonable guess that a search
    // that finds nothing is an error, which is what the harness exists to stop
    // anybody having to guess.
    let mut session = editing("alpha\n");
    let before = session.signals();

    press(&mut session, "f");
    session.on_key(Key::Char('z'));

    assert_eq!(
        session.signals(),
        before,
        "Meow messages here, it does not ring"
    );
    assert_eq!(session.echo(), Some("char z not found"));
    assert!(session.selection().selection_type().is_none());
}

#[test]
fn a_digit_repeats_a_find_using_the_remembered_character() {
    // `meow--find-continue-forward`: the character is kept, so expanding a
    // find means finding the next one rather than doing nothing.
    let mut session = editing("a.b.c.d.e\n");
    press(&mut session, "f");
    session.on_key(Key::Char('.'));
    let first = session.selection().pos();

    press(&mut session, "2");

    assert!(session.selection().pos() > first, "it found two more");
    assert_eq!(session.selection().kind().unwrap().name(), "find");
}

#[test]
fn a_negative_argument_finds_backwards() {
    let mut session = editing("a.b.c\n");
    session.set_selection(Selection::at(5));
    press(&mut session, "-");
    press(&mut session, "f");
    session.on_key(Key::Char('.'));
    assert!(session.selection().pos() < 5, "{:?}", session.selection());
}

#[test]
fn search_takes_the_selection_as_its_target_and_repeats() {
    let mut session = editing("beta alpha beta gamma beta\n");
    press(&mut session, "w"); // select "beta"
    press(&mut session, "v"); // search: pushes it, finds the next

    assert_eq!(
        session.search_ring().first().map(String::as_str),
        Some("beta")
    );
    assert_eq!(session.selection().range(), 11..15, "the second one");
    assert_eq!(session.selection().kind().unwrap().name(), "visit");

    press(&mut session, "v");
    assert_eq!(session.selection().range(), 22..26, "the third");
}

#[test]
fn search_wraps_rather_than_stopping_at_the_end() {
    // A search that stops at the bottom makes you press a different key to
    // carry on, which is the wrong shape for the fingers.
    let mut session = editing("one two one\n");
    press(&mut session, "w");
    press(&mut session, "v");
    let second = session.selection().range();
    press(&mut session, "v");
    assert_ne!(session.selection().range(), second, "it moved");
    assert_eq!(session.selection().range(), 0..3, "back to the first");
}

#[test]
fn the_search_ring_is_separate_from_the_selection_history() {
    let mut session = editing("beta alpha beta\n");
    press(&mut session, "w");
    press(&mut session, "v");
    let ring = session.search_ring().to_vec();

    // Walking the selection history back does not touch the ring.
    press(&mut session, "z");
    press(&mut session, "z");

    assert_eq!(session.search_ring(), ring.as_slice());
}

#[test]
fn visit_offers_buffer_symbols_and_selects_the_chosen_one() {
    let mut session = editing("alpha beta gamma\nalpha again\n");
    press(&mut session, "/");
    assert!(session.narrowing().is_some(), "the widget opened");

    for c in "gam".chars() {
        session.on_key(Key::Char(c));
    }
    session.on_key(Key::Enter);

    assert!(session.narrowing().is_none());
    assert_eq!(session.selection().kind().unwrap().name(), "visit");
    assert_eq!(session.selection().range(), 11..16, "gamma");
    assert_eq!(
        session.search_ring().first().map(String::as_str),
        Some("gamma"),
        "and it becomes the search target"
    );
}

// --- the universal argument -----------------------------------------------

#[test]
fn a_universal_argument_sends_the_line_motions_to_the_ends_of_the_buffer() {
    // "Use with universal argument to move to the last line of buffer."
    // `Argument` had no universal variant, so both branches were written and
    // neither had a key to arrive on.
    let text = "one\ntwo\nthree\nfour\n";
    let mut session = editing(text);

    session.on_key(Key::Ctrl('u'));
    press(&mut session, "n");
    assert_eq!(session.point(), text.len(), "point-max");

    session.on_key(Key::Ctrl('u'));
    press(&mut session, "e");
    assert_eq!(session.point(), 0, "point-min");
}

#[test]
fn a_universal_argument_reaches_meow_joins_both_sides_mode() {
    // `(or (equal '(expand . join) …) (meow--with-universal-argument-p arg))`.
    // Repeating `j` was the whole path to `meow--join-both`; now `C-u j` is
    // the other one, and it does not need a join selection to start from.
    //             0123 456789
    let text = "one\n   two   \nthree\n";
    let point = 11; // inside the run of spaces after `two`

    let mut plain = editing(text);
    plain.set_selection(Selection::at(point));
    press(&mut plain, "j");
    assert_eq!(
        plain.selection().range(),
        3..7,
        "backward reaches from the line's edges to its indentation"
    );

    let mut both = editing(text);
    both.set_selection(Selection::at(point));

    both.on_key(Key::Ctrl('u'));
    press(&mut both, "j");

    assert_eq!(
        both.selection().range(),
        10..14,
        "both sides reach from the point itself, over the blanks either way"
    );
    assert_eq!(both.selection().kind(), Some(Kind::JOIN));
}

#[test]
fn a_universal_argument_is_exactly_one_c_u() {
    // `meow--with-universal-argument-p` is `(equal '(4) arg)`. `C-u C-u` is
    // `(16)`, which is not it — so the line motion takes the count branch and
    // moves sixteen lines rather than jumping.
    let text = "a\nb\nc\nd\ne\n";
    let mut session = editing(text);

    session.on_key(Key::Ctrl('u'));
    session.on_key(Key::Ctrl('u'));
    press(&mut session, "n");

    // Sixteen lines down runs out of buffer, which signals after moving as
    // far as it can — the same as any over-long count.
    assert_eq!(session.point(), text.len());
    assert_eq!(session.signals(), 1, "it signalled rather than jumping");
}

#[test]
fn a_digit_after_c_u_replaces_it_rather_than_extending_it() {
    // `C-u 3` is three, not thirty-four.
    let text = "a\nb\nc\nd\ne\nf\n";
    let mut session = editing(text);

    session.on_key(Key::Ctrl('u'));
    press(&mut session, "3n");

    assert_eq!(session.point(), text.find('d').unwrap());
}

#[test]
fn ordinary_counts_and_negative_arguments_are_unchanged() {
    let text = "a\nb\nc\nd\ne\n";
    let mut session = editing(text);
    press(&mut session, "2n");
    assert_eq!(session.point(), text.find('c').unwrap());

    press(&mut session, "-n");
    assert_eq!(session.point(), text.find('b').unwrap(), "up one line");
}

// --- auto-save ------------------------------------------------------------

#[test]
fn the_auto_save_file_sits_beside_the_original() {
    // `make-auto-save-file-name`. Beside it rather than in a state directory,
    // because someone who has lost work will look next to the file.
    assert_eq!(
        meowr_app::autosave_path("/home/user/notes.txt"),
        "/home/user/#notes.txt#"
    );
    assert_eq!(meowr_app::autosave_path("notes.txt"), "#notes.txt#");
}

#[test]
fn an_unmodified_buffer_has_nothing_to_auto_save() {
    let mut session = editing("alpha\n");
    session.set_path("notes.txt");

    assert!(session.take_autosaves(true).is_empty());
    assert!(!session.autosave_pending());
}

#[test]
fn a_modified_buffer_is_auto_saved_on_the_way_down() {
    // The whole point: this is the path a kill signal and a closed terminal
    // take, where there is nothing to refuse *to* and the guard in
    // `Session::quit` never runs.
    let mut session = editing("alpha\n");
    session.set_path("notes.txt");
    press(&mut session, "sX");
    session.on_key(Key::Esc);
    assert!(session.is_modified());
    assert!(session.autosave_pending());

    let due = session.take_autosaves(true);

    assert_eq!(due.len(), 1);
    assert_eq!(due[0].path, "#notes.txt#");
    assert_eq!(due[0].contents, "Xalpha\n");
}

#[test]
fn a_few_edits_do_not_reach_the_interval_but_enough_of_them_do() {
    // `auto-save-interval` counts edits, not seconds: a buffer nobody is
    // typing into does not need writing out, and one being typed into hard
    // does. The idle timer is the other half, and lives in the host.
    let mut session = editing("alpha\n");
    session.set_path("notes.txt");
    press(&mut session, "s");
    press(&mut session, "x");
    assert!(
        session.take_autosaves(false).is_empty(),
        "one edit is not an interval"
    );

    for _ in 0..Options::default().auto_save_interval {
        press(&mut session, "y");
    }

    assert_eq!(session.take_autosaves(false).len(), 1);
    assert!(
        session.take_autosaves(false).is_empty(),
        "taking it resets the count"
    );
}

#[test]
fn auto_save_can_be_declined_and_then_nothing_is_written_on_any_path() {
    // `auto-save-default`, which the author's Doom config sets. Every path
    // that would write goes through one of these three, so turning it off has
    // to close all three — including the snapshot, which is what a panic
    // writes and is the path nobody presses a key to reach.
    let mut session = editing("alpha\n");
    session.set_path("notes.txt");
    session.set_options(Options {
        auto_save: false,
        ..Options::default()
    });
    press(&mut session, "sX");
    session.on_key(Key::Esc);
    assert!(session.is_modified(), "the edit still happened");

    assert!(
        !session.autosave_pending(),
        "so the host arms no idle timer"
    );
    assert!(
        session.take_autosaves(true).is_empty(),
        "and not even a forced take, which is the shutdown path"
    );
    assert!(
        session.autosave_snapshot().is_empty(),
        "nor the snapshot a panic would write"
    );
}

#[test]
fn turning_auto_save_back_on_resumes_without_a_restart() {
    // The Option is read where the work is taken rather than latched at
    // startup, so a config reloaded mid-session takes effect on the next
    // write rather than the next process.
    let mut session = editing("alpha\n");
    session.set_path("notes.txt");
    session.set_options(Options {
        auto_save: false,
        ..Options::default()
    });
    press(&mut session, "sX");
    session.on_key(Key::Esc);
    assert!(session.take_autosaves(true).is_empty());

    session.set_options(Options::default());

    assert!(session.autosave_pending());
    assert_eq!(session.take_autosaves(true).len(), 1);
}

#[test]
fn the_edit_count_before_an_auto_save_is_an_option() {
    // `auto-save-interval`. Counting commands rather than characters makes
    // the same number a shorter leash than Emacs's, which is the direction to
    // err in — and someone who disagrees can now say so.
    let mut session = editing("alpha\n");
    session.set_path("notes.txt");
    session.set_options(Options {
        auto_save_interval: 3,
        ..Options::default()
    });
    press(&mut session, "s");

    press(&mut session, "x");
    press(&mut session, "y");
    assert!(
        session.take_autosaves(false).is_empty(),
        "two edits is short of three"
    );

    press(&mut session, "z");

    assert_eq!(session.take_autosaves(false).len(), 1);
}

#[test]
fn auto_save_defaults_match_what_shipped_before_it_was_configurable() {
    // A session with no config behaves as #37 shipped it. Defaulting
    // auto-save off would silently undo that for everyone who never writes
    // an `init.scm`.
    let options = Options::default();

    assert!(options.auto_save, "on unless declined");
    assert_eq!(options.auto_save_interval, 64);
    assert_eq!(options.auto_save_timeout, Duration::from_secs(30));
}

#[test]
fn saving_stops_the_buffer_being_due_an_auto_save() {
    let mut session = editing("alpha\n");
    session.set_path("notes.txt");
    press(&mut session, "sX");
    session.on_key(Key::Esc);

    session.mark_saved();

    assert!(!session.autosave_pending());
    assert!(session.take_autosaves(true).is_empty());
}

#[test]
fn a_buffer_with_no_file_has_nowhere_to_auto_save_to() {
    // Emacs puts these under `~/`, which is a decision about the user's home
    // directory that this layer has no business making.
    let mut session = editing("alpha\n");
    press(&mut session, "sX");
    session.on_key(Key::Esc);

    assert!(session.is_modified());
    assert!(session.take_autosaves(true).is_empty());
}

#[test]
fn the_snapshot_a_panic_would_write_is_the_work_as_it_stands() {
    // Taken after every key rather than on the auto-save schedule, because
    // the edits since the last auto-save are exactly the ones a crash would
    // otherwise take. A `Rope` clone shares its nodes, which is what makes
    // that affordable.
    let mut session = editing("alpha\n");
    session.set_path("notes.txt");
    press(&mut session, "sXY");
    session.on_key(Key::Esc);

    let snapshot = session.autosave_snapshot();

    assert_eq!(snapshot.len(), 1);
    assert_eq!(snapshot[0].path, "#notes.txt#");
    assert_eq!(snapshot[0].text.to_string(), "XYalpha\n");
}

#[test]
fn recovery_is_offered_by_name_rather_than_applied() {
    // The auto-save may be older work than the file, or nonsense from a crash
    // mid-edit. Substituting it without asking would make recovery something
    // that happens *to* you.
    let mut session = editing("alpha\n");
    session.set_path("notes.txt");

    session.offer_recovery("#notes.txt#".into());

    let echo = session.echo().expect("it says so");
    assert!(echo.contains("#notes.txt#"), "{echo}");
    assert!(echo.contains("SPC f a"), "and how to take it up: {echo}");
    assert_eq!(session.contents().unwrap(), "alpha\n", "nothing applied");
    assert!(
        session.take_recovery().is_none(),
        "not until it is asked for"
    );
}

#[test]
fn recovering_asks_the_host_to_read_and_leaves_the_buffer_modified() {
    let mut session = editing("alpha\n");
    session.set_path("notes.txt");
    session.offer_recovery("#notes.txt#".into());

    press(&mut session, " fa"); // SPC f a

    assert_eq!(session.take_recovery().as_deref(), Some("#notes.txt#"));
    assert_eq!(
        session.recovered(Ok("the work that was lost\n".into())),
        Action::Redraw
    );
    assert_eq!(session.contents().unwrap(), "the work that was lost\n");
    assert!(
        session.is_modified(),
        "the file on disk still says what it said"
    );

    press(&mut session, "u");
    assert_eq!(
        session.contents().unwrap(),
        "alpha\n",
        "recovery goes through the history like any other change"
    );
}

#[test]
fn recovering_with_nothing_to_recover_signals() {
    let mut session = editing("alpha\n");
    session.set_path("notes.txt");
    let signals = session.signals();

    press(&mut session, " fa");

    assert_eq!(session.signals(), signals + 1);
    assert!(session.take_recovery().is_none());
}

// --- the Grab -------------------------------------------------------------

#[test]
fn a_grab_persists_while_the_primary_selection_moves() {
    // A genuinely independent second selection, not a saved copy of the first.
    let mut session = editing("alpha beta gamma delta\n");
    press(&mut session, "w"); // select "alpha"
    press(&mut session, "G");

    assert_eq!(session.grab(), Some(0..5));
    assert!(
        session.selection().selection_type().is_none(),
        "grabbing cancels the primary selection"
    );

    press(&mut session, "wwww");
    assert_eq!(session.grab(), Some(0..5), "the Grab did not follow");
    assert_ne!(session.selection().range(), 0..5);
}

#[test]
fn copying_after_a_grab_uses_the_mark_the_cancellation_left_behind() {
    // `G` cancels the primary selection, so `y` — `meow-save` — has nothing
    // to save and falls back to `kill-ring-save`. Emacs's `kill-ring-save`
    // needs only a *mark*, and the mark survives a cancellation, so this
    // copies rather than signalling. It used to signal here, because the mark
    // lived inside the selection and went down with it.
    let mut session = editing("alpha beta\n");
    press(&mut session, "wG");
    let signals = session.signals();

    press(&mut session, "y");
    assert_eq!(session.signals(), signals, "`y` did not signal");

    press(&mut session, "p");
    assert_eq!(
        session.document().unwrap().text().to_string(),
        "alphaalpha beta\n",
        "the copy really did reach the ring"
    );
}

#[test]
fn copying_with_no_mark_at_all_still_signals() {
    // The other half: `(mark)` is nil in a buffer nothing has marked in, and
    // `kill-ring-save` says "The mark is not set now" rather than copying an
    // empty string into the ring.
    let mut session = editing("alpha beta\n");
    let signals = session.signals();

    press(&mut session, "y");

    assert_eq!(session.signals(), signals + 1);
}

#[test]
fn grabbing_with_no_selection_puts_the_grab_away() {
    // `meow--cancel-second-selection`: `G` with nothing selected is how you
    // clear it, rather than grabbing nothing.
    let mut session = editing("alpha beta\n");
    press(&mut session, "wG");
    assert!(session.grab().is_some());

    press(&mut session, "G");
    assert!(session.grab().is_none());
}

#[test]
fn the_grab_survives_an_edit_elsewhere_in_the_buffer() {
    // An overlay in Emacs survives an edit before it; a Grab that did not
    // would be a saved copy rather than a second selection.
    let mut session = editing("alpha beta gamma\n");
    session.set_selection(Selection::at(11));
    press(&mut session, "w"); // select "gamma"
    press(&mut session, "G");
    assert_eq!(session.grab(), Some(11..16));

    // Kill "alpha" at the front; everything after it moves by five.
    session.set_selection(Selection::at(0));
    press(&mut session, "wk");

    assert_eq!(session.grab(), Some(6..11), "it moved with the text");
    assert_eq!(session.contents().unwrap(), " beta gamma\n");
}

#[test]
fn pop_grab_makes_the_grab_the_selection_and_discards_it() {
    let mut session = editing("alpha beta\n");
    press(&mut session, "wG");
    assert_eq!(session.grab(), Some(0..5));

    // `z` with no selection falls back to pop-grab, per the fallback table.
    press(&mut session, "z");

    assert_eq!(session.selection().range(), 0..5);
    assert!(session.grab().is_none());
}

// These three press `SPC g s` and `SPC g y` rather than a chord, which is
// also the test that the Grab commands are reachable from the leader at all.
// They used to sit on invented `C-x` chords under a comment saying Ring 2
// would give them leader keys; it shipped without doing so, and a binding that
// outlives the note explaining it is a binding nobody chose.
#[test]
fn swap_exchanges_the_two_texts() {
    let mut session = editing("alpha beta gamma\n");
    press(&mut session, "wG"); // grab "alpha"
    session.set_selection(Selection::at(11));
    press(&mut session, "w"); // select "gamma"

    press(&mut session, " gs"); // SPC g s

    assert_eq!(session.contents().unwrap(), "gamma beta alpha\n");
    assert_eq!(
        session.grab(),
        Some(0..5),
        "the Grab holds the other text now"
    );
}

#[test]
fn sync_makes_the_grab_agree_with_the_selection() {
    let mut session = editing("alpha beta gamma\n");
    press(&mut session, "wG"); // grab "alpha"
    session.set_selection(Selection::at(11));
    press(&mut session, "w"); // select "gamma"

    press(&mut session, " gy"); // SPC g y

    assert_eq!(session.contents().unwrap(), "gamma beta gamma\n");
    assert_eq!(session.grab(), Some(0..5));
}

#[test]
fn swapping_overlapping_ranges_is_refused_rather_than_corrupting_the_buffer() {
    let mut session = editing("alpha beta\n");
    press(&mut session, "wG");
    press(&mut session, "l"); // a line selection, which contains the Grab
    let before = session.contents().unwrap();
    let signals = session.signals();

    press(&mut session, " gs"); // SPC g s

    assert_eq!(session.contents().unwrap(), before);
    assert_eq!(session.signals(), signals + 1);
}

// --- Beacon State ---------------------------------------------------------

#[test]
fn beacon_state_is_entered_by_being_inside_the_grab_not_by_a_key() {
    // There is no Beacon key, and looking for one is the first thing that
    // confuses people: `meow--maybe-toggle-beacon-state` runs after every
    // command and switches on the point being inside the secondary selection.
    let mut session = editing("one\ntwo\nthree\nfour\n");
    press(&mut session, "lll"); // a three-line selection
    press(&mut session, "G"); // grab it
    assert_eq!(session.state(), State::Normal);

    press(&mut session, "h"); // move, landing inside the Grab

    assert_eq!(session.state(), State::Beacon);
    assert_eq!(session.beacons().len(), 3, "one per line");
}

#[test]
fn leaving_the_grab_leaves_beacon_state() {
    let mut session = editing("one\ntwo\nthree\nfour\nfive\n");
    press(&mut session, "ll");
    press(&mut session, "G");
    press(&mut session, "h");
    assert_eq!(session.state(), State::Beacon);

    // Move well past the end of the Grab.
    for _ in 0..12 {
        session.on_key(Key::Char('i'));
    }

    assert_eq!(session.state(), State::Normal);
    assert!(session.beacons().is_empty());
}

#[test]
fn an_edit_is_recorded_once_and_replayed_at_every_beacon() {
    let mut session = editing("one\ntwo\nsix\n");
    press(&mut session, "ll"); // two lines
    press(&mut session, "G");
    press(&mut session, "h");
    assert_eq!(session.state(), State::Beacon);
    assert_eq!(session.beacons().len(), 2);

    // One press, since #93 gave `s` back to `meow-insert` as the Beacon
    // keymap has it. It records *and* enters Insert, which is what
    // `meow-beacon-insert` does.
    press(&mut session, "s");
    assert!(session.is_recording());
    session.on_key(Key::Char('X'));
    session.on_key(Key::Esc); // ends the macro and applies it

    assert_eq!(session.contents().unwrap(), "Xone\nXtwo\nsix\n");
    assert!(!session.is_recording());
    assert_eq!(session.state(), State::Normal, "and it leaves Beacon State");
}

#[test]
fn s_in_beacon_state_inserts_at_every_beacon_in_one_press() {
    // `meow-keymap.el` remaps `meow-insert` to `meow-beacon-insert` in the
    // Beacon keymap, and `s` is where the Colemak table puts `meow-insert`.
    // meowr had `beacon-start` there instead, on a comment claiming Meow does
    // -- the extracted table says `s` is `meow-insert` and `meow-beacon-start`
    // has no letter key at all.
    let mut session = editing("one\ntwo\nsix\n");
    press(&mut session, "ll");
    press(&mut session, "G");
    press(&mut session, "h");
    assert_eq!(session.state(), State::Beacon);

    press(&mut session, "s");
    assert_eq!(session.state(), State::Insert, "one press, not two");
    session.on_key(Key::Char('X'));
    session.on_key(Key::Esc);

    assert_eq!(session.contents().unwrap(), "Xone\nXtwo\nsix\n");
    assert_eq!(session.state(), State::Normal);
}

#[test]
fn a_in_beacon_state_appends_at_the_end_of_every_beacon() {
    // The test that proves a beacon is a *region* and not a position: an
    // append at a position would land in column one on every line.
    let mut session = editing("one\ntwo\nsix\n");
    press(&mut session, "ll");
    press(&mut session, "G");
    press(&mut session, "h");

    press(&mut session, "a");
    session.on_key(Key::Char('!'));
    session.on_key(Key::Esc);

    assert_eq!(session.contents().unwrap(), "one!\ntwo!\nsix\n");
}

#[test]
fn c_in_beacon_state_changes_the_whole_of_every_beacon() {
    // `meow-change` takes the selection, and at a beacon the selection is the
    // beacon. Without one it would fall back to `meow-change-char` and eat a
    // single letter on each line.
    let mut session = editing("one\ntwo\nsix\n");
    press(&mut session, "ll");
    press(&mut session, "G");
    press(&mut session, "h");

    press(&mut session, "c");
    session.on_key(Key::Char('Z'));
    session.on_key(Key::Esc);

    assert_eq!(session.contents().unwrap(), "Z\nZ\nsix\n");
}

#[test]
fn beacon_start_keeps_its_name_now_that_it_has_lost_its_key() {
    // Meow reaches it only through `<remap> <kmacro-start-macro>`, so it is a
    // name rather than a key there too. `M-x` (#91) is what makes that
    // liveable, and a config may still bind it.
    let mut session = editing("one\ntwo\nsix\n");
    press(&mut session, "ll");
    press(&mut session, "G");
    press(&mut session, "h");
    assert_eq!(session.state(), State::Beacon);

    let mut set = Declarations::default();
    set.push(Declaration::Bind {
        state: State::Beacon,
        keys: vec![Key::Alt('r')],
        command: "beacon-start".into(),
    });
    session.apply(set).expect("`beacon-start` must still exist");
    session.on_key(Key::Alt('r'));

    assert!(session.is_recording(), "it still records");
}

#[test]
fn k_in_beacon_state_kills_every_beacon_at_once() {
    // The other shape of remap. `insert` and its two friends *record* and
    // replay when Insert is left; `kill` and `replace` act at every beacon
    // there and then, because nothing is typed and there is nothing to record.
    let mut session = editing("one\ntwo\nsix\n");
    press(&mut session, "ll");
    press(&mut session, "G");
    press(&mut session, "h");
    assert_eq!(session.state(), State::Beacon);

    press(&mut session, "k");

    assert_eq!(session.contents().unwrap(), "\n\nsix\n");
    assert!(
        !session.is_recording(),
        "nothing was typed, so nothing recorded"
    );
}

#[test]
fn a_beacon_kill_hands_the_reparser_edits_it_can_actually_use() {
    // The 2026-09-11 segfault: `k` in Beacon State kills every beacon in one
    // command, and `delete_beacons` records them highest-position-first so
    // each one's own offset stays valid without adjustment (its own comment
    // says so). That is the opposite of what `tree_house::Syntax::update`
    // wants — its own comment says it reverses whatever it is given so the
    // *highest* position is applied to the tree first, which only holds if
    // the input is ascending to begin with. Fed the batch in Beacon's own
    // recording order, tree-sitter's incremental parse corrupted its own
    // state on real Markdown content rather than merely mis-colouring it.
    let mut session = editing("one\ntwo\nthree\nfour\n");
    session.set_path("notes.md");
    // Drain the initial full-parse job so `take_reparse` isn't blocked by
    // `parsing` still being true for the kill's own job below.
    let (id, job) = session.take_reparse().expect("a language is set");
    assert!(job.edits.is_empty(), "nothing to reparse incrementally yet");
    session.apply_reparse(id, job.revision, None);

    press(&mut session, "lll"); // one word per line, three lines
    press(&mut session, "G");
    press(&mut session, "h");
    assert_eq!(session.state(), State::Beacon);
    assert_eq!(session.beacons().len(), 3);

    press(&mut session, "k");

    assert_eq!(session.contents().unwrap(), "\n\n\nfour\n");
    let (_, job) = session
        .take_reparse()
        .expect("the kill recorded changes to reparse");
    assert_eq!(job.edits.len(), 3, "one edit per beacon");
    let starts: Vec<u32> = job.edits.iter().map(|e| e.start_byte).collect();
    let mut sorted = starts.clone();
    sorted.sort_unstable();
    assert_eq!(
        starts, sorted,
        "tree_house::Syntax::update reverses whatever order it is given, \
         trusting the input to already be ascending by position"
    );
    assert_eq!(starts, vec![0, 4, 8], "the three original word positions");
}

#[test]
fn a_multi_line_recovery_hands_the_reparser_correct_end_points() {
    // The other half of the same finding. `recovered` deletes the whole
    // buffer then inserts the recovered text — two changes, batched into one
    // reparse now that `after_command` runs before this method returns
    // rather than whenever the next unrelated command happens to drain it.
    //
    // `meowr_syntax::edit`'s own `new_end_point` used to be read off a
    // caller-supplied snapshot at `at + inserted.len()` — correct for the
    // one caller `edit` first had, where that snapshot already contained
    // this exact edit, wrong for a batch built from one snapshot shared by
    // several edits, none of which owns it. Walked from `inserted` itself
    // instead, the way `old_end_point` already walks `removed`, it needs no
    // snapshot to be right.
    let mut session = editing("alpha\n");
    session.set_path("notes.md");
    let (id, job) = session.take_reparse().expect("a language is set");
    session.apply_reparse(id, job.revision, None);

    session.recovered(Ok("line one\nline two\nline three\n".into()));

    let (_, job) = session
        .take_reparse()
        .expect("recovered() now drains its own changes");
    assert_eq!(job.edits.len(), 2, "a delete, then an insert");

    // Both edits are anchored to the *original* text independently — the
    // shape `tree_house::Syntax::update` wants, applied highest-original-
    // position first — not chained as if replayed live one after another.
    // The insert's own position is the end of the untouched original
    // content; it does not need to equal where the delete's own end lands.
    let delete = &job.edits[0];
    assert_eq!(delete.start_point.row, 0);
    assert_eq!(delete.old_end_point.row, 1, "\"alpha\\n\" is one line");
    assert_eq!(delete.new_end_point.row, 0, "nothing is inserted here");

    let insert = &job.edits[1];
    assert_eq!(
        insert.start_point.row, 1,
        "anchored past the untouched original content, not at 0"
    );
    assert_eq!(
        insert.new_end_point.row,
        1 + 3,
        "three newlines in the recovered text, not zero from a snapshot \
         that never contained it"
    );
    assert_eq!(insert.new_end_point.col, 0);
}

#[test]
fn one_undo_takes_back_every_beacon_of_a_replace() {
    // `meow--wrap-collapse-undo`: Meow makes the whole loop one undo step. An
    // `r` down forty lines that took forty presses of `u` to take back would
    // be a worse feature than no `r` at all.
    //
    // `r` rather than `k` on purpose. `k` empties the beacons through
    // `History::delete` directly, so its edits amalgamate whether or not
    // anything asks them to, and a test written on it would pass with the
    // collapsing turned off -- which it did, until this one was written.
    let mut session = editing("one\ntwo\nsix\n");
    press(&mut session, "w");
    press(&mut session, "y");
    press(&mut session, "ll");
    press(&mut session, "G");
    press(&mut session, "h");
    press(&mut session, "r");
    assert_eq!(session.contents().unwrap(), "one\none\nsix\n");

    press(&mut session, "u");

    assert_eq!(session.contents().unwrap(), "one\ntwo\nsix\n");
}

#[test]
fn one_undo_takes_back_every_beacon_of_a_kill() {
    let mut session = editing("one\ntwo\nsix\n");
    press(&mut session, "ll");
    press(&mut session, "G");
    press(&mut session, "h");
    press(&mut session, "k");
    assert_eq!(session.contents().unwrap(), "\n\nsix\n");

    press(&mut session, "u");

    assert_eq!(session.contents().unwrap(), "one\ntwo\nsix\n");
}

#[test]
fn r_in_beacon_state_replaces_every_beacon_with_the_kill_ring_head() {
    let mut session = editing("one\ntwo\nsix\n");
    press(&mut session, "w"); // select `one`
    press(&mut session, "y"); // save it -- the kill ring's head
    press(&mut session, "ll");
    press(&mut session, "G");
    press(&mut session, "h");
    assert_eq!(session.state(), State::Beacon);

    press(&mut session, "r");

    assert_eq!(session.contents().unwrap(), "one\none\nsix\n");
}

#[test]
fn replay_may_diverge_per_position_and_does() {
    // This is the feature, not a limitation. Meow replays a keyboard macro
    // sequentially rather than editing at N cursors simultaneously, so a
    // *search* inside the macro finds something different on each line — which
    // is the whole reason to use it down forty lines of unlike text.
    let mut session = editing("a=1;\nbb=22;\nccc=333;\nrest\n");
    press(&mut session, "lll");
    press(&mut session, "G");
    press(&mut session, "h");
    assert_eq!(session.beacons().len(), 3);

    // `beacon-start`'s workflow, not `s`'s: this records keys that are not an
    // insertion, so it must not enter Insert. Since #93 it has no default key,
    // exactly as Meow has none for it -- there it is reached through
    // `<remap> <kmacro-start-macro>`, and here by name or by a config.
    let mut set = Declarations::default();
    set.push(Declaration::Bind {
        state: State::Beacon,
        keys: vec![Key::Alt('r')],
        command: "beacon-start".into(),
    });
    session.apply(set).expect("`beacon-start` is a name");
    session.on_key(Key::Alt('r')); // record
    press(&mut session, "f");
    session.on_key(Key::Char('=')); // find `=`, at a different column on each line
    // `a`, not `s`: append goes to the *end* of the selection, which is just
    // past that line's own `=`. `s` would go to its start, which is the line
    // start on every line and would show nothing.
    press(&mut session, "a");
    session.on_key(Key::Char('!'));
    session.on_key(Key::Esc);

    // The `!` lands after each line's own `=`, not at a fixed column.
    assert_eq!(
        session.contents().unwrap(),
        "a=!1;\nbb=!22;\nccc=!333;\nrest\n"
    );
}

#[test]
fn the_commands_meow_makes_no_ops_in_beacon_state_are_no_ops() {
    // Exactly the list `meow-keymap` remaps to `meow-beacon-noop`, and only
    // that list: `x` deletes a character, `y` copies, `A` and `S` open lines.
    // `c` and `k` are *not* on it, and Meow really does let them run.
    let mut session = editing("one\ntwo\nthree\n");
    press(&mut session, "ll");
    press(&mut session, "G");
    press(&mut session, "h");
    assert_eq!(session.state(), State::Beacon);
    let before = session.contents().unwrap();

    press(&mut session, "xyAS");

    assert_eq!(session.contents().unwrap(), before);
}

#[test]
fn recording_with_no_beacons_signals() {
    let mut session = editing("one\n");
    // Force Beacon State without a Grab behind it.
    let signals = session.signals();
    press(&mut session, "l");
    assert_ne!(session.state(), State::Beacon, "no Grab, no Beacon");
    assert_eq!(session.signals(), signals);
}

// --- Keypad State and the leader ------------------------------------------

#[test]
fn space_opens_keypad_state_and_a_sequence_dispatches() {
    let mut session = editing("alpha\n");
    session.on_resize(80, 12);
    assert_eq!(session.on_key(Key::Char(' ')), Action::Redraw);
    assert_eq!(session.state(), State::Keypad);

    session.on_key(Key::Char('w'));
    assert_eq!(session.keypad(), &[Key::Char('w')], "a prefix, waiting");
    assert_eq!(session.window_count(), 1);

    session.on_key(Key::Char('s'));

    assert_eq!(session.window_count(), 2, "SPC w s split");
    assert_eq!(session.state(), State::Normal, "and left Keypad State");
    assert!(session.keypad().is_empty());
}

#[test]
fn backspace_pops_a_key_and_leaves_the_leader_when_none_are_left() {
    // `meow-keypad-undo` pops one key and then decides: "(if
    // meow--keypad-keys (redisplay) (message \"KEYPAD exit\")
    // (meow--keypad-quit))". `SPC w` holds *one* key -- `meow-keypad` is
    // `meow-keypad-start-with nil` and Doom's leader takes the base-keymap
    // branch rather than seeding a dispatch prefix -- so popping it empties
    // the list and Meow leaves. meowr kept you in the leader at an empty
    // prefix and wanted a second backspace to get out (#94).
    let mut session = editing("alpha\n");
    session.on_key(Key::Char(' '));
    session.on_key(Key::Char('w'));
    assert_eq!(session.keypad(), &[Key::Char('w')]);

    session.on_key(Key::Backspace);

    assert_eq!(session.state(), State::Normal, "one backspace, not two");
    assert!(session.keypad().is_empty());
}

#[test]
fn backspace_at_the_bare_leader_leaves_it_too() {
    let mut session = editing("alpha\n");
    session.on_key(Key::Char(' '));
    assert_eq!(session.state(), State::Keypad);

    session.on_key(Key::Backspace);

    assert_eq!(session.state(), State::Normal);
}

#[test]
fn escape_leaves_the_leader_from_a_prefix() {
    let mut session = editing("alpha\n");
    session.on_key(Key::Char(' '));
    session.on_key(Key::Char('f'));

    session.on_key(Key::Esc);

    assert_eq!(session.state(), State::Normal);
}

#[test]
fn backspace_below_a_nested_prefix_steps_back_rather_than_leaving() {
    // The branch the shipped tree cannot reach: all eight of its prefixes are
    // one key deep, so there is never anything to step *back to*. Doom's tree
    // has plenty, and a config can add one, so the rule is the general one
    // rather than "always leave".
    let mut session = editing("alpha\n");
    let mut set = Declarations::default();
    set.push(Declaration::Prefix {
        keys: vec![Key::Char(' '), Key::Char('z')],
        name: "zed".into(),
    });
    set.push(Declaration::Prefix {
        keys: vec![Key::Char(' '), Key::Char('z'), Key::Char('y')],
        name: "why".into(),
    });
    set.push(Declaration::Bind {
        state: State::Normal,
        keys: vec![
            Key::Char(' '),
            Key::Char('z'),
            Key::Char('y'),
            Key::Char('q'),
        ],
        command: "quit".into(),
    });
    session.apply(set).expect("a nested prefix is bindable");

    session.on_key(Key::Char(' '));
    session.on_key(Key::Char('z'));
    session.on_key(Key::Char('y'));
    assert_eq!(session.keypad(), &[Key::Char('z'), Key::Char('y')]);

    session.on_key(Key::Backspace);

    assert_eq!(session.keypad(), &[Key::Char('z')], "one key back");
    assert_eq!(session.state(), State::Keypad, "and still in the leader");
}

#[test]
fn backspace_in_normal_state_with_nothing_pending_is_left_alone() {
    let mut session = editing("alpha\n");

    session.on_key(Key::Backspace);

    assert_eq!(session.contents().unwrap(), "alpha\n");
    assert_eq!(session.state(), State::Normal);
}

#[test]
fn an_undefined_key_does_nothing_and_certainly_does_not_type_itself() {
    // `meow-keypad-self-insert-undefined` is nil in the reference
    // configuration, which is a clue about the expected behaviour.
    let mut session = editing("alpha\n");
    let before = session.contents().unwrap();
    let signals = session.signals();

    session.on_key(Key::Char(' '));
    session.on_key(Key::Char('Z'));

    assert_eq!(session.contents().unwrap(), before);
    assert_eq!(session.keypad(), &[] as &[Key], "the key is not kept");
    assert_eq!(session.state(), State::Keypad, "and the leader stays open");
    assert_eq!(session.signals(), signals + 1);
}

#[test]
fn a_reserved_key_does_nothing_and_says_when_it_is_coming() {
    let mut session = editing("alpha\n");
    session.on_key(Key::Char(' '));
    // `SPC k c`, compile. This test asked `SPC k` until #118 turned it into a
    // prefix, then `k a`, then `k d` -- each time a row a milestone claimed --
    // and then `k a` again as `unscheduled` once M5 had bound every row it
    // claimed, then as M6's (#159), until #169 bound it. Which answer it gives
    // moves with the milestones, so the assertion is the rule rather than the
    // note: a reservation says *when*, and `unscheduled` is one of the ways to
    // say it.
    session.on_key(Key::Char('k'));
    let code = session
        .menu()
        .iter()
        .find(|e| e.key == Key::Char('c'))
        .cloned()
        .expect("SPC k c is listed");
    assert_eq!(code.kind, LeaderKind::Reserved);
    assert!(
        meowr_app::leader::RESERVATION_OPENINGS
            .iter()
            .any(|opening| code.description.starts_with(opening)),
        "{}",
        code.description
    );

    session.on_key(Key::Char('c'));
    assert_eq!(session.state(), State::Normal, "it does nothing and closes");
}

#[test]
fn menu_descriptions_come_from_the_command_registry() {
    // Not from a table beside it: two sources for one fact drift, and the one
    // nobody looks at drifts first.
    let mut session = editing("alpha\n");
    session.on_key(Key::Char(' '));
    session.on_key(Key::Char('w'));

    let menu = session.menu();
    let split = menu.iter().find(|e| e.key == Key::Char('s')).unwrap();
    assert_eq!(split.kind, LeaderKind::Command);
    assert_eq!(
        split.description, "Split the window, the new one below",
        "verbatim from `Command::described`"
    );
}

#[test]
fn the_menu_appears_only_after_an_idle_delay() {
    // Which-key's model: someone typing `SPC w s` fluently should never see
    // it, and a menu that flashes on every leader key is noise.
    let mut session = editing("alpha\n");
    session.on_key(Key::Char(' '));
    assert!(!session.menu_visible(), "not immediately");

    assert_eq!(session.show_menu(), Action::Redraw);
    assert!(session.menu_visible());

    // Another key hides it again until the delay passes afresh.
    session.on_key(Key::Char('w'));
    assert!(!session.menu_visible());
}

// --- Ring 3 and hooks -----------------------------------------------------

#[test]
fn ctrl_q_leaves_insert_state() {
    let mut session = editing("alpha\n");
    press(&mut session, "s");
    assert_eq!(session.state(), State::Insert);

    session.on_key(Key::Ctrl('q'));

    assert_eq!(session.state(), State::Normal);
}

#[test]
fn the_surround_keymap_wraps_and_unwraps_the_selection() {
    let mut session = editing("alpha beta\n");
    press(&mut session, "w"); // select "alpha"

    session.on_key(Key::Alt('\''));
    session.on_key(Key::Char('r'));
    assert_eq!(session.contents().unwrap(), "(alpha) beta\n");

    // The selection still names the text, now inside the delimiters.
    session.set_selection(Selection::typed(core::Axis::Select, core::Kind::WORD, 1, 6));
    session.on_key(Key::Alt('\''));
    session.on_key(Key::Char('\''));
    assert_eq!(session.contents().unwrap(), "alpha beta\n");
}

/// Meow binds the surround keymap to `M-'`, and `M-` is the only way a
/// terminal can reach it: `C-'` has no encoding a terminal can send.
#[test]
fn the_surround_keymap_opens_on_the_meta_key() {
    let mut session = editing("alpha beta\n");
    press(&mut session, "w"); // select "alpha"

    session.on_key(Key::Alt('\''));
    session.on_key(Key::Char('r'));

    assert_eq!(session.contents().unwrap(), "(alpha) beta\n");
}

#[test]
fn unwrapping_something_that_is_not_wrapped_is_refused() {
    let mut session = editing("alpha beta\n");
    press(&mut session, "w");
    let before = session.contents().unwrap();
    let signals = session.signals();

    session.on_key(Key::Alt('\''));
    session.on_key(Key::Char('\''));

    assert_eq!(session.contents().unwrap(), before);
    assert_eq!(session.signals(), signals + 1);
    // Named "refused" since it was written, and only a beep until #88.
    assert_eq!(session.echo(), Some("The selection is not wrapped."));
}

#[test]
fn saving_from_insert_state_leaves_insert_state_by_way_of_a_hook() {
    // The point of the slice: a user preference that lives *outside* the
    // command it affects. A flag inside `write-file` would have worked and
    // would have proved nothing.
    let mut session = editing("alpha\n");
    savable(&mut session);
    press(&mut session, "s");
    assert_eq!(session.state(), State::Insert);

    session.on_key(Key::Ctrl('x'));
    let action = session.on_key(Key::Ctrl('s'));

    assert_eq!(action, Action::Save, "the host is still asked to write");
    assert_eq!(session.state(), State::Normal, "and the hook fired");
}

#[test]
fn a_hook_can_be_registered_and_removed_by_name() {
    // A hook that cannot be removed is a leak, and M2's Steel bindings will
    // add and remove them from a config file that gets re-evaluated.
    let mut session = editing("alpha\n");
    assert!(
        session
            .hook_names()
            .contains(&"auto-exit-insert-on-save".to_string())
    );

    assert!(session.remove_hook(HookPoint::BeforeSave, "auto-exit-insert-on-save"));
    assert!(
        !session.remove_hook(HookPoint::BeforeSave, "auto-exit-insert-on-save"),
        "only once"
    );

    press(&mut session, "s");
    session.on_key(Key::Ctrl('x'));
    session.on_key(Key::Ctrl('s'));
    assert_eq!(
        session.state(),
        State::Insert,
        "without the hook, saving no longer leaves Insert State"
    );
}

#[test]
fn a_before_command_hook_can_refuse_the_command() {
    // Observing is half of it; changing behaviour from outside the command is
    // the half the Steel seam actually needs.
    let mut session = editing("alpha beta\n");
    session.add_hook("refuse-everything", HookPoint::BeforeCommand, |_| {
        Proceed::No
    });

    press(&mut session, "wk");

    assert_eq!(session.contents().unwrap(), "alpha beta\n", "nothing ran");

    session.remove_hook(HookPoint::BeforeCommand, "refuse-everything");
    press(&mut session, "wk");
    assert_eq!(session.contents().unwrap(), " beta\n");
}

#[test]
fn registering_a_hook_twice_by_the_same_name_replaces_it() {
    let mut session = editing("alpha\n");
    let before = session.hook_names().len();
    session.add_hook("mine", HookPoint::AfterCommand, |_| Proceed::Yes);
    session.add_hook("mine", HookPoint::AfterCommand, |_| Proceed::Yes);
    assert_eq!(session.hook_names().len(), before + 1);
}

#[test]
fn neither_the_sixth_state_nor_the_motion_remap_exists() {
    // Both were removed from Ring 3 deliberately (round 5, Q24): a Guest gets
    // the keyboard through Motion State instead — ADR-0005.
    let mut session = editing("alpha\n");
    let before = session.state();
    session.on_key(Key::Ctrl(']'));
    assert_eq!(session.state(), before, "no sixth state to enter");

    // `C-e`, `C-n` and friends are not remapped to motions in Normal State.
    let point = session.point();
    session.on_key(Key::Ctrl('e'));
    assert_eq!(session.point(), point);
}

// --- the Normal State keys slice 06 left unbound ---------------------------

#[test]
fn j_selects_the_indentation_joining_this_line_to_the_last() {
    let mut session = editing("a\n\n  b");
    session.set_selection(Selection::at(5)); // on `b`

    press(&mut session, "j");

    assert_eq!(session.selection().range(), 1..5);
}

#[test]
fn a_negative_argument_makes_j_reach_forward() {
    // `meow--join-forward`: from this line's end over the blanks after it.
    let mut session = editing("a\n\n  b");
    session.set_selection(Selection::at(0)); // on `a`

    press(&mut session, "-j");

    assert_eq!(session.selection().range(), 1..5);
}

#[test]
fn repeating_j_stays_a_join() {
    // Meow sends a repeat to `meow--join-both`, which reaches from the point
    // rather than the line boundaries. After a backward join the point is on
    // the first non-blank, so there is nothing forward of it to add and the
    // range does not move — the branch is taken, and lands where it was.
    let mut session = editing("a\n\n  b");
    session.set_selection(Selection::at(5));

    press(&mut session, "jj");

    assert_eq!(session.selection().range(), 1..5);
    assert_eq!(session.selection().kind(), Some(Kind::JOIN));
}

#[test]
fn capital_o_expands_to_the_end_of_the_next_block() {
    let mut session = editing("f(g(x), y) end");
    session.set_selection(Selection::at(0));

    press(&mut session, "O");

    assert_eq!(session.selection().range(), 0..10);
}

#[test]
fn quote_repeats_the_last_command() {
    let mut twice = editing("alpha beta gamma\n");
    press(&mut twice, "ww");

    let mut repeated = editing("alpha beta gamma\n");
    press(&mut repeated, "w'");

    assert_eq!(repeated.selection().range(), twice.selection().range());
}

#[test]
fn repeat_does_not_repeat_itself() {
    // Emacs's `repeat` re-runs the command before it, never itself, so a
    // second `'` advances again rather than standing still.
    let mut session = editing("alpha beta gamma delta\n");
    press(&mut session, "w'");
    let after_two = session.selection().range();

    press(&mut session, "'");

    assert_ne!(session.selection().range(), after_two);
}

#[test]
fn capital_l_goes_to_the_line_the_argument_names() {
    let mut session = editing("one\ntwo\nthree\n");

    press(&mut session, "2L"); // no selection yet, so `2` builds an argument

    assert_eq!(session.selection().range(), 4..7); // "two"
}

#[test]
fn capital_l_with_no_argument_refuses_rather_than_guessing_a_line() {
    // Emacs prompts for the number here. meowr has no prompt to offer, so it
    // signals instead of inventing a destination.
    let mut session = editing("one\ntwo\nthree\n");
    let signals = session.signals();

    press(&mut session, "L");

    assert_eq!(session.signals(), signals + 1);
}

#[test]
fn a_selection_with_no_width_does_not_break_the_modeline() {
    // `2 L` onto an empty line makes a typed selection spanning nothing, which
    // is what Emacs makes there too: `line-beginning-position` and
    // `line-end-position` are the same offset. The modeline still has to
    // report it, and one empty line is one line, not zero.
    let mut session = editing("one\n\nthree\n");

    press(&mut session, "2L");
    let text = session.modeline_of(session.focus()).text();

    assert_eq!(session.selection().range(), 4..4);
    assert!(text.contains("1L 0C"), "modeline said: {text}");
}

#[test]
fn repeat_leaves_the_same_selection_the_command_would_have() {
    // Not just the same range: the same type. A repeat that moved the point
    // but dropped the selection would still pass a range-only check.
    let mut twice = editing("alpha beta gamma\n");
    press(&mut twice, "ww");

    let mut repeated = editing("alpha beta gamma\n");
    press(&mut repeated, "w'");

    assert_eq!(
        repeated.selection().selection_type(),
        twice.selection().selection_type()
    );
}

#[test]
fn a_negative_argument_makes_capital_o_reach_backward() {
    // `meow-to-block`'s `back` flag comes from a negative argument as well as
    // from the direction of the block selection it starts from.
    let mut session = editing("(a) (b) end");
    session.set_selection(Selection::at(8)); // on `e`

    press(&mut session, "-O");

    assert_eq!(session.selection().range(), 4..8);
    assert!(session.selection().is_backward());
}

// --- #20: "Negative arguments reach motions and expand alike" -------------

#[test]
fn a_negative_argument_makes_w_select_backward() {
    let mut negated = editing("alpha beta gamma\n");
    negated.set_selection(Selection::at(11));
    press(&mut negated, "-w");

    let mut backward = editing("alpha beta gamma\n");
    backward.set_selection(Selection::at(11));
    press(&mut backward, "b");

    assert_eq!(negated.selection().range(), backward.selection().range());
}

#[test]
fn a_count_makes_w_cross_that_many_words() {
    let mut session = editing("alpha beta gamma delta\n");

    press(&mut session, "3w"); // no selection, so `3` is an argument

    assert_eq!(session.selection().range().end, 16); // end of "gamma"
}

#[test]
fn a_negative_argument_reverses_the_mark_but_a_count_does_not() {
    // `meow-mark-word` reads `(< n 0)` and nothing else.
    let mut backward = editing("alpha beta gamma\n");
    backward.set_selection(Selection::at(8));
    press(&mut backward, "-m");

    let mut counted = editing("alpha beta gamma\n");
    counted.set_selection(Selection::at(8));
    press(&mut counted, "3m");

    assert!(backward.selection().is_backward());
    assert!(!counted.selection().is_backward());
    assert_eq!(backward.selection().range(), counted.selection().range());
}

#[test]
fn a_count_makes_a_line_motion_move_that_many_lines() {
    // `meow-next`: "Use with numeric argument to move multiple lines at once."
    let mut session = editing("one\ntwo\nthree\nfour\n");

    press(&mut session, "3n");

    assert_eq!(session.point(), 14); // start of "four"
}

#[test]
fn a_negative_argument_turns_a_line_motion_round() {
    // A negative prefix reaches `next-line`, which then moves up.
    let mut session = editing("one\ntwo\nthree\nfour\n");
    session.set_selection(Selection::at(14));

    press(&mut session, "-n");

    assert_eq!(session.point(), 8); // start of "three"
}

#[test]
fn repeat_carries_the_argument_the_repeated_command_had() {
    // Emacs's `repeat` falls back to `last-prefix-arg`, so `3 w '` crosses
    // three words a second time and not one. Only observable now that the
    // motions read the argument at all.
    //
    // Spelled out rather than compared against `3 w 3 w`: with a selection in
    // hand the second `3` is `meow-expand-3` and not an argument at all, so
    // that sequence is a different thing and makes no control.
    let mut session = editing("a b c d e f g h i j\n");

    press(&mut session, "3w");
    assert_eq!(session.selection().range(), 0..5); // through "c"

    press(&mut session, "'");

    assert_eq!(session.selection().range(), 5..11); // three more, through "f"
}

#[test]
fn repeat_keeps_the_direction_a_negative_find_was_given() {
    // Emacs's `repeat` hands the command the prefix it had, so `- f x` then
    // `'` searches backward again rather than turning round.
    let mut session = editing("axbxcxd\n");
    session.set_selection(Selection::at(6)); // on `d`
    press(&mut session, "-f");
    session.on_key(Key::Char('x'));
    let first = session.selection().range();

    press(&mut session, "'");
    session.on_key(Key::Char('x'));

    assert!(
        session.selection().range().start < first.start,
        "second find went forward: {first:?} then {:?}",
        session.selection().range()
    );
}

// --- the hook points that were declared and never fired --------------------

#[test]
fn the_state_change_hook_runs_when_a_command_changes_state() {
    use std::cell::Cell;
    use std::rc::Rc;
    let count = Rc::new(Cell::new(0usize));
    let seen = Rc::clone(&count);

    let mut session = editing("alpha\n");
    session.add_hook("count-state-changes", HookPoint::StateChange, move |_| {
        seen.set(seen.get() + 1);
        Proceed::Yes
    });

    press(&mut session, "s"); // `meow-insert`, Normal -> Insert

    assert_eq!(session.state(), State::Insert);
    assert_eq!(count.get(), 1, "one change, one firing");
}

#[test]
fn the_state_change_hook_does_not_run_when_the_state_held() {
    use std::cell::Cell;
    use std::rc::Rc;
    let count = Rc::new(Cell::new(0usize));
    let seen = Rc::clone(&count);

    let mut session = editing("alpha beta\n");
    session.add_hook("count-state-changes", HookPoint::StateChange, move |_| {
        seen.set(seen.get() + 1);
        Proceed::Yes
    });

    press(&mut session, "w"); // a motion: Normal throughout

    assert_eq!(count.get(), 0);
}

#[test]
fn the_after_save_hook_runs_once_the_host_has_written() {
    use std::cell::Cell;
    use std::rc::Rc;
    let count = Rc::new(Cell::new(0usize));
    let seen = Rc::clone(&count);

    let mut session = editing("alpha\n");
    session.add_hook("count-saves", HookPoint::AfterSave, move |_| {
        seen.set(seen.get() + 1);
        Proceed::Yes
    });

    // `before-save` fires inside the command; the write itself is the host's,
    // and `mark_saved` is how it says the write happened.
    session.mark_saved();

    assert_eq!(count.get(), 1);
}

#[test]
fn the_window_thing_is_what_the_viewport_shows() {
    // The lexical registry deliberately has no `window`: it is a property of
    // the viewport, which `meowr-core` cannot see. The session supplies it,
    // and until it did, `, w` was a bound key that did nothing.
    let text: String = (0..20).map(|i| format!("line{i}\n")).collect();
    let session = editing(&text); // the test viewport is ten rows high

    let range = session
        .thing_range(0, "window", Extent::Bounds)
        .expect("`window` is a Thing the session answers for");

    // Nine lines, not ten: the modeline takes a row off the window, and the
    // Thing is what is *shown*. Each "lineN\n" is six bytes.
    let doc = session.document().unwrap();
    assert_eq!(range, 0..54);
    assert_eq!(session.viewport().height, 9);
    assert!(range.end < doc.byte_len(), "the window is not the buffer");
}

#[test]
fn replaying_keys_does_not_toggle_beacon_state() {
    // `meow--maybe-toggle-beacon-state` is wrapped in
    // `(unless (or defining-kbd-macro executing-kbd-macro) ...)`. meowr had
    // the `defining` half — it refuses while recording — and not the
    // `executing` half, so a beacon replay could switch State underneath
    // itself mid-macro.
    // `G` returns early from `dispatch`, so the toggle happens on the key
    // after it — here `g`, which leaves the point inside the Grab.
    let mut typed = editing("hello world\nsecond line\n");
    press(&mut typed, "wGg");
    assert_eq!(typed.state(), State::Beacon);

    let mut replayed = editing("hello world\nsecond line\n");
    replayed.replay(&[Key::Char('w'), Key::Char('G'), Key::Char('g')]);

    assert_eq!(replayed.state(), State::Normal, "a replay does not toggle");
    assert_eq!(replayed.grab(), typed.grab(), "but the Grab is the same");
}

// --- Ring 2 against the extracted Doom tree -------------------------------

/// The top-level keys `~/docs/meowr-doom-leader.tsv` binds.
///
/// `C-f` is among them and is the reason a leader key is a `Key`: it is the
/// one Doom prefix that cannot be spelled as a character.
const DOOM_TOP_LEVEL: &[Key] = &[
    Key::Char('v'),
    Key::Char('s'),
    Key::Char('k'),
    Key::Char('f'),
    Key::Char('d'),
    Key::Char('n'),
    Key::Char('q'),
    Key::Char('t'),
    Key::Char('o'),
    Key::Char('b'),
    Key::Char('w'),
    Key::Char('i'),
    Key::Char('&'),
    Key::Char('p'),
    Key::Char('u'),
    Key::Char('m'),
    Key::Char('l'),
    Key::Char('h'),
    Key::Char('e'),
    Key::Char('a'),
    Key::Ctrl('f'),
    // The author's own, from `config-buffer.el` — the TSV extracts them
    // alongside stock Doom's, and this list omitted them until #75. `.` and
    // `RET` are bound now; `,` is dired and `?` is the cheatsheet, both in
    // `DROPPED`.
    Key::Char('.'),
    Key::Enter,
    Key::Char(','),
    Key::Char('?'),
    // The twelve #77 accounted for. `SPC SPC` is bound (#147), `ESC` is dropped
    // for a reason no package could give, and the ten digits are handled in
    // `keypad_key` rather than in this tree — they are an argument for the
    // walk, not a step in it, so there is no Seed to find.
    Key::Char(' '),
    Key::Esc,
    Key::Char('0'),
    Key::Char('1'),
    Key::Char('2'),
    Key::Char('3'),
    Key::Char('4'),
    Key::Char('5'),
    Key::Char('6'),
    Key::Char('7'),
    Key::Char('8'),
    Key::Char('9'),
];

#[test]
fn the_leader_reserves_the_keys_doom_actually_uses() {
    // Decision #12 is "reserve, do not reassign", and a reservation on the
    // wrong key reassigns by another name. `~/docs/meowr-doom-leader.tsv`
    // puts LSP under `k` (code, 21 bindings) and has no `c` binding at all —
    // so `c` is the key a reservation for "code" would wrongly claim, and it
    // stays unbound.
    //
    // `v` is not asserted here any more. It is versioning, 49 bindings, all
    // magit, and it was reserved for the `wagit` Guest until Guests were
    // dropped; with nothing coming for it the key is dropped rather than held.
    use meowr_app::leader::{Entry, Leader};
    let leader = Leader::new();

    // `k` is code, and since #118 it is a *prefix* named that rather than a
    // reservation standing in for one: `k k` is bound and its nineteen
    // siblings are each held on their own.
    assert!(
        matches!(leader.lookup(&[Key::Char('k')]), Some(Entry::Prefix(name)) if name == "code"),
        "k is code"
    );
    assert!(
        matches!(
            leader.lookup(&[Key::Char('k'), Key::Char('c')]),
            Some(Entry::Reserved(_))
        ),
        "and the rows under it that are not built yet are held"
    );
    assert_eq!(
        leader.lookup(&[Key::Char('c')]),
        None,
        "`c` is not a Doom leader prefix and is not reserved as if it were"
    );
}

#[test]
fn a_leader_key_is_one_doom_uses_or_one_doom_leaves_free() {
    // The tree may hold fewer keys than Doom. What it must not do is claim a
    // key Doom uses for something else — that is decision #12, and it is a
    // rule about *collision*, not about staying inside Doom's alphabet.
    //
    // Some commands have no Doom row to copy because they are Meow's own:
    // the Grab is one, and Meow binds no key for swapping or syncing it. Such
    // a command goes on a key Doom leaves free, so nothing has to be taken
    // back later. `g` is one: Doom has no top-level `g`. It has `SPC v g`
    // (magit-status) further in, but that is a different path, under the `v`
    // subtree this tree reserves rather than claims.
    use meowr_app::leader::Leader;
    let leader = Leader::new();

    const MEOWRS_OWN: &[Key] = &[Key::Char('g')];

    for key in MEOWRS_OWN {
        assert!(
            !DOOM_TOP_LEVEL.contains(key),
            "{key} is Doom's, so claiming it is a reassignment"
        );
    }
    for (key, _) in leader.below(&[]) {
        assert!(
            DOOM_TOP_LEVEL.contains(key) || MEOWRS_OWN.contains(key),
            "`SPC {key}` is not a Doom top-level key and is not claimed here"
        );
    }
}

#[test]
fn the_leader_menu_lists_the_grab_prefix_and_its_keys() {
    let mut session = editing(
        "alpha
",
    );
    press(&mut session, " ");
    session.show_menu();
    let grab = session
        .menu()
        .into_iter()
        .find(|e| e.key == Key::Char('g'))
        .expect("SPC g is listed");
    assert_eq!(grab.kind, meowr_app::LeaderKind::Prefix);
    assert_eq!(grab.description, "+grab");

    press(&mut session, "g");
    session.show_menu();
    let keys: Vec<Key> = session.menu().into_iter().map(|e| e.key).collect();
    assert_eq!(keys, vec![Key::Char('s'), Key::Char('y')]);
}

#[test]
fn the_leader_menu_writes_a_chord_the_way_a_keymap_reference_does() {
    let mut session = editing(
        "alpha
",
    );
    press(&mut session, " ");
    session.show_menu();
    let fold = session
        .menu()
        .into_iter()
        .find(|e| e.key == Key::Ctrl('f'))
        .expect("Doom's fold prefix is in the menu");

    // The subject is the spelling: a chord is a `Key` and the menu writes it
    // the way a keymap reference does. What kind of entry it happens to be is
    // borrowed -- it was `Reserved` until #142 made it the prefix Doom calls
    // it, and this test failed for a change that was not about it.
    assert_eq!(fold.key.to_string(), "C-f");
    assert_eq!(fold.kind, meowr_app::LeaderKind::Prefix);
}

#[test]
fn comma_w_selects_the_window_through_the_key_path() {
    // `thing_range` answering is not enough: the key has to reach it.
    let text: String = (0..20).map(|i| format!("line{i}\n")).collect();
    let mut session = editing(&text);

    press(&mut session, ",w");

    assert_eq!(session.selection().range(), 0..54);
}

#[test]
fn a_line_count_that_runs_out_of_buffer_still_signals() {
    // Emacs's `line-move` moves as far as it can and *then* signals
    // `end-of-buffer`. `next_lines` stops at the first refusal so the signal
    // survives the count — and `vertical` can now tell it has run out, which
    // it could not while `byte_to_line` answering `None` at `byte_len` put the
    // point a line early.
    let mut session = editing("one\ntwo\n");
    let signals = session.signals();

    press(&mut session, "9n");

    assert_eq!(session.signals(), signals + 1);
}

#[test]
fn a_single_line_motion_at_the_end_of_the_buffer_signals_too() {
    let mut session = editing("one\ntwo\n");
    press(&mut session, "nn"); // to the last line
    let signals = session.signals();

    press(&mut session, "n");

    assert_eq!(session.signals(), signals + 1);
}

#[test]
fn two_windows_on_one_buffer_keep_their_own_points() {
    // #26: "Each window keeps its own point and scroll position." The scroll
    // was already per window; the point was not, so two windows on one file
    // dragged each other's cursor about.
    let mut session = editing("one\ntwo\nthree\nfour\nfive\n");
    session.on_key(Key::Ctrl('x'));
    session.on_key(Key::Char('2')); // split; both show the same buffer
    assert_eq!(session.window_count(), 2);

    press(&mut session, "nnn"); // this window goes to line 4
    let here = session.point();

    session.on_key(Key::Ctrl('x'));
    session.on_key(Key::Char('o')); // to the other window
    let there = session.point();

    assert_ne!(there, here, "the other window kept its own point");

    session.on_key(Key::Ctrl('x'));
    session.on_key(Key::Char('o')); // and back
    assert_eq!(session.point(), here, "which is still where we left it");
}

#[test]
fn a_session_command_records_itself_for_repeat() {
    // The hand-written arms returned straight out of `dispatch` without
    // recording, so `G` left `last_command` pointing at whatever ran before
    // it and `'` repeated that instead. Every entry in `SESSION_COMMANDS`
    // records itself now.
    let mut session = editing("alpha beta gamma\n");
    press(&mut session, "wG"); // grab "alpha"; the selection is cancelled
    assert!(session.grab().is_some());

    // `'` repeats the grab, which with no selection puts the Grab away.
    press(&mut session, "'");

    assert!(session.grab().is_none(), "`'` repeated `G`, not `w`");
}

#[test]
fn cancelling_a_narrow_puts_the_point_back() {
    // Preview moves the point into the window behind — that is the point of
    // previewing there rather than into a panel. Cancelling has to undo it,
    // or the preview is a move you did not ask for.
    let mut session = editing("one\ntwo\nthree\nfour\n");
    let before = session.point();

    session.open_picker();
    press(&mut session, "t");
    session.preview();
    assert_ne!(session.point(), before, "preview moved the point");

    session.cancel_narrowing();

    assert_eq!(session.point(), before, "and cancelling put it back");
}

// --- quitting with unsaved work -------------------------------------------

#[test]
fn quitting_with_unsaved_changes_refuses() {
    // Doom's `SPC q q` is `save-buffers-kill-terminal`, which prompts rather
    // than throwing the work away. meowr has no prompt, so it refuses.
    let mut session = editing("alpha\n");
    session.set_path("notes.rs"); // a buffer with somewhere to be written
    press(&mut session, "sZ"); // insert, making it modified
    session.on_key(Key::Esc);
    assert!(session.is_modified());
    let signals = session.signals();

    let action = session.on_key(Key::Ctrl('c'));

    assert_ne!(action, Action::Quit, "it did not quit");
    assert_eq!(session.signals(), signals + 1, "it signalled");

    // The signal count is invisible on screen; the Echo Area is what the
    // person actually sees, and it has to name the file and the way out.
    let echo = session.echo().expect("the Echo Area says why");
    assert!(echo.contains("notes.rs"), "names the buffer: {echo}");
    assert!(echo.contains("SPC q Q"), "names the way past: {echo}");
    assert!(echo.contains("C-x C-s"), "names how to save: {echo}");
}

#[test]
fn the_echo_area_is_cleared_by_the_next_key() {
    // Emacs clears its echo area on the next command; a message that outlives
    // the command that wrote it starts describing something already past.
    let mut session = editing("alpha\n");
    press(&mut session, "sZ");
    session.on_key(Key::Esc);
    session.on_key(Key::Ctrl('c'));
    assert!(session.echo().is_some());

    press(&mut session, "h");

    assert!(session.echo().is_none());
}

#[test]
fn a_pathless_buffer_is_not_told_to_save() {
    // `*scratch*` has nowhere to be written, so `write-file` would do nothing
    // and advising it would be a dead end.
    let mut session = Session::new(Viewport { top: 0, height: 10 });
    session.loaded(Document::from_text("alpha\n"));
    press(&mut session, "sZ");
    session.on_key(Key::Esc);

    session.on_key(Key::Ctrl('c'));

    let echo = session.echo().expect("it still refuses");
    assert!(!echo.contains("Save with"), "no dead-end advice: {echo}");
    assert!(echo.contains("SPC q Q"), "but still a way out: {echo}");
}

#[test]
fn quitting_is_fine_once_the_buffer_is_saved() {
    let mut session = editing("alpha\n");
    press(&mut session, "sZ");
    session.on_key(Key::Esc);
    session.mark_saved();

    assert_eq!(session.on_key(Key::Ctrl('c')), Action::Quit);
}

#[test]
fn quit_without_saving_goes_anyway() {
    // `SPC q Q` is `kill-emacs`: "Quit Emacs without saving".
    let mut session = editing("alpha\n");
    press(&mut session, "sZ");
    session.on_key(Key::Esc);
    assert!(session.is_modified());

    press(&mut session, " q");
    let action = session.on_key(Key::Char('Q'));

    assert_eq!(action, Action::Quit);
}

// --- binding Ring 2 at runtime (#58) ---------------------------------------

/// A leader sequence, written the way the menu writes it.
fn leader_keys(written: &str) -> Vec<Key> {
    meowr_command::parse_keys(written).expect("a key sequence")
}

#[test]
fn the_keys_for_a_command_are_looked_for_in_both_rings() {
    let session = editing("alpha\n");
    let wrote = |command| {
        session
            .keys_for(State::Normal, command)
            .map(|keys| meowr_command::Keys(&keys).to_string())
    };

    // Ring 1, where most commands are.
    assert_eq!(wrote("left").as_deref(), Some("h"));
    // Ring 2 alone: nothing in the keymap reaches `recent-files`, and
    // answering `None` would say "unbound" about a command two keys away.
    assert_eq!(wrote("recent-files").as_deref(), Some("SPC RET"));
    // Both. The keymap wins, because it is the shorter way to the same place.
    assert_eq!(wrote("write-file").as_deref(), Some("C-x C-s"));
    // Neither.
    assert_eq!(wrote("beacon-start"), None);
}

#[test]
fn a_leader_route_is_written_with_whatever_key_opens_the_leader() {
    // The leader tree stores suffixes, so the key in front of one is the
    // keymap's business — and Ring 3 lets it move. Writing `SPC` into the
    // answer would be the second source for one fact this codebase keeps
    // refusing to create.
    let mut session = editing("alpha\n");
    let mut set = Declarations::default();
    set.push(Declaration::Unbind {
        state: State::Normal,
        keys: leader_keys("SPC"),
    });
    set.push(Declaration::Bind {
        state: State::Normal,
        keys: leader_keys("t"),
        command: "leader".into(),
    });

    session.apply(set).expect("resolves as a whole");

    assert_eq!(
        session
            .keys_for(State::Normal, "recent-files")
            .map(|keys| meowr_command::Keys(&keys).to_string())
            .as_deref(),
        Some("t RET"),
    );
}

#[test]
fn the_key_that_opens_the_leader_is_the_key_routing_uses() {
    // Two sources for one fact drift. The keymap binds a key to `leader` and
    // `Session::bind` routes on `leader::KEY`; they have to be the same key,
    // or a config binds into a tree nothing opens.
    let mut session = editing("alpha\n");

    session.on_key(meowr_app::leader::KEY);

    assert_eq!(session.state(), State::Keypad, "that key opens Ring 2");
}

#[test]
fn a_leader_sequence_binds_through_the_ordinary_bind_path() {
    let mut session = editing("alpha\n");
    savable(&mut session);
    session
        .bind_prefix(&leader_keys("SPC d"), "dape")
        .expect("`d` is free");

    session
        .bind(State::Normal, &leader_keys("SPC d s"), "write-file")
        .expect("under a prefix that exists");

    session.on_key(meowr_app::leader::KEY);
    session.on_key(Key::Char('d'));
    assert_eq!(
        session.on_key(Key::Char('s')),
        Action::Save,
        "the command the config named actually ran"
    );
}

#[test]
fn a_prefix_shows_the_name_it_was_given() {
    let mut session = editing("alpha\n");
    session.bind_prefix(&leader_keys("SPC d"), "dape").unwrap();

    press(&mut session, " ");
    session.show_menu();
    let entry = session
        .menu()
        .into_iter()
        .find(|e| e.key == Key::Char('d'))
        .expect("the new prefix is listed");
    assert_eq!(entry.kind, meowr_app::LeaderKind::Prefix);
    assert_eq!(entry.description, "+dape");
}

#[test]
fn a_leader_key_can_be_unbound() {
    let mut session = editing("alpha\n");
    assert!(session.unbind(State::Normal, &leader_keys("SPC b b")));

    press(&mut session, " ");
    session.show_menu();
    press(&mut session, "b");
    session.show_menu();
    let keys: Vec<Key> = session.menu().into_iter().map(|e| e.key).collect();
    assert!(
        !keys.contains(&Key::Char('b')),
        "gone from the menu: {keys:?}"
    );
}

#[test]
fn a_reserved_leader_key_can_be_bound_over() {
    // Decision #12 is a promise meowr makes about the keys *it* ships, not a
    // lock on the user's own config.
    let mut session = editing("alpha\n");
    savable(&mut session);
    session
        .bind(State::Normal, &leader_keys("SPC v"), "write-file")
        .expect("a reservation is meowr's restraint, not the user's");

    session.on_key(meowr_app::leader::KEY);
    assert_eq!(session.on_key(Key::Char('v')), Action::Save);
}

#[test]
fn a_leader_binding_that_would_hide_another_is_refused() {
    let mut session = editing("alpha\n");

    let over = session.bind(State::Normal, &leader_keys("SPC b"), "write-file");
    assert!(over.is_err(), "a command over a subtree hides it");

    let under = session.bind(State::Normal, &leader_keys("SPC b b x"), "write-file");
    assert!(
        under.is_err(),
        "`SPC b b` is a command, so nothing is below it"
    );
}

#[test]
fn a_leader_binding_under_a_prefix_nobody_declared_is_refused() {
    // Which-key exists to say what `SPC d` leads to before you commit, and an
    // auto-created prefix with no name is a blank row in the one interface
    // whose whole job is to have none.
    let mut session = editing("alpha\n");

    let orphan = session.bind(State::Normal, &leader_keys("SPC d s"), "write-file");

    assert!(orphan.is_err(), "declare the prefix first");
}

#[test]
fn spc_l_is_not_a_second_leader() {
    // Doom's table has `+localleader` at `SPC l`. meowr has never implemented
    // it and #58 does not smuggle it in: `SPC l` is an ordinary leader key
    // reaching an ordinary command, not a nested tree.
    let mut session = editing("alpha\n");
    savable(&mut session);
    session
        .bind(State::Normal, &leader_keys("SPC l"), "write-file")
        .expect("an ordinary key under the one leader");

    session.on_key(meowr_app::leader::KEY);
    assert_eq!(session.on_key(Key::Char('l')), Action::Save);
}

// --- a set of declarations, resolved as one (#59) ---------------------------

use meowr_app::declare::{Declaration, Declarations};

/// A set that binds `SPC d s` to a command it also defines.
fn dape_set() -> Declarations {
    let mut set = Declarations::default();
    // Deliberately before the command it names: source order must not matter.
    set.push(Declaration::Bind {
        state: State::Normal,
        keys: leader_keys("SPC d s"),
        command: "start-and-save".into(),
    });
    set.push(Declaration::Prefix {
        keys: leader_keys("SPC d"),
        name: "dape".into(),
    });
    set.push(Declaration::Command {
        name: "start-and-save".into(),
        description: "Start, and save first".into(),
        body: vec!["write-file".into()],
    });
    set
}

#[test]
fn a_binding_may_name_a_command_declared_further_down() {
    // Steel evaluates top to bottom, so eager checking would make source order
    // load-bearing in a way no Emacs user expects.
    let mut session = editing("alpha\n");
    savable(&mut session);

    session.apply(dape_set()).expect("resolves as a whole");

    session.on_key(meowr_app::leader::KEY);
    session.on_key(Key::Char('d'));
    assert_eq!(session.on_key(Key::Char('s')), Action::Save);
}

#[test]
fn the_menu_shows_what_a_resolved_set_declared() {
    let mut session = editing("alpha\n");
    session.apply(dape_set()).unwrap();

    press(&mut session, " ");
    session.show_menu();
    let prefix = session
        .menu()
        .into_iter()
        .find(|e| e.key == Key::Char('d'))
        .expect("the prefix is listed");
    assert_eq!(prefix.description, "+dape");

    press(&mut session, "d");
    session.show_menu();
    let command = session
        .menu()
        .into_iter()
        .find(|e| e.key == Key::Char('s'))
        .expect("the command is listed");
    assert_eq!(
        command.description, "Start, and save first",
        "the description the set gave it"
    );
}

#[test]
fn a_name_nobody_registered_is_reported_and_named() {
    let mut session = editing("alpha\n");
    let mut set = Declarations::default();
    set.push(Declaration::Bind {
        state: State::Normal,
        keys: leader_keys("SPC f g"),
        command: "fnid-file".into(),
    });

    let problems = session.apply(set).expect_err("nothing by that name");

    assert!(
        problems.iter().any(|p| p.to_string().contains("fnid-file")),
        "{problems:?}"
    );
}

#[test]
fn a_composition_cycle_is_reported_and_named() {
    // As data this is a graph to walk before anything runs. As a closure it
    // would have been a stack overflow at the first keypress.
    let mut session = editing("alpha\n");
    let mut set = Declarations::default();
    set.push(Declaration::Command {
        name: "ping".into(),
        description: "Ping".into(),
        body: vec!["pong".into()],
    });
    set.push(Declaration::Command {
        name: "pong".into(),
        description: "Pong".into(),
        body: vec!["ping".into()],
    });

    let problems = session.apply(set).expect_err("a cycle");

    let said = problems
        .iter()
        .map(|p| p.to_string())
        .collect::<Vec<_>>()
        .join("; ");
    assert!(said.contains("ping") && said.contains("pong"), "{said}");
}

#[test]
fn every_problem_in_a_set_is_reported_not_only_the_first() {
    let mut session = editing("alpha\n");
    let mut set = Declarations::default();
    for (key, name) in [
        ("SPC f g", "ghost-one"),
        ("SPC f h", "ghost-two"),
        ("SPC f i", "ghost-three"),
    ] {
        set.push(Declaration::Bind {
            state: State::Normal,
            keys: leader_keys(key),
            command: name.into(),
        });
    }

    let problems = session.apply(set).expect_err("three of them");

    assert_eq!(problems.len(), 3, "{problems:?}");
}

#[test]
fn a_set_that_fails_changes_nothing() {
    // The whole reason the resolve pass exists: a config that throws or does
    // not resolve leaves the defaults standing.
    let mut session = editing("alpha\n");
    let mut set = dape_set();
    set.push(Declaration::Bind {
        state: State::Normal,
        keys: leader_keys("SPC f g"),
        command: "no-such-command".into(),
    });

    session.apply(set).expect_err("one bad name spoils it");

    press(&mut session, " ");
    session.show_menu();
    let keys: Vec<Key> = session.menu().into_iter().map(|e| e.key).collect();
    assert!(
        !keys.contains(&Key::Char('d')),
        "the good half was not applied either: {keys:?}"
    );
    session.on_key(meowr_app::leader::KEY);
    session.on_key(Key::Char('d'));
    assert_ne!(
        session.on_key(Key::Char('s')),
        Action::Save,
        "nor does the binding the set asked for do anything"
    );
}

#[test]
fn a_binding_a_set_could_not_make_is_reported_rather_than_half_applied() {
    // `SPC b` has a subtree under it, so a command there is refused — and the
    // refusal has to leave the rest of the set unapplied too.
    let mut session = editing("alpha\n");
    let mut set = dape_set();
    set.push(Declaration::Bind {
        state: State::Normal,
        keys: leader_keys("SPC b"),
        command: "write-file".into(),
    });

    let problems = session.apply(set).expect_err("ambiguous");

    assert!(!problems.is_empty());
    press(&mut session, " ");
    session.show_menu();
    let keys: Vec<Key> = session.menu().into_iter().map(|e| e.key).collect();
    assert!(!keys.contains(&Key::Char('d')), "{keys:?}");
}

// --- a broken config says so (#63) -----------------------------------------

#[test]
fn a_broken_config_leaves_the_editor_usable() {
    // The failure to design against: an `init.scm` that throws on line one and
    // an editor that will not start to let you repair it.
    let mut session = editing("alpha\n");
    session.config_failed("init.scm:1: unbound identifier: fnid-file", "the detail");

    press(&mut session, "wk");
    assert_eq!(session.contents().unwrap(), "\n", "editing still works");
}

#[test]
fn a_broken_config_says_so_in_one_line_and_names_the_way_to_the_rest() {
    let mut session = editing("alpha\n");

    session.config_failed("init.scm:1: unbound identifier: fnid-file", "the detail");

    let echo = session.echo().expect("it says something");
    assert!(echo.contains("fnid-file"), "what went wrong: {echo}");
    assert!(echo.contains("*config-error*"), "where the rest is: {echo}");
    assert_eq!(echo.lines().count(), 1, "the Echo Area is one line: {echo}");
}

#[test]
fn the_line_names_the_key_the_keymap_gives_rather_than_one_written_down() {
    // A config that rebinds the picker would otherwise be told to press a key
    // it had just taken away — a cruel failure for a message about that config.
    let mut session = editing("alpha\n");
    // Move the picker, the way a config would: take away the keys it ships on
    // and put it somewhere else entirely.
    assert!(session.unbind(State::Normal, &leader_keys("SPC f f")));
    assert!(session.unbind(State::Normal, &leader_keys("SPC f r")));
    session
        .bind(State::Normal, &leader_keys("SPC z"), "find-file")
        .expect("`z` is free");

    session.config_failed("init.scm:1: broken", "the detail");

    let echo = session.echo().expect("it says something");
    assert!(
        echo.contains("SPC z"),
        "the key it is bound to now, not the one it shipped on: {echo}"
    );
}

#[test]
fn the_detail_waits_in_a_buffer_rather_than_taking_the_window() {
    // A config error that seizes the window from the file you asked to open is
    // a second insult after the first.
    let mut session = editing("alpha\n");
    session.set_path("notes.txt");
    let was = session.current_buffer_id();

    session.config_failed("init.scm:1: broken", "line one\nline two\nline three");

    assert_eq!(session.current_buffer_id(), was, "still showing your file");
    assert_eq!(session.rows()[0].text, "alpha");
}

#[test]
fn the_config_error_buffer_is_one_you_can_reach_read_and_close() {
    let mut session = editing("alpha\n");
    session.set_path("notes.txt");
    session.config_failed("init.scm:1: broken", "line one\nline two");

    // Listed in the picker, which is what makes the vanishing echo line safe.
    session.on_key(Key::Ctrl('x'));
    session.on_key(Key::Ctrl('f'));
    session.on_key(Key::Char('<'));
    session.on_key(Key::Char('b'));
    let widget = session.narrowing().expect("the picker opened");
    let names: Vec<String> = (0..widget.matches().len())
        .map(|row| widget.candidate(widget.matches()[row].index).text.clone())
        .collect();
    assert!(
        names.iter().any(|n| n == "*config-error*"),
        "reachable: {names:?}"
    );
    session.on_key(Key::Esc);
}

#[test]
fn the_config_error_buffer_holds_the_detail_and_refuses_to_be_written() {
    let mut session = editing("alpha\n");
    session.set_path("notes.txt");
    session.config_failed("init.scm:1: broken", "line one\nline two");

    let id = session
        .buffers()
        .iter()
        .position(|b| b.name() == "*config-error*")
        .map(meowr_app::buffer::BufferId)
        .expect("it exists");
    session.show(id);

    assert_eq!(session.rows()[0].text, "line one");
    assert_eq!(session.rows()[1].text, "line two");

    // Read-only, tested by trying to change it. Asserting `save_target` is
    // `None` proves nothing here: a pathless buffer answers `None` whether it
    // is read-only or not, so that assertion passes even with the flag gone.
    press(&mut session, "wk");
    assert_eq!(
        session.contents().unwrap(),
        "line one\nline two",
        "a mutating command is refused"
    );
    assert!(
        session.save_target().is_none(),
        "and it visits no file, so there is nowhere to write it either"
    );
}

#[test]
fn skipping_the_config_says_that_too() {
    // Start with `--no-config` to check whether a bug is yours, get distracted,
    // and spend ten minutes wondering why your keys are wrong.
    let mut session = editing("alpha\n");

    session.config_skipped();

    let echo = session.echo().expect("it says so");
    assert!(echo.to_lowercase().contains("config"), "{echo}");
    assert!(
        session
            .buffers()
            .iter()
            .all(|b| b.name() != "*config-error*"),
        "nothing went wrong, so there is nothing to read"
    );
}

// --- SPC t, and read-only as a Buffer property ----------------------------

#[test]
fn spc_t_r_makes_a_writable_buffer_read_only_and_the_modeline_says_so() {
    let mut session = editing("alpha\n");
    assert!(!session.modeline_of(session.focus()).text().contains("%%"));

    press(&mut session, " tr");

    assert!(
        session.modeline_of(session.focus()).text().contains("%%"),
        "Emacs puts `%%` in front of the name: {}",
        session.modeline_of(session.focus()).text()
    );
}

#[test]
fn a_buffer_the_toggle_made_read_only_refuses_an_edit() {
    let mut session = editing("alpha\n");
    press(&mut session, " tr");

    press(&mut session, "sX");
    session.on_key(Key::Esc);

    assert_eq!(session.contents().unwrap(), "alpha\n");
}

#[test]
fn pressing_it_again_makes_the_buffer_writable_and_editing_works() {
    // The whole reason the toggle must not be a mutating command: a mutating
    // command is refused on a read-only buffer, so a mutating toggle could
    // turn read-only on and never off — and off is the direction that matters.
    let mut session = editing("alpha\n");
    press(&mut session, " tr");

    press(&mut session, " tr");

    assert!(!session.modeline_of(session.focus()).text().contains("%%"));
    press(&mut session, "sX");
    session.on_key(Key::Esc);
    assert_eq!(session.contents().unwrap(), "Xalpha\n");
}

#[test]
fn the_toggle_refuses_a_buffer_that_is_read_only_by_construction() {
    // `*config-error*` holds a report and the narrowing preview holds a file
    // nobody chose to open. Neither is read-only as a preference, so neither
    // is a thing to toggle — and saying so is the difference between a
    // refusal and a key that appears not to work.
    let mut session = editing("alpha\n");
    session.config_failed("init.scm: no", "nothing is called `nope`");
    // Opened in the background, so it has to be shown before it can be the
    // buffer a key acts on — #63 put the echo line, not the focus, on it.
    let id = session
        .buffers()
        .iter()
        .position(|b| b.name() == "*config-error*")
        .map(meowr_app::buffer::BufferId)
        .expect("it exists");
    session.show(id);

    press(&mut session, " tr");

    assert!(
        session.modeline_of(session.focus()).text().contains("%%"),
        "still read-only"
    );
    let echo = session.echo().expect("it says why");
    assert!(echo.contains("*config-error*"), "{echo}");
}

#[test]
fn spc_t_is_a_named_prefix_rather_than_a_reservation() {
    // #50 discharged the reservation: `SPC t r` is Doom's `read-only-mode`,
    // and it is the one Doom-precedented toggle meowr can honour — there are
    // no line numbers, no fill column and no minor modes.
    let mut session = editing("alpha\n");
    session.on_key(meowr_app::leader::KEY);

    let toggles = session
        .menu()
        .into_iter()
        .find(|e| e.key == Key::Char('t'))
        .expect("SPC t is listed");

    assert_eq!(toggles.kind, LeaderKind::Prefix, "not a reservation");
    session.on_key(Key::Char('t'));
    assert!(
        session.menu().iter().any(|e| e.key == Key::Char('r')),
        "and `r` is under it"
    );
}

#[test]
fn auto_save_is_an_option_and_gets_no_toggle_key() {
    // #50's distinction, as a test: an Option is session-wide and configured
    // once, a Buffer property belongs to one buffer and is toggled as you
    // work. Only the second gets a key — and Doom's toggle list is
    // `F I b c f l r s v w`, which auto-save is not on.
    let mut session = editing("alpha\n");
    session.on_key(meowr_app::leader::KEY);
    session.on_key(Key::Char('t'));

    let under_t = session.menu();

    // The claim is an absence, so it is asserted as one. This used to compare
    // the whole set against `[r]`, which made it break every time a toggle row
    // got decided — #100 held four of Doom's — while saying nothing more about
    // auto-save than this does.
    assert!(
        under_t
            .iter()
            .all(|e| !e.description.to_lowercase().contains("auto")),
        "{:?}",
        under_t.iter().map(|e| &e.description).collect::<Vec<_>>()
    );
    assert!(
        under_t
            .iter()
            .any(|e| e.key == Key::Char('r') && e.kind == LeaderKind::Command),
        "and read-only, which is a Buffer property, does have its key"
    );
}

// --- command-hooks (#64) ---------------------------------------------------

/// A set that hooks one command to one hook point.
fn hook_set(point: meowr_command::HookPoint, command: &str) -> Declarations {
    let mut set = Declarations::default();
    set.push(Declaration::Hook {
        point,
        command: command.into(),
    });
    set
}

#[test]
fn a_command_hooked_to_after_save_runs_when_the_buffer_is_saved() {
    // The hook cannot run the command itself — a hook is
    // `Fn(&mut Ctx) -> Proceed` and the registry is not reachable from inside
    // one. The session dispatches the name after the closures have run, which
    // is why this is observable at all.
    let mut session = editing("alpha\n");
    session.set_path("notes.txt");
    session
        .apply(hook_set(meowr_command::HookPoint::AfterSave, "open-below"))
        .expect("resolves");

    session.mark_saved();

    assert_eq!(
        session.contents().unwrap(),
        "alpha\n\n",
        "the line it opened"
    );
}

#[test]
fn a_command_hooked_to_state_change_runs_in_the_state_it_moved_to() {
    // The hook fires *after* the move, so the command it names is dispatched
    // in the new State and obeys `valid_in` like any other. `open-below` is
    // Normal-only, so the transition that runs it is the one back *out* of
    // Insert — and going in, it is refused exactly as pressing its key would
    // be.
    let mut session = editing("alpha\n");
    session
        .apply(hook_set(
            meowr_command::HookPoint::StateChange,
            "open-below",
        ))
        .expect("resolves");

    press(&mut session, "s"); // into Insert State
    assert_eq!(
        session.contents().unwrap(),
        "alpha\n",
        "a Normal-only command does not run in Insert"
    );

    session.on_key(Key::Esc); // back to Normal, where it can

    assert_eq!(session.contents().unwrap(), "alpha\n\n");
}

#[test]
fn a_command_run_by_a_hook_fires_no_hooks_of_its_own() {
    // #64's rule is flat: *nothing* fires for a command run by a hook, native
    // closures included. `write-file` answers `Outcome::Save`, which is what
    // makes `before-save` fire, and the native `auto-exit-insert-on-save`
    // there leaves Insert. So hooking `write-file` to `state-change` and
    // entering Insert is the one keystroke that tells the rule from its
    // absence: with it the State stays Insert, without it the closure runs
    // and drops back to Normal.
    let mut session = editing("alpha\n");
    session.set_path("notes.txt");
    session
        .apply(hook_set(
            meowr_command::HookPoint::StateChange,
            "write-file",
        ))
        .expect("resolves");

    press(&mut session, "s"); // Normal -> Insert, firing the hook

    assert_eq!(
        session.state(),
        State::Insert,
        "`before-save` did not fire for a command run by a hook"
    );
}

#[test]
fn a_hook_that_asks_the_host_to_act_is_refused_out_loud() {
    // `mark_saved` is called *by* the host after a write, so an `after-save`
    // hook answering `Action::Save` would bounce back through the host and
    // save again for ever — outside the reach of the in-hook guard, which
    // covers one stack and not a round trip. Said rather than dropped: a key
    // that silently does nothing is the failure this codebase keeps refusing
    // to ship.
    let mut session = editing("alpha\n");
    session.set_path("notes.txt");
    session
        .apply(hook_set(meowr_command::HookPoint::AfterSave, "write-file"))
        .expect("resolves");
    let signals = session.signals();

    session.mark_saved();

    assert_eq!(session.signals(), signals + 1);
    let echo = session.echo().expect("it says so");
    assert!(echo.contains("write-file"), "{echo}");
    assert!(echo.contains("cannot save from a hook"), "{echo}");
}

#[test]
fn a_question_refused_from_a_hook_leaves_nothing_for_the_host_to_answer() {
    // A refused command may already have written its question down, and one
    // nobody collects is answered by whichever key asks next -- about a buffer
    // three commands ago. `search-symbol` wrote one and nothing took it back
    // until #149 added `find-file-under-here`, which writes one too.
    for command in ["find-file-under-here", "search-symbol"] {
        let mut session = editing("alpha beta\n");
        session.set_path("notes.txt");
        session
            .apply(hook_set(meowr_command::HookPoint::AfterSave, command))
            .expect("resolves");

        session.mark_saved();

        let echo = session.echo().expect("it says so").to_string();
        assert!(echo.contains("from a hook"), "{command}: {echo}");
        assert_eq!(session.pending_files(), None, "{command}");
        assert_eq!(session.pending_search(), None, "{command}");
    }
}

#[test]
fn a_command_run_by_a_hook_is_not_what_repeat_repeats() {
    // The flat rule of #64 is about what a hook-run command *sees*, and
    // `last_command` is the other half of it. `repeat` reads that field, so a
    // hook writing it would make `'` repeat the hook's command rather than
    // the one the user actually ran — a key silently doing the wrong thing,
    // which is worse than a key doing nothing.
    let mut session = editing("alpha beta gamma\n");
    session.set_path("notes.txt");
    session
        .apply(hook_set(meowr_command::HookPoint::AfterSave, "write-file"))
        .expect("resolves");

    press(&mut session, "w"); // the user's command: select a word
    session.mark_saved(); // fires the hook, which runs `write-file`

    // `write-file` answers `Outcome::Save`, so if `repeat` had been left
    // pointing at it this asks the host to write. The user's own last command
    // was a motion, which cannot.
    assert_ne!(
        session.on_key(Key::Char('\'')),
        Action::Save,
        "repeat should redo the user's motion, not the hook's write-file"
    );
}

#[test]
fn a_prefix_argument_survives_a_hook_running_a_command() {
    // `dispatch_command` ends with `self.argument = next`, so a command run by
    // a hook consumes the argument the *user* was building. `run_hooks` says
    // in its own doc that "no argument is being consumed"; the command it
    // dispatches must not make that false.
    let mut session = editing("alpha beta gamma delta epsilon\n");
    session.set_path("notes.txt");
    session
        .apply(hook_set(meowr_command::HookPoint::AfterSave, "write-file"))
        .expect("resolves");

    press(&mut session, "3"); // a prefix argument, mid-build
    session.mark_saved(); // the hook runs a command, which must not eat it
    press(&mut session, "w"); // three words, not one

    assert_eq!(session.selection().range().len(), "alpha beta gamma".len());
}

#[test]
fn a_config_can_remove_a_hook_meowr_shipped() {
    // #48's own example: `(remove-hook 'before-save 'auto-exit-insert-on-save)`.
    // That hook is observable without a clock — `write-file` answers
    // `Outcome::Save`, which fires `before-save`, where it drops out of Insert.
    // Nothing in the Insert table saves — `C-s` is Normal's — so the config
    // binds one, which is the seam being exercised anyway.
    fn save_key_in_insert() -> Declarations {
        let mut set = Declarations::default();
        set.push(Declaration::Bind {
            state: State::Insert,
            keys: vec![Key::Ctrl('s')],
            command: "write-file".into(),
        });
        set
    }

    let mut session = editing("alpha\n");
    session.set_path("notes.txt");
    session.apply(save_key_in_insert()).expect("resolves");
    press(&mut session, "s"); // Insert State
    session.on_key(Key::Ctrl('s'));
    assert_eq!(
        session.state(),
        State::Normal,
        "the shipped hook leaves Insert on a save"
    );

    let mut session = editing("alpha\n");
    session.set_path("notes.txt");
    let mut set = save_key_in_insert();
    set.push(Declaration::Unhook {
        point: meowr_command::HookPoint::BeforeSave,
        name: "auto-exit-insert-on-save".into(),
    });
    session.apply(set).expect("resolves");

    press(&mut session, "s");
    session.on_key(Key::Ctrl('s'));

    assert_eq!(
        session.state(),
        State::Insert,
        "with the hook removed, saving no longer leaves Insert"
    );
}

#[test]
fn removing_a_hook_that_is_not_there_is_refused_at_load() {
    // A config removing something that does not exist is a typo, and #59's
    // atomicity says a typo stops the whole set rather than being dropped.
    let mut session = editing("alpha\n");
    let mut set = Declarations::default();
    set.push(Declaration::Unhook {
        point: meowr_command::HookPoint::BeforeSave,
        name: "auto-exit-insert-on-sav".into(),
    });

    let problems = session.apply(set).expect_err("no such hook");

    assert!(
        problems
            .iter()
            .any(|p| p.to_string().contains("auto-exit-insert-on-sav")),
        "{problems:?}"
    );
}

#[test]
fn the_arrow_keys_move_the_picker_like_ctrl_n_and_ctrl_p() {
    // consult and vertico both take the arrows in the minibuffer, and until
    // #71 meowr had no `Key` for them — so the widget offered `C-n` and `C-p`
    // and nothing else, which is a workaround wearing the clothes of a
    // decision. Aliases, not replacements: `C-n` is what the author's fingers
    // know and it keeps working.
    let (mut session, _) = picker_over_four_lines();

    session.on_key(Key::Down);
    let after_down = session.preview_target();
    assert_eq!(
        after_down,
        Some(Preview::File("/tmp/notes.rs".into())),
        "Down moved to row 1, the same row C-n reaches"
    );

    session.on_key(Key::Up);

    assert_ne!(session.preview_target(), after_down, "Up moved back");
}

#[test]
fn c_x_b_offers_the_buffers_rather_than_stepping_to_the_next_one() {
    // `C-x b` is `switch-to-buffer` in Emacs and `consult-buffer` in the
    // author's Doom; `next-buffer` is `C-x <right>`. meowr had the stepping
    // command on the choosing key, which is not a near miss — it is a
    // different operation, and the one key on this surface that #103 found
    // pointing somewhere its finger does not expect.
    let mut session = editing("alpha\n");
    session.on_key(Key::Ctrl('x'));
    session.on_key(Key::Char('b'));

    let widget = session.narrowing().expect("`C-x b` opens the picker");
    assert!(
        widget.matches().iter().count() >= 1,
        "the buffers are offered"
    );
}

// --- #89: a composition runs behind the session's guards --------------------

/// A composition of `steps`, bound to `M-k`.
fn composed(steps: &[&str]) -> Declarations {
    let mut set = Declarations::default();
    set.push(Declaration::Command {
        name: "composed".into(),
        description: "A composition".into(),
        body: steps.iter().map(|s| (*s).to_string()).collect(),
    });
    set.push(Declaration::Bind {
        state: State::Normal,
        keys: vec![Key::Alt('k')],
        command: "composed".into(),
    });
    set
}

#[test]
fn a_composition_cannot_edit_a_read_only_buffer() {
    // `dispatch_command` holds the guards and says why: "The guards live here
    // rather than in each command, so a command cannot forget them." A
    // composition forgot them all, because the session dispatched the
    // composition's own name once and the registry then ran every step
    // underneath it — so a config could edit a buffer the user had marked
    // read-only, and nothing was said.
    let mut session = editing("alpha beta\n");
    press(&mut session, "w");
    session.set_read_only(true);
    session.apply(composed(&["kill"])).expect("a composition");

    session.on_key(Key::Alt('k'));

    assert_eq!(session.contents().unwrap(), "alpha beta\n", "untouched");
    let echo = session.echo().expect("and it says why");
    assert!(echo.contains("read-only"), "{echo}");
}

#[test]
fn a_composed_step_with_no_selection_follows_the_fallback_table() {
    // The other guard, from the other side. #88 patched this one *inside*
    // `surround-*`, which is a guard in a command — the thing the comment
    // above says must not happen. With the steps dispatched, `kill` with no
    // selection reaches `kill-line` through the table, exactly as the key
    // does.
    let mut session = editing("alpha beta\ngamma\n");
    session.apply(composed(&["kill"])).expect("a composition");

    session.on_key(Key::Alt('k'));

    assert_eq!(session.contents().unwrap(), "\ngamma\n", "kill-line ran");
}

#[test]
fn repeat_after_a_composition_repeats_the_composition_not_its_last_step() {
    // `dispatch_command` writes `last_command`, so dispatching the steps
    // through it would leave the *step* as the thing `'` repeats — the bug
    // `dispatch_hook_commands` already saves and restores around.
    // Two steps, so the two readings differ: repeating the composition moves
    // four words and repeating only its last step moves three.
    const TEXT: &str = "alpha beta gamma delta epsilon\n";
    let four = {
        let mut session = editing(TEXT);
        press(&mut session, "wwww");
        session.selection().range()
    };

    let mut session = editing(TEXT);
    session
        .apply(composed(&["next-word", "next-word"]))
        .expect("a composition");

    session.on_key(Key::Alt('k'));
    press(&mut session, "'");

    assert_eq!(session.selection().range(), four, "not three");
}

#[test]
fn a_composition_that_names_itself_is_refused_when_it_is_declared() {
    // #89 listed nesting as a thing to decide when the steps move behind the
    // guards. It was decided already, and earlier: the cycle is caught when
    // the declaration set is applied, so no depth counter is needed at
    // dispatch and a config cannot get a recursive command registered at all.
    let mut session = editing("alpha\n");

    let problems = session
        .apply(composed(&["composed"]))
        .expect_err("a cycle is refused");

    assert!(
        format!("{problems:?}").contains("calls itself"),
        "{problems:?}"
    );
}

#[test]
fn a_composition_runs_its_steps_in_order() {
    // Order is the whole of what a composition adds. Reversed, `kill` would
    // find no selection and fall through the table to `kill-line`, which is a
    // different edit — so this fails loudly rather than subtly if the loop
    // ever runs backwards.
    let mut session = editing("alpha beta\n");
    session
        .apply(composed(&["next-word", "kill"]))
        .expect("a composition");

    session.on_key(Key::Alt('k'));

    assert_eq!(session.contents().unwrap(), " beta\n");
}

#[test]
fn a_step_that_is_refused_stops_the_composition() {
    // Emacs aborts a keyboard macro on a signal. Running on would let
    // `(kill-line quit)` quit after the kill did not happen.
    let mut session = editing("alpha beta\n");
    session
        .apply(composed(&["undo", "next-word"]))
        .expect("a composition");

    session.on_key(Key::Alt('k'));

    assert_eq!(
        session.selection().range(),
        0..0,
        "`undo` had nothing to undo and signalled, so `next-word` never ran"
    );
}

#[test]
fn a_composition_may_name_another_composition() {
    let mut session = editing("alpha beta gamma\n");
    let mut set = composed(&["inner", "next-word"]);
    set.push(Declaration::Command {
        name: "inner".into(),
        description: "Inner".into(),
        body: vec!["next-word".into()],
    });
    session.apply(set).expect("a nested composition");

    session.on_key(Key::Alt('k'));

    let two_words = {
        let mut plain = editing("alpha beta gamma\n");
        press(&mut plain, "ww");
        plain.selection().range()
    };
    assert_eq!(session.selection().range(), two_words);
}

#[test]
fn a_composition_naming_a_command_nobody_registered_is_refused_when_applied() {
    // #59's resolve pass catches this when the set is applied, which is why
    // `run_composed`'s check for it is a backstop rather than the guard: with
    // no way to unregister a command there is no route to a step that resolved
    // once and later did not. The backstop stays because the alternative is a
    // silent skip — an unregistered name dispatches to `Action::Nothing` and
    // says nothing at all.
    let mut session = editing("alpha\n");

    let problems = session
        .apply(composed(&["no-such-command"]))
        .expect_err("an unknown step is refused");

    assert!(
        format!("{problems:?}").contains("no-such-command"),
        "it names the step: {problems:?}"
    );
}

#[test]
fn no_composition_over_any_command_can_edit_a_read_only_buffer() {
    // #88's sweep, from the other side. That one pressed every command with no
    // key and looked for a wordless beep; this one puts every command inside a
    // composition, runs it in a buffer the user marked read-only, and looks
    // for an edit. The bug #89 found was not specific to `kill` — every
    // mutating command was reachable that way — so the test is not specific to
    // one either.
    //
    // Names only: a composition is a list of names, so this is exactly the
    // shape a config can write.
    let names: Vec<String> = meowr_app::default_registry()
        .names()
        .map(str::to_owned)
        .collect();
    assert!(
        names.len() > 100,
        "the sweep must reach the table, and reached {}",
        names.len()
    );

    const TEXT: &str = "alpha beta\ngamma\n";
    let mut edited: Vec<String> = Vec::new();
    for name in &names {
        // Fresh every time: the composition before this one may have edited.
        let mut session = editing(TEXT);
        press(&mut session, "w");
        session.set_read_only(true);
        let mut set = Declarations::default();
        set.push(Declaration::Command {
            name: "swept".into(),
            description: "One step".into(),
            body: vec![name.clone()],
        });
        set.push(Declaration::Bind {
            state: State::Normal,
            keys: vec![Key::Alt('k')],
            command: "swept".into(),
        });
        if session.apply(set).is_err() {
            continue;
        }

        let id = session.current_buffer_id();

        session.on_key(Key::Alt('k'));

        // That buffer, not whichever one is current afterwards:
        // `describe-bindings` opens its own, and reading `contents()` would
        // call that an edit. A *killed* buffer is not an edited one either —
        // read-only refuses edits, and closing a read-only buffer is
        // something `SPC b k` is allowed to do to a help buffer.
        let buffer = &session.buffers()[id.0];
        let after = buffer.document().map(|doc| doc.text().to_string());
        if !buffer.is_killed() && after.is_some_and(|text| text != TEXT) {
            edited.push(name.clone());
        }
    }
    edited.sort();

    assert!(
        edited.is_empty(),
        "these edit a read-only buffer through a composition: {edited:#?}"
    );
}

#[test]
fn before_command_fires_once_per_step_of_a_composition() {
    // Recorded as a decision on #89 and proved here rather than asserted in a
    // comment. A step is an ordinary dispatch now, so it reaches the hook
    // machinery that `run_composition` never did — which is the same answer a
    // keyboard macro gives, and the alternative is a composition whose steps
    // skip `before-save` and quietly stop being saves.
    //
    // The composition itself does not fire: it has no behaviour of its own,
    // it *is* its steps. Firing for both would run every hook N+1 times for N
    // steps, so a hook that counts commands would double-count. This is the
    // other way round from before #89, when the composition fired once and
    // none of its steps did.
    use std::cell::Cell;
    use std::rc::Rc;

    let count = Rc::new(Cell::new(0));
    let seen = Rc::clone(&count);
    let mut session = editing("alpha beta gamma\n");
    session.add_hook("count-commands", HookPoint::BeforeCommand, move |_| {
        seen.set(seen.get() + 1);
        Proceed::Yes
    });
    session
        .apply(composed(&["next-word", "next-word"]))
        .expect("a composition");

    session.on_key(Key::Alt('k'));

    assert_eq!(
        count.get(),
        2,
        "once per step, and not again for the composition around them"
    );
}

// --- #87: the names a config can use ----------------------------------------

/// Run `describe-commands`, which ships with no key of its own.
fn describe_commands(session: &mut Session) {
    let mut set = Declarations::default();
    set.push(Declaration::Bind {
        state: State::Normal,
        keys: vec![Key::Alt('d')],
        command: "describe-commands".into(),
    });
    session.apply(set).expect("`describe-commands` must exist");
    session.on_key(Key::Alt('d'));
}

/// The text of the `*commands*` buffer.
fn commands_buffer(session: &Session) -> String {
    session
        .buffers()
        .iter()
        .find(|b| !b.is_killed() && b.name() == meowr_app::COMMANDS)
        .and_then(|b| b.document())
        .map(|doc| doc.text().to_string())
        .expect("`*commands*` is open")
}

#[test]
fn describe_commands_lists_every_name_the_registry_has() {
    // `SPC ?` answers "what do my keys do". This answers "what can I put in
    // `init.scm`", which is a different question with a different answer: a
    // command with no key at all is invisible to the first and is the whole
    // point of the second.
    let mut session = editing("alpha\n");

    describe_commands(&mut session);

    let text = commands_buffer(&session);
    for name in meowr_app::default_registry().names() {
        assert!(text.contains(name), "`{name}` is missing");
    }
}

#[test]
fn a_command_with_no_key_is_listed_with_its_name_and_what_it_does() {
    // The seventy-six per-Thing names exist so that they can be named (#78),
    // and until now the only way to learn one was to read the source.
    let mut session = editing("alpha\n");

    describe_commands(&mut session);

    let text = commands_buffer(&session);
    let row = text
        .lines()
        .find(|line| line.contains("inner-of-thing/round"))
        .expect("the Thing names are listed");
    assert!(row.contains("Select inside the round"), "{row}");
}

#[test]
fn a_command_with_a_key_says_which_key() {
    // Both halves on one row: the name is what a config writes, and the key is
    // how you recognise the command you already know.
    let mut session = editing("alpha\n");

    describe_commands(&mut session);

    // `redo` on `C-r`, not `undo` on `u`: the row for `undo` contains a `u`
    // in its own name, so asserting on that passes with the whole keys column
    // deleted — which is what it did until this comment was written.
    let text = commands_buffer(&session);
    let row = text
        .lines()
        .find(|line| line.starts_with("  redo "))
        .expect("`redo` is listed");
    assert!(row.contains("C-r"), "it names the key: {row}");
}

#[test]
fn the_collapsed_thing_row_says_where_the_names_are() {
    // The row `SPC ?` already had was misleading: nineteen Things fold into
    // `, …` under one description that does not say which Thing, so the buffer
    // that answers "which" has to be named from there.
    let mut session = editing("alpha\n");

    press(&mut session, " ?");

    let text = session
        .buffers()
        .iter()
        .find(|b| !b.is_killed() && b.name() == meowr_app::BINDINGS)
        .and_then(|b| b.document())
        .map(|doc| doc.text().to_string())
        .expect("`*bindings*` is open");
    assert!(
        text.contains(meowr_app::COMMANDS),
        "it points at the names: {text}"
    );
}

#[test]
fn spc_f_y_puts_the_buffer_s_path_in_the_kill_ring() {
    // Doom's `+default/yank-buffer-path`. The one row of its file prefix meowr
    // can answer today (#98): the rest wants a project, a `~/.emacs.d`, sudo,
    // or file operations nobody has scheduled.
    let mut session = editing("alpha\n");
    session.set_path("notes/todo.md");

    press(&mut session, " fy");
    press(&mut session, "p");

    assert!(
        session.contents().unwrap().contains("notes/todo.md"),
        "{:?}",
        session.contents()
    );
}

// --- the two `SPC f` rows that need the Project's root (#148) ---------------

#[test]
fn spc_f_capital_y_asks_the_host_for_the_path_from_the_project() {
    // `+default/yank-buffer-path-relative-to-project`. The session spells
    // paths against a working directory it never sees, so "relative to the
    // root" is arithmetic only the host can do.
    use meowr_app::FromRoot;
    let mut session = editing_a_file("fn main() {}\n");
    press(&mut session, " f");
    assert_eq!(session.on_key(Key::Char('Y')), Action::FromRoot);
    assert_eq!(
        session.pending_from_root(),
        Some(FromRoot::Yank {
            about: "main.rs".into()
        })
    );
    assert_eq!(session.pending_from_root(), None, "drained, not read twice");
}

#[test]
fn the_path_from_the_project_is_yanked_and_said() {
    // `+default/yank-buffer-path` says "Copied path: %s", which is also what
    // makes its fallback outside a project -- the absolute path -- a visible
    // answer rather than ADR-0010's silent one.
    let mut session = editing_a_file("fn main() {}\n");
    press(&mut session, " fY");
    session.pending_from_root();

    assert_eq!(
        session.path_from_root("main.rs", "crates/meowr/src/main.rs".into()),
        Action::Redraw
    );
    assert_eq!(session.echo(), Some("Copied path: crates/meowr/src/main.rs"));
    press(&mut session, "p");
    assert!(
        session.contents().unwrap().contains("crates/meowr/src/main.rs"),
        "{:?}",
        session.contents()
    );
}

#[test]
fn spc_f_capital_r_asks_with_the_recent_files_to_filter() {
    use meowr_app::FromRoot;
    let mut session = editing_a_file("fn main() {}\n");
    session.set_recent_files(vec!["src/lib.rs".into(), "/elsewhere/x.rs".into()]);
    press(&mut session, " fR");
    assert_eq!(
        session.pending_from_root(),
        Some(FromRoot::Recent {
            about: "main.rs".into(),
            paths: vec!["src/lib.rs".into(), "/elsewhere/x.rs".into()],
        })
    );
}

#[test]
fn the_recent_files_in_the_project_are_a_picker_of_files() {
    let mut session = editing_a_file("fn main() {}\n");
    press(&mut session, " fR");
    session.pending_from_root();

    let answer = Ok(vec!["src/lib.rs".to_string(), "Cargo.toml".to_string()]);
    assert_eq!(session.recent_from_root("main.rs", answer), Action::Redraw);
    let widget = session.narrowing().expect("the picker opened");
    assert!(widget.sources().is_empty(), "one question's answer");
    assert_eq!(widget.matches().len(), 2);
    assert_eq!(session.accept_narrowing(), Action::Open);
    assert_eq!(session.pending_open(), Some("src/lib.rs"));
}

#[test]
fn no_recent_files_in_the_project_and_no_project_are_both_said() {
    let mut session = editing_a_file("fn main() {}\n");
    press(&mut session, " fR");
    session.pending_from_root();
    session.recent_from_root("main.rs", Ok(Vec::new()));
    assert_eq!(session.echo(), Some("No recent files in this project."));
    assert!(session.narrowing().is_none());

    session.recent_from_root("main.rs", Err("/tmp is in no git repository".into()));
    assert_eq!(session.echo(), Some("/tmp is in no git repository"));
}

#[test]
fn with_project_scope_off_an_empty_answer_claims_no_project() {
    // The buffer's directory stands in for the Project when the concept is
    // off, so "in this project" would name a thing that is not there.
    let mut session = editing_a_file("fn main() {}\n");
    let mut options = session.options();
    options.project_scope = false;
    session.set_options(options);
    press(&mut session, " fR");
    session.pending_from_root();
    session.recent_from_root("main.rs", Ok(Vec::new()));
    assert_eq!(session.echo(), Some("No recent files under this directory."));
}

#[test]
fn a_rooted_answer_about_another_buffer_is_dropped() {
    let mut session = editing_a_file("fn main() {}\n");
    assert_eq!(
        session.path_from_root("elsewhere.rs", "x.rs".into()),
        Action::Nothing
    );
    assert_eq!(session.echo(), None);
    assert_eq!(
        session.recent_from_root("elsewhere.rs", Ok(vec!["x.rs".into()])),
        Action::Nothing
    );
    assert!(session.narrowing().is_none());
}

#[test]
fn both_rows_refuse_in_a_buffer_with_no_file() {
    for keys in [" fY", " fR"] {
        let mut session = editing("scratch\n");
        press(&mut session, keys);
        assert_eq!(session.pending_from_root(), None, "{keys}");
        assert_eq!(session.echo(), Some("This buffer has no file."), "{keys}");
    }
}

#[test]
fn a_buffer_with_no_file_has_no_path_to_yank_and_says_so() {
    let mut session = editing("alpha\n");

    press(&mut session, " fy");

    let echo = session.echo().expect("it says why");
    assert!(echo.contains("no file"), "{echo}");
    assert_eq!(session.contents().unwrap(), "alpha\n");
}

#[test]
fn spc_b_dot_and_spc_b_d_reach_the_buffer_commands_doom_puts_there() {
    // Two of the six open rows under Doom's buffer prefix that meowr can
    // answer with commands it already has (#101). `b .` is the picker, which
    // `SPC .` also opens — Doom binds both, and Ring 3 asks for both. `b d` is
    // `kill-current-buffer`, which is what meowr's `kill-buffer` does.
    let mut session = editing("alpha\n");

    press(&mut session, " b.");

    assert!(session.narrowing().is_some(), "`SPC b .` opens the picker");
    session.on_key(Key::Esc);

    // A second buffer, because killing the only one leaves a scratch behind
    // and the count would not move.
    let mut second = Buffer::scratch();
    second.loaded(Document::from_text("second\n"));
    let id = session.open(second);
    let before = session.buffers().iter().filter(|b| !b.is_killed()).count();

    press(&mut session, " bd");

    let after = session.buffers().iter().filter(|b| !b.is_killed()).count();
    assert_eq!(after, before - 1, "`SPC b d` killed it");
    assert!(
        session.buffers()[id.0].is_killed(),
        "the one that was current"
    );
}

// --- source-narrowed pickers (#75) -----------------------------------------

/// A session with something in every picker source, so "narrowed to one" is a
/// claim that can fail.
fn stocked() -> Session {
    let mut session = editing("alpha\nbeta\ngamma\n");
    session.set_path("notes.txt");
    session.set_recent_files(vec!["/tmp/one.rs".into(), "/tmp/two.rs".into()]);
    session.set_project(None, Ok(vec!["/src/project.rs".into()]));
    session
}

#[test]
fn spc_dot_opens_the_picker_showing_open_buffers_only() {
    // Doom's `SPC .` is `switch-to-buffer`, and meowr's picker is one widget
    // over four sources — so the faithful binding is the picker already
    // narrowed, not the picker with everything in it.
    let mut session = stocked();

    press(&mut session, " .");

    let widget = session.narrowing().expect("the picker opened");
    assert_eq!(widget.narrowed().map(|s| s.name.as_str()), Some("buffers"));
    assert!(
        widget.matches().len() == 1,
        "just the one open buffer, not the recent files or the lines"
    );
}

#[test]
fn spc_spc_opens_the_picker_showing_project_files_only() {
    // Doom's `SPC SPC` is `projectile-find-file`, narrowed the way `SPC .` is
    // (#147).
    let mut session = stocked();
    session.set_project(
        Some("notes.txt".into()),
        Ok(vec!["src/lib.rs".into(), "Cargo.toml".into()]),
    );

    press(&mut session, " ");
    session.on_key(Key::Char(' '));

    let widget = session.narrowing().expect("the picker opened");
    assert_eq!(widget.narrowed().map(|s| s.name.as_str()), Some("project"));
    assert_eq!(widget.matches().len(), 2, "the two files in the Project");
}

#[test]
fn spc_spc_asks_for_the_project_it_is_about_to_show() {
    let mut session = stocked();
    press(&mut session, " ");
    assert_eq!(session.on_key(Key::Char(' ')), Action::Project);
    assert_eq!(session.pending_project(), Some(Some("notes.txt".into())));
}

// --- recent files are kept (#152) ------------------------------------------

fn recent(session: &Session) -> Vec<String> {
    session.recent_file_paths().map(String::from).collect()
}

#[test]
fn the_file_named_at_launch_is_a_recent_file_once_it_loads() {
    // `recentf-mode` adds a file on `find-file-hook`, which is the visit.
    let mut session = editing("");
    session.set_path("main.rs");
    session.loaded(meowr_domain::Document::from_text("fn main() {}\n"));
    assert_eq!(recent(&session), ["main.rs"]);
}

#[test]
fn a_file_opened_from_the_picker_goes_to_the_top_of_the_recent_files() {
    let mut session = editing_a_file("fn main() {}\n");
    session.set_recent_files(vec!["older.rs".into()]);
    session.set_project(Some("main.rs".into()), Ok(vec!["src/lib.rs".into()]));
    press(&mut session, "  ");
    assert_eq!(session.accept_narrowing(), Action::Open);
    session.opened("src/lib.rs", Opened::Read("pub fn f() {}\n".into()));
    assert_eq!(recent(&session), ["src/lib.rs", "older.rs"]);
}

#[test]
fn a_file_that_would_not_open_is_not_a_recent_file() {
    let mut session = editing_a_file("fn main() {}\n");
    session.set_project(Some("main.rs".into()), Ok(vec!["gone.rs".into()]));
    press(&mut session, "  ");
    session.accept_narrowing();
    session.opened("gone.rs", Opened::Failed("No such file: gone.rs".into()));
    assert!(recent(&session).iter().all(|p| p != "gone.rs"));
}

#[test]
fn writing_a_file_and_switching_a_window_to_it_bump_it_to_the_top() {
    // Doom's `doom--recentf-touch-buffer-h`, on `write-file-functions` and
    // `doom-switch-window-hook`: frecency rather than first-visit order.
    let mut session = editing_a_file("fn main() {}\n");
    session.set_recent_files(vec!["other.rs".into(), "main.rs".into()]);
    session.mark_saved();
    assert_eq!(recent(&session), ["main.rs", "other.rs"], "a write bumps");

    session.set_recent_files(vec!["other.rs".into(), "main.rs".into()]);
    press(&mut session, " ws");
    press(&mut session, " ww");
    assert_eq!(recent(&session), ["main.rs", "other.rs"], "a window switch bumps");
}

#[test]
fn closing_a_window_bumps_the_file_the_focus_lands_on() {
    // `doom-switch-window-hook` runs on any change of the selected window, not
    // only on `other-window` -- closing one moves the focus just as surely.
    let mut session = editing_a_file("fn main() {}\n");
    session.set_project(Some("main.rs".into()), Ok(vec!["src/lib.rs".into()]));
    press(&mut session, " ws");
    press(&mut session, "  ");
    session.accept_narrowing();
    session.opened("src/lib.rs", Opened::Read("pub fn f() {}\n".into()));
    assert_eq!(recent(&session)[0], "src/lib.rs");

    press(&mut session, " wd");
    assert_eq!(session.path(), Some("main.rs"), "the focus landed on main.rs");
    assert_eq!(recent(&session)[0], "main.rs", "and that bumped it");
}

#[test]
fn a_file_under_the_runtime_directory_is_never_a_recent_file() {
    // Doom's one `recentf-exclude`, which `recentf-add-file` checks as a file is
    // added -- so it is not in `SPC RET` for the rest of the session either,
    // not merely left out of what is written on the way out.
    let mut session = editing("");
    session.exclude_from_recent("/run/user/1000");
    session.set_path("/run/user/1000/scratch.txt");
    session.loaded(meowr_domain::Document::from_text("x\n"));
    assert!(recent(&session).is_empty());
}

#[test]
fn spc_return_with_no_recent_files_says_so() {
    // `consult-recent-file` answers an empty list with "No recent files"
    // rather than a picker with nothing in it -- the silence that reads as a
    // key that did nothing, which is what `SPC RET` was from M1 to #152.
    let mut session = editing("scratch\n");
    session.on_key(meowr_app::leader::KEY);
    session.on_key(Key::Enter);
    assert_eq!(
        session.narrow_view().expect("open").note.as_deref(),
        Some("No recent files.")
    );
}

#[test]
fn spc_return_opens_the_picker_showing_recent_files_only() {
    let mut session = stocked();

    session.on_key(meowr_app::leader::KEY);
    session.on_key(Key::Enter);

    let widget = session.narrowing().expect("the picker opened");
    assert_eq!(widget.narrowed().map(|s| s.name.as_str()), Some("recent"));
    assert_eq!(widget.matches().len(), 2, "the two recent files");
}

#[test]
fn a_source_narrowed_picker_can_still_be_renarrowed() {
    // `<` is consult's key and it has to keep working from here, or these two
    // bindings would be a dead end rather than a shortcut.
    let mut session = stocked();
    press(&mut session, " .");

    session.on_key(Key::Char('<'));
    session.on_key(Key::Char('l'));

    let widget = session.narrowing().unwrap();
    assert_eq!(widget.narrowed().map(|s| s.name.as_str()), Some("line"));
    assert_eq!(widget.matches().len(), 3, "the three lines");
}

#[test]
fn find_file_is_still_the_key_the_echo_area_names() {
    // The regression that reverted the first attempt: three keys on one
    // command made `keys_for` advertise `SPC .` as the way to `*config-error*`.
    // Separate commands are what keep that answer stable.
    let mut session = editing("alpha\n");
    session.config_failed("init.scm:1: broken", "nothing is called `nope`");

    let echo = session.echo().expect("it says where the rest is");

    assert!(echo.contains("SPC f f"), "{echo}");
}

#[test]
#[should_panic(expected = "no picker source is on")]
fn opening_the_picker_on_a_source_that_does_not_exist_is_caught_in_debug() {
    // The failure this guards is quiet rather than loud: `narrow_to` answers
    // `false` and the picker opens showing everything, so `SPC .` would become
    // a second `SPC f f` and nothing would say so.
    let mut session = editing("alpha\n");

    session.open_picker_on('z');
}

// --- #79: the keymap, inside the editor -----------------------------------

#[test]
fn the_help_key_opens_a_buffer_of_bindings() {
    // The gap #79 names: the leader menu is a view of the walk you are in the
    // middle of, and Ring 1 is not under `SPC` at all — so nothing on screen
    // says what a single key does.
    let mut session = editing("alpha\n");

    press(&mut session, " ?");

    assert!(
        session
            .modeline_of(session.focus())
            .text()
            .contains("*bindings*"),
        "{}",
        session.modeline_of(session.focus()).text()
    );
}

fn described(session: &Session) -> String {
    session
        .document()
        .expect("the bindings buffer is loaded")
        .text()
        .to_string()
}

#[test]
fn every_binding_is_listed_with_the_registry_s_own_description() {
    // Its own, not a second copy beside it: `leader.rs` says why, and the
    // leader menu already reads descriptions from the registry rather than
    // from a table next to the keys.
    let mut session = editing("alpha\n");

    press(&mut session, " ?");

    let text = described(&session);
    assert!(text.contains("NORMAL"), "{text}");
    assert!(
        text.contains("Select to the end of the next word"),
        "the description `next-word` was registered with\n{text}"
    );
}

#[test]
fn every_state_the_keymap_drives_gets_a_section_of_its_own() {
    let mut session = editing("alpha\n");

    press(&mut session, " ?");

    let text = described(&session);
    for section in ["NORMAL", "INSERT", "BEACON"] {
        assert!(text.contains(section), "no {section} section in\n{text}");
    }
    assert!(text.contains("Leave Insert State"), "{text}");
}

#[test]
fn the_leader_tree_is_listed_too() {
    // Ring 2 is where a config puts things and where half the editor lives.
    // A bindings buffer that stopped at the keymap would omit `find-file`.
    let mut session = editing("alpha\n");

    press(&mut session, " ?");

    let text = described(&session);
    assert!(text.contains("LEADER"), "{text}");
    assert!(text.contains("SPC f f"), "{text}");
}

#[test]
fn a_state_the_keymap_does_not_drive_says_so_rather_than_going_missing() {
    // The half of #79 that is about a State the keymap does not drive: "why
    // did that key do nothing" needs an answer, and a bindings buffer that
    // simply omitted Motion would read as though the question had none. That
    // is truer since Guests were dropped, not less: nothing enters Motion
    // State at all now, and a State meowr names and never enters is exactly
    // the kind of silence this was written against. Keypad is the same shape
    // for a different reason — its keys walk the leader tree.
    let mut session = editing("alpha\n");

    press(&mut session, " ?");

    let text = described(&session);
    assert!(text.contains("MOTION"), "{text}");
    assert!(
        text.contains("Nothing enters this State today"),
        "Motion's note says why it is empty\n{text}"
    );
    assert!(text.contains("KEYPAD"), "{text}");
}

#[test]
fn asking_twice_refreshes_the_one_buffer_rather_than_making_another() {
    // `*scratch*` is a name, not a heap of them. A generated buffer that piles
    // up is worse than one that does not exist: the picker fills with copies
    // and the newest is indistinguishable from the stalest.
    let mut session = editing("alpha\n");

    press(&mut session, " ?");
    press(&mut session, " ?");

    let named = session
        .buffers()
        .iter()
        .filter(|buffer| buffer.name() == meowr_app::BINDINGS)
        .count();
    assert_eq!(named, 1, "one buffer, refreshed");
}

#[test]
fn it_describes_the_keymap_a_config_left_rather_than_the_one_meowr_ships() {
    // The whole point of #79: a config that moves the nav cluster leaves
    // nothing on screen agreeing with your fingers. A bindings buffer built
    // from `default_keymap` would be the same lie in a new place.
    let mut session = editing("alpha\n");
    let mut set = Declarations::default();
    set.push(Declaration::Bind {
        state: State::Normal,
        keys: leader_keys("m"),
        command: "left".into(),
    });
    session.apply(set).expect("resolves as a whole");

    press(&mut session, " ?");

    let text = described(&session);
    let row = text
        .lines()
        .find(|line| line.trim_start().starts_with("m "))
        .unwrap_or_else(|| panic!("no row for `m` in\n{text}"));
    assert!(row.contains("Move left"), "{row}");
}

#[test]
fn the_bindings_buffer_is_read_only() {
    let mut session = editing("alpha\n");

    press(&mut session, " ?");

    assert!(
        session.modeline_of(session.focus()).text().contains("%%"),
        "{}",
        session.modeline_of(session.focus()).text()
    );
}

#[test]
fn a_run_of_keys_sharing_one_command_collapses_to_a_range() {
    // Eighteen Things behind each of four prefixes, plus ten digits: 82 of the
    // Normal rows repeat one of five descriptions, and the description does
    // not say which Thing or which count. A cheatsheet nobody can scan is not
    // a cheatsheet.
    let mut session = editing("alpha\n");

    press(&mut session, " ?");

    let text = described(&session);
    assert!(text.contains("0 … 9"), "the digits, as a range\n{text}");
    assert_eq!(
        text.matches("Expand the selection by a count").count(),
        1,
        "one row for the digits, not ten\n{text}"
    );
    assert_eq!(
        text.matches("Select inside a thing").count(),
        1,
        "one row for the Things, not nineteen\n{text}"
    );
}

#[test]
fn beacon_says_that_normal_s_bindings_still_apply() {
    // `on_key` resolves Beacon and then falls back to Normal — "the Beacon
    // keymap inherits Normal". Two rows under BEACON with nothing said about
    // the inheritance reads as though the other fifty keys were dead there,
    // which is the same lie by omission the Motion note exists to prevent.
    let mut session = editing("alpha\n");

    press(&mut session, " ?");

    let text = described(&session);
    let beacon = text.split("BEACON").nth(1).expect("a BEACON section");
    assert!(
        beacon.contains("Normal"),
        "BEACON says nothing about what it inherits\n{beacon}"
    );
}

#[test]
fn the_refusal_names_the_save_key_a_config_left_not_the_one_it_took_away() {
    // Found by running one: an `init.scm` that drops `C-x C-s` in favour of
    // the leader had not lost `write-file`, but the refusal asked only the
    // keymap — so the advice went silently missing while `SPC f s` worked.
    let mut session = editing("alpha\n");
    session.set_path("notes.rs");
    let mut set = Declarations::default();
    set.push(Declaration::Unbind {
        state: State::Normal,
        keys: leader_keys("C-x C-s"),
    });
    session.apply(set).expect("resolves as a whole");
    press(&mut session, "sZ");
    session.on_key(Key::Esc);

    session.on_key(Key::Ctrl('c'));

    let echo = session.echo().expect("it refuses");
    assert!(echo.contains("Save with SPC"), "{echo}");
}

#[test]
fn a_state_note_survives_the_state_gaining_a_binding() {
    // The note was reached only when the list was empty, so one Motion binding
    // in an `init.scm` deleted the ADR-0005 line — which is the whole answer
    // to "why did that key do nothing", removed by the config most likely to
    // provoke the question.
    let mut session = editing("alpha\n");
    let mut set = Declarations::default();
    set.push(Declaration::Bind {
        state: State::Motion,
        keys: leader_keys("q"),
        command: "quit".into(),
    });
    session.apply(set).expect("resolves as a whole");

    press(&mut session, " ?");

    let text = described(&session);
    let motion = text.split("MOTION").nth(1).expect("a MOTION section");
    assert!(
        motion.contains("Quit meowr"),
        "the binding is listed\n{motion}"
    );
    assert!(
        motion.contains("Nothing enters this State today"),
        "and the note is still there\n{motion}"
    );
}

#[test]
fn keys_that_are_not_a_run_are_not_written_as_one() {
    // `>= 3 on one command` is not the same claim as `a run`. Three scattered
    // keys sharing a command would print `a … z`, which says every letter
    // between them does it too.
    // Punctuation nothing else binds, so the three land next to each other in
    // the list with the gaps between them still empty — the shape that fools a
    // count into thinking it has a range.
    let mut session = editing("alpha\n");
    let mut set = Declarations::default();
    for key in ["!", "#", "%"] {
        set.push(Declaration::Bind {
            state: State::Normal,
            keys: leader_keys(key),
            command: "join".into(),
        });
    }
    session.apply(set).expect("resolves as a whole");

    press(&mut session, " ?");

    let text = described(&session);
    assert!(
        !text.contains("! … %"),
        "`\"` and `$` do no such thing\n{text}"
    );
    assert_eq!(
        text.matches("Select the indentation joining").count(),
        4,
        "listed one by one, plus the `j` they were copied from\n{text}"
    );
}

#[test]
fn a_buffer_meowr_wrote_is_dismissed_by_the_key_emacs_dismisses_one_with() {
    // Reported from use: `SPC ?` opened the bindings buffer and neither `q`
    // nor `ESC` closed it. `SPC b k` did, but nothing on screen said so, and
    // `q` is what an Emacs hand reaches for — `quit-window` in every help
    // buffer there. Meow's Normal table binds no `q`, so taking it is Ring 3
    // rather than a divergence.
    let mut session = editing("alpha\n");
    press(&mut session, " ?");
    assert!(
        session
            .modeline_of(session.focus())
            .text()
            .contains("*bindings*"),
        "it opened"
    );

    press(&mut session, "q");

    assert!(
        !session
            .modeline_of(session.focus())
            .text()
            .contains("*bindings*"),
        "and it closed: {}",
        session.modeline_of(session.focus()).text()
    );
}

#[test]
fn the_dismiss_key_leaves_a_buffer_someone_is_editing_alone() {
    // `q` over a file has to do nothing. Killing the buffer somebody was
    // working in because they reached for a help key is the worst surprise
    // this command could hold.
    let mut session = editing("alpha\n");
    session.set_path("notes.rs");

    press(&mut session, "q");

    assert!(
        session
            .modeline_of(session.focus())
            .text()
            .contains("notes"),
        "{}",
        session.modeline_of(session.focus()).text()
    );
}

#[test]
fn opening_the_bindings_buffer_says_how_to_leave_it() {
    // The other half of the report: the way out existed and nothing named it.
    let mut session = editing("alpha\n");

    press(&mut session, " ?");

    let echo = session.echo().expect("it says something");
    assert!(echo.contains('q'), "{echo}");
}

// --- #80: a refusal says why ----------------------------------------------

#[test]
fn declining_to_pop_a_grab_that_is_not_held_says_so() {
    // `z` with no selection falls through to `pop-grab`, and with no Grab
    // there is nothing to pop. It said nothing at all — no message, no beep —
    // which is indistinguishable from a mistyped key.
    let mut session = editing("alpha beta\ngamma\n");

    press(&mut session, "z");

    let echo = session.echo().expect("it says why");
    assert!(echo.contains("Grab"), "{echo}");
}

#[test]
fn the_grab_commands_say_which_half_is_missing() {
    // Two ways to fail and they want different sentences: no Grab is a
    // different mistake from no selection, and "nothing happened" told you
    // neither.
    let mut session = editing("alpha beta\ngamma\n");
    press(&mut session, " gs");
    let echo = session.echo().expect("swap says why").to_string();
    assert!(echo.contains("Grab"), "{echo}");

    // Now hold a Grab, but select nothing.
    let mut session = editing("alpha beta\ngamma\n");
    press(&mut session, "lG");
    assert!(session.grab().is_some(), "the Grab is held");
    press(&mut session, " gy");
    let echo = session.echo().expect("sync says why").to_string();
    assert!(
        echo.to_lowercase().contains("selection"),
        "the other half: {echo}"
    );
}

#[test]
fn repeating_when_nothing_has_run_says_so() {
    let mut session = editing("alpha\n");

    press(&mut session, "'");

    let echo = session.echo().expect("it says why");
    assert!(echo.to_lowercase().contains("repeat"), "{echo}");
}

#[test]
fn refusing_to_close_the_only_window_says_why_and_not_only_beeps() {
    // It signalled, which is a sound and not a sentence. Emacs says "Attempt
    // to delete sole ordinary window" — the reason, in words.
    let mut session = editing("alpha\n");

    press(&mut session, " wd");

    let echo = session.echo().expect("it says why");
    assert!(echo.to_lowercase().contains("window"), "{echo}");
}

#[test]
fn a_read_only_buffer_refuses_a_save_in_its_own_words() {
    // A different reason wanting a different sentence: `*bindings*` has no
    // file *and* cannot be edited, and "no file" would be the less useful
    // half to say.
    let mut session = editing("alpha\n");
    press(&mut session, " ?");

    press(&mut session, " fs");

    let echo = session.echo().expect("it says why");
    assert!(echo.to_lowercase().contains("read-only"), "{echo}");
}

/// Every way a session command can decline, and what it does about it.
///
/// The table #80 asks for, and the reason it exists is that the audit which
/// produced #80 got four of its ten first readings wrong: "nothing happened"
/// and "nothing should happen" look identical from outside, and nothing in the
/// codebase could say which was which. So each row states which it is.
///
/// `Says` means the Echo Area carries a reason. `Nothing to decline` means the
/// command did its job and its job was to do nothing — those are not bugs and
/// listing them is what stops the next audit re-reporting them.
const DECLINING: &[(&str, &str, Declines)] = &[
    ("z", "pop the Grab when none is held", Declines::Says),
    (" gs", "swap with a Grab that is not held", Declines::Says),
    (" gy", "sync with a Grab that is not held", Declines::Says),
    ("'", "repeat when nothing has run", Declines::Says),
    (" wd", "close the only window", Declines::Aloud),
    // `SPC f s` with no file is *not* here any more: since #84 it opens a read
    // rather than declining. What still declines is a save it could never do —
    // over a buffer meowr wrote, which is read-only by construction.
    (" ? fs", "save a buffer that is read-only", Declines::Aloud),
    // Correct as they stand — see the correction on #80. Each of these does
    // in Emacs exactly what it does here, including the silence.
    ("G", "grab with nothing selected", Declines::Nothing),
    (" wm", "close others when there are none", Declines::Nothing),
    (
        " bb",
        "next buffer when there is only one",
        Declines::Nothing,
    ),
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Declines {
    /// Says why, quietly. Emacs's `message`.
    Says,
    /// Says why and rings the bell. Emacs's `user-error`.
    Aloud,
    /// Declines nothing: it did its job, and its job was to do nothing.
    Nothing,
    /// Rang the bell and said nothing — the failure #80 exists to end.
    ///
    /// No row may claim it. It is a category so that a command in it is
    /// reported as what it is rather than folded into `Nothing`, which is what
    /// the first version of this table did: a beep with no sentence read as
    /// "did its job", and `swap-grab`'s overlapping-ranges arm sat in the tree
    /// doing exactly that.
    SilentBell,
}

#[test]
fn a_command_that_declines_says_why() {
    for (keys, what, expected) in DECLINING {
        let mut session = editing("alpha beta\ngamma\n");
        let before = session.signals();
        press(&mut session, keys);
        let said = session.echo().is_some();
        let rang = session.signals() > before;
        let actual = match (said, rang) {
            (true, false) => Declines::Says,
            (true, true) => Declines::Aloud,
            (false, true) => Declines::SilentBell,
            (false, false) => Declines::Nothing,
        };
        assert_eq!(
            actual, *expected,
            "`{keys}` — {what} — behaves as {actual:?}, table says {expected:?}"
        );
    }
}

#[test]
fn no_command_rings_the_bell_without_saying_why() {
    // The table above is a hand-written list and can only see what somebody
    // thought to write down — which is how `swap-grab`'s overlapping-ranges
    // arm survived the first pass of #80. `DROPPED` and `DOOM_TOP_LEVEL` get
    // their teeth from walking a *live* source and demanding the table cover
    // it; this walks the registry, so a command added tomorrow is swept
    // whether or not anybody remembered to list it.
    //
    // Bare on purpose: no selection, no Grab, no file, nothing in the kill
    // ring. That is the state in which a command has most reason to decline.
    // Driven by *keys*, not by name. `Session::run` passes no key, and four
    // of meowr's commands take their argument from the second keypress — so a
    // name-driven sweep reported every Thing command as mute for a reason no
    // user can reach. Pressing the sequence is both the honest question and
    // the one whose answer is actionable.
    //
    // Collected rather than asserted one at a time: the keymap is a `HashMap`,
    // so which offender surfaces first varies between runs, and a test naming
    // a different command each run is one you fix by whack-a-mole instead of
    // by reading the list.
    let mut mute: Vec<String> = Vec::new();
    let keymap = meowr_app::default_keymap();
    for (keys, name) in keymap.bindings_in(State::Normal) {
        let mut session = Session::new(Viewport { top: 0, height: 10 });
        session.loaded(Document::from_text("alpha\n"));
        let before = session.signals();

        for key in keys {
            session.on_key(*key);
        }

        if session.signals() > before && session.echo().is_none() {
            mute.push(format!("{name} (on `{}`)", meowr_command::Keys(keys)));
        }
    }
    mute.sort();
    assert!(
        mute.is_empty(),
        "these rang the bell and said nothing: {mute:?}\n\
         A beep is what a mistyped key sounds like, so a refusal that only \
         beeps is unreadable. Give each an `Outcome::Refused`."
    );
}

// --- #82: `C-g` and `ESC` get their jobs back -----------------------------

#[test]
fn escape_cancels_a_selection_before_it_does_anything_else() {
    // `+meow-escape`: "Call `meow-cancel-selection', else fallback to
    // `doom/escape'." The selection comes first, and while one is up escape
    // means only that.
    let mut session = editing("alpha beta\n");
    press(&mut session, "w");
    assert!(
        session.selection().selection_type().is_some(),
        "a selection"
    );

    session.on_key(Key::Esc);

    assert_eq!(session.selection().selection_type(), None, "cancelled");
}

#[test]
fn escape_with_nothing_to_cancel_quits() {
    // The fallback half. `cancel-selection` already stands aside for
    // `keyboard-quit` in the fallback table, which is `doom/escape`'s last
    // resort spelled in the machinery meowr already had.
    let mut session = editing("alpha beta\n");

    session.on_key(Key::Esc);

    let echo = session.echo().expect("it says something");
    assert!(echo.contains("Quit"), "{echo}");
}

#[test]
fn control_g_is_the_chord_for_the_same_abort() {
    let mut session = editing("alpha beta\n");

    session.on_key(Key::Ctrl('g'));

    let echo = session.echo().expect("it says something");
    assert!(echo.contains("Quit"), "{echo}");
}

#[test]
fn escape_closes_a_buffer_meowr_wrote() {
    // `doom/escape` runs `doom-escape-hook` — where a popup closes — before it
    // falls back to `keyboard-quit`. meowr's popup is a Buffer, so the middle
    // step is closing one.
    let mut session = editing("alpha\n");
    press(&mut session, " ?");
    assert!(
        session
            .modeline_of(session.focus())
            .text()
            .contains("*bindings*"),
        "it opened"
    );

    session.on_key(Key::Esc);

    assert!(
        !session
            .modeline_of(session.focus())
            .text()
            .contains("*bindings*"),
        "and escape closed it: {}",
        session.modeline_of(session.focus()).text()
    );
}

#[test]
fn escape_and_control_g_still_leave_the_leader_and_the_picker() {
    // Both are handled ahead of the keymap — `keypad_key` and `narrow_key`
    // take them before the tree or the buffer sees anything. Binding them in
    // Normal must not reach past those, or the leader would start closing
    // buffers instead of closing itself.
    for key in [Key::Esc, Key::Ctrl('g')] {
        let mut session = editing("alpha\n");
        press(&mut session, " ?");
        let buffers = session.buffers().len();
        session.on_key(meowr_app::leader::KEY);
        assert_eq!(session.state(), State::Keypad, "the leader is open");

        session.on_key(key);

        assert_eq!(session.state(), State::Normal, "{key} left the leader");
        assert_eq!(
            session.buffers().len(),
            buffers,
            "{key} closed the leader, not the buffer behind it"
        );
        assert!(
            session
                .modeline_of(session.focus())
                .text()
                .contains("*bindings*"),
            "{key} left the buffer alone"
        );
    }

    for key in [Key::Esc, Key::Ctrl('g')] {
        let mut session = stocked();
        press(&mut session, " ff");
        assert!(session.narrowing().is_some(), "the picker is open");

        session.on_key(key);

        assert!(session.narrowing().is_none(), "{key} cancelled the picker");
    }
}

#[test]
fn escape_out_of_a_pending_character_read_only_aborts_the_read() {
    // `f` asks for a character. `on_key` takes the pending read before it
    // checks the key is one, so a non-character aborted the read *and* fell
    // through to the keymap — where ESC now closes buffers. Emacs's `C-g`
    // there aborts the read and stops.
    let mut session = editing("alpha\n");
    press(&mut session, " ?");
    press(&mut session, "f");

    session.on_key(Key::Esc);

    assert!(
        session
            .modeline_of(session.focus())
            .text()
            .contains("*bindings*"),
        "the read was abandoned, not the buffer: {}",
        session.modeline_of(session.focus()).text()
    );
    // And the read really is over: the next `x` is a binding, not an argument.
    press(&mut session, "x");
    assert!(session.echo().is_some(), "`x` ran as a command");
}

// --- #81: moving through a file without moving the selection --------------

/// A buffer taller than any window this suite makes, numbered so a row's text
/// says which line it is.
fn tall() -> Session {
    let text: String = (0..60).map(|n| format!("line {n}\n")).collect();
    let mut session = Session::new(Viewport { top: 0, height: 12 });
    session.on_resize(40, 12);
    session.loaded(Document::from_text(&text));
    session
}

fn showing(session: &Session) -> String {
    session
        .rows()
        .first()
        .map(|r| r.text.clone())
        .unwrap_or_default()
}

#[test]
fn a_page_down_moves_the_view_and_takes_the_selection_with_it() {
    // The gap the audit found: nothing moved the view except `scroll_into_view`
    // chasing the point, so the only way to see line 40 was to select your way
    // there. This is the other direction — the view leads and the point
    // follows, which is what `scroll-up-command` does.
    let mut session = tall();
    assert_eq!(showing(&session), "line 0");

    session.on_key(Key::PageDown);

    assert_ne!(showing(&session), "line 0", "the view moved");
    let top = session.rows().first().map(|r| r.line).expect("a first row");
    let at = session
        .document()
        .and_then(|d| d.byte_to_line(session.point()))
        .expect("a line for the point");
    assert!(
        at >= top,
        "the selection came with it: line {at}, top {top}"
    );
}

#[test]
fn a_page_up_comes_back_and_the_two_overlap() {
    // Emacs's `next-screen-context-lines`: a screenful is the window less two,
    // so a paragraph straddling the break can be read without paging back.
    // Down-then-up therefore does *not* return to the top.
    let mut session = tall();

    session.on_key(Key::PageDown);
    let after_down = session.rows().first().map(|r| r.line).expect("a row");
    session.on_key(Key::PageUp);

    assert_eq!(showing(&session), "line 0", "back where it started");
    assert!(
        after_down < 12,
        "and the two screenfuls overlapped: {after_down}"
    );
}

#[test]
fn paging_stops_with_a_full_screen_and_says_so_once_it_cannot_move() {
    // Two facts, both measured in Emacs rather than reasoned about. It scrolls
    // as far as it can and raises only when it cannot move at all — so the
    // first press of a run is quiet and the ones after it are not. And it
    // stops with the last screenful *full*: this clamped against the last line
    // instead, so paging to the end left one row of text above ten blank ones.
    //
    // #81's own acceptance criterion said "stops rather than signalling",
    // which was written before Emacs was asked.
    let mut session = tall();
    let quiet = session.signals();

    session.on_key(Key::PageDown);
    assert_eq!(session.signals(), quiet, "a page that moves is quiet");

    for _ in 0..40 {
        session.on_key(Key::PageDown);
    }
    let rows = session.rows();
    let height = rows.len();
    let last = rows.last().map(|r| r.line).expect("a last row");
    assert_eq!(
        rows.iter().filter(|r| !r.text.is_empty()).count(),
        height,
        "no blank rows below the end of the buffer"
    );
    assert_eq!(last, 59, "and the last line is the one at the bottom");
    assert!(session.signals() > quiet, "it said it could go no further");
    assert_eq!(session.echo(), Some("End of buffer."));

    for _ in 0..40 {
        session.on_key(Key::PageUp);
    }
    assert_eq!(showing(&session), "line 0", "and came all the way back");
    assert_eq!(session.echo(), Some("Beginning of buffer."));
}

#[test]
fn paging_leaves_a_selection_that_is_still_on_screen_alone() {
    // The view leads; the selection is dragged only as far as it has to be.
    let mut session = tall();
    press(&mut session, "nnn");
    let at = session.point();

    session.on_key(Key::PageUp);

    assert_eq!(session.point(), at, "already visible, so untouched");
}

#[test]
fn recentring_puts_the_selection_in_the_middle_of_the_window() {
    let mut session = tall();
    for _ in 0..3 {
        session.on_key(Key::PageDown);
    }
    press(&mut session, "n");

    session.on_key(Key::Ctrl('l'));

    let top = session.rows().first().map(|r| r.line).expect("a row");
    let at = session
        .document()
        .and_then(|d| d.byte_to_line(session.point()))
        .expect("a line");
    let height = session.rows().len();
    assert_eq!(at - top, height / 2, "half a window above it");
}

#[test]
fn the_buffer_ends_are_a_key_away() {
    // `M-<` and `M->`. These move the *selection*, not the view — the view
    // follows as it does for any other motion — so unlike paging they are
    // ordinary motions that happen to go a long way.
    let mut session = tall();

    session.on_key(Key::Alt('>'));
    assert_eq!(
        session.point(),
        session.document().expect("loaded").byte_len(),
        "the end"
    );

    session.on_key(Key::Alt('<'));
    assert_eq!(session.point(), 0, "and back to the beginning");
    assert_eq!(showing(&session), "line 0", "with the view following");
}

#[test]
fn the_arrow_keys_move_the_way_the_motion_keys_do() {
    // They reached the session and died there: `to_key` has translated `Up`
    // and `Down` since the beginning and nothing bound them, so an arrow key
    // navigated the picker and did nothing in a buffer. `meow-normal` binds no
    // arrow at all, so giving them the plain motions takes nothing back.
    let mut session = editing("alpha beta\ngamma delta\n");

    session.on_key(Key::Down);
    assert_eq!(session.point(), 11, "down a line");
    session.on_key(Key::Right);
    assert_eq!(session.point(), 12, "right a character");
    session.on_key(Key::Left);
    assert_eq!(session.point(), 11, "and back");
    session.on_key(Key::Up);
    assert_eq!(session.point(), 0, "up a line");
    assert_eq!(
        session.selection().selection_type(),
        None,
        "plain motions, so nothing is selected behind them"
    );
}

// --- #84: naming a buffer that has none -----------------------------------

#[test]
fn saving_a_buffer_with_no_file_asks_for_one() {
    // #80 gave this refusal a voice — "No file behind this buffer to write
    // to." This is the feature the voice was pointing at.
    let mut session = Session::new(Viewport { top: 0, height: 10 });
    session.loaded(Document::from_text("alpha\n"));

    press(&mut session, " fs");

    assert!(session.narrowing().is_some(), "it asks");
    assert!(
        session.save_target().is_none(),
        "and has written nothing yet"
    );
}

#[test]
fn the_name_typed_becomes_the_buffers_file_and_it_is_written() {
    let mut session = Session::new(Viewport { top: 0, height: 10 });
    session.loaded(Document::from_text("alpha\n"));
    press(&mut session, " fs");

    press(&mut session, "notes.txt");
    let action = session.on_key(Key::Enter);

    assert_eq!(action, Action::Save, "the host is asked to write");
    let write = session.save_target().expect("and told what to write");
    assert_eq!(write.path, "notes.txt");
    assert_eq!(write.contents, "alpha\n");
}

#[test]
fn a_name_with_a_space_in_it_is_a_name() {
    // The same property ADR-0005's `reading_text` grants a Guest, and for the
    // same reason: a path may contain a space, so a read that treated `SPC` as
    // the leader would be unusable. The widget takes every key while it is
    // open, which is what makes it meowr's minibuffer.
    let mut session = Session::new(Viewport { top: 0, height: 10 });
    session.loaded(Document::from_text("alpha\n"));
    press(&mut session, " fs");

    press(&mut session, "my notes.txt");
    session.on_key(Key::Enter);

    assert_eq!(
        session.save_target().expect("a target").path,
        "my notes.txt"
    );
}

#[test]
fn cancelling_the_read_leaves_the_buffer_nameless_and_says_so() {
    let mut session = Session::new(Viewport { top: 0, height: 10 });
    session.loaded(Document::from_text("alpha\n"));
    press(&mut session, " fs");
    press(&mut session, "notes.txt");

    session.on_key(Key::Esc);

    assert!(session.narrowing().is_none(), "the read is over");
    assert!(session.save_target().is_none(), "and nothing was written");
    assert_eq!(session.echo(), Some("Nothing was written."));
}

#[test]
fn an_empty_name_writes_nothing() {
    let mut session = Session::new(Viewport { top: 0, height: 10 });
    session.loaded(Document::from_text("alpha\n"));
    press(&mut session, " fs");

    session.on_key(Key::Enter);

    assert!(session.save_target().is_none());
    assert_eq!(
        session.echo(),
        Some("No name given, so nothing was written.")
    );
}

#[test]
fn a_named_buffer_stops_being_scratch_everywhere_at_once() {
    // The modeline, the buffer list and the picker all ask the Buffer its
    // name, so naming it has to change all three or none. Worth asserting
    // because `*scratch*` is a label rather than a path, and a buffer that
    // gained a file while still calling itself `*scratch*` would be findable
    // under a name it no longer has.
    let mut session = Session::new(Viewport { top: 0, height: 10 });
    session.loaded(Document::from_text("alpha\n"));
    assert!(
        session
            .modeline_of(session.focus())
            .text()
            .contains("*scratch*")
    );

    press(&mut session, " fs");
    press(&mut session, "notes.txt");
    session.on_key(Key::Enter);

    let line = session.modeline_of(session.focus()).text().to_string();
    assert!(line.contains("notes.txt"), "the modeline: {line}");
    assert!(!line.contains("*scratch*"), "and not both: {line}");
    assert!(
        session.buffers().iter().any(|b| b.name() == "notes.txt"),
        "the buffer list"
    );
}

#[test]
fn a_freshly_named_buffer_asks_the_host_to_look_before_it_writes() {
    // Naming a buffer is the one write that can land on somebody else's file,
    // and the session cannot tell: checking a path is I/O and this crate does
    // none. So the request says the name is new, and the host — which owns the
    // filesystem — is the one that looks.
    let mut session = Session::new(Viewport { top: 0, height: 10 });
    session.loaded(Document::from_text("alpha\n"));
    press(&mut session, " fs");
    press(&mut session, "notes.txt");
    session.on_key(Key::Enter);

    assert!(session.save_target().expect("a target").fresh, "look first");

    session.mark_saved();
    press(&mut session, " fs");
    assert!(
        !session.save_target().expect("a target").fresh,
        "and not again once it is ours"
    );
}

#[test]
fn refusing_to_overwrite_names_the_key_that_would() {
    let mut session = Session::new(Viewport { top: 0, height: 10 });
    session.loaded(Document::from_text("alpha\n"));
    press(&mut session, " fs");
    press(&mut session, "notes.txt");
    session.on_key(Key::Enter);

    session.refuse_overwrite();

    let echo = session.echo().expect("it says why");
    assert!(echo.contains("notes.txt"), "which file: {echo}");
    assert!(echo.contains("SPC f S"), "and the way past it: {echo}");
    assert!(session.save_target().expect("still named").fresh);

    let action = session.run("overwrite-file");
    assert_eq!(action, Action::Save);
    assert!(
        !session.save_target().expect("a target").fresh,
        "the host is told not to look again"
    );
}

#[test]
fn naming_a_second_buffer_does_not_disarm_the_first() {
    // The flag lived on the Session first, which is one slot for a fact about
    // a Buffer: naming a second buffer cleared the first's, and the first then
    // wrote over an existing file without the host being asked to look.
    let mut session = Session::new(Viewport { top: 0, height: 10 });
    session.loaded(Document::from_text("first\n"));
    press(&mut session, " fs");
    press(&mut session, "one.txt");
    session.on_key(Key::Enter);
    let first = session.current_buffer_id();

    let mut other = Buffer::new();
    other.loaded(Document::from_text("second\n"));
    let second = session.open(other);
    press(&mut session, " fs");
    press(&mut session, "two.txt");
    session.on_key(Key::Enter);
    assert!(session.save_target().expect("the second").fresh);

    session.show(first);

    assert!(
        session.save_target().expect("the first").fresh,
        "still new, and still to be looked at before it is written"
    );
    let _ = second;
}

#[test]
fn forcing_a_write_over_a_buffer_that_has_no_file_still_says_so() {
    // `overwrite-file` answered `Action::Save` itself, walking past the arm
    // that refuses a save with nothing to write — so over `*scratch*` it asked
    // the host to write a file with no name, and the host said nothing. #80's
    // silence under a new key.
    let mut session = Session::new(Viewport { top: 0, height: 10 });
    session.loaded(Document::from_text("alpha\n"));

    let action = session.run("overwrite-file");

    assert_ne!(action, Action::Save, "there is nothing to write");
    assert!(session.narrowing().is_some(), "it asks for a name instead");
}

// --- #77: the leader keys nothing had accounted for -----------------------

#[test]
fn a_digit_under_the_leader_leaves_it_and_arms_a_count() {
    // `SPC 0`-`9` are `meow-digit-argument`, not `winum` window selection —
    // this ticket said winum until the TSV was read.
    //
    // Meow ends the walk on the digit rather than carrying it: keypad looks
    // the key up, finds a command, and `meow--keypad-try-execute` calls
    // `meow--keypad-quit` *before* `meow--keypad-execute`. What the digit then
    // does is `(set-transient-map meow-numeric-argument-keymap)` and
    // `digit-argument` — so the count is armed in Normal State, for a Normal
    // State command.
    //
    // Which is the point of the key existing. Normal State's own digits are
    // `meow-expand`, so `SPC 3` is the only way to say "three" to `n`.
    let mut session = editing("alpha\n");

    session.on_key(meowr_app::leader::KEY);
    press(&mut session, "3");

    assert_eq!(
        session.state(),
        State::Normal,
        "the walk ended on the digit"
    );
    assert_eq!(session.argument().value(), Some(3));
}

#[test]
fn further_digits_accumulate_in_normal_state() {
    // `meow-numeric-argument-keymap` binds the ten digits and nothing else,
    // and it is transient — so digits keep folding in and the first key that
    // is not one runs with what they built.
    let mut session = editing("alpha\nbeta\ngamma\ndelta\n");

    session.on_key(meowr_app::leader::KEY);
    press(&mut session, "12");

    assert_eq!(session.argument().value(), Some(12), "not 1 then a 2 key");
    assert_eq!(
        session.selection().selection_type(),
        None,
        "and the 2 did not expand anything, which is what it means in Normal"
    );
}

#[test]
fn the_count_reaches_the_next_normal_command() {
    let mut session = editing("alpha\nbeta\ngamma\ndelta\n");

    session.on_key(meowr_app::leader::KEY);
    press(&mut session, "3n");

    let at = session
        .document()
        .and_then(|d| d.byte_to_line(session.point()))
        .expect("a line");
    assert_eq!(at, 3, "three lines down, not one");
    assert_eq!(session.argument().value(), None, "and the count is spent");
}

#[test]
fn abandoning_a_count_does_not_arm_the_next_command() {
    // `keyboard-quit` clears `prefix-arg` in Emacs. Without that, `SPC 3` then
    // a change of mind left a 3 waiting for whatever was pressed next.
    let mut session = editing("alpha\nbeta\ngamma\ndelta\n");
    session.on_key(meowr_app::leader::KEY);
    press(&mut session, "3");

    session.on_key(Key::Ctrl('g'));

    assert_eq!(session.argument().value(), None, "the count is dropped");
    press(&mut session, "n");
    let at = session
        .document()
        .and_then(|d| d.byte_to_line(session.point()))
        .expect("a line");
    assert_eq!(at, 1, "one line down, not three");
}

// --- undoing back to the saved state ---------------------------------------

#[test]
fn undoing_back_to_what_was_saved_stops_calling_the_buffer_modified() {
    // Reported from use. `modified` was a one-way latch — any mutating command
    // set it and only saving cleared it — and `undo` is a mutating command, so
    // undoing an edit marked the buffer modified exactly as making it did.
    let mut session = editing("alpha\n");
    savable(&mut session);
    session.mark_saved();

    press(&mut session, "sZ");
    session.on_key(Key::Esc);
    assert!(session.is_modified(), "typing modified it");

    press(&mut session, "u");

    assert_eq!(
        session.document().expect("loaded").text().to_string(),
        "alpha\n",
        "the text is what was saved"
    );
    assert!(!session.is_modified(), "so the buffer is not modified");
}

#[test]
fn quitting_after_undoing_back_to_the_saved_state_is_not_refused() {
    // The half that is not cosmetic: `modified` is what `SPC q q` consults, so
    // a stale flag sent you to `SPC q Q` — the discard-my-work key — in a case
    // where nothing was at risk.
    let mut session = editing("alpha\n");
    savable(&mut session);
    session.mark_saved();
    press(&mut session, "sZ");
    session.on_key(Key::Esc);
    press(&mut session, "u");

    let action = session.run("quit");

    assert_eq!(
        action,
        Action::Quit,
        "nothing to lose, so nothing to refuse"
    );
}

#[test]
fn an_edit_after_an_undo_is_a_modification_even_at_the_same_depth() {
    // The case a depth comparison gets wrong. Undo, then type something new:
    // the redo stack is cleared and the new transaction takes the old one's
    // place, so the history is exactly as deep as it was at the save and the
    // text is different.
    let mut session = editing("alpha\n");
    savable(&mut session);
    session.mark_saved();
    press(&mut session, "sZ");
    session.on_key(Key::Esc);
    press(&mut session, "u");
    assert!(!session.is_modified(), "back at the saved state");

    press(&mut session, "sQ");
    session.on_key(Key::Esc);

    assert!(session.is_modified(), "a different edit, same depth");
}

// --- hl-line ---------------------------------------------------------------

#[test]
fn the_line_the_point_is_on_is_marked_current() {
    let mut session = editing("alpha\nbeta\ngamma\n");

    press(&mut session, "n");

    let rows = session.rows();
    assert!(rows[1].current, "the point is on line 1");
    assert!(!rows[0].current && !rows[2].current, "and only there");
}

#[test]
fn a_live_selection_puts_the_line_highlight_away() {
    // Doom's rule, and its reason: `global-hl-line-buffers` excludes a buffer
    // where `doom-region-active-p`. Two highlights over the same text say less
    // than one, and the selection is the one carrying information.
    let mut session = editing("alpha beta\ngamma\n");

    press(&mut session, "w");

    assert!(
        session.selection().selection_type().is_some(),
        "there is a selection"
    );
    assert!(
        session.rows().iter().all(|row| !row.current),
        "so no row is marked current"
    );
}

#[test]
fn only_the_focused_window_marks_a_current_line() {
    // `global-hl-line-sticky-flag 'window`, and Doom's comment gives the
    // reason in as many words: "Don't display line highlights in non-focused
    // windows, for performance sake and to reduce UI clutter."
    let mut session = editing("alpha\nbeta\n");
    press(&mut session, " ws");
    let focused = session.focus();
    let other = session
        .windows()
        .order()
        .into_iter()
        .find(|id| *id != focused)
        .expect("two windows");

    assert!(session.rows_of(focused).iter().any(|row| row.current));
    assert!(
        session.rows_of(other).iter().all(|row| !row.current),
        "the other window shows no current line"
    );
}

#[test]
fn a_preview_buffer_shows_no_current_line() {
    // Doom's predicate excludes `(doom-temp-buffer-p b)` alongside the region
    // and the minibuffer, and a narrowing preview is meowr's analogue: a file
    // nobody chose to open, shown behind the widget. A band across it would
    // point at a line in a buffer you are not editing.
    let mut session = stocked();
    press(&mut session, " ff");
    session.on_key(Key::Char('<'));
    session.on_key(Key::Char('r'));
    session.previewed("/tmp/one.rs", Ok("one\ntwo\n".into()));
    assert!(session.current().is_preview(), "a preview is on screen");

    assert!(
        session.rows().iter().all(|row| !row.current),
        "the preview shows no band"
    );
}

// --- #78: a chord can reach one Thing --------------------------------------

#[test]
fn a_chord_can_reach_one_thing() {
    // #78. The author's config binds `M-a` to `beginning-of-defun`, and there
    // was nothing to point it at: the Thing arrives in the *second* keypress,
    // so "beginning of line" was a key sequence and not a command. One name
    // per operation and Thing makes it one — and the proof is that the chord
    // and the two keys leave the buffer in the same place, by two routes that
    // share no code below the registry.
    let two_keys = {
        let mut session = editing("alpha beta\ngamma\n");
        press(&mut session, "nn");
        session.on_key(Key::Char('['));
        session.on_key(Key::Char('l'));
        session.selection()
    };

    let mut chord = editing("alpha beta\ngamma\n");
    press(&mut chord, "nn");
    let mut set = Declarations::default();
    set.push(Declaration::Bind {
        state: State::Normal,
        keys: vec![Key::Alt('a')],
        command: "beginning-of-thing/line".into(),
    });
    chord.apply(set).expect("the name exists to be bound");

    chord.on_key(Key::Alt('a'));

    assert_eq!(chord.selection().range(), two_keys.range());
    assert_eq!(chord.selection().axis(), two_keys.axis());
}

#[test]
fn a_composition_can_name_the_thing_it_wants() {
    // The other half of #78, and the reason the seventy-six names are worth
    // having beyond one chord: a composed command has no keypress, so
    // `bounds-of-thing` in a body used to mean whichever Thing the key it was
    // hung under happened to name. Bound here under `M-k`, which names none of
    // them — before the Thing became part of the name, this could not work.
    let mut session = editing("(alpha) here\n");
    let mut set = Declarations::default();
    set.push(Declaration::Command {
        name: "take-the-round".into(),
        description: "Take the round brackets and all".into(),
        body: vec!["bounds-of-thing/round".into()],
    });
    set.push(Declaration::Bind {
        state: State::Normal,
        keys: vec![Key::Alt('k')],
        command: "take-the-round".into(),
    });
    session.apply(set).expect("resolves");
    session.on_key(Key::Right);
    session.on_key(Key::Right);

    session.on_key(Key::Alt('k'));

    // What `. r` gives from the same position, which is the route this one
    // has to agree with.
    assert_eq!(session.selection().range(), 0..7, "the brackets too");
}

#[test]
fn a_thing_command_with_no_thing_key_says_so() {
    // #78's fourth question, answered from the outside. A Thing command takes
    // its Thing from the second keypress, and a composition has no keypress at
    // all — so `bounds-of-thing` in a body is a command that cannot do its job
    // and, until now, beeped without saying why. That is the shape #80 swept
    // the keymap for; this one was out of its reach because it needs a config
    // to exist at all.
    let mut session = editing("(alpha beta)\n");
    let mut set = Declarations::default();
    set.push(Declaration::Command {
        name: "kill-a-thing".into(),
        description: "Take a thing, whichever one".into(),
        body: vec!["bounds-of-thing".into()],
    });
    set.push(Declaration::Bind {
        state: State::Normal,
        keys: vec![Key::Alt('k')],
        command: "kill-a-thing".into(),
    });
    session.apply(set).expect("resolves");
    let signals = session.signals();

    session.on_key(Key::Alt('k'));

    assert_eq!(session.signals(), signals + 1, "it still signals");
    let echo = session.echo().expect("and it says why");
    assert!(echo.contains("Thing"), "{echo}");
}

#[test]
fn a_thing_command_in_a_hook_says_why_it_cannot_run() {
    // The other doorway #78 named, and the one #64 opened: a command run from
    // a hook is dispatched with no key at all, so `bounds-of-thing` there is
    // in exactly the position it is inside a composition. The sentence has to
    // reach the Echo Area by that route too — a hook that beeps and says
    // nothing is worse than one in a config the author can read back.
    let mut session = editing("(alpha beta)\n");
    session
        .apply(hook_set(HookPoint::StateChange, "bounds-of-thing"))
        .expect("resolves");

    press(&mut session, "s"); // Normal -> Insert, where it is not valid anyway
    let signals = session.signals();

    session.on_key(Key::Esc); // back to Normal, where only the key is missing

    assert_eq!(session.signals(), signals + 1, "it signals");
    let echo = session.echo().expect("and says why");
    assert!(echo.contains("Thing"), "{echo}");
    assert_eq!(
        session.selection().range(),
        0..0,
        "and touches nothing, having no Thing to take"
    );
}

#[test]
fn every_thing_has_a_name_and_it_means_exactly_what_its_two_keys_mean() {
    // The cost of #78's answer, written where it can be checked. Seventy-six
    // names is a lot to take on, and they are only worth having if each one
    // means precisely what the two keys mean — otherwise the same Thing has
    // two behaviours wearing one word, which is worse than having no name.
    //
    // The table is spelled out again here on purpose. `THING_CHARS` and the
    // names the macro builds from it are two halves of one fact and nothing in
    // the compiler makes them agree; a twentieth Thing, or a key moved from
    // one Thing to another, fails right here rather than quietly shipping a
    // name that no key can reach or a key that no name can.
    const THINGS: [(char, &str); 19] = [
        ('a', "class"),
        ('f', "function"),
        ('t', "test"),
        ('y', "entry"),
        (',', "parameter"),
        ('/', "comment"),
        ('r', "round"),
        ('s', "square"),
        ('c', "curly"),
        ('g', "string"),
        ('e', "symbol"),
        ('w', "window"),
        ('b', "buffer"),
        ('p', "paragraph"),
        ('l', "line"),
        ('v', "visual-line"),
        ('d', "defun"),
        ('.', "sentence"),
        ('m', "match"),
    ];
    // Enough shape for the delimiter Things to find something and for the
    // line-ish ones to have somewhere to stop.
    const TEXT: &str = "(alpha \"beta\") [gamma] {delta}\n\nsecond paragraph\n";

    for (prefix, op) in [
        (',', "inner-of-thing"),
        ('.', "bounds-of-thing"),
        ('[', "beginning-of-thing"),
        (']', "end-of-thing"),
    ] {
        for (ch, thing) in THINGS {
            let two_keys = {
                let mut session = editing(TEXT);
                press(&mut session, "nnnnn");
                session.on_key(Key::Char(prefix));
                session.on_key(Key::Char(ch));
                session.selection()
            };

            let mut chord = editing(TEXT);
            press(&mut chord, "nnnnn");
            let name = format!("{op}/{thing}");
            let mut set = Declarations::default();
            set.push(Declaration::Bind {
                state: State::Normal,
                keys: vec![Key::Alt('a')],
                command: name.clone(),
            });
            chord
                .apply(set)
                .unwrap_or_else(|problems| panic!("`{name}` must exist: {problems:?}"));

            chord.on_key(Key::Alt('a'));

            assert_eq!(
                chord.selection().range(),
                two_keys.range(),
                "`{name}` must reach what `{prefix} {ch}` reaches"
            );
            assert_eq!(
                chord.selection().axis(),
                two_keys.axis(),
                "`{name}` must leave the point where `{prefix} {ch}` leaves it"
            );
        }
    }
}

#[test]
fn a_composed_self_insert_says_it_has_nothing_to_insert() {
    // Found by #78's own change rather than by #78. Taking the key away from a
    // composed step fixed the Thing commands and left this one returning
    // `Handled` having inserted nothing — and before, it inserted whichever
    // key the composition was bound under, which is the same wrong answer
    // `(bounds-of-thing kill)` was giving. Wrong, then silent, now said.
    let mut session = editing("alpha\n");
    let mut set = Declarations::default();
    set.push(Declaration::Command {
        name: "type-something".into(),
        description: "Insert whatever".into(),
        body: vec!["self-insert".into()],
    });
    set.push(Declaration::Bind {
        state: State::Insert,
        keys: vec![Key::Alt('k')],
        command: "type-something".into(),
    });
    session.apply(set).expect("resolves");
    press(&mut session, "s"); // into Insert, where `self-insert` is valid
    let signals = session.signals();

    session.on_key(Key::Alt('k'));

    assert_eq!(
        session.contents().unwrap(),
        "alpha\n",
        "nothing was inserted"
    );
    assert_eq!(session.signals(), signals + 1, "and it does not go quietly");
    assert_eq!(session.echo(), Some("Nothing to insert."));
}

// --- #88: a keyless dispatch says why --------------------------------------

// The sweep over every command lives in `tests/registry.rs`, a seam below
// this one. These are the witnesses that what it checks survives the trip
// out to the Echo Area, and that a config reaches the same behaviour.

#[test]
fn a_composed_surround_knows_its_own_pair() {
    // The deeper half of #88, and the reason `surround-*` needed more than a
    // sentence. It used to read its delimiter back out of the key, so in a
    // composition — which has no key at all (#78) — it beeped and said
    // nothing. Saying why would have been the small fix. The pair is in the
    // command's own name, so the right one is that there is nothing to say.
    //
    // Bound here under `M-k`, which names no delimiter. Under the old body
    // this wrapped in whatever `surround_pair` made of the key it was hung
    // under, which for `c` would have been curly braces from a command calling
    // itself round.
    let mut session = editing("alpha beta\n");
    press(&mut session, "w"); // select "alpha"
    let mut set = Declarations::default();
    set.push(Declaration::Command {
        name: "wrap-it".into(),
        description: "Wrap the selection in round brackets".into(),
        body: vec!["surround-round".into()],
    });
    set.push(Declaration::Bind {
        state: State::Normal,
        keys: vec![Key::Alt('k')],
        command: "wrap-it".into(),
    });
    session.apply(set).expect("resolves");
    let signals = session.signals();

    session.on_key(Key::Alt('k'));

    assert_eq!(session.contents().unwrap(), "(alpha) beta\n");
    assert_eq!(session.signals(), signals, "and nothing to say about it");
}

#[test]
fn a_composed_digit_argument_says_where_its_digit_comes_from() {
    // The one the sweep cannot see: `digit-argument` returned `Handled` having
    // built no argument, and a command that claims success looks exactly like
    // one that had nothing to do. Found by reading every `ctx.key` rather than
    // by the sweep, which is why #88 asked for the enumeration as well.
    let mut session = editing("alpha\n");
    let mut set = Declarations::default();
    set.push(Declaration::Command {
        name: "count-something".into(),
        description: "Build a numeric argument".into(),
        body: vec!["digit-argument".into()],
    });
    set.push(Declaration::Bind {
        state: State::Normal,
        keys: vec![Key::Alt('k')],
        command: "count-something".into(),
    });
    session.apply(set).expect("resolves");
    let signals = session.signals();

    session.on_key(Key::Alt('k'));

    assert_eq!(session.signals(), signals + 1, "it signals");
    let echo = session.echo().expect("and says why");
    assert!(echo.contains("digit"), "{echo}");
}

#[test]
fn a_letter_before_the_selection_is_not_an_opening_delimiter() {
    // `surround_pair` was a *key* lookup: `r` meant round brackets because `r`
    // was the key you pressed for them. `unsurround` asks it a different
    // question — "is this character in the buffer an opener" — and got the key
    // letters back with it. So a selection preceded by a plain `r` and
    // followed by `)` looked wrapped, and unwrapping ate the `r`.
    //
    // Found while removing the key lookup for #88. The bug is older than that
    // change: `unsurround` never had a key to pass.
    let mut session = editing("barX)\n");
    session.set_selection(Selection::typed(core::Axis::Select, core::Kind::WORD, 3, 4));
    let before = session.contents().unwrap();
    let signals = session.signals();

    session.on_key(Key::Alt('\''));
    session.on_key(Key::Char('\''));

    assert_eq!(session.contents().unwrap(), before, "`r` opens nothing");
    assert_eq!(session.signals(), signals + 1);
    assert_eq!(session.echo(), Some("The selection is not wrapped."));
}

#[test]
fn a_composed_surround_with_no_selection_refuses_rather_than_wrapping_nothing() {
    // The regression this change nearly shipped, and the reason the sweep grew
    // a second buffer state. Taking the pair out of the key removed the early
    // return that a missing key used to trigger, so a composition with nothing
    // selected wrote `()` into the buffer and said nothing at all — a silent
    // wrong edit where there had been a wordless beep, which is a trade in the
    // wrong direction.
    //
    // `requiring_selection` should be what catches this. The session enforces
    // it on the name it *dispatches*, and a composition's steps never pass
    // through there — the same hole lets a composition edit a read-only
    // buffer, which is older than this change and is filed on its own.
    let mut session = editing("alpha beta\n");
    let mut set = Declarations::default();
    set.push(Declaration::Command {
        name: "wrap-it".into(),
        description: "Wrap the selection in round brackets".into(),
        body: vec!["surround-round".into()],
    });
    set.push(Declaration::Bind {
        state: State::Normal,
        keys: vec![Key::Alt('k')],
        command: "wrap-it".into(),
    });
    session.apply(set).expect("resolves");
    let signals = session.signals();

    session.on_key(Key::Alt('k'));

    assert_eq!(
        session.contents().unwrap(),
        "alpha beta\n",
        "nothing was wrapped"
    );
    assert_eq!(session.signals(), signals + 1, "and it says so");
    assert_eq!(session.echo(), Some("No selection."));
}

// --- diagnostics (#115) ---------------------------------------------------

fn diagnostic(range: std::ops::Range<usize>, message: &str) -> Diagnostic {
    Diagnostic {
        range,
        message: message.to_string(),
        severity: Severity::Error,
    }
}

#[test]
fn a_buffer_holds_diagnostics_and_they_reach_the_grid() {
    let mut session = editing("alpha beta\n");
    let revision = session.revision();
    assert_eq!(
        session.set_diagnostics(
            session.current_buffer_id(),
            revision,
            vec![diagnostic(0..5, "expected i32")],
        ),
        Action::Redraw
    );
    let placed = session.diagnostics_of(session.focus());
    assert_eq!(placed.len(), 1);
    assert_eq!(placed[0].name, "diagnostic.error");
    assert_eq!(placed[0].columns, 0..5);
}

#[test]
fn an_edit_before_a_diagnostic_shifts_it() {
    // The same guarantee the Grab has, and for the same reason: a diagnostic
    // is a fact about a *span of text*, and text that moved took the span with
    // it. `after_command`'s own shift closure does both.
    let mut session = editing("alpha beta gamma\n");
    let revision = session.revision();
    session.set_diagnostics(
        session.current_buffer_id(),
        revision,
        vec![diagnostic(11..16, "no such thing")],
    );

    // Kill "alpha" at the front; everything after it moves by five.
    session.set_selection(Selection::at(0));
    press(&mut session, "wk");

    assert_eq!(
        session.diagnostics()[0].range,
        6..11,
        "it moved with the text"
    );
}

#[test]
fn a_diagnostic_tagged_with_a_stale_revision_is_refused() {
    // `apply_reparse`'s own mechanism, for the same reason: the answer was
    // computed against text the buffer has since moved past, so placing it
    // would put a squiggle under something it says nothing about.
    let mut session = editing("alpha beta\n");
    let stale = session.revision();
    press(&mut session, "iX");
    press(&mut session, "\x1b");
    assert_ne!(session.revision(), stale, "the edit moved the revision on");

    assert_eq!(
        session.set_diagnostics(
            session.current_buffer_id(),
            stale,
            vec![diagnostic(0..5, "too late")],
        ),
        Action::Nothing
    );
    assert!(session.diagnostics().is_empty());
}

#[test]
fn the_modeline_reports_a_count_only_when_there_is_one() {
    let mut session = editing("alpha beta\n");
    let before = session.modeline_of(session.focus()).text();
    assert!(!before.contains('!'), "{before}");

    let revision = session.revision();
    session.set_diagnostics(
        session.current_buffer_id(),
        revision,
        vec![diagnostic(0..5, "one"), diagnostic(6..10, "two")],
    );
    let after = session.modeline_of(session.focus()).text();
    assert!(after.contains("2!"), "{after}");
}

#[test]
fn killing_a_buffer_takes_its_diagnostics_with_it() {
    let mut session = editing("alpha beta\n");
    let revision = session.revision();
    session.set_diagnostics(
        session.current_buffer_id(),
        revision,
        vec![diagnostic(0..5, "gone soon")],
    );
    assert!(!session.diagnostics().is_empty());

    press(&mut session, "\x20bk");
    assert!(
        session.diagnostics().is_empty(),
        "a tombstone holds no text, so it holds nothing said about text"
    );
}

// --- hover (#116) ---------------------------------------------------------

#[test]
fn a_hover_reaches_the_echo_area_grown_to_fit() {
    let mut session = editing_a_file("alpha beta\n");
    session.show_hover("main.rs", Some("fn main()\nRuns the program."));
    let view = session.hover_view().expect("a hover was shown");
    assert_eq!(view.lines, vec!["fn main()", "Runs the program."]);
}

#[test]
fn any_key_dismisses_a_hover() {
    // The same rule the Echo Area already keeps: Emacs clears it on the next
    // command, and a hover is the Echo Area grown to fit.
    let mut session = editing_a_file("alpha beta\n");
    session.show_hover("main.rs", Some("fn main()"));
    assert!(session.hover_view().is_some());
    press(&mut session, "e");
    assert!(session.hover_view().is_none());
}

#[test]
fn a_hover_is_capped_against_the_terminals_own_height() {
    // A twenty-line doc comment in a five-row terminal must not ask for more
    // rows than the frame has, or the windows above it are laid out negative.
    let mut session = editing_a_file("alpha\n");
    session.on_resize(40, 5);
    let long = (1..=20)
        .map(|n| n.to_string())
        .collect::<Vec<_>>()
        .join("\n");
    session.show_hover("main.rs", Some(&long));
    let view = session.hover_view().expect("a hover was shown");
    assert!(
        view.lines.len() < 5,
        "{} rows in a 5-row frame",
        view.lines.len()
    );
    assert!(view.truncated, "and it says it left something out");
}

#[test]
fn the_narrowing_widget_wins_the_bottom_of_the_frame() {
    // Exactly one of the three occupies it. The widget is a modal read taking
    // every key, so a hover underneath it would be unreachable anyway.
    let mut session = editing_a_file("alpha beta\n");
    // The widget first, then the hover: a key press dismisses a hover, so a
    // hover shown before the keys that open the widget would be gone for the
    // uninteresting reason rather than this test's one.
    press(&mut session, " ."); // Doom's `SPC .`, the buffer picker
    assert!(session.narrowing().is_some(), "the widget is open");
    session.show_hover("main.rs", Some("fn main()"));
    assert!(session.hover_view().is_none(), "and the hover stood aside");
}

#[test]
fn the_leader_menu_wins_the_bottom_of_the_frame() {
    let mut session = editing_a_file("alpha beta\n");
    press(&mut session, " ");
    session.show_menu();
    assert!(session.menu_visible(), "the menu is open");
    session.show_hover("main.rs", Some("fn main()"));
    assert!(session.hover_view().is_none(), "and the hover stood aside");
}

#[test]
fn a_hover_is_no_wider_than_the_reference_configuration_asks_for() {
    // `lsp-ui-doc-max-width` is 150 in `lsp-ui`, and 150 again in the
    // reference configuration (`config-ui.el:18`), which restates it rather
    // than changing it. So the cap is a constant and not an Option: there is
    // no configured value for it to disagree with.
    //
    // It bites only on a frame wider than a doc comment wants to be. Wrapping
    // a hover across 300 columns is the failure -- the eye cannot find the
    // start of the next line.
    let mut session = editing_a_file("alpha\n");
    session.on_resize(300, 20);
    session.show_hover("main.rs", Some(&"word ".repeat(80)));
    let view = session.hover_view().expect("a hover was shown");

    assert!(
        view.lines.iter().all(|line| line.chars().count() <= 150),
        "{:?}",
        view.lines
            .iter()
            .map(|l| l.chars().count())
            .collect::<Vec<_>>()
    );
    assert!(
        view.lines.len() > 1,
        "and the cap is what wrapped it, on a frame with room for one line"
    );
}

#[test]
fn a_message_that_arrives_later_wins_the_bottom_of_the_frame() {
    // #122. The third member of the set `hover_view` already arbitrates, and
    // the one it did not check. `Bottom` ranks a hover above a message with
    // its reason written down -- "it is the answer to a key just pressed" --
    // and that reason is about the hover, not about a message that arrived
    // afterwards. A message the editor decided to say and never drew is a
    // silent hole of exactly the kind #80's sweep exists to close.
    let mut session = editing_a_file("alpha beta\n");
    session.show_hover("main.rs", Some("fn main()"));
    assert!(session.hover_view().is_some(), "the hover is up");

    session.say("rust-analyzer: No such file or directory");

    assert!(session.hover_view().is_none(), "and it stood aside");
    assert_eq!(
        session.echo(),
        Some("rust-analyzer: No such file or directory")
    );
}

#[test]
fn a_hover_still_outranks_the_message_the_key_before_it_left() {
    // Which is why this is not a re-ranking of `Bottom`. `SPC k k`'s own case:
    // the key that asks clears the Echo Area, so any message a hover replaces
    // is one the user has already had their chance at. Turning the order round
    // outright would hide an answer just asked for behind a message already
    // read.
    let mut session = editing_a_file("alpha beta\n");
    session.say("Killed 2 other buffers.");
    press(&mut session, "w"); // any key clears the Echo Area
    session.show_hover("main.rs", Some("fn main()"));

    let view = session.hover_view().expect("the hover is shown");
    assert_eq!(view.lines, vec!["fn main()"]);
}

#[test]
fn a_hover_does_not_outlive_the_buffer_it_is_about() {
    // #123. `on_key` clearing the hover covers every way the user changes
    // buffer by hand, and looks like enough. It is not: choosing a file does
    // not open it -- the host reads it on a task and `opened` makes the new
    // buffer current whenever the read lands, with no key pressed at that
    // moment. So documentation for a struct in one file sits under another.
    let mut session = editing("struct Widget {}\n");
    session.set_path("main.rs");
    session.on_resize(40, 10);

    // Choose a file. Those keys clear the hover themselves, which is exactly
    // why the one that matters is shown *after* them: the gap is the stretch
    // between the choosing keystroke and the file arriving, and since #121 a
    // hover can appear in it without anybody pressing anything.
    chose(&mut session, "notes.md");

    session.show_hover("main.rs", Some("struct Widget"));
    assert!(session.hover_view().is_some(), "the hover is up");

    session.opened("notes.md", Opened::Read("# notes\n".to_string()));

    assert!(
        session.hover_view().is_none(),
        "and it went with the buffer it was about"
    );
}

#[test]
fn an_answer_to_a_key_is_not_drawn_under_a_file_it_is_not_about() {
    // #124, and the half #123 left. #123 records which buffer a hover is about
    // at the moment it is *shown* -- which for `SPC k k` is when the answer
    // comes back, not when the question was asked. So without this the stale
    // answer is stamped with the *new* buffer and passes the very check that
    // exists to catch it.
    let mut session = editing_a_file("struct Widget {}\n");

    press(&mut session, "\u{20}kk");
    let (asked, _) = session.pending_hover().expect("a question to ask");
    assert_eq!(asked, "main.rs");

    chose(&mut session, "notes.md");
    session.opened("notes.md", Opened::Read("# notes\n".to_string()));
    assert_eq!(session.path(), Some("notes.md"), "the buffer changed");

    assert_eq!(
        session.show_hover(&asked, Some("struct Widget")),
        Action::Nothing
    );
    assert!(session.hover_view().is_none());
}

#[test]
fn nothing_is_said_about_a_file_the_user_has_left_either() {
    // An empty answer becomes "No documentation for this." because a key asked
    // and silence reads as a key that did not work. Said about a file that is
    // no longer on screen it is noise, so the decision travels with the one
    // that drops the answer rather than staying in the arm that receives it.
    let mut session = editing_a_file("struct Widget {}\n");

    press(&mut session, "\u{20}kk");
    let (asked, _) = session.pending_hover().expect("a question to ask");

    chose(&mut session, "notes.md");
    session.opened("notes.md", Opened::Read("# notes\n".to_string()));

    assert_eq!(session.show_hover(&asked, None), Action::Nothing);
    assert!(session.echo().is_none());
}

#[test]
fn a_point_that_moved_does_not_lose_the_answer_a_key_asked_for() {
    // The asymmetry with the idle path, and a decision rather than an
    // oversight. #121 drops an idle answer whose point has moved because
    // nobody asked for it, so one about somewhere the point has left is an
    // interruption about nothing. Three keys were pressed for this one, and a
    // point that has since drifted a word does not make it unwanted.
    let mut session = editing_a_file("struct Widget {}\n");

    press(&mut session, "\u{20}kk");
    let (asked, _) = session.pending_hover().expect("a question to ask");
    press(&mut session, "w"); // the point moves while the server thinks

    assert_eq!(
        session.show_hover(&asked, Some("struct Widget")),
        Action::Redraw
    );
    let view = session.hover_view().expect("a hover was shown");
    assert_eq!(view.lines, vec!["struct Widget"]);
}

#[test]
fn an_empty_answer_to_a_key_still_says_so() {
    let mut session = editing_a_file("struct Widget {}\n");

    press(&mut session, "\u{20}kk");
    let (asked, _) = session.pending_hover().expect("a question to ask");

    assert_eq!(session.show_hover(&asked, None), Action::Redraw);
    assert_eq!(session.echo(), Some("No documentation for this."));
}

#[test]
fn an_empty_hover_reserves_nothing() {
    let mut session = editing_a_file("alpha\n");
    session.show_hover("main.rs", Some(""));
    assert!(session.hover_view().is_none());
}

// --- documentation when the point rests (#121) -----------------------------

/// A session editing a file that has a name, which is the only kind of buffer a
/// hover can be about: a server is told about documents by URI, and
/// `lookup-documentation` refuses a buffer visiting no file.
fn editing_a_file(text: &str) -> Session {
    let mut session = editing(text);
    session.set_path("main.rs");
    session.on_resize(40, 10);
    session
}

/// Choose a file in the picker, stopping at the choice.
///
/// Stopping there is the point: choosing a file does not open it. The host
/// reads it on a task and `opened` makes the new buffer current whenever the
/// read lands, so the stretch between the two is where a hover can be on
/// screen with nothing about to clear it.
fn chose(session: &mut Session, path: &str) {
    session.set_recent_files(vec![path.to_string()]);
    session.on_key(Key::Ctrl('x'));
    session.on_key(Key::Ctrl('f'));
    session.on_key(Key::Char('<'));
    session.on_key(Key::Char('r'));
    assert_eq!(session.on_key(Key::Enter), Action::Open);
}

/// A served buffer with `hover-on-rest` turned on, which is the only shape in
/// which any of this happens at all.
fn resting(text: &str) -> Session {
    let mut session = editing(text);
    session.set_path("main.rs");
    session.set_options(Options {
        hover_on_rest: true,
        ..Options::default()
    });
    session.on_resize(80, 20);
    session
}

#[test]
fn a_point_resting_in_a_served_buffer_is_a_question_to_ask() {
    let mut session = resting("fn main() {}\n");
    press(&mut session, "w");

    let asked = session.resting_hover().expect("a question to ask");
    assert_eq!(asked.path, "main.rs");
    assert_eq!(asked.at, session.selection().pos());
}

#[test]
fn typing_asks_nothing_however_long_the_fingers_pause() {
    // Insert State is the one where the text changes under the point, so a
    // hover armed here would ask a question per character -- #121's first
    // decision. `show-with-cursor` means the cursor *resting*, and in Insert
    // State it never is.
    let mut session = resting("fn main() {}\n");
    session.on_key(Key::Char('s'));
    assert_eq!(session.state(), State::Insert);

    assert!(session.resting_hover().is_none());
}

#[test]
fn an_unread_message_is_not_painted_over_by_a_hover_nobody_asked_for() {
    // `Bottom`'s priority order puts a hover above an ordinary message because
    // a hover is the answer to a key just pressed. An idle one is not: the
    // message arrived first and has not been read, and it goes on the next key
    // anyway. So the question is not *asked* while one is on screen, rather
    // than asked and then refused a place to land -- a server troubled for an
    // answer with nowhere to go is the same regression one row further back.
    let mut session = resting("fn main() {}\n");
    press(&mut session, "w");
    session.say("Wrote main.rs");

    assert!(session.resting_hover().is_none());
}

#[test]
fn once_the_message_is_gone_the_point_is_resting_again() {
    let mut session = resting("fn main() {}\n");
    session.say("Wrote main.rs");
    press(&mut session, "w"); // any key clears the Echo Area

    assert!(session.resting_hover().is_some());
}

#[test]
fn beacon_state_has_no_single_point_to_ask_about() {
    // One point per line, and a hover for one of them is a hover for the wrong
    // one. The same guard that keeps Insert State quiet.
    let mut session = resting("one\ntwo\nthree\nfour\n");
    press(&mut session, "lll"); // a three-line selection
    press(&mut session, "G"); // grab it
    press(&mut session, "h"); // move, landing inside the Grab
    assert_eq!(session.state(), State::Beacon);

    assert!(session.resting_hover().is_none());
}

#[test]
fn a_key_sequence_in_flight_is_not_a_resting_point() {
    let mut session = resting("fn main() {}\n");
    press(&mut session, "\u{20}"); // `SPC`: Keypad State, menu pending
    assert_eq!(session.state(), State::Keypad);

    assert!(session.resting_hover().is_none());
}

#[test]
fn a_widget_that_owns_the_bottom_of_the_frame_keeps_it() {
    let mut session = resting("fn main() {}\n");
    press(&mut session, "\u{20}."); // Doom's `SPC .`, the buffer picker
    assert!(session.narrowing().is_some(), "the widget is open");
    assert_eq!(
        session.state(),
        State::Normal,
        "so the widget is what refuses below, not the State"
    );

    assert!(session.resting_hover().is_none());
}

#[test]
fn a_buffer_no_language_server_serves_asks_nobody_anything() {
    let mut session = editing("# notes\n");
    session.set_path("notes.md");
    session.set_options(Options {
        hover_on_rest: true,
        ..Options::default()
    });

    assert!(session.resting_hover().is_none());
}

#[test]
fn left_alone_the_editor_is_the_one_that_waits_to_be_asked() {
    // The default. `lsp-ui-doc-show-with-cursor` is nil in `lsp-ui` and Doom
    // does not change it, so an editor nobody has configured never speaks to a
    // server except when a key asks it to.
    let mut session = editing("fn main() {}\n");
    session.set_path("main.rs");
    assert!(!session.served().is_empty(), "a server would serve it");

    assert!(session.resting_hover().is_none());
}

#[test]
fn one_resting_point_is_asked_about_once() {
    // A point resting where the server has nothing to say -- whitespace, a
    // comment -- shows nothing, and so leaves nothing on screen to say it was
    // already asked. Without a latch it would be asked again every
    // `hover-delay` for as long as nobody touched the keyboard.
    let mut session = resting("fn main() {}\n");
    press(&mut session, "w");

    let first = session.ask_at_rest().expect("a question to ask");
    assert!(
        session.resting_hover().is_none(),
        "asked once, and not again while the point has not moved"
    );

    press(&mut session, "w");
    let second = session
        .ask_at_rest()
        .expect("the point moved, so ask again");
    assert_ne!(first.at, second.at);
}

#[test]
fn an_answer_about_where_the_point_still_is_reaches_the_echo_area() {
    let mut session = resting("fn main() {}\n");
    press(&mut session, "w");
    let asked = session.ask_at_rest().expect("a question to ask");

    assert_eq!(
        session.show_hover_at(&asked, Some("fn main()")),
        Action::Redraw
    );
    let view = session.hover_view().expect("a hover was shown");
    assert_eq!(view.lines, vec!["fn main()"]);
}

#[test]
fn an_answer_about_a_point_the_user_has_left_is_dropped() {
    // #121's second decision. A hover is keyed to a *position*, and the point
    // moves without the buffer changing, so there is no revision to catch this
    // -- the position is the half of the check that does the work.
    let mut session = resting("fn main() {}\n");
    press(&mut session, "w");
    let asked = session.ask_at_rest().expect("a question to ask");

    press(&mut session, "w"); // the point moves while the server thinks

    assert_eq!(
        session.show_hover_at(&asked, Some("fn main()")),
        Action::Nothing
    );
    assert!(session.hover_view().is_none());
}

#[test]
fn an_answer_about_text_that_has_since_changed_is_dropped() {
    // The other half. An edit moves what is *at* an offset without moving the
    // point off it, so a position that still matches can still be a different
    // symbol -- `apply_reparse`'s own mechanism, carried over.
    let mut session = resting("fn main() {}\n");
    press(&mut session, "w");
    let stale = session.revision();
    press(&mut session, "iX");
    press(&mut session, "\x1b");
    assert_ne!(session.revision(), stale, "the edit moved the revision on");

    let asked = meowr_app::Resting {
        path: "main.rs".to_string(),
        at: session.selection().pos(),
        revision: stale,
    };

    assert_eq!(
        session.show_hover_at(&asked, Some("fn main()")),
        Action::Nothing
    );
    assert!(session.hover_view().is_none());
}

#[test]
fn an_answer_about_a_buffer_that_is_no_longer_on_screen_is_dropped() {
    let mut session = resting("fn main() {}\n");
    press(&mut session, "w");
    let asked = session.ask_at_rest().expect("a question to ask");

    session.open(Buffer::scratch()); // `SPC b b` while the server thinks
    session.set_path("other.rs");
    session.loaded(Document::from_text("fn other() {}\n"));

    assert_eq!(
        session.show_hover_at(&asked, Some("fn main()")),
        Action::Nothing
    );
    assert!(session.hover_view().is_none());
}

#[test]
fn an_answer_that_arrives_behind_a_message_does_not_paint_over_it() {
    // The Echo Area check is not only about whether to *ask*: a message can
    // arrive in the second the server spends thinking, and an idle hover
    // landing on top of an unread refusal is the regression #121 names.
    let mut session = resting("fn main() {}\n");
    press(&mut session, "w");
    let asked = session.ask_at_rest().expect("a question to ask");

    session.say("Buffer is read-only.");

    assert_eq!(
        session.show_hover_at(&asked, Some("fn main()")),
        Action::Nothing
    );
    assert_eq!(session.echo(), Some("Buffer is read-only."));
}

#[test]
fn nothing_to_say_about_a_resting_point_is_said_by_saying_nothing() {
    // `SPC k k` says "No documentation for this." because a key was pressed
    // and silence after a key reads as a key that did not work. Nobody pressed
    // anything here, so a message would be the editor interrupting.
    let mut session = resting("fn main() {}\n");
    press(&mut session, "w");
    let asked = session.ask_at_rest().expect("a question to ask");

    assert_eq!(session.show_hover_at(&asked, None), Action::Nothing);
    assert!(session.echo().is_none());
    assert!(session.hover_view().is_none());
}

#[test]
fn a_key_that_leaves_the_point_where_it_was_asks_again() {
    // The latch is against an *idle* editor asking one question over and over,
    // not against a user who pressed something. `lsp-ui-doc` hides on a
    // command and comes back once the idle timer runs again; so does this, and
    // the debounce means at most one question per `hover-delay` either way.
    let mut session = resting("fn main() {}\n");
    press(&mut session, "w");
    let first = session.ask_at_rest().expect("a question to ask");

    session.on_key(Key::Esc); // cancels the selection; the point stays

    let again = session.resting_hover().expect("and it is asked again");
    assert_eq!(first.at, again.at);
}

#[test]
fn an_answer_that_lands_in_insert_state_is_dropped() {
    // Typing begins in the second the server spends thinking. The point and
    // the revision may both still match -- entering Insert State is not an
    // edit -- so this is the State half of the check doing the work on its
    // own, and a hover flashing over somebody's typing is what it stops.
    let mut session = resting("fn main() {}\n");
    press(&mut session, "w");
    session.on_key(Key::Char('s'));
    assert_eq!(session.state(), State::Insert);

    let asked = meowr_app::Resting {
        path: "main.rs".to_string(),
        at: session.selection().pos(),
        revision: session.revision(),
    };

    assert_eq!(
        session.show_hover_at(&asked, Some("fn main()")),
        Action::Nothing
    );
    assert!(session.hover_view().is_none());
}

#[test]
fn a_second_set_of_diagnostics_replaces_the_first_rather_than_appending() {
    // #112's finding is that a server sends several publishes for one URI —
    // often an empty one first, and once a third clearing it again. Appending
    // would leave a squiggle under text the server has stopped complaining
    // about, and the count climbing every second.
    let mut session = editing("alpha beta\n");
    let revision = session.revision();
    let id = session.current_buffer_id();

    session.set_diagnostics(id, revision, vec![diagnostic(0..5, "first")]);
    session.set_diagnostics(id, revision, vec![diagnostic(6..10, "second")]);
    assert_eq!(session.diagnostics().len(), 1);
    assert_eq!(session.diagnostics()[0].message, "second");

    // And the clearing publish clears, rather than being ignored as empty.
    session.set_diagnostics(id, revision, Vec::new());
    assert!(session.diagnostics().is_empty());
}

// --- language servers (#119) ----------------------------------------------

#[test]
fn a_served_buffer_is_found_past_a_killed_one() {
    // The bug this test exists for: iterating the buffers, filtering to the real
    // ones and *then* enumerating gives an index into the filtered sequence,
    // while a `BufferId` indexes the whole list. `entomb` leaves a tombstone
    // **in place**, so one killed slot before a Rust file shifted every later
    // id — the lookup asked about the tombstone, got `None`, and the host
    // concluded nothing needed serving and dropped a live connection out from
    // under a file still on screen.
    let mut session = editing("fn main() {}\n");
    session.set_path("first.rs");
    press(&mut session, "\x20bk"); // kill it; the slot becomes a tombstone

    let second = session.open(Buffer::scratch());
    session.set_path("second.rs");
    session.loaded(Document::from_text("fn other() {}\n"));

    let served = session.served();
    assert_eq!(served.len(), 1, "one real Rust buffer: {served:?}");
    assert_eq!(served[0].path, "second.rs");
    assert_eq!(served[0].language, "rust");
    assert_eq!(served[0].server.program, "rust-analyzer");
    assert_eq!(
        served[0].id, second,
        "the id must index the whole buffer list, tombstone included"
    );
}

#[test]
fn a_buffer_whose_language_has_no_server_is_not_served() {
    let mut session = editing("# notes\n");
    session.set_path("notes.md");
    assert!(
        session.served().is_empty(),
        "nothing serves Markdown, and that is an ordinary answer"
    );
}

// --- a Place, and the preview that needs a file read (#133) --------------

#[test]
fn the_two_previews_that_need_a_file_read_say_which_file() {
    // The host asks this and `previewed` checks the answer against what it was
    // handed. One spelling, because two is how a preview gets loaded into a
    // slot that was waiting for a different file.
    use meowr_app::narrow::Preview;
    use meowr_domain::Place;

    assert_eq!(
        Preview::File("src/lib.rs".into()).path(),
        Some("src/lib.rs")
    );
    assert_eq!(
        Preview::At(Place::new("src/narrow.rs", 412, 8)).path(),
        Some("src/narrow.rs"),
        "a Place needs the same read; only what happens after differs"
    );
    // Neither of these is a file the host has to go and get.
    assert_eq!(Preview::Line(9).path(), None);
    assert_eq!(Preview::Buffer(BufferId(0)).path(), None);
}

// --- the Trail (#134) ----------------------------------------------------

#[test]
fn going_back_with_nowhere_to_go_says_so() {
    // Not silence. #69's clause: anything the session decides reaches a frame.
    let mut session = editing_a_file("alpha\nbeta\ngamma\n");
    assert_eq!(session.trail_back_for_test(), Action::Redraw);
    assert_eq!(session.echo(), Some("The Trail is empty."));
}

#[test]
fn a_jump_remembers_where_it_left_and_going_back_returns() {
    // The ordinary case, and the one that makes every other row of ADR-0011
    // safe to ship: jumping with no way back is a worse feature than no jump.
    let mut session = editing_a_file("alpha\nbeta\ngamma\n");
    let start = session.point();

    assert_eq!(
        session.jump_to(meowr_domain::Place::new("main.rs", 2, 1)),
        Action::Redraw,
        "the file is already open, so no read is needed"
    );
    assert_ne!(session.point(), start);

    assert_eq!(session.trail_back_for_test(), Action::Redraw);
    assert_eq!(
        session.point(),
        start,
        "back where it started, column and all"
    );
}

#[test]
fn a_jump_within_one_file_pushes_the_trail_too() {
    // "Only across files" is a rule you would have to explain, and `SPC k j`
    // moves far enough inside one file to want the same undo.
    let mut session = editing_a_file("a\nb\nc\nd\ne\n");
    session.jump_to(meowr_domain::Place::new("main.rs", 4, 0));
    assert_eq!(session.trail_back_for_test(), Action::Redraw);
    assert_eq!(session.point(), 0);
}

#[test]
fn going_back_to_a_file_that_is_gone_says_so_and_spends_the_entry() {
    // ADR-0011 refuses rather than skipping to the next live entry: "back did
    // nothing" and "back went somewhere unexpected" are indistinguishable from
    // outside, and the second is the worse of the two.
    let mut session = editing_a_file("alpha\nbeta\n");
    // Jump away to another file, so the Trail holds `main.rs` …
    assert_eq!(
        session.jump_to(meowr_domain::Place::new("other.rs", 0, 0)),
        Action::Open
    );
    session.opened("other.rs", Opened::Read("one\ntwo\n".into()));
    // … and then that buffer is gone.
    session.kill_named_for_test("main.rs");

    assert_eq!(session.trail_back_for_test(), Action::Open);
    assert_eq!(session.pending_open(), Some("main.rs"));
    session.opened("main.rs", Opened::Failed("No such file or directory".into()));

    assert_eq!(session.echo(), Some("main.rs is no longer there."));
    assert_eq!(
        session.trail_back_for_test(),
        Action::Redraw,
        "and the entry is spent, so a second press finds nothing"
    );
    assert_eq!(session.echo(), Some("The Trail is empty."));
}

#[test]
fn the_trail_key_is_one_meow_leaves_unbound() {
    // Ring 3, the shape `q` -> `quit-window` already took: a key `meow-normal`
    // binds nothing on takes nothing back, so the differential harness has
    // nothing to compare and there is no divergence to record.
    let session = editing_a_file("x\n");
    assert_eq!(
        session.keys_for(State::Normal, "trail-back"),
        Some(vec![Key::Char('`')])
    );
}

// --- one question's answer, in the picker (#135) -------------------------

fn place(path: &str, line: usize) -> meowr_domain::Place {
    meowr_domain::Place::new(path, line, 0)
}

#[test]
fn a_result_list_is_a_picker_with_nothing_to_narrow_to() {
    // The four sources are standing facts about the session; this is the answer
    // to a question just asked. A source that could be narrowed back to would
    // be stale results wearing a live key.
    let mut session = editing_a_file("alpha\n");
    session.open_results(vec![place("a.rs", 0), place("b.rs", 4)]);

    let widget = session.narrowing().expect("open");
    assert!(widget.sources().is_empty(), "`<` has nothing to offer");
    assert_eq!(widget.narrowed(), None);
    assert_eq!(widget.matches().len(), 2);
}

#[test]
fn a_row_shows_the_line_where_the_source_had_one_and_the_place_where_it_did_not() {
    // Grep hands back the matched line; a language server hands back positions
    // and nothing else. A references list of bare paths is unreadable, and a
    // `None` here is a fact about LSP rather than a gap.
    let mut session = editing_a_file("alpha\n");
    session.open_results(vec![
        place("src/lib.rs", 411).showing("    let widget = self.narrow;"),
        place("src/narrow.rs", 87),
    ]);

    let widget = session.narrowing().expect("open");
    let row = |i: usize| {
        let c = widget.candidate(widget.matches()[i].index);
        (c.text.clone(), c.annotation.clone())
    };
    assert_eq!(
        row(0),
        (
            "let widget = self.narrow;".to_string(),
            Some("src/lib.rs:412".to_string())
        ),
        "the line to match on, the place to read"
    );
    assert_eq!(
        row(1),
        ("src/narrow.rs:88".to_string(), None),
        "nothing to show but where it is"
    );
}

#[test]
fn previewing_a_result_shows_its_line_and_not_the_files_head() {
    // The whole reason `Preview` needed a fourth variant: you picked the row
    // for the line it named, so `File`'s head is the wrong thing to show.
    use meowr_app::narrow::Preview;
    let mut session = editing_a_file("alpha\n");
    session.open_results(vec![place("other.rs", 2)]);

    assert_eq!(
        session.preview_target(),
        Some(Preview::At(place("other.rs", 2))),
    );
    session.previewed("other.rs", Ok("one\ntwo\nthree\nfour\n".into()));
    assert_eq!(session.point(), 8, "at `three`, not at the head");
}

#[test]
fn accepting_a_result_jumps_there_and_the_trail_brings_you_back() {
    // End to end, through the host's read: the path #133 wired and could not
    // exercise, because nothing constructed a Place until this slice.
    let mut session = editing_a_file("alpha\nbeta\n");
    let start = session.point();
    session.open_results(vec![place("other.rs", 2)]);

    assert_eq!(
        session.accept_narrowing(),
        Action::Open,
        "the file is not open"
    );
    assert_eq!(session.pending_open(), Some("other.rs"));
    session.opened("other.rs", Opened::Read("one\ntwo\nthree\n".into()));
    assert_eq!(session.point(), 8, "landed on the line the row named");

    assert_eq!(session.trail_back_for_test(), Action::Redraw);
    assert_eq!(session.point(), start, "and back where the jump left");
}

// --- the LSP rows that produce a list (#137) -----------------------------

#[test]
fn the_references_row_asks_the_host_and_says_what_about() {
    // `SPC k j` and `k J` were built alongside this and handed back: an empty
    // `workspace/symbol` query returns a truncated, dependency-dominated list
    // from rust-analyzer, so a symbol picker is missing its own workspace's
    // symbols. Held as reservations until the picker can ask again as you type.
    use meowr_app::LookupKind;
    let mut session = editing_a_file("struct Widget {}\n");

    session.on_key(Key::Char(' '));
    session.on_key(Key::Char('k'));
    assert_eq!(session.on_key(Key::Char('D')), Action::Lookup);

    let asked = session.pending_lookup().expect("a question to put");
    assert_eq!(asked.kind, LookupKind::References);
    assert_eq!(asked.path, "main.rs");
    assert_eq!(session.pending_lookup(), None, "drained, not read twice");
}

// --- the LSP rows that look for one place (#146) --------------------------

#[test]
fn the_three_rows_that_look_for_one_place_each_ask_their_own_question() {
    // #146. One call shape with three questions, which is #137's argument for
    // `LookupKind` being a type rather than three `Action` variants.
    use meowr_app::LookupKind;
    for (key, kind) in [
        ('d', LookupKind::Definition),
        ('t', LookupKind::TypeDefinition),
        ('i', LookupKind::Implementations),
    ] {
        let mut session = editing_a_file("struct Widget {}\n");
        press(&mut session, " k");
        assert_eq!(
            session.on_key(Key::Char(key)),
            Action::Lookup,
            "SPC k {key}"
        );
        let asked = session.pending_lookup().expect("a question to put");
        assert_eq!(asked.kind, kind, "SPC k {key}");
        assert_eq!(asked.path, "main.rs");
    }
}

#[test]
fn one_place_found_is_a_jump_rather_than_a_picker_of_one() {
    // `consult-xref`, which Doom sets as both `xref-show-xrefs-function` and
    // `xref-show-definitions-function`: `(if (cdr candidates) (consult--read
    // ...) (car candidates))`. A one-row picker in front of a definition is a
    // keypress spent confirming the only answer -- and `+lookup/references`
    // goes through the same function, so `k D` jumps on one as well (#146).
    for key in ['d', 't', 'i', 'D'] {
        let mut session = editing_a_file("struct Widget {}\nfn main() {}\n");
        let start = session.point();
        press(&mut session, &format!(" k{key}"));
        let asked = session.pending_lookup().expect("a question to put");

        assert_eq!(
            session.found(&asked.path, vec![place("main.rs", 1)]),
            Action::Redraw,
            "SPC k {key}"
        );
        assert!(session.narrowing().is_none(), "SPC k {key}: no picker");
        assert_eq!(session.point(), 17, "SPC k {key}: on the line it named");
        assert_eq!(session.trail_back_for_test(), Action::Redraw);
        assert_eq!(
            session.point(),
            start,
            "SPC k {key}: and the Trail goes back"
        );
    }
}

#[test]
fn one_place_in_another_file_is_opened_and_landed_on() {
    // The usual case for a definition, and the one a same-file test cannot
    // reach: the jump is an `Action::Open`, so whoever called `found` has to
    // act on what it returns. The host discarded it until #146's pty run found
    // `SPC k d` doing nothing and saying nothing.
    let mut session = editing_a_file("let c = Circle {};\n");
    press(&mut session, " kd");
    let asked = session.pending_lookup().expect("a question to put");

    let start = session.point();
    assert_eq!(
        session.found(&asked.path, vec![place("shapes.rs", 1)]),
        Action::Open
    );
    session.opened("shapes.rs", Opened::Read("mod a;\nstruct Circle {}\n".into()));
    assert_eq!(session.point(), 7, "on the line the server named");

    session.trail_back_for_test();
    assert_eq!(session.path(), Some("main.rs"), "and the Trail goes back");
    assert_eq!(session.point(), start);
}

#[test]
fn an_answer_that_lands_while_a_picker_is_open_is_dropped() {
    // `searched`'s rule, and for its reason: a widget opened since the question
    // was asked owns the Bottom (ADR-0004), and an answer that opened a picker
    // over it -- or, since #146, jumped behind it -- would take the keyboard
    // from something the user is in the middle of. The jump would also spend
    // the open picker's `from`, so accepting from it would leave nothing on
    // the Trail to come back to.
    let mut session = editing_a_file("struct Widget {}\nfn main() {}\n");
    press(&mut session, " kd");
    let asked = session.pending_lookup().expect("a question to put");
    let start = session.point();
    press(&mut session, " .");
    assert!(session.narrowing().is_some(), "a picker opened meanwhile");

    assert_eq!(
        session.found(&asked.path, vec![place("main.rs", 1)]),
        Action::Nothing
    );
    assert!(session.narrowing().is_some(), "still the user's picker");
    assert_eq!(session.point(), start, "and nothing moved behind it");
}

// --- the LSP rows that produce a list, continued (#137) --------------------

#[test]
fn what_a_server_found_opens_the_picker_over_it() {
    let mut session = editing_a_file("struct Widget {}\n");
    session.on_key(Key::Char(' '));
    session.on_key(Key::Char('k'));
    session.on_key(Key::Char('D'));
    let asked = session.pending_lookup().expect("a question to put");

    assert_eq!(
        session.found(&asked.path, vec![place("a.rs", 3), place("b.rs", 7)]),
        Action::Redraw
    );
    let widget = session.narrowing().expect("the picker is open over them");
    assert_eq!(widget.matches().len(), 2);
    assert!(
        widget.sources().is_empty(),
        "one question's answer, not a source"
    );
}

#[test]
fn an_answer_about_a_buffer_that_is_no_longer_current_is_dropped() {
    // ADR-0009's arrival check, and the same one a key-asked hover makes: the
    // file changing is what makes the answer unwanted, not the point moving.
    let mut session = editing_a_file("struct Widget {}\n");
    session.on_key(Key::Char(' '));
    session.on_key(Key::Char('k'));
    session.on_key(Key::Char('D'));
    let asked = session.pending_lookup().expect("a question to put");

    session.opened("elsewhere.rs", Opened::Read("other\n".into()));
    session.set_path("elsewhere.rs");

    assert_eq!(
        session.found(&asked.path, vec![place("a.rs", 3)]),
        Action::Nothing,
        "about a file nobody is looking at"
    );
    assert!(session.narrowing().is_none(), "and no picker was opened");
}

#[test]
fn a_server_that_found_nothing_says_so_rather_than_opening_an_empty_picker() {
    // ADR-0010's distinction, borrowed: nothing found is an answer and deserves
    // saying, where a picker with no rows reads as a feature that did not run.
    let mut session = editing_a_file("struct Widget {}\n");
    session.on_key(Key::Char(' '));
    session.on_key(Key::Char('k'));
    session.on_key(Key::Char('D'));
    let asked = session.pending_lookup().expect("a question to put");

    assert_eq!(session.found(&asked.path, Vec::new()), Action::Redraw);
    assert_eq!(session.echo(), Some("Nothing found."));
    assert!(session.narrowing().is_none());
}

#[test]
fn a_buffer_with_no_file_has_nothing_to_look_anything_up_in() {
    // A server is told about documents by URI, and there is no URI for text
    // that is only in memory. The same refusal `lookup-documentation` makes.
    let mut session = editing("scratch\n");
    session.on_resize(40, 10);
    session.on_key(Key::Char(' '));
    session.on_key(Key::Char('k'));
    assert_eq!(session.on_key(Key::Char('D')), Action::Redraw);
    assert_eq!(session.echo(), Some("No file to look for references in."));
    assert_eq!(session.pending_lookup(), None, "and nothing was asked");
}

// --- `*diagnostics*`, and a row you can jump from (#136) ------------------

/// A served buffer carrying what a server has said about it.
///
/// `editing_a_file` names it `main.rs`, which is the Rust Language and the one
/// with a server declared — the distinction `SPC k x` refuses on.
fn diagnosed(text: &str, diagnostics: Vec<Diagnostic>) -> Session {
    let mut session = editing_a_file(text);
    let revision = session.revision();
    session.set_diagnostics(session.current_buffer_id(), revision, diagnostics);
    session
}

/// The text of the `*diagnostics*` buffer.
fn diagnostics_buffer(session: &Session) -> String {
    session
        .buffers()
        .iter()
        .find(|b| !b.is_killed() && b.name() == meowr_app::DIAGNOSTICS)
        .and_then(|b| b.document())
        .map(|doc| doc.text().to_string())
        .expect("`*diagnostics*` is open")
}

#[test]
fn spc_k_x_puts_this_buffers_diagnostics_in_a_buffer() {
    // ADR-0011's one exception: ten rows go to the picker and this one goes to
    // a generated buffer, because a diagnostic list is read top to bottom and
    // acted on rather than fuzzy-matched. It is the line Emacs draws itself —
    // `flycheck-list-errors` gets a buffer where `xref` gets the minibuffer.
    let mut session = diagnosed(
        "fn main() {\n    let x = wrong;\n}\n",
        vec![diagnostic(24..29, "cannot find value `wrong` in this scope")],
    );

    press(&mut session, " kx");

    let text = diagnostics_buffer(&session);
    assert!(
        text.contains("cannot find value `wrong` in this scope"),
        "what the server said: {text}"
    );
    assert!(text.contains("2:13"), "where it said it, one-based: {text}");
}

#[test]
fn return_on_a_row_goes_there_and_the_trail_brings_you_back() {
    // The jump key, and the thing meowr does not have: no buffer-local keymaps,
    // so this is one global command that means something where there are Places
    // (ADR-0011). A generated buffer visits no file, so what the Trail is given
    // is where the listing was made from -- otherwise `SPC k x`, a jump, and no
    // way back, which is the thing that ADR calls worse than no jump.
    let mut session = diagnosed(
        "fn main() {\n    let x = wrong;\n}\n",
        vec![diagnostic(24..29, "cannot find value `wrong` in this scope")],
    );
    session.set_selection(Selection::at(12));

    press(&mut session, " kx");
    press(&mut session, "nn"); // past the heading and its blank line, onto the row

    assert_eq!(session.on_key(Key::Enter), Action::Redraw);
    assert_eq!(session.path(), Some("main.rs"), "back in the file it named");
    assert_eq!(session.point(), 24, "at the diagnostic, not at the head");

    assert_eq!(session.trail_back_for_test(), Action::Redraw);
    assert_eq!(session.point(), 12, "and back where `SPC k x` was pressed");
}

#[test]
fn return_where_no_line_names_anywhere_does_nothing() {
    // Inert elsewhere, which is what makes one global command honest. `q` is
    // already exactly this: a key that means something in a buffer meowr wrote
    // and nothing in one you are editing.
    let mut session = editing_a_file("alpha\nbeta\n");
    let before = session.point();

    assert_eq!(session.on_key(Key::Enter), Action::Nothing);
    assert_eq!(session.point(), before, "nothing moved");
    session.trail_back_for_test();
    assert_eq!(
        session.echo(),
        Some("The Trail is empty."),
        "and nothing was remembered"
    );
}

#[test]
fn a_buffer_no_server_serves_refuses_and_opens_nothing() {
    // Empty is not one thing. This is not "no diagnostics": nobody has looked.
    let mut session = editing("# notes\n");
    session.set_path("notes.md");

    press(&mut session, " kx");

    assert_eq!(session.echo(), Some("No language server for this buffer."));
    // Set *and* reachable. ADR-0004 gives the Bottom to exactly one owner, and
    // #130's refusal passed a test on `echo()` alone while showing nothing on
    // screen, because a widget had the rows. The leader menu is that widget
    // here: three keys were pressed to get this sentence.
    assert_eq!(session.bottom(), meowr_view::Bottom::Echo);
    assert!(
        !session.buffers().iter().any(|b| b.name() == meowr_app::DIAGNOSTICS),
        "and nothing was opened to say so in"
    );
}

#[test]
fn a_server_that_has_not_answered_yet_refuses_rather_than_reporting_none() {
    // ADR-0010's finding, one subject over: `Ok(Vec::new())` from a subprocess
    // that failed is a silent fallback, and only a repository with nothing
    // tracked in it is really an empty list. An empty `*diagnostics*` for a
    // server that has not spoken is that same lie in a new place.
    let mut session = editing_a_file("fn main() {}\n");

    press(&mut session, " kx");

    assert_eq!(
        session.echo(),
        Some("The language server has not answered yet.")
    );
    assert_eq!(session.bottom(), meowr_view::Bottom::Echo, "and on screen");
    assert!(
        !session.buffers().iter().any(|b| b.name() == meowr_app::DIAGNOSTICS),
        "nobody asked, so there is nothing to show"
    );
}

#[test]
fn a_clean_buffer_gets_a_real_buffer_saying_so() {
    // The other side of the same line: a server *did* answer, and what it said
    // was nothing. That is a true answer and deserves somewhere to be read.
    let mut session = diagnosed("fn main() {}\n", Vec::new());

    press(&mut session, " kx");

    assert!(
        diagnostics_buffer(&session).contains("No diagnostics."),
        "the answer, not an empty list"
    );
    assert_eq!(
        session.echo(),
        Some("q to close."),
        "and nothing about visiting, because there is nothing to visit"
    );
}

#[test]
fn the_way_out_is_named_and_it_works() {
    // The report behind `quit-window` was as much about nothing saying which
    // key as about the key not existing, which is why the sentence is looked up
    // rather than written down.
    let mut session = diagnosed(
        "fn main() {\n    let x = wrong;\n}\n",
        vec![diagnostic(24..29, "no such value")],
    );

    press(&mut session, " kx");
    assert_eq!(session.echo(), Some("RET to visit, q to close."));
    assert_eq!(
        session.bottom(),
        meowr_view::Bottom::Echo,
        "the advice is no use if the leader menu still has the rows"
    );

    press(&mut session, "q");
    assert_eq!(session.path(), Some("main.rs"), "back where it came from");
    assert!(
        !session
            .buffers()
            .iter()
            .any(|b| !b.is_killed() && b.name() == meowr_app::DIAGNOSTICS),
        "and the buffer is gone rather than left standing"
    );
}

#[test]
fn asking_twice_regenerates_the_one_buffer() {
    // One buffer, refreshed. One per invocation accumulates buffers nobody
    // closes, which the picker then lists with nothing to choose between them
    // but which is stale -- the argument `*bindings*` already made.
    let mut session = diagnosed("fn main() {}\n", vec![diagnostic(0..2, "first")]);

    press(&mut session, " kx");
    press(&mut session, "q");
    let revision = session.revision();
    session.set_diagnostics(
        session.current_buffer_id(),
        revision,
        vec![diagnostic(3..7, "second")],
    );
    press(&mut session, " kx");

    let text = diagnostics_buffer(&session);
    assert!(text.contains("second"), "what the server says now: {text}");
    assert!(!text.contains("first"), "and not what it said before: {text}");
    assert_eq!(
        session
            .buffers()
            .iter()
            .filter(|b| !b.is_killed() && b.name() == meowr_app::DIAGNOSTICS)
            .count(),
        1,
        "one buffer, not one per asking"
    );
}

#[test]
fn a_message_that_arrives_on_three_lines_is_listed_on_one() {
    // rust-analyzer's messages contain newlines -- the candidate impls of a
    // trait arrive one per line. A row here *is* a line, so a message broken
    // over three would leave two rows naming nowhere and put the third's Place
    // under a line it says nothing about.
    let mut session = diagnosed(
        "fn main() {\n    let x = wrong;\n}\n",
        vec![diagnostic(
            24..29,
            "expected one of:\n  - `Widget`\n  - `Gadget`",
        )],
    );

    press(&mut session, " kx");

    let text = diagnostics_buffer(&session);
    assert!(
        text.contains("expected one of:   - `Widget`   - `Gadget`"),
        "the whole of what it said, on one line: {text}"
    );
    press(&mut session, "nn");
    assert_eq!(session.on_key(Key::Enter), Action::Redraw, "and the row still names its place");
    assert_eq!(session.point(), 24);
}

#[test]
fn the_heading_names_the_buffer_the_way_the_modeline_does() {
    // Found in a real terminal, which is the only place it was visible: meowr
    // opened on an absolute path put a heading wider than the frame. The fix is
    // not a second rule for shortening a path -- there is already one on screen
    // two rows below, and two rules is how the heading and the modeline come to
    // call the same buffer different things.
    let mut session = editing("fn main() {}\n");
    session.set_path("/home/user/tools/meow/crates/meowr-app/src/lib.rs");
    session.on_resize(40, 10);
    let revision = session.revision();
    session.set_diagnostics(session.current_buffer_id(), revision, Vec::new());

    press(&mut session, " kx");

    assert_eq!(
        diagnostics_buffer(&session).lines().next(),
        Some("DIAGNOSTICS  meowr-app/src/lib.rs"),
        "the last three segments, which is `uniquify-buffer-name-style 'forward`"
    );
}

// --- the four rows that search files (#139) -------------------------------

/// What the line read is asking, as the widget shows it.
fn asking(session: &mut Session) -> String {
    session.narrow_view().expect("a read is open").prompt
}

/// Type a line into an open read and accept it.
fn answer(session: &mut Session, line: &str) -> Action {
    for ch in line.chars() {
        session.on_key(Key::Char(ch));
    }
    session.on_key(Key::Enter)
}

#[test]
fn spc_s_d_asks_for_a_pattern_and_searches_this_directory() {
    // Doom's `+default/search-cwd`. No Project needed: the directory is the
    // one the buffer's file is in, which is a fact about the path alone.
    let mut session = editing_a_file("fn main() {}\n");

    press(&mut session, " sd");

    assert_eq!(asking(&mut session), "Search this directory: ");

    assert_eq!(answer(&mut session, "widget"), Action::Search);
    let asked = session.pending_search().expect("a search to run");
    assert_eq!(asked.pattern, meowr_domain::Pattern::Regex("widget".into()));
    assert_eq!(asked.subject, Subject::Here("main.rs".into()));
    assert_eq!(session.pending_search(), None, "drained, not read twice");
}

#[test]
fn what_a_search_found_opens_the_picker_over_it() {
    // The sourceless picker #135 built, over the Places #138's searcher hands
    // back. A row carries its matched line, which is the half grep gives for
    // free -- so the list is readable without previewing every row of it.
    let mut session = editing_a_file("fn main() {}\n");
    press(&mut session, " sd");
    answer(&mut session, "widget");
    session.pending_search().expect("a search to run");

    assert_eq!(
        session.searched(Ok(vec![
            place("src/lib.rs", 411).showing("    let widget = self.narrow;"),
            place("src/narrow.rs", 87).showing("let widget = Narrow::reading();"),
        ])),
        Action::Redraw
    );

    let widget = session.narrowing().expect("the picker is open");
    assert!(widget.sources().is_empty(), "`<` has nothing to offer");
    assert_eq!(widget.matches().len(), 2);
    let row = widget.candidate(widget.matches()[0].index);
    assert_eq!(row.text, "let widget = self.narrow;");
    assert_eq!(row.annotation.as_deref(), Some("src/lib.rs:412"));
}

#[test]
fn a_search_that_found_nothing_says_so_rather_than_opening_an_empty_picker() {
    // ADR-0010's distinction, which `found` already borrows for a server that
    // returned no references: an empty picker reads as a feature that did not
    // run, where "nothing found" is an answer.
    let mut session = editing_a_file("fn main() {}\n");
    press(&mut session, " sd");
    answer(&mut session, "widget");

    assert_eq!(session.searched(Ok(Vec::new())), Action::Redraw);

    assert!(session.narrowing().is_none(), "no picker with nothing in it");
    assert_eq!(session.echo(), Some("Nothing found."));
}

#[test]
fn a_search_that_could_not_run_says_what_was_missing() {
    // ADR-0010: the host writes this sentence because it is what knows where it
    // looked, and the session says it rather than swallowing it. `SPC s p`
    // outside a repository is the row this exists for.
    let mut session = editing_a_file("fn main() {}\n");
    press(&mut session, " sd");
    answer(&mut session, "widget");

    assert_eq!(
        session.searched(Err("/tmp/notes is in no git repository".into())),
        Action::Redraw
    );

    assert!(session.narrowing().is_none());
    assert_eq!(session.echo(), Some("/tmp/notes is in no git repository"));
    assert_eq!(session.bottom(), meowr_view::Bottom::Echo, "and on screen");
}

#[test]
fn accepting_a_search_result_jumps_there_and_the_trail_brings_you_back() {
    let mut session = editing_a_file("alpha\nbeta\n");
    let start = session.point();
    press(&mut session, " sd");
    answer(&mut session, "widget");
    // One hit and still a picker: `consult-ripgrep` has no single-candidate
    // branch, where `consult-xref` does and the lookups follow it (#146).
    session.searched(Ok(vec![place("other.rs", 2)]));

    assert_eq!(session.accept_narrowing(), Action::Open);
    session.opened("other.rs", Opened::Read("one\ntwo\nthree\n".into()));
    assert_eq!(session.point(), 8, "landed on the line the row named");

    assert_eq!(session.trail_back_for_test(), Action::Redraw);
    assert_eq!(session.point(), start, "and back where the search was asked");
}

#[test]
fn spc_s_p_searches_the_project_the_buffer_is_in() {
    // Doom's `+default/search-project`. The session names the subject and does
    // not resolve it: which directory the Project *is* is a `git rev-parse`
    // away, and ADR-0010 puts that question and its refusal on the host.
    let mut session = editing_a_file("fn main() {}\n");

    press(&mut session, " sp");
    assert_eq!(asking(&mut session), "Search the project: ");

    assert_eq!(answer(&mut session, "widget"), Action::Search);
    let asked = session.pending_search().expect("a search to run");
    assert_eq!(asked.subject, Subject::Project("main.rs".into()));
}

#[test]
fn spc_s_capital_d_asks_which_directory_and_then_what_for() {
    // Doom's `+default/search-other-cwd`, and the one row that asks twice. The
    // directory travels in the second read rather than being remembered
    // somewhere beside it, so there is no pair to disagree.
    let mut session = editing_a_file("fn main() {}\n");

    press(&mut session, " sD");
    assert_eq!(asking(&mut session), "Search which directory: ");
    answer(&mut session, "/tmp/elsewhere");

    assert_eq!(asking(&mut session), "Search that directory: ");
    assert_eq!(answer(&mut session, "widget"), Action::Search);

    let asked = session.pending_search().expect("a search to run");
    assert_eq!(asked.subject, Subject::Directory("/tmp/elsewhere".into()));
    assert_eq!(asked.pattern, meowr_domain::Pattern::Regex("widget".into()));
}

#[test]
fn spc_s_dot_searches_for_the_symbol_at_the_point_without_asking() {
    // `+default/search-project-for-symbol-at-point`. No prompt, because the
    // question is already answered by where the point is -- and the symbol is
    // searched as **text**: meowr's `symbol` Thing is Emacs's `w` and `_`
    // classes together, and `_` is exactly `&*+-/<=>_|`, so a symbol can be
    // `*`, which as a regex is a parse error rather than a search.
    let mut session = editing_a_file("fn main() { widget }\n");
    session.set_selection(Selection::at(12));

    press(&mut session, " s");
    assert_eq!(session.on_key(Key::Char('.')), Action::Search);

    let asked = session.pending_search().expect("a search to run");
    assert_eq!(asked.pattern, meowr_domain::Pattern::Literal("widget".into()));
    assert_eq!(asked.subject, Subject::Project("main.rs".into()));
    assert!(session.narrowing().is_none(), "and nothing was asked");
}

// --- one buffer per file (#150) --------------------------------------------

#[test]
fn choosing_a_file_that_is_already_open_goes_to_its_buffer() {
    // `open_chosen` promoted the preview or asked for a read, and asked no
    // question about what was already open -- so `SPC SPC` on the file being
    // edited opened it a second time, spelled identically, and an edit in one
    // was not in the other. Found by #150's pty run, which was looking for the
    // spelling bug and found this one standing beside it.
    let mut session = editing_a_file("fn main() {}\n");
    session.set_project(Some("main.rs".into()), Ok(vec!["main.rs".into()]));
    let real = |session: &Session| session.buffers().iter().filter(|b| b.is_real()).count();
    assert_eq!(real(&session), 1);

    press(&mut session, "  ");
    assert_eq!(session.accept_narrowing(), Action::Redraw, "nothing to read");
    assert_eq!(real(&session), 1, "still one buffer for main.rs");
    assert_eq!(session.path(), Some("main.rs"));
}

#[test]
fn a_preview_of_an_open_file_is_not_promoted_to_a_second_buffer() {
    // The other half of `open_chosen`: arrowing onto the row previews it by
    // reading the file into the preview slot, and accepting promoted that slot
    // to a real buffer beside the one already open.
    let mut session = editing_a_file("fn main() {}\n");
    session.set_project(Some("main.rs".into()), Ok(vec!["main.rs".into()]));
    let real = |session: &Session| session.buffers().iter().filter(|b| b.is_real()).count();

    press(&mut session, "  ");
    session.previewed("main.rs", Ok("fn main() {}\n".into()));
    session.accept_narrowing();

    assert_eq!(real(&session), 1, "still one buffer for main.rs");
    assert_eq!(session.path(), Some("main.rs"));
}

// --- `SPC f F`: every file under the buffer's directory (#149) -------------

#[test]
fn spc_f_capital_f_asks_for_the_files_under_the_buffer_s_directory() {
    // `+default/find-file-under-here`. The session names the buffer and the
    // host lists its directory, for the reason a search names its subject:
    // resolving a directory is I/O, and the session does none.
    let mut session = editing_a_file("fn main() {}\n");
    press(&mut session, " f");
    assert_eq!(session.on_key(Key::Char('F')), Action::Files);
    assert_eq!(
        session.pending_files(),
        Some(Files::UnderThisBuffer("main.rs".to_string()))
    );
    assert_eq!(session.pending_files(), None, "drained, not read twice");
}

#[test]
fn spc_f_capital_f_in_a_buffer_with_no_file_refuses() {
    let mut session = editing("scratch\n");
    press(&mut session, " fF");
    assert_eq!(session.pending_files(), None);
    assert_eq!(session.echo(), Some("This buffer has no file."));
}

#[test]
fn the_files_under_here_are_a_picker_that_is_not_the_project() {
    // One question's answer, not a standing source: `<p` showing the buffer's
    // directory as "the project" would be #120's bug, a listing rooted in one
    // place read as another. So there is nothing to narrow to.
    let mut session = editing_a_file("fn main() {}\n");
    press(&mut session, " fF");
    let about = session.pending_files().expect("a listing to make");

    let listed = Ok(vec!["src/a.rs".to_string(), "src/b.rs".to_string()]);
    assert_eq!(session.files_listed(&about, listed), Action::Redraw);
    let widget = session.narrowing().expect("the picker opened over them");
    assert!(widget.sources().is_empty(), "nothing for `<` to offer");
    assert_eq!(widget.matches().len(), 2);

    assert_eq!(session.accept_narrowing(), Action::Open);
    assert_eq!(session.pending_open(), Some("src/a.rs"), "a row is a file");
}

#[test]
fn a_listing_that_lands_elsewhere_is_dropped_and_a_refusal_is_said() {
    // `searched`'s two guards and `found`'s one: a widget opened meanwhile owns
    // the Bottom, and an answer about a buffer nobody is looking at is about
    // nothing. A refusal is the host's sentence and is said, not swallowed.
    let mut session = editing_a_file("fn main() {}\n");
    press(&mut session, " fF");
    let about = session.pending_files().expect("a listing to make");
    press(&mut session, " .");
    assert_eq!(
        session.files_listed(&about, Ok(vec!["src/a.rs".into()])),
        Action::Nothing,
        "a picker opened meanwhile"
    );
    session.cancel_narrowing();

    assert_eq!(
        session.files_listed(
            &Files::UnderThisBuffer("elsewhere.rs".into()),
            Ok(vec!["src/a.rs".into()])
        ),
        Action::Nothing,
        "about another buffer"
    );
    assert!(session.narrowing().is_none());

    session.files_listed(&about, Err("/nowhere is not a directory".into()));
    assert_eq!(session.echo(), Some("/nowhere is not a directory"));
}

#[test]
fn the_project_prefix_reaches_the_two_project_searches() {
    // Doom's `SPC p s` and `SPC p .` are the same two commands as `SPC s p` and
    // `SPC s .`, under the prefix named for what they search (#147).
    let mut session = editing_a_file("fn main() { widget }\n");
    press(&mut session, " ps");
    assert_eq!(asking(&mut session), "Search the project: ");

    let mut session = editing_a_file("fn main() { widget }\n");
    session.set_selection(Selection::at(12));
    press(&mut session, " p");
    assert_eq!(session.on_key(Key::Char('.')), Action::Search);
    let asked = session.pending_search().expect("a search to run");
    assert_eq!(asked.pattern, meowr_domain::Pattern::Literal("widget".into()));
    assert_eq!(asked.subject, Subject::Project("main.rs".into()));
}

#[test]
fn spc_s_dot_with_no_symbol_under_the_point_refuses() {
    // The row's whole input is where the point is, so a point on nothing is a
    // question with no subject rather than an empty search -- which `rg` would
    // answer with every line of the Project.
    let mut session = editing_a_file("fn main() {\n\n}\n");
    session.set_selection(Selection::at(12));

    press(&mut session, " s.");

    assert_eq!(session.echo(), Some("No symbol at the point."));
    assert_eq!(session.bottom(), meowr_view::Bottom::Echo, "and on screen");
}

#[test]
fn a_search_asked_in_a_buffer_with_no_file_refuses_before_the_question() {
    // Refused before the pattern rather than after it: there is no directory
    // to search and no Project to be in, and finding that out is not worth a
    // pattern typed first.
    let mut session = editing("scratch\n");

    press(&mut session, " sd");

    assert_eq!(session.echo(), Some("No file, so nowhere to search."));
    assert!(session.narrowing().is_none(), "nothing was asked for");
}

#[test]
fn abandoning_a_search_says_so_rather_than_talking_about_writing() {
    // One sentence used to cover every line read, which was true while
    // `save-as` was the only one. The read knows what it was for.
    let mut session = editing_a_file("fn main() {}\n");

    press(&mut session, " sd");
    session.on_key(Key::Esc);

    assert_eq!(session.echo(), Some("Nothing was searched."));
}

#[test]
fn the_trail_remembers_where_the_picker_opened_and_not_where_previewing_went() {
    // Found in a real terminal, and invisible to every test that came before
    // because none of them previewed. A picker is opened *over* somewhere, and
    // by the time a row is accepted the preview has already moved the point to
    // that row -- so the point pushes where you are *going*. `SPC s d`, RET,
    // backtick left you exactly where you were, which is ADR-0011's "a jump
    // with no way back" wearing a key that appears to work.
    let mut session = editing_a_file("alpha\nbeta\ngamma\ndelta\n");
    session.set_selection(Selection::at(0));

    session.open_results(vec![place("main.rs", 2)]);
    // What the host does with a `Preview::At`: read the file, then show the
    // line the row named. This is the step every earlier test skipped.
    session.previewed("main.rs", Ok("alpha\nbeta\ngamma\ndelta\n".into()));
    assert_eq!(session.point(), 11, "the preview moved the point to the row");

    session.accept_narrowing();
    assert_eq!(session.point(), 11, "accepting keeps where it took you");

    assert_eq!(session.trail_back_for_test(), Action::Redraw);
    assert_eq!(session.point(), 0, "and back where the picker was opened over");
}

#[test]
fn a_result_row_names_its_file_the_way_the_modeline_does() {
    // Found in a real terminal, and the same finding `*diagnostics*` made one
    // ticket earlier: a Place carries an absolute path so it can be opened from
    // anywhere, and an absolute path in a picker row runs off the frame and
    // takes the line number with it. Shortened the one way already on screen
    // rather than a second way invented here.
    let mut session = editing_a_file("alpha\n");

    session.open_results(vec![
        place("/home/user/tools/meow/crates/meowr-app/src/lib.rs", 411)
            .showing("    let widget = self.narrow;"),
    ]);

    let widget = session.narrowing().expect("the picker is open");
    let row = widget.candidate(widget.matches()[0].index);
    assert_eq!(
        row.annotation.as_deref(),
        Some("meowr-app/src/lib.rs:412"),
        "the last three segments, which is `uniquify-buffer-name-style 'forward`"
    );
}

// --- the three rows that search what is already open (#140) ---------------

#[test]
fn spc_s_capital_s_finds_the_symbol_at_point_in_this_buffer() {
    // Doom's `+vertico/search-symbol-at-point`: the symbol, not a prompt.
    // No filesystem and no Project: the buffer is already here, and searching
    // the file on disk would search text the buffer has moved past.
    let mut session = editing_a_file("let widget = 1;\nlet other = 2;\nwidget += 1;\n");
    session.set_selection(Selection::at(4));

    press(&mut session, " s");
    assert_eq!(session.on_key(Key::Char('S')), Action::Redraw);

    let widget = session.narrowing().expect("the picker is open");
    assert_eq!(widget.matches().len(), 2, "both occurrences, and only those");
    let row = |i: usize| {
        let c = widget.candidate(widget.matches()[i].index);
        (c.text.clone(), c.annotation.clone())
    };
    assert_eq!(
        row(0),
        ("let widget = 1;".to_string(), Some("main.rs:1".to_string()))
    );
    assert_eq!(
        row(1),
        ("widget += 1;".to_string(), Some("main.rs:3".to_string()))
    );
}

#[test]
fn spc_s_capital_s_with_no_symbol_under_the_point_refuses() {
    let mut session = editing_a_file("let x = 1;\n\nlet y = 2;\n");
    session.set_selection(Selection::at(11));

    press(&mut session, " s");
    session.on_key(Key::Char('S'));

    assert_eq!(session.echo(), Some("No symbol at the point."));
    assert!(session.narrowing().is_none());
}

#[test]
fn spc_s_capital_b_searches_every_open_buffer_for_what_you_type() {
    // `consult-line-multi` with `all-buffers` -- the extracted row calls it
    // "Search all open buffers", so the current one is included rather than
    // excluded. The buffers, not their files: one of them has unsaved edits in
    // it, and searching what is on disk would answer about text that is no
    // longer there.
    let mut session = editing_a_file("let widget = 1;\n");
    let mut other = Buffer::new();
    other.set_path("other.rs", &session.languages());
    other.loaded(Document::from_text("fn f() {}\nlet widget = 2;\n"));
    session.open(other);

    press(&mut session, " s");
    assert_eq!(session.on_key(Key::Char('B')), Action::Redraw);
    assert_eq!(asking(&mut session), "Search open buffers: ");

    assert_eq!(answer(&mut session, "widget"), Action::Redraw);

    let widget = session.narrowing().expect("the picker is open");
    assert_eq!(widget.matches().len(), 2, "one in each buffer");
    let annotations: Vec<Option<String>> = widget
        .matches()
        .iter()
        .map(|m| widget.candidate(m.index).annotation.clone())
        .collect();
    assert!(
        annotations.contains(&Some("main.rs:1".to_string()))
            && annotations.contains(&Some("other.rs:2".to_string())),
        "both buffers are named: {annotations:?}"
    );
}

#[test]
fn a_buffer_with_no_file_is_not_offered_as_somewhere_to_go() {
    // A Place carries a path because the whole reason it exists is that it may
    // be somewhere else, and one naming a buffer with no file is somewhere
    // nothing can reopen -- the answer `here` already gives. A real shortfall,
    // and the honest one: `*scratch*` cannot be searched this way.
    let mut session = editing("let widget = 1;\n");

    press(&mut session, " s");
    session.on_key(Key::Char('B'));
    answer(&mut session, "widget");

    assert_eq!(session.echo(), Some("Nothing found."));
    assert!(session.narrowing().is_none());
}

#[test]
fn spc_s_capital_i_lists_what_the_parse_knows_is_defined() {
    // `consult-imenu-multi`, and it is **imenu rather than a server call** --
    // which is why this row sits with the ones that search what is already
    // open rather than beside `SPC k D`. The tree meowr parsed for colour
    // already knows what a function and a class are, because two of the six
    // textobject captures the Things use say so.
    let mut session = parsed_rust("fn alpha() {}\n\nstruct Widget {\n    n: u8,\n}\n");

    press(&mut session, " s");
    assert_eq!(session.on_key(Key::Char('I')), Action::Redraw);

    let widget = session.narrowing().expect("the picker is open");
    let rows: Vec<String> = widget
        .matches()
        .iter()
        .map(|m| widget.candidate(m.index).text.clone())
        .collect();
    assert_eq!(
        rows,
        vec!["fn alpha() {}".to_string(), "struct Widget {".to_string()],
        "the line that names each one, in the order you read them"
    );
}

#[test]
fn spc_s_capital_i_in_a_buffer_with_no_parse_says_so() {
    // Plain text has no grammar, so there is nothing that knows what a
    // definition is. An empty picker would read as "this file defines
    // nothing", which is a different claim.
    let mut session = editing_a_file("just some prose\n");

    press(&mut session, " s");
    session.on_key(Key::Char('I'));

    assert_eq!(session.echo(), Some("Nothing found."));
    assert!(session.narrowing().is_none());
}

// --- the fold model (#141) ------------------------------------------------

/// A parsed Rust buffer with a foldable function in it.
fn foldable() -> Session {
    let mut session = parsed_rust(
        "fn alpha() {\n    one();\n    two();\n}\n\nfn beta() {\n    three();\n}\n",
    );
    session.on_resize(40, 12);
    session
}

#[test]
fn folding_leaves_the_head_and_hides_what_is_under_it() {
    // The list is the filter: `block` is one of the eleven node types
    // `treesit-fold` folds in Rust, so the body of `alpha` is foldable and the
    // `fn` line that heads it stays.
    let mut session = foldable();
    session.set_selection(Selection::at(18)); // inside `one();`

    assert!(session.fold_at_point(), "a block to fold");

    let lines: Vec<usize> = session.rows().iter().map(|row| row.line).collect();
    assert_eq!(
        lines,
        vec![0, 3, 4, 5, 6, 7],
        "lines 1 and 2 are hidden and the gutter jumps over them"
    );
    assert!(
        session.rows()[0].text.ends_with(" [...] "),
        "the head says so: {:?}",
        session.rows()[0].text
    );
}

#[test]
fn folding_a_region_the_point_is_in_moves_the_point_to_the_head() {
    // ADR-0012: the point, and nothing else. `hs-minor-mode` and `treesit-fold`
    // both move point out of a region they are about to hide -- a point sitting
    // on a line nobody can see is a cursor that has gone missing.
    let mut session = foldable();
    session.set_selection(Selection::at(17));
    press(&mut session, "wG"); // select a word and grab it
    let grabbed = session.grab().expect("a Grab to keep");
    session.set_selection(Selection::at(18)); // inside `one();`

    session.fold_at_point();

    assert_eq!(session.point(), 0, "the head line, which is line 0");
    assert_eq!(session.grab(), Some(grabbed), "and the Grab is untouched");
}

#[test]
fn folding_from_the_head_leaves_the_point_alone() {
    // "A region the point is *inside*". On the head line there is nothing to
    // move out of, so the selection really is untouched rather than replaced
    // by a bare point that happens to be in the same place.
    let mut session = foldable();
    session.set_selection(Selection::typed(Axis::Select, Kind::LINE, 0, 12));

    session.fold_at_point();

    assert_eq!(
        session.selection().selection_type(),
        Some((Axis::Select, Kind::LINE)),
        "the selection is still the one that was there"
    );
    assert_eq!(session.selection().range(), 0..12);
}

#[test]
fn a_buffer_in_two_windows_is_folded_the_same_in_both() {
    // The Buffer property, which is what ADR-0012 reversed itself onto: a fold
    // belongs to the buffer, so a split shows it folded in both halves. The
    // cost is real and that ADR accepts it -- a split exists to see two parts
    // of one file at once, and this collapses both.
    let mut session = foldable();
    session.set_selection(Selection::at(18));
    session.fold_at_point();

    session.on_key(Key::Ctrl('x'));
    session.on_key(Key::Char('2'));
    assert_eq!(session.window_count(), 2, "two windows on one buffer");

    for window in session.windows().order() {
        let lines: Vec<usize> = session.rows_of(window).iter().map(|r| r.line).collect();
        assert!(!lines.contains(&1), "line 1 is hidden in {window:?}");
    }
}

#[test]
fn unfolding_puts_the_lines_back() {
    let mut session = foldable();
    session.set_selection(Selection::at(18));
    session.fold_at_point();
    assert!(!session.rows().iter().any(|row| row.line == 1));

    assert!(session.unfold_at_point(), "there was one to open");

    let lines: Vec<usize> = session.rows().iter().map(|row| row.line).collect();
    assert_eq!(lines, vec![0, 1, 2, 3, 4, 5, 6, 7]);
    assert!(!session.unfold_at_point(), "and now there is not");
}

#[test]
fn an_edit_above_a_fold_takes_the_fold_with_it() {
    // A fold is a span of text, as the Grab and a diagnostic are. Kept as line
    // numbers it would hide the wrong region the moment a line was typed above
    // it, and say nothing about having done so.
    let mut session = foldable();
    session.set_selection(Selection::at(18));
    session.fold_at_point();

    session.set_selection(Selection::at(0));
    press(&mut session, "S"); // open a line above
    session.on_key(Key::Esc);

    let lines: Vec<usize> = session.rows().iter().map(|row| row.line).collect();
    assert!(
        !lines.contains(&2) && !lines.contains(&3),
        "the hidden lines moved down one with the text: {lines:?}"
    );
    assert!(lines.contains(&1), "and the head moved with them");
}

/// A fold with twenty hidden lines and ten visible ones after it.
fn a_long_fold() -> Session {
    let body: String = (1..=20).map(|n| format!("    line{n}();\n")).collect();
    let tail: String = (1..=10).map(|n| format!("// after {n}\n")).collect();
    let mut session = parsed_rust(&format!("fn alpha() {{\n{body}}}\n{tail}"));
    session.on_resize(40, 8);
    // Fold the body: lines 1..=20 go, leaving 0, 21 (`}`) and 22..=31.
    session.set_selection(Selection::at(20));
    assert!(session.fold_at_point(), "a body to fold");
    session
}

#[test]
fn a_jump_past_a_fold_puts_the_view_where_the_visible_lines_are() {
    // "`viewport.top` is a document line, and *down one* must step over what is
    // hidden" -- ADR-0012 calls this the real cost of folding. Reckoned in
    // document lines, the top wanted for a point on line 21 is line 16, which
    // is inside the fold and draws nothing: the view then starts at 21 and the
    // point sits on the first row with the head gone off the top. Reckoned in
    // visible lines the head is still there, one row above.
    let mut session = a_long_fold();
    assert_eq!(session.rows().first().map(|r| r.line), Some(0));

    session.jump_to(place("t.rs", 21));

    let lines: Vec<usize> = session.rows().iter().map(|r| r.line).collect();
    assert_eq!(
        lines.first(),
        Some(&0),
        "the head is still on screen: {lines:?}"
    );
    assert!(lines.contains(&21), "and so is the line jumped to");
}

#[test]
fn paging_counts_visible_lines_rather_than_document_ones() {
    // A screenful is a screenful of *rows*, and with twenty lines hidden a
    // screenful of document lines is most of the file.
    let mut session = a_long_fold();
    assert_eq!(session.rows().first().map(|r| r.line), Some(0));

    session.on_key(Key::Ctrl('v'));

    let after: Vec<usize> = session.rows().iter().map(|r| r.line).collect();
    assert!(
        after.iter().all(|line| !(1..=20).contains(line)),
        "no hidden line is ever drawn: {after:?}"
    );
    assert!(
        after.first().is_some_and(|line| *line >= 21),
        "and it moved on past the fold rather than into it: {after:?}"
    );
    assert!(after.len() > 1, "a screenful, not one row: {after:?}");
}

// --- the four fold commands under `SPC C-f` (#142) -------------------------

#[test]
fn spc_c_f_c_f_toggles_the_fold_at_the_point() {
    // `+fold/toggle`. One key that closes what is open and opens what is
    // closed, which is the row a hand reaches for -- Doom's other four fold
    // commands are bound only to evil `z` keys and cannot be reached here.
    let mut session = foldable();
    session.set_selection(Selection::at(18)); // inside `alpha`'s body

    press(&mut session, " ");
    session.on_key(Key::Ctrl('f'));
    assert_eq!(session.on_key(Key::Ctrl('f')), Action::Redraw);
    assert!(
        !session.rows().iter().any(|row| row.line == 1),
        "closed: the body is hidden"
    );

    // The point is on the head now, which is where folding put it.
    press(&mut session, " ");
    session.on_key(Key::Ctrl('f'));
    session.on_key(Key::Ctrl('f'));
    assert!(
        session.rows().iter().any(|row| row.line == 1),
        "and open again"
    );
}

#[test]
fn spc_c_f_c_u_opens_the_fold_at_the_point_and_only_opens() {
    // `+fold/open`. Not a toggle: pressing it on an open fold leaves it open
    // rather than closing it, which is the whole difference from `C-f C-f`.
    let mut session = foldable();
    session.set_selection(Selection::at(18));
    session.fold_at_point();

    press(&mut session, " ");
    session.on_key(Key::Ctrl('f'));
    assert_eq!(session.on_key(Key::Ctrl('u')), Action::Redraw);
    assert!(session.rows().iter().any(|row| row.line == 1), "opened");

    press(&mut session, " ");
    session.on_key(Key::Ctrl('f'));
    session.on_key(Key::Ctrl('u'));
    assert!(
        session.rows().iter().any(|row| row.line == 1),
        "and still open, because open is not a toggle"
    );
}

#[test]
fn spc_c_f_c_a_c_f_closes_every_fold_and_c_a_c_u_opens_them() {
    // `+fold/close-all` and `+fold/open-all`, under Doom's own nested prefix.
    let mut session = foldable();

    press(&mut session, " ");
    session.on_key(Key::Ctrl('f'));
    session.on_key(Key::Ctrl('a'));
    assert_eq!(session.on_key(Key::Ctrl('f')), Action::Redraw);

    let lines: Vec<usize> = session.rows().iter().map(|row| row.line).collect();
    assert!(
        !lines.contains(&1) && !lines.contains(&6),
        "both function bodies are folded, not just the one at the point: {lines:?}"
    );

    press(&mut session, " ");
    session.on_key(Key::Ctrl('f'));
    session.on_key(Key::Ctrl('a'));
    session.on_key(Key::Ctrl('u'));

    let lines: Vec<usize> = session.rows().iter().map(|row| row.line).collect();
    assert_eq!(lines, vec![0, 1, 2, 3, 4, 5, 6, 7], "and all of them open");
}

#[test]
fn spc_c_f_is_a_prefix_with_a_menu_rather_than_a_reservation() {
    let mut session = foldable();

    press(&mut session, " ");
    session.on_key(Key::Ctrl('f'));

    let menu = session.menu();
    assert!(!menu.is_empty(), "the prefix opens onto its rows");
    assert!(
        menu.iter().any(|e| e.key == Key::Ctrl('a')),
        "including the nested one: {:?}",
        menu.iter().map(|e| e.key).collect::<Vec<_>>()
    );
}

// --- a jump into a folded region unfolds it (#143) -------------------------

#[test]
fn a_place_inside_a_fold_unfolds_it_and_the_point_lands_visibly() {
    // The one place two efforts meet. Jumping anyway would leave the point on
    // a line nobody can see with the screen unchanged, which reads as a broken
    // jump rather than as a fold; refusing would let a fold veto a jump into
    // the buffer it is in. It unfolds enough to show the target.
    let mut session = foldable();
    session.set_selection(Selection::at(18));
    session.fold_at_point();
    assert!(!session.rows().iter().any(|row| row.line == 1), "line 1 hidden");

    // `t.rs` line 1 is `    one();`, inside the fold that was just made.
    session.jump_to(place("t.rs", 1));

    assert!(
        session.rows().iter().any(|row| row.line == 1),
        "the line jumped to is on screen"
    );
    assert_eq!(session.point(), 13, "and the point is on it");
}

#[test]
fn only_the_folds_hiding_the_target_are_opened() {
    // "Unfolds *enough*". A second fold elsewhere in the buffer is not in the
    // way and is left closed -- opening the buffer's folds wholesale would
    // make a jump undo work the reader had done.
    let mut session = foldable();
    session.fold_all();
    assert!(!session.rows().iter().any(|row| row.line == 6), "beta folded");

    session.jump_to(place("t.rs", 1));

    assert!(session.rows().iter().any(|row| row.line == 1), "alpha opened");
    assert!(
        !session.rows().iter().any(|row| row.line == 6),
        "and beta is still folded, because it was never in the way"
    );
}

#[test]
fn popping_the_trail_into_a_fold_unfolds_it_the_same_way() {
    // The same mechanism, and it has to be: a Trail entry outlives the buffer
    // state it was made in, so the line it names can have been folded since.
    let mut session = foldable();
    session.set_selection(Selection::at(13)); // on `one();`, inside alpha
    session.jump_to(place("t.rs", 6)); // away, which pushes line 1

    session.set_selection(Selection::at(18));
    session.fold_at_point(); // fold alpha, hiding where the Trail points
    assert!(!session.rows().iter().any(|row| row.line == 1));

    assert_eq!(session.trail_back_for_test(), Action::Redraw);

    assert!(
        session.rows().iter().any(|row| row.line == 1),
        "back is a jump too, and lands somewhere you can see"
    );
    assert_eq!(session.point(), 13);
}

#[test]
fn the_line_a_jump_pushed_is_one_you_can_return_to() {
    // ADR-0012's ordering clause: the unfold happens before the Place
    // resolves, so what the Trail was handed is a line that is really there.
    let mut session = foldable();
    session.fold_all();
    let visible_before: Vec<usize> = session.rows().iter().map(|r| r.line).collect();

    session.jump_to(place("t.rs", 1));
    session.trail_back_for_test();

    let line = session
        .rows()
        .iter()
        .find(|row| row.cursor.is_some())
        .map(|row| row.line);
    assert!(
        line.is_some(),
        "the point came back onto a drawn row; before the jump the rows were \
         {visible_before:?}"
    );
}

#[test]
fn folding_everything_leaves_the_point_somewhere_you_can_see_it() {
    // Found in a real terminal. `SPC C-f C-a C-f` with the point inside a body
    // hid the line it was on and left it there: the modeline said 3:1 and line
    // 3 was not drawn, so the cursor had gone missing. ADR-0012's rule is about
    // folding *a region the point is inside*, and closing every fold is that
    // for whichever region the point happens to be in.
    let mut session = foldable();
    session.set_selection(Selection::at(18)); // inside `alpha`'s body, line 1

    session.fold_all();

    let point_line = session
        .rows()
        .iter()
        .find(|row| row.cursor.is_some())
        .map(|row| row.line);
    assert_eq!(
        point_line,
        Some(0),
        "the point is on the head, and the head is drawn"
    );
}

#[test]
fn the_point_comes_out_to_the_outermost_head_that_is_really_drawn() {
    // Not the innermost fold's head: that head can itself be hidden by an
    // outer fold, and a point put there would be no more visible than where it
    // started. The only condition that matters is that the line is drawn.
    let mut session = foldable();
    session.set_selection(Selection::at(18)); // inside `alpha`'s body
    session.fold_at_point(); // the inner block; point goes to line 0
    session.fold_all(); // everything, including whatever encloses it

    let drawn: Vec<usize> = session.rows().iter().map(|row| row.line).collect();
    let point_line = session
        .rows()
        .iter()
        .find(|row| row.cursor.is_some())
        .map(|row| row.line)
        .expect("the point is on a drawn row");
    assert!(
        drawn.contains(&point_line),
        "{point_line} is drawn, among {drawn:?}"
    );
}

// --- a picker that asks its source again as you type (#144) ---------------

#[test]
fn spc_k_j_opens_a_picker_that_has_asked_nothing_yet() {
    // The widget filters a list fixed when the picker opened, and a
    // `workspace/symbol` list cannot be fixed then: the *server* is the matcher
    // and it truncates. Asked with an empty query, rust-analyzer came back
    // holding `aho_corasick` and not `Trail` -- a symbol picker missing its own
    // workspace's symbols, which is why #137 handed these two rows back.
    let mut session = editing_a_file("struct Widget {}\n");

    press(&mut session, " k");
    assert_eq!(session.on_key(Key::Char('j')), Action::Redraw);

    assert!(session.narrowing().is_some(), "the picker is open");
    assert!(
        !session.asking_pending(),
        "and nothing is asked for an input too short to ask about"
    );
    let view = session.narrow_view().expect("open");
    assert!(view.rows.is_empty(), "no rows yet");
    assert!(
        view.note.is_some_and(|note| note.contains('3')),
        "and it says what it is waiting for"
    );
}

#[test]
fn typing_enough_asks_the_server_for_what_was_typed() {
    let mut session = editing_a_file("struct Widget {}\n");
    press(&mut session, " kj");

    press(&mut session, "Tra");

    assert!(session.asking_pending(), "a question is due");
    let asked = session.take_ask().expect("a question to put");
    assert_eq!(
        asked.kind,
        meowr_app::LookupKind::Symbols {
            query: "Tra".to_string(),
            everywhere: false
        }
    );
    assert!(!session.asking_pending(), "drained, not asked twice");
}

#[test]
fn the_answer_replaces_the_rows_and_the_note_goes() {
    let mut session = editing_a_file("struct Widget {}\n");
    press(&mut session, " kj");
    press(&mut session, "Tra");
    session.take_ask();

    assert_eq!(
        session.found_symbols("Tra", vec![place("core/edit.rs", 370).showing("pub struct Trail {")]),
        Action::Redraw
    );

    let view = session.narrow_view().expect("open");
    assert_eq!(view.rows.len(), 1);
    assert_eq!(view.rows[0].text, "pub struct Trail {");
    assert_eq!(view.note, None, "settled, so nothing is in flight to say");
}

#[test]
fn an_answer_about_an_input_that_has_moved_on_is_dropped() {
    // ADR-0009, with the stamp on the **input** rather than on the buffer:
    // they arrive late and out of order, and a reply to "Tra" landing after a
    // reply to "Trail" would replace the better list with the worse one.
    let mut session = editing_a_file("struct Widget {}\n");
    press(&mut session, " kj");
    press(&mut session, "Trail");
    session.take_ask();
    session.found_symbols("Trail", vec![place("core/edit.rs", 370).showing("pub struct Trail {")]);

    // The older question, answering late.
    assert_eq!(
        session.found_symbols("Tra", vec![place("wrong.rs", 1).showing("fn transmute() {}")]),
        Action::Nothing
    );

    let view = session.narrow_view().expect("open");
    assert_eq!(view.rows.len(), 1);
    assert_eq!(view.rows[0].text, "pub struct Trail {", "the better list stands");
}

#[test]
fn the_rows_stay_up_while_the_next_question_is_in_flight() {
    // What shows while a question is out: the last answer's rows, and a note
    // saying they are one query behind. Clearing them would blink the list
    // empty on every keystroke, and saying nothing at all would be the silence
    // that looks like "no matches".
    let mut session = editing_a_file("struct Widget {}\n");
    press(&mut session, " kj");
    press(&mut session, "Tra");
    session.take_ask();
    session.found_symbols("Tra", vec![place("core/edit.rs", 370).showing("pub struct Trail {")]);

    press(&mut session, "i"); // now "Trai"
    session.take_ask();

    let view = session.narrow_view().expect("open");
    assert_eq!(view.rows.len(), 1, "the old rows are still up");
    assert!(
        view.note.is_some_and(|note| note.to_lowercase().contains("asking")),
        "and the note says why they may be stale"
    );
}

#[test]
fn a_question_that_found_nothing_says_so_rather_than_going_quiet() {
    let mut session = editing_a_file("struct Widget {}\n");
    press(&mut session, " kj");
    press(&mut session, "zzz");
    session.take_ask();

    session.found_symbols("zzz", Vec::new());

    let view = session.narrow_view().expect("open");
    assert!(view.rows.is_empty());
    assert!(
        view.note
            .as_deref()
            .is_some_and(|note| note.to_lowercase().contains("no ")),
        "distinct from still asking: {:?}",
        view.note
    );
}

#[test]
fn spc_k_capital_j_asks_every_server_rather_than_this_buffers() {
    // Doom's `k J` is `consult-lsp-symbols` with `all-workspaces`. meowr runs
    // one server per Language, so a workspace is a server and "any" is all of
    // them.
    let mut session = editing_a_file("struct Widget {}\n");
    press(&mut session, " k");
    session.on_key(Key::Char('J'));
    press(&mut session, "Tra");

    let asked = session.take_ask().expect("a question to put");
    assert_eq!(
        asked.kind,
        meowr_app::LookupKind::Symbols {
            query: "Tra".to_string(),
            everywhere: true
        }
    );
}

#[test]
fn every_server_that_answers_one_question_adds_to_the_same_list() {
    // `SPC k J` is `all-workspaces`, and meowr runs one server per Language, so
    // the host puts the question to each and their answers arrive separately.
    // Clearing the stamp on the first of them dropped the rest as though the
    // input had moved on -- so "any workspace" showed whichever server was
    // quickest. Invisible with one server running, which is every test that
    // came before this and the live check too.
    let mut session = editing_a_file("struct Widget {}\n");
    press(&mut session, " k");
    session.on_key(Key::Char('J'));
    press(&mut session, "Wid");
    session.take_ask();

    session.found_symbols("Wid", vec![place("rust/a.rs", 1).showing("Widget")]);
    assert_eq!(
        session.found_symbols("Wid", vec![place("python/b.py", 2).showing("WidgetBase")]),
        Action::Redraw,
        "the second server's answer is not stale"
    );

    let view = session.narrow_view().expect("open");
    let rows: Vec<String> = view.rows.iter().map(|row| row.text.clone()).collect();
    assert_eq!(rows.len(), 2, "both servers' symbols: {rows:?}");
    assert!(rows.contains(&"Widget".to_string()));
    assert!(rows.contains(&"WidgetBase".to_string()));
}

// --- a foreign edit in one buffer (#161, ADR-0013) ----------------------------

fn foreign(
    path: &str,
    revision: u64,
    replacements: Vec<(std::ops::Range<usize>, &str)>,
) -> meowr_app::ForeignEdit {
    meowr_app::ForeignEdit {
        path: path.to_string(),
        revision,
        from: "rustfmt".to_string(),
        replacements: replacements
            .into_iter()
            .map(|(range, text)| meowr_domain::Replacement {
                range,
                text: text.to_string(),
            })
            .collect(),
    }
}

#[test]
fn a_foreign_edit_lands_and_is_an_ordinary_edit_after() {
    // Modified, and the revision moves, which is what tells the host to send
    // `didChange` -- ADR-0013: otherwise an ordinary edit.
    let mut session = editing_a_file("let a = 1;\n");
    let asked = session.revision_of("main.rs").expect("an open buffer");

    let edit = foreign("main.rs", asked, vec![(4..5, "alpha")]);
    assert_eq!(session.apply_foreign_edit(edit), Action::Redraw);
    assert_eq!(session.contents().unwrap(), "let alpha = 1;\n");
    assert!(session.is_modified());
    assert!(session.revision_of("main.rs").unwrap() > asked);

    press(&mut session, "u");
    assert_eq!(session.contents().unwrap(), "let a = 1;\n", "one undo");
}

#[test]
fn a_foreign_edit_for_text_that_has_moved_is_dropped_and_said() {
    let mut session = editing_a_file("let a = 1;\n");
    let asked = session.revision_of("main.rs").unwrap();
    session.apply_foreign_edit(foreign("main.rs", asked, vec![(0..0, "// \n")]));

    // Computed against the revision before that edit, so about text that has
    // gone: ADR-0013 drops it rather than rebasing it.
    let stale = foreign("main.rs", asked, vec![(4..5, "alpha")]);
    session.apply_foreign_edit(stale);
    assert_eq!(session.contents().unwrap(), "// \nlet a = 1;\n");
    assert_eq!(
        session.echo(),
        Some("main.rs changed while rustfmt was working, so its edit was not applied.")
    );
}

#[test]
fn a_read_only_buffer_refuses_a_foreign_edit_whole() {
    let mut session = editing_a_file("let a = 1;\n");
    press(&mut session, " tr");
    let asked = session.revision_of("main.rs").unwrap();
    session.apply_foreign_edit(foreign("main.rs", asked, vec![(4..5, "alpha")]));
    assert_eq!(session.contents().unwrap(), "let a = 1;\n");
    assert_eq!(
        session.echo(),
        Some("main.rs is read-only, so rustfmt's edit was not applied.")
    );
}

#[test]
fn a_foreign_edit_that_cannot_be_applied_changes_nothing_and_says_why() {
    let mut session = editing_a_file("let a = 1;\n");
    let asked = session.revision_of("main.rs").unwrap();
    session.apply_foreign_edit(foreign("main.rs", asked, vec![(0..5, "x"), (3..7, "y")]));
    assert_eq!(session.contents().unwrap(), "let a = 1;\n");
    let echo = session.echo().expect("it says so");
    assert!(echo.starts_with("rustfmt's edit could not be applied"), "{echo}");
}

#[test]
fn the_grab_follows_a_foreign_edit() {
    let mut session = editing_a_file("one two\n");
    press(&mut session, "wwG"); // grab the second word
    let grabbed = session.grab().expect("a Grab");
    let asked = session.revision_of("main.rs").unwrap();
    session.apply_foreign_edit(foreign("main.rs", asked, vec![(0..3, "three")]));
    assert_eq!(
        session.grab(),
        Some(grabbed.start + 2..grabbed.end + 2),
        "moved by what the replacement before it added"
    );
}

#[test]
fn a_fold_survives_a_foreign_edit_inside_it_and_opens_when_its_edge_is_crossed() {
    // ADR-0013: a fold whose edge a replacement crosses no longer covers the
    // code that was folded, and guessing is worse than showing it.
    let mut session = foldable();
    session.set_path("t.rs");
    session.set_selection(Selection::at(18));
    assert!(session.fold_at_point());
    let folded = |session: &Session| !session.buffers()[session.current_buffer_id().0].folds().is_empty();

    let asked = session.revision_of("t.rs").unwrap();
    session.apply_foreign_edit(foreign("t.rs", asked, vec![(17..20, "uno")]));
    assert!(folded(&session), "an edit wholly inside leaves it folded");

    let asked = session.revision_of("t.rs").unwrap();
    session.apply_foreign_edit(foreign("t.rs", asked, vec![(30..37, "();}\n")]));
    assert!(!folded(&session), "an edit across its end opens it");
}

#[test]
fn a_foreign_edit_runs_no_command_hooks() {
    // It is not a command and not a save, so neither kind of hook that runs a
    // command has anything to answer: a server's edit must not set off a
    // config's `after-save`.
    let mut session = editing_a_file("let a = 1;\n");
    session
        .apply(hook_set(meowr_command::HookPoint::AfterSave, "open-below"))
        .expect("resolves");
    let asked = session.revision_of("main.rs").unwrap();
    session.apply_foreign_edit(foreign("main.rs", asked, vec![(4..5, "alpha")]));
    assert_eq!(session.contents().unwrap(), "let alpha = 1;\n", "no line opened");
}

#[test]
fn a_foreign_edit_with_nothing_in_it_changes_nothing_at_all() {
    // A formatter whose output matches the buffer hands back no hunks. That is
    // not an edit: no revision to move, nothing for auto-save to count, and no
    // `didChange` for the host to send.
    let mut session = editing_a_file("let a = 1;\n");
    let asked = session.revision_of("main.rs").unwrap();
    assert_eq!(
        session.apply_foreign_edit(foreign("main.rs", asked, Vec::new())),
        Action::Nothing
    );
    assert_eq!(session.revision_of("main.rs"), Some(asked));
    assert!(!session.is_modified());
}

#[test]
fn a_fold_after_a_foreign_edit_moves_with_the_text() {
    // Outside every replacement, so it stays folded -- and it has to hide the
    // same code it did, now two lines further down.
    let mut session = foldable();
    session.set_selection(Selection::at(18)); // inside `alpha`'s body
    assert!(session.fold_at_point());
    let id = session.current_buffer_id();
    let hides = |session: &Session, line: usize| {
        let buffer = &session.buffers()[id.0];
        buffer.folds().hides(buffer.document().expect("read"), line)
    };
    assert!(hides(&session, 1), "`one();` hidden");

    let asked = session.revision_of("t.rs").unwrap();
    session.apply_foreign_edit(foreign("t.rs", asked, vec![(0..0, "// a\n// b\n")]));
    assert!(hides(&session, 3), "the same line, two further down");
    assert!(!hides(&session, 1), "and not what is there now");
}

#[test]
fn a_reformat_leaves_the_point_on_the_code_it_was_on() {
    // #162 composed with #161: a formatter's whole output, diffed into hunks
    // and applied as a foreign edit. The point was on `two` and two lines were
    // reindented around it, one above -- it should still be on `two`.
    let old = "fn f() {\n        one();\n        two();\n}\n";
    let new = "fn f() {\n    one();\n    two();\n}\n";
    let mut session = editing_a_file(old);
    let on_two = old.find("two").unwrap() + 1;
    session.set_selection(Selection::at(on_two));
    let asked = session.revision_of("main.rs").unwrap();

    session.apply_foreign_edit(meowr_app::ForeignEdit {
        path: "main.rs".into(),
        revision: asked,
        from: "rustfmt".into(),
        replacements: meowr_domain::replacements_between(old, new),
    });

    assert_eq!(session.contents().unwrap(), new);
    assert_eq!(session.point(), new.find("two").unwrap() + 1, "still on `two`");
    press(&mut session, "u");
    assert_eq!(session.contents().unwrap(), old, "and one undo takes it all back");
}

#[test]
fn a_line_a_formatter_adds_above_an_unindented_line_does_not_take_the_point() {
    // The insertion lands exactly where the point is, at column 0, and the
    // point belongs to the code after it: the point follows `fn`, and a fold
    // headed on that line keeps its head.
    let old = "use a;\nfn f() {\n    one();\n}\n";
    let new = "use a;\nuse b;\nfn f() {\n    one();\n}\n";
    let mut session = editing_a_file(old);
    session.set_selection(Selection::at(old.find("fn").unwrap()));
    let asked = session.revision_of("main.rs").unwrap();
    session.apply_foreign_edit(meowr_app::ForeignEdit {
        path: "main.rs".into(),
        revision: asked,
        from: "rustfmt".into(),
        replacements: meowr_domain::replacements_between(old, new),
    });
    assert_eq!(session.point(), new.find("fn").unwrap(), "still on `fn`");
}

#[test]
fn a_fold_headed_where_a_formatter_adds_a_line_keeps_its_head() {
    let old = "use a;\nfn alpha() {\n    one();\n    two();\n}\n";
    let new = "use a;\nuse b;\nfn alpha() {\n    one();\n    two();\n}\n";
    let mut session = parsed_rust(old);
    session.on_resize(40, 12);
    session.set_selection(Selection::at(old.find("one").unwrap()));
    assert!(session.fold_at_point());
    let asked = session.revision_of("t.rs").unwrap();
    session.apply_foreign_edit(meowr_app::ForeignEdit {
        path: "t.rs".into(),
        revision: asked,
        from: "rustfmt".into(),
        replacements: meowr_domain::replacements_between(old, new),
    });
    let buffer = &session.buffers()[session.current_buffer_id().0];
    let doc = buffer.document().expect("read");
    assert!(buffer.folds().heads(doc, 2), "`fn alpha` still heads it");
    assert!(!buffer.folds().heads(doc, 1), "not the line added above");
    assert!(buffer.folds().hides(doc, 3), "and `one();` still hidden");
}

// --- `SPC k f` formats the buffer (#164, ADR-0014) ---------------------------

fn formatting(text: &str) -> (Session, meowr_app::Format) {
    let mut session = editing_a_file(text);
    press(&mut session, " k");
    assert_eq!(session.on_key(Key::Char('f')), Action::Format, "SPC k f asks");
    let asked = session.pending_format().expect("a format to run");
    (session, asked)
}

#[test]
fn spc_k_f_asks_the_host_to_run_the_formatter_over_the_whole_buffer() {
    let (mut session, asked) = formatting("fn main(){}\n");
    assert_eq!(asked.path, "main.rs");
    assert_eq!(asked.invocation.program, "rustfmt");
    assert_eq!(asked.text, "fn main(){}\n", "the text goes on stdin");
    assert_eq!(asked.revision, session.revision_of("main.rs").unwrap());
    assert_eq!(session.pending_format(), None, "drained, not read twice");
}

#[test]
fn the_formatted_text_is_applied_as_one_step_with_the_point_on_its_code() {
    let old = "fn main() {\n        one();\n}\n";
    let new = "fn main() {\n    one();\n}\n";
    let (mut session, asked) = formatting(old);
    session.set_selection(Selection::at(old.find("one").unwrap()));

    session.formatted(&asked, meowr_app::Formatted::Output(new.to_string()));
    assert_eq!(session.contents().unwrap(), new);
    assert_eq!(session.point(), new.find("one").unwrap());
    press(&mut session, "u");
    assert_eq!(session.contents().unwrap(), old, "one undo");
}

#[test]
fn text_already_formatted_is_said_and_left_alone() {
    // The explicit key says so: a key that changes nothing and says nothing
    // reads as a key that did not run (#69).
    let (mut session, asked) = formatting("fn main() {}\n");
    session.formatted(&asked, meowr_app::Formatted::Output("fn main() {}\n".into()));
    assert!(!session.is_modified());
    assert_eq!(session.echo(), Some("main.rs is already formatted."));
}

#[test]
fn a_format_that_lands_after_typing_is_dropped_and_said() {
    let (mut session, asked) = formatting("fn main(){}\n");
    press(&mut session, "sx"); // typed meanwhile: insert an `x`
    session.on_key(Key::Esc);
    session.formatted(&asked, meowr_app::Formatted::Output("fn main() {}\n".into()));
    assert!(session.contents().unwrap().contains("fn main(){}"), "not applied");
    assert_eq!(
        session.echo(),
        Some("main.rs changed while rustfmt was working, so its edit was not applied.")
    );
}

#[test]
fn a_formatter_that_fails_or_is_missing_is_said_and_changes_nothing() {
    let (mut session, asked) = formatting("fn main({}\n");
    session.formatted(
        &asked,
        meowr_app::Formatted::Failed("error: expected one of `)`, found `{`".into()),
    );
    assert_eq!(session.contents().unwrap(), "fn main({}\n");
    assert_eq!(
        session.echo(),
        Some("rustfmt failed: error: expected one of `)`, found `{`")
    );

    let (mut session, asked) = formatting("fn main() {}\n");
    session.formatted(&asked, meowr_app::Formatted::Missing);
    assert_eq!(session.echo(), Some("rustfmt is not installed, so rust is not formatted."));
}

#[test]
fn spc_k_f_where_nothing_formats_says_so() {
    let mut session = editing("# notes\n");
    session.set_path("notes.md");
    press(&mut session, " kf");
    assert_eq!(session.pending_format(), None);
    assert_eq!(session.echo(), Some("Nothing formats markdown."));

    let mut session = editing("scratch\n");
    press(&mut session, " kf");
    assert_eq!(session.echo(), Some("This buffer has no file."));
}

// --- saving formats: write, format, write again (#165, ADR-0014) --------------

fn saving_with_format_on_save(text: &str) -> Session {
    let mut session = editing_a_file(text);
    let mut options = session.options();
    options.format_on_save = true;
    session.set_options(options);
    session
}

/// `SPC f s`, and the host writing what it was asked to.
fn save(session: &mut Session) {
    press(session, " f");
    assert_eq!(session.on_key(Key::Char('s')), Action::Save);
    session.save_target().expect("something to write");
    session.mark_saved();
}

#[test]
fn with_format_on_save_off_a_save_is_exactly_a_save() {
    let mut session = editing_a_file("fn main(){}\n");
    save(&mut session);
    assert_eq!(session.pending_format(), None);
}

#[test]
fn with_format_on_save_on_the_written_file_is_then_formatted() {
    // apheleia's order: the write has happened and been marked before anything
    // is asked of the formatter, so a save is true when it says so.
    let mut session = saving_with_format_on_save("fn main(){}\n");
    save(&mut session);
    assert!(!session.is_modified(), "the first write stands on its own");
    let asked = session.pending_format().expect("then a format");
    assert_eq!(asked.origin, meowr_app::FormatOrigin::Save);
    assert_eq!(asked.revision, session.revision_of("main.rs").unwrap());
}

#[test]
fn a_formatted_save_is_written_again_quietly_and_ends_unmodified() {
    let mut session = saving_with_format_on_save("fn main(){}\n");
    session
        .apply(hook_set(meowr_command::HookPoint::AfterSave, "open-below"))
        .expect("resolves");
    save(&mut session);
    let lines_after_first_save = session.contents().unwrap().lines().count();
    let asked = session.pending_format().expect("a format");
    let formatted = session.contents().unwrap().replace("(){}", "() {}");

    assert_eq!(
        session.formatted(&asked, meowr_app::Formatted::Output(formatted.clone())),
        Action::Save,
        "the host is asked to write again"
    );
    let write = session.quiet_save_target().expect("the formatted text to write");
    assert_eq!(write.path, "main.rs");
    assert_eq!(write.contents, formatted);
    session.mark_saved_quietly(&write.path);

    assert!(!session.is_modified());
    assert_eq!(
        session.contents().unwrap().lines().count(),
        lines_after_first_save,
        "`after-save` ran for the save, not again for the second write"
    );
    assert_eq!(session.pending_format(), None, "and a quiet write formats nothing");
}

#[test]
fn a_save_whose_text_was_already_formatted_says_nothing() {
    let mut session = saving_with_format_on_save("fn main() {}\n");
    save(&mut session);
    let asked = session.pending_format().expect("a format");
    assert_eq!(
        session.formatted(&asked, meowr_app::Formatted::Output("fn main() {}\n".into())),
        Action::Nothing
    );
    assert_eq!(session.echo(), None);
}

#[test]
fn a_formatter_missing_on_save_is_said_once_for_its_language() {
    let mut session = saving_with_format_on_save("fn main() {}\n");
    save(&mut session);
    let asked = session.pending_format().unwrap();
    session.formatted(&asked, meowr_app::Formatted::Missing);
    assert_eq!(session.echo(), Some("rustfmt is not installed, so rust is not formatted."));

    save(&mut session);
    let asked = session.pending_format().unwrap();
    assert_eq!(session.formatted(&asked, meowr_app::Formatted::Missing), Action::Nothing);
    assert!(
        session.echo().is_none_or(|said| !said.contains("not installed")),
        "not said again on the next save: {:?}",
        session.echo()
    );
}

#[test]
fn a_universal_argument_skips_formatting_for_that_save() {
    // Doom's prefix argument on `+format/save-buffer`.
    let mut session = saving_with_format_on_save("fn main(){}\n");
    session.on_key(Key::Ctrl('u'));
    save(&mut session);
    assert_eq!(session.pending_format(), None);

    save(&mut session);
    assert!(session.pending_format().is_some(), "only that once");
}

#[test]
fn a_save_time_format_dropped_for_typing_leaves_the_first_write_standing() {
    // The refusal moved nothing, but typing had moved the revision -- and a
    // check that guessed "landed" from the revision took the refusal for a
    // success, so the host would have written the typed text quietly and
    // called it saved. The first write stands, and the typing stays unsaved.
    let mut session = saving_with_format_on_save("fn main(){}\n");
    save(&mut session);
    let asked = session.pending_format().expect("a format");
    press(&mut session, "sx");
    session.on_key(Key::Esc);

    assert_ne!(
        session.formatted(&asked, meowr_app::Formatted::Output("fn main() {}\n".into())),
        Action::Save,
        "nothing to write again"
    );
    assert_eq!(session.quiet_save_target(), None);
    assert!(session.is_modified(), "the typing is still unsaved");
    assert!(session.echo().unwrap_or_default().contains("was not applied"));
}

#[test]
fn a_universal_argument_skips_formatting_for_a_save_as_too() {
    // The name is read between the key and the save, and the read dispatches
    // `write-file` again with no argument of its own.
    let mut session = editing("fn main(){}\n");
    let mut options = session.options();
    options.format_on_save = true;
    session.set_options(options);
    session.on_key(Key::Ctrl('u'));
    press(&mut session, " fs");
    for c in "fresh.rs".chars() {
        session.on_key(Key::Char(c));
    }
    assert_eq!(session.on_key(Key::Enter), Action::Save);
    session.mark_saved();
    assert_eq!(session.pending_format(), None);
}

#[test]
fn an_after_save_hook_does_not_undo_a_universal_argument() {
    let mut session = saving_with_format_on_save("fn main(){}\n");
    session
        .apply(hook_set(meowr_command::HookPoint::AfterSave, "write-file"))
        .expect("resolves");
    session.on_key(Key::Ctrl('u'));
    save(&mut session);
    assert_eq!(session.pending_format(), None, "the hook's own write-file did not reset it");
}

// --- `save-some-buffers` (#166, ADR-0013 amended) ------------------------------

fn m_x(session: &mut Session, name: &str) -> Action {
    session.on_key(Key::Alt('x'));
    type_name(session, name);
    session.on_key(Key::Enter)
}

/// `main.rs` on screen and modified, and `other.rs` open behind it, modified.
fn two_modified_buffers() -> (Session, BufferId, BufferId) {
    let mut session = editing_a_file("fn main() {}\n");
    let main = session.current_buffer_id();
    let mut other = Buffer::scratch();
    other.set_path("other.rs", &session.languages());
    other.loaded(Document::from_text("fn other() {}\n"));
    let other = session.open(other);
    press(&mut session, "sx");
    session.on_key(Key::Esc);
    session.show(main);
    press(&mut session, "sy");
    session.on_key(Key::Esc);
    (session, main, other)
}

/// `save-some-buffers`, and the host writing everything it was asked to.
fn save_some(session: &mut Session) -> Vec<meowr_app::Write> {
    assert_eq!(m_x(session, "save-some-buffers"), Action::SaveSome);
    let writes = session.pending_saves();
    session.saved_some(writes.iter().map(|w| (w.path.clone(), Ok(()))).collect());
    writes
}

#[test]
fn save_some_buffers_writes_every_modified_buffer_shown_or_not() {
    let (mut session, main, _) = two_modified_buffers();
    assert_eq!(m_x(&mut session, "save-some-buffers"), Action::SaveSome);
    let writes = session.pending_saves();
    let paths: Vec<&str> = writes.iter().map(|w| w.path.as_str()).collect();
    assert_eq!(paths, ["main.rs", "other.rs"]);
    assert!(writes[1].contents.starts_with('x'), "other.rs's own text");

    let results = writes.iter().map(|w| (w.path.clone(), Ok(()))).collect();
    session.saved_some(results);
    assert!(session.buffers().iter().filter(|b| b.is_real()).all(|b| !b.is_modified()));
    assert_eq!(session.echo(), Some("Saved 2 buffers."));
    assert_eq!(session.current_buffer_id(), main, "the window was not moved");
}

#[test]
fn a_buffer_that_cannot_be_saved_is_named_and_the_rest_are_saved() {
    let (mut session, main, other) = two_modified_buffers();
    press(&mut session, " tr"); // main.rs read-only, still modified
    let writes = save_some(&mut session);
    assert_eq!(writes.len(), 1, "only other.rs is written");
    assert_eq!(session.echo(), Some("Saved 1 buffer; main.rs is read-only."));
    assert!(session.buffer(main).is_modified(), "main.rs was not written");
    assert!(!session.buffer(other).is_modified(), "other.rs was");
}

#[test]
fn a_write_that_fails_is_named_with_its_reason() {
    let (mut session, ..) = two_modified_buffers();
    m_x(&mut session, "save-some-buffers");
    let results = session
        .pending_saves()
        .into_iter()
        .map(|w| {
            let failed = (w.path == "other.rs").then(|| "permission denied".to_string());
            (w.path, failed.map_or(Ok(()), Err))
        })
        .collect();
    session.saved_some(results);
    assert_eq!(
        session.echo(),
        Some("Saved 1 buffer; could not write other.rs: permission denied.")
    );
}

#[test]
fn a_modified_buffer_with_no_file_is_named_not_passed_over() {
    // `SPC q q` names `*scratch*` among the unsaved, so a save that says
    // nothing about it would leave the quit refusing for no reason it gave.
    let (mut session, main, _) = two_modified_buffers();
    let mut scratch = Buffer::scratch();
    scratch.loaded(Document::from_text("notes\n"));
    let scratch = session.open(scratch);
    press(&mut session, "sx");
    session.on_key(Key::Esc);
    session.show(main);
    let writes = save_some(&mut session);
    assert_eq!(writes.len(), 2);
    let name = session.buffer(scratch).name().to_string();
    assert_eq!(session.echo(), Some(format!("Saved 2 buffers; {name} has no file.").as_str()));
}

#[test]
fn save_some_buffers_with_nothing_modified_says_so() {
    let mut session = editing_a_file("fn main() {}\n");
    assert_ne!(m_x(&mut session, "save-some-buffers"), Action::SaveSome);
    assert_eq!(session.echo(), Some("No buffers need saving."));
}

#[test]
fn each_saved_buffer_runs_its_hooks_in_itself() {
    // `save-some-buffers` in Emacs saves each with that buffer current, so an
    // `after-save` command acts on the buffer saved, not the one on screen.
    // `end-of-buffer` shows it: each buffer's own point ends at its own end,
    // and the window on screen is not moved.
    let (mut session, main, _) = two_modified_buffers();
    session
        .apply(hook_set(meowr_command::HookPoint::AfterSave, "end-of-buffer"))
        .expect("resolves");
    m_x(&mut session, "save-some-buffers");
    let writes = session.pending_saves();
    session.saved_some(writes.iter().map(|w| (w.path.clone(), Ok(()))).collect());
    assert_eq!(session.current_buffer_id(), main, "the window was not moved");
    let ids: Vec<BufferId> = (0..session.buffers().len())
        .map(BufferId)
        .filter(|id| session.buffers()[id.0].is_real())
        .collect();
    for id in ids {
        session.show(id);
        let end = session.contents().unwrap().len();
        assert!(session.point() >= end.saturating_sub(1), "{:?} at its end", session.path());
    }
}

#[test]
fn a_hook_that_moves_the_focus_leaves_both_windows_as_they_were() {
    let (mut session, main, other) = two_modified_buffers();
    session.on_key(Key::Ctrl('x'));
    session.on_key(Key::Char('2'));
    session.show(other);
    let (focus, shown) = (session.focus(), session.current_buffer_id());
    session
        .apply(hook_set(meowr_command::HookPoint::AfterSave, "other-window"))
        .expect("resolves");
    save_some(&mut session);
    assert_eq!(session.focus(), focus, "the focus is where it was");
    assert_eq!(session.current_buffer_id(), shown);
    session.on_key(Key::Ctrl('x'));
    session.on_key(Key::Char('o'));
    assert_eq!(session.current_buffer_id(), main, "and the other window still shows main.rs");
}

#[test]
fn a_refused_hook_does_not_drop_another_buffers_format() {
    let (mut session, ..) = two_modified_buffers();
    let mut options = session.options();
    options.format_on_save = true;
    session.set_options(options);
    session
        .apply(hook_set(meowr_command::HookPoint::AfterSave, "format-buffer"))
        .expect("resolves");
    save_some(&mut session);
    let formats: Vec<String> = std::iter::from_fn(|| session.pending_format())
        .map(|f| f.path)
        .collect();
    assert_eq!(formats, ["main.rs", "other.rs"]);
}

// --- a workspace edit across files (#167, ADR-0013 amended) --------------------

fn line_edit(start: (usize, usize), end: (usize, usize), text: &str) -> meowr_domain::LineEdit {
    let at = |(line, column)| meowr_domain::LineColumn { line, column };
    meowr_domain::LineEdit {
        start: at(start),
        end: at(end),
        text: text.to_string(),
    }
}

/// Edits to a file a buffer visits, so the host read nothing.
fn in_open(path: &str, edits: Vec<meowr_domain::LineEdit>) -> meowr_app::FileChange {
    meowr_app::FileChange::Edit(meowr_app::FileEdit {
        path: path.to_string(),
        read: None,
        edits,
    })
}

/// Edits to a file no buffer visits, with the text the host read from it.
fn in_unopened(path: &str, text: &str, edits: Vec<meowr_domain::LineEdit>) -> meowr_app::FileChange {
    meowr_app::FileChange::Edit(meowr_app::FileEdit {
        path: path.to_string(),
        read: Some(Ok(text.to_string())),
        edits,
    })
}

fn workspace_edit(
    asked: meowr_app::Revisions,
    changes: Vec<meowr_app::FileChange>,
) -> meowr_app::WorkspaceEdit {
    meowr_app::WorkspaceEdit {
        from: "rust-analyzer".to_string(),
        asked,
        changes,
    }
}

/// `main.rs` calling `old`, and a rename of `old` reaching `lib.rs`, unopened.
fn renaming_old_to_new(session: &Session) -> meowr_app::WorkspaceEdit {
    workspace_edit(
        session.revisions(),
        vec![
            in_open("main.rs", vec![line_edit((0, 0), (0, 3), "new")]),
            in_unopened("lib.rs", "fn old() {}\n", vec![line_edit((0, 3), (0, 6), "new")]),
        ],
    )
}

fn text_of(session: &Session, path: &str) -> Option<String> {
    session
        .buffers()
        .iter()
        .find(|b| b.is_real() && b.path() == Some(path))
        .and_then(|b| b.document())
        .map(|d| d.text().to_string())
}

#[test]
fn a_workspace_edit_lands_in_an_open_buffer_and_an_unopened_file() {
    let mut session = editing_a_file("old();\n");
    let main = session.current_buffer_id();
    let edit = renaming_old_to_new(&session);
    assert_eq!(session.apply_workspace_edit(edit), Action::Redraw);

    assert_eq!(session.contents().unwrap(), "new();\n");
    assert_eq!(text_of(&session, "lib.rs").as_deref(), Some("fn new() {}\n"));
    assert_eq!(session.current_buffer_id(), main, "lib.rs is opened, not shown");
    let lib = session.buffers().iter().find(|b| b.path() == Some("lib.rs")).unwrap();
    assert!(lib.is_modified(), "and left unsaved");
    assert_eq!(
        session.echo(),
        Some("rust-analyzer changed 2 files; 2 are unsaved, and save-some-buffers saves them.")
    );
}

#[test]
fn a_workspace_edit_is_one_undo_step_in_each_buffer() {
    let mut session = editing_a_file("old();\n");
    let edit = renaming_old_to_new(&session);
    session.apply_workspace_edit(edit);
    press(&mut session, "u");
    assert_eq!(session.contents().unwrap(), "old();\n");
    let lib = (0..session.buffers().len())
        .map(BufferId)
        .find(|id| session.buffers()[id.0].path() == Some("lib.rs"))
        .unwrap();
    session.show(lib);
    press(&mut session, "u");
    assert_eq!(session.contents().unwrap(), "fn old() {}\n", "back to the file as read");
}

#[test]
fn a_read_only_buffer_refuses_the_edit_everywhere_and_is_named() {
    let mut session = editing_a_file("old();\n");
    press(&mut session, " tr");
    let signals = session.signals();
    let edit = renaming_old_to_new(&session);
    session.apply_workspace_edit(edit);
    assert_eq!(session.contents().unwrap(), "old();\n");
    assert_eq!(text_of(&session, "lib.rs"), None, "lib.rs was not opened");
    assert_eq!(
        session.echo(),
        Some("main.rs is read-only, so rust-analyzer's edit was not applied.")
    );
    assert_eq!(session.signals(), signals, "said as a formatter's refusal is, without the bell");
}

#[test]
fn a_buffer_typed_into_since_the_question_refuses_the_edit_everywhere() {
    let mut session = editing_a_file("old();\n");
    let edit = renaming_old_to_new(&session);
    press(&mut session, "sx");
    session.on_key(Key::Esc);
    let typed = session.contents().unwrap().to_string();
    session.apply_workspace_edit(edit);
    assert_eq!(session.contents().unwrap(), typed);
    assert_eq!(text_of(&session, "lib.rs"), None);
    assert_eq!(
        session.echo(),
        Some("main.rs changed while rust-analyzer was working, so its edit was not applied.")
    );
}

#[test]
fn a_buffer_opened_and_typed_into_after_the_question_refuses_the_edit() {
    // Not stamped, because it was not open when the question went out, so the
    // server answered from the file -- which the typing no longer matches.
    let mut session = editing("scratch\n");
    let asked = session.revisions();
    let edit = |asked| {
        workspace_edit(asked, vec![in_open("main.rs", vec![line_edit((0, 0), (0, 3), "new")])])
    };
    session.set_path("main.rs");
    session.on_resize(40, 10);
    session.loaded(Document::from_text("old();\n"));
    session.apply_workspace_edit(edit(asked.clone()));
    assert_eq!(session.contents().unwrap(), "new();\n", "unmodified, so the file's text");

    press(&mut session, "u");
    press(&mut session, "sx");
    session.on_key(Key::Esc);
    let typed = session.contents().unwrap().to_string();
    session.apply_workspace_edit(edit(asked));
    assert_eq!(session.contents().unwrap(), typed);
}

#[test]
fn a_position_past_the_end_of_any_file_applies_nothing_and_names_it() {
    let mut session = editing_a_file("old();\n");
    let edit = workspace_edit(
        session.revisions(),
        vec![
            in_open("main.rs", vec![line_edit((0, 0), (0, 3), "new")]),
            in_unopened("lib.rs", "fn old() {}\n", vec![line_edit((9, 0), (9, 3), "new")]),
        ],
    );
    session.apply_workspace_edit(edit);
    assert_eq!(session.contents().unwrap(), "old();\n");
    assert_eq!(text_of(&session, "lib.rs"), None);
    assert_eq!(
        session.echo(),
        Some("rust-analyzer's edit could not be applied to lib.rs: line 10 is past its end.")
    );
}

#[test]
fn overlapping_replacements_in_one_file_apply_nothing_anywhere() {
    let mut session = editing_a_file("old();\n");
    let edit = workspace_edit(
        session.revisions(),
        vec![
            in_unopened("lib.rs", "fn old() {}\n", vec![line_edit((0, 3), (0, 6), "new")]),
            in_open(
                "main.rs",
                vec![line_edit((0, 0), (0, 3), "new"), line_edit((0, 1), (0, 4), "x")],
            ),
        ],
    );
    session.apply_workspace_edit(edit);
    assert_eq!(text_of(&session, "lib.rs"), None);
    assert_eq!(
        session.echo(),
        Some("rust-analyzer's edit could not be applied to main.rs: two of its replacements overlap.")
    );
}

#[test]
fn an_unreadable_file_applies_nothing_and_names_it() {
    let mut session = editing_a_file("old();\n");
    let edit = workspace_edit(
        session.revisions(),
        vec![
            in_open("main.rs", vec![line_edit((0, 0), (0, 3), "new")]),
            meowr_app::FileChange::Edit(meowr_app::FileEdit {
                path: "lib.rs".to_string(),
                read: Some(Err("permission denied".to_string())),
                edits: vec![line_edit((0, 3), (0, 6), "new")],
            }),
        ],
    );
    session.apply_workspace_edit(edit);
    assert_eq!(session.contents().unwrap(), "old();\n");
    assert_eq!(
        session.echo(),
        Some("lib.rs could not be read (permission denied), so rust-analyzer's edit was not applied.")
    );
}

#[test]
fn a_resource_operation_refuses_the_whole_edit() {
    let mut session = editing_a_file("mod old;\n");
    let edit = workspace_edit(
        session.revisions(),
        vec![
            in_open("main.rs", vec![line_edit((0, 4), (0, 7), "new")]),
            meowr_app::FileChange::Rename {
                from: "old.rs".to_string(),
                to: "new.rs".to_string(),
            },
        ],
    );
    session.apply_workspace_edit(edit);
    assert_eq!(session.contents().unwrap(), "mod old;\n");
    assert_eq!(
        session.echo(),
        Some("rust-analyzer's edit renames old.rs, and meowr does not move files for an edit, so nothing was applied.")
    );
}

#[test]
fn workspace_edit_positions_are_utf16_columns() {
    // `é` is one UTF-16 unit and two bytes; `😀` is two units and four bytes.
    let mut session = editing_a_file("é😀old\n");
    let edit = workspace_edit(
        session.revisions(),
        vec![in_open("main.rs", vec![line_edit((0, 3), (0, 6), "new")])],
    );
    session.apply_workspace_edit(edit);
    assert_eq!(session.contents().unwrap(), "é😀new\n");
    assert_eq!(session.echo(), Some("rust-analyzer changed 1 file; 1 is unsaved, and save-some-buffers saves it."));
}

#[test]
fn a_workspace_edit_that_changes_nothing_says_so() {
    let mut session = editing_a_file("old();\n");
    let edit = workspace_edit(session.revisions(), vec![in_open("main.rs", vec![])]);
    assert_eq!(session.apply_workspace_edit(edit), Action::Redraw);
    assert!(!session.is_modified());
    assert_eq!(session.echo(), Some("rust-analyzer's edit changes nothing."));
}

#[test]
fn a_file_named_twice_in_one_edit_applies_nothing() {
    // The protocol lets `documentChanges` name a document twice, each against
    // the text the one before left. Planned as one set against the text as it
    // stands, the second would be refused after the first had applied.
    let mut session = editing_a_file("old();\n");
    let edit = workspace_edit(
        session.revisions(),
        vec![
            in_open("main.rs", vec![line_edit((0, 0), (0, 3), "new")]),
            in_open("main.rs", vec![line_edit((0, 3), (0, 4), "[")]),
        ],
    );
    session.apply_workspace_edit(edit);
    assert_eq!(session.contents().unwrap(), "old();\n");
    assert_eq!(
        session.echo(),
        Some("rust-analyzer's edit could not be applied to main.rs: it names the file twice.")
    );
}

#[test]
fn a_buffer_killed_and_reopened_since_the_question_refuses_the_edit() {
    // The server answered against the killed buffer's unsaved text. The
    // reopened buffer counts its revisions from the start again, so the same
    // typing brings it to the stamped number with different text.
    let mut session = editing_a_file("old();\n");
    press(&mut session, "sx");
    session.on_key(Key::Esc);
    let edit = renaming_old_to_new(&session);
    m_x(&mut session, "kill-buffer-without-saving");
    let mut reopened = Buffer::new();
    reopened.set_path("main.rs", &session.languages());
    reopened.loaded(Document::from_text("old();\n"));
    session.open(reopened);
    press(&mut session, "sy");
    session.on_key(Key::Esc);
    let typed = session.contents().unwrap().to_string();

    session.apply_workspace_edit(edit);
    assert_eq!(session.contents().unwrap(), typed);
    assert_eq!(
        session.echo(),
        Some("main.rs changed while rust-analyzer was working, so its edit was not applied.")
    );
}

#[test]
fn a_buffer_killed_since_the_question_refuses_the_edit_rather_than_reading_the_file() {
    let mut session = editing_a_file("old();\n");
    press(&mut session, "sx");
    session.on_key(Key::Esc);
    let asked = session.revisions();
    m_x(&mut session, "kill-buffer-without-saving");
    let edit = workspace_edit(
        asked,
        vec![in_unopened("main.rs", "old();\n", vec![line_edit((0, 0), (0, 3), "new")])],
    );
    session.apply_workspace_edit(edit);
    assert_eq!(text_of(&session, "main.rs"), None, "not opened from the file");
    assert_eq!(
        session.echo(),
        Some("main.rs changed while rust-analyzer was working, so its edit was not applied.")
    );
}

#[test]
fn a_file_the_picker_is_still_reading_is_not_opened_twice() {
    let mut session = editing_a_file("old();\n");
    chose(&mut session, "lib.rs");
    let edit = workspace_edit(
        session.revisions(),
        vec![in_unopened("lib.rs", "fn old() {}\n", vec![line_edit((0, 3), (0, 6), "new")])],
    );
    session.apply_workspace_edit(edit);
    session.opened("lib.rs", Opened::Read("fn old() {}\n".to_string()));
    assert_eq!(session.contents().unwrap(), "fn new() {}\n", "the edited buffer is shown");
    let visiting = session.buffers().iter().filter(|b| b.is_real() && b.path() == Some("lib.rs"));
    assert_eq!(visiting.count(), 1);
}

// --- `SPC k r` renames the symbol at the point (#168, ADR-0013) ----------------

use meowr_app::{EditReply, Prepared, RenameApply, RenameQuestion, RenameSite};

/// `SPC k r` in `main.rs`, and where the rename was asked about.
fn asked_to_rename(text: &str) -> (Session, RenameSite) {
    let mut session = editing_a_file(text);
    session.on_key(Key::Char(' '));
    session.on_key(Key::Char('k'));
    assert_eq!(session.on_key(Key::Char('r')), Action::Rename);
    match session.pending_rename() {
        Some(RenameQuestion::Prepare(site)) => (session, site),
        other => panic!("asked to prepare first, not {other:?}"),
    }
}

/// Clear the seeded read, type `name` into it, and take the rename it asks for.
fn renaming_to(session: &mut Session, name: &str) -> Option<RenameApply> {
    let seeded = session.narrow_view().expect("a read is open").input.chars().count();
    for _ in 0..seeded {
        session.on_key(Key::Backspace);
    }
    match answer(session, name) {
        Action::Rename => match session.pending_rename() {
            Some(RenameQuestion::Apply(apply)) => Some(apply),
            other => panic!("asked to rename, not {other:?}"),
        },
        _ => None,
    }
}

#[test]
fn spc_k_r_first_asks_the_server_whether_the_point_can_be_renamed() {
    let (session, site) = asked_to_rename("old();\n");
    assert_eq!(
        site,
        RenameSite {
            path: "main.rs".to_string(),
            buffer: session.current_buffer_id(),
            at: 0,
            revision: session.revision(),
        }
    );
}

#[test]
fn the_new_name_is_read_seeded_with_the_servers_placeholder_then_asked_for() {
    let (mut session, site) = asked_to_rename("old();\n");
    session.rename_prepared(&site, Prepared::Placeholder("old".to_string()));
    assert_eq!(asking(&mut session), "Rename old to: ");
    assert_eq!(session.narrow_view().unwrap().input, "old");

    let stamps = session.revisions();
    let apply = renaming_to(&mut session, "new").expect("a rename to ask for");
    assert_eq!(
        apply,
        RenameApply {
            site,
            name: "new".to_string(),
            asked: stamps,
        }
    );
}

#[test]
fn without_a_placeholder_the_read_is_seeded_from_the_range_or_the_symbol_asked_about() {
    let (mut session, site) = asked_to_rename("old_name();\n");
    let at = |line, column| meowr_domain::LineColumn { line, column };
    session.rename_prepared(&site, Prepared::Range { start: at(0, 0), end: at(0, 3) });
    assert_eq!(session.narrow_view().unwrap().input, "old");
    session.on_key(Key::Esc);

    // The symbol where the rename was asked, not where the point went while
    // the server thought: moving the point is not an edit, and the request
    // goes out at the place asked about.
    let (mut session, site) = asked_to_rename("old_name(); other\n");
    session.set_selection(Selection::at("old_name(); ot".len()));
    session.rename_prepared(&site, Prepared::Default);
    assert_eq!(session.narrow_view().unwrap().input, "old_name");
}

#[test]
fn an_empty_name_renames_nothing() {
    let (mut session, site) = asked_to_rename("old();\n");
    session.rename_prepared(&site, Prepared::Placeholder("old".to_string()));
    assert_eq!(renaming_to(&mut session, ""), None);
    assert_eq!(session.pending_rename(), None);
    assert_eq!(session.echo(), Some("No name given, so nothing was renamed."));
}

#[test]
fn a_server_that_will_not_rename_here_is_said_in_its_words() {
    let (mut session, site) = asked_to_rename("fn main() {}\n");
    session.rename_prepared(&site, Prepared::Refused("No references found at position".into()));
    assert!(session.narrow_view().is_none(), "no name is asked for");
    assert_eq!(session.echo(), Some("rust-analyzer: No references found at position."));

    session.rename_prepared(&site, Prepared::Nothing);
    assert_eq!(session.echo(), Some("Nothing to rename at the point."));

    session.rename_prepared(&site, Prepared::Unsupported);
    assert_eq!(session.echo(), Some("rust-analyzer cannot rename."), "refused before a name is typed");

    // meowr's own failure is not put in the server's mouth.
    session.rename_prepared(&site, Prepared::Failed("the language server connection is closed".into()));
    assert_eq!(session.echo(), Some("Could not rename: the language server connection is closed."));
}

#[test]
fn a_prepared_rename_for_text_typed_over_since_is_dropped_and_said() {
    // ADR-0013: a rename computed before the last keystroke has to be asked
    // again, and the message says why.
    let (mut session, site) = asked_to_rename("old();\n");
    press(&mut session, "sx");
    session.on_key(Key::Esc);
    session.rename_prepared(&site, Prepared::Placeholder("old".into()));
    assert!(session.narrow_view().is_none());
    assert_eq!(
        session.echo(),
        Some("main.rs changed while rust-analyzer was working, so nothing was renamed.")
    );
}

#[test]
fn a_prepared_rename_for_a_buffer_no_longer_on_screen_is_dropped_quietly() {
    // `found`'s rule: the file changed, so the answer is about somewhere else.
    let (mut session, site) = asked_to_rename("old();\n");
    let mut other = Buffer::scratch();
    other.loaded(Document::from_text("elsewhere\n"));
    session.open(other);
    assert_eq!(session.rename_prepared(&site, Prepared::Placeholder("old".into())), Action::Nothing);
    assert!(session.narrow_view().is_none());
}

#[test]
fn the_renamed_edit_is_applied_as_a_workspace_edit_from_the_server() {
    let (mut session, site) = asked_to_rename("old();\n");
    session.rename_prepared(&site, Prepared::Placeholder("old".into()));
    let apply = renaming_to(&mut session, "new").unwrap();
    let changes = vec![
        in_open("main.rs", vec![line_edit((0, 0), (0, 3), "new")]),
        in_unopened("lib.rs", "fn old() {}\n", vec![line_edit((0, 3), (0, 6), "new")]),
    ];
    assert_eq!(session.renamed(&apply, EditReply::Edit(changes)), Action::Redraw);
    assert_eq!(session.contents().unwrap(), "new();\n");
    assert_eq!(text_of(&session, "lib.rs").as_deref(), Some("fn new() {}\n"));
    assert_eq!(
        session.echo(),
        Some("rust-analyzer changed 2 files; 2 are unsaved, and save-some-buffers saves them.")
    );
}

#[test]
fn a_refused_rename_is_said_in_the_servers_words() {
    // A module rename, which meowr's declared capabilities let rust-analyzer
    // refuse (#157, ADR-0013 amended).
    let (mut session, site) = asked_to_rename("mod old;\n");
    session.rename_prepared(&site, Prepared::Placeholder("old".into()));
    let apply = renaming_to(&mut session, "new").unwrap();
    session.renamed(&apply, EditReply::Refused("Client does not support rename capability".into()));
    assert_eq!(session.contents().unwrap(), "mod old;\n");
    assert_eq!(session.echo(), Some("rust-analyzer: Client does not support rename capability."));

    session.renamed(&apply, EditReply::Nothing);
    assert_eq!(session.echo(), Some("rust-analyzer renamed nothing."));

    session.renamed(&apply, EditReply::Failed("an edit meowr could not read".into()));
    assert_eq!(session.echo(), Some("Could not rename: an edit meowr could not read."));
}

#[test]
fn spc_k_r_in_a_buffer_with_no_file_is_refused() {
    let mut session = editing("old();\n");
    press(&mut session, " kr");
    assert_eq!(session.pending_rename(), None);
    assert_eq!(session.echo(), Some("No file to rename in."));
}

// --- `SPC k a` code actions and `SPC k o` organize imports (#169) ---------------

use meowr_app::{ChosenCodeAction, CodeAction, CodeActionEffect, CodeActionQuestion, CodeActionsAsked, CodeActionsFound};

/// `use std::fmt;` with an unused-import warning on it, the point on `fmt`.
fn with_an_unused_import() -> Session {
    let mut session = diagnosed(
        "use std::fmt;\nfn main() {}\n",
        vec![diagnostic(4..12, "unused import: `std::fmt`")],
    );
    session.set_selection(Selection::at(10));
    session
}

fn asked_for_actions(session: &mut Session, keys: &str) -> CodeActionsAsked {
    press(session, keys);
    match session.pending_code_actions() {
        Some(CodeActionQuestion::List(asked)) => asked,
        other => panic!("asked for the actions first, not {other:?}"),
    }
}

fn action(title: &str, kind: &str, effect: CodeActionEffect) -> CodeAction {
    CodeAction {
        title: title.to_string(),
        kind: (!kind.is_empty()).then(|| kind.to_string()),
        effect,
        command: None,
    }
}

fn removing_the_import() -> CodeActionEffect {
    CodeActionEffect::Edit(vec![in_open("main.rs", vec![line_edit((0, 0), (1, 0), "")])])
}

fn chosen(session: &mut Session) -> ChosenCodeAction {
    match session.pending_code_actions() {
        Some(CodeActionQuestion::Apply(chosen)) => chosen,
        other => panic!("an action to apply, not {other:?}"),
    }
}

#[test]
fn spc_k_a_asks_about_the_line_with_the_diagnostics_on_it() {
    // lsp-mode's `lsp--region-or-line` and `lsp-cur-line-diagnostics`: with no
    // region, the whole line, and every diagnostic on it.
    let mut session = with_an_unused_import();
    let asked = asked_for_actions(&mut session, " ka");
    assert_eq!(asked.path, "main.rs");
    assert_eq!(asked.buffer, session.current_buffer_id());
    assert_eq!(asked.range, 0..13, "the line, not the point");
    assert_eq!(asked.diagnostics, vec![diagnostic(4..12, "unused import: `std::fmt`")]);
    assert_eq!(asked.only, None);
    assert_eq!(asked.asked, session.revisions());
}

#[test]
fn a_selection_is_asked_about_rather_than_the_line() {
    let mut session = with_an_unused_import();
    press(&mut session, "w");
    let selected = session.selection();
    let asked = asked_for_actions(&mut session, " ka");
    assert!(!selected.is_empty(), "a word selected");
    assert_eq!(asked.range, selected.range());
}

#[test]
fn the_actions_are_offered_as_a_picker_of_titles_and_the_chosen_one_applied() {
    let mut session = with_an_unused_import();
    let asked = asked_for_actions(&mut session, " ka");
    let found = CodeActionsFound::Actions(vec![
        action("Remove all unused imports", "quickfix", removing_the_import()),
        action("Merge imports", "refactor.rewrite", CodeActionEffect::Resolve("{\"data\":1}".into())),
    ]);
    assert_eq!(session.code_actions_found(&asked, found), Action::Redraw);
    let titles: Vec<String> = session
        .narrow_view()
        .expect("a picker")
        .rows
        .iter()
        .map(|row| row.text.clone())
        .collect();
    assert_eq!(titles, ["Remove all unused imports", "Merge imports"]);

    assert_eq!(session.on_key(Key::Enter), Action::CodeActions);
    let chosen = chosen(&mut session);
    assert_eq!(chosen.action.title, "Remove all unused imports");
    let CodeActionEffect::Edit(changes) = chosen.action.effect.clone() else {
        panic!("its edit");
    };
    session.code_action_applied(&chosen, EditReply::Edit(changes));
    assert_eq!(session.contents().unwrap(), "fn main() {}\n");
}

#[test]
fn an_action_that_needs_resolving_is_asked_for_as_it_was_sent() {
    let mut session = with_an_unused_import();
    let asked = asked_for_actions(&mut session, " ka");
    let found = CodeActionsFound::Actions(vec![
        action("Remove all unused imports", "quickfix", CodeActionEffect::Resolve("{\"data\":7}".into())),
        action("Merge imports", "refactor.rewrite", CodeActionEffect::Resolve("{\"data\":8}".into())),
    ]);
    session.code_actions_found(&asked, found);
    session.on_key(Key::Enter);
    let chosen = chosen(&mut session);
    assert_eq!(chosen.action.effect, CodeActionEffect::Resolve("{\"data\":7}".into()));
    assert_eq!(chosen.asked, asked, "stamped when the actions were asked for");

    session.code_action_applied(&chosen, EditReply::Nothing);
    assert_eq!(session.echo(), Some("Remove all unused imports changed nothing."));
    session.code_action_applied(&chosen, EditReply::Refused("content modified".into()));
    assert_eq!(session.echo(), Some("rust-analyzer: content modified."));
}

#[test]
fn a_command_only_action_is_said_not_run() {
    let mut session = with_an_unused_import();
    let asked = asked_for_actions(&mut session, " ka");
    let found = CodeActionsFound::Actions(vec![
        action("Extract variable", "refactor.extract", CodeActionEffect::Command),
        action("Merge imports", "refactor.rewrite", CodeActionEffect::Resolve("{}".into())),
    ]);
    session.code_actions_found(&asked, found);
    assert_ne!(session.on_key(Key::Enter), Action::CodeActions);
    assert_eq!(session.pending_code_actions(), None);
    assert_eq!(
        session.echo(),
        Some("Extract variable is a command for rust-analyzer to run, and meowr does not run server commands.")
    );
}

#[test]
fn one_action_is_applied_without_a_picker_and_none_is_said() {
    // `lsp-auto-execute-action`, on by default: a list of one is not a choice.
    let mut session = with_an_unused_import();
    let asked = asked_for_actions(&mut session, " ka");
    let found = CodeActionsFound::Actions(vec![action("Remove the import", "quickfix", removing_the_import())]);
    assert_eq!(session.code_actions_found(&asked, found), Action::CodeActions);
    assert!(session.narrow_view().is_none());
    assert_eq!(chosen(&mut session).action.title, "Remove the import");

    session.code_actions_found(&asked, CodeActionsFound::Actions(Vec::new()));
    assert_eq!(session.echo(), Some("No code actions here."));
    session.code_actions_found(&asked, CodeActionsFound::Unsupported);
    assert_eq!(session.echo(), Some("rust-analyzer offers no code actions."));
}

#[test]
fn actions_found_for_text_typed_over_since_are_dropped_and_said() {
    let mut session = with_an_unused_import();
    let asked = asked_for_actions(&mut session, " ka");
    press(&mut session, "gg");
    press(&mut session, "sx");
    session.on_key(Key::Esc);
    let found = CodeActionsFound::Actions(vec![action("Remove the import", "quickfix", removing_the_import())]);
    session.code_actions_found(&asked, found);
    assert!(session.narrow_view().is_none());
    assert_eq!(session.pending_code_actions(), None);
    assert_eq!(
        session.echo(),
        Some("main.rs changed while rust-analyzer was working, so no action was offered.")
    );
}

#[test]
fn spc_k_o_applies_the_organize_imports_action() {
    let mut session = with_an_unused_import();
    let asked = asked_for_actions(&mut session, " ko");
    assert_eq!(asked.only.as_deref(), Some("source.organizeImports"));
    let found = CodeActionsFound::Actions(vec![
        action("Fill match arms", "quickfix", CodeActionEffect::Command),
        action("Organize Imports", "source.organizeImports", removing_the_import()),
    ]);
    assert_eq!(session.code_actions_found(&asked, found), Action::CodeActions);
    assert_eq!(chosen(&mut session).action.title, "Organize Imports");
}

#[test]
fn spc_k_o_where_the_server_has_no_such_action_says_so_and_what_it_has() {
    // rust-analyzer (#157): no `source.*` action, and `only` is ignored.
    let mut session = with_an_unused_import();
    let asked = asked_for_actions(&mut session, " ko");
    let found = CodeActionsFound::Actions(vec![
        action("Remove all unused imports", "quickfix", CodeActionEffect::Resolve("{}".into())),
        action("Merge imports", "refactor.rewrite", CodeActionEffect::Resolve("{}".into())),
    ]);
    assert_ne!(session.code_actions_found(&asked, found), Action::CodeActions);
    assert_eq!(
        session.echo(),
        Some("rust-analyzer has no action to organize imports; here it offers Remove all unused imports and Merge imports.")
    );

    session.code_actions_found(&asked, CodeActionsFound::Actions(Vec::new()));
    assert_eq!(session.echo(), Some("rust-analyzer has no action to organize imports."));
}

#[test]
fn code_actions_in_a_buffer_with_no_file_are_refused() {
    let mut session = editing("use std::fmt;\n");
    press(&mut session, " ka");
    assert_eq!(session.pending_code_actions(), None);
    assert_eq!(session.echo(), Some("No file to act on."));
}

#[test]
fn cancelling_the_actions_picker_leaves_the_next_picker_a_picker() {
    let (mut session, _, other) = two_modified_buffers();
    let asked = asked_for_actions(&mut session, " ka");
    let found = CodeActionsFound::Actions(vec![
        action("Remove all unused imports", "quickfix", removing_the_import()),
        action("Merge imports", "refactor.rewrite", CodeActionEffect::Resolve("{}".into())),
    ]);
    session.code_actions_found(&asked, found);
    session.on_key(Key::Esc);

    press(&mut session, " .");
    type_name(&mut session, "other");
    session.on_key(Key::Enter);
    assert_eq!(session.current_buffer_id(), other, "the buffer picker still switches buffers");
}

#[test]
fn only_the_diagnostics_on_the_points_line_go_with_the_question() {
    // One ending exactly where this line starts is the line before's: ranges
    // end before their last byte.
    let mut session = diagnosed(
        "let a = 1;\nlet b = 2;\n",
        vec![diagnostic(4..11, "ends at the newline"), diagnostic(0..3, "first line")],
    );
    session.set_selection(Selection::at(15));
    let asked = asked_for_actions(&mut session, " ka");
    assert_eq!(asked.diagnostics, Vec::<Diagnostic>::new());

    session.set_selection(Selection::at(2));
    let asked = asked_for_actions(&mut session, " ka");
    assert_eq!(asked.diagnostics.len(), 2, "both start on the first line");
}

#[test]
fn an_action_with_an_edit_and_a_command_says_its_command_was_not_run() {
    let mut session = with_an_unused_import();
    let asked = asked_for_actions(&mut session, " ka");
    let mut both = action("Organize Imports", "source.organizeImports", removing_the_import());
    both.command = Some("gopls.tidy".to_string());
    session.code_actions_found(&asked, CodeActionsFound::Actions(vec![both]));
    let chosen = chosen(&mut session);
    let CodeActionEffect::Edit(changes) = chosen.action.effect.clone() else {
        panic!("its edit");
    };
    session.code_action_applied(&chosen, EditReply::Edit(changes));
    assert_eq!(session.contents().unwrap(), "fn main() {}\n");
    assert_eq!(
        session.echo(),
        Some("rust-analyzer changed 1 file; 1 is unsaved, and save-some-buffers saves it. Its command, gopls.tidy, was not run.")
    );
}

#[test]
fn an_action_with_nothing_to_apply_is_not_called_a_command() {
    let mut session = with_an_unused_import();
    let asked = asked_for_actions(&mut session, " ka");
    let found = CodeActionsFound::Actions(vec![
        action("Merge imports", "refactor.rewrite", CodeActionEffect::Nothing),
        action("Remove all unused imports", "quickfix", removing_the_import()),
    ]);
    session.code_actions_found(&asked, found);
    session.on_key(Key::Enter);
    assert_eq!(session.pending_code_actions(), None);
    assert_eq!(session.echo(), Some("Merge imports has no edit rust-analyzer gave meowr to apply."));
}

// --- undo moves the revision (#171) ---------------------------------------------

#[test]
fn a_foreign_edit_stamped_before_an_undo_is_refused_after_it() {
    // ADR-0013's stamp means "the text has not moved", and an undo moves it.
    let mut session = editing_a_file("fn main(){}\n");
    press(&mut session, "sx");
    session.on_key(Key::Esc);
    let asked = session.revision();
    press(&mut session, "u");
    assert_ne!(session.revision(), asked, "an undo is an edit");

    session.apply_foreign_edit(foreign("main.rs", asked, vec![(0..0, "// \n")]));
    assert!(!session.contents().unwrap().starts_with("// "));
    assert!(session.echo().unwrap_or_default().contains("changed while"));

    let redone = session.revision();
    session.on_key(Key::Ctrl('r'));
    assert_ne!(session.revision(), redone, "and so is a redo");
}

// --- `SPC f m` moves this file and `SPC f C` copies it (#178) --------------------

use meowr_app::{FileDeletion, FileDone, FileRefusal, FileRequest, FileWork};

fn asked_to_move(keys: &str, to: &str) -> (Session, FileRequest) {
    let mut session = editing_a_file("fn main() {}\n");
    session.set_recent_files(vec!["main.rs".to_string(), "other.rs".to_string()]);
    press(&mut session, keys);
    assert_eq!(session.narrow_view().expect("a read is open").input, "main.rs", "seeded");
    let seeded = "main.rs".len();
    for _ in 0..seeded {
        session.on_key(Key::Backspace);
    }
    assert_eq!(answer(&mut session, to), Action::FileWorked);
    let asked = session.pending_file_work().expect("a question for the host");
    (session, asked)
}

#[test]
fn spc_f_m_reads_a_target_seeded_with_this_files_path() {
    let mut session = editing_a_file("fn main() {}\n");
    press(&mut session, " fm");
    assert_eq!(asking(&mut session), "Move file to: ");
    assert_eq!(session.narrow_view().unwrap().input, "main.rs");

    session.on_key(Key::Esc);
    assert_eq!(session.echo(), Some("Nothing was moved."));
    press(&mut session, " fC");
    assert_eq!(asking(&mut session), "Copy file to: ");
}

#[test]
fn moving_this_file_asks_the_host_and_the_buffer_follows_it() {
    let (mut session, asked) = asked_to_move(" fm", "src/main.rs");
    assert_eq!(
        asked,
        FileRequest {
            from: "main.rs".to_string(),
            to: "src/main.rs".to_string(),
            work: FileWork::Move,
            force: false,
        }
    );

    session.file_done(&asked, Ok(FileDone { path: "src/main.rs".into(), made_directories: true }));
    assert_eq!(session.path(), Some("src/main.rs"), "the buffer follows the file");
    assert_eq!(
        session.take_autosaves_to_forget(),
        [meowr_app::autosave_path("main.rs")],
        "and its auto-save does not stay behind under the old name"
    );
    let recent: Vec<&str> = session.recent_file_paths().collect();
    assert_eq!(recent, ["src/main.rs", "other.rs"], "the recent files are rewritten in place");
    assert_eq!(session.echo(), Some("Moved main.rs to src/main.rs, creating src/."));
}

#[test]
fn copying_this_file_opens_the_copy_and_leaves_this_buffer_alone() {
    let (mut session, asked) = asked_to_move(" fC", "copy.rs");
    assert_eq!(asked.work, FileWork::Copy);

    assert_eq!(
        session.file_done(&asked, Ok(FileDone { path: "copy.rs".into(), made_directories: false })),
        Action::Open,
        "the host reads it, as it reads any file being opened"
    );
    assert_eq!(session.path(), Some("main.rs"), "this buffer is where it was");
    assert_eq!(session.echo(), Some("Copied main.rs to copy.rs."));

    session.opened("copy.rs", Opened::Read("fn main() {}\n".to_string()));
    assert_eq!(session.path(), Some("copy.rs"), "and the copy is what you are looking at");
    assert!(session.buffers().iter().any(|b| b.is_real() && b.path() == Some("main.rs")));
}

#[test]
fn a_target_that_is_taken_is_refused_and_named_with_the_way_past() {
    let (mut session, asked) = asked_to_move(" fm", "other.rs");
    let signals = session.signals();
    session.file_done(&asked, Err(FileRefusal::Taken("other.rs".into())));
    assert_eq!(session.path(), Some("main.rs"), "nothing moved");
    assert_eq!(
        session.echo(),
        Some("other.rs exists. SPC u SPC f m to move it there anyway.")
    );
    assert_eq!(session.signals(), signals, "said as a dropped edit is, without the bell");
}

#[test]
fn a_universal_argument_says_to_go_ahead_anyway() {
    let mut session = editing_a_file("fn main() {}\n");
    session.on_key(Key::Ctrl('u'));
    press(&mut session, " fm");
    for _ in 0.."main.rs".len() {
        session.on_key(Key::Backspace);
    }
    answer(&mut session, "other.rs");
    assert!(session.pending_file_work().expect("a question").force, "SPC u carried across the read");

    press(&mut session, " fm");
    answer(&mut session, "other.rs");
    assert!(!session.pending_file_work().expect("a question").force, "and only that once");
}

#[test]
fn a_write_that_failed_moves_nothing_in_the_session() {
    let (mut session, asked) = asked_to_move(" fm", "src/main.rs");
    session.file_done(&asked, Err(FileRefusal::Failed("permission denied".into())));
    assert_eq!(session.path(), Some("main.rs"));
    assert!(session.take_autosaves_to_forget().is_empty());
    assert_eq!(session.echo(), Some("main.rs could not be moved: permission denied."));
}

#[test]
fn a_buffer_with_no_file_has_nothing_to_move_or_copy() {
    let mut session = editing("fn main() {}\n");
    press(&mut session, " fm");
    assert!(session.narrow_view().is_none());
    assert_eq!(session.echo(), Some("This buffer has no file."));
    assert_eq!(session.pending_file_work(), None);
}

#[test]
fn an_empty_answer_moves_nothing() {
    let mut session = editing_a_file("fn main() {}\n");
    press(&mut session, " fm");
    for _ in 0.."main.rs".len() {
        session.on_key(Key::Backspace);
    }
    assert_ne!(answer(&mut session, ""), Action::FileWorked);
    assert_eq!(session.echo(), Some("No name given, so nothing was moved."));
}

#[test]
fn moving_onto_a_file_a_buffer_has_open_leaves_one_buffer_with_that_path() {
    // #150's spelling exists so that one path means one buffer. The file the
    // other buffer held is what was just written over, so it is killed where
    // nothing is lost by that -- #173's rule for a deleted file.
    let (mut session, asked) = asked_to_move(" fm", "other.rs");
    let mut other = Buffer::new();
    other.set_path("other.rs", &session.languages());
    other.loaded(Document::from_text("fn other() {}\n"));
    let other = session.open(other);
    let main = session
        .buffers()
        .iter()
        .position(|b| b.is_real() && b.path() == Some("main.rs"))
        .map(BufferId)
        .expect("main.rs is open");
    session.show(main);

    session.file_done(&asked, Ok(FileDone { path: "other.rs".into(), made_directories: false }));
    assert_eq!(session.path(), Some("other.rs"));
    assert!(!session.buffer(other).is_real(), "the buffer whose file is gone is killed");
    let visiting = session
        .buffers()
        .iter()
        .filter(|b| b.is_real() && b.path() == Some("other.rs"));
    assert_eq!(visiting.count(), 1);
    assert_eq!(session.echo(), Some("Moved main.rs to other.rs."));
}

#[test]
fn a_displaced_buffer_with_unsaved_text_is_kept_and_said() {
    let (mut session, asked) = asked_to_move(" fm", "other.rs");
    let mut other = Buffer::new();
    other.set_path("other.rs", &session.languages());
    other.loaded(Document::from_text("fn other() {}\n"));
    let other = session.open(other);
    press(&mut session, "sx");
    session.on_key(Key::Esc);
    let main = session
        .buffers()
        .iter()
        .position(|b| b.is_real() && b.path() == Some("main.rs"))
        .map(BufferId)
        .expect("main.rs is open");
    session.show(main);

    session.file_done(&asked, Ok(FileDone { path: "other.rs".into(), made_directories: false }));
    assert!(session.buffer(other).is_real(), "unsaved text is not thrown away");
    assert_eq!(
        session.echo(),
        Some("Moved main.rs to other.rs. The buffer that had other.rs still holds unsaved text under that name.")
    );
}

#[test]
fn the_name_in_the_way_is_the_one_said_even_when_a_directory_was_typed() {
    let (mut session, asked) = asked_to_move(" fm", "src");
    session.file_done(&asked, Err(FileRefusal::Taken("src/main.rs".into())));
    assert_eq!(
        session.echo(),
        Some("src/main.rs exists. SPC u SPC f m to move it there anyway."),
    );
}

#[test]
fn a_file_already_where_it_is_is_said_so() {
    let (mut session, asked) = asked_to_move(" fm", "./main.rs");
    session.file_done(&asked, Err(FileRefusal::Itself));
    assert_eq!(session.echo(), Some("main.rs is already where it is."));
}

// --- `SPC f D` deletes this file (#179) -----------------------------------------

#[test]
fn spc_f_d_alone_deletes_nothing_and_names_the_file_and_the_way_past() {
    let mut session = editing_a_file("fn main() {}\n");
    let signals = session.signals();
    press(&mut session, " fD");

    assert_eq!(session.pending_file_deletion(), None, "the host was not asked");
    assert_eq!(
        session.echo(),
        Some("main.rs would be deleted, not trashed, and nothing here can undo it. SPC u SPC f D to delete it.")
    );
    assert!(session.signals() > signals, "a refusal that is worse than the four meowr already makes");
}

#[test]
fn spc_u_spc_f_d_asks_the_host_and_an_unmodified_buffer_goes_with_its_file() {
    let mut session = editing_a_file("fn main() {}\n");
    // `SPC u` and then `SPC f D`: the go-ahead is its own leader press, as
    // the pty scenario for #178 presses it.
    press(&mut session, " u f");
    assert_eq!(session.on_key(Key::Char('D')), Action::FileDeleted);

    let asked = session.pending_file_deletion().expect("a question for the host");
    assert_eq!(asked, FileDeletion { path: "main.rs".to_string() });

    session.file_deleted(&asked, Ok(()));
    assert_eq!(session.echo(), Some("Deleted main.rs."));
    assert!(
        !session.buffers().iter().any(|b| b.is_real() && b.path() == Some("main.rs")),
        "the buffer is a copy of something that is not there any more"
    );
    assert_eq!(
        session.take_autosaves_to_forget(),
        [meowr_app::autosave_path("main.rs")],
        "and the auto-save goes with the file"
    );
}

#[test]
fn a_modified_buffer_survives_its_file_and_is_told_it_holds_the_only_copy() {
    let mut session = editing_a_file("fn main() {}\n");
    press(&mut session, "sZ");
    session.on_key(Key::Esc);
    assert!(session.is_modified(), "the buffer really is modified");

    press(&mut session, " u fD");
    let asked = session.pending_file_deletion().expect("a question for the host");
    session.file_deleted(&asked, Ok(()));

    assert_eq!(session.path(), Some("main.rs"), "the buffer stays: its text is all there is");
    assert!(session.is_modified(), "and stays modified");
    assert_eq!(
        session.echo(),
        Some("Deleted main.rs. This buffer is still open and modified, so its text is the only copy of that work left.")
    );
    assert_eq!(
        session.take_autosaves_to_forget(),
        [meowr_app::autosave_path("main.rs")],
        "the auto-save goes even so: it could only be recovered into a file that is gone"
    );
}

#[test]
fn a_buffer_with_no_file_has_nothing_to_delete() {
    let mut session = editing("fn main() {}\n");
    press(&mut session, " u fD");

    assert_eq!(session.echo(), Some("This buffer has no file."));
    assert_eq!(session.pending_file_deletion(), None, "not even with the go-ahead");
}

#[test]
fn a_file_that_would_not_go_says_why_and_leaves_everything_where_it_was() {
    let mut session = editing_a_file("fn main() {}\n");
    press(&mut session, " u fD");
    let asked = session.pending_file_deletion().expect("a question for the host");

    session.file_deleted(&asked, Err("permission denied".to_string()));

    assert_eq!(session.path(), Some("main.rs"), "the buffer is where it was");
    assert!(
        session.take_autosaves_to_forget().is_empty(),
        "and the auto-save stays: the file it recovers into is still there"
    );
    assert_eq!(session.echo(), Some("main.rs could not be deleted: permission denied."));
}

#[test]
fn the_go_ahead_is_spent_on_the_delete_it_was_given_for() {
    let mut session = editing_a_file("fn main() {}\n");
    press(&mut session, " u fD");
    assert!(session.pending_file_deletion().is_some(), "the first one went through");

    // And the next `SPC f D` asks again rather than inheriting it.
    press(&mut session, " fD");
    assert_eq!(session.pending_file_deletion(), None);
    assert!(session.echo().expect("it says why").contains("would be deleted"));
}

#[test]
fn a_deleted_file_leaves_the_recent_files() {
    let mut session = editing_a_file("fn main() {}\n");
    session.set_recent_files(vec!["main.rs".to_string(), "other.rs".to_string()]);
    press(&mut session, " u fD");
    let asked = session.pending_file_deletion().expect("a question for the host");

    session.file_deleted(&asked, Ok(()));

    let recent: Vec<&str> = session.recent_file_paths().collect();
    assert_eq!(recent, ["other.rs"], "a row that opens nothing is no offer");
}

#[test]
fn a_refused_delete_does_not_leave_the_go_ahead_behind() {
    // A command that returns early never reaches the dispatch that consumes
    // the argument. If `SPC u` survived the refusal, the *next* `SPC f D`
    // would delete a file without ever stopping to ask -- which is reachable
    // from meowr's own start state: launched with no file, refused once, then
    // a path arrives.
    let mut session = editing("fn main() {}\n");
    press(&mut session, " u fD");
    assert_eq!(session.echo(), Some("This buffer has no file."));

    session.set_path("main.rs");
    press(&mut session, " fD");

    assert_eq!(session.pending_file_deletion(), None, "nothing was asked of the host");
    assert!(
        session.echo().expect("it says why").contains("would be deleted"),
        "it stopped to ask, as a first press must"
    );
}

#[test]
fn a_refused_move_does_not_leave_the_go_ahead_behind() {
    // The same leak on #178's rows, where it writes over a file instead.
    let mut session = editing("fn main() {}\n");
    press(&mut session, " u fm");
    assert_eq!(session.echo(), Some("This buffer has no file."));

    session.set_path("main.rs");
    press(&mut session, " fm");
    for _ in 0.."main.rs".len() {
        session.on_key(Key::Backspace);
    }
    answer(&mut session, "other.rs");

    assert!(
        !session.pending_file_work().expect("a question").force,
        "a name that is taken should still be refused"
    );
}

#[test]
fn an_unmodified_buffer_is_taken_out_of_every_window_that_showed_it() {
    // `doom/kill-this-buffer-in-all-windows`. A second window still pointing
    // at the buffer would be showing text for a file that is not there.
    let mut session = editing_a_file("fn main() {}\n");
    session.on_resize(80, 8);
    session.on_key(Key::Ctrl('x'));
    session.on_key(Key::Char('2'));
    assert_eq!(session.window_count(), 2);
    let showing = session.current_buffer_id();
    assert!(
        session.windows().order().iter().all(|&w| session.windows().window(w).buffer == showing),
        "both windows are on it to begin with"
    );

    press(&mut session, " u fD");
    let asked = session.pending_file_deletion().expect("a question for the host");
    session.file_deleted(&asked, Ok(()));

    assert_eq!(session.window_count(), 2, "the windows stay; only the buffer goes");
    assert!(
        session.windows().order().iter().all(|&w| session.windows().window(w).buffer != showing),
        "neither window is left pointing at it"
    );
}

#[test]
fn a_read_only_buffer_is_still_this_files_buffer_and_the_file_can_go() {
    // #173: read-only is a fact about the buffer's text, not about the file.
    // Nothing else pins this -- marking `delete-this-file` mutating would
    // reverse a decision silently.
    let mut session = editing_a_file("fn main() {}\n");
    session.set_read_only(true);

    press(&mut session, " u fD");

    assert_eq!(
        session.pending_file_deletion(),
        Some(FileDeletion { path: "main.rs".to_string() }),
        "the delete went through"
    );
}

// --- `SPC q F` kills every buffer (#176) ----------------------------------------

#[test]
fn spc_q_f_closes_the_other_windows_kills_every_buffer_and_says_how_many() {
    // `doom/kill-all-buffers`: `save-some-buffers`, `delete-other-windows`,
    // then the buffers. Nothing here is modified, so the first step has
    // nothing to do and the sentence is only about the kill.
    let (mut session, _a, _b, _c) = three_buffers();
    session.on_resize(80, 12);
    session.on_key(Key::Ctrl('x'));
    session.on_key(Key::Char('2'));
    assert_eq!(session.window_count(), 2);

    press(&mut session, " qF");

    assert_eq!(session.window_count(), 1, "Doom's `delete-other-windows` first");
    let live: Vec<&str> = session
        .buffers()
        .iter()
        .filter(|b| b.is_real())
        .map(|b| b.name())
        .collect();
    assert_eq!(live, ["*scratch*"], "the fallback is left and nothing else");
    assert_eq!(session.echo(), Some("Killed 3 buffers."));
}

#[test]
fn spc_q_f_saves_what_it_can_first_and_kills_once_the_writes_are_back() {
    let (mut session, a, _b, _c) = three_buffers();
    session.show(a);
    press(&mut session, "sZ");
    session.on_key(Key::Esc);
    assert!(session.is_modified(), "there is something to save");

    press(&mut session, " q");
    assert_eq!(
        session.on_key(Key::Char('F')),
        Action::SaveSome,
        "the writes are the host's half"
    );
    let writes = session.pending_saves();
    assert_eq!(writes.len(), 1, "the one modified buffer");
    assert_eq!(
        session.buffers().iter().filter(|b| b.is_real()).count(),
        3,
        "and nothing is killed while a write is still in flight"
    );

    session.saved_some(vec![("a.rs".to_string(), Ok(()))]);

    let live: Vec<&str> = session
        .buffers()
        .iter()
        .filter(|b| b.is_real())
        .map(|b| b.name())
        .collect();
    assert_eq!(live, ["*scratch*"]);
    assert_eq!(session.echo(), Some("Saved 1 buffer, killed 3."));
}

#[test]
fn a_buffer_that_cannot_be_saved_stops_the_whole_kill_and_names_the_way_past() {
    // After `save-some-buffers` has run, anything still modified could not be
    // written at all, so killing it throws away the only copy of that work.
    // #173's shape: say what is in the way, name the key that means yes, and
    // touch nothing.
    let (mut session, a, _b, _c) = three_buffers();
    let mut typed_in = Buffer::scratch();
    typed_in.loaded(Document::from_text("notes\n"));
    let scratch = session.open(typed_in);
    session.show(scratch);
    press(&mut session, "sZ");
    session.on_key(Key::Esc);
    session.show(a);

    // No write to wait for: the one modified buffer cannot be written at all,
    // so `save-some-buffers` comes straight back and the refusal is immediate.
    press(&mut session, " qF");

    assert_eq!(
        session.buffers().iter().filter(|b| b.is_real()).count(),
        4,
        "nothing was killed"
    );
    assert_eq!(session.window_count(), 1);
    assert_eq!(
        session.echo(),
        Some("*scratch* has no file. SPC u SPC q F to kill them anyway.")
    );
}

#[test]
fn spc_u_spc_q_f_kills_what_could_not_be_saved_too() {
    let (mut session, a, _b, _c) = three_buffers();
    let mut typed_in = Buffer::scratch();
    typed_in.loaded(Document::from_text("notes\n"));
    let scratch = session.open(typed_in);
    session.show(scratch);
    press(&mut session, "sZ");
    session.on_key(Key::Esc);
    session.show(a);

    press(&mut session, " u qF");

    let live: Vec<&str> = session
        .buffers()
        .iter()
        .filter(|b| b.is_real())
        .map(|b| b.name())
        .collect();
    assert_eq!(live, ["*scratch*"], "a fresh one, not the one that was typed in");
    assert_eq!(
        session.echo(),
        Some("Killed 4 buffers; *scratch* has no file."),
        "a go-ahead pressed straight off still says what work went"
    );
}

#[test]
fn a_refused_kill_all_does_not_leave_the_go_ahead_behind() {
    // The leak #179 found, on this row: a refused `SPC u SPC q F` must not
    // arm the next bare `SPC q F`.
    let mut session = editing("notes\n");
    press(&mut session, "sZ");
    session.on_key(Key::Esc);

    press(&mut session, " u qF");
    assert_eq!(
        session.buffers().iter().filter(|b| b.is_real()).count(),
        1,
        "forced, so it went"
    );

    // And a second buffer with work in it is refused again rather than killed.
    let mut typed_in = Buffer::scratch();
    typed_in.loaded(Document::from_text("more\n"));
    let second = session.open(typed_in);
    session.show(second);
    press(&mut session, "sZ");
    session.on_key(Key::Esc);

    press(&mut session, " qF");

    assert!(
        session.echo().expect("it says why").contains("to kill them anyway"),
        "it stopped to ask"
    );
}

#[test]
fn a_write_that_failed_stops_the_kill_as_an_unsaveable_buffer_does() {
    // The buffer still holds the only copy of that work, whether the reason is
    // that it could not be written or that the write did not land.
    let (mut session, a, _b, _c) = three_buffers();
    session.show(a);
    press(&mut session, "sZ");
    session.on_key(Key::Esc);

    press(&mut session, " q");
    assert_eq!(session.on_key(Key::Char('F')), Action::SaveSome);
    let _ = session.pending_saves();
    session.saved_some(vec![("a.rs".to_string(), Err("permission denied".to_string()))]);

    assert_eq!(
        session.buffers().iter().filter(|b| b.is_real()).count(),
        3,
        "nothing was killed"
    );
    let echo = session.echo().expect("it says why");
    assert!(echo.contains("could not write a.rs: permission denied"), "{echo}");
    assert!(echo.contains("to kill them anyway"), "{echo}");
}

#[test]
fn the_window_left_behind_fills_the_frame() {
    // `close_others` only rewrites the tree; `layout` is what gives the
    // survivor the space. Every window command pairs the two, and a kill that
    // closed windows without laying out would draw the last one in the half
    // of the frame it had while it was split.
    let laid_out = {
        let (mut session, _a, _b, _c) = three_buffers();
        session.on_resize(80, 12);
        session.on_key(Key::Ctrl('x'));
        session.on_key(Key::Char('2'));
        session.on_key(Key::Ctrl('x'));
        session.on_key(Key::Char('1'));
        session.windows().window(session.windows().focus()).area
    };

    let (mut session, _a, _b, _c) = three_buffers();
    session.on_resize(80, 12);
    session.on_key(Key::Ctrl('x'));
    session.on_key(Key::Char('2'));
    press(&mut session, " qF");

    assert_eq!(
        session.windows().window(session.windows().focus()).area,
        laid_out,
        "as `C-x 1` leaves it"
    );
}

// --- the scratch rows (#180) ----------------------------------------------------

#[test]
fn spc_f_x_shows_the_scratch_buffer_below_and_closes_its_window_on_a_second_press() {
    // `doom/toggle-scratch-buffer` deletes the scratch buffer's window when it
    // has one and pops it up otherwise. Nothing floats (ADR-0004), so the
    // popup is the split below that `SPC b X` already makes.
    let mut session = editing_a_file("fn main() {}\n");
    session.on_resize(80, 12);

    press(&mut session, " fx");
    assert_eq!(session.window_count(), 2, "split below rather than floated");
    assert_eq!(session.current().name(), "*scratch*");

    press(&mut session, " fx");
    assert_eq!(session.window_count(), 1, "the second press closes its window");
    assert_eq!(session.path(), Some("main.rs"), "and leaves you where you were");
}

#[test]
fn spc_f_capital_x_switches_to_the_scratch_buffer_in_this_window() {
    // #174 routes it to the command that already exists, which `SPC b x` also
    // reaches; the author's own `b x` and `b X` are left exactly as they are.
    let mut session = editing_a_file("fn main() {}\n");
    session.on_resize(80, 12);

    press(&mut session, " fX");

    assert_eq!(session.window_count(), 1, "this window, not another");
    assert_eq!(session.current().name(), "*scratch*");
}

#[test]
fn spc_p_capital_x_switches_to_a_scratch_named_for_the_project() {
    // The Project is ADR-0010's repository and the session never holds it, so
    // this is a round trip like every other project row: the host resolves the
    // root and the name is taken from it.
    let mut session = editing_a_file("fn main() {}\n");
    session.on_resize(80, 12);

    press(&mut session, " p");
    assert_eq!(session.on_key(Key::Char('X')), Action::FromRoot);
    assert_eq!(
        session.pending_from_root().expect("a question for the host"),
        meowr_app::FromRoot::Scratch {
            about: "main.rs".to_string(),
            row: meowr_app::ScratchRow::Switch,
        }
    );

    session.scratch_from_root(
        "main.rs",
        meowr_app::ScratchRow::Switch,
        Ok("/w/meowr".to_string()),
    );

    assert_eq!(session.current().name(), "*scratch: meowr*");
    assert_eq!(session.window_count(), 1, "this window, as `SPC f X` does");
}

#[test]
fn a_second_project_gets_a_scratch_of_its_own() {
    let mut session = editing_a_file("fn main() {}\n");
    session.on_resize(80, 12);
    let file = session.current_buffer_id();

    press(&mut session, " pX");
    session.scratch_from_root("main.rs", meowr_app::ScratchRow::Switch, Ok("/w/meowr".into()));
    let first = session.current_buffer_id();

    // Back to the file, because a Project's scratch answers with its own root
    // rather than asking again -- which is the next test.
    session.show(file);
    press(&mut session, " pX");
    session.scratch_from_root("main.rs", meowr_app::ScratchRow::Switch, Ok("/w/wagit".into()));

    assert_eq!(session.current().name(), "*scratch: wagit*");
    assert_ne!(session.current_buffer_id(), first, "not the same buffer");
    let scratches: Vec<&str> = session
        .buffers()
        .iter()
        .filter(|b| b.scratch_of().is_some())
        .map(|b| b.name())
        .collect();
    assert_eq!(scratches, ["*scratch: meowr*", "*scratch: wagit*"]);
}

#[test]
fn spc_p_x_toggles_and_closes_from_inside_without_asking_for_the_project_again() {
    let mut session = editing_a_file("fn main() {}\n");
    session.on_resize(80, 12);

    press(&mut session, " px");
    // Drained as the host drains it, so a leftover cannot be mistaken for a
    // second question below.
    assert!(session.pending_from_root().is_some(), "asked once");
    session.scratch_from_root("main.rs", meowr_app::ScratchRow::Toggle, Ok("/w/meowr".into()));
    assert_eq!(session.window_count(), 2, "split below, as nothing floats");
    assert_eq!(session.current().name(), "*scratch: meowr*");

    // The second press comes from inside the scratch, which has no path -- so
    // there is no Project to find it by, and it has to carry its own.
    press(&mut session, " p");
    assert_eq!(
        session.on_key(Key::Char('x')),
        Action::Redraw,
        "answered here rather than asked of the host"
    );
    assert_eq!(session.pending_from_root(), None);
    assert_eq!(session.window_count(), 1, "its window is closed");
    assert_eq!(session.path(), Some("main.rs"), "and you are back where you were");
}

#[test]
fn outside_a_project_both_scratch_rows_refuse_by_name() {
    // ADR-0010 names what is missing rather than guessing a directory, and the
    // sentence is the host's, which is what knows where it looked.
    let mut session = editing_a_file("fn main() {}\n");
    session.on_resize(80, 12);
    press(&mut session, " px");
    session.scratch_from_root(
        "main.rs",
        meowr_app::ScratchRow::Toggle,
        Err("main.rs is in no repository.".to_string()),
    );
    assert_eq!(session.echo(), Some("main.rs is in no repository."));
    assert_eq!(session.window_count(), 1, "nothing was shown");
    assert!(!session.buffers().iter().any(|b| b.scratch_of().is_some()));

    // And a buffer with no file has no Project to ask about at all.
    let mut session = editing("notes\n");
    press(&mut session, " pX");
    assert_eq!(
        session.echo(),
        Some("This buffer has no file, so it is in no project.")
    );
    assert_eq!(session.pending_from_root(), None, "not even asked");
}

#[test]
fn a_modified_project_scratch_still_refuses_a_quit_by_name() {
    // #166 names a modified scratch among the unsaved, which is why neither
    // scratch needs persisting: nothing is lost quietly.
    let mut session = editing_a_file("fn main() {}\n");
    session.on_resize(80, 12);
    press(&mut session, " pX");
    session.scratch_from_root("main.rs", meowr_app::ScratchRow::Switch, Ok("/w/meowr".into()));
    press(&mut session, "sZ");
    session.on_key(Key::Esc);
    assert!(session.is_modified());

    press(&mut session, " qq");

    let echo = session.echo().expect("it says why");
    assert!(echo.contains("*scratch: meowr*"), "named among the unsaved: {echo}");
}

#[test]
fn toggling_the_only_window_when_it_is_already_the_scratch_refuses() {
    // Reachable because a session started with no file argument is already
    // sitting in the global scratch: the very first `SPC f x` finds it on
    // screen with nowhere to go back to. Doom's `delete-window` signals on a
    // sole window too.
    let mut session = editing("");
    session.on_resize(80, 12);
    assert_eq!(session.current().name(), "*scratch*", "started in it");

    press(&mut session, " fx");

    assert_eq!(session.window_count(), 1);
    assert_eq!(session.echo(), Some("This is the only window."));
}

#[test]
fn a_project_scratch_saved_to_a_file_stops_being_one() {
    // A scratch visits no file by definition, so writing one somewhere ends
    // it: `SPC p X` must not hand that buffer back as the Project's scratch.
    let mut session = editing_a_file("fn main() {}\n");
    session.on_resize(80, 12);
    let file = session.current_buffer_id();
    press(&mut session, " pX");
    let _ = session.pending_from_root();
    session.scratch_from_root("main.rs", meowr_app::ScratchRow::Switch, Ok("/w/meowr".into()));
    let scratch = session.current_buffer_id();

    session.set_path("notes.md");

    assert_eq!(session.buffer(scratch).scratch_of(), None, "it has a file now");
    session.show(file);
    press(&mut session, " pX");
    let _ = session.pending_from_root();
    session.scratch_from_root("main.rs", meowr_app::ScratchRow::Switch, Ok("/w/meowr".into()));
    assert_ne!(
        session.current_buffer_id(),
        scratch,
        "a fresh one, not the file that used to be the scratch"
    );
    assert_eq!(session.current().name(), "*scratch: meowr*");
}

// --- the config rows (#181) -----------------------------------------------------

/// Where the config lives is the host's fact (`config::find`) and the session
/// is told it, which is what these rows need to name a file at all. A path
/// nobody's machine has, like `/w/meowr` next door: the session does no I/O, so
/// nothing here goes near a real home.
const CONFIG: &str = "/w/.config/meowr/init.scm";

#[test]
fn spc_f_capital_p_refuses_when_there_is_nowhere_for_a_config_to_live() {
    // `config::find` answers `None` only when there is no home to look in at
    // all, and a row that cannot name a file has nothing to open. Refused by
    // name, as every other row that cannot find what it is about refuses.
    let mut session = editing_a_file("fn main() {}\n");

    press(&mut session, " fP");

    let echo = session.echo().expect("it says why");
    assert!(echo.contains("no home to look in"), "names what is missing: {echo}");
}

#[test]
fn spc_f_capital_p_asks_the_host_to_read_the_config() {
    let mut session = editing_a_file("fn main() {}\n");
    session.set_config_path(Some(CONFIG.to_string()));

    press(&mut session, " f");

    assert_eq!(session.on_key(Key::Char('P')), Action::Open);
    assert_eq!(session.pending_open(), Some(CONFIG));
    assert!(
        session.pending_open_may_be_new(),
        "a config that is not there yet is the ordinary case, not a failure"
    );
}

#[test]
fn a_config_that_is_not_there_yet_opens_as_an_empty_buffer_that_says_it_is_new() {
    let mut session = editing_a_file("fn main() {}\n");
    session.set_config_path(Some(CONFIG.to_string()));
    press(&mut session, " fP");

    session.opened(CONFIG, Opened::Missing);

    assert_eq!(session.path(), Some(CONFIG), "visiting the file it will make");
    assert_eq!(
        session.document().expect("an ordinary buffer, not a failure").text().to_string(),
        "",
        "empty, and writing it creates the file"
    );
    let echo = session
        .echo()
        .expect("an empty buffer otherwise implies an empty config");
    assert!(echo.contains("new"), "{echo}");
}

#[test]
fn a_config_that_is_there_opens_with_what_is_in_it_and_says_nothing() {
    let mut session = editing_a_file("fn main() {}\n");
    session.set_config_path(Some(CONFIG.to_string()));
    press(&mut session, " fP");

    session.opened(
        CONFIG,
        Opened::Read("(set-option 'auto-save #t)\n".to_string()),
    );

    assert_eq!(
        session.document().unwrap().text().to_string(),
        "(set-option 'auto-save #t)\n"
    );
    assert_eq!(session.echo(), None, "nothing to say about a config that is there");
}

#[test]
fn a_config_that_will_not_read_is_a_failure_and_not_a_new_file() {
    // The distinction `config.rs` has drawn since #44: `Failed::Unreadable` is
    // "distinct from absent". An empty buffer offered for a config that exists
    // and would not open is a lie you could then write over it.
    let mut session = editing_a_file("fn main() {}\n");
    session.set_config_path(Some(CONFIG.to_string()));
    press(&mut session, " fP");

    session.opened(
        CONFIG,
        Opened::Failed(format!("Permission denied (os error 13): {CONFIG}")),
    );

    assert!(
        session.document().is_none(),
        "a buffer showing why, not one to type in"
    );
}

#[test]
fn spc_f_p_asks_the_host_for_what_is_beside_the_config() {
    // `SPC f F`'s listing (#149) rooted at the config rather than at this
    // buffer: that directory is not a Project, so the project picker would be
    // the wrong tool for it.
    let mut session = editing_a_file("fn main() {}\n");
    session.set_config_path(Some(CONFIG.to_string()));

    press(&mut session, " f");

    assert_eq!(session.on_key(Key::Char('p')), Action::Files);
    assert_eq!(
        session.pending_files().expect("a listing to make"),
        Files::BesideTheConfig(CONFIG.to_string()),
        "the config's own path, for the host to list the directory it is in"
    );
}

#[test]
fn what_is_beside_the_config_is_not_dropped_for_being_about_another_buffer() {
    // The guard `SPC f F` needs is about *its* buffer's directory; the config
    // is nobody's buffer and its path cannot change under the question, so the
    // only thing that can take the answer away is a widget owning the Bottom.
    let mut session = editing_a_file("fn main() {}\n");
    session.set_config_path(Some(CONFIG.to_string()));
    press(&mut session, " fp");
    let about = session.pending_files().expect("a listing to make");

    let listed = Ok(vec![
        "/w/.config/meowr/init.scm".to_string(),
        "/w/.config/meowr/keys.scm".to_string(),
    ]);
    assert_eq!(session.files_listed(&about, listed), Action::Redraw);

    let widget = session.narrowing().expect("the picker opened over them");
    assert_eq!(widget.matches().len(), 2);
}

#[test]
fn spc_f_p_refuses_by_name_when_there_is_no_config_directory_yet() {
    // The host's own sentence, said rather than swallowed: a directory that is
    // not there is the answer, and it names itself.
    let mut session = editing_a_file("fn main() {}\n");
    session.set_config_path(Some(CONFIG.to_string()));
    press(&mut session, " fp");
    let about = session.pending_files().expect("a listing to make");

    session.files_listed(
        &about,
        Err("No such file or directory: /w/.config/meowr".into()),
    );

    assert_eq!(
        session.echo(),
        Some("No such file or directory: /w/.config/meowr")
    );
}

#[test]
fn spc_f_p_refuses_when_there_is_nowhere_for_a_config_to_live() {
    let mut session = editing_a_file("fn main() {}\n");

    press(&mut session, " fp");

    let echo = session.echo().expect("it says why");
    assert!(echo.contains("no home to look in"), "names what is missing: {echo}");
}

#[test]
fn a_file_chosen_while_the_config_was_being_read_is_not_new() {
    // Leave to visit a file that is not there belongs to the row that asked for
    // it, and a read can still be in flight when the next one is asked for. A
    // file the picker listed and has since lost is a failure, and inheriting
    // `SPC f P`'s leave would make it an empty buffer claiming to be new.
    let mut session = editing_a_file("fn main() {}\n");
    session.set_config_path(Some(CONFIG.to_string()));
    press(&mut session, " fP");
    assert!(session.pending_open_may_be_new(), "the config's own read");

    chose(&mut session, "gone.rs");

    assert_eq!(session.pending_open(), Some("gone.rs"));
    assert!(
        !session.pending_open_may_be_new(),
        "this row never asked to visit a file that is not there"
    );
}

#[test]
fn a_save_that_had_to_make_its_directory_says_so() {
    // #172's rule for a copy and a move -- "missing parent directories are
    // created and said" -- reaching the write that meets it: a config
    // directory nobody has made is the ordinary state of a machine that has
    // never had a config. An ordinary save says nothing at all, so this line
    // is there because something happened rather than as a receipt.
    let mut session = editing_a_file("fn main() {}\n");
    session.set_config_path(Some(CONFIG.to_string()));
    press(&mut session, " fP");
    session.opened(CONFIG, Opened::Missing);

    session.mark_saved();
    session.saved_making_directories(CONFIG);

    assert_eq!(
        session.echo(),
        Some("Saved /w/.config/meowr/init.scm, creating /w/.config/meowr/."),
        "the `, creating ...` a copy and a move already say"
    );
}

// --- restarting (#182) ----------------------------------------------------------

#[test]
fn spc_q_r_restarts_when_there_is_nothing_unsaved() {
    // The refusal `SPC q q` makes and no other: a restart is a quit that comes
    // back, so what stands in its way is the work a quit would lose.
    let mut session = editing_a_file("fn main() {}\n");

    press(&mut session, " q");

    assert_eq!(session.on_key(Key::Char('r')), Action::Restart);
}

#[test]
fn spc_q_r_refuses_modified_buffers_and_names_the_way_past() {
    let mut session = editing_a_file("fn main() {}\n");
    press(&mut session, "sedited");
    session.on_key(Key::Esc);

    press(&mut session, " qr");

    let echo = session.echo().expect("it says why");
    assert!(echo.contains("Unsaved: main.rs."), "it names them: {echo}");
    assert!(
        echo.contains("SPC u SPC q r"),
        "and the go-ahead that means it anyway: {echo}"
    );
}

#[test]
fn spc_u_spc_q_r_restarts_past_the_refusal() {
    // `SPC u` is the go-ahead #172 and #173 settled for the rows that cannot
    // be undone, rather than a second command as `SPC q Q` is for the quit.
    let mut session = editing_a_file("fn main() {}\n");
    press(&mut session, "sedited");
    session.on_key(Key::Esc);

    press(&mut session, " u q");

    assert_eq!(session.on_key(Key::Char('r')), Action::Restart);
}

#[test]
fn the_go_ahead_is_spent_on_the_restart_it_was_given_for() {
    // Taken before the refusal, as `ask_file_work` takes it. A restart is an
    // intention rather than an edit, so the buffer is still modified after one
    // -- and the next press has to ask again rather than ride the last one's
    // go-ahead.
    let mut session = editing_a_file("fn main() {}\n");
    press(&mut session, "sedited");
    session.on_key(Key::Esc);

    press(&mut session, " u q");
    assert_eq!(session.on_key(Key::Char('r')), Action::Restart);

    press(&mut session, " qr");
    let echo = session.echo().expect("it asks again");
    assert!(echo.contains("Unsaved: main.rs."), "{echo}");
}
