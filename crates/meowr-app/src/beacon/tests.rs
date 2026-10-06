use super::*;

/// What meowr does about one of Meow's `meow-beacon-noop` remaps.
enum Noop {
    /// meowr has a command of this name, and [`BEACON_NOOPS`] lists it.
    Listed(&'static str),
    /// The same *function* as another row, under a second name. A remap is
    /// keyed on the symbol, so Meow has to spell both; meowr has one command
    /// for one operation and one entry covers them.
    SameFunctionAs(&'static str),
    /// Nothing here to remap, with the reason. Listing it would be a claim
    /// about a key that cannot reach the command.
    NothingToRemap(&'static str),
}

/// Meow's ten `meow-beacon-noop` remaps, and what each is here.
///
/// Copied from `meow-keymap.el:107-116` in its own order rather than derived
/// from anything: it is the specification (ADR-0002), and a table derived from
/// meowr could only ever agree with meowr.
const MEOW_BEACON_NOOPS: &[(&str, Noop)] = &[
    ("meow-delete", Noop::Listed("delete")),
    // `(defalias 'meow-delete 'meow-C-d)` -- meow-command.el:1884. Literally
    // one function with two names, which is why Meow spends two remaps on it.
    ("meow-C-d", Noop::SameFunctionAs("meow-delete")),
    ("meow-C-k", Noop::Listed("kill-line")),
    ("meow-save", Noop::Listed("save")),
    (
        "meow-insert-exit",
        Noop::NothingToRemap(
            "`leave-insert`, bound only in Insert State -- and Beacon State \
             inherits the Normal keymap, so no key in it reaches the command",
        ),
    ),
    (
        "meow-last-buffer",
        Noop::NothingToRemap("meowr has no command for it at all"),
    ),
    ("meow-open-below", Noop::Listed("open-below")),
    ("meow-open-above", Noop::Listed("open-above")),
    ("meow-swap-grab", Noop::Listed("swap-grab")),
    ("meow-sync-grab", Noop::Listed("sync-grab")),
];

#[test]
fn every_noop_meow_remaps_is_accounted_for() {
    // The check the docstring on `BEACON_NOOPS` used to be, in prose, and got
    // wrong: it said two of Meow's ten were deliberately absent and named two,
    // while three have no entry. The third is `meow-C-d`, and the reason is
    // worth having written down rather than recounted every time somebody
    // notices seven against ten.
    assert_eq!(
        MEOW_BEACON_NOOPS.len(),
        10,
        "Meow remaps ten commands to the noop"
    );

    // Every absence carries its reason, and the reason is read -- clippy was
    // right that an unread field is a comment wearing a type.
    for (meow, what) in MEOW_BEACON_NOOPS {
        if let Noop::NothingToRemap(why) = what {
            assert!(!why.is_empty(), "`{meow}` is absent and does not say why");
        }
    }

    // The arithmetic itself, which is what went wrong in prose: seven listed,
    // one a second name for a row already here, and two with nothing to remap.
    // Counted rather than tallied, so the assertion reads as the sum it is
    // making a claim about.
    let counted = |of: fn(&Noop) -> bool| {
        MEOW_BEACON_NOOPS
            .iter()
            .filter(|(_, what)| of(what))
            .count()
    };
    assert_eq!(
        (
            counted(|what| matches!(what, Noop::Listed(_))),
            counted(|what| matches!(what, Noop::SameFunctionAs(_))),
            counted(|what| matches!(what, Noop::NothingToRemap(_))),
        ),
        (7, 1, 2),
        "seven of Meow's ten are listed here, and the other three each have a reason"
    );

    let listed: Vec<&str> = MEOW_BEACON_NOOPS
        .iter()
        .filter_map(|(_, what)| match what {
            Noop::Listed(name) => Some(*name),
            _ => None,
        })
        .collect();

    for name in &listed {
        assert!(
            BEACON_NOOPS.contains(name),
            "`{name}` is claimed here and missing from BEACON_NOOPS"
        );
    }
    for name in BEACON_NOOPS {
        assert!(
            listed.contains(name),
            "`{name}` is no-opped in Beacon State and this table does not say which \
             of Meow's remaps it answers"
        );
    }
}

#[test]
fn every_command_the_table_names_is_one_meowr_has() {
    // A typo here would otherwise read as a covered row for ever: `BEACON_NOOPS`
    // is matched against a command *name*, and a name nothing answers to is a
    // no-op that never fires rather than an error.
    let registry = crate::default_registry();
    for name in BEACON_NOOPS {
        assert!(
            registry.get(name).is_some(),
            "`{name}` is no-opped in Beacon State and is not a command"
        );
    }
}

#[test]
fn a_second_name_for_one_function_is_not_a_second_entry() {
    // The row that makes the arithmetic come out, and the one a reader would
    // otherwise file as a missing no-op. Meow remaps both names because a
    // remap is keyed on the symbol and an alias is a different symbol.
    let same: Vec<&str> = MEOW_BEACON_NOOPS
        .iter()
        .filter_map(|(meow, what)| match what {
            Noop::SameFunctionAs(other) => {
                assert!(
                    MEOW_BEACON_NOOPS.iter().any(|(name, _)| name == other),
                    "`{meow}` is said to be `{other}`, which is not in this table"
                );
                Some(*meow)
            }
            _ => None,
        })
        .collect();

    assert_eq!(same, vec!["meow-C-d"]);
}
