use super::*;

const BUFFERS: SourceId = SourceId(0);
const FILES: SourceId = SourceId(1);
const LINES: SourceId = SourceId(2);

fn sources() -> Vec<Source> {
    vec![
        Source {
            id: BUFFERS,
            name: "buffers".into(),
            key: 'b',
        },
        Source {
            id: FILES,
            name: "files".into(),
            key: 'f',
        },
        Source {
            id: LINES,
            name: "lines".into(),
            key: 'l',
        },
    ]
}

fn candidates() -> Vec<Candidate> {
    vec![
        Candidate::new("lib.rs", BUFFERS),
        Candidate::new("selection.rs", BUFFERS),
        Candidate::new("crates/meowr-core/src/selection-calculus.rs", FILES),
        Candidate::new("crates/meowr-view/src/modeline.rs", FILES),
        Candidate::new("fn select_line(doc: &Document)", LINES).annotated("509"),
    ]
}

fn picker() -> Narrow {
    Narrow::new(Driver::Picker, sources(), candidates())
}

fn texts(widget: &Narrow) -> Vec<&str> {
    widget
        .matches()
        .iter()
        .map(|m| widget.candidate(m.index).text.as_str())
        .collect()
}

#[test]
fn everything_matches_an_empty_input() {
    let widget = picker();
    assert_eq!(widget.matches().len(), 5);
}

#[test]
fn space_separated_terms_all_match_in_any_order() {
    // Orderless, and it comes free with nucleo's `Pattern::parse` — this test
    // exists to pin that it is actually being used, not to test nucleo.
    let mut widget = picker();
    widget.set_input("sel cal".into());
    let forwards: Vec<String> = texts(&widget).iter().map(|s| s.to_string()).collect();
    // Fuzzy matching is permissive, so more than one candidate can match; what
    // matters is that the intended one ranks first.
    assert_eq!(forwards[0], "crates/meowr-core/src/selection-calculus.rs");

    // The other order gives the same answer, which is the point of the word
    // "orderless".
    widget.set_input("cal sel".into());
    assert_eq!(texts(&widget)[0], forwards[0]);
}

#[test]
fn matching_is_fuzzy_within_a_term() {
    let mut widget = picker();
    widget.set_input("mdln".into());
    assert!(
        texts(&widget).contains(&"crates/meowr-view/src/modeline.rs"),
        "{:?}",
        texts(&widget)
    );
}

#[test]
fn the_matched_positions_come_back_so_they_can_be_marked() {
    let mut widget = picker();
    widget.set_input("lib".into());
    let index = widget.matches()[0].index;
    assert_eq!(widget.positions(index), vec![0, 1, 2]);
}

#[test]
fn narrowing_restricts_to_one_source_and_says_which() {
    let mut widget = picker();
    assert!(widget.narrowed().is_none());

    assert!(widget.narrow_to('f'));

    assert_eq!(widget.narrowed().map(|s| s.name.as_str()), Some("files"));
    assert_eq!(widget.matches().len(), 2, "only the file candidates");

    widget.widen();
    assert_eq!(widget.matches().len(), 5);
}

#[test]
fn narrowing_to_an_unknown_key_changes_nothing() {
    let mut widget = picker();
    assert!(!widget.narrow_to('z'));
    assert!(widget.narrowed().is_none());
}

#[test]
fn narrowing_clears_the_input() {
    // It was typed against a different set and almost never means the same
    // thing against this one.
    let mut widget = picker();
    widget.set_input("lib".into());
    widget.narrow_to('f');
    assert_eq!(widget.input(), "");
}

#[test]
fn the_visible_rows_are_capped_and_follow_the_selection() {
    let many: Vec<Candidate> = (0..100)
        .map(|n| Candidate::new(format!("item-{n:03}"), BUFFERS))
        .collect();
    let mut widget = Narrow::new(Driver::Picker, sources(), many);
    assert_eq!(widget.visible().len(), MAX_ROWS);

    // Walk past the end of the window; the selection stays on screen.
    for _ in 0..MAX_ROWS + 5 {
        widget.select_next();
    }
    let visible: Vec<usize> = widget.visible().iter().map(|m| m.index).collect();
    let selected = widget.matches()[widget.selected_index()].index;
    assert!(
        visible.contains(&selected),
        "{visible:?} missing {selected}"
    );
}

#[test]
fn selection_wraps_in_both_directions() {
    let mut widget = picker();
    let last = widget.matches().len() - 1;
    widget.select_previous();
    assert_eq!(widget.selected_index(), last);
    widget.select_next();
    assert_eq!(widget.selected_index(), 0);
}

#[test]
fn equal_scores_keep_a_stable_order() {
    // A list that reshuffles under an unchanged score is unusable: the thing
    // you were about to press Enter on moves out from under you.
    let same: Vec<Candidate> = (0..20)
        .map(|n| Candidate::new(format!("same-{n:02}"), BUFFERS))
        .collect();
    let mut widget = Narrow::new(Driver::Picker, sources(), same);
    widget.set_input("same".into());
    let first: Vec<usize> = widget.matches().iter().map(|m| m.index).collect();
    widget.set_input("same".into());
    let again: Vec<usize> = widget.matches().iter().map(|m| m.index).collect();
    assert_eq!(first, again);
    assert!(
        first.windows(2).all(|w| w[0] < w[1]),
        "ties keep source order"
    );
}

#[test]
fn completion_starts_from_the_prefix_already_typed() {
    // The one real difference between the two drivers: where the input comes
    // from, and what accepting replaces.
    let widget = Narrow::completing(
        vec![
            Candidate::new("selection_type", SourceId(0)),
            Candidate::new("select_line", SourceId(0)),
            Candidate::new("history_depth", SourceId(0)),
        ],
        "sel",
        10..13,
    );
    assert_eq!(widget.driver(), Driver::Completion);
    assert_eq!(widget.input(), "sel");
    assert_eq!(widget.replacing, Some(10..13));
    assert_eq!(widget.matches().len(), 2, "history_depth does not match");
}

#[test]
fn typing_stays_responsive_over_a_large_candidate_set() {
    // Not a benchmark — a guard, and deliberately not a stopwatch. The
    // previous version of this test asserted elapsed time, so it failed at
    // 12.1s and passed at 5.6s on the same code a minute apart, purely
    // because a release build was using the cores. What it was trying to say
    // is a property of the algorithm, so it is asserted as one: the work a
    // keystroke causes, which no amount of load can change.
    //
    // Ten thousand candidates rescored on every keystroke is the shape of the
    // real workload (project files).
    //
    // Verified to hold under load rather than argued: run at load average
    // 12.3 with every core deliberately occupied, this took 52 seconds of
    // wall-clock and passed unchanged. The old assertion's budget was 8s.
    let n = 10_000;
    let many: Vec<Candidate> = (0..n)
        .map(|i| Candidate::new(format!("crates/meowr-core/src/file-{i:05}.rs"), FILES))
        .collect();
    let mut widget = Narrow::new(Driver::Picker, sources(), many);

    let typed = "meowr core file 042";
    for c in typed.chars() {
        widget.push(c);
    }
    assert!(!widget.matches().is_empty());

    let work = widget.work();
    // One pass over the candidates per keystroke, plus the pass `new` does.
    // Quadratic scoring would put this at n² and nothing else would have to
    // change for it to happen — an inner loop over `matches` would do it.
    let passes = typed.chars().count() + 1;
    assert!(
        work.scored <= passes * n,
        "{} scorings for {passes} passes over {n} candidates — more than one pass each",
        work.scored
    );
    assert!(work.scored >= n, "the candidates were never scored at all");
    // Matched positions cost several times what scoring does and are only
    // wanted for the dozen rows on screen. Asking per candidate per keystroke
    // is the regression `Match`'s docstring records: it took 3.7 seconds.
    assert_eq!(
        work.located, 0,
        "matched positions were computed while typing, not on demand"
    );
    // The haystacks are converted once, in `new`, not on every keystroke.
    assert_eq!(work.converted, n);
}

#[test]
fn drawing_a_screenful_locates_a_screenful() {
    // The other half of the bargain: positions are not free, they are
    // deferred. Asking for the visible rows costs one location each, so the
    // cost of marking matches is bounded by the screen and not by the corpus.
    let many: Vec<Candidate> = (0..1_000)
        .map(|i| Candidate::new(format!("file-{i:04}.rs"), FILES))
        .collect();
    let mut widget = Narrow::new(Driver::Picker, sources(), many);
    widget.push('f');

    let visible: Vec<usize> = widget.visible().iter().map(|m| m.index).collect();
    for index in &visible {
        widget.positions(*index);
    }
    assert_eq!(widget.work().located, visible.len());
    assert!(visible.len() <= MAX_ROWS);
}

#[test]
fn extending_keeps_the_selected_candidate_rather_than_the_row() {
    // The branch that makes a late answer bearable. Rematching reorders and
    // resets `selected` to 0, so without this an arriving Project moved the
    // selection out from under somebody mid-navigation -- the interruption
    // ADR-0009 exists to avoid.
    let mut widget = Narrow::new(
        Driver::Picker,
        Vec::new(),
        vec![
            Candidate::new("alpha", SourceId(0)),
            Candidate::new("beta", SourceId(0)),
            Candidate::new("gamma", SourceId(0)),
        ],
    );
    widget.select_next();
    let chosen = widget.selected().expect("a selection").text.clone();
    assert_ne!(chosen, "alpha", "not the first row, or this proves nothing");

    widget.extend([Candidate::new("aaa-sorts-first", SourceId(0))]);

    assert_eq!(
        widget.selected().expect("still a selection").text,
        chosen,
        "the same candidate, wherever rematching put it"
    );
    assert_eq!(widget.matches().len(), 4, "and the new one is offered");
}
