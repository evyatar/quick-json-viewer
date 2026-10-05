//! Copy a value across the Compare view.
//!
//! Makes one side match the other at a single merged-diff node: replace the
//! target value, insert a member the target lacks, or delete one only the
//! target has. Compare panes hold immutable indices with no edit overlay, so
//! the edit is a byte splice on the target document — everything outside the
//! touched span keeps its original formatting — and the caller reparses the
//! returned bytes.

use std::borrow::Cow;

use crate::diff::{DiffResult, DiffStatus};
use crate::export;
use crate::index::{JsonIndex, NodeKind};
use crate::Side;

fn doc(result: &DiffResult, side: Side) -> &JsonIndex {
    match side { Side::Left => &result.left, Side::Right => &result.right }
}

/// The node on `side` that merged node `node` refers to, if present there.
fn side_idx(result: &DiffResult, node: u32, side: Side) -> Option<u32> {
    let dn = &result.nodes[node as usize];
    match side { Side::Left => dn.left_idx(), Side::Right => dn.right_idx() }
}

/// Whether merged node `node` can be made to match on side `to`. Only rows
/// that differ qualify, and — apart from the root — only those whose parent
/// exists on `to`: inside a subtree that only the right side has, a row can
/// still be deleted from the right, but not inserted on the left (there is no
/// parent there to hold it).
pub fn can_copy(result: &DiffResult, node: u32, to: Side) -> bool {
    let Some(dn) = result.nodes.get(node as usize) else { return false };
    if dn.status == DiffStatus::Unchanged {
        return false;
    }
    node == result.root || side_idx(result, dn.parent, to).is_some()
}

/// The new bytes of the `to` document after making it match the other side at
/// merged node `node`, or `None` when the node can't be copied.
pub fn apply(result: &DiffResult, node: u32, to: Side) -> Option<Vec<u8>> {
    if !can_copy(result, node, to) {
        return None;
    }
    let src = doc(result, to.other());
    let dst = doc(result, to);
    if node == result.root {
        return Some(src.data.bytes().to_vec());
    }
    match (side_idx(result, node, to.other()), side_idx(result, node, to)) {
        (Some(s), Some(t)) => {
            let tn = &dst.nodes[t as usize];
            let text = value_text(src, s, dst.is_ndjson);
            Some(splice(dst.data.bytes(), tn.value_start as usize, tn.value_end as usize, &text))
        }
        (Some(s), None) => insert(result, node, src, s, dst, to),
        (None, Some(t)) => Some(delete(dst, t)),
        (None, None) => None,
    }
}

/// Insert source node `s` into the target-side parent of merged node `node`,
/// right after the nearest preceding merged sibling that exists on the target
/// (so key / element order follows the source).
fn insert(result: &DiffResult, node: u32, src: &JsonIndex, s: u32, dst: &JsonIndex, to: Side) -> Option<Vec<u8>> {
    let parent = result.nodes[node as usize].parent;
    let p = side_idx(result, parent, to)?;
    let pn = &dst.nodes[p as usize];

    let mut text: Vec<u8> = Vec::new();
    match pn.kind {
        NodeKind::Object => {
            text.push(b'"');
            text.extend_from_slice(key_bytes(src, s));
            text.extend_from_slice(b"\": ");
        }
        NodeKind::Array => {}
        _ => return None,
    }
    text.extend_from_slice(&value_text(src, s, dst.is_ndjson));

    let mut anchor = None;
    let mut c = result.nodes[parent as usize].first_child;
    while c != node && c != u32::MAX {
        if let Some(t) = side_idx(result, c, to) {
            anchor = Some(t);
        }
        c = result.nodes[c as usize].next_sibling;
    }

    // NDJSON records are separated by newlines, not commas.
    let sep: &[u8] = if dst.is_ndjson && p == dst.root { b"\n" } else { b", " };
    let bytes = dst.data.bytes();
    Some(match anchor {
        Some(a) => {
            let at = dst.nodes[a as usize].value_end as usize;
            splice(bytes, at, at, &[sep, &text].concat())
        }
        None if pn.child_count > 0 => {
            let at = member_start(dst, dst.first_child(p));
            splice(bytes, at, at, &[&text, sep].concat())
        }
        None => {
            let at = pn.value_start as usize + 1;
            splice(bytes, at, at, &text)
        }
    })
}

/// Remove target node `t` together with one adjacent separator.
fn delete(dst: &JsonIndex, t: u32) -> Vec<u8> {
    let bytes = dst.data.bytes();
    let n = &dst.nodes[t as usize];
    let (start, end) = if n.next_sibling != u32::MAX {
        (member_start(dst, t), member_start(dst, n.next_sibling))
    } else if let Some(prev) = prev_sibling(dst, t) {
        (dst.nodes[prev as usize].value_end as usize, n.value_end as usize)
    } else {
        // Only member: also swallow a trailing comma, which the parser accepts
        // after a member but not on its own (`{,}`).
        let mut end = n.value_end as usize;
        let mut p = end;
        while p < bytes.len() && bytes[p].is_ascii_whitespace() { p += 1; }
        if bytes.get(p) == Some(&b',') { end = p + 1; }
        (member_start(dst, t), end)
    };
    splice(bytes, start, end, b"")
}

/// The source text to write for node `s`. NDJSON targets need it on one line.
fn value_text(src: &JsonIndex, s: u32, one_line: bool) -> Cow<'_, [u8]> {
    if one_line {
        Cow::Owned(export::json_compact(src, s).into_bytes())
    } else {
        Cow::Borrowed(export::json_verbatim(src, s))
    }
}

/// Raw (still-escaped) key bytes of node `n`; empty for `""` and non-members.
fn key_bytes(idx: &JsonIndex, n: u32) -> &[u8] {
    let node = &idx.nodes[n as usize];
    if node.key_len == 0 {
        return b"";
    }
    let s = (node.value_start - node.key_start) as usize;
    &idx.data.bytes()[s..s + node.key_len as usize]
}

/// Byte offset where member `n` begins: the opening quote of its key for an
/// object member, else the value itself.
fn member_start(idx: &JsonIndex, n: u32) -> usize {
    let node = &idx.nodes[n as usize];
    if node.key_len > 0 {
        return (node.value_start - node.key_start) as usize - 1;
    }
    let in_object = idx.nodes.get(node.parent as usize).is_some_and(|p| p.kind == NodeKind::Object);
    if !in_object {
        return node.value_start as usize;
    }
    // The empty key `""` isn't recorded (key_len 0 means "no key"): walk back
    // over `"" :` from the value.
    let bytes = idx.data.bytes();
    let mut p = node.value_start as usize;
    while p > 0 && bytes[p - 1].is_ascii_whitespace() { p -= 1; }
    if p > 0 && bytes[p - 1] == b':' { p -= 1; }
    while p > 0 && bytes[p - 1].is_ascii_whitespace() { p -= 1; }
    p.saturating_sub(2)
}

fn prev_sibling(idx: &JsonIndex, n: u32) -> Option<u32> {
    let parent = idx.nodes[n as usize].parent;
    if parent == u32::MAX {
        return None;
    }
    let mut c = idx.first_child(parent);
    let mut prev = None;
    while c != u32::MAX && c != n {
        prev = Some(c);
        c = idx.nodes[c as usize].next_sibling;
    }
    prev
}

fn splice(bytes: &[u8], start: usize, end: usize, ins: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(bytes.len() - (end - start) + ins.len());
    out.extend_from_slice(&bytes[..start]);
    out.extend_from_slice(ins);
    out.extend_from_slice(&bytes[end..]);
    out
}

// ─── tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diff::{self, DiffOptions};
    use crate::index::JsonData;
    use std::sync::Arc;

    fn idx_bytes(data: Vec<u8>) -> Arc<JsonIndex> {
        let (nodes, root, is_ndjson) = crate::parser::parse_bytes(&data, &mut |_| {})
            .unwrap_or_else(|e| panic!("reparse failed: {e} in {:?}", String::from_utf8_lossy(&data)));
        Arc::new(JsonIndex { data: JsonData::Memory(data), nodes, root, is_ndjson })
    }

    fn idx(json: &str) -> Arc<JsonIndex> {
        idx_bytes(json.as_bytes().to_vec())
    }

    fn run(l: &str, r: &str) -> DiffResult {
        diff::diff(idx(l), idx(r), &DiffOptions::default())
    }

    fn find(res: &DiffResult, path: &str) -> u32 {
        (0..res.nodes.len() as u32)
            .find(|&i| diff::node_path(res, i) == path)
            .unwrap_or_else(|| panic!("no merged node at {path}"))
    }

    /// Copy the merged node at `path` onto `to` (`None` = the first
    /// difference); returns the new target text.
    fn copy_at(l: &str, r: &str, path: Option<&str>, to: Side) -> String {
        let res = run(l, r);
        let node = match path {
            Some(p) => find(&res, p),
            None    => res.diff_positions[0],
        };
        let out = apply(&res, node, to).expect("copy refused");
        idx_bytes(out.clone()); // must still parse
        String::from_utf8(out).unwrap()
    }

    fn copy(l: &str, r: &str, path: &str, to: Side) -> String {
        copy_at(l, r, Some(path), to)
    }

    fn value(s: &str) -> serde_json::Value {
        serde_json::from_str(s).unwrap()
    }

    #[test]
    fn replace_scalar_keeps_surrounding_formatting() {
        let out = copy("{\n  \"a\": 1,\n  \"b\": 2\n}", r#"{"a": 1, "b": 3}"#, "$.b", Side::Left);
        assert_eq!(out, "{\n  \"a\": 1,\n  \"b\": 3\n}");
    }

    #[test]
    fn replace_container_copies_whole_subtree() {
        let out = copy(r#"{"o": {"x": 1}}"#, r#"{"o": {"x": 2, "y": [true]}}"#, "$.o", Side::Left);
        assert_eq!(value(&out), value(r#"{"o": {"x": 2, "y": [true]}}"#));
    }

    #[test]
    fn replace_left_to_right() {
        let out = copy(r#"{"a": "new"}"#, r#"{"a": "old"}"#, "$.a", Side::Right);
        assert_eq!(out, r#"{"a": "new"}"#);
    }

    #[test]
    fn type_change_is_replaced() {
        let out = copy(r#"{"a": 1}"#, r#"{"a": [1, 2]}"#, "$.a", Side::Left);
        assert_eq!(out, r#"{"a": [1, 2]}"#);
    }

    #[test]
    fn insert_after_anchor_follows_source_order() {
        // `b` is removed on the right; copying it right lands after `a`.
        let out = copy(r#"{"a": 1, "b": 2, "c": 3}"#, r#"{"a": 1, "c": 3}"#, "$.b", Side::Right);
        assert_eq!(out, r#"{"a": 1, "b": 2, "c": 3}"#);
    }

    #[test]
    fn insert_at_first_position() {
        let out = copy(r#"{"a": 1, "b": 2}"#, r#"{"b": 2}"#, "$.a", Side::Right);
        assert_eq!(out, r#"{"a": 1, "b": 2}"#);
    }

    #[test]
    fn insert_into_empty_object() {
        let out = copy(r#"{"o": {"k": "v"}}"#, r#"{"o": {}}"#, "$.o.k", Side::Right);
        assert_eq!(out, r#"{"o": {"k": "v"}}"#);
    }

    #[test]
    fn insert_added_key_into_left() {
        let out = copy(r#"{"a": 1}"#, r#"{"a": 1, "z": {"n": null}}"#, "$.z", Side::Left);
        assert_eq!(value(&out), value(r#"{"a": 1, "z": {"n": null}}"#));
    }

    #[test]
    fn insert_preserves_escaped_key() {
        let out = copy_at(r#"{}"#, r#"{"a\"b": 1}"#, None, Side::Left);
        assert_eq!(out, r#"{"a\"b": 1}"#);
    }

    #[test]
    fn insert_array_element() {
        let out = copy("[1, 2, 3]", "[1, 2]", "$[2]", Side::Right);
        assert_eq!(out, "[1, 2, 3]");
        let out = copy("[]", "[7]", "$[0]", Side::Left);
        assert_eq!(out, "[7]");
    }

    #[test]
    fn delete_middle_last_and_only_member() {
        // Copying an absent value means deleting the target's member.
        let out = copy(r#"{"a": 1, "c": 3}"#, r#"{"a": 1, "b": 2, "c": 3}"#, "$.b", Side::Right);
        assert_eq!(out, r#"{"a": 1, "c": 3}"#);
        let out = copy(r#"{"a": 1}"#, r#"{"a": 1, "b": 2}"#, "$.b", Side::Right);
        assert_eq!(out, r#"{"a": 1}"#);
        let out = copy(r#"{}"#, r#"{"b": 2}"#, "$.b", Side::Right);
        assert_eq!(out, r#"{}"#);
        let out = copy("[1]", "[1, 2]", "$[1]", Side::Right);
        assert_eq!(out, "[1]");
    }

    #[test]
    fn delete_only_member_with_trailing_comma() {
        let out = copy(r#"{}"#, r#"{"b": 2,}"#, "$.b", Side::Right);
        assert_eq!(value(&out), value("{}"));
    }

    #[test]
    fn empty_key_members() {
        let out = copy_at(r#"{"a": 1}"#, r#"{"a": 1, "": 2}"#, None, Side::Right);
        assert_eq!(out, r#"{"a": 1}"#);
        let out = copy_at(r#"{"a": 1}"#, r#"{"": 2, "a": 1}"#, None, Side::Left);
        assert_eq!(value(&out), value(r#"{"a": 1, "": 2}"#));
    }

    #[test]
    fn root_copy_takes_the_whole_document() {
        let out = copy(r#"{"a": 1}"#, "[1,\n 2]", "$", Side::Left);
        assert_eq!(out, "[1,\n 2]");
    }

    #[test]
    fn ndjson_target_stays_line_delimited() {
        let l = "{\"id\":1}\n{\"id\":2}\n";
        // Insert a record: compact, on its own line.
        let out = copy(l, "{\"id\":1}\n{\"id\":2}\n{\"id\":3,\n\"x\": [1]}", "$[2]", Side::Left);
        assert_eq!(out, "{\"id\":1}\n{\"id\":2}\n{\"id\":3,\"x\":[1]}\n");
        assert!(idx(&out).is_ndjson);
        // Replace inside a record with a multi-line value.
        let out = copy(l, "{\"id\":1}\n{\"id\":{\n\"n\": 5}}\n", "$[1].id", Side::Left);
        assert_eq!(out, "{\"id\":1}\n{\"id\":{\"n\":5}}\n");
        // Delete a record.
        let out = copy("{\"id\":1}\n{\"id\":2}\n{\"id\":3}\n", l, "$[2]", Side::Left);
        assert_eq!(out, l);
    }

    #[test]
    fn rows_inside_an_added_subtree_can_only_be_deleted() {
        let res = run(r#"{}"#, r#"{"o": {"x": 1, "y": 2}}"#);
        let o = find(&res, "$.o");
        let x = find(&res, "$.o.x");
        assert!(can_copy(&res, o, Side::Left));
        assert!(can_copy(&res, o, Side::Right));
        // No `o` on the left to insert `x` into …
        assert!(!can_copy(&res, x, Side::Left));
        assert!(apply(&res, x, Side::Left).is_none());
        // … but it can be deleted from the right.
        assert!(can_copy(&res, x, Side::Right));
        let out = String::from_utf8(apply(&res, x, Side::Right).unwrap()).unwrap();
        assert_eq!(out, r#"{"o": {"y": 2}}"#);
    }

    #[test]
    fn rows_inside_a_removed_subtree_can_only_be_deleted() {
        let res = run(r#"{"a": [1, 2]}"#, r#"{}"#);
        let el = find(&res, "$.a[1]");
        assert!(!can_copy(&res, el, Side::Right));
        assert!(can_copy(&res, el, Side::Left));
        let out = String::from_utf8(apply(&res, el, Side::Left).unwrap()).unwrap();
        assert_eq!(out, r#"{"a": [1]}"#);
    }

    #[test]
    fn unchanged_rows_are_not_copyable() {
        let res = run(r#"{"a": 1, "b": 2}"#, r#"{"a": 1, "b": 3}"#);
        assert!(!can_copy(&res, find(&res, "$.a"), Side::Left));
        assert!(can_copy(&res, find(&res, "$.b"), Side::Left));
    }

    /// Repeatedly copying the first remaining difference converges on an
    /// identical document, re-parsing cleanly at every step.
    #[test]
    fn copying_every_difference_converges() {
        let l = r#"{"name": "a", "tags": ["x", "y", "z"], "o": {"k": 1, "gone": true}, "n": [1, {"d": 2}]}"#;
        let r = r#"{"name": "b", "tags": ["x"], "o": {"k": 2, "new": null}, "extra": {"e": []}, "n": [1, {"d": 3}, 4]}"#;
        for to in [Side::Left, Side::Right] {
            let (mut left, mut right) = (idx(l), idx(r));
            for _ in 0..50 {
                let res = diff::diff(Arc::clone(&left), Arc::clone(&right), &DiffOptions::default());
                let Some(&first) = res.diff_positions.first() else { break };
                let out = idx_bytes(apply(&res, first, to).expect("copy refused"));
                match to { Side::Left => left = out, Side::Right => right = out }
            }
            let res = diff::diff(Arc::clone(&left), Arc::clone(&right), &DiffOptions::default());
            assert_eq!(res.nodes[res.root as usize].status, DiffStatus::Unchanged, "{to:?}");
            let want = if to == Side::Left { r } else { l };
            let got = if to == Side::Left { &left } else { &right };
            assert_eq!(value(std::str::from_utf8(got.data.bytes()).unwrap()), value(want));
        }
    }
}
