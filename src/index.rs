use memmap2::Mmap;
use std::fs::File;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum NodeKind {
    Object = 0,
    Array  = 1,
    String = 2,
    Number = 3,
    Bool   = 4,
    Null   = 5,
}

/// One flat entry in the node array. All u32 indices use u32::MAX as "none".
///
/// Nodes are stored in pre-order: a container's first child (if any) is
/// always the node right after it, so `first_child` is not stored — use
/// [`first_child`] / [`JsonIndex::first_child`]. Byte offsets are u32,
/// capping supported input at 4 GiB (checked at parse time). Kept to 36
/// bytes — on multi-million-node files this dominates memory use.
#[derive(Clone, Debug)]
pub struct Node {
    /// Byte range in the source data (value_start..value_end).
    pub value_start:  u32,
    pub value_end:    u32,
    /// Object-key string lives in the source bytes just before the value:
    /// key content starts at `value_start - key_start` (back-offset), length
    /// `key_len`. Keys are stored raw (escapes untouched), same as the source.
    pub key_start:    u32,
    pub next_sibling: u32,
    pub child_count:  u32,
    pub parent:       u32,   // u32::MAX = no parent (root)
    pub array_index:  u32,   // u32::MAX = not an array element; else 0-based index in parent array
    pub key_len:      u16,   // 0 = no key
    pub depth:        u16,
    pub kind:         NodeKind,
}

const _: () = assert!(std::mem::size_of::<Node>() == 36, "Node grew — see memory note above");

/// First child of `idx`, derived from pre-order layout: the first child of a
/// non-empty container is always the next node. u32::MAX when childless.
#[inline]
pub fn first_child(nodes: &[Node], idx: u32) -> u32 {
    if nodes[idx as usize].child_count > 0 { idx + 1 } else { u32::MAX }
}

/// Backing storage for the raw JSON text — an mmap'd file or an in-memory
/// buffer (pasted text).
pub enum JsonData {
    Mapped { _file: File, mmap: Mmap },
    Memory(Vec<u8>),
}

impl JsonData {
    pub fn bytes(&self) -> &[u8] {
        match self {
            JsonData::Mapped { mmap, .. } => &mmap[..],
            JsonData::Memory(buf)         => buf,
        }
    }
}

/// Dense bit-set keyed by node index. Drop-in for the membership subset of
/// `HashSet<u32>` (contains/insert/remove/clear) at ~1 bit per node instead
/// of ~40 bytes per entry — matters after "Expand all" on multi-million-node
/// files. Synthetic added-item ids (beyond nodes.len()) grow the set lazily.
#[derive(Default, Clone)]
pub struct NodeSet {
    words: Vec<u64>,
}

impl NodeSet {
    pub fn new() -> Self {
        Self { words: Vec::new() }
    }

    #[inline]
    pub fn contains(&self, id: &u32) -> bool {
        let w = (*id as usize) / 64;
        self.words.get(w).is_some_and(|word| word & (1 << (*id as usize % 64)) != 0)
    }

    /// Returns true if the id was not already present (HashSet semantics).
    pub fn insert(&mut self, id: u32) -> bool {
        let w = (id as usize) / 64;
        if w >= self.words.len() {
            self.words.resize(w + 1, 0);
        }
        let bit = 1u64 << (id as usize % 64);
        let was = self.words[w] & bit != 0;
        self.words[w] |= bit;
        !was
    }

    /// Returns true if the id was present (HashSet semantics).
    pub fn remove(&mut self, id: &u32) -> bool {
        let w = (*id as usize) / 64;
        let Some(word) = self.words.get_mut(w) else { return false };
        let bit = 1u64 << (*id as usize % 64);
        let was = *word & bit != 0;
        *word &= !bit;
        was
    }

    pub fn clear(&mut self) {
        self.words.clear();
    }
}

pub struct JsonIndex {
    pub data:      JsonData,
    pub nodes:     Vec<Node>,
    pub root:      u32,
    pub is_ndjson: bool,
}

impl JsonIndex {
    /// See [`first_child`].
    #[inline]
    pub fn first_child(&self, idx: u32) -> u32 {
        first_child(&self.nodes, idx)
    }

    /// Borrow the key string for a node. Returns "" when key_len == 0.
    pub fn key_of<'a>(&'a self, n: &Node) -> &'a str {
        if n.key_len == 0 {
            return "";
        }
        let s = (n.value_start - n.key_start) as usize;
        let e = s + n.key_len as usize;
        std::str::from_utf8(&self.data.bytes()[s..e]).unwrap_or("?")
    }

    /// Borrow the raw bytes of a value from the source data.
    pub fn value_bytes<'a>(&'a self, n: &Node) -> &'a [u8] {
        &self.data.bytes()[n.value_start as usize..n.value_end as usize]
    }
}
