//! Draws a meowr view to a ratatui frame.
//!
//! This crate owns no state and makes no decisions about what to show. It takes
//! the rows the view produced and puts them on a frame, so that everything
//! above it stays testable without a terminal.

use meowr_view::modeline::{Fitted, Kind as SegmentKind};
use meowr_view::{Bottom, Hint, HoverView, NarrowView, Row, Screen, Styled};
use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::Paragraph,
};

/// How many rows the Bottom takes off the frame.
///
/// One cascade, because there are two readers: the host reserves the rows
/// before it lays the Windows out, and [`paint`] draws into them. Two copies is
/// how a widget comes to be drawn in rows nothing reserved, or rows to be
/// reserved that nothing draws in -- and until this was written they were not
/// merely two copies but two copies in two crates, so a fifth member of
/// [`Bottom`] needed finding twice.
pub fn reserved(
    bottom: Bottom,
    narrow: Option<&NarrowView>,
    menu: usize,
    hover: Option<&HoverView>,
) -> u16 {
    match bottom {
        Bottom::Narrow => narrow.map(narrow_height).unwrap_or(0),
        Bottom::Menu => leader_height(menu),
        Bottom::Hover => hover.map(hover_height).unwrap_or(0),
        Bottom::Echo => 1,
        Bottom::None => 0,
    }
}

/// Paint a [`Screen`], and nothing else.
///
/// Takes the model and no session, which is not an accident: it is what makes
/// "nothing is drawn that the comparison did not see" true by construction
/// rather than by care -- #127. A new thing on screen has to become a field of
/// `Screen` before it can be painted, and a field is compared.
///
/// Here rather than in the binary, which owns the terminal and delegates
/// everything it draws. It is the one function in this crate that draws a
/// *whole* frame; the rest draw a part of one and this is what says where each
/// part goes.
pub fn paint(frame: &mut Frame, screen: &Screen) {
    for window in &screen.windows {
        let rect = Rect {
            x: window.area.x,
            y: window.area.y,
            width: window.area.width,
            height: window.area.height,
        };
        // The text gets every row but the last; the modeline takes that one.
        // Not an overlay — ADR-0004.
        let text = Rect {
            height: rect.height.saturating_sub(1),
            ..rect
        };
        let body = match &window.message {
            Some(message) => Body::Message(message),
            None => Body::Rows(&window.rows),
        };
        draw_all(
            frame,
            text,
            body,
            &window.hints,
            &window.styled,
            window.gutter,
        );
        draw_modeline(frame, rect, &window.modeline, window.focused);
    }
    let whole = frame.area();
    let rows = |height: u16| Rect {
        y: whole.height.saturating_sub(height),
        height,
        ..whole
    };
    // The same rows the host reserved, asked of the same function, so the two
    // cannot disagree about how many there are.
    let at = rows(reserved(
        screen.bottom,
        screen.narrow.as_ref(),
        screen.menu.len(),
        screen.hover.as_ref(),
    ));
    match (&screen.bottom, &screen.narrow, &screen.hover, &screen.echo) {
        (Bottom::Menu, ..) => draw_leader(frame, at, &screen.prefix, &screen.menu),
        (Bottom::Hover, _, Some(view), _) => draw_hover(frame, at, view),
        (Bottom::Echo, .., Some(message)) => draw_echo(frame, at, message),
        (Bottom::Narrow, Some(view), ..) => draw_narrow(frame, at, view),
        _ => {}
    }
}

/// What the window is showing.
///
/// A message is not an error path: the document is loaded off the event loop,
/// so "loading" is an ordinary thing to render.
#[derive(Debug, Clone, Copy)]
pub enum Body<'a> {
    Rows(&'a [Row]),
    Message(&'a str),
}

/// Draw `body` into `area`.
///
/// Lines are clipped, never wrapped: a wrapped line would make the row on
/// screen stop corresponding to the line in the document, which every later
/// slice — the modeline, the selection, the beacons — depends on.
pub fn draw(frame: &mut Frame, area: Rect, body: Body<'_>) {
    draw_with_hints(frame, area, body, &[]);
}

/// As [`draw`], plus the numbered expand hints painted over the text.
///
/// The hints replace the character under them rather than being inserted
/// beside it, so the columns below a hint do not shift. Losing one character
/// for a second is the cheaper of the two confusions.
pub fn draw_with_hints(frame: &mut Frame, area: Rect, body: Body<'_>, hints: &[Hint]) {
    draw_all(frame, area, body, hints, &[], None);
}

/// The whole picture: rows, syntax colour, and the expand hints over the top.
///
/// The order is deliberate. Colour goes on first because it says what the text
/// *is*; the selection and the point are drawn by `line_for` over it because
/// they say where you are; hints go last because they are transient and must
/// be legible against whatever is underneath.
pub fn draw_all(
    frame: &mut Frame,
    area: Rect,
    body: Body<'_>,
    hints: &[Hint],
    styled: &[Styled],
    gutter: Option<usize>,
) {
    let (gutter_area, text_area) = split_gutter(area, body, gutter);
    let text = match body {
        Body::Rows(rows) => {
            let mut lines: Vec<Line> = rows.iter().map(line_for).collect();
            for run in styled {
                if let Some(line) = lines.get_mut(run.row) {
                    *line = with_colour(line, run);
                }
            }
            for hint in hints {
                if let Some(line) = lines.get_mut(hint.row) {
                    *line = with_hint(line, hint);
                }
            }
            Text::from(lines)
        }
        Body::Message(message) => Text::from(message),
    };
    frame.render_widget(Paragraph::new(text), text_area);
    if let Body::Rows(rows) = body {
        paint_current(frame, text_area, rows);
        if let Some(gutter_area) = gutter_area {
            draw_gutter(frame, gutter_area, rows);
        }
    }
}

/// Split `area` into a gutter column and the text area beside it, or hand
/// `area` back whole when there is nothing to draw one over.
///
/// Refused rather than clamped when it does not fit (`area.width <= width`):
/// a gutter with no room left for text would draw numbers over an empty
/// window, which is a worse failure than a gutter that briefly is not there.
/// A message body never gets one — it is not text with lines of its own.
fn split_gutter(area: Rect, body: Body<'_>, gutter: Option<usize>) -> (Option<Rect>, Rect) {
    let Body::Rows(_) = body else {
        return (None, area);
    };
    match gutter.and_then(|width| u16::try_from(width).ok()) {
        Some(width) if width > 0 && width < area.width => (
            Some(Rect { width, ..area }),
            Rect {
                x: area.x + width,
                width: area.width - width,
                ..area
            },
        ),
        _ => (None, area),
    }
}

/// The gutter itself: one right-aligned, one-based line number per row.
///
/// Dimmed against the ordinary text so it reads as a margin rather than a
/// fourth column of content; the row the point is on keeps the terminal's
/// own foreground and goes bold, `line-number-current-line`'s whole effect.
fn draw_gutter(frame: &mut Frame, area: Rect, rows: &[Row]) {
    let width = usize::from(area.width);
    let lines: Vec<Line> = rows
        .iter()
        .map(|row| {
            let number = (row.line + 1).to_string();
            let text = format!("{number:>pad$} ", pad = width.saturating_sub(1));
            let style = if row.current {
                Style::default().add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::DarkGray)
            };
            Line::from(Span::styled(text, style))
        })
        .collect();
    frame.render_widget(Paragraph::new(Text::from(lines)), area);
}

/// `hl-line`: a background across the row the point is on.
///
/// Painted into the buffer *after* the text rather than built into the line,
/// and both halves of that are load-bearing.
///
/// After, because it reaches past the end of the text to the window's edge —
/// that band is what the highlight is *for*, and a `Line` only occupies the
/// cells its spans fill.
///
/// Into the buffer, because a double-width glyph takes two cells and ratatui
/// writes the symbol into the first, leaving the second a skip cell that
/// neither a span's style nor the line's own reaches. Padding spans left the
/// band striped wherever CJK text sat on the current line.
///
/// Only where a cell has no background of its own, so anything that set one
/// deliberately keeps it. Colour and the point do not: syntax sets a
/// foreground and the point is drawn `REVERSED`, so both survive with the
/// band behind them.
fn paint_current(frame: &mut Frame, area: Rect, rows: &[Row]) {
    // doom-one's `bg-alt`, which is what its `hl-line` face uses: a shade
    // *darker* than the buffer background rather than lighter. The first
    // version of this was `#2E3440`, which is Nord's polar night and was in
    // the wrong palette entirely.
    let background = ratatui::style::Color::Rgb(0x21, 0x24, 0x2B);
    let buffer = frame.buffer_mut();
    for (at, _) in rows.iter().enumerate().filter(|(_, row)| row.current) {
        let Ok(at) = u16::try_from(at) else { continue };
        let y = area.y + at;
        if y >= area.y + area.height {
            continue;
        }
        for x in area.x..area.x + area.width {
            let cell = &mut buffer[(x, y)];
            if cell
                .style()
                .bg
                .is_none_or(|bg| bg == ratatui::style::Color::Reset)
            {
                cell.set_bg(background);
            }
        }
    }
}

/// Colour one run of a rendered line.
///
/// Applied under the selection and the point, which `line_for` has already
/// put on: a coloured keyword inside a selection must still look selected.
fn with_colour<'a>(line: &Line<'a>, run: &Styled) -> Line<'a> {
    let colour = colour_for(run.name);
    let mut column = 0;
    let mut spans: Vec<Span> = Vec::new();
    for span in &line.spans {
        for c in span.content.chars() {
            let inside = run.columns.contains(&column);
            spans.push(Span::styled(
                c.to_string(),
                if inside {
                    span.style.patch(colour)
                } else {
                    span.style
                },
            ));
            column += 1;
        }
    }
    Line::from(spans)
}

/// The doom-one palette, by highlight name.
///
/// A `match` rather than a map because the set is closed: `meowr-syntax` will
/// not hand back a name outside `HIGHLIGHT_NAMES`. The arms are checked
/// against that list by a test rather than by the compiler — the fallback arm
/// has to exist for `&str`, and it would otherwise swallow a new name as an
/// unstyled span nobody notices.
fn colour_for(name: &str) -> Style {
    use ratatui::style::Color;
    let colour = match name {
        "comment" => Color::Rgb(0x5B, 0x62, 0x68),
        "keyword" => Color::Rgb(0x51, 0xAF, 0xEF),
        "string" | "string.special" => Color::Rgb(0x98, 0xBE, 0x65),
        "number" | "constant" | "constant.builtin" => Color::Rgb(0xDA, 0x85, 0x48),
        "function" | "function.builtin" | "function.macro" => Color::Rgb(0xC6, 0x7B, 0xE5),
        "type" | "type.builtin" | "constructor" => Color::Rgb(0xEC, 0xBE, 0x7B),
        "variable.builtin" | "variable.parameter" => Color::Rgb(0xDC, 0xAE, 0xEA),
        // A plain variable keeps the default foreground, which is doom-one's
        // own choice: colouring every identifier makes a buffer shout. Spelled
        // out rather than left to the fallback arm, so that it reads as a
        // decision and not as a name someone forgot.
        "variable" => return Style::default(),
        "attribute" | "label" | "namespace" | "tag" => Color::Rgb(0xA9, 0xA1, 0xE1),
        "property" => Color::Rgb(0xEF, 0x9F, 0x76),
        // Markup. Emphasis and strength are *shapes* rather than colours —
        // that is what they mean in the text — so they return early with a
        // modifier and no foreground, and the reader sees the same distinction
        // the author typed.
        "text.strong" => return Style::default().add_modifier(Modifier::BOLD),
        "text.emphasis" => return Style::default().add_modifier(Modifier::ITALIC),
        "text.title" => {
            return Style::default()
                .fg(Color::Rgb(0x51, 0xAF, 0xEF))
                .add_modifier(Modifier::BOLD);
        }
        "text.uri" => {
            return Style::default()
                .fg(Color::Rgb(0x51, 0xAF, 0xEF))
                .add_modifier(Modifier::UNDERLINED);
        }
        // Inline code, the same green as a string: it is quoted text.
        "text.literal" => Color::Rgb(0x98, 0xBE, 0x65),
        "text.reference" => Color::Rgb(0xA9, 0xA1, 0xE1),
        "operator" | "punctuation" | "punctuation.bracket" | "punctuation.delimiter" => {
            Color::Rgb(0x96, 0xA7, 0xB7)
        }
        // The Grab: an underline rather than a colour, so it reads as a second
        // marking on top of whatever the syntax already said, and cannot be
        // mistaken for the selection.
        "grab" => return Style::default().add_modifier(Modifier::UNDERLINED),
        // A diagnostic is an *underline in a colour* — the nearest a terminal
        // gets to a squiggle, and the reason severity reaches this layer as
        // four names rather than one: an error and a hint must not look the
        // same. Underlined rather than a background so the syntax colour it
        // sits on top of survives, which is the same argument the Grab makes.
        "diagnostic.error" => {
            return Style::default()
                .fg(Color::Rgb(0xFF, 0x6C, 0x6B))
                .add_modifier(Modifier::UNDERLINED);
        }
        "diagnostic.warning" => {
            return Style::default()
                .fg(Color::Rgb(0xEC, 0xBE, 0x7B))
                .add_modifier(Modifier::UNDERLINED);
        }
        "diagnostic.info" => {
            return Style::default()
                .fg(Color::Rgb(0x51, 0xAF, 0xEF))
                .add_modifier(Modifier::UNDERLINED);
        }
        "diagnostic.hint" => {
            return Style::default()
                .fg(Color::Rgb(0x98, 0xBE, 0x65))
                .add_modifier(Modifier::UNDERLINED);
        }
        _ => return Style::default(),
    };
    Style::default().fg(colour)
}

/// Overlay one hint's digit on a rendered line.
///
/// Rebuilt character by character because the line is already split into
/// styled spans by the selection, and the hint may land in the middle of one.
///
/// A hint whose column is past the end of the text is *appended* rather than
/// dropped. That is not an edge case: a word motion lands on the newline at
/// the end of every line it crosses, so dropping them loses roughly one hint
/// per line — which a PTY run showed as a gap in the numbering rather than as
/// anything that looked like a bug.
fn with_hint<'a>(line: &Line<'a>, hint: &Hint) -> Line<'a> {
    let label = char::from_digit(hint.label % 10, 10).unwrap_or('?');
    let marked = Style::default().add_modifier(Modifier::BOLD | Modifier::REVERSED);
    let mut column = 0;
    let mut spans: Vec<Span> = Vec::new();
    for span in &line.spans {
        for c in span.content.chars() {
            let hit = column == hint.column;
            spans.push(Span::styled(
                if hit { label } else { c }.to_string(),
                if hit { marked } else { span.style },
            ));
            column += 1;
        }
    }
    for _ in column..hint.column {
        spans.push(Span::raw(" "));
    }
    if column <= hint.column {
        spans.push(Span::styled(label.to_string(), marked));
    }
    Line::from(spans)
}

/// Render one row, highlighting its selection and marking the point.
///
/// The point is styled separately from the rest of the selection because the
/// span alone cannot tell you which end you are at, and Meow's selections have
/// a direction that changes what the next motion does.
fn line_for(row: &Row) -> Line<'_> {
    let selected = row.selected.clone().unwrap_or(0..0);
    let cursor = row.cursor;

    // Boundaries at every point where the style can change, so a row is at
    // most a handful of spans rather than one per character.
    let mut cuts = vec![0, row.text.len()];
    cuts.extend([selected.start, selected.end]);
    if let Some(at) = cursor {
        cuts.extend([at, next_boundary(&row.text, at)]);
    }
    cuts.retain(|&at| at <= row.text.len() && row.text.is_char_boundary(at));
    cuts.sort_unstable();
    cuts.dedup();

    let mut spans: Vec<Span<'_>> = Vec::new();
    for pair in cuts.windows(2) {
        let (from, to) = (pair[0], pair[1]);
        let mut style = Style::new();
        let in_selection =
            from >= selected.start && to <= selected.end && selected.start < selected.end;
        if in_selection {
            style = style.add_modifier(Modifier::REVERSED);
        }
        // The point is always its own visible block — `REVERSED` on its own
        // cell, selection or none. Where it sits inside a wider selection
        // that is *already* reversed, `UNDERLINED` is what still tells the
        // two directions of the same span apart.
        if cursor == Some(from) {
            style = style.add_modifier(Modifier::REVERSED);
            if in_selection {
                style = style.add_modifier(Modifier::UNDERLINED);
            }
        }
        spans.push(Span::styled(&row.text[from..to], style));
    }

    // A point at the end of the line has no character to sit on, so it gets a
    // space of its own; otherwise the cursor would vanish at end of line.
    if cursor == Some(row.text.len()) {
        let in_selection = selected.start < selected.end && row.text.len() >= selected.start;
        let mut style = Style::new().add_modifier(Modifier::REVERSED);
        if in_selection {
            style = style.add_modifier(Modifier::UNDERLINED);
        }
        spans.push(Span::styled(" ", style));
    }
    Line::from(spans)
}

fn next_boundary(text: &str, at: usize) -> usize {
    (at + 1..=text.len())
        .find(|&i| text.is_char_boundary(i))
        .unwrap_or(text.len())
}

#[cfg(test)]
mod tests;

/// Draw a window's modeline along the bottom row of its area.
///
/// Not an overlay: the row belongs to the window, which is why every viewport
/// is one shorter than the area it sits in. ADR-0004 has no floating anything,
/// and a status line is the first thing most editors float.
pub fn draw_modeline(frame: &mut Frame, area: Rect, line: &Fitted, focused: bool) {
    if area.height == 0 {
        return;
    }
    let row = Rect {
        y: area.y + area.height - 1,
        height: 1,
        ..area
    };
    fn push(
        spans: &mut Vec<Span<'static>>,
        segments: &[meowr_view::modeline::Segment],
        focused: bool,
    ) {
        for (index, segment) in segments.iter().enumerate() {
            if index > 0 {
                spans.push(Span::raw(" "));
            }
            spans.push(Span::styled(
                segment.text.clone(),
                segment_style(segment.kind, focused),
            ));
        }
    }
    let mut spans: Vec<Span<'static>> = Vec::new();
    push(&mut spans, &line.left, focused);
    spans.push(Span::raw(" ".repeat(line.gap)));
    push(&mut spans, &line.right, focused);

    // An unfocused window's modeline is dimmed rather than hidden: you still
    // need to read which buffer it is, but not to mistake it for the one your
    // keys are going to.
    let background = if focused {
        Style::default().bg(ratatui::style::Color::Rgb(0x2A, 0x2E, 0x38))
    } else {
        Style::default()
            .bg(ratatui::style::Color::Rgb(0x21, 0x24, 0x2B))
            .add_modifier(Modifier::DIM)
    };
    frame.render_widget(Paragraph::new(Line::from(spans)).style(background), row);
}

fn segment_style(kind: SegmentKind, focused: bool) -> Style {
    use ratatui::style::Color;
    if !focused {
        return Style::default().fg(Color::Rgb(0x5B, 0x62, 0x68));
    }
    match kind {
        // The State is the one segment you look for without meaning to, so it
        // gets the only reversed block on the line.
        SegmentKind::State => Style::default()
            .fg(Color::Rgb(0x21, 0x24, 0x2B))
            .bg(Color::Rgb(0x51, 0xAF, 0xEF))
            .add_modifier(Modifier::BOLD),
        SegmentKind::Buffer => Style::default().fg(Color::Rgb(0xBB, 0xC2, 0xCF)),
        SegmentKind::Selection => Style::default().fg(Color::Rgb(0xC6, 0x7B, 0xE5)),
        SegmentKind::Vcs => Style::default().fg(Color::Rgb(0x98, 0xBE, 0x65)),
        SegmentKind::Position | SegmentKind::Major => {
            Style::default().fg(Color::Rgb(0x96, 0xA7, 0xB7))
        }
        // The one segment that says something is wrong, in the same red
        // `draw_echo` uses for a refusal — the two mean the same thing to a
        // reader glancing at the bottom of the frame.
        SegmentKind::Diagnostics => Style::default()
            .fg(Color::Rgb(0xE0, 0x6C, 0x75))
            .add_modifier(Modifier::BOLD),
        SegmentKind::Encoding | SegmentKind::Misc => {
            Style::default().fg(Color::Rgb(0x5B, 0x62, 0x68))
        }
    }
}

/// The colour a refusal is written in: the red the syntax theme gives an error.
///
/// Named rather than written twice, because there are two places a message can
/// go now. The Echo Area is one; the other is the narrowing widget's own note,
/// which exists because ADR-0004 gives the Bottom exactly one owner and an open
/// picker *is* that owner -- so a widget has to say its own refusals. Two
/// literals would have drifted the day either was adjusted, and the same red is
/// the point: it reads as a refusal wherever it lands.
const REFUSAL_RED: Color = Color::Rgb(0xE0, 0x6C, 0x75);

/// Draw the narrowing widget in the Echo Area, growing upward.
///
/// The input line is pinned to the bottom row of the frame and the candidates
/// stack *above* it, so the selection sits next to what is being typed rather
/// than a dozen rows away. Nothing floats — ADR-0004 — so the widget takes the
/// bottom of the frame and the windows above it are drawn shorter.
pub fn draw_narrow(frame: &mut Frame, area: Rect, view: &NarrowView) {
    use ratatui::style::Color;
    if area.height == 0 {
        return;
    }
    let mut lines: Vec<Line> = Vec::new();
    // Reversed: `rows` is nearest-first, and nearest means lowest.
    for row in view.rows.iter().rev() {
        let mut spans: Vec<Span> = Vec::new();
        let base = if row.selected {
            Style::default()
                .fg(Color::Rgb(0xBB, 0xC2, 0xCF))
                .bg(Color::Rgb(0x3F, 0x44, 0x4A))
        } else {
            Style::default().fg(Color::Rgb(0x96, 0xA7, 0xB7))
        };
        spans.push(Span::styled(if row.selected { "▸ " } else { "  " }, base));
        for (index, c) in row.text.chars().enumerate() {
            // The matched characters are marked, which is what makes a fuzzy
            // match legible: without it the user cannot tell why a candidate
            // is in the list.
            let style = if row.matched.contains(&(index as u32)) {
                base.fg(Color::Rgb(0x51, 0xAF, 0xEF))
                    .add_modifier(Modifier::BOLD)
            } else {
                base
            };
            spans.push(Span::styled(c.to_string(), style));
        }
        if let Some(annotation) = &row.annotation {
            let used: usize = row.text.chars().count() + 2;
            let width = area.width as usize;
            let pad = width.saturating_sub(used + annotation.chars().count() + 1);
            spans.push(Span::styled(" ".repeat(pad + 1), base));
            spans.push(Span::styled(
                annotation.clone(),
                base.fg(Color::Rgb(0x5B, 0x62, 0x68)),
            ));
        }
        lines.push(Line::from(spans));
    }
    // Immediately above the prompt: the widget grows upward, so this is the
    // line nearest what the user is typing rather than a dozen rows away.
    if let Some(note) = &view.note {
        lines.push(Line::from(Span::styled(
            format!("  {note}"),
            Style::default()
                .fg(REFUSAL_RED)
                .add_modifier(Modifier::ITALIC),
        )));
    }
    lines.push(Line::from(vec![
        Span::styled(
            view.prompt.clone(),
            Style::default()
                .fg(Color::Rgb(0xC6, 0x7B, 0xE5))
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            view.input.clone(),
            Style::default().fg(Color::Rgb(0xBB, 0xC2, 0xCF)),
        ),
        Span::styled("_", Style::default().add_modifier(Modifier::REVERSED)),
    ]));
    frame.render_widget(
        Paragraph::new(Text::from(lines)).style(Style::default().bg(Color::Rgb(0x21, 0x24, 0x2B))),
        area,
    );
}

/// How many rows the widget needs, given how many candidates it has to show.
pub fn narrow_height(view: &NarrowView) -> u16 {
    view.rows.len() as u16 + 1 + u16::from(view.note.is_some())
}

/// Draw one line in the Echo Area.
///
/// Not a Side Window and not an overlay — the glossary keeps those apart, and
/// ADR-0004 leaves nowhere for a message to float. The Echo Area is one of the
/// four things that can own the Bottom, and the only one that is a single row:
/// the narrowing widget and the leader menu take several, and a hover is this
/// same row grown to fit.
///
/// Red because a message here is almost always a refusal -- [`REFUSAL_RED`], which
/// is the same red the syntax theme gives an error.
pub fn draw_echo(frame: &mut Frame, area: Rect, message: &str) {
    if area.height == 0 {
        return;
    }
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            message.to_string(),
            Style::default().fg(REFUSAL_RED),
        ))),
        area,
    );
}

/// Draw the leader menu as a bottom side-window.
///
/// A side-window, not a floating box: ADR-0004, and the specific complaint
/// that made this project exist. It takes rows from the bottom of the frame
/// and the windows above are drawn shorter, exactly as the Echo Area widget
/// does.
/// The keys arrive already written the way a keymap reference writes them —
/// `f`, `C-f` — because a leader key is a chord and this layer draws text.
pub fn draw_leader(
    frame: &mut Frame,
    area: Rect,
    prefix: &str,
    entries: &[(String, String, bool)],
) {
    use ratatui::style::Color;
    if area.height == 0 {
        return;
    }
    // Three columns, because a leader menu is read by scanning for a key
    // rather than by reading down, and a single column of twenty entries
    // pushes the buffer off the screen for no gain.
    let columns = 3usize;
    let width = (area.width as usize / columns).max(12);
    let mut lines: Vec<Line> = Vec::new();
    lines.push(Line::from(Span::styled(
        format!("SPC {prefix}"),
        Style::default()
            .fg(Color::Rgb(0xC6, 0x7B, 0xE5))
            .add_modifier(Modifier::BOLD),
    )));
    for row in entries.chunks(columns) {
        let mut spans: Vec<Span> = Vec::new();
        for (key, description, reserved) in row {
            let style = if *reserved {
                // "Not yet" and "not a thing" are different answers, and the
                // menu must not conflate them.
                Style::default()
                    .fg(Color::Rgb(0x5B, 0x62, 0x68))
                    .add_modifier(Modifier::ITALIC)
            } else {
                Style::default().fg(Color::Rgb(0xBB, 0xC2, 0xCF))
            };
            let cell = format!("{key} {description}");
            let cell: String = cell.chars().take(width - 1).collect();
            spans.push(Span::styled(
                format!("{:<width$}", cell, width = width),
                style,
            ));
        }
        lines.push(Line::from(spans));
    }
    frame.render_widget(
        Paragraph::new(Text::from(lines)).style(Style::default().bg(Color::Rgb(0x21, 0x24, 0x2B))),
        area,
    );
}

/// How many rows the leader menu needs.
pub fn leader_height(entries: usize) -> u16 {
    (entries.div_ceil(3) + 1) as u16
}

/// Draw a hover in the Echo Area, grown to fit.
///
/// The same subtraction the leader menu and the narrowing widget make: rows
/// come off the bottom of the frame and the windows above are drawn shorter.
/// Nothing floats — ADR-0004 — which is why `lsp-ui-doc`'s popup is exactly
/// the casualty that decision named.
///
/// The truncation sign is appended to the last line rather than given a row of
/// its own: a row spent saying "there is more" is a row not spent showing it.
pub fn draw_hover(frame: &mut Frame, area: Rect, view: &meowr_view::HoverView) {
    use ratatui::style::Color;
    if area.height == 0 || view.lines.is_empty() {
        return;
    }
    let last = view.lines.len().saturating_sub(1);
    let lines: Vec<Line> = view
        .lines
        .iter()
        .enumerate()
        .map(|(index, text)| {
            let mut spans = vec![Span::styled(
                text.clone(),
                Style::default().fg(Color::Rgb(0xBB, 0xC2, 0xCF)),
            )];
            if view.truncated && index == last {
                spans.push(Span::styled(
                    " …",
                    Style::default()
                        .fg(Color::Rgb(0x5B, 0x62, 0x68))
                        .add_modifier(Modifier::BOLD),
                ));
            }
            Line::from(spans)
        })
        .collect();
    frame.render_widget(
        Paragraph::new(Text::from(lines)).style(Style::default().bg(Color::Rgb(0x21, 0x24, 0x2B))),
        area,
    );
}

/// How many rows a hover needs.
///
/// One per line and none for the truncation sign, which rides on the last one.
pub fn hover_height(view: &meowr_view::HoverView) -> u16 {
    view.lines.len() as u16
}
