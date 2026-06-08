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
    key_arena:   &mut Vec<u8>,
    progress_cb: &mut dyn FnMut(f32),
) -> Result<(Vec<Node>, u32, bool), ParseError> {
    let total = bytes.len();
    let mut prog = Progress { last: 0, total, cb: progress_cb };
    let mut nodes: Vec<Node> = Vec::new();

    let mut pos = 0usize;
    skip_ws(bytes, &mut pos);
    if pos >= bytes.len() {
        return Err(ParseError { offset: 0, msg: "empty input" });
    }

    // Parse first top-level value
    let root = scan_value(bytes, &mut pos, 0, u32::MAX, 0, 0, &mut nodes, key_arena, &mut prog, u32::MAX)?;

    // Detect NDJSON: remaining non-whitespace after the first value
    skip_ws_newlines(bytes, &mut pos);
    if pos < bytes.len() {
        // NDJSON mode — re-parse from scratch
        nodes.clear();
        key_arena.clear();
        pos = 0;

        // Synthetic root Array node
        nodes.push(Node {
            kind:         NodeKind::Array,
            depth:        0,
            value_start:  0,
            value_end:    bytes.len() as u64,
            key_start:    0,
            key_len:      0,
            first_child:  u32::MAX,
            next_sibling: u32::MAX,
            child_count:  0,
            parent:       u32::MAX,
            array_index:  u32::MAX,
        });
        let root_idx: u32 = 0;

        let mut prev_child: u32 = u32::MAX;
        let mut child_count: u32 = 0;

        while pos < bytes.len() {
            skip_ws_newlines(bytes, &mut pos);
            if pos >= bytes.len() { break; }

            let child_idx = scan_value(
                bytes, &mut pos, 1, root_idx, 0, 0,
                &mut nodes, key_arena, &mut prog,
                child_count,
            )?;

            if prev_child == u32::MAX {
                nodes[root_idx as usize].first_child = child_idx;
            } else {
                nodes[prev_child as usize].next_sibling = child_idx;
            }
            prev_child = child_idx;
            child_count += 1;

            prog.check(pos);
            // skip to next newline
            while pos < bytes.len() && bytes[pos] != b'\n' { pos += 1; }
        }

        nodes[root_idx as usize].child_count = child_count;
        return Ok((nodes, root_idx, true));
    }

    Ok((nodes, root, false))
}

fn skip_ws(bytes: &[u8], pos: &mut usize) {
    while *pos < bytes.len() && matches!(bytes[*pos], b' ' | b'\t' | b'\r' | b'\n') {
        *pos += 1;
    }
}

fn skip_ws_newlines(bytes: &[u8], pos: &mut usize) {
    while *pos < bytes.len() && matches!(bytes[*pos], b' ' | b'\t' | b'\r' | b'\n') {
        *pos += 1;
    }
}

/// Returns `(content_start, content_end)` inside the quotes; `pos` moves past closing `"`.
fn scan_string(bytes: &[u8], pos: &mut usize) -> Result<(usize, usize), ParseError> {
    if *pos >= bytes.len() || bytes[*pos] != b'"' {
        return Err(ParseError { offset: *pos as u64, msg: "expected '\"'" });
    }
    *pos += 1; // skip opening '"'
    let start = *pos;
    while *pos < bytes.len() {
        match bytes[*pos] {
            b'\\' => { *pos += 2; } // escape sequence — skip both bytes
            b'"'  => {
                let end = *pos;
                *pos += 1; // skip closing '"'
                return Ok((start, end));
            }
            _ => { *pos += 1; }
        }
    }
    Err(ParseError { offset: *pos as u64, msg: "unterminated string" })
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
    key_start:   u32,
    key_len:     u16,
    nodes:       &mut Vec<Node>,
    key_arena:   &mut Vec<u8>,
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
    let value_start = *pos as u64;

    match bytes[*pos] {
        b'{' => {
            nodes.push(Node {
                kind: NodeKind::Object, depth, value_start, value_end: 0,
                key_start, key_len, first_child: u32::MAX, next_sibling: u32::MAX,
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
                let k_arena_start = key_arena.len() as u32;
                let k_len = (ke - ks) as u16;
                key_arena.extend_from_slice(&bytes[ks..ke]);
                // Colon
                skip_ws(bytes, pos);
                if *pos >= bytes.len() || bytes[*pos] != b':' {
                    return Err(ParseError { offset: *pos as u64, msg: "expected ':' in object" });
                }
                *pos += 1;
                // Value
                let child = scan_value(bytes, pos, depth + 1, own_idx, k_arena_start, k_len, nodes, key_arena, prog, u32::MAX)?;
                if prev_child == u32::MAX {
                    nodes[own_idx as usize].first_child = child;
                } else {
                    nodes[prev_child as usize].next_sibling = child;
                }
                prev_child = child;
                count += 1;
            }
            nodes[own_idx as usize].child_count = count;
            nodes[own_idx as usize].value_end = *pos as u64;
        }
        b'[' => {
            nodes.push(Node {
                kind: NodeKind::Array, depth, value_start, value_end: 0,
                key_start, key_len, first_child: u32::MAX, next_sibling: u32::MAX,
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
                let child = scan_value(bytes, pos, depth + 1, own_idx, 0, 0, nodes, key_arena, prog, count)?;
                if prev_child == u32::MAX {
                    nodes[own_idx as usize].first_child = child;
                } else {
                    nodes[prev_child as usize].next_sibling = child;
                }
                prev_child = child;
                count += 1;
            }
            nodes[own_idx as usize].child_count = count;
            nodes[own_idx as usize].value_end = *pos as u64;
        }
        b'"' => {
            let start = *pos;
            scan_string(bytes, pos)?;
            nodes.push(Node {
                kind: NodeKind::String, depth, value_start: start as u64, value_end: *pos as u64,
                key_start, key_len, first_child: u32::MAX, next_sibling: u32::MAX,
                child_count: 0, parent, array_index,
            });
        }
        b't' => {
            if !scan_keyword(bytes, pos, b"true") {
                return Err(ParseError { offset: *pos as u64, msg: "invalid literal" });
            }
            nodes.push(Node {
                kind: NodeKind::Bool, depth, value_start, value_end: *pos as u64,
                key_start, key_len, first_child: u32::MAX, next_sibling: u32::MAX,
                child_count: 0, parent, array_index,
            });
        }
        b'f' => {
            if !scan_keyword(bytes, pos, b"false") {
                return Err(ParseError { offset: *pos as u64, msg: "invalid literal" });
            }
            nodes.push(Node {
                kind: NodeKind::Bool, depth, value_start, value_end: *pos as u64,
                key_start, key_len, first_child: u32::MAX, next_sibling: u32::MAX,
                child_count: 0, parent, array_index,
            });
        }
        b'n' => {
            if !scan_keyword(bytes, pos, b"null") {
                return Err(ParseError { offset: *pos as u64, msg: "invalid literal" });
            }
            nodes.push(Node {
                kind: NodeKind::Null, depth, value_start, value_end: *pos as u64,
                key_start, key_len, first_child: u32::MAX, next_sibling: u32::MAX,
                child_count: 0, parent, array_index,
            });
        }
        b'0'..=b'9' | b'-' => {
            scan_number(bytes, pos);
            nodes.push(Node {
                kind: NodeKind::Number, depth, value_start, value_end: *pos as u64,
                key_start, key_len, first_child: u32::MAX, next_sibling: u32::MAX,
                child_count: 0, parent, array_index,
            });
        }
        _ => {
            return Err(ParseError { offset: *pos as u64, msg: "unexpected byte" });
        }
    }

    Ok(own_idx)
}

