use super::*;

fn setup(text: &str) -> (Document, SelectionState, History) {
    (
        Document::from_text(text),
        SelectionState::new(Selection::at(0)),
        History::new(),
    )
}

#[test]
fn undo_reverses_the_previous_edit_and_repeats_walk_back() {
    let (mut doc, mut state, mut h) = setup("");
    h.insert(&mut doc, &mut state, 0, "one").unwrap();
    h.boundary();
    h.insert(&mut doc, &mut state, 3, "two").unwrap();
    assert_eq!(doc.text().to_string(), "onetwo");

    assert!(h.undo(&mut doc, &mut state));
    assert_eq!(doc.text().to_string(), "one");
    assert!(h.undo(&mut doc, &mut state));
    assert_eq!(doc.text().to_string(), "");
    assert!(
        !h.undo(&mut doc, &mut state),
        "and then there is nothing left"
    );
}

#[test]
fn undo_restores_the_selection_not_only_the_text() {
    let (mut doc, mut state, mut h) = setup("alpha beta\n");
    state.select(Selection::typed(
        crate::Axis::Expand,
        crate::Kind::WORD,
        0,
        5,
    ));
    let before = state.current();

    h.delete(&mut doc, &mut state, 0..5).unwrap();
    assert_ne!(state.current(), before);

    h.undo(&mut doc, &mut state);
    assert_eq!(doc.text().to_string(), "alpha beta\n");
    assert_eq!(state.current(), before, "the point comes back too");
}

#[test]
fn redo_replays_what_undo_took_away() {
    let (mut doc, mut state, mut h) = setup("");
    h.insert(&mut doc, &mut state, 0, "hello").unwrap();
    h.undo(&mut doc, &mut state);
    assert_eq!(doc.text().to_string(), "");

    assert!(h.redo(&mut doc, &mut state));
    assert_eq!(doc.text().to_string(), "hello");
    assert!(!h.redo(&mut doc, &mut state));
}

#[test]
fn a_fresh_edit_discards_the_redo_stack() {
    // The undone future no longer follows from this present.
    let (mut doc, mut state, mut h) = setup("");
    h.insert(&mut doc, &mut state, 0, "hello").unwrap();
    h.undo(&mut doc, &mut state);
    assert!(h.can_redo());

    h.insert(&mut doc, &mut state, 0, "other").unwrap();

    assert!(!h.can_redo());
}

#[test]
fn a_burst_of_typing_is_one_undo() {
    let (mut doc, mut state, mut h) = setup("");
    for (i, c) in "hello".chars().enumerate() {
        h.type_text(&mut doc, &mut state, i, &c.to_string())
            .unwrap();
    }
    assert_eq!(doc.text().to_string(), "hello");

    h.undo(&mut doc, &mut state);

    assert_eq!(doc.text().to_string(), "", "not \"hell\"");
}

#[test]
fn typing_that_is_not_contiguous_starts_a_new_transaction() {
    // Typing, moving the point, and typing again is two undos, because the
    // second run does not continue the first.
    let (mut doc, mut state, mut h) = setup("..........");
    h.type_text(&mut doc, &mut state, 0, "a").unwrap();
    h.type_text(&mut doc, &mut state, 8, "b").unwrap();

    h.undo(&mut doc, &mut state);
    assert_eq!(doc.text().to_string(), "a..........");
    h.undo(&mut doc, &mut state);
    assert_eq!(doc.text().to_string(), "..........");
}

#[test]
fn typing_stops_amalgamating_at_the_limit() {
    let (mut doc, mut state, mut h) = setup("");
    for i in 0..AMALGAMATE_LIMIT + 5 {
        h.type_text(&mut doc, &mut state, i, "x").unwrap();
    }
    h.undo(&mut doc, &mut state);
    assert_eq!(
        doc.text().to_string().len(),
        AMALGAMATE_LIMIT,
        "one undo takes the tail, not the lot"
    );
}

#[test]
fn a_boundary_between_edits_makes_them_separate_undos() {
    let (mut doc, mut state, mut h) = setup("");
    h.type_text(&mut doc, &mut state, 0, "a").unwrap();
    h.boundary();
    h.type_text(&mut doc, &mut state, 1, "b").unwrap();

    h.undo(&mut doc, &mut state);
    assert_eq!(doc.text().to_string(), "a");
}

#[test]
fn undo_in_selection_reverts_only_the_changes_inside_it() {
    // One transaction spanning two places — a typing run interrupted by
    // nothing, so both edits belong to it — and only the one inside the span
    // comes back.
    let (mut doc, mut state, mut h) = setup("alpha beta gamma\n");
    h.delete(&mut doc, &mut state, 11..16).unwrap(); // "gamma"
    h.delete(&mut doc, &mut state, 0..5).unwrap(); // "alpha", same transaction
    assert_eq!(doc.text().to_string(), " beta \n");

    // The span where "alpha" was.
    assert!(h.undo_in(&mut doc, &mut state, 0..1));

    assert_eq!(doc.text().to_string(), "alpha beta \n", "gamma stays gone");
    assert!(
        h.can_undo(),
        "and the rest of the transaction is still there"
    );
}

#[test]
fn undo_in_selection_with_nothing_in_range_does_nothing() {
    let (mut doc, mut state, mut h) = setup("alpha beta\n");
    h.delete(&mut doc, &mut state, 0..5).unwrap();
    h.boundary();

    assert!(!h.undo_in(&mut doc, &mut state, 20..25));
    assert_eq!(doc.text().to_string(), " beta\n");
}

#[test]
fn deleting_records_what_was_removed_so_undo_can_put_it_back_exactly() {
    let (mut doc, mut state, mut h) = setup("keep this exactly\n");
    h.delete(&mut doc, &mut state, 5..9).unwrap();
    // 5..9 is "this" and not the space after it, so two spaces are left.
    assert_eq!(doc.text().to_string(), "keep  exactly\n");

    h.undo(&mut doc, &mut state);

    assert_eq!(doc.text().to_string(), "keep this exactly\n");
}

// --- a foreign edit (#161, ADR-0013) ------------------------------------------

fn replacement(range: Range<usize>, text: &str) -> meowr_domain::Replacement {
    meowr_domain::Replacement {
        range,
        text: text.to_string(),
    }
}

#[test]
fn replacements_addressed_to_the_original_text_land_where_each_was_meant() {
    // How both servers #157 measured send them: every edit in a set is a
    // position in the text as it was before any of them. Given low-first here,
    // which is the order that goes wrong if they are applied as given.
    let (mut doc, mut state, mut h) = setup("let a = 1; let b = 2;\n");
    h.replace_all(
        &mut doc,
        &mut state,
        &[replacement(4..5, "alpha"), replacement(15..16, "beta")],
    )
    .unwrap();
    assert_eq!(doc.text().to_string(), "let alpha = 1; let beta = 2;\n");
}

#[test]
fn a_foreign_edit_is_one_undo_that_puts_the_selection_back() {
    let (mut doc, mut state, mut h) = setup("let a = 1; let b = 2;\n");
    state.select_without_history(Selection::at(18));
    let before = state.current();
    h.replace_all(
        &mut doc,
        &mut state,
        &[replacement(4..5, "alpha"), replacement(15..16, "beta")],
    )
    .unwrap();

    assert!(h.undo(&mut doc, &mut state));
    assert_eq!(doc.text().to_string(), "let a = 1; let b = 2;\n");
    assert_eq!(state.current(), before);
    assert!(!h.can_undo(), "one step, not one per replacement");
}

#[test]
fn typing_before_a_foreign_edit_stays_its_own_undo() {
    // Closed off on both sides: a format landing mid-word must not take the
    // word with it when undone.
    let (mut doc, mut state, mut h) = setup("x\n");
    h.type_text(&mut doc, &mut state, 0, "ab").unwrap();
    h.replace_all(&mut doc, &mut state, &[replacement(2..3, "y")])
        .unwrap();
    assert_eq!(doc.text().to_string(), "aby\n");

    h.undo(&mut doc, &mut state);
    assert_eq!(doc.text().to_string(), "abx\n", "only the foreign edit undone");
}

#[test]
fn a_point_inside_a_replaced_span_keeps_its_distance_from_the_start() {
    // Not typing's rule, which collapses it to the start: a renamed identifier
    // with the point in its middle keeps the point in its middle, clamped to
    // the new text when that is shorter.
    let (mut doc, mut state, mut h) = setup("hello world!");
    state.select_without_history(Selection::at(8));
    h.replace_all(&mut doc, &mut state, &[replacement(6..11, "Earth")])
        .unwrap();
    assert_eq!(state.current().pos(), 8, "two into the span, still");

    let (mut doc, mut state, mut h) = setup("hello world!");
    state.select_without_history(Selection::at(10));
    h.replace_all(&mut doc, &mut state, &[replacement(6..11, "W")])
        .unwrap();
    assert_eq!(state.current().pos(), 7, "clamped to the end of the new text");

    let (mut doc, mut state, mut h) = setup("hello world!");
    state.select_without_history(Selection::at(11));
    h.replace_all(&mut doc, &mut state, &[replacement(0..5, "hi")])
        .unwrap();
    assert_eq!(state.current().pos(), 8, "after a span, moved by the difference");
}

#[test]
fn an_overlapping_or_out_of_range_set_changes_nothing() {
    let (mut doc, mut state, mut h) = setup("abcdef");
    assert!(
        h.replace_all(
            &mut doc,
            &mut state,
            &[replacement(1..4, "x"), replacement(3..5, "y")],
        )
        .is_err()
    );
    assert!(
        h.replace_all(&mut doc, &mut state, &[replacement(2..3, "x"), replacement(4..99, "y")])
            .is_err()
    );
    assert_eq!(doc.text().to_string(), "abcdef", "not even the valid half");
    assert!(!h.can_undo());
}

#[test]
fn an_insert_and_a_replacement_at_one_position_apply_in_either_order_given() {
    // The protocol: an insert at `p` and a replacement starting at `p` do not
    // overlap, and the insert lands before the replaced text. Sorting by start
    // alone put the insert first in one order and refused the pair as
    // overlapping in the other.
    for set in [
        [replacement(4..5, "alpha"), replacement(4..4, "mut ")],
        [replacement(4..4, "mut "), replacement(4..5, "alpha")],
    ] {
        let (mut doc, mut state, mut h) = setup("let a = 1;");
        h.replace_all(&mut doc, &mut state, &set).unwrap();
        assert_eq!(doc.text().to_string(), "let mut alpha = 1;");
    }
}

#[test]
fn two_inserts_at_one_position_keep_the_order_they_were_given() {
    // "If multiple inserts have the same position, the order in the array
    // defines which edit to apply first" -- given high-first as well, to show
    // the order is the array's and not the positions'.
    let (mut doc, mut state, mut h) = setup("ab");
    h.replace_all(
        &mut doc,
        &mut state,
        &[replacement(2..2, "!"), replacement(1..1, "x"), replacement(1..1, "y")],
    )
    .unwrap();
    assert_eq!(doc.text().to_string(), "axyb!");
}

#[test]
fn a_point_kept_inside_a_replaced_span_is_never_inside_a_character() {
    // The distance is counted in bytes, and a replacement of wider characters
    // would otherwise leave the point in the middle of one.
    let (mut doc, mut state, mut h) = setup("xabcx");
    state.select_without_history(Selection::at(2));
    h.replace_all(&mut doc, &mut state, &[replacement(1..4, "éé")])
        .unwrap();
    let pos = state.current().pos();
    assert!(doc.text().to_string().is_char_boundary(pos), "{pos}");
    assert_eq!(pos, 1, "back to the edge of the character it fell in");
}

#[test]
fn a_foreign_edit_redoes_as_it_was_applied() {
    let (mut doc, mut state, mut h) = setup("let a = 1; let b = 2;\n");
    h.replace_all(
        &mut doc,
        &mut state,
        &[replacement(15..16, "beta"), replacement(4..5, "alpha")],
    )
    .unwrap();
    let after = state.current();
    h.undo(&mut doc, &mut state);
    assert!(h.redo(&mut doc, &mut state));
    assert_eq!(doc.text().to_string(), "let alpha = 1; let beta = 2;\n");
    assert_eq!(state.current(), after);
}

#[test]
fn a_range_inside_a_character_is_refused_as_that() {
    let (mut doc, mut state, mut h) = setup("é");
    assert_eq!(
        h.replace_all(&mut doc, &mut state, &[replacement(1..2, "x")]),
        Err(ReplaceError::Edit(EditError::NotACharBoundary))
    );
}

/// Replay changes, as whoever reads `take_changes` does -- tree-sitter's edits,
/// the Grab, a server's view of the text -- onto a copy of the text before.
///
/// Strictly: a change that does not fit the text, or claims to remove text that
/// is not there, fails here as tree-sitter's arithmetic does, rather than being
/// skipped as `apply` skips it.
fn replayed(before: &str, changes: &[Change]) -> String {
    let mut doc = Document::from_text(before);
    for change in changes {
        let end = change.at + change.removed.len();
        assert!(end <= doc.byte_len(), "{change:?} does not fit {:?}", doc.text().to_string());
        assert_eq!(slice(&doc, change.at..end), change.removed, "{change:?} removes other text");
        doc.delete(change.at..end).unwrap();
        doc.insert(change.at, &change.inserted).unwrap();
    }
    doc.text().to_string()
}

#[test]
fn undoing_and_redoing_are_changes_like_any_other() {
    // #171: they changed the document and recorded nothing, so the revision
    // stood still, no server was told, and a stale foreign edit still matched.
    let (mut doc, mut state, mut h) = setup("one\n");
    h.insert(&mut doc, &mut state, 0, "zero ").unwrap();
    h.boundary();
    h.delete(&mut doc, &mut state, 5..8).unwrap();
    let _ = h.take_changes();
    let edited = doc.text().to_string();

    assert!(h.undo(&mut doc, &mut state));
    let undone = h.take_changes();
    assert!(!undone.is_empty(), "an undo is a change");
    assert_eq!(replayed(&edited, &undone), doc.text().to_string());

    let before_redo = doc.text().to_string();
    assert!(h.redo(&mut doc, &mut state));
    assert_eq!(replayed(&before_redo, &h.take_changes()), edited, "and so is a redo");
}

#[test]
fn undoing_in_a_selection_is_a_change_too() {
    let (mut doc, mut state, mut h) = setup("aaaa bbbb\n");
    h.insert(&mut doc, &mut state, 0, "X").unwrap();
    h.insert(&mut doc, &mut state, 6, "Y").unwrap();
    let _ = h.take_changes();
    let edited = doc.text().to_string();

    assert!(h.undo_in(&mut doc, &mut state, 0..2));
    assert_eq!(replayed(&edited, &h.take_changes()), doc.text().to_string());
}

#[test]
fn what_an_undo_records_is_what_it_did_even_when_a_change_no_longer_fits() {
    // `undo_in`'s partial revert leaves the rest of the transaction at offsets
    // the text has since moved from -- its documented limit -- so a later undo
    // cannot apply every change. Recording the ones that failed described text
    // that does not exist, and tree-sitter's edit arithmetic panics on that.
    let (mut doc, mut state, mut h) = setup("ab");
    h.insert(&mut doc, &mut state, 0, "XXXX").unwrap();
    h.insert(&mut doc, &mut state, 6, "Q").unwrap();
    let _ = h.take_changes();
    assert!(h.undo_in(&mut doc, &mut state, 0..4));
    let partly = doc.text().to_string();
    let _ = h.take_changes();

    assert!(h.undo(&mut doc, &mut state));
    assert_eq!(replayed(&partly, &h.take_changes()), doc.text().to_string());
}
