use std::collections::HashSet;
use std::sync::Arc;

use crate::index::{JsonIndex, Node, NodeKind};

#[allow(dead_code)]
pub struct TreeState {
    pub index:             Arc<JsonIndex>,
    pub expanded:          HashSet<u32>,
    pub visible:           Vec<u32>,
    pub selected:          Option<u32>,
    pub search_query:      String,
    pub search_use_regex:  bool,
    pub search_results:    Vec<u32>,
    pub search_cursor:     usize,
    pub search_result_set: HashSet<u32>,
    pub scroll_to_row:     Option<usize>,
}

impl TreeState {
    pub fn new(index: Arc<JsonIndex>) -> Self {
        let root = index.root;
        let expanded = HashSet::new();
        let visible = rebuild_visible(root, &index.nodes, &expanded);
        Self {
            index,
            expanded,
            visible,
            selected: Some(root),
            search_query: String::new(),
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

    fn refresh_visible(&mut self) {
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
