use super::*;

fn given(name: &str) -> Given {
    Given {
        name: name.to_string(),
        buffer: "hello world\nsecond line\n".to_string(),
        point: 0,
        start: None,
        keys: vec!['m', 'w'],
        diverges: None,
    }
}

#[test]
fn escaping_round_trips_the_characters_a_tsv_cannot_carry() {
    let awkward = "a\tb\nc\\d\re";
    assert_eq!(unescape(&escape(awkward)).unwrap(), awkward);
    assert!(!escape(awkward).contains('\t'));
    assert!(!escape(awkward).contains('\n'));
}

#[test]
fn an_unknown_escape_is_an_error_rather_than_a_silent_drop() {
    assert!(unescape("a\\q").is_err());
    assert!(unescape("a\\").is_err());
}

#[test]
fn cases_round_trip() {
    let cases = vec![
        given("plain"),
        Given {
            start: Some(Start {
                axis: "expand".into(),
                kind: "word".into(),
                mark: 3,
            }),
            point: 8,
            diverges: Some("meow-to-block not implemented".into()),
            ..given("from-a-selection")
        },
    ];
    assert_eq!(parse_cases(&render_cases(&cases)).unwrap(), cases);
}

#[test]
fn comments_and_blank_lines_are_ignored() {
    let text = "# a header\n\nplain\tx\t0\t-\t-\t-\th\t-\n\n";
    let cases = parse_cases(text).unwrap();
    assert_eq!(cases.len(), 1);
    assert_eq!(cases[0].name, "plain");
    assert_eq!(cases[0].keys, vec!['h']);
}

#[test]
fn a_half_specified_starting_selection_is_rejected() {
    // An axis with no mark is the shape a hand-edited case file lands in, and
    // silently dropping the selection would make the case pass for the wrong
    // reason.
    let text = "bad\tx\t0\texpand\tword\t-\th\t-";
    let err = parse_cases(text).unwrap_err();
    assert_eq!(err.line, 1);
    assert!(err.message.contains("all of axis, kind and mark"));
}

#[test]
fn a_short_row_names_the_line() {
    let err = parse_cases("# header\nbad\tx\t0").unwrap_err();
    assert_eq!(err.line, 2);
    assert!(err.message.contains("expected 8"));
}

fn observation() -> Observation {
    Observation {
        text: "hello world\n".to_string(),
        point: 5,
        mark: Some(0),
        axis: Some("expand".into()),
        kind: Some("word".into()),
        history: 1,
        signals: 0,
        state: "normal".to_string(),
        grab: Some((2, 7)),
    }
}

#[test]
fn goldens_round_trip() {
    let mut map = BTreeMap::new();
    map.insert("with-selection".to_string(), observation());
    map.insert(
        "bare-point".to_string(),
        Observation {
            mark: None,
            axis: None,
            kind: None,
            history: 0,
            ..observation()
        },
    );
    assert_eq!(parse_golden(&render_golden(&map)).unwrap(), map);
}

#[test]
fn a_duplicate_case_name_in_a_golden_is_an_error() {
    // Names are the join key. A duplicate means one of the two observations
    // would be dropped and the case it belongs to would be checked against the
    // wrong row.
    let text = "dup\tx\t0\t-\t-\t-\t0\t0\tnormal\t-\ndup\ty\t1\t-\t-\t-\t0\t0\tnormal\t-";
    let err = parse_golden(text).unwrap_err();
    assert!(err.message.contains("duplicate"));
}

#[test]
fn diff_is_none_when_the_observations_agree() {
    assert_eq!(diff(&observation(), &observation()), None);
}

#[test]
fn diff_names_every_field_that_differs_not_just_the_first() {
    let actual = Observation {
        point: 6,
        history: 2,
        ..observation()
    };
    let report = diff(&observation(), &actual).expect("these differ");
    assert!(report.contains("point"), "{report}");
    assert!(report.contains("history"), "{report}");
    assert!(!report.contains("kind"), "{report}");
}

#[test]
fn diff_reports_a_selection_appearing_where_emacs_had_none() {
    let expected = Observation {
        mark: None,
        axis: None,
        kind: None,
        ..observation()
    };
    let report = diff(&expected, &observation()).expect("these differ");
    assert!(report.contains("mark"), "{report}");
    assert!(report.contains("axis"), "{report}");
}
