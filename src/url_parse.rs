//! Parses plain URLs, curl commands, and JS fetch() calls into an HTTP request
//! spec that the loader can execute.

pub struct HttpRequest {
    pub url:     String,
    pub headers: Vec<(String, String)>,
    /// HTTP method for ureq-based requests (plain URL / fetch). `None` → GET.
    /// Ignored for curl commands (the `-X` flag is forwarded inside `curl_args`).
    pub method: Option<String>,
    /// Request body extracted from `fetch(url, { body: "..." })`.
    pub body: Option<String>,
    /// When `Some`, the request came from a curl command: pass these shell-split
    /// args directly to the `curl` binary (with stdout capture appended).
    /// When `None`, use ureq to fetch `url` with `headers`.
    pub curl_args: Option<Vec<String>>,
}

/// Try to parse `text` as a URL, curl command, or fetch() call.
/// Returns `None` if the text doesn't look like any of these.
pub fn parse_request(text: &str) -> Option<HttpRequest> {
    let text = text.trim();
    if let Some(r) = parse_curl(text)  { return Some(r); }
    if let Some(r) = parse_fetch(text) { return Some(r); }
    parse_plain_url(text)
}

/// Short display name derived from a URL: host + path, no scheme or query.
pub fn url_display_name(url: &str) -> String {
    let s = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
        .unwrap_or(url);
    let s = s.split('?').next().unwrap_or(s);
    let s = s.split('#').next().unwrap_or(s);
    s.trim_end_matches('/').to_owned()
}

// ─── parsers ─────────────────────────────────────────────────────────────────

fn parse_plain_url(text: &str) -> Option<HttpRequest> {
    if text.contains('\n') || text.contains(' ') || text.contains('\t') {
        return None;
    }
    if text.starts_with("http://") || text.starts_with("https://") {
        Some(HttpRequest { url: text.to_owned(), headers: Vec::new(), method: None, body: None, curl_args: None })
    } else {
        None
    }
}

fn parse_curl(text: &str) -> Option<HttpRequest> {
    // Must start with "curl" followed by whitespace
    let rest = text
        .strip_prefix("curl")
        .filter(|s| s.starts_with(|c: char| c.is_whitespace()))?;

    let args = shell_split(rest);
    let mut url: Option<String> = None;
    let mut headers: Vec<(String, String)> = Vec::new();
    // Shell-split args forwarded verbatim to the curl binary (minus -o/--output).
    let mut curl_args: Vec<String> = Vec::new();

    let mut i = 0;
    while i < args.len() {
        let arg = &args[i];

        // --header=Value  (long form with =)
        if let Some(val) = arg.strip_prefix("--header=") {
            push_header(&mut headers, val);
            curl_args.push(arg.clone());
            i += 1;
            continue;
        }

        // --output=file — drop so we can capture stdout ourselves
        if arg.strip_prefix("--output=").is_some() {
            i += 1;
            continue;
        }

        match arg.as_str() {
            "-H" | "--header" => {
                i += 1;
                if let Some(h) = args.get(i) {
                    push_header(&mut headers, h);
                    curl_args.push("-H".to_owned());
                    curl_args.push(h.clone());
                }
            }
            // Output flags — drop so we can capture stdout ourselves
            "-o" | "--output" | "-c" | "--cookie-jar" => {
                i += 1; // skip value, don't add to curl_args
            }
            // Flags with values — forward to curl_args
            "-X" | "--request"
            | "-d" | "--data" | "--data-raw" | "--data-binary" | "--data-urlencode"
            | "-u" | "--user"
            | "-A" | "--user-agent"
            | "--connect-timeout" | "--max-time" | "-m"
            | "-F" | "--form"
            | "--cert" | "--key" | "--cacert"
            | "-e" | "--referer"
            | "--proxy" | "-x"
            | "-b" | "--cookie" => {
                i += 1;
                if let Some(val) = args.get(i) {
                    curl_args.push(arg.clone());
                    curl_args.push(val.clone());
                }
            }
            // Boolean flags — forward to curl_args
            "-L" | "--location"
            | "--silent" | "-s"
            | "-v" | "--verbose"
            | "-k" | "--insecure"
            | "-i" | "--include"
            | "--compressed"
            | "--no-keepalive"
            | "--http1.1" | "--http2" | "--http2-prior-knowledge"
            | "-I" | "--head"
            | "-g" | "--globoff" => {
                curl_args.push(arg.clone());
            }
            // URL
            _ if arg.starts_with("http://") || arg.starts_with("https://") => {
                url = Some(arg.clone());
                curl_args.push(arg.clone());
            }
            _ => {
                // Unknown — forward as-is
                curl_args.push(arg.clone());
            }
        }
        i += 1;
    }

    Some(HttpRequest { url: url?, headers, method: None, body: None, curl_args: Some(curl_args) })
}

fn push_header(headers: &mut Vec<(String, String)>, raw: &str) {
    if let Some((k, v)) = raw.split_once(':') {
        headers.push((k.trim().to_owned(), v.trim().to_owned()));
    }
}

fn parse_fetch(text: &str) -> Option<HttpRequest> {
    // Find fetch( anywhere in the text — handles `const res = await fetch(…)` etc.
    let fetch_pos = text.find("fetch(")?;
    let after = text[fetch_pos + "fetch(".len()..].trim_start();

    let url = extract_js_string_value(after)?;
    if !url.starts_with("http://") && !url.starts_with("https://") {
        return None;
    }

    let headers = extract_fetch_headers(text).unwrap_or_default();
    let method  = extract_fetch_method(text).unwrap_or_else(|| "GET".to_owned());
    let body    = extract_fetch_body(text);
    Some(HttpRequest { url, headers, method: Some(method), body, curl_args: None })
}

// ─── JS string / headers extraction ──────────────────────────────────────────

fn extract_js_string_value(text: &str) -> Option<String> {
    let chars: Vec<char> = text.chars().collect();
    let (s, _) = read_js_token(&chars, 0)?;
    Some(s)
}

/// Returns the slice of `text` that starts immediately after `key:` or `"key":`.
/// Handles both unquoted JS (`headers: {`) and quoted JSON (`"headers": {`) keys.
fn text_after_key<'a>(text: &'a str, key: &str) -> Option<&'a str> {
    // Prefer the quoted form first (more specific, avoids false matches).
    let quoted = format!("\"{key}\":");
    if let Some(pos) = text.find(&quoted) {
        return Some(&text[pos + quoted.len()..]);
    }
    let bare = format!("{key}:");
    text.find(&bare).map(|pos| &text[pos + bare.len()..])
}

fn extract_fetch_headers(text: &str) -> Option<Vec<(String, String)>> {
    let after_colon = text_after_key(text, "headers")?;
    let after = after_colon.trim_start();
    let brace_start = after.find('{')? + 1;
    let inner = &after[brace_start..];

    // Find the matching closing brace
    let mut depth = 1usize;
    let mut end = inner.len();
    for (idx, c) in inner.char_indices() {
        match c {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 { end = idx; break; }
            }
            _ => {}
        }
    }
    let content: Vec<char> = inner[..end].chars().collect();

    let mut headers = Vec::new();
    let mut i = 0;
    loop {
        // Skip whitespace and commas
        while i < content.len() && (content[i].is_whitespace() || content[i] == ',') {
            i += 1;
        }
        if i >= content.len() { break; }

        let (key, next) = match read_js_token(&content, i) {
            Some(t) => t,
            None    => break,
        };
        i = next;

        while i < content.len() && content[i].is_whitespace() { i += 1; }
        if i >= content.len() || content[i] != ':' { break; }
        i += 1;
        while i < content.len() && content[i].is_whitespace() { i += 1; }

        let (val, next) = match read_js_token(&content, i) {
            Some(t) => t,
            None    => break,
        };
        i = next;

        if !key.is_empty() {
            headers.push((key, val));
        }
    }
    Some(headers)
}

fn extract_fetch_method(text: &str) -> Option<String> {
    // Handles  method: "POST"  and  "method": "POST"
    let after = text_after_key(text, "method")?;
    let after = after.trim_start();
    let chars: Vec<char> = after.chars().collect();
    let (method, _) = read_js_token(&chars, 0)?;
    let method = method.trim().to_ascii_uppercase();
    if method.chars().all(|c| c.is_ascii_alphabetic()) && !method.is_empty() {
        Some(method)
    } else {
        None
    }
}

fn extract_fetch_body(text: &str) -> Option<String> {
    let after = text_after_key(text, "body")?;
    extract_js_string_value(after.trim_start())
}

fn read_js_token(chars: &[char], start: usize) -> Option<(String, usize)> {
    let mut i = start;
    if i >= chars.len() { return None; }

    if matches!(chars[i], '\'' | '"' | '`') {
        let quote = chars[i];
        i += 1;
        let mut s = String::new();
        while i < chars.len() {
            if chars[i] == '\\' && i + 1 < chars.len() {
                i += 1;
                s.push(chars[i]);
                i += 1;
            } else if chars[i] == quote {
                return Some((s, i + 1));
            } else {
                s.push(chars[i]);
                i += 1;
            }
        }
        Some((s, i))
    } else {
        let start_i = i;
        while i < chars.len()
            && !chars[i].is_whitespace()
            && chars[i] != ':'
            && chars[i] != ','
            && chars[i] != '}'
        {
            i += 1;
        }
        if i == start_i { return None; }
        Some((chars[start_i..i].iter().collect(), i))
    }
}

// ─── shell argument splitter ──────────────────────────────────────────────────

/// Splits a shell command line into tokens, handling single/double quotes,
/// backslash escapes, and `\<newline>` line continuations.
fn shell_split(s: &str) -> Vec<String> {
    let mut result  = Vec::new();
    let mut current = String::new();
    let mut chars   = s.chars().peekable();

    while let Some(c) = chars.next() {
        match c {
            ' ' | '\t' | '\n' | '\r' => {
                if !current.is_empty() {
                    result.push(std::mem::take(&mut current));
                }
            }
            '\'' => {
                while let Some(c) = chars.next() {
                    if c == '\'' { break; }
                    current.push(c);
                }
            }
            '"' => {
                loop {
                    match chars.next() {
                        None        => break,
                        Some('"')   => break,
                        Some('\\')  => match chars.peek() {
                            Some(&'"') | Some(&'\\') | Some(&'$')
                            | Some(&'`') | Some(&'!') => {
                                current.push(chars.next().unwrap());
                            }
                            Some(&'\n') => { chars.next(); } // continuation inside quotes
                            _ => current.push('\\'),
                        },
                        Some(c) => current.push(c),
                    }
                }
            }
            '\\' => match chars.peek() {
                Some(&'\n') => { chars.next(); } // line continuation
                Some(&'\r') => {
                    chars.next();
                    if chars.peek() == Some(&'\n') { chars.next(); }
                }
                _ => {
                    if let Some(next) = chars.next() {
                        current.push(next);
                    }
                }
            },
            _ => current.push(c),
        }
    }
    if !current.is_empty() {
        result.push(current);
    }
    result
}

// ─── tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_url() {
        let r = parse_request("https://api.example.com/data").unwrap();
        assert_eq!(r.url, "https://api.example.com/data");
        assert!(r.headers.is_empty());
    }

    #[test]
    fn plain_url_rejected_with_spaces() {
        assert!(parse_request("https://api.example.com/data extra").is_none());
    }

    #[test]
    fn curl_simple() {
        let r = parse_request("curl https://api.example.com/data").unwrap();
        assert_eq!(r.url, "https://api.example.com/data");
        assert!(r.headers.is_empty());
    }

    #[test]
    fn curl_with_headers() {
        let input = "curl -H 'Authorization: Bearer token' https://api.example.com/data";
        let r = parse_request(input).unwrap();
        assert_eq!(r.url, "https://api.example.com/data");
        assert_eq!(r.headers.len(), 1);
        assert_eq!(r.headers[0].0, "Authorization");
        assert_eq!(r.headers[0].1, "Bearer token");
    }

    #[test]
    fn curl_multiline_devtools() {
        let input = "curl 'https://api.example.com/users' \\\n  \
                     -H 'accept: application/json' \\\n  \
                     -H 'authorization: Bearer tok' \\\n  \
                     --compressed";
        let r = parse_request(input).unwrap();
        assert_eq!(r.url, "https://api.example.com/users");
        assert_eq!(r.headers.len(), 2);
        assert_eq!(r.headers[0].0, "accept");
        assert_eq!(r.headers[1].0, "authorization");
    }

    #[test]
    fn fetch_simple() {
        let r = parse_request("fetch('https://api.example.com/data')").unwrap();
        assert_eq!(r.url, "https://api.example.com/data");
    }

    #[test]
    fn fetch_with_await() {
        let r = parse_request("await fetch('https://api.example.com/data')").unwrap();
        assert_eq!(r.url, "https://api.example.com/data");
    }

    #[test]
    fn fetch_with_headers() {
        let input = r#"fetch('https://api.example.com/data', {
    headers: {
        'Authorization': 'Bearer token',
        'Content-Type': 'application/json'
    }
})"#;
        let r = parse_request(input).unwrap();
        assert_eq!(r.url, "https://api.example.com/data");
        assert_eq!(r.headers.len(), 2);
        assert_eq!(r.headers[0].0, "Authorization");
        assert_eq!(r.headers[0].1, "Bearer token");
    }

    #[test]
    fn fetch_quoted_json_keys() {
        let input = r#"fetch("http://localhost:3000/api/v1/users/auth/validate_code", {
  "headers": {
    "client-type": "web",
    "content-type": "application/json"
  },
  "method": "POST"
});"#;
        let r = parse_request(input).unwrap();
        assert_eq!(r.url, "http://localhost:3000/api/v1/users/auth/validate_code");
        assert_eq!(r.method.as_deref(), Some("POST"));
        assert_eq!(r.headers.len(), 2);
        assert_eq!(r.headers[0].0, "client-type");
        assert_eq!(r.headers[0].1, "web");
        assert_eq!(r.headers[1].0, "content-type");
        assert_eq!(r.headers[1].1, "application/json");
    }

    #[test]
    fn url_display_name_strips_scheme_and_query() {
        assert_eq!(
            url_display_name("https://api.example.com/data?key=val"),
            "api.example.com/data"
        );
    }
}
