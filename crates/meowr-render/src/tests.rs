use super::*;
use meowr_view::Row;
use ratatui::{Terminal, backend::TestBackend, buffer::Buffer};

fn row(line: usize, text: &str) -> Row {
    Row {
        line,
        text: text.to_string(),
        selected: None,
        cursor: None,
        current: false,
    }
}

fn marked(line: usize, text: &str, selected: std::ops::Range<usize>, cursor: usize) -> Row {
    Row {
        line,
        text: text.to_string(),
        selected: Some(selected),
        cursor: Some(cursor),
        current: false,
    }
}

fn draw_to(width: u16, height: u16, body: Body<'_>) -> Buffer {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal
        .draw(|frame| draw(frame, frame.area(), body))
        .unwrap();
    terminal.backend().buffer().clone()
}

#[test]
fn rows_render_one_per_line_from_the_top() {
    let rows = [row(0, "alpha"), row(1, "beta")];

    let buffer = draw_to(8, 3, Body::Rows(&rows));

    assert_eq!(
        buffer,
        Buffer::with_lines(["alpha   ", "beta    ", "        "])
    );
}

#[test]
fn a_message_renders_when_there_is_no_document() {
    let buffer = draw_to(10, 2, Body::Message("Loading…"));

    assert_eq!(buffer, Buffer::with_lines(["Loading…  ", "          "]));
}

#[test]
fn a_line_longer_than_the_window_is_clipped_not_wrapped() {
    let rows = [row(0, "abcdefghij")];

    let buffer = draw_to(4, 2, Body::Rows(&rows));

    assert_eq!(buffer, Buffer::with_lines(["abcd", "    "]));
}

#[test]
fn wide_characters_occupy_the_cells_they_actually_need() {
    // Each CJK glyph is two cells wide, so four of them fill an 8-cell row.
    let rows = [row(0, "日本語だ")];

    let buffer = draw_to(8, 1, Body::Rows(&rows));

    assert_eq!(buffer, Buffer::with_lines(["日本語だ"]));
}

use ratatui::style::Modifier;

#[test]
fn the_selected_span_is_highlighted_and_the_rest_is_not() {
    let rows = [marked(0, "alpha", 0..3, 3)];

    let buffer = draw_to(6, 1, Body::Rows(&rows));

    for x in 0..3 {
        assert!(
            buffer[(x, 0)]
                .style()
                .add_modifier
                .contains(Modifier::REVERSED),
            "cell {x} should be selected"
        );
    }
    assert!(
        !buffer[(4, 0)]
            .style()
            .add_modifier
            .contains(Modifier::REVERSED)
    );
}

#[test]
fn a_bare_point_renders_as_a_visible_block() {
    // `meow--selection-type` answering nil: an ordinary motion with nothing
    // selected, which is most of editing. Before this, a cursor with no
    // surrounding selection got no styling at all — invisible.
    let rows = [Row {
        line: 0,
        text: "alpha".to_string(),
        selected: None,
        cursor: Some(2),
        current: false,
    }];

    let buffer = draw_to(6, 1, Body::Rows(&rows));

    assert!(
        buffer[(2, 0)]
            .style()
            .add_modifier
            .contains(Modifier::REVERSED),
        "the point itself is a visible block"
    );
    assert!(
        !buffer[(1, 0)]
            .style()
            .add_modifier
            .contains(Modifier::REVERSED),
        "nothing beside it is"
    );
}

#[test]
fn a_bare_point_at_end_of_line_is_still_a_visible_block() {
    // The end-of-line branch is separate code from the main loop above, and
    // a bare point there is a different case again from a selection ending
    // at the same spot — both take the `in_selection` fork, and this is the
    // one that must land on `false`.
    let rows = [Row {
        line: 0,
        text: "alpha".to_string(),
        selected: None,
        cursor: Some(5),
        current: false,
    }];

    let buffer = draw_to(7, 1, Body::Rows(&rows));

    assert!(
        buffer[(5, 0)]
            .style()
            .add_modifier
            .contains(Modifier::REVERSED),
        "the point past the last character is still a visible block"
    );
    assert!(
        !buffer[(5, 0)]
            .style()
            .add_modifier
            .contains(Modifier::UNDERLINED),
        "nothing selected reaches here to distinguish it from"
    );
}

#[test]
fn the_cursor_cell_is_distinguishable_from_the_rest_of_the_selection() {
    // Without this the two directions of the same span look identical.
    let rows = [marked(0, "alpha", 1..4, 1)];

    let buffer = draw_to(6, 1, Body::Rows(&rows));

    assert!(
        buffer[(1, 0)]
            .style()
            .add_modifier
            .contains(Modifier::UNDERLINED)
    );
    assert!(
        !buffer[(2, 0)]
            .style()
            .add_modifier
            .contains(Modifier::UNDERLINED)
    );
}

#[test]
fn a_cursor_past_the_end_of_the_text_still_renders() {
    let rows = [marked(0, "ab", 0..2, 2)];

    let buffer = draw_to(6, 1, Body::Rows(&rows));

    assert!(
        buffer[(2, 0)]
            .style()
            .add_modifier
            .contains(Modifier::UNDERLINED)
    );
}

#[test]
fn the_echo_area_draws_its_message() {
    let mut terminal =
        Terminal::new(TestBackend::new(40, 1)).expect("a test terminal is always available");
    terminal
        .draw(|frame| draw_echo(frame, frame.area(), "Unsaved: q.txt."))
        .expect("drawing to a test backend cannot fail");
    let rendered: String = terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|c| c.symbol())
        .collect();
    assert!(rendered.starts_with("Unsaved: q.txt."), "got {rendered:?}");
}

#[test]
fn every_highlight_name_has_a_colour() {
    // `colour_for` matches on `&str`, so the fallback arm has to exist and a
    // name nobody styled becomes an unstyled span rather than a compile
    // error. This is what makes the set closed in practice.
    //
    // `meowr-syntax` is a dev-dependency only. This layer draws plain data and
    // does not know what produced it; the test is the one place that has to
    // know both, and it is a test.
    // The ones that take the default foreground on purpose; see `colour_for`.
    const DEFAULT_FOREGROUND: &[&str] = &["variable"];
    let unstyled: Vec<&str> = meowr_syntax::HIGHLIGHT_NAMES
        .iter()
        .copied()
        .filter(|name| !DEFAULT_FOREGROUND.contains(name))
        .filter(|name| colour_for(name) == Style::default())
        .collect();
    assert!(
        unstyled.is_empty(),
        "highlight names with no colour: {unstyled:?}"
    );
}

#[test]
fn the_current_line_is_painted_to_the_window_edge() {
    // The reason it is a row flag rather than a `Styled` run: a run covers
    // columns that have a character in them, and the point of this highlight
    // is the band across the window. A background that stopped at the last
    // character would draw the eye to the text's shape instead.
    let mut rows = [row(0, "alpha"), row(1, "beta")];
    rows[1].current = true;

    let buffer = draw_to(8, 2, Body::Rows(&rows));

    let painted = |x: u16, y: u16| buffer[(x, y)].style().bg;
    let highlight = painted(0, 1);
    assert_ne!(highlight, painted(0, 0), "the other line is not painted");
    for x in 0..8 {
        assert_eq!(
            painted(x, 1),
            highlight,
            "column {x} of the current line, past the end of `beta`"
        );
    }
}

#[test]
fn the_current_line_keeps_what_is_drawn_over_it() {
    // The background goes *under*. A span that set its own — the point, drawn
    // reversed — keeps it, or the cursor would vanish on the highlighted line.
    let mut rows = [marked(0, "alpha", 0..3, 1)];
    rows[0].current = true;

    let buffer = draw_to(8, 1, Body::Rows(&rows));

    assert!(
        buffer[(1, 0)]
            .style()
            .add_modifier
            .contains(Modifier::UNDERLINED),
        "the point is still marked on the current line"
    );
}

#[test]
fn the_current_line_leaves_wide_characters_where_they_were() {
    // A CJK glyph is one `char` and two cells, so anything counting characters
    // to decide how far the row reaches is wrong by the number of them.
    //
    // Only the cells that carry a symbol are asserted. The other of each pair
    // is a *skip* cell: `Buffer::diff` never sends one to the backend, because
    // the glyph physically covers both columns and the terminal paints its
    // background across the pair. So a skip cell reading `Reset` here is what
    // TestBackend can represent rather than a hole in the band.
    let mut rows = [row(0, "日本語だ")];
    rows[0].current = true;

    let buffer = draw_to(8, 1, Body::Rows(&rows));

    let plain = draw_to(8, 1, Body::Rows(&[row(0, "日本語だ")]));
    for x in [0, 2, 4, 6] {
        assert_eq!(
            buffer[(x, 0)].symbol(),
            plain[(x, 0)].symbol(),
            "cell {x}: the glyph is where it was without the highlight"
        );
        assert_eq!(
            buffer[(x, 0)].style().bg,
            buffer[(0, 0)].style().bg,
            "cell {x} carries the band"
        );
    }
}

// --- the line-number gutter (#68) -------------------------------------------

fn draw_gutter_to(width: u16, height: u16, rows: &[Row], gutter: usize) -> Buffer {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal
        .draw(|frame| {
            draw_all(
                frame,
                frame.area(),
                Body::Rows(rows),
                &[],
                &[],
                Some(gutter),
            )
        })
        .unwrap();
    terminal.backend().buffer().clone()
}

#[test]
fn a_gutter_shows_one_based_line_numbers_right_aligned() {
    let rows = [row(0, "alpha"), row(1, "beta")];

    let buffer = draw_gutter_to(10, 3, &rows, 3);

    // Content only — the gutter's dimmed style is its own test below.
    let text: Vec<String> = (0..3)
        .map(|y| {
            (0..10)
                .map(|x| buffer[(x, y)].symbol().to_string())
                .collect()
        })
        .collect();
    assert_eq!(text, [" 1 alpha  ", " 2 beta   ", "          "]);
}

#[test]
fn the_gutter_follows_a_scrolled_viewport_rather_than_starting_at_one() {
    // `Row::line` is the document's own index, not the row's position on
    // screen — a window scrolled to the tenth line must show `10`, not `1`.
    let rows = [row(9, "ten"), row(10, "eleven")];

    let buffer = draw_gutter_to(10, 2, &rows, 3);

    let text: Vec<String> = (0..2)
        .map(|y| {
            (0..10)
                .map(|x| buffer[(x, y)].symbol().to_string())
                .collect()
        })
        .collect();
    assert_eq!(text, ["10 ten    ", "11 eleven "]);
}

#[test]
fn the_gutter_is_dimmed_and_the_current_line_is_bold_instead() {
    let mut current = row(1, "beta");
    current.current = true;
    let rows = [row(0, "alpha"), current];

    let buffer = draw_gutter_to(10, 2, &rows, 3);

    assert_eq!(buffer[(0, 0)].style().fg, Some(Color::DarkGray));
    assert_eq!(
        buffer[(0, 1)].style().fg,
        Some(Color::Reset),
        "no colour override"
    );
    assert!(buffer[(0, 1)].style().add_modifier.contains(Modifier::BOLD));
}

#[test]
fn no_gutter_argument_leaves_the_text_at_the_window_s_edge() {
    // The existing behaviour, unchanged for every caller that does not ask.
    let rows = [row(0, "alpha")];
    let mut terminal = Terminal::new(TestBackend::new(8, 2)).unwrap();

    terminal
        .draw(|frame| draw_all(frame, frame.area(), Body::Rows(&rows), &[], &[], None))
        .unwrap();

    assert_eq!(
        terminal.backend().buffer().clone(),
        Buffer::with_lines(["alpha   ", "        "])
    );
}

#[test]
fn a_message_body_ignores_the_gutter() {
    // A `Loading…`/error message is not text with lines of its own, so there
    // is nothing for a gutter to number.
    let mut terminal = Terminal::new(TestBackend::new(10, 2)).unwrap();
    terminal
        .draw(|frame| {
            draw_all(
                frame,
                frame.area(),
                Body::Message("Loading…"),
                &[],
                &[],
                Some(3),
            )
        })
        .unwrap();
    let buffer = terminal.backend().buffer().clone();

    assert_eq!(buffer, Buffer::with_lines(["Loading…  ", "          "]));
}

#[test]
fn a_gutter_wider_than_the_window_is_refused_rather_than_swallowing_the_text() {
    let rows = [row(0, "alpha")];

    let buffer = draw_gutter_to(4, 2, &rows, 10);

    // The text still draws; the gutter that could not fit is simply absent.
    assert_eq!(buffer, Buffer::with_lines(["alph", "    "]));
}

// --- hover (#116) ---------------------------------------------------------

fn hover_to(width: u16, height: u16, view: &meowr_view::HoverView) -> Buffer {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal
        .draw(|frame| draw_hover(frame, frame.area(), view))
        .unwrap();
    terminal.backend().buffer().clone()
}

fn text_of(buffer: &Buffer, row: u16) -> String {
    (0..buffer.area.width)
        .map(|x| buffer[(x, row)].symbol().to_string())
        .collect::<String>()
        .trim_end()
        .to_string()
}

#[test]
fn a_hover_reserves_one_row_per_line_it_has() {
    let view = meowr_view::hover("fn main()\nRuns it.", 40, 10);
    assert_eq!(hover_height(&view), 2);
}

#[test]
fn a_truncated_hover_takes_no_extra_row_for_saying_so() {
    // The sign goes *on* the last line rather than below it: a row spent
    // saying "there is more" is a row not spent showing it.
    let text = (1..=12)
        .map(|n| n.to_string())
        .collect::<Vec<_>>()
        .join("\n");
    let view = meowr_view::hover(&text, 40, 3);
    assert_eq!(hover_height(&view), 3);
}

#[test]
fn every_line_of_a_multi_line_hover_is_drawn() {
    let view = meowr_view::hover("fn main()\nRuns the program.", 40, 10);
    let buffer = hover_to(40, 2, &view);
    assert_eq!(text_of(&buffer, 0), "fn main()");
    assert_eq!(text_of(&buffer, 1), "Runs the program.");
}

#[test]
fn a_truncated_hover_shows_a_visible_sign_that_it_was() {
    let text = (1..=12)
        .map(|n| n.to_string())
        .collect::<Vec<_>>()
        .join("\n");
    let view = meowr_view::hover(&text, 40, 3);
    let buffer = hover_to(40, 3, &view);
    assert!(
        text_of(&buffer, 2).contains('…'),
        "the last row must say something was left out: {:?}",
        text_of(&buffer, 2)
    );
}

#[test]
fn an_untruncated_hover_carries_no_sign() {
    let view = meowr_view::hover("all of it", 40, 10);
    let buffer = hover_to(40, 1, &view);
    assert_eq!(text_of(&buffer, 0), "all of it");
}

#[test]
fn a_hover_in_no_rows_draws_nothing_rather_than_panicking() {
    // The same guard `draw_echo`, `draw_narrow` and `draw_leader` all keep: a
    // terminal can be resized to nothing between a frame being decided and
    // being drawn.
    let view = meowr_view::hover("something", 40, 10);
    let mut terminal = Terminal::new(TestBackend::new(40, 1)).unwrap();
    terminal
        .draw(|frame| {
            draw_hover(
                frame,
                Rect {
                    x: 0,
                    y: 0,
                    width: 40,
                    height: 0,
                },
                &view,
            )
        })
        .unwrap();
}
