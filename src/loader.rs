use std::path::PathBuf;
use std::sync::{mpsc, Arc};

use crate::index::JsonIndex;
use crate::parser::parse_bytes;

pub enum LoadMsg {
    Progress(f32),
    Done(Arc<JsonIndex>),
    Error(String),
}

/// Spawns a background thread that mmaps + parses the file.
/// Returns a Receiver the UI polls each frame.
pub fn spawn_load(path: PathBuf) -> mpsc::Receiver<LoadMsg> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let tx2 = tx.clone();
        if let Err(e) = load_inner(path, tx) {
            let _ = tx2.send(LoadMsg::Error(e));
        }
    });
    rx
}

fn load_inner(path: PathBuf, tx: mpsc::Sender<LoadMsg>) -> Result<(), String> {
    let file = std::fs::File::open(&path)
        .map_err(|e| format!("open: {e}"))?;
    let mmap = unsafe { memmap2::Mmap::map(&file) }
        .map_err(|e| format!("mmap: {e}"))?;

    let mut key_arena: Vec<u8> = Vec::new();
    let tx_prog = tx.clone();
    let mut progress_cb = |p: f32| {
        let _ = tx_prog.send(LoadMsg::Progress(p));
    };

    let (nodes, root, is_ndjson) =
        parse_bytes(&mmap[..], &mut key_arena, &mut progress_cb)
            .map_err(|e| e.to_string())?;

    let index = Arc::new(JsonIndex {
        _file: file,
        mmap,
        nodes,
        key_arena,
        root,
        is_ndjson,
    });

    let _ = tx.send(LoadMsg::Done(index));
    Ok(())
}
