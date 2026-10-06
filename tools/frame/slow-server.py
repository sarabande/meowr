#!/usr/bin/env python3
"""A language server that answers late (#124, #170).

Not a mock of rust-analyzer. A way to hold one answer in flight long enough to
do something else while it is there -- change the buffer, move the point, open a
picker -- because that is the only way to stage what an editor does with an
answer that arrives after the world moved.

A local rust-analyzer is too fast to lose these races: it answers a hover before
a small file finishes reading, so the interesting ordering never happens and the
hole looks like it does not exist. #124's first reproduction failed exactly that
way. A hole that needs a slow server is still a hole -- that is a cold index, a
large project, or a server at the end of a link.

Every question a late-answer row asks is answered, after the delay, and from
the text the server was told rather than from a script: a rename renames the
word at the position asked about, as the file stood when the question arrived.
So an answer the editor should have dropped really is stale, and one it should
have applied really lands.

    (set-language-server 'rust "tools/frame/slow-server.py" '())
    MEOWR_SLOW_DELAY=2.0          # seconds before each answer; 2 by default
    MEOWR_HOVER_DELAY=2.0         # the hover's alone, as #124 first set it
    MEOWR_SLOW_LOG=/tmp/slow.log  # every message received, one line each

The sleep blocks its own read loop, which is wanted: a real server answering
slowly is not also racing ahead with the next request.

Only ASCII is counted: a column is taken as a character index, which is what
UTF-16 is for the text these scenarios use.
"""

import json
import os
import re
import sys
import time

DELAY = float(os.environ.get("MEOWR_SLOW_DELAY", "2.0"))
HOVER_DELAY = float(os.environ.get("MEOWR_HOVER_DELAY", str(DELAY)))
SAYS = os.environ.get("MEOWR_HOVER_TEXT", "SLOW SERVER SAYS: this answer is about the file it was asked about")

LOG = os.environ.get("MEOWR_SLOW_LOG")
WORD = re.compile(r"[A-Za-z0-9_]+")



def read_message():
    headers = {}
    while True:
        line = sys.stdin.buffer.readline()
        if not line:
            return None
        line = line.strip()
        if not line:
            break
        key, _, value = line.decode().partition(":")
        headers[key.strip().lower()] = value.strip()
    return json.loads(sys.stdin.buffer.read(int(headers.get("content-length", 0))))


def send(message):
    body = json.dumps(message).encode()
    sys.stdout.buffer.write(b"Content-Length: %d\r\n\r\n" % len(body))
    sys.stdout.buffer.write(body)
    sys.stdout.buffer.flush()


def answer(message, result):
    send({"jsonrpc": "2.0", "id": message["id"], "result": result})


def span(line, start, end):
    return {"start": {"line": line, "character": start}, "end": {"line": line, "character": end}}


def inserting(uri, text):
    return {"changes": {uri: [{"range": span(0, 0, 0), "newText": text}]}}


class Workspace:
    """What the server knows: the documents it was told about, and the root to
    read the rest from. The one thing here that changes, kept in one place."""

    def __init__(self, root):
        self.root = root
        self.told = {}

    def text_of(self, uri):
        """What a document says as far as the server knows: told, else disk."""
        if uri in self.told:
            return self.told[uri]
        try:
            with open(uri.removeprefix("file://")) as file:
                return file.read()
        except OSError:
            return ""

    def files(self):
        """Every file an answer could reach: what was told, and what is on disk."""
        found = set(self.told)
        if self.root:
            for directory, _, names in os.walk(self.root):
                if ".git" not in directory.split(os.sep):
                    found.update("file://" + os.path.join(directory, n) for n in names if n.endswith(".rs"))
        return sorted(found)

    def scan(self, pattern):
        """Every match of `pattern` in every file, as `(uri, line, match)`."""
        return [
            (uri, number, match)
            for uri in self.files()
            for number, line in enumerate(self.text_of(uri).split("\n"))
            for match in pattern.finditer(line)
        ]

    def word_at(self, uri, position):
        """The word at a position, and its span, or None."""
        lines = self.text_of(uri).split("\n")
        line = position["line"]
        if line >= len(lines):
            return None
        return next(
            ((m.group(), span(line, m.start(), m.end()))
             for m in WORD.finditer(lines[line])
             if m.start() <= position["character"] <= m.end()),
            None,
        )


def whole(word):
    return re.compile(r"\b%s\b" % re.escape(word))


def main():
    workspace = Workspace(None)
    while True:
        message = read_message()
        if message is None:
            return
        method = message.get("method")
        params = message.get("params") or {}
        if LOG:
            with open(LOG, "a") as log:
                log.write(json.dumps({"at": round(time.time(), 2), **message}) + "\n")
        if method == "initialize":
            workspace = Workspace((params.get("rootUri") or "").removeprefix("file://") or None)
            answer(message, {"capabilities": {
                "textDocumentSync": 1,
                "hoverProvider": True,
                "definitionProvider": True,
                "referencesProvider": True,
                "renameProvider": {"prepareProvider": True},
                "codeActionProvider": {"resolveProvider": True},
            }})
        elif method == "textDocument/didOpen":
            document = params["textDocument"]
            workspace.told[document["uri"]] = document["text"]
        elif method == "textDocument/didChange":
            workspace.told[params["textDocument"]["uri"]] = params["contentChanges"][-1]["text"]
        elif method == "textDocument/didClose":
            workspace.told.pop(params["textDocument"]["uri"], None)
        elif method == "textDocument/hover":
            time.sleep(HOVER_DELAY)
            answer(message, {"contents": {"kind": "plaintext", "value": SAYS}})
        elif method in ("textDocument/definition", "textDocument/references"):
            # A definition is the word after `fn`; references are every use.
            # Computed now, from the text as it stands when the question
            # arrives, then held for the delay -- as every answer below is.
            asked = workspace.word_at(params["textDocument"]["uri"], params["position"])
            found = []
            if asked:
                definition = method == "textDocument/definition"
                pattern = re.compile(r"\bfn (%s)\b" % re.escape(asked[0])) if definition else whole(asked[0])
                found = [
                    {"uri": uri, "range": span(line, match.start(match.lastindex or 0), match.end())}
                    for uri, line, match in workspace.scan(pattern)
                ]
            time.sleep(DELAY)
            answer(message, found)
        elif method == "textDocument/prepareRename":
            asked = workspace.word_at(params["textDocument"]["uri"], params["position"])
            time.sleep(DELAY)
            answer(message, asked and {"range": asked[1], "placeholder": asked[0]})
        elif method == "textDocument/rename":
            asked = workspace.word_at(params["textDocument"]["uri"], params["position"])
            edits = {}
            for uri, line, match in workspace.scan(whole(asked[0])) if asked else []:
                edits.setdefault(uri, []).append(
                    {"range": span(line, match.start(), match.end()), "newText": params["newName"]}
                )
            time.sleep(DELAY)
            answer(message, asked and {"changes": edits})
        elif method == "textDocument/codeAction":
            uri = params["textDocument"]["uri"]
            time.sleep(DELAY)
            answer(message, [
                {"title": "Slow fix: add a comment", "kind": "quickfix",
                 "edit": inserting(uri, "// slow fix\n")},
                {"title": "Slow note: resolve first", "kind": "refactor.rewrite",
                 "data": {"uri": uri}},
            ])
        elif method == "codeAction/resolve":
            time.sleep(DELAY)
            answer(message, {**params, "edit": inserting(params["data"]["uri"], "// slow note\n")})
        elif "id" in message:
            # Every other request answered emptily rather than ignored: a
            # request with no reply is one meowr waits on for ever.
            answer(message, None)


if __name__ == "__main__":
    main()
