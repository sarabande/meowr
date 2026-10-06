;;; oracle.el --- Record what real Meow does, for meowr to be compared against  -*- lexical-binding: t; -*-

;; Reads a `.cases' TSV, replays each case's keys through the real Meow package
;; in a real Emacs, and writes a `.golden' TSV of what happened.
;;
;; Run it through tools/oracle/regenerate.sh, which supplies the paths.
;;
;; Two things about this file are load-bearing and easy to undo by accident:
;;
;; 1. The buffer is put in the selected window before any key is replayed.
;;    `execute-kbd-macro' goes through `read-key-sequence', which re-syncs the
;;    current buffer to the selected window's buffer -- so inside
;;    `with-temp-buffer' the keys land in *scratch* instead. That does not
;;    error. It self-inserts into the wrong buffer and reports a plausible
;;    point, which is the worst thing an oracle can do.
;;
;; 2. Keys are replayed one at a time. `last-command' is an input to Emacs
;;    motion (`line-move' consults it for the goal column), so a single
;;    `execute-kbd-macro' over the whole sequence and a loop over single keys
;;    are not equivalent -- but the loop is what a person typing produces, and
;;    that is what Fidelity is defined against.

;;; Code:

(require 'meow)
(require 'subr-x)

;; Meow writes a cursor-shape escape straight to the terminal on every state
;; change, which in batch mode goes into the middle of this program's output.
(advice-add 'meow--update-cursor :override #'ignore)

(defvar oracle--pending nil
  "Keys a prompting command may read, as if they had been typed next.")

(defvar oracle--consumed 0
  "How many of them it read.")

;; `meow-inner-of-thing' and friends are
;; `(interactive (list (meow-thing-prompt ...)))', and `meow-thing-prompt'
;; calls `read-char' -- so `, r' is one command reading one argument, not two
;; commands. Inside `execute-kbd-macro' a `read-char' reads from the macro, and
;; a one-key macro has nothing left to give it, so it falls through to stdin
;; and the whole run hangs. Serving it from a queue the oracle controls keeps
;; the per-key replay -- and with it each key's own `condition-case' -- while
;; still letting a command take its argument.
(advice-add 'read-char :around
            (lambda (orig &rest args)
              (if oracle--pending
                  (progn (setq oracle--consumed (1+ oracle--consumed))
                         (pop oracle--pending))
                (apply orig args))))

(defun oracle--reads-a-character (command)
  "Whether COMMAND takes the next key as its own argument via a `c' spec.

`meow-find' and `meow-till' are `(interactive \"p\\ncFind:\")'. The `c'
code reads inside `callint.c' and never calls the Lisp `read-char'
advised above, so the queue that serves `meow-thing-prompt' cannot
serve these -- the read finds the one-key macro exhausted and falls
through to real input, hanging the run. Asked of the spec rather than
listed by name, because a list of commands that read a character is a
second source for a fact the command already states."
  (let ((spec (cadr (interactive-form command))))
    (and (stringp spec)
         (seq-some (lambda (line) (string-prefix-p "c" line))
                   (split-string spec "\n")))))

(defun oracle--sets-a-prefix (command)
  "Whether COMMAND leaves a prefix argument for the command after it.

Emacs's set, and there is no predicate for it -- `prefix-arg' is an
ordinary variable and any command may set it. These four are the ones
Meow's own keymap reaches, and a fifth would show up as a case whose
golden disagrees with a correct implementation rather than as silence."
  (memq command '(negative-argument
                  digit-argument
                  universal-argument
                  universal-argument-more)))

(defun oracle--repeats (command)
  "Whether COMMAND replays the one before it.

Ring 1 puts Emacs's own `repeat' on `\\='', not a Meow command, and it
refuses across `execute-kbd-macro' calls: measured, `last-repeatable-command'
reads as `meow-next-word' after a separate `w' and `\\='' still raises
\"There is nothing to repeat\", while the two keys in one call repeat the
word motion. So this key has to be in the same call as the one it
repeats -- which, unlike the other two, means reaching *backwards*."
  (eq command 'repeat))

(defun oracle--group (pending)
  "Split PENDING into (KEYS . REST): the keys that are one command's worth.

Usually one key. Two things make it more, and both are cases where
replaying key by key would be replaying something a person typing never
does:

A prefix argument does not survive `execute-kbd-macro', which clears
`prefix-arg' on entry -- measured, a spy command after `-' sees
`current-prefix-arg' nil and `p' of 1 where a person typing gives it
`-' and -1. So `- w' goes in one call, which is also what it is: one
command with an argument.

A character read by a `c' spec is read from the macro, so it has to be
in the macro."
  (let* ((key (car pending))
         (rest (cdr pending))
         (command (key-binding (vector key) t)))
    (oracle--absorb-repeats
     (cond
      ((null rest) (cons (list key) nil))
      ((oracle--sets-a-prefix command)
       (let ((beyond (oracle--group rest)))
         (cons (cons key (car beyond)) (cdr beyond))))
      ((oracle--reads-a-character command)
       (cons (list key (car rest)) (cdr rest)))
      (t (cons (list key) rest))))))

(defun oracle--absorb-repeats (group)
  "Pull any `repeat' keys following GROUP into it.

The one grouping that reaches backwards, and it has to: `repeat' replays
the command before it and cannot see one in an earlier call. `w \\=' \\='
absorbs both, because the second repeats the first."
  (let ((keys (car group))
        (rest (cdr group)))
    (while (and rest (oracle--repeats (key-binding (vector (car rest)) t)))
      (setq keys (append keys (list (car rest)))
            rest (cdr rest)))
    (cons keys rest)))

(defvar oracle-normal-keymap-tsv
  (or (getenv "MEOWR_MEOW_NORMAL_TSV")
      (expand-file-name "~/docs/meowr-meow-normal.tsv"))
  "The extracted Normal State keymap, the same table meowr's keymap was built from.

Read rather than duplicated: a second hand-maintained copy of the
bindings would be a place for the oracle and meowr to disagree for a
reason that has nothing to do with Meow's behaviour.")

;;; TSV

(defun oracle--unescape (s)
  (let ((out "") (i 0) (n (length s)))
    (while (< i n)
      (let ((c (aref s i)))
        (if (and (eq c ?\\) (< (1+ i) n))
            (let ((d (aref s (1+ i))))
              (setq out (concat out (pcase d (?n "\n") (?t "\t") (?r "\r")
                                         (?\\ "\\") (_ (error "Unknown escape \\%c" d))))
                    i (+ i 2)))
          (setq out (concat out (char-to-string c)) i (1+ i)))))
    out))

(defun oracle--escape (s)
  (thread-last s
               (replace-regexp-in-string "\\\\" "\\\\\\\\")
               (replace-regexp-in-string "\n" "\\\\n")
               (replace-regexp-in-string "\t" "\\\\t")
               (replace-regexp-in-string "\r" "\\\\r")))

(defun oracle--opt (s) (unless (equal s "-") s))

(defun oracle--rows (path)
  "Data rows of the TSV at PATH, each a list of fields."
  (with-temp-buffer
    (insert-file-contents path)
    (cl-loop for line in (split-string (buffer-string) "\n" t)
             unless (or (string-prefix-p "#" line) (string-empty-p (string-trim line)))
             collect (split-string line "\t"))))

;;; Keymap

(defun oracle--define-normal-keys ()
  "Bind Normal State from `oracle-normal-keymap-tsv'.

Only single-character keys: everything else in that table is a prefix,
a remap or a named key, and none of those are in a case file yet."
  (dolist (row (oracle--rows oracle-normal-keymap-tsv))
    (let ((key (nth 0 row))
          (command (nth 1 row)))
      (when (and (= (length key) 1)
                 command
                 (not (equal command "undefined"))
                 (not (equal command "PREFIX"))
                 (fboundp (intern command)))
        (define-key meow-normal-state-keymap (kbd key) (intern command))))))

;;; Observation

(defun oracle--byte (pos)
  "POS, an Emacs position, as a 0-based byte offset."
  (1- (position-bytes pos)))

(defun oracle--position (offset)
  "OFFSET, a 0-based byte offset, as an Emacs position."
  (or (byte-to-position (1+ offset))
      (error "Byte offset %d is not a position in this buffer" offset)))

(defun oracle--observe (name signals)
  "The observation tuple for the current buffer, as a golden row.

SIGNALS is how many of the replayed keys raised an error. It is recorded
because a key that fails is indistinguishable, in the buffer state alone,
from a key that ran and did nothing -- and the difference matters. A case
whose keys all failed would otherwise read as a case where Meow chose to
do nothing, which is how a wrong golden gets believed."
  (let* ((type (meow--selection-type))
         (active (region-active-p)))
    (string-join
     (list name
           (oracle--escape (buffer-string))
           (number-to-string (oracle--byte (point)))
           (if active (number-to-string (oracle--byte (mark))) "-")
           (if type (symbol-name (car type)) "-")
           (if type (symbol-name (cdr type)) "-")
           (number-to-string (length meow--selection-history))
           (number-to-string signals)
           (symbol-name (meow--current-state))
           (oracle--grab))
     "\t")))

(defun oracle--grab ()
  "The Grab, as `START-END', or `-' when there is none here.

Meow's Grab *is* Emacs's secondary selection: `meow-grab' calls
`secondary-selection-from-region', and everything that reads it goes through
`mouse-secondary-overlay'. It lives in an overlay rather than in point and
mark, so none of the other columns can see it -- which is why a Grab suite
without this column would record agreement it had not checked."
  (if (and (overlayp mouse-secondary-overlay)
           (eq (overlay-buffer mouse-secondary-overlay) (current-buffer)))
      (format "%d-%d"
              (oracle--byte (overlay-start mouse-secondary-overlay))
              (oracle--byte (overlay-end mouse-secondary-overlay)))
    "-"))

;;; Running

(defvar oracle--signals 0
  "How many keys signalled while replaying the current case.")

(defun oracle--run-case (row)
  "Run one case ROW and return its golden row."
  (pcase-let ((`(,name ,buffer ,point ,axis ,kind ,mark ,keys ,_diverges) row))
    (let ((buf (get-buffer-create (format "*oracle:%s*" name))))
      (unwind-protect
          (progn
            (set-window-buffer (selected-window) buf)
            (set-buffer buf)
            (fundamental-mode)
            (transient-mark-mode 1)
            (erase-buffer)
            ;; Every one of these is global rather than buffer-local, so a
            ;; fresh buffer is not a fresh case. `p' on an empty kill ring
            ;; yanked the previous case's kill and looked entirely plausible
            ;; doing it, which is how contamination gets into a golden.
            (setq meow--selection nil
                  meow--selection-history nil
                  meow--expand-nav-function nil
                  kill-ring nil
                  kill-ring-yank-pointer nil
                  prefix-arg nil
                  last-command nil
                  this-command nil
                  oracle--signals 0)
            (insert (oracle--unescape buffer))
            (goto-char (oracle--position (string-to-number point)))
            (meow-mode 1)
            (meow--switch-state 'normal)
            (when (oracle--opt axis)
              ;; ...-without-history, so a case starts at history depth 0 and
              ;; the depth in the golden counts only what the keys pushed.
              (meow--select-without-history
               (list (cons (intern axis) (intern kind))
                     (oracle--position (string-to-number mark))
                     (point))))
            (setq last-repeatable-command nil)
            (let ((pending (string-to-list (oracle--unescape keys))))
              (while pending
                (let* ((group (oracle--group pending))
                       (keys (car group))
                       (rest (cdr group)))
                  (setq oracle--pending rest
                        oracle--consumed 0)
                  (condition-case nil
                      (execute-kbd-macro (vconcat keys))
                    (error (setq oracle--signals (1+ oracle--signals)))
                    (quit (setq oracle--signals (1+ oracle--signals))))
                  ;; Skip whatever the command read as its own argument.
                  (setq pending (nthcdr oracle--consumed rest)
                        oracle--pending nil))))
            (oracle--observe name oracle--signals))
        (kill-buffer buf)))))

(defun oracle-main ()
  ;; `--' survives into `command-line-args-left' in batch mode; the paths are
  ;; whatever follows it.
  (let* ((argv (if (equal (car command-line-args-left) "--")
                   (cdr command-line-args-left)
                 command-line-args-left))
         (cases (nth 0 argv))
         (golden (nth 1 argv)))
    (unless (and cases golden)
      (error "Usage: emacs --batch -l oracle.el -- CASES GOLDEN"))
    (oracle--define-normal-keys)
    (let ((rows (mapcar #'oracle--run-case (oracle--rows cases))))
      (with-temp-file golden
        (insert "# name\ttext\tpoint\tmark\taxis\tkind\thistory\tsignals\tstate\tgrab\n")
        ;; Sorted, so the diff after a Meow bump is about Meow and not about
        ;; the order someone happened to add cases in.
        (insert (string-join (sort rows #'string<) "\n") "\n"))
      (message "oracle: %d cases -> %s" (length rows) golden))))

(oracle-main)

;;; oracle.el ends here
