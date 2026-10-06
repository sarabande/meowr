# Rings: the reference tables meowr is measured against

Ring coverage is a claim about a *configured* editor — the author's Doom with Meow —
and it is counted, not proved. See `~/docs/meowr-ring-coverage.md` for the count and
the glossary for the word.

## Why a copy lives here

The tables themselves live in `~/docs`, with the rest of the domain docs. `~/docs` is
not a git repository and CI cannot reach it, so the one table a *test* reads is
checked in here — the same trade the fidelity goldens make, and for the same reason:

> The goldens are checked in; Emacs is needed only to regenerate them, which is a
> deliberate act by someone who is about to review the diff.

`cargo test` never needs Emacs, `~/docs`, or the author's config.

## Regenerating

    tools/keymaps/extract.sh                # the global map, into ~/docs
    cp ~/docs/meowr-doom-leader.tsv rings/  # after re-extracting the leader

Then read the diff. A change here is a change in the author's config, and is the only
evidence that a key meowr holds has moved out from under it.

## What reads it

`crates/meowr-app/tests/rings.rs` asserts that every command row resolves to exactly
one of four buckets: **bound**, **held** by a milestone, **dropped** with a reason, or
**open** with a ticket. A row in none of them fails the build and names the key.

The fourth bucket is the `DIVERGENCES` trick from the fidelity harness: an open row is
parked where it can be seen rather than hidden, the suite stays green while it waits,
and closing it breaks the build until its entry goes.
