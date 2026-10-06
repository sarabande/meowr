//! Tree-sitter parsing and highlighting.
//!
//! Grammars are **linked into the binary**, not dlopened from a `.so` tree
//! beside it, and the highlight and injection queries come from the grammar
//! crates themselves. Neither of those is how tree-house's own documentation
//! reads — `Grammar::new` takes a path — and both are load-bearing for
//! single-binary distribution. See ADR-0007.
//!
//! The whole point of the layering is that this crate answers with **plain
//! data**: byte ranges and a name for each. Nothing here knows about a
//! terminal, and the render layer decides what a `keyword` looks like.

use ropey::RopeSlice;
use std::collections::HashMap;
use std::ops::Range;
use std::sync::{Arc, OnceLock};
use std::time::Duration;
use tree_house::highlighter::{Highlight, Highlighter};
use tree_house::text_object::TextObjectQuery;
use tree_house::tree_sitter::{InactiveQueryCursor, Query};
use tree_house::{InjectionLanguageMarker, LanguageConfig, LanguageLoader};

/// Re-exported so callers need not depend on tree-house directly. The seam is
/// this crate, and a caller that had to name `tree_house` would be reaching
/// through it.
pub use tree_house::tree_sitter::InputEdit;
pub use tree_house::{Language, Syntax};
use tree_house_bindings::Grammar;
use tree_sitter_language::LanguageFn;

/// How long a single parse may take before tree-house gives up.
///
/// A cancelled parse leaves the previous tree in place, so the cost of the
/// limit being hit is stale colour rather than a stalled editor.
const PARSE_TIMEOUT: Duration = Duration::from_millis(500);

/// The highlight names meowr understands.
///
/// A fixed list rather than every capture a query might contain: the render
/// layer needs a colour for each, so an open set would mean unstyled spans
/// with no way to notice. Captures outside the list simply do not highlight.
pub const HIGHLIGHT_NAMES: &[&str] = &[
    "attribute",
    "comment",
    "constant",
    "constant.builtin",
    "constructor",
    "function",
    "function.builtin",
    "function.macro",
    "keyword",
    "label",
    "namespace",
    "number",
    "operator",
    "property",
    "punctuation",
    "punctuation.bracket",
    "punctuation.delimiter",
    "string",
    "string.special",
    "tag",
    // Markup, which only the markdown grammars emit. These are
    // nvim-treesitter's capture names rather than Helix's `markup.*`, because
    // the queries come from the grammar crate and that is what it uses — the
    // same reason the highlight constants are named one by one.
    "text.emphasis",
    "text.literal",
    "text.reference",
    "text.strong",
    "text.title",
    "text.uri",
    "type",
    "type.builtin",
    "variable",
    "variable.builtin",
    "variable.parameter",
];

/// The index in [`HIGHLIGHT_NAMES`] a query capture means.
///
/// Captures are dotted and more specific than the list — `keyword.control.
/// conditional`, `function.method` — so the longest entry that is a dotted
/// prefix wins. That is the convention every tree-sitter query is written
/// against, and matching the whole name exactly would highlight almost
/// nothing.
fn highlight_for(capture: &str) -> Option<usize> {
    HIGHLIGHT_NAMES
        .iter()
        .enumerate()
        .filter(|(_, name)| capture == **name || capture.starts_with(&format!("{name}.")))
        .max_by_key(|(_, name)| name.len())
        .map(|(index, _)| index)
}

/// One coloured run of the buffer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Span {
    pub range: Range<usize>,
    /// An index into [`HIGHLIGHT_NAMES`].
    pub highlight: usize,
}

impl Span {
    pub fn name(&self) -> &'static str {
        HIGHLIGHT_NAMES[self.highlight]
    }
}

/// Every language meowr can parse, and the queries for each.
pub struct Languages {
    configs: Vec<LanguageConfig>,
    textobjects: Vec<Option<TextObjectQuery>>,
    names: Vec<&'static str>,
    by_extension: HashMap<&'static str, Language>,
    skipped: Vec<(&'static str, String)>,
}

struct Definition {
    name: &'static str,
    extensions: &'static [&'static str],
    language: LanguageFn,
    highlights: &'static str,
    injections: &'static str,
    /// Textobject queries: what `class`, `function`, `test`, `entry`,
    /// `parameter` and `comment` mean in this language.
    ///
    /// Vendored from Helix rather than taken from the `tree-sitter-*` crates,
    /// which do not publish them, and **not** from `meow-tree-sitter` — the
    /// reference configuration's own source for these — because that package
    /// is GPL-3 and vendoring it into an MIT/Apache-2.0 project would
    /// relicense the binary. See `queries/NOTICE`.
    textobjects: &'static str,
}

/// The languages of the reference configuration.
///
/// Each is an ordinary dependency, compiled from its C sources into this
/// binary by its own build script. Its queries are `include_str!`d by that
/// same crate, so they are versioned with the grammar they were written for
/// rather than vendored separately and left to drift.
fn definitions() -> Vec<Definition> {
    // The query constants are named inconsistently across the ecosystem —
    // `HIGHLIGHTS_QUERY` in some crates, `HIGHLIGHT_QUERY` in others — so each
    // is named explicitly rather than derived from the crate name.
    macro_rules! language {
        // The general form. `textobjects` is explicit because not every
        // language has one to vendor: Helix ships no markdown textobjects
        // query, and an empty file in `queries/` would imply it did.
        ($name:literal, [$($ext:literal),*], $language:expr, $highlights:expr,
         $injections:expr, $textobjects:expr) => {
            Definition {
                name: $name,
                extensions: &[$($ext),*],
                language: $language,
                highlights: $highlights,
                injections: $injections,
                textobjects: $textobjects,
            }
        };
        // The usual case: a vendored textobjects query named for the language.
        ($name:literal, [$($ext:literal),*], $language:expr, $highlights:expr,
         $injections:expr) => {
            language!(
                $name,
                [$($ext),*],
                $language,
                $highlights,
                $injections,
                include_str!(concat!("../queries/", $name, ".scm"))
            )
        };
    }
    vec![
        language!(
            "rust",
            ["rs"],
            tree_sitter_rust::LANGUAGE,
            tree_sitter_rust::HIGHLIGHTS_QUERY,
            tree_sitter_rust::INJECTIONS_QUERY
        ),
        language!(
            "json",
            ["json"],
            tree_sitter_json::LANGUAGE,
            tree_sitter_json::HIGHLIGHTS_QUERY,
            ""
        ),
        language!(
            "python",
            ["py"],
            tree_sitter_python::LANGUAGE,
            tree_sitter_python::HIGHLIGHTS_QUERY,
            ""
        ),
        language!(
            "toml",
            ["toml"],
            tree_sitter_toml_ng::LANGUAGE,
            tree_sitter_toml_ng::HIGHLIGHTS_QUERY,
            ""
        ),
        // Nix injects bash into its shell-script strings, and meowr has bash,
        // so this is the one injection here that can reach a grammar that is
        // present. `#offset!` is what cost JavaScript its injections; if this
        // one is rejected the language is skipped and the registry test says
        // so rather than the highlighting quietly stopping at the string.
        language!(
            "nix",
            ["nix"],
            tree_sitter_nix::LANGUAGE,
            tree_sitter_nix::HIGHLIGHTS_QUERY,
            tree_sitter_nix::INJECTIONS_QUERY
        ),
        // Scheme, for M2's `init.scm`. Steel is a Scheme dialect rather than
        // Scheme, so this highlights the forms the two share and knows nothing
        // of Steel's own; see the test that says which.
        //
        // No injections: `tree-sitter-scheme` ships an injections query and
        // leaves the constant *commented out* in its Rust bindings, beside
        // `LOCALS_QUERY` and `TAGS_QUERY`. ADR-0007 already says these
        // constants are named inconsistently across the ecosystem; they are
        // also not all present, and a grep for the name finds this one.
        language!(
            "scheme",
            ["scm", "ss"],
            tree_sitter_scheme::LANGUAGE,
            tree_sitter_scheme::HIGHLIGHTS_QUERY,
            ""
        ),
        language!(
            "bash",
            ["sh", "bash"],
            tree_sitter_bash::LANGUAGE,
            tree_sitter_bash::HIGHLIGHT_QUERY,
            ""
        ),
        language!(
            "yaml",
            ["yaml", "yml"],
            tree_sitter_yaml::LANGUAGE,
            tree_sitter_yaml::HIGHLIGHTS_QUERY,
            ""
        ),
        // Markdown is two grammars, which is the grammar's design and not a
        // choice here: a block parser for headings, lists and fences, and a
        // separate inline parser for what is inside a paragraph. The block
        // one injects the other, so both have to be registered or a document
        // is highlighted down to the paragraph and no further.
        language!(
            "markdown",
            ["md", "markdown"],
            tree_sitter_md::LANGUAGE,
            tree_sitter_md::HIGHLIGHT_QUERY_BLOCK,
            // Vendored, where every other injection query here comes from its
            // crate. The crate's own was written for nvim-treesitter, which
            // includes an injected node's children by default; tree-house
            // excludes them unless the query says `injection.include-children`,
            // so the inline layer was handed the paragraph with every `*` and
            // backtick cut out of it and correctly found no emphasis. Helix's
            // query is written against tree-house and says so.
            include_str!("../queries/markdown-injections.scm"),
            // Helix ships no markdown textobjects query, so there is nothing
            // to vendor and nothing pretending otherwise.
            ""
        ),
        // Not a language you open, so it is not one of the nine: it is the
        // name the injection query above uses, and it has no extension
        // because a file is never in it. `language_for_marker` resolves it by
        // that name. Counted rather than described, in `tests.rs` — a number
        // written into a comment here has gone stale twice.
        language!(
            "markdown.inline",
            [],
            tree_sitter_md::INLINE_LANGUAGE,
            tree_sitter_md::HIGHLIGHT_QUERY_INLINE,
            tree_sitter_md::INJECTION_QUERY_INLINE,
            ""
        ),
    ]
}

impl Default for Languages {
    fn default() -> Self {
        Self::new()
    }
}

/// The process-wide registry, built once.
///
/// Compiling every grammar's highlight queries takes long enough to notice —
/// building it per `Session` turned a 0.3-second test suite into a
/// 178-second one, which is how this was found. The result is immutable, so
/// there is no reason for more than one to exist.
pub fn languages() -> Arc<Languages> {
    static LANGUAGES: OnceLock<Arc<Languages>> = OnceLock::new();
    Arc::clone(LANGUAGES.get_or_init(|| Arc::new(Languages::new())))
}

impl Languages {
    /// Build the registry, skipping any language whose queries do not compile.
    ///
    /// Skipping rather than panicking: a query that fails against a newer
    /// grammar should cost that one language its colour, not stop the editor
    /// from starting.
    pub fn new() -> Self {
        let mut configs = Vec::new();
        let mut textobjects = Vec::new();
        let mut names = Vec::new();
        let mut by_extension = HashMap::new();
        let mut skipped = Vec::new();
        for definition in definitions() {
            let grammar = match Grammar::try_from(definition.language) {
                Ok(grammar) => grammar,
                Err(e) => {
                    skipped.push((definition.name, e.to_string()));
                    continue;
                }
            };
            let config = match LanguageConfig::new(
                grammar,
                definition.highlights,
                definition.injections,
                "",
            ) {
                Ok(config) => config,
                Err(e) => {
                    skipped.push((definition.name, e.to_string()));
                    continue;
                }
            };
            // Without this every capture maps to `None` and nothing
            // highlights at all — quietly, with no error anywhere. The
            // highlighter treats the indices as opaque, so deciding what a
            // capture name means is the caller's job.
            config.configure(|capture| highlight_for(capture).map(|i| Highlight::new(i as u32)));
            let language = Language::new(configs.len() as u32);
            // A language whose textobject query does not compile keeps its
            // colour and loses its Things, which is the useful half surviving.
            textobjects.push(
                Query::new(grammar, definition.textobjects, |_, _| Ok(()))
                    .map(|query| TextObjectQuery { query })
                    .map_err(|e| skipped.push((definition.name, format!("textobjects: {e}"))))
                    .ok(),
            );
            configs.push(config);
            names.push(definition.name);
            for extension in definition.extensions {
                by_extension.insert(*extension, language);
            }
        }
        Self {
            configs,
            textobjects,
            names,
            by_extension,
            skipped,
        }
    }

    /// Languages that did not load, and why.
    ///
    /// Skipping is the right behaviour — one language's colour is not worth
    /// refusing to start over — but a silent skip is how you end up wondering
    /// for a week why Go has no colour. This is how anyone finds out.
    pub fn skipped(&self) -> &[(&'static str, String)] {
        &self.skipped
    }

    /// The language for a filename, by extension. `None` is an ordinary
    /// answer: a buffer with no grammar is plain text, not an error.
    pub fn for_path(&self, path: &str) -> Option<Language> {
        let extension = path.rsplit_once('.')?.1;
        self.by_extension.get(extension).copied()
    }

    pub fn by_name(&self, name: &str) -> Option<Language> {
        self.names
            .iter()
            .position(|n| *n == name)
            .map(|i| Language::new(i as u32))
    }

    /// Every Language meowr knows, in the order they were registered.
    ///
    /// For the error a misspelt Language name raises — the `set-option`
    /// precedent, which lists the real names rather than only refusing the
    /// wrong one.
    pub fn names(&self) -> &[&'static str] {
        &self.names
    }

    pub fn name_of(&self, language: Language) -> Option<&'static str> {
        self.names.get(language.idx()).copied()
    }

    pub fn len(&self) -> usize {
        self.configs.len()
    }

    pub fn is_empty(&self) -> bool {
        self.configs.is_empty()
    }
}

impl LanguageLoader for Languages {
    /// Injections name their language in the source, so this is where an
    /// injected block finds its grammar.
    fn language_for_marker(&self, marker: InjectionLanguageMarker) -> Option<Language> {
        match marker {
            InjectionLanguageMarker::Name(name) => self.by_name(name),
            // The language is named *in the document* rather than in the
            // query: the `rust` in an opening ```rust fence. Dropping this —
            // which a `_ => None` arm did — means no injection that names its
            // language in the source can ever resolve, in any language, so a
            // fenced code block in Markdown was never going to highlight.
            //
            // A document says `sh` where the language is called `bash`, so the
            // extension table answers what the name table cannot. Helix
            // matches these by a per-language regex; this is the same idea
            // with the two lookups it already has.
            InjectionLanguageMarker::Match(text) => {
                let named = text.to_string();
                let named = named.trim();
                self.by_name(named)
                    .or_else(|| self.by_extension.get(named).copied())
            }
            _ => None,
        }
    }

    fn get_config(&self, language: Language) -> Option<&LanguageConfig> {
        self.configs.get(language.idx())
    }
}

/// A parse to run away from the keystroke path.
///
/// Carries a rope snapshot rather than a borrow, because the whole point is
/// that the buffer stays editable while this runs. Ropes share structure, so
/// the snapshot is cheap.
pub struct Reparse {
    pub text: ropey::Rope,
    pub language: Language,
    pub syntax: Option<Syntax>,
    pub edits: Vec<tree_house::tree_sitter::InputEdit>,
    /// The buffer revision this was taken at. A result whose revision no
    /// longer matches is stale and dropped rather than applied — the edits it
    /// missed would put the tree out of step with the text.
    pub revision: u64,
}

impl Reparse {
    /// Do the work. Called on a worker, never on the event loop.
    pub fn run(mut self, languages: &Languages) -> Option<Syntax> {
        match self.syntax.take() {
            Some(mut syntax) => {
                reparse(&mut syntax, self.text.slice(..), &self.edits, languages).then_some(syntax)
            }
            None => parse(self.text.slice(..), self.language, languages),
        }
    }
}

/// An edit, in the form tree-sitter needs to reuse the tree it already has.
///
/// Byte offsets *and* row/column points, because tree-sitter tracks both. The
/// points are computed from the rope rather than guessed, since getting them
/// wrong makes the reparse silently wrong rather than slow.
///
/// `context` names `at`'s own row/column and nothing past it — `old_end_point`
/// and `new_end_point` are both walked from `removed`/`inserted` themselves,
/// so the text at `at` need not have this edit applied yet, nor even be the
/// text this edit is really about: a caller building several edits against
/// one shared snapshot (see `meowr_app::buffer::tree_sitter_edits`) can pass
/// that snapshot here for every one of them.
pub fn edit(context: RopeSlice<'_>, at: usize, removed: &str, inserted: &str) -> InputEdit {
    let point_in = |slice: RopeSlice<'_>, byte: usize| {
        let byte = byte.min(slice.len_bytes());
        let line = slice.byte_to_line(byte);
        tree_house::tree_sitter::Point {
            row: line as u32,
            col: (byte - slice.line_to_byte(line)) as u32,
        }
    };
    let start_point = point_in(context, at);
    // The old end point cannot be read from the buffer — that text is gone —
    // but it can be *walked* through the removed text, which is the same
    // thing and needs no snapshot of the old rope. Getting this wrong makes a
    // reparse silently produce the wrong tree rather than a slow one.
    let walk = |from: tree_house::tree_sitter::Point, text: &str| match text.rfind('\n') {
        Some(last) => tree_house::tree_sitter::Point {
            row: from.row + text.matches('\n').count() as u32,
            col: (text.len() - last - 1) as u32,
        },
        None => tree_house::tree_sitter::Point {
            row: from.row,
            col: from.col + text.len() as u32,
        },
    };
    let old_end_point = walk(start_point, removed);
    // Walked the same way, from `inserted` rather than `removed` — and for
    // the same reason `old_end_point` is: the text `inserted` ends up as is
    // not in `after` at `at`, it is `inserted` itself, so its own row/column
    // needs no snapshot to find. Reading it via `after` instead only works
    // when `after` happens to already contain this exact edit applied at
    // this exact position — true of the one caller this function first had,
    // false of a batch built from a shared snapshot no single edit in it
    // owns.
    let new_end_point = walk(start_point, inserted);
    InputEdit {
        start_byte: at as u32,
        old_end_byte: (at + removed.len()) as u32,
        new_end_byte: (at + inserted.len()) as u32,
        start_point,
        old_end_point,
        new_end_point,
    }
}

/// Reparse in place, reusing the existing tree.
///
/// This is the incremental path: tree-sitter re-lexes only what the edits
/// touched, which is what makes highlighting a large file affordable at all.
/// `false` when the parse failed or timed out — the previous tree stays, so
/// the cost is stale colour rather than none.
pub fn reparse(
    syntax: &mut Syntax,
    source: RopeSlice<'_>,
    edits: &[tree_house::tree_sitter::InputEdit],
    languages: &Languages,
) -> bool {
    syntax
        .update(source, PARSE_TIMEOUT, edits, languages)
        .is_ok()
}

/// The six Things `meow-tree-sitter` adds, by the character that names them.
///
/// `defun` is here too: ADR-0006 sends it to tree-sitter rather than to a
/// heuristic, and a `defun` *is* a function.
pub const TREE_SITTER_THINGS: &[(&str, &str)] = &[
    ("class", "class"),
    ("function", "function"),
    ("test", "test"),
    ("entry", "entry"),
    ("parameter", "parameter"),
    ("comment", "comment"),
    ("defun", "function"),
];

/// Everything needed to resolve a tree-sitter Thing: a parsed tree and the
/// queries for its language.
///
/// One borrow rather than two so that `Ctx` gains one field rather than two.
/// `None` where a buffer has no grammar, which is the ordinary case for plain
/// text and the reason this is an `Option` at the call site.
#[derive(Clone, Copy)]
pub struct TextObjects<'a> {
    pub syntax: &'a Syntax,
    pub languages: &'a Languages,
}

impl TextObjects<'_> {
    pub fn range(
        &self,
        source: RopeSlice<'_>,
        pos: usize,
        thing: &str,
        inner: bool,
    ) -> Option<Range<usize>> {
        text_object(self.syntax, source, self.languages, pos, thing, inner)
    }
}

/// The range of a tree-sitter Thing around `pos`.
///
/// `inner` picks `.inside` over `.around` — the same distinction `,` and `.`
/// make, which is why the capture names line up with the commands.
///
/// The innermost match wins: nested functions and a method inside an impl both
/// match, and the one that starts latest is the one the point is actually in.
pub fn text_object(
    syntax: &Syntax,
    source: RopeSlice<'_>,
    languages: &Languages,
    pos: usize,
    thing: &str,
    inner: bool,
) -> Option<Range<usize>> {
    let capture = TREE_SITTER_THINGS
        .iter()
        .find(|(name, _)| *name == thing)
        .map(|(_, capture)| *capture)?;
    let name = format!("{capture}.{}", if inner { "inside" } else { "around" });
    let mut best: Option<Range<usize>> = None;
    for range in captured(syntax, source, languages, &name) {
        if range.start <= pos
            && pos <= range.end
            && best.as_ref().is_none_or(|b| range.start >= b.start)
        {
            best = Some(range);
        }
    }
    best
}

/// Every node one capture matches, as byte ranges.
///
/// The lookup both readers of the textobject queries need: the layer's
/// language, its query, its tree. Written twice before a review pointed out
/// that `definitions_in` had reimplemented `text_object`'s in a second idiom --
/// which is the shape that ends with the two disagreeing about what an
/// unparsed buffer answers.
fn captured(
    syntax: &Syntax,
    source: RopeSlice<'_>,
    languages: &Languages,
    capture: &str,
) -> Vec<Range<usize>> {
    let language = syntax.layer(syntax.root()).language;
    let Some(Some(query)) = languages.textobjects.get(language.idx()) else {
        return Vec::new();
    };
    let Some(tree) = syntax.layer(syntax.root()).tree() else {
        return Vec::new();
    };
    let cursor = InactiveQueryCursor::new(0..u32::MAX, u32::MAX);
    query
        .capture_nodes(capture, tree.root_node(), source, cursor)
        .map(|nodes| {
            nodes
                .map(|node| node.start_byte()..node.end_byte())
                .collect()
        })
        .unwrap_or_default()
}

/// Every definition the parse knows about, as byte ranges, in order.
///
/// **imenu's question, answered from the tree meowr already parsed for
/// colour.** `consult-imenu-multi` is not a server call, which is why `SPC s I`
/// sits with the rows that search what is already open rather than with the
/// LSP ones -- and why this needs no `tags.scm` vendored on top of what is
/// here: a function and a class are exactly two of the six textobject captures
/// the Things already use.
///
/// `function` and `class` and no more. `test` is a function by another name,
/// `parameter` and `entry` are parts of a definition rather than definitions,
/// and `comment` is not one at all -- so a list of all six would be a list
/// nobody could read down.
///
/// `.around` rather than `.inside`, because the range is what names the row:
/// the first line of a function is its signature, and the first line of its
/// body is whatever happens to be there.
pub fn definitions_in(
    syntax: &Syntax,
    source: RopeSlice<'_>,
    languages: &Languages,
) -> Vec<Range<usize>> {
    let mut found: Vec<Range<usize>> = ["function.around", "class.around"]
        .iter()
        .flat_map(|name| captured(syntax, source, languages, name))
        .collect();
    // In the order they appear, which is the order you read the file in. A
    // query answers in whatever order it matched, and an imenu list that moved
    // between invocations is one you cannot learn.
    found.sort_by_key(|range| (range.start, range.end));
    found.dedup();
    found
}

/// What each Language will fold, by node type.
///
/// **`treesit-fold`'s own rule sets, transcribed** (ADR-0012), one entry per
/// Grammar that has one — read out of `treesit-fold-parsers.el` rather than
/// invented here, which is the same care ADR-0007 takes over a query written
/// against a different runtime. A Language absent from this table does not
/// fold, which is an honest answer: `treesit-fold` has no rule set for it
/// either.
///
/// **The list is the filter.** There is no minimum span, because a node type is
/// either foldable or absent from the set — a better answer than a threshold,
/// since it is `match_block` and `use_list` a reader wants to fold rather than
/// everything above *n* lines.
///
/// Rust's is **eleven** entries. ADR-0012 says ten and lists ten, which is one
/// short of its own source: `block_comment` is in the rule set and missing from
/// the ADR. The source is what that ADR says to follow, so the source is what
/// is here.
pub const FOLDABLE: &[(&str, &[&str])] = &[
    (
        "rust",
        &[
            "declaration_list",
            "enum_variant_list",
            "field_declaration_list",
            "use_list",
            "field_initializer_list",
            "match_block",
            "macro_definition",
            "block",
            "token_tree",
            "line_comment",
            "block_comment",
        ],
    ),
    (
        "python",
        &[
            "import_statement",
            "import_from_statement",
            "future_import_statement",
            "function_definition",
            "class_definition",
            "while_statement",
            "for_statement",
            "if_statement",
            "elif_clause",
            "else_clause",
            "match_statement",
            "case_clause",
            "try_statement",
            "except_clause",
            "with_statement",
            "list",
            "dictionary",
            "parenthesized_expression",
            "expression_statement",
            "comment",
        ],
    ),
    ("json", &["object", "array", "comment"]),
    ("yaml", &["comment", "block_sequence_item", "block_mapping_pair"]),
    ("bash", &["compound_statement", "do_group", "comment"]),
    ("toml", &["table", "array", "inline_table", "comment"]),
    ("markdown", &["fenced_code_block", "section", "html_block"]),
    (
        "nix",
        &["attrset_expression", "interpolation", "list_expression", "comment"],
    ),
    ("scheme", &["list", "comment"]),
];

/// The innermost foldable node containing `byte`, as a byte range.
///
/// Climbs from the innermost node outwards until a node type in the Language's
/// rule set turns up — which is what makes `SPC C-f` on a line inside a `match`
/// arm fold the arm's block rather than the whole function, and pressing it
/// again on the head fold the next thing out.
///
/// `None` where nothing around the point is foldable, and where the Language
/// has no rule set at all: `treesit-fold` has none for it either, and a fold
/// invented here would be a guess at what a reader wanted collapsed.
///
/// The range is the node's **interior**: `treesit-fold-range-seq` is
/// `(1+ start)` to `(1- end)`, so the delimiters stay out and the closing
/// brace's line goes on being drawn. That is **eight** of Rust's eleven entries
/// and the shape of nearly every rule set, so it is the rule carried here.
///
/// **The other three range functions are not carried**, and what that costs is
/// worth saying rather than discovering.
///
/// `treesit-fold-range-line-comment` gathers a *run* of consecutive `//`
/// siblings into one fold. Without it, `line_comment` folds **nothing at all** —
/// not merely "a single-line one": tree-sitter never groups those siblings into
/// a node, and this walk only climbs ancestors, so there is no number of
/// consecutive comment lines that produces a foldable node. A `/* */`
/// `block_comment` still folds. `treesit-fold-range-block-comment` is
/// `range-seq` with a further `(1 . -1)`, so it trims two characters a side
/// where this trims one; and `treesit-fold-range-rust-macro` trims a macro
/// definition differently again. All three are *ranges* rather than rule sets,
/// and ADR-0012 commits to the list — its amendment records exactly this.
///
/// A range spanning one line is refused. Folding it would hide nothing and
/// leave an ellipsis claiming otherwise.
pub fn foldable_at(
    syntax: &Syntax,
    languages: &Languages,
    byte_to_line: impl Fn(usize) -> Option<usize>,
    byte: usize,
) -> Option<Range<usize>> {
    let language = syntax.layer(syntax.root()).language;
    let kinds = FOLDABLE
        .iter()
        .find(|(name, _)| Some(*name) == languages.name_of(language))
        .map(|(_, kinds)| *kinds)?;
    let at = u32::try_from(byte).ok()?;
    let mut node = syntax.descendant_for_byte_range(at, at.saturating_add(1))?;
    loop {
        if kinds.contains(&node.kind()) {
            // `treesit-fold-range-seq`, verbatim: the interior, so the
            // delimiters are not part of what is hidden.
            let range = (node.start_byte() as usize + 1)..(node.end_byte() as usize)
                .saturating_sub(1);
            // More than one line, or there is nothing to hide.
            if !range.is_empty() && byte_to_line(range.end.saturating_sub(1)) > byte_to_line(range.start)
            {
                return Some(range);
            }
        }
        node = node.parent()?;
    }
}

/// The string or comment containing `byte`, if it is inside one.
///
/// This is what the block commands need in order to stop counting a `)` that
/// is text rather than a delimiter — Emacs asks `syntax-ppss`, and meowr has
/// the tree it already parsed for colour. Plain data out, as everything in
/// this crate is: a byte range, or nothing.
///
/// Only the **innermost** node is consulted, and that is the whole design.
/// Climbing to the ancestors would report the `)` in `` `${f()}` `` as inside
/// a string, because the template literally is one; the interpolation within
/// it is code, and the innermost node is the only place that distinction
/// survives. `descendant_for_byte_range` also descends into injected layers,
/// so a bracket in SQL inside a Rust string is judged by SQL's own tree.
///
/// The kinds are matched by name rather than from a per-language table.
/// Grammars name these nodes consistently — `string`, `string_literal`,
/// `string_content`, `raw_string_literal`, `comment`, `line_comment`,
/// `doc_comment` — and ADR-0006 puts the heuristics underneath the selection
/// calculus outside what Fidelity binds, deliberately. Emacs's own answer is
/// a syntax table, which is a heuristic in the same way.
pub fn literal_at(syntax: &Syntax, byte: usize) -> Option<Range<usize>> {
    let at = u32::try_from(byte).ok()?;
    let node = syntax.descendant_for_byte_range(at, at.saturating_add(1))?;
    let kind = node.kind();
    (kind.contains("string") || kind.contains("comment"))
        .then(|| node.start_byte() as usize..node.end_byte() as usize)
}

/// Parse `source` as `language`.
pub fn parse(source: RopeSlice<'_>, language: Language, languages: &Languages) -> Option<Syntax> {
    Syntax::new(source, language, PARSE_TIMEOUT, languages).ok()
}

/// The coloured runs of `source` over the byte range `within`.
///
/// Only the visible range is asked for: highlighting a 200,000-line file in
/// order to draw fifty rows of it is the difference between a frame and a
/// stutter.
pub fn spans(
    syntax: &Syntax,
    source: RopeSlice<'_>,
    languages: &Languages,
    within: Range<usize>,
) -> Vec<Span> {
    let start = within.start as u32;
    let end = within.end as u32;
    let mut highlighter = Highlighter::new(syntax, source, languages, start..end);
    let mut spans: Vec<Span> = Vec::new();
    let mut at = highlighter.next_event_offset();
    if at == u32::MAX {
        return spans;
    }
    loop {
        let (_event, mut highlights) = highlighter.advance();
        // The innermost highlight wins: a `function` inside a `variable` is
        // the more specific statement about that byte.
        let innermost = highlights.next_back().map(index_of);
        let next = highlighter.next_event_offset();
        let until = if next == u32::MAX { end } else { next.min(end) };
        if let Some(index) = innermost
            && at < until
            && index < HIGHLIGHT_NAMES.len()
        {
            spans.push(Span {
                range: at as usize..until as usize,
                highlight: index,
            });
        }
        if next == u32::MAX || until >= end {
            break;
        }
        at = next;
    }
    spans
}

fn index_of(highlight: Highlight) -> usize {
    highlight.idx()
}

#[cfg(test)]
mod tests;
