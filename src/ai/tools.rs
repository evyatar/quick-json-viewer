//! The tool surface the model works through. Every tool reads the immutable
//! `JsonIndex` — the document itself is never sent to the provider, only the
//! (size-capped) results of these calls. `propose_edits` produces a
//! changeset for user review; nothing is applied here.

use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use serde_json::{json, Value};

use crate::export;
use crate::index::{JsonIndex, NodeKind};

use super::provider::ToolDef;

/// Cap on bytes returned by a single tool call, so huge subtrees can't blow
/// up the request size (and the user's token bill).
const MAX_RESULT_BYTES: usize = 16 * 1024;
const MAX_SEARCH_RESULTS: usize = 50;

// ─── proposed edits ─────────────────────────────────────────────────────────

#[derive(Clone, Debug, PartialEq)]
pub enum EditAction {
    /// New raw JSON text for the node's value.
    SetValue(String),
    /// New key for an object property.
    RenameKey(String),
    Delete,
}

#[derive(Clone, Debug)]
pub struct ProposedEdit {
    pub path:   String,
    pub action: EditAction,
    /// Short display of the current value/key, for the review card.
    pub old:    String,
}

// ─── tool definitions ───────────────────────────────────────────────────────

pub fn definitions() -> Vec<ToolDef> {
    vec![
        ToolDef {
            name: "get_schema",
            description: "Summarize the structure of the JSON document (or a subtree): keys, value types, array lengths and a sample element. Call this first to orient yourself.",
            schema: json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": "Path to summarize, e.g. $.orders[0]. Omit or use \"$\" for the document root."}
                }
            }),
        },
        ToolDef {
            name: "get_value",
            description: "Return the JSON value at a path (compact-serialized, truncated to a size cap). Paths look like $.store.books[2].title.",
            schema: json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": "Path of the value to read."}
                },
                "required": ["path"]
            }),
        },
        ToolDef {
            name: "search",
            description: "Search all keys and values. Query syntax: plain text matches keys or values; key:foo / value:foo target one side; comparisons like `age > 30`, `status = active`, `value >= 100` (space-separated parts are AND-ed). Set regex=true to instead treat the query as a regular expression over keys and values. Returns matching paths with value previews.",
            schema: json!({
                "type": "object",
                "properties": {
                    "query": {"type": "string"},
                    "regex": {"type": "boolean", "description": "Treat the query as a regex (default false)."}
                },
                "required": ["query"]
            }),
        },
        ToolDef {
            name: "propose_edits",
            description: "Propose edits to the document. The edits are NOT applied — they are shown to the user as a reviewable changeset with Apply/Reject controls. Use for value changes, key renames and deletions, including bulk edits (one entry per node).",
            schema: json!({
                "type": "object",
                "properties": {
                    "edits": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "path": {"type": "string", "description": "Path of the node to edit."},
                                "action": {"type": "string", "enum": ["set_value", "rename_key", "delete"]},
                                "value": {"type": "string", "description": "For set_value: the new value as raw JSON text (e.g. \"\\\"hello\\\"\", \"42\", \"true\")."},
                                "key": {"type": "string", "description": "For rename_key: the new key."}
                            },
                            "required": ["path", "action"]
                        }
                    }
                },
                "required": ["edits"]
            }),
        },
    ]
}

// ─── execution ──────────────────────────────────────────────────────────────

/// Result of executing one tool call.
pub struct ToolOutcome {
    /// Text returned to the model as the tool result.
    pub output:    String,
    pub is_error:  bool,
    /// Populated only by `propose_edits` — routed to the UI for review.
    pub proposals: Vec<ProposedEdit>,
}

impl ToolOutcome {
    fn ok(output: String) -> Self {
        Self { output, is_error: false, proposals: Vec::new() }
    }
    fn err(output: String) -> Self {
        Self { output, is_error: true, proposals: Vec::new() }
    }
}

pub fn execute(index: &Arc<JsonIndex>, name: &str, args: &Value) -> ToolOutcome {
    match name {
        "get_schema" => {
            let path = args["path"].as_str().unwrap_or("$");
            match resolve_path(index, path) {
                Ok(idx) => {
                    let mut out = String::new();
                    summarize(index, idx, 0, &mut out);
                    truncate_note(&mut out);
                    ToolOutcome::ok(out)
                }
                Err(e) => ToolOutcome::err(e),
            }
        }
        "get_value" => {
            let Some(path) = args["path"].as_str() else {
                return ToolOutcome::err("missing required argument: path".to_owned());
            };
            match resolve_path(index, path) {
                Ok(idx) => {
                    let mut out = export::json_compact(index, idx);
                    truncate_note(&mut out);
                    ToolOutcome::ok(out)
                }
                Err(e) => ToolOutcome::err(e),
            }
        }
        "search" => {
            let Some(query) = args["query"].as_str() else {
                return ToolOutcome::err("missing required argument: query".to_owned());
            };
            let use_regex = args["regex"].as_bool().unwrap_or(false);
            let cancel = AtomicBool::new(false);
            let Some(results) = crate::search::search(index, query, use_regex, &cancel) else {
                return ToolOutcome::err("search failed (invalid regex?)".to_owned());
            };
            if results.is_empty() {
                return ToolOutcome::ok("no matches".to_owned());
            }
            let total = results.len();
            let mut out = format!("{total} match(es)");
            if total > MAX_SEARCH_RESULTS {
                out.push_str(&format!(", showing first {MAX_SEARCH_RESULTS}"));
            }
            out.push('\n');
            for &n in results.iter().take(MAX_SEARCH_RESULTS) {
                out.push_str(&node_path(index, n));
                out.push_str(" = ");
                out.push_str(&preview(index, n, 120));
                out.push('\n');
            }
            ToolOutcome::ok(out)
        }
        "propose_edits" => propose_edits(index, args),
        other => ToolOutcome::err(format!("unknown tool: {other}")),
    }
}

fn truncate_note(s: &mut String) {
    if s.len() > MAX_RESULT_BYTES {
        // Truncate on a char boundary.
        let mut cut = MAX_RESULT_BYTES;
        while !s.is_char_boundary(cut) {
            cut -= 1;
        }
        s.truncate(cut);
        s.push_str("\n…[truncated — narrow the path or use search]");
    }
}

fn propose_edits(index: &Arc<JsonIndex>, args: &Value) -> ToolOutcome {
    let Some(edits) = args["edits"].as_array() else {
        return ToolOutcome::err("missing required argument: edits".to_owned());
    };
    let mut proposals = Vec::new();
    let mut errors = Vec::new();
    for (i, e) in edits.iter().enumerate() {
        let path = e["path"].as_str().unwrap_or("");
        let node_idx = match resolve_path(index, path) {
            Ok(n) => n,
            Err(msg) => {
                errors.push(format!("edit {i}: {msg}"));
                continue;
            }
        };
        let node = &index.nodes[node_idx as usize];
        let action = match e["action"].as_str().unwrap_or("") {
            "set_value" => {
                let Some(v) = e["value"].as_str() else {
                    errors.push(format!("edit {i}: set_value requires `value`"));
                    continue;
                };
                if serde_json::from_str::<Value>(v).is_err() {
                    errors.push(format!("edit {i}: `value` is not valid JSON: {v}"));
                    continue;
                }
                EditAction::SetValue(v.to_owned())
            }
            "rename_key" => {
                let Some(k) = e["key"].as_str() else {
                    errors.push(format!("edit {i}: rename_key requires `key`"));
                    continue;
                };
                if node.key_len == 0 {
                    errors.push(format!("edit {i}: {path} is not an object property"));
                    continue;
                }
                EditAction::RenameKey(k.to_owned())
            }
            "delete" => {
                if node.parent == u32::MAX {
                    errors.push(format!("edit {i}: cannot delete the document root"));
                    continue;
                }
                EditAction::Delete
            }
            other => {
                errors.push(format!("edit {i}: unknown action `{other}`"));
                continue;
            }
        };
        let old = match &action {
            EditAction::RenameKey(_) => index.key_of(node).to_owned(),
            _ => preview(index, node_idx, 120),
        };
        proposals.push(ProposedEdit { path: path.to_owned(), action, old });
    }

    let mut output = format!(
        "{} edit(s) validated and presented to the user for review. The user decides whether to apply them.",
        proposals.len()
    );
    if !errors.is_empty() {
        output.push_str("\nRejected:\n");
        output.push_str(&errors.join("\n"));
    }
    ToolOutcome { output, is_error: proposals.is_empty() && !errors.is_empty(), proposals }
}

// ─── paths ──────────────────────────────────────────────────────────────────

/// Resolve a path like `$.store.books[2].title` (or `store.books[2]`,
/// `$["odd key"][0]`) to a node index.
pub fn resolve_path(index: &JsonIndex, path: &str) -> Result<u32, String> {
    let segments = parse_path(path)?;
    let mut cur = index.root;
    for seg in &segments {
        cur = match seg {
            Segment::Key(k) => find_key_child(index, cur, k)
                .ok_or_else(|| format!("path `{path}`: key `{k}` not found"))?,
            Segment::Index(i) => find_array_child(index, cur, *i)
                .ok_or_else(|| format!("path `{path}`: index [{i}] not found"))?,
        };
    }
    Ok(cur)
}

enum Segment {
    Key(String),
    Index(u32),
}

fn parse_path(path: &str) -> Result<Vec<Segment>, String> {
    let mut chars = path.trim().chars().peekable();
    // Optional leading `$`.
    if chars.peek() == Some(&'$') {
        chars.next();
    }
    let mut segments = Vec::new();
    loop {
        match chars.peek() {
            None => break,
            Some('.') => {
                chars.next();
                // `.["key"]` is also accepted (matches the app's path display).
                if chars.peek() == Some(&'[') {
                    continue;
                }
                let mut key = String::new();
                while let Some(&c) = chars.peek() {
                    if c == '.' || c == '[' {
                        break;
                    }
                    key.push(c);
                    chars.next();
                }
                if key.is_empty() {
                    return Err(format!("path `{path}`: empty key segment"));
                }
                segments.push(Segment::Key(key));
            }
            Some('[') => {
                chars.next();
                if chars.peek() == Some(&'"') || chars.peek() == Some(&'\'') {
                    let quote = chars.next().unwrap();
                    let mut key = String::new();
                    let mut closed = false;
                    while let Some(c) = chars.next() {
                        if c == '\\' {
                            if let Some(n) = chars.next() {
                                key.push(n);
                            }
                        } else if c == quote {
                            closed = true;
                            break;
                        } else {
                            key.push(c);
                        }
                    }
                    if !closed || chars.next() != Some(']') {
                        return Err(format!("path `{path}`: unterminated bracket segment"));
                    }
                    segments.push(Segment::Key(key));
                } else {
                    let mut num = String::new();
                    while let Some(&c) = chars.peek() {
                        if c == ']' {
                            break;
                        }
                        num.push(c);
                        chars.next();
                    }
                    if chars.next() != Some(']') {
                        return Err(format!("path `{path}`: missing `]`"));
                    }
                    let i: u32 = num
                        .trim()
                        .parse()
                        .map_err(|_| format!("path `{path}`: bad array index `{num}`"))?;
                    segments.push(Segment::Index(i));
                }
            }
            Some(c) if segments.is_empty() && *c != '.' && *c != '[' => {
                // Allow a bare leading key without `$.` (e.g. `store.books`).
                let mut key = String::new();
                while let Some(&c) = chars.peek() {
                    if c == '.' || c == '[' {
                        break;
                    }
                    key.push(c);
                    chars.next();
                }
                segments.push(Segment::Key(key));
            }
            Some(c) => return Err(format!("path `{path}`: unexpected `{c}`")),
        }
    }
    Ok(segments)
}

/// Keys are stored raw (escapes untouched); compare the segment against both
/// the raw text and its JSON-unescaped form.
fn key_matches(raw: &str, seg: &str) -> bool {
    if raw == seg {
        return true;
    }
    if raw.contains('\\') {
        if let Ok(unescaped) = serde_json::from_str::<String>(&format!("\"{raw}\"")) {
            return unescaped == seg;
        }
    }
    false
}

fn find_key_child(index: &JsonIndex, parent: u32, key: &str) -> Option<u32> {
    let mut c = index.first_child(parent);
    while c != u32::MAX {
        let cn = &index.nodes[c as usize];
        if key_matches(index.key_of(cn), key) {
            return Some(c);
        }
        c = cn.next_sibling;
    }
    None
}

fn find_array_child(index: &JsonIndex, parent: u32, i: u32) -> Option<u32> {
    let mut c = index.first_child(parent);
    while c != u32::MAX {
        if index.nodes[c as usize].array_index == i {
            return Some(c);
        }
        c = index.nodes[c as usize].next_sibling;
    }
    None
}

/// Path string for a node — same shape `resolve_path` accepts.
pub fn node_path(index: &JsonIndex, node_idx: u32) -> String {
    let mut segs: Vec<String> = Vec::new();
    let mut cur = node_idx;
    loop {
        let node = &index.nodes[cur as usize];
        if node.parent == u32::MAX {
            break;
        }
        if node.key_len > 0 {
            let key = index.key_of(node);
            let simple = !key.is_empty()
                && key.chars().next().map(|c| c.is_ascii_alphabetic() || c == '_').unwrap_or(false)
                && key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
            if simple {
                segs.push(format!(".{key}"));
            } else {
                segs.push(format!("[\"{key}\"]"));
            }
        } else if node.array_index != u32::MAX {
            segs.push(format!("[{}]", node.array_index));
        }
        cur = node.parent;
    }
    segs.reverse();
    format!("${}", segs.join(""))
}

// ─── summaries & previews ───────────────────────────────────────────────────

/// Short single-line preview of a node's value.
pub fn preview(index: &JsonIndex, node_idx: u32, max: usize) -> String {
    let node = &index.nodes[node_idx as usize];
    let mut s = match node.kind {
        NodeKind::Object => format!("{{…}} ({} keys)", node.child_count),
        NodeKind::Array => format!("[…] ({} items)", node.child_count),
        _ => String::from_utf8_lossy(index.value_bytes(node)).into_owned(),
    };
    if s.len() > max {
        let mut cut = max;
        while !s.is_char_boundary(cut) {
            cut -= 1;
        }
        s.truncate(cut);
        s.push('…');
    }
    s
}

const SCHEMA_MAX_DEPTH: usize = 4;
const SCHEMA_MAX_KEYS: usize = 25;

fn summarize(index: &JsonIndex, idx: u32, depth: usize, out: &mut String) {
    let node = &index.nodes[idx as usize];
    let pad = "  ".repeat(depth);
    match node.kind {
        NodeKind::Object => {
            out.push_str(&format!("object ({} keys)", node.child_count));
            if depth >= SCHEMA_MAX_DEPTH {
                return;
            }
            let mut c = index.first_child(idx);
            let mut shown = 0;
            while c != u32::MAX {
                if shown >= SCHEMA_MAX_KEYS {
                    out.push_str(&format!("\n{pad}  …"));
                    break;
                }
                let cn = &index.nodes[c as usize];
                out.push_str(&format!("\n{pad}  {}: ", index.key_of(cn)));
                summarize(index, c, depth + 1, out);
                shown += 1;
                c = cn.next_sibling;
            }
        }
        NodeKind::Array => {
            out.push_str(&format!("array ({} items)", node.child_count));
            // Arrays are usually homogeneous — summarize the first element.
            let first = index.first_child(idx);
            if first != u32::MAX && depth < SCHEMA_MAX_DEPTH {
                out.push_str(&format!("\n{pad}  [0]: "));
                summarize(index, first, depth + 1, out);
            }
        }
        NodeKind::String => out.push_str(&format!("string, e.g. {}", preview(index, idx, 60))),
        NodeKind::Number => out.push_str(&format!("number, e.g. {}", preview(index, idx, 30))),
        NodeKind::Bool => out.push_str("bool"),
        NodeKind::Null => out.push_str("null"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::JsonData;

    fn make(json: &str) -> Arc<JsonIndex> {
        let data = json.as_bytes().to_vec();
        let (nodes, root, is_ndjson) = crate::parser::parse_bytes(&data, &mut |_| {}).unwrap();
        Arc::new(JsonIndex { data: JsonData::Memory(data), nodes, root, is_ndjson })
    }

    #[test]
    fn resolve_dotted_and_bracket_paths() {
        let idx = make(r#"{"store": {"books": [{"title": "Dune"}, {"title": "Emma"}]}}"#);
        let n = resolve_path(&idx, "$.store.books[1].title").unwrap();
        assert_eq!(idx.value_bytes(&idx.nodes[n as usize]), b"\"Emma\"");
        let n2 = resolve_path(&idx, r#"$["store"]["books"][0]["title"]"#).unwrap();
        assert_eq!(idx.value_bytes(&idx.nodes[n2 as usize]), b"\"Dune\"");
        // Bare path without `$.`
        let n3 = resolve_path(&idx, "store.books").unwrap();
        assert_eq!(idx.nodes[n3 as usize].kind, NodeKind::Array);
        // Root
        assert_eq!(resolve_path(&idx, "$").unwrap(), idx.root);
    }

    #[test]
    fn resolve_errors_on_missing() {
        let idx = make(r#"{"a": 1}"#);
        assert!(resolve_path(&idx, "$.b").is_err());
        assert!(resolve_path(&idx, "$.a[0]").is_err());
    }

    #[test]
    fn node_path_roundtrips() {
        let idx = make(r#"{"a": {"odd key": [1, 2, {"x": true}]}}"#);
        let n = resolve_path(&idx, r#"$.a["odd key"][2].x"#).unwrap();
        let p = node_path(&idx, n);
        assert_eq!(resolve_path(&idx, &p).unwrap(), n);
    }

    #[test]
    fn propose_edits_validates() {
        let idx = make(r#"{"name": "Alice", "age": 30}"#);
        let args = json!({"edits": [
            {"path": "$.name", "action": "set_value", "value": "\"Bob\""},
            {"path": "$.age", "action": "delete"},
            {"path": "$.missing", "action": "delete"},
            {"path": "$.age", "action": "set_value", "value": "not json"},
        ]});
        let out = propose_edits(&idx, &args);
        assert_eq!(out.proposals.len(), 2);
        assert!(out.output.contains("Rejected"));
        assert!(matches!(out.proposals[0].action, EditAction::SetValue(_)));
        assert!(matches!(out.proposals[1].action, EditAction::Delete));
    }

    #[test]
    fn get_schema_and_value_execute() {
        let idx = make(r#"{"users": [{"id": 1, "name": "a"}]}"#);
        let schema = execute(&idx, "get_schema", &json!({}));
        assert!(!schema.is_error);
        assert!(schema.output.contains("users"));
        let val = execute(&idx, "get_value", &json!({"path": "$.users[0]"}));
        assert!(!val.is_error);
        assert_eq!(val.output, r#"{"id":1,"name":"a"}"#);
    }

    #[test]
    fn search_returns_paths() {
        let idx = make(r#"{"users": [{"name": "alice"}, {"name": "bob"}]}"#);
        let out = execute(&idx, "search", &json!({"query": "alice"}));
        assert!(!out.is_error);
        assert!(out.output.contains("$.users[0].name"));
    }
}
