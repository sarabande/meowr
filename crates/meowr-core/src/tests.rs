use super::*;

#[test]
fn a_selection_carries_an_axis_and_a_kind() {
    let sel = Selection::typed(Axis::Expand, Kind::CHAR, 2, 5);

    assert_eq!(sel.axis(), Some(Axis::Expand));
    assert_eq!(sel.kind(), Some(Kind::CHAR));
}

#[test]
fn a_bare_point_has_no_type() {
    let sel = Selection::at(3);

    assert_eq!(sel.axis(), None);
    assert_eq!(sel.kind(), None);
    assert!(sel.is_empty());
}

#[test]
fn a_kind_may_name_any_registered_thing_not_just_a_fixed_set() {
    // Meow types a thing selection as (expand . <thing>), so every registered
    // thing is a possible kind. `defun` is not one of the literals.
    let sel = Selection::typed(Axis::Expand, Kind::thing("defun"), 0, 9);

    assert_eq!(sel.kind().unwrap().name(), "defun");
    assert_ne!(sel.kind(), Some(Kind::WORD));
}

#[test]
fn line_appears_on_both_axes() {
    // Nine combinations, not eight: `line` is the one kind that occurs both
    // expanding and selecting, so axis and kind cannot be collapsed into one
    // flat enum.
    let expanding = Selection::typed(Axis::Expand, Kind::LINE, 0, 4);
    let selecting = Selection::typed(Axis::Select, Kind::LINE, 0, 4);

    assert_ne!(expanding.selection_type(), selecting.selection_type());
    assert_eq!(expanding.kind(), selecting.kind());
}

#[test]
fn direction_runs_from_mark_to_pos() {
    let forward = Selection::typed(Axis::Expand, Kind::CHAR, 2, 7);
    let backward = Selection::typed(Axis::Expand, Kind::CHAR, 7, 2);

    assert!(!forward.is_backward());
    assert!(backward.is_backward());
    // Both cover the same bytes; only the direction differs.
    assert_eq!(forward.range(), 2..7);
    assert_eq!(backward.range(), 2..7);
}

#[test]
fn reversing_swaps_mark_and_pos_without_moving_the_selection() {
    let sel = Selection::typed(Axis::Expand, Kind::CHAR, 2, 7).reversed();

    assert!(sel.is_backward());
    assert_eq!(sel.range(), 2..7);
    assert_eq!(sel.pos(), 2);
    assert_eq!(sel.selection_type(), Some((Axis::Expand, Kind::CHAR)));
}

#[test]
fn replacing_the_selection_pushes_the_old_one_onto_the_history() {
    let mut state = SelectionState::new(Selection::at(0));

    state.select(Selection::typed(Axis::Expand, Kind::CHAR, 0, 3));
    state.select(Selection::typed(Axis::Expand, Kind::CHAR, 3, 6));

    assert_eq!(state.current().range(), 3..6);
    assert_eq!(state.history_depth(), 2);
}

#[test]
fn popping_the_history_restores_the_previous_selection() {
    let mut state = SelectionState::new(Selection::at(0));
    state.select(Selection::typed(Axis::Expand, Kind::CHAR, 0, 3));
    state.select(Selection::typed(Axis::Expand, Kind::CHAR, 3, 6));

    assert!(state.pop());

    assert_eq!(state.current().range(), 0..3);
    assert_eq!(state.history_depth(), 1);
}

#[test]
fn popping_an_empty_history_reports_that_rather_than_panicking() {
    let mut state = SelectionState::new(Selection::at(0));

    assert!(!state.pop());
    assert_eq!(state.current().pos(), 0);
}

#[test]
fn some_selections_are_made_without_recording_history() {
    let mut state = SelectionState::new(Selection::at(0));
    state.select(Selection::typed(Axis::Expand, Kind::CHAR, 0, 3));

    state.select_without_history(Selection::typed(Axis::Expand, Kind::CHAR, 3, 6));

    assert_eq!(state.current().range(), 3..6);
    assert_eq!(state.history_depth(), 1);
}

use meowr_domain::Document;

fn state_at(pos: usize) -> SelectionState {
    SelectionState::new(Selection::at(pos))
}

#[test]
fn moving_right_advances_by_a_whole_character() {
    let doc = Document::from_text("héllo");
    let mut st = state_at(0);

    right(&doc, &mut st, Axis::Select);
    assert_eq!(st.current().pos(), 1);

    // "é" is two bytes; a motion must not stop inside it.
    right(&doc, &mut st, Axis::Select);
    assert_eq!(st.current().pos(), 3);
}

#[test]
fn a_plain_character_motion_leaves_no_selection_at_all() {
    // This test asserted the opposite until the differential harness ran:
    // `meow-right` only ever *cancels*, it never creates. `i` on a bare point
    // leaves a bare point.
    let doc = Document::from_text("abcdef");
    let mut st = state_at(0);

    right(&doc, &mut st, Axis::Select);

    assert_eq!(st.current().selection_type(), None);
    assert_eq!(st.current().pos(), 1);
}

#[test]
fn a_plain_character_motion_keeps_a_char_selection_and_moves_its_end() {
    // The one selection that survives a plain motion, per the docstring:
    // "Will cancel all other selection, except char selection."
    let doc = Document::from_text("abcdef");
    let mut st = state_at(0);
    st.select(Selection::typed(Axis::Expand, Kind::CHAR, 1, 3));

    right(&doc, &mut st, Axis::Select);

    assert_eq!(st.current().range(), 1..4);
    assert_eq!(
        st.history_depth(),
        1,
        "moving an end is not a new selection"
    );
}

#[test]
fn an_expanding_character_motion_grows_the_selection() {
    let doc = Document::from_text("abcdef");
    let mut st = state_at(1);

    right(&doc, &mut st, Axis::Expand);
    right(&doc, &mut st, Axis::Expand);

    assert_eq!(st.current().range(), 1..3);
    assert_eq!(
        st.current().selection_type(),
        Some((Axis::Expand, Kind::CHAR))
    );
}

#[test]
fn expanding_from_a_selection_of_another_kind_keeps_its_span_as_a_char_selection() {
    // Also asserted backwards before the harness ran. `meow-right-expand`
    // rebuilds the selection from the *live* mark, so the extent survives and
    // only the type changes.
    let doc = Document::from_text("abcdef");
    let mut st = state_at(0);
    st.select(Selection::typed(Axis::Expand, Kind::LINE, 0, 4));

    right(&doc, &mut st, Axis::Expand);

    assert_eq!(st.current().range(), 0..5);
    assert_eq!(
        st.current().selection_type(),
        Some((Axis::Expand, Kind::CHAR))
    );
}

#[test]
fn motion_stops_at_the_edges_of_the_document() {
    let doc = Document::from_text("ab");
    let mut st = state_at(0);

    left(&doc, &mut st, Axis::Select);
    assert_eq!(st.current().pos(), 0);

    let mut st = state_at(2);
    right(&doc, &mut st, Axis::Select);
    assert_eq!(st.current().pos(), 2);
}

#[test]
fn vertical_motion_keeps_the_column() {
    let doc = Document::from_text("abcdef\nghijkl\nmnopqr\n");
    let mut st = state_at(3); // column 3 of line 0

    next(&doc, &mut st, Axis::Select);
    assert_eq!(st.current().pos(), 10); // column 3 of line 1

    prev(&doc, &mut st, Axis::Select);
    assert_eq!(st.current().pos(), 3);
}

#[test]
fn the_goal_column_survives_a_short_line() {
    // Emacs keeps the column you were aiming for across a shorter line rather
    // than truncating it permanently; so does this.
    let doc = Document::from_text("abcdef\nxy\nghijkl\n");
    let mut st = state_at(4); // column 4 of line 0

    next(&doc, &mut st, Axis::Select);
    assert_eq!(st.current().pos(), 9); // clamped to end of "xy"

    next(&doc, &mut st, Axis::Select);
    assert_eq!(st.current().pos(), 14); // column 4 again, not column 2
}

#[test]
fn a_horizontal_motion_forgets_the_goal_column() {
    // Lines: "abcdef\n" 0..6, "xy\n" 7..9, "ghijkl\n" 10..16, "mnopqr\n" 17..23
    let doc = Document::from_text("abcdef\nxy\nghijkl\nmnopqr\n");
    let mut st = state_at(4); // column 4 of line 0
    next(&doc, &mut st, Axis::Select);
    assert_eq!(st.current().pos(), 9); // clamped to the end of "xy"

    right(&doc, &mut st, Axis::Select); // crosses onto line 2, and abandons the goal
    next(&doc, &mut st, Axis::Select);

    // Column 0 of line 3, because the goal of 4 was forgotten. Had it been
    // kept, this would be byte 21.
    assert_eq!(st.current().pos(), 17);
}

#[test]
fn vertical_motion_expands_as_a_char_selection_not_a_line_one() {
    // `meow-next-expand` makes an `(expand . char)` selection. Only
    // `meow-line` makes a line one -- the kind names what built the
    // selection, not how far it happens to reach.
    let doc = Document::from_text("abcdef\nghijkl\n");
    let mut st = state_at(2);

    next(&doc, &mut st, Axis::Expand);

    assert_eq!(st.current().range(), 2..9);
    assert_eq!(st.current().kind(), Some(Kind::CHAR));
}

#[test]
fn marking_a_word_selects_it_expandably() {
    let doc = Document::from_text("alpha beta gamma");
    let mut st = state_at(7); // inside "beta"

    mark_word(&doc, &mut st, false);

    assert_eq!(st.current().range(), 6..10);
    assert_eq!(
        st.current().selection_type(),
        Some((Axis::Expand, Kind::WORD))
    );
}

#[test]
fn a_word_motion_on_its_own_makes_a_non_expandable_selection() {
    // meow-next-thing: new-type is (select . word) unless the current
    // selection was already (expand . word).
    let doc = Document::from_text("alpha beta gamma");
    let mut st = state_at(0);

    next_word(&doc, &mut st, 1);

    assert_eq!(
        st.current().selection_type(),
        Some((Axis::Select, Kind::WORD))
    );
    assert_eq!(st.current().range(), 0..5);
}

#[test]
fn a_word_motion_after_marking_continues_expanding() {
    // This is why the docstring says "start the selection with meow-mark-word".
    let doc = Document::from_text("alpha beta gamma");
    let mut st = state_at(0);
    mark_word(&doc, &mut st, false); // (expand . word) over "alpha"

    next_word(&doc, &mut st, 1);

    assert_eq!(
        st.current().selection_type(),
        Some((Axis::Expand, Kind::WORD))
    );
    assert_eq!(st.current().range(), 0..10); // "alpha beta"
}

#[test]
fn a_word_motion_over_a_selection_of_a_different_kind_cancels_it_first() {
    let doc = Document::from_text("alpha beta gamma");
    let mut st = state_at(0);
    // A line selection whose point sits at the start of "beta".
    st.select(Selection::typed(Axis::Expand, Kind::LINE, 0, 6));

    next_word(&doc, &mut st, 1);

    // The mark starts fresh at the old point. Had the line selection been
    // extended instead of discarded, the mark would still be 0 and the range
    // would be 0..10.
    assert_eq!(
        st.current().selection_type(),
        Some((Axis::Select, Kind::WORD))
    );
    assert_eq!(st.current().range(), 6..10);
}

#[test]
fn back_word_moves_and_selects_backwards() {
    let doc = Document::from_text("alpha beta gamma");
    let mut st = state_at(16);

    back_word(&doc, &mut st, 1);

    assert_eq!(st.current().range(), 11..16);
    assert!(st.current().is_backward());
}

#[test]
fn a_symbol_includes_characters_a_word_does_not() {
    let doc = Document::from_text("foo_bar-baz qux");
    let mut st = state_at(0);
    mark_symbol(&doc, &mut st, false);
    assert_eq!(st.current().range(), 0..11);
    assert_eq!(st.current().kind(), Some(Kind::SYMBOL));

    let mut st = state_at(0);
    mark_word(&doc, &mut st, false);
    assert_eq!(st.current().range(), 0..3); // just "foo"
}

#[test]
fn selecting_a_line_stops_before_the_newline() {
    // "Select the current line, eol is not included" -- `meow-line`'s own
    // first line of documentation, which this test used to contradict.
    let doc = Document::from_text("alpha\nbeta\n");
    let mut st = state_at(7);

    select_line(&doc, &mut st, 1);

    assert_eq!(st.current().range(), 6..10);
    assert_eq!(
        st.current().selection_type(),
        Some((Axis::Expand, Kind::LINE))
    );
}

#[test]
fn reversing_flips_the_selection_without_recording_history() {
    let doc = Document::from_text("alpha beta");
    let mut st = state_at(0);
    mark_word(&doc, &mut st, false);
    let before = st.history_depth();

    reverse(&mut st);

    assert!(st.current().is_backward());
    assert_eq!(st.history_depth(), before);
}

#[test]
fn cancelling_leaves_a_bare_point_where_the_selection_ended() {
    let doc = Document::from_text("alpha beta");
    let mut st = state_at(0);
    mark_word(&doc, &mut st, false);

    cancel(&mut st);

    assert_eq!(st.current().selection_type(), None);
    assert!(st.current().is_empty());
    assert_eq!(st.current().pos(), 5);
}

#[test]
fn a_block_selection_covers_the_enclosing_bracket_pair() {
    let doc = Document::from_text("call(a, b) end");
    let mut st = state_at(6); // inside the parens

    block(&doc, &mut st, false, None);

    assert_eq!(st.current().range(), 4..10); // "(a, b)"
    assert_eq!(
        st.current().selection_type(),
        Some((Axis::Expand, Kind::BLOCK))
    );
}

#[test]
fn block_selection_respects_nesting() {
    let doc = Document::from_text("f(g(x), y)");
    let mut st = state_at(4); // inside the inner parens

    block(&doc, &mut st, false, None);

    assert_eq!(st.current().range(), 3..6); // "(x)"
}

#[test]
fn a_second_block_selection_widens_to_the_next_pair_out() {
    let doc = Document::from_text("f(g(x), y)");
    let mut st = state_at(4);
    block(&doc, &mut st, false, None);

    block(&doc, &mut st, false, None);

    assert_eq!(st.current().range(), 1..10); // "(g(x), y)"
}

#[test]
fn a_block_selection_reaches_the_block_ahead_of_the_point() {
    // The gap #35 closed, and the reason `meow-block` is not an
    // enclosing-pair search: Emacs scans forward for the delimiter that
    // returns to the depth it started at, which finds a block the point is
    // merely *before* as readily as one it is inside.
    let doc = Document::from_text("foo bar (baz) qux");
    let mut st = state_at(0);

    block(&doc, &mut st, false, None);

    assert_eq!(st.current().range(), 8..13); // `(baz)`
    assert_eq!(
        st.current().selection_type(),
        Some((Axis::Expand, Kind::BLOCK))
    );
}

#[test]
fn reaching_ahead_takes_the_nearest_block_and_not_the_one_after_it() {
    let doc = Document::from_text("a (b) c (d) e");
    let mut st = state_at(0);

    block(&doc, &mut st, false, None);

    assert_eq!(st.current().range(), 2..5);
}

#[test]
fn a_block_selection_ahead_still_widens_when_repeated() {
    // Both halves at once: the first `o` reaches a block it is not inside,
    // and the second widens to the pair enclosing *that* — which is the
    // behaviour that already worked and the reason the two directions cannot
    // simply be swapped.
    let doc = Document::from_text("f(a (b) c)");
    let mut st = state_at(2); // on `a`, inside the outer pair, before the inner
    block(&doc, &mut st, false, None);
    assert_eq!(st.current().range(), 4..7); // `(b)`, ahead of the point

    block(&doc, &mut st, false, None);

    assert_eq!(st.current().range(), 1..10); // `(a (b) c)`
}

#[test]
fn a_backward_block_selection_reaches_the_block_behind_the_point() {
    // `back` is `(xor (meow--direction-backward-p) (< arg 0))`, so a negative
    // argument turns the scan round even with no selection to take a
    // direction from.
    let doc = Document::from_text("a (b) c (d) e");
    let mut st = state_at(12); // on `e`

    block(&doc, &mut st, true, None);

    assert_eq!(st.current().range(), 8..11); // `(d)`, behind the point
    assert!(st.current().is_backward());
}

#[test]
fn an_unbalanced_delimiter_ahead_makes_no_selection() {
    // `(ignore-errors (forward-list …))`: there is no partner to select back
    // to, and Meow makes no selection rather than reaching to the buffer's
    // edge.
    let doc = Document::from_text("a) b");
    let mut st = state_at(0);

    block(&doc, &mut st, false, None);

    assert_eq!(st.current().selection_type(), None);
}

#[test]
fn block_selection_outside_any_pair_leaves_the_selection_alone() {
    let doc = Document::from_text("no brackets here");
    let mut st = state_at(5);

    block(&doc, &mut st, false, None);

    assert_eq!(st.current().selection_type(), None);
}

#[test]
fn a_selection_shifts_when_text_is_inserted_before_it() {
    // Emacs uses markers, so a selection tracks the text it covers rather than
    // the offsets it was made at.
    let mut sel = Selection::typed(Axis::Expand, Kind::WORD, 6, 10);

    sel = sel.after_edit(0, 0, 3); // three bytes inserted at the start

    assert_eq!(sel.range(), 9..13);
}

#[test]
fn a_selection_does_not_move_when_text_is_inserted_after_it() {
    let mut sel = Selection::typed(Axis::Expand, Kind::WORD, 6, 10);

    sel = sel.after_edit(12, 0, 4);

    assert_eq!(sel.range(), 6..10);
}

#[test]
fn a_selection_shrinks_when_text_inside_it_is_deleted() {
    let mut sel = Selection::typed(Axis::Expand, Kind::WORD, 6, 12);

    sel = sel.after_edit(8, 2, 0);

    assert_eq!(sel.range(), 6..10);
}

#[test]
fn a_selection_collapses_when_the_text_it_covered_is_deleted() {
    let mut sel = Selection::typed(Axis::Expand, Kind::WORD, 6, 10);

    sel = sel.after_edit(4, 8, 0);

    assert_eq!(sel.range(), 4..4);
}

#[test]
fn the_whole_history_shifts_with_an_edit() {
    let mut st = state_at(0);
    st.select(Selection::typed(Axis::Expand, Kind::WORD, 6, 10));
    st.select(Selection::typed(Axis::Expand, Kind::WORD, 12, 16));

    st.after_edit(0, 0, 2);

    assert_eq!(st.current().range(), 14..18);
    st.pop();
    assert_eq!(st.current().range(), 8..12);
}

// --- expand ---------------------------------------------------------------
//
// The differential harness covers expansion after a motion. These cover the
// parts of `meow-expand`'s guard that are easier to state than to reach
// through keys.

#[test]
fn expanding_needs_both_a_selection_and_a_stashed_navigation_pair() {
    let doc = Document::from_text("alpha beta gamma\n");
    let mut st = state_at(0);

    // No selection, no nav.
    assert!(!expand(&doc, &mut st, 2, Axis::Select));

    // A selection, but nothing stashed: `meow-mark-word` clears the pair, so
    // this is the state `m` leaves behind.
    st.select(Selection::typed(Axis::Expand, Kind::WORD, 0, 5));
    st.clear_nav();
    assert!(!expand(&doc, &mut st, 2, Axis::Select));
    assert_eq!(st.current().range(), 0..5, "the selection is untouched");
}

#[test]
fn zero_expands_by_ten() {
    let doc = Document::from_text("a b c d e f g h i j k l\n");
    let mut st = state_at(0);
    next_word(&doc, &mut st, 1);
    let after_one = st.current().pos();

    expand(&doc, &mut st, 0, Axis::Select);

    // Ten more words, not none and not one.
    assert!(st.current().pos() > after_one);
    assert_eq!(st.current().pos(), 21, "ten words on from the first");
}

#[test]
fn expanding_takes_the_configured_axis_and_keeps_the_kind() {
    let doc = Document::from_text("alpha beta gamma\n");
    let mut st = state_at(0);
    next_word(&doc, &mut st, 1);

    expand(&doc, &mut st, 1, Axis::Select);
    assert_eq!(
        st.current().selection_type(),
        Some((Axis::Select, Kind::WORD)),
        "the default axis is select, so the result is transient"
    );

    expand(&doc, &mut st, 1, Axis::Expand);
    assert_eq!(
        st.current().selection_type(),
        Some((Axis::Expand, Kind::WORD)),
        "and it is configurable"
    );
}

#[test]
fn expanding_a_backward_selection_grows_it_backwards() {
    let doc = Document::from_text("alpha beta gamma delta\n");
    let mut st = state_at(22);
    back_word(&doc, &mut st, 1);
    let before = st.current();
    assert!(before.is_backward());

    expand(&doc, &mut st, 1, Axis::Select);

    assert!(
        st.current().pos() < before.pos(),
        "the point moved further back"
    );
    assert_eq!(st.current().mark(), before.mark(), "the far end stayed put");
}

// --- meow-join ------------------------------------------------------------

#[test]
fn join_selects_the_whitespace_back_to_the_previous_non_empty_line() {
    // `meow--join-backward`: from this line's indentation back over every
    // space and newline until something non-blank. Here that is the blank
    // line and the two spaces, but not the `a`.
    let doc = Document::from_text("a\n\n  b");
    let mut st = state_at(5); // on `b`

    join(&doc, &mut st, JoinSide::Backward);

    assert_eq!(st.current().range(), 1..5);
    assert_eq!(
        st.current().selection_type(),
        Some((Axis::Expand, Kind::JOIN))
    );
}

#[test]
fn a_negative_argument_makes_join_reach_forward_instead() {
    // `meow--join-forward`: from this line's end over the blanks that follow.
    let doc = Document::from_text("a\n\n  b");
    let mut st = state_at(0); // on `a`

    join(&doc, &mut st, JoinSide::Forward);

    assert_eq!(st.current().range(), 1..5);
}

#[test]
fn join_reaches_both_ways_from_the_point() {
    // `meow--join-both`, which Meow reaches with a universal argument or by
    // pressing `j` when the selection is already a join.
    let doc = Document::from_text("a  b");
    let mut st = state_at(2); // inside the run of spaces

    join(&doc, &mut st, JoinSide::Both);

    assert_eq!(st.current().range(), 1..3);
}

/// [`Delimiters`] answered from a list of spans written out by hand.
///
/// Lives here rather than in the crate: the session answers the port from its
/// syntax tree and never builds a span list, so a public plain-data version
/// would be API with no caller. Tests are the caller.
///
/// `pub(crate)` so `thing::tests` can ask the same question of
/// [`crate::thing::enclosing_pair`] with the same fake, rather than the two
/// bracket scans being tested against two different notions of a literal.
pub(crate) struct Literals<'a>(pub(crate) &'a [Range<usize>]);

impl Delimiters for Literals<'_> {
    fn counts(&self, byte: usize) -> bool {
        !self.0.iter().any(|span| span.contains(&byte))
    }
}

// --- the mark, which outlives the selection --------------------------------

#[test]
fn cancelling_a_selection_leaves_the_mark_where_it_was() {
    // Emacs's `meow--cancel-selection` deactivates the mark; it does not
    // unset it. With a history it first sends the mark back to where the run
    // of selections began, which is the point before the first one was made.
    let doc = Document::from_text("hello world\n");
    let mut st = state_at(0);
    next_word(&doc, &mut st, 1);
    assert_eq!(st.current().range(), 0..5);

    cancel(&mut st);

    assert_eq!(
        st.current().selection_type(),
        None,
        "the region is inactive"
    );
    assert_eq!(st.mark(), Some(0), "but the mark is still there");
    assert_eq!(st.current().pos(), 5, "and the point has not moved");
}

#[test]
fn a_fresh_state_has_no_mark_at_all() {
    // `(mark)` is nil in a buffer nothing has marked in, and `kill-ring-save`
    // says "The mark is not set now" rather than copying nothing.
    let st = state_at(3);
    assert_eq!(st.mark(), None);
}

#[test]
fn the_mark_moves_with_the_text_it_sits_in() {
    let doc = Document::from_text("hello world\n");
    let mut st = state_at(6);
    next_word(&doc, &mut st, 1);
    cancel(&mut st);
    assert_eq!(st.mark(), Some(6));

    st.after_edit(0, 0, 4); // four characters inserted before it

    assert_eq!(st.mark(), Some(10), "a marker moves with the text");
}

// --- brackets that are not delimiters -------------------------------------

#[test]
fn a_bracket_inside_a_string_is_not_a_delimiter() {
    //             0         1
    //             0123456789012345
    let doc = Document::from_text("f(a, \")\", b) end");
    let string = 5..8; // the `")"`, quotes included
    let mut st = state_at(2); // on `a`

    block(&doc, &mut st, false, Some(&Literals(&[string]) as _));

    // Counting the `)` at 6 would close the pair there and select `(a, "`,
    // five characters short and unbalanced.
    assert_eq!(st.current().range(), 1..12);
}

#[test]
fn a_bracket_inside_a_comment_is_not_a_delimiter() {
    let text = "f(a // )\n  , b) end";
    let doc = Document::from_text(text);
    let comment = text.find("//").unwrap()..text.find('\n').unwrap();
    let mut st = state_at(2);

    block(&doc, &mut st, false, Some(&Literals(&[comment]) as _));

    assert_eq!(st.current().range(), 1..text.find(") end").unwrap() + 1);
}

#[test]
fn the_backward_scan_skips_them_too() {
    // The opening bracket is the one buried in the string this time, so it is
    // the walk from the point *back* that has to ignore it. Both directions,
    // per the acceptance criteria — a scan that is right in one is a scan
    // that is wrong from the other end of the same selection.
    let text = "let s = \"(\"; f(x) end";
    let doc = Document::from_text(text);
    let string = 8..11; // `"("`
    let mut st = state_at(text.find('x').unwrap());

    block(&doc, &mut st, false, Some(&Literals(&[string]) as _));

    assert_eq!(st.current().range(), 14..17); // `(x)`, not reaching back to 9
}

#[test]
fn to_block_skips_them_as_well() {
    let text = "f(a, \")\", b) end";
    let doc = Document::from_text(text);
    let string = 5..8;
    let mut st = state_at(0);

    to_block(&doc, &mut st, false, Some(&Literals(&[string]) as _));

    assert_eq!(st.current().range(), 0..12);
}

#[test]
fn a_buffer_with_no_grammar_counts_every_bracket() {
    // The `None` case is not a degraded one — it is the behaviour these scans
    // have always had, kept exactly, because with no tree there is nothing
    // better to ask than the characters themselves.
    let doc = Document::from_text("f(a, \")\", b) end");
    let mut st = state_at(2);

    block(&doc, &mut st, false, None);

    assert_eq!(st.current().range(), 1..7); // stopped at the `)` in the string
}

// --- meow-to-block --------------------------------------------------------

#[test]
fn to_block_expands_from_the_point_to_the_end_of_the_next_block() {
    // `meow-to-block` scans forward for the close delimiter that brings it
    // back to the depth it started at, and selects from the point to there.
    let doc = Document::from_text("f(g(x), y) end");
    let mut st = state_at(0);

    to_block(&doc, &mut st, false, None);

    assert_eq!(st.current().range(), 0..10); // "f(g(x), y)"
    assert_eq!(
        st.current().selection_type(),
        Some((Axis::Expand, Kind::BLOCK))
    );
}

#[test]
fn to_block_refuses_when_the_next_delimiter_would_leave_the_starting_depth() {
    // On `x`, the next `)` closes the block the point is already inside, so
    // Emacs's depth check fails and `meow-to-block` does nothing at all.
    let doc = Document::from_text("f(g(x), y) end");
    let mut st = state_at(4);
    let before = st.current().range();

    to_block(&doc, &mut st, false, None);

    assert_eq!(st.current().range(), before);
}

// --- meow-goto-line -------------------------------------------------------

#[test]
fn goto_line_selects_the_whole_of_the_line_it_lands_on() {
    let doc = Document::from_text("one\ntwo\nthree\n");
    let mut st = state_at(0);

    goto_line(&doc, &mut st, 2);

    assert_eq!(st.current().range(), 4..7); // "two"
    assert_eq!(
        st.current().selection_type(),
        Some((Axis::Expand, Kind::LINE))
    );
}

#[test]
fn goto_line_widens_an_existing_line_selection_rather_than_replacing_it() {
    // "This command will expand line selection": with a line selection already
    // in hand Meow takes the union, so `L` twice covers everything between.
    let doc = Document::from_text("one\ntwo\nthree\n");
    let mut st = state_at(0);
    goto_line(&doc, &mut st, 1);

    goto_line(&doc, &mut st, 3);

    assert_eq!(st.current().range(), 0..13);
}

#[test]
fn to_block_reaches_backward_when_the_direction_says_to() {
    // `meow-to-block` computes `back` from the block selection it starts from
    // and from a negative argument, and then searches for an *opening*
    // delimiter instead. The selection it leaves runs backward.
    let doc = Document::from_text("(a) (b) end");
    let mut st = state_at(8); // on `e`

    to_block(&doc, &mut st, true, None);

    assert_eq!(st.current().range(), 4..8);
    assert!(st.current().is_backward());
}

#[test]
fn goto_line_upward_leaves_the_point_at_the_start_of_the_line() {
    // `(meow--select … t (> orig-p beg))`, and `meow--select` sends the point
    // to `beg` and the mark to `end` when that holds. So jumping *up* leaves
    // a backward selection, and jumping down a forward one.
    let doc = Document::from_text("one\ntwo\nthree\n");
    let mut st = state_at(10); // on line 3

    goto_line(&doc, &mut st, 1);

    assert_eq!(st.current().range(), 0..3);
    assert!(st.current().is_backward());
}

// --- numeric arguments reaching the motions -------------------------------

#[test]
fn a_negative_count_makes_next_word_select_backward() {
    // `meow-next-word` is `(interactive "p")` and hands n straight to
    // `meow-next-thing`, which selects to the beginning of the previous nth
    // thing when n is negative. `meow-back-word` is literally `(- n)` of it,
    // so `- w` and `b` have to land in the same place.
    let doc = Document::from_text("alpha beta gamma");
    let mut negated = state_at(11);
    let mut backward = state_at(11);

    next_word(&doc, &mut negated, -1);
    back_word(&doc, &mut backward, 1);

    assert_eq!(negated.current().range(), backward.current().range());
    assert_eq!(
        negated.current().selection_type(),
        backward.current().selection_type()
    );
}

#[test]
fn a_count_makes_next_word_cross_that_many_words() {
    // `(forward-thing thing n)` moves n things, not one.
    let doc = Document::from_text("alpha beta gamma delta");
    let mut once = state_at(0);
    let mut thrice = state_at(0);

    next_word(&doc, &mut once, 1);
    next_word(&doc, &mut thrice, 3);

    assert_eq!(once.current().range().end, 5); // end of "alpha"
    assert_eq!(thrice.current().range().end, 16); // end of "gamma"
}

#[test]
fn mark_word_takes_only_the_sign_of_its_argument() {
    // `meow-mark-word` passes `(< n 0)` — a direction, not a count. So `- m`
    // marks backward and `3 m` marks exactly what `m` does.
    let doc = Document::from_text("alpha beta gamma");
    let mut plain = state_at(8);
    let mut counted = state_at(8);

    mark_word(&doc, &mut plain, false);
    mark_word(&doc, &mut counted, false);

    assert_eq!(plain.current().range(), counted.current().range());
    assert!(!plain.current().is_backward());

    let mut backward = state_at(8);
    mark_word(&doc, &mut backward, true);
    assert!(backward.current().is_backward());
}

#[test]
fn a_saturated_negative_argument_does_not_panic() {
    // `Argument::digit` saturates on purpose — its comment says holding a
    // digit down would "overflow and panic a debug build" — so it can hand a
    // motion `i32::MIN`, and negating that has to saturate too.
    let doc = Document::from_text("alpha beta gamma");
    let mut st = state_at(11);

    back_word(&doc, &mut st, i32::MIN);

    // No panic, and the sign is still honoured: a hugely *negative* count to
    // `back_word` is a hugely positive one to `meow-next-thing`, so it runs
    // forward until the buffer stops it.
    assert_eq!(st.current().range().end, doc.byte_len());
}

#[test]
fn a_count_selects_that_many_lines() {
    // `(unless (= n 1) (forward-line (1- n)))` then `(line-end-position)`:
    // the magnitude is a distance in lines, not just a direction. Asserted
    // measured in a batch Emacs rather than taken from a golden, because a
    // count above one needs `C-u` and a `.cases` key sequence is a string of
    // single characters with nowhere to put a chord. `(meow-line 3)` from
    // point 0 of this buffer gives mark 0 and point 16.
    let doc = Document::from_text("alpha\nbeta\ngamma\ndelta\n");
    let mut st = state_at(0);

    select_line(&doc, &mut st, 3);

    assert_eq!(st.current().range(), 0..16, "three lines, eol excluded");
}

#[test]
fn a_count_past_the_end_stops_at_the_end() {
    // `forward-line` stops at the edge and reports the shortfall; nothing
    // reads the shortfall, so this clamps.
    //
    // 11 and not 10: the trailing newline makes a last line that is empty, and
    // `forward-line` lands on it, so the selection runs to the end of the
    // buffer rather than to the end of `beta`. This test first asserted 10 —
    // reasoned from "eol is not included" — and Emacs was asked instead.
    let doc = Document::from_text("alpha\nbeta\n");
    let mut st = state_at(0);

    select_line(&doc, &mut st, 99);

    assert_eq!(st.current().range(), 0..11);
}

#[test]
fn a_negative_count_selects_backwards_over_that_many_lines() {
    // `(forward-line (1+ n))` then `(line-beginning-position)`, with the mark
    // at the end of the line the point started on.
    let doc = Document::from_text("alpha\nbeta\ngamma\ndelta\n");
    let mut st = state_at(13);

    select_line(&doc, &mut st, -2);

    let selection = st.current();
    assert_eq!(selection.mark(), 16, "the end of `gamma`");
    assert_eq!(selection.pos(), 6, "two lines back, at the start of `beta`");
    assert!(selection.is_backward());
}

#[test]
fn redoing_past_the_save_point_makes_the_buffer_modified_again() {
    // The half a flag could not express at all. Undo back to the save and the
    // buffer is unmodified; redo forward and it is modified again, because the
    // question is where in the timeline you are rather than whether an edit
    // ever happened.
    let mut doc = Document::from_text("alpha\n");
    let mut st = state_at(0);
    let mut history = crate::undo::History::new();
    history.mark_saved();
    history.insert(&mut doc, &mut st, 0, "Z").unwrap();
    history.boundary();
    assert!(history.is_modified(), "typed");

    history.undo(&mut doc, &mut st);
    assert!(!history.is_modified(), "back at the save");

    history.redo(&mut doc, &mut st);
    assert!(history.is_modified(), "and forward past it again");
}

#[test]
fn a_save_made_after_undoing_moves_the_save_point() {
    let mut doc = Document::from_text("alpha\n");
    let mut st = state_at(0);
    let mut history = crate::undo::History::new();
    history.insert(&mut doc, &mut st, 0, "Z").unwrap();
    history.boundary();
    history.insert(&mut doc, &mut st, 0, "Y").unwrap();
    history.boundary();
    history.undo(&mut doc, &mut st);

    history.mark_saved();

    assert!(!history.is_modified(), "saved here");
    history.undo(&mut doc, &mut st);
    assert!(history.is_modified(), "and undoing past it leaves it");
}

#[test]
fn every_occurrence_of_a_needle_is_found_once() {
    // What `search` finds one at a time, all at once. Non-overlapping and
    // earliest-first: `aa` in `aaa` is one match at 0, because two rows on one
    // line that a reader cannot tell apart is worse than one.
    let doc = Document::from_text("widget one\ntwo widget\nwidgetwidget\n");

    assert_eq!(occurrences(&doc, "widget"), vec![0, 15, 22, 28]);
    assert_eq!(occurrences(&doc, "aaa"), Vec::<usize>::new());
    assert_eq!(
        occurrences(&doc, ""),
        Vec::<usize>::new(),
        "an empty needle matches nowhere rather than everywhere"
    );

    let overlapping = Document::from_text("aaa\n");
    assert_eq!(occurrences(&overlapping, "aa"), vec![0]);
}
