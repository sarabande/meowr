use super::*;
use crate::tests::Literals;

fn doc(text: &str) -> Document {
    Document::from_text(text)
}

fn text_of(doc: &Document, range: Range<usize>) -> String {
    let t = doc.text();
    t.slice(t.byte_to_char(range.start)..t.byte_to_char(range.end))
        .to_string()
}

// --- the depth-counted pair matcher --------------------------------------

#[test]
fn a_pair_gives_the_enclosing_delimiters_not_the_nearest_ones() {
    // From inside the inner pair, the inner pair is the answer.
    let d = doc("(alpha (beta) gamma)");
    let t = Things::new();
    let inner = t.inner(&d, 9, "round").unwrap();
    assert_eq!(text_of(&d, inner), "beta");
}

#[test]
fn a_pair_counts_depth_rather_than_matching_the_first_delimiter_seen() {
    // Sitting after the nested pair, the *outer* one encloses the point. A
    // matcher that stopped at the first `)` backwards would answer `(beta)`.
    let d = doc("(alpha (beta) gamma)");
    let t = Things::new();
    let bounds = t.bounds(&d, 15, "round").unwrap();
    assert_eq!(text_of(&d, bounds), "(alpha (beta) gamma)");
}

#[test]
fn inner_excludes_the_delimiters_and_bounds_includes_them() {
    let d = doc("x{abc}y");
    let t = Things::new();
    assert_eq!(text_of(&d, t.inner(&d, 3, "curly").unwrap()), "abc");
    assert_eq!(text_of(&d, t.bounds(&d, 3, "curly").unwrap()), "{abc}");
}

#[test]
fn a_pair_with_no_enclosing_delimiters_is_not_found() {
    let d = doc("no brackets here");
    let t = Things::new();
    assert_eq!(t.inner(&d, 5, "round"), None);
}

#[test]
fn square_and_curly_are_separate_things() {
    let d = doc("[a {b} c]");
    let t = Things::new();
    assert_eq!(text_of(&d, t.inner(&d, 4, "square").unwrap()), "a {b} c");
    assert_eq!(text_of(&d, t.inner(&d, 4, "curly").unwrap()), "b");
}

// --- the match Thing: whichever bracket encloses the point ----------------

#[test]
fn a_match_is_the_innermost_pair_around_the_point() {
    let d = doc("outer (alpha (beta) gamma) tail");
    let inner = enclosing_pair(&d, 15, None, Extent::Inner).unwrap();
    assert_eq!(text_of(&d, inner), "beta");
}

#[test]
fn a_match_does_not_care_which_of_the_three_brackets_it_is() {
    // The whole point of the Thing: `, r` `, s` `, c` each make you already
    // know which bracket you are inside, and this one does not ask.
    let d = doc("a {b [c] d}");
    assert_eq!(
        text_of(&d, enclosing_pair(&d, 6, None, Extent::Inner).unwrap()),
        "c"
    );
    assert_eq!(
        text_of(&d, enclosing_pair(&d, 3, None, Extent::Inner).unwrap()),
        "b [c] d"
    );
}

#[test]
fn a_match_inner_excludes_the_delimiters_and_bounds_includes_them() {
    let d = doc("x{abc}y");
    assert_eq!(
        text_of(&d, enclosing_pair(&d, 3, None, Extent::Inner).unwrap()),
        "abc"
    );
    assert_eq!(
        text_of(&d, enclosing_pair(&d, 3, None, Extent::Bounds).unwrap()),
        "{abc}"
    );
}

#[test]
fn a_match_is_quiet_where_no_pair_encloses_the_point() {
    // Where this Thing parts company with `meow-block`, deliberately.
    // `o` on this buffer selects `(baz)` — it scans for the delimiter that
    // returns to the starting depth, which finds a block the point is merely
    // *before*. A Thing is the one **around** the point, as every other entry
    // in `meow-char-thing-table` is, so there is nothing here to take.
    let d = doc("foo bar (baz) qux");
    assert_eq!(enclosing_pair(&d, 0, None, Extent::Inner), None);
}

#[test]
fn the_point_on_an_opening_bracket_is_not_yet_inside_it() {
    // The same rule `pair` keeps: the backward scan starts at the character
    // *before* the point, so the bracket the point sits on is not one that
    // encloses it. `, r` answers nothing here too.
    let d = doc("f(x) end");
    assert_eq!(enclosing_pair(&d, 1, None, Extent::Inner), None);
    assert_eq!(
        text_of(&d, enclosing_pair(&d, 2, None, Extent::Inner).unwrap()),
        "x"
    );
}

#[test]
fn an_unclosed_pair_is_no_match_at_all() {
    // `meow-block` wraps `forward-list` in `ignore-errors` and makes no
    // selection when there is no partner. Reaching to the end of the buffer
    // instead would be a selection nobody asked for.
    let d = doc("f(a b");
    assert_eq!(enclosing_pair(&d, 3, None, Extent::Inner), None);
}

#[test]
fn a_closer_of_the_wrong_kind_is_no_match_either() {
    // One scan over three pairs has to say what mismatched nesting means.
    // It means the text is unbalanced, and an unbalanced pair has no bounds
    // to give.
    let d = doc("f(a] b)");
    assert_eq!(enclosing_pair(&d, 3, None, Extent::Inner), None);
}

#[test]
fn a_bracket_inside_a_string_does_not_enclose_the_point() {
    //                  0         1
    //                  0123456789012345
    let d = doc("f(a, \")\", b) end");
    let string = 5..8; // the `")"`, quotes included
    let port = Literals(&[string]);

    let bounds = enclosing_pair(&d, 2, Some(&port as _), Extent::Bounds).unwrap();

    // Counting the `)` at 6 would close the pair there and answer `(a, "`,
    // five characters short and unbalanced.
    assert_eq!(text_of(&d, bounds), "(a, \")\", b)");
}

#[test]
fn the_backward_scan_skips_them_too() {
    // The point is *past* the buried bracket this time, so it is the walk
    // back that has to ignore it: counted, the `)` at 6 pairs off against the
    // real `(` at 1, the scan walks out of the buffer, and a pair the point is
    // plainly inside answers nothing at all. A scan that is right in one
    // direction and wrong from the other end of the same selection is not
    // right.
    let d = doc("f(a, \")\", b) end");
    let string = 5..8;
    let port = Literals(&[string]);

    let bounds = enclosing_pair(&d, 10, Some(&port as _), Extent::Bounds).unwrap();

    assert_eq!(text_of(&d, bounds), "(a, \")\", b)");
}

#[test]
fn with_no_grammar_every_bracket_encloses() {
    // Not a degraded answer — the honest one. With no tree there is nothing
    // better to ask than the characters themselves, which is what `None`
    // means everywhere else this port is passed.
    let d = doc("f(a, \")\", b) end");

    let bounds = enclosing_pair(&d, 2, None, Extent::Bounds).unwrap();

    assert_eq!(text_of(&d, bounds), "(a, \")");
}

#[test]
fn a_match_and_a_round_disagree_inside_a_string_and_are_meant_to() {
    // The pair Things are a bare literal search — `meow--thing-parse-pair-search`
    // consults no syntax at all — and Meow's own `o` asks `syntax-ppss`. Two
    // matchers with two rules, inherited. `match` is meowr's own and joins the
    // second camp, so this disagreement is a decision and is pinned as one.
    let d = doc("f(a, \")\", b) end");
    let string = 5..8;
    let port = Literals(&[string]);
    let t = Things::new();

    let round = t.bounds(&d, 2, "round").unwrap();
    let matched = enclosing_pair(&d, 2, Some(&port as _), Extent::Bounds).unwrap();

    assert_ne!(round, matched);
    assert_eq!(
        text_of(&d, round),
        "(a, \")",
        "the literal search stops early"
    );
    assert_eq!(
        text_of(&d, matched),
        "(a, \")\", b)",
        "the syntax-aware one does not"
    );
}

// --- strings --------------------------------------------------------------

#[test]
fn a_string_is_found_by_parity_not_by_the_nearest_quote() {
    // A quote is both an opening and a closing delimiter, so searching
    // backwards for the nearest one cannot tell which side of a string the
    // point is on. Counting from the start of the buffer can.
    let d = doc("a \"one\" b \"two\" c");
    let t = Things::new();
    assert_eq!(text_of(&d, t.inner(&d, 12, "string").unwrap()), "two");
    assert_eq!(text_of(&d, t.bounds(&d, 4, "string").unwrap()), "\"one\"");
}

#[test]
fn a_point_outside_every_string_finds_none() {
    let d = doc("a \"one\" b \"two\" c");
    let t = Things::new();
    assert_eq!(t.inner(&d, 8, "string"), None, "between the two strings");
}

// --- lines, symbols, buffer ----------------------------------------------

#[test]
fn the_inner_of_a_line_starts_at_the_indentation() {
    let d = doc("alpha\n    beta\n");
    let t = Things::new();
    assert_eq!(text_of(&d, t.inner(&d, 10, "line").unwrap()), "beta");
    assert_eq!(text_of(&d, t.bounds(&d, 10, "line").unwrap()), "    beta\n");
}

#[test]
fn a_symbol_runs_over_underscores_where_a_word_would_not() {
    let d = doc("foo_bar baz");
    let t = Things::new();
    assert_eq!(text_of(&d, t.inner(&d, 4, "symbol").unwrap()), "foo_bar");
}

#[test]
fn the_buffer_thing_is_the_whole_document() {
    let d = doc("anything at all\n");
    let t = Things::new();
    assert_eq!(t.bounds(&d, 3, "buffer").unwrap(), 0..d.byte_len());
}

// --- paragraphs and sentences --------------------------------------------

#[test]
fn a_paragraph_is_the_run_of_lines_between_blank_ones_and_keeps_its_newline() {
    // The trailing newline is part of it, which is what makes killing a
    // paragraph leave no blank line behind. Verified against Emacs, which is
    // where the newline came from — this test asserted `"three\nfour"` until
    // the harness said otherwise.
    let d = doc("one\ntwo\n\nthree\nfour\n\nfive\n");
    let t = Things::new();
    assert_eq!(
        text_of(&d, t.bounds(&d, 10, "paragraph").unwrap()),
        "three\nfour\n"
    );
}

#[test]
fn a_sentence_needs_two_spaces_after_the_full_stop_as_emacs_does() {
    // `sentence-end-double-space` defaults to t, so a single space does not
    // end a sentence and the whole line is one. Surprising until you know the
    // rule, and it explains every complaint about `M-e`. This test asserted
    // `"Then another."` until the harness said otherwise.
    let d = doc("One thing. Then another. And more.");
    let t = Things::new();
    assert_eq!(
        text_of(&d, t.bounds(&d, 14, "sentence").unwrap()),
        "One thing. Then another. And more."
    );

    // With two spaces, the sentences are separate.
    let d = doc("One thing.  Then another.  And more.");
    assert_eq!(
        text_of(&d, t.bounds(&d, 15, "sentence").unwrap()),
        "Then another."
    );
}

// --- the registry ---------------------------------------------------------

#[test]
fn the_registry_is_extensible_by_name() {
    // The same answer the command registry gives, and the reason `Kind` is a
    // newtype over a string rather than an enum.
    let mut t = Things::new();
    t.register(
        "angle",
        Thing::Pair {
            open: '<',
            close: '>',
        },
    );
    let d = doc("a <b> c");
    assert_eq!(text_of(&d, t.inner(&d, 3, "angle").unwrap()), "b");
}

#[test]
fn an_unregistered_name_is_not_found_rather_than_a_panic() {
    let d = doc("text");
    let t = Things::new();
    assert_eq!(t.inner(&d, 0, "no-such-thing"), None);
}

#[test]
fn defun_is_deliberately_absent_until_tree_sitter() {
    // ADR-0006: a Thing whose boundary a heuristic would get wrong belongs
    // with tree-sitter, where it can be right.
    let t = Things::new();
    assert!(t.get("defun").is_none());
}

// --- what counts as a symbol ----------------------------------------------

#[test]
fn a_symbol_is_what_the_standard_syntax_table_says_it_is() {
    // `meow-symbol-thing` is `'symbol`, so Meow's answer *is* the syntax
    // table's. In the standard table — which is what `fundamental-mode` uses,
    // and so what a buffer with no mode should use — the symbol constituents
    // are `&*+-/<=>_|` and the word constituents include `$` and `%`.
    // Enumerated from Emacs, not remembered.
    //
    // The differential harness found this: on `let s = "("; f(x)`, `B` from
    // the `(` lands on the `=` in Emacs, because `=` is a symbol all by
    // itself, and landed on the `s` here.
    let things = Things::new();
    let doc = Document::from_text("let s = \"(\"; f(x)\n");

    // `inner`, which is what `bounds-of-thing-at-point` answers; meowr's
    // `bounds` adds the separator beside the symbol, which Emacs does not.
    assert_eq!(things.inner(&doc, 6, "symbol"), Some(6..7), "`=` alone");
    assert_eq!(things.inner(&doc, 0, "symbol"), Some(0..3), "`let`");
    assert_eq!(things.inner(&doc, 13, "symbol"), Some(13..14), "`f`");
    assert_eq!(
        things.inner(&doc, 8, "symbol"),
        None,
        "a string quote is not a symbol constituent"
    );
}

#[test]
fn the_operator_characters_run_together_as_one_symbol() {
    // `+=` is one symbol in the standard table, not two punctuation marks
    // with nothing between them.
    let things = Things::new();
    let doc = Document::from_text("a += b\n");

    assert_eq!(things.inner(&doc, 2, "symbol"), Some(2..4));
}
