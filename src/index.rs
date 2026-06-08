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
#[derive(Clone, Debug)]
pub struct Node {
    pub kind:         NodeKind,
    pub depth:        u16,
    /// Byte range in the mmap (value_start..value_end).
    pub value_start:  u64,
    pub value_end:    u64,
    /// Byte range in JsonIndex::key_arena for the object-key string.
    pub key_start:    u32,
    pub key_len:      u16,   // 0 = no key
    pub first_child:  u32,
    pub next_sibling: u32,
    pub child_count:  u32,
    pub parent:       u32,   // u32::MAX = no parent (root)
    pub array_index:  u32,   // u32::MAX = not an array element; else 0-based index in parent array
}

pub struct JsonIndex {
    pub _file:     File,
    pub mmap:      Mmap,
    pub nodes:     Vec<Node>,
    pub key_arena: Vec<u8>,
    pub root:      u32,
    pub is_ndjson: bool,
}

impl JsonIndex {
    /// Borrow the key string for a node. Returns "" when key_len == 0.
    pub fn key_of<'a>(&'a self, n: &Node) -> &'a str {
        if n.key_len == 0 {
            return "";
        }
        let s = n.key_start as usize;
        let e = s + n.key_len as usize;
        std::str::from_utf8(&self.key_arena[s..e]).unwrap_or("?")
    }

    /// Borrow the raw bytes of a value from the mmap.
    pub fn value_bytes<'a>(&'a self, n: &Node) -> &'a [u8] {
        &self.mmap[n.value_start as usize..n.value_end as usize]
    }
}
