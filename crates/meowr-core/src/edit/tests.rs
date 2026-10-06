use super::*;
use crate::undo::History;
use crate::{Axis, Kind, Selection, next_word, select_line};

fn doc_at(text: &str, pos: usize) -> (Document, SelectionState, History) {
    (
        Document::from_text(text),
        SelectionState::new(Selection::at(pos)),
        History::new(),
    )
}

// --- the kill ring --------------------------------------------------------

#[test]
fn the_ring_is_bounded_and_the_newest_kill_is_the_head() {
    let mut ring = KillRing::new();
    for n in 0..KillRing::MAX + 10 {
        ring.push(n.to_string());
    }
    assert_eq!(ring.len(), KillRing::MAX, "kill-ring-max");
    assert_eq!(ring.head(), Some("69"), "the most recent kill");
}

#[test]
fn rotating_walks_back_through_the_ring_and_wraps() {
    let mut ring = KillRing::new();
    ring.push("first".into());
    ring.push("second".into());
    assert_eq!(ring.head(), Some("second"));
    assert_eq!(ring.rotate(), Some("first"));
    assert_eq!(ring.rotate(), Some("second"), "and round again");
}

#[test]
fn a_fresh_kill_rewinds_the_pointer() {
    // `kill-new` resets `kill-ring-yank-pointer`, so a yank after a kill
    // always gives the thing just killed rather than wherever yank-pop had
    // walked to.
    let mut ring = KillRing::new();
    ring.push("a".into());
    ring.push("b".into());
    ring.rotate();
    assert_eq!(ring.head(), Some("a"));

    ring.push("c".into());
    assert_eq!(ring.head(), Some("c"));
}

// --- yank-pop -------------------------------------------------------------
//
// Not reachable by the differential harness: since Emacs 28 `yank-pop` falls
// through to an interactive prompt unless `last-command` is `yank`, and
// replaying one key per `execute-kbd-macro` loses `last-command`. Written from
// the behaviour instead.

#[test]
fn yank_pop_swaps_the_last_yank_for_the_next_kill() {
    let (mut doc, mut state, mut h) = doc_at("", 0);
    let mut ring = KillRing::new();
    ring.push("older".into());
    ring.push("newer".into());

    yank(&mut doc, &mut state, &ring, &mut h);
    assert_eq!(doc.text().to_string(), "newer");

    yank_pop(&mut doc, &mut state, &mut ring, &mut h);
    assert_eq!(doc.text().to_string(), "older");
    assert_eq!(state.current().pos(), 5, "the point follows the new text");
}

#[test]
fn yank_pop_refuses_when_the_text_before_the_point_is_not_the_last_yank() {
    // The extent to replace is recomputed from the ring rather than
    // remembered, so it has to check its own premise: without this, typing
    // after a yank and then pressing `P` would eat unrelated text.
    let (mut doc, mut state, mut h) = doc_at("something else", 14);
    let mut ring = KillRing::new();
    ring.push("older".into());
    ring.push("newer".into());

    assert!(!yank_pop(&mut doc, &mut state, &mut ring, &mut h));
    assert_eq!(doc.text().to_string(), "something else", "untouched");
}

#[test]
fn yank_pop_on_an_empty_ring_does_nothing() {
    let (mut doc, mut state, mut h) = doc_at("text", 0);
    let mut ring = KillRing::new();
    assert!(!yank_pop(&mut doc, &mut state, &mut ring, &mut h));
}

// --- kill and change are not the same operation ---------------------------

#[test]
fn killing_a_line_takes_its_newline_but_changing_it_does_not() {
    // `meow-kill` runs `meow--prepare-region-for-kill` first; `meow-change`
    // calls `meow--delete-region` directly. Three lines apart in the elisp.
    let (mut doc, mut state, mut h) = doc_at("alpha\nbeta\n", 0);
    let mut ring = KillRing::new();
    select_line(&doc, &mut state, 1);
    kill(&mut doc, &mut state, &mut ring, &mut h);
    assert_eq!(doc.text().to_string(), "beta\n");
    assert_eq!(ring.head(), Some("alpha\n"), "the newline went with it");

    let (mut doc, mut state, mut h) = doc_at("alpha\nbeta\n", 0);
    select_line(&doc, &mut state, 1);
    change(&mut doc, &mut state, &mut h);
    assert_eq!(doc.text().to_string(), "\nbeta\n", "the newline stayed");
}

#[test]
fn change_leaves_the_ring_as_it_found_it() {
    // `meow-change` never touches the kill ring, so what was there before is
    // still what a yank would give afterwards. Seeded rather than empty: an
    // empty ring would pass this whether or not `change` could reach it.
    let (mut doc, mut state, mut h) = doc_at("alpha beta\n", 0);
    let mut ring = KillRing::new();
    ring.push("kept".into());
    next_word(&doc, &mut state, 1);

    change(&mut doc, &mut state, &mut h);

    assert_eq!(ring.head(), Some("kept"));
    assert_eq!(ring.len(), 1, "and nothing was added");
}

#[test]
fn a_kill_keeps_the_selection_history() {
    // `meow--delete-region` is a bare `delete-region`: it deactivates the
    // mark and leaves `meow--selection-history` alone, so `z` after a kill
    // still walks back.
    let (mut doc, mut state, mut h) = doc_at("alpha beta\n", 0);
    let mut ring = KillRing::new();
    next_word(&doc, &mut state, 1);
    let depth = state.history_depth();
    assert!(depth > 0);

    kill(&mut doc, &mut state, &mut ring, &mut h);

    assert_eq!(state.history_depth(), depth);
}

// --- the two deletes are different ----------------------------------------

#[test]
fn backward_delete_takes_the_whole_selection_but_delete_takes_one_character() {
    // `delete-backward-char` honours `delete-active-region`; `delete-char`
    // does not. The asymmetry is Emacs's.
    let (mut doc, mut state, mut h) = doc_at("alpha beta\n", 0);
    next_word(&doc, &mut state, 1);
    backward_delete(&mut doc, &mut state, &mut h);
    assert_eq!(doc.text().to_string(), " beta\n");

    let (mut doc, mut state, mut h) = doc_at("alpha beta\n", 0);
    next_word(&doc, &mut state, 1);
    delete_char(&mut doc, &mut state, &mut h);
    assert_eq!(doc.text().to_string(), "alphabeta\n", "just the space");
}

// --- replace --------------------------------------------------------------

#[test]
fn replace_trims_trailing_newlines_from_the_ring() {
    // Otherwise replacing a word with a copied line splits the target line.
    let (mut doc, mut state, mut h) = doc_at("alpha beta\n", 0);
    let mut ring = KillRing::new();
    ring.push("gamma\n".into());
    next_word(&doc, &mut state, 1);

    replace(&mut doc, &mut state, &ring, &mut h);

    assert_eq!(doc.text().to_string(), "gamma beta\n");
}

#[test]
fn replace_with_an_empty_ring_fails_rather_than_deleting() {
    let (mut doc, mut state, mut h) = doc_at("alpha beta\n", 0);
    let ring = KillRing::new();
    next_word(&doc, &mut state, 1);

    assert!(!replace(&mut doc, &mut state, &ring, &mut h));
    assert_eq!(doc.text().to_string(), "alpha beta\n");
}

// --- opening lines --------------------------------------------------------

#[test]
fn open_above_leaves_the_point_on_the_new_line() {
    let (mut doc, mut state, mut h) = doc_at("alpha\nbeta\n", 8);
    open_above(&mut doc, &mut state, &mut h);
    assert_eq!(doc.text().to_string(), "alpha\n\nbeta\n");
    assert_eq!(state.current().pos(), 6);
}

#[test]
fn open_below_leaves_the_point_after_the_new_newline() {
    let (mut doc, mut state, mut h) = doc_at("alpha\nbeta\n", 2);
    open_below(&mut doc, &mut state, &mut h);
    assert_eq!(doc.text().to_string(), "alpha\n\nbeta\n");
    assert_eq!(state.current().pos(), 6);
}

#[test]
fn a_typed_selection_survives_being_killed_out_from_under_the_history() {
    // `after_edit` has to move the history, not just the current selection,
    // or `z` after a kill restores a span that no longer exists.
    let (mut doc, mut state, mut h) = doc_at("alpha beta gamma\n", 0);
    let mut ring = KillRing::new();
    next_word(&doc, &mut state, 1);
    state.select(Selection::typed(Axis::Expand, Kind::WORD, 11, 16));

    kill(&mut doc, &mut state, &mut ring, &mut h);

    assert!(state.pop());
    let restored = state.current().range();
    assert!(
        restored.end <= doc.byte_len(),
        "restored {restored:?} is inside a {}-byte buffer",
        doc.byte_len()
    );
}

// --- the Trail (#134) ----------------------------------------------------

#[test]
fn the_trail_gives_back_the_place_most_recently_left() {
    let mut trail = Trail::new();
    assert_eq!(trail.pop(), None, "nowhere to go back to yet");
    trail.push(Place::new("a.rs", 1, 0));
    trail.push(Place::new("b.rs", 2, 0));
    assert_eq!(trail.pop(), Some(Place::new("b.rs", 2, 0)));
    assert_eq!(trail.pop(), Some(Place::new("a.rs", 1, 0)));
    assert!(trail.is_empty());
}

#[test]
fn the_trail_keeps_sixteen_and_drops_the_oldest() {
    // Emacs's `global-mark-ring-max`, rather than a number invented here.
    let mut trail = Trail::new();
    for line in 0..20 {
        trail.push(Place::new("a.rs", line, 0));
    }
    assert_eq!(trail.len(), Trail::MAX);
    assert_eq!(trail.pop(), Some(Place::new("a.rs", 19, 0)), "newest first");
    // The four oldest are gone, so the last one left is the fifth pushed.
    while trail.len() > 1 {
        trail.pop();
    }
    assert_eq!(trail.pop(), Some(Place::new("a.rs", 4, 0)));
}

// --- recent files (#152) ----------------------------------------------------

#[test]
fn a_file_touched_again_moves_to_the_top_rather_than_appearing_twice() {
    // `recentf-push`, and Doom's frecency: switching to a file or writing it
    // bumps it, which only means something if it leaves where it was.
    let mut recent = RecentFiles::default();
    recent.touch("a.rs");
    recent.touch("b.rs");
    recent.touch("a.rs");
    assert_eq!(recent.paths().collect::<Vec<_>>(), ["a.rs", "b.rs"]);
}

#[test]
fn recent_files_are_capped_at_doom_s_two_hundred() {
    // `recentf-max-saved-items 200` in Doom's core; Emacs's own default is 20.
    let mut recent = RecentFiles::default();
    for n in 0..=RecentFiles::MAX {
        recent.touch(&format!("{n}.rs"));
    }
    assert_eq!(RecentFiles::MAX, 200);
    assert_eq!(recent.paths().count(), 200);
    assert_eq!(recent.paths().next(), Some("200.rs"), "the newest kept");
    assert!(recent.paths().all(|p| p != "0.rs"), "the oldest dropped");
}

#[test]
fn a_list_read_back_keeps_its_order_and_loses_its_repeats() {
    // A saved list is most recent first, and one written by hand -- or by two
    // sessions racing to quit -- may name a file twice. The first is the newer.
    let recent = RecentFiles::from_paths(["b.rs", "a.rs", "b.rs"].map(String::from));
    assert_eq!(recent.paths().collect::<Vec<_>>(), ["b.rs", "a.rs"]);
}

#[test]
fn a_file_that_was_deleted_leaves_the_list() {
    // `recentf-remove-if-non-kept`, which Doom's `doom-files--update-refs`
    // calls for any path it can no longer read. An entry that opens nothing is
    // a row in the picker that can only disappoint.
    let mut recent = RecentFiles::from_paths(["a.rs", "b.rs", "c.rs"].map(String::from));
    recent.gone("b.rs");
    assert_eq!(recent.paths().collect::<Vec<_>>(), ["a.rs", "c.rs"]);

    recent.gone("nowhere.rs");
    assert_eq!(recent.paths().collect::<Vec<_>>(), ["a.rs", "c.rs"], "one it never held");
}
