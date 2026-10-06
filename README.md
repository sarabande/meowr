# meowr

A modal text editor and extensible environment in Rust, driven by the exact
keybindings of the [Meow](https://github.com/meow-edit/meow) package for Emacs.

Not an Emacs. There is no Emacs Lisp runtime and no Emacs packages. What makes
this Emacs-class rather than another Helix is the extension system: native guest
crates compiled in, and scripted plugins in Steel.

## The idea

**Fidelity, not resemblance.** Meow's behaviour is not a keymap. A selection is
typed on two axes, commands branch on the *previous* selection's type, and a
history stack sits underneath. The reference is the elisp, and the test suite
proves agreement by running the real thing and comparing.

**Windows and the echo area, nothing floats.** The leader menu is a bottom side
window, the picker grows upward from the echo area, and completion is the same
widget as the picker. No centred boxes.

**A host, not just an editor.** Buffer, window and keymap infrastructure that
Meow drives and that applications plug into — `wagit` for Git, `masys` for
system ops.

## Status

**M1 is built.** Ring 1 is complete at Fidelity and the editor is usable daily:
five States (Normal, Insert, Motion, Keypad, Beacon), the selection calculus and
its history, the Thing registry, the Grab, undo, the window tree, the doom-style
modeline, the narrowing widget behind both the picker and completion, the Ring 2
leader tree, Ring 3 bindings and hooks, and auto-save. The nine languages ADR-0007
commits to — Rust, Python, JSON, YAML, Markdown, Bash, TOML, Nix and Scheme —
are linked into the binary as ten grammars, Markdown being a block parser and
an inline one. The list is what the author opens, counted against
the filesystem rather than copied from a config file. Their highlight and injection
queries come from the grammar crates themselves, which `include_str!` their
own, except where a crate's query was written against a different runtime.
The textobject queries, which no grammar crate ships, are vendored from
Helix.

Agreement with Emacs is proved rather than claimed. **Over two thousand
differential cases** replay the same keys through a real Meow and compare all
of it: the buffer, the point, the mark, both axes of the selection's type, the
depth of the selection history, the State, the Grab, and whether Emacs
signalled. They live in `fidelity/`, which is also where the count is — a
number repeated here would be a second source for a fact that moves every time
a case is added. `fidelity/README.md` explains the harness. Two dozen cases are
recorded as *deliberate* divergences, each carrying its reason, and the suite
fails if one of them starts **matching** — an annotation that has quietly
healed is a lie. Several hundred ordinary tests sit on top.

    cargo test --workspace       # everything, differential suites included
    cargo run -p meowr -- FILE

**Two programs are expected on the PATH, and neither is bundled.** `git`,
because ADR-0010 makes the Project a git repository rather than a directory
with a marker file in it — so `SPC SPC`, `SPC p` and project search all ask it
where they are. And `ripgrep`, which the four `SPC s` rows that search files
shell out to; the reference configuration requires it for the same rows, Doom's
`SPC s p` being `consult-ripgrep`. A row whose program is missing says so and
does nothing, rather than falling back to something that looks like it worked.

**M2 is the scripted seam**: Steel embedded, a config file, and the four things
ADR-0003 gives the scripted seam — configuration, keymaps, hooks and small
commands. It is done when the author's own `init.scm` drives the editor daily.
A Steel command composes named commands and nothing else, which is Meow's own
trick: the selection spec's central finding is that Meow delegates all 44
editing operations to Emacs key sequences rather than touching buffers itself.
**M3 was Ring 2 and livability** — the IDE layer moved out of it — and closed
with the author's leader tree matched and the holes it found measured by checked-in
scenarios; whether `init.scm` drives the editor daily is the author's to say. **M4 was
LSP**, and shipped diagnostics and a hover; **M5 was the rows M4 reserved and did
not build** — lookups, the project, project search, folding and recent files — and
shipped every one. **M6 was text meowr did not type**: formatting with apheleia's
programs and on save, `save-some-buffers`, and a language server's rename and code
actions, applied across files all or nothing. It shipped every row it claimed,
leaving no leader key reserved for a milestone. **M7 was the editor's own
housekeeping**: a file copied, moved or deleted from inside meowr, with
everything that pointed at it either following or saying it cannot; the config
and the scratch buffers one key away; every buffer killed at once; and meowr
restarting itself. It is where meowr first touched a user's file for a reason
other than writing it, and where a buffer first visited a file that was not
there. The Guests are gone with ADR-0003's amendment.
The design decisions live in the issue tracker at `git.lan`, where the
`wayfinder:map` issue holds the route, and thirteen ADRs are in force and are not
relitigated in a ticket.
