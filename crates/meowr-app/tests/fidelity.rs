//! meowr's Normal State, compared against the real Meow package.
//!
//! Every expected value in `fidelity/*.golden` was produced by running Emacs.
//! Nothing here asserts what Meow *ought* to do; it asserts that meowr does
//! what Meow *did*.
//!
//! Regenerate the goldens with `tools/oracle/regenerate.sh` when the pinned
//! Meow moves. This test needs only the checked-in files, so CI does not need
//! Emacs.

use meowr_app::Session;
use meowr_command::Key;
use meowr_core::{Axis, Kind, Selection};
use meowr_domain::Document;
use meowr_fidelity::{Given, Observation, diff, parse_cases, parse_golden};
use meowr_view::Viewport;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

fn fidelity_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fidelity")
}

fn axis_from(name: &str) -> Axis {
    match name {
        "expand" => Axis::Expand,
        "select" => Axis::Select,
        other => panic!("unknown axis `{other}` in a case file"),
    }
}

/// Replay one case through the session exactly as a person would: one key at a
/// time, through the keymap, not by calling motions directly. The keymap is
/// part of what is being tested — a motion bound to the wrong key is a
/// fidelity failure like any other.
fn run(case: &Given) -> Observation {
    let mut session = Session::new(Viewport { top: 0, height: 64 });
    session.loaded(Document::from_text(&case.buffer));
    session.set_selection(match &case.start {
        None => Selection::at(case.point),
        Some(start) => Selection::typed(
            axis_from(&start.axis),
            Kind::from_name(&start.kind)
                .unwrap_or_else(|| panic!("unknown kind `{}` in a case file", start.kind)),
            start.mark,
            case.point,
        ),
    });
    // `replay`, not `on_key`: the oracle drives every key through
    // `execute-kbd-macro`, and Meow reads `executing-kbd-macro` — a key
    // *typed* can enter Beacon State where the same key replayed cannot. One
    // key at a time, as the oracle does, so the per-key error isolation the
    // signal column depends on is unchanged.
    for &key in &case.keys {
        session.replay(&[Key::Char(key)]);
    }

    let selection = session.selection();
    let typed = selection.selection_type();
    Observation {
        text: session
            .document()
            .expect("the document was loaded")
            .text()
            .to_string(),
        point: selection.pos(),
        // Emacs reports a mark when the *region* is active, which is usually
        // but not always when Meow has a selection type: `meow-pop-grab`
        // activates a region with no type at all. meowr keeps a mark either
        // way, so an untyped one counts only when it spans something.
        mark: (typed.is_some() || selection.mark() != selection.pos()).then(|| selection.mark()),
        axis: typed.map(|(axis, _)| match axis {
            Axis::Expand => "expand".to_string(),
            Axis::Select => "select".to_string(),
        }),
        kind: typed.map(|(_, kind)| kind.name().to_string()),
        history: session.history_depth(),
        signals: session.signals(),
        state: session.state().name().to_string(),
        grab: session.grab().map(|range| (range.start, range.end)),
    }
}

struct Failure {
    name: String,
    keys: String,
    report: String,
}

fn check_suite(suite: &str) -> Vec<Failure> {
    let dir = fidelity_dir();
    let cases = std::fs::read_to_string(dir.join(format!("{suite}.cases")))
        .unwrap_or_else(|e| panic!("{suite}.cases: {e}"));
    let golden = std::fs::read_to_string(dir.join(format!("{suite}.golden")))
        .unwrap_or_else(|e| panic!("{suite}.golden: {e} -- run tools/oracle/regenerate.sh"));
    let cases = parse_cases(&cases).unwrap_or_else(|e| panic!("{suite}.cases: {e}"));
    let mut golden: BTreeMap<_, _> =
        parse_golden(&golden).unwrap_or_else(|e| panic!("{suite}.golden: {e}"));

    let mut failures = Vec::new();
    for case in &cases {
        let keys: String = case.keys.iter().collect();
        let Some(expected) = golden.remove(&case.name) else {
            failures.push(Failure {
                name: case.name.clone(),
                keys,
                report: "  no golden row; run tools/oracle/regenerate.sh".into(),
            });
            continue;
        };
        let actual = run(case);
        match (diff(&expected, &actual), &case.diverges) {
            (None, None) => {}
            (Some(report), None) => failures.push(Failure {
                name: case.name.clone(),
                keys,
                report,
            }),
            // A recorded divergence that still diverges is the expected state.
            (Some(_), Some(_)) => {}
            // ...and one that has stopped diverging is a stale annotation. It
            // has to fail, or the file slowly fills with claims about meowr
            // that are no longer true.
            (None, Some(reason)) => failures.push(Failure {
                name: case.name.clone(),
                keys,
                report: format!(
                    "  marked as diverging ({reason}) but now matches Emacs;\n  \
                     drop the `diverges` column for this case"
                ),
            }),
        }
    }

    for orphan in golden.keys() {
        failures.push(Failure {
            name: orphan.clone(),
            keys: String::new(),
            report: "  golden row with no case; regenerate after editing cases".into(),
        });
    }
    failures
}

fn report(suite: &str) {
    let failures = check_suite(suite);
    if failures.is_empty() {
        return;
    }

    let mut by_key: BTreeMap<&str, usize> = BTreeMap::new();
    for f in &failures {
        *by_key.entry(f.keys.as_str()).or_default() += 1;
    }

    let shown = 25.min(failures.len());
    let mut out = format!(
        "{} of the {suite} suite diverged from Emacs.\n\nby key sequence: {}\n\nfirst {shown}:\n",
        failures.len(),
        by_key
            .iter()
            .map(|(k, n)| format!("{k}={n}"))
            .collect::<Vec<_>>()
            .join(" ")
    );
    out.push_str("\nall diverging cases:\n");
    for f in &failures {
        out.push_str(&format!("  {}\n", f.name));
    }
    out.push_str(&format!("\nfirst {shown} in detail:\n"));
    for f in failures.iter().take(shown) {
        out.push_str(&format!("\n{}\n{}\n", f.name, f.report));
    }
    panic!("{out}");
}

/// One test per suite rather than one per case: 939 separate `#[test]`s would
/// need code generation, and the useful output from a fidelity run is the
/// *pattern* across failures — which key, which starting selection — not the
/// first one alphabetically.
#[test]
fn motions_match_emacs() {
    report("motions");
}

#[test]
fn expand_matches_emacs() {
    report("expand");
}

#[test]
fn editing_matches_emacs() {
    report("editing");
}

#[test]
fn things_match_emacs() {
    report("things");
}

#[test]
fn grab_matches_emacs() {
    report("grab");
}

/// ADR-0008: every case above is measured against a `Session` no config has
/// touched.
///
/// The guarantee is really the crate graph — `meowr-app` does not depend on
/// `steel-core` and reads no files, so a config cannot reach this harness at
/// all. That holds until someone adds the dependency, and nothing but this
/// says they should not.
///
/// `expand-axis` is why it matters rather than being a tidiness point. It is
/// the one Option that reaches the selection calculus — `core::expand` takes
/// it — so a session carrying a configured value would answer the 69 cases in
/// `expand.cases` differently while every assertion still read as a claim
/// about Meow.
#[test]
fn the_harness_observes_a_session_no_config_has_touched() {
    let session = Session::new(Viewport { top: 0, height: 64 });

    // Against Meow's documented default rather than against our own, so this
    // cannot pass by agreeing with whatever `Options::default` happens to say.
    assert_eq!(
        session.options().expand_axis,
        meowr_core::Axis::Select,
        "`meow-expand-selection-type` is `select`, and `expand.cases` was \
         recorded against an Emacs that had it"
    );
    // And the tripwire for the rest: nothing else may drift either.
    assert_eq!(
        session.options(),
        meowr_command::Options::default(),
        "the harness must not be reading anyone's init.scm"
    );
}
