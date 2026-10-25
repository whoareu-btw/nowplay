//! Album art. `file://` art is used in place; http(s) art is downloaded once
//! into `~/.cache/nowplay` (small, self-pruning) so reopening is instant.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use crate::model::{cache_key, file_uri_to_path};

const MAX_BYTES: u64 = 8 * 1024 * 1024;
const KEEP_FILES: usize = 200;

pub fn cache_dir() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))?;
    Some(base.join("nowplay"))
}

/// The art if it is already on disk (a local file or a cached download).
/// Never touches the network, so it is safe on the UI thread.
pub fn local_path(url: &str) -> Option<PathBuf> {
    if let Some(p) = file_uri_to_path(url) {
        let p = PathBuf::from(p);
        return p.exists().then_some(p);
    }
    let p = cache_dir()?.join(cache_key(url));
    p.exists().then_some(p)
}

/// Download into the cache. Blocking: call from a worker thread.
pub fn fetch(url: &str) -> Option<PathBuf> {
    if let Some(p) = local_path(url) {
        return Some(p);
    }
    if !(url.starts_with("http://") || url.starts_with("https://")) {
        return None;
    }
    let dir = cache_dir()?;
    std::fs::create_dir_all(&dir).ok()?;

    let resp = ureq::get(url)
        .set("User-Agent", "nowplay/0.1")
        .timeout(Duration::from_secs(8))
        .call()
        .ok()?;
    let mut buf = Vec::new();
    resp.into_reader().take(MAX_BYTES).read_to_end(&mut buf).ok()?;
    if buf.is_empty() {
        return None;
    }

    let name = cache_key(url);
    let tmp = dir.join(format!("{name}.tmp"));
    let path = dir.join(&name);
    std::fs::write(&tmp, &buf).ok()?;
    std::fs::rename(&tmp, &path).ok()?;
    prune(&dir, KEEP_FILES);
    Some(path)
}

/// Keep the cache from growing forever: drop the oldest half when over `keep`.
fn prune(dir: &Path, keep: usize) {
    let rd = match std::fs::read_dir(dir) {
        Ok(rd) => rd,
        Err(_) => return,
    };
    let mut files: Vec<(SystemTime, PathBuf)> = rd
        .filter_map(|e| e.ok())
        .filter_map(|e| {
            let t = e.metadata().ok()?.modified().ok()?;
            Some((t, e.path()))
        })
        .collect();
    if files.len() <= keep {
        return;
    }
    files.sort_by_key(|(t, _)| *t);
    let drop_n = files.len() - keep / 2;
    for (_, p) in files.into_iter().take(drop_n) {
        let _ = std::fs::remove_file(p);
    }
}
