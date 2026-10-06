//! The functions a config calls, and what they record.
//!
//! Every one of them **declares**; none of them acts. There is no way to run a
//! command while a config loads, because there is no function that runs one —
//! #48 dropped `call` for exactly that reason, so "you cannot act at load time"
//! is a thing that cannot be written rather than a rule to enforce.
//!
//! The collector is `Arc<Mutex<_>>` and not the `Rc<RefCell<_>>` this
//! single-threaded editor would prefer, because `register_fn` requires
//! `Send + Sync + 'static` (#44). That requirement is also why nothing here
//! touches a `Ctx`: it is a bundle of `&mut` borrows and cannot cross.

use std::sync::{Arc, Mutex};

use meowr_app::declare::{Declaration, Declarations};
use meowr_command::{HookPoint, Key, OptionValue, State, parse_keys};
use steel::rvals::SteelVal;
use steel::steel_vm::engine::Engine;
use steel::steel_vm::register_fn::RegisterFn;

/// What a config asked for, and what it got wrong asking.
#[derive(Debug, Default)]
pub struct Collected {
    pub declarations: Declarations,
    pub problems: Vec<String>,
}

type Shared = Arc<Mutex<Collected>>;

/// A name from a registry: a symbol, as #48 chose.
///
/// Symbols rather than strings for anything meowr can look up, and strings only
/// for key sequences — Emacs's own split, and it marks a real difference: a
/// symbol is checked against a registry, a key sequence is *parsed*.
fn symbol(value: &SteelVal, what: &str) -> Result<String, String> {
    match value {
        SteelVal::SymbolV(name) => Ok(name.to_string()),
        SteelVal::StringV(name) => Err(format!(
            "{what} should be a symbol, not a string: write '{name} rather than \"{name}\""
        )),
        other => Err(format!("{what} should be a symbol, got {other}")),
    }
}

fn text(value: &SteelVal, what: &str) -> Result<String, String> {
    match value {
        SteelVal::StringV(s) => Ok(s.to_string()),
        other => Err(format!("{what} should be a string, got {other}")),
    }
}

fn keys(value: &SteelVal, what: &str) -> Result<Vec<Key>, String> {
    let written = text(value, what)?;
    parse_keys(&written).map_err(|e| format!("{what}: {e}"))
}

fn state(value: &SteelVal) -> Result<State, String> {
    let name = symbol(value, "a State")?;
    // Against `State::ALL` rather than a list written here. The modeline and
    // the bindings buffer both spell a State with `State::name`, and a config's
    // spelling has to be the same one or a State could be nameable in one
    // direction and not the other. The error listing comes from the same place,
    // so a State added later cannot go missing from it.
    State::ALL
        .iter()
        .find(|state| state.name() == name)
        .copied()
        .ok_or_else(|| {
            let known: Vec<&str> = State::ALL.iter().map(|state| state.name()).collect();
            format!("`{name}` is not a State; meowr has {}", known.join(", "))
        })
}

/// An Option's value, narrowed to what one can be without knowing which
/// Option it is.
///
/// The type question that can be answered here, answered here. Which of the
/// three a given Option wants — and whether the number is a duration or a
/// count — belongs to `Options::set`, which is the only thing that knows.
fn scalar(option: &str, value: &SteelVal) -> Result<OptionValue, String> {
    match value {
        SteelVal::BoolV(on) => Ok(OptionValue::Bool(*on)),
        SteelVal::IntV(n) => Ok(OptionValue::Int(*n as i64)),
        SteelVal::SymbolV(name) => Ok(OptionValue::Symbol(name.to_string())),
        // The mistake #48 chose integer milliseconds to make impossible: `0.4`
        // is the natural thing to write, and refusing it at the door is why no
        // float formatting question ever arises.
        SteelVal::NumV(fraction) => Err(format!(
            "`{option}` takes a whole number, not {fraction}; a duration is written \
             in milliseconds, so {} rather than {fraction}",
            (fraction * 1000.0).round()
        )),
        SteelVal::StringV(name) => Err(format!(
            "`{option}` takes a symbol, not a string: write '{name} rather than \
             \"{name}\""
        )),
        other => Err(format!(
            "`{option}` takes a boolean, a whole number or a symbol, got {other}"
        )),
    }
}

/// A hook point, by the name an `init.scm` writes.
///
/// All five, because all five exist — whether a hook point *accepts* a command is
/// policy, and `Session::apply` is where a config is told about that. Here the
/// only question is whether the symbol names a hook point at all.
fn point(value: &SteelVal) -> Result<HookPoint, String> {
    let name = symbol(value, "a hook point")?;
    HookPoint::ALL
        .iter()
        .find(|p| p.name() == name)
        .copied()
        .ok_or_else(|| {
            format!(
                "`{name}` is not a hook point; meowr has {}",
                HookPoint::listed(HookPoint::ALL)
            )
        })
}

/// The body of a composed command: a list of command names.
fn names(value: &SteelVal) -> Result<Vec<String>, String> {
    let SteelVal::ListV(items) = value else {
        return Err(format!(
            "a command body should be a list of command names, got {value}"
        ));
    };
    items
        .iter()
        .map(|item| symbol(item, "a command name"))
        .collect()
}

/// A list of strings: a server's arguments.
///
/// Strings rather than symbols, unlike [`names`] beside it, and the difference
/// is the seam's own rule (#48): a symbol is checked against a registry and a
/// string is not. `--stdio` is checked against nothing — it is handed to a
/// program that meowr did not write.
fn strings(value: &SteelVal, what: &str) -> Result<Vec<String>, String> {
    match value {
        SteelVal::ListV(items) => items.iter().map(|i| text(i, what)).collect(),
        other => Err(format!("{what} should be a list of strings, got {other}")),
    }
}

/// Record a declaration, or the reason there is not one.
fn record(shared: &Shared, made: Result<Declaration, String>) {
    let mut collected = shared.lock().unwrap_or_else(|e| e.into_inner());
    match made {
        Ok(declaration) => {
            collected.declarations.push(declaration);
        }
        Err(problem) => collected.problems.push(problem),
    }
}

/// Give an engine the eight functions a config may call: `define-key`,
/// `define-prefix`, `define-command`, `set-option`, `set-language-server`,
/// `set-formatter` (ADR-0014), `add-hook` and `remove-hook` — the set #48
/// settled, and the two Declarations per Language that came after it.
///
/// Named rather than counted, because the count once said four while three
/// were registered and nothing caught it, and `remove-hook` then fell through
/// the gap between #61 and #64 with both of them closed.
pub fn install(engine: &mut Engine) -> Shared {
    let shared: Shared = Arc::default();

    let into = Arc::clone(&shared);
    engine.register_fn(
        "define-key",
        move |state_value: SteelVal, keys_value: SteelVal, command: SteelVal| {
            let made = (|| {
                let state = state(&state_value)?;
                let keys = keys(&keys_value, "a key sequence")?;
                // `#f` unbinds, which is `nil` in elisp and free to learn.
                if matches!(command, SteelVal::BoolV(false)) {
                    return Ok(Declaration::Unbind { state, keys });
                }
                Ok(Declaration::Bind {
                    state,
                    keys,
                    command: symbol(&command, "a command")?,
                })
            })();
            record(&into, made);
        },
    );

    let into = Arc::clone(&shared);
    engine.register_fn(
        "define-prefix",
        move |keys_value: SteelVal, name: SteelVal| {
            let made = (|| {
                Ok(Declaration::Prefix {
                    keys: keys(&keys_value, "a key sequence")?,
                    name: text(&name, "a prefix name")?,
                })
            })();
            record(&into, made);
        },
    );

    let into = Arc::clone(&shared);
    engine.register_fn(
        "define-command",
        move |name: SteelVal, description: SteelVal, body: SteelVal| {
            let made = (|| {
                Ok(Declaration::Command {
                    name: symbol(&name, "a command name")?,
                    // Required, not optional: a command bound to a leader key
                    // with no description is a blank row in the one interface
                    // that exists to prevent blank rows.
                    description: text(&description, "a description")?,
                    body: names(&body)?,
                })
            })();
            record(&into, made);
        },
    );

    let into = Arc::clone(&shared);
    engine.register_fn("set-option", move |name: SteelVal, value: SteelVal| {
        let made = (|| {
            let name = symbol(&name, "an option name")?;
            Ok(Declaration::Option {
                value: scalar(&name, &value)?,
                name,
            })
        })();
        record(&into, made);
    });

    let into = Arc::clone(&shared);
    engine.register_fn(
        "set-language-server",
        move |language: SteelVal, program: SteelVal, args: SteelVal| {
            let made = (|| {
                Ok(Declaration::LanguageServer {
                    // A symbol: a Language is looked up in `Languages`, and a
                    // name meowr does not know is a problem said out loud.
                    language: symbol(&language, "a Language")?,
                    server: meowr_command::LanguageServer {
                        // A string: a program name is not looked up in anything
                        // of meowr's, and `~/` is the only thing expanded.
                        program: text(&program, "a server program")?,
                        args: strings(&args, "a server argument")?,
                    },
                })
            })();
            record(&into, made);
        },
    );

    // `%set-formatter` takes the rest of the arguments as one list, and
    // `set-formatter` below collects them, so `(set-formatter 'rust #f)` and
    // `(set-formatter 'rust "rustfmt" '("--emit" "stdout"))` both read as
    // ADR-0014 writes them. Steel's registered functions have a fixed arity.
    let into = Arc::clone(&shared);
    engine.register_fn(
        "%set-formatter",
        move |language: SteelVal, program: SteelVal, rest: SteelVal| {
            let made = (|| {
                let language = symbol(&language, "a Language")?;
                let args = match &rest {
                    SteelVal::ListV(items) if items.is_empty() => SteelVal::ListV(vec![].into()),
                    SteelVal::ListV(items) if items.len() == 1 => items.iter().next().cloned().unwrap_or(SteelVal::Void),
                    _ => {
                        return Err(
                            "set-formatter takes a Language, a program or #f, and a list of arguments"
                                .to_string(),
                        )
                    }
                };
                // `#f` takes the Language's formatter away, as it unbinds a key.
                if matches!(program, SteelVal::BoolV(false)) {
                    return Ok(Declaration::Formatter {
                        language,
                        formatter: None,
                    });
                }
                Ok(Declaration::Formatter {
                    language,
                    formatter: Some(meowr_command::Formatter {
                        program: text(&program, "a formatter program")?,
                        args: strings(&args, "a formatter argument")?,
                    }),
                })
            })();
            record(&into, made);
        },
    );
    engine
        .compile_and_run_raw_program(
            "(define (set-formatter language program . arguments) \
               (%set-formatter language program arguments))",
        )
        .expect("meowr's own definition of set-formatter compiles");

    let into = Arc::clone(&shared);
    engine.register_fn(
        "add-hook",
        move |point_value: SteelVal, command: SteelVal| {
            let made = (|| {
                Ok(Declaration::Hook {
                    point: point(&point_value)?,
                    command: symbol(&command, "a command")?,
                })
            })();
            record(&into, made);
        },
    );

    let into = Arc::clone(&shared);
    engine.register_fn(
        "remove-hook",
        move |point_value: SteelVal, name: SteelVal| {
            let made = (|| {
                Ok(Declaration::Unhook {
                    point: point(&point_value)?,
                    // A hook's own name, which for one meowr ships is not a
                    // command anybody can call — `auto-exit-insert-on-save` is a
                    // closure. Still a symbol: it names something in a registry.
                    name: symbol(&name, "a hook name")?,
                })
            })();
            record(&into, made);
        },
    );

    shared
}
