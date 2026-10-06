# Fidelity: meowr against real Meow

Fidelity is the premise of meowr, and it is not defensible by reading elisp
carefully. `meow-next-thing` branches on whether the previous selection's kind
matches *and* on whether its axis is `expand`; `meow-left` cancels every
selection except `(expand . char)`; `meow-line`'s docstring says the newline is
excluded and it means it. These are exactly the things a hand port gets wrong
silently.

So nothing here asserts what Meow *ought* to do. Every expected value was
produced by running Emacs.

## The files

| | |
|---|---|
| `motions.cases`, `expand.cases`, `editing.cases` | inputs: a buffer, a point, an optional starting selection, a key sequence |
| `*.golden` | observations, one line per case, **written by Emacs** |
| `../tools/oracle/oracle.el` | the oracle: replays the keys through real Meow |
| `../tools/oracle/regenerate.sh` | runs it |
| `../crates/meowr-fidelity` | the notation, and the generator for the cross product |
| `../crates/meowr-app/tests/fidelity.rs` | replays the same keys through `Session` and compares |

Both files are tab-separated with `\n`, `\t`, `\r` and `\\` escaped, and `-` for
absent. Two files rather than one so that regeneration rewrites exactly one of
them: after a Meow bump, `git diff` shows what Emacs's behaviour changed and
nothing else.

## The observation tuple

Buffer text, point, mark, selection axis, selection kind, the depth of
`meow--selection-history`, **how many keys signalled**, and the **State**. Positions are
0-based **byte** offsets on both sides; Emacs positions are 1-based character
positions, so the oracle converts.

The signal count is not decoration. Emacs's command loop swallows a signal — it
beeps and moves on — so a case whose keys all *failed* leaves a buffer
indistinguishable from a case where Meow ran and chose to do nothing. Adding
this column is what turned "the prefix argument seems to be ignored" into "the
digit key raised an error and never ran", which was a golden that had looked
entirely reasonable.

The history depth is in there because it is observable: `z` is bound to
`meow-pop-selection`. The kill ring and `last-command` are not, because no
command in this milestone reads them. Adding a field invalidates every golden,
so it is a deliberate act.

## Running

    cargo test -p meowr-app --test fidelity     # ~1.5 s for 2,032 cases

**CI does not need Emacs.** The goldens are checked in; Emacs is needed only to
regenerate them, which is a deliberate act by someone who is about to review
the diff:

    tools/oracle/regenerate.sh            # ~450 ms, all suites
    tools/oracle/regenerate.sh motions    # one suite

It prints the Emacs version and the Meow commit it read, so a golden diff can
always be attributed.

To change the *inputs* — a new motion, a new starting selection, another
buffer — edit `crates/meowr-fidelity/src/bin/cases.rs` and

    cargo run -p meowr-fidelity --bin cases && tools/oracle/regenerate.sh

## What cannot be oracled: anything that reads `last-command`

Replaying one key per `execute-kbd-macro` runs the real command loop and the
real post-command hooks, which is why the rest of the harness is trustworthy.
But **`prefix-arg` and `last-command` do not survive between two invocations**,
so any behaviour that reads them is invisible to the oracle. Two things do:

- **Numeric arguments.** `- w` reaches `meow-next-word` with no argument and
  moves *forward*.
- **`yank-pop`.** Since Emacs 28, `yank-pop` falls through to
  `yank-from-kill-ring` — an interactive prompt — unless `last-command` is
  `yank`. In batch that prompt returns the empty string, so a golden for `p P`
  would record Meow pushing `""` onto the kill ring and moving nothing.
  Confirmed by tracing the ring: `("" "\nsecond" "hello")`.

The obvious fix makes it worse. `command-execute` with the argument passed
explicitly does carry it, but skips `post-command-hook`, which is where Meow
stashes the expand navigation pair — under it, `w 2` stops expanding at all.

So those cases are covered by ordinary tests written from the elisp, in
`meowr-core`, `meowr-command` and `meowr-app`. The case generator says so too,
so nobody closes the gap with cases that would quietly lie.

### What cannot be oracled: an `(interactive "c")` character

`meow-find` and `meow-till` are `(interactive "p\ncFind:")`. The `c` spec reads
its character **in C**, inside `Fcall_interactively`, below the elisp
`read-char` the oracle's queue serves — which is why the Thing commands work,
since `meow-thing-prompt` calls `read-char` explicitly, and why `f` and `t`
hang the run instead. Replaying the whole sequence as one macro would feed the
character, but at the cost of the per-key error isolation the rest of the
harness depends on.

`meow-search` and `meow-visit` are further out of reach again: both go through
the minibuffer with a completing-read, which a batch Emacs cannot answer at
all.

### What cannot be oracled: Beacon State

`meow--maybe-toggle-beacon-state` opens with

    (unless (or defining-kbd-macro executing-kbd-macro) ...)

and the oracle replays every key through `execute-kbd-macro`. Emacs therefore
never enters Beacon State under the oracle, whatever the keys are. A beacon
suite would record Meow sitting in Normal State and call that agreement.

meowr honours the same guard — `Session::replay` is `executing-kbd-macro`, and
`maybe_toggle_beacon` refuses under it exactly as it already refused while
recording. The harness drives meowr through `replay` for that reason: without
it the two sides disagree about the State on every case with a Grab in it, and
the disagreement is the harness's rather than Meow's. Beacon State is covered
by ordinary tests in `meowr-app`.

That covers the Beacon *remaps* too (#93), and the recording half of them could
not be oracled even if the guard were lifted: `meow-beacon-insert` and its two
friends are `kmacro-start-macro` plus an insertion, ended by leaving Insert, and
kmacro state no longer exists between two `execute-kbd-macro` calls — the same
reason `yank-pop` has a divergence rather than a golden. A suite that oracled
only `k` and `r`, the two that act at once, would cover the two commands that
need it least.

The **Grab** underneath it *is* oracled, in its own suite. Meow's Grab is
Emacs's secondary selection, which lives in an overlay rather than in point and
mark, so the goldens carry a `grab` column for it — without one the suite would
be recording agreement it never checked.

### What cannot be oracled: `repeat` and `meow-goto-line`

Ring 1 binds `'` to Emacs's own `repeat`, which reads
`last-repeatable-command` — so it lands squarely in the `last-command` problem
above. Replayed one key at a time it has nothing to repeat, and a golden for
`w '` would record it doing nothing and call that agreement.

`meow-goto-line` runs `goto-line`, which reads its line number from the
minibuffer. That is the `meow-search` case again: a batch Emacs has no answer
to give it. meowr takes the number from the numeric argument instead, so `2 L`
selects the second line and a bare `L` signals rather than inventing a
destination — a deliberate divergence, and the only one of the four keys this
slice binds that is not settled against Emacs.

`repeat` carries the argument, and that is a second thing the oracle cannot
see. Emacs's falls back to `last-prefix-arg`, so `3 w '` crosses three words
again rather than one; meowr keeps `last_argument` beside `last_command` and
restores it, preferring a fresh prefix if one was typed, as Emacs does. Per-key
replay loses the prefix on both sides, so the test for it is an ordinary one in
`meowr-app` written from the elisp.

`j` and `O` have no such problem and are in the `motions` suite in full, across
every selection type and every situation, including the `(expand . join)` start
that `j` itself creates.

### What is not oracled on purpose: the tree-sitter Things

`class`, `function`, `test`, `entry`, `parameter` and `comment` come from
`meow-tree-sitter` in the reference configuration, whose answers are produced by
*its* queries under Emacs's `treesit`; meowr's come from queries vendored from
Helix under tree-house. A golden would compare two query sets rather than two
implementations of Meow, and every difference would be a difference between the
query files. ADR-0006 already puts the boundary a query found outside what
Fidelity binds — and the *calculus* is covered, because `,` `.` `[` `]` build
their selections identically whichever registry answered.

### A related trap: a fresh buffer is not a fresh case

`kill-ring`, `meow--expand-nav-function`, `prefix-arg` and `last-command` are
global `defvar`s, not `defvar-local`. The oracle made a new buffer per case and
took that for isolation, so `p` on a supposedly empty kill ring yanked the
previous case's kill — and looked entirely reasonable doing it. `oracle--run-case`
now resets all of them explicitly.

Both failure modes below were verified rather than assumed:

- **`execute-kbd-macro`, one key at a time** — what the rest of the harness
  uses, because it runs the real command loop and the real post-command hooks.
  But the `prefix-arg` → `current-prefix-arg` handoff does not survive between
  two invocations, so `- w` reaches `meow-next-word` with no argument and moves
  *forward*.
- **`command-execute`, with the argument passed explicitly** — carries the
  argument, but skips `post-command-hook`, which is where Meow stashes the
  expand navigation pair. Under it, `w 2` stops expanding at all.

So a golden for `2 w` would record the behaviour of the harness rather than the
behaviour of Meow. Numeric arguments are covered by ordinary tests written from
the elisp instead, in `meowr-command` and `meowr-app`. The case generator says
this too, so nobody closes the gap by adding cases that would quietly lie.

## Recording a divergence

ADR-0005 guarantees meowr will not match Emacs everywhere. A case may carry a
reason in its `diverges` column, which **inverts** the assertion: meowr is then
required to differ, and the suite fails if it starts matching. A divergence
that quietly heals is an annotation that has become a lie.

Divergences live in `DIVERGENCES` in the case generator, not hand-edited into
the TSV, so they survive regeneration. There are forty:

- seventeen for a digit pressed with no selection. Meow raises "No selection"
  there, because `meow--selection-fallback` looks the fallback up by
  `this-command` — which is `meow-expand-2`, while
  `meow-selection-command-fallback`'s key is `meow-expand`. The entry pairing it
  with `meow-digit-argument` can never fire. meowr does what that entry plainly
  meant. Two of the seventeen are `o` followed by a digit in a buffer with no
  brackets: `o` finds no block in either editor, so what is left is the digit;
- six for the point drifting one character past a yank, when the kill came from
  a line selection. Traced rather than guessed: `yank` is entered at 13 and
  returns at 25, correctly, and something in Meow's post-command path then
  moves point to 26. Identical under whole-macro replay, so it is Meow's
  behaviour rather than a harness artifact — but the mechanism is unidentified,
  and reproducing an offset nobody can explain is the cargo-culting ADR-0002
  exists to prevent;
- **sixteen for the `match` Thing**, `m`, which is meowr's own and has no row
  in `meow-char-thing-table`. `meow--parse-inner-of-thing-char` is a `when-let*`
  over that table and its callers are a bare `(when bounds ...)`, so `, m` in
  Meow is a silent no-op — a replayable answer, which is why these are cases
  with a recorded difference rather than cases left unwritten. Four situations
  with a bracket around the point, times `,` `.` `[` `]`. The `string` and
  `prose` situations are absent because meowr finds nothing there either and
  the two agree;
- one for `P`, which cannot be replayed at all: since Emacs 28 `yank-pop` falls
  through to an interactive prompt unless `last-command` is `yank`, and
  per-key replay loses `last-command`.

Eight retired when `meow-block` learned to reach the block ahead of the
point, and three more when the mark stopped dying with the selection.

One difference is deliberate and **not** in that table, because the table
cannot hold it: an entry there asserts that a case *does* diverge, and this
one does not. `meow-block` skips brackets inside strings only — its loop tests
`meow--in-string-p` and nothing else — so a `)` in a comment stops Emacs's
scan and is refused by the `forward-list` check afterwards. meowr's scan skips
comments too (issue #34) and so never reaches that point. Both editors make no
selection, for two different reasons, and every golden agrees. It is written
down here because agreement by coincidence is the kind that stops holding
without anything failing.

The `stringpair` cases in the `things` suite are the opposite kind of
entry: not a divergence but a guard against one. Emacs has two bracket
matchers with two different rules — `meow-block` asks `syntax-ppss` and
skips strings, `meow--thing-parse-pair-search` is a bare regexp search and
does not — so `o` and `, r` disagree, and meowr has to disagree the same
way. Reconciling them would be a divergence dressed up as a fix, and these
cases are what would catch it.

## What this found

It was written after the Normal State slice had landed with 107 passing
hand-written tests. On its first run, **420 of 870 cases diverged**. Every one
was meowr's fault:

- the plain motions `h i e n` created a selection; Meow's only ever cancel one
- `E`/`N` reported kind `line`; Meow reports `char`
- `l` included the newline; `meow-line` excludes it
- `b` at the beginning of the buffer made a selection; a thing motion that
  cannot move makes none
- `g` left the selection history in place; `meow--cancel-selection` clears it
- `bounds-of-thing-at-point` also answers for the thing *ending* at the point,
  so `w m` marks a word where meowr did nothing
- a thing motion turns the selection round *before* it tries to move, so `m b`
  at the beginning of the buffer still swaps the ends

Four hand-written unit tests asserted the wrong behaviour and had to be
rewritten. That is the argument for this harness in one line.
