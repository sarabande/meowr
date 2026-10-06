//! Finding, reading and evaluating `init.scm`.
//!
//! The binary's job, because a config is a file and the domain crates do no
//! I/O — the same division `Action::Save` and `take_reparse` already draw.
//!
//! **Before `ratatui::init`**, which is the decision #49 turned on. meowr
//! already answers `--help` there, because a `--help` that grabs the terminal
//! to print into it is no help at all, and config evaluation belongs in the
//! same window for two reasons of its own. #44 found that Steel writes a
//! bytecode disassembly to *stdout* when a program overflows its stack, and
//! on the ordinary terminal that is harmless and even readable, where on the
//! alternate screen it would be sprayed across the editor. And starting the
//! event loop with the config already applied means no keypress can land
//! between the two.

use std::path::PathBuf;
use std::time::Duration;

use steel::steel_vm::engine::Engine;

pub mod api;

/// How long a config may take before it is abandoned.
///
/// Counted from when the engine is ready, not from when meowr started: an
/// engine costs 100-250ms to build (#44) and that is meowr's own cost, not
/// something the config can be blamed for or made to fit inside.
///
/// #44 measured a real config evaluating in about half a millisecond, so this
/// sits three orders of magnitude above the honest case and cannot fire by
/// accident. It exists for the failure #49 found that nothing else covers: a
/// tail call like `(let loop () (loop))` never overflows the stack, so Steel
/// never returns an error — it simply spins, before the terminal is taken,
/// leaving no UI, no message and nothing to press.
pub const BUDGET: Duration = Duration::from_secs(2);

/// Why a config did not run.
#[derive(Debug)]
pub enum Failed {
    /// The file could not be read. Distinct from absent: see [`find`].
    Unreadable(std::io::Error),
    /// Steel refused it — a parse error, an unbound identifier, a division by
    /// zero. #44 confirmed these arrive as values rather than unwinding.
    Rejected(String),
    /// It was still running when [`BUDGET`] ran out.
    Overran,
}

impl std::fmt::Display for Failed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Failed::Unreadable(e) => write!(f, "{e}"),
            Failed::Rejected(message) => f.write_str(message),
            Failed::Overran => write!(
                f,
                "still running after {}s, so it was abandoned",
                BUDGET.as_secs()
            ),
        }
    }
}

/// Where the config lives, honouring `XDG_CONFIG_HOME`.
///
/// `None` when there is no home to look in at all. That a file is *absent* is
/// not an error and not reported here — it is the normal state of a fresh
/// checkout, so [`evaluate`] answers `Ok` for it and says nothing.
pub fn find() -> Option<PathBuf> {
    let base = match std::env::var_os("XDG_CONFIG_HOME") {
        Some(xdg) if !xdg.is_empty() => PathBuf::from(xdg),
        _ => PathBuf::from(std::env::var_os("HOME")?).join(".config"),
    };
    Some(base.join("meowr").join("init.scm"))
}

/// Read and run the config, if there is one.
///
/// `Ok(false)` means there was nothing to run, which is not a failure.
pub fn evaluate(path: Option<&std::path::Path>) -> Result<api::Collected, Failed> {
    let Some(path) = path else {
        return Ok(api::Collected::default());
    };
    let source = match std::fs::read_to_string(path) {
        Ok(source) => source,
        // Absent is silent; anything else means you meant to have a config and
        // have not got one, which is exactly where silence looks like meowr
        // ignoring you.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok(api::Collected::default());
        }
        Err(e) => return Err(Failed::Unreadable(e)),
    };
    run(source, path.to_path_buf())
}

/// Build an engine and run `source` in it, under the budget.
///
/// On a worker rather than inline, so a config that never returns can be
/// abandoned. Abandoning it leaks the thread, which is survivable exactly
/// once: this runs before the event loop, and a detached thread dies with the
/// process.
fn run(source: String, path: PathBuf) -> Result<api::Collected, Failed> {
    enum Progress {
        /// The engine exists; the clock on the config starts now.
        Ready,
        Done(Result<api::Collected, Failed>),
    }

    let (tx, rx) = std::sync::mpsc::channel();
    let ready = tx.clone();
    std::thread::spawn(move || {
        let mut engine = Engine::new();
        // The functions a config may call. Installed before it runs and never
        // after, so `init.scm` cannot reach anything meowr did not offer.
        let collector = api::install(&mut engine);
        let _ = ready.send(Progress::Ready);
        // With the path, so `(require "keys.scm")` resolves against the config's
        // own directory. Given only a string, Steel has no base to resolve from
        // and looks in its own cogs directory instead — which is where a config
        // split across files would mysteriously fail. Steel's module system is
        // on by default (#44), so there is no `include` for meowr to invent.
        let outcome = engine
            .compile_and_run_raw_program_with_path(source, path)
            .map_err(|e| Failed::Rejected(e.to_string()))
            .map(|_| std::mem::take(&mut *collector.lock().unwrap_or_else(|e| e.into_inner())));
        let _ = tx.send(Progress::Done(outcome));
    });

    // Building the engine is meowr's own cost and terminates, so it is waited
    // for without a clock. The budget covers what the *config* does, which is
    // the only part that can be written badly enough never to finish — and it
    // has to, or the number would mean something different in a debug build,
    // where an engine takes far longer than the 252ms #44 measured optimised.
    if !matches!(rx.recv(), Ok(Progress::Ready)) {
        return Err(Failed::Overran);
    }
    match rx.recv_timeout(BUDGET) {
        Ok(Progress::Done(outcome)) => outcome,
        _ => Err(Failed::Overran),
    }
}

#[cfg(test)]
mod tests;
