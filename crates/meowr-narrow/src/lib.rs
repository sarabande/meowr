//! The narrowing widget: one mechanism for the picker and for completion.
//!
//! They are the same widget because ADR-0004 says nothing floats, and Corfu's
//! child frame was the only reason Emacs ever made them two mechanisms. What
//! actually differs between them is *where the input comes from* and *what
//! accepting does* — so those are a driver, and everything else is shared.
//!
//! Candidates grow **upward** from the Echo Area with the input line pinned at
//! the bottom, next to where the eyes already are.
//!
//! Matching is nucleo, which is orderless already: `Pattern::parse` splits the
//! input on whitespace into independent atoms, so `sel cal` matches
//! `selection-calculus` without either term being a prefix. It also gives
//! `'foo` for substring, `!foo` to negate, `^foo` for prefix and `foo$` for
//! suffix. None of that needed building.

use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config, Matcher, Utf32String};

/// How many candidate rows the widget may grow to.
///
/// Twelve: enough that the answer is usually on screen, few enough that the
/// buffer behind is still readable and the eye does not have to search.
pub const MAX_ROWS: usize = 12;

/// One thing that can be chosen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    /// What is matched against and inserted.
    pub text: String,
    /// Marginalia: shown right-aligned, never matched against. A path's
    /// directory, a buffer's mode, a line's number.
    pub annotation: Option<String>,
    /// Which source it came from, for `<`.
    pub source: SourceId,
}

impl Candidate {
    pub fn new(text: impl Into<String>, source: SourceId) -> Self {
        Self {
            text: text.into(),
            annotation: None,
            source,
        }
    }

    pub fn annotated(mut self, annotation: impl Into<String>) -> Self {
        self.annotation = Some(annotation.into());
        self
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SourceId(pub usize);

/// A named group of candidates, which `<` can narrow to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Source {
    pub id: SourceId,
    pub name: String,
    /// The key that selects it after `<`.
    pub key: char,
}

/// A candidate that matched, and how well.
///
/// Deliberately **without** the matched positions. Computing those costs
/// several times what scoring does, and they are only ever wanted for the
/// dozen rows actually on screen — asking for them per candidate per keystroke
/// took 3.7 seconds over ten thousand candidates, which is the opposite of
/// what this widget is for. [`Narrow::positions`] computes them on demand.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Match {
    pub index: usize,
    pub score: u32,
}

/// What the widget did, counted rather than timed.
///
/// Responsiveness is a property of how much work a keystroke causes, and that
/// is a number the machine cannot change. Elapsed time is not: the same code
/// took 5.6s and 12.1s a minute apart because a release build was using the
/// cores, and a test that reports the load average trains everyone to re-run
/// it rather than read it.
///
/// Each field is a regression that has actually happened or is one edit away:
/// rescoring more than once per keystroke, asking for matched positions per
/// candidate rather than per visible row, and re-converting the haystacks
/// instead of converting them once.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Work {
    /// Candidates scored against the pattern.
    pub scored: usize,
    /// Candidates whose matched positions were computed. Several times the
    /// cost of scoring, and only ever wanted for the rows on screen.
    pub located: usize,
    /// Candidate texts converted into the matcher's form.
    pub converted: usize,
}

/// Convert one candidate into the form the matcher wants, counting it.
///
/// A free function taking the counter rather than a method, because the only
/// conversion happens while `Narrow` is still being built. It exists so that
/// the count is produced *by converting* — an earlier version set `converted`
/// to `haystacks.len()` after the fact, which records a length and would have
/// gone on recording one however many times the haystacks were rebuilt.
fn convert(text: &str, work: &mut Work) -> Utf32String {
    work.converted += 1;
    Utf32String::from(text)
}

/// The widget's state.
pub struct Narrow {
    candidates: Vec<Candidate>,
    /// The candidates' text in the form the matcher wants, converted once
    /// rather than on every keystroke.
    haystacks: Vec<Utf32String>,
    sources: Vec<Source>,
    input: String,
    /// `Some` once `<` has picked one.
    narrowed: Option<SourceId>,
    matches: Vec<Match>,
    selected: usize,
    matcher: Matcher,
    /// What accepting means, and where the input comes from.
    driver: Driver,
    /// For completion: the byte range in the buffer the accepted text replaces.
    pub replacing: Option<std::ops::Range<usize>>,
    work: Work,
}

/// Which of the two things this widget is being right now.
///
/// The *only* difference between the picker and completion, which is the whole
/// argument for them being one widget.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Driver {
    /// Input is a line the user types; accepting opens or runs the choice.
    Picker,
    /// Input is the prefix already under the cursor; accepting replaces it.
    Completion,
    /// Input is a line and nothing else: no candidates, no sources, and
    /// accepting hands the line back to whatever asked for it.
    ///
    /// This is meowr's minibuffer. ADR-0004 says nothing floats, so a read has
    /// to be a Window or the Echo Area, and this widget was already a modal
    /// read holding a line of input — it takes every key while it is open, for
    /// the same reason Emacs's minibuffer does. A second read beside it would
    /// have duplicated that routing to gain a narrower type.
    Reading,
}

impl Narrow {
    pub fn new(driver: Driver, sources: Vec<Source>, candidates: Vec<Candidate>) -> Self {
        let mut work = Work::default();
        let mut widget = Self {
            haystacks: candidates
                .iter()
                .map(|c| convert(&c.text, &mut work))
                .collect(),
            candidates,
            sources,
            input: String::new(),
            narrowed: None,
            matches: Vec::new(),
            selected: 0,
            matcher: Matcher::new(Config::DEFAULT),
            driver,
            replacing: None,
            work,
        };
        widget.rematch();
        widget
    }

    /// Start completing the prefix at `range`, which is already typed.
    pub fn completing(
        candidates: Vec<Candidate>,
        prefix: &str,
        range: std::ops::Range<usize>,
    ) -> Self {
        let mut widget = Self::new(Driver::Completion, Vec::new(), candidates);
        widget.replacing = Some(range);
        widget.set_input(prefix.to_string());
        widget
    }

    /// A widget that reads a line and offers nothing.
    pub fn reading() -> Self {
        Self::new(Driver::Reading, Vec::new(), Vec::new())
    }

    pub fn driver(&self) -> Driver {
        self.driver
    }

    pub fn input(&self) -> &str {
        &self.input
    }

    pub fn sources(&self) -> &[Source] {
        &self.sources
    }

    /// The source `<` narrowed to, if any.
    pub fn narrowed(&self) -> Option<&Source> {
        let id = self.narrowed?;
        self.sources.iter().find(|s| s.id == id)
    }

    pub fn matches(&self) -> &[Match] {
        &self.matches
    }

    /// What this widget has done since it was built. See [`Work`].
    pub fn work(&self) -> Work {
        self.work
    }

    pub fn candidate(&self, index: usize) -> &Candidate {
        &self.candidates[index]
    }

    pub fn selected(&self) -> Option<&Candidate> {
        self.matches
            .get(self.selected)
            .map(|m| &self.candidates[m.index])
    }

    pub fn selected_index(&self) -> usize {
        self.selected
    }

    /// The rows to draw, nearest the input line first — the widget grows
    /// upward, so the selection sits next to what the user is typing.
    pub fn visible(&self) -> Vec<&Match> {
        let start = self.selected.saturating_sub(MAX_ROWS - 1);
        self.matches
            .iter()
            .skip(start)
            .take(MAX_ROWS)
            .collect::<Vec<_>>()
    }

    /// Put a different set of candidates behind the same input.
    ///
    /// What a picker whose source is a *question* needs (#144): the input is
    /// the question and stays, the answer is the candidates and is replaced.
    /// The selection goes back to the top, because the row that was selected
    /// belonged to the previous answer and the new one may not have it —
    /// keeping an index across two lists points at whatever happens to be
    /// there, which is worse than starting again.
    pub fn replace_candidates(&mut self, candidates: Vec<Candidate>) {
        // The haystacks go with them. They are the candidates' text converted
        // once rather than per keystroke, so a set of candidates without its
        // own haystacks is a list this cannot score -- and indexes into the
        // *previous* answer's, which is an out-of-bounds panic when the new
        // answer is longer and a wrong row when it is not.
        self.haystacks = candidates
            .iter()
            .map(|c| convert(&c.text, &mut self.work))
            .collect();
        self.candidates = candidates;
        self.selected = 0;
        self.rematch();
    }

    pub fn set_input(&mut self, input: String) {
        self.input = input;
        self.rematch();
    }

    pub fn push(&mut self, c: char) {
        self.input.push(c);
        self.rematch();
    }

    pub fn backspace(&mut self) {
        self.input.pop();
        self.rematch();
    }

    /// `<`: restrict to one source.
    pub fn narrow_to(&mut self, key: char) -> bool {
        let Some(source) = self.sources.iter().find(|s| s.key == key) else {
            return false;
        };
        self.narrowed = Some(source.id);
        // The input is cleared, because it was typed against a different set
        // and almost never means the same thing against this one.
        self.input.clear();
        self.rematch();
        true
    }

    pub fn widen(&mut self) {
        self.narrowed = None;
        self.rematch();
    }

    pub fn select_next(&mut self) {
        if !self.matches.is_empty() {
            self.selected = (self.selected + 1) % self.matches.len();
        }
    }

    pub fn select_previous(&mut self) {
        if !self.matches.is_empty() {
            self.selected = (self.selected + self.matches.len() - 1) % self.matches.len();
        }
    }

    /// Which characters of a candidate the pattern matched, for marking.
    ///
    /// On demand, for the rows being drawn. See [`Match`] for why this is not
    /// computed up front.
    pub fn positions(&mut self, index: usize) -> Vec<u32> {
        let pattern = Pattern::parse(&self.input, CaseMatching::Smart, Normalization::Smart);
        self.work.located += 1;
        let mut positions = Vec::new();
        pattern.indices(
            self.haystacks[index].slice(..),
            &mut self.matcher,
            &mut positions,
        );
        positions.sort_unstable();
        positions.dedup();
        positions
    }

    /// Take more candidates after the widget is already on screen.
    ///
    /// Every source used to be supplied before a picker opened, so candidates
    /// were fixed at construction. A **Project** is asked for lazily and answers
    /// late (#130), which makes this ADR-0009's "re-checked when it lands" for a
    /// *list* rather than for a sentence.
    ///
    /// The input and the narrowed source survive, because the user typed them
    /// and an answer arriving is no reason to undo that.
    pub fn extend(&mut self, candidates: impl IntoIterator<Item = Candidate>) {
        // Which candidate was selected, not which row: rematching reorders, and
        // a late answer that moved the selection out from under somebody
        // mid-navigation is the interruption ADR-0009 exists to avoid.
        let was = self.matches.get(self.selected).map(|found| found.index);
        for candidate in candidates {
            self.haystacks
                .push(convert(&candidate.text, &mut self.work));
            self.candidates.push(candidate);
        }
        self.rematch();
        if let Some(was) = was
            && let Some(now) = self.matches.iter().position(|found| found.index == was)
        {
            self.selected = now;
        }
    }

    /// Re-score every candidate against the input.
    ///
    /// Sorted by score and then by the candidate's own order, so equal scores
    /// come out stable rather than however the sort happened to land — a list
    /// that reshuffles under an unchanged score is unusable, because the thing
    /// you were about to press Enter on moves out from under you.
    fn rematch(&mut self) {
        let pattern = Pattern::parse(&self.input, CaseMatching::Smart, Normalization::Smart);
        let mut matches = Vec::new();
        for (index, candidate) in self.candidates.iter().enumerate() {
            if let Some(only) = self.narrowed
                && candidate.source != only
            {
                continue;
            }
            self.work.scored += 1;
            if let Some(score) = pattern.score(self.haystacks[index].slice(..), &mut self.matcher) {
                matches.push(Match { index, score });
            }
        }
        matches.sort_by(|a, b| b.score.cmp(&a.score).then(a.index.cmp(&b.index)));
        self.matches = matches;
        self.selected = 0;
    }
}

#[cfg(test)]
mod tests;
