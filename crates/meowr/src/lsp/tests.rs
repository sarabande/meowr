//! The connection task, driven over an in-memory pipe.
//!
//! `Connection::over` exists for this: the framing and the id table are what
//! #114 is about, and neither needs a language server installed to exercise.
//! Two tests need a real process. The one about not leaving one behind uses
//! `cat` rather than a server — nothing about a child outliving its parent
//! depends on what the child was saying. The other is gopls itself, because
//! #160 was a server waiting on an answer and no stand-in waits the way it did;
//! it skips where gopls is not installed.

use super::*;
use tokio::io::{AsyncReadExt, AsyncWriteExt, DuplexStream};

/// A stand-in server: the far end of a duplex pipe, framing by hand.
struct Stub {
    stream: DuplexStream,
    buffered: Vec<u8>,
}

impl Stub {
    /// A connection wired to a stub, with the notification channel it hands
    /// back. No handshake: `over` does not do one, which is what lets a test
    /// watch the handshake itself happen.
    fn connected() -> (Connection, mpsc::Receiver<Notification>, Stub) {
        let (ours, theirs) = tokio::io::duplex(64 * 1024);
        // Split so the connection reads what the stub writes and vice versa.
        let (read, write) = tokio::io::split(ours);
        let (connection, notifications) = Connection::over(read, write);
        (
            connection,
            notifications,
            Stub {
                stream: theirs,
                buffered: Vec::new(),
            },
        )
    }

    /// Read one framed message the client sent.
    async fn next(&mut self) -> Value {
        loop {
            if let Some(message) = self.take() {
                return message;
            }
            let mut chunk = [0u8; 4096];
            let read = self
                .stream
                .read(&mut chunk)
                .await
                .expect("the client's end is open");
            assert!(read > 0, "the client closed without sending anything");
            self.buffered.extend_from_slice(&chunk[..read]);
        }
    }

    /// One complete message out of what has arrived so far, if there is one.
    fn take(&mut self) -> Option<Value> {
        let text = String::from_utf8_lossy(&self.buffered).into_owned();
        let split = text.find("\r\n\r\n")?;
        let length: usize = text[..split]
            .lines()
            .find_map(|line| line.strip_prefix("Content-Length:"))
            .and_then(|n| n.trim().parse().ok())?;
        let body_at = split + 4;
        if self.buffered.len() < body_at + length {
            return None;
        }
        let body = self.buffered[body_at..body_at + length].to_vec();
        self.buffered.drain(..body_at + length);
        Some(serde_json::from_slice(&body).expect("the client frames valid JSON"))
    }

    async fn send(&mut self, message: Value) {
        self.stream
            .write_all(&frame(&message))
            .await
            .expect("the client's end is open");
    }

    /// Reply to request `id` with `result`.
    async fn reply(&mut self, id: i64, result: Value) {
        self.send(json!({"jsonrpc": "2.0", "id": id, "result": result}))
            .await;
    }
}

#[tokio::test]
async fn a_request_is_framed_with_its_content_length() {
    let (connection, _notifications, mut stub) = Stub::connected();
    tokio::spawn(async move {
        let _ = connection
            .request("textDocument/hover", json!({"here": true}))
            .await;
    });
    let sent = stub.next().await;
    assert_eq!(sent["method"], "textDocument/hover");
    assert_eq!(sent["jsonrpc"], "2.0");
    assert_eq!(sent["params"]["here"], true);
    assert!(sent["id"].is_i64(), "a request carries an id");
}

#[tokio::test]
async fn a_reply_reaches_the_request_that_asked_for_it() {
    let (connection, _notifications, mut stub) = Stub::connected();
    let asking =
        tokio::spawn(async move { connection.request("textDocument/hover", json!({})).await });
    let sent = stub.next().await;
    let id = sent["id"].as_i64().expect("an id");
    stub.reply(id, json!({"contents": "fn main()"})).await;

    let answer = asking.await.expect("the task finished").expect("a reply");
    assert_eq!(answer["contents"], "fn main()");
}

#[tokio::test]
async fn two_outstanding_requests_get_their_own_answers() {
    // #110's own reason for an id table rather than one channel per
    // connection: `didChange` fires on every keystroke, and a server slower
    // than the next keystroke owes several replies at once. Answered here in
    // the *reverse* order they were asked, which is the case a table gets
    // right and a queue does not.
    let (connection, _notifications, mut stub) = Stub::connected();
    let connection = Arc::new(connection);

    let first = {
        let connection = connection.clone();
        tokio::spawn(async move { connection.request("one", json!({})).await })
    };
    let first_id = stub.next().await["id"].as_i64().expect("an id");
    let second = {
        let connection = connection.clone();
        tokio::spawn(async move { connection.request("two", json!({})).await })
    };
    let second_id = stub.next().await["id"].as_i64().expect("an id");
    assert_ne!(first_id, second_id, "each request gets its own id");

    stub.reply(second_id, json!("second")).await;
    stub.reply(first_id, json!("first")).await;

    assert_eq!(
        first.await.expect("finished").expect("a reply"),
        json!("first")
    );
    assert_eq!(
        second.await.expect("finished").expect("a reply"),
        json!("second")
    );
}

#[tokio::test]
async fn a_server_error_is_reported_rather_than_read_as_an_answer() {
    let (connection, _notifications, mut stub) = Stub::connected();
    let asking =
        tokio::spawn(async move { connection.request("textDocument/hover", json!({})).await });
    let id = stub.next().await["id"].as_i64().expect("an id");
    stub.send(json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": {"code": -32601, "message": "no such method"},
    }))
    .await;

    let answer = asking.await.expect("finished");
    assert!(matches!(answer, Err(Error::Server(_))), "{answer:?}");
}

#[tokio::test]
async fn every_notification_arrives_in_order_and_none_is_dropped() {
    // #112's own finding, made into a criterion: two of three real runs got an
    // EMPTY `publishDiagnostics` first, then the real one, and once a third
    // clearing it again. A client that stopped after the first would report
    // "no problems" when the answer had not arrived yet.
    let (_connection, mut notifications, mut stub) = Stub::connected();
    let uri = "file:///tmp/lib.rs";
    for diagnostics in [json!([]), json!([{"message": "E0308"}]), json!([])] {
        stub.send(json!({
            "jsonrpc": "2.0",
            "method": "textDocument/publishDiagnostics",
            "params": {"uri": uri, "diagnostics": diagnostics},
        }))
        .await;
    }

    let mut seen = Vec::new();
    for _ in 0..3 {
        let notification = notifications.recv().await.expect("three were sent");
        assert_eq!(notification.method, "textDocument/publishDiagnostics");
        seen.push(notification.params["diagnostics"].clone());
    }
    assert_eq!(
        seen,
        vec![json!([]), json!([{"message": "E0308"}]), json!([])],
        "in arrival order, all three"
    );
}

/// What the client sent back, or a failure rather than a hang if it sent
/// nothing -- which is exactly the bug these tests are about.
async fn answered(stub: &mut Stub) -> Value {
    tokio::time::timeout(std::time::Duration::from_secs(2), stub.next())
        .await
        .expect("the client answered the server's request")
}

#[tokio::test]
async fn a_request_from_the_server_for_a_progress_token_is_answered() {
    // #160. meowr declares `window.workDoneProgress`, so a server asks for a
    // token before reporting progress -- and gopls asks during `initialized`
    // and then waits for the answer, blocking every request after it, even
    // `shutdown`. Dropping it was never what the protocol allowed.
    let (_connection, _notifications, mut stub) = Stub::connected();
    stub.send(json!({
        "jsonrpc": "2.0",
        "id": 7,
        "method": "window/workDoneProgress/create",
        "params": {"token": "gopls-1"},
    }))
    .await;

    let answer = answered(&mut stub).await;
    assert_eq!(answer["id"], json!(7), "answered under the server's own id");
    assert_eq!(answer["result"], Value::Null);
    assert!(answer.get("error").is_none(), "{answer}");
}

#[tokio::test]
async fn gopls_still_answers_once_it_has_asked_for_a_progress_token() {
    // The real server #160 was found against, through the real `Connection`:
    // gopls asks for a token during `initialized` and, unanswered, answered
    // nothing after it -- not even `shutdown`. Skipped where gopls is not
    // installed, as the `cat` test above skips without `cat`: its absence is
    // not a meowr bug.
    let at = std::env::temp_dir().join(format!("meowr-gopls-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&at);
    std::fs::create_dir_all(&at).unwrap();
    std::fs::write(at.join("go.mod"), "module example.com/probe\n\ngo 1.21\n").unwrap();
    std::fs::write(at.join("main.go"), "package main\n\nfunc main() {}\n").unwrap();

    let spawned = tokio::time::timeout(
        std::time::Duration::from_secs(30),
        Connection::spawn("gopls", &[], &at),
    )
    .await;
    let (connection, _notifications) = match spawned {
        Ok(Ok(connected)) => connected,
        // Only a gopls that could not be started is skipped. A handshake that
        // failed or never ended is the thing under test, and fails it.
        Ok(Err(Error::Spawn(_))) => {
            let _ = std::fs::remove_dir_all(&at);
            return;
        }
        other => {
            let _ = std::fs::remove_dir_all(&at);
            panic!("gopls did not finish the handshake: {:?}", other.map(|r| r.err()));
        }
    };
    let shutdown = tokio::time::timeout(
        std::time::Duration::from_secs(30),
        connection.request("shutdown", Value::Null),
    )
    .await;
    let _ = std::fs::remove_dir_all(&at);
    assert!(
        matches!(shutdown, Ok(Ok(_))),
        "gopls answered shutdown: {shutdown:?}"
    );
}

#[tokio::test]
async fn a_request_from_the_server_nothing_here_can_answer_is_refused_out_loud() {
    // Refused with `MethodNotFound` rather than left hanging: a server told
    // "cannot" can carry on, and a server told nothing may wait for ever. Ids
    // may be strings, and an answer under a different id answers nothing.
    let (_connection, _notifications, mut stub) = Stub::connected();
    stub.send(json!({
        "jsonrpc": "2.0",
        "id": "req-3",
        "method": "workspace/applyEdit",
        "params": {"edit": {}},
    }))
    .await;

    let answer = answered(&mut stub).await;
    assert_eq!(answer["id"], json!("req-3"));
    assert_eq!(answer["error"]["code"], json!(-32601));
    assert!(answer.get("result").is_none(), "{answer}");
}

#[tokio::test]
async fn a_notification_is_not_mistaken_for_a_reply() {
    // The two are told apart by `id`. A notification landing in the reply
    // table would resolve whatever request happened to be outstanding with
    // somebody else's content.
    let (connection, mut notifications, mut stub) = Stub::connected();
    let asking = tokio::spawn(async move { connection.request("hover", json!({})).await });
    let id = stub.next().await["id"].as_i64().expect("an id");

    stub.send(json!({
        "jsonrpc": "2.0",
        "method": "window/logMessage",
        "params": {"message": "loading"},
    }))
    .await;
    let notification = notifications.recv().await.expect("the notification");
    assert_eq!(notification.method, "window/logMessage");

    stub.reply(id, json!("the real answer")).await;
    assert_eq!(
        asking.await.expect("finished").expect("a reply"),
        json!("the real answer")
    );
}

#[tokio::test]
async fn a_body_containing_a_blank_line_is_read_by_its_length() {
    // The header/body split is `\r\n\r\n` and a JSON string may hold one. This
    // is why the body is read by exact byte count rather than scanned for a
    // terminator — a hover's markdown routinely contains blank lines.
    let (_connection, mut notifications, mut stub) = Stub::connected();
    stub.send(json!({
        "jsonrpc": "2.0",
        "method": "textDocument/publishDiagnostics",
        "params": {"message": "one\r\n\r\ntwo"},
    }))
    .await;
    let notification = notifications.recv().await.expect("it was read whole");
    assert_eq!(notification.params["message"], "one\r\n\r\ntwo");
}

#[tokio::test]
async fn a_server_that_goes_away_fails_what_was_waiting_rather_than_hanging() {
    let (connection, _notifications, stub) = Stub::connected();
    let asking = tokio::spawn(async move { connection.request("hover", json!({})).await });
    drop(stub);
    let answer = asking.await.expect("finished");
    assert!(matches!(answer, Err(Error::Closed)), "{answer:?}");
}

#[tokio::test]
async fn the_handshake_completes_before_anything_else_crosses() {
    // #114's own criterion. `spawn` awaits this rather than leaving it to the
    // caller, so a request cannot go out ahead of `initialize`.
    let (connection, _notifications, mut stub) = Stub::connected();
    let root = std::path::PathBuf::from("/tmp/somewhere");
    let shaking = tokio::spawn(async move { connection.handshake(&root).await });

    let initialize = stub.next().await;
    assert_eq!(initialize["method"], "initialize");
    assert_eq!(initialize["params"]["rootUri"], "file:///tmp/somewhere");
    let id = initialize["id"].as_i64().expect("an id");
    stub.reply(id, json!({"capabilities": {}})).await;

    let initialized = stub.next().await;
    assert_eq!(initialized["method"], "initialized");
    assert!(initialized.get("id").is_none(), "a notification has no id");
    shaking.await.expect("finished").expect("the handshake");
}

#[tokio::test]
async fn dropping_a_connection_kills_the_child_rather_than_leaking_it() {
    // `cat` rather than a language server: nothing about a child outliving its
    // parent depends on what the child was saying, and a test that needed
    // rust-analyzer installed would be a test that does not run on CI.
    // `Connection::started` rather than a second `Command` builder of this
    // test's own: the flag under test is `kill_on_drop`, and a copy of the
    // builder could keep passing while the real path lost it.
    let Ok((connection, _notifications)) =
        Connection::started("cat", &[], &std::path::PathBuf::from("."))
    else {
        // No `cat` is not a meowr bug; skip rather than fail the suite.
        return;
    };
    let pid = connection
        ._child
        .as_ref()
        .and_then(|c| c.id())
        .expect("a running child has a pid");
    assert!(alive(pid), "it started");

    drop(connection);
    // `kill_on_drop` reaps on the runtime, so the kill is not synchronous with
    // the drop. Polled rather than slept on once, so a slow machine does not
    // make this flaky.
    for _ in 0..100 {
        if !alive(pid) {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    panic!("the child outlived its connection");
}

/// Whether a pid is still running, asked of the process table directly.
fn alive(pid: u32) -> bool {
    std::path::Path::new(&format!("/proc/{pid}")).exists()
}

// --- the conversions (#117, #118) -----------------------------------------

use meowr_domain::{Document, Severity};

#[test]
fn did_open_carries_the_language_across_as_the_lsp_language_id() {
    // Behavioural rather than a tautology over an identity function: what
    // matters is that whatever Language the session names reaches the wire as
    // `languageId`, for any language and not just Rust.
    for language in ["rust", "go", "python"] {
        let message = did_open("/tmp/f", language, 1, "");
        assert_eq!(message["textDocument"]["languageId"], language);
    }
}

#[test]
fn a_path_survives_the_round_trip_through_a_uri() {
    let path = "/home/user/tools/meow/crates/meowr-domain/src/lib.rs";
    assert_eq!(path_of(&uri_of(path)), Some(path));
}

#[test]
fn something_that_is_not_a_file_uri_is_not_taken_for_a_path() {
    assert_eq!(path_of("untitled:Untitled-1"), None);
}

#[test]
fn a_point_becomes_the_position_the_protocol_names_it_by() {
    let doc = Document::from_text("one\ntwo\n");
    assert_eq!(position_at(&doc, 0), json!({"line": 0, "character": 0}));
    assert_eq!(position_at(&doc, 6), json!({"line": 1, "character": 2}));
    let doc = Document::from_text("🦀x\n");
    assert_eq!(
        position_at(&doc, 4),
        json!({"line": 0, "character": 2}),
        "counted in UTF-16 units on the way out too"
    );
}

#[test]
fn a_published_diagnostic_becomes_one_a_buffer_can_hold() {
    let doc = Document::from_text("let x: i32 = \"no\";\n");
    let params = json!({
        "uri": "file:///tmp/lib.rs",
        "version": 3,
        "diagnostics": [{
            "range": {
                "start": {"line": 0, "character": 13},
                "end": {"line": 0, "character": 17},
            },
            "severity": 1,
            "message": "expected i32, found &'static str",
        }],
    });
    assert_eq!(uri_in(&params), Some("file:///tmp/lib.rs"));
    assert_eq!(version_of(&params), Some(3));

    let all = diagnostics_of(&params, &doc);
    assert_eq!(all.len(), 1);
    assert_eq!(all[0].range, 13..17);
    assert_eq!(all[0].severity, Severity::Error);
    assert_eq!(all[0].message, "expected i32, found &'static str");
}

#[test]
fn the_four_severities_are_carried_across() {
    let doc = Document::from_text("abcdef\n");
    for (code, severity) in [
        (1, Severity::Error),
        (2, Severity::Warning),
        (3, Severity::Information),
        (4, Severity::Hint),
    ] {
        let params = json!({"uri": "file:///tmp/lib.rs", "diagnostics": [{
            "range": {
                "start": {"line": 0, "character": 0},
                "end": {"line": 0, "character": 3},
            },
            "severity": code,
            "message": "something",
        }]});
        assert_eq!(diagnostics_of(&params, &doc)[0].severity, severity);
    }
}

#[test]
fn a_zero_width_diagnostic_is_still_given_something_to_draw_under() {
    // A missing semicolon has no characters to underline, and a run of zero
    // columns is a squiggle nobody sees.
    let doc = Document::from_text("let x = 1\n");
    let params = json!({"uri": "file:///tmp/lib.rs", "diagnostics": [{
        "range": {
            "start": {"line": 0, "character": 9},
            "end": {"line": 0, "character": 9},
        },
        "severity": 1,
        "message": "expected `;`",
    }]});
    let all = diagnostics_of(&params, &doc);
    assert!(all[0].range.end > all[0].range.start, "{:?}", all[0].range);
}

#[test]
fn an_empty_publish_yields_no_diagnostics_rather_than_failing() {
    // #112's own finding: two of three real runs answered with an empty array
    // first. That is an answer, not a malformed message.
    let doc = Document::from_text("fine\n");
    let params = json!({"uri": "file:///tmp/lib.rs", "diagnostics": []});
    assert!(diagnostics_of(&params, &doc).is_empty());
}

#[test]
fn a_hover_is_read_out_of_all_three_shapes_the_protocol_allows() {
    assert_eq!(
        hover_text(&json!({"contents": {"kind": "markdown", "value": "fn main()"}})),
        Some("fn main()".to_string())
    );
    assert_eq!(
        hover_text(&json!({"contents": "fn main()"})),
        Some("fn main()".to_string())
    );
    // A `MarkedString` object is `{language, value}` — both, per the protocol.
    // `lsp_types` refuses one missing `language`, which is stricter than the
    // hand-rolled reader this replaced and is the point of using it.
    assert_eq!(
        hover_text(&json!({"contents": [
            {"language": "rust", "value": "fn main()"},
            "Runs it.",
        ]})),
        Some("fn main()\nRuns it.".to_string())
    );
}

#[test]
fn a_hovers_code_fences_are_stripped_but_the_code_is_kept() {
    // rust-analyzer answers in markdown. The Echo Area cannot render a fence,
    // so it would show as literal backticks above the thing asked about.
    let reply = json!({"contents": {
        "kind": "markdown",
        "value": "```rust\nfn main()\n```\n\nRuns it.",
    }});
    assert_eq!(
        hover_text(&reply),
        Some("fn main()\n\nRuns it.".to_string())
    );
}

#[test]
fn a_hover_with_nothing_in_it_is_no_hover() {
    // rust-analyzer answers `{"contents": {"value": ""}}` over whitespace, and
    // a hover reserving rows to say nothing is worse than none.
    assert!(hover_text(&json!({"contents": {"kind": "markdown", "value": ""}})).is_none());
    assert!(hover_text(&json!({"contents": "```\n```"})).is_none());
    assert!(hover_text(&Value::Null).is_none());
    // A reply meowr cannot parse is no hover either, rather than a panic: the
    // shape is the server's choice and this one is not in the protocol.
    assert!(hover_text(&json!({"contents": {"value": "no kind"}})).is_none());
}

#[test]
fn did_open_names_the_file_its_language_and_its_version() {
    let message = did_open("/tmp/lib.rs", "rust", 7, "fn main() {}\n");
    assert_eq!(message["textDocument"]["uri"], "file:///tmp/lib.rs");
    assert_eq!(message["textDocument"]["languageId"], "rust");
    assert_eq!(message["textDocument"]["version"], 7);
    assert_eq!(message["textDocument"]["text"], "fn main() {}\n");
}

#[test]
fn did_change_sends_the_whole_text_under_the_new_version() {
    let message = did_change("/tmp/lib.rs", 8, "fn main() { }\n");
    assert_eq!(message["textDocument"]["version"], 8);
    assert_eq!(message["contentChanges"][0]["text"], "fn main() { }\n");
    assert!(
        message["contentChanges"][0].get("range").is_none(),
        "a full-text change carries no range"
    );
}

#[test]
fn a_hover_request_names_the_point_it_is_asking_about() {
    let doc = Document::from_text("one\ntwo\n");
    let message = hover_at("/tmp/lib.rs", &doc, 6);
    assert_eq!(message["textDocument"]["uri"], "file:///tmp/lib.rs");
    assert_eq!(message["position"], json!({"line": 1, "character": 2}));
}

#[test]
fn params_that_are_not_a_publish_yield_no_diagnostics_rather_than_a_guess() {
    // The server said something meowr does not understand. Placing no squiggle
    // is the honest answer; placing one somewhere arbitrary is not.
    let doc = Document::from_text("abc\n");
    assert!(diagnostics_of(&json!({"nothing": "useful"}), &doc).is_empty());
    assert!(diagnostics_of(&Value::Null, &doc).is_empty());
}

#[test]
fn a_relative_path_becomes_an_absolute_uri() {
    // The protocol's URIs are absolute and meowr's paths are whatever was
    // typed. `file://crates/…` names a host called `crates`: a real server
    // accepts it, answers nothing, and nothing on screen says why.
    let uri = uri_of("crates/meowr/src/lsp.rs");
    assert!(uri.starts_with("file:///"), "{uri}");
    assert!(uri.ends_with("/crates/meowr/src/lsp.rs"), "{uri}");
}

#[test]
fn two_spellings_of_one_path_match_once_both_are_absolute() {
    let typed = "crates/meowr/src/lsp.rs";
    let uri = uri_of(typed);
    let announced = path_of(&uri).expect("a file URI");
    assert_eq!(absolute(typed), absolute(announced));
}

#[test]
fn a_references_reply_becomes_places_with_no_text() {
    // A server answers with positions and nothing else, so these rows show
    // `path:line` -- which is what `Place::text` being optional is for.
    let reply = json!([
        {"uri": "file:///w/src/lib.rs",
         "range": {"start": {"line": 41, "character": 8},
                   "end": {"line": 41, "character": 14}}},
        {"uri": "file:///w/src/narrow.rs",
         "range": {"start": {"line": 7, "character": 0},
                   "end": {"line": 7, "character": 6}}},
    ]);
    let places = places_of(&reply);
    assert_eq!(places.len(), 2);
    assert_eq!(places[0].path, "/w/src/lib.rs");
    assert_eq!((places[0].line, places[0].column), (41, 8));
    assert_eq!(places[0].text, None, "a server gives no line to show");
}

#[test]
fn a_definition_reply_in_any_of_its_three_shapes_becomes_places() {
    // `Location | Location[] | LocationLink[]` (#146). A reader that took the
    // references shape alone would find nothing in the first and the third,
    // and say "Nothing found." about a definition the server had given.
    let one = json!(
        {"uri": "file:///w/src/lib.rs",
         "range": {"start": {"line": 3, "character": 4},
                   "end": {"line": 3, "character": 9}}});
    let places = places_of(&one);
    assert_eq!(places.len(), 1, "a bare Location");
    assert_eq!((places[0].line, places[0].column), (3, 4));

    // A link names the whole node as `targetRange` and the name inside it as
    // `targetSelectionRange`. rust-analyzer's `targetRange` for a function
    // starts at its doc comment, so the jump belongs on the selection.
    let links = json!([
        {"targetUri": "file:///w/src/narrow.rs",
         "targetRange": {"start": {"line": 10, "character": 0},
                         "end": {"line": 20, "character": 1}},
         "targetSelectionRange": {"start": {"line": 12, "character": 7},
                                  "end": {"line": 12, "character": 13}}},
    ]);
    let places = places_of(&links);
    assert_eq!(places.len(), 1, "a LocationLink");
    assert_eq!(places[0].path, "/w/src/narrow.rs");
    assert_eq!((places[0].line, places[0].column), (12, 7), "on the name");
}

#[test]
fn each_lookup_asks_the_method_doom_s_row_means() {
    use meowr_app::LookupKind;
    let doc = Document::from_text("one\ntwo\n");
    for (kind, method) in [
        (LookupKind::References, "textDocument/references"),
        (LookupKind::Definition, "textDocument/definition"),
        (LookupKind::TypeDefinition, "textDocument/typeDefinition"),
        (LookupKind::Implementations, "textDocument/implementation"),
    ] {
        let (asked, request) =
            lookup_request(&kind, "src/lib.rs", &doc, 4).expect("a place lookup");
        assert_eq!(asked, method);
        assert_eq!(request["position"], json!({"line": 1, "character": 0}));
    }
    let symbols = LookupKind::Symbols {
        query: "W".into(),
        everywhere: false,
    };
    assert!(
        lookup_request(&symbols, "src/lib.rs", &doc, 4).is_none(),
        "a symbol lookup is about a string, not a place, and is asked elsewhere"
    );
}

#[test]
fn a_reply_that_is_not_a_list_is_no_places_rather_than_a_panic() {
    // `null` is what a server sends for "nothing", and it is not an error.
    assert!(places_of(&json!(null)).is_empty());
}

#[test]
fn a_references_request_asks_for_the_declaration_too() {
    // Doom's `+lookup/references` and `xref`'s own behaviour: the definition is
    // one of the places the thing is.
    let doc = Document::from_text("one\ntwo\n");
    let request = references_at("src/lib.rs", &doc, 4);
    assert_eq!(request["context"]["includeDeclaration"], json!(true));
    assert_eq!(request["position"], json!({"line": 1, "character": 0}));
}

// --- workspace/symbol (#144) ----------------------------------------------

#[test]
fn a_symbol_reply_carries_the_name_rather_than_just_a_position() {
    // What a references reply cannot give. A `SymbolInformation` has a `name`,
    // so a row reads as the symbol instead of as `path:line` -- the optional
    // `text` on a Place earning its keep in the other direction from grep.
    let reply = serde_json::json!([
        {
            "name": "Trail",
            "kind": 23,
            "location": {
                "uri": "file:///w/crates/meowr-core/src/edit.rs",
                "range": {"start": {"line": 369, "character": 11},
                          "end": {"line": 369, "character": 16}}
            }
        },
        {
            "name": "push",
            "kind": 6,
            "containerName": "Trail",
            "location": {
                "uri": "file:///w/crates/meowr-core/src/edit.rs",
                "range": {"start": {"line": 391, "character": 11},
                          "end": {"line": 391, "character": 15}}
            }
        }
    ]);

    let places = symbols_of(&reply);

    assert_eq!(places.len(), 2);
    assert_eq!(places[0].text.as_deref(), Some("Trail"));
    assert_eq!(places[0].line, 369);
    assert_eq!(places[0].path, "/w/crates/meowr-core/src/edit.rs");
    assert_eq!(
        places[1].text.as_deref(),
        Some("Trail::push"),
        "named with its container, because two `new`s are told apart by what \
         they are on"
    );
}

#[test]
fn a_query_goes_out_with_the_request() {
    // The whole point of #144. Asked with an empty query, rust-analyzer answers
    // with a truncated list dominated by dependency crates.
    assert_eq!(workspace_symbol("Trail"), serde_json::json!({"query": "Trail"}));
}

#[test]
fn a_reply_that_is_not_a_symbol_list_yields_no_places() {
    // A server that answers `null`, which the protocol allows. Nothing rather
    // than a guess, which is `diagnostics_of`'s rule in this module already.
    assert!(symbols_of(&serde_json::Value::Null).is_empty());
}

// --- a workspace edit (#167) --------------------------------------------------

/// A file's edits as `path` and `line:column-line:column text` for each.
fn edited(change: &meowr_app::FileChange) -> (&str, Vec<String>) {
    let meowr_app::FileChange::Edit(file) = change else {
        panic!("not an edit: {change:?}");
    };
    assert_eq!(file.read, None, "reading is the host's, after this");
    let edits = file
        .edits
        .iter()
        .map(|e| {
            let (s, t) = (e.start, e.end);
            format!("{}:{}-{}:{} {}", s.line, s.column, t.line, t.column, e.text)
        })
        .collect();
    (file.path.as_str(), edits)
}

#[test]
fn a_workspace_edit_in_the_changes_form_becomes_one_edit_per_file_in_path_order() {
    // A map, so the server's order is not there to keep; path order is at
    // least the same every time.
    let reply = json!({"changes": {
        "file:///w/src/main.rs": [
            {"range": {"start": {"line": 1, "character": 4}, "end": {"line": 1, "character": 7}},
             "newText": "new"}],
        "file:///w/src/lib.rs": [
            {"range": {"start": {"line": 0, "character": 3}, "end": {"line": 0, "character": 6}},
             "newText": "new"}],
    }});
    let changes = workspace_edit_of(&reply).expect("an edit");
    assert_eq!(changes.len(), 2);
    assert_eq!(edited(&changes[0]), ("/w/src/lib.rs", vec!["0:3-0:6 new".to_string()]));
    assert_eq!(edited(&changes[1]), ("/w/src/main.rs", vec!["1:4-1:7 new".to_string()]));
}

#[test]
fn a_workspace_edit_in_the_document_changes_form_keeps_its_order_and_annotated_edits() {
    // rust-analyzer's shape (#157): `documentChanges` with a null version for a
    // file it has not been told about. The version is not what is checked --
    // ADR-0013 stamps the buffer's revision instead.
    let reply = json!({"documentChanges": [
        {"textDocument": {"uri": "file:///w/src/main.rs", "version": 4},
         "edits": [{"range": {"start": {"line": 1, "character": 4}, "end": {"line": 1, "character": 7}},
                    "newText": "new"}]},
        {"textDocument": {"uri": "file:///w/src/lib.rs", "version": null},
         "edits": [{"range": {"start": {"line": 0, "character": 3}, "end": {"line": 0, "character": 6}},
                    "newText": "new", "annotationId": "rename"}]},
    ]});
    let changes = workspace_edit_of(&reply).expect("an edit");
    assert_eq!(edited(&changes[0]).0, "/w/src/main.rs", "the server's order");
    assert_eq!(edited(&changes[1]), ("/w/src/lib.rs", vec!["0:3-0:6 new".to_string()]));
}

#[test]
fn resource_operations_in_a_workspace_edit_are_kept_for_the_session_to_refuse() {
    let reply = json!({"documentChanges": [
        {"textDocument": {"uri": "file:///w/src/lib.rs", "version": null},
         "edits": [{"range": {"start": {"line": 0, "character": 4}, "end": {"line": 0, "character": 7}},
                    "newText": "new"}]},
        {"kind": "rename", "oldUri": "file:///w/src/old.rs", "newUri": "file:///w/src/new.rs"},
        {"kind": "create", "uri": "file:///w/src/made.rs"},
        {"kind": "delete", "uri": "file:///w/src/gone.rs"},
    ]});
    let changes = workspace_edit_of(&reply).expect("an edit");
    assert_eq!(
        changes[1..],
        [
            meowr_app::FileChange::Rename {
                from: "/w/src/old.rs".into(),
                to: "/w/src/new.rs".into()
            },
            meowr_app::FileChange::Create {
                path: "/w/src/made.rs".into()
            },
            meowr_app::FileChange::Delete {
                path: "/w/src/gone.rs".into()
            },
        ]
    );
}

#[test]
fn a_null_or_unreadable_workspace_edit_is_none() {
    assert_eq!(workspace_edit_of(&Value::Null), None);
    assert_eq!(workspace_edit_of(&json!({"changes": 3})), None);
}

// --- rename (#168) ------------------------------------------------------------

fn site(at: usize) -> meowr_app::RenameSite {
    meowr_app::RenameSite {
        path: "/w/main.rs".into(),
        buffer: meowr_app::buffer::BufferId(0),
        at,
        revision: 1,
    }
}

#[test]
fn a_rename_asks_at_the_point_and_then_with_the_new_name() {
    let doc = Document::from_text("é old();\n");
    let prepare = meowr_app::RenameQuestion::Prepare(site(3));
    let (method, params) = rename_request(&prepare, &doc);
    assert_eq!(method, "textDocument/prepareRename");
    assert_eq!(params["textDocument"]["uri"], "file:///w/main.rs");
    assert_eq!(params["position"], json!({"line": 0, "character": 2}), "UTF-16");

    let rename = meowr_app::RenameQuestion::Apply(meowr_app::RenameApply {
        site: site(3),
        name: "new".into(),
        asked: meowr_app::Revisions::default(),
    });
    let (method, params) = rename_request(&rename, &doc);
    assert_eq!(method, "textDocument/rename");
    assert_eq!(params["position"], json!({"line": 0, "character": 2}));
    assert_eq!(params["newName"], "new");
}

#[test]
fn what_a_server_can_rename_is_read_from_what_it_advertised() {
    assert_eq!(rename_support_of(&json!({})), RenameSupport::None);
    assert_eq!(rename_support_of(&json!({"renameProvider": false})), RenameSupport::None);
    assert_eq!(rename_support_of(&json!({"renameProvider": true})), RenameSupport::Rename);
    assert_eq!(
        rename_support_of(&json!({"renameProvider": {"prepareProvider": true}})),
        RenameSupport::Prepare
    );
    assert_eq!(rename_support_of(&json!({"renameProvider": {}})), RenameSupport::Rename);
}

#[test]
fn a_prepare_rename_reply_in_each_of_its_shapes() {
    use meowr_app::Prepared;
    let range = json!({"start": {"line": 1, "character": 4}, "end": {"line": 1, "character": 7}});
    let at = |line, column| meowr_domain::LineColumn { line, column };
    assert_eq!(
        prepared_of(&Ok(json!({"range": range, "placeholder": "old"}))),
        Prepared::Placeholder("old".into())
    );
    assert_eq!(
        prepared_of(&Ok(range.clone())),
        Prepared::Range { start: at(1, 4), end: at(1, 7) }
    );
    assert_eq!(prepared_of(&Ok(json!({"defaultBehavior": true}))), Prepared::Default);
    assert_eq!(prepared_of(&Ok(Value::Null)), Prepared::Nothing);
    assert_eq!(
        prepared_of(&Err(Error::Server(json!({"code": -32602, "message": "No references found at position"})))),
        Prepared::Refused("No references found at position".into())
    );
    // A server that does not know the request leaves the name to the client,
    // which is what lsp-mode does when `prepareProvider` is not advertised.
    assert_eq!(
        prepared_of(&Err(Error::Server(json!({"code": -32601, "message": "unknown request"})))),
        Prepared::Default
    );
    assert_eq!(
        prepared_of(&Err(Error::Closed)),
        Prepared::Failed("the language server connection is closed".into()),
        "meowr's failure, not the server's words"
    );
}

#[test]
fn a_rename_reply_is_an_edit_nothing_or_a_refusal() {
    use meowr_app::EditReply;
    let edit = json!({"changes": {"file:///w/lib.rs": [
        {"range": {"start": {"line": 0, "character": 3}, "end": {"line": 0, "character": 6}},
         "newText": "new"}]}});
    let EditReply::Edit(changes) = renamed_of(&Ok(edit)) else {
        panic!("an edit");
    };
    assert_eq!(edited(&changes[0]).0, "/w/lib.rs");
    assert_eq!(renamed_of(&Ok(Value::Null)), EditReply::Nothing);
    assert_eq!(
        renamed_of(&Err(Error::Server(json!({"code": -32803, "message": "Client does not support rename capability"})))),
        EditReply::Refused("Client does not support rename capability".into())
    );
    assert_eq!(
        renamed_of(&Err(Error::Closed)),
        EditReply::Failed("the language server connection is closed".into())
    );
    assert_eq!(
        renamed_of(&Ok(json!({"changes": 3}))),
        EditReply::Failed("an edit meowr could not read".into())
    );
}

// --- code actions (#169) --------------------------------------------------------

fn actions_asked(only: Option<&str>) -> meowr_app::CodeActionsAsked {
    meowr_app::CodeActionsAsked {
        path: "/w/main.rs".into(),
        buffer: meowr_app::buffer::BufferId(0),
        range: 0..13,
        revision: 1,
        diagnostics: vec![meowr_domain::Diagnostic {
            range: 4..12,
            message: "unused import: `std::fmt`".into(),
            severity: Severity::Warning,
        }],
        only: only.map(str::to_string),
        asked: meowr_app::Revisions::default(),
    }
}

#[test]
fn code_actions_are_asked_over_the_range_with_the_lines_diagnostics() {
    let doc = Document::from_text("use std::fmt;\nfn main() {}\n");
    let params = code_actions_request(&actions_asked(None), &doc);
    assert_eq!(params["textDocument"]["uri"], "file:///w/main.rs");
    assert_eq!(
        params["range"],
        json!({"start": {"line": 0, "character": 0}, "end": {"line": 0, "character": 13}})
    );
    assert_eq!(
        params["context"]["diagnostics"],
        json!([{
            "range": {"start": {"line": 0, "character": 4}, "end": {"line": 0, "character": 12}},
            "severity": 2,
            "message": "unused import: `std::fmt`",
        }])
    );
    assert_eq!(params["context"].get("only"), None);

    let params = code_actions_request(&actions_asked(Some("source.organizeImports")), &doc);
    assert_eq!(params["context"]["only"], json!(["source.organizeImports"]));
}

#[test]
fn a_chosen_action_that_needs_resolving_is_sent_back_as_it_came() {
    let sent = json!({"title": "Merge imports", "kind": "refactor.rewrite", "data": {"id": 7}});
    let action = meowr_app::CodeAction {
        title: "Merge imports".into(),
        kind: Some("refactor.rewrite".into()),
        effect: meowr_app::CodeActionEffect::Resolve(sent.to_string()),
        command: None,
    };
    assert_eq!(resolve_request(&action), Some(sent));
}

#[test]
fn what_a_server_can_do_with_code_actions_is_read_from_what_it_advertised() {
    assert_eq!(code_action_support_of(&json!({})), CodeActionSupport::None);
    assert_eq!(code_action_support_of(&json!({"codeActionProvider": true})), CodeActionSupport::List);
    assert_eq!(
        code_action_support_of(&json!({"codeActionProvider": {"resolveProvider": true}})),
        CodeActionSupport::Resolve
    );
}

#[test]
fn a_code_action_reply_in_each_of_its_shapes() {
    use meowr_app::{CodeActionEffect, CodeActionsFound};
    // rust-analyzer's (#157): a rustc fix with its edit inline as `changes`,
    // the rest carrying only `data`. gopls's command, and a disabled action.
    let reply = json!([
        {"title": "Remove unused import", "kind": "quickfix",
         "edit": {"changes": {"file:///w/main.rs": [
             {"range": {"start": {"line": 0, "character": 0}, "end": {"line": 1, "character": 0}},
              "newText": ""}]}}},
        {"title": "Merge imports", "kind": "refactor.rewrite", "data": {"id": 7}},
        {"title": "Extract variable", "command": "gopls.apply_fix"},
        {"title": "Inline", "kind": "refactor.inline", "command": {"title": "Inline", "command": "x.inline"}},
        {"title": "Not here", "kind": "refactor", "disabled": {"reason": "no"}, "data": {}},
    ]);
    let CodeActionsFound::Actions(actions) = code_actions_of(&Ok(reply.clone()), CodeActionSupport::Resolve) else {
        panic!("actions");
    };
    let titles: Vec<&str> = actions.iter().map(|a| a.title.as_str()).collect();
    assert_eq!(titles, ["Remove unused import", "Merge imports", "Extract variable", "Inline"]);
    assert!(matches!(&actions[0].effect, CodeActionEffect::Edit(changes) if edited(&changes[0]).0 == "/w/main.rs"));
    assert_eq!(actions[0].kind.as_deref(), Some("quickfix"));
    let CodeActionEffect::Resolve(sent) = &actions[1].effect else {
        panic!("resolvable");
    };
    assert_eq!(serde_json::from_str::<Value>(sent).unwrap()["data"], json!({"id": 7}));
    assert_eq!(actions[2].effect, CodeActionEffect::Command);
    assert_eq!(actions[2].command.as_deref(), Some("gopls.apply_fix"));
    assert_eq!(actions[3].effect, CodeActionEffect::Command);
    assert_eq!(actions[3].command.as_deref(), Some("x.inline"));

    // A server that cannot resolve leaves an action with no edit nothing to
    // apply -- which is not the same as its being a command.
    let CodeActionsFound::Actions(actions) = code_actions_of(&Ok(reply), CodeActionSupport::List) else {
        panic!("actions");
    };
    assert_eq!(actions[1].effect, CodeActionEffect::Nothing);

    assert_eq!(
        code_actions_of(&Ok(Value::Null), CodeActionSupport::Resolve),
        CodeActionsFound::Actions(Vec::new())
    );
    assert_eq!(
        code_actions_of(&Err(Error::Closed), CodeActionSupport::Resolve),
        CodeActionsFound::Failed("the language server connection is closed".into())
    );
}

#[test]
fn a_resolved_action_is_its_edit_or_nothing() {
    use meowr_app::EditReply;
    let resolved = json!({"title": "Merge imports", "edit": {"documentChanges": [
        {"textDocument": {"uri": "file:///w/main.rs", "version": null},
         "edits": [{"range": {"start": {"line": 0, "character": 0}, "end": {"line": 0, "character": 3}},
                    "newText": "pub use"}]}]}});
    let EditReply::Edit(changes) = resolved_of(&Ok(resolved)) else {
        panic!("an edit");
    };
    assert_eq!(edited(&changes[0]), ("/w/main.rs", vec!["0:0-0:3 pub use".to_string()]));
    assert_eq!(resolved_of(&Ok(json!({"title": "Merge imports"}))), EditReply::Nothing);
    assert_eq!(
        resolved_of(&Ok(json!({"title": "Tidy", "command": {"title": "Tidy", "command": "gopls.tidy"}}))),
        EditReply::Failed("it resolved to a command, and meowr does not run server commands".into()),
    );
    assert_eq!(
        resolved_of(&Err(Error::Server(json!({"code": -32801, "message": "content modified"})))),
        EditReply::Refused("content modified".into())
    );
}
