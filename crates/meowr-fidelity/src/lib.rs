//! The differential test harness: meowr's behaviour against the real Meow
//! package, running in a real Emacs.
//!
//! Fidelity is the premise of this project, and it is not defensible by
//! reading elisp carefully. Meow branches on the previous selection's type, on
//! the selection's direction, and on a history stack — exactly the class of
//! behaviour a hand port gets wrong silently. So the expected values here are
//! not written by hand and not reasoned out: they are produced by running
//! Emacs and recording what it did.
//!
//! # The three files
//!
//! - `fidelity/*.cases` — the *inputs*. A starting buffer, a starting point, an
//!   optional starting selection, and a key sequence. Cheap to write by hand
//!   and cheap to generate.
//! - `fidelity/*.golden` — the *observations*, one line per case, written by
//!   `tools/oracle/oracle.el`. Checked in.
//! - `tools/oracle/oracle.el` — the oracle. Reads a `.cases`, replays every key
//!   through Meow with `execute-kbd-macro`, writes the `.golden`.
//!
//! Because the goldens are checked in, **CI does not need Emacs**. Emacs is
//! needed only to regenerate, which is a deliberate act by a human who is about
//! to review the resulting diff.
//!
//! # Why two TSV files rather than one format with expectations inline
//!
//! Keeping the inputs and the observations in separate files means
//! regeneration rewrites exactly one file, so `git diff` after a Meow bump
//! shows only what Emacs's behaviour changed — nothing else moves. It also
//! keeps the elisp side to a `split-string` and the Rust side to a `split('\t')`.
//!
//! # Recording a deliberate divergence
//!
//! ADR-0005 already guarantees there will be places meowr does not match
//! Emacs. A case may carry a `diverges` reason, which *inverts* the assertion:
//! meowr is then required to differ from the golden, and the test fails if it
//! starts matching. A divergence that quietly heals is a divergence whose
//! annotation is now a lie, and the harness says so rather than staying green.

use std::collections::BTreeMap;
use std::fmt::Write as _;

/// A selection to start a case from, in the same shape Meow stores one:
/// `(TYPE MARK POS)`, where POS is the case's `point`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Start {
    pub axis: String,
    pub kind: String,
    pub mark: usize,
}

/// One case's inputs. Carries no expectation: those live in the golden.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Given {
    pub name: String,
    pub buffer: String,
    pub point: usize,
    pub start: Option<Start>,
    /// One key per element. Single characters for now; slice 05 brings counts
    /// and slice 15 brings prefixes, at which point this becomes a key parser.
    pub keys: Vec<char>,
    /// `Some(reason)` if meowr is *expected* to differ from Emacs here.
    pub diverges: Option<String>,
}

/// What is compared. Everything Meow makes observable after a key sequence and
/// nothing it does not: no kill ring and no `last-command` yet, because no
/// command in this slice reads them. Extending the tuple is a deliberate act,
/// since it invalidates every golden.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Observation {
    pub text: String,
    /// Byte offset, 0-based. Emacs positions are 1-based character positions,
    /// so the oracle converts; the two agree only on ASCII and the harness is
    /// meant to be run on more than ASCII.
    pub point: usize,
    /// `None` when no selection is active — in Emacs, when the region is not
    /// active, which is the same condition under which `meow--selection-type`
    /// returns nil.
    pub mark: Option<usize>,
    pub axis: Option<String>,
    pub kind: Option<String>,
    /// Depth of `meow--selection-history`.
    pub history: usize,
    /// How many of the replayed keys raised an error in Emacs.
    ///
    /// Emacs's command loop swallows a signal -- it beeps and moves on -- so
    /// this is invisible in the buffer state, which is exactly the problem.
    /// A case whose keys all failed looks identical to a case where Meow
    /// deliberately did nothing. Recording it is what turned "the prefix
    /// argument seems to be ignored" into "the digit key errored and never
    /// ran at all".
    pub signals: usize,
    /// The Meow State the buffer is left in. `s`, `a`, `A` and `S` differ from
    /// each other only in where they leave the point, and from everything else
    /// only in this — without it, an editing suite cannot see them at all.
    pub state: String,
    /// The Grab, as a byte range, or `None` when there is none.
    ///
    /// Meow's Grab *is* Emacs's secondary selection, and it lives in an
    /// overlay rather than in point and mark — so no other column can see it.
    /// Without this one a Grab suite would record agreement it never checked.
    pub grab: Option<(usize, usize)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseError {
    pub line: usize,
    pub message: String,
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "line {}: {}", self.line, self.message)
    }
}

impl std::error::Error for ParseError {}

const CASES_HEADER: &str = "# name\tbuffer\tpoint\taxis\tkind\tmark\tkeys\tdiverges";
const GOLDEN_HEADER: &str = "# name\ttext\tpoint\tmark\taxis\tkind\thistory\tsignals\tstate\tgrab";

/// Encode a field so it survives a tab-separated line.
///
/// Only these four need escaping, and every one of them maps to a single
/// character, so `unescape` is total on anything `escape` produced.
pub fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            c => out.push(c),
        }
    }
    out
}

pub fn unescape(s: &str) -> Result<String, String> {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('\\') => out.push('\\'),
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('r') => out.push('\r'),
            Some(other) => return Err(format!("unknown escape `\\{other}`")),
            None => return Err("trailing backslash".to_string()),
        }
    }
    Ok(out)
}

fn optional(field: &str) -> Option<&str> {
    if field == "-" { None } else { Some(field) }
}

fn fields(line: &str, want: usize, no: usize) -> Result<Vec<&str>, ParseError> {
    let got: Vec<&str> = line.split('\t').collect();
    if got.len() != want {
        return Err(ParseError {
            line: no,
            message: format!("expected {want} tab-separated fields, found {}", got.len()),
        });
    }
    Ok(got)
}

fn number(field: &str, what: &str, no: usize) -> Result<usize, ParseError> {
    field.parse().map_err(|_| ParseError {
        line: no,
        message: format!("{what}: `{field}` is not a number"),
    })
}

/// Lines that carry data: not blank, not a comment.
fn significant(text: &str) -> impl Iterator<Item = (usize, &str)> {
    text.lines()
        .enumerate()
        .map(|(i, l)| (i + 1, l))
        .filter(|(_, l)| !l.trim().is_empty() && !l.starts_with('#'))
}

pub fn parse_cases(text: &str) -> Result<Vec<Given>, ParseError> {
    let mut cases = Vec::new();
    for (no, line) in significant(text) {
        let f = fields(line, 8, no)?;
        let fail = |m: String| ParseError {
            line: no,
            message: m,
        };
        let start = match (optional(f[3]), optional(f[4]), optional(f[5])) {
            (None, None, None) => None,
            (Some(axis), Some(kind), Some(mark)) => Some(Start {
                axis: axis.to_string(),
                kind: kind.to_string(),
                mark: number(mark, "mark", no)?,
            }),
            _ => {
                return Err(fail(
                    "a starting selection needs all of axis, kind and mark, or none of them".into(),
                ));
            }
        };
        cases.push(Given {
            name: f[0].to_string(),
            buffer: unescape(f[1]).map_err(|e| fail(format!("buffer: {e}")))?,
            point: number(f[2], "point", no)?,
            start,
            keys: unescape(f[6])
                .map_err(|e| fail(format!("keys: {e}")))?
                .chars()
                .collect(),
            diverges: optional(f[7])
                .map(|d| unescape(d).map_err(|e| fail(format!("diverges: {e}"))))
                .transpose()?,
        });
    }
    Ok(cases)
}

pub fn render_cases(cases: &[Given]) -> String {
    let mut out = String::from(CASES_HEADER);
    out.push('\n');
    for c in cases {
        let (axis, kind, mark) = match &c.start {
            Some(s) => (s.axis.as_str(), s.kind.as_str(), s.mark.to_string()),
            None => ("-", "-", "-".to_string()),
        };
        let keys: String = c.keys.iter().collect();
        let _ = writeln!(
            out,
            "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
            c.name,
            escape(&c.buffer),
            c.point,
            axis,
            kind,
            mark,
            escape(&keys),
            c.diverges.as_deref().map(escape).unwrap_or("-".into()),
        );
    }
    out
}

/// Keyed by case name so a golden and a case file can drift in ordering
/// without the comparison silently pairing the wrong rows.
pub fn parse_golden(text: &str) -> Result<BTreeMap<String, Observation>, ParseError> {
    let mut out = BTreeMap::new();
    for (no, line) in significant(text) {
        let f = fields(line, 10, no)?;
        let obs = Observation {
            text: unescape(f[1]).map_err(|e| ParseError {
                line: no,
                message: format!("text: {e}"),
            })?,
            point: number(f[2], "point", no)?,
            mark: optional(f[3]).map(|m| number(m, "mark", no)).transpose()?,
            axis: optional(f[4]).map(str::to_string),
            kind: optional(f[5]).map(str::to_string),
            history: number(f[6], "history", no)?,
            signals: number(f[7], "signals", no)?,
            state: f[8].to_string(),
            grab: optional(f[9])
                .map(|g| {
                    let (start, end) = g.split_once('-').ok_or_else(|| ParseError {
                        line: no,
                        message: format!("grab: `{g}` is not START-END"),
                    })?;
                    Ok((
                        number(start, "grab start", no)?,
                        number(end, "grab end", no)?,
                    ))
                })
                .transpose()?,
        };
        if out.insert(f[0].to_string(), obs).is_some() {
            return Err(ParseError {
                line: no,
                message: format!("duplicate case name `{}`", f[0]),
            });
        }
    }
    Ok(out)
}

pub fn render_golden(observations: &BTreeMap<String, Observation>) -> String {
    let mut out = String::from(GOLDEN_HEADER);
    out.push('\n');
    for (name, o) in observations {
        let _ = writeln!(
            out,
            "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
            name,
            escape(&o.text),
            o.point,
            o.mark.map(|m| m.to_string()).unwrap_or("-".into()),
            o.axis.as_deref().unwrap_or("-"),
            o.kind.as_deref().unwrap_or("-"),
            o.history,
            o.signals,
            o.state,
            o.grab
                .map(|(s, e)| format!("{s}-{e}"))
                .unwrap_or("-".into()),
        );
    }
    out
}

/// A human-readable account of how two observations differ, or `None` when
/// they do not. Used both to fail a test and to explain a divergence that has
/// healed, so it names every field rather than stopping at the first.
pub fn diff(expected: &Observation, actual: &Observation) -> Option<String> {
    let mut lines = Vec::new();
    let mut cmp = |what: &str, e: String, a: String| {
        if e != a {
            lines.push(format!("  {what:<8} emacs {e:<24} meowr {a}"));
        }
    };
    cmp(
        "text",
        format!("{:?}", expected.text),
        format!("{:?}", actual.text),
    );
    cmp(
        "point",
        expected.point.to_string(),
        actual.point.to_string(),
    );
    cmp("mark", opt(expected.mark), opt(actual.mark));
    cmp(
        "axis",
        expected.axis.clone().unwrap_or("-".into()),
        actual.axis.clone().unwrap_or("-".into()),
    );
    cmp(
        "kind",
        expected.kind.clone().unwrap_or("-".into()),
        actual.kind.clone().unwrap_or("-".into()),
    );
    cmp(
        "history",
        expected.history.to_string(),
        actual.history.to_string(),
    );
    cmp(
        "signals",
        expected.signals.to_string(),
        actual.signals.to_string(),
    );
    cmp("state", expected.state.clone(), actual.state.clone());
    let grab = |g: Option<(usize, usize)>| g.map(|(s, e)| format!("{s}-{e}")).unwrap_or("-".into());
    cmp("grab", grab(expected.grab), grab(actual.grab));
    if lines.is_empty() {
        None
    } else {
        Some(lines.join("\n"))
    }
}

fn opt(v: Option<usize>) -> String {
    v.map(|v| v.to_string()).unwrap_or("-".into())
}

#[cfg(test)]
mod tests;
