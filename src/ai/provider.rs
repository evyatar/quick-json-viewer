//! BYOK LLM provider client: Anthropic Messages API and OpenAI-compatible
//! chat/completions, both with tool calling, over blocking `ureq` (the whole
//! agent loop runs on a background thread — see `session`).

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum ProviderKind {
    #[default]
    Anthropic,
    /// Any endpoint speaking the OpenAI chat/completions dialect
    /// (OpenAI, OpenRouter, Ollama, …) — the base URL is configurable.
    OpenAiCompatible,
}

impl ProviderKind {
    /// Keychain account name for this provider's API key.
    pub fn key_account(self) -> &'static str {
        match self {
            ProviderKind::Anthropic => "anthropic",
            ProviderKind::OpenAiCompatible => "openai_compatible",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            ProviderKind::Anthropic => "Anthropic",
            ProviderKind::OpenAiCompatible => "OpenAI-compatible",
        }
    }

    pub fn default_model(self) -> &'static str {
        match self {
            ProviderKind::Anthropic => "claude-opus-4-8",
            ProviderKind::OpenAiCompatible => "gpt-4o",
        }
    }
}

#[derive(Clone)]
pub struct ProviderConfig {
    pub kind:     ProviderKind,
    pub api_key:  String,
    pub model:    String,
    /// Base URL override. Empty = provider default. For OpenAI-compatible
    /// this should point at the `/v1` root (e.g. `https://api.openai.com/v1`).
    pub base_url: String,
}

impl ProviderConfig {
    fn base(&self) -> String {
        let trimmed = self.base_url.trim().trim_end_matches('/');
        if !trimmed.is_empty() {
            return trimmed.to_owned();
        }
        match self.kind {
            ProviderKind::Anthropic => "https://api.anthropic.com".to_owned(),
            ProviderKind::OpenAiCompatible => "https://api.openai.com/v1".to_owned(),
        }
    }
}

/// One tool invocation requested by the model.
pub struct ToolCall {
    pub id:   String,
    pub name: String,
    pub args: Value,
}

/// The outcome of one model request: the provider-native assistant message
/// (appended verbatim to the history for the next round), the text shown to
/// the user, and any tool calls to execute.
pub struct ChatTurn {
    pub assistant_message: Value,
    pub text:              String,
    pub tool_calls:        Vec<ToolCall>,
}

/// A tool definition in provider-neutral form; converted to each provider's
/// wire shape in `chat`.
pub struct ToolDef {
    pub name:        &'static str,
    pub description: &'static str,
    pub schema:      Value,
}

fn agent() -> ureq::Agent {
    ureq::AgentBuilder::new()
        .timeout(std::time::Duration::from_secs(180))
        .build()
}

/// Extract a readable error message from an HTTP failure.
fn http_error(err: ureq::Error) -> String {
    match err {
        ureq::Error::Status(code, resp) => {
            let body = resp.into_string().unwrap_or_default();
            // Try to pull the provider's error message out of the JSON body.
            let detail = serde_json::from_str::<Value>(&body)
                .ok()
                .and_then(|v| {
                    v.pointer("/error/message")
                        .and_then(Value::as_str)
                        .map(str::to_owned)
                })
                .unwrap_or_else(|| body.chars().take(300).collect());
            format!("HTTP {code}: {detail}")
        }
        other => other.to_string(),
    }
}

/// Build the initial user message in the provider's native shape.
pub fn user_message(kind: ProviderKind, text: &str) -> Value {
    match kind {
        ProviderKind::Anthropic => json!({
            "role": "user",
            "content": [{"type": "text", "text": text}],
        }),
        ProviderKind::OpenAiCompatible => json!({"role": "user", "content": text}),
    }
}

/// Wrap executed tool results in the provider's native follow-up message(s).
/// `results` items are `(tool_call_id, output, is_error)`.
pub fn tool_results_messages(
    kind: ProviderKind,
    results: &[(String, String, bool)],
) -> Vec<Value> {
    match kind {
        ProviderKind::Anthropic => {
            let blocks: Vec<Value> = results
                .iter()
                .map(|(id, out, is_err)| {
                    json!({
                        "type": "tool_result",
                        "tool_use_id": id,
                        "content": out,
                        "is_error": is_err,
                    })
                })
                .collect();
            vec![json!({"role": "user", "content": blocks})]
        }
        ProviderKind::OpenAiCompatible => results
            .iter()
            .map(|(id, out, _)| {
                json!({"role": "tool", "tool_call_id": id, "content": out})
            })
            .collect(),
    }
}

pub fn chat(
    cfg: &ProviderConfig,
    system: &str,
    history: &[Value],
    tools: &[ToolDef],
) -> Result<ChatTurn, String> {
    match cfg.kind {
        ProviderKind::Anthropic => chat_anthropic(cfg, system, history, tools),
        ProviderKind::OpenAiCompatible => chat_openai(cfg, system, history, tools),
    }
}

// ─── Anthropic Messages API ─────────────────────────────────────────────────

fn chat_anthropic(
    cfg: &ProviderConfig,
    system: &str,
    history: &[Value],
    tools: &[ToolDef],
) -> Result<ChatTurn, String> {
    let tools_json: Vec<Value> = tools
        .iter()
        .map(|t| {
            json!({
                "name": t.name,
                "description": t.description,
                "input_schema": t.schema,
            })
        })
        .collect();

    let body = json!({
        "model": cfg.model,
        "max_tokens": 8192,
        "system": system,
        "messages": history,
        "tools": tools_json,
    });

    let url = format!("{}/v1/messages", cfg.base());
    let resp: Value = agent()
        .post(&url)
        .set("content-type", "application/json")
        .set("x-api-key", &cfg.api_key)
        .set("anthropic-version", "2023-06-01")
        .send_json(body)
        .map_err(http_error)?
        .into_json()
        .map_err(|e| e.to_string())?;

    let stop_reason = resp["stop_reason"].as_str().unwrap_or("");
    if stop_reason == "refusal" {
        return Err("The model declined this request (safety refusal).".to_owned());
    }

    let content = resp["content"].as_array().cloned().unwrap_or_default();
    let mut text = String::new();
    let mut tool_calls = Vec::new();
    for block in &content {
        match block["type"].as_str().unwrap_or("") {
            "text" => {
                if !text.is_empty() {
                    text.push_str("\n\n");
                }
                text.push_str(block["text"].as_str().unwrap_or(""));
            }
            "tool_use" => tool_calls.push(ToolCall {
                id:   block["id"].as_str().unwrap_or("").to_owned(),
                name: block["name"].as_str().unwrap_or("").to_owned(),
                args: block["input"].clone(),
            }),
            _ => {} // thinking blocks etc. — echoed back verbatim via the raw content
        }
    }

    Ok(ChatTurn {
        assistant_message: json!({"role": "assistant", "content": content}),
        text,
        tool_calls,
    })
}

// ─── OpenAI-compatible chat/completions ─────────────────────────────────────

fn chat_openai(
    cfg: &ProviderConfig,
    system: &str,
    history: &[Value],
    tools: &[ToolDef],
) -> Result<ChatTurn, String> {
    let tools_json: Vec<Value> = tools
        .iter()
        .map(|t| {
            json!({
                "type": "function",
                "function": {
                    "name": t.name,
                    "description": t.description,
                    "parameters": t.schema,
                },
            })
        })
        .collect();

    let mut messages = vec![json!({"role": "system", "content": system})];
    messages.extend_from_slice(history);

    let body = json!({
        "model": cfg.model,
        "messages": messages,
        "tools": tools_json,
    });

    let url = format!("{}/chat/completions", cfg.base());
    let resp: Value = agent()
        .post(&url)
        .set("content-type", "application/json")
        .set("authorization", &format!("Bearer {}", cfg.api_key))
        .send_json(body)
        .map_err(http_error)?
        .into_json()
        .map_err(|e| e.to_string())?;

    let message = resp
        .pointer("/choices/0/message")
        .cloned()
        .ok_or_else(|| "malformed response: no choices".to_owned())?;

    let text = message["content"].as_str().unwrap_or("").to_owned();
    let mut tool_calls = Vec::new();
    if let Some(calls) = message["tool_calls"].as_array() {
        for c in calls {
            // Arguments arrive as a JSON-encoded string.
            let args_raw = c.pointer("/function/arguments").and_then(Value::as_str).unwrap_or("{}");
            let args = serde_json::from_str(args_raw).unwrap_or(Value::Null);
            tool_calls.push(ToolCall {
                id:   c["id"].as_str().unwrap_or("").to_owned(),
                name: c.pointer("/function/name").and_then(Value::as_str).unwrap_or("").to_owned(),
                args,
            });
        }
    }

    Ok(ChatTurn { assistant_message: message, text, tool_calls })
}
