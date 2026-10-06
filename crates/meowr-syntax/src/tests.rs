use super::*;
use ropey::Rope;

fn highlights_of(source: &str, language: &str) -> Vec<(String, String)> {
    let languages = Languages::new();
    let rope = Rope::from_str(source);
    let lang = languages.by_name(language).expect("language is registered");
    let syntax = parse(rope.slice(..), lang, &languages).expect("it parses");
    spans(&syntax, rope.slice(..), &languages, 0..source.len())
        .into_iter()
        .map(|s| {
            (
                s.name().to_string(),
                rope.slice(rope.byte_to_char(s.range.start)..rope.byte_to_char(s.range.end))
                    .to_string(),
            )
        })
        .collect()
}

#[test]
fn every_shipped_language_loads_with_its_queries() {
    // A grammar or query that fails to compile is skipped rather than fatal,
    // so this is the test that would otherwise silently pass with none.
    // The nine ADR-0007 commits to, under the amended rule: ship what the
    // author *opens*, not what a Doom config once installed a mode for. TOML
    // and Nix are here because the filesystem says so -- `Cargo.toml` is
    // edited daily and the machine is NixOS -- and Scheme because M2's config
    // file is one.
    let languages = Languages::new();
    const SHIPPED: &[&str] = &[
        "rust", "json", "python", "bash", "yaml", "markdown", "toml", "nix", "scheme",
    ];
    // Plus the one grammar that is not a language you open: markdown's block
    // parser injects what is inside a paragraph, and the vendored query names
    // that injection `markdown.inline` — so both have to be registered or a
    // document is highlighted down to the paragraph and no further.
    const INJECTED: &[&str] = &["markdown.inline"];
    assert_eq!(
        languages.len(),
        SHIPPED.len() + INJECTED.len(),
        "{SHIPPED:?} plus {INJECTED:?}; skipped: {:?}",
        languages.skipped()
    );
    for name in SHIPPED.iter().chain(INJECTED) {
        assert!(languages.by_name(name).is_some(), "{name} is missing");
    }
    // An injected grammar has no extension: it is reached through another
    // language's query, never by opening a file.
    for name in INJECTED {
        assert!(
            languages.for_path(&format!("x.{name}")).is_none(),
            "{name} should not be reachable by extension"
        );
    }
}

#[test]
fn rust_highlights_keywords_strings_and_functions() {
    let got = highlights_of("fn main() {\n    let s = \"hi\";\n}\n", "rust");
    let names: Vec<&str> = got.iter().map(|(n, _)| n.as_str()).collect();
    assert!(names.contains(&"keyword"), "{got:?}");
    assert!(names.contains(&"function"), "{got:?}");
    assert!(
        got.iter().any(|(n, t)| n == "string" && t.contains("hi")),
        "{got:?}"
    );
}

#[test]
fn an_injection_is_parsed_with_the_injected_grammar() {
    // A Rust doc comment containing a fenced code block: the block is parsed
    // as Rust in its own layer, so `fn` inside it is a keyword rather than
    // part of the comment. This is the case that proves injections work at
    // all, which nothing else here would catch.
    let source = "/// ```rust\n/// fn inner() {}\n/// ```\nfn outer() {}\n";
    let got = highlights_of(source, "rust");
    let inner_keyword = got
        .iter()
        .any(|(name, text)| name == "keyword" && text == "fn");
    assert!(inner_keyword, "no injected keyword found in {got:?}");
}

#[test]
fn a_language_is_recognised_by_file_extension() {
    let languages = Languages::new();
    assert_eq!(
        languages.name_of(languages.for_path("src/main.rs").unwrap()),
        Some("rust")
    );
    assert_eq!(
        languages.name_of(languages.for_path("/tmp/data.json").unwrap()),
        Some("json")
    );
}

#[test]
fn an_unknown_extension_has_no_language_rather_than_erroring() {
    // A buffer with no grammar opens as plain text. `None` is an ordinary
    // answer here, not a failure.
    let languages = Languages::new();
    assert!(languages.for_path("notes.xyzzy").is_none());
    assert!(languages.for_path("Makefile").is_none());
}

#[test]
fn only_the_asked_for_range_is_highlighted() {
    // Highlighting a whole file to draw fifty rows of it is the difference
    // between a frame and a stutter, so the range is honoured.
    let source = "fn a() {}\nfn b() {}\nfn c() {}\n";
    let languages = Languages::new();
    let rope = Rope::from_str(source);
    let lang = languages.by_name("rust").unwrap();
    let syntax = parse(rope.slice(..), lang, &languages).unwrap();

    let middle = spans(&syntax, rope.slice(..), &languages, 10..20);

    assert!(!middle.is_empty());
    assert!(
        middle
            .iter()
            .all(|s| s.range.start < 20 && s.range.end > 10),
        "{middle:?}"
    );
}

#[test]
fn spans_are_in_order_and_do_not_overlap() {
    // The render layer walks them in one pass, so this is a precondition
    // rather than a nicety.
    let source = "fn main() {\n    let x = 1 + 2;\n}\n";
    let got = {
        let languages = Languages::new();
        let rope = Rope::from_str(source);
        let lang = languages.by_name("rust").unwrap();
        let syntax = parse(rope.slice(..), lang, &languages).unwrap();
        spans(&syntax, rope.slice(..), &languages, 0..source.len())
    };
    for pair in got.windows(2) {
        assert!(
            pair[0].range.end <= pair[1].range.start,
            "{:?} then {:?}",
            pair[0],
            pair[1]
        );
    }
}

// --- tree-sitter Things ---------------------------------------------------

/// The byte offset just after `needle`, so tests can name a place in the
/// source rather than count to it — a hardcoded offset is a test that breaks
/// when the fixture is reformatted, and lies about why.
fn at(source: &str, needle: &str) -> usize {
    source.find(needle).expect("needle is in the source") + needle.len()
}

fn thing_text(
    source: &str,
    language: &str,
    pos: usize,
    thing: &str,
    inner: bool,
) -> Option<String> {
    let languages = Languages::new();
    let rope = Rope::from_str(source);
    let lang = languages.by_name(language)?;
    let syntax = parse(rope.slice(..), lang, &languages)?;
    let range = text_object(&syntax, rope.slice(..), &languages, pos, thing, inner)?;
    Some(
        rope.slice(rope.byte_to_char(range.start)..rope.byte_to_char(range.end))
            .to_string(),
    )
}

const RUST_SOURCE: &str = "\
struct S { a: u32 }

impl S {
    /// Doc.
    fn method(&self, n: u32) -> u32 {
        n + 1
    }
}
";

const PYTHON_SOURCE: &str = "\
class C:
    # A note.
    def method(self, n):
        return n + 1
";

#[test]
fn function_resolves_inner_and_bounds_in_rust_and_python() {
    // The point is inside the body of each method.
    let inner = thing_text(
        RUST_SOURCE,
        "rust",
        at(RUST_SOURCE, "n + 1"),
        "function",
        true,
    )
    .unwrap();
    assert!(inner.contains("n + 1"), "{inner:?}");
    assert!(!inner.contains("fn method"), "inner excludes the signature");

    let around = thing_text(
        RUST_SOURCE,
        "rust",
        at(RUST_SOURCE, "n + 1"),
        "function",
        false,
    )
    .unwrap();
    assert!(around.contains("fn method"), "{around:?}");

    let inner = thing_text(
        PYTHON_SOURCE,
        "python",
        at(PYTHON_SOURCE, "return n"),
        "function",
        true,
    )
    .unwrap();
    assert!(inner.contains("return n + 1"), "{inner:?}");
    let around = thing_text(
        PYTHON_SOURCE,
        "python",
        at(PYTHON_SOURCE, "return n"),
        "function",
        false,
    )
    .unwrap();
    assert!(around.contains("def method"), "{around:?}");
}

#[test]
fn class_parameter_and_comment_resolve_in_both_languages() {
    let class = thing_text(
        RUST_SOURCE,
        "rust",
        at(RUST_SOURCE, "n + 1"),
        "class",
        false,
    )
    .unwrap();
    assert!(class.contains("impl S"), "{class:?}");

    let parameter = thing_text(
        RUST_SOURCE,
        "rust",
        at(RUST_SOURCE, "n: u32) ->") - 6,
        "parameter",
        false,
    )
    .unwrap();
    assert!(
        parameter.contains('n') || parameter.contains("self"),
        "{parameter:?}"
    );

    let comment = thing_text(
        RUST_SOURCE,
        "rust",
        at(RUST_SOURCE, "Doc."),
        "comment",
        false,
    )
    .unwrap();
    assert!(comment.contains("Doc."), "{comment:?}");

    let class = thing_text(
        PYTHON_SOURCE,
        "python",
        at(PYTHON_SOURCE, "return n"),
        "class",
        false,
    )
    .unwrap();
    assert!(class.contains("class C"), "{class:?}");
    let comment = thing_text(
        PYTHON_SOURCE,
        "python",
        at(PYTHON_SOURCE, "A note."),
        "comment",
        false,
    )
    .unwrap();
    assert!(comment.contains("A note."), "{comment:?}");
}

#[test]
fn defun_is_the_function_thing() {
    // ADR-0006 sends `defun` to tree-sitter rather than to a heuristic, and a
    // defun is a function.
    let by_defun = thing_text(
        RUST_SOURCE,
        "rust",
        at(RUST_SOURCE, "n + 1"),
        "defun",
        false,
    );
    let by_function = thing_text(
        RUST_SOURCE,
        "rust",
        at(RUST_SOURCE, "n + 1"),
        "function",
        false,
    );
    assert_eq!(by_defun, by_function);
    assert!(by_defun.is_some());
}

#[test]
fn the_innermost_match_wins() {
    // A method inside an impl matches both `function` and `class`; the
    // function is the one the point is actually in.
    let function = thing_text(
        RUST_SOURCE,
        "rust",
        at(RUST_SOURCE, "n + 1"),
        "function",
        false,
    )
    .unwrap();
    let class = thing_text(
        RUST_SOURCE,
        "rust",
        at(RUST_SOURCE, "n + 1"),
        "class",
        false,
    )
    .unwrap();
    assert!(function.len() < class.len(), "{function:?} vs {class:?}");
}

#[test]
fn a_thing_with_no_match_around_the_point_is_none() {
    // Outside every function: not found, which the commands treat as leaving
    // the selection alone rather than as an error.
    assert!(thing_text(RUST_SOURCE, "rust", 0, "function", false).is_none());
}

#[test]
fn no_shipped_language_is_skipped() {
    // A query that will not compile costs its language only the feature that
    // query provides, and `skipped` is what says so rather than leaving it to
    // be discovered. Nothing is skipped now, and this fails if that changes.
    assert!(
        Languages::new().skipped().is_empty(),
        "{:?}",
        Languages::new().skipped()
    );
}

#[test]
fn toml_highlights_tables_keys_and_values() {
    // `Cargo.toml` is the file this was added for.
    let got = highlights_of(
        "[package]\nname = \"meowr\"\nedition = 2024\n# a comment\n",
        "toml",
    );
    let names: Vec<&str> = got.iter().map(|(n, _)| n.as_str()).collect();
    assert!(names.contains(&"property"), "`name` is a key: {got:?}");
    assert!(
        names.contains(&"string"),
        "`\"meowr\"` is a string: {got:?}"
    );
    assert!(names.contains(&"comment"), "{got:?}");
    assert!(
        names.iter().any(|n| n.starts_with("number")),
        "2024 is a number: {got:?}"
    );
}

#[test]
fn nix_highlights_an_attribute_set() {
    // The machine this is developed on is NixOS.
    let got = highlights_of(
        "{ pkgs, ... }:\n{\n  # a comment\n  home.packages = [ pkgs.ripgrep ];\n}\n",
        "nix",
    );
    let names: Vec<&str> = got.iter().map(|(n, _)| n.as_str()).collect();
    assert!(names.contains(&"comment"), "{got:?}");
    assert!(names.contains(&"variable"), "`pkgs` is a variable: {got:?}");
    assert!(
        names.iter().any(|n| n.starts_with("punctuation")),
        "the braces and semicolon are punctuation: {got:?}"
    );
}

#[test]
fn scheme_highlights_the_steel_forms_a_config_is_made_of() {
    // This is the config from M2's map issue, and the reason Scheme is here at
    // all: `init.scm` should not be edited in a plain buffer.
    //
    // Steel is a Scheme *dialect*, so what is proved here is that the forms
    // the two share highlight — `define`, an application, a string, a comment.
    // Steel's own forms are not covered by a stock Scheme grammar and nothing
    // here claims they are; #44 and #47 are where Steel's real syntax gets
    // read, and this test is worth revisiting with a genuine `init.scm` then.
    let got = highlights_of(
        ";; a comment\n(define (save-and-exit)\n  (call \"write-file\")\n  (call \"leave-insert\"))\n",
        "scheme",
    );
    let names: Vec<&str> = got.iter().map(|(n, _)| n.as_str()).collect();
    assert!(names.contains(&"comment"), "{got:?}");
    assert!(names.contains(&"keyword"), "`define` is a keyword: {got:?}");
    assert!(
        names.contains(&"function"),
        "`save-and-exit` and `call` are functions: {got:?}"
    );
    assert!(
        names.contains(&"string"),
        "the command names are strings: {got:?}"
    );
}

#[test]
fn yaml_highlights_keys_and_values() {
    let got = highlights_of("name: meowr\nport: 8080\n# a comment\n", "yaml");
    let names: Vec<&str> = got.iter().map(|(n, _)| n.as_str()).collect();
    assert!(names.contains(&"comment"), "{got:?}");
    assert!(
        names.iter().any(|n| n.starts_with("number")),
        "8080 is a number: {got:?}"
    );
}

#[test]
fn markdown_highlights_its_block_structure() {
    let got = highlights_of(
        "# Title\n\nSome text.\n\n```rust\nfn f() {}\n```\n",
        "markdown",
    );
    let names: Vec<&str> = got.iter().map(|(n, _)| n.as_str()).collect();
    assert!(names.contains(&"text.title"), "the heading: {got:?}");
    assert!(
        names.contains(&"punctuation.delimiter"),
        "the fence: {got:?}"
    );
}

#[test]
fn markdown_highlights_inside_a_paragraph_through_its_injection() {
    // Two bugs stood between the grammar loading and a paragraph being
    // coloured, and both were here rather than upstream.
    //
    // tree-house excludes an injected node's children unless the query says
    // `injection.include-children`. The crate's own query does not say it,
    // because it was written for nvim-treesitter, which includes them by
    // default — so the inline layer was handed the paragraph with every `*`
    // and backtick cut out of it, and correctly found no emphasis. The query
    // is vendored from Helix, which is written against tree-house.
    let got = highlights_of("Some *emph* and `code` here.\n", "markdown");
    let names: Vec<&str> = got.iter().map(|(n, _)| n.as_str()).collect();
    assert!(names.contains(&"text.emphasis"), "{got:?}");
    assert!(names.contains(&"text.literal"), "{got:?}");
}

#[test]
fn the_inline_grammar_also_stands_on_its_own() {
    // Parsed directly rather than through the injection. Kept because it is
    // what localised the fault when a document highlighted down to the
    // paragraph and no further: the same text works here and did not there.
    let got = highlights_of("Some *emph* here.\n", "markdown.inline");
    let names: Vec<&str> = got.iter().map(|(n, _)| n.as_str()).collect();
    assert!(names.contains(&"text.emphasis"), "{got:?}");
}

// --- brackets that are not delimiters -------------------------------------

/// Which byte offsets of `source` the tree calls string or comment.
fn literal_offsets(source: &str, language: &str) -> Vec<usize> {
    let languages = Languages::new();
    let rope = Rope::from_str(source);
    let lang = languages.by_name(language).expect("language is registered");
    let syntax = parse(rope.slice(..), lang, &languages).expect("it parses");
    (0..source.len())
        .filter(|byte| literal_at(&syntax, *byte).is_some())
        .collect()
}

#[test]
fn a_bracket_inside_a_string_is_reported_as_text() {
    let source = "fn f() { let s = \")\"; }";
    let literals = literal_offsets(source, "rust");
    let quoted = source.find("\")\"").unwrap();

    assert!(
        literals.contains(&(quoted + 1)),
        "the `)` at {} is inside a string",
        quoted + 1
    );
    // The brackets that really are delimiters are not.
    for c in ['(', ')', '{', '}'] {
        let at = source.find(c).unwrap();
        assert!(!literals.contains(&at), "the first `{c}` is code");
    }
}

#[test]
fn a_bracket_inside_a_comment_is_reported_as_text() {
    let source = "fn f() {\n    // a ) here\n}\n";
    let literals = literal_offsets(source, "rust");
    let commented = source.find("a ) here").unwrap() + 2;

    assert!(literals.contains(&commented));
}

#[test]
fn interpolation_inside_an_f_string_is_still_code() {
    // The reason `literal_at` consults the innermost node and does not climb
    // to the ancestors. The f-string *is* a string, so any ancestor walk would
    // call the `)` of `{f()}` text, and `meow-block` would refuse to select
    // it.
    let source = "x = f\"a {f()} b\"\n";
    let literals = literal_offsets(source, "python");
    let closing = source.find("f()").unwrap() + 2;

    assert!(
        !literals.contains(&closing),
        "the `)` of `f()` is code, not the string's text"
    );
    let text = source.find("a {").unwrap();
    assert!(literals.contains(&text), "`a ` is the string's own text");
}
