#!/usr/bin/env bash
# Regenerate the fidelity goldens by running the real Meow package in Emacs.
#
# This is the only step that needs Emacs. The goldens it writes are checked in,
# so `cargo test` -- and CI -- never invoke Emacs at all.
#
#   tools/oracle/regenerate.sh            # every suite
#   tools/oracle/regenerate.sh motions    # one suite
#
# Then read the diff. A change here is a change in what Emacs does, and is the
# only evidence that the Meow bump you just made altered behaviour.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
meow="${MEOWR_MEOW_REPO:-$HOME/.config/emacs/.local/straight/repos/meow}"

if [[ ! -d "$meow" ]]; then
  echo "no Meow checkout at $meow; set MEOWR_MEOW_REPO" >&2
  exit 1
fi

echo "meow: $(git -C "$meow" rev-parse --short HEAD) $(git -C "$meow" log -1 --format=%cs)"
echo "emacs: $(emacs --version | head -1)"

for cases in "$root"/fidelity/${1:-*}.cases; do
  suite="$(basename "$cases" .cases)"
  emacs -Q --batch -L "$meow" -l "$root/tools/oracle/oracle.el" \
        -- "$cases" "$root/fidelity/$suite.golden"
done
