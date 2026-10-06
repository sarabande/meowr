//! Talking to a language server.
//!
//! In the binary crate because this is I/O — a child process and a pipe-shaped
//! protocol — and meowr's rule is that domain crates do none. #109 confirmed
//! the fit: `meowr::config` evaluates a Steel program here and `autosave`
//! writes files here, and both are the same shape of thing.
//!
//! #110 settled the shape and the decision it settled is **not a task per
//! request**. A connection is a child process the session talks to for as long
//! as that server runs, and it can push a notification with no request behind
//! it at all.
//!
//! Concretely that is **two long-lived tasks per connection, and no others** —
//! a reader draining the child's stdout and a writer feeding its stdin, both
//! living exactly as long as the connection. Two rather than the one #110's
//! wording suggests because a single loop cannot await a read and a write at
//! once without one starving the other; the decision's content is that nothing
//! is spawned *per request*, and nothing is.
//!
//! Out of the reader come **two channels, not one**: replies, correlated by the
//! JSON-RPC `id` a table remembers when the request goes out, and
//! notifications, which answer nothing and are simply delivered as they
//! arrive.
//!
//! The protocol is separated from the process on purpose. [`Connection::over`]
//! takes any reader and writer, so the framing and the dispatch are driven in
//! tests over an in-memory pipe; [`Connection::spawn`] is the thin part that
//! wires a real child's stdio into it. Without that seam the only way to test
//! any of this would be to have a real language server installed, which is
//! what #112 was for and is not a thing a unit test should need.

use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::sync::{mpsc, oneshot};

/// Something the server said that answers no request of ours.
///
/// `publishDiagnostics` chief among them, which is why this carries the method
/// rather than being one type per notification: #112 found a real server
/// sending several for the same URI in a row, and a client that understood
/// only the ones it expected would drop the rest silently.
#[derive(Debug, Clone, PartialEq)]
pub struct Notification {
    pub method: String,
    pub params: Value,
}

/// What went wrong, as far as a caller needs to know.
#[derive(Debug)]
pub enum Error {
    /// The server could not be started at all.
    Spawn(std::io::Error),
    /// The connection is gone: the task ended, or the child died.
    Closed,
    /// The server answered the request with a JSON-RPC error.
    Server(Value),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Spawn(e) => write!(f, "could not start the language server: {e}"),
            Error::Closed => write!(f, "the language server connection is closed"),
            Error::Server(e) => write!(f, "the language server refused: {e}"),
        }
    }
}

impl std::error::Error for Error {}

/// The table of requests we are waiting on answers to.
///
/// A reply carries an `id` and nothing else that says what it answers, so the
/// side that *sent* the request is the only side that can know — which is why
/// this is shared between the writer, which fills it in, and the reader, which
/// empties it.
type Pending = Arc<Mutex<HashMap<i64, oneshot::Sender<Result<Value, Error>>>>>;

/// One live connection to one language server.
///
/// Dropping it kills the child (`kill_on_drop`), which closes the pipes, ends
/// the reader at end of file, and with it the writer — which is the whole
/// reason the child is owned here rather than detached. The reader holds a
/// sender to the writer, because it answers requests from the server (#160),
/// so the writer outlives the connection until the reader ends; over
/// [`Connection::over`] with no child, that is when the far end closes.
pub struct Connection {
    outgoing: mpsc::Sender<Vec<u8>>,
    pending: Pending,
    /// Set once the reader task has ended, and never cleared.
    ///
    /// A latch rather than a one-time drain of the table, because draining
    /// answers only the requests that were *already* waiting. A request that
    /// registers after the server has gone would find nobody left to fail it
    /// and wait for an answer that cannot come — which is a hang, not an
    /// error, and the worst of the three outcomes.
    closed: Arc<AtomicBool>,
    next_id: Mutex<i64>,
    /// What the server said it can do, from its `initialize` reply: asked
    /// before a question it may not answer at all (#168).
    capabilities: std::sync::OnceLock<Value>,
    /// Kept so that dropping the connection kills the server. `Child`'s own
    /// `kill_on_drop` does this, and is set where the child is spawned.
    _child: Option<tokio::process::Child>,
}

impl Connection {
    /// Start `command` in `root` and complete the handshake.
    ///
    /// The handshake is awaited here rather than left to the caller, which is
    /// #114's own acceptance criterion: `initialize`/`initialized` completes
    /// before any other message crosses, and a caller that could forget would
    /// be a caller that sends a request a server is entitled to ignore.
    pub async fn spawn(
        command: &str,
        args: &[&str],
        root: &std::path::Path,
    ) -> Result<(Self, mpsc::Receiver<Notification>), Error> {
        let (connection, notifications) = Self::started(command, args, root)?;
        connection.handshake(root).await?;
        Ok((connection, notifications))
    }

    /// Start the child and wire its stdio up, without the handshake.
    ///
    /// Split from [`Connection::spawn`] so the tests can start a process that
    /// could not complete a handshake — `cat`, for the one question that needs
    /// a real child, which is whether dropping the connection kills it. Two
    /// copies of this `Command` builder is how the tested path and the real one
    /// come to differ in a flag like `kill_on_drop`.
    fn started(
        command: &str,
        args: &[&str],
        root: &std::path::Path,
    ) -> Result<(Self, mpsc::Receiver<Notification>), Error> {
        let mut child = tokio::process::Command::new(command)
            .args(args)
            .current_dir(root)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            // A language server on stderr is chatty and none of it is protocol.
            .stderr(std::process::Stdio::null())
            // Without this a killed editor leaves a rust-analyzer behind
            // holding a workspace open, which is #117's own criterion.
            .kill_on_drop(true)
            .spawn()
            .map_err(Error::Spawn)?;
        let stdin = child.stdin.take().ok_or(Error::Closed)?;
        let stdout = child.stdout.take().ok_or(Error::Closed)?;
        let (mut connection, notifications) = Self::over(stdout, stdin);
        connection._child = Some(child);
        Ok((connection, notifications))
    }

    /// The protocol over any reader and writer, with no process behind it.
    ///
    /// The seam this module is built around: the framing and the id table are
    /// the interesting part and neither needs a child to exercise.
    pub fn over<R, W>(reader: R, writer: W) -> (Self, mpsc::Receiver<Notification>)
    where
        R: AsyncRead + Unpin + Send + 'static,
        W: AsyncWrite + Unpin + Send + 'static,
    {
        let pending: Pending = Arc::default();
        // Bounded, so a server pushing faster than the editor drains applies
        // back-pressure rather than growing without limit. Sized well above a
        // realistic burst: #112 saw three notifications for one URI in a
        // second, and the editor drains one per frame.
        let (notify_tx, notify_rx) = mpsc::channel(64);
        let (outgoing, mut to_write) = mpsc::channel::<Vec<u8>>(32);

        let mut writer = writer;
        tokio::spawn(async move {
            while let Some(frame) = to_write.recv().await {
                if writer.write_all(&frame).await.is_err() || writer.flush().await.is_err() {
                    break;
                }
            }
        });

        let table = pending.clone();
        let closed = Arc::new(AtomicBool::new(false));
        let latch = closed.clone();
        let answers = outgoing.clone();
        tokio::spawn(async move {
            let mut reader = BufReader::new(reader);
            while let Some(message) = read_frame(&mut reader).await {
                dispatch(message, &table, &notify_tx, &answers).await;
            }
            // The server has gone. The latch goes up *before* the table is
            // drained, so a request registering concurrently either sees the
            // latch or is in the table for this drain to find — never neither,
            // which is the ordering that turns the race into a hang.
            latch.store(true, Ordering::SeqCst);
            let waiting: Vec<_> = table
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .drain()
                .collect();
            for (_, reply) in waiting {
                let _ = reply.send(Err(Error::Closed));
            }
        });

        (
            Self {
                outgoing,
                pending,
                closed,
                next_id: Mutex::new(1),
                capabilities: std::sync::OnceLock::new(),
                _child: None,
            },
            notify_rx,
        )
    }

    /// `initialize`, then `initialized`. See [`Connection::spawn`].
    async fn handshake(&self, root: &std::path::Path) -> Result<(), Error> {
        let uri = format!("file://{}", root.display());
        let reply = self.request(
            "initialize",
            json!({
                "processId": std::process::id(),
                "rootUri": uri,
                "workspaceFolders": [{ "uri": uri, "name": "root" }],
                // No `initializationOptions`, and that is a finding rather
                // than an omission. `diagnostics.experimental.enable` looks
                // like the answer to "why does an unsaved type error show
                // nothing": rust-analyzer's default diagnostics come from
                // `cargo check`, which reads the file from **disk**. It is not.
                //
                // Measured, twice, against this repository: with the flag on,
                // a deliberate `fn f() -> i32 { "s" }` in an unsaved buffer
                // produced `E0308` on one run and nothing on the next. The
                // difference is a race — whether `didOpen` lands before the
                // crate graph is built. It did on the run that "worked", and
                // that run also produced seventeen *false* diagnostics
                // (unresolved `derive`, "no method `then` on type `bool`")
                // because the file was being analysed without its crate. Once
                // the graph is loaded the false ones go and so does the real
                // one.
                //
                // So the flag buys nothing reliable and costs a screenful of
                // wrong squiggles on a cold start. #112 saw `E0308` on unsaved
                // text and it was real, but it was the transient state — not a
                // behaviour to build an acceptance criterion on.
                "capabilities": {
                    // Declared so the server reports when it has finished
                    // loading. Not decoration: rust-analyzer answers a
                    // `didOpen` it receives mid-load with an empty diagnostic
                    // list, and without `$/progress` a client cannot tell that
                    // "no problems" from a real one. #112 saw the empty answer
                    // and this is how the difference becomes observable.
                    "window": { "workDoneProgress": true },
                    "textDocument": {
                        "publishDiagnostics": { "versionSupport": true },
                        "hover": { "contentFormat": ["plaintext", "markdown"] },
                        // What lets a server advertise `prepareProvider`, which
                        // is how `SPC k r` knows to ask before a name is typed
                        // (#168). lsp-mode declares it.
                        "rename": { "prepareSupport": true },
                        // #157's measured set. Without literal support
                        // rust-analyzer answers every question with `null`;
                        // without `resolveSupport` and `dataSupport` gopls sends
                        // commands that end in a `workspace/applyEdit` meowr
                        // does not answer. No `resourceOperations` (ADR-0013
                        // amended).
                        "codeAction": {
                            "codeActionLiteralSupport": { "codeActionKind": { "valueSet": [
                                "", "quickfix", "refactor", "refactor.extract",
                                "refactor.inline", "refactor.rewrite", "source",
                                "source.organizeImports", "source.fixAll",
                            ] } },
                            "resolveSupport": { "properties": ["edit"] },
                            "dataSupport": true,
                        },
                    },
                },
            }),
        )
        .await?;
        let _ = self
            .capabilities
            .set(reply.get("capabilities").cloned().unwrap_or(Value::Null));
        self.notify("initialized", json!({}))
    }

    /// What the server said it can do when it started.
    pub fn capabilities(&self) -> &Value {
        static NONE: Value = Value::Null;
        self.capabilities.get().unwrap_or(&NONE)
    }

    /// Send a request and wait for the reply that names it.
    pub async fn request(&self, method: &str, params: Value) -> Result<Value, Error> {
        let id = {
            let mut next = self.next_id.lock().unwrap_or_else(|e| e.into_inner());
            let id = *next;
            *next += 1;
            id
        };
        let (tx, rx) = oneshot::channel();
        // Recorded *before* the frame goes out: a server fast enough to answer
        // between the write and the insert would find no one waiting.
        self.pending
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(id, tx);
        let sent = self
            .outgoing
            .send(frame(&json!({
                "jsonrpc": "2.0",
                "id": id,
                "method": method,
                "params": params,
            })))
            .await;
        // Checked after registering, which is the half of the ordering this
        // side owns: the reader raises the latch before it drains, so seeing it
        // down here means the drain has not happened and the entry above will
        // be found by it.
        if sent.is_err() || self.closed.load(Ordering::SeqCst) {
            self.pending
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .remove(&id);
            return Err(Error::Closed);
        }
        rx.await.map_err(|_| Error::Closed)?
    }

    /// Send a notification, which by definition has no reply to wait for.
    pub fn notify(&self, method: &str, params: Value) -> Result<(), Error> {
        self.outgoing
            .try_send(frame(&json!({
                "jsonrpc": "2.0",
                "method": method,
                "params": params,
            })))
            .map_err(|_| Error::Closed)
    }
}

/// Sort one message into a reply or a notification.
///
/// The two are told apart by `id`, which is what #110 decided rather than
/// forcing both down one channel: a reply answers a specific outstanding
/// request and a notification answers nothing, and sorting them at the channel
/// boundary costs less to read later than one enum doing both jobs.
async fn dispatch(
    message: Value,
    pending: &Pending,
    notify: &mpsc::Sender<Notification>,
    answers: &mpsc::Sender<Vec<u8>>,
) {
    match (message.get("id"), message.get("method")) {
        // A reply: an id, and no method of its own.
        (Some(id), None) => {
            let Some(id) = id.as_i64() else { return };
            let waiting = pending
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .remove(&id);
            if let Some(reply) = waiting {
                let answer = match message.get("error") {
                    Some(error) => Err(Error::Server(error.clone())),
                    None => Ok(message.get("result").cloned().unwrap_or(Value::Null)),
                };
                let _ = reply.send(answer);
            }
        }
        // A notification: a method, and no id.
        (None, Some(method)) => {
            let Some(method) = method.as_str() else {
                return;
            };
            let _ = notify
                .send(Notification {
                    method: method.to_string(),
                    params: message.get("params").cloned().unwrap_or(Value::Null),
                })
                .await;
        }
        // A request *from* the server: both an id and a method. It is owed an
        // answer, and a server may wait for one -- gopls asks for a progress
        // token during `initialized` and answers nothing at all, `shutdown`
        // included, until it hears back (#160). This used to say ignoring one
        // was what the protocol allowed; it is not.
        (Some(id), Some(method)) => {
            let _ = answers
                .send(frame(&answer_to(id, method.as_str().unwrap_or_default())))
                .await;
        }
        // Neither: a frame without either key, or not an object at all. Not a
        // message this protocol has.
        (None, None) => {}
    }
}

/// The answer a request from the server gets.
///
/// `window/workDoneProgress/create` is the one meowr can say yes to: it
/// declares `window.workDoneProgress`, so a server asks for a token before
/// reporting progress, and the token needs nothing kept -- `$/progress`
/// notifications already arrive whether or not one was granted. #157 measured
/// it as the only request rust-analyzer and gopls send once code actions are
/// declared with `resolveSupport` and `dataSupport`.
///
/// Anything else is refused with `MethodNotFound` rather than left unanswered,
/// so a server is told meowr cannot rather than waiting to be told anything.
/// Measured, not assumed: gopls sends `workspace/applyEdit` while running a
/// `gopls.apply_fix` command, and silence hung it for every request after;
/// with this refusal the command returns the error promptly and gopls goes on
/// answering (#160).
fn answer_to(id: &Value, method: &str) -> Value {
    match method {
        "window/workDoneProgress/create" => json!({"jsonrpc": "2.0", "id": id, "result": null}),
        _ => json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": {"code": METHOD_NOT_FOUND, "message": format!("meowr does not answer {method}")},
        }),
    }
}

/// JSON-RPC's code for a request the other side does not know.
const METHOD_NOT_FOUND: i64 = -32601;

/// One message, with the `Content-Length` header the protocol frames on.
fn frame(message: &Value) -> Vec<u8> {
    let body = message.to_string();
    format!("Content-Length: {}\r\n\r\n{body}", body.len()).into_bytes()
}

/// Read one `Content-Length`-prefixed message, or `None` once the pipe ends.
///
/// Headers are read as lines and the body by exact byte count, which is the
/// protocol: a JSON body may contain anything, newlines included, so the
/// length is the only thing that says where it stops.
async fn read_frame<R>(reader: &mut BufReader<R>) -> Option<Value>
where
    R: AsyncRead + Unpin,
{
    let mut length: Option<usize> = None;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).await.ok()? == 0 {
            return None;
        }
        let line = line.trim_end_matches(['\r', '\n']);
        if line.is_empty() {
            break;
        }
        if let Some(value) = line
            .split_once(':')
            .filter(|(name, _)| name.eq_ignore_ascii_case("content-length"))
        {
            length = value.1.trim().parse().ok();
        }
    }
    let mut body = vec![0u8; length?];
    reader.read_exact(&mut body).await.ok()?;
    serde_json::from_slice(&body).ok()
}

#[cfg(test)]
mod tests;

// --- the shapes the client speaks ------------------------------------------

/// A path as the protocol addresses documents: a `file://` URI.
///
/// Made absolute first, which is not optional: the protocol's URIs are
/// absolute, and meowr's paths are whatever was typed on the command line.
/// `meowr crates/meowr/src/lsp.rs` would otherwise send
/// `file://crates/meowr/src/lsp.rs`, which names a host called `crates` — a
/// real server accepts it, answers nothing, and the editor shows no
/// diagnostics for a reason nothing on screen explains.
///
/// `std::path::absolute` rather than `canonicalize`: it needs no filesystem
/// access and does not resolve symlinks, so the URI sent is the one that comes
/// back and [`path_of`] can match it against a buffer put through the same
/// function. Resolving links would make those two disagree whenever a
/// workspace is reached through one.
pub fn uri_of(path: &str) -> String {
    let absolute = std::path::absolute(path)
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| path.to_string());
    format!("file://{absolute}")
}

/// The absolute form of a path, so two spellings of one file compare equal.
///
/// The other half of [`uri_of`]: a buffer holds the path that was typed and a
/// notification names an absolute one, and something has to put them in the
/// same terms before they can be matched.
pub fn absolute(path: &str) -> String {
    std::path::absolute(path)
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| path.to_string())
}

/// The path back out of a URI, or `None` if it is not one of ours.
///
/// Notifications name a document by URI and the session knows it by path, so
/// something has to turn one into the other. Percent-decoding is not done and
/// does not need to be: the URIs the server answers with are the ones this
/// module sent, and `uri_of` does not encode.
pub fn path_of(uri: &str) -> Option<&str> {
    uri.strip_prefix("file://")
}

/// Turn a server's `publishDiagnostics` params into what a Buffer holds.
///
/// Deserialized through `lsp_types` rather than read field by field out of a
/// `Value`, which is #109's own conclusion — "whatever else gets decided, the
/// vocabulary is this crate" — and it earns its place here: the severity codes
/// and the three shapes of a hover's `contents` are exactly the places a
/// hand-rolled reader guesses, and a guess against a real server is the quiet
/// failure this project keeps choosing against.
///
/// A params object this cannot parse yields nothing rather than an error. The
/// server said something meowr does not understand, and the honest response is
/// to place no squiggle — not to place one somewhere arbitrary.
pub fn diagnostics_of(
    params: &Value,
    doc: &meowr_domain::Document,
) -> Vec<meowr_domain::Diagnostic> {
    let Ok(published) =
        serde_json::from_value::<lsp_types::PublishDiagnosticsParams>(params.clone())
    else {
        return Vec::new();
    };
    published
        .diagnostics
        .iter()
        .filter_map(|d| diagnostic_of(d, doc))
        .collect()
}

/// The version a `publishDiagnostics` was computed against, if it said.
///
/// The version the server echoes back is the one this module sent, which is
/// the buffer revision the answer was computed against — exactly the tag
/// `Buffer::set_diagnostics` refuses a stale answer by.
pub fn version_of(params: &Value) -> Option<u64> {
    params.get("version").and_then(Value::as_u64)
}

/// The URI a `publishDiagnostics` is about.
pub fn uri_in(params: &Value) -> Option<&str> {
    params.get("uri").and_then(Value::as_str)
}

fn diagnostic_of(
    diagnostic: &lsp_types::Diagnostic,
    doc: &meowr_domain::Document,
) -> Option<meowr_domain::Diagnostic> {
    let start = doc.offset_at(
        diagnostic.range.start.line as usize,
        diagnostic.range.start.character as usize,
    )?;
    let end = doc.offset_at(
        diagnostic.range.end.line as usize,
        diagnostic.range.end.character as usize,
    )?;
    let start = start.min(doc.byte_len());
    Some(meowr_domain::Diagnostic {
        // A server may answer with a zero-width range — a missing semicolon
        // has no characters to underline. One byte of it is given something to
        // draw under, because a run of zero columns is a squiggle nobody sees.
        range: start..end.max(start + 1),
        message: diagnostic.message.clone(),
        severity: match diagnostic.severity {
            Some(lsp_types::DiagnosticSeverity::ERROR) => meowr_domain::Severity::Error,
            Some(lsp_types::DiagnosticSeverity::WARNING) => meowr_domain::Severity::Warning,
            Some(lsp_types::DiagnosticSeverity::INFORMATION) => meowr_domain::Severity::Information,
            Some(lsp_types::DiagnosticSeverity::HINT) => meowr_domain::Severity::Hint,
            // LSP says an absent severity is the client's to decide, and a code
            // outside the four is a server meowr does not understand. An error
            // is the safe way to be wrong either way: a real problem shown too
            // loudly beats one shown as a hint and skimmed past.
            _ => meowr_domain::Severity::Error,
        },
    })
}

/// The point, as a position the protocol understands.
///
/// The walk is `Document`'s, not this module's. It was written here, and #133
/// moved the other direction into the domain on the grounds that it takes a
/// Document and two numbers and has no protocol in it — which is as true of
/// this direction, and was left as a second copy until #138 found a third
/// about to be written. The head is what an offset outside the text answers,
/// which is what the hand-rolled version did by falling back a step at a time.
pub fn position_at(doc: &meowr_domain::Document, offset: usize) -> Value {
    let (line, character) = doc.position_at(offset).unwrap_or((0, 0));
    json!({"line": line, "character": character})
}

/// The readable part of a `textDocument/hover` reply.
///
/// `contents` has three shapes in the protocol — a `MarkupContent`, a single
/// `MarkedString`, and an array of them — and a server picks whichever it
/// likes. `lsp_types::HoverContents` is that choice as an enum, which is why
/// all three are handled here rather than the one rust-analyzer happens to
/// send: the shape is not this client's to assume.
pub fn hover_text(reply: &Value) -> Option<String> {
    let hover: lsp_types::Hover = serde_json::from_value(reply.clone()).ok()?;
    let text = match hover.contents {
        lsp_types::HoverContents::Markup(markup) => markup.value,
        lsp_types::HoverContents::Scalar(marked) => marked_text(&marked),
        lsp_types::HoverContents::Array(parts) => {
            parts.iter().map(marked_text).collect::<Vec<_>>().join("\n")
        }
    };
    // rust-analyzer answers in markdown and fences its signatures. The fences
    // are noise in an Echo Area that cannot render them, so they go; the code
    // inside them is the part the user asked for.
    let stripped: Vec<&str> = text
        .lines()
        .filter(|line| !line.trim_start().starts_with("```"))
        .collect();
    let text = stripped.join("\n").trim().to_string();
    (!text.is_empty()).then_some(text)
}

/// A `MarkedString`'s text, whichever of its two shapes it is.
///
/// The language tag of a `LanguageString` is dropped: it says how to highlight
/// the value, and the Echo Area draws one style.
fn marked_text(marked: &lsp_types::MarkedString) -> String {
    match marked {
        lsp_types::MarkedString::String(text) => text.clone(),
        lsp_types::MarkedString::LanguageString(language) => language.value.clone(),
    }
}

/// `textDocument/didOpen`, which is how a server learns a file exists.
///
/// `language` is meowr's own Language name and goes across as LSP's
/// `languageId` unchanged: they are the same string for every language either
/// side has a name for — `rust`, `go`, `python`. A function mapping one to the
/// other would be two names for one thing and would drift; a language where
/// they genuinely disagree is a reason to write that disagreement down here,
/// not a reason to have a table now.
pub fn did_open(path: &str, language: &str, version: u64, text: &str) -> Value {
    json!({"textDocument": {
        "uri": uri_of(path),
        "languageId": language,
        "version": version,
        "text": text,
    }})
}

/// `textDocument/didClose`, for a path no buffer is served for any more.
pub fn did_close(path: &str) -> Value {
    json!({"textDocument": {"uri": uri_of(path)}})
}

/// `textDocument/didChange`, in full rather than as a diff.
///
/// Full text because the server is told what the buffer *is* rather than how
/// it got there, and meowr's own incremental edits are shaped for tree-sitter
/// (`tree_sitter_edits`) rather than for LSP's ranges. A file the size of
/// anything in this repository costs nothing to send whole, and a wrong
/// incremental edit desynchronises a server silently — the failure this
/// project keeps choosing against.
pub fn did_change(path: &str, version: u64, text: &str) -> Value {
    json!({
        "textDocument": {"uri": uri_of(path), "version": version},
        "contentChanges": [{"text": text}],
    })
}

/// `textDocument/hover` at a point.
pub fn hover_at(path: &str, doc: &meowr_domain::Document, offset: usize) -> Value {
    text_document_position(path, doc, offset)
}

/// `TextDocumentPositionParams`: a file and a point in it.
///
/// What a hover asks with, and what the three lookups for one place ask with
/// -- none of them says anything else about the question (#146).
fn text_document_position(path: &str, doc: &meowr_domain::Document, offset: usize) -> Value {
    json!({
        "textDocument": {"uri": uri_of(path)},
        "position": position_at(doc, offset),
    })
}

/// `textDocument/references`: everywhere the symbol at `offset` is used.
///
/// `includeDeclaration` is true, which is Doom's `+lookup/references` and
/// `xref`'s own behaviour: the definition is one of the places the thing is,
/// and a list that omitted it would be a list you had to remember was partial.
pub fn references_at(path: &str, doc: &meowr_domain::Document, offset: usize) -> Value {
    json!({
        "textDocument": {"uri": uri_of(path)},
        "position": position_at(doc, offset),
        "context": {"includeDeclaration": true},
    })
}

/// `workspace/symbol`, asked **with** a query.
///
/// The query is the whole point (#144). Asked with an empty one, rust-analyzer
/// answers with a truncated, alphabetically-first list dominated by dependency
/// crates — so the picker that filtered it was missing its own workspace's
/// symbols, which is what #137 handed two rows back for.
pub fn workspace_symbol(query: &str) -> Value {
    json!({"query": query})
}

/// The symbols a `workspace/symbol` reply holds, as Places.
///
/// **The name comes with them**, which a references reply cannot give: a
/// `SymbolInformation` carries `name` as well as a location, so a row reads as
/// the symbol rather than as `path:line`. That is the optional `text` on a
/// [`meowr_domain::Place`] earning its keep in the other direction from grep
/// (ADR-0011).
///
/// Both shapes the protocol allows are read. A server may answer with
/// `SymbolInformation[]`, whose location is a range in a file, or with
/// `WorkspaceSymbol[]`, whose location may be *just a URI* — the second exists
/// so a server can answer without having resolved a position yet, and a
/// hand-rolled reader would take the first shape and silently drop the other.
pub fn symbols_of(reply: &Value) -> Vec<meowr_domain::Place> {
    let symbols: Vec<lsp_types::SymbolInformation> =
        serde_json::from_value(reply.clone()).unwrap_or_default();
    symbols
        .iter()
        .filter_map(|symbol| {
            let place = place_of_location(&symbol.location)?;
            // Named with its container where the server gave one: two `new`s
            // in a workspace are told apart by what they are on, and that is
            // exactly what a symbol picker is for.
            Some(match &symbol.container_name {
                Some(container) if !container.is_empty() => {
                    place.showing(format!("{container}::{}", symbol.name))
                }
                _ => place.showing(&symbol.name),
            })
        })
        .collect()
}

/// What a place lookup's reply holds, as Places.
///
/// No text: a server answers with positions and nothing else, so these rows
/// show `path:line` (ADR-0011). Deserialized through `lsp_types` rather than
/// read field by field, which is #109's rule and earns its place here twice
/// over. A references reply is `Location[] | null`, and the three that look for
/// one place (#146) may answer `Location | Location[] | LocationLink[] | null`
/// -- `GotoDefinitionResponse` is all of those, and the references shape is
/// one of them, so one reader serves all four rows.
///
/// A `LocationLink` is placed by its **selection** range, the name, rather
/// than its target range, the whole node: rust-analyzer's target range for a
/// function begins at the doc comment above it.
pub fn places_of(reply: &Value) -> Vec<meowr_domain::Place> {
    match serde_json::from_value(reply.clone()) {
        Ok(lsp_types::GotoDefinitionResponse::Scalar(location)) => {
            place_of_location(&location).into_iter().collect()
        }
        Ok(lsp_types::GotoDefinitionResponse::Array(locations)) => {
            locations.iter().filter_map(place_of_location).collect()
        }
        Ok(lsp_types::GotoDefinitionResponse::Link(links)) => links
            .iter()
            .filter_map(|link| {
                place_at(link.target_uri.as_str(), link.target_selection_range.start)
            })
            .collect(),
        Err(_) => Vec::new(),
    }
}

/// The method and parameters for a lookup about the point.
///
/// `None` for `Symbols`, which is about a string rather than a place and is
/// asked as the input changes rather than once (#144).
pub fn lookup_request(
    kind: &meowr_app::LookupKind,
    path: &str,
    doc: &meowr_domain::Document,
    offset: usize,
) -> Option<(&'static str, Value)> {
    use meowr_app::LookupKind;
    let at = || text_document_position(path, doc, offset);
    match kind {
        LookupKind::References => {
            Some(("textDocument/references", references_at(path, doc, offset)))
        }
        LookupKind::Definition => Some(("textDocument/definition", at())),
        LookupKind::TypeDefinition => Some(("textDocument/typeDefinition", at())),
        LookupKind::Implementations => Some(("textDocument/implementation", at())),
        LookupKind::Symbols { .. } => None,
    }
}

/// A server's `WorkspaceEdit` as the session takes one (#167, ADR-0013).
///
/// `documentChanges` when the server sent it, which the protocol prefers, in the
/// server's order; otherwise `changes`, which is a map and so is put in path
/// order. The document versions are dropped: #157 measured them unreliable, and
/// the session stamps its own revisions instead. Resource operations are kept
/// as they came, so the session can refuse the edit in words that name them.
/// Paths are as the URIs spell them, for the host to respell; no file is read
/// here.
///
/// `None` for a reply that is not a `WorkspaceEdit` -- including `null`, which
/// is how a rename says there was nothing to rename.
pub fn workspace_edit_of(reply: &Value) -> Option<Vec<meowr_app::FileChange>> {
    use lsp_types::{DocumentChangeOperation, DocumentChanges, OneOf, ResourceOp};
    let edit: lsp_types::WorkspaceEdit = serde_json::from_value(reply.clone()).ok()?;
    let path = |uri: &lsp_types::Uri| path_of(uri.as_str()).map(str::to_string);
    let file_edit = |uri: &lsp_types::Uri, edits: Vec<&lsp_types::TextEdit>| {
        Some(meowr_app::FileChange::Edit(meowr_app::FileEdit {
            path: path(uri)?,
            read: None,
            edits: edits.into_iter().map(line_edit_of).collect(),
        }))
    };
    let document_edit = |edit: &lsp_types::TextDocumentEdit| {
        let edits = edit
            .edits
            .iter()
            .map(|one| match one {
                OneOf::Left(plain) => plain,
                OneOf::Right(annotated) => &annotated.text_edit,
            })
            .collect();
        file_edit(&edit.text_document.uri, edits)
    };
    match (edit.document_changes, edit.changes) {
        (Some(DocumentChanges::Edits(edits)), _) => edits.iter().map(document_edit).collect(),
        (Some(DocumentChanges::Operations(operations)), _) => operations
            .iter()
            .map(|operation| match operation {
                DocumentChangeOperation::Edit(edit) => document_edit(edit),
                DocumentChangeOperation::Op(ResourceOp::Create(create)) => {
                    Some(meowr_app::FileChange::Create { path: path(&create.uri)? })
                }
                DocumentChangeOperation::Op(ResourceOp::Rename(rename)) => {
                    Some(meowr_app::FileChange::Rename {
                        from: path(&rename.old_uri)?,
                        to: path(&rename.new_uri)?,
                    })
                }
                DocumentChangeOperation::Op(ResourceOp::Delete(delete)) => {
                    Some(meowr_app::FileChange::Delete { path: path(&delete.uri)? })
                }
            })
            .collect(),
        (None, Some(changes)) => {
            let mut files: Vec<_> = changes.iter().collect();
            files.sort_by(|a, b| a.0.as_str().cmp(b.0.as_str()));
            files
                .into_iter()
                .map(|(uri, edits)| file_edit(uri, edits.iter().collect()))
                .collect()
        }
        (None, None) => Some(Vec::new()),
    }
}

/// The request for one of a rename's two questions (#168).
///
/// Both are asked at the place the rename started from, as lsp-mode does: the
/// symbol does not move while its new name is read.
pub fn rename_request(
    asked: &meowr_app::RenameQuestion,
    doc: &meowr_domain::Document,
) -> (&'static str, Value) {
    let site = asked.site();
    let at = text_document_position(&site.path, doc, site.at);
    match asked {
        meowr_app::RenameQuestion::Prepare(_) => ("textDocument/prepareRename", at),
        meowr_app::RenameQuestion::Apply(apply) => {
            let mut params = at;
            params["newName"] = json!(apply.name);
            ("textDocument/rename", params)
        }
    }
}

/// How much of renaming a server said it does when it started.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RenameSupport {
    /// No `renameProvider`: refused before a name is typed, as lsp-mode does.
    None,
    /// Renames, but does not prepare: the name is the symbol at the point.
    Rename,
    /// `prepareProvider`: asked first.
    Prepare,
}

/// A server's `renameProvider`, which is `true` or an object whose
/// `prepareProvider` says whether it prepares.
pub fn rename_support_of(capabilities: &Value) -> RenameSupport {
    match capabilities.get("renameProvider") {
        Some(Value::Bool(true)) => RenameSupport::Rename,
        Some(provider @ Value::Object(_)) => {
            match provider.get("prepareProvider").and_then(Value::as_bool) {
                Some(true) => RenameSupport::Prepare,
                _ => RenameSupport::Rename,
            }
        }
        _ => RenameSupport::None,
    }
}

/// What a server said to `textDocument/prepareRename`.
///
/// A server that does not know the request leaves the name to the client,
/// which is lsp-mode's behaviour for one that does not advertise
/// `prepareProvider`. Any other error is the server's refusal, in its words; a
/// connection that closed is meowr's failure, in meowr's.
pub fn prepared_of(reply: &Result<Value, Error>) -> meowr_app::Prepared {
    use lsp_types::PrepareRenameResponse as Response;
    use meowr_app::Prepared;
    let reply = match reply {
        Ok(reply) => reply,
        Err(Error::Server(error)) if error_code(error) == Some(METHOD_NOT_FOUND) => {
            return Prepared::Default;
        }
        Err(Error::Server(error)) => return Prepared::Refused(server_words(error)),
        Err(other) => return Prepared::Failed(other.to_string()),
    };
    if reply.is_null() {
        return Prepared::Nothing;
    }
    match serde_json::from_value(reply.clone()) {
        Ok(Response::RangeWithPlaceholder { placeholder, .. }) => Prepared::Placeholder(placeholder),
        Ok(Response::Range(range)) => Prepared::Range {
            start: line_column_of(range.start),
            end: line_column_of(range.end),
        },
        Ok(Response::DefaultBehavior { .. }) => Prepared::Default,
        Err(_) => Prepared::Failed("an answer meowr could not read".to_string()),
    }
}

/// What a server said to `textDocument/rename`: its workspace edit as read, for
/// the host to respell and read files for, or why there is none.
pub fn renamed_of(reply: &Result<Value, Error>) -> meowr_app::EditReply {
    edit_reply_of(reply, |edit| Some(edit))
}

/// What a server said to `codeAction/resolve`: the action, with its edit. One
/// that resolved to a command and no edit has nothing meowr can apply.
pub fn resolved_of(reply: &Result<Value, Error>) -> meowr_app::EditReply {
    match reply {
        Ok(action) if action.get("edit").is_none() && action.get("command").is_some() => {
            meowr_app::EditReply::Failed(
                "it resolved to a command, and meowr does not run server commands".to_string(),
            )
        }
        _ => edit_reply_of(reply, |action| action.get("edit")),
    }
}

/// A reply whose answer is a workspace edit, found in it by `edit_in`.
fn edit_reply_of(
    reply: &Result<Value, Error>,
    edit_in: impl Fn(&Value) -> Option<&Value>,
) -> meowr_app::EditReply {
    use meowr_app::EditReply;
    match reply {
        Ok(reply) => match edit_in(reply).filter(|edit| !edit.is_null()) {
            None => EditReply::Nothing,
            Some(edit) => workspace_edit_of(edit).map_or_else(
                || EditReply::Failed("an edit meowr could not read".to_string()),
                EditReply::Edit,
            ),
        },
        Err(Error::Server(error)) => EditReply::Refused(server_words(error)),
        Err(other) => EditReply::Failed(other.to_string()),
    }
}

/// What a server said it does with code actions when it started (#169).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodeActionSupport {
    /// No `codeActionProvider`: refused before anything is asked.
    None,
    /// Lists them, each with its edit or nothing to apply.
    List,
    /// `resolveProvider`: an action may come without its edit and be resolved.
    Resolve,
}

/// A server's `codeActionProvider`, which is `true` or an object whose
/// `resolveProvider` says whether it resolves.
pub fn code_action_support_of(capabilities: &Value) -> CodeActionSupport {
    match capabilities.get("codeActionProvider") {
        Some(Value::Bool(true)) => CodeActionSupport::List,
        Some(provider @ Value::Object(_)) => {
            match provider.get("resolveProvider").and_then(Value::as_bool) {
                Some(true) => CodeActionSupport::Resolve,
                _ => CodeActionSupport::List,
            }
        }
        _ => CodeActionSupport::None,
    }
}

/// `textDocument/codeAction` over the range asked about, with the diagnostics
/// on the point's line and the one kind wanted, if one is (#169).
pub fn code_actions_request(asked: &meowr_app::CodeActionsAsked, doc: &meowr_domain::Document) -> Value {
    let diagnostics: Vec<Value> = asked
        .diagnostics
        .iter()
        .map(|diagnostic| {
            json!({
                "range": range_at(doc, &diagnostic.range),
                "severity": severity_code(diagnostic.severity),
                "message": diagnostic.message,
            })
        })
        .collect();
    let mut context = json!({ "diagnostics": diagnostics });
    if let Some(only) = &asked.only {
        context["only"] = json!([only]);
    }
    json!({
        "textDocument": {"uri": uri_of(&asked.path)},
        "range": range_at(doc, &asked.range),
        "context": context,
    })
}

/// `codeAction/resolve`'s parameters: the action as the server sent it. `None`
/// for an action with nothing to resolve, or whose JSON cannot be read back.
pub fn resolve_request(action: &meowr_app::CodeAction) -> Option<Value> {
    match &action.effect {
        meowr_app::CodeActionEffect::Resolve(sent) => serde_json::from_str(sent).ok(),
        _ => None,
    }
}

/// A severity as the protocol numbers it: the other direction of
/// `diagnostic_of`'s match, on the same constants.
fn severity_code(severity: meowr_domain::Severity) -> lsp_types::DiagnosticSeverity {
    match severity {
        meowr_domain::Severity::Error => lsp_types::DiagnosticSeverity::ERROR,
        meowr_domain::Severity::Warning => lsp_types::DiagnosticSeverity::WARNING,
        meowr_domain::Severity::Information => lsp_types::DiagnosticSeverity::INFORMATION,
        meowr_domain::Severity::Hint => lsp_types::DiagnosticSeverity::HINT,
    }
}

/// A byte range as the protocol's `Range`.
fn range_at(doc: &meowr_domain::Document, range: &std::ops::Range<usize>) -> Value {
    json!({"start": position_at(doc, range.start), "end": position_at(doc, range.end)})
}

/// What a server said to `textDocument/codeAction`.
///
/// An action with its edit is that edit. One without is resolved where the
/// server resolves (`support`) and the action gives it something to resolve by
/// -- `data`, or no command, which is when lsp-mode resolves; otherwise it is
/// its command, or nothing to apply at all. A bare `Command` is a command. A
/// disabled action is left out, as lsp-mode leaves it out.
pub fn code_actions_of(
    reply: &Result<Value, Error>,
    support: CodeActionSupport,
) -> meowr_app::CodeActionsFound {
    use meowr_app::{CodeAction, CodeActionEffect, CodeActionsFound};
    let reply = match reply {
        Ok(reply) => reply,
        Err(Error::Server(error)) => return CodeActionsFound::Refused(server_words(error)),
        Err(other) => return CodeActionsFound::Failed(other.to_string()),
    };
    let Some(offered) = reply.as_array() else {
        return match reply {
            Value::Null => CodeActionsFound::Actions(Vec::new()),
            _ => CodeActionsFound::Failed("an answer meowr could not read".to_string()),
        };
    };
    let actions = offered
        .iter()
        .filter(|action| action.get("disabled").is_none())
        .filter_map(|action| {
            let title = action.get("title")?.as_str()?.to_string();
            // A bare `Command` names its command in `command`; a CodeAction
            // carries a `Command` object there.
            let command = match action.get("command") {
                Some(Value::String(name)) => Some(name.clone()),
                Some(object) => object.get("command").and_then(Value::as_str).map(str::to_string),
                None => None,
            };
            let resolvable = support == CodeActionSupport::Resolve
                && (action.get("data").is_some() || command.is_none());
            let effect = match action.get("edit").and_then(workspace_edit_of) {
                Some(changes) => CodeActionEffect::Edit(changes),
                None if matches!(action.get("command"), Some(Value::String(_))) => CodeActionEffect::Command,
                None if resolvable => CodeActionEffect::Resolve(action.to_string()),
                None if command.is_some() => CodeActionEffect::Command,
                None => CodeActionEffect::Nothing,
            };
            Some(CodeAction {
                title,
                kind: action.get("kind").and_then(Value::as_str).map(str::to_string),
                effect,
                command,
            })
        })
        .collect();
    CodeActionsFound::Actions(actions)
}

fn error_code(error: &Value) -> Option<i64> {
    error.get("code").and_then(Value::as_i64)
}

/// A JSON-RPC error in the server's words: its `message`, which is what a
/// server writes for a person to read, or the whole error where it wrote none.
fn server_words(error: &Value) -> String {
    error
        .get("message")
        .and_then(Value::as_str)
        .map_or_else(|| error.to_string(), str::to_string)
}

/// A protocol position as the domain holds one.
fn line_column_of(position: lsp_types::Position) -> meowr_domain::LineColumn {
    meowr_domain::LineColumn {
        line: position.line as usize,
        column: position.character as usize,
    }
}

fn line_edit_of(edit: &lsp_types::TextEdit) -> meowr_domain::LineEdit {
    meowr_domain::LineEdit {
        start: line_column_of(edit.range.start),
        end: line_column_of(edit.range.end),
        text: edit.new_text.clone(),
    }
}

fn place_of_location(location: &lsp_types::Location) -> Option<meowr_domain::Place> {
    place_at(location.uri.as_str(), location.range.start)
}

fn place_at(uri: &str, start: lsp_types::Position) -> Option<meowr_domain::Place> {
    Some(meowr_domain::Place::new(
        path_of(uri)?,
        start.line as usize,
        start.character as usize,
    ))
}
