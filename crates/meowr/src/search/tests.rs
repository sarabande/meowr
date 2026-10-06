//! The parse, driven over recorded `rg --json` output.
//!
//! Separated from running the process for the reason `lsp.rs` separates the
//! protocol from the child: what a match record *means* is where the work is,
//! and none of it needs a tree on disk to exercise.

use super::*;

/// One `match` record, spelled the way `rg --json` spells it.
fn match_record(path: &str, line_number: usize, line: &str, start: usize) -> String {
    serde_json::json!({
        "type": "match",
        "data": {
            "path": {"text": path},
            "lines": {"text": line},
            "line_number": line_number,
            "absolute_offset": 0,
            "submatches": [{"match": {"text": "x"}, "start": start, "end": start + 1}],
        }
    })
    .to_string()
}

#[test]
fn a_match_becomes_a_place_carrying_the_line_it_matched() {
    // The half grep gives for free and a language server does not (ADR-0011):
    // the matched line comes back with the position, so a row is readable
    // without opening the file it names.
    let out = match_record("src/lib.rs", 412, "    let widget = self.narrow;\n", 8);

    let places = places_in(&out);

    assert_eq!(places.len(), 1);
    assert_eq!(places[0].path, "src/lib.rs");
    assert_eq!(places[0].line, 411, "rg counts from one and a Place from zero");
    assert_eq!(places[0].column, 8);
    assert_eq!(
        places[0].text.as_deref(),
        Some("    let widget = self.narrow;"),
        "the line, without the newline rg leaves on it"
    );
}

#[test]
fn a_path_rg_could_not_spell_yields_no_place() {
    // `rg` writes `{"bytes": …}` where a path is not valid UTF-8, and a Place
    // is somewhere to *go*: a path meowr cannot spell is one it cannot open.
    // Skipped rather than decoded lossily, which would be a row that looks
    // like a file and opens nothing.
    let out = serde_json::json!({
        "type": "match",
        "data": {
            "path": {"bytes": "L3RtcC//bmFtZQ=="},
            "lines": {"text": "let widget = 1;\n"},
            "line_number": 1,
            "submatches": [{"match": {"text": "widget"}, "start": 4, "end": 10}],
        }
    })
    .to_string();

    assert!(places_in(&out).is_empty());
}

#[test]
fn the_records_that_are_not_matches_are_passed_over() {
    // `rg --json` opens with `begin`, closes with `end`, and finishes the run
    // with `summary`. All three say what was *searched* rather than what was
    // found, and their `data` is a different shape -- which is why the kind is
    // read before the rest of the record is.
    let out = [
        r#"{"type":"begin","data":{"path":{"text":"one.rs"}}}"#,
        &match_record("one.rs", 2, "let widget = 1;\n", 4),
        r#"{"type":"end","data":{"path":{"text":"one.rs"},"stats":{"matches":1}}}"#,
        r#"{"type":"summary","data":{"stats":{"matches":1},"elapsed_total":{"secs":0}}}"#,
    ]
    .join("\n");

    let places = places_in(&out);

    assert_eq!(places.len(), 1, "the match, and only the match");
    assert_eq!(places[0].line, 1);
}

#[test]
fn a_column_is_counted_in_utf16_the_way_a_place_is() {
    // rg gives a byte offset into the line; a Place's column is UTF-16, which
    // is LSP's unit and the one the rest of the domain converted to. Grep is
    // exactly the source ADR-0011 says *can* convert, because it hands back
    // the line -- so it does, here, rather than handing on a byte count that
    // would put the point in the wrong place on any line with an emoji in it.
    let line = "let 🌍 = Selection::at(0);\n";
    let out = match_record("src/main.rs", 1, line, 11);

    let places = places_in(&out);

    assert_eq!(places[0].column, 9, "four + two + three, not eleven bytes");
}

// --- the mechanism, which has a filesystem and a child process in it --------

/// Where a test's tree goes. Named for the test, so two running at once do not
/// share one.
fn under(name: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!("meowr-search-{name}-{}", std::process::id()))
}

/// A tree to search, named for the test that wants it.
///
/// A real one, because what is being proved is what `rg` does with a real
/// directory -- the same reasoning `walked`'s tests give a few hundred lines
/// away, and the same shape of fixture.
fn tree(name: &str) -> std::path::PathBuf {
    let at = under(name);
    let _ = std::fs::remove_dir_all(&at);
    std::fs::create_dir_all(at.join("sub")).unwrap();
    at
}

#[tokio::test]
async fn a_pattern_and_a_directory_come_back_as_places_with_their_lines() {
    let at = tree("finds");
    std::fs::write(at.join("one.rs"), "fn main() {}\nlet widget = 1;\n").unwrap();
    std::fs::write(at.join("sub/two.rs"), "// nothing here\n").unwrap();

    let found = search(&Pattern::Regex("widget".into()), &at).await.expect("a directory that is there");

    assert_eq!(found.len(), 1);
    assert_eq!(found[0].path, at.join("one.rs").to_string_lossy());
    assert_eq!(found[0].line, 1, "zero-based, as a Place is");
    assert_eq!(found[0].text.as_deref(), Some("let widget = 1;"));
    std::fs::remove_dir_all(&at).unwrap();
}

#[tokio::test]
async fn an_ignored_file_is_not_searched() {
    // The Project listing honours `--exclude-standard`; this is the same
    // promise through the other tool. `rg` reads `.gitignore` only where it
    // finds a repository, which is not a gap: the walk `project-scope` off
    // performs says the same thing in its own comment -- being outside a
    // repository is most of what having no `.gitignore` means.
    let at = tree("ignores");
    std::fs::create_dir_all(at.join(".git")).unwrap();
    std::fs::write(at.join(".gitignore"), "hidden.rs\n").unwrap();
    std::fs::write(at.join("kept.rs"), "let widget = 1;\n").unwrap();
    std::fs::write(at.join("hidden.rs"), "let widget = 2;\n").unwrap();

    let found = search(&Pattern::Regex("widget".into()), &at).await.expect("a directory that is there");

    let paths: Vec<String> = found.iter().map(|place| place.path.clone()).collect();
    assert_eq!(paths, vec![at.join("kept.rs").to_string_lossy().to_string()]);
    std::fs::remove_dir_all(&at).unwrap();
}

#[tokio::test]
async fn a_directory_that_cannot_be_read_says_so_rather_than_returning_nothing() {
    // ADR-0010, one tool over: an empty list is not an answer to "why is there
    // nothing here", and `Ok(Vec::new())` on a search that never ran is the
    // silent fallback that ADR spent a commit undoing.
    let at = under("absent");
    let _ = std::fs::remove_dir_all(&at);

    let why = search(&Pattern::Regex("widget".into()), &at).await.expect_err("nothing to search");

    assert!(why.contains(&at.display().to_string()), "where it looked: {why}");
}

#[tokio::test]
async fn nothing_matching_is_an_empty_list_and_not_a_refusal() {
    // The other side of that line, and the one `rg` draws for us: it exits 1
    // when it searched and found nothing, and 2 when it could not search. A
    // tree with no match in it really is an empty list.
    let at = tree("empty");
    std::fs::write(at.join("one.rs"), "fn main() {}\n").unwrap();

    let found = search(&Pattern::Regex("widget".into()), &at).await.expect("it searched fine");

    assert!(found.is_empty());
    std::fs::remove_dir_all(&at).unwrap();
}

#[tokio::test]
async fn a_pattern_beginning_with_a_dash_is_a_pattern() {
    // What the `--` in the argument list is for. Without it `rg` reads
    // `-widget` as a flag it does not have, exits 2, and the row reports a
    // search that could not run -- for a pattern that is perfectly searchable.
    let at = tree("dash");
    std::fs::write(at.join("one.rs"), "let x = y -widget;\n").unwrap();

    let found = search(&Pattern::Regex("-widget".into()), &at).await.expect("a pattern, not a flag");

    assert_eq!(found.len(), 1);
    assert_eq!(found[0].text.as_deref(), Some("let x = y -widget;"));
    std::fs::remove_dir_all(&at).unwrap();
}

#[tokio::test]
async fn a_symbol_is_searched_as_text_rather_than_as_a_regex() {
    // meowr's `symbol` Thing is Emacs's syntax classes `w` and `_` together,
    // and the `_` class in the standard table is exactly `&*+-/<=>_|`. So the
    // symbol at the point really can be `*`, and `rg -- '*'` is a regex parse
    // error: the row would report a search that could not run, for a symbol
    // that is perfectly searchable. `SPC s .` means the text, not a pattern.
    let at = tree("literal");
    std::fs::write(at.join("one.rs"), "let x = a * b;\nlet y = c += d;\n").unwrap();

    let star = search(&Pattern::Literal("*".into()), &at)
        .await
        .expect("a symbol, searched as one");
    assert_eq!(star.len(), 1);
    assert_eq!(star[0].text.as_deref(), Some("let x = a * b;"));

    let plus = search(&Pattern::Literal("+=".into()), &at)
        .await
        .expect("and this one too");
    assert_eq!(plus.len(), 1);
    std::fs::remove_dir_all(&at).unwrap();
}

#[tokio::test]
async fn what_the_user_typed_is_read_as_a_regex() {
    // The other half, and `consult-ripgrep`'s own behaviour with the same
    // input: a pattern is a pattern. Making everything literal would take away
    // the thing project search is mostly for.
    let at = tree("regex");
    std::fs::write(at.join("one.rs"), "fn alpha() {}\nfn beta() {}\n").unwrap();

    let found = search(&Pattern::Regex(r"fn (alpha|beta)".into()), &at)
        .await
        .expect("a regex, read as one");

    assert_eq!(found.len(), 2);
    std::fs::remove_dir_all(&at).unwrap();
}
