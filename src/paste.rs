//! Turns pasted text into a viewable JSON document.
//!
//! Plain JSON passes through unchanged; a JWT token is decoded into
//! `{"header": …, "payload": …, "signature": "…"}`.

/// Decode `text` as a JWT — three dot-separated base64url segments whose
/// header and payload decode to JSON objects. Returns None when the text is
/// not a JWT (it should then be treated as plain JSON).
pub fn decode_jwt(text: &str) -> Option<Vec<u8>> {
    let text = text.trim();
    if !text
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'='))
    {
        return None;
    }

    let mut parts = text.split('.');
    let (header_b64, payload_b64, signature) = (parts.next()?, parts.next()?, parts.next()?);
    if parts.next().is_some() || header_b64.is_empty() || payload_b64.is_empty() {
        return None;
    }

    let header  = base64url_decode(header_b64)?;
    let payload = base64url_decode(payload_b64)?;
    // Both decoded segments must be JSON objects, otherwise this is not a JWT.
    if header.first() != Some(&b'{') || payload.first() != Some(&b'{') {
        return None;
    }

    let mut out = Vec::with_capacity(header.len() + payload.len() + signature.len() + 48);
    out.extend_from_slice(b"{\"header\":");
    out.extend_from_slice(&header);
    out.extend_from_slice(b",\"payload\":");
    out.extend_from_slice(&payload);
    out.extend_from_slice(b",\"signature\":\"");
    out.extend_from_slice(signature.as_bytes()); // base64url chars need no JSON escaping
    out.extend_from_slice(b"\"}");
    Some(out)
}

/// Minimal base64url (RFC 4648 §5) decoder; also accepts the standard
/// alphabet and ignores trailing `=` padding.
fn base64url_decode(s: &str) -> Option<Vec<u8>> {
    let s = s.trim_end_matches('=');
    let mut out = Vec::with_capacity(s.len() * 3 / 4 + 2);
    let mut buf: u32 = 0;
    let mut bits: u32 = 0;
    for b in s.bytes() {
        let v = match b {
            b'A'..=b'Z' => b - b'A',
            b'a'..=b'z' => b - b'a' + 26,
            b'0'..=b'9' => b - b'0' + 52,
            b'-' | b'+' => 62,
            b'_' | b'/' => 63,
            _ => return None,
        };
        buf = (buf << 6) | v as u32;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((buf >> bits) as u8);
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    // Standard HS256 example token (header/payload from jwt.io).
    const JWT: &str = "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.\
        eyJzdWIiOiIxMjM0NTY3ODkwIiwibmFtZSI6IkpvaG4gRG9lIiwiaWF0IjoxNTE2MjM5MDIyfQ.\
        SflKxwRJSMeKKF2QT4fwpMeJf36POk6yJV_adQssw5c";

    fn jwt() -> String {
        JWT.replace(char::is_whitespace, "")
    }

    #[test]
    fn decodes_jwt_into_json() {
        let out = decode_jwt(&jwt()).expect("should decode");
        let s = String::from_utf8(out.clone()).unwrap();
        assert!(s.contains(r#""alg":"HS256""#), "{s}");
        assert!(s.contains(r#""name":"John Doe""#), "{s}");
        assert!(s.contains(r#""signature":"SflKxwRJSMeKKF2QT4fwpMeJf36POk6yJV_adQssw5c""#), "{s}");

        // The produced document must itself be parseable.
        let mut arena = Vec::new();
        crate::parser::parse_bytes(&out, &mut arena, &mut |_| {}).expect("valid JSON");
    }

    #[test]
    fn decodes_jwt_with_surrounding_whitespace() {
        assert!(decode_jwt(&format!("  {}\n", jwt())).is_some());
    }

    #[test]
    fn accepts_empty_signature() {
        let unsigned: String = jwt().rsplit_once('.').map(|(hp, _)| format!("{hp}.")).unwrap();
        let out = decode_jwt(&unsigned).expect("unsigned JWT should decode");
        assert!(String::from_utf8(out).unwrap().contains(r#""signature":"""#));
    }

    #[test]
    fn rejects_plain_json() {
        assert!(decode_jwt(r#"{"a": 1}"#).is_none());
        assert!(decode_jwt(r#"{"a": "x.y.z"}"#).is_none());
    }

    #[test]
    fn rejects_non_jwt_text() {
        assert!(decode_jwt("hello world").is_none());
        assert!(decode_jwt("a.b.c").is_none());          // segments not JSON objects
        assert!(decode_jwt("1.2.3.4").is_none());        // too many segments
        assert!(decode_jwt("").is_none());
    }

    #[test]
    fn base64url_roundtrip() {
        assert_eq!(base64url_decode("aGVsbG8").unwrap(), b"hello");
        assert_eq!(base64url_decode("aGVsbG8=").unwrap(), b"hello");
        assert!(base64url_decode("a!b").is_none());
    }
}
