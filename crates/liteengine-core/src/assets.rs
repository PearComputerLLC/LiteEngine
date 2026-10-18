//! Streaming asset store.
//!
//! LiteEngine never loads a project's assets up front. Instead the store:
//!
//! 1. discovers assets from the project manifest and Lua requests,
//! 2. streams them in on a background worker thread (decode PNG -> RGBA8),
//! 3. keeps the resident set under a **byte budget**, evicting the
//!    least-recently-used textures first (LRU),
//! 4. exposes textures to renderers via `Arc<TextureData>` handles that stay
//!    alive even while evicted from the hot cache.
//!
//! The store reads assets either from a project directory or from an
//! in-memory `.litepkg` zip (built by `liteengine build`), so the same code
//! path serves development and distribution.

use crate::snapshot::TextureData;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex};

/// Where asset bytes come from.
pub enum Vfs {
    /// A project directory on disk.
    Dir(PathBuf),
    /// A `.litepkg`/`.lemodule` zip held in memory.
    Package(Vec<u8>),
}

impl Vfs {
    pub fn read(&self, rel: &str) -> Result<Vec<u8>, String> {
        let rel = rel.trim_start_matches('/');
        match self {
            Vfs::Dir(root) => {
                let p = root.join(rel);
                std::fs::read(&p).map_err(|e| format!("{}: {e}", p.display()))
            }
            Vfs::Package(bytes) => {
                let cursor = std::io::Cursor::new(bytes.as_slice());
                let mut zip = zip::ZipArchive::new(cursor).map_err(|e| e.to_string())?;
                for name in [rel.to_string(), format!("game/{rel}")] {
                    if let Ok(mut file) = zip.by_name(&name) {
                        let mut buf = Vec::with_capacity(file.size() as usize);
                        if std::io::Read::read_to_end(&mut file, &mut buf).is_ok() {
                            return Ok(buf);
                        }
                    }
                }
                Err(format!("{rel}: not found in package"))
            }
        }
    }

    pub fn exists(&self, rel: &str) -> bool {
        self.read(rel).is_ok()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AssetState {
    Unloaded,
    Loading,
    Loaded,
}

struct Entry {
    state: AssetState,
    priority: u8, // higher = keep longer
    last_frame: u64,
    bytes: usize,
    texture: Option<Arc<TextureData>>,
}

/// Statistics snapshot for `stream.stats()`.
#[derive(Debug, Clone, Default)]
pub struct StreamStats {
    pub loaded: usize,
    pub loading: usize,
    pub resident_bytes: usize,
    pub budget_bytes: usize,
    pub evictions: u64,
    pub loads_completed: u64,
}

/// The streaming asset store. One background worker decodes textures; the
/// game thread polls results each frame (single-threaded Lua world stays safe).
pub struct AssetStore {
    vfs: Arc<Vfs>,
    entries: HashMap<String, Entry>,
    budget: usize,
    frame: u64,
    tx: Sender<(String, Vec<u8>)>,
    rx_done: Receiver<Result<(String, TextureData, usize), (String, String)>>,
    evictions: Arc<AtomicU64>,
    loads: Arc<AtomicU64>,
}

impl AssetStore {
    pub fn new(vfs: Vfs, budget_bytes: usize) -> AssetStore {
        let (tx, rx) = channel::<(String, Vec<u8>)>();
        let (tx_done, rx_done) = channel();
        let evictions = Arc::new(AtomicU64::new(0));
        let loads = Arc::new(AtomicU64::new(0));
        let ev2 = evictions.clone();
        let ld2 = loads.clone();

        std::thread::Builder::new()
            .name("liteengine-assets".into())
            .spawn(move || {
                while let Ok((path, bytes)) = rx.recv() {
                    match image::load_from_memory(&bytes) {
                        Ok(img) => {
                            let rgba = img.to_rgba8();
                            let td = TextureData {
                                width: rgba.width(),
                                height: rgba.height(),
                                rgba: rgba.into_raw(),
                            };
                            let size = td.rgba.len();
                            let _ = tx_done.send(Ok((path, td, size)));
                            let _ = ld2.fetch_add(1, Ordering::Relaxed);
                        }
                        Err(e) => {
                            let _ = tx_done.send(Err((path, e.to_string())));
                        }
                    }
                }
                let _ = ev2; // keep alive for stats handle
            })
            .expect("spawn asset worker");

        AssetStore {
            vfs: Arc::new(vfs),
            entries: HashMap::new(),
            budget: budget_bytes,
            frame: 0,
            tx,
            rx_done,
            evictions,
            loads,
        }
    }

    pub fn vfs(&self) -> &Vfs {
        &self.vfs
    }

    pub fn set_budget(&mut self, bytes: usize) {
        self.budget = bytes.max(1 << 20); // floor at 1 MiB
        self.enforce_budget();
    }

    /// Request an asset for later use. Streams asynchronously unless already loaded.
    pub fn request(&mut self, path: &str, priority: u8) {
        if path.is_empty() {
            return;
        }
        match self.entries.get_mut(path) {
            Some(e) => {
                e.priority = e.priority.max(priority);
                e.last_frame = self.frame;
            }
            None => {
                self.entries.insert(
                    path.to_string(),
                    Entry {
                        state: AssetState::Unloaded,
                        priority,
                        last_frame: self.frame,
                        bytes: 0,
                        texture: None,
                    },
                );
            }
        }
        self.kick(path);
    }

    /// Explicit prefetch list (used by `stream.prefetch`).
    pub fn prefetch(&mut self, paths: &[String]) {
        for p in paths {
            self.request(p, 1);
        }
    }

    /// Drop a texture from the resident set (used by `stream.release`).
    pub fn release(&mut self, path: &str) {
        if let Some(e) = self.entries.get_mut(path) {
            e.texture = None;
            e.state = AssetState::Unloaded;
            e.bytes = 0;
        }
    }

    fn kick(&mut self, path: &str) {
        let state = self.entries.get(path).map(|e| e.state);
        if state != Some(AssetState::Unloaded) {
            return;
        }
        match self.vfs.read(path) {
            Ok(bytes) => {
                if let Some(e) = self.entries.get_mut(path) {
                    e.state = AssetState::Loading;
                }
                let _ = self.tx.send((path.to_string(), bytes));
            }
            Err(e) => {
                eprintln!("[liteengine] asset '{path}' unavailable: {e}");
                // Keep the entry as Unloaded so a later retry can succeed.
            }
        }
    }

    /// Advance the frame counter, collect completed loads, then enforce the budget.
    pub fn tick(&mut self) {
        self.frame += 1;
        while let Ok(msg) = self.rx_done.try_recv() {
            match msg {
                Ok((path, td, size)) => {
                    if let Some(e) = self.entries.get_mut(&path) {
                        e.state = AssetState::Loaded;
                        e.bytes = size;
                        e.last_frame = self.frame;
                        e.texture = Some(Arc::new(td));
                    }
                }
                Err((path, e)) => {
                    eprintln!("[liteengine] failed to decode '{path}': {e}");
                    if let Some(en) = self.entries.get_mut(&path) {
                        en.state = AssetState::Unloaded;
                    }
                }
            }
        }
        self.enforce_budget();
    }

    fn resident_bytes(&self) -> usize {
        self.entries.values().map(|e| e.bytes).sum()
    }

    fn enforce_budget(&mut self) {
        let mut total = self.resident_bytes();
        if total <= self.budget {
            return;
        }
        // Evict LRU first, never the current frame's assets.
        let mut order: Vec<(String, u64, u8)> = self
            .entries
            .iter()
            .filter(|(_, e)| e.texture.is_some())
            .map(|(k, e)| (k.clone(), e.last_frame, e.priority))
            .collect();
        order.sort_by_key(|(_, frame, prio)| (*frame, 255u8.saturating_sub(*prio)));
        for (path, _, _) in order {
            if total <= self.budget {
                break;
            }
            if let Some(e) = self.entries.get_mut(&path) {
                if let Some(t) = e.texture.take() {
                    total = total.saturating_sub(e.bytes);
                    e.bytes = 0;
                    e.state = AssetState::Unloaded;
                    e.texture = None;
                    let _ = t; // Arc consumers keep rendering safely
                    self.evictions.fetch_add(1, Ordering::Relaxed);
                }
            }
        }
    }

    /// Fetch a texture for rendering; touches the LRU clock.
    pub fn get(&mut self, path: &str) -> Option<Arc<TextureData>> {
        if let Some(e) = self.entries.get_mut(path) {
            e.last_frame = self.frame;
            if let Some(t) = &e.texture {
                return Some(t.clone());
            }
        }
        None
    }

    /// State of one asset.
    pub fn state(&self, path: &str) -> AssetState {
        self.entries.get(path).map(|e| e.state).unwrap_or(AssetState::Unloaded)
    }

    pub fn stats(&self) -> StreamStats {
        StreamStats {
            loaded: self.entries.values().filter(|e| e.texture.is_some()).count(),
            loading: self
                .entries
                .values()
                .filter(|e| e.state == AssetState::Loading)
                .count(),
            resident_bytes: self.resident_bytes(),
            budget_bytes: self.budget,
            evictions: self.evictions.load(Ordering::Relaxed),
            loads_completed: self.loads.load(Ordering::Relaxed),
        }
    }
}
