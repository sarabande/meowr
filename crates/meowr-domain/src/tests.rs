use super::*;

#[test]
fn a_document_exposes_its_lines() {
    let doc = Document::from_text("first\nsecond\nthird\n");

    assert_eq!(doc.line_count(), 3);
    assert_eq!(doc.line(0).unwrap().to_string(), "first\n");
    assert_eq!(doc.line(2).unwrap().to_string(), "third\n");
    assert!(doc.line(3).is_none());
}

#[test]
fn a_document_without_a_trailing_newline_still_has_its_last_line() {
    let doc = Document::from_text("alpha\nbeta");

    assert_eq!(doc.line_count(), 2);
    assert_eq!(doc.line(1).unwrap().to_string(), "beta");
}

#[test]
fn an_empty_document_has_one_empty_line() {
    let doc = Document::from_text("");

    assert_eq!(doc.line_count(), 1);
    assert_eq!(doc.line(0).unwrap().to_string(), "");
}

#[test]
fn byte_offsets_count_bytes_not_characters() {
    // "héllo" is 6 bytes, not 5: é is two bytes in UTF-8. Likewise "wörld".
    let doc = Document::from_text("héllo\nwörld\n");

    assert_eq!(doc.byte_len(), 14);
    assert_eq!(doc.line_to_byte(0), Some(0));
    assert_eq!(doc.line_to_byte(1), Some(7));
    assert_eq!(doc.line_to_byte(2), None);
}

#[test]
fn a_byte_offset_maps_back_to_its_line() {
    let doc = Document::from_text("héllo\nwörld\n");

    assert_eq!(doc.byte_to_line(0), Some(0));
    assert_eq!(doc.byte_to_line(6), Some(0)); // the newline ending line 0
    assert_eq!(doc.byte_to_line(7), Some(1));
    assert_eq!(doc.byte_to_line(13), Some(1));
    assert_eq!(doc.byte_to_line(15), None); // past the end
}

#[test]
fn a_byte_offset_inside_a_multi_byte_character_is_rejected() {
    // Byte 2 is the second byte of "é". Nothing may hand that to tree-sitter.
    let doc = Document::from_text("héllo");

    assert!(!doc.is_char_boundary(2));
    assert!(doc.is_char_boundary(1));
    assert!(doc.is_char_boundary(3));
}

#[test]
fn text_can_be_inserted_at_a_byte_offset() {
    let mut doc = Document::from_text("helo\n");

    doc.insert(3, "l").unwrap();

    assert_eq!(doc.line(0).unwrap().to_string(), "hello\n");
    assert_eq!(doc.byte_len(), 6);
}

#[test]
fn inserting_multi_byte_text_reports_the_bytes_it_added() {
    let mut doc = Document::from_text("hello\n");

    // "é" is two bytes, so the point must advance by two, not one.
    let added = doc.insert(5, "é").unwrap();

    assert_eq!(added, 2);
    assert_eq!(doc.line(0).unwrap().to_string(), "helloé\n");
}

#[test]
fn text_can_be_deleted_by_byte_range() {
    let mut doc = Document::from_text("hello world\n");

    doc.delete(5..11).unwrap();

    assert_eq!(doc.line(0).unwrap().to_string(), "hello\n");
}

#[test]
fn deleting_a_multi_byte_character_removes_all_of_its_bytes() {
    let mut doc = Document::from_text("héllo");

    doc.delete(1..3).unwrap();

    assert_eq!(doc.text().to_string(), "hllo");
}

#[test]
fn an_edit_at_a_non_boundary_offset_is_refused_rather_than_rounded() {
    let mut doc = Document::from_text("héllo");

    // Byte 2 is the second byte of "é". Rounding here would silently corrupt
    // the text and, later, any tree-sitter edit derived from it.
    assert!(doc.insert(2, "x").is_err());
    assert!(doc.delete(2..4).is_err());
    assert_eq!(doc.text().to_string(), "héllo");
}

#[test]
fn an_edit_past_the_end_is_refused() {
    let mut doc = Document::from_text("abc");

    assert!(doc.insert(99, "x").is_err());
    assert!(doc.delete(1..99).is_err());
}

// --- a Place, and the line-and-column it is made of (#133) ---------------

#[test]
fn a_place_becomes_an_offset_only_where_there_is_a_document() {
    // The whole reason a Place is a line and a column rather than a byte
    // offset: it names somewhere in a file that is usually not open, and an
    // offset cannot be computed for text nobody has read. The conversion
    // happens here, at the one moment there is a Document to convert against.
    let doc = Document::from_text("alpha\nbeta\ngamma\n");
    let place = Place::new("src/lib.rs", 1, 2);
    assert_eq!(place.offset_in(&doc), Some(8), "`beta`, two in");
    assert_eq!(Place::new("x", 0, 0).offset_in(&doc), Some(0));
    assert_eq!(Place::new("x", 2, 5).offset_in(&doc), Some(16));
}

#[test]
fn a_column_is_utf16_because_the_one_source_that_cannot_convert_counts_that_way() {
    // LSP names a position in a file it has not read, so it cannot count bytes
    // in one -- which makes its unit the only one every source can produce.
    // A source holding the line's text converts to it; a server cannot.
    let doc = Document::from_text("é🦀x\n");
    assert_eq!(
        Place::new("x", 0, 1).offset_in(&doc),
        Some(2),
        "past é is two bytes"
    );
    assert_eq!(
        Place::new("x", 0, 3).offset_in(&doc),
        Some(6),
        "a crab is two units"
    );
}

#[test]
fn a_place_past_the_end_lands_at_the_end_rather_than_refusing() {
    // The same clamping the LSP conversion always did, and for the same
    // reason: a server may name the line *after* the last one, because the end
    // of a range covering the final line is spelled that way.
    let doc = Document::from_text("abc\n");
    assert_eq!(
        Place::new("x", 0, 99).offset_in(&doc),
        Some(3),
        "past the line"
    );
    assert_eq!(Place::new("x", 9, 0).offset_in(&doc), Some(doc.byte_len()));
}

#[test]
fn a_place_carries_its_line_only_when_the_source_had_one() {
    // Grep hands back the matched line; a language server hands back positions
    // and nothing else. A `None` is a fact about LSP rather than a gap, and it
    // is what makes the case for previewing the selected row.
    assert_eq!(Place::new("src/lib.rs", 4, 0).text, None);
    let shown = Place::new("src/lib.rs", 4, 0).showing("    let x = 1;");
    assert_eq!(shown.text.as_deref(), Some("    let x = 1;"));
    assert_eq!(shown.line, 4, "and it is still the same place");
}

#[test]
fn a_position_becomes_the_byte_offset_of_that_column() {
    // Written for the LSP client and moved here with the walk: it takes a
    // Document and two numbers, and there is no protocol in it.
    let doc = Document::from_text("one\ntwo\nthree\n");
    assert_eq!(doc.offset_at(0, 0), Some(0));
    assert_eq!(doc.offset_at(1, 0), Some(4));
    assert_eq!(doc.offset_at(1, 2), Some(6));
    assert_eq!(doc.offset_at(2, 5), Some(13));
    // A column past the end stops at the newline rather than running into the
    // next line, which would put the end of a squiggle under a different row.
    assert_eq!(doc.offset_at(0, 99), Some(3));
}

#[test]
fn a_byte_offset_becomes_the_position_a_place_is_made_of() {
    // The other direction, and the one a diagnostic needs: it carries a byte
    // range and a Place carries a line and a UTF-16 column, so the list of
    // them cannot be built without this walk.
    let doc = Document::from_text("one\ntwo\nthree\n");
    assert_eq!(doc.position_at(0), Some((0, 0)));
    assert_eq!(doc.position_at(4), Some((1, 0)));
    assert_eq!(doc.position_at(6), Some((1, 2)));
    assert_eq!(doc.position_at(13), Some((2, 5)));
    assert_eq!(doc.position_at(99), None, "past the end names no position");
}

#[test]
fn a_position_counts_utf16_units_the_way_offset_at_reads_them() {
    // Round-trip, against the one function that already speaks this unit: an
    // emoji is two UTF-16 units and four bytes, and a column counted in either
    // of the other two puts a jump in the wrong place. "é" is one unit, two
    // bytes -- the two disagree in both directions, which is the point.
    let doc = Document::from_text("héllo 🌍 world\nsecond\n");
    for offset in (0..doc.byte_len()).filter(|o| doc.is_char_boundary(*o)) {
        let (line, column) = doc.position_at(offset).expect("inside the document");
        assert_eq!(
            doc.offset_at(line, column),
            Some(offset),
            "offset {offset} became {line}:{column}, which is somewhere else"
        );
    }
}

#[test]
fn a_line_edit_past_the_end_is_refused_where_a_diagnostic_would_be_clamped() {
    // A replacement is written, not drawn: one computed against a longer file
    // would otherwise put its text at the end of this one (#167).
    let doc = Document::from_text("one\ntwo\n");
    let edit = |start: (usize, usize), end: (usize, usize)| LineEdit {
        start: LineColumn { line: start.0, column: start.1 },
        end: LineColumn { line: end.0, column: end.1 },
        text: "x".into(),
    };
    let range = |e| doc.replacement_for(&e).map(|r| r.range);
    assert_eq!(range(edit((1, 0), (1, 3))), Ok(4..7));
    assert_eq!(range(edit((1, 3), (2, 0))), Ok(7..8), "the line after the last is the end");
    assert_eq!(range(edit((0, 1), (0, 99))), Ok(1..3), "a column past a line's end is clamped");
    assert_eq!(range(edit((0, 0), (5, 0))), Err(PastEnd { line: 5 }), "a line past the end names it");
}

// --- folds (#141) ---------------------------------------------------------

/// Three folds' worth of document: lines 0..11.
fn folded(doc: &Document, regions: &[(usize, usize)]) -> Folds {
    let mut folds = Folds::default();
    for (head, last) in regions {
        let start = doc.line_to_byte(*head).unwrap();
        let end = doc.line_to_byte(*last + 1).unwrap_or(doc.byte_len());
        folds.fold(start..end);
    }
    folds
}

fn twelve_lines() -> Document {
    Document::from_text(&(0..12).map(|n| format!("line {n}\n")).collect::<String>())
}

#[test]
fn a_fold_hides_every_line_but_its_head() {
    // "A region of a buffer collapsed to its first line, which stays" -- the
    // glossary's words. The head is what you unfold from, so hiding it would
    // leave nothing to press.
    let doc = twelve_lines();
    let folds = folded(&doc, &[(2, 5)]);

    assert!(!folds.hides(&doc, 2), "the head stays");
    for line in 3..=5 {
        assert!(folds.hides(&doc, line), "line {line} is inside");
    }
    assert!(!folds.hides(&doc, 6), "and the line after is not");
    assert!(folds.heads(&doc, 2), "the head knows it is one");
    assert!(!folds.heads(&doc, 3));
}

#[test]
fn the_next_visible_line_steps_over_what_is_hidden() {
    let doc = twelve_lines();
    let folds = folded(&doc, &[(2, 5), (8, 9)]);

    assert_eq!(folds.next_visible(&doc, 0), 0, "nothing to step over");
    assert_eq!(folds.next_visible(&doc, 3), 6, "out of the first fold");
    assert_eq!(folds.next_visible(&doc, 9), 10, "out of the second");
}

#[test]
fn walking_visible_lines_skips_folded_ones_in_both_directions() {
    // The arithmetic scrolling is made of: "down one" must step over what is
    // hidden, in both directions, or the view jumps by a screenful of nothing.
    let doc = twelve_lines();
    let folds = folded(&doc, &[(2, 5)]);

    // 0 1 2 [3 4 5 hidden] 6 7 8 ...
    assert_eq!(folds.forward(&doc, 0, 3), 6, "three visible lines on from 0");
    assert_eq!(folds.backward(&doc, 6, 3), 0, "and three back again");
    assert_eq!(folds.forward(&doc, 2, 1), 6);
    assert_eq!(folds.backward(&doc, 6, 1), 2);
    assert_eq!(folds.backward(&doc, 1, 9), 0, "clamped at the top");
}

#[test]
fn a_fold_is_a_span_of_text_and_moves_with_it() {
    // Stored as bytes rather than line numbers, which is what the Grab and a
    // diagnostic already are and for the same reason: a fold kept as "lines 2
    // to 5" hides the wrong region the moment a line is typed above it, and
    // says nothing about having done so.
    let doc = twelve_lines();
    let mut folds = folded(&doc, &[(2, 5)]);
    let with_one_more = Document::from_text(&format!("new\n{}", doc.text()));

    // "new\n" typed at the top: four bytes before everything.
    folds.shift_each(|region| region.start + 4..region.end + 4);

    assert!(folds.hides(&with_one_more, 4), "the region moved down one");
    assert!(!folds.hides(&with_one_more, 3), "and its head with it");
    assert!(folds.heads(&with_one_more, 3));
}

#[test]
fn unfolding_takes_away_the_fold_the_line_is_the_head_of() {
    let doc = twelve_lines();
    let mut folds = folded(&doc, &[(2, 5), (8, 9)]);

    assert!(folds.unfold(&doc, 2), "there was one here");
    assert!(!folds.hides(&doc, 3));
    assert!(folds.hides(&doc, 9), "the other is untouched");
    assert!(!folds.unfold(&doc, 2), "and now there is not");
}

#[test]
fn revealing_a_line_opens_what_hides_it_and_leaves_the_rest() {
    // "Unfolds *enough* to show the target". A fold elsewhere was never in the
    // way, and opening the lot would undo work the reader had done.
    let doc = twelve_lines();
    let mut folds = folded(&doc, &[(2, 5), (8, 9)]);

    assert!(folds.reveal(&doc, 4), "line 4 was hidden");

    assert!(!folds.hides(&doc, 4));
    assert!(folds.hides(&doc, 9), "the other fold is untouched");
    assert!(!folds.reveal(&doc, 4), "and nothing left to open");
}

#[test]
fn revealing_opens_every_nested_fold_in_the_way() {
    // Nested folds both hide a line, so "the one in the way" is not one fold.
    // Opening only the outermost would leave the target hidden and the jump
    // still looking broken.
    let doc = twelve_lines();
    let mut folds = folded(&doc, &[(1, 10), (3, 6)]);
    assert!(folds.hides(&doc, 5));

    folds.reveal(&doc, 5);

    assert!(!folds.hides(&doc, 5), "both were in the way, so both opened");
}

#[test]
fn a_replacement_crossing_a_folds_edge_opens_it_and_one_on_the_edge_does_not() {
    // ADR-0013. On the edge is the case that matters: a formatter reindenting
    // a fold's head line replaces bytes starting exactly where the fold does,
    // and opening every fold on every format would make folding useless in a
    // formatted file.
    let doc = Document::from_text("fn f() {\n    one();\n    two();\n}\n");
    let fold = |folds: &mut Folds| folds.fold(0..32);
    let folded = |folds: &Folds| folds.hides(&doc, 1);

    let mut folds = Folds::default();
    fold(&mut folds);
    folds.open_crossed(&[0..4, 28..32]);
    assert!(folded(&folds), "starting on its start and ending on its end");

    let mut folds = Folds::default();
    fold(&mut folds);
    folds.open_crossed(std::slice::from_ref(&(13..16)));
    assert!(folded(&folds), "wholly inside");

    let mut folds = Folds::default();
    fold(&mut folds);
    folds.open_crossed(std::slice::from_ref(&(30..34)));
    assert!(!folded(&folds), "across its end");
}

// --- a formatter's whole output as the hunks that changed (#162) -------------

/// The replacements applied to `old`, highest first. Not `History`'s own code,
/// so these tests do not depend on it; diff output never ties two ranges, which
/// is the only case its tie-breaking is for.
fn applied(old: &str, replacements: &[Replacement]) -> String {
    let mut ordered: Vec<&Replacement> = replacements.iter().collect();
    ordered.sort_by_key(|r| std::cmp::Reverse((r.range.start, r.range.end)));
    ordered.iter().fold(old.to_string(), |mut text, r| {
        text.replace_range(r.range.clone(), &r.text);
        text
    })
}

#[test]
fn text_a_formatter_left_alone_needs_no_replacements() {
    assert!(replacements_between("fn f() {}\n", "fn f() {}\n").is_empty());
}

#[test]
fn a_reindented_line_is_a_replacement_of_its_indentation_alone() {
    // What keeps the point on its code through a format: the code itself is
    // outside every replacement, so nothing moves it but the shift.
    let old = "fn f() {\n    x();\n}\n";
    let new = "fn f() {\n  x();\n}\n";
    let replacements = replacements_between(old, new);
    assert_eq!(applied(old, &replacements), new);
    assert!(
        replacements.iter().all(|r| r.range.start >= 9 && r.range.end <= 13),
        "only the four spaces before `x();`: {replacements:?}"
    );
}

#[test]
fn a_line_added_above_is_one_insertion_of_that_line() {
    let old = "use a;\nfn f() {}\n";
    let new = "use a;\nuse b;\nfn f() {}\n";
    assert_eq!(
        replacements_between(old, new),
        vec![Replacement {
            range: 7..7,
            text: "use b;\n".to_string(),
        }]
    );
}

#[test]
fn applying_the_replacements_to_the_old_text_always_gives_the_new() {
    // Many generated pairs rather than a crate for it: a deterministic walk of
    // the edits a formatter makes -- indentation changed, lines joined and
    // split, words renamed, blank lines added and taken away, and non-ASCII so
    // a byte offset inside a character would show.
    let base: Vec<String> = (0..40)
        .map(|n| format!("{}let value_{n} = “{n}” + é;", " ".repeat(n % 3 * 4)))
        .collect();
    let mut seed = 0x9e37_79b9_u32;
    let mut next = move || {
        seed ^= seed << 13;
        seed ^= seed >> 17;
        seed ^= seed << 5;
        seed as usize
    };
    for _ in 0..200 {
        let old = base.join("\n") + "\n";
        let new: String = base
            .iter()
            .filter_map(|line| match next() % 7 {
                0 => None,
                1 => Some(format!("{}\n\n", line.trim_start())),
                2 => Some(format!("  {}\n", line.replace("value", "item"))),
                3 => Some(line.replace(" + ", "+") + "\n"),
                _ => Some(format!("{line}\n")),
            })
            .collect();
        let replacements = replacements_between(&old, &new);
        assert_eq!(applied(&old, &replacements), new);
        assert!(
            replacements.windows(2).all(|pair| pair[0].range.end <= pair[1].range.start),
            "in order and never overlapping, which `History` requires: {replacements:?}"
        );
    }
}

#[test]
fn a_hunk_too_large_to_refine_is_replaced_whole() {
    // A bound, so a formatter rewriting a generated file cannot stall a save on
    // word-level diffing: past it, the changed block goes in as one replacement.
    let old = format!("{}\n", "a ".repeat(REFINE_LIMIT));
    let new = format!("{}\n", "b ".repeat(REFINE_LIMIT));
    let replacements = replacements_between(&old, &new);
    assert_eq!(replacements.len(), 1, "one replacement for the block");
    assert_eq!(applied(&old, &replacements), new);
}

#[test]
fn reindenting_several_lines_replaces_each_indentation_on_its_own_line() {
    // `similar`'s word tokens merge a newline with the indentation after it,
    // which put a replacement's start on the line above -- across the head of
    // any fold starting there, which then opened.
    let old = "fn f() {\n    if a {\n        x();\n    }\n}\n";
    let new = "fn f() {\n  if a {\n    x();\n  }\n}\n";
    let replacements = replacements_between(old, new);
    assert_eq!(applied(old, &replacements), new);
    let line_starts: Vec<usize> = std::iter::once(0)
        .chain(old.match_indices('\n').map(|(at, _)| at + 1))
        .collect();
    for r in &replacements {
        let line = line_starts.partition_point(|&start| start <= r.range.start) - 1;
        let next = line_starts.get(line + 1).copied().unwrap_or(old.len());
        assert!(r.range.end <= next, "{r:?} reaches past its own line");
        assert!(!old[r.range.clone()].contains('\n'), "{r:?} takes a newline");
    }
}
