//! Declarations: what a Plugin asks for, collected before any of it is applied.
//!
//! A Plugin evaluates to a set of these and applies none of them itself. They
//! are gathered while the file runs and resolved together once it finishes,
//! which buys two things a form-at-a-time config cannot have.
//!
//! A binding may name a command defined forty lines further down, so source
//! order is not load-bearing — Steel evaluates top to bottom and no Emacs user
//! expects otherwise. And a set that does not resolve applies *nothing*, so a
//! broken config leaves the defaults standing rather than a state that is
//! neither the defaults nor what the file asked for (#49).
//!
//! Emacs's partial application is useful precisely because each of its forms
//! acts immediately. These only record, so the useful half is not available
//! here and the confusing half is all that would be inherited.

use super::*;
use std::collections::{HashMap, HashSet};

/// One thing a Plugin asks for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Declaration {
    /// Bind a key sequence, in the keymap or in Ring 2 — [`Session::bind`]
    /// decides which by looking at the sequence.
    Bind {
        state: State,
        keys: Vec<Key>,
        command: String,
    },
    /// Take a binding away. The `#f` of #48's `define-key`.
    Unbind { state: State, keys: Vec<Key> },
    /// Name a leader prefix, so the menu can say what lies under it.
    Prefix { keys: Vec<Key>, name: String },
    /// Define a command as an ordered list of other commands' names.
    Command {
        name: String,
        description: String,
        body: Vec<String>,
    },
    /// Set an Option. The name and the value are checked together, because
    /// neither says much alone: `400` is a fine duration and a strange
    /// boolean, and only the name knows which was wanted.
    Option { name: String, value: OptionValue },
    /// Attach a command to a hook point, by name.
    ///
    /// Only the hook points in [`HookPoint::FOR_COMMANDS`] accept one.
    Hook { point: HookPoint, command: String },
    /// Say which command serves a Language.
    ///
    /// The Language is a name as `Languages` writes it -- `rust`, `go` -- and is
    /// checked against what meowr actually knows, the same way an Option's name
    /// is: a config that declares a server for a Language meowr cannot parse has
    /// made a mistake worth hearing about, not one to swallow.
    ///
    /// #119, and a Declaration rather than an Option on purpose -- see
    /// [`meowr_command::LanguageServer`] for why that keeps #50 intact.
    LanguageServer {
        language: String,
        server: meowr_command::LanguageServer,
    },
    /// Say which program formats a Language, or `None` for none (ADR-0014).
    ///
    /// The shape `LanguageServer` has, checked the same way: a Language meowr
    /// cannot parse is a mistake worth hearing about.
    Formatter {
        language: String,
        formatter: Option<meowr_command::Formatter>,
    },
    /// Take a hook away, by the name it was registered under.
    ///
    /// A name rather than a command, because the two kinds of hook are named
    /// differently and both are removable: `auto-exit-insert-on-save` is a
    /// closure meowr ships, and a command-hook is known by the command it
    /// names. #48's own example removes the former.
    Unhook { point: HookPoint, name: String },
}

// A hook that names a command still cannot *run* one: `Hooks` holds a closure
// that receives only a `Ctx`, and the registry is not reachable from inside it
// — the wall #47 hit and #64 did not move. What #64 settled is that the
// session dispatches the name *after* the closures have run, which is honest
// at two of the five hook points and not at the other three. See
// `HookPoint::FOR_COMMANDS`, which is beside `HookPoint` because which hook points can
// dispatch is a fact about a hook point, not about this pass.

/// A set of [`Declaration`]s, resolved together or not at all.
#[derive(Debug, Clone, Default)]
pub struct Declarations {
    items: Vec<Declaration>,
}

impl Declarations {
    pub fn push(&mut self, declaration: Declaration) -> &mut Self {
        self.items.push(declaration);
        self
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }
}

/// Something about a set that stopped it being applied.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Problem(String);

impl std::fmt::Display for Problem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Problem {}

impl Session {
    /// Resolve a set of Declarations and apply it, or report why not.
    ///
    /// Every problem is reported rather than the first: a config with three
    /// dangling names should say all three, because the alternative is three
    /// runs to find out.
    ///
    /// Atomic. The names are checked first, then the bindings are made against
    /// *copies* of the keymap and the leader tree, which are swapped in only if
    /// every one of them succeeded — because whether a binding is ambiguous is
    /// not knowable until it is attempted. Registering commands and adding
    /// hooks cannot fail once the names have been checked, so they go last and
    /// need no copy.
    pub fn apply(&mut self, declarations: Declarations) -> Result<(), Vec<Problem>> {
        let items = declarations.items;
        let mut problems = Vec::new();

        // What will exist once this set has been applied: everything already
        // registered, plus everything the set defines.
        let mut known: HashSet<String> = self.registry.names().map(str::to_owned).collect();
        let mut defined: HashMap<&str, &[String]> = HashMap::new();
        for item in &items {
            if let Declaration::Command { name, body, .. } = item {
                if known.contains(name) {
                    problems.push(Problem(format!("`{name}` is already a command")));
                } else if defined.insert(name, body).is_some() {
                    problems.push(Problem(format!("`{name}` is defined twice")));
                }
                known.insert(name.clone());
            }
        }

        // Every name anything points at has to be one of those.
        for item in &items {
            let named: Vec<&String> = match item {
                Declaration::Bind { command, .. } => vec![command],
                Declaration::Hook { command, .. } => vec![command],
                Declaration::Command { body, .. } => body.iter().collect(),
                Declaration::Unbind { .. }
                | Declaration::Prefix { .. }
                | Declaration::Option { .. }
                | Declaration::LanguageServer { .. }
                | Declaration::Formatter { .. }
                | Declaration::Unhook { .. } => Vec::new(),
            };
            for name in named {
                if !known.contains(name) {
                    problems.push(Problem(format!("nothing is called `{name}`")));
                }
            }
        }

        // Options resolve into a copy for the same reason the keymap does:
        // a set that does not resolve applies *nothing*, so a misspelt option
        // has to stop the bindings beside it landing rather than being dropped
        // on its own while they do.
        let mut options = self.options;
        for item in &items {
            if let Declaration::Option { name, value } = item {
                if let Err(e) = options.set(name, value.clone()) {
                    problems.push(Problem(e));
                }
            }
        }

        // Servers resolve into a copy for the reason the Options above do: a set
        // that does not resolve applies *nothing*, so a server declared for a
        // Language meowr cannot parse has to stop the bindings beside it landing
        // rather than being dropped quietly on its own while they do.
        let mut servers = self.servers.clone();
        for item in &items {
            if let Declaration::LanguageServer { language, server } = item {
                match self.languages.by_name(language) {
                    Some(_) => {
                        servers.insert(language.clone(), server.clone());
                    }
                    // Lists the real ones, which is the `set-option`
                    // precedent in full rather than half: a refusal that does
                    // not say what *would* have worked makes the reader go
                    // looking, and the list is right here.
                    None => problems.push(Problem(format!(
                        "no Language is called `{language}`, so nothing can serve it; meowr has {}",
                        meowr_command::listed(self.languages.names())
                    ))),
                }
            }
        }

        // Formatters resolve into a copy for the servers' reason, and are
        // removed by name the way `define-key` unbinds with `#f`.
        let mut formatters = self.formatters.clone();
        for item in &items {
            if let Declaration::Formatter { language, formatter } = item {
                match (self.languages.by_name(language), formatter) {
                    (Some(_), Some(formatter)) => {
                        formatters.insert(language.clone(), formatter.clone());
                    }
                    (Some(_), None) => {
                        formatters.remove(language);
                    }
                    (None, _) => problems.push(Problem(format!(
                        "no Language is called `{language}`, so nothing can format it; meowr has {}",
                        meowr_command::listed(self.languages.names())
                    ))),
                }
            }
        }

        // Whether a hook point accepts a command at all is policy, so it lives with
        // the session. The Steel bridge knows only that the symbol named a
        // hook point that exists.
        for item in &items {
            if let Declaration::Hook { point, .. } = item
                && !HookPoint::FOR_COMMANDS.contains(point)
            {
                problems.push(Problem(format!(
                    "`{}` does not take a command; meowr has {}",
                    point.name(),
                    HookPoint::listed(HookPoint::FOR_COMMANDS)
                )));
            }
        }

        // A hook can only be taken away if it is there — counting what this
        // very set adds, so a config may remove and re-add in one file. A name
        // that matches nothing is a typo, and #59 says a typo stops the set
        // rather than being dropped in silence.
        for item in &items {
            if let Declaration::Unhook { point, name } = item {
                let shipped = self.hooks.names_at(*point).any(|n| n == name);
                let attached = self.hooks.commands(*point).contains(&name.as_str());
                let in_set = items.iter().any(|other| {
                    matches!(other, Declaration::Hook { point: p, command }
                        if p == point && command == name)
                });
                if !shipped && !attached && !in_set {
                    problems.push(Problem(format!(
                        "no hook called `{name}` at `{}`",
                        point.name()
                    )));
                }
            }
        }

        problems.extend(cycles(&defined));
        if !problems.is_empty() {
            return Err(problems);
        }

        // Bindings against copies, because ambiguity only shows on the attempt.
        let mut keymap = self.keymap.clone();
        let mut leader = self.leader.clone();
        // In a canonical order, not the source's — the whole point of gathering
        // a set is that writing it down in a different order means the same
        // thing. Unbinding first, so a key can be freed and reused in one set;
        // then prefixes, shortest first, so `SPC d` exists before `SPC d e` and
        // before anything hangs off either; then the bindings themselves.
        let mut ordered: Vec<&Declaration> = items.iter().collect();
        ordered.sort_by_key(|item| match item {
            Declaration::Unbind { .. } => (0, 0),
            Declaration::Prefix { keys, .. } => (1, keys.len()),
            Declaration::Bind { .. } => (2, 0),
            Declaration::Command { .. } => (3, 0),
            Declaration::Option { .. } => (4, 0),
            Declaration::LanguageServer { .. } => (4, 0),
            Declaration::Formatter { .. } => (4, 0),
            Declaration::Hook { .. } => (5, 0),
            Declaration::Unhook { .. } => (6, 0),
        });
        for item in ordered {
            let attempt = match item {
                Declaration::Bind {
                    state,
                    keys,
                    command,
                } => bind_into(&mut keymap, &mut leader, *state, keys, command),
                Declaration::Prefix { keys, name } => match leader_suffix(State::Normal, keys) {
                    Some(suffix) => {
                        leader.set(suffix.to_vec(), leader::Entry::Prefix(name.clone()))
                    }
                    None => Err(meowr_command::BindError::NoPrefix),
                },
                Declaration::Unbind { state, keys } => {
                    match leader_suffix(*state, keys) {
                        Some(suffix) => leader.unset(suffix),
                        None => keymap.unbind(*state, keys),
                    };
                    Ok(())
                }
                _ => Ok(()),
            };
            if let Err(e) = attempt {
                problems.push(Problem(format!("{}: {e}", written(item))));
            }
        }
        if !problems.is_empty() {
            return Err(problems);
        }

        self.keymap = keymap;
        self.leader = leader;
        self.options = options;
        self.servers = servers;
        self.formatters = formatters;
        for item in &items {
            if let Declaration::Unhook { point, name } = item {
                self.hooks.remove(*point, name);
                self.hooks.remove_command(*point, name);
            }
        }
        for item in items {
            if let Declaration::Hook { point, command } = &item {
                self.hooks.add_command(*point, command.clone());
            }
            if let Declaration::Command {
                name,
                description,
                body,
            } = item
            {
                let command = Command::new(name, Handler::Composed(body)).described(description);
                // Checked above; a failure here would mean the check and this
                // disagree, which is worth hearing about loudly.
                self.registry
                    .register_new(command)
                    .expect("the name was free when it was checked");
            }
        }
        Ok(())
    }
}

/// Which of a set's own definitions eventually name themselves.
///
/// Only the set's own: a composition already in the registry was resolved by an
/// earlier pass, when the names in this set did not exist, so no edge can lead
/// from an old command back into a new one. With no reload (#49) there is one
/// pass per session anyway.
fn cycles(defined: &HashMap<&str, &[String]>) -> Vec<Problem> {
    let mut problems = Vec::new();
    for start in defined.keys() {
        let mut seen: Vec<&str> = vec![start];
        let mut frontier: Vec<&str> = defined[start].iter().map(String::as_str).collect();
        while let Some(next) = frontier.pop() {
            if next == *start {
                problems.push(Problem(format!(
                    "`{start}` eventually calls itself, through {}",
                    seen.join(" -> ")
                )));
                break;
            }
            if seen.contains(&next) {
                continue;
            }
            seen.push(next);
            if let Some(body) = defined.get(next) {
                frontier.extend(body.iter().map(String::as_str));
            }
        }
    }
    problems.sort_by(|a, b| a.0.cmp(&b.0));
    problems
}

fn bind_into(
    keymap: &mut Keymap,
    leader: &mut leader::Leader,
    state: State,
    keys: &[Key],
    command: &str,
) -> Result<(), meowr_command::BindError> {
    match leader_suffix(state, keys) {
        Some(suffix) => leader.set(suffix.to_vec(), leader::Entry::Command(command.into())),
        None => keymap.bind(state, keys, command),
    }
}

/// A declaration, written the way its own config line would be.
fn written(item: &Declaration) -> String {
    match item {
        Declaration::Bind { keys, command, .. } => {
            format!("`{}` to `{command}`", Keys(keys))
        }
        Declaration::Prefix { keys, name } => format!("`{}` as `{name}`", Keys(keys)),
        Declaration::Unbind { keys, .. } => format!("unbinding `{}`", Keys(keys)),
        Declaration::Command { name, .. } => format!("`{name}`"),
        Declaration::Option { name, value } => format!("`{name}` to `{value}`"),
        Declaration::LanguageServer { language, server } => {
            // The arguments are shown space-separated for reading, which is not
            // how they are *passed* -- they never become one string anywhere but
            // in this message.
            let args = server.args.join(" ");
            let shown = if args.is_empty() {
                server.program.clone()
            } else {
                format!("{} {args}", server.program)
            };
            format!("`{language}` served by `{shown}`")
        }
        Declaration::Formatter { language, formatter } => match formatter {
            Some(formatter) => {
                let shown = std::iter::once(formatter.program.as_str())
                    .chain(formatter.args.iter().map(String::as_str))
                    .collect::<Vec<_>>()
                    .join(" ");
                format!("`{language}` formatted by `{shown}`")
            }
            None => format!("`{language}` formatted by nothing"),
        },
        Declaration::Hook { point, command } => {
            format!("`{command}` at `{}`", point.name())
        }
        Declaration::Unhook { point, name } => {
            format!("removing `{name}` from `{}`", point.name())
        }
    }
}
