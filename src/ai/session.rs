//! Background agent loop: model request → tool execution → repeat, capped,
//! reporting progress to the UI thread over an mpsc channel (polled once per
//! egui frame, same pattern as the loader and diff threads).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::Arc;

use serde_json::Value;

use crate::index::JsonIndex;

use super::provider::{self, ProviderConfig};
use super::tools::{self, ProposedEdit};

/// Cap on model↔tool rounds per user turn.
const MAX_ROUNDS: usize = 25;

/// A transcript entry, as rendered in the chat panel.
#[derive(Clone)]
pub enum ChatEntry {
    User(String),
    Assistant(String),
    /// A one-line note about a tool call ("searched for …").
    Note(String),
    Error(String),
}

/// Messages from the agent thread to the UI.
pub enum AiMsg {
    Assistant(String),
    ToolNote(String),
    Proposal(Vec<ProposedEdit>),
    Error(String),
    /// The turn finished; `history` is the full provider-native message list
    /// (replaces the App's copy so the next turn continues the conversation).
    Done { history: Vec<Value> },
}

fn system_prompt(file_name: &str) -> String {
    format!(
        "You are an assistant embedded in Quick JSON Viewer, a JSON viewer/editor. \
         The user has the document `{file_name}` open. You cannot see the document \
         directly — explore it with the tools: call get_schema first to learn the \
         structure, then get_value/search to answer questions. Values returned by \
         tools are capped in size; narrow your queries rather than fetching huge \
         subtrees.\n\
         To change the document, call propose_edits — edits are shown to the user \
         as a reviewable changeset, never applied automatically. For bulk edits, \
         first use search/get_value to find every affected node, then propose one \
         edit per node in a single propose_edits call.\n\
         Paths use the form $.key.nested[0] or $[\"odd key\"]. Keep answers concise; \
         cite the paths you looked at."
    )
}

/// Run one user turn on a background thread. `history` already contains the
/// new user message. Returns the channel the UI polls.
pub fn spawn_turn(
    cfg: ProviderConfig,
    index: Arc<JsonIndex>,
    file_name: String,
    mut history: Vec<Value>,
    cancel: Arc<AtomicBool>,
) -> Receiver<AiMsg> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        run_turn(&cfg, &index, &file_name, &mut history, &cancel, &tx);
        let _ = tx.send(AiMsg::Done { history });
    });
    rx
}

fn run_turn(
    cfg: &ProviderConfig,
    index: &Arc<JsonIndex>,
    file_name: &str,
    history: &mut Vec<Value>,
    cancel: &AtomicBool,
    tx: &Sender<AiMsg>,
) {
    let system = system_prompt(file_name);
    let tool_defs = tools::definitions();

    for round in 0..MAX_ROUNDS {
        if cancel.load(Ordering::Relaxed) {
            let _ = tx.send(AiMsg::ToolNote("Stopped.".to_owned()));
            return;
        }

        let turn = match provider::chat(cfg, &system, history, &tool_defs) {
            Ok(t) => t,
            Err(e) => {
                let _ = tx.send(AiMsg::Error(e));
                return;
            }
        };

        history.push(turn.assistant_message);
        if !turn.text.trim().is_empty() {
            let _ = tx.send(AiMsg::Assistant(turn.text.clone()));
        }

        if turn.tool_calls.is_empty() {
            return; // model is done
        }
        if round + 1 == MAX_ROUNDS {
            let _ = tx.send(AiMsg::Error("Stopped: too many tool rounds.".to_owned()));
            return;
        }

        let mut results = Vec::new();
        for call in &turn.tool_calls {
            if cancel.load(Ordering::Relaxed) {
                let _ = tx.send(AiMsg::ToolNote("Stopped.".to_owned()));
                return;
            }
            let _ = tx.send(AiMsg::ToolNote(describe_call(&call.name, &call.args)));
            let outcome = tools::execute(index, &call.name, &call.args);
            if !outcome.proposals.is_empty() {
                let _ = tx.send(AiMsg::Proposal(outcome.proposals));
            }
            results.push((call.id.clone(), outcome.output, outcome.is_error));
        }
        history.extend(provider::tool_results_messages(cfg.kind, &results));
    }
}

/// One-line human-readable description of a tool call for the transcript.
fn describe_call(name: &str, args: &Value) -> String {
    match name {
        "get_schema" => match args["path"].as_str() {
            Some(p) if p != "$" => format!("Inspected structure of {p}"),
            _ => "Inspected document structure".to_owned(),
        },
        "get_value" => format!("Read {}", args["path"].as_str().unwrap_or("?")),
        "search" => format!("Searched for “{}”", args["query"].as_str().unwrap_or("?")),
        "propose_edits" => {
            let n = args["edits"].as_array().map(|a| a.len()).unwrap_or(0);
            format!("Proposed {n} edit(s)")
        }
        other => format!("Called {other}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai::provider::ProviderKind;
    use crate::index::JsonData;
    use std::io::{Read, Write};

    fn make(json: &str) -> Arc<JsonIndex> {
        let data = json.as_bytes().to_vec();
        let (nodes, root, is_ndjson) = crate::parser::parse_bytes(&data, &mut |_| {}).unwrap();
        Arc::new(JsonIndex { data: JsonData::Memory(data), nodes, root, is_ndjson })
    }

    /// Minimal one-shot HTTP server: answers `responses` in order, capturing
    /// each request body.
    fn mock_server(responses: Vec<String>) -> (String, std::thread::JoinHandle<Vec<String>>) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let handle = std::thread::spawn(move || {
            let mut bodies = Vec::new();
            for resp in responses {
                let (mut stream, _) = listener.accept().unwrap();
                // Read headers + body.
                let mut buf = Vec::new();
                let mut tmp = [0u8; 4096];
                let body = loop {
                    let n = stream.read(&mut tmp).unwrap();
                    buf.extend_from_slice(&tmp[..n]);
                    if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                        let headers = String::from_utf8_lossy(&buf[..pos]).to_lowercase();
                        let len: usize = headers
                            .lines()
                            .find_map(|l| l.strip_prefix("content-length:"))
                            .and_then(|v| v.trim().parse().ok())
                            .unwrap_or(0);
                        let mut body = buf[pos + 4..].to_vec();
                        while body.len() < len {
                            let n = stream.read(&mut tmp).unwrap();
                            body.extend_from_slice(&tmp[..n]);
                        }
                        break String::from_utf8_lossy(&body).into_owned();
                    }
                };
                bodies.push(body);
                let reply = format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                    resp.len(),
                    resp
                );
                stream.write_all(reply.as_bytes()).unwrap();
            }
            bodies
        });
        (format!("http://{addr}"), handle)
    }

    #[test]
    fn openai_turn_runs_tool_loop() {
        // Round 1: the model calls get_value; round 2: it answers.
        let r1 = serde_json::json!({"choices": [{"message": {
            "role": "assistant", "content": null,
            "tool_calls": [{"id": "c1", "type": "function", "function": {
                "name": "get_value", "arguments": "{\"path\": \"$.name\"}"}}]
        }}]})
        .to_string();
        let r2 = serde_json::json!({"choices": [{"message": {
            "role": "assistant", "content": "The name is Alice."
        }}]})
        .to_string();
        let (base, server) = mock_server(vec![r1, r2]);

        let index = make(r#"{"name": "Alice"}"#);
        let cfg = ProviderConfig {
            kind:     ProviderKind::OpenAiCompatible,
            api_key:  "test".to_owned(),
            model:    "test-model".to_owned(),
            base_url: base,
        };
        let history = vec![provider::user_message(cfg.kind, "what's the name?")];
        let cancel = Arc::new(AtomicBool::new(false));
        let rx = spawn_turn(cfg, index, "t.json".to_owned(), history, cancel);

        let mut notes = 0;
        let mut answer = None;
        let mut final_history = None;
        for msg in rx {
            match msg {
                AiMsg::ToolNote(_) => notes += 1,
                AiMsg::Assistant(t) => answer = Some(t),
                AiMsg::Done { history } => final_history = Some(history),
                AiMsg::Error(e) => panic!("unexpected error: {e}"),
                AiMsg::Proposal(_) => panic!("unexpected proposal"),
            }
        }
        assert_eq!(notes, 1);
        assert_eq!(answer.as_deref(), Some("The name is Alice."));
        // History: user, assistant(tool call), tool result, assistant(answer).
        assert_eq!(final_history.unwrap().len(), 4);

        // The second request must carry the tool result back to the model.
        let bodies = server.join().unwrap();
        assert!(bodies[1].contains("\"role\":\"tool\""));
        assert!(bodies[1].contains("Alice"));
    }
}
