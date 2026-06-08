use std::collections::HashSet;
use std::sync::Arc;

use crate::index::{JsonIndex, Node, NodeKind};

pub struct TreeState {
    pub index:             Arc<JsonIndex>,
    pub expanded:          HashSet<u32>,
    pub visible:           Vec<u32>,
    pub selected:          Option<u32>,
    pub search_use_regex:  bool,
    pub search_results:    Vec<u32>,
    pub search_cursor:     usize,
    pub search_result_set: HashSet<u32>,
    pub scroll_to_row:     Option<usize>,
}

impl TreeState {
    pub fn new(index: Arc<JsonIndex>) -> Self {
        let root = index.root;
        let mut expanded = HashSet::new();
        expanded.insert(root);
        let visible = rebuild_visible(root, &index.nodes, &expanded);
        Self {
            index,
            expanded,
            visible,
            selected: Some(root),
            search_use_regex: false,
            search_results: Vec::new(),
            search_cursor: 0,
            search_result_set: HashSet::new(),
            scroll_to_row: None,
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
        self.visible = rebuild_visible(self.index.root, &self.index.nodes, &self.expanded);
    }

    pub fn select_up(&mut self) {
        if let Some(sel) = self.selected {
            if let Some(pos) = self.visible.iter().position(|&n| n == sel) {
                if pos > 0 {
                    self.selected = Some(self.visible[pos - 1]);
                    self.scroll_to_row = Some(pos - 1);
                }
            }
        }
    }

    pub fn select_down(&mut self) {
        if let Some(sel) = self.selected {
            if let Some(pos) = self.visible.iter().position(|&n| n == sel) {
                if pos + 1 < self.visible.len() {
                    self.selected = Some(self.visible[pos + 1]);
                    self.scroll_to_row = Some(pos + 1);
                }
            }
        }
    }

    pub fn select_page_up(&mut self, page: usize) {
        if let Some(sel) = self.selected {
            if let Some(pos) = self.visible.iter().position(|&n| n == sel) {
                let new_pos = pos.saturating_sub(page);
                self.selected = Some(self.visible[new_pos]);
                self.scroll_to_row = Some(new_pos);
            }
        }
    }

    pub fn select_page_down(&mut self, page: usize) {
        if let Some(sel) = self.selected {
            if let Some(pos) = self.visible.iter().position(|&n| n == sel) {
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
        let mut current = node_idx;
        loop {
            let parent = self.index.nodes[current as usize].parent;
            if parent == u32::MAX {
                break;
            }
            self.expanded.insert(parent);
            current = parent;
        }
        self.refresh_visible();
        self.scroll_to_row = self.visible.iter().position(|&n| n == node_idx);
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
                let mut child = node.first_child;
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
                let mut child = node.first_child;
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

        let (parent_kind, first_child) = {
            let nodes = &self.index.nodes;
            let parent = nodes[sel as usize].parent;
            if parent == u32::MAX { return; }
            let pn = &nodes[parent as usize];
            (pn.kind, pn.first_child)
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
            if let Some(pos) = self.visible.iter().position(|&n| n == target) {
                self.scroll_to_row = Some(pos);
            }
        }
    }

    /// Replace search results (called from UI after background search finishes).
    pub fn set_search_results(&mut self, results: Vec<u32>) {
        self.search_result_set = results.iter().copied().collect();
        self.search_results = results;
        self.search_cursor = 0;
        if let Some(&first) = self.search_results.first() {
            self.selected = Some(first);
            self.ensure_visible(first);
        }
    }

}

/// Iterative DFS — avoids stack overflow on deeply nested files.
pub fn rebuild_visible(
    root: u32,
    nodes: &[Node],
    expanded: &HashSet<u32>,
) -> Vec<u32> {
    let mut visible: Vec<u32> = Vec::with_capacity(nodes.len().min(4096));
    let mut stack: Vec<u32> = vec![root];

    while let Some(node_idx) = stack.pop() {
        visible.push(node_idx);
        let node = &nodes[node_idx as usize];
        if expanded.contains(&node_idx) && node.child_count > 0 {
            // Collect children in order, push in reverse so first child is popped first
            let mut children: Vec<u32> = Vec::new();
            let mut child = node.first_child;
            while child != u32::MAX {
                children.push(child);
                child = nodes[child as usize].next_sibling;
            }
            for &c in children.iter().rev() {
                stack.push(c);
            }
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
        first_child: u32,
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
            first_child,
            next_sibling,
            child_count,
            parent,
            array_index: u32::MAX,
        }
    }

    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

    fn make_index(json: &str) -> Arc<JsonIndex> {
        let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let path = std::env::temp_dir().join(format!("qjv_tree_{n}.json"));
        std::fs::write(&path, json.as_bytes()).unwrap();
        let file = std::fs::File::open(&path).unwrap();
        let mmap = unsafe { memmap2::Mmap::map(&file).unwrap() };
        let _ = std::fs::remove_file(&path);
        let mut key_arena = Vec::new();
        let (nodes, root, is_ndjson) =
            crate::parser::parse_bytes(&mmap[..], &mut key_arena, &mut |_| {}).unwrap();
        Arc::new(JsonIndex { _file: file, mmap, nodes, key_arena, root, is_ndjson })
    }

    // ── rebuild_visible ──────────────────────────────────────────────────────

    #[test]
    fn root_only_when_nothing_expanded() {
        let ns = vec![
            make_node(NodeKind::Object, 1, u32::MAX, 2, u32::MAX), // 0 root
            make_node(NodeKind::String, u32::MAX, 2, 0, 0),        // 1 a
            make_node(NodeKind::String, u32::MAX, u32::MAX, 0, 0), // 2 b
        ];
        let visible = rebuild_visible(0, &ns, &HashSet::new());
        assert_eq!(visible, vec![0]);
    }

    #[test]
    fn expanded_root_shows_children() {
        let ns = vec![
            make_node(NodeKind::Object, 1, u32::MAX, 2, u32::MAX),
            make_node(NodeKind::String, u32::MAX, 2, 0, 0),
            make_node(NodeKind::String, u32::MAX, u32::MAX, 0, 0),
        ];
        let mut expanded = HashSet::new();
        expanded.insert(0u32);
        assert_eq!(rebuild_visible(0, &ns, &expanded), vec![0, 1, 2]);
    }

    #[test]
    fn dfs_order_is_preserved() {
        // root(0) -> a(1) -> c(3); root(0) -> b(2)
        let ns = vec![
            make_node(NodeKind::Object, 1, u32::MAX, 2, u32::MAX), // 0 root
            make_node(NodeKind::Object, 3, 2, 1, 0),               // 1 a
            make_node(NodeKind::String, u32::MAX, u32::MAX, 0, 0), // 2 b
            make_node(NodeKind::String, u32::MAX, u32::MAX, 0, 1), // 3 c (child of a)
        ];
        let mut expanded = HashSet::new();
        expanded.insert(0u32);
        expanded.insert(1u32);
        assert_eq!(rebuild_visible(0, &ns, &expanded), vec![0, 1, 3, 2]);
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
        assert_eq!(state.visible.len(), 1);
    }

    #[test]
    fn ensure_visible_expands_ancestors() {
        let idx = make_index(r#"{"a": 1}"#);
        let mut state = TreeState::new(idx);
        // Collapse root so child is hidden
        state.expanded.clear();
        state.refresh_visible();
        let child = state.index.nodes[state.index.root as usize].first_child;
        assert!(!state.visible.contains(&child));
        state.ensure_visible(child);
        assert!(state.visible.contains(&child));
    }

    #[test]
    fn toggle_expands_then_collapses() {
        let idx = make_index(r#"{"a": 1}"#);
        let mut state = TreeState::new(idx);
        let root = state.index.root;
        // Root starts expanded (TreeState::new inserts root into expanded)
        assert_eq!(state.visible.len(), 2); // root + child
        state.toggle(root);
        assert_eq!(state.visible.len(), 1); // collapsed
        state.toggle(root);
        assert_eq!(state.visible.len(), 2); // expanded again
    }
}
