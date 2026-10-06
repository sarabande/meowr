;;; dump.el --- Dump a keymap as TSV, for the Ring coverage tables  -*- lexical-binding: t; -*-

;; The five tables in `~/docs' were one-offs. This is the repeatable version,
;; and it exists because #92 asked for a sixth: the global map that Meow's
;; Normal State does not shadow, which is where `M-x' lives and which no
;; existing table covers.
;;
;; Run it through tools/keymaps/extract.sh, which supplies the Emacs.

;;; Code:

(require 'subr-x)

(defvar meowr-dump-depth 2
  "How many keys deep to walk a prefix map.

Two is `C-x C-f'. Deeper is real -- `C-x r j' exists -- but the question
here is which *surfaces* are reachable, and a third level answers it no
better while multiplying the rows past reading.")

(defun meowr-dump--keymap (def)
  "DEF as a keymap, if it is one however it is spelled.

Three spellings reach this. A keymap outright; a symbol whose *value* is one,
which is how Doom names its leader prefixes (`doom-leader-buffer-map'); and a
symbol whose function cell is one, which is how Emacs spells a prefix command.
Only the first was handled, and the other two are what took `doom-leader-map'
from 229 bindings to 19 -- every prefix looked like a command."
  (cond
   ((keymapp def) def)
   ((and (symbolp def) (boundp def) (keymapp (symbol-value def))) (symbol-value def))
   ((and (symbolp def) (fboundp def) (keymapp (symbol-function def))) (symbol-function def))))

(defun meowr-dump--rows (map prefix)
  "Every binding in MAP under PREFIX, as (KEY-DESCRIPTION . COMMAND)."
  (let (rows)
    (map-keymap
     (lambda (event def)
       ;; `map-keymap\' hands a cons for a char-table *range*, which is how a
       ;; full keymap says "every printable character does this". Those are
       ;; self-insert and are shadowed by Meow\'s own full keymap anyway; a row
       ;; per character would be thousands of rows saying one thing.
       (unless (consp event)
         (let* ((keys (vconcat prefix (vector event)))
                ;; `(DESCRIPTION . TARGET)\': how Doom stores a leader entry so
                ;; which-key has a line to show. The target may itself be a
                ;; prefix, so it is unwrapped before anything else is asked.
                (def (if (and (consp def) (stringp (car def))) (cdr def) def))
                (sub (meowr-dump--keymap def)))
           (cond
            ((and sub (< (length keys) meowr-dump-depth))
             (setq rows (append rows (meowr-dump--rows sub keys))))
            (sub (push (cons (key-description keys) 'PREFIX) rows))
            ((and def (symbolp def))
             (push (cons (key-description keys) def) rows))))))
     map)
    rows))

(defun meowr-dump (map)
  "Print MAP as a TSV of key, command, description."
  (dolist (row (sort (meowr-dump--rows map []) 
                     (lambda (a b) (string< (car a) (car b)))))
    (princ (format "%s\t%s\t%s\n"
                   (car row)
                   (cdr row)
                   (let ((doc (ignore-errors (documentation (cdr row)))))
                     (if doc (car (split-string doc "\n")) ""))))))

(defun meowr-dump-shadowed ()
  "Print the first events Meow's Normal State keymap binds, one per line.

The shadow test is by *first event*: Meow's keymap binds single keys, so a
global sequence whose first key Meow has taken is unreachable whatever the
rest of it is."
  (when (boundp 'meow-normal-state-keymap)
    (dolist (row (meowr-dump--rows meow-normal-state-keymap []))
      (princ (format "%s\n" (car row))))))

(provide 'dump)
;;; dump.el ends here

;;; The sixth table: what Meow's Normal State leaves reachable.

(defconst meowr-dump-noise
  (rx bos "<"
      (or (seq (* nonl) "mouse" (* nonl)) (seq "wheel-" (* nonl))
          "menu-bar" "tool-bar" "tab-bar" "tab-line" "header-line" "mode-line"
          "vertical-line" (seq (* nonl) "scroll-bar") (seq (* nonl) "divider")
          (seq (* nonl) "edge") (seq (* nonl) "corner") (seq (* nonl) "fringe")
          (seq (* nonl) "margin") "select-window" "switch-frame" "delete-frame"
          "iconify-frame" "make-frame-visible" "language-change" "drag-n-drop"
          (seq "XF86" (* nonl)) "Scroll_Lock" (seq "touch" (* nonl)) "pinch"
          (seq (* nonl) "sync"))
      ">" eos)
  "Pseudo-events that are not keys.

A window divider and a mouse wheel are bindings, and they are not part of
any Ring: nobody's fingers know them. Dropping them takes 711 rows to 578
without losing a keystroke.")

(defun meowr-dump--read (path)
  "Read a TSV written by `meowr-dump' into an alist of (KEY COMMAND . DESC)."
  (with-temp-buffer
    (insert-file-contents path)
    (let (rows)
      (dolist (line (split-string (buffer-string) "\n" t) (nreverse rows))
        (let ((f (split-string line "\t")))
          (when (cadr f) (push (cons (car f) (cons (cadr f) (or (caddr f) ""))) rows)))))))

(defun meowr-dump--first (key)
  "The first event of KEY, which is what a shadow is decided by."
  (car (split-string key " ")))

(defun meowr-global-table (doom vanilla shadow)
  "Print the reachable-from-Normal-State table.

DOOM and VANILLA are `meowr-dump' outputs of each `current-global-map';
SHADOW is `meowr-dump-shadowed' output. A binding is reachable when its
first event is not one Meow's Normal keymap has taken -- Meow's keymap does
not suppress, so everything it has not claimed falls straight through."
  (let* ((doom-rows (meowr-dump--read doom))
         (vanilla-rows (meowr-dump--read vanilla))
         (taken (with-temp-buffer (insert-file-contents shadow)
                                  (split-string (buffer-string) "\n" t)))
         (help (make-hash-table :test #'equal))
         (counts (list 0 0 0)))
    ;; `C-h b', `<f1> b' and `<help> b' are one binding wearing three prefixes.
    ;; Keeping all three would put 112 rows of nothing new into a reference.
    (dolist (row doom-rows)
      (when (equal "C-h" (meowr-dump--first (car row)))
        (puthash (string-join (cdr (split-string (car row) " ")) " ") (cadr row) help)))
    (dolist (row doom-rows)
      (let* ((key (car row)) (cmd (cadr row)) (desc (cddr row))
             (head (meowr-dump--first key))
             (rest (string-join (cdr (split-string key " ")) " ")))
        (unless (or (string-match-p meowr-dump-noise head)
                    (member head taken)
                    (string-prefix-p "<remap>" key)
                    (and (member head '("<f1>" "<help>"))
                         (equal cmd (gethash rest help))))
          (let ((source (let ((was (assoc key vanilla-rows)))
                          (cond ((and was (equal cmd (cadr was))) "emacs")
                                (was "doom-rebound")
                                (t "doom")))))
            (cl-incf (nth (cond ((equal source "emacs") 0)
                                ((equal source "doom-rebound") 1) (t 2))
                          counts))
            (princ (format "%s\t%s\t%s\t%s\n" key cmd source desc))))))
    (princ (format "# emacs\t%d\n# doom-rebound\t%d\n# doom\t%d\n"
                   (nth 0 counts) (nth 1 counts) (nth 2 counts)))))
