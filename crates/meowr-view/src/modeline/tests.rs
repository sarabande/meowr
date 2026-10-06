use super::*;

fn status() -> Status<'static> {
    Status {
        state: "normal",
        expand_hints: 0,
        path: Some("/home/user/tools/meow/crates/meowr-view/src/lib.rs"),
        buffer_name: "lib.rs",
        modified: false,
        read_only: false,
        line: 41,
        lines: 300,
        column: 7,
        selection: None,
        selection_lines: 0,
        selection_chars: 0,
        branch: Some("main"),
        encoding: "UTF-8",
        language: Some("rust"),
        diagnostics: 0,
        focused: true,
    }
}

#[test]
fn a_buffer_name_carries_three_path_segments() {
    // `uniquify-buffer-name-style 'forward` with `min-dir-content 3`.
    assert_eq!(
        short_path("/home/user/tools/meow/crates/meowr-view/src/lib.rs", 3),
        "meowr-view/src/lib.rs"
    );
    // A shorter path gives what there is, rather than padding.
    assert_eq!(short_path("/tmp/notes.txt", 3), "tmp/notes.txt");
    assert_eq!(short_path("notes.txt", 3), "notes.txt");
}

#[test]
fn the_state_is_always_shown_and_carries_the_expand_hint() {
    let line = fit(&modeline(&status()), 80);
    assert!(line.text().starts_with("NORMAL"), "{}", line.text());

    let with_hints = Status {
        expand_hints: 7,
        ..status()
    };
    let line = fit(&modeline(&with_hints), 80);
    assert!(line.text().starts_with("NORMAL 7"), "{}", line.text());
}

#[test]
fn the_state_survives_a_frame_too_narrow_for_anything_else() {
    // It decides what every key does. A modeline that dropped it would be
    // worse than no modeline at all.
    let line = fit(&modeline(&status()), 4);
    assert_eq!(line.left.len(), 1);
    assert_eq!(line.left[0].kind, Kind::State);
    assert!(line.right.is_empty());
}

#[test]
fn selection_info_reports_the_type_not_only_a_count() {
    // What Emacs's region cannot say. With Meow's typed selections, what the
    // next key does depends on the axis and kind.
    let selected = Status {
        selection: Some(("expand", "line")),
        selection_lines: 3,
        selection_chars: 84,
        ..status()
    };
    let text = fit(&modeline(&selected), 100).text();
    assert!(text.contains("3L"), "{text}");
    assert!(text.contains("84C"), "{text}");
    assert!(text.contains("expand·line"), "{text}");
}

#[test]
fn no_selection_means_no_selection_segment() {
    let text = fit(&modeline(&status()), 100).text();
    assert!(!text.contains('·'), "{text}");
}

#[test]
fn the_modified_and_read_only_markers_are_in_front_of_the_name() {
    let modified = Status {
        modified: true,
        ..status()
    };
    assert!(
        fit(&modeline(&modified), 100)
            .text()
            .contains("* meowr-view")
    );

    let read_only = Status {
        read_only: true,
        modified: true,
        ..status()
    };
    let text = fit(&modeline(&read_only), 100).text();
    assert!(text.contains("%% meowr-view"), "{text}");
}

#[test]
fn narrowing_drops_whole_segments_by_priority_rather_than_clipping() {
    let full = fit(&modeline(&status()), 100);
    let kinds = |f: &Fitted| -> Vec<Kind> {
        f.left
            .iter()
            .chain(f.right.iter())
            .map(|s| s.kind)
            .collect()
    };
    assert!(kinds(&full).contains(&Kind::Encoding));

    // Encoding has the lowest priority, so it goes first.
    let narrower = fit(&modeline(&status()), 44);
    assert!(
        !kinds(&narrower).contains(&Kind::Encoding),
        "{:?}",
        narrower.text()
    );
    assert!(
        kinds(&narrower).contains(&Kind::Position),
        "position outranks it"
    );

    // Nothing that survives is shortened.
    for segment in narrower.left.iter().chain(narrower.right.iter()) {
        assert!(
            full.left
                .iter()
                .chain(full.right.iter())
                .any(|s| s.text == segment.text),
            "{:?} was clipped",
            segment.text
        );
    }
}

#[test]
fn the_branch_name_is_never_truncated() {
    // `doom-modeline-vcs-max-length` is 100 in the reference configuration,
    // which is to say: never. A half-written branch name is worse than none,
    // because it can name a branch that exists.
    let long = Status {
        branch: Some("feature/the-whole-selection-calculus-rewrite"),
        ..status()
    };
    let wide = fit(&modeline(&long), 120);
    assert!(
        wide.text()
            .contains("feature/the-whole-selection-calculus-rewrite"),
        "{}",
        wide.text()
    );

    let narrow = fit(&modeline(&long), 50);
    assert!(
        !narrow.text().contains("feature/"),
        "the segment drops whole: {}",
        narrow.text()
    );
}

#[test]
fn the_line_fills_the_width_with_the_two_halves_apart() {
    let line = fit(&modeline(&status()), 80);
    assert_eq!(line.text().chars().count(), 80);
    assert!(line.gap > 0, "the halves do not touch");
}

#[test]
fn position_is_one_based_because_that_is_what_a_reader_counts() {
    let text = fit(&modeline(&status()), 100).text();
    assert!(text.contains("42:8/300"), "{text}");
}

#[test]
fn a_clean_buffer_carries_no_diagnostic_segment() {
    // `0!` on every buffer nobody has a problem with is noise, and the count
    // exists to be noticed.
    let line = fit(&modeline(&status()), 120);
    assert!(!line.text().contains('!'), "{}", line.text());
}

#[test]
fn diagnostics_are_counted_on_the_modeline() {
    let line = fit(
        &modeline(&Status {
            diagnostics: 3,
            ..status()
        }),
        120,
    );
    assert!(line.text().contains("3!"), "{}", line.text());
}

#[test]
fn the_count_outlives_the_language_on_a_narrow_line() {
    // A count of problems is worth more of a cramped line than the name of the
    // mode is, which is what the two priorities say.
    let wide = modeline(&Status {
        diagnostics: 2,
        ..status()
    });
    let narrow = fit(&wide, 40);
    assert!(narrow.text().contains("2!"), "{}", narrow.text());
    assert!(!narrow.text().contains("rust"), "{}", narrow.text());
}
