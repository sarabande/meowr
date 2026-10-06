//! Generate the `motions` case file: every Normal State motion, from every
//! selection type Meow can be in, in situations that distinguish them.
//!
//!     cargo run -p meowr-fidelity --bin cases
//!
//! The cross product is enumerated rather than hand-listed because the point
//! of it is exhaustiveness: `meow-next-thing` branches on whether the previous
//! selection's kind matches and on whether its axis is `expand`, so a motion
//! tested only from a bare point exercises one of its three paths.
//!
//! This writes the *inputs* only. The expected values come from Emacs, via
//! `tools/oracle/regenerate.sh`.

use meowr_fidelity::{Given, Start, render_cases};
use std::path::PathBuf;

/// A buffer and a point in it, chosen so the motions differ from each other.
struct Situation {
    tag: &'static str,
    buffer: &'static str,
    point: usize,
}

/// The Normal State motions that can be oracled, by the key that reaches them.
///
/// Not every motion the slice binds: `f`, `t`, `v`, `/`, `\'` and `L` are out
/// of the oracle's reach, for the six reasons written down further down this
/// file. Leaving them out silently is the thing that comment exists to stop.
const MOTIONS: &[char] = &[
    'h', 'H', 'i', 'I', 'e', 'E', 'n', 'N', 'w', 'W', 'b', 'B', 'm', 'M', 'l', 'o', 'O', 'j', ';',
    'g', 'z',
];

/// The selection types a Normal State key sequence can leave behind, plus no
/// selection at all. `char` comes from `h`/`i`, `line` from `l`, `word` from
/// `m`/`w`, `block` from `o`; the `select` axis appears when a thing motion
/// runs against a *different* previous kind.
const STARTS: &[Option<(&str, &str)>] = &[
    None,
    Some(("select", "char")),
    Some(("expand", "char")),
    Some(("expand", "line")),
    Some(("select", "word")),
    Some(("expand", "word")),
    Some(("expand", "symbol")),
    Some(("expand", "block")),
    Some(("expand", "join")),
];

/// Sequences whose second key is the interesting one: they land on the branch
/// where a previous selection of a *known* kind and axis is already in place,
/// which is where a hand port goes wrong.
const SEQUENCES: &[&str] = &[
    "mw", "mm", "mb", "ww", "wb", "wm", "MW", "WW", "ll", "lm", "oo", "ow", ";;", ";w", "mg", "mz",
    "wz", "wwz", "mzz", "m;w", "w;b", "lz", "jj", "wj", "jw", "oO", "OO", "wO",
    // `O` takes its direction from the block selection it starts from, so a
    // reversed one has to be in here or the backward half is never oracled.
    "o;O", "oO;O", "O;O",
    // `meow-find` and `meow-till`, which had no case at all until #85 gave the
    // oracle a way to feed a `c` interactive spec. Both a hit and a miss for
    // each: a failed search leaves the buffer exactly as a search that ran and
    // moved nowhere does, so the signal column is the only thing between them.
    "fo", "fq", "tl", "tq", "flo", "wfo", ";fo", "ftl",
    // `negative-argument`, which had none either, and whose goldens were wrong
    // before #85: `execute-kbd-macro` cleared the argument between keys, so
    // every one of these replayed as though the `-` had not been typed.
    "-w", "-b", "-l", "-n", "-e", "-fo", "-tl",
    // `repeat`. Ring 1 puts Emacs's own on `\'`, not a Meow command, so what
    // it repeats is whatever `last-repeatable-command` holds — and `meow-line`
    // consults `last-command` to decide whether to extend, so `l l` and `l \'`
    // are not the same question.
    "w\'", "l\'", "m\'", "ll\'", "-w\'", "wm\'", "fo\'",
    // A *fresh* argument on the repeat. Emacs's `repeat` replays the command
    // with the arguments it recorded and ignores the new one, so `f o - \'`
    // searches forward the second time as well.
    "fo-\'", "w-\'",
    // `-l` over a selection `l` already made: `meow-line` negates the count
    // when the selection runs backwards, so the sign flips twice.
    "l-l", "-l-l", "-ll",
];

/// Cases meowr is known not to match, and why.
///
/// Keyed by exact case name so that changing the generator breaks the
/// annotation loudly: a divergence is a claim about one specific behaviour,
/// and a case that has been redefined underneath it needs re-triaging, not
/// carrying forward.
///
/// The harness *requires* these to keep diverging. When one starts matching,
/// the suite fails and the entry comes out.
///
/// Two kinds live here and they are not the same claim. Most are Meow doing
/// something meowr deliberately does not reproduce, with the reason and the
/// ADR that puts it outside Fidelity. The rest — the ones naming a ticket —
/// are **meowr bugs**, parked here rather than hidden so that the suite is
/// green while they wait, and so that fixing one breaks the build until its
/// entry is deleted. A bug entry that has lost its ticket number is the thing
/// to be suspicious of.
const DIVERGENCES: &[(&str, &str)] = &[
    (
        "bof-lyp",
        "Emacs leaves the point one character past the text it yanked, when the kill came from a line selection. Traced rather than guessed: `yank` is entered at 13 and returns at 25, correctly, and something in Meow's post-command path then moves point to 26. Identical under whole-macro replay, so it is Meow's behaviour and not a harness artifact -- but the mechanism is unidentified, and reproducing an offset nobody can explain is the cargo-culting ADR-0002 exists to prevent. ADR-0006 puts a stray point offset outside what Fidelity binds.",
    ),
    (
        "bof-lkp",
        "Emacs leaves the point one character past the text it yanked, when the kill came from a line selection. Traced rather than guessed: `yank` is entered at 13 and returns at 25, correctly, and something in Meow's post-command path then moves point to 26. Identical under whole-macro replay, so it is Meow's behaviour and not a harness artifact -- but the mechanism is unidentified, and reproducing an offset nobody can explain is the cargo-culting ADR-0002 exists to prevent. ADR-0006 puts a stray point offset outside what Fidelity binds.",
    ),
    (
        "midword-lyp",
        "Emacs leaves the point one character past the text it yanked, when the kill came from a line selection. Traced rather than guessed: `yank` is entered at 13 and returns at 25, correctly, and something in Meow's post-command path then moves point to 26. Identical under whole-macro replay, so it is Meow's behaviour and not a harness artifact -- but the mechanism is unidentified, and reproducing an offset nobody can explain is the cargo-culting ADR-0002 exists to prevent. ADR-0006 puts a stray point offset outside what Fidelity binds.",
    ),
    (
        "midword-lkp",
        "Emacs leaves the point one character past the text it yanked, when the kill came from a line selection. Traced rather than guessed: `yank` is entered at 13 and returns at 25, correctly, and something in Meow's post-command path then moves point to 26. Identical under whole-macro replay, so it is Meow's behaviour and not a harness artifact -- but the mechanism is unidentified, and reproducing an offset nobody can explain is the cargo-culting ADR-0002 exists to prevent. ADR-0006 puts a stray point offset outside what Fidelity binds.",
    ),
    (
        "eol-lyp",
        "Emacs leaves the point one character past the text it yanked, when the kill came from a line selection. Traced rather than guessed: `yank` is entered at 13 and returns at 25, correctly, and something in Meow's post-command path then moves point to 26. Identical under whole-macro replay, so it is Meow's behaviour and not a harness artifact -- but the mechanism is unidentified, and reproducing an offset nobody can explain is the cargo-culting ADR-0002 exists to prevent. ADR-0006 puts a stray point offset outside what Fidelity binds.",
    ),
    (
        "eol-lkp",
        "Emacs leaves the point one character past the text it yanked, when the kill came from a line selection. Traced rather than guessed: `yank` is entered at 13 and returns at 25, correctly, and something in Meow's post-command path then moves point to 26. Identical under whole-macro replay, so it is Meow's behaviour and not a harness artifact -- but the mechanism is unidentified, and reproducing an offset nobody can explain is the cargo-culting ADR-0002 exists to prevent. ADR-0006 puts a stray point offset outside what Fidelity binds.",
    ),
    (
        "bof-b2",
        "a digit with no selection. Meow raises \"No selection\": `meow--selection-fallback` looks the fallback up by `this-command`, which is `meow-expand-2`, while `meow-selection-command-fallback`'s key is `meow-expand` -- so the entry pairing it with `meow-digit-argument` never fires. meowr does what that entry says and treats the digit as a numeric argument, which is what the table was written to mean.",
    ),
    (
        "bof-bsemi2",
        "a digit with no selection. Meow raises \"No selection\": `meow--selection-fallback` looks the fallback up by `this-command`, which is `meow-expand-2`, while `meow-selection-command-fallback`'s key is `meow-expand` -- so the entry pairing it with `meow-digit-argument` never fires. meowr does what that entry says and treats the digit as a numeric argument, which is what the table was written to mean.",
    ),
    (
        "bof-e2",
        "a digit with no selection. Meow raises \"No selection\": `meow--selection-fallback` looks the fallback up by `this-command`, which is `meow-expand-2`, while `meow-selection-command-fallback`'s key is `meow-expand` -- so the entry pairing it with `meow-digit-argument` never fires. meowr does what that entry says and treats the digit as a numeric argument, which is what the table was written to mean.",
    ),
    (
        "bof-h2",
        "a digit with no selection. Meow raises \"No selection\": `meow--selection-fallback` looks the fallback up by `this-command`, which is `meow-expand-2`, while `meow-selection-command-fallback`'s key is `meow-expand` -- so the entry pairing it with `meow-digit-argument` never fires. meowr does what that entry says and treats the digit as a numeric argument, which is what the table was written to mean.",
    ),
    (
        "bof-i2",
        "a digit with no selection. Meow raises \"No selection\": `meow--selection-fallback` looks the fallback up by `this-command`, which is `meow-expand-2`, while `meow-selection-command-fallback`'s key is `meow-expand` -- so the entry pairing it with `meow-digit-argument` never fires. meowr does what that entry says and treats the digit as a numeric argument, which is what the table was written to mean.",
    ),
    (
        "bof-n2",
        "a digit with no selection. Meow raises \"No selection\": `meow--selection-fallback` looks the fallback up by `this-command`, which is `meow-expand-2`, while `meow-selection-command-fallback`'s key is `meow-expand` -- so the entry pairing it with `meow-digit-argument` never fires. meowr does what that entry says and treats the digit as a numeric argument, which is what the table was written to mean.",
    ),
    (
        "bof-shiftb2",
        "a digit with no selection. Meow raises \"No selection\": `meow--selection-fallback` looks the fallback up by `this-command`, which is `meow-expand-2`, while `meow-selection-command-fallback`'s key is `meow-expand` -- so the entry pairing it with `meow-digit-argument` never fires. meowr does what that entry says and treats the digit as a numeric argument, which is what the table was written to mean.",
    ),
    (
        "midword-e2",
        "a digit with no selection. Meow raises \"No selection\": `meow--selection-fallback` looks the fallback up by `this-command`, which is `meow-expand-2`, while `meow-selection-command-fallback`'s key is `meow-expand` -- so the entry pairing it with `meow-digit-argument` never fires. meowr does what that entry says and treats the digit as a numeric argument, which is what the table was written to mean.",
    ),
    (
        "midword-h2",
        "a digit with no selection. Meow raises \"No selection\": `meow--selection-fallback` looks the fallback up by `this-command`, which is `meow-expand-2`, while `meow-selection-command-fallback`'s key is `meow-expand` -- so the entry pairing it with `meow-digit-argument` never fires. meowr does what that entry says and treats the digit as a numeric argument, which is what the table was written to mean.",
    ),
    (
        "midword-i2",
        "a digit with no selection. Meow raises \"No selection\": `meow--selection-fallback` looks the fallback up by `this-command`, which is `meow-expand-2`, while `meow-selection-command-fallback`'s key is `meow-expand` -- so the entry pairing it with `meow-digit-argument` never fires. meowr does what that entry says and treats the digit as a numeric argument, which is what the table was written to mean.",
    ),
    (
        "midword-n2",
        "a digit with no selection. Meow raises \"No selection\": `meow--selection-fallback` looks the fallback up by `this-command`, which is `meow-expand-2`, while `meow-selection-command-fallback`'s key is `meow-expand` -- so the entry pairing it with `meow-digit-argument` never fires. meowr does what that entry says and treats the digit as a numeric argument, which is what the table was written to mean.",
    ),
    (
        "symbol-e2",
        "a digit with no selection. Meow raises \"No selection\": `meow--selection-fallback` looks the fallback up by `this-command`, which is `meow-expand-2`, while `meow-selection-command-fallback`'s key is `meow-expand` -- so the entry pairing it with `meow-digit-argument` never fires. meowr does what that entry says and treats the digit as a numeric argument, which is what the table was written to mean.",
    ),
    (
        "symbol-h2",
        "a digit with no selection. Meow raises \"No selection\": `meow--selection-fallback` looks the fallback up by `this-command`, which is `meow-expand-2`, while `meow-selection-command-fallback`'s key is `meow-expand` -- so the entry pairing it with `meow-digit-argument` never fires. meowr does what that entry says and treats the digit as a numeric argument, which is what the table was written to mean.",
    ),
    (
        "symbol-i2",
        "a digit with no selection. Meow raises \"No selection\": `meow--selection-fallback` looks the fallback up by `this-command`, which is `meow-expand-2`, while `meow-selection-command-fallback`'s key is `meow-expand` -- so the entry pairing it with `meow-digit-argument` never fires. meowr does what that entry says and treats the digit as a numeric argument, which is what the table was written to mean.",
    ),
    (
        "symbol-n2",
        "a digit with no selection. Meow raises \"No selection\": `meow--selection-fallback` looks the fallback up by `this-command`, which is `meow-expand-2`, while `meow-selection-command-fallback`'s key is `meow-expand` -- so the entry pairing it with `meow-digit-argument` never fires. meowr does what that entry says and treats the digit as a numeric argument, which is what the table was written to mean.",
    ),
    (
        "bof-o2",
        "a digit with no selection, and nothing to do with `meow-block` any more. This buffer holds no brackets at all, so `o` finds no block in Emacs either and the two agree about it; what is left is the digit, which Meow then refuses for the reason recorded against `bof-b2`.",
    ),
    (
        "midword-o2",
        "as bof-o2: no brackets in the buffer, so `o` correctly finds nothing in both, and the divergence that remains is the digit's.",
    ),
    // The `match` Thing: sixteen, one per situation with a bracket around the
    // point times the four Thing prefixes.
    (
        "nested-innerm",
        "`m` is meowr's own `match` Thing and has no row in `meow-char-thing-table`, so Meow does nothing at all: `meow--parse-inner-of-thing-char` is a `when-let*` over that table and its callers are a bare `(when bounds ...)`. meowr selects the innermost bracket pair around the point. ADR-0008 measures Fidelity on meowr's defaults, and this is a default Meow leaves unbound -- the difference is the feature, not a failure. The `string` and `prose` situations have no bracket around the point, so meowr does nothing there either and they are absent from this table.",
    ),
    (
        "nested-dotm",
        "`m` is meowr's own `match` Thing and has no row in `meow-char-thing-table`, so Meow does nothing at all: `meow--parse-inner-of-thing-char` is a `when-let*` over that table and its callers are a bare `(when bounds ...)`. meowr selects the innermost bracket pair around the point. ADR-0008 measures Fidelity on meowr's defaults, and this is a default Meow leaves unbound -- the difference is the feature, not a failure. The `string` and `prose` situations have no bracket around the point, so meowr does nothing there either and they are absent from this table.",
    ),
    (
        "nested-begm",
        "`m` is meowr's own `match` Thing and has no row in `meow-char-thing-table`, so Meow does nothing at all: `meow--parse-inner-of-thing-char` is a `when-let*` over that table and its callers are a bare `(when bounds ...)`. meowr selects the innermost bracket pair around the point. ADR-0008 measures Fidelity on meowr's defaults, and this is a default Meow leaves unbound -- the difference is the feature, not a failure. The `string` and `prose` situations have no bracket around the point, so meowr does nothing there either and they are absent from this table.",
    ),
    (
        "nested-endm",
        "`m` is meowr's own `match` Thing and has no row in `meow-char-thing-table`, so Meow does nothing at all: `meow--parse-inner-of-thing-char` is a `when-let*` over that table and its callers are a bare `(when bounds ...)`. meowr selects the innermost bracket pair around the point. ADR-0008 measures Fidelity on meowr's defaults, and this is a default Meow leaves unbound -- the difference is the feature, not a failure. The `string` and `prose` situations have no bracket around the point, so meowr does nothing there either and they are absent from this table.",
    ),
    (
        "afterpair-innerm",
        "`m` is meowr's own `match` Thing and has no row in `meow-char-thing-table`, so Meow does nothing at all: `meow--parse-inner-of-thing-char` is a `when-let*` over that table and its callers are a bare `(when bounds ...)`. meowr selects the innermost bracket pair around the point. ADR-0008 measures Fidelity on meowr's defaults, and this is a default Meow leaves unbound -- the difference is the feature, not a failure. The `string` and `prose` situations have no bracket around the point, so meowr does nothing there either and they are absent from this table.",
    ),
    (
        "afterpair-dotm",
        "`m` is meowr's own `match` Thing and has no row in `meow-char-thing-table`, so Meow does nothing at all: `meow--parse-inner-of-thing-char` is a `when-let*` over that table and its callers are a bare `(when bounds ...)`. meowr selects the innermost bracket pair around the point. ADR-0008 measures Fidelity on meowr's defaults, and this is a default Meow leaves unbound -- the difference is the feature, not a failure. The `string` and `prose` situations have no bracket around the point, so meowr does nothing there either and they are absent from this table.",
    ),
    (
        "afterpair-begm",
        "`m` is meowr's own `match` Thing and has no row in `meow-char-thing-table`, so Meow does nothing at all: `meow--parse-inner-of-thing-char` is a `when-let*` over that table and its callers are a bare `(when bounds ...)`. meowr selects the innermost bracket pair around the point. ADR-0008 measures Fidelity on meowr's defaults, and this is a default Meow leaves unbound -- the difference is the feature, not a failure. The `string` and `prose` situations have no bracket around the point, so meowr does nothing there either and they are absent from this table.",
    ),
    (
        "afterpair-endm",
        "`m` is meowr's own `match` Thing and has no row in `meow-char-thing-table`, so Meow does nothing at all: `meow--parse-inner-of-thing-char` is a `when-let*` over that table and its callers are a bare `(when bounds ...)`. meowr selects the innermost bracket pair around the point. ADR-0008 measures Fidelity on meowr's defaults, and this is a default Meow leaves unbound -- the difference is the feature, not a failure. The `string` and `prose` situations have no bracket around the point, so meowr does nothing there either and they are absent from this table.",
    ),
    (
        "stringpair-innerm",
        "`m` is meowr's own `match` Thing and has no row in `meow-char-thing-table`, so Meow does nothing at all: `meow--parse-inner-of-thing-char` is a `when-let*` over that table and its callers are a bare `(when bounds ...)`. meowr selects the innermost bracket pair around the point. ADR-0008 measures Fidelity on meowr's defaults, and this is a default Meow leaves unbound -- the difference is the feature, not a failure. The `string` and `prose` situations have no bracket around the point, so meowr does nothing there either and they are absent from this table.",
    ),
    (
        "stringpair-dotm",
        "`m` is meowr's own `match` Thing and has no row in `meow-char-thing-table`, so Meow does nothing at all: `meow--parse-inner-of-thing-char` is a `when-let*` over that table and its callers are a bare `(when bounds ...)`. meowr selects the innermost bracket pair around the point. ADR-0008 measures Fidelity on meowr's defaults, and this is a default Meow leaves unbound -- the difference is the feature, not a failure. The `string` and `prose` situations have no bracket around the point, so meowr does nothing there either and they are absent from this table.",
    ),
    (
        "stringpair-begm",
        "`m` is meowr's own `match` Thing and has no row in `meow-char-thing-table`, so Meow does nothing at all: `meow--parse-inner-of-thing-char` is a `when-let*` over that table and its callers are a bare `(when bounds ...)`. meowr selects the innermost bracket pair around the point. ADR-0008 measures Fidelity on meowr's defaults, and this is a default Meow leaves unbound -- the difference is the feature, not a failure. The `string` and `prose` situations have no bracket around the point, so meowr does nothing there either and they are absent from this table.",
    ),
    (
        "stringpair-endm",
        "`m` is meowr's own `match` Thing and has no row in `meow-char-thing-table`, so Meow does nothing at all: `meow--parse-inner-of-thing-char` is a `when-let*` over that table and its callers are a bare `(when bounds ...)`. meowr selects the innermost bracket pair around the point. ADR-0008 measures Fidelity on meowr's defaults, and this is a default Meow leaves unbound -- the difference is the feature, not a failure. The `string` and `prose` situations have no bracket around the point, so meowr does nothing there either and they are absent from this table.",
    ),
    (
        "indent-innerm",
        "`m` is meowr's own `match` Thing and has no row in `meow-char-thing-table`, so Meow does nothing at all: `meow--parse-inner-of-thing-char` is a `when-let*` over that table and its callers are a bare `(when bounds ...)`. meowr selects the innermost bracket pair around the point. ADR-0008 measures Fidelity on meowr's defaults, and this is a default Meow leaves unbound -- the difference is the feature, not a failure. The `string` and `prose` situations have no bracket around the point, so meowr does nothing there either and they are absent from this table.",
    ),
    (
        "indent-dotm",
        "`m` is meowr's own `match` Thing and has no row in `meow-char-thing-table`, so Meow does nothing at all: `meow--parse-inner-of-thing-char` is a `when-let*` over that table and its callers are a bare `(when bounds ...)`. meowr selects the innermost bracket pair around the point. ADR-0008 measures Fidelity on meowr's defaults, and this is a default Meow leaves unbound -- the difference is the feature, not a failure. The `string` and `prose` situations have no bracket around the point, so meowr does nothing there either and they are absent from this table.",
    ),
    (
        "indent-begm",
        "`m` is meowr's own `match` Thing and has no row in `meow-char-thing-table`, so Meow does nothing at all: `meow--parse-inner-of-thing-char` is a `when-let*` over that table and its callers are a bare `(when bounds ...)`. meowr selects the innermost bracket pair around the point. ADR-0008 measures Fidelity on meowr's defaults, and this is a default Meow leaves unbound -- the difference is the feature, not a failure. The `string` and `prose` situations have no bracket around the point, so meowr does nothing there either and they are absent from this table.",
    ),
    (
        "indent-endm",
        "`m` is meowr's own `match` Thing and has no row in `meow-char-thing-table`, so Meow does nothing at all: `meow--parse-inner-of-thing-char` is a `when-let*` over that table and its callers are a bare `(when bounds ...)`. meowr selects the innermost bracket pair around the point. ADR-0008 measures Fidelity on meowr's defaults, and this is a default Meow leaves unbound -- the difference is the feature, not a failure. The `string` and `prose` situations have no bracket around the point, so meowr does nothing there either and they are absent from this table.",
    ),
];

fn main() {
    let suites = [
        ("motions", motions()),
        ("expand", expand()),
        ("editing", editing()),
        ("things", things()),
        ("grab", grab()),
    ];
    // A divergence naming no case in any suite is a stale claim about
    // behaviour, which is exactly what this table exists to stop.
    let all: Vec<String> = suites
        .iter()
        .flat_map(|(_, cases)| cases.iter().map(|c| c.name.clone()))
        .collect();
    for (name, _) in DIVERGENCES {
        assert!(
            all.iter().any(|n| n == name),
            "divergence names `{name}`, which is not a case in any suite"
        );
    }
    for (suite, cases) in suites {
        write(suite, cases);
    }
}

fn write(suite: &str, mut cases: Vec<Given>) {
    for (name, reason) in DIVERGENCES {
        if let Some(case) = cases.iter_mut().find(|c| c.name == *name) {
            case.diverges = Some((*reason).to_string());
        }
    }
    let path =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!("../../fidelity/{suite}.cases"));
    std::fs::write(&path, render_cases(&cases)).expect("write cases");
    eprintln!("{:>4} cases -> fidelity/{suite}.cases", cases.len());
}

/// Sequences for the expand slice. Every one builds its own selection first,
/// because what a digit does depends entirely on what made the selection —
/// `meow--select-expandable-p` admits only `word symbol line block find till`,
/// and `meow-mark-thing` clears the stashed navigation pair through
/// `meow--highlight-regexp-in-buffer`.
fn expand() -> Vec<Given> {
    const SEQUENCES: &[&str] = &[
        // A motion that stashes a navigation pair, then a digit.
        "w1", "w2", "w3", "w0", "W2", "b2", "B2", "l1", "l2", "l0", "o2",
        // `m` stashes nothing: mark-thing clears the pair.
        "m2", "M2", // Direction picks which half of the pair is called.
        "w;2", "b;2", // Expanding twice, and expanding what an expand produced.
        "w22", "w2w", "w2;2", "l22",
        // The plain motions leave no selection, so these are digits with
        // nothing to expand. Meow *errors* here rather than treating them as
        // numeric arguments -- see the divergence table.
        "i2", "h2", "n2", "e2",
    ];
    // Numeric and negative arguments are deliberately absent, and cannot be
    // added. Neither way of replaying keys in batch Emacs can carry a prefix
    // argument, and both failure modes were verified rather than assumed:
    //
    //   - `execute-kbd-macro`, one key at a time, is what the rest of the
    //     harness uses because it runs the real command loop. But the
    //     `prefix-arg` -> `current-prefix-arg` handoff does not survive
    //     between two invocations, so `- w` reaches `meow-next-word` with no
    //     argument and moves *forward*.
    //   - `command-execute` with the argument passed explicitly does carry it,
    //     but skips `post-command-hook`, which is where Meow stashes the
    //     expand navigation pair. Under it, `w 2` stops expanding at all.
    //
    // So a golden for `2 w` would record the behaviour of the harness rather
    // than the behaviour of Meow. Numeric arguments are covered by ordinary
    // tests written from the elisp instead, and this comment exists so nobody
    // "fixes" the gap by adding cases that would quietly lie.
    let situations = [
        ("bof", "hello world\nsecond line\nthird\n", 0usize),
        ("midword", "hello world\nsecond line\nthird\n", 8),
        ("symbol", "foo_bar baz\n\nqux(arg)\n", 4),
    ];
    let mut cases = Vec::new();
    for (tag, buffer, point) in situations {
        for seq in SEQUENCES {
            cases.push(Given {
                name: format!("{tag}-{}", seq.chars().map(named).collect::<String>()),
                buffer: buffer.to_string(),
                point,
                start: None,
                keys: seq.chars().collect(),
                diverges: None,
            });
        }
    }
    cases
}

/// The editing slice. Meow implements none of these itself — it computes a
/// selection and presses an Emacs key — so what is pinned here is the
/// observable result, which is the only thing Fidelity binds anyway.
/// The Grab: `meow-grab` from every kind of selection, and what happens to it
/// afterwards.
///
/// `G` is the only Grab command Meow binds in Normal State — `meow-swap-grab`
/// and `meow-sync-grab` are remapped in Beacon State and have no Normal key,
/// so meowr's `C-x g` and `C-x y` are its own placement until Ring 2 and are
/// not oracled. What *is* oracled is the thing that matters: `meow-grab` calls
/// `secondary-selection-from-region` and then cancels the selection, so a Grab
/// is an overlay the other columns cannot see, and the `grab` column exists to
/// see it.
fn grab() -> Vec<Given> {
    const SEQUENCES: &[&str] = &[
        // From each kind of selection, and from none — with no region,
        // `meow-grab` cancels the Grab instead of making one.
        "G", "wG", "WG", "bG", "mG", "MG", "lG", "oG", ";wG",
        // Grabbing again, over and clearing.
        "wGG", "wGwG", "wGlG", "wGmG",
        // The Grab is not the selection: moving afterwards must leave it.
        "wGw", "wGb", "wGl", "wGm", "wGg", "wGz", "wG;",
        // Edits under a Grab. The overlay has to move with the text, which is
        // the half of this a point-and-mark column could never check.
        "wGx", "wGk", "wGy", "wGp", "wGc", "lGx", "lGk",
        // And an edit *before* it in the buffer.
        "wwGiiix", "wwGhhhx",
    ];
    let situations = [
        ("bof", "hello world\nsecond line\nthird\n", 0usize),
        ("midword", "hello world\nsecond line\nthird\n", 8),
        ("eol", "hello world\nsecond line\nthird\n", 11),
    ];
    let mut cases = Vec::new();
    for (tag, buffer, point) in situations {
        for seq in SEQUENCES {
            cases.push(Given {
                name: format!("{tag}-{}", seq.chars().map(named).collect::<String>()),
                buffer: buffer.to_string(),
                point,
                start: None,
                keys: seq.chars().collect(),
                diverges: None,
            });
        }
    }
    cases
}

fn editing() -> Vec<Given> {
    const SEQUENCES: &[&str] = &[
        // Entering Insert State in its four positions, and typing there.
        "s", "a", "A", "S", "sZ", "aZ", "AZ", "SZ", "wsZ", "waZ",
        // Kill, and the line special case: `meow--prepare-region-for-kill`
        // pulls in the newline a forward line selection stops short of.
        "wk", "lk", "llk", "mk", "wwk", "b;k", // Copy, then paste it back.
        "wy", "wyp", "wywyp", "lyp",
        // Kill then yank is the move: the ring is what carries it.
        "wkp", "wkip", "lkp",
        // Yank-pop cycles the ring, so it needs two things in it.
        "wywwyp",
        // `P` is absent, and cannot be added. Since Emacs 28 `yank-pop` falls
        // through to `yank-from-kill-ring` — an interactive prompt — unless
        // `last-command` is `yank`, and per-key replay loses `last-command`
        // between invocations exactly as it loses `prefix-arg`. In batch that
        // prompt returns the empty string, so the golden would record a case
        // where Meow pushed "" onto the kill ring and moved nothing. Verified
        // by tracing the ring: `("" "\nsecond" "hello")`. Yank-pop is covered
        // by ordinary tests instead.
        // Replace the selection with the ring's head.
        "wywwr", "lywwr", "wr", // Change: kill and enter Insert.
        "wc", "wcZ", "lc", "c",
        // The plain character deletes, which are not selection commands at all.
        "x", "X", "wx", "wX", "iX", "xx",
        // With no selection, these go through the fallback table.
        "k", "y", "p",
    ];
    let situations = [
        ("bof", "hello world\nsecond line\nthird\n", 0usize),
        ("midword", "hello world\nsecond line\nthird\n", 8),
        ("eol", "hello world\nsecond line\nthird\n", 11),
    ];
    let mut cases = Vec::new();
    for (tag, buffer, point) in situations {
        for seq in SEQUENCES {
            cases.push(Given {
                name: format!("{tag}-{}", seq.chars().map(named).collect::<String>()),
                buffer: buffer.to_string(),
                point,
                start: None,
                keys: seq.chars().collect(),
                diverges: None,
            });
        }
    }
    cases
}

/// The Thing slice: `,` `.` `[` `]` over the lexical Things.
fn things() -> Vec<Given> {
    // The six tree-sitter Things — `a f t y , /` for class, function, test,
    // entry, parameter and comment — are deliberately not here.
    //
    // They come from `meow-tree-sitter`, a separate package, and its answers
    // are produced by *its* queries running under Emacs's `treesit`. meowr's
    // come from queries vendored from Helix running under tree-house. A golden
    // would therefore compare two query sets, not two implementations of Meow,
    // and every difference it found would be a difference between the query
    // files rather than a fidelity failure. ADR-0006 already says Fidelity
    // binds the calculus and not the boundary a query found — and the calculus
    // *is* covered here, by the lexical Things, since `,` `.` `[` `]` build
    // their selections the same way whichever registry answered.
    //
    // Covered by ordinary tests in `meowr-syntax` instead, in two languages.
    //
    // The keys `meow-char-thing-table` gives the lexical Things. Three are
    // absent: `window` needs the viewport, `defun` is tree-sitter's
    // (ADR-0006), and `visual-line` depends on how wide the window is —
    // `meow--inner-of-visual-line` calls `meow--visual-line-beginning-position`,
    // which in a batch Emacs with no window answers point-min. A golden for it
    // would record the absence of a window rather than the behaviour of Meow.
    // `m` is meowr's own `match` Thing and has no row in
    // `meow-char-thing-table`, so `meow--parse-inner-of-thing-char` -- a
    // `when-let*` over that table, whose callers are a bare `(when bounds
    // ...)` -- makes `, m` a silent no-op in Emacs. That is a *replayable*
    // answer rather than an absent one, so the cases belong here and the
    // difference is recorded in `DIVERGENCES` where it can never heal
    // quietly. Where the situation has no bracket around the point, meowr
    // does nothing either and the two agree; only the four situations with a
    // pair carry an entry.
    const THINGS: &[char] = &['r', 's', 'c', 'g', 'e', 'b', 'p', 'l', '.', 'm'];
    const PREFIXES: &[char] = &[',', '.', '[', ']'];
    let situations = [
        ("nested", "outer (alpha (beta) gamma) tail\n", 14usize),
        ("afterpair", "outer (alpha (beta) gamma) tail\n", 21),
        ("string", "say \"one two\" and \"three\" end\n", 7),
        // A bracket inside a string, which `meow-block` skips and the pair
        // Things do not. `meow--thing-parse-pair-search` is a bare
        // `re-search-forward` over the literal token with no `syntax-ppss` and
        // no `meow--in-string-p` anywhere near it, so `, r` counts the `)` in
        // the string that `o` ignores. Two matchers in one editor with two
        // different rules -- Meow's, and therefore meowr's. Pinned here so
        // that it is proved rather than asserted, and so that nobody
        // "reconciles" them.
        ("stringpair", "call(a, \")\", b) end\n", 6),
        (
            "prose",
            "One thing. Then another.\n\nA second para here.\n",
            14,
        ),
        ("indent", "fn main() {\n    let x = 1;\n}\n", 20),
    ];
    let mut cases = Vec::new();
    for (tag, buffer, point) in situations {
        for &prefix in PREFIXES {
            for &thing in THINGS {
                cases.push(Given {
                    name: format!("{tag}-{}{}", named(prefix), named(thing)),
                    buffer: buffer.to_string(),
                    point,
                    start: None,
                    keys: vec![prefix, thing],
                    diverges: None,
                });
            }
        }
    }
    cases
}

// `f`, `t`, `v` and `/` are deliberately not oracled, and this comment is
// where the sixth oracle limitation is written down.
//
// `meow-find` is `(interactive "p\ncFind:")`. The `c` spec reads its character
// **in C**, inside `Fcall_interactively`, below the elisp `read-char` that the
// oracle's queue can serve — which is why the Thing commands work (they call
// `read-char` explicitly, from `meow-thing-prompt`) and these hang the run
// instead. Feeding the character by replaying the whole sequence as one macro
// would work, but costs the per-key error isolation the rest of the harness
// depends on.
//
// `v` and `/` are further out of reach again: both go through the minibuffer
// with a completing-read, which a batch Emacs cannot answer at all.
//
// `\'` and `L` are absent for the same reason and are written down here too.
// `\'` is Emacs's own `repeat`, which reads `last-repeatable-command`, so
// per-key replay leaves it with nothing to repeat and a golden for `w \'` would
// record it doing nothing and call that agreement. `L` is `meow-goto-line`,
// which runs `goto-line` and reads its number from the minibuffer — the `v`
// and `/` case again. meowr takes that number from the numeric argument
// instead, which is a divergence and not a gap; `fidelity/README.md` says so.
//
// Beacon State is the seventh, and it is absolute: `meow--maybe-toggle-beacon-state`
// opens with `(unless (or defining-kbd-macro executing-kbd-macro) ...)`, and the
// oracle replays every key through `execute-kbd-macro`. Emacs therefore *never*
// enters Beacon State under the oracle, whatever the keys. There is no beacon
// suite for that reason and not for want of trying to write one — a golden for
// it would record Meow staying in Normal and call that the answer. meowr honours
// the same guard through `Session::replay`, which is what the harness drives it
// with, so the two agree about the State for the right reason rather than by
// accident. Beacon State is covered by ordinary tests in `meowr-app`.
//
// The numeric argument is the eighth, and it cuts across all of them:
// the oracle resets `prefix-arg` between keys, so `3 w`, `- w` and `- f x`
// reach Emacs here as `w`, `w` and `f x`. Every motion that reads an argument
// — and `repeat`, which restores the one the last command had — is therefore
// checked by ordinary tests written from the elisp and not by a golden.
//
// All six are covered by ordinary tests in `meowr-app`.

fn motions() -> Vec<Given> {
    let situations = [
        Situation {
            tag: "bof",
            buffer: "hello world\nsecond line\nthird\n",
            point: 0,
        },
        Situation {
            tag: "midword",
            buffer: "hello world\nsecond line\nthird\n",
            point: 8,
        },
        Situation {
            tag: "bol2",
            buffer: "hello world\nsecond line\nthird\n",
            point: 12,
        },
        Situation {
            tag: "symbol",
            buffer: "foo_bar baz\n\nqux(arg)\n",
            point: 4,
        },
        Situation {
            tag: "punct",
            buffer: "foo_bar baz\n\nqux(arg)\n",
            point: 17,
        },
        // An empty line. `meow-line`'s backward branch has a
        // `(when (meow--empty-line-p) (backward-char 1))` that no other
        // situation reaches, so without this the branch is unoracled.
        Situation {
            tag: "blankline",
            buffer: "foo_bar baz\n\nqux(arg)\n",
            point: 12,
        },
        // The two buffers that reach `meow-to-block`'s balance check -- the
        // `(ignore-errors (forward-list ...))` whose result the selection
        // never uses but whose *success* it requires. Found by replaying that
        // function's own body over every point of sixteen buffers in a batch
        // Emacs: nine points reach it, and all nine are a delimiter inside a
        // comment or a string.
        //
        // A close delimiter in a comment stops Emacs's scan, because the loop
        // skips `meow--in-string-p` and nothing else; the depth test then
        // passes and only the balance check refuses.
        Situation {
            tag: "commentclose",
            buffer: ";; ) comment\n(a)\n",
            point: 0,
        },
        // An *open* delimiter in a string is skipped by the loop, which leaves
        // the point on it when the next search fails -- again past the depth
        // test, again caught only by the balance check. Backward, which is
        // where `O` goes from a reversed block selection.
        Situation {
            tag: "stringopen",
            buffer: "let s = \"(\"; f(x)\n",
            point: 14,
        },
    ];

    let mut cases = Vec::new();
    for s in &situations {
        // A mark far enough from the point that the selection spans something,
        // and inside the buffer whichever side it falls on.
        let mark = if s.point >= 3 {
            s.point - 3
        } else {
            s.point + 3
        };
        for start in STARTS {
            let start = start.map(|(axis, kind)| Start {
                axis: axis.to_string(),
                kind: kind.to_string(),
                mark,
            });
            let tag = match &start {
                None => "none".to_string(),
                Some(st) => format!("{}.{}", st.axis, st.kind),
            };
            for &key in MOTIONS {
                cases.push(Given {
                    name: format!("{}-{}-{}", s.tag, tag, named(key)),
                    buffer: s.buffer.to_string(),
                    point: s.point,
                    start: start.clone(),
                    keys: vec![key],
                    diverges: None,
                });
            }
            // Sequences only from a bare point: their whole purpose is to
            // build the previous selection themselves, so seeding one as well
            // would multiply cases without adding a path.
            if start.is_none() {
                for seq in SEQUENCES {
                    cases.push(Given {
                        name: format!(
                            "{}-seq-{}",
                            s.tag,
                            seq.chars().map(named).collect::<String>()
                        ),
                        buffer: s.buffer.to_string(),
                        point: s.point,
                        start: None,
                        keys: seq.chars().collect(),
                        diverges: None,
                    });
                }
            }
        }
    }

    cases
}

/// A case name has to survive being a filename fragment and being read aloud
/// in a failure, so `;` and capitals get spelled out.
fn named(key: char) -> String {
    match key {
        ';' => "semi".to_string(),
        '-' => "neg".to_string(),
        ',' => "inner".to_string(),
        ' ' => "space".to_string(),
        '.' => "dot".to_string(),
        '[' => "beg".to_string(),
        ']' => "end".to_string(),
        // `repeat`. A case name is a filename fragment, and an apostrophe in
        // one is a shell quoting accident waiting to happen.
        '\'' => "rep".to_string(),
        c if c.is_uppercase() => format!("shift{}", c.to_lowercase()),
        c => c.to_string(),
    }
}
