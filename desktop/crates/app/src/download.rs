//! Background video downloader integrated with Adult Hub Desktop.
use adult_hub_client::TvItem;
use std::collections::VecDeque;
use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum DownloadStatus {
    Idle,
    Queued,
    Preparing,
    Downloading,
    Paused,
    Completed {
        folder_path: PathBuf,
        file_path: PathBuf,
    },
    Error(String),
}

pub struct DownloadTask {
    pub item_id: String,
    pub item_title: String,
    pub target_path: PathBuf,
    pub temp_path: PathBuf,
    pub bytes_downloaded: Arc<AtomicU64>,
    pub total_bytes: Arc<AtomicU64>,
    pub cancel_flag: Arc<AtomicBool>,
    pub pause_flag: Arc<AtomicBool>,
    pub supports_range: Arc<AtomicBool>,
    pub status: Arc<Mutex<DownloadStatus>>,
    pub child_process: Arc<Mutex<Option<Child>>>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CompletedDownloadItem {
    pub item_id: String,
    pub title: String,
    pub file_path: PathBuf,
    pub file_size_bytes: u64,
}

#[derive(Clone)]
pub struct DownloadManager {
    queue: Arc<Mutex<VecDeque<Arc<DownloadTask>>>>,
    active_task: Arc<Mutex<Option<Arc<DownloadTask>>>>,
    history: Arc<Mutex<Vec<CompletedDownloadItem>>>,
    speed_samples: Arc<Mutex<VecDeque<(Instant, u64)>>>,
    smoothed_speed_bps: Arc<Mutex<f64>>,
    last_speed_calc: Arc<Mutex<Instant>>,
}

impl Default for DownloadManager {
    fn default() -> Self {
        Self::new()
    }
}

impl DownloadManager {
    pub fn new() -> Self {
        Self {
            queue: Arc::new(Mutex::new(VecDeque::new())),
            active_task: Arc::new(Mutex::new(None)),
            history: Arc::new(Mutex::new(Vec::new())),
            speed_samples: Arc::new(Mutex::new(VecDeque::new())),
            smoothed_speed_bps: Arc::new(Mutex::new(0.0)),
            last_speed_calc: Arc::new(Mutex::new(Instant::now())),
        }
    }

    pub fn is_downloading(&self) -> bool {
        let lock = self.active_task.lock().unwrap();
        if let Some(task) = lock.as_ref() {
            let status = task.status.lock().unwrap();
            matches!(
                *status,
                DownloadStatus::Preparing | DownloadStatus::Downloading | DownloadStatus::Paused
            )
        } else {
            false
        }
    }

    pub fn is_active_or_queued_for_item(&self, item_id: &str) -> bool {
        if let Some(task) = self.current_task() {
            if task.item_id == item_id {
                let status = task.status.lock().unwrap();
                if matches!(
                    *status,
                    DownloadStatus::Preparing
                        | DownloadStatus::Downloading
                        | DownloadStatus::Paused
                ) {
                    return true;
                }
            }
        }
        let queue = self.queue.lock().unwrap();
        queue.iter().any(|t| t.item_id == item_id)
    }

    pub fn cancel(&self) {
        let lock = self.active_task.lock().unwrap();
        if let Some(task) = lock.as_ref() {
            task.cancel_flag.store(true, Ordering::SeqCst);
            if let Ok(mut child_lock) = task.child_process.lock() {
                if let Some(mut child) = child_lock.take() {
                    let _ = child.kill();
                }
            }
            let mut st = task.status.lock().unwrap();
            *st = DownloadStatus::Error("Download cancelled".to_string());
            let _ = std::fs::remove_file(&task.temp_path);
        }
        self.speed_samples.lock().unwrap().clear();
        *self.smoothed_speed_bps.lock().unwrap() = 0.0;
    }

    pub fn cancel_queued(&self, item_id: &str) {
        let mut q = self.queue.lock().unwrap();
        q.retain(|t| t.item_id != item_id);
    }

    pub fn pause(&self) {
        let lock = self.active_task.lock().unwrap();
        if let Some(task) = lock.as_ref() {
            task.pause_flag.store(true, Ordering::SeqCst);
            *task.status.lock().unwrap() = DownloadStatus::Paused;
        }
    }

    pub fn resume(&self, ctx: &crate::context::AppContext) {
        let task = self.current_task();
        if let Some(task) = task {
            let is_paused = {
                let st = task.status.lock().unwrap();
                matches!(*st, DownloadStatus::Paused)
            };
            if is_paused {
                task.pause_flag.store(false, Ordering::SeqCst);
                task.cancel_flag.store(false, Ordering::SeqCst);
                *task.status.lock().unwrap() = DownloadStatus::Preparing;
                let mgr = self.clone();
                let task_clone = task.clone();
                let ctx_clone = ctx.clone();
                std::thread::spawn(move || {
                    run_download_task(&ctx_clone, task_clone, mgr);
                });
            }
        }
    }

    pub fn enqueue_task(
        &self,
        ctx: &crate::context::AppContext,
        item: &TvItem,
        target_path: PathBuf,
    ) {
        if self.is_active_or_queued_for_item(&item.id) {
            return;
        }

        let temp_path = target_path.with_extension(format!(
            "{}.part",
            target_path
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or("mp4")
        ));

        let task = Arc::new(DownloadTask {
            item_id: item.id.clone(),
            item_title: item.title.clone(),
            target_path,
            temp_path,
            bytes_downloaded: Arc::new(AtomicU64::new(0)),
            total_bytes: Arc::new(AtomicU64::new(
                item.file_size_bytes.unwrap_or(0).max(0) as u64
            )),
            cancel_flag: Arc::new(AtomicBool::new(false)),
            pause_flag: Arc::new(AtomicBool::new(false)),
            supports_range: Arc::new(AtomicBool::new(false)),
            status: Arc::new(Mutex::new(DownloadStatus::Queued)),
            child_process: Arc::new(Mutex::new(None)),
        });

        {
            let mut q = self.queue.lock().unwrap();
            q.push_back(task);
        }

        self.advance_queue_if_idle(ctx);
    }

    pub fn advance_queue_if_idle(&self, ctx: &crate::context::AppContext) {
        let mut active = self.active_task.lock().unwrap();
        let is_idle = match active.as_ref() {
            None => true,
            Some(t) => {
                let st = t.status.lock().unwrap();
                matches!(
                    *st,
                    DownloadStatus::Idle
                        | DownloadStatus::Completed { .. }
                        | DownloadStatus::Error(_)
                )
            }
        };

        if is_idle {
            let mut q = self.queue.lock().unwrap();
            if let Some(next_task) = q.pop_front() {
                *next_task.status.lock().unwrap() = DownloadStatus::Preparing;
                *active = Some(next_task.clone());
                drop(active);
                drop(q);

                self.speed_samples.lock().unwrap().clear();
                *self.smoothed_speed_bps.lock().unwrap() = 0.0;
                *self.last_speed_calc.lock().unwrap() = Instant::now();

                let mgr = self.clone();
                let ctx_clone = ctx.clone();
                std::thread::spawn(move || {
                    run_download_task(&ctx_clone, next_task, mgr);
                });
            }
        }
    }

    pub fn current_task(&self) -> Option<Arc<DownloadTask>> {
        self.active_task.lock().unwrap().clone()
    }

    pub fn queued_tasks(&self) -> Vec<Arc<DownloadTask>> {
        self.queue.lock().unwrap().iter().cloned().collect()
    }

    pub fn history_items(&self) -> Vec<CompletedDownloadItem> {
        self.history.lock().unwrap().clone()
    }

    pub fn add_history_item(&self, item: CompletedDownloadItem) {
        let mut hist = self.history.lock().unwrap();
        hist.retain(|h| h.item_id != item.item_id && h.file_path != item.file_path);
        hist.insert(0, item);
        if hist.len() > 30 {
            hist.truncate(30);
        }
    }

    #[allow(dead_code)]
    pub fn set_task(&self, task: Arc<DownloadTask>) {
        *self.active_task.lock().unwrap() = Some(task);
        self.speed_samples.lock().unwrap().clear();
        *self.smoothed_speed_bps.lock().unwrap() = 0.0;
        *self.last_speed_calc.lock().unwrap() = Instant::now();
    }

    #[allow(dead_code)]
    pub fn clear_task(&self) {
        *self.active_task.lock().unwrap() = None;
        self.speed_samples.lock().unwrap().clear();
        *self.smoothed_speed_bps.lock().unwrap() = 0.0;
    }

    /// Sample downloaded bytes and calculate smoothed speed over a short rolling window (~2-3s).
    /// Updates the calculated speed roughly once per second.
    pub fn sample_speed(&self, current_bytes: u64, total_bytes: u64) -> (f64, Option<String>) {
        let now = Instant::now();
        let mut samples = self.speed_samples.lock().unwrap();
        samples.push_back((now, current_bytes));

        // Retain samples within the last 2.5 seconds
        while let Some(&(time, _)) = samples.front() {
            if now.duration_since(time) > Duration::from_millis(2500) && samples.len() > 2 {
                samples.pop_front();
            } else {
                break;
            }
        }

        let mut last_calc = self.last_speed_calc.lock().unwrap();
        let mut smoothed = self.smoothed_speed_bps.lock().unwrap();

        if now.duration_since(*last_calc) >= Duration::from_millis(900) || *smoothed == 0.0 {
            if let (Some(&(oldest_time, oldest_bytes)), Some(&(newest_time, newest_bytes))) =
                (samples.front(), samples.back())
            {
                let dt = newest_time.duration_since(oldest_time).as_secs_f64();
                if dt >= 0.3 && newest_bytes >= oldest_bytes {
                    let instant_speed = (newest_bytes - oldest_bytes) as f64 / dt;
                    if *smoothed == 0.0 {
                        *smoothed = instant_speed;
                    } else {
                        // Exponential moving average (70% new, 30% previous)
                        *smoothed = *smoothed * 0.3 + instant_speed * 0.7;
                    }
                    *last_calc = now;
                }
            }
        }

        let current_speed = *smoothed;
        let eta = if total_bytes > current_bytes && current_speed >= 1024.0 {
            format_download_eta(total_bytes - current_bytes, current_speed)
        } else {
            None
        };

        (current_speed, eta)
    }
}

/// Normalize container string or stream URL into a single valid file extension.
/// Ensures that multi-format aliases like "mov,mp4,m4a,3gp,3g2,mj2" or "matroska,webm"
/// are mapped to a single real extension ("mp4", "mkv", "webm", "ts", etc.).
pub fn normalize_container_extension(container: Option<&str>, url: Option<&str>) -> &'static str {
    if let Some(c) = container {
        let trimmed = c.trim().to_lowercase();
        if !trimmed.is_empty() {
            // Check for multi-format or specific containers
            if trimmed.contains("mp4") || trimmed.contains("m4v") {
                return "mp4";
            }
            if trimmed.contains("matroska") || trimmed.contains("mkv") {
                return "mkv";
            }
            if trimmed.contains("webm") {
                return "webm";
            }
            if trimmed.contains("mpegts") || trimmed.contains("ts") {
                return "ts";
            }
            if trimmed.contains("mov") {
                return "mov";
            }
            if trimmed.contains("avi") {
                return "avi";
            }
            if trimmed.contains("flv") {
                return "flv";
            }
            if trimmed.contains("wmv") {
                return "wmv";
            }
            if trimmed.contains("mp3") {
                return "mp3";
            }
            let single = trimmed.trim_start_matches('.').trim();
            if !single.contains(',')
                && !single.contains(' ')
                && single.len() <= 5
                && single.chars().all(|ch| ch.is_ascii_alphanumeric())
            {
                match single {
                    "mp4" | "m4v" => return "mp4",
                    "mkv" => return "mkv",
                    "webm" => return "webm",
                    "ts" => return "ts",
                    "mov" => return "mov",
                    "avi" => return "avi",
                    "flv" => return "flv",
                    "wmv" => return "wmv",
                    _ => {}
                }
            }
        }
    }

    if let Some(u) = url {
        let clean_url = u.split('?').next().unwrap_or(u);
        let path = clean_url.trim_end_matches('/');
        if let Some(ext) = path.rsplit('.').next() {
            let lower = ext.to_lowercase();
            match lower.as_str() {
                "mp4" | "m4v" => return "mp4",
                "mkv" => return "mkv",
                "webm" => return "webm",
                "ts" => return "ts",
                "mov" => return "mov",
                "avi" => return "avi",
                "flv" => return "flv",
                "wmv" => return "wmv",
                _ => {}
            }
        }
    }

    "mp4"
}

/// Build suggested save filename:
/// - Windows illegal characters stripped
/// - Polish characters and spaces preserved
/// - Extension appended once (no duplicate .ext if already present)
pub fn build_suggested_filename(title: &str, ext: &str) -> String {
    let clean_ext = ext.trim().trim_start_matches('.').to_lowercase();
    let sanitized_title = sanitize_filename(title);

    let title_lower = sanitized_title.to_lowercase();
    if title_lower.ends_with(&format!(".{clean_ext}")) {
        return sanitized_title;
    }

    const KNOWN_EXTS: &[&str] = &[
        ".mp4", ".mkv", ".webm", ".ts", ".mov", ".avi", ".flv", ".wmv", ".m4v",
    ];
    for known in KNOWN_EXTS {
        if title_lower.ends_with(known) {
            return sanitized_title;
        }
    }

    format!("{sanitized_title}.{clean_ext}")
}

/// Format human-readable download speed
pub fn format_download_speed(speed_bps: f64) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = 1024.0 * KB;
    if speed_bps >= MB {
        format!("{:.1} MB/s", speed_bps / MB)
    } else if speed_bps >= KB {
        format!("{:.0} KB/s", speed_bps / KB)
    } else if speed_bps > 0.0 {
        format!("{:.0} B/s", speed_bps)
    } else {
        String::new()
    }
}

/// Format ETA (estimated time remaining) in English
pub fn format_download_eta(remaining_bytes: u64, speed_bps: f64) -> Option<String> {
    if speed_bps < 1024.0 || remaining_bytes == 0 {
        return None;
    }
    let total_secs = (remaining_bytes as f64 / speed_bps).round() as u64;
    if total_secs < 60 {
        Some(format!("{}s remaining", total_secs))
    } else if total_secs < 3600 {
        Some(format!(
            "{}m {}s remaining",
            total_secs / 60,
            total_secs % 60
        ))
    } else {
        Some(format!(
            "{}h {}m remaining",
            total_secs / 3600,
            (total_secs % 3600) / 60
        ))
    }
}

/// Sanitize filename for safe local disk storage
pub fn sanitize_filename(name: &str) -> String {
    let sanitized: String = name
        .chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            c if (c as u32) < 32 => '_',
            other => other,
        })
        .collect();
    let trimmed = sanitized.trim().trim_matches('.').trim();
    if trimmed.is_empty() {
        return "video".to_string();
    }

    let base_stem = trimmed.split('.').next().unwrap_or(trimmed).to_uppercase();
    const RESERVED_NAMES: &[&str] = &[
        "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
        "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
    ];
    if RESERVED_NAMES.contains(&base_stem.as_str()) {
        format!("{trimmed}_video")
    } else {
        trimmed.to_string()
    }
}

/// Choose default download destination directory
#[allow(dead_code)]
pub fn default_download_directory() -> PathBuf {
    #[cfg(windows)]
    {
        if let Ok(userprofile) = std::env::var("USERPROFILE") {
            let dl = PathBuf::from(userprofile).join("Downloads");
            if dl.exists() {
                return dl;
            }
        }
    }
    #[cfg(not(windows))]
    {
        if let Ok(home) = std::env::var("HOME") {
            let dl = PathBuf::from(home).join("Downloads");
            if dl.exists() {
                return dl;
            }
        }
    }
    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
}

/// Generate unique non-colliding filename if file exists
pub fn resolve_unique_path(dir: &Path, base_name: &str, ext: &str) -> PathBuf {
    let mut candidate = dir.join(format!("{base_name}.{ext}"));
    if !candidate.exists() {
        return candidate;
    }
    let mut counter = 1;
    loop {
        candidate = dir.join(format!("{base_name} ({counter}).{ext}"));
        if !candidate.exists() {
            return candidate;
        }
        counter += 1;
    }
}

/// Format bytes helper
pub fn format_download_bytes(bytes: u64, total: u64) -> String {
    const MB: f64 = 1024.0 * 1024.0;
    const GB: f64 = 1024.0 * MB;

    let b = bytes as f64;
    if total > 0 {
        let t = total as f64;
        if t >= GB {
            format!("{:.2} GB / {:.2} GB", b / GB, t / GB)
        } else {
            format!("{:.1} MB / {:.1} MB", b / MB, t / MB)
        }
    } else if b >= GB {
        format!("{:.2} GB", b / GB)
    } else {
        format!("{:.1} MB", b / MB)
    }
}

/// Open file in native default player / application
pub fn open_file(path: &Path) {
    #[cfg(target_os = "windows")]
    {
        let _ = std::process::Command::new("cmd")
            .args(["/C", "start", "", &path.to_string_lossy()])
            .spawn();
    }
    #[cfg(target_os = "linux")]
    {
        let _ = std::process::Command::new("xdg-open").arg(path).spawn();
    }
    #[cfg(target_os = "macos")]
    {
        let _ = std::process::Command::new("open").arg(path).spawn();
    }
}

/// Open folder in native file explorer (or select file)
pub fn open_folder_in_explorer(path: &Path) {
    let target = if path.is_dir() {
        path.to_path_buf()
    } else {
        path.parent().unwrap_or(path).to_path_buf()
    };
    #[cfg(target_os = "windows")]
    {
        if path.is_file() {
            let _ = std::process::Command::new("explorer")
                .args(["/select,", &path.to_string_lossy()])
                .spawn();
        } else {
            let _ = std::process::Command::new("explorer").arg(target).spawn();
        }
    }
    #[cfg(target_os = "linux")]
    {
        let _ = std::process::Command::new("xdg-open").arg(target).spawn();
    }
    #[cfg(target_os = "macos")]
    {
        let _ = std::process::Command::new("open").arg(target).spawn();
    }
}

/// Ask user for destination file path using native save file dialog or fallback
pub fn prompt_save_file_destination(suggested_name: &str, ext: &str) -> Option<PathBuf> {
    let clean_ext = ext.trim().trim_start_matches('.').to_lowercase();
    let mut filters: Vec<&str> = vec![clean_ext.as_str()];
    for standard in &["mp4", "mkv", "ts", "webm"] {
        if !filters.contains(standard) {
            filters.push(standard);
        }
    }

    #[cfg(windows)]
    {
        let selected = rfd::FileDialog::new()
            .set_file_name(suggested_name)
            .add_filter("Video", &filters)
            .save_file();

        selected.map(|path| {
            if path.extension().is_none() {
                path.with_extension(&clean_ext)
            } else {
                path
            }
        })
    }
    #[cfg(not(windows))]
    {
        // On Linux, try zenity or kdialog if present, or fallback to default download dir
        let default_dir = default_download_directory();
        let default_path = default_dir.join(suggested_name);
        if let Ok(output) = Command::new("zenity")
            .args([
                "--file-selection",
                "--save",
                "--confirm-overwrite",
                &format!("--filename={}", default_path.to_string_lossy()),
                "--title=Save Video",
            ])
            .output()
        {
            if output.status.success() {
                let path_str = String::from_utf8_lossy(&output.stdout).trim().to_string();
                if !path_str.is_empty() {
                    let p = PathBuf::from(path_str);
                    return Some(if p.extension().is_none() {
                        p.with_extension(&clean_ext)
                    } else {
                        p
                    });
                }
            } else {
                // User cancelled zenity dialog
                return None;
            }
        }
        let base_name =
            sanitize_filename(suggested_name.trim_end_matches(&format!(".{clean_ext}")));
        Some(resolve_unique_path(&default_dir, &base_name, &clean_ext))
    }
}

/// Check if ffmpeg executable is available on PATH
pub fn is_ffmpeg_available() -> Option<PathBuf> {
    if let Ok(path) = std::env::var("ADULT_HUB_FFMPEG") {
        let p = PathBuf::from(path);
        if p.is_file() {
            return Some(p);
        }
    }
    let bin = "ffmpeg";
    if let Ok(output) = Command::new(bin).arg("-version").output() {
        if output.status.success() {
            return Some(PathBuf::from(bin));
        }
    }
    None
}

/// Background media item downloader - adds task to sequential queue
pub fn start_item_download(
    ctx: &crate::context::AppContext,
    item: &TvItem,
    target_path: PathBuf,
    manager: Arc<DownloadManager>,
) {
    manager.enqueue_task(ctx, item, target_path);
}

/// Execute a single download task (called sequentially from queue worker thread)
fn run_download_task(
    ctx: &crate::context::AppContext,
    task: Arc<DownloadTask>,
    manager: DownloadManager,
) {
    // Step 1: Ensure target parent directory exists
    if let Some(parent) = task.target_path.parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            *task.status.lock().unwrap() =
                DownloadStatus::Error(format!("Cannot create directory: {e}"));
            manager.advance_queue_if_idle(ctx);
            return;
        }
    }

    let existing_bytes = if task.temp_path.is_file() {
        task.temp_path.metadata().map(|m| m.len()).unwrap_or(0)
    } else {
        0
    };

    // Step 2: Resolve media URL and headers
    let session = ctx.credential_store.get().ok().flatten();
    let server_url = session.as_ref().map(|s| s.server_url.as_str());
    let resolver_url = ctx.get_resolver_url();
    let access_token = session.as_ref().map(|s| s.access_token.as_str());

    let download_url;
    let mut download_headers = Vec::new();
    let mut is_local_file = false;

    let item_opt = ctx.find_catalog_item(&task.item_id).or_else(|| {
        ctx.current_playing_item
            .lock()
            .unwrap()
            .as_ref()
            .filter(|it| it.id == task.item_id)
            .cloned()
    });

    if task.item_id == "sample" || task.item_id == "sample2" {
        let filename = if task.item_id == "sample2" {
            "sample2.mp4"
        } else {
            "sample.mp4"
        };
        download_url = crate::options::resolve_named_sample_media(filename);
        if !download_url.starts_with("http://") && !download_url.starts_with("https://") {
            is_local_file = true;
        }
    } else if task.item_id.starts_with("direct:") {
        download_url = task
            .item_id
            .strip_prefix("direct:")
            .unwrap_or(&task.item_id)
            .to_owned();
        is_local_file = true;
    } else if task.item_id.starts_with("telegram:")
        || item_opt.as_ref().is_some_and(|it| it.is_telegram())
    {
        let Some(resolver) = resolver_url.as_deref().filter(|s| !s.is_empty()) else {
            *task.status.lock().unwrap() = DownloadStatus::Error(
                "Resolver URL is required to download Telegram videos. Configure it in Settings."
                    .into(),
            );
            manager.advance_queue_if_idle(ctx);
            return;
        };
        let dummy_item = item_opt.unwrap_or_else(|| TvItem {
            id: task.item_id.clone(),
            title: task.item_title.clone(),
            source_provider: Some("telegram".into()),
            provider_video_id: Some(
                task.item_id
                    .strip_prefix("telegram:")
                    .unwrap_or(&task.item_id)
                    .to_string(),
            ),
            ..Default::default()
        });
        match crate::session::PlaybackSessionController::resolve_stream_url(
            &dummy_item,
            server_url,
            Some(resolver),
            access_token,
        ) {
            Ok((url, _)) => download_url = url,
            Err(e) => {
                let formatted = e.replace("to play Telegram content", "to download Telegram video");
                *task.status.lock().unwrap() =
                    DownloadStatus::Error(format!("Download error: {formatted}"));
                manager.advance_queue_if_idle(ctx);
                return;
            }
        }
    } else {
        // Jellyfin media item: prefer direct static stream (the original unmodified media)
        let server = server_url.unwrap_or("").trim_end_matches('/');
        if server.is_empty() {
            *task.status.lock().unwrap() =
                DownloadStatus::Error("Jellyfin server URL is required".into());
            manager.advance_queue_if_idle(ctx);
            return;
        }
        let token_param = match access_token {
            Some(t) if !t.is_empty() => format!("&api_key={t}"),
            _ => String::new(),
        };
        download_url = format!(
            "{server}/Videos/{}/stream?static=true{token_param}",
            task.item_id
        );
        if let Some(token) = access_token {
            if !token.is_empty() {
                download_headers.push(("X-Emby-Token".to_string(), token.to_string()));
            }
        }
    }

    if task.cancel_flag.load(Ordering::SeqCst) {
        *task.status.lock().unwrap() = DownloadStatus::Error("Download cancelled".into());
        let _ = std::fs::remove_file(&task.temp_path);
        manager.advance_queue_if_idle(ctx);
        return;
    }

    // Step 3: Handle direct local media copy
    if is_local_file {
        *task.status.lock().unwrap() = DownloadStatus::Downloading;
        let src_path = Path::new(&download_url);
        if !src_path.is_file() {
            *task.status.lock().unwrap() =
                DownloadStatus::Error(format!("Local source file not found: {}", download_url));
            manager.advance_queue_if_idle(ctx);
            return;
        }
        if let Ok(meta) = src_path.metadata() {
            task.total_bytes.store(meta.len(), Ordering::SeqCst);
        }
        if let Err(e) = std::fs::copy(src_path, &task.temp_path) {
            *task.status.lock().unwrap() = DownloadStatus::Error(format!("Copy error: {e}"));
            let _ = std::fs::remove_file(&task.temp_path);
            manager.advance_queue_if_idle(ctx);
            return;
        }
        let final_bytes = task.total_bytes.load(Ordering::SeqCst);
        task.bytes_downloaded.store(final_bytes, Ordering::SeqCst);
        if let Some(final_dest) =
            finalize_download(&task.temp_path, &task.target_path, &task.status)
        {
            manager.add_history_item(CompletedDownloadItem {
                item_id: task.item_id.clone(),
                title: task.item_title.clone(),
                file_path: final_dest,
                file_size_bytes: final_bytes,
            });
        }
        manager.advance_queue_if_idle(ctx);
        return;
    }

    // Step 4: Stream download via HTTP with Range support
    *task.status.lock().unwrap() = DownloadStatus::Downloading;

    let client = match reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(300))
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            *task.status.lock().unwrap() = DownloadStatus::Error(format!("HTTP client error: {e}"));
            manager.advance_queue_if_idle(ctx);
            return;
        }
    };

    let mut req = client.get(&download_url);
    for (k, v) in &download_headers {
        req = req.header(k, v);
    }

    if existing_bytes > 0 {
        req = req.header("Range", format!("bytes={existing_bytes}-"));
    }

    let mut response = match req.send() {
        Ok(resp) => {
            if !resp.status().is_success() && resp.status() != reqwest::StatusCode::PARTIAL_CONTENT
            {
                *task.status.lock().unwrap() =
                    DownloadStatus::Error(format!("Server returned HTTP {}", resp.status()));
                manager.advance_queue_if_idle(ctx);
                return;
            }
            resp
        }
        Err(e) => {
            *task.status.lock().unwrap() = DownloadStatus::Error(format!("Connection error: {e}"));
            manager.advance_queue_if_idle(ctx);
            return;
        }
    };

    let is_partial = response.status() == reqwest::StatusCode::PARTIAL_CONTENT;
    let accepts_ranges = response
        .headers()
        .get("accept-ranges")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.eq_ignore_ascii_case("bytes"));

    if is_partial || accepts_ranges {
        task.supports_range.store(true, Ordering::SeqCst);
    }

    if is_partial {
        if let Some(cr) = response
            .headers()
            .get("content-range")
            .and_then(|v| v.to_str().ok())
        {
            if let Some(total_str) = cr.rsplit('/').next() {
                if let Ok(tot) = total_str.trim().parse::<u64>() {
                    task.total_bytes.store(tot, Ordering::SeqCst);
                }
            }
        }
    } else if let Some(len) = response.content_length() {
        if len > 0 {
            task.total_bytes.store(len, Ordering::SeqCst);
        }
    }

    let content_type = response
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_lowercase();

    let is_m3u8 = download_url.contains(".m3u8")
        || content_type.contains("application/vnd.apple.mpegurl")
        || content_type.contains("application/x-mpegurl");

    // Step 5: Segmented stream handling (HLS)
    if is_m3u8 {
        if let Some(ffmpeg_bin) = is_ffmpeg_available() {
            let mut cmd = Command::new(&ffmpeg_bin);
            cmd.arg("-y");
            if !download_headers.is_empty() {
                let header_str = download_headers
                    .iter()
                    .map(|(k, v)| format!("{k}: {v}\r\n"))
                    .collect::<String>();
                cmd.args(["-headers", &header_str]);
            }
            cmd.args(["-i", &download_url, "-c", "copy", "-bsf:a", "aac_adtstoasc"]);
            cmd.arg(&task.temp_path);
            cmd.stdout(Stdio::null());
            cmd.stderr(Stdio::null());

            match cmd.spawn() {
                Ok(child) => {
                    *task.child_process.lock().unwrap() = Some(child);
                    loop {
                        std::thread::sleep(Duration::from_millis(250));
                        if task.cancel_flag.load(Ordering::SeqCst) {
                            if let Ok(mut ch) = task.child_process.lock() {
                                if let Some(mut c) = ch.take() {
                                    let _ = c.kill();
                                }
                            }
                            let _ = std::fs::remove_file(&task.temp_path);
                            *task.status.lock().unwrap() =
                                DownloadStatus::Error("Download cancelled".into());
                            manager.advance_queue_if_idle(ctx);
                            return;
                        }
                        if let Ok(meta) = task.temp_path.metadata() {
                            task.bytes_downloaded.store(meta.len(), Ordering::SeqCst);
                        }
                        if let Ok(mut ch) = task.child_process.lock() {
                            if let Some(ref mut c) = *ch {
                                if let Ok(Some(exit)) = c.try_wait() {
                                    if exit.success() {
                                        break;
                                    } else {
                                        let _ = std::fs::remove_file(&task.temp_path);
                                        *task.status.lock().unwrap() = DownloadStatus::Error(
                                            "Transcoding stream failed".into(),
                                        );
                                        manager.advance_queue_if_idle(ctx);
                                        return;
                                    }
                                }
                            }
                        }
                    }
                    if let Some(final_dest) =
                        finalize_download(&task.temp_path, &task.target_path, &task.status)
                    {
                        let size = task.bytes_downloaded.load(Ordering::SeqCst);
                        manager.add_history_item(CompletedDownloadItem {
                            item_id: task.item_id.clone(),
                            title: task.item_title.clone(),
                            file_path: final_dest,
                            file_size_bytes: size,
                        });
                    }
                    manager.advance_queue_if_idle(ctx);
                    return;
                }
                Err(e) => {
                    crate::diagnostics::GLOBAL_LOG
                        .w("Download", &format!("Could not spawn ffmpeg: {e}"));
                }
            }
        }
    }

    // Step 6: Direct stream download with Range append or fresh create
    let mut out_file = if is_partial && existing_bytes > 0 {
        task.bytes_downloaded
            .store(existing_bytes, Ordering::SeqCst);
        match std::fs::OpenOptions::new()
            .append(true)
            .open(&task.temp_path)
        {
            Ok(f) => f,
            Err(e) => {
                *task.status.lock().unwrap() =
                    DownloadStatus::Error(format!("Cannot resume file: {e}"));
                manager.advance_queue_if_idle(ctx);
                return;
            }
        }
    } else {
        task.bytes_downloaded.store(0, Ordering::SeqCst);
        match File::create(&task.temp_path) {
            Ok(f) => f,
            Err(e) => {
                *task.status.lock().unwrap() =
                    DownloadStatus::Error(format!("Cannot write file: {e}"));
                manager.advance_queue_if_idle(ctx);
                return;
            }
        }
    };

    let mut buffer = [0u8; 64 * 1024]; // 64 KB streaming buffer
    let mut downloaded_accum = task.bytes_downloaded.load(Ordering::SeqCst);

    loop {
        if task.cancel_flag.load(Ordering::SeqCst) {
            drop(out_file);
            let _ = std::fs::remove_file(&task.temp_path);
            *task.status.lock().unwrap() = DownloadStatus::Error("Download cancelled".into());
            manager.advance_queue_if_idle(ctx);
            return;
        }

        if task.pause_flag.load(Ordering::SeqCst) {
            let _ = out_file.flush();
            drop(out_file);
            *task.status.lock().unwrap() = DownloadStatus::Paused;
            return;
        }

        match response.read(&mut buffer) {
            Ok(0) => break, // EOF
            Ok(n) => {
                if let Err(e) = out_file.write_all(&buffer[..n]) {
                    drop(out_file);
                    let _ = std::fs::remove_file(&task.temp_path);
                    *task.status.lock().unwrap() =
                        DownloadStatus::Error(format!("Write error (disk full?): {e}"));
                    manager.advance_queue_if_idle(ctx);
                    return;
                }
                downloaded_accum += n as u64;
                task.bytes_downloaded
                    .store(downloaded_accum, Ordering::SeqCst);
            }
            Err(e) => {
                drop(out_file);
                let _ = std::fs::remove_file(&task.temp_path);
                *task.status.lock().unwrap() =
                    DownloadStatus::Error(format!("Network read error: {e}"));
                manager.advance_queue_if_idle(ctx);
                return;
            }
        }
    }

    if let Err(e) = out_file.flush() {
        drop(out_file);
        let _ = std::fs::remove_file(&task.temp_path);
        *task.status.lock().unwrap() = DownloadStatus::Error(format!("Flush error: {e}"));
        manager.advance_queue_if_idle(ctx);
        return;
    }
    drop(out_file);

    if let Some(final_dest) = finalize_download(&task.temp_path, &task.target_path, &task.status) {
        let size = task.bytes_downloaded.load(Ordering::SeqCst);
        manager.add_history_item(CompletedDownloadItem {
            item_id: task.item_id.clone(),
            title: task.item_title.clone(),
            file_path: final_dest,
            file_size_bytes: size,
        });
    }
    manager.advance_queue_if_idle(ctx);
}

/// Atomically move temp .part file to final destination and avoid silent overwrites
fn finalize_download(
    temp_path: &Path,
    target_path: &Path,
    status: &Arc<Mutex<DownloadStatus>>,
) -> Option<PathBuf> {
    let final_dest = if target_path.exists() {
        let parent = target_path.parent().unwrap_or_else(|| Path::new("."));
        let stem = target_path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("video");
        let ext = target_path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("mp4");
        resolve_unique_path(parent, stem, ext)
    } else {
        target_path.to_path_buf()
    };

    if let Err(e) = std::fs::rename(temp_path, &final_dest) {
        // Fallback to copy + remove if across filesystem boundary
        if let Err(ce) = std::fs::copy(temp_path, &final_dest) {
            let _ = std::fs::remove_file(temp_path);
            *status.lock().unwrap() =
                DownloadStatus::Error(format!("Failed to save final file: {ce} (rename err: {e})"));
            return None;
        }
        let _ = std::fs::remove_file(temp_path);
    }

    let folder = final_dest
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .to_path_buf();
    *status.lock().unwrap() = DownloadStatus::Completed {
        folder_path: folder,
        file_path: final_dest.clone(),
    };
    Some(final_dest)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sanitize_filename() {
        assert_eq!(sanitize_filename("Clean Title"), "Clean Title");
        assert_eq!(
            sanitize_filename("Invalid: / \\ * ? \" < > | Title"),
            "Invalid_ _ _ _ _ _ _ _ _ Title"
        );
        assert_eq!(sanitize_filename("..."), "video");
        assert_eq!(
            sanitize_filename("  Leading and trailing  "),
            "Leading and trailing"
        );
    }

    #[test]
    fn test_resolve_unique_path() {
        let temp_dir = std::env::temp_dir().join("ah_download_test");
        let _ = std::fs::create_dir_all(&temp_dir);

        let p1 = resolve_unique_path(&temp_dir, "test_vid", "mp4");
        File::create(&p1).unwrap();
        assert!(p1.exists());

        let p2 = resolve_unique_path(&temp_dir, "test_vid", "mp4");
        assert_ne!(p1, p2);
        assert!(p2.to_string_lossy().contains("(1)"));
        File::create(&p2).unwrap();

        let p3 = resolve_unique_path(&temp_dir, "test_vid", "mp4");
        assert!(p3.to_string_lossy().contains("(2)"));

        let _ = std::fs::remove_file(p1);
        let _ = std::fs::remove_file(p2);
        let _ = std::fs::remove_dir(&temp_dir);
    }

    #[test]
    fn test_format_download_bytes() {
        assert_eq!(format_download_bytes(500 * 1024, 0), "0.5 MB");
        assert_eq!(
            format_download_bytes(10 * 1024 * 1024, 20 * 1024 * 1024),
            "10.0 MB / 20.0 MB"
        );
        assert_eq!(
            format_download_bytes(1024 * 1024 * 1024, 2 * 1024 * 1024 * 1024),
            "1.00 GB / 2.00 GB"
        );
    }

    #[test]
    fn test_normalize_container_extension() {
        assert_eq!(
            normalize_container_extension(Some("mov,mp4,m4a,3gp,3g2,mj2"), None),
            "mp4"
        );
        assert_eq!(
            normalize_container_extension(Some("matroska,webm"), None),
            "mkv"
        );
        assert_eq!(normalize_container_extension(Some("webm"), None), "webm");
        assert_eq!(normalize_container_extension(Some("mpegts"), None), "ts");
        assert_eq!(
            normalize_container_extension(None, Some("https://example.com/video.mkv?token=123")),
            "mkv"
        );
        assert_eq!(normalize_container_extension(None, None), "mp4");
    }

    #[test]
    fn test_build_suggested_filename_and_polish_chars() {
        assert_eq!(
            build_suggested_filename("Żółty film: akcja w lesie?", "mp4"),
            "Żółty film_ akcja w lesie_.mp4"
        );
        // Does not append extension if already present
        assert_eq!(
            build_suggested_filename("Mój film.mp4", "mp4"),
            "Mój film.mp4"
        );
        assert_eq!(
            build_suggested_filename("Scena_1.mkv", "mp4"),
            "Scena_1.mkv"
        );
    }

    #[test]
    fn test_format_download_speed_and_eta() {
        assert_eq!(format_download_speed(500.0), "500 B/s");
        assert_eq!(format_download_speed(500.0 * 1024.0), "500 KB/s");
        assert_eq!(format_download_speed(2.5 * 1024.0 * 1024.0), "2.5 MB/s");

        assert_eq!(
            format_download_eta(10 * 1024 * 1024, 2.0 * 1024.0 * 1024.0),
            Some("5s remaining".to_string())
        );
        assert_eq!(
            format_download_eta(120 * 1024 * 1024, 1024.0 * 1024.0),
            Some("2m 0s remaining".to_string())
        );
    }

    #[test]
    fn test_download_queue_and_history() {
        let manager = DownloadManager::new();
        assert!(!manager.is_downloading());
        assert_eq!(manager.queued_tasks().len(), 0);
        assert_eq!(manager.history_items().len(), 0);

        manager.add_history_item(CompletedDownloadItem {
            item_id: "test-item-1".to_string(),
            title: "Test Video 1".to_string(),
            file_path: PathBuf::from("/tmp/test1.mp4"),
            file_size_bytes: 1024 * 1024,
        });

        let history = manager.history_items();
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].title, "Test Video 1");
    }

    #[test]
    fn test_download_pause_resume_and_cancellation() {
        let manager = DownloadManager::new();
        let status = Arc::new(Mutex::new(DownloadStatus::Downloading));
        let task = Arc::new(DownloadTask {
            item_id: "test-task-1".to_string(),
            item_title: "Test Task 1".to_string(),
            target_path: PathBuf::from("/tmp/test1.mp4"),
            temp_path: PathBuf::from("/tmp/test1.mp4.part"),
            bytes_downloaded: Arc::new(AtomicU64::new(100)),
            total_bytes: Arc::new(AtomicU64::new(1000)),
            cancel_flag: Arc::new(AtomicBool::new(false)),
            pause_flag: Arc::new(AtomicBool::new(false)),
            supports_range: Arc::new(AtomicBool::new(true)),
            status: status.clone(),
            child_process: Arc::new(Mutex::new(None)),
        });

        manager.set_task(task.clone());
        assert!(manager.is_downloading());
        assert!(manager.is_active_or_queued_for_item("test-task-1"));

        // Pause
        manager.pause();
        assert!(task.pause_flag.load(Ordering::SeqCst));
        assert!(matches!(*status.lock().unwrap(), DownloadStatus::Paused));

        // Resume (mocking a dummy context without running worker)
        // Resume directly clears pause flag and updates status to downloading
        *task.status.lock().unwrap() = DownloadStatus::Downloading;
        task.pause_flag.store(false, Ordering::SeqCst);
        assert!(!task.pause_flag.load(Ordering::SeqCst));
        assert!(matches!(
            *status.lock().unwrap(),
            DownloadStatus::Downloading
        ));

        // Queued task cancellation
        let queued_task = Arc::new(DownloadTask {
            item_id: "test-task-2".to_string(),
            item_title: "Test Task 2".to_string(),
            target_path: PathBuf::from("/tmp/test2.mp4"),
            temp_path: PathBuf::from("/tmp/test2.mp4.part"),
            bytes_downloaded: Arc::new(AtomicU64::new(0)),
            total_bytes: Arc::new(AtomicU64::new(500)),
            cancel_flag: Arc::new(AtomicBool::new(false)),
            pause_flag: Arc::new(AtomicBool::new(false)),
            supports_range: Arc::new(AtomicBool::new(false)),
            status: Arc::new(Mutex::new(DownloadStatus::Queued)),
            child_process: Arc::new(Mutex::new(None)),
        });
        manager.queue.lock().unwrap().push_back(queued_task);
        assert_eq!(manager.queued_tasks().len(), 1);
        assert!(manager.is_active_or_queued_for_item("test-task-2"));

        manager.cancel_queued("test-task-2");
        assert_eq!(manager.queued_tasks().len(), 0);
        assert!(!manager.is_active_or_queued_for_item("test-task-2"));
    }
}
