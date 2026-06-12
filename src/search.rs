use crate::index::{JsonIndex, Node, NodeKind};
use std::borrow::Cow;

// ─── query model ─────────────────────────────────────────────────────────────

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CmpOp {
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Rhs {
    Num(f64),
    Str(String),
}

#[derive(Clone, Debug, PartialEq)]
pub enum QueryPart {
    /// Plain substring matched against key or leaf value.
    Anywhere(String),
    /// `key:foo` — keys containing the pattern.
    KeyPattern(String),
    /// `value:foo` — leaf values containing the pattern.
    ValuePattern(String),
    /// `age > 30`, `status = ok`, `value >= 100` (key=None matches any key).
    Compare {
        key: Option<String>,
        op:  CmpOp,
        rhs: Rhs,
    },
}

// ─── tokenizer ───────────────────────────────────────────────────────────────

#[derive(Clone, Debug)]
struct Token {
    text:   String,
    quoted: bool,
}

/// Split on whitespace; a double-quoted run becomes a single token with the
/// quotes stripped, never re-split on operators.
fn tokenize(q: &str) -> Vec<Token> {
    let mut tokens = Vec::new();
    let mut chars = q.chars().peekable();
    while let Some(&c) = chars.peek() {
        if c.is_whitespace() {
            chars.next();
        } else if c == '"' {
            chars.next();
            let mut s = String::new();
            for c in chars.by_ref() {
                if c == '"' {
                    break;
                }
                s.push(c);
            }
            tokens.push(Token { text: s, quoted: true });
        } else {
            let mut s = String::new();
            while let Some(&c) = chars.peek() {
                if c.is_whitespace() || c == '"' {
                    break;
                }
                s.push(c);
                chars.next();
            }
            tokens.push(Token { text: s, quoted: false });
        }
    }
    tokens
}

fn parse_op(s: &str) -> Option<CmpOp> {
    match s {
        "=" | "==" => Some(CmpOp::Eq),
        "!="       => Some(CmpOp::Ne),
        "<"        => Some(CmpOp::Lt),
        "<="       => Some(CmpOp::Le),
        ">"        => Some(CmpOp::Gt),
        ">="       => Some(CmpOp::Ge),
        _          => None,
    }
}

/// Break unquoted tokens around comparison operators so `age>30` parses the
/// same as `age > 30`. Operators are ASCII, so byte positions are safe
/// char boundaries.
fn split_ops(tokens: Vec<Token>) -> Vec<Token> {
    let mut out = Vec::new();
    for t in tokens {
        if t.quoted {
            out.push(t);
            continue;
        }
        let mut rest = t.text.as_str();
        loop {
            let mut found: Option<(usize, usize)> = None; // (byte pos, op len)
            for i in 0..rest.len() {
                if let Some(two) = rest.get(i..i + 2) {
                    if matches!(two, ">=" | "<=" | "!=" | "==") {
                        found = Some((i, 2));
                        break;
                    }
                }
                if matches!(rest.as_bytes()[i], b'>' | b'<' | b'=') {
                    found = Some((i, 1));
                    break;
                }
            }
            match found {
                Some((pos, len)) => {
                    if pos > 0 {
                        out.push(Token { text: rest[..pos].to_owned(), quoted: false });
                    }
                    out.push(Token { text: rest[pos..pos + len].to_owned(), quoted: false });
                    rest = &rest[pos + len..];
                    if rest.is_empty() {
                        break;
                    }
                }
                None => {
                    out.push(Token { text: rest.to_owned(), quoted: false });
                    break;
                }
            }
        }
    }
    out
}

// ─── parser ──────────────────────────────────────────────────────────────────

/// Parse a query string into AND-ed parts. Incomplete fragments (a dangling
/// operator while the user is still typing) are dropped rather than erroring.
pub fn parse_query(q: &str) -> Vec<QueryPart> {
    let tokens = split_ops(tokenize(q));
    let mut parts = Vec::new();
    let mut i = 0;
    while i < tokens.len() {
        let t = &tokens[i];
        let t_is_op = !t.quoted && parse_op(&t.text).is_some();

        // `lhs op rhs`
        if !t_is_op && !t.text.is_empty() {
            if let Some(next) = tokens.get(i + 1) {
                if !next.quoted {
                    if let Some(op) = parse_op(&next.text) {
                        if let Some(rhs_tok) = tokens.get(i + 2) {
                            let rhs = if rhs_tok.quoted {
                                Rhs::Str(rhs_tok.text.clone())
                            } else {
                                rhs_tok.text.parse::<f64>()
                                    .map(Rhs::Num)
                                    .unwrap_or_else(|_| Rhs::Str(rhs_tok.text.clone()))
                            };
                            let key = if !t.quoted && t.text == "value" {
                                None
                            } else {
                                Some(t.text.clone())
                            };
                            parts.push(QueryPart::Compare { key, op, rhs });
                            i += 3;
                            continue;
                        }
                        // `lhs op` with no rhs yet — drop both, user mid-typing
                        i += 2;
                        continue;
                    }
                }
            }
        }

        if t_is_op {
            // dangling operator
            i += 1;
            continue;
        }

        if !t.quoted {
            if let Some(rest) = t.text.strip_prefix("key:") {
                if !rest.is_empty() {
                    parts.push(QueryPart::KeyPattern(rest.to_owned()));
                }
                i += 1;
                continue;
            }
            if let Some(rest) = t.text.strip_prefix("value:") {
                if !rest.is_empty() {
                    parts.push(QueryPart::ValuePattern(rest.to_owned()));
                }
                i += 1;
                continue;
            }
        }

        if !t.text.is_empty() {
            parts.push(QueryPart::Anywhere(t.text.clone()));
        }
        i += 1;
    }
    parts
}

// ─── evaluation ──────────────────────────────────────────────────────────────

/// Leaf value as text: strings without their surrounding quotes,
/// numbers/bools/null as raw bytes. Containers have no value.
fn leaf_value<'a>(index: &'a JsonIndex, node: &Node) -> Option<Cow<'a, str>> {
    match node.kind {
        NodeKind::String => {
            let raw = index.value_bytes(node);
            let inner = if raw.len() >= 2 { &raw[1..raw.len() - 1] } else { raw };
            Some(String::from_utf8_lossy(inner))
        }
        NodeKind::Number | NodeKind::Bool | NodeKind::Null => {
            Some(String::from_utf8_lossy(index.value_bytes(node)))
        }
        NodeKind::Object | NodeKind::Array => None,
    }
}

fn ord_matches(op: CmpOp, ord: Option<std::cmp::Ordering>) -> bool {
    use std::cmp::Ordering::*;
    match (op, ord) {
        (CmpOp::Eq, Some(Equal))          => true,
        (CmpOp::Ne, Some(o))              => o != Equal,
        (CmpOp::Lt, Some(Less))           => true,
        (CmpOp::Le, Some(Less | Equal))   => true,
        (CmpOp::Gt, Some(Greater))        => true,
        (CmpOp::Ge, Some(Greater | Equal)) => true,
        _                                  => false,
    }
}

fn cmp_matches(op: CmpOp, val: &str, rhs: &Rhs) -> bool {
    match rhs {
        // Numeric rhs: the node value must parse as a number too.
        Rhs::Num(n) => match val.trim().parse::<f64>() {
            Ok(v)  => ord_matches(op, v.partial_cmp(n)),
            Err(_) => false,
        },
        // String rhs: lexicographic — also covers ISO dates like 2024-01-01.
        Rhs::Str(s) => ord_matches(op, Some(val.cmp(s.as_str()))),
    }
}

fn part_matches(index: &JsonIndex, node: &Node, part: &QueryPart) -> bool {
    match part {
        QueryPart::Anywhere(p) => {
            (node.key_len > 0 && index.key_of(node).contains(p.as_str()))
                || leaf_value(index, node).is_some_and(|v| v.contains(p.as_str()))
        }
        QueryPart::KeyPattern(p) => {
            node.key_len > 0 && index.key_of(node).contains(p.as_str())
        }
        QueryPart::ValuePattern(p) => {
            leaf_value(index, node).is_some_and(|v| v.contains(p.as_str()))
        }
        QueryPart::Compare { key, op, rhs } => {
            if let Some(k) = key {
                if node.key_len == 0 || index.key_of(node) != k {
                    return false;
                }
            }
            leaf_value(index, node).is_some_and(|v| cmp_matches(*op, &v, rhs))
        }
    }
}

// ─── entry point ─────────────────────────────────────────────────────────────

/// Search node keys and leaf values for `query`.
/// Regex mode matches the pattern against keys and raw values (no DSL).
/// Otherwise the query is parsed as smart-search parts AND-ed together.
/// Returns node indices that match.
pub fn search(index: &JsonIndex, query: &str, use_regex: bool) -> Vec<u32> {
    if query.is_empty() {
        return Vec::new();
    }

    if use_regex {
        let Ok(re) = regex::Regex::new(query) else {
            return Vec::new();
        };
        return index.nodes.iter().enumerate()
            .filter(|(_, node)| {
                (node.key_len > 0 && re.is_match(index.key_of(node)))
                    || match node.kind {
                        NodeKind::String | NodeKind::Number | NodeKind::Bool | NodeKind::Null => {
                            re.is_match(String::from_utf8_lossy(index.value_bytes(node)).as_ref())
                        }
                        NodeKind::Object | NodeKind::Array => false,
                    }
            })
            .map(|(i, _)| i as u32)
            .collect();
    }

    let parts = parse_query(query);
    if parts.is_empty() {
        return Vec::new();
    }

    index.nodes.iter().enumerate()
        .filter(|(_, node)| parts.iter().all(|p| part_matches(index, node, p)))
        .map(|(i, _)| i as u32)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::JsonIndex;
    use crate::parser::parse_bytes;
    use std::sync::Arc;

    fn make_index(json: &str) -> Arc<JsonIndex> {
        let data = json.as_bytes().to_vec();
        let mut key_arena = Vec::new();
        let (nodes, root, is_ndjson) =
            parse_bytes(&data, &mut key_arena, &mut |_| {}).unwrap();
        Arc::new(JsonIndex {
            data: crate::index::JsonData::Memory(data),
            nodes, key_arena, root, is_ndjson,
        })
    }

    #[test]
    fn empty_query_returns_nothing() {
        let idx = make_index(r#"{"key": "value"}"#);
        assert!(search(&idx, "", false).is_empty());
    }

    #[test]
    fn plain_text_matches_key() {
        let idx = make_index(r#"{"hello": 1}"#);
        assert_eq!(search(&idx, "hello", false).len(), 1);
    }

    #[test]
    fn plain_text_matches_string_value() {
        let idx = make_index(r#"{"x": "world"}"#);
        assert_eq!(search(&idx, "world", false).len(), 1);
    }

    #[test]
    fn no_match_returns_empty() {
        let idx = make_index(r#"{"a": 1}"#);
        assert!(search(&idx, "zzz", false).is_empty());
    }

    #[test]
    fn multiple_keys_match() {
        let idx = make_index(r#"{"foo": 1, "foobar": 2, "baz": 3}"#);
        assert_eq!(search(&idx, "foo", false).len(), 2);
    }

    #[test]
    fn regex_matches_pattern() {
        let idx = make_index(r#"{"abc123": 1}"#);
        assert_eq!(search(&idx, r"\d+", true).len(), 1);
    }

    #[test]
    fn regex_no_match() {
        // Neither key ("name") nor value ("Alice") is all-digits
        let idx = make_index(r#"{"name": "Alice"}"#);
        assert!(search(&idx, r"^\d+$", true).is_empty());
    }

    #[test]
    fn invalid_regex_returns_empty_not_panic() {
        let idx = make_index(r#"{"a": 1}"#);
        assert!(search(&idx, "[invalid", true).is_empty());
    }

    #[test]
    fn number_value_is_searchable() {
        let idx = make_index(r#"{"n": 42}"#);
        assert_eq!(search(&idx, "42", false).len(), 1);
    }

    #[test]
    fn bool_value_is_searchable() {
        let idx = make_index(r#"{"flag": true}"#);
        assert_eq!(search(&idx, "true", false).len(), 1);
    }

    // ── smart search ──────────────────────────────────────────────────────

    #[test]
    fn key_pattern_only_matches_keys() {
        let idx = make_index(r#"{"name": "x", "x": "name"}"#);
        let res = search(&idx, "key:name", false);
        assert_eq!(res.len(), 1);
        assert_eq!(idx.key_of(&idx.nodes[res[0] as usize]), "name");
    }

    #[test]
    fn value_pattern_only_matches_values() {
        let idx = make_index(r#"{"name": "x", "x": "name"}"#);
        let res = search(&idx, "value:name", false);
        assert_eq!(res.len(), 1);
        assert_eq!(idx.key_of(&idx.nodes[res[0] as usize]), "x");
    }

    #[test]
    fn numeric_greater_than() {
        let idx = make_index(r#"{"age": 35, "score": 20}"#);
        let res = search(&idx, "age > 30", false);
        assert_eq!(res.len(), 1);
        assert_eq!(idx.key_of(&idx.nodes[res[0] as usize]), "age");
        assert!(search(&idx, "age > 100", false).is_empty());
    }

    #[test]
    fn operators_without_spaces() {
        let idx = make_index(r#"{"age": 35}"#);
        assert_eq!(search(&idx, "age>30", false).len(), 1);
        assert_eq!(search(&idx, "age>=35", false).len(), 1);
        assert!(search(&idx, "age<35", false).is_empty());
        assert_eq!(search(&idx, "age<=35", false).len(), 1);
        assert_eq!(search(&idx, "age!=30", false).len(), 1);
        assert_eq!(search(&idx, "age==35", false).len(), 1);
    }

    #[test]
    fn numeric_range_two_parts() {
        let idx = make_index(r#"{"a": 75, "b": 85, "c": 95}"#);
        let res = search(&idx, "value >= 80 value < 90", false);
        assert_eq!(res.len(), 1);
        assert_eq!(idx.key_of(&idx.nodes[res[0] as usize]), "b");
    }

    #[test]
    fn string_equality() {
        let idx = make_index(r#"{"status": "active", "other": "inactive"}"#);
        let res = search(&idx, "status = active", false);
        assert_eq!(res.len(), 1);
        assert_eq!(idx.key_of(&idx.nodes[res[0] as usize]), "status");
        // exact match: "inactive" != "active", and key must equal "status"
        assert!(search(&idx, "other = active", false).is_empty());
    }

    #[test]
    fn quoted_string_with_spaces() {
        let idx = make_index(r#"{"city": "New York", "ny": "NewYork"}"#);
        let res = search(&idx, r#"city = "New York""#, false);
        assert_eq!(res.len(), 1);
        assert_eq!(idx.key_of(&idx.nodes[res[0] as usize]), "city");
    }

    #[test]
    fn value_keyword_compares_any_key() {
        let idx = make_index(r#"{"a": 5, "b": 500}"#);
        let res = search(&idx, "value > 100", false);
        assert_eq!(res.len(), 1);
        assert_eq!(idx.key_of(&idx.nodes[res[0] as usize]), "b");
    }

    #[test]
    fn combined_parts_are_anded() {
        let idx = make_index(r#"{"user_id": 1500, "user_name": "bob", "id": 2000}"#);
        let res = search(&idx, "key:user value > 1000", false);
        assert_eq!(res.len(), 1);
        assert_eq!(idx.key_of(&idx.nodes[res[0] as usize]), "user_id");
    }

    #[test]
    fn lexicographic_date_compare() {
        let idx = make_index(r#"{"old": "2023-06-01", "new": "2024-03-15"}"#);
        let res = search(&idx, "value >= 2024-01-01", false);
        assert_eq!(res.len(), 1);
        assert_eq!(idx.key_of(&idx.nodes[res[0] as usize]), "new");
    }

    #[test]
    fn bool_equality() {
        let idx = make_index(r#"{"on": true, "off": false}"#);
        let res = search(&idx, "on = true", false);
        assert_eq!(res.len(), 1);
    }

    #[test]
    fn incomplete_query_while_typing_matches_nothing() {
        let idx = make_index(r#"{"age": 35}"#);
        // dangling operator — parts are dropped, no flicker of all-match
        assert!(search(&idx, "age >", false).is_empty());
        assert!(search(&idx, ">", false).is_empty());
        assert!(search(&idx, "key:", false).is_empty());
    }

    #[test]
    fn compare_ignores_containers() {
        let idx = make_index(r#"{"a": {"a": 5}}"#);
        // outer "a" is an object — only the inner number node can match
        let res = search(&idx, "a > 1", false);
        assert_eq!(res.len(), 1);
        assert_eq!(idx.nodes[res[0] as usize].kind, NodeKind::Number);
    }

    #[test]
    fn negative_number_rhs() {
        let idx = make_index(r#"{"t": -12, "u": 3}"#);
        let res = search(&idx, "value < -5", false);
        assert_eq!(res.len(), 1);
        assert_eq!(idx.key_of(&idx.nodes[res[0] as usize]), "t");
    }
}
