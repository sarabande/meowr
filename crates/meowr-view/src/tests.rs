use super::*;
use meowr_domain::{Diagnostic, Document, Folds, Severity};

fn doc() -> Document {
    Document::from_text("one\ntwo\nthree\nfour\nfive\n")
}

#[test]
fn a_viewport_yields_the_visible_lines_without_their_newlines() {
    let rows = rows(&doc(), Viewport { top: 0, height: 3 });

    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0].line, 0);
    assert_eq!(rows[0].text, "one");
    assert_eq!(rows[2].text, "three");
}

#[test]
fn scrolling_offsets_which_lines_appear() {
    let rows = rows(&doc(), Viewport { top: 2, height: 2 });

    assert_eq!(rows[0].line, 2);
    assert_eq!(rows[0].text, "three");
    assert_eq!(rows[1].text, "four");
}

#[test]
fn a_viewport_taller_than_what_remains_yields_only_what_remains() {
    let rows = rows(&doc(), Viewport { top: 3, height: 10 });

    assert_eq!(rows.len(), 2);
    assert_eq!(rows[1].text, "five");
}

#[test]
fn a_viewport_entirely_past_the_end_yields_nothing() {
    assert!(rows(&doc(), Viewport { top: 99, height: 5 }).is_empty());
}

#[test]
fn a_zero_height_viewport_yields_nothing() {
    assert!(rows(&doc(), Viewport { top: 0, height: 0 }).is_empty());
}

#[test]
fn multi_byte_text_survives_intact() {
    let doc = Document::from_text("héllo wörld\n日本語のテキスト\n");
    let rows = rows(&doc, Viewport { top: 0, height: 2 });

    assert_eq!(rows[0].text, "héllo wörld");
    assert_eq!(rows[1].text, "日本語のテキスト");
}

use meowr_core::{Axis, Kind, Selection};

fn sel(mark: usize, pos: usize) -> Selection {
    Selection::typed(Axis::Expand, Kind::CHAR, mark, pos)
}

#[test]
fn a_row_reports_the_selected_span_within_its_own_text() {
    let doc = Document::from_text("alpha\nbeta\n");
    // Bytes 6..9 is "bet" on line 1, which starts at byte 6.
    let out = rows_with(&doc, Viewport { top: 0, height: 2 }, Some(sel(6, 9)), false, &Folds::default());

    assert_eq!(out[0].selected, None);
    assert_eq!(out[1].selected, Some(0..3));
}

#[test]
fn a_selection_spanning_lines_is_clipped_to_each_row() {
    let doc = Document::from_text("alpha\nbeta\n");
    let out = rows_with(&doc, Viewport { top: 0, height: 2 }, Some(sel(3, 8)), false, &Folds::default());

    assert_eq!(out[0].selected, Some(3..5)); // "ha", newline excluded
    assert_eq!(out[1].selected, Some(0..2)); // "be"
}

#[test]
fn the_cursor_sits_on_the_row_holding_the_point() {
    let doc = Document::from_text("alpha\nbeta\n");
    let out = rows_with(&doc, Viewport { top: 0, height: 2 }, Some(sel(6, 9)), false, &Folds::default());

    assert_eq!(out[0].cursor, None);
    assert_eq!(out[1].cursor, Some(3));
}

#[test]
fn a_backward_selection_puts_the_cursor_at_its_start() {
    // Direction is what distinguishes these two; the span is identical.
    let doc = Document::from_text("alpha\n");
    let forward = rows_with(&doc, Viewport { top: 0, height: 1 }, Some(sel(1, 4)), false, &Folds::default());
    let backward = rows_with(&doc, Viewport { top: 0, height: 1 }, Some(sel(4, 1)), false, &Folds::default());

    assert_eq!(forward[0].selected, backward[0].selected);
    assert_eq!(forward[0].cursor, Some(4));
    assert_eq!(backward[0].cursor, Some(1));
}

#[test]
fn a_row_with_no_selection_reports_none() {
    let doc = Document::from_text("alpha\n");
    let out = rows_with(&doc, Viewport { top: 0, height: 1 }, None, false, &Folds::default());

    assert_eq!(out[0].selected, None);
    assert_eq!(out[0].cursor, None);
}

#[test]
fn a_bare_point_still_reports_a_cursor() {
    // `Selection::spanning` is `meow--selection-type` answering nil — an
    // ordinary motion with nothing selected, which is most of editing. The
    // point still has to land somewhere: a `None` here is what let the
    // render layer skip drawing it entirely.
    let doc = Document::from_text("alpha\nbeta\n");
    let out = rows_with(
        &doc,
        Viewport { top: 0, height: 2 },
        Some(Selection::spanning(7, 7)),
        false,
        &Folds::default(),
    );

    assert_eq!(out[1].selected, None, "nothing is selected");
    assert_eq!(out[1].cursor, Some(1), "but the point is still on the row");
}

// --- expand hints ---------------------------------------------------------

#[test]
fn hints_are_placed_by_row_and_column_within_the_viewport() {
    let doc = Document::from_text("alpha beta\ngamma delta\n");
    let viewport = Viewport { top: 0, height: 10 };

    // Offsets 5 and 16: the space on line 0, and the space on line 1, which
    // starts at 11.
    let placed = hints(&doc, viewport, &[5, 16]);

    assert_eq!(placed.len(), 2);
    assert_eq!(placed[0].row, 0);
    assert_eq!(placed[0].column, 5);
    assert_eq!(placed[0].label, 1);
    assert_eq!(placed[1].row, 1);
    assert_eq!(placed[1].column, 5);
    assert_eq!(
        placed[1].label, 2,
        "labels count the positions, not the rows"
    );
}

#[test]
fn hints_outside_the_viewport_are_dropped_without_shifting_the_rest() {
    let doc = Document::from_text("one\ntwo\nthree\nfour\n");
    // Showing lines 1 and 2 only.
    let viewport = Viewport { top: 1, height: 2 };

    // Offsets on lines 0, 1 and 3.
    let placed = hints(&doc, viewport, &[1, 5, 15]);

    assert_eq!(placed.len(), 1, "only the one on a visible line survives");
    assert_eq!(placed[0].row, 0, "rows are relative to the viewport");
    assert_eq!(
        placed[0].label, 2,
        "and it keeps the label matching the digit that reaches it"
    );
}

#[test]
fn a_hint_on_a_line_end_keeps_that_line_s_row() {
    // A word motion lands on the newline at the end of a line, which is the
    // most common hint position of all.
    let doc = Document::from_text("alpha\nbeta\n");
    let placed = hints(&doc, Viewport { top: 0, height: 4 }, &[5]);

    assert_eq!(placed[0].row, 0);
    assert_eq!(placed[0].column, 5, "one past the last character");
}

// --- the line-number gutter (#68) -------------------------------------------

#[test]
fn a_one_digit_buffer_gets_a_two_column_gutter() {
    // The digit, plus one column of separation from the text — Emacs's own
    // `display-line-numbers-width` never lets a number touch the buffer.
    assert_eq!(gutter_width(1), 2);
    assert_eq!(gutter_width(9), 2);
}

#[test]
fn the_gutter_widens_with_the_buffer_rather_than_the_window() {
    // Sized from the *document's* line count, not from what's on screen, so
    // scrolling into more digits does not shove the text sideways mid-read.
    assert_eq!(gutter_width(10), 3);
    assert_eq!(gutter_width(99), 3);
    assert_eq!(gutter_width(100), 4);
}

#[test]
fn an_empty_buffer_still_gets_room_for_one_digit() {
    // `Document::line_count` on an empty document is 1 (line 1, no text), and
    // a gutter that shrank to zero here would be a column that disappears the
    // moment you delete the last character.
    assert_eq!(gutter_width(0), 2);
}

// --- diagnostics (#115) ---------------------------------------------------

fn diag(range: core::ops::Range<usize>, severity: Severity) -> Diagnostic {
    Diagnostic {
        range,
        message: "something".to_string(),
        severity,
    }
}

#[test]
fn a_diagnostic_becomes_a_run_named_for_its_severity() {
    // The render layer keys on the name and decides what a squiggle looks
    // like; this only says where one goes, exactly as `grabbed` does.
    let placed = diagnostics(
        &doc(),
        Viewport { top: 0, height: 3 },
        // `0..3` is "one" and stays on its own row; `0..5` would reach the
        // second line, which the splitting test below is about instead.
        &[diag(0..3, Severity::Error)],
    );
    assert_eq!(placed.len(), 1);
    assert_eq!(placed[0].name, "diagnostic.error");
    assert_eq!(placed[0].row, 0);
    assert_eq!(placed[0].columns, 0..3);
}

#[test]
fn each_severity_gets_its_own_name_so_an_error_and_a_hint_differ() {
    let all = [
        (Severity::Error, "diagnostic.error"),
        (Severity::Warning, "diagnostic.warning"),
        (Severity::Information, "diagnostic.info"),
        (Severity::Hint, "diagnostic.hint"),
    ];
    for (severity, name) in all {
        let placed = diagnostics(
            &doc(),
            Viewport { top: 0, height: 1 },
            &[diag(0..3, severity)],
        );
        assert_eq!(placed[0].name, name);
    }
}

#[test]
fn a_diagnostic_crossing_lines_is_split_per_row() {
    // Same guarantee `styled` already gives: the render layer never sees a
    // byte offset, so a span covering two lines arrives as two runs.
    let placed = diagnostics(
        &doc(),
        Viewport { top: 0, height: 3 },
        &[diag(0..8, Severity::Warning)],
    );
    assert!(placed.len() >= 2, "{placed:?}");
    assert_eq!(placed[0].row, 0);
    assert_eq!(placed[1].row, 1);
}

#[test]
fn a_diagnostic_below_the_viewport_is_dropped_rather_than_clamped() {
    // A run clamped onto the last visible row would put a squiggle under text
    // it says nothing about.
    let placed = diagnostics(
        &doc(),
        Viewport { top: 0, height: 1 },
        &[diag(6..10, Severity::Error)],
    );
    assert!(placed.is_empty(), "{placed:?}");
}

#[test]
fn no_diagnostics_places_nothing() {
    assert!(diagnostics(&doc(), Viewport { top: 0, height: 3 }, &[]).is_empty());
}

// --- hover (#116) ---------------------------------------------------------

#[test]
fn a_short_hover_is_one_line_and_is_not_truncated() {
    let view = hover("fn main()", 40, 10);
    assert_eq!(view.lines, vec!["fn main()"]);
    assert!(!view.truncated);
}

#[test]
fn a_multi_line_hover_keeps_its_own_line_breaks() {
    // A server's hover is markdown and its newlines mean something: a
    // signature on one line and its doc comment below is the commonest shape
    // there is.
    let view = hover("fn main()\n\nRuns the program.", 40, 10);
    assert_eq!(view.lines, vec!["fn main()", "", "Runs the program."]);
    assert!(!view.truncated);
}

#[test]
fn a_line_longer_than_the_width_wraps_rather_than_being_clipped() {
    // Clipping loses the end of a sentence silently; the Echo Area has rows to
    // spare and a hover is prose.
    let view = hover("alpha beta gamma delta", 11, 10);
    assert_eq!(view.lines, vec!["alpha beta", "gamma delta"]);
    assert!(!view.truncated);
}

#[test]
fn a_word_longer_than_the_width_is_broken_rather_than_dropped() {
    // A single unbreakable token — a long type name — must still appear.
    let view = hover("aaaaaaaaaa", 4, 10);
    assert_eq!(view.lines, vec!["aaaa", "aaaa", "aa"]);
}

#[test]
fn a_hover_past_the_cap_is_truncated_and_says_so() {
    // Silently showing the first three lines of a twelve-line doc comment
    // reads as "that is all there is", which is the quiet-failure shape this
    // project keeps going looking for.
    let text = (1..=12)
        .map(|n| n.to_string())
        .collect::<Vec<_>>()
        .join("\n");
    let view = hover(&text, 40, 3);
    assert_eq!(view.lines.len(), 3);
    assert!(view.truncated);
}

#[test]
fn a_hover_exactly_at_the_cap_is_not_marked_truncated() {
    let text = (1..=3)
        .map(|n| n.to_string())
        .collect::<Vec<_>>()
        .join("\n");
    let view = hover(&text, 40, 3);
    assert_eq!(view.lines.len(), 3);
    assert!(!view.truncated, "nothing was left out");
}

#[test]
fn an_empty_hover_is_no_hover_at_all() {
    // A server answering with nothing must not reserve a row to say nothing
    // in.
    assert!(hover("", 40, 10).lines.is_empty());
    assert!(hover("   \n  \n", 40, 10).lines.is_empty());
}

#[test]
fn a_zero_cap_or_zero_width_yields_nothing_rather_than_looping() {
    assert!(hover("something", 40, 0).lines.is_empty());
    assert!(hover("something", 0, 10).lines.is_empty());
}

#[test]
fn a_hover_that_fits_keeps_its_indentation() {
    // A hover is mostly code, and a struct whose fields all sit in column one
    // reads as a different struct.
    let view = hover("struct S {\n    field: u8,\n}", 40, 10);
    assert_eq!(view.lines, vec!["struct S {", "    field: u8,", "}"]);
}

// --- folds (#141) ---------------------------------------------------------

fn twelve() -> Document {
    Document::from_text(&(0..12).map(|n| format!("line {n}\n")).collect::<String>())
}

fn folding(doc: &Document, head: usize, last: usize) -> Folds {
    let mut folds = Folds::default();
    folds.fold(doc.line_to_byte(head).unwrap()..doc.line_to_byte(last + 1).unwrap());
    folds
}

#[test]
fn a_folded_region_leaves_its_head_and_takes_the_rest_of_the_rows() {
    // "`rows_with` stops being a range" -- ADR-0012 names this as the real
    // work. It has to yield the next N *unfolded* lines from the top rather
    // than the N lines after it.
    let doc = twelve();
    let folds = folding(&doc, 2, 5);

    let out = rows_with(&doc, Viewport { top: 0, height: 5 }, None, false, &folds);

    assert_eq!(
        out.iter().map(|row| row.line).collect::<Vec<_>>(),
        vec![0, 1, 2, 6, 7],
        "the gutter jumps rather than renumbering, which is the second fold \
         indicator and it is free"
    );
}

#[test]
fn a_folded_head_carries_doom_s_ellipsis() {
    // `+fold-ellipsis` is `" [...] "`, verbatim: an ellipsis somebody has
    // looked at for years is worth more than one chosen here.
    let doc = twelve();
    let folds = folding(&doc, 2, 5);

    let out = rows_with(&doc, Viewport { top: 0, height: 4 }, None, false, &folds);

    assert_eq!(out[2].text, "line 2 [...] ");
    assert_eq!(out[1].text, "line 1", "and nothing else is marked");
}

#[test]
fn a_viewport_starting_inside_a_fold_starts_at_the_next_line_it_can_show() {
    // Scrolling can leave `top` pointing at a line nothing draws -- an edit
    // above a fold, or a fold made while the view was further down. The rows
    // begin at the next line that is really there rather than coming back
    // empty.
    let doc = twelve();
    let folds = folding(&doc, 2, 5);

    let out = rows_with(&doc, Viewport { top: 4, height: 3 }, None, false, &folds);

    assert_eq!(
        out.iter().map(|row| row.line).collect::<Vec<_>>(),
        vec![6, 7, 8]
    );
}

#[test]
fn a_selection_inside_a_fold_marks_the_head_rather_than_nothing() {
    // The point is moved to the head when a region is folded, so the ordinary
    // case never arises -- but a Grab, a beacon or an edit elsewhere can leave
    // a selection spanning hidden text, and a row that dropped its half of it
    // would draw a selection with a piece missing.
    let doc = twelve();
    let folds = folding(&doc, 1, 3);
    // "line 0\n" is 7 bytes, so byte 8 is inside line 1.
    let out = rows_with(&doc, Viewport { top: 0, height: 3 }, Some(sel(8, 10)), false, &folds);

    assert_eq!(out[1].line, 1, "the head");
    assert_eq!(out[1].selected, Some(1..3));
}
