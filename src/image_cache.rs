use std::{
    collections::{hash_map::DefaultHasher, HashMap, HashSet},
    fs,
    hash::{Hash, Hasher},
    path::{Path, PathBuf},
    thread,
    time::{Duration, Instant, SystemTime},
};

use crossbeam_channel::{unbounded, Receiver, Sender};
use directories::ProjectDirs;
use eframe::egui::{self, TextureHandle, TextureOptions};

const THUMBNAIL_EDGE_PX: u32 = 360;
const MAX_DECODE_EDGE_PX: u32 = 512;
const MEMORY_ENTRY_LIMIT: usize = 96;
const MAX_PENDING_REQUESTS: usize = 48;
const DISK_CACHE_LIMIT_BYTES: u64 = 256 * 1024 * 1024;
const DOWNLOAD_WORKERS: usize = 3;
const FAILURE_RETRY_AFTER: Duration = Duration::from_secs(60);

#[derive(Debug, Clone)]
struct ImageRequest {
    key: String,
    fetch_url: String,
    cache_path: PathBuf,
}

#[derive(Debug)]
struct ImageReady {
    key: String,
    result: Result<DecodedImage, String>,
}

#[derive(Debug)]
struct DecodedImage {
    width: usize,
    height: usize,
    rgba: Vec<u8>,
}

struct MemoryEntry {
    texture: TextureHandle,
    last_used: u64,
}

pub struct ImageCache {
    request_tx: Sender<ImageRequest>,
    ready_rx: Receiver<ImageReady>,
    cache_dir: Option<PathBuf>,
    memory: HashMap<String, MemoryEntry>,
    pending: HashSet<String>,
    failures: HashMap<String, Instant>,
    use_clock: u64,
}

impl ImageCache {
    pub fn new() -> Self {
        let cache_dir = cache_dir();
        let (request_tx, request_rx) = unbounded::<ImageRequest>();
        let (ready_tx, ready_rx) = unbounded::<ImageReady>();

        if let Some(dir) = cache_dir.clone() {
            let _ = fs::create_dir_all(&dir);
            let prune_dir = dir.clone();
            thread::Builder::new()
                .name("baby-image-cache-prune".into())
                .spawn(move || prune_disk_cache(&prune_dir, DISK_CACHE_LIMIT_BYTES))
                .ok();
        }

        for worker_index in 0..DOWNLOAD_WORKERS {
            let rx = request_rx.clone();
            let tx = ready_tx.clone();
            thread::Builder::new()
                .name(format!("baby-image-{worker_index}"))
                .spawn(move || image_worker(rx, tx))
                .expect("failed to spawn image cache worker");
        }

        Self {
            request_tx,
            ready_rx,
            cache_dir,
            memory: HashMap::new(),
            pending: HashSet::new(),
            failures: HashMap::new(),
            use_clock: 0,
        }
    }

    /// Poll completed background loads and upload decoded pixels to egui/GPU textures.
    pub fn process_ready(&mut self, ctx: &egui::Context) {
        while let Ok(ready) = self.ready_rx.try_recv() {
            self.pending.remove(&ready.key);
            match ready.result {
                Ok(decoded) => {
                    let color_image = egui::ColorImage::from_rgba_unmultiplied(
                        [decoded.width, decoded.height],
                        &decoded.rgba,
                    );
                    let texture = ctx.load_texture(
                        format!("baby-product:{}", ready.key),
                        color_image,
                        TextureOptions::LINEAR,
                    );
                    self.use_clock = self.use_clock.wrapping_add(1);
                    self.memory.insert(
                        ready.key.clone(),
                        MemoryEntry {
                            texture,
                            last_used: self.use_clock,
                        },
                    );
                    self.failures.remove(&ready.key);
                    self.evict_memory_if_needed();
                    ctx.request_repaint();
                }
                Err(_) => {
                    // Do not hammer a failed/missing CDN URL every frame.
                    self.failures.insert(ready.key, Instant::now());
                }
            }
        }
    }

    /// Return a texture if it is already in memory. Otherwise enqueue a lazy load and return None.
    pub fn texture(&mut self, image_url: &str) -> Option<TextureHandle> {
        let canonical = normalize_image_url(image_url)?;
        let key = cache_key(&canonical);

        if let Some(entry) = self.memory.get_mut(&key) {
            self.use_clock = self.use_clock.wrapping_add(1);
            entry.last_used = self.use_clock;
            return Some(entry.texture.clone());
        }

        if self.pending.contains(&key) {
            return None;
        }
        if self.pending.len() >= MAX_PENDING_REQUESTS {
            return None;
        }

        if let Some(failed_at) = self.failures.get(&key) {
            if failed_at.elapsed() < FAILURE_RETRY_AFTER {
                return None;
            }
        }

        let cache_path = self
            .cache_dir
            .as_ref()
            .map(|dir| dir.join(format!("{key}.img")))
            .unwrap_or_default();
        let fetch_url = thumbnail_url(&canonical, THUMBNAIL_EDGE_PX);
        let request = ImageRequest {
            key: key.clone(),
            fetch_url,
            cache_path,
        };

        if self.request_tx.send(request).is_ok() {
            self.pending.insert(key);
        }
        None
    }

    fn evict_memory_if_needed(&mut self) {
        while self.memory.len() > MEMORY_ENTRY_LIMIT {
            let Some(oldest_key) = self
                .memory
                .iter()
                .min_by_key(|(_, entry)| entry.last_used)
                .map(|(key, _)| key.clone())
            else {
                break;
            };
            self.memory.remove(&oldest_key);
        }
    }
}

fn image_worker(request_rx: Receiver<ImageRequest>, ready_tx: Sender<ImageReady>) {
    let client = reqwest::blocking::Client::builder()
        .user_agent("BABY-Watch/0.3 image-cache")
        .timeout(Duration::from_secs(15))
        .build()
        .expect("failed to build image HTTP client");

    let mut processed = 0usize;
    while let Ok(request) = request_rx.recv() {
        let cache_parent = request.cache_path.parent().map(Path::to_path_buf);
        let result = load_or_download(&client, &request);
        let _ = ready_tx.send(ImageReady {
            key: request.key,
            result,
        });

        processed = processed.wrapping_add(1);
        if processed.is_multiple_of(64) {
            if let Some(dir) = cache_parent {
                prune_disk_cache(&dir, DISK_CACHE_LIMIT_BYTES);
            }
        }
    }
}

fn load_or_download(
    client: &reqwest::blocking::Client,
    request: &ImageRequest,
) -> Result<DecodedImage, String> {
    if !request.cache_path.as_os_str().is_empty() {
        if let Ok(bytes) = fs::read(&request.cache_path) {
            if let Ok(decoded) = decode(&bytes) {
                // Touching on a disk hit makes mtime a useful approximation of LRU.
                let _ = touch_file(&request.cache_path, &bytes);
                return Ok(decoded);
            }
            let _ = fs::remove_file(&request.cache_path);
        }
    }

    let response = client
        .get(&request.fetch_url)
        .header(
            reqwest::header::ACCEPT,
            "image/webp,image/png,image/jpeg,image/*;q=0.8",
        )
        .send()
        .map_err(|e| format!("image request failed: {e}"))?;
    if !response.status().is_success() {
        return Err(format!("image HTTP {}", response.status()));
    }
    let bytes = response
        .bytes()
        .map_err(|e| format!("image body failed: {e}"))?
        .to_vec();

    let decoded = decode(&bytes)?;
    if !request.cache_path.as_os_str().is_empty() {
        let _ = atomic_write(&request.cache_path, &bytes);
    }
    Ok(decoded)
}

fn decode(bytes: &[u8]) -> Result<DecodedImage, String> {
    let mut image =
        image::load_from_memory(bytes).map_err(|e| format!("image decode failed: {e}"))?;
    if image.width() > MAX_DECODE_EDGE_PX || image.height() > MAX_DECODE_EDGE_PX {
        image = image.thumbnail(MAX_DECODE_EDGE_PX, MAX_DECODE_EDGE_PX);
    }
    let rgba = image.to_rgba8();
    Ok(DecodedImage {
        width: rgba.width() as usize,
        height: rgba.height() as usize,
        rgba: rgba.into_raw(),
    })
}

fn normalize_image_url(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    if trimmed.starts_with("//") {
        Some(format!("https:{trimmed}"))
    } else if trimmed.starts_with("http://") || trimmed.starts_with("https://") {
        Some(trimmed.to_owned())
    } else {
        None
    }
}

fn thumbnail_url(original: &str, width: u32) -> String {
    let Ok(mut url) = url::Url::parse(original) else {
        return original.to_owned();
    };
    // Shopify CDN understands ?width=. Keep the version query supplied by Shopify so a
    // changed image naturally gets a different cache key and does not need explicit invalidation.
    if url
        .host_str()
        .map(|host| host.contains("shopify") || host.contains("cdn"))
        .unwrap_or(false)
    {
        url.query_pairs_mut()
            .append_pair("width", &width.to_string());
    }
    url.to_string()
}

fn cache_key(canonical_url: &str) -> String {
    let mut hasher = DefaultHasher::new();
    canonical_url.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

fn cache_dir() -> Option<PathBuf> {
    let dirs = ProjectDirs::from("com", "baby-watch", "BABY Watch")?;
    Some(dirs.cache_dir().join("images-v1"))
}

fn atomic_write(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("tmp");
    fs::write(&tmp, bytes)?;
    if path.exists() {
        let _ = fs::remove_file(path);
    }
    match fs::rename(&tmp, path) {
        Ok(()) => Ok(()),
        Err(err) => {
            let _ = fs::remove_file(&tmp);
            Err(err)
        }
    }
}

fn touch_file(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    // std does not expose a portable utimens API. Rewriting the same small thumbnail is a
    // portable way to refresh mtime; this happens only on disk-cache misses in memory.
    atomic_write(path, bytes)
}

fn prune_disk_cache(dir: &Path, limit_bytes: u64) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };

    let mut files = Vec::new();
    let mut total = 0u64;
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(meta) = entry.metadata() else {
            continue;
        };
        if !meta.is_file() || path.extension().and_then(|x| x.to_str()) != Some("img") {
            continue;
        }
        let size = meta.len();
        total = total.saturating_add(size);
        let modified = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
        files.push((modified, size, path));
    }

    if total <= limit_bytes {
        return;
    }

    files.sort_by_key(|(modified, _, _)| *modified);
    for (_, size, path) in files {
        if total <= limit_bytes {
            break;
        }
        if fs::remove_file(path).is_ok() {
            total = total.saturating_sub(size);
        }
    }
}
