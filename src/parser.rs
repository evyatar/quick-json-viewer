use memchr::memchr2;

use crate::index::{Node, NodeKind};

#[derive(Debug)]
pub struct ParseError {
    pub offset: u64,
    pub msg:    &'static str,
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "parse error at byte {}: {}", self.offset, self.msg)
    }
}

struct Progress<'a> {
    last:  usize,
    total: usize,
    cb:    &'a mut dyn FnMut(f32),
}

impl<'a> Progress<'a> {
    fn check(&mut self, pos: usize) {
        const INTERVAL: usize = 16 * 1024 * 1024;
        if pos.saturating_sub(self.last) >= INTERVAL {
            self.last = pos;
            (self.cb)(pos as f32 / self.total.max(1) as f32);
        }
    }
}

pub fn parse_bytes(
    bytes:       &[u8],
    progress_cb: &mut dyn FnMut(f32),
) -> Result<(Vec<Node>, u32, bool), ParseError> {
    let total = bytes.len();
    // Node offsets are u32 — see `index::Node`.
    if total > u32::MAX as usize {
        return Err(ParseError { offset: 0, msg: "file too large (max 4 GiB)" });
    }
    let mut prog = Progress { last: 0, total, cb: progress_cb };
    // Pre-allocate based on file size to avoid repeated doubling reallocations.
    // Empirically, dense JSON produces ~1 node per 24-45 bytes; shrunk to fit
    // after parsing.
    let mut nodes: Vec<Node> = Vec::with_capacity((total / 24).max(64));

    let mut pos = 0usize;
    skip_ws(bytes, &mut pos);
    if pos >= bytes.len() {
        return Err(ParseError { offset: 0, msg: "empty input" });
    }

    // Parse first top-level value
    let root = scan_value(bytes, &mut pos, 0, u32::MAX, 0, 0, &mut nodes, &mut prog, u32::MAX)?;

    // Detect NDJSON: remaining non-whitespace after the first value
    skip_ws(bytes, &mut pos);
    if pos < bytes.len() {
        // NDJSON mode — re-parse from scratch
        nodes.clear();
        pos = 0;

        // Synthetic root Array node
        nodes.push(Node {
            kind:         NodeKind::Array,
            depth:        0,
            value_start:  0,
            value_end:    bytes.len() as u32,
            key_start:    0,
            key_len:      0,
            next_sibling: u32::MAX,
            child_count:  0,
            parent:       u32::MAX,
            array_index:  u32::MAX,
        });
        let root_idx: u32 = 0;

        let mut prev_child: u32 = u32::MAX;
        let mut child_count: u32 = 0;

        while pos < bytes.len() {
            skip_ws(bytes, &mut pos);
            if pos >= bytes.len() { break; }

            let child_idx = scan_value(
                bytes, &mut pos, 1, root_idx, 0, 0,
                &mut nodes, &mut prog,
                child_count,
            )?;

            if prev_child != u32::MAX {
                nodes[prev_child as usize].next_sibling = child_idx;
            }
            prev_child = child_idx;
            child_count += 1;

            prog.check(pos);
            // skip to next newline
            while pos < bytes.len() && bytes[pos] != b'\n' { pos += 1; }
        }

        nodes[root_idx as usize].child_count = child_count;
        nodes.shrink_to_fit();
        return Ok((nodes, root_idx, true));
    }

    nodes.shrink_to_fit();
    Ok((nodes, root, false))
}

fn skip_ws(bytes: &[u8], pos: &mut usize) {
    let mut p = *pos;
    let len = bytes.len();
    loop {
        // Pretty-printed JSON is dominated by indentation runs — skip
        // 8 spaces at a time before falling back to byte-wise.
        const SPACES: u64 = u64::from_le_bytes(*b"        ");
        while p + 8 <= len
            && u64::from_le_bytes(bytes[p..p + 8].try_into().unwrap()) == SPACES
        {
            p += 8;
        }
        if p < len && matches!(bytes[p], b' ' | b'\t' | b'\r' | b'\n') {
            p += 1;
        } else {
            break;
        }
    }
    *pos = p;
}

/// Returns `(content_start, content_end)` inside the quotes; `pos` moves past closing `"`.
fn scan_string(bytes: &[u8], pos: &mut usize) -> Result<(usize, usize), ParseError> {
    if *pos >= bytes.len() || bytes[*pos] != b'"' {
        return Err(ParseError { offset: *pos as u64, msg: "expected '\"'" });
    }
    *pos += 1; // skip opening '"'
    let start = *pos;
    // Use SIMD-backed memchr2 to jump past plain bytes in bulk.
    loop {
        match memchr2(b'"', b'\\', &bytes[*pos..]) {
            None => return Err(ParseError { offset: bytes.len() as u64, msg: "unterminated string" }),
            Some(i) => {
                *pos += i;
                if bytes[*pos] == b'"' {
                    let end = *pos;
                    *pos += 1;
                    return Ok((start, end));
                }
                // backslash escape — skip both bytes
                *pos += 2;
            }
        }
    }
}

fn scan_number(bytes: &[u8], pos: &mut usize) -> usize {
    if *pos < bytes.len() && bytes[*pos] == b'-' { *pos += 1; }
    while *pos < bytes.len() && bytes[*pos].is_ascii_digit() { *pos += 1; }
    if *pos < bytes.len() && bytes[*pos] == b'.' {
        *pos += 1;
        while *pos < bytes.len() && bytes[*pos].is_ascii_digit() { *pos += 1; }
    }
    if *pos < bytes.len() && matches!(bytes[*pos], b'e' | b'E') {
        *pos += 1;
        if *pos < bytes.len() && matches!(bytes[*pos], b'+' | b'-') { *pos += 1; }
        while *pos < bytes.len() && bytes[*pos].is_ascii_digit() { *pos += 1; }
    }
    *pos
}

fn scan_keyword(bytes: &[u8], pos: &mut usize, kw: &[u8]) -> bool {
    let end = *pos + kw.len();
    if end <= bytes.len() && &bytes[*pos..end] == kw {
        *pos = end;
        true
    } else {
        false
    }
}

fn scan_value(
    bytes:       &[u8],
    pos:         &mut usize,
    depth:       u16,
    parent:      u32,
    key_abs:     usize, // absolute source offset of key content (ignored when key_len == 0)
    key_len:     u16,
    nodes:       &mut Vec<Node>,
    prog:        &mut Progress<'_>,
    array_index: u32,
) -> Result<u32, ParseError> {
    if depth > 10_000 {
        return Err(ParseError { offset: *pos as u64, msg: "max depth exceeded" });
    }

    skip_ws(bytes, pos);
    if *pos >= bytes.len() {
        return Err(ParseError { offset: *pos as u64, msg: "unexpected end of input" });
    }

    prog.check(*pos);

    let own_idx = nodes.len() as u32;
    let value_start = *pos as u32;
    // Key is stored as a back-offset from value_start into the source bytes.
    let key_start = if key_len == 0 { 0 } else { value_start - key_abs as u32 };

    match bytes[*pos] {
        b'{' => {
            nodes.push(Node {
                kind: NodeKind::Object, depth, value_start, value_end: 0,
                key_start, key_len, next_sibling: u32::MAX,
                child_count: 0, parent, array_index,
            });
            *pos += 1; // skip '{'
            let mut prev_child: u32 = u32::MAX;
            let mut count: u32 = 0;
            loop {
                skip_ws(bytes, pos);
                if *pos >= bytes.len() {
                    return Err(ParseError { offset: *pos as u64, msg: "unclosed object" });
                }
                if bytes[*pos] == b'}' { *pos += 1; break; }
                if count > 0 {
                    if bytes[*pos] != b',' {
                        return Err(ParseError { offset: *pos as u64, msg: "expected ',' in object" });
                    }
                    *pos += 1;
                    skip_ws(bytes, pos);
                    if *pos < bytes.len() && bytes[*pos] == b'}' { *pos += 1; break; } // trailing comma
                }
                // Key
                let (ks, ke) = scan_string(bytes, pos)?;
                let k_len = (ke - ks) as u16;
                // Colon
                skip_ws(bytes, pos);
                if *pos >= bytes.len() || bytes[*pos] != b':' {
                    return Err(ParseError { offset: *pos as u64, msg: "expected ':' in object" });
                }
                *pos += 1;
                // Value
                let child = scan_value(bytes, pos, depth + 1, own_idx, ks, k_len, nodes, prog, u32::MAX)?;
                if prev_child != u32::MAX {
                    nodes[prev_child as usize].next_sibling = child;
                }
                prev_child = child;
                count += 1;
            }
            nodes[own_idx as usize].child_count = count;
            nodes[own_idx as usize].value_end = *pos as u32;
        }
        b'[' => {
            nodes.push(Node {
                kind: NodeKind::Array, depth, value_start, value_end: 0,
                key_start, key_len, next_sibling: u32::MAX,
                child_count: 0, parent, array_index,
            });
            *pos += 1; // skip '['
            let mut prev_child: u32 = u32::MAX;
            let mut count: u32 = 0;
            loop {
                skip_ws(bytes, pos);
                if *pos >= bytes.len() {
                    return Err(ParseError { offset: *pos as u64, msg: "unclosed array" });
                }
                if bytes[*pos] == b']' { *pos += 1; break; }
                if count > 0 {
                    if bytes[*pos] != b',' {
                        return Err(ParseError { offset: *pos as u64, msg: "expected ',' in array" });
                    }
                    *pos += 1;
                    skip_ws(bytes, pos);
                    if *pos < bytes.len() && bytes[*pos] == b']' { *pos += 1; break; } // trailing comma
                }
                let child = scan_value(bytes, pos, depth + 1, own_idx, 0, 0, nodes, prog, count)?;
                if prev_child != u32::MAX {
                    nodes[prev_child as usize].next_sibling = child;
                }
                prev_child = child;
                count += 1;
            }
            nodes[own_idx as usize].child_count = count;
            nodes[own_idx as usize].value_end = *pos as u32;
        }
        b'"' => {
            let start = *pos;
            scan_string(bytes, pos)?;
            nodes.push(Node {
                kind: NodeKind::String, depth, value_start: start as u32, value_end: *pos as u32,
                key_start, key_len, next_sibling: u32::MAX,
                child_count: 0, parent, array_index,
            });
        }
        b't' => {
            if !scan_keyword(bytes, pos, b"true") {
                return Err(ParseError { offset: *pos as u64, msg: "invalid literal" });
            }
            nodes.push(Node {
                kind: NodeKind::Bool, depth, value_start, value_end: *pos as u32,
                key_start, key_len, next_sibling: u32::MAX,
                child_count: 0, parent, array_index,
            });
        }
        b'f' => {
            if !scan_keyword(bytes, pos, b"false") {
                return Err(ParseError { offset: *pos as u64, msg: "invalid literal" });
            }
            nodes.push(Node {
                kind: NodeKind::Bool, depth, value_start, value_end: *pos as u32,
                key_start, key_len, next_sibling: u32::MAX,
                child_count: 0, parent, array_index,
            });
        }
        b'n' => {
            if !scan_keyword(bytes, pos, b"null") {
                return Err(ParseError { offset: *pos as u64, msg: "invalid literal" });
            }
            nodes.push(Node {
                kind: NodeKind::Null, depth, value_start, value_end: *pos as u32,
                key_start, key_len, next_sibling: u32::MAX,
                child_count: 0, parent, array_index,
            });
        }
        b'0'..=b'9' | b'-' => {
            scan_number(bytes, pos);
            nodes.push(Node {
                kind: NodeKind::Number, depth, value_start, value_end: *pos as u32,
                key_start, key_len, next_sibling: u32::MAX,
                child_count: 0, parent, array_index,
            });
        }
        _ => {
            return Err(ParseError { offset: *pos as u64, msg: "unexpected byte" });
        }
    }

    Ok(own_idx)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::{first_child, NodeKind};

    fn parse(s: &str) -> (Vec<Node>, u32, bool) {
        parse_bytes(s.as_bytes(), &mut |_| {}).unwrap()
    }

    fn parse_err(s: &str) -> ParseError {
        parse_bytes(s.as_bytes(), &mut |_| {}).unwrap_err()
    }

    #[test]
    fn empty_input_is_error() {
        parse_err("");
        parse_err("   ");
    }

    #[test]
    fn empty_object() {
        let (nodes, root, is_ndjson) = parse("{}");
        assert_eq!(nodes[root as usize].kind, NodeKind::Object);
        assert_eq!(nodes[root as usize].child_count, 0);
        assert!(!is_ndjson);
    }

    #[test]
    fn empty_array() {
        let (nodes, root, _) = parse("[]");
        assert_eq!(nodes[root as usize].kind, NodeKind::Array);
        assert_eq!(nodes[root as usize].child_count, 0);
    }

    #[test]
    fn object_with_string_value() {
        let input: &[u8] = b"{\"key\": \"value\"}";
        let (nodes, root, _) = parse_bytes(input, &mut |_| {}).unwrap();
        assert_eq!(nodes[root as usize].child_count, 1);
        let child = &nodes[first_child(&nodes, root) as usize];
        assert_eq!(child.kind, NodeKind::String);
        let ks = (child.value_start - child.key_start) as usize;
        assert_eq!(&input[ks..ks + child.key_len as usize], b"key");
    }

    #[test]
    fn array_with_numbers() {
        let (nodes, root, _) = parse("[1, 2, 3]");
        assert_eq!(nodes[root as usize].kind, NodeKind::Array);
        assert_eq!(nodes[root as usize].child_count, 3);
        let first = first_child(&nodes, root) as usize;
        assert_eq!(nodes[first].kind, NodeKind::Number);
    }

    #[test]
    fn bool_and_null_values() {
        let (nodes, root, _) = parse("[true, false, null]");
        let first = first_child(&nodes, root) as usize;
        assert_eq!(nodes[first].kind, NodeKind::Bool);
        let second = nodes[first].next_sibling as usize;
        assert_eq!(nodes[second].kind, NodeKind::Bool);
        let third = nodes[second].next_sibling as usize;
        assert_eq!(nodes[third].kind, NodeKind::Null);
    }

    #[test]
    fn nested_object() {
        let (nodes, root, _) = parse(r#"{"a": {"b": 1}}"#);
        assert_eq!(nodes[root as usize].child_count, 1);
        let a = first_child(&nodes, root) as usize;
        assert_eq!(nodes[a].kind, NodeKind::Object);
        assert_eq!(nodes[a].child_count, 1);
    }

    #[test]
    fn trailing_comma_in_object() {
        let (nodes, root, _) = parse(r#"{"a": 1,}"#);
        assert_eq!(nodes[root as usize].child_count, 1);
    }

    #[test]
    fn trailing_comma_in_array() {
        let (nodes, root, _) = parse("[1, 2,]");
        assert_eq!(nodes[root as usize].child_count, 2);
    }

    #[test]
    fn ndjson_detected() {
        let (nodes, root, is_ndjson) = parse("{}\n{}\n");
        assert!(is_ndjson);
        assert_eq!(nodes[root as usize].kind, NodeKind::Array);
        assert_eq!(nodes[root as usize].child_count, 2);
    }

    #[test]
    fn parent_links_are_correct() {
        let (nodes, root, _) = parse(r#"{"a": 1}"#);
        let child = first_child(&nodes, root);
        assert_eq!(nodes[child as usize].parent, root);
        assert_eq!(nodes[root as usize].parent, u32::MAX);
    }

    #[test]
    fn sibling_links_are_correct() {
        let (nodes, root, _) = parse("[1, 2]");
        let first = first_child(&nodes, root);
        let second = nodes[first as usize].next_sibling;
        assert_ne!(second, u32::MAX);
        assert_eq!(nodes[second as usize].next_sibling, u32::MAX);
    }

    #[test]
    fn array_indices_are_zero_based() {
        let (nodes, root, _) = parse("[10, 20, 30]");
        let mut child = first_child(&nodes, root);
        for expected_idx in 0u32..3 {
            assert_eq!(nodes[child as usize].array_index, expected_idx);
            child = nodes[child as usize].next_sibling;
        }
    }

    #[test]
    fn unclosed_object_is_error() {
        parse_err(r#"{"a": 1"#);
    }

    #[test]
    fn unterminated_string_is_error() {
        parse_err(r#"{"a"#);
    }

    #[test]
    fn invalid_literal_is_error() {
        parse_err("xyz");
    }

    #[test]
    fn value_byte_ranges_are_correct() {
        let input = b"[42]";
        let (nodes, root, _) = parse_bytes(input, &mut |_| {}).unwrap();
        let child = first_child(&nodes, root) as usize;
        let start = nodes[child].value_start as usize;
        let end = nodes[child].value_end as usize;
        assert_eq!(&input[start..end], b"42");
    }
}

