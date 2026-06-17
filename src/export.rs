use std::collections::{HashMap, HashSet};

use crate::index::{JsonIndex, NodeKind};

// ─── selection geometry ───────────────────────────────────────────────────────

/// Lowest common ancestor of a set of nodes, using `parent`/`depth` links.
/// Every node descends from the root, so the walk always converges.
pub fn lca(index: &JsonIndex, selected: &[u32]) -> u32 {
    let mut iter = selected.iter().copied();
    let mut acc = match iter.next() {
        Some(n) => n,
        None => return index.root,
    };
    for n in iter {
        acc = lca_pair(index, acc, n);
    }
    acc
}

fn lca_pair(index: &JsonIndex, mut a: u32, mut b: u32) -> u32 {
    let nodes = &index.nodes;
    while nodes[a as usize].depth > nodes[b as usize].depth {
        a = nodes[a as usize].parent;
    }
    while nodes[b as usize].depth > nodes[a as usize].depth {
        b = nodes[b as usize].parent;
    }
    while a != b {
        a = nodes[a as usize].parent;
        b = nodes[b as usize].parent;
    }
    a
}

/// Returns `(lca, keep)` where `keep` contains the LCA, every selected node,
/// and every node on the path from each selected node up to the LCA. A selected
/// node's own subtree is emitted in full by the serializers (they treat a
/// selected node as "include everything below"), so it need not be in `keep`.
pub fn build_keep_set(index: &JsonIndex, selected: &[u32]) -> (u32, HashSet<u32>) {
    let root = lca(index, selected);
    let mut keep = HashSet::new();
    keep.insert(root);
    for &s in selected {
        let mut cur = s;
        loop {
            keep.insert(cur);
            if cur == root {
                break;
            }
            let p = index.nodes[cur as usize].parent;
            if p == u32::MAX {
                break;
            }
            cur = p;
        }
    }
    (root, keep)
}

/// Should a child be walked, given the current include-all flag and keep-set?
#[inline]
fn included(child: u32, include_all: bool, keep: Option<&HashSet<u32>>) -> bool {
    include_all || keep.map_or(true, |k| k.contains(&child))
}

// ─── JSON ──────────────────────────────────────────────────────────────────────

/// Raw byte slice of a node's value — verbatim, preserving original formatting.
/// Correct for any single node or for a whole non-NDJSON document.
pub fn json_verbatim(index: &JsonIndex, idx: u32) -> Vec<u8> {
    index.value_bytes(&index.nodes[idx as usize]).to_vec()
}

/// Pretty-printed (2-space) JSON rooted at `root`.
///
/// * `sel`  — nodes whose entire subtree must be emitted in full.
/// * `keep` — `Some(set)` prunes containers to children in the set (plus any
///   subtree under a `sel` node); `None` emits everything.
///
/// Used for NDJSON whole-file export (reconstructs a real array) and for the
/// pruned common-ancestor subtree of a multi-node selection.
pub fn json_pretty(
    index: &JsonIndex,
    root: u32,
    sel: &HashSet<u32>,
    keep: Option<&HashSet<u32>>,
) -> String {
    let mut out = String::new();
    write_json(index, root, sel, keep, 0, &mut out);
    out.push('\n');
    out
}

fn write_json(
    index: &JsonIndex,
    idx: u32,
    sel: &HashSet<u32>,
    keep: Option<&HashSet<u32>>,
    level: usize,
    out: &mut String,
) {
    let node = &index.nodes[idx as usize];
    let include_all = keep.is_none() || sel.contains(&idx);

    match node.kind {
        NodeKind::Object | NodeKind::Array => {
            let is_obj = node.kind == NodeKind::Object;

            let mut children: Vec<u32> = Vec::new();
            let mut c = node.first_child;
            while c != u32::MAX {
                if included(c, include_all, keep) {
                    children.push(c);
                }
                c = index.nodes[c as usize].next_sibling;
            }

            if children.is_empty() {
                out.push_str(if is_obj { "{}" } else { "[]" });
                return;
            }

            out.push(if is_obj { '{' } else { '[' });
            out.push('\n');
            // Once we include a node fully, everything beneath it is emitted too.
            let child_keep = if include_all { None } else { keep };
            for (i, &ch) in children.iter().enumerate() {
                indent(out, level + 1);
                if is_obj {
                    out.push('"');
                    out.push_str(index.key_of(&index.nodes[ch as usize]));
                    out.push_str("\": ");
                }
                write_json(index, ch, sel, child_keep, level + 1, out);
                if i + 1 < children.len() {
                    out.push(',');
                }
                out.push('\n');
            }
            indent(out, level);
            out.push(if is_obj { '}' } else { ']' });
        }
        _ => out.push_str(&String::from_utf8_lossy(index.value_bytes(node))),
    }
}

fn indent(out: &mut String, level: usize) {
    for _ in 0..level {
        out.push_str("  ");
    }
}

// ─── CSV ─────────────────────────────────────────────────────────────────────

/// CSV rooted at `root`, flattening nested objects/arrays into dotted columns
/// (`address.city`, `tags[0]`). An array root yields one row per element; any
/// other root yields a single row. `sel`/`keep` carry the same pruning meaning
/// as [`json_pretty`].
pub fn csv(
    index: &JsonIndex,
    root: u32,
    sel: &HashSet<u32>,
    keep: Option<&HashSet<u32>>,
) -> String {
    let root_node = &index.nodes[root as usize];
    let include_all_root = keep.is_none() || sel.contains(&root);

    // Rows: array elements, or the root itself for objects/scalars.
    let row_roots: Vec<u32> = if root_node.kind == NodeKind::Array {
        let mut v = Vec::new();
        let mut c = root_node.first_child;
        while c != u32::MAX {
            if included(c, include_all_root, keep) {
                v.push(c);
            }
            c = index.nodes[c as usize].next_sibling;
        }
        v
    } else {
        vec![root]
    };

    let mut columns: Vec<String> = Vec::new();
    let mut col_seen: HashMap<String, ()> = HashMap::new();
    let mut rows: Vec<HashMap<String, String>> = Vec::new();

    let mut prefix = String::new();
    for &rr in &row_roots {
        let mut cells: Vec<(String, String)> = Vec::new();
        let row_include = keep.is_none() || sel.contains(&rr);
        prefix.clear();
        flatten(index, rr, sel, keep, row_include, &mut prefix, &mut cells);
        let mut map = HashMap::new();
        for (k, v) in cells {
            if col_seen.insert(k.clone(), ()).is_none() {
                columns.push(k.clone());
            }
            map.insert(k, v);
        }
        rows.push(map);
    }

    let mut out = String::new();
    for (i, c) in columns.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push_str(&csv_escape(c));
    }
    out.push('\n');
    for row in &rows {
        for (i, c) in columns.iter().enumerate() {
            if i > 0 {
                out.push(',');
            }
            if let Some(v) = row.get(c) {
                out.push_str(&csv_escape(v));
            }
        }
        out.push('\n');
    }
    out
}

/// Flatten `idx` into `(column, value)` cells, appending dotted/bracketed path
/// segments onto the shared `prefix` buffer and truncating back after each
/// child — so no per-node string allocation is needed.
fn flatten(
    index: &JsonIndex,
    idx: u32,
    sel: &HashSet<u32>,
    keep: Option<&HashSet<u32>>,
    include_all: bool,
    prefix: &mut String,
    out: &mut Vec<(String, String)>,
) {
    use std::fmt::Write;

    let node = &index.nodes[idx as usize];
    let include_all = include_all || sel.contains(&idx);

    match node.kind {
        NodeKind::Object | NodeKind::Array => {
            let is_obj = node.kind == NodeKind::Object;
            let mut any = false;
            let mut c = node.first_child;
            while c != u32::MAX {
                let cn = &index.nodes[c as usize];
                let next = cn.next_sibling;
                if included(c, include_all, keep) {
                    any = true;
                    let restore = prefix.len();
                    if is_obj {
                        if restore != 0 {
                            prefix.push('.');
                        }
                        prefix.push_str(index.key_of(cn));
                    } else {
                        let _ = write!(prefix, "[{}]", cn.array_index);
                    }
                    let child_all = if include_all { true } else { sel.contains(&c) };
                    flatten(index, c, sel, keep, child_all, prefix, out);
                    prefix.truncate(restore);
                }
                c = next;
            }
            if !any {
                out.push((column_name(prefix), if is_obj { "{}".into() } else { "[]".into() }));
            }
        }
        _ => out.push((column_name(prefix), leaf_value(index, idx))),
    }
}

fn column_name(prefix: &str) -> String {
    if prefix.is_empty() {
        "value".to_owned()
    } else {
        prefix.to_owned()
    }
}

/// Decoded scalar value for a CSV cell. Strings are JSON-unescaped; other
/// scalars are emitted as their source text.
fn leaf_value(index: &JsonIndex, idx: u32) -> String {
    let node = &index.nodes[idx as usize];
    let raw = index.value_bytes(node);
    if node.kind == NodeKind::String {
        serde_json::from_slice::<String>(raw).unwrap_or_else(|_| {
            let inner = if raw.len() >= 2 { &raw[1..raw.len() - 1] } else { raw };
            String::from_utf8_lossy(inner).into_owned()
        })
    } else {
        String::from_utf8_lossy(raw).into_owned()
    }
}

fn csv_escape(s: &str) -> String {
    if s.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::{JsonData, JsonIndex};
    use std::sync::Arc;

    fn make(json: &str) -> Arc<JsonIndex> {
        let data = json.as_bytes().to_vec();
        let mut key_arena = Vec::new();
        let (nodes, root, is_ndjson) =
            crate::parser::parse_bytes(&data, &mut key_arena, &mut |_| {}).unwrap();
        Arc::new(JsonIndex {
            data: JsonData::Memory(data),
            nodes,
            key_arena,
            root,
            is_ndjson,
        })
    }

    /// Walk to the node at a sequence of object keys / array indices from root.
    fn nav(index: &JsonIndex, path: &[&str]) -> u32 {
        let mut cur = index.root;
        for seg in path {
            let node = &index.nodes[cur as usize];
            let mut c = node.first_child;
            let mut found = None;
            while c != u32::MAX {
                let cn = &index.nodes[c as usize];
                let matches = if let Ok(i) = seg.parse::<u32>() {
                    cn.array_index == i
                } else {
                    index.key_of(cn) == *seg
                };
                if matches {
                    found = Some(c);
                    break;
                }
                c = cn.next_sibling;
            }
            cur = found.unwrap_or_else(|| panic!("path segment {seg:?} not found"));
        }
        cur
    }

    // ── LCA ────────────────────────────────────────────────────────────────

    #[test]
    fn lca_of_siblings_is_parent() {
        let idx = make(r#"{"a": {"x": 1, "y": 2}}"#);
        let x = nav(&idx, &["a", "x"]);
        let y = nav(&idx, &["a", "y"]);
        let a = nav(&idx, &["a"]);
        assert_eq!(lca(&idx, &[x, y]), a);
    }

    #[test]
    fn lca_across_depths() {
        let idx = make(r#"{"a": {"b": {"c": 1}}, "d": 2}"#);
        let c = nav(&idx, &["a", "b", "c"]);
        let d = nav(&idx, &["d"]);
        assert_eq!(lca(&idx, &[c, d]), idx.root);
    }

    #[test]
    fn lca_of_single_node_is_itself() {
        let idx = make(r#"{"a": 1}"#);
        let a = nav(&idx, &["a"]);
        assert_eq!(lca(&idx, &[a]), a);
    }

    // ── JSON verbatim ────────────────────────────────────────────────────────

    #[test]
    fn verbatim_preserves_node_bytes() {
        let idx = make(r#"{"a": {"b": 1}, "c": 2}"#);
        let a = nav(&idx, &["a"]);
        assert_eq!(json_verbatim(&idx, a), b"{\"b\": 1}");
    }

    // ── JSON pruned ────────────────────────────────────────────────────────

    #[test]
    fn pruned_keeps_only_selected_branches() {
        let idx = make(r#"{"a": {"x": 1, "y": 2}, "b": {"z": 3}}"#);
        let x = nav(&idx, &["a", "x"]);
        let z = nav(&idx, &["b", "z"]);
        let sel: HashSet<u32> = [x, z].into_iter().collect();
        let (root, keep) = build_keep_set(&idx, &[x, z]);
        assert_eq!(root, idx.root);
        let out = json_pretty(&idx, root, &sel, Some(&keep));
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v, serde_json::json!({"a": {"x": 1}, "b": {"z": 3}}));
    }

    #[test]
    fn pruned_selected_container_emitted_in_full() {
        let idx = make(r#"{"a": {"x": 1, "y": 2}, "b": 9}"#);
        let a = nav(&idx, &["a"]);
        let b = nav(&idx, &["b"]);
        let sel: HashSet<u32> = [a, b].into_iter().collect();
        let (root, keep) = build_keep_set(&idx, &[a, b]);
        let out = json_pretty(&idx, root, &sel, Some(&keep));
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v, serde_json::json!({"a": {"x": 1, "y": 2}, "b": 9}));
    }

    #[test]
    fn ndjson_full_reconstructs_array() {
        let idx = make("{\"a\":1}\n{\"a\":2}\n");
        assert!(idx.is_ndjson);
        let out = json_pretty(&idx, idx.root, &HashSet::new(), None);
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v, serde_json::json!([{"a": 1}, {"a": 2}]));
    }

    // ── CSV ──────────────────────────────────────────────────────────────────

    #[test]
    fn csv_array_of_objects() {
        let idx = make(r#"[{"name": "ann", "age": 30}, {"name": "bob", "age": 25}]"#);
        let out = csv(&idx, idx.root, &HashSet::new(), None);
        assert_eq!(out, "name,age\nann,30\nbob,25\n");
    }

    #[test]
    fn csv_flattens_nested_objects() {
        let idx = make(r#"[{"id": 1, "addr": {"city": "NYC"}}]"#);
        let out = csv(&idx, idx.root, &HashSet::new(), None);
        assert_eq!(out, "id,addr.city\n1,NYC\n");
    }

    #[test]
    fn csv_single_object_is_one_row() {
        let idx = make(r#"{"a": 1, "b": 2}"#);
        let out = csv(&idx, idx.root, &HashSet::new(), None);
        assert_eq!(out, "a,b\n1,2\n");
    }

    #[test]
    fn csv_escapes_special_chars() {
        let idx = make(r#"[{"s": "a,b\"c"}]"#);
        let out = csv(&idx, idx.root, &HashSet::new(), None);
        assert_eq!(out, "s\n\"a,b\"\"c\"\n");
    }

    #[test]
    fn csv_union_of_differing_keys() {
        let idx = make(r#"[{"a": 1}, {"b": 2}]"#);
        let out = csv(&idx, idx.root, &HashSet::new(), None);
        assert_eq!(out, "a,b\n1,\n,2\n");
    }

    #[test]
    fn csv_array_indices_in_columns() {
        let idx = make(r#"[{"tags": ["x", "y"]}]"#);
        let out = csv(&idx, idx.root, &HashSet::new(), None);
        assert_eq!(out, "tags[0],tags[1]\nx,y\n");
    }
}
