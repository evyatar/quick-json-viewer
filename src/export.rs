use std::collections::{HashMap, HashSet};

use crate::index::{JsonIndex, Node, NodeKind};

/// Per-node edit overlay. `None` fields keep the original document bytes.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct NodeEdit {
    pub key_override:   Option<String>, // None = keep original key bytes
    pub value_override: Option<String>, // None = keep original; stored as raw JSON text
    pub deleted:        bool,           // true = exclude this node from saved output
}

/// A pending array element or object property that doesn't exist in the parsed
/// node arena yet — added interactively and not backed by any byte range in
/// the source data. Identified by a synthetic id: `real_node_count +
/// index_in_this_vec`, which is always greater than every real `u32` node index.
#[derive(Clone, Debug, PartialEq)]
pub struct AddedItem {
    pub parent:    u32,            // real Array or Object node this item is appended to
    pub key:       Option<String>, // Some for an Object property, None for an Array element
    pub raw_value: String,         // raw JSON text, as typed (validated JSON)
}

/// True when `node_idx` is a synthetic id from `AddedItem`, not a real node.
#[inline]
pub fn is_added(nodes_len: usize, node_idx: u32) -> bool {
    node_idx as usize >= nodes_len
}

/// Group `added` by parent, in insertion order, keyed by the parent's real
/// node index. Values are the synthetic ids (not indices into `added`).
pub fn build_added_map(nodes_len: usize, added: &[AddedItem]) -> HashMap<u32, Vec<u32>> {
    let mut map: HashMap<u32, Vec<u32>> = HashMap::new();
    for (i, item) in added.iter().enumerate() {
        map.entry(item.parent).or_default().push((nodes_len + i) as u32);
    }
    map
}

/// Display index (as if appended after the parent's real children) for a
/// synthetic id — used for the tree row's index label, breadcrumbs, and paths.
pub fn added_display_index(nodes: &[Node], added: &[AddedItem], node_idx: u32) -> u32 {
    let local = node_idx as usize - nodes.len();
    let parent = added[local].parent;
    let base = nodes[parent as usize].child_count;
    let offset = added[..=local].iter().filter(|it| it.parent == parent).count() as u32 - 1;
    base + offset
}

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
/// Correct for any single node or for a whole non-NDJSON document. Borrows the
/// source (possibly the whole mmap'd file) instead of copying it.
pub fn json_verbatim(index: &JsonIndex, idx: u32) -> &[u8] {
    index.value_bytes(&index.nodes[idx as usize])
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
            let mut c = index.first_child(idx);
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

/// Minified (no whitespace) JSON for the single value at `idx`, including its
/// full subtree. Unlike [`json_pretty`] there is no pruning — used for the
/// "Copy Value" context-menu action when compact copying is enabled.
pub fn json_compact(index: &JsonIndex, idx: u32) -> String {
    let mut out = String::new();
    write_json_compact(index, idx, &mut out);
    out
}

fn write_json_compact(index: &JsonIndex, idx: u32, out: &mut String) {
    let node = &index.nodes[idx as usize];
    match node.kind {
        NodeKind::Object | NodeKind::Array => {
            let is_obj = node.kind == NodeKind::Object;
            out.push(if is_obj { '{' } else { '[' });
            let mut c = index.first_child(idx);
            let mut first = true;
            while c != u32::MAX {
                if !first {
                    out.push(',');
                }
                first = false;
                if is_obj {
                    out.push('"');
                    out.push_str(index.key_of(&index.nodes[c as usize]));
                    out.push_str("\":");
                }
                write_json_compact(index, c, out);
                c = index.nodes[c as usize].next_sibling;
            }
            out.push(if is_obj { '}' } else { ']' });
        }
        _ => out.push_str(&String::from_utf8_lossy(index.value_bytes(node))),
    }
}

// ─── JSON with edits ───────────────────────────────────────────────────────────

/// Pretty-printed (2-space) JSON rooted at `root`, applying the `edits` overlay
/// and appending any pending `added` array items.
/// Unlike [`json_pretty`] there is no pruning — the whole tree is emitted, with
/// edited keys/values substituted for the original document bytes.
pub fn json_with_edits(
    index: &JsonIndex,
    root:  u32,
    edits: &std::collections::HashMap<u32, NodeEdit>,
    added: &[AddedItem],
) -> String {
    let added_map = build_added_map(index.nodes.len(), added);
    let mut out = String::new();
    write_json_edited(index, root, edits, added, &added_map, 0, &mut out);
    out.push('\n');
    out
}

fn write_json_edited(
    index:     &JsonIndex,
    idx:       u32,
    edits:     &std::collections::HashMap<u32, NodeEdit>,
    added:     &[AddedItem],
    added_map: &HashMap<u32, Vec<u32>>,
    level:     usize,
    out:       &mut String,
) {
    if is_added(index.nodes.len(), idx) {
        // A pending array item — no source bytes; emit its (possibly edited) text.
        let local = idx as usize - index.nodes.len();
        let v = edits
            .get(&idx)
            .and_then(|e| e.value_override.as_deref())
            .unwrap_or(added[local].raw_value.as_str());
        out.push_str(v);
        return;
    }

    let node = &index.nodes[idx as usize];
    match node.kind {
        NodeKind::Object | NodeKind::Array => {
            let is_obj = node.kind == NodeKind::Object;
            let (open, close) = if is_obj { ('{', '}') } else { ('[', ']') };

            // Collect children, skipping any marked deleted.
            let mut children: Vec<u32> = Vec::new();
            let mut c = index.first_child(idx);
            while c != u32::MAX {
                if !edits.get(&c).map_or(false, |e| e.deleted) {
                    children.push(c);
                }
                c = index.nodes[c as usize].next_sibling;
            }
            // Pending items are always appended after the real children.
            if let Some(extra) = added_map.get(&idx) {
                for &sid in extra {
                    if !edits.get(&sid).map_or(false, |e| e.deleted) {
                        children.push(sid);
                    }
                }
            }

            if children.is_empty() {
                out.push(open);
                out.push(close);
                return;
            }
            out.push(open);
            out.push('\n');
            for (i, &ch) in children.iter().enumerate() {
                indent(out, level + 1);
                if is_obj {
                    // Use edited key if present, original (or the typed key,
                    // for a pending added property) otherwise.
                    let key = edits.get(&ch).and_then(|e| e.key_override.as_deref()).unwrap_or_else(|| {
                        if is_added(index.nodes.len(), ch) {
                            added[ch as usize - index.nodes.len()].key.as_deref().unwrap_or("")
                        } else {
                            index.key_of(&index.nodes[ch as usize])
                        }
                    });
                    out.push('"');
                    push_json_escaped(out, key);
                    out.push_str("\": ");
                }
                write_json_edited(index, ch, edits, added, added_map, level + 1, out);
                if i + 1 < children.len() {
                    out.push(',');
                }
                out.push('\n');
            }
            indent(out, level);
            out.push(close);
        }
        _ => {
            // Use edited value if present, original raw bytes otherwise.
            if let Some(v) = edits.get(&idx).and_then(|e| e.value_override.as_deref()) {
                out.push_str(v);
            } else {
                out.push_str(&String::from_utf8_lossy(index.value_bytes(node)));
            }
        }
    }
}

/// Minified (no whitespace) JSON rooted at `root`, applying the `edits` overlay.
/// Compact counterpart of [`json_with_edits`] — used for the "Copy Modified
/// Value" context-menu action when compact copying is enabled.
pub fn json_compact_with_edits(
    index: &JsonIndex,
    root:  u32,
    edits: &std::collections::HashMap<u32, NodeEdit>,
    added: &[AddedItem],
) -> String {
    let added_map = build_added_map(index.nodes.len(), added);
    let mut out = String::new();
    write_json_compact_edited(index, root, edits, added, &added_map, &mut out);
    out
}

fn write_json_compact_edited(
    index:     &JsonIndex,
    idx:       u32,
    edits:     &std::collections::HashMap<u32, NodeEdit>,
    added:     &[AddedItem],
    added_map: &HashMap<u32, Vec<u32>>,
    out:       &mut String,
) {
    if is_added(index.nodes.len(), idx) {
        let local = idx as usize - index.nodes.len();
        let v = edits
            .get(&idx)
            .and_then(|e| e.value_override.as_deref())
            .unwrap_or(added[local].raw_value.as_str());
        out.push_str(v);
        return;
    }

    let node = &index.nodes[idx as usize];
    match node.kind {
        NodeKind::Object | NodeKind::Array => {
            let is_obj = node.kind == NodeKind::Object;
            out.push(if is_obj { '{' } else { '[' });
            let mut children: Vec<u32> = Vec::new();
            let mut c = index.first_child(idx);
            while c != u32::MAX {
                children.push(c);
                c = index.nodes[c as usize].next_sibling;
            }
            if let Some(extra) = added_map.get(&idx) {
                children.extend(extra.iter().copied());
            }
            let mut first = true;
            for c in children {
                if !edits.get(&c).map_or(false, |e| e.deleted) {
                    if !first {
                        out.push(',');
                    }
                    first = false;
                    if is_obj {
                        let key = edits.get(&c).and_then(|e| e.key_override.as_deref()).unwrap_or_else(|| {
                            if is_added(index.nodes.len(), c) {
                                added[c as usize - index.nodes.len()].key.as_deref().unwrap_or("")
                            } else {
                                index.key_of(&index.nodes[c as usize])
                            }
                        });
                        out.push('"');
                        push_json_escaped(out, key);
                        out.push_str("\":");
                    }
                    write_json_compact_edited(index, c, edits, added, added_map, out);
                }
            }
            out.push(if is_obj { '}' } else { ']' });
        }
        _ => {
            if let Some(v) = edits.get(&idx).and_then(|e| e.value_override.as_deref()) {
                out.push_str(v);
            } else {
                out.push_str(&String::from_utf8_lossy(index.value_bytes(node)));
            }
        }
    }
}

fn push_json_escaped(out: &mut String, s: &str) {
    for c in s.chars() {
        match c {
            '"'  => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c    => out.push(c),
        }
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
        let mut c = index.first_child(root);
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

    // Intern column names to dense ids once; rows hold (id, value) pairs
    // instead of a per-row HashMap with cloned column-name keys.
    let mut columns: Vec<String> = Vec::new();
    let mut col_ids: HashMap<String, usize> = HashMap::new();
    let mut rows: Vec<Vec<(usize, String)>> = Vec::new();

    let mut prefix = String::new();
    for &rr in &row_roots {
        let mut cells: Vec<(String, String)> = Vec::new();
        let row_include = keep.is_none() || sel.contains(&rr);
        prefix.clear();
        flatten(index, rr, sel, keep, row_include, &mut prefix, &mut cells);
        let mut row: Vec<(usize, String)> = Vec::with_capacity(cells.len());
        for (k, v) in cells {
            let id = match col_ids.get(&k) {
                Some(&id) => id,
                None => {
                    let id = columns.len();
                    col_ids.insert(k.clone(), id);
                    columns.push(k);
                    id
                }
            };
            row.push((id, v));
        }
        rows.push(row);
    }

    let mut out = String::new();
    for (i, c) in columns.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push_str(&csv_escape(c));
    }
    out.push('\n');
    // Scatter each row's cells into a reusable column-indexed scratch slot
    // (duplicate columns: last value wins, as the old map insert did).
    let mut slot: Vec<Option<&String>> = vec![None; columns.len()];
    for row in &rows {
        for (id, v) in row {
            slot[*id] = Some(v);
        }
        for (i, s) in slot.iter().enumerate() {
            if i > 0 {
                out.push(',');
            }
            if let Some(v) = s {
                out.push_str(&csv_escape(v));
            }
        }
        for (id, _) in row {
            slot[*id] = None;
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
            let mut c = index.first_child(idx);
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
        let (nodes, root, is_ndjson) =
            crate::parser::parse_bytes(&data, &mut |_| {}).unwrap();
        Arc::new(JsonIndex {
            data: JsonData::Memory(data),
            nodes,
            root,
            is_ndjson,
        })
    }

    /// Walk to the node at a sequence of object keys / array indices from root.
    fn nav(index: &JsonIndex, path: &[&str]) -> u32 {
        let mut cur = index.root;
        for seg in path {
            let mut c = index.first_child(cur);
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

    // ── JSON with edits ────────────────────────────────────────────────────────

    #[test]
    fn edits_override_string_value() {
        let idx = make(r#"{"name": "Alice", "age": 30}"#);
        let name = nav(&idx, &["name"]);
        let mut edits: HashMap<u32, NodeEdit> = HashMap::new();
        edits.insert(name, NodeEdit { key_override: None, value_override: Some("\"Bob\"".to_owned()), deleted: false });
        let out = json_with_edits(&idx, idx.root, &edits, &[]);
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v, serde_json::json!({"name": "Bob", "age": 30}));
    }

    #[test]
    fn edits_override_number_value() {
        let idx = make(r#"{"name": "Alice", "age": 30}"#);
        let age = nav(&idx, &["age"]);
        let mut edits: HashMap<u32, NodeEdit> = HashMap::new();
        edits.insert(age, NodeEdit { key_override: None, value_override: Some("99".to_owned()), deleted: false });
        let out = json_with_edits(&idx, idx.root, &edits, &[]);
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v, serde_json::json!({"name": "Alice", "age": 99}));
    }

    #[test]
    fn edits_override_object_key() {
        let idx = make(r#"{"age": 30}"#);
        let age = nav(&idx, &["age"]);
        let mut edits: HashMap<u32, NodeEdit> = HashMap::new();
        edits.insert(age, NodeEdit { key_override: Some("years".to_owned()), value_override: None, deleted: false });
        let out = json_with_edits(&idx, idx.root, &edits, &[]);
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v, serde_json::json!({"years": 30}));
    }

    #[test]
    fn edits_key_with_special_chars_is_escaped() {
        let idx = make(r#"{"a": 1}"#);
        let a = nav(&idx, &["a"]);
        let mut edits: HashMap<u32, NodeEdit> = HashMap::new();
        edits.insert(a, NodeEdit { key_override: Some("a\"b\tc".to_owned()), value_override: None, deleted: false });
        let out = json_with_edits(&idx, idx.root, &edits, &[]);
        // Output must still be valid JSON, and the key round-trips exactly.
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v, serde_json::json!({"a\"b\tc": 1}));
    }

    #[test]
    fn edits_delete_object_key() {
        let idx = make(r#"{"name": "Alice", "age": 30}"#);
        let age = nav(&idx, &["age"]);
        let mut edits: HashMap<u32, NodeEdit> = HashMap::new();
        edits.insert(age, NodeEdit { key_override: None, value_override: None, deleted: true });
        let out = json_with_edits(&idx, idx.root, &edits, &[]);
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v, serde_json::json!({"name": "Alice"}));
    }

    #[test]
    fn edits_delete_array_element() {
        let idx = make(r#"[1, 2, 3]"#);
        let two = nav(&idx, &["1"]);
        let mut edits: HashMap<u32, NodeEdit> = HashMap::new();
        edits.insert(two, NodeEdit { key_override: None, value_override: None, deleted: true });
        let out = json_with_edits(&idx, idx.root, &edits, &[]);
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v, serde_json::json!([1, 3]));
    }

    #[test]
    fn edits_delete_container_removes_subtree() {
        let idx = make(r#"{"a": {"x": 1}, "b": 2}"#);
        let a = nav(&idx, &["a"]);
        let mut edits: HashMap<u32, NodeEdit> = HashMap::new();
        edits.insert(a, NodeEdit { key_override: None, value_override: None, deleted: true });
        let out = json_with_edits(&idx, idx.root, &edits, &[]);
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v, serde_json::json!({"b": 2}));
    }

    #[test]
    fn edits_empty_overlay_roundtrips() {
        let idx = make(r#"{"a": {"x": 1, "y": [1, 2, 3]}, "b": "hi"}"#);
        let edits: HashMap<u32, NodeEdit> = HashMap::new();
        let out = json_with_edits(&idx, idx.root, &edits, &[]);
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v, serde_json::json!({"a": {"x": 1, "y": [1, 2, 3]}, "b": "hi"}));
    }

    #[test]
    fn edits_on_ndjson_produces_valid_array() {
        let idx = make("{\"a\":1}\n{\"a\":2}\n");
        assert!(idx.is_ndjson);
        // First row is the root array's first child; find its "a" child.
        let first_row = idx.first_child(idx.root);
        let mut a = idx.first_child(first_row);
        while a != u32::MAX && idx.key_of(&idx.nodes[a as usize]) != "a" {
            a = idx.nodes[a as usize].next_sibling;
        }
        let mut edits: HashMap<u32, NodeEdit> = HashMap::new();
        edits.insert(a, NodeEdit { key_override: None, value_override: Some("42".to_owned()), deleted: false });
        let out = json_with_edits(&idx, idx.root, &edits, &[]);
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v, serde_json::json!([{"a": 42}, {"a": 2}]));
    }

    #[test]
    fn compact_edits_apply_overrides_and_deletes() {
        let idx = make(r#"{"name": "Alice", "age": 30, "tags": ["a", "b"]}"#);
        let name = nav(&idx, &["name"]);
        let age  = nav(&idx, &["age"]);
        let mut edits: HashMap<u32, NodeEdit> = HashMap::new();
        edits.insert(name, NodeEdit { key_override: None, value_override: Some("\"Bob\"".to_owned()), deleted: false });
        edits.insert(age,  NodeEdit { key_override: None, value_override: None, deleted: true });
        let out = json_compact_with_edits(&idx, idx.root, &edits, &[]);
        assert_eq!(out, r#"{"name":"Bob","tags":["a","b"]}"#);
    }

    #[test]
    fn compact_edits_subtree_root() {
        let idx = make(r#"{"a": {"x": 1, "y": 2}}"#);
        let a = nav(&idx, &["a"]);
        let y = nav(&idx, &["a", "y"]);
        let mut edits: HashMap<u32, NodeEdit> = HashMap::new();
        edits.insert(y, NodeEdit { key_override: None, value_override: None, deleted: true });
        let out = json_compact_with_edits(&idx, a, &edits, &[]);
        assert_eq!(out, r#"{"x":1}"#);
    }

    // ── added items ─────────────────────────────────────────────────────────

    #[test]
    fn added_item_appends_to_array() {
        let idx = make(r#"[1, 2]"#);
        let added = vec![AddedItem { parent: idx.root, key: None, raw_value: "3".to_owned() }];
        let out = json_with_edits(&idx, idx.root, &HashMap::new(), &added);
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v, serde_json::json!([1, 2, 3]));
    }

    #[test]
    fn added_items_preserve_insertion_order() {
        let idx = make(r#"[1]"#);
        let added = vec![
            AddedItem { parent: idx.root, key: None, raw_value: "\"a\"".to_owned() },
            AddedItem { parent: idx.root, key: None, raw_value: "\"b\"".to_owned() },
        ];
        let out = json_compact_with_edits(&idx, idx.root, &HashMap::new(), &added);
        assert_eq!(out, r#"[1,"a","b"]"#);
    }

    #[test]
    fn added_item_edited_after_add_uses_override() {
        let idx = make(r#"[1]"#);
        let added = vec![AddedItem { parent: idx.root, key: None, raw_value: "2".to_owned() }];
        let synthetic_id = idx.nodes.len() as u32; // first (and only) synthetic id
        let mut edits: HashMap<u32, NodeEdit> = HashMap::new();
        edits.insert(synthetic_id, NodeEdit { key_override: None, value_override: Some("99".to_owned()), deleted: false });
        let out = json_compact_with_edits(&idx, idx.root, &edits, &added);
        assert_eq!(out, "[1,99]");
    }

    #[test]
    fn added_item_marked_deleted_is_excluded() {
        let idx = make(r#"[1]"#);
        let added = vec![AddedItem { parent: idx.root, key: None, raw_value: "2".to_owned() }];
        let synthetic_id = idx.nodes.len() as u32;
        let mut edits: HashMap<u32, NodeEdit> = HashMap::new();
        edits.insert(synthetic_id, NodeEdit { key_override: None, value_override: None, deleted: true });
        let out = json_compact_with_edits(&idx, idx.root, &edits, &added);
        assert_eq!(out, "[1]");
    }

    #[test]
    fn added_property_appends_to_object() {
        let idx = make(r#"{"a": 1}"#);
        let added = vec![AddedItem { parent: idx.root, key: Some("b".to_owned()), raw_value: "2".to_owned() }];
        let out = json_with_edits(&idx, idx.root, &HashMap::new(), &added);
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v, serde_json::json!({"a": 1, "b": 2}));
    }

    #[test]
    fn added_property_key_can_be_edited_after_add() {
        let idx = make(r#"{"a": 1}"#);
        let added = vec![AddedItem { parent: idx.root, key: Some("b".to_owned()), raw_value: "2".to_owned() }];
        let synthetic_id = idx.nodes.len() as u32;
        let mut edits: HashMap<u32, NodeEdit> = HashMap::new();
        edits.insert(synthetic_id, NodeEdit { key_override: Some("renamed".to_owned()), value_override: None, deleted: false });
        let out = json_compact_with_edits(&idx, idx.root, &edits, &added);
        assert_eq!(out, r#"{"a":1,"renamed":2}"#);
    }

    #[test]
    fn added_display_index_continues_from_child_count() {
        let idx = make(r#"[1, 2]"#);
        let added = vec![
            AddedItem { parent: idx.root, key: None, raw_value: "3".to_owned() },
            AddedItem { parent: idx.root, key: None, raw_value: "4".to_owned() },
        ];
        let base = idx.nodes.len() as u32;
        assert_eq!(added_display_index(&idx.nodes, &added, base), 2);
        assert_eq!(added_display_index(&idx.nodes, &added, base + 1), 3);
    }
}
