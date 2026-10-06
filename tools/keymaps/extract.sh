#!/usr/bin/env bash
# Extract the global map Meow's Normal State does not shadow (#92).
#
# The five tables in `~/docs/meowr-meow-*.tsv` and `meowr-doom-leader.tsv` were
# one-offs. This is the repeatable one, and it is repeatable because the config
# moves: the reference for Ring coverage is the live Doom, not a snapshot of it.
#
#   tools/keymaps/extract.sh              # writes ~/docs/meowr-doom-global.tsv
#   tools/keymaps/extract.sh /tmp/out.tsv
#
# Needs Emacs and the author's Doom. It starts a private daemon rather than
# loading Doom in batch: `--batch` runs Doom's CLI path, which loads the core
# and never initialises the modules, so `meow-normal-state-keymap` is not
# there to ask. A daemon is the live config by construction.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
dump="$root/tools/keymaps/dump.el"
out="${1:-$HOME/docs/meowr-doom-global.tsv}"
work="$(mktemp -d)"
server="meowr-extract-$$"
trap 'emacsclient -s "$server" --eval "(kill-emacs)" >/dev/null 2>&1 || true; rm -rf "$work"' EXIT

echo "emacs: $(emacs --version | head -1)" >&2

# Vanilla, for provenance: a binding that is here and identical is Emacs's own,
# and one that is not is something Doom brought.
emacs -Q --batch -l "$dump" --eval '(meowr-dump (current-global-map))' > "$work/vanilla.tsv" 2>/dev/null

emacs --daemon="$server" >/dev/null 2>&1
emacsclient -s "$server" --eval "(progn
  (load \"$dump\" nil t)
  (with-temp-file \"$work/doom.tsv\"
    (let ((standard-output (current-buffer))) (meowr-dump (current-global-map))))
  (with-temp-file \"$work/shadow.txt\"
    (let ((standard-output (current-buffer))) (meowr-dump-shadowed)))
  (with-temp-file \"$work/versions.txt\"
    (insert (format \"emacs %s, doom %s\" emacs-version doom-version)))
  'ok)" >/dev/null

meow="${MEOWR_MEOW_REPO:-$HOME/.config/emacs/.local/straight/repos/meow}"
if [[ -d "$meow" ]]; then
  printf ', meow %s %s' "$(git -C "$meow" rev-parse --short HEAD)" \
                        "$(git -C "$meow" log -1 --format=%cs)" >> "$work/versions.txt"
fi

{
  echo "# meowr-doom-global"
  echo "#"
  echo "# The global map reachable from Meow's Normal State: everything Meow's"
  echo "# own keymap has not claimed falls straight through to it, because that"
  echo "# keymap does not suppress. This is the surface no other table covers,"
  echo "# and \`M-x\` is the proof it has daily traffic."
  echo "#"
  echo "# Regenerate with tools/keymaps/extract.sh. Do not hand-edit."
  printf '# extracted\t%s\n' "$(date -I)"
  printf '# versions\t%s\n' "$(cat "$work/versions.txt")"
  echo "#"
  echo "# source: emacs = Emacs's own, unchanged; doom-rebound = Doom took an"
  echo "# existing key; doom = Doom's own. Mouse and window-part pseudo-events,"
  echo "# <remap> entries, and the <f1>/<help> aliases of C-h are dropped."
  echo "#"
  printf '# key\tcommand\tsource\tdesc\n'
  emacs -Q --batch -l "$dump" --eval \
    "(meowr-global-table \"$work/doom.tsv\" \"$work/vanilla.tsv\" \"$work/shadow.txt\")" 2>/dev/null
} > "$out"

echo "$(grep -vc '^#' "$out") rows -> $out" >&2
