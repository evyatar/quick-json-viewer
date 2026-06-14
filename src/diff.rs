//! Semantic JSON diff engine.
//!
//! Walks two `JsonIndex` trees in parallel and produces a single *merged* tree
//! (`Vec<DiffNode>`, mirroring the flat node model in `index.rs`). Each merged
//! node references an optional left node and an optional right node plus a
//! status. The merged tree drives the side-by-side aligned diff view: matching
//! nodes line up on the same row, additions get an empty left cell and removals
//! an empty right cell.
//!
//! Comparison is configurable via [`DiffOptions`] — ignore case, ignore keys
//! (list + regex), ignore array order, null == missing, type coercion, and
//! whitespace trimming.

use std::collections::HashSet;
use std::sync::Arc;

use crate::index::{JsonIndex, NodeKind};

// ─── options ───────────────────────────────────────────────────────────────

/// Knobs that decide what counts as "equal".
#[derive(Clone, Default)]
pub struct DiffOptions {
    /// Case-insensitive comparison of string values and object keys.
    pub ignore_case: bool,
    /// Treat arrays as multisets: `[1,2,3]` equals `[3,1,2]`.
    pub ignore_array_order: bool,
    /// Object keys to drop entirely before comparing (exact match, honors
    /// `ignore_case`).
    pub ignore_keys: Vec<String>,
    /// Object keys matching this regex are dropped before comparing.
    pub ignore_key_pattern: Option<regex::Regex>,
    /// A present `null` value equals an absent key.
    pub null_equals_missing: bool,
    /// Loose cross-type equality: `"30"` == `30`, `"true"` == `true`.
    pub type_coercion: bool,
    /// Ignore leading/trailing whitespace in string values.
    pub trim_whitespace: bool,
}

// ─── merged-tree data model ──────────────────────────────────────────────────

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DiffStatus {
    /// Present and equal on both sides.
    Unchanged,
    /// Present on both sides but differing (value change, type change, or a
    /// container with differing contents).
    Changed,
    /// Present only on the right side.
    Added,
    /// Present only on the left side.
    Removed,
}

/// One entry in the merged diff tree. `left`/`right` are indices into the
/// respective `JsonIndex.nodes`; `u32::MAX` means "absent on that side".
#[derive(Clone, Debug)]
pub struct DiffNode {
    pub left:         u32,
    pub right:        u32,
    pub status:       DiffStatus,
    pub depth:        u16,
    pub first_child:  u32,
    pub next_sibling: u32,
    pub child_count:  u32,
    pub parent:       u32,
    /// `true` when this node or any descendant differs (equivalently:
    /// `status != Unchanged`).
    pub subtree_changed: bool,
}

impl DiffNode {
    pub fn left_idx(&self) -> Option<u32> {
        (self.left != u32::MAX).then_some(self.left)
    }
    pub fn right_idx(&self) -> Option<u32> {
        (self.right != u32::MAX).then_some(self.right)
    }
}

/// The result of a diff: the merged tree plus the two source documents and
/// summary counts.
pub struct DiffResult {
    pub left:   Arc<JsonIndex>,
    pub right:  Arc<JsonIndex>,
    pub nodes:  Vec<DiffNode>,
    pub root:   u32,
    pub changed: usize,
    pub added:   usize,
    pub removed: usize,
    /// Pre-order list of "difference entry" nodes for next/prev navigation:
    /// each added/removed subtree root and each changed leaf, in tree order.
    pub diff_positions: Vec<u32>,
}

// ─── public entry point ──────────────────────────────────────────────────────

/// Diff two parsed documents under `opts`.
pub fn diff(left: Arc<JsonIndex>, right: Arc<JsonIndex>, opts: &DiffOptions) -> DiffResult {
    // Fingerprint-accelerated multiset matching turns the O(n²)-deep-compare
    // "ignore array order" pass into roughly O(n) hashing + bucket lookups. It
    // is only sound when value-equality is transitive — type coercion breaks
    // transitivity — so build the tables only when ignoring array order under
    // exact-equality settings; otherwise leave them empty and fall back to the
    // original scan.
    let use_fp = opts.ignore_array_order && !opts.type_coercion;
    let (lfp, rfp) = if use_fp {
        (fingerprints(&left, opts), fingerprints(&right, opts))
    } else {
        (Vec::new(), Vec::new())
    };

    let nodes = {
        let mut b = Builder {
            left: &left, right: &right, opts,
            lfp: &lfp, rfp: &rfp, nodes: Vec::new(),
        };
        b.build_pair(left.root, right.root, 0, u32::MAX);
        b.nodes
    };

    let (mut changed, mut added, mut removed) = (0usize, 0usize, 0usize);
    let mut diff_positions: Vec<u32> = Vec::new();
    for (i, n) in nodes.iter().enumerate() {
        let parent_same_status = n.parent != u32::MAX
            && nodes[n.parent as usize].status == n.status;
        match n.status {
            // Only count/navigate the *root* of an added/removed subtree.
            DiffStatus::Added if !parent_same_status   => { added += 1;   diff_positions.push(i as u32); }
            DiffStatus::Removed if !parent_same_status => { removed += 1; diff_positions.push(i as u32); }
            // A changed *leaf* is a real value/type change; changed containers
            // are just aggregations of their descendants.
            DiffStatus::Changed if n.child_count == 0  => { changed += 1; diff_positions.push(i as u32); }
            _ => {}
        }
    }
    diff_positions.sort_unstable();

    DiffResult { left, right, nodes, root: 0, changed, added, removed, diff_positions }
}

// ─── builder ─────────────────────────────────────────────────────────────────

struct Builder<'a> {
    left:  &'a JsonIndex,
    right: &'a JsonIndex,
    opts:  &'a DiffOptions,
    /// Per-node canonical fingerprints for each side, indexed by node id.
    /// Empty unless `ignore_array_order` matching is active (see [`diff`]).
    lfp:   &'a [u64],
    rfp:   &'a [u64],
    nodes: Vec<DiffNode>,
}

/// Read-only bundle threaded through the equality predicates so they can reach
/// the fingerprint tables for fast multiset matching.
struct EqCtx<'a> {
    left:   &'a JsonIndex,
    right:  &'a JsonIndex,
    opts:   &'a DiffOptions,
    lfp:    &'a [u64],
    rfp:    &'a [u64],
    use_fp: bool,
}

impl<'a> Builder<'a> {
    /// Build a node for two paired values present on both sides. Recurses into
    /// matching containers. Returns the new node's index.
    fn build_pair(&mut self, l: u32, r: u32, depth: u16, parent: u32) -> u32 {
        let own = self.nodes.len() as u32;
        self.nodes.push(DiffNode {
            left: l, right: r, status: DiffStatus::Unchanged, depth,
            first_child: u32::MAX, next_sibling: u32::MAX, child_count: 0,
            parent, subtree_changed: false,
        });

        let lkind = self.left.nodes[l as usize].kind;
        let rkind = self.right.nodes[r as usize].kind;

        let status = if lkind == rkind && lkind == NodeKind::Object {
            let (children, changed) = self.match_object(l, r, depth + 1, own);
            self.link_children(own, &children);
            if changed { DiffStatus::Changed } else { DiffStatus::Unchanged }
        } else if lkind == rkind && lkind == NodeKind::Array {
            let (children, changed) = self.match_array(l, r, depth + 1, own);
            self.link_children(own, &children);
            if changed { DiffStatus::Changed } else { DiffStatus::Unchanged }
        } else if scalars_equal(self.left, l, self.right, r, self.opts) {
            DiffStatus::Unchanged
        } else {
            DiffStatus::Changed
        };

        self.nodes[own as usize].status = status;
        self.nodes[own as usize].subtree_changed = status != DiffStatus::Unchanged;
        own
    }

    /// Build a whole subtree that exists only on one side.
    fn build_single(&mut self, idx: u32, is_left: bool, depth: u16, parent: u32) -> u32 {
        let own = self.nodes.len() as u32;
        let (left, right, status) = if is_left {
            (idx, u32::MAX, DiffStatus::Removed)
        } else {
            (u32::MAX, idx, DiffStatus::Added)
        };
        self.nodes.push(DiffNode {
            left, right, status, depth,
            first_child: u32::MAX, next_sibling: u32::MAX, child_count: 0,
            parent, subtree_changed: true,
        });

        let src = if is_left { self.left } else { self.right };
        let node = &src.nodes[idx as usize];
        if matches!(node.kind, NodeKind::Object | NodeKind::Array) {
            let children = collect_children(src, idx);
            let mut built = Vec::with_capacity(children.len());
            for child in children {
                built.push(self.build_single(child, is_left, depth + 1, own));
            }
            self.link_children(own, &built);
        }
        own
    }

    /// Match object children by key (honoring ignore-key filters and
    /// `ignore_case`). Returns the merged child node list and whether anything
    /// differed.
    fn match_object(&mut self, l: u32, r: u32, depth: u16, parent: u32) -> (Vec<u32>, bool) {
        let (left, right, opts) = (self.left, self.right, self.opts);
        let lc: Vec<u32> = collect_children(left, l)
            .into_iter().filter(|&c| !key_ignored(left, c, opts)).collect();
        let rc: Vec<u32> = collect_children(right, r)
            .into_iter().filter(|&c| !key_ignored(right, c, opts)).collect();

        let mut right_used = vec![false; rc.len()];
        let mut out: Vec<u32> = Vec::new();
        let mut any_changed = false;

        for &lchild in &lc {
            let lkey = left.key_of(&left.nodes[lchild as usize]);
            let matched = rc.iter().enumerate().find(|&(ri, &rchild)| {
                !right_used[ri]
                    && keys_equal(lkey, right.key_of(&right.nodes[rchild as usize]), opts)
            }).map(|(ri, &rchild)| (ri, rchild));

            match matched {
                Some((ri, rchild)) => {
                    right_used[ri] = true;
                    let n = self.build_pair(lchild, rchild, depth, parent);
                    if self.nodes[n as usize].status != DiffStatus::Unchanged { any_changed = true; }
                    out.push(n);
                }
                None => {
                    if opts.null_equals_missing && is_null(left, lchild) { continue; }
                    out.push(self.build_single(lchild, true, depth, parent));
                    any_changed = true;
                }
            }
        }
        for (ri, &rchild) in rc.iter().enumerate() {
            if right_used[ri] { continue; }
            if opts.null_equals_missing && is_null(right, rchild) { continue; }
            out.push(self.build_single(rchild, false, depth, parent));
            any_changed = true;
        }
        (out, any_changed)
    }

    fn eq_ctx(&self) -> EqCtx<'_> {
        EqCtx {
            left: self.left, right: self.right, opts: self.opts,
            lfp: self.lfp, rfp: self.rfp, use_fp: !self.lfp.is_empty(),
        }
    }

    /// Match array children either positionally or as a multiset (when
    /// `ignore_array_order`).
    fn match_array(&mut self, l: u32, r: u32, depth: u16, parent: u32) -> (Vec<u32>, bool) {
        let (left, right, opts) = (self.left, self.right, self.opts);
        let lc = collect_children(left, l);
        let rc = collect_children(right, r);
        let mut out: Vec<u32> = Vec::new();
        let mut any_changed = false;

        if opts.ignore_array_order {
            // Decide all pairings up front (read-only) so the borrow checker is
            // happy, then build the merged nodes from the result.
            let (match_of, right_used) = multiset_pairing(&self.eq_ctx(), &lc, &rc);
            for (li, &lchild) in lc.iter().enumerate() {
                match match_of[li] {
                    // Deep-equal pair → Unchanged (build_pair re-confirms).
                    Some(ri) => out.push(self.build_pair(lchild, rc[ri], depth, parent)),
                    None => {
                        out.push(self.build_single(lchild, true, depth, parent));
                        any_changed = true;
                    }
                }
            }
            for (ri, &rchild) in rc.iter().enumerate() {
                if right_used[ri] { continue; }
                out.push(self.build_single(rchild, false, depth, parent));
                any_changed = true;
            }
        } else {
            let common = lc.len().min(rc.len());
            for i in 0..common {
                let n = self.build_pair(lc[i], rc[i], depth, parent);
                if self.nodes[n as usize].status != DiffStatus::Unchanged { any_changed = true; }
                out.push(n);
            }
            for &lchild in &lc[common..] {
                out.push(self.build_single(lchild, true, depth, parent));
                any_changed = true;
            }
            for &rchild in &rc[common..] {
                out.push(self.build_single(rchild, false, depth, parent));
                any_changed = true;
            }
        }
        (out, any_changed)
    }

    fn link_children(&mut self, parent: u32, children: &[u32]) {
        let mut prev = u32::MAX;
        for &c in children {
            if prev == u32::MAX {
                self.nodes[parent as usize].first_child = c;
            } else {
                self.nodes[prev as usize].next_sibling = c;
            }
            prev = c;
        }
        self.nodes[parent as usize].child_count = children.len() as u32;
    }
}

// ─── equality predicates ─────────────────────────────────────────────────────

fn collect_children(idx: &JsonIndex, n: u32) -> Vec<u32> {
    let mut v = Vec::new();
    let mut c = idx.nodes[n as usize].first_child;
    while c != u32::MAX {
        v.push(c);
        c = idx.nodes[c as usize].next_sibling;
    }
    v
}

fn is_null(idx: &JsonIndex, n: u32) -> bool {
    idx.nodes[n as usize].kind == NodeKind::Null
}

fn key_ignored(idx: &JsonIndex, n: u32, opts: &DiffOptions) -> bool {
    let node = &idx.nodes[n as usize];
    if node.key_len == 0 {
        return false;
    }
    let key = idx.key_of(node);
    if opts.ignore_keys.iter().any(|k| keys_equal(k, key, opts)) {
        return true;
    }
    matches!(&opts.ignore_key_pattern, Some(re) if re.is_match(key))
}

fn keys_equal(a: &str, b: &str, opts: &DiffOptions) -> bool {
    if opts.ignore_case {
        a.to_lowercase() == b.to_lowercase()
    } else {
        a == b
    }
}

/// Returns `(kind, scalar text)` for a node. For strings the text is the inner
/// content (quotes stripped); escapes are not decoded, matching the viewer.
fn scalar_text<'a>(idx: &'a JsonIndex, n: u32) -> (NodeKind, &'a str) {
    let node = &idx.nodes[n as usize];
    let raw = idx.value_bytes(node);
    let bytes = match node.kind {
        NodeKind::String if raw.len() >= 2 => &raw[1..raw.len() - 1],
        _ => raw,
    };
    (node.kind, std::str::from_utf8(bytes).unwrap_or(""))
}

/// Pair left children to right children as multisets (ignore order). Returns,
/// for each left index, the matched position in `rc` (or `None`), plus the
/// `right_used` mask. When fingerprints are available it buckets the right side
/// by fingerprint so each left element matches in ~O(1); otherwise it falls
/// back to the original O(n²) scan. A deep [`values_equal`] check always
/// confirms the pairing, so a fingerprint collision can never produce a wrong
/// match — it only costs a little extra scanning.
fn multiset_pairing(ctx: &EqCtx, lc: &[u32], rc: &[u32]) -> (Vec<Option<usize>>, Vec<bool>) {
    let mut match_of = vec![None; lc.len()];
    let mut right_used = vec![false; rc.len()];

    if ctx.use_fp {
        use std::collections::{HashMap, VecDeque};
        let mut buckets: HashMap<u64, VecDeque<usize>> = HashMap::new();
        for (ri, &rchild) in rc.iter().enumerate() {
            buckets.entry(ctx.rfp[rchild as usize]).or_default().push_back(ri);
        }
        for (li, &lchild) in lc.iter().enumerate() {
            let Some(bucket) = buckets.get_mut(&ctx.lfp[lchild as usize]) else { continue };
            // Pop candidates until one deep-equals; stash any fingerprint
            // collisions (almost never hit) so they remain available to later
            // left elements.
            let mut stash: Vec<usize> = Vec::new();
            while let Some(ri) = bucket.pop_front() {
                if values_equal(ctx, lchild, rc[ri]) {
                    right_used[ri] = true;
                    match_of[li] = Some(ri);
                    break;
                }
                stash.push(ri);
            }
            for ri in stash { bucket.push_front(ri); }
        }
    } else {
        for (li, &lchild) in lc.iter().enumerate() {
            for (ri, &rchild) in rc.iter().enumerate() {
                if !right_used[ri] && values_equal(ctx, lchild, rchild) {
                    right_used[ri] = true;
                    match_of[li] = Some(ri);
                    break;
                }
            }
        }
    }
    (match_of, right_used)
}

/// Full recursive value equality under `ctx.opts` — used for multiset array
/// matching and fingerprint-collision verification.
fn values_equal(ctx: &EqCtx, l: u32, r: u32) -> bool {
    let lk = ctx.left.nodes[l as usize].kind;
    let rk = ctx.right.nodes[r as usize].kind;
    match (lk, rk) {
        (NodeKind::Object, NodeKind::Object) => objects_equal(ctx, l, r),
        (NodeKind::Array, NodeKind::Array)   => arrays_equal(ctx, l, r),
        _ => scalars_equal(ctx.left, l, ctx.right, r, ctx.opts),
    }
}

fn objects_equal(ctx: &EqCtx, l: u32, r: u32) -> bool {
    let (left, right, opts) = (ctx.left, ctx.right, ctx.opts);
    let lc: Vec<u32> = collect_children(left, l)
        .into_iter().filter(|&c| !key_ignored(left, c, opts)).collect();
    let rc: Vec<u32> = collect_children(right, r)
        .into_iter().filter(|&c| !key_ignored(right, c, opts)).collect();

    let mut right_used = vec![false; rc.len()];
    for &lchild in &lc {
        let lkey = left.key_of(&left.nodes[lchild as usize]);
        let matched = rc.iter().enumerate().find(|&(ri, &rchild)| {
            !right_used[ri] && keys_equal(lkey, right.key_of(&right.nodes[rchild as usize]), opts)
        }).map(|(ri, &rchild)| (ri, rchild));
        match matched {
            Some((ri, rchild)) => {
                right_used[ri] = true;
                if !values_equal(ctx, lchild, rchild) {
                    return false;
                }
            }
            None => {
                if opts.null_equals_missing && is_null(left, lchild) { continue; }
                return false;
            }
        }
    }
    for (ri, &rchild) in rc.iter().enumerate() {
        if right_used[ri] { continue; }
        if opts.null_equals_missing && is_null(right, rchild) { continue; }
        return false;
    }
    true
}

fn arrays_equal(ctx: &EqCtx, l: u32, r: u32) -> bool {
    let lc = collect_children(ctx.left, l);
    let rc = collect_children(ctx.right, r);
    if lc.len() != rc.len() {
        return false;
    }
    if ctx.opts.ignore_array_order {
        // Equal-length arrays are multiset-equal iff every left element finds a
        // distinct deep-equal right element (matching is injective).
        let (match_of, _) = multiset_pairing(ctx, &lc, &rc);
        match_of.iter().all(Option::is_some)
    } else {
        lc.iter().zip(rc.iter())
            .all(|(&a, &b)| values_equal(ctx, a, b))
    }
}

/// Leaf-value equality (strings, numbers, bools, nulls), with optional type
/// coercion across kinds. Containers are never "scalar equal".
fn scalars_equal(left: &JsonIndex, l: u32, right: &JsonIndex, r: u32, opts: &DiffOptions) -> bool {
    let (lk, ltext) = scalar_text(left, l);
    let (rk, rtext) = scalar_text(right, r);

    if matches!(lk, NodeKind::Object | NodeKind::Array)
        || matches!(rk, NodeKind::Object | NodeKind::Array)
    {
        return false;
    }

    if lk == rk {
        return match lk {
            NodeKind::Number       => numbers_equal(ltext, rtext),
            NodeKind::String       => strings_equal(ltext, rtext, opts),
            NodeKind::Bool | NodeKind::Null => ltext == rtext,
            _ => false,
        };
    }

    // Differing scalar kinds.
    if opts.type_coercion {
        if let (Some(x), Some(y)) = (coerce_num(lk, ltext), coerce_num(rk, rtext)) {
            return x == y;
        }
        // Fall back to textual comparison (e.g. "true" string vs true bool).
        return strings_equal(ltext, rtext, opts);
    }
    false
}

fn numbers_equal(a: &str, b: &str) -> bool {
    if a == b {
        return true;
    }
    match (a.parse::<f64>(), b.parse::<f64>()) {
        // NB: comparison is via f64, so two distinct integers beyond f64's
        // 53-bit mantissa could compare equal — acceptable for a viewer.
        (Ok(x), Ok(y)) => x == y,
        _ => false,
    }
}

fn coerce_num(kind: NodeKind, text: &str) -> Option<f64> {
    match kind {
        NodeKind::Number => text.parse::<f64>().ok(),
        NodeKind::String => text.trim().parse::<f64>().ok(),
        _ => None,
    }
}

fn strings_equal(a: &str, b: &str, opts: &DiffOptions) -> bool {
    let (a, b) = if opts.trim_whitespace { (a.trim(), b.trim()) } else { (a, b) };
    if opts.ignore_case {
        a.to_lowercase() == b.to_lowercase()
    } else {
        a == b
    }
}

// ─── canonical fingerprints ──────────────────────────────────────────────────
//
// A fingerprint is a 64-bit hash that is equal for any two nodes that compare
// equal under `opts`, *provided* equality is transitive — i.e. type coercion
// is off (enforced by the `use_fp` gate in
// `diff`). It lets the "ignore array order" pass bucket candidates by hash
// instead of deep-comparing every pair. Collisions only cost extra deep-equal
// checks (which always run), never correctness.

// Per-kind tags keep e.g. the number 1 distinct from the string "1".
const TAG_OBJECT: u64 = 0x0b1e_5500_0000_0001;
const TAG_ARRAY:  u64 = 0x0b1e_5500_0000_0002;
const TAG_NUMBER: u64 = 0x0b1e_5500_0000_0003;
const TAG_STRING: u64 = 0x0b1e_5500_0000_0004;
const TAG_BOOL:   u64 = 0x0b1e_5500_0000_0005;
const TAG_NULL:   u64 = 0x0b1e_5500_0000_0006;

fn mix64(mut x: u64) -> u64 {
    x = x.wrapping_add(0x9e37_79b9_7f4a_7c15);
    x = (x ^ (x >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    x ^ (x >> 31)
}

fn fnv1a(mut h: u64, bytes: &[u8]) -> u64 {
    for &b in bytes {
        h ^= b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// Compute the canonical fingerprint of every node in `idx`, indexed by node
/// id. Computed once per side per diff; memoized so each node is hashed once.
fn fingerprints(idx: &JsonIndex, opts: &DiffOptions) -> Vec<u64> {
    let mut memo = vec![0u64; idx.nodes.len()];
    let mut done = vec![false; idx.nodes.len()];
    for n in 0..idx.nodes.len() as u32 {
        if !done[n as usize] {
            fp_node(idx, n, opts, &mut memo, &mut done);
        }
    }
    memo
}

fn fp_node(idx: &JsonIndex, n: u32, opts: &DiffOptions, memo: &mut [u64], done: &mut [bool]) -> u64 {
    if done[n as usize] {
        return memo[n as usize];
    }
    let kind = idx.nodes[n as usize].kind;
    let h = match kind {
        NodeKind::Object => {
            // Order-independent over entries; ignored keys and (when null ==
            // missing) null-valued keys are dropped so they don't affect the
            // hash — matching `objects_equal`.
            let mut acc: u64 = 0;
            let mut c = idx.nodes[n as usize].first_child;
            while c != u32::MAX {
                let next = idx.nodes[c as usize].next_sibling;
                let cnull = idx.nodes[c as usize].kind == NodeKind::Null;
                if !(key_ignored(idx, c, opts) || (opts.null_equals_missing && cnull)) {
                    let key = idx.key_of(&idx.nodes[c as usize]);
                    let kh = if opts.ignore_case {
                        fnv1a(TAG_STRING, key.to_lowercase().as_bytes())
                    } else {
                        fnv1a(TAG_STRING, key.as_bytes())
                    };
                    let vh = fp_node(idx, c, opts, memo, done);
                    acc = acc.wrapping_add(mix64(kh ^ mix64(vh)));
                }
                c = next;
            }
            mix64(TAG_OBJECT ^ acc)
        }
        NodeKind::Array => {
            let mut c = idx.nodes[n as usize].first_child;
            if opts.ignore_array_order {
                let mut acc: u64 = 0;
                let mut count: u64 = 0;
                while c != u32::MAX {
                    acc = acc.wrapping_add(mix64(fp_node(idx, c, opts, memo, done)));
                    count += 1;
                    c = idx.nodes[c as usize].next_sibling;
                }
                mix64(TAG_ARRAY ^ acc ^ mix64(count))
            } else {
                let mut h = TAG_ARRAY;
                while c != u32::MAX {
                    h = mix64(h ^ fp_node(idx, c, opts, memo, done));
                    c = idx.nodes[c as usize].next_sibling;
                }
                h
            }
        }
        _ => fp_scalar(idx, n, opts),
    };
    memo[n as usize] = h;
    done[n as usize] = true;
    h
}

fn fp_scalar(idx: &JsonIndex, n: u32, opts: &DiffOptions) -> u64 {
    let (kind, text) = scalar_text(idx, n);
    match kind {
        // `1` and `1.0` are equal, so hash by the parsed f64 (canonicalizing
        // ±0). Numbers that don't parse fall back to their text.
        NodeKind::Number => match text.parse::<f64>() {
            Ok(x) => {
                let bits = if x == 0.0 { 0 } else { x.to_bits() };
                mix64(TAG_NUMBER ^ bits)
            }
            Err(_) => fnv1a(TAG_NUMBER, text.as_bytes()),
        },
        NodeKind::String => {
            let t = if opts.trim_whitespace { text.trim() } else { text };
            if opts.ignore_case {
                fnv1a(TAG_STRING, t.to_lowercase().as_bytes())
            } else {
                fnv1a(TAG_STRING, t.as_bytes())
            }
        }
        NodeKind::Bool => fnv1a(TAG_BOOL, text.as_bytes()),
        _ => TAG_NULL,
    }
}

// ─── merged-tree view state ──────────────────────────────────────────────────

/// Iterative DFS over the merged tree producing the visible row list. When
/// `only_diffs` is set, entire Unchanged subtrees are skipped.
pub fn rebuild_visible_diff(
    root: u32,
    nodes: &[DiffNode],
    expanded: &HashSet<u32>,
    only_diffs: bool,
) -> Vec<u32> {
    let mut visible: Vec<u32> = Vec::new();
    let mut stack: Vec<u32> = vec![root];
    while let Some(idx) = stack.pop() {
        let node = &nodes[idx as usize];
        if only_diffs && idx != root && node.status == DiffStatus::Unchanged {
            continue;
        }
        visible.push(idx);
        if expanded.contains(&idx) && node.child_count > 0 {
            let mut children = Vec::new();
            let mut c = node.first_child;
            while c != u32::MAX {
                children.push(c);
                c = nodes[c as usize].next_sibling;
            }
            for &c in children.iter().rev() {
                stack.push(c);
            }
        }
    }
    visible
}

/// Expansion / selection / navigation over a [`DiffResult`]. Mirrors
/// `tree::TreeState` but methods take the `DiffResult` as an argument so the
/// state isn't self-referential (it can live next to the result in app state).
pub struct DiffTreeState {
    pub expanded:       HashSet<u32>,
    pub visible:        Vec<u32>,
    pub selected:       Option<u32>,
    pub scroll_to_row:  Option<usize>,
    pub diff_cursor:    usize,
    pub only_diffs:     bool,
}

impl DiffTreeState {
    pub fn new(result: &DiffResult) -> Self {
        // Expand every container that contains a difference so all diffs are
        // visible by default; leave clean subtrees collapsed.
        let mut expanded = HashSet::new();
        expanded.insert(result.root);
        for (i, n) in result.nodes.iter().enumerate() {
            if n.child_count > 0 && n.status != DiffStatus::Unchanged {
                expanded.insert(i as u32);
            }
        }
        let mut s = Self {
            expanded,
            visible: Vec::new(),
            selected: Some(result.root),
            scroll_to_row: None,
            diff_cursor: 0,
            only_diffs: false,
        };
        s.refresh_visible(result);
        if let Some(&first) = result.diff_positions.first() {
            s.selected = Some(first);
        }
        s
    }

    pub fn refresh_visible(&mut self, result: &DiffResult) {
        self.expanded.insert(result.root);
        self.visible = rebuild_visible_diff(result.root, &result.nodes, &self.expanded, self.only_diffs);
    }

    pub fn toggle(&mut self, idx: u32, result: &DiffResult) {
        if !self.expanded.remove(&idx) {
            self.expanded.insert(idx);
        }
        self.refresh_visible(result);
    }

    pub fn expand_all(&mut self, result: &DiffResult) {
        for (i, n) in result.nodes.iter().enumerate() {
            if n.child_count > 0 {
                self.expanded.insert(i as u32);
            }
        }
        self.refresh_visible(result);
    }

    pub fn collapse_all(&mut self, result: &DiffResult) {
        self.expanded.clear();
        self.refresh_visible(result);
    }

    pub fn set_only_diffs(&mut self, only: bool, result: &DiffResult) {
        self.only_diffs = only;
        self.refresh_visible(result);
        if let Some(sel) = self.selected {
            if !self.visible.contains(&sel) {
                self.selected = self.visible.first().copied();
            }
        }
    }

    fn pos(&self) -> Option<usize> {
        self.selected.and_then(|s| self.visible.iter().position(|&n| n == s))
    }

    pub fn select_up(&mut self) {
        if let Some(p) = self.pos() {
            if p > 0 { self.selected = Some(self.visible[p - 1]); self.scroll_to_row = Some(p - 1); }
        }
    }
    pub fn select_down(&mut self) {
        if let Some(p) = self.pos() {
            if p + 1 < self.visible.len() { self.selected = Some(self.visible[p + 1]); self.scroll_to_row = Some(p + 1); }
        }
    }
    pub fn select_page_up(&mut self, page: usize) {
        if let Some(p) = self.pos() {
            let np = p.saturating_sub(page);
            self.selected = Some(self.visible[np]); self.scroll_to_row = Some(np);
        }
    }
    pub fn select_page_down(&mut self, page: usize) {
        if let Some(p) = self.pos() {
            let np = (p + page).min(self.visible.len().saturating_sub(1));
            self.selected = Some(self.visible[np]); self.scroll_to_row = Some(np);
        }
    }
    pub fn select_home(&mut self) {
        if !self.visible.is_empty() { self.selected = Some(self.visible[0]); self.scroll_to_row = Some(0); }
    }
    pub fn select_end(&mut self) {
        if let Some(last) = self.visible.last().copied() {
            self.selected = Some(last); self.scroll_to_row = Some(self.visible.len() - 1);
        }
    }

    /// Collapse the current container, else jump to its parent.
    pub fn select_left(&mut self, result: &DiffResult) {
        let Some(sel) = self.selected else { return };
        let node = &result.nodes[sel as usize];
        if node.child_count > 0 && self.expanded.contains(&sel) {
            self.toggle(sel, result);
        } else if node.parent != u32::MAX {
            self.selected = Some(node.parent);
        }
    }

    /// Expand the current collapsed container.
    pub fn select_right(&mut self, result: &DiffResult) {
        let Some(sel) = self.selected else { return };
        let node = &result.nodes[sel as usize];
        if node.child_count > 0 && !self.expanded.contains(&sel) {
            self.toggle(sel, result);
        }
    }

    /// Expand all ancestors of `idx` so it becomes visible, then scroll to it.
    pub fn ensure_visible(&mut self, idx: u32, result: &DiffResult) {
        let mut cur = idx;
        loop {
            let parent = result.nodes[cur as usize].parent;
            if parent == u32::MAX { break; }
            self.expanded.insert(parent);
            cur = parent;
        }
        self.refresh_visible(result);
        self.scroll_to_row = self.visible.iter().position(|&n| n == idx);
    }

    pub fn next_diff(&mut self, result: &DiffResult) {
        if result.diff_positions.is_empty() { return; }
        self.diff_cursor = (self.diff_cursor + 1) % result.diff_positions.len();
        let target = result.diff_positions[self.diff_cursor];
        self.selected = Some(target);
        self.ensure_visible(target, result);
    }
    pub fn prev_diff(&mut self, result: &DiffResult) {
        if result.diff_positions.is_empty() { return; }
        self.diff_cursor = if self.diff_cursor == 0 {
            result.diff_positions.len() - 1
        } else {
            self.diff_cursor - 1
        };
        let target = result.diff_positions[self.diff_cursor];
        self.selected = Some(target);
        self.ensure_visible(target, result);
    }
}

// ─── tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::JsonData;

    fn idx(json: &str) -> Arc<JsonIndex> {
        let data = json.as_bytes().to_vec();
        let mut key_arena = Vec::new();
        let (nodes, root, is_ndjson) =
            crate::parser::parse_bytes(&data, &mut key_arena, &mut |_| {}).unwrap();
        Arc::new(JsonIndex { data: JsonData::Memory(data), nodes, key_arena, root, is_ndjson })
    }

    fn run(l: &str, r: &str, opts: DiffOptions) -> DiffResult {
        diff(idx(l), idx(r), &opts)
    }

    fn root_status(res: &DiffResult) -> DiffStatus {
        res.nodes[res.root as usize].status
    }

    #[test]
    fn identical_documents_are_unchanged() {
        let res = run(r#"{"a":1,"b":[1,2,3]}"#, r#"{"a":1,"b":[1,2,3]}"#, DiffOptions::default());
        assert_eq!(root_status(&res), DiffStatus::Unchanged);
        assert_eq!((res.changed, res.added, res.removed), (0, 0, 0));
    }

    #[test]
    fn changed_scalar_value() {
        let res = run(r#"{"a":1}"#, r#"{"a":2}"#, DiffOptions::default());
        assert_eq!(root_status(&res), DiffStatus::Changed);
        assert_eq!((res.changed, res.added, res.removed), (1, 0, 0));
    }

    #[test]
    fn added_and_removed_keys() {
        let res = run(r#"{"a":1,"b":2}"#, r#"{"a":1,"c":3}"#, DiffOptions::default());
        assert_eq!((res.changed, res.added, res.removed), (0, 1, 1));
    }

    #[test]
    fn key_order_does_not_matter() {
        let res = run(r#"{"a":1,"b":2}"#, r#"{"b":2,"a":1}"#, DiffOptions::default());
        assert_eq!(root_status(&res), DiffStatus::Unchanged);
    }

    #[test]
    fn type_change_is_changed() {
        let res = run(r#"{"a":1}"#, r#"{"a":"1"}"#, DiffOptions::default());
        assert_eq!(root_status(&res), DiffStatus::Changed);
        // With type coercion, "1" == 1.
        let res = run(r#"{"a":1}"#, r#"{"a":"1"}"#, DiffOptions { type_coercion: true, ..Default::default() });
        assert_eq!(root_status(&res), DiffStatus::Unchanged);
    }

    #[test]
    fn ignore_case_on_values_and_keys() {
        let base = DiffOptions { ignore_case: true, ..Default::default() };
        assert_eq!(root_status(&run(r#"{"a":"HELLO"}"#, r#"{"a":"hello"}"#, base.clone())), DiffStatus::Unchanged);
        assert_eq!(root_status(&run(r#"{"Name":1}"#, r#"{"name":1}"#, base)), DiffStatus::Unchanged);
        // Without the flag they differ.
        assert_eq!(root_status(&run(r#"{"a":"HELLO"}"#, r#"{"a":"hello"}"#, DiffOptions::default())), DiffStatus::Changed);
    }

    #[test]
    fn ignore_keys_list() {
        let opts = DiffOptions { ignore_keys: vec!["updatedAt".into()], ..Default::default() };
        let res = run(r#"{"id":1,"updatedAt":"x"}"#, r#"{"id":1,"updatedAt":"y"}"#, opts);
        assert_eq!(root_status(&res), DiffStatus::Unchanged);
    }

    #[test]
    fn ignore_keys_pattern() {
        let opts = DiffOptions {
            ignore_key_pattern: Some(regex::Regex::new("^_").unwrap()),
            ..Default::default()
        };
        let res = run(r#"{"id":1,"_meta":1}"#, r#"{"id":1,"_meta":999}"#, opts);
        assert_eq!(root_status(&res), DiffStatus::Unchanged);
    }

    #[test]
    fn ignore_array_order() {
        let opts = DiffOptions { ignore_array_order: true, ..Default::default() };
        assert_eq!(root_status(&run("[1,2,3]", "[3,1,2]", opts.clone())), DiffStatus::Unchanged);
        // Different multiset still differs.
        assert_eq!(root_status(&run("[1,2,3]", "[3,1,4]", opts)), DiffStatus::Changed);
        // Positional (default) sees a difference.
        assert_eq!(root_status(&run("[1,2,3]", "[3,1,2]", DiffOptions::default())), DiffStatus::Changed);
    }

    #[test]
    fn ignore_array_order_with_objects() {
        let opts = DiffOptions { ignore_array_order: true, ..Default::default() };
        let res = run(r#"[{"id":1},{"id":2}]"#, r#"[{"id":2},{"id":1}]"#, opts);
        assert_eq!(root_status(&res), DiffStatus::Unchanged);
    }

    #[test]
    fn numbers_equal_across_int_float() {
        // 1 vs 1.0 compare equal.
        assert_eq!(root_status(&run(r#"{"a":1}"#, r#"{"a":1.0}"#, DiffOptions::default())), DiffStatus::Unchanged);
    }

    #[test]
    fn null_equals_missing() {
        let opts = DiffOptions { null_equals_missing: true, ..Default::default() };
        assert_eq!(root_status(&run(r#"{"a":1,"b":null}"#, r#"{"a":1}"#, opts.clone())), DiffStatus::Unchanged);
        assert_eq!(root_status(&run(r#"{"a":1}"#, r#"{"a":1,"b":null}"#, opts)), DiffStatus::Unchanged);
        // Without the flag, the null key is a removal.
        let res = run(r#"{"a":1,"b":null}"#, r#"{"a":1}"#, DiffOptions::default());
        assert_eq!(res.removed, 1);
    }

    #[test]
    fn trim_whitespace() {
        let opts = DiffOptions { trim_whitespace: true, ..Default::default() };
        assert_eq!(root_status(&run(r#"{"a":"  hi  "}"#, r#"{"a":"hi"}"#, opts)), DiffStatus::Unchanged);
    }

    #[test]
    fn nested_change_propagates_to_root() {
        let res = run(r#"{"a":{"b":{"c":1}}}"#, r#"{"a":{"b":{"c":2}}}"#, DiffOptions::default());
        assert_eq!(root_status(&res), DiffStatus::Changed);
        // One changed leaf; intermediate containers are Changed but not counted.
        assert_eq!(res.changed, 1);
    }

    #[test]
    fn diff_positions_track_each_difference() {
        let res = run(r#"{"a":1,"b":2,"c":3}"#, r#"{"a":9,"b":2,"d":3}"#, DiffOptions::default());
        // a changed, c removed, d added.
        assert_eq!(res.diff_positions.len(), 3);
        assert_eq!((res.changed, res.added, res.removed), (1, 1, 1));
    }

    #[test]
    fn added_subtree_counts_once() {
        // Right adds an object subtree under key "b"; counts as a single add.
        let res = run(r#"{"a":1}"#, r#"{"a":1,"b":{"x":1,"y":2}}"#, DiffOptions::default());
        assert_eq!((res.added, res.changed, res.removed), (1, 0, 0));
    }

    #[test]
    fn merged_tree_links_are_consistent() {
        let res = run(r#"{"a":1}"#, r#"{"a":2}"#, DiffOptions::default());
        let root = &res.nodes[res.root as usize];
        assert_eq!(root.child_count, 1);
        let child = root.first_child;
        assert_ne!(child, u32::MAX);
        assert_eq!(res.nodes[child as usize].parent, res.root);
        assert_eq!(res.nodes[child as usize].status, DiffStatus::Changed);
    }

    #[test]
    fn only_diffs_filter_hides_unchanged() {
        let res = run(r#"{"a":1,"b":2,"c":3}"#, r#"{"a":1,"b":9,"c":3}"#, DiffOptions::default());
        // Full tree: root + 3 children.
        let full = rebuild_visible_diff(res.root, &res.nodes, &{
            let mut e = HashSet::new(); e.insert(res.root); e
        }, false);
        assert_eq!(full.len(), 4);
        // Only diffs: root + the single changed child "b".
        let only = rebuild_visible_diff(res.root, &res.nodes, &{
            let mut e = HashSet::new(); e.insert(res.root); e
        }, true);
        assert_eq!(only.len(), 2);
    }

    #[test]
    fn ndjson_arrays_compare() {
        let res = run("{\"a\":1}\n{\"a\":2}\n", "{\"a\":1}\n{\"a\":3}\n", DiffOptions::default());
        // Two records; second differs.
        assert_eq!(root_status(&res), DiffStatus::Changed);
        assert_eq!(res.changed, 1);
    }

    // ── fingerprint-accelerated ignore_array_order path ──────────────────────
    //
    // These exercise the hash-bucketing multiset matcher across every option
    // that feeds the fingerprint, ensuring it agrees with the deep comparison.

    fn ord() -> DiffOptions {
        DiffOptions { ignore_array_order: true, ..Default::default() }
    }

    #[test]
    fn ordered_multiset_respects_duplicates() {
        // Same multiset, shuffled → equal.
        assert_eq!(root_status(&run("[1,1,2]", "[2,1,1]", ord())), DiffStatus::Unchanged);
        // Different multiplicities → changed (one removal, one addition).
        let res = run("[1,1,2]", "[1,2,2]", ord());
        assert_eq!(root_status(&res), DiffStatus::Changed);
        assert_eq!((res.added, res.removed), (1, 1));
    }

    #[test]
    fn ordered_large_shuffle_is_unchanged() {
        // Big enough that an O(n²) regression would be obvious; verifies the
        // bucketing matches every element regardless of order.
        let fwd: Vec<String> = (0..500).map(|i| format!(r#"{{"id":{i}}}"#)).collect();
        let rev: Vec<String> = (0..500).rev().map(|i| format!(r#"{{"id":{i}}}"#)).collect();
        let l = format!("[{}]", fwd.join(","));
        let r = format!("[{}]", rev.join(","));
        assert_eq!(root_status(&run(&l, &r, ord())), DiffStatus::Unchanged);
    }

    #[test]
    fn ordered_with_nested_arrays() {
        // Inner arrays are themselves order-insensitive (global option).
        assert_eq!(root_status(&run("[[1,2],[3,4]]", "[[4,3],[2,1]]", ord())), DiffStatus::Unchanged);
        assert_eq!(root_status(&run("[[1,2],[3,4]]", "[[4,3],[2,9]]", ord())), DiffStatus::Changed);
    }

    #[test]
    fn ordered_number_normalization() {
        // 1 and 1.0 hash and compare equal inside the multiset.
        assert_eq!(root_status(&run("[1,2.0,3]", "[3.0,1.0,2]", ord())), DiffStatus::Unchanged);
    }

    #[test]
    fn ordered_with_ignore_case() {
        let opts = DiffOptions { ignore_array_order: true, ignore_case: true, ..Default::default() };
        assert_eq!(
            root_status(&run(r#"[{"Name":"BOB"},{"Name":"amy"}]"#, r#"[{"name":"AMY"},{"name":"bob"}]"#, opts)),
            DiffStatus::Unchanged
        );
    }

    #[test]
    fn ordered_with_null_equals_missing() {
        let opts = DiffOptions { ignore_array_order: true, null_equals_missing: true, ..Default::default() };
        // {"a":1,"b":null} must match {"a":1} within the shuffled array.
        assert_eq!(
            root_status(&run(r#"[{"a":1,"b":null},{"a":2}]"#, r#"[{"a":2},{"a":1}]"#, opts)),
            DiffStatus::Unchanged
        );
    }

    #[test]
    fn ordered_with_ignore_keys() {
        let opts = DiffOptions {
            ignore_array_order: true,
            ignore_keys: vec!["ts".into()],
            ..Default::default()
        };
        assert_eq!(
            root_status(&run(r#"[{"id":1,"ts":"x"},{"id":2,"ts":"y"}]"#, r#"[{"id":2,"ts":"z"},{"id":1,"ts":"w"}]"#, opts)),
            DiffStatus::Unchanged
        );
    }

    #[test]
    fn ordered_with_trim_whitespace() {
        let opts = DiffOptions { ignore_array_order: true, trim_whitespace: true, ..Default::default() };
        assert_eq!(root_status(&run(r#"["  a ","b"]"#, r#"["b","a"]"#, opts)), DiffStatus::Unchanged);
    }

    #[test]
    fn ordered_distinguishes_type() {
        // Number 1 and string "1" are not equal without coercion, even shuffled.
        let res = run(r#"[1,"x"]"#, r#"["1","x"]"#, ord());
        assert_eq!(root_status(&res), DiffStatus::Changed);
    }

    #[test]
    fn ordered_falls_back_under_coercion() {
        // Type coercion disables fingerprinting (non-transitive); the scan
        // still matches reordered cross-type-equal values.
        let opts = DiffOptions { ignore_array_order: true, type_coercion: true, ..Default::default() };
        assert_eq!(root_status(&run(r#"[1,"2"]"#, r#"["2",1]"#, opts)), DiffStatus::Unchanged);
    }
}
