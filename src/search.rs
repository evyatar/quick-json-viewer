use crate::index::{JsonIndex, NodeKind};

/// Search node keys and leaf values for `query` (plain substring or regex).
/// Returns node indices that match.
pub fn search(index: &JsonIndex, query: &str, use_regex: bool) -> Vec<u32> {
    if query.is_empty() {
        return Vec::new();
    }

    let re: Option<regex::Regex> = if use_regex {
        regex::Regex::new(query).ok()
    } else {
        None
    };

    let matches_str = |s: &str| -> bool {
        if let Some(re) = &re {
            re.is_match(s)
        } else {
            s.contains(query)
        }
    };

    let matches_bytes = |b: &[u8]| -> bool {
        let s = String::from_utf8_lossy(b);
        matches_str(s.as_ref())
    };

    let mut results = Vec::new();

    for (i, node) in index.nodes.iter().enumerate() {
        let mut hit = false;

        // Check key
        if node.key_len > 0 {
            hit |= matches_str(index.key_of(node));
        }

        // Check value for leaf nodes
        if !hit {
            match node.kind {
                NodeKind::String | NodeKind::Number | NodeKind::Bool | NodeKind::Null => {
                    hit |= matches_bytes(index.value_bytes(node));
                }
                NodeKind::Object | NodeKind::Array => {}
            }
        }

        if hit {
            results.push(i as u32);
        }
    }

    results
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::JsonIndex;
    use crate::parser::parse_bytes;
    use std::sync::Arc;

    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

    fn make_index(json: &str) -> Arc<JsonIndex> {
        let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let path = std::env::temp_dir().join(format!("qjv_search_{n}.json"));
        std::fs::write(&path, json.as_bytes()).unwrap();
        let file = std::fs::File::open(&path).unwrap();
        let mmap = unsafe { memmap2::Mmap::map(&file).unwrap() };
        let _ = std::fs::remove_file(&path);
        let mut key_arena = Vec::new();
        let (nodes, root, is_ndjson) =
            parse_bytes(&mmap[..], &mut key_arena, &mut |_| {}).unwrap();
        Arc::new(JsonIndex { _file: file, mmap, nodes, key_arena, root, is_ndjson })
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
}
