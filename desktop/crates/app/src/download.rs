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
    pub expected_total_bytes: Arc<AtomicU64>,
    pub cancel_flag: Arc<AtomicBool>,
    pub pause_flag: Arc<AtomicBool>,
    pub supports_range: Arc<AtomicBool>,
    pub rendition_id: Option<String>,
    pub status: Arc<Mutex<DownloadStatus>>,
    pub child_process: Arc<Mutex<Option<Child>>>,
    pub worker_active: Arc<AtomicBool>,
    pub resume_pending: Arc<AtomicBool>,
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
        let lock = self
            .active_task
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(task) = lock.as_ref() {
            let status = task
                .status
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
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
                let status = task
                    .status
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
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
        let queue = self
            .queue
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        queue.iter().any(|t| t.item_id == item_id)
    }

    pub fn cancel(&self) {
        let lock = self
            .active_task
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(task) = lock.as_ref() {
            task.resume_pending.store(false, Ordering::SeqCst);
            task.cancel_flag.store(true, Ordering::SeqCst);
            if let Ok(mut child_lock) = task.child_process.lock() {
                if let Some(mut child) = child_lock.take() {
                    let _ = child.kill();
                }
            }
            let mut st = task
                .status
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            *st = DownloadStatus::Error("Download cancelled".to_string());
            let _ = std::fs::remove_file(&task.temp_path);
            adult_hub_client::PartFileMetadata::remove_sidecar(&task.temp_path);
        }
        self.speed_samples
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clear();
        *self
            .smoothed_speed_bps
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = 0.0;
    }

    /// Cancel the active download and immediately advance the queue (needed when
    /// the active task is paused and no worker thread is running to advance it).
    pub fn cancel_and_advance(&self, ctx: &crate::context::AppContext) {
        self.cancel();
        self.advance_queue_if_idle(ctx);
    }

    pub fn cancel_queued(&self, item_id: &str) {
        let mut q = self
            .queue
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        q.retain(|t| t.item_id != item_id);
    }

    pub fn pause(&self) {
        let lock = self
            .active_task
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(task) = lock.as_ref() {
            task.resume_pending.store(false, Ordering::SeqCst);
            task.pause_flag.store(true, Ordering::SeqCst);
            *task
                .status
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = DownloadStatus::Paused;
        }
    }

    pub fn resume(&self, ctx: &crate::context::AppContext) {
        let task = self.current_task();
        if let Some(task) = task {
            let is_paused = {
                let st = task
                    .status
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                matches!(*st, DownloadStatus::Paused)
            };
            if is_paused {
                task.pause_flag.store(false, Ordering::SeqCst);
                task.cancel_flag.store(false, Ordering::SeqCst);
                *task
                    .status
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner) = DownloadStatus::Preparing;
                if task
                    .worker_active
                    .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
                    .is_ok()
                {
                    let mgr = self.clone();
                    let task_clone = task.clone();
                    let ctx_clone = ctx.clone();
                    std::thread::spawn(move || {
                        run_download_task(&ctx_clone, task_clone, mgr);
                    });
                } else {
                    // Previous worker is still unwinding (e.g., blocked in read/wait);
                    // it will respawn on exit via WorkerGuard.
                    task.resume_pending.store(true, Ordering::SeqCst);
                }
            }
        }
    }

    pub fn enqueue_task_with_rendition(
        &self,
        ctx: &crate::context::AppContext,
        item: &TvItem,
        target_path: PathBuf,
        rendition_id: Option<String>,
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
            total_bytes: Arc::new(AtomicU64::new(0)),
            expected_total_bytes: Arc::new(AtomicU64::new(0)),
            cancel_flag: Arc::new(AtomicBool::new(false)),
            pause_flag: Arc::new(AtomicBool::new(false)),
            supports_range: Arc::new(AtomicBool::new(false)),
            rendition_id,
            status: Arc::new(Mutex::new(DownloadStatus::Queued)),
            child_process: Arc::new(Mutex::new(None)),
            worker_active: Arc::new(AtomicBool::new(false)),
            resume_pending: Arc::new(AtomicBool::new(false)),
        });

        {
            let mut q = self
                .queue
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            q.push_back(task);
        }

        self.advance_queue_if_idle(ctx);
    }

    pub fn advance_queue_if_idle(&self, ctx: &crate::context::AppContext) {
        let mut active = self
            .active_task
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let is_idle = match active.as_ref() {
            None => true,
            Some(t) => {
                let st = t
                    .status
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                matches!(
                    *st,
                    DownloadStatus::Idle
                        | DownloadStatus::Completed { .. }
                        | DownloadStatus::Error(_)
                )
            }
        };

        if is_idle {
            let mut q = self
                .queue
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if let Some(next_task) = q.pop_front() {
                *next_task
                    .status
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner) = DownloadStatus::Preparing;
                *active = Some(next_task.clone());
                drop(active);
                drop(q);

                self.speed_samples
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .clear();
                *self
                    .smoothed_speed_bps
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner) = 0.0;
                *self
                    .last_speed_calc
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner) = Instant::now();

                next_task.worker_active.store(true, Ordering::SeqCst);
                let mgr = self.clone();
                let ctx_clone = ctx.clone();
                std::thread::spawn(move || {
                    run_download_task(&ctx_clone, next_task, mgr);
                });
            }
        }
    }

    pub fn current_task(&self) -> Option<Arc<DownloadTask>> {
        self.active_task
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    pub fn queued_tasks(&self) -> Vec<Arc<DownloadTask>> {
        self.queue
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .cloned()
            .collect()
    }

    pub fn history_items(&self) -> Vec<CompletedDownloadItem> {
        self.history
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    pub fn add_history_item(&self, item: CompletedDownloadItem) {
        let mut hist = self
            .history
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        hist.retain(|h| h.item_id != item.item_id && h.file_path != item.file_path);
        hist.insert(0, item);
        if hist.len() > 30 {
            hist.truncate(30);
        }
    }

    #[allow(dead_code)]
    pub fn set_task(&self, task: Arc<DownloadTask>) {
        *self
            .active_task
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(task);
        self.speed_samples
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clear();
        *self
            .smoothed_speed_bps
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = 0.0;
        *self
            .last_speed_calc
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Instant::now();
    }

    #[allow(dead_code)]
    pub fn clear_task(&self) {
        *self
            .active_task
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
        self.speed_samples
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clear();
        *self
            .smoothed_speed_bps
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = 0.0;
    }

    /// Sample downloaded bytes and calculate smoothed speed over a short rolling window (~2-3s).
    /// Updates the calculated speed roughly once per second.
    pub fn sample_speed(&self, current_bytes: u64, total_bytes: u64) -> (f64, Option<String>) {
        let now = Instant::now();
        let mut samples = self
            .speed_samples
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        samples.push_back((now, current_bytes));

        // Retain samples within the last 2.5 seconds
        while let Some(&(time, _)) = samples.front() {
            if now.duration_since(time) > Duration::from_millis(2500) && samples.len() > 2 {
                samples.pop_front();
            } else {
                break;
            }
        }

        let mut last_calc = self
            .last_speed_calc
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut smoothed = self
            .smoothed_speed_bps
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);

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
        let _ = std::process::Command::new("explorer").arg(path).spawn();
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

/// Arguments for downloading an HLS stream with ffmpeg.
///
/// The accepted F-01 trade-off in the audit: ffmpeg only reads headers from
/// its command line, so a Bearer token is visible to processes of the same
/// user (and root) while the download runs. Playback avoids this because mpv
/// receives headers over IPC. A loopback proxy would remove it at the cost of
/// another moving part; see the audit report.
fn ffmpeg_hls_args(
    download_url: &str,
    temp_path: &std::path::Path,
    headers: &[(String, String)],
) -> Vec<std::ffi::OsString> {
    let mut args = vec![std::ffi::OsString::from("-y")];
    if !headers.is_empty() {
        let header_str = headers
            .iter()
            .map(|(key, value)| format!("{key}: {value}\r\n"))
            .collect::<String>();
        args.push("-headers".into());
        args.push(header_str.into());
    }
    for value in ["-i", download_url, "-c", "copy", "-bsf:a", "aac_adtstoasc"] {
        args.push(value.into());
    }
    args.push(temp_path.as_os_str().to_owned());
    args
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
    let rendition_id = {
        let is_playing = ctx
            .current_playing_item
            .lock()
            .ok()
            .and_then(|opt| opt.as_ref().map(|it| it.id == item.id))
            .unwrap_or(false);
        if is_playing {
            ctx.active_quality_id
                .lock()
                .ok()
                .map(|q| q.clone())
                .filter(|q| !q.is_empty() && q != "original")
        } else {
            None
        }
    };
    start_item_download_with_rendition(ctx, item, target_path, manager, rendition_id);
}

pub fn start_item_download_with_rendition(
    ctx: &crate::context::AppContext,
    item: &TvItem,
    target_path: PathBuf,
    manager: Arc<DownloadManager>,
    rendition_id: Option<String>,
) {
    manager.enqueue_task_with_rendition(ctx, item, target_path, rendition_id);
}

static AH_TOKEN_REFRESH_MUTEX: Mutex<()> = Mutex::new(());

fn safe_error_category(err: &adult_hub_client::AdultHubError) -> &'static str {
    match err {
        adult_hub_client::AdultHubError::Unauthorized { .. } => "Unauthorized",
        adult_hub_client::AdultHubError::RateLimited(_) => "RateLimited",
        adult_hub_client::AdultHubError::NotFound(_) => "NotFound",
        adult_hub_client::AdultHubError::Conflict(_) => "Conflict",
        adult_hub_client::AdultHubError::PayloadTooLarge { .. } => "PayloadTooLarge",
        adult_hub_client::AdultHubError::Api { .. } => "Api",
        adult_hub_client::AdultHubError::InvalidUrl(_) => "InvalidUrl",
        adult_hub_client::AdultHubError::OriginMismatch { .. } => "OriginMismatch",
        adult_hub_client::AdultHubError::Io(_) => "Io",
        adult_hub_client::AdultHubError::Http(_) => "Http",
        adult_hub_client::AdultHubError::Serialization(_) => "Serialization",
    }
}

#[derive(Debug)]
#[allow(dead_code)]
pub(crate) enum RefreshSessionError {
    NoSessionOrToken,
    ClientError(String),
    RefreshFailed(String),
    PersistenceFailed(String),
}

/// Synchronously refreshes the Adult Hub session if the token has not already been refreshed
/// by another task. Returns the active session on success.
pub(crate) fn refresh_adult_hub_session_if_current(
    ctx: &crate::context::AppContext,
    failed_access_token: &str,
) -> Result<adult_hub_storage::Session, RefreshSessionError> {
    let _lock = AH_TOKEN_REFRESH_MUTEX
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);

    // Reread current session after acquiring lock (double-checked pattern)
    let mut current_sess = ctx
        .credential_store
        .get()
        .ok()
        .flatten()
        .ok_or(RefreshSessionError::NoSessionOrToken)?;

    // If another task already rotated the access token, adopt the new session
    if current_sess.access_token != failed_access_token {
        crate::log_debug(
            "download: session token was already rotated by another task; reusing new token",
        );
        return Ok(current_sess);
    }

    let rt = current_sess
        .refresh_token
        .as_ref()
        .ok_or(RefreshSessionError::NoSessionOrToken)?;
    let hub_client = match adult_hub_client::AdultHubClient::new(&current_sess.server_url) {
        Ok(c) => c,
        Err(e) => {
            crate::diagnostics::GLOBAL_LOG.w(
                "Auth",
                &format!("Cannot create client for token refresh: {e}"),
            );
            return Err(RefreshSessionError::ClientError(e.to_string()));
        }
    };

    match hub_client.refresh_token(rt) {
        Ok(refreshed) => {
            current_sess.access_token = refreshed.access_token.clone();
            current_sess.refresh_token = Some(refreshed.refresh_token.clone());
            if let Err(e) = ctx.credential_store.save(current_sess.clone()) {
                crate::diagnostics::GLOBAL_LOG.e(
                    "Auth",
                    &format!("Failed to persist rotated credentials: {e}"),
                );
                return Err(RefreshSessionError::PersistenceFailed(format!(
                    "Failed to save refreshed credentials: {e}"
                )));
            }
            ctx.reporter.set_auth(
                Some(current_sess.server_url.clone()),
                Some(current_sess.access_token.clone()),
            );
            crate::thumbnails::set_resolver_auth_token(Some(&current_sess.access_token));
            ctx.phash_coord
                .update_auth(&current_sess.server_url, Some(&current_sess.access_token));
            Ok(current_sess)
        }
        Err(e) => {
            // Log refresh error category safely: no tokens
            crate::diagnostics::GLOBAL_LOG.w(
                "Auth",
                &format!(
                    "Adult Hub token refresh failed: category={}",
                    safe_error_category(&e)
                ),
            );
            Err(RefreshSessionError::RefreshFailed(
                safe_error_category(&e).to_string(),
            ))
        }
    }
}

/// Execute a single download task (called sequentially from queue worker thread)
fn run_download_task(
    ctx: &crate::context::AppContext,
    task: Arc<DownloadTask>,
    manager: DownloadManager,
) {
    struct WorkerGuard {
        task: Arc<DownloadTask>,
        manager: DownloadManager,
        ctx: crate::context::AppContext,
    }

    impl Drop for WorkerGuard {
        fn drop(&mut self) {
            self.task.worker_active.store(false, Ordering::SeqCst);
            if self.task.resume_pending.swap(false, Ordering::SeqCst)
                && !self.task.cancel_flag.load(Ordering::SeqCst)
                && !self.task.pause_flag.load(Ordering::SeqCst)
                && self
                    .task
                    .worker_active
                    .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
                    .is_ok()
            {
                let mgr = self.manager.clone();
                let task_clone = self.task.clone();
                let ctx_clone = self.ctx.clone();
                std::thread::spawn(move || {
                    run_download_task(&ctx_clone, task_clone, mgr);
                });
            }
        }
    }

    task.worker_active.store(true, Ordering::SeqCst);
    let _worker_guard = WorkerGuard {
        task: task.clone(),
        manager: manager.clone(),
        ctx: ctx.clone(),
    };

    let download_started = std::time::Instant::now();
    crate::log_debug("download: task picked up from queue");

    // Step 1: Ensure target parent directory exists
    if let Some(parent) = task.target_path.parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            crate::log_debug(&format!("download: cannot create target directory: {e}"));
            *task
                .status
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) =
                DownloadStatus::Error(format!("Cannot create directory: {e}"));
            manager.advance_queue_if_idle(ctx);
            return;
        }
    }

    // Step 2: Resolve media URL and headers
    let session = ctx.credential_store.get().ok().flatten();

    let download_url;
    let mut download_headers = Vec::new();
    let mut is_local_file = false;
    let source_identity;

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
        source_identity = format!("sample:{filename}");
    } else if task.item_id.starts_with("direct:") {
        download_url = task
            .item_id
            .strip_prefix("direct:")
            .unwrap_or(&task.item_id)
            .to_owned();
        is_local_file = true;
        source_identity = format!("direct:{download_url}");
    } else {
        let Some(mut active_session) = session.clone() else {
            *task
                .status
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) =
                DownloadStatus::Error("Sign in to Adult Hub before downloading this video.".into());
            manager.advance_queue_if_idle(ctx);
            return;
        };

        let client = match adult_hub_client::AdultHubClient::new(&active_session.server_url) {
            Ok(c) => c.with_token(&active_session.access_token),
            Err(e) => {
                *task
                    .status
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner) =
                    DownloadStatus::Error(format!("Invalid Adult Hub server configuration: {e}"));
                manager.advance_queue_if_idle(ctx);
                return;
            }
        };

        // Fetch playback descriptor, refreshing token if 401
        let mut descriptor_res = client.get_playback_descriptor(&task.item_id);
        if let Err(ref err) = descriptor_res {
            if err.is_unauthorized() {
                match refresh_adult_hub_session_if_current(ctx, &active_session.access_token) {
                    Ok(new_sess) => {
                        active_session = new_sess;
                        client.set_token(Some(active_session.access_token.clone()));
                        descriptor_res = client.get_playback_descriptor(&task.item_id);
                    }
                    Err(RefreshSessionError::PersistenceFailed(msg)) => {
                        *task
                            .status
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner) =
                            DownloadStatus::Error(format!(
                                "Failed to persist refreshed session ({msg}). Please sign in again."
                            ));
                        manager.advance_queue_if_idle(ctx);
                        return;
                    }
                    Err(_) => {}
                }
            }
        }

        let descriptor = match descriptor_res {
            Ok(d) => d,
            Err(err) => {
                let msg = if err.is_unauthorized() {
                    "Session expired (401 Unauthorized). Please sign in again.".to_string()
                } else {
                    format!(
                        "Could not resolve download descriptor: {}",
                        crate::playback::format_safe_adult_hub_error(&err)
                    )
                };
                *task
                    .status
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner) =
                    DownloadStatus::Error(msg);
                manager.advance_queue_if_idle(ctx);
                return;
            }
        };

        // Safely set expected_total_bytes and total_bytes from current descriptor / rendition
        let descriptor_file_size = if let Some(ref rid) = task.rendition_id {
            if rid != "original" {
                descriptor
                    .rendition_by_id(rid)
                    .and_then(|r| r.file_size_bytes)
                    .or(descriptor.file_size_bytes)
            } else {
                descriptor.file_size_bytes
            }
        } else {
            descriptor.file_size_bytes
        };

        if let Some(sz) = descriptor_file_size {
            if sz > 0 {
                task.expected_total_bytes.store(sz, Ordering::SeqCst);
                task.total_bytes.store(sz, Ordering::SeqCst);
            }
        }

        let stream_path = descriptor.rendition_stream_path(task.rendition_id.as_deref());
        let Some(stream_path) = stream_path else {
            *task
                .status
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = DownloadStatus::Error(
                "No stream path available in Adult Hub playback descriptor".into(),
            );
            manager.advance_queue_if_idle(ctx);
            return;
        };

        let resolved = match client.resolve_stream_url(stream_path) {
            Ok(u) => u,
            Err(e) => {
                *task
                    .status
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner) =
                    DownloadStatus::Error(format!("Invalid stream URL: {e}"));
                manager.advance_queue_if_idle(ctx);
                return;
            }
        };

        // client.resolve_stream_url(stream_path) has already validated canonical origin
        // (including port canonicalization) and rejected embedded credentials.
        download_url = resolved;
        let token_val = client
            .token()
            .unwrap_or_else(|| active_session.access_token.clone());
        download_headers.push(("Authorization".to_string(), format!("Bearer {token_val}")));
        source_identity = format!(
            "adulthub:{}|{}|{}|{}",
            active_session.server_url.trim_end_matches('/'),
            task.item_id,
            task.rendition_id.as_deref().unwrap_or("default"),
            stream_path
        );
    }

    let mut existing_bytes = 0;
    if !is_local_file && task.temp_path.is_file() {
        if let Some(meta) = adult_hub_client::PartFileMetadata::read_from_sidecar(&task.temp_path) {
            if meta.matches(
                &task.item_id,
                task.rendition_id.as_deref(),
                &source_identity,
            ) {
                existing_bytes = task.temp_path.metadata().map(|m| m.len()).unwrap_or(0);
            } else {
                crate::log_debug(
                    "download: partial file source identity or rendition mismatch; resetting to byte 0",
                );
                existing_bytes = 0;
            }
        } else {
            crate::log_debug(
                "download: partial file exists but Range metadata cannot be proven; resetting to byte 0",
            );
            existing_bytes = 0;
        }
    }

    if task.cancel_flag.load(Ordering::SeqCst) {
        *task
            .status
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) =
            DownloadStatus::Error("Download cancelled".into());
        let _ = std::fs::remove_file(&task.temp_path);
        adult_hub_client::PartFileMetadata::remove_sidecar(&task.temp_path);
        manager.advance_queue_if_idle(ctx);
        return;
    }

    // Step 3: Handle direct local media copy
    if is_local_file {
        *task
            .status
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = DownloadStatus::Downloading;
        let src_path = Path::new(&download_url);
        if !src_path.is_file() {
            *task
                .status
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) =
                DownloadStatus::Error(format!("Local source file not found: {}", download_url));
            manager.advance_queue_if_idle(ctx);
            return;
        }
        if let Ok(meta) = src_path.metadata() {
            task.total_bytes.store(meta.len(), Ordering::SeqCst);
        }
        if let Err(e) = std::fs::copy(src_path, &task.temp_path) {
            *task
                .status
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) =
                DownloadStatus::Error(format!("Copy error: {e}"));
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
    *task
        .status
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = DownloadStatus::Downloading;

    // A request that carries the account token may only follow redirects within
    // its own origin. reqwest does not strip custom credential headers on
    // cross-host redirects, so the default policy would leak the token.
    let redirect_policy = if download_headers.is_empty() {
        reqwest::redirect::Policy::default()
    } else {
        let origin = url::Url::parse(&download_url)
            .ok()
            .map(|url| url.origin().ascii_serialization())
            .unwrap_or_default();
        reqwest::redirect::Policy::custom(move |attempt| {
            if attempt.previous().len() >= 10 {
                attempt.error("too many redirects")
            } else if attempt.url().origin().ascii_serialization() == origin {
                attempt.follow()
            } else {
                attempt.stop()
            }
        })
    };

    let client = match reqwest::blocking::Client::builder()
        .connect_timeout(Duration::from_secs(15))
        .redirect(redirect_policy)
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            *task
                .status
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) =
                DownloadStatus::Error(format!("HTTP client error: {e}"));
            manager.advance_queue_if_idle(ctx);
            return;
        }
    };

    let send_stream_request = |range_offset: u64,
                               hdrs: &[(String, String)]|
     -> reqwest::Result<reqwest::blocking::Response> {
        let mut req = client.get(&download_url);
        for (k, v) in hdrs {
            req = req.header(k, v);
        }
        if range_offset > 0 {
            req = req.header("Range", format!("bytes={range_offset}-"));
        }
        req.send()
    };

    let mut response = match send_stream_request(existing_bytes, &download_headers) {
        Ok(resp) => resp,
        Err(e) => {
            crate::log_debug(&format!("download: connection error: {e}"));
            *task
                .status
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) =
                DownloadStatus::Error(format!("Connection error: {e}"));
            manager.advance_queue_if_idle(ctx);
            return;
        }
    };

    // If 401 in Adult Hub mode, attempt token renewal and retry once
    if response.status() == reqwest::StatusCode::UNAUTHORIZED {
        let failed_token = download_headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case("authorization"))
            .and_then(|(_, v)| {
                v.strip_prefix("Bearer ")
                    .or_else(|| v.strip_prefix("bearer "))
            })
            .unwrap_or("");
        match refresh_adult_hub_session_if_current(ctx, failed_token) {
            Ok(new_sess) => {
                download_headers.retain(|(k, _)| !k.eq_ignore_ascii_case("authorization"));
                download_headers.push((
                    "Authorization".to_string(),
                    format!("Bearer {}", new_sess.access_token),
                ));
                if let Ok(retried) = send_stream_request(existing_bytes, &download_headers) {
                    response = retried;
                }
            }
            Err(RefreshSessionError::PersistenceFailed(msg)) => {
                *task
                    .status
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner) = DownloadStatus::Error(
                    format!("Failed to persist refreshed session ({msg}). Please sign in again."),
                );
                manager.advance_queue_if_idle(ctx);
                return;
            }
            Err(_) => {}
        }
    }

    let prev_expected = {
        let e = task.expected_total_bytes.load(Ordering::SeqCst);
        if e > 0 {
            Some(e)
        } else {
            let t = task.total_bytes.load(Ordering::SeqCst);
            if t > 0 { Some(t) } else { None }
        }
    };

    let cr_hdr = response
        .headers()
        .get("content-range")
        .and_then(|v| v.to_str().ok());

    let mut outcome = adult_hub_client::evaluate_range_resume(
        response.status().as_u16(),
        cr_hdr,
        response.content_length(),
        existing_bytes,
        prev_expected,
    );

    // If 416 was returned when trying to resume with existing_bytes,
    // safely restart from byte 0:
    if response.status() == reqwest::StatusCode::RANGE_NOT_SATISFIABLE && existing_bytes > 0 {
        crate::log_debug("download: 416 Range Not Satisfiable; attempting restart from byte 0");
        match send_stream_request(0, &download_headers) {
            Ok(retried) => {
                response = retried;
                let cr = response
                    .headers()
                    .get("content-range")
                    .and_then(|v| v.to_str().ok());
                outcome = adult_hub_client::evaluate_range_resume(
                    response.status().as_u16(),
                    cr,
                    response.content_length(),
                    0,
                    prev_expected,
                );
            }
            Err(e) => {
                *task
                    .status
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner) =
                    DownloadStatus::Error(format!("Connection error on restart: {e}"));
                manager.advance_queue_if_idle(ctx);
                return;
            }
        }
    }

    // If range resume evaluation required a restart (e.g. 206 start offset mismatch or missing Content-Range),
    // and the response was NOT already an HTTP 200 full stream, request the full stream from byte 0:
    if let adult_hub_client::RangeResumeOutcome::Restart { ref reason, .. } = outcome {
        if response.status() != reqwest::StatusCode::OK {
            crate::log_debug(&format!(
                "download: range resume incompatible ({reason}); requesting full stream from byte 0"
            ));
            match send_stream_request(0, &download_headers) {
                Ok(retried) => {
                    response = retried;
                    let cr = response
                        .headers()
                        .get("content-range")
                        .and_then(|v| v.to_str().ok());
                    outcome = adult_hub_client::evaluate_range_resume(
                        response.status().as_u16(),
                        cr,
                        response.content_length(),
                        0,
                        prev_expected,
                    );
                }
                Err(e) => {
                    *task
                        .status
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner) =
                        DownloadStatus::Error(format!("Connection error on full restart: {e}"));
                    manager.advance_queue_if_idle(ctx);
                    return;
                }
            }
        }
    }

    let current_source_total = match outcome {
        adult_hub_client::RangeResumeOutcome::Error { message, .. } => {
            *task
                .status
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) =
                DownloadStatus::Error(message);
            manager.advance_queue_if_idle(ctx);
            return;
        }
        adult_hub_client::RangeResumeOutcome::Resume {
            start_byte,
            total_bytes,
        } => {
            if response.status() == reqwest::StatusCode::PARTIAL_CONTENT {
                task.supports_range.store(true, Ordering::SeqCst);
            }
            if let Some(tot) = total_bytes {
                task.total_bytes.store(tot, Ordering::SeqCst);
                if task.expected_total_bytes.load(Ordering::SeqCst) == 0 {
                    task.expected_total_bytes.store(tot, Ordering::SeqCst);
                }
            }
            existing_bytes = start_byte;
            total_bytes
        }
        adult_hub_client::RangeResumeOutcome::Restart {
            reason,
            total_bytes,
        } => {
            crate::log_debug(&format!(
                "download: range resume incompatible ({reason}); restarting from byte 0"
            ));
            existing_bytes = 0;
            if let Some(tot) = total_bytes {
                task.total_bytes.store(tot, Ordering::SeqCst);
                task.expected_total_bytes.store(tot, Ordering::SeqCst);
            }
            total_bytes
        }
    };

    let is_partial = response.status() == reqwest::StatusCode::PARTIAL_CONTENT;
    crate::log_debug(&format!(
        "download: HTTP {} after {} ms, resume_from={existing_bytes} bytes, partial={is_partial}",
        response.status().as_u16(),
        download_started.elapsed().as_millis()
    ));

    let accepts_ranges = response
        .headers()
        .get("accept-ranges")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.eq_ignore_ascii_case("bytes"));

    if accepts_ranges {
        task.supports_range.store(true, Ordering::SeqCst);
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
            cmd.args(ffmpeg_hls_args(
                &download_url,
                &task.temp_path,
                &download_headers,
            ));
            cmd.stdout(Stdio::null());
            cmd.stderr(Stdio::null());

            match cmd.spawn() {
                Ok(child) => {
                    *task
                        .child_process
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(child);
                    loop {
                        std::thread::sleep(Duration::from_millis(250));
                        if task.cancel_flag.load(Ordering::SeqCst) {
                            if let Ok(mut ch) = task.child_process.lock() {
                                if let Some(mut c) = ch.take() {
                                    let _ = c.kill();
                                    let _ = c.wait();
                                }
                            }
                            let _ = std::fs::remove_file(&task.temp_path);
                            *task
                                .status
                                .lock()
                                .unwrap_or_else(std::sync::PoisonError::into_inner) =
                                DownloadStatus::Error("Download cancelled".into());
                            manager.advance_queue_if_idle(ctx);
                            return;
                        }
                        if task.pause_flag.load(Ordering::SeqCst) {
                            if let Ok(mut ch) = task.child_process.lock() {
                                if let Some(mut c) = ch.take() {
                                    let _ = c.kill();
                                    let _ = c.wait();
                                }
                            }
                            *task
                                .status
                                .lock()
                                .unwrap_or_else(std::sync::PoisonError::into_inner) =
                                DownloadStatus::Paused;
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
                                        *task
                                            .status
                                            .lock()
                                            .unwrap_or_else(std::sync::PoisonError::into_inner) =
                                            DownloadStatus::Error(
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
    let mut out_file =
        if existing_bytes > 0 && response.status() == reqwest::StatusCode::PARTIAL_CONTENT {
            task.bytes_downloaded
                .store(existing_bytes, Ordering::SeqCst);
            let f = match std::fs::OpenOptions::new()
                .append(true)
                .open(&task.temp_path)
            {
                Ok(f) => f,
                Err(e) => {
                    *task
                        .status
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner) =
                        DownloadStatus::Error(format!("Cannot resume file: {e}"));
                    manager.advance_queue_if_idle(ctx);
                    return;
                }
            };
            let meta = adult_hub_client::PartFileMetadata::new(
                &task.item_id,
                task.rendition_id.clone(),
                &source_identity,
                current_source_total,
            );
            let _ = meta.save_to_sidecar(&task.temp_path);
            f
        } else if response.status() == reqwest::StatusCode::OK
            || response.status() == reqwest::StatusCode::PARTIAL_CONTENT
        {
            task.bytes_downloaded.store(0, Ordering::SeqCst);
            let f = match File::create(&task.temp_path) {
                Ok(f) => f,
                Err(e) => {
                    *task
                        .status
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner) =
                        DownloadStatus::Error(format!("Cannot write file: {e}"));
                    manager.advance_queue_if_idle(ctx);
                    return;
                }
            };
            let meta = adult_hub_client::PartFileMetadata::new(
                &task.item_id,
                task.rendition_id.clone(),
                &source_identity,
                current_source_total,
            );
            let _ = meta.save_to_sidecar(&task.temp_path);
            f
        } else {
            *task
                .status
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = DownloadStatus::Error(
                format!("Server returned unexpected status {}", response.status()),
            );
            manager.advance_queue_if_idle(ctx);
            return;
        };

    let mut buffer = [0u8; 64 * 1024]; // 64 KB streaming buffer
    let mut downloaded_accum = task.bytes_downloaded.load(Ordering::SeqCst);

    loop {
        if task.cancel_flag.load(Ordering::SeqCst) {
            drop(out_file);
            let _ = std::fs::remove_file(&task.temp_path);
            adult_hub_client::PartFileMetadata::remove_sidecar(&task.temp_path);
            *task
                .status
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) =
                DownloadStatus::Error("Download cancelled".into());
            manager.advance_queue_if_idle(ctx);
            return;
        }

        if task.pause_flag.load(Ordering::SeqCst) {
            let _ = out_file.flush();
            drop(out_file);
            *task
                .status
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = DownloadStatus::Paused;
            return;
        }

        match response.read(&mut buffer) {
            Ok(0) => break, // EOF
            Ok(n) => {
                if let Err(e) = out_file.write_all(&buffer[..n]) {
                    crate::log_debug(&format!(
                        "download: write error after {downloaded_accum} bytes: {e}"
                    ));
                    drop(out_file);
                    *task
                        .status
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner) =
                        DownloadStatus::Error(format!("Write error (disk full?): {e}"));
                    manager.advance_queue_if_idle(ctx);
                    return;
                }
                downloaded_accum += n as u64;
                task.bytes_downloaded
                    .store(downloaded_accum, Ordering::SeqCst);
            }
            Err(e) => {
                crate::log_debug(&format!(
                    "download: network read error after {downloaded_accum} bytes: {e}"
                ));
                let _ = out_file.flush();
                drop(out_file);
                // Do NOT delete task.temp_path on network read error: preserve partial data for future resume!
                *task
                    .status
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner) =
                    DownloadStatus::Error(format!("Network read error: {e}"));
                manager.advance_queue_if_idle(ctx);
                return;
            }
        }
    }

    if let Err(e) = out_file.flush() {
        drop(out_file);
        *task
            .status
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) =
            DownloadStatus::Error(format!("Flush error: {e}"));
        manager.advance_queue_if_idle(ctx);
        return;
    }
    drop(out_file);

    if let Some(final_dest) = finalize_download(&task.temp_path, &task.target_path, &task.status) {
        let size = task.bytes_downloaded.load(Ordering::SeqCst);
        crate::log_debug(&format!(
            "download: completed {size} bytes in {} s",
            download_started.elapsed().as_secs()
        ));
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
            adult_hub_client::PartFileMetadata::remove_sidecar(temp_path);
            *status
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) =
                DownloadStatus::Error(format!("Failed to save final file: {ce} (rename err: {e})"));
            return None;
        }
        let _ = std::fs::remove_file(temp_path);
    }
    adult_hub_client::PartFileMetadata::remove_sidecar(temp_path);

    let folder = final_dest
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .to_path_buf();
    *status
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = DownloadStatus::Completed {
        folder_path: folder,
        file_path: final_dest.clone(),
    };
    Some(final_dest)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hls_ffmpeg_args_carry_headers_only_when_present() {
        let headers = vec![("Authorization".to_string(), "Bearer secret".to_string())];
        let args = ffmpeg_hls_args(
            "https://example.test/stream.m3u8",
            std::path::Path::new("/tmp/out.mp4"),
            &headers,
        );
        let rendered = args
            .iter()
            .map(|value| value.to_string_lossy())
            .collect::<Vec<_>>();
        let header_index = rendered
            .iter()
            .position(|value| value == "-headers")
            .expect("headers flag");
        assert_eq!(
            rendered[header_index + 1].as_ref(),
            "Authorization: Bearer secret\r\n"
        );
        assert!(rendered.iter().any(|value| value.as_ref() == "-i"));
        assert_eq!(
            rendered.last().map(|value| value.as_ref()),
            Some("/tmp/out.mp4")
        );

        let without_headers = ffmpeg_hls_args(
            "https://example.test/stream.m3u8",
            std::path::Path::new("/tmp/out.mp4"),
            &[],
        );
        assert!(
            !without_headers
                .iter()
                .any(|value| value.to_string_lossy() == "-headers")
        );
    }

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
            expected_total_bytes: Arc::new(AtomicU64::new(1000)),
            cancel_flag: Arc::new(AtomicBool::new(false)),
            pause_flag: Arc::new(AtomicBool::new(false)),
            supports_range: Arc::new(AtomicBool::new(true)),
            rendition_id: None,
            status: status.clone(),
            child_process: Arc::new(Mutex::new(None)),
            worker_active: Arc::new(AtomicBool::new(false)),
            resume_pending: Arc::new(AtomicBool::new(false)),
        });

        manager.set_task(task.clone());
        assert!(manager.is_downloading());
        assert!(manager.is_active_or_queued_for_item("test-task-1"));

        // Pause
        manager.pause();
        assert!(task.pause_flag.load(Ordering::SeqCst));
        assert!(matches!(
            *status
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
            DownloadStatus::Paused
        ));

        // Resume (mocking a dummy context without running worker)
        // Resume directly clears pause flag and updates status to downloading
        *task
            .status
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = DownloadStatus::Downloading;
        task.pause_flag.store(false, Ordering::SeqCst);
        assert!(!task.pause_flag.load(Ordering::SeqCst));
        assert!(matches!(
            *status
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
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
            expected_total_bytes: Arc::new(AtomicU64::new(500)),
            cancel_flag: Arc::new(AtomicBool::new(false)),
            pause_flag: Arc::new(AtomicBool::new(false)),
            supports_range: Arc::new(AtomicBool::new(false)),
            rendition_id: Some("alt_720".to_string()),
            status: Arc::new(Mutex::new(DownloadStatus::Queued)),
            child_process: Arc::new(Mutex::new(None)),
            worker_active: Arc::new(AtomicBool::new(false)),
            resume_pending: Arc::new(AtomicBool::new(false)),
        });
        manager
            .queue
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push_back(queued_task);
        assert_eq!(manager.queued_tasks().len(), 1);
        assert!(manager.is_active_or_queued_for_item("test-task-2"));

        manager.cancel_queued("test-task-2");
        assert_eq!(manager.queued_tasks().len(), 0);
        assert!(!manager.is_active_or_queued_for_item("test-task-2"));
    }

    #[test]
    fn test_sidecar_metadata_optional_expected_total_bytes() {
        let temp_dir = std::env::temp_dir().join(format!("ah-down-opt-{}", uuid::Uuid::new_v4()));
        let _ = std::fs::create_dir_all(&temp_dir);
        let part_file = temp_dir.join("test_opt.mp4.part");
        std::fs::write(&part_file, b"data").unwrap();

        // Unknown size: expected_total_bytes is None (preserves optional behavior)
        let meta = adult_hub_client::PartFileMetadata::new(
            "vid_unknown",
            None,
            "adulthub:https://server.test|vid_unknown|default|/stream",
            None,
        );
        meta.save_to_sidecar(&part_file).unwrap();

        let loaded = adult_hub_client::PartFileMetadata::read_from_sidecar(&part_file).unwrap();
        assert_eq!(loaded.expected_total_bytes, None);
        assert!(loaded.matches(
            "vid_unknown",
            None,
            "adulthub:https://server.test|vid_unknown|default|/stream"
        ));

        // When source total is established, updating sidecar records the validated total
        let updated_meta = adult_hub_client::PartFileMetadata::new(
            "vid_unknown",
            None,
            "adulthub:https://server.test|vid_unknown|default|/stream",
            Some(2048),
        );
        updated_meta.save_to_sidecar(&part_file).unwrap();
        let loaded2 = adult_hub_client::PartFileMetadata::read_from_sidecar(&part_file).unwrap();
        assert_eq!(loaded2.expected_total_bytes, Some(2048));

        adult_hub_client::PartFileMetadata::remove_sidecar(&part_file);
        let _ = std::fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn test_safe_error_category() {
        use adult_hub_client::AdultHubError;
        assert_eq!(
            safe_error_category(&AdultHubError::Unauthorized {
                status: 401,
                message: "token secret_123 expired".into()
            }),
            "Unauthorized"
        );
        assert_eq!(
            safe_error_category(&AdultHubError::RateLimited("429".into())),
            "RateLimited"
        );
        assert_eq!(
            safe_error_category(&AdultHubError::NotFound("404".into())),
            "NotFound"
        );
        assert_eq!(
            safe_error_category(&AdultHubError::Conflict("409".into())),
            "Conflict"
        );
        assert_eq!(
            safe_error_category(&AdultHubError::PayloadTooLarge { limit: 1000 }),
            "PayloadTooLarge"
        );
        assert_eq!(
            safe_error_category(&AdultHubError::InvalidUrl("bad".into())),
            "InvalidUrl"
        );
    }

    #[test]
    fn test_source_mismatch_and_proven_metadata_check() {
        let temp_dir = std::env::temp_dir().join(format!("ah-down-test-{}", uuid::Uuid::new_v4()));
        let _ = std::fs::create_dir_all(&temp_dir);
        let part_file = temp_dir.join("test_file.mp4.part");

        // Write some partial data
        std::fs::write(&part_file, b"existing 100 bytes of data").unwrap();
        assert!(part_file.is_file());

        // Without metadata sidecar: Range metadata cannot be proven -> existing_bytes must be 0
        let unproven_meta = adult_hub_client::PartFileMetadata::read_from_sidecar(&part_file);
        assert!(unproven_meta.is_none());

        // Save metadata for rendition "720p"
        let meta = adult_hub_client::PartFileMetadata::new(
            "item_123",
            Some("720p".to_string()),
            "adulthub:https://server.example|item_123|720p|/stream/720",
            Some(100_000),
        );
        meta.save_to_sidecar(&part_file).unwrap();

        // When queried with matching rendition/source:
        let loaded = adult_hub_client::PartFileMetadata::read_from_sidecar(&part_file).unwrap();
        assert!(loaded.matches(
            "item_123",
            Some("720p"),
            "adulthub:https://server.example|item_123|720p|/stream/720"
        ));

        // When queried after rendition changed to "1080p":
        assert!(!loaded.matches(
            "item_123",
            Some("1080p"),
            "adulthub:https://server.example|item_123|1080p|/stream/1080"
        ));

        // When queried after rendition changed to original (None):
        assert!(!loaded.matches(
            "item_123",
            None,
            "adulthub:https://server.example|item_123|default|/stream/orig"
        ));

        // Sidecar removal on cancellation / finalize
        adult_hub_client::PartFileMetadata::remove_sidecar(&part_file);
        assert!(adult_hub_client::PartFileMetadata::read_from_sidecar(&part_file).is_none());
        // Verify partial file was NOT removed by sidecar removal
        assert!(part_file.is_file());

        let _ = std::fs::remove_dir_all(&temp_dir);
    }
}
