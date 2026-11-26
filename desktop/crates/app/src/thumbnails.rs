use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::io::{Cursor, Read};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Condvar, Mutex, OnceLock, RwLock};
use std::time::{Duration, UNIX_EPOCH};

const DEFAULT_CACHE_LIMIT_MB: usize = 256;
const MAX_IMAGE_BYTES: usize = 10 * 1024 * 1024;

/// Width the card grid asks for. Cards are a few hundred logical pixels wide,
/// so decoding full-resolution artwork for them only wastes CPU and RAM.
pub const CARD_ARTWORK_WIDTH: u32 = 480;
/// Width the details screen asks for: its poster fills a large panel.
pub const DETAIL_ARTWORK_WIDTH: u32 = 1280;
/// Persistent cache entries are normalised to this width.
const CACHE_ARTWORK_WIDTH: u32 = 1280;
const CACHE_JPEG_QUALITY: u8 = 82;
const MEMORY_CACHE_LIMIT_BYTES: usize = 96 * 1024 * 1024;
const MEMORY_CACHE_LIMIT_BYTES_LOW_RAM: usize = 32 * 1024 * 1024;
const MAX_CONCURRENT_IMAGE_DOWNLOADS: usize = 6;

static CACHE_LIMIT_MB: AtomicUsize = AtomicUsize::new(DEFAULT_CACHE_LIMIT_MB);
static LOW_RAM_MODE: AtomicBool = AtomicBool::new(false);
static CACHE_WRITE_LOCK: Mutex<()> = Mutex::new(());
static RESOLVER_BASE_URL: OnceLock<RwLock<String>> = OnceLock::new();
static RESOLVER_AUTH_TOKEN: OnceLock<RwLock<Option<String>>> = OnceLock::new();

/// Configure the persistent image cache. A zero limit keeps images in memory
/// for the current view and does not retain them on disk.
pub fn set_cache_limit_mb(limit_mb: usize) {
    let Ok(_guard) = CACHE_WRITE_LOCK.lock() else {
        return;
    };
    CACHE_LIMIT_MB.store(limit_mb, Ordering::Relaxed);
    if limit_mb == 0 {
        clear_cache_files();
    } else {
        prune_cache(limit_mb.saturating_mul(1024 * 1024), None);
    }
}

pub fn set_cache_limit_mb_async(limit_mb: usize) {
    std::thread::spawn(move || {
        set_cache_limit_mb(limit_mb);
    });
}

pub fn current_cache_size_bytes() -> u64 {
    let dir = cache_dir_path();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    entries
        .flatten()
        .filter_map(|e| e.metadata().ok())
        .filter(|m| m.is_file())
        .map(|m| m.len())
        .sum()
}

pub fn format_cache_bytes(bytes: u64) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = KB * 1024.0;
    const GB: f64 = MB * 1024.0;
    let b = bytes as f64;
    if b >= GB {
        format!("{:.2} GB", b / GB)
    } else if b >= MB {
        format!("{:.1} MB", b / MB)
    } else if b >= KB {
        format!("{:.0} KB", b / KB)
    } else {
        format!("{bytes} B")
    }
}

/// Render a configured cache limit exactly as it is stored.
///
/// The label used to integer-divide by 1024, so a custom 1536 MB limit read
/// back as "1 GB" after a restart and looked like the value had been lost even
/// though the setting was persisted correctly.
pub fn format_cache_limit(megabytes: i32) -> String {
    if megabytes <= 0 {
        return "Off (RAM only)".to_string();
    }
    if megabytes < 1024 {
        return format!("{megabytes} MB");
    }
    let gigabytes = megabytes as f64 / 1024.0;
    if (gigabytes - gigabytes.round()).abs() < 0.05 {
        format!("{} GB", gigabytes.round() as i32)
    } else {
        format!("{gigabytes:.1} GB")
    }
}

/// Remove every persisted thumbnail while preserving the configured cache limit.
pub fn clear_cache() {
    let Ok(_guard) = CACHE_WRITE_LOCK.lock() else {
        return;
    };
    clear_cache_files();
}

/// Remove the exact image cache entries used for a card before an explicit
/// retry. The server may serve a tagged image URL and an untagged fallback.
pub fn invalidate_cached_image(url: &str) {
    let Ok(_guard) = CACHE_WRITE_LOCK.lock() else {
        return;
    };
    for variant in cache_url_variants(url) {
        let path = url_to_cache_path_without_create(&variant);
        let _ = std::fs::remove_file(path);
    }
}

fn cache_url_variants(url: &str) -> Vec<String> {
    let url = url.trim();
    if url.is_empty() {
        return Vec::new();
    }
    let mut variants = vec![url.to_owned()];
    if let Ok(mut parsed) = url::Url::parse(url) {
        let original_query_count = parsed.query_pairs().count();
        let params = parsed
            .query_pairs()
            .filter(|(key, _)| key != "tag")
            .map(|(key, value)| (key.into_owned(), value.into_owned()))
            .collect::<Vec<_>>();
        if params.len() != original_query_count {
            parsed.set_query(None);
            if !params.is_empty() {
                parsed.query_pairs_mut().extend_pairs(params);
            }
            variants.push(parsed.to_string());
        }
    }
    variants
}

pub fn set_low_ram_mode(enabled: bool) {
    LOW_RAM_MODE.store(enabled, Ordering::Relaxed);
}

pub fn set_resolver_base_url(base_url: &str) {
    if let Ok(mut current) = resolver_base_url().write() {
        *current = base_url.trim().trim_end_matches('/').to_string();
    }
}

fn resolver_base_url() -> &'static RwLock<String> {
    RESOLVER_BASE_URL.get_or_init(|| RwLock::new(String::new()))
}

/// Keep the active Adult Hub token available for image endpoints that use the
/// same authentication contract as Resolver pHash operations. The request layer
/// only sends this token to the configured Resolver origin.
pub fn set_resolver_auth_token(token: Option<&str>) {
    if let Ok(mut current) = resolver_auth_token().write() {
        *current = token.filter(|value| !value.is_empty()).map(str::to_owned);
    }
}

fn resolver_auth_token() -> &'static RwLock<Option<String>> {
    RESOLVER_AUTH_TOKEN.get_or_init(|| RwLock::new(None))
}

pub fn phash_thumbnail_url(video_id: &str) -> Option<String> {
    let base = resolver_base_url().read().ok()?.clone();
    let video_id = video_id.trim();
    if base.is_empty() || !video_id.starts_with("telegram:") {
        return None;
    }
    // Canonical Telegram refs are `telegram:<channel>:<message>:<file>`.
    // Legacy resume rows used `telegram:<hash>` and have no resolver artwork.
    if video_id
        .trim_start_matches("telegram:")
        .matches(':')
        .count()
        < 2
    {
        return None;
    }
    let query = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("video_id", video_id)
        .finish();
    Some(format!("{base}/admin/api/v2/video-phash/thumbnail?{query}"))
}

/// Resolve Resolver-provided image URLs like the Android client does. Resolver
/// responses may contain relative paths or an `/admin/` URL generated for a
/// different external origin than the one configured in the desktop app.
pub fn resolve_resolver_thumbnail_url(url_or_path: &str) -> Option<String> {
    let value = url_or_path.trim();
    if value.is_empty() {
        return None;
    }

    let base = resolver_base_url().read().ok()?.clone();
    resolve_resolver_thumbnail_url_at(value, &base)
}

fn resolve_resolver_thumbnail_url_at(url_or_path: &str, base: &str) -> Option<String> {
    let value = url_or_path.trim();
    if value.is_empty() || value.starts_with("//") {
        return None;
    }
    if let Ok(image_url) = url::Url::parse(value) {
        if !matches!(image_url.scheme(), "http" | "https") {
            return None;
        }
        if !image_url.username().is_empty() || image_url.password().is_some() {
            return None;
        }
        let is_rewrite_route = image_url.path().starts_with("/admin/")
            || image_url.path() == "/admin"
            || image_url.path().starts_with("/api/v1/artwork/")
            || image_url.path() == "/api/v1/artwork";

        if is_rewrite_route {
            if base.is_empty() {
                return None;
            }
            let configured_origin = url::Url::parse(base).ok()?;
            if !matches!(configured_origin.scheme(), "http" | "https") {
                return None;
            }
            let mut configured = configured_origin.clone();
            let base_path = configured_origin.path().trim_end_matches('/');
            let target_path = image_url.path();
            let full_path = if base_path.is_empty() {
                target_path.to_string()
            } else {
                format!("{base_path}{target_path}")
            };
            configured.set_path(&full_path);
            configured.set_query(image_url.query());
            configured.set_fragment(image_url.fragment());
            return Some(configured.into());
        }
        return Some(image_url.to_string());
    }

    if base.is_empty() {
        return None;
    }
    let mut base_url = url::Url::parse(base).ok()?;
    if !matches!(base_url.scheme(), "http" | "https") {
        return None;
    }
    let (without_frag, frag) = match value.split_once('#') {
        Some((w, f)) => (w, Some(f)),
        None => (value, None),
    };
    let (path_only, query) = match without_frag.split_once('?') {
        Some((p, q)) => (p, Some(q)),
        None => (without_frag, None),
    };
    let base_path = base_url.path().trim_end_matches('/');
    let rel_path = path_only.trim_start_matches('/');
    let full_path = if base_path.is_empty() {
        format!("/{rel_path}")
    } else {
        format!("{base_path}/{rel_path}")
    };
    base_url.set_path(&full_path);
    base_url.set_query(query);
    base_url.set_fragment(frag);
    matches!(base_url.scheme(), "http" | "https").then(|| base_url.to_string())
}

pub fn cache_dir() -> PathBuf {
    let dir = cache_dir_path();
    let _ = std::fs::create_dir_all(&dir);
    dir
}

fn cache_dir_path() -> PathBuf {
    #[cfg(windows)]
    if let Some(local_app_data) = std::env::var_os("LOCALAPPDATA") {
        return PathBuf::from(local_app_data)
            .join("AdultHubDesktop")
            .join("cache")
            .join("thumbnails");
    }

    #[cfg(not(windows))]
    {
        if let Some(xdg_cache_home) = std::env::var_os("XDG_CACHE_HOME") {
            return PathBuf::from(xdg_cache_home)
                .join("adult-hub-desktop")
                .join("thumbnails");
        }
        if let Some(home) = std::env::var_os("HOME") {
            return PathBuf::from(home)
                .join(".cache")
                .join("adult-hub-desktop")
                .join("thumbnails");
        }
    }

    adult_hub_storage::default_db_path()
        .parent()
        .unwrap_or_else(|| std::path::Path::new("."))
        .join("cache")
        .join("thumbnails")
}

fn clear_cache_files() {
    let dir = cache_dir();
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

fn prune_cache(limit_bytes: usize, keep: Option<&std::path::PathBuf>) {
    let dir = cache_dir();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };

    let mut files = entries
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            let metadata = entry.metadata().ok()?;
            if !metadata.is_file() {
                return None;
            }
            Some((
                path,
                metadata.len() as usize,
                metadata.modified().unwrap_or(UNIX_EPOCH),
            ))
        })
        .collect::<Vec<_>>();
    let mut total = files.iter().map(|(_, size, _)| *size).sum::<usize>();
    if total <= limit_bytes {
        return;
    }
    files.sort_by_key(|(_, _, modified)| *modified);
    for (path, size, _) in files {
        if total <= limit_bytes {
            break;
        }
        if keep.is_some_and(|keep| keep == &path) {
            continue;
        }
        if std::fs::remove_file(path).is_ok() {
            total = total.saturating_sub(size);
        }
    }
}

fn decode_dynamic_image(bytes: &[u8]) -> Option<image::DynamicImage> {
    let mut reader = image::ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .ok()?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(8192);
    limits.max_image_height = Some(8192);
    limits.max_alloc = Some(96 * 1024 * 1024);
    reader.limits(limits);
    reader.decode().ok()
}

/// Artwork decoded once on a worker thread and handed to the UI as plain RGBA.
///
/// `slint::Image` is not `Send`, so the pipeline never moves image handles
/// between threads: workers decode, resize and report whether the artwork is
/// usable, and the UI only copies the bytes into a `SharedPixelBuffer`.
#[derive(Clone, Debug)]
pub struct DecodedArtwork {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
    pub poor: bool,
}

impl DecodedArtwork {
    pub fn byte_len(&self) -> usize {
        self.rgba.len()
    }
}

/// Artwork image kept in this process so returning to a screen does not decode
/// the same file again. Slint images are bound to the UI thread, so this lives
/// in a thread-local and is only consulted from card builders on that thread.
struct MemoryImageCache {
    entries: HashMap<(String, u32), (slint::Image, usize)>,
    order: VecDeque<(String, u32)>,
    total_bytes: usize,
}

impl MemoryImageCache {
    fn new() -> Self {
        Self {
            entries: HashMap::new(),
            order: VecDeque::new(),
            total_bytes: 0,
        }
    }

    fn limit(&self) -> usize {
        if LOW_RAM_MODE.load(Ordering::Relaxed) {
            MEMORY_CACHE_LIMIT_BYTES_LOW_RAM
        } else {
            MEMORY_CACHE_LIMIT_BYTES
        }
    }

    fn get(&mut self, key: &(String, u32)) -> Option<slint::Image> {
        let image = self.entries.get(key).map(|(image, _)| image.clone());
        if image.is_some() {
            if let Some(position) = self.order.iter().position(|entry| entry == key) {
                if let Some(entry) = self.order.remove(position) {
                    self.order.push_back(entry);
                }
            }
        }
        image
    }

    fn insert(&mut self, key: (String, u32), image: slint::Image, bytes: usize) {
        if let Some((_, previous)) = self.entries.remove(&key) {
            self.total_bytes = self.total_bytes.saturating_sub(previous);
        }
        if let Some(position) = self.order.iter().position(|entry| entry == &key) {
            self.order.remove(position);
        }
        self.entries.insert(key.clone(), (image, bytes));
        self.order.push_back(key);
        self.total_bytes = self.total_bytes.saturating_add(bytes);
        let limit = self.limit();
        while self.total_bytes > limit {
            let Some(oldest) = self.order.pop_front() else {
                break;
            };
            if let Some((_, removed)) = self.entries.remove(&oldest) {
                self.total_bytes = self.total_bytes.saturating_sub(removed);
            }
        }
    }
}

thread_local! {
    static MEMORY_IMAGES: RefCell<MemoryImageCache> = RefCell::new(MemoryImageCache::new());
}

struct CountPermit {
    semaphore: &'static (Mutex<usize>, Condvar),
}

impl Drop for CountPermit {
    fn drop(&mut self) {
        let (active, available) = self.semaphore;
        if let Ok(mut count) = active.lock() {
            *count = count.saturating_sub(1);
            available.notify_one();
        }
    }
}

fn acquire_count_permit(
    semaphore: &'static (Mutex<usize>, Condvar),
    limit: usize,
) -> Option<CountPermit> {
    let (active, available) = semaphore;
    let mut count = active.lock().ok()?;
    while *count >= limit.max(1) {
        count = available.wait(count).ok()?;
    }
    *count += 1;
    Some(CountPermit { semaphore })
}

static NETWORK_PERMITS: OnceLock<(Mutex<usize>, Condvar)> = OnceLock::new();
static DECODE_PERMITS: OnceLock<(Mutex<usize>, Condvar)> = OnceLock::new();

fn network_permit() -> Option<CountPermit> {
    acquire_count_permit(
        NETWORK_PERMITS.get_or_init(|| (Mutex::new(0), Condvar::new())),
        MAX_CONCURRENT_IMAGE_DOWNLOADS,
    )
}

fn decode_limit() -> usize {
    std::thread::available_parallelism()
        .map(|threads| threads.get())
        .unwrap_or(4)
        .clamp(2, 8)
}

fn decode_permit() -> Option<CountPermit> {
    acquire_count_permit(
        DECODE_PERMITS.get_or_init(|| (Mutex::new(0), Condvar::new())),
        decode_limit(),
    )
}

#[cfg(test)]
pub(crate) fn acquire_network_permit_for_test() -> Option<CountPermit> {
    network_permit()
}

#[cfg(test)]
pub(crate) fn network_permit_limit_for_test() -> usize {
    MAX_CONCURRENT_IMAGE_DOWNLOADS
}

fn rgba_from_dynamic(image: image::DynamicImage) -> image::RgbaImage {
    image.into_rgba8()
}

fn display_width(requested: u32) -> u32 {
    if LOW_RAM_MODE.load(Ordering::Relaxed) {
        requested.min(320)
    } else {
        requested.max(1)
    }
}

fn downscale_rgba(image: image::RgbaImage, max_width: u32) -> image::RgbaImage {
    let max_width = max_width.max(1);
    if image.width() <= max_width {
        return image;
    }
    let height = ((u64::from(image.height()) * u64::from(max_width)) / u64::from(image.width()))
        .max(1) as u32;
    image::imageops::resize(
        &image,
        max_width,
        height,
        image::imageops::FilterType::Triangle,
    )
}

/// Mirrors the Android client check for missing or unhelpfully dark artwork.
/// Samples a 12x12 grid straight from the decoded pixels, which matches the old
/// 24x24 NEAREST downscale at even coordinates without a second allocation.
fn rgba_is_poor(image: &image::RgbaImage) -> bool {
    let (width, height) = image.dimensions();
    if (1..320).contains(&width) || (1..160).contains(&height) {
        return true;
    }
    let mut count = 0usize;
    let mut dark_count = 0usize;
    let mut luminance_sum = 0.0f64;
    let mut luminance_squared_sum = 0.0f64;
    for row in 0..12u32 {
        let y = ((u64::from(row) * u64::from(height)) / 12) as u32;
        let y = y.min(height.saturating_sub(1));
        for column in 0..12u32 {
            let x = ((u64::from(column) * u64::from(width)) / 12) as u32;
            let x = x.min(width.saturating_sub(1));
            let pixel = image.get_pixel(x, y).0;
            let alpha = f64::from(pixel[3]) / 255.0;
            let luminance = (0.2126 * f64::from(pixel[0])
                + 0.7152 * f64::from(pixel[1])
                + 0.0722 * f64::from(pixel[2]))
                * alpha;
            if luminance < 18.0 {
                dark_count += 1;
            }
            luminance_sum += luminance;
            luminance_squared_sum += luminance * luminance;
            count += 1;
        }
    }
    if count == 0 {
        return false;
    }
    let average = luminance_sum / count as f64;
    let variance = (luminance_squared_sum / count as f64 - average * average).max(0.0);
    let nearly_all_black = dark_count as f64 / count as f64 >= 0.88 && average < 55.0;
    nearly_all_black || (average < 32.0 && variance < 180.0)
}

fn artwork_from_rgba(image: image::RgbaImage, max_width: u32, poor: bool) -> DecodedArtwork {
    let image = downscale_rgba(image, display_width(max_width));
    let (width, height) = image.dimensions();
    DecodedArtwork {
        width,
        height,
        rgba: image.into_raw(),
        poor,
    }
}

/// Decode artwork once, at most `max_width` pixels wide.
fn decode_artwork(bytes: &[u8], max_width: u32) -> Option<DecodedArtwork> {
    let _permit = decode_permit();
    let image = rgba_from_dynamic(decode_dynamic_image(bytes)?);
    let poor = rgba_is_poor(&image);
    Some(artwork_from_rgba(image, max_width, poor))
}

fn encode_cache_jpeg(image: &image::RgbaImage) -> Option<Vec<u8>> {
    let image = downscale_rgba(image.clone(), CACHE_ARTWORK_WIDTH);
    let rgb = image::DynamicImage::ImageRgba8(image).into_rgb8();
    let mut encoded = Vec::new();
    let mut encoder =
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut encoded, CACHE_JPEG_QUALITY);
    encoder.encode_image(&rgb).ok()?;
    Some(encoded)
}

/// Build the Slint image from already decoded pixels. Cheap: one copy of the
/// RGBA buffer, no decoding, safe to call on the UI thread.
pub fn artwork_to_image(artwork: &DecodedArtwork) -> slint::Image {
    let buffer = slint::SharedPixelBuffer::<slint::Rgba8Pixel>::clone_from_slice(
        &artwork.rgba,
        artwork.width,
        artwork.height,
    );
    slint::Image::from_rgba8(buffer)
}

/// Return a previously decoded image for this URL, if it is still in memory.
pub fn cached_memory_image(url: &str, max_width: u32) -> Option<slint::Image> {
    if url.trim().is_empty() {
        return None;
    }
    let key = (url.to_owned(), display_width(max_width));
    MEMORY_IMAGES.with(|cache| cache.borrow_mut().get(&key))
}

/// Build the UI image and remember it so the next screen build skips decoding.
pub fn remember_artwork_image(url: &str, max_width: u32, artwork: &DecodedArtwork) -> slint::Image {
    let image = artwork_to_image(artwork);
    if !url.trim().is_empty() {
        let key = (url.to_owned(), display_width(max_width));
        MEMORY_IMAGES.with(|cache| {
            cache
                .borrow_mut()
                .insert(key, image.clone(), artwork.byte_len())
        });
    }
    image
}

pub fn url_to_cache_path(url: &str) -> PathBuf {
    let hash = ring::digest::digest(&ring::digest::SHA256, url.as_bytes());
    let hex: String = hash.as_ref().iter().map(|b| format!("{:02x}", b)).collect();
    cache_dir().join(format!("{hex}.jpg"))
}

fn url_to_cache_path_without_create(url: &str) -> PathBuf {
    let hash = ring::digest::digest(&ring::digest::SHA256, url.as_bytes());
    let hex: String = hash
        .as_ref()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    cache_dir_path().join(format!("{hex}.jpg"))
}

fn http_client() -> Option<&'static reqwest::blocking::Client> {
    static CLIENT: OnceLock<Option<reqwest::blocking::Client>> = OnceLock::new();
    CLIENT
        .get_or_init(|| {
            reqwest::blocking::Client::builder()
                .timeout(Duration::from_secs(5))
                // Handle redirects explicitly: reqwest cannot know that the
                // custom X-Emby-Token header is a credential.
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .ok()
        })
        .as_ref()
}

fn is_resolver_origin_request(original: &url::Url, current: &url::Url, base: &str) -> bool {
    let Ok(configured) = url::Url::parse(base) else {
        return false;
    };
    original.origin() == current.origin() && original.origin() == configured.origin()
}

fn is_non_image_content_type(value: &str) -> bool {
    let content_type = value
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();
    content_type.starts_with("text/")
        || content_type == "application/json"
        || content_type.ends_with("+json")
        || content_type == "application/xml"
        || content_type.ends_with("+xml")
}

fn retryable_image_status(status: reqwest::StatusCode) -> bool {
    status == reqwest::StatusCode::REQUEST_TIMEOUT
        || status == reqwest::StatusCode::TOO_MANY_REQUESTS
        || status.is_server_error()
}

#[derive(Debug)]
enum ImageRequestError {
    Transport(reqwest::Error),
    InvalidRedirect,
    RedirectLimit,
}

/// Classified reason an artwork fetch did not produce a usable image. The
/// diagnostics surface reports the category and HTTP code (where one exists)
/// plus the stage that failed, so "download failed" is no longer the only
/// answer available to the user.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ThumbnailFailure {
    InvalidUrl,
    HttpStatus(u16),
    NonImageContentType(String),
    TooLarge,
    EmptyBody,
    ReadBody,
    Timeout,
    Connect,
    Request,
    InvalidRedirect,
    RedirectLimit,
    Decode,
}

impl ThumbnailFailure {
    /// True for outages that a later background attempt can plausibly clear.
    pub fn is_transient(&self) -> bool {
        match self {
            Self::Timeout
            | Self::Connect
            | Self::Request
            | Self::EmptyBody
            | Self::ReadBody
            | Self::RedirectLimit => true,
            Self::HttpStatus(status) => *status == 408 || *status == 429 || *status >= 500,
            _ => false,
        }
    }

    pub fn stage(&self) -> &'static str {
        match self {
            Self::InvalidUrl => "artwork selection",
            Self::Decode => "decode",
            _ => "download",
        }
    }

    pub fn label(&self) -> String {
        match self {
            Self::InvalidUrl => "no usable artwork URL".into(),
            Self::HttpStatus(status) => format!("HTTP {status}"),
            Self::NonImageContentType(content_type) => {
                format!("server answered {content_type} instead of an image")
            }
            Self::TooLarge => "image exceeded the 10 MB limit".into(),
            Self::EmptyBody => "empty image response".into(),
            Self::ReadBody => "failed to read image response".into(),
            Self::Timeout => "request timed out".into(),
            Self::Connect => "could not connect to image server".into(),
            Self::Request => "request failed".into(),
            Self::InvalidRedirect => "invalid or unsafe redirect".into(),
            Self::RedirectLimit => "redirect limit exceeded".into(),
            Self::Decode => {
                "response could not be decoded (unsupported format or invalid data)".into()
            }
        }
    }
}

fn send_image_request(
    url: &str,
    token: Option<&str>,
) -> Result<reqwest::blocking::Response, ImageRequestError> {
    let original = url::Url::parse(url).map_err(|_| ImageRequestError::InvalidRedirect)?;
    if !matches!(original.scheme(), "http" | "https")
        || !original.username().is_empty()
        || original.password().is_some()
    {
        return Err(ImageRequestError::InvalidRedirect);
    }
    let resolver_base = resolver_base_url()
        .read()
        .map(|base| base.clone())
        .unwrap_or_default();
    let mut current = original.clone();
    for redirect_count in 0..=5 {
        let mut request = http_client()
            .ok_or(ImageRequestError::InvalidRedirect)?
            .get(current.clone());
        // Keep credentials on the matching configured service origin only. A
        // redirect to a CDN or other host is fetched without auth headers.
        if is_resolver_origin_request(&original, &current, &resolver_base) {
            let resolver_token = resolver_auth_token()
                .read()
                .ok()
                .and_then(|value| value.clone())
                .or_else(|| token.map(str::to_owned));
            if let Some(token) = resolver_token.filter(|value| !value.is_empty()) {
                request = request
                    .header("X-Emby-Token", &token)
                    .header(reqwest::header::AUTHORIZATION, format!("Bearer {token}"));
            }
        }
        let response = request.send().map_err(ImageRequestError::Transport)?;
        if !response.status().is_redirection() {
            return Ok(response);
        }
        if redirect_count == 5 {
            return Err(ImageRequestError::RedirectLimit);
        }
        let location = response
            .headers()
            .get(reqwest::header::LOCATION)
            .and_then(|value| value.to_str().ok())
            .ok_or(ImageRequestError::InvalidRedirect)?;
        let next = response
            .url()
            .join(location)
            .map_err(|_| ImageRequestError::InvalidRedirect)?;
        if !matches!(next.scheme(), "http" | "https")
            || !next.username().is_empty()
            || next.password().is_some()
        {
            return Err(ImageRequestError::InvalidRedirect);
        }
        current = next;
    }
    Err(ImageRequestError::RedirectLimit)
}

/// Return a cached, already decoded image for this URL.
///
/// Card builders run on the UI thread; they must never decode artwork there.
/// Only images decoded earlier by the worker pipeline are returned.
pub fn load_cached_image(url: &str) -> Option<slint::Image> {
    cached_memory_image(url, CARD_ARTWORK_WIDTH)
}

fn discard_corrupt_cache_entry(url: &str, path: &std::path::Path) {
    let removed = CACHE_WRITE_LOCK
        .lock()
        .ok()
        .is_some_and(|_guard| std::fs::remove_file(path).is_ok());
    crate::diagnostics::GLOBAL_LOG.w(
        "Thumbnail",
        &format!(
            "{}: cached image failed decoding{}",
            diagnostic_subject(url),
            if removed {
                " and the corrupt entry was removed"
            } else {
                ""
            }
        ),
    );
}

fn load_cached_artwork(url: &str, max_width: u32) -> Option<DecodedArtwork> {
    let path = url_to_cache_path_without_create(url);
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return None,
        Err(error) => {
            crate::diagnostics::GLOBAL_LOG.w(
                "Thumbnail",
                &format!(
                    "{}: cached image could not be read ({})",
                    diagnostic_subject(url),
                    error.kind()
                ),
            );
            return None;
        }
    };
    match decode_artwork(&bytes, max_width) {
        Some(artwork) => Some(artwork),
        None => {
            discard_corrupt_cache_entry(url, &path);
            None
        }
    }
}

fn download_image_bytes(url: &str, token: Option<&str>) -> Result<Vec<u8>, ThumbnailFailure> {
    let _network = network_permit();
    let mut downloaded_bytes = None;
    let mut final_failure = ThumbnailFailure::Request;
    for attempt in 0..2 {
        match send_image_request(url, token) {
            Ok(resp) => {
                let status = resp.status();
                if status.is_success() {
                    let content_type = resp
                        .headers()
                        .get(reqwest::header::CONTENT_TYPE)
                        .and_then(|value| value.to_str().ok())
                        .unwrap_or_default();
                    if is_non_image_content_type(content_type) {
                        final_failure = ThumbnailFailure::NonImageContentType(
                            content_type
                                .split(';')
                                .next()
                                .unwrap_or("unknown")
                                .trim()
                                .to_owned(),
                        );
                        break;
                    }
                    if resp
                        .content_length()
                        .is_some_and(|size| size > MAX_IMAGE_BYTES as u64)
                    {
                        final_failure = ThumbnailFailure::TooLarge;
                        break;
                    }
                    let mut bytes = Vec::new();
                    let mut limited_response = resp.take(MAX_IMAGE_BYTES as u64 + 1);
                    match limited_response.read_to_end(&mut bytes) {
                        Ok(_) if !bytes.is_empty() && bytes.len() <= MAX_IMAGE_BYTES => {
                            downloaded_bytes = Some(bytes);
                            break;
                        }
                        Ok(_) if bytes.len() > MAX_IMAGE_BYTES => {
                            final_failure = ThumbnailFailure::TooLarge;
                            break;
                        }
                        Ok(_) if attempt < 1 => {
                            std::thread::sleep(Duration::from_millis(180 * (attempt + 1) as u64));
                            continue;
                        }
                        Ok(_) => {
                            final_failure = ThumbnailFailure::EmptyBody;
                            break;
                        }
                        Err(_) if attempt < 1 => {
                            std::thread::sleep(Duration::from_millis(180 * (attempt + 1) as u64));
                            continue;
                        }
                        Err(_) => {
                            final_failure = ThumbnailFailure::ReadBody;
                            break;
                        }
                    }
                }
                let retryable = retryable_image_status(status);
                if retryable && attempt < 1 {
                    std::thread::sleep(Duration::from_millis(180 * (attempt + 1) as u64));
                    continue;
                }
                final_failure = ThumbnailFailure::HttpStatus(status.as_u16());
                break;
            }
            Err(ImageRequestError::Transport(error)) => {
                let retryable = error.is_timeout() || error.is_connect() || error.is_request();
                if retryable && attempt < 1 {
                    std::thread::sleep(Duration::from_millis(180 * (attempt + 1) as u64));
                    continue;
                }
                final_failure = if error.is_timeout() {
                    ThumbnailFailure::Timeout
                } else if error.is_connect() {
                    ThumbnailFailure::Connect
                } else {
                    ThumbnailFailure::Request
                };
                break;
            }
            Err(ImageRequestError::InvalidRedirect) => {
                final_failure = ThumbnailFailure::InvalidRedirect;
                break;
            }
            Err(ImageRequestError::RedirectLimit) => {
                final_failure = ThumbnailFailure::RedirectLimit;
                break;
            }
        }
    }
    let Some(bytes) = downloaded_bytes else {
        let subject = diagnostic_subject(url);
        if let ThumbnailFailure::HttpStatus(404) = final_failure {
            static SEEN_404: Mutex<Option<std::collections::HashSet<String>>> = Mutex::new(None);
            let is_new = {
                let mut guard = SEEN_404
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                let set = guard.get_or_insert_with(std::collections::HashSet::new);
                if set.len() > 200 {
                    set.clear();
                }
                set.insert(subject.clone())
            };
            if is_new {
                crate::log_debug(&format!("artwork not found (HTTP 404): {subject}"));
                crate::diagnostics::GLOBAL_LOG
                    .w("Thumbnail", &format!("{subject}: HTTP 404 not found"));
            }
        } else {
            crate::log_debug(&format!(
                "thumbnail fetch failed: {stage} - {detail}",
                stage = final_failure.stage(),
                detail = final_failure.label()
            ));
            crate::diagnostics::GLOBAL_LOG.w(
                "Thumbnail",
                &format!(
                    "{subject}: {stage} failed: {detail}",
                    stage = final_failure.stage(),
                    detail = final_failure.label()
                ),
            );
        }
        return Err(final_failure);
    };
    Ok(bytes)
}

fn store_cached_artwork(url: &str, image: &image::RgbaImage) {
    let Ok(_guard) = CACHE_WRITE_LOCK.lock() else {
        return;
    };
    let limit_mb = CACHE_LIMIT_MB.load(Ordering::Relaxed);
    if limit_mb == 0 {
        return;
    }
    let Some(encoded) = encode_cache_jpeg(image) else {
        return;
    };
    let limit_bytes = limit_mb.saturating_mul(1024 * 1024);
    if encoded.len() > limit_bytes {
        return;
    }
    let path = url_to_cache_path(url);
    prune_cache(limit_bytes.saturating_sub(encoded.len()), Some(&path));
    if write_cache_atomically(&path, &encoded) {
        prune_cache(limit_bytes, Some(&path));
    }
}

/// Fetch (or reuse from disk) artwork and return it decoded once, at most
/// `max_width` pixels wide, together with the "poor artwork" verdict.
pub fn fetch_and_cache_artwork(
    url: &str,
    token: Option<&str>,
    max_width: u32,
) -> Result<DecodedArtwork, ThumbnailFailure> {
    fetch_image_artwork(url, token, max_width, true)
}

/// Connection diagnostic fetch: fresh download, failure reported by category,
/// HTTP code and stage instead of a bare success flag.
pub fn probe_image(url: &str, token: Option<&str>) -> Result<(), ThumbnailFailure> {
    fetch_image_artwork(url, token, CARD_ARTWORK_WIDTH, false).map(|_| ())
}

fn fetch_image_artwork(
    url: &str,
    token: Option<&str>,
    max_width: u32,
    allow_cached_image: bool,
) -> Result<DecodedArtwork, ThumbnailFailure> {
    let url = url.trim();
    if !(url.starts_with("http://") || url.starts_with("https://")) {
        return Err(ThumbnailFailure::InvalidUrl);
    }
    if allow_cached_image && let Some(artwork) = load_cached_artwork(url, max_width) {
        return Ok(artwork);
    }

    let bytes = download_image_bytes(url, token)?;
    let image = {
        let _permit = decode_permit();
        rgba_from_dynamic(decode_dynamic_image(&bytes).ok_or_else(|| {
            crate::log_debug("thumbnail decode failed");
            crate::diagnostics::GLOBAL_LOG.w(
                "Thumbnail",
                &format!(
                    "{}: decode failed: {detail}",
                    diagnostic_subject(url),
                    detail = ThumbnailFailure::Decode.label()
                ),
            );
            ThumbnailFailure::Decode
        })?)
    };
    let poor = rgba_is_poor(&image);
    if allow_cached_image {
        store_cached_artwork(url, &image);
    }
    crate::diagnostics::GLOBAL_LOG.i(
        "Thumbnail",
        &format!(
            "{}: image downloaded and decoded successfully ({} bytes)",
            diagnostic_subject(url),
            bytes.len()
        ),
    );
    Ok(artwork_from_rgba(image, max_width, poor))
}

/// Persist a cache entry via a temporary file and a rename. A direct
/// `fs::write` that is interrupted (disk full, crash) leaves a half-written
/// PNG that still decodes into a damaged picture — the "corrupt thumbnail"
/// reports — and that entry then survives every later load instead of being
/// re-fetched.
fn write_cache_atomically(path: &std::path::Path, bytes: &[u8]) -> bool {
    let temp_path = cache_temp_path(path);
    if std::fs::write(&temp_path, bytes).is_err() {
        let _ = std::fs::remove_file(&temp_path);
        return false;
    }
    if std::fs::rename(&temp_path, path).is_err() {
        let _ = std::fs::remove_file(&temp_path);
        return false;
    }
    true
}

fn cache_temp_path(path: &std::path::Path) -> PathBuf {
    static TEMP_SEQUENCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "image".into());
    let sequence = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    path.with_file_name(format!("{name}.{}.{sequence}.tmp", std::process::id()))
}

/// Compact, credential-free identity of the image subject for diagnostics.
/// Item image URLs contribute the leading item GUID characters and the image
/// type; everything else contributes only the host. Query parameters (image
/// tags, api keys) are never included.
fn diagnostic_subject(url: &str) -> String {
    let Ok(parsed) = url::Url::parse(url) else {
        return "thumbnail".to_string();
    };
    let segments = parsed
        .path_segments()
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();
    if let Some(items_index) = segments
        .iter()
        .rposition(|segment| segment.eq_ignore_ascii_case("Items"))
        && let Some(id) = segments.get(items_index + 1)
        && !id.is_empty()
    {
        let short_id = id.chars().take(8).collect::<String>();
        let image_type = segments.get(items_index + 3).filter(|_| {
            segments
                .get(items_index + 2)
                .is_some_and(|segment| segment.eq_ignore_ascii_case("Images"))
        });
        return match image_type {
            Some(image_type) => format!("thumbnail for item {short_id} ({image_type})"),
            None => format!("thumbnail for item {short_id}"),
        };
    }
    if let Some(video_id) = parsed
        .query_pairs()
        .find(|(key, _)| key == "video_id")
        .map(|(_, value)| value.chars().take(24).collect::<String>())
    {
        return format!("thumbnail for media {video_id}");
    }
    match parsed.host_str() {
        Some(host) => format!("thumbnail from {host}"),
        None => "thumbnail".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        cache_url_variants, decode_dynamic_image, is_non_image_content_type,
        is_resolver_origin_request, resolve_resolver_thumbnail_url_at, retryable_image_status,
    };
    use std::io::{Cursor, Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::sync::Mutex;
    use std::time::{SystemTime, UNIX_EPOCH};

    static RESOLVER_REQUEST_TEST_LOCK: Mutex<()> = Mutex::new(());

    fn read_request(stream: &mut TcpStream) -> String {
        let mut request = Vec::new();
        let mut buffer = [0_u8; 1024];
        while let Ok(count) = stream.read(&mut buffer) {
            if count == 0 {
                break;
            }
            request.extend_from_slice(&buffer[..count]);
            if request.windows(4).any(|window| window == b"\r\n\r\n") {
                break;
            }
        }
        String::from_utf8_lossy(&request).into_owned()
    }

    fn respond(stream: &mut TcpStream, response: &[u8]) {
        stream.write_all(response).unwrap();
        stream.flush().unwrap();
    }

    #[test]
    fn resolves_absolute_resolver_admin_urls_to_configured_origin() {
        let resolved = resolve_resolver_thumbnail_url_at(
            "https://old.example/admin/api/v2/video-phash/thumbnail?video_id=telegram%3Ac%3A1",
            "https://resolver.example/prefix",
        );
        assert_eq!(
            resolved.as_deref(),
            Some(
                "https://resolver.example/prefix/admin/api/v2/video-phash/thumbnail?video_id=telegram%3Ac%3A1"
            )
        );

        let resolved_trailing = resolve_resolver_thumbnail_url_at(
            "https://old.example/admin/api/v2/video-phash/thumbnail?video_id=telegram%3Ac%3A1",
            "https://resolver.example/prefix/",
        );
        assert_eq!(
            resolved_trailing.as_deref(),
            Some(
                "https://resolver.example/prefix/admin/api/v2/video-phash/thumbnail?video_id=telegram%3Ac%3A1"
            )
        );

        let resolved_no_prefix = resolve_resolver_thumbnail_url_at(
            "https://old.example/admin/api/v2/video-phash/thumbnail?video_id=telegram%3Ac%3A1",
            "https://resolver.example",
        );
        assert_eq!(
            resolved_no_prefix.as_deref(),
            Some(
                "https://resolver.example/admin/api/v2/video-phash/thumbnail?video_id=telegram%3Ac%3A1"
            )
        );

        let resolved_api = resolve_resolver_thumbnail_url_at(
            "https://old.example/api/v1/artwork/video/telegram%3Ac%3A1/poster",
            "https://resolver.example/prefix",
        );
        assert_eq!(
            resolved_api.as_deref(),
            Some("https://resolver.example/prefix/api/v1/artwork/video/telegram%3Ac%3A1/poster")
        );

        let resolved_rel_api = resolve_resolver_thumbnail_url_at(
            "/api/v1/artwork/performer/stashdb%3Auuid-123/photo",
            "https://resolver.example/prefix",
        );
        assert_eq!(
            resolved_rel_api.as_deref(),
            Some(
                "https://resolver.example/prefix/api/v1/artwork/performer/stashdb%3Auuid-123/photo"
            )
        );
    }

    #[test]
    fn joins_relative_image_paths_without_losing_base_prefix_or_query_encoding() {
        let resolved = resolve_resolver_thumbnail_url_at(
            "images/poster%20one.jpg?size=small&tag=a%2Bb",
            "https://resolver.example/prefix/",
        );
        assert_eq!(
            resolved.as_deref(),
            Some("https://resolver.example/prefix/images/poster%20one.jpg?size=small&tag=a%2Bb")
        );
    }

    #[test]
    fn leaves_external_https_cdn_urls_on_their_origin() {
        let resolved = resolve_resolver_thumbnail_url_at(
            "https://cdn.example/poster.jpg",
            "https://resolver.example",
        );
        assert_eq!(resolved.as_deref(), Some("https://cdn.example/poster.jpg"));

        // External CDN URL remains usable even when resolver base is empty or invalid
        assert_eq!(
            resolve_resolver_thumbnail_url_at("https://cdn.example/poster.jpg", "").as_deref(),
            Some("https://cdn.example/poster.jpg")
        );
        assert_eq!(
            resolve_resolver_thumbnail_url_at(
                "https://cdn.example/poster.jpg",
                "invalid-scheme://"
            )
            .as_deref(),
            Some("https://cdn.example/poster.jpg")
        );

        // External /api/... URLs untouched (not narrowed /api/v1/artwork/ rewrite routes)
        assert_eq!(
            resolve_resolver_thumbnail_url_at(
                "https://cdn.example/api/v2/items/poster.jpg",
                "https://resolver.example"
            )
            .as_deref(),
            Some("https://cdn.example/api/v2/items/poster.jpg")
        );
        assert_eq!(
            resolve_resolver_thumbnail_url_at("https://cdn.example/api/v2/items/poster.jpg", "")
                .as_deref(),
            Some("https://cdn.example/api/v2/items/poster.jpg")
        );

        // Resolver local routes require valid resolver base; rejected when base is empty
        assert_eq!(
            resolve_resolver_thumbnail_url_at(
                "https://old.example/api/v1/artwork/video/v1/poster",
                ""
            ),
            None
        );
        assert_eq!(
            resolve_resolver_thumbnail_url_at(
                "https://old.example/admin/api/v2/video-phash/thumbnail?id=1",
                ""
            ),
            None
        );
    }

    #[test]
    fn rejects_non_http_image_schemes() {
        assert_eq!(
            resolve_resolver_thumbnail_url_at("file:///tmp/poster.jpg", "https://resolver.example"),
            None
        );
        assert_eq!(
            resolve_resolver_thumbnail_url_at("file:///tmp/poster.jpg", ""),
            None
        );
        assert_eq!(
            resolve_resolver_thumbnail_url_at(
                "ftp://cdn.example/poster.jpg",
                "https://resolver.example"
            ),
            None
        );
        assert_eq!(
            resolve_resolver_thumbnail_url_at("javascript:alert(1)", "https://resolver.example"),
            None
        );
        assert_eq!(
            resolve_resolver_thumbnail_url_at("data:image/png;base64,123", ""),
            None
        );
        assert_eq!(
            resolve_resolver_thumbnail_url_at(
                "https://user:secret@cdn.example/poster.jpg",
                "https://resolver.example"
            ),
            None
        );
        assert_eq!(
            resolve_resolver_thumbnail_url_at("https://user:secret@cdn.example/poster.jpg", ""),
            None
        );
        assert_eq!(
            resolve_resolver_thumbnail_url_at(
                "https://user@cdn.example/poster.jpg",
                "https://resolver.example"
            ),
            None
        );
        assert_eq!(
            resolve_resolver_thumbnail_url_at(
                "https://:secret@cdn.example/poster.jpg",
                "https://resolver.example"
            ),
            None
        );
    }

    #[test]
    fn resolver_token_is_scoped_to_its_origin_and_not_redirected_hosts() {
        let original =
            url::Url::parse("https://resolver.example/admin/api/v2/video-phash/thumbnail?id=anon")
                .unwrap();
        let same_origin = url::Url::parse("https://resolver.example/cdn/image.jpg").unwrap();
        let redirected = url::Url::parse("https://cdn.example/image.jpg").unwrap();

        assert!(is_resolver_origin_request(
            &original,
            &original,
            "https://resolver.example/base"
        ));
        assert!(is_resolver_origin_request(
            &original,
            &same_origin,
            "https://resolver.example/base"
        ));
        assert!(!is_resolver_origin_request(
            &original,
            &redirected,
            "https://resolver.example/base"
        ));
        assert!(!is_resolver_origin_request(
            &redirected,
            &redirected,
            "https://resolver.example/base"
        ));
    }

    #[test]
    fn resolver_thumbnail_request_sends_token_only_to_configured_origin() {
        let _guard = RESOLVER_REQUEST_TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let resolver = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let resolver_address = resolver.local_addr().unwrap();
        let cdn = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let cdn_address = cdn.local_addr().unwrap();

        let cdn_server = std::thread::spawn(move || {
            let (mut stream, _) = cdn.accept().unwrap();
            let request = read_request(&mut stream);
            respond(
                &mut stream,
                b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok",
            );
            request
        });
        let resolver_server = std::thread::spawn(move || {
            let (mut stream, _) = resolver.accept().unwrap();
            let request = read_request(&mut stream);
            let location = format!("http://{cdn_address}/thumbnail.jpg");
            let response = format!(
                "HTTP/1.1 302 Found\r\nLocation: {location}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            );
            respond(&mut stream, response.as_bytes());
            request
        });

        super::set_resolver_base_url(&format!("http://{resolver_address}"));
        super::set_resolver_auth_token(Some("test-resolver-token"));
        let result = super::send_image_request(
            &format!("http://{resolver_address}/admin/api/v2/video-phash/thumbnail?id=anonymous"),
            None,
        );
        assert!(result.is_ok());
        assert!(result.unwrap().status().is_success());

        let resolver_request = resolver_server.join().unwrap().to_ascii_lowercase();
        let cdn_request = cdn_server.join().unwrap().to_ascii_lowercase();
        assert!(resolver_request.contains("x-emby-token: test-resolver-token"));
        assert!(!cdn_request.contains("x-emby-token:"));
        super::set_resolver_auth_token(None);
        super::set_resolver_base_url("");
    }

    #[test]
    fn rejects_html_and_json_success_responses_as_invalid_image_responses() {
        assert!(is_non_image_content_type("text/html; charset=utf-8"));
        assert!(is_non_image_content_type("application/json"));
        assert!(is_non_image_content_type("application/problem+json"));
        assert!(is_non_image_content_type("application/xml"));
        assert!(!is_non_image_content_type("image/jpeg"));
        assert!(!is_non_image_content_type("application/octet-stream"));
        assert!(!is_non_image_content_type(""));
    }

    #[test]
    fn diagnostic_subject_identifies_items_without_credentials() {
        assert_eq!(
            super::diagnostic_subject(
                "https://media.example/Items/6ba7b810-9dad-11d1-80b4-00c04fd430c8/Images/Primary?tag=secret-tag&api_key=abc123"
            ),
            "thumbnail for item 6ba7b810 (Primary)"
        );
        assert_eq!(
            super::diagnostic_subject(
                "https://resolver.example/admin/api/v2/video-phash/thumbnail?video_id=telegram%3Ac%3A1"
            ),
            "thumbnail for media telegram:c:1"
        );
        let fallback = super::diagnostic_subject("https://cdn.example/poster.jpg?token=xyz");
        assert_eq!(fallback, "thumbnail from cdn.example");
        assert!(!fallback.contains("xyz"));
        assert_eq!(super::diagnostic_subject("not a url"), "thumbnail");
    }

    #[test]
    fn retry_invalidates_tagged_and_untagged_cache_keys() {
        let variants = cache_url_variants(
            "https://media.example/Items/abc/Images/Primary?tag=opaque&quality=90",
        );
        assert_eq!(variants.len(), 2);
        assert_eq!(
            variants[1],
            "https://media.example/Items/abc/Images/Primary?quality=90"
        );
        assert!(cache_url_variants("").is_empty());
    }

    #[test]
    fn retry_removes_existing_tagged_and_untagged_cache_entries() {
        let url = format!(
            "https://media.example/Items/retry-test/Images/Primary?tag=retry-{}&quality=90",
            std::process::id()
        );
        let paths = cache_url_variants(&url)
            .iter()
            .map(|variant| super::url_to_cache_path_without_create(variant))
            .collect::<Vec<_>>();
        for path in &paths {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, b"stale-error-cache-entry").unwrap();
        }

        super::invalidate_cached_image(&url);

        assert!(paths.iter().all(|path| !path.exists()));
    }

    #[test]
    fn access_denied_and_missing_are_not_retried() {
        assert!(!retryable_image_status(reqwest::StatusCode::FORBIDDEN));
        assert!(!retryable_image_status(reqwest::StatusCode::NOT_FOUND));
        assert!(!retryable_image_status(reqwest::StatusCode::UNAUTHORIZED));
        assert!(retryable_image_status(
            reqwest::StatusCode::TOO_MANY_REQUESTS
        ));
        assert!(retryable_image_status(
            reqwest::StatusCode::SERVICE_UNAVAILABLE
        ));
    }

    #[test]
    fn image_decoder_accepts_supported_images_and_rejects_invalid_or_oversized_data() {
        let mut valid = Cursor::new(Vec::new());
        image::DynamicImage::new_rgba8(2, 2)
            .write_to(&mut valid, image::ImageFormat::Png)
            .unwrap();
        assert!(decode_dynamic_image(valid.get_ref()).is_some());
        assert!(decode_dynamic_image(b"<html>not an image</html>").is_none());

        let mut oversized = Cursor::new(Vec::new());
        image::DynamicImage::new_rgba8(8193, 1)
            .write_to(&mut oversized, image::ImageFormat::Png)
            .unwrap();
        assert!(decode_dynamic_image(oversized.get_ref()).is_none());
    }

    #[test]
    fn corrupt_cache_recovery_removes_only_the_bad_entry() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let directory = std::env::temp_dir().join(format!(
            "adult-hub-thumbnail-corrupt-cache-{}-{unique}",
            std::process::id()
        ));
        std::fs::create_dir_all(&directory).unwrap();
        let corrupt = directory.join("corrupt.png");
        let neighbor = directory.join("unrelated.png");
        std::fs::write(&corrupt, b"not an image").unwrap();
        std::fs::write(&neighbor, b"leave this cache entry alone").unwrap();

        super::discard_corrupt_cache_entry(
            "https://media.example/Items/12345678-1234-4234-8234-123456789012/Images/Primary",
            &corrupt,
        );

        assert!(!corrupt.exists());
        assert!(neighbor.exists());
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn cache_limit_label_keeps_the_stored_megabytes() {
        assert_eq!(super::format_cache_limit(0), "Off (RAM only)");
        assert_eq!(super::format_cache_limit(-1), "Off (RAM only)");
        assert_eq!(super::format_cache_limit(512), "512 MB");
        assert_eq!(super::format_cache_limit(1024), "1 GB");
        assert_eq!(super::format_cache_limit(1536), "1.5 GB");
        assert_eq!(super::format_cache_limit(20480), "20 GB");
    }

    #[test]
    fn atomic_cache_write_leaves_no_partial_entry_behind() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let directory = std::env::temp_dir().join(format!(
            "adult-hub-thumbnail-atomic-{}-{unique}",
            std::process::id()
        ));
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("thumbnail.png");

        assert!(super::write_cache_atomically(
            &path,
            b"complete image bytes"
        ));

        assert_eq!(std::fs::read(&path).unwrap(), b"complete image bytes");
        let leftovers = std::fs::read_dir(&directory)
            .unwrap()
            .flatten()
            .filter(|entry| entry.path() != path)
            .count();
        assert_eq!(leftovers, 0);
        std::fs::remove_dir_all(directory).unwrap();
    }
}
