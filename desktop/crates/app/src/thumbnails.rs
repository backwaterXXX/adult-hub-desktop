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
static RESOLVER_AUTH_TOKEN: OnceLock<RwLock<Option<String>>> = OnceLock::new();
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

/// Remove every persisted thumbnail while preserving the configured cache limit.
pub fn clear_cache() {
    let Ok(_guard) = CACHE_WRITE_LOCK.lock() else {
        return;
    };
    clear_cache_files();
}

/// Remove the exact image cache entries used for a card before an explicit
/// retry. Jellyfin may serve a tagged image URL and an untagged fallback.
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

/// Keep the active Jellyfin token available for Resolver image endpoints that
/// use the same authentication contract as Resolver pHash operations. The
/// request layer only sends this token to the configured Resolver origin.
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
        if !image_url.username().is_empty() || image_url.password().is_some() {
            return None;
        }
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

fn decode_image(bytes: &[u8]) -> Option<(slint::Image, Vec<u8>)> {
    let decoded = decode_dynamic_image(bytes)?.into_rgba8();
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

fn url_to_cache_path_without_create(url: &str) -> PathBuf {
    let hash = ring::digest::digest(&ring::digest::SHA256, url.as_bytes());
    let hex: String = hash
        .as_ref()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    cache_dir_path().join(format!("{hex}.png"))
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
        if is_resolver_origin_request(&original, &current, &resolver_base)
            && !is_jellyfin_item_image_url(current.as_str())
        {
            let resolver_token = resolver_auth_token()
                .read()
                .ok()
                .and_then(|value| value.clone())
                .or_else(|| token.map(str::to_owned));
            if let Some(token) = resolver_token.filter(|value| !value.is_empty()) {
                request = request.header("X-Emby-Token", token);
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
    decode_dynamic_image(&bytes)?;
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
    let Some(image) = decode_dynamic_image(bytes) else {
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
    if allow_cached_image
        && path.is_file()
        && let Ok(cached_bytes) = std::fs::read(&path)
        && let Some((image, normalized_bytes)) = decode_image(&cached_bytes)
    {
        return Some((image, normalized_bytes));
    }

    let mut downloaded_bytes = None;
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
                        crate::diagnostics::GLOBAL_LOG.w(
                            "Thumbnail",
                            &format!(
                                "{}: image endpoint returned non-image content type ({})",
                                diagnostic_subject(url),
                                content_type.split(';').next().unwrap_or("unknown").trim()
                            ),
                        );
                        return None;
                    }
                    if resp
                        .content_length()
                        .is_some_and(|size| size > MAX_IMAGE_BYTES as u64)
                    {
                        crate::diagnostics::GLOBAL_LOG.w(
                            "Thumbnail",
                            &format!(
                                "{}: image exceeded the 10 MB limit",
                                diagnostic_subject(url)
                            ),
                        );
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
                            crate::diagnostics::GLOBAL_LOG.w(
                                "Thumbnail",
                                &format!(
                                    "{}: image exceeded the 10 MB limit",
                                    diagnostic_subject(url)
                                ),
                            );
                            return None;
                        }
                        Ok(_) if attempt < 1 => {
                            std::thread::sleep(Duration::from_millis(180 * (attempt + 1) as u64));
                            continue;
                        }
                        Ok(_) => {
                            crate::diagnostics::GLOBAL_LOG.w(
                                "Thumbnail",
                                &format!("{}: image response was empty", diagnostic_subject(url)),
                            );
                            return None;
                        }
                        Err(_) if attempt < 1 => {
                            std::thread::sleep(Duration::from_millis(180 * (attempt + 1) as u64));
                            continue;
                        }
                        Err(_) => {
                            crate::diagnostics::GLOBAL_LOG.w(
                                "Thumbnail",
                                &format!(
                                    "{}: failed to read image response",
                                    diagnostic_subject(url)
                                ),
                            );
                            return None;
                        }
                    }
                }
                let retryable = retryable_image_status(status);
                if retryable && attempt < 1 {
                    std::thread::sleep(Duration::from_millis(180 * (attempt + 1) as u64));
                    continue;
                }
                crate::diagnostics::GLOBAL_LOG.w(
                    "Thumbnail",
                    &format!(
                        "{}: image server returned HTTP {}",
                        diagnostic_subject(url),
                        status.as_u16()
                    ),
                );
                return None;
            }
            Err(ImageRequestError::Transport(error)) => {
                let retryable = error.is_timeout() || error.is_connect() || error.is_request();
                if retryable && attempt < 1 {
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
                crate::diagnostics::GLOBAL_LOG.w(
                    "Thumbnail",
                    &format!("{}: {reason}", diagnostic_subject(url)),
                );
                return None;
            }
            Err(ImageRequestError::InvalidRedirect) => {
                crate::diagnostics::GLOBAL_LOG.w(
                    "Thumbnail",
                    &format!(
                        "{}: image server returned an invalid or unsafe redirect",
                        diagnostic_subject(url)
                    ),
                );
                return None;
            }
            Err(ImageRequestError::RedirectLimit) => {
                crate::diagnostics::GLOBAL_LOG.w(
                    "Thumbnail",
                    &format!(
                        "{}: image server exceeded the redirect limit",
                        diagnostic_subject(url)
                    ),
                );
                return None;
            }
        }
    }
    let bytes = downloaded_bytes?;
    let (image, cache_bytes) = decode_image(&bytes).or_else(|| {
        crate::diagnostics::GLOBAL_LOG.w(
            "Thumbnail",
            &format!(
                "{}: image response could not be decoded (unsupported format or invalid data)",
                diagnostic_subject(url)
            ),
        );
        None
    })?;

    crate::diagnostics::GLOBAL_LOG.i(
        "Thumbnail",
        &format!(
            "{}: image downloaded and decoded successfully ({} bytes)",
            diagnostic_subject(url),
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

/// Compact, credential-free identity of the image subject for diagnostics.
/// Jellyfin/Person image URLs contribute the leading item GUID characters and
/// the image type; everything else contributes only the host. Query parameters
/// (image tags, api keys) are never included.
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
        assert_eq!(
            resolve_resolver_thumbnail_url_at(
                "https://user:secret@cdn.example/poster.jpg",
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
        let _guard = RESOLVER_REQUEST_TEST_LOCK.lock().unwrap();
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
                "https://jellyfin.example/Items/6ba7b810-9dad-11d1-80b4-00c04fd430c8/Images/Primary?tag=secret-tag&api_key=abc123"
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
            "https://jellyfin.example/Items/abc/Images/Primary?tag=opaque&quality=90",
        );
        assert_eq!(variants.len(), 2);
        assert_eq!(
            variants[1],
            "https://jellyfin.example/Items/abc/Images/Primary?quality=90"
        );
        assert!(cache_url_variants("").is_empty());
    }

    #[test]
    fn retry_removes_existing_tagged_and_untagged_cache_entries() {
        let url = format!(
            "https://jellyfin.example/Items/retry-test/Images/Primary?tag=retry-{}&quality=90",
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
}
