use std::path::PathBuf;
use std::sync::{mpsc, Arc};

use crate::index::{JsonData, JsonIndex};
use crate::parser::parse_bytes;

/// Surrounding bytes extracted at the parse-error position, ready for display.
#[derive(Clone)]
pub struct ErrorContext {
    pub before: String,
    pub at:     String,
    pub after:  String,
}

pub enum LoadMsg {
    Progress(f32),
    Done(Arc<JsonIndex>),
    Error(String, Option<ErrorContext>),
}

/// Spawns a background thread that mmaps + parses the file.
/// Returns a Receiver the UI polls each frame.
pub fn spawn_load(path: PathBuf) -> mpsc::Receiver<LoadMsg> {
    spawn_build(move || {
        let file = std::fs::File::open(&path)
            .map_err(|e| format!("open: {e}"))?;
        let mmap = unsafe { memmap2::Mmap::map(&file) }
            .map_err(|e| format!("mmap: {e}"))?;
        // The parser reads the file front-to-back once; tell the kernel to
        // read ahead aggressively. Best-effort — ignore failures.
        let _ = mmap.advise(memmap2::Advice::Sequential);
        Ok(JsonData::Mapped { _file: file, mmap })
    })
}

/// Spawns a background thread that parses an in-memory buffer (pasted text).
pub fn spawn_parse(data: Vec<u8>) -> mpsc::Receiver<LoadMsg> {
    spawn_build(move || Ok(JsonData::Memory(data)))
}

/// Spawns a background thread that runs the `curl` binary with the given
/// shell-split args and parses its stdout as JSON.
/// `-o -` (stdout) and `--no-progress-meter` are appended automatically.
pub fn spawn_exec_curl(curl_args: Vec<String>) -> mpsc::Receiver<LoadMsg> {
    spawn_build(move || {
        let output = std::process::Command::new("curl")
            .args(&curl_args)
            .arg("--no-progress-meter")
            .arg("-o")
            .arg("-") // write body to stdout
            .output()
            .map_err(|e| format!("failed to run curl: {e}"))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
            return Err(if stderr.is_empty() {
                format!("curl exited with code {}", output.status)
            } else {
                format!("curl: {stderr}")
            });
        }

        Ok(JsonData::Memory(output.stdout))
    })
}

/// Spawns a background thread that fetches a URL and parses the response as JSON.
pub fn spawn_fetch_url(
    url: String,
    method: Option<String>,
    headers: Vec<(String, String)>,
    body: Option<String>,
) -> mpsc::Receiver<LoadMsg> {
    spawn_build(move || {
        let method = match method.as_deref().unwrap_or("GET").to_ascii_uppercase().as_str() {
            "POST"   => ureq::http::Method::POST,
            "PUT"    => ureq::http::Method::PUT,
            "DELETE" => ureq::http::Method::DELETE,
            "PATCH"  => ureq::http::Method::PATCH,
            _        => ureq::http::Method::GET,
        };
        let mut req = ureq::http::Request::builder()
            .method(method)
            .uri(&url)
            .header("User-Agent", "quick-json-viewer")
            .header("Accept", "application/json, text/plain, */*");
        for (k, v) in &headers {
            req = req.header(k.as_str(), v.as_str());
        }

        let agent: ureq::Agent = ureq::Agent::config_builder()
            .timeout_global(Some(std::time::Duration::from_secs(30)))
            .build()
            .into();
        // `Agent::run` sends a body with any method (curl allows e.g. DELETE -d).
        let resp = if let Some(body) = body {
            req.body(body).map_err(|e| format!("bad request: {e}")).and_then(|r| {
                agent.run(r).map_err(|e| format!("HTTP error: {e}"))
            })?
        } else {
            req.body(()).map_err(|e| format!("bad request: {e}")).and_then(|r| {
                agent.run(r).map_err(|e| format!("HTTP error: {e}"))
            })?
        };

        let data = resp
            .into_body()
            .with_config()
            .limit(50 * 1024 * 1024) // 50 MB limit
            .read_to_vec()
            .map_err(|e| format!("reading response: {e}"))?;

        Ok(JsonData::Memory(data))
    })
}

fn bytes_to_display(bytes: &[u8]) -> String {
    bytes.iter().map(|&b| match b {
        b'\n' => "\n".to_string(),
        b'\r' => String::new(),
        b'\t' => "\t".to_string(),
        b if b.is_ascii_graphic() || b == b' ' => (b as char).to_string(),
        b => format!("\\x{b:02X}"),
    }).collect()
}

fn extract_error_context(bytes: &[u8], offset: usize) -> ErrorContext {
    const WINDOW: usize = 200;
    let start = offset.saturating_sub(WINDOW);
    let end = (offset + WINDOW + 1).min(bytes.len());
    let before = bytes_to_display(&bytes[start..offset]);
    let at = if offset < bytes.len() {
        bytes_to_display(&bytes[offset..offset + 1])
    } else {
        "‹end of stream› ".to_string()
    };
    let after_start = (offset + 1).min(bytes.len());
    let after = bytes_to_display(&bytes[after_start..end]);
    ErrorContext { before, at, after }
}

fn spawn_build<F>(make_data: F) -> mpsc::Receiver<LoadMsg>
where
    F: FnOnce() -> Result<JsonData, String> + Send + 'static,
{
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let tx2 = tx.clone();
        if let Err(e) = build_inner(make_data, tx) {
            let _ = tx2.send(LoadMsg::Error(e, None));
        }
    });
    rx
}

fn build_inner<F>(make_data: F, tx: mpsc::Sender<LoadMsg>) -> Result<(), String>
where
    F: FnOnce() -> Result<JsonData, String>,
{
    let data = make_data()?;

    let tx_prog = tx.clone();
    let mut progress_cb = |p: f32| {
        let _ = tx_prog.send(LoadMsg::Progress(p));
    };

    let (nodes, root, is_ndjson) = match parse_bytes(data.bytes(), &mut progress_cb) {
        Ok(r) => r,
        Err(e) => {
            let ctx = extract_error_context(data.bytes(), e.offset as usize);
            let _ = tx.send(LoadMsg::Error(e.to_string(), Some(ctx)));
            return Ok(());
        }
    };

    let index = Arc::new(JsonIndex {
        data,
        nodes,
        root,
        is_ndjson,
    });

    let _ = tx.send(LoadMsg::Done(index));
    Ok(())
}
