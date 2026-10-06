use super::*;

#[test]
fn every_binding_is_reachable_from_its_prefix() {
    // A key whose prefix is not itself in the tree cannot be typed, and the
    // menu would never list it.
    let leader = Leader::new();
    for binding in leader.bindings() {
        if binding.keys.len() > 1 {
            let prefix = &binding.keys[..binding.keys.len() - 1];
            assert!(
                leader.lookup(prefix).is_some(),
                "{:?} has no prefix entry {:?}",
                binding.keys,
                prefix
            );
        }
    }
}

#[test]
fn every_prefix_has_something_under_it() {
    let leader = Leader::new();
    for binding in leader.bindings() {
        if let Entry::Prefix(name) = &binding.entry {
            assert!(
                !leader.below(&binding.keys).is_empty(),
                "the {name} prefix is empty"
            );
        }
    }
}

#[test]
fn no_key_is_bound_twice() {
    let mut seen: Vec<&[Key]> = Vec::new();
    let leader = Leader::new();
    for binding in leader.bindings() {
        assert!(
            !seen.contains(&binding.keys.as_slice()),
            "{:?} is bound twice",
            binding.keys
        );
        seen.push(&binding.keys);
    }
}

#[test]
fn every_reservation_says_when_it_is_coming() {
    // `Reserved` holds a key for something that does not exist yet, so the note
    // is the whole value of the entry: the menu shows it, and a note that does
    // not say *when* is indistinguishable from a key nobody got round to.
    //
    // Checked by prefix rather than by `contains`, which would pass on a note
    // that merely mentions a milestone somewhere in its prose. #46 renumbered
    // nine of these by hand and the one that was wrong -- `SPC b k`, reading
    // "M1 -- buffer list" after M1 shipped -- was found by reading, not by a
    // test. This is the test.
    // Two lists, and they must stay disjoint: a milestone moves from OPENINGS
    // to SHIPPED on the day it closes, and a renumbering that updated one and
    // not the other would either reject every note or accept a stale one.
    // OPENINGS lives beside `Entry` now, because `rings.rs` asks the same
    // question of the author's whole tree and a second copy would go stale.
    const OPENINGS: &[&str] = RESERVATION_OPENINGS;
    // M4 and M5 on the day M5 closed (#128): M4 had closed a milestone
    // earlier and stayed in OPENINGS, which is the renumbering slip this list
    // exists to catch, found by the milestone after it. M5 claimed nothing it
    // did not answer, so no note names either.
    // M6 on the day it closed (#154), after #169 bound its last two rows.
    // M3 after it, though it was charted first: its map (#69) closed once
    // #170 measured the last of its holes, with #68's "the author drives it
    // daily" carried on its own because nobody can close that for him.
    // M7 joins the list on the day its map (#177) closes, as every milestone
    // before it did -- and joins it having never been *in* OPENINGS: every row
    // it took said `unscheduled`, because M7 was charted out of what was left
    // over rather than promised in advance. The guard is still worth having:
    // a note naming a milestone that has shipped is the slip this catches.
    const SHIPPED: &[&str] = &["M1", "M2", "M3", "M4", "M5", "M6", "M7"];
    for shipped in SHIPPED {
        assert!(
            !OPENINGS.iter().any(|o| o.starts_with(shipped)),
            "{shipped} is both shipped and still coming"
        );
    }
    let mut reserved = 0;
    let leader = Leader::new();
    for binding in leader.bindings() {
        let Entry::Reserved(note) = &binding.entry else {
            continue;
        };
        reserved += 1;
        assert!(
            OPENINGS.iter().any(|opening| note.starts_with(opening)),
            "{:?}: {note:?} starts with none of {OPENINGS:?}",
            binding.keys
        );
        for shipped in SHIPPED {
            assert!(
                !note.contains(shipped),
                "{:?}: {note:?} — {shipped} has shipped, so nothing waits for it",
                binding.keys
            );
        }
    }
    // Thirty-eight, and the number is finished climbing for now: since #104
    // there is no row of the author's leader left undecided, which `rings.rs`
    // asserts outright.
    //
    // It went down four times. `SPC b k` stopped being a reservation in #52
    // and `SPC t` in #67, each when the thing holding its key got built; `SPC
    // v` and `SPC o` went together when Guests were dropped; and `SPC b D`,
    // `b o`, `b x` and `b X` went in #68, which is the ordinary way and the
    // one worth reading. Those four said `unscheduled` and nothing was ever in
    // the way of them — the window tree, `kill-buffer` and `Buffer::scratch`
    // were each built for something else — so what they were waiting for was
    // somebody with a reason to reach for them, and running the config that
    // names all four is that reason.
    //
    // It went up four times, all of them the Ring coverage inventory walking
    // Doom's tree prefix by prefix: one in #77, seven in #97 (search), ten in
    // #98 (file), and nineteen across #99 to #102 (quit, toggle, buffer,
    // window).
    //
    // The climb is what an inventory looks like from here, and it is not a
    // scope increase. Those thirty-seven keys were *open* before, which is
    // worse than reserved: an open key is one nobody has decided about, and it
    // looks exactly like a key somebody decided to drop. Six of them still say
    // `unscheduled`, which is the honest note for a thing meowr would want and
    // nobody has planned.
    //
    // The third downward move is the unusual one, and worth reading twice: a
    // reservation is meant to end by being *bound*, and those two ended by
    // being dropped. Nothing is coming for them, so holding the keys would
    // have been a promise with nobody behind it.
    //
    // Which direction it moves is the point rather than the number. Down means
    // something shipped; up means a key was deliberately held. A change with
    // neither story behind it is a key somebody forgot, and #74 renumbered six
    // of these to M4 without touching the count, which is the other thing this
    // number catches.
    // 38 before #118, and *up* to 56 — which looks like the wrong direction
    // and is not. `SPC k` was one blanket reservation standing for all twenty
    // rows of Doom's `code` prefix. #118 bound `k k` to
    // `lookup-documentation`, which turns `k` into the prefix Doom calls it and
    // leaves the other nineteen rows needing entries of their own: a row under
    // a *bound* prefix with no entry is a key that silently does nothing,
    // where a row under a reserved one was covered by the reservation.
    //
    // So one promise became nineteen, each saying what it is waiting for — the
    // count going up is the reservation getting more honest, not less.
    // 56 until #137 bound `k D` to references. Down is the direction a
    // reservation count is supposed to move: a promise kept stops being one.
    //
    // Only one, because `k j` and `k J` were built and handed back: an empty
    // `workspace/symbol` query returns a truncated, dependency-dominated list
    // from rust-analyzer, and a symbol picker missing this workspace's own
    // symbols is the kind of row that looks like it works.
    // 55 until #136 bound `k x` to `list-diagnostics`, which is ADR-0011's one
    // row that goes to a generated buffer rather than to the picker.
    // 54 until #139 bound the four rows that search files -- `s p`, `s d`,
    // `s D` and `s .` -- over #138's searcher. Four promises kept at once,
    // which is the direction the count is supposed to move.
    // 46 until #144 built the picker that asks its source again as you type,
    // which is what `SPC k j` and `k J` were handed back for: a list fixed when
    // the picker opened cannot hold a `workspace/symbol` answer, because the
    // server is the matcher and it truncates.
    // 47 until #142 made `SPC C-f` the fold prefix it is in Doom, which is one
    // reservation ending and no new ones: four of its rows are bound and two
    // are dropped rather than held, because nothing creates a vimish fold to
    // delete.
    // 50 until #140 bound the three that search what is already open: `s S`,
    // `s B` and `s I`. One of the three was reserved as *"LSP: symbols across
    // buffers"* and is not an LSP row at all -- `consult-imenu-multi` reads
    // imenu, which meowr's own parse already answers. A reservation can be
    // wrong about what it is holding the key for, which is a thing only the
    // slice that spends it finds out.
    // 44 until #146 bound `k d`, `k t` and `k i` -- three single-place
    // lookups the map's Destination named and no slice had been cut for. `k l`
    // moved to `unscheduled` in the same pass, which changes when it is
    // answered and not the count.
    // 41 until #147, and *up* to 42 -- #118's shape again, one prefix down.
    // `SPC SPC` was bound and `SPC p` stopped being one blanket reservation and
    // became the prefix Doom calls it: two of its five rows are routes to the
    // project searches, and the other three are held one at a time, because a
    // row under a bound prefix with no entry is a key that silently does
    // nothing.
    // 42 until #149 bound `f F` to `find-file-under-here`, the row #128
    // found was never a project row at all.
    // 41 until #148 bound `f R` and `f Y`, the last two rows reading `M5 — `.
    // Every reservation left says `unscheduled`: M5 claimed nothing it did not
    // answer.
    // 39 still after #159, which moved `k a` `k r` `k o` and `k f` from
    // `unscheduled` to M6 (#154): a row changing *when* it is answered is not
    // a row gained or lost, which is the count #74's renumbering left alone
    // for the same reason.
    // 39 until #164 bound `k f` to `format-buffer`.
    // 38 until #168 bound `k r` to `lsp-rename`.
    // 37 until #169 bound `k a` and `k o`, the last `M6 — ` rows.
    // 35 until #178 bound `f C` and `f m`, M7's first two.
    // 33 until #179 bound `f D`, the third.
    // 32 until #176 bound `q F`.
    // 31 until #180 bound `f x`, `f X`, `p x` and `p X`.
    // 27 until #181 bound `f P` and `f p`, the last two `f` rows M7 held.
    // 25 until #182 bound `q r`, the last row on M7's map.
    assert_eq!(reserved, 24, "every reservation should be checked");
}

// `the_keys_of_out_of_scope_packages_are_reserved_rather_than_reassigned` stood
// here and is gone with #142. Its table was hand-maintained and is empty now:
// `SPC t` was discharged by #67, `SPC k` by #118 and `SPC C-f` by #142, each of
// them bound, which is the only way a reservation is meant to end — and `v` and
// `o` left it the other way, dropped with ADR-0003's amendment because nothing
// was coming for them. A loop over an empty table asserts nothing, and what it
// was guarding is guarded better by
// `every_reservation_says_when_it_is_coming` above, which counts *every*
// reservation left and makes each one say when.

#[test]
fn a_chord_can_be_a_leader_key_and_resolves_like_any_other() {
    let leader = Leader::new();
    // The whole of #42: `C-f` is a `Key` and not a `char`, so the tree can
    // hold it, `lookup` finds it, and the menu writes it the way a keymap
    // reference does. *Which* kind of entry it holds is not this test's
    // business -- it was `Reserved` until #142 made it the fold prefix, and
    // asserting on that was this test borrowing a fact it does not own.
    assert!(
        leader.lookup(&[Key::Ctrl('f')]).is_some(),
        "a chord key is in the tree"
    );
    assert!(
        matches!(
            leader.lookup(&[Key::Ctrl('f'), Key::Ctrl('f')]),
            Some(Entry::Command(_))
        ),
        "and so is a sequence of two of them"
    );
    assert!(
        leader.below(&[]).iter().any(|(k, _)| **k == Key::Ctrl('f')),
        "the root menu lists it"
    );
    assert_eq!(format!("{}", Key::Ctrl('f')), "C-f");
}

#[test]
fn the_grab_commands_have_leader_homes_on_a_key_doom_does_not_use() {
    let leader = Leader::new();
    // Decision #12 governs a key Meow itself never bound: `g` appears in no
    // Doom leader binding, so nothing has to be taken back later.
    assert_eq!(
        leader.lookup(&[Key::Char('g')]),
        Some(&Entry::Prefix("grab".into()))
    );
    assert_eq!(
        leader.suffix_for("swap-grab"),
        Some(vec![Key::Char('g'), Key::Char('s')])
    );
    assert_eq!(
        leader.suffix_for("sync-grab"),
        Some(vec![Key::Char('g'), Key::Char('y')])
    );
}

#[test]
fn a_reserved_key_is_not_also_a_prefix() {
    // "Held" and "has things under it" are different states, and a key that
    // was both would show a menu of nothing.
    let leader = Leader::new();
    for binding in leader.bindings() {
        if matches!(binding.entry, Entry::Reserved(_)) {
            assert!(
                leader.below(&binding.keys).is_empty(),
                "{:?} is reserved but has children",
                binding.keys
            );
        }
    }
}

#[test]
fn the_menu_lists_only_the_level_below_the_prefix() {
    let leader = Leader::new();
    let root = leader.below(&[]);
    assert!(root.iter().any(|(k, _)| **k == Key::Char('f')));

    // Sixteen since #98 decided Doom's file prefix: five bound before it,
    // `f y` bound by it, and ten held — three for the project milestone and
    // seven `unscheduled`, which is a note about what is not planned rather
    // than a milestone invented to make a row look decided.
    let files = leader.below(&[Key::Char('f')]);
    assert_eq!(files.len(), 16, "five bound, one added, ten held");
}

// --- the tree can change (#57) ---------------------------------------------

#[test]
fn a_binding_added_at_runtime_resolves_and_is_listed() {
    // The whole point of #57: until this, Ring 2 was a `const`, so a config
    // could not add `SPC d s` no matter what the seam offered.
    let mut leader = Leader::new();
    assert!(
        leader.lookup(&[Key::Char('d')]).is_none(),
        "`d` is free in the default tree"
    );

    leader
        .set(vec![Key::Char('d')], Entry::Prefix("dape".into()))
        .expect("`d` is free");
    leader
        .set(
            vec![Key::Char('d'), Key::Char('s')],
            Entry::Command("dape-start".into()),
        )
        .expect("under a prefix that now exists");

    assert!(matches!(
        leader.lookup(&[Key::Char('d'), Key::Char('s')]),
        Some(Entry::Command(name)) if name == "dape-start"
    ));
    assert!(leader.is_prefix(&[Key::Char('d')]), "and it is a prefix");
    assert!(
        leader
            .below(&[Key::Char('d')])
            .iter()
            .any(|(key, _)| **key == Key::Char('s')),
        "the menu under `SPC d` lists it"
    );
    assert_eq!(
        leader.suffix_for("dape-start"),
        Some(vec![Key::Char('d'), Key::Char('s')])
    );
}

#[test]
fn setting_a_key_that_is_taken_replaces_what_was_there() {
    // Including a key the default table holds: a config may rebind anything
    // meowr ships, reservations included. Decision #12 is a promise about the
    // keys meowr *ships*, not a lock on the user's own config.
    // A row rather than a top-level key: #118 discharged `SPC k`, #142
    // discharged `SPC C-f`, and there are no top-level reservations left to
    // borrow. `SPC k l` is reserved, and this test is about a *reservation*
    // being bound over, whatever depth it sits at. It borrowed `SPC k a` until
    // #169 bound it and `SPC f C` until #178 did.
    let taken = vec![Key::Char('k'), Key::Char('l')];
    let mut leader = Leader::new();
    assert!(matches!(
        leader.lookup(&taken),
        Some(Entry::Reserved(_))
    ));

    leader
        .set(taken.clone(), Entry::Command("undo".into()))
        .expect("a reservation may be bound over");

    assert!(matches!(
        leader.lookup(&taken),
        Some(Entry::Command(name)) if name == "undo"
    ));
    assert_eq!(
        leader.bindings().iter().filter(|b| b.keys == taken).count(),
        1,
        "replaced, not shadowed by a second entry"
    );
}

#[test]
fn a_binding_can_be_taken_out_again() {
    let mut leader = Leader::new();
    assert!(leader.lookup(&[Key::Char('b'), Key::Char('b')]).is_some());

    assert!(leader.unset(&[Key::Char('b'), Key::Char('b')]));

    assert!(leader.lookup(&[Key::Char('b'), Key::Char('b')]).is_none());
    assert!(
        !leader.unset(&[Key::Char('b'), Key::Char('b')]),
        "only once"
    );
}

#[test]
fn the_seeded_tree_is_the_table_and_nothing_else() {
    // The conversion is the one place the table could lose an entry or change
    // what one means. Comparing lengths alone cannot fail — `Leader::new` is a
    // one-to-one map — so this walks the pairs and checks the kind survived
    // too: a `Prefix` arriving as a `Command` would resolve, list, and be
    // wrong.
    let leader = Leader::new();
    assert_eq!(leader.bindings().len(), SEED.len());
    for (seed, binding) in SEED.iter().zip(leader.bindings()) {
        assert_eq!(seed.keys, binding.keys.as_slice());
        match (seed.entry, &binding.entry) {
            (SeedEntry::Command(from), Entry::Command(to)) => assert_eq!(from, to),
            (SeedEntry::Prefix(from), Entry::Prefix(to)) => assert_eq!(from, to),
            (SeedEntry::Reserved(from), Entry::Reserved(to)) => assert_eq!(from, to),
            (from, to) => panic!("{:?}: {from:?} became {to:?}", binding.keys),
        }
    }
}

#[test]
fn the_config_rows_are_bound() {
    // #181, the last two `f` rows M7 held: `doom/open-private-config` and
    // `doom/find-file-in-private-config`, under names with no "private" in
    // them -- Doom's word for `~/.doom.d` against the one Emacs ships, and
    // meowr has only the one config.
    let leader = Leader::new();

    for (key, command) in [
        (Key::Char('P'), "open-config"),
        (Key::Char('p'), "find-file-in-config"),
    ] {
        match leader.lookup(&[Key::Char('f'), key]) {
            Some(Entry::Command(name)) => assert_eq!(name, command, "SPC f {key}"),
            other => panic!("SPC f {key} should run {command}, is {other:?}"),
        }
    }
}

#[test]
fn the_restart_row_is_bound() {
    // #182, the last row M7 held: `doom/restart`.
    let leader = Leader::new();

    match leader.lookup(&[Key::Char('q'), Key::Char('r')]) {
        Some(Entry::Command(name)) => assert_eq!(name, "restart"),
        other => panic!("SPC q r should run restart, is {other:?}"),
    }
}

#[test]
fn the_authors_own_top_level_leader_keys_are_bound() {
    // `SPC u` is stock Doom's `meow-universal-argument`, and meowr had the
    // command already on `C-u` — the gap was the binding, which is what Ring 2
    // exists to close. The author's `SPC .` and `SPC RET` are bound too, each
    // to its own command rather than to a second name for `find-file`; they
    // are checked by `spc_dot_opens_the_picker_showing_open_buffers_only` and
    // friends over in the session tests, where the picker they open can be
    // looked at.
    let leader = Leader::new();

    for (key, command) in [(Key::Char('u'), "universal-argument")] {
        match leader.lookup(&[key]) {
            Some(Entry::Command(name)) => assert_eq!(name, command, "SPC {key}"),
            other => panic!("SPC {key} should run {command}, is {other:?}"),
        }
    }
}
