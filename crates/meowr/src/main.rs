//! meowr: a modal editor driven by the exact keybindings of Emacs Meow.
//!
//! This crate is the only one that touches the terminal. Everything it does
//! with an event is delegated to `meowr-app`, and everything it draws is
//! delegated to `meowr-render`, so the parts worth testing are testable without
//! a tty.

use crossterm::event::{Event, EventStream, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use futures::StreamExt;
use meowr_app::buffer::BufferId;
use meowr_app::{Action, Files, Opened, Session};
use meowr_command::{Key, Keys, State};
use meowr_domain::Document;
use meowr_render::paint;
use meowr_view::{Screen, Viewport, WindowView};
use serde_json::Value;

/// What the command line asked for.
#[derive(Debug, PartialEq, Eq)]
enum Invocation {
    /// Open a file, or the scratch buffer when there is none.
    ///
    /// `skip_config` is `--no-config`. Its job is diagnosis rather than rescue:
    /// a broken config can no longer stop meowr starting, so what it answers is
    /// "is this meowr misbehaving, or is it mine?" — the question you need
    /// settled before filing a bug.
    Open {
        path: Option<String>,
        skip_config: bool,
    },
    /// `--help`. Carries `--no-config` because the help text reads its keys
    /// out of a live keymap, and a config may have moved every one of them.
    Help {
        skip_config: bool,
    },
    Version,
    /// Something to say and then stop: an unknown flag, or more files than
    /// one window can show. Carries the line to print.
    Complaint(String),
}

fn invocation(args: impl Iterator<Item = String>) -> Invocation {
    let mut paths: Vec<String> = Vec::new();
    let mut options_ended = false;
    let mut skip_config = false;
    // Held rather than returned, because `--no-config` may come after
    // `--help` and the help text is built from a keymap the config has had
    // its say over. A complaint still wins immediately: there is nothing to
    // learn from the rest of a line that has a misspelling in it.
    let mut asked: Option<Invocation> = None;
    for arg in args {
        if options_ended || !arg.starts_with('-') {
            paths.push(arg);
            continue;
        }
        match arg.as_str() {
            "--" => options_ended = true,
            "--no-config" => skip_config = true,
            "-h" | "--help" => asked = asked.or(Some(Invocation::Help { skip_config })),
            "-V" | "--version" => asked = asked.or(Some(Invocation::Version)),
            flag => return Invocation::Complaint(format!("unknown option `{flag}`")),
        }
    }
    if let Some(asked) = asked {
        // `--no-config` is answered wherever it appeared, which is why this is
        // rebuilt rather than returned as it was found.
        return match asked {
            Invocation::Help { .. } => Invocation::Help { skip_config },
            other => other,
        };
    }
    match paths.len() {
        0 => Invocation::Open {
            path: None,
            skip_config,
        },
        1 => Invocation::Open {
            path: paths.pop(),
            skip_config,
        },
        // Silently keeping the last would open a file the user did not name
        // last for any reason they would recognise.
        n => Invocation::Complaint(format!("{n} files given; meowr opens one")),
    }
}

/// The session `--help` reads its keys out of.
///
/// Not the session the editor runs: that one is built against the real
/// terminal and carries a file. This one exists only to be asked questions, so
/// the viewport is a number rather than a measurement — nothing is drawn.
///
/// A config that does not resolve leaves the defaults standing, for the same
/// reason it does in the editor (#59). It is not reported here either, because
/// the editor says so in the Echo Area the moment it starts and `--help` is
/// not where anybody looks for that.
fn described(skip_config: bool) -> Session {
    let mut session = Session::new(Viewport { top: 0, height: 24 });
    if skip_config {
        return session;
    }
    if let Ok(collected) = config::evaluate(config::find().as_deref())
        && collected.problems.is_empty()
    {
        let _ = session.apply(collected.declarations);
    }
    session
}

/// The `--help` text.
///
/// The keys are looked up, not written out beside them. A help screen nobody
/// re-runs is exactly the "second source for one fact" that `leader.rs` warns
/// about — and it had become one: this used to ask `default_keymap` and so
/// described a keymap the reader may not have been running, in a layout the
/// prose named and a config could contradict.
///
/// [`Session::keys_for`] rather than [`Keymap::keys_for`] because a command
/// may sit under the leader alone. A config that drops `C-x C-s` has not lost
/// `write-file`; it has moved it to `SPC f s`, and this has to say so.
fn usage(session: &Session) -> String {
    let key = |command: &str| {
        session
            .keys_for(State::Normal, command)
            .map(|keys| Keys(&keys).to_string())
            .unwrap_or_else(|| "unbound".to_string())
    };
    format!(
        "\
meowr — a modal editor driven by the keybindings of Emacs Meow

USAGE
    meowr [FILE]

OPTIONS
    -h, --help       Show this
    -V, --version    Show the version
    --no-config      Start without reading init.scm

KEYS
    {:<16} Quit (refuses if a buffer is unsaved)
    {:<16} Save
    {:<16} Leader; the menu appears after a moment
    {:<16} Left, down, up, right
    {:<16} Every binding, in a buffer

Most keys select; a few act on the selection.",
        key("quit"),
        key("write-file"),
        key("leader"),
        format!(
            "{} {} {} {}",
            key("left"),
            key("next"),
            key("prev"),
            key("right")
        ),
        key("describe-bindings"),
    )
}
/// Translate a terminal key into the session's own key type.
///
/// Returns `None` for keys the session has no vocabulary for yet.
///
/// This used to promise that "the keymap registry (ticket #17) replaces this
/// with a table". #17 shipped and did not: it maps a [`Key`] to a command,
/// where this maps a *terminal event* to a `Key`, and one layer cannot stand
/// in for the other. Growing the match is therefore the way this changes —
/// but only for a key something actually reads, which is the rule the old
/// promise was reaching for.
fn to_key(event: KeyEvent) -> Option<Key> {
    if event.kind != KeyEventKind::Press {
        return None;
    }
    match event.code {
        KeyCode::Char(c) if event.modifiers.contains(KeyModifiers::CONTROL) => Some(Key::Ctrl(c)),
        KeyCode::Char(c) if event.modifiers.contains(KeyModifiers::ALT) => Some(Key::Alt(c)),
        KeyCode::Char(c) => Some(Key::Char(c)),
        KeyCode::Enter => Some(Key::Enter),
        KeyCode::Esc => Some(Key::Esc),
        KeyCode::Backspace => Some(Key::Backspace),
        // Modifiers are dropped rather than refused: crossterm reports Shift
        // on `BackTab` on some terminals and not others, and a binding that
        // worked in one and not the next would be worse than one that ignores
        // a modifier meowr has no `Key` for anyway.
        KeyCode::Tab => Some(Key::Tab),
        KeyCode::BackTab => Some(Key::BackTab),
        KeyCode::Up => Some(Key::Up),
        KeyCode::Down => Some(Key::Down),
        KeyCode::Left => Some(Key::Left),
        KeyCode::Right => Some(Key::Right),
        KeyCode::PageUp => Some(Key::PageUp),
        KeyCode::PageDown => Some(Key::PageDown),
        _ => None,
    }
}

mod config;
mod lsp;
mod search;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Answered before `ratatui::init`, which takes the alternate screen: a
    // `--help` that grabs the terminal to print into it is no help at all,
    // and on a machine with no tty it panics instead of saying anything.
    let (path, skip_config) = match invocation(std::env::args().skip(1)) {
        Invocation::Open { path, skip_config } => (path, skip_config),
        Invocation::Help { skip_config } => {
            println!("{}", usage(&described(skip_config)));
            return Ok(());
        }
        Invocation::Version => {
            println!("meowr {}", env!("CARGO_PKG_VERSION"));
            return Ok(());
        }
        Invocation::Complaint(complaint) => {
            // The defaults, deliberately: the line is already wrong, and
            // running the config to answer it would be one more thing that
            // could go wrong while saying so.
            eprintln!("meowr: {complaint}\n\n{}", usage(&described(true)));
            std::process::exit(2);
        }
    };

    // Before the alternate screen, for the same reason `--help` is: #44 found
    // that Steel writes a bytecode disassembly to stdout when a program
    // overflows its stack, and here that lands on the ordinary terminal where
    // it is harmless. Evaluating first also means the event loop starts with
    // the config already in effect, so no keypress can land before it.
    //
    // Nothing is exposed to the config yet — that is #61 — so this is the
    // runtime arriving rather than the API. A failure goes to stderr for now,
    // which is visible precisely because the terminal has not been taken; #63
    // replaces it with a line in the Echo Area and a `*config-error*` buffer.
    // Where it lives is worked out once and used twice: to run it now, and to
    // tell the session, whose `SPC f P` opens it whether or not it ran -- a
    // config that was skipped or would not parse is exactly one you want to
    // open (#181).
    let config_at = config::find();
    let config = if skip_config {
        ConfigOutcome::Skipped
    } else {
        match config::evaluate(config_at.as_deref()) {
            // Steel accepted it, but a `define-key` may still have been handed
            // a string where a symbol belongs. Those are collected rather than
            // raised, so a config with three of them says all three.
            Ok(collected) if !collected.problems.is_empty() => ConfigOutcome::Failed {
                summary: first_line(&collected.problems),
                detail: collected.problems.join("\n"),
            },
            Ok(collected) => ConfigOutcome::Declared(collected.declarations),
            Err(e) => ConfigOutcome::Failed {
                summary: e.to_string(),
                detail: e.to_string(),
            },
        }
    };

    // `ratatui::init` enters the alternate screen, turns on raw mode, and
    // installs a panic hook that restores both. Without that hook a panic
    // leaves the user's terminal unusable.
    let mut terminal = ratatui::init();
    let ending = run(&mut terminal, path, config, config_at).await;
    // Before the `exec` below, and before anything is printed: the panic hook
    // `ratatui::init` installed restores these too, and a restart is the one
    // ordinary way out that has something to do afterwards.
    ratatui::restore();
    match ending? {
        Ending::Quit => Ok(()),
        // `exec` does not come back when it works, so what this binds is the
        // failure: the binary could not be found, or could not be run.
        Ending::Restart => Err(become_a_new_meowr()),
    }
}

/// The current branch, or `None` outside a repository.
async fn branch_of(path: Option<&str>) -> Option<String> {
    let directory = match path {
        Some(path) => std::path::Path::new(path).parent()?.to_owned(),
        None => std::env::current_dir().ok()?,
    };
    let output = tokio::process::Command::new("git")
        .args(["rev-parse", "--abbrev-ref", "HEAD"])
        .current_dir(directory)
        .output()
        .await
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned())
        .filter(|branch| !branch.is_empty())
}

/// Where the project picker looks, what its rows are relative to, and which
/// mechanism answers.
///
/// One value rather than two, because #120 was exactly these two drifting
/// apart: a listing prints paths relative to where it ran, so whatever changes
/// the directory has to change the prefix in the same breath.
///
/// The mechanism belongs in the decision rather than below it, because it is
/// what `project-scope` off actually buys. `git ls-files` is the better listing
/// wherever it can run -- `--exclude-standard` keeps `target/` out for nothing
/// -- and outside a repository it cannot run at all.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Listing {
    /// Run `git ls-files` in `directory`, and join every row with `prefix` so
    /// that picking one resolves from where the process already resolves paths.
    Git {
        directory: std::path::PathBuf,
        prefix: std::path::PathBuf,
    },
    /// Walk `directory` instead, because `project-scope` is off and there is no
    /// repository here for `git ls-files` to run in.
    Walk {
        directory: std::path::PathBuf,
        prefix: std::path::PathBuf,
    },
    /// `project-scope` is on and this file is in no repository. ADR-0010 takes
    /// the refusal deliberately: a silent fallback reads as success whether or
    /// not it found anything, which is the shape of #120's bug.
    NoProject,
}

/// Decide it.
///
/// Pure, and split from the subprocess for the reason [`candidates`] already
/// is: the decision is the part with a mistake available in it.
///
/// `asking` is the directory git was asked about, already resolved by
/// [`asking_about`] -- never empty. `root` is what git said, and `None` means no
/// repository.
///
/// `root` is consulted in both settings now. It used to be asked for only when
/// `project_scope` was on, on the reasoning that off "never consults git about a
/// root" -- true, and what made the Option a promise it could not keep. Off does
/// not use the root as a *directory*, but whether there is one is exactly what
/// decides which mechanism can answer, and one `rev-parse` is nothing against a
/// listing that is about to walk a tree.
fn listing_of(
    project_scope: bool,
    asking: &std::path::Path,
    root: Option<&std::path::Path>,
    cwd: &std::path::Path,
) -> Listing {
    let directory = match (project_scope, root) {
        (true, Some(root)) => root.to_owned(),
        (true, None) => return Listing::NoProject,
        // Off is the pre-Project editor: it looked where the file was and
        // nowhere else.
        (false, _) => asking.to_owned(),
    };
    // When the listing already ran where paths resolve from, joining would only
    // add `./` to every row.
    let prefix = if directory == cwd {
        std::path::PathBuf::new()
    } else {
        directory.clone()
    };
    // `root` is `None` only with `project_scope` off by here -- on returned
    // above -- so this is the one case ADR-0010 leaves the Option to rescue.
    match root {
        Some(_) => Listing::Git { directory, prefix },
        None => Listing::Walk { directory, prefix },
    }
}

/// The directory to ask git about, for a launch path.
///
/// A bare filename's parent is the **empty** path, which is where paths already
/// resolve from rather than nothing at all -- an empty `current_dir` runs a
/// subprocess nowhere. `None` for a path with no parent, which is the
/// filesystem root itself and has no directory above it to look in.
///
/// One place, because `listing_of` used to answer this too, and two answers to
/// one question is the drift that made #120.
fn asking_about(path: Option<&str>, cwd: &std::path::Path) -> Option<std::path::PathBuf> {
    match path {
        Some(path) => match std::path::Path::new(path).parent() {
            Some(parent) if parent.as_os_str().is_empty() => Some(cwd.to_owned()),
            Some(parent) => Some(parent.to_owned()),
            None => None,
        },
        None => Some(cwd.to_owned()),
    }
}

/// Files the project picker offers, or why there are none.
///
/// `git ls-files -zco --exclude-standard` -- Doom's own `projectile-git-command`,
/// flag for flag. `-c` and `-o` together are why a file you created a moment ago
/// appears at all; bare `ls-files` is tracked-only, so meowr's picker was blind
/// to a new file that Doom's `SPC SPC` showed. `--exclude-standard` is what keeps
/// `-o` from being the startup that walks `node_modules`.
///
/// `Err` is a file in no repository with `project-scope` on. ADR-0010 takes that
/// refusal deliberately rather than falling back to a directory, and the
/// sentence is written here because this is the layer that knows where it looked.
/// With `project-scope` *off* the same file is walked instead, which is the
/// rescue the Option exists to be and which it did not perform: the listing was
/// `git ls-files` either way, and `git ls-files` exits 128 outside a repository,
/// so off refused in exactly the place it promises not to.
async fn project_of(path: Option<&str>, project_scope: bool) -> Result<Vec<String>, String> {
    let Ok(cwd) = std::env::current_dir() else {
        return Err("the working directory could not be read".into());
    };
    let Some(asking) = asking_about(path, &cwd) else {
        return Err(format!(
            "`{}` has no directory to look in",
            path.unwrap_or_default()
        ));
    };
    // Asked in both settings, for the reason `listing_of` gives: off does not
    // use a root, but whether there is one is what decides the mechanism.
    let root = toplevel(&asking).await;
    match listing_of(project_scope, &asking, root.as_deref(), &cwd) {
        Listing::NoProject => Err(no_repository(&asking)),
        Listing::Git { directory, prefix } => {
            // An empty list is not an answer to "why is there nothing here".
            // ADR-0010 declines a silent fallback for exactly this reason, and
            // `Ok(Vec::new())` on a failed subprocess was one. A repository with
            // nothing tracked in it is the one case that really is an empty
            // list, and it exits successfully.
            let Ok(output) = tokio::process::Command::new("git")
                .args(["ls-files", "-zco", "--exclude-standard"])
                .current_dir(&directory)
                .output()
                .await
            else {
                return Err(format!("git could not be run in {}", directory.display()));
            };
            // A race rather than the ordinary path now -- `listing_of` only
            // names `Git` where git named a root a moment earlier -- but a
            // repository can be taken away between the two calls.
            if !output.status.success() {
                return Err(no_repository(&directory));
            }
            Ok(candidates(
                &prefix,
                &String::from_utf8_lossy(&output.stdout),
            ))
        }
        Listing::Walk { directory, prefix } => Ok(under(&prefix, walked(&directory).await?)),
    }
}

/// The one spelling of a file the session is handed, whoever named it (#150).
///
/// Relative to the working directory when the file is under it, absolute when
/// it is not, with `.` and `..` spelled out lexically. The session finds an
/// open buffer by comparing paths as strings, and it does no I/O, so it cannot
/// be the one to decide that `src/main.rs` and `/w/proj/src/main.rs` are one
/// file. Every path that reaches it is spelled here -- the command line, a
/// language server's URI, `rg`, a listing, and the one the session sets for
/// itself, a name typed at save-as, once the write lands -- so a jump into an
/// open file finds its buffer rather than reading a second.
///
/// Lexical rather than `canonicalize`, which would resolve symlinks and needs
/// the file to exist: a path being saved to does not yet. The cost is a `..`
/// after a symlinked directory, which this collapses and the kernel would not.
fn spelled(path: &str, cwd: &std::path::Path) -> String {
    let normal = absolute_from(path, cwd);
    match normal.strip_prefix(cwd) {
        Ok(under) if !under.as_os_str().is_empty() => under.to_string_lossy().into_owned(),
        _ => normal.to_string_lossy().into_owned(),
    }
}

/// `path` made absolute against `cwd`, with `.` and `..` spelled out lexically.
fn absolute_from(path: impl AsRef<std::path::Path>, cwd: &std::path::Path) -> std::path::PathBuf {
    use std::path::Component;
    cwd.join(path)
        .components()
        .fold(std::path::PathBuf::new(), |mut built, part| {
            match part {
                Component::CurDir => {}
                Component::ParentDir => {
                    built.pop();
                }
                other => built.push(other),
            }
            built
        })
}

/// `SPC f Y`'s path: from the Project's root, or whole with `~` for home.
///
/// `+default/yank-buffer-path` is `(abbreviate-file-name (if root
/// (file-relative-name filename root) filename))`: relative to the root where
/// there is one, the whole path where there is not, and `~` for the home
/// directory either way -- which a path relative to a root never starts with.
fn yanked_path(
    file: &std::path::Path,
    root: Option<&std::path::Path>,
    home: Option<&std::path::Path>,
) -> String {
    match (root.and_then(|root| file.strip_prefix(root).ok()), home) {
        (Some(from_root), _) => from_root.to_string_lossy().into_owned(),
        (None, Some(home)) => match file.strip_prefix(home) {
            Ok(under) => format!("~/{}", under.display()),
            Err(_) => file.to_string_lossy().into_owned(),
        },
        (None, None) => file.to_string_lossy().into_owned(),
    }
}

/// `SPC f R`'s rows: the recent files under the Project's root, in the order
/// and spelling they came in.
///
/// `projectile-recentf-files` filters by `string-prefix-p` on the root; this is
/// the same filter by path components, so `/w/proj-old` is not inside
/// `/w/proj`. Rows keep the session's spelling rather than Doom's
/// root-relative one, because a row is also the path the picker opens.
fn recent_under(paths: &[String], root: &std::path::Path, cwd: &std::path::Path) -> Vec<String> {
    let root = absolute_from(root, cwd);
    paths
        .iter()
        .filter(|path| absolute_from(path, cwd).starts_with(&root))
        .cloned()
        .collect()
}

/// The answer to a [`meowr_app::FromRoot`] question, on its way back.
#[derive(Debug)]
enum FromRootAnswer {
    Yank { about: String, path: String },
    Recent { about: String, answer: Result<Vec<String>, String> },
    Scratch {
        about: String,
        row: meowr_app::ScratchRow,
        answer: Result<String, String>,
    },
}

/// Resolve the Project a [`meowr_app::FromRoot`] question is about, and answer it.
///
/// The root is `directory_of(Subject::Project)`, which the project searches
/// use: the repository root, or the buffer's directory with `project-scope`
/// off, or a refusal.
///
/// **A yank takes no stand-in root.** `project-scope` off is ADR-0010's "no
/// project support", consult's `nil`, so there is no root to be relative to and
/// the whole path is copied -- the same answer as outside a repository, where
/// Doom's `doom-project-root` is nil. The buffer's own directory would make the
/// yank a bare file name, which is `SPC f y`'s neighbour doing less. `SPC f R`
/// does take the stand-in, because a list of recent files "under here" is still
/// a useful list.
async fn answer_from_root(
    asked: meowr_app::FromRoot,
    project_scope: bool,
    cwd: std::path::PathBuf,
    home: Option<std::path::PathBuf>,
) -> FromRootAnswer {
    match asked {
        meowr_app::FromRoot::Yank { about } => {
            let root = if project_scope {
                directory_of(&meowr_app::Subject::Project(about.clone()), true)
                    .await
                    .ok()
            } else {
                None
            };
            let path = yanked_path(
                &absolute_from(&about, &cwd),
                root.as_deref(),
                home.as_deref().filter(|home| !home.as_os_str().is_empty()),
            );
            FromRootAnswer::Yank { about, path }
        }
        meowr_app::FromRoot::Recent { about, paths } => {
            let answer = directory_of(&meowr_app::Subject::Project(about.clone()), project_scope)
                .await
                .map(|root| recent_under(&paths, &root, &cwd));
            FromRootAnswer::Recent { about, answer }
        }
        // **A Project's scratch takes no stand-in root**, as a yank does not.
        // `project-scope` off is ADR-0010's "no project support", so there is
        // no Project for a scratch to belong to -- and the buffer's own
        // directory would make `*scratch: src*`, a name promising a Project
        // that is not there. A list of recent files "under here" is still a
        // useful list; a scratch buffer named after a directory is not.
        meowr_app::FromRoot::Scratch { about, row } => {
            let answer = match project_scope {
                true => directory_of(&meowr_app::Subject::Project(about.clone()), true)
                    .await
                    .map(|root| root.to_string_lossy().into_owned()),
                false => Err("project scope is off, so there is no project to have a scratch".into()),
            };
            FromRootAnswer::Scratch { about, row, answer }
        }
    }
}

/// Where the recent files are kept: `$XDG_STATE_HOME/meowr/recentf`.
///
/// State rather than cache, which is where Doom puts `recentf-save-file`: the
/// XDG spec's own example of state is "actions history", and a cache is
/// something that can be deleted without losing anything (#152). An empty
/// variable counts as unset, which the spec says, and with no `$HOME` either
/// there is nowhere to keep one.
fn recent_files_at(state_home: Option<&str>, home: Option<&str>) -> Option<std::path::PathBuf> {
    let state = match state_home.filter(|dir| !dir.is_empty()) {
        Some(dir) => std::path::PathBuf::from(dir),
        None => std::path::Path::new(home.filter(|dir| !dir.is_empty())?).join(".local/state"),
    };
    Some(state.join("meowr/recentf"))
}

/// Where Doom's one `recentf-exclude` points: `$XDG_RUNTIME_DIR`, or `/run`.
fn runtime_dir() -> std::path::PathBuf {
    std::env::var_os("XDG_RUNTIME_DIR")
        .filter(|dir| !dir.is_empty())
        .map_or_else(|| std::path::PathBuf::from("/run"), std::path::PathBuf::from)
}

/// The recent files as they are written: one absolute path per line.
///
/// Absolute, because the list is read by a session launched from somewhere
/// else. A path with a newline in it cannot survive a line per path and is
/// left out rather than turned into two, and nothing under the runtime
/// directory is kept.
fn recent_files_text<'a>(
    paths: impl Iterator<Item = &'a str>,
    cwd: &std::path::Path,
    runtime: &std::path::Path,
) -> String {
    paths
        .map(|path| absolute_from(path, cwd))
        .filter(|path| !path.starts_with(runtime))
        .map(|path| path.to_string_lossy().into_owned())
        .filter(|path| !path.contains('\n'))
        .map(|path| path + "\n")
        .collect()
}

/// The recent files read back, in the session's spelling (#150).
fn recent_files_read(
    text: &str,
    cwd: &std::path::Path,
    runtime: &std::path::Path,
) -> Vec<String> {
    text.lines()
        .filter(|line| !line.is_empty())
        .filter(|line| !std::path::Path::new(line).starts_with(runtime))
        .map(|line| spelled(line, cwd))
        .collect()
}

/// Where the recent files live, and the two facts reading and writing them need.
///
/// Three values that travel together to every step, so one type (#152).
struct RecentFilesKept {
    at: std::path::PathBuf,
    cwd: std::path::PathBuf,
    runtime: std::path::PathBuf,
}

impl RecentFilesKept {
    fn read(&self, text: &str) -> Vec<String> {
        recent_files_read(text, &self.cwd, &self.runtime)
    }
}

/// Keep the session's recent files, if there is anywhere to keep them.
///
/// On every way out that runs any code: a quit, a signal, and the terminal
/// going. Not from the panic hook, which Doom's `kill-emacs-hook` does not run
/// in either -- a crash costs one session's history, and leaves the file as the
/// last clean exit wrote it.
async fn keep_recent(recent: Option<&RecentFilesKept>, session: &Session) {
    if let Some(recent) = recent {
        keep_recent_files(
            &recent.at,
            session.recent_file_paths(),
            &recent.cwd,
            &recent.runtime,
        )
        .await;
    }
}

/// Write the recent files out, having dropped those that are no longer there.
///
/// Doom's `recentf-cleanup` then `recentf-save-list`, on quit. Nothing is said
/// if the write fails: meowr is on its way out and there is no frame left to
/// say it in, and a list not kept costs one session's history.
async fn keep_recent_files<'a>(
    at: &std::path::Path,
    paths: impl Iterator<Item = &'a str>,
    cwd: &std::path::Path,
    runtime: &std::path::Path,
) {
    let mut existing = Vec::new();
    for path in paths {
        if tokio::fs::try_exists(absolute_from(path, cwd)).await.unwrap_or(false) {
            existing.push(path);
        }
    }
    if let Some(parent) = at.parent() {
        let _ = tokio::fs::create_dir_all(parent).await;
    }
    let _ = tokio::fs::write(at, recent_files_text(existing.into_iter(), cwd, runtime)).await;
}

/// How long a formatter may take before it is killed. `rustfmt` on this
/// repository's largest file takes well under a second; ten is for a slow
/// formatter on a large file, not a hung one.
const FORMATTER_LIMIT: std::time::Duration = std::time::Duration::from_secs(10);

/// Why a buffer's write did not land.
enum Unwritten {
    /// The buffer's name was just typed and a file already has it.
    Taken,
    Failed(std::io::Error),
}

impl std::fmt::Display for Unwritten {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Unwritten::Taken => f.write_str("it already exists"),
            Unwritten::Failed(e) => e.fmt(f),
        }
    }
}

/// Write a buffer the session asked to be written, as every save does.
///
/// `SPC f s` and `save-some-buffers` both come through here, because the one
/// check only this layer can make is not optional for either: a name just
/// typed may belong to somebody else's file, and a save-as refused for that
/// leaves the buffer holding the name (#166). A write that lands takes its
/// auto-save with it -- `delete-auto-save-file-if-necessary`: the file now says
/// what the auto-save said, and one left beside it would offer to recover work
/// that is not missing.
async fn write_out(write: &meowr_app::Write) -> Result<bool, Unwritten> {
    if write.fresh && tokio::fs::try_exists(&write.path).await.unwrap_or(false) {
        return Err(Unwritten::Taken);
    }
    // The directory the file needs, as `move_or_copy` makes it for a file it is
    // putting somewhere new (#178). A write was the one file operation left
    // failing with the raw `io::Error` for a directory that is not there, and
    // `SPC f P` is the row that meets it: a config directory that has never
    // been made is the ordinary state of a machine that has never had a config
    // (#181). Answered rather than done quietly, because #172's rule for the
    // other two is that they are created *and said*.
    let mut made_directories = false;
    if let Some(parent) = std::path::Path::new(&write.path)
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        && !tokio::fs::try_exists(parent).await.unwrap_or(false)
    {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(Unwritten::Failed)?;
        made_directories = true;
    }
    tokio::fs::write(&write.path, &write.contents)
        .await
        .map_err(Unwritten::Failed)?;
    let _ = tokio::fs::remove_file(meowr_app::autosave_path(&write.path)).await;
    Ok(made_directories)
}

/// Copy or move a file, and say where it ended up (#178).
///
/// The name typed is resolved here because every question about it is one only
/// the filesystem answers: whether it names a directory (then the file keeps
/// its basename, as `doom/move-this-file` does), whether something is already
/// there (refused unless `SPC u` said otherwise), and whether its parents exist
/// (created, as `doom/copy-this-file` creates them). The path handed back is in
/// the one spelling (#150), because it is the path the buffer will hold.
///
/// A move falls back to copying and removing when the rename crosses a
/// filesystem, which is what `mv` does and what `rename(2)` refuses to.
async fn move_or_copy(
    asked: &meowr_app::FileRequest,
    respell: impl Fn(&str) -> String,
) -> Result<meowr_app::FileDone, meowr_app::FileRefusal> {
    use meowr_app::{FileDone, FileRefusal, FileWork};
    let failed = |e: std::io::Error| FileRefusal::Failed(e.to_string());
    let named = std::path::Path::new(&asked.to);
    let target = match tokio::fs::metadata(named).await {
        Ok(found) if found.is_dir() => named.join(
            std::path::Path::new(&asked.from)
                .file_name()
                .ok_or_else(|| FileRefusal::Failed("it has no name".to_string()))?,
        ),
        _ => named.to_path_buf(),
    };
    let path = respell(&target.to_string_lossy());
    if path == asked.from {
        return Err(FileRefusal::Itself);
    }
    if tokio::fs::try_exists(&target).await.unwrap_or(false) && !asked.force {
        return Err(FileRefusal::Taken(path));
    }
    let mut made_directories = false;
    if let Some(parent) = target.parent().filter(|parent| !parent.as_os_str().is_empty())
        && !tokio::fs::try_exists(parent).await.unwrap_or(false)
    {
        tokio::fs::create_dir_all(parent).await.map_err(failed)?;
        made_directories = true;
    }
    match asked.work {
        FileWork::Copy => {
            tokio::fs::copy(&asked.from, &target).await.map_err(failed)?;
        }
        FileWork::Move => {
            if tokio::fs::rename(&asked.from, &target).await.is_err() {
                tokio::fs::copy(&asked.from, &target).await.map_err(failed)?;
                // Said as both, because that is what is there: a copy that
                // landed and an original that would not go.
                tokio::fs::remove_file(&asked.from).await.map_err(|e| {
                    FileRefusal::Failed(format!("it was copied to {path} but not removed: {e}"))
                })?;
            }
        }
    }
    Ok(FileDone {
        path,
        made_directories,
    })
}

/// Delete a file, for `SPC f D` (#179).
///
/// An unlink and not a trash, which #173 decided and the sentence that asked
/// for the go-ahead says out loud rather than implying it can be undone: a
/// trash is a desktop specification meowr would be implementing on a guess.
///
/// Only the reason comes back. Where the file went is not a question -- it is
/// gone -- so unlike `move_or_copy` there is nothing for the host to resolve
/// and hand over.
async fn delete_file(asked: &meowr_app::FileDeletion) -> Result<(), String> {
    tokio::fs::remove_file(&asked.path)
        .await
        .map_err(|e| e.to_string())
}

/// A server's workspace edit made ready for the session (#167, ADR-0013).
///
/// Every path is given the one spelling the session knows files by (#150): a
/// server names them absolute, and a rename that did not respell would find no
/// buffer visiting any of them. Then every file no buffer visits is read, since
/// reading is I/O and the session does none -- one that cannot be read is handed
/// along as that, for the session to refuse the whole edit naming it.
async fn with_files_read(
    changes: Vec<meowr_app::FileChange>,
    respell: impl Fn(&str) -> String,
    visited: impl Fn(&str) -> bool,
) -> Vec<meowr_app::FileChange> {
    use meowr_app::FileChange;
    let mut ready = Vec::with_capacity(changes.len());
    for change in changes {
        ready.push(match change {
            FileChange::Edit(file) => {
                let path = respell(&file.path);
                let read = match visited(&path) {
                    true => None,
                    false => Some(tokio::fs::read_to_string(&path).await.map_err(|e| e.to_string())),
                };
                FileChange::Edit(meowr_app::FileEdit { path, read, ..file })
            }
            FileChange::Create { path } => FileChange::Create { path: respell(&path) },
            FileChange::Rename { from, to } => FileChange::Rename {
                from: respell(&from),
                to: respell(&to),
            },
            FileChange::Delete { path } => FileChange::Delete { path: respell(&path) },
        });
    }
    ready
}

/// A reply that is a workspace edit, made ready for the session: its paths
/// respelled and the files no buffer visits read, against the session as it
/// stands now. Any other reply is handed along as it came.
async fn with_edit_read(
    session: &meowr_app::Session,
    reply: meowr_app::EditReply,
    respell: impl Fn(&str) -> String,
) -> meowr_app::EditReply {
    match reply {
        meowr_app::EditReply::Edit(changes) => meowr_app::EditReply::Edit(
            with_files_read(changes, respell, |path| session.revision_of(path).is_some()).await,
        ),
        other => other,
    }
}

/// A code action question's answer: the question, the reply, and what the
/// server said it does with code actions -- which decides what an action with
/// no edit can become (#169).
type ActionsAnswer = (
    meowr_app::CodeActionQuestion,
    Result<serde_json::Value, lsp::Error>,
    lsp::CodeActionSupport,
);

/// Put the session's pending code action question, if it has one (#169).
///
/// A chosen action that came with its edit needs nothing more from the server,
/// and is applied here, inline, as a rename's files are read. The list, and an
/// action to resolve, go to the server; its answer comes back on `answers`. A
/// server that said it has no code actions is refused before anything is
/// asked.
async fn put_code_action_question(
    session: &mut meowr_app::Session,
    servers: &std::collections::HashMap<String, std::sync::Arc<lsp::Connection>>,
    home: Option<&str>,
    respell: impl Fn(&str) -> String,
    answers: &tokio::sync::mpsc::Sender<ActionsAnswer>,
) {
    use meowr_app::{CodeActionEffect, CodeActionQuestion, CodeActionsFound, EditReply};
    let Some(asked) = session.pending_code_actions() else {
        return;
    };
    if let CodeActionQuestion::Apply(chosen) = &asked
        && let CodeActionEffect::Edit(changes) = &chosen.action.effect
    {
        let answer = with_edit_read(session, EditReply::Edit(changes.clone()), respell).await;
        session.code_action_applied(chosen, answer);
        return;
    }
    let about = asked.asked();
    let Some(connection) = connection_for(session, servers, home, &about.path) else {
        session.say("No language server for this file.");
        return;
    };
    let support = lsp::code_action_support_of(connection.capabilities());
    let request = match &asked {
        CodeActionQuestion::List(listing) if support == lsp::CodeActionSupport::None => {
            session.code_actions_found(listing, CodeActionsFound::Unsupported);
            return;
        }
        CodeActionQuestion::List(listing) => {
            let Some(doc) = session.buffer(listing.buffer).document() else {
                return;
            };
            ("textDocument/codeAction", lsp::code_actions_request(listing, doc))
        }
        CodeActionQuestion::Apply(chosen) => match lsp::resolve_request(&chosen.action) {
            Some(params) => ("codeAction/resolve", params),
            None => {
                session.code_action_applied(
                    chosen,
                    EditReply::Failed("the action as the server sent it could not be read back".into()),
                );
                return;
            }
        },
    };
    let (method, params) = request;
    let answers = answers.clone();
    tokio::spawn(async move {
        let reply = connection.request(method, params).await;
        let _ = answers.send((asked, reply, support)).await;
    });
}

/// Run a formatter over `text` and say what it came to (#164, ADR-0014).
///
/// apheleia's plumbing: the text on stdin, the result on stdout, no shell. A
/// program that is not there is `Missing` rather than a failure, so the session
/// can say that in its own words. stderr's first line is kept, which is where
/// every formatter here puts the reason (`rustfmt`'s `error: expected ...`).
///
/// **Given up on after `limit`**, and killed: the task is detached, so nothing
/// else would ever stop a formatter that hangs, and each press would leave
/// another running. apheleia stops a previous run when a new one starts; a limit
/// covers that and a save nobody repeats.
async fn run_formatter(
    invocation: &meowr_command::Invocation,
    text: &str,
    limit: std::time::Duration,
) -> meowr_app::Formatted {
    use tokio::io::AsyncWriteExt;
    let spawned = tokio::process::Command::new(&invocation.program)
        .args(&invocation.args)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn();
    let mut child = match spawned {
        Ok(child) => child,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return meowr_app::Formatted::Missing,
        Err(e) => return meowr_app::Formatted::Failed(e.to_string()),
    };
    // Written from its own task: a formatter that writes a large output before
    // it has read all of its input would otherwise wait on a full stdout pipe
    // while this waits on a full stdin one.
    let feeding = child.stdin.take().map(|mut stdin| {
        let text = text.to_string();
        tokio::spawn(async move {
            let _ = stdin.write_all(text.as_bytes()).await;
        })
    });
    // Dropping the timed-out future drops the child, and `kill_on_drop` ends it.
    let Ok(output) = tokio::time::timeout(limit, child.wait_with_output()).await else {
        return meowr_app::Formatted::Failed(format!(
            "did not finish within {}s",
            limit.as_secs_f32()
        ));
    };
    if let Some(feeding) = feeding {
        let _ = feeding.await;
    }
    match output {
        Ok(output) if output.status.success() => {
            meowr_app::Formatted::Output(String::from_utf8_lossy(&output.stdout).into_owned())
        }
        Ok(output) => meowr_app::Formatted::Failed(
            String::from_utf8_lossy(&output.stderr)
                .lines()
                .find(|line| !line.trim().is_empty())
                .map_or_else(|| format!("exited with {}", output.status), str::to_string),
        ),
        Err(e) => meowr_app::Formatted::Failed(e.to_string()),
    }
}

/// Every file under the directory `path` is in, for `SPC f F` (#149).
///
/// `project_of` with the scope off -- git's listing of the directory where a
/// repository sits above it, a walk where none does -- **and a walk where git
/// lists nothing**. Doom's branch for a directory below the root lists every
/// file, ignored or not (`project-find-file-in` with `include-all`); keeping
/// git's filter is what keeps `target/` out, and a divergence taken on purpose.
/// But in a directory git ignores whole, the filter's answer is an empty list,
/// and "nothing here" is false with the files on screen. A tracked directory
/// always lists at least the buffer's own file, so empty means ignored.
async fn files_under(path: &str) -> Result<Vec<String>, String> {
    match project_of(Some(path), false).await {
        Ok(rows) if rows.is_empty() => {
            let Ok(cwd) = std::env::current_dir() else {
                return Err("the working directory could not be read".into());
            };
            let Some(directory) = asking_about(Some(path), &cwd) else {
                return Err(format!("`{path}` has no directory to look in"));
            };
            match listing_of(false, &directory, None, &cwd) {
                Listing::Walk { directory, prefix } => Ok(under(&prefix, walked(&directory).await?)),
                // `listing_of` walks whenever it is given no root.
                _ => Ok(rows),
            }
        }
        answer => answer,
    }
}

/// Where a [`Subject`] actually is.
///
/// The session names the subject and this resolves it, which is ADR-0010's own
/// division: a Project is `git rev-parse --show-toplevel` away, and both that
/// question and the refusal when there is no repository belong to the side that
/// knows where it looked. `Here` and `Directory` need no repository and so
/// cannot refuse for want of one.
///
/// `project-scope` off turns `Project` into `Here`, which is the rescue that
/// Option exists to be: outside a repository the row searches the directory
/// rather than refusing.
async fn directory_of(
    subject: &meowr_app::Subject,
    project_scope: bool,
) -> Result<std::path::PathBuf, String> {
    let Ok(cwd) = std::env::current_dir() else {
        return Err("the working directory could not be read".into());
    };
    let named = |path: &str| {
        asking_about(Some(path), &cwd).ok_or_else(|| format!("`{path}` has no directory to look in"))
    };
    match subject {
        meowr_app::Subject::Here(path) => named(path),
        meowr_app::Subject::Directory(path) => {
            let at = std::path::Path::new(path);
            if at.is_dir() {
                Ok(at.to_path_buf())
            } else {
                Err(format!("{path} is not a directory"))
            }
        }
        meowr_app::Subject::Project(path) => {
            let asking = named(path)?;
            if !project_scope {
                return Ok(asking);
            }
            toplevel(&asking).await.ok_or_else(|| no_repository(&asking))
        }
    }
}

/// Why there is no Project here.
///
/// One sentence in one place: `project_of` reaches this from the decision and
/// again from a `git ls-files` that failed, and two spellings of one refusal is
/// drift a reader has to diff to be sure about.
fn no_repository(at: &std::path::Path) -> String {
    format!("{} is in no git repository", at.display())
}

/// Whether a directory entry is one the walk passes over.
///
/// The dot, and nothing else. That is `fd`'s default -- what consult runs when
/// `consult-project-function` is `nil` -- minus the `.gitignore` half, which
/// has nothing to read here: being outside a repository is most of what having
/// no `.gitignore` *means*. `.git` is the entry it most has to catch, since a
/// linked worktree keeps a whole tree beneath one.
fn hidden(name: &std::ffi::OsStr) -> bool {
    name.to_string_lossy().starts_with('.')
}

/// Every file under `directory`, relative to it and sorted.
///
/// What `project-scope` off reaches outside a repository, where `git ls-files`
/// exits 128 and has nothing to say. Sorted because `git ls-files` is, and a
/// picker whose rows move between runs is one you cannot learn.
///
/// A symlink to a *file* is listed and a symlink to a *directory* is left out of
/// both halves: not descended into, because following links is how a walk finds
/// a cycle to go round for ever, and not offered either, because it is not a
/// file and a row the picker cannot open is #120's whole class. A link whose
/// target cannot be read at all is still offered, as `git ls-files` offers a
/// broken one it tracks.
///
/// Three things are skipped in silence and each is a gap in the listing rather
/// than a reason there is none -- the difference ADR-0010 keeps drawing, where
/// only the second is something to say. A subdirectory that cannot be read: its
/// rows are missing and the rest are not. An entry whose type will not come
/// back: one row. A directory that stops part way through being read: the rest
/// of that one directory. `directory` itself failing is the other kind, and is
/// returned.
async fn walked(directory: &std::path::Path) -> Result<Vec<String>, String> {
    let mut rows = Vec::new();
    let mut pending = vec![std::path::PathBuf::new()];
    while let Some(relative) = pending.pop() {
        let at = directory.join(&relative);
        let mut entries = match tokio::fs::read_dir(&at).await {
            Ok(entries) => entries,
            Err(e) if relative.as_os_str().is_empty() => {
                return Err(format!("{} could not be read: {e}", at.display()));
            }
            Err(_) => continue,
        };
        while let Ok(Some(entry)) = entries.next_entry().await {
            let name = entry.file_name();
            if hidden(&name) {
                continue;
            }
            let Ok(kind) = entry.file_type().await else {
                continue;
            };
            // `file_type` does not follow a link, which is what the descent
            // wants and the opposite of what the row wants: a link to a
            // directory comes back "not a directory" and was being offered as a
            // file. Resolved just far enough to tell them apart.
            if kind.is_symlink()
                && matches!(tokio::fs::metadata(at.join(&name)).await, Ok(it) if it.is_dir())
            {
                continue;
            }
            let path = relative.join(&name);
            if kind.is_dir() {
                pending.push(path);
            } else {
                rows.push(path.to_string_lossy().into_owned());
            }
        }
    }
    rows.sort();
    Ok(rows)
}

/// The repository root above a directory, or `None` where there is none.
///
/// Asked of git rather than found by walking up for `.git`, because `.git` is a
/// file in a linked worktree and a submodule's own in a submodule, and git
/// already knows both (ADR-0010). It also keeps the root and the listing
/// answered by one authority: split them and a root can exist where
/// `git ls-files` returns nothing.
async fn toplevel(directory: &std::path::Path) -> Option<std::path::PathBuf> {
    let output = tokio::process::Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .current_dir(directory)
        .output()
        .await
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let said = String::from_utf8_lossy(&output.stdout).trim().to_string();
    (!said.is_empty()).then(|| std::path::PathBuf::from(said))
}

/// `git ls-files` output, as paths that resolve from the working directory.
///
/// Split from [`project_of`] so the part with a decision in it can be tested
/// without a repository or a subprocess — the shape #119's own
/// `Connection::over` took, for the same reason.
///
/// Split on NUL rather than by line: `git ls-files -z` is asked for, because a
/// filename may contain a newline and `lines()` turned one such file into two
/// candidates, neither of which opens.
///
/// `git ls-files` prints paths relative to the directory it ran in. When that
/// is not the directory paths are resolved against, every candidate needs the
/// difference put back, or picking one reports "No such file or directory" for
/// a file that is plainly there (#120).
fn candidates(prefix: &std::path::Path, listing: &str) -> Vec<String> {
    under(
        prefix,
        listing
            .split('\0')
            .filter(|row| !row.is_empty())
            .map(str::to_string),
    )
}

/// Put back the difference between where a listing ran and where paths resolve.
///
/// Shared by both mechanisms, because the correction is a property of the
/// `prefix` rather than of whatever produced the rows -- and #120 was one
/// listing whose rows resolved against somewhere else.
fn under(prefix: &std::path::Path, rows: impl IntoIterator<Item = String>) -> Vec<String> {
    rows.into_iter()
        .map(|row| {
            if prefix.as_os_str().is_empty() {
                row
            } else {
                prefix.join(row).to_string_lossy().into_owned()
            }
        })
        .collect()
}

/// The unsaved work, kept where a panic hook can reach it.
///
/// A panic runs nothing but its hooks, and a hook cannot borrow the session —
/// so what it writes has to have been put here in advance. `Rope` clones share
/// their nodes, so keeping this current after every key costs nothing worth
/// counting.
///
/// Named for what it holds rather than for the disaster it is held against:
/// this is the unsaved work, and it is unsaved work whether or not anything
/// ever goes wrong.
type Unsaved = std::sync::Arc<std::sync::Mutex<Vec<meowr_app::Snapshot>>>;

/// Write the held snapshot out, synchronously, from inside a panic.
///
/// `into_inner` on a poisoned lock rather than a panic of its own: the process
/// is already coming down, and a second panic inside the hook would take the
/// work with it.
fn flush_unsaved(unsaved: &Unsaved) {
    let held = unsaved.lock().unwrap_or_else(|e| e.into_inner());
    for snapshot in held.iter() {
        let _ = std::fs::write(&snapshot.path, snapshot.text.to_string());
    }
}

/// Delete the auto-saves of buffers that have been killed.
///
/// The counterpart of `write_autosaves`, and the same division: the session
/// decides which files should no longer exist and the binary that owns the
/// filesystem carries it out. Drained, so a removal is attempted once.
async fn remove_forgotten_autosaves(session: &mut Session) {
    for path in session.take_autosaves_to_forget() {
        let _ = tokio::fs::remove_file(&path).await;
    }
}

/// Take the auto-save files away, for a quit that meant it.
///
/// A quit that got past `Session::quit` either had nothing unsaved or was
/// `SPC q Q`, which is a decision. Leaving an auto-save behind either way
/// would offer, on the next visit, to recover work that was saved or thrown
/// away on purpose.
async fn remove_autosaves(session: &Session) {
    for buffer in session.buffers() {
        if let Some(path) = buffer.path() {
            let _ = tokio::fs::remove_file(meowr_app::autosave_path(path)).await;
        }
    }
}

/// Resolve when the process is being asked to end.
///
/// One future over both signals rather than two `select!` arms that did the
/// same thing: what matters is that the process is going down, not which of
/// the two said so.
async fn going_down(
    terminate: &mut tokio::signal::unix::Signal,
    hangup: &mut tokio::signal::unix::Signal,
) {
    tokio::select! {
        _ = terminate.recv() => {}
        _ = hangup.recv() => {}
    }
}

/// Write out what the session says is due.
///
/// Errors are swallowed on purpose. An auto-save is a safety net, and a net
/// that interrupts you to say it could not be strung is worse than one that
/// quietly is not there — the visible failure that matters is the one
/// `write-file` reports, on the path the user actually asked for.
async fn write_autosaves(session: &mut Session, force: bool) {
    for autosave in session.take_autosaves(force) {
        let _ = tokio::fs::write(&autosave.path, &autosave.contents).await;
    }
}

/// Say so if unsaved work from a previous run is sitting beside `path`.
///
/// "Newer than the file" is the test Emacs makes, and it is the whole
/// question: an auto-save older than the file is work that was saved, and
/// offering it back would offer to undo the save.
async fn offer_recovery(session: &mut Session, path: &str) {
    let auto = meowr_app::autosave_path(path);
    let (Ok(saved), Ok(automatic)) = (
        tokio::fs::metadata(path).await,
        tokio::fs::metadata(&auto).await,
    ) else {
        return;
    };
    let (Ok(saved), Ok(automatic)) = (saved.modified(), automatic.modified()) else {
        return;
    };
    if automatic > saved {
        session.offer_recovery(auto);
    }
}

/// How large a file a preview will read.
///
/// A preview happens while you are typing, so the thing it must never do is
/// stall the keyboard on one candidate. A quarter of a megabyte is far more
/// than the screenful a preview shows and small enough that arrowing down a
/// list of project files stays smooth.
///
/// The limit refuses rather than truncates. A preview cut off at a byte count
/// looks exactly like a file that ends there, and a preview that lies about
/// what it is showing is worse than one that says it will not show it.
const PREVIEW_LIMIT: u64 = 256 * 1024;

/// Read the file the session is waiting on, off the loop.
///
/// Spawned rather than awaited, for the reason the file named on the command
/// line is: a large one must not stop the editor drawing or taking keys. A
/// function rather than an arm because two doors lead here -- a key, and an
/// answer that turned out to be a jump.
fn read_pending_open(session: &mut Session, opened: &tokio::sync::mpsc::Sender<(String, Opened)>) {
    // Asked before the path is taken: it is a fact about the question, and the
    // question is what is being drained.
    let may_be_new = session.pending_open_may_be_new();
    let Some(path) = session.pending_open() else {
        return;
    };
    let path = path.to_string();
    let tx = opened.clone();
    tokio::spawn(async move {
        let read = tokio::fs::read_to_string(&path).await;
        let _ = tx.send((path.clone(), what_was_read(read, may_be_new, &path))).await;
    });
}

/// What a read came to, for a row that may or may not mind a file being absent
/// (#181).
///
/// The one decision in reading a file for a buffer, so it is a function of its
/// arguments and nothing else. `config::evaluate` draws the same line for the
/// config it *runs* -- [`Failed::Unreadable`] is documented there as distinct
/// from absent -- and this draws it for the config you open to edit.
fn what_was_read(read: std::io::Result<String>, may_be_new: bool, path: &str) -> Opened {
    match read {
        Ok(text) => Opened::Read(text),
        Err(e) if may_be_new && e.kind() == std::io::ErrorKind::NotFound => Opened::Missing,
        Err(e) => Opened::Failed(format!("{e}: {path}")),
    }
}

/// Read `path` for a preview, answering with a message rather than an error.
///
/// The message becomes the buffer's `Failed` content, which the UI already
/// knows how to draw: "not yet" and "no" are states a buffer has, so a file
/// that will not open previews as the reason it will not.
async fn read_for_preview(path: &str) -> Result<String, String> {
    let size = tokio::fs::metadata(path)
        .await
        .map_err(|e| format!("{e}: {path}"))?
        .len();
    if size > PREVIEW_LIMIT {
        return Err(format!(
            "{path} is {size} bytes — too large to preview; open it to read it"
        ));
    }
    let bytes = tokio::fs::read(path)
        .await
        .map_err(|e| format!("{e}: {path}"))?;
    // Lossy rather than refusing: a file that is not quite UTF-8 is still
    // worth a glance, and a preview is a glance.
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

/// One line for the Echo Area, saying how many others there are.
///
/// The Echo Area is a single line by definition, so a config with four problems
/// has to say so there and keep the rest in `*config-error*` — telling you
/// about one of four would be worse than telling you about none.
fn first_line(problems: &[String]) -> String {
    match problems.split_first() {
        Some((first, [])) => format!("init.scm: {first}"),
        Some((first, rest)) => format!("init.scm: {first} (and {} more)", rest.len()),
        None => "init.scm: it did not resolve".into(),
    }
}

/// What became of the config, decided before the terminal was taken.
enum ConfigOutcome {
    /// It ran, and this is what it asked for. Empty when there was no config,
    /// which is the same thing to the editor and silent either way: an absent
    /// config is the normal state of a fresh checkout, so nothing is wrong and
    /// nothing is said.
    Declared(meowr_app::declare::Declarations),
    /// `--no-config`.
    Skipped,
    Failed {
        summary: String,
        detail: String,
    },
}

/// What a server published about one document, on its way to the session.
///
/// A type rather than the `(String, Option<u64>, Value)` this was: the three
/// travel together everywhere they go, and an `Option<u64>` in the middle of a
/// tuple is the element a reader has to stop and work out.
struct Published {
    uri: String,
    /// The revision the answer was computed against, when the server said.
    version: Option<u64>,
    params: Value,
}

/// The connection serving a path, if one is up.
///
/// Both hovers ask this same question -- the key-asked one and the idle one --
/// and asking it twice in two arms is how they come to disagree about *which*
/// server a path belongs to. The lookup goes through the session's own
/// `served` list rather than a map of this layer's, for the reason #119 gives:
/// with two Languages open, whichever connection came first is the wrong one.
fn connection_for(
    session: &meowr_app::Session,
    servers: &std::collections::HashMap<String, std::sync::Arc<lsp::Connection>>,
    home: Option<&str>,
    path: &str,
) -> Option<std::sync::Arc<lsp::Connection>> {
    let program = session
        .served()
        .iter()
        .find(|served| served.path == path)?
        .server
        .resolved(home);
    servers.get(&program).cloned()
}

/// A hover that came back, and what had asked for it.
///
/// One type with a discriminant rather than two channels, because the two
/// differ in ways the receiving arm has to know about anyway: what silence
/// means, and whether the answer can have gone stale on the way back.
enum Answered {
    /// `SPC k k`. Carries the file the question was about, because the buffer
    /// can change while the server thinks and an answer about a file no longer
    /// on screen is about text that is not there -- #124. An empty answer is
    /// said out loud, silence after a key reading as a key that did not work,
    /// and the session decides that too: it is the same decision.
    ByKey(String, Option<String>),
    /// The point rested -- #121. Carries where it rested, because a hover is
    /// keyed to a *position* and the point moves without the buffer changing,
    /// so there is no revision alone to check the answer against.
    AtRest(meowr_app::Resting, Option<String>),
}

/// A buffer a language server has something to say about.
///
/// `text` is `None` when the server has already been told this revision, which
/// is the common case and the reason it is an `Option`: an idle editor must not
/// clone a rope per pass just to discover nothing changed.
/// One buffer's language-server needs, owned so the borrow of the session ends.
///
/// Distinct from [`meowr_app::Served`], which borrows: the host has to spawn and
/// send while holding `&mut session`, so it takes what it needs by value first.
struct Wanted {
    /// The program to run, with `~/` already expanded.
    program: String,
    args: Vec<String>,
    path: String,
    /// meowr's own Language name, which is also LSP's `languageId`.
    language: String,
    revision: u64,
    /// The text to send, or `None` when this server was already told this
    /// revision -- which is the common case and why it is an `Option`.
    text: Option<String>,
}

async fn run(
    terminal: &mut ratatui::DefaultTerminal,
    path: Option<String>,
    config: ConfigOutcome,
    config_at: Option<std::path::PathBuf>,
) -> anyhow::Result<Ending> {
    // How the loop below ended, which only `SPC q r` ever changes. Every other
    // way out is a quit: the terminal has gone, the stream has broken, or the
    // editor was asked to stop.
    let mut ending = Ending::Quit;
    // Read once: meowr never changes directory, and every path the session is
    // handed is spelled against this (#150).
    let cwd = std::env::current_dir().ok();
    let respell = |path: &str| {
        cwd.as_deref()
            .map_or_else(|| path.to_string(), |cwd| spelled(path, cwd))
    };
    let respelled_rows = |rows: Vec<String>| -> Vec<String> {
        rows.iter().map(|row| respell(row)).collect()
    };
    let respelled_places = |places: Vec<meowr_domain::Place>| -> Vec<meowr_domain::Place> {
        places
            .into_iter()
            .map(|place| meowr_domain::Place {
                path: respell(&place.path),
                ..place
            })
            .collect()
    };
    let path = path.map(|path| respell(&path));
    // Where the recent files are kept, and what not to keep (#152).
    let recent = match (
        recent_files_at(
            std::env::var("XDG_STATE_HOME").ok().as_deref(),
            std::env::var("HOME").ok().as_deref(),
        ),
        cwd.clone(),
    ) {
        (Some(at), Some(cwd)) => Some(RecentFilesKept {
            at,
            cwd,
            runtime: runtime_dir(),
        }),
        _ => None,
    };
    let size = terminal.size()?;
    let mut session = Session::new(Viewport {
        top: 0,
        height: size.height as usize,
    });
    session.on_resize(size.width, size.height);
    // Read before the launch file lands, so its visit goes on top of the old
    // list rather than being replaced by it.
    if let Some(recent) = &recent {
        session.exclude_from_recent(&recent.runtime.to_string_lossy());
        match tokio::fs::read_to_string(&recent.at).await {
            Ok(text) => session.set_recent_files(recent.read(&text)),
            // The first session there has ever been, which is not news.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            // Anything else is: a list that is there and cannot be read would
            // otherwise be `SPC RET` saying "No recent files." about files it
            // has, and the next quit writing over them.
            Err(e) => session.say(&format!(
                "The recent files could not be read: {e}: {}",
                recent.at.display()
            )),
        }
    }
    // Where the config rows go, which is the host's fact: finding it means
    // `XDG_CONFIG_HOME` and `HOME`, and the session reads no environment (#181).
    //
    // In the one spelling, like every other path the session is handed (#150):
    // `SPC f p`'s rows come back through `respelled_rows`, so a meowr started
    // inside the config directory would otherwise have the picker offering
    // `init.scm` and `SPC f P` offering the absolute path -- one file, two
    // spellings, two buffers.
    session.set_config_path(config_at.map(|at| respell(&at.to_string_lossy())));
    // Said once the session exists, so the Echo Area can carry it — the
    // terminal was not taken when the config ran, which is the whole point of
    // running it then.
    match config {
        // Applied as one, or not at all: a set that does not resolve leaves the
        // defaults standing rather than half a config (#59).
        ConfigOutcome::Declared(declarations) => {
            if let Err(problems) = session.apply(declarations) {
                let said: Vec<String> = problems.iter().map(|p| p.to_string()).collect();
                session.config_failed(&first_line(&said), &said.join("\n"));
            }
        }
        ConfigOutcome::Skipped => session.config_skipped(),
        ConfigOutcome::Failed { summary, detail } => session.config_failed(&summary, &detail),
    }

    // The file is read on the runtime rather than before the loop starts, so a
    // large file cannot stop the editor from drawing or accepting keys.
    let (parsed_tx, mut parsed_rx) = tokio::sync::mpsc::channel(1);
    // Files chosen from the picker, read off the loop. Separate from `tx`
    // below, which carries the one file the command line named: that one
    // becomes the buffer already on screen and this one becomes a new buffer,
    // so they land in the session by different doors.
    let (opened_tx, mut opened_rx) =
        tokio::sync::mpsc::channel::<(String, Opened)>(1);
    let (tx, mut rx) = tokio::sync::mpsc::channel(1);
    if let Some(path) = &path {
        session.set_path(path);
    }
    // The branch, read once at startup. Running `git` is the host's business;
    // nothing below this layer is allowed to shell out, and a modeline that
    // ran a subprocess per frame would be a different kind of mistake.
    session.set_branch(branch_of(path.as_deref()).await);
    // Walking a directory is I/O, so the host does it and hands the result in.
    // No Project is looked up here. It is asked for on the first row that wants
    // one (#130), which is a question most sessions never put -- and a session
    // that opens files from two repositories needs a different answer for each,
    // so one computed at startup would be wrong for the second of them anyway.
    // Before the file is read, so that the offer is on screen from the first
    // frame rather than appearing once the load lands.
    if let Some(path) = &path {
        offer_recovery(&mut session, path).await;
    }
    match path.clone() {
        Some(path) => {
            tokio::spawn(async move {
                let loaded = tokio::fs::read_to_string(&path)
                    .await
                    // Reason first, path second: the message is clipped to the
                    // window width, and clipping away *why* it failed leaves the
                    // user staring at the path they already typed.
                    .map_err(|e| format!("{e}: {path}"));
                let _ = tx.send(loaded).await;
            });
        }
        None => session.loaded(Document::default()),
    }

    // The language server, if there is one for this file and it is installed.
    // Non-fatal either way: meowr is an editor before it is a client, and a
    // machine without `rust-analyzer` gets an editor rather than an error.
    //
    // Hardcoded to one server for `.rs`, which is #117's decision and its
    // reason: *which* command a Language runs is the per-Language question
    // #108's map left named and unscheduled, and one language honestly
    // hardcoded is what this milestone asks for.
    // One connection per server program. See the sweep below for why it is keyed
    // that way rather than one connection for the session.
    let mut servers: std::collections::HashMap<String, std::sync::Arc<lsp::Connection>> =
        std::collections::HashMap::new();
    // Programs that would not start, so the message is said once rather than
    // repainted every time the loop turns.
    let mut refused: std::collections::HashSet<String> = std::collections::HashSet::new();
    // Read once, here, because reading the environment is the binary's job:
    // `LanguageServer::resolved` takes it as a parameter precisely so that a
    // domain crate does not do this.
    let home = std::env::var("HOME").ok();
    // What the draining task knows: which document, which version, and what
    // was said. Resolving the URI to a `BufferId` needs the session, which
    // lives on the other side of this channel.
    //
    // Sixteen deep because a server answers a whole workspace at once -- the
    // cold-start burst on this repository is a dozen files in a second -- and
    // four for hovers because a hover is one key, or one point come to rest,
    // and the answer is drawn before the next one can ask for another.
    let (diagnostics_tx, mut diagnostics_rx) = tokio::sync::mpsc::channel::<Published>(16);
    let (symbols_tx, mut symbols_rx) =
        tokio::sync::mpsc::channel::<(String, Vec<meowr_domain::Place>)>(16);
    let (searches_tx, mut searches_rx) =
        tokio::sync::mpsc::channel::<Result<Vec<meowr_domain::Place>, String>>(16);
    let (hovers_tx, mut hovers_rx) = tokio::sync::mpsc::channel::<Answered>(4);
    // What a server found, and which file it was asked about. The stamp is
    // ADR-0009's, for the reason every other late answer carries one.
    let (found_tx, mut found_rx) =
        tokio::sync::mpsc::channel::<(String, Vec<meowr_domain::Place>)>(4);
    // A rename's answers, with the question each answers: the question is the
    // stamp, and says whether the answer is a name to read or an edit (#168).
    let (renames_tx, mut renames_rx) = tokio::sync::mpsc::channel::<(
        meowr_app::RenameQuestion,
        Result<serde_json::Value, lsp::Error>,
    )>(4);
    // A code action question's answers, with the question and whether the
    // server said it resolves actions -- which decides what an action with no
    // edit can become (#169).
    let (actions_tx, mut actions_rx) = tokio::sync::mpsc::channel::<ActionsAnswer>(4);
    // A Project is asked for lazily and answered off the loop, for the reason a
    // hover is: `git ls-files -zco` over a large repository is not something to
    // hold the editor still for, and this machine has one with 225 of them under
    // a single directory.
    let (projects_tx, mut projects_rx) =
        tokio::sync::mpsc::channel::<(Option<String>, Result<Vec<String>, String>)>(1);
    // `SPC f F`'s listing, stamped with the buffer it is about. Not the
    // projects channel: that answer is the picker's project source (#149).
    let (files_tx, mut files_rx) =
        tokio::sync::mpsc::channel::<(Files, Result<Vec<String>, String>)>(1);
    // `SPC f Y` and `SPC f R`, answered once the Project's root is known (#148).
    let (from_root_tx, mut from_root_rx) = tokio::sync::mpsc::channel::<FromRootAnswer>(1);
    // A formatter's outcome, with the question it answers so the session can
    // check the revision it was asked about (ADR-0013, ADR-0014).
    let (formats_tx, mut formats_rx) =
        tokio::sync::mpsc::channel::<(meowr_app::Format, meowr_app::Formatted)>(1);
    // One way to run a formatter, reached from `SPC k f` and from a save. The
    // session never knows the home directory; this is the one place a `~/` in
    // a formatter's program is expanded.
    let run_format = |asked: meowr_app::Format| {
        let invocation = asked.invocation.clone().at_home(home.as_deref());
        let tx = formats_tx.clone();
        tokio::spawn(async move {
            let outcome = run_formatter(&invocation, &asked.text, FORMATTER_LIMIT).await;
            let _ = tx.send((asked, outcome)).await;
        });
    };
    // What each path was last told about, so a buffer that has moved on gets a
    // `didChange` and one that has not costs a `u64` comparison.
    // Kept with the program told, so a path can be closed with the server that
    // opened it.
    let mut told: std::collections::HashMap<String, (String, u64)> = std::collections::HashMap::new();

    let mut events = EventStream::new();
    // The frame on screen, or `None` before the first one is painted.
    let mut shown: Option<Screen> = None;
    let mut last_reserved: u16 = 0;

    // A kill signal is one of the ways work was being lost, and the only one
    // the process gets any warning about. `Ctrl-C` does not arrive here —
    // raw mode delivers it as a key — so this is `kill`, a logout, or a
    // session manager shutting the machine down.
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    let mut hangup = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::hangup())?;

    // Chained onto the hook `ratatui::init` installed, which restores the
    // terminal. Ours runs first: restoring a terminal is worth less than not
    // losing the work, and if the one below it aborts, this has already run.
    let unsaved: Unsaved = Unsaved::default();
    let held = unsaved.clone();
    let restore = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        flush_unsaved(&held);
        restore(info);
    }));

    loop {
        // Armed only while hints are showing, so an idle editor waits on the
        // keyboard and nothing else. `meow-expand-hint-remove-delay`.
        let expiry = session
            .hints_pending()
            .then(|| tokio::time::sleep(session.options().expand_hint_delay));

        // The preview waits for the selection to sit still. Debounced rather
        // than immediate because previewing on every keystroke of a fuzzy
        // search is unreadable — `consult-preview-delay`.
        // Which-key's idle delay before the leader menu appears.
        let leader_pending =
            session.state() == meowr_command::State::Keypad && !session.menu_visible();
        let leader = leader_pending.then(|| tokio::time::sleep(session.leader_delay()));

        let previewing = session.preview_target().is_some();
        let preview = previewing.then(|| tokio::time::sleep(session.options().preview_delay));

        // `lsp-ui-doc-show-with-cursor`: documentation when the point comes to
        // rest, with no key pressed. Debounced exactly as the preview is --
        // the sleep is built afresh every pass, so what this measures is "no
        // event for `hover-delay`" rather than a stopwatch on the selection.
        // Which States it is live in, and what stands in its way, are the
        // session's to say: `resting_hover` is `None` whenever it would be
        // wrong, and this layer only asks how long to wait.
        let resting = session.resting_hover().is_some();
        let rest = resting.then(|| tokio::time::sleep(session.options().hover_delay));

        // `consult-async-input-debounce`: a picker whose source is a *question*
        // puts it again once the input has sat still. Debounced exactly as the
        // hover and the preview are, and for a sharper reason -- without it the
        // server is asked once per keystroke.
        let asking = session.asking_pending();
        let ask = asking.then(|| tokio::time::sleep(session.options().ask_debounce));

        // `auto-save-timeout`: an idle buffer is written out even if it has
        // not reached the edit count, because the moment you stop typing is
        // exactly the moment you walk away from the machine.
        let autosaving = session.autosave_pending();
        let idle = autosaving.then(|| tokio::time::sleep(session.options().auto_save_timeout));

        // Parsing runs on a blocking worker with a rope snapshot, never on
        // the event loop. The buffer stays editable while it runs, and a
        // result whose revision no longer matches is dropped — the buffer
        // moved on, and the next edit schedules another parse.
        // Which buffers a language server has anything to say about, and which
        // server each wants. Asked of the session as one question (#119): the id
        // has to come from the same list the server lookup uses, or a killed
        // slot before a served buffer shifts every later id.
        //
        // Cheap enough to ask every pass -- a `u64` per buffer -- and the text
        // is cloned only for a buffer that has actually moved on, so an idle
        // editor is not copying a rope per pass.
        let served = session.served();
        // A path no buffer is served for any more is closed with its server,
        // and forgotten. Remembered, a buffer reopened for it -- by a key, or
        // unshown by a workspace edit (#167) -- that reached the same revision
        // number would never be sent at all, and the server would go on
        // believing the text from before.
        told.retain(|path, (program, _)| {
            let serving = served.iter().any(|s| s.path == path.as_str());
            if !serving && let Some(connection) = servers.get(program.as_str()) {
                let _ = connection.notify("textDocument/didClose", lsp::did_close(path));
            }
            serving
        });
        let wanted: Vec<Wanted> = served
            .into_iter()
            .map(|served| Wanted {
                program: served.server.resolved(home.as_deref()),
                args: served.server.args.clone(),
                path: served.path.to_string(),
                language: served.language.to_string(),
                revision: served.revision,
                text: (told.get(served.path).map(|(_, told)| *told) != Some(served.revision))
                    .then(|| {
                        session
                            .buffer(served.id)
                            .document()
                            .map(|d| d.text().to_string())
                    })
                    .flatten(),
            })
            .collect();

        // One connection per **server program**, started when a buffer wants it
        // and stopped when none does.
        //
        // Keyed by program rather than one connection for the whole session,
        // because a `.rs` and a `.ts` buffer open together want two different
        // processes -- and a single connection would send the second one's
        // `didOpen` to the first one's server, with a correct `languageId` and
        // the wrong program behind it. #119's own criterion is that a second
        // Language reaches a real server, not a plausible one.
        //
        // Started on *opening* a buffer rather than once at startup, which is
        // what #117 asks for: `SPC f f` opens a Rust file as much as an argument
        // does. The far end of the same rule is #117's other criterion --
        // killing the buffer must not leave the child behind -- and dropping the
        // last handle is what takes it.
        let live: std::collections::HashSet<String> =
            wanted.iter().map(|w| w.program.clone()).collect();
        servers.retain(|program, _| {
            // Dropping the handle ends both of a connection's tasks and
            // `kill_on_drop` takes the child process with them.
            let keep = live.contains(program);
            if !keep {
                told.retain(|_, (told_program, _)| told_program != program);
            }
            keep
        });
        for want in &wanted {
            if servers.contains_key(&want.program) || refused.contains(&want.program) {
                continue;
            }
            let root = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
            let args: Vec<&str> = want.args.iter().map(String::as_str).collect();
            match lsp::Connection::spawn(&want.program, &args, &root).await {
                Ok((connection, mut notifications)) => {
                    servers.insert(want.program.clone(), std::sync::Arc::new(connection));
                    let tx = diagnostics_tx.clone();
                    // One task draining this connection's notifications, which
                    // is #110's shape: they answer no request, so there is
                    // nothing to correlate them against and they are simply
                    // delivered as they arrive.
                    tokio::spawn(async move {
                        while let Some(notification) = notifications.recv().await {
                            if notification.method != "textDocument/publishDiagnostics" {
                                continue;
                            }
                            let Some(uri) = lsp::uri_in(&notification.params) else {
                                continue;
                            };
                            // The buffer is resolved on the far side, where the
                            // session is; this task knows only the URI it was
                            // told about.
                            let _ = tx
                                .send(Published {
                                    uri: uri.to_string(),
                                    version: lsp::version_of(&notification.params),
                                    params: notification.params.clone(),
                                })
                                .await;
                        }
                    });
                }
                // Remembered as refused rather than retried: a machine with no
                // `rust-analyzer` on it would otherwise repaint the message and
                // re-spawn as fast as the loop turns. Per program, so one
                // missing server does not stop another from starting.
                Err(e) => {
                    session.say(&format!("{e}"));
                    refused.insert(want.program.clone());
                }
            }
        }

        // Each buffer whose server has not been told the current text. The same
        // shape of question the reparse poll below asks, and the answer is no
        // for every buffer nobody is typing into.
        for want in wanted {
            let Some(text) = want.text else { continue };
            let Some(connection) = servers.get(&want.program) else {
                continue;
            };
            let (method, message) = match told.contains_key(&want.path) {
                true => (
                    "textDocument/didChange",
                    lsp::did_change(&want.path, want.revision, &text),
                ),
                false => (
                    "textDocument/didOpen",
                    lsp::did_open(&want.path, &want.language, want.revision, &text),
                ),
            };
            let _ = connection.notify(method, message);
            told.insert(want.path, (want.program, want.revision));
        }

        if let Some((buffer, job)) = session.take_reparse() {
            let revision = job.revision;
            let languages = session.languages();
            let tx = parsed_tx.clone();
            tokio::task::spawn_blocking(move || {
                let _ = tx.blocking_send((buffer, revision, job.run(&languages)));
            });
        }

        // Drawn here, last, rather than at the top of the loop: everything
        // above can change what the user should see -- a server that would
        // not start says so (#125) -- and a frame drawn before that work is
        // a frame without it in. `dirty` only takes effect on the *next*
        // pass, and the next pass is on the far side of a `select!` that
        // blocks until something happens. With nothing happening there is
        // no next pass, and `refused` above means no second chance to say
        // it either.
        //
        // So the loop reads: work, draw, wait. Which is the order it always
        // meant, and the reason the bug looked like it worked -- any
        // unrelated event at all would carry the message to a frame.
        // #127. The frame is drawn when it *differs from the frame on screen*,
        // not when somebody remembered to say it changed. `dirty` was a bool
        // set by hand at every site that altered what the user should see, and
        // #125 was the site that did not: reordering the loop fixed that
        // instance and left the class, because a flag records a belief and a
        // belief can be wrong in the one direction that fails silently.
        //
        // A screen cannot be wrong about whether it is the screen. Building it
        // and comparing costs what the draw already did — every read below was
        // already here, and the closure that paints touches the session not
        // once, which is what makes `paint` a function of the model alone.
        // Nothing can be drawn that the comparison did not see.
        let screen = {
            // Every window is drawn into its own rectangle. They tile, so
            // there is no order to get wrong and nothing to draw on top of
            // anything else — ADR-0004 in the shape of a loop.
            // The widget takes the bottom of the frame and the windows above
            // it are laid out shorter. Nothing floats — ADR-0004 — so this is
            // a subtraction rather than an overlay.
            let narrow = session.narrow_view();
            let menu: Vec<(String, String, bool)> = if session.menu_visible() {
                session
                    .menu()
                    .into_iter()
                    .map(|e| {
                        (
                            e.key.to_string(),
                            e.description,
                            e.kind == meowr_app::LeaderKind::Reserved,
                        )
                    })
                    .collect()
            } else {
                Vec::new()
            };
            let prefix = Keys(session.keypad()).to_string();
            // The Echo Area is one row, and only when nothing else is using
            // the bottom: a widget that is open owns it, and the message is
            // gone by the next key anyway.
            let echo = session.echo().map(str::to_owned);
            // A hover is the Echo Area grown to fit, so it reserves rows the
            // same way and stands aside for the same two widgets — the session
            // decides which of the three owns the bottom, because that is a
            // fact about the session rather than about this layer. It says so
            // itself now; this layer asked four accessors and re-derived it.
            let hover = session.hover_view();
            let bottom = session.bottom();
            // Read twice -- once to reserve the rows and once to draw into them
            // -- so it is asked of one function both times.
            let reserved =
                meowr_render::reserved(bottom, narrow.as_ref(), menu.len(), hover.as_ref());
            // The one thing here that is not a read. It has to happen before
            // the Windows are laid out, and it is idempotent: the rows reserved
            // have to change before it does anything.
            if reserved != last_reserved {
                let size = terminal.size()?;
                session.on_resize(size.width, size.height.saturating_sub(reserved));
                last_reserved = reserved;
            }
            let areas = session.layout();
            let focus = session.focus();
            let windows: Vec<WindowView> = areas
                .iter()
                .map(|(id, area)| {
                    let buffer = session.buffer(session.windows().window(*id).buffer);
                    let message = buffer
                        .error()
                        .map(str::to_owned)
                        .or_else(|| buffer.is_loading().then(|| "Loading…".to_owned()));
                    WindowView {
                        area: *area,
                        rows: session.rows_of(*id),
                        // Order is the stacking order: syntax colour first
                        // because it says what the text *is*, then the Grab,
                        // then diagnostics last so a squiggle is legible over
                        // whatever it lands on.
                        styled: {
                            let mut runs = session.styled_of(*id);
                            runs.extend(session.grabbed_of(*id));
                            runs.extend(session.diagnostics_of(*id));
                            runs
                        },
                        message,
                        hints: if *id == focus {
                            session.hints()
                        } else {
                            Vec::new()
                        },
                        modeline: session.modeline_of(*id),
                        focused: *id == focus,
                        gutter: session.gutter_of(*id),
                    }
                })
                .collect();
            Screen {
                narrow,
                menu,
                prefix,
                echo,
                hover,
                bottom,
                windows,
            }
        };
        if shown.as_ref() != Some(&screen) {
            terminal.draw(|frame| paint(frame, &screen))?;
            shown = Some(screen);
        }

        tokio::select! {
            () = async { leader.expect("guarded by leader_pending").await }, if leader_pending => {
                session.show_menu();
            }
            () = async { preview.expect("guarded by previewing").await }, if previewing => {
                // A file candidate has to be read before it can be shown, and
                // reading is the binary's: the session says what previewing
                // means and this carries it out, the same division `Action::
                // Save` and `take_reparse` already draw.
                // Which file, if any, is the session's to say: a Place in an
                // open buffer needs nothing read, and one in a closed file needs
                // the file whether it ends as a head or a line (#153).
                if let Some(path) = session.preview_read() {
                    let text = read_for_preview(&path).await;
                    session.previewed(&path, text);
                } else {
                    session.preview();
                }
            }
            () = async { rest.expect("guarded by resting").await }, if resting => {
                // Asking is I/O, so it happens here -- and it is not awaited,
                // which matters more here than it does for `SPC k k`: nobody
                // pressed anything, so a server thinking for a second would be
                // an editor that stopped taking keys for no visible reason.
                // Taken before the connection is looked for, and deliberately:
                // with no server running -- one that refused to start, one not
                // installed -- the question is marked asked and nobody is
                // asked it. The alternative is a timer that re-arms every
                // `hover-delay` for ever on a machine with no `rust-analyzer`
                // on it. The next key clears the latch, so nothing is lost
                // once a server does come up.
                let asked = session.ask_at_rest().and_then(|asked| {
                    let connection =
                        connection_for(&session, &servers, home.as_deref(), &asked.path)?;
                    Some((asked, connection))
                });
                if let Some((asked, connection)) = asked
                    && let Some(doc) = session.document()
                {
                    let request = lsp::hover_at(&asked.path, doc, asked.at);
                    let tx = hovers_tx.clone();
                    tokio::spawn(async move {
                        let reply = connection
                            .request("textDocument/hover", request)
                            .await
                            .ok();
                        let _ = tx
                            .send(Answered::AtRest(
                                asked,
                                reply.as_ref().and_then(lsp::hover_text),
                            ))
                            .await;
                    });
                }
            }
            () = async { ask.expect("guarded by asking").await }, if asking => {
                // Every connection for `all-workspaces` and this buffer's alone
                // otherwise: meowr runs one server per Language, so a workspace
                // is a server. The query goes out stamped with itself, and the
                // session drops an answer whose question it has moved past --
                // ADR-0009 with the stamp on the *input*, which is the only
                // thing a `workspace/symbol` answer is about.
                if let Some(asked) = session.take_ask()
                    && let meowr_app::LookupKind::Symbols { query, everywhere } = asked.kind
                {
                    let asking: Vec<std::sync::Arc<lsp::Connection>> = if everywhere {
                        servers.values().cloned().collect()
                    } else {
                        connection_for(&session, &servers, home.as_deref(), &asked.path)
                            .into_iter()
                            .collect()
                    };
                    for connection in asking {
                        let request = lsp::workspace_symbol(&query);
                        let tx = symbols_tx.clone();
                        let about = query.clone();
                        tokio::spawn(async move {
                            let reply = connection
                                .request("workspace/symbol", request)
                                .await
                                .ok();
                            let places = reply
                                .as_ref()
                                .map(lsp::symbols_of)
                                .unwrap_or_default();
                            let _ = tx.send((about, places)).await;
                        });
                    }
                }
            }
            () = async { idle.expect("guarded by autosaving").await }, if autosaving => {
                write_autosaves(&mut session, true).await;
            }
            // Ends the process without another frame, so the auto-save has to
            // happen here rather than after the loop.
            () = going_down(&mut terminate, &mut hangup) => {
                write_autosaves(&mut session, true).await;
                keep_recent(recent.as_ref(), &session).await;
                break;
            }
            () = async { expiry.expect("guarded by hints_pending").await },
                if session.hints_pending() =>
            {
                session.expire_hints();
            }
            event = events.next() => match event {
                Some(Ok(Event::Key(key))) => {
                    if let Some(key) = to_key(key) {
                        match session.on_key(key) {
                            // A refused quit never reaches here; this is
                            // `SPC q q` with nothing unsaved, or `SPC q Q`,
                            // which is a decision to lose the work and so
                            // takes the auto-save with it.
                            Action::Quit => {
                                stopping(&session, recent.as_ref()).await;
                                break;
                            }
                            // A quit that comes back, and so exactly a quit
                            // here -- the same call, not the same lines copied:
                            // #182 says the auto-saves and the recent files are
                            // dealt with as a quit deals with them, and two
                            // copies of that is how one of them stops being
                            // true. The `exec` waits until `main` has put the
                            // terminal back.
                            Action::Restart => {
                                stopping(&session, recent.as_ref()).await;
                                ending = Ending::Restart;
                                break;
                            }
                            // Nothing to do: since #127 the host does not act
                            // on `Redraw`, because whether the frame changed is
                            // a question the screen answers for itself. The
                            // variant stays because it is still what a command
                            // *means* to the session.
                            Action::Redraw => {}
                            Action::Save => {
                                // Writing is the binary's job: nothing below
                                // this layer touches the filesystem. Done
                                // inline rather than spawned so that "saved"
                                // is true when the indicator says so.
                                //
                                // *Which* file is the session's, not this
                                // layer's: the buffer on screen names its own,
                                // and answering from the path on the command
                                // line wrote the buffer you were looking at
                                // over the file you started in.
                                //
                                // Since #80 the session refuses a save it has
                                // nothing to write — it knows the four reasons
                                // and this layer knows none of them — so
                                // `Action::Save` now means there *is* a
                                // target. The binding stays rather than
                                // becoming an `expect`: the invariant is one
                                // arm of a match away and worth an editor
                                // panicking over precisely never.
                                if let Some(write) = session.save_target() {
                                    // `Taken` does not `continue`: that would skip
                                    // the tail of this iteration, and the tail
                                    // keeps the panic snapshot and the auto-saves
                                    // current -- at the one moment the buffer has
                                    // just gained a name, so both would have
                                    // stayed under the old one.
                                    match write_out(&write).await {
                                        Ok(made_directories) => {
                                            // A name typed at save-as is the
                                            // one path the session sets for
                                            // itself, so it is given the one
                                            // spelling here, where the write
                                            // lands: `./new.rs` and `new.rs`
                                            // would be two buffers the next
                                            // time anything named it (#150).
                                            if write.fresh {
                                                session.respell_path(&write.path, &respell(&write.path));
                                            }
                                            // Before `mark_saved`, which runs
                                            // the `after-save` hooks: a hook
                                            // with something of its own to say
                                            // has more right to the line than
                                            // this has. In the spelling the
                                            // buffer now holds (#150).
                                            if made_directories {
                                                let said = if write.fresh {
                                                    respell(&write.path)
                                                } else {
                                                    write.path.clone()
                                                };
                                                session.saved_making_directories(&said);
                                            }
                                            session.mark_saved();
                                            // `format-on-save`: the save is true
                                            // now, and the format follows it
                                            // (ADR-0014).
                                            while let Some(asked) = session.pending_format() {
                                                run_format(asked);
                                            }
                                        }
                                        Err(Unwritten::Taken) => session.refuse_overwrite(),
                                        Err(Unwritten::Failed(e)) => {
                                            session.failed(format!("{e}: {}", write.path))
                                        }
                                    }
                                }
                            }
                            // A chosen file has to be read before it can be
                            // shown, and reading is the binary's.
                            Action::Open => read_pending_open(&mut session, &opened_tx),
                            // Copying and moving are the filesystem's, so they
                            // are this layer's. Awaited rather than spawned,
                            // as a save is: the sentence the session then says
                            // is about something that has already happened.
                            Action::FileWorked => {
                                if let Some(asked) = session.pending_file_work() {
                                    let done = move_or_copy(&asked, respell).await;
                                    // A copy is opened, which is a read, which
                                    // is this layer's too.
                                    if session.file_done(&asked, done) == Action::Open {
                                        read_pending_open(&mut session, &opened_tx);
                                    }
                                }
                            }
                            // Awaited for the reason a move is: the sentence
                            // the session then says is about a file that has
                            // already gone. The auto-save beside it goes with
                            // it, on `remove_forgotten_autosaves` below, which
                            // runs after every key for exactly this.
                            Action::FileDeleted => {
                                if let Some(asked) = session.pending_file_deletion() {
                                    let gone = delete_file(&asked).await;
                                    session.file_deleted(&asked, gone);
                                }
                            }
                            // Asking a server is I/O, so it happens here. Not
                            // awaited: a server thinking for a second must not
                            // stop the editor taking keys, which is the rule
                            // every other background task here already keeps.
                            Action::Hover => {
                                // The connection for *this buffer's* server, not
                                // a single one for the session: with two
                                // Languages open, asking whichever connection
                                // came first would send the question to the
                                // wrong program.
                                let asked = session.pending_hover().and_then(|(path, at)| {
                                    let connection = connection_for(
                                        &session,
                                        &servers,
                                        home.as_deref(),
                                        &path,
                                    )?;
                                    Some((path, at, connection))
                                });
                                if let Some((path, at, connection)) = asked {
                                    let Some(doc) = session.document() else {
                                        continue;
                                    };
                                    let request = lsp::hover_at(&path, doc, at);
                                    let tx = hovers_tx.clone();
                                    tokio::spawn(async move {
                                        let reply = connection
                                            .request("textDocument/hover", request)
                                            .await
                                            .ok();
                                        let _ = tx
                                            .send(Answered::ByKey(
                                                path,
                                                reply.as_ref().and_then(lsp::hover_text),
                                            ))
                                            .await;
                                    });
                                }
                            }
                            Action::Lookup => {
                                let Some(asked) = session.pending_lookup() else {
                                    continue;
                                };
                                let Some(connection) = connection_for(
                                    &session,
                                    &servers,
                                    home.as_deref(),
                                    &asked.path,
                                ) else {
                                    // Named rather than silent: a row that does
                                    // nothing and says nothing is the hole
                                    // #69's clause is about.
                                    session.say("No language server for this file.");
                                    continue;
                                };
                                let Some(doc) = session.document() else {
                                    continue;
                                };
                                let Some((method, request)) =
                                    lsp::lookup_request(&asked.kind, &asked.path, doc, asked.at)
                                else {
                                    continue;
                                };
                                let tx = found_tx.clone();
                                let about = asked.path.clone();
                                tokio::spawn(async move {
                                    let reply = connection.request(method, request).await.ok();
                                    let places = reply
                                        .as_ref()
                                        .map(lsp::places_of)
                                        .unwrap_or_default();
                                    let _ = tx.send((about, places)).await;
                                });
                            }
                            Action::Rename => {
                                let Some(asked) = session.pending_rename() else {
                                    continue;
                                };
                                let site = asked.site().clone();
                                let Some(connection) =
                                    connection_for(&session, &servers, home.as_deref(), &site.path)
                                else {
                                    session.say("No language server for this file.");
                                    continue;
                                };
                                // What the server said it does when it started
                                // answers the first question without asking it:
                                // a server that cannot rename is refused before
                                // a name is typed, and one that cannot prepare
                                // is given the symbol at the point (lsp-mode).
                                if let meowr_app::RenameQuestion::Prepare(_) = asked {
                                    let answered = match lsp::rename_support_of(connection.capabilities()) {
                                        lsp::RenameSupport::None => Some(meowr_app::Prepared::Unsupported),
                                        lsp::RenameSupport::Rename => Some(meowr_app::Prepared::Default),
                                        lsp::RenameSupport::Prepare => None,
                                    };
                                    if let Some(answer) = answered {
                                        session.rename_prepared(&site, answer);
                                        continue;
                                    }
                                }
                                // The buffer the rename is about, which the
                                // session checked is the one on screen -- asked
                                // by id rather than as the current one, so a
                                // hook that moved the focus cannot put the
                                // position in another file's text.
                                let Some(doc) = session.buffer(site.buffer).document() else {
                                    continue;
                                };
                                let (method, request) = lsp::rename_request(&asked, doc);
                                let tx = renames_tx.clone();
                                tokio::spawn(async move {
                                    let reply = connection.request(method, request).await;
                                    let _ = tx.send((asked, reply)).await;
                                });
                            }
                            Action::CodeActions => {
                                put_code_action_question(
                                    &mut session,
                                    &servers,
                                    home.as_deref(),
                                    respell,
                                    &actions_tx,
                                )
                                .await;
                            }
                            Action::Project => {
                                // `project-scope` read here rather than kept,
                                // because an `init.scm` reloaded mid-session is
                                // allowed to change it and a copy taken at
                                // startup would be the stale second source this
                                // codebase keeps refusing to create.
                                let scope = session.options().project_scope;
                                if let Some(about) = session.pending_project() {
                                    let tx = projects_tx.clone();
                                    tokio::spawn(async move {
                                        let answer =
                                            project_of(about.as_deref(), scope).await;
                                        let _ = tx.send((about, answer)).await;
                                    });
                                }
                            }
                            Action::Search => {
                                let Some(asked) = session.pending_search() else {
                                    continue;
                                };
                                let scope = session.options().project_scope;
                                let tx = searches_tx.clone();
                                tokio::spawn(async move {
                                    let answer = match directory_of(&asked.subject, scope).await
                                    {
                                        Ok(directory) => {
                                            search::search(&asked.pattern, &directory).await
                                        }
                                        Err(why) => Err(why),
                                    };
                                    let _ = tx.send(answer).await;
                                });
                            }
                            Action::Format => {
                                while let Some(asked) = session.pending_format() {
                                    run_format(asked);
                                }
                            }
                            // `save-some-buffers`: every write, then one answer
                            // so the session can say it in one sentence (#166).
                            Action::SaveSome => {
                                let mut results = Vec::new();
                                for write in session.pending_saves() {
                                    // What it had to make on the way is dropped:
                                    // this row's sentence counts buffers, and a
                                    // directory named inside a count of three
                                    // would be a sentence about the wrong thing
                                    // (#181).
                                    let written = write_out(&write)
                                        .await
                                        .map(|_| ())
                                        .map_err(|e| e.to_string());
                                    // Given the one spelling as `SPC f s` gives it,
                                    // and named by it, since that is the path the
                                    // buffer then has.
                                    let path = if write.fresh && written.is_ok() {
                                        let spelled = respell(&write.path);
                                        session.respell_path(&write.path, &spelled);
                                        spelled
                                    } else {
                                        write.path
                                    };
                                    results.push((path, written));
                                }
                                session.saved_some(results);
                                while let Some(asked) = session.pending_format() {
                                    run_format(asked);
                                }
                            }
                            Action::FromRoot => {
                                let Some(asked) = session.pending_from_root() else {
                                    continue;
                                };
                                // Named rather than dropped: a row that needs a
                                // root and cannot place one says why (ADR-0010).
                                let Some(cwd) = cwd.clone() else {
                                    session.say("The working directory could not be read.");
                                    continue;
                                };
                                let scope = session.options().project_scope;
                                let home = home.clone().map(std::path::PathBuf::from);
                                let tx = from_root_tx.clone();
                                tokio::spawn(async move {
                                    let answer = answer_from_root(asked, scope, cwd, home).await;
                                    let _ = tx.send(answer).await;
                                });
                            }
                            // `project_of` with the scope forced off, which is
                            // the listing `+default/find-file-under-here`
                            // means: git's own listing of the directory where
                            // there is a repository, a walk where there is not,
                            // and no refusal for want of a root either way.
                            Action::Files => {
                                if let Some(about) = session.pending_files() {
                                    let tx = files_tx.clone();
                                    tokio::spawn(async move {
                                        let answer = files_under(about.path()).await;
                                        let _ = tx.send((about, answer)).await;
                                    });
                                }
                            }
                            Action::Nothing => {}
                        }
                        // `auto-save-interval`, counted in edits. Not forced,
                        // so this writes only when the count says to.
                        write_autosaves(&mut session, false).await;
                        // A killed buffer's auto-save goes with the buffer.
                        remove_forgotten_autosaves(&mut session).await;
                        // What a panic would write. Kept current after every
                        // key, because the edits since the last auto-save are
                        // exactly the ones a crash would otherwise take.
                        *unsaved.lock().unwrap_or_else(|e| e.into_inner()) =
                            session.autosave_snapshot();
                        // `recover-file` asked for the auto-save to be read.
                        if let Some(auto) = session.take_recovery() {
                            let text = tokio::fs::read_to_string(&auto)
                                .await
                                .map_err(|e| format!("{e}: {auto}"));
                            session.recovered(text);
                        }
                    }
                }
                Some(Ok(Event::Resize(width, height))) => {
                    session.on_resize(width, height);
                }
                Some(Ok(_)) => {}
                // Neither of these is a quit, and neither consults whether a
                // buffer is modified: the terminal has gone or the event
                // stream has broken, so there is nothing to refuse *to*. What
                // there is to do is write the work out, which is why both go
                // through `write_autosaves` on the way.
                Some(Err(e)) => {
                    write_autosaves(&mut session, true).await;
                    keep_recent(recent.as_ref(), &session).await;
                    return Err(e.into());
                }
                None => {
                    write_autosaves(&mut session, true).await;
                    keep_recent(recent.as_ref(), &session).await;
                    break;
                }
            },
            Some((buffer, revision, syntax)) = parsed_rx.recv() => {
                session.apply_reparse(buffer, revision, syntax);
            }
            Some(loaded) = rx.recv() => {
                match loaded {
                    Ok(text) => session.loaded(Document::from_text(&text)),
                    Err(message) => session.failed(message),
                }
            }
            Some((path, read)) = opened_rx.recv() => {
                session.opened(&path, read);
            }
            Some(Published { uri, version, params }) = diagnostics_rx.recv() => {
                // The URI names a path and the session names buffers; matching
                // them is this layer's because only it holds both.
                let placed = lsp::path_of(&uri).and_then(|path| {
                    // Compared in absolute terms: the buffer holds the path
                    // that was typed and the notification names an absolute
                    // one, and `meowr crates/…/lsp.rs` makes those differ.
                    let id = session
                        .buffers()
                        .iter()
                        .position(|b| {
                            b.is_real()
                                && b.path().is_some_and(|p| lsp::absolute(p) == lsp::absolute(path))
                        })
                        .map(BufferId)?;
                    let doc = session.buffer(id).document()?;
                    // The version the server answered with is the revision the
                    // answer was computed against, so a buffer that has since
                    // moved on refuses it — `apply_reparse`'s own contract. A
                    // server that omits the version is taken at the buffer's
                    // current one, because dropping everything such a server
                    // said would be the worse failure.
                    let revision = version.unwrap_or(session.buffer(id).revision());
                    Some((id, revision, lsp::diagnostics_of(&params, doc)))
                });
                if let Some((id, revision, diagnostics)) = placed {
                    session.set_diagnostics(id, revision, diagnostics);
                }
            }
            Some((about, answer)) = projects_rx.recv() => {
                // Stamped with the buffer it is about, and the session decides
                // on arrival whether that is still the buffer the picker is
                // about -- ADR-0009, and the same division the hover arm draws.
                session.set_project(about, answer.map(respelled_rows));
            }
            Some((query, places)) = symbols_rx.recv() => {
                // Re-checked by the session, not here: whether this answer is
                // still the question being asked is a fact about the widget's
                // input, and #124 is what this layer deciding it cost once.
                session.found_symbols(&query, respelled_places(places));
            }
            Some((asked, outcome)) = formats_rx.recv() => {
                // A save-time format that landed is written again: quietly,
                // with no hooks and no format of its own (ADR-0014).
                if session.formatted(&asked, outcome) == Action::Save
                    && let Some(write) = session.quiet_save_target()
                {
                    match write_out(&write).await {
                        Ok(_) => session.mark_saved_quietly(&write.path),
                        // Said, not `failed`: that replaces the current
                        // buffer's text with the error, and the buffer written
                        // here need not be the current one.
                        Err(e) => session.say(&format!(
                            "The formatted {} could not be written: {e}",
                            write.path
                        )),
                    }
                }
            }
            Some(answered) = from_root_rx.recv() => match answered {
                FromRootAnswer::Yank { about, path } => {
                    session.path_from_root(&about, path);
                }
                FromRootAnswer::Recent { about, answer } => {
                    session.recent_from_root(&about, answer);
                }
                FromRootAnswer::Scratch { about, row, answer } => {
                    session.scratch_from_root(&about, row, answer);
                }
            },
            Some((about, answer)) = files_rx.recv() => {
                session.files_listed(&about, answer.map(respelled_rows));
            }
            Some(answer) = searches_rx.recv() => {
                // Handed over whole, refusal and all: ADR-0010 puts the
                // sentence on this side because this is what knows where it
                // looked, and the session decides whether there is anywhere to
                // say it -- which is the division the `found` arm below draws
                // in the same words.
                session.searched(answer.map(respelled_places));
            }
            Some((asked, reply, support)) = actions_rx.recv() => match &asked {
                meowr_app::CodeActionQuestion::List(listing) => {
                    // One action is applied without a picker, and that is a
                    // question of its own to put -- acted on here, or a lone
                    // "Organize Imports" was chosen and never applied.
                    let found = lsp::code_actions_of(&reply, support);
                    if session.code_actions_found(listing, found) == Action::CodeActions {
                        put_code_action_question(
                            &mut session,
                            &servers,
                            home.as_deref(),
                            respell,
                            &actions_tx,
                        )
                        .await;
                    }
                }
                meowr_app::CodeActionQuestion::Apply(chosen) => {
                    let answer = with_edit_read(&session, lsp::resolved_of(&reply), respell).await;
                    session.code_action_applied(chosen, answer);
                }
            },
            Some((asked, reply)) = renames_rx.recv() => match &asked {
                meowr_app::RenameQuestion::Prepare(site) => {
                    session.rename_prepared(site, lsp::prepared_of(&reply));
                }
                // The edit's files are read here, on the loop, as a save's are
                // written: inline, so no key lands between reading a file and
                // the session deciding whether a buffer visits it.
                meowr_app::RenameQuestion::Apply(apply) => {
                    let answer = with_edit_read(&session, lsp::renamed_of(&reply), respell).await;
                    session.renamed(apply, answer);
                }
            },
            Some((about, places)) = found_rx.recv() => {
                // Re-checked by the session, not here: whether the answer is
                // still about anything is a fact about the buffer, and #124 is
                // what this layer deciding it cost last time.
                //
                // What it returns is acted on, because one Place is a jump and a
                // jump into another file is an `Open` (#146). Discarded, `SPC k
                // d` pushed the Trail and went nowhere, and said nothing -- the
                // arm was written when every answer here opened a picker.
                if session.found(&about, respelled_places(places)) == Action::Open {
                    read_pending_open(&mut session, &opened_tx);
                }
            }
            Some(answered) = hovers_rx.recv() => {
                // Which of the three an answer turns into -- a hover, a spoken
                // "nothing here", or nothing at all -- is the session's, and
                // the two arms differ only in what was asked. This layer knew
                // the answer to that question until #124 and got it wrong,
                // because it cannot see what has happened to the buffer since.
                match answered {
                    Answered::ByKey(path, text) => session.show_hover(&path, text.as_deref()),
                    Answered::AtRest(asked, text) => {
                        session.show_hover_at(&asked, text.as_deref())
                    }
                };
            }
        }
    }
    Ok(ending)
}

/// What stopping deliberately means for what is on disk.
///
/// The auto-saves go, because the work they held is either written or was
/// thrown away on purpose, and one left beside a file would offer to recover
/// what nobody lost. The recent files are written, because this process is the
/// one holding them. Shared by the two rows that end a session so that "a
/// restart is a quit that comes back" is one call rather than a resemblance
/// (#182).
async fn stopping(session: &Session, recent: Option<&RecentFilesKept>) {
    remove_autosaves(session).await;
    keep_recent(recent, session).await;
}

/// How the editor stopped (#182).
///
/// A restart is not a third thing the loop does; it is a quit that `main` acts
/// on differently, once the terminal is back in the state it was handed in.
#[derive(Debug)]
enum Ending {
    Quit,
    Restart,
}

/// Become a new meowr, with the arguments this one was given (#182).
///
/// `exec` rather than spawn-and-exit: the shell that started meowr is waiting
/// on *this* process, and a child would leave it waiting on the wrong one --
/// and the pid, the terminal and the process group all stay as they were, which
/// is what makes a restart feel like one editor rather than two.
///
/// Called only after `ratatui::restore`, or the new process inherits the
/// alternate screen and raw mode without having asked for either. The language
/// servers go the way they go on a quit: the pipes to them are close-on-exec,
/// so each sees the end of its input and stops.
///
/// Returns only on failure. `exec` does not come back when it works -- and it
/// fails loudly rather than quietly: a binary replaced in place while meowr ran
/// leaves `current_exe` naming a path that is gone, and the sentence says so
/// where a silent quit would look like a restart that worked.
fn become_a_new_meowr() -> anyhow::Error {
    use std::os::unix::process::CommandExt;
    let exe = match std::env::current_exe() {
        Ok(exe) => exe,
        Err(e) => return anyhow::anyhow!("meowr could not find its own binary to restart: {e}"),
    };
    let failed = std::process::Command::new(exe)
        .args(std::env::args_os().skip(1))
        .exec();
    anyhow::anyhow!("meowr could not restart: {failed}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn press(code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, modifiers)
    }

    #[test]
    fn a_file_that_is_not_there_is_new_only_for_a_row_that_said_it_could_be() {
        // The distinction `config::evaluate` already draws for reading the
        // config itself: absent is the ordinary state of a fresh checkout, and
        // unreadable is a failure. `SPC f P` is the only row that says absence
        // is acceptable, so every other one keeps the failed buffer it had.
        let missing = std::io::Error::from(std::io::ErrorKind::NotFound);
        assert_eq!(
            what_was_read(Err(missing), true, "init.scm"),
            Opened::Missing
        );

        let missing = std::io::Error::from(std::io::ErrorKind::NotFound);
        assert!(matches!(
            what_was_read(Err(missing), false, "gone.rs"),
            Opened::Failed(why) if why.contains("gone.rs")
        ));
    }

    #[test]
    fn a_config_that_will_not_read_is_a_failure_even_though_it_could_have_been_new() {
        // Anything but "there is nothing there" is a real failure, and an empty
        // buffer offered for it is one you could write over the top of.
        let denied = std::io::Error::from(std::io::ErrorKind::PermissionDenied);
        assert!(matches!(
            what_was_read(Err(denied), true, "init.scm"),
            Opened::Failed(why) if why.contains("init.scm")
        ));
    }

    #[test]
    fn a_panic_writes_the_held_work_out() {
        // The editor does not crash on demand — resizing to nothing, and to
        // one column, and opening the leader menu in two rows, all failed to
        // make it — so the hook is exercised where it lives instead: hold a
        // snapshot, install the hook that writes it, and panic.
        //
        // The hook is not chained here, so an unrelated failing test cannot
        // have its message swallowed by ours for longer than this takes.
        let dir = std::env::temp_dir().join(format!("meowr-panic-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("a scratch directory");
        let path = dir.join("#notes.txt#");
        let unsaved: Unsaved = Unsaved::default();
        *unsaved.lock().expect("fresh") = vec![meowr_app::Snapshot {
            path: path.to_string_lossy().into_owned(),
            text: meowr_domain::Rope::from_str("work that was only in memory\n"),
        }];

        let held = unsaved.clone();
        let restore = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |_| flush_unsaved(&held)));
        let fell_over = std::panic::catch_unwind(|| panic!("the editor fell over"));
        std::panic::set_hook(restore);

        assert!(fell_over.is_err(), "it really did panic");
        assert_eq!(
            std::fs::read_to_string(&path).expect("the hook wrote it"),
            "work that was only in memory\n"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    // --- one spelling per file (#150) -----------------------------------------

    #[test]
    fn a_path_under_the_working_directory_is_spelled_relative_to_it() {
        // How a buffer launched as `meowr src/main.rs` is named, and so how a
        // server's `file:///w/proj/src/main.rs` has to arrive for the session
        // to see one file rather than two.
        let cwd = std::path::Path::new("/w/proj");
        assert_eq!(spelled("/w/proj/src/main.rs", cwd), "src/main.rs");
        assert_eq!(spelled("src/main.rs", cwd), "src/main.rs");
    }

    #[test]
    fn a_path_elsewhere_is_spelled_absolute() {
        // `../x.rs` would be a second spelling of `/w/x.rs` the moment anything
        // named it absolutely, and a server always does.
        let cwd = std::path::Path::new("/w/proj");
        assert_eq!(spelled("/other/x.rs", cwd), "/other/x.rs");
        assert_eq!(spelled("../x.rs", cwd), "/w/x.rs");
    }

    #[test]
    fn dots_are_spelled_out_of_a_path() {
        // Lexically, without asking the filesystem: a file being created has
        // nothing on disk to canonicalize against. Not `std::path::absolute`,
        // which keeps `..` on Unix -- and so, unlike this, can differ from the
        // kernel's answer only where a symlinked directory precedes a `..`.
        let cwd = std::path::Path::new("/w/proj");
        assert_eq!(spelled("./src/../src/./main.rs", cwd), "src/main.rs");
        assert_eq!(spelled("/w/proj/crates/../src/main.rs", cwd), "src/main.rs");
    }

    // --- copying and moving a file (#178) --------------------------------------

    fn asking(from: &str, to: &str, work: meowr_app::FileWork, force: bool) -> meowr_app::FileRequest {
        meowr_app::FileRequest {
            from: from.to_string(),
            to: to.to_string(),
            work,
            force,
        }
    }

    /// A scratch directory of this test's own, and what it holds.
    fn scratch(name: &str, files: &[(&str, &str)]) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("meowr-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("a scratch directory");
        for (path, text) in files {
            std::fs::write(dir.join(path), text).expect("written");
        }
        dir
    }

    #[tokio::test]
    async fn writing_a_buffer_makes_the_directory_it_needs() {
        // `SPC f P` opens `init.scm` in a config directory that may never have
        // been made, and #181 says writing that buffer creates the file. A
        // write is the only file operation that did not already do this: #178
        // gave copy and move the same answer, and a save-as into a directory
        // that is not there had been failing with the raw `io::Error` (#181).
        let dir = scratch("write", &[]);
        let path = dir.join("meowr/init.scm").to_string_lossy().into_owned();
        let write = meowr_app::Write {
            path: path.clone(),
            contents: "(set-option 'auto-save #t)\n".to_string(),
            fresh: true,
        };

        let made = write_out(&write).await.map_err(|e| e.to_string()).expect("written");

        assert!(made, "and says it had to, as a copy and a move say it (#172)");
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "(set-option 'auto-save #t)\n"
        );

        // The second write goes into a directory that is there, and says
        // nothing: the line exists for what happened, not for every save. Not
        // `fresh`, because the buffer is visiting that path now -- a fresh name
        // over a file that exists is the refusal `write_out` opens with.
        let again = meowr_app::Write { fresh: false, ..write };
        let again = write_out(&again).await.map_err(|e| e.to_string()).expect("written");
        assert!(!again, "nothing was made this time");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn moving_a_file_makes_the_directories_it_needs_and_says_so() {
        let dir = scratch("move", &[("main.rs", "fn main() {}\n")]);
        let at = |name: &str| dir.join(name).to_string_lossy().into_owned();
        let asked = asking(&at("main.rs"), &at("src/main.rs"), meowr_app::FileWork::Move, false);
        let done = move_or_copy(&asked, |path| spelled(path, &dir)).await.expect("moved");

        assert_eq!(done, meowr_app::FileDone { path: "src/main.rs".into(), made_directories: true });
        assert!(dir.join("src/main.rs").exists() && !dir.join("main.rs").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn a_target_that_is_there_is_refused_unless_it_was_forced() {
        let dir = scratch("taken", &[("main.rs", "one\n"), ("other.rs", "two\n")]);
        let at = |name: &str| dir.join(name).to_string_lossy().into_owned();
        let copying = |force| asking(&at("main.rs"), &at("other.rs"), meowr_app::FileWork::Copy, force);
        let respell = |path: &str| spelled(path, &dir);

        assert_eq!(
            move_or_copy(&copying(false), respell).await,
            Err(meowr_app::FileRefusal::Taken("other.rs".into())),
            "the path in the way, in the spelling the session knows"
        );
        assert_eq!(std::fs::read_to_string(dir.join("other.rs")).unwrap(), "two\n");

        move_or_copy(&copying(true), respell).await.expect("forced");
        assert_eq!(std::fs::read_to_string(dir.join("other.rs")).unwrap(), "one\n");
        assert!(dir.join("main.rs").exists(), "a copy leaves the original");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn a_directory_named_as_the_target_takes_the_files_own_name_and_says_what_is_there() {
        let dir = scratch("into", &[("main.rs", "fn main() {}\n")]);
        std::fs::create_dir_all(dir.join("src")).expect("a directory");
        let at = |name: &str| dir.join(name).to_string_lossy().into_owned();
        let asked = asking(&at("main.rs"), &at("src"), meowr_app::FileWork::Move, false);
        let done = move_or_copy(&asked, |path| spelled(path, &dir)).await.expect("moved");

        assert_eq!(done.path, "src/main.rs");
        assert!(!done.made_directories, "it was already there");

        // And what is in the way is that resolved name, not the directory.
        std::fs::write(dir.join("main.rs"), "again\n").expect("written");
        let asked = asking(&at("main.rs"), &at("src"), meowr_app::FileWork::Move, false);
        assert_eq!(
            move_or_copy(&asked, |path| spelled(path, &dir)).await,
            Err(meowr_app::FileRefusal::Taken("src/main.rs".into()))
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn a_file_that_cannot_be_moved_says_why_and_a_move_onto_itself_is_refused() {
        let dir = scratch("gone", &[]);
        let at = |name: &str| dir.join(name).to_string_lossy().into_owned();
        let respell = |path: &str| spelled(path, &dir);
        let missing = asking(&at("gone.rs"), &at("elsewhere.rs"), meowr_app::FileWork::Move, false);
        assert!(matches!(
            move_or_copy(&missing, respell).await,
            Err(meowr_app::FileRefusal::Failed(_))
        ));

        std::fs::write(dir.join("main.rs"), "one\n").expect("written");
        let itself = asking("main.rs", &at("main.rs"), meowr_app::FileWork::Move, true);
        assert_eq!(move_or_copy(&itself, respell).await, Err(meowr_app::FileRefusal::Itself));
        assert!(dir.join("main.rs").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    // --- deleting a file (#179) ------------------------------------------------

    #[tokio::test]
    async fn deleting_a_file_unlinks_it() {
        let dir = scratch("delete", &[("main.rs", "fn main() {}\n")]);
        let at = |name: &str| dir.join(name).to_string_lossy().into_owned();

        delete_file(&meowr_app::FileDeletion { path: at("main.rs") })
            .await
            .expect("deleted");

        assert!(!dir.join("main.rs").exists(), "gone, not moved to a trash");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn a_file_that_cannot_be_deleted_says_why() {
        let dir = scratch("delete-refused", &[]);
        let at = |name: &str| dir.join(name).to_string_lossy().into_owned();

        // A directory, which `remove_file` will not take: the nearest thing to
        // a refusal that does not need a permission to set up.
        std::fs::create_dir_all(dir.join("src")).expect("a directory");
        let why = delete_file(&meowr_app::FileDeletion { path: at("src") })
            .await
            .expect_err("not a file");

        assert!(!why.is_empty(), "the reason reaches the sentence the session says");
        assert!(dir.join("src").exists(), "and nothing went");
        let _ = std::fs::remove_dir_all(&dir);
    }

    // --- a workspace edit made ready (#167) ------------------------------------

    #[tokio::test]
    async fn a_workspace_edit_is_respelled_and_only_unvisited_files_are_read() {
        // Spelled against the real working directory, which is what the reads
        // are relative to; the scratch directory is outside it, so its paths
        // stay absolute and lose only their dots.
        let cwd = std::env::current_dir().expect("a working directory");
        let dir = std::env::temp_dir().join(format!("meowr-edit-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("sub")).expect("a scratch directory");
        std::fs::write(dir.join("lib.rs"), "fn old() {}\n").expect("written");
        let at = |path: &str| dir.join(path).to_string_lossy().into_owned();
        let edit = |path: &str| {
            meowr_app::FileChange::Edit(meowr_app::FileEdit {
                path: at(path),
                read: None,
                edits: Vec::new(),
            })
        };
        let changes = vec![edit("main.rs"), edit("sub/../lib.rs"), edit("gone.rs")];
        let main = at("main.rs");
        let ready = with_files_read(changes, |path| spelled(path, &cwd), |path| path == main).await;

        let reads: Vec<(String, Option<Result<String, String>>)> = ready
            .into_iter()
            .map(|change| match change {
                meowr_app::FileChange::Edit(file) => (file.path, file.read),
                other => panic!("not an edit: {other:?}"),
            })
            .collect();
        assert_eq!(reads[0], (at("main.rs"), None), "a visited file is not read");
        assert_eq!(reads[1], (at("lib.rs"), Some(Ok("fn old() {}\n".to_string()))), "respelled");
        assert_eq!(reads[2].0, at("gone.rs"));
        assert!(matches!(reads[2].1, Some(Err(_))), "a file that cannot be read says so");
        let _ = std::fs::remove_dir_all(&dir);
    }

    // --- running a formatter (#164, ADR-0014) ------------------------------------

    fn running(program: &str, args: &[&str]) -> meowr_command::Invocation {
        meowr_command::Invocation {
            program: program.to_string(),
            args: args.iter().map(|a| a.to_string()).collect(),
        }
    }

    #[tokio::test]
    async fn a_formatter_is_handed_the_text_on_stdin_and_read_from_stdout() {
        // `cat` formats nothing and so shows the plumbing alone: what went in
        // is what came back.
        let outcome = run_formatter(&running("cat", &[]), "fn main(){}\n", LIMIT).await;
        assert_eq!(outcome, meowr_app::Formatted::Output("fn main(){}\n".to_string()));
    }

    #[tokio::test]
    async fn a_formatter_that_fails_is_reported_with_the_first_line_it_complained_on() {
        let outcome = run_formatter(
            &running("sh", &["-c", "cat >/dev/null; echo 'error: expected )' >&2; echo more >&2; exit 1"]),
            "fn main({}\n",
            LIMIT,
        )
        .await;
        assert_eq!(outcome, meowr_app::Formatted::Failed("error: expected )".to_string()));
    }

    #[tokio::test]
    async fn a_formatter_that_is_not_installed_is_reported_as_missing() {
        let outcome = run_formatter(&running("meowr-no-such-formatter", &[]), "x", LIMIT).await;
        assert_eq!(outcome, meowr_app::Formatted::Missing);
    }

    const LIMIT: std::time::Duration = std::time::Duration::from_secs(10);

    #[tokio::test]
    async fn a_formatter_that_never_finishes_is_given_up_on_and_killed() {
        // Nothing else bounds it: the task is detached, so `kill_on_drop` only
        // fires once the future is dropped -- which the time limit does.
        let started = std::time::Instant::now();
        let outcome = run_formatter(
            &running("sleep", &["30"]),
            "",
            std::time::Duration::from_millis(200),
        )
        .await;
        assert!(started.elapsed() < std::time::Duration::from_secs(5), "given up on");
        assert_eq!(
            outcome,
            meowr_app::Formatted::Failed("did not finish within 0.2s".to_string())
        );
    }

    // --- the two rows that need the Project's root (#148) -----------------------

    #[test]
    fn a_yanked_path_is_from_the_root_or_whole_and_abbreviated() {
        // `+default/yank-buffer-path`: `(abbreviate-file-name (if root
        // (file-relative-name filename root) filename))`.
        let file = std::path::Path::new("/home/user/proj/crates/a/src/lib.rs");
        let home = Some(std::path::Path::new("/home/user"));
        assert_eq!(
            yanked_path(file, Some(std::path::Path::new("/home/user/proj")), home),
            "crates/a/src/lib.rs"
        );
        assert_eq!(yanked_path(file, None, home), "~/proj/crates/a/src/lib.rs");
        assert_eq!(
            yanked_path(std::path::Path::new("/etc/hosts"), None, home),
            "/etc/hosts",
            "abbreviated only under home"
        );
        assert_eq!(yanked_path(file, None, None), file.to_string_lossy());
    }

    #[test]
    fn the_recent_files_in_a_project_are_those_under_its_root_in_order() {
        // `projectile-recentf-files`: `string-prefix-p project-root` over the
        // list, which keeps its order -- and by the root as a *path*, so
        // `/w/proj-old` is not inside `/w/proj`.
        let kept = recent_under(
            &[
                "src/lib.rs".to_string(),
                "/w/proj-old/x.rs".to_string(),
                "/w/proj/README.md".to_string(),
                "/elsewhere/y.rs".to_string(),
            ],
            std::path::Path::new("/w/proj"),
            std::path::Path::new("/w/proj/crates/a"),
        );
        assert_eq!(kept, ["src/lib.rs", "/w/proj/README.md"]);
    }

    #[tokio::test]
    async fn with_project_scope_off_a_yank_is_whole_and_recent_files_are_those_here() {
        // No repository is needed to prove this, which is the point: with the
        // concept off, nothing asks git. The yank has no root to be relative
        // to; the recent files take the buffer's directory as ADR-0010's
        // stand-in.
        let at = std::env::temp_dir().join(format!("meowr-offroot-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&at);
        std::fs::create_dir_all(at.join("sub")).unwrap();
        let about = at.join("sub/a.rs").to_string_lossy().into_owned();

        let yank = meowr_app::FromRoot::Yank { about: about.clone() };
        match answer_from_root(yank, false, at.clone(), None).await {
            FromRootAnswer::Yank { path, .. } => assert_eq!(path, about),
            other => panic!("asked for a yank, got {other:?}"),
        }

        let beside = at.join("sub/b.rs").to_string_lossy().into_owned();
        let above = at.join("c.rs").to_string_lossy().into_owned();
        let recent = meowr_app::FromRoot::Recent {
            about,
            paths: vec![above, beside.clone()],
        };
        match answer_from_root(recent, false, at.clone(), None).await {
            FromRootAnswer::Recent { answer, .. } => assert_eq!(answer, Ok(vec![beside])),
            other => panic!("asked for recent files, got {other:?}"),
        }

        // A Project's scratch takes no stand-in: a name is a claim about a
        // Project, and with the concept off there is none to claim (#180).
        let scratch = meowr_app::FromRoot::Scratch {
            about: at.join("sub/a.rs").to_string_lossy().into_owned(),
            row: meowr_app::ScratchRow::Switch,
        };
        match answer_from_root(scratch, false, at.clone(), None).await {
            FromRootAnswer::Scratch { answer, .. } => {
                assert!(answer.is_err(), "no project, so no project scratch");
            }
            other => panic!("asked for a scratch, got {other:?}"),
        }
        std::fs::remove_dir_all(&at).unwrap();
    }

    // --- recent files, kept between sessions (#152) ---------------------------

    #[test]
    fn the_recent_files_live_in_the_xdg_state_directory() {
        // History is state in the XDG spec's terms -- "actions history" is its
        // own example -- where Doom uses its cache dir. An empty variable is an
        // unset one, which the spec says in as many words.
        assert_eq!(
            recent_files_at(Some("/s"), Some("/h")),
            Some(std::path::PathBuf::from("/s/meowr/recentf"))
        );
        assert_eq!(
            recent_files_at(None, Some("/h")),
            Some(std::path::PathBuf::from("/h/.local/state/meowr/recentf"))
        );
        assert_eq!(
            recent_files_at(Some(""), Some("/h")),
            Some(std::path::PathBuf::from("/h/.local/state/meowr/recentf"))
        );
        assert_eq!(recent_files_at(None, None), None);
    }

    #[test]
    fn recent_files_are_kept_absolute_and_read_back_in_the_session_s_spelling() {
        // A list read by a session launched somewhere else is meaningless
        // relative, and read back it has to be spelled the way #150 spells
        // every path, or a recent file opens a second buffer for an open one.
        let run = std::path::Path::new("/run/user/1000");
        let written = recent_files_text(
            ["src/a.rs", "/other/b.rs"].into_iter(),
            std::path::Path::new("/w/proj"),
            run,
        );
        assert_eq!(written, "/w/proj/src/a.rs\n/other/b.rs\n");

        let read = recent_files_read(&written, std::path::Path::new("/w"), run);
        assert_eq!(read, ["proj/src/a.rs", "/other/b.rs"]);
    }

    #[test]
    fn nothing_under_the_runtime_directory_is_kept_or_read_back() {
        // Doom's one `recentf-exclude`: `$XDG_RUNTIME_DIR`, or `/run`. A file
        // there is gone by the next boot and was never a file anybody meant.
        let cwd = std::path::Path::new("/w");
        let run = std::path::Path::new("/run/user/1000");
        let written = recent_files_text(["/run/user/1000/x", "a.rs"].into_iter(), cwd, run);
        assert_eq!(written, "/w/a.rs\n");
        let read = recent_files_read("/run/user/1000/y\n/w/a.rs\n", cwd, run);
        assert_eq!(read, ["a.rs"]);
    }

    #[tokio::test]
    async fn a_recent_file_that_no_longer_exists_is_dropped_on_the_way_out() {
        // Doom runs `recentf-cleanup` before `recentf-save-list` on quit, so
        // the list written is the list of files that are there.
        let at = std::env::temp_dir().join(format!("meowr-recent-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&at);
        std::fs::create_dir_all(&at).unwrap();
        let here = at.join("here.rs");
        std::fs::write(&here, "").unwrap();
        let gone = at.join("gone.rs");
        let list = at.join("state/meowr/recentf");

        keep_recent_files(
            &list,
            [here.to_str().unwrap(), gone.to_str().unwrap()].into_iter(),
            &at,
            std::path::Path::new("/run/user/1000"),
        )
        .await;

        assert_eq!(
            std::fs::read_to_string(&list).expect("written, directories and all"),
            format!("{}\n", here.display())
        );
        std::fs::remove_dir_all(&at).unwrap();
    }

    // --- the project picker's candidates (#120) ----------------------------

    #[test]
    fn a_filename_containing_a_newline_survives_the_listing() {
        // `-z` rather than a line per path, because git allows a newline in a
        // filename and `lines()` turned one file into two candidates, neither
        // of which opens.
        let listed = candidates(std::path::Path::new(""), "ordinary.rs\0od\nd.rs\0");
        assert_eq!(listed, ["ordinary.rs", "od\nd.rs"]);
    }

    // --- what the picker is scoped to (#130) -------------------------------

    #[test]
    fn project_scope_on_looks_at_the_repository_root() {
        // The whole point of having a Project: `meowr crates/meowr-app/src/lib.rs`
        // offered only what was under `src`, which #120 recorded and declined
        // to fix until a root existed.
        let root = std::path::Path::new("/home/user/proj");
        let listing = listing_of(
            true,
            std::path::Path::new("crates/meowr-app/src"),
            Some(root),
            std::path::Path::new("/home/user"),
        );
        assert_eq!(
            listing,
            Listing::Git {
                directory: root.to_owned(),
                prefix: root.to_owned(),
            }
        );
    }

    #[test]
    fn a_root_that_is_already_where_paths_resolve_needs_no_prefix() {
        // The ordinary case -- launched from the top of the repository -- and
        // the one that decides whether rows read `crates/...` or
        // `/home/user/proj/crates/...`.
        let root = std::path::Path::new("/home/user/proj");
        let listing = listing_of(true, std::path::Path::new("src"), Some(root), root);
        assert_eq!(
            listing,
            Listing::Git {
                directory: root.to_owned(),
                prefix: std::path::PathBuf::new(),
            }
        );
    }

    #[test]
    fn a_file_in_no_repository_has_no_project() {
        // ADR-0010's accepted cost, and the thing it refuses to paper over:
        // `~/docs` is not a repository and is edited daily. No fallback.
        let listing = listing_of(
            true,
            std::path::Path::new("/home/user/docs"),
            None,
            std::path::Path::new("/home/user"),
        );
        assert_eq!(listing, Listing::NoProject);
    }

    #[test]
    fn project_scope_off_looks_where_the_file_is() {
        // Consult's `nil` case, and the pre-Project editor: told to, rather
        // than guessing. Inside a repository the listing is still git's, so
        // `target/` and the rest of `.gitignore` stay out of the picker --
        // only the directory it runs in differs from `project-scope` on.
        let listing = listing_of(
            false,
            std::path::Path::new("src"),
            Some(std::path::Path::new("/home/user/proj")),
            std::path::Path::new("/home/user/proj"),
        );
        assert_eq!(
            listing,
            Listing::Git {
                directory: std::path::PathBuf::from("src"),
                prefix: std::path::PathBuf::from("src"),
            }
        );
    }

    #[test]
    fn project_scope_off_outside_a_repository_walks_the_directory() {
        // The Option's whole reason for existing. ADR-0010 takes the refusal
        // deliberately *and* names the casualty it costs -- `~/docs`, which
        // holds the ADRs and is edited beside the code daily -- then leaves
        // `project-scope` as the switch that rescues it. `git ls-files` exits
        // 128 there, so the switch has to reach a different mechanism or it
        // rescues nothing and the refusal it promises to lift is still the
        // answer.
        let listing = listing_of(
            false,
            std::path::Path::new("/home/user/docs"),
            None,
            std::path::Path::new("/home/user"),
        );
        assert_eq!(
            listing,
            Listing::Walk {
                directory: std::path::PathBuf::from("/home/user/docs"),
                prefix: std::path::PathBuf::from("/home/user/docs"),
            }
        );
    }

    #[tokio::test]
    async fn the_files_under_a_directory_git_ignores_whole_are_walked_rather_than_none() {
        // `SPC f F` (#149). Doom's below-the-root branch lists *every* file,
        // ignored or not (`project-find-file-in` with `include-all`); meowr keeps
        // git's filter, because `target/` is the noise it keeps out. But inside
        // a directory git ignores entirely, the filter's answer is "nothing",
        // and that is false -- the files are right there. So an empty listing
        // is walked instead, and a directory with some ignored files in it is
        // still filtered.
        let at = std::env::temp_dir().join(format!("meowr-under-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&at);
        std::fs::create_dir_all(at.join("ignored/deep")).unwrap();
        std::fs::create_dir_all(at.join("kept")).unwrap();
        std::fs::write(at.join(".gitignore"), "ignored/\n*.log\n").unwrap();
        std::fs::write(at.join("ignored/x.txt"), "").unwrap();
        std::fs::write(at.join("ignored/deep/y.txt"), "").unwrap();
        std::fs::write(at.join("kept/a.txt"), "").unwrap();
        std::fs::write(at.join("kept/noise.log"), "").unwrap();
        let init = std::process::Command::new("git")
            .args(["init", "-q"])
            .current_dir(&at)
            .status()
            .expect("git runs");
        assert!(init.success());

        let inside = at.join("ignored/x.txt");
        let rows = files_under(inside.to_str().unwrap()).await.expect("walked");
        let ignored = at.join("ignored");
        assert_eq!(
            rows,
            vec![
                ignored.join("deep/y.txt").to_string_lossy().into_owned(),
                ignored.join("x.txt").to_string_lossy().into_owned(),
            ]
        );

        let beside = at.join("kept/a.txt");
        let rows = files_under(beside.to_str().unwrap()).await.expect("listed");
        assert_eq!(
            rows,
            vec![at.join("kept/a.txt").to_string_lossy().into_owned()],
            "git's filter still applies where it leaves something"
        );
        std::fs::remove_dir_all(&at).unwrap();
    }

    #[tokio::test]
    async fn a_walk_finds_what_is_under_a_directory_and_not_what_is_hidden() {
        // The decision is unit-tested above; this is the mechanism, which is
        // the half that has a filesystem in it. Rooted in a real directory
        // because the thing being proved is that there is no repository here
        // and rows come back anyway -- which is `~/docs`, and which `git
        // ls-files` answered with exit 128.
        let at = std::env::temp_dir().join(format!("meowr-walk-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&at);
        std::fs::create_dir_all(at.join("sub")).unwrap();
        std::fs::create_dir_all(at.join(".git")).unwrap();
        std::fs::write(at.join("meowr-context.md"), "").unwrap();
        std::fs::write(at.join("sub/deeper.md"), "").unwrap();
        std::fs::write(at.join(".gitignore"), "").unwrap();
        std::fs::write(at.join(".git/HEAD"), "").unwrap();

        let rows = walked(&at)
            .await
            .expect("a directory that is plainly there");

        // Sorted, recursive, and the dot is gone in both the file and the
        // directory spelling -- `.git/HEAD` is the row that proves the skip
        // stopped the descent rather than merely dropping the directory's name.
        assert_eq!(rows, vec!["meowr-context.md", "sub/deeper.md"]);
        std::fs::remove_dir_all(&at).unwrap();
    }

    #[tokio::test]
    async fn a_link_to_a_directory_is_neither_followed_nor_offered() {
        // It is not a file, so a row naming one is a candidate the picker
        // cannot open -- which is #120, the bug this whole listing exists
        // downstream of. `file_type` does not follow a link, so it reported
        // "not a directory" and the row went out.
        let at = std::env::temp_dir().join(format!("meowr-link-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&at);
        std::fs::create_dir_all(at.join("real")).unwrap();
        std::fs::write(at.join("real/inside.md"), "").unwrap();
        std::fs::write(at.join("plain.md"), "").unwrap();
        std::os::unix::fs::symlink(at.join("real"), at.join("loop")).unwrap();
        std::os::unix::fs::symlink(at.join("plain.md"), at.join("alias.md")).unwrap();

        let rows = walked(&at)
            .await
            .expect("a directory that is plainly there");

        // `loop` is absent from both halves: not a row, and `loop/inside.md`
        // is not one either, so it was not descended into. A link to a *file*
        // is still a file worth opening.
        assert_eq!(rows, vec!["alias.md", "plain.md", "real/inside.md"]);
        std::fs::remove_dir_all(&at).unwrap();
    }

    #[tokio::test]
    async fn a_subject_becomes_the_directory_it_names() {
        // The session names the subject and this resolves it, which is
        // ADR-0010's division: a Project is a `git rev-parse` away and the
        // refusal belongs to the side that knows where it looked.
        let at = std::env::temp_dir().join(format!("meowr-subject-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&at);
        std::fs::create_dir_all(at.join("sub")).unwrap();
        std::fs::write(at.join("sub/one.rs"), "").unwrap();

        // `Here` is the file's parent, and needs no repository to be one.
        assert_eq!(
            directory_of(
                &meowr_app::Subject::Here(at.join("sub/one.rs").display().to_string()),
                true
            )
            .await,
            Ok(at.join("sub"))
        );
        // A directory named outright is taken as it stands.
        assert_eq!(
            directory_of(
                &meowr_app::Subject::Directory(at.display().to_string()),
                true
            )
            .await,
            Ok(at.clone())
        );
        std::fs::remove_dir_all(&at).unwrap();
    }

    #[tokio::test]
    async fn a_directory_that_is_not_a_directory_says_so() {
        // `SPC s D` takes whatever was typed, and a typo is the ordinary case.
        // Refused by name rather than handed to `rg` to fail its own way.
        let at = std::env::temp_dir().join(format!("meowr-nodir-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&at);

        let why = directory_of(&meowr_app::Subject::Directory(at.display().to_string()), true)
            .await
            .expect_err("nothing there to search");

        assert!(why.contains("is not a directory"), "{why}");
    }

    #[tokio::test]
    async fn searching_a_project_outside_a_repository_refuses_and_names_where() {
        // ADR-0010, which declines a fallback deliberately: a silent one reads
        // as success whether or not it found anything. `project-scope` *off* is
        // the rescue that Option exists to be, and turns the row into a search
        // of the directory rather than a refusal.
        let at = std::env::temp_dir().join(format!("meowr-noproject-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&at);
        std::fs::create_dir_all(&at).unwrap();
        std::fs::write(at.join("one.rs"), "").unwrap();
        let file = meowr_app::Subject::Project(at.join("one.rs").display().to_string());

        let why = directory_of(&file, true)
            .await
            .expect_err("a directory in no repository");
        assert!(why.contains("is in no git repository"), "{why}");

        assert_eq!(
            directory_of(&file, false).await,
            Ok(at.clone()),
            "off searches the directory rather than refusing"
        );
        std::fs::remove_dir_all(&at).unwrap();
    }

    #[tokio::test]
    async fn a_directory_that_is_not_there_says_so_rather_than_offering_nothing() {
        // ADR-0010's rule, at the mechanism this commit adds: an empty list is
        // not an answer to "why is there nothing here".
        let at = std::env::temp_dir().join(format!("meowr-absent-{}", std::process::id()));
        let why = walked(&at).await.expect_err("nothing to walk");
        assert!(why.contains("could not be read"), "{why}");
    }

    #[test]
    fn a_walk_passes_over_what_a_dot_hides() {
        // `fd`'s default, which is what consult runs when
        // `consult-project-function` is `nil`. Outside a repository there is
        // usually no `.gitignore` to honour -- that is most of what being
        // outside a repository *means* -- so the dot is the rule that is left,
        // and `.git` itself is the entry it most has to catch.
        assert!(hidden(std::ffi::OsStr::new(".git")));
        assert!(hidden(std::ffi::OsStr::new(".gitignore")));
        assert!(!hidden(std::ffi::OsStr::new("meowr-context.md")));
        assert!(!hidden(std::ffi::OsStr::new("src")));
    }

    #[test]
    fn a_bare_filename_is_asked_about_in_the_working_directory() {
        // Its parent is the *empty* path, which is not nothing -- it is where
        // paths already resolve from, and an empty `current_dir` would run `git`
        // nowhere at all.
        let cwd = std::path::Path::new("/home/user/proj");
        assert_eq!(asking_about(Some("README.md"), cwd), Some(cwd.to_owned()));
        assert_eq!(asking_about(None, cwd), Some(cwd.to_owned()));
        assert_eq!(
            asking_about(Some("crates/meowr/src/main.rs"), cwd),
            Some(std::path::PathBuf::from("crates/meowr/src")),
            "and a path with a parent is asked about there"
        );
        assert_eq!(
            asking_about(Some("/"), cwd),
            None,
            "the filesystem root has no directory above it"
        );
    }

    #[test]
    fn a_listing_from_a_subdirectory_carries_it_on_every_candidate() {
        // #120: `meowr src/lib.rs` ran `git ls-files` in `src`, so a row read
        // `thing.py` while opening one resolved against the project root — and
        // the picker reported "No such file or directory" for a file plainly
        // there. The directory the listing came from has to be put back.
        let listed = candidates(
            std::path::Path::new("src"),
            "lib.rs\0thing.py\0nested/deeper.rs\0",
        );
        assert_eq!(
            listed,
            ["src/lib.rs", "src/thing.py", "src/nested/deeper.rs"]
        );
    }

    #[test]
    fn a_listing_from_the_working_directory_is_left_alone() {
        // `meowr README.md` and `meowr` with no argument both list from where
        // paths already resolve, so joining would only add `./` to every row.
        let listed = candidates(std::path::Path::new(""), "README.md\0src/lib.rs\0");
        assert_eq!(listed, ["README.md", "src/lib.rs"]);
    }

    #[test]
    fn an_absolute_launch_path_gives_absolute_candidates() {
        // Correct rather than pretty: they resolve from anywhere, which is what
        // matters, and this is the case where the alternative would be to work
        // out a relative path between two absolute ones.
        let listed = candidates(std::path::Path::new("/home/user/proj/src"), "lib.rs\0");
        assert_eq!(listed, ["/home/user/proj/src/lib.rs"]);
    }

    #[test]
    fn an_empty_listing_offers_nothing() {
        // No repository, or a repository with nothing tracked in it.
        //
        // The empty separator is NUL now, not a newline: under `-z` a payload
        // of "\n\n" is one file *named* that, which git allows and which
        // `a_filename_containing_a_newline_survives_the_listing` wants offered.
        assert!(candidates(std::path::Path::new("src"), "").is_empty());
        assert!(candidates(std::path::Path::new(""), "\0\0").is_empty());
    }

    fn parsed(args: &[&str]) -> Invocation {
        invocation(args.iter().map(|a| (*a).to_string()))
    }

    #[test]
    fn a_bare_name_is_the_file_to_open() {
        assert_eq!(
            parsed(&["notes.rs"]),
            Invocation::Open {
                path: Some("notes.rs".into()),
                skip_config: false
            }
        );
    }

    #[test]
    fn no_argument_opens_the_scratch_buffer() {
        assert_eq!(
            parsed(&[]),
            Invocation::Open {
                path: None,
                skip_config: false
            }
        );
    }

    #[test]
    fn help_and_version_are_answered_without_taking_the_terminal() {
        // The first thing anyone types about an unfamiliar binary. Treating
        // `--help` as a filename opens a file called `--help`.
        assert_eq!(parsed(&["--help"]), Invocation::Help { skip_config: false });
        assert_eq!(parsed(&["-h"]), Invocation::Help { skip_config: false });
        assert_eq!(parsed(&["--version"]), Invocation::Version);
        assert_eq!(parsed(&["-V"]), Invocation::Version);
    }

    #[test]
    fn no_config_reaches_help_from_either_side_of_it() {
        // `--help` prints keys read out of a live keymap, so `--no-config`
        // changes what it says and has to be seen wherever it was written.
        for line in [["--no-config", "--help"], ["--help", "--no-config"]] {
            assert_eq!(
                parsed(&line),
                Invocation::Help { skip_config: true },
                "{line:?}"
            );
        }
    }

    #[test]
    fn an_unknown_flag_is_not_a_filename() {
        assert_eq!(
            parsed(&["--colour"]),
            Invocation::Complaint("unknown option `--colour`".into())
        );
    }

    #[test]
    fn more_than_one_file_is_refused_rather_than_silently_dropped() {
        assert_eq!(
            parsed(&["one.rs", "two.rs"]),
            Invocation::Complaint("2 files given; meowr opens one".into())
        );
    }

    #[test]
    fn an_empty_path_is_still_a_path() {
        // Not a flag, so it reaches the open path and fails there with a real
        // filesystem error rather than being mistaken for "no argument".
        assert_eq!(
            parsed(&[""]),
            Invocation::Open {
                path: Some(String::new()),
                skip_config: false
            }
        );
    }

    #[test]
    fn a_path_that_begins_with_a_dash_can_still_be_opened() {
        // `--` ends the options, as it does everywhere else.
        assert_eq!(
            parsed(&["--", "-weird.rs"]),
            Invocation::Open {
                path: Some("-weird.rs".into()),
                skip_config: false
            }
        );
    }

    /// Meow binds `M-'` to the surround keymap. Crossterm delivers Alt as a
    /// modifier, so a translation that only knows about Control drops it and
    /// hands the session a bare `'` — leaving the keymap unreachable.
    #[test]
    fn alt_is_not_flattened_onto_the_bare_character() {
        let alt = to_key(press(KeyCode::Char('\''), KeyModifiers::ALT));
        let bare = to_key(press(KeyCode::Char('\''), KeyModifiers::NONE));

        assert_ne!(alt, bare, "M-' must not arrive as a bare `'`");
    }

    /// The arrows and both page keys ended at this function's `_ => None`
    /// until #71, so nothing downstream could have delivered them however it
    /// was designed.
    ///
    /// meowr binds them too since #81, which is a change from what this said.
    /// Meow is modal and reaches everything from the home row, so `meow-normal`
    /// binds no arrow and no page key at all — but four keys arriving at the
    /// session and dying there is not the same as a decision, and an arrow that
    /// moved the narrowing widget and did nothing in a buffer was the shape of
    /// that oversight.
    #[test]
    fn the_navigation_keys_reach_the_session() {
        for (code, expected) in [
            (KeyCode::Tab, Key::Tab),
            (KeyCode::BackTab, Key::BackTab),
            (KeyCode::Up, Key::Up),
            (KeyCode::Down, Key::Down),
            (KeyCode::Left, Key::Left),
            (KeyCode::Right, Key::Right),
            (KeyCode::PageUp, Key::PageUp),
            (KeyCode::PageDown, Key::PageDown),
        ] {
            assert_eq!(
                to_key(press(code, KeyModifiers::NONE)),
                Some(expected),
                "{code:?} should arrive as {expected:?}"
            );
        }
    }
}

#[cfg(test)]
mod no_config_tests {
    use super::*;

    fn parsed(args: &[&str]) -> Invocation {
        invocation(args.iter().map(|a| a.to_string()))
    }

    #[test]
    fn no_config_is_a_flag_and_still_opens_the_file() {
        assert_eq!(
            parsed(&["--no-config", "notes.rs"]),
            Invocation::Open {
                path: Some("notes.rs".into()),
                skip_config: true
            }
        );
    }

    #[test]
    fn no_config_is_off_unless_asked_for() {
        assert_eq!(
            parsed(&["notes.rs"]),
            Invocation::Open {
                path: Some("notes.rs".into()),
                skip_config: false
            }
        );
    }

    #[test]
    fn a_file_named_like_the_flag_is_still_a_file_after_the_separator() {
        assert_eq!(
            parsed(&["--", "--no-config"]),
            Invocation::Open {
                path: Some("--no-config".into()),
                skip_config: false
            }
        );
    }
}
