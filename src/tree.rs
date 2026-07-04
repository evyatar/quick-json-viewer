use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use crate::export::{self, AddedItem};
use crate::index::{JsonIndex, Node, NodeKind, NodeSet};

pub struct TreeState {
    pub index:             Arc<JsonIndex>,
    pub expanded:          NodeSet,
    pub visible:           Vec<u32>,
    pub selected:          Option<u32>,
    /// When true, rows show a checkbox and `checked` drives multi-node export.
    pub multi_select:      bool,
    pub checked:           HashSet<u32>,
    pub search_use_regex:  bool,
    pub search_results:    Vec<u32>,
    pub search_cursor:     usize,
    pub search_result_set: NodeSet,
    pub scroll_to_row:     Option<usize>,
    pub reveal_row:        Option<usize>,
    /// Array items added interactively but not yet saved to disk. Identified
    /// by synthetic ids beyond the real node range (see [`export::AddedItem`]).
    pub added_items:       Vec<AddedItem>,
}

impl TreeState {
    pub fn new(index: Arc<JsonIndex>) -> Self {
        let root = index.root;
        let mut expanded = NodeSet::new();
        expanded.insert(root);
        let visible = rebuild_visible(root, &index.nodes, &expanded, &HashMap::new());
        Self {
            index,
            expanded,
            visible,
            selected: Some(root),
            multi_select: false,
            checked: HashSet::new(),
            search_use_regex: false,
            search_results: Vec::new(),
            search_cursor: 0,
            search_result_set: NodeSet::new(),
            scroll_to_row: None,
            reveal_row: None,
            added_items: Vec::new(),
        }
    }

    /// True when `node_idx` is a pending added item rather than a real node.
    pub fn is_added(&self, node_idx: u32) -> bool {
        export::is_added(self.index.nodes.len(), node_idx)
    }

    pub fn added_item(&self, node_idx: u32) -> &AddedItem {
        &self.added_items[node_idx as usize - self.index.nodes.len()]
    }

    pub fn added_parent(&self, node_idx: u32) -> u32 {
        self.added_item(node_idx).parent
    }

    fn build_added_map(&self) -> HashMap<u32, Vec<u32>> {
        export::build_added_map(self.index.nodes.len(), &self.added_items)
    }

    /// Append a new pending item to `parent` (an Array or Object node —
    /// `key` is `Some` for an object property, `None` for an array element),
    /// expand it so the item is visible, and return the new item's synthetic id.
    pub fn add_item(&mut self, parent: u32, key: Option<String>, raw_value: String) -> u32 {
        self.added_items.push(AddedItem { parent, key, raw_value });
        let new_id = (self.index.nodes.len() + self.added_items.len() - 1) as u32;
        self.expanded.insert(parent);
        self.refresh_visible();
        new_id
    }

    /// Undo the most recent [`add_item`] — removes the last pending item.
    /// Relies on strict undo/redo LIFO ordering: the add being undone is
    /// always the most recently pushed entry (see `App::push_undo_add`).
    pub fn remove_last_added_item(&mut self) {
        let Some(item) = self.added_items.pop() else { return };
        let removed_id = (self.index.nodes.len() + self.added_items.len()) as u32;
        if self.selected == Some(removed_id) {
            self.selected = Some(item.parent);
        }
        self.checked.remove(&removed_id);
        self.search_result_set.remove(&removed_id);
        self.refresh_visible();
    }

    pub fn toggle_check(&mut self, node_idx: u32) {
        if !self.checked.insert(node_idx) {
            self.checked.remove(&node_idx);
        }
    }

    /// Turn multi-select mode on/off; leaving it clears the checked set.
    pub fn set_multi_select(&mut self, on: bool) {
        self.multi_select = on;
        if !on {
            self.checked.clear();
        }
    }

    pub fn toggle(&mut self, node_idx: u32) {
        if self.expanded.contains(&node_idx) {
            self.expanded.remove(&node_idx);
        } else {
            self.expanded.insert(node_idx);
        }
        self.refresh_visible();
    }

    pub fn refresh_visible(&mut self) {
        // The root has no caret and is always expanded.
        self.expanded.insert(self.index.root);
        let added_map = self.build_added_map();
        self.visible = rebuild_visible(self.index.root, &self.index.nodes, &self.expanded, &added_map);
    }

    /// Row index of `id` in `visible`. With no pending added items the DFS
    /// pre-order guarantees ascending node ids, so binary search applies;
    /// added items break the ordering, so fall back to a linear scan.
    pub fn pos_of(&self, id: u32) -> Option<usize> {
        if self.added_items.is_empty() {
            let p = self.visible.partition_point(|&n| n < id);
            (self.visible.get(p) == Some(&id)).then_some(p)
        } else {
            self.visible.iter().position(|&n| n == id)
        }
    }

    pub fn select_up(&mut self) {
        if let Some(sel) = self.selected {
            if let Some(pos) = self.pos_of(sel) {
                if pos > 0 {
                    self.selected = Some(self.visible[pos - 1]);
                    self.reveal_row = Some(pos - 1);
                }
            }
        }
    }

    pub fn select_down(&mut self) {
        if let Some(sel) = self.selected {
            if let Some(pos) = self.pos_of(sel) {
                if pos + 1 < self.visible.len() {
                    self.selected = Some(self.visible[pos + 1]);
                    self.reveal_row = Some(pos + 1);
                }
            }
        }
    }

    pub fn select_page_up(&mut self, page: usize) {
        if let Some(sel) = self.selected {
            if let Some(pos) = self.pos_of(sel) {
                let new_pos = pos.saturating_sub(page);
                self.selected = Some(self.visible[new_pos]);
                self.scroll_to_row = Some(new_pos);
            }
        }
    }

    pub fn select_page_down(&mut self, page: usize) {
        if let Some(sel) = self.selected {
            if let Some(pos) = self.pos_of(sel) {
                let new_pos = (pos + page).min(self.visible.len().saturating_sub(1));
                self.selected = Some(self.visible[new_pos]);
                self.scroll_to_row = Some(new_pos);
            }
        }
    }

    pub fn select_home(&mut self) {
        if !self.visible.is_empty() {
            self.selected = Some(self.visible[0]);
            self.scroll_to_row = Some(0);
        }
    }

    pub fn select_end(&mut self) {
        if let Some(last) = self.visible.last().copied() {
            let pos = self.visible.len() - 1;
            self.selected = Some(last);
            self.scroll_to_row = Some(pos);
        }
    }

    /// Collapse current node (if expanded), or jump to parent.
    pub fn select_left(&mut self) {
        let Some(sel) = self.selected else { return; };
        if self.is_added(sel) {
            self.selected = Some(self.added_parent(sel));
            return;
        }
        let node = &self.index.nodes[sel as usize];
        if matches!(node.kind, NodeKind::Object | NodeKind::Array)
            && self.expanded.contains(&sel)
        {
            self.toggle(sel);
        } else {
            let parent = node.parent;
            if parent != u32::MAX {
                self.selected = Some(parent);
            }
        }
    }

    /// Expand current node if it's a collapsed container.
    pub fn select_right(&mut self) {
        let Some(sel) = self.selected else { return; };
        if self.is_added(sel) {
            return; // added items are always leaves
        }
        let node = &self.index.nodes[sel as usize];
        if matches!(node.kind, NodeKind::Object | NodeKind::Array)
            && node.child_count > 0
            && !self.expanded.contains(&sel)
        {
            self.toggle(sel);
        }
    }

    pub fn search_next(&mut self) {
        if self.search_results.is_empty() {
            return;
        }
        self.search_cursor = (self.search_cursor + 1) % self.search_results.len();
        let node_idx = self.search_results[self.search_cursor];
        self.selected = Some(node_idx);
        self.ensure_visible(node_idx);
    }

    pub fn search_prev(&mut self) {
        if self.search_results.is_empty() {
            return;
        }
        self.search_cursor = if self.search_cursor == 0 {
            self.search_results.len() - 1
        } else {
            self.search_cursor - 1
        };
        let node_idx = self.search_results[self.search_cursor];
        self.selected = Some(node_idx);
        self.ensure_visible(node_idx);
    }

    /// Expand all ancestors of `node_idx` so it becomes visible, then rebuild.
    pub fn ensure_visible(&mut self, node_idx: u32) {
        let mut current = if self.is_added(node_idx) {
            let parent = self.added_parent(node_idx);
            self.expanded.insert(parent);
            parent
        } else {
            node_idx
        };
        loop {
            let parent = self.index.nodes[current as usize].parent;
            if parent == u32::MAX {
                break;
            }
            self.expanded.insert(parent);
            current = parent;
        }
        self.refresh_visible();
        self.scroll_to_row = self.pos_of(node_idx);
    }

    pub fn collapse_all(&mut self) {
        self.expanded.clear();
        self.refresh_visible();
    }

    pub fn expand_all(&mut self) {
        for (i, node) in self.index.nodes.iter().enumerate() {
            if matches!(node.kind, NodeKind::Object | NodeKind::Array) && node.child_count > 0 {
                self.expanded.insert(i as u32);
            }
        }
        self.refresh_visible();
    }

    pub fn collapse_recursive(&mut self, root: u32) {
        let mut stack = vec![root];
        while let Some(idx) = stack.pop() {
            let node = &self.index.nodes[idx as usize];
            if matches!(node.kind, NodeKind::Object | NodeKind::Array) && node.child_count > 0 {
                self.expanded.remove(&idx);
                let mut child = self.index.first_child(idx);
                while child != u32::MAX {
                    stack.push(child);
                    child = self.index.nodes[child as usize].next_sibling;
                }
            }
        }
        self.refresh_visible();
    }

    pub fn expand_recursive(&mut self, root: u32) {
        let mut stack = vec![root];
        while let Some(idx) = stack.pop() {
            let node = &self.index.nodes[idx as usize];
            if matches!(node.kind, NodeKind::Object | NodeKind::Array) && node.child_count > 0 {
                self.expanded.insert(idx);
                let mut child = self.index.first_child(idx);
                while child != u32::MAX {
                    stack.push(child);
                    child = self.index.nodes[child as usize].next_sibling;
                }
            }
        }
        self.refresh_visible();
    }

    /// Jump to the next sibling (within the same object) whose key starts with
    /// `prefix` (case-insensitive).  Single-char prefix cycles from current+1;
    /// multi-char prefix finds the first match from the beginning.
    pub fn type_ahead_select(&mut self, prefix: &str) {
        let Some(sel) = self.selected else { return };
        if self.is_added(sel) { return; } // added items are never object children

        let (parent_kind, first_child) = {
            let nodes = &self.index.nodes;
            let parent = nodes[sel as usize].parent;
            if parent == u32::MAX { return; }
            (nodes[parent as usize].kind, self.index.first_child(parent))
        };
        if !matches!(parent_kind, NodeKind::Object) { return; }

        let prefix_lower = prefix.to_lowercase();

        let siblings: Vec<u32> = {
            let nodes = &self.index.nodes;
            let mut v = Vec::new();
            let mut child = first_child;
            while child != u32::MAX {
                v.push(child);
                child = nodes[child as usize].next_sibling;
            }
            v
        };
        if siblings.is_empty() { return; }

        let cur_pos = siblings.iter().position(|&n| n == sel).unwrap_or(0);
        let n = siblings.len();
        // Single char: start from next sibling (cycle); multi-char: from start.
        let start = if prefix.chars().count() == 1 { 1 } else { 0 };

        let found = (0..n).find_map(|i| {
            let idx = (cur_pos + start + i) % n;
            let nid = siblings[idx];
            let key = self.index.key_of(&self.index.nodes[nid as usize]);
            if key.to_lowercase().starts_with(&prefix_lower) { Some(nid) } else { None }
        });

        if let Some(target) = found {
            self.selected = Some(target);
            if let Some(pos) = self.pos_of(target) {
                self.scroll_to_row = Some(pos);
            }
        }
    }

    /// Replace search results (called from UI after background search finishes).
    pub fn set_search_results(&mut self, results: Vec<u32>) {
        self.search_result_set.clear();
        for &r in &results {
            self.search_result_set.insert(r);
        }
        self.search_results = results;
        self.search_cursor = 0;
        if let Some(&first) = self.search_results.first() {
            self.selected = Some(first);
            self.ensure_visible(first);
        }
    }

}

/// Iterative DFS — avoids stack overflow on deeply nested files.
///
/// `added` maps a real container's node index to the synthetic ids of any
/// pending (not-yet-saved) items appended to it — see [`crate::export::AddedItem`].
/// Those ids are always emitted last, after the container's real children,
/// and never expand further (they're always leaves).
pub fn rebuild_visible(
    root: u32,
    nodes: &[Node],
    expanded: &NodeSet,
    added: &HashMap<u32, Vec<u32>>,
) -> Vec<u32> {
    let mut visible: Vec<u32> = Vec::with_capacity(nodes.len().min(4096));
    let mut stack: Vec<u32> = vec![root];

    while let Some(node_idx) = stack.pop() {
        visible.push(node_idx);
        if node_idx as usize >= nodes.len() {
            continue; // synthetic added item — always a leaf
        }
        let node = &nodes[node_idx as usize];
        let extra = added.get(&node_idx);
        let has_extra = extra.is_some_and(|v| !v.is_empty());
        if expanded.contains(&node_idx) && (node.child_count > 0 || has_extra) {
            // Push children onto the stack, then reverse that range in place
            // so the first child is popped first — no per-node temp Vec.
            let mark = stack.len();
            let mut child = crate::index::first_child(nodes, node_idx);
            while child != u32::MAX {
                stack.push(child);
                child = nodes[child as usize].next_sibling;
            }
            if let Some(extra) = extra {
                stack.extend(extra.iter().copied());
            }
            stack[mark..].reverse();
        }
    }

    visible
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::{JsonIndex, Node, NodeKind};
    use std::sync::Arc;

    fn make_node(
        kind: NodeKind,
        next_sibling: u32,
        child_count: u32,
        parent: u32,
    ) -> Node {
        Node {
            kind,
            depth: 0,
            value_start: 0,
            value_end: 0,
            key_start: 0,
            key_len: 0,
            next_sibling,
            child_count,
            parent,
            array_index: u32::MAX,
        }
    }

    fn make_index(json: &str) -> Arc<JsonIndex> {
        let data = json.as_bytes().to_vec();
        let (nodes, root, is_ndjson) =
            crate::parser::parse_bytes(&data, &mut |_| {}).unwrap();
        Arc::new(JsonIndex {
            data: crate::index::JsonData::Memory(data),
            nodes, root, is_ndjson,
        })
    }

    // ── rebuild_visible ──────────────────────────────────────────────────────

    #[test]
    fn root_only_when_nothing_expanded() {
        let ns = vec![
            make_node(NodeKind::Object, u32::MAX, 2, u32::MAX), // 0 root
            make_node(NodeKind::String, 2, 0, 0),               // 1 a
            make_node(NodeKind::String, u32::MAX, 0, 0),        // 2 b
        ];
        let visible = rebuild_visible(0, &ns, &NodeSet::new(), &HashMap::new());
        assert_eq!(visible, vec![0]);
    }

    #[test]
    fn expanded_root_shows_children() {
        let ns = vec![
            make_node(NodeKind::Object, u32::MAX, 2, u32::MAX),
            make_node(NodeKind::String, 2, 0, 0),
            make_node(NodeKind::String, u32::MAX, 0, 0),
        ];
        let mut expanded = NodeSet::new();
        expanded.insert(0u32);
        assert_eq!(rebuild_visible(0, &ns, &expanded, &HashMap::new()), vec![0, 1, 2]);
    }

    #[test]
    fn dfs_order_is_preserved() {
        // Pre-order layout: root(0) -> a(1) -> c(2); root(0) -> b(3)
        let ns = vec![
            make_node(NodeKind::Object, u32::MAX, 2, u32::MAX), // 0 root
            make_node(NodeKind::Object, 3, 1, 0),               // 1 a
            make_node(NodeKind::String, u32::MAX, 0, 1),        // 2 c (child of a)
            make_node(NodeKind::String, u32::MAX, 0, 0),        // 3 b
        ];
        let mut expanded = NodeSet::new();
        expanded.insert(0u32);
        expanded.insert(1u32);
        assert_eq!(rebuild_visible(0, &ns, &expanded, &HashMap::new()), vec![0, 1, 2, 3]);
    }

    // ── TreeState ────────────────────────────────────────────────────────────

    #[test]
    fn new_state_expands_root() {
        let idx = make_index(r#"[1, 2, 3]"#);
        let state = TreeState::new(idx);
        assert_eq!(state.visible.len(), 4); // root + 3 items
        assert_eq!(state.selected, Some(state.index.root));
    }

    #[test]
    fn select_down_advances_selection() {
        let idx = make_index(r#"[1, 2, 3]"#);
        let mut state = TreeState::new(idx);
        let initial = state.selected;
        state.select_down();
        assert_ne!(state.selected, initial);
        assert_eq!(state.selected, Some(state.visible[1]));
        assert_eq!(state.scroll_to_row, None);
        assert_eq!(state.reveal_row, Some(1));
    }

    #[test]
    fn select_up_from_root_stays_at_root() {
        let idx = make_index(r#"[1, 2]"#);
        let mut state = TreeState::new(idx);
        state.select_up();
        assert_eq!(state.selected, Some(state.visible[0]));
    }

    #[test]
    fn home_and_end_navigate_to_extremes() {
        let idx = make_index(r#"[1, 2, 3]"#);
        let mut state = TreeState::new(idx);
        state.select_end();
        assert_eq!(state.selected, state.visible.last().copied());
        state.select_home();
        assert_eq!(state.selected, Some(state.visible[0]));
    }

    #[test]
    fn collapse_all_hides_all_children() {
        let idx = make_index(r#"{"a": {"b": 1}}"#);
        let mut state = TreeState::new(idx);
        state.expand_all();
        assert!(state.visible.len() > 1);
        state.collapse_all();
        // refresh_visible always re-inserts root, so root's direct children stay visible
        assert_eq!(state.visible.len(), 2); // root + "a" (root is always expanded)
    }

    #[test]
    fn ensure_visible_expands_ancestors() {
        let idx = make_index(r#"{"a": {"b": 1}}"#);
        let mut state = TreeState::new(idx);
        // Clear expanded — refresh_visible will re-add root only
        state.expanded.clear();
        state.refresh_visible();
        let root = state.index.root;
        let a_node = state.index.first_child(root);
        let b_node = state.index.first_child(a_node);
        // b_node is hidden because a_node is not expanded
        assert!(!state.visible.contains(&b_node));
        state.ensure_visible(b_node);
        assert!(state.visible.contains(&b_node));
    }

    #[test]
    fn toggle_expands_then_collapses() {
        let idx = make_index(r#"{"a": {"b": 1}}"#);
        let mut state = TreeState::new(idx);
        let root = state.index.root;
        let child = state.index.first_child(root); // "a" node
        // Root starts expanded; "a" is visible but collapsed
        assert_eq!(state.visible.len(), 2); // root + "a"
        state.toggle(child);
        assert_eq!(state.visible.len(), 3); // root + "a" + "b"
        state.toggle(child);
        assert_eq!(state.visible.len(), 2); // collapsed again
    }

    // ── added items ─────────────────────────────────────────────────────────

    #[test]
    fn add_item_appears_as_trailing_visible_row() {
        let idx = make_index(r#"[1, 2]"#);
        let mut state = TreeState::new(idx);
        let root = state.index.root;
        assert_eq!(state.visible.len(), 3); // root + 2 items
        let new_id = state.add_item(root, None, "3".to_owned());
        assert!(state.is_added(new_id));
        let first = state.index.first_child(root);
        assert_eq!(state.visible, vec![root, first,
            state.index.nodes[first as usize].next_sibling, new_id]);
    }

    #[test]
    fn add_item_auto_expands_collapsed_parent() {
        let idx = make_index(r#"[[1]]"#);
        let mut state = TreeState::new(idx);
        let root = state.index.root;
        let inner = state.index.first_child(root);
        state.expanded.remove(&inner);
        state.refresh_visible();
        assert!(!state.visible.contains(&state.index.first_child(inner)));
        let new_id = state.add_item(inner, None, "2".to_owned());
        // Adding auto-expands the parent so the new item is immediately visible.
        assert!(state.expanded.contains(&inner));
        assert!(state.visible.contains(&new_id));
    }

    #[test]
    fn add_item_with_key_appends_to_object() {
        let idx = make_index(r#"{"a": 1}"#);
        let mut state = TreeState::new(idx);
        let root = state.index.root;
        let new_id = state.add_item(root, Some("b".to_owned()), "2".to_owned());
        assert!(state.is_added(new_id));
        assert_eq!(state.added_item(new_id).key.as_deref(), Some("b"));
    }

    #[test]
    fn remove_last_added_item_reselects_parent() {
        let idx = make_index(r#"[1]"#);
        let mut state = TreeState::new(idx);
        let root = state.index.root;
        let new_id = state.add_item(root, None, "2".to_owned());
        state.selected = Some(new_id);
        state.remove_last_added_item();
        assert!(state.added_items.is_empty());
        assert_eq!(state.selected, Some(root));
        assert!(!state.visible.contains(&new_id));
    }

    #[test]
    fn added_items_do_not_expand_or_select_right() {
        let idx = make_index(r#"[1]"#);
        let mut state = TreeState::new(idx);
        let root = state.index.root;
        let new_id = state.add_item(root, None, "2".to_owned());
        state.selected = Some(new_id);
        state.select_right(); // no-op, must not panic
        assert_eq!(state.selected, Some(new_id));
        state.select_left(); // jumps to parent
        assert_eq!(state.selected, Some(root));
    }
}
