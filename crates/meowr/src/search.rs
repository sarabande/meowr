//! Searching files on disk for a pattern.
//!
//! In the binary crate because this is I/O — a child process and its output —
//! and meowr's rule is that domain crates do none. The same fit `lsp.rs`
//! records: `project_of` shells out to `git` a few hundred lines up, and this
//! is that shape of thing again.
//!
//! **It shells out to `rg`, and #138 asked for that decided by measurement
//! rather than by argument.** Against `grep-searcher`, `grep-regex` and
//! `ignore` — which are ripgrep's own guts, so the comparison is with the same
//! code in-process:
//!
//! | | `rg` | the three crates |
//! |---|---|---|
//! | new crates in the lockfile | 0 | 18 |
//! | cold build of them | — | ~50 s |
//! | this workspace, ~100 files | 10 ms | 10 ms |
//! | the crates.io source cache, 15,218 `.rs` files over 473 MB | 119 ms | 531 ms |
//!
//! Reproducible, because a number nobody can re-run is an estimate wearing a
//! table. The crate side was a scratch package — `cargo add grep-searcher
//! grep-regex ignore`, a `WalkBuilder` walk with a `Searcher` per path on one
//! thread, which is the shape this module would have taken — and the count is
//! that package's `cargo tree` differenced against this workspace's
//! `Cargo.lock`, so the 13 crates both already share are not counted twice.
//! The build is `cargo clean` then `cargo build --release`. Each timing is the
//! best of three runs against the same tree, `rg --vimgrep` against the
//! equivalent walk, output discarded on both sides.
//!
//! The large tree is where it is decided. `rg` walks and searches in parallel;
//! matching that in-process means `WalkParallel` and a searcher per thread,
//! which is more concurrency in a layer that has none — so the crates cost 18
//! dependencies *and* the work, to arrive somewhere `rg` already is. On a real
//! project the two are indistinguishable, which is the case that matters and
//! the reason this is not a performance argument at all.
//!
//! The rest is that meowr already requires one external program for exactly
//! this class of question — ADR-0010 makes the Project a git repository, asked
//! of `git` — and the reference configuration requires this one: Doom's
//! `SPC s p` is `consult-ripgrep`, which shells out to `rg` and fails the same
//! way when it is absent. A missing `rg` is refusable in the shape ADR-0010
//! already established for a missing repository.
//!
//! `--json` rather than `--vimgrep`, and that is not a style choice. A vimgrep
//! row is `path:line:column:text` with nothing escaped, so a filename with a
//! colon in it parses as a different file at a different line. The JSON also
//! carries the matched line, which is what lets the byte offset `rg` reports
//! become the UTF-16 column a [`Place`] holds — grep being precisely the source
//! ADR-0011 says can convert for itself, because it has the line to count.
//!
//! `SPC s p`, `s d`, `s D` and `s .` are what call it (#139). It arrived one
//! ticket ahead of them because a searcher, Project scoping and seven leader
//! rows are three pieces of work wearing one number.

use meowr_domain::{Pattern, Place};
use serde::Deserialize;
use serde_json::Value;

/// One line of a `rg --json` stream.
///
/// Read through a type rather than by indexing a `Value`, which is the rule
/// `lsp.rs` states and the reason it gives: a hand-rolled reader guesses
/// exactly where a format has more than one shape, and this one has two.
/// `data` stays a `Value` because the four record kinds do not share a shape —
/// the same two-step `diagnostics_of` takes, where a `Value` becomes a typed
/// thing once its kind is known.
#[derive(Deserialize)]
struct Record {
    #[serde(rename = "type")]
    kind: String,
    data: Value,
}

/// What `rg` found on one line of one file.
///
/// Only the four fields this reads. The record carries more — an absolute
/// offset, the matched text itself — and naming fields nobody uses would be
/// modelling rg's format rather than meowr's question.
#[derive(Deserialize)]
struct Match {
    path: Spelled,
    lines: Spelled,
    line_number: usize,
    #[serde(default)]
    submatches: Vec<SubMatch>,
}

/// Where a match starts within its line, in bytes.
#[derive(Deserialize)]
struct SubMatch {
    start: usize,
}

/// A string `rg` could spell, or one it could not.
///
/// Every path and every line in the stream arrives as `{"text": …}` when it is
/// valid UTF-8 and `{"bytes": …}`, base64, when it is not. That is an
/// externally tagged enum on the wire, so it is one here: the shape meowr
/// cannot use is a variant rather than an absent field, which is the
/// distinction `HoverContents` is modelled on one module over.
#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum Spelled {
    Text(String),
    /// The bytes themselves are not read, and the type says so: there is
    /// nothing meowr could do with them that would not be a guess at a name.
    Bytes(serde::de::IgnoredAny),
}

impl Spelled {
    /// The string, where there is one meowr can read.
    fn text(&self) -> Option<&str> {
        match self {
            Spelled::Text(text) => Some(text),
            Spelled::Bytes(_) => None,
        }
    }
}

/// What matches `pattern` under `directory`, or why nothing could be looked at.
///
/// The paths a [`Place`] comes back with are the ones `rg` was handed, so an
/// absolute `directory` yields absolute Places and a relative one yields
/// relative. Callers pass the Project root, which ADR-0010 asks of
/// `git rev-parse --show-toplevel` and which answers absolutely — so a Place
/// from here opens from anywhere, which is what a Place is for.
///
/// **`rg`'s three exit codes are the whole of the error handling**, and they
/// draw ADR-0010's line for us. Nought is matches; **one is a search that ran
/// and found nothing**, which is a real empty list and the one case that is
/// genuinely `Ok(Vec::new())`; two is a search that could not run — an
/// unreadable directory, a pattern that is not a regex — and says so with what
/// `rg` said. A missing `rg` is the fourth case and reads the same way: the
/// program this needs is not there, which is a reason rather than a result.
///
/// No `--no-require-git`, deliberately. `rg` reads `.gitignore` only where it
/// finds a repository, and that is the position `project_of`'s own walk already
/// takes in its own words: being outside a repository is most of what having no
/// `.gitignore` means.
pub async fn search(pattern: &Pattern, directory: &std::path::Path) -> Result<Vec<Place>, String> {
    let Ok(output) = tokio::process::Command::new("rg")
        .arg("--json")
        // `--fixed-strings` is what makes a [`Pattern::Literal`] literal. The
        // symbol at the point can be `*` -- Emacs's `_` syntax class is exactly
        // `&*+-/<=>_|` -- and as a regex that is a parse error rather than a
        // search, so `SPC s .` would report a row that could not run for a
        // symbol that is perfectly searchable.
        .args(match pattern {
            Pattern::Literal(_) => &["--fixed-strings"][..],
            Pattern::Regex(_) => &[][..],
        })
        // `--` so a pattern beginning with a dash is a pattern rather than a
        // flag `rg` does not have. The directory goes after it as the one path
        // to search.
        .arg("--")
        .arg(pattern.text())
        .arg(directory)
        .output()
        .await
    else {
        return Err(format!(
            "rg could not be run, and {} is what it would have searched",
            directory.display()
        ));
    };
    match output.status.code() {
        Some(0) => Ok(places_in(&String::from_utf8_lossy(&output.stdout))),
        // It searched and there was nothing. The one empty list that is an
        // answer rather than a silence.
        Some(1) => Ok(Vec::new()),
        _ => Err(format!(
            "{} could not be searched: {}",
            directory.display(),
            // `rg` names the path and the reason on stderr; its sentence is
            // better than one invented here, which would have to guess between
            // an unreadable directory and a pattern that is not a regex.
            String::from_utf8_lossy(&output.stderr).trim()
        )),
    }
}

/// Every match in a `rg --json` stream, as [`Place`]s.
///
/// One JSON object per line, of which `match` is the only kind this reads:
/// `begin`, `end` and `summary` say what was searched rather than what was
/// found. A record this cannot read yields no Place rather than a wrong one,
/// which is `diagnostics_of`'s rule in `lsp.rs` and the same reasoning — the
/// tool said something meowr does not understand, and inventing a position for
/// it is worse than having none.
pub fn places_in(stdout: &str) -> Vec<Place> {
    stdout
        .lines()
        .filter_map(|line| serde_json::from_str::<Record>(line).ok())
        .filter(|record| record.kind == "match")
        .filter_map(|record| serde_json::from_value::<Match>(record.data).ok())
        .filter_map(place_of)
        .collect()
}

/// One match as a Place, if it is one meowr can name.
fn place_of(found: Match) -> Option<Place> {
    // A path meowr cannot spell is somewhere it cannot open, and a Place is
    // somewhere to *go*. Skipped rather than lossily decoded, for the reason
    // `diagnostics_of` places no squiggle for a record it cannot read: a wrong
    // answer is worse than none.
    let path = found.path.text()?;
    let text = found.lines.text()?;
    // rg counts lines from one and a Place from zero -- LSP's convention,
    // which is where the column's unit comes from too.
    let line = found.line_number.checked_sub(1)?;
    // The first match on the line. A line matching twice is one row, because
    // it is one line to go to, and going to the first of them is what
    // `consult-ripgrep` does with the same output.
    let start = found.submatches.first().map_or(0, |at| at.start);
    let column = text
        .get(..start.min(text.len()))
        .unwrap_or_default()
        .chars()
        .map(char::len_utf16)
        .sum();
    Some(Place::new(path, line, column).showing(text.trim_end_matches(['\n', '\r'])))
}

#[cfg(test)]
mod tests;
