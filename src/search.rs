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
