//! The modeline, as plain data.
//!
//! One per window, because a modeline describes *a window's* buffer — with two
//! windows on one file, the point and the selection differ between them, and a
//! single shared line could only be right about one.
//!
//! Segments are data rather than a fixed list, decided in #9: it costs nothing
//! now and it is what lets Steel add one in M2 without this file changing.
//! Each carries a **priority** and a **minimum width**, because truncation is
//! the part of a status line that is always decided badly by accident. Whole
//! segments drop, by ascending priority, until the line fits; nothing is
//! clipped mid-word except the buffer name, which knows how to shorten itself.

use std::fmt::Write as _;

/// Where a segment sits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    Left,
    Right,
}

/// What a segment is about, so the render layer can colour it without parsing
/// the text back.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// The Meow State. Always visible: it decides what every key does, so a
    /// modeline that dropped it would be worse than no modeline.
    State,
    Buffer,
    Position,
    Selection,
    Vcs,
    Encoding,
    Major,
    /// How many diagnostics the buffer has. Its own kind rather than `Misc`
    /// because it is the one segment that means *something is wrong*, and the
    /// render layer has to be able to colour it accordingly.
    Diagnostics,
    Misc,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Segment {
    pub kind: Kind,
    pub side: Side,
    pub text: String,
    /// Lower drops first. The State is `u8::MAX` and never drops.
    pub priority: u8,
    /// Below this width the segment is not worth showing at all.
    pub min_width: usize,
}

/// What a window's modeline says, before it is fitted to a width.
#[derive(Debug, Clone, Default)]
pub struct Modeline {
    pub segments: Vec<Segment>,
}

/// The information a modeline is built from.
///
/// A plain struct rather than a borrow of the session: the view layer stays
/// ignorant of buffers and windows, and a test can build one by hand.
#[derive(Debug, Clone)]
pub struct Status<'a> {
    pub state: &'a str,
    /// How many positions a digit could expand to, if any. Meow shows this and
    /// the reference configuration leaves it up for four seconds, so it is
    /// something the user reads rather than something that flickers.
    pub expand_hints: usize,
    pub path: Option<&'a str>,
    pub buffer_name: &'a str,
    pub modified: bool,
    pub read_only: bool,
    pub line: usize,
    pub lines: usize,
    pub column: usize,
    /// The selection's axis and kind, e.g. `("expand", "line")`.
    pub selection: Option<(&'a str, &'a str)>,
    /// How many lines and characters it covers.
    pub selection_lines: usize,
    pub selection_chars: usize,
    pub branch: Option<&'a str>,
    pub encoding: &'a str,
    pub language: Option<&'a str>,
    /// How many diagnostics the buffer holds. Reported the way the selection's
    /// size already is, and absent entirely at zero: a `0!` on every clean
    /// buffer is noise, and the count exists to be noticed.
    pub diagnostics: usize,
    pub focused: bool,
}

/// How many path segments a buffer name carries.
///
/// `uniquify-buffer-name-style 'forward` with `min-dir-content 3`, from the
/// reference configuration.
pub const PATH_SEGMENTS: usize = 3;

/// The last [`PATH_SEGMENTS`] components of a path.
pub fn short_path(path: &str, segments: usize) -> String {
    let parts: Vec<&str> = path.split('/').filter(|p| !p.is_empty()).collect();
    let take = parts.len().min(segments);
    parts[parts.len() - take..].join("/")
}

/// Build the modeline for a window.
/// The modeline for a buffer a **Guest** owns.
///
/// Its segments, not meowr's: half of [`Status`] — line, column, selection,
/// encoding, language — describes text, and a Guest's buffer is not text. A
/// `wagit` status buffer wants the branch and its ahead/behind counts, which
/// only wagit can compute.
///
/// The State survives, first and undroppable, because it is the one thing on
/// that line which is still Meow's: it is how you know `SPC` is listening even
/// while a Guest has everything else.
pub fn modeline_for_guest(state: &str, mut segments: Vec<Segment>) -> Modeline {
    let state = state.to_uppercase();
    let mut all = vec![Segment {
        kind: Kind::State,
        side: Side::Left,
        min_width: state.chars().count(),
        text: state,
        priority: u8::MAX,
    }];
    all.append(&mut segments);
    Modeline { segments: all }
}

pub fn modeline(status: &Status<'_>) -> Modeline {
    let mut segments = Vec::new();

    // `modals`: the State, and the expand hint. Never dropped.
    let mut state = status.state.to_uppercase();
    if status.expand_hints > 0 {
        let _ = write!(state, " {}", status.expand_hints);
    }
    segments.push(Segment {
        kind: Kind::State,
        side: Side::Left,
        min_width: state.chars().count(),
        text: state,
        priority: u8::MAX,
    });

    // `buffer-info`: the name, carrying three path segments, with the
    // modified and read-only markers Emacs puts in front of it.
    let name = match status.path {
        Some(path) => short_path(path, PATH_SEGMENTS),
        None => status.buffer_name.to_string(),
    };
    let marker = match (status.read_only, status.modified) {
        (true, _) => "%% ",
        (false, true) => "* ",
        (false, false) => "",
    };
    segments.push(Segment {
        kind: Kind::Buffer,
        side: Side::Left,
        min_width: 8,
        text: format!("{marker}{name}"),
        priority: 250,
    });

    // `selection-info`: what Emacs's region cannot say. The *type* is the
    // point of it — with Meow's typed selections, what the next key does
    // depends on the axis and kind, so a character count alone would leave out
    // the half that matters.
    if let Some((axis, kind)) = status.selection {
        segments.push(Segment {
            kind: Kind::Selection,
            side: Side::Left,
            min_width: 10,
            text: format!(
                "{}L {}C ({axis}·{kind})",
                status.selection_lines, status.selection_chars
            ),
            priority: 200,
        });
    }

    // `buffer-position`.
    segments.push(Segment {
        kind: Kind::Position,
        side: Side::Right,
        min_width: 7,
        text: format!("{}:{}/{}", status.line + 1, status.column + 1, status.lines),
        priority: 240,
    });

    // Above the language and the encoding in priority: a count of problems is
    // worth more of a narrow line than the name of the mode is.
    if status.diagnostics > 0 {
        segments.push(Segment {
            kind: Kind::Diagnostics,
            side: Side::Right,
            min_width: 2,
            text: format!("{}!", status.diagnostics),
            priority: 190,
        });
    }

    if let Some(language) = status.language {
        segments.push(Segment {
            kind: Kind::Major,
            side: Side::Right,
            min_width: 3,
            text: language.to_string(),
            priority: 150,
        });
    }

    segments.push(Segment {
        kind: Kind::Encoding,
        side: Side::Right,
        min_width: 4,
        text: status.encoding.to_string(),
        priority: 100,
    });

    // `vcs`. `doom-modeline-vcs-max-length` is 100 in the reference
    // configuration, which is to say: never truncate the branch. So this
    // segment drops whole rather than shortening — a half-written branch name
    // is worse than none, because it can name a branch that exists.
    if let Some(branch) = status.branch {
        segments.push(Segment {
            kind: Kind::Vcs,
            side: Side::Right,
            min_width: branch.chars().count() + 2,
            text: format!(" {branch}"),
            priority: 180,
        });
    }

    Modeline { segments }
}

/// One modeline, fitted to `width`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fitted {
    pub left: Vec<Segment>,
    pub right: Vec<Segment>,
    /// Spaces between the two halves.
    pub gap: usize,
}

impl Fitted {
    /// The whole line as text, for tests and for a terminal that cannot
    /// colour.
    pub fn text(&self) -> String {
        let left = self
            .left
            .iter()
            .map(|s| s.text.as_str())
            .collect::<Vec<_>>()
            .join(" ");
        let right = self
            .right
            .iter()
            .map(|s| s.text.as_str())
            .collect::<Vec<_>>()
            .join(" ");
        format!("{left}{}{right}", " ".repeat(self.gap))
    }
}

/// Drop whole segments, lowest priority first, until the line fits.
///
/// Never clips mid-segment. A truncated `buffer-position` reads as a different
/// position and a truncated branch names a different branch; a missing segment
/// only says less.
pub fn fit(modeline: &Modeline, width: usize) -> Fitted {
    let mut kept: Vec<&Segment> = modeline.segments.iter().collect();
    kept.sort_by_key(|s| std::cmp::Reverse(s.priority));

    let mut chosen: Vec<&Segment> = Vec::new();
    for segment in kept {
        let mut candidate = chosen.clone();
        candidate.push(segment);
        if measure(&candidate) <= width || segment.priority == u8::MAX {
            chosen = candidate;
        }
    }

    // Back into the order they were declared in, which is the order they read.
    let mut left: Vec<Segment> = Vec::new();
    let mut right: Vec<Segment> = Vec::new();
    for segment in &modeline.segments {
        if chosen.iter().any(|c| std::ptr::eq(*c, segment)) {
            match segment.side {
                Side::Left => left.push(segment.clone()),
                Side::Right => right.push(segment.clone()),
            }
        }
    }
    Fitted {
        left,
        right,
        // The gap is whatever the two halves do not use, which is what pushes
        // the right half to the edge. `measure` adds a separator on top of
        // that so the halves cannot end up adjacent, but the separator is part
        // of the gap rather than extra to it — counting it twice made every
        // line one column short.
        gap: width.saturating_sub(content(&chosen)),
    }
}

/// Width of the text itself, with the single space between neighbours on the
/// same side.
fn content(segments: &[&Segment]) -> usize {
    let width = |side: Side| {
        let on_side: Vec<&&Segment> = segments.iter().filter(|s| s.side == side).collect();
        let text: usize = on_side.iter().map(|s| s.text.chars().count()).sum();
        text + on_side.len().saturating_sub(1)
    };
    width(Side::Left) + width(Side::Right)
}

/// What a set of segments needs to fit: the text, plus at least one space
/// between the two halves so they never touch.
fn measure(segments: &[&Segment]) -> usize {
    let any = |side: Side| segments.iter().any(|s| s.side == side);
    content(segments) + usize::from(any(Side::Left) && any(Side::Right))
}

#[cfg(test)]
mod tests;
