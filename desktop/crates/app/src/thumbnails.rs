use std::io::{Cursor, Read};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock, RwLock};
use std::time::{Duration, UNIX_EPOCH};

const DEFAULT_CACHE_LIMIT_MB: usize = 256;
const MAX_IMAGE_BYTES: usize = 10 * 1024 * 1024;

static CACHE_LIMIT_MB: AtomicUsize = AtomicUsize::new(DEFAULT_CACHE_LIMIT_MB);
static LOW_RAM_MODE: AtomicBool = AtomicBool::new(false);
static CACHE_WRITE_LOCK: Mutex<()> = Mutex::new(());
static RESOLVER_BASE_URL: OnceLock<RwLock<String>> = OnceLock::new();
static JELLYFIN_DEVICE_ID: OnceLock<RwLock<String>> = OnceLock::new();

pub fn set_jellyfin_device_id(device_id: &str) {
    if let Ok(mut current) = jellyfin_device_id().write() {
        *current = device_id.to_string();
    }
}

fn jellyfin_device_id() -> &'static RwLock<String> {
    JELLYFIN_DEVICE_ID.get_or_init(|| RwLock::new("adult-hub-desktop".into()))
}

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

/// Remove every persisted thumbnail while preserving the configured cache limit.
pub fn clear_cache() {
    let Ok(_guard) = CACHE_WRITE_LOCK.lock() else {
        return;
    };
    clear_cache_files();
}

pub fn set_low_ram_mode(enabled: bool) {
    LOW_RAM_MODE.store(enabled, Ordering::Relaxed);
}

pub fn thumbnail_max_width() -> u32 {
    if LOW_RAM_MODE.load(Ordering::Relaxed) {
        320
    } else {
        480
    }
}

pub fn person_thumbnail_max_width() -> u32 {
    if LOW_RAM_MODE.load(Ordering::Relaxed) {
        200
    } else {
        300
    }
}

pub fn set_resolver_base_url(base_url: &str) {
    if let Ok(mut current) = resolver_base_url().write() {
        *current = base_url.trim().trim_end_matches('/').to_string();
    }
}

fn resolver_base_url() -> &'static RwLock<String> {
    RESOLVER_BASE_URL.get_or_init(|| RwLock::new(String::new()))
}

pub fn phash_thumbnail_url(video_id: &str) -> Option<String> {
    let base = resolver_base_url().read().ok()?.clone();
    let video_id = video_id.trim();
    if base.is_empty() || !video_id.starts_with("telegram:") {
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
    if value.is_empty() {
        return None;
    }
    if let Ok(image_url) = url::Url::parse(value) {
        if image_url.path().starts_with("/admin/") && !base.is_empty() {
            let mut configured_origin = url::Url::parse(base).ok()?;
            if !matches!(configured_origin.scheme(), "http" | "https") {
                return None;
            }
            configured_origin.set_path(image_url.path());
            configured_origin.set_query(image_url.query());
            configured_origin.set_fragment(image_url.fragment());
            return Some(configured_origin.into());
        }
        return matches!(image_url.scheme(), "http" | "https").then(|| image_url.to_string());
    }

    if base.is_empty() {
        return None;
    }
    let mut base_url = url::Url::parse(base).ok()?;
    if !matches!(base_url.scheme(), "http" | "https") {
        return None;
    }
    if !base_url.path().ends_with('/') {
        let path = format!("{}/", base_url.path());
        base_url.set_path(&path);
    }
    let resolved = base_url.join(value).ok()?;
    matches!(resolved.scheme(), "http" | "https").then(|| resolved.to_string())
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

fn decode_image(bytes: &[u8]) -> Option<(slint::Image, Vec<u8>)> {
    let decoded = image::load_from_memory(bytes).ok()?.into_rgba8();
    let pixels = slint::SharedPixelBuffer::<slint::Rgba8Pixel>::clone_from_slice(
        decoded.as_raw(),
        decoded.width(),
        decoded.height(),
    );
    let slint_image = slint::Image::from_rgba8(pixels);

    let mut encoded_png = Cursor::new(Vec::new());
    image::DynamicImage::ImageRgba8(decoded)
        .write_to(&mut encoded_png, image::ImageFormat::Png)
        .ok()?;

    Some((slint_image, encoded_png.into_inner()))
}

pub fn url_to_cache_path(url: &str) -> PathBuf {
    let hash = ring::digest::digest(&ring::digest::SHA256, url.as_bytes());
    let hex: String = hash.as_ref().iter().map(|b| format!("{:02x}", b)).collect();
    cache_dir().join(format!("{hex}.png"))
}

fn http_client() -> Option<&'static reqwest::blocking::Client> {
    static CLIENT: OnceLock<Option<reqwest::blocking::Client>> = OnceLock::new();
    CLIENT
        .get_or_init(|| {
            reqwest::blocking::Client::builder()
                .timeout(Duration::from_secs(8))
                // Handle redirects explicitly: reqwest cannot know that the
                // custom X-Emby-Token header is a credential.
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .ok()
        })
        .as_ref()
}

enum ImageRequestError {
    Transport(reqwest::Error),
    InvalidRedirect,
    RedirectLimit,
}

fn send_image_request(
    url: &str,
    token: Option<&str>,
) -> Result<reqwest::blocking::Response, ImageRequestError> {
    let original = url::Url::parse(url).map_err(|_| ImageRequestError::InvalidRedirect)?;
    let mut current = original.clone();
    for redirect_count in 0..=5 {
        let mut request = http_client()
            .ok_or(ImageRequestError::InvalidRedirect)?
            .get(current.clone());
        // Keep credentials on the original Jellyfin origin only. A redirect to
        // a CDN or any other host is fetched without Authorization headers.
        if current.origin() == original.origin()
            && is_jellyfin_item_image_url(current.as_str())
            && let Some(token) = token.filter(|token| !token.is_empty())
        {
            let device_id = jellyfin_device_id()
                .read()
                .map(|value| value.clone())
                .unwrap_or_else(|_| "adult-hub-desktop".into());
            let authorization = format!(
                "MediaBrowser Client=\"{}\", Device=\"Desktop\", DeviceId=\"{}\", Version=\"{}\", Token=\"{}\"",
                adult_hub_client::CLIENT_NAME,
                device_id,
                env!("CARGO_PKG_VERSION"),
                token
            );
            request = request
                .header("X-Emby-Authorization", &authorization)
                .header(reqwest::header::AUTHORIZATION, &authorization)
                .header("X-Emby-Token", token);
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

#[cfg(test)]
mod tests {
    use super::resolve_resolver_thumbnail_url_at;

    #[test]
    fn resolves_absolute_resolver_admin_urls_to_configured_origin() {
        let resolved = resolve_resolver_thumbnail_url_at(
            "https://old.example/admin/api/v2/video-phash/thumbnail?video_id=telegram%3Ac%3A1",
            "https://resolver.example/prefix",
        );
        assert_eq!(
            resolved.as_deref(),
            Some(
                "https://resolver.example/admin/api/v2/video-phash/thumbnail?video_id=telegram%3Ac%3A1"
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
    }

    #[test]
    fn rejects_non_http_image_schemes() {
        assert_eq!(
            resolve_resolver_thumbnail_url_at("file:///tmp/poster.jpg", "https://resolver.example"),
            None
        );
    }
}

pub fn load_cached_image(url: &str) -> Option<slint::Image> {
    if url.trim().is_empty() {
        return None;
    }
    let path = url_to_cache_path(url);
    if path.is_file() {
        slint::Image::load_from_path(&path).ok()
    } else {
        None
    }
}

/// Retrieve an already persisted thumbnail as Send-safe bytes.
pub fn load_cached_image_bytes(url: &str) -> Option<Vec<u8>> {
    if url.trim().is_empty() {
        return None;
    }
    let bytes = std::fs::read(url_to_cache_path(url)).ok()?;
    image::load_from_memory(&bytes).ok()?;
    Some(bytes)
}

/// Create a Slint image from encoded bytes. Call this on the UI thread; Slint
/// image handles are deliberately not sent across worker threads.
pub fn image_from_bytes(bytes: &[u8]) -> Option<slint::Image> {
    decode_image(bytes).map(|(image, _)| image)
}

/// Mirrors the Android client check for missing or unhelpfully dark artwork.
pub fn is_poor_thumbnail(url: &str) -> bool {
    let path = url_to_cache_path(url);
    let Ok(bytes) = std::fs::read(path) else {
        return false;
    };
    is_poor_thumbnail_bytes(&bytes)
}

pub fn is_poor_thumbnail_bytes(bytes: &[u8]) -> bool {
    let Ok(image) = image::load_from_memory(bytes) else {
        return false;
    };
    if (1..320).contains(&image.width()) || (1..160).contains(&image.height()) {
        return true;
    }

    let pixels = image
        .resize_exact(24, 24, image::imageops::FilterType::Nearest)
        .to_rgba8();
    let mut count = 0usize;
    let mut dark_count = 0usize;
    let mut luminance_sum = 0.0f64;
    let mut luminance_squared_sum = 0.0f64;
    for y in (0..24).step_by(2) {
        for x in (0..24).step_by(2) {
            let pixel = pixels.get_pixel(x, y).0;
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

/// Fetch an image and return its normalized PNG bytes for worker-to-UI transfer.
/// This works even when persistent caching is disabled.
pub fn fetch_and_cache_image_bytes(url: &str, token: Option<&str>) -> Option<Vec<u8>> {
    fetch_and_cache_image_data(url, token, true).map(|(_, bytes)| bytes)
}

/// Fetch a thumbnail from its server even when a cached copy is available.
/// Used by the connection diagnostic so it reports network image availability.
pub fn fetch_fresh_image(url: &str, token: Option<&str>) -> Option<slint::Image> {
    fetch_and_cache_image_impl(url, token, false)
}

fn fetch_and_cache_image_impl(
    url: &str,
    token: Option<&str>,
    allow_cached_image: bool,
) -> Option<slint::Image> {
    fetch_and_cache_image_data(url, token, allow_cached_image).map(|(image, _)| image)
}

fn fetch_and_cache_image_data(
    url: &str,
    token: Option<&str>,
    allow_cached_image: bool,
) -> Option<(slint::Image, Vec<u8>)> {
    let url = url.trim();
    if !(url.starts_with("http://") || url.starts_with("https://")) {
        return None;
    }
    let path = url_to_cache_path(url);
    if allow_cached_image && path.is_file() {
        if let Ok(cached_bytes) = std::fs::read(&path)
            && let Some((image, normalized_bytes)) = decode_image(&cached_bytes)
        {
            return Some((image, normalized_bytes));
        }
    }

    let mut downloaded_bytes = None;
    for attempt in 0..3 {
        match send_image_request(url, token) {
            Ok(resp) => {
                let status = resp.status();
                if status.is_success() {
                    if resp
                        .content_length()
                        .is_some_and(|size| size > MAX_IMAGE_BYTES as u64)
                    {
                        crate::diagnostics::GLOBAL_LOG
                            .w("Thumbnail", "image exceeded the 10 MB limit");
                        return None;
                    }
                    let mut bytes = Vec::new();
                    let mut limited_response = resp.take(MAX_IMAGE_BYTES as u64 + 1);
                    match limited_response.read_to_end(&mut bytes) {
                        Ok(_) if !bytes.is_empty() && bytes.len() <= MAX_IMAGE_BYTES => {
                            downloaded_bytes = Some(bytes);
                            break;
                        }
                        Ok(_) if bytes.len() > MAX_IMAGE_BYTES => {
                            crate::diagnostics::GLOBAL_LOG
                                .w("Thumbnail", "image exceeded the 10 MB limit");
                            return None;
                        }
                        Ok(_) if attempt < 2 => {
                            std::thread::sleep(Duration::from_millis(180 * (attempt + 1) as u64));
                            continue;
                        }
                        Ok(_) => {
                            crate::diagnostics::GLOBAL_LOG
                                .w("Thumbnail", "image response was empty");
                            return None;
                        }
                        Err(_) if attempt < 2 => {
                            std::thread::sleep(Duration::from_millis(180 * (attempt + 1) as u64));
                            continue;
                        }
                        Err(_) => {
                            crate::diagnostics::GLOBAL_LOG
                                .w("Thumbnail", "failed to read image response");
                            return None;
                        }
                    }
                }
                let retryable = status == reqwest::StatusCode::REQUEST_TIMEOUT
                    || status == reqwest::StatusCode::TOO_MANY_REQUESTS
                    || status.is_server_error();
                if retryable && attempt < 2 {
                    std::thread::sleep(Duration::from_millis(180 * (attempt + 1) as u64));
                    continue;
                }
                let image_type = jellyfin_image_type(url)
                    .map(|image_type| format!("{image_type} "))
                    .unwrap_or_default();
                crate::diagnostics::GLOBAL_LOG.w(
                    "Thumbnail",
                    &format!("{image_type}image server returned HTTP {}", status.as_u16()),
                );
                return None;
            }
            Err(ImageRequestError::Transport(error)) => {
                let retryable = error.is_timeout() || error.is_connect() || error.is_request();
                if retryable && attempt < 2 {
                    std::thread::sleep(Duration::from_millis(180 * (attempt + 1) as u64));
                    continue;
                }
                let reason = if error.is_timeout() {
                    "request timed out"
                } else if error.is_connect() {
                    "could not connect to image server"
                } else {
                    "request failed"
                };
                crate::diagnostics::GLOBAL_LOG.w("Thumbnail", reason);
                return None;
            }
            Err(ImageRequestError::InvalidRedirect) => {
                crate::diagnostics::GLOBAL_LOG.w(
                    "Thumbnail",
                    "image server returned an invalid or unsafe redirect",
                );
                return None;
            }
            Err(ImageRequestError::RedirectLimit) => {
                crate::diagnostics::GLOBAL_LOG
                    .w("Thumbnail", "image server exceeded the redirect limit");
                return None;
            }
        }
    }
    let bytes = downloaded_bytes?;
    let (image, cache_bytes) = decode_image(&bytes).or_else(|| {
        let image_type = jellyfin_image_type(url)
            .map(|image_type| format!("{image_type} "))
            .unwrap_or_default();
        crate::diagnostics::GLOBAL_LOG.w(
            "Thumbnail",
            &format!("{image_type}image response could not be decoded (unsupported format or invalid data)"),
        );
        None
    })?;

    let image_type = jellyfin_image_type(url)
        .map(|image_type| format!("{image_type} "))
        .unwrap_or_default();
    crate::diagnostics::GLOBAL_LOG.i(
        "Thumbnail",
        &format!(
            "{image_type}image downloaded and decoded successfully ({} bytes)",
            bytes.len()
        ),
    );

    let _guard = CACHE_WRITE_LOCK.lock().ok()?;
    let limit_mb = CACHE_LIMIT_MB.load(Ordering::Relaxed);
    if limit_mb == 0 {
        return Some((image, cache_bytes));
    }

    let limit_bytes = limit_mb.saturating_mul(1024 * 1024);
    if cache_bytes.len() > limit_bytes {
        return Some((image, cache_bytes));
    }

    prune_cache(limit_bytes.saturating_sub(cache_bytes.len()), Some(&path));
    if std::fs::write(&path, &cache_bytes).is_err() {
        return Some((image, cache_bytes));
    }
    prune_cache(limit_bytes, Some(&path));
    Some((image, cache_bytes))
}

fn is_jellyfin_item_image_url(url: &str) -> bool {
    let Ok(parsed) = url::Url::parse(url) else {
        return false;
    };
    let segments = parsed
        .path_segments()
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();
    segments.windows(3).any(|window| {
        window[0].eq_ignore_ascii_case("Items")
            && !window[1].is_empty()
            && window[2].eq_ignore_ascii_case("Images")
    })
}

fn jellyfin_image_type(url: &str) -> Option<String> {
    let parsed = url::Url::parse(url).ok()?;
    let segments = parsed.path_segments()?.collect::<Vec<_>>();
    let image_segment = segments
        .windows(3)
        .find(|window| {
            window[0].eq_ignore_ascii_case("Items")
                && !window[1].is_empty()
                && window[2].eq_ignore_ascii_case("Images")
        })
        .and_then(|_| {
            let image_index = segments
                .iter()
                .position(|segment| segment.eq_ignore_ascii_case("Images"))?;
            segments.get(image_index + 1)
        })?;
    Some(image_segment.to_string())
}
